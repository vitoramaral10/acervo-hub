//! Binário do `acervo-hub`.
//!
//! `serve` é o modo de vida principal: um processo longo que responde buscas
//! Torznab, serve a interface e roda as tarefas de fundo — a limpeza entre
//! elas. `apply` roda um ciclo de limpeza à mão: ler o mundo, planejar,
//! relatar e executar.
//!
//! A configuração mora no Postgres e se edita pela tela. Fora do banco só
//! há duas variáveis de ambiente: `ACERVO_DATABASE_URL`, que todo comando
//! usa, e `ACERVO_BIND`, o endereço de escuta do `serve`.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod apply;
mod automatic;
mod collect;
mod config;
mod cycle;
mod decide;
mod definitions;
mod events;
mod grab;
mod import_config;
mod ledger;
mod library;
mod mediainfo;
mod metadata;
mod movies;
mod naming;
mod report;
mod rules;
mod search;
mod serve;
mod settings;
mod sync;
mod tasks;
mod watched;
mod web;

#[derive(Debug, Parser)]
#[command(
    name = "acervo-hub",
    version,
    about = "Reconcilia a biblioteca de mídia e serve os indexadores"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Um ciclo de limpeza à mão: lê, planeja, relata e executa.
    Apply,
    /// Serve os indexadores configurados pela API Torznab.
    Serve,
    /// Cadastra os indexadores servidos nos gerenciadores. Sem `--apply`, só
    /// mostra o plano.
    Sync {
        /// Executa o plano em vez de só mostrá-lo.
        #[arg(long)]
        apply: bool,
    },
    /// Catálogo de filmes.
    Movies {
        #[command(subcommand)]
        action: MoviesAction,
    },
    /// Contas da interface web.
    Users {
        #[command(subcommand)]
        action: UsersAction,
    },
    /// Busca manual nos indexadores configurados.
    Search {
        /// Termo da busca.
        term: String,
        /// Só este indexador; sem ele, todos.
        #[arg(long, short)]
        indexer: Option<String>,
        /// Categoria Newznab (repetível), como 5000 para TV ou 2000 para filmes.
        #[arg(long = "categoria", short = 'k')]
        categories: Vec<u32>,
    },
}

#[derive(Debug, Subcommand)]
enum MoviesAction {
    /// Confere cada arquivo do catálogo contra o disco.
    Check,
    /// Busca um filme que falta, decide e, com `--apply`, manda o escolhido
    /// ao qBittorrent.
    Grab {
        /// Id do filme no TMDB.
        tmdb: u32,
        /// Pega de verdade em vez de só mostrar a escolha.
        #[arg(long)]
        apply: bool,
    },
    /// Importa os downloads do acervo que terminaram: hardlink na pasta do
    /// filme, com o nome que o gerenciador daria. Sem `--apply`, só mostra.
    Downloads {
        #[arg(long)]
        apply: bool,
    },
    /// Adiciona um filme ao catálogo a partir do TMDB, com o acervo como dono.
    Add {
        /// Id do filme no TMDB.
        tmdb: u32,
        /// Pasta raiz, como o gerenciador a vê.
        #[arg(long, default_value = "/media/movies")]
        root: String,
        /// Adiciona sem monitorar.
        #[arg(long)]
        unmonitored: bool,
    },
    /// Atualiza os metadados pelo TMDB.
    Refresh {
        /// Todos, e não só os conferidos há mais de um dia.
        #[arg(long)]
        all: bool,
    },
    /// Lê os releases recentes de todos os indexadores e decide contra a
    /// biblioteca inteira, como a busca automática faz. Sem `--apply`, só
    /// mostra o que pegaria.
    Rss {
        #[arg(long)]
        apply: bool,
    },
}

fn print_grab(report: &grab::GrabReport) {
    match &report.escolhido {
        Some(pick) => println!(
            "{}: {} {} — {} ({}, {:.1} GiB)",
            report.filme,
            if report.aplicado { "pegou" } else { "pegaria" },
            pick.titulo,
            pick.indexador,
            pick.qualidade,
            f64::from(u32::try_from(pick.tamanho / 1_048_576).unwrap_or(u32::MAX)) / 1024.0,
        ),
        None => println!(
            "{}: nada entre {} releases — {}",
            report.filme,
            report.releases,
            report
                .motivos
                .iter()
                .map(|(reason, n)| format!("{reason} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    if report.escolhido.is_some() && !report.aplicado {
        println!("  simulação: rode com --apply para pegar");
    }
}

#[derive(Debug, Subcommand)]
enum UsersAction {
    /// Cria o usuário ou troca a senha dele, lida da entrada padrão (uma
    /// linha). Trocar a senha encerra as sessões abertas.
    Set {
        /// Nome de usuário.
        name: String,
    },
    /// Remove o usuário e as sessões dele.
    Remove {
        /// Nome de usuário.
        name: String,
    },
    /// Lista os usuários.
    List,
}

/// Senha da entrada padrão, sem o fim de linha. Nunca vem por argumento:
/// argumento aparece em `ps` e no histórico do shell.
fn read_password() -> Result<String> {
    use std::io::{BufRead, IsTerminal};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("Senha (aparece ao digitar): ");
    }
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

async fn users(store: &acervo_store::Store, action: UsersAction) -> Result<()> {
    match action {
        UsersAction::Set { name } => {
            let password = read_password()?;
            store.set_password(&name, &password).await?;
            println!("senha de `{}` gravada", name.trim());
        }
        UsersAction::Remove { name } => {
            if store.remove_user(&name).await? {
                println!("`{name}` removido");
            } else {
                anyhow::bail!("`{name}` não existe");
            }
        }
        UsersAction::List => {
            for name in store.users().await? {
                println!("{name}");
            }
        }
    }
    Ok(())
}

/// Os subcomandos de `movies`. Devolve se algo falhou.
#[allow(clippy::too_many_lines)] // Um braço por subcomando.
async fn movies_command(
    config: &config::Config,
    store: acervo_store::Store,
    action: MoviesAction,
) -> Result<bool> {
    let records = store.indexers().await?;
    Ok(match action {
        MoviesAction::Check => movies::check(config, &store).await? > 0,
        MoviesAction::Grab { tmdb, apply } => {
            let movie = store
                .movies()
                .await?
                .into_iter()
                .find(|m| m.movie.tmdb_id == tmdb)
                .ok_or_else(|| anyhow::anyhow!("TMDB {tmdb} não está no catálogo"))?;
            let catalog = acervo_api::Catalog::new(serve::entries(config, &records).await)?;
            let report = grab::grab(config, &store, &catalog, movie.id, apply).await?;
            print_grab(&report);
            report.escolhido.is_none()
        }
        MoviesAction::Downloads { apply } => {
            let lines = grab::import_downloads(config, &store, None, apply).await?;
            if lines.is_empty() {
                println!("nenhum download do acervo em andamento");
            }
            for line in &lines {
                println!(
                    "{:<10} {} — {}{}",
                    line.estado,
                    line.filme,
                    line.release,
                    line.destino
                        .as_deref()
                        .or(line.detalhe.as_deref())
                        .map(|d| format!(" → {d}"))
                        .unwrap_or_default()
                );
            }
            lines.iter().any(|l| l.estado == "falhou")
        }
        MoviesAction::Add {
            tmdb,
            root,
            unmonitored,
        } => {
            let client = metadata::require_tmdb(config, &store).await?;
            let id = library::add(
                &store,
                &client,
                &library::AddRequest {
                    tmdb_id: tmdb,
                    root_folder: root,
                    monitored: !unmonitored,
                },
            )
            .await?;
            let added = store.movies().await?.into_iter().find(|m| m.id == id);
            if let Some(added) = added {
                println!(
                    "adicionado: {} — {} ({})",
                    added.movie.title,
                    added.movie.path,
                    added.movie.status.as_deref().unwrap_or("?")
                );
            }
            false
        }
        MoviesAction::Refresh { all } => {
            let client = metadata::require_tmdb(config, &store).await?;
            let report = library::refresh(&store, &client, if all { 0 } else { 24 }).await?;
            for name in &report.atualizados {
                println!("atualizado  {name}");
            }
            for (name, error) in &report.falhas {
                println!("falhou      {name} — {error}");
            }
            println!(
                "{} conferidos, {} atualizados, {} falhas",
                report.conferidos,
                report.atualizados.len(),
                report.falhas.len()
            );
            !report.falhas.is_empty()
        }
        MoviesAction::Rss { apply } => {
            let catalog = acervo_api::Catalog::new(serve::entries(config, &records).await)?;
            let grabs = automatic::rss(config, &store, &catalog, apply).await?;
            if grabs.is_empty() {
                println!("nada entre os releases recentes serve à biblioteca");
            }
            for grab in &grabs {
                println!(
                    "{} {} — {}{}",
                    if apply { "pegou  " } else { "pegaria" },
                    grab.filme,
                    grab.release,
                    grab.erro
                        .as_deref()
                        .map(|e| format!(" (falhou: {e})"))
                        .unwrap_or_default()
                );
            }
            grabs.iter().any(|g| g.erro.is_some())
        }
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "acervo_hub=info,acervo_arr=info,acervo_fs=info,acervo_api=info".into()
            }),
        )
        .with_target(false)
        .init();

    match run().await {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

/// O endereço do banco, do ambiente.
fn database_url() -> Result<String> {
    std::env::var("ACERVO_DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ACERVO_DATABASE_URL ausente: defina-a como \
                 postgres://usuário:senha@host:5432/banco"
            )
        })
}

/// O padrão escuta em todas as interfaces: o serviço roda em container, e
/// `127.0.0.1` lá dentro não seria alcançável pelos vizinhos.
const DEFAULT_BIND: &str = "0.0.0.0:9797";

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let url = database_url()?;
    if matches!(cli.command, Command::Serve) {
        let bind = std::env::var("ACERVO_BIND")
            .ok()
            .filter(|bind| !bind.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BIND.to_owned());
        serve::run(serve::connect(&url).await, &bind).await?;
        return Ok(ExitCode::SUCCESS);
    }
    // Comando avulso: uma tentativa só, e a configuração de agora.
    let store = acervo_store::Store::connect(&url)
        .await
        .map_err(|error| anyhow::anyhow!("conectando ao banco: {error}"))?;
    let config = settings::Settings::load(store.clone()).await?.get();
    let failed = match cli.command {
        Command::Apply => {
            let report = cycle::run(&config, &store, true).await?;
            return Ok(ExitCode::from(report.exit_code()));
        }
        Command::Serve => unreachable!("tratado acima"),
        Command::Search {
            term,
            indexer,
            categories,
        } => search::run(&config, &store, &term, indexer.as_deref(), &categories).await? > 0,
        Command::Users { action } => {
            users(&store, action).await?;
            false
        }
        Command::Movies { action } => movies_command(&config, store, action).await?,
        Command::Sync { apply } => sync::run(&config, &store, apply).await? > 0,
    };
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

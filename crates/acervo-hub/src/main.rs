//! Binário do `acervo-hub`.
//!
//! Um ciclo é sempre a mesma sequência: ler o mundo, planejar, relatar e — só
//! com `apply` — executar. O modo não muda o caminho de código, só o que
//! acontece no último passo. `serve` é o outro modo de vida do binário: um
//! processo longo que responde buscas Torznab.

use std::path::PathBuf;
use std::process::ExitCode;

use acervo_janitor::Mode;
use anyhow::Result;
use clap::{Parser, Subcommand};

mod apply;
mod collect;
mod config;
mod credentials;
mod cycle;
mod definitions;
mod grab;
mod ledger;
mod metadata;
mod movies;
mod naming;
mod registry;
mod report;
mod search;
mod serve;
mod shadow;
mod sync;

#[derive(Debug, Parser)]
#[command(
    name = "acervo-hub",
    version,
    about = "Reconcilia a biblioteca de mídia e serve os indexadores"
)]
struct Cli {
    /// Arquivo de configuração.
    #[arg(long, short, default_value = "/etc/acervo-hub/config.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Lê, planeja e relata. Não altera nada.
    Plan,
    /// Lê, planeja, relata e executa.
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
    /// Espelha os gerenciadores de filmes no catálogo. Sem `--apply`, só
    /// mostra o que mudaria.
    Import {
        /// Grava em vez de só mostrar.
        #[arg(long)]
        apply: bool,
    },
    /// Confere cada arquivo do catálogo contra o disco.
    Check,
    /// Busca os filmes que faltam nos indexadores daqui e decide o que
    /// pegaria, sem pegar nada.
    Shadow {
        /// Quantos filmes buscar nesta rodada.
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Em vez de buscar, compara a última sombra de cada filme com o que o
        /// gerenciador pegou depois.
        #[arg(long)]
        report: bool,
    },
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

async fn users(config: &config::Config, action: UsersAction) -> Result<()> {
    let store = config.store().await?;
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

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let config = config::Config::load(&config::expand_tilde(&cli.config))?;
    let mode = match cli.command {
        Command::Plan => Mode::DryRun,
        Command::Apply => Mode::Apply,
        Command::Serve => {
            serve::run(config).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Search {
            term,
            indexer,
            categories,
        } => {
            let failures = search::run(&config, &term, indexer.as_deref(), &categories).await?;
            return Ok(if failures > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
        Command::Users { action } => {
            users(&config, action).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Movies { action } => {
            let store = config.store().await?;
            let failed = match action {
                MoviesAction::Import { apply } => movies::import(&config, &store, apply, true)
                    .await?
                    .iter()
                    .any(|instance| instance.erro.is_some()),
                MoviesAction::Check => movies::check(&config, &store).await? > 0,
                MoviesAction::Shadow { report: true, .. } => {
                    shadow::report(&config, &store).await? > 0
                }
                MoviesAction::Grab { tmdb, apply } => {
                    let movie = store
                        .movies()
                        .await?
                        .into_iter()
                        .find(|m| m.movie.tmdb_id == tmdb)
                        .ok_or_else(|| anyhow::anyhow!("TMDB {tmdb} não está no catálogo"))?;
                    let catalog = acervo_api::Catalog::new(serve::entries(&config).await?)?;
                    let report = grab::grab(&config, &store, &catalog, movie.id, apply).await?;
                    print_grab(&report);
                    report.escolhido.is_none()
                }
                MoviesAction::Downloads { apply } => {
                    let lines = grab::import_downloads(&config, &store, apply).await?;
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
                MoviesAction::Shadow { limit, .. } => {
                    let catalog = acervo_api::Catalog::new(serve::entries(&config).await?)?;
                    shadow::run(&config, &store, &catalog, limit, true)
                        .await?
                        .iter()
                        .any(|line| line.erro.is_some())
                }
            };
            return Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
        Command::Sync { apply } => {
            let failures = sync::run(&config, apply).await?;
            return Ok(if failures > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
    };

    let report = cycle::run(&config, mode, true).await?;
    cycle::record(&config, &report);
    Ok(ExitCode::from(report.exit_code()))
}

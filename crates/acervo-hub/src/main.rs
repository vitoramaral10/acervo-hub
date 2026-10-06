//! Binário do `acervo-hub`.
//!
//! `serve` é o modo de vida do serviço: um processo longo que serve a
//! interface e roda as tarefas de fundo — busca, RSS, importação, metadados,
//! limpeza, numeração de cena, definições. `users` cuida das contas da
//! interface.
//!
//! A configuração mora no Postgres e se edita pela tela. Fora do banco só
//! há duas variáveis de ambiente: `ACERVO_DATABASE_URL`, que todo comando
//! usa, e `ACERVO_BIND`, o endereço de escuta do `serve`.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod agenda;
mod apply;
mod automatic;
mod collect;
mod config;
mod cycle;
mod decide;
mod definitions;
mod discover;
mod events;
mod grab;
mod heal;
mod health;
mod ledger;
mod library;
mod marks;
mod mediainfo;
mod metadata;
mod movies;
mod naming;
mod registry;
mod rename;
mod rules;
mod series;
mod serve;
mod settings;
mod subtitles;
mod tasks;
mod verify;
mod watched;
mod web;

#[derive(Debug, Parser)]
#[command(
    name = "acervo-hub",
    version,
    about = "Gerencia a biblioteca de filmes e séries e serve a interface"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sobe o serviço: a interface e as tarefas de fundo.
    Serve,
    /// Pergunta ao serviço local se está pronto (`/health/ready`) e sai com
    /// 0 ou 1: o `HEALTHCHECK` do container, que não tem curl.
    Healthcheck,
    /// Contas da interface web.
    Users {
        #[command(subcommand)]
        action: UsersAction,
    },
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

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "acervo_hub=info,acervo_fs=info,acervo_api=info".into()),
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

/// O endereço de escuta: `ACERVO_BIND` ou o padrão.
fn bind() -> String {
    std::env::var("ACERVO_BIND")
        .ok()
        .filter(|bind| !bind.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BIND.to_owned())
}

/// O endereço da prontidão, no mesmo processo: a porta vem de `bind`, mas o
/// destino é sempre o próprio `127.0.0.1`, que o `0.0.0.0` do `serve` inclui.
fn ready_url(bind: &str) -> Result<String> {
    let port = bind
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("ACERVO_BIND sem porta válida: `{bind}`"))?;
    Ok(format!("http://127.0.0.1:{port}/health/ready"))
}

/// Tempo máximo da pergunta: um pouco além do teto de cada verificação do
/// serviço.
const HEALTHCHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

async fn healthcheck() -> Result<()> {
    let bind = bind();
    let response = reqwest::Client::builder()
        .timeout(HEALTHCHECK_TIMEOUT)
        .build()?
        .get(ready_url(&bind)?)
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    // O corpo diz o que falhou, sem segredo; vai para o `docker inspect`.
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("não está pronto ({status}): {body}")
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Healthcheck => healthcheck().await?,
        Command::Serve => {
            let url = database_url()?;
            let bind = bind();
            serve::run(serve::connect(&url).await, &bind).await?;
        }
        Command::Users { action } => {
            // Comando avulso: uma tentativa só.
            let store = acervo_store::Store::connect(&database_url()?)
                .await
                .map_err(|error| anyhow::anyhow!("conectando ao banco: {error}"))?;
            users(&store, action).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::ready_url;

    #[test]
    fn healthcheck_usa_a_porta_do_bind_no_loopback() {
        assert_eq!(
            ready_url("0.0.0.0:9797").unwrap(),
            "http://127.0.0.1:9797/health/ready"
        );
        assert_eq!(
            ready_url(" [::]:8080 ").unwrap(),
            "http://127.0.0.1:8080/health/ready"
        );
        assert!(ready_url("sem-porta").is_err());
        assert!(ready_url("host:abc").is_err());
    }
}

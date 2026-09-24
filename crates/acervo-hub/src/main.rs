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
mod ledger;
mod registry;
mod report;
mod search;
mod serve;
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

//! Binário do `acervo-hub`.
//!
//! Um ciclo é sempre a mesma sequência: ler o mundo, planejar, relatar e — só
//! com `apply` — executar. O modo não muda o caminho de código, só o que
//! acontece no último passo. `serve` é o outro modo de vida do binário: um
//! processo longo que responde buscas Torznab.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use acervo_janitor::{Mode, reconcile};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod apply;
mod collect;
mod config;
mod ledger;
mod report;
mod serve;

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
            serve::run(&config).await?;
            return Ok(ExitCode::SUCCESS);
        }
    };

    let session = collect::collect(&config).await?;
    report::inventory(&session.inventory);

    let ledger_path = config::expand_tilde(&config.state.ledger);
    let mut strikes = ledger::load(&ledger_path)?;

    let plan = match reconcile(
        &session.inventory,
        &config.policy.to_policy(mode),
        &mut strikes,
        SystemTime::now(),
    ) {
        Ok(plan) => plan,
        Err(abort) => {
            // Abortar é resultado esperado, não defeito: a leitura do mundo não
            // estava confiável. Sai com código próprio para que um agendador
            // possa distinguir isso de falha de execução.
            tracing::warn!("ciclo abortado: {abort}");
            println!("Ciclo abortado: {abort}");
            println!("Nenhuma alteração foi feita.");
            return Ok(ExitCode::from(3));
        }
    };

    report::plan(&plan);

    if mode == Mode::DryRun {
        // Simulação não avança strike: se avançasse, repetir a simulação
        // levaria o item ao limite sem ninguém ter decidido nada.
        println!("Simulação: strikes não foram gravados.");
        return Ok(ExitCode::SUCCESS);
    }

    ledger::save(&ledger_path, &strikes)
        .with_context(|| format!("gravando os strikes em `{}`", ledger_path.display()))?;

    let outcome = apply::execute(&session, &plan).await;
    println!(
        "Executadas {} ações, {} falharam.",
        outcome.done, outcome.failed
    );

    Ok(if outcome.failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

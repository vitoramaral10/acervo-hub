//! `serve`: monta o catálogo de indexadores e expõe a superfície Torznab.

use std::sync::Arc;
use std::time::Duration;

use acervo_api::{Catalog, Entry};
use acervo_indexers::{CardigannClient, CardigannDefinition, TorznabClient};
use anyhow::{Context, Result};

use crate::config::{CardigannIndexer, Config, IndexerConfig, TorznabIndexer, expand_tilde};

/// Sobe o servidor e só volta no SIGTERM ou no Ctrl-C.
///
/// # Errors
///
/// Configuração incompleta, definição inválida, endereço ocupado ou nenhum
/// indexador utilizável.
pub async fn run(config: &Config) -> Result<()> {
    let server = config.server()?;
    let catalog = catalog(config).await?;
    tracing::info!(
        indexadores = catalog.len(),
        bind = %server.bind,
        "servindo Torznab"
    );

    let listener = tokio::net::TcpListener::bind(&server.bind)
        .await
        .with_context(|| format!("abrindo `{}`", server.bind))?;
    axum::serve(
        listener,
        acervo_api::router(catalog, server.api_key.clone()),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .context("servidor HTTP")?;
    Ok(())
}

/// Definição Cardigann inválida é erro de configuração e derruba a subida:
/// é arquivo local, e subir sem ela esconderia o erro atrás de "nenhum
/// resultado". Endpoint Torznab que não responde `caps` é diferente — é rede,
/// e um tracker fora do ar não pode tirar os outros do ar junto. Ele fica de
/// fora, com aviso, até o próximo reinício.
async fn catalog(config: &Config) -> Result<Catalog> {
    let mut entries = Vec::new();
    for spec in &config.indexers {
        match spec {
            IndexerConfig::Cardigann(spec) => entries.push(cardigann(spec, config.http_timeout())?),
            IndexerConfig::Torznab(spec) => match torznab(spec, config.http_timeout()).await {
                Ok(entry) => entries.push(entry),
                Err(error) => tracing::error!(
                    indexer = spec.name,
                    "fora do catálogo até o próximo reinício: {error:#}"
                ),
            },
        }
    }
    anyhow::ensure!(
        !entries.is_empty(),
        "nenhum indexador utilizável: não haveria o que servir"
    );
    Ok(Catalog::new(entries)?)
}

fn cardigann(spec: &CardigannIndexer, timeout: Duration) -> Result<Entry> {
    let path = expand_tilde(&spec.definition);
    let yaml = std::fs::read_to_string(&path)
        .with_context(|| format!("lendo a definição `{}`", path.display()))?;
    let definition = CardigannDefinition::from_yaml_v11(&yaml)
        .with_context(|| format!("carregando a definição `{}`", path.display()))?;
    let capabilities = definition.capabilities().clone();
    let client = CardigannClient::new(definition, spec.link, spec.settings.clone(), timeout)
        .with_context(|| format!("configurando a definição `{}`", path.display()))?;
    Ok(Entry {
        indexer: Arc::new(client),
        capabilities,
    })
}

async fn torznab(spec: &TorznabIndexer, timeout: Duration) -> Result<Entry> {
    let client = TorznabClient::new(
        spec.name.clone(),
        &spec.url,
        spec.api_key.clone(),
        timeout,
        Duration::from_secs_f64(spec.request_interval_seconds),
    )?;
    let capabilities = client
        .capabilities()
        .await
        .context("lendo as capacidades")?;
    Ok(Entry {
        indexer: Arc::new(client),
        capabilities,
    })
}

async fn shutdown() {
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!("sem escuta de SIGTERM: {error}");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminate => {}
    }
    tracing::info!("encerrando");
}

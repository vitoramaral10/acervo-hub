//! `serve`: monta o catálogo de indexadores e expõe a superfície Torznab e a
//! interface web.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use acervo_api::{Admin, Catalog, Entry, SettingView};
use acervo_indexers::{
    CardigannClient, CardigannDefinition, SettingInfo, SettingInfoKind, TorznabClient,
};
use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::config::{CardigannIndexer, Config, IndexerConfig, TorznabIndexer, expand_tilde};
use crate::credentials::{self, Overrides};

/// Sobe o servidor e só volta no SIGTERM ou no Ctrl-C.
///
/// # Errors
///
/// Configuração incompleta, definição inválida, endereço ocupado ou nenhum
/// indexador utilizável.
pub async fn run(config: &Config) -> Result<()> {
    let server = config.server()?;
    let catalog = Catalog::new(entries(config).await?)?;
    let admin = HubAdmin::new(config)?;
    tracing::info!(
        indexadores = catalog.len(),
        bind = %server.bind,
        "servindo Torznab e a interface web"
    );

    let listener = tokio::net::TcpListener::bind(&server.bind)
        .await
        .with_context(|| format!("abrindo `{}`", server.bind))?;
    axum::serve(
        listener,
        acervo_api::router_with_admin(catalog, server.api_key.clone(), Some(Arc::new(admin))),
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
///
/// # Errors
///
/// Definição Cardigann ilegível ou inválida, credenciais ilegíveis, ou nenhum
/// indexador utilizável.
pub async fn entries(config: &Config) -> Result<Vec<Entry>> {
    let overrides = credentials::load(&expand_tilde(&config.state.credentials))?;
    let mut entries = Vec::new();
    for spec in &config.indexers {
        match spec {
            IndexerConfig::Cardigann(spec) => {
                let definition = load_definition(spec)?;
                let settings = merged(spec, overrides.get(definition.id()));
                entries.push(cardigann(
                    spec,
                    definition,
                    settings,
                    config.http_timeout(),
                )?);
            }
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
    Ok(entries)
}

fn load_definition(spec: &CardigannIndexer) -> Result<CardigannDefinition> {
    let path = expand_tilde(&spec.definition);
    let yaml = std::fs::read_to_string(&path)
        .with_context(|| format!("lendo a definição `{}`", path.display()))?;
    CardigannDefinition::from_yaml_v11(&yaml)
        .with_context(|| format!("carregando a definição `{}`", path.display()))
}

/// Settings da config com as credenciais trocadas pela interface por cima.
fn merged(
    spec: &CardigannIndexer,
    overrides: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut settings = spec.settings.clone();
    if let Some(overrides) = overrides {
        settings.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    settings
}

fn cardigann(
    spec: &CardigannIndexer,
    definition: CardigannDefinition,
    settings: BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Entry> {
    let capabilities = definition.capabilities().clone();
    let client = CardigannClient::new(definition, spec.link, settings, timeout)
        .with_context(|| format!("configurando a definição `{}`", spec.definition.display()))?;
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

/// Reconfiguração de indexadores Cardigann pela interface.
#[derive(Debug)]
struct HubAdmin {
    /// Por id da definição.
    indexers: BTreeMap<String, CardigannIndexer>,
    credentials: PathBuf,
    timeout: Duration,
    /// Serializa as gravações: duas trocas simultâneas não podem se apagar.
    write: tokio::sync::Mutex<()>,
}

impl HubAdmin {
    fn new(config: &Config) -> Result<Self> {
        let mut indexers = BTreeMap::new();
        for spec in &config.indexers {
            if let IndexerConfig::Cardigann(spec) = spec {
                let id = load_definition(spec)?.id().to_owned();
                indexers.insert(id, spec.clone());
            }
        }
        Ok(Self {
            indexers,
            credentials: expand_tilde(&config.state.credentials),
            timeout: config.http_timeout(),
            write: tokio::sync::Mutex::new(()),
        })
    }

    fn current(&self, name: &str, spec: &CardigannIndexer) -> BTreeMap<String, String> {
        // Credencial ilegível já derrubou a subida; aqui, vazio é o seguro.
        let overrides = credentials::load(&self.credentials).unwrap_or_default();
        merged(spec, overrides.get(name))
    }
}

fn view(info: &SettingInfo, value: Option<&String>) -> SettingView {
    let value = value.cloned().or_else(|| info.default.clone());
    let secret = info.is_secret();
    let (kind, options) = match &info.kind {
        SettingInfoKind::Text => ("text", Vec::new()),
        SettingInfoKind::Password => ("password", Vec::new()),
        SettingInfoKind::Checkbox => ("checkbox", Vec::new()),
        SettingInfoKind::Select(options) => ("select", options.clone()),
    };
    SettingView {
        name: info.name.clone(),
        label: info.label.clone(),
        kind,
        options,
        secret,
        is_set: value.as_ref().is_some_and(|value| !value.is_empty()),
        value: if secret { None } else { value },
    }
}

#[async_trait]
impl Admin for HubAdmin {
    fn settings(&self, indexer: &str) -> Option<Vec<SettingView>> {
        let spec = self.indexers.get(indexer)?;
        let definition = load_definition(spec).ok()?;
        let current = self.current(indexer, spec);
        let views: Vec<_> = definition
            .settings()
            .iter()
            .map(|info| view(info, current.get(&info.name)))
            .collect();
        (!views.is_empty()).then_some(views)
    }

    async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        let _guard = self.write.lock().await;
        let spec = self
            .indexers
            .get(indexer)
            .ok_or_else(|| "indexador sem settings editáveis".to_owned())?;
        let definition = load_definition(spec).map_err(|error| format!("{error:#}"))?;
        let declared = definition.settings();

        let mut overrides: Overrides =
            credentials::load(&self.credentials).map_err(|error| format!("{error:#}"))?;
        let saved = overrides.entry(indexer.to_owned()).or_default();
        for (name, value) in values {
            let info = declared
                .iter()
                .find(|info| info.name == name)
                .ok_or_else(|| format!("setting desconhecido: {name}"))?;
            // Segredo em branco é "manter o atual": ele nunca volta à tela,
            // então o campo vazio não pode apagá-lo.
            if info.is_secret() && value.is_empty() {
                continue;
            }
            saved.insert(name, value);
        }
        let settings = merged(spec, overrides.get(indexer));
        let entry = cardigann(spec, definition, settings, self.timeout)
            .map_err(|error| format!("{error:#}"))?;
        credentials::save(&self.credentials, &overrides).map_err(|error| format!("{error:#}"))?;
        tracing::info!(indexer, "credenciais atualizadas pela interface");
        Ok(entry)
    }
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

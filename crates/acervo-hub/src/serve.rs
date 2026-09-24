//! `serve`: monta o catálogo de indexadores e expõe a superfície Torznab e a
//! interface web.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use acervo_api::{Admin, Catalog, DefinitionView, Entry, SettingView};
use acervo_indexers::{
    Capabilities, CardigannClient, CardigannDefinition, SettingInfo, SettingInfoKind, TorznabClient,
};
use acervo_janitor::Mode;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;

use crate::config::{CardigannIndexer, Config, IndexerConfig, TorznabIndexer, expand_tilde};
use crate::credentials::{self, Overrides};
use crate::definitions::Definitions;
use crate::registry::{self, Added, Registry};

/// Nome da "definição" que adiciona um endpoint Torznab qualquer.
const TORZNAB: &str = "torznab";

/// Sobe o servidor e só volta no SIGTERM ou no Ctrl-C.
///
/// # Errors
///
/// Configuração incompleta, definição inválida ou endereço ocupado.
pub async fn run(config: Config) -> Result<()> {
    let server = config.server()?;
    let bind = server.bind.clone();
    let api_key = server.api_key.clone();
    let catalog = Catalog::new(entries(&config).await?)?;
    let admin = HubAdmin::new(config)?;
    tracing::info!(indexadores = catalog.len(), bind = %bind, "servindo Torznab e a interface web");

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("abrindo `{bind}`"))?;
    axum::serve(
        listener,
        acervo_api::router_with_admin(catalog, api_key, Some(Arc::new(admin))),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .context("servidor HTTP")?;
    Ok(())
}

/// Indexadores servidos: os da config e os adicionados pela interface, menos
/// os desativados.
///
/// Definição Cardigann inválida é erro de configuração e derruba a subida: é
/// arquivo local, e subir sem ela esconderia o erro atrás de "nenhum
/// resultado". Endpoint Torznab que não responde `caps` é diferente — é rede,
/// e um tracker fora do ar não pode tirar os outros do ar junto. Ele fica de
/// fora, com aviso, até o próximo reinício.
///
/// # Errors
///
/// Definição Cardigann ilegível ou inválida, ou credenciais e registro
/// ilegíveis.
pub async fn entries(config: &Config) -> Result<Vec<Entry>> {
    let overrides = credentials::load(&expand_tilde(&config.state.credentials))?;
    let registry = registry::load(&expand_tilde(&config.state.registry))?;
    let mut specs: Vec<IndexerConfig> = config.indexers.clone();
    specs.extend(registry.added.iter().map(Added::to_config));

    let mut entries = Vec::new();
    for spec in &specs {
        match spec {
            IndexerConfig::Cardigann(spec) => {
                let definition = load_definition(spec)?;
                if registry.disabled.contains(definition.id()) {
                    continue;
                }
                let settings = merged(&spec.settings, overrides.get(definition.id()));
                entries.push(cardigann(
                    spec,
                    definition,
                    settings,
                    config.http_timeout(),
                )?);
            }
            IndexerConfig::Torznab(spec) => {
                if registry.disabled.contains(&spec.name) {
                    continue;
                }
                let spec = with_key(spec, overrides.get(&spec.name));
                match torznab(&spec, config.http_timeout()).await {
                    Ok(entry) => entries.push(entry),
                    Err(error) => tracing::error!(
                        indexer = spec.name,
                        "fora do catálogo até o próximo reinício: {error:#}"
                    ),
                }
            }
        }
    }
    Ok(entries)
}

impl Added {
    fn to_config(&self) -> IndexerConfig {
        match self {
            Self::Cardigann { path, .. } => IndexerConfig::Cardigann(CardigannIndexer {
                definition: path.clone(),
                link: 0,
                settings: BTreeMap::new(),
            }),
            Self::Torznab(spec) => IndexerConfig::Torznab(spec.clone()),
        }
    }
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
    base: &BTreeMap<String, String>,
    overrides: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut settings = base.clone();
    if let Some(overrides) = overrides {
        settings.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    settings
}

/// A chave de um Torznab adicionado pela tela mora nas credenciais.
fn with_key(spec: &TorznabIndexer, overrides: Option<&BTreeMap<String, String>>) -> TorznabIndexer {
    let mut spec = spec.clone();
    if let Some(key) = overrides.and_then(|values| values.get("api_key")) {
        spec.api_key = Some(key.clone());
    }
    spec
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

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name != "all"
        && name != "ui"
}

/// De onde vem um indexador e como montá-lo.
enum Source {
    Cardigann(CardigannIndexer, &'static str),
    Torznab(TorznabIndexer, &'static str),
}

/// Administração pela interface.
#[derive(Debug)]
struct HubAdmin {
    config: Config,
    definitions: Definitions,
    /// Indexadores Cardigann da config, pelo id da definição.
    config_cardigann: BTreeMap<String, CardigannIndexer>,
    config_torznab: BTreeMap<String, TorznabIndexer>,
    credentials: PathBuf,
    registry: PathBuf,
    /// Serializa as gravações: duas mudanças simultâneas não podem se apagar.
    write: tokio::sync::Mutex<()>,
}

impl HubAdmin {
    fn new(config: Config) -> Result<Self> {
        let catalogs: Vec<PathBuf> = config
            .server
            .as_ref()
            .map(|server| {
                server
                    .catalogs
                    .iter()
                    .map(|dir| expand_tilde(dir))
                    .collect()
            })
            .unwrap_or_default();
        let mut definitions = Definitions::scan(&catalogs);
        let mut config_cardigann = BTreeMap::new();
        let mut config_torznab = BTreeMap::new();
        for spec in &config.indexers {
            match spec {
                IndexerConfig::Cardigann(spec) => {
                    definitions.include(&expand_tilde(&spec.definition));
                    let id = load_definition(spec)?.id().to_owned();
                    config_cardigann.insert(id, spec.clone());
                }
                IndexerConfig::Torznab(spec) => {
                    config_torznab.insert(spec.name.clone(), spec.clone());
                }
            }
        }
        Ok(Self {
            credentials: expand_tilde(&config.state.credentials),
            registry: expand_tilde(&config.state.registry),
            config,
            definitions,
            config_cardigann,
            config_torznab,
            write: tokio::sync::Mutex::new(()),
        })
    }

    fn load_registry(&self) -> Registry {
        // Registro ilegível já derrubou a subida; aqui, vazio é o seguro.
        registry::load(&self.registry).unwrap_or_default()
    }

    fn overrides(&self) -> Overrides {
        credentials::load(&self.credentials).unwrap_or_default()
    }

    fn source(&self, name: &str) -> Option<Source> {
        if let Some(spec) = self.config_cardigann.get(name) {
            return Some(Source::Cardigann(spec.clone(), "config"));
        }
        if let Some(spec) = self.config_torznab.get(name) {
            return Some(Source::Torznab(spec.clone(), "config"));
        }
        self.load_registry()
            .added
            .iter()
            .find(|added| added.name() == name)
            .map(|added| match added.to_config() {
                IndexerConfig::Cardigann(spec) => Source::Cardigann(spec, "interface"),
                IndexerConfig::Torznab(spec) => Source::Torznab(spec, "interface"),
            })
    }

    async fn build(&self, name: &str) -> Result<Entry, String> {
        let overrides = self.overrides();
        match self.source(name).ok_or("indexador desconhecido")? {
            Source::Cardigann(spec, _) => {
                let definition = load_definition(&spec).map_err(|e| format!("{e:#}"))?;
                let settings = merged(&spec.settings, overrides.get(name));
                cardigann(&spec, definition, settings, self.config.http_timeout())
                    .map_err(|e| format!("{e:#}"))
            }
            Source::Torznab(spec, _) => {
                let spec = with_key(&spec, overrides.get(name));
                torznab(&spec, self.config.http_timeout())
                    .await
                    .map_err(|e| format!("{e:#}"))
            }
        }
    }

    fn save(
        &self,
        registry: Option<&Registry>,
        overrides: Option<&Overrides>,
    ) -> Result<(), String> {
        if let Some(registry) = registry {
            registry::save(&self.registry, registry).map_err(|e| format!("{e:#}"))?;
        }
        if let Some(overrides) = overrides {
            credentials::save(&self.credentials, overrides).map_err(|e| format!("{e:#}"))?;
        }
        Ok(())
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

fn plain(name: &str, label: &str, kind: &'static str, value: Option<String>) -> SettingView {
    let secret = kind == "password";
    SettingView {
        name: name.into(),
        label: label.into(),
        kind,
        options: Vec::new(),
        secret,
        is_set: value.as_ref().is_some_and(|value| !value.is_empty()),
        value: if secret { None } else { value },
    }
}

#[async_trait]
impl Admin for HubAdmin {
    fn settings(&self, indexer: &str) -> Option<Vec<SettingView>> {
        match self.source(indexer)? {
            Source::Cardigann(spec, _) => {
                let definition = load_definition(&spec).ok()?;
                let current = merged(&spec.settings, self.overrides().get(indexer));
                let views: Vec<_> = definition
                    .settings()
                    .iter()
                    .map(|info| view(info, current.get(&info.name)))
                    .collect();
                (!views.is_empty()).then_some(views)
            }
            Source::Torznab(spec, "interface") => {
                let key = self
                    .overrides()
                    .get(indexer)
                    .and_then(|values| values.get("api_key"))
                    .cloned();
                Some(vec![
                    plain("url", "URL do endpoint Torznab", "text", Some(spec.url)),
                    plain("api_key", "Chave de API", "password", key),
                ])
            }
            Source::Torznab(..) => None,
        }
    }

    async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        let _guard = self.write.lock().await;
        let mut overrides = credentials::load(&self.credentials).map_err(|e| format!("{e:#}"))?;
        match self
            .source(indexer)
            .ok_or("indexador sem settings editáveis")?
        {
            Source::Cardigann(spec, _) => {
                let definition = load_definition(&spec).map_err(|e| format!("{e:#}"))?;
                let declared = definition.settings();
                let saved = overrides.entry(indexer.to_owned()).or_default();
                for (name, value) in values {
                    let info = declared
                        .iter()
                        .find(|info| info.name == name)
                        .ok_or_else(|| format!("setting desconhecido: {name}"))?;
                    // Segredo em branco é "manter o atual": ele nunca volta à
                    // tela, então o campo vazio não pode apagá-lo.
                    if info.is_secret() && value.is_empty() {
                        continue;
                    }
                    saved.insert(name, value);
                }
                let settings = merged(&spec.settings, overrides.get(indexer));
                let entry = cardigann(&spec, definition, settings, self.config.http_timeout())
                    .map_err(|e| format!("{e:#}"))?;
                self.save(None, Some(&overrides))?;
                tracing::info!(indexer, "settings atualizados pela interface");
                Ok(entry)
            }
            Source::Torznab(spec, "interface") => {
                let mut registry = self.load_registry();
                let mut spec = spec;
                if let Some(url) = values.get("url").filter(|url| !url.is_empty()) {
                    spec.url.clone_from(url);
                }
                if let Some(key) = values.get("api_key").filter(|key| !key.is_empty()) {
                    overrides
                        .entry(indexer.to_owned())
                        .or_default()
                        .insert("api_key".into(), key.clone());
                }
                let keyed = with_key(&spec, overrides.get(indexer));
                let entry = torznab(&keyed, self.config.http_timeout())
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                for added in &mut registry.added {
                    if let Added::Torznab(existing) = added
                        && existing.name == indexer
                    {
                        *existing = spec.clone();
                    }
                }
                self.save(Some(&registry), Some(&overrides))?;
                Ok(entry)
            }
            Source::Torznab(..) => Err("indexador do config.toml: edite o arquivo".into()),
        }
    }

    fn origin(&self, indexer: &str) -> Option<&'static str> {
        self.source(indexer).map(|source| match source {
            Source::Cardigann(_, origin) | Source::Torznab(_, origin) => origin,
        })
    }

    fn disabled(&self) -> Vec<(String, &'static str)> {
        self.load_registry()
            .disabled
            .into_iter()
            .filter_map(|name| self.origin(&name).map(|origin| (name, origin)))
            .collect()
    }

    fn definitions(&self) -> Vec<DefinitionView> {
        let exists = |id: &str| self.source(id).is_some();
        let mut list = vec![DefinitionView {
            id: TORZNAB.into(),
            name: "Torznab genérico".into(),
            description: "Qualquer indexador que já fale Torznab: outro agregador, Jackett, ou \
                          um tracker com API própria."
                .into(),
            language: String::new(),
            private: false,
            supported: true,
            reason: None,
            added: false,
        }];
        list.extend(self.definitions.iter().map(|known| DefinitionView {
            id: known.header.id.clone(),
            name: known.header.name.clone(),
            description: known.header.description.clone(),
            language: known.header.language.clone(),
            private: known.header.private,
            supported: known.refusal.is_none(),
            reason: known.refusal.clone(),
            added: exists(&known.header.id),
        }));
        list
    }

    fn definition_settings(&self, definition: &str) -> Option<Vec<SettingView>> {
        if definition == TORZNAB {
            return Some(vec![
                plain(
                    "name",
                    "Nome (letras minúsculas, números e hífens)",
                    "text",
                    None,
                ),
                plain(
                    "url",
                    "URL do endpoint Torznab (termina em /api)",
                    "text",
                    None,
                ),
                plain("api_key", "Chave de API", "password", None),
            ]);
        }
        let known = self.definitions.get(definition)?;
        if known.refusal.is_some() {
            return None;
        }
        let yaml = std::fs::read_to_string(&known.path).ok()?;
        let parsed = CardigannDefinition::from_yaml_v11(&yaml).ok()?;
        Some(
            parsed
                .settings()
                .iter()
                .map(|info| view(info, None))
                .collect(),
        )
    }

    async fn add(
        &self,
        definition: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        let _guard = self.write.lock().await;
        let mut registry = registry::load(&self.registry).map_err(|e| format!("{e:#}"))?;
        let mut overrides = credentials::load(&self.credentials).map_err(|e| format!("{e:#}"))?;

        if definition == TORZNAB {
            let name = values
                .get("name")
                .map(|name| name.trim().to_owned())
                .unwrap_or_default();
            if !valid_name(&name) {
                return Err("nome inválido: use letras minúsculas, números e hífens".into());
            }
            if self.source(&name).is_some() {
                return Err(format!("já existe um indexador chamado {name}"));
            }
            let url = values
                .get("url")
                .map(|url| url.trim().to_owned())
                .unwrap_or_default();
            let spec = TorznabIndexer {
                name: name.clone(),
                url,
                api_key: None,
                request_interval_seconds: 2.0,
            };
            if let Some(key) = values.get("api_key").filter(|key| !key.is_empty()) {
                overrides
                    .entry(name.clone())
                    .or_default()
                    .insert("api_key".into(), key.clone());
            }
            let keyed = with_key(&spec, overrides.get(&name));
            let entry = torznab(&keyed, self.config.http_timeout())
                .await
                .map_err(|e| format!("o endpoint não respondeu às capacidades: {e:#}"))?;
            registry.added.push(Added::Torznab(spec));
            registry.disabled.remove(&name);
            self.save(Some(&registry), Some(&overrides))?;
            tracing::info!(indexer = name, "Torznab adicionado pela interface");
            return Ok(entry);
        }

        let known = self
            .definitions
            .get(definition)
            .ok_or("definição desconhecida")?
            .clone();
        if let Some(reason) = &known.refusal {
            return Err(format!("definição ainda não suportada: {reason}"));
        }
        if self.source(definition).is_some() {
            return Err(format!("{definition} já está adicionado"));
        }
        let spec = CardigannIndexer {
            definition: known.path.clone(),
            link: 0,
            settings: BTreeMap::new(),
        };
        let parsed = load_definition(&spec).map_err(|e| format!("{e:#}"))?;
        let declared = parsed.settings();
        let mut saved = BTreeMap::new();
        for (name, value) in values {
            if !declared.iter().any(|info| info.name == name) {
                return Err(format!("setting desconhecido: {name}"));
            }
            if !value.is_empty() {
                saved.insert(name, value);
            }
        }
        let entry = cardigann(&spec, parsed, saved.clone(), self.config.http_timeout())
            .map_err(|e| format!("{e:#}"))?;
        overrides.insert(definition.to_owned(), saved);
        registry.added.push(Added::Cardigann {
            definition: definition.to_owned(),
            path: known.path,
        });
        registry.disabled.remove(definition);
        self.save(Some(&registry), Some(&overrides))?;
        tracing::info!(indexer = definition, "indexador adicionado pela interface");
        Ok(entry)
    }

    async fn remove(&self, indexer: &str) -> Result<(), String> {
        let _guard = self.write.lock().await;
        match self.origin(indexer) {
            None => return Err("indexador desconhecido".into()),
            Some("config") => {
                return Err("indexador do config.toml: remova do arquivo, ou desative aqui".into());
            }
            Some(_) => {}
        }
        let mut registry = registry::load(&self.registry).map_err(|e| format!("{e:#}"))?;
        let mut overrides = credentials::load(&self.credentials).map_err(|e| format!("{e:#}"))?;
        registry.added.retain(|added| added.name() != indexer);
        registry.disabled.remove(indexer);
        overrides.remove(indexer);
        self.save(Some(&registry), Some(&overrides))?;
        tracing::info!(indexer, "indexador removido pela interface");
        Ok(())
    }

    async fn set_enabled(&self, indexer: &str, enabled: bool) -> Result<Option<Entry>, String> {
        let _guard = self.write.lock().await;
        if self.origin(indexer).is_none() {
            return Err("indexador desconhecido".into());
        }
        let mut registry = registry::load(&self.registry).map_err(|e| format!("{e:#}"))?;
        if !enabled {
            registry.disabled.insert(indexer.to_owned());
            self.save(Some(&registry), None)?;
            return Ok(None);
        }
        // Monta antes de gravar: se não sobe, continua desativado.
        let entry = self.build(indexer).await?;
        registry.disabled.remove(indexer);
        self.save(Some(&registry), None)?;
        Ok(Some(entry))
    }

    fn apps(&self) -> serde_json::Value {
        let public_url = self
            .config
            .server
            .as_ref()
            .and_then(|server| server.public_url.clone());
        json!({
            "endereco_publico": public_url,
            "instancias": self.config.instances.iter().map(|instance| json!({
                "nome": instance.name,
                "tipo": match instance.kind {
                    crate::config::InstanceKind::Series => "series",
                    crate::config::InstanceKind::Movie => "filmes",
                },
                "url": instance.url,
            })).collect::<Vec<_>>(),
        })
    }

    async fn sync(
        &self,
        indexers: Vec<(String, Capabilities)>,
        apply: bool,
    ) -> Result<serde_json::Value, String> {
        let report = crate::sync::execute(&self.config, &indexers, apply, false)
            .await
            .map_err(|e| format!("{e:#}"))?;
        serde_json::to_value(report).map_err(|e| e.to_string())
    }

    fn last_cycle(&self) -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(self.config.state.last_cycle()).ok()?;
        serde_json::from_str(&text).ok()
    }

    async fn simulate_cycle(&self) -> Result<serde_json::Value, String> {
        self.config.janitor().map_err(|e| format!("{e:#}"))?;
        let report = crate::cycle::run(&self.config, Mode::DryRun, false)
            .await
            .map_err(|e| format!("{e:#}"))?;
        serde_json::to_value(report).map_err(|e| e.to_string())
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

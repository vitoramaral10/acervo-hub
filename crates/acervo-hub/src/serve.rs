//! `serve`: monta o catálogo de indexadores, sobe as tarefas de fundo e
//! serve a interface web.
//!
//! Tudo vem do banco: a configuração ([`Settings`]) e os indexadores
//! cadastrados (tabela `indexers`). Não há indexador "do arquivo" e "da
//! tela": todo cadastro se edita e se remove pela interface.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use crate::api::{ApiKey, Catalog, DefinitionCatalog, DefinitionView, Entry, SettingView};
use acervo_indexers::{CardigannDefinition, SettingInfo, SettingInfoKind};
use acervo_store::{IndexerRecord, Store};
use anyhow::{Context, Result};
use serde_json::json;

use crate::config::{Config, SERVIDOR, ServerConfig};
use crate::decide::Progress;
use crate::registry::{
    CARDIGANN, FLARESOLVERR_OPTIONS, Registry, USE_FLARESOLVERR, USE_PROXY, build, cardigann,
    check_reserved, reserved, scan,
};
use crate::settings::Settings;
use crate::tasks::{BUSCA, Tasks};

/// O banco do catálogo e das contas.
///
/// `serve` só sobe depois de conectar, então em produção ele está sempre
/// presente; o vazio ([`Default`]) existe para os testes do agendador.
#[derive(Debug, Clone, Default)]
pub struct Database(Arc<std::sync::OnceLock<Store>>);

impl Database {
    #[must_use]
    pub fn connected(store: Store) -> Self {
        Self(Arc::new(std::sync::OnceLock::from(store)))
    }

    pub(crate) fn get(&self) -> Result<&Store, String> {
        self.0
            .get()
            .ok_or_else(|| "banco de dados ainda indisponível; tente de novo em instantes".into())
    }
}

impl Database {
    pub(crate) async fn login(&self, user: &str, password: &str) -> Result<Option<String>, String> {
        self.get()?
            .login(user, password)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn session_user(&self, token: &str) -> Result<Option<String>, String> {
        self.get()?
            .session_user(token)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn logout(&self, token: &str) -> Result<(), String> {
        self.get()?.logout(token).await.map_err(|e| e.to_string())
    }
}

/// Conecta ao banco, tentando de novo até conseguir: o Postgres pode subir
/// depois do serviço, e sem ele não há configuração para servir.
pub async fn connect(url: &str) -> Store {
    loop {
        match Store::connect(url).await {
            Ok(store) => {
                tracing::info!("banco de dados conectado");
                return store;
            }
            Err(error) => {
                tracing::warn!("banco de dados indisponível, nova tentativa em 30 s: {error}");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

/// Quanto o desligamento espera as tarefas em execução terminarem. Abaixo do
/// `stop_grace_period` do compose (30 s), para o serviço sair sozinho antes
/// do SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(25);

/// Como a tela rotula um indexador cadastrado.
const ORIGIN: &str = "cadastro";

/// Sobe o servidor e só volta no SIGTERM ou no Ctrl-C.
///
/// # Errors
///
/// Configuração ilegível no banco ou endereço ocupado.
pub async fn run(store: Store, bind: &str) -> Result<()> {
    let settings = Arc::new(Settings::load(store.clone()).await?);
    let records = store.indexers().await.context("lendo os indexadores")?;
    let rows = store
        .definitions()
        .await
        .context("lendo as definições do banco")?;
    let registry = Arc::new(Registry::new(&settings.get(), &rows, records));
    drop(rows);
    let catalog = Catalog::new(entries(&settings.get(), &registry).await)?;
    let database = Database::connected(store);
    // O andamento da busca dos que faltam, que a tela de filmes acompanha.
    let missing = Arc::new(Progress::default());
    let tasks = Arc::new(Tasks::new(
        database.clone(),
        crate::tasks::service(&settings, &database, &catalog, &missing),
    ));
    tasks.watch_settings(settings.subscribe());
    tasks.start().await;
    // Indexador falhando e pouco espaço em disco viram aviso na mudança.
    tokio::spawn(
        Arc::new(crate::health::Monitor::new(
            Arc::clone(&settings),
            database.clone(),
            catalog.clone(),
        ))
        .run(tasks.cancel_token()),
    );
    let accounts = Arc::new(database.clone());
    // A chave é lida a cada requisição: trocada na tela, vale na hora.
    let api_key = ApiKey::dynamic({
        let settings = Arc::clone(&settings);
        move || settings.get().server.api_key.clone()
    });
    let web = Arc::new(crate::web::Web {
        settings: Arc::clone(&settings),
        database: database.clone(),
        catalog: catalog.clone(),
        searches: tokio::sync::Mutex::default(),
        series_searches: tokio::sync::Mutex::default(),
    });
    let readiness = Arc::new(crate::health::Readiness {
        settings: Arc::clone(&settings),
        database: database.clone(),
    });
    let admin = HubAdmin::new(
        settings,
        catalog.clone(),
        database,
        missing,
        Arc::clone(&tasks),
        registry,
    );
    tracing::info!(indexadores = catalog.len(), bind = %bind, "servindo a interface web");

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("abrindo `{bind}`"))?;
    axum::serve(
        listener,
        crate::api::router(catalog, api_key, Arc::new(admin), accounts)
            .merge(crate::web::router(Arc::clone(&web)))
            .merge(crate::agenda::router(Arc::clone(&web)))
            .merge(crate::marks::router(Arc::clone(&web)))
            .merge(crate::discover::router(Arc::clone(&web)))
            .merge(crate::series::web::router(web))
            .merge(crate::health::router(readiness)),
    )
    .with_graceful_shutdown({
        let tasks = Arc::clone(&tasks);
        async move {
            shutdown().await;
            // Para de agendar já; quem está rodando termina em `drain`.
            tasks.stop();
        }
    })
    .await
    .context("servidor HTTP")?;
    let running = tasks.drain(SHUTDOWN_GRACE).await;
    if running > 0 {
        tracing::warn!(
            tarefas = running,
            "tarefas ainda rodando após {} s; saindo assim mesmo",
            SHUTDOWN_GRACE.as_secs()
        );
    }
    Ok(())
}

/// Os indexadores cadastrados: os cadastros ativos que sobem.
///
/// Cadastro que não sobe — definição ilegível, configuração de rede inválida — fica de fora, com o motivo no log, sem derrubar os outros nem a
/// subida: a tela continua de pé para consertá-lo, e ele aparece lá como
/// desativado até ser reativado.
pub async fn entries(config: &Config, registry: &Registry) -> Vec<Entry> {
    let mut entries = Vec::new();
    for record in registry.records().iter().filter(|record| record.enabled) {
        match build(record, config, registry).await {
            Ok(entry) => entries.push(entry),
            Err(error) => tracing::error!(
                indexer = record.name,
                "fora do catálogo até ser reativado: {error:#}"
            ),
        }
    }
    entries
}

fn now() -> String {
    crate::decide::now_rfc3339()
}

/// Administração pela interface.
#[derive(Debug)]
pub(crate) struct HubAdmin {
    settings: Arc<Settings>,
    /// O catálogo servido: a busca dos que faltam e a da tela passam por ele
    /// e dividem as consultas guardadas.
    catalog: Catalog,
    database: Database,
    /// Andamento da busca dos que faltam.
    missing: Arc<Progress>,
    /// As tarefas de fundo; o botão de buscar os que faltam dispara a `busca`.
    tasks: Arc<Tasks>,
    /// Os cadastros, as definições em uso e o cache do catálogo remoto.
    registry: Arc<Registry>,
}

/// Os dois settings do cadastro que não são da definição: proxy e
/// `FlareSolverr`.
fn network_views(record: Option<&IndexerRecord>, with_flaresolverr: bool) -> Vec<SettingView> {
    let value = |name: &str| record.and_then(|record| record.settings.get(name).cloned());
    let mut views = vec![plain(
        USE_PROXY,
        "Sair pelo proxy do servidor",
        "checkbox",
        Some(value(USE_PROXY).unwrap_or_else(|| "false".into())),
    )];
    if with_flaresolverr {
        views.push(SettingView {
            name: USE_FLARESOLVERR.into(),
            label: "Usar o FlareSolverr diante do desafio do Cloudflare".into(),
            kind: "select",
            options: FLARESOLVERR_OPTIONS
                .iter()
                .map(|o| (*o).to_owned())
                .collect(),
            secret: false,
            is_set: true,
            value: Some(value(USE_FLARESOLVERR).unwrap_or_else(|| FLARESOLVERR_OPTIONS[0].into())),
        });
    }
    views
}

/// A rede do servidor mudou: os clientes precisam ser remontados.
fn network_changed(before: &ServerConfig, after: &ServerConfig) -> bool {
    before.proxy_url != after.proxy_url
        || before.proxy_username != after.proxy_username
        || before.proxy_password != after.proxy_password
        || before.flaresolverr_url != after.flaresolverr_url
        || before.flaresolverr_timeout_s != after.flaresolverr_timeout_s
}

impl HubAdmin {
    fn new(
        settings: Arc<Settings>,
        catalog: Catalog,
        database: Database,
        missing: Arc<Progress>,
        tasks: Arc<Tasks>,
        registry: Arc<Registry>,
    ) -> Self {
        Self {
            settings,
            catalog,
            database,
            missing,
            tasks,
            registry,
        }
    }

    fn record(&self, name: &str) -> Option<IndexerRecord> {
        self.registry.record(name)
    }

    fn store(&self) -> Result<&Store, String> {
        self.database.get()
    }

    /// Grava o cadastro — novo ou alterado — e o espelho.
    async fn persist(&self, record: IndexerRecord, new: bool) -> Result<(), String> {
        let store = self.store()?;
        let done = if new {
            store.insert_indexer(&record).await
        } else {
            store.update_indexer(&record).await
        }
        .map_err(|e| e.to_string())?;
        if !done {
            return Err(if new {
                format!("já existe um indexador chamado {}", record.name)
            } else {
                "indexador desconhecido".into()
            });
        }
        self.registry
            .records
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(record.name.clone(), record);
        Ok(())
    }

    /// Remonta todos os indexadores cadastrados — o proxy ou
    /// o `FlareSolverr` mudaram, e eles moram dentro de cada cliente.
    async fn reload_catalog(&self) {
        let records: Vec<_> = self
            .registry
            .records()
            .into_iter()
            .filter(|record| record.enabled)
            .collect();
        let config = self.settings.get();
        for record in records {
            self.catalog.remove(&record.name);
            match build(&record, &config, &self.registry).await {
                Ok(entry) => {
                    if let Err(error) = self.catalog.insert(entry) {
                        tracing::error!(indexer = record.name, "fora do catálogo: {error}");
                    }
                }
                Err(error) => {
                    tracing::error!(indexer = record.name, "fora do catálogo: {error:#}");
                }
            }
        }
    }

    /// O andamento da busca dos que faltam, como a tela de filmes o lê;
    /// `iniciada` diz se este pedido disparou a busca (falso: já havia uma).
    fn missing_json(&self, started: bool) -> serde_json::Value {
        let view = self.missing.view();
        // Pedida e ainda não começada: o andamento é o da busca anterior.
        let (done, total) = if view.rodando {
            (view.buscados, view.total)
        } else {
            (0, 0)
        };
        json!({
            "iniciada": started,
            "rodando": view.rodando || self.tasks.is_running(BUSCA),
            "buscados": done,
            "total": total,
        })
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

fn message(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

impl HubAdmin {
    pub(crate) fn settings(&self, indexer: &str) -> Option<Vec<SettingView>> {
        let record = self.record(indexer)?;
        if record.kind == CARDIGANN {
            let definition = self.registry.definition(&record).ok()?;
            let mut views: Vec<_> = definition
                .settings()
                .iter()
                .map(|info| view(info, record.settings.get(&info.name)))
                .collect();
            views.extend(network_views(Some(&record), true));
            return Some(views);
        }
        None
    }

    pub(crate) async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        let _guard = self.registry.write.lock().await;
        let mut record = self.record(indexer).ok_or("indexador desconhecido")?;
        let config = self.settings.get();
        let mut values = values;
        let reserved_values: Vec<(String, String)> = values
            .keys()
            .filter(|name| reserved(name))
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|name| values.remove_entry(&name))
            .collect();
        for (name, value) in reserved_values {
            if name == USE_FLARESOLVERR && record.kind != CARDIGANN {
                return Err(format!("setting desconhecido: {name}"));
            }
            check_reserved(&name, &value)?;
            record.settings.insert(name, value);
        }
        let entry = if record.kind == CARDIGANN {
            let definition = self.registry.definition(&record).map_err(|e| message(&e))?;
            let declared = definition.settings();
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
                record.settings.insert(name, value);
            }
            cardigann(&record, definition, &config).map_err(|e| message(&e))?
        } else {
            return Err("tipo de indexador desconhecido".into());
        };
        // Monta antes de gravar: credencial que não sobe não substitui a boa.
        self.persist(record, false).await?;
        tracing::info!(indexer, "settings atualizados pela interface");
        Ok(entry)
    }

    pub(crate) fn origin(&self, indexer: &str) -> Option<&'static str> {
        self.record(indexer).map(|_| ORIGIN)
    }

    pub(crate) fn disabled(&self) -> Vec<(String, &'static str)> {
        let served: HashSet<String> = self
            .catalog
            .views()
            .into_iter()
            .map(|view| view.name)
            .collect();
        self.registry
            .records
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .filter(|name| !served.contains(*name))
            .map(|name| (name.clone(), ORIGIN))
            .collect()
    }

    pub(crate) async fn definitions(&self) -> Result<DefinitionCatalog, String> {
        let definitions = self.registry.available().await.map_err(|e| message(&e))?;
        Ok(DefinitionCatalog {
            warning: definitions.warning,
            definitions: definitions
                .definitions
                .iter()
                .map(|known| DefinitionView {
                    id: known.header.id.clone(),
                    name: known.header.name.clone(),
                    description: known.header.description.clone(),
                    language: known.header.language.clone(),
                    private: known.header.private,
                    supported: known.refusal.is_none(),
                    reason: known.refusal.clone(),
                    added: self.record(&known.header.id).is_some(),
                    settings: known
                        .settings
                        .iter()
                        .map(|info| view(info, None))
                        .chain(network_views(None, true))
                        .collect(),
                })
                .collect(),
        })
    }

    pub(crate) fn definition_settings(&self, definition: &str) -> Option<Vec<SettingView>> {
        let yaml = {
            let definitions = self.registry.available_cached();
            let known = definitions.get(definition)?;
            if known.refusal.is_some() {
                return None;
            }
            known.yaml().ok()?
        };
        let parsed = CardigannDefinition::from_yaml_v11(&yaml).ok()?;
        let mut views: Vec<_> = parsed
            .settings()
            .iter()
            .map(|info| view(info, None))
            .collect();
        views.extend(network_views(None, true));
        Some(views)
    }

    pub(crate) async fn add(
        &self,
        definition: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        let _guard = self.registry.write.lock().await;
        let config = self.settings.get();
        if self.registry.available_cached().get(definition).is_none() {
            self.registry.available().await.map_err(|e| message(&e))?;
        }
        let known = self
            .registry
            .available_cached()
            .get(definition)
            .cloned()
            .ok_or("definição desconhecida")?;
        if let Some(reason) = &known.refusal {
            return Err(format!("definição ainda não suportada: {reason}"));
        }
        if self.record(definition).is_some() {
            return Err(format!("{definition} já está adicionado"));
        }
        let yaml = known.yaml().map_err(|e| message(&e))?;
        let parsed = CardigannDefinition::from_yaml_v11(&yaml).map_err(|e| e.to_string())?;
        let declared = parsed.settings();
        let mut settings = BTreeMap::new();
        for (name, value) in values {
            if reserved(&name) {
                check_reserved(&name, &value)?;
                settings.insert(name, value);
                continue;
            }
            if !declared.iter().any(|info| info.name == name) {
                return Err(format!("setting desconhecido: {name}"));
            }
            if !value.is_empty() {
                settings.insert(name, value);
            }
        }
        // A definição local fica fixada no arquivo; a remota é guardada no banco.
        let record = IndexerRecord {
            name: definition.to_owned(),
            kind: CARDIGANN.into(),
            definition: known
                .is_local()
                .then(|| known.path().map(|path| path.display().to_string()))
                .flatten(),
            url: None,
            settings,
            enabled: true,
            added_at: Some(now()),
        };
        let entry = cardigann(&record, parsed, &config).map_err(|e| message(&e))?;
        let row = (!known.is_local()).then(|| acervo_store::DefinitionRow {
            id: definition.to_owned(),
            sha: crate::definitions::sha(&yaml),
            yaml: yaml.clone(),
            updated_at: now(),
        });
        if !self
            .store()?
            .insert_indexer_with_definition(&record, row.as_ref())
            .await
            .map_err(|e| e.to_string())?
        {
            return Err(format!("{definition} já está adicionado"));
        }
        self.registry
            .records
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(record.name.clone(), record);
        if row.is_some() {
            self.registry
                .definitions
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .offer(
                    crate::definitions::Source::Database(Arc::from(yaml.as_str())),
                    &yaml,
                );
        }
        tracing::info!(indexer = definition, "indexador adicionado pela interface");
        Ok(entry)
    }

    pub(crate) async fn remove(&self, indexer: &str) -> Result<(), String> {
        let _guard = self.registry.write.lock().await;
        if !self
            .store()?
            .delete_indexer(indexer)
            .await
            .map_err(|e| e.to_string())?
        {
            return Err("indexador desconhecido".into());
        }
        self.registry
            .records
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(indexer);
        tracing::info!(indexer, "indexador removido pela interface");
        Ok(())
    }

    pub(crate) async fn set_enabled(
        &self,
        indexer: &str,
        enabled: bool,
    ) -> Result<Option<Entry>, String> {
        let _guard = self.registry.write.lock().await;
        let mut record = self.record(indexer).ok_or("indexador desconhecido")?;
        record.enabled = enabled;
        if !enabled {
            self.persist(record, false).await?;
            return Ok(None);
        }
        // Monta antes de gravar: se não sobe, continua desativado.
        let entry = build(&record, &self.settings.get(), &self.registry)
            .await
            .map_err(|e| message(&e))?;
        self.persist(record, false).await?;
        Ok(Some(entry))
    }

    pub(crate) fn tasks(&self) -> serde_json::Value {
        self.tasks.view()
    }

    pub(crate) async fn task_history(&self) -> Result<serde_json::Value, String> {
        self.tasks.history().await
    }

    pub(crate) fn run_task(&self, id: &str) -> Option<serde_json::Value> {
        let started = self.tasks.run_now(id)?;
        Some(json!({ "iniciada": started, "tarefa": self.tasks.view_one(id) }))
    }

    pub(crate) async fn movies(&self) -> Result<serde_json::Value, String> {
        let list = crate::movies::list(self.store()?)
            .await
            .map_err(|e| message(&e))?;
        serde_json::to_value(list).map_err(|e| e.to_string())
    }

    pub(crate) fn search_missing(&self) -> Result<serde_json::Value, String> {
        // Falha logo, na resposta, se o banco ainda não subiu.
        self.store()?;
        let started = self
            .tasks
            .run_now(BUSCA)
            .ok_or("a busca dos que faltam precisa do banco de dados")?;
        Ok(self.missing_json(started))
    }

    pub(crate) fn missing_status(&self) -> serde_json::Value {
        self.missing_json(false)
    }

    pub(crate) async fn grab_movie(
        &self,
        movie_id: i64,
        apply: bool,
    ) -> Result<serde_json::Value, String> {
        let report = crate::grab::grab(
            &self.settings.get(),
            self.store()?,
            &self.catalog,
            movie_id,
            apply,
        )
        .await
        .map_err(|e| message(&e))?;
        serde_json::to_value(report).map_err(|e| e.to_string())
    }

    pub(crate) async fn configuration(&self) -> Result<serde_json::Value, String> {
        let store = self.store()?;
        let key = store
            .setting(crate::metadata::TMDB_KEY)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "tmdb": { "definida": key.is_some() } }))
    }

    pub(crate) async fn save_configuration(
        &self,
        values: BTreeMap<String, Option<String>>,
    ) -> Result<serde_json::Value, String> {
        let store = self.store()?;
        let at = now();
        for (name, value) in values {
            match name.as_str() {
                "tmdb_chave" => {
                    let value = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
                    if let Some(key) = &value {
                        acervo_metadata::Tmdb::new(
                            key,
                            crate::metadata::LANGUAGE,
                            crate::config::HTTP_TIMEOUT,
                        )
                        .map_err(|e| e.to_string())?
                        .validate()
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                    store
                        .set_setting(crate::metadata::TMDB_KEY, value.as_deref(), &at)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                other => return Err(format!("configuração desconhecida: `{other}`")),
            }
        }
        self.configuration().await
    }

    pub(crate) fn config_section(&self, section: &str) -> Result<serde_json::Value, String> {
        self.settings.view(section)
    }

    pub(crate) async fn save_config_section(
        &self,
        section: &str,
        value: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let before = self.settings.get();
        let saved = self.settings.save_section(section, value).await?;
        if section == SERVIDOR {
            let after = self.settings.get();
            let catalogs = after.server.catalogs != before.server.catalogs;
            if catalogs {
                let rows = self
                    .store()?
                    .definitions()
                    .await
                    .map_err(|e| e.to_string())?;
                let records = self
                    .registry
                    .records
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                self.registry.set_definitions(scan(&after, &rows, &records));
            }
            // A precedência mudou: a definição de um indexador em uso pode
            // ser outra agora.
            if catalogs || network_changed(&before.server, &after.server) {
                self.reload_catalog().await;
            }
        }
        Ok(saved)
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

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../acervo-indexers/tests/fixtures"
    );

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// Um admin sobre o banco de teste, com o diretório de fixtures como
    /// catálogo de definições.
    async fn admin(db: &acervo_store::testing::TestDb) -> (HubAdmin, Arc<Settings>) {
        let settings = Arc::new(Settings::load(db.store.clone()).await.unwrap());
        settings
            .save_section(SERVIDOR, json!({ "catalogos": [FIXTURES] }))
            .await
            .unwrap();
        let records = db.store.indexers().await.unwrap();
        let registry = Arc::new(Registry::new(&settings.get(), &[], records));
        let admin = HubAdmin::new(
            Arc::clone(&settings),
            Catalog::default(),
            Database::connected(db.store.clone()),
            Arc::default(),
            Arc::default(),
            registry,
        );
        (admin, settings)
    }

    #[tokio::test]
    async fn cardigann_cadastrado_edita_desativa_e_sai() {
        let Some(db) = acervo_store::testing::TestDb::new("serve_cardigann").await else {
            return;
        };
        let (admin, settings) = admin(&db).await;
        *admin.registry.remote.write().unwrap() = Some((
            std::time::Instant::now(),
            crate::definitions::Definitions::default(),
        ));
        let listed = admin.definitions().await.unwrap();
        let known = listed
            .definitions
            .iter()
            .find(|d| d.id == "cookie-privado")
            .unwrap();
        assert!(known.supported && !known.added);

        let entry = admin
            .add("cookie-privado", values(&[("cookie", "do-cadastro")]))
            .await
            .unwrap();
        admin.catalog.insert(entry).unwrap();
        assert_eq!(admin.origin("cookie-privado"), Some(ORIGIN));
        assert!(admin.disabled().is_empty());
        let stored = db.store.indexers().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].settings["cookie"], "do-cadastro");
        assert_eq!(entries(&settings.get(), &admin.registry).await.len(), 1);

        // Segredo em branco mantém; o resto troca, e nada volta com valor.
        admin
            .update(
                "cookie-privado",
                values(&[("freeleech", "true"), ("cookie", "")]),
            )
            .await
            .unwrap();
        let views = admin.settings("cookie-privado").unwrap();
        let freeleech = views.iter().find(|s| s.name == "freeleech").unwrap();
        assert_eq!(freeleech.value.as_deref(), Some("true"));
        let cookie = views.iter().find(|s| s.name == "cookie").unwrap();
        assert!(cookie.is_set && cookie.value.is_none());
        assert_eq!(
            db.store.indexers().await.unwrap()[0].settings["cookie"],
            "do-cadastro"
        );

        // Proxy e FlareSolverr são do cadastro, não da definição.
        let views = admin.settings("cookie-privado").unwrap();
        let flaresolverr = views.iter().find(|s| s.name == USE_FLARESOLVERR).unwrap();
        assert_eq!(flaresolverr.value.as_deref(), Some("automático"));
        assert_eq!(flaresolverr.options, FLARESOLVERR_OPTIONS);
        admin
            .update("cookie-privado", values(&[(USE_FLARESOLVERR, "sim")]))
            .await
            .unwrap();
        assert_eq!(
            db.store.indexers().await.unwrap()[0].settings[USE_FLARESOLVERR],
            "sim"
        );
        // Pedir o proxy sem o servidor ter um não monta, e nada muda.
        let error = admin
            .update("cookie-privado", values(&[(USE_PROXY, "true")]))
            .await
            .unwrap_err();
        assert!(error.contains("proxy"), "{error}");
        assert!(
            !db.store.indexers().await.unwrap()[0]
                .settings
                .contains_key(USE_PROXY)
        );
        assert!(
            admin
                .update("cookie-privado", values(&[(USE_FLARESOLVERR, "talvez")]))
                .await
                .is_err()
        );

        // Desativado: fora do catálogo servido, ainda listado.
        assert!(
            admin
                .set_enabled("cookie-privado", false)
                .await
                .unwrap()
                .is_none()
        );
        admin.catalog.remove("cookie-privado");
        assert_eq!(admin.disabled(), [("cookie-privado".to_owned(), ORIGIN)]);
        assert!(entries(&settings.get(), &admin.registry).await.is_empty());

        admin.remove("cookie-privado").await.unwrap();
        assert_eq!(admin.origin("cookie-privado"), None);
        assert!(db.store.indexers().await.unwrap().is_empty());
        assert!(admin.remove("cookie-privado").await.is_err());
        db.drop().await;
    }

    async fn call(
        client: &reqwest::Client,
        method: reqwest::Method,
        url: &str,
        key: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = client
            .request(method, url)
            .header("X-Api-Key", key)
            .header("X-Acervo", "1");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta, pela API.
    async fn api_de_configuracoes_nunca_devolve_segredo() {
        let Some(db) = acervo_store::testing::TestDb::new("serve_api_config").await else {
            return;
        };
        let (admin, settings) = admin(&db).await;
        let first = "chave-inicial-0123456789";
        settings
            .save_section(SERVIDOR, json!({ "api_key": first }))
            .await
            .unwrap();
        let api_key = ApiKey::dynamic({
            let settings = Arc::clone(&settings);
            move || settings.get().server.api_key.clone()
        });
        let app = crate::api::router(
            Catalog::default(),
            api_key,
            Arc::new(admin),
            Arc::new(Database::default()),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::new();
        let url = |section: &str| format!("{base}/ui/api/configuracoes/{section}");

        let (status, body) = call(
            &client,
            reqwest::Method::PUT,
            &url("qbittorrent"),
            first,
            Some(json!({ "url": "http://qbit:8080", "username": "u", "password": "senha-qbit" })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["password"], json!({ "definida": true }));
        let (status, body) = call(
            &client,
            reqwest::Method::GET,
            &url("qbittorrent"),
            first,
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["url"], "http://qbit:8080");
        assert!(!body.to_string().contains("senha-qbit"));

        // Jellyfin sem chave é recusado; seção que não existe mais, 404.
        let (status, _) = call(
            &client,
            reqwest::Method::GET,
            &url("gerenciadores"),
            first,
            None,
        )
        .await;
        assert_eq!(status, 404);
        let (status, body) = call(
            &client,
            reqwest::Method::PUT,
            &url("jellyfin"),
            first,
            Some(json!({ "url": "http://j:8096" })),
        )
        .await;
        assert_eq!(status, 422);
        assert!(body["erro"].as_str().unwrap().contains("chave"), "{body}");

        // Validação em português; seção desconhecida.
        let (status, body) = call(
            &client,
            reqwest::Method::PUT,
            &url("tarefas"),
            first,
            Some(json!({ "intervalos": { "rss": 1 } })),
        )
        .await;
        assert_eq!(status, 422);
        assert!(
            body["erro"].as_str().unwrap().contains("desconhecida"),
            "{body}"
        );
        let (status, _) = call(&client, reqwest::Method::GET, &url("outra"), first, None).await;
        assert_eq!(status, 404);

        // A chave do servidor nunca volta; trocada, vale na hora.
        let (_, body) = call(&client, reqwest::Method::GET, &url("servidor"), first, None).await;
        assert_eq!(body["api_key"], json!({ "definida": true }));
        assert!(!body.to_string().contains(first));
        let second = "chave-nova-abcdefghijklmn";
        let (status, body) = call(
            &client,
            reqwest::Method::PUT,
            &url("servidor"),
            first,
            Some(json!({ "api_key": second })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert!(!body.to_string().contains(second));
        let (status, _) = call(&client, reqwest::Method::GET, &url("servidor"), first, None).await;
        assert_eq!(status, 401);
        let (status, _) = call(
            &client,
            reqwest::Method::GET,
            &url("servidor"),
            second,
            None,
        )
        .await;
        assert_eq!(status, 200);
        db.drop().await;
    }
}

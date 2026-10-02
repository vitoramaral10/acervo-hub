//! Importador TEMPORÁRIO do `config.toml` e dos arquivos de estado antigos.
//!
//! Único lugar do código que conhece TOML e os arquivos de `[state]`
//! (credenciais, registro de indexadores e strikes). Roda no início do
//! `serve` quando `ACERVO_IMPORT_CONFIG` aponta para um arquivo e o banco
//! ainda não tem configuração nenhuma; grava tudo numa transação e, dali em
//! diante, não roda mais. Quando todas as instalações tiverem migrado, este
//! módulo sai inteiro, com a dependência `toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use acervo_indexers::CardigannDefinition;
use acervo_store::{Imported, IndexerRecord, Store};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{
    Config, InstanceConfig, JellyfinConfig, LibraryConfig, PolicyConfig, QbitConfig, SECTIONS,
    ServerConfig, TasksConfig,
};

/// Variável que aponta o `config.toml` a importar.
const ENV: &str = "ACERVO_IMPORT_CONFIG";

// --- O formato antigo, campo a campo ---------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    qbittorrent: Option<QbitConfig>,
    #[serde(default)]
    instances: Vec<InstanceConfig>,
    #[serde(default)]
    paths: BTreeMap<String, String>,
    #[serde(default)]
    library: FileLibrary,
    #[serde(default)]
    policy: PolicyConfig,
    #[serde(default)]
    state: FileState,
    #[serde(default = "default_timeout")]
    http_timeout_seconds: u64,
    #[serde(default)]
    server: Option<FileServer>,
    #[serde(default)]
    indexers: Vec<FileIndexer>,
    /// Agora vem de `ACERVO_DATABASE_URL`; aceito para o arquivo ler, e
    /// ignorado.
    #[serde(default)]
    database: Option<toml::Value>,
    #[serde(default)]
    movies: FileMovies,
    #[serde(default)]
    jellyfin: Option<FileJellyfin>,
}

const fn default_timeout() -> u64 {
    30
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLibrary {
    #[serde(default)]
    roots: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileState {
    #[serde(default = "default_ledger")]
    ledger: PathBuf,
    #[serde(default = "default_credentials")]
    credentials: PathBuf,
    #[serde(default = "default_registry")]
    registry: PathBuf,
}

impl Default for FileState {
    fn default() -> Self {
        Self {
            ledger: default_ledger(),
            credentials: default_credentials(),
            registry: default_registry(),
        }
    }
}

fn default_ledger() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/strikes.json")
}
fn default_credentials() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/credenciais.toml")
}
fn default_registry() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/indexadores.toml")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServer {
    /// Agora vem de `ACERVO_BIND`; ignorado.
    #[serde(default)]
    bind: Option<String>,
    api_key: String,
    #[serde(default)]
    public_url: Option<String>,
    #[serde(default)]
    catalogs: Vec<PathBuf>,
    #[serde(default)]
    search_interval_minutes: Option<u64>,
    #[serde(default = "default_search_limit")]
    search_limit: usize,
    #[serde(default = "default_cleanup_interval")]
    cleanup_interval_minutes: u64,
}

const fn default_search_limit() -> usize {
    5
}
const fn default_cleanup_interval() -> u64 {
    60
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMovies {
    #[serde(default = "default_category")]
    category: String,
    #[serde(default = "default_import_interval")]
    import_interval_minutes: u64,
    #[serde(default = "default_root_folders")]
    root_folders: Vec<String>,
    #[serde(default = "default_rss_interval")]
    rss_interval_minutes: u64,
}

impl Default for FileMovies {
    fn default() -> Self {
        Self {
            category: default_category(),
            import_interval_minutes: default_import_interval(),
            root_folders: default_root_folders(),
            rss_interval_minutes: default_rss_interval(),
        }
    }
}

fn default_category() -> String {
    "acervo".into()
}
const fn default_import_interval() -> u64 {
    5
}
fn default_root_folders() -> Vec<String> {
    vec!["/media/movies".into()]
}
const fn default_rss_interval() -> u64 {
    30
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileJellyfin {
    url: String,
    api_key: String,
    #[serde(default = "default_watched_grace")]
    delete_watched_after_minutes: u64,
    #[serde(default = "default_watched_interval")]
    interval_minutes: u64,
}

const fn default_watched_grace() -> u64 {
    60
}
const fn default_watched_interval() -> u64 {
    15
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum FileIndexer {
    Torznab(FileTorznab),
    Cardigann(FileCardigann),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTorznab {
    name: String,
    url: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default = "default_request_interval")]
    request_interval_seconds: f64,
}

const fn default_request_interval() -> f64 {
    2.0
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCardigann {
    definition: PathBuf,
    #[serde(default)]
    link: usize,
    #[serde(default)]
    settings: BTreeMap<String, String>,
}

/// `indexadores.toml`: o que a tela adicionou, desativou e removeu.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default)]
    disabled: BTreeSet<String>,
    #[serde(default)]
    added: Vec<Added>,
    #[serde(default)]
    removed: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Added {
    Cardigann { definition: String, path: PathBuf },
    Torznab(FileTorznab),
}

/// `credenciais.toml`: por indexador, por setting.
type Overrides = BTreeMap<String, BTreeMap<String, String>>;

// --- Leitura ----------------------------------------------------------------

/// Expande `~` no início do caminho.
fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    std::env::var_os("HOME")
        .map_or_else(|| path.to_path_buf(), |home| PathBuf::from(home).join(rest))
}

/// Arquivo de estado opcional: ausente é vazio.
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("lendo `{}`", path.display())),
    }
}

fn read_toml<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    read_optional(path)?.map_or_else(
        || Ok(T::default()),
        |text| toml::from_str(&text).with_context(|| format!("interpretando `{}`", path.display())),
    )
}

fn read_ledger(path: &Path) -> Result<Vec<(String, u32)>> {
    let Some(text) = read_optional(path)? else {
        return Ok(Vec::new());
    };
    let map: BTreeMap<String, u32> = serde_json::from_str(&text)
        .with_context(|| format!("interpretando os strikes em `{}`", path.display()))?;
    Ok(map.into_iter().collect())
}

/// O que vai para o banco.
struct Converted {
    sections: Vec<(String, Value)>,
    indexers: Vec<IndexerRecord>,
    strikes: Vec<(String, u32)>,
}

fn torznab_record(
    spec: &FileTorznab,
    overrides: Option<&BTreeMap<String, String>>,
    enabled: bool,
    at: &str,
) -> IndexerRecord {
    let mut settings = BTreeMap::new();
    // URL e chave trocadas pela tela valiam por cima do arquivo.
    let url = overrides
        .and_then(|values| values.get("url"))
        .unwrap_or(&spec.url);
    if let Some(key) = overrides
        .and_then(|values| values.get("api_key"))
        .or(spec.api_key.as_ref())
    {
        settings.insert("api_key".to_owned(), key.clone());
    }
    if (spec.request_interval_seconds - default_request_interval()).abs() > f64::EPSILON {
        settings.insert(
            "request_interval_seconds".to_owned(),
            spec.request_interval_seconds.to_string(),
        );
    }
    IndexerRecord {
        name: spec.name.clone(),
        kind: "torznab".into(),
        definition: None,
        url: Some(url.clone()),
        settings,
        enabled,
        added_at: Some(at.to_owned()),
    }
}

fn convert(file: File, at: &str) -> Result<Converted> {
    if file.database.is_some() {
        tracing::info!("`[database]` ignorado: o banco agora vem de ACERVO_DATABASE_URL");
    }
    let server = file.server.as_ref();
    if server.and_then(|s| s.bind.as_ref()).is_some() {
        tracing::info!("`server.bind` ignorado: o endereço agora vem de ACERVO_BIND");
    }
    let mut tasks = TasksConfig::default();
    let mut interval = |id: &str, minutes: u64| {
        tasks.intervalos.insert(id.to_owned(), minutes);
    };
    interval(
        "busca",
        server.and_then(|s| s.search_interval_minutes).unwrap_or(0),
    );
    // O agendador antigo nunca rodava o RSS a menos de 5 minutos.
    interval("rss", file.movies.rss_interval_minutes.max(5));
    interval("importacao", file.movies.import_interval_minutes);
    interval("limpeza", server.map_or(60, |s| s.cleanup_interval_minutes));
    interval(
        "assistidos",
        file.jellyfin
            .as_ref()
            .map_or(default_watched_interval(), |j| j.interval_minutes),
    );
    tasks.search_limit = server.map_or(default_search_limit(), |s| s.search_limit);

    let config = Config {
        server: ServerConfig {
            api_key: server.map(|s| s.api_key.clone()).unwrap_or_default(),
            public_url: server.and_then(|s| s.public_url.clone()),
            catalogs: server
                .map(|s| s.catalogs.iter().map(|dir| expand_tilde(dir)).collect())
                .unwrap_or_default(),
            http_timeout_seconds: file.http_timeout_seconds,
        },
        qbittorrent: file.qbittorrent.unwrap_or_default(),
        jellyfin: file
            .jellyfin
            .as_ref()
            .map_or_else(JellyfinConfig::default, |j| JellyfinConfig {
                url: j.url.clone(),
                api_key: j.api_key.clone(),
                delete_watched_after_minutes: j.delete_watched_after_minutes,
            }),
        instances: file.instances,
        library: LibraryConfig {
            roots: file.library.roots.iter().map(|r| expand_tilde(r)).collect(),
            root_folders: file.movies.root_folders,
            category: file.movies.category,
            paths: file.paths,
        },
        policy: file.policy,
        tasks,
    };
    config
        .validate()
        .map_err(anyhow::Error::msg)
        .context("a configuração do arquivo não passa na validação")?;
    let sections = SECTIONS
        .iter()
        .map(|name| {
            Ok((
                (*name).to_owned(),
                config.section(name).map_err(anyhow::Error::msg)?,
            ))
        })
        .collect::<Result<_>>()?;

    let indexers = indexers(&file.indexers, &file.state, at)?;
    Ok(Converted {
        sections,
        indexers,
        strikes: read_ledger(&expand_tilde(&file.state.ledger))?,
    })
}

/// Os indexadores do arquivo e os da tela, com as credenciais da tela por
/// cima e os removidos de fora.
fn indexers(specs: &[FileIndexer], state: &FileState, at: &str) -> Result<Vec<IndexerRecord>> {
    let overrides: Overrides = read_toml(&expand_tilde(&state.credentials))?;
    let registry: Registry = read_toml(&expand_tilde(&state.registry))?;
    let enabled = |name: &str| !registry.disabled.contains(name);
    let mut indexers: Vec<IndexerRecord> = Vec::new();
    for spec in specs {
        match spec {
            FileIndexer::Torznab(spec) => {
                if registry.removed.contains(&spec.name) {
                    continue;
                }
                indexers.push(torznab_record(
                    spec,
                    overrides.get(&spec.name),
                    enabled(&spec.name),
                    at,
                ));
            }
            FileIndexer::Cardigann(spec) => {
                let path = expand_tilde(&spec.definition);
                let yaml = std::fs::read_to_string(&path)
                    .with_context(|| format!("lendo a definição `{}`", path.display()))?;
                let definition = CardigannDefinition::from_yaml_v11(&yaml)
                    .with_context(|| format!("carregando a definição `{}`", path.display()))?;
                let id = definition.id().to_owned();
                if registry.removed.contains(&id) {
                    continue;
                }
                // O índice do link vira a URL dele: sobrevive a definição
                // que reordene os links.
                let url = match spec.link {
                    0 => None,
                    n => Some(
                        definition
                            .links()
                            .get(n)
                            .with_context(|| format!("`{id}`: link {n} não existe na definição"))?
                            .to_string(),
                    ),
                };
                let mut settings = spec.settings.clone();
                if let Some(values) = overrides.get(&id) {
                    settings.extend(values.clone());
                }
                indexers.push(IndexerRecord {
                    enabled: enabled(&id),
                    name: id,
                    kind: "cardigann".into(),
                    definition: Some(path.display().to_string()),
                    url,
                    settings,
                    added_at: Some(at.to_owned()),
                });
            }
        }
    }
    for added in &registry.added {
        indexers.push(match added {
            Added::Cardigann { definition, path } => IndexerRecord {
                name: definition.clone(),
                kind: "cardigann".into(),
                definition: Some(expand_tilde(path).display().to_string()),
                url: None,
                settings: overrides.get(definition).cloned().unwrap_or_default(),
                enabled: enabled(definition),
                added_at: Some(at.to_owned()),
            },
            Added::Torznab(spec) => {
                torznab_record(spec, overrides.get(&spec.name), enabled(&spec.name), at)
            }
        });
    }
    let mut seen = BTreeSet::new();
    indexers.retain(|record| {
        let first = seen.insert(record.name.clone());
        if !first {
            tracing::warn!(
                indexer = record.name,
                "nome repetido no arquivo; fica o primeiro"
            );
        }
        first
    });

    Ok(indexers)
}

/// Importa o arquivo dado, se o banco ainda não tem configuração. `None`
/// quando já tinha — nada é lido nem gravado.
///
/// # Errors
///
/// Arquivo ilegível ou inválido, definição ou arquivo de estado ilegível,
/// configuração que não passa na validação, ou falha ao gravar — e então
/// nada fica gravado.
pub async fn import_file(store: &Store, path: &Path) -> Result<Option<Imported>> {
    // Já importado (ou configurado pela tela): não lê nada, para que
    // arquivos de estado apagados depois não derrubem a subida.
    if !store.config_sections().await?.is_empty() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("lendo a configuração em `{}`", path.display()))?;
    let file: File = toml::from_str(&text)
        .with_context(|| format!("interpretando a configuração em `{}`", path.display()))?;
    let at = crate::decide::now_rfc3339();
    let converted = convert(file, &at)?;
    store
        .import_config(
            &converted.sections,
            &converted.indexers,
            &converted.strikes,
            &at,
        )
        .await
        .context("gravando a configuração importada")
}

/// O passo do `serve`: importa se `ACERVO_IMPORT_CONFIG` aponta para um
/// arquivo e o banco está sem configuração.
///
/// # Errors
///
/// Como [`import_file`]: importação pedida que falha derruba a subida, em
/// vez de subir com o banco pela metade.
pub async fn run(store: &Store) -> Result<()> {
    let Some(path) = std::env::var_os(ENV).map(PathBuf::from) else {
        return Ok(());
    };
    if !path.is_file() {
        tracing::warn!(
            arquivo = %path.display(),
            "{ENV} não aponta para um arquivo; nada importado"
        );
        return Ok(());
    }
    if let Some(done) = import_file(store, &path).await? {
        tracing::info!(
            arquivo = %path.display(),
            secoes = done.sections,
            indexadores = done.indexers,
            strikes = done.strikes,
            "configuração importada para o banco; {ENV} pode sair do ambiente"
        );
    } else {
        tracing::info!("o banco já tem configuração: {ENV} ignorada");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../acervo-indexers/tests/fixtures"
    );

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Um `config.toml` com tudo e os três arquivos de estado ao lado.
    fn files() -> (Scratch, PathBuf) {
        let dir = std::env::temp_dir().join(format!("acervo-importa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = format!(
            r#"
            http_timeout_seconds = 20

            [qbittorrent]
            url = "http://qbit:8080"
            username = "u"
            password = "senha-qbit"

            [[instances]]
            name = "series"
            kind = "series"
            url = "http://sonarr:8989"
            api_key = "chave-sonarr"

            [paths]
            "/media" = "/mnt/acervo"

            [library]
            roots = ["/mnt/acervo/movies"]

            [policy]
            orphan_strikes = 4
            managed_categories = ["tv-sonarr", "filmes-acervo"]

            [state]
            ledger = "{dir}/strikes.json"
            credentials = "{dir}/credenciais.toml"
            registry = "{dir}/indexadores.toml"

            [database]
            url = "postgres://ignorado"

            [movies]
            category = "filmes-acervo"
            rss_interval_minutes = 45
            import_interval_minutes = 10

            [server]
            bind = "127.0.0.1:9797"
            api_key = "0123456789abcdef"
            public_url = "http://acervo:9797"
            catalogs = ["{fixtures}"]
            search_interval_minutes = 120
            search_limit = 3
            cleanup_interval_minutes = 0

            [jellyfin]
            url = "http://jellyfin:8096"
            api_key = "chave-jellyfin"
            interval_minutes = 20

            [[indexers]]
            kind = "torznab"
            name = "agregador"
            url = "http://prowlarr:9696/1/api"
            api_key = "chave-do-arquivo"

            [[indexers]]
            kind = "torznab"
            name = "removido"
            url = "http://x/api"

            [[indexers]]
            kind = "cardigann"
            definition = "{fixtures}/cardigann-cookie.yml"
            settings = {{ cookie = "do-arquivo", freeleech = "true" }}
            "#,
            dir = dir.display(),
            fixtures = FIXTURES,
        );
        let path = dir.join("config.toml");
        std::fs::write(&path, config).unwrap();
        std::fs::write(
            dir.join("indexadores.toml"),
            format!(
                r#"
                disabled = ["agregador"]
                removed = ["removido"]

                [[added]]
                kind = "cardigann"
                definition = "arquivo-publico"
                path = "{FIXTURES}/cardigann-public.yml"

                [[added]]
                kind = "torznab"
                name = "da-tela"
                url = "http://jackett:9117/api"
                request_interval_seconds = 5.0
                "#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("credenciais.toml"),
            r#"
            agregador = { api_key = "chave-da-tela" }
            cookie-privado = { cookie = "da-tela" }
            da-tela = { api_key = "k2" }
            "#,
        )
        .unwrap();
        std::fs::write(dir.join("strikes.json"), r#"{ "series/hash:aa": 2 }"#).unwrap();
        (Scratch(dir), path)
    }

    #[tokio::test]
    async fn importa_tudo_uma_vez_so() {
        let Some(db) = acervo_store::testing::TestDb::new("importa").await else {
            return;
        };
        let (_dir, path) = files();
        let done = import_file(&db.store, &path).await.unwrap().unwrap();
        assert_eq!(
            done,
            Imported {
                sections: 7,
                indexers: 4,
                strikes: 1
            }
        );

        let config = crate::settings::Settings::load(db.store.clone())
            .await
            .unwrap()
            .get();
        assert_eq!(config.server.api_key, "0123456789abcdef");
        assert_eq!(
            config.server.public_url.as_deref(),
            Some("http://acervo:9797")
        );
        assert_eq!(config.http_timeout().as_secs(), 20);
        assert_eq!(config.qbittorrent.password, "senha-qbit");
        assert_eq!(config.instances[0].api_key, "chave-sonarr");
        assert_eq!(config.jellyfin.api_key, "chave-jellyfin");
        assert_eq!(config.library.category, "filmes-acervo");
        assert_eq!(config.library.paths["/media"], "/mnt/acervo");
        assert_eq!(config.policy.orphan_strikes, 4);
        assert_eq!(config.tasks.minutes("busca"), 120);
        assert_eq!(config.tasks.minutes("rss"), 45);
        assert_eq!(config.tasks.minutes("importacao"), 10);
        assert_eq!(config.tasks.minutes("limpeza"), 0);
        assert_eq!(config.tasks.minutes("assistidos"), 20);
        assert_eq!(config.tasks.minutes("metadados"), 360);
        assert_eq!(config.tasks.search_limit, 3);

        let indexers: BTreeMap<String, IndexerRecord> = db
            .store
            .indexers()
            .await
            .unwrap()
            .into_iter()
            .map(|record| (record.name.clone(), record))
            .collect();
        let names: Vec<_> = indexers.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["agregador", "arquivo-publico", "cookie-privado", "da-tela"]
        );
        // A credencial da tela vale por cima do arquivo; o desativado segue
        // desativado; o removido não volta.
        assert!(!indexers["agregador"].enabled);
        assert_eq!(indexers["agregador"].settings["api_key"], "chave-da-tela");
        let cookie = &indexers["cookie-privado"];
        assert!(cookie.enabled);
        assert_eq!(cookie.settings["cookie"], "da-tela");
        assert_eq!(cookie.settings["freeleech"], "true");
        assert!(
            cookie
                .definition
                .as_deref()
                .unwrap()
                .ends_with("cardigann-cookie.yml")
        );
        assert_eq!(indexers["da-tela"].settings["api_key"], "k2");
        assert_eq!(
            indexers["da-tela"].settings["request_interval_seconds"],
            "5"
        );
        assert_eq!(
            db.store.strikes().await.unwrap(),
            [("series/hash:aa".to_owned(), 2)]
        );

        // Os cadastros sobem como qualquer outro.
        let catalog = crate::serve::entries(&config, std::slice::from_ref(cookie)).await;
        assert_eq!(catalog.len(), 1);

        // Segunda vez: nada muda, nem com o arquivo trocado.
        std::fs::write(&path, "isto nem é TOML válido = = =").unwrap();
        assert_eq!(import_file(&db.store, &path).await.unwrap(), None);
        assert_eq!(db.store.indexers().await.unwrap().len(), 4);
        db.drop().await;
    }

    #[tokio::test]
    async fn arquivo_invalido_nao_grava_nada() {
        let Some(db) = acervo_store::testing::TestDb::new("importa_invalido").await else {
            return;
        };
        let (_dir, path) = files();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("orphan_strikes = 4", "orphan_strikez = 4"),
        )
        .unwrap();
        let error = format!("{:#}", import_file(&db.store, &path).await.unwrap_err());
        assert!(error.contains("orphan_strikez"), "{error}");
        assert!(db.store.config_sections().await.unwrap().is_empty());
        assert!(db.store.indexers().await.unwrap().is_empty());
        db.drop().await;
    }
}

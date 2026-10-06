//! Catálogo local e em uso, cache remoto sob demanda e montagem dos indexadores.
//! A rede de cada cadastro preserva proxy e `FlareSolverr`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use crate::api::Entry;
use acervo_indexers::{
    CardigannClient, CardigannDefinition, Challenge, FlareSolverr, Network, Proxy,
};
use acervo_store::{DefinitionRow, IndexerRecord};
use anyhow::{Context, Result};

use crate::config::Config;
use crate::definitions::{self, Definitions};

pub const CARDIGANN: &str = "cardigann";

/// Setting do cadastro, fora da definição: o indexador sai pelo proxy do
/// servidor (`true`/`false`).
pub const USE_PROXY: &str = "acervo.proxy";
/// Setting do cadastro, fora da definição: `automático`, `sim` ou `não`.
pub const USE_FLARESOLVERR: &str = "acervo.flaresolverr";
/// As opções de [`USE_FLARESOLVERR`], na ordem da tela.
pub const FLARESOLVERR_OPTIONS: [&str; 3] = ["automático", "sim", "não"];

/// Setting que é do cadastro, e não da definição.
#[must_use]
pub fn reserved(name: &str) -> bool {
    name == USE_PROXY || name == USE_FLARESOLVERR
}

/// Confere o valor de um setting do cadastro.
///
/// # Errors
///
/// Valor fora das opções.
pub fn check_reserved(name: &str, value: &str) -> Result<(), String> {
    let ok = match name {
        USE_PROXY => matches!(value, "true" | "false"),
        USE_FLARESOLVERR => FLARESOLVERR_OPTIONS.contains(&value),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("valor inválido para {name}"))
    }
}

/// Catálogo disponível, com aviso quando a atualização remota falhou.
#[derive(Debug)]
pub struct Available {
    pub definitions: Definitions,
    pub warning: Option<String>,
}

/// Os cadastros em memória e os dois catálogos de definições.
#[derive(Debug, Default)]
pub struct Registry {
    pub definitions: RwLock<Definitions>,
    /// Os cadastros, espelhados em memória: as consultas da tela são
    /// síncronas, e só o serviço grava a tabela enquanto roda.
    pub records: RwLock<BTreeMap<String, IndexerRecord>>,
    /// Serializa as gravações: duas mudanças simultâneas não podem se apagar.
    pub write: tokio::sync::Mutex<()>,
    pub remote: RwLock<Option<(std::time::Instant, Definitions)>>,
    fetch: tokio::sync::Mutex<()>,
}

impl Registry {
    /// Monta o catálogo de definições e o espelho dos cadastros.
    #[must_use]
    pub fn new(config: &Config, rows: &[DefinitionRow], records: Vec<IndexerRecord>) -> Self {
        let records: BTreeMap<_, _> = records
            .into_iter()
            .map(|record| (record.name.clone(), record))
            .collect();
        Self {
            definitions: RwLock::new(scan(config, rows, &records)),
            records: RwLock::new(records),
            write: tokio::sync::Mutex::new(()),
            remote: RwLock::new(None),
            fetch: tokio::sync::Mutex::new(()),
        }
    }

    pub fn record(&self, name: &str) -> Option<IndexerRecord> {
        self.records
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    pub fn records(&self) -> Vec<IndexerRecord> {
        self.records
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    pub fn definitions(&self) -> std::sync::RwLockReadGuard<'_, Definitions> {
        self.definitions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_definitions(&self, definitions: Definitions) {
        *self
            .definitions
            .write()
            .unwrap_or_else(PoisonError::into_inner) = definitions;
    }

    /// Catálogo para adicionar: locais e definições em uso têm precedência.
    /// O download e a interpretação são compartilhados por quinze minutos.
    pub async fn available(&self) -> Result<Available> {
        self.available_with(async {
            let downloaded =
                definitions::download(crate::config::DEFINITIONS_URL, crate::config::HTTP_TIMEOUT)
                    .await?;
            tokio::task::spawn_blocking(move || {
                let mut remote = Definitions::default();
                for (_, yaml) in downloaded {
                    remote.offer(
                        definitions::Source::Database(Arc::from(yaml.as_str())),
                        &yaml,
                    );
                }
                remote
            })
            .await
            .map_err(anyhow::Error::from)
        })
        .await
    }

    /// O carregador é injetável para exercitar falhas sem acessar a rede.
    async fn available_with(
        &self,
        refresh: impl std::future::Future<Output = Result<Definitions>>,
    ) -> Result<Available> {
        let _fetch = self.fetch.lock().await;
        let fresh = self
            .remote
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(15 * 60));
        let mut warning = None;
        if !fresh {
            match refresh.await {
                Ok(remote) => {
                    *self.remote.write().unwrap_or_else(PoisonError::into_inner) =
                        Some((std::time::Instant::now(), remote));
                }
                Err(error) => {
                    let cached = self.available_cached();
                    if cached.iter().next().is_none() {
                        return Err(error);
                    }
                    tracing::warn!(%error, "catálogo remoto indisponível; usando definições locais e em cache");
                    warning = Some(format!(
                        "Não foi possível atualizar o catálogo remoto. As definições locais e em cache continuam disponíveis: {error:#}"
                    ));
                }
            }
        }
        Ok(Available {
            definitions: self.available_cached(),
            warning,
        })
    }

    pub fn available_cached(&self) -> Definitions {
        let mut all = self.definitions().clone();
        if let Some((_, remote)) = self
            .remote
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            all.extend(remote);
        }
        all
    }

    /// A definição de um cadastro Cardigann, pela precedência.
    ///
    /// # Errors
    ///
    /// Cadastro sem definição alcançável ou YAML que o executor recusa.
    pub fn definition(&self, record: &IndexerRecord) -> Result<CardigannDefinition> {
        let resolved = self
            .definitions()
            .resolve(&record.name, record.definition.as_deref())?;
        CardigannDefinition::from_yaml_v11(&resolved.yaml)
            .with_context(|| format!("carregando a definição de `{}`", record.name))
    }
}

/// O catálogo de definições: as duas fontes e as definições fixadas nos
/// cadastros, mesmo fora delas.
#[must_use]
pub fn scan(
    config: &Config,
    rows: &[DefinitionRow],
    records: &BTreeMap<String, IndexerRecord>,
) -> Definitions {
    let mut definitions = Definitions::assemble(&config.server.catalogs, rows);
    for record in records.values() {
        if let Some(path) = &record.definition {
            definitions.include(Path::new(path));
        }
    }
    definitions
}

/// A rede de um cadastro: o proxy, se ele o pede, e o `FlareSolverr` do
/// servidor no modo que ele escolheu.
///
/// # Errors
///
/// Cadastro que pede o proxy sem o servidor ter um, ou proxy e `FlareSolverr`
/// que não montam.
pub fn network(config: &Config, record: &IndexerRecord) -> Result<Network> {
    let server = &config.server;
    let proxy = if record.settings.get(USE_PROXY).is_some_and(|v| v == "true") {
        let url = server.proxy_url.as_deref().context(
            "o indexador sai pelo proxy, mas o servidor não tem um (Configurações → Servidor)",
        )?;
        Some(Proxy::new(
            url,
            Some(server.proxy_username.as_str()),
            Some(server.proxy_password.as_str()),
        )?)
    } else {
        None
    };
    let flaresolverr = server
        .flaresolverr_url
        .as_deref()
        .map(|url| FlareSolverr::new(url, Duration::from_secs(server.flaresolverr_timeout_s)))
        .transpose()?;
    let challenge = match record.settings.get(USE_FLARESOLVERR).map(String::as_str) {
        Some("sim") => Challenge::Always,
        Some("não" | "nao") => Challenge::Never,
        _ => Challenge::Auto,
    };
    Ok(Network {
        proxy,
        flaresolverr,
        challenge,
    })
}

/// Monta o cliente de um cadastro, sem olhar se ele está ativo.
///
/// # Errors
///
/// Definição ilegível ou recusada, settings inválidos, rede que não monta ou
/// configuração de rede inválida.
pub async fn build(record: &IndexerRecord, config: &Config, registry: &Registry) -> Result<Entry> {
    match record.kind.as_str() {
        CARDIGANN => cardigann(record, registry.definition(record)?, config),
        other => anyhow::bail!("tipo de indexador desconhecido: {other}"),
    }
}

/// O cliente Cardigann de um cadastro, com uma definição já carregada.
///
/// # Errors
///
/// Id que não bate, link que sumiu da definição, settings inválidos ou
/// rede que não monta.
pub fn cardigann(
    record: &IndexerRecord,
    definition: CardigannDefinition,
    config: &Config,
) -> Result<Entry> {
    let client = cardigann_client(record, definition, crate::config::HTTP_TIMEOUT)?
        .with_network(network(config, record)?)
        .with_context(|| format!("configurando a rede de `{}`", record.name))?;
    let capabilities = client.capabilities().clone();
    Ok(Entry {
        indexer: Arc::new(client),
        capabilities,
    })
}

/// O cliente com os settings do cadastro, antes de configurar sua rede.
fn cardigann_client(
    record: &IndexerRecord,
    definition: CardigannDefinition,
    timeout: Duration,
) -> Result<CardigannClient> {
    // O nome servido é o id da definição: arquivo trocado por outro com id
    // diferente não pode servir sob o nome velho.
    anyhow::ensure!(
        definition.id() == record.name,
        "a definição agora tem o id `{}`, e o cadastro é `{}`",
        definition.id(),
        record.name
    );
    // O link escolhido, guardado como URL; ausente é o primeiro.
    let link = match &record.url {
        Some(url) => definition
            .links()
            .iter()
            .position(|link| link.as_str() == url)
            .with_context(|| format!("o link `{url}` não está mais na definição"))?,
        None => 0,
    };
    let settings = record
        .settings
        .iter()
        .filter(|(name, _)| !reserved(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    CardigannClient::new(definition, link, settings, timeout)
        .with_context(|| format!("configurando a definição de `{}`", record.name))
}

#[cfg(test)]
mod tests {

    use super::*;

    fn record(name: &str, settings: &[(&str, &str)]) -> IndexerRecord {
        IndexerRecord {
            name: name.into(),
            kind: CARDIGANN.into(),
            definition: None,
            url: None,
            settings: settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            enabled: true,
            added_at: None,
        }
    }

    #[test]
    fn rede_segue_o_cadastro_e_o_servidor() {
        let mut config = Config::default();
        let plain = network(&config, &record("a", &[])).unwrap();
        assert_eq!(plain, Network::default());
        // Pede o proxy sem o servidor ter um: erro, e não saída direta.
        assert!(network(&config, &record("a", &[(USE_PROXY, "true")])).is_err());
        config.server.proxy_url = Some("socks5://proxy.invalid:1080".into());
        config.server.flaresolverr_url = Some("http://flaresolverr:8191".into());
        let proxied = network(
            &config,
            &record("a", &[(USE_PROXY, "true"), (USE_FLARESOLVERR, "sim")]),
        )
        .unwrap();
        assert!(proxied.proxy.is_some() && proxied.flaresolverr.is_some());
        assert_eq!(proxied.challenge, Challenge::Always);
        let never = network(&config, &record("a", &[(USE_FLARESOLVERR, "não")])).unwrap();
        assert!(never.proxy.is_none());
        assert_eq!(never.challenge, Challenge::Never);
        assert!(check_reserved(USE_FLARESOLVERR, "talvez").is_err());
        assert!(check_reserved(USE_PROXY, "true").is_ok());
    }
    #[tokio::test]
    async fn catalogo_remoto_em_cache_nao_carrega_indexadores_e_local_vence() {
        use crate::definitions::tests::{Dir, yaml};
        let local = Dir::new("catalogo-demanda", &[("um.yml", yaml("um", "Um local"))]);
        let mut config = Config::default();
        config.server.catalogs = vec![local.0.clone()];
        let registry = Registry::new(&config, &[], Vec::new());
        let mut remote = Definitions::default();
        for (id, name) in [("um", "Um remoto"), ("dois", "Dois remoto")] {
            let yaml = yaml(id, name);
            remote.offer(
                definitions::Source::Database(Arc::from(yaml.as_str())),
                &yaml,
            );
        }
        *registry.remote.write().unwrap() = Some((std::time::Instant::now(), remote));
        let available = registry.available().await.unwrap();
        assert_eq!(
            available.definitions.get("um").unwrap().header.name,
            "Um local"
        );
        assert_eq!(
            available.definitions.get("dois").unwrap().header.name,
            "Dois remoto"
        );
        assert!(registry.definitions().get("dois").is_none());
        assert!(registry.records().is_empty());
    }
    #[tokio::test]
    async fn falha_remota_preserva_locais_em_uso_e_cache_antigo() {
        use crate::definitions::tests::{Dir, yaml};
        let local = Dir::new("catalogo-fallback", &[("um.yml", yaml("um", "Um local"))]);
        let mut config = Config::default();
        config.server.catalogs = vec![local.0.clone()];
        let yaml_in_use = yaml("em-uso", "Em uso");
        let registry = Registry::new(
            &config,
            &[DefinitionRow {
                id: "em-uso".into(),
                yaml: yaml_in_use.clone(),
                sha: definitions::sha(&yaml_in_use),
                updated_at: String::new(),
            }],
            Vec::new(),
        );
        let mut remote = Definitions::default();
        for (id, name) in [("um", "Um remoto"), ("dois", "Dois remoto")] {
            let text = yaml(id, name);
            remote.offer(
                definitions::Source::Database(Arc::from(text.as_str())),
                &text,
            );
        }
        let expired = std::time::Instant::now()
            .checked_sub(Duration::from_secs(16 * 60))
            .unwrap();
        *registry.remote.write().unwrap() = Some((expired, remote));
        let available = registry
            .available_with(async { anyhow::bail!("falha simulada") })
            .await
            .unwrap();
        assert!(available.warning.unwrap().contains("falha simulada"));
        assert_eq!(
            available.definitions.get("um").unwrap().header.name,
            "Um local"
        );
        assert!(available.definitions.get("em-uso").is_some());
        assert!(available.definitions.get("dois").is_some());
        assert_eq!(registry.remote.read().unwrap().as_ref().unwrap().0, expired);

        // Sem cache remoto, os locais e em uso ainda permitem cadastrar.
        *registry.remote.write().unwrap() = None;
        let available = registry
            .available_with(async { anyhow::bail!("falha simulada") })
            .await
            .unwrap();
        assert!(available.warning.is_some());
        assert_eq!(available.definitions.iter().count(), 2);
    }

    #[tokio::test]
    async fn falha_remota_sem_locais_usa_cache_antigo() {
        use crate::definitions::tests::yaml;
        let registry = Registry::default();
        let mut remote = Definitions::default();
        let text = yaml("um", "Um remoto");
        remote.offer(
            definitions::Source::Database(Arc::from(text.as_str())),
            &text,
        );
        *registry.remote.write().unwrap() = Some((
            std::time::Instant::now()
                .checked_sub(Duration::from_secs(16 * 60))
                .unwrap(),
            remote,
        ));
        let available = registry
            .available_with(async { anyhow::bail!("falha simulada") })
            .await
            .unwrap();
        assert!(available.warning.is_some());
        assert!(available.definitions.get("um").is_some());
    }

    #[tokio::test]
    async fn falha_remota_so_e_erro_sem_nenhuma_definicao() {
        let error = Registry::default()
            .available_with(async { anyhow::bail!("falha simulada") })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "falha simulada");
    }
}

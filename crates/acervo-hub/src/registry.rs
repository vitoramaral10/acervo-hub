//! Os indexadores cadastrados e o catálogo de definições, divididos entre a
//! administração pela tela e a tarefa que atualiza as definições: os dois
//! leem os mesmos cadastros e gravam um de cada vez.
//!
//! Aqui também mora a montagem de um indexador a partir do cadastro — a
//! definição pela precedência, os settings e a rede (proxy e `FlareSolverr`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use acervo_api::{Catalog, Entry};
use acervo_indexers::{
    CardigannClient, CardigannDefinition, Challenge, FlareSolverr, Network, Proxy, TorznabClient,
};
use acervo_store::{DefinitionRow, IndexerRecord, Store};
use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::definitions::{self, Definitions};

pub const TORZNAB: &str = "torznab";
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

/// O que a tela administra, em memória, e o que a tarefa `definicoes` lê.
#[derive(Debug, Default)]
pub struct Registry {
    pub definitions: RwLock<Definitions>,
    /// Os cadastros, espelhados em memória: as consultas da tela são
    /// síncronas, e só o serviço grava a tabela enquanto roda.
    pub records: RwLock<BTreeMap<String, IndexerRecord>>,
    /// Serializa as gravações: duas mudanças simultâneas não podem se apagar.
    pub write: tokio::sync::Mutex<()>,
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

/// O catálogo de definições: as três fontes e as definições fixadas nos
/// cadastros, mesmo fora delas.
#[must_use]
pub fn scan(
    config: &Config,
    rows: &[DefinitionRow],
    records: &BTreeMap<String, IndexerRecord>,
) -> Definitions {
    let mut definitions = Definitions::assemble(
        &config.server.catalogs,
        rows,
        &config.server.reserve_catalogs,
    );
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
/// endpoint Torznab que não responde `caps`.
pub async fn build(record: &IndexerRecord, config: &Config, registry: &Registry) -> Result<Entry> {
    match record.kind.as_str() {
        CARDIGANN => cardigann(record, registry.definition(record)?, config),
        TORZNAB => torznab(record, config).await,
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
    let client = cardigann_client(record, definition, config.http_timeout())?
        .with_network(network(config, record)?)
        .with_context(|| format!("configurando a rede de `{}`", record.name))?;
    let capabilities = client.capabilities().clone();
    Ok(Entry {
        indexer: Arc::new(client),
        capabilities,
    })
}

/// O cliente sem a rede: o que a atualização de definições confere antes de
/// trocar a de um indexador em uso.
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

/// Intervalo mínimo entre duas requisições ao mesmo endpoint Torznab.
fn request_interval(record: &IndexerRecord) -> Result<Duration> {
    let seconds = match record.settings.get("request_interval_seconds") {
        Some(text) => text
            .parse::<f64>()
            .context("`request_interval_seconds` não é um número")?,
        None => 2.0,
    };
    anyhow::ensure!(
        seconds.is_finite() && (0.0..=3600.0).contains(&seconds),
        "`request_interval_seconds` precisa ficar entre 0 e 3600"
    );
    Ok(Duration::from_secs_f64(seconds))
}

/// O cliente de um endpoint Torznab, já com as capacidades lidas.
///
/// # Errors
///
/// Endpoint inválido, proxy que não monta ou `caps` que falha.
pub async fn torznab(record: &IndexerRecord, config: &Config) -> Result<Entry> {
    let client = TorznabClient::new(
        record.name.clone(),
        record.url.as_deref().unwrap_or_default(),
        record.settings.get("api_key").cloned(),
        config.http_timeout(),
        request_interval(record)?,
    )?
    .with_proxy(network(config, record)?.proxy.as_ref())?;
    let capabilities = client
        .capabilities()
        .await
        .context("lendo as capacidades")?;
    Ok(Entry {
        indexer: Arc::new(client),
        capabilities,
    })
}

// --- Atualização das definições ---------------------------------------------

/// Uma definição que não entrou, ou não carregou.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failed {
    pub definicao: String,
    pub motivo: String,
    /// Em uso: a versão anterior continua valendo.
    pub em_uso: bool,
}

/// Um indexador cadastrado cuja definição mudou.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub indexador: String,
    /// Remontado no catálogo servido, sem reiniciar. Falso em desativado (a
    /// definição nova vale quando ele for ativado) ou em falha.
    pub recarregado: bool,
    pub motivo: Option<String>,
}

/// O relatório da tarefa `definicoes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UpdateReport {
    pub baixadas: usize,
    pub novas: Vec<String>,
    pub atualizadas: Vec<String>,
    pub falharam: Vec<Failed>,
    pub em_uso_mudaram: Vec<Changed>,
}

impl UpdateReport {
    /// Ok, e a linha da tela.
    #[must_use]
    pub fn summary(&self) -> (bool, String) {
        let in_use_failed = self.falharam.iter().filter(|f| f.em_uso).count();
        let reload_failed = self
            .em_uso_mudaram
            .iter()
            .filter(|c| c.motivo.is_some())
            .count();
        let mut summary = format!(
            "{} baixadas: {} novas, {} atualizadas, {} não carregam",
            self.baixadas,
            self.novas.len(),
            self.atualizadas.len(),
            self.falharam.len()
        );
        if !self.em_uso_mudaram.is_empty() {
            let reloaded = self.em_uso_mudaram.iter().filter(|c| c.recarregado).count();
            summary = format!("{summary}; em uso: {reloaded} recarregadas");
        }
        if in_use_failed > 0 {
            summary = format!("{summary}; {in_use_failed} em uso mantidas na versão anterior");
        }
        (in_use_failed == 0 && reload_failed == 0, summary)
    }
}

/// Aplica as definições baixadas: grava no banco as novas e as que mudaram,
/// refaz o catálogo e remonta os indexadores em uso cuja definição mudou.
///
/// Definição em uso que a versão nova quebraria — não carrega, ou não monta
/// com os settings do cadastro — não é gravada: a anterior segue valendo,
/// inclusive depois de reiniciar. Definição local nunca é trocada.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn apply_update(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    registry: &Registry,
    downloaded: Vec<(String, String)>,
    at: &str,
) -> Result<UpdateReport> {
    let _guard = registry.write.lock().await;
    let mut report = UpdateReport {
        baixadas: downloaded.len(),
        ..UpdateReport::default()
    };
    let stored: BTreeMap<String, String> = store.definition_shas().await?.into_iter().collect();
    let records = registry.records();
    let before = registry.definitions().clone();
    // A definição efetiva de cada cadastro Cardigann, antes.
    let effective = |definitions: &Definitions, record: &IndexerRecord| {
        definitions
            .resolve(&record.name, record.definition.as_deref())
            .ok()
    };
    let in_use: BTreeMap<&str, &IndexerRecord> = records
        .iter()
        .filter(|record| record.kind == CARDIGANN)
        .filter(|record| effective(&before, record).is_none_or(|resolved| !resolved.local))
        .map(|record| (record.name.as_str(), record))
        .collect();

    let mut rows = Vec::new();
    for (file, yaml) in downloaded {
        let Some(header) = acervo_indexers::DefinitionHeader::peek(&yaml) else {
            report.falharam.push(Failed {
                definicao: file,
                motivo: "YAML sem id e nome".into(),
                em_uso: false,
            });
            continue;
        };
        let id = header.id;
        let sha = definitions::sha(&yaml);
        if stored.get(&id) == Some(&sha) {
            continue;
        }
        let loaded = CardigannDefinition::from_yaml_v11(&yaml).map_err(anyhow::Error::from);
        if let Some(record) = in_use.get(id.as_str()) {
            let checked = loaded
                .and_then(|definition| cardigann_client(record, definition, config.http_timeout()));
            if let Err(error) = checked {
                report.falharam.push(Failed {
                    definicao: id,
                    motivo: format!("{error:#}"),
                    em_uso: true,
                });
                continue;
            }
        } else if let Err(error) = loaded {
            // Fora de uso, entra assim mesmo: o catálogo a mostra recusada,
            // com o motivo.
            report.falharam.push(Failed {
                definicao: id.clone(),
                motivo: format!("{error:#}"),
                em_uso: false,
            });
        }
        if stored.contains_key(&id) {
            report.atualizadas.push(id.clone());
        } else {
            report.novas.push(id.clone());
        }
        rows.push(DefinitionRow {
            id,
            yaml,
            sha,
            updated_at: at.to_owned(),
        });
    }
    if rows.is_empty() {
        return Ok(report);
    }
    store
        .save_definitions(&rows)
        .await
        .context("gravando as definições")?;
    let all = store.definitions().await?;
    let mirror = registry
        .records
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let after = scan(config, &all, &mirror);
    drop(all);

    let mut reload = Vec::new();
    for record in in_use.values() {
        let old = effective(&before, record).map(|resolved| resolved.sha);
        let new = effective(&after, record).map(|resolved| resolved.sha);
        if old != new {
            reload.push(*record);
        }
    }
    registry.set_definitions(after);
    for record in reload {
        report
            .em_uso_mudaram
            .push(reload_one(record, config, catalog, registry).await);
    }
    Ok(report)
}

/// Remonta no catálogo servido o indexador cuja definição mudou. Desativado
/// fica como está: a definição nova vale quando ele for ativado.
async fn reload_one(
    record: &IndexerRecord,
    config: &Config,
    catalog: &Catalog,
    registry: &Registry,
) -> Changed {
    let mut changed = Changed {
        indexador: record.name.clone(),
        recarregado: false,
        motivo: None,
    };
    if record.enabled {
        match build(record, config, registry).await {
            Ok(entry) => {
                let swapped = if catalog.replace(&record.name, entry.clone()).is_ok() {
                    Ok(())
                } else {
                    // Fora do catálogo (não tinha subido): entra agora.
                    catalog.insert(entry).map_err(|error| error.to_string())
                };
                match swapped {
                    Ok(()) => changed.recarregado = true,
                    Err(error) => changed.motivo = Some(error),
                }
            }
            Err(error) => changed.motivo = Some(format!("{error:#}")),
        }
    }
    tracing::info!(
        indexer = record.name,
        recarregado = changed.recarregado,
        "definição em uso atualizada"
    );
    changed
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    fn with_tv(yaml: &str) -> String {
        yaml.replace(
            "    - {id: 1, cat: Movies, desc: Filmes}",
            "    - {id: 1, cat: Movies, desc: Filmes}\n    - {id: 2, cat: TV, desc: Séries}",
        )
    }

    #[tokio::test]
    async fn atualizacao_grava_recarrega_em_uso_e_poupa_local_e_quebrada() {
        use crate::definitions::tests::{Dir, yaml};
        let Some(db) = acervo_store::testing::TestDb::new("definicoes_update").await else {
            return;
        };
        let local = Dir::new("update-local", &[("l.yml", yaml("l", "L local"))]);
        let reserve = Dir::new(
            "update-reserva",
            &[
                ("a.yml", yaml("a", "A velha")),
                ("b.yml", yaml("b", "B velha")),
            ],
        );
        let mut config = Config::default();
        config.server.catalogs = vec![local.0.clone()];
        config.server.reserve_catalogs = vec![reserve.0.clone()];
        let pinned = |name: &str, path: PathBuf| IndexerRecord {
            definition: Some(path.display().to_string()),
            ..record(name, &[])
        };
        let records = vec![
            // Fixado na reserva: segue a precedência.
            pinned("a", reserve.0.join("a.yml")),
            // Fixado no diretório local: nunca é trocado.
            pinned("l", local.0.join("l.yml")),
        ];
        let registry = Registry::new(&config, &[], records.clone());
        let catalog = Catalog::new(Vec::new()).unwrap();
        for record in &records {
            catalog
                .insert(build(record, &config, &registry).await.unwrap())
                .unwrap();
        }
        assert_eq!(catalog.capabilities("a").unwrap().categories.len(), 1);

        let downloaded = vec![
            ("a".to_owned(), with_tv(&yaml("a", "A nova"))),
            ("b".to_owned(), yaml("b", "B nova")),
            ("c".to_owned(), "id: c\nname: C\nlinks: [nada]\n".to_owned()),
            ("l".to_owned(), with_tv(&yaml("l", "L do repositório"))),
        ];
        let report = apply_update(
            &config,
            &db.store,
            &catalog,
            &registry,
            downloaded.clone(),
            "t1",
        )
        .await
        .unwrap();
        assert_eq!(report.baixadas, 4);
        assert_eq!(report.novas, ["a", "b", "c", "l"]);
        assert!(report.atualizadas.is_empty());
        assert_eq!(report.falharam.len(), 1);
        assert_eq!(report.falharam[0].definicao, "c");
        assert!(!report.falharam[0].em_uso);
        // Só o "a" mudou de verdade para quem o usa; o local ficou.
        assert_eq!(
            report.em_uso_mudaram,
            [Changed {
                indexador: "a".into(),
                recarregado: true,
                motivo: None,
            }]
        );
        assert_eq!(catalog.capabilities("a").unwrap().categories.len(), 2);
        assert_eq!(catalog.capabilities("l").unwrap().categories.len(), 1);
        assert_eq!(
            registry.definitions().get("b").unwrap().header.name,
            "B nova"
        );
        assert_eq!(
            registry.definitions().get("l").unwrap().header.name,
            "L local"
        );
        assert_eq!(db.store.definitions().await.unwrap().len(), 4);

        // A mesma coisa de novo: nada muda.
        let again = apply_update(&config, &db.store, &catalog, &registry, downloaded, "t2")
            .await
            .unwrap();
        assert!(again.novas.is_empty() && again.atualizadas.is_empty());
        assert!(again.em_uso_mudaram.is_empty());

        // Versão nova do "a" que não carrega: mantém a anterior, no banco
        // e no catálogo servido.
        let broken = vec![("a".to_owned(), "id: a\nname: A\nlinks: [nada]\n".to_owned())];
        let kept = apply_update(&config, &db.store, &catalog, &registry, broken, "t3")
            .await
            .unwrap();
        assert!(kept.atualizadas.is_empty());
        assert_eq!(kept.falharam.len(), 1);
        assert!(kept.falharam[0].em_uso);
        assert!(!kept.summary().0);
        assert_eq!(catalog.capabilities("a").unwrap().categories.len(), 2);
        let stored = db.store.definitions().await.unwrap();
        let a = stored.iter().find(|row| row.id == "a").unwrap();
        assert!(a.yaml.contains("A nova"));
        db.drop().await;
    }

    #[test]
    fn resumo_conta_e_so_falha_com_definicao_em_uso() {
        let mut report = UpdateReport {
            baixadas: 3,
            novas: vec!["a".into()],
            atualizadas: vec!["b".into()],
            falharam: vec![Failed {
                definicao: "c".into(),
                motivo: "x".into(),
                em_uso: false,
            }],
            em_uso_mudaram: vec![Changed {
                indexador: "b".into(),
                recarregado: true,
                motivo: None,
            }],
        };
        let (ok, summary) = report.summary();
        assert!(ok, "{summary}");
        assert_eq!(
            summary,
            "3 baixadas: 1 novas, 1 atualizadas, 1 não carregam; em uso: 1 recarregadas"
        );
        report.falharam[0].em_uso = true;
        assert!(!report.summary().0);
    }
}

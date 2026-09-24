//! Os indexadores servidos e o despacho de uma consulta entre eles.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use acervo_indexers::{
    Capabilities, Category, Indexer, IndexerFailure, Release, SearchMode, SearchQuery,
    SearchSupport,
};
use futures::future::join_all;
use time::OffsetDateTime;

use crate::TorznabError;

/// Nome reservado para a busca em todos os indexadores de uma vez.
pub const ALL: &str = "all";

/// Nome reservado para as rotas da interface web.
pub const UI: &str = "ui";

/// Um indexador e as capacidades que ele anunciou.
///
/// As capacidades vêm de fora porque cada tipo as obtém de um jeito: a
/// definição Cardigann as declara, o cliente Torznab as busca na rede.
#[derive(Debug, Clone)]
pub struct Entry {
    pub indexer: Arc<dyn Indexer>,
    pub capabilities: Capabilities,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("nome de indexador inválido `{0}`: use letras minúsculas, números e hífens")]
    InvalidName(String),
    #[error("`{ALL}` e `{UI}` são nomes reservados")]
    ReservedName,
    #[error("indexador `{0}` registrado duas vezes")]
    Duplicate(String),
    #[error("indexador `{0}` não está no catálogo")]
    Unknown(String),
}

/// O que se sabe da saúde de um indexador desde que o processo subiu.
///
/// Fica em memória de propósito: o estado que importa é o de agora, e um
/// reinício que zera o histórico não esconde nada que a próxima busca não
/// mostre de novo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    pub last_success: Option<OffsetDateTime>,
    pub last_failure: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub last_results: Option<usize>,
}

impl Health {
    fn success(&mut self, results: usize) {
        self.last_success = Some(OffsetDateTime::now_utc());
        self.last_results = Some(results);
        self.consecutive_failures = 0;
    }

    fn failure(&mut self, error: String) {
        self.last_failure = Some(OffsetDateTime::now_utc());
        self.last_error = Some(error);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }
}

/// Um indexador visto de fora: o que uma interface lista.
#[derive(Debug, Clone)]
pub struct IndexerView {
    pub name: String,
    pub capabilities: Capabilities,
    pub proxies_downloads: bool,
    pub health: Health,
}

/// Catálogo compartilhado entre as rotas. Clones veem as mesmas entradas e a
/// mesma saúde; trocar a credencial de um indexador troca para todos.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: Arc<RwLock<BTreeMap<String, Entry>>>,
    health: Arc<Mutex<HashMap<String, Health>>>,
}

/// Resultado de uma consulta servida: página já cortada e falhas parciais.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub releases: Vec<Release>,
    pub failures: Vec<IndexerFailure>,
}

impl Catalog {
    /// # Errors
    ///
    /// Nome fora do formato de rota, nome reservado ou repetido.
    pub fn new(entries: impl IntoIterator<Item = Entry>) -> Result<Self, CatalogError> {
        let mut catalog = BTreeMap::new();
        for entry in entries {
            let name = entry.indexer.name().to_owned();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(CatalogError::InvalidName(name));
            }
            if name == ALL || name == UI {
                return Err(CatalogError::ReservedName);
            }
            if catalog.contains_key(&name) {
                return Err(CatalogError::Duplicate(name));
            }
            catalog.insert(name, entry);
        }
        Ok(Self {
            entries: Arc::new(RwLock::new(catalog)),
            health: Arc::default(),
        })
    }

    // Nenhum lock atravessa `await`: as entradas são copiadas (são `Arc`) e
    // o lock solto antes de qualquer requisição.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Entry>> {
        self.entries.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, name: &str, outcome: Result<usize, String>) {
        let mut health = self.health.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = health.entry(name.to_owned()).or_default();
        match outcome {
            Ok(results) => entry.success(results),
            Err(error) => entry.failure(error),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.read().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    fn targets(&self, target: &str) -> Result<Vec<Entry>, TorznabError> {
        let entries = self.read();
        if target == ALL {
            return Ok(entries.values().cloned().collect());
        }
        entries
            .get(target)
            .map(|entry| vec![entry.clone()])
            .ok_or(TorznabError::NoSuchIndexer)
    }

    /// Indexadores, com capacidades e saúde, em ordem de nome.
    #[must_use]
    pub fn views(&self) -> Vec<IndexerView> {
        let health = self
            .health
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.read()
            .iter()
            .map(|(name, entry)| IndexerView {
                name: name.clone(),
                capabilities: entry.capabilities.clone(),
                proxies_downloads: entry.indexer.proxies_downloads(),
                health: health.get(name).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// Troca um indexador já catalogado — por exemplo, com credencial nova.
    /// A saúde é zerada: ela descrevia o indexador antigo.
    ///
    /// # Errors
    ///
    /// Nome que não está no catálogo, ou que não bate com o do indexador novo.
    pub fn replace(&self, name: &str, entry: Entry) -> Result<(), CatalogError> {
        if entry.indexer.name() != name {
            return Err(CatalogError::InvalidName(entry.indexer.name().to_owned()));
        }
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        let slot = entries
            .get_mut(name)
            .ok_or_else(|| CatalogError::Unknown(name.to_owned()))?;
        *slot = entry;
        drop(entries);
        self.health
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        Ok(())
    }

    /// O indexador precisa intermediar os downloads dele?
    #[must_use]
    pub fn proxies_downloads(&self, name: &str) -> bool {
        self.read()
            .get(name)
            .is_some_and(|entry| entry.indexer.proxies_downloads())
    }

    /// Baixa um `.torrent` pela sessão do indexador.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido (inclusive `all`), ou falha do indexador — link
    /// fora da origem dele, sessão recusada, resposta que não é `.torrent`.
    pub async fn download(&self, name: &str, link: &url::Url) -> Result<Vec<u8>, TorznabError> {
        let entry = self
            .read()
            .get(name)
            .cloned()
            .ok_or(TorznabError::NoSuchIndexer)?;
        entry.indexer.download(link).await.map_err(|error| {
            tracing::warn!(indexer = name, %error, "download falhou");
            self.record(name, Err(error.to_string()));
            TorznabError::DownloadFailed
        })
    }

    /// Busca recente num indexador só, sem termo — o que o gerenciador faz no
    /// RSS. Serve de teste: exercita login, página e parser de uma vez.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido, ou a mensagem do erro do indexador.
    pub async fn test(&self, name: &str) -> Result<usize, String> {
        let entry = self
            .read()
            .get(name)
            .cloned()
            .ok_or_else(|| "indexador desconhecido".to_owned())?;
        let outcome = entry
            .indexer
            .search(&SearchQuery::general(""))
            .await
            .map(|releases| releases.len())
            .map_err(|error| error.to_string());
        self.record(name, outcome.clone());
        outcome
    }

    /// Capacidades de um indexador, ou a união de todos em `all`.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido.
    pub fn capabilities(&self, target: &str) -> Result<Capabilities, TorznabError> {
        let targets = self.targets(target)?;
        Ok(merge(targets.iter().map(|entry| &entry.capabilities)))
    }

    /// Consulta os indexadores elegíveis em paralelo e corta a página.
    ///
    /// Cada indexador recebe a consulta reduzida ao que anunciou saber
    /// responder; o que não sabe responder nem é consultado. Paginação e
    /// limite são aplicados aqui, sobre a primeira página de cada um: nem todo
    /// indexador pagina, e pedir a página 2 a quem não pagina devolveria a 1
    /// de novo — duplicata que o consumidor tomaria por release nova.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido, ou **todos** os consultados falharam. Falha
    /// total não pode virar lista vazia: para o consumidor, "nada encontrado"
    /// e "tracker fora do ar" pedem reações opostas.
    pub async fn search(&self, target: &str, query: &SearchQuery) -> Result<Page, TorznabError> {
        let eligible: Vec<_> = self
            .targets(target)?
            .into_iter()
            .filter_map(|entry| adapt(query, &entry.capabilities).map(|adapted| (entry, adapted)))
            .collect();
        let consulted = eligible.len();

        let pending = eligible.into_iter().map(|(entry, adapted)| async move {
            let name = entry.indexer.name().to_owned();
            (name, entry.indexer.search(&adapted).await)
        });

        let mut page = Page::default();
        for (indexer, result) in join_all(pending).await {
            match result {
                Ok(mut releases) => {
                    self.record(&indexer, Ok(releases.len()));
                    page.releases.append(&mut releases);
                }
                Err(error) => {
                    tracing::warn!(indexer, %error, "indexador falhou na busca");
                    self.record(&indexer, Err(error.to_string()));
                    page.failures.push(IndexerFailure {
                        indexer,
                        error: error.to_string(),
                    });
                }
            }
        }

        if consulted > 0 && page.failures.len() == consulted {
            return Err(TorznabError::AllFailed(consulted));
        }

        page.releases
            .sort_by_key(|release| Reverse(release.published));
        let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let limit = usize::from(query.limit.unwrap_or(crate::request::MAX_RESULTS));
        page.releases = page.releases.into_iter().skip(offset).take(limit).collect();
        Ok(page)
    }
}

/// Reduz a consulta ao que o indexador anunciou.
///
/// Devolve `None` quando o indexador não deve ser consultado: modo não
/// anunciado, nenhuma das categorias pedidas, ou uma busca que identificava
/// algo e, sem os parâmetros que ele não entende, viraria busca genérica —
/// e "últimos lançamentos" respondido como se fosse "episódio 3" é import
/// errado esperando para acontecer.
fn adapt(query: &SearchQuery, caps: &Capabilities) -> Option<SearchQuery> {
    let support = match query.mode {
        SearchMode::General => &caps.general,
        SearchMode::Tv { .. } => &caps.tv,
        SearchMode::Movie { .. } => &caps.movie,
    };
    if !support.available {
        return None;
    }
    let keeps = |param: &str| support.supported_params.contains(param);

    let mut adapted = query.clone();
    adapted.offset = 0;
    adapted.limit = None;
    if !keeps("q") {
        adapted.term = None;
    }
    match &mut adapted.mode {
        SearchMode::General => {}
        SearchMode::Tv {
            season,
            episode,
            tvdb_id,
            imdb_id,
        } => {
            if !keeps("season") {
                *season = None;
                *episode = None;
            }
            if !keeps("ep") {
                *episode = None;
            }
            if !keeps("tvdbid") {
                *tvdb_id = None;
            }
            if !keeps("imdbid") {
                *imdb_id = None;
            }
        }
        SearchMode::Movie {
            year,
            tmdb_id,
            imdb_id,
        } => {
            if !keeps("year") {
                *year = None;
            }
            if !keeps("tmdbid") {
                *tmdb_id = None;
            }
            if !keeps("imdbid") {
                *imdb_id = None;
            }
        }
    }
    if identifies(query) && !identifies(&adapted) {
        return None;
    }
    // Temporada e episódio estreitam a busca; perder só eles devolveria a
    // série inteira para quem pediu um episódio.
    if narrows(query) && !narrows(&adapted) {
        return None;
    }

    if !query.categories.is_empty() {
        adapted.categories.retain(|requested| {
            caps.categories
                .iter()
                .any(|category| covers(*requested, category.id))
        });
        if adapted.categories.is_empty() {
            return None;
        }
    }
    Some(adapted)
}

fn identifies(query: &SearchQuery) -> bool {
    query.term.is_some()
        || match &query.mode {
            SearchMode::General => false,
            SearchMode::Tv {
                tvdb_id, imdb_id, ..
            } => tvdb_id.is_some() || imdb_id.is_some(),
            SearchMode::Movie {
                tmdb_id, imdb_id, ..
            } => tmdb_id.is_some() || imdb_id.is_some(),
        }
}

fn narrows(query: &SearchQuery) -> bool {
    match &query.mode {
        SearchMode::Tv {
            season, episode, ..
        } => season.is_some() || episode.is_some(),
        SearchMode::Movie { year, .. } => year.is_some(),
        SearchMode::General => false,
    }
}

/// Categoria-mãe (múltipla de 1000) cobre as filhas.
fn covers(requested: u32, actual: u32) -> bool {
    requested == actual || (requested.is_multiple_of(1000) && requested / 1000 == actual / 1000)
}

fn merge<'a>(all: impl Iterator<Item = &'a Capabilities>) -> Capabilities {
    let mut merged = Capabilities::default();
    let mut categories: BTreeMap<u32, Category> = BTreeMap::new();
    for caps in all {
        union(&mut merged.general, &caps.general);
        union(&mut merged.tv, &caps.tv);
        union(&mut merged.movie, &caps.movie);
        for category in &caps.categories {
            categories
                .entry(category.id)
                .or_insert_with(|| category.clone());
        }
    }
    merged.categories = categories.into_values().collect();
    merged
}

fn union(target: &mut SearchSupport, source: &SearchSupport) {
    if source.available {
        target.available = true;
        target
            .supported_params
            .extend(source.supported_params.iter().cloned());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn support(params: &[&str]) -> SearchSupport {
        SearchSupport {
            available: true,
            supported_params: params.iter().map(|param| (*param).to_owned()).collect(),
        }
    }

    fn caps(tv: &[&str]) -> Capabilities {
        Capabilities {
            general: support(&["q"]),
            tv: support(tv),
            categories: vec![Category {
                id: 5040,
                name: "TV/HD".into(),
                parent: Some(5000),
            }],
            ..Capabilities::default()
        }
    }

    #[test]
    fn busca_por_id_sem_suporte_a_id_nao_vira_busca_generica() {
        let query = SearchQuery::tv("").with_tvdb_id(42).with_episode(1, "2");
        assert_eq!(adapt(&query, &caps(&["q", "season", "ep"])), None);
    }

    #[test]
    fn ids_nao_anunciados_sao_retirados_quando_sobra_o_termo() {
        let query = SearchQuery::tv("Série")
            .with_tvdb_id(42)
            .with_episode(1, "2");
        let adapted = adapt(&query, &caps(&["q", "season", "ep"])).unwrap();
        assert_eq!(
            adapted.mode,
            SearchMode::Tv {
                season: Some(1),
                episode: Some("2".into()),
                tvdb_id: None,
                imdb_id: None,
            }
        );
    }

    #[test]
    fn episodio_sem_suporte_a_temporada_nao_vira_serie_inteira() {
        let query = SearchQuery::tv("Série").with_episode(1, "2");
        assert_eq!(adapt(&query, &caps(&["q"])), None);
    }

    #[test]
    fn categoria_mae_cobre_a_filha_e_categoria_alheia_exclui() {
        let wants_tv = SearchQuery::tv("x").with_categories([5000]);
        assert_eq!(adapt(&wants_tv, &caps(&["q"])).unwrap().categories, [5000]);
        let wants_movie = SearchQuery::tv("x").with_categories([2000]);
        assert_eq!(adapt(&wants_movie, &caps(&["q"])), None);
    }

    #[test]
    fn paginacao_nunca_e_repassada() {
        let query = SearchQuery::general("x").with_offset(100).with_limit(50);
        let adapted = adapt(&query, &caps(&["q"])).unwrap();
        assert_eq!((adapted.offset, adapted.limit), (0, None));
    }

    #[test]
    fn uniao_de_capacidades() {
        let movie_only = Capabilities {
            movie: support(&["q", "tmdbid"]),
            ..Capabilities::default()
        };
        let merged = merge([caps(&["q", "season"]), movie_only].iter());
        assert!(merged.general.available && merged.tv.available && merged.movie.available);
        assert_eq!(
            merged.movie.supported_params,
            BTreeSet::from(["q".to_owned(), "tmdbid".to_owned()])
        );
        assert_eq!(merged.categories.len(), 1);
    }
}

//! Busca de séries: a dos episódios que faltam (dentro da tarefa `busca`,
//! depois dos filmes), o RSS (dentro da `rss`), a busca de uma série só e a
//! interativa da tela. Temporada toda exibida busca por temporada, que traz
//! o pacote e os avulsos; temporada em andamento, por episódio.

use std::collections::{BTreeMap, HashMap, HashSet};

use acervo_api::{ALL, Catalog};
use acervo_decision::{
    BlockedEpisode, EpisodeDecision, EpisodeEngine, EpisodeState, Indexer, Mode, Scope,
    SeriesTarget, Settings, pick,
};
use acervo_indexers::SearchQuery;
use acervo_store::{CatalogSeries, SeriesPick, SeriesSearch, Store};
use anyhow::{Result, bail};
use serde::Serialize;
use time::Date;

use crate::config::Config;
use crate::decide::{Turn, candidates, now_rfc3339};

/// Uma busca de série por vez: duas decidindo sobre a mesma fila pegariam o
/// mesmo episódio duas vezes.
static SEARCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// O que se pede ao indexador.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// A série toda (só a busca interativa).
    Series,
    Season(u16),
    Episode(u16, u16),
}

impl Query {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Series => "série".into(),
            Self::Season(season) => format!("S{season:02}"),
            Self::Episode(season, number) => format!("S{season:02}E{number:02}"),
        }
    }

    fn scope(self, series: i64) -> Scope {
        match self {
            Self::Series => Scope {
                series,
                season: None,
                episodes: Vec::new(),
            },
            Self::Season(season) => Scope {
                series,
                season: Some(season),
                episodes: Vec::new(),
            },
            Self::Episode(season, number) => Scope {
                series,
                season: Some(season),
                episodes: vec![number],
            },
        }
    }
}

/// As consultas da busca automática de uma série, sem IO. Entra cada
/// temporada com episódio em Quero já exibido (em `only`, se vier): toda
/// exibida, uma busca de temporada; em andamento, uma por episódio. Os
/// especiais vão sempre por episódio.
#[must_use]
pub fn plan(target: &SeriesTarget, only: Option<&HashSet<i64>>) -> Vec<Query> {
    let mut seasons: BTreeMap<u16, Vec<&acervo_decision::EpisodeTarget>> = BTreeMap::new();
    for episode in &target.episodes {
        seasons.entry(episode.season).or_default().push(episode);
    }
    let mut queries = Vec::new();
    for (season, episodes) in seasons {
        let wanted: Vec<u16> = episodes
            .iter()
            .filter(|e| e.state == EpisodeState::Wanted && e.aired)
            .filter(|e| only.is_none_or(|only| only.contains(&e.id)))
            .map(|e| e.number)
            .collect();
        if wanted.is_empty() {
            continue;
        }
        if season > 0 && episodes.iter().all(|e| e.aired) {
            queries.push(Query::Season(season));
        } else {
            queries.extend(wanted.into_iter().map(|n| Query::Episode(season, n)));
        }
    }
    queries
}

/// Indexadores, configurações e bloqueios: o que o motor lê além da série.
pub(crate) struct Parts {
    indexers: Vec<Indexer>,
    settings: Settings,
    blocklist: Vec<BlockedEpisode>,
}

impl Parts {
    /// # Errors
    ///
    /// Banco inalcançável ou nenhum indexador.
    pub async fn load(store: &Store, catalog: &Catalog) -> Result<Self> {
        let rules = crate::rules::stored(store).await?;
        let (definitions, blocked) =
            tokio::try_join!(store.quality_definitions(), store.blocklist())?;
        let served: Vec<String> = catalog.views().into_iter().map(|view| view.name).collect();
        if served.is_empty() {
            bail!("nenhum indexador ativo para buscar");
        }
        Ok(Self {
            indexers: crate::decide::indexers(&served, &rules),
            settings: rules.settings(definitions),
            // Bloqueio de filme não diz nada a série.
            blocklist: blocked
                .into_iter()
                .filter(|b| b.movie_id.is_none())
                .map(|b| BlockedEpisode {
                    series: b.series_id,
                    title: b.source_title,
                    indexer: b.indexer,
                })
                .collect(),
        })
    }

    pub fn engine<'a>(&'a self, library: &'a [SeriesTarget]) -> EpisodeEngine<'a> {
        EpisodeEngine {
            library,
            indexers: &self.indexers,
            settings: &self.settings,
            blocklist: &self.blocklist,
        }
    }
}

/// Busca nos indexadores, sem decidir. Termo: o título em inglês; com o
/// `tvdbid` quando o indexador anunciar.
///
/// # Errors
///
/// A busca falhou em todos os indexadores.
pub async fn fetch(
    catalog: &Catalog,
    entry: &CatalogSeries,
    query: Query,
) -> Result<Vec<acervo_indexers::Release>, String> {
    let mut search =
        SearchQuery::tv(super::folder_title(&entry.series)).with_categories([super::TV]);
    match query {
        Query::Series => {}
        Query::Season(season) => search = search.with_episode(season, ""),
        Query::Episode(season, number) => search = search.with_episode(season, number.to_string()),
    }
    if let Some(tvdb) = entry.series.tvdb_id {
        search = search.with_tvdb_id(u64::from(tvdb));
    }
    catalog
        .search(ALL, &search)
        .await
        .map(|page| page.releases)
        .map_err(|error| error.to_string())
}

/// Motivo de rejeição e quantos releases ele barrou, do mais comum ao menos.
fn summarize(decisions: &[EpisodeDecision], counts: &mut BTreeMap<String, usize>) {
    for decision in decisions.iter().filter(|d| !d.approved()) {
        let reason = decision
            .rejections
            .first()
            .map_or("?", acervo_decision::Rejection::reason);
        *counts.entry(reason.to_owned()).or_default() += 1;
    }
}

fn sorted(counts: BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = counts.into_iter().collect();
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    counts
}

/// O que a busca de uma série fez, para o relato da tarefa.
#[derive(Debug, Serialize)]
pub struct SeriesLine {
    pub serie: String,
    pub consultas: Vec<String>,
    pub releases: usize,
    pub escolhidos: Vec<String>,
    pub erro: Option<String>,
}

impl SeriesLine {
    fn from(entry: &CatalogSeries, run: &SeriesSearch) -> Self {
        Self {
            serie: entry.series.title.clone(),
            consultas: run.queries.clone(),
            releases: run.releases,
            escolhidos: run.picks.iter().map(|p| p.title.clone()).collect(),
            erro: run.error.clone(),
        }
    }
}

/// Quem busca: o que toda busca de série precisa, lido uma vez.
struct Searcher<'a> {
    config: &'a Config,
    store: &'a Store,
    catalog: &'a Catalog,
    parts: Parts,
    today: Date,
}

impl Searcher<'_> {
    /// Busca a série pelas consultas do plano, decide cada resultado, pega o
    /// que `pick` escolhe e devolve o registro. O que é pego passa a contar
    /// como fila para as consultas seguintes.
    async fn search(
        &self,
        entry: &CatalogSeries,
        queued: &mut HashSet<i64>,
        only: Option<&HashSet<i64>>,
    ) -> SeriesSearch {
        let mut run = SeriesSearch {
            series_id: entry.id,
            at: now_rfc3339(),
            queries: Vec::new(),
            releases: 0,
            picks: Vec::new(),
            rejections: Vec::new(),
            error: None,
        };
        let mut counts = BTreeMap::new();
        let mut errors = Vec::new();
        for query in plan(&super::target(entry, queued, self.today), only) {
            run.queries.push(query.label());
            let releases = match fetch(self.catalog, entry, query).await {
                Ok(releases) => releases,
                Err(error) => {
                    errors.push(format!("{}: {error}", query.label()));
                    continue;
                }
            };
            run.releases += releases.len();
            let library = [super::target(entry, queued, self.today)];
            let decisions = self.parts.engine(&library).search(
                &query.scope(entry.id),
                &candidates(&releases),
                Mode::Automatic,
            );
            summarize(&decisions, &mut counts);
            for (decision, wanted) in pick(&decisions) {
                let release = &releases[decision.release];
                let quality = decision
                    .quality
                    .map_or(acervo_parser::Quality::Unknown, |q| q.quality);
                let sent = super::grab::send(
                    self.config,
                    self.store,
                    self.catalog,
                    entry.id,
                    release,
                    quality,
                    &wanted,
                )
                .await;
                match sent {
                    Ok(_) => {
                        tracing::info!(
                            serie = entry.series.title,
                            release = release.title,
                            "pegou"
                        );
                        queued.extend(wanted.iter().copied());
                        run.picks.push(SeriesPick {
                            title: release.title.clone(),
                            indexer: release.indexer.clone(),
                            quality,
                            size: release.size,
                            episode_ids: wanted,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            serie = entry.series.title,
                            release = release.title,
                            "grab automático falhou: {error:#}"
                        );
                        errors.push(format!("{}: {error:#}", release.title));
                    }
                }
            }
        }
        run.rejections = sorted(counts);
        if !errors.is_empty() {
            run.error = Some(errors.join("; "));
        }
        run
    }
}

/// Busca as séries com episódio em Quero já exibido — até `limit` (todas,
/// sem `limit`), as que estão há mais tempo sem busca primeiro — e pega o
/// que serve. Grava a busca de cada uma.
///
/// # Errors
///
/// Banco inalcançável ou nenhum indexador.
pub async fn missing(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    limit: Option<usize>,
    turn: &Turn,
) -> Result<Vec<SeriesLine>> {
    let _guard = SEARCH_LOCK.lock().await;
    let list = store.series_list().await?;
    if list.is_empty() {
        return Ok(Vec::new());
    }
    let searcher = Searcher {
        config,
        store,
        catalog,
        parts: Parts::load(store, catalog).await?,
        today: super::today(),
    };
    let mut queued = super::queued(&store.series_grabs().await?);
    let latest: HashMap<i64, String> = store
        .latest_series_searches()
        .await?
        .into_iter()
        .map(|run| (run.series_id, run.at))
        .collect();
    let mut wanted: Vec<&CatalogSeries> = list
        .iter()
        .filter(|entry| !plan(&super::target(entry, &queued, searcher.today), None).is_empty())
        .collect();
    wanted.sort_by_key(|entry| latest.get(&entry.id).map_or("", String::as_str));
    if let Some(limit) = limit {
        wanted.truncate(limit);
    }
    turn.add_total(wanted.len());
    let mut lines = Vec::new();
    for entry in wanted {
        let run = searcher.search(entry, &mut queued, None).await;
        store.record_series_search(&run).await?;
        lines.push(SeriesLine::from(entry, &run));
        turn.advance();
    }
    Ok(lines)
}

/// A busca automática de uma série só: o botão da tela e a nova busca
/// depois de uma falha (`only`: só esses episódios).
///
/// # Errors
///
/// Série fora do catálogo, banco inalcançável ou nenhum indexador.
pub async fn series_now(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    series_id: i64,
    only: Option<&HashSet<i64>>,
) -> Result<SeriesLine> {
    let _guard = SEARCH_LOCK.lock().await;
    let Some(entry) = store.series(series_id).await? else {
        bail!("série fora do catálogo");
    };
    let searcher = Searcher {
        config,
        store,
        catalog,
        parts: Parts::load(store, catalog).await?,
        today: super::today(),
    };
    let mut queued = super::queued(&store.series_grabs().await?);
    let run = searcher.search(&entry, &mut queued, only).await;
    store.record_series_search(&run).await?;
    Ok(SeriesLine::from(&entry, &run))
}

/// Um grab da sincronização de RSS.
#[derive(Debug, Serialize)]
pub struct RssGrab {
    pub serie: String,
    pub release: String,
    pub erro: Option<String>,
}

/// RSS de séries: os releases recentes da categoria de TV, cada um casado
/// com a biblioteca inteira; `pick` escolhe e, com `apply`, vai ao cliente.
///
/// # Errors
///
/// Nenhum indexador ou busca que falhou em todos.
pub async fn rss(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    apply: bool,
) -> Result<Vec<RssGrab>> {
    let _guard = SEARCH_LOCK.lock().await;
    let list = store.series_list().await?;
    if list.is_empty() {
        return Ok(Vec::new());
    }
    let parts = Parts::load(store, catalog).await?;
    let queued = super::queued(&store.series_grabs().await?);
    let today = super::today();
    let library: Vec<SeriesTarget> = list
        .iter()
        .map(|entry| super::target(entry, &queued, today))
        .collect();
    let page = catalog
        .search(ALL, &SearchQuery::general("").with_categories([super::TV]))
        .await
        .map_err(|error| anyhow::anyhow!("RSS de séries: {error}"))?;
    let decisions = parts.engine(&library).rss(&candidates(&page.releases));
    let mut grabs = Vec::new();
    for (decision, wanted) in pick(&decisions) {
        let Some(entry) = list.iter().find(|e| Some(e.id) == decision.series) else {
            continue;
        };
        let release = &page.releases[decision.release];
        let mut line = RssGrab {
            serie: super::label(entry, &wanted),
            release: release.title.clone(),
            erro: None,
        };
        if apply {
            let quality = decision
                .quality
                .map_or(acervo_parser::Quality::Unknown, |q| q.quality);
            if let Err(error) =
                super::grab::send(config, store, catalog, entry.id, release, quality, &wanted).await
            {
                line.erro = Some(format!("{error:#}"));
            }
        }
        grabs.push(line);
    }
    Ok(grabs)
}

/// Uma busca interativa decidida: os releases como vieram, a decisão de
/// cada um e quais `pick` pegaria.
pub struct Interactive {
    pub releases: Vec<acervo_indexers::Release>,
    pub decisions: Vec<EpisodeDecision>,
    pub picked: HashSet<usize>,
}

/// A consulta da busca interativa: episódio, temporada ou a série.
#[must_use]
pub fn interactive_query(entry: &CatalogSeries, season: Option<u16>, episode_ids: &[i64]) -> Query {
    let chosen: Vec<(u16, u16)> = entry
        .episodes
        .iter()
        .filter(|e| episode_ids.contains(&e.id))
        .map(|e| (e.episode.season, e.episode.number))
        .collect();
    match (chosen.as_slice(), season) {
        ([(s, n)], _) => Query::Episode(*s, *n),
        ([(s, _), rest @ ..], _) if rest.iter().all(|(other, _)| other == s) => Query::Season(*s),
        ([], Some(season)) => Query::Season(season),
        _ => Query::Series,
    }
}

/// Busca interativa: decide cada release como a automática decidiria, sem
/// pegar nada.
///
/// # Errors
///
/// Série fora do catálogo, nenhum indexador ou busca que falhou em todos.
pub async fn interactive(
    store: &Store,
    catalog: &Catalog,
    series_id: i64,
    query: Query,
) -> Result<Interactive> {
    let Some(entry) = store.series(series_id).await? else {
        bail!("série fora do catálogo");
    };
    let parts = Parts::load(store, catalog).await?;
    let queued = super::queued(&store.series_grabs().await?);
    let releases = fetch(catalog, &entry, query)
        .await
        .map_err(|error| anyhow::anyhow!("busca falhou: {error}"))?;
    let library = [super::target(&entry, &queued, super::today())];
    let decisions = parts.engine(&library).search(
        &query.scope(series_id),
        &candidates(&releases),
        Mode::UserInvoked,
    );
    let picked = pick(&decisions).iter().map(|(d, _)| d.release).collect();
    Ok(Interactive {
        releases,
        decisions,
        picked,
    })
}

#[cfg(test)]
mod tests {
    use acervo_decision::EpisodeTarget;
    use acervo_parser::{Language, Quality, QualityModel, Revision};

    use super::*;

    fn episode(
        id: i64,
        season: u16,
        number: u16,
        aired: bool,
        state: EpisodeState,
    ) -> EpisodeTarget {
        EpisodeTarget {
            id,
            season,
            number,
            aired,
            state,
        }
    }

    fn series(episodes: Vec<EpisodeTarget>) -> SeriesTarget {
        SeriesTarget {
            id: 1,
            tvdb_id: None,
            titles: vec!["show".into()],
            year: None,
            runtime: 45,
            language: Language::English,
            episodes,
        }
    }

    const HAVE: EpisodeState = EpisodeState::Have(QualityModel {
        quality: Quality::WebDl1080p,
        revision: Revision {
            version: 1,
            real: 0,
            is_repack: false,
        },
    });

    #[test]
    fn temporada_toda_exibida_busca_por_temporada() {
        let target = series(vec![
            episode(1, 1, 1, true, HAVE),
            episode(2, 1, 2, true, EpisodeState::Wanted),
            episode(3, 1, 3, true, EpisodeState::Wanted),
            // Temporada em andamento: por episódio, só os já exibidos.
            episode(4, 2, 1, true, EpisodeState::Wanted),
            episode(5, 2, 2, true, EpisodeState::Skipped),
            episode(6, 2, 3, false, EpisodeState::Wanted),
            // Nada em Quero: nem entra.
            episode(7, 3, 1, true, HAVE),
            // Tudo baixando: nada a buscar.
            episode(8, 4, 1, true, EpisodeState::Queued),
        ]);
        assert_eq!(
            plan(&target, None),
            [Query::Season(1), Query::Episode(2, 1)]
        );
        // Só os episódios pedidos.
        assert_eq!(
            plan(&target, Some(&HashSet::from([4]))),
            [Query::Episode(2, 1)]
        );
        // Especial vai por episódio mesmo com a temporada 0 toda exibida.
        let specials = series(vec![episode(9, 0, 1, true, EpisodeState::Wanted)]);
        assert_eq!(plan(&specials, None), [Query::Episode(0, 1)]);
        assert_eq!(Query::Season(1).label(), "S01");
        assert_eq!(Query::Episode(2, 10).label(), "S02E10");
    }
}

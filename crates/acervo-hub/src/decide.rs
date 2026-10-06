//! A decisão: o [`Decider`] carrega a biblioteca, as regras de
//! [`crate::rules`] e os downloads do acervo em andamento (que contam como
//! fila), e escolhe entre os releases. Aqui também mora a busca dos filmes
//! que faltam, que pega o escolhido de cada um.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use acervo_api::{ALL, Catalog};
use acervo_decision::{
    BlockedRelease, Decision, Delay, Engine, ExistingFile, Indexer, Mode, Profile, Queued, Release,
    Settings, Target,
};
use acervo_indexers::SearchQuery;
use acervo_parser::{Language, QualityModel, Revision, clean_movie_title};
use acervo_store::{CatalogMovie, SearchPick, SearchRun, Store};
use anyhow::{Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::rules::DecisionRules;

/// Categoria Newznab de filmes.
const MOVIES: u32 = 2000;

/// Bit de freeleech, no formato de flags da referência.
const FREELEECH: u32 = 1;

/// O título da base de metadados (em inglês) não sai na API do gerenciador;
/// a pasta nasce dele.
fn metadata_title(movie: &acervo_store::Movie) -> String {
    let folder = Path::new(&movie.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&movie.title);
    let folder = folder.split(" {imdb-").next().unwrap_or(folder);
    match folder.rfind(" (") {
        Some(at) => folder[..at].to_owned(),
        None => folder.to_owned(),
    }
}

/// Todo filme decide pelo perfil automático: da melhor qualidade para a
/// pior, sem upgrade. O idioma é o original do filme, como era em todos os
/// perfis vindos do gerenciador — dual-audio com o original passa, só
/// dublado não.
fn automatic_profile() -> Profile {
    Profile::automatic(Language::Original)
}

/// Dias desde uma data RFC 3339; desconhecida conta como antiga.
fn age_days(date: Option<&str>, now: time::OffsetDateTime) -> u32 {
    date.and_then(|d| {
        time::OffsetDateTime::parse(d, &time::format_description::well_known::Rfc3339).ok()
    })
    .map_or(u32::MAX, |added| {
        u32::try_from((now - added).whole_days().max(0)).unwrap_or(u32::MAX)
    })
}

struct Remote {
    rules: DecisionRules,
}

fn target(entry: &CatalogMovie, remote: &Remote, now: time::OffsetDateTime) -> Target {
    let movie = &entry.movie;
    let profile = automatic_profile();
    let mut clean_titles: Vec<String> = movie.clean_title.iter().cloned().collect();
    for title in movie
        .original_title
        .iter()
        .chain(std::iter::once(&movie.title))
        .chain(&movie.alternate_titles)
    {
        clean_titles.push(clean_movie_title(title));
    }
    let original_language = movie
        .original_language
        .as_deref()
        .and_then(Language::from_name)
        .unwrap_or(Language::Unknown);
    Target {
        id: entry.id,
        title: metadata_title(movie),
        clean_titles,
        year: movie.year,
        tmdb_id: movie.tmdb_id,
        imdb_id: movie.imdb_id.clone(),
        original_language,
        runtime: movie.runtime,
        monitored: movie.monitored,
        // Calculada das datas, como a referência faz, com a carência dela.
        available: crate::library::is_available(movie, now.date(), remote.rules.carencia_dias),
        file: movie.file.as_ref().map(|file| ExistingFile {
            quality: file.quality,
            release_group: file.release_group.clone(),
            age_days: age_days(file.date_added.as_deref(), now),
        }),
        profile,
        // A fila é a dos grabs do acervo, somada em `Decider::load`.
        queued: Vec::new(),
    }
}

/// Os indexadores cadastrados, com a prioridade e os seeders mínimos das
/// regras.
pub(crate) fn indexers(served: &[String], rules: &DecisionRules) -> Vec<Indexer> {
    served
        .iter()
        .map(|name| {
            let rules = rules.indexer(name);
            Indexer {
                name: name.clone(),
                priority: rules.prioridade,
                minimum_seeders: rules.seeders_minimos,
                multi_languages: Vec::new(),
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct MissingLine {
    pub filme: String,
    pub releases: usize,
    pub escolhido: Option<String>,
    pub motivos: Vec<(String, usize)>,
    pub erro: Option<String>,
}

pub(crate) fn summarize(decisions: &[Decision], movie: i64) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for decision in decisions.iter().filter(|d| !d.approved()) {
        // O motivo que pesa: o do filme buscado, ou "outro filme".
        let reason = if decision.movie.is_some_and(|m| m != movie) {
            "WrongMovie"
        } else {
            decision
                .rejections
                .first()
                .map_or("?", acervo_decision::Rejection::reason)
        };
        *counts.entry(reason).or_default() += 1;
    }
    let mut counts: Vec<(String, usize)> =
        counts.into_iter().map(|(r, n)| (r.to_owned(), n)).collect();
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    counts
}

/// Tudo o que a decisão precisa, lido uma vez: a biblioteca como alvos, os
/// indexadores cadastrados e as regras.
pub(crate) struct Decider {
    pub library: Vec<Target>,
    /// Os filmes prioritários: buscados antes dos outros.
    pub priority: std::collections::HashSet<i64>,
    indexers: Vec<Indexer>,
    settings: Settings,
    blocklist: Vec<BlockedRelease>,
    delay: Delay,
}

/// Uma busca decidida: os releases como vieram do indexador e a decisão de
/// cada um, na ordem de preferência.
pub(crate) struct Outcome {
    pub releases: Vec<acervo_indexers::Release>,
    pub decisions: Vec<Decision>,
}

impl Outcome {
    /// O release que seria pego, com a decisão dele.
    pub fn pick(&self) -> Option<(&Decision, &acervo_indexers::Release)> {
        self.decisions
            .iter()
            .find(|d| d.approved())
            .map(|d| (d, &self.releases[d.release]))
    }
}

impl Decider {
    /// # Errors
    ///
    /// Catálogo vazio, banco inalcançável ou nenhum indexador.
    #[allow(clippy::too_many_lines)] // Leitura em sequência; dividir só espalharia.
    pub async fn load(store: &Store, catalog: &Catalog) -> Result<Self> {
        let rules = crate::rules::stored(store).await?;
        let (movies, grabs, blocked) =
            tokio::try_join!(store.movies(), store.grabs(), store.blocklist())?;
        if movies.is_empty() {
            bail!("catálogo vazio: adicione filmes antes");
        }
        let remote = Remote { rules };
        let now = time::OffsetDateTime::now_utc();
        let mut library: Vec<Target> = movies
            .iter()
            .map(|entry| target(entry, &remote, now))
            .collect();
        // O que o acervo mesmo mandou ao cliente conta como fila: sem isso,
        // pegaria o mesmo filme de novo enquanto o primeiro baixa.
        for grab in grabs
            .iter()
            .filter(|g| g.state == acervo_store::GrabState::Downloading)
        {
            if let Some(target) = library.iter_mut().find(|t| t.id == grab.movie_id) {
                target.queued.push(Queued {
                    quality: QualityModel {
                        quality: grab.quality,
                        revision: Revision::default(),
                    },
                });
            }
        }
        let served: Vec<String> = catalog.views().into_iter().map(|view| view.name).collect();
        if served.is_empty() {
            bail!("nenhum indexador ativo para buscar");
        }
        Ok(Self {
            indexers: indexers(&served, &remote.rules),
            settings: remote.rules.settings(),
            delay: remote.rules.delay(),
            // Bloqueio por falta de seeds expira; a linha fica na tela.
            blocklist: blocked
                .into_iter()
                .filter(|b| crate::grab::still_blocks(b, now))
                .map(|b| BlockedRelease {
                    movie: b.movie_id,
                    title: b.source_title,
                    indexer: b.indexer,
                })
                .collect(),
            priority: movies.iter().filter(|m| m.priority).map(|m| m.id).collect(),
            library,
        })
    }

    fn engine(&self) -> Engine<'_> {
        Engine {
            library: &self.library,
            indexers: &self.indexers,
            settings: &self.settings,
            blocklist: &self.blocklist,
            delay: self.delay,
        }
    }

    /// Decide releases já em mãos para o filme, como a busca interativa.
    pub fn decide_releases(
        &self,
        movie: i64,
        releases: Vec<acervo_indexers::Release>,
        mode: Mode,
    ) -> Outcome {
        let decisions = self.engine().search(movie, &candidates(&releases), mode);
        Outcome {
            releases,
            decisions,
        }
    }

    pub fn target(&self, movie_id: i64) -> Option<&Target> {
        self.library.iter().find(|t| t.id == movie_id)
    }

    /// Busca o filme em todos os indexadores e decide cada release.
    ///
    /// # Errors
    ///
    /// A busca falhou em todos os indexadores.
    pub async fn decide(&self, catalog: &Catalog, movie: &Target) -> Result<Outcome, String> {
        let releases = Self::fetch(catalog, movie).await?;
        Ok(self.decide_releases(movie.id, releases, Mode::Automatic))
    }

    /// Busca o filme em todos os indexadores, sem decidir.
    ///
    /// # Errors
    ///
    /// A busca falhou em todos os indexadores.
    pub async fn fetch(
        catalog: &Catalog,
        movie: &Target,
    ) -> Result<Vec<acervo_indexers::Release>, String> {
        let mut query = SearchQuery::movie(movie.title.clone()).with_categories([MOVIES]);
        if let Some(year) = movie.year {
            query = query.with_year(year);
        }
        if let Some(imdb) = &movie.imdb_id {
            query = query.with_imdb_id(imdb.clone());
        }
        if movie.tmdb_id != 0 {
            query = query.with_tmdb_id(u64::from(movie.tmdb_id));
        }
        catalog
            .search(ALL, &query)
            .await
            .map(|page| page.releases)
            .map_err(|error| error.to_string())
    }

    /// Releases recentes, sem filme buscado: cada um casado com a biblioteca
    /// inteira, como a sincronização de RSS da referência.
    pub fn rss(&self, releases: Vec<acervo_indexers::Release>) -> Outcome {
        let decisions = self.engine().rss(&candidates(&releases));
        Outcome {
            releases,
            decisions,
        }
    }
}

/// Os releases do indexador no formato que a decisão lê.
pub(crate) fn candidates(releases: &[acervo_indexers::Release]) -> Vec<Release> {
    releases
        .iter()
        .map(|r| Release {
            title: r.title.clone(),
            indexer: r.indexer.clone(),
            size: r.size,
            seeders: r.seeders,
            peers: r.seeders.map(|s| s + r.leechers.unwrap_or(0)),
            imdb_id: None,
            tmdb_id: None,
            tvdb_id: None,
            languages: Vec::new(),
            container: None,
            flags: if r.tags.iter().any(|t| t.eq_ignore_ascii_case("freeleech")) {
                FREELEECH
            } else {
                0
            },
            #[allow(clippy::cast_precision_loss)]
            age_hours: r
                .published
                .map(|at| (time::OffsetDateTime::now_utc() - at).whole_minutes() as f64 / 60.0),
        })
        .collect()
}

pub(crate) fn label(movie: &Target) -> String {
    match movie.year {
        Some(year) => format!("{} ({year})", movie.title),
        None => movie.title.clone(),
    }
}

/// Estado da busca dos que faltam, dividido entre a rodada automática e o
/// botão da tela: só uma roda por vez, e a tela lê o andamento daqui.
#[derive(Debug, Default)]
pub struct Progress {
    running: AtomicBool,
    done: AtomicUsize,
    total: AtomicUsize,
}

/// Instantâneo de [`Progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProgressView {
    pub rodando: bool,
    pub buscados: usize,
    pub total: usize,
}

/// Posse da vez de buscar; solta ao sair de escopo, inclusive em pânico.
#[derive(Debug)]
pub struct Turn(Arc<Progress>);

impl Turn {
    /// Soma ao total da busca: as séries entram depois dos filmes.
    pub(crate) fn add_total(&self, more: usize) {
        self.0.total.fetch_add(more, Ordering::SeqCst);
    }

    /// Mais um buscado.
    pub(crate) fn advance(&self) {
        self.0.done.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

impl Progress {
    /// Toma a vez de buscar; `None` se já há uma busca rodando.
    #[must_use]
    pub fn start(self: &Arc<Self>) -> Option<Turn> {
        self.running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        self.done.store(0, Ordering::SeqCst);
        self.total.store(0, Ordering::SeqCst);
        Some(Turn(Arc::clone(self)))
    }

    #[must_use]
    pub fn view(&self) -> ProgressView {
        ProgressView {
            rodando: self.running.load(Ordering::SeqCst),
            buscados: self.done.load(Ordering::SeqCst),
            total: self.total.load(Ordering::SeqCst),
        }
    }
}

/// Busca até `limit` filmes que faltam (todos, sem `limit`), começando pelos
/// que estão há mais tempo sem busca, e pega o escolhido de cada um.
/// `catalog` é o dos indexadores cadastrados: a busca divide sessão e consultas
/// guardadas com o RSS e a tela. Filme com download em
/// andamento não entra: já tem o que esperar.
///
/// `turn` prova que quem chama tomou a vez em [`Progress::start`]; o
/// andamento sai do mesmo `Progress`.
///
/// # Errors
///
/// Catálogo vazio ou nenhum indexador.
#[allow(clippy::too_many_lines)]
pub async fn search(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    limit: Option<usize>,
    turn: &Turn,
) -> Result<Vec<MissingLine>> {
    let progress = &turn.0;
    let (decider, latest) = tokio::try_join!(Decider::load(store, catalog), async {
        Ok(store.latest_searches().await?)
    },)?;
    let last_run: BTreeMap<i64, &str> =
        latest.iter().map(|r| (r.movie_id, r.at.as_str())).collect();
    // Download em andamento não entra: já tem o que esperar. O que só está
    // parado na fila, sem ter baixado nada, entra: se a decisão escolher outro
    // release, ele é melhor (a decisão recusa o que não supera a fila), e a
    // troca não custa nada.
    let mut downloading: HashMap<i64, Vec<acervo_store::Grab>> = HashMap::new();
    for grab in store.grabs().await? {
        if grab.state == acervo_store::GrabState::Downloading {
            downloading.entry(grab.movie_id).or_default().push(grab);
        }
    }
    let waiting = if downloading.is_empty() {
        std::collections::HashSet::new()
    } else {
        crate::grab::waiting(config).await
    };
    let mut wanted: Vec<&Target> = decider
        .library
        .iter()
        .filter(|t| t.monitored && t.available && t.file.is_none())
        .filter(|t| {
            downloading
                .get(&t.id)
                .is_none_or(|olds| olds.iter().all(|g| waiting.contains(&g.hash)))
        })
        .collect();
    // Prioritários primeiro; em cada grupo, nunca buscados primeiro e depois
    // o que está há mais tempo sem busca.
    wanted.sort_by_key(|t| {
        (
            !decider.priority.contains(&t.id),
            last_run.get(&t.id).copied().unwrap_or(""),
        )
    });
    if let Some(limit) = limit {
        wanted.truncate(limit);
    }
    progress.total.store(wanted.len(), Ordering::SeqCst);

    let mut lines = Vec::new();
    for movie in wanted {
        let at = now_rfc3339();
        let (run, line) = match decider.decide(catalog, movie).await {
            Err(error) => (
                SearchRun {
                    movie_id: movie.id,
                    at,
                    releases: 0,
                    pick: None,
                    rejections: Vec::new(),
                    error: Some(error.clone()),
                },
                MissingLine {
                    filme: label(movie),
                    releases: 0,
                    escolhido: None,
                    motivos: Vec::new(),
                    erro: Some(error),
                },
            ),
            Ok(outcome) => {
                if let Some((decision, release)) = outcome.pick() {
                    let quality = decision
                        .parsed
                        .as_ref()
                        .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality);
                    // Os que só esperam na fila dão lugar ao novo: a troca.
                    let olds = downloading.get(&movie.id).map_or(&[][..], Vec::as_slice);
                    let swapping: Vec<i64> = olds.iter().map(|g| g.id).collect();
                    if let Err(error) = crate::grab::send(
                        config, store, catalog, movie.id, release, quality, None, &swapping,
                    )
                    .await
                    {
                        tracing::warn!(filme = label(movie), "grab automático falhou: {error:#}");
                    } else {
                        tracing::info!(filme = label(movie), release = release.title, "pegou");
                        if let Err(error) =
                            crate::grab::swap_out(config, store, olds, &release.title).await
                        {
                            tracing::warn!(filme = label(movie), "troca na fila: {error:#}");
                        }
                    }
                }
                let pick = outcome.pick().map(|(decision, release)| SearchPick {
                    title: release.title.clone(),
                    indexer: release.indexer.clone(),
                    quality: decision
                        .parsed
                        .as_ref()
                        .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality),
                    size: release.size,
                });
                let reasons = summarize(&outcome.decisions, movie.id);
                (
                    SearchRun {
                        movie_id: movie.id,
                        at,
                        releases: outcome.releases.len(),
                        pick: pick.clone(),
                        rejections: reasons.clone(),
                        error: None,
                    },
                    MissingLine {
                        filme: label(movie),
                        releases: outcome.releases.len(),
                        escolhido: pick.map(|p| p.title),
                        motivos: reasons,
                        erro: None,
                    },
                )
            }
        };
        // Grava a cada filme: numa busca longa, a tela já mostra o que saiu e
        // um reinício no meio não perde o que foi feito.
        store.record_search(&run).await?;
        lines.push(line);
        progress.done.fetch_add(1, Ordering::SeqCst);
    }
    Ok(lines)
}

pub(crate) fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

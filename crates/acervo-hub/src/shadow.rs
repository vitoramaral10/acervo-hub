//! Decisão em sombra: o acervo-hub busca os filmes que faltam nos próprios
//! indexadores e decide o que pegaria, sem pegar nada. O gerenciador de
//! filmes segue decidindo e baixando; o relatório compara as duas escolhas.
//!
//! As regras de decisão vêm de [`crate::rules`]: do gerenciador enquanto ele
//! for o dono, do banco depois que o acervo assumir. A fila soma a do
//! gerenciador (se ele responde) com os downloads do próprio acervo.

use std::collections::BTreeMap;
use std::path::Path;

use acervo_api::{ALL, Catalog};
use acervo_arr::{ArrClient, ArrKind, RemoteQueueItem};
use acervo_core::InstanceName;
use acervo_decision::{
    BlockedRelease, CustomFormat, Decision, Delay, Engine, ExistingFile, FormatInput, Indexer,
    Mode, Profile, ProfileItem, Queued, Release, Settings, Target,
};
use acervo_indexers::SearchQuery;
use acervo_parser::{Language, QualityModel, Revision, clean_movie_title, parse_movie_title};
use acervo_store::{CatalogMovie, QualityProfile, ShadowPick, ShadowRun, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{Config, InstanceKind};
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

fn profile(stored: &QualityProfile) -> Profile {
    Profile {
        name: stored.name.clone(),
        items: stored
            .items
            .iter()
            .map(|item| ProfileItem {
                name: item.name.clone(),
                qualities: item.qualities.clone(),
                allowed: item.allowed,
            })
            .collect(),
        cutoff: stored.cutoff,
        upgrade_allowed: stored.upgrade_allowed,
        language: stored
            .language
            .as_deref()
            .and_then(Language::from_name)
            .unwrap_or(Language::Any),
        min_format_score: stored.min_format_score,
        cutoff_format_score: stored.cutoff_format_score,
        format_scores: stored
            .format_scores
            .iter()
            .map(|(id, score)| (*id, *score))
            .collect(),
    }
}

/// Nota de formatos de um nome de release ou de arquivo no perfil.
pub(crate) fn title_score(
    formats: &[CustomFormat],
    profile: &Profile,
    title: &str,
    original_language: Language,
    size: u64,
    languages: Option<&[Language]>,
) -> i32 {
    if formats.is_empty() {
        return 0;
    }
    let parsed = parse_movie_title(title);
    let found = parsed
        .as_ref()
        .map(|p| p.languages.clone())
        .unwrap_or_default();
    let quality = parsed.as_ref().map_or_else(
        || acervo_parser::parse_quality(title).quality,
        |p| p.quality.quality,
    );
    let input = FormatInput {
        title,
        release_group: parsed.as_ref().and_then(|p| p.release_group.as_deref()),
        edition: parsed.as_ref().and_then(|p| p.edition.as_deref()),
        languages: languages.unwrap_or(&found),
        original_language,
        quality,
        size,
        flags: 0,
    };
    profile.score(&acervo_decision::matching(formats, &input))
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
    /// A fila do gerenciador; vazia quando ele não responde.
    queue: Vec<RemoteQueueItem>,
    /// Espaço livre por pasta raiz, como o gerenciador vê a pasta.
    free: BTreeMap<String, u64>,
    rules: DecisionRules,
}

fn target(
    entry: &CatalogMovie,
    profiles: &BTreeMap<String, Profile>,
    remote: &Remote,
    formats: &[CustomFormat],
    now: time::OffsetDateTime,
) -> Option<Target> {
    let movie = &entry.movie;
    let profile = profiles.get(movie.quality_profile.as_deref()?)?.clone();
    let mut clean_titles: Vec<String> = movie.clean_title.iter().cloned().collect();
    for title in movie
        .original_title
        .iter()
        .chain(std::iter::once(&movie.title))
        .chain(&movie.alternate_titles)
    {
        clean_titles.push(clean_movie_title(title));
    }
    let source_id = entry.origin.as_ref().map(|(_, id)| *id);
    let original_language = movie
        .original_language
        .as_deref()
        .and_then(Language::from_name)
        .unwrap_or(Language::Unknown);
    Some(Target {
        id: entry.id,
        title: metadata_title(movie),
        clean_titles,
        year: movie.year,
        secondary_year: movie.secondary_year,
        tmdb_id: movie.tmdb_id,
        imdb_id: movie.imdb_id.clone(),
        original_language,
        runtime: movie.runtime,
        monitored: movie.monitored,
        // Calculada das datas, como a referência faz, com a carência dela.
        available: crate::library::is_available(movie, now.date(), remote.rules.carencia_dias),
        file: movie.file.as_ref().map(|file| {
            let languages: Vec<Language> = file
                .languages
                .iter()
                .filter_map(|l| Language::from_name(l))
                .collect();
            ExistingFile {
                quality: file.quality,
                release_group: file.release_group.clone(),
                age_days: age_days(file.date_added.as_deref(), now),
                format_score: title_score(
                    formats,
                    &profile,
                    file.scene_name.as_deref().unwrap_or(&file.relative_path),
                    original_language,
                    file.size,
                    (!languages.is_empty()).then_some(&languages[..]),
                ),
            }
        }),
        profile,
        queued: remote
            .queue
            .iter()
            .filter(|item| item.movie_id.is_some() && item.movie_id == source_id)
            .filter(|item| item.tracked_download_state.as_deref() != Some("failedPending"))
            .filter_map(|item| item.quality.as_ref())
            .map(|q| Queued {
                quality: QualityModel {
                    quality: acervo_parser::Quality::from_id(q.quality.id)
                        .unwrap_or(acervo_parser::Quality::Unknown),
                    revision: q
                        .revision
                        .as_ref()
                        .map_or_else(Revision::default, |r| Revision {
                            version: r.version,
                            real: r.real,
                            is_repack: r.is_repack,
                        }),
                },
                format_score: 0,
            })
            .collect(),
        free_space: std::path::Path::new(&movie.path)
            .parent()
            .and_then(|root| remote.free.get(&root.display().to_string()))
            .copied(),
    })
}

/// Os indexadores daqui, com prioridade e seeders mínimos do cadastro deles
/// no gerenciador (`<nome> (acervo-hub)`).
fn indexers(served: &[String], remote: &Remote) -> Vec<Indexer> {
    served
        .iter()
        .map(|name| {
            let rules = remote.rules.indexer(name);
            Indexer {
                name: name.clone(),
                priority: rules.prioridade,
                minimum_seeders: rules.seeders_minimos,
                multi_languages: Vec::new(),
            }
        })
        .collect()
}

/// Espaço livre de cada pasta raiz dos filmes, lido do disco.
async fn free_space(config: &Config, movies: &[CatalogMovie]) -> BTreeMap<String, u64> {
    let mut roots: std::collections::BTreeSet<String> =
        config.movies.root_folders.iter().cloned().collect();
    for entry in movies {
        if let Some(root) = std::path::Path::new(&entry.movie.path).parent() {
            roots.insert(root.display().to_string());
        }
    }
    let map = config.path_map();
    tokio::task::spawn_blocking(move || {
        roots
            .into_iter()
            .filter_map(|root| {
                let host = map.to_host(std::path::Path::new(&root)).ok()?;
                Some((root, acervo_fs::free_space(&host).ok()?))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

pub(crate) fn movie_client(config: &Config) -> Result<ArrClient> {
    let spec = config
        .instances
        .iter()
        .find(|spec| matches!(spec.kind, InstanceKind::Movie))
        .context("nenhum gerenciador de filmes em [[instances]]")?;
    Ok(ArrClient::new(
        InstanceName::new(spec.name.clone()),
        &spec.url,
        &spec.api_key,
        ArrKind::Movie,
        config.http_timeout(),
    )?)
}

#[derive(Debug, Serialize)]
pub struct ShadowLine {
    pub filme: String,
    pub releases: usize,
    pub pegaria: Option<String>,
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
/// indexadores servidos e a configuração do gerenciador.
pub(crate) struct Decider {
    pub library: Vec<Target>,
    indexers: Vec<Indexer>,
    settings: Settings,
    formats: Vec<CustomFormat>,
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
    /// Catálogo vazio, gerenciador de filmes inalcançável ou nenhum indexador.
    #[allow(clippy::too_many_lines)] // Leitura em sequência; dividir só espalharia.
    pub async fn load(config: &Config, store: &Store, catalog: &Catalog) -> Result<Self> {
        let owner = crate::rules::owner(config, store).await?;
        let has_manager = config
            .instances
            .iter()
            .any(|spec| matches!(spec.kind, InstanceKind::Movie));
        let live = async {
            let client = movie_client(config)?;
            if owner != "radarr" {
                return anyhow::Ok((client.movie_queue().await?, None));
            }
            let (queue, rules) = tokio::try_join!(
                async { Ok(client.movie_queue().await?) },
                crate::rules::from_manager(&client),
            )?;
            anyhow::Ok((queue, Some(rules)))
        }
        .await;
        let (queue, rules) = match live {
            Ok((queue, Some(rules))) => {
                crate::rules::save(store, &rules).await?;
                (queue, rules)
            }
            Ok((queue, None)) => (queue, crate::rules::stored(store).await?),
            Err(error) => {
                if has_manager {
                    tracing::warn!(
                        "gerenciador de filmes inalcançável, decidindo com as regras guardadas: {error:#}"
                    );
                }
                (Vec::new(), crate::rules::stored(store).await?)
            }
        };
        let (movies, stored_profiles, definitions, grabs, stored_formats, blocked) = tokio::try_join!(
            store.movies(),
            store.profiles(),
            store.quality_definitions(),
            store.grabs(),
            store.custom_formats(),
            store.blocklist(),
        )?;
        if movies.is_empty() {
            bail!("catálogo vazio: adicione filmes ou importe do gerenciador antes");
        }
        let formats = crate::rules::compiled_formats(&stored_formats);
        let remote = Remote {
            queue,
            free: free_space(config, &movies).await,
            rules,
        };
        let now = time::OffsetDateTime::now_utc();
        let profiles: BTreeMap<String, Profile> = stored_profiles
            .iter()
            .map(|p| (p.name.clone(), profile(p)))
            .collect();
        let mut library: Vec<Target> = movies
            .iter()
            .filter_map(|entry| target(entry, &profiles, &remote, &formats, now))
            .collect();
        // O que o acervo mesmo mandou ao cliente conta como fila: sem isso,
        // pegaria o mesmo filme de novo enquanto o primeiro baixa.
        for grab in grabs
            .iter()
            .filter(|g| g.state == acervo_store::GrabState::Downloading)
        {
            if let Some(target) = library.iter_mut().find(|t| t.id == grab.movie_id) {
                let format_score = title_score(
                    &formats,
                    &target.profile,
                    &grab.title,
                    target.original_language,
                    grab.size,
                    None,
                );
                target.queued.push(Queued {
                    quality: QualityModel {
                        quality: grab.quality,
                        revision: Revision::default(),
                    },
                    format_score,
                });
            }
        }
        let served: Vec<String> = catalog.views().into_iter().map(|view| view.name).collect();
        if served.is_empty() {
            bail!("nenhum indexador ativo para buscar");
        }
        Ok(Self {
            indexers: indexers(&served, &remote),
            settings: remote.rules.settings(definitions),
            delay: remote.rules.delay(),
            formats,
            blocklist: blocked
                .into_iter()
                .map(|b| BlockedRelease {
                    movie: b.movie_id,
                    title: b.source_title,
                    indexer: b.indexer,
                })
                .collect(),
            library,
        })
    }

    fn engine(&self) -> Engine<'_> {
        Engine {
            library: &self.library,
            indexers: &self.indexers,
            settings: &self.settings,
            formats: &self.formats,
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

    /// Nome de cada formato, pelo id.
    pub fn format_name(&self, id: i64) -> Option<&str> {
        self.formats
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.name.as_str())
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
fn candidates(releases: &[acervo_indexers::Release]) -> Vec<Release> {
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

/// Busca em sombra até `limit` filmes que faltam, começando pelos que estão
/// há mais tempo sem sombra. `catalog` é o dos indexadores servidos: dentro
/// do serviço, a sombra divide sessão e consultas guardadas com os
/// gerenciadores.
///
/// # Errors
///
/// Catálogo vazio, gerenciador de filmes inalcançável ou nenhum indexador.
pub async fn run(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    limit: usize,
    print: bool,
) -> Result<Vec<ShadowLine>> {
    search(config, store, catalog, limit, print, false).await
}

/// Como [`run`]; com `grab`, o escolhido de cada filme vai ao cliente — é a
/// busca automática dos filmes que faltam.
///
/// # Errors
///
/// Catálogo vazio ou nenhum indexador.
pub async fn search(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    limit: usize,
    print: bool,
    grab: bool,
) -> Result<Vec<ShadowLine>> {
    let (decider, latest) = tokio::try_join!(Decider::load(config, store, catalog), async {
        Ok(store.latest_shadow_runs().await?)
    },)?;
    let last_run: BTreeMap<i64, &str> =
        latest.iter().map(|r| (r.movie_id, r.at.as_str())).collect();
    let mut wanted: Vec<&Target> = decider
        .library
        .iter()
        .filter(|t| t.monitored && t.available && t.file.is_none())
        .collect();
    // Nunca buscados primeiro; depois, o que está há mais tempo sem busca.
    wanted.sort_by_key(|t| last_run.get(&t.id).copied().unwrap_or(""));
    wanted.truncate(limit);

    let mut lines = Vec::new();
    let mut runs = Vec::new();
    for movie in wanted {
        let at = now_rfc3339();
        let (run, line) = match decider.decide(catalog, movie).await {
            Err(error) => (
                ShadowRun {
                    movie_id: movie.id,
                    at,
                    releases: 0,
                    pick: None,
                    rejections: Vec::new(),
                    error: Some(error.clone()),
                },
                ShadowLine {
                    filme: label(movie),
                    releases: 0,
                    pegaria: None,
                    motivos: Vec::new(),
                    erro: Some(error),
                },
            ),
            Ok(outcome) => {
                if grab && let Some((decision, release)) = outcome.pick() {
                    let quality = decision
                        .parsed
                        .as_ref()
                        .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality);
                    if let Err(error) =
                        crate::grab::send(config, store, catalog, movie.id, release, quality, None)
                            .await
                    {
                        tracing::warn!(filme = label(movie), "grab automático falhou: {error:#}");
                    } else {
                        tracing::info!(filme = label(movie), release = release.title, "pegou");
                    }
                }
                let pick = outcome.pick().map(|(decision, release)| ShadowPick {
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
                    ShadowRun {
                        movie_id: movie.id,
                        at,
                        releases: outcome.releases.len(),
                        pick: pick.clone(),
                        rejections: reasons.clone(),
                        error: None,
                    },
                    ShadowLine {
                        filme: label(movie),
                        releases: outcome.releases.len(),
                        pegaria: pick.map(|p| p.title),
                        motivos: reasons,
                        erro: None,
                    },
                )
            }
        };
        if print {
            print_line(&line);
        }
        runs.push(run);
        lines.push(line);
    }

    for run in &runs {
        store.record_shadow(run).await?;
    }
    Ok(lines)
}

pub(crate) fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn print_line(line: &ShadowLine) {
    match (&line.erro, &line.pegaria) {
        (Some(error), _) => println!("{}: busca falhou — {error}", line.filme),
        (None, Some(pick)) => println!("{}: pegaria {pick}", line.filme),
        (None, None) => println!(
            "{}: nada entre {} releases — {}",
            line.filme,
            line.releases,
            line.motivos
                .iter()
                .map(|(reason, n)| format!("{reason} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Como a escolha em sombra se compara com o que o gerenciador pegou depois.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Os dois pegaram o mesmo release.
    Same,
    /// Os dois pegaram, releases diferentes.
    Different,
    /// A sombra pegaria; o gerenciador ainda não pegou nada.
    Pending,
    /// O gerenciador pegou algo que a sombra não pegaria.
    OnlyReference,
    /// Nenhum dos dois.
    Neither,
}

/// `movies shadow --report`: a última sombra de cada filme contra o primeiro
/// grab do gerenciador depois dela. Devolve quantas divergem.
///
/// # Errors
///
/// Catálogo vazio ou gerenciador inalcançável.
pub async fn report(config: &Config, store: &Store) -> Result<usize> {
    let client = movie_client(config)?;
    let (movies, latest) = tokio::try_join!(store.movies(), store.latest_shadow_runs())?;

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for run in &latest {
        let Some(entry) = movies.iter().find(|m| m.id == run.movie_id) else {
            continue;
        };
        let Some((_, source_id)) = &entry.origin else {
            continue;
        };
        let grabs = client.movie_grabs(*source_id).await?;
        // O primeiro grab depois da sombra (RFC 3339 em UTC compara como texto).
        let after = grabs
            .iter()
            .rev()
            .find(|g| g.date.as_str() >= run.at.as_str());
        let verdict = match (&run.pick, after) {
            (Some(pick), Some(grab)) if pick.title == grab.source_title => Verdict::Same,
            (Some(_), Some(_)) => Verdict::Different,
            (Some(_), None) => Verdict::Pending,
            (None, Some(_)) => Verdict::OnlyReference,
            (None, None) => Verdict::Neither,
        };
        let key = match verdict {
            Verdict::Same => "igual",
            Verdict::Different => "diferente",
            Verdict::Pending => "só a sombra (o gerenciador ainda não pegou)",
            Verdict::OnlyReference => "só o gerenciador",
            Verdict::Neither => "nenhum dos dois",
        };
        *counts.entry(key).or_default() += 1;
        if matches!(verdict, Verdict::Different | Verdict::OnlyReference) {
            println!(
                "{} — sombra: {} | gerenciador: {}",
                entry.movie.title,
                run.pick.as_ref().map_or("nada", |p| p.title.as_str()),
                after.map_or("nada", |g| g.source_title.as_str())
            );
        }
    }
    for (key, n) in &counts {
        println!("{n:>4}  {key}");
    }
    Ok(counts.get("diferente").copied().unwrap_or(0)
        + counts.get("só o gerenciador").copied().unwrap_or(0))
}

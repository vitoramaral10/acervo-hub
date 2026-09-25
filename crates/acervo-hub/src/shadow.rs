//! Decisão em sombra: o acervo-hub busca os filmes que faltam nos próprios
//! indexadores e decide o que pegaria, sem pegar nada. O gerenciador de
//! filmes segue decidindo e baixando; o relatório compara as duas escolhas.
//!
//! A configuração de decisão (tetos, seeders mínimos, fila, espaço livre) vem
//! do gerenciador: nesta fase ele é a autoridade, e a sombra precisa decidir
//! com as mesmas regras para a comparação valer.

use std::collections::BTreeMap;
use std::path::Path;

use acervo_api::{ALL, Catalog};
use acervo_arr::{ArrClient, ArrKind, RemoteIndexer, RemoteQueueItem, RemoteRootFolder};
use acervo_core::InstanceName;
use acervo_decision::{
    Decision, Engine, ExistingFile, Indexer, Mode, Profile, ProfileItem, Propers,
    QualityDefinition, Release, Settings, Target,
};
use acervo_indexers::SearchQuery;
use acervo_parser::{Language, QualityModel, Revision, clean_movie_title};
use acervo_store::{CatalogMovie, QualityProfile, ShadowPick, ShadowRun, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, InstanceKind};

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
    }
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
    queue: Vec<RemoteQueueItem>,
    roots: Vec<RemoteRootFolder>,
    indexer_config: Value,
    media_config: Value,
    indexers: Vec<RemoteIndexer>,
}

fn target(
    entry: &CatalogMovie,
    profiles: &BTreeMap<String, Profile>,
    remote: &Remote,
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
    Some(Target {
        id: entry.id,
        title: metadata_title(movie),
        clean_titles,
        year: movie.year,
        secondary_year: movie.secondary_year,
        tmdb_id: movie.tmdb_id,
        imdb_id: movie.imdb_id.clone(),
        original_language: movie
            .original_language
            .as_deref()
            .and_then(Language::from_name)
            .unwrap_or(Language::Unknown),
        runtime: movie.runtime,
        monitored: movie.monitored,
        available: movie.available,
        profile,
        file: movie.file.as_ref().map(|file| ExistingFile {
            quality: file.quality,
            release_group: file.release_group.clone(),
            age_days: age_days(file.date_added.as_deref(), now),
        }),
        queued: remote
            .queue
            .iter()
            .filter(|item| item.movie_id.is_some() && item.movie_id == source_id)
            .filter(|item| item.tracked_download_state.as_deref() != Some("failedPending"))
            .filter_map(|item| item.quality.as_ref())
            .map(|q| QualityModel {
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
            })
            .collect(),
        free_space: remote
            .roots
            .iter()
            .filter(|root| movie.path.starts_with(&root.path))
            .max_by_key(|root| root.path.len())
            .and_then(|root| root.free_space),
    })
}

fn settings(remote: &Remote, definitions: Vec<acervo_store::QualityDefinition>) -> Settings {
    let indexer = &remote.indexer_config;
    let media = &remote.media_config;
    Settings {
        definitions: definitions
            .into_iter()
            .map(|d| QualityDefinition {
                quality: d.quality,
                min_size: d.min_size,
                max_size: d.max_size,
                preferred_size: d.preferred_size,
            })
            .collect(),
        maximum_size_mb: indexer["maximumSize"].as_u64().unwrap_or(0),
        allow_hardcoded_subs: indexer["allowHardcodedSubs"].as_bool().unwrap_or(false),
        whitelisted_hardcoded_subs: indexer["whitelistedHardcodedSubs"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        propers: match media["downloadPropersAndRepacks"].as_str() {
            Some("doNotUpgrade") => Propers::DoNotUpgrade,
            Some("doNotPrefer") => Propers::DoNotPrefer,
            _ => Propers::PreferAndUpgrade,
        },
        prefer_indexer_flags: indexer["preferIndexerFlags"].as_bool().unwrap_or(false),
        minimum_free_space_mb: media["minimumFreeSpaceWhenImporting"]
            .as_u64()
            .unwrap_or(100),
        skip_free_space_check: media["skipFreeSpaceCheckWhenImporting"]
            .as_bool()
            .unwrap_or(false),
    }
}

/// Os indexadores daqui, com prioridade e seeders mínimos do cadastro deles
/// no gerenciador (`<nome> (acervo-hub)`).
fn indexers(served: &[String], remote: &Remote) -> Vec<Indexer> {
    served
        .iter()
        .map(|name| {
            let registered = remote
                .indexers
                .iter()
                .find(|i| i.name == format!("{name}{}", crate::sync::SUFFIX));
            Indexer {
                name: name.clone(),
                priority: registered
                    .and_then(RemoteIndexer::priority)
                    .and_then(|p| i32::try_from(p).ok())
                    .unwrap_or(25),
                minimum_seeders: registered
                    .and_then(RemoteIndexer::minimum_seeders)
                    .and_then(|s| u32::try_from(s).ok())
                    .unwrap_or(1),
                multi_languages: Vec::new(),
            }
        })
        .collect()
}

fn movie_client(config: &Config) -> Result<ArrClient> {
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

fn summarize(decisions: &[Decision], movie: i64) -> Vec<(String, usize)> {
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

/// Busca em sombra até `limit` filmes que faltam, começando pelos que estão
/// há mais tempo sem sombra. `catalog` é o dos indexadores servidos: dentro
/// do serviço, a sombra divide sessão e consultas guardadas com os
/// gerenciadores.
///
/// # Errors
///
/// Catálogo vazio, gerenciador de filmes inalcançável ou nenhum indexador.
#[allow(clippy::too_many_lines)]
pub async fn run(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    limit: usize,
    print: bool,
) -> Result<Vec<ShadowLine>> {
    let client = movie_client(config)?;
    let (queue, roots, indexer_config, media_config, remote_indexers) = tokio::try_join!(
        client.movie_queue(),
        client.root_folders(),
        client.indexer_config(),
        client.media_management_config(),
        client.indexers(),
    )?;
    let remote = Remote {
        queue,
        roots,
        indexer_config,
        media_config,
        indexers: remote_indexers,
    };

    let (catalog_movies, stored_profiles, definitions, latest) = tokio::try_join!(
        store.movies(),
        store.profiles(),
        store.quality_definitions(),
        store.latest_shadow_runs(),
    )?;
    if catalog_movies.is_empty() {
        bail!("catálogo vazio: rode `movies import --apply` antes");
    }

    let now = time::OffsetDateTime::now_utc();
    let profiles: BTreeMap<String, Profile> = stored_profiles
        .iter()
        .map(|p| (p.name.clone(), profile(p)))
        .collect();
    let library: Vec<Target> = catalog_movies
        .iter()
        .filter_map(|entry| target(entry, &profiles, &remote, now))
        .collect();
    let settings = settings(&remote, definitions);

    let served: Vec<String> = catalog.views().into_iter().map(|view| view.name).collect();
    if served.is_empty() {
        bail!("nenhum indexador ativo para buscar");
    }
    let indexers = indexers(&served, &remote);
    let engine = Engine {
        library: &library,
        indexers: &indexers,
        settings: &settings,
    };

    let last_run: BTreeMap<i64, &str> =
        latest.iter().map(|r| (r.movie_id, r.at.as_str())).collect();
    let mut wanted: Vec<&Target> = library
        .iter()
        .filter(|t| t.monitored && t.available && t.file.is_none())
        .collect();
    // Nunca buscados primeiro; depois, o que está há mais tempo sem busca.
    wanted.sort_by_key(|t| last_run.get(&t.id).copied().unwrap_or(""));
    wanted.truncate(limit);

    let mut lines = Vec::new();
    let mut runs = Vec::new();
    for movie in wanted {
        let label = match movie.year {
            Some(year) => format!("{} ({year})", movie.title),
            None => movie.title.clone(),
        };
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
        let at = now_rfc3339();
        let (run, line) = match catalog.search(ALL, &query).await {
            Err(error) => {
                let error = error.to_string();
                (
                    ShadowRun {
                        movie_id: movie.id,
                        at,
                        releases: 0,
                        pick: None,
                        rejections: Vec::new(),
                        error: Some(error.clone()),
                    },
                    ShadowLine {
                        filme: label,
                        releases: 0,
                        pegaria: None,
                        motivos: Vec::new(),
                        erro: Some(error),
                    },
                )
            }
            Ok(page) => {
                let releases: Vec<Release> = page
                    .releases
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
                    })
                    .collect();
                let decisions = engine.search(movie.id, &releases, Mode::Automatic);
                let pick = decisions.iter().find(|d| d.approved()).map(|d| {
                    let release = &releases[d.release];
                    ShadowPick {
                        title: release.title.clone(),
                        indexer: release.indexer.clone(),
                        quality: d
                            .parsed
                            .as_ref()
                            .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality),
                        size: release.size,
                    }
                });
                let reasons = summarize(&decisions, movie.id);
                (
                    ShadowRun {
                        movie_id: movie.id,
                        at,
                        releases: releases.len(),
                        pick: pick.clone(),
                        rejections: reasons.clone(),
                        error: None,
                    },
                    ShadowLine {
                        filme: label,
                        releases: releases.len(),
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

fn now_rfc3339() -> String {
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

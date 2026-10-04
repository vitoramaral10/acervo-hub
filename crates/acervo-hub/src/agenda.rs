//! "Faltando" e "Calendário": as listas que olham o catálogo inteiro pela
//! data, sob `/ui/api/biblioteca/`, com a mesma entrada das outras rotas
//! (sessão ou chave).
//!
//! Faltando: os episódios em Quero já exibidos e os filmes monitorados e
//! disponíveis sem arquivo, do mais recente ao mais antigo. Calendário: os
//! episódios que vão ou foram ao ar no intervalo e os lançamentos (cinema,
//! digital, físico) dos filmes monitorados.

// Handler devolve a resposta de erro pronta; é o formato do axum.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::HashSet;
use std::sync::Arc;

use acervo_decision::EpisodeState;
use acervo_store::{CatalogMovie, CatalogSeries, Grab, GrabState, SeriesGrab};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};
use time::{Date, Duration};

use crate::series::date;
use crate::web::{Shared, Web, WebResult, bad, enter, fail, ok};

/// O maior intervalo que o calendário aceita, em dias.
const MAX_DAYS: i64 = 366;

pub fn router(web: Arc<Web>) -> Router {
    routes().with_state(web)
}

/// As rotas, ainda sem o estado.
pub(crate) fn routes() -> Router<Arc<Web>> {
    Router::new()
        .route("/ui/api/biblioteca/faltando", get(missing))
        .route("/ui/api/biblioteca/calendario", get(calendar))
}

fn grid(poster: Option<&String>) -> Option<String> {
    poster.map(|p| p.replacen("/t/p/original/", "/t/p/w342/", 1))
}

/// O que se quer na lista de faltando.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Movies,
    Episodes,
}

/// A data de lançamento que torna o filme disponível: a primeira entre a
/// digital e a física; sem nenhuma, a do cinema.
fn release_date(entry: &CatalogMovie) -> Option<String> {
    let movie = &entry.movie;
    [&movie.digital_release, &movie.physical_release]
        .into_iter()
        .flatten()
        .filter_map(|d| date(Some(d)).map(|parsed| (parsed, d)))
        .min_by_key(|(parsed, _)| *parsed)
        .map(|(_, d)| d.get(..10).unwrap_or(d).to_owned())
        .or_else(|| movie.in_cinemas.clone())
}

fn downloading_movies(grabs: &[Grab]) -> HashSet<i64> {
    grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| g.movie_id)
        .collect()
}

/// Do mais recente ao mais antigo; sem data, no fim.
fn by_date_desc(items: &mut [Value]) {
    items.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                v["data"].as_str().map(str::to_owned),
                v["titulo_ordem"].as_str().map(str::to_owned),
            )
        };
        let (da, ta) = key(a);
        let (db, tb) = key(b);
        match (da, db) {
            (Some(da), Some(db)) => db.cmp(&da).then(ta.cmp(&tb)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => ta.cmp(&tb),
        }
    });
    for item in items.iter_mut() {
        if let Some(object) = item.as_object_mut() {
            object.remove("titulo_ordem");
        }
    }
}

/// A lista de faltando, sem IO. `available` diz se um filme já está
/// disponível (a regra de lançado, com a carência das regras).
fn missing_items(
    series: &[CatalogSeries],
    series_grabs: &[SeriesGrab],
    movies: &[CatalogMovie],
    grabs: &[Grab],
    today: Date,
    available: impl Fn(&CatalogMovie) -> bool,
    only: Option<Kind>,
) -> Vec<Value> {
    let mut items = Vec::new();
    if only != Some(Kind::Movies) {
        let queued = crate::series::queued(series_grabs);
        for entry in series {
            for episode in &entry.episodes {
                let state = crate::series::state(entry, episode, &queued);
                if !matches!(state, EpisodeState::Wanted | EpisodeState::Queued)
                    || !crate::series::aired(&episode.episode, today)
                {
                    continue;
                }
                items.push(json!({
                    "tipo": "episodio",
                    "serie_id": entry.id,
                    "serie": entry.series.title,
                    "poster": grid(entry.series.poster.as_ref()),
                    "prioritario": entry.priority,
                    "episodio_id": episode.id,
                    "temporada": episode.episode.season,
                    "numero": episode.episode.number,
                    "titulo": episode.episode.title,
                    "data": episode.episode.air_date,
                    "baixando": state == EpisodeState::Queued,
                    "titulo_ordem": format!(
                        "{}\u{0}{:05}{:05}",
                        entry.series.title.to_lowercase(),
                        episode.episode.season,
                        episode.episode.number
                    ),
                }));
            }
        }
    }
    if only != Some(Kind::Episodes) {
        let downloading = downloading_movies(grabs);
        for entry in movies {
            if !entry.movie.monitored || entry.movie.file.is_some() || !available(entry) {
                continue;
            }
            items.push(json!({
                "tipo": "filme",
                "filme_id": entry.id,
                "filme": crate::events::label(&entry.movie.title, entry.movie.year),
                "titulo": entry.movie.title,
                "ano": entry.movie.year,
                "poster": grid(entry.extras.poster.as_ref()),
                "prioritario": entry.priority,
                "data": release_date(entry),
                "baixando": downloading.contains(&entry.id),
                "titulo_ordem": entry.movie.title.to_lowercase(),
            }));
        }
    }
    by_date_desc(&mut items);
    items
}

/// O calendário, sem IO: episódios e lançamentos de filme monitorado com
/// data em `[from, to]`, por data.
fn calendar_items(
    series: &[CatalogSeries],
    series_grabs: &[SeriesGrab],
    movies: &[CatalogMovie],
    grabs: &[Grab],
    today: Date,
    from: Date,
    to: Date,
) -> Vec<Value> {
    let within = |text: Option<&str>| date(text).is_some_and(|d| from <= d && d <= to);
    let queued = crate::series::queued(series_grabs);
    let mut items: Vec<(String, u8, String, Value)> = Vec::new();
    for entry in series {
        for episode in &entry.episodes {
            let air = episode.episode.air_date.as_deref();
            if !within(air) {
                continue;
            }
            let state = crate::series::state(entry, episode, &queued);
            items.push((
                air.unwrap_or_default()
                    .get(..10)
                    .unwrap_or_default()
                    .to_owned(),
                1,
                format!(
                    "{}\u{0}{:05}{:05}",
                    entry.series.title.to_lowercase(),
                    episode.episode.season,
                    episode.episode.number
                ),
                json!({
                    "tipo": "episodio",
                    "data": air,
                    "serie_id": entry.id,
                    "serie": entry.series.title,
                    "poster": grid(entry.series.poster.as_ref()),
                    "prioritario": entry.priority,
                    "episodio_id": episode.id,
                    "temporada": episode.episode.season,
                    "numero": episode.episode.number,
                    "titulo": episode.episode.title,
                    "estado": match state {
                        EpisodeState::Have(_) => "tenho",
                        EpisodeState::Skipped => "dispensado",
                        EpisodeState::Wanted | EpisodeState::Queued => "quero",
                    },
                    "baixando": state == EpisodeState::Queued,
                    "exibido": crate::series::aired(&episode.episode, today),
                }),
            ));
        }
    }
    let downloading = downloading_movies(grabs);
    for entry in movies.iter().filter(|m| m.movie.monitored) {
        let movie = &entry.movie;
        let state = if movie.file.is_some() {
            "tenho"
        } else if downloading.contains(&entry.id) {
            "baixando"
        } else {
            "falta"
        };
        for (kind, when) in [
            ("cinema", &movie.in_cinemas),
            ("digital", &movie.digital_release),
            ("fisico", &movie.physical_release),
        ] {
            if !within(when.as_deref()) {
                continue;
            }
            let day = when.as_deref().unwrap_or_default();
            items.push((
                day.get(..10).unwrap_or_default().to_owned(),
                0,
                movie.title.to_lowercase(),
                json!({
                    "tipo": "filme",
                    "data": when,
                    "lancamento": kind,
                    "filme_id": entry.id,
                    "filme": crate::events::label(&movie.title, movie.year),
                    "titulo": movie.title,
                    "ano": movie.year,
                    "poster": grid(entry.extras.poster.as_ref()),
                    "prioritario": entry.priority,
                    "estado": state,
                }),
            ));
        }
    }
    items.sort_by(|a, b| (&a.0, a.1, &a.2).cmp(&(&b.0, b.1, &b.2)));
    items.into_iter().map(|(_, _, _, value)| value).collect()
}

#[derive(Deserialize)]
struct MissingQuery {
    /// `filme` ou `serie` (`episodio` também vale); ausente, os dois.
    #[serde(default)]
    tipo: Option<String>,
}

async fn missing(
    State(web): Shared,
    headers: HeaderMap,
    Query(q): Query<MissingQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let only = match q.tipo.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("filme" | "filmes") => Some(Kind::Movies),
        Some("serie" | "series" | "episodio" | "episodios") => Some(Kind::Episodes),
        Some(other) => {
            return Err(fail(bad(format!(
                "tipo `{other}` inválido: use filme ou serie"
            ))));
        }
    };
    let (series, series_grabs, movies, grabs) = tokio::try_join!(
        store.series_list(),
        store.series_grabs(),
        store.movies(),
        store.grabs()
    )
    .map_err(|e| fail(bad(e)))?;
    let rules = crate::rules::stored(store)
        .await
        .map_err(|e| fail(crate::web::anyhow_bad(&e)))?;
    let today = crate::series::today();
    let items = missing_items(
        &series,
        &series_grabs,
        &movies,
        &grabs,
        today,
        |m| crate::library::is_available(&m.movie, today, rules.carencia_dias),
        only,
    );
    ok(&json!({ "total": items.len(), "itens": items }))
}

#[derive(Deserialize)]
struct CalendarQuery {
    #[serde(default)]
    de: Option<String>,
    #[serde(default)]
    ate: Option<String>,
}

/// O intervalo pedido; o que faltar vale hoje −7 e hoje +30.
fn range(q: &CalendarQuery, today: Date) -> Result<(Date, Date), String> {
    let read = |text: &Option<String>, fallback: Date, name: &str| match text.as_deref() {
        None | Some("") => Ok(fallback),
        Some(text) => date(Some(text))
            .filter(|_| text.len() == 10)
            .ok_or_else(|| format!("`{name}` inválido: use AAAA-MM-DD")),
    };
    let from = read(&q.de, today - Duration::days(7), "de")?;
    let to = read(&q.ate, today + Duration::days(30), "ate")?;
    if to < from {
        return Err("`ate` antes de `de`".into());
    }
    if (to - from).whole_days() > MAX_DAYS {
        return Err(format!("intervalo maior que {MAX_DAYS} dias"));
    }
    Ok((from, to))
}

async fn calendar(
    State(web): Shared,
    headers: HeaderMap,
    Query(q): Query<CalendarQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let today = crate::series::today();
    let (from, to) = range(&q, today).map_err(|e| fail(bad(e)))?;
    let (series, series_grabs, movies, grabs) = tokio::try_join!(
        store.series_list(),
        store.series_grabs(),
        store.movies(),
        store.grabs()
    )
    .map_err(|e| fail(bad(e)))?;
    let items = calendar_items(&series, &series_grabs, &movies, &grabs, today, from, to);
    ok(&json!({
        "de": from.to_string(),
        "ate": to.to_string(),
        "total": items.len(),
        "itens": items,
    }))
}

#[cfg(test)]
mod tests {
    use acervo_parser::{Quality, QualityModel, Revision};
    use acervo_store::{
        CatalogEpisode, CatalogEpisodeFile, Episode, EpisodeFile, Movie, MovieExtras, MovieFile,
        Series, Skip,
    };

    use super::*;

    fn day(text: &str) -> Date {
        date(Some(text)).unwrap()
    }

    fn episode(
        id: i64,
        number: u16,
        air: Option<&str>,
        file: Option<i64>,
        skip: Option<Skip>,
    ) -> CatalogEpisode {
        CatalogEpisode {
            id,
            episode: Episode {
                season: 1,
                number,
                tmdb_id: None,
                title: Some(format!("Ep {number}")),
                air_date: air.map(str::to_owned),
                overview: None,
                runtime: 0,
            },
            skip,
            skipped_at: None,
            file_id: file,
        }
    }

    fn series() -> CatalogSeries {
        CatalogSeries {
            id: 1,
            series: Series {
                tmdb_id: 1,
                tvdb_id: None,
                imdb_id: None,
                title: "Show".into(),
                original_title: None,
                metadata_title: None,
                original_language: None,
                year: None,
                status: None,
                overview: None,
                network: None,
                runtime: 0,
                poster: Some("https://image.tmdb.org/t/p/original/p.jpg".into()),
                fanart: None,
                path: "/s/Show".into(),
                season_folder: true,
                monitor_new: true,
                added: None,
                refreshed_at: None,
                alternate_titles: Vec::new(),
            },
            episodes: vec![
                episode(11, 1, Some("2026-09-01"), Some(100), None),
                episode(12, 2, Some("2026-09-08"), None, None),
                episode(13, 3, Some("2026-09-15"), None, Some(Skip::Deleted)),
                episode(14, 4, Some("2026-09-22"), None, None),
                episode(15, 5, Some("2026-10-20"), None, None),
                episode(16, 6, None, None, None),
            ],
            files: vec![CatalogEpisodeFile {
                id: 100,
                file: EpisodeFile {
                    relative_path: "e1.mkv".into(),
                    size: 1,
                    quality: QualityModel {
                        quality: Quality::WebDl1080p,
                        revision: Revision::default(),
                    },
                    languages: Vec::new(),
                    release_group: None,
                    scene_name: None,
                    date_added: None,
                },
            }],
            priority: true,
            scene: Vec::new(),
            subtitles: Vec::new(),
        }
    }

    fn movie(id: i64, title: &str, digital: Option<&str>, file: bool) -> CatalogMovie {
        CatalogMovie {
            id,
            movie: Movie {
                tmdb_id: u32::try_from(id).unwrap(),
                imdb_id: None,
                title: title.into(),
                original_title: None,
                original_language: None,
                year: Some(2026),
                status: None,
                monitored: true,
                path: format!("/f/{title}"),
                added: None,
                file: file.then(|| MovieFile {
                    relative_path: "f.mkv".into(),
                    size: 1,
                    quality: QualityModel {
                        quality: Quality::WebDl1080p,
                        revision: Revision::default(),
                    },
                    languages: Vec::new(),
                    release_group: None,
                    edition: None,
                    scene_name: None,
                    date_added: None,
                }),
                runtime: 0,
                secondary_year: None,
                clean_title: None,
                alternate_titles: Vec::new(),
                in_cinemas: Some("2026-06-01".into()),
                digital_release: digital.map(str::to_owned),
                physical_release: None,
                overview: None,
            },
            extras: MovieExtras::default(),
            priority: false,
            subtitles: Vec::new(),
        }
    }

    fn grab(movie_id: i64) -> Grab {
        Grab {
            id: 1,
            movie_id,
            hash: "h".into(),
            title: "t".into(),
            indexer: "i".into(),
            quality: Quality::WebDl1080p,
            size: 1,
            grabbed_at: "2026-10-01T00:00:00Z".into(),
            state: GrabState::Downloading,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces: None,
        }
    }

    fn series_grab(episodes: Vec<i64>) -> SeriesGrab {
        SeriesGrab {
            id: 1,
            series_id: 1,
            episode_ids: episodes,
            hash: "s".into(),
            title: "t".into(),
            indexer: "i".into(),
            quality: Quality::WebDl1080p,
            size: 1,
            grabbed_at: "2026-10-01T00:00:00Z".into(),
            state: GrabState::Downloading,
            message: None,
            finished_at: None,
        }
    }

    #[test]
    fn faltando_episodios_em_quero_exibidos_e_filmes_disponiveis() {
        let movies = [
            movie(1, "Antigo", Some("2026-08-01"), false),
            movie(2, "Tenho", Some("2026-08-01"), true),
            movie(3, "Baixando", Some("2026-09-10"), false),
            movie(4, "Futuro", Some("2026-12-01"), false),
        ];
        let items = missing_items(
            &[series()],
            &[series_grab(vec![14])],
            &movies,
            &[grab(3)],
            day("2026-10-03"),
            |m| m.id != 4,
            None,
        );
        let got: Vec<(String, Option<&str>, bool)> = items
            .iter()
            .map(|i| {
                (
                    format!(
                        "{}:{}",
                        i["tipo"].as_str().unwrap(),
                        i["episodio_id"]
                            .as_i64()
                            .or(i["filme_id"].as_i64())
                            .unwrap()
                    ),
                    i["data"].as_str(),
                    i["baixando"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("episodio:14".into(), Some("2026-09-22"), true),
                ("filme:3".into(), Some("2026-09-10"), true),
                ("episodio:12".into(), Some("2026-09-08"), false),
                ("filme:1".into(), Some("2026-08-01"), false),
            ]
        );
        assert_eq!(items[0]["prioritario"], true);
        assert_eq!(items[0]["poster"], "https://image.tmdb.org/t/p/w342/p.jpg");
        assert!(items[0].get("titulo_ordem").is_none());
        let only_movies = missing_items(
            &[series()],
            &[],
            &movies,
            &[],
            day("2026-10-03"),
            |_| true,
            Some(Kind::Movies),
        );
        assert!(only_movies.iter().all(|i| i["tipo"] == "filme"));
    }

    #[test]
    fn calendario_com_episodios_e_lancamentos_de_filme() {
        let movies = [
            movie(1, "Digital", Some("2026-09-20"), false),
            movie(2, "Tenho", Some("2026-09-25"), true),
        ];
        let items = calendar_items(
            &[series()],
            &[series_grab(vec![14])],
            &movies,
            &[],
            day("2026-10-03"),
            day("2026-09-10"),
            day("2026-10-31"),
        );
        let got: Vec<(&str, &str, &str)> = items
            .iter()
            .map(|i| {
                (
                    i["data"].as_str().unwrap(),
                    i["tipo"].as_str().unwrap(),
                    i["estado"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("2026-09-15", "episodio", "dispensado"),
                ("2026-09-20", "filme", "falta"),
                ("2026-09-22", "episodio", "quero"),
                ("2026-09-25", "filme", "tenho"),
                ("2026-10-20", "episodio", "quero"),
            ]
        );
        assert_eq!(items[1]["lancamento"], "digital");
        assert_eq!(items[2]["baixando"], true);
        assert_eq!(items[4]["exibido"], false);
    }

    #[test]
    fn intervalo_padrao_e_validacao() {
        let today = day("2026-10-03");
        let q = |de: Option<&str>, ate: Option<&str>| CalendarQuery {
            de: de.map(str::to_owned),
            ate: ate.map(str::to_owned),
        };
        assert_eq!(
            range(&q(None, None), today).unwrap(),
            (day("2026-09-26"), day("2026-11-02"))
        );
        assert_eq!(
            range(&q(Some("2026-01-01"), Some("2026-01-31")), today).unwrap(),
            (day("2026-01-01"), day("2026-01-31"))
        );
        assert!(range(&q(Some("2026-02-01"), Some("2026-01-01")), today).is_err());
        assert!(range(&q(Some("ontem"), None), today).is_err());
        assert!(range(&q(Some("2026-01-01"), Some("2028-01-01")), today).is_err());
    }
}

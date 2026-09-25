//! O pedaço da API v3 do gerenciador de filmes que os apps em volta usam: o
//! de pedidos (listar, procurar, adicionar, monitorar, remover e buscar
//! filmes; fila de downloads) e o de legendas (filmes com o arquivo e as
//! faixas dele, pastas raiz, tags, histórico de grabs).
//!
//! As respostas seguem o formato da referência nos campos que esses apps
//! leem; os ids são os do catálogo, que no corte ficaram iguais aos do
//! gerenciador. A chave é a mesma da superfície Torznab, em `X-Api-Key` ou
//! `apikey`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use acervo_api::Catalog;
use acervo_parser::{Language, Quality};
use acervo_store::{CatalogMovie, GrabState, Store};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::serve::Database;
use crate::shadow::now_rfc3339;

/// Versão anunciada: o app de legendas escolhe o dialeto da API por ela, e
/// o desta é o da série 5.
const VERSION: &str = "5.26.2.10099";

#[derive(Debug)]
pub struct V3 {
    pub config: Arc<Config>,
    pub database: Database,
    pub catalog: Catalog,
    pub api_key: String,
}

type Shared = State<Arc<V3>>;

/// Erro no formato que os clientes da referência entendem.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = if self.0 == StatusCode::BAD_REQUEST {
            json!([{ "propertyName": "", "errorMessage": self.1 }])
        } else {
            json!({ "message": self.1 })
        };
        (self.0, Json(body)).into_response()
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "NotFound".into())
}

type ApiResult = Result<Response, ApiError>;

#[allow(clippy::unnecessary_wraps)] // Mesma forma de todo handler.
fn ok(value: Value) -> ApiResult {
    Ok(Json(value).into_response())
}

/// Chave certa em `X-Api-Key` ou em `apikey`, e o banco de pé.
fn store<'a>(
    v3: &'a V3,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<&'a Store, ApiError> {
    let presented = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            query
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("apikey"))
                .map(|(_, v)| v.as_str())
        })
        .unwrap_or_default();
    if !constant_time_eq(presented.as_bytes(), v3.api_key.as_bytes()) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into()));
    }
    v3.database
        .get()
        .map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn router(v3: Arc<V3>) -> Router {
    Router::new()
        .route("/api/system/status", get(system_status))
        .route("/api/v3/system/status", get(system_status))
        .route("/api/v3/qualityprofile", get(quality_profiles))
        .route("/api/v3/qualityProfile", get(quality_profiles))
        .route("/api/v3/rootfolder", get(root_folders))
        .route("/api/v3/rootFolder", get(root_folders))
        .route("/api/v3/tag", get(tags).post(create_tag))
        .route("/api/v3/tag/{id}", get(tag).put(rename_tag))
        .route(
            "/api/v3/movie",
            get(movies).post(add_movie).put(update_movie),
        )
        .route("/api/v3/movie/lookup", get(lookup))
        .route(
            "/api/v3/movie/{id}",
            get(movie).put(update_movie).delete(delete_movie),
        )
        .route("/api/v3/queue", get(queue))
        .route("/api/v3/command", post(command))
        .route("/api/v3/history", get(history))
        .with_state(v3)
}

async fn system_status(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    store(&v3, &headers, &query)?;
    ok(json!({
        "appName": "acervo-hub",
        "instanceName": "acervo-hub",
        "version": VERSION,
        "urlBase": "",
        "isDebug": false,
        "isProduction": true,
        "isAdmin": false,
        "isUserInteractive": false,
        "startupPath": "/",
        "appData": "/var/lib/acervo-hub",
        "osName": "linux",
        "isDocker": true,
        "isLinux": true,
        "isOsx": false,
        "isWindows": false,
        "branch": "main",
        "authentication": "external",
        "migrationVersion": 0,
        "runtimeVersion": "",
        "databaseType": "postgreSQL",
        "startTime": now_rfc3339(),
    }))
}

fn language(name: Option<&str>) -> Value {
    let language = name
        .and_then(Language::from_name)
        .unwrap_or(Language::Unknown);
    json!({ "id": language.id(), "name": language.name() })
}

fn quality_json(quality: Quality) -> Value {
    json!({
        "id": quality.id(),
        "name": quality.name(),
        "source": format!("{:?}", quality.source()).to_lowercase(),
        "resolution": quality.resolution(),
        "modifier": format!("{:?}", quality.modifier()).to_lowercase(),
    })
}

async fn quality_profiles(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let (ids, profiles) =
        tokio::try_join!(store.profile_ids(), store.profiles()).map_err(internal)?;
    ok(Value::Array(
        ids.iter()
            .filter_map(|(id, name)| {
                let profile = profiles.iter().find(|p| &p.name == name)?;
                Some(json!({
                    "id": id,
                    "name": name,
                    "upgradeAllowed": profile.upgrade_allowed,
                    "cutoff": profile
                        .cutoff
                        .and_then(|c| profile.items.get(c))
                        .and_then(|item| item.qualities.first())
                        .map(|q| q.id()),
                    "items": profile.items.iter().map(|item| json!({
                        "name": item.name,
                        "allowed": item.allowed,
                        "items": item.qualities.iter().map(|q| json!({
                            "quality": quality_json(*q),
                            "items": [],
                            "allowed": item.allowed,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "minFormatScore": profile.min_format_score,
                    "cutoffFormatScore": profile.cutoff_format_score,
                    "formatItems": [],
                    "language": language(profile.language.as_deref()),
                }))
            })
            .collect(),
    ))
}

async fn root_folders(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    store(&v3, &headers, &query)?;
    let map = v3.config.path_map();
    let roots = v3.config.movies.root_folders.clone();
    let listed = tokio::task::spawn_blocking(move || {
        roots
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let free = map
                    .to_host(Path::new(path))
                    .ok()
                    .and_then(|host| acervo_fs::free_space(&host).ok());
                json!({
                    "id": index + 1,
                    "path": path,
                    "accessible": free.is_some(),
                    "freeSpace": free,
                    "unmappedFolders": [],
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(internal)?;
    ok(Value::Array(listed))
}

async fn tags(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let tags = store.tags().await.map_err(internal)?;
    ok(Value::Array(
        tags.iter()
            .map(|t| json!({ "id": t.id, "label": t.label }))
            .collect(),
    ))
}

async fn tag(
    State(v3): Shared,
    UrlPath(id): UrlPath<i64>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let tags = store.tags().await.map_err(internal)?;
    let tag = tags.iter().find(|t| t.id == id).ok_or_else(not_found)?;
    ok(json!({ "id": tag.id, "label": tag.label }))
}

#[derive(Deserialize)]
struct TagBody {
    label: String,
}

async fn create_tag(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<TagBody>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let tag = store.create_tag(&body.label).await.map_err(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": tag.id, "label": tag.label })),
    )
        .into_response())
}

async fn rename_tag(
    State(v3): Shared,
    UrlPath(id): UrlPath<i64>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<TagBody>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let tag = store
        .rename_tag(id, &body.label)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    ok(json!({ "id": tag.id, "label": tag.label }))
}

/// Data do catálogo (`AAAA-MM-DD`) no formato da referência.
fn instant(day: Option<&str>) -> Value {
    day.map_or(Value::Null, |d| Value::String(format!("{d}T00:00:00Z")))
}

fn parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// Um filme do catálogo no formato da referência.
fn movie_json(entry: &CatalogMovie, profiles: &HashMap<String, i64>, delay_days: i64) -> Value {
    let movie = &entry.movie;
    let today = time::OffsetDateTime::now_utc().date();
    let file = movie.file.as_ref().map(|file| {
        json!({
            "id": file.id,
            "movieId": entry.id,
            "relativePath": file.relative_path,
            "path": format!("{}/{}", movie.path, file.relative_path),
            "size": file.size,
            "dateAdded": file.date_added,
            "sceneName": file.scene_name,
            "releaseGroup": file.release_group,
            "edition": file.edition.clone().unwrap_or_default(),
            "languages": file.languages.iter().map(|l| language(Some(l))).collect::<Vec<_>>(),
            "quality": {
                "quality": quality_json(file.quality.quality),
                "revision": {
                    "version": file.quality.revision.version,
                    "real": file.quality.revision.real,
                    "isRepack": file.quality.revision.is_repack,
                },
            },
            "customFormats": [],
            "customFormatScore": 0,
            "indexerFlags": 0,
            "mediaInfo": file.media_info,
            "originalFilePath": file.scene_name,
            "qualityCutoffNotMet": false,
        })
    });
    let size = movie.file.as_ref().map_or(0, |f| f.size);
    let release_date = [
        movie.digital_release.as_deref(),
        movie.physical_release.as_deref(),
    ]
    .into_iter()
    .flatten()
    .min();
    let mut images = Vec::new();
    for (kind, url) in [
        ("poster", &entry.extras.poster),
        ("fanart", &entry.extras.fanart),
    ] {
        if let Some(url) = url {
            images.push(json!({ "coverType": kind, "url": url, "remoteUrl": url }));
        }
    }
    let mut value = json!({
        "id": entry.id,
        "title": movie.title,
        "originalTitle": movie.original_title,
        "originalLanguage": language(movie.original_language.as_deref()),
        "alternateTitles": movie.alternate_titles.iter().map(|t| json!({
            "sourceType": "tmdb",
            "movieMetadataId": entry.id,
            "title": t,
        })).collect::<Vec<_>>(),
        "secondaryYear": movie.secondary_year,
        "secondaryYearSourceId": 0,
        "sortTitle": movie.title.to_lowercase(),
        "sizeOnDisk": size,
        "status": crate::library::status(movie, today),
        "overview": movie.overview.clone().unwrap_or_default(),
        "inCinemas": instant(movie.in_cinemas.as_deref()),
        "physicalRelease": instant(movie.physical_release.as_deref()),
        "digitalRelease": instant(movie.digital_release.as_deref()),
        "releaseDate": instant(release_date),
        "images": images,
        "website": "",
        "year": movie.year.unwrap_or(0),
        "hasFile": movie.file.is_some(),
        "youTubeTrailerId": "",
        "studio": "",
        "path": movie.path,
        "qualityProfileId": movie.quality_profile.as_ref().and_then(|p| profiles.get(p)),
        "monitored": movie.monitored,
        "minimumAvailability": movie.minimum_availability.clone().unwrap_or_else(|| "released".into()),
        "isAvailable": crate::library::is_available(movie, today, delay_days),
        "folderName": movie.path,
        "runtime": movie.runtime,
        "cleanTitle": movie.clean_title.clone().unwrap_or_default(),
        "imdbId": movie.imdb_id,
        "tmdbId": movie.tmdb_id,
        "titleSlug": movie.tmdb_id.to_string(),
        "rootFolderPath": parent(&movie.path),
        "genres": [],
        "tags": movie.tags,
        "added": movie.added,
        "ratings": {},
        "popularity": 0,
        "statistics": {
            "movieFileCount": usize::from(movie.file.is_some()),
            "sizeOnDisk": size,
            "releaseGroups": movie.file.iter().filter_map(|f| f.release_group.clone()).collect::<Vec<_>>(),
        },
    });
    if let Some(file) = file {
        value["movieFile"] = file;
    }
    value
}

/// Ids de perfil pelo nome, como a API mostra.
async fn profile_map(store: &Store) -> Result<HashMap<String, i64>, ApiError> {
    Ok(store
        .profile_ids()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|(id, name)| (name, id))
        .collect())
}

async fn movies(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let (movies, profiles) = tokio::try_join!(
        async { store.movies().await.map_err(internal) },
        profile_map(store)
    )?;
    let tmdb: Option<u32> = query.get("tmdbId").and_then(|t| t.parse().ok());
    ok(Value::Array(
        movies
            .iter()
            .filter(|m| tmdb.is_none_or(|t| m.movie.tmdb_id == t))
            .map(|m| movie_json(m, &profiles, 0))
            .collect(),
    ))
}

async fn movie(
    State(v3): Shared,
    UrlPath(id): UrlPath<i64>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let (movies, profiles) = tokio::try_join!(
        async { store.movies().await.map_err(internal) },
        profile_map(store)
    )?;
    let entry = movies.iter().find(|m| m.id == id).ok_or_else(not_found)?;
    ok(movie_json(entry, &profiles, 0))
}

/// `term=tmdb:123`, `term=imdb:tt123`: o filme do catálogo, ou o que o TMDB
/// diz dele (sem id, ainda não adicionado).
async fn lookup(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let term = query.get("term").map(|t| t.trim()).unwrap_or_default();
    let tmdb = crate::metadata::tmdb(&v3.config, store)
        .await
        .map_err(internal)?;
    let tmdb_id: Option<u32> = if let Some(id) = term.strip_prefix("tmdb:") {
        id.trim().parse().ok()
    } else if let Some(imdb) = term.strip_prefix("imdb:") {
        match &tmdb {
            Some(tmdb) => tmdb.find_imdb(imdb.trim()).await.map_err(internal)?,
            None => None,
        }
    } else {
        None
    };
    let Some(tmdb_id) = tmdb_id else {
        return ok(json!([]));
    };
    let (movies, profiles) = tokio::try_join!(
        async { store.movies().await.map_err(internal) },
        profile_map(store)
    )?;
    if let Some(entry) = movies.iter().find(|m| m.movie.tmdb_id == tmdb_id) {
        return ok(json!([movie_json(entry, &profiles, 0)]));
    }
    let Some(tmdb) = tmdb else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "chave do TMDB não configurada".into(),
        ));
    };
    let Ok((movie, extras)) = crate::library::lookup(&tmdb, tmdb_id).await else {
        return ok(json!([]));
    };
    let entry = CatalogMovie {
        id: 0,
        movie,
        origin: None,
        extras,
    };
    let mut value = movie_json(&entry, &profiles, 0);
    if let Some(object) = value.as_object_mut() {
        object.remove("id");
        object.insert("folder".into(), json!(entry.extras.metadata_title));
    }
    ok(json!([value]))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddBody {
    tmdb_id: u32,
    #[serde(default)]
    quality_profile_id: Option<i64>,
    #[serde(default)]
    root_folder_path: Option<String>,
    #[serde(default)]
    monitored: Option<bool>,
    #[serde(default)]
    minimum_availability: Option<String>,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    add_options: Option<AddOptions>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddOptions {
    #[serde(default)]
    search_for_movie: bool,
}

async fn add_movie(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<AddBody>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let tmdb = crate::metadata::require_tmdb(&v3.config, store)
        .await
        .map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let profiles = profile_map(store).await?;
    let profile = match body.quality_profile_id {
        Some(id) => profiles
            .iter()
            .find(|(_, pid)| **pid == id)
            .map(|(name, _)| name.clone()),
        None => profiles.keys().next().cloned(),
    }
    .ok_or_else(|| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "Quality profile does not exist".into(),
        )
    })?;
    let id = crate::library::add(
        store,
        &tmdb,
        &crate::library::AddRequest {
            tmdb_id: body.tmdb_id,
            quality_profile: profile,
            root_folder: body
                .root_folder_path
                .or_else(|| v3.config.movies.root_folders.first().cloned())
                .unwrap_or_else(|| "/media/movies".into()),
            monitored: body.monitored.unwrap_or(true),
            minimum_availability: body
                .minimum_availability
                .unwrap_or_else(|| "released".into()),
            tags: body.tags,
        },
    )
    .await
    .map_err(|e| {
        let message = format!("{e:#}");
        if message.contains("já está no catálogo") {
            ApiError(
                StatusCode::BAD_REQUEST,
                "This movie has already been added".into(),
            )
        } else {
            ApiError(StatusCode::BAD_REQUEST, message)
        }
    })?;
    if body.add_options.is_some_and(|o| o.search_for_movie) {
        search_later(&v3, vec![id]);
    }
    let movies = store.movies().await.map_err(internal)?;
    let entry = movies.iter().find(|m| m.id == id).ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, Json(movie_json(entry, &profiles, 0))).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    monitored: Option<bool>,
    #[serde(default)]
    quality_profile_id: Option<i64>,
    #[serde(default)]
    minimum_availability: Option<String>,
    #[serde(default)]
    tags: Option<Vec<i64>>,
    #[serde(default)]
    add_options: Option<AddOptions>,
}

async fn update_movie(
    State(v3): Shared,
    path_id: Option<UrlPath<i64>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<UpdateBody>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let id = path_id
        .map(|UrlPath(id)| id)
        .or(body.id)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "id ausente".into()))?;
    let profiles = profile_map(store).await?;
    let movies = store.movies().await.map_err(internal)?;
    let entry = movies.iter().find(|m| m.id == id).ok_or_else(not_found)?;
    let mut movie = entry.movie.clone();
    if let Some(monitored) = body.monitored {
        movie.monitored = monitored;
    }
    if let Some(profile) = body
        .quality_profile_id
        .and_then(|pid| profiles.iter().find(|(_, p)| **p == pid))
    {
        movie.quality_profile = Some(profile.0.clone());
    }
    if let Some(minimum) = body.minimum_availability {
        movie.minimum_availability = Some(minimum);
    }
    if let Some(tags) = body.tags {
        movie.tags = tags;
    }
    store.update_movie(id, &movie).await.map_err(internal)?;
    if body.add_options.is_some_and(|o| o.search_for_movie) {
        search_later(&v3, vec![id]);
    }
    let updated = CatalogMovie {
        movie,
        ..entry.clone()
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(movie_json(&updated, &profiles, 0)),
    )
        .into_response())
}

/// Tira o filme do catálogo; com `deleteFiles`, apaga também a pasta dele,
/// como a referência faz sem lixeira. A pasta tem de estar dentro de uma
/// pasta raiz e não ser ela.
async fn delete_movie(
    State(v3): Shared,
    UrlPath(id): UrlPath<i64>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let movies = store.movies().await.map_err(internal)?;
    let entry = movies.iter().find(|m| m.id == id).ok_or_else(not_found)?;
    let delete_files = query
        .get("deleteFiles")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if delete_files {
        let folder = Path::new(&entry.movie.path);
        let inside_root = v3.config.movies.root_folders.iter().any(|root| {
            folder
                .parent()
                .is_some_and(|p| p == Path::new(root.trim_end_matches('/')))
        });
        if !inside_root {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                format!(
                    "a pasta `{}` não está direto numa pasta raiz; nada apagado",
                    entry.movie.path
                ),
            ));
        }
        let host = v3.config.path_map().to_host(folder).map_err(internal)?;
        tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&host) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        tracing::info!(
            filme = entry.movie.title,
            pasta = entry.movie.path,
            "filme apagado a pedido"
        );
    }
    store.delete_movie(id).await.map_err(internal)?;
    ok(json!({}))
}

/// Busca e pega em segundo plano, como o comando da referência: quem pediu
/// não espera a busca.
fn search_later(v3: &Arc<V3>, ids: Vec<i64>) {
    let v3 = Arc::clone(v3);
    tokio::spawn(async move {
        let Ok(store) = v3.database.get() else {
            return;
        };
        for id in ids {
            match crate::grab::grab(&v3.config, store, &v3.catalog, id, true).await {
                Ok(report) => tracing::info!(
                    filme = report.filme,
                    pegou = report.escolhido.as_ref().map(|p| p.titulo.as_str()),
                    "busca pedida pela API"
                ),
                Err(error) => tracing::info!(filme = id, "busca pedida pela API: {error:#}"),
            }
        }
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommandBody {
    name: String,
    #[serde(default)]
    movie_ids: Vec<i64>,
    #[serde(default)]
    movie_id: Option<i64>,
}

async fn command(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<CommandBody>,
) -> ApiResult {
    store(&v3, &headers, &query)?;
    match body.name.as_str() {
        "MoviesSearch" => {
            let mut ids = body.movie_ids.clone();
            ids.extend(body.movie_id);
            search_later(&v3, ids);
        }
        "RefreshMonitoredDownloads" | "DownloadedMoviesScan" => {
            let v3 = Arc::clone(&v3);
            tokio::spawn(async move {
                if let Ok(store) = v3.database.get()
                    && let Err(error) = crate::grab::import_downloads(&v3.config, store, true).await
                {
                    tracing::warn!("importação pedida pela API: {error:#}");
                }
            });
        }
        // Reler a pasta e atualizar metadados: o serviço já faz sozinho.
        _ => {}
    }
    let now = now_rfc3339();
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": 1,
            "name": body.name,
            "commandName": body.name,
            "status": "queued",
            "queued": now,
            "trigger": "manual",
            "priority": "normal",
        })),
    )
        .into_response())
}

async fn queue(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let grabs: Vec<_> = store
        .grabs()
        .await
        .map_err(internal)?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .collect();
    let client = match &v3.config.qbittorrent {
        Some(spec) if !grabs.is_empty() => acervo_clients::QbitClient::login(
            &spec.url,
            &spec.username,
            &spec.password,
            v3.config.http_timeout(),
        )
        .await
        .ok(),
        _ => None,
    };
    let mut records = Vec::new();
    for grab in &grabs {
        let torrent = match &client {
            Some(client) => client.torrent(&grab.hash).await.ok().flatten(),
            None => None,
        };
        let progress = torrent.as_ref().map_or(0.0, |t| t.progress);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let left = ((1.0 - progress).max(0.0) * grab.size as f64) as u64;
        records.push(json!({
            "id": grab.id,
            "movieId": grab.movie_id,
            "title": grab.title,
            "size": grab.size,
            "sizeleft": left,
            "timeleft": Value::Null,
            "estimatedCompletionTime": Value::Null,
            "status": if progress >= 1.0 { "completed" } else { "downloading" },
            "trackedDownloadStatus": "ok",
            "trackedDownloadState": if progress >= 1.0 { "importPending" } else { "downloading" },
            "statusMessages": [],
            "downloadId": grab.hash.to_uppercase(),
            "protocol": "torrent",
            "downloadClient": "qBittorrent",
            "indexer": grab.indexer,
            "quality": { "quality": quality_json(grab.quality), "revision": { "version": 1, "real": 0, "isRepack": false } },
            "languages": [],
            "customFormats": [],
        }));
    }
    ok(json!({
        "page": 1,
        "pageSize": records.len().max(10),
        "sortKey": "timeleft",
        "sortDirection": "ascending",
        "totalRecords": records.len(),
        "records": records,
    }))
}

/// Grabs de um filme (`eventType=1`), no formato paginado da referência.
async fn history(
    State(v3): Shared,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let store = store(&v3, &headers, &query)?;
    let wanted: Vec<i64> = query
        .get("movieIds")
        .map(|ids| {
            ids.split(',')
                .filter_map(|id| id.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let records: Vec<Value> = store
        .grabs()
        .await
        .map_err(internal)?
        .iter()
        .filter(|g| wanted.is_empty() || wanted.contains(&g.movie_id))
        .map(|g| {
            json!({
                "id": g.id,
                "movieId": g.movie_id,
                "sourceTitle": g.title,
                "quality": { "quality": quality_json(g.quality), "revision": { "version": 1, "real": 0, "isRepack": false } },
                "date": g.grabbed_at,
                "eventType": "grabbed",
                "downloadId": g.hash.to_uppercase(),
                "data": { "indexer": g.indexer, "size": g.size.to_string(), "nzbInfoUrl": "" },
            })
        })
        .collect();
    ok(json!({
        "page": 1,
        "pageSize": records.len().max(10),
        "totalRecords": records.len(),
        "records": records,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use acervo_parser::{QualityModel, Revision};
    use acervo_store::{Movie, MovieExtras, MovieFile};

    #[test]
    fn filme_no_formato_que_os_apps_leem() {
        let entry = CatalogMovie {
            id: 1088,
            origin: None,
            extras: MovieExtras::default(),
            movie: Movie {
                tmdb_id: 1_249_199,
                imdb_id: Some("tt11327404".into()),
                title: "19 Outra Vez".into(),
                original_title: Some("The Throwback".into()),
                original_language: Some("English".into()),
                year: Some(2024),
                status: None,
                minimum_availability: Some("released".into()),
                monitored: true,
                quality_profile: Some("Any".into()),
                path: "/media/movies/The Throwback (2024) {imdb-tt11327404}".into(),
                added: None,
                file: Some(MovieFile {
                    relative_path: "The Throwback (2024) {imdb-tt11327404}.mkv".into(),
                    size: 10,
                    quality: QualityModel {
                        quality: Quality::WebDl1080p,
                        revision: Revision::default(),
                    },
                    languages: vec!["Portuguese".into()],
                    release_group: Some("GRUPO".into()),
                    edition: None,
                    scene_name: None,
                    date_added: None,
                    id: Some(77),
                    media_info: Some(json!({ "audioLanguages": "Portuguese" })),
                }),
                runtime: 96,
                secondary_year: None,
                clean_title: Some("thethrowback".into()),
                alternate_titles: Vec::new(),
                available: true,
                in_cinemas: None,
                digital_release: Some("2024-03-15".into()),
                physical_release: None,
                overview: None,
                tags: vec![3],
            },
        };
        let profiles = HashMap::from([("Any".to_owned(), 1)]);
        let value = movie_json(&entry, &profiles, 0);
        // O de pedidos lê estes.
        assert_eq!(value["id"], 1088);
        assert_eq!(value["tmdbId"], 1_249_199);
        assert_eq!(value["titleSlug"], "1249199");
        assert_eq!(value["hasFile"], true);
        assert_eq!(value["monitored"], true);
        // O de legendas, estes.
        assert_eq!(value["qualityProfileId"], 1);
        assert_eq!(value["movieFile"]["id"], 77);
        assert_eq!(value["movieFile"]["quality"]["quality"]["resolution"], 1080);
        assert_eq!(value["movieFile"]["quality"]["quality"]["source"], "webdl");
        assert_eq!(
            value["movieFile"]["mediaInfo"]["audioLanguages"],
            "Portuguese"
        );
        assert_eq!(
            value["movieFile"]["path"],
            "/media/movies/The Throwback (2024) {imdb-tt11327404}/The Throwback (2024) {imdb-tt11327404}.mkv"
        );
        assert_eq!(value["originalLanguage"]["name"], "English");
        assert_eq!(value["digitalRelease"], "2024-03-15T00:00:00Z");
        assert_eq!(value["status"], "released");
        assert_eq!(value["isAvailable"], true);
        assert_eq!(value["rootFolderPath"], "/media/movies");
        assert_eq!(value["tags"], json!([3]));
    }
}

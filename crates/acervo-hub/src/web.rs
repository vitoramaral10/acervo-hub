//! A parte da interface que faz do acervo um gerenciador: adicionar, editar
//! e remover filmes, busca interativa, fila e histórico, lista de bloqueio,
//! exclusões, perfis, formatos, tamanhos, regras, notificações, listas de
//! importação e a migração.
//!
//! Tudo sob `/ui/api/biblioteca/`, com a mesma entrada da tela (sessão ou
//! chave, e o cabeçalho anti-CSRF em ação que muda estado).

// Handler devolve a resposta de erro pronta; é o formato do axum, e caixa
// aqui só custaria alocação em todo erro.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acervo_api::{Accounts, Catalog, authorize_ui, ui_json};
use acervo_decision::Mode;
use acervo_parser::Quality;
use acervo_store::{
    CustomFormat, ImportList, ProfileItem, QualityDefinition, QualityProfile, Store,
};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::{get, post, put};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::serve::Database;
use crate::shadow::{Decider, now_rfc3339};

/// Por quanto tempo uma busca interativa fica guardada para o grab.
const SEARCH_TTL: Duration = Duration::from_secs(30 * 60);

type Cached = (Instant, Vec<acervo_indexers::Release>);

#[derive(Debug)]
pub struct Web {
    pub config: Arc<Config>,
    pub database: Database,
    pub catalog: Catalog,
    pub api_key: String,
    pub accounts: Option<Arc<dyn Accounts>>,
    /// Última busca interativa de cada filme: o grab escolhe dela pelo guid.
    pub searches: tokio::sync::Mutex<HashMap<i64, Cached>>,
}

type Shared = State<Arc<Web>>;

struct WebError(StatusCode, String);

impl axum::response::IntoResponse for WebError {
    fn into_response(self) -> Response {
        ui_json(self.0, &json!({ "erro": self.1 }))
    }
}

fn bad(error: impl std::fmt::Display) -> WebError {
    WebError(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
}

fn anyhow_bad(error: &anyhow::Error) -> WebError {
    WebError(StatusCode::UNPROCESSABLE_ENTITY, format!("{error:#}"))
}

type WebResult = Result<Response, Response>;

fn ok(body: &Value) -> WebResult {
    Ok(ui_json(StatusCode::OK, body))
}

fn fail(error: WebError) -> Response {
    axum::response::IntoResponse::into_response(error)
}

/// Autoriza e devolve o banco.
async fn enter<'a>(
    web: &'a Web,
    headers: &HeaderMap,
    method: &Method,
) -> Result<&'a Store, Response> {
    authorize_ui(&web.api_key, web.accounts.as_ref(), headers, method)
        .await
        .map_err(|response| *response)?;
    web.database
        .get()
        .map_err(|e| fail(WebError(StatusCode::SERVICE_UNAVAILABLE, e)))
}

/// As regras só mudam pela tela depois que o acervo as assume.
async fn require_owner(web: &Web, store: &Store) -> Result<(), Response> {
    let owner = crate::rules::owner(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    if owner == "acervo" {
        Ok(())
    } else {
        Err(fail(WebError(
            StatusCode::CONFLICT,
            "quem decide ainda é o Radarr: assuma em Configurações → Regras para editar aqui"
                .into(),
        )))
    }
}

pub fn router(web: Arc<Web>) -> Router {
    Router::new()
        .route("/ui/api/biblioteca/opcoes", get(options))
        .route("/ui/api/biblioteca/tmdb", get(tmdb_search))
        .route("/ui/api/biblioteca/filmes", post(add_movie))
        .route(
            "/ui/api/biblioteca/filmes/{id}",
            axum::routing::patch(edit_movie).delete(remove_movie),
        )
        .route(
            "/ui/api/biblioteca/filmes/{id}/releases",
            post(interactive_search),
        )
        .route("/ui/api/biblioteca/filmes/{id}/pegar", post(grab_release))
        .route(
            "/ui/api/biblioteca/filmes/{id}/arquivo",
            axum::routing::delete(delete_file),
        )
        .route(
            "/ui/api/biblioteca/filmes/{id}/historico",
            get(movie_history),
        )
        .route("/ui/api/biblioteca/fila", get(queue))
        .route(
            "/ui/api/biblioteca/fila/{id}",
            axum::routing::delete(remove_download),
        )
        .route("/ui/api/biblioteca/historico", get(history))
        .route("/ui/api/biblioteca/bloqueados", get(blocklist))
        .route(
            "/ui/api/biblioteca/bloqueados/{id}",
            axum::routing::delete(unblock),
        )
        .route(
            "/ui/api/biblioteca/exclusoes",
            get(exclusions).post(add_exclusion),
        )
        .route(
            "/ui/api/biblioteca/exclusoes/{tmdb}",
            axum::routing::delete(remove_exclusion),
        )
        .route("/ui/api/biblioteca/regras", get(rules).put(save_rules))
        .route("/ui/api/biblioteca/regras/assumir", post(take_over))
        .route("/ui/api/biblioteca/regras/devolver", post(give_back))
        .route(
            "/ui/api/biblioteca/perfis",
            get(profiles).post(create_profile),
        )
        .route(
            "/ui/api/biblioteca/perfis/{id}",
            put(update_profile).delete(delete_profile),
        )
        .route(
            "/ui/api/biblioteca/formatos",
            get(formats).post(create_format),
        )
        .route(
            "/ui/api/biblioteca/formatos/{id}",
            put(update_format).delete(delete_format),
        )
        .route("/ui/api/biblioteca/formatos/testar", post(test_formats))
        .route(
            "/ui/api/biblioteca/tamanhos",
            get(definitions).put(save_definitions),
        )
        .route(
            "/ui/api/biblioteca/notificacoes",
            get(notifications).put(save_notifications),
        )
        .route(
            "/ui/api/biblioteca/notificacoes/testar",
            post(test_notification),
        )
        .route("/ui/api/biblioteca/listas", get(lists).post(create_list))
        .route(
            "/ui/api/biblioteca/listas/{id}",
            put(update_list).delete(delete_list),
        )
        .route(
            "/ui/api/biblioteca/listas/{id}/sincronizar",
            post(sync_list),
        )
        .route("/ui/api/biblioteca/listas/{id}/previa", get(preview_list))
        .route("/ui/api/biblioteca/migrar", post(migrate))
        .with_state(web)
}

// ---------------------------------------------------------------- opções

async fn options(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (profiles, tags, owner) = tokio::try_join!(
        async { store.profile_ids().await.map_err(anyhow::Error::from) },
        async { store.tags().await.map_err(anyhow::Error::from) },
        crate::rules::owner(&web.config, store),
    )
    .map_err(|e| fail(anyhow_bad(&e)))?;
    let map = web.config.path_map();
    let roots = web.config.movies.root_folders.clone();
    let folders = tokio::task::spawn_blocking(move || {
        roots
            .into_iter()
            .map(|root| {
                let free = map
                    .to_host(std::path::Path::new(&root))
                    .ok()
                    .and_then(|host| acervo_fs::free_space(&host).ok());
                json!({ "caminho": root, "livre": free })
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    ok(&json!({
        "perfis": profiles.iter().map(|(id, name)| json!({ "id": id, "nome": name })).collect::<Vec<_>>(),
        "pastas": folders,
        "tags": tags.iter().map(|t| json!({ "id": t.id, "nome": t.label })).collect::<Vec<_>>(),
        "dono_das_regras": owner,
        "qualidades": Quality::ALL.iter().filter(|q| **q != Quality::Unknown)
            .map(|q| json!({ "id": q.id(), "nome": q.name() })).collect::<Vec<_>>(),
        "indexadores": web.catalog.views().into_iter().map(|v| v.name).collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------- filmes

#[derive(Deserialize)]
struct TmdbQuery {
    termo: String,
}

async fn tmdb_search(
    State(web): Shared,
    headers: HeaderMap,
    Query(q): Query<TmdbQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let tmdb = crate::metadata::require_tmdb(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let term = q.termo.trim();
    if term.is_empty() {
        return ok(&json!({ "resultados": [] }));
    }
    // "tmdb:123", "imdb:tt123" ou "Título 2020".
    let results = if let Some(id) = term
        .strip_prefix("tmdb:")
        .and_then(|i| i.trim().parse().ok())
    {
        match crate::library::lookup(&tmdb, id).await {
            Ok((movie, extras)) => vec![acervo_metadata::MovieSummary {
                tmdb_id: id,
                title: movie.title,
                original_title: movie.original_title.unwrap_or_default(),
                year: movie.year,
                overview: movie.overview,
                poster: extras
                    .poster
                    .map(|p| p.replacen("/t/p/original/", "/t/p/w342/", 1)),
                vote_average: 0.0,
                popularity: 0.0,
            }],
            Err(_) => Vec::new(),
        }
    } else if let Some(imdb) = term
        .strip_prefix("imdb:")
        .or_else(|| term.starts_with("tt").then_some(term))
    {
        match tmdb
            .find_imdb(imdb.trim())
            .await
            .map_err(|e| fail(bad(e)))?
        {
            Some(id) => match crate::library::lookup(&tmdb, id).await {
                Ok((movie, extras)) => vec![acervo_metadata::MovieSummary {
                    tmdb_id: id,
                    title: movie.title,
                    original_title: movie.original_title.unwrap_or_default(),
                    year: movie.year,
                    overview: movie.overview,
                    poster: extras
                        .poster
                        .map(|p| p.replacen("/t/p/original/", "/t/p/w342/", 1)),
                    vote_average: 0.0,
                    popularity: 0.0,
                }],
                Err(_) => Vec::new(),
            },
            None => Vec::new(),
        }
    } else {
        // Ano no fim afina a busca: "Duna 2021".
        let (title, year) = match term.rsplit_once(' ') {
            Some((title, year)) if year.len() == 4 && year.parse::<u16>().is_ok() => {
                (title, year.parse().ok())
            }
            _ => (term, None),
        };
        tmdb.search(title, year).await.map_err(|e| fail(bad(e)))?
    };
    let (movies, exclusions) =
        tokio::try_join!(store.movies(), store.exclusions()).map_err(|e| fail(bad(e)))?;
    let list: Vec<Value> = results
        .into_iter()
        .map(|r| {
            let existing = movies
                .iter()
                .find(|m| m.movie.tmdb_id == r.tmdb_id)
                .map(|m| m.id);
            json!({
                "tmdb": r.tmdb_id,
                "titulo": r.title,
                "titulo_original": r.original_title,
                "ano": r.year,
                "sinopse": r.overview,
                "poster": r.poster,
                "nota": r.vote_average,
                "no_catalogo": existing,
                "excluido": exclusions.iter().any(|e| e.tmdb_id == r.tmdb_id),
            })
        })
        .collect();
    ok(&json!({ "resultados": list }))
}

#[derive(Deserialize)]
struct AddBody {
    tmdb: u32,
    perfil: String,
    pasta: String,
    #[serde(default = "yes")]
    monitorado: bool,
    #[serde(default = "released")]
    disponibilidade_minima: String,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    buscar: bool,
}

const fn yes() -> bool {
    true
}

fn released() -> String {
    "released".into()
}

async fn add_movie(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<AddBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    if !web.config.movies.root_folders.contains(&body.pasta) {
        return Err(fail(bad(format!(
            "pasta raiz `{}` não configurada",
            body.pasta
        ))));
    }
    let tmdb = crate::metadata::require_tmdb(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let request = crate::library::AddRequest {
        tmdb_id: body.tmdb,
        quality_profile: body.perfil,
        root_folder: body.pasta,
        monitored: body.monitorado,
        minimum_availability: body.disponibilidade_minima,
        tags: body.tags,
    };
    let id = crate::library::add_anywhere(&web.config, store, &tmdb, &request, body.buscar)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    // Adicionado por engano como excluído: a pessoa quis, a exclusão sai.
    let _ = store.remove_exclusion(body.tmdb).await;
    let manager_decides = crate::rules::owner(&web.config, store).await.ok() == Some("radarr");
    if body.buscar && !manager_decides {
        let web = Arc::clone(&web);
        tokio::spawn(async move {
            if let Ok(store) = web.database.get()
                && let Err(error) =
                    crate::grab::grab(&web.config, store, &web.catalog, id, true).await
            {
                tracing::info!(filme = id, "busca ao adicionar: {error:#}");
            }
        });
    }
    ok(&json!({ "id": id }))
}

async fn edit_movie(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(change): axum::Json<crate::library::MovieEdit>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PATCH).await?;
    crate::library::edit(&web.config, store, id, &change)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    apagar_arquivos: bool,
    #[serde(default)]
    excluir: bool,
}

async fn remove_movie(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<RemoveQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    crate::library::remove(&web.config, store, id, q.apagar_arquivos, q.excluir)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

async fn delete_file(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    crate::library::delete_file(&web.config, store, id)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

fn age_hours(release: &acervo_indexers::Release) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    release
        .published
        .map(|at| (time::OffsetDateTime::now_utc() - at).whole_minutes() as f64 / 60.0)
}

/// Busca em todos os indexadores e mostra cada release com a decisão: o que
/// seria pego, e por que os outros não.
async fn interactive_search(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let decider = Decider::load(&web.config, store, &web.catalog)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let target = decider
        .target(id)
        .ok_or_else(|| fail(bad("filme fora do catálogo ou sem perfil de qualidade")))?;
    let releases = Decider::fetch(&web.catalog, target)
        .await
        .map_err(|e| fail(bad(format!("busca falhou: {e}"))))?;
    let outcome = decider.decide_releases(id, releases, Mode::UserInvoked);
    let list: Vec<Value> = outcome
        .decisions
        .iter()
        .map(|decision| {
            let release = &outcome.releases[decision.release];
            let other_movie = decision.movie.is_some_and(|m| m != id);
            json!({
                "guid": release.guid,
                "titulo": release.title,
                "indexador": release.indexer,
                "tamanho": release.size,
                "seeders": release.seeders,
                "leechers": release.leechers,
                "idade_horas": age_hours(release),
                "qualidade": decision.parsed.as_ref().map(|p| p.quality.quality.name()),
                "idiomas": decision.languages.iter().map(|l| l.name()).collect::<Vec<_>>(),
                "formatos": decision.formats.iter()
                    .filter_map(|f| decider.format_name(*f)).collect::<Vec<_>>(),
                "nota": decision.format_score,
                "aprovado": decision.approved(),
                "outro_filme": other_movie,
                "motivos": decision.rejections.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "info": release.info_url.as_ref().map(ToString::to_string),
            })
        })
        .collect();
    web.searches
        .lock()
        .await
        .insert(id, (Instant::now(), outcome.releases));
    ok(&json!({ "releases": list }))
}

#[derive(Deserialize)]
struct GrabBody {
    guid: String,
}

async fn grab_release(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<GrabBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let release = {
        let mut searches = web.searches.lock().await;
        searches.retain(|_, (at, _)| at.elapsed() < SEARCH_TTL);
        searches
            .get(&id)
            .and_then(|(_, releases)| releases.iter().find(|r| r.guid == body.guid).cloned())
    }
    .ok_or_else(|| fail(bad("a busca expirou; busque de novo")))?;
    crate::grab::send_chosen(&web.config, store, &web.catalog, id, &release)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true, "titulo": release.title }))
}

// ---------------------------------------------------------------- atividade

async fn queue(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (grabs, movies) =
        tokio::try_join!(store.grabs(), store.movies()).map_err(|e| fail(bad(e)))?;
    let downloading: Vec<_> = grabs
        .into_iter()
        .filter(|g| g.state == acervo_store::GrabState::Downloading)
        .collect();
    // Progresso do cliente, se ele responde.
    let torrents: HashMap<String, acervo_clients::TorrentInfo> = if downloading.is_empty() {
        HashMap::new()
    } else {
        match crate::grab::qbit(&web.config).await {
            Ok(client) => client
                .torrents()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|t| (t.hash.to_ascii_lowercase(), t))
                .collect(),
            Err(error) => {
                tracing::warn!("fila: qBittorrent inalcançável: {error:#}");
                HashMap::new()
            }
        }
    };
    let items: Vec<Value> = downloading
        .iter()
        .map(|grab| {
            let movie = movies.iter().find(|m| m.id == grab.movie_id);
            let torrent = torrents.get(&grab.hash);
            json!({
                "id": grab.id,
                "filme_id": grab.movie_id,
                "filme": movie.map(|m| crate::events::label(&m.movie.title, m.movie.year)),
                "poster": movie.and_then(|m| m.extras.poster.as_ref())
                    .map(|p| p.replacen("/t/p/original/", "/t/p/w342/", 1)),
                "release": grab.title,
                "indexador": grab.indexer,
                "qualidade": grab.quality.name(),
                "tamanho": grab.size,
                "pego_em": grab.grabbed_at,
                "mensagem": grab.message,
                "no_cliente": torrent.is_some(),
                "progresso": torrent.map(|t| t.progress),
                "estado_cliente": torrent.map(|t| t.state.clone()),
                "velocidade": torrent.map(|t| t.dlspeed),
                "restante_segundos": torrent.map(|t| t.eta).filter(|e| *e > 0 && *e < 8_640_000),
                "seeds": torrent.map(|t| t.num_seeds),
                "upgrade": grab.replaces.is_some(),
            })
        })
        .collect();
    ok(&json!({ "fila": items }))
}

#[derive(Deserialize)]
struct RemoveDownload {
    #[serde(default = "yes")]
    remover_do_cliente: bool,
    #[serde(default)]
    bloquear: bool,
    #[serde(default)]
    buscar: bool,
}

async fn remove_download(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<RemoveDownload>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    crate::grab::remove_download(
        &web.config,
        store,
        &web.catalog,
        id,
        q.remover_do_cliente,
        q.bloquear,
        q.buscar,
    )
    .await
    .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    evento: Option<String>,
    #[serde(default = "first_page")]
    pagina: i64,
    #[serde(default = "page_size")]
    tamanho: i64,
}

const fn first_page() -> i64 {
    1
}

const fn page_size() -> i64 {
    50
}

async fn history_page(store: &Store, movie: Option<i64>, q: &HistoryQuery) -> WebResult {
    let size = q.tamanho.clamp(1, 250);
    let page = store
        .history(
            movie,
            q.evento.as_deref().filter(|e| !e.is_empty()),
            size,
            (q.pagina.max(1) - 1) * size,
        )
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "total": page.total, "eventos": page.events }))
}

async fn history(
    State(web): Shared,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    history_page(store, None, &q).await
}

async fn movie_history(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    history_page(store, Some(id), &q).await
}

async fn blocklist(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (blocked, movies) =
        tokio::try_join!(store.blocklist(), store.movies()).map_err(|e| fail(bad(e)))?;
    let items: Vec<Value> = blocked
        .iter()
        .map(|b| {
            let movie = b.movie_id.and_then(|id| movies.iter().find(|m| m.id == id));
            let mut value = serde_json::to_value(b).unwrap_or_default();
            value["filme"] =
                json!(movie.map(|m| crate::events::label(&m.movie.title, m.movie.year)));
            value
        })
        .collect();
    ok(&json!({ "bloqueados": items }))
}

async fn unblock(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    store.unblock(id).await.map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

async fn exclusions(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let list = store.exclusions().await.map_err(|e| fail(bad(e)))?;
    ok(&json!({ "exclusoes": list }))
}

#[derive(Deserialize)]
struct ExclusionBody {
    tmdb: u32,
    titulo: String,
    #[serde(default)]
    ano: Option<u16>,
}

async fn add_exclusion(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<ExclusionBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    store
        .add_exclusions(&[acervo_store::Exclusion {
            tmdb_id: body.tmdb,
            title: body.titulo,
            year: body.ano,
        }])
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

async fn remove_exclusion(
    State(web): Shared,
    Path(tmdb): Path<u32>,
    headers: HeaderMap,
) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    store
        .remove_exclusion(tmdb)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

// ---------------------------------------------------------------- regras

async fn rules(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let owner = crate::rules::owner(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let rules = crate::rules::stored(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let has_manager = web
        .config
        .instances
        .iter()
        .any(|spec| matches!(spec.kind, crate::config::InstanceKind::Movie));
    ok(&json!({
        "dono": owner,
        "tem_radarr": has_manager,
        "regras": rules,
        "indexadores": web.catalog.views().into_iter().map(|v| v.name).collect::<Vec<_>>(),
    }))
}

async fn save_rules(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(rules): axum::Json<crate::rules::DecisionRules>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PUT).await?;
    require_owner(&web, store).await?;
    if !["preferir_e_atualizar", "nao_atualizar", "nao_preferir"].contains(&rules.propers.as_str())
    {
        return Err(fail(bad("valor de propers inválido")));
    }
    crate::rules::save(store, &rules)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

async fn take_over(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    crate::rules::take_over(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "dono": "acervo" }))
}

async fn give_back(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    crate::rules::give_back(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "dono": "radarr" }))
}

fn profile_json(id: i64, profile: &QualityProfile, used: usize) -> Value {
    json!({
        "id": id,
        "nome": profile.name,
        "upgrade": profile.upgrade_allowed,
        "corte": profile.cutoff,
        "idioma": profile.language,
        "itens": profile.items.iter().map(|item| json!({
            "nome": item.name,
            "qualidades": item.qualities.iter().map(|q| q.id()).collect::<Vec<_>>(),
            "permitido": item.allowed,
        })).collect::<Vec<_>>(),
        "nota_minima": profile.min_format_score,
        "nota_corte": profile.cutoff_format_score,
        "notas": profile.format_scores.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
        "em_uso": used,
        "do_radarr": profile.source_id.is_some(),
    })
}

async fn profiles(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (profiles, movies) =
        tokio::try_join!(store.profiles_by_id(), store.movies()).map_err(|e| fail(bad(e)))?;
    let list: Vec<Value> = profiles
        .iter()
        .map(|(id, p)| {
            let used = movies
                .iter()
                .filter(|m| m.movie.quality_profile.as_deref() == Some(&p.name))
                .count();
            profile_json(*id, p, used)
        })
        .collect();
    ok(&json!({ "perfis": list }))
}

#[derive(Deserialize)]
struct ProfileBody {
    nome: String,
    upgrade: bool,
    corte: Option<usize>,
    idioma: Option<String>,
    itens: Vec<ItemBody>,
    #[serde(default)]
    nota_minima: i32,
    #[serde(default)]
    nota_corte: i32,
    #[serde(default)]
    notas: BTreeMap<String, i32>,
}

#[derive(Deserialize)]
struct ItemBody {
    nome: String,
    qualidades: Vec<u8>,
    permitido: bool,
}

fn profile_from(body: ProfileBody, source_id: Option<i64>) -> Result<QualityProfile, WebError> {
    let name = body.nome.trim().to_owned();
    if name.is_empty() {
        return Err(bad("o perfil precisa de nome"));
    }
    let items: Vec<ProfileItem> = body
        .itens
        .into_iter()
        .map(|item| {
            Ok(ProfileItem {
                name: item.nome,
                qualities: item
                    .qualidades
                    .into_iter()
                    .map(|id| {
                        Quality::from_id(id)
                            .ok_or_else(|| bad(format!("qualidade {id} desconhecida")))
                    })
                    .collect::<Result<_, _>>()?,
                allowed: item.permitido,
            })
        })
        .collect::<Result<_, WebError>>()?;
    if !items.iter().any(|i| i.allowed) {
        return Err(bad("o perfil precisa aceitar ao menos uma qualidade"));
    }
    if let Some(cutoff) = body.corte
        && !items.get(cutoff).is_some_and(|i| i.allowed)
    {
        return Err(bad("o corte tem de ser uma qualidade aceita"));
    }
    if let Some(language) = &body.idioma
        && !matches!(language.as_str(), "Any" | "Original")
        && acervo_parser::Language::from_name(language).is_none()
    {
        return Err(bad(format!("idioma `{language}` desconhecido")));
    }
    Ok(QualityProfile {
        name,
        upgrade_allowed: body.upgrade,
        cutoff: body.corte,
        language: body.idioma,
        items,
        min_format_score: body.nota_minima,
        cutoff_format_score: body.nota_corte,
        source_id,
        format_scores: body
            .notas
            .into_iter()
            .filter(|(_, score)| *score != 0)
            .filter_map(|(id, score)| Some((id.parse().ok()?, score)))
            .collect(),
    })
}

async fn create_profile(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<ProfileBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    require_owner(&web, store).await?;
    let profile = profile_from(body, None).map_err(fail)?;
    let id = store
        .save_profile(None, &profile)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "id": id }))
}

async fn update_profile(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<ProfileBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PUT).await?;
    require_owner(&web, store).await?;
    let current = store
        .profiles_by_id()
        .await
        .map_err(|e| fail(bad(e)))?
        .into_iter()
        .find(|(pid, _)| *pid == id)
        .ok_or_else(|| {
            fail(WebError(
                StatusCode::NOT_FOUND,
                "perfil desconhecido".into(),
            ))
        })?;
    let profile = profile_from(body, current.1.source_id).map_err(fail)?;
    store
        .save_profile(Some(id), &profile)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "id": id }))
}

async fn delete_profile(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    require_owner(&web, store).await?;
    store.delete_profile(id).await.map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

async fn formats(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let list = store.custom_formats().await.map_err(|e| fail(bad(e)))?;
    ok(&json!({ "formatos": list.iter().map(|f| json!({
        "id": f.id,
        "nome": f.name,
        "especificacoes": f.specifications,
        "no_nome_do_arquivo": f.include_when_renaming,
    })).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct FormatBody {
    nome: String,
    especificacoes: Value,
    #[serde(default)]
    no_nome_do_arquivo: bool,
}

fn format_from(id: i64, body: FormatBody) -> Result<CustomFormat, WebError> {
    let name = body.nome.trim().to_owned();
    if name.is_empty() {
        return Err(bad("o formato precisa de nome"));
    }
    let specs: Vec<acervo_decision::FormatSpec> =
        serde_json::from_value(body.especificacoes.clone())
            .map_err(|e| bad(format!("especificações: {e}")))?;
    if specs.is_empty() {
        return Err(bad("o formato precisa de ao menos uma especificação"));
    }
    acervo_decision::CustomFormat::new(id, name.clone(), specs).map_err(bad)?;
    Ok(CustomFormat {
        id,
        name,
        specifications: body.especificacoes,
        include_when_renaming: body.no_nome_do_arquivo,
    })
}

async fn create_format(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<FormatBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    require_owner(&web, store).await?;
    let format = format_from(0, body).map_err(fail)?;
    let id = store
        .save_custom_format(&format)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "id": id }))
}

async fn update_format(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<FormatBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PUT).await?;
    require_owner(&web, store).await?;
    let format = format_from(id, body).map_err(fail)?;
    store
        .save_custom_format(&format)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "id": id }))
}

async fn delete_format(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    require_owner(&web, store).await?;
    store
        .delete_custom_format(id)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

#[derive(Deserialize)]
struct TestBody {
    titulo: String,
}

/// Que formatos casam com um nome de release — para montar formato sem
/// esperar um release de verdade.
async fn test_formats(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<TestBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let stored = store.custom_formats().await.map_err(|e| fail(bad(e)))?;
    let formats = crate::rules::compiled_formats(&stored);
    let parsed = acervo_parser::parse_movie_title(&body.titulo);
    let languages = parsed
        .as_ref()
        .map(|p| p.languages.clone())
        .unwrap_or_default();
    let quality = parsed.as_ref().map_or_else(
        || acervo_parser::parse_quality(&body.titulo).quality,
        |p| p.quality.quality,
    );
    let input = acervo_decision::FormatInput {
        title: &body.titulo,
        release_group: parsed.as_ref().and_then(|p| p.release_group.as_deref()),
        edition: parsed.as_ref().and_then(|p| p.edition.as_deref()),
        languages: &languages,
        original_language: acervo_parser::Language::English,
        quality,
        size: 0,
        flags: 0,
    };
    let matched = acervo_decision::matching(&formats, &input);
    ok(&json!({
        "qualidade": quality.name(),
        "idiomas": languages.iter().map(|l| l.name()).collect::<Vec<_>>(),
        "grupo": input.release_group,
        "formatos": formats.iter().filter(|f| matched.contains(&f.id))
            .map(|f| json!({ "id": f.id, "nome": f.name })).collect::<Vec<_>>(),
    }))
}

async fn definitions(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let list = store
        .quality_definitions()
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "tamanhos": list.iter().map(|d| json!({
        "qualidade": d.quality.id(),
        "nome": d.quality.name(),
        "minimo": d.min_size,
        "maximo": d.max_size,
        "preferido": d.preferred_size,
    })).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct DefinitionBody {
    qualidade: u8,
    minimo: Option<f64>,
    maximo: Option<f64>,
    preferido: Option<f64>,
}

async fn save_definitions(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Vec<DefinitionBody>>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PUT).await?;
    require_owner(&web, store).await?;
    let list: Vec<QualityDefinition> = body
        .into_iter()
        .map(|d| {
            let quality = Quality::from_id(d.qualidade)
                .ok_or_else(|| bad(format!("qualidade {} desconhecida", d.qualidade)))?;
            if let (Some(min), Some(max)) = (d.minimo, d.maximo)
                && max != 0.0
                && min > max
            {
                return Err(bad(format!("{}: mínimo acima do máximo", quality.name())));
            }
            Ok(QualityDefinition {
                quality,
                min_size: d.minimo,
                max_size: d.maximo,
                preferred_size: d.preferido,
            })
        })
        .collect::<Result<_, WebError>>()
        .map_err(fail)?;
    store
        .set_quality_definitions(&list)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

// ---------------------------------------------------------------- notificações

async fn notifications(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let gotify = crate::events::gotify(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "gotify": crate::events::public_view(gotify.as_ref()) }))
}

#[derive(Deserialize)]
struct GotifyBody {
    servidor: String,
    /// Ausente ou vazio mantém o token guardado.
    #[serde(default)]
    token: Option<String>,
    #[serde(default = "default_priority")]
    prioridade: u8,
    #[serde(default)]
    eventos: crate::events::NotifyOn,
    #[serde(default = "yes")]
    ligado: bool,
}

const fn default_priority() -> u8 {
    5
}

async fn merged_gotify(store: &Store, body: GotifyBody) -> Result<crate::events::Gotify, WebError> {
    let current = crate::events::gotify(store)
        .await
        .map_err(|e| anyhow_bad(&e))?;
    let token = body
        .token
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .or_else(|| current.map(|c| c.token))
        .ok_or_else(|| bad("informe o token do aplicativo no Gotify"))?;
    let server = body.servidor.trim().to_owned();
    url::Url::parse(&server).map_err(|_| bad("endereço do Gotify inválido"))?;
    Ok(crate::events::Gotify {
        servidor: server,
        token,
        prioridade: body.prioridade.min(10),
        eventos: body.eventos,
        ligado: body.ligado,
    })
}

async fn save_notifications(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Option<GotifyBody>>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PUT).await?;
    let value = match body {
        None => None,
        Some(body) => Some(
            serde_json::to_string(&merged_gotify(store, body).await.map_err(fail)?)
                .map_err(|e| fail(bad(e)))?,
        ),
    };
    store
        .set_setting(crate::events::NOTIFY_KEY, value.as_deref(), &now_rfc3339())
        .await
        .map_err(|e| fail(bad(e)))?;
    let gotify = crate::events::gotify(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "gotify": crate::events::public_view(gotify.as_ref()) }))
}

async fn test_notification(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<GotifyBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let gotify = merged_gotify(store, body).await.map_err(fail)?;
    gotify
        .send(
            "acervo-hub",
            "Teste de notificação: está funcionando.",
            None,
        )
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

// ---------------------------------------------------------------- listas

async fn lists(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let list = store.import_lists().await.map_err(|e| fail(bad(e)))?;
    ok(&json!({ "listas": list }))
}

#[derive(Deserialize)]
struct ListBody {
    nome: String,
    tipo: String,
    configuracao: Value,
    #[serde(default = "yes")]
    ligada: bool,
    #[serde(default = "yes")]
    monitorar: bool,
    #[serde(default)]
    buscar_ao_adicionar: bool,
    perfil: i64,
    pasta: String,
    #[serde(default = "released")]
    disponibilidade_minima: String,
    #[serde(default)]
    tags: Vec<i64>,
}

fn list_from(web: &Web, id: i64, body: ListBody) -> Result<ImportList, WebError> {
    if !["tmdb_person", "tmdb_collection", "tmdb_list"].contains(&body.tipo.as_str()) {
        return Err(bad(format!("tipo de lista `{}` desconhecido", body.tipo)));
    }
    if !web.config.movies.root_folders.contains(&body.pasta) {
        return Err(bad(format!("pasta raiz `{}` não configurada", body.pasta)));
    }
    if body.nome.trim().is_empty() {
        return Err(bad("a lista precisa de nome"));
    }
    Ok(ImportList {
        id,
        name: body.nome.trim().to_owned(),
        kind: body.tipo,
        settings: body.configuracao,
        enabled: body.ligada,
        monitor: body.monitorar,
        search_on_add: body.buscar_ao_adicionar,
        quality_profile_id: Some(body.perfil),
        root_folder: body.pasta,
        minimum_availability: body.disponibilidade_minima,
        tags: body.tags,
        last_sync: None,
        last_error: None,
    })
}

async fn create_list(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<ListBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let list = list_from(&web, 0, body).map_err(fail)?;
    let id = store
        .save_import_list(&list)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "id": id }))
}

async fn update_list(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<ListBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PUT).await?;
    let list = list_from(&web, id, body).map_err(fail)?;
    store
        .save_import_list(&list)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "id": id }))
}

async fn delete_list(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    store
        .delete_import_list(id)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true }))
}

async fn find_list(store: &Store, id: i64) -> Result<ImportList, Response> {
    store
        .import_lists()
        .await
        .map_err(|e| fail(bad(e)))?
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| fail(WebError(StatusCode::NOT_FOUND, "lista desconhecida".into())))
}

async fn sync_list(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let list = find_list(store, id).await?;
    let report = crate::lists::sync(&web.config, store, Some(&web.catalog), &list)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&serde_json::to_value(report).unwrap_or_default())
}

async fn preview_list(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let list = find_list(store, id).await?;
    let tmdb = crate::metadata::require_tmdb(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let found = crate::lists::movies(&tmdb, &list)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let (movies, exclusions) =
        tokio::try_join!(store.movies(), store.exclusions()).map_err(|e| fail(bad(e)))?;
    ok(&json!({ "filmes": found.iter().map(|m| json!({
        "tmdb": m.tmdb_id,
        "titulo": m.title,
        "ano": m.year,
        "poster": m.poster,
        "no_catalogo": movies.iter().any(|c| c.movie.tmdb_id == m.tmdb_id),
        "excluido": exclusions.iter().any(|e| e.tmdb_id == m.tmdb_id),
    })).collect::<Vec<_>>() }))
}

async fn migrate(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let report = crate::migrate::run(&web.config, store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&serde_json::to_value(report).unwrap_or_default())
}

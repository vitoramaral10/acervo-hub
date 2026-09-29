//! A parte da interface que faz do acervo um gerenciador: adicionar, editar
//! e remover filmes, busca interativa, fila e histórico, lista de bloqueio,
//! regras e notificações.
//!
//! Tudo sob `/ui/api/biblioteca/`, com a mesma entrada da tela (sessão ou
//! chave, e o cabeçalho anti-CSRF em ação que muda estado).

// Handler devolve a resposta de erro pronta; é o formato do axum, e caixa
// aqui só custaria alocação em todo erro.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use acervo_api::{Accounts, Catalog, authorize_ui, ui_json};
use acervo_decision::Mode;
use acervo_parser::Quality;
use acervo_store::Store;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::decide::{Decider, now_rfc3339};
use crate::serve::Database;

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
        .route("/ui/api/biblioteca/regras", get(rules).put(save_rules))
        .route(
            "/ui/api/biblioteca/notificacoes",
            get(notifications).put(save_notifications),
        )
        .route(
            "/ui/api/biblioteca/notificacoes/testar",
            post(test_notification),
        )
        .with_state(web)
}

// ---------------------------------------------------------------- opções

async fn options(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let tags = store.tags().await.map_err(|e| fail(bad(e)))?;
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
        "pastas": folders,
        "tags": tags.iter().map(|t| json!({ "id": t.id, "nome": t.label })).collect::<Vec<_>>(),
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
    let movies = store.movies().await.map_err(|e| fail(bad(e)))?;
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
            })
        })
        .collect();
    ok(&json!({ "resultados": list }))
}

#[derive(Deserialize)]
struct AddBody {
    tmdb: u32,
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
        quality_profile: None,
        root_folder: body.pasta,
        monitored: body.monitorado,
        minimum_availability: body.disponibilidade_minima,
        tags: body.tags,
    };
    let id = crate::library::add(store, &tmdb, &request)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    if body.buscar {
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
    crate::library::edit(store, id, &change)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    apagar_arquivos: bool,
}

async fn remove_movie(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<RemoveQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    crate::library::remove(&web.config, store, id, q.apagar_arquivos)
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
    let decider = Decider::load(store, &web.catalog)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let target = decider
        .target(id)
        .ok_or_else(|| fail(bad("filme fora do catálogo")))?;
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

// ---------------------------------------------------------------- regras

async fn rules(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let rules = crate::rules::stored(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({
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
    if !["preferir_e_atualizar", "nao_atualizar", "nao_preferir"].contains(&rules.propers.as_str())
    {
        return Err(fail(bad("valor de propers inválido")));
    }
    crate::rules::save(store, &rules)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
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

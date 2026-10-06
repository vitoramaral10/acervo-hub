//! Interface web: estado dos indexadores, teste e troca de credencial.
//!
//! Uma página estática com JS simples, servida pelo próprio binário, e uma
//! API JSON por baixo. A entrada é por usuário e senha: ela abre uma sessão
//! cujo token vai num cookie `HttpOnly` + `SameSite=Strict`. Script e
//! automação podem, em vez disso, mandar a chave de API do servidor em
//! `X-Api-Key`. Toda ação que muda estado exige o cabeçalho `X-Acervo`, que
//! um formulário de outra origem não consegue mandar.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;

use super::{AccountsBackend, Server, constant_time_eq};

const COOKIE: &str = "acervo_sessao";
const CSRF_HEADER: &str = "x-acervo";
const INDEX: &str = include_str!("ui/dist/index.html");
const STYLE: &str = include_str!("ui/dist/app.css");
const SCRIPT: &str = include_str!("ui/dist/app.js");
const ICON: &str = include_str!("ui/dist/icone.svg");

/// Impressão do conteúdo (FNV-1a de 64 bits): o mesmo conteúdo dá sempre a
/// mesma versão, sem relógio nem dependência. Fora de `const` porque o avaliador
/// de constantes engasga com 600 KB de JS.
fn fingerprint(content: &str) -> u64 {
    content.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// O `index.html` com JS e CSS apontando para `?v=<hash>`: o conteúdo novo
/// vira URL nova, então o navegador pode guardar os estáticos para sempre.
static VERSIONED_INDEX: LazyLock<String> = LazyLock::new(|| {
    let (script, style) = (fingerprint(SCRIPT), fingerprint(STYLE));
    INDEX
        .replace("\"/ui/app.js\"", &format!("\"/ui/app.js?v={script:016x}\""))
        .replace(
            "\"/ui/app.css\"",
            &format!("\"/ui/app.css?v={style:016x}\""),
        )
});

/// Catálogo com um aviso não bloqueante sobre a atualização remota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefinitionCatalog {
    #[serde(rename = "definicoes")]
    pub definitions: Vec<DefinitionView>,
    #[serde(rename = "aviso")]
    pub warning: Option<String>,
}

/// Uma definição do catálogo, como a tela a lista.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefinitionView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub language: String,
    pub private: bool,
    /// O executor roda esta definição.
    pub supported: bool,
    /// Por que não roda, quando não roda.
    pub reason: Option<String>,
    /// Já existe um indexador com este id.
    pub added: bool,
    /// Esquema de configuração, sem valores secretos.
    pub settings: Vec<SettingView>,
}

/// Um setting como a tela o vê. Segredo nunca carrega valor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingView {
    pub name: String,
    pub label: String,
    /// `text`, `password`, `checkbox` ou `select`.
    pub kind: &'static str,
    pub options: Vec<String>,
    pub secret: bool,
    pub is_set: bool,
    pub value: Option<String>,
}

pub(crate) fn routes() -> Router<Arc<Server>> {
    Router::new()
        .route(
            "/",
            get(|| async { page(VERSIONED_INDEX.clone(), "text/html; charset=utf-8") }),
        )
        .route(
            "/ui/app.css",
            get(|| async { immutable(STYLE, "text/css; charset=utf-8") }),
        )
        .route(
            "/ui/app.js",
            get(|| async { immutable(SCRIPT, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/ui/icone.svg",
            get(|| async { page(ICON, "image/svg+xml") }),
        )
        .route("/ui/api/entrar", post(login))
        .route("/ui/api/sair", post(logout))
        .route("/ui/api/sessao", get(session))
        .route("/ui/api/indexadores", get(indexers).post(add_indexer))
        .route(
            "/ui/api/indexadores/{nome}",
            axum::routing::delete(remove_indexer),
        )
        .route(
            "/ui/api/indexadores/{nome}/ativo",
            axum::routing::put(set_enabled),
        )
        .route("/ui/api/catalogo", get(definitions))
        .route(
            "/ui/api/catalogo/{definicao}/settings",
            get(definition_settings),
        )
        .route("/ui/api/tarefas", get(tasks))
        .route("/ui/api/tarefas/historico", get(task_history))
        .route("/ui/api/tarefas/{id}/rodar", post(run_task))
        .route("/ui/api/filmes", get(movies))
        .route(
            "/ui/api/filmes/buscar",
            get(missing_status).post(search_missing),
        )
        .route("/ui/api/filmes/{id}/pegar", post(grab_movie))
        .route(
            "/ui/api/configuracoes",
            get(configuration).put(save_configuration),
        )
        .route(
            "/ui/api/configuracoes/{secao}",
            get(config_section).put(save_config_section),
        )
        .route("/ui/api/indexadores/{nome}/testar", post(test))
        .route(
            "/ui/api/indexadores/{nome}/settings",
            get(settings).put(update_settings),
        )
}

/// Resposta que o navegador não guarda: o `index.html` (que carrega a versão
/// vigente dos estáticos) e o ícone, que não tem versão na URL.
fn page(body: impl IntoResponse, media_type: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    secure_headers(headers);
    response
}

/// JS e CSS: a URL leva o hash do conteúdo (`?v=`), então a resposta nunca
/// muda sob a mesma URL e pode ser guardada por um ano.
fn immutable(body: &'static str, media_type: &'static str) -> Response {
    let mut response = page(body, media_type);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

/// Cabeçalhos de toda resposta da interface. A CSP só aceita script servido
/// daqui — nada inline, nada de terceiros. Estilo inline é permitido porque
/// os componentes (Radix, sonner) o injetam em tempo de execução; estilo não
/// executa código. Pôster vem direto do TMDB.
fn secure_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https://image.tmdb.org; \
             connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// Erro da API da interface: status e uma mensagem para a tela.
struct UiError(StatusCode, String);

impl IntoResponse for UiError {
    fn into_response(self) -> Response {
        let mut response = (self.0, Json(json!({ "erro": self.1 }))).into_response();
        secure_headers(response.headers_mut());
        response
    }
}

fn ok(body: serde_json::Value) -> Response {
    let mut response = Json(body).into_response();
    secure_headers(response.headers_mut());
    response
}

fn unauthorized() -> UiError {
    UiError(
        StatusCode::UNAUTHORIZED,
        "sessão ausente ou expirada".into(),
    )
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, value)| *name == COOKIE && !value.is_empty())
        .map(|(_, value)| value)
}

fn missing_csrf() -> UiError {
    UiError(
        StatusCode::FORBIDDEN,
        "requisição sem o cabeçalho da interface".into(),
    )
}

fn accounts_down(error: &str) -> UiError {
    tracing::warn!(%error, "contas da interface inalcançáveis");
    UiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "o banco de contas não respondeu; tente de novo em instantes".into(),
    )
}

/// Quem fez a requisição: o usuário da sessão, ou `None` se veio com a chave
/// de API. Se a ação muda estado, exige também o cabeçalho anti-CSRF.
async fn guard(
    server: &Server,
    headers: &HeaderMap,
    method: &Method,
) -> Result<Option<String>, UiError> {
    check(
        &server.api_key.get(),
        server.accounts.as_ref(),
        headers,
        method,
    )
    .await
}

async fn check(
    api_key: &str,
    accounts: &AccountsBackend,
    headers: &HeaderMap,
    method: &Method,
) -> Result<Option<String>, UiError> {
    let user = if let Some(key) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    {
        if !constant_time_eq(key.as_bytes(), api_key.as_bytes()) {
            return Err(unauthorized());
        }
        None
    } else {
        let Some(token) = session_token(headers) else {
            return Err(unauthorized());
        };
        match accounts.session_user(token).await {
            Ok(Some(user)) => Some(user),
            Ok(None) => return Err(unauthorized()),
            Err(error) => return Err(accounts_down(&error)),
        }
    };
    if method != Method::GET && headers.get(CSRF_HEADER).is_none() {
        return Err(missing_csrf());
    }
    Ok(user)
}

/// A mesma entrada da tela, para as demais rotas do serviço: sessão
/// (com o cabeçalho anti-CSRF em ação que muda estado) ou chave de API. O
/// erro já é a resposta a devolver.
///
/// # Errors
///
/// Sem sessão válida nem chave certa, ou sem o cabeçalho da interface.
pub async fn authorize_ui(
    api_key: &str,
    accounts: &AccountsBackend,
    headers: &HeaderMap,
    method: &Method,
) -> Result<Option<String>, Box<Response>> {
    check(api_key, accounts, headers, method)
        .await
        .map_err(|error| Box::new(error.into_response()))
}

/// Resposta JSON da tela, com os cabeçalhos de segurança.
#[must_use]
pub fn ui_json(status: StatusCode, body: &serde_json::Value) -> Response {
    let mut response = (status, Json(body.clone())).into_response();
    secure_headers(response.headers_mut());
    response
}

fn secure_cookie(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        == Some("https")
}

#[derive(Deserialize)]
struct LoginBody {
    usuario: String,
    senha: String,
}

async fn login(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response, UiError> {
    if headers.get(CSRF_HEADER).is_none() {
        return Err(missing_csrf());
    }
    let accounts = &server.accounts;
    let token = match accounts.login(body.usuario.trim(), &body.senha).await {
        Ok(Some(token)) => token,
        Ok(None) => {
            // Atraso fixo, somado ao custo do argon2: tentativa às cegas fica
            // cara sem precisar de estado.
            tokio::time::sleep(Duration::from_millis(600)).await;
            return Err(UiError(
                StatusCode::UNAUTHORIZED,
                "usuário ou senha incorretos".into(),
            ));
        }
        Err(error) => return Err(accounts_down(&error)),
    };
    let secure = if secure_cookie(&headers) {
        "; Secure"
    } else {
        ""
    };
    let max_age = 60 * 60 * 24 * 30;
    let cookie =
        format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}");
    let mut response = ok(json!({ "ok": true, "usuario": body.usuario.trim() }));
    if let Ok(cookie) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

async fn logout(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    if headers.get(CSRF_HEADER).is_none() {
        return Err(missing_csrf());
    }
    if let Some(token) = session_token(&headers)
        && let Err(error) = server.accounts.logout(token).await
    {
        return Err(accounts_down(&error));
    }
    let mut response = ok(json!({ "ok": true }));
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("acervo_sessao=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    );
    Ok(response)
}

async fn session(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    let user = guard(&server, &headers, &Method::GET).await?;
    Ok(ok(json!({ "ok": true, "usuario": user })))
}

fn timestamp(value: Option<time::OffsetDateTime>) -> serde_json::Value {
    value
        .and_then(|value| value.format(&Rfc3339).ok())
        .map_or(serde_json::Value::Null, serde_json::Value::String)
}

fn indexer_json(server: &Server, view: &super::IndexerView) -> serde_json::Value {
    let caps = &view.capabilities;
    let admin = server.admin.as_ref();
    json!({
        "nome": view.name,
        "ativo": true,
        "origem": admin.origin(&view.name).unwrap_or("cadastro"),
        "privado": view.proxies_downloads,
        "editavel": admin.settings(&view.name).is_some(),
        "modos": {
            "busca": caps.general.available,
            "series": caps.tv.available,
            "filmes": caps.movie.available,
        },
        "categorias": caps.categories.iter().map(|category| json!({
            "id": category.id,
            "nome": category.name,
        })).collect::<Vec<_>>(),
        "saude": {
            "ultimo_sucesso": timestamp(view.health.last_success),
            "ultima_falha": timestamp(view.health.last_failure),
            "ultimo_erro": view.health.last_error,
            "falhas_seguidas": view.health.consecutive_failures,
            "resultados": view.health.last_results,
            "em_espera_ate": timestamp(view.health.waiting_until()),
        },
    })
}

async fn indexers(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let mut list: Vec<_> = server
        .catalog
        .views()
        .iter()
        .map(|view| indexer_json(&server, view))
        .collect();
    {
        let admin = &server.admin;
        for (name, origin) in admin.disabled() {
            list.push(json!({
                "nome": name,
                "ativo": false,
                "origem": origin,
                "privado": false,
                "editavel": admin.settings(&name).is_some(),
                "modos": { "busca": false, "series": false, "filmes": false },
                "categorias": [],
                "saude": {
                    "ultimo_sucesso": null, "ultima_falha": null, "ultimo_erro": null,
                    "falhas_seguidas": 0, "resultados": null, "em_espera_ate": null,
                },
            }));
        }
    }
    list.sort_by(|a, b| a["nome"].as_str().cmp(&b["nome"].as_str()));
    Ok(ok(json!({ "indexadores": list })))
}

#[derive(Deserialize)]
struct AddBody {
    definicao: String,
    #[serde(default)]
    settings: BTreeMap<String, String>,
}

async fn add_indexer(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<AddBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let entry = server
        .admin
        .add(&body.definicao, body.settings)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    let name = entry.indexer.name().to_owned();
    server
        .catalog
        .insert(entry)
        .map_err(|error| UiError(StatusCode::CONFLICT, error.to_string()))?;
    let result = test_result(server.catalog.test(&name).await);
    Ok(ok(json!({ "ok": true, "nome": name, "teste": result })))
}

async fn remove_indexer(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::DELETE).await?;
    server
        .admin
        .remove(&name)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    server.catalog.remove(&name);
    Ok(ok(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct EnabledBody {
    ativo: bool,
}

async fn set_enabled(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<EnabledBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::PUT).await?;
    let entry = server
        .admin
        .set_enabled(&name, body.ativo)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    if body.ativo {
        if let Some(entry) = entry {
            server
                .catalog
                .insert(entry)
                .map_err(|error| UiError(StatusCode::CONFLICT, error.to_string()))?;
        }
    } else {
        server.catalog.remove(&name);
    }
    Ok(ok(json!({ "ok": true })))
}

async fn definitions(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    Ok(ok(json!(
        server
            .admin
            .definitions()
            .await
            .map_err(|e| UiError(StatusCode::BAD_GATEWAY, e))?
    )))
}

async fn definition_settings(
    State(server): State<Arc<Server>>,
    Path(definition): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let views = server
        .admin
        .definition_settings(&definition)
        .ok_or_else(|| {
            UiError(
                StatusCode::NOT_FOUND,
                "definição desconhecida ou não suportada".into(),
            )
        })?;
    Ok(ok(json!({ "settings": views })))
}

async fn tasks(State(server): State<Arc<Server>>, headers: HeaderMap) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    Ok(ok(json!({ "tarefas": server.admin.tasks() })))
}

async fn task_history(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let history = server
        .admin
        .task_history()
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(json!({ "historico": history })))
}

async fn run_task(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let started = server
        .admin
        .run_task(&id)
        .ok_or_else(|| UiError(StatusCode::NOT_FOUND, "tarefa desconhecida".into()))?;
    Ok(ok(started))
}

async fn movies(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let list = server
        .admin
        .movies()
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(json!({ "filmes": list })))
}

async fn search_missing(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let started = server
        .admin
        .search_missing()
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(started))
}

async fn missing_status(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    Ok(ok(server.admin.missing_status()))
}

#[derive(Deserialize)]
struct GrabBody {
    /// Sem ele, só a prévia: o que pegaria.
    #[serde(default)]
    aplicar: bool,
}

async fn grab_movie(
    State(server): State<Arc<Server>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<GrabBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let report = server
        .admin
        .grab_movie(id, body.aplicar)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(report))
}

async fn configuration(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let body = server
        .admin
        .configuration()
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(body))
}

async fn save_configuration(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(values): Json<BTreeMap<String, Option<String>>>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::PUT).await?;
    let body = server
        .admin
        .save_configuration(values)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(body))
}

async fn config_section(
    State(server): State<Arc<Server>>,
    Path(section): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let body = server
        .admin
        .config_section(&section)
        .map_err(|error| UiError(StatusCode::NOT_FOUND, error))?;
    Ok(ok(body))
}

async fn save_config_section(
    State(server): State<Arc<Server>>,
    Path(section): Path<String>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::PUT).await?;
    let body = server
        .admin
        .save_config_section(&section, value)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(body))
}

fn test_result(outcome: Result<usize, String>) -> serde_json::Value {
    match outcome {
        Ok(results) => json!({ "ok": true, "resultados": results }),
        Err(error) => json!({ "ok": false, "erro": error }),
    }
}

async fn test(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    Ok(ok(test_result(server.catalog.test(&name).await)))
}

async fn settings(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let views = server.admin.settings(&name).ok_or_else(|| {
        UiError(
            StatusCode::NOT_FOUND,
            "indexador sem settings editáveis".into(),
        )
    })?;
    Ok(ok(json!({ "settings": views })))
}

async fn update_settings(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(values): Json<BTreeMap<String, String>>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::PUT).await?;
    let entry = server
        .admin
        .update(&name, values)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    // Indexador desativado não está no catálogo servido: a credencial fica
    // salva e vale quando ele for ativado. Não é erro.
    if server.catalog.replace(&name, entry).is_err() {
        return Ok(ok(json!({
            "ok": true,
            "teste": {
                "ok": false,
                "erro": "credencial salva; o indexador está desativado — ative-o para testar",
            },
        })));
    }
    // Testa na hora: credencial salva que não funciona precisa aparecer já.
    let result = test_result(server.catalog.test(&name).await);
    Ok(ok(json!({ "ok": true, "teste": result })))
}

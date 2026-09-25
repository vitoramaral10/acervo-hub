//! Interface web: estado dos indexadores, teste, troca de credencial e busca
//! manual.
//!
//! Uma página estática com JS simples, servida pelo próprio binário, e uma
//! API JSON por baixo. A entrada é por usuário e senha: ela abre uma sessão
//! cujo token vai num cookie `HttpOnly` + `SameSite=Strict`. Script e
//! automação podem, em vez disso, mandar a chave da superfície Torznab em
//! `X-Api-Key`. Toda ação que muda estado exige o cabeçalho `X-Acervo`, que
//! um formulário de outra origem não consegue mandar.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use acervo_indexers::{Release, SearchQuery};
use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;

use crate::{ALL, Entry, Server, TorznabError, constant_time_eq};

const COOKIE: &str = "acervo_sessao";
const CSRF_HEADER: &str = "x-acervo";
const INDEX: &str = include_str!("ui/dist/index.html");
const STYLE: &str = include_str!("ui/dist/app.css");
const SCRIPT: &str = include_str!("ui/dist/app.js");
const ICON: &str = include_str!("ui/dist/icone.svg");

/// O que a interface administra e só o binário sabe fazer: a configuração,
/// o catálogo de definições, os gerenciadores e o ciclo de limpeza.
///
/// Mensagens de erro vão para a tela e não podem conter valor de setting.
#[async_trait]
pub trait Admin: Send + Sync + std::fmt::Debug {
    /// Settings editáveis do indexador; `None` se ele não tem nenhum.
    fn settings(&self, indexer: &str) -> Option<Vec<SettingView>>;

    /// Monta o indexador com os valores novos e os persiste. Campo ausente
    /// mantém o valor atual — é assim que segredo não precisa voltar à tela.
    ///
    /// # Errors
    ///
    /// Setting desconhecido, valor inválido ou falha ao gravar.
    async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String>;

    /// De onde o indexador vem: `config` (arquivo, só leitura) ou `interface`.
    fn origin(&self, indexer: &str) -> Option<&'static str>;

    /// Indexadores desativados — fora do catálogo servido, mas ainda listados.
    fn disabled(&self) -> Vec<(String, &'static str)>;

    /// Todas as definições conhecidas, suportadas ou não.
    fn definitions(&self) -> Vec<DefinitionView>;

    /// Settings que uma definição pede para ser adicionada.
    fn definition_settings(&self, definition: &str) -> Option<Vec<SettingView>>;

    /// Adiciona um indexador a partir de uma definição do catálogo.
    ///
    /// # Errors
    ///
    /// Definição desconhecida ou não suportada, id já em uso, settings
    /// inválidos, falha ao gravar.
    async fn add(
        &self,
        definition: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String>;

    /// Remove um indexador adicionado pela interface.
    ///
    /// # Errors
    ///
    /// Indexador do `config.toml` (só leitura) ou falha ao gravar.
    async fn remove(&self, indexer: &str) -> Result<(), String>;

    /// Ativa ou desativa. Ativar devolve o indexador montado, para entrar no
    /// catálogo servido.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido ou falha ao montar ou gravar.
    async fn set_enabled(&self, indexer: &str, enabled: bool) -> Result<Option<Entry>, String>;

    /// Gerenciadores configurados (Sonarr, Radarr).
    fn apps(&self) -> serde_json::Value;

    /// Planeja e, com `apply`, executa o cadastro dos indexadores nos
    /// gerenciadores.
    ///
    /// # Errors
    ///
    /// Configuração incompleta.
    async fn sync(
        &self,
        indexers: Vec<(String, acervo_indexers::Capabilities)>,
        apply: bool,
    ) -> Result<serde_json::Value, String>;

    /// Resultado do último ciclo de limpeza, se houver.
    fn last_cycle(&self) -> Option<serde_json::Value>;

    /// Roda o ciclo de limpeza em simulação, agora.
    ///
    /// # Errors
    ///
    /// Configuração do ciclo ausente ou falha de leitura.
    async fn simulate_cycle(&self) -> Result<serde_json::Value, String>;

    /// O catálogo de filmes, com o estado de cada arquivo no disco.
    ///
    /// # Errors
    ///
    /// Catálogo ilegível.
    async fn movies(&self) -> Result<serde_json::Value, String>;

    /// Espelha os gerenciadores de filmes no catálogo; sem `apply`, só
    /// relata o que mudaria.
    ///
    /// # Errors
    ///
    /// Catálogo impossível de abrir.
    async fn import_movies(&self, apply: bool) -> Result<serde_json::Value, String>;

    /// Uma rodada de decisão em sombra: busca até `limit` filmes que faltam e
    /// grava o que pegaria, sem pegar nada.
    ///
    /// # Errors
    ///
    /// Catálogo vazio ou gerenciador de filmes inalcançável.
    async fn shadow(&self, limit: usize) -> Result<serde_json::Value, String>;

    /// Busca e decide um filme do catálogo; com `apply`, manda o escolhido
    /// ao cliente de download.
    ///
    /// # Errors
    ///
    /// Filme desconhecido ou que já tem arquivo, busca que falhou, cliente
    /// inalcançável.
    async fn grab_movie(&self, movie_id: i64, apply: bool) -> Result<serde_json::Value, String>;

    /// Os downloads do acervo; com `import`, importa agora os que terminaram.
    ///
    /// # Errors
    ///
    /// Banco ou cliente de download inalcançável.
    async fn downloads(&self, import: bool) -> Result<serde_json::Value, String>;

    /// As configurações guardadas pela tela. Segredo nunca volta: só se está
    /// definido.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn configuration(&self) -> Result<serde_json::Value, String>;

    /// Grava configurações. Chave ausente mantém o valor, `null` apaga, texto
    /// grava — depois de testar, quando dá para testar.
    ///
    /// # Errors
    ///
    /// Valor recusado no teste ou banco inalcançável.
    async fn save_configuration(
        &self,
        values: BTreeMap<String, Option<String>>,
    ) -> Result<serde_json::Value, String>;
}

/// Contas da interface: confere usuário e senha e guarda as sessões.
///
/// Mensagens de erro vão para o log, não para a tela.
#[async_trait]
pub trait Accounts: Send + Sync + std::fmt::Debug {
    /// Token de uma sessão nova, se usuário e senha batem.
    ///
    /// # Errors
    ///
    /// Banco inalcançável — senha errada é `Ok(None)`.
    async fn login(&self, user: &str, password: &str) -> Result<Option<String>, String>;

    /// Dono da sessão, se ela existe e não venceu.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn session_user(&self, token: &str) -> Result<Option<String>, String>;

    /// Encerra a sessão.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn logout(&self, token: &str) -> Result<(), String>;
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
            get(|| async { asset(INDEX, "text/html; charset=utf-8") }),
        )
        .route(
            "/ui/app.css",
            get(|| async { asset(STYLE, "text/css; charset=utf-8") }),
        )
        .route(
            "/ui/app.js",
            get(|| async { asset(SCRIPT, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/ui/icone.svg",
            get(|| async { asset(ICON, "image/svg+xml") }),
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
        .route("/ui/api/aplicativos", get(apps))
        .route("/ui/api/aplicativos/sincronizar", post(sync))
        .route("/ui/api/limpeza", get(last_cycle))
        .route("/ui/api/limpeza/simular", post(simulate_cycle))
        .route("/ui/api/filmes", get(movies))
        .route("/ui/api/filmes/importar", post(import_movies))
        .route("/ui/api/filmes/sombra", post(shadow))
        .route("/ui/api/filmes/{id}/pegar", post(grab_movie))
        .route("/ui/api/downloads", get(downloads))
        .route(
            "/ui/api/configuracoes",
            get(configuration).put(save_configuration),
        )
        .route("/ui/api/downloads/importar", post(import_downloads))
        .route("/ui/api/indexadores/{nome}/testar", post(test))
        .route(
            "/ui/api/indexadores/{nome}/settings",
            get(settings).put(update_settings),
        )
        .route("/ui/api/busca", get(search))
        .route("/ui/baixar", get(download))
}

fn asset(body: &'static str, media_type: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    secure_headers(headers);
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
    check(&server.api_key, server.accounts.as_ref(), headers, method).await
}

async fn check(
    api_key: &str,
    accounts: Option<&Arc<dyn Accounts>>,
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
        let (Some(token), Some(accounts)) = (session_token(headers), accounts) else {
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

/// A mesma entrada da tela, para rotas montadas fora deste crate: sessão
/// (com o cabeçalho anti-CSRF em ação que muda estado) ou chave de API. O
/// erro já é a resposta a devolver.
///
/// # Errors
///
/// Sem sessão válida nem chave certa, ou sem o cabeçalho da interface.
pub async fn authorize_ui(
    api_key: &str,
    accounts: Option<&Arc<dyn Accounts>>,
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
    let Some(accounts) = &server.accounts else {
        return Err(UiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "nenhum banco de contas configurado: defina [database] no config.toml".into(),
        ));
    };
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
    if let (Some(token), Some(accounts)) = (session_token(&headers), &server.accounts)
        && let Err(error) = accounts.logout(token).await
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

fn indexer_json(server: &Server, view: &crate::IndexerView) -> serde_json::Value {
    let caps = &view.capabilities;
    let admin = server.admin.as_ref();
    json!({
        "nome": view.name,
        "ativo": true,
        "origem": admin.and_then(|admin| admin.origin(&view.name)).unwrap_or("config"),
        "privado": view.proxies_downloads,
        "editavel": admin.and_then(|admin| admin.settings(&view.name)).is_some(),
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
    if let Some(admin) = &server.admin {
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
                    "falhas_seguidas": 0, "resultados": null,
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
    let entry = admin(&server)?
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
    admin(&server)?
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
    let entry = admin(&server)?
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
    Ok(ok(json!({ "definicoes": admin(&server)?.definitions() })))
}

async fn definition_settings(
    State(server): State<Arc<Server>>,
    Path(definition): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let views = admin(&server)?
        .definition_settings(&definition)
        .ok_or_else(|| {
            UiError(
                StatusCode::NOT_FOUND,
                "definição desconhecida ou não suportada".into(),
            )
        })?;
    Ok(ok(json!({ "settings": views })))
}

async fn apps(State(server): State<Arc<Server>>, headers: HeaderMap) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    Ok(ok(json!({ "aplicativos": admin(&server)?.apps() })))
}

#[derive(Deserialize)]
struct SyncBody {
    #[serde(default)]
    aplicar: bool,
}

async fn sync(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<SyncBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let indexers = server
        .catalog
        .views()
        .into_iter()
        .map(|view| (view.name, view.capabilities))
        .collect();
    let report = admin(&server)?
        .sync(indexers, body.aplicar)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(report))
}

async fn last_cycle(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    Ok(ok(json!({ "ultimo": admin(&server)?.last_cycle() })))
}

async fn simulate_cycle(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let report = admin(&server)?
        .simulate_cycle()
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(report))
}

async fn movies(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let list = admin(&server)?
        .movies()
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(json!({ "filmes": list })))
}

async fn import_movies(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<SyncBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let report = admin(&server)?
        .import_movies(body.aplicar)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(json!({ "instancias": report })))
}

#[derive(Deserialize)]
struct ShadowBody {
    #[serde(default = "default_shadow_limit")]
    limite: usize,
}

const fn default_shadow_limit() -> usize {
    5
}

async fn shadow(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(body): Json<ShadowBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let report = admin(&server)?
        .shadow(body.limite.clamp(1, 20))
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(json!({ "filmes": report })))
}

async fn grab_movie(
    State(server): State<Arc<Server>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<SyncBody>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let report = admin(&server)?
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
    let body = admin(&server)?
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
    let body = admin(&server)?
        .save_configuration(values)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(body))
}

async fn downloads(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let list = admin(&server)?
        .downloads(false)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(list))
}

async fn import_downloads(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::POST).await?;
    let list = admin(&server)?
        .downloads(true)
        .await
        .map_err(|error| UiError(StatusCode::UNPROCESSABLE_ENTITY, error))?;
    Ok(ok(list))
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

fn admin(server: &Server) -> Result<&Arc<dyn Admin>, UiError> {
    server.admin.as_ref().ok_or_else(|| {
        UiError(
            StatusCode::NOT_FOUND,
            "este serviço não permite editar indexadores".into(),
        )
    })
}

async fn settings(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let views = admin(&server)?.settings(&name).ok_or_else(|| {
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
    let entry = admin(&server)?
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

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    indexador: Option<String>,
    #[serde(default)]
    cat: Option<String>,
}

fn download_link(server: &Server, release: &Release) -> String {
    if release.download_url.scheme() == "magnet"
        || !server.catalog.proxies_downloads(&release.indexer)
    {
        return release.download_url.to_string();
    }
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("indexador", &release.indexer)
        .append_pair("link", release.download_url.as_str())
        .append_pair("nome", &release.title)
        .finish();
    format!("/ui/baixar?{query}")
}

async fn search(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(params): Query<SearchParams>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let categories: Vec<u32> = params
        .cat
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().parse())
        .collect::<Result<_, _>>()
        .map_err(|_| UiError(StatusCode::BAD_REQUEST, "categoria inválida".into()))?;
    let query = SearchQuery::general(params.q.trim())
        .with_categories(categories)
        .with_limit(crate::request::MAX_RESULTS);
    let target = params
        .indexador
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| ALL.to_owned());
    let page = server
        .catalog
        .search(&target, &query)
        .await
        .map_err(|error| match error {
            TorznabError::NoSuchIndexer => {
                UiError(StatusCode::NOT_FOUND, "indexador desconhecido".into())
            }
            other => UiError(StatusCode::BAD_GATEWAY, other.to_string()),
        })?;
    let results: Vec<_> = page
        .releases
        .iter()
        .map(|release| {
            json!({
                "titulo": release.title,
                "indexador": release.indexer,
                "tamanho": release.size,
                "seeders": release.seeders,
                "leechers": release.leechers,
                "downloads": release.grabs,
                "categorias": release.categories,
                "publicado": timestamp(release.published),
                "detalhes": release.info_url.as_ref().map(url::Url::as_str),
                "download": download_link(&server, release),
            })
        })
        .collect();
    let failures: Vec<_> = page
        .failures
        .iter()
        .map(|failure| json!({ "indexador": failure.indexer, "erro": failure.error }))
        .collect();
    Ok(ok(json!({ "resultados": results, "falhas": failures })))
}

#[derive(Deserialize)]
struct DownloadParams {
    indexador: String,
    link: String,
    #[serde(default)]
    nome: Option<String>,
}

async fn download(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(params): Query<DownloadParams>,
) -> Result<Response, UiError> {
    guard(&server, &headers, &Method::GET).await?;
    let link = url::Url::parse(&params.link)
        .map_err(|_| UiError(StatusCode::BAD_REQUEST, "link inválido".into()))?;
    let torrent = server
        .catalog
        .download(&params.indexador, &link)
        .await
        .map_err(|error| UiError(StatusCode::BAD_GATEWAY, error.to_string()))?;
    // Nome de arquivo só com caracteres seguros: vem do título do tracker.
    let base: String = params
        .nome
        .unwrap_or_else(|| params.indexador.clone())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '.'
            }
        })
        .take(150)
        .collect();
    let mut response = torrent.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-bittorrent"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{base}.torrent\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    secure_headers(headers);
    Ok(response)
}

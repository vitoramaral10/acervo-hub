//! Superfície HTTP do `acervo-hub`.
//!
//! Por ora, só a parte Torznab: os gerenciadores de série e de filme
//! cadastram `http://<host>/<indexador>/api` como fariam com o agregador de
//! indexadores atual, e `all` responde por todos de uma vez. A troca de um
//! pelo outro não exige mudar nada nos consumidores além da URL e da chave.

mod catalog;
mod render;
mod request;

use std::sync::Arc;

use acervo_indexers::Release;
use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use time::OffsetDateTime;

pub use catalog::{ALL, Catalog, CatalogError, Entry, Page};

/// Erros do contrato Torznab, com os códigos que os consumidores entendem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TorznabError {
    #[error("chave de API ausente ou incorreta")]
    IncorrectApiKey,
    #[error("parâmetro obrigatório ausente: {0}")]
    MissingParameter(&'static str),
    #[error("parâmetro com valor inválido: {0}")]
    IncorrectParameter(&'static str),
    #[error("função não suportada")]
    NoSuchFunction,
    #[error("indexador desconhecido")]
    NoSuchIndexer,
    #[error("todos os {0} indexadores consultados falharam")]
    AllFailed(usize),
    #[error("o indexador não entregou o arquivo")]
    DownloadFailed,
}

impl TorznabError {
    /// Código numérico da especificação Newznab/Torznab.
    #[must_use]
    pub const fn code(&self) -> u16 {
        match self {
            Self::IncorrectApiKey => 100,
            Self::MissingParameter(_) => 200,
            Self::IncorrectParameter(_) => 201,
            Self::NoSuchFunction => 202,
            Self::NoSuchIndexer => 300,
            Self::AllFailed(_) | Self::DownloadFailed => 900,
        }
    }

    const fn status(&self) -> StatusCode {
        match self {
            // Os consumidores leem o código do XML, mas um 401/404 de verdade
            // também deixa o erro legível em log de proxy e em `curl`.
            Self::IncorrectApiKey => StatusCode::UNAUTHORIZED,
            Self::NoSuchIndexer => StatusCode::NOT_FOUND,
            Self::MissingParameter(_) | Self::IncorrectParameter(_) | Self::NoSuchFunction => {
                StatusCode::BAD_REQUEST
            }
            Self::AllFailed(_) | Self::DownloadFailed => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for TorznabError {
    fn into_response(self) -> Response {
        xml(self.status(), "application/xml", render::error(&self))
    }
}

struct Server {
    catalog: Catalog,
    api_key: String,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Server")
            .field("catalog", &self.catalog)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Monta as rotas: `GET /{indexador}/api`, `GET /{indexador}/download` e
/// `GET /health`.
///
/// A chave vale para todos os indexadores e é aceita em `apikey` ou no
/// cabeçalho `X-Api-Key`, como os consumidores mandam.
pub fn router(catalog: Catalog, api_key: impl Into<String>) -> Router {
    let server = Arc::new(Server {
        catalog,
        api_key: api_key.into(),
    });
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/{indexer}/api", get(torznab))
        .route("/{indexer}/download", get(download))
        .with_state(server)
}

fn parse_query(raw: Option<String>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

/// A chave é conferida antes de qualquer outra coisa: sem ela, nem a
/// existência de um indexador pelo nome é revelada.
fn authorize(
    server: &Server,
    params: &[(String, String)],
    headers: &HeaderMap,
) -> Result<(), TorznabError> {
    let presented = params
        .iter()
        .find(|(key, _)| key == "apikey")
        .map(|(_, value)| value.as_str())
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .unwrap_or_default();
    if constant_time_eq(presented.as_bytes(), server.api_key.as_bytes()) {
        Ok(())
    } else {
        Err(TorznabError::IncorrectApiKey)
    }
}

/// Origem pela qual o consumidor nos alcançou, para montar links que ele
/// consiga seguir de volta.
fn public_origin(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|host| !host.is_empty() && !host.contains(['/', '?', '#', '@', ' ']))
        .unwrap_or("localhost");
    let scheme = match headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
    {
        Some("https") => "https",
        _ => "http",
    };
    format!("{scheme}://{host}")
}

async fn torznab(
    State(server): State<Arc<Server>>,
    Path(indexer): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, TorznabError> {
    let params = parse_query(raw);
    authorize(&server, &params, &headers)?;

    match request::parse(&params)? {
        request::Function::Caps => {
            let caps = server.catalog.capabilities(&indexer)?;
            Ok(xml(
                StatusCode::OK,
                "application/xml",
                render::capabilities(&caps),
            ))
        }
        request::Function::Search(query) => {
            let page = server.catalog.search(&indexer, &query).await?;
            let origin = public_origin(&headers);
            // Link de tracker privado só funciona com a sessão, que mora aqui:
            // o consumidor baixa por nós. Magnet e link público vão direto.
            let link = |release: &Release| {
                if release.download_url.scheme() == "magnet"
                    || !server.catalog.proxies_downloads(&release.indexer)
                {
                    return release.download_url.to_string();
                }
                let query: String = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("apikey", &server.api_key)
                    .append_pair("link", release.download_url.as_str())
                    .finish();
                format!("{origin}/{}/download?{query}", release.indexer)
            };
            Ok(xml(
                StatusCode::OK,
                "application/rss+xml",
                render::releases(&indexer, &page.releases, OffsetDateTime::now_utc(), &link),
            ))
        }
    }
}

async fn download(
    State(server): State<Arc<Server>>,
    Path(indexer): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, TorznabError> {
    let params = parse_query(raw);
    authorize(&server, &params, &headers)?;
    let link = params
        .iter()
        .find(|(key, _)| key == "link")
        .map(|(_, value)| value.as_str())
        .ok_or(TorznabError::MissingParameter("link"))?;
    let link = url::Url::parse(link).map_err(|_| TorznabError::IncorrectParameter("link"))?;
    let torrent = server.catalog.download(&indexer, &link).await?;
    let mut response = (StatusCode::OK, torrent).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-bittorrent"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{indexer}.torrent\""))
    {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

fn xml(status: StatusCode, media_type: &'static str, body: String) -> Response {
    let mut response = (status, body).into_response();
    let value = match media_type {
        "application/rss+xml" => HeaderValue::from_static("application/rss+xml; charset=utf-8"),
        _ => HeaderValue::from_static("application/xml; charset=utf-8"),
    };
    response.headers_mut().insert(header::CONTENT_TYPE, value);
    response
}

/// Comparação que não termina cedo no primeiro byte diferente.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() || right.is_empty() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

//! Superfície HTTP do `acervo-hub`: o catálogo de indexadores cadastrados e a
//! interface web por cima dele. Nada daqui é API para terceiros: quem fala
//! com ela é a própria tela, ou um script com a chave em `X-Api-Key`.

mod catalog;
mod ui;

use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

pub use catalog::{
    ALL, Catalog, CatalogError, Entry, Health, IndexerView, Page, QueryObserver, QueryRecord,
};
pub use ui::{Accounts, Admin, DefinitionView, SettingView, authorize_ui, ui_json};

/// A chave da interface, lida a cada requisição: trocada pela tela, a nova
/// vale na hora, sem reiniciar.
#[derive(Clone)]
pub struct ApiKey(Arc<dyn Fn() -> String + Send + Sync>);

impl ApiKey {
    /// Chave que muda: `current` é chamada a cada requisição.
    pub fn dynamic(current: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self(Arc::new(current))
    }

    /// A chave atual.
    #[must_use]
    pub fn get(&self) -> String {
        (self.0)()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey(<redacted>)")
    }
}

impl From<String> for ApiKey {
    fn from(key: String) -> Self {
        Self::dynamic(move || key.clone())
    }
}

impl From<&str> for ApiKey {
    fn from(key: &str) -> Self {
        key.to_owned().into()
    }
}

/// Erros de uma consulta ao catálogo de indexadores.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("indexador desconhecido")]
    NoSuchIndexer,
    #[error("todos os {0} indexadores consultados falharam")]
    AllFailed(usize),
    #[error("o indexador não entregou o arquivo")]
    DownloadFailed,
    #[error(
        "indexador `{indexer}` em espera até {until} UTC por excesso de requisições ou falhas seguidas"
    )]
    IndexerWaiting { indexer: String, until: String },
    #[error("todos os indexadores em espera até {until} UTC")]
    AllWaiting { until: String },
}

struct Server {
    catalog: Catalog,
    api_key: ApiKey,
    admin: Option<Arc<dyn Admin>>,
    accounts: Option<Arc<dyn Accounts>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Server")
            .field("catalog", &self.catalog)
            .field("api_key", &self.api_key)
            .field("admin", &self.admin)
            .field("accounts", &self.accounts)
            .finish()
    }
}

/// Monta as rotas: a interface, a API dela e `GET /health`.
pub fn router(catalog: Catalog, api_key: impl Into<ApiKey>) -> Router {
    router_with_admin(catalog, api_key, None, None)
}

/// Como [`router`], com a interface podendo reconfigurar indexadores por meio
/// de `admin`. Sem ele, a interface lista, testa e busca, mas não edita. Sem
/// `accounts`, ninguém entra pela tela: só vale a chave em `X-Api-Key`.
pub fn router_with_admin(
    catalog: Catalog,
    api_key: impl Into<ApiKey>,
    admin: Option<Arc<dyn Admin>>,
    accounts: Option<Arc<dyn Accounts>>,
) -> Router {
    let server = Arc::new(Server {
        catalog,
        api_key: api_key.into(),
        admin,
        accounts,
    });
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(ui::routes())
        .with_state(server)
}

/// `301` para o magnet: o navegador o entrega ao cliente de torrent.
pub(crate) fn magnet_redirect(magnet: &url::Url) -> Response {
    let mut response = StatusCode::MOVED_PERMANENTLY.into_response();
    if let Ok(location) = HeaderValue::from_str(magnet.as_str()) {
        response.headers_mut().insert(header::LOCATION, location);
    }
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

//! Superfície HTTP do `acervo-hub`: o catálogo de indexadores cadastrados e a
//! interface web por cima dele. Nada daqui é API para terceiros: quem fala
//! com ela é a própria tela, ou um script com a chave em `X-Api-Key`.

mod catalog;
#[cfg(test)]
mod tests;
mod ui;

#[cfg(test)]
pub use tests::support::{Accounts, Admin};
// Test builds substitute HTTP fixtures without a database.
#[cfg(test)]
type AdminBackend = dyn Admin;
#[cfg(not(test))]
type AdminBackend = crate::serve::HubAdmin;
#[cfg(test)]
type AccountsBackend = dyn Accounts;
#[cfg(not(test))]
type AccountsBackend = crate::serve::Database;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

pub use catalog::{ALL, Catalog, Entry, IndexerView};
#[cfg(test)]
pub use catalog::{CatalogError, Health};
pub use ui::{DefinitionCatalog, DefinitionView, SettingView, authorize_ui, ui_json};

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
    admin: Arc<AdminBackend>,
    accounts: Arc<AccountsBackend>,
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
pub fn router(
    catalog: Catalog,
    api_key: impl Into<ApiKey>,
    admin: Arc<AdminBackend>,
    accounts: Arc<AccountsBackend>,
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

//! Busca em indexadores Torznab.
//!
//! O crate separa três responsabilidades que costumam ficar misturadas no
//! agregador: o contrato HTTP/XML, o orçamento temporal de cada tracker e a
//! política de falha parcial ao consultar vários indexadores.

mod aggregate;
mod model;
mod rate;
mod torznab;

pub use aggregate::{AggregateSearch, Indexer, IndexerFailure, SearchReport};
pub use model::{
    Capabilities, Category, IndexerError, Release, SearchMode, SearchQuery, SearchSupport,
};
pub use rate::RateBudget;
pub use torznab::TorznabClient;

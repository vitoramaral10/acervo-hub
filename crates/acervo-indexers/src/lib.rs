//! Busca em indexadores Cardigann.
//!
//! O crate separa três responsabilidades que costumam ficar misturadas no
//! agregador: o executor das definições, o orçamento temporal de cada tracker e a
//! política de falha parcial ao consultar vários indexadores.

mod aggregate;
mod cardigann;
mod model;
mod network;
mod rate;

pub use aggregate::{AggregateSearch, Indexer, IndexerFailure, SearchReport};
pub use cardigann::{
    CardigannClient, CardigannDefinition, DefinitionHeader, SettingInfo, SettingInfoKind,
};
pub use model::{
    Capabilities, Category, IndexerError, Release, ResolvedDownload, SearchMode, SearchQuery,
    SearchSupport,
};
pub use network::{Challenge, FlareSolverr, Network, Proxy};
pub use rate::RateBudget;

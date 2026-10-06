//! Domínio do `acervo-hub`: obras, downloads, fila e tamanhos.
//!
//! Este crate é **puro**. Não abre socket, não lê disco, não conhece HTTP nem
//! banco. Toda decisão do serviço é escrita contra estes tipos e testada sem
//! subir nada — os adaptadores de IO ficam nos crates de fora.

pub mod download;
pub mod ids;
pub mod inventory;
pub mod size;

pub use download::{Download, DownloadState, FileFacts};
pub use ids::DownloadHash;
pub use inventory::{Inventory, UnreadableDownload};
pub use size::{Allocated, Apparent};

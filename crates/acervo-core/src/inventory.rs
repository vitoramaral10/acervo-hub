//! A fotografia de um ciclo: a fila do acervo e o que o cliente tem.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::download::Download;
use crate::ids::DownloadHash;
use crate::size::Allocated;

/// Um download cujos arquivos não puderam ser inspecionados no filesystem.
///
/// Sem `st_nlink` não há como saber se a biblioteca ainda aponta para aquele
/// inode, e "não sei" nunca autoriza remoção. O download fica **fora** de
/// [`Inventory::downloads`] — logo, fora de qualquer decisão — e aparece aqui
/// para ser relatado.
#[derive(Debug, Clone)]
pub struct UnreadableDownload {
    pub hash: DownloadHash,
    pub name: String,
    pub path: PathBuf,
    pub reason: String,
}

/// Tudo que um ciclo de reconciliação leu.
#[derive(Debug, Clone)]
pub struct Inventory {
    /// Hashes dos grabs internos em andamento; ficam fora da limpeza.
    pub queued_hashes: HashSet<DownloadHash>,
    pub downloads: Vec<Download>,
    pub unreadable: Vec<UnreadableDownload>,
    /// Espaço total ocupado pela biblioteca, para a trava proporcional.
    pub library_size: Allocated,
}

impl Inventory {
    #[must_use]
    pub fn new(library_size: Allocated) -> Self {
        Self {
            queued_hashes: HashSet::new(),
            downloads: Vec::new(),
            unreadable: Vec::new(),
            library_size,
        }
    }
}

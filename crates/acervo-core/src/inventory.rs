//! A fotografia de um ciclo: a fila do acervo e o que o cliente tem.

use std::path::PathBuf;

use crate::download::Download;
use crate::ids::{DownloadHash, InstanceName};
use crate::queue::{InstanceSnapshot, QueueItem};
use crate::size::Allocated;

/// Uma fila que não pôde ser lida.
///
/// Guardar a falha em vez de omitir a fila é deliberado: sem ela, todo
/// download em andamento pareceria sem dono. Quem decide precisa **ver** o
/// buraco para poder abortar.
#[derive(Debug, Clone)]
pub struct UnreachableInstance {
    pub instance: InstanceName,
    pub reason: String,
}

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
    pub snapshots: Vec<InstanceSnapshot>,
    pub unreachable: Vec<UnreachableInstance>,
    pub downloads: Vec<Download>,
    pub unreadable: Vec<UnreadableDownload>,
    /// Espaço total ocupado pela biblioteca, para a trava proporcional.
    pub library_size: Allocated,
}

impl Inventory {
    #[must_use]
    pub fn new(library_size: Allocated) -> Self {
        Self {
            snapshots: Vec::new(),
            unreachable: Vec::new(),
            downloads: Vec::new(),
            unreadable: Vec::new(),
            library_size,
        }
    }

    /// Todos os itens de fila.
    pub fn queue_items(&self) -> impl Iterator<Item = &QueueItem> {
        self.snapshots.iter().flat_map(|s| s.queue.iter())
    }

    /// Hashes presentes em alguma fila.
    ///
    /// Quem está em fila é download em andamento e fica fora da limpeza.
    #[must_use]
    pub fn hashes_in_any_queue(&self) -> Vec<&DownloadHash> {
        self.queue_items()
            .filter_map(|i| i.download.as_ref())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{QueueItemId, WorkId};

    fn item(hash: Option<&str>, work: Option<i64>) -> QueueItem {
        QueueItem {
            id: QueueItemId(1),
            instance: InstanceName::new("filmes"),
            title: "exemplo".into(),
            download: hash.map(DownloadHash::new),
            work: work.map(WorkId),
        }
    }

    #[test]
    fn fila_agrega_todas_as_leituras() {
        let mut inv = Inventory::new(Allocated::ZERO);
        inv.snapshots.push(InstanceSnapshot {
            instance: InstanceName::new("filmes"),
            queue: vec![item(Some("aa"), Some(1))],
            known_works: 1,
        });
        inv.snapshots.push(InstanceSnapshot {
            instance: InstanceName::new("series"),
            queue: vec![item(Some("bb"), None)],
            known_works: 1,
        });

        assert_eq!(inv.queue_items().count(), 2);
        assert_eq!(inv.hashes_in_any_queue().len(), 2);
        assert_eq!(item(None, Some(3)).work, Some(WorkId(3)));
    }
}

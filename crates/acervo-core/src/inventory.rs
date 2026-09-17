//! A fotografia de um ciclo: o que cada instância reportou e o que o cliente tem.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::download::Download;
use crate::ids::{DownloadHash, InstanceName};
use crate::queue::{InstanceSnapshot, QueueItem};
use crate::size::Allocated;

/// Uma instância que não respondeu.
///
/// Guardar a falha em vez de omitir a instância é deliberado: uma instância
/// ausente faz o acervo inteiro parecer órfão. Quem decide precisa **ver** o
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

    /// Todos os itens de fila, de todas as instâncias.
    pub fn queue_items(&self) -> impl Iterator<Item = &QueueItem> {
        self.snapshots.iter().flat_map(|s| s.queue.iter())
    }

    /// Índice hash → download, para cruzar fila e cliente.
    #[must_use]
    pub fn downloads_by_hash(&self) -> HashMap<&DownloadHash, &Download> {
        self.downloads.iter().map(|d| (&d.hash, d)).collect()
    }

    /// Hashes presentes em alguma fila.
    ///
    /// Sustenta a separação entre os dois lados da limpeza: quem está em fila é
    /// caso da reconciliação de fila; quem está em seeding fora de fila é caso
    /// da limpeza de download. Não há sobreposição, por construção.
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
            work: QueueItem::normalize_work(work),
        }
    }

    #[test]
    fn fila_agrega_todas_as_instancias() {
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
        assert_eq!(inv.queue_items().filter(|i| i.is_orphaned()).count(), 1);
        assert_eq!(item(None, Some(3)).work, Some(WorkId(3)));
    }
}

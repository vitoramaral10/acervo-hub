//! A fila do acervo: os downloads em andamento, cada um com a obra dona.

use crate::ids::{DownloadHash, InstanceName, QueueItemId, WorkId};

/// Um item na fila de download.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: QueueItemId,
    pub instance: InstanceName,
    pub title: String,
    /// O torrent correspondente no cliente.
    pub download: Option<DownloadHash>,
    /// A obra dona do item.
    pub work: Option<WorkId>,
}

/// A fila lida num ciclo.
#[derive(Debug, Clone)]
pub struct InstanceSnapshot {
    pub instance: InstanceName,
    pub queue: Vec<QueueItem>,
    /// Quantas obras a instância conhece. Zero com fila não vazia é sintoma de
    /// instância meio-viva, e aborta o ciclo — ver `acervo_janitor::guard`.
    pub known_works: usize,
}

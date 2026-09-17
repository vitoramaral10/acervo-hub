//! A fila de uma instância `*arr` e o que faz dela um órfão.

use crate::ids::{DownloadHash, InstanceName, QueueItemId, WorkId};

/// Um item na fila de download de uma instância.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: QueueItemId,
    pub instance: InstanceName,
    pub title: String,
    /// O torrent correspondente no cliente. `None` quando a instância reporta
    /// o item sem `downloadId`.
    pub download: Option<DownloadHash>,
    /// A obra dona do item.
    ///
    /// `None` é o discriminador de órfão: a série ou o filme foi apagado e o
    /// item da fila ficou pendurado. Nas APIs `*arr` isso chega como
    /// `seriesId`/`movieId` ausente ou zero — por isso o construtor trata
    /// `Some(0)` como `None`.
    pub work: Option<WorkId>,
}

impl QueueItem {
    /// Normaliza o id da obra: a API responde `0` para "sem pai", não `null`.
    #[must_use]
    pub fn normalize_work(raw: Option<i64>) -> Option<WorkId> {
        match raw {
            Some(0) | None => None,
            Some(id) => Some(WorkId(id)),
        }
    }

    /// Item de fila sem obra dona.
    ///
    /// Independe de o torrent estar pausado, travado ou baixando — era
    /// exatamente o caso que uma regra de "stalled" não pegava, porque item
    /// pausado nunca acumula strike por travamento.
    #[must_use]
    pub const fn is_orphaned(&self) -> bool {
        self.work.is_none()
    }
}

/// O inventário lido de uma instância `*arr` num ciclo.
#[derive(Debug, Clone)]
pub struct InstanceSnapshot {
    pub instance: InstanceName,
    pub queue: Vec<QueueItem>,
    /// Quantas obras a instância conhece. Zero com fila não vazia é sintoma de
    /// instância meio-viva, e aborta o ciclo — ver `acervo_janitor::guard`.
    pub known_works: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_e_ausencia_significam_a_mesma_coisa() {
        assert_eq!(QueueItem::normalize_work(Some(0)), None);
        assert_eq!(QueueItem::normalize_work(None), None);
        assert_eq!(QueueItem::normalize_work(Some(7)), Some(WorkId(7)));
    }
}

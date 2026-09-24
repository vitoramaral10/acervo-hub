//! O download no cliente de torrent, e os fatos de filesystem que decidem se
//! apagá-lo libera espaço.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::ids::DownloadHash;
use crate::size::{Allocated, Apparent};

/// Estado do torrent no cliente.
///
/// `Paused` é um estado próprio, e **não** é sinônimo de órfão: a maior parte
/// dos pausados é download desejado esperando liberar espaço. Órfão é o item
/// cuja obra não existe mais — ver [`crate::queue::QueueItem::is_orphaned`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadState {
    Downloading,
    Paused,
    Seeding,
    Errored,
    Unknown,
}

impl DownloadState {
    /// Só torrent em seeding entra na avaliação de limpeza. Download em
    /// andamento ou ainda não importado nunca corre risco.
    #[must_use]
    pub const fn is_seeding(self) -> bool {
        matches!(self, Self::Seeding)
    }
}

/// Fatos de `stat(2)` sobre um arquivo do download.
#[derive(Debug, Clone)]
pub struct FileFacts {
    pub path: PathBuf,
    /// `st_nlink`. Maior que 1 significa que a biblioteca compartilha o inode.
    pub links: u64,
    /// `st_blocks * 512`.
    pub allocated: Allocated,
    /// `st_size`, guardado só para exibição.
    pub apparent: Apparent,
    pub modified: SystemTime,
}

impl FileFacts {
    /// Se a biblioteca aponta para o mesmo inode, apagar o torrent libera zero.
    #[must_use]
    pub const fn is_shared(&self) -> bool {
        self.links > 1
    }
}

/// Um torrent no cliente de download.
#[derive(Debug, Clone)]
pub struct Download {
    pub hash: DownloadHash,
    pub name: String,
    pub state: DownloadState,
    /// Tracker privado. Muda a política: hit&run custa acesso ao tracker.
    pub private: bool,
    /// Categoria no cliente. É o que separa o que os *arr baixaram do que
    /// alguém baixou à mão — e só o primeiro é da conta da limpeza.
    pub category: String,
    pub ratio: f64,
    pub seeded_for: Duration,
    pub files: Vec<FileFacts>,
}

impl Download {
    /// Algum arquivo ainda é compartilhado com a biblioteca.
    ///
    /// Esta é a invariante central da limpeza: **só sai o que já saiu da
    /// biblioteca**. Ratio e tempo de seed são critério secundário, nunca
    /// gatilho.
    #[must_use]
    pub fn has_library_link(&self) -> bool {
        self.files.iter().any(FileFacts::is_shared)
    }

    /// Espaço que volta ao apagar: só os arquivos sem outro link.
    #[must_use]
    pub fn reclaimable(&self) -> Allocated {
        self.files
            .iter()
            .filter(|f| !f.is_shared())
            .map(|f| f.allocated)
            .sum()
    }

    /// O arquivo mexido mais recentemente. `None` se o download não tem arquivos.
    #[must_use]
    pub fn last_modified(&self) -> Option<SystemTime> {
        self.files.iter().map(|f| f.modified).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(links: u64, blocks: u64) -> FileFacts {
        FileFacts {
            path: PathBuf::from("/media/downloads/exemplo.mkv"),
            links,
            allocated: Allocated::from_blocks(blocks),
            apparent: Apparent::from_bytes(blocks * 512),
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    fn download(files: Vec<FileFacts>) -> Download {
        Download {
            hash: DownloadHash::new("abc"),
            name: "exemplo".into(),
            state: DownloadState::Seeding,
            private: true,
            category: "tv-sonarr".into(),
            ratio: 0.0,
            seeded_for: Duration::ZERO,
            files,
        }
    }

    #[test]
    fn seed_com_hardlink_na_biblioteca_nao_libera_nada() {
        // O caso medido em produção: 1743 GB de seeds, todos compartilhados.
        // Apagar qualquer um libera zero byte e só custa ratio.
        let d = download(vec![facts(2, 1000)]);
        assert!(d.has_library_link());
        assert_eq!(d.reclaimable(), Allocated::ZERO);
    }

    #[test]
    fn seed_sem_hardlink_libera_o_que_ocupa() {
        let d = download(vec![facts(1, 1000)]);
        assert!(!d.has_library_link());
        assert_eq!(d.reclaimable(), Allocated::from_blocks(1000));
    }

    #[test]
    fn download_misto_conta_so_a_parte_exclusiva() {
        let d = download(vec![facts(2, 1000), facts(1, 40)]);
        assert!(d.has_library_link());
        assert_eq!(d.reclaimable(), Allocated::from_blocks(40));
    }

    #[test]
    fn pausado_nao_e_seeding() {
        assert!(!DownloadState::Paused.is_seeding());
        assert!(DownloadState::Seeding.is_seeding());
    }
}

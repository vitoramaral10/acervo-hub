//! O plano de um ciclo: o que fazer, o que pular, e por quê.
//!
//! Nada aqui executa. O planejador devolve uma descrição; quem aplica é o
//! adaptador. Isso é o que permite `DryRun` ser o mesmo código do caminho real
//! em vez de um ramo paralelo que diverge com o tempo.

use std::fmt;

use acervo_core::{Allocated, DownloadHash, InstanceName, QueueItemId};

use crate::policy::Mode;

/// Uma ação a executar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Órfão confirmado neste ciclo, ainda abaixo do limite de strikes.
    StrikeOrphan {
        item: QueueItemId,
        instance: InstanceName,
        title: String,
        strikes: u32,
        limit: u32,
    },
    /// Órfão que bateu o limite: sai da fila e, conforme a política, do cliente.
    RemoveOrphan {
        item: QueueItemId,
        instance: InstanceName,
        title: String,
        download: Option<DownloadHash>,
        delete_files: bool,
    },
    /// Seed que perdeu o vínculo com a biblioteca.
    DeleteUnlinked {
        download: DownloadHash,
        name: String,
        reclaim: Allocated,
    },
}

/// Por que um candidato não virou ação.
///
/// Registrar o motivo é o que evita o diagnóstico às cegas: o sintoma chega
/// como "não apagou X", e sem o motivo não se sabe em qual etapa parou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Está em fila de alguma instância — caso da reconciliação de fila.
    InQueue,
    /// Categoria fora das gerenciadas: download manual.
    UnmanagedCategory,
    /// Ainda compartilha inode com a biblioteca: apagar libera zero.
    StillLinked,
    /// Só seed entra na avaliação.
    NotSeeding,
    /// Arquivo mexido dentro da janela de carência.
    RecentlyModified,
    /// Tracker privado dentro da carência de seed.
    SeedGrace,
    /// Órfão de fila cujo torrent não está no cliente.
    MissingInClient,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::InQueue => "está em fila de um *arr",
            Self::UnmanagedCategory => "categoria fora das gerenciadas (download manual)",
            Self::StillLinked => "ainda tem hardlink na biblioteca",
            Self::NotSeeding => "não está em seeding",
            Self::RecentlyModified => "arquivo mexido recentemente",
            Self::SeedGrace => "privado dentro da carência de seed",
            Self::MissingInClient => "não encontrado no cliente de download",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub what: String,
    pub reason: SkipReason,
}

/// O ciclo inteiro foi abortado. Nenhuma ação do plano vale.
#[derive(Debug, Clone, PartialEq)]
pub enum Abort {
    /// Uma instância não respondeu. Sem a fila dela, downloads que ela conhece
    /// pareceriam fora de fila — e seriam apagados.
    InstanceUnreachable {
        instance: InstanceName,
        reason: String,
    },
    /// Instância respondeu, mas diz não conhecer nenhuma obra. Meio-viva é pior
    /// que morta: responde rápido e mente.
    EmptyInventory { instance: InstanceName },
    /// O lote passou do teto absoluto.
    BatchTooLarge {
        reclaim: Allocated,
        limit: Allocated,
    },
    /// O lote passou do teto proporcional à biblioteca.
    BatchFractionTooLarge { fraction: f64, limit: f64 },
    /// A biblioteca mediu zero mas há remoção planejada.
    ///
    /// Biblioteca de tamanho zero desliga silenciosamente a trava proporcional
    /// — qualquer lote seria 0% dela. Quase sempre significa raiz não montada,
    /// que é exatamente quando tudo parece órfão.
    LibraryUnmeasured { reclaim: Allocated },
}

impl fmt::Display for Abort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstanceUnreachable { instance, reason } => {
                write!(f, "instância `{instance}` não respondeu: {reason}")
            }
            Self::EmptyInventory { instance } => {
                write!(f, "instância `{instance}` não reportou nenhuma obra")
            }
            Self::BatchTooLarge { reclaim, limit } => {
                write!(f, "lote de {reclaim} passa do teto de {limit}")
            }
            Self::BatchFractionTooLarge { fraction, limit } => write!(
                f,
                "lote é {:.1}% da biblioteca, teto {:.1}%",
                fraction * 100.0,
                limit * 100.0
            ),
            Self::LibraryUnmeasured { reclaim } => write!(
                f,
                "biblioteca mediu zero com {reclaim} a remover — raiz não montada?"
            ),
        }
    }
}

impl std::error::Error for Abort {}

/// O resultado de um ciclo bem-sucedido.
#[derive(Debug, Clone)]
pub struct Plan {
    pub mode: Mode,
    pub actions: Vec<Action>,
    pub skipped: Vec<Skipped>,
    /// Espaço que as ações liberam. Conta só arquivo sem outro link.
    pub reclaim: Allocated,
}

impl Plan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Ações que apagam de fato — as que merecem log em nível alto.
    pub fn destructive(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(|a| {
            matches!(
                a,
                Action::DeleteUnlinked { .. } | Action::RemoveOrphan { .. }
            )
        })
    }
}

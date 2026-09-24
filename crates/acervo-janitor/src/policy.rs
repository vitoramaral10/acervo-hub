//! Política de limpeza. O padrão é conservador e não apaga nada.

use std::time::Duration;

use acervo_core::Allocated;

/// Se o plano pode ser executado ou só relatado.
///
/// `DryRun` é o padrão do [`Default`] de propósito: toda regra nova se valida
/// em seco antes de ligar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    DryRun,
    Apply,
}

impl Mode {
    #[must_use]
    pub const fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}

/// Travas que abortam o ciclo inteiro sem apagar nada.
///
/// Cada uma existe por uma falha observada. Nenhuma é ajuste fino: se uma
/// dispara, a leitura do mundo está errada e nenhuma remoção daquele ciclo é
/// confiável — inclusive as que pareciam corretas.
#[derive(Debug, Clone)]
pub struct Guards {
    /// Download com arquivo mexido dentro desta janela nunca é apagado.
    pub recent_change_grace: Duration,
    /// Teto absoluto de espaço a liberar num ciclo.
    pub max_batch: Allocated,
    /// Teto proporcional ao tamanho da biblioteca, de 0.0 a 1.0.
    pub max_batch_fraction: f64,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            recent_change_grace: Duration::from_secs(24 * 60 * 60),
            max_batch: Allocated::from_bytes(300 * 1024 * 1024 * 1024),
            max_batch_fraction: 0.30,
        }
    }
}

/// A política completa de um ciclo.
#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: Mode,
    /// Quantas execuções consecutivas um item precisa aparecer como órfão
    /// antes de sair. Um strike por ciclo, sem janela de espera.
    pub orphan_strikes: u32,
    /// Apagar também os arquivos de órfão de tracker privado.
    pub delete_private_orphans: bool,
    /// Órfão de fila cujo torrent sumiu do cliente: pular em vez de agir.
    pub skip_orphan_if_missing_in_client: bool,
    /// Carência de seed para torrent privado que perdeu o vínculo com a
    /// biblioteca. `None` apaga assim que o vínculo cai — rápido, mas expõe a
    /// hit&run se o torrent for recente.
    pub private_seed_grace: Option<Duration>,
    /// Categorias do cliente cujos seeds a limpeza pode apagar por perda de
    /// hardlink. Fora delas é download manual e nunca é tocado. Vazia, a
    /// regra não apaga nada: o padrão seguro é não saber o que é de quem.
    pub managed_categories: Vec<String>,
    pub guards: Guards,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::DryRun,
            orphan_strikes: 3,
            delete_private_orphans: false,
            skip_orphan_if_missing_in_client: true,
            private_seed_grace: Some(Duration::from_secs(120 * 60 * 60)),
            managed_categories: Vec::new(),
            guards: Guards::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o_padrao_nao_apaga_nada() {
        let p = Policy::default();
        assert!(p.mode.is_dry_run());
        assert!(!p.delete_private_orphans);
    }
}

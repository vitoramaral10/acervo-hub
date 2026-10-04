//! Política de limpeza. O padrão é conservador: não apaga seed nem arquivo
//! de tracker privado sem que a configuração diga.

use std::time::Duration;

use acervo_core::Allocated;

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
    /// Quantas execuções consecutivas um download precisa aparecer sem dono
    /// antes de sair. Um strike por ciclo, sem janela de espera.
    pub orphan_strikes: u32,
    /// Apagar também os arquivos de download sem dono de tracker privado.
    pub delete_private_orphans: bool,
    /// Carência de seed para torrent privado que perdeu o vínculo com a
    /// biblioteca. `None` apaga assim que o vínculo cai — rápido, mas expõe a
    /// hit&run se o torrent for recente.
    ///
    /// É o teto: as duas condições abaixo antecipam a saída, nunca a atrasam.
    pub private_seed_grace: Option<Duration>,
    /// Ratio a partir do qual o seed privado sem vínculo já cumpriu o que o
    /// tracker espera. `None` desliga a condição.
    pub private_seed_ratio: Option<f64>,
    /// Tempo sem nenhuma transferência a partir do qual ninguém mais está
    /// baixando e o seed só prende disco. `None` desliga a condição.
    pub private_seed_idle: Option<Duration>,
    /// Categorias do cliente cujos seeds a limpeza pode apagar por perda de
    /// hardlink. Fora delas é download manual e nunca é tocado. Vazia, a
    /// regra não apaga nada: o padrão seguro é não saber o que é de quem.
    pub managed_categories: Vec<String>,
    pub guards: Guards,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            orphan_strikes: 3,
            delete_private_orphans: false,
            private_seed_grace: Some(Duration::from_secs(120 * 60 * 60)),
            private_seed_ratio: Some(1.0),
            private_seed_idle: Some(Duration::from_secs(24 * 60 * 60)),
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
        assert!(!p.delete_private_orphans);
        // Sem categoria gerenciada, a regra de hardlink perdido não apaga nada.
        assert!(p.managed_categories.is_empty());
    }
}

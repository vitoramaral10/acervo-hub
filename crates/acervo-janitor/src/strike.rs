//! Contagem de strikes: um download precisa aparecer sem dono várias
//! execuções seguidas antes de sair.
//!
//! O que a contagem protege não é o falso positivo lógico — é a leitura
//! instantânea errada. Uma leitura torta do cliente ou do catálogo marca um
//! strike, não apaga.

use std::collections::HashMap;

use acervo_core::DownloadHash;
use serde::{Deserialize, Serialize};

/// Chave estável de um download entre ciclos: o hash dele.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrikeKey(String);

impl StrikeKey {
    /// Chave de um download sem dono. O prefixo `download/` vem de quando
    /// havia também chave de item de fila; ficou para não zerar os strikes
    /// gravados.
    #[must_use]
    pub fn for_download(hash: &DownloadHash) -> Self {
        Self(format!("download/hash:{hash}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Strikes acumulados, entre execuções.
///
/// Serializa como um mapa simples de chave para contagem: o estado precisa
/// sobreviver ao reinício do processo, senão três strikes nunca se completam.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrikeLedger {
    counts: HashMap<StrikeKey, u32>,
}

impl StrikeLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marca um strike e devolve o total acumulado.
    pub fn strike(&mut self, key: StrikeKey) -> u32 {
        let entry = self.counts.entry(key).or_insert(0);
        *entry += 1;
        *entry
    }

    #[must_use]
    pub fn count(&self, key: &StrikeKey) -> u32 {
        self.counts.get(key).copied().unwrap_or(0)
    }

    /// Esquece tudo que não apareceu neste ciclo.
    ///
    /// Sem isso, um download que voltou a ter dono guardaria strikes antigos
    /// e seria apagado na primeira recaída.
    pub fn retain_only(&mut self, seen: &[StrikeKey]) {
        self.counts.retain(|k, _| seen.contains(k));
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.counts.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chave(hash: &str) -> StrikeKey {
        StrikeKey::for_download(&DownloadHash::new(hash))
    }

    #[test]
    fn strikes_acumulam_um_por_ciclo() {
        let mut l = StrikeLedger::new();
        let k = chave("aa");
        assert_eq!(l.strike(k.clone()), 1);
        assert_eq!(l.strike(k.clone()), 2);
        assert_eq!(l.count(&k), 2);
    }

    #[test]
    fn download_que_voltou_a_ter_dono_perde_os_strikes() {
        let mut l = StrikeLedger::new();
        let sumiu = chave("aa");
        let ficou = chave("bb");
        l.strike(sumiu.clone());
        l.strike(ficou.clone());

        l.retain_only(std::slice::from_ref(&ficou));

        assert_eq!(l.count(&sumiu), 0);
        assert_eq!(l.count(&ficou), 1);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn chave_e_o_hash_sem_caixa() {
        assert_eq!(chave("AA"), chave("aa"));
        assert_eq!(chave("aa").as_str(), "download/hash:aa");
    }
}

//! Contagem de strikes: um item precisa aparecer como órfão várias execuções
//! seguidas antes de sair.
//!
//! O que a contagem protege não é o falso positivo lógico — é a leitura
//! instantânea errada. Uma instância que responde uma vez com a fila torta
//! marca um strike, não apaga.

use std::collections::HashMap;

use acervo_core::QueueItem;

/// Chave estável de um item entre ciclos.
///
/// Prefere o hash do download ao id do item: ids de fila são reatribuídos
/// quando a instância reinicia, e um id reciclado herdaria strikes alheios.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StrikeKey(String);

impl StrikeKey {
    #[must_use]
    pub fn for_item(item: &QueueItem) -> Self {
        let tail = item
            .download
            .as_ref()
            .map_or_else(|| format!("titulo:{}", item.title), |h| format!("hash:{h}"));
        Self(format!("{}/{tail}", item.instance))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Strikes acumulados, entre execuções.
#[derive(Debug, Clone, Default)]
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
    /// Sem isso, um item que deixou de ser órfão — porque a obra voltou, ou
    /// porque a instância estava fora do ar — guardaria strikes antigos e
    /// seria apagado na primeira recaída.
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
    use acervo_core::{DownloadHash, InstanceName, QueueItemId};

    fn item(id: i64, hash: Option<&str>) -> QueueItem {
        QueueItem {
            id: QueueItemId(id),
            instance: InstanceName::new("filmes"),
            title: format!("titulo {id}"),
            download: hash.map(DownloadHash::new),
            work: None,
        }
    }

    #[test]
    fn strikes_acumulam_um_por_ciclo() {
        let mut l = StrikeLedger::new();
        let k = StrikeKey::for_item(&item(1, Some("aa")));
        assert_eq!(l.strike(k.clone()), 1);
        assert_eq!(l.strike(k.clone()), 2);
        assert_eq!(l.count(&k), 2);
    }

    #[test]
    fn id_reciclado_nao_herda_strike_de_outro_torrent() {
        let a = StrikeKey::for_item(&item(1, Some("aa")));
        let b = StrikeKey::for_item(&item(1, Some("bb")));
        assert_ne!(a, b);
    }

    #[test]
    fn item_que_deixou_de_ser_orfao_perde_os_strikes() {
        let mut l = StrikeLedger::new();
        let sumiu = StrikeKey::for_item(&item(1, Some("aa")));
        let ficou = StrikeKey::for_item(&item(2, Some("bb")));
        l.strike(sumiu.clone());
        l.strike(ficou.clone());

        l.retain_only(std::slice::from_ref(&ficou));

        assert_eq!(l.count(&sumiu), 0);
        assert_eq!(l.count(&ficou), 1);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn item_sem_hash_cai_no_titulo() {
        let k = StrikeKey::for_item(&item(1, None));
        assert!(k.as_str().contains("titulo:"));
    }
}

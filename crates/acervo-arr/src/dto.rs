//! Formato da resposta da API v3, no mínimo necessário.
//!
//! Todo campo além do `id` é opcional de propósito: a fila carrega dezenas de
//! campos que variam entre versões e entre produtos, e falhar a
//! desserialização inteira por causa de um deles transformaria uma diferença
//! cosmética em instância inalcançável — que aborta o ciclo.

use serde::Deserialize;

use crate::ArrKind;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueuePage {
    #[serde(default)]
    pub records: Vec<QueueRecord>,
    /// Ausente em algumas versões; por isso `Option` e não `0`. Um total
    /// implícito de zero encerraria a paginação na primeira página e truncaria
    /// a fila em silêncio.
    #[serde(default)]
    pub total_records: Option<usize>,
}

/// Um item de fila.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRecord {
    pub id: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// Hash do torrent no cliente.
    #[serde(default)]
    pub download_id: Option<String>,
    #[serde(default)]
    pub series_id: Option<i64>,
    #[serde(default)]
    pub movie_id: Option<i64>,
}

impl QueueRecord {
    /// O id da obra dona, conforme o tipo da instância.
    ///
    /// Lê só o campo do próprio tipo: aceitar o campo do outro produto faria um
    /// registro cruzado parecer ter dono e esconderia um órfão de verdade.
    #[must_use]
    pub const fn parent_id(&self, kind: ArrKind) -> Option<i64> {
        match kind {
            ArrKind::Series => self.series_id,
            ArrKind::Movie => self.movie_id,
        }
    }
}

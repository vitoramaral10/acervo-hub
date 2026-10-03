//! Formato das respostas da `WebUI` API v2, no mínimo necessário.

use std::time::Duration;

use serde::Deserialize;

/// Um torrent, como `torrents/info` o reporta.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentInfo {
    pub hash: String,
    pub name: String,
    pub state: String,
    pub save_path: String,
    #[serde(default)]
    pub ratio: f64,
    /// Segundos em seeding.
    #[serde(default)]
    pub seeding_time: i64,
    /// Só existe a partir do qBittorrent 5.0.
    #[serde(default)]
    pub private: Option<bool>,
    #[serde(default)]
    pub category: String,
    /// De 0.0 a 1.0.
    #[serde(default)]
    pub progress: f64,
    /// Bytes por segundo.
    #[serde(default)]
    pub dlspeed: u64,
    /// Segundos até terminar; 8640000 é "infinito" para o qBittorrent.
    #[serde(default)]
    pub eta: i64,
    #[serde(default)]
    pub size: u64,
    /// Bytes que faltam baixar.
    #[serde(default)]
    pub amount_left: u64,
    /// Seeds conectados agora.
    #[serde(default)]
    pub num_seeds: u32,
    /// Seeds no enxame, segundo o tracker; zero ou negativo é "não sei".
    #[serde(default)]
    pub num_complete: i64,
    /// Epoch da última transferência; zero é nunca.
    #[serde(default)]
    pub last_activity: i64,
    /// Segundos ativo (baixando ou semeando).
    #[serde(default)]
    pub time_active: i64,
    /// Separadas por vírgula e espaço.
    #[serde(default)]
    pub tags: String,
    /// Quando entrou no cliente, em segundos Unix; zero se não veio.
    #[serde(default)]
    pub added_on: i64,
}

impl TorrentInfo {
    /// Trata "não sei" como privado.
    ///
    /// A assimetria é deliberada: errar para privado custa um seed a mais no
    /// disco, errar para público custa hit&run e, no limite, o acesso ao
    /// tracker.
    #[must_use]
    pub fn is_private(&self) -> bool {
        self.private.unwrap_or(true)
    }

    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.split(',').any(|t| t.trim() == tag)
    }

    /// Tempo de seed, saturando negativo em zero.
    #[must_use]
    pub fn seeded_for(&self) -> Duration {
        Duration::from_secs(u64::try_from(self.seeding_time).unwrap_or(0))
    }

    /// Tempo desde a última transferência, dado o epoch de agora.
    ///
    /// `last_activity` zero é "nunca transferiu": o torrent está parado desde
    /// que completou, então a ociosidade é o tempo de seed. Atividade no futuro
    /// (relógio torto) satura em zero, nunca em "muito ocioso".
    #[must_use]
    pub fn idle_for(&self, now_epoch: i64) -> Duration {
        if self.last_activity <= 0 {
            return self.seeded_for();
        }
        Duration::from_secs(u64::try_from(now_epoch - self.last_activity).unwrap_or(0))
    }
}

/// Um arquivo do torrent. `name` é relativo ao `save_path` do torrent.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentFile {
    /// Posição no torrent: é por ela que a prioridade se muda.
    #[serde(default)]
    pub index: usize,
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// `0` não baixa; `1`, `6` e `7` baixam (normal, alta, máxima).
    #[serde(default = "normal_priority")]
    pub priority: u8,
    /// De 0 a 1.
    #[serde(default)]
    pub progress: f64,
}

fn normal_priority() -> u8 {
    1
}

/// O pedaço de `sync/maindata` que interessa: o estado do servidor.
#[derive(Debug, Deserialize)]
pub(crate) struct MainData {
    pub server_state: ServerState,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServerState {
    /// `-1` quando o qBittorrent não consegue medir (pasta de download
    /// inexistente), por isso com sinal.
    #[serde(default)]
    pub free_space_on_disk: i64,
}

/// O pedaço de `app/preferences` que o acervo lê.
#[derive(Debug, Deserialize)]
pub(crate) struct Preferences {
    #[serde(default)]
    pub preallocate_all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn espaco_livre_menos_um_desserializa() {
        let main: MainData =
            serde_json::from_str(r#"{"server_state": {"free_space_on_disk": -1}}"#).unwrap();
        assert_eq!(main.server_state.free_space_on_disk, -1);
    }
}

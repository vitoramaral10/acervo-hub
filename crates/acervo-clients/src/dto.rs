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
    #[serde(default)]
    pub num_seeds: u32,
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
    #[serde(default)]
    pub free_space_on_disk: u64,
}

/// O pedaço de `app/preferences` que o acervo lê.
#[derive(Debug, Deserialize)]
pub(crate) struct Preferences {
    #[serde(default)]
    pub preallocate_all: bool,
}

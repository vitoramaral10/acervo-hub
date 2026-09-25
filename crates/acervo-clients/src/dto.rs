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

    /// Tempo de seed, saturando negativo em zero.
    #[must_use]
    pub fn seeded_for(&self) -> Duration {
        Duration::from_secs(u64::try_from(self.seeding_time).unwrap_or(0))
    }
}

/// Um arquivo do torrent. `name` é relativo ao `save_path` do torrent.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentFile {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

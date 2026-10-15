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
    /// Posição na fila de downloads do cliente, 1 primeiro; zero ou negativo
    /// é fora dela (completo, ou fila do cliente desligada).
    #[serde(default)]
    pub priority: i64,
    /// O arquivo (torrent de um arquivo) ou a pasta raiz, como o cliente vê.
    /// Dois torrents com o mesmo caminho semeiam dos mesmos arquivos.
    #[serde(default)]
    pub content_path: String,
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

/// Um tracker do torrent, como `torrents/trackers` o reporta. DHT, `PeX` e
/// LSD vêm na mesma lista, com a URL `** [DHT] **` e afins.
#[derive(Debug, Clone, Deserialize)]
pub struct Tracker {
    pub url: String,
    /// 0 desligado (DHT, `PeX`, LSD), 1 ainda não contatado, 2 funcionando,
    /// 3 atualizando, 4 não funciona.
    #[serde(default)]
    pub status: i64,
    /// O que o tracker respondeu por último.
    #[serde(default)]
    pub msg: String,
}

impl Tracker {
    /// Tracker de verdade, não DHT, `PeX` nem LSD.
    #[must_use]
    pub fn is_real(&self) -> bool {
        !self.url.starts_with("** [")
    }
}

/// Em que pé está uma tarefa de criação de torrent (`torrentcreator/status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TorrentTaskState {
    Queued,
    Running,
    Finished,
    Failed,
    /// Estado que uma versão futura do cliente inventar: quem chama trata
    /// como "ainda não acabou".
    #[serde(other)]
    Unknown,
}

/// Estado de uma tarefa de criação de torrent.
///
/// O status do cliente também ecoa os `trackers` da tarefa (o announce com
/// passkey); de propósito, nada disso é lido aqui.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentTaskStatus {
    pub state: TorrentTaskState,
    /// De 0 a 100; o cliente só informa com a tarefa rodando.
    pub progress: u8,
    /// Só em tarefa que falhou.
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTaskStatus {
    pub status: TorrentTaskState,
    #[serde(default)]
    pub progress: i64,
    #[serde(default, rename = "errorMessage")]
    pub error_message: Option<String>,
}

impl From<RawTaskStatus> for TorrentTaskStatus {
    fn from(raw: RawTaskStatus) -> Self {
        Self {
            state: raw.status,
            progress: u8::try_from(raw.progress.clamp(0, 100)).unwrap_or(0),
            error: raw.error_message.filter(|m| !m.is_empty()),
        }
    }
}

/// Uma tarefa de criação na lista do cliente (`torrentcreator/status` sem
/// `taskID`): o bastante para achar e apagar a que ficou órfã. Como em
/// [`TorrentTaskStatus`], os `trackers` (com a passkey) não são lidos.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TorrentTaskEntry {
    #[serde(rename = "taskID")]
    pub task_id: String,
    /// O que a tarefa empacota.
    #[serde(default, rename = "sourcePath")]
    pub source_path: String,
    #[serde(rename = "status")]
    pub state: TorrentTaskState,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatedTask {
    #[serde(rename = "taskID")]
    pub task_id: String,
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
    #[serde(default)]
    pub queueing_enabled: bool,
    /// Downloads ativos ao mesmo tempo; negativo é sem limite.
    #[serde(default)]
    pub max_active_downloads: i64,
    /// Torrents ativos ao mesmo tempo, baixando ou semeando; negativo é sem
    /// limite.
    #[serde(default)]
    pub max_active_torrents: i64,
}

/// O announce que funciona: a primeira URL real (http, https ou udp; não
/// DHT, `PeX` nem LSD) com status 2. É a URL com passkey do tracker, por isso
/// quem chama a trata como segredo.
#[must_use]
pub fn working_announce(trackers: &[Tracker]) -> Option<String> {
    trackers
        .iter()
        .find(|t| t.status == 2 && t.is_real() && has_announce_scheme(&t.url))
        .map(|t| t.url.clone())
}

fn has_announce_scheme(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://", "udp://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(url: &str, status: i64) -> Tracker {
        Tracker {
            url: url.into(),
            status,
            msg: String::new(),
        }
    }

    #[test]
    fn announce_e_a_primeira_url_real_funcionando() {
        let trackers = [
            tracker("** [DHT] **", 2),
            tracker("https://morto.exemplo/announce?passkey=a", 4),
            tracker("https://espera.exemplo/announce?passkey=b", 1),
            tracker("HTTPS://bom.exemplo/announce?passkey=c", 2),
            tracker("udp://outro.exemplo:80/announce", 2),
        ];
        assert_eq!(
            working_announce(&trackers).as_deref(),
            Some("HTTPS://bom.exemplo/announce?passkey=c")
        );
    }

    #[test]
    fn announce_ignora_esquema_que_nao_e_de_tracker() {
        let trackers = [
            tracker("ws://x.exemplo/a", 2),
            tracker("udp://u.exemplo/a", 2),
        ];
        assert_eq!(
            working_announce(&trackers).as_deref(),
            Some("udp://u.exemplo/a")
        );
    }

    #[test]
    fn sem_tracker_funcionando_nao_ha_announce() {
        assert_eq!(working_announce(&[]), None);
        let trackers = [
            tracker("** [PeX] **", 2),
            tracker("** [LSD] **", 2),
            tracker("https://x.exemplo/a", 3),
        ];
        assert_eq!(working_announce(&trackers), None);
    }

    #[test]
    fn status_de_tarefa_desconhecido_nao_quebra_e_progresso_e_limitado() {
        let raw: RawTaskStatus =
            serde_json::from_str(r#"{"status":"Novo","progress":250,"trackers":["x"]}"#).unwrap();
        let status = TorrentTaskStatus::from(raw);
        assert_eq!(status.state, TorrentTaskState::Unknown);
        assert_eq!(status.progress, 100);
        assert_eq!(status.error, None);
    }

    #[test]
    fn espaco_livre_menos_um_desserializa() {
        let main: MainData =
            serde_json::from_str(r#"{"server_state": {"free_space_on_disk": -1}}"#).unwrap();
        assert_eq!(main.server_state.free_space_on_disk, -1);
    }
}

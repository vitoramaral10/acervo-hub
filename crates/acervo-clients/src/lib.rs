//! Cliente do qBittorrent (`WebUI` API v2) e, em [`jellyfin`], o mínimo do
//! Jellyfin.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use acervo_core::{DownloadHash, DownloadState};
use url::Url;

mod dto;
pub mod jellyfin;
mod torrent;

pub use dto::{
    TorrentFile, TorrentInfo, TorrentTaskEntry, TorrentTaskState, TorrentTaskStatus, Tracker,
    working_announce,
};
pub use torrent::{announce, info_hash, layout, magnet_hash, piece_digest};

#[derive(Debug, thiserror::Error)]
pub enum QbitError {
    #[error("url inválida: {0}")]
    BadUrl(#[from] url::ParseError),

    #[error("falha de transporte: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("qBittorrent respondeu {status} em `{path}`")]
    Status {
        status: reqwest::StatusCode,
        path: String,
    },

    #[error("login recusado: usuário ou senha do qBittorrent incorretos")]
    LoginRefused,

    #[error("o qBittorrent recusou o torrent (já existe ou arquivo inválido)")]
    AddRefused,

    #[error("o announce tem `|`, que o qBittorrent usaria para separar trackers")]
    InvalidAnnounce,

    #[error("o qBittorrent não aceitou criar o torrent (tarefas demais ativas)")]
    CreateRefused,

    #[error("o qBittorrent respondeu sem o `.torrent` da tarefa (não terminou ou falhou)")]
    TorrentFileUnavailable,

    #[error("o qBittorrent não sabe o espaço livre: a pasta de download existe?")]
    FreeSpaceUnknown,
}

/// O que mandar ao cliente: o arquivo `.torrent` ou um link magnet.
#[derive(Debug, Clone)]
pub enum NewTorrent {
    File(Vec<u8>),
    Magnet(String),
}

/// Serializa adicionar torrent e apagar com arquivos: a conferência de
/// conteúdo compartilhado de [`QbitClient::delete`] só vale se nenhum irmão
/// entrar entre ela e a remoção. Vale para o processo, que é quem fala com o
/// cliente.
static CONTENT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Onde e como o torrent novo entra.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub category: String,
    /// Pasta de download como o cliente a vê; sem ela, a da categoria.
    pub save_path: Option<String>,
    /// Entra parado, sem alocar nem baixar nada.
    pub stopped: bool,
    /// Entra andando só até ter os metadados, e para (`stopCondition`, da
    /// 4.5 em diante): o magnet de um pacote precisa da lista de arquivos
    /// antes de baixar qualquer um. Ignorado com `stopped`.
    pub stop_after_metadata: bool,
    pub tags: Vec<String>,
}

/// Formato do `.torrent` criado (`format`; só vale em build com libtorrent 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentFormat {
    V1,
    V2,
    Hybrid,
}

impl TorrentFormat {
    fn as_param(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
            Self::Hybrid => "hybrid",
        }
    }
}

/// O que pedir à criação de torrent (`torrentcreator/addTask`, qBittorrent 5).
///
/// Os padrões de [`CreateTorrentOptions::new`] são os do lançamento: v1,
/// peça automática, sem otimizar alinhamento (o layout não pode mudar),
/// privado e sem semear (o torrent novo não entra no cliente).
#[derive(Clone)]
pub struct CreateTorrentOptions {
    /// Arquivo ou pasta, como o **cliente** o vê.
    pub source_path: String,
    pub format: TorrentFormat,
    /// Em bytes; `0` é automático.
    pub piece_size: u32,
    pub optimize_alignment: bool,
    pub private: bool,
    pub start_seeding: bool,
    /// URLs de announce. **Segredo** (passkey): nunca vai a log nem a erro.
    pub trackers: Vec<String>,
    pub comment: Option<String>,
    /// Campo `source` do torrent. O qBittorrent 5.0.0 ignora o parâmetro
    /// `source` e copia o `comment` para ele; só versões que o leem usam
    /// este valor.
    pub source: Option<String>,
}

impl CreateTorrentOptions {
    #[must_use]
    pub fn new(source_path: impl Into<String>, trackers: Vec<String>) -> Self {
        Self {
            source_path: source_path.into(),
            format: TorrentFormat::V1,
            piece_size: 0,
            optimize_alignment: false,
            private: true,
            start_seeding: false,
            trackers,
            comment: None,
            source: None,
        }
    }
}

// À mão: o derive imprimiria o announce com a passkey.
impl std::fmt::Debug for CreateTorrentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateTorrentOptions")
            .field("source_path", &self.source_path)
            .field("format", &self.format)
            .field("piece_size", &self.piece_size)
            .field("optimize_alignment", &self.optimize_alignment)
            .field("private", &self.private)
            .field("start_seeding", &self.start_seeding)
            .field(
                "trackers",
                &format_args!("<{} redigido(s)>", self.trackers.len()),
            )
            .field("comment", &self.comment)
            .field("source", &self.source)
            .finish()
    }
}

/// Sessão autenticada no qBittorrent.
#[derive(Debug, Clone)]
pub struct QbitClient {
    base: Url,
    http: reqwest::Client,
    login: Arc<Login>,
}

/// Credencial guardada para refazer o login quando a sessão cai.
struct Login {
    username: String,
    password: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .field("password", &"<redigido>")
            .finish()
    }
}

impl QbitClient {
    /// Autentica e devolve a sessão.
    ///
    /// O cookie de sessão fica no `cookie_store` do cliente HTTP.
    ///
    /// # Errors
    ///
    /// Url malformada, falha de transporte ou credencial recusada.
    pub async fn login(
        base_url: &str,
        username: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<Self, QbitError> {
        let normalized = if base_url.ends_with('/') {
            base_url.to_string()
        } else {
            format!("{base_url}/")
        };
        let base = Url::parse(&normalized)?;

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .cookie_store(true)
            .build()?;

        let client = Self {
            base,
            http,
            login: Arc::new(Login {
                username: username.to_string(),
                password: password.to_string(),
            }),
        };
        client.authenticate().await?;
        Ok(client)
    }

    /// Abre uma sessão nova no `cookie_store`, trocando a anterior.
    async fn authenticate(&self) -> Result<(), QbitError> {
        let url = self.base.join("api/v2/auth/login")?;
        let response = self
            .http
            .post(url)
            // O Referer é exigido pela proteção de CSRF da WebUI; sem ele o
            // login responde 403 mesmo com credencial correta.
            .header(reqwest::header::REFERER, self.base.as_str())
            .form(&[
                ("username", self.login.username.as_str()),
                ("password", self.login.password.as_str()),
            ])
            .send()
            .await?;

        // Duas gerações da API convivem: até a 5.0, sucesso é 200 com corpo
        // "Ok." e credencial errada é 200 com "Fails."; da 5.1 em diante,
        // sucesso é 204 sem corpo e credencial errada é 401. Aceitar só a
        // forma antiga recusava o login certo de um cliente atualizado.
        match response.status() {
            reqwest::StatusCode::NO_CONTENT => {}
            reqwest::StatusCode::OK => {
                if response.text().await?.trim() != "Ok." {
                    return Err(QbitError::LoginRefused);
                }
            }
            reqwest::StatusCode::UNAUTHORIZED => return Err(QbitError::LoginRefused),
            status => {
                return Err(QbitError::Status {
                    status,
                    path: "api/v2/auth/login".into(),
                });
            }
        }

        Ok(())
    }

    /// Manda o pedido; se a sessão caiu (403), autentica de novo e repete uma
    /// vez. Desde a 5.3 o qBittorrent derruba as sessões mais antigas de quem
    /// loga muito, e uma sessão guardada pelo preparo de um lançamento morre
    /// no meio dele.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, QbitError> {
        // shortcut: o multipart (add) não se clona e não é repetido; mudar se
        // o add também cair no 403.
        let retry = request.try_clone();
        let response = request.send().await?;
        match retry {
            Some(retry) if response.status() == reqwest::StatusCode::FORBIDDEN => {
                self.authenticate().await?;
                Ok(retry.send().await?)
            }
            _ => Ok(response),
        }
    }

    /// Lista todos os torrents do cliente.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn torrents(&self) -> Result<Vec<TorrentInfo>, QbitError> {
        self.get("api/v2/torrents/info", &[]).await
    }

    /// Um torrent pelo hash, se o cliente o tem.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn torrent(&self, hash: &str) -> Result<Option<TorrentInfo>, QbitError> {
        let found: Vec<TorrentInfo> = self
            .get("api/v2/torrents/info", &[("hashes", hash)])
            .await?;
        Ok(found.into_iter().next())
    }

    /// O `.torrent` de um torrent do cliente (`torrents/export`, qBittorrent
    /// 4.5+). Leva o announce com a passkey: quem o usa não o loga.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx (404: torrent desconhecido ou
    /// cliente sem a rota; 409: sem metadados).
    pub async fn export_torrent(&self, hash: &str) -> Result<Vec<u8>, QbitError> {
        let path = "api/v2/torrents/export";
        let response = self
            .send(
                self.http
                    .get(self.base.join(path)?)
                    .query(&[("hash", hash)]),
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(QbitError::Status {
                status,
                path: path.into(),
            });
        }
        Ok(response.bytes().await?.to_vec())
    }

    /// Espaço livre, em bytes, no disco da pasta de download padrão.
    ///
    /// # Errors
    ///
    /// Falha de transporte, status não-2xx ou espaço desconhecido: o
    /// qBittorrent responde `-1` quando a pasta de download não existe.
    pub async fn free_space(&self) -> Result<u64, QbitError> {
        let main: dto::MainData = self.get("api/v2/sync/maindata", &[]).await?;
        u64::try_from(main.server_state.free_space_on_disk).map_err(|_| QbitError::FreeSpaceUnknown)
    }

    /// Se o cliente reserva o arquivo inteiro no disco ao iniciar.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn preallocates(&self) -> Result<bool, QbitError> {
        let preferences: dto::Preferences = self.get("api/v2/app/preferences", &[]).await?;
        Ok(preferences.preallocate_all)
    }

    /// Quantos downloads o próprio cliente deixa ativos, segurando o
    /// excesso na fila dele (`queuedDL`): o menor dos limites de downloads e
    /// de torrents ativos. `None` é fila desligada ou sem limite nenhum.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn download_slots(&self) -> Result<Option<usize>, QbitError> {
        let preferences: dto::Preferences = self.get("api/v2/app/preferences", &[]).await?;
        if !preferences.queueing_enabled {
            return Ok(None);
        }
        Ok([
            preferences.max_active_downloads,
            preferences.max_active_torrents,
        ]
        .into_iter()
        .filter_map(|n| usize::try_from(n).ok().filter(|n| *n > 0))
        .min())
    }

    /// Liga a pré-alocação: torrent iniciado já ocupa o tamanho inteiro.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn enable_preallocation(&self) -> Result<(), QbitError> {
        self.post(
            "api/v2/app/setPreferences",
            &[("json", r#"{"preallocate_all":true}"#)],
        )
        .await
    }

    /// Inicia torrents parados.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn start(&self, hashes: &[&str]) -> Result<(), QbitError> {
        // `resume` até a 4.x, `start` da 5.0 em diante.
        self.post_either("api/v2/torrents/start", "api/v2/torrents/resume", hashes)
            .await
    }

    /// Para torrents.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn stop(&self, hashes: &[&str]) -> Result<(), QbitError> {
        self.post_either("api/v2/torrents/stop", "api/v2/torrents/pause", hashes)
            .await
    }

    /// Manda o cliente verificar de novo os arquivos dos torrents (o "forçar
    /// verificação"): o que sumiu do disco deixa de contar como baixado, e o
    /// torrent volta a baixá-lo. Lista vazia não chama o cliente.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn recheck(&self, hashes: &[&str]) -> Result<(), QbitError> {
        if hashes.is_empty() {
            return Ok(());
        }
        self.post("api/v2/torrents/recheck", &[("hashes", &hashes.join("|"))])
            .await
    }

    /// Põe uma tag em torrents; a tag é criada se não existir.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn add_tag(&self, hashes: &[&str], tag: &str) -> Result<(), QbitError> {
        self.post(
            "api/v2/torrents/addTags",
            &[("hashes", &hashes.join("|")), ("tags", tag)],
        )
        .await
    }

    /// Tira uma tag de torrents.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn remove_tag(&self, hashes: &[&str], tag: &str) -> Result<(), QbitError> {
        self.post(
            "api/v2/torrents/removeTags",
            &[("hashes", &hashes.join("|")), ("tags", tag)],
        )
        .await
    }

    /// Cria a categoria; já existir não é erro.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status inesperado.
    pub async fn ensure_category(&self, name: &str) -> Result<(), QbitError> {
        let path = "api/v2/torrents/createCategory";
        let response = self
            .send(
                self.http
                    .post(self.base.join(path)?)
                    .header(reqwest::header::REFERER, self.base.as_str())
                    .form(&[("category", name), ("savePath", "")]),
            )
            .await?;
        // 409: a categoria já existe.
        match response.status() {
            status if status.is_success() => Ok(()),
            reqwest::StatusCode::CONFLICT => Ok(()),
            status => Err(QbitError::Status {
                status,
                path: path.into(),
            }),
        }
    }

    /// Adiciona um torrent. O cliente não devolve o hash: quem chama o tira
    /// de [`info_hash`] ou [`magnet_hash`] antes.
    ///
    /// # Errors
    ///
    /// Falha de transporte, status não-2xx ou torrent recusado.
    pub async fn add(&self, torrent: NewTorrent, options: &AddOptions) -> Result<(), QbitError> {
        let _guard = CONTENT_LOCK.lock().await;
        let path = "api/v2/torrents/add";
        let mut form = reqwest::multipart::Form::new().text("category", options.category.clone());
        if let Some(save_path) = &options.save_path {
            form = form.text("savepath", save_path.clone());
        }
        if options.stopped {
            // `paused` até a 4.x, `stopped` da 5.0 em diante; cada versão
            // ignora o campo da outra.
            form = form.text("paused", "true").text("stopped", "true");
        } else if options.stop_after_metadata {
            form = form.text("stopCondition", "MetadataReceived");
        }
        if !options.tags.is_empty() {
            form = form.text("tags", options.tags.join(","));
        }
        form = match torrent {
            NewTorrent::File(bytes) => form.part(
                "torrents",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name("acervo.torrent")
                    .mime_str("application/x-bittorrent")?,
            ),
            NewTorrent::Magnet(link) => form.text("urls", link),
        };
        let response = self
            .send(
                self.http
                    .post(self.base.join(path)?)
                    .header(reqwest::header::REFERER, self.base.as_str())
                    .multipart(form),
            )
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::CONFLICT {
            return Err(QbitError::AddRefused);
        }
        if !status.is_success() {
            return Err(QbitError::Status {
                status,
                path: path.into(),
            });
        }
        // Até a 5.0, recusa é 200 com "Fails.".
        if response.text().await?.trim() == "Fails." {
            return Err(QbitError::AddRefused);
        }
        Ok(())
    }

    /// Lista os arquivos de um torrent, com caminho relativo ao `save_path`.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn files(&self, hash: &DownloadHash) -> Result<Vec<TorrentFile>, QbitError> {
        self.get("api/v2/torrents/files", &[("hash", hash.as_str())])
            .await
    }

    /// Muda a prioridade de arquivos do torrent, pelo `index` de
    /// [`TorrentFile`]. Prioridade `0` faz o cliente não baixar o arquivo: é
    /// assim que de um pacote de temporada sai só o episódio que falta.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn set_file_priority(
        &self,
        hash: &DownloadHash,
        indexes: &[usize],
        priority: u8,
    ) -> Result<(), QbitError> {
        if indexes.is_empty() {
            return Ok(());
        }
        let ids = indexes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("|");
        self.post(
            "api/v2/torrents/filePrio",
            &[
                ("hash", hash.as_str()),
                ("id", &ids),
                ("priority", &priority.to_string()),
            ],
        )
        .await
    }

    /// Põe torrents no topo da fila do próprio cliente (`queuedDL` sai
    /// antes dos outros). Com a fila do cliente desligada (409), não há o que
    /// subir: não é erro.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status inesperado.
    pub async fn top_priority(&self, hashes: &[&str]) -> Result<(), QbitError> {
        if hashes.is_empty() {
            return Ok(());
        }
        let path = "api/v2/torrents/topPrio";
        let joined = hashes.join("|");
        match self
            .post_status(path, &[("hashes", joined.as_str())])
            .await?
        {
            status if status.is_success() => Ok(()),
            reqwest::StatusCode::CONFLICT => Ok(()),
            status => Err(QbitError::Status {
                status,
                path: path.into(),
            }),
        }
    }

    /// Alterna o download sequencial do torrent: é um toggle, não um
    /// "ligar". Quem chama lê `seq_dl` ([`TorrentInfo`]) antes e só alterna
    /// se estiver desligado.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn toggle_sequential(&self, hash: &str) -> Result<(), QbitError> {
        self.post(
            "api/v2/torrents/toggleSequentialDownload",
            &[("hashes", hash)],
        )
        .await
    }

    /// Os trackers de um torrent, com o estado e a última mensagem de cada.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn trackers(&self, hash: &str) -> Result<Vec<Tracker>, QbitError> {
        self.get("api/v2/torrents/trackers", &[("hash", hash)])
            .await
    }

    /// Acrescenta trackers a um torrent já no cliente (`torrents/addTrackers`).
    /// As URLs são segredo (levam a passkey): nenhum erro as carrega. Lista
    /// vazia não chama o cliente.
    ///
    /// # Errors
    ///
    /// URL com quebra de linha (separa as URLs no corpo), falha de transporte ou
    /// status não-2xx.
    pub async fn add_trackers(&self, hash: &str, urls: &[String]) -> Result<(), QbitError> {
        if urls.is_empty() {
            return Ok(());
        }
        if urls.iter().any(|u| u.contains(['\n', '\r'])) {
            return Err(QbitError::InvalidAnnounce);
        }
        self.post(
            "api/v2/torrents/addTrackers",
            &[("hash", hash), ("urls", &urls.join("\n"))],
        )
        .await
    }

    /// Cria uma tarefa de criação de `.torrent` no cliente e devolve o id.
    ///
    /// O cliente calcula os hashes (ele já vê os arquivos); quem chama
    /// acompanha com [`Self::torrent_task_status`], baixa com
    /// [`Self::torrent_task_file`] e apaga com [`Self::delete_torrent_task`].
    ///
    /// # Errors
    ///
    /// Announce com `|`; falha de transporte; 409 (tarefas demais) ou outro
    /// status não-2xx. Nenhum erro carrega o announce.
    pub async fn create_torrent_task(
        &self,
        options: &CreateTorrentOptions,
    ) -> Result<String, QbitError> {
        if options.trackers.iter().any(|t| t.contains('|')) {
            return Err(QbitError::InvalidAnnounce);
        }
        let path = "api/v2/torrentcreator/addTask";
        let mut form: Vec<(&str, String)> = vec![
            ("sourcePath", options.source_path.clone()),
            ("format", options.format.as_param().into()),
            ("pieceSize", options.piece_size.to_string()),
            ("optimizeAlignment", options.optimize_alignment.to_string()),
            ("private", options.private.to_string()),
            ("startSeeding", options.start_seeding.to_string()),
            ("trackers", options.trackers.join("|")),
        ];
        if let Some(comment) = &options.comment {
            form.push(("comment", comment.clone()));
        }
        if let Some(source) = &options.source {
            form.push(("source", source.clone()));
        }
        let response = self
            .send(
                self.http
                    .post(self.base.join(path)?)
                    .header(reqwest::header::REFERER, self.base.as_str())
                    .form(&form),
            )
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::CONFLICT {
            return Err(QbitError::CreateRefused);
        }
        if !status.is_success() {
            return Err(QbitError::Status {
                status,
                path: path.into(),
            });
        }
        let created: dto::CreatedTask = response.json().await?;
        Ok(created.task_id)
    }

    /// Estado de uma tarefa de criação; `None` se o cliente não a conhece
    /// (404: apagada, ou o cliente reiniciou).
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status inesperado.
    pub async fn torrent_task_status(
        &self,
        task_id: &str,
    ) -> Result<Option<TorrentTaskStatus>, QbitError> {
        let path = "api/v2/torrentcreator/status";
        let response = self
            .send(
                self.http
                    .get(self.base.join(path)?)
                    .query(&[("taskID", task_id)]),
            )
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(QbitError::Status {
                status,
                path: path.into(),
            });
        }
        let tasks: Vec<dto::RawTaskStatus> = response.json().await?;
        Ok(tasks.into_iter().next().map(TorrentTaskStatus::from))
    }

    /// Todas as tarefas de criação que o cliente tem, para achar a que ficou
    /// órfã (o acervo caiu no meio e nunca a apagou).
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn torrent_tasks(&self) -> Result<Vec<TorrentTaskEntry>, QbitError> {
        self.get("api/v2/torrentcreator/status", &[]).await
    }

    /// Os bytes do `.torrent` de uma tarefa terminada.
    ///
    /// # Errors
    ///
    /// Falha de transporte; 404 ou 409 (tarefa desconhecida, não terminada
    /// ou falha) viram [`QbitError::TorrentFileUnavailable`]; outro status
    /// não-2xx.
    pub async fn torrent_task_file(&self, task_id: &str) -> Result<Vec<u8>, QbitError> {
        let path = "api/v2/torrentcreator/torrentFile";
        let response = self
            .send(
                self.http
                    .get(self.base.join(path)?)
                    .query(&[("taskID", task_id)]),
            )
            .await?;
        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::CONFLICT
        ) {
            return Err(QbitError::TorrentFileUnavailable);
        }
        if !status.is_success() {
            return Err(QbitError::Status {
                status,
                path: path.into(),
            });
        }
        Ok(response.bytes().await?.to_vec())
    }

    /// Apaga a tarefa de criação (e libera a vaga dela no cliente). Tarefa
    /// que já não existe (404) não é erro: a limpeza roda em qualquer
    /// desfecho e precisa ser repetível.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status inesperado.
    pub async fn delete_torrent_task(&self, task_id: &str) -> Result<(), QbitError> {
        let path = "api/v2/torrentcreator/deleteTask";
        match self.post_status(path, &[("taskID", task_id)]).await? {
            status if status.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Ok(()),
            status => Err(QbitError::Status {
                status,
                path: path.into(),
            }),
        }
    }

    /// Apaga torrents, opcionalmente com os arquivos.
    ///
    /// Com os arquivos, o torrent cujo conteúdo outro torrent do cliente
    /// também usa (cross-seed: o mesmo arquivo semeando para dois trackers)
    /// sai **sem** eles: apagar levaria o dado do outro junto, e ele passaria
    /// a baixar tudo de novo ou viraria hit&run. O último a sair leva os
    /// arquivos.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx; com os arquivos, também a
    /// lista de torrents ilegível (sem ela, nada é apagado).
    pub async fn delete(
        &self,
        hashes: &[DownloadHash],
        delete_files: bool,
    ) -> Result<(), QbitError> {
        if hashes.is_empty() || !delete_files {
            return self.delete_raw(hashes, delete_files).await;
        }
        // A leitura e a remoção ficam juntas: um irmão adicionado entre as
        // duas perderia os arquivos.
        let _guard = CONTENT_LOCK.lock().await;
        let shared = shared_content(hashes, &self.torrents().await?);
        let (keep, free): (Vec<DownloadHash>, Vec<DownloadHash>) = hashes
            .iter()
            .cloned()
            .partition(|h| shared.contains(&h.as_str().to_ascii_lowercase()));
        self.delete_raw(&free, true).await?;
        self.delete_raw(&keep, false).await
    }

    async fn delete_raw(
        &self,
        hashes: &[DownloadHash],
        delete_files: bool,
    ) -> Result<(), QbitError> {
        if hashes.is_empty() {
            return Ok(());
        }

        let joined = hashes
            .iter()
            .map(DownloadHash::as_str)
            .collect::<Vec<_>>()
            .join("|");
        let url = self.base.join("api/v2/torrents/delete")?;

        let response = self
            .send(
                self.http
                    .post(url)
                    .header(reqwest::header::REFERER, self.base.as_str())
                    .form(&[
                        ("hashes", joined.as_str()),
                        ("deleteFiles", if delete_files { "true" } else { "false" }),
                    ]),
            )
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(QbitError::Status {
                status: response.status(),
                path: "api/v2/torrents/delete".into(),
            })
        }
    }

    async fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<(), QbitError> {
        let status = self.post_status(path, form).await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(QbitError::Status {
                status,
                path: path.into(),
            })
        }
    }

    /// Manda para `current`; se a versão do cliente não o conhece (404),
    /// para `legacy`.
    async fn post_either(
        &self,
        current: &str,
        legacy: &str,
        hashes: &[&str],
    ) -> Result<(), QbitError> {
        let joined = hashes.join("|");
        let form = [("hashes", joined.as_str())];
        match self.post_status(current, &form).await? {
            reqwest::StatusCode::NOT_FOUND => self.post(legacy, &form).await,
            status if status.is_success() => Ok(()),
            status => Err(QbitError::Status {
                status,
                path: current.into(),
            }),
        }
    }

    async fn post_status(
        &self,
        path: &str,
        form: &[(&str, &str)],
    ) -> Result<reqwest::StatusCode, QbitError> {
        Ok(self
            .send(
                self.http
                    .post(self.base.join(path)?)
                    .header(reqwest::header::REFERER, self.base.as_str())
                    .form(form),
            )
            .await?
            .status())
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, QbitError> {
        let url = self.base.join(path)?;
        let response = self.send(self.http.get(url).query(query)).await?;

        if !response.status().is_success() {
            return Err(QbitError::Status {
                status: response.status(),
                path: path.to_string(),
            });
        }

        Ok(response.json().await?)
    }
}

/// Traduz o estado do qBittorrent para o do domínio.
///
/// Dos `targets`, os que dividem o conteúdo com um torrent que fica no
/// cliente: mesmo caminho, ou um dentro do outro. Hashes em minúsculas.
///
/// Caminho desconhecido (cliente que não informa `content_path`) é "não
/// sei", e na dúvida o arquivo fica: o alvo sem caminho conta como
/// compartilhado, e um torrent que fica sem caminho pode ser irmão de
/// qualquer um.
#[must_use]
pub fn shared_content(targets: &[DownloadHash], all: &[TorrentInfo]) -> HashSet<String> {
    let leaving: HashSet<String> = targets
        .iter()
        .map(|h| h.as_str().to_ascii_lowercase())
        .collect();
    let (going, staying): (Vec<&TorrentInfo>, Vec<&TorrentInfo>) = all
        .iter()
        .partition(|t| leaving.contains(&t.hash.to_ascii_lowercase()));
    let staying: Vec<Option<String>> = staying
        .iter()
        .map(|t| normalized(&t.content_path))
        .collect();
    going
        .iter()
        .filter(|t| match normalized(&t.content_path) {
            None => true,
            Some(path) => staying.iter().any(|other| match other {
                None => true,
                Some(other) => overlaps(&path, other),
            }),
        })
        .map(|t| t.hash.to_ascii_lowercase())
        .collect()
}

/// O caminho com `/` como separador e sem barra no fim; vazio é `None`.
fn normalized(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let path = path.trim_end_matches('/');
    (!path.is_empty()).then(|| path.to_owned())
}

/// Mesmo caminho, ou um dentro do outro, componente a componente.
fn overlaps(a: &str, b: &str) -> bool {
    let inside = |long: &str, short: &str| {
        long.strip_prefix(short)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    inside(a, b) || inside(b, a)
}

/// Só o que está **de fato semeando** conta como seeding. Torrent completo mas
/// parado é `Paused`: ele não está devolvendo nada ao tracker, e tratá-lo como
/// seed o colocaria na avaliação de limpeza junto com os seeds reais.
#[must_use]
pub fn state_from_qbit(raw: &str) -> DownloadState {
    match raw {
        "uploading" | "stalledUP" | "forcedUP" | "queuedUP" | "checkingUP" => {
            DownloadState::Seeding
        }
        "downloading" | "stalledDL" | "forcedDL" | "metaDL" | "forcedMetaDL" | "queuedDL"
        | "checkingDL" | "allocating" | "checkingResumeData" | "moving" => {
            DownloadState::Downloading
        }
        "pausedUP" | "stoppedUP" | "pausedDL" | "stoppedDL" => DownloadState::Paused,
        "error" => DownloadState::Errored,
        "missingFiles" => DownloadState::MissingFiles,
        _ => DownloadState::Unknown,
    }
}

/// Caminho completo de um arquivo, como o **cliente** o vê.
#[must_use]
pub fn client_path(torrent: &TorrentInfo, file: &TorrentFile) -> PathBuf {
    PathBuf::from(&torrent.save_path).join(&file.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_das_opcoes_de_criacao_nao_mostra_o_announce() {
        let options = CreateTorrentOptions::new(
            "/media/Filme",
            vec!["https://tracker.exemplo/announce?passkey=segredo123".into()],
        );
        let texto = format!("{options:?}");
        assert!(!texto.contains("segredo123"));
        assert!(!texto.contains("tracker.exemplo"));
        assert!(texto.contains("/media/Filme"));
    }

    #[test]
    fn opcoes_de_criacao_seguem_o_padrao_do_lancamento() {
        let o = CreateTorrentOptions::new("/m/F", vec![]);
        assert_eq!(o.format, TorrentFormat::V1);
        assert_eq!(o.piece_size, 0);
        assert!(!o.optimize_alignment && o.private && !o.start_seeding);
    }

    #[test]
    fn completo_mas_parado_nao_e_seeding() {
        // qBittorrent 5.x renomeou `paused*` para `stopped*`; os dois nomes
        // convivem no parque instalado.
        assert_eq!(state_from_qbit("pausedUP"), DownloadState::Paused);
        assert_eq!(state_from_qbit("stoppedUP"), DownloadState::Paused);
        assert_eq!(state_from_qbit("stoppedDL"), DownloadState::Paused);
    }

    #[test]
    fn variantes_de_seeding_sao_reconhecidas() {
        for s in ["uploading", "stalledUP", "forcedUP"] {
            assert!(state_from_qbit(s).is_seeding(), "{s} devia ser seeding");
        }
    }

    #[test]
    fn estado_desconhecido_nao_vira_seeding() {
        // Estado novo numa versão futura não pode abrir caminho para remoção.
        assert_eq!(state_from_qbit("missingFiles"), DownloadState::MissingFiles);
        assert_eq!(state_from_qbit("algoNovo"), DownloadState::Unknown);
        assert!(!state_from_qbit("algoNovo").is_seeding());
    }

    #[test]
    fn caminho_do_arquivo_junta_save_path_e_nome_relativo() {
        let torrent: TorrentInfo = serde_json::from_str(
            r#"{"hash":"AA","name":"Serie S01","state":"uploading",
                "save_path":"/media/downloads","ratio":1.0,"seeding_time":100}"#,
        )
        .unwrap();
        let file: TorrentFile =
            serde_json::from_str(r#"{"name":"Serie S01/ep01.mkv","size":10}"#).unwrap();

        assert_eq!(
            client_path(&torrent, &file),
            PathBuf::from("/media/downloads/Serie S01/ep01.mkv")
        );
    }

    #[test]
    fn torrent_sem_campo_private_e_tratado_como_privado() {
        // Versões antigas não expõem `private`. Assumir público aqui removeria
        // a proteção de tracker privado justamente onde ela não pode falhar.
        let torrent: TorrentInfo = serde_json::from_str(
            r#"{"hash":"AA","name":"x","state":"uploading",
                "save_path":"/media","ratio":0.0,"seeding_time":0}"#,
        )
        .unwrap();

        assert!(torrent.is_private());
    }

    #[test]
    fn torrent_publico_declarado_e_publico() {
        let torrent: TorrentInfo = serde_json::from_str(
            r#"{"hash":"AA","name":"x","state":"uploading","private":false,
                "save_path":"/media","ratio":0.0,"seeding_time":0}"#,
        )
        .unwrap();

        assert!(!torrent.is_private());
    }

    fn com_caminho(hash: &str, path: &str) -> TorrentInfo {
        serde_json::from_value(serde_json::json!({
            "hash": hash, "name": hash, "state": "stalledUP", "save_path": "/d",
            "content_path": path,
        }))
        .unwrap()
    }

    #[test]
    fn cross_seed_nao_perde_os_arquivos_quando_o_irmao_sai() {
        let all = [
            com_caminho("BJ", "/d/Filme.mkv"),
            com_caminho("amigos", "/d/Filme.mkv"),
            com_caminho("pack", "/d/Serie.S01"),
            com_caminho("ep", "/d/Serie.S01/E01.mkv"),
            com_caminho("sozinho", "/d/Outro.mkv"),
            com_caminho("prefixo", "/d/Outro.mkv.parte"),
        ];
        let saindo = |hs: &[&str]| -> Vec<DownloadHash> {
            hs.iter().map(|h| DownloadHash::new(*h)).collect()
        };
        // Um irmão fica: o que sai não leva os arquivos.
        assert_eq!(
            shared_content(&saindo(&["BJ"]), &all),
            HashSet::from(["bj".to_owned()])
        );
        // Os dois saem juntos: os arquivos vão.
        assert!(shared_content(&saindo(&["BJ", "amigos"]), &all).is_empty());
        // Pasta e arquivo dentro dela também dividem conteúdo.
        assert_eq!(shared_content(&saindo(&["pack"]), &all).len(), 1);
        // Mesmo começo de nome não é o mesmo conteúdo.
        assert!(shared_content(&saindo(&["sozinho"]), &all).is_empty());
        // Barra no fim e separador do Windows não escondem o irmão.
        let barras = [
            com_caminho("p", "/d/Pack/"),
            com_caminho("e", "/d/Pack/E01.mkv"),
        ];
        assert_eq!(shared_content(&saindo(&["p"]), &barras).len(), 1);
        let windows = [
            com_caminho("p", "D:\\Pack"),
            com_caminho("e", "D:/Pack/E01.mkv"),
        ];
        assert_eq!(shared_content(&saindo(&["p"]), &windows).len(), 1);
        // Caminho desconhecido: na dúvida, o arquivo fica.
        let sem = [com_caminho("x", ""), com_caminho("y", "/d/Y.mkv")];
        assert_eq!(shared_content(&saindo(&["x"]), &sem).len(), 1);
        assert_eq!(shared_content(&saindo(&["y"]), &sem).len(), 1);
    }
}

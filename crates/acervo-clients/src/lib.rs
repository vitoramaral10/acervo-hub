//! Cliente do qBittorrent (`WebUI` API v2).

use std::path::PathBuf;
use std::time::Duration;

use acervo_core::{DownloadHash, DownloadState};
use url::Url;

mod dto;

pub use dto::{TorrentFile, TorrentInfo};

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
}

/// Sessão autenticada no qBittorrent.
#[derive(Debug, Clone)]
pub struct QbitClient {
    base: Url,
    http: reqwest::Client,
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

        let url = base.join("api/v2/auth/login")?;
        let response = http
            .post(url)
            // O Referer é exigido pela proteção de CSRF da WebUI; sem ele o
            // login responde 403 mesmo com credencial correta.
            .header(reqwest::header::REFERER, base.as_str())
            .form(&[("username", username), ("password", password)])
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

        Ok(Self { base, http })
    }

    /// Lista todos os torrents do cliente.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn torrents(&self) -> Result<Vec<TorrentInfo>, QbitError> {
        self.get("api/v2/torrents/info", &[]).await
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

    /// Apaga torrents, opcionalmente com os arquivos.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou status não-2xx.
    pub async fn delete(
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
            .http
            .post(url)
            .header(reqwest::header::REFERER, self.base.as_str())
            .form(&[
                ("hashes", joined.as_str()),
                ("deleteFiles", if delete_files { "true" } else { "false" }),
            ])
            .send()
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

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, QbitError> {
        let url = self.base.join(path)?;
        let response = self.http.get(url).query(query).send().await?;

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
        "error" | "missingFiles" => DownloadState::Errored,
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
}

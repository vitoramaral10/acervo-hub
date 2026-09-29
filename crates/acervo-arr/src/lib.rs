//! Cliente da API v3 das instâncias `*arr`.
//!
//! Só o que a reconciliação precisa: a fila (com os itens cuja obra sumiu), a
//! contagem de obras conhecidas, e a remoção de item de fila.

use std::time::Duration;

use acervo_core::{DownloadHash, InstanceName, InstanceSnapshot, QueueItem, QueueItemId};
use url::Url;

mod dto;
mod indexer;

pub use dto::QueueRecord;
pub use indexer::{RemoteIndexer, TorznabSpec};

/// Qual árvore a instância gerencia.
///
/// Muda três coisas na API: o parâmetro que inclui itens de fila órfãos, o
/// campo que aponta para a obra, e a rota que lista as obras.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrKind {
    Series,
    Movie,
}

impl ArrKind {
    /// Sem este parâmetro a instância **omite** da fila justamente os itens
    /// cuja obra foi apagada — que são os que a reconciliação procura.
    const fn unknown_items_param(self) -> &'static str {
        match self {
            Self::Series => "includeUnknownSeriesItems",
            Self::Movie => "includeUnknownMovieItems",
        }
    }

    const fn works_path(self) -> &'static str {
        match self {
            Self::Series => "api/v3/series",
            Self::Movie => "api/v3/movie",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArrError {
    #[error("url inválida para a instância `{instance}`: {source}")]
    BadUrl {
        instance: InstanceName,
        #[source]
        source: url::ParseError,
    },

    #[error("falha de transporte com a instância `{instance}`: {source}")]
    Transport {
        instance: InstanceName,
        #[source]
        source: reqwest::Error,
    },

    #[error("instância `{instance}` respondeu {status} em `{path}`")]
    Status {
        instance: InstanceName,
        status: reqwest::StatusCode,
        path: String,
    },

    #[error("não foi possível construir o cliente http: {0}")]
    Build(#[source] reqwest::Error),
}

impl ArrError {
    /// Texto curto para o relato de instância fora do ar.
    #[must_use]
    pub fn short(&self) -> String {
        match self {
            Self::Transport { source, .. } if source.is_timeout() => "timeout".into(),
            Self::Transport { source, .. } if source.is_connect() => "conexão recusada".into(),
            Self::Status { status, .. } => format!("HTTP {status}"),
            other => other.to_string(),
        }
    }
}

/// Uma instância `*arr` alcançável por HTTP.
#[derive(Debug, Clone)]
pub struct ArrClient {
    name: InstanceName,
    kind: ArrKind,
    base: Url,
    http: reqwest::Client,
}

impl ArrClient {
    /// # Errors
    ///
    /// Url malformada ou falha ao montar o cliente HTTP.
    pub fn new(
        name: InstanceName,
        base_url: &str,
        api_key: &str,
        kind: ArrKind,
        timeout: Duration,
    ) -> Result<Self, ArrError> {
        // Barra final garante que `join` trate a base como diretório; sem ela,
        // `join("api/v3/queue")` descarta o último segmento do caminho.
        let normalized = if base_url.ends_with('/') {
            base_url.to_string()
        } else {
            format!("{base_url}/")
        };
        let base = Url::parse(&normalized).map_err(|source| ArrError::BadUrl {
            instance: name.clone(),
            source,
        })?;

        let mut headers = reqwest::header::HeaderMap::new();
        let mut key =
            reqwest::header::HeaderValue::from_str(api_key).map_err(|_| ArrError::Status {
                instance: name.clone(),
                status: reqwest::StatusCode::UNAUTHORIZED,
                path: "api-key".into(),
            })?;
        key.set_sensitive(true);
        headers.insert("X-Api-Key", key);

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .default_headers(headers)
            .build()
            .map_err(ArrError::Build)?;

        Ok(Self {
            name,
            kind,
            base,
            http,
        })
    }

    #[must_use]
    pub fn name(&self) -> &InstanceName {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> ArrKind {
        self.kind
    }

    /// Lê fila e contagem de obras num ciclo.
    ///
    /// # Errors
    ///
    /// Qualquer falha de rede ou status não-2xx. O erro sobe inteiro: quem
    /// chama precisa registrar a instância como inalcançável, não seguir com
    /// uma fila pela metade.
    pub async fn snapshot(&self) -> Result<InstanceSnapshot, ArrError> {
        let queue = self.queue().await?;
        let known_works = self.count_works().await?;

        Ok(InstanceSnapshot {
            instance: self.name.clone(),
            queue,
            known_works,
        })
    }

    async fn queue(&self) -> Result<Vec<QueueItem>, ArrError> {
        const PAGE_SIZE: u32 = 200;

        let mut items = Vec::new();
        let mut page = 1_u32;

        loop {
            let path = "api/v3/queue";
            let url = self.url(path)?;
            let page_text = page.to_string();
            let size_text = PAGE_SIZE.to_string();
            let response: dto::QueuePage = self
                .get(
                    url,
                    &[
                        ("page", page_text.as_str()),
                        ("pageSize", size_text.as_str()),
                        (self.kind.unknown_items_param(), "true"),
                    ],
                    path,
                )
                .await?;

            let received = response.records.len();
            items.extend(
                response
                    .records
                    .into_iter()
                    .map(|r| self.queue_item_from(r)),
            );

            // Para tanto por página curta quanto por total atingido: instâncias
            // divergem em qual dos dois sinais é confiável.
            let total_reached = response.total_records.is_some_and(|t| items.len() >= t);
            if received < PAGE_SIZE as usize || total_reached {
                break;
            }
            page += 1;
        }

        Ok(items)
    }

    /// Conta as obras que a instância conhece.
    ///
    /// A v3 não expõe contagem, então lista tudo e conta. O custo é aceitável
    /// num acervo doméstico e o número serve a uma única pergunta: a instância
    /// está respondendo com inventário de verdade, ou respondendo vazia?
    async fn count_works(&self) -> Result<usize, ArrError> {
        let path = self.kind.works_path();
        let url = self.url(path)?;
        let works: Vec<serde::de::IgnoredAny> = self.get(url, &[], path).await?;
        Ok(works.len())
    }

    /// Remove um item da fila.
    ///
    /// `remove_from_client` também apaga o torrent e os dados no cliente.
    /// `skipRedownload` evita que a instância saia procurando substituto do que
    /// acabou de ser declarado órfão.
    ///
    /// # Errors
    ///
    /// Falha de rede ou status não-2xx.
    pub async fn remove_queue_item(
        &self,
        item: QueueItemId,
        remove_from_client: bool,
    ) -> Result<(), ArrError> {
        let path = format!("api/v3/queue/{item}");
        let url = self.url(&path)?;

        let response = self
            .http
            .delete(url)
            .query(&[
                ("removeFromClient", bool_param(remove_from_client)),
                ("blocklist", "false"),
                ("skipRedownload", "true"),
                ("changeCategory", "false"),
            ])
            .send()
            .await
            .map_err(|source| ArrError::Transport {
                instance: self.name.clone(),
                source,
            })?;

        self.check_status(response, &path).map(|_| ())
    }

    fn url(&self, path: &str) -> Result<Url, ArrError> {
        self.base.join(path).map_err(|source| ArrError::BadUrl {
            instance: self.name.clone(),
            source,
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        query: &[(&str, &str)],
        path: &str,
    ) -> Result<T, ArrError> {
        let response = self
            .http
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|source| ArrError::Transport {
                instance: self.name.clone(),
                source,
            })?;

        self.check_status(response, path)?
            .json()
            .await
            .map_err(|source| ArrError::Transport {
                instance: self.name.clone(),
                source,
            })
    }

    fn check_status(
        &self,
        response: reqwest::Response,
        path: &str,
    ) -> Result<reqwest::Response, ArrError> {
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(ArrError::Status {
                instance: self.name.clone(),
                status: response.status(),
                path: path.to_string(),
            })
        }
    }

    fn queue_item_from(&self, record: QueueRecord) -> QueueItem {
        let work = QueueItem::normalize_work(record.parent_id(self.kind));

        QueueItem {
            id: QueueItemId(record.id),
            instance: self.name.clone(),
            title: record
                .title
                .unwrap_or_else(|| format!("item {}", record.id)),
            download: record
                .download_id
                .filter(|d| !d.trim().is_empty())
                .map(DownloadHash::new),
            work,
        }
    }
}

const fn bool_param(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(kind: ArrKind) -> ArrClient {
        ArrClient::new(
            InstanceName::new("teste"),
            "http://localhost:7878",
            "chave",
            kind,
            Duration::from_secs(5),
        )
        .expect("cliente válido")
    }

    #[test]
    fn base_sem_barra_final_ainda_resolve_a_rota() {
        let c = client(ArrKind::Movie);
        assert_eq!(
            c.url("api/v3/queue").unwrap().as_str(),
            "http://localhost:7878/api/v3/queue"
        );
    }

    #[test]
    fn base_com_subcaminho_e_preservada() {
        let c = ArrClient::new(
            InstanceName::new("teste"),
            "http://localhost/radarr",
            "chave",
            ArrKind::Movie,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            c.url("api/v3/queue").unwrap().as_str(),
            "http://localhost/radarr/api/v3/queue"
        );
    }

    #[test]
    fn registro_sem_obra_vira_item_orfao() {
        let c = client(ArrKind::Movie);
        let record: QueueRecord = serde_json::from_str(
            r#"{"id": 7, "title": "Um Filme", "downloadId": "ABCDEF", "movieId": 0}"#,
        )
        .unwrap();

        let item = c.queue_item_from(record);

        assert!(item.is_orphaned());
        assert_eq!(item.download.unwrap().as_str(), "abcdef");
    }

    #[test]
    fn registro_com_obra_nao_e_orfao() {
        let c = client(ArrKind::Series);
        let record: QueueRecord =
            serde_json::from_str(r#"{"id": 7, "title": "Uma Série", "seriesId": 42}"#).unwrap();

        let item = c.queue_item_from(record);

        assert!(!item.is_orphaned());
        assert!(item.download.is_none());
    }

    #[test]
    fn campo_da_obra_e_lido_conforme_o_tipo_da_instancia() {
        // Uma instância de filmes não deve aceitar `seriesId` como pai: se
        // aceitasse, um registro cruzado esconderia um órfão de verdade.
        let record: QueueRecord =
            serde_json::from_str(r#"{"id": 1, "seriesId": 42, "movieId": 0}"#).unwrap();

        assert_eq!(record.parent_id(ArrKind::Movie), Some(0));
        assert_eq!(record.parent_id(ArrKind::Series), Some(42));
    }

    #[test]
    fn download_id_vazio_conta_como_ausente() {
        let c = client(ArrKind::Movie);
        let record: QueueRecord =
            serde_json::from_str(r#"{"id": 1, "downloadId": "  ", "movieId": 3}"#).unwrap();

        assert!(c.queue_item_from(record).download.is_none());
    }
}

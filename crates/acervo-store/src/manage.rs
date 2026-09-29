//! O que faz do acervo um gerenciador, e não só um espelho: histórico,
//! lista de bloqueio, exclusões, formatos personalizados, perfis editáveis e
//! listas de importação.

use acervo_parser::Quality;
use deadpool_postgres::GenericClient;
use serde::Serialize;
use serde_json::Value;

use crate::{Result, Store, quality};

/// Um evento do histórico, como gravado.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryEvent {
    pub id: i64,
    /// `None` quando o filme saiu do catálogo; o título fica.
    pub movie_id: Option<i64>,
    pub movie_title: String,
    /// `grabbed`, `imported`, `upgraded`, `failed`, `file_deleted`,
    /// `movie_added`, `movie_deleted`, `ignored`.
    pub event: String,
    /// RFC 3339, em UTC.
    pub at: String,
    pub source_title: Option<String>,
    #[serde(serialize_with = "quality_name")]
    pub quality: Option<Quality>,
    pub indexer: Option<String>,
    pub download_id: Option<String>,
    pub data: Value,
}

#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // Assinatura que o serde pede.
fn quality_name<S: serde::Serializer>(
    quality: &Option<Quality>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match quality {
        Some(q) => serializer.serialize_str(q.name()),
        None => serializer.serialize_none(),
    }
}

/// Um evento a gravar.
#[derive(Debug, Clone, PartialEq)]
pub struct NewHistory {
    pub movie_id: Option<i64>,
    pub movie_title: String,
    pub event: String,
    pub at: String,
    pub source_title: Option<String>,
    pub quality: Option<Quality>,
    pub indexer: Option<String>,
    pub download_id: Option<String>,
    pub data: Value,
}

/// Uma página do histórico.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryPage {
    pub total: i64,
    pub events: Vec<HistoryEvent>,
}

/// Um release que não se pega de novo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocked {
    pub id: i64,
    pub movie_id: Option<i64>,
    pub source_title: String,
    pub indexer: Option<String>,
    #[serde(serialize_with = "quality_name")]
    pub quality: Option<Quality>,
    pub size: Option<u64>,
    pub hash: Option<String>,
    pub at: String,
    pub message: Option<String>,
}

fn size(value: Option<i64>) -> Option<u64> {
    value.and_then(|v| u64::try_from(v).ok())
}

fn history_row(row: &tokio_postgres::Row) -> Result<HistoryEvent> {
    Ok(HistoryEvent {
        id: row.try_get(0)?,
        movie_id: row.try_get(1)?,
        movie_title: row.try_get(2)?,
        event: row.try_get(3)?,
        at: row.try_get(4)?,
        source_title: row.try_get(5)?,
        quality: row.try_get::<_, Option<i16>>(6)?.map(quality).transpose()?,
        indexer: row.try_get(7)?,
        download_id: row.try_get(8)?,
        data: row.try_get(9)?,
    })
}

const HISTORY_COLUMNS: &str =
    "id, movie_id, movie_title, event, at, source_title, quality, indexer, download_id, data";

impl Store {
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn record_history(&self, event: &NewHistory) -> Result<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "INSERT INTO history (movie_id, movie_title, event, at, source_title, quality,
                     indexer, download_id, data)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
                &[
                    &event.movie_id,
                    &event.movie_title,
                    &event.event,
                    &event.at,
                    &event.source_title,
                    &event.quality.map(|q| i16::from(q.id())),
                    &event.indexer,
                    &event.download_id,
                    &event.data,
                ],
            )
            .await?;
        Ok(row.try_get(0)?)
    }

    /// Histórico do mais novo ao mais velho, de um filme ou de todos.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn history(
        &self,
        movie_id: Option<i64>,
        event: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<HistoryPage> {
        let client = self.pool.get().await?;
        let total: i64 = client
            .query_one(
                "SELECT COUNT(*) FROM history
                 WHERE ($1::BIGINT IS NULL OR movie_id = $1) AND ($2::TEXT IS NULL OR event = $2)",
                &[&movie_id, &event],
            )
            .await?
            .try_get(0)?;
        let rows = client
            .query(
                &format!(
                    "SELECT {HISTORY_COLUMNS} FROM history
                     WHERE ($1::BIGINT IS NULL OR movie_id = $1) AND ($2::TEXT IS NULL OR event = $2)
                     ORDER BY at DESC, id DESC LIMIT $3 OFFSET $4"
                ),
                &[&movie_id, &event, &limit, &offset],
            )
            .await?;
        Ok(HistoryPage {
            total,
            events: rows.iter().map(history_row).collect::<Result<_>>()?,
        })
    }

    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn block(&self, blocked: &Blocked) -> Result<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "INSERT INTO blocklist (movie_id, source_title, indexer, quality, size, hash, at, message)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
                &[
                    &blocked.movie_id,
                    &blocked.source_title,
                    &blocked.indexer,
                    &blocked.quality.map(|q| i16::from(q.id())),
                    &blocked.size.and_then(|s| i64::try_from(s).ok()),
                    &blocked.hash,
                    &blocked.at,
                    &blocked.message,
                ],
            )
            .await?;
        Ok(row.try_get(0)?)
    }

    /// Lista de bloqueio, do mais novo ao mais velho.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn blocklist(&self) -> Result<Vec<Blocked>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT id, movie_id, source_title, indexer, quality, size, hash, at, message
                 FROM blocklist ORDER BY at DESC, id DESC",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(Blocked {
                    id: row.try_get(0)?,
                    movie_id: row.try_get(1)?,
                    source_title: row.try_get(2)?,
                    indexer: row.try_get(3)?,
                    quality: row.try_get::<_, Option<i16>>(4)?.map(quality).transpose()?,
                    size: size(row.try_get(5)?),
                    hash: row.try_get(6)?,
                    at: row.try_get(7)?,
                    message: row.try_get(8)?,
                })
            })
            .collect()
    }

    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn unblock(&self, id: i64) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute("DELETE FROM blocklist WHERE id = $1", &[&id])
            .await?
            > 0)
    }
}

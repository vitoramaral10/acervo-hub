//! O que faz do acervo um gerenciador, e não só um espelho: histórico,
//! lista de bloqueio, exclusões, formatos personalizados, perfis editáveis e
//! listas de importação.

use acervo_parser::Quality;
use deadpool_postgres::GenericClient;
use serde::Serialize;
use serde_json::Value;

use crate::{
    QualityDefinition, QualityProfile, Result, Store, StoreError, decode_items, decode_scores,
    encode_items, encode_scores, quality,
};

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

/// Um filme que nenhuma lista traz de volta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exclusion {
    pub tmdb_id: u32,
    pub title: String,
    pub year: Option<u16>,
}

/// Formato personalizado. As especificações ficam no formato que o motor de
/// decisão lê; o banco só as guarda.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomFormat {
    pub id: i64,
    pub name: String,
    pub specifications: Value,
    pub include_when_renaming: bool,
}

/// Uma lista de importação: de onde vêm filmes, e como eles entram.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportList {
    pub id: i64,
    pub name: String,
    /// `tmdb_person`, `tmdb_collection` ou `tmdb_list`.
    pub kind: String,
    pub settings: Value,
    pub enabled: bool,
    pub monitor: bool,
    pub search_on_add: bool,
    /// Id do perfil.
    pub quality_profile_id: Option<i64>,
    pub root_folder: String,
    pub minimum_availability: String,
    pub tags: Vec<i64>,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
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

/// Espelha os formatos da origem com os ids de lá.
pub(crate) async fn mirror_formats(
    client: &impl GenericClient,
    formats: &[CustomFormat],
) -> Result<()> {
    for format in formats {
        client
            .execute(
                "INSERT INTO custom_formats (id, name, specifications, include_when_renaming)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name,
                     specifications = excluded.specifications,
                     include_when_renaming = excluded.include_when_renaming",
                &[
                    &format.id,
                    &format.name,
                    &format.specifications,
                    &format.include_when_renaming,
                ],
            )
            .await?;
    }
    if !formats.is_empty() {
        client
            .execute(
                "SELECT setval('custom_formats_id_seq', GREATEST((SELECT MAX(id) FROM custom_formats), 1))",
                &[],
            )
            .await?;
    }
    Ok(())
}

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

    /// Grava vários eventos numa transação — a migração do histórico.
    ///
    /// # Errors
    ///
    /// Falha de escrita; nada fica gravado.
    pub async fn record_history_batch(&self, events: &[NewHistory]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let statement = tx
            .prepare(
                "INSERT INTO history (movie_id, movie_title, event, at, source_title, quality,
                     indexer, download_id, data)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .await?;
        for event in events {
            tx.execute(
                &statement,
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
        }
        tx.commit().await?;
        Ok(())
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

    /// Quantos eventos já vieram de uma origem (`data.origem`): a migração
    /// não grava duas vezes.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn history_from(&self, origin: &str) -> Result<i64> {
        let client = self.pool.get().await?;
        Ok(client
            .query_one(
                "SELECT COUNT(*) FROM history WHERE data->>'origem' = $1",
                &[&origin],
            )
            .await?
            .try_get(0)?)
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

    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn exclusions(&self) -> Result<Vec<Exclusion>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT tmdb_id, title, year FROM exclusions ORDER BY title",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(Exclusion {
                    tmdb_id: u32::try_from(row.try_get::<_, i64>(0)?)
                        .map_err(|_| StoreError::Corrupt("tmdb da exclusão".into()))?,
                    title: row.try_get(1)?,
                    year: row
                        .try_get::<_, Option<i32>>(2)?
                        .and_then(|y| u16::try_from(y).ok()),
                })
            })
            .collect()
    }

    /// Acrescenta exclusões; as que já existem ficam como estão.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn add_exclusions(&self, exclusions: &[Exclusion]) -> Result<u64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut added = 0;
        for exclusion in exclusions {
            added += tx
                .execute(
                    "INSERT INTO exclusions (tmdb_id, title, year) VALUES ($1, $2, $3)
                     ON CONFLICT (tmdb_id) DO NOTHING",
                    &[
                        &i64::from(exclusion.tmdb_id),
                        &exclusion.title,
                        &exclusion.year.map(i32::from),
                    ],
                )
                .await?;
        }
        tx.commit().await?;
        Ok(added)
    }

    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn remove_exclusion(&self, tmdb_id: u32) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "DELETE FROM exclusions WHERE tmdb_id = $1",
                &[&i64::from(tmdb_id)],
            )
            .await?
            > 0)
    }

    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn custom_formats(&self) -> Result<Vec<CustomFormat>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT id, name, specifications, include_when_renaming
                 FROM custom_formats ORDER BY name",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(CustomFormat {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    specifications: row.try_get(2)?,
                    include_when_renaming: row.try_get(3)?,
                })
            })
            .collect()
    }

    /// Cria (`id` zero) ou atualiza um formato. Devolve o id.
    ///
    /// # Errors
    ///
    /// Nome repetido ou falha de escrita.
    pub async fn save_custom_format(&self, format: &CustomFormat) -> Result<i64> {
        let client = self.pool.get().await?;
        if format.id == 0 {
            let row = client
                .query_one(
                    "INSERT INTO custom_formats (name, specifications, include_when_renaming)
                     VALUES ($1, $2, $3) RETURNING id",
                    &[
                        &format.name,
                        &format.specifications,
                        &format.include_when_renaming,
                    ],
                )
                .await?;
            return Ok(row.try_get(0)?);
        }
        client
            .execute(
                "UPDATE custom_formats SET name = $2, specifications = $3,
                     include_when_renaming = $4 WHERE id = $1",
                &[
                    &format.id,
                    &format.name,
                    &format.specifications,
                    &format.include_when_renaming,
                ],
            )
            .await?;
        Ok(format.id)
    }

    /// Apaga o formato e a nota dele em todos os perfis.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn delete_custom_format(&self, id: i64) -> Result<bool> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "UPDATE quality_profiles SET format_scores = format_scores - $1",
            &[&id.to_string()],
        )
        .await?;
        let removed = tx
            .execute("DELETE FROM custom_formats WHERE id = $1", &[&id])
            .await?;
        tx.commit().await?;
        Ok(removed > 0)
    }

    /// Os perfis, com o id de cada um.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn profiles_by_id(&self) -> Result<Vec<(i64, QualityProfile)>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT id, name, upgrade_allowed, cutoff, language, items, min_format_score,
                        cutoff_format_score, source_id, format_scores
                 FROM quality_profiles ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok((
                    row.try_get(0)?,
                    QualityProfile {
                        name: row.try_get(1)?,
                        upgrade_allowed: row.try_get(2)?,
                        cutoff: row
                            .try_get::<_, Option<i32>>(3)?
                            .and_then(|c| usize::try_from(c).ok()),
                        language: row.try_get(4)?,
                        items: decode_items(row.try_get(5)?)?,
                        min_format_score: row.try_get(6)?,
                        cutoff_format_score: row.try_get(7)?,
                        source_id: row.try_get(8)?,
                        format_scores: decode_scores(row.try_get(9)?)?,
                    },
                ))
            })
            .collect()
    }

    /// Cria (`id` `None`) ou atualiza um perfil. Devolve o id.
    ///
    /// # Errors
    ///
    /// Nome repetido ou falha de escrita.
    pub async fn save_profile(&self, id: Option<i64>, profile: &QualityProfile) -> Result<i64> {
        let client = self.pool.get().await?;
        let cutoff = profile.cutoff.and_then(|c| i32::try_from(c).ok());
        let items = encode_items(&profile.items);
        let scores = encode_scores(&profile.format_scores);
        if let Some(id) = id {
            client
                .execute(
                    "UPDATE quality_profiles SET name = $2, upgrade_allowed = $3, cutoff = $4,
                         language = $5, items = $6, min_format_score = $7,
                         cutoff_format_score = $8, format_scores = $9
                     WHERE id = $1",
                    &[
                        &id,
                        &profile.name,
                        &profile.upgrade_allowed,
                        &cutoff,
                        &profile.language,
                        &items,
                        &profile.min_format_score,
                        &profile.cutoff_format_score,
                        &scores,
                    ],
                )
                .await?;
            return Ok(id);
        }
        let row = client
            .query_one(
                "INSERT INTO quality_profiles (name, upgrade_allowed, cutoff, language, items,
                     min_format_score, cutoff_format_score, format_scores)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
                &[
                    &profile.name,
                    &profile.upgrade_allowed,
                    &cutoff,
                    &profile.language,
                    &items,
                    &profile.min_format_score,
                    &profile.cutoff_format_score,
                    &scores,
                ],
            )
            .await?;
        Ok(row.try_get(0)?)
    }

    /// Apaga o perfil, se nenhum filme nem lista o usa.
    ///
    /// # Errors
    ///
    /// Perfil em uso ou falha de escrita.
    pub async fn delete_profile(&self, id: i64) -> Result<bool> {
        let client = self.pool.get().await?;
        let used: i64 = client
            .query_one(
                "SELECT (SELECT COUNT(*) FROM movies WHERE quality_profile_id = $1)
                      + (SELECT COUNT(*) FROM import_lists WHERE quality_profile_id = $1)",
                &[&id],
            )
            .await?
            .try_get(0)?;
        if used > 0 {
            return Err(StoreError::Corrupt(format!(
                "o perfil está em uso por {used} filmes ou listas"
            )));
        }
        Ok(client
            .execute("DELETE FROM quality_profiles WHERE id = $1", &[&id])
            .await?
            > 0)
    }

    /// Troca os tamanhos por qualidade.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_quality_definitions(&self, definitions: &[QualityDefinition]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        for definition in definitions {
            tx.execute(
                "INSERT INTO quality_definitions (quality, min_size, max_size, preferred_size)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (quality) DO UPDATE SET min_size = excluded.min_size,
                     max_size = excluded.max_size, preferred_size = excluded.preferred_size",
                &[
                    &i16::from(definition.quality.id()),
                    &definition.min_size,
                    &definition.max_size,
                    &definition.preferred_size,
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn import_lists(&self) -> Result<Vec<ImportList>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT id, name, kind, settings, enabled, monitor, search_on_add,
                        quality_profile_id, root_folder, minimum_availability, tags,
                        last_sync, last_error
                 FROM import_lists ORDER BY name",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(ImportList {
                    id: row.try_get(0)?,
                    name: row.try_get(1)?,
                    kind: row.try_get(2)?,
                    settings: row.try_get(3)?,
                    enabled: row.try_get(4)?,
                    monitor: row.try_get(5)?,
                    search_on_add: row.try_get(6)?,
                    quality_profile_id: row.try_get(7)?,
                    root_folder: row.try_get(8)?,
                    minimum_availability: row.try_get(9)?,
                    tags: serde_json::from_value(row.try_get(10)?)
                        .map_err(|e| StoreError::Corrupt(format!("tags da lista: {e}")))?,
                    last_sync: row.try_get(11)?,
                    last_error: row.try_get(12)?,
                })
            })
            .collect()
    }

    /// Cria (`id` zero) ou atualiza uma lista. Devolve o id.
    ///
    /// # Errors
    ///
    /// Perfil inexistente ou falha de escrita.
    pub async fn save_import_list(&self, list: &ImportList) -> Result<i64> {
        let client = self.pool.get().await?;
        let tags = serde_json::to_value(&list.tags).unwrap_or(Value::Array(Vec::new()));
        if list.id == 0 {
            let row = client
                .query_one(
                    "INSERT INTO import_lists (name, kind, settings, enabled, monitor, search_on_add,
                         quality_profile_id, root_folder, minimum_availability, tags)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
                    &[
                        &list.name,
                        &list.kind,
                        &list.settings,
                        &list.enabled,
                        &list.monitor,
                        &list.search_on_add,
                        &list.quality_profile_id,
                        &list.root_folder,
                        &list.minimum_availability,
                        &tags,
                    ],
                )
                .await?;
            return Ok(row.try_get(0)?);
        }
        client
            .execute(
                "UPDATE import_lists SET name = $2, kind = $3, settings = $4, enabled = $5,
                     monitor = $6, search_on_add = $7, quality_profile_id = $8, root_folder = $9,
                     minimum_availability = $10, tags = $11
                 WHERE id = $1",
                &[
                    &list.id,
                    &list.name,
                    &list.kind,
                    &list.settings,
                    &list.enabled,
                    &list.monitor,
                    &list.search_on_add,
                    &list.quality_profile_id,
                    &list.root_folder,
                    &list.minimum_availability,
                    &tags,
                ],
            )
            .await?;
        Ok(list.id)
    }

    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn delete_import_list(&self, id: i64) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute("DELETE FROM import_lists WHERE id = $1", &[&id])
            .await?
            > 0)
    }

    /// Anota a última sincronização da lista.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_import_list_sync(&self, id: i64, at: &str, error: Option<&str>) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE import_lists SET last_sync = $2, last_error = $3 WHERE id = $1",
                &[&id, &at, &error],
            )
            .await?;
        Ok(())
    }
}

//! As definições Cardigann baixadas do repositório oficial e a estatística
//! diária de cada indexador.

use crate::{Result, Store};

/// Uma definição guardada no banco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionRow {
    pub id: String,
    pub yaml: String,
    /// SHA-256 do YAML, em hexadecimal.
    pub sha: String,
    pub updated_at: String,
}

/// O que somar à estatística de um indexador num dia.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsDelta {
    pub indexer: String,
    /// `AAAA-MM-DD`, em UTC.
    pub day: String,
    pub queries: u32,
    pub failures: u32,
    pub rate_limited: u32,
    pub grabs: u32,
    pub total_ms: u64,
}

/// A estatística de um indexador num dia.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexerDayStats {
    pub indexer: String,
    /// `AAAA-MM-DD`.
    pub day: String,
    pub queries: u32,
    pub failures: u32,
    pub rate_limited: u32,
    pub grabs: u32,
    pub total_ms: u64,
}

fn int(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn uint(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

impl Store {
    /// Todas as definições guardadas, por id.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn definitions(&self) -> Result<Vec<DefinitionRow>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, yaml, sha, updated_at FROM definitions ORDER BY id",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok(DefinitionRow {
                    id: row.try_get(0)?,
                    yaml: row.try_get(1)?,
                    sha: row.try_get(2)?,
                    updated_at: row.try_get(3)?,
                })
            })
            .collect()
    }

    /// Os SHA das definições guardadas, sem o YAML: o que a atualização
    /// precisa para saber o que mudou.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn definition_shas(&self) -> Result<Vec<(String, String)>> {
        let client = self.pool.get().await?;
        let rows = client
            .query("SELECT id, sha FROM definitions ORDER BY id", &[])
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
            .collect()
    }

    /// Grava as definições por cima das de mesmo id, numa transação só:
    /// ou entram todas, ou nenhuma.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn save_definitions(&self, rows: &[DefinitionRow]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let statement = tx
            .prepare(
                "INSERT INTO definitions (id, yaml, sha, updated_at) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (id) DO UPDATE SET yaml = excluded.yaml, sha = excluded.sha,
                     updated_at = excluded.updated_at",
            )
            .await?;
        for row in rows {
            tx.execute(&statement, &[&row.id, &row.yaml, &row.sha, &row.updated_at])
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Soma à estatística de cada indexador no dia, criando a linha do dia
    /// se ainda não existe.
    ///
    /// # Errors
    ///
    /// Dia fora do formato `AAAA-MM-DD` ou falha de escrita.
    pub async fn add_indexer_stats(&self, deltas: &[StatsDelta]) -> Result<()> {
        if deltas.is_empty() {
            return Ok(());
        }
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let statement = tx
            .prepare(
                "INSERT INTO indexer_stats
                     (indexer, day, queries, failures, rate_limited, grabs, total_ms)
                 VALUES ($1, CAST($2::text AS date), $3, $4, $5, $6, $7)
                 ON CONFLICT (indexer, day) DO UPDATE SET
                     queries = indexer_stats.queries + excluded.queries,
                     failures = indexer_stats.failures + excluded.failures,
                     rate_limited = indexer_stats.rate_limited + excluded.rate_limited,
                     grabs = indexer_stats.grabs + excluded.grabs,
                     total_ms = indexer_stats.total_ms + excluded.total_ms",
            )
            .await?;
        for delta in deltas {
            let total_ms = i64::try_from(delta.total_ms).unwrap_or(i64::MAX);
            tx.execute(
                &statement,
                &[
                    &delta.indexer,
                    &delta.day,
                    &int(delta.queries),
                    &int(delta.failures),
                    &int(delta.rate_limited),
                    &int(delta.grabs),
                    &total_ms,
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// A estatística de todos os indexadores a partir do dia `since`
    /// (`AAAA-MM-DD`, incluso), por indexador e dia.
    ///
    /// # Errors
    ///
    /// Dia fora do formato ou falha de leitura.
    pub async fn indexer_stats(&self, since: &str) -> Result<Vec<IndexerDayStats>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT indexer, day::text, queries, failures, rate_limited, grabs, total_ms
                 FROM indexer_stats WHERE day >= CAST($1::text AS date)
                 ORDER BY indexer, day",
                &[&since],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok(IndexerDayStats {
                    indexer: row.try_get(0)?,
                    day: row.try_get(1)?,
                    queries: uint(row.try_get(2)?),
                    failures: uint(row.try_get(3)?),
                    rate_limited: uint(row.try_get(4)?),
                    grabs: uint(row.try_get(5)?),
                    total_ms: u64::try_from(row.try_get::<_, i64>(6)?).unwrap_or(0),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDb;

    fn row(id: &str, yaml: &str, sha: &str) -> DefinitionRow {
        DefinitionRow {
            id: id.into(),
            yaml: yaml.into(),
            sha: sha.into(),
            updated_at: "2026-10-04T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn definicoes_gravam_por_cima_do_mesmo_id() {
        let Some(db) = TestDb::new("definicoes").await else {
            return;
        };
        let store = &db.store;
        assert!(store.definitions().await.unwrap().is_empty());
        store
            .save_definitions(&[row("um", "id: um", "a1"), row("dois", "id: dois", "b1")])
            .await
            .unwrap();
        store
            .save_definitions(&[row("um", "id: um\nname: Um", "a2")])
            .await
            .unwrap();
        let all = store.definitions().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].id, "um");
        assert_eq!(all[1].yaml, "id: um\nname: Um");
        assert_eq!(
            store.definition_shas().await.unwrap(),
            [
                ("dois".to_owned(), "b1".to_owned()),
                ("um".to_owned(), "a2".to_owned())
            ]
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn estatistica_soma_por_indexador_e_dia() {
        let Some(db) = TestDb::new("estatistica").await else {
            return;
        };
        let store = &db.store;
        let delta = |indexer: &str, day: &str, queries, failures, grabs, total_ms| StatsDelta {
            indexer: indexer.into(),
            day: day.into(),
            queries,
            failures,
            rate_limited: failures / 2,
            grabs,
            total_ms,
        };
        store
            .add_indexer_stats(&[
                delta("a", "2026-10-01", 3, 2, 0, 900),
                delta("a", "2026-10-02", 1, 0, 1, 100),
                delta("b", "2026-10-02", 2, 0, 0, 50),
            ])
            .await
            .unwrap();
        store
            .add_indexer_stats(&[delta("a", "2026-10-02", 2, 1, 1, 400)])
            .await
            .unwrap();
        let since = store.indexer_stats("2026-10-02").await.unwrap();
        assert_eq!(
            since,
            [
                IndexerDayStats {
                    indexer: "a".into(),
                    day: "2026-10-02".into(),
                    queries: 3,
                    failures: 1,
                    rate_limited: 0,
                    grabs: 2,
                    total_ms: 500,
                },
                IndexerDayStats {
                    indexer: "b".into(),
                    day: "2026-10-02".into(),
                    queries: 2,
                    failures: 0,
                    rate_limited: 0,
                    grabs: 0,
                    total_ms: 50,
                },
            ]
        );
        let all = store.indexer_stats("2026-01-01").await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].rate_limited, 1);
        assert!(
            store
                .add_indexer_stats(&[delta("a", "ontem", 1, 0, 0, 1)])
                .await
                .is_err()
        );
        db.drop().await;
    }
}

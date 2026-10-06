//! Preferências persistentes de títulos no Descobrir.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiscoverKind {
    #[serde(rename = "filme")]
    Filme,
    #[serde(rename = "serie")]
    Serie,
}

impl DiscoverKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filme => "filme",
            Self::Serie => "serie",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenTitle {
    pub kind: DiscoverKind,
    pub tmdb_id: u32,
    pub title: String,
    pub hidden_at: String,
}

fn id(value: i32) -> Result<u32> {
    u32::try_from(value).map_err(|_| StoreError::Corrupt(format!("id negativo: {value}")))
}

fn sql_id(value: u32) -> Result<i32> {
    i32::try_from(value).map_err(|_| StoreError::Corrupt(format!("id fora do intervalo: {value}")))
}

impl Store {
    /// Títulos ocultos, mais recentes primeiro.
    ///
    /// # Errors
    /// Falha de leitura ou registro inconsistente.
    pub async fn hidden_titles(&self) -> Result<Vec<HiddenTitle>> {
        let client = self.pool.get().await?;
        client.query("SELECT kind, tmdb_id, title, hidden_at FROM discover_hidden_titles ORDER BY hidden_at DESC, kind, tmdb_id", &[]).await?
            .iter().map(|row| {
                let kind: &str = row.try_get(0)?;
                Ok(HiddenTitle {
                    kind: match kind { "filme" => DiscoverKind::Filme, "serie" => DiscoverKind::Serie, _ => return Err(StoreError::Corrupt(format!("tipo desconhecido: {kind}"))) },
                    tmdb_id: id(row.try_get(1)?)?, title: row.try_get(2)?, hidden_at: row.try_get(3)?,
                })
            }).collect()
    }

    /// Oculta um título; repetir preserva o registro original.
    ///
    /// # Errors
    /// Falha de escrita ou id fora do intervalo do banco.
    pub async fn hide_title(
        &self,
        kind: DiscoverKind,
        tmdb_id: u32,
        title: &str,
        at: &str,
    ) -> Result<()> {
        self.pool.get().await?.execute("INSERT INTO discover_hidden_titles (kind, tmdb_id, title, hidden_at) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING", &[&kind.as_str(), &sql_id(tmdb_id)?, &title, &at]).await?;
        Ok(())
    }

    /// Mostra o título de novo; indica se havia uma preferência.
    ///
    /// # Errors
    /// Falha de escrita ou id fora do intervalo do banco.
    pub async fn unhide_title(&self, kind: DiscoverKind, tmdb_id: u32) -> Result<bool> {
        Ok(self
            .pool
            .get()
            .await?
            .execute(
                "DELETE FROM discover_hidden_titles WHERE kind = $1 AND tmdb_id = $2",
                &[&kind.as_str(), &sql_id(tmdb_id)?],
            )
            .await?
            > 0)
    }

    /// Id local de um filme ou série pelo id TMDB.
    ///
    /// # Errors
    /// Falha de leitura do catálogo.
    pub async fn catalog_id_by_tmdb(
        &self,
        kind: DiscoverKind,
        tmdb_id: u32,
    ) -> Result<Option<i64>> {
        let sql = match kind {
            DiscoverKind::Filme => "SELECT id FROM movies WHERE tmdb_id = $1",
            DiscoverKind::Serie => "SELECT id FROM series WHERE tmdb_id = $1",
        };
        self.pool
            .get()
            .await?
            .query_opt(sql, &[&i64::from(tmdb_id)])
            .await?
            .map(|row| row.try_get(0).map_err(StoreError::from))
            .transpose()
    }

    /// Só os ids TMDB do catálogo, separados entre filmes e séries.
    ///
    /// # Errors
    /// Falha de leitura ou id inválido no catálogo.
    pub async fn catalog_tmdb_ids(&self) -> Result<(HashSet<u32>, HashSet<u32>)> {
        let client = self.pool.get().await?;
        let mut sets = Vec::new();
        for sql in ["SELECT tmdb_id FROM movies", "SELECT tmdb_id FROM series"] {
            sets.push(
                client
                    .query(sql, &[])
                    .await?
                    .iter()
                    .map(|row| {
                        let value: i64 = row.try_get(0)?;
                        u32::try_from(value)
                            .map_err(|_| StoreError::Corrupt(format!("tmdb {value}")))
                    })
                    .collect::<Result<HashSet<_>>>()?,
            );
        }
        let series = sets.pop().unwrap_or_default();
        Ok((sets.pop().unwrap_or_default(), series))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDb;

    #[tokio::test]
    async fn preferencias_idempotentes_e_ids_do_catalogo() {
        let Some(db) = TestDb::new("descobrir_store").await else {
            return;
        };
        let store = &db.store;
        let old = "2026-01-01T00:00:00Z";
        let new = "2026-01-02T00:00:00Z";
        store
            .hide_title(DiscoverKind::Filme, 1, "Um", old)
            .await
            .unwrap();
        store
            .hide_title(DiscoverKind::Filme, 1, "Outro", new)
            .await
            .unwrap();
        store
            .hide_title(DiscoverKind::Serie, 1, "Série", new)
            .await
            .unwrap();
        let titles = store.hidden_titles().await.unwrap();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0].kind, DiscoverKind::Serie);
        assert_eq!(titles[1].title, "Um");
        assert_eq!(titles[1].hidden_at, old);
        let client = store.pool.get().await.unwrap();
        client.batch_execute("INSERT INTO movies (tmdb_id, title, path, monitored) VALUES (10, 'Um', '/filmes/Um', true); INSERT INTO series (tmdb_id, title, path, season_folder, monitor_new) VALUES (20, 'Série', '/series/Serie', true, true);").await.unwrap();
        assert_eq!(
            store.catalog_tmdb_ids().await.unwrap(),
            (HashSet::from([10]), HashSet::from([20]))
        );
        let movie_id: i64 = client
            .query_one("SELECT id FROM movies WHERE tmdb_id = 10", &[])
            .await
            .unwrap()
            .get(0);
        let series_id: i64 = client
            .query_one("SELECT id FROM series WHERE tmdb_id = 20", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            store
                .catalog_id_by_tmdb(DiscoverKind::Filme, 10)
                .await
                .unwrap(),
            Some(movie_id)
        );
        assert_eq!(
            store
                .catalog_id_by_tmdb(DiscoverKind::Serie, 20)
                .await
                .unwrap(),
            Some(series_id)
        );
        assert_eq!(
            store
                .catalog_id_by_tmdb(DiscoverKind::Serie, 10)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .catalog_id_by_tmdb(DiscoverKind::Filme, 20)
                .await
                .unwrap(),
            None
        );
        assert!(store.unhide_title(DiscoverKind::Filme, 1).await.unwrap());
        assert!(!store.unhide_title(DiscoverKind::Filme, 1).await.unwrap());
        assert_eq!(store.hidden_titles().await.unwrap().len(), 1);
        drop(client);
        db.drop().await;
    }
}

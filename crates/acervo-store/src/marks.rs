//! Marcas de "para apagar": o que o usuário escolheu tirar do disco e ainda
//! não confirmou. Marcar não apaga nada; quem apaga é o hub, ao confirmar.
//! A marca sai sozinha com o filme ou a série (chave estrangeira em
//! cascata); a de série que fica no catálogo sai por [`Store::unmark`].

use serde::Serialize;

use crate::{Result, Store, StoreError};

/// O que se marca: um filme, uma temporada ou a série inteira.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum MarkTarget {
    Movie(i64),
    /// `season` ausente é a série inteira.
    Series {
        series_id: i64,
        season: Option<u16>,
    },
}

/// Uma marca, como gravada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeletionMark {
    pub target: MarkTarget,
    /// RFC 3339.
    pub marked_at: String,
}

fn read(row: &tokio_postgres::Row) -> Result<DeletionMark> {
    let movie: Option<i64> = row.try_get(0)?;
    let series: Option<i64> = row.try_get(1)?;
    let season: Option<i32> = row.try_get(2)?;
    let target = match (movie, series) {
        (Some(id), None) => MarkTarget::Movie(id),
        (None, Some(series_id)) => MarkTarget::Series {
            series_id,
            season: season
                .map(|s| {
                    u16::try_from(s)
                        .map_err(|_| StoreError::Corrupt(format!("temporada {s} marcada")))
                })
                .transpose()?,
        },
        _ => return Err(StoreError::Corrupt("marca sem filme nem série".into())),
    };
    Ok(DeletionMark {
        target,
        marked_at: row.try_get(3)?,
    })
}

impl Store {
    /// Todas as marcas, da mais velha para a mais nova.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn deletion_marks(&self) -> Result<Vec<DeletionMark>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT movie_id, series_id, season, marked_at FROM deletion_marks ORDER BY id",
                &[],
            )
            .await?;
        rows.iter().map(read).collect()
    }

    /// Marca para apagar. Marcar de novo não muda nada. A série inteira
    /// engole as marcas das temporadas dela, e temporada de série já marcada
    /// inteira não ganha marca própria: a série inteira já a cobre. Devolve
    /// quantas marcas novas entraram.
    ///
    /// # Errors
    ///
    /// Filme ou série fora do catálogo, ou falha de escrita.
    pub async fn mark_for_deletion(&self, targets: &[MarkTarget], at: &str) -> Result<u64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut added = 0;
        for target in targets {
            added += match *target {
                MarkTarget::Movie(id) => {
                    tx.execute(
                        "INSERT INTO deletion_marks (movie_id, marked_at) VALUES ($1, $2)
                         ON CONFLICT DO NOTHING",
                        &[&id, &at],
                    )
                    .await?
                }
                MarkTarget::Series {
                    series_id,
                    season: None,
                } => {
                    tx.execute(
                        "DELETE FROM deletion_marks WHERE series_id = $1 AND season IS NOT NULL",
                        &[&series_id],
                    )
                    .await?;
                    tx.execute(
                        "INSERT INTO deletion_marks (series_id, marked_at) VALUES ($1, $2)
                         ON CONFLICT DO NOTHING",
                        &[&series_id, &at],
                    )
                    .await?
                }
                MarkTarget::Series {
                    series_id,
                    season: Some(season),
                } => {
                    tx.execute(
                        "INSERT INTO deletion_marks (series_id, season, marked_at)
                         SELECT $1, $2, $3
                         WHERE NOT EXISTS (
                             SELECT 1 FROM deletion_marks WHERE series_id = $1 AND season IS NULL
                         )
                         ON CONFLICT DO NOTHING",
                        &[&series_id, &i32::from(season), &at],
                    )
                    .await?
                }
            };
        }
        tx.commit().await?;
        Ok(added)
    }

    /// Tira as marcas. A da série inteira não leva as das temporadas, nem o
    /// contrário: cada uma sai pelo próprio alvo. Devolve quantas saíram.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn unmark(&self, targets: &[MarkTarget]) -> Result<u64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut removed = 0;
        for target in targets {
            removed += match *target {
                MarkTarget::Movie(id) => {
                    tx.execute("DELETE FROM deletion_marks WHERE movie_id = $1", &[&id])
                        .await?
                }
                MarkTarget::Series { series_id, season } => {
                    tx.execute(
                        "DELETE FROM deletion_marks
                         WHERE series_id = $1 AND season IS NOT DISTINCT FROM $2",
                        &[&series_id, &season.map(i32::from)],
                    )
                    .await?
                }
            };
        }
        tx.commit().await?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDb;

    const AT: &str = "2026-10-01T00:00:00Z";

    /// Um filme e uma série mínimos, direto no banco.
    async fn catalog(store: &Store) -> (i64, i64) {
        let client = store.pool.get().await.unwrap();
        let movie: i64 = client
            .query_one(
                "INSERT INTO movies (tmdb_id, title, path, monitored)
                 VALUES (1, 'Um', '/filmes/Um', true) RETURNING id",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let series: i64 = client
            .query_one(
                "INSERT INTO series (tmdb_id, title, path, season_folder, monitor_new)
                 VALUES (2, 'Show', '/series/Show', true, true) RETURNING id",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        (movie, series)
    }

    fn season(series_id: i64, season: u16) -> MarkTarget {
        MarkTarget::Series {
            series_id,
            season: Some(season),
        }
    }

    fn whole(series_id: i64) -> MarkTarget {
        MarkTarget::Series {
            series_id,
            season: None,
        }
    }

    fn targets(marks: &[DeletionMark]) -> Vec<MarkTarget> {
        marks.iter().map(|m| m.target).collect()
    }

    #[tokio::test]
    async fn marca_desmarca_e_a_serie_inteira_engole_as_temporadas() {
        let Some(db) = TestDb::new("marcas").await else {
            return;
        };
        let store = &db.store;
        let (movie, series) = catalog(store).await;

        let added = store
            .mark_for_deletion(
                &[
                    MarkTarget::Movie(movie),
                    season(series, 1),
                    season(series, 2),
                ],
                AT,
            )
            .await
            .unwrap();
        assert_eq!(added, 3);
        // De novo: nada muda.
        assert_eq!(
            store
                .mark_for_deletion(&[MarkTarget::Movie(movie), season(series, 1)], AT)
                .await
                .unwrap(),
            0
        );
        let marks = store.deletion_marks().await.unwrap();
        assert_eq!(
            targets(&marks),
            [
                MarkTarget::Movie(movie),
                season(series, 1),
                season(series, 2)
            ]
        );
        assert_eq!(marks[0].marked_at, AT);

        // A série inteira substitui as temporadas; temporada nova não entra.
        store.mark_for_deletion(&[whole(series)], AT).await.unwrap();
        assert_eq!(
            store
                .mark_for_deletion(&[season(series, 3)], AT)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            targets(&store.deletion_marks().await.unwrap()),
            [MarkTarget::Movie(movie), whole(series)]
        );

        // Desmarcar temporada não leva a série inteira; o alvo certo, sim.
        assert_eq!(store.unmark(&[season(series, 1)]).await.unwrap(), 0);
        assert_eq!(
            store
                .unmark(&[whole(series), MarkTarget::Movie(movie)])
                .await
                .unwrap(),
            2
        );
        assert!(store.deletion_marks().await.unwrap().is_empty());

        // A marca sai com o filme e com a série.
        store
            .mark_for_deletion(&[MarkTarget::Movie(movie), season(series, 1)], AT)
            .await
            .unwrap();
        store.delete_movie(movie).await.unwrap();
        store.delete_series(series).await.unwrap();
        assert!(store.deletion_marks().await.unwrap().is_empty());

        // Alvo fora do catálogo é erro, e nada da chamada entra.
        assert!(store.mark_for_deletion(&[whole(series)], AT).await.is_err());
        db.drop().await;
    }

    #[tokio::test]
    async fn migracao_tira_a_tarefa_de_assistidos() {
        let Some(db) = TestDb::before(
            "marcas_migra",
            "CREATE TABLE deletion_marks",
            r#"
            INSERT INTO config_sections (name, value, updated_at) VALUES
                ('tarefas', '{"intervalos": {"rss": 45, "assistidos": 15}, "search_limit": 3}',
                 '2026-09-01T00:00:00Z');
            INSERT INTO task_runs (task, started_at, finished_at, ok, summary) VALUES
                ('assistidos', '2026-09-01T00:00:00Z', '2026-09-01T00:00:01Z', true, 'nada a apagar'),
                ('limpeza', '2026-09-01T00:00:00Z', '2026-09-01T00:00:01Z', true, 'ok');
            "#,
        )
        .await
        else {
            return;
        };
        let sections = db.store.config_sections().await.unwrap();
        let (_, tasks) = sections.iter().find(|(name, _)| name == "tarefas").unwrap();
        assert_eq!(
            *tasks,
            serde_json::json!({ "intervalos": { "rss": 45 }, "search_limit": 3 })
        );
        let runs = db.store.task_runs(10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].task, "limpeza");
        db.drop().await;
    }
}

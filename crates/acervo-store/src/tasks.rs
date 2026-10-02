//! Histórico das tarefas de fundo do serviço: busca, RSS, importação,
//! metadados, limpeza.

use serde::Serialize;
use serde_json::Value;

use crate::{Result, Store};

/// Uma execução, como gravada.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskRun {
    pub id: i64,
    /// Id estável da tarefa (`busca`, `limpeza`...).
    pub task: String,
    /// RFC 3339, em UTC.
    pub started_at: String,
    pub finished_at: String,
    pub ok: bool,
    /// Uma linha, para a lista.
    pub summary: String,
    /// O relatório da execução, quando a tarefa tem um.
    pub detail: Option<Value>,
}

/// Uma execução a gravar.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTaskRun {
    pub task: String,
    pub started_at: String,
    pub finished_at: String,
    pub ok: bool,
    pub summary: String,
    pub detail: Option<Value>,
}

fn read(row: &tokio_postgres::Row) -> Result<TaskRun> {
    Ok(TaskRun {
        id: row.try_get(0)?,
        task: row.try_get(1)?,
        started_at: row.try_get(2)?,
        finished_at: row.try_get(3)?,
        ok: row.try_get(4)?,
        summary: row.try_get(5)?,
        detail: row.try_get(6)?,
    })
}

impl Store {
    /// Grava uma execução e apaga as mais velhas da mesma tarefa, para o
    /// histórico guardar só as `keep` mais recentes de cada uma: a importação,
    /// de 5 em 5 minutos, não empurra a limpeza de hora em hora para fora.
    /// Devolve o id.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn record_task_run(&self, run: &NewTaskRun, keep: i64) -> Result<i64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let id: i64 = tx
            .query_one(
                "INSERT INTO task_runs (task, started_at, finished_at, ok, summary, detail)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 RETURNING id",
                &[
                    &run.task,
                    &run.started_at,
                    &run.finished_at,
                    &run.ok,
                    &run.summary,
                    &run.detail,
                ],
            )
            .await?
            .try_get(0)?;
        // A ordem de gravação é a de término: o id basta para saber quais são
        // as mais velhas.
        tx.execute(
            "DELETE FROM task_runs WHERE task = $2 AND id <= (
                 SELECT id FROM task_runs WHERE task = $2 ORDER BY id DESC OFFSET $1 LIMIT 1
             )",
            &[&keep.max(1), &run.task],
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// As últimas `limit` execuções, da mais nova para a mais velha.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn task_runs(&self, limit: i64) -> Result<Vec<TaskRun>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, task, started_at, finished_at, ok, summary, detail
                 FROM task_runs
                 ORDER BY started_at DESC, id DESC
                 LIMIT $1",
                &[&limit],
            )
            .await?;
        rows.iter().map(read).collect()
    }

    /// A execução mais recente de cada tarefa, sem o detalhe — é o que a lista
    /// de tarefas mostra ao subir o serviço.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn last_task_runs(&self) -> Result<Vec<TaskRun>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT DISTINCT ON (task) id, task, started_at, finished_at, ok, summary,
                        NULL::jsonb
                 FROM task_runs
                 ORDER BY task, id DESC",
                &[],
            )
            .await?;
        rows.iter().map(read).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;

    use crate::testing::TestDb;
    use crate::{MIGRATIONS, Store};

    use super::*;

    fn run(task: &str, minute: u32, detail: Option<Value>) -> NewTaskRun {
        NewTaskRun {
            task: task.into(),
            started_at: format!("2026-01-01T00:{minute:02}:00Z"),
            finished_at: format!("2026-01-01T00:{minute:02}:30Z"),
            ok: true,
            summary: format!("rodada {minute}"),
            detail,
        }
    }

    #[tokio::test]
    async fn historico_guarda_so_as_mais_recentes() {
        let Some(db) = TestDb::new("tarefas").await else {
            return;
        };
        let store = &db.store;
        for minute in 0..5 {
            let task = if minute % 2 == 0 { "busca" } else { "limpeza" };
            store
                .record_task_run(&run(task, minute, None), 2)
                .await
                .unwrap();
        }
        // Duas de cada tarefa: a busca, mais frequente, não apaga a limpeza.
        let runs = store.task_runs(100).await.unwrap();
        let summaries: Vec<&str> = runs.iter().map(|r| r.summary.as_str()).collect();
        assert_eq!(summaries, ["rodada 4", "rodada 3", "rodada 2", "rodada 1"]);

        let last = store.last_task_runs().await.unwrap();
        assert_eq!(last.len(), 2);
        assert_eq!(last[0].task, "busca");
        assert_eq!(last[0].summary, "rodada 4");
        assert_eq!(last[1].summary, "rodada 3");
        assert_eq!(store.task_runs(1).await.unwrap().len(), 1);
        db.drop().await;
    }

    #[tokio::test]
    async fn migracao_cria_o_historico_de_tarefas() {
        let Some(url) = std::env::var("ACERVO_TEST_DATABASE_URL").ok() else {
            eprintln!("ACERVO_TEST_DATABASE_URL ausente: teste de banco pulado");
            return;
        };
        let schema = format!("teste_tarefas_migra_{}", std::process::id());
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        // Banco parado na versão anterior, como o de produção antes desta
        // mudança.
        let before = MIGRATIONS.len() - 1;
        let mut setup = format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema};"
        );
        for migration in &MIGRATIONS[..before] {
            setup.push_str(migration);
        }
        let _ = write!(
            setup,
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES ({before});"
        );
        client.batch_execute(&setup).await.unwrap();

        let mut config: tokio_postgres::Config = url.parse().unwrap();
        config.options(format!("-c search_path={schema}"));
        let store = Store::with_config(config).await.unwrap();
        let version: i32 = client
            .query_one(&format!("SELECT version FROM {schema}.schema_version"), &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(usize::try_from(version).unwrap(), MIGRATIONS.len());

        let detail = json!({ "acoes": [], "espaco": "0 B" });
        store
            .record_task_run(&run("limpeza", 1, Some(detail.clone())), 500)
            .await
            .unwrap();
        let runs = store.task_runs(100).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].detail, Some(detail));
        assert!(runs[0].ok);
        client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
    }
}

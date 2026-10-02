//! Registro das tarefas de fundo do serviço, como a tela "Tarefas" dos
//! gerenciadores as mostra: cada uma com intervalo, estado, última e próxima
//! execução, e um "rodar agora".
//!
//! Cada tarefa tem um laço próprio que espera o que vier primeiro — a hora
//! agendada ou um pedido de rodar agora — e roda uma execução por vez. O
//! resultado vai para o histórico no banco, que guarda as últimas
//! [`KEEP`] execuções de cada tarefa. Tarefa nova é uma implementação de [`Job`] e uma
//! entrada em [`service`].

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use acervo_api::Catalog;
use acervo_store::NewTaskRun;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::config::Config;
use crate::decide::{Progress, now_rfc3339};
use crate::serve::Database;

/// Quantas execuções de cada tarefa o histórico guarda; as mais velhas saem
/// ao gravar.
const KEEP: i64 = 100;
/// Quantas a tela de histórico mostra.
const SHOWN: i64 = 100;
/// Teto do resumo: uma linha na tela, não um log.
const SUMMARY_CHARS: usize = 300;

/// Id da busca dos que faltam: o botão da tela de filmes dispara esta tarefa.
pub const BUSCA: &str = "busca";

/// Por que uma execução começou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Pela hora agendada.
    Scheduled,
    /// Por "rodar agora".
    Manual,
}

/// Como uma execução terminou.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    /// Uma linha, para a lista.
    pub summary: String,
    /// O relatório completo, quando a tarefa tem um.
    pub detail: Option<Value>,
}

impl Outcome {
    fn new(ok: bool, summary: impl Into<String>) -> Self {
        Self {
            ok,
            summary: summary.into(),
            detail: None,
        }
    }
}

/// O trabalho de uma tarefa.
#[async_trait]
pub trait Job: Send + Sync + std::fmt::Debug {
    /// Uma execução. Erro vira execução com falha no histórico, com a
    /// mensagem como resumo.
    async fn run(&self, trigger: Trigger) -> Result<Outcome>;

    /// Andamento da execução em curso, como "3 de 10".
    fn progress(&self) -> Option<String> {
        None
    }
}

/// Quando a primeira execução agendada acontece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum First {
    /// Assim que o agendamento começa.
    Now,
    /// Depois de um intervalo inteiro.
    AfterInterval,
}

/// Uma tarefa registrada.
#[derive(Debug)]
pub struct Task {
    /// Estável: é o que a URL e o histórico guardam.
    pub id: &'static str,
    /// Para a tela.
    pub name: &'static str,
    /// Zero desliga o agendamento; "rodar agora" continua valendo.
    pub interval: Duration,
    pub first: First,
    pub job: Arc<dyn Job>,
}

#[derive(Debug, Clone)]
struct LastRun {
    started: String,
    finished: String,
    duration_ms: u64,
    ok: bool,
    summary: String,
}

#[derive(Debug, Default)]
struct State {
    /// Tomado por quem dispara — o laço ou "rodar agora" —, solto no fim.
    running: bool,
    /// "Rodar agora" pediu e o laço ainda não atendeu.
    requested: bool,
    started: Option<String>,
    next: Option<String>,
    last: Option<LastRun>,
}

#[derive(Debug)]
struct Slot {
    task: Task,
    state: Mutex<State>,
    wake: Notify,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Estado envenenado ainda é estado: só flags e textos.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn view(&self) -> Value {
        let state = self.lock();
        json!({
            "id": self.task.id,
            "nome": self.task.name,
            "intervalo_minutos": self.task.interval.as_secs() / 60,
            "rodando": state.running,
            "iniciada_em": state.started,
            "andamento": if state.running { self.task.job.progress() } else { None },
            "proxima": if state.running { None } else { state.next.clone() },
            "ultima": state.last.as_ref().map(|last| json!({
                "inicio": last.started,
                "fim": last.finished,
                "duracao_ms": last.duration_ms,
                "ok": last.ok,
                "resumo": last.summary,
            })),
        })
    }
}

/// O registro. Sem tarefas ([`Default`]), responde vazio.
#[derive(Debug, Default)]
pub struct Tasks {
    slots: Vec<Arc<Slot>>,
    database: Database,
}

impl Tasks {
    #[must_use]
    pub fn new(database: Database, tasks: Vec<Task>) -> Self {
        Self {
            slots: tasks
                .into_iter()
                .map(|task| {
                    Arc::new(Slot {
                        task,
                        state: Mutex::default(),
                        wake: Notify::new(),
                    })
                })
                .collect(),
            database,
        }
    }

    fn slot(&self, id: &str) -> Option<&Arc<Slot>> {
        self.slots.iter().find(|slot| slot.task.id == id)
    }

    /// Começa o agendamento: lê do histórico a última execução de cada
    /// tarefa, para a tela não mostrar "nunca" depois de um reinício, e sobe
    /// um laço por tarefa.
    pub async fn start(self: &Arc<Self>) {
        self.load_last().await;
        for slot in &self.slots {
            tokio::spawn(Arc::clone(self).schedule(Arc::clone(slot)));
        }
    }

    async fn load_last(&self) {
        let Ok(store) = self.database.get() else {
            return;
        };
        let runs = match store.last_task_runs().await {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!("histórico das tarefas ilegível: {error}");
                return;
            }
        };
        for run in runs {
            if let Some(slot) = self.slot(&run.task) {
                slot.lock().last = Some(LastRun {
                    duration_ms: elapsed_ms(&run.started_at, &run.finished_at),
                    started: run.started_at,
                    finished: run.finished_at,
                    ok: run.ok,
                    summary: run.summary,
                });
            }
        }
    }

    /// "Rodar agora". `Some(false)` se a tarefa já está rodando — não
    /// dispara outra —, `None` se ela não existe.
    pub fn run_now(&self, id: &str) -> Option<bool> {
        let slot = self.slot(id)?;
        let mut state = slot.lock();
        if state.running {
            return Some(false);
        }
        // Toma a vez aqui, e não no laço: um segundo pedido logo em seguida
        // já encontra a tarefa rodando.
        state.running = true;
        state.requested = true;
        drop(state);
        slot.wake.notify_one();
        Some(true)
    }

    /// Se a tarefa está rodando (ou pedida, prestes a rodar).
    pub fn is_running(&self, id: &str) -> bool {
        self.slot(id).is_some_and(|slot| slot.lock().running)
    }

    /// As tarefas, na ordem do registro, com o estado de cada uma.
    pub fn view(&self) -> Value {
        Value::Array(self.slots.iter().map(|slot| slot.view()).collect())
    }

    /// Uma tarefa, como em [`Tasks::view`].
    pub fn view_one(&self, id: &str) -> Option<Value> {
        self.slot(id).map(|slot| slot.view())
    }

    /// As últimas execuções, da mais nova para a mais velha.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn history(&self) -> Result<Value, String> {
        let runs = self
            .database
            .get()?
            .task_runs(SHOWN)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Value::Array(
            runs.into_iter()
                .map(|run| {
                    let name = self.slot(&run.task).map_or("", |slot| slot.task.name);
                    json!({
                        "id": run.id,
                        "tarefa": run.task,
                        "nome": name,
                        "inicio": run.started_at,
                        "fim": run.finished_at,
                        "duracao_ms": elapsed_ms(&run.started_at, &run.finished_at),
                        "ok": run.ok,
                        "resumo": run.summary,
                        "detalhe": run.detail,
                    })
                })
                .collect(),
        ))
    }

    /// O laço de uma tarefa: espera a hora agendada ou um pedido, roda,
    /// reagenda a partir do início da execução.
    async fn schedule(self: Arc<Self>, slot: Arc<Slot>) {
        let every = slot.task.interval;
        let mut next = (!every.is_zero()).then(|| match slot.task.first {
            First::Now => Instant::now(),
            First::AfterInterval => Instant::now() + every,
        });
        loop {
            slot.lock().next = next.map(wall_clock);
            let timer = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let woken = tokio::select! {
                () = timer => false,
                () = slot.wake.notified() => true,
            };
            let trigger = {
                let mut state = slot.lock();
                if state.requested {
                    state.requested = false;
                    Trigger::Manual
                } else if woken {
                    // Aviso que sobrou de um pedido já atendido junto com a
                    // hora agendada.
                    continue;
                } else {
                    state.running = true;
                    Trigger::Scheduled
                }
            };
            let started = Instant::now();
            self.execute(&slot, trigger).await;
            next = (!every.is_zero()).then(|| started + every);
        }
    }

    async fn execute(&self, slot: &Slot, trigger: Trigger) {
        let started_at = now_rfc3339();
        slot.lock().started = Some(started_at.clone());
        let clock = Instant::now();
        let job = Arc::clone(&slot.task.job);
        // Numa task à parte: pânico no trabalho vira execução com falha, e o
        // laço segue agendando.
        let outcome = match tokio::spawn(async move { job.run(trigger).await }).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => Outcome::new(false, format!("{error:#}")),
            Err(_) => Outcome::new(false, "a tarefa parou com pânico"),
        };
        let summary: String = outcome.summary.chars().take(SUMMARY_CHARS).collect();
        let finished_at = now_rfc3339();
        let duration_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        if outcome.ok {
            tracing::info!(tarefa = slot.task.id, ?trigger, "{summary}");
        } else {
            tracing::warn!(tarefa = slot.task.id, ?trigger, "{summary}");
        }
        // Sem banco não há histórico; a tela ainda mostra a última execução.
        if let Ok(store) = self.database.get() {
            let run = NewTaskRun {
                task: slot.task.id.to_owned(),
                started_at: started_at.clone(),
                finished_at: finished_at.clone(),
                ok: outcome.ok,
                summary: summary.clone(),
                detail: outcome.detail,
            };
            if let Err(error) = store.record_task_run(&run, KEEP).await {
                tracing::warn!(tarefa = slot.task.id, "execução fora do histórico: {error}");
            }
        }
        let mut state = slot.lock();
        state.running = false;
        state.started = None;
        state.last = Some(LastRun {
            started: started_at,
            finished: finished_at,
            duration_ms,
            ok: outcome.ok,
            summary,
        });
    }
}

/// A hora de relógio de um instante do agendamento.
fn wall_clock(at: Instant) -> String {
    let ahead = at.saturating_duration_since(Instant::now());
    (OffsetDateTime::now_utc() + ahead)
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn elapsed_ms(started: &str, finished: &str) -> u64 {
    match (
        OffsetDateTime::parse(started, &Rfc3339),
        OffsetDateTime::parse(finished, &Rfc3339),
    ) {
        (Ok(started), Ok(finished)) => {
            u64::try_from((finished - started).whole_milliseconds()).unwrap_or(0)
        }
        _ => 0,
    }
}

/// "1 importado", "3 importados".
fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

// --- As tarefas do serviço --------------------------------------------------

/// As tarefas de `serve`, com os intervalos da configuração. Cada uma só
/// entra se o serviço tem o que ela precisa.
#[must_use]
pub fn service(
    config: &Arc<Config>,
    database: &Database,
    catalog: &Catalog,
    progress: &Arc<Progress>,
) -> Vec<Task> {
    let minutes = |m: u64| Duration::from_secs(m * 60);
    let with_database = config.database.is_some();
    let with_client = with_database && config.qbittorrent.is_some();
    let server = config.server.as_ref();
    let mut tasks = Vec::new();
    if with_database {
        tasks.push(Task {
            id: BUSCA,
            name: "Busca dos que faltam",
            // Sem intervalo, só pelo botão.
            interval: minutes(server.and_then(|s| s.search_interval_minutes).unwrap_or(0)),
            // Espera um ciclo para não somar a busca à subida do serviço.
            first: First::AfterInterval,
            job: Arc::new(Missing {
                config: Arc::clone(config),
                database: database.clone(),
                catalog: catalog.clone(),
                progress: Arc::clone(progress),
                limit: server.map_or(5, |s| s.search_limit),
            }),
        });
    }
    if with_client {
        tasks.push(Task {
            id: "rss",
            name: "Sincronização de RSS",
            interval: minutes(config.movies.rss_interval_minutes.max(5)),
            first: First::Now,
            job: Arc::new(Rss {
                config: Arc::clone(config),
                database: database.clone(),
                catalog: catalog.clone(),
            }),
        });
        tasks.push(Task {
            id: "importacao",
            name: "Importação de downloads",
            interval: minutes(config.movies.import_interval_minutes),
            first: First::Now,
            job: Arc::new(Import {
                config: Arc::clone(config),
                database: database.clone(),
                catalog: catalog.clone(),
            }),
        });
    }
    if with_database {
        tasks.push(Task {
            id: "metadados",
            name: "Atualização de metadados",
            interval: Duration::from_secs(6 * 3600),
            first: First::Now,
            job: Arc::new(Metadata {
                config: Arc::clone(config),
                database: database.clone(),
            }),
        });
    }
    match config.janitor() {
        Ok(_) => tasks.push(Task {
            id: "limpeza",
            name: "Limpeza",
            interval: minutes(server.map_or(60, |s| s.cleanup_interval_minutes)),
            // Um ciclo avança strikes: reiniciar o serviço não pode valer
            // como ciclo.
            first: First::AfterInterval,
            job: Arc::new(Cleanup {
                config: Arc::clone(config),
            }),
        }),
        Err(error) => tracing::info!("limpeza fora das tarefas: {error:#}"),
    }
    tasks
}

fn store(database: &Database) -> Result<&acervo_store::Store> {
    database.get().map_err(anyhow::Error::msg)
}

/// Busca dos que faltam: a agendada pega `limit` filmes; "rodar agora", todos.
#[derive(Debug)]
struct Missing {
    config: Arc<Config>,
    database: Database,
    catalog: Catalog,
    progress: Arc<Progress>,
    limit: usize,
}

#[async_trait]
impl Job for Missing {
    async fn run(&self, trigger: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        // O registro já garante uma execução por vez; a vez no `Progress` é
        // o que alimenta o andamento.
        let turn = self
            .progress
            .start()
            .context("já há uma busca dos que faltam rodando")?;
        let limit = (trigger == Trigger::Scheduled).then_some(self.limit);
        let lines = crate::decide::search(&self.config, store, &self.catalog, limit, &turn).await?;
        if lines.is_empty() {
            return Ok(Outcome::new(true, "nenhum filme faltando"));
        }
        let picked = lines.iter().filter(|l| l.escolhido.is_some()).count();
        let failed = lines.iter().filter(|l| l.erro.is_some()).count();
        let mut summary = format!(
            "{}, {}",
            count(lines.len(), "buscado", "buscados"),
            count(picked, "escolhido", "escolhidos")
        );
        if failed > 0 {
            summary = format!("{summary}, {failed} com erro");
        }
        Ok(Outcome::new(true, summary))
    }

    fn progress(&self) -> Option<String> {
        let view = self.progress.view();
        (view.rodando && view.total > 0).then(|| format!("{} de {}", view.buscados, view.total))
    }
}

/// Sincronização de RSS: pega o que serve à biblioteca.
#[derive(Debug)]
struct Rss {
    config: Arc<Config>,
    database: Database,
    catalog: Catalog,
}

#[async_trait]
impl Job for Rss {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let grabs = crate::automatic::rss(&self.config, store, &self.catalog, true).await?;
        for grab in &grabs {
            tracing::info!(
                filme = grab.filme,
                release = grab.release,
                erro = grab.erro,
                "RSS pegou"
            );
        }
        let failed = grabs.iter().filter(|g| g.erro.is_some()).count();
        let summary = match (grabs.len(), failed) {
            (0, _) => "nada novo serve à biblioteca".to_owned(),
            (n, 0) => count(n, "pego", "pegos"),
            (n, failed) => format!("{}, {failed} falharam", count(n, "pego", "pegos")),
        };
        Ok(Outcome::new(failed == 0, summary))
    }
}

/// Importa os downloads do acervo que terminaram.
#[derive(Debug)]
struct Import {
    config: Arc<Config>,
    database: Database,
    catalog: Catalog,
}

#[async_trait]
impl Job for Import {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let lines =
            crate::grab::import_downloads(&self.config, store, Some(&self.catalog), true).await?;
        for line in lines.iter().filter(|l| l.estado != "baixando") {
            tracing::info!(
                filme = line.filme,
                estado = line.estado,
                destino = line.destino,
                detalhe = line.detalhe,
                "importação de download"
            );
        }
        let by = |estado: &str| lines.iter().filter(|l| l.estado == estado).count();
        let failed = by("falhou");
        let parts: Vec<String> = [
            (by("importado"), "importado", "importados"),
            (failed, "falhou", "falharam"),
            (by("atencao"), "travado", "travados"),
            (by("baixando"), "baixando", "baixando"),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, one, many)| count(n, one, many))
        .collect();
        let summary = if parts.is_empty() {
            "nenhum download do acervo".to_owned()
        } else {
            parts.join(", ")
        };
        Ok(Outcome::new(failed == 0, summary))
    }
}

/// Mantém os metadados em dia: os filmes conferidos há mais de um dia. Sem
/// chave do TMDB, não há o que fazer.
#[derive(Debug)]
struct Metadata {
    config: Arc<Config>,
    database: Database,
}

#[async_trait]
impl Job for Metadata {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let Some(tmdb) = crate::metadata::tmdb(&self.config, store).await? else {
            return Ok(Outcome::new(true, "sem chave do TMDB; nada a fazer"));
        };
        let report = crate::library::refresh(store, &tmdb, 24).await?;
        let summary = format!(
            "{}, {}, {}",
            count(report.conferidos, "conferido", "conferidos"),
            count(report.atualizados.len(), "atualizado", "atualizados"),
            count(report.falhas.len(), "falha", "falhas"),
        );
        Ok(Outcome::new(report.falhas.is_empty(), summary))
    }
}

/// O ciclo de limpeza, aplicado. O relatório vira o detalhe da execução.
#[derive(Debug)]
struct Cleanup {
    config: Arc<Config>,
}

#[async_trait]
impl Job for Cleanup {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let report = crate::cycle::run(&self.config, false).await?;
        let (ok, summary) = report.summary();
        Ok(Outcome {
            ok,
            summary,
            detail: Some(serde_json::to_value(&report)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Um trabalho que conta as execuções e só termina quando o teste solta.
    #[derive(Debug, Default)]
    struct Gate {
        runs: AtomicUsize,
        triggers: Mutex<Vec<Trigger>>,
        started: Notify,
        release: Notify,
    }

    #[async_trait]
    impl Job for Gate {
        async fn run(&self, trigger: Trigger) -> Result<Outcome> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.triggers.lock().unwrap().push(trigger);
            self.started.notify_one();
            self.release.notified().await;
            Ok(Outcome::new(true, "feito"))
        }
    }

    fn registry(database: Database, interval: Duration, first: First) -> (Arc<Tasks>, Arc<Gate>) {
        let gate = Arc::new(Gate::default());
        let tasks = Arc::new(Tasks::new(
            database,
            vec![Task {
                id: "teste",
                name: "Teste",
                interval,
                first,
                job: Arc::clone(&gate) as Arc<dyn Job>,
            }],
        ));
        (tasks, gate)
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("esperando: {what}");
    }

    #[tokio::test]
    async fn rodar_agora_dispara_e_so_uma_execucao_por_vez() {
        // Intervalo zero: desligada no agendamento, viva no "rodar agora".
        let (tasks, gate) = registry(Database::default(), Duration::ZERO, First::Now);
        tasks.start().await;
        assert_eq!(tasks.run_now("outra"), None);
        assert_eq!(tasks.view()[0]["proxima"], Value::Null);

        assert_eq!(tasks.run_now("teste"), Some(true));
        gate.started.notified().await;
        assert!(tasks.is_running("teste"));
        // Rodando: o segundo pedido não dispara outra.
        assert_eq!(tasks.run_now("teste"), Some(false));
        assert_eq!(tasks.view()[0]["rodando"], true);

        gate.release.notify_one();
        until("fim da execução", || !tasks.is_running("teste")).await;
        let view = tasks.view_one("teste").unwrap();
        assert_eq!(view["ultima"]["ok"], true);
        assert_eq!(view["ultima"]["resumo"], "feito");

        // Livre de novo, dispara de novo.
        assert_eq!(tasks.run_now("teste"), Some(true));
        gate.started.notified().await;
        gate.release.notify_one();
        until("segunda execução", || !tasks.is_running("teste")).await;
        assert_eq!(gate.runs.load(Ordering::SeqCst), 2);
        assert_eq!(
            *gate.triggers.lock().unwrap(),
            [Trigger::Manual, Trigger::Manual]
        );
    }

    #[tokio::test]
    async fn agenda_pelo_intervalo_e_respeita_a_primeira_execucao() {
        let (tasks, gate) = registry(
            Database::default(),
            Duration::from_millis(150),
            First::AfterInterval,
        );
        tasks.start().await;
        until("próxima execução agendada", || {
            tasks.view()[0]["proxima"].is_string()
        })
        .await;
        // Espera um intervalo antes da primeira.
        assert_eq!(gate.runs.load(Ordering::SeqCst), 0);
        gate.started.notified().await;
        assert_eq!(*gate.triggers.lock().unwrap(), [Trigger::Scheduled]);
        // Agendada rodando: "rodar agora" não dispara outra.
        assert_eq!(tasks.run_now("teste"), Some(false));
        gate.release.notify_one();
        gate.started.notified().await;
        gate.release.notify_one();
        assert_eq!(gate.runs.load(Ordering::SeqCst), 2);
    }

    #[derive(Debug)]
    struct Fails;

    #[async_trait]
    impl Job for Fails {
        async fn run(&self, _: Trigger) -> Result<Outcome> {
            anyhow::bail!("cliente de download fora do ar")
        }
    }

    #[tokio::test]
    async fn historico_grava_cada_execucao_e_poda_as_velhas() {
        let Some(db) = acervo_store::testing::TestDb::new("registro").await else {
            return;
        };
        let database = Database::connected(db.store.clone());
        let tasks = Arc::new(Tasks::new(
            database,
            vec![Task {
                id: "falha",
                name: "Falha",
                interval: Duration::ZERO,
                first: First::Now,
                job: Arc::new(Fails),
            }],
        ));
        tasks.start().await;
        for _ in 0..3 {
            assert_eq!(tasks.run_now("falha"), Some(true));
            until("fim da execução", || !tasks.is_running("falha")).await;
        }
        let history = tasks.history().await.unwrap();
        let runs = history.as_array().unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0]["tarefa"], "falha");
        assert_eq!(runs[0]["nome"], "Falha");
        assert_eq!(runs[0]["ok"], false);
        assert_eq!(runs[0]["resumo"], "cliente de download fora do ar");
        assert!(runs[0]["id"].as_i64() > runs[2]["id"].as_i64());

        // A poda guarda só as mais recentes.
        let extra = NewTaskRun {
            task: "falha".into(),
            started_at: now_rfc3339(),
            finished_at: now_rfc3339(),
            ok: true,
            summary: "a mais nova".into(),
            detail: None,
        };
        db.store.record_task_run(&extra, 2).await.unwrap();
        let history = tasks.history().await.unwrap();
        assert_eq!(history.as_array().unwrap().len(), 2);

        // Ao subir de novo, a última execução vem do histórico.
        let again = Tasks::new(
            Database::connected(db.store.clone()),
            vec![Task {
                id: "falha",
                name: "Falha",
                interval: Duration::ZERO,
                first: First::Now,
                job: Arc::new(Fails),
            }],
        );
        again.load_last().await;
        assert_eq!(again.view()[0]["ultima"]["resumo"], "a mais nova");
        db.drop().await;
    }
}

//! Registro das tarefas de fundo do serviço, como a tela "Tarefas" as
//! mostra: cada uma com intervalo, estado, última e próxima execução, e um
//! "rodar agora".
//!
//! Cada tarefa tem um laço próprio que espera o que vier primeiro — a hora
//! agendada ou um pedido de rodar agora — e roda uma execução por vez. O
//! intervalo é fixo para cada tarefa. O
//! resultado vai para o histórico no banco, que guarda as últimas
//! [`KEEP`] execuções de cada tarefa. Tarefa nova é uma implementação de [`Job`] e uma
//! entrada em [`service`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use acervo_api::Catalog;
use acervo_clients::jellyfin::JellyfinClient;
use acervo_store::NewTaskRun;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

use crate::decide::{Progress, now_rfc3339};
use crate::events::{Alert, Change, notify_alert};
use crate::health::{TASK_FAILURES, cleanup_message, task_message, track_streak};
use crate::serve::Database;
use crate::settings::Settings;

/// Quantas execuções de cada tarefa o histórico guarda; as mais velhas saem
/// ao gravar.
const KEEP: i64 = 100;
/// Quantas a tela de histórico mostra.
const SHOWN: i64 = 100;
/// Teto do resumo: uma linha na tela, não um log.
const SUMMARY_CHARS: usize = 300;
/// Quanto uma execução pode levar. Passou disso, algo travou (um banco ou um
/// serviço que não responde e não desiste): a execução é dada como falha e
/// a tarefa segue agendada. Generoso: a busca de todos os que faltam, pelo
/// botão, leva bem menos.
const CEILING: Duration = Duration::from_secs(2 * 60 * 60);

/// Maior atraso aleatório da primeira execução das tarefas que começam na
/// subida: sem ele, todas batem juntas no banco, nos indexadores e no cliente
/// de download logo depois de um reinício.
const STAGGER: Duration = Duration::from_secs(90);
/// De quanto em quanto tempo uma tarefa que espera o cliente de download
/// responder tenta de novo.
const READY_RETRY: Duration = Duration::from_secs(15);

/// Aviso de desligamento. Clones veem o mesmo estado.
#[derive(Debug, Clone)]
pub struct Cancel(Arc<watch::Sender<bool>>);

impl Default for Cancel {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl Cancel {
    /// Pede para parar. Vale para sempre.
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Espera o pedido; volta já se ele veio.
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        // O remetente vive em `self`: o canal não fecha.
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

/// Um atraso aleatório de zero até `max`. Sem gerador de números aleatórios
/// no projeto, a semente aleatória de cada `RandomState` serve: aqui só
/// importa espalhar, não imprevisibilidade.
fn jitter(max: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let millis = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    if millis == 0 {
        return Duration::ZERO;
    }
    let roll = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    Duration::from_millis(roll % (millis + 1))
}

/// Id da busca dos que faltam: o botão da tela de filmes dispara esta tarefa.
pub const BUSCA: &str = "busca";
/// Id da tarefa que baixa a numeração de cena (XEM).
pub const CENA: &str = "cena";
/// Id da conferência do disco: o que sumiu da biblioteca volta pelo mesmo
/// torrent.
pub const DISCO: &str = "disco";

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

    /// Por que a tarefa não tem como rodar com a configuração de agora —
    /// sem cliente de download, sem Jellyfin. Enquanto houver motivo, ela
    /// sai do agendamento; "rodar agora" ainda tenta, e falha com ele.
    fn unavailable(&self) -> Option<String> {
        None
    }

    /// Se a primeira execução agendada, na subida, já pode começar. Enquanto
    /// não, o agendador tenta de novo a cada poucos segundos. "Rodar agora"
    /// não espera por isto.
    async fn ready(&self) -> bool {
        true
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
pub struct Task {
    /// Estável: é o que a URL e o histórico guardam.
    pub id: &'static str,
    /// Para a tela.
    pub name: &'static str,
    /// Intervalo fixo. Zero desliga o
    /// agendamento; "rodar agora" continua valendo.
    pub interval: Duration,
    pub first: First,
    pub job: Arc<dyn Job>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Task")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("interval", &self.interval)
            .field("first", &self.first)
            .field("job", &self.job)
            .finish()
    }
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
    /// Uma execução em curso, de fato: `running` também vale entre o pedido
    /// e o início. É o que o desligamento espera.
    executing: bool,
    /// Falhas seguidas, para o aviso.
    failures: u32,
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

    /// O intervalo que vale para agendar: zero se a tarefa não tem como
    /// rodar agora.
    fn every(&self) -> Duration {
        if self.task.job.unavailable().is_some() {
            Duration::ZERO
        } else {
            self.task.interval
        }
    }

    fn view(&self) -> Value {
        let unavailable = self.task.job.unavailable();
        let state = self.lock();
        json!({
            "id": self.task.id,
            "nome": self.task.name,
            "intervalo_minutos": self.task.interval.as_secs() / 60,
            "indisponivel": unavailable,
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
#[derive(Debug)]
pub struct Tasks {
    slots: Vec<Arc<Slot>>,
    database: Database,
    /// [`CEILING`]; menor nos testes.
    ceiling: Duration,
    /// [`STAGGER`]; menor nos testes.
    stagger: Duration,
    cancel: Cancel,
    /// Quando o desligamento começou: dali conta a espera das execuções.
    stopped_at: Mutex<Option<Instant>>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self::new(Database::default(), Vec::new())
    }
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
            ceiling: CEILING,
            stagger: STAGGER,
            cancel: Cancel::default(),
            stopped_at: Mutex::new(None),
        }
    }

    /// O aviso de desligamento, para quem mais precisa parar com as tarefas.
    #[must_use]
    pub fn cancel_token(&self) -> Cancel {
        self.cancel.clone()
    }

    /// Começa o desligamento: nenhuma execução nova é agendada nem aceita.
    /// As que estão rodando seguem; [`Tasks::drain`] espera por elas.
    pub fn stop(&self) {
        self.stopped_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Instant::now);
        self.cancel.cancel();
    }

    /// Espera as execuções em curso terminarem, até `grace` depois do
    /// [`Tasks::stop`]. Devolve quantas ainda rodavam ao desistir.
    pub async fn drain(&self, grace: Duration) -> usize {
        let started = (*self
            .stopped_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
        .unwrap_or_else(Instant::now);
        let deadline = started + grace;
        loop {
            let busy = self.slots.iter().filter(|s| s.lock().executing).count();
            if busy == 0 || Instant::now() >= deadline {
                return busy;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
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

    /// Mudança de configuração pode liberar ou bloquear uma dependência,
    /// mesmo com intervalos fixos. O aviso faz os laços recalcularem a próxima.
    pub fn watch_settings(
        self: &Arc<Self>,
        mut changes: watch::Receiver<Arc<crate::config::Config>>,
    ) {
        let tasks = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    update = changes.changed() => {
                        if update.is_err() {
                            return;
                        }
                        for slot in &tasks.slots {
                            slot.wake.notify_one();
                        }
                    }
                    () = tasks.cancel.cancelled() => return,
                }
            }
        });
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
        // Desligando, não começa nada novo.
        if self.cancel.is_cancelled() {
            return Some(false);
        }
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
    /// reagenda a partir do início da execução. Aviso sem pedido recalcula
    /// a disponibilidade antes de voltar a esperar.
    async fn schedule(self: Arc<Self>, slot: Arc<Slot>) {
        // De onde se conta o intervalo: a subida, depois o início da última
        // execução. As que começam na subida largam espalhadas.
        let mut anchor = Instant::now();
        if slot.task.first == First::Now {
            anchor += jitter(self.stagger);
        }
        let mut ran = false;
        loop {
            if self.cancel.is_cancelled() {
                return;
            }
            let every = slot.every();
            let next = (!every.is_zero()).then(|| {
                if !ran && slot.task.first == First::Now {
                    anchor
                } else {
                    anchor + every
                }
            });
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
                () = self.cancel.cancelled() => return,
            };
            // A primeira execução da subida espera o que ela precisa
            // responder (o cliente de download, na importação).
            if !woken && !ran && slot.task.first == First::Now && !slot.lock().requested {
                let ready = tokio::select! {
                    ready = slot.task.job.ready() => ready,
                    () = self.cancel.cancelled() => return,
                };
                if !ready {
                    tracing::info!(
                        tarefa = slot.task.id,
                        "o serviço de que depende não responde; a primeira execução espera"
                    );
                    anchor = Instant::now() + READY_RETRY;
                    continue;
                }
            }
            if self.cancel.is_cancelled() {
                return;
            }
            let trigger = {
                let mut state = slot.lock();
                if state.requested {
                    state.requested = false;
                    state.executing = true;
                    Trigger::Manual
                } else if woken {
                    // Configuração mudou, ou sobrou aviso de um pedido atendido.
                    // Recalcula a disponibilidade sem disparar execução manual.
                    continue;
                } else {
                    state.running = true;
                    state.executing = true;
                    Trigger::Scheduled
                }
            };
            anchor = Instant::now();
            ran = true;
            self.execute(&slot, trigger).await;
        }
    }

    async fn execute(&self, slot: &Slot, trigger: Trigger) {
        let started_at = now_rfc3339();
        slot.lock().started = Some(started_at.clone());
        let clock = Instant::now();
        let job = Arc::clone(&slot.task.job);
        // Numa task à parte: pânico no trabalho vira execução com falha, e o
        // laço segue agendando. Travada além do teto, é abortada.
        let mut handle = tokio::spawn(async move { job.run(trigger).await });
        let outcome = match tokio::time::timeout(self.ceiling, &mut handle).await {
            Ok(Ok(Ok(outcome))) => outcome,
            Ok(Ok(Err(error))) => Outcome::new(false, format!("{error:#}")),
            Ok(Err(_)) => Outcome::new(false, "a tarefa parou com pânico"),
            Err(_) => {
                handle.abort();
                Outcome::new(
                    false,
                    format!(
                        "travada: passou de {} min sem terminar; dada como falha",
                        self.ceiling.as_secs() / 60
                    ),
                )
            }
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
        let (failures, change) = {
            let mut state = slot.lock();
            state.running = false;
            state.executing = false;
            state.started = None;
            state.last = Some(LastRun {
                started: started_at,
                finished: finished_at,
                duration_ms,
                ok: outcome.ok,
                summary: summary.clone(),
            });
            let (failures, change) = track_streak(state.failures, outcome.ok, TASK_FAILURES);
            state.failures = failures;
            (failures, change)
        };
        if let Some(change) = change
            && let Ok(store) = self.database.get()
        {
            let (title, body) = task_message(change, slot.task.name, failures, &summary);
            notify_alert(store, Alert::Task, title, body).await;
        }
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

/// O intervalo fixo de uma tarefa.
fn interval(id: &'static str) -> Duration {
    let minutes = crate::config::TASK_DEFAULTS
        .iter()
        .find(|(name, _)| *name == id)
        .map_or(0, |(_, minutes)| *minutes);
    Duration::from_secs(minutes * 60)
}

/// As tarefas de `serve`. Todas entram; a que não tem o que precisa na
/// configuração de agora fica fora do agendamento até ter.
#[must_use]
pub fn service(
    settings: &Arc<Settings>,
    database: &Database,
    catalog: &Catalog,
    progress: &Arc<Progress>,
) -> Vec<Task> {
    vec![
        Task {
            id: BUSCA,
            name: "Busca dos que faltam",
            interval: interval(BUSCA),
            // Espera um ciclo para não somar a busca à subida do serviço.
            first: First::AfterInterval,
            job: Arc::new(Missing {
                settings: Arc::clone(settings),
                database: database.clone(),
                catalog: catalog.clone(),
                progress: Arc::clone(progress),
            }),
        },
        Task {
            id: "rss",
            name: "Sincronização de RSS",
            interval: interval("rss"),
            first: First::Now,
            job: Arc::new(Rss {
                settings: Arc::clone(settings),
                database: database.clone(),
                catalog: catalog.clone(),
            }),
        },
        Task {
            id: "importacao",
            name: "Importação de downloads",
            interval: interval("importacao"),
            first: First::Now,
            job: Arc::new(Import {
                settings: Arc::clone(settings),
                database: database.clone(),
                catalog: catalog.clone(),
                disk: tokio::sync::Mutex::default(),
            }),
        },
        Task {
            id: "metadados",
            name: "Atualização de metadados",
            interval: interval("metadados"),
            first: First::Now,
            job: Arc::new(Metadata {
                database: database.clone(),
            }),
        },
        Task {
            id: "limpeza",
            name: "Limpeza",
            interval: interval("limpeza"),
            // Um ciclo avança strikes: reiniciar o serviço não pode valer
            // como ciclo.
            first: First::AfterInterval,
            job: Arc::new(Cleanup {
                settings: Arc::clone(settings),
                database: database.clone(),
                pruned: tokio::sync::Mutex::default(),
                aborted: AtomicBool::new(false),
            }),
        },
        Task {
            id: CENA,
            name: "Numeração de cena (XEM)",
            interval: interval(CENA),
            first: First::Now,
            job: Arc::new(Scene {
                database: database.clone(),
            }),
        },
        Task {
            id: DISCO,
            name: "Conferência do disco",
            interval: interval(DISCO),
            // Não soma a leitura do catálogo inteiro à subida do serviço; e
            // um disco que demora a montar não pode ser lido como vazio.
            first: First::AfterInterval,
            job: Arc::new(DiskHeal {
                settings: Arc::clone(settings),
                database: database.clone(),
            }),
        },
    ]
}

/// O resumo de uma tarefa que faz filmes e depois séries. Sem parte de
/// séries, é o resultado dos filmes como sempre foi; com ela, as duas
/// partes, e erro de uma não esconde a outra.
fn with_series(movies: Result<String>, series: Option<Result<String>>) -> Result<Outcome> {
    match (movies, series) {
        (movies, None) => movies.map(|summary| Outcome::new(true, summary)),
        (Ok(movies), Some(Ok(series))) => {
            Ok(Outcome::new(true, format!("{movies}; séries: {series}")))
        }
        (Err(movies), Some(Ok(series))) => Ok(Outcome::new(
            false,
            format!("filmes: {movies:#}; séries: {series}"),
        )),
        (Ok(movies), Some(Err(series))) => {
            Ok(Outcome::new(false, format!("{movies}; séries: {series:#}")))
        }
        (Err(movies), Some(Err(series))) => {
            anyhow::bail!("filmes: {movies:#}; séries: {series:#}")
        }
    }
}

fn store(database: &Database) -> Result<&acervo_store::Store> {
    database.get().map_err(anyhow::Error::msg)
}

/// Sem cliente de download, nada a pegar nem a importar.
fn without_client(settings: &Settings) -> Option<String> {
    settings
        .get()
        .qbittorrent()
        .is_none()
        .then(|| "cliente de download não configurado".to_owned())
}

/// Busca dos que faltam: a agendada pega o limite da configuração; "rodar
/// agora", todos.
#[derive(Debug)]
struct Missing {
    settings: Arc<Settings>,
    database: Database,
    catalog: Catalog,
    progress: Arc<Progress>,
}

#[async_trait]
impl Job for Missing {
    async fn run(&self, trigger: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let config = self.settings.get();
        // O registro já garante uma execução por vez; a vez no `Progress` é
        // o que alimenta o andamento.
        let turn = self
            .progress
            .start()
            .context("já há uma busca dos que faltam rodando")?;
        let limit = (trigger == Trigger::Scheduled).then_some(crate::config::SEARCH_LIMIT);
        let movies = crate::decide::search(&config, store, &self.catalog, limit, &turn)
            .await
            .map(|lines| {
                if lines.is_empty() {
                    return "nenhum filme faltando".to_owned();
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
                summary
            });
        // As séries depois dos filmes, na mesma vez.
        let series =
            crate::series::search::missing(&config, store, &self.catalog, limit, &turn).await;
        let (series, detail) = match series {
            Ok(lines) if lines.is_empty() => (None, None),
            Ok(lines) => {
                let picked: usize = lines.iter().map(|l| l.escolhidos.len()).sum();
                let failed = lines.iter().filter(|l| l.erro.is_some()).count();
                let mut summary = format!(
                    "{}, {}",
                    count(lines.len(), "buscada", "buscadas"),
                    count(picked, "pego", "pegos")
                );
                if failed > 0 {
                    summary = format!("{summary}, {failed} com erro");
                }
                (Some(Ok(summary)), Some(json!({ "series": lines })))
            }
            Err(error) => (Some(Err(error)), None),
        };
        let mut outcome = with_series(movies, series)?;
        outcome.detail = detail;
        Ok(outcome)
    }

    fn progress(&self) -> Option<String> {
        let view = self.progress.view();
        (view.rodando && view.total > 0).then(|| format!("{} de {}", view.buscados, view.total))
    }
}

/// Sincronização de RSS: pega o que serve à biblioteca.
#[derive(Debug)]
struct Rss {
    settings: Arc<Settings>,
    database: Database,
    catalog: Catalog,
}

#[async_trait]
impl Job for Rss {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let config = self.settings.get();
        let movies = crate::automatic::rss(&config, store, &self.catalog)
            .await
            .map(|grabs| {
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
                (failed == 0, summary)
            });
        let series = match crate::series::search::rss(&config, store, &self.catalog).await {
            Ok(grabs) if grabs.is_empty() => None,
            Ok(grabs) => {
                for grab in &grabs {
                    tracing::info!(
                        serie = grab.serie,
                        release = grab.release,
                        erro = grab.erro,
                        "RSS pegou"
                    );
                }
                let failed = grabs.iter().filter(|g| g.erro.is_some()).count();
                let summary = if failed == 0 {
                    count(grabs.len(), "pego", "pegos")
                } else {
                    format!("{}, {failed} falharam", count(grabs.len(), "pego", "pegos"))
                };
                Some(Ok((failed == 0, summary)))
            }
            Err(error) => Some(Err(error)),
        };
        let ok = movies.as_ref().is_ok_and(|(ok, _)| *ok)
            && series
                .as_ref()
                .is_none_or(|s| s.as_ref().is_ok_and(|(ok, _)| *ok));
        let mut outcome = with_series(movies.map(|(_, s)| s), series.map(|r| r.map(|(_, s)| s)))?;
        outcome.ok = outcome.ok && ok;
        Ok(outcome)
    }

    fn unavailable(&self) -> Option<String> {
        without_client(&self.settings)
    }
}

/// De quanto em quanto tempo a importação também verifica o disco das
/// séries.
const DISK_EVERY: Duration = Duration::from_secs(3600);

/// A última verificação de disco: quando foi e a marca da pasta de cada
/// série naquela hora. Só em memória: reiniciar verifica tudo de novo.
#[derive(Debug, Default)]
struct DiskCheck {
    at: Option<Instant>,
    stamps: std::collections::HashMap<i64, std::time::SystemTime>,
}

/// Importa os downloads do acervo que terminaram.
#[derive(Debug)]
struct Import {
    settings: Arc<Settings>,
    database: Database,
    catalog: Catalog,
    disk: tokio::sync::Mutex<DiskCheck>,
}

impl Import {
    /// Uma vez por hora, no fim da importação: liga o arquivo novo que
    /// apareceu na pasta de uma série (só o sem ambiguidade; nada sai do
    /// catálogo). Devolve quantos ligou.
    async fn check_disk(&self, store: &acervo_store::Store) -> usize {
        let mut disk = self.disk.lock().await;
        if disk.at.is_some_and(|at| at.elapsed() < DISK_EVERY) {
            return 0;
        }
        disk.at = Some(Instant::now());
        match crate::verify::new_files(store, &mut disk.stamps).await {
            Ok(n) => n,
            Err(error) => {
                tracing::warn!("verificar disco: {error:#}");
                0
            }
        }
    }
}

#[async_trait]
impl Job for Import {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let config = self.settings.get();
        let lines = crate::grab::import_downloads(&config, store, Some(&self.catalog)).await;
        let series =
            crate::series::import::import_downloads(&config, store, Some(&self.catalog)).await;
        // A fila uma vez por volta, depois de filmes e séries: o que terminou
        // ou desistiu já liberou espaço e vaga.
        let queue = match crate::grab::qbit(&config).await {
            Ok(client) => crate::grab::start_queued(&config, store, &client).await,
            Err(error) => Err(error),
        };
        if let Err(error) = queue {
            tracing::warn!("fila de downloads: {error:#}");
        }
        let (lines, series) = match (lines, series) {
            (Ok(lines), Ok(series)) => (lines, series),
            (Err(error), Ok(series)) if series.is_empty() => return Err(error),
            (Ok(lines), Err(error)) if lines.is_empty() => {
                return Err(error.context("séries"));
            }
            (Err(error), _) => return Err(error),
            (_, Err(error)) => return Err(error.context("séries")),
        };
        for line in lines.iter().filter(|l| l.estado != "baixando") {
            tracing::info!(
                filme = line.filme,
                estado = line.estado,
                destino = line.destino,
                detalhe = line.detalhe,
                "importação de download"
            );
        }
        for line in series.iter().filter(|l| l.estado != "baixando") {
            tracing::info!(
                serie = line.serie,
                estado = line.estado,
                destinos = ?line.destinos,
                detalhe = line.detalhe,
                "importação de download"
            );
        }
        let by = |estado: &str| {
            lines.iter().filter(|l| l.estado == estado).count()
                + series.iter().filter(|l| l.estado == estado).count()
        };
        let failed = by("falhou");
        let parts: Vec<String> = [
            (by("importado"), "importado", "importados"),
            (failed, "falhou", "falharam"),
            (by("atencao"), "travado", "travados"),
            (by("descartado"), "descartado", "descartados"),
            (by("baixando"), "baixando", "baixando"),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, one, many)| count(n, one, many))
        .collect();
        let mut summary = if parts.is_empty() {
            "nenhum download do acervo".to_owned()
        } else {
            parts.join(", ")
        };
        let found = self.check_disk(store).await;
        if found > 0 {
            summary = format!(
                "{summary}; {}",
                count(found, "ligado do disco", "ligados do disco")
            );
        }
        // Arquivo novo na biblioteca: o Jellyfin varre já, sem esperar a
        // varredura agendada dele.
        if (by("importado") > 0 || found > 0)
            && let Some(jellyfin) = config.jellyfin()
        {
            let refreshed = match JellyfinClient::new(
                &jellyfin.url,
                &jellyfin.api_key,
                crate::config::HTTP_TIMEOUT,
            ) {
                Ok(client) => client.refresh_library().await.map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if let Err(error) = refreshed {
                tracing::warn!("varredura do Jellyfin: {error}");
                summary = format!("{summary}; varredura do Jellyfin não pedida");
            }
        }
        Ok(Outcome::new(failed == 0, summary))
    }

    fn unavailable(&self) -> Option<String> {
        without_client(&self.settings)
    }

    /// Importar logo após o reinício, com o cliente ainda subindo, só gera
    /// falha na lista: espera ele responder.
    async fn ready(&self) -> bool {
        crate::grab::qbit(&self.settings.get()).await.is_ok()
    }
}

/// Mantém os metadados em dia: os filmes conferidos há mais de um dia. Sem
/// chave do TMDB, não há o que fazer.
#[derive(Debug)]
struct Metadata {
    database: Database,
}

#[async_trait]
impl Job for Metadata {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let Some(tmdb) = crate::metadata::tmdb(store).await? else {
            return Ok(Outcome::new(true, "sem chave do TMDB; nada a fazer"));
        };
        let report = crate::library::refresh(store, &tmdb, 24).await?;
        let mut summary = format!(
            "{}, {}, {}",
            count(report.conferidos, "conferido", "conferidos"),
            count(report.atualizados.len(), "atualizado", "atualizados"),
            count(report.falhas.len(), "falha", "falhas"),
        );
        // Qual filme falhou e por quê: sem isso a tela só mostra a contagem.
        let mut detail = (!report.atualizados.is_empty() || !report.falhas.is_empty()).then(|| {
            json!({
                "atualizados": report.atualizados,
                "falhas": report.falhas.iter().map(|(filme, erro)| json!({
                    "filme": filme,
                    "erro": erro,
                })).collect::<Vec<_>>(),
            })
        });
        // Séries a cada 12 h: episódio novo aparece mais depressa que filme.
        let series = crate::series::library::refresh(store, &tmdb, 12).await?;
        if series.conferidas > 0 {
            summary = format!(
                "{summary}; séries: {}, {}, {}",
                count(series.conferidas, "conferida", "conferidas"),
                count(series.atualizadas.len(), "atualizada", "atualizadas"),
                count(series.falhas.len(), "falha", "falhas"),
            );
        }
        // Episódio que entrou como `TBA` e ganhou título: o arquivo acompanha.
        match crate::rename::titled_files(store).await {
            Ok(0) => {}
            Ok(n) => summary = format!("{summary}; {}", count(n, "renomeado", "renomeados")),
            Err(error) => tracing::warn!("renomear depois dos metadados: {error:#}"),
        }
        if !series.atualizadas.is_empty() || !series.falhas.is_empty() {
            detail.get_or_insert_with(|| json!({}))["series"] = json!({
                "atualizadas": series.atualizadas,
                "episodios_novos": series.episodios_novos,
                "falhas": series.falhas.iter().map(|(serie, erro)| json!({
                    "serie": serie,
                    "erro": erro,
                })).collect::<Vec<_>>(),
            });
        }
        Ok(Outcome {
            ok: report.falhas.is_empty() && series.falhas.is_empty(),
            summary,
            detail,
        })
    }
}

/// De quanto em quanto tempo a limpeza também poda o banco.
const PRUNE_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Até que idade uma busca fica guardada (a mais recente de cada obra fica
/// sempre).
const SEARCHES_KEPT: time::Duration = time::Duration::days(30);

/// O ciclo de limpeza, aplicado. O relatório vira o detalhe da execução.
/// Uma vez por dia, também poda o banco ([`prune`]).
#[derive(Debug)]
struct Cleanup {
    settings: Arc<Settings>,
    database: Database,
    /// Quando foi a última poda. Só em memória: reiniciar poda de novo.
    pruned: tokio::sync::Mutex<Option<Instant>>,
    /// O último ciclo foi abortado por trava? Só a mudança vira aviso.
    aborted: AtomicBool,
}

/// A poda: as buscas com mais de 30 dias (menos a mais recente de cada
/// obra) e os bloqueios automáticos vencidos. O histórico fica: é do
/// usuário. Devolve o resumo, se algo saiu.
async fn prune(store: &acervo_store::Store, now: OffsetDateTime) -> Result<Option<String>> {
    let at = |when: OffsetDateTime| when.format(&Rfc3339).unwrap_or_default();
    let searches = store.prune_searches(&at(now - SEARCHES_KEPT)).await?;
    let blocks = store
        .delete_blocks(
            &crate::grab::EXPIRING,
            &at(now - crate::grab::NO_SEEDS_BLOCK),
        )
        .await?;
    let parts: Vec<String> = [
        (searches, "busca antiga", "buscas antigas"),
        (blocks, "bloqueio vencido", "bloqueios vencidos"),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, one, many)| count(usize::try_from(n).unwrap_or(usize::MAX), one, many))
    .collect();
    Ok((!parts.is_empty()).then(|| format!("podou {}", parts.join(" e "))))
}

impl Cleanup {
    /// A poda, se a última foi há mais de um dia. Falha vira aviso: a
    /// limpeza do cliente não depende dela.
    async fn prune_daily(&self, store: &acervo_store::Store) -> Option<String> {
        let mut last = self.pruned.lock().await;
        if last.is_some_and(|at| at.elapsed() < PRUNE_EVERY) {
            return None;
        }
        match prune(store, OffsetDateTime::now_utc()).await {
            Ok(summary) => {
                *last = Some(Instant::now());
                summary
            }
            Err(error) => {
                tracing::warn!("poda do banco: {error:#}");
                None
            }
        }
    }
}

#[async_trait]
impl Job for Cleanup {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let config = self.settings.get();
        let pruned = self.prune_daily(store).await;
        let report = crate::cycle::run(&config, store).await?;
        let (ok, mut summary) = report.summary();
        if let Some(pruned) = pruned {
            summary = format!("{summary}; {pruned}");
        }
        // Trava de segurança é resultado esperado, mas ciclo atrás de ciclo
        // sem limpar é algo que alguém precisa ver: avisa na mudança.
        let aborted = report.abortado.is_some();
        let was = self.aborted.swap(aborted, Ordering::SeqCst);
        if let Some(change) = Change::between(was, aborted) {
            let reason = report.abortado.as_deref().unwrap_or_default();
            let (title, body) = cleanup_message(change, reason);
            notify_alert(store, Alert::Cleanup, title, body).await;
        }
        Ok(Outcome {
            ok,
            summary,
            detail: Some(serde_json::to_value(&report)?),
        })
    }

    fn unavailable(&self) -> Option<String> {
        self.settings
            .get()
            .janitor()
            .err()
            .map(|error| format!("{error:#}"))
    }
}

/// Baixa do XEM a numeração de cena das séries do catálogo. Falha aqui só
/// deixa o mapa como estava.
#[derive(Debug)]
struct Scene {
    database: Database,
}

#[async_trait]
impl Job for Scene {
    async fn run(&self, _: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let report = crate::series::scene::refresh(store).await?;
        let summary = format!(
            "{}, {}, {}",
            count(report.com_mapa, "série com mapa", "séries com mapa"),
            count(report.atualizadas.len(), "atualizada", "atualizadas"),
            count(report.falhas.len(), "falha", "falhas"),
        );
        Ok(Outcome {
            ok: report.falhas.is_empty(),
            summary,
            detail: Some(serde_json::to_value(&report)?),
        })
    }
}

/// Confere no disco os arquivos que o catálogo diz estar na biblioteca: o
/// que sumiu volta pelo mesmo torrent, ou sai do catálogo para a busca. Na
/// hora agendada, sumiço em massa não é reparado (disco desmontado?); "rodar
/// agora" confirma. O relatório vira o detalhe da execução.
#[derive(Debug)]
struct DiskHeal {
    settings: Arc<Settings>,
    database: Database,
}

#[async_trait]
impl Job for DiskHeal {
    async fn run(&self, trigger: Trigger) -> Result<Outcome> {
        let store = store(&self.database)?;
        let config = self.settings.get();
        let report = crate::heal::run(&config, store, trigger).await?;
        let mut outcome = with_series(
            Ok(report.filmes.summary()),
            Some(Ok(report.series.summary())),
        )?;
        outcome.ok = report.filmes.ok() && report.series.ok();
        outcome.detail = Some(serde_json::to_value(&report)?);
        Ok(outcome)
    }

    fn unavailable(&self) -> Option<String> {
        without_client(&self.settings)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

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

    #[tokio::test]
    async fn execucao_travada_passado_o_teto_e_falha_e_a_tarefa_segue() {
        let (tasks, gate) = registry(Database::default(), Duration::ZERO, First::Now);
        let mut tasks = Arc::try_unwrap(tasks).unwrap();
        tasks.ceiling = Duration::from_millis(50);
        let tasks = Arc::new(tasks);
        tasks.start().await;
        // A primeira nunca é solta: trava até o teto.
        assert_eq!(tasks.run_now("teste"), Some(true));
        gate.started.notified().await;
        until("execução abortada", || !tasks.is_running("teste")).await;
        let view = tasks.view_one("teste").unwrap();
        assert_eq!(view["ultima"]["ok"], false);
        assert!(
            view["ultima"]["resumo"]
                .as_str()
                .unwrap()
                .starts_with("travada"),
            "{view}"
        );
        // Livre de novo: a próxima roda normalmente.
        assert_eq!(tasks.run_now("teste"), Some(true));
        gate.started.notified().await;
        gate.release.notify_one();
        until("segunda execução", || !tasks.is_running("teste")).await;
        assert_eq!(tasks.view_one("teste").unwrap()["ultima"]["ok"], true);
    }

    #[tokio::test]
    async fn desligar_para_de_agendar_e_espera_a_execucao_em_curso() {
        let (tasks, gate) = registry(
            Database::default(),
            Duration::from_millis(30),
            First::AfterInterval,
        );
        tasks.start().await;
        gate.started.notified().await;
        tasks.stop();
        // Rodando: o desligamento espera, e desiste ao fim da graça.
        assert_eq!(tasks.drain(Duration::from_millis(50)).await, 1);
        // Pedido de "rodar agora" depois do desligamento não começa nada.
        assert_eq!(tasks.run_now("teste"), Some(false));

        gate.release.notify_one();
        assert_eq!(tasks.drain(Duration::from_secs(5)).await, 0);
        // Livre, e o intervalo passa de sobra: nenhuma execução nova.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(gate.runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn subida_espalha_a_primeira_execucao_das_tarefas_que_largam_juntas() {
        let (tasks, gate) = registry(Database::default(), Duration::from_secs(3600), First::Now);
        let mut tasks = Arc::try_unwrap(tasks).unwrap();
        tasks.stagger = Duration::from_millis(400);
        let tasks = Arc::new(tasks);
        let slot_jitter = jitter(Duration::from_millis(400));
        assert!(slot_jitter <= Duration::from_millis(400));
        tasks.start().await;
        // Com atraso, a execução acontece, mas dentro da janela.
        tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
            .await
            .expect("a primeira execução vem dentro da janela");
        gate.release.notify_one();
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }

    /// Só fica pronto quando o teste manda.
    #[derive(Debug, Default)]
    struct Late {
        up: AtomicBool,
        runs: AtomicUsize,
    }

    #[async_trait]
    impl Job for Late {
        async fn run(&self, _: Trigger) -> Result<Outcome> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(Outcome::new(true, "feito"))
        }

        async fn ready(&self) -> bool {
            self.up.load(Ordering::SeqCst)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn primeira_execucao_espera_o_servico_responder() {
        let job = Arc::new(Late::default());
        let tasks = Arc::new(Tasks::new(
            Database::default(),
            vec![Task {
                id: "teste",
                name: "Teste",
                interval: Duration::from_secs(3600),
                first: First::Now,
                job: Arc::clone(&job) as Arc<dyn Job>,
            }],
        ));
        let mut tasks = Arc::try_unwrap(tasks).unwrap();
        tasks.stagger = Duration::ZERO;
        let tasks = Arc::new(tasks);
        tasks.start().await;
        // Fora do ar: tenta de novo a cada 15 s, sem rodar.
        tokio::time::sleep(Duration::from_secs(50)).await;
        assert_eq!(job.runs.load(Ordering::SeqCst), 0);

        job.up.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(16)).await;
        assert_eq!(job.runs.load(Ordering::SeqCst), 1);
        tasks.stop();
    }

    #[derive(Debug)]
    struct Unavailable;

    #[async_trait]
    impl Job for Unavailable {
        async fn run(&self, _: Trigger) -> Result<Outcome> {
            anyhow::bail!("não devia rodar agendada")
        }

        fn unavailable(&self) -> Option<String> {
            Some("cliente de download não configurado".into())
        }
    }

    #[tokio::test]
    async fn indisponivel_sai_do_agendamento_e_diz_por_que() {
        let tasks = Arc::new(Tasks::new(
            Database::default(),
            vec![Task {
                id: "teste",
                name: "Teste",
                interval: Duration::from_millis(10),
                first: First::Now,
                job: Arc::new(Unavailable),
            }],
        ));
        tasks.start().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let view = tasks.view_one("teste").unwrap();
        assert_eq!(view["proxima"], Value::Null);
        assert_eq!(view["ultima"], Value::Null);
        assert_eq!(view["indisponivel"], "cliente de download não configurado");
    }

    #[derive(Debug)]
    struct ClientGate {
        config: watch::Receiver<Arc<crate::config::Config>>,
        gate: Arc<Gate>,
    }

    #[async_trait]
    impl Job for ClientGate {
        async fn run(&self, trigger: Trigger) -> Result<Outcome> {
            self.gate.run(trigger).await
        }

        fn unavailable(&self) -> Option<String> {
            self.config
                .borrow()
                .qbittorrent()
                .is_none()
                .then(|| "cliente de download não configurado".into())
        }
    }

    #[tokio::test]
    async fn configurar_cliente_acorda_tarefa_indisponivel_sem_reiniciar() {
        let (settings, changes) = watch::channel(Arc::new(crate::config::Config::default()));
        let gate = Arc::new(Gate::default());
        let tasks = Arc::new(Tasks::new(
            Database::default(),
            vec![Task {
                id: "teste",
                name: "Teste",
                interval: Duration::from_millis(100),
                first: First::AfterInterval,
                job: Arc::new(ClientGate {
                    config: changes.clone(),
                    gate: Arc::clone(&gate),
                }),
            }],
        ));
        tasks.watch_settings(changes);
        tasks.start().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(tasks.view()[0]["proxima"], Value::Null);
        assert_eq!(gate.runs.load(Ordering::SeqCst), 0);

        // O mesmo canal que Settings::save_section publica após salvar no banco.
        let mut configured = crate::config::Config::default();
        configured.qbittorrent.url = "http://cliente.invalid".into();
        settings.send_replace(Arc::new(configured));
        until("próxima agendada", || {
            tasks.view()[0]["proxima"].is_string()
        })
        .await;
        gate.started.notified().await;
        assert_eq!(*gate.triggers.lock().unwrap(), [Trigger::Scheduled]);

        settings.send_replace(Arc::new(crate::config::Config::default()));
        gate.release.notify_one();
        until("dependência removida", || {
            tasks.view()[0]["proxima"].is_null() && !tasks.is_running("teste")
        })
        .await;
        tasks.stop();
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

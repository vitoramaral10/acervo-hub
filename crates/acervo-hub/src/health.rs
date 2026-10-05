//! Saúde do serviço: a prontidão (`GET /health/ready`, para o `HEALTHCHECK`
//! do container) e os avisos operacionais — o que falha por muito tempo não
//! pode passar em silêncio.
//!
//! Os avisos só saem na transição: quando o estado vira ruim e quando volta,
//! nunca a cada rodada que o repete. O estado vive em memória; reiniciar o
//! serviço o zera, e o pior que isso causa é repetir um aviso.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use acervo_api::{Catalog, IndexerView};
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use serde_json::{Value, json};

use crate::config::Config;
use crate::events::{Alert, Change, notify_alert};
use crate::serve::Database;
use crate::settings::Settings;
use crate::tasks::Cancel;

/// Falhas seguidas de uma tarefa que viram aviso.
pub const TASK_FAILURES: u32 = 3;

/// Falhas seguidas de um indexador que viram aviso. É o mesmo número que abre
/// o disjuntor do catálogo.
pub const INDEXER_FAILURES: u32 = 5;

/// Fração do disco, em %, abaixo da qual o espaço livre vira aviso (ou a
/// folga das regras, o que for maior).
const DISK_PERCENT: u64 = 5;

/// De quanto em quanto tempo o monitor confere indexadores e disco.
const MONITOR_EVERY: Duration = Duration::from_secs(5 * 60);

/// Espera antes da primeira conferência: a subida ainda está acontecendo.
const MONITOR_FIRST: Duration = Duration::from_secs(60);

/// Teto de cada verificação da prontidão: o `HEALTHCHECK` não pode pendurar.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

// --- Prontidão --------------------------------------------------------------

/// O resultado de uma verificação da prontidão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub ok: bool,
    /// Não aplicável agora (sem cliente configurado); não reprova.
    pub skipped: bool,
    /// Texto fixo, nunca a mensagem crua de um erro: a rota não tem login.
    pub detail: String,
}

impl Probe {
    fn pass(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            skipped: false,
            detail: detail.into(),
        }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            skipped: false,
            detail: detail.into(),
        }
    }

    fn skip(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            skipped: true,
            detail: detail.into(),
        }
    }

    fn json(&self) -> Value {
        let mut value = json!({ "ok": self.ok, "detalhe": self.detail });
        if self.skipped {
            value["pulada"] = json!(true);
        }
        value
    }
}

/// O espaço livre contra a folga das regras.
#[must_use]
pub fn space_probe(free: u64, reserve_mb: u64) -> Probe {
    let free_mb = free >> 20;
    let detail = format!("{free_mb} MB livres, folga de {reserve_mb} MB");
    if free_mb > reserve_mb {
        Probe::pass(detail)
    } else {
        Probe::fail(detail)
    }
}

/// O corpo e o status de `/health/ready`: 200 se tudo passou, 503 se algo
/// falhou.
#[must_use]
pub fn report(database: &Probe, qbittorrent: &Probe, space: &Probe) -> (StatusCode, Value) {
    let ready = database.ok && qbittorrent.ok && space.ok;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = json!({
        "pronto": ready,
        "verificacoes": {
            "banco": database.json(),
            "qbittorrent": qbittorrent.json(),
            "espaco": space.json(),
        },
    });
    (status, body)
}

/// O que a prontidão consulta.
#[derive(Debug)]
pub struct Readiness {
    pub settings: Arc<Settings>,
    pub database: Database,
}

impl Readiness {
    /// Confere banco, cliente de download e espaço livre. Cada verificação
    /// tem teto de tempo, e o motivo de uma falha vai para o log, não para a
    /// resposta.
    pub async fn check(&self) -> (StatusCode, Value) {
        let (database, reserve) = self.database_probe().await;
        let config = self.settings.get();
        let (qbittorrent, space) = Self::client_probes(&config, reserve).await;
        report(&database, &qbittorrent, &space)
    }

    /// O banco e, de carona, a folga das regras (se o banco respondeu).
    async fn database_probe(&self) -> (Probe, Option<u64>) {
        let store = match self.database.get() {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!("prontidão: {error}");
                return (Probe::fail("banco indisponível"), None);
            }
        };
        let reply = tokio::time::timeout(PROBE_TIMEOUT, crate::rules::stored(store)).await;
        match reply {
            Ok(Ok(rules)) => (Probe::pass("respondeu"), Some(rules.folga_minima_mb)),
            Ok(Err(error)) => {
                tracing::warn!("prontidão: banco: {error:#}");
                (Probe::fail("consulta ao banco falhou"), None)
            }
            Err(_) => (Probe::fail("banco não respondeu a tempo"), None),
        }
    }

    async fn client_probes(config: &Config, reserve_mb: Option<u64>) -> (Probe, Probe) {
        if config.qbittorrent().is_none() {
            return (
                Probe::skip("cliente de download não configurado"),
                Probe::skip("sem cliente de download para medir"),
            );
        }
        let reading = tokio::time::timeout(PROBE_TIMEOUT, async {
            let client = crate::grab::qbit(config).await?;
            Ok::<_, anyhow::Error>(client.free_space().await)
        })
        .await;
        match reading {
            Ok(Ok(Ok(free))) => (
                Probe::pass("respondeu"),
                reserve_mb.map_or_else(
                    || Probe::skip("folga das regras ilegível"),
                    |reserve| space_probe(free, reserve),
                ),
            ),
            // Alcançável, mas sem medida: o cliente responde `-1` quando a
            // pasta de download não existe.
            Ok(Ok(Err(acervo_clients::QbitError::FreeSpaceUnknown))) => (
                Probe::pass("respondeu"),
                Probe::skip("o cliente não mediu o espaço"),
            ),
            // Entrou, mas a leitura falhou (transporte, status, resposta
            // ilegível): o cliente não está pronto.
            Ok(Ok(Err(error))) => {
                tracing::warn!("prontidão: espaço livre: {error}");
                (
                    Probe::fail("cliente de download não respondeu à leitura"),
                    Probe::skip("sem leitura do cliente"),
                )
            }
            Ok(Err(error)) => {
                tracing::warn!("prontidão: cliente de download: {error:#}");
                (
                    Probe::fail("cliente de download inalcançável"),
                    Probe::skip("sem leitura do cliente"),
                )
            }
            Err(_) => (
                Probe::fail("cliente de download não respondeu a tempo"),
                Probe::skip("sem leitura do cliente"),
            ),
        }
    }
}

/// `GET /health/ready`, sem login como `/health`. A resposta só diz o que
/// passou e o que falhou, sem endereço, usuário nem chave.
pub fn router(readiness: Arc<Readiness>) -> Router {
    Router::new()
        .route("/health/ready", get(ready))
        .with_state(readiness)
}

async fn ready(State(readiness): State<Arc<Readiness>>) -> Response {
    let (status, body) = readiness.check().await;
    let mut response = acervo_api::ui_json(status, &body);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

// --- Transições -------------------------------------------------------------

/// Conta as falhas seguidas e diz se passou do limite para um lado ou outro.
/// `before` são as falhas até a rodada anterior.
#[must_use]
pub fn track_streak(before: u32, ok: bool, threshold: u32) -> (u32, Option<Change>) {
    let after = if ok { 0 } else { before.saturating_add(1) };
    (
        after,
        Change::between(before >= threshold, after >= threshold),
    )
}

/// Abaixo de `threshold` livre é pouco. Quem já está em aviso só sai dele
/// 10% acima do limite: um download enchendo e liberando disco perto da linha
/// não pode avisar a cada conferência.
#[must_use]
pub const fn disk_is_low(was_low: bool, free: u64, threshold: u64) -> bool {
    if was_low {
        free < threshold.saturating_add(threshold / 10)
    } else {
        free < threshold
    }
}

/// O limite de espaço livre: 5% do disco ou a folga das regras, o que for
/// maior. Sem o tamanho do disco, só a folga.
#[must_use]
pub fn disk_threshold(total: Option<u64>, reserve: u64) -> u64 {
    total
        .map_or(0, |total| total / 100 * DISK_PERCENT)
        .max(reserve)
}

/// As mudanças de estado dos indexadores desde a conferência anterior.
/// `flagged` são os que já estão em aviso; quem saiu do catálogo deixa de
/// contar sem aviso.
#[must_use]
pub fn indexer_changes(
    flagged: &mut HashSet<String>,
    views: &[IndexerView],
) -> Vec<(Change, String, u32, Option<String>)> {
    flagged.retain(|name| views.iter().any(|view| &view.name == name));
    let mut changes = Vec::new();
    for view in views {
        let failures = view.health.consecutive_failures;
        let was_bad = flagged.contains(&view.name);
        let Some(change) = Change::between(was_bad, failures >= INDEXER_FAILURES) else {
            continue;
        };
        match change {
            Change::Entered => flagged.insert(view.name.clone()),
            Change::Recovered => flagged.remove(&view.name),
        };
        changes.push((
            change,
            view.name.clone(),
            failures,
            view.health.last_error.clone(),
        ));
    }
    changes
}

// --- Textos dos avisos ------------------------------------------------------

/// Título e corpo do aviso de tarefa.
#[must_use]
pub fn task_message(change: Change, name: &str, failures: u32, summary: &str) -> (String, String) {
    match change {
        Change::Entered => (
            format!("Tarefa falhando: {name}"),
            format!("{failures} falhas seguidas.\n\n{summary}"),
        ),
        Change::Recovered => (
            format!("Tarefa voltou a passar: {name}"),
            "A última execução terminou bem.".to_owned(),
        ),
    }
}

/// Título e corpo do aviso de indexador.
#[must_use]
pub fn indexer_message(
    change: Change,
    name: &str,
    failures: u32,
    error: Option<&str>,
) -> (String, String) {
    match change {
        Change::Entered => (
            format!("Indexador falhando: {name}"),
            format!(
                "{failures} falhas seguidas; fora das buscas por um tempo, que dobra a cada \
                 nova falha.\n\n{}",
                error.unwrap_or("sem detalhe do erro")
            ),
        ),
        Change::Recovered => (
            format!("Indexador voltou: {name}"),
            "Respondeu de novo.".to_owned(),
        ),
    }
}

/// Título e corpo do aviso de espaço.
#[must_use]
pub fn disk_message(change: Change, free: u64, threshold: u64) -> (String, String) {
    let mb = |bytes: u64| bytes >> 20;
    match change {
        Change::Entered => (
            "Pouco espaço na pasta de download".to_owned(),
            format!(
                "{} MB livres; o limite é {} MB (5% do disco ou a folga das regras).",
                mb(free),
                mb(threshold)
            ),
        ),
        Change::Recovered => (
            "Espaço na pasta de download voltou ao normal".to_owned(),
            format!("{} MB livres.", mb(free)),
        ),
    }
}

/// Título e corpo do aviso de limpeza abortada.
#[must_use]
pub fn cleanup_message(change: Change, reason: &str) -> (String, String) {
    match change {
        Change::Entered => (
            "Limpeza abortada por trava de segurança".to_owned(),
            format!("Nada foi alterado.\n\n{reason}"),
        ),
        Change::Recovered => (
            "Limpeza voltou a rodar".to_owned(),
            "O ciclo não foi mais abortado.".to_owned(),
        ),
    }
}

// --- Monitor ----------------------------------------------------------------

/// Confere de tempos em tempos o que não tem um evento próprio onde pendurar
/// o aviso: a saúde dos indexadores e o espaço em disco.
#[derive(Debug)]
pub struct Monitor {
    pub settings: Arc<Settings>,
    pub database: Database,
    pub catalog: Catalog,
    indexers: Mutex<HashSet<String>>,
    disk_low: Mutex<bool>,
}

impl Monitor {
    #[must_use]
    pub fn new(settings: Arc<Settings>, database: Database, catalog: Catalog) -> Self {
        Self {
            settings,
            database,
            catalog,
            indexers: Mutex::default(),
            disk_low: Mutex::default(),
        }
    }

    /// Roda até o cancelamento.
    pub async fn run(self: Arc<Self>, cancel: Cancel) {
        let mut wait = MONITOR_FIRST;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = cancel.cancelled() => return,
            }
            wait = MONITOR_EVERY;
            self.tick().await;
        }
    }

    /// Uma conferência: indexadores e disco.
    pub async fn tick(&self) {
        let Ok(store) = self.database.get() else {
            return;
        };
        let changes = {
            let mut flagged = self.indexers.lock().unwrap_or_else(PoisonError::into_inner);
            indexer_changes(&mut flagged, &self.catalog.views())
        };
        for (change, name, failures, error) in changes {
            let (title, body) = indexer_message(change, &name, failures, error.as_deref());
            notify_alert(store, Alert::Indexer, title, body).await;
        }
        match self.disk_reading(store).await {
            Ok(Some((free, threshold))) => {
                let change = {
                    let mut low = self.disk_low.lock().unwrap_or_else(PoisonError::into_inner);
                    let is_low = disk_is_low(*low, free, threshold);
                    let change = Change::between(*low, is_low);
                    *low = is_low;
                    change
                };
                if let Some(change) = change {
                    let (title, body) = disk_message(change, free, threshold);
                    notify_alert(store, Alert::Disk, title, body).await;
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("conferindo o espaço em disco: {error:#}"),
        }
    }

    /// O espaço livre da pasta de download e o limite de aviso; `None` sem
    /// cliente de download configurado.
    async fn disk_reading(
        &self,
        store: &acervo_store::Store,
    ) -> anyhow::Result<Option<(u64, u64)>> {
        let config = self.settings.get();
        if config.qbittorrent().is_none() {
            return Ok(None);
        }
        let reserve = crate::rules::stored(store).await?.folga_minima_mb << 20;
        let free = tokio::time::timeout(PROBE_TIMEOUT, async {
            crate::grab::qbit(&config)
                .await?
                .free_space()
                .await
                .map_err(anyhow::Error::from)
        })
        .await??;
        let total = config
            .library
            .roots
            .first()
            .and_then(|root| total_space(root));
        Ok(Some((free, disk_threshold(total, reserve))))
    }
}

/// O tamanho do disco onde `path` está.
fn total_space(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(stat.f_blocks.saturating_mul(stat.f_frsize))
}

#[cfg(test)]
mod tests {
    use acervo_api::Health;

    use super::*;

    fn view(name: &str, failures: u32) -> IndexerView {
        let mut health = Health::default();
        health.consecutive_failures = failures;
        health.last_error = Some("timeout".into());
        IndexerView {
            name: name.to_owned(),
            capabilities: acervo_indexers::Capabilities::default(),
            proxies_downloads: false,
            health,
        }
    }

    #[test]
    fn tarefa_avisa_na_terceira_falha_e_quando_volta_e_nao_repete() {
        let mut streak = 0;
        let mut seen = Vec::new();
        for ok in [false, false, false, false, false, true, true, false] {
            let (next, change) = track_streak(streak, ok, TASK_FAILURES);
            streak = next;
            seen.push(change);
        }
        assert_eq!(
            seen,
            [
                None,
                None,
                Some(Change::Entered),
                None,
                None,
                Some(Change::Recovered),
                None,
                None,
            ]
        );
    }

    #[test]
    fn sucesso_no_meio_zera_a_contagem_da_tarefa() {
        let (streak, change) = track_streak(2, true, TASK_FAILURES);
        assert_eq!((streak, change), (0, None));
        let (streak, change) = track_streak(streak, false, TASK_FAILURES);
        assert_eq!((streak, change), (1, None));
    }

    #[test]
    fn indexador_avisa_ao_chegar_em_cinco_e_ao_voltar_sem_repetir() {
        let mut flagged = HashSet::new();
        assert!(indexer_changes(&mut flagged, &[view("a", 4)]).is_empty());

        let changes = indexer_changes(&mut flagged, &[view("a", 5), view("b", 0)]);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].0, changes[0].1.as_str()),
            (Change::Entered, "a")
        );

        // Continua ruim, e piora: nada novo.
        assert!(indexer_changes(&mut flagged, &[view("a", 9), view("b", 0)]).is_empty());

        let changes = indexer_changes(&mut flagged, &[view("a", 0), view("b", 0)]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, Change::Recovered);
        assert!(indexer_changes(&mut flagged, &[view("a", 0)]).is_empty());
    }

    #[test]
    fn indexador_que_sai_do_catalogo_nao_avisa_e_pode_avisar_de_novo() {
        let mut flagged = HashSet::new();
        let _ = indexer_changes(&mut flagged, &[view("a", 5)]);
        assert!(indexer_changes(&mut flagged, &[]).is_empty());
        let changes = indexer_changes(&mut flagged, &[view("a", 5)]);
        assert_eq!(changes[0].0, Change::Entered);
    }

    #[test]
    fn limite_do_disco_e_o_maior_entre_cinco_por_cento_e_a_folga() {
        let gb = 1_u64 << 30;
        assert_eq!(disk_threshold(Some(1000 * gb), 100 << 20), 50 * gb);
        assert_eq!(disk_threshold(Some(10 * gb), 2 * gb), 2 * gb);
        assert_eq!(disk_threshold(None, 3 * gb), 3 * gb);
    }

    #[test]
    fn disco_avisa_ao_cruzar_o_limite_e_so_volta_com_folga_acima_dele() {
        let threshold = 1000;
        let mut low = false;
        let mut seen = Vec::new();
        for free in [2000, 999, 500, 1050, 1100, 5000, 900] {
            let is_low = disk_is_low(low, free, threshold);
            seen.push(Change::between(low, is_low));
            low = is_low;
        }
        assert_eq!(
            seen,
            [
                None,
                Some(Change::Entered),
                None,
                None, // 1050 ainda está dentro da histerese
                Some(Change::Recovered),
                None,
                Some(Change::Entered),
            ]
        );
    }

    #[test]
    fn prontidao_200_com_tudo_e_503_com_o_que_falhou() {
        let ok = Probe::pass("respondeu");
        let (status, body) = report(&ok, &ok, &space_probe(10 << 30, 100));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pronto"], true);
        assert_eq!(body["verificacoes"]["banco"]["ok"], true);
        assert_eq!(
            body["verificacoes"]["espaco"]["detalhe"],
            "10240 MB livres, folga de 100 MB"
        );

        let down = Probe::fail("cliente de download inalcançável");
        let (status, body) = report(&ok, &down, &Probe::skip("sem leitura do cliente"));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["pronto"], false);
        assert_eq!(body["verificacoes"]["banco"]["ok"], true);
        assert_eq!(body["verificacoes"]["qbittorrent"]["ok"], false);
        assert_eq!(
            body["verificacoes"]["qbittorrent"]["detalhe"],
            "cliente de download inalcançável"
        );
        assert_eq!(body["verificacoes"]["espaco"]["pulada"], true);
    }

    #[test]
    fn prontidao_reprova_espaco_na_folga_ou_abaixo_dela() {
        assert!(!space_probe(100 << 20, 100).ok);
        assert!(!space_probe(50 << 20, 100).ok);
        assert!(space_probe(101 << 20, 100).ok);
        let (status, body) = report(
            &Probe::pass("respondeu"),
            &Probe::pass("respondeu"),
            &space_probe(1 << 20, 100),
        );
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["verificacoes"]["espaco"]["ok"], false);
        assert!(!body.to_string().contains("senha"));
    }

    #[test]
    fn textos_dos_avisos_nomeiam_o_que_mudou() {
        let (title, body) = task_message(Change::Entered, "Importação", 3, "fora do ar");
        assert!(title.contains("Importação"));
        assert!(body.contains("3 falhas") && body.contains("fora do ar"));
        let (title, _) = indexer_message(Change::Recovered, "idx", 0, None);
        assert!(title.contains("idx") && title.contains("voltou"));
        let (_, body) = disk_message(Change::Entered, 100 << 20, 500 << 20);
        assert!(body.contains("100 MB") && body.contains("500 MB"));
        let (title, _) = cleanup_message(Change::Entered, "lote grande demais");
        assert!(title.contains("abortada"));
    }
}

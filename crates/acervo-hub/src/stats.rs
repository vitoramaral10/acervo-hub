//! Estatística por indexador: cada consulta que sai (com o tempo, se falhou,
//! se foi 429) e cada grab, somados por dia (UTC) no banco.
//!
//! O catálogo avisa de cada consulta ([`observer`]); um escritor à parte
//! junta os avisos que chegaram e grava de uma vez, sem segurar a busca.

// Handler devolve a resposta de erro pronta, como em `web`.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::sync::Arc;

use acervo_api::{QueryObserver, QueryRecord};
use acervo_store::{IndexerDayStats, StatsDelta, Store};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Date, OffsetDateTime};
use tokio::sync::mpsc;

use crate::web::{Shared, Web, WebResult, enter, fail, ok};

/// `AAAA-MM-DD`.
fn day(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// O dia de hoje, em UTC.
fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// Avisos de consulta somados por indexador, num dia.
#[must_use]
pub fn deltas(records: &[QueryRecord], on: &str) -> Vec<StatsDelta> {
    let mut by_indexer: BTreeMap<&str, StatsDelta> = BTreeMap::new();
    for record in records {
        let delta = by_indexer
            .entry(record.indexer.as_str())
            .or_insert_with(|| StatsDelta {
                indexer: record.indexer.clone(),
                day: on.to_owned(),
                ..StatsDelta::default()
            });
        delta.queries += 1;
        delta.failures += u32::from(!record.ok);
        delta.rate_limited += u32::from(record.rate_limited);
        delta.total_ms = delta
            .total_ms
            .saturating_add(u64::try_from(record.elapsed.as_millis()).unwrap_or(u64::MAX));
    }
    by_indexer.into_values().collect()
}

#[derive(Debug)]
struct Sink(mpsc::UnboundedSender<QueryRecord>);

impl QueryObserver for Sink {
    fn observe(&self, record: QueryRecord) {
        // Escritor parado (o serviço está saindo): a consulta só não conta.
        let _ = self.0.send(record);
    }
}

/// O observador que o catálogo avisa, e o escritor que grava no banco.
#[must_use]
pub fn observer(store: Store) -> Arc<dyn QueryObserver> {
    let (sender, mut receiver) = mpsc::unbounded_channel::<QueryRecord>();
    tokio::spawn(async move {
        while let Some(first) = receiver.recv().await {
            let mut batch = vec![first];
            while let Ok(more) = receiver.try_recv() {
                batch.push(more);
            }
            let deltas = deltas(&batch, &day(today()));
            if let Err(error) = store.add_indexer_stats(&deltas).await {
                tracing::warn!(
                    consultas = batch.len(),
                    "estatística de indexador perdida: {error}"
                );
            }
        }
    });
    Arc::new(Sink(sender))
}

/// Um grab a mais no indexador do release. Falha só vai ao log: o grab já
/// aconteceu.
pub async fn grabbed(store: &Store, indexer: &str) {
    let delta = StatsDelta {
        indexer: indexer.to_owned(),
        day: day(today()),
        grabs: 1,
        ..StatsDelta::default()
    };
    if let Err(error) = store.add_indexer_stats(&[delta]).await {
        tracing::warn!(indexer, "grab fora da estatística: {error}");
    }
}

/// Um dia da série de um indexador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayView {
    pub dia: String,
    pub consultas: u32,
    pub falhas: u32,
    pub grabs: u32,
}

/// Os números de um indexador na janela.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexerView {
    pub nome: String,
    pub consultas: u32,
    pub falhas: u32,
    /// Respostas 429.
    pub limitadas: u32,
    pub grabs: u32,
    /// De 0 a 1; zero sem consulta.
    pub taxa_falha: f64,
    /// `None` sem consulta.
    pub tempo_medio_ms: Option<u64>,
    /// Um ponto por dia da janela, do mais velho ao de hoje, com zero nos
    /// dias sem nada.
    pub serie: Vec<DayView>,
}

/// Os dias da janela que termina em `last`, do mais velho ao mais novo.
#[must_use]
pub fn window(last: Date, days: u16) -> Vec<String> {
    (0..i64::from(days))
        .rev()
        .filter_map(|back| last.checked_sub(time::Duration::days(back)))
        .map(day)
        .collect()
}

/// Soma as linhas por indexador sobre a janela `days`.
#[must_use]
pub fn aggregate(rows: &[IndexerDayStats], days: &[String]) -> Vec<IndexerView> {
    let mut by_indexer: BTreeMap<&str, Vec<&IndexerDayStats>> = BTreeMap::new();
    for row in rows.iter().filter(|row| days.contains(&row.day)) {
        by_indexer
            .entry(row.indexer.as_str())
            .or_default()
            .push(row);
    }
    by_indexer
        .into_iter()
        .map(|(name, rows)| {
            let sum = |field: fn(&IndexerDayStats) -> u32| {
                rows.iter()
                    .map(|row| field(row))
                    .fold(0_u32, u32::saturating_add)
            };
            let queries = sum(|row| row.queries);
            let failures = sum(|row| row.failures);
            let total_ms: u64 = rows.iter().map(|row| row.total_ms).sum();
            let serie = days
                .iter()
                .map(|dia| {
                    let found = rows.iter().find(|row| &row.day == dia);
                    DayView {
                        dia: dia.clone(),
                        consultas: found.map_or(0, |row| row.queries),
                        falhas: found.map_or(0, |row| row.failures),
                        grabs: found.map_or(0, |row| row.grabs),
                    }
                })
                .collect();
            IndexerView {
                nome: name.to_owned(),
                consultas: queries,
                falhas: failures,
                limitadas: sum(|row| row.rate_limited),
                grabs: sum(|row| row.grabs),
                taxa_falha: if queries == 0 {
                    0.0
                } else {
                    f64::from(failures) / f64::from(queries)
                },
                tempo_medio_ms: (queries > 0).then(|| total_ms / u64::from(queries)),
                serie,
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct Params {
    #[serde(default = "thirty")]
    dias: u16,
}

const fn thirty() -> u16 {
    30
}

pub fn router(web: Arc<Web>) -> Router {
    Router::new()
        .route("/ui/api/indexadores/estatisticas", get(statistics))
        .with_state(web)
}

/// `GET /ui/api/indexadores/estatisticas?dias=30`: os números de cada
/// indexador nos últimos dias (1 a 365), com a série diária.
async fn statistics(
    State(web): Shared,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let days = window(today(), params.dias.clamp(1, 365));
    let since = days.first().cloned().unwrap_or_else(|| day(today()));
    let rows = store
        .indexer_stats(&since)
        .await
        .map_err(|error| fail(crate::web::bad(error)))?;
    ok(&json!({
        "dias": days.len(),
        "indexadores": aggregate(&rows, &days),
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn record(indexer: &str, ok: bool, rate_limited: bool, ms: u64) -> QueryRecord {
        QueryRecord {
            indexer: indexer.into(),
            ok,
            rate_limited,
            elapsed: Duration::from_millis(ms),
        }
    }

    #[test]
    fn avisos_viram_uma_soma_por_indexador() {
        let deltas = deltas(
            &[
                record("b", true, false, 100),
                record("a", false, true, 50),
                record("b", false, false, 300),
            ],
            "2026-10-04",
        );
        assert_eq!(
            deltas,
            [
                StatsDelta {
                    indexer: "a".into(),
                    day: "2026-10-04".into(),
                    queries: 1,
                    failures: 1,
                    rate_limited: 1,
                    grabs: 0,
                    total_ms: 50,
                },
                StatsDelta {
                    indexer: "b".into(),
                    day: "2026-10-04".into(),
                    queries: 2,
                    failures: 1,
                    rate_limited: 0,
                    grabs: 0,
                    total_ms: 400,
                },
            ]
        );
    }

    #[test]
    fn janela_termina_hoje_e_cruza_o_mes() {
        let last = Date::from_calendar_date(2026, time::Month::October, 2).unwrap();
        assert_eq!(window(last, 3), ["2026-09-30", "2026-10-01", "2026-10-02"]);
        assert_eq!(window(last, 1), ["2026-10-02"]);
    }

    fn row(
        indexer: &str,
        day: &str,
        queries: u32,
        failures: u32,
        grabs: u32,
        total_ms: u64,
    ) -> IndexerDayStats {
        IndexerDayStats {
            indexer: indexer.into(),
            day: day.into(),
            queries,
            failures,
            rate_limited: failures,
            grabs,
            total_ms,
        }
    }

    #[test]
    fn agrega_totais_taxa_media_e_serie_com_zeros() {
        let days = window(
            Date::from_calendar_date(2026, time::Month::October, 3).unwrap(),
            3,
        );
        let rows = [
            // Fora da janela: não conta.
            row("a", "2026-09-30", 50, 50, 9, 99_999),
            row("a", "2026-10-01", 3, 1, 0, 600),
            row("a", "2026-10-03", 1, 0, 2, 200),
            row("b", "2026-10-02", 0, 0, 1, 0),
        ];
        let views = aggregate(&rows, &days);
        assert_eq!(views.len(), 2);
        let a = &views[0];
        assert_eq!((a.consultas, a.falhas, a.limitadas, a.grabs), (4, 1, 1, 2));
        assert!((a.taxa_falha - 0.25).abs() < f64::EPSILON);
        assert_eq!(a.tempo_medio_ms, Some(200));
        assert_eq!(
            a.serie
                .iter()
                .map(|d| (d.dia.as_str(), d.consultas, d.falhas))
                .collect::<Vec<_>>(),
            [
                ("2026-10-01", 3, 1),
                ("2026-10-02", 0, 0),
                ("2026-10-03", 1, 0)
            ]
        );
        // Só grab, nenhuma consulta: taxa zero e sem tempo médio.
        let b = &views[1];
        assert_eq!((b.consultas, b.grabs), (0, 1));
        assert!(b.taxa_falha.abs() < f64::EPSILON);
        assert_eq!(b.tempo_medio_ms, None);
    }
}

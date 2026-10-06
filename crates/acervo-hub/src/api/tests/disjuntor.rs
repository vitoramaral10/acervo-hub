//! Disjuntor por indexador: depois de 5 falhas seguidas, de qualquer tipo, o
//! catálogo para de consultá-lo por um recuo exponencial (5 min dobrando até
//! 6 h), e um sucesso zera tudo.
//!
//! O relógio é o do tokio, parado: o prazo anda com `tokio::time::advance`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::api::{ALL, Catalog, Entry, SearchError};
use acervo_indexers::{Capabilities, Indexer, IndexerError, Release, SearchQuery, SearchSupport};
use async_trait::async_trait;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);

/// Com `broken`, toda consulta cai em timeout.
#[derive(Debug)]
struct Tracker {
    broken: AtomicBool,
    searches: AtomicUsize,
}

impl Tracker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            broken: AtomicBool::new(true),
            searches: AtomicUsize::new(0),
        })
    }

    fn searches(&self) -> usize {
        self.searches.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Indexer for Tracker {
    fn name(&self) -> &'static str {
        "unico"
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.searches.fetch_add(1, Ordering::SeqCst);
        if self.broken.load(Ordering::SeqCst) {
            return Err(IndexerError::Transport {
                indexer: "unico".into(),
                kind: "timeout",
            });
        }
        Ok(vec![Release {
            indexer: "unico".into(),
            guid: query.term.clone().unwrap_or_default(),
            title: "Filme 2025 1080p".into(),
            download_url: url::Url::parse("https://tracker.invalid/dl/1").unwrap(),
            info_url: None,
            size: 1024,
            published: None,
            seeders: Some(5),
            leechers: None,
            grabs: None,
            categories: vec![2000],
            tags: Vec::new(),
        }])
    }
}

fn catalog(tracker: &Arc<Tracker>) -> Catalog {
    Catalog::new([Entry {
        indexer: Arc::clone(tracker) as Arc<dyn Indexer>,
        capabilities: Capabilities {
            general: SearchSupport {
                available: true,
                supported_params: ["q".to_owned()].into(),
            },
            ..Capabilities::default()
        },
    }])
    .unwrap()
}

fn waiting(catalog: &Catalog) -> bool {
    catalog.views()[0].health.waiting_until().is_some()
}

/// Buscas com termos diferentes (a mesma seria a resposta guardada).
async fn fail_times(catalog: &Catalog, times: usize, tag: &str) {
    for n in 0..times {
        let _ = catalog
            .search(ALL, &SearchQuery::general(format!("{tag}{n}")))
            .await;
    }
}

#[tokio::test(start_paused = true)]
async fn cinco_falhas_seguidas_abrem_o_disjuntor_e_a_espera_dobra_ate_seis_horas() {
    let tracker = Tracker::new();
    let catalog = catalog(&tracker);

    fail_times(&catalog, 4, "a").await;
    assert!(!waiting(&catalog), "quatro falhas ainda consultam");
    fail_times(&catalog, 1, "b").await;
    assert!(waiting(&catalog));
    assert_eq!(tracker.searches(), 5);

    // Em espera, a busca nem sai.
    let error = catalog
        .search(ALL, &SearchQuery::general("c"))
        .await
        .unwrap_err();
    assert!(matches!(error, SearchError::AllWaiting { .. }));
    assert_eq!(tracker.searches(), 5);

    // 1ª abertura: 5 minutos.
    tokio::time::advance(4 * MINUTE).await;
    assert!(waiting(&catalog));
    tokio::time::advance(2 * MINUTE).await;
    assert!(!waiting(&catalog));

    // A sonda falha: 10 minutos.
    fail_times(&catalog, 1, "d").await;
    assert_eq!(tracker.searches(), 6);
    tokio::time::advance(9 * MINUTE).await;
    assert!(waiting(&catalog));
    tokio::time::advance(2 * MINUTE).await;
    assert!(!waiting(&catalog));

    // 20, 40, ..., e o teto de 6 h, que se repete.
    for round in 0..10 {
        fail_times(&catalog, 1, &format!("e{round}")).await;
        assert!(waiting(&catalog), "rodada {round}");
        tokio::time::advance(6 * HOUR + Duration::from_secs(1)).await;
        assert!(!waiting(&catalog), "rodada {round}");
    }
    fail_times(&catalog, 1, "final").await;
    tokio::time::advance(5 * HOUR + 59 * MINUTE).await;
    assert!(waiting(&catalog), "o teto é 6 h, não menos");
}

#[tokio::test(start_paused = true)]
async fn sucesso_zera_a_contagem_e_a_espera_volta_aos_cinco_minutos() {
    let tracker = Tracker::new();
    let catalog = catalog(&tracker);
    fail_times(&catalog, 5, "a").await;
    tokio::time::advance(6 * MINUTE).await;
    fail_times(&catalog, 1, "b").await;
    assert_eq!(catalog.views()[0].health.breaker_trips, 2);

    tokio::time::advance(11 * MINUTE).await;
    tracker.broken.store(false, Ordering::SeqCst);
    catalog
        .search(ALL, &SearchQuery::general("ok"))
        .await
        .unwrap();
    let health = &catalog.views()[0].health;
    assert_eq!(health.consecutive_failures, 0);
    assert_eq!(health.breaker_trips, 0);

    tracker.broken.store(true, Ordering::SeqCst);
    fail_times(&catalog, 5, "c").await;
    tokio::time::advance(6 * MINUTE).await;
    assert!(!waiting(&catalog), "voltou à espera de 5 minutos");
}

#[tokio::test(start_paused = true)]
async fn teste_manual_fura_o_disjuntor_e_o_sucesso_dele_fecha() {
    let tracker = Tracker::new();
    let catalog = catalog(&tracker);
    fail_times(&catalog, 5, "a").await;
    assert!(waiting(&catalog));

    // Credencial consertada: o teste manual consulta mesmo em espera.
    tracker.broken.store(false, Ordering::SeqCst);
    assert_eq!(catalog.test("unico").await, Ok(1));
    assert!(!waiting(&catalog));
    assert_eq!(catalog.views()[0].health.consecutive_failures, 0);
}

//! Recuo por indexador depois de HTTP 429: o tracker pediu para parar, e o
//! catálogo não o consulta (nem busca, nem download) até o prazo vencer.
//!
//! O relógio é o do tokio, parado: o prazo anda com `tokio::time::advance`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use std::sync::Arc;

use acervo_api::{ALL, Catalog, Entry, TorznabError};
use acervo_indexers::{Capabilities, Indexer, IndexerError, Release, SearchQuery, SearchSupport};
use async_trait::async_trait;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);

/// Com `limited`, o indexador responde 429 (com `retry_after`, se houver).
#[derive(Debug)]
struct Tracker {
    name: &'static str,
    limited: AtomicBool,
    retry_after: Mutex<Option<Duration>>,
    searches: AtomicUsize,
    downloads: AtomicUsize,
}

impl Tracker {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            limited: AtomicBool::new(false),
            retry_after: Mutex::new(None),
            searches: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
        })
    }

    fn limit(&self, retry_after: Option<Duration>) {
        *self.retry_after.lock().unwrap() = retry_after;
        self.limited.store(true, Ordering::SeqCst);
    }

    fn release(&self) {
        self.limited.store(false, Ordering::SeqCst);
    }

    fn outcome(&self) -> Result<(), IndexerError> {
        if self.limited.load(Ordering::SeqCst) {
            return Err(IndexerError::RateLimited {
                indexer: self.name.into(),
                retry_after: *self.retry_after.lock().unwrap(),
            });
        }
        Ok(())
    }

    fn searches(&self) -> usize {
        self.searches.load(Ordering::SeqCst)
    }

    fn downloads(&self) -> usize {
        self.downloads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Indexer for Tracker {
    fn name(&self) -> &str {
        self.name
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.searches.fetch_add(1, Ordering::SeqCst);
        self.outcome()?;
        Ok(vec![Release {
            indexer: self.name.into(),
            guid: query.term.clone().unwrap_or_default(),
            title: format!("{} 2025 1080p", query.term.clone().unwrap_or_default()),
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

    fn proxies_downloads(&self) -> bool {
        true
    }

    async fn download(&self, _url: &url::Url) -> Result<Vec<u8>, IndexerError> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        self.outcome()?;
        Ok(b"d4:testee".to_vec())
    }
}

fn entry(tracker: &Arc<Tracker>) -> Entry {
    Entry {
        indexer: Arc::clone(tracker) as Arc<dyn Indexer>,
        capabilities: Capabilities {
            general: SearchSupport {
                available: true,
                supported_params: ["q".to_owned()].into(),
            },
            ..Capabilities::default()
        },
    }
}

fn link() -> url::Url {
    url::Url::parse("https://tracker.invalid/dl/1").unwrap()
}

fn query(term: &str) -> SearchQuery {
    SearchQuery::general(term)
}

fn waiting(catalog: &Catalog, name: &str) -> bool {
    catalog
        .views()
        .into_iter()
        .find(|view| view.name == name)
        .unwrap()
        .health
        .waiting_until()
        .is_some()
}

#[tokio::test(start_paused = true)]
async fn entra_em_espera_pula_na_busca_e_recusa_o_download() {
    let limited = Tracker::new("limitado");
    let healthy = Tracker::new("saudavel");
    limited.limit(None);
    let catalog = Catalog::new([entry(&limited), entry(&healthy)]).unwrap();

    // O 429 chega numa busca em que o outro indexador responde: a busca vale.
    let page = catalog.search(ALL, &query("um")).await.unwrap();
    assert_eq!(page.releases.len(), 1);
    assert_eq!(page.failures.len(), 1);
    assert!(waiting(&catalog, "limitado"));
    assert!(!waiting(&catalog, "saudavel"));
    assert_eq!(limited.searches(), 1);

    // Em espera: não é consultado e não conta como falha.
    let page = catalog.search(ALL, &query("dois")).await.unwrap();
    assert_eq!(page.releases.len(), 1);
    assert!(page.failures.is_empty());
    assert_eq!(limited.searches(), 1);
    assert_eq!(healthy.searches(), 2);

    // Busca direta no indexador em espera: todos os consultados estão fora.
    let error = catalog
        .search("limitado", &query("tres"))
        .await
        .unwrap_err();
    assert!(matches!(error, TorznabError::AllWaiting { .. }));
    assert_eq!(error.code(), 900);
    assert!(
        error
            .to_string()
            .contains("todos os indexadores em espera até")
    );
    assert_eq!(limited.searches(), 1);

    // O download recusa sem requisição.
    let error = catalog.download("limitado", &link()).await.unwrap_err();
    assert!(matches!(error, TorznabError::IndexerWaiting { .. }));
    let message = error.to_string();
    assert!(message.contains("indexador `limitado` em espera até "));
    assert!(message.contains("UTC por excesso de requisições"));
    assert_eq!(limited.downloads(), 0);

    // O outro segue servindo downloads.
    assert!(catalog.download("saudavel", &link()).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn todos_em_espera_viram_erro_e_o_prazo_vencido_volta_a_consultar() {
    let tracker = Tracker::new("unico");
    tracker.limit(None);
    let catalog = Catalog::new([entry(&tracker)]).unwrap();

    // A consulta que levou o 429 falhou, e é a única: erro de falha total.
    let error = catalog.search(ALL, &query("um")).await.unwrap_err();
    assert!(matches!(error, TorznabError::AllFailed(1)));

    let error = catalog.search(ALL, &query("um")).await.unwrap_err();
    assert!(matches!(error, TorznabError::AllWaiting { .. }));
    assert_eq!(tracker.searches(), 1, "o erro de espera não foi ao tracker");

    // Passados os 10 minutos, o indexador volta a ser consultado — e o erro
    // anterior não ficou guardado no cache de consulta.
    tracker.release();
    tokio::time::advance(10 * MINUTE + Duration::from_secs(1)).await;
    assert!(!waiting(&catalog, "unico"));
    let page = catalog.search(ALL, &query("um")).await.unwrap();
    assert_eq!(page.releases.len(), 1);
    assert_eq!(tracker.searches(), 2);
}

#[tokio::test(start_paused = true)]
async fn recuo_dobra_a_cada_429_seguido_e_sucesso_zera_a_sequencia() {
    let tracker = Tracker::new("unico");
    tracker.limit(None);
    let catalog = Catalog::new([entry(&tracker)]).unwrap();
    let _ = catalog.search(ALL, &query("a")).await;
    assert!(waiting(&catalog, "unico"));

    // 1º 429: 10 minutos.
    tokio::time::advance(9 * MINUTE).await;
    assert!(waiting(&catalog, "unico"));
    tokio::time::advance(2 * MINUTE).await;
    assert!(!waiting(&catalog, "unico"));

    // 2º 429 seguido: 20 minutos.
    let _ = catalog.search(ALL, &query("b")).await;
    tokio::time::advance(19 * MINUTE).await;
    assert!(waiting(&catalog, "unico"));
    tokio::time::advance(2 * MINUTE).await;
    assert!(!waiting(&catalog, "unico"));

    // 3º: 40 minutos.
    let _ = catalog.search(ALL, &query("c")).await;
    tokio::time::advance(39 * MINUTE).await;
    assert!(waiting(&catalog, "unico"));
    tokio::time::advance(2 * MINUTE).await;
    assert!(!waiting(&catalog, "unico"));

    // Um sucesso zera a sequência: o próximo 429 volta aos 10 minutos.
    tracker.release();
    catalog.search(ALL, &query("d")).await.unwrap();
    let views = catalog.views();
    assert_eq!(views[0].health.rate_limit_streak, 0);
    tracker.limit(None);
    let _ = catalog.search(ALL, &query("e")).await;
    tokio::time::advance(11 * MINUTE).await;
    assert!(!waiting(&catalog, "unico"));
}

#[tokio::test(start_paused = true)]
async fn recuo_calculado_tem_teto_de_seis_horas() {
    let tracker = Tracker::new("unico");
    tracker.limit(None);
    let catalog = Catalog::new([entry(&tracker)]).unwrap();

    // 10 min, 20, 40, 80, 160, 320 e depois 360 (teto) — duas vezes.
    for round in 0..8 {
        let _ = catalog.search(ALL, &query(&round.to_string())).await;
        assert!(waiting(&catalog, "unico"), "rodada {round}");
        tokio::time::advance(6 * HOUR + Duration::from_secs(1)).await;
        assert!(!waiting(&catalog, "unico"), "rodada {round}");
    }
    let _ = catalog.search(ALL, &query("final")).await;
    tokio::time::advance(5 * HOUR + 59 * MINUTE).await;
    assert!(waiting(&catalog, "unico"), "o teto é 6 h, não menos");
    tokio::time::advance(2 * MINUTE).await;
    assert!(!waiting(&catalog, "unico"));
}

#[tokio::test(start_paused = true)]
async fn retry_after_manda_na_duracao() {
    let tracker = Tracker::new("unico");
    tracker.limit(Some(Duration::from_secs(30)));
    let catalog = Catalog::new([entry(&tracker)]).unwrap();
    let _ = catalog.search(ALL, &query("a")).await;
    assert!(waiting(&catalog, "unico"));

    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(waiting(&catalog, "unico"));
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(!waiting(&catalog, "unico"));
}

#[tokio::test(start_paused = true)]
async fn download_com_429_poe_em_espera_e_o_teste_manual_tambem_recua() {
    let tracker = Tracker::new("unico");
    let catalog = Catalog::new([entry(&tracker)]).unwrap();
    tracker.limit(Some(HOUR));

    let error = catalog.download("unico", &link()).await.unwrap_err();
    assert!(matches!(error, TorznabError::IndexerWaiting { .. }));
    assert_eq!(tracker.downloads(), 1);

    assert!(catalog.download("unico", &link()).await.is_err());
    assert_eq!(tracker.downloads(), 1, "em espera, sem segunda requisição");

    let error = catalog.test("unico").await.unwrap_err();
    assert!(error.contains("em espera"));
    assert_eq!(tracker.searches(), 0);
}

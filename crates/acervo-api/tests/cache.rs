//! Consultas reaproveitadas: o tracker vê uma requisição por consulta, não
//! uma por consumidor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use acervo_api::{ALL, Catalog, Entry};
use acervo_indexers::{
    Capabilities, Category, Indexer, IndexerError, Release, SearchQuery, SearchSupport,
};
use async_trait::async_trait;

#[derive(Debug)]
struct Counting {
    name: String,
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl Default for Counting {
    fn default() -> Self {
        Self {
            name: "tracker".into(),
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl Indexer for Counting {
    fn name(&self) -> &str {
        &self.name
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Lento como tracker de verdade: dá tempo de o segundo pedido chegar
        // com o primeiro ainda a caminho.
        tokio::time::sleep(Duration::from_millis(50)).await;
        if self.fail.load(Ordering::SeqCst) {
            return Err(IndexerError::Transport {
                indexer: "tracker".into(),
                kind: "timeout",
            });
        }
        Ok(vec![Release {
            indexer: "tracker".into(),
            guid: "1".into(),
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
}

fn entry(indexer: Arc<Counting>) -> Entry {
    let support = SearchSupport {
        available: true,
        supported_params: ["q".to_owned()].into(),
    };
    Entry {
        indexer,
        capabilities: Capabilities {
            general: support.clone(),
            movie: support,
            categories: vec![Category {
                id: 2000,
                name: "Movies".into(),
                parent: None,
            }],
            ..Capabilities::default()
        },
    }
}

fn movie(term: &str) -> SearchQuery {
    SearchQuery::movie(term).with_categories([2000])
}

#[tokio::test]
async fn pedidos_simultaneos_iguais_viram_uma_requisicao() {
    let indexer = Arc::new(Counting::default());
    let catalog = Catalog::new([entry(Arc::clone(&indexer))]).unwrap();
    let query = movie("Filme");
    let (a, b) = tokio::join!(
        catalog.search(ALL, &query),
        catalog.search("tracker", &query)
    );
    assert_eq!(a.unwrap().releases.len(), 1);
    assert_eq!(b.unwrap().releases.len(), 1);
    assert_eq!(indexer.calls.load(Ordering::SeqCst), 1);

    // Pouco depois, o mesmo pedido vem do cache; outro termo vai ao tracker.
    catalog.search(ALL, &movie("Filme")).await.unwrap();
    assert_eq!(indexer.calls.load(Ordering::SeqCst), 1);
    catalog.search(ALL, &movie("Outro")).await.unwrap();
    assert_eq!(indexer.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn paginacao_diferente_usa_a_mesma_resposta() {
    let indexer = Arc::new(Counting::default());
    let catalog = Catalog::new([entry(Arc::clone(&indexer))]).unwrap();
    catalog
        .search(ALL, &movie("Filme").with_limit(10))
        .await
        .unwrap();
    catalog
        .search(ALL, &movie("Filme").with_offset(50))
        .await
        .unwrap();
    assert_eq!(indexer.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn erro_nao_fica_guardado() {
    let indexer = Arc::new(Counting::default());
    indexer.fail.store(true, Ordering::SeqCst);
    let catalog = Catalog::new([entry(Arc::clone(&indexer))]).unwrap();
    assert!(catalog.search(ALL, &movie("Filme")).await.is_err());
    indexer.fail.store(false, Ordering::SeqCst);
    assert!(catalog.search(ALL, &movie("Filme")).await.is_ok());
    assert_eq!(indexer.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn trocar_o_indexador_esquece_o_que_era_dele() {
    let old = Arc::new(Counting::default());
    let catalog = Catalog::new([entry(Arc::clone(&old))]).unwrap();
    catalog.search(ALL, &movie("Filme")).await.unwrap();

    let new = Arc::new(Counting::default());
    catalog.replace("tracker", entry(Arc::clone(&new))).unwrap();
    catalog.search(ALL, &movie("Filme")).await.unwrap();
    assert_eq!(new.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn busca_termina_mesmo_se_quem_pediu_desistir() {
    let indexer = Arc::new(Counting::default());
    let catalog = Catalog::new([entry(Arc::clone(&indexer))]).unwrap();
    // O gerenciador corta antes de o tracker responder...
    let gave_up = tokio::time::timeout(
        Duration::from_millis(10),
        catalog.search(ALL, &movie("Filme")),
    )
    .await;
    assert!(gave_up.is_err());
    // ...e a busca segue; a próxima tentativa encontra a resposta.
    tokio::time::sleep(Duration::from_millis(100)).await;
    catalog.search(ALL, &movie("Filme")).await.unwrap();
    assert_eq!(indexer.calls.load(Ordering::SeqCst), 1);
}

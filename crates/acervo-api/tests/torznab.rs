//! Contrato da superfície Torznab, falado por HTTP de verdade — do jeito que
//! um gerenciador de séries ou de filmes a consulta.

use std::sync::{Arc, Mutex};

use acervo_api::{Catalog, Entry, router};
use acervo_indexers::{
    Capabilities, Category, Indexer, IndexerError, Release, SearchMode, SearchQuery, SearchSupport,
};
use async_trait::async_trait;

const KEY: &str = "chave-de-teste-0123456789";

#[derive(Debug)]
struct Fake {
    name: &'static str,
    fail: bool,
    seen: Mutex<Vec<SearchQuery>>,
}

#[async_trait]
impl Indexer for Fake {
    fn name(&self) -> &str {
        self.name
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.seen.lock().unwrap().push(query.clone());
        if self.fail {
            return Err(IndexerError::Transport {
                indexer: self.name.into(),
                kind: "timeout",
            });
        }
        Ok((0..3)
            .map(|index| Release {
                indexer: self.name.into(),
                guid: format!("{}-{index}", self.name),
                title: format!("{}.S01E0{index}", self.name),
                download_url: url::Url::parse(&format!("https://{}.invalid/dl/{index}", self.name))
                    .unwrap(),
                info_url: None,
                size: 1024,
                published: None,
                seeders: Some(1),
                leechers: None,
                grabs: None,
                categories: vec![5040],
                tags: Vec::new(),
            })
            .collect())
    }
}

fn caps(tv: &[&str]) -> Capabilities {
    let support = |params: &[&str]| SearchSupport {
        available: true,
        supported_params: params.iter().map(|param| (*param).to_owned()).collect(),
    };
    Capabilities {
        general: support(&["q"]),
        tv: support(tv),
        categories: vec![Category {
            id: 5040,
            name: "TV/HD".into(),
            parent: Some(5000),
        }],
        ..Capabilities::default()
    }
}

fn fake(name: &'static str, fail: bool) -> Arc<Fake> {
    Arc::new(Fake {
        name,
        fail,
        seen: Mutex::new(Vec::new()),
    })
}

async fn serve(entries: Vec<Entry>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(Catalog::new(entries).unwrap(), KEY);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

async fn get(url: &str) -> (u16, String, String) {
    let response = reqwest::get(url).await.unwrap();
    let status = response.status().as_u16();
    let kind = response
        .headers()
        .get("content-type")
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (status, kind, response.text().await.unwrap())
}

#[tokio::test]
async fn chave_errada_nao_revela_nem_se_o_indexador_existe() {
    let base = serve(vec![Entry {
        indexer: fake("publico", false),
        capabilities: caps(&["q"]),
    }])
    .await;

    for url in [
        format!("{base}/publico/api?t=caps"),
        format!("{base}/publico/api?t=caps&apikey=errada"),
        format!("{base}/inexistente/api?t=caps&apikey=errada"),
    ] {
        let (status, _, body) = get(&url).await;
        assert_eq!(status, 401, "{url}");
        assert!(body.contains(r#"code="100""#));
    }

    let (status, _, body) = get(&format!("{base}/inexistente/api?t=caps&apikey={KEY}")).await;
    assert_eq!(status, 404);
    assert!(body.contains(r#"code="300""#));
}

#[tokio::test]
async fn caps_de_all_e_a_uniao_dos_indexadores() {
    let base = serve(vec![
        Entry {
            indexer: fake("um", false),
            capabilities: caps(&["q", "season", "ep"]),
        },
        Entry {
            indexer: fake("dois", false),
            capabilities: caps(&["q", "tvdbid"]),
        },
    ])
    .await;

    let (status, kind, body) = get(&format!("{base}/all/api?t=caps&apikey={KEY}")).await;
    assert_eq!(status, 200);
    assert!(kind.starts_with("application/xml"));
    assert!(body.contains(r#"<tv-search available="yes" supportedParams="ep,q,season,tvdbid"/>"#));
    assert!(body.contains(r#"<movie-search available="no""#));
    assert!(body.contains(r#"<subcat id="5040" name="TV/HD"/>"#));
}

#[tokio::test]
async fn busca_por_id_so_vai_a_quem_entende_o_id() {
    let by_term = fake("por-termo", false);
    let by_id = fake("por-id", false);
    let base = serve(vec![
        Entry {
            indexer: by_term.clone(),
            capabilities: caps(&["q", "season", "ep"]),
        },
        Entry {
            indexer: by_id.clone(),
            capabilities: caps(&["q", "season", "ep", "tvdbid"]),
        },
    ])
    .await;

    let (status, kind, body) = get(&format!(
        "{base}/all/api?t=tvsearch&tvdbid=42&season=1&ep=2&cat=5000&extended=1&apikey={KEY}"
    ))
    .await;

    assert_eq!(status, 200);
    assert!(kind.starts_with("application/rss+xml"));
    assert_eq!(body.matches("<item>").count(), 3);
    assert!(by_term.seen.lock().unwrap().is_empty());
    let seen = by_id.seen.lock().unwrap();
    assert_eq!(
        seen[0].mode,
        SearchMode::Tv {
            season: Some(1),
            episode: Some("2".into()),
            tvdb_id: Some(42),
            imdb_id: None,
        }
    );
}

#[tokio::test]
async fn falha_parcial_entrega_o_resto_e_falha_total_e_erro() {
    let base = serve(vec![
        Entry {
            indexer: fake("vivo", false),
            capabilities: caps(&["q"]),
        },
        Entry {
            indexer: fake("morto", true),
            capabilities: caps(&["q"]),
        },
    ])
    .await;

    let (status, _, body) = get(&format!("{base}/all/api?t=search&q=x&apikey={KEY}")).await;
    assert_eq!(status, 200);
    assert_eq!(body.matches("<item>").count(), 3);

    let (status, _, body) = get(&format!("{base}/morto/api?t=search&q=x&apikey={KEY}")).await;
    assert_eq!(status, 502);
    assert!(body.contains(r#"code="900""#));
}

#[tokio::test]
async fn paginacao_e_local_e_nao_repete_a_primeira_pagina() {
    let indexer = fake("publico", false);
    let base = serve(vec![Entry {
        indexer: indexer.clone(),
        capabilities: caps(&["q"]),
    }])
    .await;

    let (_, _, first) = get(&format!(
        "{base}/publico/api?t=search&q=x&limit=2&offset=0&apikey={KEY}"
    ))
    .await;
    let (_, _, second) = get(&format!(
        "{base}/publico/api?t=search&q=x&limit=2&offset=2&apikey={KEY}"
    ))
    .await;
    let (_, _, past_end) = get(&format!(
        "{base}/publico/api?t=search&q=x&limit=2&offset=100&apikey={KEY}"
    ))
    .await;

    assert_eq!(first.matches("<item>").count(), 2);
    assert_eq!(second.matches("<item>").count(), 1);
    assert_eq!(past_end.matches("<item>").count(), 0);
    assert!(
        indexer
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|query| query.offset == 0 && query.limit.is_none())
    );
}

#[tokio::test]
async fn chave_tambem_vale_no_cabecalho() {
    let base = serve(vec![Entry {
        indexer: fake("publico", false),
        capabilities: caps(&["q"]),
    }])
    .await;
    let response = reqwest::Client::new()
        .get(format!("{base}/publico/api?t=caps"))
        .header("X-Api-Key", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
}

#[test]
fn nome_reservado_ou_repetido_e_recusado() {
    let entry = |name| Entry {
        indexer: fake(name, false),
        capabilities: caps(&["q"]),
    };
    assert!(Catalog::new([entry("all")]).is_err());
    assert!(Catalog::new([entry("um"), entry("um")]).is_err());
    assert!(Catalog::new([entry("Com Espaço")]).is_err());
}

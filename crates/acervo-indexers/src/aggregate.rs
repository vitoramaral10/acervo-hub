use std::cmp::Reverse;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::join_all;

use crate::{IndexerError, Release, SearchQuery};

#[async_trait]
pub trait Indexer: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError>;

    /// Os links de download deste indexador só funcionam com a sessão dele?
    ///
    /// Tracker privado entrega `.torrent` só a quem está logado. Quem consulta
    /// não tem a sessão, então o download precisa passar por aqui.
    fn proxies_downloads(&self) -> bool {
        false
    }

    /// Baixa um `.torrent` com a sessão do indexador.
    ///
    /// # Errors
    ///
    /// Indexador que não intermedia downloads, link fora da origem dele,
    /// sessão recusada ou resposta que não é `.torrent`.
    async fn download(&self, url: &url::Url) -> Result<Vec<u8>, IndexerError> {
        let _ = url;
        Err(IndexerError::UnsupportedQuery {
            reason: "este indexador não intermedia downloads",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerFailure {
    pub indexer: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchReport {
    pub releases: Vec<Release>,
    pub failures: Vec<IndexerFailure>,
}

/// Consulta indexadores independentes em paralelo e preserva falhas parciais.
#[derive(Debug, Clone, Default)]
pub struct AggregateSearch {
    indexers: Vec<Arc<dyn Indexer>>,
}

impl AggregateSearch {
    #[must_use]
    pub fn new(indexers: Vec<Arc<dyn Indexer>>) -> Self {
        Self { indexers }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indexers.is_empty()
    }

    pub async fn search(&self, query: &SearchQuery) -> SearchReport {
        let pending = self.indexers.iter().map(|indexer| async move {
            let name = indexer.name().to_string();
            (name, indexer.search(query).await)
        });

        let mut report = SearchReport::default();
        for (indexer, result) in join_all(pending).await {
            match result {
                Ok(mut releases) => report.releases.append(&mut releases),
                Err(error) => report.failures.push(IndexerFailure {
                    indexer,
                    error: error.to_string(),
                }),
            }
        }

        report
            .releases
            .sort_by_key(|release| Reverse(release.published));
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeIndexer {
        name: &'static str,
        fail: bool,
        delay: std::time::Duration,
    }

    #[async_trait]
    impl Indexer for FakeIndexer {
        fn name(&self) -> &str {
            self.name
        }

        async fn search(&self, _query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
            tokio::time::sleep(self.delay).await;
            if self.fail {
                Err(IndexerError::InvalidRelease {
                    indexer: self.name.into(),
                    field: "title",
                })
            } else {
                Ok(vec![release(self.name)])
            }
        }
    }

    fn release(indexer: &str) -> Release {
        Release {
            indexer: indexer.into(),
            guid: format!("guid-passkey-secreta-{indexer}"),
            title: format!("release-{indexer}"),
            download_url: url::Url::parse("https://tracker.invalid/passkey-secreta").unwrap(),
            info_url: None,
            size: 42,
            published: None,
            seeders: Some(1),
            leechers: Some(0),
            grabs: None,
            categories: vec![5000],
            tags: Vec::new(),
        }
    }

    #[tokio::test]
    async fn falha_de_um_indexador_nao_apaga_o_resultado_dos_outros() {
        let aggregate = AggregateSearch::new(vec![
            Arc::new(FakeIndexer {
                name: "bom",
                fail: false,
                delay: std::time::Duration::ZERO,
            }),
            Arc::new(FakeIndexer {
                name: "ruim",
                fail: true,
                delay: std::time::Duration::ZERO,
            }),
        ]);

        let report = aggregate.search(&SearchQuery::general("teste")).await;

        assert_eq!(report.releases.len(), 1);
        assert_eq!(report.releases[0].indexer, "bom");
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].indexer, "ruim");
    }

    #[tokio::test(start_paused = true)]
    async fn indexadores_independentes_sao_consultados_em_paralelo() {
        let aggregate = AggregateSearch::new(vec![
            Arc::new(FakeIndexer {
                name: "um",
                fail: false,
                delay: std::time::Duration::from_secs(2),
            }),
            Arc::new(FakeIndexer {
                name: "dois",
                fail: false,
                delay: std::time::Duration::from_secs(2),
            }),
        ]);
        let start = tokio::time::Instant::now();

        let report = aggregate.search(&SearchQuery::general("teste")).await;

        assert_eq!(report.releases.len(), 2);
        assert_eq!(
            tokio::time::Instant::now() - start,
            std::time::Duration::from_secs(2)
        );
    }

    #[test]
    fn debug_da_release_nao_expoe_url_de_download() {
        let debug = format!("{:?}", release("tracker"));

        assert!(!debug.contains("passkey-secreta"));
        assert!(!debug.contains("tracker.invalid"));
    }
}

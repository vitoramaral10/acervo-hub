//! `search`: a busca manual, pela linha de comando.
//!
//! Serve para conferir um indexador antes de cadastrá-lo nos gerenciadores —
//! login, parser e categorias — com a mesma consulta que eles fariam.

use acervo_api::{ALL, Catalog};
use acervo_indexers::SearchQuery;
use anyhow::Result;

use crate::config::Config;

/// Executa a busca e imprime uma linha por release. Devolve quantos
/// indexadores falharam.
///
/// # Errors
///
/// Configuração incompleta, definição inválida ou todos os indexadores
/// consultados falharam.
pub async fn run(
    config: &Config,
    term: &str,
    indexer: Option<&str>,
    categories: &[u32],
) -> Result<usize> {
    config.server()?;
    let catalog = Catalog::new(crate::serve::entries(config).await?)?;
    let query = SearchQuery::general(term).with_categories(categories.iter().copied());
    let page = catalog
        .search(indexer.unwrap_or(ALL), &query)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    for release in &page.releases {
        println!(
            "{:<18} {:>9} {:>5}s {:<12} {}",
            release.indexer,
            human_size(release.size),
            release
                .seeders
                .map_or_else(|| "?".to_owned(), |seeders| seeders.to_string()),
            release
                .categories
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
            release.title
        );
    }
    for failure in &page.failures {
        println!("{:<18} falhou: {}", failure.indexer, failure.error);
    }
    println!(
        "{} resultados, {} indexadores com falha.",
        page.releases.len(),
        page.failures.len()
    );
    Ok(page.failures.len())
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut unit = 0;
    let mut whole = bytes;
    let mut remainder = 0;
    while whole >= 1024 && unit < UNITS.len() - 1 {
        remainder = whole % 1024;
        whole /= 1024;
        unit += 1;
    }
    // Uma casa decimal, sem ponto flutuante.
    format!("{whole}.{} {}", remainder * 10 / 1024, UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn tamanho_legivel() {
        assert_eq!(human_size(42), "42.0 B");
        assert_eq!(human_size(1_610_612_736), "1.5 GiB");
        assert_eq!(human_size(700 * 1024 * 1024), "700.0 MiB");
    }
}

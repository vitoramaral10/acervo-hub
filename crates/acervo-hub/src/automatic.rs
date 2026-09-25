//! Busca automática: o acervo decide e pega sozinho, como o gerenciador de
//! filmes fazia. Duas frentes, iguais às da referência — releases recentes
//! (RSS) casados com a biblioteca inteira, que também trazem os upgrades; e a
//! busca rotativa dos filmes que faltam.
//!
//! Desligada por padrão: enquanto o gerenciador estiver pegando, ligá-la
//! baixaria tudo duas vezes. Liga-se nas Configurações, no corte.

use std::collections::BTreeSet;

use acervo_api::{ALL, Catalog};
use acervo_indexers::SearchQuery;
use acervo_store::Store;
use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::shadow::Decider;

/// Onde a chave liga-desliga fica na tabela de configurações.
pub const KEY: &str = "busca.automatica";

/// Categoria Newznab de filmes.
const MOVIES: u32 = 2000;

/// A busca automática está ligada?
///
/// # Errors
///
/// Banco inalcançável.
pub async fn enabled(store: &Store) -> Result<bool> {
    Ok(store.setting(KEY).await?.as_deref() == Some("true"))
}

/// Um grab da sincronização de RSS.
#[derive(Debug, Serialize)]
pub struct RssGrab {
    pub filme: String,
    pub release: String,
    pub erro: Option<String>,
}

/// Uma sincronização de RSS: os releases recentes de todos os indexadores,
/// decididos contra a biblioteca; de cada filme, o melhor aprovado vai ao
/// cliente (filme com arquivo, só se for upgrade). Sem `apply`, só diz.
///
/// # Errors
///
/// Catálogo vazio, nenhum indexador ou busca que falhou em todos.
pub async fn rss(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    apply: bool,
) -> Result<Vec<RssGrab>> {
    let decider = Decider::load(config, store, catalog).await?;
    let page = catalog
        .search(ALL, &SearchQuery::general("").with_categories([MOVIES]))
        .await
        .map_err(|error| anyhow::anyhow!("RSS: {error}"))?;
    let outcome = decider.rss(page.releases);
    let movies = store.movies().await?;
    let mut taken = BTreeSet::new();
    let mut grabs = Vec::new();
    // A ordem já é a de preferência: o primeiro aprovado de cada filme é o
    // melhor dele.
    for decision in outcome.decisions.iter().filter(|d| d.approved()) {
        let Some(movie_id) = decision.movie else {
            continue;
        };
        if !taken.insert(movie_id) {
            continue;
        }
        let release = &outcome.releases[decision.release];
        let entry = movies.iter().find(|m| m.id == movie_id);
        let filme = entry.map_or_else(|| movie_id.to_string(), |m| m.movie.title.clone());
        let mut line = RssGrab {
            filme,
            release: release.title.clone(),
            erro: None,
        };
        if apply {
            let quality = decision
                .parsed
                .as_ref()
                .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality);
            let replaces =
                entry.and_then(|m| m.movie.file.as_ref().map(|f| f.relative_path.clone()));
            if let Err(error) =
                crate::grab::send(config, store, catalog, movie_id, release, quality, replaces)
                    .await
            {
                line.erro = Some(format!("{error:#}"));
            }
        }
        grabs.push(line);
    }
    Ok(grabs)
}

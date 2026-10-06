//! Busca automática: o acervo decide e pega sozinho. Duas frentes — os
//! releases recentes (RSS), casados com a biblioteca inteira, e a busca
//! rotativa dos filmes que faltam, em [`crate::decide`].

use std::collections::{BTreeSet, HashMap};

use crate::api::{ALL, Catalog};
use acervo_indexers::SearchQuery;
use acervo_store::Store;
use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::decide::Decider;

/// Categoria Newznab de filmes.
const MOVIES: u32 = 2000;

/// Um grab da sincronização de RSS.
#[derive(Debug, Serialize)]
pub struct RssGrab {
    pub filme: String,
    pub release: String,
    pub erro: Option<String>,
}

/// Uma sincronização de RSS: os releases recentes de todos os indexadores,
/// decididos contra a biblioteca; de cada filme, o melhor aprovado vai ao
/// cliente. Filme com download em andamento fica de fora; o que só espera
/// na fila, sem ter começado, dá lugar ao novo (a decisão só aprova o que
/// supera a fila).
///
/// # Errors
///
/// Catálogo vazio, nenhum indexador ou busca que falhou em todos.
pub async fn rss(config: &Config, store: &Store, catalog: &Catalog) -> Result<Vec<RssGrab>> {
    let decider = Decider::load(store, catalog).await?;
    let page = catalog
        .search(ALL, &SearchQuery::general("").with_categories([MOVIES]))
        .await
        .map_err(|error| anyhow::anyhow!("RSS: {error}"))?;
    let outcome = decider.rss(page.releases);
    let movies = store.movies().await?;
    let mut downloading: HashMap<i64, Vec<acervo_store::Grab>> = HashMap::new();
    for grab in store.grabs().await? {
        if grab.state == acervo_store::GrabState::Downloading {
            downloading.entry(grab.movie_id).or_default().push(grab);
        }
    }
    let waiting = if downloading.is_empty() {
        std::collections::HashSet::new()
    } else {
        crate::grab::waiting(config).await
    };
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
        let olds = downloading.get(&movie_id).map_or(&[][..], Vec::as_slice);
        if !olds.iter().all(|g| waiting.contains(&g.hash)) {
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
        let quality = decision
            .parsed
            .as_ref()
            .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality);
        let replaces = entry.and_then(|m| m.movie.file.as_ref().map(|f| f.relative_path.clone()));
        let swapping: Vec<i64> = olds.iter().map(|g| g.id).collect();
        match crate::grab::send(
            config, store, catalog, movie_id, release, quality, replaces, &swapping,
        )
        .await
        {
            Ok(()) => {
                if let Err(error) = crate::grab::swap_out(config, store, olds, &release.title).await
                {
                    tracing::warn!(filme = line.filme, "troca na fila: {error:#}");
                }
            }
            Err(error) => line.erro = Some(format!("{error:#}")),
        }
        grabs.push(line);
    }
    Ok(grabs)
}

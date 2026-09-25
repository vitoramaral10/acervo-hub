//! Listas de importação: filmes de uma pessoa, de uma coleção ou de uma
//! lista do TMDB entram sozinhos no catálogo, com o perfil, a pasta e o
//! monitoramento da lista. Filme excluído nunca volta.

use std::time::Duration;

use acervo_api::Catalog;
use acervo_metadata::{MovieSummary, Tmdb};
use acervo_store::{ImportList, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::library::AddRequest;
use crate::shadow::now_rfc3339;

/// O que uma sincronização fez.
#[derive(Debug, Default, Serialize)]
pub struct SyncReport {
    pub lista: String,
    pub encontrados: usize,
    pub adicionados: Vec<String>,
    pub ja_no_catalogo: usize,
    pub excluidos: usize,
    /// Ainda sem data de lançamento conhecida: ficam para depois.
    pub sem_data: usize,
    pub falhas: Vec<(String, String)>,
}

/// Os filmes que a lista traz hoje.
///
/// # Errors
///
/// Configuração da lista incompleta ou TMDB inalcançável.
pub async fn movies(tmdb: &Tmdb, list: &ImportList) -> Result<Vec<MovieSummary>> {
    let settings = &list.settings;
    let id = |name: &str| -> Result<u32> {
        settings[name]
            .as_u64()
            .or_else(|| settings[name].as_str().and_then(|s| s.trim().parse().ok()))
            .and_then(|v| u32::try_from(v).ok())
            .with_context(|| format!("a lista não tem `{name}`"))
    };
    Ok(match list.kind.as_str() {
        "tmdb_person" => {
            let departments: Vec<&str> = settings["departamentos"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d.as_str())
                .collect();
            tmdb.person_movies(
                id("pessoa")?,
                settings["elenco"].as_bool().unwrap_or(true),
                &departments,
            )
            .await?
        }
        "tmdb_collection" => tmdb.collection_movies(id("colecao")?).await?,
        "tmdb_list" => {
            let list_id = settings["lista"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| settings["lista"].as_u64().map(|v| v.to_string()))
                .context("a lista não tem `lista`")?;
            tmdb.list_movies(&list_id).await?
        }
        other => bail!("tipo de lista desconhecido: `{other}`"),
    })
}

/// Sincroniza uma lista: adiciona o que falta, pula o que já está e o que
/// foi excluído. Filme sem ano (anunciado sem data) espera a próxima.
///
/// # Errors
///
/// TMDB sem chave ou inalcançável, ou lista mal configurada.
pub async fn sync(
    config: &Config,
    store: &Store,
    catalog: Option<&Catalog>,
    list: &ImportList,
) -> Result<SyncReport> {
    let tmdb = crate::metadata::require_tmdb(config, store).await?;
    let result = sync_with(config, store, catalog, &tmdb, list).await;
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    store
        .set_import_list_sync(list.id, &now_rfc3339(), error.as_deref())
        .await?;
    result
}

async fn sync_with(
    config: &Config,
    store: &Store,
    catalog: Option<&Catalog>,
    tmdb: &Tmdb,
    list: &ImportList,
) -> Result<SyncReport> {
    let found = movies(tmdb, list).await?;
    let (catalog_movies, exclusions, profiles) =
        tokio::try_join!(store.movies(), store.exclusions(), store.profile_ids())?;
    let profile = list
        .quality_profile_id
        .and_then(|id| profiles.iter().find(|(pid, _)| *pid == id))
        .map(|(_, name)| name.clone())
        .context("a lista não tem perfil de qualidade")?;
    let mut report = SyncReport {
        lista: list.name.clone(),
        encontrados: found.len(),
        ..SyncReport::default()
    };
    for movie in found {
        if catalog_movies
            .iter()
            .any(|m| m.movie.tmdb_id == movie.tmdb_id)
        {
            report.ja_no_catalogo += 1;
            continue;
        }
        if exclusions.iter().any(|e| e.tmdb_id == movie.tmdb_id) {
            report.excluidos += 1;
            continue;
        }
        if movie.year.is_none() {
            report.sem_data += 1;
            continue;
        }
        let label = crate::events::label(&movie.title, movie.year);
        let request = AddRequest {
            tmdb_id: movie.tmdb_id,
            quality_profile: profile.clone(),
            root_folder: list.root_folder.clone(),
            monitored: list.monitor,
            minimum_availability: list.minimum_availability.clone(),
            tags: list.tags.clone(),
        };
        match crate::library::add_anywhere(config, store, tmdb, &request, list.search_on_add).await
        {
            Ok(id) => {
                report.adicionados.push(label);
                let manager_decides = crate::rules::owner(config, store).await? == "radarr";
                if list.search_on_add
                    && !manager_decides
                    && let Some(catalog) = catalog
                    && let Err(error) = crate::grab::grab(config, store, catalog, id, true).await
                {
                    tracing::info!(filme = id, "busca ao adicionar da lista: {error:#}");
                }
            }
            Err(error) => report.falhas.push((label, format!("{error:#}"))),
        }
    }
    Ok(report)
}

/// Sincroniza todas as listas ligadas.
///
/// # Errors
///
/// Banco inalcançável. Falha de uma lista fica no relato dela.
pub async fn sync_all(
    config: &Config,
    store: &Store,
    catalog: Option<&Catalog>,
) -> Result<Vec<SyncReport>> {
    let mut reports = Vec::new();
    for list in store.import_lists().await?.iter().filter(|l| l.enabled) {
        match sync(config, store, catalog, list).await {
            Ok(report) => reports.push(report),
            Err(error) => reports.push(SyncReport {
                lista: list.name.clone(),
                falhas: vec![(String::new(), format!("{error:#}"))],
                ..SyncReport::default()
            }),
        }
    }
    Ok(reports)
}

/// Sincroniza as listas a cada 12 horas. A primeira rodada espera o serviço
/// assentar.
pub async fn sync_loop(
    config: std::sync::Arc<Config>,
    database: crate::serve::Database,
    catalog: Catalog,
) {
    let mut every = tokio::time::interval(Duration::from_secs(12 * 3600));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::time::sleep(Duration::from_secs(600)).await;
    loop {
        every.tick().await;
        let Ok(store) = database.get() else {
            continue;
        };
        // Enquanto o gerenciador decide, as listas são dele.
        if crate::rules::owner(&config, store).await.ok() != Some("acervo") {
            continue;
        }
        match sync_all(&config, store, Some(&catalog)).await {
            Ok(reports) => {
                for report in reports {
                    tracing::info!(
                        lista = report.lista,
                        encontrados = report.encontrados,
                        adicionados = report.adicionados.len(),
                        falhas = report.falhas.len(),
                        "lista de importação"
                    );
                }
            }
            Err(error) => tracing::warn!("listas de importação: {error:#}"),
        }
    }
}

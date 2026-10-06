//! A API da tela de séries, sob `/ui/api/biblioteca/series`, com a mesma
//! entrada das rotas de filmes (sessão ou chave, e o cabeçalho anti-CSRF em
//! ação que muda estado).

// Handler devolve a resposta de erro pronta; é o formato do axum.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use acervo_decision::EpisodeState;
use acervo_store::{CatalogSeries, GrabState, SeriesGrab, SeriesSearch, Skip};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use super::library::{AddRequest, Monitor, SeriesEdit};
use super::search::{interactive, interactive_query};
use crate::decide::now_rfc3339;
use crate::web::{
    HistoryQuery, SEARCH_TTL, Shared, Web, WebError, WebResult, age_hours, anyhow_bad, bad, enter,
    fail, ok, yes,
};

pub fn router(web: Arc<Web>) -> Router {
    routes().with_state(web)
}

/// As rotas, ainda sem o estado.
pub(crate) fn routes() -> Router<Arc<Web>> {
    Router::new()
        .route("/ui/api/biblioteca/series", get(list).post(add))
        .route("/ui/api/biblioteca/series/buscar-tmdb", get(tmdb_search))
        .route(
            "/ui/api/biblioteca/series/{id}",
            get(detail).patch(edit).delete(remove),
        )
        .route(
            "/ui/api/biblioteca/series/{id}/episodios/apagar",
            post(delete_episodes),
        )
        .route(
            "/ui/api/biblioteca/series/{id}/episodios/skip",
            post(set_skip),
        )
        .route("/ui/api/biblioteca/series/{id}/buscar", post(search))
        .route("/ui/api/biblioteca/series/{id}/pegar", post(grab))
        .route(
            "/ui/api/biblioteca/series/{id}/buscar-agora",
            post(search_now),
        )
        .route("/ui/api/biblioteca/series/{id}/historico", get(history))
        .route("/ui/api/biblioteca/series/{id}/renomear", post(rename))
        .route("/ui/api/biblioteca/series/{id}/verificar", post(verify))
}

/// `?aplicar=true` faz; sem ele, só mostra o plano.
#[derive(Deserialize, Default)]
pub(crate) struct ApplyQuery {
    #[serde(default)]
    pub aplicar: bool,
}

async fn rename(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<ApplyQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let entry = store
        .series(id)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| fail(bad("série fora do catálogo")))?;
    let disk = crate::rename::disk_subtitles(&entry.series.path).await;
    let plan = crate::rename::series_plan(&entry, &disk);
    let plan = if q.aplicar {
        crate::rename::apply_series(store, &entry, plan)
            .await
            .map_err(|e| fail(anyhow_bad(&e)))?
    } else {
        plan
    };
    ok(&json!({
        "aplicado": q.aplicar,
        "renomeados": plan.iter().filter(|r| r.feito).count(),
        "erros": plan.iter().filter(|r| r.erro.is_some()).count(),
        "plano": plan,
    }))
}

async fn verify(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<ApplyQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let check = crate::verify::series(store, id, q.aplicar, crate::verify::Mode::MANUAL)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&serde_json::to_value(&check).map_err(|e| fail(bad(e)))?)
}

fn grid(poster: Option<&String>) -> Option<String> {
    poster.map(|p| p.replacen("/t/p/original/", "/t/p/w342/", 1))
}

fn skip_name(skip: Skip) -> &'static str {
    match skip {
        Skip::Unwanted => "unwanted",
        Skip::Deleted => "deleted",
    }
}

/// Quero, Tenho, Dispensado e quantos dos Quero estão baixando.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    wanted: usize,
    aired: usize,
    have: usize,
    skipped: usize,
    queued: usize,
}

impl Totals {
    fn add(&mut self, state: EpisodeState, aired: bool) {
        match state {
            EpisodeState::Have(_) => self.have += 1,
            EpisodeState::Skipped => self.skipped += 1,
            EpisodeState::Wanted | EpisodeState::Queued => {
                self.wanted += 1;
                if aired {
                    self.aired += 1;
                }
                if state == EpisodeState::Queued {
                    self.queued += 1;
                }
            }
        }
    }

    fn json(self) -> Value {
        json!({
            "quero": self.wanted,
            "quero_exibidos": self.aired,
            "tenho": self.have,
            "dispensado": self.skipped,
            "baixando": self.queued,
        })
    }
}

/// O resumo de uma série, como a lista mostra.
fn summary(entry: &CatalogSeries, queued: &HashSet<i64>) -> Value {
    let today = super::today();
    let mut totals = Totals::default();
    for episode in &entry.episodes {
        totals.add(
            super::state(entry, episode, queued),
            super::aired(&episode.episode, today),
        );
    }
    let series = &entry.series;
    json!({
        "id": entry.id,
        "tmdb": series.tmdb_id,
        "tvdb": series.tvdb_id,
        "imdb": series.imdb_id,
        "titulo": series.title,
        "titulo_original": series.original_title,
        "titulo_ingles": series.metadata_title,
        "ano": series.year,
        "status": series.status,
        "rede": series.network,
        "poster": grid(series.poster.as_ref()),
        "fundo": series.fanart,
        "pasta": series.path,
        "pasta_de_temporada": series.season_folder,
        "monitorar_novos": series.monitor_new,
        "prioritario": entry.priority,
        "adicionada": series.added,
        "atualizada": series.refreshed_at,
        "episodios": totals.json(),
        "tamanho": entry.files.iter().map(|f| f.file.size).sum::<u64>(),
    })
}

fn last_search(run: &SeriesSearch) -> Value {
    json!({
        "quando": run.at,
        "consultas": run.queries,
        "releases": run.releases,
        "escolhidos": run.picks.iter().map(|p| json!({
            "titulo": p.title,
            "indexador": p.indexer,
            "qualidade": p.quality.name(),
            "tamanho": p.size,
            "episodios": p.episode_ids,
        })).collect::<Vec<_>>(),
        "motivos": run.rejections,
        "erro": run.error,
    })
}

async fn list(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (list, grabs) =
        tokio::try_join!(store.series_list(), store.series_grabs()).map_err(|e| fail(bad(e)))?;
    let queued = super::queued(&grabs);
    let items: Vec<Value> = list.iter().map(|entry| summary(entry, &queued)).collect();
    ok(&json!({ "series": items }))
}

async fn detail(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (entry, grabs, searches) = tokio::try_join!(
        store.series(id),
        store.series_grabs(),
        store.latest_series_searches()
    )
    .map_err(|e| fail(bad(e)))?;
    let entry = entry.ok_or_else(|| fail(bad("série fora do catálogo")))?;
    let queued = super::queued(&grabs);
    let today = super::today();
    let mut seasons: BTreeMap<u16, (Totals, Vec<Value>)> = BTreeMap::new();
    for episode in &entry.episodes {
        let state = super::state(&entry, episode, &queued);
        let aired = super::aired(&episode.episode, today);
        let file = episode
            .file_id
            .and_then(|id| entry.files.iter().find(|f| f.id == id));
        let (totals, items) = seasons.entry(episode.episode.season).or_default();
        totals.add(state, aired);
        items.push(json!({
            "id": episode.id,
            "numero": episode.episode.number,
            "titulo": episode.episode.title,
            "data": episode.episode.air_date,
            "exibido": aired,
            "sinopse": episode.episode.overview,
            "duracao": episode.episode.runtime,
            "estado": match state {
                EpisodeState::Have(_) => "tenho",
                EpisodeState::Skipped => "dispensado",
                EpisodeState::Wanted | EpisodeState::Queued => "quero",
            },
            "baixando": state == EpisodeState::Queued,
            "motivo": episode.skip.map(skip_name),
            "motivo_em": episode.skipped_at,
            "qualidade": file.map(|f| f.file.quality.quality.name()),
            "arquivo": file.map(|f| json!({
                "id": f.id,
                "nome": f.file.relative_path,
                "tamanho": f.file.size,
                "qualidade": f.file.quality.quality.name(),
                "idiomas": f.file.languages,
                "grupo": f.file.release_group,
                "release": f.file.scene_name,
                "adicionado": f.file.date_added,
            })),
        }));
    }
    let mut body = summary(&entry, &queued);
    body["sinopse"] = json!(entry.series.overview);
    body["idioma_original"] = json!(entry.series.original_language);
    body["duracao"] = json!(entry.series.runtime);
    body["titulos_alternativos"] = json!(entry.series.alternate_titles);
    body["temporadas"] = json!(
        seasons
            .into_iter()
            .map(|(number, (totals, episodes))| json!({
                "numero": number,
                "episodios": episodes,
                "totais": totals.json(),
            }))
            .collect::<Vec<_>>()
    );
    body["downloads"] = json!(
        grabs
            .iter()
            .filter(|g| g.series_id == id && g.state == GrabState::Downloading)
            .map(|g| json!({
                "id": g.id,
                "release": g.title,
                "qualidade": g.quality.name(),
                "tamanho": g.size,
                "mensagem": g.message,
                "pego_em": g.grabbed_at,
                "episodios": g.episode_ids,
            }))
            .collect::<Vec<_>>()
    );
    body["ultima_busca"] = searches
        .iter()
        .find(|run| run.series_id == id)
        .map_or(Value::Null, last_search);
    ok(&body)
}

#[derive(Deserialize)]
struct TmdbQuery {
    #[serde(alias = "termo")]
    q: String,
}

async fn tmdb_search(
    State(web): Shared,
    headers: HeaderMap,
    Query(query): Query<TmdbQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let term = query.q.trim();
    if term.is_empty() {
        return ok(&json!({ "resultados": [] }));
    }
    // "tmdb:123", "tvdb:123" ou o título.
    let id = if let Some(id) = term.strip_prefix("tmdb:") {
        id.trim().parse().ok()
    } else if let Some(id) = term.strip_prefix("tvdb:") {
        match id.trim().parse() {
            Ok(external) => tmdb.find_tvdb(external).await.map_err(|e| fail(bad(e)))?,
            Err(_) => None,
        }
    } else {
        None
    };
    let results = if term.starts_with("tmdb:") || term.starts_with("tvdb:") {
        match id {
            Some(id) => match tmdb.series(id).await {
                Ok(meta) => vec![acervo_metadata::SeriesSummary {
                    tmdb_id: id,
                    title: meta.localized_title.unwrap_or(meta.title),
                    original_title: meta.original_title,
                    year: meta.year,
                    overview: meta.overview,
                    poster: grid(meta.poster.as_ref()),
                }],
                Err(_) => Vec::new(),
            },
            None => Vec::new(),
        }
    } else {
        tmdb.search_series(term).await.map_err(|e| fail(bad(e)))?
    };
    let list = store.series_list().await.map_err(|e| fail(bad(e)))?;
    let items: Vec<Value> = results
        .into_iter()
        .map(|r| {
            let existing = list
                .iter()
                .find(|s| s.series.tmdb_id == r.tmdb_id)
                .map(|s| s.id);
            json!({
                "tmdb": r.tmdb_id,
                "titulo": r.title,
                "titulo_original": r.original_title,
                "ano": r.year,
                "sinopse": r.overview,
                "poster": r.poster,
                "no_catalogo": existing,
            })
        })
        .collect();
    ok(&json!({ "resultados": items }))
}

#[derive(Deserialize)]
struct AddBody {
    tmdb: u32,
    #[serde(default = "yes")]
    monitorar_novos: bool,
    #[serde(default = "yes")]
    pasta_de_temporada: bool,
    /// O que buscar: `tudo`, `ultima_temporada` ou `proximos`.
    #[serde(default)]
    buscar: Monitor,
    /// Busca logo depois de adicionar.
    #[serde(default)]
    buscar_agora: bool,
}

async fn add(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<AddBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let request = AddRequest {
        tmdb_id: body.tmdb,
        monitor_new: body.monitorar_novos,
        season_folder: body.pasta_de_temporada,
        monitor: body.buscar,
    };
    let id = super::library::add(&web.config(), store, &tmdb, &request)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    if body.buscar_agora {
        spawn_search(&web, id);
    }
    ok(&json!({ "id": id }))
}

/// A busca automática de uma série, em segundo plano: o resultado fica na
/// última busca do detalhe e no histórico.
fn spawn_search(web: &Arc<Web>, id: i64) {
    let web = Arc::clone(web);
    tokio::spawn(async move {
        let Ok(store) = web.database.get() else {
            return;
        };
        match super::search::series_now(&web.config(), store, &web.catalog, id, None).await {
            Ok(line) => {
                tracing::info!(serie = line.serie, pegou = ?line.escolhidos, "busca da série");
            }
            Err(error) => tracing::info!(serie = id, "busca da série: {error:#}"),
        }
    });
}

async fn edit(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(change): axum::Json<SeriesEdit>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::PATCH).await?;
    super::library::edit(store, id, &change)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    apagar_arquivos: bool,
}

async fn remove(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<RemoveQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    super::library::remove(&web.config(), store, id, q.apagar_arquivos)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true }))
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

async fn delete_episodes(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Ids>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    if body.ids.is_empty() {
        return Err(fail(bad("nenhum episódio escolhido")));
    }
    let removal = super::remove::delete_episodes(&web.config(), store, id, &body.ids)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let mut value = serde_json::to_value(&removal).map_err(|e| fail(bad(e)))?;
    value["ok"] = json!(true);
    ok(&value)
}

#[derive(Deserialize)]
struct SkipBody {
    ids: Vec<i64>,
    /// `null` devolve à busca ("Quero de novo"); `"unwanted"` dispensa.
    skip: Option<String>,
}

async fn set_skip(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<SkipBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let skip = match body.skip.as_deref() {
        None => None,
        Some("unwanted") => Some(Skip::Unwanted),
        Some(other) => {
            return Err(fail(bad(format!(
                "skip `{other}` inválido: use null ou \"unwanted\""
            ))));
        }
    };
    let entry = store
        .series(id)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| fail(bad("série fora do catálogo")))?;
    if body
        .ids
        .iter()
        .any(|e| !entry.episodes.iter().any(|known| known.id == *e))
    {
        return Err(fail(bad("há episódio que não é desta série")));
    }
    let changed = store
        .set_skip(&body.ids, skip, &now_rfc3339())
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true, "alterados": changed }))
}

#[derive(Deserialize, Default)]
struct SearchBody {
    #[serde(default, alias = "season")]
    temporada: Option<u16>,
    /// Ids de episódio.
    #[serde(default, alias = "episodes")]
    episodios: Vec<i64>,
}

async fn search(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    body: Option<axum::Json<SearchBody>>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let body = body.map(|b| b.0).unwrap_or_default();
    let entry = store
        .series(id)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| fail(bad("série fora do catálogo")))?;
    let query = interactive_query(&entry, body.temporada, &body.episodios);
    let found = interactive(store, &web.catalog, id, query)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let code = |ids: &[i64]| {
        let numbers: Vec<(u16, u16)> = entry
            .episodes
            .iter()
            .filter(|e| ids.contains(&e.id))
            .map(|e| (e.episode.season, e.episode.number))
            .collect();
        super::episode_code(&numbers)
    };
    let list: Vec<Value> = found
        .decisions
        .iter()
        .map(|decision| {
            let release = &found.releases[decision.release];
            json!({
                "guid": release.guid,
                "titulo": release.title,
                "indexador": release.indexer,
                "tamanho": release.size,
                "seeders": release.seeders,
                "leechers": release.leechers,
                "idade_horas": age_hours(release),
                "qualidade": decision.quality.map(|q| q.quality.name()),
                "aprovado": decision.approved(),
                "seria_pego": found.picked.contains(&decision.release),
                "outra_serie": decision.series.is_some_and(|s| s != id),
                "episodios": code(&decision.covers),
                "episodio_ids": decision.covers,
                "quero": decision.wanted,
                "motivos": decision.rejections.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "info": release.info_url.as_ref().map(ToString::to_string),
            })
        })
        .collect();
    web.series_searches
        .lock()
        .await
        .insert(id, (Instant::now(), found.releases));
    ok(&json!({ "consulta": query.label(), "releases": list }))
}

#[derive(Deserialize)]
struct GrabBody {
    guid: String,
}

async fn grab(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<GrabBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let release = {
        let mut searches = web.series_searches.lock().await;
        searches.retain(|_, (at, _)| at.elapsed() < SEARCH_TTL);
        searches
            .get(&id)
            .and_then(|(_, releases)| releases.iter().find(|r| r.guid == body.guid).cloned())
    }
    .ok_or_else(|| fail(bad("a busca expirou; busque de novo")))?;
    let grab_id = super::grab::send_chosen(&web.config(), store, &web.catalog, id, &release)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(&json!({ "ok": true, "titulo": release.title, "download": grab_id }))
}

async fn search_now(State(web): Shared, Path(id): Path<i64>, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    if store.series(id).await.map_err(|e| fail(bad(e)))?.is_none() {
        return Err(fail(WebError(
            axum::http::StatusCode::NOT_FOUND,
            "série fora do catálogo".into(),
        )));
    }
    spawn_search(&web, id);
    ok(&json!({ "iniciada": true }))
}

async fn history(
    State(web): Shared,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let size = q.tamanho.clamp(1, 250);
    let page = store
        .series_history(
            id,
            q.evento.as_deref().filter(|e| !e.is_empty()),
            size,
            (q.pagina.max(1) - 1) * size,
        )
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "total": page.total, "eventos": page.events }))
}

/// Um download de série na fila geral, no formato dos de filme. O `id` leva
/// o prefixo `serie-`: os ids de grab de filme e de série são contados à
/// parte, e o de série sai por `DELETE /fila/series/{grab_id}`.
pub(crate) fn queue_item(
    grab: &SeriesGrab,
    list: &[CatalogSeries],
    torrent: Option<&acervo_clients::TorrentInfo>,
) -> Value {
    let entry = list.iter().find(|s| s.id == grab.series_id);
    let label = entry.map(|e| super::label(e, &grab.episode_ids));
    json!({
        "id": format!("serie-{}", grab.id),
        "tipo": "serie",
        "grab_id": grab.id,
        "serie_id": grab.series_id,
        "filme_id": null,
        "filme": label,
        "titulo": label,
        "episodios": grab.episode_ids,
        "poster": entry.and_then(|e| grid(e.series.poster.as_ref())),
        "release": grab.title,
        "indexador": grab.indexer,
        "qualidade": grab.quality.name(),
        "tamanho": grab.size,
        "pego_em": grab.grabbed_at,
        "mensagem": grab.message,
        "no_cliente": torrent.is_some(),
        "progresso": torrent.map(|t| t.progress),
        "estado_cliente": torrent.map(|t| t.state.clone()),
        "velocidade": torrent.map(|t| t.dlspeed),
        "restante_segundos": torrent.map(|t| t.eta).filter(|e| *e > 0 && *e < 8_640_000),
        "seeds": torrent.map(|t| t.num_seeds),
        "upgrade": false,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn rotas_de_series_convivem_com_as_de_filmes() {
        // Rota repetida ou ambígua faz o axum entrar em pânico ao juntar.
        let _ = crate::web::routes()
            .merge(super::routes())
            .merge(crate::agenda::routes())
            .merge(crate::marks::routes())
            .merge(crate::discover::routes());
    }
}

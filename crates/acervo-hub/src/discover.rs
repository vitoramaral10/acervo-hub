//! Descoberta pelo TMDB; o cache guarda só a origem, nunca os filtros do usuário.

// Os handlers devolvem a resposta pronta, seguindo as outras rotas da interface.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use acervo_metadata::{
    DiscoverDetails, DiscoverItem, DiscoverKind, DiscoverList, DiscoverPage, Genre, Tmdb,
};
use acervo_store::{DiscoverKind as StoredKind, Store};
use anyhow::{Result, bail};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, Method},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::{Date, Weekday};

use crate::decide::now_rfc3339;
use crate::web::{Shared, Web, WebResult, anyhow_bad, bad, enter, fail, ok};

const DETAIL_TTL: Duration = Duration::from_secs(6 * 3600);
const SEARCH_TTL: Duration = Duration::from_secs(10 * 60);
const WEEK_TTL: Duration = Duration::from_secs(6 * 3600);
const LIST_TTL: Duration = Duration::from_secs(3600);
/// Títulos visíveis que uma chamada de lista tenta entregar.
const LIST_FILL: usize = 20;
/// Páginas do TMDB lidas, no máximo, por chamada de lista.
const LIST_SCAN: u32 = 10;
/// O TMDB não serve lista além da página 500.
const LIST_PAGES: u32 = 500;
const GENRE_TTL: Duration = Duration::from_secs(24 * 3600);

type Cache<T> = LazyLock<Mutex<HashMap<String, (Instant, T)>>>;
static DETAILS: Cache<DiscoverDetails> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SEARCHES: Cache<DiscoverPage> = LazyLock::new(|| Mutex::new(HashMap::new()));
static WEEKS: Cache<Vec<DiscoverItem>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static LISTS: Cache<DiscoverPage> = LazyLock::new(|| Mutex::new(HashMap::new()));
static GENRES: Cache<Vec<Genre>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached<T: Clone>(cache: &Cache<T>, key: &str, ttl: Duration) -> Option<T> {
    let mut entries = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries.retain(|_, (at, _)| at.elapsed() < ttl);
    entries.get(key).map(|(_, value)| value.clone())
}

fn save<T: Clone>(cache: &Cache<T>, key: String, value: &T) {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, (Instant::now(), value.clone()));
}

async fn genres(tmdb: &Tmdb) -> Result<Vec<Genre>> {
    if let Some(genres) = cached(&GENRES, "generos", GENRE_TTL) {
        return Ok(genres);
    }
    let genres = tmdb.genres().await?;
    save(&GENRES, "generos".into(), &genres);
    Ok(genres)
}

fn stored_kind(kind: DiscoverKind) -> StoredKind {
    match kind {
        DiscoverKind::Movie => StoredKind::Filme,
        DiscoverKind::Series => StoredKind::Serie,
    }
}

fn parse_kind(kind: &str) -> Result<StoredKind> {
    match kind {
        "filme" => Ok(StoredKind::Filme),
        "serie" => Ok(StoredKind::Serie),
        _ => bail!("tipo deve ser filme ou serie"),
    }
}

fn valid_id(id: u32) -> Result<()> {
    if id == 0 || id > i32::MAX.cast_unsigned() {
        bail!("id fora do intervalo válido");
    }
    Ok(())
}

fn valid_year(year: i32) -> Result<()> {
    if !(1900..=2100).contains(&year) {
        bail!("ano deve estar entre 1900 e 2100");
    }
    Ok(())
}

fn week_dates(year: i32, week: u8) -> Result<(String, String)> {
    valid_year(year)?;
    let start = Date::from_iso_week_date(year, week, Weekday::Monday)?;
    let end = start + time::Duration::days(6);
    Ok((start.to_string(), end.to_string()))
}

fn week_key(year: i32, week: u8) -> String {
    format!("{year}-{week}")
}

#[derive(Debug)]
struct Filters {
    titles: HashSet<(StoredKind, u32)>,
    movies: HashSet<u32>,
    series: HashSet<u32>,
}

impl Filters {
    async fn load(store: &Store) -> Result<Self> {
        let titles = store
            .hidden_titles()
            .await?
            .into_iter()
            .map(|t| (t.kind, t.tmdb_id))
            .collect();
        let (movies, series) = store.catalog_tmdb_ids().await?;
        Ok(Self {
            titles,
            movies,
            series,
        })
    }

    fn visible(&self, item: &DiscoverItem) -> bool {
        !self
            .titles
            .contains(&(stored_kind(item.kind), item.tmdb_id))
            && !(match item.kind {
                DiscoverKind::Movie => &self.movies,
                DiscoverKind::Series => &self.series,
            })
            .contains(&item.tmdb_id)
    }

    fn items(&self, items: &[DiscoverItem], genres: &[Genre]) -> Vec<Value> {
        let names: HashMap<_, _> = genres.iter().map(|g| (g.id, &g.name)).collect();
        items
            .iter()
            .filter(|item| self.visible(item))
            .map(|item| item_json(item, &names))
            .collect()
    }
}

fn item_json(item: &DiscoverItem, names: &HashMap<u32, &String>) -> Value {
    json!({
        "tipo": stored_kind(item.kind).as_str(), "tmdb": item.tmdb_id,
        "titulo": item.title, "titulo_original": item.original_title,
        "data": item.date, "ano": item.year, "sinopse": item.overview,
        "poster": item.poster, "nota": item.vote_average, "popularidade": item.popularity,
        "generos": item.genre_ids.iter().filter_map(|id| names.get(id)).collect::<Vec<_>>(),
    })
}

fn in_catalog(filters: &Filters, item: &DiscoverItem) -> bool {
    match item.kind {
        DiscoverKind::Movie => &filters.movies,
        DiscoverKind::Series => &filters.series,
    }
    .contains(&item.tmdb_id)
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "first_page")]
    pagina: u32,
}
fn search_args(query: &SearchQuery) -> Result<&str> {
    let q = query.q.trim();
    if !(1..=100).contains(&q.chars().count()) {
        bail!("busca deve ter de 1 a 100 caracteres");
    }
    if !(1..=500).contains(&query.pagina) {
        bail!("página deve estar entre 1 e 500");
    }
    Ok(q)
}

async fn search(
    State(web): Shared,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let q = search_args(&query).map_err(|e| fail(anyhow_bad(&e)))?;
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let key = format!("{}:{q}", query.pagina);
    let page = if let Some(page) = cached(&SEARCHES, &key, SEARCH_TTL) {
        page
    } else {
        let page = tmdb
            .discover_search(q, query.pagina)
            .await
            .map_err(|e| fail(bad(e)))?;
        save(&SEARCHES, key, &page);
        page
    };
    let genres = genres(&tmdb).await.map_err(|e| fail(anyhow_bad(&e)))?;
    let names = genres.iter().map(|genre| (genre.id, &genre.name)).collect();
    let filters = Filters::load(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let items: Vec<_> = page
        .items
        .iter()
        .map(|item| {
            let mut value = item_json(item, &names);
            value["no_acervo"] = json!(in_catalog(&filters, item));
            value["oculto"] = json!(
                filters
                    .titles
                    .contains(&(stored_kind(item.kind), item.tmdb_id))
            );
            value
        })
        .collect();
    ok(&json!({"itens": items, "pagina": page.page, "total_paginas": page.total_pages}))
}

async fn details(
    State(web): Shared,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, u32)>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let stored = parse_kind(&kind).map_err(|e| fail(anyhow_bad(&e)))?;
    valid_id(id).map_err(|e| fail(anyhow_bad(&e)))?;
    let kind = match stored {
        StoredKind::Filme => DiscoverKind::Movie,
        StoredKind::Serie => DiscoverKind::Series,
    };
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let key = format!("{}:{id}", stored.as_str());
    let detail = if let Some(detail) = cached(&DETAILS, &key, DETAIL_TTL) {
        detail
    } else {
        let detail = tmdb
            .discover_details(kind, id)
            .await
            .map_err(|e| fail(bad(e)))?;
        save(&DETAILS, key, &detail);
        detail
    };
    let filters = Filters::load(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let local_id = store
        .catalog_id_by_tmdb(stored, id)
        .await
        .map_err(|e| fail(bad(e)))?;
    let genres = genres(&tmdb).await.map_err(|e| fail(anyhow_bad(&e)))?;
    let mut value = item_json(&detail.item, &HashMap::new());
    let extra = json!({
        "generos": detail.genres, "tagline": detail.tagline, "backdrop": detail.backdrop,
        "duracao": detail.runtime, "temporadas": detail.number_of_seasons, "episodios": detail.number_of_episodes,
        "status": detail.status, "diretores": detail.directors, "criadores": detail.creators,
        "elenco": detail.cast.iter().map(|person| json!({"nome": person.name, "personagem": person.character, "foto": person.profile})).collect::<Vec<_>>(),
        "trailer": detail.trailer, "recomendacoes": filters.items(&detail.recommendations, &genres),
        "no_acervo": local_id.is_some(), "id_acervo": local_id,
        "oculto": filters.titles.contains(&(stored, id)),
    });
    value
        .as_object_mut()
        .expect("objeto de título")
        .extend(extra.as_object().expect("objeto de detalhes").clone());
    ok(&value)
}

pub fn router(web: Arc<Web>) -> Router {
    routes().with_state(web)
}

pub(crate) fn routes() -> Router<Arc<Web>> {
    Router::new()
        .route("/ui/api/descobrir/titulo/{tipo}/{tmdb}", get(details))
        .route("/ui/api/descobrir/busca", get(search))
        .route("/ui/api/descobrir/semanas/{ano}/{semana}", get(week))
        .route("/ui/api/descobrir/listas/{lista}", get(list))
        .route("/ui/api/descobrir/generos", get(genre_list))
        .route("/ui/api/descobrir/ocultos", get(hidden))
        .route("/ui/api/descobrir/ocultos/titulos", post(hide_title))
        .route(
            "/ui/api/descobrir/ocultos/titulos/{tipo}/{tmdb}",
            delete(unhide_title),
        )
}

async fn week(
    State(web): Shared,
    headers: HeaderMap,
    Path((year, week)): Path<(i32, u8)>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (start, end) = week_dates(year, week).map_err(|e| fail(anyhow_bad(&e)))?;
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let key = week_key(year, week);
    let items = if let Some(items) = cached(&WEEKS, &key, WEEK_TTL) {
        items
    } else {
        let items = tmdb
            .releases_br(&start, &end)
            .await
            .map_err(|e| fail(bad(e)))?;
        save(&WEEKS, key, &items);
        items
    };
    let genres = genres(&tmdb).await.map_err(|e| fail(anyhow_bad(&e)))?;
    let filters = Filters::load(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    ok(
        &json!({"ano": year, "semana": week, "inicio": start, "fim": end, "itens": filters.items(&items, &genres)}),
    )
}

#[derive(Deserialize)]
struct ListQuery {
    tipo: String,
    #[serde(default = "first_page")]
    pagina: u32,
}
fn first_page() -> u32 {
    1
}

fn list_args(name: &str, query: &ListQuery) -> Result<(DiscoverList, DiscoverKind)> {
    let kind = match parse_kind(&query.tipo)? {
        StoredKind::Filme => DiscoverKind::Movie,
        StoredKind::Serie => DiscoverKind::Series,
    };
    let list = match name {
        "em_alta" => DiscoverList::Trending,
        "populares" => DiscoverList::Popular,
        "em_breve" if kind == DiscoverKind::Movie => DiscoverList::Upcoming,
        "no_ar" if kind == DiscoverKind::Series => DiscoverList::OnTheAir,
        _ => bail!("lista inválida para este tipo de obra"),
    };
    if query.pagina == 0 || query.pagina > 500 {
        bail!("página deve estar entre 1 e 500");
    }
    Ok((list, kind))
}

async fn list(
    State(web): Shared,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<ListQuery>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (list, kind) = list_args(&name, &query).map_err(|e| fail(anyhow_bad(&e)))?;
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let genres = genres(&tmdb).await.map_err(|e| fail(anyhow_bad(&e)))?;
    let filters = Filters::load(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    // Com quase tudo oculto ou no acervo, uma página do TMDB sai vazia: segue
    // lendo até encher a grade, e a próxima chamada continua de onde parou.
    let mut items = Vec::new();
    let mut number = query.pagina;
    let mut total = number;
    for read in 0..LIST_SCAN {
        let key = format!("{name}-{}-{number}", query.tipo);
        let page = if let Some(page) = cached(&LISTS, &key, LIST_TTL) {
            page
        } else {
            let page = tmdb
                .discover_list(list, kind, number)
                .await
                .map_err(|e| fail(bad(e)))?;
            save(&LISTS, key, &page);
            page
        };
        total = page.total_pages.min(LIST_PAGES);
        items.extend(filters.items(&page.items, &genres));
        if items.len() >= LIST_FILL || number >= total || read + 1 == LIST_SCAN {
            break;
        }
        number += 1;
    }
    ok(&json!({"itens": items, "pagina": number, "total_paginas": total}))
}

async fn genre_list(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let tmdb = crate::metadata::require_tmdb(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let genres = genres(&tmdb).await.map_err(|e| fail(anyhow_bad(&e)))?;
    ok(
        &json!({"generos": genres.iter().map(|g| json!({"id": g.id, "nome": g.name})).collect::<Vec<_>>()}),
    )
}

async fn hidden(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let titles = store.hidden_titles().await.map_err(|e| fail(bad(e)))?;
    ok(&json!({
        "titulos": titles.iter().map(|t| json!({"tipo": t.kind.as_str(), "tmdb": t.tmdb_id, "titulo": t.title, "em": t.hidden_at})).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct TitleBody {
    tipo: String,
    tmdb: u32,
    titulo: String,
}
async fn hide_title(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<TitleBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let kind = parse_kind(&body.tipo).map_err(|e| fail(anyhow_bad(&e)))?;
    valid_id(body.tmdb).map_err(|e| fail(anyhow_bad(&e)))?;
    store
        .hide_title(kind, body.tmdb, &body.titulo, &now_rfc3339())
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({"ok": true}))
}

async fn unhide_title(
    State(web): Shared,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, u32)>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::DELETE).await?;
    let kind = parse_kind(&kind).map_err(|e| fail(anyhow_bad(&e)))?;
    valid_id(id).map_err(|e| fail(anyhow_bad(&e)))?;
    let removed = store
        .unhide_title(kind, id)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({"ok": true, "removido": removed}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busca_valida_caracteres_e_paginas() {
        let query = SearchQuery {
            q: "  série  ".into(),
            pagina: 1,
        };
        assert_eq!(search_args(&query).unwrap(), "série");
        for q in [String::new(), "   ".into(), "á".repeat(101)] {
            assert!(search_args(&SearchQuery { q, pagina: 1 }).is_err());
        }
        assert!(
            search_args(&SearchQuery {
                q: "á".repeat(100),
                pagina: 500
            })
            .is_ok()
        );
        for pagina in [0, 501] {
            assert!(
                search_args(&SearchQuery {
                    q: "filme".into(),
                    pagina
                })
                .is_err()
            );
        }
        assert!(valid_id(0).is_err());
        assert!(valid_id(u32::MAX).is_err());
        assert!(parse_kind("pessoa").is_err());
    }

    #[test]
    fn filtros_das_recomendacoes_e_indicadores_da_busca() {
        let filters = Filters {
            titles: HashSet::from([(StoredKind::Filme, 1)]),
            movies: HashSet::from([2]),
            series: HashSet::from([3]),
        };
        let mut item = DiscoverItem {
            kind: DiscoverKind::Movie,
            tmdb_id: 1,
            title: "Filme".into(),
            original_title: String::new(),
            date: None,
            year: None,
            overview: String::new(),
            poster: None,
            vote_average: 0.0,
            popularity: 0.0,
            genre_ids: vec![],
        };
        assert!(!filters.visible(&item));
        assert!(!in_catalog(&filters, &item));
        item.tmdb_id = 2;
        assert!(in_catalog(&filters, &item));
        assert!(!filters.visible(&item));
        item.kind = DiscoverKind::Series;
        assert!(!in_catalog(&filters, &item));
        assert!(filters.visible(&item));
        item.tmdb_id = 3;
        assert!(in_catalog(&filters, &item));
        item.tmdb_id = 4;
        item.genre_ids = vec![18];
        assert!(filters.visible(&item));
        assert_eq!(filters.items(&[item.clone()], &[]).len(), 1);
        // A serialização da busca preserva obras que os filtros das listas ocultam.
        assert_eq!(item_json(&item, &HashMap::new())["tmdb"], 4);
    }

    #[test]
    fn semanas_iso_e_validacao() {
        assert_eq!(time::util::weeks_in_year(2026), 53);
        assert_eq!(
            week_dates(2026, 1).unwrap(),
            ("2025-12-29".into(), "2026-01-04".into())
        );
        assert_eq!(
            week_dates(2026, 53).unwrap(),
            ("2026-12-28".into(), "2027-01-03".into())
        );
        assert!(week_dates(2025, 53).is_err());
        assert!(week_dates(2026, 0).is_err());
        assert!(week_dates(1899, 1).is_err());
        assert!(week_dates(2101, 1).is_err());
        for (list, kind) in [
            ("em_breve", "serie"),
            ("no_ar", "filme"),
            ("outra", "filme"),
            ("populares", "outro"),
        ] {
            assert!(
                list_args(
                    list,
                    &ListQuery {
                        tipo: kind.into(),
                        pagina: 1
                    }
                )
                .is_err()
            );
        }
    }
}

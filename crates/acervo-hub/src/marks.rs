//! A tela "Para apagar", sob `/ui/api/biblioteca/para-apagar`, com a mesma
//! entrada das outras rotas da biblioteca (sessão ou chave, e o cabeçalho
//! anti-CSRF em ação que muda estado).
//!
//! Nada sai do disco sozinho por ter sido assistido: o usuário marca um
//! filme, uma temporada ou a série inteira, vê o espaço que cada marca
//! libera e confirma. Apagar reaproveita os caminhos de sempre: o filme sai
//! como no "Remover" com arquivos e download; a temporada e a série, como no
//! "Apagar temporada" do detalhe (só os arquivos; a série fica no catálogo,
//! com os episódios apagados fora da busca). As sugestões são o que a regra
//! antiga de assistidos apagaria, calculadas na hora a partir do Jellyfin:
//! nada guardado que possa envelhecer.

// Handler devolve a resposta de erro pronta; é o formato do axum.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use acervo_clients::jellyfin::JellyfinClient;
use acervo_store::{CatalogMovie, CatalogSeries, DeletionMark, MarkTarget, Store};
use anyhow::Result;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::web::{Shared, Web, WebResult, anyhow_bad, bad, enter, fail, ok};

/// O motivo que vai à frente da mensagem do evento de filme removido.
const REASON: &str = "marcado para apagar";

pub fn router(web: Arc<Web>) -> Router {
    routes().with_state(web)
}

/// As rotas, ainda sem o estado.
pub(crate) fn routes() -> Router<Arc<Web>> {
    Router::new()
        .route("/ui/api/biblioteca/para-apagar", get(list))
        .route("/ui/api/biblioteca/para-apagar/marcar", post(mark))
        .route("/ui/api/biblioteca/para-apagar/desmarcar", post(unmark))
        .route("/ui/api/biblioteca/para-apagar/apagar", post(purge_route))
        .route("/ui/api/biblioteca/para-apagar/sugestoes", get(suggestions))
}

// ---------------------------------------------------------------- regra

/// O que apagar uma marca de série tira do disco.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scope {
    /// Os episódios com arquivo — os que o "Apagar temporada" manda.
    pub episode_ids: Vec<i64>,
    /// Arquivos distintos: um multi-episódio conta uma vez.
    pub files: usize,
    /// Bytes desses arquivos. Multi-episódio que passa da temporada sai
    /// inteiro, e conta inteiro.
    pub size: u64,
}

/// O alcance de uma marca de série: a temporada `season`, ou todas.
#[must_use]
pub fn series_scope(entry: &CatalogSeries, season: Option<u16>) -> Scope {
    let mut episode_ids = Vec::new();
    let mut file_ids = BTreeSet::new();
    for episode in &entry.episodes {
        if season.is_some_and(|s| s != episode.episode.season) {
            continue;
        }
        if let Some(file_id) = episode.file_id {
            episode_ids.push(episode.id);
            file_ids.insert(file_id);
        }
    }
    let size = entry
        .files
        .iter()
        .filter(|f| file_ids.contains(&f.id))
        .map(|f| f.file.size)
        .sum();
    Scope {
        episode_ids,
        files: file_ids.len(),
        size,
    }
}

/// O tamanho do arquivo do filme; sem arquivo, nada a liberar.
fn movie_size(entry: &CatalogMovie) -> u64 {
    entry.movie.file.as_ref().map_or(0, |f| f.size)
}

/// "Temporada 2"; a zero é a dos especiais, como na tela da série.
fn season_name(season: u16) -> String {
    if season == 0 {
        "Especiais".into()
    } else {
        format!("Temporada {season}")
    }
}

fn grid(poster: Option<&String>) -> Option<String> {
    poster.map(|p| p.replacen("/t/p/original/", "/t/p/w342/", 1))
}

/// A chave estável de um alvo, para a tela e para o relatório.
fn key(target: MarkTarget) -> String {
    match target {
        MarkTarget::Movie(id) => format!("filme-{id}"),
        MarkTarget::Series {
            series_id,
            season: None,
        } => format!("serie-{series_id}"),
        MarkTarget::Series {
            series_id,
            season: Some(season),
        } => format!("serie-{series_id}-t{season}"),
    }
}

/// O alvo como a tela o manda e o recebe.
fn target_json(target: MarkTarget) -> Value {
    match target {
        MarkTarget::Movie(id) => json!({ "tipo": "filme", "filme": id }),
        MarkTarget::Series {
            series_id,
            season: None,
        } => json!({ "tipo": "serie", "serie": series_id }),
        MarkTarget::Series {
            series_id,
            season: Some(season),
        } => json!({ "tipo": "temporada", "serie": series_id, "temporada": season }),
    }
}

/// Uma marca, pronta para a tela; `None` se o dono sumiu do catálogo entre
/// as leituras.
fn describe(
    mark: &DeletionMark,
    movies: &[CatalogMovie],
    series: &[CatalogSeries],
) -> Option<Value> {
    let (title, detail, poster, files, size) = match mark.target {
        MarkTarget::Movie(id) => {
            let entry = movies.iter().find(|m| m.id == id)?;
            (
                crate::events::label(&entry.movie.title, entry.movie.year),
                None,
                grid(entry.extras.poster.as_ref()),
                usize::from(entry.movie.file.is_some()),
                movie_size(entry),
            )
        }
        MarkTarget::Series { series_id, season } => {
            let entry = series.iter().find(|s| s.id == series_id)?;
            let scope = series_scope(entry, season);
            (
                crate::events::label(&entry.series.title, entry.series.year),
                Some(season.map_or_else(|| "Série inteira".into(), season_name)),
                grid(entry.series.poster.as_ref()),
                scope.files,
                scope.size,
            )
        }
    };
    let mut item = target_json(mark.target);
    item["chave"] = json!(key(mark.target));
    item["titulo"] = json!(title);
    item["detalhe"] = json!(detail);
    item["poster"] = json!(poster);
    item["arquivos"] = json!(files);
    item["tamanho"] = json!(size);
    item["marcado_em"] = json!(mark.marked_at);
    Some(item)
}

// ---------------------------------------------------------------- apagar

/// Uma marca apagada.
#[derive(Debug, Clone, Serialize)]
pub struct Purged {
    pub chave: String,
    pub titulo: String,
    pub tamanho: u64,
}

/// Uma marca que não saiu, e por quê. A marca fica.
#[derive(Debug, Clone, Serialize)]
pub struct Failed {
    pub chave: String,
    pub titulo: String,
    pub erro: String,
}

/// O resultado de apagar os marcados.
#[derive(Debug, Default, Serialize)]
pub struct Purge {
    pub apagados: Vec<Purged>,
    pub falhas: Vec<Failed>,
    /// Bytes dos arquivos que saíram.
    pub liberado: u64,
    /// O que saiu mas deixou pendência, como torrent que ficou para o ciclo
    /// de limpeza.
    pub avisos: Vec<String>,
}

/// O título e os bytes do que saiu, ou o título e o erro.
type Outcome = std::result::Result<(String, u64), (String, String)>;

/// Tira o filme como o "Remover" com arquivos e download; a marca sai em
/// cascata com ele.
///
/// Download que não saiu do cliente vira aviso, e o tamanho não conta como
/// liberado: com hardlink, o espaço só volta quando o torrent sair.
async fn purge_movie(
    config: &Config,
    store: &Store,
    entry: &CatalogMovie,
    warnings: &mut Vec<String>,
) -> Outcome {
    let title = crate::events::label(&entry.movie.title, entry.movie.year);
    let size = movie_size(entry);
    match crate::library::remove_because(config, store, entry.id, true, true, Some(REASON)).await {
        Ok(None) => Ok((title, size)),
        Ok(Some(download)) => {
            warnings.push(format!(
                "{title}: biblioteca apagada, mas o download ficou no cliente ({download}); \
                 o espaço volta quando o torrent sair"
            ));
            Ok((title, 0))
        }
        Err(error) => {
            tracing::warn!(filme = title, "marcado não apagado: {error:#}");
            Err((title, format!("{error:#}")))
        }
    }
}

/// Apaga os arquivos da temporada ou da série como o "Apagar temporada" e
/// tira a marca, que a série, no catálogo, não leva embora.
async fn purge_series(
    config: &Config,
    store: &Store,
    series_id: i64,
    season: Option<u16>,
    warnings: &mut Vec<String>,
) -> Outcome {
    let target = MarkTarget::Series { series_id, season };
    // Lida a cada marca: outra temporada da mesma série pode ter acabado de
    // sair.
    let entry = match store.series(series_id).await {
        Ok(Some(entry)) => entry,
        Ok(None) => return Err((key(target), "série fora do catálogo".into())),
        Err(error) => return Err((key(target), error.to_string())),
    };
    let title = format!(
        "{} · {}",
        crate::events::label(&entry.series.title, entry.series.year),
        season.map_or_else(|| "série inteira".into(), season_name)
    );
    let scope = series_scope(&entry, season);
    if !scope.episode_ids.is_empty() {
        match crate::series::remove::delete_episodes(config, store, series_id, &scope.episode_ids)
            .await
        {
            Ok(removal) => {
                if let Some(warning) = removal.aviso {
                    warnings.push(format!("{title}: {warning}"));
                }
            }
            Err(error) => {
                tracing::warn!(serie = title, "marcado não apagado: {error:#}");
                return Err((title, format!("{error:#}")));
            }
        }
    }
    Ok((title, scope.size))
}

/// Apaga os alvos pedidos, um a um: falha num não segura os outros. Só sai
/// o que está marcado: cada marca é tomada (apagada) logo antes do item, então
/// desmarcar durante o lote vence; se o item falha, a marca volta.
///
/// # Errors
///
/// Banco ilegível antes de começar.
pub async fn purge(config: &Config, store: &Store, targets: &[MarkTarget]) -> Result<Purge> {
    let movies = store.movies().await?;
    let mut report = Purge::default();
    let mut seen = HashSet::new();
    for &target in targets {
        if !seen.insert(target) {
            continue;
        }
        let chave = key(target);
        // Tomar a marca é o que autoriza apagar: quem desmarcou antes ganha.
        let claimed = match store.unmark(&[target]).await {
            Ok(removed) => removed > 0,
            Err(error) => {
                report.falhas.push(Failed {
                    chave: chave.clone(),
                    titulo: chave.clone(),
                    erro: format!("marca ilegível: {error}"),
                });
                continue;
            }
        };
        let outcome = if claimed {
            match target {
                MarkTarget::Movie(id) => match movies.iter().find(|m| m.id == id) {
                    Some(entry) => purge_movie(config, store, entry, &mut report.avisos).await,
                    None => Err((chave.clone(), "filme fora do catálogo".into())),
                },
                MarkTarget::Series { series_id, season } => {
                    purge_series(config, store, series_id, season, &mut report.avisos).await
                }
            }
        } else {
            Err((chave.clone(), "não está marcado para apagar".into()))
        };
        match outcome {
            Ok((titulo, tamanho)) => {
                report.liberado += tamanho;
                report.apagados.push(Purged {
                    chave,
                    titulo,
                    tamanho,
                });
            }
            Err((titulo, erro)) => {
                // Não saiu: a marca tomada volta, para tentar de novo.
                if claimed
                    && let Err(error) = store
                        .mark_for_deletion(&[target], &crate::decide::now_rfc3339())
                        .await
                {
                    report.avisos.push(format!(
                        "{titulo}: não apagado e a marca não voltou: {error}"
                    ));
                }
                report.falhas.push(Failed {
                    chave,
                    titulo,
                    erro,
                });
            }
        }
    }
    // O Jellyfin só vê que os arquivos sumiram na próxima varredura.
    if !report.apagados.is_empty()
        && let Some(jellyfin) = config.jellyfin()
    {
        let refreshed = match JellyfinClient::new(
            &jellyfin.url,
            &jellyfin.api_key,
            crate::config::HTTP_TIMEOUT,
        ) {
            Ok(client) => client.refresh_library().await.map_err(|e| e.to_string()),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = refreshed {
            tracing::warn!("varredura do Jellyfin: {error}");
            report
                .avisos
                .push(format!("varredura do Jellyfin não pedida: {error}"));
        }
    }
    Ok(report)
}

// ---------------------------------------------------------------- rotas

/// Um alvo como a tela manda: `{"filme": 1}`, `{"serie": 2}` (a série
/// inteira) ou `{"serie": 2, "temporada": 1}`. O `tipo`, que a listagem
/// devolve, é aceito e ignorado: o alvo sai dos ids.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetBody {
    #[serde(default)]
    filme: Option<i64>,
    #[serde(default)]
    serie: Option<i64>,
    #[serde(default)]
    temporada: Option<u16>,
    #[serde(default, rename = "tipo")]
    _kind: Option<String>,
}

impl TargetBody {
    fn target(&self) -> Result<MarkTarget, String> {
        match (self.filme, self.serie, self.temporada) {
            (Some(id), None, None) => Ok(MarkTarget::Movie(id)),
            (None, Some(series_id), season) => Ok(MarkTarget::Series { series_id, season }),
            _ => Err(
                "cada item é um filme (`filme`) ou uma série (`serie`, com ou sem \
                      `temporada`)"
                    .into(),
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TargetsBody {
    itens: Vec<TargetBody>,
}

impl TargetsBody {
    fn targets(&self) -> Result<Vec<MarkTarget>, String> {
        if self.itens.is_empty() {
            return Err("nenhum item escolhido".into());
        }
        self.itens.iter().map(TargetBody::target).collect()
    }
}

async fn list(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (marks, movies, series) =
        tokio::try_join!(store.deletion_marks(), store.movies(), store.series_list())
            .map_err(|e| fail(bad(e)))?;
    let items: Vec<Value> = marks
        .iter()
        .filter_map(|mark| describe(mark, &movies, &series))
        .collect();
    let total: u64 = items.iter().filter_map(|i| i["tamanho"].as_u64()).sum();
    ok(&json!({ "itens": items, "total": total }))
}

/// Confere que cada alvo existe no catálogo: o banco também recusaria, mas
/// com uma mensagem que não ajuda ninguém.
async fn check(store: &Store, targets: &[MarkTarget]) -> Result<(), String> {
    let movies: Option<Vec<CatalogMovie>> =
        if targets.iter().any(|t| matches!(t, MarkTarget::Movie(_))) {
            Some(store.movies().await.map_err(|e| e.to_string())?)
        } else {
            None
        };
    for target in targets {
        match *target {
            MarkTarget::Movie(id) => {
                if !movies.iter().flatten().any(|m| m.id == id) {
                    return Err(format!("filme {id} fora do catálogo"));
                }
            }
            MarkTarget::Series { series_id, season } => {
                let entry = store
                    .series(series_id)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("série {series_id} fora do catálogo"))?;
                if let Some(season) = season
                    && !entry.episodes.iter().any(|e| e.episode.season == season)
                {
                    return Err(format!(
                        "a série {} não tem a temporada {season}",
                        entry.series.title
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn mark(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<TargetsBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let targets = body.targets().map_err(|e| fail(bad(e)))?;
    check(store, &targets).await.map_err(|e| fail(bad(e)))?;
    let added = store
        .mark_for_deletion(&targets, &now_rfc3339())
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true, "marcados": added }))
}

async fn unmark(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<TargetsBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let targets = body.targets().map_err(|e| fail(bad(e)))?;
    let removed = store.unmark(&targets).await.map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true, "desmarcados": removed }))
}

async fn purge_route(
    State(web): Shared,
    headers: HeaderMap,
    axum::Json(body): axum::Json<TargetsBody>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let targets = body.targets().map_err(|e| fail(bad(e)))?;
    let report = purge(&web.config(), store, &targets)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let mut value = serde_json::to_value(&report).map_err(|e| fail(bad(e)))?;
    value["ok"] = json!(report.falhas.is_empty());
    ok(&value)
}

/// O que a regra antiga de assistidos apagaria, menos o que já está marcado.
/// Sem Jellyfin, `configurado` é falso e as listas vêm vazias.
async fn suggestions(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let config = web.config();
    let Some(jellyfin) = config.jellyfin() else {
        return ok(&json!({ "configurado": false, "filmes": [], "temporadas": [] }));
    };
    let client = JellyfinClient::new(
        &jellyfin.url,
        &jellyfin.api_key,
        crate::config::HTTP_TIMEOUT,
    )
    .map_err(|e| fail(bad(e)))?;
    let grace = crate::config::SUGGESTION_GRACE_MINUTES;
    let (movie_found, season_found) = tokio::try_join!(
        crate::watched::suggest(store, &client, grace),
        crate::series::watched::suggest(store, &client, grace)
    )
    .map_err(|e| fail(anyhow_bad(&e)))?;
    let (marks, movies, series) =
        tokio::try_join!(store.deletion_marks(), store.movies(), store.series_list())
            .map_err(|e| fail(bad(e)))?;
    let marked: HashSet<MarkTarget> = marks.into_iter().map(|m| m.target).collect();
    let when = |at: time::OffsetDateTime| at.format(&Rfc3339).unwrap_or_default();

    let movie_items: Vec<Value> = movie_found
        .iter()
        .filter(|s| !marked.contains(&MarkTarget::Movie(s.movie_id)))
        .filter_map(|s| {
            let entry = movies.iter().find(|m| m.id == s.movie_id)?;
            let mut item = target_json(MarkTarget::Movie(s.movie_id));
            item["chave"] = json!(key(MarkTarget::Movie(s.movie_id)));
            item["titulo"] = json!(crate::events::label(&entry.movie.title, entry.movie.year));
            item["detalhe"] = Value::Null;
            item["poster"] = json!(grid(entry.extras.poster.as_ref()));
            item["arquivos"] = json!(1);
            item["tamanho"] = json!(movie_size(entry));
            item["assistido_por"] = json!(s.user);
            item["assistido_em"] = json!(when(s.at));
            Some(item)
        })
        .collect();
    let season_items: Vec<Value> = season_found
        .iter()
        .filter(|s| {
            let whole = MarkTarget::Series {
                series_id: s.series_id,
                season: None,
            };
            let one = MarkTarget::Series {
                series_id: s.series_id,
                season: Some(s.season),
            };
            !marked.contains(&whole) && !marked.contains(&one)
        })
        .filter_map(|s| {
            let entry = series.iter().find(|e| e.id == s.series_id)?;
            let target = MarkTarget::Series {
                series_id: s.series_id,
                season: Some(s.season),
            };
            let mut item = target_json(target);
            item["chave"] = json!(key(target));
            item["titulo"] = json!(crate::events::label(&entry.series.title, entry.series.year));
            item["detalhe"] = json!(season_name(s.season));
            item["poster"] = json!(grid(entry.series.poster.as_ref()));
            item["arquivos"] = json!(s.files);
            item["tamanho"] = json!(s.size);
            item["assistido_por"] = json!(s.user);
            item["assistido_em"] = json!(when(s.at));
            Some(item)
        })
        .collect();
    ok(&json!({
        "configurado": true,
        "filmes": movie_items,
        "temporadas": season_items,
    }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use acervo_store::{Episode, EpisodeFile, Movie, MovieExtras, MovieFile, Series, Skip};
    use reqwest::Method as Http;

    use super::*;
    use crate::series::watched::tests::sample;
    use crate::settings::Settings;

    #[test]
    fn espaco_da_temporada_e_da_serie_conta_cada_arquivo_uma_vez() {
        // T1: um multi-episódio (E1-E2, arquivo 10) e um avulso (E3, 11),
        // mais um episódio sem arquivo. T2: um arquivo. O multi-episódio
        // conta uma vez.
        let entry = sample(
            1,
            &[
                (1, 1, 1, Some(10)),
                (2, 1, 2, Some(10)),
                (3, 1, 3, Some(11)),
                (4, 1, 4, None),
                (5, 2, 1, Some(20)),
            ],
            &[(10, 1000, None), (11, 300, None), (20, 7, None)],
        );
        assert_eq!(
            series_scope(&entry, Some(1)),
            Scope {
                episode_ids: vec![1, 2, 3],
                files: 2,
                size: 1300,
            }
        );
        assert_eq!(
            series_scope(&entry, None),
            Scope {
                episode_ids: vec![1, 2, 3, 5],
                files: 3,
                size: 1307,
            }
        );
        // Temporada sem arquivo, ou que não existe: nada a liberar.
        assert_eq!(series_scope(&entry, Some(9)), Scope::default());
    }

    #[test]
    fn alvo_vem_de_filme_ou_de_serie_e_nunca_dos_dois() {
        let parse = |value: Value| {
            serde_json::from_value::<TargetBody>(value)
                .map_err(|e| e.to_string())
                .and_then(|b| b.target())
        };
        assert_eq!(parse(json!({ "filme": 3 })), Ok(MarkTarget::Movie(3)));
        assert_eq!(
            parse(json!({ "tipo": "temporada", "serie": 2, "temporada": 1 })),
            Ok(MarkTarget::Series {
                series_id: 2,
                season: Some(1)
            })
        );
        assert_eq!(
            parse(json!({ "serie": 2 })),
            Ok(MarkTarget::Series {
                series_id: 2,
                season: None
            })
        );
        assert!(parse(json!({ "filme": 3, "serie": 2 })).is_err());
        assert!(parse(json!({ "filme": 3, "temporada": 1 })).is_err());
        assert!(parse(json!({})).is_err());
        assert!(parse(json!({ "outro": 1 })).is_err());
        assert_eq!(key(MarkTarget::Movie(3)), "filme-3");
        assert_eq!(
            key(MarkTarget::Series {
                series_id: 2,
                season: Some(0)
            }),
            "serie-2-t0"
        );
        assert_eq!(season_name(0), "Especiais");
    }

    const KEY: &str = "chave-de-teste-0123456789";

    fn movie(title: &str, size: u64) -> Movie {
        Movie {
            tmdb_id: 10,
            imdb_id: None,
            title: title.into(),
            original_title: None,
            original_language: None,
            year: Some(2024),
            status: None,
            monitored: false,
            path: format!("/media/movies/{title} (2024)"),
            added: None,
            file: Some(MovieFile {
                relative_path: format!("{title}.mkv"),
                size,
                quality: acervo_parser::parse_quality("x.1080p.WEB-DL"),
                languages: Vec::new(),
                release_group: None,
                edition: None,
                scene_name: None,
                date_added: Some("2026-08-01T00:00:00Z".into()),
            }),
            runtime: 0,
            clean_title: None,
            alternate_titles: Vec::new(),
            in_cinemas: None,
            digital_release: None,
            physical_release: None,
            overview: None,
        }
    }

    fn show() -> Series {
        let mut series = sample(1, &[], &[]).series;
        series.title = "Show".into();
        series.path = "/media/series/Show".into();
        series
    }

    fn episode(season: u16, number: u16) -> Episode {
        Episode {
            season,
            number,
            tmdb_id: None,
            title: None,
            air_date: Some("2026-01-01".into()),
            overview: None,
            runtime: 0,
        }
    }

    fn episode_file(relative: &str, size: u64) -> EpisodeFile {
        EpisodeFile {
            relative_path: relative.into(),
            size,
            quality: acervo_parser::parse_quality("x.1080p.WEB-DL"),
            languages: Vec::new(),
            release_group: None,
            scene_name: None,
            date_added: Some("2026-08-01T00:00:00Z".into()),
        }
    }

    fn write(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"video").unwrap();
    }

    async fn call(
        http: &reqwest::Client,
        method: Http,
        url: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = http
            .request(method, url)
            .header("X-Api-Key", KEY)
            .header("X-Acervo", "1");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn desmarcado_durante_o_lote_nao_e_apagado() {
        let Some(db) = acervo_store::testing::TestDb::new("para_apagar_lote").await else {
            return;
        };
        let store = &db.store;
        let root =
            std::env::temp_dir().join(format!("acervo-para-apagar-lote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let gone = root.join("movies/Visto (2024)/Visto.mkv");
        let kept = root.join("movies/Fica (2024)/Fica.mkv");
        write(&gone);
        write(&kept);
        let settings = Settings::load(store.clone()).await.unwrap();
        settings
            .save_section(
                crate::config::BIBLIOTECA,
                json!({
                    "root_folders": [root.join("movies")],
                    "series_root": root.join("series"),
                }),
            )
            .await
            .unwrap();
        let config = settings.get();
        let mut title = movie("Visto", 4000);
        title.path = root.join("movies/Visto (2024)").display().to_string();
        let visto = store
            .add_movie(&title, &MovieExtras::default())
            .await
            .unwrap();
        let fica = store
            .add_movie(
                &Movie {
                    tmdb_id: 11,
                    path: root.join("movies/Fica (2024)").display().to_string(),
                    ..movie("Fica", 4000)
                },
                &MovieExtras::default(),
            )
            .await
            .unwrap();
        let targets = [MarkTarget::Movie(visto), MarkTarget::Movie(fica)];
        store
            .mark_for_deletion(&targets, "2026-10-05T00:00:00Z")
            .await
            .unwrap();
        // O lote foi pedido com os dois, mas `Fica` foi desmarcado antes de a
        // vez dele chegar.
        store.unmark(&[MarkTarget::Movie(fica)]).await.unwrap();

        let report = purge(&config, store, &targets).await.unwrap();

        assert_eq!(report.apagados.len(), 1, "{report:?}");
        assert_eq!(report.falhas.len(), 1, "{report:?}");
        assert!(!gone.exists());
        assert!(kept.exists(), "o desmarcado não pode sair");
        assert!(store.deletion_marks().await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta, pela API.
    async fn marca_lista_e_apaga_pela_api() {
        let Some(db) = acervo_store::testing::TestDb::new("para_apagar").await else {
            return;
        };
        let store = &db.store;
        let root = std::env::temp_dir().join(format!("acervo-para-apagar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let movie_file = root.join("movies/Visto (2024)/Visto.mkv");
        let s1e1 = root.join("series/Show/Season 01/e1.mkv");
        let s1e2 = root.join("series/Show/Season 01/e2.mkv");
        let s2e1 = root.join("series/Show/Season 02/e1.mkv");
        for path in [&movie_file, &s1e1, &s1e2, &s2e1] {
            write(path);
        }

        let settings = Arc::new(Settings::load(store.clone()).await.unwrap());
        settings
            .save_section(crate::config::SERVIDOR, json!({ "api_key": KEY }))
            .await
            .unwrap();
        settings
            .save_section(
                crate::config::BIBLIOTECA,
                json!({
                    "root_folders": [root.join("movies")],
                    "series_root": root.join("series"),
                }),
            )
            .await
            .unwrap();

        let mut title = movie("Visto", 4000);
        title.path = root.join("movies/Visto (2024)").display().to_string();
        let movie_id = store
            .add_movie(&title, &MovieExtras::default())
            .await
            .unwrap();
        let mut title = show();
        title.path = root.join("series/Show").display().to_string();
        let series_id = store
            .add_series(&title, &[episode(1, 1), episode(1, 2), episode(2, 1)])
            .await
            .unwrap();
        let entry = store.series(series_id).await.unwrap().unwrap();
        let id_of = |season: u16, number: u16| {
            entry
                .episodes
                .iter()
                .find(|e| e.episode.season == season && e.episode.number == number)
                .unwrap()
                .id
        };
        for (relative, size, ids) in [
            ("Season 01/e1.mkv", 100, [id_of(1, 1)]),
            ("Season 01/e2.mkv", 200, [id_of(1, 2)]),
            ("Season 02/e1.mkv", 50, [id_of(2, 1)]),
        ] {
            store
                .add_episode_file(series_id, &episode_file(relative, size), &ids)
                .await
                .unwrap();
        }

        let web = Arc::new(Web {
            settings,
            database: crate::serve::Database::connected(store.clone()),
            catalog: acervo_api::Catalog::default(),
            accounts: None,
            searches: tokio::sync::Mutex::default(),
            series_searches: tokio::sync::Mutex::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}/ui/api/biblioteca/para-apagar",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, router(web)).await });
        let http = reqwest::Client::new();

        // Sem o cabeçalho da interface, marcar é recusado; sem chave, tudo.
        let response = http
            .post(format!("{base}/marcar"))
            .header("X-Api-Key", KEY)
            .json(&json!({ "itens": [{ "filme": movie_id }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 403);
        assert_eq!(http.get(&base).send().await.unwrap().status().as_u16(), 401);

        // Vazio.
        let (status, body) = call(&http, Http::GET, &base, None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body, json!({ "itens": [], "total": 0 }));

        // Alvo que não existe é recusado, e nada entra.
        let (status, body) = call(
            &http,
            Http::POST,
            &format!("{base}/marcar"),
            Some(
                json!({ "itens": [{ "filme": movie_id }, { "serie": series_id, "temporada": 7 }] }),
            ),
        )
        .await;
        assert_eq!(status, 422);
        assert!(
            body["erro"].as_str().unwrap().contains("temporada 7"),
            "{body}"
        );
        assert!(store.deletion_marks().await.unwrap().is_empty());

        let (status, body) = call(
            &http,
            Http::POST,
            &format!("{base}/marcar"),
            Some(json!({ "itens": [
                { "filme": movie_id },
                { "serie": series_id, "temporada": 1 },
                { "serie": series_id, "temporada": 2 },
            ] })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["marcados"], 3);
        let (_, body) = call(
            &http,
            Http::POST,
            &format!("{base}/desmarcar"),
            Some(json!({ "itens": [{ "serie": series_id, "temporada": 2 }] })),
        )
        .await;
        assert_eq!(body["desmarcados"], 1);

        let (_, body) = call(&http, Http::GET, &base, None).await;
        assert_eq!(body["total"], 4300, "{body}");
        let items = body["itens"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["tipo"], "filme");
        assert_eq!(items[0]["titulo"], "Visto (2024)");
        assert_eq!(items[0]["tamanho"], 4000);
        assert_eq!(items[1]["tipo"], "temporada");
        assert_eq!(items[1]["detalhe"], "Temporada 1");
        assert_eq!(items[1]["arquivos"], 2);
        assert_eq!(items[1]["tamanho"], 300);

        // Sem Jellyfin, sem sugestões — e sem erro.
        let (status, body) = call(&http, Http::GET, &format!("{base}/sugestoes"), None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["configurado"], false);

        // Apaga o marcado; o que não está marcado falha sem segurar o resto.
        let (status, body) = call(
            &http,
            Http::POST,
            &format!("{base}/apagar"),
            Some(json!({ "itens": [
                { "filme": movie_id },
                { "serie": series_id, "temporada": 2 },
                { "serie": series_id, "temporada": 1 },
            ] })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["ok"], false);
        assert_eq!(body["liberado"], 4300);
        let done: Vec<&str> = body["apagados"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["chave"].as_str().unwrap())
            .collect();
        assert_eq!(
            done,
            [format!("filme-{movie_id}"), format!("serie-{series_id}-t1")]
        );
        assert_eq!(body["falhas"][0]["chave"], format!("serie-{series_id}-t2"));
        assert_eq!(body["falhas"][0]["erro"], "não está marcado para apagar");

        // O filme saiu do catálogo e do disco; a temporada 1, do disco, com
        // os episódios fora da busca; a 2 ficou. Nenhuma marca sobrou.
        assert!(!movie_file.exists());
        assert!(store.movies().await.unwrap().is_empty());
        assert!(!s1e1.exists() && !s1e2.exists());
        assert!(s2e1.exists());
        let entry = store.series(series_id).await.unwrap().unwrap();
        assert_eq!(entry.files.len(), 1);
        for episode in &entry.episodes {
            let expected = (episode.episode.season == 1).then_some(Skip::Deleted);
            assert_eq!(episode.skip, expected, "{:?}", episode.episode);
        }
        assert!(store.deletion_marks().await.unwrap().is_empty());
        let history = store
            .history(None, Some("movie_deleted"), 10, 0)
            .await
            .unwrap();
        let message = history.events[0].data["mensagem"].as_str().unwrap();
        assert!(message.starts_with(REASON), "{message}");

        std::fs::remove_dir_all(&root).unwrap();
        db.drop().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Montar o Jellyfin e o catálogo.
    async fn sugestoes_vem_do_jellyfin_e_poupam_o_marcado() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let Some(db) = acervo_store::testing::TestDb::new("para_apagar_sugestoes").await else {
            return;
        };
        let store = &db.store;
        let jellyfin = MockServer::start().await;
        let settings = Arc::new(Settings::load(store.clone()).await.unwrap());
        settings
            .save_section(crate::config::SERVIDOR, json!({ "api_key": KEY }))
            .await
            .unwrap();
        settings
            .save_section(
                crate::config::JELLYFIN,
                json!({ "url": jellyfin.uri(), "api_key": "x" }),
            )
            .await
            .unwrap();
        let title = movie("Visto", 4000);
        let movie_id = store
            .add_movie(&title, &MovieExtras::default())
            .await
            .unwrap();
        let title = show();
        let series_id = store
            .add_series(&title, &[episode(1, 1), episode(1, 2), episode(2, 1)])
            .await
            .unwrap();
        let entry = store.series(series_id).await.unwrap().unwrap();
        for (relative, size, season, number) in [
            ("Season 01/e1.mkv", 100, 1, 1),
            ("Season 01/e2.mkv", 200, 1, 2),
            ("Season 02/e1.mkv", 50, 2, 1),
        ] {
            let id = entry
                .episodes
                .iter()
                .find(|e| e.episode.season == season && e.episode.number == number)
                .unwrap()
                .id;
            store
                .add_episode_file(series_id, &episode_file(relative, size), &[id])
                .await
                .unwrap();
        }

        let seen = json!({ "Played": true, "IsFavorite": false, "LastPlayedDate": "2026-09-01T20:00:00.0000000Z" });
        let unseen = json!({ "Played": false, "IsFavorite": false });
        let episode_item = |season: u16, number: u16, data: &Value| {
            json!({
                "Name": "Ep", "Id": format!("e{season}{number}"), "Type": "Episode",
                "IndexNumber": number, "ParentIndexNumber": season, "SeriesId": "sa",
                "UserData": data
            })
        };
        Mock::given(method("GET"))
            .and(path("/Users"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{ "Name": "vitor", "Id": "u1" }])),
            )
            .mount(&jellyfin)
            .await;
        for (kind, items) in [
            (
                "Movie",
                json!([{
                    "Name": "Visto", "Id": "m10", "Type": "Movie",
                    "ProviderIds": { "Tmdb": "10" }, "UserData": seen
                }]),
            ),
            (
                "Series",
                json!([{
                    "Name": "Show", "Id": "sa", "Type": "Series",
                    "ProviderIds": { "Tmdb": entry.series.tmdb_id.to_string() },
                    "UserData": unseen
                }]),
            ),
            (
                "Episode",
                json!([
                    episode_item(1, 1, &seen),
                    episode_item(1, 2, &seen),
                    episode_item(2, 1, &unseen)
                ]),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path("/Items"))
                .and(query_param("includeItemTypes", kind))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Items": items })))
                .mount(&jellyfin)
                .await;
        }

        let web = Arc::new(Web {
            settings,
            database: crate::serve::Database::connected(store.clone()),
            catalog: acervo_api::Catalog::default(),
            accounts: None,
            searches: tokio::sync::Mutex::default(),
            series_searches: tokio::sync::Mutex::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/ui/api/biblioteca/para-apagar/sugestoes",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, router(web)).await });
        let http = reqwest::Client::new();

        let (status, body) = call(&http, Http::GET, &url, None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["configurado"], true);
        let movies = body["filmes"].as_array().unwrap();
        assert_eq!(movies.len(), 1, "{body}");
        assert_eq!(movies[0]["filme"], movie_id);
        assert_eq!(movies[0]["assistido_por"], "vitor");
        assert_eq!(movies[0]["assistido_em"], "2026-09-01T20:00:00Z");
        assert_eq!(movies[0]["tamanho"], 4000);
        // Só a temporada 1, inteira assistida; a 2 ninguém viu.
        let seasons = body["temporadas"].as_array().unwrap();
        assert_eq!(seasons.len(), 1, "{body}");
        assert_eq!(seasons[0]["tipo"], "temporada");
        assert_eq!(seasons[0]["serie"], series_id);
        assert_eq!(seasons[0]["temporada"], 1);
        assert_eq!(seasons[0]["arquivos"], 2);
        assert_eq!(seasons[0]["tamanho"], 300);

        // O que já está marcado sai das sugestões; a série inteira cobre a
        // temporada.
        store
            .mark_for_deletion(
                &[
                    MarkTarget::Movie(movie_id),
                    MarkTarget::Series {
                        series_id,
                        season: None,
                    },
                ],
                "2026-10-01T00:00:00Z",
            )
            .await
            .unwrap();
        let (_, body) = call(&http, Http::GET, &url, None).await;
        assert_eq!(body["filmes"], json!([]));
        assert_eq!(body["temporadas"], json!([]));
        // Sugerir não apaga nada.
        assert_eq!(store.movies().await.unwrap().len(), 1);
        assert_eq!(
            store.series(series_id).await.unwrap().unwrap().files.len(),
            3
        );
        db.drop().await;
    }
}

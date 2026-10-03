//! Assistidos de séries, dentro da tarefa `assistidos`: episódio assistido
//! por algum usuário do Jellyfin há mais que a carência, que não é favorito
//! e cuja série não é favorita, perde o arquivo e fica `watched`. Arquivo
//! multi-episódio só sai quando todos os episódios dele foram assistidos. O
//! torrent fica semeando até a limpeza, como nos filmes.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use acervo_clients::jellyfin::{JellyfinClient, JellyfinEpisode};
use acervo_store::{CatalogSeries, Skip, Store};
use anyhow::{Context, Result};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::events::{self, Event, Kind};

/// O que um usuário fez com um episódio (ou com um arquivo multi-episódio:
/// `number..=last`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeViewing {
    pub user: String,
    pub season: u16,
    pub number: u16,
    pub last: u16,
    pub played: bool,
    pub last_played: Option<OffsetDateTime>,
    pub favorite: bool,
    pub series_favorite: bool,
}

/// O destino de um arquivo de série com algum episódio assistido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Sai: `user` foi o último a assistir, em `at`.
    Delete { user: String, at: OffsetDateTime },
    /// Fica: o episódio ou a série é favorito de alguém.
    Favorite { user: String },
    /// Fica até `until`.
    InGrace { until: OffsetDateTime },
    /// Fica: `user` assistiu, mas não se sabe quando.
    NoDate { user: String },
    /// Fica: multi-episódio com episódio ainda não assistido.
    Partial,
}

/// A regra, sem rede nem banco, para uma série: `files` são os arquivos,
/// cada um com os `(temporada, número)` que cobre; `viewings`, o que os
/// usuários fizeram com os episódios dela. Só entra arquivo com algum
/// episódio assistido.
#[must_use]
pub fn select(
    files: &[(i64, Vec<(u16, u16)>)],
    viewings: &[EpisodeViewing],
    now: OffsetDateTime,
    grace: Duration,
) -> BTreeMap<i64, Verdict> {
    let series_fan = viewings.iter().find(|v| v.series_favorite);
    let of = |season: u16, number: u16| -> Vec<&EpisodeViewing> {
        viewings
            .iter()
            .filter(|v| v.season == season && (v.number..=v.last.max(v.number)).contains(&number))
            .collect()
    };
    files
        .iter()
        .filter_map(|(file_id, episodes)| {
            let views: Vec<Vec<&EpisodeViewing>> =
                episodes.iter().map(|(s, n)| of(*s, *n)).collect();
            if !views.iter().flatten().any(|v| v.played) {
                return None;
            }
            // Favorito de qualquer usuário segura, mesmo de quem não assistiu.
            if let Some(fan) =
                series_fan.or_else(|| views.iter().flatten().copied().find(|v| v.favorite))
            {
                return Some((
                    *file_id,
                    Verdict::Favorite {
                        user: fan.user.clone(),
                    },
                ));
            }
            if views.iter().any(|v| !v.iter().any(|v| v.played)) {
                return Some((*file_id, Verdict::Partial));
            }
            let played: Vec<&&EpisodeViewing> =
                views.iter().flatten().filter(|v| v.played).collect();
            if let Some(undated) = played.iter().find(|v| v.last_played.is_none()) {
                return Some((
                    *file_id,
                    Verdict::NoDate {
                        user: undated.user.clone(),
                    },
                ));
            }
            let (user, at) = played
                .iter()
                .filter_map(|v| v.last_played.map(|at| (&v.user, at)))
                .max_by_key(|(_, at)| *at)?;
            let verdict = if now - at > grace {
                Verdict::Delete {
                    user: user.clone(),
                    at,
                }
            } else {
                Verdict::InGrace {
                    until: at.checked_add(grace).unwrap_or(at),
                }
            };
            Some((*file_id, verdict))
        })
        .collect()
}

/// Um arquivo de série apagado.
#[derive(Debug, Clone, Serialize)]
pub struct Deleted {
    /// "Série S01E02".
    pub episodio: String,
    pub assistido_por: String,
    pub assistido_em: String,
    pub tamanho: u64,
}

/// Um arquivo assistido que ficou, e por quê.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub episodio: String,
    pub motivo: String,
}

/// O que a parte de séries fez.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SeriesWatchedReport {
    pub apagados: Vec<Deleted>,
    pub pulados: Vec<Skipped>,
    pub recusados: usize,
    pub liberado: u64,
    pub aviso: Option<String>,
}

fn human(at: OffsetDateTime) -> String {
    let format = time::macros::format_description!("[day]/[month]/[year] [hour]:[minute] UTC");
    at.format(&format).unwrap_or_default()
}

/// A série do catálogo de um episódio do Jellyfin: pelo TMDB, senão pelo
/// TVDB.
fn owner<'a>(episode: &JellyfinEpisode, list: &'a [CatalogSeries]) -> Option<&'a CatalogSeries> {
    episode
        .series_tmdb_id
        .and_then(|id| list.iter().find(|s| s.series.tmdb_id == id))
        .or_else(|| {
            episode
                .series_tvdb_id
                .and_then(|id| list.iter().find(|s| s.series.tvdb_id == Some(id)))
        })
}

/// Marca os episódios `watched` e só então apaga o arquivo do disco e do
/// catálogo: falha no meio nunca devolve episódio à busca.
async fn delete_file(
    config: &Config,
    store: &Store,
    entry: &CatalogSeries,
    file: &acervo_store::CatalogEpisodeFile,
    episode_ids: &[i64],
) -> Result<()> {
    let path = PathBuf::from(&entry.series.path).join(&file.file.relative_path);
    let host = config.path_map().to_host(&path)?;
    store
        .set_skip(episode_ids, Some(Skip::Watched), &now_rfc3339())
        .await?;
    tokio::task::spawn_blocking(move || match std::fs::remove_file(&host) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    })
    .await??;
    store.delete_episode_file(file.id).await?;
    Ok(())
}

/// Lê o que cada usuário fez com os episódios do catálogo, por série.
async fn viewings(
    jellyfin: &JellyfinClient,
    list: &[CatalogSeries],
) -> Result<HashMap<i64, Vec<EpisodeViewing>>> {
    let users = jellyfin
        .users()
        .await
        .context("listando os usuários do Jellyfin")?;
    let mut by_series: HashMap<i64, Vec<EpisodeViewing>> = HashMap::new();
    for user in &users {
        let episodes = jellyfin
            .episodes(&user.id)
            .await
            .with_context(|| format!("lendo os episódios de `{}` no Jellyfin", user.name))?;
        for episode in episodes {
            let (Some(entry), Some(season), Some(number)) =
                (owner(&episode, list), episode.season, episode.number)
            else {
                continue;
            };
            by_series.entry(entry.id).or_default().push(EpisodeViewing {
                user: user.name.clone(),
                season,
                number,
                last: episode.index_end.unwrap_or(number),
                played: episode.played,
                last_played: episode
                    .last_played
                    .as_deref()
                    .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok()),
                favorite: episode.favorite,
                series_favorite: episode.series_favorite,
            });
        }
    }
    Ok(by_series)
}

/// Uma execução: lê os episódios de cada usuário, aplica a regra e apaga.
///
/// # Errors
///
/// Jellyfin inalcançável ou catálogo ilegível, antes de apagar qualquer
/// coisa. Falha num arquivo fica no relatório; os outros seguem.
#[allow(clippy::too_many_lines)] // Um braço por veredito, como nos filmes.
pub async fn run(
    config: &Config,
    store: &Store,
    jellyfin: &JellyfinClient,
    grace_minutes: u64,
) -> Result<SeriesWatchedReport> {
    let list = store.series_list().await?;
    let mut report = SeriesWatchedReport::default();
    if list.is_empty() {
        return Ok(report);
    }
    let by_series = viewings(jellyfin, &list).await?;
    let grace =
        Duration::seconds(i64::try_from(grace_minutes.saturating_mul(60)).unwrap_or(i64::MAX));
    let now = OffsetDateTime::now_utc();
    for entry in &list {
        let Some(viewings) = by_series.get(&entry.id) else {
            continue;
        };
        let files: Vec<(i64, Vec<(u16, u16)>)> = entry
            .files
            .iter()
            .map(|f| {
                let episodes = entry
                    .episodes
                    .iter()
                    .filter(|e| e.file_id == Some(f.id))
                    .map(|e| (e.episode.season, e.episode.number))
                    .collect();
                (f.id, episodes)
            })
            .collect();
        for (file_id, verdict) in select(&files, viewings, now, grace) {
            let Some(file) = entry.files.iter().find(|f| f.id == file_id) else {
                continue;
            };
            let episode_ids: Vec<i64> = entry
                .episodes
                .iter()
                .filter(|e| e.file_id == Some(file_id))
                .map(|e| e.id)
                .collect();
            let label = super::label(entry, &episode_ids);
            let skip = |motivo: String| Skipped {
                episodio: label.clone(),
                motivo,
            };
            match verdict {
                Verdict::Delete { user, at } => {
                    let reason = format!("assistido por {user} em {}", human(at));
                    match delete_file(config, store, entry, file, &episode_ids).await {
                        Ok(()) => {
                            events::record(
                                store,
                                Event {
                                    source_title: file.file.scene_name.clone(),
                                    quality: Some(file.file.quality.quality),
                                    message: Some(format!(
                                        "{reason}; arquivo apagado: {}; o download segue \
                                         semeando até o ciclo de limpeza",
                                        file.file.relative_path
                                    )),
                                    poster: entry.series.poster.clone(),
                                    ..Event::series(
                                        Kind::FileDeleted,
                                        entry.id,
                                        &label,
                                        &episode_ids,
                                    )
                                },
                            )
                            .await;
                            report.liberado += file.file.size;
                            report.apagados.push(Deleted {
                                episodio: label.clone(),
                                assistido_por: user,
                                assistido_em: at.format(&Rfc3339).unwrap_or_default(),
                                tamanho: file.file.size,
                            });
                        }
                        Err(error) => {
                            tracing::warn!(episodio = label, "assistido não removido: {error:#}");
                            report.recusados += 1;
                            report
                                .pulados
                                .push(skip(format!("remoção recusada: {error:#}")));
                        }
                    }
                }
                Verdict::Favorite { user } => {
                    report.pulados.push(skip(format!("favorito de {user}")));
                }
                Verdict::InGrace { until } => report
                    .pulados
                    .push(skip(format!("dentro da carência, até {}", human(until)))),
                Verdict::NoDate { user } => report
                    .pulados
                    .push(skip(format!("assistido por {user} sem data conhecida"))),
                Verdict::Partial => report.pulados.push(skip(
                    "multi-episódio com episódio ainda não assistido".into(),
                )),
            }
        }
    }
    if !report.apagados.is_empty()
        && let Err(error) = jellyfin.refresh_library().await
    {
        tracing::warn!("varredura do Jellyfin: {error}");
        report.aviso = Some(format!("varredura da biblioteca não pedida: {error}"));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> OffsetDateTime {
        OffsetDateTime::parse(text, &Rfc3339).unwrap()
    }

    const NOW: &str = "2026-10-02T12:00:00Z";

    fn view(user: &str, number: u16, when: Option<&str>) -> EpisodeViewing {
        EpisodeViewing {
            user: user.into(),
            season: 1,
            number,
            last: number,
            played: true,
            last_played: when.map(at),
            favorite: false,
            series_favorite: false,
        }
    }

    fn decide(
        files: &[(i64, Vec<(u16, u16)>)],
        viewings: &[EpisodeViewing],
    ) -> BTreeMap<i64, Verdict> {
        select(files, viewings, at(NOW), Duration::minutes(60))
    }

    #[test]
    fn assistido_passada_a_carencia_sai_e_dentro_dela_fica() {
        let files = [(1, vec![(1, 1)]), (2, vec![(1, 2)]), (3, vec![(1, 3)])];
        let verdicts = decide(
            &files,
            &[
                view("vitor", 1, Some("2026-10-02T10:00:00Z")),
                view("ana", 2, Some("2026-10-02T11:30:00Z")),
                EpisodeViewing {
                    played: false,
                    ..view("vitor", 3, None)
                },
            ],
        );
        assert_eq!(
            verdicts[&1],
            Verdict::Delete {
                user: "vitor".into(),
                at: at("2026-10-02T10:00:00Z")
            }
        );
        assert_eq!(
            verdicts[&2],
            Verdict::InGrace {
                until: at("2026-10-02T12:30:00Z")
            }
        );
        // Ninguém assistiu: nem entra.
        assert!(!verdicts.contains_key(&3));
    }

    #[test]
    fn favorito_do_episodio_ou_da_serie_segura() {
        let files = [(1, vec![(1, 1)])];
        let mut fan = view("ana", 1, None);
        fan.played = false;
        fan.favorite = true;
        let verdicts = decide(
            &files,
            &[view("vitor", 1, Some("2026-09-01T00:00:00Z")), fan],
        );
        assert_eq!(verdicts[&1], Verdict::Favorite { user: "ana".into() });
        // Série favorita em qualquer episódio vale para todos.
        let mut series_fan = view("ana", 9, None);
        series_fan.played = false;
        series_fan.series_favorite = true;
        let verdicts = decide(
            &files,
            &[view("vitor", 1, Some("2026-09-01T00:00:00Z")), series_fan],
        );
        assert_eq!(verdicts[&1], Verdict::Favorite { user: "ana".into() });
    }

    #[test]
    fn multi_episodio_so_sai_com_todos_assistidos() {
        let files = [(1, vec![(1, 1), (1, 2)])];
        let old = Some("2026-09-01T00:00:00Z");
        // Só o primeiro assistido: fica.
        assert_eq!(
            decide(&files, &[view("vitor", 1, old)])[&1],
            Verdict::Partial
        );
        // Os dois, por usuários diferentes: sai, com a data mais recente.
        let verdicts = decide(
            &files,
            &[
                view("vitor", 1, old),
                view("ana", 2, Some("2026-09-02T00:00:00Z")),
            ],
        );
        assert_eq!(
            verdicts[&1],
            Verdict::Delete {
                user: "ana".into(),
                at: at("2026-09-02T00:00:00Z")
            }
        );
        // O Jellyfin vê o arquivo como um item só, de 1 a 2.
        let mut whole = view("vitor", 1, old);
        whole.last = 2;
        assert!(matches!(
            decide(&files, &[whole])[&1],
            Verdict::Delete { .. }
        ));
        // Assistido sem data: fica.
        assert_eq!(
            decide(&files, &[view("vitor", 1, old), view("ana", 2, None)])[&1],
            Verdict::NoDate { user: "ana".into() }
        );
    }
}

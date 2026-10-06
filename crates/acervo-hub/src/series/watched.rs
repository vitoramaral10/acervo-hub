//! Sugestões de assistidos de séries, como as dos filmes: o arquivo de
//! episódio assistido por algum usuário do Jellyfin há mais que a carência,
//! que não é favorito e cuja série não é favorita, é o que a regra apagaria.
//! Arquivo multi-episódio só entra quando todos os episódios dele foram
//! assistidos, e só o arquivo que chegou antes de assistirem. A sugestão é
//! por temporada, a unidade que se marca: entra a temporada em que todo
//! arquivo passa na regra. Nada sai sozinho.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use acervo_clients::jellyfin::{JellyfinClient, JellyfinEpisode};
use acervo_store::{CatalogSeries, Store};
use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

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

/// Candidato a sugestão: assistido por `user` em `at`, passada a carência.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub user: String,
    pub at: OffsetDateTime,
}

/// A regra, sem rede nem banco, para uma série: `files` são os arquivos,
/// cada um com os `(temporada, número)` que cobre; `viewings`, o que os
/// usuários fizeram com os episódios dela. Só entra arquivo com todos
/// os episódios assistidos, sem favorito e passada a carência.
#[must_use]
pub fn select(
    files: &[(i64, Vec<(u16, u16)>)],
    viewings: &[EpisodeViewing],
    now: OffsetDateTime,
    grace: Duration,
) -> BTreeMap<i64, Candidate> {
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
            if series_fan.is_some() || views.iter().flatten().any(|v| v.favorite) {
                return None;
            }
            if views.iter().any(|v| !v.iter().any(|v| v.played)) {
                return None;
            }
            let played: Vec<&&EpisodeViewing> =
                views.iter().flatten().filter(|v| v.played).collect();
            if played.iter().any(|v| v.last_played.is_none()) {
                return None;
            }
            let (user, at) = played
                .iter()
                .filter_map(|v| v.last_played.map(|at| (&v.user, at)))
                .max_by_key(|(_, at)| *at)?;
            (now - at > grace).then(|| {
                (
                    *file_id,
                    Candidate {
                        user: user.clone(),
                        at,
                    },
                )
            })
        })
        .collect()
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

/// Uma temporada em que todo arquivo passa na regra: marcá-la apaga o que
/// a regra apagaria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeasonSuggestion {
    pub series_id: i64,
    pub season: u16,
    /// Quantos arquivos a temporada tem no disco.
    pub files: usize,
    /// Bytes desses arquivos.
    pub size: u64,
    /// Quem assistiu por último, e quando.
    pub user: String,
    pub at: OffsetDateTime,
}

/// As temporadas de uma série que viram sugestão, a partir dos candidatos de
/// [`select`], sem rede nem banco. Temporada com algum arquivo fora da regra
/// (favorito, na carência, sem data, multi-episódio pela metade, nunca
/// assistido, ou que chegou depois de assistirem) fica de fora inteira:
/// marcar a temporada apagaria esse arquivo também. Arquivo que cobre duas
/// temporadas conta nas duas.
#[must_use]
pub fn season_suggestions(
    entry: &CatalogSeries,
    candidates: &BTreeMap<i64, Candidate>,
) -> Vec<SeasonSuggestion> {
    let mut seasons: BTreeMap<u16, BTreeSet<i64>> = BTreeMap::new();
    for episode in &entry.episodes {
        if let Some(file_id) = episode.file_id {
            seasons
                .entry(episode.episode.season)
                .or_default()
                .insert(file_id);
        }
    }
    seasons
        .into_iter()
        .filter_map(|(season, file_ids)| {
            let mut last: Option<(&String, OffsetDateTime)> = None;
            let mut size = 0;
            for file_id in &file_ids {
                let file = entry.files.iter().find(|f| f.id == *file_id)?;
                let Candidate { user, at } = candidates.get(file_id)?;
                if !crate::watched::watched_after_added(*at, file.file.date_added.as_deref()) {
                    return None;
                }
                if last.is_none_or(|(_, latest)| *at > latest) {
                    last = Some((user, *at));
                }
                size += file.file.size;
            }
            let (user, at) = last?;
            Some(SeasonSuggestion {
                series_id: entry.id,
                season,
                files: file_ids.len(),
                size,
                user: user.clone(),
                at,
            })
        })
        .collect()
}

/// Os candidatos de cada arquivo da série.
fn candidates(
    entry: &CatalogSeries,
    viewings: &[EpisodeViewing],
    now: OffsetDateTime,
    grace: Duration,
) -> BTreeMap<i64, Candidate> {
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
    select(&files, viewings, now, grace)
}

/// Lê os episódios de cada usuário e devolve as temporadas que a regra
/// apagaria inteiras. Só lê: apagar é com o usuário, pela tela "Para
/// apagar".
///
/// # Errors
///
/// Jellyfin inalcançável ou catálogo ilegível.
pub async fn suggest(
    store: &Store,
    jellyfin: &JellyfinClient,
    grace_minutes: u64,
) -> Result<Vec<SeasonSuggestion>> {
    let list = store.series_list().await?;
    if list.is_empty() {
        return Ok(Vec::new());
    }
    let by_series = viewings(jellyfin, &list).await?;
    let grace = crate::watched::grace(grace_minutes);
    let now = OffsetDateTime::now_utc();
    Ok(list
        .iter()
        .filter_map(|entry| Some((entry, by_series.get(&entry.id)?)))
        .flat_map(|(entry, viewings)| {
            season_suggestions(entry, &candidates(entry, viewings, now, grace))
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
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
    ) -> BTreeMap<i64, Candidate> {
        select(files, viewings, at(NOW), Duration::minutes(60))
    }

    #[test]
    fn assistido_passada_a_carencia_sai_e_dentro_dela_fica() {
        let files = [(1, vec![(1, 1)]), (2, vec![(1, 2)]), (3, vec![(1, 3)])];
        let candidates = decide(
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
            candidates[&1],
            Candidate {
                user: "vitor".into(),
                at: at("2026-10-02T10:00:00Z")
            }
        );
        assert!(!candidates.contains_key(&2));
        // Ninguém assistiu: nem entra.
        assert!(!candidates.contains_key(&3));
    }

    #[test]
    fn favorito_do_episodio_ou_da_serie_segura() {
        let files = [(1, vec![(1, 1)])];
        let mut fan = view("ana", 1, None);
        fan.played = false;
        fan.favorite = true;
        let candidates = decide(
            &files,
            &[view("vitor", 1, Some("2026-09-01T00:00:00Z")), fan],
        );
        assert!(candidates.is_empty());
        // Série favorita em qualquer episódio vale para todos.
        let mut series_fan = view("ana", 9, None);
        series_fan.played = false;
        series_fan.series_favorite = true;
        let candidates = decide(
            &files,
            &[view("vitor", 1, Some("2026-09-01T00:00:00Z")), series_fan],
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn multi_episodio_so_sai_com_todos_assistidos() {
        let files = [(1, vec![(1, 1), (1, 2)])];
        let old = Some("2026-09-01T00:00:00Z");
        // Só o primeiro assistido: fica.
        assert!(decide(&files, &[view("vitor", 1, old)]).is_empty());
        // Os dois, por usuários diferentes: sai, com a data mais recente.
        let candidates = decide(
            &files,
            &[
                view("vitor", 1, old),
                view("ana", 2, Some("2026-09-02T00:00:00Z")),
            ],
        );
        assert_eq!(
            candidates[&1],
            Candidate {
                user: "ana".into(),
                at: at("2026-09-02T00:00:00Z")
            }
        );
        // O Jellyfin vê o arquivo como um item só, de 1 a 2.
        let mut whole = view("vitor", 1, old);
        whole.last = 2;
        assert!(matches!(decide(&files, &[whole])[&1], Candidate { .. }));
        // Assistido sem data: fica.
        assert!(decide(&files, &[view("vitor", 1, old), view("ana", 2, None)]).is_empty());
    }

    /// Uma série de teste: `episodes` são `(id, temporada, número, arquivo)`;
    /// `files`, `(id, bytes, adicionado em)`.
    pub(crate) fn sample(
        id: i64,
        episodes: &[(i64, u16, u16, Option<i64>)],
        files: &[(i64, u64, Option<&str>)],
    ) -> CatalogSeries {
        use acervo_store::{CatalogEpisode, CatalogEpisodeFile, Episode, EpisodeFile, Series};
        CatalogSeries {
            id,
            series: Series {
                tmdb_id: u32::try_from(id).unwrap(),
                tvdb_id: None,
                imdb_id: None,
                title: "Show".into(),
                original_title: None,
                metadata_title: None,
                original_language: None,
                year: Some(2024),
                status: None,
                overview: None,
                network: None,
                runtime: 0,
                poster: None,
                fanart: None,
                path: "/media/series/Show".into(),
                season_folder: true,
                monitor_new: true,
                added: None,
                refreshed_at: None,
                alternate_titles: Vec::new(),
            },
            episodes: episodes
                .iter()
                .map(|&(id, season, number, file_id)| CatalogEpisode {
                    id,
                    episode: Episode {
                        season,
                        number,
                        tmdb_id: None,
                        title: None,
                        air_date: None,
                        overview: None,
                        runtime: 0,
                    },
                    skip: None,
                    skipped_at: None,
                    file_id,
                })
                .collect(),
            files: files
                .iter()
                .map(|&(id, size, added)| CatalogEpisodeFile {
                    id,
                    file: EpisodeFile {
                        relative_path: format!("e{id}.mkv"),
                        size,
                        quality: acervo_parser::parse_quality("x.1080p.WEB-DL"),
                        languages: Vec::new(),
                        release_group: None,
                        scene_name: None,
                        date_added: added.map(str::to_owned),
                    },
                })
                .collect(),
            priority: false,
            scene: Vec::new(),
            subtitles: Vec::new(),
        }
    }

    #[test]
    fn sugere_a_temporada_em_que_todo_arquivo_sairia() {
        let before = Some("2026-08-01T00:00:00Z");
        // T1: dois arquivos que saem. T2: um sai, outro é favorito. T3: o
        // arquivo chegou depois de assistirem. T4: nenhum veredito (ninguém
        // assistiu). O episódio sem arquivo da T1 não conta.
        let entry = sample(
            7,
            &[
                (1, 1, 1, Some(10)),
                (2, 1, 2, Some(11)),
                (3, 1, 3, None),
                (4, 2, 1, Some(20)),
                (5, 2, 2, Some(21)),
                (6, 3, 1, Some(30)),
                (7, 4, 1, Some(40)),
            ],
            &[
                (10, 100, before),
                (11, 50, before),
                (20, 1, before),
                (21, 1, before),
                (30, 1, Some("2026-09-10T00:00:00Z")),
                (40, 1, before),
            ],
        );
        let delete = |user: &str, when: &str| Candidate {
            user: user.into(),
            at: at(when),
        };
        let candidates = BTreeMap::from([
            (10, delete("vitor", "2026-09-01T00:00:00Z")),
            (11, delete("ana", "2026-09-02T00:00:00Z")),
            (20, delete("vitor", "2026-09-01T00:00:00Z")),
            (30, delete("vitor", "2026-09-01T00:00:00Z")),
        ]);
        assert_eq!(
            season_suggestions(&entry, &candidates),
            [SeasonSuggestion {
                series_id: 7,
                season: 1,
                files: 2,
                size: 150,
                user: "ana".into(),
                at: at("2026-09-02T00:00:00Z"),
            }]
        );
    }
}

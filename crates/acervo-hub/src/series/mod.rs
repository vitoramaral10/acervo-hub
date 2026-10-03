//! Séries, com o contrato de `docs/series.md`: o desenho dos filmes (busca,
//! decisão, grab, fila por espaço, importação por hardlink, remoção,
//! assistidos), com o episódio guardando o motivo de não ser buscado, o
//! pacote de temporada servindo para um episódio só e nenhum upgrade.

use std::collections::{BTreeMap, HashSet};

use acervo_decision::{EpisodeState, EpisodeTarget, SeriesTarget};
use acervo_parser::{Language, QualityModel, Revision, clean_series_title};
use acervo_store::{CatalogEpisode, CatalogSeries, GrabState, SeriesGrab};
use time::{Date, OffsetDateTime};

pub mod grab;
pub mod import;
pub mod library;
pub mod migrate;
pub mod naming;
pub mod remove;
pub mod search;
pub mod watched;
pub mod web;

/// Categoria Newznab de TV.
pub const TV: u32 = 5000;

/// `AAAA-MM-DD` (o resto, se houver, não conta).
pub(crate) fn date(text: Option<&str>) -> Option<Date> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(text?.get(..10)?, &format).ok()
}

pub(crate) fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// Foi ao ar: `air_date <= hoje` (UTC). Sem data, não foi.
#[must_use]
pub fn aired(episode: &acervo_store::Episode, today: Date) -> bool {
    date(episode.air_date.as_deref()).is_some_and(|d| d <= today)
}

/// `S01E02`, `S01E01-E03`, `S01E01-E03+E05`; temporadas separadas por
/// espaço.
#[must_use]
pub fn episode_code(numbers: &[(u16, u16)]) -> String {
    let mut seasons: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
    for (season, number) in numbers {
        seasons.entry(*season).or_default().push(*number);
    }
    seasons
        .into_iter()
        .map(|(season, mut numbers)| {
            numbers.sort_unstable();
            numbers.dedup();
            let runs: Vec<String> = runs(&numbers)
                .into_iter()
                .map(|(first, last)| {
                    if first == last {
                        format!("E{first:02}")
                    } else {
                        format!("E{first:02}-E{last:02}")
                    }
                })
                .collect();
            format!("S{season:02}{}", runs.join("+"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Sequências de números consecutivos, de uma lista ordenada.
pub(crate) fn runs(numbers: &[u16]) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = Vec::new();
    for &n in numbers {
        match out.last_mut() {
            Some((_, last)) if *last + 1 == n => *last = n,
            _ => out.push((n, n)),
        }
    }
    out
}

/// "Série S01E02": como os eventos, a fila e as notificações nomeiam o que
/// aconteceu.
#[must_use]
pub fn label(entry: &CatalogSeries, episode_ids: &[i64]) -> String {
    let numbers: Vec<(u16, u16)> = entry
        .episodes
        .iter()
        .filter(|e| episode_ids.contains(&e.id))
        .map(|e| (e.episode.season, e.episode.number))
        .collect();
    let code = episode_code(&numbers);
    if code.is_empty() {
        entry.series.title.clone()
    } else {
        format!("{} {code}", entry.series.title)
    }
}

/// Os episódios com grab em andamento.
pub(crate) fn queued(grabs: &[SeriesGrab]) -> HashSet<i64> {
    grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .flat_map(|g| g.episode_ids.iter().copied())
        .collect()
}

/// Quero, Tenho, Dispensado — ou Quero com grab em andamento.
pub(crate) fn state(
    entry: &CatalogSeries,
    episode: &CatalogEpisode,
    queued: &HashSet<i64>,
) -> EpisodeState {
    if let Some(file_id) = episode.file_id {
        let quality = entry.files.iter().find(|f| f.id == file_id).map_or(
            QualityModel {
                quality: acervo_parser::Quality::Unknown,
                revision: Revision::default(),
            },
            |f| f.file.quality,
        );
        EpisodeState::Have(quality)
    } else if episode.skip.is_some() {
        EpisodeState::Skipped
    } else if queued.contains(&episode.id) {
        EpisodeState::Queued
    } else {
        EpisodeState::Wanted
    }
}

/// A série como a decisão a lê.
pub(crate) fn target(entry: &CatalogSeries, queued: &HashSet<i64>, today: Date) -> SeriesTarget {
    let series = &entry.series;
    let mut titles: Vec<String> = Vec::new();
    for title in std::iter::once(&series.title)
        .chain(&series.original_title)
        .chain(&series.metadata_title)
        .chain(&series.alternate_titles)
    {
        let clean = clean_series_title(title);
        if !clean.trim().is_empty() && !titles.contains(&clean) {
            titles.push(clean);
        }
    }
    SeriesTarget {
        id: entry.id,
        tvdb_id: series.tvdb_id,
        titles,
        year: series.year,
        runtime: series.runtime,
        language: series
            .original_language
            .as_deref()
            .and_then(Language::from_name)
            .unwrap_or(Language::Unknown),
        episodes: entry
            .episodes
            .iter()
            .map(|e| EpisodeTarget {
                id: e.id,
                season: e.episode.season,
                number: e.episode.number,
                aired: aired(&e.episode, today),
                state: state(entry, e, queued),
            })
            .collect(),
    }
}

/// O título de pasta e arquivo: o da base de metadados, em inglês.
pub(crate) fn folder_title(series: &acervo_store::Series) -> &str {
    series.metadata_title.as_deref().unwrap_or(&series.title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codigo_de_episodios() {
        assert_eq!(episode_code(&[(1, 2)]), "S01E02");
        assert_eq!(episode_code(&[(1, 3), (1, 1), (1, 2)]), "S01E01-E03");
        assert_eq!(episode_code(&[(1, 1), (1, 2), (1, 5)]), "S01E01-E02+E05");
        assert_eq!(episode_code(&[(2, 1), (1, 10)]), "S01E10 S02E01");
        assert_eq!(episode_code(&[]), "");
    }

    #[test]
    fn foi_ao_ar_e_ate_hoje_e_sem_data_nao() {
        let today = date(Some("2026-10-03")).unwrap();
        let episode = |air: Option<&str>| acervo_store::Episode {
            season: 1,
            number: 1,
            tmdb_id: None,
            title: None,
            air_date: air.map(str::to_owned),
            overview: None,
            runtime: 0,
        };
        assert!(aired(&episode(Some("2026-10-03")), today));
        assert!(aired(&episode(Some("2020-01-01")), today));
        assert!(!aired(&episode(Some("2026-10-04")), today));
        assert!(!aired(&episode(None), today));
    }
}

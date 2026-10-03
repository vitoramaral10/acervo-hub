//! Ordem de preferência entre releases do mesmo filme.
//!
//! Com `healthy_seeders` no perfil, a saúde do torrent vem antes de tudo:
//! bem semeado, fraco, morto. Depois, critérios em cascata, como na
//! referência: qualidade (posição no perfil, depois revisão), prioridade do
//! indexador, seeders e peers em ordem de grandeza, e tamanho mais perto do
//! preferido. Flags do indexador contam só se preferidas. Protocolo e idade
//! não separam nada: é tudo torrent.

use std::cmp::Ordering;

use crate::specs::{megabytes, runtime};
use acervo_parser::{ParsedMovie, QualityModel};

use crate::{Decision, Engine, Indexer, Profile, Propers, Release, Settings};

const DEFAULT_PRIORITY: i32 = 25;

const FREELEECH: u32 = 1;
const HALFLEECH: u32 = 2;
const DOUBLE_UPLOAD: u32 = 4;
const GOLDEN: u32 = 8;
const APPROVED: u32 = 16;
const INTERNAL: u32 = 32;

/// Freeleech, upload dobrado, internal e afins valem 2; halfleech, 1.
fn flag_score(flags: u32) -> i32 {
    let strong = [FREELEECH, DOUBLE_UPLOAD, GOLDEN, APPROVED, INTERNAL]
        .iter()
        .filter(|bit| flags & **bit != 0)
        .count();
    let strong = i32::try_from(strong).unwrap_or(0) * 2;
    strong + i32::from(flags & HALFLEECH != 0)
}

/// Ordem de grandeza arredondada para o par, como o `Math.Round` do .NET.
#[allow(clippy::cast_possible_truncation)]
fn magnitude(value: Option<u32>) -> i64 {
    match value {
        Some(v) if v > 0 => f64::from(v).log10().round_ties_even() as i64,
        _ => 0,
    }
}

/// `Round(nível)` da referência: trunca em direção ao zero.
fn truncate_to(value: i64, level: i64) -> i64 {
    value / level * level
}

/// Quão perto o tamanho fica do preferido, em degraus de 200 MB; sem
/// preferido ou sem duração, só o tamanho truncado.
pub(crate) fn size_score(preferred: Option<f64>, size: u64, minutes: Option<i64>) -> i64 {
    let level = megabytes(200.0);
    let size = i64::try_from(size).unwrap_or(i64::MAX);
    match (preferred, minutes) {
        (Some(preferred), Some(minutes)) => {
            let target = minutes * megabytes(preferred);
            -truncate_to(size - target, level).abs()
        }
        _ => truncate_to(size, level),
    }
}

fn movie_size_score(engine: &Engine<'_>, decision: &Decision, release: &Release) -> i64 {
    let preferred = decision
        .parsed
        .as_ref()
        .and_then(|p| engine.settings.definition(p.quality.quality))
        .and_then(|d| d.preferred_size);
    let movie = decision.movie.and_then(|id| engine.movie(id));
    size_score(preferred, release.size, movie.map(runtime))
}

/// Faixa de saúde: 2 com seeders suficientes, 1 com algum (ou sem contagem),
/// 0 sem nenhum.
fn health(seeders: Option<u32>, healthy: u32) -> u8 {
    match seeders {
        Some(0) => 0,
        Some(count) if count >= healthy => 2,
        _ => 1,
    }
}

/// Os critérios de preferência de um release, já na ordem em que decidem:
/// comparar duas chaves é o `compare`. Maior é melhor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Key {
    health: u8,
    quality: i64,
    version: u8,
    real: u8,
    /// Negativo: prioridade menor do indexador é melhor.
    priority: i64,
    flags: i32,
    seeders: i64,
    peers: i64,
    size: i64,
}

pub(crate) fn key(
    settings: &Settings,
    indexers: &[Indexer],
    profile: &Profile,
    quality: QualityModel,
    release: &Release,
    size: i64,
) -> Key {
    let propers = settings.propers != Propers::DoNotPrefer;
    let priority =
        crate::find_indexer(indexers, &release.indexer).map_or(DEFAULT_PRIORITY, |i| i.priority);
    Key {
        health: profile
            .healthy_seeders
            .map_or(0, |healthy| health(release.seeders, healthy)),
        quality: profile.index(quality.quality),
        version: if propers { quality.revision.version } else { 0 },
        real: if propers { quality.revision.real } else { 0 },
        priority: -i64::from(priority),
        flags: if settings.prefer_indexer_flags {
            flag_score(release.flags)
        } else {
            0
        },
        seeders: magnitude(release.seeders),
        peers: magnitude(release.peers),
        size,
    }
}

/// Compara dois releases do mesmo filme: `Greater` é o preferido.
#[must_use]
pub fn compare(engine: &Engine<'_>, releases: &[Release], a: &Decision, b: &Decision) -> Ordering {
    let (Some(pa), Some(pb)) = (&a.parsed, &b.parsed) else {
        return Ordering::Equal;
    };
    let Some(movie) = a.movie.and_then(|id| engine.movie(id)) else {
        return Ordering::Equal;
    };
    let (ra, rb) = (&releases[a.release], &releases[b.release]);
    let key = |parsed: &ParsedMovie, release: &Release, decision: &Decision| {
        key(
            engine.settings,
            engine.indexers,
            &movie.profile,
            parsed.quality,
            release,
            movie_size_score(engine, decision, release),
        )
    };
    key(pa, ra, a).cmp(&key(pb, rb, b))
}

/// Agrupa por filme, na ordem em que cada filme aparece, e ordena cada grupo
/// do preferido ao pior; empate mantém a ordem recebida. Os que não casaram
/// com filme vão para o fim.
pub(crate) fn prioritize(
    engine: &Engine<'_>,
    releases: &[Release],
    decisions: Vec<Decision>,
) -> Vec<Decision> {
    let mut movies: Vec<i64> = Vec::new();
    for id in decisions.iter().filter_map(|d| d.movie) {
        if !movies.contains(&id) {
            movies.push(id);
        }
    }
    let (mut matched, unmatched): (Vec<_>, Vec<_>) =
        decisions.into_iter().partition(|d| d.movie.is_some());
    let mut ordered = Vec::with_capacity(matched.len() + unmatched.len());
    for id in movies {
        let mut group: Vec<Decision> = Vec::new();
        let mut rest = Vec::new();
        for decision in matched {
            if decision.movie == Some(id) {
                group.push(decision);
            } else {
                rest.push(decision);
            }
        }
        matched = rest;
        group.sort_by(|a, b| compare(engine, releases, b, a));
        ordered.extend(group);
    }
    ordered.extend(unmatched);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grandeza_e_truncamento() {
        assert_eq!(magnitude(None), 0);
        assert_eq!(magnitude(Some(1)), 0);
        assert_eq!(magnitude(Some(3)), 0);
        assert_eq!(magnitude(Some(4)), 1);
        assert_eq!(magnitude(Some(40)), 2);
        assert_eq!(truncate_to(450, 200), 400);
        assert_eq!(truncate_to(-450, 200), -400);
    }

    #[test]
    fn faixas_de_saude() {
        assert_eq!(health(Some(0), 5), 0);
        assert_eq!(health(Some(1), 5), 1);
        assert_eq!(health(Some(4), 5), 1);
        assert_eq!(health(None, 5), 1);
        assert_eq!(health(Some(5), 5), 2);
        assert_eq!(health(Some(500), 5), 2);
    }
}

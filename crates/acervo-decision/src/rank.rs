//! Ordem de preferência entre releases do mesmo filme.
//!
//! Critérios em cascata, como na referência: qualidade (posição no perfil,
//! depois revisão), nota de formatos, prioridade do indexador, seeders e peers em ordem de
//! grandeza, e tamanho mais perto do preferido. Flags do indexador contam só
//! se preferidas. Protocolo e idade não separam nada: é tudo torrent.

use std::cmp::Ordering;

use crate::specs::{megabytes, runtime};
use crate::{Decision, Engine, Propers, Release};

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

fn size_score(engine: &Engine<'_>, decision: &Decision, release: &Release) -> i64 {
    let level = megabytes(200.0);
    let size = i64::try_from(release.size).unwrap_or(i64::MAX);
    let preferred = decision
        .parsed
        .as_ref()
        .and_then(|p| engine.settings.definition(p.quality.quality))
        .and_then(|d| d.preferred_size);
    let movie = decision.movie.and_then(|id| engine.movie(id));
    match (preferred, movie) {
        (Some(preferred), Some(movie)) => {
            let target = runtime(movie) * megabytes(preferred);
            -truncate_to(size - target, level).abs()
        }
        _ => truncate_to(size, level),
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
    let profile = &movie.profile;

    let mut quality = profile
        .index(pa.quality.quality)
        .cmp(&profile.index(pb.quality.quality));
    if engine.settings.propers != Propers::DoNotPrefer {
        quality = quality.then(
            pa.quality
                .revision
                .version
                .cmp(&pb.quality.revision.version)
                .then(pa.quality.revision.real.cmp(&pb.quality.revision.real)),
        );
    }
    let priority = |r: &Release| {
        engine
            .indexer(&r.indexer)
            .map_or(DEFAULT_PRIORITY, |i| i.priority)
    };
    let flags = if engine.settings.prefer_indexer_flags {
        flag_score(ra.flags).cmp(&flag_score(rb.flags))
    } else {
        Ordering::Equal
    };
    quality
        .then(a.format_score.cmp(&b.format_score))
        .then(priority(rb).cmp(&priority(ra)))
        .then(flags)
        .then(magnitude(ra.seeders).cmp(&magnitude(rb.seeders)))
        .then(magnitude(ra.peers).cmp(&magnitude(rb.peers)))
        .then(size_score(engine, a, ra).cmp(&size_score(engine, b, rb)))
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
}

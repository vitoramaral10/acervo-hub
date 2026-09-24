//! As especificações: cada uma recusa o release por um motivo, e o release só
//! serve se nenhuma recusar. Todas são avaliadas — a lista de motivos é
//! completa, como na referência.

use std::cmp::Ordering;
use std::sync::LazyLock;

use acervo_parser::{Language, ParsedMovie, QualityModel, Revision};
use fancy_regex::Regex;

use crate::{Engine, ExistingFile, Mode, Profile, Propers, Rejection, Release, Target};

const MEGABYTE: f64 = 1024.0 * 1024.0;

/// `Megabytes()` da referência: arredonda para o par mais próximo.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn megabytes(value: f64) -> i64 {
    (value * MEGABYTE).round_ties_even() as i64
}

/// Duração usada no cálculo de tamanho: sem ela, a mediana de 110 minutos.
pub(crate) fn runtime(movie: &Target) -> i64 {
    if movie.runtime == 0 {
        110
    } else {
        i64::from(movie.runtime)
    }
}

static DISC: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)(?:dis[ck])(?:[-_. ]\d+[-_. ])(?:(?:(?:480|720|1080|2160)[ip]|)[-_. ])?(?:Blu\-?ray)",
        r"(?i)(?:(?:480|720|1080|2160)[ip]|)[-_. ](?:full)[-_. ](?:Blu\-?ray)",
        r"(?i)(?:\d?x?M?DVD-?[R59])(?:[ ._]|$)",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap_or_else(|e| panic!("{p}: {e}")))
    .collect()
});

fn revision_cmp(a: Revision, b: Revision) -> Ordering {
    a.version.cmp(&b.version).then(a.real.cmp(&b.real))
}

fn quality_cmp(profile: &Profile, a: QualityModel, b: QualityModel) -> Ordering {
    profile
        .index(a.quality)
        .cmp(&profile.index(b.quality))
        .then(revision_cmp(a.revision, b.revision))
}

pub(crate) fn is_revision_upgrade(current: QualityModel, new: QualityModel) -> bool {
    current.quality == new.quality && revision_cmp(new.revision, current.revision).is_gt()
}

fn quality_cutoff_not_met(profile: &Profile, current: QualityModel, new: QualityModel) -> bool {
    profile.index(current.quality) < profile.effective_cutoff() || is_revision_upgrade(current, new)
}

/// Custom format não entra: nota zero dos dois lados, sempre.
fn cutoff_not_met(profile: &Profile, current: QualityModel, new: QualityModel) -> bool {
    let format_cutoff = if profile.upgrade_allowed {
        profile.cutoff_format_score
    } else {
        profile.min_format_score
    };
    quality_cutoff_not_met(profile, current, new) || 0 < format_cutoff
}

enum NotUpgradable {
    BetterQuality,
    BetterRevision,
    QualityCutoff,
    CustomFormatScore,
    UpgradesNotAllowed,
}

fn is_upgradable(
    profile: &Profile,
    propers: Propers,
    current: QualityModel,
    new: QualityModel,
) -> Result<(), NotUpgradable> {
    let quality = profile
        .index(new.quality)
        .cmp(&profile.index(current.quality));
    if quality.is_gt() && quality_cutoff_not_met(profile, current, new) {
        return Ok(());
    }
    if quality.is_lt() {
        return Err(NotUpgradable::BetterQuality);
    }
    let revision = revision_cmp(new.revision, current.revision);
    if propers != Propers::DoNotPrefer && revision.is_gt() {
        return Ok(());
    }
    if !profile.upgrade_allowed {
        return Err(NotUpgradable::UpgradesNotAllowed);
    }
    if propers != Propers::DoNotPrefer && revision.is_lt() {
        return Err(NotUpgradable::BetterRevision);
    }
    if quality.is_gt() {
        return Err(NotUpgradable::QualityCutoff);
    }
    // Nota nova (zero) nunca supera a atual (zero).
    Err(NotUpgradable::CustomFormatScore)
}

fn is_upgrade_allowed(profile: &Profile, current: QualityModel, new: QualityModel) -> bool {
    if is_revision_upgrade(current, new) {
        return true;
    }
    let quality_upgrade = quality_cmp(profile, new, current).is_gt();
    !quality_upgrade || profile.upgrade_allowed
}

fn size_checks(
    engine: &Engine<'_>,
    release: &Release,
    parsed: &ParsedMovie,
    movie: &Target,
    out: &mut Vec<Rejection>,
) {
    let size = i64::try_from(release.size).unwrap_or(i64::MAX);
    if size > 0
        && let Some(definition) = engine.settings.definition(parsed.quality.quality)
    {
        let minutes = runtime(movie);
        if let Some(min) = definition.min_size {
            let minimum = megabytes(min) * minutes;
            if size < minimum {
                out.push(Rejection::BelowMinimumSize {
                    size: release.size,
                    minimum: u64::try_from(minimum).unwrap_or(0),
                });
            }
        }
        if let Some(max) = definition.max_size.filter(|m| *m != 0.0) {
            let maximum = megabytes(max) * minutes;
            if size > maximum {
                out.push(Rejection::AboveMaximumSize {
                    size: release.size,
                    maximum: u64::try_from(maximum).unwrap_or(0),
                });
            }
        }
    }
    let ceiling = engine.settings.maximum_size_mb;
    if ceiling > 0 && release.size > 0 {
        let maximum = ceiling * 1024 * 1024;
        if release.size > maximum {
            out.push(Rejection::MaximumSizeExceeded {
                size: release.size,
                maximum,
            });
        }
    }
    if release.title.to_lowercase().contains("sample") && size < megabytes(70.0) {
        out.push(Rejection::Sample);
    }
}

fn file_checks(
    engine: &Engine<'_>,
    parsed: &ParsedMovie,
    profile: &Profile,
    file: &ExistingFile,
    rss: bool,
    out: &mut Vec<Rejection>,
) {
    let current = file.quality;
    let new = parsed.quality;
    let propers = engine.settings.propers;

    if !is_upgrade_allowed(profile, current, new) {
        out.push(Rejection::QualityUpgradesDisabled);
    }
    if cutoff_not_met(profile, current, new) {
        match is_upgradable(profile, propers, current, new) {
            Ok(()) => {}
            Err(NotUpgradable::BetterQuality) => out.push(Rejection::DiskHigherPreference),
            Err(NotUpgradable::BetterRevision) => out.push(Rejection::DiskHigherRevision),
            Err(NotUpgradable::QualityCutoff) => out.push(Rejection::DiskCutoffMet),
            Err(NotUpgradable::CustomFormatScore) => out.push(Rejection::DiskCustomFormatScore),
            Err(NotUpgradable::UpgradesNotAllowed) => out.push(Rejection::DiskUpgradesNotAllowed),
        }
    } else {
        out.push(Rejection::DiskCutoffMet);
    }

    if new.revision.is_repack
        && propers != Propers::DoNotPrefer
        && is_revision_upgrade(current, new)
    {
        let release_group = parsed
            .release_group
            .as_deref()
            .filter(|g| !g.trim().is_empty());
        let file_group = file
            .release_group
            .as_deref()
            .filter(|g| !g.trim().is_empty());
        match (propers, file_group, release_group) {
            (Propers::DoNotUpgrade, _, _) => out.push(Rejection::RepackDisabled),
            (_, None, _) | (_, _, None) => out.push(Rejection::RepackUnknownReleaseGroup),
            (_, Some(file_group), Some(release_group))
                if !file_group.eq_ignore_ascii_case(release_group) =>
            {
                out.push(Rejection::RepackReleaseGroupDoesNotMatch);
            }
            _ => {}
        }
    }

    if rss && propers != Propers::DoNotPrefer && is_revision_upgrade(current, new) {
        if propers == Propers::DoNotUpgrade {
            out.push(Rejection::PropersDisabled);
        } else if file.age_days > 7 {
            out.push(Rejection::ProperForOldFile);
        }
    }
}

pub(crate) fn evaluate(
    engine: &Engine<'_>,
    release: &Release,
    parsed: &ParsedMovie,
    languages: &[Language],
    movie: &Target,
    searched: Option<i64>,
    mode: Mode,
) -> Vec<Rejection> {
    let mut out = Vec::new();
    let profile = &movie.profile;

    if profile
        .items
        .get(usize::try_from(profile.index(parsed.quality.quality)).unwrap_or(usize::MAX))
        .is_none_or(|item| !item.allowed)
    {
        out.push(Rejection::QualityNotWanted(parsed.quality.quality));
    }

    let wanted = match profile.language {
        Language::Any => None,
        Language::Original => Some(movie.original_language),
        other => Some(other),
    };
    if let Some(wanted) = wanted.filter(|w| !languages.contains(w)) {
        out.push(Rejection::WantedLanguage {
            wanted,
            found: languages.to_vec(),
        });
    }

    size_checks(engine, release, parsed, movie, &mut out);

    if let Some(subs) = &parsed.hardcoded_subs
        && !subs.trim().is_empty()
        && !engine.settings.allow_hardcoded_subs
    {
        let allowed = engine
            .settings
            .whitelisted_hardcoded_subs
            .split(',')
            .any(|term| {
                !term.trim().is_empty() && subs.to_lowercase().contains(&term.to_lowercase())
            });
        if !allowed {
            out.push(Rejection::HardcodeSubtitles(subs.clone()));
        }
    }

    let raw_title = DISC
        .iter()
        .any(|r| r.is_match(&release.title).unwrap_or(false));
    let raw_container = release
        .container
        .as_deref()
        .is_some_and(|c| ["vob", "iso", "m2ts"].contains(&c.to_lowercase().as_str()));
    if raw_title || raw_container {
        out.push(Rejection::Raw);
    }

    if let (Some(indexer), Some(seeders)) = (engine.indexer(&release.indexer), release.seeders)
        && seeders < indexer.minimum_seeders
    {
        out.push(Rejection::MinimumSeeders {
            seeders,
            minimum: indexer.minimum_seeders,
        });
    }

    if 0 < profile.min_format_score {
        out.push(Rejection::CustomFormatMinimumScore);
    }

    if let Some(file) = &movie.file {
        file_checks(engine, parsed, profile, file, searched.is_none(), &mut out);
    }

    if searched.is_some_and(|id| id != movie.id) {
        out.push(Rejection::WrongMovie);
    }

    queue_check(engine, parsed, movie, &mut out);

    if mode == Mode::Automatic {
        if !movie.available {
            out.push(Rejection::Availability);
        }
        if !movie.monitored {
            out.push(Rejection::MovieNotMonitored);
        }
    }

    // Prioridade "disco": só roda quando todo o resto aprovou.
    if out.is_empty() {
        free_space_check(engine, release, movie, &mut out);
    }
    out
}

/// Download já na fila que atinge o corte, ou que é igual ou melhor, barra o
/// release — como no arquivo em disco.
fn queue_check(
    engine: &Engine<'_>,
    parsed: &ParsedMovie,
    movie: &Target,
    out: &mut Vec<Rejection>,
) {
    let profile = &movie.profile;
    let new = parsed.quality;
    for &queued in &movie.queued {
        if !cutoff_not_met(profile, queued, new) {
            out.push(Rejection::QueueCutoffMet);
            return;
        }
        let rejection = match is_upgradable(profile, engine.settings.propers, queued, new) {
            Ok(()) => None,
            Err(NotUpgradable::BetterQuality) => Some(Rejection::QueueHigherPreference),
            Err(NotUpgradable::BetterRevision) => Some(Rejection::QueueHigherRevision),
            Err(NotUpgradable::QualityCutoff) => Some(Rejection::QueueCutoffMet),
            Err(NotUpgradable::CustomFormatScore) => Some(Rejection::QueueCustomFormatScore),
            Err(NotUpgradable::UpgradesNotAllowed) => Some(Rejection::QueueUpgradesNotAllowed),
        };
        if let Some(rejection) = rejection {
            out.push(rejection);
            return;
        }
        if is_revision_upgrade(queued, new) && engine.settings.propers == Propers::DoNotUpgrade {
            out.push(Rejection::QueuePropersDisabled);
            return;
        }
    }
}

fn free_space_check(
    engine: &Engine<'_>,
    release: &Release,
    movie: &Target,
    out: &mut Vec<Rejection>,
) {
    if engine.settings.skip_free_space_check {
        return;
    }
    let Some(free) = movie.free_space else {
        return;
    };
    let minimum =
        i64::try_from(engine.settings.minimum_free_space_mb).unwrap_or(i64::MAX) * 1024 * 1024;
    let remaining =
        i64::try_from(free).unwrap_or(i64::MAX) - i64::try_from(release.size).unwrap_or(i64::MAX);
    if remaining <= 0 || remaining < minimum {
        out.push(Rejection::MinimumFreeSpace { remaining });
    }
}

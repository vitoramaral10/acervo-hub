//! Qualidade de um release de episódio.
//!
//! O gerenciador de séries tem o próprio parser de qualidade, parecido com o
//! de filmes mas não igual: sem CAM/TS/SCR/BR-DISK, com `Xvid` e `H265` mais
//! estritos, outra tabela de extensões e o 576p só no Bluray. Este módulo é o
//! porte dele; o de filmes (`quality.rs`) não muda. Os nomes de remux seguem o
//! enum compartilhado (`Remux1080p` é o "Bluray-1080p Remux" de lá).

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::common::{
    captures, episode_quality_for_extension, is_match, last_captures, path_extension, regex,
};
use crate::quality::{
    ALTERNATIVE_RESOLUTION, ANIME_BLURAY, ANIME_WEB_DL, HIGH_DEF_PDTV, MPEG2, OTHER_SOURCE, PROPER,
    Quality, QualityModel, RAW_HD, REAL, REPACK, Resolution, Revision, contains_ignore_case,
};

static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)\b(?:",
        r"(?<bluray>BluRay|Blu-Ray|HD-?DVD|BDMux|BD(?!$))|",
        r"(?<webdl>WEB[-_. ]DL(?:mux)?|WEBDL|AmazonHD|AmazonSD|iTunesHD|MaxdomeHD|NetflixU?HD|WebHD|HBOMaxHD|DisneyHD|[. ]WEB[. ](?:[xh][ .]?26[45]|AVC|HEVC|DDP?5[. ]1)|[. ](?-i:WEB)$|(?:720|1080|2160)p[-. ]WEB[-. ]|[-. ]WEB[-. ](?:720|1080|2160)p|\b\s/\sWEB\s/\s\b|(?:AMZN|NF|DP)[. -]WEB[. -](?!Rip))|",
        r"(?<webrip>WebRip|Web-Rip|WEBMux)|",
        r"(?<hdtv>HDTV)|",
        r"(?<bdrip>BDRip|BDLight)|",
        r"(?<brrip>BRRip)|",
        r"(?<dvd>DVD|DVDRip|NTSC|PAL|xvidvd)|",
        r"(?<dsr>WS[-_. ]DSR|DSR)|",
        r"(?<pdtv>PDTV)|",
        r"(?<sdtv>SDTV)|",
        r"(?<tvrip>TVRip)",
        r")(?:\b|$|[ .])",
    ))
});

static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\d[-._ ]?v(?<v1>\d)[-._ ]|\[v(?<v2>\d)\]|repack(?<v3>\d)|rerip(?<v4>\d)|(?:480|576|720|1080|2160)p[._ ]v(?<v5>\d)",
    )
});

static RESOLUTION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?:(?<R360p>360p)|(?<R480p>480p|480i|640x480|848x480)|(?<R540p>540p)|(?<R576p>576p)|(?<R720p>720p|1280x720|960p)|(?<R1080p>1080p|1920x1080|1440p|FHD|1080i|4kto1080p)|(?<R2160p>2160p|3840x2160|4k[-_. ](?:UHD|HEVC|BD|H265)|(?:UHD|HEVC|BD|H265)[-_. ]4k))\b",
    )
});

static CODEC: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)\b(?:(?<x264>x264)|(?<h264>h264)|(?<xvidhd>XvidHD)|(?<xvid>Xvid)|(?<divx>divx))\b")
});

static REMUX: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?:[_. ]|\d{4}p-|\bHybrid-)(?<remux>(?:(BD|UHD)[-_. ]?)?Remux)\b|(?<remux2>(?:(BD|UHD)[-_. ]?)?Remux[_. ]\d{4}p)",
    )
});

/// A tabela de qualidades do gerenciador de séries: fonte, resolução e o que
/// ele chama de fonte (`BlurayRaw` é remux).
const fn series_source(quality: Quality) -> (SeriesSource, u16) {
    use SeriesSource as S;
    match quality {
        Quality::Sdtv => (S::Television, 480),
        Quality::Dvd => (S::Dvd, 480),
        Quality::WebDl1080p => (S::Web, 1080),
        Quality::Hdtv720p => (S::Television, 720),
        Quality::WebDl720p => (S::Web, 720),
        Quality::Bluray720p => (S::Bluray, 720),
        Quality::Bluray1080p => (S::Bluray, 1080),
        Quality::WebDl480p => (S::Web, 480),
        Quality::Hdtv1080p => (S::Television, 1080),
        Quality::RawHd => (S::TelevisionRaw, 1080),
        Quality::WebRip480p => (S::WebRip, 480),
        Quality::Bluray480p => (S::Bluray, 480),
        Quality::Bluray576p => (S::Bluray, 576),
        Quality::WebRip720p => (S::WebRip, 720),
        Quality::WebRip1080p => (S::WebRip, 1080),
        Quality::Hdtv2160p => (S::Television, 2160),
        Quality::WebRip2160p => (S::WebRip, 2160),
        Quality::WebDl2160p => (S::Web, 2160),
        Quality::Bluray2160p => (S::Bluray, 2160),
        Quality::Remux1080p => (S::BlurayRaw, 1080),
        Quality::Remux2160p => (S::BlurayRaw, 2160),
        _ => (S::Unknown, 0),
    }
}

/// Mesma ordem do `QualitySource` de lá: é a da busca pela qualidade vizinha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SeriesSource {
    Unknown,
    Television,
    TelevisionRaw,
    Web,
    WebRip,
    Dvd,
    Bluray,
    BlurayRaw,
}

/// Séries do enum que o gerenciador de séries conhece, na ordem da tabela dele.
const SERIES_QUALITIES: [Quality; 21] = [
    Quality::Sdtv,
    Quality::Dvd,
    Quality::WebDl1080p,
    Quality::Hdtv720p,
    Quality::WebDl720p,
    Quality::Bluray720p,
    Quality::Bluray1080p,
    Quality::WebDl480p,
    Quality::Hdtv1080p,
    Quality::RawHd,
    Quality::WebRip480p,
    Quality::Bluray480p,
    Quality::Bluray576p,
    Quality::WebRip720p,
    Quality::WebRip1080p,
    Quality::Hdtv2160p,
    Quality::WebRip2160p,
    Quality::WebDl2160p,
    Quality::Bluray2160p,
    Quality::Remux1080p,
    Quality::Remux2160p,
];

/// `QualityFinder.FindBySourceAndResolution`.
fn find_by_source_and_resolution(source: SeriesSource, resolution: u16) -> Quality {
    if let Some(exact) = SERIES_QUALITIES
        .into_iter()
        .find(|q| series_source(*q) == (source, resolution))
    {
        return exact;
    }
    // 576p de TV ou web não sobe para Bluray 576p.
    if resolution < 720 {
        match source {
            SeriesSource::Television => return Quality::Sdtv,
            SeriesSource::Web => return Quality::WebDl480p,
            SeriesSource::WebRip => return Quality::WebRip480p,
            _ => {}
        }
    }
    let mut same_resolution: Vec<_> = SERIES_QUALITIES
        .into_iter()
        .filter(|q| series_source(*q).1 == resolution)
        .collect();
    same_resolution.sort_by_key(|q| series_source(*q).0);
    same_resolution
        .into_iter()
        .find(|q| series_source(*q).0 >= source)
        .unwrap_or(Quality::Unknown)
}

/// Qualidade de um nome de release ou de arquivo de episódio.
#[must_use]
pub fn parse_episode_quality(name: &str) -> QualityModel {
    let name = name.trim();
    if name.is_empty() {
        return QualityModel {
            quality: Quality::Unknown,
            revision: Revision::default(),
        };
    }
    let mut result = parse_quality_name(name);
    if result.quality == Quality::Unknown && !name.contains('\0') {
        result.quality = episode_quality_for_extension(path_extension(name));
    }
    result
}

pub(crate) fn parse_quality_name(name: &str) -> QualityModel {
    let normalized = name.replace('_', " ");
    let normalized = normalized.trim();
    let revision = parse_revision(name, normalized);
    QualityModel {
        quality: parse_quality_only(name, normalized),
        revision,
    }
}

#[allow(clippy::too_many_lines)]
fn parse_quality_only(name: &str, normalized: &str) -> Quality {
    if is_match(&RAW_HD, normalized) {
        return Quality::RawHd;
    }

    let source = last_captures(&SOURCE, normalized);
    let resolution = parse_resolution(normalized);
    let codec = captures(&CODEC, normalized);
    let codec_is = |group_name: &str| codec.as_ref().is_some_and(|c| c.name(group_name).is_some());
    let remux = is_match(&REMUX, normalized);

    if let Some(source) = &source {
        let is = |group_name: &str| source.name(group_name).is_some();
        if is("bluray") {
            if codec_is("xvid") || codec_is("divx") {
                return Quality::Bluray480p;
            }
            return match resolution {
                Resolution::R2160p if remux => Quality::Remux2160p,
                Resolution::R2160p => Quality::Bluray2160p,
                // Remux sem resolução é 1080p, não 720p.
                Resolution::R1080p | Resolution::Unknown if remux => Quality::Remux1080p,
                Resolution::R1080p => Quality::Bluray1080p,
                Resolution::R576p => Quality::Bluray576p,
                Resolution::R360p | Resolution::R480p | Resolution::R540p => Quality::Bluray480p,
                Resolution::R720p | Resolution::Unknown => Quality::Bluray720p,
            };
        }
        if is("webdl") {
            return match resolution {
                Resolution::R2160p => Quality::WebDl2160p,
                Resolution::R1080p => Quality::WebDl1080p,
                Resolution::R720p => Quality::WebDl720p,
                _ if name.contains("[WEBDL]") => Quality::WebDl720p,
                _ => Quality::WebDl480p,
            };
        }
        if is("webrip") {
            return match resolution {
                Resolution::R2160p => Quality::WebRip2160p,
                Resolution::R1080p => Quality::WebRip1080p,
                Resolution::R720p => Quality::WebRip720p,
                _ => Quality::WebRip480p,
            };
        }
        if is("hdtv") {
            if is_match(&MPEG2, normalized) {
                return Quality::RawHd;
            }
            return match resolution {
                Resolution::R2160p => Quality::Hdtv2160p,
                Resolution::R1080p => Quality::Hdtv1080p,
                Resolution::R720p => Quality::Hdtv720p,
                _ if name.contains("[HDTV]") => Quality::Hdtv720p,
                _ => Quality::Sdtv,
            };
        }
        if is("bdrip") || is("brrip") {
            return match resolution {
                Resolution::R720p => Quality::Bluray720p,
                Resolution::R1080p => Quality::Bluray1080p,
                Resolution::R2160p => Quality::Bluray2160p,
                _ => Quality::Bluray480p,
            };
        }
        if is("dvd") {
            return Quality::Dvd;
        }
        if is("pdtv") || is("sdtv") || is("dsr") || is("tvrip") {
            if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
                return Quality::Hdtv1080p;
            }
            if resolution == Resolution::R720p || contains_ignore_case(normalized, "720p") {
                return Quality::Hdtv720p;
            }
            if is_match(&HIGH_DEF_PDTV, normalized) {
                return Quality::Hdtv720p;
            }
            return Quality::Sdtv;
        }
    }

    if source.is_none() && remux {
        match resolution {
            Resolution::R480p => return Quality::Bluray480p,
            Resolution::R720p => return Quality::Bluray720p,
            Resolution::R2160p => return Quality::Remux2160p,
            Resolution::R1080p => return Quality::Remux1080p,
            _ => {}
        }
    }

    let sd = matches!(
        resolution,
        Resolution::R360p | Resolution::R480p | Resolution::R540p | Resolution::R576p
    );

    if is_match(&ANIME_BLURAY, normalized) {
        if sd || contains_ignore_case(normalized, "480p") {
            return Quality::Dvd;
        }
        if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
            return if remux {
                Quality::Remux1080p
            } else {
                Quality::Bluray1080p
            };
        }
        if resolution == Resolution::R2160p || contains_ignore_case(normalized, "2160p") {
            return if remux {
                Quality::Remux2160p
            } else {
                Quality::Bluray2160p
            };
        }
        if remux && resolution != Resolution::R720p {
            return Quality::Remux1080p;
        }
        return Quality::Bluray720p;
    }

    if is_match(&ANIME_WEB_DL, normalized) {
        if sd || contains_ignore_case(normalized, "480p") {
            return Quality::WebDl480p;
        }
        if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
            return Quality::WebDl1080p;
        }
        if resolution == Resolution::R2160p || contains_ignore_case(normalized, "2160p") {
            return Quality::WebDl2160p;
        }
        return Quality::WebDl720p;
    }

    if resolution != Resolution::Unknown {
        let source = if remux {
            SeriesSource::BlurayRaw
        } else {
            let by_extension = episode_quality_for_extension(path_extension(name));
            if by_extension == Quality::Unknown {
                SeriesSource::Unknown
            } else {
                series_source(by_extension).0
            }
        };
        let (fallback, pixels) = match resolution {
            Resolution::R2160p => (Quality::Hdtv2160p, 2160),
            Resolution::R1080p => (Quality::Hdtv1080p, 1080),
            Resolution::R720p => (Quality::Hdtv720p, 720),
            _ => (Quality::Sdtv, 480),
        };
        return if source == SeriesSource::Unknown {
            fallback
        } else {
            find_by_source_and_resolution(source, pixels)
        };
    }

    if codec_is("x264") {
        return Quality::Sdtv;
    }

    if contains_ignore_case(normalized, "848x480") {
        if normalized.contains("dvd") {
            return Quality::Dvd;
        }
        if contains_ignore_case(normalized, "bluray") {
            return Quality::Bluray480p;
        }
        return Quality::Sdtv;
    }
    if contains_ignore_case(normalized, "1280x720") {
        return if contains_ignore_case(normalized, "bluray") {
            Quality::Bluray720p
        } else {
            Quality::Hdtv720p
        };
    }
    if contains_ignore_case(normalized, "1920x1080") {
        return if contains_ignore_case(normalized, "bluray") {
            Quality::Bluray1080p
        } else {
            Quality::Hdtv1080p
        };
    }
    if contains_ignore_case(normalized, "bluray720p") {
        return Quality::Bluray720p;
    }
    if contains_ignore_case(normalized, "bluray1080p") {
        return Quality::Bluray1080p;
    }
    if contains_ignore_case(normalized, "bluray2160p") {
        return Quality::Bluray2160p;
    }

    match captures(&OTHER_SOURCE, normalized) {
        Some(c) if c.name("sdtv").is_some() => Quality::Sdtv,
        Some(c) if c.name("hdtv").is_some() => Quality::Hdtv720p,
        _ => Quality::Unknown,
    }
}

fn parse_resolution(name: &str) -> Resolution {
    let alternative = is_match(&ALTERNATIVE_RESOLUTION, name);
    let Some(found) = captures(&RESOLUTION, name) else {
        return if alternative {
            Resolution::R2160p
        } else {
            Resolution::Unknown
        };
    };
    let is = |group_name: &str| found.name(group_name).is_some();
    if is("R360p") {
        Resolution::R360p
    } else if is("R480p") {
        Resolution::R480p
    } else if is("R540p") {
        Resolution::R540p
    } else if is("R576p") {
        Resolution::R576p
    } else if is("R720p") {
        Resolution::R720p
    } else if is("R1080p") {
        Resolution::R1080p
    } else if is("R2160p") || alternative {
        Resolution::R2160p
    } else {
        Resolution::Unknown
    }
}

fn parse_revision(name: &str, normalized: &str) -> Revision {
    let mut revision = Revision::default();
    let version = captures(&VERSION, normalized).and_then(|c| {
        ["v1", "v2", "v3", "v4", "v5"]
            .iter()
            .find_map(|g| c.name(g).map(|m| m.as_str()))
            .and_then(|v| v.parse::<u8>().ok())
    });
    if let Some(version) = version {
        revision.version = version;
    }
    let bumped = version.map_or(2, |v| v.saturating_add(1));
    if is_match(&PROPER, normalized) {
        revision.version = bumped;
    }
    if is_match(&REPACK, normalized) {
        revision.version = bumped;
        revision.is_repack = true;
    }
    let real = REAL.find_iter(name).map_while(Result::ok).count();
    revision.real = u8::try_from(real).unwrap_or(u8::MAX);
    revision
}

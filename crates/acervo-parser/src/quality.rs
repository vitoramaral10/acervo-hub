//! Qualidade de um release: fonte, resolução e modificador, mais a revisão
//! (PROPER, REPACK, REAL).
//!
//! Porte do parser de qualidade do gerenciador de filmes que este projeto
//! substitui, com a mesma tabela e a mesma ordem de decisão: é ela que o
//! corpus confere.

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::common::{
    captures, group, is_match, last_captures, path_extension, quality_for_extension, regex,
};

/// De onde o vídeo veio. A ordem importa: é a da busca por qualidade vizinha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Unknown,
    Cam,
    Telesync,
    Telecine,
    Workprint,
    Dvd,
    Tv,
    WebDl,
    WebRip,
    Bluray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    None,
    Regional,
    Screener,
    RawHd,
    BrDisk,
    Remux,
}

/// As qualidades conhecidas, com o id e o nome que a API v3 usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    Unknown,
    Workprint,
    Cam,
    Telesync,
    Telecine,
    DvdScr,
    Regional,
    Sdtv,
    Dvd,
    DvdR,
    Hdtv720p,
    Hdtv1080p,
    Hdtv2160p,
    WebDl480p,
    WebDl720p,
    WebDl1080p,
    WebDl2160p,
    WebRip480p,
    WebRip720p,
    WebRip1080p,
    WebRip2160p,
    Bluray480p,
    Bluray576p,
    Bluray720p,
    Bluray1080p,
    Bluray2160p,
    Remux1080p,
    Remux2160p,
    BrDisk,
    RawHd,
}

impl Quality {
    /// Todas, na ordem da tabela de referência.
    pub const ALL: [Self; 30] = [
        Self::Unknown,
        Self::Workprint,
        Self::Cam,
        Self::Telesync,
        Self::Telecine,
        Self::DvdScr,
        Self::Regional,
        Self::Sdtv,
        Self::Dvd,
        Self::DvdR,
        Self::Hdtv720p,
        Self::Hdtv1080p,
        Self::Hdtv2160p,
        Self::WebDl480p,
        Self::WebDl720p,
        Self::WebDl1080p,
        Self::WebDl2160p,
        Self::WebRip480p,
        Self::WebRip720p,
        Self::WebRip1080p,
        Self::WebRip2160p,
        Self::Bluray480p,
        Self::Bluray576p,
        Self::Bluray720p,
        Self::Bluray1080p,
        Self::Bluray2160p,
        Self::Remux1080p,
        Self::Remux2160p,
        Self::BrDisk,
        Self::RawHd,
    ];

    /// Id, nome, fonte, resolução e modificador.
    const fn spec(self) -> (u8, &'static str, Source, u16, Modifier) {
        use Modifier as M;
        use Source as S;
        match self {
            Self::Unknown => (0, "Unknown", S::Unknown, 0, M::None),
            Self::Workprint => (24, "WORKPRINT", S::Workprint, 0, M::None),
            Self::Cam => (25, "CAM", S::Cam, 0, M::None),
            Self::Telesync => (26, "TELESYNC", S::Telesync, 0, M::None),
            Self::Telecine => (27, "TELECINE", S::Telecine, 0, M::None),
            Self::DvdScr => (28, "DVDSCR", S::Dvd, 480, M::Screener),
            Self::Regional => (29, "REGIONAL", S::Dvd, 480, M::Regional),
            Self::Sdtv => (1, "SDTV", S::Tv, 480, M::None),
            Self::Dvd => (2, "DVD", S::Dvd, 0, M::None),
            Self::DvdR => (23, "DVD-R", S::Dvd, 480, M::Remux),
            Self::Hdtv720p => (4, "HDTV-720p", S::Tv, 720, M::None),
            Self::Hdtv1080p => (9, "HDTV-1080p", S::Tv, 1080, M::None),
            Self::Hdtv2160p => (16, "HDTV-2160p", S::Tv, 2160, M::None),
            Self::WebDl480p => (8, "WEBDL-480p", S::WebDl, 480, M::None),
            Self::WebDl720p => (5, "WEBDL-720p", S::WebDl, 720, M::None),
            Self::WebDl1080p => (3, "WEBDL-1080p", S::WebDl, 1080, M::None),
            Self::WebDl2160p => (18, "WEBDL-2160p", S::WebDl, 2160, M::None),
            Self::WebRip480p => (12, "WEBRip-480p", S::WebRip, 480, M::None),
            Self::WebRip720p => (14, "WEBRip-720p", S::WebRip, 720, M::None),
            Self::WebRip1080p => (15, "WEBRip-1080p", S::WebRip, 1080, M::None),
            Self::WebRip2160p => (17, "WEBRip-2160p", S::WebRip, 2160, M::None),
            Self::Bluray480p => (20, "Bluray-480p", S::Bluray, 480, M::None),
            Self::Bluray576p => (21, "Bluray-576p", S::Bluray, 576, M::None),
            Self::Bluray720p => (6, "Bluray-720p", S::Bluray, 720, M::None),
            Self::Bluray1080p => (7, "Bluray-1080p", S::Bluray, 1080, M::None),
            Self::Bluray2160p => (19, "Bluray-2160p", S::Bluray, 2160, M::None),
            Self::Remux1080p => (30, "Remux-1080p", S::Bluray, 1080, M::Remux),
            Self::Remux2160p => (31, "Remux-2160p", S::Bluray, 2160, M::Remux),
            Self::BrDisk => (22, "BR-DISK", S::Bluray, 1080, M::BrDisk),
            Self::RawHd => (10, "Raw-HD", S::Tv, 1080, M::RawHd),
        }
    }

    #[must_use]
    pub const fn id(self) -> u8 {
        self.spec().0
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.spec().1
    }

    #[must_use]
    pub const fn source(self) -> Source {
        self.spec().2
    }

    #[must_use]
    pub const fn resolution(self) -> u16 {
        self.spec().3
    }

    #[must_use]
    pub const fn modifier(self) -> Modifier {
        self.spec().4
    }

    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|quality| quality.id() == id)
    }

    /// A qualidade de fonte, resolução e modificador dados; sem uma exata,
    /// a vizinha de fonte igual ou melhor.
    fn find(source: Source, resolution: u16, modifier: Modifier) -> Self {
        if let Some(exact) = Self::ALL.into_iter().find(|q| {
            q.source() == source && q.resolution() == resolution && q.modifier() == modifier
        }) {
            return exact;
        }
        let unknown_resolution: Vec<_> = Self::ALL
            .into_iter()
            .filter(|q| {
                q.source() == source
                    && q.resolution() == 0
                    && q.modifier() == modifier
                    && *q != Self::Unknown
            })
            .collect();
        if let [only] = unknown_resolution.as_slice() {
            return *only;
        }
        if let Some(quality) = unknown_resolution.iter().find(|q| q.source() >= source) {
            return *quality;
        }
        let mut same_resolution: Vec<_> = Self::ALL
            .into_iter()
            .filter(|q| q.modifier() == modifier && q.resolution() == resolution)
            .collect();
        same_resolution.sort_by_key(|q| q.source());
        same_resolution
            .into_iter()
            .find(|q| q.source() >= source)
            .unwrap_or(Self::Unknown)
    }
}

/// Revisão: versão 1 é o release original; PROPER e REPACK sobem a versão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Revision {
    pub version: u8,
    /// Quantas vezes "REAL" aparece, em maiúsculas.
    pub real: u8,
    pub is_repack: bool,
}

impl Default for Revision {
    fn default() -> Self {
        Self {
            version: 1,
            real: 0,
            is_repack: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityModel {
    pub quality: Quality,
    pub revision: Revision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    Unknown,
    R360p,
    R480p,
    R540p,
    R576p,
    R720p,
    R1080p,
    R2160p,
}

static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)\b(?:",
        r"(?<bluray>M?Blu[-_. ]?Ray|HD[-_. ]?DVD|BD(?!$)|UHD2?BD|BDISO|BDMux|BD25|BD50|BR[-_. ]?DISK)|",
        r"(?<webdl>WEB[-_. ]?DL(?:mux)?|AmazonHD|AmazonSD|iTunesHD|MaxdomeHD|NetflixU?HD|WebHD|HBOMaxHD|DisneyHD|[. ]WEB[. ](?:[xh][ .]?26[45]|AVC|HEVC|DDP?5[. ]1)|[. ](?-i:WEB)$|(?:\d{3,4}0p)[-. ](?:Hybrid[-_. ]?)?WEB[-. ]|[-. ]WEB[-. ]\d{3,4}0p|\b\s/\sWEB\s/\s\b|(?:AMZN|NF|DP)[. -]WEB[. -](?!Rip))|",
        r"(?<webrip>WebRip|Web-Rip|WEBMux)|",
        r"(?<hdtv>HDTV)|",
        r"(?<bdrip>BDRip|BDLight|HD[-_. ]?DVDRip|UHDBDRip)|",
        r"(?<brrip>BRRip)|",
        r"(?<dvdr>\d?x?M?DVD-?[R59])|",
        r"(?<dvd>DVD(?!-R)|DVDRip|xvidvd)|",
        r"(?<dsr>WS[-_. ]DSR|DSR)|",
        r"(?<regional>R[0-9]{1}|REGIONAL)|",
        r"(?<scr>SCR|SCREENER|DVDSCR|DVDSCREENER)|",
        r"(?<ts>TS[-_. ]|TELESYNCH?|HD-TS|HDTS|PDVD|TSRip|HDTSRip)|",
        r"(?<tc>TC|TELECINE|HD-TC|HDTC)|",
        r"(?<cam>CAMRIP|(?:NEW)?CAM|HD-?CAM(?:Rip)?|HQCAM)|",
        r"(?<wp>WORKPRINT|WP)|",
        r"(?<pdtv>PDTV)|",
        r"(?<sdtv>SDTV)|",
        r"(?<tvrip>TVRip)",
        r")(?:\b|$|[ .])",
    ))
});

static RAW_HD: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\b(?<rawhd>RawHD|Raw[-_. ]HD)\b"));

static MPEG2: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(?<mpeg2>MPEG[-_. ]?2)\b"));

static BR_DISK: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)^(?!.*\b((?<!HD[._ -]|HD)DVD|BDRip|720p|MKV|XviD|WMV|d3g|(BD)?REMUX|^(?=.*1080p)(?=.*HEVC)|[xh][-_. ]?26[45]|German.*[DM]L|((?<=\d{4}).*German.*([DM]L)?)(?=.*\b(AVC|HEVC|VC[-_. ]?1|MVC|MPEG[-_. ]?2)\b))\b)",
        r"(((?=.*\b(Blu[-_. ]?ray|BD|HD[-_. ]?DVD)\b)(?=.*\b(AVC|HEVC|VC[-_. ]?1|MVC|MPEG[-_. ]?2|BDMV|ISO)\b))",
        r"|^((?=.*\b(((?=.*\b((.*_)?COMPLETE.*|Dis[ck])\b)(?=.*(Blu[-_. ]?ray|HD[-_. ]?DVD)))|3D[-_. ]?BD|BR[-_. ]?DISK|Full[-_. ]?Blu[-_. ]?ray|^((?=.*((BD|UHD)[-_. ]?(25|50|66|100|ISO)))))))).*",
    ))
});

static PROPER: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\b(?<proper>proper)\b"));

static REPACK: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\b(?<repack>repack\d?|rerip\d?)\b"));

static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)\d[-._ ]?v(?<v1>\d)[-._ ]|\[v(?<v2>\d)\]|repack(?<v3>\d)|rerip(?<v4>\d)")
});

static REAL: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(?<real>REAL)\b"));

static RESOLUTION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?:(?<R360p>360p)|(?<R480p>480p|480i|640x480|848x480)|(?<R540p>540p)|(?<R576p>576p)|(?<R720p>720p|1280x720|960p)|(?<R1080p>1080p|1920x1080|1440p|FHD|1080i|4kto1080p)|(?<R2160p>2160p|3840x2160|4k[-_. ](?:UHD|HEVC|BD|H\.?265)|(?:UHD|HEVC|BD|H\.?265)[-_. ]4k))\b",
    )
});

static ALTERNATIVE_RESOLUTION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(?<a>UHD)\b|(?<b>\[4K\])"));

static CODEC: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?:(?<x264>x264)|(?<h264>h264)|(?<xvidhd>XvidHD)|(?<xvid>X-?vid)|(?<divx>divx))\b",
    )
});

static OTHER_SOURCE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)(?<hdtv>HD[-_. ]TV)|(?<sdtv>SD[-_. ]TV)"));

static ANIME_BLURAY: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)bd(?:720|1080|2160)|(?<=[-_. (\[])bd(?=[-_. )\]])"));

static ANIME_WEB_DL: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\[WEB\]|[\[\(]WEB[ .]"));

static HIGH_DEF_PDTV: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)hr[-_. ]ws"));

static REMUX: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?:[_. \[]|\d{4}p-|\bHybrid-)(?<remux>(?:(BD|UHD)[-_. ]?)?Remux)\b|(?<remux2>(?:(BD|UHD)[-_. ]?)?Remux[_. ]\d{4}p)",
    )
});

static GERMAN_REMUX: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)((?<=\d{4}).*German.*([DM]L)?)(?=.*\b(AVC|HEVC|VC[_. -]?1|MVC|MPEG[_. -]?2))(?=.*Blu-?ray)",
    )
});

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Qualidade de um nome de release ou de arquivo.
#[must_use]
pub fn parse_quality(name: &str) -> QualityModel {
    let name = name.trim();
    if name.is_empty() {
        return QualityModel {
            quality: Quality::Unknown,
            revision: Revision::default(),
        };
    }
    let mut result = parse_quality_name(name);
    if result.quality == Quality::Unknown && !name.contains('\0') {
        result.quality = quality_for_extension(path_extension(name));
    }
    result
}

/// Qualidade só pelo nome, sem olhar a extensão no fim.
#[must_use]
pub fn parse_quality_name(name: &str) -> QualityModel {
    let normalized = name.replace('_', " ");
    let normalized = normalized.trim();
    let revision = parse_revision(name, normalized);
    let quality = parse_quality_only(name, normalized);
    QualityModel { quality, revision }
}

#[allow(clippy::too_many_lines)]
fn parse_quality_only(name: &str, normalized: &str) -> Quality {
    let source = last_captures(&SOURCE, normalized);
    let resolution = parse_resolution(normalized);
    let codec = captures(&CODEC, normalized);
    let codec_is = |group_name: &str| codec.as_ref().is_some_and(|c| c.name(group_name).is_some());
    let remux = is_match(&REMUX, normalized) || is_match(&GERMAN_REMUX, normalized);
    let br_disk = is_match(&BR_DISK, normalized);

    if is_match(&RAW_HD, normalized) && !br_disk {
        return Quality::RawHd;
    }

    if let Some(source) = &source {
        let is = |group_name: &str| source.name(group_name).is_some();
        if is("bluray") {
            if br_disk {
                return Quality::BrDisk;
            }
            if codec_is("xvid") || codec_is("divx") {
                return Quality::Bluray480p;
            }
            return match resolution {
                Resolution::R2160p if remux => Quality::Remux2160p,
                Resolution::R2160p => Quality::Bluray2160p,
                // Remux sem resolução é 1080p, não 720p.
                Resolution::R1080p | Resolution::Unknown if remux => Quality::Remux1080p,
                Resolution::R1080p => Quality::Bluray1080p,
                Resolution::R720p | Resolution::Unknown => Quality::Bluray720p,
                Resolution::R576p => Quality::Bluray576p,
                Resolution::R360p | Resolution::R480p | Resolution::R540p => Quality::Bluray480p,
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
        if is("scr") {
            return Quality::DvdScr;
        }
        if is("cam") {
            return Quality::Cam;
        }
        if is("ts") {
            return Quality::Telesync;
        }
        if is("tc") {
            return Quality::Telecine;
        }
        if is("wp") {
            return Quality::Workprint;
        }
        if is("regional") {
            return Quality::Regional;
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
                Resolution::R576p => Quality::Bluray576p,
                _ => Quality::Bluray480p,
            };
        }
        if is("dvdr") {
            return Quality::DvdR;
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
        let (source, modifier) = if remux {
            (Source::Bluray, Modifier::Remux)
        } else {
            let by_extension = quality_for_extension(path_extension(name));
            if by_extension == Quality::Unknown {
                (Source::Unknown, Modifier::None)
            } else {
                (by_extension.source(), Modifier::None)
            }
        };
        let (fallback, pixels) = match resolution {
            Resolution::R2160p => (Quality::Hdtv2160p, 2160),
            Resolution::R1080p => (Quality::Hdtv1080p, 1080),
            Resolution::R720p => (Quality::Hdtv720p, 720),
            _ => (Quality::Sdtv, 480),
        };
        return if source == Source::Unknown {
            fallback
        } else {
            Quality::find(source, pixels, modifier)
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
    let found = captures(&RESOLUTION, name);
    let alternative = is_match(&ALTERNATIVE_RESOLUTION, name);
    let Some(found) = found else {
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
        ["v1", "v2", "v3", "v4"]
            .iter()
            .find_map(|g| group(&c, g))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todos_os_padroes_compilam() {
        for regex in [
            &*SOURCE,
            &*RAW_HD,
            &*MPEG2,
            &*BR_DISK,
            &*PROPER,
            &*REPACK,
            &*VERSION,
            &*REAL,
            &*RESOLUTION,
            &*ALTERNATIVE_RESOLUTION,
            &*CODEC,
            &*OTHER_SOURCE,
            &*ANIME_BLURAY,
            &*ANIME_WEB_DL,
            &*HIGH_DEF_PDTV,
            &*REMUX,
            &*GERMAN_REMUX,
        ] {
            let _ = regex.is_match("x");
        }
    }

    #[test]
    fn casos_de_referencia() {
        for (name, expected) in [
            (
                "Movie.2020.1080p.WEB-DL.DDP5.1.H.264-GRP",
                Quality::WebDl1080p,
            ),
            (
                "Movie.2020.2160p.WEB-DL.DV.HDR.H265-GRP",
                Quality::WebDl2160p,
            ),
            ("Movie.2020.1080p.BluRay.x264-GRP", Quality::Bluray1080p),
            (
                "Movie.2020.1080p.BluRay.REMUX.AVC.DTS-HD.MA-GRP",
                Quality::Remux1080p,
            ),
            ("Movie.2020.720p.WEBRip.x264-GRP", Quality::WebRip720p),
            ("Movie.2020.HDCAM.x264-GRP", Quality::Cam),
            ("Movie.2020.DVDRip.XviD-GRP", Quality::Dvd),
            ("Movie.2020.1080p.HDTV.x264-GRP", Quality::Hdtv1080p),
            ("Movie.2020.COMPLETE.BLURAY-GRP", Quality::BrDisk),
            ("Movie 2020 1080p", Quality::Hdtv1080p),
        ] {
            assert_eq!(parse_quality(name).quality, expected, "{name}");
        }
    }

    #[test]
    fn revisao() {
        let proper = parse_quality("Movie.2020.1080p.WEB-DL.PROPER-GRP").revision;
        assert_eq!(proper.version, 2);
        let repack = parse_quality("Movie.2020.1080p.WEB-DL.REPACK2-GRP").revision;
        assert_eq!((repack.version, repack.is_repack), (3, true));
        assert_eq!(parse_quality("Movie.REAL.REAL.2020.1080p").revision.real, 2);
    }
}

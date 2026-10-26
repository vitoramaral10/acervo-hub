//! O nome do release no padrão do Amigos (seção 16 das regras de 24/10/2026).
//!
//! O padrão vale mesmo que o nome original seja outro (16.1.4), e colocá-lo no
//! padrão é a única mudança permitida, sempre mantendo a tag original
//! (16.12.8). [`standard`] refaz o nome de um vídeo ou da pasta: título, ano
//! ou `SxxEyy`, edição, Hybrid, REPACK, fonte, sigla de streaming e tags vêm
//! do nome original, que é o que o arquivo não diz; resolução, áudio, HDR,
//! HFR, codec de vídeo e DUAL/MULTi vêm do ffprobe. O que o padrão pede e
//! nenhum dos dois diz (o ano, a fonte de um "WEB" sem codec) não é presumido
//! (16.1.1): vira lacuna, e o pacote fica inelegível.
//!
//! Função pura: quem decide onde o nome entra (vídeo, pasta, legenda) são as
//! regras e a pasta adaptada.

use acervo_parser::Language;
use unicode_normalization::UnicodeNormalization;

use crate::mediainfo::{AudioFacts, Hdr, MediaFacts, VideoFacts};

/// O nome no padrão, sem extensão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Standard {
    pub name: String,
    /// A tag do grupo original; `None` quando o nome não tinha, e o padrão
    /// leva `NoGroup` (16.1.3).
    pub group: Option<String>,
    /// O que não impede o envio, mas a staff pode questionar (16.7).
    pub warnings: Vec<String>,
}

/// O que se sabe do item a nomear.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Subject<'a> {
    /// O nome original, sem extensão.
    pub original: &'a str,
    pub media: &'a MediaFacts,
    pub original_language: Option<Language>,
    /// Série: o identificador é `SxxEyy`/`Sxx` e não há ano (16.5.1).
    pub series: bool,
    /// Áudio original + PT no arquivo (o DUAL das regras).
    pub dual: bool,
    /// Encode de Blu-ray abaixo do bitrate mínimo (15.2.5): a source é `BRRip`
    /// (16.6), qualquer que seja a do nome original.
    pub brrip: bool,
    /// O título original da obra em inglês, do catálogo (16.2.1: obra
    /// estrangeira usa o título em inglês do `IMDb`); `None` fora de obra em
    /// inglês.
    pub english_title: Option<&'a str>,
}

/// Só letras e dígitos, minúsculos: para comparar títulos sem pontuação.
fn squash(text: &str) -> String {
    clean_title(text)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// As palavras do título do catálogo no padrão: `Spider-Man: Across the
/// Spider-Verse` → `Spider-Man`, `Across`, `the`, `Spider-Verse`.
fn title_words(title: &str) -> Vec<String> {
    title
        .split(|c: char| c.is_whitespace() || c == '.')
        .map(clean_title)
        .filter(|t| !t.is_empty())
        .collect()
}

/// 16.7, na grafia da regra.
const SERVICES: [&str; 27] = [
    "AMZN", "ATVP", "ATV", "BAND", "BELA", "BP", "CTVP", "DSNP", "GLBO", "CR", "ADN", "iT", "FUNI",
    "B-Global", "HMAX", "HULU", "ITAU", "MPL", "NF", "PCOK", "PLTV", "PMTP", "RCRD", "TLBR",
    "VIKI", "MAX", "AO",
];

/// Siglas de streaming comuns que a lista do Amigos não traz: ficam no nome
/// (a sigla do release original é preservada, 16.6), com aviso.
const OTHER_SERVICES: [&str; 22] = [
    "STAN", "CRAV", "iP", "SHO", "HTSR", "JC", "ROKU", "TUBI", "MUBI", "CC", "AMC", "DCU", "SKST",
    "NOW", "BCORE", "CRKL", "VRV", "HBO", "DSNY", "APTN", "RED", "PCOCK",
];

/// Edições de 16.14, como aparecem no nome (minúsculas, em tokens) e como o
/// padrão as escreve.
const EDITIONS: [(&[&str], &str); 13] = [
    (&["extended", "cut"], "Extended.Cut"),
    (&["extended"], "Extended"),
    (&["directors", "cut"], "Directors.Cut"),
    (&["director's", "cut"], "Directors.Cut"),
    (&["theatrical", "cut"], "Theatrical.Cut"),
    (&["unrated"], "Unrated"),
    (&["open", "matte"], "Open.Matte"),
    (&["imax"], "IMAX"),
    (&["restored"], "Restored"),
    (&["remastered"], "Remastered"),
    (&["black", "and", "white"], "Black.and.White"),
    (&["black", "and", "chrome"], "Black.and.Chrome"),
    (&["colorized"], "COLORiZED"),
];

/// A fonte que o nome original declara.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    WebDl,
    WebRip,
    /// "WEB" sozinho: o codec do nome decide (H.264 é WEB-DL, x264 é `WEBRip`).
    Web,
    BluRay,
    BrRip,
    DvdRip,
    Tv(&'static str),
    /// Telesync, telecine, CAM: fora do padrão de nome.
    Cinema(&'static str),
}

fn source_of(token: &str) -> Option<Source> {
    Some(match token.to_ascii_lowercase().as_str() {
        "web-dl" | "webdl" => Source::WebDl,
        "webrip" | "web-rip" => Source::WebRip,
        "web" => Source::Web,
        "bluray" | "blu-ray" | "bdrip" | "bdremux" => Source::BluRay,
        "brrip" => Source::BrRip,
        "dvdrip" => Source::DvdRip,
        "hdtv" => Source::Tv("HDTV"),
        "pdtv" => Source::Tv("PDTV"),
        "tvrip" => Source::Tv("TVRip"),
        "cam" | "hdcam" => Source::Cinema("CAM"),
        "ts" | "hdts" | "telesync" => Source::Cinema("TS"),
        "tc" | "hdtc" | "telecine" => Source::Cinema("TC"),
        _ => return None,
    })
}

fn is_resolution(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "2160p" | "1080p" | "1080i" | "720p" | "576p" | "480p" | "4k"
    )
}

fn service_of(token: &str) -> Option<(&'static str, bool)> {
    SERVICES
        .iter()
        .find(|s| s.eq_ignore_ascii_case(token))
        .map(|s| (*s, true))
        .or_else(|| {
            OTHER_SERVICES
                .iter()
                .find(|s| s.eq_ignore_ascii_case(token))
                .map(|s| (*s, false))
        })
}

/// Ano entre 1900 e 2099, com ou sem parênteses.
fn year_of(token: &str) -> Option<&str> {
    let bare = token.trim_matches(['(', ')', '[', ']']);
    (bare.len() == 4
        && bare.bytes().all(|b| b.is_ascii_digit())
        && (bare.starts_with("19") || bare.starts_with("20")))
    .then_some(bare)
}

/// `S01`, `S01E02`, `S01E01-E02`, `S01E001`: a forma do padrão (maiúscula).
fn season_id(token: &str) -> Option<String> {
    let upper = token.to_ascii_uppercase();
    let rest = upper.strip_prefix('S')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits < 2 {
        return None;
    }
    let mut tail = &rest[digits..];
    while !tail.is_empty() {
        tail = tail.strip_prefix('-').unwrap_or(tail);
        let after = tail.strip_prefix('E')?;
        let n = after.bytes().take_while(u8::is_ascii_digit).count();
        if n < 2 {
            return None;
        }
        tail = &after[n..];
    }
    Some(upper)
}

fn is_episode_id(id: &str) -> bool {
    id.contains('E')
}

/// As tags do fim do nome: `-GRUPO`, ou `-GRUPO[.REPACK].DUAL-OUTRO` (16.12.7).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tags {
    first: Option<String>,
    /// A segunda tag, com o REPACK de quem a pôs (16.13.2).
    second: Option<(bool, String)>,
}

/// O que vem depois do último hífen e é parte de um termo técnico
/// (`WEB-DL`, `DTS-HD`, `VC-1`, `S01E01-E02`), não uma tag.
fn technical_suffix(before: &str, after: &str) -> bool {
    let before = before.to_ascii_lowercase();
    let after = after.to_ascii_lowercase();
    let pairs: [(&str, &[&str]); 7] = [
        ("web", &["dl", "rip"]),
        ("dts", &["hd", "x", "es"]),
        ("vc", &["1"]),
        ("blu", &["ray"]),
        ("ac", &["3"]),
        ("mpeg", &["2", "4"]),
        ("b", &["global"]),
    ];
    pairs
        .iter()
        .any(|(end, nexts)| before.ends_with(end) && nexts.contains(&after.as_str()))
        || (after.starts_with('e')
            && after.len() > 1
            && after[1..].bytes().all(|b| b.is_ascii_digit()))
}

/// Uma tag de verdade: sem ponto nem espaço, não vazia.
fn tag_at_end(text: &str) -> Option<(&str, &str)> {
    let (body, tag) = text.rsplit_once('-')?;
    let valid = !tag.is_empty()
        && !body.is_empty()
        && !tag.contains(['.', ' ', '_'])
        && !technical_suffix(body, tag);
    valid.then_some((body, tag))
}

fn split_tags(stem: &str) -> (&str, Tags) {
    let Some((body, last)) = tag_at_end(stem) else {
        return (
            stem,
            Tags {
                first: None,
                second: None,
            },
        );
    };
    let lower = body.to_ascii_lowercase();
    for lang in [".dual", ".multi"] {
        if let Some(cut) = lower.strip_suffix(lang).map(str::len) {
            let rest = &body[..cut];
            let (rest, repack) = match rest.len().checked_sub(".repack".len()) {
                Some(at) if rest[at..].eq_ignore_ascii_case(".repack") => (&rest[..at], true),
                _ => (rest, false),
            };
            if let Some((inner, first)) = tag_at_end(rest) {
                return (
                    inner,
                    Tags {
                        first: Some(first.to_owned()),
                        second: Some((repack, last.to_owned())),
                    },
                );
            }
        }
    }
    (
        body,
        Tags {
            first: Some(last.to_owned()),
            second: None,
        },
    )
}

/// Sem acento, sem os caracteres que 16.2.4 manda tirar, `&` vira `and`.
fn clean_title(token: &str) -> String {
    token
        .replace('&', "and")
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .filter(|c| !",:;!?*\"'()/\\|<>[]{}".contains(*c))
        .collect()
}

/// O que o nome original afirma depois do identificador.
// Um registro do que o nome afirma, campo a campo: não é máquina de estados.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
struct Claims {
    editions: Vec<&'static str>,
    hybrid: bool,
    repack: Option<String>,
    service: Option<(&'static str, bool)>,
    source: Option<Source>,
    remux: bool,
    uhd: bool,
    atmos: bool,
    dolby_vision: bool,
    hdr: Option<&'static str>,
    /// `x264`/`x265` no nome: alguém reencodou.
    x_codec: bool,
    /// `H.264`/`H265`/`AVC`/`HEVC` no nome.
    h_codec: bool,
    /// O que sobrou entre o identificador e a parte técnica (nome do episódio).
    loose: Vec<String>,
    /// O que a parte técnica traz e o acervo não reconhece. Pode ser a tag de
    /// um grupo fora do lugar (`x264 FLAME DUAL-OUTRO`): descartar seria retag.
    unknown: Vec<String>,
}

/// Tags de quem acrescenta o áudio PT ao release de outro grupo (a segunda
/// tag de 16.12.7), vistas nos releases de 10/2026. Como única tag, o grupo
/// original foi apagado. O `PiA` também aparece como segunda tag, mas a staff
/// aceitou um release dele como única tag (Let It Snow), então fica fora.
const DUAL_ADDERS: [&str; 24] = [
    "shaggy",
    "shagyy",
    "RK",
    "GMN",
    "Lannister",
    "3LT0N",
    "xprince00",
    "C76",
    "alfaHD",
    "SynysteR",
    "ASM",
    "GoT",
    "KABAL",
    "LAPUMiA",
    "THYRSO10",
    "wastaken",
    "GUR",
    "Hiro360",
    "JK",
    "FAMG",
    "DSB",
    "SpartacusBR",
    "Tontom",
    "3LTON",
];

/// Termos técnicos que o nome original pode trazer e o nome no padrão refaz
/// a partir do arquivo, sem os dígitos de canais colados (`DDP5`, `AAC2`).
const TECHNICAL: [&str; 62] = [
    "dd",
    "dd+",
    "ddp",
    "ddpa",
    "eac3",
    "e-ac-3",
    "ac3",
    "aac",
    "dts",
    "dts-hd",
    "dts-hdma",
    "dts-hdm",
    "dts-x",
    "dts-es",
    "ma",
    "hra",
    "truehd",
    "flac",
    "opus",
    "lpcm",
    "pcm",
    "mp3",
    "ch",
    "xvid",
    "vc-1",
    "mpeg-2",
    "av1",
    "10bit",
    "8bit",
    "hdr10plus",
    "dual-audio",
    "audio",
    "dual",
    "multi",
    "dublado",
    "legendado",
    "nacional",
    "pt-br",
    "ptbr",
    "internal",
    "limited",
    "rerip",
    "h",
    "-",
    "french",
    "english",
    "spanish",
    "portuguese",
    "italian",
    "german",
    "japanese",
    "korean",
    "latino",
    "castellano",
    "ita",
    "fre",
    "spa",
    "ger",
    "jpn",
    "subs",
    "subbed",
    "dubbed",
];

/// Termo que a parte técnica pode ter sem perder informação no padrão.
fn is_technical(word: &str) -> bool {
    let bare = word.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    bare.is_empty() || TECHNICAL.contains(&bare) || NOISE.contains(&word)
}

/// Termos que não são nome de episódio e o padrão não usa.
const NOISE: [&str; 22] = [
    "dual",
    "multi",
    "dub",
    "dublado",
    "legendado",
    "leg",
    "nacional",
    "pt-br",
    "ptbr",
    "eng",
    "por",
    "internal",
    "limited",
    "complete",
    "10bit",
    "8bit",
    "hevc",
    "avc",
    "proper",
    "repack",
    "rerip",
    "readnfo",
];

impl Claims {
    #[allow(clippy::too_many_lines)]
    fn read(tokens: &[&str], technical_from: usize) -> Self {
        let mut claims = Self::default();
        let lower: Vec<String> = tokens.iter().map(|t| t.to_ascii_lowercase()).collect();
        let mut i = 0;
        'tokens: while i < tokens.len() {
            for (words, label) in EDITIONS {
                if lower[i..].len() >= words.len()
                    && words.iter().zip(&lower[i..]).all(|(w, t)| w == t)
                {
                    if !claims.editions.contains(&label) {
                        claims.editions.push(label);
                    }
                    i += words.len();
                    continue 'tokens;
                }
            }
            let token = tokens[i];
            let word = lower[i].as_str();
            let next = lower.get(i + 1).map(String::as_str);
            let technical = i >= technical_from;
            match word {
                "hybrid" => claims.hybrid = true,
                "repack" | "proper" | "repack2" | "repack3" | "proper2" => {
                    claims.repack = Some(token.to_ascii_uppercase());
                }
                "remux" => claims.remux = true,
                "uhd" => claims.uhd = true,
                "atmos" => claims.atmos = true,
                "dv" | "dovi" => claims.dolby_vision = true,
                "hdr" => claims.hdr = Some("HDR"),
                "hdr10" => claims.hdr = Some("HDR10"),
                "hdr10+" => claims.hdr = Some("HDR10+"),
                "hdr10p" => claims.hdr = Some("HDR10P"),
                "x264" | "x265" => claims.x_codec = true,
                "h264" | "h265" | "avc" | "hevc" => claims.h_codec = true,
                "h" if matches!(next, Some("264" | "265")) => {
                    claims.h_codec = true;
                    i += 2;
                    continue;
                }
                _ => {
                    if let Some(source) = source_of(word) {
                        claims.source.get_or_insert(source);
                    } else if is_resolution(word) {
                        // A resolução sai do arquivo (16.10).
                    } else if let Some(service) = service_of(word).filter(|_| {
                        // Antes da parte técnica, "Max" pode ser o nome do
                        // episódio: só conta colado na fonte.
                        technical || next.is_some_and(|n| source_of(n).is_some())
                    }) {
                        claims.service.get_or_insert(service);
                    } else if !technical && !NOISE.contains(&word) {
                        claims.loose.push(token.to_owned());
                    } else if technical && !is_technical(word) {
                        claims.unknown.push(token.to_owned());
                    }
                }
            }
            i += 1;
        }
        claims
    }
}

/// A classe de source que decide a ordem dos campos (16.4) e o codec (16.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    WebDl,
    WebRip,
    Remux,
    Encode,
    Tv,
}

/// A resolução pela classe do vídeo (16.10): corte e barra preta não mudam.
fn resolution(video: &VideoFacts, dvd: bool) -> &'static str {
    let (w, h) = (video.width, video.height);
    if w >= 2800 || h >= 1900 {
        "2160p"
    } else if w >= 1400 || h >= 1000 {
        "1080p"
    } else if w >= 950 || h >= 700 {
        "720p"
    } else if dvd && h > 480 {
        "576p"
    } else {
        "480p"
    }
}

/// O codec de vídeo pela classe da source, não pela Writing library (16.8).
fn video_codec(codec: &str, class: Class) -> Result<&'static str, String> {
    Ok(match (codec, class) {
        ("h264", Class::Remux) => "AVC",
        ("h264", Class::WebDl | Class::Tv) => "H.264",
        ("h264", Class::WebRip | Class::Encode) => "x264",
        ("hevc", Class::Remux) => "HEVC",
        ("hevc", Class::WebDl | Class::Tv) => "H.265",
        ("hevc", Class::WebRip | Class::Encode) => "x265",
        ("av1", Class::WebDl) => "AV1",
        ("av1", _) => return Err("encode em AV1 não é aceito (15.2.4)".to_owned()),
        ("vc1", Class::Remux) => "VC-1",
        ("mpeg4", Class::Encode) => "Xvid",
        (other, _) => {
            return Err(format!(
                "o codec de vídeo {other} não tem grafia no padrão para esta source (16.8)"
            ));
        }
    })
}

/// Título de faixa de comentário: nunca entra no nome (16.12.1).
fn is_commentary(track: &AudioFacts) -> bool {
    track.title.as_deref().is_some_and(|t| {
        let t = t.to_lowercase();
        t.contains("coment") || t.contains("comment")
    })
}

fn portuguese(language: Language) -> bool {
    matches!(language, Language::Portuguese | Language::PortugueseBr)
}

/// A faixa que dá o áudio do nome: a do idioma original (a PT-BR na obra
/// nacional), sem comentário; entre várias, a de mais canais (16.12.1).
fn name_track(audio: &[AudioFacts], original: Option<Language>) -> Option<&AudioFacts> {
    let tracks: Vec<&AudioFacts> = audio.iter().filter(|a| !is_commentary(a)).collect();
    let wanted = original.map(|o| {
        if portuguese(o) {
            Language::Portuguese
        } else {
            o
        }
    });
    let preferred: Vec<&AudioFacts> = tracks
        .iter()
        .copied()
        .filter(|a| {
            wanted.is_some_and(|w| a.language == w || (portuguese(w) && portuguese(a.language)))
        })
        .collect();
    let pool = if preferred.is_empty() {
        // Sem a faixa original marcada: a primeira que não é a dublagem.
        let foreign: Vec<&AudioFacts> = tracks
            .iter()
            .copied()
            .filter(|a| !portuguese(a.language))
            .collect();
        if foreign.is_empty() { tracks } else { foreign }
    } else {
        preferred
    };
    pool.iter()
        .copied()
        .reduce(|best, a| if a.channels > best.channels { a } else { best })
}

/// `DDP5.1`, `TrueHD7.1.Atmos`, `DTS-HD.MA5.1` (16.12.1 e 16.12.2).
fn audio_label(track: &AudioFacts, original_name: &str, claims: &Claims) -> Result<String, String> {
    let profile = track
        .profile
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let title = track
        .title
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = original_name.to_ascii_lowercase();
    let (codec, can_atmos) = match track.codec.as_str() {
        "ac3" => ("DD", false),
        "eac3" => ("DDP", true),
        "truehd" | "mlp" => ("TrueHD", true),
        "dts" => {
            let said = |t: &str| profile.contains(t) || (profile.is_empty() && name.contains(t));
            (
                if said("dts:x") || said("dts-x") {
                    "DTS-X"
                } else if said("ma") {
                    "DTS-HD.MA"
                } else if said("hra") || said("96/24") {
                    "DTS-HD.HRA"
                } else if said("es") {
                    "DTS-ES"
                } else {
                    "DTS"
                },
                false,
            )
        }
        "aac" => ("AAC", false),
        "opus" => ("Opus", false),
        "flac" => ("FLAC", false),
        "mp3" => ("MP3.", false),
        pcm if pcm.starts_with("pcm_") => ("LPCM", false),
        other => {
            return Err(format!(
                "o codec de áudio {other} não tem grafia no padrão (16.12.1)"
            ));
        }
    };
    let channels = match track.channels {
        Some(1) => "1.0",
        Some(2) => "2.0",
        Some(3) => "2.1",
        Some(4) => "4.0",
        Some(5) => "5.0",
        Some(6) => "5.1",
        Some(7) => "6.1",
        Some(8) => "7.1",
        _ => return Err("o ffprobe não disse os canais do áudio (16.12.2)".to_owned()),
    };
    let atmos = can_atmos
        && (claims.atmos
            || profile.contains("atmos")
            || profile.contains("joc")
            || title.contains("atmos"));
    Ok(format!(
        "{codec}{channels}{}",
        if atmos { ".Atmos" } else { "" }
    ))
}

/// HDR do ffprobe na grafia que o nome original escolheu, quando ela cabe
/// (16.9): HDR10+ não é presumido, mas é mantido se o release o diz.
fn hdr_label(hdr: Hdr, claims: &Claims) -> Option<String> {
    match hdr {
        Hdr::Sdr => None,
        Hdr::Hdr10 => Some(claims.hdr.unwrap_or("HDR").to_owned()),
        Hdr::Hlg => Some("HLG".to_owned()),
        Hdr::DolbyVision => Some(match claims.hdr {
            Some(h) => format!("DV.{h}"),
            None => "DV".to_owned(),
        }),
    }
}

/// DUAL ou `MULTi` pelos idiomas do áudio (16.12.3 a 16.12.6): conta idioma,
/// não faixa, e obra nacional não leva.
fn language_tag(subject: &Subject<'_>) -> Option<&'static str> {
    if !subject.dual || subject.original_language.is_some_and(portuguese) {
        return None;
    }
    let mut languages: Vec<Language> = Vec::new();
    for track in subject.media.audio.iter().filter(|a| !is_commentary(a)) {
        let language = if portuguese(track.language) {
            Language::Portuguese
        } else {
            track.language
        };
        if language != Language::Unknown && !languages.contains(&language) {
            languages.push(language);
        }
    }
    Some(if languages.len() >= 3 {
        "MULTi"
    } else {
        "DUAL"
    })
}

/// O nome no padrão, ou as lacunas que impedem montá-lo.
///
/// # Errors
///
/// O que o padrão exige e nem o nome nem o arquivo dizem: ano (filme),
/// `SxxEyy` (série), fonte, vídeo sem sonda, codec sem grafia no padrão.
#[allow(clippy::too_many_lines)]
pub(crate) fn standard(subject: &Subject<'_>) -> Result<Standard, Vec<String>> {
    let mut gaps = Vec::new();
    let mut warnings = Vec::new();
    let (body, tags) = split_tags(subject.original.trim());
    let tokens: Vec<&str> = body
        .split(['.', ' ', '_'])
        .filter(|t| !t.is_empty())
        .collect();
    let first_resolution = tokens
        .iter()
        .skip(1)
        .position(|t| is_resolution(t))
        .map_or(tokens.len(), |p| p + 1);
    let id_at = if subject.series {
        tokens.iter().position(|t| season_id(t).is_some())
    } else {
        tokens[..first_resolution]
            .iter()
            .rposition(|t| year_of(t).is_some())
            .filter(|at| *at > 0)
    };
    let Some(id_at) = id_at else {
        return Err(vec![if subject.series {
            "o nome não tem a temporada e o episódio (S01E01), que o padrão exige (16.5)".to_owned()
        } else {
            "o nome não tem o ano, que o padrão exige (16.2.2)".to_owned()
        }]);
    };
    let id = if subject.series {
        season_id(tokens[id_at]).unwrap_or_default()
    } else {
        year_of(tokens[id_at]).unwrap_or_default().to_owned()
    };
    let rest = &tokens[id_at + 1..];
    let technical_from = rest
        .iter()
        .position(|t| is_resolution(t) || source_of(t).is_some())
        .unwrap_or(rest.len());
    let claims = Claims::read(rest, technical_from);
    if !claims.unknown.is_empty() {
        gaps.push(format!(
            "o nome tem na parte técnica o que o acervo não reconhece ({}): pode ser a tag de um grupo, e renomear a perderia (retag, 15.2.26)",
            claims.unknown.join(", ")
        ));
    }
    for tag in tags
        .first
        .iter()
        .chain(tags.second.iter().map(|(_, tag)| tag))
        .filter(|tag| tag.contains('@'))
    {
        gaps.push(format!(
            "a tag {tag} cita outro site, o que o Amigos proíbe (1.4 e 15.1.8)"
        ));
    }
    // Quem só acrescenta o áudio PT a um release de outro grupo e assina
    // sozinho apagou a tag original: relançar com ela é retag (15.2.26),
    // e a staff bane (Big Hero, `x264-shaggy`, 26/10).
    if tags.second.is_none()
        && let Some(tag) = tags
            .first
            .as_deref()
            .filter(|tag| DUAL_ADDERS.iter().any(|a| a.eq_ignore_ascii_case(tag)))
    {
        gaps.push(format!(
            "a única tag é {tag}, que só acrescenta o áudio PT ao release de outro grupo: a tag original sumiu, e relançar assim é retag (15.2.26)"
        ));
    }

    let Some(video) = subject.media.video.as_ref() else {
        return Err(vec![
            "sem a sonda do vídeo não há como montar o nome".to_owned(),
        ]);
    };
    let class = match (claims.source, claims.remux) {
        (Some(Source::BluRay) | None, true) => Some(Class::Remux),
        (Some(Source::WebDl), _) => Some(Class::WebDl),
        (Some(Source::WebRip), _) => Some(Class::WebRip),
        (Some(Source::Web), _) if claims.x_codec => Some(Class::WebRip),
        (Some(Source::Web), _) if claims.h_codec => Some(Class::WebDl),
        (Some(Source::Web), _) => {
            gaps.push(
                "o nome diz só WEB, sem H.264 ou x264: não dá para saber se é WEB-DL ou WEBRip (16.6)"
                    .to_owned(),
            );
            None
        }
        (Some(Source::BluRay | Source::BrRip | Source::DvdRip), _) => Some(Class::Encode),
        (Some(Source::Tv(_)), _) => Some(Class::Tv),
        (Some(Source::Cinema(kind)), _) => {
            gaps.push(format!(
                "a source {kind} (cinema) não tem lugar no padrão de nome do acervo"
            ));
            None
        }
        (None, false) => {
            gaps.push(
                "o nome não diz a source (WEB-DL, WEBRip, BluRay, HDTV...), e o padrão não a presume (16.6)"
                    .to_owned(),
            );
            None
        }
    };
    let audio = name_track(&subject.media.audio, subject.original_language)
        .ok_or_else(|| "o vídeo não tem faixa de áudio para o nome (16.12.1)".to_owned())
        .and_then(|track| audio_label(track, subject.original, &claims));
    let audio = audio.map_err(|gap| gaps.push(gap)).ok();
    let codec = class.and_then(|class| {
        video_codec(&video.codec, class)
            .map_err(|gap| gaps.push(gap))
            .ok()
    });
    let language = language_tag(subject);
    if let Some((_, second)) = &tags.second
        && language.is_none()
    {
        gaps.push(format!(
            "o nome tem a segunda tag ({second}) de quem acrescentou o áudio PT, mas o arquivo não tem áudio original e PT (16.12.7)"
        ));
    }
    let (Some(class), Some(audio), Some(codec)) = (class, audio, codec) else {
        return Err(gaps);
    };
    if !gaps.is_empty() {
        return Err(gaps);
    }

    let mut parts: Vec<String> = tokens[..id_at]
        .iter()
        .map(|t| clean_title(t))
        .filter(|t| !t.is_empty())
        .collect();
    // 16.2.1: obra estrangeira leva o título em inglês. O do release fica
    // quando já é ele (preserva a escolha do hífen) ou, na série, quando
    // começa por ele (o ano ou o país que separa séries de mesmo nome).
    if let Some(english) = subject.english_title.filter(|t| !squash(t).is_empty()) {
        let ours = squash(&parts.join("."));
        let theirs = squash(english);
        let keep = ours == theirs || (subject.series && ours.starts_with(&theirs));
        if !keep {
            parts = title_words(english);
        }
    } else if subject
        .original_language
        .is_some_and(|l| !portuguese(l) && l != Language::English && l != Language::Unknown)
    {
        warnings.push(
            "obra estrangeira fora do inglês: confira se o título do nome é o em inglês do IMDb (16.2.1)"
                .to_owned(),
        );
    }
    parts.push(id.clone());
    if subject.series && is_episode_id(&id) {
        parts.extend(
            claims
                .loose
                .iter()
                .map(|t| clean_title(t))
                .filter(|t| !t.is_empty()),
        );
    }
    parts.extend(claims.editions.iter().map(|e| (*e).to_owned()));
    if claims.hybrid && class != Class::Tv {
        parts.push("Hybrid".to_owned());
    }
    if let Some(repack) = &claims.repack {
        parts.push(repack.clone());
    }
    let dvd = claims.source == Some(Source::DvdRip);
    parts.push(resolution(video, dvd).to_owned());
    let hdr = hdr_label(video.hdr, &claims);
    let hfr = video
        .frame_rate
        .filter(|fps| *fps >= 49.5)
        .map(|_| "HFR".to_owned());
    let bluray = if claims.uhd { "UHD.BluRay" } else { "BluRay" };
    match class {
        Class::Remux => {
            parts.extend([bluray.to_owned(), "REMUX".to_owned()]);
            parts.extend(hdr);
            parts.extend(hfr);
            parts.extend([codec.to_owned(), audio]);
        }
        Class::Encode => {
            parts.push(match claims.source {
                Some(Source::BluRay) if subject.brrip => "BRRip".to_owned(),
                Some(Source::BrRip) => "BRRip".to_owned(),
                Some(Source::DvdRip) => "DVDRip".to_owned(),
                _ => bluray.to_owned(),
            });
            parts.push(audio);
            parts.extend(hdr);
            parts.extend(hfr);
            parts.push(codec.to_owned());
        }
        Class::WebDl | Class::WebRip => {
            if let Some((service, listed)) = claims.service {
                if !listed {
                    warnings.push(format!(
                        "a sigla de streaming {service} não está na lista do Amigos: consulte a staff (16.7)"
                    ));
                }
                parts.push(service.to_owned());
            }
            parts.push(
                if class == Class::WebDl {
                    "WEB-DL"
                } else {
                    "WEBRip"
                }
                .to_owned(),
            );
            parts.push(audio);
            parts.extend(hdr);
            parts.extend(hfr);
            parts.push(codec.to_owned());
        }
        Class::Tv => {
            let Some(Source::Tv(label)) = claims.source else {
                unreachable!("Class::Tv só sai de Source::Tv");
            };
            parts.extend([label.to_owned(), audio]);
            // Captura de TV costuma vir com x264 de quem gravou; sem isso, H.264.
            parts.push(if claims.x_codec {
                codec.replace("H.26", "x26")
            } else {
                codec.to_owned()
            });
        }
    }
    let body = parts.join(".");
    let name = match (&tags.first, &tags.second, language) {
        (Some(first), Some((repack, second)), Some(language)) => format!(
            "{body}-{first}{}.{language}-{second}",
            if *repack { ".REPACK" } else { "" }
        ),
        (first, _, language) => format!(
            "{body}{}-{}",
            language.map(|l| format!(".{l}")).unwrap_or_default(),
            first.as_deref().unwrap_or("NoGroup")
        ),
    };
    Ok(Standard {
        name,
        group: tags.first.clone(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mediainfo::AudioFacts;

    fn video(codec: &str, width: u32, height: u32, hdr: Hdr) -> VideoFacts {
        VideoFacts {
            codec: codec.to_owned(),
            width,
            height,
            pix_fmt: None,
            bit_depth: None,
            hdr,
            frame_rate: Some(23.976),
            bit_rate: None,
        }
    }

    fn track(language: Language, codec: &str, channels: u32) -> AudioFacts {
        AudioFacts {
            language,
            raw_language: None,
            codec: codec.to_owned(),
            channels: Some(channels),
            title: None,
            profile: None,
        }
    }

    fn media(video: VideoFacts, audio: Vec<AudioFacts>) -> MediaFacts {
        MediaFacts {
            container: "matroska,webm".to_owned(),
            duration_seconds: Some(6000.0),
            video: Some(video),
            audio,
            subtitles: Vec::new(),
        }
    }

    fn name(
        original: &str,
        media: &MediaFacts,
        series: bool,
        dual: bool,
    ) -> Result<String, Vec<String>> {
        standard(&Subject {
            original,
            media,
            original_language: Some(Language::English),
            series,
            dual,
            brrip: false,
            english_title: None,
        })
        .map(|s| s.name)
    }

    fn web_1080() -> MediaFacts {
        media(
            video("h264", 1920, 1080, Hdr::Sdr),
            vec![track(Language::English, "eac3", 6)],
        )
    }

    #[test]
    fn nome_ja_no_padrao_fica_igual() {
        let original = "Past.Lives.2023.1080p.AMZN.WEB-DL.DDP5.1.H.264-ASC";
        assert_eq!(name(original, &web_1080(), false, false).unwrap(), original);
        let dual = media(
            video("hevc", 3840, 2160, Hdr::DolbyVision),
            vec![
                AudioFacts {
                    profile: Some("Dolby Digital Plus + Dolby Atmos".to_owned()),
                    ..track(Language::English, "eac3", 6)
                },
                track(Language::Portuguese, "eac3", 6),
            ],
        );
        let original = "Inside.Out.2.2024.2160p.DSNP.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265.DUAL-ASC";
        assert_eq!(name(original, &dual, false, true).unwrap(), original);
    }

    #[test]
    fn poe_no_padrao_o_que_o_arquivo_diz() {
        // Toy Story recusado em 18/10: X265 maiúsculo e sem áudio no nome.
        let toy = media(
            video("hevc", 3840, 2160, Hdr::Hdr10),
            vec![
                track(Language::English, "truehd", 8),
                track(Language::Portuguese, "ac3", 6),
            ],
        );
        assert_eq!(
            name(
                "Toy.Story.1995.2160p.UHD.BluRay.X265-IAMABLE.DUAL-Lannister",
                &toy,
                false,
                true
            )
            .unwrap(),
            "Toy.Story.1995.2160p.UHD.BluRay.TrueHD7.1.HDR.x265-IAMABLE.DUAL-Lannister"
        );
        // Ordem trocada, espaço, acento, ano entre parênteses e lixo no meio.
        assert_eq!(
            name(
                "Coração Valente (1995) 1080p FRENCH WEBRip x264 AAC-GRP",
                &media(
                    video("h264", 1920, 800, Hdr::Sdr),
                    vec![track(Language::English, "aac", 2)]
                ),
                false,
                false
            )
            .unwrap(),
            "Coracao.Valente.1995.1080p.WEBRip.AAC2.0.x264-GRP"
        );
    }

    #[test]
    fn remux_poe_o_codec_antes_do_audio() {
        let remux = media(
            video("hevc", 3840, 2160, Hdr::DolbyVision),
            vec![
                AudioFacts {
                    profile: Some("Dolby TrueHD + Dolby Atmos".to_owned()),
                    ..track(Language::English, "truehd", 8)
                },
                track(Language::Portuguese, "ac3", 6),
            ],
        );
        assert_eq!(
            name(
                "The.Matrix.1999.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.TrueHD7.1.Atmos-ASC.DUAL-SuaTag",
                &remux,
                false,
                true
            )
            .unwrap(),
            "The.Matrix.1999.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.TrueHD7.1.Atmos-ASC.DUAL-SuaTag"
        );
    }

    #[test]
    fn serie_episodio_e_temporada() {
        assert_eq!(
            name(
                "the.bear.s03e01.1080p.dsnp.web-dl.ddp5.1.h264-ASC",
                &web_1080(),
                true,
                false
            )
            .unwrap(),
            "the.bear.S03E01.1080p.DSNP.WEB-DL.DDP5.1.H.264-ASC"
        );
        assert_eq!(
            name(
                "The.Bear.S03.1080p.WEB.h264-ETHEL",
                &web_1080(),
                true,
                false
            )
            .unwrap(),
            "The.Bear.S03.1080p.WEB-DL.DDP5.1.H.264-ETHEL"
        );
        // Nome do episódio fica logo depois do identificador.
        assert_eq!(
            name(
                "Lost.S01E01-E02.Pilot.720p.HDTV.x264-LOL",
                &media(
                    video("h264", 1280, 720, Hdr::Sdr),
                    vec![track(Language::English, "ac3", 6)]
                ),
                true,
                false
            )
            .unwrap(),
            "Lost.S01E01-E02.Pilot.720p.HDTV.DD5.1.x264-LOL"
        );
    }

    #[test]
    fn sem_grupo_vira_nogroup_e_dual_entra() {
        let dual = media(
            video("h264", 1920, 1080, Hdr::Sdr),
            vec![
                track(Language::English, "eac3", 6),
                track(Language::Portuguese, "eac3", 2),
                track(Language::Spanish, "eac3", 2),
            ],
        );
        let standard = standard(&Subject {
            original: "Filme.2020.1080p.NF.WEB-DL",
            media: &dual,
            original_language: Some(Language::English),
            series: false,
            dual: true,
            brrip: false,
            english_title: None,
        })
        .unwrap();
        assert_eq!(
            standard.name,
            "Filme.2020.1080p.NF.WEB-DL.DDP5.1.H.264.MULTi-NoGroup"
        );
        assert_eq!(standard.group, None);
    }

    #[test]
    fn lacunas_que_o_padrao_nao_presume() {
        let gaps = name("Filme.1080p.WEB-DL.H.264-GRP", &web_1080(), false, false).unwrap_err();
        assert!(gaps[0].contains("ano"));
        let gaps = name("Filme.2020.1080p.WEB.DDP5.1-GRP", &web_1080(), false, false).unwrap_err();
        assert!(gaps[0].contains("WEB-DL ou WEBRip"));
        let gaps = name(
            "Filme.2020.1080p.DDP5.1.H.264-GRP",
            &web_1080(),
            false,
            false,
        )
        .unwrap_err();
        assert!(gaps[0].contains("source"));
        let av1 = media(
            video("av1", 1920, 1080, Hdr::Sdr),
            vec![track(Language::English, "opus", 2)],
        );
        let gaps = name("Filme.2020.1080p.WEBRip.x265-GRP", &av1, false, false).unwrap_err();
        assert!(gaps[0].contains("AV1"));
        let gaps = name(
            "Filme.2020.1080p.WEB-DL.H.264-A.DUAL-B",
            &web_1080(),
            false,
            false,
        )
        .unwrap_err();
        assert!(gaps[0].contains("segunda tag"));
    }

    #[test]
    fn tag_fora_do_lugar_nao_some() {
        let media = media(
            video("h264", 1920, 1080, Hdr::Sdr),
            vec![
                track(Language::English, "dts", 6),
                track(Language::Portuguese, "ac3", 6),
            ],
        );
        for original in [
            "Covil de Ladroes (2018) 1080p BluRay x264 FLAME DUAL-SynysteR",
            "Irresistible.2020.1080p.BluRay.DTS.x264-iFT.DUAL.THYRSO10",
            "Nefarious.2023.BluRay.1080p.x264.Dual - By Tontom",
        ] {
            let gaps = name(original, &media, false, true).unwrap_err();
            assert!(gaps[0].contains("retag"), "{original}: {gaps:?}");
        }
        let gaps = name(
            "Os.Descendentes.2011.1080p.BluRay.DTS.x264-EPiC@HDArea.DUAL-GMN",
            &media,
            false,
            true,
        )
        .unwrap_err();
        assert!(gaps[0].contains("outro site"), "{gaps:?}");
        // O que é só técnico continua indo para o padrão.
        assert_eq!(
            name(
                "Ted.2012.1080p.BluRay.DTS-HD.MA.5.1.10bit.x264-DAA.DUAL-3LT0N",
                &media,
                false,
                true
            )
            .unwrap(),
            "Ted.2012.1080p.BluRay.DTS-HD.MA5.1.x264-DAA.DUAL-3LT0N"
        );
    }

    #[test]
    fn obra_em_ingles_leva_o_titulo_em_ingles() {
        let with = |original: &str, english: &str, series: bool| {
            standard(&Subject {
                original,
                media: &web_1080(),
                original_language: Some(Language::English),
                series,
                dual: false,
                brrip: false,
                english_title: Some(english),
            })
            .unwrap()
            .name
        };
        // Deixe a Neve Cair, 26/10: o título em português passou.
        assert_eq!(
            with(
                "Deixe.A.Neve.Cair.2019.1080p.NF.WEB-DL.DD5.1.x264-PiA",
                "Let It Snow",
                false
            ),
            "Let.It.Snow.2019.1080p.NF.WEB-DL.DDP5.1.H.264-PiA"
        );
        // Já em inglês: fica como está, inclusive o hífen.
        assert_eq!(
            with(
                "Spider-Man.Across.the.Spider-Verse.2023.1080p.WEB-DL.DDP5.1.H.264-G",
                "Spider-Man: Across the Spider-Verse",
                false
            ),
            "Spider-Man.Across.the.Spider-Verse.2023.1080p.WEB-DL.DDP5.1.H.264-G"
        );
        // Série com o ano que a separa da outra de mesmo nome.
        assert_eq!(
            with(
                "Drops.of.God.2023.S01E01.1080p.WEB-DL.H.264-G",
                "Drops of God",
                true
            ),
            "Drops.of.God.2023.S01E01.1080p.WEB-DL.DDP5.1.H.264-G"
        );
    }

    #[test]
    fn tag_so_de_quem_dubla_e_retag() {
        let media = media(
            video("h264", 1920, 1080, Hdr::Sdr),
            vec![
                track(Language::English, "ac3", 6),
                track(Language::Portuguese, "ac3", 6),
            ],
        );
        // Big Hero, 26/10: removido pela staff por retag.
        let gaps = name(
            "Operacao.Big.Hero.2015.1080p.BluRay.x264-shaggy",
            &media,
            false,
            true,
        )
        .unwrap_err();
        assert!(gaps[0].contains("retag"), "{gaps:?}");
        // Como segunda tag, com o grupo original, está certo.
        assert!(
            name(
                "Ted.2.2015.1080p.BluRay.x264-BLOW.DUAL-RK",
                &media,
                false,
                true
            )
            .is_ok()
        );
        // O grupo que lança o próprio DUAL passa.
        assert!(
            name(
                "Let.It.Snow.2019.1080p.NF.WEB-DL.DD5.1.H.264.DUAL-PiA",
                &media,
                false,
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn tags_e_termos_tecnicos() {
        assert_eq!(split_tags("A.WEB-DL").1.first, None);
        assert_eq!(split_tags("A.DTS-HD").1.first, None);
        assert_eq!(split_tags("A.S01E01-E02").1.first, None);
        assert_eq!(split_tags("A.H.264-SJB").1.first.as_deref(), Some("SJB"));
        let (body, tags) = split_tags("A.x264-DON.REPACK.DUAL-xprince00");
        assert_eq!(body, "A.x264");
        assert_eq!(tags.first.as_deref(), Some("DON"));
        assert_eq!(tags.second, Some((true, "xprince00".to_owned())));
    }

    #[test]
    fn hfr_e_titulo_com_simbolos() {
        let mut fast = web_1080();
        fast.video.as_mut().unwrap().frame_rate = Some(59.94);
        assert_eq!(
            name(
                "Tom & Jerry: O Filme! 2021 1080p WEB-DL H.264-X",
                &fast,
                false,
                false
            )
            .unwrap(),
            "Tom.and.Jerry.O.Filme.2021.1080p.WEB-DL.DDP5.1.HFR.H.264-X"
        );
    }
}

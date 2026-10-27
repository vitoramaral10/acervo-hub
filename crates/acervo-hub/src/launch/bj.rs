//! Envio ao BJ pelo formulário do próprio site (`upload.php`, Gazelle), com a
//! sessão do indexador. É decisão do usuário (26/10/2026), ciente de que a
//! regra 1.12 do BJ proíbe navegação automatizada: as requisições são as
//! normais do formulário, sem nada para disfarçar a automação.
//!
//! O formulário faz, no navegador:
//!
//! 1. ao colar o IMDB, `ajax.php?action=imdb_title` (JSON no formato do `OMDb`,
//!    com o achado do TMDB), que preenche título, gêneros, elenco, duração,
//!    capa, sinopse...;
//! 2. o upload de cada captura em `ajax.php?action=screen_up` (Dropzone);
//! 3. o `POST upload.php` multipart com tudo.
//!
//! O acervo refaz os mesmos passos. As listas do formulário são texto (o
//! valor enviado é o próprio nome), fixas aqui como estão na página; valor que
//! não bate com nenhuma opção impede o envio, com o motivo.

use std::fmt::Write as _;

use acervo_indexers::{SitePart, SiteRequest, SiteResponse};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;

use super::amigos::{Chosen, Refusal, imdb_id};
use crate::api::{Catalog, SearchError};

/// O indexador do BJ no catálogo.
pub(crate) const INDEXER: &str = "bjsharecustom";

/// O `source` que o BJ grava no `.torrent` recebido. Gerado sem ele, o hash
/// muda no site (o semeado não registra no tracker) e o envio volta 200 com
/// o torrent novo para baixar, em vez do redirect para o criado.
pub(crate) const TORRENT_SOURCE: &str = "BJ";

const UPLOAD_PAGE: &str = "/upload.php";

/// Base das imagens do TMDB, para o pôster que vier como caminho relativo.
const TMDB_IMAGE: &str = "https://image.tmdb.org/t/p/original";

/// O mínimo e o máximo de capturas do formulário.
const SCREENSHOTS_MIN: usize = 2;
const SCREENSHOTS_MAX: usize = 6;

// ------------------------------------------------------------ listas do formulário

const FORMATOS: &[&str] = &["MKV", "MP4", "AVI", "ISO", "VOB", "M2TS", "TS", "Outro"];
const QUALIDADES: &[&str] = &[
    "BD25", "BD50", "BD66", "BD100", "BDRip", "BRRip", "Blu-ray", "DVD5", "DVD9", "DVDRip",
    "DVDScr", "HD DVD", "HDRip", "HDTC", "HDTV", "PDTV", "SDTV", "TC", "TVRip", "UHDTV", "VHSRip",
    "WEB", "WEB-DL", "WEBRip", "Outro",
];
const AUDIOS: &[&str] = &[
    "Dual Áudio",
    "Dublado",
    "Legendado",
    "Nacional",
    "Sem Áudio",
    "Outro",
];
const TIPOS_LEGENDA: &[&str] = &[
    "Embutida",
    "Arquivos Separados",
    "Queimada no vídeo",
    "Nenhuma",
];
const CODECS_VIDEO: &[&str] = &[
    "AV1",
    "DivX",
    "H.263",
    "H.264",
    "H.265",
    "KVCD",
    "MPEG-1",
    "MPEG-2",
    "RealVideo",
    "VC-1",
    "VP6",
    "VP8",
    "VP9",
    "Windows Media Video",
    "XviD",
    "x264",
    "x265",
    "Outro",
];
const CODECS_AUDIO: &[&str] = &[
    "AAC",
    "AC3",
    "AC3/AAC",
    "AC3/DTS",
    "AC3/E-AC-3",
    "Cook",
    "DTS",
    "DTS-HD",
    "DTS:X",
    "E-AC-3",
    "FLAC",
    "LPCM",
    "MP1",
    "MP2",
    "MP3",
    "Opus",
    "PCM",
    "RealAudio",
    "TrueHD",
    "Vorbis",
    "Windows Media Audio",
    "Outro",
];

const IDIOMAS: &[&str] = &[
    "Alemão",
    "Árabe",
    "Argelino",
    "Búlgaro",
    "Cantonês",
    "Chinês",
    "Coreano",
    "Croata",
    "Dinamarquês",
    "Egípcio",
    "Espanhol",
    "Estoniano",
    "Filipino",
    "Finlandês",
    "Francês",
    "Grego",
    "Hebraico",
    "Hindi",
    "Holandês",
    "Húngaro",
    "Indonésio",
    "Inglês",
    "Islandês",
    "Italiano",
    "Japonês",
    "Macedônio",
    "Malaio",
    "Marati",
    "Nigeriano",
    "Norueguês",
    "Persa",
    "Polaco",
    "Polonês",
    "Português",
    "Português (pt)",
    "Romeno",
    "Russo",
    "Sueco",
    "Tailandês",
    "Tamil",
    "Tcheco",
    "Telugo",
    "Turco",
    "Ucraniano",
    "Urdu",
    "Vietnamita",
    "Zulu",
    "Outro",
];

/// Idioma original: código ISO 639-1 do TMDB, nome em inglês do `OMDb` e a
/// grafia da lista do formulário.
const LANGUAGES: &[(&str, &str, &str)] = &[
    ("en", "English", "Inglês"),
    ("es", "Spanish", "Espanhol"),
    ("pt", "Portuguese", "Português"),
    ("fr", "French", "Francês"),
    ("de", "German", "Alemão"),
    ("it", "Italian", "Italiano"),
    ("ja", "Japanese", "Japonês"),
    ("ko", "Korean", "Coreano"),
    ("zh", "Chinese", "Chinês"),
    ("zh", "Mandarin", "Chinês"),
    ("cn", "Cantonese", "Cantonês"),
    ("ru", "Russian", "Russo"),
    ("hi", "Hindi", "Hindi"),
    ("ar", "Arabic", "Árabe"),
    ("tr", "Turkish", "Turco"),
    ("sv", "Swedish", "Sueco"),
    ("da", "Danish", "Dinamarquês"),
    ("no", "Norwegian", "Norueguês"),
    ("nb", "Norwegian", "Norueguês"),
    ("nl", "Dutch", "Holandês"),
    ("pl", "Polish", "Polonês"),
    ("fi", "Finnish", "Finlandês"),
    ("th", "Thai", "Tailandês"),
    ("he", "Hebrew", "Hebraico"),
    ("fa", "Persian", "Persa"),
    ("id", "Indonesian", "Indonésio"),
    ("ms", "Malay", "Malaio"),
    ("tl", "Filipino", "Filipino"),
    ("uk", "Ukrainian", "Ucraniano"),
    ("cs", "Czech", "Tcheco"),
    ("el", "Greek", "Grego"),
    ("hu", "Hungarian", "Húngaro"),
    ("ro", "Romanian", "Romeno"),
    ("bg", "Bulgarian", "Búlgaro"),
    ("hr", "Croatian", "Croata"),
    ("is", "Icelandic", "Islandês"),
    ("et", "Estonian", "Estoniano"),
    ("vi", "Vietnamese", "Vietnamita"),
    ("ta", "Tamil", "Tamil"),
    ("te", "Telugu", "Telugo"),
    ("mr", "Marathi", "Marati"),
    ("ur", "Urdu", "Urdu"),
    ("zu", "Zulu", "Zulu"),
    ("mk", "Macedonian", "Macedônio"),
];

/// A sigla de streaming do nome do release, por extenso como o BJ escreve.
/// A caixa conta: `iT` e `MAX` só nessa grafia.
const STREAMINGS: &[(&str, &str)] = &[
    ("AMZN", "Amazon"),
    ("NF", "Netflix"),
    ("DSNP", "Disney+"),
    ("iT", "iTunes"),
    ("ATVP", "Apple TV+"),
    ("HMAX", "HBO Max"),
    ("MAX", "Max"),
    ("GLBO", "Globoplay"),
    ("PCOK", "Peacock"),
    ("PMTP", "Paramount+"),
    ("HULU", "Hulu"),
];

/// Edições do nome (em minúsculas, palavras por `.`) e a etiqueta do campo
/// "Informações adicionais".
const EDITIONS: &[(&str, &str)] = &[
    ("directors.cut", "Director's Cut"),
    ("director's.cut", "Director's Cut"),
    ("extended", "Extended Edition"),
    ("imax", "IMAX"),
    ("open.matte", "Open Matte"),
    ("theatrical", "Theatrical Cut"),
    ("final.cut", "Final Cut"),
    ("unrated", "Unrated"),
    ("uncut", "Uncut"),
    ("uncensored", "Uncensored"),
    ("colorized", "Colorized"),
    ("noir.edition", "Noir Edition"),
];

// ------------------------------------------------------------ página

/// O `auth` do formulário (`<input type="hidden" name="auth">`). Sem ele a
/// página não é o formulário de quem está logado.
pub(crate) fn auth_from_page(html: &str) -> Result<String> {
    html.match_indices("<input")
        .find_map(|(at, _)| {
            let tag = &html[at..at + html[at..].find('>')?];
            if !tag.contains(r#"name="auth""#) {
                return None;
            }
            let value = tag.split_once(r#"value=""#)?.1;
            let value = &value[..value.find('"')?];
            (!value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric()))
                .then(|| value.to_owned())
        })
        .context(
            "a página de envio do BJ não trouxe o `auth` do formulário: a sessão do indexador \
             caiu (confira o login do bjsharecustom)",
        )
}

// ------------------------------------------------------------ IMDB do site

/// O que `ajax.php?action=imdb_title` dá ao formulário, já sem os "N/A".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Lookup {
    /// `movie`, `series` ou `episode`.
    kind: String,
    title: Option<String>,
    brazilian_title: Option<String>,
    genres: Option<String>,
    year: Option<String>,
    cast: Option<String>,
    director: Option<String>,
    writer: Option<String>,
    runtime_minutes: Option<u32>,
    released: Option<String>,
    rating: Option<String>,
    poster: Option<String>,
    synopsis: Option<String>,
    country: Option<String>,
    rated: Option<String>,
    seasons: Option<String>,
    /// O IMDB da série, quando o pedido foi de um episódio.
    series_imdb: Option<String>,
    /// O idioma original na grafia da lista do formulário.
    language: Option<&'static str>,
    /// O `original_language` do TMDB, para dizer qual código não tem opção.
    language_code: Option<String>,
}

impl Lookup {
    /// O idioma original, ou o motivo de não haver opção.
    fn language_choice(&self) -> Result<&'static str, String> {
        match (self.language, &self.language_code) {
            (Some(language), _) => Ok(language),
            (None, Some(code)) => Err(format!("idioma original '{code}' sem opção na lista do BJ")),
            (None, None) => Err("o IMDB do BJ não disse o idioma original".to_owned()),
        }
    }
}

/// O texto de um campo do JSON; vazio e `N/A` são ausência, como no
/// JavaScript do site.
fn text(value: &Value, key: &str) -> Option<String> {
    let text = match value.get(key)? {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!text.is_empty() && text != "N/A").then_some(text)
}

fn language_by_code(code: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(c, _, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, _, name)| *name)
}

fn language_by_english(name: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(_, english, _)| english.eq_ignore_ascii_case(name))
        .map(|(_, _, name)| *name)
}

/// Lê a resposta de `imdb_title` como o `upload.js` a lê: filme pelo
/// `tmdbData.movie_results[0]`, série e episódio pelo `seriesData` e
/// `tmdbSeriesData.tv_results[0]`.
pub(crate) fn read_lookup(value: &Value) -> Result<Lookup> {
    if value.get("Response").and_then(Value::as_str) == Some("False") {
        bail!(
            "a busca no IMDB do BJ não achou a obra: {}",
            text(value, "Error").unwrap_or_else(|| "sem motivo".to_owned())
        );
    }
    let kind = text(value, "Type")
        .context("a busca no IMDB do BJ não disse o tipo da obra")?
        .to_ascii_lowercase();
    let movie = kind == "movie";
    let series_data = value.get("seriesData").unwrap_or(&Value::Null);
    let tmdb = if movie {
        value.pointer("/tmdbData/movie_results/0")
    } else {
        value.pointer("/tmdbSeriesData/tv_results/0")
    }
    .unwrap_or(&Value::Null);
    let title = if movie {
        text(value, "Title")
    } else {
        text(series_data, "Title").or_else(|| text(value, "Title"))
    };
    let brazilian_title =
        text(tmdb, if movie { "title" } else { "name" }).filter(|b| Some(b) != title.as_ref());
    let year = text(value, "Year")
        .map(|y| y.chars().take(4).collect::<String>())
        .filter(|y| y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()));
    let runtime_minutes = text(value, "Runtime").and_then(|r| {
        r.split_whitespace()
            .next()
            .and_then(|minutes| minutes.parse().ok())
    });
    let poster = text(tmdb, "poster_path").map(|path| {
        if path.starts_with('/') {
            format!("{TMDB_IMAGE}{path}")
        } else {
            path
        }
    });
    let country = text(value, "Country");
    let language_code = text(tmdb, "original_language");
    let language = match language_code.as_deref() {
        // Português de Portugal tem opção própria.
        Some(code)
            if code.eq_ignore_ascii_case("pt")
                && country
                    .as_deref()
                    .and_then(|c| c.split(',').next())
                    .is_some_and(|first| first.trim() == "Portugal") =>
        {
            Some("Português (pt)")
        }
        Some(code) => language_by_code(code),
        None => text(value, "Language").and_then(|l| {
            l.split(',')
                .next()
                .and_then(|first| language_by_english(first.trim()))
        }),
    };
    Ok(Lookup {
        series_imdb: (kind == "episode")
            .then(|| text(value, "seriesID"))
            .flatten(),
        title,
        brazilian_title,
        genres: text(value, "Genre"),
        year,
        cast: text(value, "Actors"),
        director: text(value, "Director"),
        writer: text(value, "Writer"),
        runtime_minutes,
        released: text(value, "Released"),
        rating: text(value, "imdbRating"),
        poster,
        synopsis: text(tmdb, "overview"),
        country,
        rated: text(value, "Rated"),
        seasons: text(value, "totalSeasons").or_else(|| text(series_data, "totalSeasons")),
        language,
        language_code,
        kind,
    })
}

// ------------------------------------------------------------ MediaInfo

/// Uma faixa de áudio do relatório.
#[derive(Debug, Clone, Default)]
struct AudioTrack {
    format: String,
    commercial: String,
    profile: String,
    language: String,
    features: String,
}

impl AudioTrack {
    /// Dolby Atmos: o objeto JOC do E-AC-3 ou o nome comercial.
    fn atmos(&self) -> bool {
        self.features.contains("JOC") || self.commercial.to_ascii_lowercase().contains("atmos")
    }
}

/// O que o envio lê do relatório do `MediaInfo` do vídeo principal (o mesmo
/// texto que vai em `fichatecnica`).
#[derive(Debug, Clone, Default)]
struct Media {
    container: Option<String>,
    video_format: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    bit_depth: Option<u32>,
    hdr_format: String,
    transfer: String,
    audio: Vec<AudioTrack>,
    subtitle_languages: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Video,
    Audio,
    Text,
    Other,
}

fn read_mediainfo(report: &str) -> Media {
    let mut media = Media::default();
    let mut section = Section::Other;
    let mut videos = 0;
    for line in report.lines() {
        let Some((key, value)) = line.split_once(" : ") else {
            let header = line.trim();
            if header.is_empty() {
                continue;
            }
            section = match header.split_whitespace().next().unwrap_or_default() {
                "General" => Section::General,
                "Video" => {
                    videos += 1;
                    Section::Video
                }
                "Audio" => {
                    media.audio.push(AudioTrack::default());
                    Section::Audio
                }
                "Text" => {
                    media.subtitle_languages.push(String::new());
                    Section::Text
                }
                _ => Section::Other,
            };
            continue;
        };
        let (key, value) = (key.trim(), value.trim().to_owned());
        let pixels = || {
            value
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        };
        match (section, key) {
            (Section::General, "Format") => media.container = Some(value),
            // Só o primeiro vídeo: é o principal.
            (Section::Video, "Format") if videos == 1 => media.video_format = Some(value),
            (Section::Video, "Bit depth") if videos == 1 => media.bit_depth = pixels(),
            (Section::Video, "HDR format") if videos == 1 => {
                media.hdr_format = format!("{} {value}", media.hdr_format);
            }
            (Section::Video, "Transfer characteristics") if videos == 1 => {
                media.transfer = format!("{} {value}", media.transfer);
            }
            (Section::Video, "Width") if videos == 1 => media.width = pixels(),
            (Section::Video, "Height") if videos == 1 => media.height = pixels(),
            (Section::Audio, "Format") => set(&mut media.audio, |t| &mut t.format, value),
            (Section::Audio, "Commercial name") => {
                set(&mut media.audio, |t| &mut t.commercial, value);
            }
            (Section::Audio, "Format profile") => set(&mut media.audio, |t| &mut t.profile, value),
            (Section::Audio, "Language") => set(&mut media.audio, |t| &mut t.language, value),
            (Section::Audio, "Format additional features") => {
                set(&mut media.audio, |t| &mut t.features, value);
            }
            (Section::Text, "Language") => {
                if let Some(last) = media.subtitle_languages.last_mut() {
                    *last = value;
                }
            }
            _ => {}
        }
    }
    media
}

/// Grava o campo da última faixa, se ainda vazio (a primeira linha vale).
fn set(tracks: &mut [AudioTrack], field: fn(&mut AudioTrack) -> &mut String, value: String) {
    if let Some(track) = tracks.last_mut() {
        let slot = field(track);
        if slot.is_empty() {
            *slot = value;
        }
    }
}

fn is_portuguese(language: &str) -> bool {
    language.to_ascii_lowercase().starts_with("portug")
}

// ------------------------------------------------------------ campos técnicos

/// As palavras do nome do release.
fn tokens(release: &str) -> Vec<&str> {
    release
        .split(['.', ' ', '_', '[', ']', '(', ')'])
        .filter(|t| !t.is_empty())
        .collect()
}

/// A palavra, sem caixa, também com o grupo colado (`x264-GRP`).
fn has(tokens: &[&str], word: &str) -> bool {
    tokens.iter().any(|token| {
        token.eq_ignore_ascii_case(word)
            || token
                .get(..word.len() + 1)
                .is_some_and(|head| head.eq_ignore_ascii_case(&format!("{word}-")))
    })
}

/// As palavras depois da primeira que `mark` reconhece (resolução, ano): é
/// onde ficam streaming e edição, longe do título. Sem a marca, todas.
fn after<'a>(tokens: &[&'a str], mark: impl Fn(&str) -> bool) -> Vec<&'a str> {
    match tokens.iter().position(|t| mark(t)) {
        Some(at) => tokens[at + 1..].to_vec(),
        None => tokens.to_vec(),
    }
}

fn is_resolution(token: &str) -> bool {
    token
        .strip_suffix(['p', 'P'])
        .is_some_and(|n| (3..=4).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_year(token: &str) -> bool {
    token.len() == 4
        && token.bytes().all(|b| b.is_ascii_digit())
        && (token.starts_with("19") || token.starts_with("20"))
}

/// `S01`, `S01E02`: o identificador de temporada/episódio do nome.
fn is_season_mark(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(['s', 'S']) else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let after = &rest[digits..];
    digits > 0
        && (after.is_empty()
            || (after.starts_with(['e', 'E'])
                && after[1..].starts_with(|c: char| c.is_ascii_digit())))
}

fn is_remux(tokens: &[&str]) -> bool {
    has(tokens, "remux") || has(tokens, "bdremux") || has(tokens, "dvdremux")
}

/// O nome diz DVD como fonte (`DVD`, `DVD5`, `DVD9`, `DVDRemux`).
fn is_dvd(tokens: &[&str]) -> bool {
    ["dvd", "dvd5", "dvd9", "dvdremux"]
        .iter()
        .any(|t| has(tokens, t))
}

fn container(media: &Media) -> Result<&'static str, String> {
    match media.container.as_deref() {
        Some("Matroska") => Ok("MKV"),
        Some("MPEG-4") => Ok("MP4"),
        Some("AVI") => Ok("AVI"),
        Some("MPEG-TS") => Ok("TS"),
        Some("BDAV") => Ok("M2TS"),
        Some(other) => Err(format!("o contêiner \"{other}\" não é opção")),
        None => Err("o MediaInfo não diz o contêiner".to_owned()),
    }
}

/// A Qualidade pela source do nome, lida só depois do ano, da temporada ou da
/// resolução (o título pode ter "Web"). O encode e o REMUX de Blu-ray são
/// `Blu-ray` (o REMUX ganha "Remux" nas informações adicionais).
fn quality(tokens: &[&str]) -> Result<&'static str, String> {
    let tail = after(tokens, |t| {
        is_year(t) || is_resolution(t) || is_season_mark(t)
    });
    let tokens = &tail[..];
    let bluray = has(tokens, "bluray") || has(tokens, "blu-ray") || has(tokens, "bdremux");
    // REMUX de vídeo é de disco: sem DVD no nome, é de Blu-ray
    // (`2160p.UHD.REMUX.HEVC`, `1080p.REMUX.AVC`).
    let quality = if is_remux(tokens) {
        if is_dvd(tokens) && !bluray {
            return Err("REMUX de DVD: escolha a qualidade à mão".to_owned());
        }
        "Blu-ray"
    } else if bluray {
        "Blu-ray"
    } else if has(tokens, "brrip") {
        "BRRip"
    } else if has(tokens, "bdrip") {
        "BDRip"
    } else if has(tokens, "web-dl") || has(tokens, "webdl") {
        "WEB-DL"
    } else if has(tokens, "webrip") || has(tokens, "web-rip") {
        "WEBRip"
    } else if has(tokens, "web") {
        "WEB"
    } else if has(tokens, "hdtv") {
        "HDTV"
    } else if has(tokens, "hdrip") {
        "HDRip"
    } else if has(tokens, "dvdrip") {
        "DVDRip"
    } else {
        return Err("o nome do release não diz a fonte (WEB-DL, BluRay...)".to_owned());
    };
    Ok(quality)
}

fn streaming(tokens: &[&str]) -> Option<&'static str> {
    let tail = after(tokens, is_resolution);
    STREAMINGS
        .iter()
        .find(|(code, _)| tail.iter().any(|t| t == code))
        .map(|(_, name)| *name)
}

/// O codec de vídeo: a família sai do nome do release e, sem nada nele, do
/// `Format` do `MediaInfo`; a grafia vem da qualidade (`modern`: WEB-DL, WEB e
/// REMUX levam `H.264`/`H.265`; encode, `WEBRip`, `HDTV` e o resto, `x264`/`x265`).
fn video_codec(release: &str, media: &Media, modern: bool) -> Result<&'static str, String> {
    // Só depois do ano, da temporada ou da resolução: o título pode ter
    // `h264` ou `mpeg2`.
    let words = after(&tokens(release), |t| {
        is_year(t) || is_resolution(t) || is_season_mark(t)
    });
    let squashed: String = words
        .join(".")
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let by_name = if squashed.contains("x265") || squashed.contains("h265") || has(&words, "hevc") {
        Some("265")
    } else if squashed.contains("x264") || squashed.contains("h264") || has(&words, "avc") {
        Some("264")
    } else if has(&words, "av1") {
        Some("AV1")
    } else if has(&words, "xvid") {
        Some("XviD")
    } else if squashed.contains("vc1") {
        Some("VC-1")
    } else if squashed.contains("mpeg2") {
        Some("MPEG-2")
    } else {
        None
    };
    let family = match (by_name, media.video_format.as_deref()) {
        (Some(family), _) => family,
        (None, Some("AVC")) => "264",
        (None, Some("HEVC")) => "265",
        (None, Some("AV1")) => "AV1",
        (None, Some("VC-1")) => "VC-1",
        (None, Some("MPEG Video")) => "MPEG-2",
        (None, Some(other)) => return Err(format!("o formato de vídeo \"{other}\" não é opção")),
        (None, None) => return Err("nem o nome nem o MediaInfo dizem o codec".to_owned()),
    };
    Ok(match (family, modern) {
        ("264", true) => "H.264",
        ("264", false) => "x264",
        ("265", true) => "H.265",
        ("265", false) => "x265",
        (other, _) => other,
    })
}

/// O codec de uma faixa, na grafia da lista.
fn track_codec(track: &AudioTrack) -> Option<&'static str> {
    let format = track.format.as_str();
    let commercial = track.commercial.to_ascii_lowercase();
    Some(
        if commercial.contains("dts:x") || format.starts_with("DTS XLL X") {
            "DTS:X"
        } else if format.starts_with("DTS") {
            if format.contains("XLL") || format.contains("XBR") || commercial.contains("dts-hd") {
                "DTS-HD"
            } else {
                "DTS"
            }
        } else if format.starts_with("E-AC-3") {
            "E-AC-3"
        } else if format.starts_with("AC-3") {
            "AC3"
        } else if format.starts_with("MLP FBA") || commercial.contains("truehd") {
            "TrueHD"
        } else if format.starts_with("AAC") {
            "AAC"
        } else if format.starts_with("MPEG Audio") {
            match track.profile.as_str() {
                "Layer 2" => "MP2",
                "Layer 1" => "MP1",
                _ => "MP3",
            }
        } else {
            match format {
                "FLAC" => "FLAC",
                "Opus" => "Opus",
                "PCM" => "PCM",
                "Vorbis" => "Vorbis",
                _ => return None,
            }
        },
    )
}

/// O codec de áudio pelas faixas do vídeo principal: um só codec é ele; dois
/// codecs que a lista combina (`AC3/DTS`, `AC3/E-AC-3`, `AC3/AAC`) vão
/// combinados; senão, o codec da faixa original (a primeira que não é PT).
fn audio_codec(media: &Media) -> Result<&'static str, String> {
    if media.audio.is_empty() {
        return Err("o MediaInfo não tem faixa de áudio".to_owned());
    }
    let mut codecs: Vec<&'static str> = Vec::new();
    for track in &media.audio {
        let codec = track_codec(track)
            .ok_or_else(|| format!("o áudio \"{}\" não é opção", track.format))?;
        if !codecs.contains(&codec) {
            codecs.push(codec);
        }
    }
    if let [only] = codecs[..] {
        return Ok(only);
    }
    if let [a, b] = codecs[..]
        && let Some(both) = CODECS_AUDIO
            .iter()
            .find(|o| **o == format!("{a}/{b}") || **o == format!("{b}/{a}"))
    {
        return Ok(both);
    }
    let original = media
        .audio
        .iter()
        .find(|t| !is_portuguese(&t.language))
        .unwrap_or(&media.audio[0]);
    track_codec(original).ok_or_else(|| "sem codec de áudio".to_owned())
}

fn audio(kind: Option<&str>) -> Result<&'static str, String> {
    match kind {
        Some("dual") => Ok("Dual Áudio"),
        Some("dublado") => Ok("Dublado"),
        Some("legendado") => Ok("Legendado"),
        Some("nacional") => Ok("Nacional"),
        Some(other) => Err(format!("o laudo diz \"{other}\", que não é opção")),
        None => Err("o laudo não diz o áudio: prepare o pacote de novo".to_owned()),
    }
}

/// Arquivo de legenda no torrent vale mais que a embutida; sem arquivo, a
/// legenda PT embutida; sem nenhuma das duas, nenhuma.
fn subtitle_kind(has_file: bool, media: &Media) -> &'static str {
    if has_file {
        "Arquivos Separados"
    } else if media.subtitle_languages.iter().any(|l| is_portuguese(l)) {
        "Embutida"
    } else {
        "Nenhuma"
    }
}

/// "Informações adicionais": `Remux`, as edições do nome e os recursos que o
/// `MediaInfo` do vídeo principal mostra, separados por ` / `, como as
/// etiquetas do formulário. Vazio quando não há nenhuma.
fn remaster(tokens: &[&str], media: &Media) -> String {
    // Edição só depois do ano ou da temporada: "Extended.Family.S01" não é
    // Extended Edition.
    let tail = after(tokens, |t| is_year(t) || is_season_mark(t));
    let dotted = format!(".{}.", tail.join(".").to_lowercase());
    let mut tags: Vec<&str> = Vec::new();
    if is_remux(tokens) {
        tags.push("Remux");
    }
    for (key, tag) in EDITIONS {
        if dotted.contains(&format!(".{key}.")) && !tags.contains(tag) {
            tags.push(tag);
        }
    }
    let hdr = media.hdr_format.to_ascii_lowercase();
    let transfer = media.transfer.to_ascii_lowercase();
    let dolby_vision = hdr.contains("dolby vision");
    let hdr10_plus = hdr.contains("hdr10+") || hdr.contains("st 2094 app 4");
    // HDR10+ não leva também HDR10; DV leva quando a camada base é HDR10.
    let hdr10 = !hdr10_plus
        && (hdr.contains("hdr10")
            || hdr.contains("st 2086")
            || (!dolby_vision && transfer.contains("pq")));
    let hlg = hdr.contains("hlg") || transfer.contains("hlg");
    let ultrawide = matches!((media.width, media.height), (Some(w), Some(h)) if w > 2 * h);
    let features = [
        (media.bit_depth == Some(10), "10-bit"),
        (dolby_vision, "Dolby Vision"),
        (hdr10, "HDR10"),
        (hdr10_plus, "HDR10+"),
        (hlg, "HLG"),
        (media.audio.iter().any(AudioTrack::atmos), "Dolby Atmos"),
        (ultrawide, "Ultrawide"),
    ];
    tags.extend(features.iter().filter(|(on, _)| *on).map(|(_, tag)| *tag));
    tags.join(" / ")
}

// ------------------------------------------------------------ rascunho

/// Um campo do formulário: o nome que vai no `POST` e, se a prévia o mostra,
/// o rótulo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Field {
    name: &'static str,
    label: Option<&'static str>,
    value: String,
}

#[derive(Debug, Default)]
struct Form {
    fields: Vec<Field>,
    missed: Vec<String>,
}

impl Form {
    fn text(&mut self, name: &'static str, value: impl Into<String>) {
        self.fields.push(Field {
            name,
            label: None,
            value: value.into(),
        });
    }

    fn shown(&mut self, name: &'static str, label: &'static str, value: impl Into<String>) {
        self.fields.push(Field {
            name,
            label: Some(label),
            value: value.into(),
        });
    }

    /// Campo que o formulário pede preenchido: ausente impede o envio.
    fn required(&mut self, name: &'static str, label: &'static str, value: Option<String>) {
        if value.is_none() {
            self.missed
                .push(format!("{label}: o IMDB do BJ não trouxe o dado"));
        }
        self.text(name, value.unwrap_or_default());
    }

    /// Uma lista: a opção exata, ou o motivo que impede o envio.
    fn pick(
        &mut self,
        name: &'static str,
        label: &'static str,
        options: &[&str],
        value: Result<&str, String>,
    ) {
        match value {
            Ok(value) if options.contains(&value) => self.shown(name, label, value),
            Ok(value) => self
                .missed
                .push(format!("{label}: \"{value}\" não é opção do formulário")),
            Err(reason) => self.missed.push(format!("{label}: {reason}")),
        }
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.value.as_str())
    }
}

/// Os campos técnicos, na ordem do formulário (de Formato a Resolução).
fn technical(
    form: &mut Form,
    package: &Package,
    media: &Media,
    language: Result<&'static str, String>,
) {
    let words = tokens(&package.release);
    form.pick("formato", "Formato", FORMATOS, container(media));
    let quality = quality(&words);
    let modern = is_remux(&words) || matches!(quality, Ok("WEB-DL" | "WEB"));
    form.pick("qualidade", "Qualidade", QUALIDADES, quality);
    form.shown(
        "release",
        "Streaming",
        streaming(&words).unwrap_or_default(),
    );
    form.pick("audio", "Áudio", AUDIOS, audio(package.audio.as_deref()));
    form.pick(
        "tipolegenda",
        "Tipo de legenda",
        TIPOS_LEGENDA,
        Ok(subtitle_kind(package.has_subtitle_file, media)),
    );
    form.pick(
        "codecvideo",
        "Codec de vídeo",
        CODECS_VIDEO,
        video_codec(&package.release, media, modern),
    );
    form.pick(
        "codecaudio",
        "Codec de áudio",
        CODECS_AUDIO,
        audio_codec(media),
    );
    // A obra nacional é falada em português, diga o TMDB o que disser.
    let language = if package.audio.as_deref() == Some("nacional") {
        Ok("Português")
    } else {
        language
    };
    form.pick("idioma", "Idioma original", IDIOMAS, language);
    // Checkbox marcado vai como `on`; desmarcado não vai.
    if has(&words, "repack") || has(&words, "proper") {
        form.shown("repack", "Repack", "on");
    }
    let remaster = remaster(&words, media);
    if remaster.chars().count() > 80 {
        form.missed.push(format!(
            "Informações adicionais: \"{remaster}\" passa dos 80 caracteres do campo"
        ));
    }
    form.shown("remaster_title", "Informações adicionais", remaster);
    match (media.width, media.height) {
        (Some(width), Some(height)) => {
            form.text("resolucaow", width.to_string());
            form.text("resolucaoh", height.to_string());
        }
        _ => form
            .missed
            .push("Resolução: o MediaInfo não diz largura e altura".to_owned()),
    }
}

/// O que vai ao BJ, montado antes de qualquer upload: é a prévia que o
/// usuário confirma e, refeito na hora, o envio. Mesmo formato da prévia do
/// Amigos, para a tela ser a mesma.
#[derive(Clone, Serialize)]
pub(crate) struct Draft {
    pub(crate) tipo: &'static str,
    pub(crate) nome: String,
    pub(crate) ano: String,
    pub(crate) imdb: String,
    pub(crate) largura: Option<u32>,
    pub(crate) altura: Option<u32>,
    pub(crate) capa: bool,
    pub(crate) campos: Vec<Chosen>,
    /// O tipo de legenda vai em `campos`.
    pub(crate) legenda: Option<String>,
    pub(crate) capturas: usize,
    /// O que impede o envio.
    pub(crate) faltando: Vec<String>,
    #[serde(skip)]
    auth: String,
    #[serde(skip)]
    fields: Vec<Field>,
}

/// O que o pacote dá ao rascunho.
#[derive(Debug, Clone)]
pub(crate) struct Package {
    pub(crate) series: bool,
    /// Na série, a temporada; no episódio avulso, também o episódio.
    pub(crate) season: Option<u16>,
    pub(crate) episode: Option<u16>,
    pub(crate) release: String,
    pub(crate) imdb_id: String,
    pub(crate) mediainfo: String,
    pub(crate) synopsis: String,
    /// `report.audio`: `dual`, `dublado`, `legendado` ou `nacional`.
    pub(crate) audio: Option<String>,
    /// A duração do vídeo pelo laudo, em segundos, para quando o IMDB não diz.
    pub(crate) duration: Option<f64>,
    /// `report.idioma_original`: o nome em inglês do idioma pelo catálogo.
    pub(crate) original_language: Option<String>,
    pub(crate) has_subtitle_file: bool,
    pub(crate) screenshots: usize,
}

/// O pacote como o envio ao BJ o lê.
pub(crate) fn package(
    launch: &acervo_store::Launch,
    has_subtitle_file: bool,
    screenshots: usize,
) -> Result<Package> {
    let field = |name: &str| {
        launch
            .fields
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let release = field("release");
    let kind = launch.report.get("tipo").and_then(Value::as_str);
    let parsed = acervo_parser::parse_episode_title(&release);
    let series = match kind {
        Some(kind) => kind != "filme",
        None => parsed.is_some(),
    };
    let (season, episode) = match (&parsed, series) {
        (Some(parsed), true) => {
            let single = match kind {
                Some("episodio") => true,
                Some("temporada") => false,
                _ => !parsed.full_season && parsed.episodes.len() == 1,
            };
            (
                Some(parsed.season),
                single.then(|| parsed.episodes.first().copied()).flatten(),
            )
        }
        _ => (None, None),
    };
    if series && season.is_none() {
        bail!("o nome do release não diz a temporada: o BJ pede temporada e episódio");
    }
    let imdb_id = imdb_id(&field("imdb"))
        .map(str::to_owned)
        .context("o pacote não tem o código do IMDB: o BJ preenche o formulário por ele")?;
    let mediainfo = field("mediainfo");
    if mediainfo.is_empty() {
        bail!("o pacote não tem o relatório do MediaInfo: prepare de novo");
    }
    Ok(Package {
        series,
        season,
        episode,
        release,
        imdb_id,
        mediainfo,
        synopsis: field("sinopse"),
        audio: launch
            .report
            .get("audio")
            .and_then(Value::as_str)
            .map(str::to_owned),
        duration: launch
            .report
            .pointer("/capturas/duracao")
            .and_then(Value::as_f64),
        original_language: launch
            .report
            .get("idioma_original")
            .and_then(Value::as_str)
            .map(str::to_owned),
        has_subtitle_file,
        screenshots,
    })
}

/// O caminho de `imdb_title` para o pacote, como o `upload.js` o monta.
fn lookup_path(package: &Package, auth: &str) -> String {
    let mut path = format!(
        "/ajax.php?action=imdb_title&category={}&imdbid={}&auth={auth}",
        if package.series { "tv" } else { "movie" },
        package.imdb_id
    );
    // Temporada e episódio só no episódio avulso: no pacote de temporada o
    // pedido é o da série.
    if let (Some(season), Some(episode)) = (package.season, package.episode) {
        let _ = write!(path, "&season={season}&episode={episode}");
    }
    path
}

/// A duração em horas e minutos (as listas vão até 9 h).
fn duration(form: &mut Form, minutes: Option<u32>) {
    form.text("duracaotipo", "selectbox");
    match minutes {
        Some(minutes) if minutes / 60 <= 9 => {
            form.shown("duracaoHR", "Duração (horas)", (minutes / 60).to_string());
            form.shown(
                "duracaoMIN",
                "Duração (minutos)",
                (minutes % 60).to_string(),
            );
        }
        Some(minutes) => form
            .missed
            .push(format!("Duração: {minutes} min passa das 9 h da lista")),
        None => form
            .missed
            .push("Duração: nem o IMDB do BJ nem o laudo dizem".to_owned()),
    }
}

/// Os campos da obra, os que o IMDB do site preenche, na ordem de cada
/// formulário (o de série tem tipo, temporada e episódio e não tem "adulto").
fn work(form: &mut Form, package: &Package, lookup: &Lookup, imdb: &str, minutes: Option<u32>) {
    if package.series {
        form.text("type", "1");
        form.text("validimdb", "yes");
        form.text("imdbrating", lookup.rating.clone().unwrap_or_default());
        let single = package.episode.is_some();
        form.shown("tipo", "Tipo", if single { "episode" } else { "season" });
        form.shown(
            "season",
            "Temporada",
            package.season.map(|s| s.to_string()).unwrap_or_default(),
        );
        form.shown(
            "episode",
            "Episódio",
            package.episode.map(|e| e.to_string()).unwrap_or_default(),
        );
        form.text("imdblink", imdb);
        form.text("network", "");
        form.required("title", "Título original", lookup.title.clone());
        form.shown(
            "titulobrasileiro",
            "Título nacional",
            lookup.brazilian_title.clone().unwrap_or_default(),
        );
        form.text("numtemporadas", lookup.seasons.clone().unwrap_or_default());
        form.required("year", "Ano", lookup.year.clone());
        form.required(
            "datalancamento",
            "Data de lançamento",
            lookup.released.clone(),
        );
        form.required("tags", "Gêneros", lookup.genres.clone());
        form.text("pais", lookup.country.clone().unwrap_or_default());
        form.required("elenco", "Elenco", lookup.cast.clone());
        form.text("diretorserie", lookup.director.clone().unwrap_or_default());
        form.required("diretor", "Criador", lookup.writer.clone());
        duration(form, minutes);
        form.text("avaliacao", lookup.rated.clone().unwrap_or_default());
    } else {
        form.text("type", "0");
        form.text("adulto", "2");
        form.text("imdblink", imdb);
        form.text("validimdb", "yes");
        form.required("title", "Título original", lookup.title.clone());
        form.shown(
            "titulobrasileiro",
            "Título nacional",
            lookup.brazilian_title.clone().unwrap_or_default(),
        );
        form.text("imdbrating", lookup.rating.clone().unwrap_or_default());
        form.required("tags", "Gêneros", lookup.genres.clone());
        form.required("year", "Ano", lookup.year.clone());
        form.required("elenco", "Elenco", lookup.cast.clone());
        form.text("diretorserie", lookup.director.clone().unwrap_or_default());
        duration(form, minutes);
        form.required(
            "datalancamento",
            "Data de lançamento",
            lookup.released.clone(),
        );
    }
}

/// Junta o pacote e a leitura do IMDB do site no rascunho, na ordem dos
/// campos do formulário.
pub(crate) fn draft_from(package: &Package, lookup: &Lookup, auth: String) -> Draft {
    let media = read_mediainfo(&package.mediainfo);
    let mut form = Form::default();
    let is_movie = lookup.kind == "movie";
    if package.series == is_movie {
        form.missed.push(format!(
            "o IMDB {} diz que a obra é {}, e o pacote é {}",
            package.imdb_id,
            if is_movie { "filme" } else { "série" },
            if package.series { "série" } else { "filme" }
        ));
    }
    let imdb = lookup
        .series_imdb
        .clone()
        .unwrap_or_else(|| package.imdb_id.clone());
    let minutes = lookup.runtime_minutes.or_else(|| {
        package
            .duration
            .filter(|d| d.is_finite() && *d > 0.0)
            // Finita e positiva: minutos de vídeo cabem em `u32`.
            .map(|d| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let minutes = (d / 60.0).round() as u32;
                minutes
            })
    });
    let genres = lookup.genres.as_deref().unwrap_or_default().to_lowercase();
    if (genres.contains("animation") || genres.contains("animação"))
        && lookup.language == Some("Japonês")
    {
        form.missed
            .push("anime vai na categoria Animes do BJ, que o acervo não envia".to_owned());
    }
    work(&mut form, package, lookup, &imdb, minutes);
    form.text("traileryoutube", "");
    // Sem idioma no IMDB do site, o do catálogo ("Portuguese (Brazil)" vira "Portuguese").
    let language = lookup.language_choice().or_else(|missing| {
        package
            .original_language
            .as_deref()
            .and_then(|name| language_by_english(name.split(" (").next().unwrap_or(name)))
            .ok_or(missing)
    });
    technical(&mut form, package, &media, language);
    // A sinopse do pacote é a PT-BR do TMDB que o usuário revisou (4.1.6);
    // sem ela, a que o IMDB do site trouxe.
    let synopsis = Some(package.synopsis.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| lookup.synopsis.clone());
    if synopsis.is_none() {
        form.missed.push(
            "Sinopse: obrigatória em PT-BR (4.1.6), e nem o pacote nem o IMDB do BJ a têm"
                .to_owned(),
        );
    }
    form.text("sinopse", synopsis.unwrap_or_default());
    form.text("fichatecnica", &package.mediainfo);
    if lookup.poster.is_none() {
        form.missed
            .push("Capa: o IMDB do BJ não trouxe pôster".to_owned());
    }
    form.text("image", lookup.poster.clone().unwrap_or_default());
    let screenshots = package.screenshots.min(SCREENSHOTS_MAX);
    if screenshots < SCREENSHOTS_MIN {
        form.missed.push(format!(
            "Capturas: o BJ pede ao menos {SCREENSHOTS_MIN}, o pacote tem {screenshots}"
        ));
    }
    let campos = form
        .fields
        .iter()
        .filter_map(|f| {
            let label = f.label?;
            let valor = match f.name {
                "repack" => "sim".to_owned(),
                _ if f.value.is_empty() => return None,
                _ => f.value.clone(),
            };
            Some(Chosen {
                campo: label.to_owned(),
                valor,
            })
        })
        .collect();
    Draft {
        tipo: if package.series { "Série" } else { "Filme" },
        nome: form.value("title").unwrap_or_default().to_owned(),
        ano: form.value("year").unwrap_or_default().to_owned(),
        imdb,
        largura: media.width,
        altura: media.height,
        capa: lookup.poster.is_some(),
        campos,
        legenda: None,
        capturas: screenshots,
        faltando: form.missed,
        auth,
        fields: form.fields,
    }
}

/// O corpo do `POST upload.php`, como o navegador o manda: os ocultos
/// `submit` e `auth`, o `.torrent` em `file_input`, os campos na ordem da
/// página e as capturas hospedadas em `screenshots[]`. Checkbox desmarcado
/// (anônimo) não vai.
pub(crate) fn parts(
    draft: &Draft,
    torrent: Vec<u8>,
    torrent_name: &str,
    screenshots: &[String],
) -> Vec<SitePart> {
    let mut parts = vec![
        SitePart::text("submit", "true"),
        SitePart::text("auth", &draft.auth),
        SitePart::file(
            "file_input",
            torrent_name,
            "application/x-bittorrent",
            torrent,
        ),
    ];
    parts.extend(
        draft
            .fields
            .iter()
            .map(|f| SitePart::text(f.name, &f.value)),
    );
    parts.push(SitePart::text("screenshots", ""));
    parts.extend(
        screenshots
            .iter()
            .map(|url| SitePart::text("screenshots[]", url)),
    );
    parts
}

// ------------------------------------------------------------ resposta do envio

/// O id em `torrents.php?id=<grupo>[&torrentid=<torrent>]`: o do torrent
/// quando vem, senão o do grupo.
fn created_id(location: &str) -> Option<String> {
    let query = location.split_once("torrents.php?")?.1.split('#').next()?;
    let param = |key: &str| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                .then(|| value.to_owned())
        })
    };
    param("torrentid").or_else(|| param("id"))
}

/// O erro que o Gazelle escreve acima do formulário devolvido
/// (`<p style="color: red; …">`), sem tags.
fn page_error(page: &str) -> Option<String> {
    let lower = page.to_ascii_lowercase();
    let at = lower
        .find(r#"<p style="color: red"#)
        .or_else(|| lower.find(r#"<p style="color:red"#))?;
    let open = at + page[at..].find('>')? + 1;
    let close = open + lower[open..].find("</p>")?;
    let mut text = String::new();
    let mut inside = false;
    for c in page[open..close].chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// O id do torrent criado, pelo redirect que o envio devolve; ou o motivo da
/// recusa, como o site o escreveu. O que não dá para ler é incerto: o torrent
/// pode existir.
pub(crate) fn created(response: &SiteResponse) -> Result<String, Refusal> {
    if (300..400).contains(&response.status) {
        let location = response.location.as_deref().unwrap_or_default();
        if let Some(id) = created_id(location) {
            return Ok(id);
        }
        // Sem a query: pode levar `authkey` ou `torrent_pass`.
        let path = location.split('?').next().unwrap_or_default();
        return Err(Refusal::unknown(format!(
            "o BJ redirecionou para {path}, e não para o torrent criado"
        )));
    }
    if response.status == 200 {
        let page = response.text();
        // O Gazelle devolve o próprio formulário, com o erro, quando recusa o
        // envio. Sem o erro lido não há como saber que foi recusa.
        if page.contains(r#"id="upload_table""#) {
            return Err(match page_error(&page) {
                Some(reason) => Refusal::certain(format!("o BJ recusou o envio: {reason}")),
                None => Refusal::unknown(
                    "o BJ devolveu o formulário sem dizer o motivo: confira o envio no site",
                ),
            });
        }
    }
    // O título da página diz o que ela é sem levar nada dela ao log.
    let title = page_title(&response.text())
        .map(|title| format!(", página \"{title}\""))
        .unwrap_or_default();
    Err(Refusal::unknown(format!(
        "resposta inesperada do BJ ao envio (HTTP {}{title})",
        response.status
    )))
}

/// O `<title>` da página, com os espaços juntados e no máximo 80 caracteres.
fn page_title(page: &str) -> Option<String> {
    let lower = page.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + page[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title>")?;
    let title = page[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!title.is_empty()).then(|| title.chars().take(80).collect())
}

// ------------------------------------------------------------ com o site

async fn request(catalog: &Catalog, request: SiteRequest) -> Result<SiteResponse> {
    catalog
        .site_request(INDEXER, &request)
        .await
        .map_err(|error| match error {
            SearchError::NoSuchIndexer => {
                anyhow!("o indexador do BJ não está configurado no acervo")
            }
            other => anyhow!(other),
        })
}

/// O rascunho do envio, com as duas leituras do site (o `auth` da página e o
/// IMDB). Nada é publicado.
pub(crate) async fn draft(catalog: &Catalog, package: &Package) -> Result<Draft> {
    let page = request(catalog, SiteRequest::get(UPLOAD_PAGE)).await?;
    if page.status != 200 {
        bail!(
            "a página de envio do BJ respondeu HTTP {}: confira o login do indexador",
            page.status
        );
    }
    let auth = auth_from_page(&page.text())?;
    let response = request(catalog, SiteRequest::get(lookup_path(package, &auth))).await?;
    if !(200..300).contains(&response.status) {
        bail!("a busca no IMDB do BJ respondeu HTTP {}", response.status);
    }
    let value: Value = serde_json::from_slice(&response.body)
        .context("a busca no IMDB do BJ devolveu algo que não é JSON")?;
    let lookup = read_lookup(&value)?;
    Ok(draft_from(package, &lookup, auth))
}

/// Hospeda uma captura PNG no BJ (`screen_up`, campo `file` do Dropzone): a
/// URL que vai em `screenshots[]`.
pub(crate) async fn host_screenshot(
    catalog: &Catalog,
    image: Vec<u8>,
    file_name: &str,
) -> Result<String> {
    let response = request(
        catalog,
        SiteRequest::multipart(
            "/ajax.php?action=screen_up",
            vec![SitePart::file("file", file_name, "image/png", image)],
        ),
    )
    .await?;
    let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    match text(&body, "url") {
        Some(url) if (200..300).contains(&response.status) => Ok(url),
        _ => bail!(
            "o BJ recusou a captura: {}",
            text(&body, "error")
                .or_else(|| text(&body, "message"))
                .unwrap_or_else(|| format!("HTTP {}", response.status))
        ),
    }
}

/// O `POST upload.php`. `Ok` com o id do torrent criado. Sem resposta (rede,
/// sessão, espera do indexador) a recusa não é certa: a requisição pode ter
/// chegado.
///
/// O BJ aceita o envio com uma página "Atenção" (HTTP 200), não com o
/// redirect do Gazelle: sem resposta legível num 200, o torrent é procurado
/// no grupo do IMDB por um dos `names` (o release e o nome do `.torrent`).
pub(crate) async fn publish(
    catalog: &Catalog,
    imdb_id: &str,
    names: &[String],
    parts: Vec<SitePart>,
) -> Result<String, Refusal> {
    let response = request(catalog, SiteRequest::multipart(UPLOAD_PAGE, parts))
        .await
        .map_err(|error| Refusal::unknown(format!("{error:#}")))?;
    match created(&response) {
        Err(refusal) if !refusal.certain && response.status == 200 => {
            match find_uploaded(catalog, imdb_id, names).await {
                Ok(Some(id)) => Ok(id),
                Ok(None) => Err(refusal),
                Err(error) => {
                    tracing::warn!("conferindo o envio no grupo do BJ: {error:#}");
                    Err(refusal)
                }
            }
        }
        outcome => outcome,
    }
}

/// O id do torrent enviado, no grupo que a busca pelo IMDB abre (com um só
/// grupo, `searchstr` redireciona para ele). `None` se não estiver lá.
async fn find_uploaded(
    catalog: &Catalog,
    imdb_id: &str,
    names: &[String],
) -> Result<Option<String>> {
    let search = format!("/torrents.php?searchstr={imdb_id}");
    let mut response = request(catalog, SiteRequest::get(search)).await?;
    if (300..400).contains(&response.status) {
        let Some(group) = response.location.as_deref().and_then(group_path) else {
            return Ok(None);
        };
        response = request(catalog, SiteRequest::get(group)).await?;
    }
    if response.status != 200 {
        return Ok(None);
    }
    let page = response.text();
    Ok(names.iter().find_map(|name| uploaded_id(&page, name)))
}

/// O caminho do grupo no `Location` da busca (`torrents.php?id=<grupo>`).
fn group_path(location: &str) -> Option<String> {
    let query = location.split_once("torrents.php?")?.1;
    query
        .starts_with("id=")
        .then(|| format!("/torrents.php?{query}"))
}

/// O id da linha do grupo cujo `data-torrentname` é `name`, escapado como o
/// site escreve (`htmlspecialchars`): `id="torrent<id>"` vem antes, na mesma
/// tag.
fn uploaded_id(page: &str, name: &str) -> Option<String> {
    let escaped = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;");
    let at = page.find(&format!(r#" data-torrentname="{escaped}""#))?;
    let tag = &page[page[..at].rfind('<')?..at];
    let start = tag.rfind(r#"id="torrent"#)? + r#"id="torrent"#.len();
    let id: String = tag[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!id.is_empty() && tag[start + id.len()..].starts_with('"')).then_some(id)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const WEB_DL: &str = "General
Complete name                            : Filme.2024.1080p.AMZN.WEB-DL.DDP5.1.H.264.DUAL-GRP.mkv
Format                                   : Matroska

Video
ID                                       : 1
Format                                   : AVC
Width                                    : 1 920 pixels
Height                                   : 1 080 pixels

Audio #1
Format                                   : E-AC-3
Commercial name                          : Dolby Digital Plus
Language                                 : Portuguese (BR)

Audio #2
Format                                   : E-AC-3
Commercial name                          : Dolby Digital Plus
Language                                 : English

Text #1
Format                                   : UTF-8
Language                                 : Portuguese (BR)
";

    const ENCODE: &str = "General
Format                                   : Matroska

Video
Format                                   : HEVC
Writing library                          : x265 3.5:[Linux][GCC 11.2.0][64 bit] 10bit
Width                                    : 3 840 pixels
Height                                   : 1 600 pixels

Audio #1
Format                                   : DTS
Language                                 : English

Audio #2
Format                                   : AC-3
Commercial name                          : Dolby Digital
Language                                 : English
";

    const REMUX: &str = "General
Format                                   : Matroska

Video
Format                                   : AVC
Width                                    : 1 920 pixels
Height                                   : 1 080 pixels

Audio #1
Format                                   : DTS XLL
Commercial name                          : DTS-HD Master Audio
Language                                 : Portuguese

Audio #2
Format                                   : DTS XLL
Commercial name                          : DTS-HD Master Audio
Language                                 : English
";

    fn package(release: &str, mediainfo: &str, audio: &str, has_file: bool) -> Package {
        Package {
            series: false,
            season: None,
            episode: None,
            release: release.into(),
            imdb_id: "tt1234567".into(),
            mediainfo: mediainfo.into(),
            synopsis: "Sinopse do pacote.".into(),
            audio: Some(audio.into()),
            duration: Some(5400.0),
            original_language: None,
            has_subtitle_file: has_file,
            screenshots: 4,
        }
    }

    fn sent(form: &Form) -> std::collections::HashMap<&str, &str> {
        form.fields
            .iter()
            .map(|f| (f.name, f.value.as_str()))
            .collect()
    }

    fn movie_lookup() -> Value {
        json!({
            "Title": "The Movie", "Year": "2015", "Rated": "PG", "Released": "25 Nov 2015",
            "Runtime": "93 min", "Genre": "Animation, Adventure", "Director": "N/A",
            "Writer": "Fulano", "Actors": "Atriz Um, Ator Dois", "Language": "English, Spanish",
            "Country": "United States", "imdbRating": "N/A", "Type": "movie", "Response": "True",
            "tmdbData": {"movie_results": [{
                "title": "O Filme", "overview": "Sinopse do TMDB.",
                "poster_path": "/abc.jpg", "original_language": "en"
            }]}
        })
    }

    #[test]
    fn web_dl_dual_h264_da_amazon() {
        let package = package(
            "Filme.2024.1080p.AMZN.WEB-DL.DDP5.1.H.264.DUAL-GRP",
            WEB_DL,
            "dual",
            false,
        );
        let mut form = Form::default();
        technical(&mut form, &package, &read_mediainfo(WEB_DL), Ok("Inglês"));
        assert!(form.missed.is_empty(), "{:?}", form.missed);
        let sent = sent(&form);
        assert_eq!(sent["formato"], "MKV");
        assert_eq!(sent["qualidade"], "WEB-DL");
        assert_eq!(sent["release"], "Amazon");
        assert_eq!(sent["audio"], "Dual Áudio");
        assert_eq!(sent["tipolegenda"], "Embutida");
        assert_eq!(sent["codecvideo"], "H.264");
        assert_eq!(sent["codecaudio"], "E-AC-3");
        assert_eq!(sent["idioma"], "Inglês");
        assert_eq!(sent["remaster_title"], "");
        assert_eq!((sent["resolucaow"], sent["resolucaoh"]), ("1920", "1080"));
        assert!(!sent.contains_key("repack"));
    }

    #[test]
    fn encode_de_blu_ray_x265_legendado_com_srt() {
        let package = package(
            "Filme.2019.Directors.Cut.2160p.BluRay.x265.10bit.REPACK-GRP",
            ENCODE,
            "legendado",
            true,
        );
        let mut form = Form::default();
        technical(&mut form, &package, &read_mediainfo(ENCODE), Ok("Inglês"));
        assert!(form.missed.is_empty(), "{:?}", form.missed);
        let sent = sent(&form);
        assert_eq!(sent["qualidade"], "Blu-ray");
        assert_eq!(sent["codecvideo"], "x265");
        assert_eq!(sent["audio"], "Legendado");
        assert_eq!(sent["tipolegenda"], "Arquivos Separados");
        // Duas faixas de codecs que a lista combina.
        assert_eq!(sent["codecaudio"], "AC3/DTS");
        assert_eq!(sent["release"], "");
        // 3840x1600 passa de 2,0.
        assert_eq!(sent["remaster_title"], "Director's Cut / Ultrawide");
        assert_eq!(sent["repack"], "on");
        assert_eq!((sent["resolucaow"], sent["resolucaoh"]), ("3840", "1600"));
    }

    #[test]
    fn remux_leva_remux_nas_informacoes_adicionais() {
        let package = package(
            "Filme.2010.Extended.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1.DUAL-GRP",
            REMUX,
            "dual",
            false,
        );
        let mut form = Form::default();
        technical(&mut form, &package, &read_mediainfo(REMUX), Ok("Inglês"));
        assert!(form.missed.is_empty(), "{:?}", form.missed);
        let sent = sent(&form);
        assert_eq!(sent["qualidade"], "Blu-ray");
        assert_eq!(sent["codecvideo"], "H.264");
        assert_eq!(sent["codecaudio"], "DTS-HD");
        assert_eq!(sent["tipolegenda"], "Nenhuma");
        assert_eq!(sent["remaster_title"], "Remux / Extended Edition");
    }

    #[test]
    fn codec_e_idioma_sem_opcao_impedem_o_envio() {
        let mediainfo = WEB_DL.replace("E-AC-3", "WMA");
        let package = package(
            "Filme.2024.1080p.WEB-DL.H.264-GRP",
            &mediainfo,
            "dual",
            false,
        );
        let mut form = Form::default();
        technical(
            &mut form,
            &package,
            &read_mediainfo(&mediainfo),
            Err("o IMDB do BJ não disse o idioma original".into()),
        );
        assert_eq!(form.missed.len(), 2, "{:?}", form.missed);
        assert!(form.missed[0].starts_with("Codec de áudio") && form.missed[0].contains("WMA"));
        assert!(form.missed[1].starts_with("Idioma original"));
        // Obra nacional é Português sem depender do IMDB.
        let package = super::tests::package(
            "Filme.2024.1080p.WEB-DL.H.264-GRP",
            WEB_DL,
            "nacional",
            false,
        );
        let mut form = Form::default();
        technical(
            &mut form,
            &package,
            &read_mediainfo(WEB_DL),
            Err("sem idioma".into()),
        );
        assert_eq!(sent(&form)["idioma"], "Português");
        // Sem combinação na lista, o codec da faixa original (a não PT).
        let mixed = WEB_DL.replacen("E-AC-3", "AAC LC", 1);
        assert_eq!(audio_codec(&read_mediainfo(&mixed)), Ok("E-AC-3"));
    }

    #[test]
    fn imdb_do_site_de_filme_sem_os_n_a() {
        let lookup = read_lookup(&movie_lookup()).unwrap();
        assert_eq!(lookup.kind, "movie");
        assert_eq!(lookup.title.as_deref(), Some("The Movie"));
        assert_eq!(lookup.brazilian_title.as_deref(), Some("O Filme"));
        assert_eq!(lookup.director, None);
        assert_eq!(lookup.rating, None);
        assert_eq!(lookup.runtime_minutes, Some(93));
        assert_eq!(
            lookup.poster.as_deref(),
            Some("https://image.tmdb.org/t/p/original/abc.jpg")
        );
        assert_eq!(lookup.language, Some("Inglês"));
        assert_eq!(lookup.series_imdb, None);

        let draft = draft_from(
            &package(
                "Filme.2015.1080p.AMZN.WEB-DL.H.264-GRP",
                WEB_DL,
                "dual",
                false,
            ),
            &lookup,
            "abc123".into(),
        );
        assert!(draft.faltando.is_empty(), "{:?}", draft.faltando);
        let value = |name: &str| {
            draft
                .fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.value.as_str())
        };
        assert_eq!(value("type"), Some("0"));
        assert_eq!(value("adulto"), Some("2"));
        assert_eq!(value("validimdb"), Some("yes"));
        assert_eq!(value("imdbrating"), Some(""));
        assert_eq!(value("diretorserie"), Some(""));
        assert_eq!(
            (value("duracaoHR"), value("duracaoMIN")),
            (Some("1"), Some("33"))
        );
        assert_eq!(value("tags"), Some("Animation, Adventure"));
        // A sinopse PT do pacote vale mais que a do site.
        assert_eq!(value("sinopse"), Some("Sinopse do pacote."));
        assert_eq!(value("fichatecnica"), Some(WEB_DL));
        assert_eq!(
            (draft.nome.as_str(), draft.ano.as_str()),
            ("The Movie", "2015")
        );
        assert!(draft.capa);
    }

    #[test]
    fn imdb_do_site_de_episodio_usa_a_serie() {
        let value = json!({
            "Title": "Pilot", "Year": "2019–2023", "Released": "12 Nov 2019", "Runtime": "N/A",
            "Genre": "Drama", "Director": "Diretora", "Writer": "Roteirista", "Actors": "A, B",
            "Country": "N/A", "Rated": "TV-14", "imdbRating": "8.0", "seriesID": "tt7654321",
            "Season": "1", "Episode": "1", "Type": "episode", "Response": "True",
            "seriesData": {"Title": "The Show", "totalSeasons": "3"},
            "tmdbSeriesData": {"tv_results": [{
                "name": "O Show", "overview": "Sinopse da série.",
                "poster_path": "https://image.tmdb.org/t/p/w500/x.jpg", "original_language": "ja"
            }]}
        });
        let lookup = read_lookup(&value).unwrap();
        assert_eq!(lookup.title.as_deref(), Some("The Show"));
        assert_eq!(lookup.brazilian_title.as_deref(), Some("O Show"));
        assert_eq!(lookup.series_imdb.as_deref(), Some("tt7654321"));
        assert_eq!(lookup.year.as_deref(), Some("2019"));
        assert_eq!(lookup.country, None);
        assert_eq!(lookup.runtime_minutes, None);
        assert_eq!(lookup.seasons.as_deref(), Some("3"));
        assert_eq!(lookup.language, Some("Japonês"));

        let mut episode = package(
            "Show.S01E01.1080p.NF.WEB-DL.H.264-GRP",
            WEB_DL,
            "dual",
            false,
        );
        episode.series = true;
        episode.season = Some(1);
        episode.episode = Some(1);
        episode.synopsis = String::new();
        assert_eq!(
            lookup_path(&episode, "abc"),
            "/ajax.php?action=imdb_title&category=tv&imdbid=tt1234567&auth=abc&season=1&episode=1"
        );
        let draft = draft_from(&episode, &lookup, "abc".into());
        assert!(draft.faltando.is_empty(), "{:?}", draft.faltando);
        let value = |name: &str| {
            draft
                .fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.value.as_str())
        };
        assert_eq!(value("type"), Some("1"));
        assert_eq!(value("tipo"), Some("episode"));
        assert_eq!(value("imdblink"), Some("tt7654321"));
        assert_eq!(value("diretor"), Some("Roteirista"));
        assert_eq!(value("avaliacao"), Some("TV-14"));
        assert_eq!(value("release"), Some("Netflix"));
        // Sem duração no IMDB, a do laudo (5400 s).
        assert_eq!(
            (value("duracaoHR"), value("duracaoMIN")),
            (Some("1"), Some("30"))
        );
        assert_eq!(value("sinopse"), Some("Sinopse da série."));
        assert_eq!(value("adulto"), None);

        // Filme no IMDB para um pacote de série impede o envio.
        let movie = read_lookup(&movie_lookup()).unwrap();
        let draft = draft_from(&episode, &movie, "abc".into());
        assert!(
            draft.faltando[0].contains("diz que a obra é filme"),
            "{:?}",
            draft.faltando
        );

        let missing = read_lookup(&json!({"Response": "False", "Error": "Incorrect IMDb ID."}));
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("Incorrect IMDb ID.")
        );
    }

    #[test]
    fn auth_do_formulario_ou_sessao_caida() {
        let page = r#"<form id="upload_table"><input type="hidden" name="submit" value="true"> <input type="hidden" name="auth" value="0a1b2c3d"></form>"#;
        assert_eq!(auth_from_page(page).unwrap(), "0a1b2c3d");
        let login = r#"<form action="login.php"><input type="text" name="username"></form>"#;
        assert!(
            auth_from_page(login)
                .unwrap_err()
                .to_string()
                .contains("sessão")
        );
        assert!(auth_from_page(r#"<input name="auth" value="">"#).is_err());
    }

    #[test]
    fn corpo_do_envio_como_o_navegador_manda() {
        let lookup = read_lookup(&movie_lookup()).unwrap();
        let draft = draft_from(
            &package("Filme.2015.1080p.WEB-DL.H.264-GRP", WEB_DL, "dual", false),
            &lookup,
            "abc123".into(),
        );
        let parts = parts(
            &draft,
            b"d4:infoe".to_vec(),
            "x.torrent",
            &["https://img/1.png".into(), "https://img/2.png".into()],
        );
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names[..3], ["submit", "auth", "file_input"]);
        assert_eq!(names.iter().filter(|n| **n == "screenshots[]").count(), 2);
        assert!(!names.contains(&"anonymous"));
        assert!(matches!(
            &parts[1].value,
            acervo_indexers::SitePartValue::Text(t) if t == "abc123"
        ));
    }

    /// Duas linhas do grupo como o BJ as escreve: um release de um arquivo só
    /// (com a extensão e o apóstrofo escapado) e um de pasta.
    const GROUP_ROWS: &str = r#"<tr class="torrent_row releases_0 groupid_445908 group_torrent "
        style="font-weight: normal; " id="torrent770856" data-torrentname="Now.You.See.Me.Now.You.Don&#039;t.2025.UHD.BluRay.2160p.REMUX-FraMeSToR.mkv">
        <td><span>[ <a href="torrents.php?action=download&amp;id=770856">DL</a></span></td></tr>
        <tr class="torrent_row releases_0 groupid_445908 group_torrent "
        style="font-weight: normal; " id="torrent815102" data-torrentname="Now.You.See.Me.Now.You.Dont.2025.UHD.BluRay.2160p.x265-hallowed">
        <td>x</td></tr>"#;

    #[test]
    fn envio_sem_redirect_e_achado_pelo_nome_no_grupo() {
        assert_eq!(
            uploaded_id(
                GROUP_ROWS,
                "Now.You.See.Me.Now.You.Dont.2025.UHD.BluRay.2160p.x265-hallowed"
            ),
            Some("815102".into())
        );
        // O nome com apóstrofo vem escapado no atributo.
        assert_eq!(
            uploaded_id(
                GROUP_ROWS,
                "Now.You.See.Me.Now.You.Don't.2025.UHD.BluRay.2160p.REMUX-FraMeSToR.mkv"
            ),
            Some("770856".into())
        );
        // Prefixo de outro nome não é o release.
        assert_eq!(
            uploaded_id(GROUP_ROWS, "Now.You.See.Me.Now.You.Dont.2025.UHD"),
            None
        );
        assert_eq!(uploaded_id(GROUP_ROWS, "Outro.Filme.2025"), None);
        // O id tem de estar na mesma tag do nome.
        let apart = r#"<tr id="torrent1"></tr><tr data-torrentname="Filme">"#;
        assert_eq!(uploaded_id(apart, "Filme"), None);

        assert_eq!(
            group_path("https://bj-share.info/torrents.php?id=445908"),
            Some("/torrents.php?id=445908".into())
        );
        assert_eq!(
            group_path("torrents.php?id=540653"),
            Some("/torrents.php?id=540653".into())
        );
        assert_eq!(group_path("torrents.php?searchstr=tt1"), None);
        assert_eq!(group_path("login.php"), None);
    }

    #[test]
    fn resposta_do_envio_da_o_id_ou_o_motivo() {
        let response = |status, location: Option<&str>, body: &str| SiteResponse {
            url: "https://site.invalid/upload.php".to_owned(),
            status,
            location: location.map(str::to_owned),
            body: body.as_bytes().to_vec(),
        };
        assert_eq!(
            created(&response(
                302,
                Some("torrents.php?id=123&torrentid=456"),
                ""
            )),
            Ok("456".into())
        );
        assert_eq!(
            created(&response(
                302,
                Some("https://bj-share.info/torrents.php?id=123"),
                ""
            )),
            Ok("123".into())
        );
        let form = r#"<div class="thin"><p style="color: red; text-align: center;">O torrent j&aacute; existe: <a href="torrents.php?id=9">aqui</a> &amp; ali</p><form id="upload_table">"#;
        let refused = created(&response(200, None, form)).unwrap_err();
        assert!(refused.certain);
        assert_eq!(
            refused.message,
            "o BJ recusou o envio: O torrent j&aacute; existe: aqui & ali"
        );
        // Formulário sem o erro lido: não dá para afirmar que foi recusa.
        let back = created(&response(200, None, r#"<form id="upload_table">"#)).unwrap_err();
        assert!(!back.certain && back.message.contains("sem dizer o motivo"));
        let upload = created(&response(302, Some("upload.php?authkey=segredo"), "")).unwrap_err();
        assert!(!upload.certain && !upload.message.contains("segredo"));
        let other = created(&response(
            302,
            Some("https://bj-share.info/index.php?torrent_pass=segredo"),
            "",
        ))
        .unwrap_err();
        assert!(
            !other.certain
                && other.message.contains("index.php,")
                && !other.message.contains("segredo")
        );
        // Sem dar para ler a resposta, o torrent pode ter sido criado.
        let unknown = created(&response(500, None, "")).unwrap_err();
        assert!(!unknown.certain && unknown.message.contains("HTTP 500"));
        assert!(
            !created(&response(200, None, "<html>ok</html>"))
                .unwrap_err()
                .certain
        );
        assert!(
            !created(&response(302, Some("index.php"), ""))
                .unwrap_err()
                .certain
        );
        // A página que não é o formulário leva o título à mensagem.
        let page = "<html><head><TITLE>\n  Envio  :: BJ-Share\n</TITLE></head><body>x</body>";
        let titled = created(&response(200, None, page)).unwrap_err();
        assert!(!titled.certain);
        assert_eq!(
            titled.message,
            "resposta inesperada do BJ ao envio (HTTP 200, página \"Envio :: BJ-Share\")"
        );
    }

    const DV_ATMOS: &str = "General
Format                                   : Matroska

Video
Format                                   : HEVC
Bit depth                                : 10 bits
HDR format                               : Dolby Vision, Version 1.0, Profile 8.1, dolby-vision, and HDR10 compatible
Transfer characteristics                 : PQ
Width                                    : 3 840 pixels
Height                                   : 1 600 pixels

Audio #1
Format                                   : E-AC-3
Format/Info                              : Enhanced AC-3
Commercial name                          : Dolby Digital Plus with Dolby Atmos
Format additional features               : JOC
Language                                 : English
";

    #[test]
    fn web_dl_dv_hdr10_atmos_ultrawide_leva_as_etiquetas() {
        let package = package(
            "Filme.2026.2160p.NF.WEB-DL.DDP5.1.Atmos.DV.HDR10.H.265-GRP",
            DV_ATMOS,
            "legendado",
            false,
        );
        let mut form = Form::default();
        technical(&mut form, &package, &read_mediainfo(DV_ATMOS), Ok("Inglês"));
        assert!(form.missed.is_empty(), "{:?}", form.missed);
        assert_eq!(
            sent(&form)["remaster_title"],
            "10-bit / Dolby Vision / HDR10 / Dolby Atmos / Ultrawide"
        );
        // HDR10+ não leva também HDR10; 1920x800 também é ultrawide.
        let plus = DV_ATMOS
            .replace(
                "Dolby Vision, Version 1.0, Profile 8.1, dolby-vision, and HDR10 compatible",
                "SMPTE ST 2094 App 4, Version 1, HDR10+ Profile B compatible",
            )
            .replace("3 840", "1 920")
            .replace("1 600", "800");
        assert_eq!(
            remaster(
                &tokens("Filme.2026.2160p.WEB-DL-GRP"),
                &read_mediainfo(&plus)
            ),
            "10-bit / HDR10+ / Dolby Atmos / Ultrawide"
        );
        // Mais de 80 caracteres bloqueia.
        let long = package_with_long_edition();
        let mut form = Form::default();
        technical(&mut form, &long, &read_mediainfo(DV_ATMOS), Ok("Inglês"));
        assert!(
            form.missed.iter().any(|m| m.contains("80 caracteres")),
            "{:?}",
            form.missed
        );
    }

    fn package_with_long_edition() -> Package {
        package(
            "Filme.2026.Directors.Cut.Extended.IMAX.Open.Matte.Theatrical.Final.Cut.Unrated.Uncut.Uncensored.Colorized.Noir.Edition.2160p.BluRay.REMUX.H.265-GRP",
            DV_ATMOS,
            "dual",
            false,
        )
    }

    #[test]
    fn remux_leva_remux_primeiro_e_noir_edition() {
        let words = tokens("Filme.2010.Noir.Edition.1080p.BluRay.REMUX.AVC-GRP");
        assert_eq!(
            remaster(&words, &read_mediainfo(REMUX)),
            "Remux / Noir Edition"
        );
    }

    #[test]
    fn codec_de_video_pela_familia_do_nome_e_grafia_da_qualidade() {
        let words = |name: &str| {
            let package = package(name, WEB_DL, "dual", false);
            let mut form = Form::default();
            technical(&mut form, &package, &read_mediainfo(WEB_DL), Ok("Inglês"));
            sent(&form)["codecvideo"].to_owned()
        };
        assert_eq!(
            words("Filme.2024.1080p.AMZN.WEB-DL.DDP5.1.x264-GRP"),
            "H.264"
        );
        assert_eq!(words("Filme.2024.1080p.BluRay.H.264-GRP"), "x264");
        assert_eq!(words("Filme.2024.1080p.WEBRip.H.265-GRP"), "x265");
        assert_eq!(words("Filme.2024.1080p.WEB-DL.AV1-GRP"), "AV1");
        // Sem nada no nome, o Format do MediaInfo (AVC) e a qualidade.
        assert_eq!(words("Filme.2024.1080p.WEB-DL-GRP"), "H.264");
        assert_eq!(words("Filme.2024.1080p.HDTV-GRP"), "x264");
    }

    #[test]
    fn bordas_da_revisao() {
        // REMUX sem BluRay no nome é de disco; de DVD bloqueia.
        assert_eq!(
            quality(&tokens("Filme.2019.2160p.UHD.REMUX.HEVC-G")),
            Ok("Blu-ray")
        );
        assert_eq!(
            quality(&tokens("Filme.2010.1080p.REMUX.AVC-G")),
            Ok("Blu-ray")
        );
        assert!(
            quality(&tokens("Filme.2001.DVD9.REMUX-G"))
                .unwrap_err()
                .contains("DVD")
        );
        assert!(
            quality(&tokens("Filme.2001.DVDRemux-G"))
                .unwrap_err()
                .contains("DVD")
        );
        // Edição só depois do ano ou da temporada.
        let media = Media::default();
        assert!(
            !remaster(&tokens("Extended.Family.S01.1080p.WEB-DL.H.264-G"), &media)
                .contains("Extended")
        );
        // O codec não sai do título.
        assert_eq!(
            video_codec("MPEG2.Story.2020.1080p.WEB-DL.H.265-G", &media, true),
            Ok("H.265")
        );
    }

    #[test]
    fn anime_bloqueia_o_envio() {
        let mut value = movie_lookup();
        value["tmdbData"]["movie_results"][0]["original_language"] = json!("ja");
        let lookup = read_lookup(&value).unwrap();
        let pack = package(
            "Filme.2015.1080p.AMZN.WEB-DL.H.264-GRP",
            WEB_DL,
            "dual",
            false,
        );
        let draft = draft_from(&pack, &lookup, "abc".into());
        assert!(
            draft
                .faltando
                .iter()
                .any(|m| m.contains("anime vai na categoria Animes do BJ")),
            "{:?}",
            draft.faltando
        );
        // Animação em inglês não é anime.
        let draft = draft_from(&pack, &read_lookup(&movie_lookup()).unwrap(), "abc".into());
        assert!(draft.faltando.is_empty(), "{:?}", draft.faltando);
    }

    #[test]
    fn idioma_sem_opcao_cita_o_codigo_e_portugal_tem_opcao_propria() {
        let mut value = movie_lookup();
        value["tmdbData"]["movie_results"][0]["original_language"] = json!("sr");
        let lookup = read_lookup(&value).unwrap();
        assert_eq!(
            lookup.language_choice(),
            Err("idioma original 'sr' sem opção na lista do BJ".to_owned())
        );
        value["tmdbData"]["movie_results"][0]["original_language"] = json!("pt");
        assert_eq!(read_lookup(&value).unwrap().language, Some("Português"));
        value["Country"] = json!("Portugal, France");
        assert_eq!(
            read_lookup(&value).unwrap().language,
            Some("Português (pt)")
        );
        // Coprodução brasileira com Portugal é português do Brasil.
        value["Country"] = json!("Brazil, Portugal");
        assert_eq!(read_lookup(&value).unwrap().language, Some("Português"));
        // Sem idioma nenhum, a mensagem antiga.
        let mut none = movie_lookup();
        none["tmdbData"]["movie_results"][0]["original_language"] = Value::Null;
        none["Language"] = json!("N/A");
        assert_eq!(
            read_lookup(&none).unwrap().language_choice(),
            Err("o IMDB do BJ não disse o idioma original".to_owned())
        );
        // Nesse caso vale o idioma do catálogo.
        let mut pack = package(
            "Filme.2015.1080p.AMZN.WEB-DL.H.264-GRP",
            WEB_DL,
            "dual",
            false,
        );
        let lookup = read_lookup(&none).unwrap();
        let draft = draft_from(&pack, &lookup, "abc".into());
        assert!(
            draft
                .faltando
                .iter()
                .any(|m| m.starts_with("Idioma original")),
            "{:?}",
            draft.faltando
        );
        pack.original_language = Some("Portuguese (Brazil)".into());
        let draft = draft_from(&pack, &lookup, "abc".into());
        assert!(
            !draft
                .faltando
                .iter()
                .any(|m| m.starts_with("Idioma original")),
            "{:?}",
            draft.faltando
        );
    }

    #[test]
    fn elenco_data_e_criador_sao_obrigatorios() {
        let mut value = movie_lookup();
        value["Actors"] = json!("N/A");
        let pack = package(
            "Filme.2015.1080p.AMZN.WEB-DL.H.264-GRP",
            WEB_DL,
            "dual",
            false,
        );
        let draft = draft_from(&pack, &read_lookup(&value).unwrap(), "abc".into());
        assert!(
            draft.faltando.iter().any(|m| m.starts_with("Elenco")),
            "{:?}",
            draft.faltando
        );
        value["Released"] = json!("N/A");
        let draft = draft_from(&pack, &read_lookup(&value).unwrap(), "abc".into());
        assert!(
            draft
                .faltando
                .iter()
                .any(|m| m.starts_with("Data de lançamento"))
        );
        // Série: Writer vazio bloqueia como "Criador".
        let mut show = movie_lookup();
        show["Type"] = json!("series");
        show["Writer"] = json!("N/A");
        let mut episode = pack.clone();
        episode.series = true;
        episode.season = Some(1);
        let draft = draft_from(&episode, &read_lookup(&show).unwrap(), "abc".into());
        assert!(
            draft.faltando.iter().any(|m| m.starts_with("Criador")),
            "{:?}",
            draft.faltando
        );
    }

    #[test]
    fn fonte_so_depois_do_ano_e_remux_de_dvd_bloqueia() {
        let q = |name: &str| quality(&tokens(name));
        assert_eq!(q("Charlotte's.Web.2006.720p.HDTV.x264-GRP"), Ok("HDTV"));
        assert_eq!(q("Show.Web.S01E01.1080p.NF.WEB-DL-GRP"), Ok("WEB-DL"));
        assert_eq!(q("Filme.2010.1080p.BDRemux.AVC-GRP"), Ok("Blu-ray"));
        assert_eq!(q("Filme.2010.1080p.BluRay.REMUX.AVC-GRP"), Ok("Blu-ray"));
        assert_eq!(
            q("Filme.2003.DVD.REMUX.MPEG-2-GRP"),
            Err("REMUX de DVD: escolha a qualidade à mão".to_owned())
        );
    }
}

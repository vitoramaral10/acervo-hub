//! Nome de release de episódio: título da série, temporada, episódios (ou
//! numeração absoluta, ou data de exibição), qualidade, idiomas e grupo.
//!
//! Porte do `ParseTitle` e do `ParsePath` do gerenciador de séries: o mesmo
//! pré-processamento, a tabela `ReportTitleRegex` na mesma ordem e a mesma
//! leitura do casamento. Qualidade, idioma e grupo têm variante própria para
//! episódio (`quality_episode.rs`, `language.rs`, `group.rs`): o parser de
//! séries não lê igual ao de filmes.
//!
//! Onde o porte diverge do original:
//! - número de temporada, episódio ou absoluto que não cabe em `u16` (o
//!   original usa `int`, e há padrão de 5 dígitos) faz o título não ser lido;
//! - a data de hoje, que barra episódio no futuro, é a de UTC;
//! - o diretório de `parse_episode_path` vem só do próprio texto: caminho sem
//!   `/` não tem diretório (o .NET usaria o diretório de trabalho).

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};
use unicode_normalization::UnicodeNormalization;

use crate::common::{
    captures, group, is_match, path_extension, regex, remove_episode_file_extension, replace_all,
    strip_torrent_suffix, strip_website_postfix, strip_website_prefix,
};
use crate::episode_table::{PRE_SUBSTITUTION, REPORT_TITLE};
use crate::group::parse_episode_release_group;
use crate::language::{Language, parse_episode_languages};
use crate::quality::{Quality, QualityModel};
use crate::quality_episode::{parse_episode_quality, parse_quality_name};
use crate::repeat::{Match, Pattern, Span};

/// Os títulos que a série tem no nome do release.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesTitleInfo {
    pub title: String,
    pub title_without_year: String,
    pub year: Option<u16>,
    /// Vazio quando o nome traz um título só; senão todos, o principal junto.
    pub all_titles: Vec<String>,
}

/// O que se leu de um nome de release de episódio. Os `bool` são os da
/// referência, um a um.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ParsedEpisode {
    pub release_title: String,
    pub series_title: String,
    pub series_title_info: SeriesTitleInfo,
    pub season: u16,
    /// A faixa inteira entre o primeiro e o último número do nome: "E05-E08"
    /// dá 5, 6, 7 e 8.
    pub episodes: Vec<u16>,
    pub absolute_episodes: Vec<u16>,
    /// Absoluto com parte decimal (12.5): especial de anime.
    pub special_absolute_episodes: Vec<f64>,
    /// `AAAA-MM-DD`, nos programas diários.
    pub air_date: Option<String>,
    pub daily_part: Option<u8>,
    pub full_season: bool,
    pub partial_season: bool,
    pub multi_season: bool,
    pub season_extra: bool,
    pub split_episode: bool,
    pub mini_series: bool,
    pub special: bool,
    pub season_part: u16,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub release_hash: Option<String>,
    /// O que sobra do nome depois da temporada e do episódio: é onde se
    /// procuram os idiomas.
    pub release_tokens: String,
}

impl ParsedEpisode {
    fn empty(release_title: &str) -> Self {
        Self {
            release_title: release_title.to_owned(),
            series_title: String::new(),
            series_title_info: SeriesTitleInfo::default(),
            season: 0,
            episodes: Vec::new(),
            absolute_episodes: Vec::new(),
            special_absolute_episodes: Vec::new(),
            air_date: None,
            daily_part: None,
            full_season: false,
            partial_season: false,
            multi_season: false,
            season_extra: false,
            split_episode: false,
            mini_series: false,
            special: false,
            season_part: 0,
            quality: QualityModel {
                quality: Quality::Unknown,
                revision: crate::quality::Revision::default(),
            },
            languages: Vec::new(),
            release_group: None,
            release_hash: None,
            release_tokens: String::new(),
        }
    }

    #[must_use]
    pub fn is_daily(&self) -> bool {
        self.air_date.is_some()
    }

    #[must_use]
    pub fn is_absolute_numbering(&self) -> bool {
        !self.absolute_episodes.is_empty()
    }
}

static PRE_SUBSTITUTIONS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    PRE_SUBSTITUTION
        .iter()
        .map(|(pattern, template)| (regex(&format!("(?i){pattern}")), *template))
        .collect()
});

static REPORT_TITLES: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    REPORT_TITLE
        .iter()
        .map(|pattern| Pattern::new(pattern))
        .collect()
});

static REJECT_HASHED: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^[0-9a-zA-Z]{32}",
        r"^[a-z0-9]{24}$",
        r"^[A-Z]{11}\d{3}$",
        r"^[a-z]{12}\d{3}$",
        r"^Backup_\d{5,}S\d{2}-\d{2}$",
        r"^123$",
        r"(?i)^abc$",
        r"(?i)^abc[-_. ]xyz",
        r"(?i)^b00bs$",
        r"^\d{6}_\d{2}$",
        r"^[0-9a-zA-Z]{30}",
        r"^[0-9a-zA-Z]{26}",
        r"^[0-9a-zA-Z]{39}",
        r"^[0-9a-zA-Z]{24}",
    ]
    .iter()
    .map(|pattern| regex(pattern))
    .collect()
});

static SEASON_FOLDER_REJECT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^(Season[ ._-]*\d+|Specials)$"));

/// Diferente do de filmes: também pega "01E02S", o nome escrito ao contrário.
static REVERSED_TITLE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?:^|[-._ ])(p027|p0801|\d{2,3}E\d{2}S)[-._ ]"));

static SIMPLE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?:(480|540|576|720|1080|2160)[ip]|[xh][\W_]?26[45]|DD\W?5\W1|[<>?*]|848x480|1280x720|1920x1080|3840x2160|4096x2160|(?<![a-f0-9])(8|10)(b(?![a-z0-9])|bit)|10-bit)\s*?",
    )
});

static CLEAN_QUALITY_BRACKETS: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\[[a-z0-9 ._-]+\]$"));

static SIX_DIGIT_AIR_DATE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?<=[_.-])(?<airdate>(?<!\d)(?<airyear>[1-9]\d{1})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9]))(?=[_.-])",
    )
});

static REQUEST_INFO: LazyLock<Regex> = LazyLock::new(|| regex(r"^(?:\[.+?\])+"));

static YEAR_IN_TITLE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)^(?<title>.+?)[-_. ]+?[\(\[]?(?<year>\d{4})[\]\)]?"));

static TITLE_COMPONENTS: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^(?:(?<title>.+?) \((?<title_2>.+?)\)|(?<title_3>.+?) \| (?<title_4>.+?)|(?<title_5>.+?) AKA (?<title_6>.+?))$",
    )
});

static SEASON_FOLDER: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^(?:S|Season|Saison|Series|Stagione)[-_. ]*(?<season>(?<!\d+)\d{1,4}(?!\d+))(?:[_. ]+(?!\d+)|$)",
    )
});

static SIMPLE_EPISODE_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^[ex]?(?<episode>(?<!\d+)\d{1,3}(?!\d+))(?:[ex-](?<episode_2>(?<!\d+)\d{1,3}(?!\d+)))?(?:[_. ](?!\d+)(?<remaining>.+)|$)",
    )
});

const NUMBERS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

/// Troca feita antes da tabela, com o molde `${grupo}` ou `$1` do original.
/// Nome repetido no padrão (`episode`, `episode_2`) vale o que casou.
fn expand(template: &str, found: &Captures<'_, str>, regex: &Regex) -> String {
    let named = |name: &str| -> String {
        regex
            .capture_names()
            .flatten()
            .filter(|candidate| {
                *candidate == name
                    || candidate
                        .strip_prefix(name)
                        .and_then(|rest| rest.strip_prefix('_'))
                        .is_some_and(|digits| digits.chars().all(|c| c.is_ascii_digit()))
            })
            .find_map(|candidate| found.name(candidate))
            .map_or_else(String::new, |m| m.as_str().to_owned())
    };
    let mut out = String::new();
    let mut rest = template;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        if let Some(inner) = rest.strip_prefix('{') {
            let end = inner.find('}').unwrap_or(inner.len());
            out.push_str(&named(&inner[..end]));
            rest = &inner[(end + 1).min(inner.len())..];
        } else {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            let index: usize = rest[..digits].parse().unwrap_or(0);
            out.push_str(found.get(index).map_or("", |m| m.as_str()));
            rest = &rest[digits..];
        }
    }
    out.push_str(rest);
    out
}

/// As substituições que antecedem a tabela. Com `first_only`, só até a
/// primeira que casa (o que `ParseReleaseGroup` faz).
pub(crate) fn pre_substitute(title: &str, first_only: bool) -> String {
    let mut title = title.to_owned();
    for (pattern, template) in PRE_SUBSTITUTIONS.iter() {
        let matched = is_match(pattern, &title);
        title = pattern
            .replace_all(&title, |found: &Captures<'_, str>| {
                expand(template, found, pattern)
            })
            .into_owned();
        if first_only && matched {
            break;
        }
    }
    title
}

/// Lê um nome de release de episódio. `None` quando não dá para afirmar
/// nem o título da série: nome embaralhado, só símbolos, pasta de temporada,
/// ou nenhum padrão reconhece.
#[must_use]
pub fn parse_episode_title(title: &str) -> Option<ParsedEpisode> {
    if !valid_before_parsing(title) {
        return None;
    }
    let title = unreverse(title);

    let release_title = remove_episode_file_extension(&title)
        .replace('【', "[")
        .replace('】', "]");
    let release_title = pre_substitute(&release_title, false);

    let simple_title = replace_all(&SIMPLE_TITLE, &release_title, "");
    let simple_title = strip_website_prefix(&simple_title);
    let simple_title = strip_website_postfix(&simple_title);
    let simple_title = strip_torrent_suffix(&simple_title);
    let simple_title = CLEAN_QUALITY_BRACKETS
        .replace_all(&simple_title, |c: &Captures<'_, str>| {
            if parse_quality_name(&c[0]).quality == Quality::Unknown {
                c[0].to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    let simple_title = fix_six_digit_air_date(simple_title);

    for pattern in REPORT_TITLES.iter() {
        let matches = pattern.matches(&simple_title);
        if matches.is_empty() {
            continue;
        }
        match parse_match_collection(&matches, &simple_title, &release_title) {
            Ok(mut result) => {
                if result.full_season && contains_ignore_case(&result.release_tokens, "Special") {
                    result.full_season = false;
                    result.special = true;
                }
                result.languages = parse_episode_languages(&result.release_tokens);
                result.quality = parse_episode_quality(&title);

                result.release_group = parse_episode_release_group(&release_title);
                let subgroup = matches[0].value("subgroup");
                if !subgroup.trim().is_empty() {
                    result.release_group = Some(subgroup.to_owned());
                }
                result.release_hash = release_hash(&matches[0]);
                return Some(result);
            }
            Err(Failure::Skip) => {}
            Err(Failure::Abort) => return None,
        }
    }
    None
}

/// Lê um caminho de arquivo: o nome sozinho e, quando o nome só traz o número
/// do episódio, ele junto da pasta da temporada ou da pasta da série.
#[must_use]
pub fn parse_episode_path(path: &str) -> Option<ParsedEpisode> {
    let (directory, name) = match path.rsplit_once('/') {
        Some(("", name)) => (Some("/"), name),
        Some((parent, name)) => (parent.rsplit('/').next(), name),
        None => (None, path),
    };
    let mut result = parse_episode_title(name);

    // Pasta e arquivo lidos em separado, mas juntos quando os dois valem.
    if let (Some(directory), Some(numbers)) = (directory, captures(&SIMPLE_EPISODE_NUMBER, name))
        && result
            .as_ref()
            .is_none_or(|r| r.mini_series || !r.absolute_episodes.is_empty())
        && let Some(season) = captures(&SEASON_FOLDER, directory)
            .and_then(|found| group(&found, "season").map(str::to_owned))
    {
        let first = group(&numbers, "episode")?;
        let last = group(&numbers, "episode_2").unwrap_or(first);
        let (first, last) = (parse_number(first).ok()?, parse_number(last).ok()?);
        let range = if first == last {
            String::new()
        } else {
            format!("-E{last:02}")
        };
        let remaining = group(&numbers, "remaining").map_or_else(String::new, |r| format!(" {r}"));
        return parse_episode_title(&format!("S{season}E{first:02}{range}{remaining}"));
    }

    let Some(directory) = directory else {
        return result;
    };
    let stem = name.rfind('.').map_or(name, |dot| &name[..dot]);
    if result.is_none()
        && let Ok(number) = stem.trim().parse::<i32>()
    {
        result = parse_episode_title(directory);
        let number = u16::try_from(number).ok();
        result = match (result, number) {
            (Some(mut found), Some(number)) if found.absolute_episodes.contains(&number) => {
                found.absolute_episodes = vec![number];
                Some(found)
            }
            (Some(mut found), Some(number)) if found.episodes.contains(&number) => {
                found.episodes = vec![number];
                Some(found)
            }
            _ => None,
        };
    }
    if result.is_none() {
        result = parse_episode_title(&format!("{directory} {name}"));
    }
    if result.is_none() {
        result = parse_episode_title(&format!("{directory}{}", path_extension(name)));
    }
    result
}

/// `Skip` descarta o padrão e segue para o próximo; `Abort` encerra a leitura
/// (data inválida, número que não é número).
enum Failure {
    Skip,
    Abort,
}

fn valid_before_parsing(title: &str) -> bool {
    let lower = title.to_lowercase();
    if lower.contains("password") && lower.contains("yenc") {
        return false;
    }
    if !title.chars().any(char::is_alphanumeric) {
        return false;
    }
    let without_extension = remove_episode_file_extension(title);
    !REJECT_HASHED
        .iter()
        .any(|pattern| is_match(pattern, &without_extension))
        && !is_match(&SEASON_FOLDER_REJECT, &without_extension)
}

/// Nome escrito de trás para frente ("p0801" é "1080p" ao contrário).
fn unreverse(title: &str) -> String {
    if !is_match(&REVERSED_TITLE, title) {
        return title.to_owned();
    }
    let without_extension = remove_episode_file_extension(title);
    let extension = &title[without_extension.len()..];
    without_extension.chars().rev().collect::<String>() + extension
}

/// "180412" entre separadores vira "2018.04.12", para os padrões de data.
fn fix_six_digit_air_date(simple_title: String) -> String {
    let Some(found) = captures(&SIX_DIGIT_AIR_DATE, &simple_title) else {
        return simple_title;
    };
    let (Some(date), Some(year), Some(month), Some(day)) = (
        group(&found, "airdate"),
        group(&found, "airyear"),
        group(&found, "airmonth"),
        group(&found, "airday"),
    ) else {
        return simple_title;
    };
    if month == "00" && day == "00" {
        return simple_title;
    }
    simple_title.replace(date, &format!("20{year}.{month}.{day}"))
}

fn release_hash(first: &Match<'_, '_>) -> Option<String> {
    let hash = first
        .value("hash")
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')'));
    (!hash.is_empty() && hash != "1280x720").then(|| hash.to_owned())
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Posição de byte de um índice em UTF-16; no meio de um par substituto,
/// o próximo caractere.
fn utf16_to_byte(text: &str, index: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units >= index {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// Primeiro código de cada bloco de dígitos decimais Unicode (categoria Nd)
/// que o NFKC não leva para ASCII: o dígito vale o deslocamento no bloco.
const DIGIT_ZEROS: [u32; 54] = [
    0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6, 0x0D66,
    0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x16A60, 0x16AC0,
];

/// `Normalize(FormKC)` seguido de `ConvertToNumerals`: dígitos de qualquer
/// escrita viram ASCII.
fn to_ascii_digits(value: &str) -> String {
    value
        .nfkc()
        .map(|c| {
            let code = u32::from(c);
            DIGIT_ZEROS
                .iter()
                .find(|zero| (**zero..**zero + 10).contains(&code))
                .and_then(|zero| char::from_digit(code - zero, 10))
                .unwrap_or(c)
        })
        .collect()
}

/// Número como o original lê: dígitos de qualquer escrita ou o número por
/// extenso até "nine".
fn parse_number(value: &str) -> Result<u32, Failure> {
    if let Ok(number) = to_ascii_digits(value).trim().parse::<u32>() {
        return Ok(number);
    }
    NUMBERS
        .iter()
        .position(|word| word.eq_ignore_ascii_case(value))
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(Failure::Abort)
}

fn parse_decimal(value: &str) -> Result<f64, Failure> {
    to_ascii_digits(value)
        .trim()
        .parse::<f64>()
        .map_err(|_| Failure::Abort)
}

fn to_u16(value: u32) -> Result<u16, Failure> {
    u16::try_from(value).map_err(|_| Failure::Abort)
}

/// Do primeiro ao último número, inclusive.
fn range(first: u32, last: u32) -> Result<Vec<u16>, Failure> {
    (first..=last).map(to_u16).collect()
}

fn parse_match_collection(
    matches: &[Match<'_, '_>],
    simple_title: &str,
    release_title: &str,
) -> Result<ParsedEpisode, Failure> {
    let first_match = &matches[0];
    let end16 = |span: Span| utf16_len(&simple_title[..span.end]);

    let series_name = first_match.value("title").replace(['.', '_'], " ");
    let series_name = replace_all(&REQUEST_INFO, &series_name, "")
        .trim_matches(' ')
        .to_owned();
    let air_year: i64 = first_match.value("airyear").parse().unwrap_or(0);
    let mut last_index = first_match.last("title").map_or(0, end16);

    let mut result = ParsedEpisode::empty(release_title);
    if air_year < 1900 {
        read_episodes(matches, &end16, &mut result, &mut last_index)?;
    } else {
        read_air_date(first_match, air_year, &end16, &mut result, &mut last_index)?;
    }

    result.release_tokens = if last_index < utf16_len(release_title) {
        release_title[utf16_to_byte(release_title, last_index)..].to_owned()
    } else {
        release_title.to_owned()
    };
    result.series_title = series_name;
    result.series_title_info = series_title_info(&result.series_title, first_match);
    Ok(result)
}

/// Temporada e episódios (ou numeração absoluta, ou pacote de temporada).
fn read_episodes(
    matches: &[Match<'_, '_>],
    end16: &dyn Fn(Span) -> usize,
    result: &mut ParsedEpisode,
    last_index: &mut usize,
) -> Result<(), Failure> {
    let first_match = &matches[0];
    for found in matches {
        let episodes = found.spans("episode");
        let absolutes = found.spans("absoluteepisode");

        if let (Some(first), Some(last)) = (episodes.first(), episodes.last()) {
            let first_number = parse_number(found.text(*first))?;
            let last_number = parse_number(found.text(*last))?;
            if first_number > last_number {
                return Err(Failure::Skip);
            }
            result.episodes = range(first_number, last_number)?;
            *last_index = (*last_index).max(end16(*last));
            if found.success("special") {
                result.special = true;
            }
            if found.success("splitepisode") {
                result.split_episode = true;
            }
        }

        if let (Some(first), Some(last)) = (absolutes.first(), absolutes.last()) {
            let first_number = parse_decimal(found.text(*first))?;
            let last_number = parse_decimal(found.text(*last))?;
            if first_number > last_number {
                return Err(Failure::Skip);
            }
            if first_number.fract() != 0.0 || last_number.fract() != 0.0 {
                // Vários casamentos não valem para especial.
                if absolutes.len() != 1 {
                    return Err(Failure::Skip);
                }
                result.special_absolute_episodes = vec![first_number];
                result.special = true;
                *last_index = (*last_index).max(end16(*first));
            } else {
                // Os dois são inteiros aqui: o truncamento é exato.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (from, to) = (first_number as u32, last_number as u32);
                result.absolute_episodes = range(from, to)?;
                if found.success("special") {
                    result.special = true;
                }
                *last_index = (*last_index).max(end16(*last));
            }
        }

        if episodes.is_empty() && absolutes.is_empty() {
            // "Extras" e "SUBPACK" ficam marcados para serem filtrados.
            if !first_match.value("extras").trim().is_empty() {
                result.season_extra = true;
            }
            // Pacote parcial tem `seasonpart`, o que o separa da
            // temporada inteira e do episódio.
            let part = first_match.value("seasonpart");
            if part.trim().is_empty() {
                result.full_season = true;
            } else {
                result.season_part = to_u16(part.parse().map_err(|_| Failure::Abort)?)?;
                result.partial_season = true;
            }
        }

        if episodes.len() == 2
            && first_match.success("episodecount")
            && episodes.last().map(|s| found.text(*s)) == Some(first_match.value("episodecount"))
        {
            result.episodes.clear();
            result.full_season = true;
        }
    }

    let mut seasons = Vec::new();
    for span in first_match.spans("season") {
        if let Ok(season) = first_match.text(span).parse::<u32>() {
            seasons.push(to_u16(season)?);
            *last_index = (*last_index).max(end16(span));
        }
    }
    let mut distinct = seasons.clone();
    distinct.sort_unstable();
    distinct.dedup();
    // Mais de uma temporada: quem usa o resultado decide rejeitar.
    result.multi_season = distinct.len() > 1;

    if let Some(season) = seasons.first() {
        result.season = *season;
    } else if result.absolute_episodes.is_empty() && !result.episodes.is_empty() {
        // Sem temporada e sem ser só absoluto: minissérie, temporada 1.
        result.season = 1;
        result.mini_series = true;
    }

    Ok(())
}

/// Programa diário: a data de exibição no lugar de temporada e episódio.
fn read_air_date(
    first_match: &Match<'_, '_>,
    air_year: i64,
    end16: &dyn Fn(Span) -> usize,
    result: &mut ParsedEpisode,
    last_index: &mut usize,
) -> Result<(), Failure> {
    let (month, day) =
        if first_match.success("ambiguousairmonth") && first_match.success("ambiguousairday") {
            let month: i64 = first_match
                .value("ambiguousairmonth")
                .parse()
                .map_err(|_| Failure::Abort)?;
            let day: i64 = first_match
                .value("ambiguousairday")
                .parse()
                .map_err(|_| Failure::Abort)?;
            if day <= 12 && month <= 12 {
                // Dia e mês indistinguíveis.
                return Err(Failure::Abort);
            }
            (month, day)
        } else {
            let month: i64 = first_match
                .value("airmonth")
                .parse()
                .map_err(|_| Failure::Abort)?;
            let day: i64 = first_match
                .value("airday")
                .parse()
                .map_err(|_| Failure::Abort)?;
            // Mês acima de 12: dia e mês trocados no nome.
            if month > 12 {
                (day, month)
            } else {
                (month, day)
            }
        };

    if !valid_date(air_year, month, day) {
        return Err(Failure::Abort);
    }
    // Episódio no futuro é quase sempre erro de leitura.
    let (year_now, month_now, day_now) = tomorrow();
    if (air_year, month, day) > (year_now, i64::from(month_now), i64::from(day_now)) {
        return Err(Failure::Abort);
    }
    if air_year < 1970 && first_match.value("titleyear").trim().is_empty() {
        return Err(Failure::Abort);
    }

    for name in ["airyear", "airmonth", "airday"] {
        *last_index = (*last_index).max(first_match.last(name).map_or(0, end16));
    }
    result.air_date = Some(format!("{air_year:04}-{month:02}-{day:02}"));
    if first_match.success("part") {
        result.daily_part = Some(
            first_match
                .value("part")
                .parse()
                .map_err(|_| Failure::Abort)?,
        );
    }

    Ok(())
}

fn series_title_info(title: &str, first_match: &Match<'_, '_>) -> SeriesTitleInfo {
    let mut info = SeriesTitleInfo {
        title: title.to_owned(),
        ..SeriesTitleInfo::default()
    };
    match captures(&YEAR_IN_TITLE, title) {
        Some(found) => {
            group(&found, "title")
                .unwrap_or_default()
                .clone_into(&mut info.title_without_year);
            info.year = group(&found, "year").and_then(|year| year.parse().ok());
        }
        None => title.clone_into(&mut info.title_without_year),
    }

    if let Some(found) = captures(&TITLE_COMPONENTS, &info.title_without_year) {
        info.all_titles = TITLE_COMPONENTS
            .capture_names()
            .flatten()
            .filter_map(|name| found.name(name))
            .map(|m| m.as_str().to_owned())
            .collect();
    } else {
        let titles = first_match.spans("title");
        if titles.len() > 1 {
            info.all_titles = titles
                .into_iter()
                .map(|span| first_match.text(span).replace(['.', '_'], " "))
                .collect();
        }
    }
    info
}

fn valid_date(year: i64, month: i64, day: i64) -> bool {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&day)
}

/// A data de amanhã em UTC: o limite do "episódio no futuro".
fn tomorrow() -> (i64, u32, u32) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0) + 1;
    // Dias desde 1970 para data civil (algoritmo de Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * month_part + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    })
    .unwrap_or(1);
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Casos do conjunto de testes do gerenciador de séries, em nomes genéricos:
    // título, o que se espera da série, temporada e episódios.
    // Cada tabela é uma amostra de uma família de formatos.

    const UNICO: &[(&str, &str, u16, u16)] = &[
        (r"Series.With.Title.S02E15", r"Series With Title", 2, 15),
        (
            r"Series.Title.S10E27.WS.DSR.XviD-2HD",
            r"Series Title",
            10,
            27,
        ),
        (r"S03E09 WS PDTV XviD FUtV", r"", 3, 9),
        (
            r"24-7 Series - Title - Road to the Sonarr - S01E03 - Episode 3.mkv",
            r"24-7 Series - Title - Road to the Sonarr",
            1,
            3,
        ),
        (
            r"Series.Title.S01E07.21.0.Jump.Street.720p.WEB-DL.DD5.1.h.264-KiNGS",
            r"Series Title",
            1,
            7,
        ),
        (
            r"S6E02-Unwrapped-(Playing With Food) - [DarkData]",
            r"",
            6,
            2,
        ),
        (
            r"series.six-0.2010.217.hdtv-lol",
            r"series six-0 2010",
            2,
            17,
        ),
        (r"S08E20 50-50 Carla [DVD]", r"", 8, 20),
        (
            r"Series.S03E10.Alexandra.720p.WEB-DL.AAC2.0.H.264-CROM.mkv",
            r"Series",
            3,
            10,
        ),
        (
            r"10.Lines.You.Know.About.Code.S02E04.Prohibition.HDTV.XviD-AFG",
            r"10 Lines You Know About Code",
            2,
            4,
        ),
        (
            r"Never.Trust.The.B----.in.Code.23.S01E01",
            r"Never Trust The B---- in Code 23",
            1,
            1,
        ),
        (
            r"The_Series_US_s06e19_04.28.2014_hdtv.x264.Poke.mp4",
            r"The Series US",
            6,
            19,
        ),
        (
            r"[Impatience] Series - 0x01 [720p][34073169].mkv",
            r"Series",
            0,
            1,
        ),
        (
            r"Series.2012.S02E18.720p.HDTV.X264-DIMENSION.mkv",
            r"Series 2012",
            2,
            18,
        ),
        (
            r"Series.2009.S06E03.720p.HDTV.X264-DIMENSION [PublicHD].mkv",
            r"Series 2009",
            6,
            3,
        ),
        (
            r"The Series And the Show - S42 E10591 - 2015-01-27",
            r"The Series And the Show",
            42,
            10591,
        ),
        (
            r"Series - 01x02 - The Rooster Prince - [itz_theo]",
            r"Series",
            1,
            2,
        ),
        (
            r"Judge Developer 2016 02 25 S20E142",
            r"Judge Developer",
            20,
            142,
        ),
        (r"Series - S2016E231", r"Series", 2016, 231),
        (
            r"Plus Series la title - S14E3533 FRENCH WEBRIP H.264 AAC (09.05.2018)",
            r"Plus Series la title",
            14,
            3533,
        ),
        (
            r"Series.Title.S01.E01.English.AC3.DL.1080p.BluRay-Sonarr",
            r"Series Title",
            1,
            1,
        ),
        (
            r"[RlsGrp] Series Title - S01E27 - 24-Hour",
            r"Series Title",
            1,
            27,
        ),
        (r"1x1", r"", 1, 1),
        (
            r"Series-S07E12-31st_Century_Fox-[Bluray-1080p].mkv",
            r"Series",
            7,
            12,
        ),
        (
            r"Босх: Спадок (S2E1) / Series: Legacy (S2E1) (2023) WEB-DL 1080p Ukr/Eng | sub Eng",
            r"Series: Legacy",
            2,
            1,
        ),
        (
            r"[SubsPlus+] Series no Chill - S02E01 (NF WEB 1080p AVC AAC)",
            r"Series no Chill",
            2,
            1,
        ),
    ];

    const MULTI: &[(&str, &str, u16, &[u16])] = &[
        (
            r"Series.S03E01-06.DUAL.BDRip.XviD.AC3.-HELLYWOOD",
            r"Series",
            3,
            &[1, 2, 3, 4, 5, 6],
        ),
        (
            r"The Series S01e01 e02 ShoHD On Demand 1080i DD5 1 ALANiS",
            r"The Series",
            1,
            &[1, 2],
        ),
        (r"S03E01.S03E02.720p.HDTV.X264-DIMENSION", r"", 3, &[1, 2]),
        (
            r"Series.Kings.S02E09-E10.HDTV.x264-ASAP",
            r"Series Kings",
            2,
            &[9, 10],
        ),
        (
            r"Hell.on.Series.S02E09-E10.720p.HDTV.x264-EVOLVE",
            r"Hell on Series",
            2,
            &[9, 10],
        ),
        (
            r"Series.S29E161-E165.PDTV.x264-FQM",
            r"Series",
            29,
            &[161, 162, 163, 164, 165],
        ),
        (r"Series.10910.hdtv-lol.mp4", r"Series", 1, &[9, 10]),
        (
            r"The Series US S01E01-E02 720p HDTV x264",
            r"The Series US",
            1,
            &[1, 2],
        ),
        (
            r"Series.S01E02E03.1080p.BluRay.x264-DeBTViD",
            r"Series",
            1,
            &[2, 3],
        ),
        (
            r"Series Title.S6E1-E2.Episode Name.1080p.WEB-DL",
            r"Series Title",
            6,
            &[1, 2],
        ),
        (
            r"Series Title.S6.E1E3.Episode Name.1080p.WEB-DL",
            r"Series Title",
            6,
            &[1, 2, 3],
        ),
        (
            r"Series Title.S6.E1-E2-E3.Episode Name.1080p.WEB-DL",
            r"Series Title",
            6,
            &[1, 2, 3],
        ),
        (r"1x01-x03 - Episode Title.HDTV-720p", r"", 1, &[1, 2, 3]),
        (
            r"The Series Title (2010) - [S01E01-02-03] - Episode Title",
            r"The Series Title (2010)",
            1,
            &[1, 2, 3],
        ),
        (
            r"Series Title - S15E06-07 - City Sushi HDTV-720p",
            r"Series Title",
            15,
            &[6, 7],
        ),
        (
            r"Series Title - [02x01-02] - Episode 1",
            r"Series Title",
            2,
            &[1, 2],
        ),
        (
            r"Series Title! (2013) - S04E44-E45 - Il 200 spettacolare episodio da narcisisti! [NetflixHD 720p HEVC] [ITA+ENG].mkv",
            r"Series Title! (2013)",
            4,
            &[44, 45],
        ),
        (
            r"Босх: Спадок (S2E1-4) / Series: Legacy (S2E1-4) (2023) WEB-DL 1080p Ukr/Eng | sub Eng",
            r"Series: Legacy",
            2,
            &[1, 2, 3, 4],
        ),
    ];

    const TEMPORADA: &[(&str, &str, u16)] = &[
        (r"30.Series.Season.04.HDTV.XviD-DIMENSION", r"30 Series", 4),
        (
            r"Series.of.Sonarr.S03.720p.BluRay-CLUE\REWARD",
            r"Series of Sonarr",
            3,
        ),
        (
            r"Series Five 0 S01 720p WEB DL DD5 1 H 264 NT",
            r"Series Five 0",
            1,
        ),
        (
            r"Series Season 1 720p WEB DL DD 5 1 h264 TjHD",
            r"Series",
            1,
        ),
        (r"Series Confidential   Season 3", r"Series Confidential", 3),
        (r"My.Series.S2014.720p.HDTV.x264-ME", r"My Series", 2014),
        (r"Series Title - Series 1 (1970) DivX", r"Series Title", 1),
        (r"Series.Stagione.3.HDTV.XviD-NOTAG", r"Series", 3),
        (
            r"Series Title / S1E1-8 of 8 [2024, WEB-DL 1080p] + Original + RUS",
            r"Series Title",
            1,
        ),
        (
            r"[HorribleRips] Mobile Series 00 S1 [1080p]",
            r"Mobile Series 00",
            1,
        ),
    ];

    const VARIAS_TEMPORADAS: &[(&str, &str, u16)] = &[
        (
            r"The Series S01-05 WS BDRip X264-REWARD-No Rars",
            r"The Series",
            1,
        ),
        (
            r"Series.Title.S01-S09.1080p.AMZN.WEB-DL.DDP2.0.H.264-NTb",
            r"Series Title",
            1,
        ),
        (
            r"Series Title Season 01-07 BluRay 1080p x264 REPACK -SacReD",
            r"Series Title",
            1,
        ),
        (
            r"Series Title Season 01 - Season 07 BluRay 1080p x264 REPACK -SacReD",
            r"Series Title",
            1,
        ),
        (
            r"Series Title S01 S04 (1080p BluRay x265 HEVC 10bit AAC 5.1 Vyndros)",
            r"Series Title",
            1,
        ),
    ];

    const ABSOLUTO: &[(&str, &str, u16, u16, u16)] = &[
        (
            r"[SubDESU]_Show_One_07_(1280x720_x264-AAC)_[6B7FD717]",
            r"Show One",
            7,
            0,
            0,
        ),
        (
            r"[K-F] Some Anime Show 214 10x14",
            r"Some Anime Show",
            214,
            10,
            14,
        ),
        (
            r"[Chibiki] Series Title!! - 42 [360p][7A4FC77B]",
            r"Series Title!!",
            42,
            0,
            0,
        ),
        (
            r"Initial_Series_Title - 14 DVD - Central Anime",
            r"Initial Series Title",
            14,
            0,
            0,
        ),
        (
            r"[HorribleSubs] Series Title 2 - 05 [720p].mkv",
            r"Series Title 2",
            5,
            0,
            0,
        ),
        (
            r"[Hatsuyuki] Series Title (2014) - 017 (115) [1280x720][B2CFBC0F]",
            r"Series Title (2014)",
            17,
            0,
            0,
        ),
        (
            r"Series Title S03 - EP14 VOSTFR [1080p] [HardSub] Yass'Kun",
            r"Series Title S03",
            14,
            0,
            0,
        ),
        (
            r"SeriesTitle.E1206.In.seinen.Augen.2022.GERMAN.1080p.WEB.h264-Group",
            r"SeriesTitle",
            1206,
            0,
            0,
        ),
        (
            r"Anime Title the Final - 09 (2021) [SubsPlease] [WEBRip] [HD 1080p]",
            r"Anime Title the Final",
            9,
            0,
            0,
        ),
        (
            r"Dubbed show 79.BLM Sezon Finali(25.06.2023) 720p WEB-DL AAC2.0 H.264-TURG",
            r"Dubbed show",
            79,
            0,
            0,
        ),
    ];

    const ABSOLUTO_MULTI: &[(&str, &str, u16, u16)] = &[
        (
            r"[ANBU-AonE]_SeriesTitle_26-27_[F224EF26].avi",
            r"SeriesTitle",
            26,
            27,
        ),
        (
            r"[RlsGrp] Series Title (2010) - S01E01-02-03 - 001-002-003 - Episode Title HDTV-720p v2",
            r"Series Title (2010)",
            1,
            3,
        ),
        (
            r"Some Anime Show (2011) Episode 99-100 [1080p] [Dual.Audio] [x265]",
            r"Some Anime Show (2011)",
            99,
            100,
        ),
        (
            r"[Judas] Some Anime Show 091-123 [1080p][HEVC x265 10bit][Dual-Audio][Multi-Subs]",
            r"Some Anime Show",
            91,
            123,
        ),
        (
            r"[Erai-raws] Series-Title! 2 - 01~10 [1080p][Multiple Subtitle]",
            r"Series-Title! 2",
            1,
            10,
        ),
    ];

    const DIARIO: &[(&str, &str, u16, u16, u16)] = &[
        (
            r"Series Title 2011 04 18 Emma Roberts HDTV XviD BFF",
            r"Series Title",
            2011,
            4,
            18,
        ),
        (
            r"A.Late.Talk.Show.2010.10.11.Johnny.Knoxville.iTouch-MW",
            r"A Late Talk Show",
            2010,
            10,
            11,
        ),
        (
            r"2011.03.13 - A Late Talk Show - HD TV.mkv",
            r"",
            2011,
            3,
            13,
        ),
        (
            r"2020.A.Late.Talk.Show.2012.13.02.PDTV.XviD-C4TV",
            r"2020 A Late Talk Show",
            2012,
            2,
            13,
        ),
        (
            r"The_Series_US_04.28.2014_hdtv.x264.Poke.mp4",
            r"The Series US",
            2014,
            4,
            28,
        ),
        (
            r"The Show Series 2015 02 09 WEBRIP s01e13",
            r"The Show Series",
            2015,
            2,
            9,
        ),
        (r"2018-11-14.1080.all.mp4", r"", 2018, 11, 14),
        (
            r"Series Title (1955) - 1954-01-23 05 00 00 - Cottage for Sale.ts",
            r"Series Title (1955)",
            1954,
            1,
            23,
        ),
    ];

    const MINISSERIE: &[(&str, &str, u16)] = &[
        (
            r"The.Big.Series.Leader.Part.2.DSR.XviD-SYS",
            r"The Big Series Leader",
            2,
        ),
        (r"kill-roy-was-here-e07-720p", r"kill-roy-was-here", 7),
        (
            r"Series and Show 2012 Part 1 REPACK 720p HDTV x264 2HD",
            r"Series and Show 2012",
            1,
        ),
        (
            r"Series Show.2016.E04.Power.720p.WEB-DL.DD5.1.H.264-MARS",
            r"Series Show 2016",
            4,
        ),
    ];

    const CAMINHO: &[(&str, u16, u16)] = &[
        (
            r"z:/tv shows/series title (2003)/Season 3/S03E05 - Title.mkv",
            3,
            5,
        ),
        (
            r"z:/tv shows/series title/Specials/S00E16 - Dear Title - SD TV.avi",
            0,
            16,
        ),
        (r"C:/Test/TV/Series.4x05.HDTV.XviD-LOL", 4, 5),
        (
            r"S:/TV Drop/Series - 10x11 - Title [SDTV]/1011 - Title.avi",
            10,
            11,
        ),
        (
            r"/TV Drop/Series Title - 10x12 - 24 Hours of Development [SDTV]/1012 - Hours of Development.avi",
            10,
            12,
        ),
        (
            r"/TV Drop/Series Title - 10x12 - 24 Hours of Development [SDTV]/Hours of Development.avi",
            10,
            12,
        ),
        (
            r"/C/Test/Unsorted/Series.Title.S02E19.720p.BluRay.x264-SiNNERS-RP/ba27283b17c00d01193eacc02a8ba98eeb523a76.mkv",
            2,
            19,
        ),
        (r"/C/Test/Series/Season 01/01 Pilot (1080p HD).mkv", 1, 1),
        (
            r"/C/Test/Series/Season 1/2 Honor Thy Developer (1080p HD).m4v",
            1,
            2,
        ),
        (
            r"/C/Test/Series/Season 2/01. Total Series Action - Episode 1 - Monster Cash.mkv",
            2,
            1,
        ),
    ];

    const LIXO: &[&str] = &[
        r"76El6LcgLzqb426WoVFg1vVVVGx4uCYopQkfjmLe",
        r"TDAsqTea7k4o6iofVx3MQGuDK116FSjPobMuh8oB",
        r"oxXo8S2272KE1 lfppvxo3iwEJBrBmhlQVK1gqGc",
        r#"password - "bdc435cb-93c4-4902-97ea-ca00568c3887.337" yEnc"#,
        r"45a55debe3856da318cc35882ad07e43cd32fd15",
        r"ce39afb7da6cf7c04eba3090f0a309f609883862",
        r"Vh1FvU3bJXw6zs8EEUX4bMo5vbbMdHghxHirc.mkv",
        r"185d86a343e39f3341e35c4dad3ff159",
        r"qrdSD3rYzWb7cPdVIGSn4E7",
        r"abc.xyz.af6021c37f7852",
    ];

    fn parse(title: &str) -> ParsedEpisode {
        parse_episode_title(title).unwrap_or_else(|| panic!("não leu {title}"))
    }

    #[test]
    fn episodio_unico() {
        for (title, series, season, episode) in UNICO {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(parsed.season, *season, "{title}");
            assert_eq!(parsed.episodes, [*episode], "{title}");
            assert!(parsed.absolute_episodes.is_empty(), "{title}");
            assert!(!parsed.full_season, "{title}");
        }
    }

    #[test]
    fn multiepisodio() {
        for (title, series, season, episodes) in MULTI {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(parsed.season, *season, "{title}");
            assert_eq!(parsed.episodes, *episodes, "{title}");
            assert!(parsed.absolute_episodes.is_empty(), "{title}");
            assert!(!parsed.full_season, "{title}");
        }
    }

    #[test]
    fn pacote_de_temporada() {
        for (title, series, season) in TEMPORADA {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(parsed.season, *season, "{title}");
            assert!(parsed.episodes.is_empty(), "{title}");
            assert!(parsed.full_season, "{title}");
            assert!(!parsed.multi_season, "{title}");
        }
    }

    #[test]
    fn pacote_de_varias_temporadas() {
        for (title, series, first) in VARIAS_TEMPORADAS {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(parsed.season, *first, "{title}");
            assert!(parsed.full_season && parsed.multi_season, "{title}");
            assert!(!parsed.partial_season, "{title}");
        }
    }

    #[test]
    fn extras_e_pacote_parcial() {
        let extras = parse("Punky Series S01 EXTRAS DVDRip XviD RUNNER");
        assert!(extras.full_season && extras.season_extra);
        let subpack = parse("Series.S11.SUBPACK.DVDRip.XviD-REWARD");
        assert_eq!((subpack.season, subpack.season_extra), (11, true));

        let part = parse("The.Series.S06.P1.1080p.Blu-Ray.10-Bit.Dual-Audio.TrueHD.x265-iAHD");
        assert!(part.partial_season && !part.full_season);
        assert_eq!((part.season, part.season_part), (6, 1));
        let volume = parse("The.Series.S07.Vol.2.1080p.NF.WEBRip.DD5.1.x264-NTb");
        assert_eq!((volume.season, volume.season_part), (7, 2));
    }

    #[test]
    fn episodio_especial_e_dividido() {
        let special = parse("Series Title S01E11.5 [SP]-The Poppies Bloom Red on the Battlefield");
        assert!(special.special);
        assert_eq!(special.episodes, [11]);

        let split = parse(
            "Series.Title.S06E01b.Fade.Out.Fade.in.Part.2.1080p.DSNP.WEB-DL.AAC2.0.H.264-FLUX",
        );
        assert!(split.split_episode);
        assert_eq!((split.season, split.episodes), (6, vec![1]));

        // Especial na lista de pacotes: a temporada inteira deixa de valer.
        let pack = parse("Series.Title.S02.Special.1080p.WEB-DL");
        assert!(pack.special && !pack.full_season);
    }

    #[test]
    fn numeracao_absoluta() {
        for (title, series, absolute, season, episode) in ABSOLUTO {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(parsed.absolute_episodes, [*absolute], "{title}");
            assert_eq!(parsed.season, *season, "{title}");
            if *episode == 0 {
                assert!(
                    parsed.episodes.is_empty() || parsed.episodes == [0],
                    "{title}"
                );
            } else {
                assert_eq!(parsed.episodes, [*episode], "{title}");
            }
            assert!(!parsed.full_season, "{title}");
        }
        for (title, series, first, last) in ABSOLUTO_MULTI {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(
                parsed.absolute_episodes,
                (*first..=*last).collect::<Vec<_>>(),
                "{title}"
            );
        }
    }

    #[test]
    fn absoluto_com_parte_decimal_e_especial() {
        let recap = parse("[Anime Time] Series no Mayo - 12.5.mkv");
        assert!(recap.absolute_episodes.is_empty());
        assert_eq!(recap.special_absolute_episodes, [12.5]);
        assert!(recap.special);

        let ova = parse("[DeadFish] Another Anime Show - 01 - OVA [BD][720p][AAC]");
        assert!(ova.special);
        assert_eq!(ova.absolute_episodes, [1]);

        let standard = parse("[sam] Anime - 15.5 (S00E01) [BD 1080p FLAC] [3E8D676D]");
        assert_eq!((standard.season, standard.episodes), (0, vec![1]));
        assert_eq!(standard.special_absolute_episodes, [15.5]);
        assert_eq!(standard.release_group.as_deref(), Some("sam"));
        assert_eq!(standard.release_hash.as_deref(), Some("3E8D676D"));
    }

    #[test]
    fn programa_diario() {
        for (title, series, year, month, day) in DIARIO {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(
                parsed.air_date.as_deref(),
                Some(format!("{year:04}-{month:02}-{day:02}").as_str()),
                "{title}"
            );
            assert!(parsed.episodes.is_empty() && !parsed.full_season, "{title}");
            assert!(parsed.is_daily(), "{title}");
        }
        let part = parse("Series.Title.2015.09.07.Part.2.720p.HULU.WEBRip.AAC2.0.H.264-GRUPO");
        assert_eq!(part.daily_part, Some(2));
    }

    #[test]
    fn data_invalida_nao_le() {
        // Dia e mês indistinguíveis.
        assert!(parse_episode_title("Programa - 05-06-2024 HDTV 1080p H264 AAC").is_none());
        // Anterior a 1970 e sem ano no título.
        assert!(parse_episode_title("Series.Title.1950.10.14.720p.HDTV").is_none());
        // No futuro.
        assert!(parse_episode_title("Series.Title.2999.10.14.720p.HDTV").is_none());
        // Dia que não existe.
        assert!(parse_episode_title("Series.Title.2015.02.31.720p.HDTV").is_none());
    }

    #[test]
    fn minisserie() {
        for (title, series, episode) in MINISSERIE {
            let parsed = parse(title);
            assert_eq!(parsed.series_title, *series, "{title}");
            assert_eq!(parsed.season, 1, "{title}");
            assert_eq!(parsed.episodes, [*episode], "{title}");
            assert!(parsed.mini_series, "{title}");
        }
    }

    #[test]
    fn caminho_de_arquivo() {
        for (path, season, episode) in CAMINHO {
            let parsed = parse_episode_path(path).unwrap_or_else(|| panic!("não leu {path}"));
            assert_eq!(parsed.season, *season, "{path}");
            assert_eq!(parsed.episodes, [*episode], "{path}");
            assert!(
                parsed.absolute_episodes.is_empty() && !parsed.full_season,
                "{path}"
            );
        }

        // Só o número no nome: a temporada vem da pasta.
        let parsed = parse_episode_path("/Series/Season 01/02 Titulo (1080p HD).m4v").unwrap();
        assert_eq!((parsed.season, parsed.episodes), (1, vec![2]));
        let multi =
            parse_episode_path("/Season 2/E05-06 - Episode Title HDTV-720p Proper").unwrap();
        assert_eq!((multi.season, multi.episodes), (2, vec![5, 6]));
        // Nome sem informação: lido pela pasta.
        let batch =
            parse_episode_path("/C/Test/Series Title 921-928 [English Dub][1080p][grupo]/921.mkv")
                .unwrap();
        assert_eq!(batch.absolute_episodes, [921]);
    }

    #[test]
    fn caminho_com_nome_embaralhado() {
        for (path, series, quality, group) in [
            (
                "/C/Test/Some.Hashed.Release.S01E01.720p.WEB-DL.AAC2.0.H.264-Mercury/0e895c37245186812cb08aab1529cf8ee389dd05.mkv",
                "Some Hashed Release",
                Quality::WebDl720p,
                Some("Mercury"),
            ),
            (
                "/C/Test/Fake.Dir.S01E01-Test/yrucreM-462.H.0.2CAA.LD-BEW.p027.10E10S.esaeleR.dehsaH.emoS.mkv",
                "Some Hashed Release",
                Quality::WebDl720p,
                Some("Mercury"),
            ),
            (
                "/C/Test/Title.S01E10.DVDRip.XviD-GRUPO/AHFMZXGHEWD660.mkv",
                "Title",
                Quality::Dvd,
                Some("GRUPO"),
            ),
            (
                "/C/Test/XxQVHK4GJMP3n2dLpmhW/MKV/010E70S.nuF.fo.snoS.mkv",
                "Sons of Fun",
                Quality::Hdtv720p,
                None,
            ),
            ("50E50S.denorD.mkv", "Droned", Quality::Hdtv720p, None),
        ] {
            let parsed = parse_episode_path(path).unwrap_or_else(|| panic!("não leu {path}"));
            assert_eq!(parsed.series_title, series, "{path}");
            assert_eq!(parsed.quality.quality, quality, "{path}");
            assert_eq!(parsed.release_group.as_deref(), group, "{path}");
        }
    }

    #[test]
    fn nome_ao_contrario() {
        let parsed = parse("yrucreM-462.H.0.2CAA.LD-BEW.p027.10E10S.esaeleR.dehsaH.emoS");
        assert_eq!(parsed.series_title, "Some Hashed Release");
        assert_eq!((parsed.season, parsed.episodes), (1, vec![1]));
    }

    #[test]
    fn nao_le() {
        for title in LIXO {
            assert!(parse_episode_title(title).is_none(), "{title}");
        }
        for title in [
            "0123456789abcdef0123456789abcdef",
            "Season 3",
            "Specials",
            "----",
            "Series Title - Some Pack password yenc",
            "Ola Mundo",
        ] {
            assert!(parse_episode_title(title).is_none(), "{title}");
        }
    }

    #[test]
    fn titulos_da_serie() {
        let info = parse("Series (2022) S03E14 720p HDTV X264-DIMENSION").series_title_info;
        assert_eq!(
            (info.title_without_year.as_str(), info.year),
            ("Series", Some(2022))
        );
        let info = parse("1234.S03E14.720p.HDTV.X264-DIMENSION").series_title_info;
        assert_eq!(
            (info.title_without_year.as_str(), info.year),
            ("1234", None)
        );

        let info =
            parse("Один / Series: Legacy / S2E1-4 of 10 (2023) WEB-DL 1080p Ukr/Eng | sub Eng")
                .series_title_info;
        assert_eq!(info.all_titles, ["Один", "Series: Legacy"]);
        let info = parse("Один AKA Series Legacy S02 1080p NF WEB-DL").series_title_info;
        assert_eq!(info.all_titles, ["Один", "Series Legacy"]);

        let parsed =
            parse("[scnzbefnet][509103] 2.Developers.Series.S03E18.720p.HDTV.X264-DIMENSION");
        assert_eq!(parsed.series_title, "2 Developers Series");
    }

    #[test]
    fn de_um_pacote_de_temporadas_com_episodios_de_quatro_digitos() {
        let parsed = parse("Series - S2016E231");
        assert_eq!((parsed.season, parsed.episodes), (2016, vec![231]));
        let big = parse("Shortland.Series.S22E5363-E5366.HDTV.x264-GRUPO");
        assert_eq!(big.episodes, [5363, 5364, 5365, 5366]);
    }

    #[test]
    fn numero_fora_do_alcance_nao_le() {
        // Cinco dígitos acima de `u16`: o original lê, este porte não.
        assert!(parse_episode_title("Series Title - 1x99999").is_none());
    }

    #[test]
    fn digitos_de_outra_escrita() {
        let parsed = parse("[Subz] My Series - １５８ [h264 10-bit][1080p]");
        assert_eq!(parsed.absolute_episodes, [158]);
        assert!(parse_episode_title("علم نف) أ.دعادل الأبيض ٢٠٢٤ ٣ ٣").is_some());
    }

    #[test]
    fn anime_chines_e_substituicoes() {
        let parsed = parse("[桜都字幕组][盾之勇者成名录/Anime Series Title][01][BIG5][720P]");
        assert_eq!(parsed.series_title, "Anime Series Title");
        assert_eq!(parsed.release_group.as_deref(), Some("桜都字幕组"));
        assert_eq!(parsed.absolute_episodes, [1]);
        let parsed =
            parse("[风车字幕组][名侦探柯南][857][米花町反复变化之谜（前篇）][简体][MP4][1080P]");
        assert_eq!(parsed.series_title, "名侦探柯南");
        assert_eq!(parsed.absolute_episodes, [857]);
    }

    #[test]
    fn qualidade_idioma_e_grupo_de_episodio() {
        let parsed = parse("Series.Title.S01E02.1080p.NF.WEB-DL.DDP5.1.H.264.DUAL-GRUPO");
        assert_eq!(parsed.quality.quality, Quality::WebDl1080p);
        assert_eq!(parsed.languages, [Language::Unknown]);
        assert_eq!(parsed.release_group.as_deref(), Some("GRUPO"));
        assert_eq!(
            parsed.release_tokens,
            ".1080p.NF.WEB-DL.DDP5.1.H.264.DUAL-GRUPO"
        );

        // O idioma só vale depois da temporada e do episódio: o título da
        // série não conta ("Dublado" aqui é do nome da série).
        let parsed =
            parse("Series Title - S04 2021 MKV / H.264 / WEB-DL / 1080p / Dublado / Plataforma");
        assert!(parsed.full_season);
        assert_eq!(parsed.languages, [Language::PortugueseBr]);
        assert_eq!(parsed.quality.quality, Quality::WebDl1080p);

        let parsed = parse("Series.Title.S01E03.PROPER.720p.HDTV.x264-GRUPO");
        assert_eq!(parsed.quality.revision.version, 2);

        // O remux de séries é o de filmes com outro nome, e o `.mkv` sozinho
        // vale 720p ali (em filmes, é WEB-DL 720p).
        assert_eq!(
            parse_episode_quality("Series.S01E01.1080p.BluRay.Remux.AVC-GRUPO").quality,
            Quality::Remux1080p
        );
        assert_eq!(
            parse_episode_quality("episodio.mkv").quality,
            Quality::Hdtv720p
        );
    }

    #[test]
    fn todos_os_padroes_compilam() {
        assert_eq!(REPORT_TITLES.len(), 98);
        assert_eq!(PRE_SUBSTITUTIONS.len(), 12);
        assert_eq!(REJECT_HASHED.len(), 14);
    }
}

//! Nome de release de filme: título (e títulos alternativos), ano, edição,
//! qualidade, idiomas, grupo e ids embutidos.

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

use crate::common::{
    captures, group, is_match, last_captures, regex, remove_file_extension, replace_all,
    strip_torrent_suffix, strip_website_postfix, strip_website_prefix,
};
use crate::group::parse_release_group;
use crate::language::{Language, parse_languages};
use crate::quality::{Quality, QualityModel, parse_quality, parse_quality_name};

/// O que se leu de um nome de release de filme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMovie {
    /// O título principal primeiro; depois os alternativos ("AKA").
    pub titles: Vec<String>,
    pub year: Option<u16>,
    pub edition: Option<String>,
    pub quality: QualityModel,
    pub languages: Vec<Language>,
    pub release_group: Option<String>,
    pub hardcoded_subs: Option<String>,
    pub release_hash: Option<String>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u32>,
}

impl ParsedMovie {
    #[must_use]
    pub fn primary_title(&self) -> &str {
        self.titles.first().map_or("", String::as_str)
    }
}

const EDITION: &str = r"\(?\b(?<edition>(((Recut.|Extended.|Ultimate.)?(Director.?s|Collector.?s|Theatrical|Ultimate|Extended|Despecialized|(Special|Rouge|Final|Assembly|Imperial|Diamond|Signature|Hunter|Rekall)(?=(.(Cut|Edition|Version)))|\d{2,3}(th)?.Anniversary)(?:.(Cut|Edition|Version))?(.(Extended|Uncensored|Remastered|Unrated|Uncut|Open.?Matte|IMAX|Fan.?Edit))?|((Uncensored|Remastered|Unrated|Uncut|Open?.Matte|IMAX|Fan.?Edit|Restored|((2|3|4)in1))))))\b\)?";

static REPORT_EDITION: LazyLock<Regex> = LazyLock::new(|| regex(&format!("(?i)^.+?{EDITION}")));

static HARDCODED_SUBS: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)\b((?<hcsub>(\w+(?<!SOFT|MULTI|HORRIBLE)SUBS?))|(?<hc>(HC|SUBBED)))\b")
});

/// Tentados em ordem; o primeiro que casa e dá título vence.
static MOVIE_TITLE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // [Grupo] Título 1993 (1993) [VHS]: ano entre parênteses.
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)\s*\((?<year>(1(8|9)|20)\d{2})\).*?(?<hash>\[\w{8}\])?(?:$|\.)".to_owned(),
        // [Grupo] e ano.
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|x|\d+|\]|\W\d+)))+.*?(?<hash>\[\w{8}\])?(?:$|\.)".to_owned(),
        // [Grupo] sem ano, título com versão, hash.
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)((v)(?:\d{1,2})(?:([-_. ])))(\[.*)?(?:[\[(][^])])?.*?(?<hash>\[\w{8}\])(?:$|\.)".to_owned(),
        // [Grupo] sem ano, informação entre colchetes duplos, hash.
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)(\[.*).*?(?<hash>\[\w{8}\])(?:$|\.)".to_owned(),
        // [Grupo] sem ano, informação entre parênteses ou colchetes, hash.
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+)(?:[\[(][^])]).*?(?<hash>\[\w{8}\])(?:$|\.)".to_owned(),
        // Formatos alemães e TrueFrench, às vezes sem ano.
        format!(r"^(?<title>(?![(\[]).+?)((\W|_))({EDITION}.{{1,3}})?(?:(?<!(19|20)\d{{2}}.*?)(?<!(?:Good|The)[_ .-])(German|TrueFrench))(.+?)(?=((19|20)\d{{2}}|$))(?<year>(19|20)\d{{2}}(?!p|i|\d+|\]|\W\d+))?(\W+|_|$)(?!\\)"),
        // Edição antes do ano: Mission.Impossible.3.Special.Edition.2011.
        format!(r"^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*{EDITION}.{{1,3}}(?<year>(1(8|9)|20)\d{{2}}(?!p|i|\d+|\]|\W\d+)))+(\W+|_|$)(?!\\)"),
        // O formato normal: Mission.Impossible.3.2011.
        r"^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|(1(8|9)|20)\d{2}|\]|\W(1(8|9)|20)\d{2})))+(\W+|_|$)(?!\\)".to_owned(),
        // Nome de torrent com o site entre colchetes: Star.Wars[Site].
        r"^(?<title>.+?)?(?:(?:[-_\W](?<![()\[!]))*(?<year>(\[\w *\])))+(\W+|_|$)(?!\\)".to_owned(),
        // Ano entre colchetes.
        r"^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\W\d+)))+(\W+|_|$)(?!\\)".to_owned(),
        // Último recurso, para título com ( ou [.
        r"^(?<title>.+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\]|\W\d+)))+(\W+|_|$)(?!\\)".to_owned(),
    ]
    .iter()
    .map(|pattern| regex(&format!("(?i){pattern}")))
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
    ]
    .iter()
    .map(|pattern| regex(pattern))
    .collect()
});

static REVERSED_TITLE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?:^|[-._ ])(p027|p0801)[-._ ]"));

static ALTERNATIVE_TITLE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)[ ]+(?:AKA|/)[ ]+"));

static BRACKETED_ALTERNATIVE_TITLE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)(.*) \([ ]*AKA[ ]+(.*)\)"));

static NORMALIZE_ALTERNATIVE_TITLE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)[ ]+(?:A\.K\.A\.)[ ]+"));

static IMDB_ID: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)(?<imdbid>tt\d{7,8})"));

static TMDB_ID: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)tmdb(id)?-(?<tmdbid>\d+)"));

static SIMPLE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?:(480|540|576|720|1080|2160)[ip]|[xh][\W_]?26[45]|DD\W?5\W1|[<>?*]|848x480|1280x720|1920x1080|3840x2160|4096x2160|(8|10)b(it)?|10-bit)\s*?(?![a-b0-9])",
    )
});

static SIMPLE_RELEASE_TITLE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\s*(?:[<>?*|])"));

static CLEAN_QUALITY_BRACKETS: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\[[a-z0-9 ._-]+\]$"));

static REQUEST_INFO: LazyLock<Regex> = LazyLock::new(|| regex(r"^(?:\[.+?\])+"));

/// Lê um nome de release de filme. `None` quando não dá para afirmar nem o
/// título: nome embaralhado, só símbolos, ou nenhum padrão reconhece.
#[must_use]
pub fn parse_movie_title(title: &str) -> Option<ParsedMovie> {
    if !valid_before_parsing(title) {
        return None;
    }
    let title = unreverse(title);

    let release_title = remove_file_extension(&title);
    let release_title = release_title
        .trim_matches(|c| c == '-' || c == '_')
        .replace('【', "[")
        .replace('】', "]");

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

    for pattern in MOVIE_TITLE.iter() {
        let Some(found) = captures(pattern, &simple_title) else {
            continue;
        };
        let Some((titles, year, edition)) = titles_and_year(&found) else {
            continue;
        };
        // Um padrão que casou e deu título decide; o resto só lê detalhes.
        return details(
            &title,
            &release_title,
            &simple_title,
            &found,
            titles,
            year,
            edition,
        );
    }
    None
}

fn details(
    original: &str,
    release_title: &str,
    simple_title: &str,
    found: &Captures<'_, str>,
    titles: Vec<String>,
    year: Option<u16>,
    edition: Option<String>,
) -> Option<ParsedMovie> {
    let title_match = found.name("title")?;
    let mut simple_release_title = replace_all(&SIMPLE_RELEASE_TITLE, release_title, "");
    if !title_match.as_str().trim().is_empty() {
        let replacement = if title_match.as_str().contains('.') {
            "A.Movie"
        } else {
            "A Movie"
        };
        // A referência aplica a posição do título no nome simplificado ao
        // nome sem símbolos — outra string. Em UTF-16. E desiste do título
        // quando a posição cai fora dele.
        let start = utf16_len(&simple_title[..title_match.start()]);
        let length = utf16_len(title_match.as_str());
        simple_release_title = splice_utf16(&simple_release_title, start, length, replacement)?;
    }

    let mut release_group = parse_release_group(&simple_release_title);
    if let Some(subgroup) = group(found, "subgroup").filter(|s| !s.trim().is_empty()) {
        release_group = Some(subgroup.to_owned());
    }
    let language_title = match &release_group {
        Some(group) if !group.trim().is_empty() => simple_release_title.replace(group, "RlsGrp"),
        _ => simple_release_title.clone(),
    };
    let edition = edition.or_else(|| parse_edition(&simple_release_title));
    let release_hash = group(found, "hash")
        .map(|hash| hash.trim_matches(|c| c == '[' || c == ']'))
        .filter(|hash| !hash.is_empty() && *hash != "1280x720")
        .map(str::to_owned);

    Some(ParsedMovie {
        titles,
        year,
        edition,
        quality: parse_quality(original),
        languages: parse_languages(&language_title),
        release_group,
        hardcoded_subs: parse_hardcoded_subs(original),
        release_hash,
        imdb_id: captures(&IMDB_ID, &simple_release_title)
            .and_then(|c| group(&c, "imdbid").map(str::to_owned)),
        tmdb_id: captures(&TMDB_ID, &simple_release_title)
            .and_then(|c| group(&c, "tmdbid").and_then(|id| id.parse().ok())),
    })
}

fn valid_before_parsing(title: &str) -> bool {
    let lower = title.to_lowercase();
    if lower.contains("password") && lower.contains("yenc") {
        return false;
    }
    if !title.chars().any(char::is_alphanumeric) {
        return false;
    }
    let without_extension = remove_file_extension(title);
    !REJECT_HASHED
        .iter()
        .any(|pattern| is_match(pattern, &without_extension))
}

/// Nome escrito de trás para frente ("p0801" é "1080p" ao contrário).
fn unreverse(title: &str) -> String {
    if !is_match(&REVERSED_TITLE, title) {
        return title.to_owned();
    }
    let without_extension = remove_file_extension(title);
    let extension = &title[without_extension.len()..];
    without_extension.chars().rev().collect::<String>() + extension
}

/// Título(s), ano e edição do padrão que casou; `None` se não deu título.
fn titles_and_year(
    found: &Captures<'_, str>,
) -> Option<(Vec<String>, Option<u16>, Option<String>)> {
    let raw = group(found, "title")?;
    if raw == "(" {
        return None;
    }
    let name = raw.replace('_', " ");
    let name = replace_all(&NORMALIZE_ALTERNATIVE_TITLE, &name, " AKA ");
    let name = replace_all(&REQUEST_INFO, &name, "");
    let name = join_acronyms(name.trim_matches(' '));

    let year = group(found, "year")
        .and_then(|year| year.parse::<u16>().ok())
        .filter(|year| *year != 0);
    let edition = group(found, "edition").map(|edition| edition.replace('.', " "));

    let unbracketed = BRACKETED_ALTERNATIVE_TITLE
        .replace_all(&name, "$1 AKA $2")
        .into_owned();
    let mut titles = vec![name.clone()];
    let mut rest = 0;
    let mut split = Vec::new();
    for separator in ALTERNATIVE_TITLE
        .find_iter(&unbracketed)
        .map_while(Result::ok)
    {
        split.push(&unbracketed[rest..separator.start()]);
        rest = separator.end();
    }
    split.push(&unbracketed[rest..]);
    titles.extend(
        split
            .into_iter()
            .filter(|alternative| !alternative.trim().is_empty() && *alternative != name)
            .map(str::to_owned),
    );
    Some((titles, year, edition))
}

/// Troca os pontos por espaços, menos os de sigla: "S.W.A.T" fica.
fn join_acronyms(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    let is_digit = |part: &str| part.len() == 1 && part.chars().all(|c| c.is_ascii_digit());
    let mut out = String::new();
    let mut previous_acronym = false;
    for (n, part) in parts.iter().enumerate() {
        let next = parts.get(n + 1).copied().unwrap_or("");
        let lower = part.to_lowercase();
        let single = part.chars().count() == 1;
        let letter_of_acronym = single
            && lower != "a"
            && !is_digit(part)
            && (previous_acronym || n < parts.len() - 1)
            && (previous_acronym || next.chars().count() != 1 || !is_digit(next));
        let article_in_acronym = lower == "a" && (previous_acronym || next.chars().count() == 1);
        if letter_of_acronym || article_in_acronym || lower == "dr" {
            out.push_str(part);
            out.push('.');
            previous_acronym = true;
        } else {
            if previous_acronym {
                out.push(' ');
                previous_acronym = false;
            }
            out.push_str(part);
            out.push(' ');
        }
    }
    out.trim_matches(' ').to_owned()
}

fn parse_edition(title: &str) -> Option<String> {
    captures(&REPORT_EDITION, title)
        .and_then(|c| group(&c, "edition").map(|edition| edition.replace('.', " ")))
        .filter(|edition| !edition.trim().is_empty())
}

fn parse_hardcoded_subs(title: &str) -> Option<String> {
    let found = last_captures(&HARDCODED_SUBS, title)?;
    if let Some(subs) = group(&found, "hcsub") {
        return Some(subs.to_owned());
    }
    group(&found, "hc").map(|_| "Generic Hardcoded Subs".to_owned())
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Posição em UTF-16 para posição em bytes; `None` se cair no meio de um
/// caractere ou fora do texto.
fn utf16_to_byte(text: &str, index: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units == index {
            return Some(byte);
        }
        units += c.len_utf16();
    }
    (units == index).then_some(text.len())
}

/// `Remove(start, length).Insert(start, with)` do .NET; `None` onde ele
/// lançaria exceção.
fn splice_utf16(text: &str, start: usize, length: usize, with: &str) -> Option<String> {
    let from = utf16_to_byte(text, start)?;
    let to = utf16_to_byte(text, start.checked_add(length)?)?;
    Some(format!("{}{with}{}", &text[..from], &text[to..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(title: &str) -> ParsedMovie {
        parse_movie_title(title).unwrap_or_else(|| panic!("não leu {title}"))
    }

    #[test]
    fn formato_normal() {
        let movie = parse("Mission.Impossible.3.2006.1080p.BluRay.x264-GRUPO");
        assert_eq!(movie.titles, ["Mission Impossible 3"]);
        assert_eq!(movie.year, Some(2006));
        assert_eq!(movie.quality.quality, Quality::Bluray1080p);
        assert_eq!(movie.release_group.as_deref(), Some("GRUPO"));
    }

    #[test]
    fn sigla_e_aka() {
        let movie = parse("S.W.A.T.2003.720p.WEB-DL-GRUPO");
        assert_eq!(movie.titles, ["S.W.A.T."]);
        let movie = parse("Le Film AKA The Movie 2010 1080p");
        assert_eq!(
            movie.titles,
            ["Le Film AKA The Movie", "Le Film", "The Movie"]
        );
    }

    #[test]
    fn edicao() {
        let movie = parse("Aliens.1986.Directors.Cut.1080p.BluRay-GRUPO");
        assert_eq!(movie.edition.as_deref(), Some("Directors Cut"));
        let movie = parse("Mission.Impossible.3.Special.Edition.2011.720p");
        assert_eq!(movie.edition.as_deref(), Some("Special Edition"));
    }

    #[test]
    fn ids_embutidos() {
        let movie = parse("Filme (2020) {imdb-tt1234567} tmdb-4242.mkv");
        assert_eq!(movie.imdb_id.as_deref(), Some("tt1234567"));
        assert_eq!(movie.tmdb_id, Some(4242));
    }

    #[test]
    fn titulo_acentuado_e_dual_audio() {
        let movie = parse("Ação Mortal 2025 1080p WEB-DL Dual-Audio Brazilian Original");
        assert_eq!(movie.titles, ["Ação Mortal"]);
        assert_eq!(
            movie.languages,
            [Language::PortugueseBr, Language::Original]
        );
    }

    /// Esquisitices da referência que o porte reproduz de propósito: o "o"
    /// de uma letra vira sigla, e o grupo é o que vem depois do último hífen.
    #[test]
    fn esquisitices_da_referencia() {
        let movie =
            parse("Ingresso.Para.o.Paraiso.2022.2160p.HDR.WEB-DL.DDP5.1.Atmos.H265-NAISU.Dual-C76");
        assert_eq!(movie.titles, ["Ingresso Para o. Paraiso"]);
        assert_eq!(movie.release_group.as_deref(), Some("C76"));
        assert_eq!(movie.quality.quality, Quality::WebDl2160p);
        let movie = parse("Filme 2025 1080p WEB-DL Dual-Audio Brazilian Original");
        assert_eq!(movie.release_group.as_deref(), Some("Audio"));
    }

    #[test]
    fn recusa_hash_e_simbolo() {
        assert!(parse_movie_title("0123456789abcdef0123456789abcdef").is_none());
        assert!(parse_movie_title("----").is_none());
    }

    #[test]
    fn todos_os_padroes_compilam() {
        assert_eq!(MOVIE_TITLE.len(), 11);
        assert_eq!(REJECT_HASHED.len(), 9);
        let _ = (
            &*REPORT_EDITION,
            &*HARDCODED_SUBS,
            &*SIMPLE_TITLE,
            &*TMDB_ID,
        );
    }
}

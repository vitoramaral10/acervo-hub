//! Idiomas anunciados no nome do release.

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::common::{is_match, regex};

/// Idiomas com o id e o nome que a API v3 usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Unknown,
    English,
    French,
    Spanish,
    German,
    Italian,
    Danish,
    Dutch,
    Japanese,
    Icelandic,
    Chinese,
    Russian,
    Polish,
    Vietnamese,
    Swedish,
    Norwegian,
    Finnish,
    Turkish,
    Portuguese,
    Flemish,
    Greek,
    Korean,
    Hungarian,
    Hebrew,
    Lithuanian,
    Czech,
    Hindi,
    Romanian,
    Thai,
    Bulgarian,
    PortugueseBr,
    Arabic,
    Ukrainian,
    Persian,
    Bengali,
    Slovak,
    Latvian,
    SpanishLatino,
    Catalan,
    Tamil,
    Telugu,
    Malayalam,
    Kannada,
    Albanian,
    Afrikaans,
    Marathi,
    Tagalog,
    Urdu,
    Romansh,
    Mongolian,
    Georgian,
    /// O idioma original da obra, seja qual for.
    Original,
    /// Qualquer um: só aparece em perfil, nunca sai do parser.
    Any,
}

impl Language {
    #[must_use]
    pub const fn id(self) -> i16 {
        match self {
            Self::Unknown => 0,
            Self::English => 1,
            Self::French => 2,
            Self::Spanish => 3,
            Self::German => 4,
            Self::Italian => 5,
            Self::Danish => 6,
            Self::Dutch => 7,
            Self::Japanese => 8,
            Self::Icelandic => 9,
            Self::Chinese => 10,
            Self::Russian => 11,
            Self::Polish => 12,
            Self::Vietnamese => 13,
            Self::Swedish => 14,
            Self::Norwegian => 15,
            Self::Finnish => 16,
            Self::Turkish => 17,
            Self::Portuguese => 18,
            Self::Flemish => 19,
            Self::Greek => 20,
            Self::Korean => 21,
            Self::Hungarian => 22,
            Self::Hebrew => 23,
            Self::Lithuanian => 24,
            Self::Czech => 25,
            Self::Hindi => 26,
            Self::Romanian => 27,
            Self::Thai => 28,
            Self::Bulgarian => 29,
            Self::PortugueseBr => 30,
            Self::Arabic => 31,
            Self::Ukrainian => 32,
            Self::Persian => 33,
            Self::Bengali => 34,
            Self::Slovak => 35,
            Self::Latvian => 36,
            Self::SpanishLatino => 37,
            Self::Catalan => 38,
            Self::Tamil => 43,
            Self::Telugu => 45,
            Self::Malayalam => 48,
            Self::Kannada => 49,
            Self::Albanian => 50,
            Self::Afrikaans => 51,
            Self::Marathi => 52,
            Self::Tagalog => 53,
            Self::Urdu => 54,
            Self::Romansh => 55,
            Self::Mongolian => 56,
            Self::Georgian => 57,
            Self::Original => -2,
            Self::Any => -1,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::English => "English",
            Self::French => "French",
            Self::Spanish => "Spanish",
            Self::German => "German",
            Self::Italian => "Italian",
            Self::Danish => "Danish",
            Self::Dutch => "Dutch",
            Self::Japanese => "Japanese",
            Self::Icelandic => "Icelandic",
            Self::Chinese => "Chinese",
            Self::Russian => "Russian",
            Self::Polish => "Polish",
            Self::Vietnamese => "Vietnamese",
            Self::Swedish => "Swedish",
            Self::Norwegian => "Norwegian",
            Self::Finnish => "Finnish",
            Self::Turkish => "Turkish",
            Self::Portuguese => "Portuguese",
            Self::Flemish => "Flemish",
            Self::Greek => "Greek",
            Self::Korean => "Korean",
            Self::Hungarian => "Hungarian",
            Self::Hebrew => "Hebrew",
            Self::Lithuanian => "Lithuanian",
            Self::Czech => "Czech",
            Self::Hindi => "Hindi",
            Self::Romanian => "Romanian",
            Self::Thai => "Thai",
            Self::Bulgarian => "Bulgarian",
            Self::PortugueseBr => "Portuguese (Brazil)",
            Self::Arabic => "Arabic",
            Self::Ukrainian => "Ukrainian",
            Self::Persian => "Persian",
            Self::Bengali => "Bengali",
            Self::Slovak => "Slovak",
            Self::Latvian => "Latvian",
            Self::SpanishLatino => "Spanish (Latino)",
            Self::Catalan => "Catalan",
            Self::Tamil => "Tamil",
            Self::Telugu => "Telugu",
            Self::Malayalam => "Malayalam",
            Self::Kannada => "Kannada",
            Self::Albanian => "Albanian",
            Self::Afrikaans => "Afrikaans",
            Self::Marathi => "Marathi",
            Self::Tagalog => "Tagalog",
            Self::Urdu => "Urdu",
            Self::Romansh => "Romansh",
            Self::Mongolian => "Mongolian",
            Self::Georgian => "Georgian",
            Self::Original => "Original",
            Self::Any => "Any",
        }
    }

    /// Todos, para busca por nome ou id.
    pub const ALL: [Self; 53] = [
        Self::Unknown,
        Self::English,
        Self::French,
        Self::Spanish,
        Self::German,
        Self::Italian,
        Self::Danish,
        Self::Dutch,
        Self::Japanese,
        Self::Icelandic,
        Self::Chinese,
        Self::Russian,
        Self::Polish,
        Self::Vietnamese,
        Self::Swedish,
        Self::Norwegian,
        Self::Finnish,
        Self::Turkish,
        Self::Portuguese,
        Self::Flemish,
        Self::Greek,
        Self::Korean,
        Self::Hungarian,
        Self::Hebrew,
        Self::Lithuanian,
        Self::Czech,
        Self::Hindi,
        Self::Romanian,
        Self::Thai,
        Self::Bulgarian,
        Self::PortugueseBr,
        Self::Arabic,
        Self::Ukrainian,
        Self::Persian,
        Self::Bengali,
        Self::Slovak,
        Self::Latvian,
        Self::SpanishLatino,
        Self::Catalan,
        Self::Tamil,
        Self::Telugu,
        Self::Malayalam,
        Self::Kannada,
        Self::Albanian,
        Self::Afrikaans,
        Self::Marathi,
        Self::Tagalog,
        Self::Urdu,
        Self::Romansh,
        Self::Mongolian,
        Self::Georgian,
        Self::Original,
        Self::Any,
    ];

    /// Pelo código ISO 639-1 que a base de metadados usa para o idioma
    /// original (`fr`, `pt`). O que a API v3 não conhece vira `Unknown`.
    #[must_use]
    pub fn from_iso639_1(code: &str) -> Self {
        match code.to_ascii_lowercase().as_str() {
            "en" => Self::English,
            "fr" => Self::French,
            "es" => Self::Spanish,
            "de" => Self::German,
            "it" => Self::Italian,
            "da" => Self::Danish,
            "nl" => Self::Dutch,
            "ja" => Self::Japanese,
            "is" => Self::Icelandic,
            // A base usa `cn` para cantonês.
            "zh" | "cn" => Self::Chinese,
            "ru" => Self::Russian,
            "pl" => Self::Polish,
            "vi" => Self::Vietnamese,
            "sv" => Self::Swedish,
            "no" | "nb" | "nn" => Self::Norwegian,
            "fi" => Self::Finnish,
            "tr" => Self::Turkish,
            "pt" => Self::Portuguese,
            "el" => Self::Greek,
            "ko" => Self::Korean,
            "hu" => Self::Hungarian,
            "he" => Self::Hebrew,
            "lt" => Self::Lithuanian,
            "cs" => Self::Czech,
            "hi" => Self::Hindi,
            "ro" => Self::Romanian,
            "th" => Self::Thai,
            "bg" => Self::Bulgarian,
            "ar" => Self::Arabic,
            "uk" => Self::Ukrainian,
            "fa" => Self::Persian,
            "bn" => Self::Bengali,
            "sk" => Self::Slovak,
            "lv" => Self::Latvian,
            "ca" => Self::Catalan,
            "ta" => Self::Tamil,
            "te" => Self::Telugu,
            "ml" => Self::Malayalam,
            "kn" => Self::Kannada,
            "sq" => Self::Albanian,
            "af" => Self::Afrikaans,
            "mr" => Self::Marathi,
            "tl" => Self::Tagalog,
            "ur" => Self::Urdu,
            "rm" => Self::Romansh,
            "mn" => Self::Mongolian,
            "ka" => Self::Georgian,
            _ => Self::Unknown,
        }
    }

    /// Pelo código ISO 639-2 das faixas de um arquivo (`por`, `eng`). Os
    /// códigos bibliográficos e os terminológicos valem os dois.
    #[must_use]
    pub fn from_iso639_2(code: &str) -> Self {
        let code = code.to_ascii_lowercase();
        let two = match code.as_str() {
            "eng" => "en",
            "fre" | "fra" => "fr",
            "spa" => "es",
            "ger" | "deu" => "de",
            "ita" => "it",
            "dan" => "da",
            "dut" | "nld" => "nl",
            "jpn" => "ja",
            "ice" | "isl" => "is",
            "chi" | "zho" | "cmn" | "yue" => "zh",
            "rus" => "ru",
            "pol" => "pl",
            "vie" => "vi",
            "swe" => "sv",
            "nor" | "nob" | "nno" => "no",
            "fin" => "fi",
            "tur" => "tr",
            "por" => "pt",
            "gre" | "ell" => "el",
            "kor" => "ko",
            "hun" => "hu",
            "heb" => "he",
            "lit" => "lt",
            "cze" | "ces" => "cs",
            "hin" => "hi",
            "rum" | "ron" => "ro",
            "tha" => "th",
            "bul" => "bg",
            "ara" => "ar",
            "ukr" => "uk",
            "per" | "fas" => "fa",
            "ben" => "bn",
            "slo" | "slk" => "sk",
            "lav" => "lv",
            "cat" => "ca",
            "tam" => "ta",
            "tel" => "te",
            "mal" => "ml",
            "kan" => "kn",
            "alb" | "sqi" => "sq",
            "afr" => "af",
            "mar" => "mr",
            "tgl" | "fil" => "tl",
            "urd" => "ur",
            "roh" => "rm",
            "mon" => "mn",
            "geo" | "kat" => "ka",
            other => other,
        };
        Self::from_iso639_1(two)
    }

    /// Pelo nome que a API v3 usa ("Portuguese (Brazil)").
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(name))
    }
}

/// Palavra por extenso em qualquer ponto do título, na ordem de referência.
const WORDS: &[(&[&str], Language)] = &[
    (&["english"], Language::English),
    (&["spanish"], Language::Spanish),
    (&["danish"], Language::Danish),
    (&["dutch"], Language::Dutch),
    (&["japanese"], Language::Japanese),
    (&["icelandic"], Language::Icelandic),
    (&["mandarin", "cantonese", "chinese"], Language::Chinese),
    (&["korean"], Language::Korean),
    (&["russian"], Language::Russian),
    (&["romanian"], Language::Romanian),
    (&["hindi"], Language::Hindi),
    (&["arabic"], Language::Arabic),
    (&["thai"], Language::Thai),
    (&["bulgarian"], Language::Bulgarian),
    (&["polish"], Language::Polish),
    (&["vietnamese"], Language::Vietnamese),
    (&["swedish"], Language::Swedish),
    (&["norwegian"], Language::Norwegian),
    (&["finnish"], Language::Finnish),
    (&["turkish"], Language::Turkish),
    (&["portuguese"], Language::Portuguese),
    (&["brazilian"], Language::PortugueseBr),
    (&["hungarian"], Language::Hungarian),
    (&["hebrew"], Language::Hebrew),
    (&["ukrainian"], Language::Ukrainian),
    (&["persian"], Language::Persian),
    (&["bengali"], Language::Bengali),
    (&["slovak"], Language::Slovak),
    (&["latvian"], Language::Latvian),
    (&["latino"], Language::SpanishLatino),
    (&["tamil"], Language::Tamil),
    (&["telugu"], Language::Telugu),
    (&["malayalam"], Language::Malayalam),
    (&["kannada"], Language::Kannada),
    (&["albanian"], Language::Albanian),
    (&["afrikaans"], Language::Afrikaans),
    (&["marathi"], Language::Marathi),
    (&["tagalog"], Language::Tagalog),
];

static CASE_SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?:(?i)(?<!SUB[\W|_|^]))(?:(?<english>\bEN\b)|(?<lithuanian>\bLT\b)|(?<czech>\bCZ\b)|",
        r"(?<polish>\bPL\b)|(?<bulgarian>\bBG\b)|(?<slovak>\bSK\b)|(?<german>\bDE\b)|",
        r"(?<spanish>\b(?<!DTS[._ -])ES\b))(?:(?i)(?![\W|_|^]SUB))",
    ))
});

static CASE_INSENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"(?i)(?:\W|_|^)(?<english>\beng\b)|",
        r"(?<italian>\b(?:ita|italian)\b)|",
        r"(?<german>(?:swiss)?german\b|videomann|ger[. ]dub|\bger\b)|",
        r"(?<flemish>flemish)|",
        r"(?<bulgarian>bgaudio)|",
        r"(?<romanian>rodubbed)|",
        r"(?<brazilian>\b(dublado|pt-BR)\b)|",
        r"(?<greek>greek)|",
        r"(?<french>\b(?:FR|VO|VF|VFF|VFQ|VFI|VF2|TRUEFRENCH|FRENCH|FRE|FRA)\b)|",
        r"(?<russian>\b(?:rus|ru)\b)|",
        r"(?<hungarian>\b(?:HUNDUB|HUN)\b)|",
        r"(?<hebrew>\b(?:HebDub|HebDubbed)\b)|",
        r"(?<polish>\b(?:PL\W?DUB|DUB\W?PL|LEK\W?PL|PL\W?LEK)\b)|",
        r"(?<chinese>\[(?:CH[ST]|BIG5|GB)\]|简|繁|字幕)|",
        r"(?<ukrainian>(?:(?:\dx)?UKR))|",
        r"(?<spanish>\b(?:español|castellano)\b)|",
        r"(?<catalan>\b(?:catalan?|catalán|català)\b)|",
        r"(?<latvian>\b(?:lat|lav|lv)\b)|",
        r"(?<telugu>\btel\b)|",
        r"(?<vietnamese>\bVIE\b)|",
        r"(?<japanese>\bJAP\b)|",
        r"(?<korean>\bKOR\b)|",
        r"(?<urdu>\burdu\b)|",
        r"(?<romansh>\b(?:romansh|rumantsch|romansch)\b)|",
        r"(?<mongolian>\b(?:mongolian|khalkha)\b)|",
        r"(?<georgian>\b(?:georgian|geo|ka|kat)\b)|",
        r"(?<original>\b(?:orig|original)\b)",
    ))
});

static GERMAN_DUAL: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)(?<!WEB[-_. ]?)\bDL\b"));

static GERMAN_MULTI: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\bML\b"));

const CASE_SENSITIVE_GROUPS: &[(&str, Language)] = &[
    ("english", Language::English),
    ("lithuanian", Language::Lithuanian),
    ("czech", Language::Czech),
    ("polish", Language::Polish),
    ("bulgarian", Language::Bulgarian),
    ("slovak", Language::Slovak),
    ("spanish", Language::Spanish),
    ("german", Language::German),
];

const CASE_INSENSITIVE_GROUPS: &[(&str, Language)] = &[
    ("english", Language::English),
    ("italian", Language::Italian),
    ("german", Language::German),
    ("flemish", Language::Flemish),
    ("greek", Language::Greek),
    ("french", Language::French),
    ("russian", Language::Russian),
    ("bulgarian", Language::Bulgarian),
    ("brazilian", Language::PortugueseBr),
    ("hungarian", Language::Hungarian),
    ("hebrew", Language::Hebrew),
    ("polish", Language::Polish),
    ("chinese", Language::Chinese),
    ("spanish", Language::Spanish),
    ("catalan", Language::Catalan),
    ("ukrainian", Language::Ukrainian),
    ("latvian", Language::Latvian),
    ("romanian", Language::Romanian),
    ("telugu", Language::Telugu),
    ("vietnamese", Language::Vietnamese),
    ("japanese", Language::Japanese),
    ("korean", Language::Korean),
    ("urdu", Language::Urdu),
    ("romansh", Language::Romansh),
    ("mongolian", Language::Mongolian),
    ("georgian", Language::Georgian),
    ("original", Language::Original),
];

fn from_groups(regex: &Regex, title: &str, groups: &[(&str, Language)], into: &mut Vec<Language>) {
    for captures in regex.captures_iter(title).map_while(Result::ok) {
        for (name, language) in groups {
            if captures.name(name).is_some() {
                into.push(*language);
            }
        }
    }
}

/// Idiomas do título, sem repetição, na ordem em que foram achados.
/// Nenhum achado é `[Unknown]`.
#[must_use]
pub fn parse_languages(title: &str) -> Vec<Language> {
    let lower = title.to_lowercase();
    let mut languages: Vec<Language> = WORDS
        .iter()
        .filter(|(words, _)| words.iter().any(|word| lower.contains(word)))
        .map(|(_, language)| *language)
        .collect();
    from_groups(
        &CASE_SENSITIVE,
        title,
        CASE_SENSITIVE_GROUPS,
        &mut languages,
    );
    from_groups(
        &CASE_INSENSITIVE,
        title,
        CASE_INSENSITIVE_GROUPS,
        &mut languages,
    );

    if languages.is_empty() {
        languages.push(Language::Unknown);
    }
    if languages == [Language::German] {
        if is_match(&GERMAN_DUAL, title) {
            languages.push(Language::Original);
        } else if is_match(&GERMAN_MULTI, title) {
            languages.push(Language::Original);
            languages.push(Language::English);
        }
    }
    let mut seen = Vec::with_capacity(languages.len());
    languages.retain(|language| {
        let new = !seen.contains(language);
        seen.push(*language);
        new
    });
    languages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_audio_brasileiro() {
        assert_eq!(
            parse_languages("Filme 2025 1080p WEB-DL Dual-Audio Brazilian Original"),
            vec![Language::PortugueseBr, Language::Original]
        );
        assert_eq!(
            parse_languages("Filme.2025.1080p.DUBLADO.WEB-DL"),
            vec![Language::PortugueseBr]
        );
        assert_eq!(
            parse_languages("Filme.2025.1080p.WEB-DL"),
            vec![Language::Unknown]
        );
    }

    #[test]
    fn sigla_so_em_maiusculas() {
        assert_eq!(
            parse_languages("Filme.2025.EN.1080p"),
            vec![Language::English]
        );
        assert_eq!(
            parse_languages("Filme.2025.DTS-ES.1080p"),
            vec![Language::Unknown]
        );
    }
}

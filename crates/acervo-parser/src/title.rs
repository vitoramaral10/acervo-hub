//! Formas canônicas de título, para comparar o do release com o do filme.

use std::sync::LazyLock;

use fancy_regex::Regex;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::common::{regex, replace_all};

/// Artigos e conjunções soltos saem, e tudo que não é letra ou número também:
/// "The Lord of the Rings" e "Lord.Rings" dão o mesmo título limpo.
static NORMALIZE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)((?:\b|_)(?<!^|[^a-zA-Z0-9_']\w[^a-zA-Z0-9_'])([aà](?!$|[^a-zA-Z0-9_']\w[^a-zA-Z0-9_'])|an|the|and|or|of)(?!$)(?:\b|_))|\W|_",
    )
});

static SPECIAL_WORD: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(part|special|edition|christmas)\b\s?"));

static PUNCTUATION: LazyLock<Regex> = LazyLock::new(|| regex(r"[^\w\s]"));

static DUPLICATE_SPACES: LazyLock<Regex> = LazyLock::new(|| regex(r"\s{2,}"));

/// Tira acento: decompõe, descarta as marcas, recompõe.
#[must_use]
pub fn remove_accents(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_combining_mark(*c))
        .nfc()
        .collect()
}

fn replace_german_umlauts(text: &str) -> String {
    text.replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .replace('Ä', "Ae")
        .replace('Ö', "Oe")
        .replace('Ü', "Ue")
        .replace('ß', "ss")
}

/// O título limpo com que release e filme são comparados.
#[must_use]
pub fn clean_movie_title(title: &str) -> String {
    if title.trim().is_empty() {
        return title.to_owned();
    }
    // Título só de número fica como está: "1917" não perde nada.
    let digits = title.trim().trim_start_matches(['+', '-']);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) && digits.len() <= 19 {
        return title.to_owned();
    }
    remove_accents(&replace_german_umlauts(
        &replace_all(&NORMALIZE, title, "").to_lowercase(),
    ))
}

/// Título em palavras minúsculas, sem pontuação: a forma que a agregação de
/// idiomas procura dentro do nome do release.
#[must_use]
pub fn normalize_movie_title(title: &str) -> String {
    let title = replace_all(&SPECIAL_WORD, title, "");
    let title = replace_all(&PUNCTUATION, &title, " ");
    let title = replace_all(&DUPLICATE_SPACES, &title, " ");
    title.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titulo_limpo() {
        assert_eq!(clean_movie_title("The Lord of the Rings"), "thelordrings");
        assert_eq!(clean_movie_title("Lord.of.the.Rings"), "lordrings");
        assert_eq!(clean_movie_title("The Break-Up"), "thebreakup");
        assert_eq!(clean_movie_title("Coração Partido"), "coracaopartido");
        assert_eq!(clean_movie_title("1917"), "1917");
        assert_eq!(clean_movie_title("A Quiet Place"), "aquietplace");
        assert_eq!(
            clean_movie_title("Mission: Impossible"),
            "missionimpossible"
        );
    }

    #[test]
    fn titulo_normalizado() {
        assert_eq!(
            normalize_movie_title("Crank: High Voltage"),
            "crank high voltage"
        );
        assert_eq!(
            normalize_movie_title("Star Wars Special Edition"),
            "star wars"
        );
    }
}

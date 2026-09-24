//! Idiomas do release depois de casado com o filme.
//!
//! "Original" vira o idioma original do filme, e nome sem idioma nenhum
//! também — a referência presume que o release está no original. Por isso
//! "Brazilian Dual-Audio Original" passa num perfil que pede o original, e
//! "Dublado" sozinho, não.

use std::sync::LazyLock;

use acervo_parser::{Language, ParsedMovie, normalize_movie_title, parse_languages};
use fancy_regex::Regex;

use crate::{Indexer, Release, Target};

static MULTI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[_. ](?<multi>multi)[_. ]").unwrap_or_else(|e| panic!("{e}"))
});

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

/// Tira do nome do release o trecho onde o título do filme estaria. A
/// posição vem do nome normalizado e é aplicada ao original, em UTF-16, como
/// na referência.
fn without_title(tokens: &str, normalized_tokens: &str, normalized_title: &str) -> Option<String> {
    let at = normalized_tokens.find(normalized_title)?;
    let start = normalized_tokens[..at].encode_utf16().count();
    let length = normalized_title.encode_utf16().count();
    let from = utf16_to_byte(tokens, start)?;
    let to = utf16_to_byte(tokens, start + length)?;
    Some(format!("{}{}", &tokens[..from], &tokens[to..]))
}

pub(crate) fn aggregate(
    parsed: &ParsedMovie,
    release: &Release,
    movie: &Target,
    indexer: Option<&Indexer>,
) -> Vec<Language> {
    let mut languages = parsed.languages.clone();
    if release.languages.is_empty() {
        // Idioma que só aparece porque está no título do filme ("The Italian
        // Job") não é idioma do release.
        let tokens = &parsed.simple_release_title;
        let title_languages = parse_languages(&movie.title);
        let mut remove = Vec::new();
        let mut remaining = tokens.clone();
        if !title_languages.contains(&Language::Unknown) {
            let normalized_tokens = normalize_movie_title(tokens);
            let normalized_title = normalize_movie_title(&movie.title);
            if let Some(cut) = without_title(tokens, &normalized_tokens, &normalized_title) {
                remaining = cut;
                remove.extend(title_languages);
            }
        }
        let still_there = parse_languages(&remaining);
        remove.retain(|l| !still_there.contains(l));
        languages.retain(|l| !remove.contains(l));
    } else {
        languages.clone_from(&release.languages);
    }

    if let Some(indexer) = indexer.filter(|i| !i.multi_languages.is_empty())
        && MULTI.is_match(&release.title).unwrap_or(false)
    {
        if languages.is_empty() || languages == [Language::Unknown] {
            languages.clone_from(&indexer.multi_languages);
        } else {
            for language in &indexer.multi_languages {
                if !languages.contains(language) {
                    languages.push(*language);
                }
            }
        }
    }

    if languages.is_empty() || languages == [Language::Unknown] {
        languages = vec![movie.original_language];
    }
    if let Some(at) = languages.iter().position(|l| *l == Language::Original) {
        languages.remove(at);
        if languages.contains(&movie.original_language) {
            languages.push(Language::Unknown);
        } else {
            languages.push(movie.original_language);
        }
    }
    languages
}

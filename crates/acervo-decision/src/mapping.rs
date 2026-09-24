//! A que filme da biblioteca o release pertence.
//!
//! Na ordem da referência: id do `IMDb` que o indexador mandou (procurado na
//! biblioteca inteira, não só no filme buscado), id do `TMDb`, e só então o
//! título. Id que aponta para um filme de outro ano não vale: indexador erra
//! id com frequência.

use acervo_parser::{ParsedMovie, clean_movie_title};

use crate::{Release, Target};

fn year_fits(parsed: &ParsedMovie, movie: &Target) -> bool {
    match parsed.year {
        Some(year) if year >= 1800 => {
            movie.year == Some(year) || movie.secondary_year == Some(year)
        }
        _ => true,
    }
}

/// `tt` mais o número com pelo menos sete dígitos, como a referência grava.
fn normalize_imdb(id: u32) -> String {
    format!("tt{id:07}")
}

/// Arábico para romano minúsculo, de 1 a 20.
fn roman(number: u32) -> String {
    const PARTS: [(u32, &str); 5] = [(10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut rest = number;
    let mut out = String::new();
    for (value, symbol) in PARTS {
        while rest >= value {
            out.push_str(symbol);
            rest -= value;
        }
    }
    out
}

/// Algum título do filme bate com algum do release, direto ou trocando
/// algarismo por numeral romano ("rocky2" e "rockyii").
fn titles_match(possible: &[String], parsed_clean: &[String]) -> bool {
    possible.iter().any(|title| {
        parsed_clean.contains(title)
            || (1..=20).any(|n| {
                let arabic = n.to_string();
                let numeral = roman(n);
                parsed_clean.contains(&title.replace(&arabic, &numeral))
                    || parsed_clean
                        .iter()
                        .any(|t| t.replace(&arabic, &numeral) == *title)
            })
    })
}

pub(crate) fn find(
    library: &[Target],
    parsed: &ParsedMovie,
    release: &Release,
    searched: Option<i64>,
) -> Option<i64> {
    if let Some(imdb) = release.imdb_id.filter(|id| *id != 0) {
        let imdb = normalize_imdb(imdb);
        if let Some(movie) = library
            .iter()
            .find(|m| m.imdb_id.as_deref() == Some(imdb.as_str()))
            .filter(|m| year_fits(parsed, m))
        {
            return Some(movie.id);
        }
    }
    if let Some(tmdb) = release.tmdb_id.filter(|id| *id != 0)
        && let Some(movie) = library
            .iter()
            .find(|m| m.tmdb_id == tmdb)
            .filter(|m| year_fits(parsed, m))
    {
        return Some(movie.id);
    }

    let parsed_clean: Vec<String> = parsed.titles.iter().map(|t| clean_movie_title(t)).collect();
    if let Some(searched) = searched {
        let movie = library.iter().find(|m| m.id == searched)?;
        if titles_match(&movie.clean_titles, &parsed_clean) && year_fits(parsed, movie) {
            return Some(movie.id);
        }
        // Id do TMDb que aponta para o filme buscado vale mesmo com título ou
        // ano diferente. O do IMDb a referência compara sem o "tt" contra com
        // o "tt", e nunca casa; o porte mantém o comportamento.
        if release
            .tmdb_id
            .is_some_and(|id| id != 0 && id == movie.tmdb_id)
        {
            return Some(movie.id);
        }
        return None;
    }

    library
        .iter()
        .find(|m| titles_match(&m.clean_titles, &parsed_clean) && year_fits(parsed, m))
        .map(|m| m.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn romanos() {
        assert_eq!(roman(1), "i");
        assert_eq!(roman(4), "iv");
        assert_eq!(roman(9), "ix");
        assert_eq!(roman(14), "xiv");
        assert_eq!(roman(20), "xx");
        assert!(titles_match(&["rocky2".into()], &["rockyii".into()]));
        assert!(!titles_match(&["rocky2".into()], &["rocky3".into()]));
    }
}

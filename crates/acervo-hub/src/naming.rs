//! Nome do arquivo de um filme, como o gerenciador de filmes daria.
//!
//! O formato em produção é `{Movie CleanTitle} ({Release Year}) {imdb-{ImdbId}}`
//! para pasta e arquivo. A pasta nasce com esse nome quando o filme entra, e
//! não é renomeada depois; então, quando ela já está no formato, o arquivo
//! leva o nome dela — é o único lugar onde o título em inglês da base de
//! metadados aparece. Pasta de um formato antigo cai no título original.
//! Contra os 134 arquivos de um gerenciador real, a regra acerta todos.

use std::path::Path;
use std::sync::LazyLock;

use acervo_parser::remove_accents;
use acervo_store::Movie;
use fancy_regex::Regex;

/// `ScenifyRemoveChars` da referência: pontuação solta entre espaços, apóstrofo
/// e afins no fim de palavra (menos em contração), e todo tipo de parêntese.
static REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?<=\s)(,|<|>|/|\\|;|:|'|"|\||`|’|~|!|\?|@|\$|%|\^|\*|-|_|=){1}(?=\s)|('|`|’|:|\?|,)(?=(?:(?:s|m|t|ve|ll|d|re)\s)|\s|$)|(\(|\)|\[|\]|\{|\})"#,
    )
    .expect("padrão válido")
});

/// `{Movie CleanTitle}`.
#[must_use]
pub fn clean_title(title: &str) -> String {
    let title = title.replace('&', "and").replace(['/', '\\'], " ");
    remove_accents(&REMOVE.replace_all(&title, ""))
}

/// O que ainda não pode ir em nome de arquivo, trocado como a referência
/// troca, com dois-pontos no modo "smart" (`a: b` vira `a - b`); e espaço
/// repetido, que a limpeza do título deixa onde tirou pontuação, vira um.
fn file_safe(name: &str) -> String {
    let name = name.replace(": ", " - ");
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '\\' | '/' => out.push('+'),
            '?' => out.push('!'),
            '*' | ':' => out.push('-'),
            '<' | '>' | '|' | '"' => {}
            ' ' if out.ends_with(' ') => {}
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// A pasta tem a forma `Título (AAAA) {imdb-ttN}` com o `IMDb` deste filme? O
/// ano não precisa bater: ele pode ter mudado na base depois de a pasta
/// nascer, e o arquivo segue a pasta.
fn in_format(folder: &str, imdb: &str) -> bool {
    let Some(rest) = folder.strip_suffix(&format!(" {{imdb-{imdb}}}")) else {
        return false;
    };
    let Some(title) = rest
        .strip_suffix(')')
        .and_then(|r| r.get(..r.len().saturating_sub(4)))
        .and_then(|r| r.strip_suffix(" ("))
    else {
        return false;
    };
    let year = &rest[rest.len() - 5..rest.len() - 1];
    !title.is_empty() && year.bytes().all(|b| b.is_ascii_digit())
}

/// `{Movie CleanTitle} ({Release Year}) {imdb-{ImdbId}}`: o nome da pasta de
/// um filme novo, e o do arquivo.
#[must_use]
pub fn formatted_name(title: &str, year: Option<u16>, imdb: Option<&str>) -> String {
    let suffix = match (year, imdb) {
        (Some(year), Some(imdb)) => format!(" ({year}) {{imdb-{imdb}}}"),
        (Some(year), None) => format!(" ({year})"),
        (None, Some(imdb)) => format!(" {{imdb-{imdb}}}"),
        (None, None) => String::new(),
    };
    file_safe(&format!("{}{suffix}", clean_title(title)))
}

/// Nome do arquivo, sem extensão. `metadata_title` é o título em inglês da
/// base de metadados, quando o acervo o tem: com ele o nome sai exatamente
/// como o gerenciador daria, sem depender da pasta.
#[must_use]
pub fn movie_file_stem(movie: &Movie, metadata_title: Option<&str>) -> String {
    let suffix = match (movie.year, &movie.imdb_id) {
        (Some(year), Some(imdb)) => format!(" ({year}) {{imdb-{imdb}}}"),
        (Some(year), None) => format!(" ({year})"),
        (None, Some(imdb)) => format!(" {{imdb-{imdb}}}"),
        (None, None) => String::new(),
    };
    let folder = Path::new(&movie.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if let Some(title) = metadata_title {
        return file_safe(&format!("{}{suffix}", clean_title(title)));
    }
    // Filme em inglês: o título original é o da base de metadados, e é
    // dele — atual — que o nome sai. Nos outros, o título em inglês só está
    // na pasta.
    let english = movie.original_language.as_deref() == Some("English");
    if !english
        && let Some(imdb) = &movie.imdb_id
        && in_format(folder, imdb)
    {
        return folder.to_owned();
    }
    let title = movie.original_title.as_deref().unwrap_or(&movie.title);
    file_safe(&format!("{}{suffix}", clean_title(title)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie(title: &str, year: Option<u16>, imdb: Option<&str>, path: &str) -> Movie {
        foreign(title, "English", year, imdb, path)
    }

    fn foreign(
        title: &str,
        language: &str,
        year: Option<u16>,
        imdb: Option<&str>,
        path: &str,
    ) -> Movie {
        Movie {
            tmdb_id: 1,
            imdb_id: imdb.map(str::to_owned),
            title: "Título traduzido".into(),
            original_title: Some(title.into()),
            original_language: Some(language.into()),
            year,
            status: None,
            minimum_availability: None,
            monitored: true,
            quality_profile: None,
            path: path.into(),
            added: None,
            file: None,
            runtime: 0,
            secondary_year: None,
            clean_title: None,
            alternate_titles: Vec::new(),
            available: true,
            in_cinemas: None,
            digital_release: None,
            physical_release: None,
            overview: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn titulo_limpo_como_a_referencia() {
        assert_eq!(
            clean_title("Now You See Me: Now You Don't"),
            "Now You See Me Now You Don't"
        );
        assert_eq!(clean_title("Fast & Furious"), "Fast and Furious");
        assert_eq!(clean_title("Amélie (Le Fabuleux)"), "Amelie Le Fabuleux");
        assert_eq!(clean_title("Face/Off"), "Face Off");
        // Apóstrofo antes de "s " sai; em "Don't" no fim, fica.
        assert_eq!(clean_title("Who's There?"), "Whos There");
    }

    #[test]
    fn pasta_no_formato_vale_o_nome_dela() {
        // Título em inglês da base de metadados, que a API não expõe.
        let m = foreign(
            "13 Jours, 13 Nuits",
            "French",
            Some(2025),
            Some("tt28291010"),
            "/media/movies/13 Days 13 Nights (2025) {imdb-tt28291010}",
        );
        assert_eq!(
            movie_file_stem(&m, None),
            "13 Days 13 Nights (2025) {imdb-tt28291010}"
        );
    }

    #[test]
    fn titulo_da_base_de_metadados_decide_quando_existe() {
        let m = foreign(
            "13 Jours, 13 Nuits",
            "French",
            Some(2025),
            Some("tt28291010"),
            "/media/movies/13 Jours (2025)",
        );
        assert_eq!(
            movie_file_stem(&m, Some("13 Days, 13 Nights")),
            "13 Days 13 Nights (2025) {imdb-tt28291010}"
        );
    }

    #[test]
    fn pasta_antiga_cai_no_titulo_original() {
        let m = movie(
            "Now You See Me: Now You Don't",
            Some(2025),
            Some("tt4712810"),
            "/media/movies/Now You See Me - Now You Don't (2025)",
        );
        assert_eq!(
            movie_file_stem(&m, None),
            "Now You See Me Now You Don't (2025) {imdb-tt4712810}"
        );
        // Pasta de outro filme com o mesmo formato não conta.
        let m = movie(
            "Mean Girls",
            Some(2024),
            Some("tt11762114"),
            "/media/movies/Mean Girls (2004) {imdb-tt0377092}",
        );
        assert_eq!(
            movie_file_stem(&m, None),
            "Mean Girls (2024) {imdb-tt11762114}"
        );
        // Filme em inglês: título e ano atuais, mesmo com a pasta velha.
        let m = movie(
            "Apex",
            Some(2025),
            Some("tt16431404"),
            "/media/movies/APEX (2026) {imdb-tt16431404}",
        );
        assert_eq!(movie_file_stem(&m, None), "Apex (2025) {imdb-tt16431404}");
        // Em outra língua, a pasta no formato vale com qualquer ano.
        let m = foreign(
            "Le Film",
            "French",
            Some(2025),
            Some("tt1"),
            "/media/movies/The Film (2026) {imdb-tt1}",
        );
        assert_eq!(movie_file_stem(&m, None), "The Film (2026) {imdb-tt1}");
    }

    /// Contra a lista de filmes de um gerenciador real (`GET /api/v3/movie`),
    /// fora do repositório: todo arquivo existente tem de ter o nome que a
    /// regra dá, a menos do ano — o arquivo guarda o ano do dia em que foi
    /// importado, e a base pode tê-lo corrigido depois.
    #[test]
    #[ignore = "precisa de ACERVO_NAMING_CORPUS"]
    fn corpus() {
        let path = std::env::var("ACERVO_NAMING_CORPUS").expect("ACERVO_NAMING_CORPUS");
        let movies: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut checked = 0;
        let mut wrong = Vec::new();
        for m in &movies {
            let Some(file) = m["movieFile"]["relativePath"].as_str() else {
                continue;
            };
            let expected = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
            let entry = foreign(
                m["originalTitle"].as_str().unwrap_or_default(),
                m["originalLanguage"]["name"].as_str().unwrap_or_default(),
                m["year"].as_u64().and_then(|y| u16::try_from(y).ok()),
                m["imdbId"].as_str(),
                m["path"].as_str().unwrap_or_default(),
            );
            checked += 1;
            let got = movie_file_stem(&entry, None);
            let without_year = |s: &str| {
                s.rsplit_once(" (")
                    .map(|(title, rest)| format!("{title}{}", &rest[rest.find(')').unwrap_or(0)..]))
                    .unwrap_or_default()
            };
            if got != expected && without_year(&got) != without_year(expected) {
                wrong.push(format!("{expected:?} ≠ {got:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} de {checked}:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
        eprintln!("{checked} arquivos conferidos");
    }
}

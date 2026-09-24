//! Tradução da query string Torznab para uma consulta do domínio.

use acervo_indexers::SearchQuery;

use crate::TorznabError;

/// Maior página que a superfície entrega, anunciada em `t=caps`.
pub const MAX_RESULTS: u16 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Function {
    Caps,
    Search(SearchQuery),
}

/// Interpreta os parâmetros de `/{indexador}/api`.
///
/// Parâmetro desconhecido é ignorado: os consumidores mandam `extended`,
/// `rid`, `tvmazeid` e outros conforme a versão, e recusar qualquer um deles
/// derrubaria a integração inteira. Parâmetro conhecido com valor ilegível,
/// ao contrário, é erro — ignorá-lo transformaria uma busca específica numa
/// busca genérica.
///
/// # Errors
///
/// `t` ausente ou desconhecido, número ilegível, `ep` sem `season`.
pub fn parse(params: &[(String, String)]) -> Result<Function, TorznabError> {
    let value = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.trim())
            .filter(|value| !value.is_empty())
    };

    let function = value("t").ok_or(TorznabError::MissingParameter("t"))?;
    let term = value("q").unwrap_or_default();
    let mut query = match function {
        "caps" => return Ok(Function::Caps),
        "search" => SearchQuery::general(term),
        "tvsearch" => {
            let mut query = SearchQuery::tv(term);
            match (number::<u16>(value("season"), "season")?, value("ep")) {
                (Some(season), episode) => {
                    query = query.with_episode(season, episode.unwrap_or_default());
                }
                (None, Some(_)) => return Err(TorznabError::IncorrectParameter("ep")),
                (None, None) => {}
            }
            if let Some(id) = number(value("tvdbid"), "tvdbid")? {
                query = query.with_tvdb_id(id);
            }
            query
        }
        "movie" => {
            let mut query = SearchQuery::movie(term);
            if let Some(year) = number(value("year"), "year")? {
                query = query.with_year(year);
            }
            if let Some(id) = number(value("tmdbid"), "tmdbid")? {
                query = query.with_tmdb_id(id);
            }
            query
        }
        _ => return Err(TorznabError::NoSuchFunction),
    };

    if let Some(id) = value("imdbid") {
        query = query.with_imdb_id(imdb_id(id)?);
    }
    if let Some(list) = value("cat") {
        let categories = list
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| {
                id.parse::<u32>()
                    .map_err(|_| TorznabError::IncorrectParameter("cat"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        query = query.with_categories(categories);
    }
    let limit = number::<u16>(value("limit"), "limit")?.unwrap_or(MAX_RESULTS);
    query = query
        .with_limit(limit.clamp(1, MAX_RESULTS))
        .with_offset(number(value("offset"), "offset")?.unwrap_or(0));

    Ok(Function::Search(query))
}

fn number<T: std::str::FromStr>(
    value: Option<&str>,
    name: &'static str,
) -> Result<Option<T>, TorznabError> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| TorznabError::IncorrectParameter(name))
        })
        .transpose()
}

/// Uns consumidores mandam `tt0123456`, outros só os dígitos.
fn imdb_id(value: &str) -> Result<String, TorznabError> {
    let digits = value.strip_prefix("tt").unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TorznabError::IncorrectParameter("imdbid"));
    }
    Ok(format!("tt{digits}"))
}

#[cfg(test)]
mod tests {
    use acervo_indexers::SearchMode;

    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn search(pairs: &[(&str, &str)]) -> SearchQuery {
        match parse(&params(pairs)).unwrap() {
            Function::Search(query) => query,
            Function::Caps => panic!("esperava busca"),
        }
    }

    #[test]
    fn busca_de_episodio_como_o_gerenciador_de_series_manda() {
        let query = search(&[
            ("t", "tvsearch"),
            ("cat", "5000,5040"),
            ("extended", "1"),
            ("apikey", "x"),
            ("offset", "0"),
            ("limit", "100"),
            ("tvdbid", "42"),
            ("season", "2"),
            ("ep", "3"),
            ("imdbid", "0123456"),
        ]);

        assert_eq!(
            query.mode,
            SearchMode::Tv {
                season: Some(2),
                episode: Some("3".into()),
                tvdb_id: Some(42),
                imdb_id: Some("tt0123456".into()),
            }
        );
        assert_eq!(query.term, None);
        assert_eq!(query.categories, [5000, 5040]);
        assert_eq!(query.limit, Some(100));
    }

    #[test]
    fn busca_de_filme_com_termo_vazio_vira_sem_termo() {
        let query = search(&[
            ("t", "movie"),
            ("q", " "),
            ("tmdbid", "7"),
            ("year", "2026"),
        ]);

        assert_eq!(query.term, None);
        assert_eq!(
            query.mode,
            SearchMode::Movie {
                year: Some(2026),
                tmdb_id: Some(7),
                imdb_id: None,
            }
        );
    }

    #[test]
    fn limite_fora_da_faixa_e_contido() {
        assert_eq!(
            search(&[("t", "search"), ("limit", "5000")]).limit,
            Some(100)
        );
        assert_eq!(search(&[("t", "search"), ("limit", "0")]).limit, Some(1));
        assert_eq!(search(&[("t", "search")]).limit, Some(100));
    }

    #[test]
    fn parametro_conhecido_ilegivel_e_erro_e_nao_busca_generica() {
        for pairs in [
            &[("t", "tvsearch"), ("season", "S01")][..],
            &[("t", "tvsearch"), ("ep", "3")],
            &[("t", "tvsearch"), ("tvdbid", "abc")],
            &[("t", "movie"), ("imdbid", "tt12x")],
            &[("t", "search"), ("cat", "5000,tv")],
            &[("t", "search"), ("offset", "-1")],
        ] {
            assert!(
                matches!(
                    parse(&params(pairs)),
                    Err(TorznabError::IncorrectParameter(_))
                ),
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn funcao_ausente_ou_desconhecida() {
        assert!(matches!(
            parse(&params(&[("q", "x")])),
            Err(TorznabError::MissingParameter("t"))
        ));
        assert!(matches!(
            parse(&params(&[("t", "music")])),
            Err(TorznabError::NoSuchFunction)
        ));
        assert_eq!(parse(&params(&[("t", "caps")])).unwrap(), Function::Caps);
    }
}

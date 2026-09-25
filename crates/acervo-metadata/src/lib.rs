//! Metadados de filmes pela API v3 do The Movie Database.
//!
//! Com o acervo dono do catálogo, é daqui que vêm título, ano, duração, datas
//! de lançamento (que decidem quando um filme fica disponível) e títulos
//! alternativos (que o casamento de release usa). Uma chamada por filme: o
//! detalhe já traz datas, títulos alternativos e traduções anexados.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

const BASE: &str = "https://api.themoviedb.org/3/";
const IMAGES: &str = "https://image.tmdb.org/t/p/original";

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("chave do TMDB recusada")]
    InvalidKey,

    #[error("filme {0} não existe no TMDB")]
    NotFound(u32),

    #[error("TMDB respondeu {0}")]
    Status(reqwest::StatusCode),

    #[error("falha ao falar com o TMDB: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("url inválida: {0}")]
    Url(#[from] url::ParseError),
}

/// Um filme, com o que o catálogo guarda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieMetadata {
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    /// Título em inglês — o que dá nome à pasta e ao arquivo.
    pub title: String,
    pub original_title: String,
    /// ISO 639-1.
    pub original_language: String,
    /// Título na língua pedida, se houver tradução.
    pub localized_title: Option<String>,
    pub overview: Option<String>,
    pub year: Option<u16>,
    /// Minutos; zero é desconhecido.
    pub runtime: u32,
    /// `Rumored`, `Planned`, `In Production`, `Post Production`, `Released`
    /// ou `Canceled`.
    pub status: String,
    /// `AAAA-MM-DD`.
    pub in_cinemas: Option<String>,
    pub digital_release: Option<String>,
    pub physical_release: Option<String>,
    pub alternate_titles: Vec<String>,
    pub poster: Option<String>,
    pub fanart: Option<String>,
}

#[derive(Deserialize)]
struct RawMovie {
    id: u32,
    imdb_id: Option<String>,
    title: String,
    original_title: String,
    original_language: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    runtime: Option<u32>,
    #[serde(default)]
    status: String,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    release_dates: Option<RawReleaseDates>,
    #[serde(default)]
    alternative_titles: Option<RawAlternativeTitles>,
    #[serde(default)]
    translations: Option<RawTranslations>,
}

#[derive(Deserialize)]
struct RawReleaseDates {
    results: Vec<RawCountryReleases>,
}

#[derive(Deserialize)]
struct RawCountryReleases {
    iso_3166_1: String,
    release_dates: Vec<RawRelease>,
}

#[derive(Deserialize)]
struct RawRelease {
    release_date: String,
    /// 1 estreia, 2 limitado, 3 cinema, 4 digital, 5 físico, 6 TV.
    #[serde(rename = "type")]
    kind: u8,
}

#[derive(Deserialize)]
struct RawAlternativeTitles {
    titles: Vec<RawTitle>,
}

#[derive(Deserialize)]
struct RawTitle {
    title: String,
}

#[derive(Deserialize)]
struct RawTranslations {
    translations: Vec<RawTranslation>,
}

#[derive(Deserialize)]
struct RawTranslation {
    iso_3166_1: String,
    iso_639_1: String,
    data: RawTranslationData,
}

#[derive(Deserialize)]
struct RawTranslationData {
    #[serde(default)]
    title: String,
    #[serde(default)]
    overview: String,
}

/// Data de um tipo de lançamento: a dos EUA se houver, senão a mais cedo de
/// qualquer país.
fn release(dates: &[RawCountryReleases], kinds: &[u8]) -> Option<String> {
    let pick = |country: Option<&str>| {
        dates
            .iter()
            .filter(|c| country.is_none_or(|wanted| c.iso_3166_1 == wanted))
            .flat_map(|c| &c.release_dates)
            .filter(|r| kinds.contains(&r.kind))
            .map(|r| {
                r.release_date
                    .get(..10)
                    .unwrap_or(&r.release_date)
                    .to_owned()
            })
            .min()
    };
    pick(Some("US")).or_else(|| pick(None))
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty())
}

impl RawMovie {
    fn into_metadata(self, language: &str) -> MovieMetadata {
        let (lang, region) = language.split_once('-').unwrap_or((language, ""));
        let translation = self.translations.as_ref().and_then(|t| {
            t.translations
                .iter()
                .find(|t| t.iso_639_1 == lang && t.iso_3166_1 == region)
                .or_else(|| t.translations.iter().find(|t| t.iso_639_1 == lang))
        });
        let dates = self
            .release_dates
            .as_ref()
            .map_or(&[][..], |d| d.results.as_slice());
        let release_date = non_empty(self.release_date);
        MovieMetadata {
            tmdb_id: self.id,
            imdb_id: non_empty(self.imdb_id),
            localized_title: translation
                .map(|t| t.data.title.clone())
                .filter(|t| !t.is_empty() && *t != self.title),
            overview: translation
                .map(|t| t.data.overview.clone())
                .filter(|o| !o.is_empty())
                .or_else(|| non_empty(self.overview)),
            year: release_date
                .as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok()),
            runtime: self.runtime.unwrap_or(0),
            status: self.status,
            in_cinemas: release(dates, &[1, 2, 3]).or(release_date),
            digital_release: release(dates, &[4]),
            physical_release: release(dates, &[5]),
            alternate_titles: self
                .alternative_titles
                .map(|a| a.titles.into_iter().map(|t| t.title).collect())
                .unwrap_or_default(),
            poster: self.poster_path.map(|p| format!("{IMAGES}{p}")),
            fanart: self.backdrop_path.map(|p| format!("{IMAGES}{p}")),
            title: self.title,
            original_title: self.original_title,
            original_language: self.original_language,
        }
    }
}

/// Cliente da API v3, autenticado pela chave.
#[derive(Clone)]
pub struct Tmdb {
    base: Url,
    key: String,
    /// Língua do título localizado, como `pt-BR`.
    language: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Tmdb {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tmdb")
            .field("base", &self.base.as_str())
            .field("key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Tmdb {
    /// # Errors
    ///
    /// Cliente HTTP impossível de montar.
    pub fn new(key: &str, language: &str, timeout: Duration) -> Result<Self, MetadataError> {
        Self::with_base(BASE, key, language, timeout)
    }

    /// Como [`Tmdb::new`], noutro endereço — para testes.
    ///
    /// # Errors
    ///
    /// Endereço inválido ou cliente HTTP impossível de montar.
    pub fn with_base(
        base: &str,
        key: &str,
        language: &str,
        timeout: Duration,
    ) -> Result<Self, MetadataError> {
        Ok(Self {
            base: Url::parse(base)?,
            key: key.trim().to_owned(),
            language: language.to_owned(),
            http: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(concat!("acervo-hub/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<T>, MetadataError> {
        let response = self
            .http
            .get(self.base.join(path)?)
            .query(&[("api_key", self.key.as_str())])
            .query(query)
            .send()
            .await?;
        match response.status() {
            status if status.is_success() => Ok(Some(response.json().await?)),
            reqwest::StatusCode::UNAUTHORIZED => Err(MetadataError::InvalidKey),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            status => Err(MetadataError::Status(status)),
        }
    }

    /// Confere a chave.
    ///
    /// # Errors
    ///
    /// Chave recusada ou TMDB inalcançável.
    pub async fn validate(&self) -> Result<(), MetadataError> {
        self.get::<serde::de::IgnoredAny>("configuration", &[])
            .await
            .map(|_| ())
    }

    /// O filme pelo id do TMDB.
    ///
    /// # Errors
    ///
    /// Filme inexistente, chave recusada ou TMDB inalcançável.
    pub async fn movie(&self, tmdb_id: u32) -> Result<MovieMetadata, MetadataError> {
        let raw: RawMovie = self
            .get(
                &format!("movie/{tmdb_id}"),
                &[(
                    "append_to_response",
                    "release_dates,alternative_titles,translations",
                )],
            )
            .await?
            .ok_or(MetadataError::NotFound(tmdb_id))?;
        Ok(raw.into_metadata(&self.language))
    }

    /// O filme pelo id do `IMDb`.
    ///
    /// # Errors
    ///
    /// Chave recusada ou TMDB inalcançável.
    pub async fn find_imdb(&self, imdb_id: &str) -> Result<Option<u32>, MetadataError> {
        #[derive(Deserialize)]
        struct Found {
            movie_results: Vec<Id>,
        }
        #[derive(Deserialize)]
        struct Id {
            id: u32,
        }
        let found: Option<Found> = self
            .get(
                &format!("find/{imdb_id}"),
                &[("external_source", "imdb_id")],
            )
            .await?;
        Ok(found.and_then(|f| f.movie_results.first().map(|m| m.id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &serde_json::Value) -> RawMovie {
        serde_json::from_value(json.clone()).unwrap()
    }

    #[test]
    fn datas_preferem_os_eua_e_titulo_localizado_vem_da_traducao() {
        let movie = raw(&serde_json::json!({
            "id": 1_249_199, "imdb_id": "tt11327404", "title": "The Throwback",
            "original_title": "The Throwback", "original_language": "en",
            "overview": "EN", "release_date": "2024-03-15", "runtime": 96,
            "status": "Released", "poster_path": "/p.jpg", "backdrop_path": null,
            "release_dates": { "results": [
                { "iso_3166_1": "BR", "release_dates": [
                    { "release_date": "2026-04-09T00:00:00.000Z", "type": 4 }] },
                { "iso_3166_1": "US", "release_dates": [
                    { "release_date": "2024-03-15T00:00:00.000Z", "type": 4 },
                    { "release_date": "2024-03-01T00:00:00.000Z", "type": 3 }] }
            ]},
            "alternative_titles": { "titles": [{ "title": "Plötzlich Teenie" }] },
            "translations": { "translations": [
                { "iso_3166_1": "PT", "iso_639_1": "pt", "data": { "title": "Regresso", "overview": "" } },
                { "iso_3166_1": "BR", "iso_639_1": "pt", "data": { "title": "19 Outra Vez", "overview": "PT" } }
            ]}
        }))
        .into_metadata("pt-BR");
        assert_eq!(movie.localized_title.as_deref(), Some("19 Outra Vez"));
        assert_eq!(movie.overview.as_deref(), Some("PT"));
        assert_eq!(movie.year, Some(2024));
        assert_eq!(movie.in_cinemas.as_deref(), Some("2024-03-01"));
        assert_eq!(movie.digital_release.as_deref(), Some("2024-03-15"));
        assert_eq!(movie.physical_release, None);
        assert_eq!(movie.alternate_titles, ["Plötzlich Teenie"]);
        assert_eq!(
            movie.poster.as_deref(),
            Some("https://image.tmdb.org/t/p/original/p.jpg")
        );
    }

    #[test]
    fn sem_datas_nem_traducao() {
        let movie = raw(&serde_json::json!({
            "id": 1, "imdb_id": "", "title": "Untitled", "original_title": "Untitled",
            "original_language": "en", "release_date": "", "status": "Rumored",
            "poster_path": null, "backdrop_path": null
        }))
        .into_metadata("pt-BR");
        assert_eq!(movie.imdb_id, None);
        assert_eq!(movie.year, None);
        assert_eq!(movie.in_cinemas, None);
        assert_eq!(movie.localized_title, None);
        assert_eq!(movie.runtime, 0);
    }

    #[tokio::test]
    async fn chave_recusada_e_filme_inexistente() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .and(query_param("api_key", "errada"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/3/movie/9"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let base = format!("{}/3/", server.uri());
        let wrong = Tmdb::with_base(&base, "errada", "pt-BR", Duration::from_secs(5)).unwrap();
        assert!(matches!(
            wrong.validate().await,
            Err(MetadataError::InvalidKey)
        ));
        let tmdb = Tmdb::with_base(&base, "certa", "pt-BR", Duration::from_secs(5)).unwrap();
        assert!(matches!(
            tmdb.movie(9).await,
            Err(MetadataError::NotFound(9))
        ));
    }
}

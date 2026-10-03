//! Séries pela API v3 do TMDB: o detalhe, os episódios por temporada, a busca
//! e a ponte do id do TVDB, que é o único que o Sonarr conhece.

use serde::Deserialize;

use crate::{GRID_IMAGES, IMAGES, MetadataError, Tmdb, non_empty};

/// Uma série, com o que o catálogo guarda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesMetadata {
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
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
    /// `continuing`, `ended` ou `upcoming`.
    pub status: String,
    pub network: Option<String>,
    /// Minutos por episódio; zero é desconhecido.
    pub runtime: u32,
    pub alternate_titles: Vec<String>,
    pub poster: Option<String>,
    pub fanart: Option<String>,
    /// Números das temporadas, especiais (0) incluídos.
    pub seasons: Vec<u16>,
}

/// Um episódio; título e sinopse em inglês, como o da série.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeMetadata {
    pub season: u16,
    pub number: u16,
    pub tmdb_id: u32,
    pub title: Option<String>,
    /// `AAAA-MM-DD`.
    pub air_date: Option<String>,
    pub overview: Option<String>,
    /// Minutos; zero é desconhecido.
    pub runtime: u32,
}

/// Um resultado de busca: o bastante para escolher a série.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SeriesSummary {
    pub tmdb_id: u32,
    /// Na língua pedida, quando há tradução.
    pub title: String,
    pub original_title: String,
    pub year: Option<u16>,
    pub overview: Option<String>,
    /// Pôster no tamanho de grade.
    pub poster: Option<String>,
}

#[derive(Deserialize)]
struct RawSeries {
    id: u32,
    name: String,
    original_name: String,
    original_language: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    networks: Vec<RawNamed>,
    #[serde(default)]
    episode_run_time: Vec<u32>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    seasons: Vec<RawSeason>,
    #[serde(default)]
    external_ids: Option<RawExternalIds>,
    #[serde(default)]
    alternative_titles: Option<RawAlternativeTitles>,
    #[serde(default)]
    translations: Option<RawTranslations>,
}

#[derive(Deserialize)]
struct RawNamed {
    name: String,
}

#[derive(Deserialize)]
struct RawSeason {
    season_number: u16,
}

#[derive(Deserialize)]
struct RawExternalIds {
    #[serde(default)]
    tvdb_id: Option<u32>,
    #[serde(default)]
    imdb_id: Option<String>,
}

#[derive(Deserialize)]
struct RawAlternativeTitles {
    results: Vec<RawTitle>,
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

/// Na série a tradução traz `name`, não `title`.
#[derive(Deserialize)]
struct RawTranslationData {
    #[serde(default)]
    name: String,
    #[serde(default)]
    overview: String,
}

/// A tradução da língua (`pt-BR`): a da região exata, ou a primeira da língua.
fn translation<'a>(
    translations: Option<&'a RawTranslations>,
    language: &str,
) -> Option<&'a RawTranslation> {
    let (lang, region) = language.split_once('-').unwrap_or((language, ""));
    translations.and_then(|t| {
        t.translations
            .iter()
            .find(|t| t.iso_639_1 == lang && t.iso_3166_1 == region)
            .or_else(|| t.translations.iter().find(|t| t.iso_639_1 == lang))
    })
}

/// `continuing`, `ended` ou `upcoming`; o que o TMDB não promete vira
/// `continuing`.
fn normalize_status(status: &str) -> &'static str {
    match status {
        "Ended" | "Canceled" => "ended",
        "Planned" | "Pilot" => "upcoming",
        _ => "continuing",
    }
}

fn year_of(date: Option<&str>) -> Option<u16> {
    date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

impl RawSeries {
    fn into_metadata(self, language: &str) -> SeriesMetadata {
        let localized = translation(self.translations.as_ref(), language);
        // O detalhe sem `language` já vem em inglês, mas cai no original
        // quando falta tradução; a tradução inglesa, quando existe, manda.
        let english = translation(self.translations.as_ref(), "en-US")
            .map(|t| t.data.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.name.clone());
        let mut alternate_titles: Vec<String> = Vec::new();
        let from_alternatives = self
            .alternative_titles
            .iter()
            .flat_map(|a| a.results.iter().map(|t| t.title.as_str()));
        let from_translations = self
            .translations
            .iter()
            .flat_map(|t| t.translations.iter().map(|t| t.data.name.as_str()));
        for candidate in from_alternatives.chain(from_translations) {
            if !candidate.is_empty()
                && candidate != english
                && !alternate_titles.iter().any(|t| t == candidate)
            {
                alternate_titles.push(candidate.to_owned());
            }
        }
        let first_air_date = non_empty(self.first_air_date);
        let external = self.external_ids.as_ref();
        SeriesMetadata {
            tmdb_id: self.id,
            tvdb_id: external.and_then(|e| e.tvdb_id),
            imdb_id: non_empty(external.and_then(|e| e.imdb_id.clone())),
            localized_title: localized
                .map(|t| t.data.name.clone())
                .filter(|t| !t.is_empty() && *t != english),
            overview: localized
                .map(|t| t.data.overview.clone())
                .filter(|o| !o.is_empty())
                .or_else(|| non_empty(self.overview)),
            year: year_of(first_air_date.as_deref()),
            status: normalize_status(&self.status).to_owned(),
            network: self.networks.into_iter().next().map(|n| n.name),
            runtime: self.episode_run_time.first().copied().unwrap_or(0),
            alternate_titles,
            poster: self.poster_path.map(|p| format!("{IMAGES}{p}")),
            fanart: self.backdrop_path.map(|p| format!("{IMAGES}{p}")),
            seasons: self.seasons.into_iter().map(|s| s.season_number).collect(),
            title: english,
            original_title: self.original_name,
            original_language: self.original_language,
        }
    }
}

#[derive(Deserialize)]
struct RawSeasonDetail {
    #[serde(default)]
    episodes: Vec<RawEpisode>,
}

#[derive(Deserialize)]
struct RawEpisode {
    id: u32,
    season_number: u16,
    episode_number: u16,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    runtime: Option<u32>,
}

impl RawEpisode {
    fn into_metadata(self) -> EpisodeMetadata {
        EpisodeMetadata {
            season: self.season_number,
            number: self.episode_number,
            tmdb_id: self.id,
            title: non_empty(self.name),
            air_date: non_empty(self.air_date),
            overview: non_empty(self.overview),
            runtime: self.runtime.unwrap_or(0),
        }
    }
}

#[derive(Deserialize)]
struct RawSeriesSummary {
    id: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    original_name: String,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}

impl RawSeriesSummary {
    fn into_summary(self) -> SeriesSummary {
        SeriesSummary {
            tmdb_id: self.id,
            year: year_of(self.first_air_date.as_deref()),
            title: self.name,
            original_title: self.original_name,
            overview: non_empty(self.overview),
            poster: self.poster_path.map(|p| format!("{GRID_IMAGES}{p}")),
        }
    }
}

impl Tmdb {
    /// A série pelo id do TMDB.
    ///
    /// # Errors
    ///
    /// Série inexistente, chave recusada ou TMDB inalcançável.
    pub async fn series(&self, tmdb_id: u32) -> Result<SeriesMetadata, MetadataError> {
        let raw: RawSeries = self
            .get(
                &format!("tv/{tmdb_id}"),
                &[(
                    "append_to_response",
                    "external_ids,alternative_titles,translations",
                )],
            )
            .await?
            .ok_or(MetadataError::SeriesNotFound(tmdb_id))?;
        Ok(raw.into_metadata(&self.language))
    }

    /// Os episódios das temporadas pedidas, em ordem de `(temporada, número)`.
    /// Temporada que o TMDB não conhece é pulada.
    ///
    /// # Errors
    ///
    /// Chave recusada ou TMDB inalcançável.
    pub async fn series_episodes(
        &self,
        tmdb_id: u32,
        seasons: &[u16],
    ) -> Result<Vec<EpisodeMetadata>, MetadataError> {
        let mut episodes = Vec::new();
        for season in seasons {
            let detail: Option<RawSeasonDetail> = self
                .get(&format!("tv/{tmdb_id}/season/{season}"), &[])
                .await?;
            episodes.extend(
                detail
                    .into_iter()
                    .flat_map(|d| d.episodes)
                    .map(RawEpisode::into_metadata),
            );
        }
        episodes.sort_by_key(|e| (e.season, e.number));
        Ok(episodes)
    }

    /// Busca séries por título, na língua do cliente.
    ///
    /// # Errors
    ///
    /// Chave recusada ou TMDB inalcançável.
    pub async fn search_series(&self, query: &str) -> Result<Vec<SeriesSummary>, MetadataError> {
        #[derive(Deserialize)]
        struct Page {
            results: Vec<RawSeriesSummary>,
        }
        let page: Option<Page> = self
            .get(
                "search/tv",
                &[
                    ("query", query),
                    ("language", self.language.as_str()),
                    ("include_adult", "false"),
                ],
            )
            .await?;
        Ok(page
            .map(|p| {
                p.results
                    .into_iter()
                    .map(RawSeriesSummary::into_summary)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A série pelo id do TVDB.
    ///
    /// # Errors
    ///
    /// Chave recusada ou TMDB inalcançável.
    pub async fn find_tvdb(&self, tvdb_id: u32) -> Result<Option<u32>, MetadataError> {
        #[derive(Deserialize)]
        struct Found {
            tv_results: Vec<Id>,
        }
        #[derive(Deserialize)]
        struct Id {
            id: u32,
        }
        let found: Option<Found> = self
            .get(
                &format!("find/{tvdb_id}"),
                &[("external_source", "tvdb_id")],
            )
            .await?;
        Ok(found.and_then(|f| f.tv_results.first().map(|s| s.id)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    async fn serve(route: &str, status: u16, body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        mount(&server, route, status, body).await;
        server
    }

    async fn mount(server: &MockServer, route: &str, status: u16, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    fn client(server: &MockServer) -> Tmdb {
        let base = format!("{}/3/", server.uri());
        Tmdb::with_base(&base, "chave", "pt-BR", Duration::from_secs(5)).unwrap()
    }

    fn series_json() -> serde_json::Value {
        serde_json::json!({
            "id": 1396, "name": "Breaking Bad (Origem)", "original_name": "Breaking Bad",
            "original_language": "en", "overview": "EN", "first_air_date": "2008-01-20",
            "status": "Ended", "episode_run_time": [47, 45],
            "networks": [{ "id": 174, "name": "AMC" }, { "id": 1, "name": "Outra" }],
            "poster_path": "/p.jpg", "backdrop_path": "/b.jpg",
            "seasons": [{ "season_number": 0 }, { "season_number": 1 }],
            "external_ids": { "imdb_id": "tt0903747", "tvdb_id": 81189 },
            "alternative_titles": { "results": [
                { "iso_3166_1": "DE", "title": "Braking Bad", "type": "" },
                { "iso_3166_1": "FR", "title": "Breaking Bad", "type": "" }
            ]},
            "translations": { "translations": [
                { "iso_3166_1": "US", "iso_639_1": "en", "data": { "name": "Breaking Bad", "overview": "EN" } },
                { "iso_3166_1": "PT", "iso_639_1": "pt", "data": { "name": "Guerra Química", "overview": "" } },
                { "iso_3166_1": "BR", "iso_639_1": "pt", "data": { "name": "Breaking Bad: A Química do Mal", "overview": "PT" } }
            ]}
        })
    }

    #[tokio::test]
    async fn titulo_em_ingles_vem_das_traducoes_e_o_resto_do_detalhe() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1396"))
            .and(query_param(
                "append_to_response",
                "external_ids,alternative_titles,translations",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_json()))
            .mount(&server)
            .await;
        let series = client(&server).series(1396).await.unwrap();
        assert_eq!(series.title, "Breaking Bad");
        assert_eq!(series.original_title, "Breaking Bad");
        assert_eq!(
            series.localized_title.as_deref(),
            Some("Breaking Bad: A Química do Mal")
        );
        assert_eq!(series.overview.as_deref(), Some("PT"));
        assert_eq!(series.year, Some(2008));
        assert_eq!(series.status, "ended");
        assert_eq!(series.network.as_deref(), Some("AMC"));
        assert_eq!(series.runtime, 47);
        assert_eq!(series.seasons, [0, 1]);
        // Sem repetir e sem o próprio título.
        assert_eq!(
            series.alternate_titles,
            [
                "Braking Bad",
                "Guerra Química",
                "Breaking Bad: A Química do Mal"
            ]
        );
        assert_eq!(
            series.poster.as_deref(),
            Some("https://image.tmdb.org/t/p/original/p.jpg")
        );
        assert_eq!(
            series.fanart.as_deref(),
            Some("https://image.tmdb.org/t/p/original/b.jpg")
        );
    }

    #[tokio::test]
    async fn tvdb_e_imdb_vem_de_external_ids() {
        let server = serve("/3/tv/1396", 200, series_json()).await;
        let series = client(&server).series(1396).await.unwrap();
        assert_eq!(series.tvdb_id, Some(81189));
        assert_eq!(series.imdb_id.as_deref(), Some("tt0903747"));

        let server = serve(
            "/3/tv/2",
            200,
            serde_json::json!({
                "id": 2, "name": "Sem Ids", "original_name": "Sem Ids",
                "original_language": "en", "first_air_date": "",
                "poster_path": null, "backdrop_path": null
            }),
        )
        .await;
        let series = client(&server).series(2).await.unwrap();
        assert_eq!(series.tvdb_id, None);
        assert_eq!(series.imdb_id, None);
        assert_eq!(series.year, None);
        assert_eq!(series.runtime, 0);
        assert_eq!(series.status, "continuing");
        assert!(series.seasons.is_empty());
    }

    #[test]
    fn status_normalizado() {
        for (raw, expected) in [
            ("Returning Series", "continuing"),
            ("In Production", "continuing"),
            ("Qualquer Coisa", "continuing"),
            ("", "continuing"),
            ("Ended", "ended"),
            ("Canceled", "ended"),
            ("Planned", "upcoming"),
            ("Pilot", "upcoming"),
        ] {
            assert_eq!(normalize_status(raw), expected, "{raw}");
        }
    }

    #[tokio::test]
    async fn serie_inexistente() {
        let server = serve("/3/tv/9", 404, serde_json::json!({})).await;
        assert!(matches!(
            client(&server).series(9).await,
            Err(MetadataError::SeriesNotFound(9))
        ));
    }

    #[tokio::test]
    async fn episodios_de_duas_temporadas_pulando_a_404() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/3/tv/1396/season/2",
            200,
            serde_json::json!({ "season_number": 2, "episodes": [
                { "id": 22, "season_number": 2, "episode_number": 2, "name": "Grilled",
                  "air_date": "2009-03-22", "overview": "B", "runtime": 47 },
                { "id": 21, "season_number": 2, "episode_number": 1, "name": "Seven Thirty-Seven",
                  "air_date": "", "overview": "", "runtime": null }
            ]}),
        )
        .await;
        mount(
            &server,
            "/3/tv/1396/season/1",
            200,
            serde_json::json!({ "season_number": 1, "episodes": [
                { "id": 11, "season_number": 1, "episode_number": 1, "name": "Pilot",
                  "air_date": "2008-01-20", "overview": "A", "runtime": 58 }
            ]}),
        )
        .await;
        mount(&server, "/3/tv/1396/season/7", 404, serde_json::json!({})).await;
        let episodes = client(&server)
            .series_episodes(1396, &[2, 7, 1])
            .await
            .unwrap();
        let keys: Vec<_> = episodes.iter().map(|e| (e.season, e.number)).collect();
        assert_eq!(keys, [(1, 1), (2, 1), (2, 2)]);
        assert_eq!(episodes[0].title.as_deref(), Some("Pilot"));
        assert_eq!(episodes[0].air_date.as_deref(), Some("2008-01-20"));
        assert_eq!(episodes[0].runtime, 58);
        assert_eq!(episodes[1].air_date, None);
        assert_eq!(episodes[1].overview, None);
        assert_eq!(episodes[1].runtime, 0);
        assert_eq!(episodes[2].tmdb_id, 22);
    }

    #[tokio::test]
    async fn temporada_com_outro_erro_e_erro() {
        let server = serve("/3/tv/1/season/1", 500, serde_json::json!({})).await;
        assert!(matches!(
            client(&server).series_episodes(1, &[1]).await,
            Err(MetadataError::Status(_))
        ));
    }

    #[tokio::test]
    async fn find_tvdb_devolve_o_primeiro() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/81189"))
            .and(query_param("external_source", "tvdb_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "movie_results": [], "person_results": [],
                "tv_results": [{ "id": 1396 }, { "id": 5 }]
            })))
            .mount(&server)
            .await;
        mount(
            &server,
            "/3/find/1",
            200,
            serde_json::json!({ "movie_results": [], "tv_results": [] }),
        )
        .await;
        let tmdb = client(&server);
        assert_eq!(tmdb.find_tvdb(81189).await.unwrap(), Some(1396));
        assert_eq!(tmdb.find_tvdb(1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn busca_na_lingua_do_cliente() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/tv"))
            .and(query_param("query", "breaking"))
            .and(query_param("language", "pt-BR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "page": 1, "total_results": 2, "results": [
                    { "id": 1396, "name": "Breaking Bad: A Química do Mal",
                      "original_name": "Breaking Bad", "first_air_date": "2008-01-20",
                      "overview": "PT", "poster_path": "/p.jpg" },
                    { "id": 7, "name": "Outra", "original_name": "Other",
                      "first_air_date": "", "overview": "", "poster_path": null }
                ]
            })))
            .mount(&server)
            .await;
        let found = client(&server).search_series("breaking").await.unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            SeriesSummary {
                tmdb_id: 1396,
                title: "Breaking Bad: A Química do Mal".into(),
                original_title: "Breaking Bad".into(),
                year: Some(2008),
                overview: Some("PT".into()),
                poster: Some("https://image.tmdb.org/t/p/w342/p.jpg".into()),
            }
        );
        assert_eq!(found[1].year, None);
        assert_eq!(found[1].overview, None);
        assert_eq!(found[1].poster, None);
    }
}

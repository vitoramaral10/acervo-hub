//! Listas e lançamentos para descobrir obras fora do catálogo.

use std::collections::HashMap;

use serde::Deserialize;

use crate::{GRID_IMAGES, IMAGES, MetadataError, Tmdb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscoverKind {
    Movie,
    Series,
}

#[derive(Debug, Clone)]
pub struct DiscoverItem {
    pub kind: DiscoverKind,
    pub tmdb_id: u32,
    pub title: String,
    pub original_title: String,
    pub date: Option<String>,
    pub year: Option<u16>,
    pub overview: String,
    pub poster: Option<String>,
    pub vote_average: f64,
    pub popularity: f64,
    pub genre_ids: Vec<u32>,
}

/// Detalhes de uma obra com créditos, trailer e recomendações.
#[derive(Debug, Clone)]
pub struct DiscoverDetails {
    pub item: DiscoverItem,
    pub tagline: String,
    pub backdrop: Option<String>,
    pub genres: Vec<String>,
    pub runtime: Option<u32>,
    pub number_of_seasons: Option<u32>,
    pub number_of_episodes: Option<u32>,
    pub status: Option<String>,
    pub cast: Vec<DiscoverCast>,
    pub directors: Vec<String>,
    pub creators: Vec<String>,
    pub trailer: Option<String>,
    pub recommendations: Vec<DiscoverItem>,
}

#[derive(Debug, Clone)]
pub struct DiscoverCast {
    pub name: String,
    pub character: String,
    pub profile: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawCredits {
    #[serde(default)]
    cast: Vec<RawCast>,
    #[serde(default)]
    crew: Vec<RawCrew>,
}
#[derive(Deserialize)]
struct RawCast {
    name: String,
    #[serde(default)]
    character: String,
    profile_path: Option<String>,
}
#[derive(Deserialize)]
struct RawCrew {
    name: String,
    job: String,
}
#[derive(Default, Deserialize)]
struct RawVideos {
    #[serde(default)]
    results: Vec<RawVideo>,
}
#[derive(Deserialize)]
struct RawVideo {
    key: String,
    site: String,
    #[serde(rename = "type")]
    kind: String,
    iso_639_1: Option<String>,
}
#[derive(Default, Deserialize)]
struct RawRecommendations {
    #[serde(default)]
    results: Vec<RawItem>,
}
#[derive(Deserialize)]
struct RawCreator {
    name: String,
}
#[derive(Deserialize)]
struct RawDetails {
    #[serde(flatten)]
    item: RawItem,
    #[serde(default)]
    tagline: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
    runtime: Option<u32>,
    number_of_seasons: Option<u32>,
    number_of_episodes: Option<u32>,
    status: Option<String>,
    #[serde(default)]
    created_by: Vec<RawCreator>,
    #[serde(default)]
    credits: RawCredits,
    #[serde(default)]
    videos: RawVideos,
    #[serde(default)]
    recommendations: RawRecommendations,
}
impl RawDetails {
    fn into_details(mut self, kind: DiscoverKind) -> DiscoverDetails {
        self.item.genre_ids = self.genres.iter().map(|genre| genre.id).collect();
        let trailer = self
            .videos
            .results
            .into_iter()
            .filter(|video| {
                video.site == "YouTube" && video.kind == "Trailer" && !video.key.is_empty()
            })
            .min_by_key(|video| match video.iso_639_1.as_deref() {
                Some("pt") => 0,
                Some("en") => 1,
                _ => 2,
            })
            .map(|video| video.key);
        let mut directors = Vec::new();
        for person in self
            .credits
            .crew
            .into_iter()
            .filter(|person| person.job == "Director")
        {
            if !directors.contains(&person.name) {
                directors.push(person.name);
            }
        }
        DiscoverDetails {
            item: self.item.into_item(kind),
            tagline: self.tagline.unwrap_or_default(),
            backdrop: self
                .backdrop_path
                .filter(|path| !path.is_empty())
                .map(|path| format!("{IMAGES}{path}")),
            genres: self.genres.into_iter().map(|genre| genre.name).collect(),
            runtime: if kind == DiscoverKind::Movie {
                self.runtime
            } else {
                None
            },
            number_of_seasons: self.number_of_seasons,
            number_of_episodes: self.number_of_episodes,
            status: self.status,
            directors,
            creators: self
                .created_by
                .into_iter()
                .map(|person| person.name)
                .collect(),
            cast: self
                .credits
                .cast
                .into_iter()
                .take(12)
                .map(|person| DiscoverCast {
                    name: person.name,
                    character: person.character,
                    profile: person
                        .profile_path
                        .filter(|path| !path.is_empty())
                        .map(|path| format!("{GRID_IMAGES}{path}")),
                })
                .collect(),
            trailer,
            recommendations: self
                .recommendations
                .results
                .into_iter()
                .take(20)
                .map(|item| item.into_item(kind))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct RawSearchItem {
    media_type: String,
    #[serde(flatten)]
    item: RawItem,
}
#[derive(Deserialize)]
struct RawSearchPage {
    results: Vec<RawSearchItem>,
    page: u32,
    total_pages: u32,
}
impl RawSearchPage {
    fn into_page(self) -> DiscoverPage {
        DiscoverPage {
            items: self
                .results
                .into_iter()
                .filter_map(|raw| {
                    let kind = match raw.media_type.as_str() {
                        "movie" => DiscoverKind::Movie,
                        "tv" => DiscoverKind::Series,
                        _ => return None,
                    };
                    Some(raw.item.into_item(kind))
                })
                .collect(),
            page: self.page,
            total_pages: self.total_pages,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Genre {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverList {
    Trending,
    Popular,
    Upcoming,
    OnTheAir,
}

#[derive(Debug, Clone)]
pub struct DiscoverPage {
    pub items: Vec<DiscoverItem>,
    pub page: u32,
    pub total_pages: u32,
}

#[derive(Deserialize)]
struct RawPage {
    results: Vec<RawItem>,
    page: u32,
    total_pages: u32,
}

#[derive(Deserialize)]
struct RawItem {
    id: u32,
    #[serde(default, alias = "name")]
    title: String,
    #[serde(default, alias = "original_name")]
    original_title: String,
    #[serde(default, alias = "first_air_date")]
    release_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    vote_average: f64,
    #[serde(default)]
    popularity: f64,
    #[serde(default)]
    genre_ids: Vec<u32>,
}

impl RawPage {
    fn into_page(self, kind: DiscoverKind) -> DiscoverPage {
        DiscoverPage {
            items: self
                .results
                .into_iter()
                .map(|raw| raw.into_item(kind))
                .collect(),
            page: self.page,
            total_pages: self.total_pages,
        }
    }
}

impl RawItem {
    fn into_item(self, kind: DiscoverKind) -> DiscoverItem {
        let date = self.release_date.filter(|d| !d.trim().is_empty());
        let year = date
            .as_deref()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok());
        DiscoverItem {
            kind,
            tmdb_id: self.id,
            title: self.title,
            original_title: self.original_title,
            date,
            year,
            overview: self.overview.unwrap_or_default(),
            poster: self
                .poster_path
                .filter(|p| !p.is_empty())
                .map(|p| format!("{GRID_IMAGES}{p}")),
            vote_average: self.vote_average,
            popularity: self.popularity,
            genre_ids: self.genre_ids,
        }
    }
}

impl Tmdb {
    /// Detalhes, créditos, vídeos e recomendações em uma única consulta.
    ///
    /// # Errors
    /// Id inválido, falha de comunicação ou resposta inválida do TMDB.
    pub async fn discover_details(
        &self,
        kind: DiscoverKind,
        tmdb_id: u32,
    ) -> Result<DiscoverDetails, MetadataError> {
        if tmdb_id == 0 {
            return Err(MetadataError::Argument("id deve ser maior que zero".into()));
        }
        let media = match kind {
            DiscoverKind::Movie => "movie",
            DiscoverKind::Series => "tv",
        };
        let raw: RawDetails = self
            .get(
                &format!("{media}/{tmdb_id}"),
                &[
                    ("language", &self.language),
                    ("append_to_response", "credits,videos,recommendations"),
                    ("include_video_language", "pt,en,null"),
                ],
            )
            .await?
            .ok_or(MetadataError::Status(reqwest::StatusCode::NOT_FOUND))?;
        Ok(raw.into_details(kind))
    }

    /// Busca unificada de filmes e séries, sem resultados de pessoas.
    ///
    /// # Errors
    /// Busca ou página inválida, falha de comunicação ou resposta inválida do TMDB.
    pub async fn discover_search(
        &self,
        query: &str,
        page: u32,
    ) -> Result<DiscoverPage, MetadataError> {
        let query = query.trim();
        if !(1..=100).contains(&query.chars().count()) || !(1..=500).contains(&page) {
            return Err(MetadataError::Argument(
                "busca deve ter de 1 a 100 caracteres e página de 1 a 500".into(),
            ));
        }
        let number = page.to_string();
        let raw: RawSearchPage = self
            .get(
                "search/multi",
                &[
                    ("language", &self.language),
                    ("query", query),
                    ("page", &number),
                ],
            )
            .await?
            .ok_or(MetadataError::Status(reqwest::StatusCode::NOT_FOUND))?;
        Ok(raw.into_page())
    }

    /// Lançamentos no Brasil, na ordem de popularidade, até 25 páginas.
    ///
    /// # Errors
    /// Falha de comunicação ou resposta inválida do TMDB.
    pub async fn releases_br(
        &self,
        gte: &str,
        lte: &str,
    ) -> Result<Vec<DiscoverItem>, MetadataError> {
        let mut items = Vec::new();
        for page in 1..=25 {
            let number = page.to_string();
            let raw: RawPage = self
                .get(
                    "discover/movie",
                    &[
                        ("language", &self.language),
                        ("region", "BR"),
                        ("sort_by", "popularity.desc"),
                        ("release_date.gte", gte),
                        ("release_date.lte", lte),
                        ("page", &number),
                    ],
                )
                .await?
                .ok_or(MetadataError::Status(reqwest::StatusCode::NOT_FOUND))?;
            let total = raw.total_pages;
            items.extend(raw.into_page(DiscoverKind::Movie).items);
            if page >= total {
                break;
            }
        }
        Ok(items)
    }

    /// Uma página de uma lista de filmes ou séries.
    ///
    /// # Errors
    /// Combinação inválida, página zero ou falha do TMDB.
    pub async fn discover_list(
        &self,
        list: DiscoverList,
        kind: DiscoverKind,
        page: u32,
    ) -> Result<DiscoverPage, MetadataError> {
        let path = match (list, kind) {
            (DiscoverList::Trending, DiscoverKind::Movie) => "trending/movie/week",
            (DiscoverList::Trending, DiscoverKind::Series) => "trending/tv/week",
            (DiscoverList::Popular, DiscoverKind::Movie) => "movie/popular",
            (DiscoverList::Popular, DiscoverKind::Series) => "tv/popular",
            (DiscoverList::Upcoming, DiscoverKind::Movie) => "movie/upcoming",
            (DiscoverList::OnTheAir, DiscoverKind::Series) => "tv/on_the_air",
            _ => {
                return Err(MetadataError::Argument(
                    "lista incompatível com o tipo de obra".into(),
                ));
            }
        };
        if page == 0 {
            return Err(MetadataError::Argument(
                "página deve ser maior que zero".into(),
            ));
        }
        let number = page.to_string();
        let mut query = vec![
            ("language", self.language.as_str()),
            ("page", number.as_str()),
        ];
        if kind == DiscoverKind::Movie
            && matches!(list, DiscoverList::Popular | DiscoverList::Upcoming)
        {
            query.push(("region", "BR"));
        }
        let raw: RawPage = self
            .get(path, &query)
            .await?
            .ok_or(MetadataError::Status(reqwest::StatusCode::NOT_FOUND))?;
        Ok(raw.into_page(kind))
    }

    /// União dos gêneros de filmes e séries, por nome e sem repetir id.
    ///
    /// # Errors
    /// Falha de comunicação ou resposta inválida do TMDB.
    pub async fn genres(&self) -> Result<Vec<Genre>, MetadataError> {
        #[derive(Deserialize)]
        struct Genres {
            genres: Vec<Genre>,
        }
        let mut genres = HashMap::new();
        for path in ["genre/movie/list", "genre/tv/list"] {
            let raw: Genres = self
                .get(path, &[("language", &self.language)])
                .await?
                .ok_or(MetadataError::Status(reqwest::StatusCode::NOT_FOUND))?;
            for genre in raw.genres {
                genres.entry(genre.id).or_insert(genre);
            }
        }
        let mut genres: Vec<_> = genres.into_values().collect();
        genres.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(genres)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{path, query_param},
    };

    #[test]
    fn detalhes_mapeiam_creditos_trailer_e_limites() {
        let raw: RawDetails = serde_json::from_value(json!({
            "id": 10, "title": "Filme", "original_title": "Movie", "release_date": "2024-03-01",
            "tagline": "Uma jornada", "backdrop_path": "/f.jpg", "runtime": 125,
            "genres": [{"id": 18, "name": "Drama"}],
            "credits": {"cast": (0..14).map(|i| json!({"name": format!("Ator {i}"), "character": "Papel", "profile_path": "/p.jpg"})).collect::<Vec<_>>(),
                "crew": [{"name": "Diretora", "job": "Director"}, {"name": "Diretora", "job": "Director"}, {"name": "Editor", "job": "Editor"}]},
            "videos": {"results": [
                {"key": "teaser", "site": "YouTube", "type": "Teaser", "iso_639_1": "pt"},
                {"key": "english", "site": "YouTube", "type": "Trailer", "iso_639_1": "en"},
                {"key": "outro", "site": "Vimeo", "type": "Trailer", "iso_639_1": "pt"},
                {"key": "portugues", "site": "YouTube", "type": "Trailer", "iso_639_1": "pt"}]},
            "recommendations": {"results": (1..=25).map(|id| json!({"id": id, "title": "Recomendação"})).collect::<Vec<_>>()}
        })).unwrap();
        let detail = raw.into_details(DiscoverKind::Movie);
        assert_eq!(detail.item.year, Some(2024));
        assert_eq!(detail.item.genre_ids, [18]);
        assert_eq!(detail.genres, ["Drama"]);
        assert_eq!(detail.tagline, "Uma jornada");
        assert_eq!(detail.runtime, Some(125));
        assert_eq!(
            detail.backdrop.as_deref(),
            Some("https://image.tmdb.org/t/p/original/f.jpg")
        );
        assert_eq!(detail.cast.len(), 12);
        assert_eq!(detail.cast[0].character, "Papel");
        assert_eq!(
            detail.cast[0].profile.as_deref(),
            Some("https://image.tmdb.org/t/p/w342/p.jpg")
        );
        assert_eq!(detail.directors, ["Diretora"]);
        assert_eq!(detail.trailer.as_deref(), Some("portugues"));
        assert_eq!(detail.recommendations.len(), 20);
        assert_eq!(detail.recommendations[0].kind, DiscoverKind::Movie);
    }

    #[test]
    fn detalhes_de_series_e_campos_ausentes() {
        let raw: RawDetails = serde_json::from_value(json!({
            "id": 20, "name": "Série", "first_air_date": "", "number_of_seasons": 3,
            "number_of_episodes": 24, "status": "Ended", "created_by": [{"name": "Criadora"}],
            "videos": {"results": [{"key": "neutro", "site": "YouTube", "type": "Trailer", "iso_639_1": null},
                {"key": "english", "site": "YouTube", "type": "Trailer", "iso_639_1": "en"}]},
            "recommendations": {"results": [{"id": 21, "name": "Outra série"}]}
        })).unwrap();
        let detail = raw.into_details(DiscoverKind::Series);
        assert_eq!(detail.item.title, "Série");
        assert_eq!(detail.item.year, None);
        assert_eq!(detail.number_of_seasons, Some(3));
        assert_eq!(detail.number_of_episodes, Some(24));
        assert_eq!(detail.status.as_deref(), Some("Ended"));
        assert_eq!(detail.creators, ["Criadora"]);
        assert_eq!(detail.runtime, None);
        assert!(detail.cast.is_empty());
        assert_eq!(detail.backdrop, None);
        assert_eq!(detail.trailer.as_deref(), Some("english"));
        assert_eq!(detail.recommendations[0].kind, DiscoverKind::Series);
        let empty: RawDetails =
            serde_json::from_value(json!({"id": 1, "title": "Sem detalhes"})).unwrap();
        assert_eq!(empty.into_details(DiscoverKind::Movie).trailer, None);
    }

    #[test]
    fn busca_mista_descarta_pessoas_e_preserva_paginacao() {
        let raw: RawSearchPage = serde_json::from_value(json!({
            "page": 2, "total_pages": 4, "results": [
                {"id": 1, "media_type": "person", "name": "Pessoa"},
                {"id": 2, "media_type": "movie", "title": "Filme", "release_date": "2022-01-01", "genre_ids": [18]},
                {"id": 3, "media_type": "tv", "name": "Série", "original_name": "Show", "first_air_date": "2023-01-01"}
            ]
        })).unwrap();
        let page = raw.into_page();
        assert_eq!((page.page, page.total_pages), (2, 4));
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].kind, DiscoverKind::Movie);
        assert_eq!(page.items[0].title, "Filme");
        assert_eq!(page.items[0].year, Some(2022));
        assert_eq!(page.items[0].genre_ids, [18]);
        assert_eq!(page.items[1].kind, DiscoverKind::Series);
        assert_eq!(page.items[1].title, "Série");
        assert_eq!(page.items[1].original_title, "Show");
        assert_eq!(page.items[1].year, Some(2023));
    }

    #[tokio::test]
    async fn detalhes_em_uma_consulta_e_busca_mista() {
        let server = MockServer::start().await;
        Mock::given(path("/movie/10"))
            .and(query_param("language", "pt-BR"))
            .and(query_param(
                "append_to_response",
                "credits,videos,recommendations",
            ))
            .and(query_param("include_video_language", "pt,en,null"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id": 10, "title": "Filme"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/search/multi"))
            .and(query_param("language", "pt-BR"))
            .and(query_param("query", "obra"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "page": 2, "total_pages": 3, "results": [
                    {"id": 1, "media_type": "person", "name": "Pessoa"},
                    {"id": 10, "media_type": "movie", "title": "Filme"},
                    {"id": 20, "media_type": "tv", "name": "Série", "first_air_date": "2023-01-01"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let tmdb = Tmdb::with_base(
            &format!("{}/", server.uri()),
            "key",
            "pt-BR",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            tmdb.discover_details(DiscoverKind::Movie, 10)
                .await
                .unwrap()
                .item
                .title,
            "Filme"
        );
        let page = tmdb.discover_search(" obra ", 2).await.unwrap();
        assert_eq!((page.page, page.total_pages), (2, 3));
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].kind, DiscoverKind::Movie);
        assert_eq!(page.items[1].kind, DiscoverKind::Series);
        assert_eq!(page.items[1].title, "Série");
        assert_eq!(page.items[1].year, Some(2023));
        assert!(tmdb.discover_search(" ", 1).await.is_err());
        assert!(tmdb.discover_search("obra", 0).await.is_err());
        assert!(
            tmdb.discover_details(DiscoverKind::Series, 0)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn lancamentos_percorrem_paginas_e_mapeiam_filmes() {
        let server = MockServer::start().await;
        for page in 1..=2 {
            Mock::given(path("/discover/movie"))
                .and(query_param("page", page.to_string()))
                .and(query_param("language", "pt-BR"))
                .and(query_param("region", "BR"))
                .and(query_param("sort_by", "popularity.desc"))
                .and(query_param("release_date.gte", "2026-01-01"))
                .and(query_param("release_date.lte", "2026-01-07"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "page": page, "total_pages": 2, "results": [{"id": page, "title": "Filme", "original_title": "Movie", "release_date": if page == 1 { "2026-01-02" } else { "" }, "poster_path": "/p.jpg", "genre_ids": [18]}]
                }))).expect(1).mount(&server).await;
        }
        let tmdb = Tmdb::with_base(
            &format!("{}/", server.uri()),
            "key",
            "pt-BR",
            Duration::from_secs(5),
        )
        .unwrap();
        let items = tmdb.releases_br("2026-01-01", "2026-01-07").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Filme");
        assert_eq!(items[0].original_title, "Movie");
        assert_eq!(items[0].year, Some(2026));
        assert_eq!(items[0].date.as_deref(), Some("2026-01-02"));
        assert_eq!(
            items[0].poster.as_deref(),
            Some("https://image.tmdb.org/t/p/w342/p.jpg")
        );
        assert_eq!(items[1].date, None);
        assert_eq!(items[1].year, None);
    }

    #[tokio::test]
    async fn series_e_uniao_dos_generos() {
        let server = MockServer::start().await;
        Mock::given(path("/trending/tv/week")).and(query_param("page", "2")).and(query_param("language", "pt-BR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"page": 2, "total_pages": 3, "results": [{"id": 5, "name": "Série", "original_name": "Show", "first_air_date": "2024-03-01"}]}))).mount(&server).await;
        for (endpoint, genres) in [
            ("movie", json!([{ "id": 18, "name": "Drama" }])),
            (
                "tv",
                json!([{ "id": 18, "name": "Drama" }, {"id": 1, "name": "Ação"}]),
            ),
        ] {
            Mock::given(path(format!("/genre/{endpoint}/list")))
                .and(query_param("language", "pt-BR"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"genres": genres})))
                .mount(&server)
                .await;
        }
        let tmdb = Tmdb::with_base(
            &format!("{}/", server.uri()),
            "key",
            "pt-BR",
            Duration::from_secs(5),
        )
        .unwrap();
        let page = tmdb
            .discover_list(DiscoverList::Trending, DiscoverKind::Series, 2)
            .await
            .unwrap();
        assert_eq!((page.page, page.total_pages), (2, 3));
        assert_eq!(page.items[0].kind, DiscoverKind::Series);
        assert_eq!(page.items[0].title, "Série");
        assert_eq!(page.items[0].original_title, "Show");
        assert_eq!(page.items[0].date.as_deref(), Some("2024-03-01"));
        assert_eq!(
            tmdb.genres()
                .await
                .unwrap()
                .iter()
                .map(|g| g.id)
                .collect::<Vec<_>>(),
            [1, 18]
        );
        assert!(
            tmdb.discover_list(DiscoverList::Upcoming, DiscoverKind::Series, 1)
                .await
                .is_err()
        );
        assert!(
            tmdb.discover_list(DiscoverList::OnTheAir, DiscoverKind::Movie, 1)
                .await
                .is_err()
        );
    }
}

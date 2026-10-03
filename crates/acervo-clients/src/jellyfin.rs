//! Cliente mínimo do Jellyfin: usuários, filmes e episódios com o que cada
//! um assistiu e o pedido de varredura da biblioteca.
//!
//! A chave vai no cabeçalho `Authorization` no esquema `MediaBrowser`, e
//! nunca na URL: URL aparece em erro de transporte e em log.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::Deserialize;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum JellyfinError {
    #[error("url inválida: {0}")]
    BadUrl(#[from] url::ParseError),

    #[error("chave do Jellyfin com caractere inválido para cabeçalho")]
    BadKey,

    #[error("falha ao falar com o Jellyfin: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("Jellyfin respondeu {status} em `{path}`")]
    Status {
        status: reqwest::StatusCode,
        path: String,
    },
}

/// Um usuário do servidor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinUser {
    pub id: String,
    pub name: String,
}

/// Um filme, visto por um usuário.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinMovie {
    pub name: String,
    pub year: Option<u16>,
    /// `ProviderIds.Tmdb`, quando o item tem um id numérico.
    pub tmdb_id: Option<u32>,
    pub played: bool,
    /// Como o servidor manda (ISO 8601, UTC); quem usa interpreta.
    pub last_played: Option<String>,
    pub favorite: bool,
}

/// Um episódio, visto por um usuário, com os dados da série a que pertence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinEpisode {
    /// `ProviderIds.Tmdb` da série, quando ela tem um id numérico.
    pub series_tmdb_id: Option<u32>,
    /// `ProviderIds.Tvdb` da série, quando ela tem um id numérico.
    pub series_tvdb_id: Option<u32>,
    /// `ParentIndexNumber`: a temporada.
    pub season: Option<u16>,
    /// `IndexNumber`: o episódio; no multi-episódio, o primeiro.
    pub number: Option<u16>,
    /// `IndexNumberEnd`: o último episódio de um arquivo multi-episódio.
    pub index_end: Option<u16>,
    pub played: bool,
    /// Como o servidor manda (ISO 8601, UTC); quem usa interpreta.
    pub last_played: Option<String>,
    pub favorite: bool,
    /// A série está nos favoritos do usuário.
    pub series_favorite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawUser {
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawItems {
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    /// Só o episódio traz: o id da série dona.
    #[serde(default)]
    series_id: Option<String>,
    #[serde(default)]
    parent_index_number: Option<u16>,
    #[serde(default)]
    index_number: Option<u16>,
    #[serde(default)]
    index_number_end: Option<u16>,
    #[serde(default)]
    production_year: Option<u16>,
    #[serde(default)]
    provider_ids: Option<HashMap<String, Option<String>>>,
    #[serde(default)]
    user_data: Option<RawUserData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawUserData {
    #[serde(default)]
    played: bool,
    #[serde(default)]
    last_played_date: Option<String>,
    #[serde(default)]
    is_favorite: bool,
}

impl From<RawItem> for JellyfinMovie {
    fn from(item: RawItem) -> Self {
        // A chave é "Tmdb"; comparar sem caixa não custa e protege de um
        // plugin que grave diferente.
        let tmdb_id = item.provider_ids.and_then(|ids| {
            ids.into_iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("tmdb"))
                .and_then(|(_, value)| value?.trim().parse().ok())
        });
        let data = item.user_data.unwrap_or(RawUserData {
            played: false,
            last_played_date: None,
            is_favorite: false,
        });
        Self {
            name: item.name,
            year: item.production_year,
            tmdb_id,
            played: data.played,
            last_played: data.last_played_date.filter(|d| !d.trim().is_empty()),
            favorite: data.is_favorite,
        }
    }
}

/// Id numérico de um provedor; a chave é comparada sem caixa, como no filme.
fn provider_id(ids: Option<&HashMap<String, Option<String>>>, key: &str) -> Option<u32> {
    ids?.iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .and_then(|(_, value)| value.as_deref()?.trim().parse().ok())
}

/// O que se sabe de uma série para preencher seus episódios.
#[derive(Clone, Copy)]
struct SeriesInfo {
    tmdb_id: Option<u32>,
    tvdb_id: Option<u32>,
    favorite: bool,
}

/// Cliente autenticado por chave de API.
#[derive(Clone)]
pub struct JellyfinClient {
    base: Url,
    http: reqwest::Client,
    auth: HeaderValue,
}

impl std::fmt::Debug for JellyfinClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // O cabeçalho carrega a chave.
        formatter
            .debug_struct("JellyfinClient")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl JellyfinClient {
    /// # Errors
    ///
    /// Url malformada, chave que não cabe num cabeçalho ou cliente HTTP que
    /// não sobe.
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> Result<Self, JellyfinError> {
        let normalized = if base_url.ends_with('/') {
            base_url.to_string()
        } else {
            format!("{base_url}/")
        };
        let base = Url::parse(&normalized)?;
        let mut auth = HeaderValue::from_str(&format!("MediaBrowser Token=\"{api_key}\""))
            .map_err(|_| JellyfinError::BadKey)?;
        // Fora de qualquer dump de requisição.
        auth.set_sensitive(true);
        let http = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self { base, http, auth })
    }

    /// Os usuários do servidor.
    ///
    /// # Errors
    ///
    /// Falha de transporte, chave recusada ou resposta ilegível.
    pub async fn users(&self) -> Result<Vec<JellyfinUser>, JellyfinError> {
        let users: Vec<RawUser> = self.get("Users", &[]).await?;
        Ok(users
            .into_iter()
            .map(|u| JellyfinUser {
                id: u.id,
                name: u.name,
            })
            .collect())
    }

    /// Todos os filmes, com os dados de quem é `user_id`: assistido, quando,
    /// favorito.
    ///
    /// `/Items?userId=`: a forma antiga, `/Users/{id}/Items`, saiu da API
    /// (não está no `OpenAPI` do 12.1).
    ///
    /// # Errors
    ///
    /// Falha de transporte, chave recusada ou resposta ilegível.
    pub async fn movies(&self, user_id: &str) -> Result<Vec<JellyfinMovie>, JellyfinError> {
        let page: RawItems = self
            .get(
                "Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Movie"),
                    ("recursive", "true"),
                    ("fields", "ProviderIds"),
                    ("enableUserData", "true"),
                ],
            )
            .await?;
        Ok(page.items.into_iter().map(JellyfinMovie::from).collect())
    }

    /// Todos os episódios, com os dados de quem é `user_id`, e com os ids e o
    /// favorito da série a que cada um pertence.
    ///
    /// O episódio não traz os `ProviderIds` da série: são duas buscas, a das
    /// séries e a dos episódios, juntadas pelo `SeriesId`. Episódio cuja
    /// série não veio na busca fica sem ids e sem favorito. Como em
    /// [`Self::movies`], não há paginação.
    ///
    /// # Errors
    ///
    /// Falha de transporte, chave recusada ou resposta ilegível.
    pub async fn episodes(&self, user_id: &str) -> Result<Vec<JellyfinEpisode>, JellyfinError> {
        let series: RawItems = self
            .get(
                "Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Series"),
                    ("recursive", "true"),
                    ("fields", "ProviderIds"),
                    ("enableUserData", "true"),
                ],
            )
            .await?;
        let series: HashMap<String, SeriesInfo> = series
            .items
            .into_iter()
            .map(|item| {
                let info = SeriesInfo {
                    tmdb_id: provider_id(item.provider_ids.as_ref(), "tmdb"),
                    tvdb_id: provider_id(item.provider_ids.as_ref(), "tvdb"),
                    favorite: item.user_data.is_some_and(|data| data.is_favorite),
                };
                (item.id, info)
            })
            .collect();

        let page: RawItems = self
            .get(
                "Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Episode"),
                    ("recursive", "true"),
                    ("fields", "ProviderIds"),
                    ("enableUserData", "true"),
                ],
            )
            .await?;
        Ok(page
            .items
            .into_iter()
            .map(|item| {
                let info = item.series_id.as_ref().and_then(|id| series.get(id));
                let data = item.user_data.unwrap_or(RawUserData {
                    played: false,
                    last_played_date: None,
                    is_favorite: false,
                });
                JellyfinEpisode {
                    series_tmdb_id: info.and_then(|s| s.tmdb_id),
                    series_tvdb_id: info.and_then(|s| s.tvdb_id),
                    season: item.parent_index_number,
                    number: item.index_number,
                    index_end: item.index_number_end,
                    played: data.played,
                    last_played: data.last_played_date.filter(|d| !d.trim().is_empty()),
                    favorite: data.is_favorite,
                    series_favorite: info.is_some_and(|s| s.favorite),
                }
            })
            .collect())
    }

    /// Pede a varredura de todas as bibliotecas; o servidor responde antes
    /// de terminar.
    ///
    /// # Errors
    ///
    /// Falha de transporte ou chave sem permissão de administrador.
    pub async fn refresh_library(&self) -> Result<(), JellyfinError> {
        let path = "Library/Refresh";
        let response = self
            .http
            .post(self.base.join(path)?)
            .header(AUTHORIZATION, self.auth.clone())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(JellyfinError::Status {
                status: response.status(),
                path: path.into(),
            });
        }
        Ok(())
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, JellyfinError> {
        let response = self
            .http
            .get(self.base.join(path)?)
            .header(AUTHORIZATION, self.auth.clone())
            .query(query)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(JellyfinError::Status {
                status: response.status(),
                path: path.to_string(),
            });
        }
        Ok(response.json().await?)
    }
}

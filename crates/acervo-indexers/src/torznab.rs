use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;
use url::Url;

use crate::{
    Capabilities, Category, Indexer, IndexerError, RateBudget, Release, SearchMode, SearchQuery,
    SearchSupport,
};

/// Cliente de um endpoint Torznab.
#[derive(Clone)]
pub struct TorznabClient {
    name: String,
    endpoint: Url,
    api_key: Option<String>,
    http: reqwest::Client,
    budget: Arc<RateBudget>,
}

impl std::fmt::Debug for TorznabClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TorznabClient")
            .field("name", &self.name)
            .field("endpoint", &"<configured>")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl TorznabClient {
    /// # Errors
    ///
    /// URL malformada ou falha ao montar o cliente HTTP.
    pub fn new(
        name: impl Into<String>,
        endpoint: &str,
        api_key: Option<String>,
        timeout: Duration,
        request_interval: Duration,
    ) -> Result<Self, IndexerError> {
        let name = name.into();
        let mut endpoint = Url::parse(endpoint).map_err(|source| IndexerError::BadUrl {
            indexer: name.clone(),
            source,
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(IndexerError::BadEndpointScheme { indexer: name });
        }
        // O endpoint é configuração estrutural. Query e fragmento fornecidos
        // junto da URL seriam descartados em toda requisição de qualquer
        // forma; removê-los aqui também impede que um segredo acidental fique
        // guardado na struct.
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let api_key = api_key.filter(|key| !key.trim().is_empty());
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(IndexerError::Build)?;

        Ok(Self {
            name,
            endpoint,
            api_key,
            http,
            budget: Arc::new(RateBudget::new(request_interval)),
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Lê os modos, limites e categorias anunciados pelo indexador.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx, erro Torznab ou XML inválido.
    pub async fn capabilities(&self) -> Result<Capabilities, IndexerError> {
        let body = self.request("caps", &[]).await?;
        parse_capabilities(&self.name, &body)
    }

    /// Executa uma busca neste indexador.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx, erro Torznab, XML inválido ou release
    /// sem os campos obrigatórios do contrato.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        let params = query_params(query);
        let body = self.request(query.mode.function(), &params).await?;
        parse_releases(&self.name, &self.endpoint, &body)
    }

    async fn request(
        &self,
        function: &str,
        params: &[(String, String)],
    ) -> Result<String, IndexerError> {
        self.budget.acquire().await;

        let mut url = self.endpoint.clone();
        url.set_query(None);
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("t", function);
            if let Some(api_key) = &self.api_key {
                query.append_pair("apikey", api_key);
            }
            for (name, value) in params {
                query.append_pair(name, value);
            }
        }

        let response =
            self.http
                .get(url)
                .send()
                .await
                .map_err(|source| IndexerError::Transport {
                    indexer: self.name.clone(),
                    kind: transport_kind(&source),
                })?;

        if !response.status().is_success() {
            return Err(IndexerError::Status {
                indexer: self.name.clone(),
                status: response.status(),
            });
        }

        response
            .text()
            .await
            .map_err(|source| IndexerError::Transport {
                indexer: self.name.clone(),
                kind: transport_kind(&source),
            })
    }
}

#[async_trait]
impl Indexer for TorznabClient {
    fn name(&self) -> &str {
        self.name()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.search(query).await
    }
}

fn query_params(query: &SearchQuery) -> Vec<(String, String)> {
    let mut params = vec![("extended".into(), "1".into())];

    if let Some(term) = &query.term {
        params.push(("q".into(), term.clone()));
    }
    if !query.categories.is_empty() {
        params.push((
            "cat".into(),
            query
                .categories
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ));
    }
    if let Some(limit) = query.limit {
        params.push(("limit".into(), limit.to_string()));
    }
    if query.offset > 0 {
        params.push(("offset".into(), query.offset.to_string()));
    }

    match &query.mode {
        SearchMode::General => {}
        SearchMode::Tv {
            season,
            episode,
            tvdb_id,
            imdb_id,
        } => {
            push_display(&mut params, "season", *season);
            push_optional(&mut params, "ep", episode.as_deref());
            push_display(&mut params, "tvdbid", *tvdb_id);
            push_optional(&mut params, "imdbid", imdb_id.as_deref());
        }
        SearchMode::Movie {
            year,
            tmdb_id,
            imdb_id,
        } => {
            push_display(&mut params, "year", *year);
            push_display(&mut params, "tmdbid", *tmdb_id);
            push_optional(&mut params, "imdbid", imdb_id.as_deref());
        }
    }

    params
}

fn push_display<T: std::fmt::Display>(
    params: &mut Vec<(String, String)>,
    name: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        params.push((name.into(), value.to_string()));
    }
}

fn push_optional(params: &mut Vec<(String, String)>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        params.push((name.into(), value.into()));
    }
}

fn transport_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "conexão recusada"
    } else if error.is_decode() {
        "resposta incompleta"
    } else {
        "requisição falhou"
    }
}

#[derive(Debug, Default)]
struct ReleaseBuilder {
    title: Option<String>,
    guid: Option<String>,
    link: Option<String>,
    comments: Option<String>,
    published: Option<String>,
    enclosure_url: Option<String>,
    enclosure_length: Option<u64>,
    attrs: Vec<(String, String)>,
}

fn parse_releases(indexer: &str, endpoint: &Url, xml: &str) -> Result<Vec<Release>, IndexerError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut current = None;
    let mut releases = Vec::new();
    let mut saw_rss = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if has_local_name(&element, b"rss") => {
                saw_rss = true;
            }
            Ok(Event::Start(element)) if has_local_name(&element, b"item") => {
                current = Some(ReleaseBuilder::default());
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == b"item" => {
                if let Some(builder) = current.take() {
                    releases.push(builder.finish(indexer, endpoint)?);
                }
            }
            Ok(Event::Start(element) | Event::Empty(element))
                if has_local_name(&element, b"error") =>
            {
                return Err(api_error(indexer, &reader, &element));
            }
            Ok(Event::Start(element)) if current.is_some() => {
                read_item_start(indexer, &mut reader, &mut current, &element)?;
            }
            Ok(Event::Empty(element)) if current.is_some() => {
                read_item_empty(&reader, &mut current, &element);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(source) => {
                return Err(IndexerError::Xml {
                    indexer: indexer.into(),
                    source,
                });
            }
        }
    }

    if saw_rss {
        Ok(releases)
    } else {
        Err(IndexerError::UnexpectedDocument {
            indexer: indexer.into(),
            expected: "RSS Torznab",
        })
    }
}

fn read_item_start(
    indexer: &str,
    reader: &mut Reader<&[u8]>,
    current: &mut Option<ReleaseBuilder>,
    element: &BytesStart<'_>,
) -> Result<(), IndexerError> {
    let local = element.local_name();
    let name = local.as_ref();
    if name == b"attr" {
        if let (Some(attr_name), Some(value)) = (
            xml_attribute(reader, element, b"name"),
            xml_attribute(reader, element, b"value"),
        ) {
            current.as_mut().unwrap().attrs.push((attr_name, value));
        }
        return Ok(());
    }

    let target = match name {
        b"title" | b"guid" | b"link" | b"comments" | b"pubDate" => Some(name.to_vec()),
        _ => None,
    };
    if let Some(target) = target {
        let text = reader
            .read_text(element.name())
            .map_err(|source| IndexerError::Xml {
                indexer: indexer.into(),
                source,
            })?
            .into_owned();
        let item = current.as_mut().unwrap();
        match target.as_slice() {
            b"title" => item.title = Some(text),
            b"guid" => item.guid = Some(text),
            b"link" => item.link = Some(text),
            b"comments" => item.comments = Some(text),
            b"pubDate" => item.published = Some(text),
            _ => {}
        }
    }
    Ok(())
}

fn read_item_empty(
    reader: &Reader<&[u8]>,
    current: &mut Option<ReleaseBuilder>,
    element: &BytesStart<'_>,
) {
    let item = current.as_mut().unwrap();
    match element.local_name().as_ref() {
        b"enclosure" => {
            item.enclosure_url = xml_attribute(reader, element, b"url");
            item.enclosure_length =
                xml_attribute(reader, element, b"length").and_then(|value| value.parse().ok());
        }
        b"attr" => {
            if let (Some(name), Some(value)) = (
                xml_attribute(reader, element, b"name"),
                xml_attribute(reader, element, b"value"),
            ) {
                item.attrs.push((name, value));
            }
        }
        _ => {}
    }
}

impl ReleaseBuilder {
    fn finish(self, indexer: &str, endpoint: &Url) -> Result<Release, IndexerError> {
        let title = required_string(self.title, indexer, "title")?;
        let guid = required_string(self.guid, indexer, "guid")?;
        let download = self.enclosure_url.or(self.link);
        let download_url = parse_item_url(download, indexer, endpoint, "download_url")?;
        let info_url = parse_optional_http_url(self.comments, indexer, endpoint, "info_url")?;

        let attr = |name: &str| {
            self.attrs
                .iter()
                .find_map(|(key, value)| (key == name).then_some(value.as_str()))
        };
        let numbers = |name: &str| {
            self.attrs
                .iter()
                .filter_map(|(key, value)| (key == name).then(|| value.parse().ok()).flatten())
                .collect::<Vec<_>>()
        };
        let strings = |name: &str| {
            self.attrs
                .iter()
                .filter_map(|(key, value)| (key == name).then_some(value.clone()))
                .collect::<Vec<_>>()
        };

        let size = attr("size")
            .and_then(|value| value.parse().ok())
            .or(self.enclosure_length)
            .ok_or_else(|| IndexerError::InvalidRelease {
                indexer: indexer.into(),
                field: "size",
            })?;
        let published = self
            .published
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &Rfc2822).ok());

        let mut categories = numbers("category");
        categories.sort_unstable();
        categories.dedup();
        let mut tags = strings("tag");
        tags.sort_unstable();
        tags.dedup();

        Ok(Release {
            indexer: indexer.into(),
            guid,
            title,
            download_url,
            info_url,
            size,
            published,
            seeders: parse_optional_number(attr("seeders")),
            leechers: parse_optional_number(attr("leechers")),
            grabs: parse_optional_number(attr("grabs")),
            categories,
            tags,
        })
    }
}

fn required_string(
    value: Option<String>,
    indexer: &str,
    field: &'static str,
) -> Result<String, IndexerError> {
    let value = required(value, indexer, field)?;
    if value.trim().is_empty() {
        Err(IndexerError::InvalidRelease {
            indexer: indexer.into(),
            field,
        })
    } else {
        Ok(value)
    }
}

fn required<T>(value: Option<T>, indexer: &str, field: &'static str) -> Result<T, IndexerError> {
    value.ok_or_else(|| IndexerError::InvalidRelease {
        indexer: indexer.into(),
        field,
    })
}

fn parse_item_url(
    value: Option<String>,
    indexer: &str,
    endpoint: &Url,
    field: &'static str,
) -> Result<Url, IndexerError> {
    let value = required_string(value, indexer, field)?;
    let url = endpoint
        .join(&value)
        .map_err(|_| IndexerError::InvalidRelease {
            indexer: indexer.into(),
            field,
        })?;
    if matches!(url.scheme(), "http" | "https" | "magnet") {
        Ok(url)
    } else {
        Err(IndexerError::InvalidRelease {
            indexer: indexer.into(),
            field,
        })
    }
}

fn parse_optional_http_url(
    value: Option<String>,
    indexer: &str,
    endpoint: &Url,
    field: &'static str,
) -> Result<Option<Url>, IndexerError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let url = endpoint
        .join(&value)
        .map_err(|_| IndexerError::InvalidRelease {
            indexer: indexer.into(),
            field,
        })?;
    if matches!(url.scheme(), "http" | "https") {
        Ok(Some(url))
    } else {
        Err(IndexerError::InvalidRelease {
            indexer: indexer.into(),
            field,
        })
    }
}

fn parse_optional_number(value: Option<&str>) -> Option<u32> {
    value.and_then(|value| value.parse().ok())
}

fn parse_capabilities(indexer: &str, xml: &str) -> Result<Capabilities, IndexerError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut capabilities = Capabilities::default();
    let mut parent_category = None;
    let mut saw_caps = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => match element.local_name().as_ref() {
                b"caps" => saw_caps = true,
                b"error" => return Err(api_error(indexer, &reader, &element)),
                b"category" => {
                    if let Some(category) = category(&reader, &element, None) {
                        parent_category = Some(category.id);
                        capabilities.categories.push(category);
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(element)) => match element.local_name().as_ref() {
                b"error" => return Err(api_error(indexer, &reader, &element)),
                b"limits" => {
                    capabilities.max_results = xml_number(&reader, &element, b"max");
                    capabilities.default_results = xml_number(&reader, &element, b"default");
                }
                b"search" => capabilities.general = search_support(&reader, &element),
                b"tv-search" => capabilities.tv = search_support(&reader, &element),
                b"movie-search" => capabilities.movie = search_support(&reader, &element),
                b"category" => {
                    if let Some(category) = category(&reader, &element, None) {
                        capabilities.categories.push(category);
                    }
                }
                b"subcat" => {
                    if let Some(category) = category(&reader, &element, parent_category) {
                        capabilities.categories.push(category);
                    }
                }
                _ => {}
            },
            Ok(Event::End(element)) if element.local_name().as_ref() == b"category" => {
                parent_category = None;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(source) => {
                return Err(IndexerError::Xml {
                    indexer: indexer.into(),
                    source,
                });
            }
        }
    }

    if saw_caps {
        Ok(capabilities)
    } else {
        Err(IndexerError::UnexpectedDocument {
            indexer: indexer.into(),
            expected: "capacidades Torznab",
        })
    }
}

fn search_support(reader: &Reader<&[u8]>, element: &BytesStart<'_>) -> SearchSupport {
    let available = xml_attribute(reader, element, b"available")
        .is_some_and(|value| matches!(value.as_str(), "yes" | "true" | "1"));
    let supported_params = xml_attribute(reader, element, b"supportedParams")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();

    SearchSupport {
        available,
        supported_params,
    }
}

fn category(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    parent: Option<u32>,
) -> Option<Category> {
    Some(Category {
        id: xml_number(reader, element, b"id")?,
        name: xml_attribute(reader, element, b"name")?,
        parent,
    })
}

fn api_error(indexer: &str, reader: &Reader<&[u8]>, element: &BytesStart<'_>) -> IndexerError {
    IndexerError::Api {
        indexer: indexer.into(),
        code: xml_attribute(reader, element, b"code").unwrap_or_else(|| "?".into()),
        description: xml_attribute(reader, element, b"description")
            .unwrap_or_else(|| "erro sem descrição".into()),
    }
}

fn xml_number<T: std::str::FromStr>(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
    name: &[u8],
) -> Option<T> {
    xml_attribute(reader, element, name)?.parse().ok()
}

fn xml_attribute(reader: &Reader<&[u8]>, element: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == name)
        .and_then(|attribute| attribute.decode_and_unescape_value(reader.decoder()).ok())
        .map(std::borrow::Cow::into_owned)
}

fn has_local_name(element: &BytesStart<'_>, name: &[u8]) -> bool {
    element.local_name().as_ref() == name
}

//! Executor conservador de um subconjunto Cardigann v11 GET/HTML.
//!
//! O formato não possui um campo `version`: `from_yaml_v11` seleciona o
//! contrato explicitamente. Campos não implementados são recusados no load.

mod definition;
mod parse;
mod template;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use scraper::Selector;
use url::Url;

use crate::{Capabilities, Indexer, IndexerError, RateBudget, Release, SearchMode, SearchQuery};
use definition::{CompiledField, Document, Setting, search_url, validate_setting};
use template::Template;

/// Definição validada e compilada. URLs, settings e templates nunca vão a Debug.
pub struct CardigannDefinition {
    id: String,
    name: String,
    description: String,
    language: String,
    links: Vec<Url>,
    delay: Duration,
    capabilities: Capabilities,
    mappings: BTreeMap<String, Vec<u32>>,
    settings: BTreeMap<String, Setting>,
    path: String,
    inputs: BTreeMap<String, Template>,
    allow_empty_inputs: bool,
    rows: Selector,
    fields: BTreeMap<String, CompiledField>,
}

impl std::fmt::Debug for CardigannDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CardigannDefinition")
            .field("contents", &"<redacted>")
            .finish()
    }
}

impl CardigannDefinition {
    /// Carrega uma definição do diretório v11, recusando recursos fora do recorte.
    ///
    /// # Errors
    /// Retorna erro sanitizado para YAML inválido, selectors inválidos, recursos
    /// não implementados e capacidades que não são consumidas pelos inputs.
    pub fn from_yaml_v11(yaml: &str) -> Result<Self, IndexerError> {
        if yaml.len() > 1024 * 1024 {
            return Err(invalid("yaml", "limite de 1 MiB excedido"));
        }
        // Não propagar mensagens do desserializador: incluem valores de settings.
        let document: Document = serde_yaml_ng::from_str(yaml).map_err(|_| {
            invalid(
                "yaml",
                "sintaxe, tipo ou campo não suportado pelo subconjunto v11",
            )
        })?;
        document.compile()
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    #[must_use]
    pub fn links(&self) -> &[Url] {
        &self.links
    }
}

/// Cliente GET/HTML; clones compartilham definição, conexão e rate budget.
#[derive(Clone)]
pub struct CardigannClient {
    definition: Arc<CardigannDefinition>,
    base: Url,
    settings: BTreeMap<String, String>,
    http: reqwest::Client,
    budget: Arc<RateBudget>,
}

impl std::fmt::Debug for CardigannClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CardigannClient")
            .field("configuration", &"<redacted>")
            .finish()
    }
}

impl CardigannClient {
    /// Seleciona um link declarado e aplica overrides aos settings conhecidos.
    ///
    /// # Errors
    /// Recusa índice inexistente, setting desconhecido/ausente ou tipo inválido.
    /// Redirects ficam desabilitados para impedir envio de settings a outra origem.
    pub fn new(
        definition: CardigannDefinition,
        link_index: usize,
        mut overrides: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Self, IndexerError> {
        let base = definition
            .links
            .get(link_index)
            .cloned()
            .ok_or_else(|| invalid("links", "índice do link inexistente"))?;
        if overrides
            .keys()
            .any(|key| !definition.settings.contains_key(key))
        {
            return Err(invalid("settings", "override de setting desconhecido"));
        }
        let mut settings = BTreeMap::new();
        for (name, setting) in &definition.settings {
            let value = overrides
                .remove(name)
                .or_else(|| setting.default.as_ref().map(definition::Scalar::value))
                .ok_or_else(|| invalid("settings", "setting obrigatório não configurado"))?;
            validate_setting(&setting.kind, &value)?;
            settings.insert(name.clone(), value);
        }
        if timeout.is_zero() {
            return Err(invalid("timeout", "timeout deve ser maior que zero"));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(IndexerError::Build)?;
        let budget = Arc::new(RateBudget::new(definition.delay));
        Ok(Self {
            definition: Arc::new(definition),
            base,
            settings,
            http,
            budget,
        })
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        self.definition.capabilities()
    }

    fn query_url(&self, query: &SearchQuery) -> Result<Url, IndexerError> {
        self.validate_query(query)?;
        let mut url = search_url(&self.base, &self.definition.path)?;
        {
            let mut pairs = url.query_pairs_mut();
            for (key, template) in &self.definition.inputs {
                let value = template.render(query, &self.settings);
                if self.definition.allow_empty_inputs || !value.is_empty() {
                    pairs.append_pair(key, &value);
                }
            }
        }
        Ok(url)
    }

    fn validate_query(&self, query: &SearchQuery) -> Result<(), IndexerError> {
        if query.offset != 0 {
            return Err(unsupported("paginação por offset ainda não é suportada"));
        }
        let support = match &query.mode {
            SearchMode::General => &self.definition.capabilities.general,
            SearchMode::Tv { .. } => &self.definition.capabilities.tv,
            SearchMode::Movie { .. } => &self.definition.capabilities.movie,
        };
        if !support.available {
            return Err(unsupported("modo não anunciado pela definição"));
        }
        let mut used = Vec::new();
        if query.term.is_some() {
            used.push("q");
        }
        match &query.mode {
            SearchMode::General => {}
            SearchMode::Tv {
                season,
                episode,
                tvdb_id,
                imdb_id,
            } => {
                if season.is_some() {
                    used.push("season");
                }
                if episode.is_some() {
                    used.push("ep");
                }
                if tvdb_id.is_some() {
                    used.push("tvdbid");
                }
                if imdb_id.is_some() {
                    used.push("imdbid");
                }
            }
            SearchMode::Movie {
                year,
                tmdb_id,
                imdb_id,
            } => {
                if year.is_some() {
                    used.push("year");
                }
                if tmdb_id.is_some() {
                    used.push("tmdbid");
                }
                if imdb_id.is_some() {
                    used.push("imdbid");
                }
            }
        }
        if used
            .iter()
            .any(|param| !support.supported_params.contains(*param))
        {
            return Err(unsupported("parâmetro não anunciado pela definição"));
        }
        if query.categories.iter().any(|requested| {
            !self
                .definition
                .capabilities
                .categories
                .iter()
                .any(|category| category_matches(*requested, category.id))
        }) {
            return Err(unsupported("categoria não anunciada pela definição"));
        }
        Ok(())
    }

    fn transport(&self, error: &reqwest::Error) -> IndexerError {
        IndexerError::Transport {
            indexer: self.definition.id.clone(),
            kind: if error.is_timeout() {
                "timeout"
            } else {
                "requisição ou corpo indisponível"
            },
        }
    }
}

#[async_trait]
impl Indexer for CardigannClient {
    fn name(&self) -> &str {
        self.definition.id()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        let url = self.query_url(query)?;
        self.budget.acquire().await;
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| self.transport(&error))?;
        if !response.status().is_success() {
            return Err(IndexerError::Status {
                indexer: self.definition.id.clone(),
                status: response.status(),
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default();
        let media_type = content_type.split(';').next().unwrap_or_default().trim();
        if !media_type.eq_ignore_ascii_case("text/html")
            && !media_type.eq_ignore_ascii_case("application/xhtml+xml")
        {
            return Err(IndexerError::UnexpectedDocument {
                indexer: self.definition.id.clone(),
                expected: "HTML UTF-8",
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.transport(&error))?
        {
            if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err(IndexerError::UnexpectedDocument {
                    indexer: self.definition.id.clone(),
                    expected: "HTML de até 8 MiB",
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        let html = std::str::from_utf8(&bytes).map_err(|_| IndexerError::UnexpectedDocument {
            indexer: self.definition.id.clone(),
            expected: "HTML UTF-8",
        })?;
        let mut releases = self.definition.parse(html, response.url())?;
        if !query.categories.is_empty() {
            releases.retain(|release| {
                query.categories.iter().any(|requested| {
                    release
                        .categories
                        .iter()
                        .any(|category| category_matches(*requested, *category))
                })
            });
        }
        if let Some(limit) = query.limit {
            releases.truncate(usize::from(limit));
        }
        Ok(releases)
    }
}

fn invalid(section: &'static str, reason: &'static str) -> IndexerError {
    IndexerError::Definition { section, reason }
}

fn unsupported(reason: &'static str) -> IndexerError {
    IndexerError::UnsupportedQuery { reason }
}

fn category_matches(requested: u32, actual: u32) -> bool {
    requested == actual || (requested.is_multiple_of(1000) && requested / 1000 == actual / 1000)
}

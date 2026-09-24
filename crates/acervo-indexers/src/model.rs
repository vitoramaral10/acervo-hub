use std::collections::BTreeSet;

use time::OffsetDateTime;
use url::Url;

/// Modo de busca previsto pelo contrato Torznab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMode {
    General,
    Tv {
        season: Option<u16>,
        episode: Option<String>,
        tvdb_id: Option<u64>,
        imdb_id: Option<String>,
    },
    Movie {
        year: Option<u16>,
        tmdb_id: Option<u64>,
        imdb_id: Option<String>,
    },
}

impl SearchMode {
    pub(crate) const fn function(&self) -> &'static str {
        match self {
            Self::General => "search",
            Self::Tv { .. } => "tvsearch",
            Self::Movie { .. } => "movie",
        }
    }
}

/// Uma consulta independente do indexador que vai executá-la.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub mode: SearchMode,
    pub term: Option<String>,
    pub categories: Vec<u32>,
    pub limit: Option<u16>,
    pub offset: u32,
}

impl SearchQuery {
    #[must_use]
    pub fn general(term: impl Into<String>) -> Self {
        Self {
            mode: SearchMode::General,
            term: nonempty(term),
            categories: Vec::new(),
            limit: None,
            offset: 0,
        }
    }

    #[must_use]
    pub fn tv(term: impl Into<String>) -> Self {
        Self {
            mode: SearchMode::Tv {
                season: None,
                episode: None,
                tvdb_id: None,
                imdb_id: None,
            },
            term: nonempty(term),
            categories: Vec::new(),
            limit: None,
            offset: 0,
        }
    }

    #[must_use]
    pub fn movie(term: impl Into<String>) -> Self {
        Self {
            mode: SearchMode::Movie {
                year: None,
                tmdb_id: None,
                imdb_id: None,
            },
            term: nonempty(term),
            categories: Vec::new(),
            limit: None,
            offset: 0,
        }
    }

    #[must_use]
    pub fn with_categories(mut self, categories: impl IntoIterator<Item = u32>) -> Self {
        self.categories = categories.into_iter().collect();
        self.categories.sort_unstable();
        self.categories.dedup();
        self
    }

    #[must_use]
    pub const fn with_limit(mut self, limit: u16) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub const fn with_offset(mut self, offset: u32) -> Self {
        self.offset = offset;
        self
    }

    #[must_use]
    pub fn with_episode(mut self, season: u16, episode: impl Into<String>) -> Self {
        if let SearchMode::Tv {
            season: target_season,
            episode: target_episode,
            ..
        } = &mut self.mode
        {
            *target_season = Some(season);
            *target_episode = nonempty(episode);
        }
        self
    }

    #[must_use]
    pub const fn with_tvdb_id(mut self, id: u64) -> Self {
        if let SearchMode::Tv { tvdb_id, .. } = &mut self.mode {
            *tvdb_id = Some(id);
        }
        self
    }

    #[must_use]
    pub fn with_imdb_id(mut self, id: impl Into<String>) -> Self {
        let value = nonempty(id);
        match &mut self.mode {
            SearchMode::Tv { imdb_id, .. } | SearchMode::Movie { imdb_id, .. } => {
                *imdb_id = value;
            }
            SearchMode::General => {}
        }
        self
    }

    #[must_use]
    pub const fn with_tmdb_id(mut self, id: u64) -> Self {
        if let SearchMode::Movie { tmdb_id, .. } = &mut self.mode {
            *tmdb_id = Some(id);
        }
        self
    }

    #[must_use]
    pub const fn with_year(mut self, year: u16) -> Self {
        if let SearchMode::Movie {
            year: target_year, ..
        } = &mut self.mode
        {
            *target_year = Some(year);
        }
        self
    }
}

fn nonempty(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    (!value.trim().is_empty()).then_some(value)
}

/// Candidato devolvido por um indexador.
#[derive(Clone, PartialEq, Eq)]
pub struct Release {
    pub indexer: String,
    pub guid: String,
    pub title: String,
    pub download_url: Url,
    pub info_url: Option<Url>,
    pub size: u64,
    pub published: Option<OffsetDateTime>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub grabs: Option<u32>,
    pub categories: Vec<u32>,
    pub tags: Vec<String>,
}

impl std::fmt::Debug for Release {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Release")
            .field("indexer", &self.indexer)
            .field("guid", &"<redacted>")
            .field("title", &self.title)
            // Downloads de tracker costumam carregar passkey na query ou no
            // caminho. O valor existe para o grab, mas nunca vai ao log.
            .field("download_url", &"<redacted>")
            .field("info_url", &self.info_url.as_ref().map(|_| "<redacted>"))
            .field("size", &self.size)
            .field("published", &self.published)
            .field("seeders", &self.seeders)
            .field("leechers", &self.leechers)
            .field("grabs", &self.grabs)
            .field("categories", &self.categories)
            .field("tags", &self.tags)
            .finish()
    }
}

/// Suporte declarado para um modo de busca.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchSupport {
    pub available: bool,
    pub supported_params: BTreeSet<String>,
}

/// Categoria anunciada em `t=caps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: u32,
    pub name: String,
    pub parent: Option<u32>,
}

/// Parte operacional das capacidades Torznab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub max_results: Option<u32>,
    pub default_results: Option<u32>,
    pub general: SearchSupport,
    pub tv: SearchSupport,
    pub movie: SearchSupport,
    pub categories: Vec<Category>,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("definição Cardigann inválida ou não suportada em `{section}`: {reason}")]
    Definition {
        section: &'static str,
        reason: &'static str,
    },

    #[error("chave `{key}` não suportada pelo subconjunto Cardigann implementado")]
    UnsupportedDefinitionKey { key: String },

    #[error("consulta não suportada: {reason}")]
    UnsupportedQuery { reason: &'static str },

    #[error("login em `{indexer}` falhou: {reason}")]
    Login {
        indexer: String,
        reason: &'static str,
    },

    #[error("url inválida para o indexador `{indexer}`: {source}")]
    BadUrl {
        indexer: String,
        #[source]
        source: url::ParseError,
    },

    #[error("o endpoint de `{indexer}` precisa usar HTTP ou HTTPS")]
    BadEndpointScheme { indexer: String },

    #[error("não foi possível construir o cliente http: {0}")]
    Build(#[source] reqwest::Error),

    #[error("falha de transporte com o indexador `{indexer}`: {kind}")]
    Transport { indexer: String, kind: &'static str },

    #[error("indexador `{indexer}` respondeu HTTP {status}")]
    Status {
        indexer: String,
        status: reqwest::StatusCode,
    },

    #[error("indexador `{indexer}` recusou a consulta ({code}): {description}")]
    Api {
        indexer: String,
        code: String,
        description: String,
    },

    #[error("xml inválido devolvido por `{indexer}`: {source}")]
    Xml {
        indexer: String,
        #[source]
        source: quick_xml::Error,
    },

    #[error("`{indexer}` devolveu um documento que não é {expected}")]
    UnexpectedDocument {
        indexer: String,
        expected: &'static str,
    },

    #[error("release inválido devolvido por `{indexer}`: campo `{field}`")]
    InvalidRelease {
        indexer: String,
        field: &'static str,
    },
}

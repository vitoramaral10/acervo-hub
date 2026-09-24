//! Executor de definições Cardigann v11.
//!
//! O formato não tem campo `version`: `from_yaml_v11` escolhe o contrato
//! explicitamente. Cobre indexador público e privado (login por formulário ou
//! por cookie), GET e HTML UTF-8. O que a definição pedir além disso é recusado
//! na carga, com o motivo, em vez de executado pela metade.

mod definition;
mod filters;
mod parse;
mod selector;
mod template;

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{COOKIE, HeaderValue, LOCATION};
use scraper::Html;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use url::Url;

use crate::{Capabilities, Indexer, IndexerError, RateBudget, Release, SearchMode, SearchQuery};
use definition::{
    Document, Field, Login, LoginMethod, Rows, SearchPath, Setting, SettingKind, same_origin,
    validate_setting,
};
use filters::Filter;
use selector::Css;
use template::{Template, Value, Vars};

const MAX_PAGE: usize = 8 * 1024 * 1024;
const MAX_TORRENT: usize = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Definição validada e compilada. URLs, settings e templates nunca vão a Debug.
pub struct CardigannDefinition {
    id: String,
    name: String,
    description: String,
    language: String,
    private: bool,
    links: Vec<Url>,
    delay: Duration,
    capabilities: Capabilities,
    mappings: BTreeMap<String, Vec<u32>>,
    settings: Vec<Setting>,
    login: Option<Login>,
    paths: Vec<SearchPath>,
    inputs: Vec<(String, Template)>,
    raw_inputs: Option<Template>,
    allow_empty_inputs: bool,
    keywords_filters: Vec<Filter>,
    headers: Vec<(String, String)>,
    rows: Rows,
    and_match: bool,
    fields: Vec<(String, Field)>,
}

impl std::fmt::Debug for CardigannDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CardigannDefinition")
            .field("id", &self.id)
            .field("contents", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl CardigannDefinition {
    /// Carrega uma definição do diretório v11, recusando recursos fora do recorte.
    ///
    /// # Errors
    ///
    /// Erro sanitizado para YAML inválido, chave não suportada, seletor, regex
    /// ou template inválido, e recursos não implementados.
    pub fn from_yaml_v11(yaml: &str) -> Result<Self, IndexerError> {
        if yaml.len() > 1024 * 1024 {
            return Err(invalid("yaml", "limite de 1 MiB excedido"));
        }
        let document: Document = serde_yaml_ng::from_str(yaml).map_err(|error| {
            // A mensagem do desserializador pode citar valores — inclusive de
            // settings. Só o nome de uma chave desconhecida é repassado.
            let message = error.to_string();
            unknown_key(&message).map_or_else(
                || invalid("yaml", "sintaxe ou tipo não suportado pelo subconjunto v11"),
                |key| IndexerError::UnsupportedDefinitionKey { key },
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

    /// Exige sessão para buscar e para baixar.
    #[must_use]
    pub fn is_private(&self) -> bool {
        self.private
    }

    /// Settings que a definição declara, na ordem dela — o que uma interface
    /// precisa para montar o formulário. Nenhum valor vai junto.
    #[must_use]
    pub fn settings(&self) -> Vec<SettingInfo> {
        self.settings
            .iter()
            .map(|setting| SettingInfo {
                name: setting.name.clone(),
                label: setting.label.clone(),
                kind: match &setting.kind {
                    SettingKind::Text => SettingInfoKind::Text,
                    SettingKind::Password => SettingInfoKind::Password,
                    SettingKind::Checkbox => SettingInfoKind::Checkbox,
                    SettingKind::Select(options) => {
                        SettingInfoKind::Select(options.iter().cloned().collect())
                    }
                },
                default: setting.default.clone(),
            })
            .collect()
    }
}

/// Um setting declarado pela definição.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingInfo {
    pub name: String,
    pub label: String,
    pub kind: SettingInfoKind,
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingInfoKind {
    Text,
    Password,
    Checkbox,
    /// Chaves aceitas, em ordem.
    Select(Vec<String>),
}

impl SettingInfo {
    /// Valor que não deve voltar para a tela: senha, cookie, chave, token.
    ///
    /// Definições declaram cookie como `text`, então o tipo sozinho não basta.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        let name = self.name.to_ascii_lowercase();
        matches!(self.kind, SettingInfoKind::Password)
            || ["cookie", "pass", "key", "token", "secret", "pid", "rss"]
                .iter()
                .any(|word| name.contains(word))
    }
}

fn unknown_key(message: &str) -> Option<String> {
    let start = message.find("unknown field `")? + "unknown field `".len();
    let end = message[start..].find('`')?;
    let key = &message[start..start + end];
    key.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'$'))
        .then(|| key.to_owned())
}

/// Cliente de uma definição; clones compartilham sessão, conexão e rate budget.
#[derive(Clone)]
pub struct CardigannClient {
    definition: Arc<CardigannDefinition>,
    base: Url,
    config: Arc<Vars>,
    http: reqwest::Client,
    budget: Arc<RateBudget>,
    session: Arc<Mutex<Session>>,
}

#[derive(Default)]
struct Session {
    logged_in: bool,
    cookie: Option<HeaderValue>,
}

impl std::fmt::Debug for CardigannClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CardigannClient")
            .field("id", &self.definition.id)
            .field("configuration", &"<redacted>")
            .finish_non_exhaustive()
    }
}

struct Page {
    body: String,
    url: Url,
}

impl CardigannClient {
    /// Seleciona um link declarado e aplica overrides aos settings conhecidos.
    ///
    /// # Errors
    ///
    /// Recusa índice inexistente, setting desconhecido ou ausente e valor
    /// inválido. Redirects ficam desligados na busca: um redirect para outra
    /// origem levaria junto cookie e settings.
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
        if overrides.keys().any(|key| {
            !definition
                .settings
                .iter()
                .any(|setting| &setting.name == key)
        }) {
            return Err(invalid("settings", "override de setting desconhecido"));
        }
        let mut config = Vars::default();
        for setting in &definition.settings {
            let value = overrides
                .remove(&setting.name)
                .or_else(|| setting.default.clone())
                .ok_or_else(|| invalid("settings", "setting obrigatório não configurado"))?;
            validate_setting(&setting.kind, &value)?;
            // Checkbox desmarcado é nulo, como na referência: `if` o lê falso.
            let value = match setting.kind {
                SettingKind::Checkbox if value == "true" => Value::Bool(true),
                SettingKind::Checkbox => Value::Null,
                _ => Value::Str(value),
            };
            config.set(format!(".Config.{}", setting.name), value);
        }
        if timeout.is_zero() {
            return Err(invalid("timeout", "timeout deve ser maior que zero"));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Mozilla/5.0 (compatible; acervo-hub)")
            .build()
            .map_err(IndexerError::Build)?;
        let budget = Arc::new(RateBudget::new(definition.delay));
        Ok(Self {
            definition: Arc::new(definition),
            base,
            config: Arc::new(config),
            http,
            budget,
            session: Arc::new(Mutex::new(Session::default())),
        })
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        self.definition.capabilities()
    }

    fn validate_query(&self, query: &SearchQuery) -> Result<(), IndexerError> {
        if query.offset != 0 {
            return Err(unsupported("paginação por offset não é suportada"));
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
                used.extend(season.is_some().then_some("season"));
                used.extend(episode.is_some().then_some("ep"));
                used.extend(tvdb_id.is_some().then_some("tvdbid"));
                used.extend(imdb_id.is_some().then_some("imdbid"));
            }
            SearchMode::Movie {
                year,
                tmdb_id,
                imdb_id,
            } => {
                used.extend(year.is_some().then_some("year"));
                used.extend(tmdb_id.is_some().then_some("tmdbid"));
                used.extend(imdb_id.is_some().then_some("imdbid"));
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

    /// Variáveis de uma busca, com os nomes do motor de referência.
    fn request_vars(&self, query: &SearchQuery) -> Vars {
        let mut vars = (*self.config).clone();
        let term = query.term.clone().unwrap_or_default();
        let (season, episode, year, imdb, movie_id, series_id) = match &query.mode {
            SearchMode::General => (None, None, None, None, None, None),
            SearchMode::Tv {
                season,
                episode,
                tvdb_id,
                imdb_id,
            } => (
                *season,
                episode.clone(),
                None,
                imdb_id.clone(),
                None,
                *tvdb_id,
            ),
            SearchMode::Movie {
                year,
                tmdb_id,
                imdb_id,
            } => (None, None, *year, imdb_id.clone(), *tmdb_id, None),
        };
        let episode_string = season.map_or_else(String::new, |season| {
            let mut value = format!("S{season:02}");
            if let Some(episode) = &episode {
                let _ = match episode.parse::<u16>() {
                    Ok(number) => write!(value, "E{number:02}"),
                    Err(_) => write!(value, "E{episode}"),
                };
            }
            value
        });
        let keywords = [
            term.clone(),
            year.map(|year| year.to_string()).unwrap_or_default(),
            episode_string.clone(),
        ]
        .into_iter()
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let filtered = self
            .definition
            .keywords_filters
            .iter()
            .try_fold(keywords.clone(), |value, filter| filter.apply(value, &vars))
            .unwrap_or_else(|()| keywords.clone());
        let text = |value: Option<String>| Value::from_option(value.filter(|v| !v.is_empty()));
        vars.set(".Query.Q", text(Some(term)));
        vars.set(".Query.Keywords", text(Some(keywords)));
        vars.set(".Keywords", text(Some(filtered.trim().to_owned())));
        vars.set(".Query.Season", text(season.map(|value| value.to_string())));
        vars.set(".Query.Ep", text(episode));
        vars.set(".Query.Episode", text(Some(episode_string)));
        vars.set(".Query.Year", text(year.map(|value| value.to_string())));
        let imdb_digits = imdb.map(|value| value.trim_start_matches("tt").to_owned());
        vars.set(
            ".Query.IMDBID",
            text(imdb_digits.as_ref().map(|digits| format!("tt{digits}"))),
        );
        vars.set(".Query.IMDBIDShort", text(imdb_digits));
        vars.set(
            ".Query.TMDBID",
            text(movie_id.map(|value| value.to_string())),
        );
        vars.set(
            ".Query.TVDBID",
            text(series_id.map(|value| value.to_string())),
        );
        vars.set(".Categories", Value::List(self.tracker_categories(query)));
        vars.set(".True", Value::Str("True".into()));
        vars.set(".False", Value::Null);
        vars.set(
            ".Today.Year",
            Value::Str(OffsetDateTime::now_utc().year().to_string()),
        );
        vars
    }

    /// Categorias do tracker que cobrem as categorias pedidas.
    fn tracker_categories(&self, query: &SearchQuery) -> Vec<String> {
        self.definition
            .mappings
            .iter()
            .filter(|(_, ids)| {
                ids.iter().any(|id| {
                    query
                        .categories
                        .iter()
                        .any(|requested| category_matches(*requested, *id))
                })
            })
            .map(|(tracker, _)| tracker.clone())
            .collect()
    }

    fn query_urls(
        &self,
        query: &SearchQuery,
        vars: &Vars,
    ) -> Result<Vec<(Url, bool)>, IndexerError> {
        let categories = self.tracker_categories(query);
        let mut urls: Vec<(Url, bool)> = Vec::new();
        for path in &self.definition.paths {
            // Rota restrita a categorias só vale quando a busca pede alguma delas.
            if !path.categories.is_empty()
                && !query.categories.is_empty()
                && !path.categories.iter().any(|id| categories.contains(id))
            {
                continue;
            }
            let mut url = same_origin(&self.base, &path.path, "search.paths.path")?;
            {
                let mut pairs = url.query_pairs_mut();
                for (key, template) in self.definition.inputs.iter().chain(&path.inputs) {
                    let value = template.render(vars);
                    if self.definition.allow_empty_inputs || !value.is_empty() {
                        pairs.append_pair(key, &value);
                    }
                }
                if let Some(raw) = &self.definition.raw_inputs {
                    for pair in raw.render(vars).split('&').filter(|pair| !pair.is_empty()) {
                        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                        if !key.is_empty() {
                            pairs.append_pair(key, value);
                        }
                    }
                }
            }
            if url.query() == Some("") {
                url.set_query(None);
            }
            if !urls.iter().any(|(known, _)| *known == url) {
                urls.push((url, path.follow_redirect));
            }
        }
        Ok(urls)
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

    fn login_error(&self, reason: &'static str) -> IndexerError {
        IndexerError::Login {
            indexer: self.definition.id.clone(),
            reason,
        }
    }

    async fn get(
        &self,
        url: Url,
        cookie: Option<&HeaderValue>,
    ) -> Result<reqwest::Response, IndexerError> {
        self.budget.acquire().await;
        let mut request = self.http.get(url);
        for (name, value) in &self.definition.headers {
            request = request.header(name, value);
        }
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie.clone());
        }
        request.send().await.map_err(|error| self.transport(&error))
    }

    /// Garante sessão. `force` refaz o login mesmo com sessão marcada — é o
    /// caminho quando o site deixou de reconhecê-la.
    async fn ensure_session(&self, force: bool) -> Result<Option<HeaderValue>, IndexerError> {
        let Some(login) = &self.definition.login else {
            return Ok(None);
        };
        let mut session = self.session.lock().await;
        if session.logged_in && !force {
            return Ok(session.cookie.clone());
        }
        session.logged_in = false;
        session.cookie = None;

        match &login.method {
            LoginMethod::Cookie(template) => {
                let value = template.render(&self.config);
                let value = HeaderValue::from_str(value.trim())
                    .ok()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| self.login_error("cookie vazio ou com caractere inválido"))?;
                session.cookie = Some(value);
            }
            LoginMethod::Form { path, post, inputs } => {
                let url = same_origin(&self.base, path, "login.path")?;
                let form: Vec<(String, String)> = inputs
                    .iter()
                    .map(|(key, template)| (key.clone(), template.render(&self.config)))
                    .collect();
                self.budget.acquire().await;
                let request = if *post {
                    self.http.post(url.clone()).form(&form)
                } else {
                    self.http.get(url.clone()).query(&form)
                };
                let response = request
                    .send()
                    .await
                    .map_err(|error| self.transport(&error))?;
                let page = self.follow(response, url, None).await?;
                let document = Html::parse_document(&page.body);
                if login
                    .errors
                    .iter()
                    .any(|css| css.select(document.root_element()).next().is_some())
                {
                    return Err(self.login_error("o site recusou as credenciais"));
                }
            }
        }

        if let Some(test) = &login.test {
            // Cookie recusado quase sempre é cookie vencido, e o conserto é do
            // operador: dizer isso poupa a investigação.
            let refused = match login.method {
                LoginMethod::Cookie(_) => {
                    "o site recusou o cookie (vencido?); copie um novo do navegador logado"
                }
                LoginMethod::Form { .. } => "a página de teste não reconheceu a sessão",
            };
            let url = same_origin(&self.base, &test.path, "login.test.path")?;
            let response = self.get(url, session.cookie.as_ref()).await?;
            if response.status().is_redirection() || !response.status().is_success() {
                return Err(self.login_error(refused));
            }
            if let Some(selector) = &test.selector {
                let body = self.read_text(response).await?;
                if !matches_document(selector, &body) {
                    return Err(self.login_error(refused));
                }
            }
        }
        session.logged_in = true;
        Ok(session.cookie.clone())
    }

    /// Segue redirects na mesma origem — o login por formulário costuma
    /// responder 302 para a página inicial.
    async fn follow(
        &self,
        mut response: reqwest::Response,
        mut url: Url,
        cookie: Option<&HeaderValue>,
    ) -> Result<Page, IndexerError> {
        for _ in 0..=MAX_REDIRECTS {
            if !response.status().is_redirection() {
                if !response.status().is_success() {
                    return Err(IndexerError::Status {
                        indexer: self.definition.id.clone(),
                        status: response.status(),
                    });
                }
                let body = self.read_text(response).await?;
                return Ok(Page { body, url });
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| url.join(location).ok())
                .filter(|next| next.origin() == self.base.origin())
                .ok_or_else(|| IndexerError::Status {
                    indexer: self.definition.id.clone(),
                    status: response.status(),
                })?;
            url = location;
            response = self.get(url.clone(), cookie).await?;
        }
        Err(unsupported("redirects demais"))
    }

    async fn read_text(&self, response: reqwest::Response) -> Result<String, IndexerError> {
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if media_type != "text/html" && media_type != "application/xhtml+xml" {
            return Err(self.unexpected("HTML UTF-8"));
        }
        let bytes = self
            .read_limited(response, MAX_PAGE, "HTML de até 8 MiB")
            .await?;
        String::from_utf8(bytes).map_err(|_| self.unexpected("HTML UTF-8"))
    }

    async fn read_limited(
        &self,
        mut response: reqwest::Response,
        limit: usize,
        expected: &'static str,
    ) -> Result<Vec<u8>, IndexerError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.transport(&error))?
        {
            if bytes.len() + chunk.len() > limit {
                return Err(self.unexpected(expected));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn unexpected(&self, expected: &'static str) -> IndexerError {
        IndexerError::UnexpectedDocument {
            indexer: self.definition.id.clone(),
            expected,
        }
    }

    /// Busca uma página de resultados, refazendo o login uma vez se o site
    /// não reconhecer a sessão (redirect, 401/403 ou página sem o marcador de
    /// login).
    async fn fetch(&self, url: &Url, follow_redirect: bool) -> Result<Page, IndexerError> {
        let mut cookie = self.ensure_session(false).await?;
        for attempt in 0..2 {
            let response = self.get(url.clone(), cookie.as_ref()).await?;
            let status = response.status();
            let lost_session = self.definition.login.is_some()
                && (status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                    || (status.is_redirection() && !follow_redirect));
            if lost_session && attempt == 0 {
                cookie = self.ensure_session(true).await?;
                continue;
            }
            if status.is_redirection() && !follow_redirect {
                return Err(IndexerError::Status {
                    indexer: self.definition.id.clone(),
                    status,
                });
            }
            let page = self.follow(response, url.clone(), cookie.as_ref()).await?;
            let marker = self
                .definition
                .login
                .as_ref()
                .and_then(|login| login.test.as_ref())
                .and_then(|test| test.selector.as_ref());
            if let Some(marker) = marker
                && !matches_document(marker, &page.body)
            {
                if attempt == 0 {
                    cookie = self.ensure_session(true).await?;
                    continue;
                }
                return Err(self.login_error("o site não reconheceu a sessão durante a busca"));
            }
            return Ok(page);
        }
        Err(self.login_error("o site não reconheceu a sessão durante a busca"))
    }
}

fn matches_document(css: &Css, body: &str) -> bool {
    let document = Html::parse_document(body);
    css.select(document.root_element()).next().is_some()
}

#[async_trait]
impl Indexer for CardigannClient {
    fn name(&self) -> &str {
        self.definition.id()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.validate_query(query)?;
        let vars = self.request_vars(query);
        let urls = self.query_urls(query, &vars)?;
        let now = OffsetDateTime::now_utc();
        let mut releases = Vec::new();
        for (url, follow_redirect) in urls {
            let page = self.fetch(&url, follow_redirect).await?;
            releases.extend(self.definition.parse(&page.body, &page.url, &vars, now)?);
        }
        let identified = match &query.mode {
            SearchMode::General => false,
            SearchMode::Tv {
                tvdb_id, imdb_id, ..
            } => tvdb_id.is_some() || imdb_id.is_some(),
            SearchMode::Movie {
                tmdb_id, imdb_id, ..
            } => tmdb_id.is_some() || imdb_id.is_some(),
        };
        if self.definition.and_match
            && !identified
            && let Some(term) = &query.term
        {
            parse::and_match(&mut releases, term);
        }
        let mut releases: Vec<Release> = releases.into_iter().map(|(release, _)| release).collect();
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

    fn proxies_downloads(&self) -> bool {
        self.definition.login.is_some()
    }

    async fn download(&self, url: &Url) -> Result<Vec<u8>, IndexerError> {
        if url.origin() != self.base.origin() {
            return Err(unsupported("link de download fora da origem do indexador"));
        }
        let mut cookie = self.ensure_session(false).await?;
        for attempt in 0..2 {
            let response = self.get(url.clone(), cookie.as_ref()).await?;
            let status = response.status();
            if (status.is_redirection()
                || status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN)
                && attempt == 0
                && self.definition.login.is_some()
            {
                cookie = self.ensure_session(true).await?;
                continue;
            }
            if !status.is_success() {
                return Err(IndexerError::Status {
                    indexer: self.definition.id.clone(),
                    status,
                });
            }
            let bytes = self
                .read_limited(response, MAX_TORRENT, "arquivo .torrent de até 10 MiB")
                .await?;
            // Um .torrent é um dicionário bencode. Qualquer outra coisa —
            // tipicamente a página de login — não pode chegar ao cliente de
            // download como se fosse o arquivo.
            if bytes.first() != Some(&b'd') {
                return Err(self.unexpected("arquivo .torrent"));
            }
            return Ok(bytes);
        }
        Err(self.login_error("o site não reconheceu a sessão no download"))
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

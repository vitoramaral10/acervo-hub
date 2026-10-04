//! Executor de definições Cardigann v11.
//!
//! O formato não tem campo `version`: `from_yaml_v11` escolhe o contrato
//! explicitamente. Cobre indexador público e privado (login por cookie, post,
//! get, oneurl ou formulário), busca por GET ou POST, resposta HTML ou JSON em
//! UTF-8 e o bloco `download`. O que a definição pedir além disso — outra
//! codificação, resposta XML, captcha — é recusado na carga ou no login, com o
//! motivo, em vez de executado pela metade.

mod charset;
mod dates;
mod definition;
mod filters;
mod json;
mod parse;
mod selector;
mod template;

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderValue, LOCATION, REFERER};
use scraper::{ElementRef, Html, Selector};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use url::Url;

use crate::{
    Capabilities, Indexer, IndexerError, RateBudget, Release, ResolvedDownload, SearchMode,
    SearchQuery,
};
use charset::Charset;
use definition::{
    Document, Download, DownloadSelector, Field, FormLogin, Format, Headers, Login, LoginMethod,
    RAW_INPUT, Rows, SearchPath, Setting, SettingKind, same_origin, validate_setting,
};
use filters::Filter;
use parse::{Ctx, evaluate_value, magnet_url};
use selector::Css;
use template::{Template, Value, Vars, url_encode};

const MAX_PAGE: usize = 8 * 1024 * 1024;
const MAX_TORRENT: usize = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Definição validada e compilada. URLs, settings e templates nunca vão a Debug.
// Os booleanos são chaves independentes do YAML, não estados de uma máquina.
#[allow(clippy::struct_excessive_bools)]
pub struct CardigannDefinition {
    id: String,
    name: String,
    description: String,
    language: String,
    private: bool,
    links: Vec<Url>,
    delay: Duration,
    capabilities: Capabilities,
    /// `encoding:` da definição: das páginas lidas e das consultas escritas.
    charset: Charset,
    mappings: BTreeMap<String, Vec<u32>>,
    /// Categorias por descrição (minúscula), para o campo `categorydesc`.
    description_mappings: BTreeMap<String, Vec<u32>>,
    /// Ids do tracker marcados `default`: valem quando a busca não pede categoria.
    default_categories: Vec<String>,
    settings: Vec<Setting>,
    login: Option<Login>,
    paths: Vec<SearchPath>,
    inputs: Vec<(String, Template)>,
    allow_empty_inputs: bool,
    keywords_filters: Vec<Filter>,
    /// Aplicados à página inteira antes do HTML ser lido.
    preprocessing_filters: Vec<Filter>,
    headers: Headers,
    format: Format,
    /// `search.error`: página que casa com algum deles é erro do site.
    search_errors: Vec<Css>,
    rows: Rows,
    and_match: bool,
    /// `rows.after`: quantas linhas seguintes cada linha absorve.
    after: usize,
    date_headers: Option<Field>,
    fields: Vec<(String, Field)>,
    download: Option<Download>,
    test_link_torrent: bool,
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
        // Há definição com BOM, que o YAML não aceita antes do `---`.
        let yaml = yaml.strip_prefix('\u{feff}').unwrap_or(yaml);
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

/// Cabeçalho de uma definição, lido mesmo quando ela é recusada — o que um
/// catálogo precisa para listar tudo, inclusive o que ainda não roda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionHeader {
    pub id: String,
    pub name: String,
    pub description: String,
    pub language: String,
    pub private: bool,
}

impl DefinitionHeader {
    /// `None` se o YAML nem tem `id` e `name`.
    #[must_use]
    pub fn peek(yaml: &str) -> Option<Self> {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).ok()?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_yaml_ng::Value::as_str)
                .map(str::to_owned)
        };
        Some(Self {
            id: text("id")?,
            name: text("name")?,
            description: text("description").unwrap_or_default(),
            language: text("language").unwrap_or_default(),
            private: text("type").is_some_and(|kind| kind != "public"),
        })
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
    /// Os cookies do `http`, lidos no login para o `XSRF-TOKEN`.
    jar: Arc<Jar>,
    budget: Arc<RateBudget>,
    session: Arc<Mutex<Session>>,
    /// Maior página já vista por rota: o tamanho de página do site, aprendido.
    page_sizes: Arc<std::sync::Mutex<BTreeMap<String, usize>>>,
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

/// O que se espera de uma resposta, para decidir o que é "outro tipo de
/// documento". Login aceita qualquer texto: só os seletores de erro importam,
/// e API devolve JSON ali.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Html,
    Json,
    Any,
}

impl From<Format> for Expect {
    fn from(format: Format) -> Self {
        match format {
            Format::Html => Self::Html,
            Format::Json => Self::Json,
        }
    }
}

/// Uma requisição de busca já montada: a URL, o corpo se for POST e o que a
/// rota declara para a resposta.
struct SearchRequest {
    url: Url,
    form: Option<Vec<(String, String)>>,
    follow_redirect: bool,
    no_results: Option<String>,
}

/// O que o login por formulário vai enviar, já lido da página.
struct FormSubmit {
    url: Url,
    pairs: Vec<(String, String)>,
    multipart: bool,
}

static INPUT: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("input").expect("seletor fixo"));
static SIMPLE_CAPTCHA: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"script[src*="simpleCaptcha"]"#).expect("seletor fixo"));

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
        // O link em uso, com a barra final: as definições o colam ao caminho.
        // Uma setting de mesmo nome, se houver, vale mais.
        config.set(".Config.sitelink", Value::Str(base.to_string()));
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
        let jar = Arc::new(Jar::default());
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .cookie_provider(Arc::clone(&jar))
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
            jar,
            budget,
            session: Arc::new(Mutex::new(Session::default())),
            page_sizes: Arc::default(),
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
        // Os demais `.Query.*` (DoubanID, Artist, Genre...) não têm origem
        // aqui e ficam nulos, que é como a referência os declara.
        vars.set(".Query.Type", text(Some(query.mode.function().to_owned())));
        vars.set(
            ".Query.Limit",
            text(query.limit.map(|limit| limit.to_string())),
        );
        vars.set(
            ".Query.Categories",
            Value::List(query.categories.iter().map(u32::to_string).collect()),
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

    /// Categorias do tracker que cobrem as categorias pedidas; sem nenhuma,
    /// as que a definição marca como `default`.
    fn tracker_categories(&self, query: &SearchQuery) -> Vec<String> {
        let mapped: Vec<String> = self
            .definition
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
            .collect();
        if mapped.is_empty() {
            self.definition.default_categories.clone()
        } else {
            mapped
        }
    }

    /// Junta pares à query da URL, na codificação da definição.
    fn append_query(&self, url: &mut Url, pairs: &[(String, String)]) {
        if pairs.is_empty() {
            return;
        }
        let encoded = self.definition.charset.form_encode(pairs);
        let query = match url.query() {
            Some(existing) if !existing.is_empty() => format!("{existing}&{encoded}"),
            _ => encoded,
        };
        url.set_query(Some(&query));
    }

    /// Corpo de formulário na codificação da definição.
    fn form_body(
        &self,
        request: reqwest::RequestBuilder,
        pairs: &[(String, String)],
    ) -> reqwest::RequestBuilder {
        request
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(self.definition.charset.form_encode(pairs))
    }

    /// Cabeçalhos de uma seção com os templates resolvidos em `vars`.
    fn render_headers(headers: &Headers, vars: &Vars) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, template)| (name.clone(), template.render(vars)))
            .collect()
    }

    /// Os cabeçalhos de `search.headers`, resolvidos só com as settings.
    fn plain_headers(&self) -> Vec<(String, String)> {
        Self::render_headers(&self.definition.headers, &self.config)
    }

    fn search_requests(
        &self,
        query: &SearchQuery,
        vars: &Vars,
    ) -> Result<Vec<SearchRequest>, IndexerError> {
        let tracker = self.tracker_categories(query);
        let mut requests: Vec<SearchRequest> = Vec::new();
        for path in &self.definition.paths {
            let mut vars = vars.clone();
            let mut categories = tracker.clone();
            // Rota restrita a categorias só vale quando a busca mapeia alguma
            // delas; `!` na frente inverte a regra. A rota enxerga só as
            // categorias que compartilha com a busca.
            if !path.categories.is_empty() && !tracker.is_empty() {
                let negated = path.categories.first().is_some_and(|first| first == "!");
                let shared: Vec<String> = tracker
                    .iter()
                    .filter(|id| path.categories.contains(id))
                    .cloned()
                    .collect();
                if shared.is_empty() != negated {
                    continue;
                }
                categories = shared;
            }
            vars.set(".Categories", Value::List(categories));
            // Variável em caminho de URL vai codificada; `+` vira `%20`, que é
            // o que um caminho entende por espaço.
            let rendered = match path.path.literal() {
                Some(literal) => literal.to_owned(),
                None => path.path.render_with(&vars, url_encode).replace('+', "%20"),
            };
            let mut url = same_origin(&self.base, &rendered, "search.paths.path")?;
            let mut pairs: Vec<(String, String)> = Vec::new();
            let inherited = if path.inherit_inputs {
                &self.definition.inputs[..]
            } else {
                &[]
            };
            for (key, template) in inherited.iter().chain(&path.inputs) {
                if key == RAW_INPUT {
                    // Pares já prontos: `a=1&b=2`.
                    for pair in template
                        .render(&vars)
                        .split('&')
                        .filter(|pair| !pair.is_empty())
                    {
                        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                        if !name.is_empty() {
                            pairs.push((name.to_owned(), value.to_owned()));
                        }
                    }
                    continue;
                }
                let value = template.render(&vars);
                if self.definition.allow_empty_inputs || !value.trim().is_empty() {
                    pairs.push((key.clone(), value));
                }
            }
            let form = if path.post {
                Some(pairs)
            } else {
                self.append_query(&mut url, &pairs);
                if url.query() == Some("") {
                    url.set_query(None);
                }
                None
            };
            if form.is_none()
                && requests
                    .iter()
                    .any(|known| known.form.is_none() && known.url == url)
            {
                continue;
            }
            requests.push(SearchRequest {
                url,
                form,
                follow_redirect: path.follow_redirect,
                no_results: path.no_results.clone(),
            });
        }
        Ok(requests)
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
        self.get_with(url, cookie, &self.plain_headers()).await
    }

    async fn get_with(
        &self,
        url: Url,
        cookie: Option<&HeaderValue>,
        headers: &[(String, String)],
    ) -> Result<reqwest::Response, IndexerError> {
        self.budget.acquire().await;
        let mut request = self.http.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|error| self.transport(&error))?;
        self.not_limited(response)
    }

    async fn post_with(
        &self,
        url: Url,
        form: &[(String, String)],
        cookie: Option<&HeaderValue>,
        headers: &[(String, String)],
    ) -> Result<reqwest::Response, IndexerError> {
        self.budget.acquire().await;
        let mut request = self.form_body(self.http.post(url), form);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|error| self.transport(&error))?;
        self.not_limited(response)
    }

    /// 429 vira erro na hora, em qualquer requisição (busca, login, página
    /// seguinte, download): quem chama para ali, sem tentar a próxima.
    fn not_limited(&self, response: reqwest::Response) -> Result<reqwest::Response, IndexerError> {
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(IndexerError::from_too_many_requests(
                &self.definition.id,
                &response,
            ));
        }
        Ok(response)
    }

    /// Cabeçalhos do login: os do próprio bloco, ou os da busca.
    fn login_headers(&self, login: &Login) -> Vec<(String, String)> {
        match &login.headers {
            Some(headers) => Self::render_headers(headers, &self.config),
            None => self.plain_headers(),
        }
    }

    /// Os cookies fixos de `login.cookies` entram no jar antes do login.
    fn seed_login_cookies(&self, login: &Login) {
        for cookie in &login.cookies {
            self.jar
                .add_cookie_str(&format!("{}; Path=/", cookie.trim()), &self.base);
        }
    }

    /// O site recusou o login? `login.error` casa na página que ele devolveu.
    fn check_login_errors(&self, login: &Login, body: &str) -> Result<(), IndexerError> {
        let document = Html::parse_document(body);
        if login
            .errors
            .iter()
            .any(|css| css.first(document.root_element()).is_some())
        {
            return Err(self.login_error("o site recusou as credenciais"));
        }
        Ok(())
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
            LoginMethod::Post { path, inputs } => {
                let url = same_origin(&self.base, &path.render(&self.config), "login.path")?;
                let form = self.render_pairs(inputs);
                self.seed_login_cookies(login);
                let request = self.post_request(&url, &self.login_headers(login)).await?;
                let request = self.form_body(request, &form);
                let response = request
                    .send()
                    .await
                    .map_err(|error| self.transport(&error))?;
                let response = self.not_limited(response)?;
                let page = self.follow(response, url, None, Expect::Any).await?;
                self.check_login_errors(login, &page.body)?;
            }
            LoginMethod::Get { path, inputs } => {
                let url = same_origin(&self.base, &path.render(&self.config), "login.path")?;
                let form = self.render_pairs(inputs);
                self.seed_login_cookies(login);
                self.budget.acquire().await;
                let mut target = url.clone();
                self.append_query(&mut target, &form);
                let mut request = self.http.get(target);
                for (name, value) in self.login_headers(login) {
                    request = request.header(name, value);
                }
                let response = request
                    .send()
                    .await
                    .map_err(|error| self.transport(&error))?;
                let response = self.not_limited(response)?;
                let page = self.follow(response, url, None, Expect::Any).await?;
                self.check_login_errors(login, &page.body)?;
            }
            LoginMethod::OneUrl { path, input } => {
                let target = format!(
                    "{}{}",
                    path.render(&self.config),
                    input.render(&self.config)
                );
                let url = same_origin(&self.base, &target, "login.path")?;
                self.seed_login_cookies(login);
                let response = self
                    .get_with(url.clone(), None, &self.login_headers(login))
                    .await?;
                let page = self.follow(response, url, None, Expect::Any).await?;
                self.check_login_errors(login, &page.body)?;
            }
            LoginMethod::Form(form) => self.form_login(login, form).await?,
        }

        if let Some(test) = &login.test {
            // Cookie recusado quase sempre é cookie vencido, e o conserto é do
            // operador: dizer isso poupa a investigação.
            let refused = match login.method {
                LoginMethod::Cookie(_) => {
                    "o site recusou o cookie (vencido?); copie um novo do navegador logado"
                }
                _ => "a página de teste não reconheceu a sessão",
            };
            let url = same_origin(&self.base, &test.path, "login.test.path")?;
            let response = self.get(url, session.cookie.as_ref()).await?;
            if response.status().is_redirection() || !response.status().is_success() {
                return Err(self.login_error(refused));
            }
            // O marcador é de HTML: uma página de teste que responde JSON (API)
            // só tem o status para conferir.
            if let Some(selector) = &test.selector
                && is_html(&response)
            {
                let body = self.read_text(response, Expect::Html).await?;
                if !matches_document(selector, &body) {
                    return Err(self.login_error(refused));
                }
            }
        }
        session.logged_in = true;
        Ok(session.cookie.clone())
    }

    fn render_pairs(&self, inputs: &[(String, Template)]) -> Vec<(String, String)> {
        inputs
            .iter()
            .map(|(key, template)| (key.clone(), template.render(&self.config)))
            .collect()
    }

    /// Login por formulário, como a referência: abre a página, junta os
    /// campos que o formulário já traz (tokens escondidos), sobrepõe os
    /// `inputs` da definição e envia para o `action` — ou `submitpath`.
    ///
    /// Captcha não se resolve aqui: se a página o mostra, o login para com
    /// essa causa, em vez de enviar credencial sem a resposta.
    async fn form_login(&self, login: &Login, form: &FormLogin) -> Result<(), IndexerError> {
        let login_url = same_origin(&self.base, &form.path.render(&self.config), "login.path")?;
        self.seed_login_cookies(login);
        let headers = self.login_headers(login);
        let response = self.get_with(login_url.clone(), None, &headers).await?;
        let landing = self
            .follow(response, login_url.clone(), None, Expect::Html)
            .await?;
        let submit = self.prepare_form_login(form, &login_url, &landing.body)?;
        let FormSubmit {
            url: submit,
            pairs,
            multipart,
        } = submit;

        self.budget.acquire().await;
        let mut request = self
            .http
            .post(submit.clone())
            .header(REFERER, login_url.as_str());
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        request = if multipart {
            let boundary = "----acervo-hub-form-boundary";
            let mut body = String::new();
            for (name, value) in &pairs {
                let _ = write!(
                    body,
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n",
                    name.replace('"', "%22")
                );
            }
            let _ = write!(body, "--{boundary}--\r\n");
            request
                .header(
                    CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(body)
        } else {
            self.form_body(request, &pairs)
        };
        let response = request
            .send()
            .await
            .map_err(|error| self.transport(&error))?;
        let response = self.not_limited(response)?;
        let page = self.follow(response, submit, None, Expect::Any).await?;
        self.check_login_errors(login, &page.body)
    }

    /// Lê a página de login e monta o envio. Fica separado do envio porque o
    /// documento HTML não atravessa um `await`.
    fn prepare_form_login(
        &self,
        form: &FormLogin,
        login_url: &Url,
        body: &str,
    ) -> Result<FormSubmit, IndexerError> {
        let document = Html::parse_document(body);
        let root = document.root_element();

        if form
            .captcha
            .as_ref()
            .is_some_and(|css| css.first(root).is_some())
            || root.select(&SIMPLE_CAPTCHA).next().is_some()
        {
            return Err(self.login_error(
                "o site exige captcha no login; este executor não resolve captcha (use login por cookie)",
            ));
        }
        let element = form
            .form
            .first(root)
            .ok_or_else(|| self.login_error("formulário de login não encontrado na página"))?;

        let mut pairs: Vec<(String, String)> = Vec::new();
        for input in element.select(&INPUT) {
            let attributes = input.value();
            let Some(name) = attributes.attr("name") else {
                continue;
            };
            if attributes.attr("disabled").is_some() {
                continue;
            }
            let kind = attributes
                .attr("type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(kind.as_str(), "checkbox" | "radio") && attributes.attr("checked").is_none()
            {
                continue;
            }
            set_pair(
                &mut pairs,
                name,
                attributes.attr("value").unwrap_or_default(),
            );
        }
        for input in &form.inputs {
            let key = match &input.key_css {
                Some(css) => css
                    .first(root)
                    .and_then(|found| found.value().attr("name"))
                    .ok_or_else(|| self.login_error("campo de login não encontrado pelo seletor"))?
                    .to_owned(),
                None => input.key.clone(),
            };
            set_pair(&mut pairs, &key, &input.value.render(&self.config));
        }
        for (key, field) in &form.selector_inputs {
            if let Some(value) = self.read_login_field(field, root)? {
                set_pair(&mut pairs, key, &value);
            }
        }
        let mut query: Vec<(String, String)> = Vec::new();
        for (key, field) in &form.get_selector_inputs {
            if let Some(value) = self.read_login_field(field, root)? {
                query.push((key.clone(), value));
            }
        }

        let action = form
            .submit_path
            .as_deref()
            .or_else(|| element.value().attr("action"))
            .unwrap_or_default();
        let mut submit = login_url
            .join(action)
            .map_err(|_| invalid("login.form", "action do formulário inválido"))?;
        if submit.origin() != self.base.origin()
            || !submit.username().is_empty()
            || submit.password().is_some()
        {
            return Err(self.login_error("o formulário envia para outra origem"));
        }
        self.append_query(&mut submit, &query);
        let multipart = element.value().attr("enctype") == Some("multipart/form-data");
        Ok(FormSubmit {
            url: submit,
            pairs,
            multipart,
        })
    }

    /// Campo lido da página de login: ausente é erro, a não ser que opcional.
    fn read_login_field(
        &self,
        field: &Field,
        root: ElementRef<'_>,
    ) -> Result<Option<String>, IndexerError> {
        match evaluate_value(field, &Ctx::Html(root), &self.config) {
            Some(value) => Ok(Some(value)),
            None if field.optional => Ok(None),
            None => Err(self.login_error("campo do formulário de login não encontrado")),
        }
    }

    /// POST de login como o navegador faz: abre a página antes, e se o site
    /// deixou o `XSRF-TOKEN` (Laravel, Rails com axios) devolve-o no
    /// cabeçalho. Sem isso, o site responde 419 e a senha nem é conferida.
    async fn post_request(
        &self,
        url: &Url,
        headers: &[(String, String)],
    ) -> Result<reqwest::RequestBuilder, IndexerError> {
        // O que a página responde não importa: só os cookies que ela deixa.
        drop(self.get_with(url.clone(), None, headers).await?);
        let mut request = self.http.post(url.clone());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(token) = self.xsrf_token(url) {
            request = request.header("X-XSRF-TOKEN", token);
        }
        self.budget.acquire().await;
        Ok(request)
    }

    fn xsrf_token(&self, url: &Url) -> Option<HeaderValue> {
        let cookies = self.jar.cookies(url)?;
        let token = cookies
            .to_str()
            .ok()?
            .split(';')
            .filter_map(|pair| pair.trim().split_once('='))
            .find_map(|(name, value)| (name == "XSRF-TOKEN").then_some(value))?;
        // Percent-decode sem a regra de formulário, que leria `+` como espaço.
        let token = token.replace('+', "%2B");
        let decoded = url::form_urlencoded::parse(format!("t={token}").as_bytes())
            .next()
            .map(|(_, value)| value.into_owned())?;
        HeaderValue::from_str(&decoded).ok()
    }

    /// Segue redirects na mesma origem — o login por formulário costuma
    /// responder 302 para a página inicial.
    async fn follow(
        &self,
        mut response: reqwest::Response,
        mut url: Url,
        cookie: Option<&HeaderValue>,
        expect: Expect,
    ) -> Result<Page, IndexerError> {
        for _ in 0..=MAX_REDIRECTS {
            if !response.status().is_redirection() {
                if !response.status().is_success() {
                    return Err(IndexerError::Status {
                        indexer: self.definition.id.clone(),
                        status: response.status(),
                    });
                }
                let body = self.read_text(response, expect).await?;
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

    /// Corpo como texto UTF-8. HTML só chega com tipo de HTML — um JSON ou uma
    /// imagem no lugar da página é o site fora do contrato. Resposta JSON
    /// declarada pela definição aceita o tipo que o site mandar: API que
    /// responde `text/plain` existe, e o conteúdo é conferido ao ler.
    async fn read_text(
        &self,
        response: reqwest::Response,
        expect: Expect,
    ) -> Result<String, IndexerError> {
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
        let (expected, limit) = match expect {
            Expect::Html => ("HTML UTF-8", "HTML de até 8 MiB"),
            Expect::Json => ("JSON UTF-8", "JSON de até 8 MiB"),
            Expect::Any => ("texto UTF-8", "texto de até 8 MiB"),
        };
        let acceptable = match expect {
            Expect::Any => true,
            Expect::Html => matches!(media_type.as_str(), "text/html" | "application/xhtml+xml"),
            Expect::Json => {
                media_type.contains("json")
                    || media_type.starts_with("text/")
                    || media_type.is_empty()
            }
        };
        if !acceptable {
            return Err(self.unexpected(expected));
        }
        let bytes = self.read_limited(response, MAX_PAGE, limit).await?;
        self.definition
            .charset
            .decode(&bytes)
            .ok_or_else(|| self.unexpected(expected))
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
    async fn fetch(&self, request: &SearchRequest, vars: &Vars) -> Result<Page, IndexerError> {
        let headers = Self::render_headers(&self.definition.headers, vars);
        let format = self.definition.format;
        let mut cookie = self.ensure_session(false).await?;
        for attempt in 0..2 {
            let response = match &request.form {
                Some(form) => {
                    self.post_with(request.url.clone(), form, cookie.as_ref(), &headers)
                        .await?
                }
                None => {
                    self.get_with(request.url.clone(), cookie.as_ref(), &headers)
                        .await?
                }
            };
            let status = response.status();
            let lost_session = self.definition.login.is_some()
                && (status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                    || (status.is_redirection() && !request.follow_redirect));
            if lost_session && attempt == 0 {
                cookie = self.ensure_session(true).await?;
                continue;
            }
            if status.is_redirection() && !request.follow_redirect {
                return Err(IndexerError::Status {
                    indexer: self.definition.id.clone(),
                    status,
                });
            }
            let page = self
                .follow(
                    response,
                    request.url.clone(),
                    cookie.as_ref(),
                    format.into(),
                )
                .await?;
            // O marcador de sessão é HTML: numa resposta JSON não se aplica.
            let marker = self
                .definition
                .login
                .as_ref()
                .and_then(|login| login.test.as_ref())
                .and_then(|test| test.selector.as_ref())
                .filter(|_| format == Format::Html);
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

    /// Variáveis do bloco `download`: as settings e as partes da URL do release
    /// (`.DownloadUri.*`).
    fn download_vars(&self, link: &Url) -> Vars {
        let mut vars = (*self.config).clone();
        let text = |value: &str| Value::Str(value.to_owned());
        vars.set(".DownloadUri.AbsoluteUri", text(link.as_str()));
        vars.set(".DownloadUri.AbsolutePath", text(link.path()));
        vars.set(".DownloadUri.Scheme", text(link.scheme()));
        vars.set(
            ".DownloadUri.Host",
            text(link.host_str().unwrap_or_default()),
        );
        vars.set(
            ".DownloadUri.Port",
            Value::Str(
                link.port_or_known_default()
                    .map(|port| port.to_string())
                    .unwrap_or_default(),
            ),
        );
        let path_and_query = match link.query() {
            Some(query) => format!("{}?{query}", link.path()),
            None => link.path().to_owned(),
        };
        vars.set(".DownloadUri.PathAndQuery", Value::Str(path_and_query));
        vars.set(
            ".DownloadUri.Query",
            Value::Str(
                link.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default(),
            ),
        );
        let mut seen = Vec::new();
        for (key, value) in link.query_pairs() {
            // Parâmetro repetido: vale o primeiro.
            if !seen.contains(&key) {
                vars.set(
                    format!(".DownloadUri.Query.{key}"),
                    Value::Str(value.to_string()),
                );
                seen.push(key);
            }
        }
        vars
    }

    /// Página para o bloco `download`: GET na URL, sem sair da origem.
    async fn download_page(
        &self,
        url: &Url,
        cookie: Option<&HeaderValue>,
        headers: &[(String, String)],
    ) -> Result<Page, IndexerError> {
        let response = self.get_with(url.clone(), cookie, headers).await?;
        self.follow(response, url.clone(), cookie, Expect::Html)
            .await
    }

    /// `selector` do bloco `download` aplicado a uma página: o valor do
    /// primeiro elemento que casa (`attribute` ou texto), depois dos filtros.
    fn match_download_selector(
        selector: &DownloadSelector,
        page: &Page,
        vars: &Vars,
    ) -> Option<String> {
        let css = Css::parse(&selector.selector.render(vars), "download.selector").ok()?;
        let document = Html::parse_document(&page.body);
        let element = css.first(document.root_element())?;
        let mut value = match &selector.attribute {
            Some(attribute) => element.value().attr(attribute)?.to_owned(),
            None => element.text().collect(),
        };
        for filter in &selector.filters {
            value = filter.apply(value, vars).ok()?;
        }
        Some(value)
    }

    /// Segue o bloco `download` da definição até o arquivo (ou o magnet).
    ///
    /// Tudo precisa ficar na origem do indexador: o link que a página
    /// entrega vem de fora, e seguir um endereço qualquer a partir dele
    /// deixaria o servidor buscar o que a página mandar. Magnet não é
    /// buscado, só devolvido.
    async fn resolve_plan(
        &self,
        plan: &Download,
        link: &Url,
        cookie: Option<&HeaderValue>,
    ) -> Result<ResolvedDownload, IndexerError> {
        let vars = self.download_vars(link);
        let headers = match &plan.headers {
            Some(headers) => Self::render_headers(headers, &vars),
            None => Self::render_headers(&self.definition.headers, &vars),
        };
        let mut before_page: Option<Page> = None;
        if let Some(before) = &plan.before {
            let path = match &before.path_selector {
                Some(selector) => {
                    let page = self.download_page(link, cookie, &headers).await?;
                    Self::match_download_selector(selector, &page, &vars)
                        .ok_or_else(|| self.unexpected("página com o caminho do download"))?
                }
                None => before
                    .path
                    .as_ref()
                    .map(|path| path.render(&vars))
                    .unwrap_or_default(),
            };
            let mut target = same_origin(&self.base, &path, "download.before.path")?;
            let pairs: Vec<(String, String)> = before
                .inputs
                .iter()
                .map(|(key, template)| (key.clone(), template.render(&vars)))
                .collect();
            let response = if before.post {
                self.post_with(target, &pairs, cookie, &headers).await?
            } else {
                self.append_query(&mut target, &pairs);
                self.get_with(target.clone(), cookie, &headers).await?
            };
            let url = link.clone();
            before_page = Some(self.follow(response, url, cookie, Expect::Html).await?);
        }

        if let Some(infohash) = &plan.infohash {
            let own;
            let page = match &before_page {
                Some(page) if infohash.use_before_response => page,
                _ => {
                    own = self.download_page(link, cookie, &headers).await?;
                    &own
                }
            };
            let hash = Self::match_download_selector(&infohash.hash, page, &vars);
            let title = Self::match_download_selector(&infohash.title, page, &vars);
            return hash
                .zip(title)
                .and_then(|(hash, title)| magnet_url(&hash, &title))
                .map(ResolvedDownload::Magnet)
                .ok_or_else(|| self.unexpected("página com o infohash e o título"));
        }

        if plan.selectors.is_empty() {
            return self.fetch_torrent(link, plan.post, cookie, &headers).await;
        }
        let mut own: Option<Page> = None;
        for selector in &plan.selectors {
            let page = match &before_page {
                Some(page) if selector.use_before_response => page,
                _ => {
                    if own.is_none() {
                        own = Some(self.download_page(link, cookie, &headers).await?);
                    }
                    own.as_ref().expect("página acabou de ser lida")
                }
            };
            let Some(href) = Self::match_download_selector(selector, page, &vars) else {
                continue;
            };
            let Ok(target) = link.join(href.trim()) else {
                continue;
            };
            if target.scheme() == "magnet" {
                return Ok(ResolvedDownload::Magnet(target));
            }
            if target.origin() != self.base.origin()
                || !target.username().is_empty()
                || target.password().is_some()
            {
                continue;
            }
            match self
                .fetch_torrent(&target, plan.post, cookie, &headers)
                .await
            {
                Ok(torrent) => return Ok(torrent),
                // Link que não entrega um .torrent: tenta o seletor seguinte
                // (a menos que a definição tenha desligado a conferência).
                Err(IndexerError::UnexpectedDocument { .. })
                    if self.definition.test_link_torrent => {}
                Err(other) => return Err(other),
            }
        }
        Err(self.unexpected("página com um link de .torrent"))
    }

    /// Baixa o `.torrent` e confere que é bencode.
    async fn fetch_torrent(
        &self,
        url: &Url,
        post: bool,
        cookie: Option<&HeaderValue>,
        headers: &[(String, String)],
    ) -> Result<ResolvedDownload, IndexerError> {
        let response = if post {
            self.post_with(url.clone(), &[], cookie, headers).await?
        } else {
            self.get_with(url.clone(), cookie, headers).await?
        };
        let status = response.status();
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
        Ok(ResolvedDownload::Torrent(bytes))
    }
}

/// Define `key`, trocando o valor se ela já existe: o formulário não repete
/// nome, e o que a definição declara vale mais que o que a página trazia.
fn set_pair(pairs: &mut Vec<(String, String)>, key: &str, value: &str) {
    match pairs.iter_mut().find(|(known, _)| known == key) {
        Some(pair) => value.clone_into(&mut pair.1),
        None => pairs.push((key.to_owned(), value.to_owned())),
    }
}

fn is_html(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|header| header.to_str().ok())
        .is_none_or(|media| {
            let media = media.to_ascii_lowercase();
            media.contains("text/html") || media.contains("application/xhtml+xml")
        })
}

/// O seletor acha algo no documento — a própria raiz inclusive, que é onde
/// `:root:contains(...)` casa.
fn matches_document(css: &Css, body: &str) -> bool {
    let document = Html::parse_document(body);
    css.first(document.root_element()).is_some()
}

#[async_trait]
impl Indexer for CardigannClient {
    fn name(&self) -> &str {
        self.definition.id()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        self.validate_query(query)?;
        let vars = self.request_vars(query);
        let requests = self.search_requests(query, &vars)?;
        let now = OffsetDateTime::now_utc();
        let mut releases = Vec::new();
        // Rotas com o mesmo caminho são páginas da mesma listagem. Página
        // vazia, ou menor que a anterior, é a última: pedir as seguintes só
        // gasta o intervalo do tracker para receber nada — num site lento com
        // cinco páginas fixas, era a diferença entre 6 s e 30 s.
        let mut previous: Option<(String, usize)> = None;
        let mut exhausted: Option<String> = None;
        for request in requests {
            let path = request.url.path().to_owned();
            if exhausted.as_deref() == Some(path.as_str()) {
                continue;
            }
            let page = self.fetch(&request, &vars).await?;
            if let Some(message) = &request.no_results {
                // Mensagem vazia quer dizer "corpo vazio", como na referência.
                let nothing = if message.trim().is_empty() {
                    page.body.trim().is_empty()
                } else {
                    page.body.contains(message.as_str())
                };
                if nothing {
                    exhausted = Some(path);
                    continue;
                }
            }
            if self.definition.format == Format::Html
                && !self.definition.search_errors.is_empty()
                && matches_any(&self.definition.search_errors, &page.body)
            {
                return Err(self
                    .unexpected("página de resultados (o site respondeu com uma página de erro)"));
            }
            let (found, rows) = self.definition.parse(&page.body, &page.url, &vars, now)?;
            releases.extend(found);
            let shorter = previous
                .as_ref()
                .is_some_and(|(last_path, last_rows)| *last_path == path && rows < *last_rows);
            // O tamanho de página do site não vem na definição, mas se aprende:
            // é a maior página já vista nesta rota. Página menor que ele já é a
            // última, sem precisar pedir a seguinte para descobrir.
            let below_known_size = {
                let mut sizes = self
                    .page_sizes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let known = sizes.entry(path.clone()).or_insert(0);
                let below = rows < *known;
                *known = (*known).max(rows);
                below
            };
            if rows == 0 || shorter || below_known_size {
                exhausted = Some(path.clone());
            }
            previous = Some((path, rows));
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
        self.definition.login.is_some() || self.definition.download.is_some()
    }

    async fn download(&self, url: &Url) -> Result<Vec<u8>, IndexerError> {
        match self.resolve_download(url).await? {
            ResolvedDownload::Torrent(bytes) => Ok(bytes),
            ResolvedDownload::Magnet(_) => Err(unsupported(
                "o download resolve para um magnet; use resolve_download",
            )),
        }
    }

    async fn resolve_download(&self, url: &Url) -> Result<ResolvedDownload, IndexerError> {
        if url.scheme() == "magnet" {
            return Ok(ResolvedDownload::Magnet(url.clone()));
        }
        if url.origin() != self.base.origin() {
            return Err(unsupported("link de download fora da origem do indexador"));
        }
        let mut cookie = self.ensure_session(false).await?;
        for attempt in 0..2 {
            let outcome = match &self.definition.download {
                Some(plan) => self.resolve_plan(plan, url, cookie.as_ref()).await,
                None => {
                    self.fetch_torrent(url, false, cookie.as_ref(), &self.plain_headers())
                        .await
                }
            };
            let session_lost = match &outcome {
                Err(IndexerError::Status { status, .. }) => {
                    status.is_redirection()
                        || *status == reqwest::StatusCode::UNAUTHORIZED
                        || *status == reqwest::StatusCode::FORBIDDEN
                }
                // Sem sessão, o site devolve a página de login no lugar do
                // arquivo (ou das páginas que o bloco `download` lê).
                Err(IndexerError::UnexpectedDocument { .. }) => self.definition.download.is_some(),
                _ => false,
            };
            if session_lost && attempt == 0 && self.definition.login.is_some() {
                cookie = self.ensure_session(true).await?;
                continue;
            }
            return outcome;
        }
        Err(self.login_error("o site não reconheceu a sessão no download"))
    }
}

fn matches_any(selectors: &[Css], body: &str) -> bool {
    let document = Html::parse_document(body);
    selectors
        .iter()
        .any(|css| css.first(document.root_element()).is_some())
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

//! O YAML de uma definição e sua compilação.
//!
//! Tudo que a definição declara é validado aqui — seletor, regex, template,
//! rota —, para que um erro apareça na subida e não na primeira busca. Chave
//! desconhecida é recusada: um recurso ignorado em silêncio vira resultado
//! errado sem ninguém saber por quê.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use reqwest::header::HeaderName;
use serde::Deserialize;
use serde_yaml_ng::{Mapping, Value as Yaml};
use url::Url;

use super::charset::Charset;
use super::filters::Filter;
use super::json::{JsonPath, path_part};
use super::selector::Css;
use super::template::{Names, Scope, Template, Vars};
use super::{CardigannDefinition, invalid};
use crate::{Capabilities, Category, IndexerError, SearchSupport};

// Tipos de entrada são privados e nunca implementam Debug: o YAML pode
// conter credencial, inclusive em campos que a validação vai recusar.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    id: String,
    name: String,
    description: String,
    language: String,
    #[serde(rename = "type")]
    kind: String,
    encoding: String,
    #[serde(default, rename = "requestDelay")]
    request_delay: Option<f64>,
    links: Vec<String>,
    #[serde(default)]
    legacylinks: Vec<String>,
    #[serde(default)]
    replaces: Vec<String>,
    #[serde(default)]
    followredirect: bool,
    // `false` desliga a conferência do link resolvido por `download.selectors`.
    #[serde(default = "yes")]
    testlinktorrent: bool,
    // Metadado de TLS da referência: não muda a busca.
    #[serde(default)]
    certificates: Option<Yaml>,
    caps: RawCaps,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    login: Option<RawLogin>,
    search: RawSearch,
    #[serde(default)]
    download: Option<RawDownload>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCaps {
    #[serde(default)]
    categories: Option<BTreeMap<String, String>>,
    #[serde(default)]
    categorymappings: Option<Vec<RawMapping>>,
    modes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    allowrawsearch: Option<bool>,
    // A referência não lê esta chave; vem nas definições herdadas do Jackett.
    #[serde(default)]
    allowtvsearchimdb: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMapping {
    id: Yaml,
    cat: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    default: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    default: Option<Yaml>,
    #[serde(default)]
    options: Option<Mapping>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLogin {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    submitpath: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    form: Option<String>,
    #[serde(default)]
    selectors: bool,
    #[serde(default)]
    cookies: Vec<String>,
    #[serde(default)]
    inputs: Mapping,
    #[serde(default)]
    selectorinputs: Mapping,
    #[serde(default)]
    getselectorinputs: Mapping,
    #[serde(default)]
    headers: Option<BTreeMap<String, Yaml>>,
    #[serde(default)]
    error: Vec<RawLoginError>,
    #[serde(default)]
    test: Option<RawLoginTest>,
    #[serde(default)]
    captcha: Option<RawCaptcha>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCaptcha {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    selector: Option<String>,
    // O nome do campo onde o operador digitaria a resposta: sem tela para
    // isso, não é lido.
    #[serde(default)]
    input: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoginError {
    #[serde(default)]
    path: Option<String>,
    selector: String,
    #[serde(default)]
    message: Option<Yaml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoginTest {
    path: String,
    #[serde(default)]
    selector: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSearch {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    paths: Option<Vec<RawPath>>,
    #[serde(default)]
    inputs: Mapping,
    #[serde(default)]
    error: Vec<RawLoginError>,
    #[serde(default)]
    keywordsfilters: Vec<RawFilterYaml>,
    #[serde(default)]
    preprocessingfilters: Vec<RawFilterYaml>,
    #[serde(default)]
    headers: Option<BTreeMap<String, Yaml>>,
    #[serde(default, rename = "allowEmptyInputs")]
    allow_empty_inputs: bool,
    rows: RawRows,
    fields: Mapping,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPath {
    path: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    inputs: Mapping,
    #[serde(default)]
    categories: Vec<Yaml>,
    #[serde(default)]
    followredirect: Option<bool>,
    #[serde(default)]
    inheritinputs: Option<bool>,
    #[serde(default)]
    response: Option<RawResponse>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResponse {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "noResultsMessage")]
    no_results_message: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRows {
    selector: String,
    #[serde(default)]
    filters: Vec<RawFilterYaml>,
    #[serde(default)]
    after: Option<usize>,
    #[serde(default)]
    dateheaders: Option<RawField>,
    #[serde(default)]
    count: Option<RawField>,
    #[serde(default)]
    multiple: bool,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default, rename = "missingAttributeEqualsNoResults")]
    missing_attribute_equals_no_results: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default)]
    text: Option<Yaml>,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    default: Option<Yaml>,
    #[serde(default)]
    filters: Vec<RawFilterYaml>,
    #[serde(default)]
    remove: Option<String>,
    #[serde(default)]
    case: Option<Mapping>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFilterYaml {
    name: String,
    #[serde(default)]
    args: Option<Yaml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDownload {
    #[serde(default)]
    selectors: Vec<RawDownloadSelector>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    before: Option<RawBefore>,
    #[serde(default)]
    infohash: Option<RawInfohash>,
    #[serde(default)]
    headers: Option<BTreeMap<String, Yaml>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDownloadSelector {
    selector: String,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default)]
    usebeforeresponse: bool,
    #[serde(default)]
    filters: Vec<RawFilterYaml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBefore {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    inputs: Mapping,
    #[serde(default)]
    pathselector: Option<RawDownloadSelector>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInfohash {
    hash: RawDownloadSelector,
    title: RawDownloadSelector,
    #[serde(default)]
    usebeforeresponse: bool,
}

/// Filtro com os argumentos já em texto.
pub(super) struct RawFilter {
    pub name: String,
    pub args: Vec<String>,
}

impl RawFilterYaml {
    fn flatten(self) -> Result<RawFilter, IndexerError> {
        let args = match self.args {
            None => Vec::new(),
            Some(Yaml::Sequence(values)) => values
                .iter()
                .map(|value| scalar(value, "filters.args"))
                .collect::<Result<_, _>>()?,
            Some(value) => vec![scalar(&value, "filters.args")?],
        };
        Ok(RawFilter {
            name: self.name,
            args,
        })
    }
}

/// Escalar YAML em texto; `1.0` vira `1`, como no motor de referência.
fn scalar(value: &Yaml, section: &'static str) -> Result<String, IndexerError> {
    match value {
        Yaml::String(value) => Ok(value.clone()),
        Yaml::Bool(value) => Ok(value.to_string()),
        Yaml::Number(number) => Ok(number
            .as_f64()
            .filter(|float| float.fract() == 0.0 && number.is_f64())
            .map_or_else(|| number.to_string(), |float| format!("{float:.0}"))),
        _ => Err(invalid(section, "esperado texto, número ou booleano")),
    }
}

fn pairs(mapping: Mapping, section: &'static str) -> Result<Vec<(String, String)>, IndexerError> {
    mapping
        .into_iter()
        .map(|(key, value)| Ok((scalar(&key, section)?, scalar(&value, section)?)))
        .collect()
}

// ---------------------------------------------------------------------------
// Forma compilada.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SettingKind {
    Text,
    Password,
    Checkbox,
    Select(BTreeSet<String>),
}

#[derive(Debug)]
pub(super) struct Setting {
    pub name: String,
    pub label: String,
    pub kind: SettingKind,
    pub default: Option<String>,
}

/// Formato da resposta de busca. Uma definição não mistura os dois: os campos
/// são compilados para um só.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Html,
    Json,
}

pub(super) type Inputs = Vec<(String, Template)>;

/// Chave de input cujo valor são pares `a=1&b=2` prontos.
pub(super) const RAW_INPUT: &str = "$raw";
pub(super) type Headers = Vec<(String, Template)>;

pub(super) enum LoginMethod {
    /// O cookie colado nas settings vai em todo request.
    Cookie(Template),
    /// POST de formulário direto no caminho; a sessão fica no cookie jar.
    Post { path: Template, inputs: Inputs },
    /// GET com os inputs na query — o login por chave de API.
    Get { path: Template, inputs: Inputs },
    /// GET em que o valor de `oneurl` é colado ao caminho.
    OneUrl { path: Template, input: Template },
    /// Abre a página, lê o formulário e o envia.
    Form(Box<FormLogin>),
}

pub(super) struct FormLogin {
    pub path: Template,
    pub form: Css,
    pub submit_path: Option<String>,
    pub inputs: Vec<LoginInput>,
    /// Campos lidos da página de login com seletor (tokens CSRF, por exemplo).
    pub selector_inputs: Vec<(String, Field)>,
    /// Os mesmos, mas vão na query do envio.
    pub get_selector_inputs: Vec<(String, Field)>,
    /// Elemento que denuncia captcha de imagem na página de login.
    pub captcha: Option<Css>,
}

pub(super) struct LoginInput {
    pub key: String,
    /// Com `login.selectors`, a chave é um seletor e o nome do campo vem do
    /// atributo `name` do elemento achado.
    pub key_css: Option<Css>,
    pub value: Template,
}

pub(super) struct Login {
    pub method: LoginMethod,
    pub errors: Vec<Css>,
    pub test: Option<LoginTest>,
    /// `nome=valor` que vão junto da abertura do login.
    pub cookies: Vec<String>,
    pub headers: Option<Headers>,
}

pub(super) struct LoginTest {
    pub path: String,
    pub selector: Option<Css>,
}

pub(super) struct SearchPath {
    pub path: Template,
    pub post: bool,
    pub inputs: Inputs,
    pub categories: Vec<String>,
    pub inherit_inputs: bool,
    pub follow_redirect: bool,
    /// `response.noResultsMessage`: texto que, na resposta, quer dizer "nada".
    pub no_results: Option<String>,
}

pub(super) enum Source {
    Text(Template),
    Select {
        selector: Option<Selection>,
        attribute: Option<String>,
        remove: Option<Css>,
        case: Vec<(Css, Template)>,
    },
    Json {
        selector: Option<Template>,
        /// Seletor que começa com `..` lê da linha, não do filho de `multiple`.
        from_row: bool,
        case: Vec<(String, Template)>,
    },
}

/// Seletor CSS de um campo: fixo, ou com template (`a[{{ if ... }}href^=x{{ end }}]`)
/// e então compilado a cada linha.
pub(super) enum Selection {
    Fixed(Css),
    Templated(Template),
}

impl Selection {
    /// O seletor com as variáveis da linha; `None` se o resultado não compila.
    pub fn resolve<'a>(&'a self, vars: &Vars, scratch: &'a mut Option<Css>) -> Option<&'a Css> {
        match self {
            Self::Fixed(css) => Some(css),
            Self::Templated(template) => {
                *scratch = Css::parse(&template.render(vars), "search.fields.selector").ok();
                scratch.as_ref()
            }
        }
    }
}

pub(super) struct Field {
    pub source: Source,
    pub optional: bool,
    /// `campo|append`: o valor se junta ao que o campo já tinha (título,
    /// descrição) — em categoria, soma, que já é o padrão.
    pub append: bool,
    /// `campo|noappend`: em categoria, troca em vez de somar.
    pub no_append: bool,
    pub default: Option<Template>,
    pub filters: Vec<Filter>,
}

pub(super) enum Rows {
    Fixed(Css),
    Templated(Template),
    Json(Box<JsonRows>),
}

pub(super) struct JsonRows {
    pub selector: Template,
    pub attribute: Option<String>,
    pub multiple: bool,
    pub missing_is_empty: bool,
    pub count: Option<Field>,
}

/// Seletor de um bloco `download` (`selectors`, `infohash`, `pathselector`).
pub(super) struct DownloadSelector {
    pub selector: Template,
    pub attribute: Option<String>,
    pub use_before_response: bool,
    pub filters: Vec<Filter>,
}

pub(super) struct Before {
    pub path: Option<Template>,
    pub post: bool,
    pub inputs: Inputs,
    pub path_selector: Option<DownloadSelector>,
}

pub(super) struct Infohash {
    pub hash: DownloadSelector,
    pub title: DownloadSelector,
    pub use_before_response: bool,
}

/// Como chegar do link do release ao `.torrent` (ou ao magnet).
pub(super) struct Download {
    pub post: bool,
    pub before: Option<Before>,
    pub infohash: Option<Infohash>,
    pub selectors: Vec<DownloadSelector>,
    pub headers: Option<Headers>,
}

/// Campos que a busca exige para montar um release.
/// Intervalo entre requisições, em segundos, quando a definição não declara
/// `requestDelay`. Tracker privado proíbe automação abusiva: sem declaração
/// explícita, o ritmo é o conservador.
const DEFAULT_REQUEST_DELAY: f64 = 5.0;

const REQUIRED_FIELDS: [&str; 2] = ["title", "size"];

impl Document {
    /// Metadados: devolve se é privado e o intervalo entre requisições
    /// (`DEFAULT_REQUEST_DELAY` quando a definição não declara).
    fn validate_metadata(&self) -> Result<(bool, f64, Charset), IndexerError> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid("id", "use letras minúsculas, números e hífens"));
        }
        if self.name.trim().is_empty() || self.language.trim().is_empty() {
            return Err(invalid("metadados", "name e language são obrigatórios"));
        }
        let private = match self.kind.as_str() {
            "public" => false,
            "private" | "semi-private" => true,
            _ => return Err(invalid("type", "esperado public, private ou semi-private")),
        };
        let charset = Charset::from_label(&self.encoding).ok_or_else(|| {
            invalid(
                "encoding",
                "codificação não suportada (UTF-8, Latin-1/2 e Windows 125x/874 sim; de vários bytes não)",
            )
        })?;
        let delay = self.request_delay.unwrap_or(DEFAULT_REQUEST_DELAY);
        if !delay.is_finite() || !(0.0..=3600.0).contains(&delay) {
            return Err(invalid("requestDelay", "esperado entre 0 e 3600 segundos"));
        }
        Ok((private, delay, charset))
    }

    // Leitura em sequência, seção por seção; dividir só espalharia.
    #[allow(clippy::too_many_lines)]
    pub fn compile(self) -> Result<CardigannDefinition, IndexerError> {
        let (private, delay, charset) = self.validate_metadata()?;
        let links = self
            .links
            .iter()
            .map(|link| base_url(link))
            .collect::<Result<Vec<_>, _>>()?;
        if links.is_empty() {
            return Err(invalid("links", "ao menos um link é obrigatório"));
        }
        let _ = (
            self.legacylinks,
            self.replaces,
            self.certificates,
            self.caps.allowrawsearch,
            self.caps.allowtvsearchimdb,
        );

        let (categories, mappings) = compile_categories(&self.caps)?;
        let settings = compile_settings(self.settings)?;
        let setting_names: Vec<String> = settings
            .iter()
            .map(|setting| setting.name.clone())
            .collect();

        let field_names: Vec<String> = self
            .search
            .fields
            .keys()
            .map(|key| {
                let key = scalar(key, "search.fields")?;
                Ok(key.split('|').next().unwrap_or_default().to_owned())
            })
            .collect::<Result<_, IndexerError>>()?;
        let names = Names {
            settings: &setting_names,
            fields: &field_names,
            charset,
        };

        let format = detect_format(self.search.paths.as_deref())?;
        let login = self
            .login
            .map(|login| compile_login(login, &names, &links))
            .transpose()?;
        if private && login.is_none() {
            return Err(invalid("login", "indexador privado sem bloco de login"));
        }

        let search = self.search;
        let inputs = compile_inputs(search.inputs, &names, "search.inputs")?;
        let paths = compile_paths(
            search.path,
            search.paths,
            self.followredirect,
            &names,
            &links,
        )?;
        let keywords_filters = search
            .keywordsfilters
            .into_iter()
            .map(|raw| Filter::compile(raw.flatten()?, Scope::Request, &names))
            .collect::<Result<Vec<_>, _>>()?;
        let preprocessing_filters = search
            .preprocessingfilters
            .into_iter()
            .map(|raw| Filter::compile(raw.flatten()?, Scope::Request, &names))
            .collect::<Result<Vec<_>, _>>()?;
        if format == Format::Json && !preprocessing_filters.is_empty() {
            return Err(invalid(
                "search.preprocessingfilters",
                "preprocessingfilters só valem para resposta HTML",
            ));
        }
        let headers = compile_headers(search.headers, Scope::Request, &names)?;
        let search_errors = search
            .error
            .into_iter()
            .map(|error| Css::parse(&error.selector, "search.error.selector"))
            .collect::<Result<Vec<_>, _>>()?;
        if format == Format::Json && !search_errors.is_empty() {
            return Err(invalid(
                "search.error",
                "search.error só vale para resposta HTML",
            ));
        }
        let rows = compile_rows(search.rows, &names, format)?;
        let fields = compile_fields(search.fields, &names, format)?;
        let download = self
            .download
            .map(|download| compile_download(download, &names))
            .transpose()?;

        let mut templates: Vec<&Template> = inputs.iter().map(|(_, template)| template).collect();
        templates.extend(paths.iter().flat_map(|path| {
            path.inputs
                .iter()
                .map(|(_, template)| template)
                .chain(std::iter::once(&path.path))
        }));
        templates.extend(headers.iter().map(|(_, template)| template));
        match &rows.rows {
            Rows::Templated(template) => templates.push(template),
            Rows::Json(json) => templates.push(&json.selector),
            Rows::Fixed(_) => {}
        }
        let consumed: BTreeSet<String> = templates
            .into_iter()
            .flat_map(Template::variables)
            .map(str::to_owned)
            .collect();
        let capabilities = compile_modes(&self.caps.modes, categories, &consumed)?;

        Ok(CardigannDefinition {
            id: self.id,
            name: self.name,
            description: self.description,
            language: self.language,
            private,
            links,
            delay: Duration::from_secs_f64(delay),
            capabilities,
            charset,
            mappings: mappings.by_id,
            description_mappings: mappings.by_description,
            default_categories: mappings.defaults,
            settings,
            login,
            paths,
            inputs,
            allow_empty_inputs: search.allow_empty_inputs,
            keywords_filters,
            preprocessing_filters,
            headers,
            format,
            search_errors,
            rows: rows.rows,
            and_match: rows.and_match,
            after: rows.after,
            date_headers: rows.date_headers,
            fields,
            download,
            test_link_torrent: self.testlinktorrent,
        })
    }
}

/// O formato único das rotas de busca; `search.path` sozinho é HTML.
fn detect_format(paths: Option<&[RawPath]>) -> Result<Format, IndexerError> {
    let mut found: Option<Format> = None;
    for raw in paths.unwrap_or_default() {
        let format = match raw.response.as_ref().map(|response| response.kind.as_str()) {
            None | Some("html") => Format::Html,
            Some("json") => Format::Json,
            Some("xml") => {
                return Err(invalid(
                    "search.paths.response",
                    "resposta XML não é suportada (somente html e json)",
                ));
            }
            Some(_) => {
                return Err(invalid(
                    "search.paths.response",
                    "tipo de resposta desconhecido (esperado html ou json)",
                ));
            }
        };
        if found.is_some_and(|known| known != format) {
            return Err(invalid(
                "search.paths.response",
                "rotas com respostas html e json na mesma definição",
            ));
        }
        found = Some(format);
    }
    Ok(found.unwrap_or(Format::Html))
}

/// Inputs na ordem declarada. A chave `$raw` (pares `a=1&b=2` já prontos)
/// fica onde foi escrita: a ordem dos parâmetros na URL segue a da definição.
fn compile_inputs(
    raw: Mapping,
    names: &Names<'_>,
    section: &'static str,
) -> Result<Inputs, IndexerError> {
    pairs(raw, section)?
        .into_iter()
        .map(|(key, value)| {
            if key != RAW_INPUT {
                check_input_key(&key)?;
            }
            Ok((key, Template::compile(&value, Scope::Request, names)?))
        })
        .collect()
}

fn compile_paths(
    path: Option<String>,
    paths: Option<Vec<RawPath>>,
    follow_redirect: bool,
    names: &Names<'_>,
    links: &[Url],
) -> Result<Vec<SearchPath>, IndexerError> {
    let raw_paths = match (path, paths) {
        (Some(path), None) => vec![RawPath {
            path,
            method: None,
            inputs: Mapping::new(),
            categories: Vec::new(),
            followredirect: None,
            inheritinputs: None,
            response: None,
        }],
        (None, Some(paths)) if !paths.is_empty() => paths,
        _ => {
            return Err(invalid(
                "search.paths",
                "declare path ou uma lista em paths",
            ));
        }
    };
    let mut compiled = Vec::new();
    for raw in raw_paths {
        let post = match raw
            .method
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("get") => false,
            Some("post") => true,
            Some(_) => {
                return Err(invalid(
                    "search.paths.method",
                    "método desconhecido (esperado get ou post)",
                ));
            }
        };
        let template = Template::compile(&raw.path, Scope::Request, names)?;
        check_templated_path(&template, links, "search.paths.path")?;
        let inputs = compile_inputs(raw.inputs, names, "search.paths.inputs")?;
        compiled.push(SearchPath {
            path: template,
            post,
            inputs,
            categories: raw
                .categories
                .iter()
                .map(|value| scalar(value, "search.paths.categories"))
                .collect::<Result<_, _>>()?,
            inherit_inputs: raw.inheritinputs.unwrap_or(true),
            follow_redirect: raw.followredirect.unwrap_or(follow_redirect),
            no_results: raw
                .response
                .and_then(|response| response.no_results_message),
        });
    }
    Ok(compiled)
}

/// Cabeçalhos de uma seção; o valor é templado e, como na referência, só o
/// primeiro de uma lista vale.
fn compile_headers(
    raw: Option<BTreeMap<String, Yaml>>,
    scope: Scope,
    names: &Names<'_>,
) -> Result<Headers, IndexerError> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|(name, value)| {
            if HeaderName::from_bytes(name.as_bytes()).is_err() {
                return Err(invalid("headers", "nome de cabeçalho inválido"));
            }
            let value = match value {
                Yaml::Sequence(values) if !values.is_empty() => scalar(&values[0], "headers")?,
                other => scalar(&other, "headers")?,
            };
            Ok((name, Template::compile(&value, scope, names)?))
        })
        .collect()
}

/// Linhas da busca e o que vale só para HTML (`after`, `dateheaders`).
struct RowsPlan {
    rows: Rows,
    and_match: bool,
    after: usize,
    date_headers: Option<Field>,
}

fn compile_rows(raw: RawRows, names: &Names<'_>, format: Format) -> Result<RowsPlan, IndexerError> {
    let mut and_match = false;
    for filter in raw.filters {
        match filter.name.as_str() {
            "andmatch" => and_match = true,
            // Diagnóstico da referência: não muda a linha.
            "strdump" => {}
            _ => {
                return Err(invalid(
                    "search.rows.filters",
                    "filtro de linha não implementado",
                ));
            }
        }
    }
    let template = Template::compile(&raw.selector, Scope::Request, names)?;
    if format == Format::Json {
        if raw.after.is_some() || raw.dateheaders.is_some() {
            return Err(invalid(
                "search.rows",
                "after e dateheaders só valem para resposta HTML",
            ));
        }
        // O caminho das linhas precisa ser legível já na carga.
        let probe = template.render(&Vars::default());
        if JsonPath::parse(path_part(&probe)).is_none() {
            return Err(invalid(
                "search.rows.selector",
                "caminho JSON fora do subconjunto suportado",
            ));
        }
        if let Some(attribute) = &raw.attribute
            && JsonPath::parse(attribute).is_none()
        {
            return Err(invalid(
                "search.rows.attribute",
                "caminho JSON fora do subconjunto suportado",
            ));
        }
        let count = raw
            .count
            .map(|count| compile_field(count, names, format))
            .transpose()?;
        return Ok(RowsPlan {
            rows: Rows::Json(Box::new(JsonRows {
                selector: template,
                attribute: raw.attribute,
                multiple: raw.multiple,
                missing_is_empty: raw.missing_attribute_equals_no_results,
                count,
            })),
            and_match,
            after: 0,
            date_headers: None,
        });
    }
    if raw.count.is_some()
        || raw.multiple
        || raw.attribute.is_some()
        || raw.missing_attribute_equals_no_results
    {
        return Err(invalid(
            "search.rows",
            "count, multiple, attribute e missingAttributeEqualsNoResults só valem para JSON",
        ));
    }
    let rows = if let Some(literal) = template.literal() {
        Rows::Fixed(Css::parse(literal, "search.rows.selector")?)
    } else {
        // Valida já: renderizado sem variáveis, o seletor precisa compilar,
        // senão a primeira busca é que descobriria.
        Css::parse(&template.render(&Vars::default()), "search.rows.selector")?;
        Rows::Templated(template)
    };
    if raw.after.unwrap_or(0) > 0 && raw.dateheaders.is_some() {
        return Err(invalid(
            "search.rows",
            "after e dateheaders juntos não são suportados",
        ));
    }
    let date_headers = raw
        .dateheaders
        .map(|headers| compile_field(headers, names, format))
        .transpose()?;
    Ok(RowsPlan {
        rows,
        and_match,
        after: raw.after.unwrap_or(0),
        date_headers,
    })
}

fn compile_fields(
    raw: Mapping,
    names: &Names<'_>,
    format: Format,
) -> Result<Vec<(String, Field)>, IndexerError> {
    let mut fields = Vec::new();
    for (key, value) in raw {
        let key = scalar(&key, "search.fields")?;
        let mut modifiers = key.split('|');
        let name = modifiers.next().unwrap_or_default().to_owned();
        let raw: RawField = serde_yaml_ng::from_value(value)
            .map_err(|_| invalid("search.fields", "campo com chave ou tipo não suportado"))?;
        let mut field = compile_field(raw, names, format)?;
        for modifier in modifiers {
            let accumulates = matches!(
                name.as_str(),
                "title" | "description" | "category" | "categorydesc"
            );
            match modifier {
                "optional" => field.optional = true,
                "append" if accumulates => field.append = true,
                "noappend" if name.starts_with("category") => field.no_append = true,
                _ => {
                    return Err(invalid(
                        "search.fields",
                        "modificador de campo não suportado (aceitos: optional, append, noappend)",
                    ));
                }
            }
        }
        fields.push((name, field));
    }
    for required in REQUIRED_FIELDS {
        if !fields.iter().any(|(name, _)| name == required) {
            return Err(invalid("search.fields", "title e size são obrigatórios"));
        }
    }
    if !fields
        .iter()
        .any(|(name, _)| matches!(name.as_str(), "download" | "magnet" | "infohash"))
    {
        return Err(invalid(
            "search.fields",
            "download, magnet ou infohash é obrigatório",
        ));
    }
    Ok(fields)
}

fn check_input_key(key: &str) -> Result<(), IndexerError> {
    if key.is_empty()
        || !key.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'[' | b']')
        })
    {
        return Err(invalid("search.inputs", "chave de input inválida"));
    }
    Ok(())
}

fn compile_settings(raw: Vec<RawSetting>) -> Result<Vec<Setting>, IndexerError> {
    let mut settings: Vec<Setting> = Vec::new();
    for setting in raw {
        // `info`, `info_cookie`, `info_flaresolverr`...: texto de ajuda da
        // interface da referência. Não vira variável.
        if setting.kind == "info" || setting.kind.starts_with("info_") {
            continue;
        }
        let kind = match setting.kind.as_str() {
            "text" => SettingKind::Text,
            "password" => SettingKind::Password,
            "checkbox" => SettingKind::Checkbox,
            "select" => {
                let options = setting
                    .options
                    .as_ref()
                    .ok_or_else(|| invalid("settings.options", "select sem options"))?
                    .keys()
                    .map(|key| scalar(key, "settings.options"))
                    .collect::<Result<BTreeSet<_>, _>>()?;
                if options.is_empty() {
                    return Err(invalid("settings.options", "select sem options"));
                }
                SettingKind::Select(options)
            }
            _ => {
                return Err(invalid(
                    "settings.type",
                    "tipo de setting não suportado (multi-select, ...)",
                ));
            }
        };
        if setting.name.is_empty()
            || !setting
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid("settings", "nome inválido"));
        }
        let label = setting
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| setting.name.clone());
        let mut default = setting
            .default
            .as_ref()
            .map(|value| scalar(value, "settings.default"))
            .transpose()?;
        // Default de `select` que não é uma das opções é erro da definição, e a
        // referência também não o consegue usar (sem opção escolhida, a
        // busca nem monta): fica sem default, e o operador precisa escolher.
        if matches!(&kind, SettingKind::Select(options) if default.as_ref().is_some_and(|value| !options.contains(value)))
        {
            default = None;
        }
        if let Some(value) = &default {
            validate_setting(&kind, value)?;
        }
        if settings.iter().any(|known| known.name == setting.name) {
            return Err(invalid("settings", "nome duplicado"));
        }
        settings.push(Setting {
            name: setting.name,
            label,
            kind,
            default,
        });
    }
    Ok(settings)
}

pub(super) fn validate_setting(kind: &SettingKind, value: &str) -> Result<(), IndexerError> {
    match kind {
        SettingKind::Checkbox if !matches!(value, "true" | "false") => {
            Err(invalid("settings", "checkbox exige true ou false"))
        }
        SettingKind::Select(options) if !options.contains(value) => {
            Err(invalid("settings", "valor fora das opções do select"))
        }
        _ => Ok(()),
    }
}

// Um método de login por braço; dividir só espalharia.
#[allow(clippy::too_many_lines)]
fn compile_login(raw: RawLogin, names: &Names<'_>, links: &[Url]) -> Result<Login, IndexerError> {
    let method = raw.method.as_deref().unwrap_or("post").to_ascii_lowercase();
    let inputs = pairs(raw.inputs, "login.inputs")?;
    let template = |value: &str| Template::compile(value, Scope::Request, names);
    let path = |path: Option<String>| -> Result<Template, IndexerError> {
        let path = path.ok_or_else(|| invalid("login.path", "o login exige path"))?;
        let compiled = template(&path)?;
        check_templated_path(&compiled, links, "login.path")?;
        Ok(compiled)
    };
    let templated = |inputs: Vec<(String, String)>| -> Result<Inputs, IndexerError> {
        inputs
            .into_iter()
            .map(|(key, value)| Ok((key, template(&value)?)))
            .collect()
    };
    let method = match method.as_str() {
        "cookie" => {
            let [(key, value)] = inputs.as_slice() else {
                return Err(invalid(
                    "login.inputs",
                    "login por cookie espera só o input cookie",
                ));
            };
            if key != "cookie" {
                return Err(invalid(
                    "login.inputs",
                    "login por cookie espera só o input cookie",
                ));
            }
            LoginMethod::Cookie(template(value)?)
        }
        "post" => LoginMethod::Post {
            path: path(raw.path)?,
            inputs: templated(inputs)?,
        },
        "get" => LoginMethod::Get {
            path: path(raw.path)?,
            inputs: templated(inputs)?,
        },
        "oneurl" => {
            let [(key, value)] = inputs.as_slice() else {
                return Err(invalid(
                    "login.inputs",
                    "login oneurl espera só o input oneurl",
                ));
            };
            if key != "oneurl" {
                return Err(invalid(
                    "login.inputs",
                    "login oneurl espera só o input oneurl",
                ));
            }
            LoginMethod::OneUrl {
                path: path(raw.path)?,
                input: template(value)?,
            }
        }
        "form" => {
            let form = Css::parse(raw.form.as_deref().unwrap_or("form"), "login.form")?;
            let inputs = inputs
                .into_iter()
                .map(|(key, value)| {
                    Ok(LoginInput {
                        key_css: raw
                            .selectors
                            .then(|| Css::parse(&key, "login.inputs"))
                            .transpose()?,
                        key,
                        value: template(&value)?,
                    })
                })
                .collect::<Result<_, IndexerError>>()?;
            let captcha = match raw.captcha {
                None => None,
                Some(captcha) if captcha.kind == "image" => {
                    let _ = captcha.input;
                    let selector = captcha.selector.ok_or_else(|| {
                        invalid("login.captcha", "captcha de imagem exige selector")
                    })?;
                    Some(Css::parse(&selector, "login.captcha.selector")?)
                }
                Some(_) => {
                    return Err(invalid(
                        "login.captcha",
                        "captcha que não seja de imagem não é suportado",
                    ));
                }
            };
            if let Some(submit) = &raw.submitpath {
                check_static_path(submit, links, "login.submitpath")?;
            }
            LoginMethod::Form(Box::new(FormLogin {
                path: path(raw.path)?,
                form,
                submit_path: raw.submitpath,
                inputs,
                selector_inputs: compile_selector_inputs(
                    raw.selectorinputs,
                    names,
                    "login.selectorinputs",
                )?,
                get_selector_inputs: compile_selector_inputs(
                    raw.getselectorinputs,
                    names,
                    "login.getselectorinputs",
                )?,
                captcha,
            }))
        }
        _ => {
            return Err(invalid(
                "login.method",
                "método desconhecido (suportados: cookie, post, get, oneurl e form)",
            ));
        }
    };
    let errors = raw
        .error
        .into_iter()
        .map(|error| {
            let _ = (error.message, error.path);
            Css::parse(&error.selector, "login.error.selector")
        })
        .collect::<Result<_, _>>()?;
    let test = raw
        .test
        .map(|test| {
            check_static_path(&test.path, links, "login.test.path")?;
            Ok::<_, IndexerError>(LoginTest {
                path: test.path,
                selector: test
                    .selector
                    .map(|selector| Css::parse(&selector, "login.test.selector"))
                    .transpose()?,
            })
        })
        .transpose()?;
    if raw
        .cookies
        .iter()
        .any(|cookie| !cookie.contains('=') || cookie.contains(['\r', '\n']))
    {
        return Err(invalid("login.cookies", "esperado `nome=valor`"));
    }
    let headers = raw
        .headers
        .map(|headers| compile_headers(Some(headers), Scope::Request, names))
        .transpose()?;
    Ok(Login {
        method,
        errors,
        test,
        cookies: raw.cookies,
        headers,
    })
}

fn compile_selector_inputs(
    raw: Mapping,
    names: &Names<'_>,
    section: &'static str,
) -> Result<Vec<(String, Field)>, IndexerError> {
    raw.into_iter()
        .map(|(key, value)| {
            let raw: RawField = serde_yaml_ng::from_value(value)
                .map_err(|_| invalid(section, "campo com chave ou tipo não suportado"))?;
            Ok((
                scalar(&key, section)?,
                compile_field(raw, names, Format::Html)?,
            ))
        })
        .collect()
}

/// Seletor do bloco `download`: o texto é um template (`.DownloadUri.*`) e o
/// CSS só se confere por inteiro depois de renderizado; sendo literal, confere
/// já.
fn compile_download_selector(
    raw: RawDownloadSelector,
    names: &Names<'_>,
) -> Result<DownloadSelector, IndexerError> {
    let selector = Template::compile(&raw.selector, Scope::Download, names)?;
    if let Some(literal) = selector.literal() {
        Css::parse(literal, "download.selector")?;
    }
    Ok(DownloadSelector {
        selector,
        attribute: raw.attribute,
        use_before_response: raw.usebeforeresponse,
        filters: raw
            .filters
            .into_iter()
            .map(|filter| Filter::compile(filter.flatten()?, Scope::Download, names))
            .collect::<Result<_, _>>()?,
    })
}

fn compile_download(raw: RawDownload, names: &Names<'_>) -> Result<Download, IndexerError> {
    let post = match raw
        .method
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("get") => false,
        Some("post") => true,
        Some(_) => {
            return Err(invalid(
                "download.method",
                "método desconhecido (esperado get ou post)",
            ));
        }
    };
    let before = raw
        .before
        .map(|before| {
            let post = match before
                .method
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                None | Some("get") => false,
                Some("post") => true,
                Some(_) => {
                    return Err(invalid(
                        "download.before.method",
                        "método desconhecido (esperado get ou post)",
                    ));
                }
            };
            if before.path.is_none() && before.pathselector.is_none() {
                return Err(invalid("download.before", "declare path ou pathselector"));
            }
            let inputs = pairs(before.inputs, "download.before.inputs")?
                .into_iter()
                .map(|(key, value)| Ok((key, Template::compile(&value, Scope::Download, names)?)))
                .collect::<Result<_, IndexerError>>()?;
            Ok(Before {
                path: before
                    .path
                    .map(|path| Template::compile(&path, Scope::Download, names))
                    .transpose()?,
                post,
                inputs,
                path_selector: before
                    .pathselector
                    .map(|selector| compile_download_selector(selector, names))
                    .transpose()?,
            })
        })
        .transpose()?;
    let infohash = raw
        .infohash
        .map(|infohash| {
            Ok::<_, IndexerError>(Infohash {
                hash: compile_download_selector(infohash.hash, names)?,
                title: compile_download_selector(infohash.title, names)?,
                use_before_response: infohash.usebeforeresponse,
            })
        })
        .transpose()?;
    let selectors = raw
        .selectors
        .into_iter()
        .map(|selector| compile_download_selector(selector, names))
        .collect::<Result<Vec<_>, _>>()?;
    let headers = raw
        .headers
        .map(|headers| compile_headers(Some(headers), Scope::Download, names))
        .transpose()?;
    Ok(Download {
        post,
        before,
        infohash,
        selectors,
        headers,
    })
}

// Um braço por tipo de fonte (texto, HTML, JSON); dividir só espalharia.
#[allow(clippy::too_many_lines)]
fn compile_field(raw: RawField, names: &Names<'_>, format: Format) -> Result<Field, IndexerError> {
    let template = |value: &str| Template::compile(value, Scope::Field, names);
    let source = match (raw.text, raw.selector) {
        (Some(text), None) => {
            if raw.attribute.is_some() || raw.remove.is_some() || raw.case.is_some() {
                return Err(invalid(
                    "search.fields",
                    "text não combina com attribute, remove ou case",
                ));
            }
            Source::Text(template(&scalar(&text, "search.fields.text")?)?)
        }
        (None, selector) if format == Format::Json => {
            if raw.attribute.is_some() || raw.remove.is_some() {
                return Err(invalid(
                    "search.fields",
                    "attribute e remove não valem em resposta JSON",
                ));
            }
            let case = raw
                .case
                .unwrap_or_default()
                .into_iter()
                .map(|(key, value)| {
                    // `True` e `False` são booleanos para o YAML, e o JSON
                    // chega ao `case` como o texto .NET deles.
                    let key = match key {
                        Yaml::Bool(true) => "True".to_owned(),
                        Yaml::Bool(false) => "False".to_owned(),
                        other => scalar(&other, "search.fields.case")?,
                    };
                    Ok((key, template(&scalar(&value, "search.fields.case")?)?))
                })
                .collect::<Result<Vec<_>, IndexerError>>()?;
            let from_row = selector.as_deref().is_some_and(|s| s.starts_with(".."));
            let selector = selector
                .map(|selector| {
                    let compiled = template(selector.trim_start_matches('.'))?;
                    // Seletor literal precisa ser legível já na carga.
                    if let Some(literal) = compiled.literal()
                        && !path_part(literal).trim().is_empty()
                        && JsonPath::parse(path_part(literal)).is_none()
                    {
                        return Err(invalid(
                            "search.fields.selector",
                            "caminho JSON fora do subconjunto suportado",
                        ));
                    }
                    Ok(compiled)
                })
                .transpose()?;
            Source::Json {
                selector,
                from_row,
                case,
            }
        }
        // Sem selector, o campo lê a própria linha, como na referência.
        (None, selector) => {
            let case = raw
                .case
                .unwrap_or_default()
                .into_iter()
                .map(|(key, value)| {
                    Ok((
                        Css::parse(&scalar(&key, "search.fields.case")?, "search.fields.case")?,
                        template(&scalar(&value, "search.fields.case")?)?,
                    ))
                })
                .collect::<Result<Vec<_>, IndexerError>>()?;
            Source::Select {
                selector: selector
                    .map(|selector| {
                        let compiled = template(&selector)?;
                        match compiled.literal() {
                            Some(literal) => {
                                Css::parse(literal, "search.fields.selector").map(Selection::Fixed)
                            }
                            None => {
                                // Só dá para conferir em cada linha, com as
                                // variáveis dela; seletor que não compila
                                // deixa o campo vazio, como na referência.
                                Ok(Selection::Templated(compiled))
                            }
                        }
                    })
                    .transpose()?,
                attribute: raw.attribute,
                remove: raw
                    .remove
                    .map(|remove| Css::parse(&remove, "search.fields.remove"))
                    .transpose()?,
                case,
            }
        }
        (Some(_), Some(_)) => {
            return Err(invalid(
                "search.fields",
                "use selector ou text, não os dois",
            ));
        }
    };
    Ok(Field {
        source,
        optional: raw.optional,
        append: false,
        no_append: false,
        default: raw
            .default
            .map(|value| template(&scalar(&value, "search.fields.default")?))
            .transpose()?,
        filters: raw
            .filters
            .into_iter()
            .map(|filter| Filter::compile(filter.flatten()?, Scope::Field, names))
            .collect::<Result<_, _>>()?,
    })
}

/// Categorias do tracker mapeadas para as do Newznab.
struct CategoryMaps {
    /// Por id do tracker.
    by_id: BTreeMap<String, Vec<u32>>,
    /// Por descrição (minúscula), que `categorydesc` consulta.
    by_description: BTreeMap<String, Vec<u32>>,
    /// Ids do tracker marcados `default`: valem quando a busca não pede categoria.
    defaults: Vec<String>,
}

fn compile_categories(caps: &RawCaps) -> Result<(Vec<Category>, CategoryMaps), IndexerError> {
    /// id do tracker, categoria Newznab, descrição, padrão.
    type Entry<'a> = (String, &'a str, Option<&'a str>, bool);
    let entries: Vec<Entry<'_>> = match (&caps.categories, &caps.categorymappings) {
        (Some(categories), None) if !categories.is_empty() => categories
            .iter()
            .map(|(id, name)| (id.clone(), name.as_str(), None, false))
            .collect(),
        (None, Some(mappings)) if !mappings.is_empty() => mappings
            .iter()
            .map(|entry| {
                Ok((
                    scalar(&entry.id, "caps.categorymappings")?,
                    entry.cat.as_str(),
                    entry.desc.as_deref(),
                    entry.default.unwrap_or(false),
                ))
            })
            .collect::<Result<_, IndexerError>>()?,
        _ => {
            return Err(invalid(
                "caps",
                "declare categories ou categorymappings não vazio",
            ));
        }
    };
    let mut maps = CategoryMaps {
        by_id: BTreeMap::new(),
        by_description: BTreeMap::new(),
        defaults: Vec::new(),
    };
    let mut categories = BTreeMap::new();
    for (tracker, name, description, default) in entries {
        if tracker.is_empty() {
            return Err(invalid("caps", "id de categoria vazio"));
        }
        let id =
            category_id(name).ok_or_else(|| invalid("caps", "categoria Newznab desconhecida"))?;
        if default && !maps.defaults.contains(&tracker) {
            maps.defaults.push(tracker.clone());
        }
        if let Some(description) = description.filter(|text| !text.trim().is_empty()) {
            maps.by_description
                .entry(description.to_lowercase())
                .or_default()
                .push(id);
        }
        maps.by_id.entry(tracker).or_default().push(id);
        categories.insert(
            id,
            Category {
                id,
                name: name.to_owned(),
                parent: (!id.is_multiple_of(1000)).then_some(id / 1000 * 1000),
            },
        );
    }
    for ids in maps
        .by_id
        .values_mut()
        .chain(maps.by_description.values_mut())
    {
        ids.sort_unstable();
        ids.dedup();
    }
    Ok((categories.into_values().collect(), maps))
}

/// Modos anunciados, reduzidos ao que algum template de fato consome.
///
/// Parâmetro anunciado e não consumido seria ignorado na busca — e busca por
/// ID que ignora o ID devolve qualquer coisa. Então ele sai das capacidades:
/// quem consulta não manda o que não será usado.
fn compile_modes(
    modes: &BTreeMap<String, Vec<String>>,
    categories: Vec<Category>,
    consumed: &BTreeSet<String>,
) -> Result<Capabilities, IndexerError> {
    if !modes.contains_key("search") {
        return Err(invalid("caps.modes", "search obrigatório"));
    }
    let uses = |candidates: &[&str]| candidates.iter().any(|name| consumed.contains(*name));
    let mut result = Capabilities {
        categories,
        ..Capabilities::default()
    };
    for (mode, params) in modes {
        let target = match mode.as_str() {
            "search" => &mut result.general,
            "tv-search" => &mut result.tv,
            "movie-search" => &mut result.movie,
            // Livro e música ficam fora desta superfície.
            _ => continue,
        };
        let keywords = [".Keywords", ".Query.Keywords"];
        let supported = params
            .iter()
            .filter(|param| match param.as_str() {
                "q" => uses(&[".Keywords", ".Query.Keywords", ".Query.Q"]),
                "season" => uses(&[keywords[0], keywords[1], ".Query.Season", ".Query.Episode"]),
                "ep" => uses(&[keywords[0], keywords[1], ".Query.Ep", ".Query.Episode"]),
                "year" => uses(&[keywords[0], keywords[1], ".Query.Year"]),
                "imdbid" => uses(&[".Query.IMDBID", ".Query.IMDBIDShort"]),
                "tvdbid" => uses(&[".Query.TVDBID"]),
                "tmdbid" => uses(&[".Query.TMDBID"]),
                _ => false,
            })
            .cloned()
            .collect();
        *target = SearchSupport {
            available: true,
            supported_params: supported,
        };
    }
    Ok(result)
}

pub(super) fn base_url(value: &str) -> Result<Url, IndexerError> {
    let url = Url::parse(value).map_err(|_| invalid("links", "URL inválida"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "links",
            "use HTTP/HTTPS sem credenciais, query ou fragmento",
        ));
    }
    if !url.path().ends_with('/') {
        return Err(invalid("links", "o link base deve terminar em /"));
    }
    Ok(url)
}

/// Caminho estático que não troca de origem.
fn check_static_path(path: &str, links: &[Url], section: &'static str) -> Result<(), IndexerError> {
    if path.contains(['{', '}']) {
        return Err(invalid(
            section,
            "templates no caminho não são suportados; use inputs",
        ));
    }
    for link in links {
        same_origin(link, path, section)?;
    }
    Ok(())
}

/// Caminho com templates: o que dá para conferir sem a busca é conferido
/// renderizado sem variáveis; o resultado real se confere de novo a cada
/// requisição, em `same_origin`.
fn check_templated_path(
    path: &Template,
    links: &[Url],
    section: &'static str,
) -> Result<(), IndexerError> {
    let probe = path.render(&Vars::default());
    for link in links {
        same_origin(link, &probe, section)?;
    }
    Ok(())
}

/// Junta `path` ao link base e recusa o que sair da origem.
pub(super) fn same_origin(
    base: &Url,
    path: &str,
    section: &'static str,
) -> Result<Url, IndexerError> {
    let url = base
        .join(path)
        .map_err(|_| invalid(section, "URL inválida"))?;
    if url.origin() != base.origin()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid(
            section,
            "a requisição precisa permanecer na origem do link base",
        ));
    }
    Ok(url)
}

/// Tabela de categorias Newznab.
fn category_id(name: &str) -> Option<u32> {
    Some(match name {
        "Console" => 1000,
        "Console/NDS" => 1010,
        "Console/PSP" => 1020,
        "Console/Wii" => 1030,
        "Console/XBox" => 1040,
        "Console/XBox 360" => 1050,
        "Console/Wiiware" => 1060,
        "Console/XBox 360 DLC" => 1070,
        "Console/PS3" => 1080,
        "Console/Other" => 1090,
        "Console/3DS" => 1110,
        "Console/PS Vita" => 1120,
        "Console/WiiU" => 1130,
        "Console/XBox One" => 1140,
        "Console/PS4" => 1180,
        "Movies" => 2000,
        "Movies/Foreign" => 2010,
        "Movies/Other" => 2020,
        "Movies/SD" => 2030,
        "Movies/HD" => 2040,
        "Movies/UHD" => 2045,
        "Movies/BluRay" => 2050,
        "Movies/3D" => 2060,
        "Movies/DVD" => 2070,
        "Movies/WEB-DL" => 2080,
        "Audio" => 3000,
        "Audio/MP3" => 3010,
        "Audio/Video" => 3020,
        "Audio/Audiobook" => 3030,
        "Audio/Lossless" => 3040,
        "Audio/Other" => 3050,
        "Audio/Foreign" => 3060,
        "PC" => 4000,
        "PC/0day" => 4010,
        "PC/ISO" => 4020,
        "PC/Mac" => 4030,
        "PC/Mobile-Other" => 4040,
        "PC/Games" => 4050,
        "PC/Mobile-iOS" => 4060,
        "PC/Mobile-Android" => 4070,
        "TV" => 5000,
        "TV/WEB-DL" => 5010,
        "TV/Foreign" => 5020,
        "TV/SD" => 5030,
        "TV/HD" => 5040,
        "TV/UHD" => 5045,
        "TV/Other" => 5050,
        "TV/Sport" => 5060,
        "TV/Anime" => 5070,
        "TV/Documentary" => 5080,
        "XXX" => 6000,
        "XXX/DVD" => 6010,
        "XXX/WMV" => 6020,
        "XXX/XviD" => 6030,
        "XXX/x264" => 6040,
        "XXX/UHD" => 6045,
        "XXX/Pack" => 6050,
        "XXX/ImageSet" => 6060,
        "XXX/Other" => 6070,
        "XXX/SD" => 6080,
        "XXX/WEB-DL" => 6090,
        "Books" => 7000,
        "Books/Mags" => 7010,
        "Books/EBook" => 7020,
        "Books/Comics" => 7030,
        "Books/Technical" => 7040,
        "Books/Other" => 7050,
        "Books/Foreign" => 7060,
        "Other" => 8000,
        "Other/Misc" => 8010,
        "Other/Hashed" => 8020,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/cardigann-public.yml");

    #[test]
    fn requestdelay_ausente_usa_cinco_segundos() {
        let yaml = FIXTURE.replace("requestDelay: 0\n", "");
        assert_ne!(yaml, FIXTURE, "a fixture declarava requestDelay");
        let definition = CardigannDefinition::from_yaml_v11(&yaml).unwrap();
        assert_eq!(definition.delay, Duration::from_secs(5));
    }

    #[test]
    fn requestdelay_declarado_prevalece() {
        let definition = CardigannDefinition::from_yaml_v11(FIXTURE).unwrap();
        assert_eq!(definition.delay, Duration::ZERO);
    }
}

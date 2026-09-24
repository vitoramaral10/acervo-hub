//! O YAML de uma definição e sua compilação.
//!
//! Tudo que a definição declara é validado aqui — seletor, regex, template,
//! rota —, para que um erro apareça na subida e não na primeira busca. Chave
//! desconhecida é recusada: um recurso ignorado em silêncio vira resultado
//! errado sem ninguém saber por quê.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Deserialize;
use serde_yaml_ng::{Mapping, Value as Yaml};
use url::Url;

use super::filters::Filter;
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
    // Metadados de teste e de TLS da referência: não mudam a busca.
    #[serde(default)]
    testlinktorrent: Option<Yaml>,
    #[serde(default)]
    certificates: Option<Yaml>,
    caps: RawCaps,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    login: Option<RawLogin>,
    search: RawSearch,
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
    method: Option<String>,
    #[serde(default)]
    inputs: Mapping,
    #[serde(default)]
    error: Vec<RawLoginError>,
    #[serde(default)]
    test: Option<RawLoginTest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoginError {
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
    keywordsfilters: Vec<RawFilterYaml>,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRows {
    selector: String,
    #[serde(default)]
    filters: Vec<RawFilterYaml>,
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

pub(super) enum LoginMethod {
    /// O cookie colado nas settings vai em todo request.
    Cookie(Template),
    /// POST ou GET de formulário; a sessão fica no cookie jar.
    Form {
        path: String,
        post: bool,
        inputs: Vec<(String, Template)>,
    },
}

pub(super) struct Login {
    pub method: LoginMethod,
    pub errors: Vec<Css>,
    pub test: Option<LoginTest>,
}

pub(super) struct LoginTest {
    pub path: String,
    pub selector: Option<Css>,
}

pub(super) struct SearchPath {
    pub path: String,
    pub inputs: Vec<(String, Template)>,
    pub categories: Vec<String>,
    pub follow_redirect: bool,
}

pub(super) enum Source {
    Text(Template),
    Select {
        selector: Option<Css>,
        attribute: Option<String>,
        remove: Option<Css>,
        case: Vec<(Css, Template)>,
    },
}

pub(super) struct Field {
    pub source: Source,
    pub optional: bool,
    pub default: Option<Template>,
    pub filters: Vec<Filter>,
}

pub(super) enum Rows {
    Fixed(Css),
    Templated(Template),
}

/// Campos que a busca exige para montar um release.
const REQUIRED_FIELDS: [&str; 2] = ["title", "size"];

/// Campos da referência cujo valor seria perdido em silêncio aqui.
const REFUSED_FIELDS: [&str; 1] = ["categorydesc"];

impl Document {
    /// Metadados: devolve se é privado e o intervalo entre requisições.
    fn validate_metadata(&self) -> Result<(bool, f64), IndexerError> {
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
        if !self.encoding.eq_ignore_ascii_case("utf-8") {
            return Err(invalid("encoding", "somente UTF-8 é suportado"));
        }
        let delay = self.request_delay.unwrap_or(2.0);
        if !delay.is_finite() || !(0.0..=3600.0).contains(&delay) {
            return Err(invalid("requestDelay", "esperado entre 0 e 3600 segundos"));
        }
        Ok((private, delay))
    }

    pub fn compile(self) -> Result<CardigannDefinition, IndexerError> {
        let (private, delay) = self.validate_metadata()?;
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
            self.testlinktorrent,
            self.certificates,
            self.caps.allowrawsearch,
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
            .map(|key| scalar(key, "search.fields"))
            .collect::<Result<_, _>>()?;
        let names = Names {
            settings: &setting_names,
            fields: &field_names,
        };

        let login = self
            .login
            .map(|login| compile_login(login, &names, &links))
            .transpose()?;
        if private && login.is_none() {
            return Err(invalid("login", "indexador privado sem bloco de login"));
        }

        let search = self.search;
        let (inputs, raw_inputs) = compile_inputs(search.inputs, &names)?;
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
        let headers = compile_headers(search.headers.unwrap_or_default())?;
        let (rows, and_match) = compile_rows(search.rows, &names)?;
        let fields = compile_fields(search.fields, &names)?;

        let mut templates: Vec<&Template> = inputs.iter().map(|(_, template)| template).collect();
        templates.extend(
            paths
                .iter()
                .flat_map(|path| path.inputs.iter().map(|(_, template)| template)),
        );
        templates.extend(raw_inputs.as_ref());
        if let Rows::Templated(template) = &rows {
            templates.push(template);
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
            mappings,
            settings,
            login,
            paths,
            inputs,
            raw_inputs,
            allow_empty_inputs: search.allow_empty_inputs,
            keywords_filters,
            headers,
            rows,
            and_match,
            fields,
        })
    }
}

type Inputs = Vec<(String, Template)>;

fn compile_inputs(
    raw: Mapping,
    names: &Names<'_>,
) -> Result<(Inputs, Option<Template>), IndexerError> {
    let mut inputs = Vec::new();
    let mut raw_inputs = None;
    for (key, value) in pairs(raw, "search.inputs")? {
        let template = Template::compile(&value, Scope::Request, names)?;
        if key == "$raw" {
            raw_inputs = Some(template);
        } else {
            check_input_key(&key)?;
            inputs.push((key, template));
        }
    }
    Ok((inputs, raw_inputs))
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
        if raw
            .method
            .as_deref()
            .is_some_and(|method| !method.eq_ignore_ascii_case("get"))
        {
            return Err(invalid("search.paths.method", "somente GET é suportado"));
        }
        if raw.inheritinputs == Some(false) {
            return Err(invalid(
                "search.paths.inheritinputs",
                "inheritinputs: false não é suportado",
            ));
        }
        check_static_path(&raw.path, links, "search.paths.path")?;
        let mut inputs = Vec::new();
        for (key, value) in pairs(raw.inputs, "search.paths.inputs")? {
            check_input_key(&key)?;
            inputs.push((key, Template::compile(&value, Scope::Request, names)?));
        }
        compiled.push(SearchPath {
            path: raw.path,
            inputs,
            categories: raw
                .categories
                .iter()
                .map(|value| scalar(value, "search.paths.categories"))
                .collect::<Result<_, _>>()?,
            follow_redirect: raw.followredirect.unwrap_or(follow_redirect),
        });
    }
    Ok(compiled)
}

fn compile_headers(raw: BTreeMap<String, Yaml>) -> Result<Vec<(String, String)>, IndexerError> {
    raw.into_iter()
        .map(|(name, value)| {
            let value = match value {
                Yaml::Sequence(values) if values.len() == 1 => {
                    scalar(&values[0], "search.headers")?
                }
                other => scalar(&other, "search.headers")?,
            };
            if value.contains("{{") {
                return Err(invalid(
                    "search.headers",
                    "templates em cabeçalho não são suportados",
                ));
            }
            Ok((name, value))
        })
        .collect()
}

fn compile_rows(raw: RawRows, names: &Names<'_>) -> Result<(Rows, bool), IndexerError> {
    let mut and_match = false;
    for filter in raw.filters {
        if filter.name != "andmatch" {
            return Err(invalid(
                "search.rows.filters",
                "filtro de linha não implementado",
            ));
        }
        and_match = true;
    }
    let template = Template::compile(&raw.selector, Scope::Request, names)?;
    let rows = if let Some(literal) = template.literal() {
        Rows::Fixed(Css::parse(literal, "search.rows.selector")?)
    } else {
        // Valida já: renderizado sem variáveis, o seletor precisa compilar,
        // senão a primeira busca é que descobriria.
        Css::parse(&template.render(&Vars::default()), "search.rows.selector")?;
        Rows::Templated(template)
    };
    Ok((rows, and_match))
}

fn compile_fields(raw: Mapping, names: &Names<'_>) -> Result<Vec<(String, Field)>, IndexerError> {
    let mut fields = Vec::new();
    for (key, value) in raw {
        let name = scalar(&key, "search.fields")?;
        if REFUSED_FIELDS.contains(&name.as_str()) {
            return Err(invalid(
                "search.fields",
                "campo não implementado (categorydesc)",
            ));
        }
        let raw: RawField = serde_yaml_ng::from_value(value)
            .map_err(|_| invalid("search.fields", "campo com chave ou tipo não suportado"))?;
        fields.push((name, compile_field(raw, names)?));
    }
    for required in REQUIRED_FIELDS {
        if !fields.iter().any(|(name, _)| name == required) {
            return Err(invalid("search.fields", "title e size são obrigatórios"));
        }
    }
    if !fields
        .iter()
        .any(|(name, _)| name == "download" || name == "magnet")
    {
        return Err(invalid("search.fields", "download ou magnet é obrigatório"));
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
        let default = setting
            .default
            .as_ref()
            .map(|value| scalar(value, "settings.default"))
            .transpose()?;
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

fn compile_login(raw: RawLogin, names: &Names<'_>, links: &[Url]) -> Result<Login, IndexerError> {
    let method = raw.method.as_deref().unwrap_or("post").to_ascii_lowercase();
    let inputs = pairs(raw.inputs, "login.inputs")?;
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
            LoginMethod::Cookie(Template::compile(value, Scope::Request, names)?)
        }
        "post" | "get" => {
            let path = raw
                .path
                .ok_or_else(|| invalid("login.path", "login por formulário exige path"))?;
            check_static_path(&path, links, "login.path")?;
            let inputs = inputs
                .into_iter()
                .map(|(key, value)| Ok((key, Template::compile(&value, Scope::Request, names)?)))
                .collect::<Result<_, IndexerError>>()?;
            LoginMethod::Form {
                path,
                post: method == "post",
                inputs,
            }
        }
        _ => {
            return Err(invalid(
                "login.method",
                "suportados: post, get e cookie (form e captcha não)",
            ));
        }
    };
    let errors = raw
        .error
        .into_iter()
        .map(|error| {
            let _ = error.message;
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
    Ok(Login {
        method,
        errors,
        test,
    })
}

fn compile_field(raw: RawField, names: &Names<'_>) -> Result<Field, IndexerError> {
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
        (None, selector) => {
            if selector.is_none() && raw.case.is_none() {
                return Err(invalid("search.fields", "campo sem selector, text ou case"));
            }
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
                    .map(|selector| Css::parse(&selector, "search.fields.selector"))
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

type CategoryMappings = BTreeMap<String, Vec<u32>>;

fn compile_categories(caps: &RawCaps) -> Result<(Vec<Category>, CategoryMappings), IndexerError> {
    let entries: Vec<(String, &str)> = match (&caps.categories, &caps.categorymappings) {
        (Some(categories), None) if !categories.is_empty() => categories
            .iter()
            .map(|(id, name)| (id.clone(), name.as_str()))
            .collect(),
        (None, Some(mappings)) if !mappings.is_empty() => mappings
            .iter()
            .map(|entry| {
                let _ = (&entry.desc, entry.default);
                Ok((
                    scalar(&entry.id, "caps.categorymappings")?,
                    entry.cat.as_str(),
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
    let mut mappings: CategoryMappings = BTreeMap::new();
    let mut categories = BTreeMap::new();
    for (tracker, name) in entries {
        if tracker.is_empty() {
            return Err(invalid("caps", "id de categoria vazio"));
        }
        let id =
            category_id(name).ok_or_else(|| invalid("caps", "categoria Newznab desconhecida"))?;
        mappings.entry(tracker).or_default().push(id);
        categories.insert(
            id,
            Category {
                id,
                name: name.to_owned(),
                parent: (!id.is_multiple_of(1000)).then_some(id / 1000 * 1000),
            },
        );
    }
    for ids in mappings.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    Ok((categories.into_values().collect(), mappings))
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

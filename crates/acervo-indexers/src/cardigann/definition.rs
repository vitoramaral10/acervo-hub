use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use scraper::Selector;
use serde::Deserialize;
use url::Url;

use super::template::Template;
use super::{CardigannDefinition, invalid};
use crate::{Capabilities, Category, IndexerError, SearchSupport};

type CategoryMappings = BTreeMap<String, Vec<u32>>;

// Tipos de entrada são privados e nunca implementam Debug: YAML pode conter
// credenciais, inclusive em campos que serão recusados durante a validação.
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
    caps: Caps,
    #[serde(default)]
    settings: Vec<Setting>,
    search: Search,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Caps {
    #[serde(default)]
    categories: Option<BTreeMap<String, String>>,
    #[serde(default)]
    categorymappings: Option<Vec<Mapping>>,
    modes: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    id: Scalar,
    cat: String,
    #[serde(default)]
    desc: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Setting {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    label: String,
    pub default: Option<Scalar>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Scalar {
    String(String),
    Integer(i64),
    Boolean(bool),
}

impl Scalar {
    pub fn value(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Integer(value) => value.to_string(),
            Self::Boolean(value) => value.to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    path: Option<String>,
    paths: Option<Vec<SearchPath>>,
    #[serde(default)]
    inputs: BTreeMap<String, Scalar>,
    #[serde(default, rename = "allowEmptyInputs")]
    allow_empty_inputs: bool,
    rows: Rows,
    fields: BTreeMap<String, Field>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchPath {
    path: String,
    method: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, Scalar>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rows {
    selector: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    selector: Option<String>,
    attribute: Option<String>,
    text: Option<Scalar>,
    #[serde(default)]
    optional: bool,
    default: Option<Scalar>,
    #[serde(default)]
    filters: Vec<Filter>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    name: String,
    #[serde(default)]
    args: Option<Arguments>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Arguments {
    Many(Vec<Scalar>),
    One(Scalar),
}

pub(super) struct CompiledField {
    pub selector: Option<Selector>,
    pub attribute: Option<String>,
    pub text: Option<String>,
    pub optional: bool,
    pub default: Option<String>,
    pub filters: Vec<CompiledFilter>,
}

pub(super) enum CompiledFilter {
    Trim,
    Replace(String, String),
    Append(String),
    Prepend(String),
    Split(String, isize),
}

impl Document {
    fn validate_metadata(&self) -> Result<(), IndexerError> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid("id", "use letras minúsculas, números e hífens"));
        }
        if self.name.trim().is_empty()
            || self.description.trim().is_empty()
            || self.language.trim().is_empty()
        {
            return Err(invalid(
                "metadados",
                "name, description e language são obrigatórios",
            ));
        }
        if self.kind != "public" {
            return Err(invalid(
                "type",
                "somente indexadores públicos são suportados",
            ));
        }
        if !self.encoding.eq_ignore_ascii_case("utf-8") {
            return Err(invalid("encoding", "somente UTF-8 é suportado"));
        }
        Ok(())
    }

    pub fn compile(self) -> Result<CardigannDefinition, IndexerError> {
        self.validate_metadata()?;
        let delay = self.request_delay.unwrap_or(2.0);
        if !delay.is_finite() || !(0.0..=3600.0).contains(&delay) {
            return Err(invalid(
                "requestDelay",
                "esperado intervalo entre 0 e 3600 segundos",
            ));
        }
        let links = self
            .links
            .iter()
            .map(|link| base_url(link))
            .collect::<Result<Vec<_>, _>>()?;
        if links.is_empty() {
            return Err(invalid("links", "ao menos um link é obrigatório"));
        }
        // Metadados históricos não afetam a execução nem elegem um mirror.
        let _ = (self.legacylinks, self.replaces);
        let (categories, mappings) = self.caps.compile_categories()?;
        let capabilities = self.caps.compile_modes(categories)?;
        let settings = compile_settings(self.settings)?;
        let path = match (self.search.path, self.search.paths) {
            (Some(path), None) => SearchPath {
                path,
                method: None,
                inputs: BTreeMap::new(),
            },
            (None, Some(mut paths)) if paths.len() == 1 => paths.remove(0),
            _ => {
                return Err(invalid(
                    "search.paths",
                    "declare path ou exatamente uma rota em paths",
                ));
            }
        };
        if path
            .method
            .is_some_and(|method| !method.eq_ignore_ascii_case("get"))
        {
            return Err(invalid("search.paths.method", "somente GET é suportado"));
        }
        // Um caminho estático evita mudar origem ou codificar termos como rota.
        if path.path.contains(['{', '}']) {
            return Err(invalid(
                "search.paths.path",
                "templates no caminho ainda não são suportados; use inputs",
            ));
        }
        for link in &links {
            search_url(link, &path.path)?;
        }
        let mut raw_inputs = self.search.inputs;
        raw_inputs.extend(path.inputs);
        let mut inputs = BTreeMap::new();
        for (key, value) in raw_inputs {
            if key.is_empty()
                || !key.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'[' | b']')
                })
            {
                return Err(invalid(
                    "search.inputs",
                    "chave inválida ou $raw não suportado",
                ));
            }
            inputs.insert(key, Template::compile(&value.value(), &settings)?);
        }
        validate_mode_templates(&capabilities, &inputs)?;
        let rows = selector(&self.search.rows.selector, "search.rows.selector")?;
        let fields = compile_fields(self.search.fields)?;
        Ok(CardigannDefinition {
            id: self.id,
            name: self.name,
            description: self.description,
            language: self.language,
            links,
            delay: Duration::from_secs_f64(delay),
            capabilities,
            mappings,
            settings,
            path: path.path,
            inputs,
            allow_empty_inputs: self.search.allow_empty_inputs,
            rows,
            fields,
        })
    }
}

fn compile_settings(raw: Vec<Setting>) -> Result<BTreeMap<String, Setting>, IndexerError> {
    let mut settings = BTreeMap::new();
    for setting in raw {
        if !matches!(setting.kind.as_str(), "text" | "password" | "checkbox") {
            return Err(invalid(
                "settings.type",
                "suportados: text, password e checkbox",
            ));
        }
        if setting.name.is_empty()
            || setting.label.trim().is_empty()
            || !setting
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid("settings", "nome ou label inválido"));
        }
        if let Some(value) = &setting.default {
            validate_setting(&setting.kind, &value.value())?;
        }
        if settings.insert(setting.name.clone(), setting).is_some() {
            return Err(invalid("settings", "nome duplicado"));
        }
    }
    Ok(settings)
}

fn compile_fields(
    raw: BTreeMap<String, Field>,
) -> Result<BTreeMap<String, CompiledField>, IndexerError> {
    let mut fields = BTreeMap::new();
    for (name, field) in raw {
        if !matches!(
            name.as_str(),
            "title"
                | "details"
                | "download"
                | "magnet"
                | "size"
                | "seeders"
                | "leechers"
                | "grabs"
                | "category"
        ) {
            return Err(invalid("search.fields", "campo não implementado"));
        }
        fields.insert(name, field.compile()?);
    }
    for name in ["title", "size", "seeders", "category"] {
        if !fields.contains_key(name) || fields[name].optional {
            return Err(invalid(
                "search.fields",
                "title, size, seeders e category são obrigatórios",
            ));
        }
    }
    if !["download", "magnet"]
        .iter()
        .any(|name| fields.get(*name).is_some_and(|field| !field.optional))
    {
        return Err(invalid(
            "search.fields",
            "download ou magnet obrigatório deve fornecer URL direta",
        ));
    }
    Ok(fields)
}

impl Caps {
    fn compile_categories(&self) -> Result<(Vec<Category>, CategoryMappings), IndexerError> {
        let mut mappings: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        let mut categories = BTreeMap::new();
        let entries: Vec<(String, &str)> = match (&self.categories, &self.categorymappings) {
            (Some(categories), None) if !categories.is_empty() => categories
                .iter()
                .map(|(id, name)| (id.clone(), name.as_str()))
                .collect(),
            (None, Some(mappings)) if !mappings.is_empty() => mappings
                .iter()
                .map(|entry| {
                    let _ = &entry.desc;
                    (entry.id.value(), entry.cat.as_str())
                })
                .collect(),
            _ => {
                return Err(invalid(
                    "caps",
                    "declare categories ou categorymappings não vazio",
                ));
            }
        };
        for (tracker, name) in entries {
            if tracker.is_empty() {
                return Err(invalid("caps", "id de categoria vazio"));
            }
            let id = category_id(name)
                .ok_or_else(|| invalid("caps", "categoria Torznab não implementada"))?;
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

    fn compile_modes(&self, categories: Vec<Category>) -> Result<Capabilities, IndexerError> {
        if !self.modes.contains_key("search") {
            return Err(invalid("caps.modes", "search obrigatório"));
        }
        let mut result = Capabilities {
            categories,
            ..Capabilities::default()
        };
        for (mode, params) in &self.modes {
            let (target, allowed): (&mut SearchSupport, &[&str]) = match mode.as_str() {
                "search" => (&mut result.general, &["q"]),
                "tv-search" => (&mut result.tv, &["q", "season", "ep", "tvdbid", "imdbid"]),
                "movie-search" => (&mut result.movie, &["q", "year", "tmdbid", "imdbid"]),
                _ => {
                    return Err(invalid(
                        "caps.modes",
                        "suportados: search, tv-search e movie-search",
                    ));
                }
            };
            if params
                .iter()
                .any(|param| !allowed.contains(&param.as_str()))
            {
                return Err(invalid("caps.modes", "parâmetro de busca não implementado"));
            }
            *target = SearchSupport {
                available: true,
                supported_params: params.iter().cloned().collect::<BTreeSet<_>>(),
            };
        }
        Ok(result)
    }
}

fn validate_mode_templates(
    caps: &Capabilities,
    inputs: &BTreeMap<String, Template>,
) -> Result<(), IndexerError> {
    let references: BTreeSet<&str> = inputs.values().flat_map(Template::variables).collect();
    for support in [&caps.general, &caps.tv, &caps.movie] {
        for param in &support.supported_params {
            let candidates: &[&str] = match param.as_str() {
                "q" => &[".Keywords", ".Query.Keywords"],
                "season" => &[".Keywords", ".Query.Season"],
                "ep" => &[".Keywords", ".Query.Ep"],
                "year" => &[".Keywords", ".Query.Year"],
                "tvdbid" => &[".Query.TVDBID"],
                "tmdbid" => &[".Query.TMDBID"],
                "imdbid" => &[".Query.IMDBID", ".Query.IMDBIDShort"],
                _ => unreachable!("modos validados"),
            };
            if !candidates
                .iter()
                .any(|candidate| references.contains(candidate))
            {
                return Err(invalid(
                    "search.inputs",
                    "capacidade anunciada não é consumida por template",
                ));
            }
        }
    }
    Ok(())
}

impl Field {
    fn compile(self) -> Result<CompiledField, IndexerError> {
        if self.selector.is_some() == self.text.is_some()
            || (self.attribute.is_some() && self.selector.is_none())
            || (self.default.is_some() && !self.optional)
        {
            return Err(invalid(
                "search.fields",
                "use selector ou text; attribute exige selector; default exige optional",
            ));
        }
        let text = self.text.map(|value| value.value());
        let default = self.default.map(|value| value.value());
        for value in text
            .iter()
            .chain(default.iter())
            .chain(self.attribute.iter())
        {
            if value.contains(['{', '}']) {
                return Err(invalid(
                    "search.fields",
                    "templates de campo ainda não são suportados",
                ));
            }
        }
        Ok(CompiledField {
            selector: self
                .selector
                .map(|value| selector(&value, "search.fields.selector"))
                .transpose()?,
            attribute: self.attribute,
            text,
            default,
            optional: self.optional,
            filters: self
                .filters
                .into_iter()
                .map(Filter::compile)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl Filter {
    fn compile(self) -> Result<CompiledFilter, IndexerError> {
        let args = match self.args {
            None => Vec::new(),
            Some(Arguments::One(value)) => vec![value.value()],
            Some(Arguments::Many(values)) => values.iter().map(Scalar::value).collect(),
        };
        if args.iter().any(|value| value.contains(['{', '}'])) {
            return Err(invalid(
                "search.fields.filters",
                "templates em filtros não são suportados",
            ));
        }
        match (self.name.as_str(), args.as_slice()) {
            ("trim", []) => Ok(CompiledFilter::Trim),
            ("replace", [from, to]) if !from.is_empty() => {
                Ok(CompiledFilter::Replace(from.clone(), to.clone()))
            }
            ("append", [value]) => Ok(CompiledFilter::Append(value.clone())),
            ("prepend", [value]) => Ok(CompiledFilter::Prepend(value.clone())),
            ("split", [separator, index]) if !separator.is_empty() => Ok(CompiledFilter::Split(
                separator.clone(),
                index.parse().map_err(|_| {
                    invalid("search.fields.filters.split", "índice inteiro obrigatório")
                })?,
            )),
            _ => Err(invalid(
                "search.fields.filters",
                "filtro ou argumentos não suportados; use trim, replace, append, prepend ou split",
            )),
        }
    }
}

pub(super) fn validate_setting(kind: &str, value: &str) -> Result<(), IndexerError> {
    if kind == "checkbox" && !matches!(value, "true" | "false") {
        return Err(invalid("settings", "checkbox exige true ou false"));
    }
    Ok(())
}

fn selector(value: &str, section: &'static str) -> Result<Selector, IndexerError> {
    Selector::parse(value).map_err(|_| invalid(section, "seletor CSS inválido ou não suportado"))
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

pub(super) fn search_url(base: &Url, path: &str) -> Result<Url, IndexerError> {
    let url = base
        .join(path)
        .map_err(|_| invalid("search.path", "URL inválida"))?;
    if url.origin() != base.origin()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid(
            "search.path",
            "a busca precisa permanecer na origem do link base",
        ));
    }
    Ok(url)
}

fn category_id(name: &str) -> Option<u32> {
    Some(match name {
        "Console" => 1000,
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
        "PC" => 4000,
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
        "Books" => 7000,
        "Other" => 8000,
        _ => return None,
    })
}

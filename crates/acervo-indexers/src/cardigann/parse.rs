//! Resposta de resultados (HTML ou JSON) → releases.

use std::collections::BTreeMap;
use std::fmt::Write;

use regex::Regex;
use scraper::{ElementRef, Html};
use serde_json::Value;
use time::OffsetDateTime;
use url::Url;

use super::CardigannDefinition;
use super::definition::{Field, JsonRows, Rows, Source};
use super::filters::{parse_count, parse_date, parse_size};
use super::json::{self, JsonPath};
use super::selector::Css;
use super::template::{Value as Var, Vars};
use crate::{IndexerError, Release};

/// A linha de onde um campo lê: elemento HTML, ou objeto JSON com a linha
/// original (seletores `..` leem dela, não do filho de `multiple`).
pub(super) enum Ctx<'a> {
    Html(ElementRef<'a>),
    Json { row: &'a Value, object: &'a Value },
}

type Parsed = (Vec<(Release, String)>, usize);

impl CardigannDefinition {
    /// Converte uma página de resultados.
    ///
    /// Linha que não rende release é pulada, como no motor de referência: um
    /// anúncio ou separador no meio da tabela não pode derrubar a busca. Mas
    /// se **nenhuma** linha rende, é erro — página inteira ilegível é o site
    /// que mudou de layout, e isso não pode virar "nada encontrado".
    /// Devolve também quantas linhas a página tinha, com ou sem release —
    /// é o que diz se ela era a última de uma listagem paginada.
    pub(super) fn parse(
        &self,
        body: &str,
        page: &Url,
        request: &Vars,
        now: OffsetDateTime,
    ) -> Result<Parsed, IndexerError> {
        match &self.rows {
            Rows::Json(rows) => self.parse_json(rows, body, page, request, now),
            Rows::Fixed(_) | Rows::Templated(_) => self.parse_html(body, page, request, now),
        }
    }

    fn parse_html(
        &self,
        html: &str,
        page: &Url,
        request: &Vars,
        now: OffsetDateTime,
    ) -> Result<Parsed, IndexerError> {
        // Filtro que falha deixa a página sem linhas — e página sem linhas é
        // o "nada encontrado" de sempre.
        let html = self
            .preprocessing_filters
            .iter()
            .try_fold(html.to_owned(), |body, filter| filter.apply(body, request))
            .unwrap_or_default();
        let document = Html::parse_document(&html);
        let rendered;
        let selector = match &self.rows {
            Rows::Fixed(css) => css,
            Rows::Templated(template) => {
                rendered = Css::parse(&template.render(request), "search.rows.selector")?;
                &rendered
            }
            Rows::Json(_) => unreachable!("parse despacha JSON antes"),
        };
        let mut rows: Vec<ElementRef<'_>> = selector.select(document.root_element()).collect();
        // `rows.after`: a linha absorve as seguintes. Os seletores de campo
        // contam `nth-child` na linha já fundida, então a fusão é de verdade:
        // um documento novo, com os filhos de todas na mesma linha.
        let merged: Vec<Html>;
        if self.after > 0 {
            merged = rows
                .chunks(self.after + 1)
                .filter_map(|group| merge_rows(group))
                .collect();
            rows = merged.iter().filter_map(merged_row).collect();
        }

        let mut releases = Vec::new();
        let mut first_failure = None;
        let count = rows.len();
        for row in rows {
            let ctx = Ctx::Html(row);
            let outcome = self
                .release(&ctx, page, request, now)
                .and_then(|mut found| {
                    if found.0.published.is_none()
                        && let Some(headers) = &self.date_headers
                    {
                        found.0.published = Some(date_from_header(headers, row, request, now)?);
                    }
                    Ok(found)
                });
            match outcome {
                Ok(release) => releases.push(release),
                Err(field) => {
                    tracing::debug!(indexer = self.id, field, "linha sem release");
                    first_failure.get_or_insert(field);
                }
            }
        }
        self.summarize(releases, first_failure, count)
    }

    fn parse_json(
        &self,
        rows: &JsonRows,
        body: &str,
        page: &Url,
        request: &Vars,
        now: OffsetDateTime,
    ) -> Result<Parsed, IndexerError> {
        let unexpected = |expected| IndexerError::UnexpectedDocument {
            indexer: self.id.clone(),
            expected,
        };
        let root: Value = serde_json::from_str(body).map_err(|_| unexpected("JSON"))?;
        let root_ctx = Ctx::Json {
            row: &root,
            object: &root,
        };
        if let Some(count) = &rows.count
            && let Some(text) = evaluate(count, &root_ctx, request)
            && let Ok(total) = text.trim().parse::<i64>()
            && total < 1
        {
            return Ok((Vec::new(), 0));
        }

        let selector = rows.selector.render(request);
        let path = json::path_part(&selector);
        let filters = &selector[path.len()..];
        let list = JsonPath::parse(path)
            .and_then(|path| path.select(&root))
            .and_then(Value::as_array);
        let Some(list) = list else {
            if rows.missing_is_empty {
                return Ok((Vec::new(), 0));
            }
            return Err(unexpected("JSON com as linhas no caminho declarado"));
        };

        let mut releases = Vec::new();
        let mut first_failure = None;
        let mut count = 0;
        for row in list
            .iter()
            .filter(|row| row.is_object() && json::field_selector(row, filters).is_some())
        {
            let selected = match &rows.attribute {
                Some(attribute) => JsonPath::parse(attribute).and_then(|path| path.select(row)),
                None => Some(row),
            };
            let Some(selected) = selected else {
                first_failure.get_or_insert("rows.attribute");
                continue;
            };
            let objects = if rows.multiple {
                json::object_children(selected)
            } else if selected.is_object() {
                vec![selected]
            } else {
                Vec::new()
            };
            for object in objects {
                count += 1;
                let ctx = Ctx::Json { row, object };
                match self.release(&ctx, page, request, now) {
                    Ok(release) => releases.push(release),
                    Err(field) => {
                        tracing::debug!(indexer = self.id, field, "linha sem release");
                        first_failure.get_or_insert(field);
                    }
                }
            }
        }
        self.summarize(releases, first_failure, count)
    }

    fn summarize(
        &self,
        releases: Vec<(Release, String)>,
        first_failure: Option<&'static str>,
        count: usize,
    ) -> Result<Parsed, IndexerError> {
        match first_failure {
            Some(field) if releases.is_empty() => Err(IndexerError::InvalidRelease {
                indexer: self.id.clone(),
                field,
            }),
            _ => Ok((releases, count)),
        }
    }

    /// Categorias Newznab de um valor de `category` (id do tracker) ou de
    /// `categorydesc` (descrição), sem distinguir caixa, como a referência.
    fn category_ids(&self, value: &str, by_description: bool) -> Vec<u32> {
        let value = value.trim();
        if by_description {
            return self
                .description_mappings
                .get(&value.to_lowercase())
                .cloned()
                .unwrap_or_default();
        }
        self.mappings
            .iter()
            .filter(|(tracker, _)| tracker.eq_ignore_ascii_case(value))
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect()
    }

    /// Release e o texto em que o `andmatch` procura (título + descrição).
    fn release(
        &self,
        ctx: &Ctx<'_>,
        page: &Url,
        request: &Vars,
        now: OffsetDateTime,
    ) -> Result<(Release, String), &'static str> {
        let mut vars = request.clone();
        let mut values: BTreeMap<&str, String> = BTreeMap::new();
        let mut categories: Vec<u32> = Vec::new();
        for (name, field) in &self.fields {
            // Campo que falha fica nulo e a linha segue: é assim que os
            // campos auxiliares `_x` funcionam na referência. O que decide se
            // a linha rende release são os campos essenciais, conferidos no
            // fim.
            let mut value = evaluate(field, ctx, &vars);
            if let Some(found) = &value {
                if matches!(name.as_str(), "category" | "categorydesc") {
                    let ids = self.category_ids(found, name == "categorydesc");
                    if field.no_append {
                        categories.clear();
                    }
                    categories.extend(ids);
                }
                if field.append
                    && matches!(name.as_str(), "title" | "description")
                    && let Some(before) = values.get(name.as_str())
                {
                    value = Some(format!("{before}{found}"));
                }
            }
            if let Some(value) = &value {
                values.insert(name, value.clone());
            }
            vars.set(format!(".Result.{name}"), Var::from_option(value));
        }
        categories.sort_unstable();
        categories.dedup();

        let title = values.get("title").cloned().ok_or("title")?;
        let download_url = match values.get("download").or_else(|| values.get("magnet")) {
            Some(link) => item_url(page, link, true).ok_or("download")?,
            // Só o hash: vira magnet público. Tracker privado não publica
            // magnet (o passkey faz parte do torrent), então não vale.
            None if !self.private => values
                .get("infohash")
                .and_then(|hash| magnet_url(hash, &title))
                .ok_or("download")?,
            None => return Err("download"),
        };
        let info_url = values
            .get("details")
            .or_else(|| values.get("comments"))
            .and_then(|value| item_url(page, value, false));
        let size = values
            .get("size")
            .and_then(|value| parse_size(value))
            .ok_or("size")?;
        let count = |name: &str| {
            values
                .get(name)
                .and_then(|value| parse_count(value).ok().flatten())
        };
        let published = values.get("date").and_then(|value| parse_date(value, now));
        let haystack = format!(
            "{title} {}",
            values.get("description").map_or("", String::as_str)
        );
        Ok((
            Release {
                indexer: self.id.clone(),
                guid: info_url.as_ref().unwrap_or(&download_url).to_string(),
                title,
                download_url,
                info_url,
                size,
                published,
                seeders: count("seeders"),
                leechers: count("leechers"),
                grabs: count("grabs"),
                categories,
                tags: Vec::new(),
            },
            haystack,
        ))
    }
}

/// Valor final do campo, ou `None` quando falhou ou veio vazio.
pub(super) fn evaluate(field: &Field, ctx: &Ctx<'_>, vars: &Vars) -> Option<String> {
    let extracted = evaluate_value(field, ctx, vars);
    let value = extracted
        .filter(|value| !value.trim().is_empty())
        // `campo|append` junta o valor como veio: o espaço de uma ponta é de quem
        // escreveu a definição.
        .map(|value| {
            if field.append {
                value
            } else {
                value.trim().to_owned()
            }
        });
    if value.is_some() || !field.optional {
        return value;
    }
    // O default da referência é templado e não passa pelos filtros.
    field
        .default
        .as_ref()
        .map(|default| default.render(vars).trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// O valor depois dos filtros, mesmo vazio: `None` só quando o campo falhou.
pub(super) fn evaluate_value(field: &Field, ctx: &Ctx<'_>, vars: &Vars) -> Option<String> {
    let value = extract(&field.source, ctx, vars)?;
    // Só `text` chega aos filtros como foi escrito; o que veio da página é
    // aparado antes, como na referência.
    let mut value = if matches!(field.source, Source::Text(_)) {
        value
    } else {
        value.trim().to_owned()
    };
    for filter in &field.filters {
        value = filter.apply(value, vars).ok()?;
    }
    Some(value)
}

fn extract(source: &Source, ctx: &Ctx<'_>, vars: &Vars) -> Option<String> {
    match (source, ctx) {
        (Source::Text(template), _) => Some(template.render(vars)),
        (
            Source::Select {
                selector,
                attribute,
                remove,
                case,
            },
            Ctx::Html(row),
        ) => {
            let mut scratch = None;
            let element = match selector {
                Some(selection) => selection.resolve(vars, &mut scratch)?.first(*row)?,
                None => *row,
            };
            if !case.is_empty() {
                // Com `case`, `attribute` não vale: o valor é o da primeira
                // chave que casa, na ordem declarada.
                return case
                    .iter()
                    .find(|(css, _)| css.matches_or_contains(element))
                    .map(|(_, value)| value.render(vars));
            }
            if let Some(attribute) = attribute {
                return element.value().attr(attribute).map(str::to_owned);
            }
            let mut text = String::new();
            visible_text(element, remove.as_ref(), &mut text);
            Some(text)
        }
        (
            Source::Json {
                selector,
                from_row,
                case,
            },
            Ctx::Json { row, object },
        ) => {
            let parent = if *from_row { *row } else { *object };
            let mut value = None;
            if let Some(template) = selector {
                let rendered = template.render(vars);
                let path = json::field_selector(parent, &rendered)?;
                let node = if path.trim().is_empty() {
                    parent
                } else {
                    JsonPath::parse(&path)?.select(parent)?
                };
                value = json::token_text(node);
            }
            // Sem chave que case, vale o valor que já se tinha.
            if let Some((_, matched)) = case
                .iter()
                .find(|(key, _)| value.as_deref() == Some(key.as_str()) || key == "*")
            {
                value = Some(matched.render(vars));
            }
            value
        }
        // Fonte e linha de formatos diferentes não se montam: o formato é
        // único por definição.
        _ => None,
    }
}

/// Texto do elemento sem os descendentes que `remove` tira.
fn visible_text(element: ElementRef<'_>, remove: Option<&Css>, output: &mut String) {
    for child in element.children() {
        if let Some(child_element) = ElementRef::wrap(child) {
            if remove.is_some_and(|css| css.matches(child_element)) {
                continue;
            }
            visible_text(child_element, remove, output);
        } else if let Some(text) = child.value().as_text() {
            output.push_str(text);
        }
    }
}

/// Data de uma linha sem campo `date`: a do cabeçalho mais próximo acima.
///
/// Anda pelos irmãos anteriores e, esgotados, pelo irmão anterior do pai —
/// a tabela agrupada por dia, com uma linha de cabeçalho por grupo.
fn date_from_header(
    headers: &Field,
    row: ElementRef<'_>,
    vars: &Vars,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, &'static str> {
    let mut candidate = previous_element(row);
    while let Some(element) = candidate {
        if let Some(value) = evaluate_value(headers, &Ctx::Html(element), vars) {
            return parse_date(&value, now).ok_or("date");
        }
        candidate = previous_element(element);
    }
    Err("date")
}

/// O elemento anterior: o irmão de antes ou, esgotados, o irmão anterior do pai.
fn previous_element(element: ElementRef<'_>) -> Option<ElementRef<'_>> {
    element
        .prev_siblings()
        .find_map(ElementRef::wrap)
        .or_else(|| {
            element
                .parent()
                .and_then(ElementRef::wrap)
                .and_then(|parent| parent.prev_siblings().find_map(ElementRef::wrap))
        })
}

/// Une cada grupo de linhas numa só, com os filhos na ordem: o `rows.after`
/// da referência. Devolve um documento por grupo.
fn merge_rows(group: &[ElementRef<'_>]) -> Option<Html> {
    let first = group.first()?;
    let name = first.value().name();
    let mut attributes = String::new();
    for (key, value) in first.value().attrs() {
        let _ = write!(
            attributes,
            " {key}=\"{}\"",
            value.replace('&', "&amp;").replace('"', "&quot;")
        );
    }
    let inner: String = group.iter().map(ElementRef::inner_html).collect();
    let row = format!("<{name}{attributes}>{inner}</{name}>");
    // Linha de tabela só se reconhece dentro da tabela.
    let markup = match name {
        "tr" => format!("<table><tbody>{row}</tbody></table>"),
        "td" | "th" => format!("<table><tbody><tr>{row}</tr></tbody></table>"),
        "li" => format!("<ul>{row}</ul>"),
        _ => row,
    };
    Some(Html::parse_fragment(&markup))
}

/// A linha fundida de dentro do documento que `merge_rows` montou.
fn merged_row(document: &Html) -> Option<ElementRef<'_>> {
    let wrappers = ["html", "table", "tbody", "ul"];
    document
        .root_element()
        .descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .find(|element| !wrappers.contains(&element.value().name()))
}

fn item_url(page: &Url, value: &str, allow_magnet: bool) -> Option<Url> {
    let url = page.join(value.trim()).ok()?;
    let allowed =
        matches!(url.scheme(), "http" | "https") || (allow_magnet && url.scheme() == "magnet");
    (allowed && url.username().is_empty() && url.password().is_none()).then_some(url)
}

/// Magnet público a partir do hash e do título, como a referência o monta para
/// `infohash`. Sem trackers: o cliente de torrent acha pares pela DHT.
pub(super) fn magnet_url(hash: &str, title: &str) -> Option<Url> {
    let hash = hash.trim();
    let valid =
        matches!(hash.len(), 32 | 40) && hash.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if !valid {
        return None;
    }
    let name = url::form_urlencoded::byte_serialize(title.trim().as_bytes()).collect::<String>();
    Url::parse(&format!("magnet:?xt=urn:btih:{hash}&dn={name}")).ok()
}

/// Filtro de linhas `andmatch`, com a regra da referência: termos de dois
/// caracteres ou mais, sem `and`/`the`/`an`/`of`; com mais de um termo, ao
/// menos dois precisam aparecer no título ou na descrição.
pub(super) fn and_match(releases: &mut Vec<(Release, String)>, term: &str) {
    let separator = Regex::new(r"[^\w]+").expect("regex fixa");
    let terms: Vec<String> = separator
        .split(term)
        .filter(|word| word.chars().count() > 1)
        .map(str::to_lowercase)
        .filter(|word| !matches!(word.as_str(), "and" | "the" | "an" | "of"))
        .collect();
    if terms.is_empty() {
        return;
    }
    let needed = terms.len().min(2);
    releases.retain(|(_, haystack)| {
        let haystack = haystack.to_lowercase();
        terms
            .iter()
            .filter(|word| haystack.contains(word.as_str()))
            .count()
            >= needed
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(title: &str) -> (Release, String) {
        (
            Release {
                indexer: "x".into(),
                guid: "g".into(),
                title: title.into(),
                download_url: Url::parse("https://tracker.invalid/d").unwrap(),
                info_url: None,
                size: 1,
                published: None,
                seeders: None,
                leechers: None,
                grabs: None,
                categories: Vec::new(),
                tags: Vec::new(),
            },
            title.into(),
        )
    }

    #[test]
    fn andmatch_exige_dois_termos_e_ignora_palavras_vazias() {
        let mut releases = vec![
            release("The Office S01E01"),
            release("Office Space 1999"),
            release("Parks and Recreation"),
        ];
        and_match(&mut releases, "The Office S01E01");
        let titles: Vec<_> = releases.iter().map(|(r, _)| r.title.as_str()).collect();
        assert_eq!(titles, ["The Office S01E01"]);

        let mut single = vec![release("Office Space"), release("Outra Coisa")];
        and_match(&mut single, "office");
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn remove_tira_o_texto_do_descendente() {
        let document =
            Html::parse_fragment(r#"<div><a>Título <span class="tag">NOVO</span></a></div>"#);
        let css = Css::parse("a", "t").unwrap();
        let element = css.select(document.root_element()).next().unwrap();
        let mut text = String::new();
        visible_text(
            element,
            Some(&Css::parse("span.tag", "t").unwrap()),
            &mut text,
        );
        assert_eq!(text.trim(), "Título");
    }

    #[test]
    fn magnet_de_infohash_exige_hash_valido_e_codifica_o_titulo() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let url = magnet_url(hash, " Título & Cia ").unwrap();
        assert_eq!(
            url.as_str(),
            format!("magnet:?xt=urn:btih:{hash}&dn=T%C3%ADtulo+%26+Cia")
        );
        assert!(magnet_url("curto", "x").is_none());
        assert!(magnet_url(&"z!".repeat(20), "x").is_none());
    }

    #[test]
    fn linhas_fundidas_juntam_os_filhos_e_mantem_a_posicao_dos_nth_child() {
        let document = Html::parse_document(
            r#"<table><tbody>
                <tr id="a"><td>um</td><td>dois</td></tr>
                <tr><td>tres</td></tr>
                <tr id="b"><td>quatro</td><td>cinco</td></tr>
                <tr><td>seis</td></tr>
            </tbody></table>"#,
        );
        let tr = Css::parse("tr", "t").unwrap();
        let rows: Vec<_> = tr.select(document.root_element()).collect();
        let merged: Vec<Html> = rows.chunks(2).filter_map(merge_rows).collect();
        let fused: Vec<_> = merged.iter().filter_map(merged_row).collect();
        assert_eq!(fused.len(), 2);
        let third = Css::parse("td:nth-child(3)", "t").unwrap();
        let text = |row: ElementRef<'_>| {
            third
                .select(row)
                .next()
                .map(|td| td.text().collect::<String>())
        };
        assert_eq!(text(fused[0]).as_deref(), Some("tres"));
        assert_eq!(text(fused[1]).as_deref(), Some("seis"));
        assert_eq!(fused[0].value().attr("id"), Some("a"));
    }
}

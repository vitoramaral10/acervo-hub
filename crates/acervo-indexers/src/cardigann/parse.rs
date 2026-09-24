//! HTML de resultados → releases.

use std::collections::BTreeMap;

use regex::Regex;
use scraper::{ElementRef, Html};
use time::OffsetDateTime;
use url::Url;

use super::CardigannDefinition;
use super::definition::{Field, Rows, Source};
use super::filters::{parse_count, parse_date, parse_size};
use super::selector::Css;
use super::template::{Value, Vars};
use crate::{IndexerError, Release};

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
        html: &str,
        page: &Url,
        request: &Vars,
        now: OffsetDateTime,
    ) -> Result<(Vec<(Release, String)>, usize), IndexerError> {
        let document = Html::parse_document(html);
        let rendered;
        let rows = match &self.rows {
            Rows::Fixed(css) => css,
            Rows::Templated(template) => {
                rendered = Css::parse(&template.render(request), "search.rows.selector")?;
                &rendered
            }
        };
        let mut releases = Vec::new();
        let mut first_failure = None;
        let mut count = 0;
        for row in rows.select(document.root_element()) {
            count += 1;
            match self.release(row, page, request, now) {
                Ok(release) => releases.push(release),
                Err(field) => {
                    tracing::debug!(indexer = self.id, field, "linha sem release");
                    first_failure.get_or_insert(field);
                }
            }
        }
        match first_failure {
            Some(field) if releases.is_empty() => Err(IndexerError::InvalidRelease {
                indexer: self.id.clone(),
                field,
            }),
            _ => Ok((releases, count)),
        }
    }

    /// Release e o texto em que o `andmatch` procura (título + descrição).
    fn release(
        &self,
        row: ElementRef<'_>,
        page: &Url,
        request: &Vars,
        now: OffsetDateTime,
    ) -> Result<(Release, String), &'static str> {
        let mut vars = request.clone();
        let mut values: BTreeMap<&str, String> = BTreeMap::new();
        for (name, field) in &self.fields {
            // Campo que falha fica nulo e a linha segue: é assim que os
            // campos auxiliares `_x` funcionam na referência. O que decide se
            // a linha rende release são os campos essenciais, conferidos no
            // fim.
            let value = evaluate(field, row, &vars);
            if let Some(value) = &value {
                values.insert(name, value.clone());
            }
            vars.set(format!(".Result.{name}"), Value::from_option(value));
        }

        let title = values.get("title").cloned().ok_or("title")?;
        let download = values
            .get("download")
            .or_else(|| values.get("magnet"))
            .ok_or("download")?;
        let download_url = item_url(page, download, true).ok_or("download")?;
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
        let categories = values
            .get("category")
            .and_then(|value| self.mappings.get(value.trim()))
            .cloned()
            .unwrap_or_default();
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
fn evaluate(field: &Field, row: ElementRef<'_>, vars: &Vars) -> Option<String> {
    let extracted = extract(&field.source, row, vars).and_then(|mut value| {
        for filter in &field.filters {
            value = filter.apply(value, vars).ok()?;
        }
        Some(value)
    });
    let value = extracted
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
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

fn extract(source: &Source, row: ElementRef<'_>, vars: &Vars) -> Option<String> {
    match source {
        Source::Text(template) => Some(template.render(vars)),
        Source::Select {
            selector,
            attribute,
            remove,
            case,
        } => {
            let element = match selector {
                Some(css) => css.select(row).next()?,
                None => row,
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

fn item_url(page: &Url, value: &str, allow_magnet: bool) -> Option<Url> {
    let url = page.join(value.trim()).ok()?;
    let allowed =
        matches!(url.scheme(), "http" | "https") || (allow_magnet && url.scheme() == "magnet");
    (allowed && url.username().is_empty() && url.password().is_none()).then_some(url)
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
}

//! Seletores CSS com a extensão `:contains(...)` que as definições usam.
//!
//! `:contains` não é CSS e o `scraper` não o conhece. Ele é aceito só no
//! último composto de cada seletor — o elemento que o seletor devolve —, que é
//! onde todas as definições reais o usam: sai do texto antes de compilar e
//! vira um filtro pelo texto do elemento. Em qualquer outra posição, a
//! definição é recusada em vez de ter o filtro aplicado no elemento errado.

use scraper::{ElementRef, Selector};

use super::invalid;
use crate::IndexerError;

#[derive(Debug)]
pub(super) struct Css(Vec<Part>);

#[derive(Debug)]
struct Part {
    selector: Selector,
    contains: Vec<String>,
}

impl Css {
    pub fn parse(source: &str, section: &'static str) -> Result<Self, IndexerError> {
        let parts = split_top_level(source)
            .into_iter()
            .map(|part| Part::parse(part.trim(), section))
            .collect::<Result<Vec<_>, _>>()?;
        if parts.is_empty() {
            return Err(invalid(section, "seletor vazio"));
        }
        Ok(Self(parts))
    }

    pub fn matches(&self, element: ElementRef<'_>) -> bool {
        self.0.iter().any(|part| {
            part.selector.matches(&element) && {
                let text: String = element.text().collect();
                part.contains
                    .iter()
                    .all(|needle| text.contains(needle.as_str()))
            }
        })
    }

    /// Descendentes que casam, em ordem de documento. A união de vários
    /// seletores separados por vírgula sai na ordem do documento, e não
    /// seletor por seletor — é o que os motores de navegador fazem.
    pub fn select<'a>(
        &'a self,
        scope: ElementRef<'a>,
    ) -> impl Iterator<Item = ElementRef<'a>> + 'a {
        scope
            .descendants()
            .skip(1)
            .filter_map(ElementRef::wrap)
            .filter(|element| self.matches(*element))
    }

    /// O próprio elemento ou algum descendente casa — o critério de `case`.
    pub fn matches_or_contains(&self, element: ElementRef<'_>) -> bool {
        self.matches(element) || self.select(element).next().is_some()
    }
}

impl Part {
    fn parse(source: &str, section: &'static str) -> Result<Self, IndexerError> {
        let mut rest = source;
        let mut stripped = String::new();
        let mut contains = Vec::new();
        while let Some(start) = rest.find(":contains(") {
            stripped.push_str(&rest[..start]);
            let argument_start = start + ":contains(".len();
            let (argument, consumed) = argument(&rest[argument_start..])
                .ok_or_else(|| invalid(section, ":contains malformado"))?;
            contains.push(argument);
            rest = &rest[argument_start + consumed..];
            if has_combinator(rest) {
                return Err(invalid(
                    section,
                    ":contains só é suportado no último elemento do seletor",
                ));
            }
        }
        stripped.push_str(rest);
        let stripped = stripped.trim();
        // `div :contains(x)` deixaria o composto vazio depois do combinador.
        let stripped = if stripped.is_empty() || stripped.ends_with([' ', '>', '+', '~']) {
            format!("{stripped}*")
        } else {
            stripped.to_owned()
        };
        let selector = Selector::parse(&stripped)
            .map_err(|_| invalid(section, "seletor CSS inválido ou não suportado"))?;
        Ok(Self { selector, contains })
    }
}

/// Lê o argumento de `:contains(` até o `)` que o fecha. Devolve o texto e
/// quantos bytes foram consumidos, incluindo o `)`.
fn argument(source: &str) -> Option<(String, usize)> {
    let trimmed = source.trim_start();
    let leading = source.len() - trimmed.len();
    let quote = trimmed.chars().next().filter(|c| *c == '"' || *c == '\'');
    if let Some(quote) = quote {
        let body = &trimmed[1..];
        let end = body.find(quote)?;
        let after = &body[end + 1..];
        let close = after.find(')')?;
        if !after[..close].trim().is_empty() {
            return None;
        }
        Some((body[..end].to_owned(), leading + 1 + end + 1 + close + 1))
    } else {
        let close = trimmed.find(')')?;
        Some((trimmed[..close].trim().to_owned(), leading + close + 1))
    }
}

/// Há combinador fora de parênteses, colchetes e aspas?
fn has_combinator(rest: &str) -> bool {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    for character in rest.chars() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, ' ' | '>' | '+' | '~') if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Divide nas vírgulas de nível zero: `a:contains("x, y"), b` são dois.
fn split_top_level(source: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&source[start..]);
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use scraper::Html;

    use super::*;

    const HTML: &str = r#"<table><tbody>
        <tr class="colhead"><td>S01E02</td></tr>
        <tr class="group"><td>grupo</td></tr>
        <tr><td>Série S01E02</td></tr>
        <tr><td>Série S01E03</td></tr>
        <tr class="group"><td>outro S01E02</td></tr>
    </tbody></table>
    <p><span class="badge">1080p</span><span class="badge">WEB-DL</span></p>"#;

    fn texts(css: &str) -> Vec<String> {
        let document = Html::parse_document(HTML);
        let css = Css::parse(css, "teste").unwrap();
        css.select(document.root_element())
            .map(|element| element.text().collect::<String>().trim().to_owned())
            .collect()
    }

    #[test]
    fn uniao_com_contains_sai_em_ordem_de_documento_sem_repetir() {
        assert_eq!(
            texts(
                "tbody > tr:not(tr.colhead).group,
                 tbody tr:not(tr.colhead):contains('S01E02')"
            ),
            ["grupo", "Série S01E02", "outro S01E02"]
        );
    }

    #[test]
    fn contains_com_aspas_duplas_e_virgula_dentro() {
        assert_eq!(
            texts(r#"p span.badge:contains("1080p"), p span.badge:contains("WEB-")"#),
            ["1080p", "WEB-DL"]
        );
        assert_eq!(texts(r#"td:contains("a, b")"#), Vec::<String>::new());
    }

    #[test]
    fn contains_vazio_casa_tudo() {
        assert_eq!(texts("tr.group:contains('')").len(), 2);
    }

    #[test]
    fn contains_fora_do_ultimo_elemento_e_recusado() {
        assert!(Css::parse("tr:contains('x') td", "teste").is_err());
        assert!(Css::parse("tr:contains('x') > td", "teste").is_err());
        assert!(Css::parse("tr:contains('x", "teste").is_err());
    }

    #[test]
    fn case_casa_no_proprio_elemento_ou_em_descendente() {
        let document = Html::parse_document(HTML);
        let css = Css::parse("p", "teste").unwrap();
        let paragraph = css.select(document.root_element()).next().unwrap();
        assert!(
            Css::parse("span:contains(\"1080p\")", "t")
                .unwrap()
                .matches_or_contains(paragraph)
        );
        assert!(Css::parse("*", "t").unwrap().matches_or_contains(paragraph));
        assert!(
            !Css::parse("span:contains(\"4k\")", "t")
                .unwrap()
                .matches_or_contains(paragraph)
        );
    }
}

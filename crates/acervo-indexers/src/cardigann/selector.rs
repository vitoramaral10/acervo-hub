//! Seletores CSS com a extensão `:contains(...)` que as definições usam.
//!
//! `:contains` não é CSS e o `scraper` não o conhece. Seletor sem ele vai
//! inteiro para o `scraper`. Seletor com ele é desmontado em compostos
//! (`tr:not(:contains(x))` `>` `td`), ligados pelos combinadores, e casado da
//! direita para a esquerda: cada composto é um seletor do `scraper` mais os
//! filtros de texto — `:contains(x)`, e `:not(...)`/`:has(...)` que o contenham
//! (`td:has(h2:contains(x))`), cujo argumento é um seletor completo avaliado por
//! este mesmo módulo. `:contains` dentro de outro pseudo-seletor (`:is(...)`)
//! é recusado, em vez de ter o filtro aplicado no elemento errado.
//!
//! `[atributo!=valor]`, de uso corrente nas definições, vira
//! `:not([atributo=valor])`, e `[atributo=1]` (valor sem aspas que o CSS não
//! aceita) ganha aspas, como no motor de referência.

use std::fmt::Write;

use scraper::{ElementRef, Selector};

use super::invalid;
use crate::IndexerError;

/// Os pseudo-seletores que o `scraper` não resolve, ou resolve só sem texto.
const PREFIXES: [&str; 3] = [":contains(", ":not(", ":has("];

#[derive(Debug)]
pub(super) struct Css(Vec<Part>);

#[derive(Debug)]
enum Part {
    /// Sem texto: o `scraper` casa o seletor inteiro.
    Plain(Selector),
    /// Com `:contains` em algum composto. Cada elo traz o combinador que o liga
    /// ao elo anterior (o do primeiro não conta).
    Chain(Vec<(Combinator, Compound)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
    Next,
    Subsequent,
}

/// Um composto: o seletor do `scraper` e o que ele não sabe fazer.
#[derive(Debug)]
struct Compound {
    selector: Selector,
    contains: Vec<String>,
    /// `:not(...)` com `:contains` lá dentro: o elemento não casa com nenhum.
    not: Vec<Css>,
    /// `:has(...)` com `:contains` lá dentro: algum descendente casa.
    has: Vec<Css>,
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
        self.0.iter().any(|part| part.matches(element))
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

    /// O próprio elemento, se casa; senão o primeiro descendente que casa — a
    /// busca de um seletor de campo na referência.
    pub fn first<'a>(&'a self, scope: ElementRef<'a>) -> Option<ElementRef<'a>> {
        if self.matches(scope) {
            Some(scope)
        } else {
            self.select(scope).next()
        }
    }

    /// O próprio elemento ou algum descendente casa — o critério de `case`.
    pub fn matches_or_contains(&self, element: ElementRef<'_>) -> bool {
        self.matches(element) || self.select(element).next().is_some()
    }
}

impl Part {
    fn parse(source: &str, section: &'static str) -> Result<Self, IndexerError> {
        let source = normalize_attributes(source);
        if !needs_engine(&source) {
            return Selector::parse(&source)
                .map(Self::Plain)
                .map_err(|_| invalid(section, "seletor CSS inválido ou não suportado"));
        }
        let links = split_compounds(&source)
            .ok_or_else(|| invalid(section, "seletor CSS inválido ou não suportado"))?
            .into_iter()
            .map(|(combinator, text)| Ok((combinator, Compound::parse(text, section)?)))
            .collect::<Result<Vec<_>, IndexerError>>()?;
        Ok(Self::Chain(links))
    }

    fn matches(&self, element: ElementRef<'_>) -> bool {
        match self {
            Self::Plain(selector) => selector.matches(&element),
            Self::Chain(links) => chain_matches(links, element),
        }
    }
}

/// O último elo casa o elemento e os anteriores casam o que o combinador diz.
fn chain_matches(links: &[(Combinator, Compound)], element: ElementRef<'_>) -> bool {
    let Some(((combinator, compound), earlier)) = links.split_last() else {
        return true;
    };
    compound.matches(element) && relation_matches(earlier, *combinator, element)
}

fn relation_matches(
    earlier: &[(Combinator, Compound)],
    combinator: Combinator,
    element: ElementRef<'_>,
) -> bool {
    if earlier.is_empty() {
        return true;
    }
    let mut related: Box<dyn Iterator<Item = ElementRef<'_>> + '_> = match combinator {
        Combinator::Child => Box::new(element.parent().and_then(ElementRef::wrap).into_iter()),
        Combinator::Descendant => Box::new(element.ancestors().filter_map(ElementRef::wrap)),
        Combinator::Next => Box::new(
            element
                .prev_siblings()
                .find_map(ElementRef::wrap)
                .into_iter(),
        ),
        Combinator::Subsequent => Box::new(element.prev_siblings().filter_map(ElementRef::wrap)),
    };
    related.any(|candidate| chain_matches(earlier, candidate))
}

/// Desmonta um seletor nos compostos e nos combinadores entre eles, fora de
/// parênteses, colchetes e aspas. `None` se começa ou termina num combinador.
fn split_compounds(source: &str) -> Option<Vec<(Combinator, &str)>> {
    let mut compounds = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut pending = Combinator::Descendant;
    let mut index = 0;
    let bytes = source.as_bytes();
    while index < source.len() {
        let character = source[index..].chars().next()?;
        let width = character.len_utf8();
        match (quote, character) {
            (Some(_), '\\') => {
                index += width
                    + source[index + width..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
            }
            (Some(open), c) if c == open => {
                quote = None;
                index += width;
            }
            (None, '"' | '\'') => {
                quote = Some(character);
                index += width;
            }
            (None, '(' | '[') => {
                depth += 1;
                index += width;
            }
            (None, ')' | ']') => {
                depth -= 1;
                index += width;
            }
            (None, ' ' | '\t' | '\n' | '\r' | '>' | '+' | '~') if depth == 0 => {
                let text = source[start..index].trim();
                // Come a corrida de espaços e símbolos: o símbolo, se houver, é o
                // combinador; só espaço é descendente.
                let mut combinator = Combinator::Descendant;
                while index < source.len()
                    && matches!(
                        bytes[index],
                        b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'+' | b'~'
                    )
                {
                    match bytes[index] {
                        b'>' => combinator = Combinator::Child,
                        b'+' => combinator = Combinator::Next,
                        b'~' => combinator = Combinator::Subsequent,
                        _ => {}
                    }
                    index += 1;
                }
                if text.is_empty() {
                    // Combinador no começo: só vale `:contains` sozinho depois dele.
                    if compounds.is_empty() {
                        return None;
                    }
                    pending = combinator;
                } else {
                    compounds.push((pending, text));
                    pending = combinator;
                }
                start = index;
            }
            _ => index += width,
        }
    }
    let last = source[start..].trim();
    if last.is_empty() {
        return None;
    }
    compounds.push((pending, last));
    Some(compounds)
}

impl Compound {
    /// Os filtros de texto do composto, depois de o seletor ter casado.
    fn matches(&self, element: ElementRef<'_>) -> bool {
        if !self.selector.matches(&element) {
            return false;
        }
        if !self.contains.is_empty() {
            let text: String = element.text().collect();
            if !self
                .contains
                .iter()
                .all(|needle| text.contains(needle.as_str()))
            {
                return false;
            }
        }
        !self.not.iter().any(|css| css.matches(element))
            && self
                .has
                .iter()
                .all(|css| css.select(element).next().is_some())
    }

    fn parse(source: &str, section: &'static str) -> Result<Self, IndexerError> {
        let mut rest = source;
        let mut stripped = String::new();
        let mut compound = Self {
            // Substituído abaixo, depois de o composto estar limpo.
            selector: Selector::parse("*").expect("seletor fixo"),
            contains: Vec::new(),
            not: Vec::new(),
            has: Vec::new(),
        };
        // O pseudo-seletor de texto (ou `:not(`/`:has(` que o contém) que vem
        // primeiro no que sobrou.
        while let Some((start, prefix)) = PREFIXES
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|at| (at, *prefix)))
            .min_by_key(|(at, _)| *at)
        {
            let argument_start = start + prefix.len();
            // Dentro de outro pseudo-seletor (`:is(a:contains(x))`) o filtro
            // cairia no elemento errado.
            let enclosed = open_parens(&format!("{stripped}{}", &rest[..start])) > 0;
            if prefix == ":contains(" {
                if enclosed {
                    return Err(invalid(
                        section,
                        ":contains só é suportado diretamente no composto, ou dentro de :not e :has",
                    ));
                }
                stripped.push_str(&rest[..start]);
                let (argument, consumed) = argument(&rest[argument_start..])
                    .ok_or_else(|| invalid(section, ":contains malformado"))?;
                compound.contains.push(argument);
                rest = &rest[argument_start + consumed..];
                continue;
            }
            // O argumento vai até o `)` que fecha o `:not(` / `:has(`.
            let length = closing_paren(&rest[argument_start..])
                .ok_or_else(|| invalid(section, "seletor CSS inválido ou não suportado"))?;
            let inner = &rest[argument_start..argument_start + length];
            let end = argument_start + length + 1;
            // `:has` dentro de `:has` o `scraper` recusa (o CSS também); aqui vale.
            if (needs_engine(inner) || (prefix == ":has(" && inner.contains(":has("))) && !enclosed
            {
                stripped.push_str(&rest[..start]);
                let nested = Css::parse(inner, section)?;
                if prefix == ":not(" {
                    compound.not.push(nested);
                } else {
                    compound.has.push(nested);
                }
            } else {
                // Sem texto lá dentro, o `scraper` cuida: segue adiante.
                stripped.push_str(&rest[..end]);
            }
            rest = &rest[end..];
        }
        stripped.push_str(rest);
        let stripped = stripped.trim();
        // `:contains(x)` sozinho deixa o composto vazio.
        let stripped = if stripped.is_empty() { "*" } else { stripped };
        compound.selector = Selector::parse(stripped)
            .map_err(|_| invalid(section, "seletor CSS inválido ou não suportado"))?;
        Ok(compound)
    }
}

/// O seletor precisa do motor deste módulo: tem `:contains`, ou `:has` dentro
/// de `:has`, que o `scraper` não aceita.
fn needs_engine(source: &str) -> bool {
    if source.contains(":contains(") {
        return true;
    }
    let mut rest = source;
    while let Some(at) = rest.find(":has(") {
        let inner_start = at + ":has(".len();
        let Some(length) = closing_paren(&rest[inner_start..]) else {
            return false;
        };
        if rest[inner_start..inner_start + length].contains(":has(") {
            return true;
        }
        rest = &rest[inner_start..];
    }
    false
}

/// Posição do `)` que fecha o parêntese já aberto, fora de aspas e colchetes.
fn closing_paren(text: &str) -> Option<usize> {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut chars = text.char_indices();
    while let Some((index, character)) = chars.next() {
        match (quote, character) {
            (Some(_), '\\') => {
                chars.next();
            }
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') if depth == 0 => return Some(index),
            (None, ')') => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Ajusta os seletores de atributo ao que as definições escrevem e o motor de
/// referência aceita: `[nome!=valor]` vira `:not([nome=valor])`, e valor sem
/// aspas que não é identificador CSS (`[border=1]`) ganha aspas.
fn normalize_attributes(source: &str) -> String {
    if !source.contains('[') {
        return source.to_owned();
    }
    let mut output = String::with_capacity(source.len() + 8);
    let mut rest = source;
    while let Some(open) = attribute_start(rest) {
        output.push_str(&rest[..open]);
        let Some(close) = attribute_end(&rest[open + 1..]).map(|at| open + 1 + at) else {
            break;
        };
        let inside = &rest[open + 1..close];
        rest = &rest[close + 1..];
        let Some(at) = inside.find(['=', '~', '|', '^', '$', '*', '!']) else {
            let _ = write!(output, "[{inside}]");
            continue;
        };
        let name = &inside[..at];
        let after = &inside[at..];
        let operator_length = after.find('=').map_or(after.len(), |eq| eq + 1);
        let (operator, value) = after.split_at(operator_length);
        let value = value.trim();
        let (value, flag) = match value.rsplit_once(' ') {
            Some((head, flag)) if matches!(flag, "i" | "s") && !head.trim().is_empty() => {
                (head.trim(), format!(" {flag}"))
            }
            _ => (value, String::new()),
        };
        let quoted = if value.starts_with(['"', '\'']) || is_identifier(value) {
            value.to_owned()
        } else {
            format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
        };
        if operator == "!=" {
            let _ = write!(output, ":not([{name}={quoted}{flag}])");
        } else {
            let _ = write!(output, "[{name}{operator}{quoted}{flag}]");
        }
    }
    output.push_str(rest);
    output
}

/// Posição do próximo `[` que abre um seletor de atributo: o que está entre
/// aspas (`:contains('[x]')`) é texto.
fn attribute_start(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut chars = text.char_indices();
    while let Some((index, character)) = chars.next() {
        match (quote, character) {
            (Some(_), '\\') => {
                chars.next();
            }
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '[') => return Some(index),
            _ => {}
        }
    }
    None
}

/// Posição do `]` que fecha o seletor de atributo, fora de aspas.
fn attribute_end(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut chars = text.char_indices();
    while let Some((index, character)) = chars.next() {
        match (quote, character) {
            (Some(_), '\\') => {
                chars.next();
            }
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, ']') => return Some(index),
            _ => {}
        }
    }
    None
}

/// Identificador CSS simples: letra ou `_` (ou `-` e letra) seguido de letras,
/// dígitos, `-` e `_`.
fn is_identifier(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    let mut chars = body.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || !first.is_ascii())
        && chars.all(|c| c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii())
}

/// Lê o argumento de `:contains(` até o `)` que o fecha. Devolve o texto e
/// quantos bytes foram consumidos, incluindo o `)`. Entre aspas, `\"` e `\\`
/// são escapes, como nas strings do CSS.
fn argument(source: &str) -> Option<(String, usize)> {
    let trimmed = source.trim_start();
    let leading = source.len() - trimmed.len();
    let quote = trimmed.chars().next().filter(|c| *c == '"' || *c == '\'');
    if let Some(quote) = quote {
        let mut text = String::new();
        let mut chars = trimmed[1..].char_indices();
        let end = loop {
            let (index, character) = chars.next()?;
            match character {
                '\\' => {
                    let (_, escaped) = chars.next()?;
                    if escaped != quote && escaped != '\\' {
                        text.push('\\');
                    }
                    text.push(escaped);
                }
                c if c == quote => break index,
                c => text.push(c),
            }
        };
        let after = &trimmed[1 + end + 1..];
        let close = after.find(')')?;
        if !after[..close].trim().is_empty() {
            return None;
        }
        Some((text, leading + 1 + end + 1 + close + 1))
    } else {
        let close = trimmed.find(')')?;
        Some((trimmed[..close].trim().to_owned(), leading + close + 1))
    }
}

/// Parênteses abertos e ainda não fechados, fora de aspas.
fn open_parens(text: &str) -> i32 {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        match (quote, character) {
            (Some(_), '\\') => {
                chars.next();
            }
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Divide nas vírgulas de nível zero: `a:contains("x, y"), b` são dois.
fn split_top_level(source: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut chars = source.char_indices();
    while let Some((index, character)) = chars.next() {
        match (quote, character) {
            (Some(_), '\\') => {
                chars.next();
            }
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
    fn not_contains_e_has_contains_no_composto() {
        assert_eq!(
            texts("tbody > tr:not(tr.colhead):not(:contains('grupo')):not(:contains(\"outro\"))"),
            ["Série S01E02", "Série S01E03"]
        );
        assert_eq!(texts("tr:has(:contains('S01E03'))"), ["Série S01E03"]);
        // O texto do próprio elemento não conta: o descendente é que precisa tê-lo.
        assert_eq!(texts("td:has(:contains('S01E03'))"), Vec::<String>::new());
        assert!(Css::parse("tr:not(:contains('x') td", "teste").is_err());
    }

    #[test]
    fn has_e_not_com_seletor_completo_e_contains_lá_dentro() {
        let document = Html::parse_document(
            r#"<div class="c"><h2>Falha de login</h2><p>x</p></div>
               <div class="c"><h2>Bem-vindo</h2><p>Falha</p></div>
               <div class="d"><span><b>Falha</b></span></div>"#,
        );
        let count = |css: &str| {
            Css::parse(css, "t")
                .unwrap()
                .select(document.root_element())
                .count()
        };
        assert_eq!(count("div.c:has(h2:contains('Falha'))"), 1);
        assert_eq!(count("div:has(b:contains('Falha'))"), 1);
        assert_eq!(count("div:has(span b:contains('Falha'))"), 1);
        assert_eq!(count("div.c:not(:has(h2:contains('Falha')))"), 1);
        assert_eq!(count("div:not(.d):not(:has(p:contains('Falha')))"), 1);
        assert_eq!(count("div:not(.c, .d:contains('zzz'))"), 1);
        assert_eq!(count("div:has(p, h2:contains('Bem'))"), 2);
        // Sem texto lá dentro, a regra é a do CSS comum.
        assert_eq!(count("div:has(h2):not(.d)"), 2);
    }

    #[test]
    fn has_dentro_de_has_vale_aqui_mesmo_sem_texto() {
        let document = Html::parse_document(
            r#"<table><tr><td><a href="magnet:?x">m</a></td></tr><tr><td><a href="/t">t</a></td></tr></table>"#,
        );
        let css = Css::parse(r#"tr:has(td:has(a[href^="magnet:"]))"#, "t").unwrap();
        assert_eq!(css.select(document.root_element()).count(), 1);
    }

    #[test]
    fn valor_de_atributo_sem_aspas_que_nao_e_identificador_ganha_aspas() {
        let document = Html::parse_document(
            r#"<table border="1" width="100%"><tr><td>a</td></tr></table><table border="0"></table>"#,
        );
        let count = |css: &str| {
            Css::parse(css, "t")
                .unwrap()
                .select(document.root_element())
                .count()
        };
        assert_eq!(count("table[border=1]"), 1);
        assert_eq!(count(r#"table[width="100%"][border=1]"#), 1);
        assert_eq!(count("table[border!=1]"), 1);
        assert_eq!(count("table[border=1 i]"), 1);
        assert_eq!(count("table[border]"), 2);
        assert_eq!(normalize_attributes("a[href^=x]"), "a[href^=x]");
        assert_eq!(
            normalize_attributes("a[data-x=2024]:contains('[y]')"),
            "a[data-x=\"2024\"]:contains('[y]')"
        );
        assert_eq!(
            normalize_attributes("a:contains('[b=1 c]')"),
            "a:contains('[b=1 c]')"
        );
    }

    #[test]
    fn atributo_diferente_vira_not_e_aspas_escapadas_no_contains() {
        let document = Html::parse_document(
            r#"<div id="a" class="x"></div><div id="b" class="x">{"ok":false}</div>"#,
        );
        let all = |css: &str| {
            Css::parse(css, "t")
                .unwrap()
                .select(document.root_element())
                .count()
        };
        assert_eq!(all(r#"div[id!="a"]"#), 1);
        assert_eq!(all("div[id!=a][class='x']"), 1);
        assert_eq!(all(r#"div:contains("{\"ok\":false}")"#), 1);
        assert_eq!(all(r#"div:contains("{\"ok\":true}")"#), 0);
    }

    #[test]
    fn contains_vazio_casa_tudo() {
        assert_eq!(texts("tr.group:contains('')").len(), 2);
    }

    #[test]
    fn contains_em_qualquer_composto_e_recusado_so_onde_nao_tem_sentido() {
        // Antes um combinador: casa o ancestral certo, não o elemento final.
        let document = Html::parse_document(
            r#"<table><tr><td>Latest</td></tr><tr class="x"><td>a</td></tr></table>
               <table><tr><td>Older</td></tr><tr class="x"><td>b</td></tr></table>"#,
        );
        let texts = |css: &str| -> Vec<String> {
            Css::parse(css, "t")
                .unwrap()
                .select(document.root_element())
                .map(|e| e.text().collect::<String>())
                .collect()
        };
        assert_eq!(texts("table:contains('Latest') tr.x"), ["a"]);
        assert_eq!(texts("table:contains('Older') > tbody > tr.x td"), ["b"]);
        assert_eq!(texts("tr:contains('Latest') + tr.x"), ["a"]);
        assert_eq!(texts("tr:contains('Latest') ~ tr"), ["a"]);
        assert_eq!(
            texts("table:not(:has(td:contains('Latest'))) td:contains('b')"),
            ["b"]
        );
        assert!(Css::parse("tr:contains('x", "teste").is_err());
        assert!(Css::parse("tr:is(.a:contains('x'))", "teste").is_err());
        assert!(Css::parse("tr:contains('x') >", "teste").is_err());
        assert!(Css::parse("> tr:contains('x')", "teste").is_err());
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

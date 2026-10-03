//! Padrão com histórico de capturas, como o `Group.Captures` do .NET.
//!
//! O .NET guarda toda captura de um grupo dentro de uma repetição
//! (`(?:...(?<episode>\d+))+`); o `fancy-regex` só a última. A tabela do
//! parser de episódio precisa da primeira e da última, e de quantas foram.
//! A saída é marcar a repetição no padrão com `⟦ ⟧` (no lugar de `(?:` e `)`):
//! o padrão ganha um grupo que a envolve, e depois do casamento cada
//! repetição é refeita, uma a uma, a partir do começo do grupo.
//!
//! Isso supõe que cada repetição tomou o caminho preferido do corpo, o que
//! vale para os padrões da tabela; se a conta não fechar no fim do grupo,
//! o grupo cai para a última captura, que é o que o `fancy-regex` dá.
//!
//! Nome de grupo repetido no original (`episode` duas vezes) aparece aqui com
//! sufixo (`episode_2`): a consulta é pelo nome-base e junta todos.

use std::collections::HashMap;
use std::fmt::Write;

use fancy_regex::{Captures, Regex};

use crate::common::regex;

/// Faixa de bytes no texto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
}

struct Repeat {
    /// Grupo que envolve a repetição inteira no padrão principal.
    wrapper: String,
    /// O corpo de uma repetição, com as repetições aninhadas já envolvidas.
    body: Regex,
}

pub(crate) struct Pattern {
    regex: Regex,
    repeats: Vec<Repeat>,
    /// Repetições que envolvem cada grupo, da mais externa para a mais interna.
    chains: HashMap<String, Vec<usize>>,
}

/// Nome sem o sufixo `_N` que desfaz a repetição de nome do original.
fn base_name(name: &str) -> &str {
    match name.rsplit_once('_') {
        Some((base, digits))
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) =>
        {
            base
        }
        _ => name,
    }
}

/// Lê o quantificador que vem depois de `⟧` (`+`, `*`, `?` ou `{n,m}`, com
/// `?` opcional de preguiçoso) e deixa `i` no último caractere dele.
fn take_quantifier(chars: &[char], i: &mut usize) -> String {
    let mut quantifier = String::new();
    match chars.get(*i + 1) {
        Some(&c @ ('+' | '*' | '?')) => {
            quantifier.push(c);
            *i += 1;
        }
        Some('{') => {
            while let Some(&c) = chars.get(*i + 1) {
                quantifier.push(c);
                *i += 1;
                if c == '}' {
                    break;
                }
            }
        }
        _ => return quantifier,
    }
    if chars.get(*i + 1) == Some(&'?') {
        quantifier.push('?');
        *i += 1;
    }
    quantifier
}

impl Pattern {
    /// Compila um padrão sem diferenciar maiúsculas, como a tabela inteira.
    pub fn new(source: &str) -> Self {
        let mut expanded = String::new();
        let mut open: Vec<(usize, usize)> = Vec::new();
        let mut bodies: Vec<String> = Vec::new();
        let mut chains: HashMap<String, Vec<usize>> = HashMap::new();

        let chars: Vec<char> = source.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' => {
                    expanded.push('\\');
                    if let Some(next) = chars.get(i + 1) {
                        expanded.push(*next);
                    }
                    i += 2;
                    continue;
                }
                '⟦' => {
                    open.push((bodies.len(), expanded.len()));
                    bodies.push(String::new());
                }
                '⟧' => {
                    let (id, from) = open.pop().expect("⟧ sem ⟦");
                    let body = expanded.split_off(from);
                    let quantifier = take_quantifier(&chars, &mut i);
                    let _ = write!(expanded, "(?<l{id}>(?:{body}){quantifier})");
                    bodies[id] = body;
                }
                '(' if chars.get(i + 1) == Some(&'?')
                    && chars.get(i + 2) == Some(&'<')
                    && !matches!(chars.get(i + 3), Some('=' | '!')) =>
                {
                    let name: String = chars[i + 3..].iter().take_while(|c| **c != '>').collect();
                    if !open.is_empty() {
                        chains.insert(name, open.iter().map(|(id, _)| *id).collect());
                    }
                    expanded.push('(');
                }
                other => expanded.push(other),
            }
            i += 1;
        }
        assert!(open.is_empty(), "⟦ sem ⟧ em `{source}`");

        let repeats = bodies
            .iter()
            .enumerate()
            .map(|(id, body)| Repeat {
                wrapper: format!("l{id}"),
                body: regex(&format!("(?i){body}")),
            })
            .collect();
        Self {
            regex: regex(&format!("(?i){expanded}")),
            repeats,
            chains,
        }
    }

    /// Todos os casamentos no texto, como o `Regex.Matches`.
    pub fn matches<'p, 't>(&'p self, text: &'t str) -> Vec<Match<'p, 't>> {
        self.regex
            .captures_iter(text)
            .map_while(Result::ok)
            .map(|captures| Match {
                pattern: self,
                text,
                captures,
            })
            .collect()
    }
}

pub(crate) struct Match<'p, 't> {
    pattern: &'p Pattern,
    text: &'t str,
    captures: Captures<'t, str>,
}

impl<'t> Match<'_, 't> {
    /// Todas as capturas dos grupos de nome-base `base`, na ordem do texto.
    pub fn spans(&self, base: &str) -> Vec<Span> {
        let mut all = Vec::new();
        for name in self.pattern.regex.capture_names().flatten() {
            if base_name(name) != base {
                continue;
            }
            match self.pattern.chains.get(name) {
                None => all.extend(self.single(name)),
                Some(chain) => match self.history(name, chain) {
                    Some(found) => all.extend(found),
                    None => all.extend(self.single(name)),
                },
            }
        }
        all.sort();
        all.dedup();
        all
    }

    /// A última captura: o `Group.Value` do .NET.
    pub fn last(&self, base: &str) -> Option<Span> {
        self.spans(base).pop()
    }

    pub fn success(&self, base: &str) -> bool {
        self.last(base).is_some()
    }

    pub fn text(&self, span: Span) -> &'t str {
        &self.text[span.start..span.end]
    }

    /// Texto da última captura; vazio se o grupo não casou.
    pub fn value(&self, base: &str) -> &'t str {
        self.last(base).map_or("", |span| self.text(span))
    }

    fn single(&self, name: &str) -> Option<Span> {
        self.captures.name(name).map(|m| Span {
            start: m.start(),
            end: m.end(),
        })
    }

    /// Refaz as repetições da cadeia e devolve as capturas de `name`.
    fn history(&self, name: &str, chain: &[usize]) -> Option<Vec<Span>> {
        let wrapper = |id: usize| &self.pattern.repeats[id].wrapper;
        let mut spans: Vec<Span> = self.single(wrapper(chain[0])).into_iter().collect();
        for (level, id) in chain.iter().enumerate() {
            let mut iterations = Vec::new();
            for span in &spans {
                iterations.extend(self.replay(*id, *span)?);
            }
            let next = chain.get(level + 1).map_or(name, |next| wrapper(*next));
            spans = iterations
                .iter()
                .filter_map(|captures| {
                    captures.name(next).map(|m| Span {
                        start: m.start(),
                        end: m.end(),
                    })
                })
                .collect();
        }
        Some(spans)
    }

    /// As repetições de `id` dentro de `span`, uma a uma; `None` se não
    /// fecharem exatamente no fim.
    fn replay(&self, id: usize, span: Span) -> Option<Vec<Captures<'t, str>>> {
        let body = &self.pattern.repeats[id].body;
        let mut position = span.start;
        let mut iterations = Vec::new();
        while position < span.end {
            let captures = body.captures_from_pos(self.text, position).ok()??;
            let found = captures.get(0)?;
            if found.start() != position || found.end() == position || found.end() > span.end {
                return None;
            }
            position = found.end();
            iterations.push(captures);
        }
        (position == span.end).then_some(iterations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historico_de_repeticao() {
        let pattern = Pattern::new(r"^(?<title>.+?)⟦[-_. ]e(?<episode>\d{2})⟧+(?:$|\.)");
        let found = pattern.matches("Show-e01-e02-e03");
        let found = &found[0];
        let values: Vec<_> = found
            .spans("episode")
            .into_iter()
            .map(|s| found.text(s))
            .collect();
        assert_eq!(values, ["01", "02", "03"]);
        assert_eq!(found.value("title"), "Show");
    }

    #[test]
    fn repeticao_aninhada_e_nome_repetido() {
        let pattern =
            Pattern::new(r"^⟦\W*S(?<season>\d{2})⟦e(?<episode>\d{2})⟧+⟧{2,}|(?<season_2>x)");
        let found = pattern.matches("S01E01E02.S02E03");
        let found = &found[0];
        let seasons: Vec<_> = found
            .spans("season")
            .into_iter()
            .map(|s| found.text(s))
            .collect();
        let episodes: Vec<_> = found
            .spans("episode")
            .into_iter()
            .map(|s| found.text(s))
            .collect();
        assert_eq!(seasons, ["01", "02"]);
        assert_eq!(episodes, ["01", "02", "03"]);
    }
}

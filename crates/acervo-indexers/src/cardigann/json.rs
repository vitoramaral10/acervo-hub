//! Respostas JSON: caminhos e seletores do motor de referência.
//!
//! A referência lê JSON com o `SelectToken` do Newtonsoft e uma extensão
//! própria de `:has(...)`, `:not(...)` e `:contains(...)` sobre o caminho.
//! Aqui vale o subconjunto que as definições usam — chaves, índices e
//! `$` na raiz. Curinga, descendente recursivo e filtro de script ficam de
//! fora e são recusados na carga quando o seletor é literal.

use std::sync::LazyLock;

use serde_json::Value;

/// `:filtro(chave)` no fim do caminho, como o regex da referência. O
/// lookahead pede `:` ou o fim depois do `)`, o que deixa filtros aninhados
/// (`:has(:contains(x))`) inteiros na chave.
static FILTER: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r":(?<filter>.+?)\((?<key>.+?)\)(?=:|\z)").expect("regex fixa")
});

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

/// Caminho até um valor: `a.b[0].c`, `$.a`, `['a b']`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JsonPath(Vec<Step>);

impl JsonPath {
    /// `None` para sintaxe fora do subconjunto.
    pub fn parse(source: &str) -> Option<Self> {
        let source = source.trim();
        let mut rest = source.strip_prefix('$').unwrap_or(source);
        let mut steps = Vec::new();
        let mut first = !source.starts_with('$');
        while !rest.is_empty() {
            if let Some(tail) = rest.strip_prefix('[') {
                let end = tail.find(']')?;
                let inside = &tail[..end];
                rest = &tail[end + 1..];
                let quoted = ['\'', '"'].iter().find_map(|quote| {
                    inside
                        .strip_prefix(*quote)
                        .and_then(|inner| inner.strip_suffix(*quote))
                });
                steps.push(match quoted {
                    Some(key) if !key.is_empty() && !key.contains(['\'', '"']) => {
                        Step::Key(key.to_owned())
                    }
                    Some(_) => return None,
                    None => Step::Index(inside.parse().ok()?),
                });
            } else {
                let tail = if first { rest } else { rest.strip_prefix('.')? };
                let end = tail.find(['.', '[']).unwrap_or(tail.len());
                let key = &tail[..end];
                if key.is_empty() || key.contains(['*', '?', '@', '(', ')', ']', '\'', '"', '$']) {
                    return None;
                }
                steps.push(Step::Key(key.to_owned()));
                rest = &tail[end..];
            }
            first = false;
        }
        Some(Self(steps))
    }

    /// O valor no caminho; caminho vazio é a própria raiz.
    pub fn select<'a>(&self, root: &'a Value) -> Option<&'a Value> {
        self.0.iter().try_fold(root, |node, step| match step {
            Step::Key(key) => node.get(key.as_str()),
            Step::Index(index) => node.get(*index),
        })
    }
}

/// Parte do seletor antes do primeiro `:`: o caminho propriamente dito.
pub(super) fn path_part(selector: &str) -> &str {
    selector.split(':').next().unwrap_or("")
}

/// Lê o caminho do seletor e confere os filtros `:has`, `:not` e `:contains`
/// contra `parsed`. Devolve o caminho se o valor existe e passa nos filtros.
pub(super) fn field_selector(parsed: &Value, selector: &str) -> Option<String> {
    let path = path_part(selector);
    let node = if path.trim().is_empty() {
        parsed
    } else {
        JsonPath::parse(path)?.select(parsed)?
    };
    for captures in FILTER.captures_iter(selector).flatten() {
        let filter = captures.name("filter")?.as_str();
        let key = captures.name("key")?.as_str();
        match filter {
            "has" | "not" => {
                let present = if FILTER.is_match(key).unwrap_or(false) {
                    field_selector(node, key).is_some()
                } else {
                    JsonPath::parse(key).is_some_and(|path| path.select(node).is_some())
                };
                if present != (filter == "has") {
                    return None;
                }
            }
            "contains" => {
                if !display(node).contains(key) {
                    return None;
                }
            }
            // A referência registra e segue; aqui o seletor não casa.
            _ => return None,
        }
    }
    Some(path.to_owned())
}

/// Texto de um valor como o `JToken.ToString()` do .NET: texto sem aspas,
/// booleano `True`/`False`, objeto e lista em JSON indentado.
pub(super) fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => text.clone(),
        Value::Number(number) => {
            if number.is_f64()
                && let Some(float) = number.as_f64()
                && float.fract() == 0.0
                && float.abs() < 1e15
            {
                format!("{float:.0}")
            } else {
                number.to_string()
            }
        }
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string_pretty(value).unwrap_or_default()
        }
    }
}

/// `Value<string>()` da referência sobre o valor selecionado: lista vira os
/// itens separados por vírgula; nulo e objeto não rendem texto.
pub(super) fn token_text(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Object(_) => None,
        Value::Array(items) => Some(items.iter().map(display).collect::<Vec<_>>().join(",")),
        other => Some(display(other)),
    }
}

/// Os valores de `parent` que são objetos — o `Values<JObject>()` de
/// `rows.multiple`.
pub(super) fn object_children(parent: &Value) -> Vec<&Value> {
    match parent {
        Value::Array(items) => items.iter().filter(|item| item.is_object()).collect(),
        Value::Object(map) => map.values().filter(|item| item.is_object()).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn caminho_com_chaves_indices_e_raiz() {
        let json = json!({"a": {"b": [{"c": 1}, {"c": 2}]}, "x y": true});
        let get = |path: &str| JsonPath::parse(path).and_then(|p| p.select(&json).cloned());
        assert_eq!(get("a.b[1].c"), Some(json!(2)));
        assert_eq!(get("$.a.b[0].c"), Some(json!(1)));
        assert_eq!(get("['x y']"), Some(json!(true)));
        assert_eq!(get("a.nada"), None);
        assert_eq!(get("$"), Some(json));
        for fora in ["a..b", "a.*", "a[?(@.x)]", "a[*]", "$..a", "a[", "a.b."] {
            assert!(JsonPath::parse(fora).is_none(), "{fora}");
        }
    }

    #[test]
    fn filtros_has_not_e_contains_inclusive_aninhados() {
        let row = json!({"nome": "Série X", "promo": {"mult": "0.5"}, "link": "https://a.invalid"});
        let casa = |selector: &str| field_selector(&row, selector).is_some();
        assert!(casa(":has(promo)"));
        assert!(!casa(":has(ausente)"));
        assert!(casa(":not(ausente)"));
        assert!(!casa(":not(promo)"));
        assert!(casa("nome:contains(Série)"));
        assert!(!casa("nome:contains(Filme)"));
        assert!(casa(":has(promo:contains(0.5))"));
        assert!(!casa(":has(promo:contains(9))"));
        assert!(casa("link:has(:contains(https))"));
        assert!(casa(":not(promo:contains(9)):has(nome)"));
        assert!(!casa(":desconhecido(x)"));
    }

    #[test]
    fn texto_dos_valores_como_no_dotnet() {
        assert_eq!(token_text(&json!(true)).as_deref(), Some("True"));
        assert_eq!(token_text(&json!(25.0)).as_deref(), Some("25"));
        assert_eq!(token_text(&json!(1.5)).as_deref(), Some("1.5"));
        assert_eq!(
            token_text(&json!(["a", 2, false])).as_deref(),
            Some("a,2,False")
        );
        assert_eq!(token_text(&json!(null)), None);
        assert_eq!(token_text(&json!({"a": 1})), None);
    }

    #[test]
    fn filhos_objeto_de_lista_e_de_mapa() {
        let lista = json!([{"a": 1}, 3, {"b": 2}]);
        let mapa = json!({"x": {"a": 1}, "y": 4});
        assert_eq!(object_children(&lista).len(), 2);
        assert_eq!(object_children(&mapa).len(), 1);
    }
}

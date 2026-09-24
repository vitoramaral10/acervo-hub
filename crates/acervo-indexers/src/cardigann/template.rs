//! Subconjunto dos templates Go que as definições Cardigann usam.
//!
//! Um parser de verdade, não uma sequência de regex: condições aninhadas
//! (`or (eq .A "1") (or ...)`) aparecem nas definições reais e uma regex não
//! as avalia sem ambiguidade. Tudo é validado na compilação — variável
//! desconhecida, função fora da lista, `{{` sem fechamento — para que um erro
//! de definição apareça na subida e não na primeira busca.

use std::collections::HashMap;

use regex::Regex;

use super::invalid;
use crate::IndexerError;

/// Valor de uma variável no momento da avaliação.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Null,
    Bool(bool),
    Str(String),
    List(Vec<String>),
}

impl Value {
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::Str(value) => !value.is_empty(),
            Self::List(values) => !values.is_empty(),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Bool(value) => value.to_string(),
            Self::Str(value) => value.clone(),
            Self::List(values) => format!("[{}]", values.join(" ")),
        }
    }

    pub fn from_option(value: Option<String>) -> Self {
        value.map_or(Self::Null, Self::Str)
    }
}

/// Variáveis visíveis a um template, pelo nome completo (`.Config.x`).
#[derive(Debug, Default, Clone)]
pub(super) struct Vars(HashMap<String, Value>);

impl Vars {
    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        self.0.insert(name.into(), value);
    }

    fn get(&self, name: &str) -> Value {
        self.0.get(name).cloned().unwrap_or(Value::Null)
    }
}

/// Onde o template vive decide o que ele pode referenciar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// Inputs de busca e de login, `keywordsfilters`, seletor de linhas.
    Request,
    /// Campos de resultado: podem ler `.Result.*` dos campos anteriores.
    Field,
}

/// Nomes que a compilação aceita.
#[derive(Debug)]
pub(super) struct Names<'a> {
    pub settings: &'a [String],
    pub fields: &'a [String],
}

#[derive(Debug)]
pub(super) struct Template(Vec<Node>);

#[derive(Debug)]
enum Node {
    Text(String),
    Output(Expr),
    If {
        branches: Vec<(Expr, Vec<Node>)>,
        otherwise: Vec<Node>,
    },
    Range {
        over: Expr,
        body: Vec<Node>,
    },
}

#[derive(Debug)]
enum Expr {
    /// Caminho completo; `.` sozinho é o item corrente de um `range`.
    Var(String),
    Literal(String),
    Call(Func, Vec<Expr>),
    /// `re_replace` com padrão literal: compilado uma vez só.
    ReReplace(Box<Expr>, Regex, String),
}

#[derive(Debug, Clone, Copy)]
enum Func {
    Eq,
    Ne,
    And,
    Or,
    Not,
    Join,
    ReReplace,
}

impl Func {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "eq" => Self::Eq,
            "ne" => Self::Ne,
            "and" => Self::And,
            "or" => Self::Or,
            "not" => Self::Not,
            "join" => Self::Join,
            "re_replace" => Self::ReReplace,
            _ => return None,
        })
    }
}

impl Template {
    pub fn compile(source: &str, scope: Scope, names: &Names<'_>) -> Result<Self, IndexerError> {
        let pieces = split(source)?;
        let mut parser = Parser {
            pieces,
            position: 0,
            scope,
            names,
        };
        let (nodes, stop) = parser.nodes()?;
        if stop.is_some() {
            return Err(syntax("`else` ou `end` sem abertura"));
        }
        Ok(Self(nodes))
    }

    /// Template sem nenhuma ação: devolve o texto literal.
    pub fn literal(&self) -> Option<&str> {
        match self.0.as_slice() {
            [] => Some(""),
            [Node::Text(text)] => Some(text),
            _ => None,
        }
    }

    pub fn render(&self, vars: &Vars) -> String {
        let mut output = String::new();
        render(&self.0, vars, None, &mut output);
        output
    }

    /// Variáveis citadas, para conferir que as capacidades anunciadas são
    /// de fato consumidas por algum input.
    pub fn variables(&self) -> Vec<&str> {
        let mut found = Vec::new();
        collect_nodes(&self.0, &mut found);
        found
    }
}

fn render(nodes: &[Node], vars: &Vars, dot: Option<&str>, output: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text) => output.push_str(text),
            Node::Output(expr) => output.push_str(&eval(expr, vars, dot).text()),
            Node::If {
                branches,
                otherwise,
            } => {
                let chosen = branches
                    .iter()
                    .find(|(condition, _)| eval(condition, vars, dot).truthy())
                    .map_or(otherwise, |(_, body)| body);
                render(chosen, vars, dot, output);
            }
            Node::Range { over, body } => {
                let items = match eval(over, vars, dot) {
                    Value::List(items) => items,
                    Value::Str(item) => vec![item],
                    Value::Null | Value::Bool(_) => Vec::new(),
                };
                for item in &items {
                    render(body, vars, Some(item), output);
                }
            }
        }
    }
}

fn eval(expr: &Expr, vars: &Vars, dot: Option<&str>) -> Value {
    match expr {
        Expr::Var(name) if name == "." => dot.map_or(Value::Null, |item| Value::Str(item.into())),
        Expr::Var(name) => vars.get(name),
        Expr::Literal(value) => Value::Str(value.clone()),
        Expr::ReReplace(input, pattern, replacement) => Value::Str(
            pattern
                .replace_all(&eval(input, vars, dot).text(), replacement.as_str())
                .into_owned(),
        ),
        Expr::Call(func, args) => {
            let mut values = args.iter().map(|arg| eval(arg, vars, dot));
            match func {
                Func::Eq | Func::Ne => {
                    let first = values.next().unwrap_or(Value::Null).text();
                    let equal = values.any(|value| value.text() == first);
                    Value::Bool(equal == matches!(func, Func::Eq))
                }
                // Como em Go: devolvem o próprio argumento que decidiu.
                Func::And => {
                    let mut last = Value::Bool(true);
                    for value in values {
                        if !value.truthy() {
                            return value;
                        }
                        last = value;
                    }
                    last
                }
                Func::Or => {
                    let mut last = Value::Bool(false);
                    for value in values {
                        if value.truthy() {
                            return value;
                        }
                        last = value;
                    }
                    last
                }
                Func::Not => Value::Bool(!values.next().unwrap_or(Value::Null).truthy()),
                Func::Join => {
                    let list = values.next().unwrap_or(Value::Null);
                    let separator = values.next().map(|value| value.text()).unwrap_or_default();
                    match list {
                        Value::List(items) => Value::Str(items.join(&separator)),
                        other => Value::Str(other.text()),
                    }
                }
                Func::ReReplace => unreachable!("re_replace compila para Expr::ReReplace"),
            }
        }
    }
}

fn collect_nodes<'a>(nodes: &'a [Node], found: &mut Vec<&'a str>) {
    for node in nodes {
        match node {
            Node::Text(_) => {}
            Node::Output(expr) => collect_expr(expr, found),
            Node::If {
                branches,
                otherwise,
            } => {
                for (condition, body) in branches {
                    collect_expr(condition, found);
                    collect_nodes(body, found);
                }
                collect_nodes(otherwise, found);
            }
            Node::Range { over, body } => {
                collect_expr(over, found);
                collect_nodes(body, found);
            }
        }
    }
}

fn collect_expr<'a>(expr: &'a Expr, found: &mut Vec<&'a str>) {
    match expr {
        Expr::Var(name) => found.push(name),
        Expr::Literal(_) => {}
        Expr::ReReplace(input, _, _) => collect_expr(input, found),
        Expr::Call(_, args) => {
            for arg in args {
                collect_expr(arg, found);
            }
        }
    }
}

enum Piece {
    Text(String),
    Action(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Var(String),
    Str(String),
    Open,
    Close,
}

/// Separa texto de ações, respeitando `{{-` e `-}}`.
fn split(source: &str) -> Result<Vec<Piece>, IndexerError> {
    let mut pieces = Vec::new();
    let mut rest = source;
    let mut trim_next = false;
    while let Some(start) = rest.find("{{") {
        let mut text = &rest[..start];
        if trim_next {
            text = text.trim_start();
        }
        let inner_start = start + 2;
        let trim_before = rest[inner_start..].starts_with('-');
        if trim_before {
            text = text.trim_end();
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text.to_owned()));
        }
        let body_start = inner_start + usize::from(trim_before);
        let end = find_close(&rest[body_start..])
            .ok_or_else(|| syntax("`{{` sem `}}` correspondente"))?;
        let mut body = &rest[body_start..body_start + end];
        trim_next = body.ends_with('-') && body[..body.len() - 1].ends_with(char::is_whitespace);
        if trim_next {
            body = &body[..body.len() - 1];
        }
        pieces.push(Piece::Action(tokenize(body)?));
        rest = &rest[body_start + end + 2..];
    }
    let text = if trim_next { rest.trim_start() } else { rest };
    if text.contains("}}") && !text.is_empty() {
        // `}}` solto em texto é legal em Go, mas nas definições é sempre
        // sintoma de template quebrado.
        return Err(syntax("`}}` sem `{{` correspondente"));
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text.to_owned()));
    }
    Ok(pieces)
}

/// Acha o `}}` que fecha a ação, ignorando os que estão dentro de strings.
fn find_close(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut index = 0;
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(b'"') if byte == b'\\' => index += 1,
            Some(open) if byte == open => quote = None,
            None if byte == b'"' || byte == b'`' => quote = Some(byte),
            None if byte == b'}' && bytes.get(index + 1) == Some(&b'}') => return Some(index),
            _ => {}
        }
        index += 1;
    }
    None
}

fn tokenize(body: &str) -> Result<Vec<Token>, IndexerError> {
    let mut tokens = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some(&(start, character)) = chars.peek() {
        match character {
            _ if character.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                tokens.push(Token::Open);
            }
            ')' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut value = String::new();
                loop {
                    match chars.next() {
                        // Só `\"` é escape: o resto chega cru, porque é quase
                        // sempre regex (`"[\s]+"`) e a referência não o
                        // desescapa.
                        Some((_, '\\')) => match chars.next() {
                            Some((_, '"')) => value.push('"'),
                            Some((_, escaped)) => {
                                value.push('\\');
                                value.push(escaped);
                            }
                            None => return Err(syntax("string não terminada")),
                        },
                        Some((_, '"')) => break,
                        Some((_, other)) => value.push(other),
                        None => return Err(syntax("string não terminada")),
                    }
                }
                tokens.push(Token::Str(value));
            }
            '`' => {
                chars.next();
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some((_, '`')) => break,
                        Some((_, other)) => value.push(other),
                        None => return Err(syntax("string não terminada")),
                    }
                }
                tokens.push(Token::Str(value));
            }
            '|' => return Err(syntax("pipeline `|` não suportado")),
            _ => {
                let mut end = start;
                while let Some(&(index, next)) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '(' | ')' | '"' | '`' | '|') {
                        break;
                    }
                    end = index + next.len_utf8();
                    chars.next();
                }
                let word = &body[start..end];
                tokens.push(if word.starts_with('.') {
                    Token::Var(word.to_owned())
                } else {
                    Token::Word(word.to_owned())
                });
            }
        }
    }
    Ok(tokens)
}

enum Stop {
    Else(Option<Expr>),
    End,
}

struct Parser<'a> {
    pieces: Vec<Piece>,
    position: usize,
    scope: Scope,
    names: &'a Names<'a>,
}

impl Parser<'_> {
    fn nodes(&mut self) -> Result<(Vec<Node>, Option<Stop>), IndexerError> {
        let mut nodes = Vec::new();
        while self.position < self.pieces.len() {
            let piece =
                std::mem::replace(&mut self.pieces[self.position], Piece::Text(String::new()));
            self.position += 1;
            let tokens = match piece {
                Piece::Text(text) => {
                    nodes.push(Node::Text(text));
                    continue;
                }
                Piece::Action(tokens) => tokens,
            };
            match tokens.first() {
                Some(Token::Word(word)) if word == "if" => {
                    let condition = self.expr(&tokens[1..])?;
                    nodes.push(self.conditional(condition)?);
                }
                Some(Token::Word(word)) if word == "range" => {
                    let over = self.expr(&tokens[1..])?;
                    let (body, stop) = self.nodes()?;
                    if !matches!(stop, Some(Stop::End)) {
                        return Err(syntax("`range` sem `end`"));
                    }
                    nodes.push(Node::Range { over, body });
                }
                Some(Token::Word(word)) if word == "else" => {
                    let condition = match tokens.get(1) {
                        None => None,
                        Some(Token::Word(word)) if word == "if" => Some(self.expr(&tokens[2..])?),
                        Some(_) => return Err(syntax("`else` seguido de algo que não é `if`")),
                    };
                    return Ok((nodes, Some(Stop::Else(condition))));
                }
                Some(Token::Word(word)) if word == "end" && tokens.len() == 1 => {
                    return Ok((nodes, Some(Stop::End)));
                }
                Some(Token::Word(word))
                    if matches!(word.as_str(), "with" | "define" | "template" | "block") =>
                {
                    return Err(syntax("ação de template não suportada"));
                }
                None => return Err(syntax("ação vazia")),
                Some(_) => nodes.push(Node::Output(self.expr(&tokens)?)),
            }
        }
        Ok((nodes, None))
    }

    fn conditional(&mut self, first: Expr) -> Result<Node, IndexerError> {
        let mut branches = Vec::new();
        let mut condition = first;
        loop {
            let (body, stop) = self.nodes()?;
            branches.push((condition, body));
            match stop {
                Some(Stop::End) => {
                    return Ok(Node::If {
                        branches,
                        otherwise: Vec::new(),
                    });
                }
                Some(Stop::Else(Some(next))) => condition = next,
                Some(Stop::Else(None)) => {
                    let (otherwise, stop) = self.nodes()?;
                    if !matches!(stop, Some(Stop::End)) {
                        return Err(syntax("`if` sem `end`"));
                    }
                    return Ok(Node::If {
                        branches,
                        otherwise,
                    });
                }
                None => return Err(syntax("`if` sem `end`")),
            }
        }
    }

    fn expr(&self, tokens: &[Token]) -> Result<Expr, IndexerError> {
        let mut position = 0;
        let expr = self.command(tokens, &mut position)?;
        // `)` sobrando no fim da ação: há definição real assim, e o motor de
        // referência (regex) a aceita. Sem ambiguidade — nada vem depois.
        while tokens.get(position) == Some(&Token::Close) {
            position += 1;
        }
        if position != tokens.len() {
            return Err(syntax("argumentos sobrando na ação"));
        }
        Ok(expr)
    }

    /// `func arg...` ou um operando sozinho. Consome até o fim ou até `)`.
    fn command(&self, tokens: &[Token], position: &mut usize) -> Result<Expr, IndexerError> {
        match tokens.get(*position) {
            Some(Token::Word(word)) => {
                let func = Func::parse(word).ok_or_else(|| syntax("função não suportada"))?;
                *position += 1;
                let mut args = Vec::new();
                while let Some(token) = tokens.get(*position) {
                    if *token == Token::Close {
                        break;
                    }
                    // Um nome de função no meio dos argumentos — `or eq .A "8"`,
                    // que aparece em definição real — é lido como chamada que
                    // consome o resto. Em Go seria erro; aqui é a única leitura
                    // que corresponde à intenção de quem escreveu.
                    if matches!(token, Token::Word(_)) {
                        args.push(self.command(tokens, position)?);
                        break;
                    }
                    args.push(self.operand(tokens, position)?);
                }
                self.call(func, args)
            }
            Some(_) => {
                let operand = self.operand(tokens, position)?;
                match tokens.get(*position) {
                    None | Some(Token::Close) => Ok(operand),
                    Some(_) => Err(syntax("operando seguido de argumentos")),
                }
            }
            None => Err(syntax("expressão vazia")),
        }
    }

    fn operand(&self, tokens: &[Token], position: &mut usize) -> Result<Expr, IndexerError> {
        let token = tokens
            .get(*position)
            .ok_or_else(|| syntax("expressão vazia"))?;
        *position += 1;
        match token {
            Token::Var(name) => {
                self.check_variable(name)?;
                Ok(Expr::Var(name.clone()))
            }
            Token::Str(value) => Ok(Expr::Literal(value.clone())),
            Token::Word(word) if word.parse::<f64>().is_ok() => Ok(Expr::Literal(word.clone())),
            Token::Open => {
                let expr = self.command(tokens, position)?;
                if tokens.get(*position) != Some(&Token::Close) {
                    return Err(syntax("`(` sem `)`"));
                }
                *position += 1;
                Ok(expr)
            }
            Token::Close | Token::Word(_) => Err(syntax("argumento inválido")),
        }
    }

    fn call(&self, func: Func, mut args: Vec<Expr>) -> Result<Expr, IndexerError> {
        let arity_ok = match func {
            Func::Eq | Func::Ne => args.len() >= 2,
            Func::And | Func::Or => !args.is_empty(),
            Func::Not => args.len() == 1,
            Func::Join => args.len() == 2,
            Func::ReReplace => args.len() == 3,
        };
        if !arity_ok {
            return Err(syntax("número de argumentos inválido"));
        }
        if let Func::ReReplace = func {
            let replacement = args.pop();
            let pattern = args.pop();
            let input = args.pop();
            let (Some(input), Some(Expr::Literal(pattern)), Some(Expr::Literal(replacement))) =
                (input, pattern, replacement)
            else {
                return Err(syntax("re_replace exige padrão e substituição literais"));
            };
            return Ok(Expr::ReReplace(
                Box::new(input),
                compile_regex(&pattern, "template")?,
                dotnet_replacement(&replacement),
            ));
        }
        let _ = self;
        Ok(Expr::Call(func, args))
    }

    fn check_variable(&self, name: &str) -> Result<(), IndexerError> {
        let known = name == "."
            || matches!(
                name,
                ".Keywords"
                    | ".Query.Keywords"
                    | ".Query.Q"
                    | ".Query.Season"
                    | ".Query.Ep"
                    | ".Query.Episode"
                    | ".Query.Year"
                    | ".Query.IMDBID"
                    | ".Query.IMDBIDShort"
                    | ".Query.TMDBID"
                    | ".Query.TVDBID"
                    | ".Categories"
                    | ".True"
                    | ".False"
                    | ".Today.Year"
            )
            || name
                .strip_prefix(".Config.")
                .is_some_and(|setting| self.names.settings.iter().any(|known| known == setting))
            || (self.scope == Scope::Field
                && name
                    .strip_prefix(".Result.")
                    .is_some_and(|field| self.names.fields.iter().any(|known| known == field)));
        if known {
            Ok(())
        } else {
            Err(invalid(
                "template",
                "variável desconhecida ou fora de escopo",
            ))
        }
    }
}

/// Regex das definições, com teto de tamanho: a definição é arquivo de fora.
pub(super) fn compile_regex(pattern: &str, section: &'static str) -> Result<Regex, IndexerError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|_| {
            invalid(
                section,
                "regex inválida ou com recurso não suportado (lookaround, backreference)",
            )
        })
}

/// `$1` do .NET vira `${1}`: em Rust, `$1abc` seria o grupo chamado `1abc`.
pub(super) fn dotnet_replacement(replacement: &str) -> String {
    let mut output = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
            output.push_str("${");
            while let Some(digit) = chars.next_if(char::is_ascii_digit) {
                output.push(digit);
            }
            output.push('}');
        } else if character == '$' {
            output.push_str("$$");
        } else {
            output.push(character);
        }
    }
    output
}

fn syntax(reason: &'static str) -> IndexerError {
    invalid("template", reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> (Vec<String>, Vec<String>) {
        (
            vec!["freeleech".into(), "sort".into()],
            vec!["x".into(), "cat".into(), "a".into(), "b".into()],
        )
    }

    fn render(source: &str, vars: &Vars) -> String {
        let (settings, fields) = names();
        Template::compile(
            source,
            Scope::Field,
            &Names {
                settings: &settings,
                fields: &fields,
            },
        )
        .unwrap()
        .render(vars)
    }

    fn compiles(source: &str, scope: Scope) -> bool {
        let (settings, fields) = names();
        Template::compile(
            source,
            scope,
            &Names {
                settings: &settings,
                fields: &fields,
            },
        )
        .is_ok()
    }

    #[test]
    fn if_else_com_configuracao() {
        let mut vars = Vars::default();
        let source = "{{ if .Config.freeleech }}1{{ else }}2{{ end }}";
        assert_eq!(render(source, &vars), "2");
        vars.set(".Config.freeleech", Value::Bool(true));
        assert_eq!(render(source, &vars), "1");
    }

    #[test]
    fn range_sobre_categorias() {
        let mut vars = Vars::default();
        vars.set(".Categories", Value::List(vec!["1".into(), "14".into()]));
        assert_eq!(
            render("{{ range .Categories }}filter_cat[{{.}}]=1&{{end}}", &vars),
            "filter_cat[1]=1&filter_cat[14]=1&"
        );
    }

    #[test]
    fn condicao_aninhada_em_varias_linhas_e_o_atalho_or_eq() {
        let source = "{{ if\n    or (eq .Result.cat \"1\")\n   (or (eq .Result.cat \"2\")\n   (or eq .Result.cat \"8\"))\n }}filme{{ else }}outro{{ end }}";
        let mut vars = Vars::default();
        for (cat, expected) in [
            ("1", "filme"),
            ("2", "filme"),
            ("8", "filme"),
            ("3", "outro"),
        ] {
            vars.set(".Result.cat", Value::Str(cat.into()));
            assert_eq!(render(source, &vars), expected, "{cat}");
        }
        vars.set(".Result.cat", Value::Null);
        assert_eq!(render(source, &vars), "outro");
    }

    #[test]
    fn re_replace_com_replacement_do_dotnet() {
        let mut vars = Vars::default();
        vars.set(".Keywords", Value::Str("uma  série boa".into()));
        assert_eq!(
            render(r#"{{ re_replace .Keywords "[\s]+" "%" }}"#, &vars),
            "uma%série%boa"
        );
        assert_eq!(dotnet_replacement("$2$3"), "${2}${3}");
        assert_eq!(dotnet_replacement(" 0$1x"), " 0${1}x");
    }

    #[test]
    fn if_com_else_if_e_trim() {
        let mut vars = Vars::default();
        vars.set(".Result.b", Value::Str("sim".into()));
        let source = "a {{- if .Result.a -}} A {{- else if .Result.b }} B{{ else }}C{{ end }}";
        assert_eq!(render(source, &vars), "a B");
    }

    #[test]
    fn eq_com_string_entre_aspas_simples_do_yaml() {
        let mut vars = Vars::default();
        vars.set(".Result.x", Value::Str("Dublado".into()));
        assert_eq!(
            render(
                r#"{{ if eq .Result.x "Dublado" }} Dublado Brazilian{{ else }}{{ end }}"#,
                &vars
            ),
            " Dublado Brazilian"
        );
    }

    #[test]
    fn nomes_desconhecidos_e_escopo_sao_recusados_na_compilacao() {
        assert!(!compiles("{{ .Config.unknown }}", Scope::Request));
        assert!(!compiles("{{ .Result.x }}", Scope::Request));
        assert!(compiles("{{ .Result.x }}", Scope::Field));
        assert!(!compiles("{{ .Result.nao_declarado }}", Scope::Field));
        assert!(!compiles("{{ printf \"%s\" .Keywords }}", Scope::Request));
        assert!(!compiles("{{ .Keywords | html }}", Scope::Request));
        assert!(!compiles("{{ if .Keywords }}sem fim", Scope::Request));
        assert!(!compiles("{{ end }}", Scope::Request));
        assert!(!compiles("{{ .Keywords", Scope::Request));
        assert!(!compiles("texto }}", Scope::Request));
        assert!(!compiles(
            "{{ re_replace .Keywords .Keywords \"x\" }}",
            Scope::Request
        ));
        assert!(!compiles(
            "{{ re_replace .Keywords \"(?=x)\" \"\" }}",
            Scope::Request
        ));
    }

    #[test]
    fn chaves_dentro_de_string_nao_fecham_a_acao() {
        let vars = Vars::default();
        assert_eq!(render(r#"{{ if eq "}}" "}}" }}ok{{ end }}"#, &vars), "ok");
    }
}

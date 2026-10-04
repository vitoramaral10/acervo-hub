//! Filtros de campo, de `keywordsfilters`, e a interpretação de tamanho,
//! contagem e data — com a semântica do motor Cardigann de referência.

use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use super::charset::Charset;
use super::dates::{fuzzy, time_ago};
use super::definition::RawFilter;
use super::invalid;
use super::template::{Names, Pattern, Scope, Template, Vars, compile_regex, dotnet_replacement};
use crate::IndexerError;

#[derive(Debug)]
pub(super) enum Filter {
    Trim(Option<String>),
    Replace(String, Template),
    ReReplace(Pattern, Template),
    Regexp(Pattern),
    /// `jsonjoinarray`: caminho `$.a.b` num objeto JSON e separador.
    JsonJoinArray(Vec<String>, String),
    Append(Template),
    Prepend(Template),
    Split(char, isize),
    QueryString(String),
    DateParse(Vec<Token>),
    ToLower,
    ToUpper,
    UrlDecode(Charset),
    UrlEncode(Charset),
    HtmlDecode,
    HtmlEncode,
    /// `timeago` e `reltime`: "2 hours ago" → data.
    TimeAgo,
    FuzzyTime,
    ValidFileName,
    /// `diacritics: replace`: letra acentuada vira a letra-base.
    Diacritics,
    /// `validate`: fica só o que está na lista de termos aceitos.
    Validate(Vec<String>),
    /// Diagnóstico da referência (`strdump`, `hexdump`): não muda o valor.
    Noop,
}

impl Filter {
    pub fn compile(raw: RawFilter, scope: Scope, names: &Names<'_>) -> Result<Self, IndexerError> {
        let args = raw.args;
        let template = |value: &str| Template::compile(value, scope, names);
        let one = || match args.as_slice() {
            [value] => Ok(value.clone()),
            _ => Err(invalid("filters", "argumentos inválidos para o filtro")),
        };
        Ok(match (raw.name.as_str(), args.as_slice()) {
            ("trim", []) => Self::Trim(None),
            ("trim", [chars]) => Self::Trim(Some(chars.clone())),
            ("replace", [from, to]) if !from.is_empty() => {
                Self::Replace(from.clone(), template(to)?)
            }
            ("re_replace", [pattern, to]) => Self::ReReplace(
                compile_regex(pattern, "filters.re_replace")?,
                template(&dotnet_replacement(to))?,
            ),
            ("regexp", [pattern]) => Self::Regexp(compile_regex(pattern, "filters.regexp")?),
            ("append", _) => Self::Append(template(&one()?)?),
            ("prepend", _) => Self::Prepend(template(&one()?)?),
            ("split", [separator, index]) => Self::Split(
                separator
                    .chars()
                    .next()
                    .ok_or_else(|| invalid("filters.split", "separador vazio"))?,
                index
                    .parse()
                    .map_err(|_| invalid("filters.split", "índice inteiro obrigatório"))?,
            ),
            ("jsonjoinarray", [path, separator]) => {
                Self::JsonJoinArray(json_path(path)?, separator.clone())
            }
            ("querystring", [name]) => Self::QueryString(name.clone()),
            ("dateparse" | "timeparse", [layout]) => Self::DateParse(layout_tokens(layout)?),
            ("tolower", []) => Self::ToLower,
            ("toupper", []) => Self::ToUpper,
            ("urldecode", []) => Self::UrlDecode(names.charset),
            ("urlencode", _) => Self::UrlEncode(names.charset),
            ("htmldecode", _) => Self::HtmlDecode,
            ("htmlencode", _) => Self::HtmlEncode,
            ("timeago" | "reltime", _) => Self::TimeAgo,
            ("fuzzytime", _) => Self::FuzzyTime,
            ("validfilename", _) => Self::ValidFileName,
            ("diacritics", [operation]) if operation == "replace" => Self::Diacritics,
            ("validate", [terms]) => Self::Validate(words(terms)),
            ("strdump" | "hexdump", _) => Self::Noop,
            _ => {
                return Err(invalid(
                    "filters",
                    "filtro não implementado ou com argumentos inválidos",
                ));
            }
        })
    }

    /// Aplica o filtro. `Err(())` é "o valor não serve" — o campo falhou.
    pub fn apply(&self, value: String, vars: &Vars) -> Result<String, ()> {
        Ok(match self {
            Self::Trim(None) => value.trim().to_owned(),
            Self::Trim(Some(chars)) => value
                .trim_matches(|character| chars.contains(character))
                .to_owned(),
            Self::Replace(from, to) => value.replace(from.as_str(), &to.render(vars)),
            Self::ReReplace(pattern, to) => pattern.replace_all(&value, &to.render(vars))?,
            // Como na referência: sempre o primeiro grupo; sem casar, vazio.
            Self::Regexp(pattern) => pattern.first_group(&value).unwrap_or_default(),
            Self::JsonJoinArray(path, separator) => json_join(&value, path, separator).ok_or(())?,
            Self::Append(suffix) => value + &suffix.render(vars),
            Self::Prepend(prefix) => prefix.render(vars) + &value,
            Self::Split(separator, index) => {
                let parts: Vec<_> = value.split(*separator).collect();
                let position = if *index < 0 {
                    parts.len().checked_sub(index.unsigned_abs()).ok_or(())?
                } else {
                    usize::try_from(*index).map_err(|_| ())?
                };
                (*parts.get(position).ok_or(())?).to_owned()
            }
            Self::QueryString(name) => query_value(&value, name).unwrap_or_default(),
            Self::DateParse(layout) => parse_with_layout(value.trim(), layout)
                .ok_or(())?
                .format(&Rfc3339)
                .map_err(|_| ())?,
            Self::Noop => value,
            Self::ToLower => value.to_lowercase(),
            Self::ToUpper => value.to_uppercase(),
            Self::UrlDecode(charset) => charset.url_decode(&value),
            Self::UrlEncode(charset) => charset.url_encode(&value),
            Self::HtmlDecode => html_decode(&value),
            Self::HtmlEncode => html_encode(&value),
            Self::TimeAgo => time_ago(&value, OffsetDateTime::now_utc())
                .ok_or(())?
                .format(&Rfc3339)
                .map_err(|_| ())?,
            Self::FuzzyTime => fuzzy(&value, OffsetDateTime::now_utc())
                .ok_or(())?
                .format(&Rfc3339)
                .map_err(|_| ())?,
            Self::ValidFileName => valid_file_name(&value),
            Self::Diacritics => value.chars().map(strip_diacritic).collect(),
            Self::Validate(accepted) => {
                let present = words(&value);
                accepted
                    .iter()
                    .filter(|term| present.contains(term))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
    }
}

/// Termos do filtro `validate`: minúsculos, separados pelos delimitadores da
/// referência, sem repetição.
fn words(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text
        .to_lowercase()
        .split(|c: char| ", /)(.;[]\"|:".contains(c))
        .map(str::trim)
        .filter(|word| !word.is_empty())
    {
        if !found.iter().any(|known| known == word) {
            found.push(word.to_owned());
        }
    }
    found
}

/// `WebUtility.HtmlDecode` para as entidades que aparecem em página de
/// tracker: as cinco do XML, `&nbsp;` e as numéricas. O resto fica como veio.
fn html_decode(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest.find(';').filter(|end| *end <= 10).and_then(|end| {
            let entity = &rest[1..end];
            let character = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity
                    .strip_prefix('#')
                    .and_then(|number| match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => number.parse().ok(),
                    })
                    .and_then(char::from_u32),
            }?;
            Some((character, end + 1))
        });
        if let Some((character, consumed)) = decoded {
            output.push(character);
            rest = &rest[consumed..];
        } else {
            output.push('&');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

/// `WebUtility.HtmlEncode`: `<>&"'` e tudo acima de Latin-1 vira entidade.
fn html_encode(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            c if (' '..='~').contains(&c) || c.is_control() => output.push(c),
            c => {
                let _ =
                    std::fmt::Write::write_fmt(&mut output, format_args!("&#{};", u32::from(c)));
            }
        }
    }
    output
}

/// Caracteres que o Windows não aceita em nome de arquivo viram `_`; nome
/// vazio vira `_`, como o `MakeValidFileName` da referência.
fn valid_file_name(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_control() || "\"<>|:*?\\/".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Letra acentuada do Latin-1 e do Latin Estendido-A → letra-base. O que não
/// se decompõe (`Đ`, `ł`, `ø`) fica, como no filtro da referência.
fn strip_diacritic(character: char) -> char {
    const TABLE: [(&str, char); 38] = [
        ("ÀÁÂÃÄÅĀĂĄǍ", 'A'),
        ("àáâãäåāăąǎ", 'a'),
        ("ÇĆĈĊČ", 'C'),
        ("çćĉċč", 'c'),
        ("Ď", 'D'),
        ("ď", 'd'),
        ("ÈÉÊËĒĔĖĘĚ", 'E'),
        ("èéêëēĕėęě", 'e'),
        ("ĜĞĠĢ", 'G'),
        ("ĝğġģ", 'g'),
        ("Ĥ", 'H'),
        ("ĥ", 'h'),
        ("ÌÍÎÏĨĪĬĮİǏ", 'I'),
        ("ìíîïĩīĭįǐ", 'i'),
        ("Ĵ", 'J'),
        ("ĵ", 'j'),
        ("Ķ", 'K'),
        ("ķ", 'k'),
        ("ĹĻĽ", 'L'),
        ("ĺļľ", 'l'),
        ("ÑŃŅŇ", 'N'),
        ("ñńņň", 'n'),
        ("ÒÓÔÕÖŌŎŐǑ", 'O'),
        ("òóôõöōŏőǒ", 'o'),
        ("ŔŖŘ", 'R'),
        ("ŕŗř", 'r'),
        ("ŚŜŞŠ", 'S'),
        ("śŝşš", 's'),
        ("ŢŤ", 'T'),
        ("ţť", 't'),
        ("ÙÚÛÜŨŪŬŮŰŲǓ", 'U'),
        ("ùúûüũūŭůűųǔ", 'u'),
        ("Ŵ", 'W'),
        ("ŵ", 'w'),
        ("ÝŶŸ", 'Y'),
        ("ýÿŷ", 'y'),
        ("ŹŻŽ", 'Z'),
        ("źżž", 'z'),
    ];
    TABLE
        .iter()
        .find(|(accented, _)| accented.contains(character))
        .map_or(character, |(_, base)| *base)
}

/// Só o subconjunto que as definições usam: `$` seguido de chaves.
fn json_path(path: &str) -> Result<Vec<String>, IndexerError> {
    let refused = || {
        invalid(
            "filters.jsonjoinarray",
            "caminho JSON fora de `$.chave.chave`",
        )
    };
    let keys = path.strip_prefix('$').ok_or_else(refused)?;
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = keys
        .strip_prefix('.')
        .ok_or_else(refused)?
        .split('.')
        .map(str::to_owned)
        .collect();
    if keys
        .iter()
        .any(|key| key.is_empty() || key.contains(['[', ']', '*', '\'', '"']))
    {
        return Err(refused());
    }
    Ok(keys)
}

/// Como na referência: o valor no caminho é uma lista, e os itens escalares
/// se juntam pelo separador. Texto que não é JSON, ou caminho que não leva a
/// uma lista, faz o campo falhar.
fn json_join(value: &str, path: &[String], separator: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(value).ok()?;
    let items = path
        .iter()
        .try_fold(&json, |node, key| node.get(key))?
        .as_array()?;
    let parts: Vec<String> = items
        .iter()
        .map(|item| match item {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect();
    Some(parts.join(separator))
}

fn query_value(value: &str, name: &str) -> Option<String> {
    let query = value.split_once('?').map_or(value, |(_, query)| query);
    let query = query.split_once('#').map_or(query, |(query, _)| query);
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, found)| found.into_owned())
}

/// Tamanho no formato dos sites: `1.5 GB`, `1,5 GiB`, `1.234,56 MB`, `42 B`.
///
/// Vírgula vira ponto e, havendo vários pontos, só o último é decimal — a
/// mesma normalização da referência, para que os dois motores leiam o mesmo
/// número. Aritmética inteira: nada de arredondar `u64::MAX` ou aceitar `NaN`.
pub(super) fn parse_size(value: &str) -> Option<u64> {
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    let unit_start = compact
        .find(|c: char| c.is_alphabetic())
        .unwrap_or(compact.len());
    let (number, unit) = compact.split_at(unit_start);
    let unit = unit.to_ascii_lowercase().replace('i', "");
    let factor: u128 = match unit.as_str() {
        "" | "b" | "bytes" => 1,
        "kb" => 1 << 10,
        "mb" => 1 << 20,
        "gb" => 1 << 30,
        "tb" => 1 << 40,
        "pb" => 1 << 50,
        _ => return None,
    };
    if number.is_empty()
        || !number
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    let normalized = number.replace(',', ".");
    let (whole, fraction) = normalized.rsplit_once('.').unwrap_or((&normalized, ""));
    let whole: String = whole.chars().filter(char::is_ascii_digit).collect();
    if whole.is_empty() || fraction.len() > 18 {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?.checked_mul(factor)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?.checked_mul(factor)?
            / 10_u128.pow(u32::try_from(fraction.len()).ok()?)
    };
    u64::try_from(whole.checked_add(fraction)?).ok()
}

/// Contagem: só os dígitos contam (`1,234` e `1.234` são mil e pouco).
/// Célula sem dígito nenhum (`-`, `—`) é "não informado", não erro.
pub(super) fn parse_count(value: &str) -> Result<Option<u32>, ()> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return Ok(None);
    }
    digits.parse().map(Some).map_err(|_| ())
}

/// Data sem `dateparse`: as formas que os sites e o próprio filtro produzem.
/// Sem fuso no texto, assume UTC.
pub(super) fn parse_date(value: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("now") || value.eq_ignore_ascii_case("today") {
        return Some(now);
    }
    if let Ok(seconds) = value.parse::<i64>() {
        return OffsetDateTime::from_unix_timestamp(seconds).ok();
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .or_else(|_| OffsetDateTime::parse(value, &Rfc2822))
        .ok()
        .or_else(|| {
            ["yyyy-MM-dd HH:mm:ss", "yyyy-MM-dd HH:mm", "yyyy-MM-dd"]
                .iter()
                .find_map(|layout| parse_with_layout(value, &layout_tokens(layout).ok()?))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    Year4,
    Year2,
    MonthName,
    Month2,
    Month1,
    Day2,
    Day1,
    /// `ddd` e `dddd`: nome do dia da semana, lido e descartado.
    DayName,
    Hour24,
    Hour12,
    Minute,
    Second,
    Meridiem,
    /// `z`, `zz` e `zzz` do .NET: `+1`, `+01`, `+01:00`.
    Offset,
    Literal(char),
}

/// Aceita o layout .NET (`dd/MM/yy HH:mm:ss`) e o layout Go
/// (`02/01/06 15:04:05`): a referência converte o segundo no primeiro.
fn layout_tokens(layout: &str) -> Result<Vec<Token>, IndexerError> {
    let layout = if layout.chars().any(|c| c.is_ascii_digit()) {
        go_to_dotnet(layout)
    } else {
        layout.to_owned()
    };
    let chars: Vec<char> = layout.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        let run = chars[index..].iter().take_while(|c| **c == current).count();
        let token = match (current, run) {
            ('y', 4) => Token::Year4,
            ('y', 2) => Token::Year2,
            ('M', 3 | 4) => Token::MonthName,
            ('M', 2) => Token::Month2,
            ('M', 1) => Token::Month1,
            ('d', 3 | 4) => Token::DayName,
            ('d', 2) => Token::Day2,
            ('d', 1) => Token::Day1,
            ('H', 1 | 2) => Token::Hour24,
            ('h', 1 | 2) => Token::Hour12,
            ('m', 1 | 2) => Token::Minute,
            ('s', 1 | 2) => Token::Second,
            ('t', 2) => Token::Meridiem,
            ('z', 1..=3) => Token::Offset,
            // `\a` é a letra `a` e não o formato.
            ('\\', _) => {
                let literal = chars
                    .get(index + 1)
                    .ok_or_else(|| invalid("filters.dateparse", "escape sem caractere"))?;
                tokens.push(Token::Literal(*literal));
                index += 2;
                continue;
            }
            ('\'', _) => {
                let end = chars[index + 1..]
                    .iter()
                    .position(|c| *c == '\'')
                    .ok_or_else(|| invalid("filters.dateparse", "literal sem fechamento"))?;
                tokens.extend(
                    chars[index + 1..index + 1 + end]
                        .iter()
                        .map(|c| Token::Literal(*c)),
                );
                index += end + 2;
                continue;
            }
            (c, _) if c.is_ascii_alphabetic() => {
                return Err(invalid(
                    "filters.dateparse",
                    "token de data não suportado (dia da semana, fração, ...)",
                ));
            }
            (c, _) => {
                tokens.push(Token::Literal(c));
                index += 1;
                continue;
            }
        };
        tokens.push(token);
        index += run;
    }
    Ok(tokens)
}

fn go_to_dotnet(layout: &str) -> String {
    let mut output = layout.to_owned();
    for (go, dotnet) in [
        ("2006", "yyyy"),
        ("January", "MMMM"),
        ("Jan", "MMM"),
        ("15", "HH"),
        ("01", "MM"),
        ("02", "dd"),
        ("_2", "d"),
        ("03", "hh"),
        ("04", "mm"),
        ("05", "ss"),
        ("06", "yy"),
        ("PM", "tt"),
        ("pm", "tt"),
        // Os de um dígito por último: a esta altura só sobraram eles.
        ("1", "M"),
        ("2", "d"),
        ("3", "h"),
        ("4", "m"),
        ("5", "s"),
    ] {
        output = output.replace(go, dotnet);
    }
    output
}

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn parse_with_layout(value: &str, layout: &[Token]) -> Option<OffsetDateTime> {
    let mut rest = value;
    let (mut year, mut month, mut day) = (None, None, None);
    let (mut hour, mut minute, mut second, mut pm) = (0_u8, 0_u8, 0_u8, None);
    let mut offset = UtcOffset::UTC;
    let digits = |rest: &mut &str, min: usize, max: usize| -> Option<u32> {
        let count = rest
            .chars()
            .take(max)
            .take_while(char::is_ascii_digit)
            .count();
        if count < min {
            return None;
        }
        let (number, tail) = rest.split_at(count);
        *rest = tail;
        number.parse().ok()
    };
    for token in layout {
        match token {
            Token::Year4 => year = Some(i32::try_from(digits(&mut rest, 4, 4)?).ok()?),
            Token::Year2 => year = Some(2000 + i32::try_from(digits(&mut rest, 2, 2)?).ok()?),
            Token::Month2 => month = Some(digits(&mut rest, 2, 2)?),
            Token::Month1 => month = Some(digits(&mut rest, 1, 2)?),
            Token::MonthName => {
                let name: String = rest.chars().take_while(|c| c.is_alphabetic()).collect();
                let key = name.to_lowercase();
                let index = MONTHS.iter().position(|month| key.starts_with(month))?;
                month = Some(u32::try_from(index).ok()? + 1);
                rest = &rest[name.len()..];
            }
            Token::DayName => {
                let name: String = rest.chars().take_while(|c| c.is_alphabetic()).collect();
                let key = name.to_lowercase();
                if !WEEKDAYS.iter().any(|weekday| key.starts_with(weekday)) {
                    return None;
                }
                rest = &rest[name.len()..];
            }
            Token::Day2 => day = Some(digits(&mut rest, 2, 2)?),
            Token::Day1 => day = Some(digits(&mut rest, 1, 2)?),
            Token::Hour24 | Token::Hour12 => hour = u8::try_from(digits(&mut rest, 1, 2)?).ok()?,
            Token::Minute => minute = u8::try_from(digits(&mut rest, 1, 2)?).ok()?,
            Token::Second => second = u8::try_from(digits(&mut rest, 1, 2)?).ok()?,
            Token::Meridiem => {
                let marker = rest.get(..2)?.to_ascii_lowercase();
                pm = Some(match marker.as_str() {
                    "am" => false,
                    "pm" => true,
                    _ => return None,
                });
                rest = &rest[2..];
            }
            Token::Offset => {
                let negative = match rest.chars().next()? {
                    '+' => false,
                    '-' => true,
                    _ => return None,
                };
                rest = &rest[1..];
                let hours = i8::try_from(digits(&mut rest, 1, 2)?).ok()?;
                let minutes = match rest.strip_prefix(':') {
                    Some(tail) => {
                        rest = tail;
                        i8::try_from(digits(&mut rest, 2, 2)?).ok()?
                    }
                    None => 0,
                };
                let sign = if negative { -1 } else { 1 };
                offset = UtcOffset::from_hms(sign * hours, sign * minutes, 0).ok()?;
            }
            Token::Literal(expected) => {
                let mut chars = rest.chars();
                if chars.next()? != *expected {
                    return None;
                }
                rest = chars.as_str();
            }
        }
    }
    if !rest.trim().is_empty() {
        return None;
    }
    if let Some(pm) = pm {
        hour = match (hour, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (hour, true) => hour + 12,
            (hour, false) => hour,
        };
    }
    let date = Date::from_calendar_date(
        year?,
        Month::try_from(u8::try_from(month?).ok()?).ok()?,
        u8::try_from(day?).ok()?,
    )
    .ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    Some(PrimitiveDateTime::new(date, time).assume_offset(offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tamanhos_como_os_sites_escrevem() {
        assert_eq!(parse_size("1.5 GiB"), Some(1_610_612_736));
        assert_eq!(parse_size("1,5 GB"), Some(1_610_612_736));
        assert_eq!(parse_size("1.234,5 MB"), Some(1_294_467_072));
        assert_eq!(parse_size("2048 MB"), Some(2_147_483_648));
        assert_eq!(parse_size("42 B"), Some(42));
        assert_eq!(parse_size("18446744073709551615 B"), Some(u64::MAX));
        for bad in ["NaN", "-1 GB", "1 XB", "", "GB", "18446744073709551616"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }

    #[test]
    fn contagens_ignoram_separador_e_celula_vazia_nao_e_erro() {
        assert_eq!(parse_count("1,234"), Ok(Some(1234)));
        assert_eq!(parse_count(" 12 "), Ok(Some(12)));
        assert_eq!(parse_count("-"), Ok(None));
        assert_eq!(parse_count("99999999999"), Err(()));
    }

    #[test]
    fn dateparse_com_layout_dotnet_e_go() {
        let tokens = layout_tokens("dd/MM/yy HH:mm:ss").unwrap();
        let parsed = parse_with_layout("24/09/26 09:05:07", &tokens).unwrap();
        assert_eq!(parsed.format(&Rfc3339).unwrap(), "2026-09-24T09:05:07Z");

        let go = layout_tokens("02 Jan 2006 3:04 PM").unwrap();
        let parsed = parse_with_layout("24 Sep 2026 9:05 PM", &go).unwrap();
        assert_eq!(parsed.format(&Rfc3339).unwrap(), "2026-09-24T21:05:00Z");

        assert!(parse_with_layout("31/02/26 00:00:00", &tokens).is_none());
        // Nome do dia da semana é lido e descartado; `\a` é a letra.
        let weekday = layout_tokens("dddd, MMMM d, yyyy \\a\\t h:mmtt zzz").unwrap();
        let parsed = parse_with_layout("Thursday, September 24, 2026 at 9:05PM +00:00", &weekday);
        assert_eq!(
            parsed.unwrap().format(&Rfc3339).unwrap(),
            "2026-09-24T21:05:00Z"
        );
        assert!(
            parse_with_layout("Someday, September 24, 2026 at 9:05PM +00:00", &weekday).is_none()
        );
        assert!(layout_tokens("dd/MM/yyyy fff").is_err());
    }

    #[test]
    fn dateparse_com_fuso() {
        let tokens = layout_tokens("yyyy-MM-dd zzz").unwrap();
        let parsed = parse_with_layout("2026-09-26 +00:01", &tokens).unwrap();
        assert_eq!(
            parsed.format(&Rfc3339).unwrap(),
            "2026-09-26T00:00:00+00:01"
        );
        let tokens = layout_tokens("dd/MM/yyyy HH:mm z").unwrap();
        let parsed = parse_with_layout("24/09/2026 10:00 -3", &tokens).unwrap();
        assert_eq!(parsed.unix_timestamp(), 1_790_254_800);
        assert!(parse_with_layout("24/09/2026 10:00 3", &tokens).is_none());
    }

    #[test]
    fn jsonjoinarray_junta_a_lista_do_caminho() {
        let join = |path: &str, value: &str| json_join(value, &json_path(path).unwrap(), ", ");
        assert_eq!(
            join("$.genres", r#"{"genres":["Drama","Ação"]}"#),
            Some("Drama, Ação".into())
        );
        assert_eq!(
            join("$.v", r#"{"v":["Cora\u00e7\u00e3o \/ 2"]}"#),
            Some("Coração / 2".into())
        );
        assert_eq!(join("$.a.b", r#"{"a":{"b":[1,2]}}"#), Some("1, 2".into()));
        assert_eq!(join("$.genres", r#"{"genres":"Drama"}"#), None);
        assert_eq!(join("$.genres", "não é json"), None);
        for path in ["genres", "$..x", "$.a[0]", "$.*"] {
            assert!(json_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn datas_sem_layout() {
        let now = OffsetDateTime::from_unix_timestamp(1_000).unwrap();
        assert_eq!(parse_date("now", now), Some(now));
        assert_eq!(parse_date("0", now), Some(OffsetDateTime::UNIX_EPOCH));
        assert!(parse_date("2026-09-24 10:00:00", now).is_some());
        assert!(parse_date("2026-09-24T10:00:00Z", now).is_some());
        assert!(parse_date("ontem", now).is_none());
    }

    #[test]
    fn filtros_de_texto_urlencode_html_validfilename_diacritics_e_validate() {
        let vars = Vars::default();
        let apply = |filter: Filter, value: &str| filter.apply(value.into(), &vars).unwrap();
        assert_eq!(
            apply(Filter::UrlEncode(Charset::Utf8), "a b&c/é"),
            "a+b%26c%2F%C3%A9"
        );
        assert_eq!(
            apply(Filter::UrlDecode(Charset::Utf8), "a+b%26c%2F%C3%A9"),
            "a b&c/é"
        );
        assert_eq!(
            apply(Filter::HtmlDecode, "A &amp; B &#39;c&#x41; &foo; &"),
            "A & B 'cA &foo; &"
        );
        assert_eq!(
            apply(Filter::HtmlEncode, "<a href=\"x\">é'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#233;&#39;&lt;/a&gt;"
        );
        assert_eq!(apply(Filter::ValidFileName, "a/b:c?.txt"), "a_b_c_.txt");
        assert_eq!(apply(Filter::ValidFileName, ""), "_");
        assert_eq!(
            apply(Filter::Diacritics, "Žluťoučký kůň Đ"),
            "Zlutoucky kun Đ"
        );
        let validate = Filter::Validate(words("Drama, Action; Comedy"));
        assert_eq!(
            apply(validate, "comedy / thriller | drama"),
            "drama, comedy"
        );
        // O Rfc3339 do filtro se lê de volta na data do release.
        let ago = apply(Filter::TimeAgo, "3 hours ago");
        let parsed = OffsetDateTime::parse(&ago, &Rfc3339).unwrap();
        let drift = (OffsetDateTime::now_utc() - parsed - time::Duration::hours(3)).abs();
        assert!(drift < time::Duration::seconds(5), "{ago}");
        assert!(
            Filter::TimeAgo
                .apply("3 fortnights ago".into(), &vars)
                .is_err()
        );
    }

    #[test]
    fn querystring_de_url_relativa() {
        assert_eq!(
            query_value("/browse.php?cat=7&x=1", "cat"),
            Some("7".into())
        );
        assert_eq!(query_value("browse.php?x=1", "cat"), None);
    }
}

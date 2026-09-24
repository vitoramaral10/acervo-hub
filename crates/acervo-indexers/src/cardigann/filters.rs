//! Filtros de campo, de `keywordsfilters`, e a interpretação de tamanho,
//! contagem e data — com a semântica do motor Cardigann de referência.

use regex::Regex;
use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use super::definition::RawFilter;
use super::invalid;
use super::template::{Names, Scope, Template, Vars, compile_regex, dotnet_replacement};
use crate::IndexerError;

#[derive(Debug)]
pub(super) enum Filter {
    Trim(Option<String>),
    Replace(String, Template),
    ReReplace(Regex, Template),
    Regexp(Regex),
    Append(Template),
    Prepend(Template),
    Split(char, isize),
    QueryString(String),
    DateParse(Vec<Token>),
    ToLower,
    ToUpper,
    UrlDecode,
    /// Diagnóstico da referência (`strdump`): não muda o valor.
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
            ("querystring", [name]) => Self::QueryString(name.clone()),
            ("dateparse" | "timeparse", [layout]) => Self::DateParse(layout_tokens(layout)?),
            ("tolower", []) => Self::ToLower,
            ("toupper", []) => Self::ToUpper,
            ("urldecode", []) => Self::UrlDecode,
            ("strdump", _) => Self::Noop,
            _ => {
                return Err(invalid(
                    "filters",
                    "filtro não implementado (timeago, fuzzytime, validfilename, ...)",
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
            Self::ReReplace(pattern, to) => pattern
                .replace_all(&value, to.render(vars).as_str())
                .into_owned(),
            // Como na referência: sempre o primeiro grupo; sem casar, vazio.
            Self::Regexp(pattern) => pattern
                .captures(&value)
                .and_then(|captures| captures.get(1))
                .map(|group| group.as_str().to_owned())
                .unwrap_or_default(),
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
            Self::UrlDecode => url::form_urlencoded::parse(format!("x={value}").as_bytes())
                .next()
                .map(|(_, decoded)| decoded.into_owned())
                .unwrap_or_default(),
        })
    }
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
    Hour24,
    Hour12,
    Minute,
    Second,
    Meridiem,
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
            ('d', 2) => Token::Day2,
            ('d', 1) => Token::Day1,
            ('H', 1 | 2) => Token::Hour24,
            ('h', 1 | 2) => Token::Hour12,
            ('m', 1 | 2) => Token::Minute,
            ('s', 1 | 2) => Token::Second,
            ('t', 2) => Token::Meridiem,
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
                    "token de data não suportado (fuso, dia da semana, fração)",
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

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn parse_with_layout(value: &str, layout: &[Token]) -> Option<OffsetDateTime> {
    let mut rest = value;
    let (mut year, mut month, mut day) = (None, None, None);
    let (mut hour, mut minute, mut second, mut pm) = (0_u8, 0_u8, 0_u8, None);
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
    Some(PrimitiveDateTime::new(date, time).assume_utc())
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
        assert!(layout_tokens("dd/MM/yyyy zzz").is_err());
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
    fn querystring_de_url_relativa() {
        assert_eq!(
            query_value("/browse.php?cat=7&x=1", "cat"),
            Some("7".into())
        );
        assert_eq!(query_value("browse.php?x=1", "cat"), None);
    }
}

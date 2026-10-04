//! Datas relativas e "fuzzy" dos filtros `timeago` e `fuzzytime`.
//!
//! A referência delega o `fuzzytime` a uma biblioteca de heurísticas de
//! data. Aqui vale o que dá para ler sem ambiguidade: ISO, RFC 2822/3339,
//! "há N unidades", hoje/ontem/amanhã, dia da semana, e datas com nome de mês.
//! Data numérica `NN/NN/AAAA` segue o padrão americano da referência (mês
//! primeiro), exceto quando o primeiro número só pode ser dia. O que não se
//! lê vira erro do campo, não data adivinhada. Sem fuso no texto, UTC.

use std::sync::LazyLock;

use regex::Regex;
use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, Weekday};

static AGO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bago").expect("regex fixa"));
static RELATIVE_DAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(today|tomorrow|yesterday)(?:[\s,]+(?:at){0,1}\s*|[\s,]*|$)")
        .expect("regex fixa")
});
static WEEKDAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(monday|tuesday|wednesday|thursday|friday|saturday|sunday)\s+at\s+")
        .expect("regex fixa")
});
static AMOUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*?([\d\.]+)\s*?([^\d\s\.]+)\s*?").expect("regex fixa"));
static CLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(\d{1,2}):(\d{2})(?::(\d{2}))?(?:\.\d+)?\s*([ap]\.?m\.?)?")
        .expect("regex fixa")
});
static MISSING_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{1,2}-\d{1,2})(\s|$)").expect("regex fixa"));
static ISO_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})\b").expect("regex fixa"));
static NUMERIC_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d{1,2})[-/.](\d{1,2})[-/.](\d{4}|\d{2})\b").expect("regex fixa")
});
static NAMED_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:(\d{1,2})(?:st|nd|rd|th)?[\s.,-]+([a-z]{3,9})\.?[\s.,-]+(\d{4})|([a-z]{3,9})\.?\s+(\d{1,2})(?:st|nd|rd|th)?[\s.,]+(\d{4}))\b",
    )
    .expect("regex fixa")
});

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// "2 hours 1 day", "3 weeks ago": `now` menos a soma das parcelas.
pub(super) fn time_ago(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let text = text.to_lowercase();
    if text.contains("now") {
        return Some(now);
    }
    let text = text
        .replace([','], "")
        .replace("ago", "")
        .replace("and", "");
    let mut seconds = 0.0_f64;
    for found in AMOUNT.captures_iter(&text) {
        let value: f64 = found[1].parse().ok()?;
        let unit = &found[2];
        let scale = if unit.contains("sec") || unit == "s" {
            1.0
        } else if unit.contains("min") || unit == "m" {
            60.0
        } else if unit.contains("hour") || unit.contains("hr") || unit == "h" {
            3600.0
        } else if unit.contains("day") || unit == "d" {
            86_400.0
        } else if unit.contains("week") || unit.contains("wk") || unit == "w" {
            7.0 * 86_400.0
        } else if unit.contains("month") || unit == "mo" {
            30.0 * 86_400.0
        } else if unit.contains("year") || unit == "y" {
            365.0 * 86_400.0
        } else {
            return None;
        };
        seconds += value * scale;
    }
    // Os limites evitam estourar a conversão com um número absurdo.
    if !seconds.is_finite() || seconds > 1e10 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(now - Duration::seconds(seconds.round() as i64))
}

/// O `FromUnknown` da referência, no subconjunto descrito no módulo.
pub(super) fn fuzzy(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let text = text.trim();
    if let Ok(parsed) =
        OffsetDateTime::parse(text, &Rfc3339).or_else(|_| OffsetDateTime::parse(text, &Rfc2822))
    {
        return Some(parsed);
    }
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return OffsetDateTime::from_unix_timestamp(text.parse().ok()?).ok();
    }
    if text.to_lowercase().contains("now") {
        return Some(now);
    }
    if AGO.is_match(text) {
        return time_ago(text, now);
    }
    let midnight = PrimitiveDateTime::new(now.date(), Time::MIDNIGHT).assume_utc();
    if let Some(found) = RELATIVE_DAY.captures(text) {
        let rest = text.replace(&found[0], "");
        let day = midnight + clock(&rest)?;
        return Some(match found[1].to_lowercase().as_str() {
            "yesterday" => day - Duration::days(1),
            "tomorrow" => day + Duration::days(1),
            _ => day,
        });
    }
    if let Some(found) = WEEKDAY.captures(text) {
        let rest = text.replace(&found[0], "");
        let mut day = midnight + clock(&rest)?;
        let wanted = match found[1].to_lowercase().as_str() {
            "monday" => Weekday::Monday,
            "tuesday" => Weekday::Tuesday,
            "wednesday" => Weekday::Wednesday,
            "thursday" => Weekday::Thursday,
            "friday" => Weekday::Friday,
            "saturday" => Weekday::Saturday,
            _ => Weekday::Sunday,
        };
        while day.weekday() != wanted {
            day -= Duration::days(1);
        }
        return Some(day);
    }
    let owned;
    let text = if let Some(found) = MISSING_YEAR.captures(text) {
        owned = text.replacen(&found[1], &format!("{}-{}", now.year(), &found[1]), 1);
        owned.as_str()
    } else {
        text
    };
    absolute(text, now)
}

/// Hora do dia de um trecho como `14:22`, `2:22 PM` ou vazio (meia-noite).
fn clock(text: &str) -> Option<Duration> {
    if text.trim().is_empty() {
        return Some(Duration::ZERO);
    }
    let found = CLOCK.captures(text)?;
    let time = clock_of(&found)?;
    Some(Duration::seconds(
        i64::from(time.hour()) * 3600 + i64::from(time.minute()) * 60 + i64::from(time.second()),
    ))
}

fn clock_of(found: &regex::Captures<'_>) -> Option<Time> {
    let mut hour: u8 = found[1].parse().ok()?;
    let minute: u8 = found[2].parse().ok()?;
    let second: u8 = found.get(3).map_or(Some(0), |s| s.as_str().parse().ok())?;
    if let Some(marker) = found.get(4) {
        let pm = marker.as_str().to_ascii_lowercase().starts_with('p');
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = match (hour, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (hour, true) => hour + 12,
            (hour, false) => hour,
        };
    }
    Time::from_hms(hour, minute, second).ok()
}

/// Data com dia, mês e ano no texto, mais hora opcional.
fn absolute(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let _ = now;
    let time = CLOCK
        .captures(text)
        .map_or(Some(Time::MIDNIGHT), |found| clock_of(&found))?;
    let date_text = CLOCK.replace(text, " ");
    let date = date_in(&date_text)?;
    Some(PrimitiveDateTime::new(date, time).assume_utc())
}

/// A data de um texto sem a hora: ISO, com nome de mês ou numérica.
fn date_in(text: &str) -> Option<Date> {
    if let Some(found) = ISO_DATE.captures(text) {
        return date_from(
            found[1].parse().ok()?,
            found[2].parse().ok()?,
            found[3].parse().ok()?,
        );
    }
    if let Some(found) = NAMED_DATE.captures(text) {
        let (day, month, year) = match found.get(1) {
            Some(day) => (day.as_str(), &found[2], &found[3]),
            None => (&found[5], &found[4], &found[6]),
        };
        let key = month.to_lowercase();
        let month = MONTHS.iter().position(|name| key.starts_with(name))? + 1;
        return date_from(
            year.parse().ok()?,
            u8::try_from(month).ok()?,
            day.parse().ok()?,
        );
    }
    let found = NUMERIC_DATE.captures(text)?;
    let (first, second): (u8, u8) = (found[1].parse().ok()?, found[2].parse().ok()?);
    let mut year: i32 = found[3].parse().ok()?;
    if found[3].len() == 2 {
        year += 2000;
    }
    // Padrão americano; só o que não pode ser mês inverte.
    let (month, day) = if first > 12 {
        (second, first)
    } else {
        (first, second)
    };
    date_from(year, month, day)
}

fn date_from(year: i32, month: u8, day: u8) -> Option<Date> {
    Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> OffsetDateTime {
        PrimitiveDateTime::new(
            date_from(year, month, day).unwrap(),
            Time::from_hms(hour, minute, second).unwrap(),
        )
        .assume_utc()
    }

    fn now() -> OffsetDateTime {
        at(2026, 9, 24, 10, 30, 0)
    }

    #[test]
    fn timeago_soma_as_parcelas_e_recusa_unidade_desconhecida() {
        assert_eq!(
            time_ago("2 hours ago", now()),
            Some(now() - Duration::hours(2))
        );
        assert_eq!(
            time_ago("1 day, 3 hrs and 20 min ago", now()),
            Some(now() - Duration::days(1) - Duration::hours(3) - Duration::minutes(20))
        );
        assert_eq!(
            time_ago("1.5 weeks", now()),
            Some(now() - Duration::hours(252))
        );
        assert_eq!(time_ago("Just now", now()), Some(now()));
        assert_eq!(time_ago("3 fortnights ago", now()), None);
    }

    #[test]
    fn fuzzy_le_relativos_iso_e_nome_de_mes() {
        assert_eq!(
            fuzzy("1648900000", now()),
            OffsetDateTime::from_unix_timestamp(1_648_900_000).ok()
        );
        assert_eq!(
            fuzzy("5 minutes ago", now()),
            Some(now() - Duration::minutes(5))
        );
        assert_eq!(
            fuzzy("Today at 14:22", now()),
            Some(at(2026, 9, 24, 14, 22, 0))
        );
        assert_eq!(
            fuzzy("yesterday 2:05 PM", now()),
            Some(at(2026, 9, 23, 14, 5, 0))
        );
        assert_eq!(fuzzy("tomorrow", now()), Some(at(2026, 9, 25, 0, 0, 0)));
        // 2026-09-24 é quinta; a sexta anterior é a de 18.
        assert_eq!(
            fuzzy("Friday at 08:00", now()),
            Some(at(2026, 9, 18, 8, 0, 0))
        );
        assert_eq!(
            fuzzy("2026-03-07 09:08:07", now()),
            Some(at(2026, 3, 7, 9, 8, 7))
        );
        assert_eq!(fuzzy("03-07 09:08", now()), Some(at(2026, 3, 7, 9, 8, 0)));
        assert_eq!(
            fuzzy("Sep 3, 2025 10:11 PM", now()),
            Some(at(2025, 9, 3, 22, 11, 0))
        );
        assert_eq!(fuzzy("3 March 2025", now()), Some(at(2025, 3, 3, 0, 0, 0)));
        assert_eq!(
            fuzzy("2026-09-24T10:00:00+02:00", now()).map(OffsetDateTime::unix_timestamp),
            Some(1_790_236_800)
        );
    }

    #[test]
    fn fuzzy_numerico_segue_o_padrao_americano_e_recusa_o_ilegivel() {
        assert_eq!(fuzzy("03/04/2025", now()), Some(at(2025, 3, 4, 0, 0, 0)));
        assert_eq!(fuzzy("25/04/2025", now()), Some(at(2025, 4, 25, 0, 0, 0)));
        assert_eq!(fuzzy("31/02/2025", now()), None);
        assert_eq!(fuzzy("quando der", now()), None);
    }
}

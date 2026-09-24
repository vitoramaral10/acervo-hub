use std::collections::BTreeMap;
use std::fmt::Write;

use super::definition::Setting;
use super::invalid;
use crate::{IndexerError, SearchMode, SearchQuery};

pub(super) struct Template(Vec<Part>);

enum Part {
    Literal(String),
    Variable(String),
}

impl Template {
    pub fn compile(
        value: &str,
        settings: &BTreeMap<String, Setting>,
    ) -> Result<Self, IndexerError> {
        let mut parts = Vec::new();
        let mut remaining = value;
        while let Some(start) = remaining.find("{{") {
            let literal = &remaining[..start];
            if literal.contains("}}") {
                return Err(invalid("search.inputs", "template malformado"));
            }
            parts.push(Part::Literal(literal.into()));
            let tail = &remaining[start + 2..];
            let end = tail
                .find("}}")
                .ok_or_else(|| invalid("search.inputs", "template não terminado"))?;
            let variable = tail[..end].trim();
            let known = matches!(
                variable,
                ".Keywords"
                    | ".Query.Keywords"
                    | ".Query.Season"
                    | ".Query.Ep"
                    | ".Query.Year"
                    | ".Query.IMDBID"
                    | ".Query.IMDBIDShort"
                    | ".Query.TMDBID"
                    | ".Query.TVDBID"
            ) || variable
                .strip_prefix(".Config.")
                .is_some_and(|name| settings.contains_key(name));
            if !known {
                return Err(invalid(
                    "search.inputs",
                    "variável desconhecida ou expressão Go não suportada",
                ));
            }
            parts.push(Part::Variable(variable.into()));
            remaining = &tail[end + 2..];
        }
        if remaining.contains("}}") {
            return Err(invalid("search.inputs", "template malformado"));
        }
        parts.push(Part::Literal(remaining.into()));
        Ok(Self(parts))
    }

    pub fn variables(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|part| match part {
            Part::Variable(name) => Some(name.as_str()),
            Part::Literal(_) => None,
        })
    }

    pub fn render(&self, query: &SearchQuery, settings: &BTreeMap<String, String>) -> String {
        let mut output = String::new();
        for part in &self.0 {
            match part {
                Part::Literal(value) => output.push_str(value),
                Part::Variable(name) => output.push_str(&variable(name, query, settings)),
            }
        }
        output
    }
}

fn variable(name: &str, query: &SearchQuery, settings: &BTreeMap<String, String>) -> String {
    if let Some(key) = name.strip_prefix(".Config.") {
        return settings.get(key).cloned().unwrap_or_default();
    }
    if name == ".Query.Keywords" {
        return query.term.clone().unwrap_or_default();
    }
    if name == ".Keywords" {
        let mut words = vec![query.term.clone().unwrap_or_default()];
        match &query.mode {
            SearchMode::Tv {
                season, episode, ..
            } => {
                let mut suffix = season
                    .map(|value| format!("S{value:02}"))
                    .unwrap_or_default();
                if let Some(episode) = episode {
                    if let Ok(number) = episode.parse::<u16>() {
                        let _ = write!(suffix, "E{number:02}");
                    } else {
                        let _ = write!(suffix, "E{episode}");
                    }
                }
                words.push(suffix);
            }
            SearchMode::Movie {
                year: Some(year), ..
            } => words.push(year.to_string()),
            _ => {}
        }
        return words
            .into_iter()
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }
    match (&query.mode, name) {
        (SearchMode::Tv { season, .. }, ".Query.Season") => {
            season.map(|value| value.to_string()).unwrap_or_default()
        }
        (SearchMode::Tv { episode, .. }, ".Query.Ep") => episode.clone().unwrap_or_default(),
        (SearchMode::Tv { tvdb_id, .. }, ".Query.TVDBID") => {
            tvdb_id.map(|value| value.to_string()).unwrap_or_default()
        }
        (SearchMode::Movie { year, .. }, ".Query.Year") => {
            year.map(|value| value.to_string()).unwrap_or_default()
        }
        (SearchMode::Movie { tmdb_id, .. }, ".Query.TMDBID") => {
            tmdb_id.map(|value| value.to_string()).unwrap_or_default()
        }
        (SearchMode::Movie { imdb_id, .. } | SearchMode::Tv { imdb_id, .. }, ".Query.IMDBID") => {
            imdb_id.clone().unwrap_or_default()
        }
        (
            SearchMode::Movie { imdb_id, .. } | SearchMode::Tv { imdb_id, .. },
            ".Query.IMDBIDShort",
        ) => imdb_id
            .as_deref()
            .unwrap_or_default()
            .trim_start_matches("tt")
            .to_owned(),
        _ => String::new(),
    }
}

//! Fidelidade contra a referência, num corpus de títulos de episódio reais.
//!
//! O corpus é um JSON com a leitura que o gerenciador de séries faz de cada
//! título (o `parsedEpisodeInfo` do endpoint `/api/v3/parse`), e fica fora do
//! repositório: títulos reais carregam nome de tracker privado. Rode com
//!
//! ```text
//! ACERVO_SERIES_CORPUS=corpus-series.json cargo test -p acervo-parser --test corpus_series -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use acervo_parser::{ParsedEpisode, Quality, parse_episode_title};
use serde::Deserialize;

#[derive(Deserialize)]
struct Entry {
    title: String,
    sonarr: Option<Reference>,
}

/// O `parsedEpisodeInfo` do original, campo a campo: os `bool` são dele.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct Reference {
    series_title: String,
    series_title_info: TitleInfo,
    quality: QualityRef,
    season_number: u16,
    episode_numbers: Vec<u16>,
    absolute_episode_numbers: Vec<u16>,
    #[serde(default)]
    air_date: Option<String>,
    languages: Vec<Named>,
    full_season: bool,
    is_partial_season: bool,
    is_multi_season: bool,
    is_season_extra: bool,
    is_split_episode: bool,
    is_mini_series: bool,
    special: bool,
    release_hash: Option<String>,
    season_part: u16,
    release_tokens: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TitleInfo {
    title_without_year: String,
    year: u16,
    #[serde(default)]
    all_titles: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct QualityRef {
    quality: Named,
    revision: RevisionRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionRef {
    version: u8,
    real: u8,
    is_repack: bool,
}

fn blank_is_none(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty()).map(str::to_owned)
}

/// O gerenciador de séries chama o remux de "Bluray-1080p Remux".
fn series_quality_name(quality: Quality) -> &'static str {
    match quality {
        Quality::Remux1080p => "Bluray-1080p Remux",
        Quality::Remux2160p => "Bluray-2160p Remux",
        other => other.name(),
    }
}

#[allow(clippy::too_many_lines)]
fn compare_all(
    parsed: &ParsedEpisode,
    reference: &Reference,
    mut compare: impl FnMut(&'static str, String, String),
) {
    compare(
        "série",
        parsed.series_title.clone(),
        reference.series_title.clone(),
    );
    compare(
        "título sem ano",
        parsed.series_title_info.title_without_year.clone(),
        reference.series_title_info.title_without_year.clone(),
    );
    compare(
        "ano no título",
        parsed.series_title_info.year.unwrap_or(0).to_string(),
        reference.series_title_info.year.to_string(),
    );
    compare(
        "títulos",
        format!("{:?}", parsed.series_title_info.all_titles),
        format!(
            "{:?}",
            reference
                .series_title_info
                .all_titles
                .clone()
                .unwrap_or_default()
        ),
    );
    compare(
        "temporada",
        parsed.season.to_string(),
        reference.season_number.to_string(),
    );
    compare(
        "episódios",
        format!("{:?}", parsed.episodes),
        format!("{:?}", reference.episode_numbers),
    );
    compare(
        "absolutos",
        format!("{:?}", parsed.absolute_episodes),
        format!("{:?}", reference.absolute_episode_numbers),
    );
    compare(
        "data",
        format!("{:?}", parsed.air_date),
        format!("{:?}", blank_is_none(reference.air_date.as_deref())),
    );
    compare(
        "fullSeason",
        parsed.full_season.to_string(),
        reference.full_season.to_string(),
    );
    compare(
        "isMultiSeason",
        parsed.multi_season.to_string(),
        reference.is_multi_season.to_string(),
    );
    compare(
        "isPartialSeason",
        parsed.partial_season.to_string(),
        reference.is_partial_season.to_string(),
    );
    compare(
        "isSeasonExtra",
        parsed.season_extra.to_string(),
        reference.is_season_extra.to_string(),
    );
    compare(
        "isSplitEpisode",
        parsed.split_episode.to_string(),
        reference.is_split_episode.to_string(),
    );
    compare(
        "isMiniSeries",
        parsed.mini_series.to_string(),
        reference.is_mini_series.to_string(),
    );
    compare(
        "special",
        parsed.special.to_string(),
        reference.special.to_string(),
    );
    compare(
        "seasonPart",
        parsed.season_part.to_string(),
        reference.season_part.to_string(),
    );
    compare(
        "qualidade",
        series_quality_name(parsed.quality.quality).to_owned(),
        reference.quality.quality.name.clone(),
    );
    compare(
        "revisão",
        format!(
            "{}/{}/{}",
            parsed.quality.revision.version,
            parsed.quality.revision.real,
            parsed.quality.revision.is_repack
        ),
        format!(
            "{}/{}/{}",
            reference.quality.revision.version,
            reference.quality.revision.real,
            reference.quality.revision.is_repack
        ),
    );
    compare(
        "idiomas",
        format!(
            "{:?}",
            parsed
                .languages
                .iter()
                .map(|l| l.name())
                .collect::<Vec<_>>()
        ),
        format!(
            "{:?}",
            reference
                .languages
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
        ),
    );
    compare(
        "hash",
        format!("{:?}", parsed.release_hash),
        format!("{:?}", blank_is_none(reference.release_hash.as_deref())),
    );
    compare(
        "tokens",
        parsed.release_tokens.clone(),
        reference.release_tokens.clone(),
    );
}

/// O cargo roda o teste na pasta do crate: caminho relativo que não existe
/// ali vale a partir da raiz do workspace.
fn corpus_path(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.exists() || path.is_absolute() {
        return path;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

#[test]
#[ignore = "precisa do corpus fora do repositório (ACERVO_SERIES_CORPUS)"]
fn corpus_series() {
    let path = corpus_path(&std::env::var("ACERVO_SERIES_CORPUS").expect("ACERVO_SERIES_CORPUS"));
    let entries: Vec<Entry> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let mut differences: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut compared = 0usize;
    for entry in &entries {
        let parsed = parse_episode_title(&entry.title);
        let (parsed, reference) = match (&parsed, &entry.sonarr) {
            (Some(parsed), Some(reference)) => (parsed, reference),
            (None, None) => continue,
            (None, Some(_)) => {
                differences
                    .entry("não leu")
                    .or_default()
                    .push(entry.title.clone());
                continue;
            }
            (Some(_), None) => {
                differences
                    .entry("leu a mais")
                    .or_default()
                    .push(entry.title.clone());
                continue;
            }
        };
        compared += 1;
        compare_all(parsed, reference, |field, ours, theirs| {
            if ours != theirs {
                differences.entry(field).or_default().push(format!(
                    "{}\n      nosso: {ours}\n      ref.:  {theirs}",
                    entry.title
                ));
            }
        });
    }

    println!(
        "{} títulos de episódio, {compared} lidos pelos dois",
        entries.len()
    );
    for (field, cases) in &differences {
        println!("\n{field}: {} divergências", cases.len());
        for case in cases.iter().take(5) {
            println!("   {case}");
        }
    }
    let wrong: usize = differences.values().map(Vec::len).sum();
    println!("\n{wrong} divergências no total");
    assert_eq!(wrong, 0, "divergências contra a referência");
}

//! Fidelidade contra a referência: buscas interativas reais, com a decisão
//! que o gerenciador de filmes tomou em cada release.
//!
//! Dois arquivos fora do repositório (têm nome de tracker privado):
//!
//! ```text
//! ACERVO_DECISION_CORPUS=corpus-decisao.json \
//! ACERVO_DECISION_LIBRARY=corpus-biblioteca.json \
//!   cargo test -p acervo-decision --test corpus -- --ignored --nocapture
//! ```
//!
//! O primeiro tem as buscas (`GET /api/v3/release`) e a configuração; o
//! segundo, a biblioteca inteira, porque o id do `IMDb` que o indexador manda é
//! procurado nela toda.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use acervo_decision::{
    Delay, Engine, ExistingFile, Indexer, Mode, Profile, ProfileItem, Propers, QualityDefinition,
    Queued, Release, Settings, Target, compare,
};
use acervo_parser::{Language, Quality, QualityModel, Revision, clean_movie_title};
use serde_json::Value;

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn number<T: TryFrom<u64>>(value: &Value) -> Option<T> {
    value.as_u64().and_then(|n| T::try_from(n).ok())
}

fn language(value: &Value) -> Language {
    value["name"]
        .as_str()
        .and_then(Language::from_name)
        .unwrap_or(Language::Unknown)
}

fn quality_model(value: &Value) -> QualityModel {
    QualityModel {
        quality: number(&value["quality"]["id"])
            .and_then(Quality::from_id)
            .unwrap_or(Quality::Unknown),
        revision: Revision {
            version: number(&value["revision"]["version"]).unwrap_or(1),
            real: number(&value["revision"]["real"]).unwrap_or(0),
            is_repack: value["revision"]["isRepack"].as_bool().unwrap_or(false),
        },
    }
}

fn profile(value: &Value) -> Profile {
    let items: Vec<(Option<i64>, ProfileItem)> = value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            let qualities: Vec<Quality> = if item["quality"].is_object() {
                number(&item["quality"]["id"])
                    .and_then(Quality::from_id)
                    .into_iter()
                    .collect()
            } else {
                item["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|q| number(&q["quality"]["id"]).and_then(Quality::from_id))
                    .collect()
            };
            let id = if item["quality"].is_object() {
                item["quality"]["id"].as_i64()
            } else {
                item["id"].as_i64()
            };
            (
                id,
                ProfileItem {
                    name: text(&item["name"]).unwrap_or_default(),
                    qualities,
                    allowed: item["allowed"].as_bool().unwrap_or(false),
                },
            )
        })
        .collect();
    let cutoff = value["cutoff"].as_i64();
    Profile {
        name: text(&value["name"]).unwrap_or_default(),
        cutoff: items.iter().position(|(id, _)| *id == cutoff),
        items: items.into_iter().map(|(_, item)| item).collect(),
        upgrade_allowed: value["upgradeAllowed"].as_bool().unwrap_or(false),
        language: language(&value["language"]),
        min_format_score: i32::try_from(value["minFormatScore"].as_i64().unwrap_or(0)).unwrap_or(0),
        cutoff_format_score: i32::try_from(value["cutoffFormatScore"].as_i64().unwrap_or(0))
            .unwrap_or(0),
        format_scores: Vec::new(),
    }
}

/// O título da base de metadados (em inglês) não sai na API; a pasta nasce
/// dele.
fn metadata_title(movie: &Value) -> String {
    let folder = movie["path"].as_str().unwrap_or_default();
    let folder = folder.rsplit('/').next().unwrap_or(folder);
    let folder = folder.split(" {imdb-").next().unwrap_or(folder);
    match folder.rfind(" (") {
        Some(at) => folder[..at].to_owned(),
        None => folder.to_owned(),
    }
}

fn target(movie: &Value, profiles: &BTreeMap<i64, Profile>, library: &Value) -> Target {
    let mut clean_titles = vec![text(&movie["cleanTitle"]).unwrap_or_default()];
    for title in [&movie["originalTitle"], &movie["title"]] {
        if let Some(title) = text(title) {
            clean_titles.push(clean_movie_title(&title));
        }
    }
    for alternative in movie["alternateTitles"].as_array().into_iter().flatten() {
        if let Some(title) = text(&alternative["title"]) {
            clean_titles.push(clean_movie_title(&title));
        }
    }
    let file = movie["movieFile"].is_object().then(|| ExistingFile {
        quality: quality_model(&movie["movieFile"]["quality"]),
        release_group: text(&movie["movieFile"]["releaseGroup"]),
        age_days: 30,
        format_score: 0,
    });
    Target {
        id: movie["id"].as_i64().unwrap_or_default(),
        title: metadata_title(movie),
        clean_titles,
        year: number(&movie["year"]).filter(|y: &u16| *y != 0),
        secondary_year: number(&movie["secondaryYear"]).filter(|y: &u16| *y != 0),
        tmdb_id: number(&movie["tmdbId"]).unwrap_or(0),
        imdb_id: text(&movie["imdbId"]),
        original_language: language(&movie["originalLanguage"]),
        runtime: number(&movie["runtime"]).unwrap_or(0),
        monitored: movie["monitored"].as_bool().unwrap_or(false),
        available: movie["isAvailable"].as_bool().unwrap_or(true),
        profile: profiles
            .get(&movie["qualityProfileId"].as_i64().unwrap_or_default())
            .cloned()
            .unwrap_or_else(|| panic!("perfil do filme {}", movie["title"])),
        file,
        queued: library["fila"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|q| q["movieId"] == movie["id"] && q["status"] != "failedPending")
            .map(|q| Queued {
                quality: quality_model(&q["quality"]),
                format_score: 0,
            })
            .collect(),
        free_space: library["raiz"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|root| {
                movie["path"]
                    .as_str()
                    .zip(root["path"].as_str())
                    .is_some_and(|(path, root)| path.starts_with(root))
            })
            .and_then(|root| root["freeSpace"].as_u64()),
    }
}

fn indexer_field<'a>(indexer: &'a Value, name: &str) -> &'a Value {
    indexer["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["name"] == name)
        .map_or(&Value::Null, |f| &f["value"])
}

fn flags(value: &Value) -> u32 {
    const NAMES: [(&str, u32); 6] = [
        ("G_Freeleech", 1),
        ("G_Halfleech", 2),
        ("G_DoubleUpload", 4),
        ("PTP_Golden", 8),
        ("PTP_Approved", 16),
        ("G_Internal", 32),
    ];
    if let Some(bits) = value.as_u64() {
        return u32::try_from(bits).unwrap_or(0);
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|name| NAMES.iter().find(|(n, _)| *n == name).map(|(_, bit)| *bit))
        .fold(0, |all, bit| all | bit)
}

fn release(value: &Value) -> Release {
    let seeders: Option<u32> = number(&value["seeders"]);
    let leechers: Option<u32> = number(&value["leechers"]);
    Release {
        title: text(&value["title"]).unwrap_or_default(),
        indexer: text(&value["indexer"]).unwrap_or_default(),
        size: value["size"].as_u64().unwrap_or(0),
        seeders,
        peers: seeders.map(|s| s + leechers.unwrap_or(0)),
        imdb_id: number(&value["imdbId"]).filter(|id: &u32| *id != 0),
        tmdb_id: number(&value["tmdbId"]).filter(|id: &u32| *id != 0),
        languages: Vec::new(),
        container: None,
        flags: flags(&value["indexerFlags"]),
        age_hours: None,
    }
}

/// Motivo da referência a partir da mensagem. `None` para os que dependem do
/// estado do gerenciador (fila, histórico, bloqueio), que o motor não vê.
fn reference_reason(message: &str) -> Option<&'static str> {
    const TABLE: [(&str, &str); 30] = [
        (
            "Quality for release in queue already meets cutoff",
            "QueueCutoffMet",
        ),
        ("Release in queue meets quality cutoff", "QueueCutoffMet"),
        (
            "Release in queue is of equal or higher preference",
            "QueueHigherPreference",
        ),
        (
            "Release in queue is of equal or higher revision",
            "QueueHigherRevision",
        ),
        (
            "Release in queue has an equal or higher Custom Format score",
            "QueueCustomFormatScore",
        ),
        (
            "Release in queue and Quality Profile",
            "QueueUpgradesNotAllowed",
        ),
        (
            "Importing after download will exceed available disk space",
            "MinimumFreeSpace",
        ),
        ("Not enough free space", "MinimumFreeSpace"),
        ("Unknown Movie", "UnknownMovie"),
        ("Unable to parse", "UnableToParse"),
        ("Wrong movie", "WrongMovie"),
        ("is not wanted in profile", "QualityNotWanted"),
        ("is wanted, but found", "WantedLanguage"),
        ("is smaller than minimum allowed", "BelowMinimumSize"),
        ("is larger than maximum allowed", "AboveMaximumSize"),
        ("is too big, maximum size", "MaximumSizeExceeded"),
        ("Hardcode subs found", "HardcodeSubtitles"),
        ("Raw ", "Raw"),
        ("Not enough seeders", "MinimumSeeders"),
        ("Existing file meets cutoff", "DiskCutoffMet"),
        (
            "Existing file on disk meets quality cutoff",
            "DiskCutoffMet",
        ),
        (
            "Existing file on disk is of equal or higher preference",
            "DiskHigherPreference",
        ),
        (
            "Existing file on disk is of equal or higher revision",
            "DiskHigherRevision",
        ),
        (
            "Existing file on disk has a equal or higher Custom Format score",
            "DiskCustomFormatScore",
        ),
        (
            "Existing file on disk and Quality Profile",
            "DiskUpgradesNotAllowed",
        ),
        (
            "Existing file and the Quality profile does not allow upgrades",
            "QualityUpgradesDisabled",
        ),
        ("Repack", "Repack"),
        ("Movie is not monitored", "MovieNotMonitored"),
        ("will only be considered available", "Availability"),
        ("have score", "CustomFormatMinimumScore"),
    ];
    if message == "Sample" {
        return Some("Sample");
    }
    TABLE
        .iter()
        .find(|(needle, _)| message.contains(needle))
        .map(|(_, reason)| *reason)
}

#[test]
#[ignore = "precisa do corpus fora do repositório (ACERVO_DECISION_CORPUS e ACERVO_DECISION_LIBRARY)"]
#[allow(clippy::too_many_lines)]
fn corpus() {
    let read = |var: &str| -> Value {
        let path = std::env::var(var).unwrap_or_else(|_| panic!("{var}"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read("ACERVO_DECISION_CORPUS");
    let library_json = read("ACERVO_DECISION_LIBRARY");

    let profiles: BTreeMap<i64, Profile> = corpus["perfis"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_i64().unwrap(), profile(p)))
        .collect();
    let library: Vec<Target> = library_json["filmes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| target(m, &profiles, &library_json))
        .collect();
    let config = &corpus["config_indexador"];
    let settings = Settings {
        definitions: corpus["qualidades"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| {
                Some(QualityDefinition {
                    quality: number(&d["quality"]["id"]).and_then(Quality::from_id)?,
                    min_size: d["minSize"].as_f64(),
                    max_size: d["maxSize"].as_f64(),
                    preferred_size: d["preferredSize"].as_f64(),
                })
            })
            .collect(),
        maximum_size_mb: config["maximumSize"].as_u64().unwrap_or(0),
        allow_hardcoded_subs: config["allowHardcodedSubs"].as_bool().unwrap_or(false),
        whitelisted_hardcoded_subs: text(&config["whitelistedHardcodedSubs"]).unwrap_or_default(),
        propers: match library_json["midia"]["downloadPropersAndRepacks"].as_str() {
            Some("doNotUpgrade") => Propers::DoNotUpgrade,
            Some("doNotPrefer") => Propers::DoNotPrefer,
            _ => Propers::PreferAndUpgrade,
        },
        prefer_indexer_flags: config["preferIndexerFlags"].as_bool().unwrap_or(false),
        minimum_free_space_mb: library_json["midia"]["minimumFreeSpaceWhenImporting"]
            .as_u64()
            .unwrap_or(100),
        skip_free_space_check: library_json["midia"]["skipFreeSpaceCheckWhenImporting"]
            .as_bool()
            .unwrap_or(false),
    };
    let indexers: Vec<Indexer> = corpus["indexadores"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| Indexer {
            name: text(&i["name"]).unwrap_or_default(),
            priority: i32::try_from(i["priority"].as_i64().unwrap_or(25)).unwrap_or(25),
            minimum_seeders: number(indexer_field(i, "minimumSeeders")).unwrap_or(0),
            multi_languages: indexer_field(i, "multiLanguages")
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .filter_map(|id| Language::ALL.into_iter().find(|l| i64::from(l.id()) == id))
                .collect(),
        })
        .collect();
    let engine = Engine {
        library: &library,
        indexers: &indexers,
        settings: &settings,
        formats: &[],
        blocklist: &[],
        delay: Delay::default(),
    };

    let mut releases_total = 0usize;
    let mut external = 0usize;
    let mut differences: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut reasons_seen: BTreeMap<&str, usize> = BTreeMap::new();
    for search in corpus["buscas"].as_array().unwrap() {
        let movie_id = search["movie"]["id"].as_i64().unwrap();
        let reference: Vec<&Value> = search["releases"].as_array().unwrap().iter().collect();
        let releases: Vec<Release> = reference.iter().map(|r| release(r)).collect();
        let decisions = engine.search(movie_id, &releases, Mode::UserInvoked);
        let by_release: BTreeMap<usize, &acervo_decision::Decision> =
            decisions.iter().map(|d| (d.release, d)).collect();
        let movie = search["movie"]["title"].as_str().unwrap_or_default();

        for (index, theirs) in reference.iter().enumerate() {
            releases_total += 1;
            let ours = by_release[&index];
            let messages: Vec<&str> = theirs["rejections"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if messages.iter().any(|m| reference_reason(m).is_none()) {
                external += 1;
                continue;
            }
            let theirs_reasons: BTreeSet<&str> = messages
                .iter()
                .filter_map(|m| reference_reason(m))
                .collect();
            let ours_reasons: BTreeSet<&str> = ours
                .rejections
                .iter()
                .map(|r| match r.reason() {
                    "RepackDisabled"
                    | "RepackUnknownReleaseGroup"
                    | "RepackReleaseGroupDoesNotMatch" => "Repack",
                    "QueuePropersDisabled" | "PropersDisabled" => "PropersDisabled",
                    other => other,
                })
                .collect();
            for reason in &theirs_reasons {
                *reasons_seen.entry(reason).or_default() += 1;
            }
            let title = theirs["title"].as_str().unwrap_or_default();
            if ours.movie.is_some() != theirs["downloadAllowed"].as_bool().unwrap_or(false) {
                differences.entry("casamento").or_default().push(format!(
                    "[{movie}] {title}\n      nosso: {:?}  ref.: {messages:?}",
                    ours.movie
                ));
            }
            if ours_reasons != theirs_reasons {
                differences.entry("motivos").or_default().push(format!(
                    "[{movie}] {title}\n      nosso: {ours_reasons:?} {:?}\n      ref.:  {messages:?}",
                    ours.rejections.iter().map(ToString::to_string).collect::<Vec<_>>()
                ));
            }
        }

        // A ordem da referência não pode contradizer o nosso comparador
        // dentro do filme buscado; empate pode vir em qualquer ordem.
        let requested: Vec<usize> = (0..reference.len())
            .filter(|i| reference[*i]["movieRequested"].as_bool().unwrap_or(false))
            .collect();
        for pair in requested.windows(2) {
            let (a, b) = (by_release[&pair[0]], by_release[&pair[1]]);
            if compare(&engine, &releases, a, b) == Ordering::Less {
                differences.entry("ordem").or_default().push(format!(
                    "[{movie}] ref. põe antes {}\n      e depois {}",
                    releases[pair[0]].title, releases[pair[1]].title
                ));
            }
        }
        let their_pick =
            (0..reference.len()).find(|i| reference[*i]["approved"].as_bool().unwrap_or(false));
        let our_pick = decisions.iter().find(|d| d.approved());
        match (their_pick, our_pick) {
            (None, None) => {}
            (Some(theirs), Some(ours))
                if theirs == ours.release
                    || compare(&engine, &releases, ours, by_release[&theirs])
                        == Ordering::Equal => {}
            (theirs, ours) => differences.entry("escolha").or_default().push(format!(
                "[{movie}] nosso: {:?}  ref.: {:?}",
                ours.map(|d| &releases[d.release].title),
                theirs.map(|i| &releases[i].title)
            )),
        }
    }

    println!(
        "{} buscas, {releases_total} releases ({external} com motivo de estado do gerenciador, fora da conta)",
        corpus["buscas"].as_array().unwrap().len()
    );
    println!("motivos da referência: {reasons_seen:?}");
    for (kind, cases) in &differences {
        println!("\n{kind}: {} divergências", cases.len());
        for case in cases.iter().take(10) {
            println!("   {case}");
        }
    }
    let wrong: usize = differences.values().map(Vec::len).sum();
    assert_eq!(wrong, 0, "divergências contra a referência");
}

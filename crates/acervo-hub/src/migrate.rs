//! Migração do que só o gerenciador de filmes sabe: histórico, lista de
//! bloqueio, exclusões, notificações e listas de importação.
//!
//! Pode rodar mais de uma vez: histórico já migrado não entra de novo,
//! exclusões repetidas ficam como estão, bloqueio e lista iguais são pulados
//! e a notificação só é copiada se ainda não houver uma aqui.

use std::collections::HashMap;

use acervo_arr::ArrClient;
use acervo_parser::Quality;
use acervo_store::{Blocked, Exclusion, ImportList, NewHistory, Store};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::shadow::movie_client;

/// Origem gravada em `data.origem` dos eventos migrados.
const ORIGIN: &str = "radarr";

#[derive(Debug, Default, Serialize)]
pub struct MigrationReport {
    pub historico: usize,
    pub historico_ja_migrado: bool,
    pub bloqueados: usize,
    pub exclusoes: u64,
    pub notificacao: bool,
    pub listas: Vec<String>,
    pub avisos: Vec<String>,
}

fn quality(value: &Value) -> Option<Quality> {
    value["quality"]["id"]
        .as_u64()
        .and_then(|id| u8::try_from(id).ok())
        .and_then(Quality::from_id)
}

fn event(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "grabbed" => "grabbed",
        "downloadFolderImported" | "movieFolderImported" => "imported",
        "downloadFailed" => "failed",
        "movieFileDeleted" => "file_deleted",
        "downloadIgnored" => "ignored",
        "movieFileRenamed" => "renamed",
        _ => return None,
    })
}

async fn paged(client: &ArrClient, path: &str) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    for page in 1.. {
        let page_text = page.to_string();
        let value = client
            .get_json(
                path,
                &[
                    ("page", page_text.as_str()),
                    ("pageSize", "250"),
                    ("sortKey", "date"),
                    ("sortDirection", "ascending"),
                ],
            )
            .await?;
        let records = value["records"].as_array().cloned().unwrap_or_default();
        let done = records.len() < 250;
        all.extend(records);
        if done {
            break;
        }
    }
    Ok(all)
}

/// Histórico do gerenciador, do mais velho ao mais novo.
async fn history(
    client: &ArrClient,
    store: &Store,
    ids: &HashMap<i64, (i64, String)>,
    report: &mut MigrationReport,
) -> Result<()> {
    if store.history_from(ORIGIN).await? > 0 {
        report.historico_ja_migrado = true;
        return Ok(());
    }
    let records = paged(client, "api/v3/history").await?;
    let movies: HashMap<i64, String> = client
        .get_json("api/v3/movie", &[])
        .await?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some((
                m["id"].as_i64()?,
                crate::events::label(
                    m["title"].as_str()?,
                    m["year"].as_u64().and_then(|y| u16::try_from(y).ok()),
                ),
            ))
        })
        .collect();
    let events: Vec<NewHistory> = records
        .iter()
        .filter_map(|record| {
            let kind = event(record["eventType"].as_str()?)?;
            let source_id = record["movieId"].as_i64()?;
            let local = ids.get(&source_id);
            let mut data = record["data"].clone();
            if !data.is_object() {
                data = json!({});
            }
            data["origem"] = json!(ORIGIN);
            data["id_origem"] = record["id"].clone();
            Some(NewHistory {
                movie_id: local.map(|(id, _)| *id),
                movie_title: local.map_or_else(
                    || movies.get(&source_id).cloned().unwrap_or_default(),
                    |(_, label)| label.clone(),
                ),
                event: kind.to_owned(),
                at: record["date"].as_str()?.to_owned(),
                source_title: record["sourceTitle"].as_str().map(str::to_owned),
                quality: quality(&record["quality"]),
                indexer: record["data"]["indexer"].as_str().map(str::to_owned),
                download_id: record["downloadId"].as_str().map(str::to_ascii_lowercase),
                data,
            })
        })
        .collect();
    store.record_history_batch(&events).await?;
    report.historico = events.len();
    Ok(())
}

async fn blocklist(
    client: &ArrClient,
    store: &Store,
    ids: &HashMap<i64, (i64, String)>,
    report: &mut MigrationReport,
) -> Result<()> {
    let existing = store.blocklist().await?;
    for record in paged(client, "api/v3/blocklist").await? {
        let Some(title) = record["sourceTitle"].as_str() else {
            continue;
        };
        if existing.iter().any(|b| b.source_title == title) {
            continue;
        }
        store
            .block(&Blocked {
                id: 0,
                movie_id: record["movieId"]
                    .as_i64()
                    .and_then(|id| ids.get(&id))
                    .map(|(id, _)| *id),
                source_title: title.to_owned(),
                // Os nomes de indexador de lá não são os daqui.
                indexer: None,
                quality: quality(&record["quality"]),
                size: record["size"].as_u64(),
                hash: None,
                at: record["date"].as_str().unwrap_or_default().to_owned(),
                message: record["message"].as_str().map(str::to_owned),
            })
            .await?;
        report.bloqueados += 1;
    }
    Ok(())
}

async fn exclusions(client: &ArrClient, store: &Store, report: &mut MigrationReport) -> Result<()> {
    let list: Vec<Exclusion> = client
        .get_json("api/v3/exclusions", &[])
        .await?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(Exclusion {
                tmdb_id: u32::try_from(e["tmdbId"].as_u64()?).ok()?,
                title: e["movieTitle"].as_str().unwrap_or_default().to_owned(),
                year: e["movieYear"]
                    .as_u64()
                    .and_then(|y| u16::try_from(y).ok())
                    .filter(|y| *y > 0),
            })
        })
        .collect();
    report.exclusoes = store.add_exclusions(&list).await?;
    Ok(())
}

fn field<'a>(item: &'a Value, name: &str) -> &'a Value {
    item["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["name"] == name)
        .map_or(&Value::Null, |f| &f["value"])
}

async fn notification(
    client: &ArrClient,
    store: &Store,
    report: &mut MigrationReport,
) -> Result<()> {
    if crate::events::gotify(store).await?.is_some() {
        return Ok(());
    }
    let all = client.get_json("api/v3/notification", &[]).await?;
    let Some(gotify) = all
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["implementation"] == "Gotify")
    else {
        return Ok(());
    };
    let token = field(gotify, "appToken").as_str().unwrap_or_default();
    if token.is_empty() || token.chars().all(|c| c == '*') {
        report
            .avisos
            .push("o token do Gotify não veio do gerenciador; configure na tela".into());
        return Ok(());
    }
    let settings = crate::events::Gotify {
        servidor: field(gotify, "server")
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        token: token.to_owned(),
        prioridade: field(gotify, "priority")
            .as_u64()
            .and_then(|p| u8::try_from(p).ok())
            .unwrap_or(5),
        eventos: crate::events::NotifyOn {
            pegou: gotify["onGrab"].as_bool().unwrap_or(true),
            importou: gotify["onDownload"].as_bool().unwrap_or(true),
            atualizou: gotify["onUpgrade"].as_bool().unwrap_or(true),
            falhou: gotify["onManualInteractionRequired"]
                .as_bool()
                .unwrap_or(true),
            removido: gotify["onMovieDelete"].as_bool().unwrap_or(false),
        },
        ligado: true,
    };
    store
        .set_setting(
            crate::events::NOTIFY_KEY,
            Some(&serde_json::to_string(&settings)?),
            &crate::shadow::now_rfc3339(),
        )
        .await?;
    report.notificacao = true;
    Ok(())
}

async fn import_lists(
    client: &ArrClient,
    store: &Store,
    report: &mut MigrationReport,
) -> Result<()> {
    let existing = store.import_lists().await?;
    let profiles = store.profiles_by_id().await?;
    for item in client
        .get_json("api/v3/importlist", &[])
        .await?
        .as_array()
        .into_iter()
        .flatten()
    {
        let name = item["name"].as_str().unwrap_or_default().to_owned();
        if existing.iter().any(|l| l.name == name) {
            continue;
        }
        let (kind, settings) = match item["implementation"].as_str().unwrap_or_default() {
            "TMDbPersonImport" => {
                let departments: Vec<&str> = [
                    ("personCastDirector", "Directing"),
                    ("personCastProducer", "Production"),
                    ("personCastSound", "Sound"),
                    ("personCastWriting", "Writing"),
                ]
                .into_iter()
                .filter(|(f, _)| field(item, f).as_bool().unwrap_or(false))
                .map(|(_, d)| d)
                .collect();
                (
                    "tmdb_person",
                    json!({
                        "pessoa": field(item, "personId").as_str()
                            .and_then(|s| s.parse::<u64>().ok())
                            .or_else(|| field(item, "personId").as_u64()),
                        "elenco": field(item, "personCast").as_bool().unwrap_or(true),
                        "departamentos": departments,
                    }),
                )
            }
            "TMDbCollectionImport" => (
                "tmdb_collection",
                json!({ "colecao": field(item, "collectionId").as_str()
                    .and_then(|s| s.parse::<u64>().ok()) }),
            ),
            "TMDbListImport" => (
                "tmdb_list",
                json!({ "lista": field(item, "listId").as_str() }),
            ),
            other => {
                report
                    .avisos
                    .push(format!("lista `{name}` ({other}) não tem equivalente aqui"));
                continue;
            }
        };
        let source_profile = item["qualityProfileId"].as_i64();
        store
            .save_import_list(&ImportList {
                id: 0,
                name: name.clone(),
                kind: kind.to_owned(),
                settings,
                enabled: item["enabled"].as_bool().unwrap_or(true)
                    && item["enableAuto"].as_bool().unwrap_or(true),
                monitor: item["monitor"].as_str() != Some("none"),
                search_on_add: item["searchOnAdd"].as_bool().unwrap_or(false),
                quality_profile_id: profiles
                    .iter()
                    .find(|(_, p)| p.source_id.is_some() && p.source_id == source_profile)
                    .map(|(id, _)| *id),
                root_folder: item["rootFolderPath"]
                    .as_str()
                    .unwrap_or("/media/movies")
                    .to_owned(),
                minimum_availability: item["minimumAvailability"]
                    .as_str()
                    .unwrap_or("released")
                    .to_owned(),
                tags: item["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                    .collect(),
                last_sync: None,
                last_error: None,
            })
            .await?;
        report.listas.push(name);
    }
    Ok(())
}

/// Traz tudo do gerenciador.
///
/// # Errors
///
/// Gerenciador ou banco inalcançável.
pub async fn run(config: &Config, store: &Store) -> Result<MigrationReport> {
    let client = movie_client(config)?;
    // Id do filme lá → id e rótulo aqui.
    let ids: HashMap<i64, (i64, String)> = store
        .movies()
        .await?
        .into_iter()
        .filter_map(|m| {
            let (_, source) = m.origin.as_ref()?;
            Some((
                *source,
                (m.id, crate::events::label(&m.movie.title, m.movie.year)),
            ))
        })
        .collect();
    let mut report = MigrationReport::default();
    history(&client, store, &ids, &mut report)
        .await
        .context("histórico")?;
    blocklist(&client, store, &ids, &mut report)
        .await
        .context("lista de bloqueio")?;
    exclusions(&client, store, &mut report)
        .await
        .context("exclusões")?;
    notification(&client, store, &mut report)
        .await
        .context("notificações")?;
    import_lists(&client, store, &mut report)
        .await
        .context("listas de importação")?;
    Ok(report)
}

//! `series import-sonarr`: traz as séries do gerenciador anterior, uma vez,
//! sem mexer no disco. Cada série é achada no TMDB pelo `tvdbId`; pasta,
//! pasta de temporada e "monitorar novos" vêm de lá; episódio sem arquivo
//! entra sem `skip` se está monitorado de fato (série, temporada e episódio
//! monitorados) e `unwanted` se não; cada arquivo entra com o que o
//! gerenciador sabe dele. Cada série é gravada numa transação só.
//! Episódio que o TMDB não tem fica no relatório, fora do catálogo. Série já
//! cadastrada é pulada: rodar de novo não duplica nada.

use std::collections::HashMap;
use std::time::Duration;

use acervo_metadata::Tmdb;
use acervo_parser::{QualityModel, Revision, parse_episode_quality};
use acervo_store::{Episode, EpisodeFile, Skip, Store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::decide::now_rfc3339;

/// Uma série do gerenciador anterior (`/api/v3/series`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonarrSeries {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub tvdb_id: u32,
    pub path: String,
    #[serde(default = "yes")]
    pub season_folder: bool,
    /// `all` ou `none`; versões antigas não mandam.
    #[serde(default)]
    pub monitor_new_items: Option<String>,
    #[serde(default = "yes")]
    pub monitored: bool,
    #[serde(default)]
    pub seasons: Vec<SonarrSeason>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonarrSeason {
    pub season_number: u16,
    #[serde(default = "yes")]
    pub monitored: bool,
}

impl SonarrSeries {
    /// Monitorado de fato: a série e a temporada também. Temporada que o
    /// gerenciador não lista conta como monitorada.
    fn monitors(&self, episode: &SonarrEpisode) -> bool {
        self.monitored
            && episode.monitored
            && self
                .seasons
                .iter()
                .find(|s| s.season_number == episode.season_number)
                .is_none_or(|s| s.monitored)
    }
}

const fn yes() -> bool {
    true
}

/// Um episódio (`/api/v3/episode?seriesId=`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonarrEpisode {
    pub season_number: u16,
    pub episode_number: u16,
    #[serde(default)]
    pub monitored: bool,
    /// Zero quando não tem arquivo.
    #[serde(default)]
    pub episode_file_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrName {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonarrRevision {
    #[serde(default = "one")]
    pub version: u8,
    #[serde(default)]
    pub real: u8,
    #[serde(default)]
    pub is_repack: bool,
}

const fn one() -> u8 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrQuality {
    pub quality: SonarrName,
    pub revision: Option<SonarrRevision>,
}

/// Um arquivo (`/api/v3/episodefile?seriesId=`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonarrFile {
    pub id: u64,
    pub relative_path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub date_added: Option<String>,
    #[serde(default)]
    pub scene_name: Option<String>,
    #[serde(default)]
    pub release_group: Option<String>,
    #[serde(default)]
    pub languages: Vec<SonarrName>,
    pub quality: Option<SonarrQuality>,
}

/// O que gravar de uma série, sem IO.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// O `skip` de cada episódio que o gerenciador conhece e o TMDB também.
    pub skips: Vec<((u16, u16), Option<Skip>)>,
    /// Cada arquivo, com os episódios que cobre.
    pub files: Vec<(EpisodeFile, Vec<(u16, u16)>)>,
    /// Episódios do gerenciador que o TMDB não tem.
    pub unmatched: Vec<(u16, u16)>,
}

/// A qualidade de um arquivo: pelo nome da qualidade no gerenciador; se ele
/// não diz, pelo nome do arquivo. A revisão é a do gerenciador.
fn quality(file: &SonarrFile) -> QualityModel {
    let by_name = file
        .quality
        .as_ref()
        .map(|q| parse_episode_quality(&q.quality.name));
    let mut model = match by_name {
        Some(model) if model.quality != acervo_parser::Quality::Unknown => model,
        _ => parse_episode_quality(&file.relative_path),
    };
    if let Some(revision) = file.quality.as_ref().and_then(|q| q.revision.as_ref()) {
        model.revision = Revision {
            version: revision.version,
            real: revision.real,
            is_repack: revision.is_repack,
        };
    }
    model
}

/// Cruza o que o gerenciador diz com os episódios do TMDB.
#[must_use]
pub fn plan(
    series: &SonarrSeries,
    tmdb: &[Episode],
    episodes: &[SonarrEpisode],
    files: &[SonarrFile],
) -> Plan {
    let known = |key: (u16, u16)| tmdb.iter().any(|e| (e.season, e.number) == key);
    let mut out = Plan::default();
    for episode in episodes {
        let key = (episode.season_number, episode.episode_number);
        if !known(key) {
            out.unmatched.push(key);
            continue;
        }
        let has_file =
            episode.episode_file_id != 0 && files.iter().any(|f| f.id == episode.episode_file_id);
        let skip = (!has_file && !series.monitors(episode)).then_some(Skip::Unwanted);
        out.skips.push((key, skip));
    }
    for file in files {
        let mut covers: Vec<(u16, u16)> = episodes
            .iter()
            .filter(|e| e.episode_file_id == file.id)
            .map(|e| (e.season_number, e.episode_number))
            .filter(|key| known(*key))
            .collect();
        if covers.is_empty() {
            continue;
        }
        covers.sort_unstable();
        out.files.push((
            EpisodeFile {
                relative_path: file.relative_path.clone(),
                size: file.size,
                quality: quality(file),
                languages: file.languages.iter().map(|l| l.name.clone()).collect(),
                release_group: file.release_group.clone().filter(|g| !g.is_empty()),
                scene_name: file.scene_name.clone().filter(|s| !s.is_empty()),
                date_added: file.date_added.clone(),
            },
            covers,
        ));
    }
    out.unmatched.sort_unstable();
    out
}

/// O gerenciador anterior, pela API v3.
struct Sonarr {
    base: url::Url,
    http: reqwest::Client,
}

impl Sonarr {
    fn new(url: &str, api_key: &str, timeout: Duration) -> Result<Self> {
        let base = url::Url::parse(&format!("{}/", url.trim_end_matches('/')))
            .context("URL do gerenciador inválida")?;
        let mut key = reqwest::header::HeaderValue::from_str(api_key)
            .context("chave de API com caractere inválido")?;
        key.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-Api-Key", key);
        Ok(Self {
            base,
            http: reqwest::Client::builder()
                .timeout(timeout)
                .default_headers(headers)
                .build()?,
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let response = self
            .http
            .get(self.base.join(path)?)
            .query(query)
            .send()
            .await
            .with_context(|| format!("falando com o gerenciador em `{path}`"))?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "o gerenciador respondeu {status} em `{path}`"
        );
        response
            .json()
            .await
            .with_context(|| format!("lendo `{path}`"))
    }

    async fn series(&self) -> Result<Vec<SonarrSeries>> {
        self.get("api/v3/series", &[]).await
    }

    async fn episodes(&self, series: u64) -> Result<Vec<SonarrEpisode>> {
        self.get("api/v3/episode", &[("seriesId", series.to_string())])
            .await
    }

    async fn files(&self, series: u64) -> Result<Vec<SonarrFile>> {
        self.get("api/v3/episodefile", &[("seriesId", series.to_string())])
            .await
    }
}

/// Uma série na migração.
#[derive(Debug, Serialize)]
pub struct MigrationLine {
    pub serie: String,
    pub tvdb: u32,
    pub tmdb: Option<u32>,
    /// `importada`, `importaria`, `ja_cadastrada`, `nao_achada` ou `falhou`.
    pub estado: &'static str,
    pub episodios: usize,
    pub arquivos: usize,
    /// Episódios do gerenciador que o TMDB não tem, como `S01E05`.
    pub sem_par: Vec<String>,
    pub detalhe: Option<String>,
}

/// Grava a série do plano numa transação: os episódios do TMDB, o `skip` de
/// cada um (o do gerenciador; quem ele não conhece, o padrão) e os arquivos.
async fn write(
    store: &Store,
    series: &acervo_store::Series,
    episodes: &[Episode],
    plan: &Plan,
) -> Result<()> {
    let skips: HashMap<(u16, u16), Option<Skip>> = plan.skips.iter().copied().collect();
    let episodes: Vec<(Episode, Option<Skip>)> = episodes
        .iter()
        .map(|e| {
            let skip = skips
                .get(&(e.season, e.number))
                .copied()
                .unwrap_or_else(|| acervo_store::default_skip(series, e));
            (e.clone(), skip)
        })
        .collect();
    store
        .import_series(series, &episodes, &plan.files, &now_rfc3339())
        .await?;
    Ok(())
}

/// Lê o gerenciador e, com `apply`, grava. Sem `apply`, só relata.
///
/// # Errors
///
/// Gerenciador inalcançável ou catálogo ilegível. Falha numa série fica na
/// linha dela; as outras seguem.
pub async fn import_sonarr(
    store: &Store,
    tmdb: &Tmdb,
    url: &str,
    api_key: &str,
    timeout: Duration,
    apply: bool,
) -> Result<Vec<MigrationLine>> {
    let sonarr = Sonarr::new(url, api_key, timeout)?;
    let mut lines = Vec::new();
    for remote in sonarr.series().await? {
        let mut line = MigrationLine {
            serie: remote.title.clone(),
            tvdb: remote.tvdb_id,
            tmdb: None,
            estado: "falhou",
            episodios: 0,
            arquivos: 0,
            sem_par: Vec::new(),
            detalhe: None,
        };
        let result: Result<()> = async {
            let catalog = store.series_list().await?;
            if remote.tvdb_id != 0
                && let Some(existing) = catalog
                    .iter()
                    .find(|s| s.series.tvdb_id == Some(remote.tvdb_id))
            {
                line.tmdb = Some(existing.series.tmdb_id);
                line.estado = "ja_cadastrada";
                return Ok(());
            }
            let found = if remote.tvdb_id == 0 {
                None
            } else {
                tmdb.find_tvdb(remote.tvdb_id).await?
            };
            let Some(tmdb_id) = found else {
                line.estado = "nao_achada";
                line.detalhe = Some("o TMDB não conhece este tvdbId".into());
                return Ok(());
            };
            line.tmdb = Some(tmdb_id);
            if catalog.iter().any(|s| s.series.tmdb_id == tmdb_id) {
                line.estado = "ja_cadastrada";
                return Ok(());
            }
            let (mut series, episodes) = super::library::lookup(tmdb, tmdb_id).await?;
            series.path.clone_from(&remote.path);
            series.season_folder = remote.season_folder;
            series.monitor_new = remote.monitor_new_items.as_deref() != Some("none");
            series.added = Some(now_rfc3339());
            let plan = plan(
                &remote,
                &episodes,
                &sonarr.episodes(remote.id).await?,
                &sonarr.files(remote.id).await?,
            );
            line.episodios = episodes.len();
            line.arquivos = plan.files.len();
            line.sem_par = plan
                .unmatched
                .iter()
                .map(|(s, n)| format!("S{s:02}E{n:02}"))
                .collect();
            if apply {
                write(store, &series, &episodes, &plan).await?;
                line.estado = "importada";
            } else {
                line.estado = "importaria";
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            line.estado = "falhou";
            line.detalhe = Some(format!("{error:#}"));
        }
        lines.push(line);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn tmdb(season: u16, number: u16) -> Episode {
        Episode {
            season,
            number,
            tmdb_id: None,
            title: None,
            air_date: None,
            overview: None,
            runtime: 0,
        }
    }

    fn episodes() -> Vec<SonarrEpisode> {
        serde_json::from_value(json!([
            {"seasonNumber": 1, "episodeNumber": 1, "monitored": true, "episodeFileId": 10, "hasFile": true},
            {"seasonNumber": 1, "episodeNumber": 2, "monitored": true, "episodeFileId": 10, "hasFile": true},
            {"seasonNumber": 1, "episodeNumber": 3, "monitored": true, "episodeFileId": 0},
            {"seasonNumber": 1, "episodeNumber": 4, "monitored": false, "episodeFileId": 0},
            {"seasonNumber": 1, "episodeNumber": 9, "monitored": true, "episodeFileId": 11}
        ]))
        .unwrap()
    }

    fn files() -> Vec<SonarrFile> {
        serde_json::from_value(json!([
            {"id": 10, "relativePath": "Season 1/Show - S01E01-E02 - A + B WEBDL-1080p.mkv",
             "size": 2000, "sceneName": "Show.S01E01E02.1080p.WEB-DL-GRP", "releaseGroup": "GRP",
             "dateAdded": "2024-01-01T00:00:00Z",
             "languages": [{"id": 1, "name": "English"}],
             "quality": {"quality": {"id": 3, "name": "WEBDL-1080p"},
                         "revision": {"version": 2, "real": 0, "isRepack": true}}},
            {"id": 11, "relativePath": "Season 1/Show - S01E09 - Sem par HDTV-720p.mkv",
             "size": 1000, "quality": {"quality": {"id": 0, "name": "Unknown"}}}
        ]))
        .unwrap()
    }

    fn remote(value: serde_json::Value) -> SonarrSeries {
        serde_json::from_value(value).unwrap()
    }

    fn show() -> SonarrSeries {
        remote(json!({"id": 7, "title": "Show", "tvdbId": 123, "path": "/tv/Show"}))
    }

    #[test]
    fn plano_cruza_gerenciador_e_tmdb() {
        let tmdb = [tmdb(1, 1), tmdb(1, 2), tmdb(1, 3), tmdb(1, 4)];
        let plan = plan(&show(), &tmdb, &episodes(), &files());
        assert_eq!(
            plan.skips,
            [
                ((1, 1), None),
                ((1, 2), None),
                ((1, 3), None),
                ((1, 4), Some(Skip::Unwanted)),
            ]
        );
        // O E09 o TMDB não tem: relatório, e o arquivo dele não entra.
        assert_eq!(plan.unmatched, [(1, 9)]);
        assert_eq!(plan.files.len(), 1);
        let (file, covers) = &plan.files[0];
        assert_eq!(covers, &[(1, 1), (1, 2)]);
        assert_eq!(file.quality.quality, acervo_parser::Quality::WebDl1080p);
        assert_eq!(file.quality.revision.version, 2);
        assert!(file.quality.revision.is_repack);
        assert_eq!(file.languages, ["English"]);
        assert_eq!(file.release_group.as_deref(), Some("GRP"));
    }

    #[test]
    fn monitorado_de_fato_e_serie_temporada_e_episodio() {
        let tmdb = [tmdb(1, 1), tmdb(1, 2), tmdb(1, 3), tmdb(1, 4), tmdb(2, 1)];
        let mut episodes = episodes();
        episodes.push(
            serde_json::from_value(
                json!({"seasonNumber": 2, "episodeNumber": 1, "monitored": true}),
            )
            .unwrap(),
        );
        let skips = |series: &SonarrSeries| plan(series, &tmdb, &episodes, &files()).skips;
        // Temporada 1 desmonitorada: o E03 sem arquivo vira `unwanted`; os
        // com arquivo ficam sem `skip`; a temporada 2 segue monitorada.
        let season_off = remote(json!({
            "id": 7, "title": "Show", "tvdbId": 123, "path": "/tv/Show", "monitored": true,
            "seasons": [{"seasonNumber": 1, "monitored": false},
                        {"seasonNumber": 2, "monitored": true}]
        }));
        assert_eq!(
            skips(&season_off),
            [
                ((1, 1), None),
                ((1, 2), None),
                ((1, 3), Some(Skip::Unwanted)),
                ((1, 4), Some(Skip::Unwanted)),
                ((2, 1), None),
            ]
        );
        // Série desmonitorada: nada sem arquivo fica em Quero.
        let series_off = remote(json!({
            "id": 7, "title": "Show", "tvdbId": 123, "path": "/tv/Show", "monitored": false,
            "seasons": [{"seasonNumber": 1, "monitored": true}]
        }));
        assert_eq!(
            skips(&series_off),
            [
                ((1, 1), None),
                ((1, 2), None),
                ((1, 3), Some(Skip::Unwanted)),
                ((1, 4), Some(Skip::Unwanted)),
                ((2, 1), Some(Skip::Unwanted)),
            ]
        );
    }

    #[test]
    fn qualidade_desconhecida_sai_do_nome_do_arquivo() {
        let files = files();
        assert_eq!(quality(&files[1]).quality, acervo_parser::Quality::Hdtv720p);
    }

    #[tokio::test]
    async fn le_a_api_v3_com_a_chave_no_cabecalho() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sonarr/api/v3/series"))
            .and(header("X-Api-Key", "chave"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 7, "title": "Show", "tvdbId": 123, "path": "/tv/Show",
                 "seasonFolder": false, "monitorNewItems": "none", "monitored": true}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/sonarr/api/v3/episode"))
            .and(query_param("seriesId", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"seasonNumber": 1, "episodeNumber": 1, "monitored": true, "episodeFileId": 0}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/sonarr/api/v3/episodefile"))
            .and(query_param("seriesId", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        let sonarr = Sonarr::new(
            &format!("{}/sonarr/", server.uri()),
            "chave",
            Duration::from_secs(5),
        )
        .unwrap();
        let series = sonarr.series().await.unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].tvdb_id, 123);
        assert!(!series[0].season_folder);
        assert_eq!(series[0].monitor_new_items.as_deref(), Some("none"));
        assert_eq!(sonarr.episodes(7).await.unwrap().len(), 1);
        assert!(sonarr.files(7).await.unwrap().is_empty());
        let wrong = Sonarr::new(&server.uri(), "chave", Duration::from_secs(5)).unwrap();
        assert!(wrong.series().await.is_err());
    }
}

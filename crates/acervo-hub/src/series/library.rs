//! A biblioteca de séries: o TMDB como fonte, adicionar com a escolha do
//! que buscar, editar, atualizar os metadados e remover a série inteira.

use std::collections::HashSet;

use acervo_metadata::{EpisodeMetadata, SeriesMetadata, Tmdb};
use acervo_parser::Language;
use acervo_store::{CatalogSeries, Episode, Series, Skip, Store};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::events::{self, Event, Kind};
use crate::naming::file_safe;

/// O que buscar de uma série que entra: o resto fica Dispensado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Monitor {
    /// Tudo o que falta.
    #[default]
    #[serde(rename = "tudo")]
    All,
    /// Só a partir da última temporada.
    #[serde(rename = "ultima_temporada")]
    LastSeason,
    /// Só os próximos episódios, que ainda não foram ao ar.
    #[serde(rename = "proximos")]
    Future,
}

/// Os episódios (fora os especiais) que ficam de fora da escolha.
#[must_use]
pub fn left_out(monitor: Monitor, episodes: &[(i64, &Episode)], today: Date) -> Vec<i64> {
    let last = episodes
        .iter()
        .map(|(_, e)| e.season)
        .filter(|s| *s > 0)
        .max()
        .unwrap_or(0);
    episodes
        .iter()
        .filter(|(_, e)| e.season > 0)
        .filter(|(_, e)| match monitor {
            Monitor::All => false,
            Monitor::LastSeason => e.season < last,
            Monitor::Future => super::aired(e, today),
        })
        .map(|(id, _)| *id)
        .collect()
}

/// Um episódio do TMDB no formato do catálogo.
#[must_use]
pub fn episode(meta: &EpisodeMetadata) -> Episode {
    Episode {
        season: meta.season,
        number: meta.number,
        tmdb_id: Some(meta.tmdb_id),
        title: meta.title.clone(),
        air_date: meta.air_date.clone(),
        overview: meta.overview.clone(),
        runtime: meta.runtime,
    }
}

/// Passa para a série o que veio da base. Pasta, pasta de temporada,
/// `monitor_new` e a data de entrada são de quem usa, e ficam.
fn apply(series: &mut Series, meta: &SeriesMetadata) {
    series.tvdb_id = meta.tvdb_id;
    series.imdb_id.clone_from(&meta.imdb_id);
    series.title = meta
        .localized_title
        .clone()
        .unwrap_or_else(|| meta.title.clone());
    series.original_title = Some(meta.original_title.clone());
    series.metadata_title = Some(meta.title.clone());
    series.original_language = Some(
        Language::from_iso639_1(&meta.original_language)
            .name()
            .to_owned(),
    );
    series.year = meta.year;
    series.status = Some(meta.status.clone());
    series.overview.clone_from(&meta.overview);
    series.network.clone_from(&meta.network);
    series.runtime = meta.runtime;
    series.poster.clone_from(&meta.poster);
    series.fanart.clone_from(&meta.fanart);
    series.alternate_titles.clone_from(&meta.alternate_titles);
}

/// Uma série nova, montada do TMDB, com os episódios, sem gravar.
///
/// # Errors
///
/// Série inexistente ou TMDB inalcançável.
pub async fn lookup(tmdb: &Tmdb, tmdb_id: u32) -> Result<(Series, Vec<Episode>)> {
    let meta = tmdb
        .series(tmdb_id)
        .await
        .with_context(|| format!("buscando a série {tmdb_id} no TMDB"))?;
    let episodes = tmdb
        .series_episodes(tmdb_id, &meta.seasons)
        .await
        .with_context(|| format!("buscando os episódios da série {tmdb_id} no TMDB"))?;
    let mut series = Series {
        tmdb_id,
        tvdb_id: None,
        imdb_id: None,
        title: String::new(),
        original_title: None,
        metadata_title: None,
        original_language: None,
        year: None,
        status: None,
        overview: None,
        network: None,
        runtime: 0,
        poster: None,
        fanart: None,
        path: String::new(),
        season_folder: true,
        monitor_new: true,
        added: None,
        refreshed_at: Some(now_rfc3339()),
        alternate_titles: Vec::new(),
    };
    apply(&mut series, &meta);
    Ok((series, episodes.iter().map(episode).collect()))
}

/// O que é preciso para adicionar uma série.
#[derive(Debug, Clone)]
pub struct AddRequest {
    pub tmdb_id: u32,
    pub monitor_new: bool,
    pub season_folder: bool,
    pub monitor: Monitor,
}

/// A pasta nova: `{raiz}/{título em inglês}`; com outra série já nela, o
/// ano entra no nome. `None` se também essa (ou, sem ano, a primeira) já é
/// de outra série: duas séries na mesma pasta misturariam os arquivos.
fn folder(root: &str, series: &Series, taken: &HashSet<String>) -> Option<String> {
    let root = root.trim_end_matches('/');
    let plain = format!("{root}/{}", file_safe(super::folder_title(series)));
    let free = |path: String| (!taken.contains(&path)).then_some(path);
    free(plain.clone()).or_else(|| {
        let year = series.year?;
        free(format!(
            "{root}/{}",
            file_safe(&format!("{} ({year})", super::folder_title(series)))
        ))
    })
}

/// Os episódios que a escolha deixa de fora e ainda estão em Quero. "Última
/// temporada" se mede sobre todos os episódios, com arquivo ou não: senão,
/// com a última inteira baixada, a penúltima viraria a "última".
fn leaving(monitor: Monitor, episodes: &[(i64, &Episode, bool)], today: Date) -> Vec<i64> {
    let all: Vec<(i64, &Episode)> = episodes.iter().map(|(id, e, _)| (*id, *e)).collect();
    let out = left_out(monitor, &all, today);
    episodes
        .iter()
        .filter(|(id, _, open)| *open && out.contains(id))
        .map(|(id, _, _)| *id)
        .collect()
}

/// Põe `unwanted` no que a escolha deixa de fora.
async fn leave_out(store: &Store, entry: &CatalogSeries, monitor: Monitor) -> Result<()> {
    let episodes: Vec<(i64, &Episode, bool)> = entry
        .episodes
        .iter()
        .map(|e| (e.id, &e.episode, e.file_id.is_none() && e.skip.is_none()))
        .collect();
    let out = leaving(monitor, &episodes, super::today());
    store
        .set_skip(&out, Some(Skip::Unwanted), &now_rfc3339())
        .await?;
    Ok(())
}

/// Adiciona a série ao catálogo. Devolve o id.
///
/// # Errors
///
/// Série já no catálogo, TMDB inalcançável ou falha de escrita.
pub async fn add(config: &Config, store: &Store, tmdb: &Tmdb, request: &AddRequest) -> Result<i64> {
    let list = store.series_list().await?;
    if list.iter().any(|s| s.series.tmdb_id == request.tmdb_id) {
        bail!("a série {} já está no catálogo", request.tmdb_id);
    }
    let (mut series, episodes) = lookup(tmdb, request.tmdb_id).await?;
    let taken: HashSet<String> = list
        .iter()
        .map(|s| s.series.path.trim_end_matches('/').to_owned())
        .collect();
    series.path = folder(&config.library.series_root, &series, &taken).with_context(|| {
        format!(
            "a pasta de `{}` já é de outra série do catálogo, também com o ano no nome",
            super::folder_title(&series)
        )
    })?;
    series.monitor_new = request.monitor_new;
    series.season_folder = request.season_folder;
    series.added = Some(now_rfc3339());
    let id = store.add_series(&series, &episodes).await?;
    let entry = store
        .series(id)
        .await?
        .context("a série sumiu logo depois de entrar")?;
    leave_out(store, &entry, request.monitor).await?;
    events::record(
        store,
        Event {
            poster: series.poster.clone(),
            message: Some(series.path.clone()),
            ..Event::series(
                Kind::SeriesAdded,
                id,
                events::label(&series.title, series.year),
                &[],
            )
        },
    )
    .await;
    Ok(id)
}

/// O que se muda numa série. Campo ausente fica como está.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeriesEdit {
    #[serde(default, rename = "monitorar_novos")]
    pub monitor_new: Option<bool>,
    /// Vale para os arquivos importados daqui em diante; os de antes ficam
    /// onde estão.
    #[serde(default, rename = "pasta_de_temporada")]
    pub season_folder: Option<bool>,
    /// Refaz a escolha do que buscar: o que fica de fora vira Dispensado, e
    /// o que entra e estava dispensado por "nunca quis" volta a Quero. Os
    /// apagados e assistidos não voltam.
    #[serde(default, rename = "buscar")]
    pub monitor: Option<Monitor>,
}

/// Muda uma série.
///
/// # Errors
///
/// Série desconhecida ou falha de escrita.
pub async fn edit(store: &Store, id: i64, change: &SeriesEdit) -> Result<()> {
    let entry = store.series(id).await?.context("série fora do catálogo")?;
    let mut series = entry.series.clone();
    if let Some(monitor_new) = change.monitor_new {
        series.monitor_new = monitor_new;
    }
    if let Some(season_folder) = change.season_folder {
        series.season_folder = season_folder;
    }
    if series != entry.series {
        store.update_series(id, &series).await?;
    }
    if let Some(monitor) = change.monitor {
        let episodes: Vec<(i64, &Episode)> =
            entry.episodes.iter().map(|e| (e.id, &e.episode)).collect();
        let out: HashSet<i64> = left_out(monitor, &episodes, super::today())
            .into_iter()
            .collect();
        let back: Vec<i64> = entry
            .episodes
            .iter()
            .filter(|e| e.episode.season > 0 && !out.contains(&e.id))
            .filter(|e| e.skip == Some(Skip::Unwanted))
            .map(|e| e.id)
            .collect();
        store.set_skip(&back, None, &now_rfc3339()).await?;
        leave_out(store, &entry, monitor).await?;
    }
    Ok(())
}

/// Resultado de uma atualização de metadados das séries.
#[derive(Debug, Default, Serialize)]
pub struct RefreshReport {
    pub conferidas: usize,
    pub atualizadas: Vec<String>,
    pub episodios_novos: usize,
    pub falhas: Vec<(String, String)>,
}

/// Atualiza as séries não conferidas nas últimas `stale_hours` horas (zero:
/// todas): os dados da série e os episódios, por `sync_episodes`, que nunca
/// mexe em `skip` nem em arquivo.
///
/// # Errors
///
/// Catálogo ilegível. Falha numa série fica no relato; as outras seguem.
pub async fn refresh(store: &Store, tmdb: &Tmdb, stale_hours: i64) -> Result<RefreshReport> {
    let cutoff = OffsetDateTime::now_utc() - Duration::hours(stale_hours);
    let mut report = RefreshReport::default();
    for entry in store.series_list().await? {
        let fresh = entry
            .series
            .refreshed_at
            .as_deref()
            .and_then(|at| {
                OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok()
            })
            .is_some_and(|at| at > cutoff);
        if stale_hours > 0 && fresh {
            continue;
        }
        report.conferidas += 1;
        let label = entry.series.title.clone();
        let fetched = async {
            let meta = tmdb.series(entry.series.tmdb_id).await?;
            let episodes = tmdb.series_episodes(meta.tmdb_id, &meta.seasons).await?;
            Ok::<_, acervo_metadata::MetadataError>((meta, episodes))
        }
        .await;
        let (meta, episodes) = match fetched {
            Ok(found) => found,
            Err(acervo_metadata::MetadataError::SeriesNotFound(id)) => {
                report
                    .falhas
                    .push((label, format!("não existe mais no TMDB (id {id})")));
                continue;
            }
            Err(error) => {
                report.falhas.push((label, error.to_string()));
                continue;
            }
        };
        let mut series = entry.series.clone();
        apply(&mut series, &meta);
        let changed = series != entry.series;
        series.refreshed_at = Some(now_rfc3339());
        store.update_series(entry.id, &series).await?;
        let episodes: Vec<Episode> = episodes.iter().map(episode).collect();
        let sync = store.sync_episodes(entry.id, &episodes).await?;
        report.episodios_novos += sync.added.len();
        if changed || sync.updated > 0 || !sync.added.is_empty() || sync.removed > 0 {
            report.atualizadas.push(label);
        }
    }
    Ok(report)
}

/// Tira a série do catálogo, como o filme: com `delete_files`, a pasta e os
/// torrents que semeiam os arquivos dela (casados por inode) saem juntos.
/// Se o cliente falhar, a série sai mesmo assim e o ciclo limpa depois. O
/// torrent de grab ainda na fila, sem nada baixado, sai do cliente sempre.
///
/// # Errors
///
/// Série desconhecida, pasta fora da raiz de séries ou falha de escrita.
pub async fn remove(config: &Config, store: &Store, id: i64, delete_files: bool) -> Result<()> {
    let entry = store.series(id).await?.context("série fora do catálogo")?;
    let path = &entry.series.path;
    let downloads = if delete_files {
        crate::library::downloads_of(config, path)
            .await
            .map_err(|error| format!("{error:#}"))
    } else {
        Ok(Vec::new())
    };
    if delete_files {
        crate::library::delete_folder_within(
            config,
            path,
            std::slice::from_ref(&config.library.series_root),
        )
        .await?;
    }
    // O grab que ainda espera na fila sairia com a série, e a fila iniciaria
    // o torrent dele como órfão.
    let queued: Vec<_> = store
        .series_grabs()
        .await?
        .into_iter()
        .filter(|g| g.series_id == id && g.state == acervo_store::GrabState::Downloading)
        .collect();
    if !queued.is_empty() {
        match crate::grab::qbit(config).await {
            Ok(client) => {
                for grab in &queued {
                    super::grab::drop_queued(config, store, &client, grab).await;
                }
            }
            Err(error) => tracing::warn!("torrents da fila não conferidos: {error:#}"),
        }
    }
    store.delete_series(id).await?;
    let removed = match downloads {
        Ok(found) if found.is_empty() => Ok(Vec::new()),
        Ok(found) => crate::library::delete_downloads(config, found).await,
        Err(error) => Err(error),
    };
    if let Err(error) = &removed {
        tracing::warn!(
            serie = entry.series.title,
            %error,
            "download não apagado; fica para o ciclo de limpeza"
        );
    }
    tracing::info!(
        serie = entry.series.title,
        arquivos = delete_files,
        downloads = removed.as_ref().map_or(0, Vec::len),
        "série removida"
    );
    events::record(
        store,
        Event {
            message: delete_files.then(|| crate::library::removal_message(path, &removed)),
            poster: entry.series.poster.clone(),
            // A série já saiu: o histórico não tem mais a que ligar.
            ..Event::new(
                Kind::SeriesDeleted,
                None,
                events::label(&entry.series.title, entry.series.year),
            )
        },
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(season: u16, number: u16, air: Option<&str>) -> Episode {
        Episode {
            season,
            number,
            tmdb_id: None,
            title: None,
            air_date: air.map(str::to_owned),
            overview: None,
            runtime: 0,
        }
    }

    #[test]
    fn escolha_do_que_buscar_ao_adicionar() {
        let today = crate::series::date(Some("2026-10-03")).unwrap();
        let all = [
            ep(0, 1, Some("2020-01-01")),
            ep(1, 1, Some("2020-01-01")),
            ep(2, 1, Some("2026-09-01")),
            ep(2, 2, Some("2026-10-10")),
            ep(2, 3, None),
        ];
        let episodes: Vec<(i64, &Episode)> = (1..).zip(all.iter()).collect();
        assert!(left_out(Monitor::All, &episodes, today).is_empty());
        assert_eq!(left_out(Monitor::LastSeason, &episodes, today), [2]);
        assert_eq!(left_out(Monitor::Future, &episodes, today), [2, 3]);
        // Especial nunca entra na conta: ele já nasce dispensado.
        assert!(!left_out(Monitor::Future, &episodes, today).contains(&1));
    }

    #[test]
    fn pasta_nova_com_titulo_em_ingles_e_ano_se_ja_ocupada() {
        let mut series = Series {
            tmdb_id: 1,
            tvdb_id: None,
            imdb_id: None,
            title: "A Casa".into(),
            original_title: None,
            metadata_title: Some("The House: Reborn".into()),
            original_language: None,
            year: Some(2024),
            status: None,
            overview: None,
            network: None,
            runtime: 0,
            poster: None,
            fanart: None,
            path: String::new(),
            season_folder: true,
            monitor_new: true,
            added: None,
            refreshed_at: None,
            alternate_titles: Vec::new(),
        };
        assert_eq!(
            folder("/media/series/", &series, &HashSet::new()).as_deref(),
            Some("/media/series/The House - Reborn")
        );
        let mut taken = HashSet::from(["/media/series/The House - Reborn".to_owned()]);
        assert_eq!(
            folder("/media/series", &series, &taken).as_deref(),
            Some("/media/series/The House - Reborn (2024)")
        );
        // Também com o ano já é de outra: recusa, não mistura.
        taken.insert("/media/series/The House - Reborn (2024)".to_owned());
        assert_eq!(folder("/media/series", &series, &taken), None);
        // Sem ano para desempatar: recusa.
        series.year = None;
        assert_eq!(folder("/media/series", &series, &taken), None);
        series.metadata_title = None;
        assert_eq!(
            folder("/media/series", &series, &HashSet::new()).as_deref(),
            Some("/media/series/A Casa")
        );
    }

    #[test]
    fn ultima_temporada_conta_tambem_a_que_ja_tem_arquivo() {
        let today = crate::series::date(Some("2026-10-03")).unwrap();
        let all = [
            ep(1, 1, Some("2020-01-01")),
            ep(2, 1, Some("2021-01-01")),
            ep(3, 1, Some("2022-01-01")),
        ];
        // A 3 inteira baixada (fechada); 1 e 2 em Quero.
        let episodes: Vec<(i64, &Episode, bool)> =
            vec![(1, &all[0], true), (2, &all[1], true), (3, &all[2], false)];
        assert_eq!(leaving(Monitor::LastSeason, &episodes, today), [1, 2]);
        assert!(leaving(Monitor::All, &episodes, today).is_empty());
    }

    #[test]
    fn escolha_le_os_nomes_da_tela() {
        let edit: SeriesEdit = serde_json::from_value(serde_json::json!({
            "monitorar_novos": false, "buscar": "ultima_temporada"
        }))
        .unwrap();
        assert_eq!(edit.monitor_new, Some(false));
        assert_eq!(edit.monitor, Some(Monitor::LastSeason));
        assert_eq!(edit.season_folder, None);
    }
}

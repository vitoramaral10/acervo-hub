//! Importação de séries, dentro da tarefa `importacao`: cada vídeo escolhido
//! de um torrent terminado vira um hardlink na pasta da série, com o nome do
//! padrão, e o catálogo liga o arquivo aos episódios.
//!
//! Falha segue a dos filmes: torrent com erro vai para a lista de bloqueio
//! (com a série) e os episódios são buscados de novo; torrent sumido do
//! cliente é buscado de novo sem bloqueio; disco cheio devolve o torrent à
//! fila; problema na importação espera a próxima volta. Arquivo que o
//! cliente dá por baixado e não está no disco faz o torrent ser verificado de
//! novo, como nos filmes.
//!
//! A volta liga os arquivos no disco primeiro e grava tudo de uma vez no
//! fim ([`Store::import_series_files`]): arquivos, legendas e o grab.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use acervo_api::Catalog;
use acervo_clients::{QbitClient, TorrentFile, TorrentInfo, client_path};
use acervo_core::DownloadHash;
use acervo_store::{
    CatalogEpisodeFile, CatalogSeries, EpisodeFile, FailReason, GrabState, SeriesGrab,
    SeriesImport, Store,
};
use anyhow::Result;
use serde::Serialize;
use time::{Duration, OffsetDateTime};

use super::grab::{Choice, apply_selection, give_up};
use super::naming::{TBA, episode_path};
use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::events::{self, Event, Kind};
use crate::grab::{
    CHECKING, Condition, Failure, NO_SEEDS, QUEUE_TAG, QUEUED, UNREGISTERED, absent, checking,
    client_trouble, ensure_client_listed, install, left, no_seeds, present, progress, qbit,
    recheck_once, watch,
};

/// Quanto se espera pelo título do episódio depois da exibição.
const TITLE_WAIT: Duration = Duration::hours(48);

/// Um download de série na importação.
#[derive(Debug, Serialize)]
pub struct ImportLine {
    /// "Série S01E02".
    pub serie: String,
    pub release: String,
    /// `baixando`, `importado`, `atencao` (importação travada, tenta de
    /// novo), `falhou` ou `descartado` (nada mais a importar dele).
    pub estado: &'static str,
    pub detalhe: Option<String>,
    /// Caminhos dos arquivos na pasta da série, como o cliente vê.
    pub destinos: Vec<String>,
}

/// O título que vai no nome: o da base; sem ele, [`TBA`] depois de 48 h da
/// exibição (ou do grab, para episódio sem data); antes disso, `None`:
/// espera.
#[must_use]
pub fn episode_title(
    title: Option<&str>,
    air_date: Option<&str>,
    grabbed_at: &str,
    now: OffsetDateTime,
) -> Option<String> {
    if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(title.to_owned());
    }
    let since = match super::date(air_date) {
        Some(day) => day.midnight().assume_utc(),
        None => OffsetDateTime::parse(grabbed_at, &time::format_description::well_known::Rfc3339)
            .ok()?,
    };
    (now - since >= TITLE_WAIT).then(|| TBA.to_owned())
}

/// Os episódios de um arquivo que ele vai cobrir no catálogo: os que o grab
/// foi buscar e ainda estão em Quero (sem arquivo e sem `skip`). Já com
/// arquivo ou dispensado no meio não é religado; não há upgrade.
#[must_use]
pub fn to_link(
    episodes: &[acervo_store::CatalogEpisode],
    wanted: &HashSet<i64>,
    in_file: &[i64],
) -> Vec<i64> {
    in_file
        .iter()
        .copied()
        .filter(|id| wanted.contains(id))
        .filter(|id| {
            episodes
                .iter()
                .any(|e| e.id == *id && e.file_id.is_none() && e.skip.is_none())
        })
        .collect()
}

/// Um arquivo que a volta ligou no disco, ainda por gravar.
struct Linked {
    /// O destino, como o cliente vê.
    shown: String,
    quality: acervo_parser::Quality,
    record: SeriesImport,
}

/// O que uma volta fez com um grab.
#[derive(Default)]
struct Step {
    /// Arquivos ligados no disco nesta volta.
    linked: Vec<Linked>,
    /// Um arquivo não ligou depois de outros que ligaram: os ligados são
    /// gravados, e o grab fica em atenção com este erro.
    failed: Option<String>,
    /// Por que ainda não terminou.
    waiting: Option<String>,
    /// Nada mais a importar dele, e nunca importou nada.
    discard: Option<String>,
}

impl Step {
    fn waiting(why: impl Into<String>) -> Self {
        Self {
            waiting: Some(why.into()),
            ..Self::default()
        }
    }
}

/// A pasta (com a barra final, ou vazia) e o stem de um caminho de vídeo.
pub(crate) fn split_video(video: &str) -> (&str, &str) {
    let (dir, file) = match video.rfind('/') {
        Some(at) => (&video[..=at], &video[at + 1..]),
        None => ("", video),
    };
    (dir, file.rsplit_once('.').map_or(file, |(stem, _)| stem))
}

struct Importer<'a> {
    store: &'a Store,
    client: QbitClient,
    free_space: Option<u64>,
}

impl Importer<'_> {
    /// Liga as legendas ao lado do vídeo e devolve as que ligou, a gravar
    /// com ele. Falha numa legenda vira aviso: ela não segura a importação.
    async fn link_subtitles(
        &self,
        torrent: &TorrentInfo,
        entry: &CatalogSeries,
        video: &str,
        files: &[&TorrentFile],
    ) -> Vec<acervo_store::Subtitle> {
        let mut subtitles = Vec::new();
        if files.is_empty() {
            return subtitles;
        }

        let folder = PathBuf::from(&entry.series.path);
        let (dir, stem) = split_video(video);
        let originals: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        let named = crate::subtitles::names(stem, &originals, &HashSet::new());
        for (file, named) in files.iter().zip(named) {
            let relative = format!("{dir}{}", named.name);
            let source = client_path(torrent, file);
            let target = folder.join(&relative);
            let linked = tokio::task::spawn_blocking(move || crate::grab::link(&source, &target))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r);
            if let Err(error) = linked {
                tracing::warn!(legenda = file.name, "legenda não importada: {error:#}");
                continue;
            }
            subtitles.push(acervo_store::Subtitle {
                relative_path: relative,
                language: named.language.map(str::to_owned),
                forced: named.forced,
                origin: acervo_store::SubtitleOrigin::Import,
            });
        }
        subtitles
    }

    #[allow(clippy::too_many_lines)] // A sequência da importação; dividir só espalharia.
    async fn step(&self, grab: &SeriesGrab, entry: &CatalogSeries) -> Result<Step, Failure> {
        let torrent = self
            .client
            .torrent(&grab.hash)
            .await
            .map_err(|e| e.to_string())?;
        let now = OffsetDateTime::now_utc();
        let Some(torrent) = torrent else {
            return Err(absent(&mut watch(), &grab.hash, &grab.grabbed_at, now));
        };
        present(&mut watch(), &grab.hash, now);
        if torrent.state == "error" {
            let free = self
                .free_space
                .ok_or("erro no cliente e espaço livre ilegível")?;
            if free < left(torrent.size, torrent.progress) {
                watch().observe(&torrent.hash, Condition::ClientError, false, now);
                // A fila não é falta de seed: a contagem do sem seeds recomeça.
                watch().observe(&torrent.hash, Condition::NoSeeds, false, now);
                self.client
                    .stop(&[&grab.hash])
                    .await
                    .map_err(|e| e.to_string())?;
                self.client
                    .add_tag(&[&grab.hash], QUEUE_TAG)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(Step::waiting(QUEUED));
            }
        }
        let trouble = client_trouble(&mut watch(), &torrent, now);
        if let Some(failure) = trouble {
            // Arquivo sumido: antes de desistir, o mesmo torrent é verificado
            // de novo e baixa o que falta. Se persistir depois disso, o
            // torrent perdido sai do cliente (se é só deste grab), senão a
            // nova busca devolveria o mesmo hash, ainda em `missingFiles`.
            // Sem bloqueio: a culpa não é do release.
            match failure {
                Failure::Lost(..) => {
                    self.drop_torrent(grab, &torrent, "arquivos sumidos")
                        .await?;
                }
                _ if torrent.state == "missingFiles" => {
                    recheck_once(&self.client, &torrent).await;
                }
                _ => {}
            }
            return Err(failure);
        }
        if crate::grab::gone_from_tracker(&self.client, &torrent).await {
            tracing::info!(
                serie = entry.series.title,
                release = grab.title,
                "o tracker não reconhece mais o torrent"
            );
            // Como o sem seeds: só sai do cliente se é só deste grab.
            self.drop_torrent(grab, &torrent, "torrent desregistrado")
                .await?;
            return Err(Failure::Download(
                FailReason::Unregistered,
                UNREGISTERED.into(),
            ));
        }
        let stuck = no_seeds(&mut watch(), &torrent, now);
        if stuck {
            tracing::info!(
                serie = entry.series.title,
                release = grab.title,
                "trocando release sem seeds"
            );
            // O bloqueio e a nova busca vêm do `Failure::Download`; o torrent
            // travado ocuparia vaga e reserva. Só sai se é só deste grab.
            self.drop_torrent(grab, &torrent, "torrent sem seeds")
                .await?;
            return Err(Failure::Download(FailReason::NoSeeds, NO_SEEDS.into()));
        }
        let Some(selection) = apply_selection(
            &self.client,
            &grab.hash,
            entry,
            &grab.episode_ids,
            &grab.title,
            true,
        )
        .await
        .map_err(|e| format!("{e:#}"))?
        else {
            return Ok(Step::waiting("aguardando os metadados do torrent"));
        };
        let chosen: Vec<(&TorrentFile, &Choice)> = selection.videos().collect();
        let subtitles: Vec<(&TorrentFile, &Choice)> = selection.subtitles().collect();
        if chosen.is_empty() {
            // Nunca baixou nada: não há o que semear. Mas só sai se o torrent
            // é deste grab e de mais ninguém; na dúvida, fica no cliente.
            self.drop_torrent(grab, &torrent, "torrent sem arquivo útil")
                .await?;
            return Err(Failure::Download(
                FailReason::Other,
                "nenhum arquivo do torrent é de episódio que se quer".into(),
            ));
        }
        // Pendente: algum episódio do grab que ele traz ainda sem arquivo e
        // sem `skip` — apagado ou dispensado no meio não é importado.
        let wanted: HashSet<i64> = grab.episode_ids.iter().copied().collect();
        let pending: Vec<&(&TorrentFile, &Choice)> = chosen
            .iter()
            .filter(|(_, choice)| !to_link(&entry.episodes, &wanted, &choice.episodes).is_empty())
            .collect();
        if pending.is_empty() {
            let ours = entry.files.iter().any(|f| {
                f.file.scene_name.as_deref() == Some(grab.title.as_str())
                    && entry
                        .episodes
                        .iter()
                        .any(|e| e.file_id == Some(f.id) && wanted.contains(&e.id))
            });
            if !ours {
                super::grab::drop_queued(self.store, &self.client, grab).await;
            }
            return Ok(if ours {
                Step::default()
            } else {
                Step {
                    discard: Some(
                        "nada a importar: os episódios já estão na biblioteca ou foram dispensados"
                            .into(),
                    ),
                    ..Step::default()
                }
            });
        }
        if checking(&torrent) {
            return Ok(Step::waiting(CHECKING));
        }
        if selection.changed || torrent.progress < 1.0 || torrent.state == "moving" {
            return Ok(Step::waiting(progress(&torrent)));
        }

        let mut step = Step::default();
        let mut waiting = Vec::new();
        // O que o cliente dá por baixado e não está no disco.
        let mut vanished = Vec::new();

        let series = &entry.series;
        for (file, choice) in pending {
            let mut episodes: Vec<&acervo_store::CatalogEpisode> = entry
                .episodes
                .iter()
                .filter(|e| choice.episodes.contains(&e.id))
                .collect();
            episodes.sort_by_key(|e| (e.episode.season, e.episode.number));
            let titles: Vec<Option<String>> = episodes
                .iter()
                .map(|e| {
                    episode_title(
                        e.episode.title.as_deref(),
                        e.episode.air_date.as_deref(),
                        &grab.grabbed_at,
                        now,
                    )
                })
                .collect();
            if titles.iter().any(Option::is_none) {
                waiting.push(format!(
                    "aguardando o título de {}",
                    super::label(entry, &choice.episodes)
                ));
                continue;
            }
            let titles: Vec<String> = titles.into_iter().flatten().collect();
            // Um arquivo que não liga não desfaz os que já ligaram nesta
            // volta: eles são gravados, e o grab fica em atenção.
            let done = async {
                let named: Vec<(u16, u16, &str)> = episodes
                    .iter()
                    .zip(&titles)
                    .map(|(e, t)| (e.episode.season, e.episode.number, t.as_str()))
                    .collect();
                let mut quality = acervo_parser::parse_episode_quality(&grab.title);
                if quality.quality == acervo_parser::Quality::Unknown {
                    quality = acervo_parser::parse_episode_quality(&file.name);
                }
                let extension = Path::new(&file.name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("mkv");
                let relative = episode_path(
                    super::folder_title(series),
                    series.season_folder,
                    &named,
                    quality,
                    extension,
                );
                let destination = PathBuf::from(&series.path).join(&relative);
                let shown = destination.display().to_string();
                let source = client_path(&torrent, file);
                if crate::grab::not_found(&source).await {
                    return Ok(None);
                }
                let target = destination.clone();
                // O mesmo nome de um arquivo que já está lá: troca no lugar.
                let same_name = entry.files.iter().any(|f| f.file.relative_path == relative);
                let (from, to) = (source, target.clone());
                tokio::task::spawn_blocking(move || {
                    install(&from, &to, same_name.then_some(to.as_path()))
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("{e:#}"))?;
                let probe = crate::mediainfo::probe(&target).await;
                let languages = probe
                    .map(|p| p.audio_languages)
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| {
                        acervo_parser::parse_episode_title(&grab.title)
                            .map(|p| p.languages)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|l| *l != acervo_parser::Language::Unknown)
                            .collect()
                    });
                let record = EpisodeFile {
                    relative_path: relative,
                    size: file.size,
                    quality,
                    languages: languages.iter().map(|l| l.name().to_owned()).collect(),
                    release_group: acervo_parser::parse_episode_release_group(&grab.title),
                    scene_name: Some(grab.title.clone()),
                    date_added: Some(now_rfc3339()),
                };
                // Só os em Quero ganham o arquivo: o que já tem arquivo ou foi
                // dispensado não é religado (não há upgrade), e o antigo fica.
                let linked = to_link(&entry.episodes, &wanted, &choice.episodes);
                // As legendas do torrent que são destes episódios vão ao lado.
                let own: Vec<&TorrentFile> = subtitles
                    .iter()
                    .filter(|(_, sub)| {
                        !sub.episodes.is_empty()
                            && sub.episodes.iter().all(|e| choice.episodes.contains(e))
                    })
                    .map(|(f, _)| *f)
                    .collect();
                let beside = self
                    .link_subtitles(&torrent, entry, &record.relative_path, &own)
                    .await;
                Ok::<_, Failure>(Some(Linked {
                    shown,
                    quality: quality.quality,
                    record: SeriesImport {
                        file: record,
                        episode_ids: linked,
                        subtitles: beside,
                    },
                }))
            }
            .await;
            match done {
                Ok(Some(linked)) => step.linked.push(linked),
                Ok(None) => vanished.push(client_path(&torrent, file)),
                Err(Failure::Import(error)) if !step.linked.is_empty() => {
                    step.failed = Some(error);
                    break;
                }
                Err(failure) => return Err(failure),
            }
        }
        // Os que sumiram do disco: o mesmo torrent é verificado e baixa de
        // novo, sem desfazer os que ligaram.
        if let Some(source) = vanished.into_iter().next() {
            match crate::grab::source_gone(&self.client, &torrent, &source).await {
                Ok(Some(why)) => waiting.push(why),
                Ok(None) => {}
                Err(failure) if !step.linked.is_empty() => {
                    if let Failure::Import(error) = failure {
                        step.failed.get_or_insert(error);
                    }
                }
                Err(failure) => return Err(failure),
            }
        }
        if !waiting.is_empty() {
            step.waiting = Some(waiting.join("; "));
        }
        Ok(step)
    }
}

impl Importer<'_> {
    /// Apaga do cliente o torrent de um grab que desistiu, se ele é só deste
    /// grab e nenhum arquivo tem outro link; na dúvida, fica, com aviso.
    async fn drop_torrent(
        &self,
        grab: &SeriesGrab,
        torrent: &TorrentInfo,
        why: &str,
    ) -> Result<(), Failure> {
        if super::grab::owns(self.store, &self.client, grab, torrent).await {
            self.client
                .delete(&[DownloadHash::new(grab.hash.clone())], true)
                .await
                .map_err(|e| e.to_string())?;
        } else {
            tracing::warn!(
                release = grab.title,
                "{why}, mas não é só deste grab: fica no cliente"
            );
        }
        Ok(())
    }
}

/// Tira do disco os arquivos que a importação trocou, já fora do catálogo,
/// com as legendas deles que vieram do torrent; a posta à mão fica.
fn remove_replaced(entry: &CatalogSeries, replaced: &[CatalogEpisodeFile]) {
    for old in replaced {
        for sub in entry.subtitles.iter().filter(|s| s.owner == old.id) {
            let path = PathBuf::from(&entry.series.path).join(&sub.subtitle.relative_path);
            if let Err(error) =
                crate::subtitles::remove_if_from_torrent(&path, Some(sub.subtitle.origin))
            {
                tracing::warn!(arquivo = %path.display(), "legenda antiga não apagada: {error}");
            }
        }
        let path = PathBuf::from(&entry.series.path).join(&old.file.relative_path);
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(arquivo = %path.display(), "antigo não apagado: {error}");
        }
    }
}

/// Grava o que a volta deu: os arquivos e o estado do grab (numa
/// transação), ou o bloqueio e a nova busca. `false`, sem gravar nada, se
/// outro fluxo já fechou o grab: ele sai desta rodada.
#[allow(clippy::too_many_lines)] // Um braço por desfecho da volta; dividir só espalharia.
async fn finish(
    config: &Config,
    store: &Store,
    catalog: Option<&Catalog>,
    grab: &SeriesGrab,
    entry: &CatalogSeries,
    step: Result<Step, Failure>,
    line: &mut ImportLine,
) -> Result<bool> {
    let at = now_rfc3339();
    match step {
        Ok(Step {
            discard: Some(reason),
            ..
        }) => {
            line.estado = "descartado";
            line.detalhe = Some(reason.clone());
            give_up(store, grab, entry, &reason, None, Kind::Ignored).await
        }
        Ok(step) => {
            let (records, shown): (Vec<SeriesImport>, Vec<(String, acervo_parser::Quality)>) = step
                .linked
                .into_iter()
                .map(|l| (l.record, (l.shown, l.quality)))
                .unzip();
            // O arquivo que não ligou põe o grab em atenção, como a falha de
            // importação; os que ligaram são gravados do mesmo jeito.
            let message = step
                .failed
                .as_ref()
                .map(|error| format!("importação: {error}"))
                .or_else(|| step.waiting.clone());
            let state = if message.is_some() {
                GrabState::Downloading
            } else {
                GrabState::Imported
            };
            let Some(written) = store
                .import_series_files(grab.id, entry.id, &records, state, message.as_deref(), &at)
                .await?
            else {
                tracing::warn!(
                    release = grab.title,
                    "grab encerrado por outro fluxo no meio da importação: o que foi ligado \
                     fica para a verificação do disco"
                );
                return Ok(false);
            };
            if state == GrabState::Imported {
                watch().forget(&grab.hash);
            }
            for ((record, (destination, quality)), (_, replaced)) in
                records.iter().zip(&shown).zip(&written)
            {
                remove_replaced(entry, replaced);
                let linked = &record.episode_ids;
                events::record(
                    store,
                    Event {
                        source_title: Some(grab.title.clone()),
                        quality: Some(*quality),
                        indexer: Some(grab.indexer.clone()),
                        download_id: Some(grab.hash.clone()),
                        message: Some(destination.clone()),
                        poster: entry.series.poster.clone(),
                        ..Event::series(
                            Kind::Imported,
                            entry.id,
                            super::label(entry, linked),
                            linked,
                        )
                    },
                )
                .await;
            }
            line.destinos = shown
                .into_iter()
                .map(|(destination, _)| destination)
                .collect();
            line.estado = if step.failed.is_some() {
                line.detalhe = step.failed;
                "atencao"
            } else if line.destinos.is_empty() && step.waiting.is_some() {
                line.detalhe = step.waiting;
                "baixando"
            } else {
                line.detalhe = step.waiting;
                "importado"
            };
            Ok(true)
        }
        Err(failure @ (Failure::Download(..) | Failure::Lost(..))) => {
            // O arquivo sumido do disco, ou o torrent do cliente, não é culpa
            // do release: sem bloqueio.
            let (cause, error, block) = match failure {
                Failure::Download(cause, error) => (cause, error, Some(cause)),
                Failure::Lost(cause, error) => (cause, error, None),
                Failure::Import(_) => unreachable!("o braço de baixo trata"),
            };
            line.estado = "falhou";
            line.detalhe = Some(error.clone());
            if !give_up(store, grab, entry, &error, block, Kind::Failed).await? {
                return Ok(false);
            }
            if let Some(catalog) = catalog {
                let only: HashSet<i64> = grab.episode_ids.iter().copied().collect();
                // O avulso que o tracker apagou deu lugar ao pacote.
                let prefer_pack = cause == FailReason::Unregistered;
                match super::search::series_now_with(
                    config,
                    store,
                    catalog,
                    entry.id,
                    Some(&only),
                    prefer_pack,
                )
                .await
                {
                    Ok(found) => tracing::info!(
                        serie = found.serie,
                        pegou = ?found.escolhidos,
                        "nova busca depois de falha"
                    ),
                    Err(error) => {
                        tracing::info!(serie = entry.id, "nova busca depois de falha: {error:#}");
                    }
                }
            }
            Ok(true)
        }
        Err(Failure::Import(error)) => {
            line.estado = "atencao";
            line.detalhe = Some(error.clone());
            let message = format!("importação: {error}");
            Ok(store
                .update_series_grab(grab.id, GrabState::Downloading, Some(&message), None)
                .await?)
        }
    }
}

/// Importa os downloads de série do acervo que terminaram.
///
/// # Errors
///
/// Catálogo ilegível ou cliente inalcançável. Falha de um download fica na
/// linha dele; os outros seguem.
pub async fn import_downloads(
    config: &Config,
    store: &Store,
    catalog: Option<&Catalog>,
) -> Result<Vec<ImportLine>> {
    let pending: Vec<SeriesGrab> = store
        .series_grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let client = qbit(config).await?;
    if !ensure_client_listed(config, &client, pending.len()).await? {
        return Ok(Vec::new());
    }
    let importer = Importer {
        store,
        free_space: client.free_space().await.ok(),
        client,
    };
    let mut lines = Vec::new();
    for grab in pending {
        // Lida a cada grab: o anterior pode ter ligado arquivo nesta série.
        let Some(entry) = store.series(grab.series_id).await? else {
            continue;
        };
        let mut line = ImportLine {
            serie: super::label(&entry, &grab.episode_ids),
            release: grab.title.clone(),
            estado: "baixando",
            detalhe: None,
            destinos: Vec::new(),
        };
        let step = importer.step(&grab, &entry).await;
        let stuck = matches!(
            step,
            Err(Failure::Import(_))
                | Ok(Step {
                    failed: Some(_),
                    ..
                })
        );
        if !finish(config, store, catalog, &grab, &entry, step, &mut line).await? {
            tracing::info!(
                serie = line.serie,
                release = grab.title,
                "grab encerrado por outro fluxo nesta volta: pulado"
            );
            continue;
        }
        crate::grab::notify_attention(
            store,
            &grab.hash,
            stuck,
            &line.serie,
            &grab.title,
            line.detalhe.as_deref().unwrap_or_default(),
            entry.series.poster.as_deref(),
        )
        .await;
        lines.push(line);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> OffsetDateTime {
        OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap()
    }

    #[test]
    fn so_o_que_esta_em_quero_ganha_o_arquivo() {
        let episode = |id: i64, file_id: Option<i64>, skip: Option<acervo_store::Skip>| {
            acervo_store::CatalogEpisode {
                id,
                episode: acervo_store::Episode {
                    season: 1,
                    number: u16::try_from(id).unwrap(),
                    tmdb_id: None,
                    title: None,
                    air_date: None,
                    overview: None,
                    runtime: 0,
                },
                skip,
                skipped_at: None,
                file_id,
            }
        };
        let episodes = [
            episode(1, None, None),
            // Já tem arquivo: o antigo fica, sem troca.
            episode(2, Some(50), None),
            // Dispensado no meio do download.
            episode(3, None, Some(acervo_store::Skip::Deleted)),
            episode(4, None, None),
            episode(5, None, None),
        ];
        // O grab foi buscar 1 a 4; o 5 está no arquivo, mas não no grab.
        let wanted = HashSet::from([1, 2, 3, 4]);
        assert_eq!(to_link(&episodes, &wanted, &[1, 2, 3, 4, 5]), [1, 4]);
        assert!(to_link(&episodes, &wanted, &[2, 3]).is_empty());
    }

    #[test]
    fn titulo_obrigatorio_com_tba_depois_de_48_horas() {
        let grabbed = "2026-10-01T00:00:00Z";
        // Com título, sempre ele.
        assert_eq!(
            episode_title(Some("Pilot"), None, grabbed, at("2026-10-01T01:00:00Z")),
            Some("Pilot".into())
        );
        // Sem título, exibido há menos de 48 h: espera.
        assert_eq!(
            episode_title(
                None,
                Some("2026-10-01"),
                grabbed,
                at("2026-10-02T23:59:00Z")
            ),
            None
        );
        assert_eq!(
            episode_title(
                Some("  "),
                Some("2026-10-01"),
                grabbed,
                at("2026-10-03T00:00:00Z")
            ),
            Some(TBA.into())
        );
        // Sem data, conta do grab.
        assert_eq!(
            episode_title(None, None, grabbed, at("2026-10-02T00:00:00Z")),
            None
        );
        assert_eq!(
            episode_title(None, None, grabbed, at("2026-10-03T00:00:00Z")),
            Some(TBA.into())
        );
    }
}

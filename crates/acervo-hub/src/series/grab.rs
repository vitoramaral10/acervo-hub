//! Grab de série: como o de filmes (parado, com a tag da fila, na categoria
//! do acervo), mais a escolha de arquivos — de um pacote só baixa o que
//! falta. A escolha é pura ([`choose_files`]) e idempotente: roda ao
//! adicionar, antes de o torrent sair da fila e a cada volta da importação.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use acervo_api::Catalog;
use acervo_clients::{NewTorrent, QbitClient, TorrentFile, TorrentInfo, client_path};
use acervo_core::DownloadHash;
use acervo_decision::{SceneMapping, scene_to_catalog};
use acervo_parser::{ParsedEpisode, parse_episode_path, parse_episode_title};
use acervo_store::{CatalogSeries, FailReason, GrabState, SeriesGrab, Store};
use anyhow::{Context, Result, bail};

use super::remove::OnDisk;
use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::events::{self, Event, Kind};
use crate::grab::{QUEUE_TAG, QUEUED, VIDEO, left, qbit, start_queued};

/// Folga entre o relógio do cliente e o do acervo, em segundos.
const CLOCK_SLACK: i64 = 300;

/// Pastas de material extra dentro de um torrent.
const EXTRAS: &[&str] = &[
    "extras",
    "extra",
    "featurettes",
    "behind the scenes",
    "deleted scenes",
    "interviews",
    "trailers",
    "shorts",
];

/// Prioridade normal no qBittorrent; zero não baixa.
const NORMAL: u8 = 1;

/// O destino de um arquivo do torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// `index` do arquivo no torrent.
    pub index: usize,
    pub priority: u8,
    /// Os episódios do catálogo que o arquivo traz (vazio fora dos vídeos e
    /// das legendas).
    pub episodes: Vec<i64>,
    /// Legenda, não vídeo: vai junto do vídeo dos mesmos episódios.
    pub subtitle: bool,
}

fn is_video(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()))
}

/// Amostra ou extra: pelo nome do arquivo ou por uma das pastas.
pub(crate) fn is_sample_or_extra(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    stem.contains("sample")
        || stem.ends_with("-trailer")
        || parts
            .iter()
            .any(|dir| *dir == "sample" || *dir == "samples" || EXTRAS.contains(dir))
}

/// Os episódios do catálogo que um nome lido cobre, já na numeração do
/// catálogo (a de cena traduzida).
pub(crate) fn episodes_of(
    parsed: &ParsedEpisode,
    episodes: &[(i64, u16, u16)],
    scene: &[SceneMapping],
) -> Vec<i64> {
    if parsed.episodes.is_empty() {
        return Vec::new();
    }
    let numbered = scene_to_catalog(scene, parsed.season, &parsed.episodes, |s, n| {
        episodes
            .iter()
            .any(|(_, season, number)| (*season, *number) == (s, n))
    });
    episodes
        .iter()
        .filter(|(_, season, number)| numbered.contains(&(*season, *number)))
        .map(|(id, _, _)| *id)
        .collect()
}

/// Os episódios de uma legenda: pelo caminho dela (nome e pasta, como os
/// vídeos); senão por uma das pastas (da mais perto para a mais longe);
/// senão pelo vídeo da mesma pasta cujo stem é o começo do nome dela; senão,
/// se o torrent tem um vídeo só, os dele.
fn subtitle_episodes(
    name: &str,
    videos: &[(&str, Vec<i64>)],
    episodes: &[(i64, u16, u16)],
    scene: &[SceneMapping],
) -> Vec<i64> {
    let read = |text: &str| {
        parse_episode_title(text)
            .map(|p| episodes_of(&p, episodes, scene))
            .unwrap_or_default()
    };
    let by_path = parse_episode_path(name)
        .map(|p| episodes_of(&p, episodes, scene))
        .unwrap_or_default();
    if !by_path.is_empty() {
        return by_path;
    }
    let mut parts: Vec<&str> = name.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    for dir in parts.iter().rev() {
        let by_dir = read(dir);
        if !by_dir.is_empty() {
            return by_dir;
        }
    }
    let folder = parts.join("/");
    if let Some((_, covered)) = videos.iter().find(|(video, covered)| {
        let (dir, video_file) = video.rsplit_once('/').unwrap_or(("", video));
        let stem = video_file.rsplit_once('.').map_or(video_file, |(s, _)| s);
        !covered.is_empty()
            && dir == folder
            && file
                .strip_prefix(stem)
                .is_some_and(|rest| rest.starts_with('.'))
    }) {
        return covered.clone();
    }
    match videos {
        [(_, covered)] => covered.clone(),
        _ => Vec::new(),
    }
}

/// A escolha de arquivos, sem IO: cada vídeo é lido pelo caminho (o nome e,
/// se ele não basta, a pasta); o que cruza algum episódio de `wanted` fica
/// com prioridade normal, todo o resto — episódio que já se tem ou foi
/// dispensado, legenda, amostra, extra — com zero. Num avulso, o vídeo
/// principal sem episódio no nome é o do release. `episodes` são `(id,
/// temporada, número)` da série.
#[must_use]
pub fn choose_files(
    files: &[TorrentFile],
    episodes: &[(i64, u16, u16)],
    wanted: &HashSet<i64>,
    release: &str,
    scene: &[SceneMapping],
) -> Vec<Choice> {
    let single = parse_episode_title(release)
        .filter(|p| !p.episodes.is_empty() && !p.full_season && !p.multi_season)
        .map(|p| episodes_of(&p, episodes, scene))
        .unwrap_or_default();
    let candidates: Vec<TorrentFile> = files
        .iter()
        .filter(|f| is_video(&f.name) && !is_sample_or_extra(&f.name))
        .cloned()
        .collect();
    let main = crate::grab::main_video(&candidates).map(|f| f.index);
    let priority = |covered: &[i64]| {
        if covered.iter().any(|e| wanted.contains(e)) {
            NORMAL
        } else {
            0
        }
    };
    let mut choices: Vec<Choice> = files
        .iter()
        .map(|file| {
            if !is_video(&file.name) || is_sample_or_extra(&file.name) {
                return Choice {
                    index: file.index,
                    priority: 0,
                    episodes: Vec::new(),
                    subtitle: false,
                };
            }
            let mut covered = parse_episode_path(&file.name)
                .map(|p| episodes_of(&p, episodes, scene))
                .unwrap_or_default();
            if covered.is_empty() && main == Some(file.index) {
                covered.clone_from(&single);
            }
            Choice {
                index: file.index,
                priority: priority(&covered),
                episodes: covered,
                subtitle: false,
            }
        })
        .collect();
    // Legenda de episódio escolhido baixa junto.
    let videos: Vec<(&str, Vec<i64>)> = files
        .iter()
        .zip(&choices)
        .filter(|(_, c)| !c.episodes.is_empty())
        .map(|(f, c)| (f.name.as_str(), c.episodes.clone()))
        .collect();
    for (file, choice) in files.iter().zip(choices.iter_mut()) {
        if crate::subtitles::subtitle_extension(&file.name).is_none()
            || is_sample_or_extra(&file.name)
        {
            continue;
        }
        let covered = subtitle_episodes(&file.name, &videos, episodes, scene);
        choice.priority = priority(&covered);
        choice.episodes = covered;
        choice.subtitle = true;
    }
    choices
}

/// Os episódios da série como a escolha os lê.
pub(crate) fn numbers(entry: &CatalogSeries) -> Vec<(i64, u16, u16)> {
    entry
        .episodes
        .iter()
        .map(|e| (e.id, e.episode.season, e.episode.number))
        .collect()
}

/// Uma escolha aplicada no cliente.
#[derive(Debug)]
pub(crate) struct Selection {
    pub files: Vec<TorrentFile>,
    pub choices: Vec<Choice>,
    /// Alguma prioridade mudou agora: o progresso do cliente ainda não conta.
    pub changed: bool,
}

impl Selection {
    /// Os vídeos escolhidos, com o destino de cada um.
    pub fn videos(&self) -> impl Iterator<Item = (&TorrentFile, &Choice)> {
        self.chosen().filter(|(_, c)| !c.subtitle)
    }

    /// As legendas escolhidas.
    pub fn subtitles(&self) -> impl Iterator<Item = (&TorrentFile, &Choice)> {
        self.chosen().filter(|(_, c)| c.subtitle)
    }

    /// Os arquivos escolhidos (vídeos e legendas), com o destino de cada um.
    pub fn chosen(&self) -> impl Iterator<Item = (&TorrentFile, &Choice)> {
        self.files.iter().filter_map(|file| {
            self.choices
                .iter()
                .find(|c| c.index == file.index && c.priority > 0)
                .map(|c| (file, c))
        })
    }

    /// Bytes que ainda faltam baixar dos escolhidos.
    pub fn need(&self) -> u64 {
        self.chosen().map(|(f, _)| left(f.size, f.progress)).sum()
    }

    pub fn size(&self) -> u64 {
        self.chosen().map(|(f, _)| f.size).sum()
    }
}

/// Lê os arquivos do torrent e acerta as prioridades (só com `write`; sem,
/// só diz o que faria). `None` quando o cliente ainda não tem a lista
/// (magnet sem metadados). Prioridade acima da normal, posta à mão, fica.
///
/// # Errors
///
/// Cliente inalcançável.
pub(crate) async fn apply_selection(
    client: &QbitClient,
    hash: &str,
    entry: &CatalogSeries,
    wanted: &[i64],
    release: &str,
    write: bool,
) -> Result<Option<Selection>> {
    let hash = DownloadHash::new(hash);
    let files = client
        .files(&hash)
        .await
        .context("listando os arquivos do torrent")?;
    if files.is_empty() {
        return Ok(None);
    }
    let wanted: HashSet<i64> = wanted.iter().copied().collect();
    let choices = choose_files(
        &files,
        &numbers(entry),
        &wanted,
        release,
        &super::scene(entry),
    );
    let current = |index: usize| files.iter().find(|f| f.index == index).map(|f| f.priority);
    let off: Vec<usize> = choices
        .iter()
        .filter(|c| c.priority == 0 && current(c.index) != Some(0))
        .map(|c| c.index)
        .collect();
    let on: Vec<usize> = choices
        .iter()
        .filter(|c| c.priority > 0 && current(c.index) == Some(0))
        .map(|c| c.index)
        .collect();
    if write {
        client
            .set_file_priority(&hash, &off, 0)
            .await
            .context("tirando arquivos do download")?;
        client
            .set_file_priority(&hash, &on, NORMAL)
            .await
            .context("escolhendo arquivos do download")?;
    }
    let changed = !off.is_empty() || !on.is_empty();
    let files = if changed {
        files
            .into_iter()
            .map(|mut f| {
                if off.contains(&f.index) {
                    f.priority = 0;
                } else if on.contains(&f.index) {
                    f.priority = NORMAL;
                }
                f
            })
            .collect()
    } else {
        files
    };
    Ok(Some(Selection {
        files,
        choices,
        changed,
    }))
}

/// Antes de o torrent sair da fila: escolhe os arquivos e diz quanto falta
/// baixar. `None` se ainda não dá para sair (sem metadados, ou nada a
/// baixar).
///
/// # Errors
///
/// Cliente inalcançável ou catálogo ilegível.
pub(crate) async fn prepare(
    store: &Store,
    client: &QbitClient,
    grab: &SeriesGrab,
) -> Result<Option<u64>> {
    let Some(entry) = store.series(grab.series_id).await? else {
        return Ok(None);
    };
    let selection = apply_selection(
        client,
        &grab.hash,
        &entry,
        &grab.episode_ids,
        &grab.title,
        true,
    )
    .await?;
    Ok(selection
        .filter(|s| s.videos().next().is_some())
        .map(|s| s.need()))
}

/// Manda um release ao cliente, escolhe os arquivos e grava o grab com os
/// episódios `wanted`. Devolve o id do grab.
///
/// Torrent que já está no cliente é recusado: é de outro grab, de filme ou
/// de fora do acervo, e mexer nas prioridades dele ou apagá-lo depois
/// estragaria o que é dos outros.
///
/// # Errors
///
/// Torrent já no cliente, `.torrent` inválido, cliente inalcançável ou
/// falha ao registrar.
pub async fn send(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    series_id: i64,
    release: &acervo_indexers::Release,
    quality: acervo_parser::Quality,
    wanted: &[i64],
) -> Result<i64> {
    if wanted.is_empty() {
        bail!("nenhum episódio a pegar neste release");
    }
    let entry = store
        .series(series_id)
        .await?
        .context("série fora do catálogo")?;
    let (torrent, hash) = crate::grab::fetch_torrent(catalog, release).await?;
    let magnet = matches!(torrent, NewTorrent::Magnet(_));
    let client = qbit(config).await?;
    if client.torrent(&hash).await?.is_some() {
        bail!("o torrent já está no cliente");
    }
    crate::grab::add_queued(config, &client, torrent, &hash, magnet, false).await?;
    // O `.torrent` traz a lista de arquivos: o cliente a mostra assim que
    // termina de carregá-lo. O magnet só depois dos metadados; aí a fila e a
    // importação escolhem.
    let mut size = release.size;
    if !magnet {
        let mut last = None;
        for _ in 0..10 {
            match apply_selection(&client, &hash, &entry, wanted, &release.title, true).await {
                Ok(Some(selection)) => {
                    size = selection.size();
                    last = None;
                    break;
                }
                // Recém-chegado, o cliente pode nem conhecer o hash ainda.
                Ok(None) => {}
                Err(error) => last = Some(error),
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if let Some(error) = last {
            tracing::warn!(release = release.title, "escolha de arquivos: {error:#}");
        }
    }
    let id = store
        .record_series_grab(&SeriesGrab {
            id: 0,
            series_id,
            episode_ids: wanted.to_vec(),
            hash: hash.clone(),
            title: release.title.clone(),
            indexer: release.indexer.clone(),
            quality,
            size,
            grabbed_at: now_rfc3339(),
            state: GrabState::Downloading,
            message: Some(QUEUED.into()),
            finished_at: None,
        })
        .await
        .context("registrando o grab")?;
    if let Err(error) = start_queued(config, store, &client).await {
        tracing::warn!("fila de downloads: {error:#}");
    }
    events::record(
        store,
        Event {
            source_title: Some(release.title.clone()),
            quality: Some(quality),
            indexer: Some(release.indexer.clone()),
            download_id: Some(hash),
            poster: entry.series.poster.clone(),
            ..Event::series(
                Kind::Grabbed,
                series_id,
                super::label(&entry, wanted),
                wanted,
            )
        },
    )
    .await;
    Ok(id)
}

/// Se o torrent de um grab pode sair do cliente sem levar o que é dos
/// outros, sem IO. Só quando foi este grab que o pôs lá (entrou no cliente
/// junto com o grab, não antes) e nenhum outro grab usa o mesmo hash, e
/// nenhum arquivo dele tem outro link. `files` `None` é leitura duvidosa.
/// Na dúvida, fica.
#[must_use]
pub fn deletable(added_on: i64, grabbed_at: &str, shared: bool, files: Option<&[OnDisk]>) -> bool {
    let Ok(grabbed) =
        time::OffsetDateTime::parse(grabbed_at, &time::format_description::well_known::Rfc3339)
    else {
        return false;
    };
    added_on > 0
        && added_on >= grabbed.unix_timestamp() - CLOCK_SLACK
        && !shared
        && files.is_some_and(|files| files.iter().all(|f| f.nlink <= 1))
}

/// De quem é um grab: filme ou série, pelo id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Movie(i64),
    Series(i64),
}

/// [`deletable`] para um grab de série, lendo o catálogo, o cliente e o
/// disco. Qualquer erro no caminho é dúvida: `false`.
pub(crate) async fn owns(
    store: &Store,
    client: &QbitClient,
    grab: &SeriesGrab,
    torrent: &TorrentInfo,
) -> bool {
    owns_torrent(
        store,
        client,
        Owner::Series(grab.id),
        &grab.grabbed_at,
        torrent,
    )
    .await
}

/// [`deletable`] para o grab `owner`, de filme ou de série, que saiu em
/// `grabbed_at` com o torrent `torrent`. Qualquer erro no caminho é dúvida:
/// `false`.
pub(crate) async fn owns_torrent(
    store: &Store,
    client: &QbitClient,
    owner: Owner,
    grabbed_at: &str,
    torrent: &TorrentInfo,
) -> bool {
    let hash = &torrent.hash;
    let shared = match (store.series_grabs().await, store.grabs().await) {
        (Ok(series), Ok(movies)) => {
            series
                .iter()
                .any(|g| owner != Owner::Series(g.id) && g.hash.eq_ignore_ascii_case(hash))
                || movies
                    .iter()
                    .any(|g| owner != Owner::Movie(g.id) && g.hash.eq_ignore_ascii_case(hash))
        }
        _ => return false,
    };
    let Ok(files) = client.files(&DownloadHash::new(hash.clone())).await else {
        return false;
    };

    let paths: Vec<PathBuf> = files
        .iter()
        .map(|file| client_path(torrent, file))
        .collect();
    let files = tokio::task::spawn_blocking(move || super::remove::stat_all(&paths))
        .await
        .ok()
        .flatten();
    deletable(torrent.added_on, grabbed_at, shared, files.as_deref())
}

/// Apaga do cliente o torrent de um grab que sai ainda na fila, sem nada
/// baixado: sem grab em andamento, ele ficaria no cliente sem dono. Só se
/// for dele ([`owns`]); erro vira aviso.
pub(crate) async fn drop_queued(store: &Store, client: &QbitClient, grab: &SeriesGrab) {
    let torrent = match client.torrent(&grab.hash).await {
        Ok(Some(torrent)) if torrent.has_tag(QUEUE_TAG) && torrent.progress <= 0.0 => torrent,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(
                release = grab.title,
                "torrent da fila não conferido: {error}"
            );
            return;
        }
    };
    if !owns(store, client, grab, &torrent).await {
        tracing::warn!(
            release = grab.title,
            "torrent na fila sem grab, mas não é só deste: fica no cliente"
        );
        return;
    }
    if let Err(error) = client
        .delete(&[DownloadHash::new(grab.hash.clone())], true)
        .await
    {
        tracing::warn!(release = grab.title, "torrent da fila não apagado: {error}");
    }
}

/// Episódios do catálogo que um nome de release cobre: pacote, a
/// temporada; multi-temporada, todas menos a 0.
pub(crate) fn covered(parsed: &ParsedEpisode, entry: &CatalogSeries) -> Vec<i64> {
    let numbered = scene_to_catalog(
        &super::scene(entry),
        parsed.season,
        &parsed.episodes,
        super::has_episode(entry),
    );
    entry
        .episodes
        .iter()
        .filter(|e| {
            let e = &e.episode;
            if parsed.multi_season {
                e.season != 0
            } else if parsed.full_season || parsed.partial_season {
                e.season == parsed.season
            } else {
                numbered.contains(&(e.season, e.number))
            }
        })
        .map(|e| e.id)
        .collect()
}

/// Manda um release escolhido na mão: pula a decisão, mas só vai buscar os
/// episódios em Quero que ele cobre (já exibidos ou não), e a escolha de
/// arquivos vale igual.
///
/// # Errors
///
/// Série fora do catálogo, nome sem temporada nem episódio, nada em Quero
/// no release, `.torrent` inválido ou cliente inalcançável.
pub async fn send_chosen(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    series_id: i64,
    release: &acervo_indexers::Release,
) -> Result<i64> {
    let entry = store
        .series(series_id)
        .await?
        .context("série fora do catálogo")?;
    let parsed = parse_episode_title(&release.title)
        .context("o nome do release não diz temporada nem episódio")?;
    let queued = super::queued(&store.series_grabs().await?);
    let covers = covered(&parsed, &entry);
    let wanted: Vec<i64> = entry
        .episodes
        .iter()
        .filter(|e| covers.contains(&e.id))
        .filter(|e| e.file_id.is_none() && e.skip.is_none() && !queued.contains(&e.id))
        .map(|e| e.id)
        .collect();
    if wanted.is_empty() {
        bail!(
            "nenhum episódio em Quero neste release (todos na biblioteca, dispensados ou já baixando)"
        );
    }
    let quality = acervo_parser::parse_episode_quality(&release.title).quality;
    send(config, store, catalog, series_id, release, quality, &wanted).await
}

/// Desiste de um download de série: marca como falho e, com `block`,
/// bloqueia o release (com a série) com esse motivo — as duas coisas numa
/// transação — e registra o evento. `false`, sem gravar nada, se outro fluxo
/// já fechou o grab.
///
/// # Errors
///
/// Falha de escrita.
pub async fn give_up(
    store: &Store,
    grab: &SeriesGrab,
    entry: &CatalogSeries,
    reason: &str,
    block: Option<FailReason>,
    kind: Kind,
) -> Result<bool> {
    let at = now_rfc3339();
    let blocked = block.map(|cause| acervo_store::Blocked {
        id: 0,
        movie_id: None,
        series_id: Some(grab.series_id),
        source_title: grab.title.clone(),
        indexer: Some(grab.indexer.clone()),
        quality: Some(grab.quality),
        size: Some(grab.size),
        hash: Some(grab.hash.clone()),
        at: at.clone(),
        message: Some(reason.to_owned()),
        reason: cause,
    });
    if !store
        .fail_series_grab(grab.id, reason, blocked.as_ref(), &at)
        .await?
    {
        tracing::info!(
            release = grab.title,
            "grab já encerrado por outro fluxo: nada a desistir"
        );
        return Ok(false);
    }
    crate::grab::watch().forget(&grab.hash);
    events::record(
        store,
        Event {
            source_title: Some(grab.title.clone()),
            quality: Some(grab.quality),
            indexer: Some(grab.indexer.clone()),
            download_id: Some(grab.hash.clone()),
            message: Some(reason.to_owned()),
            poster: entry.series.poster.clone(),
            ..Event::series(
                kind,
                entry.id,
                super::label(entry, &grab.episode_ids),
                &grab.episode_ids,
            )
        },
    )
    .await;
    Ok(true)
}

/// Tira um download de série da fila, como o de filme: apaga do cliente
/// (com os arquivos) se pedido — senão, só tira a tag da fila —, bloqueia se
/// pedido e busca de novo os episódios dele se pedido.
///
/// # Errors
///
/// Download desconhecido, cliente inalcançável ou falha de escrita.
pub async fn remove_download(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    grab_id: i64,
    remove_from_client: bool,
    blocklist: bool,
    search: bool,
) -> Result<()> {
    let grab = store
        .series_grabs()
        .await?
        .into_iter()
        .find(|g| g.id == grab_id)
        .context("download desconhecido")?;
    if grab.state != GrabState::Downloading {
        bail!("o download já terminou");
    }
    let entry = store
        .series(grab.series_id)
        .await?
        .context("a série do download saiu do catálogo")?;
    let client = qbit(config).await?;
    if remove_from_client {
        client
            .delete(&[DownloadHash::new(grab.hash.clone())], true)
            .await
            .context("apagando do qBittorrent")?;
    } else {
        client
            .remove_tag(&[&grab.hash], QUEUE_TAG)
            .await
            .context("tirando da fila do qBittorrent")?;
    }
    let (reason, kind) = if blocklist {
        ("marcado como falho na tela", Kind::Failed)
    } else {
        ("tirado da fila na tela", Kind::Ignored)
    };
    let block = blocklist.then_some(FailReason::Other);
    if !give_up(store, &grab, &entry, reason, block, kind).await? {
        // A importação fechou o grab no meio: nada mais a fazer por ele.
        return Ok(());
    }
    if search {
        let only: HashSet<i64> = grab.episode_ids.iter().copied().collect();
        if let Err(error) =
            super::search::series_now(config, store, catalog, entry.id, Some(&only)).await
        {
            tracing::info!(serie = entry.id, "nova busca: {error:#}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(index: usize, name: &str, size: u64) -> TorrentFile {
        serde_json::from_value(serde_json::json!({
            "index": index, "name": name, "size": size, "priority": 1, "progress": 0.0
        }))
        .unwrap()
    }

    fn season_one() -> Vec<(i64, u16, u16)> {
        (1..=10).map(|n| (100 + i64::from(n), 1, n)).collect()
    }

    #[test]
    fn pacote_com_um_so_episodio_em_quero() {
        let mut files: Vec<TorrentFile> = (1_u8..=10)
            .map(|n| {
                file(
                    usize::from(n) - 1,
                    &format!("Show.S01.1080p.WEB-DL-GRP/Show.S01E{n:02}.1080p.WEB-DL-GRP.mkv"),
                    1_000,
                )
            })
            .collect();
        files.push(file(
            10,
            "Show.S01.1080p.WEB-DL-GRP/Subs/Show.S01E10.por.srt",
            1,
        ));
        files.push(file(
            11,
            "Show.S01.1080p.WEB-DL-GRP/Sample/show.s01e10.sample.mkv",
            20,
        ));
        files.push(file(12, "Show.S01.1080p.WEB-DL-GRP/Show.S01E10.nfo", 1));
        // E01–E09 em Tenho; só o E10 em Quero.
        let wanted = HashSet::from([110]);
        let choices = choose_files(
            &files,
            &season_one(),
            &wanted,
            "Show.S01.1080p.WEB-DL-GRP",
            &[],
        );
        let on: Vec<usize> = choices
            .iter()
            .filter(|c| c.priority > 0)
            .map(|c| c.index)
            .collect();
        // O vídeo do E10 e a legenda dele.
        assert_eq!(on, [9, 10]);
        assert_eq!(choices[9].episodes, [110]);
        // Os outros vídeos sabem que episódio são, mas não baixam.
        assert_eq!(choices[0].episodes, [101]);
        assert_eq!(choices[0].priority, 0);
        // A legenda do episódio escolhido baixa junto.
        assert_eq!(choices[10].priority, NORMAL);
        assert!(choices[10].subtitle);
        assert_eq!(choices[10].episodes, [110]);
        // Amostra e o resto ficam com zero.
        for index in [11, 12] {
            assert_eq!(choices[index].priority, 0, "arquivo {index}");
            assert!(choices[index].episodes.is_empty());
        }
    }

    #[test]
    fn legenda_casa_pelo_nome_pela_pasta_ou_pelo_video() {
        let files = [
            file(0, "Show.S01/Show.S01E01.1080p.mkv", 1_000),
            file(1, "Show.S01/Show.S01E02.1080p.mkv", 1_000),
            // Pelo nome.
            file(2, "Show.S01/Subs/Show.S01E02.por.srt", 1),
            // Pela pasta.
            file(3, "Show.S01/Subs/Show.S01E01.1080p/2_English.srt", 1),
            // Pelo stem do vídeo.
            file(4, "Show.S01/Show.S01E01.1080p.pt-BR.srt", 1),
            // Sem como saber, num pacote: fica.
            file(5, "Show.S01/Subs/English.srt", 1),
        ];
        let episodes = [(1, 1, 1), (2, 1, 2)];
        let choices = choose_files(
            &files,
            &episodes,
            &HashSet::from([1]),
            "Show.S01.1080p",
            &[],
        );
        let state: Vec<(u8, Vec<i64>)> = choices
            .iter()
            .map(|c| (c.priority, c.episodes.clone()))
            .collect();
        assert_eq!(
            state,
            [
                (NORMAL, vec![1]),
                (0, vec![2]),
                (0, vec![2]),
                (NORMAL, vec![1]),
                (NORMAL, vec![1]),
                (0, vec![]),
            ]
        );
        // Avulso: a legenda sem episódio no nome é do vídeo único.
        let single = [
            file(0, "Show.S01E01.1080p/video.mkv", 1_000),
            file(1, "Show.S01E01.1080p/Subs/English.srt", 1),
        ];
        let choices = choose_files(
            &single,
            &episodes,
            &HashSet::from([1]),
            "Show.S01E01.1080p",
            &[],
        );
        assert_eq!(choices[1].priority, NORMAL);
    }

    #[test]
    fn legenda_de_pacote_pelo_caminho_e_pelo_video_da_mesma_pasta() {
        let files = [
            // O número só no nome, a temporada só na pasta.
            file(0, "Show/Season 1/01 - Piloto.mkv", 1_000),
            file(1, "Show/Season 2/01 - Abertura.mkv", 1_000),
            file(2, "Show/Season 1/01.srt", 1),
            file(3, "Show/Season 2/01.srt", 1),
            file(4, "Show/Season 2/01 - Abertura.pt-BR.srt", 1),
        ];
        let episodes = [(11, 1, 1), (21, 2, 1)];
        let choices = choose_files(&files, &episodes, &HashSet::from([21]), "Show", &[]);
        let got: Vec<(u8, Vec<i64>)> = choices
            .iter()
            .map(|c| (c.priority, c.episodes.clone()))
            .collect();
        assert_eq!(
            got,
            [
                (0, vec![11]),
                (NORMAL, vec![21]),
                (0, vec![11]),
                (NORMAL, vec![21]),
                (NORMAL, vec![21]),
            ]
        );
        // Pelo stem, sem número no nome nem na pasta: a pasta tem de ser a
        // mesma do vídeo.
        let videos = [("a/x.mkv", vec![1]), ("b/x.mkv", vec![2])];
        let numbered = [(1, 1, 1), (2, 1, 2)];
        assert_eq!(
            subtitle_episodes("b/x.eng.srt", &videos, &numbered, &[]),
            [2]
        );
        assert!(subtitle_episodes("c/x.eng.srt", &videos, &numbered, &[]).is_empty());
    }

    #[test]
    fn par_de_cena_que_existe_no_catalogo_vale_como_esta() {
        let scene = [SceneMapping {
            scene_season: 1,
            scene_episode: 13,
            season: 2,
            episode: 1,
        }];
        let files = [file(0, "Show.S01E13.1080p.mkv", 1_000)];
        // O catálogo tem S01E13: o arquivo é ele, não o S02E01.
        let episodes = [(7, 1, 13), (8, 2, 1)];
        let choices = choose_files(
            &files,
            &episodes,
            &HashSet::from([7]),
            "Show.S01E13",
            &scene,
        );
        assert_eq!(choices[0].episodes, [7]);
    }

    #[test]
    fn arquivo_com_numeracao_de_cena_vira_o_episodio_do_catalogo() {
        let scene = [SceneMapping {
            scene_season: 1,
            scene_episode: 13,
            season: 2,
            episode: 1,
        }];
        let files = [file(0, "Show.S01E13.1080p.mkv", 1_000)];
        // O catálogo não tem S01E13: traduz.
        let episodes = [(7, 1, 12), (8, 2, 1)];
        let choices = choose_files(
            &files,
            &episodes,
            &HashSet::from([8]),
            "Show.S01E13",
            &scene,
        );
        assert_eq!(choices[0].episodes, [8]);
        assert_eq!(choices[0].priority, NORMAL);
    }

    #[test]
    fn torrent_so_sai_se_e_do_grab_e_sem_outro_link() {
        let at = "2026-10-03T12:00:00Z";
        let grabbed = 1_791_028_800; // 2026-10-03T12:00:00Z
        let one = [OnDisk {
            dev: 1,
            ino: 1,
            nlink: 1,
        }];
        let linked = [OnDisk {
            dev: 1,
            ino: 2,
            nlink: 2,
        }];
        assert!(deletable(grabbed - 2, at, false, Some(&one)));
        assert!(deletable(grabbed, at, false, Some(&[])));
        // Já estava no cliente muito antes do grab: é de outro.
        assert!(!deletable(grabbed - 86_400, at, false, Some(&one)));
        // Outro grab usa o mesmo hash.
        assert!(!deletable(grabbed, at, true, Some(&one)));
        // Arquivo com outro link (biblioteca de alguém).
        assert!(!deletable(grabbed, at, false, Some(&linked)));
        // Sem saber: disco ilegível, cliente sem a data, data do grab ruim.
        assert!(!deletable(grabbed, at, false, None));
        assert!(!deletable(0, at, false, Some(&one)));
        assert!(!deletable(grabbed, "ontem", false, Some(&one)));
    }

    #[test]
    fn avulso_sem_episodio_no_nome_e_o_do_release() {
        let files = [
            file(0, "Show.S02E05.720p/video.mkv", 900),
            file(1, "Show.S02E05.720p/extras/bastidores.mkv", 500),
        ];
        let episodes = [(7, 2, 5), (8, 2, 6)];
        let choices = choose_files(
            &files,
            &episodes,
            &HashSet::from([7]),
            "Show.S02E05.720p.HDTV-GRP",
            &[],
        );
        assert_eq!(choices[0].priority, NORMAL);
        assert_eq!(choices[0].episodes, [7]);
        assert_eq!(choices[1].priority, 0);
    }

    #[test]
    fn multi_episodio_baixa_se_cruza_algum_quero() {
        let files = [file(0, "Show.S01E01E02.1080p.mkv", 2_000)];
        let episodes = [(1, 1, 1), (2, 1, 2)];
        let choices = choose_files(
            &files,
            &episodes,
            &HashSet::from([2]),
            "Show.S01E01E02",
            &[],
        );
        assert_eq!(choices[0].priority, NORMAL);
        assert_eq!(choices[0].episodes, [1, 2]);
        // Nada em Quero: tudo zero, inclusive o vídeo.
        let choices = choose_files(&files, &episodes, &HashSet::new(), "Show.S01E01E02", &[]);
        assert_eq!(choices[0].priority, 0);
    }
}

//! Grab de série: como o de filmes (parado, com a tag da fila, na categoria
//! do acervo), mais a escolha de arquivos — de um pacote só baixa o que
//! falta. A escolha é pura ([`choose_files`]) e idempotente: roda ao
//! adicionar, antes de o torrent sair da fila e a cada volta da importação.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::api::Catalog;
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

/// Prioridade alta e máxima de arquivo no qBittorrent: o segundo e o primeiro
/// episódio que faltam.
const HIGH: u8 = 6;
const MAXIMUM: u8 = 7;

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

/// Chave de ordem de episódios na série: `(especial, temporada, episódio)`.
/// Temporada 0 (especiais) vai para o fim; o menor vem primeiro.
pub(crate) type OrderKey = (bool, u16, u16);

/// A chave de ordem de um grab (ou arquivo): o menor `(temporada, episódio)`
/// entre os episódios `wanted` dele, na numeração da biblioteca. `episodes`
/// são `(id, temporada, número)` da série. Sem nenhum episódio conhecido,
/// vai para o fim de tudo.
#[must_use]
pub(crate) fn order_key(wanted: &[i64], episodes: &[(i64, u16, u16)]) -> OrderKey {
    episodes
        .iter()
        .filter(|(id, _, _)| wanted.contains(id))
        .map(|(_, season, number)| (*season == 0, *season, *number))
        .min()
        .unwrap_or((true, u16::MAX, u16::MAX))
}

/// A prioridade que cada arquivo escolhido deve ter para baixar em ordem de
/// episódio, por `index`: o primeiro episódio que falta em prioridade máxima,
/// o seguinte em alta, os demais em normal. Arquivos do mesmo episódio (vídeo
/// e legendas) andam juntos. Episódio com todos os arquivos prontos não
/// conta e fica de fora do mapa: o que já tem, fica.
fn rank_files(
    files: &[TorrentFile],
    choices: &[Choice],
    episodes: &[(i64, u16, u16)],
) -> BTreeMap<usize, u8> {
    let mut groups: BTreeMap<OrderKey, Vec<(usize, bool)>> = BTreeMap::new();
    for choice in choices.iter().filter(|c| c.priority > 0) {
        let done = files
            .iter()
            .find(|f| f.index == choice.index)
            .is_none_or(|f| f.progress >= 1.0);
        groups
            .entry(order_key(&choice.episodes, episodes))
            .or_default()
            .push((choice.index, done));
    }
    let mut ranks = BTreeMap::new();
    let mut pending = 0;
    for members in groups.values() {
        if members.iter().all(|(_, done)| *done) {
            continue;
        }
        let priority = match pending {
            0 => MAXIMUM,
            1 => HIGH,
            _ => NORMAL,
        };
        pending += 1;
        ranks.extend(members.iter().map(|(index, _)| (*index, priority)));
    }
    ranks
}

/// Serializa a leitura e o toggle do download sequencial: a fila e a volta
/// da importação podem pegar o mesmo torrent juntas, e dois toggles a partir
/// do mesmo `seq_dl = false` ligariam e desligariam o modo.
static SEQUENTIAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Liga o download sequencial do torrent se estiver desligado: o
/// `toggleSequentialDownload` só alterna, então o estado é relido do cliente
/// dentro da trava antes de alternar (o `torrent` recebido pode estar velho).
/// Falha só avisa: a ordem é ajuste, não condição para baixar.
pub(crate) async fn ensure_sequential(client: &QbitClient, torrent: &TorrentInfo) {
    if torrent.seq_dl {
        return;
    }
    let _guard = SEQUENTIAL_LOCK.lock().await;
    let current = match client.torrent(&torrent.hash).await {
        Ok(Some(current)) => current,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(torrent = %torrent.name, "download sequencial: {error}");
            return;
        }
    };
    if current.seq_dl {
        return;
    }
    if let Err(error) = client.toggle_sequential(&torrent.hash).await {
        tracing::warn!(torrent = %torrent.name, "download sequencial: {error}");
    }
}

/// O torrent reaproveitado baixa em ordem de episódio, esteja parado ou não:
/// semeando, o arquivo novo volta a baixar sozinho, sem passar pela fila (que
/// é quem liga o modo nos grabs novos). Com `need == Some(0)` nada falta.
async fn sequential_on_reuse(
    client: &QbitClient,
    existing: Option<&TorrentInfo>,
    need: Option<u64>,
) {
    if let Some(existing) = existing.filter(|_| need != Some(0)) {
        ensure_sequential(client, existing).await;
    }
}

/// A volta da importação religa o modo num grab que ainda baixa: o que saiu
/// da fila antes de o modo existir, ou foi reaproveitado sem passar por aqui,
/// também baixa em ordem. Terminado, não há o que ordenar.
pub(crate) async fn sequential_while_downloading(client: &QbitClient, torrent: &TorrentInfo) {
    if torrent.progress < 1.0 {
        ensure_sequential(client, torrent).await;
    }
}

/// Uma escolha aplicada no cliente.
#[derive(Debug)]
pub(crate) struct Selection {
    pub files: Vec<TorrentFile>,
    pub choices: Vec<Choice>,
    /// Algum arquivo entrou ou saiu do download agora: o progresso do cliente
    /// ainda não conta. Mudar a ordem entre os escolhidos não conta.
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
/// só diz o que faria): zero no que não se quer e, nos escolhidos, a ordem de
/// episódio ([`rank_files`]), refeita a cada chamada e só mandada ao cliente
/// quando muda. `None` quando o cliente ainda não tem a lista (magnet sem
/// metadados).
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
    let episodes = numbers(entry);
    let choices = choose_files(&files, &episodes, &wanted, release, &super::scene(entry));
    let ranks = rank_files(&files, &choices, &episodes);
    let current = |index: usize| files.iter().find(|f| f.index == index).map(|f| f.priority);
    // O destino de cada arquivo: zero fora da escolha; o lugar na ordem; e,
    // se o episódio já está pronto, o que o cliente já tem.
    let target = |choice: &Choice| {
        if choice.priority == 0 {
            return 0;
        }
        ranks
            .get(&choice.index)
            .copied()
            .unwrap_or_else(|| current(choice.index).filter(|p| *p > 0).unwrap_or(NORMAL))
    };
    let moves: Vec<(usize, u8)> = choices
        .iter()
        .map(|c| (c.index, target(c)))
        .filter(|(index, priority)| current(*index) != Some(*priority))
        .collect();
    let to = |priority: u8| -> Vec<usize> {
        moves
            .iter()
            .filter(|(_, p)| *p == priority)
            .map(|(index, _)| *index)
            .collect()
    };
    if write {
        client
            .set_file_priority(&hash, &to(0), 0)
            .await
            .context("tirando arquivos do download")?;
        for priority in [MAXIMUM, HIGH, NORMAL] {
            client
                .set_file_priority(&hash, &to(priority), priority)
                .await
                .context("escolhendo arquivos do download")?;
        }
    }
    let changed = moves
        .iter()
        .any(|(index, priority)| *priority == 0 || current(*index) == Some(0));
    let files = if moves.is_empty() {
        files
    } else {
        files
            .into_iter()
            .map(|mut f| {
                if let Some((_, priority)) = moves.iter().find(|(index, _)| *index == f.index) {
                    f.priority = *priority;
                }
                f
            })
            .collect()
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

/// Se o torrent `hash`, já no cliente, pode ser reaproveitado por um grab da
/// série `series_id`, sem IO. Só o de um grab importado da mesma série: o
/// torrent é dele, e pegá-lo de novo só acrescenta episódios. Grab em
/// andamento, falho (o motivo continua valendo), de filme, de outra série ou
/// nenhum (torrent mandado à mão) é de outro: devolve por que recusa. O hash
/// é a identidade; o título muda com o status do tracker.
///
/// # Errors
///
/// O motivo da recusa.
pub fn reusable<'a>(
    hash: &str,
    series_id: i64,
    series: &'a [SeriesGrab],
    movies: &[acervo_store::Grab],
) -> Result<&'a SeriesGrab, &'static str> {
    if movies.iter().any(|g| g.hash.eq_ignore_ascii_case(hash)) {
        return Err("o torrent já está no cliente, num download de filme");
    }
    let mut same = series.iter().filter(|g| g.hash.eq_ignore_ascii_case(hash));
    let Some(grab) = same.next() else {
        return Err("o torrent já está no cliente, fora do acervo");
    };
    if same.next().is_some() {
        return Err("o torrent já está no cliente, em mais de um download");
    }
    if grab.series_id != series_id {
        return Err("o torrent já está no cliente, num download de outra série");
    }
    match grab.state {
        GrabState::Imported => Ok(grab),
        GrabState::Downloading => Err("o torrent já está no cliente, baixando"),
        GrabState::Failed => Err("o torrent já está no cliente, num download que falhou"),
    }
}

/// Manda um release ao cliente, escolhe os arquivos e grava o grab com os
/// episódios `wanted`. Devolve o id do grab.
///
/// Torrent que já está no cliente só é reaproveitado se é de um grab
/// importado da mesma série ([`reusable`]): o grab volta a baixar com os
/// episódios dele mais os de agora, e os já importados a importação pula.
/// Fora isso é recusado: é de outro grab, de filme ou de fora do acervo, e
/// mexer nas prioridades dele ou apagá-lo depois estragaria o que é dos
/// outros.
///
/// # Errors
///
/// Torrent já no cliente e não reaproveitável, `.torrent` inválido, cliente
/// inalcançável ou falha ao registrar.
#[allow(clippy::too_many_lines)] // A sequência do grab; dividir só espalharia.
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
    // Os episódios do grab: os de agora e, reaproveitado, os de antes.
    let mut episodes = wanted.to_vec();
    let mut hash = hash;
    // Torrent parado de um grab reaproveitado: volta a andar, depois do grab
    // gravado, se falta baixar algum dos escolhidos.
    let mut stopped = false;
    let existing = client.torrent(&hash).await?;
    if let Some(existing) = &existing {
        let (series, movies) = (store.series_grabs().await?, store.grabs().await?);
        let grab = reusable(&hash, series_id, &series, &movies).map_err(anyhow::Error::msg)?;
        // A união: os já importados a importação pula ([`super::import::to_link`]).
        for id in &grab.episode_ids {
            if !episodes.contains(id) {
                episodes.push(*id);
            }
        }
        // O hash do grab, como gravado: é por ele que o registro é achado.
        hash.clone_from(&grab.hash);
        stopped = existing.state.starts_with("stopped") || existing.state.starts_with("paused");
        // O indexador pode ter ligado o modo ratio depois do primeiro grab.
        crate::grab::tag_seed(store, &client, &release.indexer, &hash).await;
        tracing::info!(
            release = release.title,
            "torrent de um grab importado da série: reaproveitado"
        );
    } else {
        let seed = crate::grab::seed_tag(store, &release.indexer).await;
        crate::grab::add_queued(
            config,
            &client,
            torrent,
            &hash,
            magnet,
            false,
            seed.as_deref(),
        )
        .await?;
    }
    // O `.torrent` traz a lista de arquivos: o cliente a mostra assim que
    // termina de carregá-lo. O magnet só depois dos metadados; aí a fila e a
    // importação escolhem. O reaproveitado já tem a lista, venha como vier.
    let mut size = release.size;
    let mut need = None;
    if !magnet || existing.is_some() {
        let mut last = None;
        for _ in 0..10 {
            match apply_selection(&client, &hash, &entry, &episodes, &release.title, true).await {
                Ok(Some(selection)) => {
                    size = selection.size();
                    need = Some(selection.need());
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
            episode_ids: episodes,
            hash: hash.clone(),
            title: release.title.clone(),
            indexer: release.indexer.clone(),
            quality,
            size,
            grabbed_at: now_rfc3339(),
            state: GrabState::Downloading,
            message: existing.is_none().then(|| QUEUED.into()),
            finished_at: None,
        })
        .await
        .context("registrando o grab")?;
    // Sem a tag da fila: com ela, se o grab falhasse, o torrent viraria
    // órfão da fila e sairia do cliente com os arquivos.
    sequential_on_reuse(&client, existing.as_ref(), need).await;
    if stopped
        && need != Some(0)
        && let Err(error) = client.start(&[&hash]).await
    {
        tracing::warn!(
            release = release.title,
            "torrent reaproveitado parado: {error}"
        );
    }
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::{Request, Respond};

    /// `torrents/info` de um qBittorrent falso com estado: devolve o
    /// `seq_dl` atual do torrent "h".
    struct Info(Arc<AtomicBool>);

    impl Respond for Info {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "hash": "h", "name": "n", "state": "downloading", "save_path": "/",
                "seq_dl": self.0.load(Ordering::SeqCst),
            }]))
        }
    }

    /// `toggleSequentialDownload` que alterna de verdade e conta as chamadas.
    struct Toggle(Arc<AtomicBool>, Arc<AtomicUsize>);

    impl Respond for Toggle {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            self.0.fetch_xor(true, Ordering::SeqCst);
            self.1.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
        }
    }

    /// Cliente logado num qBittorrent falso cujo torrent "h" começa com
    /// `seq_dl` desligado. Devolve o servidor, o estado e o contador de toggles.
    async fn sequential_fake() -> (MockServer, QbitClient, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
            .mount(&server)
            .await;
        let on = Arc::new(AtomicBool::new(false));
        let toggles = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(Info(Arc::clone(&on)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/toggleSequentialDownload"))
            .and(body_string_contains("hashes=h"))
            .respond_with(Toggle(Arc::clone(&on), Arc::clone(&toggles)))
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "p", Duration::from_secs(5))
            .await
            .unwrap();
        (server, client, on, toggles)
    }

    #[tokio::test]
    async fn ligar_o_sequencial_em_paralelo_nao_desliga_o_que_o_outro_ligou() {
        // A fila e a importação pegam o mesmo torrent com o mesmo retrato
        // velho (`seq_dl = false`): dois toggles desligariam o modo.
        let (_server, client, on, toggles) = sequential_fake().await;
        let stale: TorrentInfo = serde_json::from_value(serde_json::json!({
            "hash": "h", "name": "n", "state": "downloading", "save_path": "/", "seq_dl": false,
        }))
        .unwrap();
        futures::future::join_all((0..8).map(|_| ensure_sequential(&client, &stale))).await;
        assert!(on.load(Ordering::SeqCst));
        assert_eq!(toggles.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reaproveitado_que_semeia_tambem_baixa_em_sequencia() {
        let (_server, client, _on, toggles) = sequential_fake().await;
        let info = |state: &str, seq_dl: bool| -> TorrentInfo {
            serde_json::from_value(serde_json::json!({
                "hash": "h", "name": "n", "state": state, "save_path": "/", "seq_dl": seq_dl,
            }))
            .unwrap()
        };
        // Semeando, sem o modo: liga (a única chamada esperada).
        sequential_on_reuse(&client, Some(&info("stalledUP", false)), Some(3)).await;
        // Já sequencial, nada a ligar; sem torrent, nem chama.
        sequential_on_reuse(&client, Some(&info("stalledUP", true)), Some(3)).await;
        sequential_on_reuse(&client, None, None).await;
        // Nada falta baixar: não mexe.
        sequential_on_reuse(&client, Some(&info("stalledUP", false)), Some(0)).await;
        assert_eq!(toggles.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn importacao_liga_o_modo_so_no_grab_que_ainda_baixa() {
        let (_server, client, _on, toggles) = sequential_fake().await;
        let info = |progress: f64, seq_dl: bool| -> TorrentInfo {
            serde_json::from_value(serde_json::json!({
                "hash": "h", "name": "n", "state": "downloading", "save_path": "/",
                "progress": progress, "seq_dl": seq_dl,
            }))
            .unwrap()
        };
        // Baixando sem o modo: liga (a única chamada esperada).
        sequential_while_downloading(&client, &info(0.4, false)).await;
        // Já ligado, ou terminado: não mexe.
        sequential_while_downloading(&client, &info(0.4, true)).await;
        sequential_while_downloading(&client, &info(1.0, false)).await;
        assert_eq!(toggles.load(Ordering::SeqCst), 1);
    }

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
    fn chave_de_ordem_e_o_menor_episodio_com_especiais_no_fim() {
        let episodes = [(1, 0, 1), (2, 1, 3), (3, 1, 1), (4, 2, 1), (5, 5, 1)];
        assert_eq!(order_key(&[2, 3], &episodes), (false, 1, 1));
        assert_eq!(order_key(&[5, 4], &episodes), (false, 2, 1));
        assert!(order_key(&[5], &episodes) > order_key(&[4], &episodes));
        // Especial depois de qualquer temporada.
        assert!(order_key(&[1], &episodes) > order_key(&[5], &episodes));
        assert_eq!(order_key(&[1, 5], &episodes), (false, 5, 1));
        // Sem episódio conhecido, no fim de tudo.
        assert!(order_key(&[99], &episodes) > order_key(&[1], &episodes));
    }

    fn choice(index: usize, episodes: &[i64]) -> Choice {
        Choice {
            index,
            priority: NORMAL,
            episodes: episodes.to_vec(),
            subtitle: false,
        }
    }

    #[test]
    fn arquivos_baixam_por_prioridade_decrescente_do_primeiro_que_falta() {
        let episodes = [(1, 1, 1), (2, 1, 2), (3, 1, 3), (4, 1, 4)];
        // Fora de ordem no torrent: o E03 é o arquivo 0.
        let mut files = vec![
            file(0, "E03.mkv", 10),
            file(1, "E01.mkv", 10),
            file(2, "E04.mkv", 10),
            file(3, "E02.mkv", 10),
            file(4, "E01.srt", 1),
        ];
        let mut choices = vec![
            choice(0, &[3]),
            choice(1, &[1]),
            choice(2, &[4]),
            choice(3, &[2]),
            Choice {
                subtitle: true,
                ..choice(4, &[1])
            },
        ];
        let ranks = rank_files(&files, &choices, &episodes);
        assert_eq!(ranks[&1], MAXIMUM);
        assert_eq!(ranks[&4], MAXIMUM, "a legenda anda com o vídeo");
        assert_eq!(ranks[&3], HIGH);
        assert_eq!(ranks[&0], NORMAL);
        assert_eq!(ranks[&2], NORMAL);
        // O E01 terminou: o E02 passa a máxima, o E03 a alta, e o E01 fica.
        for index in [1, 4] {
            files[index].progress = 1.0;
        }
        let ranks = rank_files(&files, &choices, &episodes);
        assert!(!ranks.contains_key(&1) && !ranks.contains_key(&4));
        assert_eq!(ranks[&3], MAXIMUM);
        assert_eq!(ranks[&0], HIGH);
        assert_eq!(ranks[&2], NORMAL);
        // Arquivo fora da escolha (prioridade zero) não entra.
        choices[2].priority = 0;
        assert!(!rank_files(&files, &choices, &episodes).contains_key(&2));
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

    fn series_grab(series_id: i64, hash: &str, state: GrabState) -> SeriesGrab {
        SeriesGrab {
            id: 27,
            series_id,
            episode_ids: vec![261],
            hash: hash.into(),
            title: "Reacher.S04.1080p.WEB-DL-GRP".into(),
            indexer: "tracker".into(),
            quality: acervo_parser::Quality::WebDl1080p,
            size: 1_000,
            grabbed_at: "2026-10-05T16:54:00Z".into(),
            state,
            message: None,
            finished_at: None,
        }
    }

    fn movie_grab(hash: &str) -> acervo_store::Grab {
        acervo_store::Grab {
            id: 3,
            movie_id: 9,
            hash: hash.into(),
            title: "Filme.2024.1080p.WEB-DL-GRP".into(),
            indexer: "tracker".into(),
            quality: acervo_parser::Quality::WebDl1080p,
            size: 1_000,
            grabbed_at: "2026-10-01T00:00:00Z".into(),
            state: GrabState::Imported,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces: None,
        }
    }

    #[test]
    fn reaproveita_o_torrent_de_grab_importado_da_mesma_serie() {
        let grabs = [series_grab(5, "c8cd9036", GrabState::Imported)];
        // O cliente devolve o hash em outra caixa: conta igual.
        let grab = reusable("C8CD9036", 5, &grabs, &[]).unwrap();
        assert_eq!(grab.id, 27);
        assert_eq!(grab.episode_ids, [261]);
    }

    #[test]
    fn recusa_torrent_que_e_de_outro() {
        let imported = [series_grab(5, "aaaa", GrabState::Imported)];
        // Grab em andamento: o store também recusaria.
        let downloading = [series_grab(5, "aaaa", GrabState::Downloading)];
        assert!(reusable("aaaa", 5, &downloading, &[]).is_err());
        // Grab falho: o bloqueio ou o erro continua valendo.
        let failed = [series_grab(5, "aaaa", GrabState::Failed)];
        assert!(reusable("aaaa", 5, &failed, &[]).is_err());
        // De outra série.
        assert!(reusable("aaaa", 6, &imported, &[]).is_err());
        // De filme, mesmo com um grab da série no mesmo hash.
        assert!(reusable("aaaa", 5, &imported, &[movie_grab("aaaa")]).is_err());
        // Sem grab: torrent mandado à mão.
        assert!(reusable("bbbb", 5, &imported, &[]).is_err());
        assert!(reusable("bbbb", 5, &[], &[]).is_err());
        // Dois grabs no mesmo hash: na dúvida, não mexe.
        let twice = [
            series_grab(5, "aaaa", GrabState::Imported),
            series_grab(5, "AAAA", GrabState::Imported),
        ];
        assert!(reusable("aaaa", 5, &twice, &[]).is_err());
    }

    #[test]
    fn torrent_reaproveitado_nao_sai_pelo_grab_de_agora() {
        // O grab ganha `grabbed_at` novo; o torrent entrou no cliente dias
        // antes, no grab original.
        let added = 1_791_028_800; // 2026-10-03T12:00:00Z
        let one = [OnDisk {
            dev: 1,
            ino: 1,
            nlink: 1,
        }];
        assert!(!deletable(added, "2026-10-12T12:00:00Z", false, Some(&one)));
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

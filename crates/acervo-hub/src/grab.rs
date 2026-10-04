//! Grab e importação: o acervo pega o release que a decisão escolheu e, quando
//! o download termina, liga o arquivo na pasta do filme.
//!
//! O que é pego vai ao cliente numa categoria própria, que o gerenciador de
//! filmes não importa. A importação liga (hardlink) o arquivo baixado com o
//! nome que o gerenciador daria, na pasta do filme; num upgrade, troca o
//! antigo.
//!
//! Download que o cliente dá como perdido vai para a lista de bloqueio e o
//! filme é buscado de novo (com a busca automática ligada). Problema na
//! importação (arquivo no caminho, disco diferente) não é culpa do release:
//! o download fica na fila, com o aviso, até dar certo.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use acervo_api::Catalog;
use acervo_clients::{
    AddOptions, NewTorrent, QbitClient, QbitError, client_path, info_hash, magnet_hash,
};
use acervo_store::{Grab, GrabState, MovieFile, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::decide::{Decider, label, now_rfc3339, summarize};
use crate::events::{self, Event, Kind};
use crate::naming::movie_file_stem;

/// Extensões de vídeo que a importação aceita.
pub(crate) const VIDEO: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "ts", "m2ts", "wmv", "mov", "webm",
];

/// O que um grab escolheu.
#[derive(Debug, Serialize)]
pub struct Picked {
    pub titulo: String,
    pub indexador: String,
    pub qualidade: &'static str,
    pub tamanho: u64,
}

/// Resultado de um grab, pego ou só planejado.
#[derive(Debug, Serialize)]
pub struct GrabReport {
    pub filme: String,
    pub releases: usize,
    pub escolhido: Option<Picked>,
    /// Motivo de rejeição e quantos releases ele barrou.
    pub motivos: Vec<(String, usize)>,
    pub aplicado: bool,
}

/// Tag dos torrents que o acervo pôs na fila: só esses ele inicia.
pub(crate) const QUEUE_TAG: &str = "acervo:fila";
pub(crate) const QUEUED: &str = "na fila: aguardando espaço";

/// Uma rodada da fila por vez: duas lendo o mesmo espaço livre iniciariam
/// torrents para o mesmo buraco.
static QUEUE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Torrent do acervo que está baixando de verdade: não parado e incompleto.
fn is_active(torrent: &acervo_clients::TorrentInfo) -> bool {
    !torrent.state.starts_with("stopped")
        && !torrent.state.starts_with("paused")
        && torrent.progress < 1.0
}

/// Quem sai da fila agora: do menor para o maior, pulando quem não cabe, e
/// no máximo até o limite de downloads ativos.
///
/// `candidates` são (hash, bytes que faltam); `active_left`, o que falta de
/// cada torrent ativo e incompleto do acervo. O orçamento é o espaço livre
/// menos a folga menos tudo o que falta dos ativos. O qBittorrent reserva o
/// espaço aos poucos depois de iniciar, então o livre lido logo em seguida
/// ainda sobra; quem já pré-alocou é descontado duas vezes, de propósito:
/// errar para menos nunca enche o disco.
fn pick_starts(
    candidates: &[(String, u64)],
    active_left: &[u64],
    free: u64,
    reserve: u64,
    limit: usize,
) -> Vec<String> {
    let mut budget = free
        .saturating_sub(active_left.iter().sum())
        .saturating_sub(reserve);
    let mut slots = limit.saturating_sub(active_left.len());
    let mut sorted: Vec<&(String, u64)> = candidates.iter().collect();
    sorted.sort_by_key(|(_, need)| *need);
    let mut picked = Vec::new();
    for (hash, need) in sorted {
        if slots == 0 {
            break;
        }
        if *need > budget {
            continue;
        }
        budget -= need;
        slots -= 1;
        picked.push(hash.clone());
    }
    picked
}

/// [`pick_starts`] com os prioritários antes: eles escolhem primeiro, e o
/// resto escolhe com o que sobrou de espaço e de vaga. Dentro de cada grupo,
/// a regra é a mesma: menor primeiro, pulando quem não cabe.
fn pick_starts_prioritized(
    candidates: &[(String, u64)],
    priority: &HashSet<String>,
    active_left: &[u64],
    free: u64,
    reserve: u64,
    limit: usize,
) -> Vec<String> {
    let group = |wanted: bool| -> Vec<(String, u64)> {
        candidates
            .iter()
            .filter(|(hash, _)| priority.contains(hash) == wanted)
            .cloned()
            .collect()
    };
    let (first, rest) = (group(true), group(false));
    let mut picked = pick_starts(&first, active_left, free, reserve, limit);
    // O que saiu agora passa a contar como ativo: desconta espaço e vaga.
    let mut busy = active_left.to_vec();
    busy.extend(
        first
            .iter()
            .filter(|(hash, _)| picked.contains(hash))
            .map(|(_, need)| *need),
    );
    picked.extend(pick_starts(&rest, &busy, free, reserve, limit));
    picked
}

/// Inicia os torrents da fila que cabem no disco, do menor para o maior,
/// pulando quem não cabe, sem passar do limite de downloads simultâneos das
/// regras (disco mecânico não aguenta dezenas de escritas aleatórias). O
/// espaço livre não basta: o qBittorrent reserva aos poucos, então se
/// desconta o que falta de todo torrent ativo do acervo. A folga mínima das
/// regras fica sempre livre: a importação cria a pasta do filme e o servidor
/// de mídia grava miniaturas no mesmo disco.
///
/// # Errors
///
/// Cliente inalcançável ou que recusou um comando.
pub(crate) async fn start_queued(store: &Store, client: &QbitClient) -> Result<Vec<String>> {
    let _guard = QUEUE_LOCK.lock().await;
    if !client.preallocates().await? {
        client.enable_preallocation().await?;
        tracing::info!("pré-alocação ligada no qBittorrent");
    }
    // Magnet parado não tem metadados: o tamanho vem do indexador.
    let mut known: HashMap<String, u64> = store
        .grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| (g.hash, g.size))
        .collect();
    let series: Vec<acervo_store::SeriesGrab> = store
        .series_grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .collect();
    known.extend(series.iter().map(|g| (g.hash.clone(), g.size)));
    let size = |t: &acervo_clients::TorrentInfo| {
        if t.size > 0 {
            t.size
        } else {
            known.get(&t.hash).copied().unwrap_or(0)
        }
    };
    let torrents = client.torrents().await?;
    let priority = store.priority_hashes().await?;
    // Prioritário que o próprio cliente segura na fila dele (limite de
    // downloads ativos do qBittorrent) sobe para o topo dela.
    let held: Vec<&str> = torrents
        .iter()
        .filter(|t| t.state == "queuedDL" && priority.contains(&t.hash))
        .map(|t| t.hash.as_str())
        .collect();
    if let Err(error) = client.top_priority(&held).await {
        tracing::warn!("topo da fila do cliente: {error}");
    }
    // Só os do acervo: grab em andamento, filme ou série. Sem `amount_left`
    // (cliente antigo), cai na conta pelo tamanho.
    let active_left: Vec<u64> = torrents
        .iter()
        .filter(|t| known.contains_key(&t.hash) && is_active(t))
        .map(|t| {
            if t.amount_left > 0 {
                t.amount_left
            } else {
                left(size(t), t.progress)
            }
        })
        .collect();
    let rules = crate::rules::stored(store).await?;
    let limit = rules.max_downloads();
    if active_left.len() >= limit {
        return Ok(Vec::new());
    }
    let reserve = rules.folga_minima_mb << 20;
    let free = client.free_space().await?;
    let mut queue = Vec::new();
    for torrent in torrents
        .iter()
        .filter(|t| t.has_tag(QUEUE_TAG) && size(t) > 0)
    {
        // De série, só sai da fila com os arquivos escolhidos; e o que falta
        // baixar é só o deles.
        let need = match series.iter().find(|g| g.hash == torrent.hash) {
            Some(grab) => match crate::series::grab::prepare(store, client, grab).await {
                Ok(Some(need)) => need,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(torrent = %torrent.name, "escolha de arquivos: {error:#}");
                    continue;
                }
            },
            None => left(size(torrent), torrent.progress),
        };
        queue.push((torrent.hash.clone(), need));
    }
    let mut started = Vec::new();
    for hash in pick_starts_prioritized(&queue, &priority, &active_left, free, reserve, limit) {
        let Some(torrent) = torrents.iter().find(|t| t.hash == hash) else {
            continue;
        };
        client.start(&[&hash]).await?;
        client.remove_tag(&[&hash], QUEUE_TAG).await?;
        if priority.contains(&hash)
            && let Err(error) = client.top_priority(&[&hash]).await
        {
            tracing::warn!(torrent = %torrent.name, "topo da fila do cliente: {error}");
        }
        tracing::info!(torrent = %torrent.name, "iniciado da fila");
        started.push(torrent.name.clone());
    }
    Ok(started)
}

/// Hashes dos torrents que ainda esperam na fila do acervo, sem nunca terem
/// começado. Vazio se o cliente não responde: aí nada é trocado.
pub(crate) async fn waiting(config: &Config) -> HashSet<String> {
    let torrents = match qbit(config).await {
        Ok(client) => client.torrents().await.map_err(anyhow::Error::from),
        Err(error) => Err(error),
    };
    match torrents {
        Ok(torrents) => torrents
            .into_iter()
            .filter(|t| t.has_tag(QUEUE_TAG))
            .map(|t| t.hash)
            .collect(),
        Err(error) => {
            tracing::warn!("lendo a fila do cliente: {error:#}");
            HashSet::new()
        }
    }
}

/// Tira da fila os grabs que deram lugar a um release melhor: apaga o
/// torrent, que nunca baixou nada, e encerra o grab sem bloquear o release.
///
/// # Errors
///
/// Cliente inalcançável ou falha de escrita.
pub(crate) async fn swap_out(
    config: &Config,
    store: &Store,
    olds: &[Grab],
    by: &str,
) -> Result<()> {
    let Some(first) = olds.first() else {
        return Ok(());
    };
    let movie = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == first.movie_id)
        .context("o filme saiu do catálogo")?;
    let hashes: Vec<_> = olds
        .iter()
        .map(|g| acervo_core::DownloadHash::new(g.hash.clone()))
        .collect();
    qbit(config)
        .await?
        .delete(&hashes, true)
        .await
        .context("apagando da fila do qBittorrent")?;
    let reason = format!("trocado na fila por {by}");
    for grab in olds {
        give_up(store, grab, &movie, &reason, false, Kind::Ignored).await?;
    }
    Ok(())
}

/// Bytes que faltam baixar.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub(crate) fn left(size: u64, progress: f64) -> u64 {
    (size as f64 * (1.0 - progress.clamp(0.0, 1.0))) as u64
}

pub(crate) async fn qbit(config: &Config) -> Result<QbitClient> {
    let spec = config.qbittorrent().context(
        "cliente de download não configurado: o grab manda o torrent ao qBittorrent \
         (Configurações → Cliente de download)",
    )?;
    QbitClient::login(
        &spec.url,
        &spec.username,
        &spec.password,
        config.http_timeout(),
    )
    .await
    .context("entrando no qBittorrent")
}

/// Manda um release ao cliente e registra o grab. `replaces` é o arquivo que
/// o filme tem hoje, se tiver: a importação o troca pelo novo.
///
/// # Errors
///
/// `.torrent` inválido, cliente inalcançável ou falha ao registrar.
pub async fn send(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    release: &acervo_indexers::Release,
    quality: acervo_parser::Quality,
    replaces: Option<String>,
) -> Result<()> {
    let (torrent, hash) = fetch_torrent(catalog, release).await?;
    let client = qbit(config).await?;
    add_queued(config, &client, torrent, &hash, false, true).await?;
    store
        .record_grab(&Grab {
            id: 0,
            movie_id,
            hash: hash.clone(),
            title: release.title.clone(),
            indexer: release.indexer.clone(),
            quality,
            size: release.size,
            grabbed_at: now_rfc3339(),
            state: GrabState::Downloading,
            message: Some(QUEUED.into()),
            imported_path: None,
            finished_at: None,
            replaces,
        })
        .await
        .context("registrando o grab")?;
    // Já registrado: a fila sabe o tamanho mesmo de um magnet sem metadados.
    if let Err(error) = start_queued(store, &client).await {
        tracing::warn!("fila de downloads: {error:#}");
    }
    let movie = store.movies().await?.into_iter().find(|m| m.id == movie_id);
    events::record(
        store,
        Event {
            source_title: Some(release.title.clone()),
            quality: Some(quality),
            indexer: Some(release.indexer.clone()),
            download_id: Some(hash),
            poster: movie.as_ref().and_then(|m| m.extras.poster.clone()),
            ..Event::new(
                Kind::Grabbed,
                Some(movie_id),
                movie.map_or_else(String::new, |m| events::label(&m.movie.title, m.movie.year)),
            )
        },
    )
    .await;
    Ok(())
}

/// O `.torrent` (ou o magnet) do release, com o infohash.
///
/// # Errors
///
/// Magnet sem infohash, indexador fora do ar ou `.torrent` inválido.
pub(crate) async fn fetch_torrent(
    catalog: &Catalog,
    release: &acervo_indexers::Release,
) -> Result<(NewTorrent, String)> {
    Ok(if release.download_url.scheme() == "magnet" {
        let link = release.download_url.to_string();
        let hash = magnet_hash(&link).context("link magnet sem infohash")?;
        (NewTorrent::Magnet(link), hash)
    } else {
        let bytes = catalog
            .download(&release.indexer, &release.download_url)
            .await
            .map_err(|error| anyhow::anyhow!("baixando o .torrent: {error}"))?;
        let hash = info_hash(&bytes).context("o indexador não devolveu um .torrent válido")?;
        (NewTorrent::File(bytes), hash)
    })
}

/// Põe o torrent no cliente, na categoria do acervo e na fila: parado ou,
/// com `metadata_first`, andando só até ter a lista de arquivos. Com
/// `adopt`, um torrent que já estava no cliente conta como adicionado.
///
/// # Errors
///
/// Cliente inalcançável ou que recusou o torrent (já estava lá, sem
/// `adopt`).
pub(crate) async fn add_queued(
    config: &Config,
    client: &QbitClient,
    torrent: NewTorrent,
    hash: &str,
    metadata_first: bool,
    adopt: bool,
) -> Result<()> {
    client
        .ensure_category(&config.library.category)
        .await
        .context("criando a categoria no qBittorrent")?;
    let added = client
        .add(
            torrent,
            &AddOptions {
                category: config.library.category.clone(),
                save_path: None,
                stopped: !metadata_first,
                stop_after_metadata: metadata_first,
                tags: vec![QUEUE_TAG.into()],
            },
        )
        .await;
    match added {
        // O mesmo release de um grab que falhou, ou de um registro que não
        // chegou ao banco: o torrent já está no cliente, só falta o grab.
        Err(QbitError::AddRefused) if adopt && client.torrent(hash).await?.is_some() => {}
        added => added.context("mandando o torrent ao qBittorrent")?,
    }
    Ok(())
}

/// Manda um release escolhido na mão (busca interativa) ao cliente.
///
/// # Errors
///
/// Filme fora do catálogo, `.torrent` inválido ou cliente inalcançável.
pub async fn send_chosen(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    release: &acervo_indexers::Release,
) -> Result<()> {
    let replaces = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == movie_id)
        .context("filme fora do catálogo")?
        .movie
        .file
        .map(|f| f.relative_path);
    let quality = acervo_parser::parse_quality(&release.title).quality;
    send(config, store, catalog, movie_id, release, quality, replaces).await
}

/// Busca o filme, decide e, com `apply`, manda o escolhido ao cliente. Filme
/// com arquivo só é pego se a decisão aprovar como upgrade.
///
/// # Errors
///
/// Filme fora do catálogo, busca que falhou em todos os indexadores,
/// `.torrent` inválido, cliente inalcançável.
pub async fn grab(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    apply: bool,
) -> Result<GrabReport> {
    let decider = Decider::load(store, catalog).await?;
    let movie = decider.target(movie_id).context("filme fora do catálogo")?;
    let outcome = decider
        .decide(catalog, movie)
        .await
        .map_err(|error| anyhow::anyhow!("busca falhou: {error}"))?;
    let mut report = GrabReport {
        filme: label(movie),
        releases: outcome.releases.len(),
        escolhido: None,
        motivos: summarize(&outcome.decisions, movie.id),
        aplicado: false,
    };
    let Some((decision, release)) = outcome.pick() else {
        return Ok(report);
    };
    let quality = decision
        .parsed
        .as_ref()
        .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality);
    report.escolhido = Some(Picked {
        titulo: release.title.clone(),
        indexador: release.indexer.clone(),
        qualidade: quality.name(),
        tamanho: release.size,
    });
    if !apply {
        return Ok(report);
    }
    let replaces = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == movie_id)
        .and_then(|m| m.movie.file.map(|f| f.relative_path));
    send(config, store, catalog, movie_id, release, quality, replaces).await?;
    report.aplicado = true;
    Ok(report)
}

/// Um download do acervo na importação.
#[derive(Debug, Serialize)]
pub struct ImportLine {
    pub filme: String,
    pub release: String,
    /// `baixando`, `importaria`, `importado`, `atencao` (importação
    /// travada, tenta de novo) ou `falhou`.
    pub estado: &'static str,
    pub detalhe: Option<String>,
    /// Caminho do arquivo na pasta do filme, como o gerenciador vê.
    pub destino: Option<String>,
}

/// O que a importação de um filme ligou: o destino como o cliente vê, o
/// caminho relativo, o tamanho, o caminho no host e as legendas.
struct Imported {
    shown: String,
    relative: String,
    size: u64,
    host: PathBuf,
    subtitles: Vec<acervo_store::Subtitle>,
}

/// Liga as legendas do torrent ao lado do vídeo do filme, com o nome dele.
/// Falha numa legenda vira aviso: ela não segura a importação.
async fn link_movie_subtitles(
    map: &acervo_fs::PathMap,
    torrent: &acervo_clients::TorrentInfo,
    files: &[acervo_clients::TorrentFile],
    folder: &str,
    video: &str,
    known: &[acervo_store::CatalogSubtitle],
) -> Vec<acervo_store::Subtitle> {
    let found: Vec<&acervo_clients::TorrentFile> = files
        .iter()
        .filter(|f| {
            crate::subtitles::subtitle_extension(&f.name).is_some()
                && !crate::series::grab::is_sample_or_extra(&f.name)
        })
        .collect();
    let (dir, stem) = crate::series::import::split_video(video);
    let originals: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
    let named = crate::subtitles::names(stem, &originals, &HashSet::new());
    let mut linked = Vec::new();
    for (file, named) in found.into_iter().zip(named) {
        let relative = format!("{dir}{}", named.name);
        // O que está no destino hoje, se o catálogo o conhece.
        let origin = known
            .iter()
            .find(|s| s.subtitle.relative_path == relative)
            .map(|s| s.subtitle.origin);
        let done = match (
            map.to_host(&client_path(torrent, file)),
            map.to_host(&PathBuf::from(folder).join(&relative)),
        ) {
            (Ok(source), Ok(target)) => tokio::task::spawn_blocking(move || {
                crate::subtitles::place(&source, &target, origin)
            })
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r),
            (Err(error), _) | (_, Err(error)) => Err(error.into()),
        };
        match done {
            Ok(true) => linked.push(acervo_store::Subtitle {
                relative_path: relative,
                language: named.language.map(str::to_owned),
                forced: named.forced,
                origin: acervo_store::SubtitleOrigin::Import,
            }),
            Ok(false) => tracing::warn!(
                legenda = file.name,
                destino = relative,
                "legenda posta à mão no lugar: a do torrent não entra"
            ),
            Err(error) => tracing::warn!(legenda = file.name, "legenda não importada: {error:#}"),
        }
    }
    linked
}

/// O arquivo principal do torrent: o maior vídeo que não é amostra.
pub(crate) fn main_video(
    files: &[acervo_clients::TorrentFile],
) -> Option<&acervo_clients::TorrentFile> {
    files
        .iter()
        .filter(|file| {
            let path = Path::new(&file.name);
            let video = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()));
            let sample = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_ascii_lowercase().contains("sample"));
            video && !sample
        })
        .max_by_key(|file| file.size)
}

/// Liga `source` em `target`, criando a pasta. Ligação que já existe para o
/// mesmo arquivo não é erro; arquivo diferente no destino é.
pub(crate) fn link(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando `{}`", parent.display()))?;
    }
    let from =
        std::fs::metadata(source).with_context(|| format!("lendo `{}`", source.display()))?;
    match std::fs::metadata(target) {
        Ok(existing) if existing.dev() == from.dev() && existing.ino() == from.ino() => {
            return Ok(());
        }
        Ok(_) => bail!("já existe outro arquivo em `{}`", target.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("lendo `{}`", target.display()));
        }
    }
    std::fs::hard_link(source, target).map_err(|error| {
        if error.raw_os_error() == Some(18) {
            // EXDEV: download e biblioteca em discos diferentes. Copiar
            // dobraria o espaço sem avisar; melhor parar.
            anyhow::anyhow!("download e biblioteca estão em discos diferentes: hardlink impossível")
        } else {
            anyhow::Error::new(error).context(format!("ligando `{}`", target.display()))
        }
    })
}

/// Põe o arquivo novo no lugar. Num upgrade com o mesmo nome, liga num nome
/// temporário e renomeia por cima do antigo (troca atômica); com nome
/// diferente, liga o novo e só então apaga o antigo.
pub(crate) fn install(source: &Path, target: &Path, old: Option<&Path>) -> Result<()> {
    match old {
        Some(old) if old == target => {
            let temporary = target.with_extension("acervo-novo");
            match std::fs::remove_file(&temporary) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error).context("limpando o temporário");
                }
                _ => {}
            }
            link(source, &temporary)?;
            std::fs::rename(&temporary, target)
                .with_context(|| format!("trocando `{}`", target.display()))
        }
        old => {
            link(source, target)?;
            if let Some(old) = old {
                match std::fs::remove_file(old) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(error)
                            .with_context(|| format!("apagando o antigo `{}`", old.display()));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

/// O registro do arquivo importado, com o que o nome do release diz e as
/// faixas que o `ffprobe` leu.
fn imported_file(
    grab: &Grab,
    relative_path: String,
    size: u64,
    probe: Option<crate::mediainfo::Probe>,
) -> MovieFile {
    let parsed = acervo_parser::parse_movie_title(&grab.title);
    let from_name: Vec<acervo_parser::Language> = acervo_parser::parse_languages(&grab.title)
        .into_iter()
        .filter(|l| *l != acervo_parser::Language::Unknown)
        .collect();
    let languages = probe
        .map(|p| p.audio_languages)
        .filter(|l| !l.is_empty())
        .unwrap_or(from_name);
    MovieFile {
        relative_path,
        size,
        quality: acervo_parser::parse_quality(&grab.title),
        languages: languages.into_iter().map(|l| l.name().to_owned()).collect(),
        release_group: acervo_parser::parse_release_group(&grab.title),
        edition: parsed.and_then(|p| p.edition),
        scene_name: Some(grab.title.clone()),
        date_added: Some(now_rfc3339()),
    }
}

/// Quanto tempo um download ativo pode ficar sem seed e sem transferir antes
/// de ser trocado.
const STALL: time::Duration = time::Duration::minutes(30);

/// A mensagem da falha por falta de seeds. Marca a expiração do bloqueio:
/// não mude sem migrar as linhas que já a guardam.
pub(crate) const NO_SEEDS: &str = "sem seeds há 30 min";

/// Por quanto tempo o bloqueio por falta de seeds vale: o release pode só
/// ter estado sem seeds naquela hora.
const NO_SEEDS_BLOCK: time::Duration = time::Duration::days(7);

/// Download ativo que não anda: sem seed algum, sem transferir nada e já há
/// 30 min ativo. Parado, na fila, verificando ou com erro não conta.
pub(crate) fn stalled(torrent: &acervo_clients::TorrentInfo, now: time::OffsetDateTime) -> bool {
    let limit = STALL.whole_seconds();
    let quiet = torrent.last_activity == 0 || now.unix_timestamp() - torrent.last_activity >= limit;
    matches!(
        torrent.state.as_str(),
        "downloading" | "stalledDL" | "metaDL" | "forcedDL"
    ) && torrent.progress < 1.0
        && torrent.time_active >= limit
        && torrent.num_complete <= 0
        && torrent.num_seeds == 0
        && quiet
}

/// O bloqueio ainda vale? O de "sem seeds" e o de torrent desregistrado
/// expiram em 7 dias; qualquer outro é para sempre. A linha fica, para a
/// tela de bloqueados.
pub(crate) fn still_blocks(blocked: &acervo_store::Blocked, now: time::OffsetDateTime) -> bool {
    if !matches!(blocked.message.as_deref(), Some(NO_SEEDS | UNREGISTERED)) {
        return true;
    }
    // Data ilegível: na dúvida, bloqueia.
    match time::OffsetDateTime::parse(&blocked.at, &time::format_description::well_known::Rfc3339) {
        Ok(at) => now - at < NO_SEEDS_BLOCK,
        Err(_) => true,
    }
}

/// A mensagem da falha de torrent que o tracker não reconhece mais (o
/// avulso apagado quando sai o pacote). Marca a expiração do bloqueio, como
/// a de [`NO_SEEDS`]: não mude sem migrar as linhas que já a guardam.
pub(crate) const UNREGISTERED: &str = "o tracker não reconhece mais o torrent";

/// As frases com que os trackers dizem que o torrent deixou de existir
/// neles, comparadas sem caixa. Frase inteira: "not registered" sozinho
/// aparece em mensagem de usuário, não de torrent.
const UNREGISTERED_PHRASES: &[&str] = &[
    "unregistered torrent",
    "torrent not registered",
    "torrent not found",
    "torrent não registrado",
    "torrent nao registrado",
];

/// Todo tracker de verdade (não DHT, `PeX` nem LSD) não funciona (estado 4)
/// e algum deles diz, com uma das frases, que não conhece o torrent. Sem
/// tracker de verdade, não.
pub(crate) fn unregistered(trackers: &[acervo_clients::Tracker]) -> bool {
    let real: Vec<&acervo_clients::Tracker> = trackers.iter().filter(|t| t.is_real()).collect();
    !real.is_empty()
        && real.iter().all(|t| t.status == 4)
        && real.iter().any(|t| {
            let msg = t.msg.to_lowercase();
            UNREGISTERED_PHRASES.iter().any(|p| msg.contains(p))
        })
}

/// Os hashes que os trackers deram por desregistrados na volta anterior.
/// Só a segunda volta seguida conta: uma resposta ruim do tracker não
/// derruba o download.
#[derive(Debug, Default)]
pub(crate) struct Suspects(HashSet<String>);

impl Suspects {
    /// Registra o que se viu agora. `true` se é a segunda volta seguida; sem
    /// a condição, o hash é esquecido.
    pub fn observe(&mut self, hash: &str, now: bool) -> bool {
        if now {
            !self.0.insert(hash.to_owned())
        } else {
            self.0.remove(hash);
            false
        }
    }
}

static SUSPECTS: std::sync::LazyLock<std::sync::Mutex<Suspects>> =
    std::sync::LazyLock::new(std::sync::Mutex::default);

/// O torrent perdeu o registro no tracker: ativo, incompleto, sem seed
/// conectado, ativo há 30 min, e [`unregistered`] em duas voltas seguidas.
/// Só consulta os trackers de quem passa pelo resto. Sem `record` (a volta
/// que só mostra), não conta a volta. Erro na consulta é "não sei": `false`,
/// sem mudar o que se lembra.
pub(crate) async fn gone_from_tracker(
    client: &QbitClient,
    torrent: &acervo_clients::TorrentInfo,
    record: bool,
) -> bool {
    let candidate = is_active(torrent)
        && torrent.num_seeds == 0
        && torrent.time_active >= STALL.whole_seconds();
    let now = if candidate {
        match client.trackers(&torrent.hash).await {
            Ok(trackers) => unregistered(&trackers),
            Err(error) => {
                tracing::warn!(torrent = %torrent.name, "trackers ilegíveis: {error}");
                return false;
            }
        }
    } else {
        false
    };
    let mut suspects = SUSPECTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if record {
        suspects.observe(&torrent.hash, now)
    } else {
        now && suspects.0.contains(&torrent.hash)
    }
}

/// Nenhum arquivo do torrent tem outro link (biblioteca de alguém, outro
/// seed): só assim ele pode sair do cliente. Leitura duvidosa (`None`) é
/// "não".
pub(crate) fn no_other_link(files: Option<&[crate::series::remove::OnDisk]>) -> bool {
    files.is_some_and(|files| files.iter().all(|f| f.nlink <= 1))
}

/// [`no_other_link`], lendo os arquivos do torrent no cliente e no disco.
async fn only_link(
    config: &Config,
    client: &QbitClient,
    torrent: &acervo_clients::TorrentInfo,
) -> bool {
    let Ok(files) = client
        .files(&acervo_core::DownloadHash::new(torrent.hash.clone()))
        .await
    else {
        return false;
    };
    let map = config.path_map();
    let Some(paths) = files
        .iter()
        .map(|file| map.to_host(&client_path(torrent, file)).ok())
        .collect::<Option<Vec<PathBuf>>>()
    else {
        return false;
    };
    let found = tokio::task::spawn_blocking(move || crate::series::remove::stat_all(&paths))
        .await
        .ok()
        .flatten();
    no_other_link(found.as_deref())
}

/// Por quanto tempo um torrent recém-mandado pode ainda não aparecer no
/// cliente: o qBittorrent 5 responde ao `add` antes de listá-lo, e a nova
/// busca roda dentro da mesma volta da importação que o procura.
const FRESH_GRAB: time::Duration = time::Duration::minutes(10);

/// O grab saiu há pouco: torrent ausente é atraso do cliente, não perda.
pub(crate) fn fresh_grab(grabbed_at: &str, now: time::OffsetDateTime) -> bool {
    time::OffsetDateTime::parse(grabbed_at, &time::format_description::well_known::Rfc3339)
        .is_ok_and(|at| now - at < FRESH_GRAB)
}

/// Torrent ausente do cliente: perda, ou só atraso se o grab é recente.
fn missing(grabbed_at: &str) -> Failure {
    if fresh_grab(grabbed_at, time::OffsetDateTime::now_utc()) {
        Failure::Import("o torrent ainda não apareceu no cliente".into())
    } else {
        Failure::Download("o torrent sumiu do cliente".into())
    }
}

enum Failure {
    /// O release é o problema: o cliente perdeu ou deu erro. Bloqueia e
    /// busca de novo.
    Download(String),
    /// A importação é o problema: tenta de novo na próxima rodada.
    Import(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Import(message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Import(message.to_owned())
    }
}

/// Desiste de um download: marca como falho, bloqueia o release se pedido
/// e registra o evento.
///
/// # Errors
///
/// Falha de escrita.
pub async fn give_up(
    store: &Store,
    grab: &Grab,
    movie: &acervo_store::CatalogMovie,
    reason: &str,
    blocklist: bool,
    kind: Kind,
) -> Result<()> {
    let at = now_rfc3339();
    store
        .update_grab(grab.id, GrabState::Failed, Some(reason), None, &at)
        .await?;
    if blocklist {
        store
            .block(&acervo_store::Blocked {
                id: 0,
                movie_id: Some(grab.movie_id),
                series_id: None,
                source_title: grab.title.clone(),
                indexer: Some(grab.indexer.clone()),
                quality: Some(grab.quality),
                size: Some(grab.size),
                hash: Some(grab.hash.clone()),
                at: at.clone(),
                message: Some(reason.to_owned()),
            })
            .await?;
    }
    events::record(
        store,
        Event {
            source_title: Some(grab.title.clone()),
            quality: Some(grab.quality),
            indexer: Some(grab.indexer.clone()),
            download_id: Some(grab.hash.clone()),
            message: Some(reason.to_owned()),
            poster: movie.extras.poster.clone(),
            ..Event::new(
                kind,
                Some(movie.id),
                events::label(&movie.movie.title, movie.movie.year),
            )
        },
    )
    .await;
    Ok(())
}

/// Busca o filme de novo depois de uma falha.
async fn search_again(config: &Config, store: &Store, catalog: Option<&Catalog>, movie_id: i64) {
    let Some(catalog) = catalog else {
        return;
    };
    match grab(config, store, catalog, movie_id, true).await {
        Ok(report) => tracing::info!(
            filme = report.filme,
            pegou = report.escolhido.as_ref().map(|p| p.titulo.as_str()),
            "nova busca depois de falha"
        ),
        Err(error) => tracing::info!(filme = movie_id, "nova busca depois de falha: {error:#}"),
    }
}

/// Tira um download da fila: apaga do cliente (com os arquivos) se pedido,
/// bloqueia o release se pedido e busca outro se pedido.
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
        .grabs()
        .await?
        .into_iter()
        .find(|g| g.id == grab_id)
        .context("download desconhecido")?;
    let movie = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == grab.movie_id)
        .context("o filme do download saiu do catálogo")?;
    if remove_from_client {
        qbit(config)
            .await?
            .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
            .await
            .context("apagando do qBittorrent")?;
    }
    let (reason, kind) = if blocklist {
        ("marcado como falho na tela", Kind::Failed)
    } else {
        ("tirado da fila na tela", Kind::Ignored)
    };
    give_up(store, &grab, &movie, reason, blocklist, kind).await?;
    if search {
        match self::grab(config, store, catalog, movie.id, true).await {
            Ok(_) => {}
            Err(error) => tracing::info!(filme = movie.id, "nova busca: {error:#}"),
        }
    }
    Ok(())
}

/// Importa os downloads do acervo que terminaram. Sem `apply`, só diz o que
/// faria.
///
/// # Errors
///
/// Catálogo ilegível ou cliente inalcançável. Falha de um download fica na
/// linha dele; os outros seguem.
#[allow(clippy::too_many_lines)]
pub async fn import_downloads(
    config: &Config,
    store: &Store,
    catalog: Option<&Catalog>,
    apply: bool,
) -> Result<Vec<ImportLine>> {
    let pending: Vec<Grab> = store
        .grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let client = qbit(config).await?;
    if apply && let Err(error) = start_queued(store, &client).await {
        tracing::warn!("fila de downloads: {error:#}");
    }
    let free_space = client.free_space().await.ok();
    let movies = store.movies().await?;
    let map = config.path_map();
    let mut lines = Vec::new();
    for grab in pending {
        let Some(entry) = movies.iter().find(|m| m.id == grab.movie_id) else {
            continue;
        };
        let movie = &entry.movie;
        let filme = match movie.year {
            Some(year) => format!("{} ({year})", movie.title),
            None => movie.title.clone(),
        };
        let mut line = ImportLine {
            filme,
            release: grab.title.clone(),
            estado: "baixando",
            detalhe: None,
            destino: None,
        };
        let outcome: Result<Option<Imported>, Failure> = async {
            let torrent = client
                .torrent(&grab.hash)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| missing(&grab.grabbed_at))?;
            // Disco cheio não é culpa do release: bloquear e buscar outro só
            // empilha torrents que dão o mesmo erro. Volta para a fila, com
            // o que já baixou.
            if torrent.state == "error" {
                // Sem saber o espaço não dá para culpar o release: tenta de
                // novo na próxima rodada.
                let free = free_space.ok_or("erro no cliente e espaço livre ilegível")?;
                if free < left(torrent.size, torrent.progress) {
                    if apply {
                        client
                            .stop(&[&grab.hash])
                            .await
                            .map_err(|e| e.to_string())?;
                        client
                            .add_tag(&[&grab.hash], QUEUE_TAG)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    line.detalhe = Some(QUEUED.into());
                    return Ok(None);
                }
            }
            if matches!(torrent.state.as_str(), "error" | "missingFiles") {
                return Err(Failure::Download(format!(
                    "o cliente marcou o torrent com `{}`",
                    torrent.state
                )));
            }
            if gone_from_tracker(&client, &torrent, apply).await {
                tracing::info!(
                    filme = line.filme,
                    release = grab.title,
                    "o tracker não reconhece mais o torrent"
                );
                // Como o sem seeds: bloqueio e nova busca vêm do
                // `Failure::Download`; o torrent morto sai do cliente, se
                // nenhum arquivo dele tem outro link.
                if apply {
                    if only_link(config, &client, &torrent).await {
                        client
                            .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
                            .await
                            .map_err(|e| e.to_string())?;
                    } else {
                        tracing::warn!(
                            release = grab.title,
                            "torrent desregistrado com arquivo ligado em outro lugar: fica no cliente"
                        );
                    }
                }
                return Err(Failure::Download(UNREGISTERED.into()));
            }
            if stalled(&torrent, time::OffsetDateTime::now_utc()) {
                tracing::info!(
                    filme = line.filme,
                    release = grab.title,
                    "trocando release sem seeds"
                );
                // O bloqueio e a nova busca vêm do `Failure::Download`; o
                // torrent travado, porém, ocuparia vaga e reserva.
                if apply {
                    client
                        .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                return Err(Failure::Download(NO_SEEDS.into()));
            }
            if torrent.progress < 1.0 {
                line.detalhe = Some(if torrent.has_tag(QUEUE_TAG) {
                    QUEUED.into()
                } else if matches!(torrent.state.as_str(), "pausedDL" | "stoppedDL") {
                    format!("parado, {:.0}%", torrent.progress * 100.0)
                } else {
                    format!("{:.0}%", torrent.progress * 100.0)
                });
                return Ok(None);
            }
            // Arquivo que não é o que o grab ia trocar: alguém importou por
            // outro caminho no meio.
            let current = movie.file.as_ref().map(|f| f.relative_path.as_str());
            if current.is_some() && current != grab.replaces.as_deref() {
                return Err("o filme ganhou arquivo por outro caminho".into());
            }
            let hash = acervo_core::DownloadHash::new(grab.hash.clone());
            let files = client.files(&hash).await.map_err(|e| e.to_string())?;
            let video = main_video(&files).ok_or("nenhum vídeo no torrent")?;
            let extension = Path::new(&video.name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mkv")
                .to_ascii_lowercase();
            let relative = format!(
                "{}.{extension}",
                movie_file_stem(movie, entry.extras.metadata_title.as_deref())
            );
            let destination = PathBuf::from(&movie.path).join(&relative);
            let old_host = match current {
                Some(old) => Some(
                    map.to_host(&PathBuf::from(&movie.path).join(old))
                        .map_err(|e| e.to_string())?,
                ),
                None => None,
            };
            let size = video.size;
            let source_host = map
                .to_host(&client_path(&torrent, video))
                .map_err(|e| e.to_string())?;
            let destination_host = map.to_host(&destination).map_err(|e| e.to_string())?;
            let shown = destination.display().to_string();
            if !apply {
                return Ok(Some(Imported {
                    shown,
                    relative,
                    size,
                    host: destination_host,
                    subtitles: Vec::new(),
                }));
            }
            let installed = destination_host.clone();
            tokio::task::spawn_blocking(move || {
                install(&source_host, &installed, old_host.as_deref())
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
            let subtitles =
                link_movie_subtitles(
                    &map,
                    &torrent,
                    &files,
                    &movie.path,
                    &relative,
                    &entry.subtitles,
                )
                .await;
            Ok(Some(Imported {
                shown,
                relative,
                size,
                host: destination_host,
                subtitles,
            }))
        }
        .await;

        let at = now_rfc3339();
        match outcome {
            Ok(None) => {
                if apply {
                    store
                        .update_grab(
                            grab.id,
                            GrabState::Downloading,
                            line.detalhe.as_deref(),
                            None,
                            &at,
                        )
                        .await?;
                }
            }
            Ok(Some(imported)) if !apply => {
                line.estado = "importaria";
                line.destino = Some(imported.shown);
            }
            Ok(Some(Imported {
                shown: destination,
                relative,
                size,
                host,
                subtitles,
            })) => {
                let probe = crate::mediainfo::probe(&host).await;
                store
                    .set_movie_file(entry.id, Some(&imported_file(&grab, relative, size, probe)))
                    .await?;
                // As legendas antigas que não viraram as de agora: a do
                // torrent sai do disco; a posta à mão fica, e continua no
                // catálogo (como do disco). As novas entram no catálogo.
                let mut kept = Vec::new();
                for old in &entry.subtitles {
                    let path = &old.subtitle.relative_path;
                    if subtitles.iter().any(|s| &s.relative_path == path) {
                        continue;
                    }
                    let Ok(host) = map.to_host(&PathBuf::from(&movie.path).join(path)) else {
                        continue;
                    };
                    let origin = Some(old.subtitle.origin);
                    let gone = tokio::task::spawn_blocking(move || {
                        crate::subtitles::remove_if_from_torrent(&host, origin)
                    })
                    .await;
                    match gone {
                        Ok(Ok(true)) => {}
                        Ok(Ok(false)) => kept.push(acervo_store::Subtitle {
                            origin: acervo_store::SubtitleOrigin::Disk,
                            ..old.subtitle.clone()
                        }),
                        Ok(Err(error)) => {
                            tracing::warn!(legenda = path, "antiga não apagada: {error}");
                        }
                        Err(error) => tracing::warn!(legenda = path, "antiga não apagada: {error}"),
                    }
                }
                for subtitle in subtitles.iter().chain(&kept) {
                    if let Err(error) = store.add_movie_subtitle(entry.id, subtitle).await {
                        tracing::warn!(
                            legenda = subtitle.relative_path,
                            "legenda fora do catálogo: {error}"
                        );
                    }
                }
                store
                    .update_grab(grab.id, GrabState::Imported, None, Some(&destination), &at)
                    .await?;
                line.estado = "importado";
                line.destino = Some(destination.clone());
                events::record(
                    store,
                    Event {
                        source_title: Some(grab.title.clone()),
                        quality: Some(grab.quality),
                        indexer: Some(grab.indexer.clone()),
                        download_id: Some(grab.hash.clone()),
                        message: Some(destination),
                        poster: entry.extras.poster.clone(),
                        ..Event::new(
                            if grab.replaces.is_some() {
                                Kind::Upgraded
                            } else {
                                Kind::Imported
                            },
                            Some(entry.id),
                            events::label(&movie.title, movie.year),
                        )
                    },
                )
                .await;
            }
            Err(Failure::Download(error)) => {
                line.estado = "falhou";
                line.detalhe = Some(error.clone());
                if apply {
                    give_up(store, &grab, entry, &error, true, Kind::Failed).await?;
                    search_again(config, store, catalog, entry.id).await;
                }
            }
            Err(Failure::Import(error)) => {
                line.estado = "atencao";
                line.detalhe = Some(error.clone());
                if apply {
                    let message = format!("importação: {error}");
                    store
                        .update_grab(grab.id, GrabState::Downloading, Some(&message), None, &at)
                        .await?;
                }
            }
        }
        lines.push(line);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_ausente_de_grab_recente_e_atraso_nao_perda() {
        let now = time::OffsetDateTime::parse(
            "2026-10-03T15:30:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(fresh_grab("2026-10-03T15:27:53.096959272Z", now));
        assert!(!fresh_grab("2026-10-03T15:19:00Z", now));
        assert!(!fresh_grab("data ilegível", now));
    }

    fn file(name: &str, size: u64) -> acervo_clients::TorrentFile {
        serde_json::from_value(serde_json::json!({ "name": name, "size": size })).unwrap()
    }

    fn queue(needs: &[u64]) -> Vec<(String, u64)> {
        needs.iter().map(|n| (format!("h{n}"), *n)).collect()
    }

    #[test]
    fn limite_segura_a_fila_mesmo_com_espaco_sobrando() {
        // Dois ativos de limite 3: só uma vaga, apesar do disco enorme.
        let picked = pick_starts(&queue(&[10, 20, 30]), &[1, 1], 1_000_000, 0, 3);
        assert_eq!(picked, ["h10"]);
        // Limite já atingido: nada inicia.
        assert!(pick_starts(&queue(&[10]), &[1, 1, 1], 1_000_000, 0, 3).is_empty());
    }

    #[test]
    fn conta_desconta_o_que_falta_dos_ativos() {
        // 100 livres, 30 faltam nos ativos, folga 10: sobram 60.
        let picked = pick_starts(&queue(&[50, 70]), &[10, 20], 100, 10, 10);
        assert_eq!(picked, ["h50"]);
        // Sem ativos, o orçamento é 90: o de 50 cabe e o de 70 não, depois dele.
        assert_eq!(pick_starts(&queue(&[50, 70]), &[], 100, 10, 10), ["h50"]);
        assert!(pick_starts(&queue(&[50]), &[60], 100, 10, 10).is_empty());
    }

    #[test]
    fn prioritarios_escolhem_antes_e_o_resto_com_o_que_sobra() {
        let priority = HashSet::from(["h40".to_owned(), "h95".to_owned()]);
        // Orçamento 90: o prioritário de 40 primeiro (o de 95 não cabe), e
        // dos outros, menor primeiro, com os 50 que sobram: 5 e 30.
        let picked =
            pick_starts_prioritized(&queue(&[80, 5, 40, 30, 95]), &priority, &[], 100, 10, 10);
        assert_eq!(picked, ["h40", "h5", "h30"]);
        // A vaga também: com uma só, é do prioritário, mesmo sendo maior.
        let picked = pick_starts_prioritized(&queue(&[5, 40]), &priority, &[], 100, 10, 1);
        assert_eq!(picked, ["h40"]);
        // Sem prioritário, é a regra de sempre.
        let picked =
            pick_starts_prioritized(&queue(&[80, 5, 40, 30]), &HashSet::new(), &[], 100, 10, 10);
        assert_eq!(
            picked,
            pick_starts(&queue(&[80, 5, 40, 30]), &[], 100, 10, 10)
        );
    }

    #[test]
    fn menor_primeiro_pulando_quem_nao_cabe() {
        let picked = pick_starts(&queue(&[80, 5, 40, 30]), &[], 100, 10, 10);
        // Orçamento 90: 5, 30, 40 (soma 75); o de 80 não cabe mais.
        assert_eq!(picked, ["h5", "h30", "h40"]);
        let picked = pick_starts(&queue(&[95, 20]), &[], 100, 10, 10);
        assert_eq!(picked, ["h20"]);
    }

    fn at(text: &str) -> time::OffsetDateTime {
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap()
    }

    /// Torrent travado no limite: ativo há 30 min, sem seed, sem transferir.
    fn travado(now: time::OffsetDateTime) -> acervo_clients::TorrentInfo {
        serde_json::from_value(serde_json::json!({
            "hash": "h", "name": "n", "state": "stalledDL", "save_path": "/",
            "progress": 0.4, "time_active": 1800, "num_complete": 0, "num_seeds": 0,
            "last_activity": now.unix_timestamp() - 1800,
        }))
        .unwrap()
    }

    #[test]
    fn sem_seeds_so_com_todas_as_condicoes() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(stalled(&travado(now), now));
        // Nunca transferiu: zero conta como nunca.
        let mut t = travado(now);
        t.last_activity = 0;
        assert!(stalled(&t, now));
        // Estados ativos de download.
        for state in ["downloading", "stalledDL", "metaDL", "forcedDL"] {
            let mut t = travado(now);
            state.clone_into(&mut t.state);
            assert!(stalled(&t, now), "{state}");
        }
        // Estados que não são download ativo.
        for state in [
            "queuedDL",
            "stoppedDL",
            "pausedDL",
            "checkingDL",
            "checkingResumeData",
            "error",
            "missingFiles",
            "stalledUP",
            "uploading",
        ] {
            let mut t = travado(now);
            state.clone_into(&mut t.state);
            assert!(!stalled(&t, now), "{state}");
        }
        let mut t = travado(now);
        t.progress = 1.0;
        assert!(!stalled(&t, now));
        // Ativo há 29 min59.
        let mut t = travado(now);
        t.time_active = 1799;
        assert!(!stalled(&t, now));
        // Seed no enxame ou conectado.
        let mut t = travado(now);
        t.num_complete = 1;
        assert!(!stalled(&t, now));
        let mut t = travado(now);
        t.num_seeds = 1;
        assert!(!stalled(&t, now));
        // Transferiu há 29 min59.
        let mut t = travado(now);
        t.last_activity = now.unix_timestamp() - 1799;
        assert!(!stalled(&t, now));
    }

    #[test]
    fn bloqueio_sem_seeds_expira_em_sete_dias() {
        let blocked = |message: Option<&str>| acervo_store::Blocked {
            id: 1,
            movie_id: Some(1),
            series_id: None,
            source_title: "Filme.2020.1080p".into(),
            indexer: None,
            quality: None,
            size: None,
            hash: None,
            at: "2026-10-01T00:00:00Z".into(),
            message: message.map(str::to_owned),
        };
        let sem_seeds = blocked(Some(NO_SEEDS));
        assert!(still_blocks(&sem_seeds, at("2026-10-07T23:59:59Z")));
        assert!(!still_blocks(&sem_seeds, at("2026-10-08T00:00:00Z")));
        // Outra mensagem, ou nenhuma, nunca expira.
        let outra = blocked(Some("o torrent sumiu do cliente"));
        assert!(still_blocks(&outra, at("2027-10-01T00:00:00Z")));
        assert!(still_blocks(&blocked(None), at("2027-10-01T00:00:00Z")));
    }

    fn tracker(url: &str, status: i64, msg: &str) -> acervo_clients::Tracker {
        serde_json::from_value(serde_json::json!({ "url": url, "status": status, "msg": msg }))
            .unwrap()
    }

    #[test]
    fn desregistrado_so_com_todos_os_trackers_reais_parados_e_a_frase() {
        let dht = tracker("** [DHT] **", 0, "");
        let real = |status, msg| tracker("https://tracker.example.invalid/a", status, msg);
        for msg in [
            "Unregistered torrent",
            "Torrent not registered with this tracker",
            "torrent not found",
            "Torrent não registrado",
            "TORRENT NAO REGISTRADO",
        ] {
            assert!(unregistered(&[dht.clone(), real(4, msg)]), "{msg}");
        }
        // Só "not registered", sem a frase inteira, não conta.
        assert!(!unregistered(&[real(4, "user not registered")]));
        assert!(!unregistered(&[real(4, "unregistered")]));
        // Outro tracker de verdade ainda funcionando: não.
        assert!(!unregistered(&[
            real(4, "Unregistered torrent"),
            tracker("https://outro.example.invalid/a", 2, ""),
        ]));
        // Todos parados, um com a frase e outro com outro motivo: sim.
        assert!(unregistered(&[
            real(4, "Unregistered torrent"),
            tracker("https://outro.example.invalid/a", 4, "timed out"),
        ]));
        // Funcionando, ou falhando por outro motivo, não conta.
        assert!(!unregistered(&[real(2, "unregistered torrent")]));
        assert!(!unregistered(&[real(4, "timed out")]));
        // DHT com a mensagem não é tracker; sem tracker de verdade, não.
        assert!(!unregistered(&[tracker(
            "** [DHT] **",
            4,
            "unregistered torrent"
        )]));
        assert!(!unregistered(&[]));
    }

    #[test]
    fn desregistrado_so_na_segunda_volta_seguida() {
        let mut suspects = Suspects::default();
        assert!(!suspects.observe("h", true));
        assert!(suspects.observe("h", true));
        // A condição some: esquece, e recomeça do zero.
        assert!(!suspects.observe("h", false));
        assert!(!suspects.observe("h", true));
        assert!(suspects.observe("h", true));
        // Cada hash por si.
        assert!(!suspects.observe("outro", true));
    }

    #[test]
    fn bloqueio_de_desregistrado_expira_como_o_sem_seeds() {
        let blocked = acervo_store::Blocked {
            id: 1,
            movie_id: Some(1),
            series_id: None,
            source_title: "Filme.2020.1080p".into(),
            indexer: None,
            quality: None,
            size: None,
            hash: None,
            at: "2026-10-01T00:00:00Z".into(),
            message: Some(UNREGISTERED.into()),
        };
        assert!(still_blocks(&blocked, at("2026-10-07T23:59:59Z")));
        assert!(!still_blocks(&blocked, at("2026-10-08T00:00:00Z")));
    }

    #[test]
    fn torrent_so_sai_sem_outro_link() {
        use crate::series::remove::OnDisk;
        let one = OnDisk {
            dev: 1,
            ino: 1,
            nlink: 1,
        };
        let linked = OnDisk {
            dev: 1,
            ino: 2,
            nlink: 2,
        };
        assert!(no_other_link(Some(&[one])));
        assert!(no_other_link(Some(&[])));
        assert!(!no_other_link(Some(&[one, linked])));
        assert!(!no_other_link(None));
    }

    #[test]
    fn so_conta_como_ativo_o_que_baixa_de_verdade() {
        let torrent = |state: &str, progress: f64| -> acervo_clients::TorrentInfo {
            serde_json::from_value(serde_json::json!({
                "hash": "h", "name": "n", "state": state, "save_path": "/",
                "progress": progress,
            }))
            .unwrap()
        };
        assert!(is_active(&torrent("downloading", 0.5)));
        assert!(is_active(&torrent("metaDL", 0.0)));
        assert!(!is_active(&torrent("stoppedDL", 0.0)));
        assert!(!is_active(&torrent("pausedDL", 0.3)));
        assert!(!is_active(&torrent("stalledUP", 1.0)));
    }

    #[test]
    fn video_principal_e_o_maior_que_nao_e_amostra() {
        let files = [
            file("Filme.2020/Sample/filme-sample.mkv", 50),
            file("Filme.2020/Filme.2020.1080p.mkv", 4_000),
            file("Filme.2020/Filme.2020.nfo", 1),
            file("Filme.2020/Extras/bastidores.mp4", 900),
        ];
        assert_eq!(
            main_video(&files).unwrap().name,
            "Filme.2020/Filme.2020.1080p.mkv"
        );
        assert!(main_video(&[file("x.nfo", 1), file("x-sample.mkv", 9)]).is_none());
    }

    #[test]
    fn upgrade_troca_o_arquivo_antigo() {
        let dir = std::env::temp_dir().join(format!("acervo-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Filme")).unwrap();
        let old = dir.join("Filme").join("Filme.mkv");
        std::fs::write(&old, b"antigo").unwrap();
        let new = dir.join("novo.mkv");
        std::fs::write(&new, b"novo").unwrap();
        // Mesmo nome: troca no lugar.
        install(&new, &old, Some(&old)).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"novo");
        assert!(!dir.join("Filme").join("Filme.acervo-novo").exists());
        // Nome diferente: liga o novo e apaga o antigo.
        let other = dir.join("outro.mp4");
        std::fs::write(&other, b"outro").unwrap();
        let target = dir.join("Filme").join("Filme.mp4");
        install(&other, &target, Some(&old)).unwrap();
        assert!(!old.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"outro");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ligar_cria_a_pasta_e_repetir_nao_e_erro() {
        let dir = std::env::temp_dir().join(format!("acervo-grab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("baixado.mkv");
        std::fs::write(&source, b"video").unwrap();
        let target = dir.join("Filme (2020)").join("Filme (2020).mkv");

        link(&source, &target).unwrap();
        link(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"video");

        let other = dir.join("outro.mkv");
        std::fs::write(&other, b"outro").unwrap();
        assert!(link(&other, &target).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

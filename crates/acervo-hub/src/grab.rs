//! Grab e importação: o acervo pega o release que a decisão escolheu e, quando
//! o download termina, liga o arquivo na pasta do filme.
//!
//! O que é pego vai ao cliente numa categoria própria, parado e com a tag da
//! fila; a fila o inicia quando cabe no disco. A importação liga (hardlink) o
//! arquivo baixado na pasta do filme, com o nome do padrão; se o filme já
//! tinha arquivo (troca escolhida na mão), troca o antigo.
//!
//! Download que o cliente dá como perdido vai para a lista de bloqueio e o
//! filme é buscado de novo. Problema na importação (arquivo no caminho, disco
//! diferente) não é culpa do release: o download fica na fila, com o aviso,
//! até dar certo. O que o cliente diz uma vez só (arquivos sumidos, erro, sem
//! seeds) precisa persistir 30 min observados antes de virar falha.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use acervo_api::Catalog;
use acervo_clients::{
    AddOptions, NewTorrent, QbitClient, QbitError, client_path, info_hash, magnet_hash,
};
use acervo_indexers::ResolvedDownload;
use acervo_store::{Grab, GrabState, MovieFile, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use time::OffsetDateTime;

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

/// A mensagem de quem pede um segundo download do mesmo filme.
pub(crate) const ALREADY_DOWNLOADING: &str = "já há um download em andamento";

/// Uma rodada da fila por vez: duas lendo o mesmo espaço livre iniciariam
/// torrents para o mesmo buraco.
static QUEUE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// O que se acompanha de um torrent, volta a volta, em memória.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Condition {
    /// O cliente não acha os arquivos (`missingFiles`).
    MissingFiles,
    /// O cliente marcou erro, e há espaço livre: não é disco cheio.
    ClientError,
    /// Baixando, sem seed nenhum.
    NoSeeds,
    /// A importação travou ("atenção").
    Attention,
}

/// Desde quando cada condição vale, sem interrupção, para cada torrent — como
/// este processo a viu. Só em memória: reiniciar recomeça a contagem, e
/// condição observada uma vez só nunca vira falha.
#[derive(Debug, Default)]
pub(crate) struct Watch {
    since: HashMap<(String, Condition), OffsetDateTime>,
    /// Os torrents cujo aviso de importação travada já saiu.
    noticed: HashSet<String>,
}

impl Watch {
    /// Registra o que se viu agora e devolve há quanto tempo a condição vale
    /// sem interrupção: zero se não vale (e aí a contagem recomeça).
    pub fn observe(
        &mut self,
        hash: &str,
        condition: Condition,
        holds: bool,
        now: OffsetDateTime,
    ) -> time::Duration {
        let key = (hash.to_owned(), condition);
        if !holds {
            self.since.remove(&key);
            return time::Duration::ZERO;
        }
        let since = *self.since.entry(key).or_insert(now);
        (now - since).max(time::Duration::ZERO)
    }

    /// `true` uma vez só por torrent, até [`Watch::forget`].
    pub fn first_notice(&mut self, hash: &str) -> bool {
        self.noticed.insert(hash.to_owned())
    }

    /// O grab terminou: tudo o que se acompanhava do torrent sai.
    pub fn forget(&mut self, hash: &str) {
        self.since.retain(|(h, _), _| h != hash);
        self.noticed.remove(hash);
    }
}

static WATCH: LazyLock<Mutex<Watch>> = LazyLock::new(Mutex::default);

/// O acompanhamento dos torrents, dividido entre filmes e séries.
pub(crate) fn watch() -> MutexGuard<'static, Watch> {
    WATCH.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Quanto uma condição passageira do cliente precisa persistir, observada,
/// antes de virar falha.
const PERSIST: time::Duration = time::Duration::minutes(30);

/// Quanto a importação pode ficar travada antes do aviso.
const ATTENTION_NOTICE: time::Duration = time::Duration::hours(6);

/// A mensagem do torrent cujos arquivos o cliente não acha.
pub(crate) const MISSING_FILES: &str = "o cliente não acha os arquivos do torrent";

/// A mensagem da falha por erro do cliente com espaço sobrando. Marca a
/// expiração do bloqueio, como a de [`NO_SEEDS`]: não mude sem migrar as
/// linhas que já a guardam.
pub(crate) const CLIENT_ERROR: &str = "o cliente marcou o torrent com `error`";

/// O que o estado do torrent no cliente diz da volta: `missingFiles` e
/// `error` (o de disco cheio já foi tratado antes) tentam de novo até
/// persistirem [`PERSIST`] observados. Aí, arquivo sumido é falha sem
/// bloqueio — o release não tem culpa —, e erro é falha com o bloqueio que
/// expira, como o de sem seeds.
pub(crate) fn client_trouble(
    watch: &mut Watch,
    torrent: &acervo_clients::TorrentInfo,
    now: OffsetDateTime,
) -> Option<Failure> {
    let state = torrent.state.as_str();
    let missing = watch.observe(
        &torrent.hash,
        Condition::MissingFiles,
        state == "missingFiles",
        now,
    );
    let error = watch.observe(&torrent.hash, Condition::ClientError, state == "error", now);
    match state {
        "missingFiles" if missing >= PERSIST => Some(Failure::Lost(MISSING_FILES.into())),
        "missingFiles" => Some(Failure::Import(format!(
            "{MISSING_FILES}; tenta de novo por 30 min"
        ))),
        "error" if error >= PERSIST => Some(Failure::Download(CLIENT_ERROR.into())),
        "error" => Some(Failure::Import(format!(
            "{CLIENT_ERROR}; tenta de novo por 30 min"
        ))),
        _ => None,
    }
}

/// O aviso da importação travada: `true` uma vez só, quando o torrent passa
/// de [`ATTENTION_NOTICE`] seguidas em atenção. Fora de atenção, a contagem
/// recomeça.
pub(crate) fn attention_due(
    watch: &mut Watch,
    hash: &str,
    stuck: bool,
    now: OffsetDateTime,
) -> bool {
    watch.observe(hash, Condition::Attention, stuck, now) >= ATTENTION_NOTICE
        && watch.first_notice(hash)
}

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
/// Torrent na fila do acervo sem grab em andamento que o reclame não é de
/// ninguém: é apagado com os arquivos, nunca iniciado ([`orphans`]).
///
/// # Errors
///
/// Cliente inalcançável ou que recusou um comando.
pub(crate) async fn start_queued(
    config: &Config,
    store: &Store,
    client: &QbitClient,
) -> Result<Vec<String>> {
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
    let mut torrents = client.torrents().await?;
    drop_orphans(config, store, client, &torrents, &known).await?;
    // O que foi apagado, ou ficou por ter link, não entra na conta.
    torrents.retain(|t| !t.has_tag(QUEUE_TAG) || known.contains_key(&t.hash));
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

/// Por quanto tempo um torrent recém-posto na fila pode ainda não ter grab:
/// o grab é gravado logo depois de o torrent entrar no cliente.
const ORPHAN_GRACE: i64 = 10 * 60;

/// Os torrents da fila do acervo (tag e categoria dele) que nenhum grab em
/// andamento reclama, entrados no cliente há mais de [`ORPHAN_GRACE`]. Sem
/// data de entrada, na dúvida, nenhum.
fn orphans(
    torrents: &[acervo_clients::TorrentInfo],
    known: &HashMap<String, u64>,
    category: &str,
    now: i64,
) -> Vec<String> {
    torrents
        .iter()
        .filter(|t| t.has_tag(QUEUE_TAG) && t.category == category)
        .filter(|t| !known.contains_key(&t.hash))
        .filter(|t| t.added_on > 0 && now - t.added_on >= ORPHAN_GRACE)
        .map(|t| t.hash.clone())
        .collect()
}

/// Hashes dos grabs em andamento, de filme e de série, lidos agora.
async fn claimed(store: &Store) -> Result<Vec<String>> {
    let mut hashes: Vec<String> = store
        .grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| g.hash)
        .collect();
    hashes.extend(
        store
            .series_grabs()
            .await?
            .into_iter()
            .filter(|g| g.state == GrabState::Downloading)
            .map(|g| g.hash),
    );
    Ok(hashes)
}

/// Apaga com os arquivos os torrents da fila sem grab ([`orphans`]), menos o
/// que tem arquivo ligado em outro lugar: esse fica parado, com aviso. Antes
/// de cada remoção os grabs são lidos de novo: um grab gravado depois da
/// leitura de `known` (o torrent entrou e foi adotado) reclama o torrent, que
/// então fica. Remoção recusada pelo cliente vira aviso: não trava a fila.
///
/// # Errors
///
/// Catálogo ilegível.
async fn drop_orphans(
    config: &Config,
    store: &Store,
    client: &QbitClient,
    torrents: &[acervo_clients::TorrentInfo],
    known: &HashMap<String, u64>,
) -> Result<()> {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    for hash in orphans(torrents, known, &config.library.category, now) {
        let Some(torrent) = torrents.iter().find(|t| t.hash == hash) else {
            continue;
        };
        if !only_link(config, client, torrent).await {
            tracing::warn!(
                torrent = %torrent.name,
                "torrent na fila sem grab, com arquivo ligado em outro lugar: fica parado"
            );
            continue;
        }
        if claimed(store)
            .await?
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&hash))
        {
            continue;
        }
        if let Err(error) = client
            .delete(&[acervo_core::DownloadHash::new(hash)], true)
            .await
        {
            tracing::warn!(torrent = %torrent.name, "torrent sem grab não apagado: {error}");
            continue;
        }
        tracing::info!(torrent = %torrent.name, "torrent na fila sem grab apagado");
    }
    Ok(())
}

/// O torrent espera na fila do acervo e nunca começou.
fn is_waiting(torrent: &acervo_clients::TorrentInfo) -> bool {
    torrent.has_tag(QUEUE_TAG) && torrent.progress <= 0.0
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
            .filter(is_waiting)
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
/// Cada torrent é lido de novo antes de apagar, sob [`QUEUE_LOCK`]: o que
/// iniciou desde a leitura de [`waiting`] fica, com o grab. O que tem o
/// mesmo hash do release novo também fica no cliente: já é o torrent dele.
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
    let client = qbit(config).await?;
    let grabs = store.grabs().await?;
    let reason = format!("trocado na fila por {by}");
    let _guard = QUEUE_LOCK.lock().await;
    for grab in olds {
        // Outro grab em andamento com o mesmo hash é o release novo.
        let reused = grabs.iter().any(|g| {
            g.state == GrabState::Downloading
                && olds.iter().all(|old| old.id != g.id)
                && g.hash.eq_ignore_ascii_case(&grab.hash)
        });
        if !reused {
            match client.torrent(&grab.hash).await? {
                Some(torrent) if !is_waiting(&torrent) => {
                    tracing::info!(release = grab.title, "torrent já iniciou: não é trocado");
                    continue;
                }
                Some(_) => {
                    client
                        .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
                        .await
                        .context("apagando da fila do qBittorrent")?;
                }
                None => {}
            }
        }
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

/// Recusa um download novo para um filme que já tem um em andamento: dois
/// grabs do mesmo filme brigariam pela importação. `swapping` são os grabs
/// que o novo vem trocar (a troca na fila), e esses não contam.
///
/// # Errors
///
/// [`ALREADY_DOWNLOADING`], ou banco inalcançável.
async fn ensure_none_downloading(store: &Store, movie_id: i64, swapping: &[i64]) -> Result<()> {
    let busy = store.grabs().await?.into_iter().any(|g| {
        g.movie_id == movie_id && g.state == GrabState::Downloading && !swapping.contains(&g.id)
    });
    if busy {
        bail!(ALREADY_DOWNLOADING);
    }
    Ok(())
}

/// Manda um release ao cliente e registra o grab. `replaces` é o arquivo que
/// o filme tem hoje, se tiver: a importação o troca pelo novo. Com outro
/// grab do filme em andamento, recusa — menos os de `swapping`, que quem
/// chama vai tirar da fila ([`swap_out`]).
///
/// # Errors
///
/// Download em andamento, `.torrent` inválido, cliente inalcançável ou
/// falha ao registrar.
#[allow(clippy::too_many_arguments)] // O release e o que vem com ele.
pub async fn send(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    release: &acervo_indexers::Release,
    quality: acervo_parser::Quality,
    replaces: Option<String>,
    swapping: &[i64],
) -> Result<()> {
    ensure_none_downloading(store, movie_id, swapping).await?;
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
    crate::stats::grabbed(store, &release.indexer).await;
    // Já registrado: a fila sabe o tamanho mesmo de um magnet sem metadados.
    if let Err(error) = start_queued(config, store, &client).await {
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
    resolve(catalog, &release.indexer, &release.download_url).await
}

/// O link de download de um indexador resolvido pela sessão dele: o
/// `.torrent`, ou o magnet quando a página só oferece isso. Magnet direto
/// nem passa pelo indexador.
///
/// # Errors
///
/// Magnet sem infohash, indexador fora do ar ou `.torrent` inválido.
pub(crate) async fn resolve(
    catalog: &Catalog,
    indexer: &str,
    link: &url::Url,
) -> Result<(NewTorrent, String)> {
    let resolved = if link.scheme() == "magnet" {
        ResolvedDownload::Magnet(link.clone())
    } else {
        catalog
            .resolve_download(indexer, link)
            .await
            .map_err(|error| anyhow::anyhow!("baixando o .torrent: {error}"))?
    };
    Ok(match resolved {
        ResolvedDownload::Magnet(magnet) => {
            let magnet = magnet.to_string();
            let hash = magnet_hash(&magnet).context("link magnet sem infohash")?;
            (NewTorrent::Magnet(magnet), hash)
        }
        ResolvedDownload::Torrent(bytes) => {
            let hash = info_hash(&bytes).context("o indexador não devolveu um .torrent válido")?;
            (NewTorrent::File(bytes), hash)
        }
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
        // O mandado à mão pela busca é de quem o mandou: nenhum grab o adota.
        Err(QbitError::AddRefused) if adopt => match client.torrent(hash).await? {
            Some(torrent) if torrent.category == config.library.manual_category => {
                bail!(
                    "o torrent já está no cliente, mandado à mão (categoria {})",
                    torrent.category
                );
            }
            Some(_) => {}
            None => Err(QbitError::AddRefused).context("mandando o torrent ao qBittorrent")?,
        },
        added => added.context("mandando o torrent ao qBittorrent")?,
    }
    Ok(())
}

/// Manda um release escolhido na mão (busca interativa) ao cliente.
///
/// # Errors
///
/// Filme fora do catálogo, download já em andamento, `.torrent` inválido ou
/// cliente inalcançável.
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
    send(
        config,
        store,
        catalog,
        movie_id,
        release,
        quality,
        replaces,
        &[],
    )
    .await
}

/// Busca o filme, decide e, com `apply`, manda o escolhido ao cliente; sem
/// ele, é a prévia da tela. Com download já em andamento, nem busca.
///
/// # Errors
///
/// Filme fora do catálogo, download já em andamento, busca que falhou em
/// todos os indexadores, `.torrent` inválido, cliente inalcançável.
pub async fn grab(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    apply: bool,
) -> Result<GrabReport> {
    let decider = Decider::load(store, catalog).await?;
    let movie = decider.target(movie_id).context("filme fora do catálogo")?;
    if apply {
        ensure_none_downloading(store, movie_id, &[]).await?;
    }
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
    send(
        config,
        store,
        catalog,
        movie_id,
        release,
        quality,
        replaces,
        &[],
    )
    .await?;
    report.aplicado = true;
    Ok(report)
}

/// Um download do acervo na importação.
#[derive(Debug, Serialize)]
pub struct ImportLine {
    pub filme: String,
    pub release: String,
    /// `baixando`, `importado`, `atencao` (importação travada, tenta de
    /// novo), `falhou` ou `descartado` (o filme ganhou arquivo por outro
    /// caminho).
    pub estado: &'static str,
    pub detalhe: Option<String>,
    /// Caminho do arquivo na pasta do filme, como o cliente vê.
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

/// Por quanto tempo um bloqueio automático vale: o release pode só ter
/// estado sem seeds naquela hora.
pub(crate) const NO_SEEDS_BLOCK: time::Duration = time::Duration::days(7);

/// Baixando de verdade e sem seed nenhum, conectado ou no enxame. Parado,
/// na fila (do acervo ou do cliente), verificando ou com erro não conta.
fn seedless(torrent: &acervo_clients::TorrentInfo) -> bool {
    matches!(
        torrent.state.as_str(),
        "downloading" | "stalledDL" | "metaDL" | "forcedDL"
    ) && torrent.progress < 1.0
        && torrent.num_complete <= 0
        && torrent.num_seeds == 0
}

/// Download ativo que não anda, pelo que o cliente diz agora: sem seed algum,
/// sem transferir nada e já há 30 min ativo.
pub(crate) fn stalled(torrent: &acervo_clients::TorrentInfo, now: OffsetDateTime) -> bool {
    let limit = STALL.whole_seconds();
    let quiet = torrent.last_activity == 0 || now.unix_timestamp() - torrent.last_activity >= limit;
    seedless(torrent) && torrent.time_active >= limit && quiet
}

/// Sem seeds de verdade: [`stalled`] agora e sem seed há 30 min observados
/// por este processo, sem interrupção. O `time_active` do cliente soma o
/// tempo ativo de antes de uma parada; sem a observação, torrent retomado
/// depois de parado ou de esperar na fila cairia como sem seeds na primeira
/// volta.
pub(crate) fn no_seeds(
    watch: &mut Watch,
    torrent: &acervo_clients::TorrentInfo,
    now: OffsetDateTime,
) -> bool {
    let seen = watch.observe(&torrent.hash, Condition::NoSeeds, seedless(torrent), now);
    stalled(torrent, now) && seen >= STALL
}

/// As mensagens dos bloqueios automáticos que expiram: o release pode só
/// ter estado ruim naquela hora.
pub(crate) const EXPIRING: [&str; 3] = [NO_SEEDS, UNREGISTERED, CLIENT_ERROR];

/// O bloqueio ainda vale? Os de [`EXPIRING`] expiram em 7 dias; qualquer
/// outro é para sempre. A linha fica, para a tela de bloqueados, até a poda
/// da limpeza.
pub(crate) fn still_blocks(blocked: &acervo_store::Blocked, now: OffsetDateTime) -> bool {
    if !blocked
        .message
        .as_deref()
        .is_some_and(|message| EXPIRING.contains(&message))
    {
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

static SUSPECTS: LazyLock<Mutex<Suspects>> = LazyLock::new(Mutex::default);

/// O torrent perdeu o registro no tracker: ativo, incompleto, sem seed
/// conectado, ativo há 30 min, e [`unregistered`] em duas voltas seguidas.
/// Só consulta os trackers de quem passa pelo resto. Erro na consulta é "não
/// sei": `false`, sem mudar o que se lembra.
pub(crate) async fn gone_from_tracker(
    client: &QbitClient,
    torrent: &acervo_clients::TorrentInfo,
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
    SUSPECTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .observe(&torrent.hash, now)
}

/// Nenhum arquivo do torrent tem outro link (biblioteca de alguém, outro
/// seed): só assim ele pode sair do cliente. Leitura duvidosa (`None`) é
/// "não".
pub(crate) fn no_other_link(files: Option<&[crate::series::remove::OnDisk]>) -> bool {
    files.is_some_and(|files| files.iter().all(|f| f.nlink <= 1))
}

/// [`no_other_link`], lendo os arquivos do torrent no cliente e no disco.
pub(crate) async fn only_link(
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
pub(crate) fn missing(grabbed_at: &str) -> Failure {
    if fresh_grab(grabbed_at, OffsetDateTime::now_utc()) {
        Failure::Import("o torrent ainda não apareceu no cliente".into())
    } else {
        Failure::Download("o torrent sumiu do cliente".into())
    }
}

/// Por que um download não importou nesta volta.
pub(crate) enum Failure {
    /// O release é o problema: o cliente perdeu ou deu erro. Bloqueia e
    /// busca de novo.
    Download(String),
    /// O download se perdeu sem culpa do release (os arquivos sumiram do
    /// disco): desiste e busca de novo, sem bloquear.
    Lost(String),
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
    watch().forget(&grab.hash);
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
/// bloqueia o release se pedido e busca outro se pedido. Sem apagar, o
/// torrent perde a tag da fila: fica no cliente, parado, e a fila não o
/// inicia nem o apaga.
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
    let client = qbit(config).await?;
    if remove_from_client {
        client
            .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
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
    give_up(store, &grab, &movie, reason, blocklist, kind).await?;
    if search {
        match self::grab(config, store, catalog, movie.id, true).await {
            Ok(_) => {}
            Err(error) => tracing::info!(filme = movie.id, "nova busca: {error:#}"),
        }
    }
    Ok(())
}

/// Apaga do cliente o torrent de um grab de filme, com os arquivos, se
/// nenhum deles tem outro link. Com link, fica, com aviso.
async fn delete_unlinked(
    config: &Config,
    client: &QbitClient,
    torrent: &acervo_clients::TorrentInfo,
    why: &str,
) -> Result<(), String> {
    if only_link(config, client, torrent).await {
        client
            .delete(
                &[acervo_core::DownloadHash::new(torrent.hash.clone())],
                true,
            )
            .await
            .map_err(|e| e.to_string())
    } else {
        tracing::warn!(
            torrent = %torrent.name,
            "{why}, com arquivo ligado em outro lugar: fica no cliente"
        );
        Ok(())
    }
}

/// Apaga do cliente o torrent de um grab de filme que sai ainda na fila, sem
/// nada baixado: sem grab em andamento, ele ficaria no cliente sem dono. Só
/// se for dele; erro vira aviso.
pub(crate) async fn drop_queued(config: &Config, store: &Store, client: &QbitClient, grab: &Grab) {
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
    let owner = crate::series::grab::Owner::Movie(grab.id);
    if !crate::series::grab::owns_torrent(config, store, client, owner, &grab.grabbed_at, &torrent)
        .await
    {
        tracing::warn!(
            release = grab.title,
            "torrent na fila sem grab, mas não é só deste: fica no cliente"
        );
        return;
    }
    if let Err(error) = client
        .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
        .await
    {
        tracing::warn!(release = grab.title, "torrent da fila não apagado: {error}");
    }
}

/// O que uma volta da importação fez com um grab de filme.
enum Step {
    /// Ainda não terminou: o porquê, para a tela.
    Waiting(String),
    /// O arquivo foi ligado na pasta do filme.
    Imported(Imported),
    /// O filme ganhou arquivo por outro caminho: o grab não tem mais o que
    /// fazer.
    Superseded,
}

/// O andamento como a fila mostra.
pub(crate) fn progress(torrent: &acervo_clients::TorrentInfo) -> String {
    if torrent.has_tag(QUEUE_TAG) {
        QUEUED.into()
    } else if matches!(torrent.state.as_str(), "pausedDL" | "stoppedDL") {
        format!("parado, {:.0}%", torrent.progress * 100.0)
    } else {
        format!("{:.0}%", torrent.progress * 100.0)
    }
}

/// O que a importação de filmes lê uma vez por volta.
struct MovieImport<'a> {
    config: &'a Config,
    client: QbitClient,
    free_space: Option<u64>,
    map: acervo_fs::PathMap,
}

impl MovieImport<'_> {
    /// Uma volta de um grab: o estado do torrent decide se espera, se
    /// desiste ou se liga o arquivo.
    #[allow(clippy::too_many_lines)] // A sequência da importação; dividir só espalharia.
    async fn step(&self, entry: &acervo_store::CatalogMovie, grab: &Grab) -> Result<Step, Failure> {
        let movie = &entry.movie;
        let client = &self.client;
        let torrent = client
            .torrent(&grab.hash)
            .await
            .map_err(|e| e.to_string())?;
        // Arquivo que não é o que o grab ia trocar: alguém importou por
        // outro caminho no meio. O torrent, que nada ligou, sai do cliente.
        let current = movie.file.as_ref().map(|f| f.relative_path.as_str());
        if current.is_some() && current != grab.replaces.as_deref() {
            if let Some(torrent) = &torrent {
                delete_unlinked(self.config, client, torrent, "grab que perdeu o motivo").await?;
            }
            return Ok(Step::Superseded);
        }
        let torrent = torrent.ok_or_else(|| missing(&grab.grabbed_at))?;
        let now = OffsetDateTime::now_utc();
        // Disco cheio não é culpa do release: bloquear e buscar outro só
        // empilha torrents que dão o mesmo erro. Volta para a fila, com o que
        // já baixou.
        if torrent.state == "error" {
            // Sem saber o espaço não dá para culpar o release: tenta de novo
            // na próxima rodada.
            let free = self
                .free_space
                .ok_or("erro no cliente e espaço livre ilegível")?;
            if free < left(torrent.size, torrent.progress) {
                watch().observe(&torrent.hash, Condition::ClientError, false, now);
                // A fila não é falta de seed: a contagem do sem seeds recomeça.
                watch().observe(&torrent.hash, Condition::NoSeeds, false, now);
                client
                    .stop(&[&grab.hash])
                    .await
                    .map_err(|e| e.to_string())?;
                client
                    .add_tag(&[&grab.hash], QUEUE_TAG)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(Step::Waiting(QUEUED.into()));
            }
        }
        let trouble = client_trouble(&mut watch(), &torrent, now);
        if let Some(failure) = trouble {
            // Arquivo sumido: o torrent perdido sai do cliente (se nenhum
            // arquivo tem outro link), senão a nova busca devolveria o mesmo
            // hash, ainda em `missingFiles`, e o ciclo nunca acabaria. Sem
            // bloqueio: a culpa não é do release.
            if matches!(failure, Failure::Lost(_)) {
                delete_unlinked(self.config, client, &torrent, "arquivos sumidos").await?;
            }
            return Err(failure);
        }
        if gone_from_tracker(client, &torrent).await {
            tracing::info!(
                filme = movie.title,
                release = grab.title,
                "o tracker não reconhece mais o torrent"
            );
            // Como o sem seeds: bloqueio e nova busca vêm do
            // `Failure::Download`; o torrent morto sai do cliente, se nenhum
            // arquivo dele tem outro link.
            delete_unlinked(self.config, client, &torrent, "torrent desregistrado").await?;
            return Err(Failure::Download(UNREGISTERED.into()));
        }
        let stuck = no_seeds(&mut watch(), &torrent, now);
        if stuck {
            tracing::info!(
                filme = movie.title,
                release = grab.title,
                "trocando release sem seeds"
            );
            // O bloqueio e a nova busca vêm do `Failure::Download`; o torrent
            // travado, porém, ocuparia vaga e reserva.
            delete_unlinked(self.config, client, &torrent, "torrent sem seeds").await?;
            return Err(Failure::Download(NO_SEEDS.into()));
        }
        if torrent.progress < 1.0 {
            return Ok(Step::Waiting(progress(&torrent)));
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
        let map = &self.map;
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
        let installed = destination_host.clone();
        tokio::task::spawn_blocking(move || install(&source_host, &installed, old_host.as_deref()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
        let subtitles = link_movie_subtitles(
            map,
            &torrent,
            &files,
            &movie.path,
            &relative,
            &entry.subtitles,
        )
        .await;
        Ok(Step::Imported(Imported {
            shown: destination.display().to_string(),
            relative,
            size,
            host: destination_host,
            subtitles,
        }))
    }
}

/// Grava o arquivo que a volta ligou: o catálogo, as legendas (as antigas do
/// torrent saem do disco) e o evento.
async fn record_import(
    store: &Store,
    map: &acervo_fs::PathMap,
    entry: &acervo_store::CatalogMovie,
    grab: &Grab,
    imported: Imported,
) -> Result<String> {
    let Imported {
        shown: destination,
        relative,
        size,
        host,
        subtitles,
    } = imported;
    let movie = &entry.movie;
    let probe = crate::mediainfo::probe(&host).await;
    store
        .set_movie_file(entry.id, Some(&imported_file(grab, relative, size, probe)))
        .await?;
    // As legendas antigas que não viraram as de agora: a do torrent sai do
    // disco; a posta à mão fica, e continua no catálogo (como do disco). As
    // novas entram no catálogo.
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
        .update_grab(
            grab.id,
            GrabState::Imported,
            None,
            Some(&destination),
            &now_rfc3339(),
        )
        .await?;
    watch().forget(&grab.hash);
    events::record(
        store,
        Event {
            source_title: Some(grab.title.clone()),
            quality: Some(grab.quality),
            indexer: Some(grab.indexer.clone()),
            download_id: Some(grab.hash.clone()),
            message: Some(destination.clone()),
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
    Ok(destination)
}

/// A importação travada há horas avisa, uma vez por grab.
pub(crate) async fn notify_attention(
    store: &Store,
    hash: &str,
    stuck: bool,
    label: &str,
    release: &str,
    message: &str,
    poster: Option<&str>,
) {
    let due = attention_due(&mut watch(), hash, stuck, OffsetDateTime::now_utc());
    if due {
        events::notify_stuck(store, label, release, message, poster).await;
    }
}

/// Importa os downloads do acervo que terminaram.
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
    let round = MovieImport {
        config,
        free_space: client.free_space().await.ok(),
        client,
        map: config.path_map(),
    };
    let movies = store.movies().await?;
    let mut lines = Vec::new();
    for grab in pending {
        let Some(entry) = movies.iter().find(|m| m.id == grab.movie_id) else {
            continue;
        };
        let filme = events::label(&entry.movie.title, entry.movie.year);
        let mut line = ImportLine {
            filme: filme.clone(),
            release: grab.title.clone(),
            estado: "baixando",
            detalhe: None,
            destino: None,
        };
        let step = round.step(entry, &grab).await;
        let stuck = matches!(step, Err(Failure::Import(_)));
        match step {
            Ok(Step::Waiting(detail)) => {
                store
                    .update_grab(
                        grab.id,
                        GrabState::Downloading,
                        Some(&detail),
                        None,
                        &now_rfc3339(),
                    )
                    .await?;
                line.detalhe = Some(detail);
            }
            Ok(Step::Imported(imported)) => {
                let destination = record_import(store, &round.map, entry, &grab, imported).await?;
                line.estado = "importado";
                line.destino = Some(destination);
            }
            Ok(Step::Superseded) => {
                let reason = "o filme ganhou arquivo por outro caminho";
                line.estado = "descartado";
                line.detalhe = Some(reason.into());
                give_up(store, &grab, entry, reason, false, Kind::Ignored).await?;
            }
            Err(Failure::Download(error)) => {
                line.estado = "falhou";
                line.detalhe = Some(error.clone());
                give_up(store, &grab, entry, &error, true, Kind::Failed).await?;
                search_again(config, store, catalog, entry.id).await;
            }
            // O arquivo sumido do disco não é culpa do release: sem bloqueio.
            Err(Failure::Lost(error)) => {
                line.estado = "falhou";
                line.detalhe = Some(error.clone());
                give_up(store, &grab, entry, &error, false, Kind::Failed).await?;
                search_again(config, store, catalog, entry.id).await;
            }
            Err(Failure::Import(error)) => {
                line.estado = "atencao";
                line.detalhe = Some(error.clone());
                let message = format!("importação: {error}");
                store
                    .update_grab(
                        grab.id,
                        GrabState::Downloading,
                        Some(&message),
                        None,
                        &now_rfc3339(),
                    )
                    .await?;
            }
        }
        notify_attention(
            store,
            &grab.hash,
            stuck,
            &filme,
            &grab.title,
            line.detalhe.as_deref().unwrap_or_default(),
            entry.extras.poster.as_deref(),
        )
        .await;
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

    fn torrent(state: &str) -> acervo_clients::TorrentInfo {
        serde_json::from_value(serde_json::json!({
            "hash": "h", "name": "n", "state": state, "save_path": "/", "progress": 0.4,
        }))
        .unwrap()
    }

    #[test]
    fn so_espera_na_fila_o_torrent_com_a_tag_e_sem_progresso() {
        let queued = |progress: f64| -> acervo_clients::TorrentInfo {
            serde_json::from_value(serde_json::json!({
                "hash": "h", "name": "n", "state": "pausedDL", "save_path": "/",
                "tags": QUEUE_TAG, "progress": progress,
            }))
            .unwrap()
        };
        assert!(is_waiting(&queued(0.0)));
        assert!(!is_waiting(&queued(0.02)));
        // Sem a tag, mesmo parado e sem progresso, não é da fila.
        let mut other = queued(0.0);
        other.tags = String::new();
        assert!(!is_waiting(&other));
    }

    #[test]
    fn arquivos_sumidos_so_viram_falha_sem_bloqueio_depois_de_30_min_observados() {
        let mut watch = Watch::default();
        let start = at("2026-10-03T12:00:00Z");
        let missing = torrent("missingFiles");
        for minutes in [0, 10, 29] {
            assert!(
                matches!(
                    client_trouble(
                        &mut watch,
                        &missing,
                        start + time::Duration::minutes(minutes)
                    ),
                    Some(Failure::Import(_))
                ),
                "{minutes} min"
            );
        }
        assert!(matches!(
            client_trouble(&mut watch, &missing, start + time::Duration::minutes(30)),
            Some(Failure::Lost(message)) if message == MISSING_FILES
        ));
        // Voltou ao normal: a contagem recomeça.
        assert!(client_trouble(&mut watch, &torrent("stalledDL"), start).is_none());
        assert!(matches!(
            client_trouble(&mut watch, &missing, start + time::Duration::hours(2)),
            Some(Failure::Import(_))
        ));
    }

    #[test]
    fn erro_com_espaco_vira_falha_com_bloqueio_que_expira() {
        let mut watch = Watch::default();
        let start = at("2026-10-03T12:00:00Z");
        let error = torrent("error");
        assert!(matches!(
            client_trouble(&mut watch, &error, start),
            Some(Failure::Import(_))
        ));
        let Some(Failure::Download(message)) =
            client_trouble(&mut watch, &error, start + time::Duration::minutes(31))
        else {
            panic!("erro persistente vira falha de download");
        };
        assert_eq!(message, CLIENT_ERROR);
        assert!(EXPIRING.contains(&message.as_str()));
        // Cada torrent conta por si.
        let mut other = torrent("error");
        other.hash = "outro".into();
        assert!(matches!(
            client_trouble(&mut watch, &other, start + time::Duration::minutes(31)),
            Some(Failure::Import(_))
        ));
    }

    #[test]
    fn sem_seeds_exige_30_min_observados_e_retomar_recomeca() {
        let now = at("2026-10-03T12:00:00Z");
        let mut watch = Watch::default();
        // O cliente diz que está ativo há horas (tempo de antes de parar), mas
        // este processo acabou de vê-lo: ainda não.
        let mut resumed = travado(now);
        resumed.time_active = 5 * 3600;
        assert!(!no_seeds(&mut watch, &resumed, now));
        assert!(!no_seeds(
            &mut watch,
            &resumed,
            now + time::Duration::minutes(29)
        ));
        let later = now + time::Duration::minutes(30);
        let mut quiet = resumed.clone();
        quiet.last_activity = later.unix_timestamp() - 1800;
        assert!(no_seeds(&mut watch, &quiet, later));

        // Parado ou na fila do cliente no meio: recomeça do zero.
        let mut watch = Watch::default();
        assert!(!no_seeds(&mut watch, &resumed, now));
        let mut held = resumed.clone();
        held.state = "queuedDL".into();
        assert!(!no_seeds(
            &mut watch,
            &held,
            now + time::Duration::minutes(15)
        ));
        assert!(!no_seeds(&mut watch, &quiet, later));
        assert!(no_seeds(
            &mut watch,
            &quiet,
            later + time::Duration::minutes(30)
        ));
    }

    #[test]
    fn atencao_avisa_uma_vez_depois_de_6_horas_seguidas() {
        let mut watch = Watch::default();
        let start = at("2026-10-03T00:00:00Z");
        let hours = |n: i64| start + time::Duration::hours(n);
        assert!(!attention_due(&mut watch, "h", true, start));
        assert!(!attention_due(&mut watch, "h", true, hours(5)));
        // Saiu de atenção: recomeça.
        assert!(!attention_due(&mut watch, "h", false, hours(5)));
        assert!(!attention_due(&mut watch, "h", true, hours(6)));
        assert!(attention_due(&mut watch, "h", true, hours(12)));
        // Uma vez só por grab.
        assert!(!attention_due(&mut watch, "h", true, hours(13)));
        assert!(!attention_due(&mut watch, "h", true, hours(30)));
        // O grab terminou: outro grab do mesmo torrent avisa de novo.
        watch.forget("h");
        assert!(!attention_due(&mut watch, "h", true, hours(31)));
        assert!(attention_due(&mut watch, "h", true, hours(37)));
    }

    #[test]
    fn fila_sem_grab_so_com_tag_categoria_e_passada_a_carencia() {
        let now = 1_000_000;
        let queued = |hash: &str, category: &str, tags: &str, added_on: i64| {
            serde_json::from_value::<acervo_clients::TorrentInfo>(serde_json::json!({
                "hash": hash, "name": hash, "state": "stoppedDL", "save_path": "/",
                "category": category, "tags": tags, "added_on": added_on,
            }))
            .unwrap()
        };
        let old = now - ORPHAN_GRACE;
        let torrents = [
            queued("orfao", "acervo", QUEUE_TAG, old),
            queued("com-grab", "acervo", QUEUE_TAG, old),
            queued("recente", "acervo", QUEUE_TAG, now - 60),
            queued("sem-data", "acervo", QUEUE_TAG, 0),
            queued("manual", "manual", QUEUE_TAG, old),
            queued("sem-tag", "acervo", "", old),
        ];
        let known = HashMap::from([("com-grab".to_owned(), 1)]);
        assert_eq!(orphans(&torrents, &known, "acervo", now), ["orfao"]);
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
        // O de erro do cliente também.
        assert!(!still_blocks(
            &blocked(Some(CLIENT_ERROR)),
            at("2026-10-08T00:00:00Z")
        ));
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

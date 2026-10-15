//! "Para lançar": prepara o pacote de um release que existe num tracker e não
//! no outro, para o usuário enviar à mão (ver `docs/para-lancar.md`).
//!
//! O preparo é um job em fundo, um por vez no processo: sonda, busca no
//! destino, regras, texto, capturas e `.torrent`. Cada passo grava o
//! andamento em `launches.progress`, que a tela acompanha. Falha técnica vira
//! `falhou` com a mensagem; regra reprovada vira `inelegivel`. Os arquivos
//! nunca mudam: o `.torrent` é dos arquivos da origem como estão, e o layout
//! dele é conferido contra a origem antes de valer.
//!
//! Segredo não sai: o announce (com a passkey) e a chave do `ImgBB` são
//! tirados de qualquer mensagem de erro gravada ([`redact`]).

pub(crate) mod imgbb;
pub(crate) mod rules;
pub(crate) mod text;
pub(crate) mod web;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use acervo_clients::{
    CreateTorrentOptions, QbitClient, TorrentInfo, TorrentTaskState, working_announce,
};
use acervo_core::DownloadHash;
use acervo_metadata::{DiscoverKind, country_name_pt};
use acervo_parser::Language;
use acervo_store::{LaunchState, Store};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::api::Catalog;
use crate::config::Config;
use crate::crossseed::{Origin, Source};
use crate::tasks::Cancel;
use crate::web::Web;
use rules::{
    DestinationRelease, Facts, Kind, ProbedVideo, SeriesStatus, Site, SourceFile, Verdict,
};
use text::{Credits, Input, Work};

/// O andamento de quem espera o pacote da frente.
pub(crate) const QUEUED: &str = "na fila";

/// Mensagem de quando falta a chave do `ImgBB` (spec, passo 6).
const NO_IMGBB_KEY: &str = "configure a chave do ImgBB em Configurações → Imagens";

/// Os vídeos do torrent, pela extensão.
const VIDEO_EXTENSIONS: [&str; 5] = ["mkv", "mp4", "avi", "m2ts", "ts"];

/// Onde as capturas são tiradas, em fração da duração: longe da abertura e
/// dos créditos.
const SCREEN_POINTS: [f64; 4] = [0.15, 0.35, 0.55, 0.75];

/// Intervalo entre consultas ao andamento da criação do `.torrent`.
const CREATOR_POLL: Duration = Duration::from_secs(5);
/// Falhas seguidas ao consultar o andamento que ainda se toleram: um blip
/// (timeout com o qBittorrent ocupado, 5xx) não pode jogar fora horas de hash.
const CREATOR_FAILURES: u32 = 3;
/// Teto da criação: o mesmo das tarefas de fundo.
const CREATOR_LIMIT: Duration = Duration::from_secs(2 * 3600);

/// Torrents do destino que têm o announce lido, no máximo, até achar um
/// funcionando.
const ANNOUNCE_TRIES: usize = 5;

/// Um pacote por vez no processo: ffmpeg, upload e hash pesam, e o
/// container é pequeno.
static SLOT: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(1));

/// Aviso de desligamento dos jobs. O job é um `tokio::spawn` solto: sem ele,
/// o fim do processo descartaria o future parado em [`follow_task`] e a
/// tarefa de criação ficaria no qBittorrent, lendo o conteúdo inteiro à toa.
static SHUTDOWN: LazyLock<Cancel> = LazyLock::new(Cancel::default);

/// Como termina o pacote cortado pelo desligamento.
const INTERRUPTED: &str =
    "interrompido: o serviço foi desligado no meio do preparo; prepare de novo";

/// Começa o desligamento: os jobs que esperam a vez desistem, e o que gera
/// o `.torrent` apaga a tarefa no qBittorrent e termina.
pub(crate) fn stop() {
    SHUTDOWN.cancel();
}

/// Espera os jobs saírem, até `grace`. Devolve quantos ainda rodavam ao
/// desistir.
pub(crate) async fn drain(grace: Duration) -> usize {
    let deadline = Instant::now() + grace;
    loop {
        let busy = CLAIMED.lock().unwrap_or_else(PoisonError::into_inner).len();
        if busy == 0 || Instant::now() >= deadline {
            return busy;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Os pares com job em andamento ou na fila. O banco também diz
/// `preparando`, mas dois cliques juntos leriam o estado antes de qualquer um
/// gravar.
static CLAIMED: LazyLock<Mutex<HashSet<(String, String)>>> = LazyLock::new(Mutex::default);

/// A posse do par enquanto o job dele vive; solta ao sair de escopo.
#[derive(Debug)]
pub(crate) struct Claim {
    pair: (String, String),
}

impl Drop for Claim {
    fn drop(&mut self) {
        CLAIMED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.pair);
    }
}

/// Toma o par para um job; `None` se outro já o tem.
pub(crate) fn claim(hash: &str, indexer: &str) -> Option<Claim> {
    let pair = (hash.to_ascii_lowercase(), indexer.to_owned());
    // A trava sai antes de qualquer `Claim` existir: o `Drop` dele a toma.
    let taken = CLAIMED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(pair.clone());
    if taken { Some(Claim { pair }) } else { None }
}

/// Como o preparo terminou, fora a falha técnica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    Ready,
    Ineligible,
}

/// O par em preparo e o que precisa sair de qualquer mensagem.
struct Job<'a> {
    store: &'a Store,
    hash: String,
    indexer: String,
    /// Announce e chave, conforme aparecem.
    secrets: Vec<String>,
    /// O último andamento gravado, para não regravar o mesmo.
    shown: String,
}

impl Job<'_> {
    /// Grava o andamento. Falha de banco aqui não derruba o preparo: o
    /// estado final é gravado de novo no fim.
    async fn step(&mut self, text: &str) {
        if self.shown == text {
            return;
        }
        text.clone_into(&mut self.shown);
        if let Err(error) = self
            .store
            .upsert_launch_state(
                &self.hash,
                &self.indexer,
                LaunchState::Preparando,
                Some(text),
                None,
            )
            .await
        {
            tracing::warn!(indexer = self.indexer, "andamento do lançamento: {error}");
        }
    }
}

/// Tira os segredos de uma mensagem que vai ao banco, ao log ou à tela.
pub(crate) fn redact(message: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(message.to_owned(), |text, secret| {
            text.replace(secret.as_str(), "<segredo>")
        })
}

/// O job de um par: espera a vez, prepara e grava o desfecho. Roda em
/// fundo; o par já está `preparando` com [`QUEUED`].
pub(crate) async fn run(web: Arc<Web>, hash: String, indexer: String, claim: Claim) {
    let _claim = claim;
    let store = match web.database.get() {
        Ok(store) => store.clone(),
        Err(error) => {
            tracing::error!(indexer, "lançamento sem banco: {error}");
            return;
        }
    };
    let permit = tokio::select! {
        permit = SLOT.acquire() => permit,
        () = SHUTDOWN.cancelled() => {
            // Na fila, ainda sem nada no qBittorrent: só deixa o motivo.
            let error = store
                .upsert_launch_state(
                    &hash.to_ascii_lowercase(),
                    &indexer,
                    LaunchState::Falhou,
                    None,
                    Some(INTERRUPTED),
                )
                .await;
            if let Err(error) = error {
                tracing::warn!(indexer, "pacote da fila não marcado como interrompido: {error}");
            }
            return;
        }
    };
    let Ok(_permit) = permit else {
        return;
    };
    let config = web.config();
    let mut job = Job {
        store: &store,
        hash: hash.to_ascii_lowercase(),
        indexer,
        secrets: Vec::new(),
        shown: QUEUED.to_owned(),
    };
    let outcome = prepare(&config, &web.catalog, &mut job).await;
    let (state, error) = match outcome {
        Ok(Finish::Ready) => (LaunchState::Pronto, None),
        Ok(Finish::Ineligible) => (LaunchState::Inelegivel, None),
        Err(error) => {
            let message = redact(&format!("{error:#}"), &job.secrets);
            tracing::warn!(indexer = job.indexer, "lançamento falhou: {message}");
            (LaunchState::Falhou, Some(message))
        }
    };
    save_outcome(&store, &job, state, error.as_deref(), &OUTCOME_RETRY).await;
}

/// Esperas entre as tentativas de gravar o desfecho: uma queda curta do
/// banco no fim de um job de horas não pode deixar o par `preparando`.
const OUTCOME_RETRY: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(20),
];

/// Grava o desfecho do job, repetindo a cada espera de `waits` se o banco
/// falhar. Se mesmo assim não grava, [`heal`] acaba tirando o par de
/// `preparando`.
async fn save_outcome(
    store: &Store,
    job: &Job<'_>,
    state: LaunchState,
    error: Option<&str>,
    waits: &[Duration],
) {
    let mut waits = waits.iter();
    loop {
        match store
            .upsert_launch_state(&job.hash, &job.indexer, state, None, error)
            .await
        {
            Ok(()) => return,
            Err(failure) => {
                let Some(wait) = waits.next() else {
                    tracing::error!(
                        indexer = job.indexer,
                        "desfecho do lançamento não gravado: {failure}"
                    );
                    return;
                };
                tracing::warn!(
                    indexer = job.indexer,
                    "desfecho do lançamento não gravado, de novo em instantes: {failure}"
                );
                tokio::time::sleep(*wait).await;
            }
        }
    }
}

/// Pacote que ficou `preparando` quando o serviço caiu nunca terminaria, e
/// o botão de preparar não vale nesse estado: vira `falhou`.
///
/// # Errors
///
/// Banco inalcançável.
pub(crate) async fn recover(store: &Store) -> Result<usize> {
    let mut recovered = 0;
    for launch in store.launches().await? {
        if launch.state == LaunchState::Preparando {
            store
                .upsert_launch_state(
                    &launch.hash,
                    &launch.indexer,
                    LaunchState::Falhou,
                    None,
                    Some("interrompido: o serviço reiniciou no meio do preparo; prepare de novo"),
                )
                .await?;
            recovered += 1;
        }
    }
    Ok(recovered)
}

/// Tira de `preparando` o pacote que nenhum job tem: o handler que gravou o
/// estado e falhou antes de disparar o job, ou o job que não conseguiu gravar
/// o desfecho. Sem isto, `preparar` e `descartar` recusariam o par e a tela
/// seguiria consultando o andamento até o serviço reiniciar.
///
/// Quem tem a posse ([`claim`]) tem job vivo e não é tocado. A posse é tomada
/// durante a troca, para que um `preparar` não comece no meio dela.
///
/// # Errors
///
/// Banco inalcançável.
pub(crate) async fn heal(store: &Store) -> Result<usize> {
    let mut healed = 0;
    for (hash, indexer, _) in store.launches_preparing().await? {
        let Some(_claim) = claim(&hash, &indexer) else {
            continue;
        };
        if store
            .fail_launch_if_preparing(&hash, &indexer, STALE)
            .await?
        {
            healed += 1;
        }
    }
    Ok(healed)
}

/// Como termina o pacote que ficou `preparando` sem job.
const STALE: &str = "interrompido: o preparo parou sem terminar; prepare de novo";

/// Os campos de um pacote que recomeça: o relatório, o texto e as capturas
/// velhos sairiam misturados com o andamento novo. O `.torrent` velho fica,
/// mas a rota de download só o entrega com o pacote pronto.
///
/// # Errors
///
/// Banco inalcançável.
pub(crate) async fn reset(store: &Store, hash: &str, indexer: &str) -> Result<()> {
    store.save_launch_report(hash, indexer, &json!({})).await?;
    store.save_launch_fields(hash, indexer, &json!({})).await?;
    store.save_launch_screens(hash, indexer, &json!([])).await?;
    Ok(())
}

// ---------------------------------------------------------------- funções puras

/// Se o caminho é de vídeo, pela extensão.
pub(crate) fn is_video(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// O vídeo principal: o maior. No empate, o primeiro pelo caminho, para o
/// mesmo pacote dar sempre o mesmo.
pub(crate) fn main_video(files: &[SourceFile]) -> Option<&SourceFile> {
    files
        .iter()
        .filter(|f| is_video(&f.path))
        .min_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)))
}

/// Os instantes das capturas, em segundos.
pub(crate) fn screen_times(duration: f64) -> Vec<f64> {
    SCREEN_POINTS.iter().map(|p| duration * p).collect()
}

/// A pasta raiz do torrent, ou o arquivo se for avulso: é o nome do release.
pub(crate) fn release_name(files: &[SourceFile]) -> String {
    let Some(first) = files.first() else {
        return String::new();
    };
    match first.path.split_once('/') {
        Some((root, _)) => root.to_owned(),
        None => first.path.clone(),
    }
}

/// O que o qBittorrent deve empacotar: `content_path` (pasta raiz, ou o
/// arquivo avulso); qBittorrent antigo sem ele, montado dos arquivos.
pub(crate) fn source_path(torrent: &TorrentInfo, files: &[SourceFile]) -> String {
    if !torrent.content_path.is_empty() {
        return torrent.content_path.clone();
    }
    PathBuf::from(&torrent.save_path)
        .join(release_name(files))
        .to_string_lossy()
        .into_owned()
}

/// O torrent tem pasta raiz própria (ou é um arquivo avulso) e o
/// `content_path` termina nela. Sem raiz (layout "sem subpasta" do
/// qBittorrent) o `content_path` de um pacote de vários arquivos é o
/// `save_path` inteiro, e o `addTask` empacotaria todos os outros torrents
/// da pasta: horas de leitura do disco para o `layout_matches` recusar no fim.
///
/// # Errors
///
/// Vários arquivos sem uma pasta raiz comum, ou `content_path` que não é o
/// release.
pub(crate) fn ensure_root(torrent: &TorrentInfo, files: &[SourceFile]) -> Result<()> {
    let release = release_name(files);
    if files.len() > 1 {
        let prefix = format!("{release}/");
        ensure!(
            files.iter().all(|f| f.path.starts_with(&prefix)),
            "o torrent de vários arquivos não tem pasta raiz (layout sem subpasta no qBittorrent): \
             o .torrent empacotaria a pasta de download inteira"
        );
    }
    if !torrent.content_path.is_empty() {
        let last = Path::new(&torrent.content_path)
            .file_name()
            .and_then(|n| n.to_str());
        ensure!(
            last == Some(release.as_str()),
            "o caminho do conteúdo no qBittorrent não é o do release `{release}`: \
             o .torrent empacotaria outra coisa"
        );
    }
    Ok(())
}

/// O `.torrent` gerado tem exatamente os arquivos da origem: sem isso o
/// cross-seed nunca acharia o publicado.
pub(crate) fn layout_matches(torrent: &[u8], files: &[SourceFile]) -> bool {
    let origin: Vec<(String, u64)> = files.iter().map(|f| (f.path.clone(), f.size)).collect();
    acervo_clients::layout(torrent)
        .is_some_and(|generated| crate::crossseed::same_layout(&origin, &generated))
}

/// Episódio avulso ou temporada, pelo nome do torrent de série. Mais de um
/// episódio sem ser a temporada inteira conta como temporada: as regras
/// conferem se ela está completa.
fn series_work(name: &str) -> Option<Work> {
    let parsed = acervo_parser::parse_episode_title(name)?;
    Some(match parsed.episodes.as_slice() {
        [episode] if !parsed.full_season => Work::Episode {
            season: parsed.season,
            episode: *episode,
        },
        _ => Work::Season {
            season: parsed.season,
        },
    })
}

fn series_status(status: Option<&str>) -> Option<SeriesStatus> {
    match status? {
        "ended" => Some(SeriesStatus::Ended),
        "continuing" => Some(SeriesStatus::Continuing),
        _ => None,
    }
}

fn non_empty(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

// ---------------------------------------------------------------- preparo

/// O que o catálogo sabe da obra do torrent.
struct Catalogued {
    work: Work,
    tmdb_kind: DiscoverKind,
    tmdb_id: u32,
    imdb_id: Option<String>,
    title: String,
    original_title: Option<String>,
    year: Option<u16>,
    original_language: Option<Language>,
    series_status: Option<SeriesStatus>,
    /// Os pares (temporada, episódio) exibidos da temporada (até hoje).
    season_episodes: Option<Vec<(u16, u16)>>,
    /// Episódios da temporada que ainda não foram ao ar (data futura ou ausente).
    season_pending: u32,
    /// A sinopse que o catálogo guarda, para quando o TMDB não tem outra.
    overview: Option<String>,
}

async fn catalogued(store: &Store, source: &Source) -> Result<Catalogued> {
    match source.origin {
        Origin::Movie(id) => {
            let entry = store
                .movies()
                .await?
                .into_iter()
                .find(|m| m.id == id)
                .context("o filme do torrent saiu do catálogo")?;
            let movie = entry.movie;
            Ok(Catalogued {
                work: Work::Movie,
                tmdb_kind: DiscoverKind::Movie,
                tmdb_id: movie.tmdb_id,
                imdb_id: movie.imdb_id,
                title: movie.title,
                original_title: movie.original_title,
                year: movie.year,
                original_language: movie
                    .original_language
                    .as_deref()
                    .and_then(Language::from_name),
                series_status: None,
                season_episodes: None,
                season_pending: 0,
                overview: non_empty(movie.overview.as_deref()),
            })
        }
        Origin::Series(id) => {
            let entry = store
                .series(id)
                .await?
                .context("a série do torrent saiu do catálogo")?;
            let work = series_work(&source.name).with_context(|| {
                format!(
                    "o nome do torrent não diz temporada e episódio: {}",
                    source.name
                )
            })?;
            let season = match work {
                Work::Episode { season, .. } | Work::Season { season } => season,
                Work::Movie => 0,
            };
            let today = time::OffsetDateTime::now_utc().date();
            let in_season = entry
                .episodes
                .iter()
                .filter(|e| e.episode.season == season)
                .count();
            let aired: Vec<(u16, u16)> = entry
                .episodes
                .iter()
                .filter(|e| e.episode.season == season)
                .filter(|e| {
                    crate::series::date(e.episode.air_date.as_deref()).is_some_and(|d| d <= today)
                })
                .map(|e| (e.episode.season, e.episode.number))
                .collect();
            let series = entry.series;
            Ok(Catalogued {
                work,
                tmdb_kind: DiscoverKind::Series,
                tmdb_id: series.tmdb_id,
                imdb_id: series.imdb_id,
                title: series.title,
                original_title: series.original_title,
                year: series.year,
                original_language: series
                    .original_language
                    .as_deref()
                    .and_then(Language::from_name),
                series_status: series_status(series.status.as_deref()),
                season_pending: u32::try_from(in_season - aired.len()).unwrap_or(u32::MAX),
                season_episodes: Some(aired),
                overview: non_empty(series.overview.as_deref()),
            })
        }
    }
}

/// O que o TMDB em pt-BR dá ao texto; vale acima do que o catálogo guarda.
struct TmdbText {
    title: Option<String>,
    original_title: Option<String>,
    year: Option<u16>,
    synopsis: Option<String>,
    credits: Credits,
}

/// Ficha e sinopse em pt-BR: do filme, ou do episódio/temporada, senão a da
/// série.
async fn tmdb_text(store: &Store, work: &Catalogued) -> Result<TmdbText> {
    let tmdb = crate::metadata::require_tmdb(store).await?;
    let details = tmdb
        .discover_details(work.tmdb_kind, work.tmdb_id)
        .await
        .context("ficha no TMDB")?;
    let specific = match work.work {
        Work::Movie => None,
        Work::Episode { season, episode } => tmdb
            .season_text(work.tmdb_id, season)
            .await
            .context("sinopse do episódio no TMDB")?
            .and_then(|s| s.episodes.into_iter().find(|e| e.number == episode))
            .and_then(|e| e.overview),
        Work::Season { season } => tmdb
            .season_text(work.tmdb_id, season)
            .await
            .context("sinopse da temporada no TMDB")?
            .and_then(|s| s.overview),
    };
    let directors = if work.tmdb_kind == DiscoverKind::Movie {
        details.directors
    } else {
        details.creators
    };
    Ok(TmdbText {
        title: non_empty(Some(&details.item.title)),
        original_title: non_empty(Some(&details.item.original_title)),
        year: details.item.year,
        synopsis: specific.or_else(|| non_empty(Some(&details.item.overview))),
        credits: Credits {
            directors,
            cast: details.cast.into_iter().map(|c| c.name).collect(),
            genres: details.genres,
            countries: details
                .countries
                .iter()
                .map(|c| country_name_pt(c))
                .collect(),
        },
    })
}

/// Um announce que funciona no destino, de um torrent do cliente que veio
/// dele. É a URL com passkey: segredo.
async fn announce(
    client: &QbitClient,
    torrents: &[TorrentInfo],
    owners: &HashMap<String, (String, Origin)>,
    indexer: &str,
) -> Result<Option<String>> {
    let from_destination = torrents.iter().filter(|t| {
        owners
            .get(&t.hash.to_ascii_lowercase())
            .is_some_and(|(owner, _)| owner == indexer)
    });
    for torrent in from_destination.take(ANNOUNCE_TRIES) {
        let trackers = client
            .trackers(&torrent.hash)
            .await
            .context("lendo os trackers de um torrent do destino")?;
        if let Some(url) = working_announce(&trackers) {
            return Ok(Some(url));
        }
    }
    Ok(None)
}

/// Esperas entre as tentativas de apagar uma tarefa de criação. Curtas: no
/// desligamento o serviço tem ~25 s para sair.
const DELETE_RETRY: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(8),
];

/// Apaga a tarefa de criação, repetindo se o cliente não responde: o erro
/// que derrubou o acompanhamento (rede, qBittorrent reiniciando) costuma ser
/// o mesmo que faria o primeiro delete falhar, e a tarefa órfã continua
/// lendo o pacote inteiro. Devolve se apagou.
async fn delete_task(client: &QbitClient, task: &str, waits: &[Duration]) -> bool {
    let mut waits = waits.iter();
    loop {
        match client.delete_torrent_task(task).await {
            Ok(()) => return true,
            Err(error) => {
                let Some(wait) = waits.next() else {
                    tracing::warn!("tarefa de criação de torrent não apagada: {error}");
                    return false;
                };
                tracing::warn!("tarefa de criação de torrent não apagada, de novo: {error}");
                tokio::time::sleep(*wait).await;
            }
        }
    }
}

/// Caminho sem barra final, para comparar o `sourcePath` do cliente com o
/// `content_path` do pacote.
fn plain(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// Apaga as tarefas de criação sobre `source_path` cuja id não está em
/// `before` (as que o cliente já tinha antes do `addTask`). Devolve as ids
/// que o cliente lista agora, ou `None` se não listou. Falha aqui não impede
/// o preparo.
async fn delete_matching(
    client: &QbitClient,
    source_path: &str,
    before: &HashSet<String>,
) -> Option<HashSet<String>> {
    let tasks = match client.torrent_tasks().await {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::warn!("tarefas de criação do cliente não listadas: {error}");
            return None;
        }
    };
    let wanted = plain(source_path);
    for task in tasks
        .iter()
        .filter(|t| plain(&t.source_path) == wanted && !before.contains(&t.task_id))
    {
        tracing::warn!("apagando tarefa de criação órfã do mesmo pacote");
        delete_task(client, &task.task_id, &DELETE_RETRY).await;
    }
    Some(tasks.into_iter().map(|t| t.task_id).collect())
}

/// Apaga as tarefas de criação do mesmo pacote que um acervo derrubado no
/// meio (queda, SIGKILL) deixou no cliente: sem isto elas seguem hasheando o
/// pacote enquanto a nova roda em paralelo. O acervo só tem um job por vez,
/// então toda tarefa sobre o mesmo caminho é dele e está órfã. Devolve as ids
/// que o cliente tinha, para [`create_torrent`] reconhecer depois a que o
/// `addTask` criou sem responder.
async fn clear_orphans(client: &QbitClient, source_path: &str) -> HashSet<String> {
    delete_matching(client, source_path, &HashSet::new())
        .await
        .unwrap_or_default()
}

/// Cria o `.torrent` no qBittorrent e apaga a tarefa em qualquer desfecho,
/// inclusive o desligamento (`cancel`). Tarefas do mesmo pacote deixadas por
/// uma queda anterior saem antes. Se o `addTask` falha sem resposta
/// (timeout, JSON inválido) o cliente pode tê-lo aceitado: relista e apaga a
/// tarefa nova do pacote antes de devolver o erro.
async fn create_torrent(
    client: &QbitClient,
    job: &mut Job<'_>,
    source_path: &str,
    announce: String,
    cancel: &Cancel,
) -> Result<Vec<u8>> {
    ensure!(!cancel.is_cancelled(), INTERRUPTED);
    let before = clear_orphans(client, source_path).await;
    let options = CreateTorrentOptions::new(source_path, vec![announce]);
    let task = match client.create_torrent_task(&options).await {
        Ok(task) => task,
        Err(error) => {
            delete_matching(client, source_path, &before).await;
            return Err(anyhow::Error::new(error)
                .context("pedindo o .torrent ao qBittorrent (precisa da versão 5)"));
        }
    };
    let result = tokio::select! {
        result = follow_task(client, job, &task) => result,
        () = cancel.cancelled() => Err(anyhow!(INTERRUPTED)),
    };
    delete_task(client, &task, &DELETE_RETRY).await;
    result
}

/// No reinício, apaga as tarefas de criação que um acervo anterior deixou
/// para pacotes que não estão mais `preparando` (`packages`: hash → se algum
/// destino dele está preparando). Só toca tarefa cujo `sourcePath` é o
/// `content_path` de um pacote do acervo; qualquer outra é do usuário.
/// Devolve quantas apagou.
async fn sweep_stale_tasks(client: &QbitClient, packages: &HashMap<String, bool>) -> usize {
    let (tasks, torrents) = match tokio::try_join!(
        async { client.torrent_tasks().await.map_err(anyhow::Error::new) },
        async { client.torrents().await.map_err(anyhow::Error::new) },
    ) {
        Ok(listed) => listed,
        Err(error) => {
            tracing::warn!("tarefas de criação órfãs não conferidas: {error:#}");
            return 0;
        }
    };
    let stale: HashSet<&str> = torrents
        .iter()
        .filter(|t| packages.get(&t.hash.to_ascii_lowercase()) == Some(&false))
        .map(|t| plain(&t.content_path))
        .filter(|path| !path.is_empty())
        .collect();
    let mut deleted = 0;
    for task in tasks
        .iter()
        .filter(|t| stale.contains(plain(&t.source_path)))
    {
        tracing::warn!("apagando tarefa de criação de um preparo que não existe mais");
        if delete_task(client, &task.task_id, &DELETE_RETRY).await {
            deleted += 1;
        }
    }
    deleted
}

/// Tarefa de criação de torrent que um preparo interrompido deixou no
/// qBittorrent segue lendo o pacote: limpa em segundo plano, depois de
/// [`recover`], sem atrasar a subida do serviço.
pub(crate) fn spawn_clear_stale_tasks(config: Arc<Config>, store: Store) {
    tokio::spawn(async move {
        match clear_stale_tasks(&config, &store).await {
            Ok(0) => {}
            Ok(count) => tracing::warn!(tarefas = count, "tarefas de criação órfãs apagadas"),
            Err(error) => tracing::warn!("tarefas de criação órfãs não conferidas: {error:#}"),
        }
    });
}

/// Chamado no início do serviço, depois de [`recover`]: tira do qBittorrent
/// as tarefas de criação que o preparo interrompido deixou lendo o pacote.
///
/// # Errors
///
/// Banco inalcançável.
async fn clear_stale_tasks(config: &Config, store: &Store) -> Result<usize> {
    let mut packages: HashMap<String, bool> = HashMap::new();
    for launch in store.launches().await? {
        let preparing = packages
            .entry(launch.hash.to_ascii_lowercase())
            .or_default();
        *preparing |= launch.state == LaunchState::Preparando;
    }
    if packages.is_empty() || config.qbittorrent().is_none() {
        return Ok(0);
    }
    let Ok(client) = crate::grab::qbit(config).await else {
        return Ok(0);
    };
    Ok(sweep_stale_tasks(&client, &packages).await)
}

/// Conta uma falha ao consultar o andamento: dentro da tolerância segue
/// (`Ok`), na [`CREATOR_FAILURES`]-ésima seguida devolve o erro.
fn tolerate_failure<E>(failures: &mut u32, error: E) -> Result<()>
where
    E: std::error::Error + Send + Sync + 'static,
{
    *failures += 1;
    if *failures >= CREATOR_FAILURES {
        return Err(anyhow::Error::new(error).context(format!(
            "{failures} consultas seguidas ao andamento do .torrent falharam"
        )));
    }
    tracing::warn!("consulta ao andamento do .torrent falhou, tentando de novo: {error}");
    Ok(())
}

async fn follow_task(client: &QbitClient, job: &mut Job<'_>, task: &str) -> Result<Vec<u8>> {
    let deadline = Instant::now() + CREATOR_LIMIT;
    let mut failures = 0;
    loop {
        match client.torrent_task_status(task).await {
            Err(error) => tolerate_failure(&mut failures, error)?,
            Ok(polled) => {
                failures = 0;
                let status = polled
                    .context("o qBittorrent perdeu a tarefa de criação do .torrent (reiniciou?)")?;
                match status.state {
                    TorrentTaskState::Finished => {
                        return Ok(client.torrent_task_file(task).await?);
                    }
                    TorrentTaskState::Failed => bail!(
                        "o qBittorrent não conseguiu criar o .torrent: {}",
                        status.error.as_deref().unwrap_or("sem mensagem")
                    ),
                    TorrentTaskState::Queued => {
                        job.step("gerando o .torrent: na fila do qBittorrent").await;
                    }
                    TorrentTaskState::Running | TorrentTaskState::Unknown => {
                        job.step(&format!("gerando o .torrent ({}%)", status.progress))
                            .await;
                    }
                }
            }
        }
        ensure!(
            Instant::now() < deadline,
            "o qBittorrent levou mais de 2 h para criar o .torrent"
        );
        tokio::time::sleep(CREATOR_POLL).await;
    }
}

/// Os passos do pacote, da origem ao `.torrent`.
#[allow(clippy::too_many_lines)]
async fn prepare(config: &Config, catalog: &Catalog, job: &mut Job<'_>) -> Result<Finish> {
    let store = job.store;
    let indexer = job.indexer.clone();
    let site = Site::from_indexer(&indexer)
        .with_context(|| format!("`{indexer}` não é destino de lançamento"))?;

    // 1. Origem: completo, arquivos inteiros.
    job.step("lendo o torrent no cliente").await;
    let client = crate::grab::qbit(config).await?;
    let torrents = client
        .torrents()
        .await
        .context("lendo os torrents do cliente")?;
    let torrent = torrents
        .iter()
        .find(|t| t.hash.eq_ignore_ascii_case(&job.hash))
        .context("o torrent de origem não está mais no cliente")?;
    ensure!(
        torrent.progress >= 1.0,
        "o torrent de origem ainda não terminou de baixar"
    );
    let client_files = client
        .files(&DownloadHash::new(torrent.hash.clone()))
        .await
        .context("lendo os arquivos do torrent")?;
    ensure!(
        !client_files.is_empty()
            && client_files
                .iter()
                .all(|f| f.priority > 0 && f.progress >= 1.0),
        "pacote baixado em parte: o .torrent precisaria de arquivos que não estão no disco"
    );
    let owners = crate::crossseed::owners(store).await?;
    let (origin_indexer, origin) = owners
        .get(&job.hash)
        .cloned()
        .context("o torrent não tem grab do acervo: não dá para saber que obra é")?;
    ensure!(
        origin_indexer != indexer,
        "o torrent veio do próprio {}",
        site.label()
    );
    let source = Source {
        hash: job.hash.clone(),
        name: torrent.name.clone(),
        save_path: torrent.save_path.clone(),
        indexer: origin_indexer,
        origin,
    };
    let files: Vec<SourceFile> = client_files
        .iter()
        .map(|f| SourceFile {
            path: f.name.clone(),
            size: f.size,
        })
        .collect();
    // Antes de gastar ffprobe, ffmpeg e upload num pacote que o `.torrent`
    // não saberia empacotar.
    ensure_root(torrent, &files)?;
    let main = main_video(&files)
        .context("nenhum vídeo (.mkv, .mp4, .avi, .m2ts, .ts) no torrent")?
        .clone();
    let total_size: u64 = files.iter().map(|f| f.size).sum();

    // 2. Sonda: todos os vídeos para a checagem; o texto usa o principal.
    job.step("lendo os vídeos com o ffprobe").await;
    let mut videos = Vec::new();
    for file in files.iter().filter(|f| is_video(&f.path)) {
        let path = Path::new(&torrent.save_path).join(&file.path);
        let media = crate::mediainfo::probe_full(&path)
            .await
            .with_context(|| format!("o ffprobe não leu {}", file.path))?;
        videos.push(ProbedVideo {
            path: file.path.clone(),
            media,
        });
    }
    let main_media = videos
        .iter()
        .find(|v| v.path == main.path)
        .map(|v| v.media.clone())
        .context("o vídeo principal não foi lido")?;
    let main_path = Path::new(&torrent.save_path).join(&main.path);

    // 3. Destino: a mesma consulta do cross-seed, que sai do cache.
    job.step(&format!("buscando no {}", site.label())).await;
    let work = catalogued(store, &source).await?;
    let query = crate::crossseed::query(store, &source)
        .await?
        .context("sem como montar a busca no destino")?;
    let kind: Kind = work.work.kind();
    let page = catalog
        .search(&indexer, &query)
        .await
        .map_err(|error| anyhow!("buscando no {}: {error}", site.label()))?;
    let mut destination: Vec<DestinationRelease> = page
        .releases
        .iter()
        .map(|r| DestinationRelease::new(&r.title, r.size, kind))
        .collect();
    // Episódio avulso: a busca por "S01E03" não traz o pacote da temporada, e é
    // ele que a 4.2.1.13 compara. Uma segunda busca pela temporada inteira,
    // só para episódio.
    if kind == Kind::Episode
        && let acervo_indexers::SearchMode::Tv {
            season: Some(season),
            ..
        } = query.mode
    {
        let season_page = catalog
            .search(&indexer, &query.clone().with_episode(season, ""))
            .await
            .map_err(|error| anyhow!("buscando no {}: {error}", site.label()))?;
        for release in &season_page.releases {
            let extra = DestinationRelease::new(&release.title, release.size, kind);
            if extra.season_pack.is_some() && !destination.iter().any(|d| d.title == extra.title) {
                destination.push(extra);
            }
        }
    }

    // 4. Checagem.
    job.step("conferindo as regras").await;
    let release = release_name(&files);
    let facts = Facts {
        kind,
        release_name: release.clone(),
        files: files.clone(),
        videos,
        original_language: work.original_language,
        series_status: work.series_status,
        season_episodes: work.season_episodes.clone(),
        season_pending: work.season_pending,
        destination,
    };
    let mut report = rules::check(site, &facts);
    store
        .save_launch_report(&job.hash, &indexer, &serde_json::to_value(&report)?)
        .await?;
    if report.verdict == Verdict::Inelegivel {
        return Ok(Finish::Ineligible);
    }

    // 5. Texto: os dados agora; o texto sai depois das capturas, que entram
    // na descrição.
    job.step("buscando a ficha no TMDB").await;
    let tmdb = tmdb_text(store, &work).await?;
    let national = matches!(
        work.original_language,
        Some(Language::Portuguese | Language::PortugueseBr)
    );

    // 6. Capturas.
    let key = config.imgbb_key().context(NO_IMGBB_KEY)?.to_owned();
    job.secrets.push(key.clone());
    let duration = main_media
        .duration_seconds
        .filter(|d| d.is_finite() && *d > 0.0)
        .context("o ffprobe não disse a duração do vídeo: sem ela não há onde tirar as capturas")?;
    let imgbb = imgbb::Imgbb::new()?;
    let times = screen_times(duration);
    let mut screens = Vec::new();
    for (index, at) in times.iter().enumerate() {
        let number = index + 1;
        job.step(&format!("captura {number} de {}", times.len()))
            .await;
        let png = crate::mediainfo::screenshot(&main_path, *at).await?;
        let screen = imgbb
            .upload(&key, png, &format!("captura-{number}.png"))
            .await
            .with_context(|| format!("enviando a captura {number}"))?;
        screens.push(screen);
    }
    store
        .save_launch_screens(&job.hash, &indexer, &serde_json::to_value(&screens)?)
        .await?;

    let input = Input {
        title: tmdb.title.unwrap_or(work.title),
        original_title: tmdb.original_title.or(work.original_title),
        national,
        year: tmdb.year.or(work.year),
        imdb_id: work.imdb_id,
        tmdb_id: work.tmdb_id,
        work: work.work,
        synopsis: tmdb.synopsis.or(work.overview),
        credits: tmdb.credits,
        release_name: release,
        files: files.clone(),
        size: total_size,
        media: main_media,
        screens: screens.iter().map(|s| s.url.clone()).collect(),
    };
    let built = text::build_fields(site, &input);
    report.checks.extend(built.warnings);
    store
        .save_launch_report(&job.hash, &indexer, &serde_json::to_value(&report)?)
        .await?;
    store
        .save_launch_fields(&job.hash, &indexer, &serde_json::to_value(&built.fields)?)
        .await?;

    // 7. `.torrent`, com o announce do destino, conferido contra a origem.
    job.step(&format!("lendo o announce do {}", site.label()))
        .await;
    let announce = announce(&client, &torrents, &owners, &indexer)
        .await?
        .with_context(|| {
            format!(
                "nenhum torrent do {} no cliente para ler o announce",
                site.label()
            )
        })?;
    job.secrets.push(announce.clone());
    job.step("gerando o .torrent").await;
    let path = source_path(torrent, &files);
    let bytes = create_torrent(&client, job, &path, announce, &SHUTDOWN).await?;
    ensure!(
        layout_matches(&bytes, &files),
        "o .torrent gerado não tem os mesmos arquivos da origem; nada foi guardado"
    );
    store
        .save_launch_torrent(&job.hash, &indexer, &bytes)
        .await?;
    Ok(Finish::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> SourceFile {
        SourceFile {
            path: path.into(),
            size,
        }
    }

    #[test]
    fn consulta_ao_andamento_tolera_falhas_ate_o_limite() {
        let blip = || std::io::Error::other("timeout");
        let mut failures = 0;
        for _ in 1..CREATOR_FAILURES {
            assert!(tolerate_failure(&mut failures, blip()).is_ok());
        }
        // A seguinte estoura; uma consulta boa no meio zeraria a conta
        // (`failures = 0` em `follow_task`).
        assert!(tolerate_failure(&mut failures, blip()).is_err());
    }

    #[test]
    fn video_principal_e_o_maior_entre_os_videos() {
        let files = [
            file("Filme/Filme.nfo", 10),
            file("Filme/Sample/sample.mkv", 50),
            file("Filme/Filme.mkv", 4_000),
            file("Filme/Filme.pt-BR.srt", 9_000),
        ];
        assert_eq!(main_video(&files).unwrap().path, "Filme/Filme.mkv");
        // Extensão em maiúsculas também é vídeo; empate fica com o primeiro
        // caminho.
        let files = [file("S/E02.MP4", 7), file("S/E01.m2ts", 7)];
        assert_eq!(main_video(&files).unwrap().path, "S/E01.m2ts");
        assert!(main_video(&[file("a.srt", 1)]).is_none());
        assert!(is_video("x.ts") && !is_video("x.tsx") && !is_video("mkv"));
    }

    #[test]
    fn capturas_saem_a_15_35_55_e_75_por_cento_da_duracao() {
        let times = screen_times(6_000.0);
        assert_eq!(times.len(), 4);
        for (got, want) in times.iter().zip([900.0, 2_100.0, 3_300.0, 4_500.0]) {
            assert!((got - want).abs() < 1e-6, "{got} != {want}");
        }
    }

    #[test]
    fn nome_do_release_e_a_pasta_raiz_ou_o_arquivo() {
        assert_eq!(
            release_name(&[file("Filme.2024.1080p/Filme.mkv", 1)]),
            "Filme.2024.1080p"
        );
        assert_eq!(release_name(&[file("Filme.2024.mkv", 1)]), "Filme.2024.mkv");
        assert_eq!(release_name(&[]), "");
    }

    fn info(save_path: &str, content_path: &str) -> TorrentInfo {
        serde_json::from_value(json!({
            "hash": "h", "name": "n", "state": "uploading", "save_path": save_path,
            "content_path": content_path,
        }))
        .unwrap()
    }

    #[test]
    fn origem_do_torrent_novo_e_o_content_path() {
        let files = [file("Pasta/a.mkv", 1)];
        assert_eq!(
            source_path(&info("/media/d", "/media/d/Pasta"), &files),
            "/media/d/Pasta"
        );
        // qBittorrent sem `content_path`: a pasta raiz dentro do save_path.
        assert_eq!(source_path(&info("/media/d", ""), &files), "/media/d/Pasta");
    }

    #[test]
    fn pacote_sem_pasta_raiz_nao_vai_ao_qbittorrent() {
        // Layout sem subpasta: `content_path` é o `save_path` inteiro e os
        // arquivos vêm sem raiz.
        let season = [file("E01.mkv", 1), file("E02.mkv", 1)];
        let error = ensure_root(&info("/media/series", "/media/series"), &season).unwrap_err();
        assert!(format!("{error}").contains("pasta raiz"), "{error}");
        // Raiz só em parte dos arquivos também não vale.
        let mixed = [file("Temp/E01.mkv", 1), file("E02.mkv", 1)];
        assert!(ensure_root(&info("/media/series", "/media/series/Temp"), &mixed).is_err());
        // Com raiz, o `content_path` precisa terminar nela.
        let rooted = [file("Temp/E01.mkv", 1), file("Temp/E02.mkv", 1)];
        assert!(ensure_root(&info("/media/series", "/media/series/Temp"), &rooted).is_ok());
        assert!(ensure_root(&info("/media/series", "/media/series/Temp/"), &rooted).is_ok());
        assert!(ensure_root(&info("/media/series", "/media/series"), &rooted).is_err());
        // Sem `content_path` (qBittorrent antigo), vale a raiz dos arquivos.
        assert!(ensure_root(&info("/media/series", ""), &rooted).is_ok());
        // Arquivo avulso: o próprio arquivo; pasta com um arquivo só, a pasta.
        let single = [file("Filme.mkv", 1)];
        assert!(ensure_root(&info("/media/f", "/media/f/Filme.mkv"), &single).is_ok());
        assert!(ensure_root(&info("/media/f", "/media/f"), &single).is_err());
        let boxed = [file("Filme/Filme.mkv", 1)];
        assert!(ensure_root(&info("/media/f", "/media/f/Filme"), &boxed).is_ok());
    }

    /// Um `.torrent` v1 mínimo com os arquivos dados (`pieces` vazio: o
    /// layout não o lê). Caminho vazio é o torrent de arquivo avulso.
    fn torrent(name: &str, files: &[(&[&str], u64)]) -> Vec<u8> {
        let mut out = b"d4:infod".to_vec();
        if let [(path, size)] = files
            && path.is_empty()
        {
            out.extend(format!("6:lengthi{size}e").bytes());
        } else {
            out.extend(b"5:filesl");
            for (path, size) in files {
                out.extend(format!("d6:lengthi{size}e4:pathl").bytes());
                for part in *path {
                    out.extend(format!("{}:{part}", part.len()).bytes());
                }
                out.extend(b"ee");
            }
            out.extend(b"e");
        }
        out.extend(format!("4:name{}:{name}", name.len()).bytes());
        out.extend(b"12:piece lengthi16384e6:pieces0:ee");
        out
    }

    #[test]
    fn torrent_gerado_precisa_dos_mesmos_arquivos_da_origem() {
        let origin = [file("Filme/Filme.mkv", 4_000), file("Filme/Filme.srt", 10)];
        let same = torrent("Filme", &[(&["Filme.srt"], 10), (&["Filme.mkv"], 4_000)]);
        assert!(layout_matches(&same, &origin));
        // Pasta raiz renomeada, arquivo a menos ou tamanho diferente: não vale.
        let renamed = torrent("Outro", &[(&["Filme.mkv"], 4_000), (&["Filme.srt"], 10)]);
        assert!(!layout_matches(&renamed, &origin));
        let missing = torrent("Filme", &[(&["Filme.mkv"], 4_000)]);
        assert!(!layout_matches(&missing, &origin));
        let bigger = torrent("Filme", &[(&["Filme.mkv"], 4_001), (&["Filme.srt"], 10)]);
        assert!(!layout_matches(&bigger, &origin));
        // Arquivo avulso.
        let single = torrent("Filme.mkv", &[(&[], 4_000)]);
        assert!(layout_matches(&single, &[file("Filme.mkv", 4_000)]));
        assert!(!layout_matches(b"lixo", &[file("Filme.mkv", 4_000)]));
    }

    #[test]
    fn mensagem_de_erro_nunca_leva_announce_nem_chave() {
        let secrets = vec![
            "https://tracker.exemplo/announce?passkey=abc123".to_owned(),
            "chave0123".to_owned(),
            String::new(),
        ];
        let message = "falhou https://tracker.exemplo/announce?passkey=abc123 com chave0123";
        let shown = redact(message, &secrets);
        assert_eq!(shown, "falhou <segredo> com <segredo>");
    }

    #[test]
    fn serie_vira_episodio_ou_temporada_pelo_nome() {
        assert_eq!(
            series_work("Show.S01E02.1080p.WEB-DL.DUAL"),
            Some(Work::Episode {
                season: 1,
                episode: 2
            })
        );
        assert_eq!(
            series_work("Show.S02.1080p.WEB-DL.DUAL"),
            Some(Work::Season { season: 2 })
        );
        assert_eq!(series_work("qualquer coisa"), None);
        assert_eq!(series_status(Some("ended")), Some(SeriesStatus::Ended));
        assert_eq!(series_status(Some("upcoming")), None);
    }

    #[tokio::test]
    async fn desligar_no_meio_da_criacao_apaga_a_tarefa_no_qbittorrent() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let Some(db) = acervo_store::testing::TestDb::new("lancar_desligamento").await else {
            return;
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/addTask"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"taskID": "t1"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrentcreator/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "taskID": "t1", "status": "Running", "progress": 10,
            }])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/deleteTask"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        let mut job = Job {
            store: &db.store,
            hash: "a".repeat(40),
            indexer: "bjsharecustom".into(),
            secrets: Vec::new(),
            shown: QUEUED.to_owned(),
        };
        let cancel = Cancel::default();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            stopper.cancel();
        });
        let result = create_torrent(
            &client,
            &mut job,
            "/media/Filme",
            "https://t.exemplo/announce".into(),
            &cancel,
        )
        .await;
        assert!(
            format!("{:#}", result.unwrap_err()).contains("interrompido"),
            "o desligamento deve cortar o acompanhamento"
        );
        // O `expect(1)` do deleteTask é conferido ao soltar o servidor.
        drop(server);
        // Já desligado, nem cria a tarefa.
        let again = create_torrent(&client, &mut job, "/media/Filme", "x".into(), &cancel).await;
        assert!(again.is_err());
        db.drop().await;
    }

    /// Um qBittorrent de mentira com login e a lista de tarefas dada.
    async fn qbit_with_tasks(tasks: serde_json::Value) -> (wiremock::MockServer, QbitClient) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrentcreator/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tasks))
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        (server, client)
    }

    #[tokio::test]
    async fn tarefa_orfa_do_mesmo_pacote_e_apagada_antes_de_criar_outra() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let Some(db) = acervo_store::testing::TestDb::new("lancar_orfas").await else {
            return;
        };
        let (server, client) = qbit_with_tasks(json!([
            // A de um acervo que caiu no meio, sobre o mesmo pacote.
            {"taskID": "velha", "sourcePath": "/media/Filme/", "status": "Running"},
            // Outro pacote: não é dela.
            {"taskID": "alheia", "sourcePath": "/media/Outro", "status": "Running"},
        ]))
        .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/addTask"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"taskID": "nova"})))
            .mount(&server)
            .await;
        for (task, times) in [("velha", 1), ("nova", 1), ("alheia", 0)] {
            Mock::given(method("POST"))
                .and(path("/api/v2/torrentcreator/deleteTask"))
                .and(body_string_contains(format!("taskID={task}")))
                .respond_with(ResponseTemplate::new(200))
                .expect(times)
                .mount(&server)
                .await;
        }
        let mut job = Job {
            store: &db.store,
            hash: "a".repeat(40),
            indexer: "bjsharecustom".into(),
            secrets: Vec::new(),
            shown: QUEUED.to_owned(),
        };
        let cancel = Cancel::default();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            stopper.cancel();
        });
        let result = create_torrent(
            &client,
            &mut job,
            "/media/Filme",
            "https://t.exemplo/announce".into(),
            &cancel,
        )
        .await;
        assert!(result.is_err());
        // Os `expect` dos deletes são conferidos ao soltar o servidor.
        drop(server);
        db.drop().await;
    }

    #[tokio::test]
    async fn add_task_sem_resposta_apaga_a_tarefa_que_o_cliente_criou() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let Some(db) = acervo_store::testing::TestDb::new("lancar_add_ambiguo").await else {
            return;
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        // Antes do addTask só há uma tarefa alheia; depois, a fantasma.
        Mock::given(method("GET"))
            .and(path("/api/v2/torrentcreator/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"taskID": "alheia", "sourcePath": "/media/Outro", "status": "Running"},
            ])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrentcreator/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"taskID": "alheia", "sourcePath": "/media/Outro", "status": "Running"},
                {"taskID": "fantasma", "sourcePath": "/media/Filme", "status": "Running"},
            ])))
            .mount(&server)
            .await;
        // O cliente aceitou, mas a resposta não é JSON.
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/addTask"))
            .respond_with(ResponseTemplate::new(200).set_body_string("isto não é json"))
            .mount(&server)
            .await;
        for (task, times) in [("fantasma", 1), ("alheia", 0)] {
            Mock::given(method("POST"))
                .and(path("/api/v2/torrentcreator/deleteTask"))
                .and(body_string_contains(format!("taskID={task}")))
                .respond_with(ResponseTemplate::new(200))
                .expect(times)
                .mount(&server)
                .await;
        }
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        let mut job = Job {
            store: &db.store,
            hash: "a".repeat(40),
            indexer: "bjsharecustom".into(),
            secrets: Vec::new(),
            shown: QUEUED.to_owned(),
        };
        let result = create_torrent(
            &client,
            &mut job,
            "/media/Filme",
            "https://t.exemplo/announce".into(),
            &Cancel::default(),
        )
        .await;
        assert!(result.is_err());
        // Os `expect` dos deletes são conferidos ao soltar o servidor.
        drop(server);
        db.drop().await;
    }

    #[tokio::test]
    async fn reinicio_apaga_tarefa_de_pacote_que_nao_esta_mais_preparando() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (server, client) = qbit_with_tasks(json!([
            {"taskID": "velha", "sourcePath": "/media/Filme/", "status": "Running"},
            {"taskID": "viva", "sourcePath": "/media/Serie", "status": "Running"},
            {"taskID": "alheia", "sourcePath": "/media/Outro", "status": "Finished"},
        ]))
        .await;
        let torrent = |hash: &str, path: &str| {
            json!({"hash": hash, "name": "n", "save_path": "/media", "content_path": path,
                   "progress": 1.0, "state": "uploading"})
        };
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                torrent(&"a".repeat(40), "/media/Filme"),
                torrent(&"b".repeat(40), "/media/Serie"),
                torrent(&"c".repeat(40), "/media/Outro"),
            ])))
            .mount(&server)
            .await;
        for (task, times) in [("velha", 1), ("viva", 0), ("alheia", 0)] {
            Mock::given(method("POST"))
                .and(path("/api/v2/torrentcreator/deleteTask"))
                .and(body_string_contains(format!("taskID={task}")))
                .respond_with(ResponseTemplate::new(200))
                .expect(times)
                .mount(&server)
                .await;
        }
        // `a` é pacote do acervo parado; `b` ainda está preparando; `c` não é
        // pacote do acervo.
        let packages = HashMap::from([("a".repeat(40), false), ("b".repeat(40), true)]);
        assert_eq!(sweep_stale_tasks(&client, &packages).await, 1);
        drop(server);
    }

    #[tokio::test]
    async fn apagar_a_tarefa_repete_quando_o_cliente_nao_responde() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        // Cai duas vezes e volta.
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/deleteTask"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/deleteTask"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        let wait = Duration::from_millis(10);
        // Sem repetição a primeira queda já perderia a tarefa.
        assert!(!delete_task(&client, "t1", &[]).await);
        assert!(delete_task(&client, "t1", &[wait, wait, wait]).await);
        // Esgotadas as tentativas, desiste sem erro.
        let down = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&down)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrentcreator/deleteTask"))
            .respond_with(ResponseTemplate::new(500))
            .expect(3)
            .mount(&down)
            .await;
        let client = QbitClient::login(&down.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!delete_task(&client, "t1", &[wait, wait]).await);
    }

    #[tokio::test]
    async fn resgate_tira_o_par_orfao_de_preparando() {
        let Some(db) = acervo_store::testing::TestDb::new("lancar_resgate").await else {
            return;
        };
        let store = &db.store;
        let hash = "c".repeat(40);
        store
            .upsert_launch_state(
                &hash,
                "bjsharecustom",
                LaunchState::Preparando,
                Some(QUEUED),
                None,
            )
            .await
            .unwrap();
        // Com job vivo (a posse está tomada) o par não é tocado.
        let alive = claim(&hash, "bjsharecustom").unwrap();
        assert_eq!(heal(store).await.unwrap(), 0);
        assert_eq!(
            store
                .launch(&hash, "bjsharecustom")
                .await
                .unwrap()
                .unwrap()
                .state,
            LaunchState::Preparando
        );
        // Sem job (handler que falhou antes de disparar, ou desfecho que não
        // gravou): vira `falhou`, e o par volta a aceitar `preparar`.
        drop(alive);
        assert_eq!(heal(store).await.unwrap(), 1);
        let launch = store.launch(&hash, "bjsharecustom").await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Falhou);
        assert_eq!(launch.error.as_deref(), Some(STALE));
        assert_eq!(heal(store).await.unwrap(), 0);
        assert!(claim(&hash, "bjsharecustom").is_some());
        db.drop().await;
    }

    #[test]
    fn um_job_por_par() {
        let first = claim("ABCD", "bjsharecustom").unwrap();
        assert!(claim("abcd", "bjsharecustom").is_none());
        // Outro destino do mesmo torrent é outro par.
        let other = claim("abcd", "amigosshare").unwrap();
        drop(first);
        assert!(claim("abcd", "bjsharecustom").is_some());
        drop(other);
    }
}

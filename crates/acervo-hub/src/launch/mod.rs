//! "Para lançar": prepara o pacote de um release que existe num tracker e não
//! no outro, para o usuário enviar à mão (ver `docs/para-lancar.md`).
//!
//! O preparo é um job em fundo, um por vez no processo: sonda, busca no
//! destino, regras, texto, capturas e `.torrent`. Cada passo grava o
//! andamento em `launches.progress`, que a tela acompanha. Falha técnica vira
//! `falhou` com a mensagem; regra reprovada vira `inelegivel`. Os arquivos
//! nunca mudam: o `.torrent` é dos arquivos da origem como estão, e o layout
//! dele é conferido contra a origem antes de valer. A única exceção é a pasta
//! adaptada do BJ (legenda PT embutida que vira `.srt` ao lado do vídeo): uma
//! pasta nova em `<raiz>/lancamentos/bj/` com hardlinks da origem e os `.srt`
//! ([`adapt`] planeja, [`folder`] monta e apaga); aí o `.torrent` é dela.
//!
//! Segredo não sai: o announce (com a passkey) e a chave do `ImgBB` são
//! tirados de qualquer mensagem de erro gravada ([`redact`]).

pub(crate) mod adapt;
pub(crate) mod amigos;
pub(crate) mod folder;
pub(crate) mod imgbb;
pub(crate) mod name_check;
pub(crate) mod rules;
pub(crate) mod subtitle_sniff;
pub(crate) mod text;
pub(crate) mod web;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use acervo_clients::{
    AddOptions, CreateTorrentOptions, NewTorrent, QbitClient, TorrentInfo, TorrentTaskState,
    working_announce,
};
use acervo_core::DownloadHash;
use acervo_metadata::{DiscoverKind, country_name_pt};
use acervo_parser::Language;
use acervo_store::{Launch, LaunchState, Store};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::api::Catalog;
use crate::config::Config;
use crate::crossseed::{Origin, Source};
use crate::live::{self, Bus};
use crate::tasks::Cancel;
use crate::web::Web;
use adapt::{AdaptationPlan, Extraction};
use folder::Cleanup;
use rules::{
    DestinationRelease, Facts, Kind, ProbedVideo, Report, SeriesStatus, Site, SourceFile, Verdict,
};
use subtitle_sniff::Sniffs;
use text::{Credits, Input, Work};

/// O andamento de quem espera o pacote da frente.
pub(crate) const QUEUED: &str = "na fila";

/// O aviso do laudo quando falta a chave do `ImgBB` (spec, passo 6): o pacote
/// segue sem links e as capturas saem para baixar.
const NO_IMGBB_WARNING: &str = "sem a chave do ImgBB (Configurações → Imagens): as capturas não \
     foram hospedadas; baixe os 4 PNG do pacote e anexe-os no formulário";

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
    /// Cada mudança de estado ou de andamento avisa a tela (`lancar`).
    events: Bus,
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
        self.events.emit(&[live::LAUNCH]);
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
            web.events.emit(&[live::LAUNCH]);
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
        events: web.events.clone(),
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
            Ok(()) => {
                job.events.emit(&[live::LAUNCH]);
                return;
            }
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

/// O instante da captura `number` (1 a 4): o mesmo que o preparo usa, para a
/// rota de download tirar o quadro igual ao que iria ao `ImgBB`.
pub(crate) fn screen_time(duration: f64, number: usize) -> Option<f64> {
    screen_times(duration).get(number.checked_sub(1)?).copied()
}

/// Quantas capturas o pacote tem.
pub(crate) const SCREEN_COUNT: usize = SCREEN_POINTS.len();

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

/// Os `(caminho, tamanho)` dos arquivos da origem.
fn origin_layout(files: &[SourceFile]) -> Vec<(String, u64)> {
    files.iter().map(|f| (f.path.clone(), f.size)).collect()
}

/// O `.torrent` gerado tem exatamente o layout `expected` (os arquivos da
/// origem, ou os da pasta adaptada): sem isso o cross-seed nunca acharia o
/// publicado.
pub(crate) fn has_layout(torrent: &[u8], expected: &[(String, u64)]) -> bool {
    acervo_clients::layout(torrent)
        .is_some_and(|generated| crate::crossseed::same_layout(expected, &generated))
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
/// `content_path` de um pacote do acervo, ou a pasta adaptada de um par que
/// não está preparando (`adapted`); qualquer outra é do usuário. Devolve
/// quantas apagou.
async fn sweep_stale_tasks(
    client: &QbitClient,
    packages: &HashMap<String, bool>,
    adapted: &HashSet<String>,
) -> usize {
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
        .chain(adapted.iter().map(|path| plain(path)))
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
    let mut adapted = HashSet::new();
    for launch in store.launches().await? {
        let preparing = launch.state == LaunchState::Preparando;
        *packages
            .entry(launch.hash.to_ascii_lowercase())
            .or_default() |= preparing;
        if !preparing && let Some(path) = launch.adapted_path {
            adapted.insert(path);
        }
    }
    if packages.is_empty() || config.qbittorrent().is_none() {
        return Ok(0);
    }
    let Ok(client) = crate::grab::qbit(config).await else {
        return Ok(0);
    };
    Ok(sweep_stale_tasks(&client, &packages, &adapted).await)
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

// ---------------------------------------------------------------- pasta adaptada

/// Por que um pacote já enviado não tem a pasta mexida.
const SENT_FOLDER: &str = "o pacote já foi enviado ao site: a pasta adaptada é de onde o publicado \
                           semeia, e não é apagada nem refeita";

/// A pasta adaptada montada: onde está e o layout dela, relativo ao pai.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Adapted {
    target: PathBuf,
    layout: Vec<(String, u64)>,
}

/// O layout gravado da pasta adaptada (`[[caminho, tamanho]]`), se o pacote
/// tem uma.
pub(crate) fn adapted_layout(launch: &Launch) -> Option<Vec<(String, u64)>> {
    launch
        .adapted_layout
        .as_ref()
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .filter(|layout: &Vec<(String, u64)>| !layout.is_empty())
}

/// A pasta adaptada do pacote pode ser apagada: ele não foi ao site. Depois
/// do envio, o irmão do cross-seed semeia dela.
pub(crate) fn may_remove_folder(launch: &Launch) -> bool {
    launch.sent_at.is_none()
        && !matches!(launch.state, LaunchState::Enviado | LaunchState::Semeando)
}

/// Outro par, já enviado ou semeando, usa a mesma pasta.
fn folder_in_use(launches: &[Launch], hash: &str, indexer: &str, target: &Path) -> bool {
    launches.iter().any(|l| {
        !(l.hash.eq_ignore_ascii_case(hash) && l.indexer == indexer)
            && l.adapted_path
                .as_deref()
                .is_some_and(|p| Path::new(p) == target)
            && !may_remove_folder(l)
    })
}

/// Os caminhos, relativos ao pai da pasta adaptada, que os arquivos da origem
/// têm nela: os mesmos, ou dentro da pasta nova no arquivo único.
fn origin_in_folder(files: &[SourceFile]) -> HashSet<String> {
    let single = files.len() == 1 && !files[0].path.contains('/');
    match adapt::folder_name(files) {
        Some(name) if single => files.iter().map(|f| format!("{name}/{}", f.path)).collect(),
        _ => files.iter().map(|f| f.path.clone()).collect(),
    }
}

/// Os `.srt` que o acervo gerou na pasta adaptada `target` do pacote, com o
/// tamanho gravado no layout (`None`: montagem interrompida, sem layout, e
/// qualquer tamanho serve). Só vale para a pasta que o pacote registrou como
/// dele (`adapted_path`): sem isso nenhum `.srt` é do acervo, e a limpeza
/// recusa a pasta. É o que ela pode apagar além dos hardlinks.
fn generated_subtitles(
    files: &[SourceFile],
    launch: Option<&Launch>,
    target: &Path,
) -> HashMap<String, Option<u64>> {
    let mut out = HashMap::new();
    let Some(launch) = launch else {
        return out;
    };
    if launch.adapted_path.as_deref().map(Path::new) != Some(target) {
        return out;
    }
    if let Some(layout) = adapted_layout(launch) {
        let origin = origin_in_folder(files);
        out.extend(
            layout
                .into_iter()
                .filter(|(path, _)| folder::is_srt(path) && !origin.contains(path))
                .map(|(path, size)| (path, Some(size))),
        );
    } else {
        // Caminho gravado e layout não: a montagem caiu no meio. O que o laudo
        // mandou extrair pode estar no disco, com o tamanho que for.
        let planned: Vec<Extraction> = launch
            .report
            .get("adaptations")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        out.extend(planned.into_iter().map(|e| (e.output, None)));
    }
    out
}

/// O laudo como vai ao banco, com o nome de cada faixa a extrair
/// (`adaptations[].faixa`): a tela diz "a partir da faixa 3 (Português -
/// Brasil)".
/// O laudo como vai ao banco: o de [`report_value`] mais o tipo do pacote
/// (`filme`, `episodio` ou `temporada`) e o tipo de áudio (`dual`, `dublado`,
/// `legendado`, `nacional`), que o envio pelo site usa para escolher a
/// categoria e a opção de áudio.
fn report_json(report: &Report, facts: &rules::Facts, main: &str) -> Result<Value> {
    let mut value = report_value(report, &facts.videos, main)?;
    value["tipo"] = json!(kind_name(facts.kind));
    if let Some(audio) = rules::package_audio(facts) {
        value["audio"] = json!(audio);
    }
    Ok(value)
}

pub(crate) const fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Movie => "filme",
        Kind::Episode => "episodio",
        Kind::Season => "temporada",
    }
}

fn report_value(report: &Report, videos: &[ProbedVideo], main: &str) -> Result<Value> {
    let mut value = serde_json::to_value(report)?;
    // O que a tela mostra ao lado dos botões de captura, e a duração que a
    // rota de download usa para cair nos mesmos instantes do preparo.
    if let Some(video) = videos.iter().find(|v| v.path == main)
        && let Some(facts) = &video.media.video
    {
        value["capturas"] = json!({
            "arquivo": main,
            "largura": facts.width,
            "altura": facts.height,
            "duracao": video.media.duration_seconds,
        });
    }
    if let Some(list) = value.get_mut("adaptations").and_then(Value::as_array_mut) {
        for (item, extraction) in list.iter_mut().zip(&report.adaptations) {
            if let Some(label) = track_label(videos, extraction) {
                item["faixa"] = json!(label);
            }
        }
    }
    Ok(value)
}

/// O nome da faixa de legenda que a extração lê: o título dela, ou o idioma.
fn track_label(videos: &[ProbedVideo], extraction: &Extraction) -> Option<String> {
    let track = videos
        .iter()
        .find(|v| v.path == extraction.video)?
        .media
        .subtitles
        .iter()
        .find(|s| s.index == extraction.stream)?;
    Some(non_empty(track.title.as_deref()).unwrap_or_else(|| {
        match track.raw_language.as_deref().filter(|r| !r.is_empty()) {
            Some(raw) => format!("Português, {raw}"),
            None => "Português".to_owned(),
        }
    }))
}

/// Roda a limpeza de [`folder::remove`] fora do runtime.
async fn clean(
    target: PathBuf,
    root: PathBuf,
    origin: Vec<PathBuf>,
    generated: HashMap<String, Option<u64>>,
) -> Result<Cleanup> {
    tokio::task::spawn_blocking(move || folder::remove(&target, &root, &origin, &generated))
        .await
        .context("a limpeza da pasta adaptada parou")?
}

/// Onde ficam as pastas adaptadas da origem: o pai do `save_path`.
fn launch_root(save_path: &Path) -> Result<PathBuf> {
    save_path
        .parent()
        .map(Path::to_path_buf)
        .context("a pasta de download da origem não tem pasta acima: sem onde pôr `lancamentos`")
}

/// O passo "adaptando a pasta". Apaga a pasta adaptada anterior do par (e
/// qualquer coisa já no lugar da nova), sempre pela limpeza segura; com
/// adaptação no laudo, extrai as legendas, monta a pasta e grava caminho e
/// layout. Sem adaptação, `None`: o `.torrent` é da origem.
///
/// # Errors
///
/// Origem que não deixa montar o plano, pasta com o que o acervo não criou
/// (nada é apagado), extração ou montagem que falhou (o que foi criado é
/// desfeito), banco inalcançável.
async fn adapt_folder(
    job: &mut Job<'_>,
    torrent: &TorrentInfo,
    files: &[SourceFile],
    videos: &[ProbedVideo],
    sniffs: &Sniffs,
    report: &Report,
    site: Site,
) -> Result<Option<Adapted>> {
    let store = job.store;
    let save_path = PathBuf::from(&torrent.save_path);
    let plan: Option<AdaptationPlan> = if report.adaptations.is_empty() {
        None
    } else {
        let content = PathBuf::from(source_path(torrent, files));
        Some(
            adapt::plan(files, videos, sniffs, &content, &save_path, site).context(
                "a legenda PT precisa ir para arquivo separado, mas a origem não deixa montar \
                 a pasta adaptada",
            )?,
        )
    };
    let launches = store.launches().await?;
    let own = launches
        .iter()
        .find(|l| l.hash.eq_ignore_ascii_case(&job.hash) && l.indexer == job.indexer);
    let previous = own.and_then(|l| l.adapted_path.clone()).map(PathBuf::from);
    if plan.is_none() && previous.is_none() {
        return Ok(None);
    }
    job.step("adaptando a pasta").await;
    if let Some(own) = own {
        ensure!(may_remove_folder(own), SENT_FOLDER);
    }
    let root = launch_root(&save_path)?;
    let origin: Vec<PathBuf> = files.iter().map(|f| save_path.join(&f.path)).collect();
    let mut targets: Vec<PathBuf> = previous.into_iter().collect();
    if let Some(plan) = &plan
        && !targets.contains(&plan.target)
    {
        targets.push(plan.target.clone());
    }
    for target in targets {
        ensure!(
            !folder_in_use(&launches, &job.hash, &job.indexer, &target),
            "a pasta `{}` é de outro pacote já enviado ao site: nada foi apagado",
            target.display()
        );
        let generated = generated_subtitles(files, own, &target);
        match clean(target, root.clone(), origin.clone(), generated).await? {
            Cleanup::Absent | Cleanup::Removed => {}
            Cleanup::Refused(reason) => {
                tracing::warn!(indexer = job.indexer, "pasta adaptada não limpa: {reason}");
                bail!("{reason}; tire à mão o que não é do acervo e prepare de novo");
            }
        }
    }
    store
        .save_launch_adaptation(&job.hash, &job.indexer, None, None)
        .await?;
    let Some(plan) = plan else {
        return Ok(None);
    };

    build_folder(job, plan, &save_path).await.map(Some)
}

/// Extrai as legendas do plano, monta a pasta e grava caminho e layout no
/// pacote. As legendas vêm primeiro: falha do ffmpeg não deixa nada no disco.
async fn build_folder(
    job: &mut Job<'_>,
    plan: AdaptationPlan,
    save_path: &Path,
) -> Result<Adapted> {
    let store = job.store;
    let total = plan.extractions.len();
    let mut subtitles = Vec::with_capacity(total);
    for (index, extraction) in plan.extractions.iter().enumerate() {
        job.step(&format!(
            "adaptando a pasta: extraindo a legenda {} de {total}",
            index + 1
        ))
        .await;
        let video = save_path.join(&extraction.video);
        let bytes = crate::mediainfo::extract_subtitle_srt(&video, extraction.stream)
            .await
            .with_context(|| {
                format!(
                    "extraindo a legenda da faixa {} de {}",
                    extraction.stream, extraction.video
                )
            })?;
        subtitles.push((extraction.output.clone(), bytes));
    }
    job.step("adaptando a pasta: hardlinks e legendas").await;
    let target = plan.target.to_string_lossy().into_owned();
    // Gravada antes de criar: uma queda no meio deixa o rastro para a limpeza.
    store
        .save_launch_adaptation(&job.hash, &job.indexer, Some(&target), None)
        .await?;
    let built = {
        let plan = plan.clone();
        let root = launch_root(save_path)?;
        tokio::task::spawn_blocking(move || folder::build(&plan, &root, &subtitles))
            .await
            .context("a montagem da pasta adaptada parou")?
    };
    let layout = match built {
        Ok(layout) => layout,
        Err(error) => {
            // `build` já desfez o que criou.
            if let Err(failure) = store
                .save_launch_adaptation(&job.hash, &job.indexer, None, None)
                .await
            {
                tracing::warn!(
                    indexer = job.indexer,
                    "pasta adaptada desfeita, banco não: {failure}"
                );
            }
            return Err(error.context("montando a pasta adaptada"));
        }
    };
    store
        .save_launch_adaptation(
            &job.hash,
            &job.indexer,
            Some(&target),
            Some(&serde_json::to_value(&layout)?),
        )
        .await?;
    Ok(Adapted {
        target: plan.target,
        layout,
    })
}

/// Apaga a pasta adaptada do pacote descartado, pela limpeza segura. Devolve o
/// aviso para a tela quando ela fica no disco (enviado, com o que o acervo não
/// criou, ou sem a origem para conferir os hardlinks).
pub(crate) async fn discard_folder(
    config: &Config,
    store: &Store,
    launch: &Launch,
) -> Option<String> {
    let target = launch.adapted_path.as_deref()?;
    if !may_remove_folder(launch) {
        return Some(format!("a pasta adaptada `{target}` fica: {SENT_FOLDER}"));
    }
    match remove_folder(config, store, launch, Path::new(target)).await {
        Ok(Cleanup::Absent | Cleanup::Removed) => {
            if let Err(error) = store
                .save_launch_adaptation(&launch.hash, &launch.indexer, None, None)
                .await
            {
                tracing::warn!(
                    indexer = launch.indexer,
                    "pasta adaptada apagada, banco não: {error}"
                );
            }
            None
        }
        Ok(Cleanup::Refused(reason)) => {
            tracing::warn!(
                indexer = launch.indexer,
                "pasta adaptada não apagada: {reason}"
            );
            Some(reason)
        }
        Err(error) => {
            let message = format!("a pasta adaptada `{target}` ficou no disco: {error:#}");
            tracing::warn!(indexer = launch.indexer, "{message}");
            Some(message)
        }
    }
}

/// A limpeza da pasta de um pacote fora do preparo: a origem vem do cliente
/// (sem ela não há inode para conferir, e nada é apagado). Pasta que já não
/// existe não pede o cliente: não há o que conferir nem o que apagar.
async fn remove_folder(
    config: &Config,
    store: &Store,
    launch: &Launch,
    target: &Path,
) -> Result<Cleanup> {
    match std::fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Cleanup::Absent),
        _ => {}
    }
    let launches = store.launches().await?;
    if folder_in_use(&launches, &launch.hash, &launch.indexer, target) {
        return Ok(Cleanup::Refused(format!(
            "a pasta `{}` é de outro pacote já enviado ao site: nada foi apagado",
            target.display()
        )));
    }
    let client = crate::grab::qbit(config).await?;
    let torrents = client
        .torrents()
        .await
        .context("lendo os torrents do cliente")?;
    let Some(torrent) = torrents
        .iter()
        .find(|t| t.hash.eq_ignore_ascii_case(&launch.hash))
    else {
        return Ok(Cleanup::Refused(format!(
            "a pasta adaptada `{}` ficou no disco: o torrent de origem saiu do cliente, e sem ele \
             não há como conferir que os arquivos são hardlinks dele",
            target.display()
        )));
    };
    let files: Vec<SourceFile> = client
        .files(&DownloadHash::new(torrent.hash.clone()))
        .await
        .context("lendo os arquivos do torrent")?
        .into_iter()
        .map(|f| SourceFile {
            path: f.name,
            size: f.size,
        })
        .collect();
    let save_path = PathBuf::from(&torrent.save_path);
    let root = launch_root(&save_path)?;
    let origin = files.iter().map(|f| save_path.join(&f.path)).collect();
    let generated = generated_subtitles(&files, Some(launch), target);
    clean(target.to_path_buf(), root, origin, generated).await
}

/// Lê do disco as legendas em arquivo sem idioma no nome (`<vídeo>.srt`) e
/// as classifica pelo conteúdo. Só leitura. Arquivo ilegível ou maior que
/// [`subtitle_sniff::MAX_BYTES`] fica de fora: sem leitura a legenda não vale
/// como PT, e a regra reprova dizendo que falta.
async fn sniff_subtitles(save_path: &str, files: &[SourceFile], videos: &[ProbedVideo]) -> Sniffs {
    let targets: Vec<(String, PathBuf, Option<f64>)> = rules::sniff_targets(files, videos)
        .into_iter()
        .map(|(path, video)| {
            (
                path.to_owned(),
                Path::new(save_path).join(path),
                video.media.duration_seconds,
            )
        })
        .collect();
    let read = tokio::task::spawn_blocking(move || {
        let mut found = Sniffs::new();
        for (relative, disk, duration) in targets {
            match read_limited(&disk, subtitle_sniff::MAX_BYTES) {
                Ok(bytes) => {
                    found.insert(relative, subtitle_sniff::sniff(&bytes, duration));
                }
                Err(error) => {
                    tracing::warn!("legenda `{relative}` não lida para classificar: {error}");
                }
            }
        }
        found
    })
    .await;
    read.unwrap_or_else(|error| {
        tracing::warn!("leitura das legendas interrompida: {error}");
        Sniffs::new()
    })
}

/// O conteúdo do arquivo, ou erro se passar de `limit` bytes.
fn read_limited(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!("passa de {limit} bytes")));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------- semear

/// Quantos nomes a mensagem de "o que difere" lista de cada tipo.
const DIFF_LISTED: usize = 5;

/// Se o `.torrent` já estava no cliente (mesmo hash) ou foi adicionado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seeded {
    Added,
    AlreadyThere,
}

/// A pasta de download do pacote, como o cliente a vê: o pai da pasta
/// adaptada, ou a da origem.
fn seed_path(torrents: &[TorrentInfo], launch: &Launch) -> Result<String> {
    if let (Some(_), Some(path)) = (adapted_layout(launch), launch.adapted_path.as_deref()) {
        let parent = Path::new(path)
            .parent()
            .context("a pasta adaptada não tem pasta pai")?;
        return Ok(parent.to_string_lossy().into_owned());
    }
    torrents
        .iter()
        .find(|t| t.hash.eq_ignore_ascii_case(&launch.hash))
        .map(|t| t.save_path.clone())
        .context("o torrent de origem saiu do cliente: sem ele não há onde semear o pacote")
}

/// Os arquivos que o pacote publica: o layout da pasta adaptada, ou o dos
/// arquivos do torrent de origem no cliente.
///
/// # Errors
///
/// Cliente inalcançável ou torrent de origem fora dele.
pub(crate) async fn package_layout(config: &Config, launch: &Launch) -> Result<Vec<(String, u64)>> {
    if let Some(layout) = adapted_layout(launch) {
        return Ok(layout);
    }
    let client = crate::grab::qbit(config).await?;
    let torrents = client.torrents().await.context("lendo o cliente")?;
    ensure!(
        torrents
            .iter()
            .any(|t| t.hash.eq_ignore_ascii_case(&launch.hash)),
        "o torrent de origem saiu do cliente: sem ele não há como conferir o layout"
    );
    let files = client
        .files(&DownloadHash::new(launch.hash.clone()))
        .await
        .context("lendo os arquivos do torrent de origem")?;
    Ok(files.into_iter().map(|f| (f.name, f.size)).collect())
}

/// O host de uma URL de tracker, minúsculo. A URL leva a passkey: só o host
/// sai daqui.
pub(crate) fn tracker_host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// A identidade de um tracker: host, porta (a padrão do esquema, se a URL não
/// traz) e o primeiro segmento do caminho (`host:443/announce`). Dois
/// trackers no mesmo host com caminhos de announce diferentes não são o
/// mesmo. A query (passkey) nunca entra: o resultado pode ser logado.
pub(crate) fn tracker_key(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port_or_known_default()?;
    let first = url.path_segments()?.next().unwrap_or_default();
    Some(format!("{host}:{port}/{first}"))
}

/// Adiciona o `.torrent` do pacote ao cliente **parado**, na categoria do
/// acervo, com a tag da fila de conferência e a de semear do destino: o
/// `verify` do cross-seed confere os arquivos e só inicia com 100%.
///
/// Depois do `add` manda conferir já, sem esperar a volta do `verify`.
///
/// Mesmo hash já no cliente (o torrent de outro tracker, ou o gerado de um
/// envio anterior), ou outro hash do mesmo tracker com os mesmos arquivos (o
/// `.torrent` baixado do site e adicionado à mão): não adiciona de novo, mas o adota: põe a tag de semear do
/// destino; se ele está incompleto ou parado sem estar na fila, para e
/// encaminha à conferência (`PENDING_TAG`); e, se nenhum tracker dele é o do
/// destino, acrescenta o announce do `.torrent` (segredo: não é logado).
///
/// # Errors
///
/// Cliente inalcançável, `.torrent` ilegível ou origem fora do cliente.
pub(crate) async fn seed_package(
    config: &Config,
    launch: &Launch,
    torrent: Vec<u8>,
) -> Result<Seeded> {
    let hash =
        acervo_clients::info_hash(&torrent).context("o .torrent não tem infohash legível")?;
    let client = crate::grab::qbit(config).await?;
    let torrents = client.torrents().await.context("lendo o cliente")?;
    seed_into(
        &client,
        &config.library.category,
        &torrents,
        launch,
        torrent,
        &hash,
    )
    .await
}

/// [`seed_package`] sobre um cliente já aberto e sua lista de torrents.
async fn seed_into(
    client: &QbitClient,
    category: &str,
    torrents: &[TorrentInfo],
    launch: &Launch,
    torrent: Vec<u8>,
    hash: &str,
) -> Result<Seeded> {
    if let Some(existing) = torrents.iter().find(|t| t.hash.eq_ignore_ascii_case(hash)) {
        adopt_existing(client, existing, launch, &torrent).await?;
        return Ok(Seeded::AlreadyThere);
    }
    let save_path = seed_path(torrents, launch)?;
    // Outro hash, mesmo tracker e mesmos arquivos já no cliente (o `.torrent`
    // que o usuário baixou do site e adicionou à mão, com `source`): um
    // irmão a mais seria duplicata que o tracker não registra. Adota-o.
    if let Some(existing) =
        crate::crossseed::same_tracker_sibling(client, torrents, &save_path, &torrent).await?
    {
        adopt_existing(client, &existing, launch, &torrent).await?;
        return Ok(Seeded::AlreadyThere);
    }
    let options = AddOptions {
        category: category.to_owned(),
        save_path: Some(save_path),
        stopped: true,
        stop_after_metadata: false,
        tags: vec![
            crate::crossseed::PENDING_TAG.to_owned(),
            acervo_core::seed_tag(&launch.indexer),
        ],
    };
    crate::crossseed::add_sibling(client, NewTorrent::File(torrent), &options, hash)
        .await
        .context("adicionando o .torrent ao cliente")?;
    Ok(Seeded::Added)
}

/// O torrent do pacote já estava no cliente: garante a semeadura no destino.
async fn adopt_existing(
    client: &QbitClient,
    existing: &TorrentInfo,
    launch: &Launch,
    torrent: &[u8],
) -> Result<()> {
    let hash = existing.hash.as_str();
    let seed = acervo_core::seed_tag(&launch.indexer);
    if !existing.has_tag(&seed) {
        client
            .add_tag(&[hash], &seed)
            .await
            .context("pondo a tag de semear no torrent do cliente")?;
    }
    // O announce do destino: sem um tracker dele no torrent, o pacote nunca
    // semeia lá, por mais que esteja no cliente.
    if let Some(announce) = acervo_clients::announce(torrent) {
        let host = tracker_host(&announce);
        let trackers = client
            .trackers(hash)
            .await
            .context("lendo os trackers do torrent do cliente")?;
        let present = trackers
            .iter()
            .filter(|t| t.is_real())
            .any(|t| host.is_some() && tracker_host(&t.url) == host);
        if !present {
            client
                .add_trackers(hash, &[announce])
                .await
                .context("acrescentando o tracker do destino ao torrent do cliente")?;
        }
    }
    let stopped = existing.state.starts_with("stopped") || existing.state.starts_with("paused");
    if !existing.has_tag(crate::crossseed::PENDING_TAG) && (existing.progress < 1.0 || stopped) {
        // Incompleto, ou parado sem ninguém que o confira: para (nada de
        // baixar por cima) e vai à fila, onde o `verify` confere e só inicia
        // com 100%.
        client
            .stop(&[hash])
            .await
            .context("parando o torrent do cliente")?;
        client
            .add_tag(&[hash], crate::crossseed::PENDING_TAG)
            .await
            .context("pondo o torrent do cliente na conferência")?;
    }
    Ok(())
}

/// Diz, sem tocar em nada além de nomes de arquivo e tamanhos, o que o
/// `.torrent` do site tem de diferente do pacote.
pub(crate) fn layout_diff(expected: &[(String, u64)], got: &[(String, u64)]) -> String {
    let expected: HashMap<&str, u64> = expected.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let got: HashMap<&str, u64> = got.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let mut missing: Vec<&str> = expected
        .keys()
        .filter(|p| !got.contains_key(*p))
        .copied()
        .collect();
    let mut extra: Vec<&str> = got
        .keys()
        .filter(|p| !expected.contains_key(*p))
        .copied()
        .collect();
    let mut sizes: Vec<String> = expected
        .iter()
        .filter_map(|(path, size)| {
            got.get(path)
                .filter(|other| *other != size)
                .map(|other| format!("{path} (pacote {size} B, torrent {other} B)"))
        })
        .collect();
    missing.sort_unstable();
    extra.sort_unstable();
    sizes.sort_unstable();
    let list = |label: &str, items: &[String]| -> Option<String> {
        if items.is_empty() {
            return None;
        }
        let shown = items
            .iter()
            .take(DIFF_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = items.len().saturating_sub(DIFF_LISTED);
        Some(if more > 0 {
            format!("{label}: {shown} e mais {more}")
        } else {
            format!("{label}: {shown}")
        })
    };
    let owned = |names: Vec<&str>| names.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let parts: Vec<String> = [
        list("faltam no torrent", &owned(missing)),
        list("a mais no torrent", &owned(extra)),
        list("tamanho diferente", &sizes),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "os arquivos têm a mesma lista, mas em ordem ou forma que o acervo não reconhece".into()
    } else {
        parts.join("; ")
    }
}

/// O status do tracker do destino para um torrent já no cliente, em texto
/// e sem a URL: funcionando, não registrado, erro ou sem resposta (ainda
/// não contatado, atualizando ou desligado). Com `host` (o do destino), só o
/// tracker desse host conta: o torrent de outro tracker, com o announce do
/// destino acrescentado, tem o status dos dois misturado. Sem tracker desse
/// host: "sem resposta".
fn tracker_status_of(trackers: &[acervo_clients::Tracker], host: Option<&str>) -> &'static str {
    let only: Vec<acervo_clients::Tracker>;
    let trackers = if let Some(host) = host {
        only = trackers
            .iter()
            .filter(|t| t.is_real() && tracker_host(&t.url).as_deref() == Some(host))
            .cloned()
            .collect();
        &only[..]
    } else {
        trackers
    };
    if crate::grab::unregistered(trackers) {
        return "não registrado";
    }
    let real: Vec<&acervo_clients::Tracker> = trackers.iter().filter(|t| t.is_real()).collect();
    if real.iter().any(|t| t.status == 2) {
        "funcionando"
    } else if real.iter().any(|t| t.status == 4) {
        "erro"
    } else {
        "sem resposta"
    }
}

/// O status do tracker do pacote enviado: do torrent do site, se ele está
/// no cliente, senão do gerado. Torrent fora do cliente ou cliente fora do
/// ar: "sem resposta".
pub(crate) async fn seeded_tracker_status(
    config: &Config,
    store: &Store,
    launch: &Launch,
) -> &'static str {
    const NONE: &str = "sem resposta";
    let bytes = match store.launch_torrent(&launch.hash, &launch.indexer).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                indexer = launch.indexer,
                "torrent do pacote ilegível: {error}"
            );
            None
        }
    };
    let generated = bytes.as_deref().and_then(acervo_clients::info_hash);
    // O tracker que importa é o do destino, pelo host do announce do `.torrent`
    // do pacote (só o host: a URL leva a passkey).
    let host = bytes
        .as_deref()
        .and_then(acervo_clients::announce)
        .and_then(|announce| tracker_host(&announce));
    let Ok(client) = crate::grab::qbit(config).await else {
        return NONE;
    };
    let Ok(torrents) = client.torrents().await else {
        return NONE;
    };
    let in_client = |hash: &str| torrents.iter().find(|t| t.hash.eq_ignore_ascii_case(hash));
    let Some(torrent) = launch
        .site_hash
        .as_deref()
        .and_then(in_client)
        .or_else(|| generated.as_deref().and_then(in_client))
    else {
        return NONE;
    };
    match client.trackers(&torrent.hash).await {
        Ok(trackers) => tracker_status_of(&trackers, host.as_deref()),
        Err(error) => {
            tracing::warn!(indexer = launch.indexer, "trackers ilegíveis: {error}");
            NONE
        }
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
    let sniffs = sniff_subtitles(&torrent.save_path, &files, &videos).await;
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
        sniffs,
    };
    let mut report = rules::check(site, &facts);
    store
        .save_launch_report(
            &job.hash,
            &indexer,
            &report_json(&report, &facts, &main.path)?,
        )
        .await?;
    if report.verdict == Verdict::Inelegivel {
        // Preparo anterior que montou a pasta: sem pacote a enviar, ela não
        // pode ficar segurando o vídeo no disco.
        let launches = store.launches().await?;
        if let Some(own) = launches
            .iter()
            .find(|l| l.hash.eq_ignore_ascii_case(&job.hash) && l.indexer == indexer)
            && let Some(warning) = discard_folder(config, store, own).await
        {
            tracing::warn!(indexer, "pasta adaptada anterior fica: {warning}");
        }
        return Ok(Finish::Ineligible);
    }

    // 5. Texto: os dados agora; o texto sai depois das capturas, que entram
    // na descrição. O MediaInfo vem antes de gastar TMDB e upload: no Amigos
    // a falta dele reprova o pacote.
    job.step("gerando o relatório do MediaInfo").await;
    let (mediainfo, mediainfo_warning) = mediainfo_for(site, &main_path).await?;
    job.step("buscando a ficha no TMDB").await;
    let tmdb = tmdb_text(store, &work).await?;
    let national = matches!(
        work.original_language,
        Some(Language::Portuguese | Language::PortugueseBr)
    );

    // 6. Capturas.
    let screens = host_screens(config, job, &main_path, main_media.duration_seconds).await?;
    if screens.is_empty() {
        report.checks.push(rules::Check {
            rule: "Capturas".to_owned(),
            status: rules::Status::Warn,
            message: NO_IMGBB_WARNING.to_owned(),
        });
    }

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
    let mut built = text::build_fields(site, &input);
    built.fields.mediainfo = mediainfo;
    report.checks.extend(built.warnings);
    report.checks.extend(mediainfo_warning);
    store
        .save_launch_report(
            &job.hash,
            &indexer,
            &report_json(&report, &facts, &main.path)?,
        )
        .await?;
    store
        .save_launch_fields(&job.hash, &indexer, &serde_json::to_value(&built.fields)?)
        .await?;

    // 7. Pasta adaptada: a legenda PT só embutida vira `.srt` ao lado do vídeo.
    let adapted = adapt_folder(
        job,
        torrent,
        &files,
        &facts.videos,
        &facts.sniffs,
        &report,
        site,
    )
    .await?;

    // 8. `.torrent`, com o announce do destino, conferido contra a origem (ou
    // contra a pasta adaptada, que é o que ele empacota).
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
    let (path, expected) = match &adapted {
        Some(adapted) => (
            adapted.target.to_string_lossy().into_owned(),
            adapted.layout.clone(),
        ),
        None => (source_path(torrent, &files), origin_layout(&files)),
    };
    let bytes = create_torrent(&client, job, &path, announce, &SHUTDOWN).await?;
    ensure!(
        has_layout(&bytes, &expected),
        "o .torrent gerado não tem os mesmos arquivos da {}; nada foi guardado",
        if adapted.is_some() {
            "pasta adaptada"
        } else {
            "origem"
        }
    );
    store
        .save_launch_torrent(&job.hash, &indexer, &bytes)
        .await?;
    Ok(Finish::Ready)
}

/// As 4 capturas no `ImgBB`, em resolução original (PNG, ou JPEG se o PNG
/// passar de 5 MiB). Sem chave configurada o
/// passo é pulado e a lista sai vazia: o pacote não falha, e a tela entrega os
/// PNG para baixar e anexar no formulário.
///
/// # Errors
///
/// Com chave: sem duração no ffprobe, o ffmpeg ou o upload falhando.
async fn host_screens(
    config: &Config,
    job: &mut Job<'_>,
    video: &Path,
    duration: Option<f64>,
) -> Result<Vec<imgbb::Screen>> {
    let Some(key) = config.imgbb_key().map(str::to_owned) else {
        return Ok(Vec::new());
    };
    job.secrets.push(key.clone());
    let duration = duration
        .filter(|d| d.is_finite() && *d > 0.0)
        .context("o ffprobe não disse a duração do vídeo: sem ela não há onde tirar as capturas")?;
    let imgbb = imgbb::Imgbb::new()?;
    let times = screen_times(duration);
    let mut screens = Vec::new();
    for (index, at) in times.iter().enumerate() {
        let number = index + 1;
        job.step(&format!("captura {number} de {}", times.len()))
            .await;
        // PNG, ou JPEG na mesma resolução se o PNG passar do limite do Amigos.
        let (image, format) = crate::mediainfo::screenshot(video, *at).await?;
        let name = format!("captura-{number}.{}", format.extension());
        let screen = imgbb
            .upload(&key, image, &name, format.mime())
            .await
            .with_context(|| format!("enviando a captura {number}"))?;
        screens.push(screen);
    }
    job.store
        .save_launch_screens(&job.hash, &job.indexer, &serde_json::to_value(&screens)?)
        .await?;
    Ok(screens)
}

/// O relatório do `MediaInfo` do vídeo principal. O formulário do Amigos o exige
/// (e se preenche a partir dele): lá a falha derruba o pacote. O BJ não pede,
/// então a falha vira só um aviso e o campo fica vazio.
async fn mediainfo_for(site: Site, video: &Path) -> Result<(String, Option<rules::Check>)> {
    match crate::mediainfo::mediainfo_report(video).await {
        Ok(report) => Ok((report, None)),
        Err(error) => match site {
            Site::Amigos => Err(error.context("relatório do MediaInfo (o Amigos exige)")),
            Site::Bj => Ok((
                String::new(),
                Some(rules::Check {
                    rule: "MediaInfo".to_owned(),
                    status: rules::Status::Warn,
                    message: format!("sem o relatório do MediaInfo: {error}"),
                }),
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn falha_do_mediainfo_reprova_o_amigos_e_so_avisa_no_bj() {
        // Pasta que não existe: o processo nem sobe, com ou sem MediaInfo no host.
        let video = Path::new("/nao-existe-acervo/Filme.mkv");
        let erro = mediainfo_for(Site::Amigos, video).await.unwrap_err();
        assert!(format!("{erro:#}").contains("MediaInfo"), "{erro:#}");
        let (relatorio, aviso) = mediainfo_for(Site::Bj, video).await.unwrap();
        assert!(relatorio.is_empty());
        let aviso = aviso.expect("aviso");
        assert_eq!(aviso.status, rules::Status::Warn);
        assert!(aviso.message.contains("sem o relatório do MediaInfo"));
    }

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
    fn rota_e_preparo_tiram_o_quadro_no_mesmo_instante() {
        for duration in [6_000.0, 7_265.432, 0.8] {
            let times = screen_times(duration);
            assert_eq!(times.len(), SCREEN_COUNT);
            for (index, at) in times.iter().enumerate() {
                assert_eq!(screen_time(duration, index + 1), Some(*at));
            }
            assert_eq!(screen_time(duration, 0), None);
            assert_eq!(screen_time(duration, SCREEN_COUNT + 1), None);
        }
    }

    #[test]
    fn laudo_guarda_a_resolucao_e_a_duracao_do_video_principal() {
        let report = Report {
            checks: Vec::new(),
            destination: Vec::new(),
            verdict: Verdict::Elegivel,
            adaptations: Vec::new(),
        };
        let videos = vec![ProbedVideo {
            path: "Filme/Filme.mkv".into(),
            media: crate::mediainfo::MediaFacts {
                container: "matroska,webm".into(),
                duration_seconds: Some(7_200.5),
                video: Some(crate::mediainfo::VideoFacts {
                    codec: "hevc".into(),
                    width: 3840,
                    height: 2024,
                    pix_fmt: None,
                    bit_depth: None,
                    hdr: crate::mediainfo::Hdr::Sdr,
                }),
                audio: Vec::new(),
                subtitles: Vec::new(),
            },
        }];
        let value = report_value(&report, &videos, "Filme/Filme.mkv").unwrap();
        assert_eq!(
            value["capturas"],
            json!({ "arquivo": "Filme/Filme.mkv", "largura": 3840, "altura": 2024, "duracao": 7_200.5 })
        );
        assert!(
            report_value(&report, &videos, "Outro.mkv")
                .unwrap()
                .get("capturas")
                .is_none()
        );
    }

    #[tokio::test]
    async fn sem_chave_do_imgbb_o_passo_pula_o_upload_e_deixa_as_capturas_vazias() {
        let Some(db) = acervo_store::testing::TestDb::new("capturas_sem_chave").await else {
            return;
        };
        let hash = "e".repeat(40);
        let mut job = Job {
            store: &db.store,
            hash,
            indexer: "bjsharecustom".into(),
            secrets: Vec::new(),
            shown: String::new(),
            events: Bus::default(),
        };
        // Sem chave não toca no vídeo (que nem existe) nem na duração.
        let screens = host_screens(
            &Config::default(),
            &mut job,
            Path::new("/nao/existe.mkv"),
            None,
        )
        .await
        .unwrap();
        assert!(screens.is_empty());
        assert!(job.secrets.is_empty());
        assert_eq!(job.shown, "", "nenhum andamento de captura");
        db.drop().await;
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
        assert!(has_layout(&same, &origin_layout(&origin)));
        // Pasta raiz renomeada, arquivo a menos ou tamanho diferente: não vale.
        let renamed = torrent("Outro", &[(&["Filme.mkv"], 4_000), (&["Filme.srt"], 10)]);
        assert!(!has_layout(&renamed, &origin_layout(&origin)));
        let missing = torrent("Filme", &[(&["Filme.mkv"], 4_000)]);
        assert!(!has_layout(&missing, &origin_layout(&origin)));
        let bigger = torrent("Filme", &[(&["Filme.mkv"], 4_001), (&["Filme.srt"], 10)]);
        assert!(!has_layout(&bigger, &origin_layout(&origin)));
        // Arquivo avulso.
        let single = torrent("Filme.mkv", &[(&[], 4_000)]);
        assert!(has_layout(
            &single,
            &origin_layout(&[file("Filme.mkv", 4_000)])
        ));
        assert!(!has_layout(
            b"lixo",
            &origin_layout(&[file("Filme.mkv", 4_000)])
        ));
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
            events: Bus::default(),
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
            events: Bus::default(),
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
            events: Bus::default(),
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
            {"taskID": "adaptada", "sourcePath": "/media/lancamentos/bj/Filme",
             "status": "Running"},
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
        for (task, times) in [("velha", 1), ("viva", 0), ("alheia", 0), ("adaptada", 1)] {
            Mock::given(method("POST"))
                .and(path("/api/v2/torrentcreator/deleteTask"))
                .and(body_string_contains(format!("taskID={task}")))
                .respond_with(ResponseTemplate::new(200))
                .expect(times)
                .mount(&server)
                .await;
        }
        // `a` é pacote do acervo parado; `b` ainda está preparando; `c` não é
        // pacote do acervo; a pasta adaptada é de um par parado.
        let packages = HashMap::from([("a".repeat(40), false), ("b".repeat(40), true)]);
        let adapted = HashSet::from(["/media/lancamentos/bj/Filme".to_owned()]);
        assert_eq!(sweep_stale_tasks(&client, &packages, &adapted).await, 2);
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

    /// Um `.mkv` de 2 s com uma legenda `SubRip` em português embutida, gerado
    /// pelo ffmpeg. `None` sem ffmpeg na máquina.
    fn mkv_with_portuguese_subtitle(dir: &Path) -> Option<PathBuf> {
        let ffmpeg = std::env::var("ACERVO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let srt = dir.join("entrada.srt");
        std::fs::write(
            &srt,
            "1\n00:00:00,100 --> 00:00:01,000\nOlá, ação!\n\n2\n00:00:01,200 --> 00:00:01,900\nSegunda fala\n",
        )
        .unwrap();
        let video = dir.join("Filme.mkv");
        let made = std::process::Command::new(ffmpeg)
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=2:size=160x120:rate=10")
            .arg("-i")
            .arg(&srt)
            .args(["-map", "0:v", "-map", "1:s", "-c:v", "mpeg4", "-c:s", "srt"])
            .args(["-metadata:s:s:0", "language=por"])
            .args(["-metadata:s:s:0", "title=Português - Brasil"])
            .arg(&video)
            .status();
        std::fs::remove_file(&srt).unwrap();
        made.is_ok_and(|s| s.success()).then_some(video)
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário inteiro: monta, refaz, recusa.
    async fn pasta_adaptada_e_montada_refeita_e_nunca_apaga_o_alheio() {
        use std::os::unix::fs::MetadataExt;

        let Some(db) = acervo_store::testing::TestDb::new("lancar_pasta_adaptada").await else {
            return;
        };
        let root = std::env::temp_dir().join(format!("acervo-lancar-pasta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let content = root.join("downloads/Filme");
        std::fs::create_dir_all(&content).unwrap();
        let Some(video) = mkv_with_portuguese_subtitle(&content) else {
            let _ = std::fs::remove_dir_all(&root);
            db.drop().await;
            return;
        };
        let size = std::fs::metadata(&video).unwrap().len();
        let files = vec![file("Filme/Filme.mkv", size)];
        let media = crate::mediainfo::probe_full(&video).await.expect("ffprobe");
        let videos = vec![ProbedVideo {
            path: "Filme/Filme.mkv".into(),
            media,
        }];
        let report = Report {
            checks: Vec::new(),
            destination: Vec::new(),
            verdict: Verdict::Elegivel,
            adaptations: adapt::extractions(&files, &videos, &Sniffs::new()),
        };
        assert_eq!(report.adaptations.len(), 1, "a faixa PT vira um .srt");
        // O nome da faixa vai junto ao laudo, para a tela.
        let value = report_value(&report, &videos, "Filme/Filme.mkv").unwrap();
        assert_eq!(value["adaptations"][0]["faixa"], "Português - Brasil");
        assert_eq!(value["adaptations"][0]["output"], "Filme/Filme.pt-BR.srt");

        let origin_torrent = info(
            root.join("downloads").to_str().unwrap(),
            content.to_str().unwrap(),
        );
        let hash = "d".repeat(40);
        let store = &db.store;
        store
            .upsert_launch_state(&hash, "bjsharecustom", LaunchState::Preparando, None, None)
            .await
            .unwrap();
        let mut job = Job {
            store,
            hash: hash.clone(),
            indexer: "bjsharecustom".into(),
            secrets: Vec::new(),
            shown: QUEUED.to_owned(),
            events: Bus::default(),
        };
        let target = root.join("lancamentos/bj/Filme");

        // Monta: hardlink do vídeo (o mesmo inode), .srt extraído, layout gravado.
        let adapted = adapt_folder(
            &mut job,
            &origin_torrent,
            &files,
            &videos,
            &Sniffs::new(),
            &report,
            Site::Bj,
        )
        .await
        .expect("pasta montada")
        .expect("havia o que adaptar");
        assert_eq!(adapted.target, target);
        let (origin, linked) = (
            std::fs::metadata(&video).unwrap(),
            std::fs::metadata(target.join("Filme.mkv")).unwrap(),
        );
        assert_eq!((origin.dev(), origin.ino()), (linked.dev(), linked.ino()));
        let srt = std::fs::read_to_string(target.join("Filme.pt-BR.srt")).unwrap();
        assert!(
            srt.contains("Olá, ação!") && srt.contains("Segunda fala"),
            "{srt}"
        );
        let srt_size = u64::try_from(srt.len()).unwrap();
        assert_eq!(
            adapted.layout,
            vec![
                ("Filme/Filme.mkv".to_owned(), size),
                ("Filme/Filme.pt-BR.srt".to_owned(), srt_size),
            ]
        );
        let launch = store.launch(&hash, "bjsharecustom").await.unwrap().unwrap();
        assert_eq!(launch.adapted_path.as_deref(), target.to_str());
        assert_eq!(adapted_layout(&launch), Some(adapted.layout.clone()));
        // Um .torrent da pasta tem o layout dela, não o da origem.
        let bytes = torrent(
            "Filme",
            &[(&["Filme.mkv"], size), (&["Filme.pt-BR.srt"], srt_size)],
        );
        assert!(has_layout(&bytes, &adapted.layout));
        assert!(!has_layout(&bytes, &origin_layout(&files)));

        // Preparar de novo: a pasta anterior sai pela limpeza e volta igual.
        let again = adapt_folder(
            &mut job,
            &origin_torrent,
            &files,
            &videos,
            &Sniffs::new(),
            &report,
            Site::Bj,
        )
        .await
        .expect("pasta refeita")
        .expect("havia o que adaptar");
        assert_eq!(again, adapted);

        // Arquivo do usuário na pasta: nada é apagado e o preparo falha dizendo.
        std::fs::write(target.join("minhas-notas.txt"), b"x").unwrap();
        let error = adapt_folder(
            &mut job,
            &origin_torrent,
            &files,
            &videos,
            &Sniffs::new(),
            &report,
            Site::Bj,
        )
        .await
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("minhas-notas.txt"), "{message}");
        assert!(message.contains("nada foi apagado"), "{message}");
        for name in ["Filme.mkv", "Filme.pt-BR.srt", "minhas-notas.txt"] {
            assert!(target.join(name).exists(), "{name}");
        }
        std::fs::remove_file(target.join("minhas-notas.txt")).unwrap();

        // Sem adaptação no laudo (ex.: a legenda virou arquivo na origem), a
        // pasta anterior sai e o pacote volta a ser da origem.
        let plain = Report {
            adaptations: Vec::new(),
            ..report.clone()
        };
        let none = adapt_folder(
            &mut job,
            &origin_torrent,
            &files,
            &videos,
            &Sniffs::new(),
            &plain,
            Site::Bj,
        )
        .await
        .unwrap();
        assert!(none.is_none());
        assert!(!target.exists());
        assert!(video.is_file(), "a origem nunca é tocada");
        let launch = store.launch(&hash, "bjsharecustom").await.unwrap().unwrap();
        assert_eq!(launch.adapted_path, None);
        assert_eq!(launch.adapted_layout, None);

        // Pacote já enviado: a pasta é de onde o publicado semeia.
        adapt_folder(
            &mut job,
            &origin_torrent,
            &files,
            &videos,
            &Sniffs::new(),
            &report,
            Site::Bj,
        )
        .await
        .unwrap();
        store
            .mark_launch_sent(&hash, "bjsharecustom", "2026-10-14T10:00:00Z")
            .await
            .unwrap();
        let error = adapt_folder(
            &mut job,
            &origin_torrent,
            &files,
            &videos,
            &Sniffs::new(),
            &report,
            Site::Bj,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("enviado"), "{error:#}");
        assert!(target.join("Filme.mkv").exists());

        let _ = std::fs::remove_dir_all(&root);
        db.drop().await;
    }

    /// O `.torrent` do pacote, com o announce do destino (a passkey é segredo).
    const DEST_ANNOUNCE: &str = "https://destino.exemplo/announce?passkey=PASSKEY123";

    fn package_torrent() -> Vec<u8> {
        format!(
            "d8:announce{}:{DEST_ANNOUNCE}4:infod6:lengthi1e4:name1:a12:piece lengthi16384e6:pieces1:xee",
            DEST_ANNOUNCE.len()
        )
        .into_bytes()
    }

    fn launch_of(indexer: &str) -> Launch {
        Launch {
            hash: "a".repeat(40),
            indexer: indexer.into(),
            state: LaunchState::Pronto,
            progress: None,
            report: json!({}),
            fields: json!({}),
            screens: json!([]),
            has_torrent: true,
            error: None,
            created_at: String::new(),
            updated_at: String::new(),
            sent_at: None,
            adapted_path: None,
            site_hash: None,
            site_id: None,
            adapted_layout: None,
        }
    }

    /// Um qBittorrent de mentira com os trackers do torrent `hash`. Devolve o
    /// servidor, o cliente e o que o torrent `existing` (json) reporta.
    async fn adopt_with(
        existing: serde_json::Value,
        trackers: serde_json::Value,
    ) -> (wiremock::MockServer, QbitClient, TorrentInfo) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/trackers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(trackers))
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        (server, client, serde_json::from_value(existing).unwrap())
    }

    /// Quantas vezes o cliente recebeu `POST path` (e o corpo da primeira).
    async fn posts(server: &wiremock::MockServer, path: &str) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == path)
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect()
    }

    async fn accept_posts(server: &wiremock::MockServer) {
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    fn existing(state: &str, progress: f64, tags: &str) -> serde_json::Value {
        json!({
            "hash": "a".repeat(40), "name": "Raiz", "state": state,
            "save_path": "/downloads", "progress": progress, "tags": tags,
        })
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário por estado do torrent no cliente.
    async fn torrent_que_ja_esta_no_cliente_e_adotado_com_a_tag_o_tracker_e_a_conferencia() {
        let seed = acervo_core::seed_tag("bjsharecustom");
        let origin_only = json!([
            { "url": "** [DHT] **", "status": 0, "msg": "" },
            { "url": "https://origem.exemplo/announce?passkey=OUTRA", "status": 2, "msg": "" },
        ]);

        // Completo e semeando para a origem: ganha a tag e o announce do
        // destino, e continua rodando (nada de parar nem de fila).
        let (server, client, torrent) =
            adopt_with(existing("stalledUP", 1.0, ""), origin_only.clone()).await;
        accept_posts(&server).await;
        adopt_existing(
            &client,
            &torrent,
            &launch_of("bjsharecustom"),
            &package_torrent(),
        )
        .await
        .unwrap();
        let tags = posts(&server, "/api/v2/torrents/addTags").await;
        assert_eq!(tags.len(), 1, "{tags:?}");
        assert!(tags[0].contains(&seed.replace(':', "%3A")), "{tags:?}");
        let added = posts(&server, "/api/v2/torrents/addTrackers").await;
        assert_eq!(added.len(), 1, "{added:?}");
        assert!(added[0].contains("destino.exemplo"), "{added:?}");
        assert!(posts(&server, "/api/v2/torrents/stop").await.is_empty());

        // Já tem o tracker do destino (e a tag): nada a acrescentar.
        let both = json!([
            { "url": "https://origem.exemplo/announce", "status": 2, "msg": "" },
            { "url": "https://DESTINO.exemplo/announce?passkey=OUTRA", "status": 2, "msg": "" },
        ]);
        let (server, client, torrent) = adopt_with(existing("stalledUP", 1.0, &seed), both).await;
        accept_posts(&server).await;
        adopt_existing(
            &client,
            &torrent,
            &launch_of("bjsharecustom"),
            &package_torrent(),
        )
        .await
        .unwrap();
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| { r.method.as_str() == "GET" || r.url.path() == "/api/v2/auth/login" })
        );

        // Incompleto rodando: para e vai à conferência.
        let (server, client, torrent) =
            adopt_with(existing("downloading", 0.4, ""), origin_only.clone()).await;
        accept_posts(&server).await;
        adopt_existing(
            &client,
            &torrent,
            &launch_of("bjsharecustom"),
            &package_torrent(),
        )
        .await
        .unwrap();
        assert_eq!(posts(&server, "/api/v2/torrents/stop").await.len(), 1);
        let tags = posts(&server, "/api/v2/torrents/addTags").await;
        assert!(
            tags.iter().any(|t| t.contains("acervo%3Across-seed")),
            "{tags:?}"
        );

        // Completo mas parado, fora da fila: também vai à conferência, que
        // inicia com 100%; o cliente é que não o inicia aqui.
        let (server, client, torrent) =
            adopt_with(existing("stoppedUP", 1.0, &seed), origin_only.clone()).await;
        accept_posts(&server).await;
        adopt_existing(
            &client,
            &torrent,
            &launch_of("bjsharecustom"),
            &package_torrent(),
        )
        .await
        .unwrap();
        assert_eq!(posts(&server, "/api/v2/torrents/stop").await.len(), 1);
        let tags = posts(&server, "/api/v2/torrents/addTags").await;
        assert_eq!(tags.len(), 1, "só a da fila: {tags:?}");
        assert!(tags[0].contains("acervo%3Across-seed"), "{tags:?}");
        assert!(posts(&server, "/api/v2/torrents/start").await.is_empty());

        // Parado mas já na fila: o `verify` decide, nada é refeito.
        let queued = format!("{seed}, {}", crate::crossseed::PENDING_TAG);
        let (server, client, torrent) =
            adopt_with(existing("stoppedUP", 1.0, &queued), both_trackers()).await;
        accept_posts(&server).await;
        adopt_existing(
            &client,
            &torrent,
            &launch_of("bjsharecustom"),
            &package_torrent(),
        )
        .await
        .unwrap();
        assert!(posts(&server, "/api/v2/torrents/stop").await.is_empty());
        assert!(posts(&server, "/api/v2/torrents/addTags").await.is_empty());

        // O cliente recusando o tracker é erro (o envio avisa), sem a passkey.
        let (server, client, torrent) =
            adopt_with(existing("stalledUP", 1.0, ""), origin_only).await;
        {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, ResponseTemplate};
            Mock::given(method("POST"))
                .and(path("/api/v2/torrents/addTrackers"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;
            accept_posts(&server).await;
        }
        let error = adopt_existing(
            &client,
            &torrent,
            &launch_of("bjsharecustom"),
            &package_torrent(),
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:?}").contains("PASSKEY123"), "{error:?}");
    }

    fn both_trackers() -> serde_json::Value {
        json!([
            { "url": "https://destino.exemplo/announce", "status": 2, "msg": "" },
        ])
    }

    #[test]
    fn status_do_tracker_olha_so_o_host_do_destino() {
        let tracker = |url: &str, status: i64, msg: &str| acervo_clients::Tracker {
            url: url.to_owned(),
            status,
            msg: msg.to_owned(),
        };
        // A origem funciona e o destino não conhece o torrent: o status é o do
        // destino, não o "funcionando" da origem.
        let trackers = [
            tracker("https://origem.exemplo/announce?passkey=A", 2, ""),
            tracker(
                "https://destino.exemplo/announce?passkey=B",
                4,
                "Torrent not registered with this tracker",
            ),
        ];
        assert_eq!(tracker_status_of(&trackers, None), "funcionando");
        assert_eq!(
            tracker_status_of(&trackers, Some("destino.exemplo")),
            "não registrado"
        );
        assert_eq!(
            tracker_status_of(&trackers, Some("origem.exemplo")),
            "funcionando"
        );
        // O destino não está entre os trackers do torrent: sem resposta.
        assert_eq!(
            tracker_status_of(&trackers[..1], Some("destino.exemplo")),
            "sem resposta"
        );
        assert_eq!(tracker_host("não é url"), None);
        assert_eq!(
            tracker_host("https://Destino.Exemplo:8443/a?k=1").as_deref(),
            Some("destino.exemplo")
        );
    }

    #[test]
    fn srt_gerado_so_vale_na_pasta_do_pacote_e_com_o_tamanho_do_layout() {
        let files = [file("Filme/Filme.mkv", 10), file("Filme/Filme.en.srt", 1)];
        let mut launch = Launch {
            hash: "a".into(),
            indexer: "bjsharecustom".into(),
            state: LaunchState::Pronto,
            progress: None,
            report: json!({ "adaptations": [{
                "video": "Filme/Filme.mkv", "stream": 3,
                "output": "Filme/Filme.pt-BR.forced.srt", "forced": true,
            }]}),
            fields: json!({}),
            screens: json!([]),
            has_torrent: false,
            error: None,
            created_at: String::new(),
            updated_at: String::new(),
            sent_at: None,
            adapted_path: Some("/media/lancamentos/bj/Filme".into()),
            site_hash: None,
            site_id: None,
            adapted_layout: Some(json!([
                ["Filme/Filme.mkv", 10],
                ["Filme/Filme.en.srt", 1],
                ["Filme/Filme.pt-BR.srt", 5],
            ])),
        };
        let target = Path::new("/media/lancamentos/bj/Filme");
        let generated = generated_subtitles(&files, Some(&launch), target);
        // Do layout gravado, com o tamanho, menos os arquivos da origem.
        assert_eq!(
            generated,
            HashMap::from([("Filme/Filme.pt-BR.srt".to_owned(), Some(5))])
        );
        // Pasta que não é a registrada pelo pacote: nenhum .srt é do acervo.
        let other = Path::new("/media/lancamentos/bj/Outra");
        assert!(generated_subtitles(&files, Some(&launch), other).is_empty());
        assert!(generated_subtitles(&files, None, target).is_empty());
        // Montagem interrompida (caminho sem layout): vale o que o laudo planejou.
        launch.adapted_layout = None;
        assert_eq!(
            generated_subtitles(&files, Some(&launch), target),
            HashMap::from([("Filme/Filme.pt-BR.forced.srt".to_owned(), None)])
        );
        // Arquivo único: na pasta ele fica dentro da pasta nova.
        let single = [file("Filme.2024.srt", 1)];
        assert_eq!(
            origin_in_folder(&single),
            HashSet::from(["Filme.2024/Filme.2024.srt".to_owned()])
        );
        assert!(may_remove_folder(&launch));
        launch.sent_at = Some("2026-10-14T10:00:00Z".into());
        assert!(!may_remove_folder(&launch));
        launch.sent_at = None;
        launch.state = LaunchState::Semeando;
        assert!(!may_remove_folder(&launch));
        // Outro par enviado com a mesma pasta a segura.
        launch.hash = "b".into();
        let target = Path::new("/media/lancamentos/bj/Filme");
        assert!(folder_in_use(
            std::slice::from_ref(&launch),
            "a",
            "bjsharecustom",
            target
        ));
        assert!(!folder_in_use(
            std::slice::from_ref(&launch),
            "b",
            "bjsharecustom",
            target
        ));
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

    fn pair_of(items: &[(&str, u64)]) -> Vec<(String, u64)> {
        items.iter().map(|(p, s)| ((*p).to_owned(), *s)).collect()
    }

    #[test]
    fn diferenca_de_layout_diz_o_que_falta_sobra_e_muda_de_tamanho() {
        let pacote = pair_of(&[("R/a.mkv", 100), ("R/a.srt", 5), ("R/b.nfo", 1)]);
        let site = pair_of(&[("R/a.mkv", 99), ("R/a.srt", 5), ("R/c.txt", 2)]);
        let texto = layout_diff(&pacote, &site);
        assert_eq!(
            texto,
            "faltam no torrent: R/b.nfo; a mais no torrent: R/c.txt; \
             tamanho diferente: R/a.mkv (pacote 100 B, torrent 99 B)"
        );
        // Lista longa é cortada.
        let muitos: Vec<(String, u64)> = (0..8).map(|n| (format!("R/{n}.mkv"), 1)).collect();
        let texto = layout_diff(&muitos, &[]);
        assert!(texto.ends_with("e mais 3"), "{texto}");
    }

    fn tracker(url: &str, status: i64, msg: &str) -> acervo_clients::Tracker {
        acervo_clients::Tracker {
            url: url.into(),
            status,
            msg: msg.into(),
        }
    }

    #[test]
    fn status_do_tracker_sai_em_texto_sem_url() {
        let dht = tracker("** [DHT] **", 0, "");
        let ok = tracker("https://t.exemplo/announce?passkey=SEGREDO", 2, "");
        let nao = tracker(
            "https://t.exemplo/announce?passkey=SEGREDO",
            4,
            "Torrent not registered with this tracker",
        );
        let erro = tracker("https://t.exemplo/announce?passkey=SEGREDO", 4, "timed out");
        let novo = tracker("https://t.exemplo/announce?passkey=SEGREDO", 1, "");
        assert_eq!(tracker_status_of(&[dht.clone(), ok], None), "funcionando");
        assert_eq!(
            tracker_status_of(&[dht.clone(), nao], None),
            "não registrado"
        );
        assert_eq!(tracker_status_of(&[dht.clone(), erro], None), "erro");
        assert_eq!(
            tracker_status_of(&[dht.clone(), novo], None),
            "sem resposta"
        );
        assert_eq!(tracker_status_of(&[dht], None), "sem resposta");
        assert_eq!(tracker_status_of(&[], None), "sem resposta");
    }
    #[test]
    fn leitura_de_legenda_para_no_limite() {
        let dir = std::env::temp_dir().join(format!("acervo-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.srt");
        std::fs::write(&path, vec![b'a'; 10]).unwrap();
        assert_eq!(read_limited(&path, 10).unwrap().len(), 10);
        assert!(read_limited(&path, 9).is_err());
        assert!(read_limited(&dir.join("falta.srt"), 10).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn pacote_com_torrent_do_mesmo_tracker_no_cliente_adota_em_vez_de_adicionar() {
        use wiremock::matchers::{body_string_contains, method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        // O .torrent do site, baixado e adicionado à mão (hash diferente do
        // gerado), já semeia os mesmos arquivos para o mesmo tracker.
        let hash = "b".repeat(40);
        let info = |hash: &str| {
            json!({
                "hash": hash, "name": "a", "state": "stalledUP", "save_path": "/downloads",
                "progress": 1.0, "tags": "", "content_path": "/downloads/a",
            })
        };
        let torrents: Vec<TorrentInfo> = [info(&"a".repeat(40)), info(&hash)]
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        let (server, client, _) = adopt_with(existing("stalledUP", 1.0, ""), json!([])).await;
        server.reset().await;
        let mock_get = |route: &str, hash: &str, body: serde_json::Value| {
            Mock::given(method("GET"))
                .and(path(format!("/api/v2/torrents/{route}")))
                .and(query_param("hash", hash.to_owned()))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
        };
        mock_get(
            "trackers",
            &"a".repeat(40),
            json!([{ "url": "https://origem.exemplo/announce", "status": 2 }]),
        )
        .mount(&server)
        .await;
        mock_get(
            "trackers",
            &hash,
            json!([{ "url": "https://destino.exemplo/announce?passkey=OUTRA", "status": 2 }]),
        )
        .mount(&server)
        .await;
        mock_get("files", &hash, json!([{ "name": "a", "size": 1 }]))
            .mount(&server)
            .await;
        // Mesmas peças, com `source`: o export prova o conteúdo.
        let exported = "d4:infod6:lengthi1e4:name1:a12:piece lengthi16384e6:pieces1:x6:source1:zee";
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/export"))
            .and(query_param("hash", hash.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(exported.as_bytes().to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/addTags"))
            .and(body_string_contains(format!("hashes={hash}")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let seeded = seed_into(
            &client,
            "acervo",
            &torrents,
            &launch_of("bjsharecustom"),
            package_torrent(),
            &acervo_clients::info_hash(&package_torrent()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(seeded, Seeded::AlreadyThere);
        drop(server);
    }

    #[tokio::test]
    async fn sem_prova_de_conteudo_o_pacote_e_adicionado_e_conferido() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        // Mesmo tracker e mesmos arquivos, mas o cliente não exporta o
        // .torrent do existente: sem prova, não adota.
        let hash = "b".repeat(40);
        let info = |hash: &str| {
            json!({
                "hash": hash, "name": "a", "state": "stalledUP", "save_path": "/downloads",
                "progress": 1.0, "tags": "", "content_path": "/downloads/a",
            })
        };
        let torrents: Vec<TorrentInfo> = [info(&"a".repeat(40)), info(&hash)]
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        let (server, client, _) = adopt_with(existing("stalledUP", 1.0, ""), json!([])).await;
        server.reset().await;
        let own = acervo_clients::info_hash(&package_torrent()).unwrap();
        for (route, hash, body) in [
            (
                "trackers",
                "a".repeat(40),
                json!([{ "url": "https://origem.exemplo/announce", "status": 2 }]),
            ),
            (
                "trackers",
                hash.clone(),
                json!([{ "url": "https://destino.exemplo/announce?passkey=OUTRA", "status": 2 }]),
            ),
            ("files", hash.clone(), json!([{ "name": "a", "size": 1 }])),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/api/v2/torrents/{route}")))
                .and(query_param("hash", hash))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/export"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .and(query_param("hashes", own.clone()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "hash": own, "name": "a", "state": "stoppedDL", "save_path": "/downloads" }
            ])))
            .mount(&server)
            .await;
        accept_posts(&server).await;

        let seeded = seed_into(
            &client,
            "acervo",
            &torrents,
            &launch_of("bjsharecustom"),
            package_torrent(),
            &own,
        )
        .await
        .unwrap();
        assert_eq!(seeded, Seeded::Added);
        assert_eq!(posts(&server, "/api/v2/torrents/add").await.len(), 1);
        assert_eq!(posts(&server, "/api/v2/torrents/recheck").await.len(), 1);
    }
}

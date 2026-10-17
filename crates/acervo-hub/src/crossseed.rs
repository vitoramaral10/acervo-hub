//! Cross-seed: o mesmo conteúdo semeando para mais de um tracker.
//!
//! Um release que existe idêntico, byte a byte, em dois trackers semeia dos
//! mesmos arquivos para os dois sem ocupar disco a mais. Para cada torrent
//! completo do acervo, os outros indexadores são consultados pelo mesmo
//! filme ou episódio; um candidato de tamanho parecido tem o `.torrent`
//! baixado e só vale se a lista de arquivos (caminho e tamanho) for a mesma.
//! Mesmo assim ele entra **parado** e conferido: só começa a semear com
//! 100%. Abaixo disso sai do cliente sem tocar nos arquivos — andando, ele
//! baixaria por cima do arquivo do irmão.
//!
//! O irmão é um torrent comum do acervo, sem grab: a limpeza o trata como
//! os outros, e a remoção com arquivos de um dos dois nunca leva o conteúdo
//! que o outro usa ([`acervo_clients::QbitClient::delete`]).
//!
//! Par lançado pela tela "Para lançar" (o usuário enviou o pacote ao site) é
//! urgente na semana do envio: o torrent publicado é o próprio arquivo do
//! disco, e quem envia semeia (Amigos 4.7, H&R do uploader). Achado, o irmão
//! leva a tag de semear do indexador mesmo sem o modo ratio. Se o pacote foi
//! de uma pasta adaptada (legenda separada para o BJ), o publicado é dela: os
//! candidatos são comparados com o layout dela e o irmão semeia de lá.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use acervo_clients::{
    AddOptions, NewTorrent, QbitClient, QbitError, TorrentInfo, announce, layout, piece_digest,
};
use acervo_core::DownloadHash;
use acervo_indexers::SearchQuery;
use acervo_store::{CrossSeedCheck, CrossSeedOutcome, GrabState, Launch, LaunchState, Store};
use anyhow::Result;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::api::Catalog;
use crate::config::Config;

/// Irmão adicionado e ainda não conferido.
pub(crate) const PENDING_TAG: &str = "acervo:cross-seed";
/// Irmão já mandado conferir: o próximo resultado é o definitivo.
const CHECKING_TAG: &str = "acervo:cross-seed:conferindo";

/// Consultas (torrent × indexador) por volta. Cada uma é uma busca no
/// tracker: devagar, para não parecer robô (o BJ proíbe scraper).
const PER_RUN: usize = 4;
/// Candidatos por consulta que têm o `.torrent` baixado para comparar.
const CANDIDATES: usize = 3;
/// Depois de tantas consultas sem achar, desiste do par.
const MAX_ATTEMPTS: i32 = 3;
/// Dias entre consultas sem achar, multiplicado pela tentativa: 2, 4.
const RETRY_DAYS: i64 = 2;
/// Horas até reconsultar um par cuja última consulta deu erro. O erro não
/// gasta tentativa (ver `record_cross_seed_check`): volta logo, sem a
/// espera crescente de quem buscou e não achou.
const ERROR_RETRY_HOURS: i64 = 12;
/// Tempo que o irmão recém-adicionado tem para aparecer e começar a
/// conferência antes de o resultado contar.
const SETTLE_SECS: i64 = 10 * 60;
/// Dias depois do envio em que o par lançado é urgente: o site costuma
/// publicar em horas, e o H&R do uploader corre desde o envio.
const URGENT_DAYS: i64 = 7;
/// Espera entre consultas de um par urgente. Uma busca por hora e por par
/// fica longe de parecer robô, e o achado não demora um dia.
const URGENT_WAIT_MINUTES: i64 = 60;

/// Teto de releases lembrados como "layout diferente": passando dele a
/// memória recomeça, em vez de crescer sem limite.
const DIFFERING_CAP: usize = 2048;

/// Motivo gravado no pacote descartado quando o release aparece no destino.
const ALREADY_THERE: &str = "já existe no destino: outro uploader publicou o mesmo release";

/// De onde veio um torrent do acervo: o que buscar no outro tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    Movie(i64),
    Series(i64),
}

/// Um torrent completo do acervo que pode ter irmão em outro tracker.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    /// Infohash, em minúsculas.
    pub(crate) hash: String,
    pub(crate) name: String,
    pub(crate) save_path: String,
    /// Indexador de onde veio (o do grab).
    pub(crate) indexer: String,
    pub(crate) origin: Origin,
}

/// O que fazer com um irmão que espera conferência.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Ainda conferindo, ou entrou há pouco.
    Wait,
    /// Ainda não foi mandado conferir.
    Recheck,
    /// Conferido com 100%: começa a semear.
    Start,
    /// Conferido abaixo de 100%: não é o mesmo conteúdo.
    Reject,
}

fn verdict(torrent: &TorrentInfo, now: i64) -> Verdict {
    let state = torrent.state.as_str();
    // Erro de disco não é resultado de conferência: nem inicia (o cliente
    // recarregaria os arquivos) nem recusa. Espera o estado voltar.
    if state.starts_with("checking") || matches!(state, "moving" | "error" | "missingFiles") {
        return Verdict::Wait;
    }
    if !torrent.has_tag(CHECKING_TAG) {
        return Verdict::Recheck;
    }
    let stopped = state.starts_with("stopped") || state.starts_with("paused");
    if torrent.progress >= 1.0 {
        // Parado e completo, ou já semeando (o início saiu e a tag ficou).
        return if stopped || state.ends_with("UP") {
            Verdict::Start
        } else {
            Verdict::Wait
        };
    }
    // Incompleto andando estaria baixando por cima do arquivo do irmão.
    if !stopped {
        return Verdict::Reject;
    }
    // A conferência pode ainda não ter começado logo depois do pedido.
    if torrent.added_on > 0 && now - torrent.added_on < SETTLE_SECS {
        return Verdict::Wait;
    }
    Verdict::Reject
}

/// Mesmo conteúdo no disco: os mesmos caminhos com os mesmos tamanhos.
pub(crate) fn same_layout(a: &[(String, u64)], b: &[(String, u64)]) -> bool {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    !a.is_empty() && a == b
}

/// O tamanho que o indexador mostra é arredondado ("5,87 GB"): conta como
/// parecido dentro de 1% ou 50 MiB, o que for maior.
fn close_in_size(release: u64, total: u64) -> bool {
    let tolerance = (total / 100).max(50 << 20);
    release.abs_diff(total) <= tolerance
}

/// Pares (hash de origem, indexador) de um conjunto de lançamentos.
pub(crate) type Pairs = HashSet<(String, String)>;

/// O usuário enviou o pacote ao site (`sent_at`) e o par ainda não semeia
/// nem foi abandonado. Vale o `sent_at`, não só o estado `enviado`: preparar
/// de novo depois do envio troca o estado, e o publicado no site continua
/// sendo o arquivo do disco.
pub(crate) fn sent_to_site(launch: &Launch) -> bool {
    launch.sent_at.is_some()
        && !matches!(
            launch.state,
            LaunchState::Semeando | LaunchState::Descartado
        )
}

/// Os pares lançados: o usuário marcou o pacote como enviado ao site. É o
/// que passa a `semeando` quando o irmão aparece.
fn launched(launches: &[Launch]) -> Pairs {
    launches
        .iter()
        .filter(|l| sent_to_site(l))
        .map(|l| (l.hash.to_ascii_lowercase(), l.indexer.clone()))
        .collect()
}

/// O par lançado de que `sibling` é o irmão achado. O irmão só vale como
/// publicado do lançamento depois da conferência: até lá o par segue
/// `enviado` (e urgente), e a recusa não deixa estado falso para trás.
fn launched_pair_of(
    checks: &[CrossSeedCheck],
    launched: &Pairs,
    sibling: &str,
) -> Option<(String, String)> {
    checks
        .iter()
        .filter(|c| {
            c.found
                .as_deref()
                .is_some_and(|f| f.eq_ignore_ascii_case(sibling))
        })
        .map(|c| (c.hash.to_ascii_lowercase(), c.indexer.clone()))
        .find(|pair| launched.contains(pair))
}

/// O que o publicado de um par lançado tem no disco: o layout e a pasta onde o
/// irmão é adicionado (`save_path`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Published {
    layout: Vec<(String, u64)>,
    save_path: String,
}

/// Os pares lançados de uma pasta adaptada: o publicado é ela, não a origem.
/// O irmão vai para o pai da pasta, onde o nome dela é a raiz do torrent.
fn adapted_published(launches: &[Launch]) -> HashMap<(String, String), Published> {
    launches
        .iter()
        .filter(|l| sent_to_site(l))
        .filter_map(|l| {
            let layout = crate::launch::adapted_layout(l)?;
            let parent = std::path::Path::new(l.adapted_path.as_deref()?).parent()?;
            Some((
                (l.hash.to_ascii_lowercase(), l.indexer.clone()),
                Published {
                    layout,
                    save_path: parent.to_string_lossy().into_owned(),
                },
            ))
        })
        .collect()
}

/// O que comparar com os candidatos e onde pôr o irmão: a pasta adaptada do
/// par lançado, se houver, senão os arquivos e a pasta da origem.
fn published(source: &Source, files: Vec<(String, u64)>, adapted: Option<&Published>) -> Published {
    adapted.cloned().unwrap_or_else(|| Published {
        layout: files,
        save_path: source.save_path.clone(),
    })
}

/// Releases do destino cujo `.torrent` já foi baixado e tem layout diferente
/// do pacote: (infohash do par, indexador, guid do release). O par urgente
/// é consultado de hora em hora por uma semana, e sem esta memória cada
/// volta baixaria de novo os mesmos `.torrent` do tracker privado. Vive só no
/// processo: depois de um reinício a dúvida é refeita uma vez.
#[derive(Default)]
struct Differing(Mutex<HashSet<(String, String, String)>>);

impl Differing {
    fn key(source: &str, indexer: &str, guid: &str) -> (String, String, String) {
        (source.to_owned(), indexer.to_owned(), guid.to_owned())
    }

    fn knows(&self, source: &str, indexer: &str, guid: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&Self::key(source, indexer, guid))
    }

    fn remember(&self, source: &str, indexer: &str, guid: &str) {
        let mut known = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if known.len() >= DIFFERING_CAP {
            known.clear();
        }
        known.insert(Self::key(source, indexer, guid));
    }
}

static DIFFERING: LazyLock<Differing> = LazyLock::new(Differing::default);

/// Estados de semeadura ativa do qBittorrent.
const SEEDING_STATES: [&str; 5] = [
    "uploading",
    "stalledUP",
    "queuedUP",
    "forcedUP",
    "checkingUP",
];

/// Em que pé está um irmão que já estava no cliente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sibling {
    /// Completo e semeando.
    Seeding,
    /// Completo e parado: basta iniciar.
    Stopped,
    /// Incompleto: não prova ser o mesmo conteúdo, vai à conferência.
    Incomplete,
    /// Completo mas em outro estado (mudando de lugar, erro de disco).
    Busy,
}

fn sibling(torrent: &TorrentInfo) -> Sibling {
    let state = torrent.state.as_str();
    if torrent.progress < 1.0 {
        Sibling::Incomplete
    } else if SEEDING_STATES.contains(&state) {
        Sibling::Seeding
    } else if state.starts_with("stopped") || state.starts_with("paused") {
        Sibling::Stopped
    } else {
        Sibling::Busy
    }
}

/// O irmão achado já no cliente (hash igual): põe nele a tag de semear do
/// indexador e devolve se já semeia. Parado e completo: inicia antes de
/// contar. Incompleto: para, entra na fila de conferência (`PENDING_TAG`:
/// recheck, e só inicia com 100%). Em qualquer outro estado só espera.
async fn adopt(client: &QbitClient, torrent: &TorrentInfo, seed: &str) -> Result<bool> {
    let hash = torrent.hash.as_str();
    if torrent.has_tag(PENDING_TAG) {
        // Já na fila de conferência: `verify` decide.
        client.add_tag(&[hash], seed).await?;
        return Ok(false);
    }
    match sibling(torrent) {
        Sibling::Seeding => {
            client.add_tag(&[hash], seed).await?;
            Ok(true)
        }
        Sibling::Stopped => {
            client.start(&[hash]).await?;
            client.add_tag(&[hash], seed).await?;
            Ok(true)
        }
        Sibling::Incomplete => {
            // Parado antes de tudo: nada de baixar por cima do irmão.
            client.stop(&[hash]).await?;
            client.add_tag(&[hash], PENDING_TAG).await?;
            client.add_tag(&[hash], seed).await?;
            tracing::info!(
                torrent = torrent.name,
                "cross-seed: irmão já no cliente, incompleto; indo à conferência"
            );
            Ok(false)
        }
        Sibling::Busy => {
            client.add_tag(&[hash], seed).await?;
            Ok(false)
        }
    }
}

/// Tentativas de achar o irmão recém-adicionado para mandar conferir: o
/// qBittorrent 5 responde ao `add` antes de listar o torrent.
const RECHECK_TRIES: usize = 5;
/// Espera entre elas.
const RECHECK_WAIT: Duration = Duration::from_secs(1);

/// Adiciona o irmão e já manda conferir, em vez de esperar a próxima volta do
/// `verify`: o torrent entra parado e a conferência é o que decide se ele
/// semeia. Se o cliente ainda não o lista depois das tentativas, fica para o
/// `verify` (sem a tag de conferência, ele pede o recheck).
///
/// # Errors
///
/// A adição falhou. Falha do recheck não é erro: o irmão já está no cliente.
pub(crate) async fn add_sibling(
    client: &QbitClient,
    torrent: NewTorrent,
    options: &AddOptions,
    hash: &str,
) -> Result<(), QbitError> {
    client.add(torrent, options).await?;
    if !recheck_now(client, hash, RECHECK_TRIES, RECHECK_WAIT).await {
        tracing::info!(
            "cross-seed: irmão ainda fora da lista do cliente; a conferência fica para o verify"
        );
    }
    Ok(())
}

/// Manda conferir `hash` assim que o cliente o lista e marca [`CHECKING_TAG`].
/// Devolve se conseguiu (ou se o `verify` já o tinha mandado).
///
/// Sob o mesmo [`VERIFY_LOCK`] do `verify`, com a tag antes do recheck e o
/// torrent relido já dentro da trava: um `verify` concorrente que o viu sem a
/// tag já mandou conferir, e um que vem depois já a encontra. Nenhum chamador
/// segura a trava aqui (só `verify` a toma, e ele não adiciona irmãos).
async fn recheck_now(client: &QbitClient, hash: &str, tries: usize, wait: Duration) -> bool {
    for attempt in 0..tries {
        if attempt > 0 {
            tokio::time::sleep(wait).await;
        }
        let _turn = VERIFY_LOCK.lock().await;
        match client.torrent(hash).await {
            Ok(Some(found)) => {
                if found.has_tag(CHECKING_TAG) {
                    return true;
                }
                let sent = async {
                    client.add_tag(&[hash], CHECKING_TAG).await?;
                    if let Err(error) = client.recheck(&[hash]).await {
                        // Sem recheck a tag mentiria: o `verify` esperaria um
                        // resultado que não vem e recusaria o irmão.
                        let _ = client.remove_tag(&[hash], CHECKING_TAG).await;
                        return Err(error);
                    }
                    Ok::<(), QbitError>(())
                }
                .await;
                return match sent {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!("cross-seed: conferência imediata falhou: {error}");
                        false
                    }
                };
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("cross-seed: procurando o irmão adicionado: {error}"),
        }
    }
    false
}

/// Caminho com `/` como separador e sem barra no fim.
fn normalized_path(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_owned()
}

/// Um torrent do cliente que já semeia o conteúdo de `candidate` (um
/// `.torrent` de outro hash) **para o mesmo tracker**: duplicá-lo no mesmo
/// tracker nunca registraria. Vale o que tem o mesmo `content_path` do
/// pacote (`save_path` + raiz do layout, o atalho barato), um tracker real
/// com o mesmo host, porta e primeiro segmento do announce do candidato
/// ([`crate::launch::tracker_key`]), os mesmos arquivos **e** a mesma prova de
/// conteúdo: o `.torrent` exportado pelo cliente tem o mesmo tamanho de peça
/// e os mesmos hashes de peça (só `source` e afins podem diferir). Só esses
/// pedem trackers, arquivos e export ao cliente. Sem a prova (export
/// recusado, cliente antigo, peças diferentes) não adota. Nada do announce
/// (passkey) é logado.
///
/// # Errors
///
/// O cliente falhou ao listar trackers ou arquivos: na dúvida não adiciona.
pub(crate) async fn same_tracker_sibling<'a>(
    client: &QbitClient,
    torrents: impl IntoIterator<Item = &'a TorrentInfo>,
    save_path: &str,
    candidate: &[u8],
) -> Result<Option<TorrentInfo>> {
    let (Some(key), Some(files), Some(digest)) = (
        announce(candidate).and_then(|url| crate::launch::tracker_key(&url)),
        layout(candidate),
        piece_digest(candidate),
    ) else {
        return Ok(None);
    };
    let Some(root) = files.first().and_then(|(path, _)| path.split('/').next()) else {
        return Ok(None);
    };
    let content = normalized_path(&format!("{}/{root}", save_path.trim_end_matches('/')));
    let own = acervo_clients::info_hash(candidate);
    for existing in torrents {
        if existing.content_path.is_empty()
            || normalized_path(&existing.content_path) != content
            || own
                .as_deref()
                .is_some_and(|hash| hash.eq_ignore_ascii_case(&existing.hash))
        {
            continue;
        }
        let trackers = client.trackers(&existing.hash).await?;
        if !trackers
            .iter()
            .filter(|t| t.is_real())
            .any(|t| crate::launch::tracker_key(&t.url).as_deref() == Some(key.as_str()))
        {
            continue;
        }
        let there = client
            .files(&DownloadHash::new(existing.hash.clone()))
            .await?;
        let there: Vec<(String, u64)> = there.into_iter().map(|f| (f.name, f.size)).collect();
        if !same_layout(&files, &there) {
            continue;
        }
        // Mesmos arquivos não provam o mesmo conteúdo: só as peças provam.
        match client.export_torrent(&existing.hash).await {
            Ok(exported) if piece_digest(&exported).as_ref() == Some(&digest) => {
                return Ok(Some(existing.clone()));
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(
                "cross-seed: sem o .torrent do irmão para provar o conteúdo; não adota: {error}"
            ),
        }
    }
    Ok(None)
}

/// O pacote do par que ainda não foi ao site (pronto, falhou ou inelegível):
/// o que perde o sentido quando o release aparece no destino.
fn unsent_package<'a>(launches: &'a [Launch], hash: &str, indexer: &str) -> Option<&'a Launch> {
    launches
        .iter()
        .find(|l| l.hash.eq_ignore_ascii_case(hash) && l.indexer == indexer)
        .filter(|l| {
            l.sent_at.is_none()
                && matches!(
                    l.state,
                    LaunchState::Pronto | LaunchState::Falhou | LaunchState::Inelegivel
                )
        })
}

/// Os pares urgentes: enviados há menos de [`URGENT_DAYS`] dias.
fn urgent(launches: &[Launch], now: OffsetDateTime) -> Pairs {
    launches
        .iter()
        .filter(|l| sent_to_site(l))
        .filter(|l| {
            l.sent_at
                .as_deref()
                .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok())
                .is_some_and(|at| now - at < time::Duration::days(URGENT_DAYS))
        })
        .map(|l| (l.hash.to_ascii_lowercase(), l.indexer.clone()))
        .collect()
}

/// Os pares (torrent, indexador) que já podem ser consultados, na ordem:
/// urgentes primeiro, depois os nunca consultados, depois os mais antigos.
/// Fica de fora o indexador de onde o torrent veio, o par que já achou
/// irmão e o que esgotou as tentativas ou ainda está no intervalo entre
/// elas. O par urgente não esgota: espera só [`URGENT_WAIT_MINUTES`]. O
/// lançado passada a urgência também não: as consultas da semana urgente
/// gastaram as tentativas, e o site pode publicar depois (moderação), então
/// ele segue sendo procurado a cada [`RETRY_DAYS`] dias.
fn due<'a>(
    sources: &'a [Source],
    indexers: &'a [String],
    checks: &[CrossSeedCheck],
    launched: &Pairs,
    urgent: &Pairs,
    now: OffsetDateTime,
) -> Vec<(&'a Source, &'a str)> {
    let by_pair: HashMap<(&str, &str), &CrossSeedCheck> = checks
        .iter()
        .map(|c| ((c.hash.as_str(), c.indexer.as_str()), c))
        .collect();
    let mut pairs: Vec<(bool, Option<OffsetDateTime>, &Source, &str)> = Vec::new();
    for source in sources {
        for indexer in indexers {
            if *indexer == source.indexer {
                continue;
            }
            let pair = (source.hash.clone(), indexer.clone());
            let hurry = urgent.contains(&pair);
            let sent = launched.contains(&pair);
            let Some(check) = by_pair.get(&(source.hash.as_str(), indexer.as_str())) else {
                pairs.push((hurry, None, source, indexer));
                continue;
            };
            let failed = check.outcome == CrossSeedOutcome::Error;
            if check.found.is_some() || (!sent && !failed && check.attempts >= MAX_ATTEMPTS) {
                continue;
            }
            let Ok(at) = OffsetDateTime::parse(&check.checked_at, &Rfc3339) else {
                continue;
            };
            let wait = if hurry {
                time::Duration::minutes(URGENT_WAIT_MINUTES)
            } else if failed {
                time::Duration::hours(ERROR_RETRY_HOURS)
            } else if sent {
                time::Duration::days(RETRY_DAYS)
            } else {
                time::Duration::days(RETRY_DAYS * i64::from(check.attempts))
            };
            if now - at >= wait {
                pairs.push((hurry, Some(at), source, indexer));
            }
        }
    }
    pairs.sort_by_key(|(hurry, at, source, _)| (!*hurry, *at, source.hash.clone()));
    pairs
        .into_iter()
        .map(|(_, _, source, indexer)| (source, indexer))
        .collect()
}

/// Os torrents completos do acervo com grab (de onde se sabe o que são),
/// fora os próprios irmãos de cross-seed.
pub(crate) fn sources(
    torrents: &[TorrentInfo],
    category: &str,
    owners: &HashMap<String, (String, Origin)>,
) -> Vec<Source> {
    torrents
        .iter()
        .filter(|t| t.category == category && t.progress >= 1.0)
        .filter(|t| !t.has_tag(PENDING_TAG))
        .filter_map(|t| {
            let (indexer, origin) = owners.get(&t.hash.to_ascii_lowercase())?;
            Some(Source {
                hash: t.hash.to_ascii_lowercase(),
                name: t.name.clone(),
                save_path: t.save_path.clone(),
                indexer: indexer.clone(),
                origin: origin.clone(),
            })
        })
        .collect()
}

/// Quem pegou cada torrent: indexador e o filme ou a série. Grab falho não
/// conta: o torrent dele não é o que está no disco.
pub(crate) async fn owners(store: &Store) -> Result<HashMap<String, (String, Origin)>> {
    let mut out = HashMap::new();
    for grab in store.grabs().await? {
        if grab.state != GrabState::Failed {
            out.insert(
                grab.hash.to_ascii_lowercase(),
                (grab.indexer, Origin::Movie(grab.movie_id)),
            );
        }
    }
    for grab in store.series_grabs().await? {
        if grab.state != GrabState::Failed {
            out.insert(
                grab.hash.to_ascii_lowercase(),
                (grab.indexer, Origin::Series(grab.series_id)),
            );
        }
    }
    Ok(out)
}

/// A busca do mesmo conteúdo noutro indexador: o filme pelos ids, ou a
/// temporada ou episódio que o nome do torrent diz.
pub(crate) async fn query(store: &Store, source: &Source) -> Result<Option<SearchQuery>> {
    match source.origin {
        Origin::Movie(id) => {
            let Some(entry) = store.movies().await?.into_iter().find(|m| m.id == id) else {
                return Ok(None);
            };
            let movie = entry.movie;
            let mut query =
                SearchQuery::movie(movie.title.clone()).with_categories([crate::decide::MOVIES]);
            if let Some(year) = movie.year {
                query = query.with_year(year);
            }
            if let Some(imdb) = movie.imdb_id {
                query = query.with_imdb_id(imdb);
            }
            if movie.tmdb_id != 0 {
                query = query.with_tmdb_id(u64::from(movie.tmdb_id));
            }
            Ok(Some(query))
        }
        Origin::Series(id) => {
            let Some(entry) = store.series(id).await? else {
                return Ok(None);
            };
            let Some(parsed) = acervo_parser::parse_episode_title(&source.name) else {
                return Ok(None);
            };
            let mut query = SearchQuery::tv(crate::series::folder_title(&entry.series))
                .with_categories([crate::series::TV]);
            query = match parsed.episodes.as_slice() {
                [episode] if !parsed.full_season => {
                    query.with_episode(parsed.season, episode.to_string())
                }
                _ => query.with_episode(parsed.season, ""),
            };
            if let Some(tvdb) = entry.series.tvdb_id {
                query = query.with_tvdb_id(u64::from(tvdb));
            }
            Ok(Some(query))
        }
    }
}

/// Põe o irmão achado no cliente. Devolve o hash a gravar como achado e se
/// já semeia.
///
/// Mesmo hash já no cliente, ou outro hash **do mesmo tracker** com os mesmos
/// arquivos (o `.torrent` que o usuário baixou do site e adicionou à mão):
/// não adiciona, porque um irmão a mais seria duplicata que o tracker não
/// registra. Adota o que existe e grava o hash dele. Senão adiciona parado e
/// já manda conferir ([`add_sibling`]).
#[allow(clippy::too_many_arguments)]
async fn place(
    config: &Config,
    store: &Store,
    client: &QbitClient,
    present: &HashMap<String, TorrentInfo>,
    source: &Source,
    indexer: &str,
    launched: bool,
    target: &Published,
    bytes: Vec<u8>,
    hash: String,
) -> Result<(String, bool)> {
    if let Some(existing) = present.get(&hash) {
        let seeding = launched && adopt(client, existing, &acervo_core::seed_tag(indexer)).await?;
        return Ok((hash, seeding));
    }
    if let Some(existing) =
        same_tracker_sibling(client, present.values(), &target.save_path, &bytes).await?
    {
        let seed = if launched {
            Some(acervo_core::seed_tag(indexer))
        } else {
            crate::grab::seed_tag(store, indexer).await
        };
        let seeding = match seed {
            Some(seed) => adopt(client, &existing, &seed).await? && launched,
            None => false,
        };
        tracing::info!(
            torrent = source.name,
            indexer,
            "cross-seed: o tracker já tem torrent com estes arquivos no cliente; adotado"
        );
        return Ok((existing.hash.to_ascii_lowercase(), seeding));
    }
    let mut tags = vec![PENDING_TAG.to_owned()];
    tags.extend(crate::grab::seed_tag(store, indexer).await);
    let seed = acervo_core::seed_tag(indexer);
    if launched && !tags.contains(&seed) {
        tags.push(seed);
    }
    let options = AddOptions {
        category: config.library.category.clone(),
        save_path: Some(target.save_path.clone()),
        stopped: true,
        stop_after_metadata: false,
        tags,
    };
    add_sibling(client, NewTorrent::File(bytes), &options, &hash).await?;
    tracing::info!(
        torrent = source.name,
        indexer,
        "cross-seed: irmão adicionado, conferindo"
    );
    Ok((hash, false))
}

/// Procura o irmão de `source` em `indexer` e, achando, o adiciona parado
/// para conferir. Devolve o desfecho da consulta e, achando, o infohash do
/// irmão.
///
/// `launched`: o par foi enviado pelo usuário, que tem de semear o que
/// publicou; o irmão leva a tag de semear do indexador mesmo com o modo
/// ratio desligado. `adapted`: o publicado é a pasta adaptada do par.
#[allow(clippy::too_many_arguments)]
async fn find(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    client: &QbitClient,
    present: &HashMap<String, TorrentInfo>,
    source: &Source,
    indexer: &str,
    launched: bool,
    adapted: Option<&Published>,
) -> Result<(CrossSeedOutcome, Option<String>, bool)> {
    let Some(query) = query(store, source).await? else {
        return Ok((CrossSeedOutcome::Skipped, None, false));
    };
    let files = client
        .files(&DownloadHash::new(source.hash.clone()))
        .await?;
    // Pacote baixado em parte (episódios com prioridade zero): o irmão
    // precisaria de arquivos que não estão no disco, e nunca conferiria.
    if files.iter().any(|f| f.priority == 0 || f.progress < 1.0) {
        return Ok((CrossSeedOutcome::Skipped, None, false));
    }
    let files: Vec<(String, u64)> = files.into_iter().map(|f| (f.name, f.size)).collect();
    let target = published(source, files, adapted);
    let files = &target.layout;
    // A memória de "layout diferente" é por layout comparado: o veredito
    // contra a origem não vale para a pasta adaptada (o publicado dela teria
    // sido lembrado como diferente antes de o usuário marcar o envio).
    let memory = if adapted.is_some() {
        format!("{}#adaptada", source.hash)
    } else {
        source.hash.clone()
    };
    let total: u64 = files.iter().map(|(_, size)| size).sum();
    let mut releases: Vec<acervo_indexers::Release> = catalog
        .search(indexer, &query)
        .await
        .map_err(|error| anyhow::anyhow!("buscando em {indexer}: {error}"))?
        .releases
        .into_iter()
        .filter(|r| close_in_size(r.size, total))
        .collect();
    releases.sort_by_key(|r| r.size.abs_diff(total));
    // O que impediu de concluir "não existe": layout diferente é um achado
    // (provável duplicado); download que falhou deixa a dúvida.
    let (mut layout_differs, mut failed) = (false, false);
    for release in releases.into_iter().take(CANDIDATES) {
        if DIFFERING.knows(&memory, indexer, &release.guid) {
            layout_differs = true;
            continue;
        }
        let (torrent, hash) = match crate::grab::fetch_torrent(catalog, &release).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(indexer, release = release.title, "cross-seed: {error:#}");
                failed = true;
                continue;
            }
        };
        let NewTorrent::File(bytes) = &torrent else {
            continue;
        };
        if !layout(bytes).is_some_and(|other| same_layout(files, &other)) {
            layout_differs = true;
            DIFFERING.remember(&memory, indexer, &release.guid);
            continue;
        }
        let NewTorrent::File(bytes) = torrent else {
            continue;
        };
        let (hash, seeding) = place(
            config, store, client, present, source, indexer, launched, &target, bytes, hash,
        )
        .await?;
        return Ok((CrossSeedOutcome::Found, Some(hash), seeding));
    }
    let outcome = if layout_differs {
        CrossSeedOutcome::LayoutDiffers
    } else if failed {
        CrossSeedOutcome::Error
    } else {
        CrossSeedOutcome::NotFound
    };
    Ok((outcome, None, false))
}

static VERIFY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Confere os irmãos pendentes: manda conferir, inicia o que deu 100% e
/// tira do cliente, sem os arquivos, o que não deu. Devolve (iniciados,
/// recusados).
///
/// Lê o cliente sob uma trava: a volta do cross-seed e a importação o chamam
/// ao mesmo tempo, e cada uma agiria sobre uma lista já velha da outra.
async fn verify(store: &Store, client: &QbitClient, launched: &Pairs) -> Result<(usize, usize)> {
    let _turn = VERIFY_LOCK.lock().await;
    let torrents = client.torrents().await?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let (mut started, mut rejected) = (0, 0);
    for torrent in torrents.iter().filter(|t| t.has_tag(PENDING_TAG)) {
        let hash = torrent.hash.as_str();
        match verdict(torrent, now) {
            Verdict::Wait => {}
            Verdict::Recheck => {
                client.recheck(&[hash]).await?;
                client.add_tag(&[hash], CHECKING_TAG).await?;
            }
            Verdict::Start => {
                // Inicia antes de tirar as tags: falha no meio volta aqui na
                // próxima volta, e iniciar de novo não muda nada.
                client.start(&[hash]).await?;
                client.remove_tag(&[hash], CHECKING_TAG).await?;
                client.remove_tag(&[hash], PENDING_TAG).await?;
                tracing::info!(torrent = torrent.name, "cross-seed: conferido, semeando");
                started += 1;
                // O lançamento só passa a `semeando` aqui, com o irmão
                // conferido: adicionado ainda não é semeando.
                let checks = store.cross_seed_checks().await?;
                if let Some((source, indexer)) = launched_pair_of(&checks, launched, hash) {
                    store
                        .set_launch_state(&source, &indexer, LaunchState::Semeando)
                        .await?;
                    tracing::info!(
                        torrent = torrent.name,
                        indexer,
                        "cross-seed: o publicado do lançamento foi conferido; semeando"
                    );
                }
            }
            Verdict::Reject => {
                client
                    .delete(&[DownloadHash::new(torrent.hash.clone())], false)
                    .await?;
                // O par volta a ser procurado: outro candidato pode servir.
                store.forget_cross_seed(&torrent.hash).await?;
                tracing::warn!(
                    torrent = torrent.name,
                    progresso = torrent.progress,
                    "cross-seed: conferência não fechou; irmão removido sem tocar nos arquivos"
                );
                rejected += 1;
            }
        }
    }
    Ok((started, rejected))
}

/// Só a parte de conferir e iniciar os irmãos pendentes, sem buscas: roda
/// também na importação, para o irmão adicionado não esperar a próxima volta
/// do cross-seed. Devolve (iniciados, recusados).
///
/// # Errors
///
/// Cliente ou banco inalcançável.
pub(crate) async fn verify_pending(config: &Config, store: &Store) -> Result<(usize, usize)> {
    let client = crate::grab::qbit(config).await?;
    let launches = store.launches().await?;
    verify(store, &client, &launched(&launches)).await
}

/// Uma volta: confere os pendentes e consulta os próximos pares.
///
/// # Errors
///
/// Cliente ou banco inalcançável. Falha de um indexador só conta como
/// consulta sem achar.
pub(crate) async fn run(config: &Config, store: &Store, catalog: &Catalog) -> Result<String> {
    let client = crate::grab::qbit(config).await?;
    let launches = store.launches().await?;
    let sent = launched(&launches);
    let (started, rejected) = verify(store, &client, &sent).await?;
    // Depois do verify: a lista já reflete o que ele iniciou ou removeu.
    let torrents = client.torrents().await?;

    let indexers: Vec<String> = store
        .indexers()
        .await?
        .into_iter()
        .filter(|r| r.enabled)
        .map(|r| r.name)
        .collect();
    let owners = owners(store).await?;
    let sources = sources(&torrents, &config.library.category, &owners);
    let checks = store.cross_seed_checks().await?;
    let now = OffsetDateTime::now_utc();
    let urgent = urgent(&launches, now);
    let adapted = adapted_published(&launches);
    let present: HashMap<String, TorrentInfo> = torrents
        .iter()
        .map(|t| (t.hash.to_ascii_lowercase(), t.clone()))
        .collect();
    let (mut added, mut consulted) = (0, 0);
    for (source, indexer) in due(&sources, &indexers, &checks, &sent, &urgent, now)
        .into_iter()
        .take(PER_RUN)
    {
        consulted += 1;
        let pair = (source.hash.clone(), indexer.to_owned());
        let is_launched = sent.contains(&pair);
        let (outcome, found, seeding) = match find(
            config,
            store,
            catalog,
            &client,
            &present,
            source,
            indexer,
            is_launched,
            adapted.get(&pair),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(torrent = source.name, indexer, "cross-seed: {error:#}");
                (CrossSeedOutcome::Error, None, false)
            }
        };
        added += usize::from(found.is_some());
        let at = now.format(&Rfc3339)?;
        store
            .record_cross_seed_check(&source.hash, indexer, &at, found.as_deref(), outcome)
            .await?;
        if !is_launched && found.is_some() {
            // Pacote ainda não enviado e o publicado de outro uploader já
            // está no destino: enviar seria duplicar.
            if let Some(launch) = unsent_package(&launches, &source.hash, indexer) {
                store
                    .upsert_launch_state(
                        &launch.hash,
                        indexer,
                        LaunchState::Descartado,
                        None,
                        Some(ALREADY_THERE),
                    )
                    .await?;
                tracing::info!(
                    torrent = source.name,
                    indexer,
                    "cross-seed: o release já existe no destino; pacote descartado"
                );
                // A pasta adaptada não serve mais a ninguém: o hardlink dela
                // seguraria o vídeo no disco depois que a origem sair.
                if launch.adapted_path.is_some()
                    && let Some(warning) =
                        crate::launch::discard_folder(config, store, launch).await
                {
                    tracing::warn!(
                        torrent = source.name,
                        indexer,
                        "cross-seed: pasta adaptada fica: {warning}"
                    );
                }
            }
        }
        // Irmão recém-adicionado espera a conferência (`verify`); só o que já
        // estava no cliente completo e semeando (ou parado e iniciado agora)
        // conta como semeando. Parado/incompleto mantém o par urgente.
        if is_launched && seeding {
            store
                .set_launch_state(&source.hash, indexer, LaunchState::Semeando)
                .await?;
            tracing::info!(
                torrent = source.name,
                indexer,
                "cross-seed: o publicado do lançamento foi achado; semeando"
            );
        }
    }
    Ok(format!(
        "{consulted} consultados, {added} irmãos achados; {started} conferidos e semeando, {rejected} recusados"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn torrent(state: &str, progress: f64, tags: &str, added_on: i64) -> TorrentInfo {
        serde_json::from_value(serde_json::json!({
            "hash": "h", "name": "n", "state": state, "save_path": "/d",
            "progress": progress, "tags": tags, "added_on": added_on,
        }))
        .unwrap()
    }

    #[test]
    fn release_de_layout_diferente_nao_e_baixado_de_novo() {
        let known = Differing::default();
        assert!(!known.knows("a", "bj", "g1"));
        known.remember("a", "bj", "g1");
        assert!(known.knows("a", "bj", "g1"));
        // Outro par, outro indexador ou outro release não herdam o veredito.
        assert!(!known.knows("b", "bj", "g1"));
        assert!(!known.knows("a", "amigos", "g1"));
        assert!(!known.knows("a", "bj", "g2"));
        // Passando do teto a memória recomeça com o último.
        for n in 0..DIFFERING_CAP {
            known.remember("a", "bj", &format!("x{n}"));
        }
        assert!(known.knows("a", "bj", &format!("x{}", DIFFERING_CAP - 1)));
        assert!(!known.knows("a", "bj", "g1"));
    }

    #[test]
    fn irmao_so_semeia_conferido_com_cem_por_cento() {
        let pending = PENDING_TAG;
        let checking = format!("{PENDING_TAG}, {CHECKING_TAG}");
        let now = 100_000;
        // Recém-adicionado, nunca conferido: manda conferir.
        assert_eq!(
            verdict(&torrent("stoppedDL", 0.0, pending, now), now),
            Verdict::Recheck
        );
        assert_eq!(
            verdict(&torrent("checkingDL", 0.4, &checking, 1), now),
            Verdict::Wait
        );
        assert_eq!(
            verdict(&torrent("stoppedUP", 1.0, &checking, 1), now),
            Verdict::Start
        );
        // Conferência pedida há pouco e ainda não começou: espera.
        assert_eq!(
            verdict(&torrent("stoppedDL", 0.0, &checking, now - 60), now),
            Verdict::Wait
        );
        // Erro de disco: nem inicia nem recusa.
        assert_eq!(
            verdict(&torrent("missingFiles", 1.0, &checking, 1), now),
            Verdict::Wait
        );
        assert_eq!(
            verdict(&torrent("error", 0.5, &checking, 1), now),
            Verdict::Wait
        );
        // Início que saiu com as tags ficando: tira as tags.
        assert_eq!(
            verdict(&torrent("stalledUP", 1.0, &checking, 1), now),
            Verdict::Start
        );
        // Incompleto andando: baixaria por cima do irmão.
        assert_eq!(
            verdict(&torrent("downloading", 0.97, &checking, now), now),
            Verdict::Reject
        );
        // Conferido abaixo de 100%: não é o mesmo arquivo.
        assert_eq!(
            verdict(&torrent("stoppedDL", 0.97, &checking, 1), now),
            Verdict::Reject
        );
    }

    #[test]
    fn irmao_so_e_o_publicado_do_lancamento_se_o_par_foi_enviado() {
        let checks = [
            check("a", "bj", "2026-10-01T00:00:00Z", 1, Some("Irmao")),
            check("b", "amigos", "2026-10-01T00:00:00Z", 1, Some("outro")),
        ];
        let sent: Pairs = [("a".to_owned(), "bj".to_owned())].into();
        assert_eq!(
            launched_pair_of(&checks, &sent, "irmao"),
            Some(("a".to_owned(), "bj".to_owned()))
        );
        // Irmão de um par que não foi enviado: nada a mudar no lançamento.
        assert_eq!(launched_pair_of(&checks, &sent, "outro"), None);
        assert_eq!(launched_pair_of(&checks, &Pairs::new(), "irmao"), None);
    }

    #[test]
    fn so_semeia_o_irmao_completo_em_semeadura_ativa() {
        for state in [
            "uploading",
            "stalledUP",
            "queuedUP",
            "forcedUP",
            "checkingUP",
        ] {
            assert_eq!(
                sibling(&torrent(state, 1.0, "", 1)),
                Sibling::Seeding,
                "{state}"
            );
        }
        for state in ["stoppedUP", "pausedUP"] {
            assert_eq!(
                sibling(&torrent(state, 1.0, "", 1)),
                Sibling::Stopped,
                "{state}"
            );
        }
        for state in ["stalledDL", "stoppedDL", "downloading", "uploading"] {
            assert_eq!(
                sibling(&torrent(state, 0.6, "", 1)),
                Sibling::Incomplete,
                "{state}"
            );
        }
        assert_eq!(sibling(&torrent("missingFiles", 1.0, "", 1)), Sibling::Busy);
    }

    #[test]
    fn irmao_ja_no_cliente_parado_inicia_e_incompleto_vai_a_conferencia() {
        assert_eq!(sibling(&torrent("uploading", 1.0, "", 1)), Sibling::Seeding);
        assert_eq!(sibling(&torrent("stoppedUP", 1.0, "", 1)), Sibling::Stopped);
        assert_eq!(sibling(&torrent("pausedUP", 1.0, "", 1)), Sibling::Stopped);
        assert_eq!(
            sibling(&torrent("stalledDL", 0.99, "", 1)),
            Sibling::Incomplete
        );
        assert_eq!(
            sibling(&torrent("stoppedDL", 0.0, "", 1)),
            Sibling::Incomplete
        );
        assert_eq!(sibling(&torrent("moving", 1.0, "", 1)), Sibling::Busy);
    }

    #[tokio::test]
    async fn irmao_ja_no_cliente_so_conta_se_semeia_de_verdade() {
        use std::time::Duration;
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let post = |route: &str| Mock::given(method("POST")).and(path(format!("/api/v2/{route}")));
        post("auth/login")
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        // Só o parado inicia; só o incompleto para e vai à fila de conferência.
        post("torrents/start")
            .and(body_string_contains("hashes=parado"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        post("torrents/start")
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        post("torrents/stop")
            .and(body_string_contains("hashes=incompleto"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        post("torrents/stop")
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        post("torrents/addTags")
            .and(body_string_contains("acervo%3Across-seed"))
            .and(body_string_contains("hashes=incompleto"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        post("torrents/addTags")
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        let with = |hash: &str, state: &str, progress: f64| {
            let mut t = torrent(state, progress, "", 1);
            t.hash = hash.to_owned();
            t
        };
        let seed = "seed:bj";
        // Na fila de conferência, mesmo completo e parado: quem decide é o `verify`.
        let pending = torrent("stoppedUP", 1.0, PENDING_TAG, 1);
        assert!(!adopt(&client, &pending, seed).await.unwrap());
        assert!(
            adopt(&client, &with("ativo", "uploading", 1.0), seed)
                .await
                .unwrap()
        );
        assert!(
            adopt(&client, &with("parado", "stoppedUP", 1.0), seed)
                .await
                .unwrap()
        );
        assert!(
            !adopt(&client, &with("incompleto", "stalledDL", 0.5), seed)
                .await
                .unwrap()
        );
        assert!(
            !adopt(&client, &with("movendo", "moving", 1.0), seed)
                .await
                .unwrap()
        );
        // Os `expect` são conferidos ao soltar o servidor.
        drop(server);
    }

    #[test]
    fn mesmo_conteudo_e_mesmos_caminhos_e_tamanhos() {
        let a = vec![("F/a.mkv".to_owned(), 10), ("F/a.srt".to_owned(), 1)];
        let b = vec![("F/a.srt".to_owned(), 1), ("F/a.mkv".to_owned(), 10)];
        assert!(same_layout(&a, &b));
        assert!(!same_layout(&a, &[("F/a.mkv".to_owned(), 10)]));
        assert!(!same_layout(
            &a,
            &[("G/a.mkv".to_owned(), 10), ("G/a.srt".to_owned(), 1)]
        ));
        assert!(!same_layout(&[], &[]));
    }

    #[test]
    fn tamanho_parecido_tolera_o_arredondamento_do_indexador() {
        let gib = 1 << 30;
        // "5,87 GB" de um arquivo de 5,874 GiB.
        assert!(close_in_size(5_870 * (gib / 1000), 5_874 * (gib / 1000)));
        assert!(!close_in_size(6 * gib, 5 * gib));
        // Arquivo pequeno: vale a margem fixa.
        assert!(close_in_size(500 << 20, 530 << 20));
    }

    fn fonte(hash: &str, indexer: &str) -> Source {
        Source {
            hash: hash.into(),
            name: hash.into(),
            save_path: "/d".into(),
            indexer: indexer.into(),
            origin: Origin::Movie(1),
        }
    }

    fn check(
        hash: &str,
        indexer: &str,
        at: &str,
        attempts: i32,
        found: Option<&str>,
    ) -> CrossSeedCheck {
        CrossSeedCheck {
            hash: hash.into(),
            indexer: indexer.into(),
            checked_at: at.into(),
            attempts,
            found: found.map(str::to_owned),
            outcome: if found.is_some() {
                CrossSeedOutcome::Found
            } else {
                CrossSeedOutcome::NotFound
            },
        }
    }

    #[test]
    fn consulta_que_deu_erro_volta_em_12_horas_sem_limite_de_tentativas() {
        // A conta presa no Amigos recusa o .torrent: o erro não diz se o
        // release existe lá, e o par não pode se esgotar antes de a prisão
        // acabar.
        let now = OffsetDateTime::parse("2026-10-15T12:00:00Z", &Rfc3339).unwrap();
        let sources = [fonte("a", "bj"), fonte("b", "bj")];
        let indexers = ["bj".to_owned(), "amigos".to_owned()];
        let erro = |hash: &str, at: &str| CrossSeedCheck {
            outcome: CrossSeedOutcome::Error,
            ..check(hash, "amigos", at, MAX_ATTEMPTS, None)
        };
        let checks = [
            erro("a", "2026-10-14T23:00:00Z"), // 13 h atrás: volta
            erro("b", "2026-10-15T01:00:00Z"), // 11 h atrás: espera
        ];
        let none = Pairs::new();
        let pairs: Vec<&str> = due(&sources, &indexers, &checks, &none, &none, now)
            .into_iter()
            .map(|(s, _)| s.hash.as_str())
            .collect();
        assert_eq!(pairs, ["a"]);
    }

    #[test]
    fn consulta_cada_par_uma_vez_e_volta_a_tentar_com_intervalo() {
        let now = OffsetDateTime::parse("2026-10-14T12:00:00Z", &Rfc3339).unwrap();
        let sources = [fonte("a", "bj"), fonte("b", "amigos"), fonte("c", "bj")];
        let indexers = ["bj".to_owned(), "amigos".to_owned()];
        let checks = [
            // Achou: não consulta mais.
            check("a", "amigos", "2026-10-01T00:00:00Z", 1, Some("x")),
            // Uma tentativa há 3 dias: já pode de novo (espera 2).
            check("b", "bj", "2026-10-11T12:00:00Z", 1, None),
            // Duas tentativas há 3 dias: espera 4, ainda não.
            check("c", "amigos", "2026-10-11T12:00:00Z", 2, None),
        ];
        let pairs: Vec<(&str, &str)> = due(
            &sources,
            &indexers,
            &checks,
            &Pairs::new(),
            &Pairs::new(),
            now,
        )
        .into_iter()
        .map(|(s, i)| (s.hash.as_str(), i))
        .collect();
        // Nunca o indexador de origem; só "b" volta.
        assert_eq!(pairs, [("b", "bj")]);

        let esgotado = [check("b", "bj", "2026-01-01T00:00:00Z", MAX_ATTEMPTS, None)];
        let novos: Vec<&str> = due(
            &sources,
            &indexers,
            &esgotado,
            &Pairs::new(),
            &Pairs::new(),
            now,
        )
        .into_iter()
        .map(|(s, _)| s.hash.as_str())
        .collect();
        assert_eq!(novos, ["a", "c"]);
    }

    fn launch(hash: &str, indexer: &str, state: LaunchState, sent_at: Option<&str>) -> Launch {
        Launch {
            hash: hash.into(),
            indexer: indexer.into(),
            state,
            progress: None,
            report: serde_json::json!({}),
            fields: serde_json::json!({}),
            screens: serde_json::json!([]),
            has_torrent: true,
            error: None,
            created_at: "2026-10-01T00:00:00Z".into(),
            updated_at: "2026-10-01T00:00:00Z".into(),
            sent_at: sent_at.map(str::to_owned),
            adapted_path: None,
            adapted_layout: None,
            site_hash: None,
        }
    }

    #[test]
    fn par_enviado_ha_menos_de_sete_dias_e_urgente() {
        let now = OffsetDateTime::parse("2026-10-14T12:00:00Z", &Rfc3339).unwrap();
        let launches = [
            launch(
                "a",
                "bj",
                LaunchState::Enviado,
                Some("2026-10-14T10:00:00Z"),
            ),
            // Enviado há mais de 7 dias: volta ao ritmo normal.
            launch(
                "b",
                "bj",
                LaunchState::Enviado,
                Some("2026-10-01T00:00:00Z"),
            ),
            // Pronto, mas não enviado: o site ainda não tem.
            launch("c", "bj", LaunchState::Pronto, None),
            // Já semeando: nada a procurar.
            launch(
                "d",
                "bj",
                LaunchState::Semeando,
                Some("2026-10-14T10:00:00Z"),
            ),
        ];
        let urgentes = urgent(&launches, now);
        assert_eq!(urgentes, Pairs::from([("a".to_owned(), "bj".to_owned())]));
        // Lançado é todo enviado, urgente ou não.
        assert_eq!(launched(&launches).len(), 2);
    }

    #[test]
    fn par_urgente_vem_primeiro_ignora_tentativas_e_espera_uma_hora() {
        let now = OffsetDateTime::parse("2026-10-14T12:00:00Z", &Rfc3339).unwrap();
        let sources = [
            fonte("a", "amigos"),
            fonte("b", "amigos"),
            fonte("c", "amigos"),
        ];
        let indexers = ["bj".to_owned(), "amigos".to_owned()];
        let urgentes = Pairs::from([
            ("b".to_owned(), "bj".to_owned()),
            ("c".to_owned(), "bj".to_owned()),
        ]);
        let checks = [
            // Esgotado e consultado há 2 h: urgente ignora o teto e o intervalo.
            check("b", "bj", "2026-10-14T10:00:00Z", MAX_ATTEMPTS + 2, None),
            // Consultado há 30 min: urgente, mas ainda dentro da hora.
            check("c", "bj", "2026-10-14T11:30:00Z", 1, None),
        ];
        let pairs: Vec<(&str, &str)> = due(&sources, &indexers, &checks, &urgentes, &urgentes, now)
            .into_iter()
            .map(|(s, i)| (s.hash.as_str(), i))
            .collect();
        // "b" passa à frente de "a", que nunca foi consultado.
        assert_eq!(pairs, [("b", "bj"), ("a", "bj")]);

        // Com irmão achado, nem o urgente volta.
        let achado = [check("b", "bj", "2026-10-01T00:00:00Z", 1, Some("x"))];
        let pairs: Vec<&str> = due(&sources, &indexers, &achado, &urgentes, &urgentes, now)
            .into_iter()
            .map(|(s, _)| s.hash.as_str())
            .collect();
        assert_eq!(pairs, ["c", "a"]);
    }

    #[test]
    fn par_lancado_passada_a_urgencia_segue_procurado_a_cada_dois_dias() {
        let now = OffsetDateTime::parse("2026-10-14T12:00:00Z", &Rfc3339).unwrap();
        let sources = [fonte("a", "amigos")];
        let indexers = ["bj".to_owned(), "amigos".to_owned()];
        let lancados = Pairs::from([("a".to_owned(), "bj".to_owned())]);
        let due_hashes = |checks: &[CrossSeedCheck]| {
            due(&sources, &indexers, checks, &lancados, &Pairs::new(), now).len()
        };
        // 168 consultas da semana urgente, a última há 3 dias: volta.
        let gasto = [check("a", "bj", "2026-10-11T12:00:00Z", 168, None)];
        assert_eq!(due_hashes(&gasto), 1);
        // Consultado há 1 dia: ainda dentro dos 2 dias.
        let recente = [check("a", "bj", "2026-10-13T12:00:00Z", 168, None)];
        assert_eq!(due_hashes(&recente), 0);
        // Sem ser lançado, o mesmo histórico esgota.
        assert!(
            due(
                &sources,
                &indexers,
                &gasto,
                &Pairs::new(),
                &Pairs::new(),
                now
            )
            .is_empty()
        );
    }

    #[test]
    fn preparar_de_novo_depois_do_envio_mantem_o_par_lancado() {
        let now = OffsetDateTime::parse("2026-10-14T12:00:00Z", &Rfc3339).unwrap();
        let enviado = Some("2026-10-14T10:00:00Z");
        let launches = [
            launch("a", "bj", LaunchState::Pronto, enviado),
            launch("b", "bj", LaunchState::Falhou, enviado),
            launch("c", "bj", LaunchState::Preparando, enviado),
            // Descartar é desistir; semeando já achou.
            launch("d", "bj", LaunchState::Descartado, enviado),
            launch("e", "bj", LaunchState::Semeando, enviado),
            // Nunca enviado.
            launch("f", "bj", LaunchState::Pronto, None),
        ];
        let esperado: Pairs = ["a", "b", "c"]
            .into_iter()
            .map(|h| (h.to_owned(), "bj".to_owned()))
            .collect();
        assert_eq!(launched(&launches), esperado);
        assert_eq!(urgent(&launches, now), esperado);
    }

    #[test]
    fn par_lancado_de_pasta_adaptada_compara_com_ela_e_semeia_do_pai_dela() {
        let enviado = Some("2026-10-14T10:00:00Z");
        let adaptado = |mut launch: Launch| {
            launch.adapted_path = Some("/media/lancamentos/bj/Filme".into());
            launch.adapted_layout = Some(serde_json::json!([
                ["Filme/Filme.mkv", 4000],
                ["Filme/Filme.pt-BR.srt", 70]
            ]));
            launch
        };
        let launches = [
            adaptado(launch("a", "bj", LaunchState::Enviado, enviado)),
            // Pronto e não enviado: ainda não há publicado.
            adaptado(launch("b", "bj", LaunchState::Pronto, None)),
            // Enviado sem pasta adaptada: o publicado é a origem.
            launch("c", "bj", LaunchState::Enviado, enviado),
        ];
        let adapted = adapted_published(&launches);
        assert_eq!(adapted.len(), 1);
        let pasta = &adapted[&("a".to_owned(), "bj".to_owned())];
        assert_eq!(pasta.save_path, "/media/lancamentos/bj");

        let origem = vec![("Filme/Filme.mkv".to_owned(), 4000)];
        // O publicado tem o .srt extraído: bate com a pasta, não com a origem.
        let publicado = vec![
            ("Filme/Filme.pt-BR.srt".to_owned(), 70),
            ("Filme/Filme.mkv".to_owned(), 4000),
        ];
        let alvo = published(&fonte("a", "amigos"), origem.clone(), Some(pasta));
        assert!(same_layout(&alvo.layout, &publicado));
        assert!(!same_layout(&origem, &publicado));
        assert_eq!(alvo.save_path, "/media/lancamentos/bj");
        // Sem pasta adaptada, tudo como antes: a origem e a pasta dela.
        let alvo = published(&fonte("c", "amigos"), origem.clone(), None);
        assert_eq!(alvo.layout, origem);
        assert_eq!(alvo.save_path, "/d");
    }

    #[test]
    fn release_achado_no_destino_descarta_so_pacote_nao_enviado() {
        let launches = [
            launch("a", "bj", LaunchState::Pronto, None),
            launch("b", "bj", LaunchState::Falhou, None),
            launch("c", "bj", LaunchState::Inelegivel, None),
            // Já enviado (mesmo preparado de novo): vira semeando, não descarte.
            launch("d", "bj", LaunchState::Pronto, Some("2026-10-14T10:00:00Z")),
            launch("e", "bj", LaunchState::Preparando, None),
            launch("f", "bj", LaunchState::Descartado, None),
        ];
        let achados: Vec<&str> = ["a", "b", "c", "d", "e", "f", "g"]
            .into_iter()
            .filter(|h| unsent_package(&launches, h, "bj").is_some())
            .collect();
        assert_eq!(achados, ["a", "b", "c"]);
        assert!(unsent_package(&launches, "a", "amigos").is_none());
    }

    // ---- mesmo tracker no cliente e conferência imediata

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// O `.torrent` de um arquivo (`Filme.mkv`, 10 bytes) com o announce dado.
    fn site_torrent(announce: &str) -> Vec<u8> {
        site_torrent_with(announce, "x", None)
    }

    /// Como [`site_torrent`], com as peças dadas e, se houver, um `source` no
    /// `info` (muda o infohash sem mudar o conteúdo).
    fn site_torrent_with(announce: &str, pieces: &str, source: Option<&str>) -> Vec<u8> {
        let source = source.map_or_else(String::new, |s| format!("6:source{}:{s}", s.len()));
        format!(
            "d8:announce{}:{announce}4:infod6:lengthi10e4:name9:Filme.mkv12:piece lengthi16384e6:pieces{}:{pieces}{source}ee",
            announce.len(),
            pieces.len()
        )
        .into_bytes()
    }

    /// O irmão `hash` no cliente: tracker do destino (outra passkey) e os
    /// mesmos arquivos; o export devolve `exported` (`None`: 404).
    async fn mount_sibling(server: &MockServer, hash: &str, exported: Option<Vec<u8>>) {
        mount_sibling_at(
            server,
            hash,
            "https://AMIGOS.exemplo/announce?passkey=OUTRA",
            exported,
        )
        .await;
    }

    async fn mount_sibling_at(
        server: &MockServer,
        hash: &str,
        tracker: &str,
        exported: Option<Vec<u8>>,
    ) {
        mount_get(
            server,
            "trackers",
            hash,
            serde_json::json!([{ "url": tracker, "status": 2 }]),
        )
        .await;
        mount_get(
            server,
            "files",
            hash,
            serde_json::json!([{ "name": "Filme.mkv", "size": 10 }]),
        )
        .await;
        mount_export(server, hash, exported).await;
    }

    async fn mount_export(server: &MockServer, hash: &str, exported: Option<Vec<u8>>) {
        let response = exported.map_or_else(
            || ResponseTemplate::new(404),
            |bytes| ResponseTemplate::new(200).set_body_bytes(bytes),
        );
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/export"))
            .and(query_param("hash", hash))
            .respond_with(response)
            .mount(server)
            .await;
    }

    const AMIGOS: &str = "https://amigos.exemplo/announce?passkey=SEGREDO";

    fn in_client(hash: &str, state: &str, tags: &str, content: &str) -> TorrentInfo {
        serde_json::from_value(serde_json::json!({
            "hash": hash, "name": "Filme", "state": state, "save_path": "/d",
            "progress": 1.0, "tags": tags, "category": "acervo",
            "content_path": content,
        }))
        .unwrap()
    }

    async fn qbit_mock() -> (MockServer, QbitClient) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let client = QbitClient::login(&server.uri(), "u", "s", Duration::from_secs(5))
            .await
            .unwrap();
        (server, client)
    }

    async fn mount_get(server: &MockServer, route: &str, hash: &str, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/torrents/{route}")))
            .and(query_param("hash", hash))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_post(server: &MockServer, route: &str, matcher: &str, times: u64) {
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/torrents/{route}")))
            .and(body_string_contains(matcher))
            .respond_with(ResponseTemplate::new(200))
            .expect(times)
            .mount(server)
            .await;
    }

    fn target() -> Published {
        Published {
            layout: vec![("Filme.mkv".to_owned(), 10)],
            save_path: "/d".to_owned(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn place_in(
        store: &Store,
        client: &QbitClient,
        present: &[TorrentInfo],
        launched: bool,
        bytes: Vec<u8>,
    ) -> (String, bool) {
        let present: HashMap<String, TorrentInfo> = present
            .iter()
            .map(|t| (t.hash.clone(), t.clone()))
            .collect();
        let hash = acervo_clients::info_hash(&bytes).unwrap();
        let mut config = Config::default();
        config.library.category = "acervo".into();
        place(
            &config,
            store,
            client,
            &present,
            &fonte("src", "bj"),
            "amigos",
            launched,
            &target(),
            bytes,
            hash,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn irmao_ja_no_cliente_com_o_mesmo_tracker_e_adotado_sem_adicionar() {
        let Some(db) = acervo_store::testing::TestDb::new("crossseed_adota").await else {
            return;
        };
        let (server, client) = qbit_mock().await;
        let mine = "8".repeat(40);
        let origin = "1".repeat(40);
        let other = "2".repeat(40);
        // O .torrent do site que o usuário adicionou à mão: mesmo conteúdo,
        // tracker do destino, hash diferente do candidato do cross-seed.
        let present = [
            in_client(&mine, "stalledUP", "", "/d/Filme.mkv"),
            in_client(&origin, "stalledUP", "", "/d/Filme.mkv"),
            in_client(&other, "stalledUP", "", "/d/Outro.mkv"),
        ];
        mount_get(
            &server,
            "trackers",
            &mine,
            serde_json::json!([
                { "url": "** [DHT] **", "status": 0 },
                { "url": "https://AMIGOS.exemplo/announce?passkey=OUTRA", "status": 2 },
            ]),
        )
        .await;
        mount_get(
            &server,
            "trackers",
            &origin,
            serde_json::json!([{ "url": "https://origem.exemplo/announce", "status": 2 }]),
        )
        .await;
        // Outro caminho: nem trackers nem arquivos são pedidos.
        Mock::given(method("GET"))
            .and(query_param("hash", other.as_str()))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        mount_get(
            &server,
            "files",
            &mine,
            serde_json::json!([{ "name": "Filme.mkv", "size": 10 }]),
        )
        .await;
        // As mesmas peças, com um `source` do site no info (outro infohash).
        mount_export(
            &server,
            &mine,
            Some(site_torrent_with(AMIGOS, "x", Some("amigos"))),
        )
        .await;
        mount_post(&server, "add", "", 0).await;
        mount_post(&server, "addTags", &format!("hashes={mine}"), 1).await;

        let (hash, seeding) =
            place_in(&db.store, &client, &present, true, site_torrent(AMIGOS)).await;
        // Grava o hash do que já existia, não o do candidato.
        assert_eq!(hash, mine);
        assert!(seeding);
        drop(server);
        db.drop().await;
    }

    #[tokio::test]
    async fn existente_de_outro_tracker_com_os_mesmos_arquivos_e_cross_seed_legitimo() {
        let Some(db) = acervo_store::testing::TestDb::new("crossseed_outro_tracker").await else {
            return;
        };
        let (server, client) = qbit_mock().await;
        let origin = "1".repeat(40);
        let present = [in_client(&origin, "stalledUP", "", "/d/Filme.mkv")];
        mount_get(
            &server,
            "trackers",
            &origin,
            serde_json::json!([{ "url": "https://origem.exemplo/announce", "status": 2 }]),
        )
        .await;
        // Tracker diferente: os arquivos nem são comparados.
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/files"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let bytes = site_torrent(AMIGOS);
        let candidate = acervo_clients::info_hash(&bytes).unwrap();
        // Adiciona, acha o torrent na lista e manda conferir na hora.
        mount_post(&server, "add", "", 1).await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .and(query_param("hashes", candidate.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([in_client_json(&candidate)])),
            )
            .mount(&server)
            .await;
        mount_post(&server, "recheck", &format!("hashes={candidate}"), 1).await;
        mount_post(&server, "addTags", "cross-seed%3Aconferindo", 1).await;

        let (hash, seeding) = place_in(&db.store, &client, &present, true, bytes).await;
        assert_eq!(hash, candidate);
        assert!(!seeding);
        drop(server);
        db.drop().await;
    }

    fn in_client_json(hash: &str) -> serde_json::Value {
        serde_json::json!({
            "hash": hash, "name": "Filme", "state": "stoppedDL", "save_path": "/d",
        })
    }

    #[tokio::test]
    async fn mesmo_tracker_com_arquivos_diferentes_nao_e_o_mesmo_conteudo() {
        let (server, client) = qbit_mock().await;
        let mine = "8".repeat(40);
        let present = [in_client(&mine, "stalledUP", "", "/d/Filme.mkv")];
        mount_get(
            &server,
            "trackers",
            &mine,
            serde_json::json!([{ "url": AMIGOS, "status": 2 }]),
        )
        .await;
        mount_get(
            &server,
            "files",
            &mine,
            serde_json::json!([{ "name": "Filme.mkv", "size": 11 }]),
        )
        .await;
        let found = same_tracker_sibling(&client, &present, "/d", &site_torrent(AMIGOS))
            .await
            .unwrap();
        assert!(found.is_none());
        // O próprio candidato no cliente não é "outro": é o caso do mesmo hash.
        let own = acervo_clients::info_hash(&site_torrent(AMIGOS)).unwrap();
        let present = [in_client(&own, "stalledUP", "", "/d/Filme.mkv")];
        let found = same_tracker_sibling(&client, &present, "/d", &site_torrent(AMIGOS))
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn so_adota_com_as_mesmas_pecas_e_o_mesmo_caminho_de_announce() {
        let (server, client) = qbit_mock().await;
        let mine = "8".repeat(40);
        let present = [in_client(&mine, "stalledUP", "", "/d/Filme.mkv")];
        let candidate = site_torrent(AMIGOS);
        let find = || same_tracker_sibling(&client, &present, "/d", &candidate);

        // Peças diferentes: mesmo layout e tracker, outro conteúdo.
        mount_sibling(&server, &mine, Some(site_torrent_with(AMIGOS, "y", None))).await;
        assert!(find().await.unwrap().is_none());

        // Mesmas peças com `source` diferente: adota.
        server.reset().await;
        mount_sibling(
            &server,
            &mine,
            Some(site_torrent_with(AMIGOS, "x", Some("amigos"))),
        )
        .await;
        let found = find().await.unwrap().expect("adota");
        assert_eq!(found.hash, mine);

        // Export recusado: sem prova, não adota (e não é erro).
        server.reset().await;
        mount_sibling(&server, &mine, None).await;
        assert!(find().await.unwrap().is_none());

        // Export que não é um .torrent: idem.
        server.reset().await;
        mount_sibling(&server, &mine, Some(b"<html>".to_vec())).await;
        assert!(find().await.unwrap().is_none());

        // Outro caminho de announce no mesmo host: outro tracker.
        server.reset().await;
        mount_sibling_at(
            &server,
            &mine,
            "https://amigos.exemplo/outro/announce",
            Some(site_torrent_with(AMIGOS, "x", None)),
        )
        .await;
        assert!(find().await.unwrap().is_none());
        // Outra porta, idem.
        server.reset().await;
        mount_sibling_at(
            &server,
            &mine,
            "https://amigos.exemplo:8443/announce",
            Some(site_torrent_with(AMIGOS, "x", None)),
        )
        .await;
        assert!(find().await.unwrap().is_none());
        drop(server);
    }

    #[tokio::test]
    async fn sem_prova_de_conteudo_o_irmao_e_adicionado_e_conferido() {
        let Some(db) = acervo_store::testing::TestDb::new("crossseed_sem_prova").await else {
            return;
        };
        let (server, client) = qbit_mock().await;
        let mine = "8".repeat(40);
        let present = [in_client(&mine, "stalledUP", "", "/d/Filme.mkv")];
        mount_sibling(&server, &mine, None).await;
        let bytes = site_torrent(AMIGOS);
        let candidate = acervo_clients::info_hash(&bytes).unwrap();
        mount_post(&server, "add", "", 1).await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .and(query_param("hashes", candidate.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([in_client_json(&candidate)])),
            )
            .mount(&server)
            .await;
        mount_post(&server, "recheck", &format!("hashes={candidate}"), 1).await;
        mount_post(&server, "addTags", "cross-seed%3Aconferindo", 1).await;

        let (hash, seeding) = place_in(&db.store, &client, &present, true, bytes).await;
        assert_eq!(hash, candidate);
        assert!(!seeding);
        drop(server);
        db.drop().await;
    }

    /// Responde à lista do cliente com o irmão pendente; a tag de conferência
    /// só aparece depois que o cliente a recebeu.
    struct Listing {
        hash: String,
        tagged: Arc<AtomicBool>,
    }

    impl wiremock::Respond for Listing {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let tags = if self.tagged.load(Ordering::SeqCst) {
                format!("{PENDING_TAG}, {CHECKING_TAG}")
            } else {
                PENDING_TAG.to_owned()
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "hash": self.hash, "name": "Filme", "state": "stoppedDL",
                "save_path": "/d", "progress": 0.0, "tags": tags,
                "added_on": OffsetDateTime::now_utc().unix_timestamp(),
            }]))
        }
    }

    struct Tagging(Arc<AtomicBool>);

    impl wiremock::Respond for Tagging {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            self.0.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200)
        }
    }

    #[tokio::test]
    async fn verify_concorrente_nao_manda_um_segundo_recheck() {
        let Some(db) = acervo_store::testing::TestDb::new("crossseed_recheck_unico").await else {
            return;
        };
        let (server, client) = qbit_mock().await;
        let hash = "c".repeat(40);
        let tagged = Arc::new(AtomicBool::new(false));
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(Listing {
                hash: hash.clone(),
                tagged: tagged.clone(),
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/addTags"))
            .respond_with(Tagging(tagged))
            .mount(&server)
            .await;
        mount_post(&server, "recheck", &format!("hashes={hash}"), 1).await;
        // Um recheck só: nada de iniciar nem de recusar o irmão.
        mount_post(&server, "start", "", 0).await;
        mount_post(&server, "delete", "", 0).await;

        let launched = Pairs::new();
        let (rechecked, verified) = tokio::join!(
            recheck_now(&client, &hash, 3, Duration::from_millis(1)),
            verify(&db.store, &client, &launched),
        );
        assert!(rechecked);
        assert_eq!(verified.unwrap(), (0, 0));
        drop(server);
        db.drop().await;
    }

    #[tokio::test]
    async fn recheck_sem_o_torrent_na_lista_fica_para_o_verify() {
        let (server, client) = qbit_mock().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(2)
            .mount(&server)
            .await;
        mount_post(&server, "recheck", "", 0).await;
        assert!(!recheck_now(&client, &"c".repeat(40), 2, Duration::from_millis(1)).await);
        drop(server);
    }

    #[tokio::test]
    async fn verify_pending_inicia_o_irmao_conferido_a_cem_por_cento() {
        let Some(db) = acervo_store::testing::TestDb::new("crossseed_verify_pending").await else {
            return;
        };
        let (server, _) = qbit_mock().await;
        let hash = "c".repeat(40);
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "hash": hash, "name": "Filme", "state": "stoppedUP", "save_path": "/d",
                    "progress": 1.0, "added_on": 1,
                    "tags": format!("{PENDING_TAG}, {CHECKING_TAG}"),
                }])),
            )
            .mount(&server)
            .await;
        mount_post(&server, "start", &format!("hashes={hash}"), 1).await;
        mount_post(&server, "removeTags", "", 2).await;
        let mut config = Config::default();
        config.qbittorrent.url = server.uri();
        let done = verify_pending(&config, &db.store).await.unwrap();
        assert_eq!(done, (1, 0));
        drop(server);
        db.drop().await;
    }
}

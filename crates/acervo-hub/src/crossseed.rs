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

use std::collections::{HashMap, HashSet};

use acervo_clients::{AddOptions, NewTorrent, QbitClient, TorrentInfo, layout};
use acervo_core::DownloadHash;
use acervo_indexers::SearchQuery;
use acervo_store::{CrossSeedCheck, GrabState, Store};
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
/// Tempo que o irmão recém-adicionado tem para aparecer e começar a
/// conferência antes de o resultado contar.
const SETTLE_SECS: i64 = 10 * 60;

/// De onde veio um torrent do acervo: o que buscar no outro tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    Movie(i64),
    Series(i64),
}

/// Um torrent completo do acervo que pode ter irmão em outro tracker.
#[derive(Debug, Clone)]
struct Source {
    hash: String,
    name: String,
    save_path: String,
    indexer: String,
    origin: Origin,
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
fn same_layout(a: &[(String, u64)], b: &[(String, u64)]) -> bool {
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

/// Os pares (torrent, indexador) que já podem ser consultados, na ordem:
/// nunca consultados primeiro, depois os mais antigos. Fica de fora o
/// indexador de onde o torrent veio, o par que já achou irmão e o que
/// esgotou as tentativas ou ainda está no intervalo entre elas.
fn due<'a>(
    sources: &'a [Source],
    indexers: &'a [String],
    checks: &[CrossSeedCheck],
    now: OffsetDateTime,
) -> Vec<(&'a Source, &'a str)> {
    let by_pair: HashMap<(&str, &str), &CrossSeedCheck> = checks
        .iter()
        .map(|c| ((c.hash.as_str(), c.indexer.as_str()), c))
        .collect();
    let mut pairs: Vec<(Option<OffsetDateTime>, &Source, &str)> = Vec::new();
    for source in sources {
        for indexer in indexers {
            if *indexer == source.indexer {
                continue;
            }
            let Some(check) = by_pair.get(&(source.hash.as_str(), indexer.as_str())) else {
                pairs.push((None, source, indexer));
                continue;
            };
            if check.found.is_some() || check.attempts >= MAX_ATTEMPTS {
                continue;
            }
            let Ok(at) = OffsetDateTime::parse(&check.checked_at, &Rfc3339) else {
                continue;
            };
            let wait = time::Duration::days(RETRY_DAYS * i64::from(check.attempts));
            if now - at >= wait {
                pairs.push((Some(at), source, indexer));
            }
        }
    }
    pairs.sort_by_key(|(at, source, _)| (*at, source.hash.clone()));
    pairs
        .into_iter()
        .map(|(_, source, indexer)| (source, indexer))
        .collect()
}

/// Os torrents completos do acervo com grab (de onde se sabe o que são),
/// fora os próprios irmãos de cross-seed.
fn sources(
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
async fn owners(store: &Store) -> Result<HashMap<String, (String, Origin)>> {
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
async fn query(store: &Store, source: &Source) -> Result<Option<SearchQuery>> {
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

/// Procura o irmão de `source` em `indexer` e, achando, o adiciona parado
/// para conferir. Devolve o infohash do irmão.
async fn find(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    client: &QbitClient,
    present: &HashSet<String>,
    source: &Source,
    indexer: &str,
) -> Result<Option<String>> {
    let Some(query) = query(store, source).await? else {
        return Ok(None);
    };
    let files = client
        .files(&DownloadHash::new(source.hash.clone()))
        .await?;
    // Pacote baixado em parte (episódios com prioridade zero): o irmão
    // precisaria de arquivos que não estão no disco, e nunca conferiria.
    if files.iter().any(|f| f.priority == 0 || f.progress < 1.0) {
        return Ok(None);
    }
    let files: Vec<(String, u64)> = files.into_iter().map(|f| (f.name, f.size)).collect();
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
    for release in releases.into_iter().take(CANDIDATES) {
        let (torrent, hash) = match crate::grab::fetch_torrent(catalog, &release).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(indexer, release = release.title, "cross-seed: {error:#}");
                continue;
            }
        };
        let NewTorrent::File(bytes) = &torrent else {
            continue;
        };
        if !layout(bytes).is_some_and(|other| same_layout(&files, &other)) {
            continue;
        }
        if present.contains(&hash) {
            return Ok(Some(hash));
        }
        let mut tags = vec![PENDING_TAG.to_owned()];
        tags.extend(crate::grab::seed_tag(store, indexer).await);
        let options = AddOptions {
            category: config.library.category.clone(),
            save_path: Some(source.save_path.clone()),
            stopped: true,
            stop_after_metadata: false,
            tags,
        };
        client.add(torrent, &options).await?;
        tracing::info!(
            torrent = source.name,
            indexer,
            "cross-seed: irmão adicionado, conferindo"
        );
        return Ok(Some(hash));
    }
    Ok(None)
}

/// Confere os irmãos pendentes: manda conferir, inicia o que deu 100% e
/// tira do cliente, sem os arquivos, o que não deu. Devolve (iniciados,
/// recusados).
async fn verify(
    store: &Store,
    client: &QbitClient,
    torrents: &[TorrentInfo],
) -> Result<(usize, usize)> {
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

/// Uma volta: confere os pendentes e consulta os próximos pares.
///
/// # Errors
///
/// Cliente ou banco inalcançável. Falha de um indexador só conta como
/// consulta sem achar.
pub(crate) async fn run(config: &Config, store: &Store, catalog: &Catalog) -> Result<String> {
    let client = crate::grab::qbit(config).await?;
    let torrents = client.torrents().await?;
    let (started, rejected) = verify(store, &client, &torrents).await?;

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
    let present: HashSet<String> = torrents
        .iter()
        .map(|t| t.hash.to_ascii_lowercase())
        .collect();
    let (mut added, mut consulted) = (0, 0);
    for (source, indexer) in due(&sources, &indexers, &checks, now)
        .into_iter()
        .take(PER_RUN)
    {
        consulted += 1;
        let found = match find(config, store, catalog, &client, &present, source, indexer).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(torrent = source.name, indexer, "cross-seed: {error:#}");
                None
            }
        };
        added += usize::from(found.is_some());
        let at = now.format(&Rfc3339)?;
        store
            .record_cross_seed_check(&source.hash, indexer, &at, found.as_deref())
            .await?;
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
        }
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
        let pairs: Vec<(&str, &str)> = due(&sources, &indexers, &checks, now)
            .into_iter()
            .map(|(s, i)| (s.hash.as_str(), i))
            .collect();
        // Nunca o indexador de origem; só "b" volta.
        assert_eq!(pairs, [("b", "bj")]);

        let esgotado = [check("b", "bj", "2026-01-01T00:00:00Z", MAX_ATTEMPTS, None)];
        let novos: Vec<&str> = due(&sources, &indexers, &esgotado, now)
            .into_iter()
            .map(|(s, _)| s.hash.as_str())
            .collect();
        assert_eq!(novos, ["a", "c"]);
    }
}

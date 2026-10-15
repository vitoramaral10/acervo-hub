//! A tela "Para lançar", sob `/ui/api/biblioteca/para-lancar`, com a mesma
//! entrada das outras rotas da biblioteca (sessão ou chave, e o cabeçalho
//! anti-CSRF em ação que muda estado).
//!
//! A lista não é guardada: sai das fontes do cross-seed (torrents completos
//! do acervo com grab), das consultas já feitas a cada destino e dos pacotes
//! em `launches`. Nada aqui devolve o `.torrent` nem o announce, fora a rota
//! de download, que só entrega pacote pronto.

// Handler devolve a resposta de erro pronta; é o formato do axum.
#![allow(clippy::result_large_err, clippy::unnecessary_wraps)]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use acervo_clients::TorrentInfo;
use acervo_store::{CrossSeedCheck, CrossSeedOutcome, Launch, LaunchState, Store};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::rules::Site;
use crate::crossseed::{Origin, Source};
use crate::web::{Shared, Web, WebError, WebResult, anyhow_bad, bad, enter, fail, ok};

pub fn router(web: Arc<Web>) -> Router {
    routes().with_state(web)
}

/// As rotas, ainda sem o estado.
pub(crate) fn routes() -> Router<Arc<Web>> {
    Router::new()
        .route("/ui/api/biblioteca/para-lancar", get(list))
        .route("/ui/api/biblioteca/para-lancar/andamento", get(progress))
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}",
            get(detail),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/preparar",
            post(prepare),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/torrent",
            get(download),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/enviado",
            post(sent),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/descartar",
            post(discard),
        )
}

// ---------------------------------------------------------------- candidatos

/// Um par da lista: a fonte (se ainda está no cliente), a última consulta
/// ao destino e o pacote.
#[derive(Debug, Clone)]
pub(crate) struct Candidate<'a> {
    pub(crate) hash: String,
    pub(crate) indexer: String,
    /// `None`: o pacote existe, mas o torrent de origem saiu do cliente (ou
    /// deixou de ser fonte).
    pub(crate) source: Option<&'a Source>,
    pub(crate) outcome: Option<CrossSeedOutcome>,
    pub(crate) launch: Option<&'a Launch>,
}

/// Os candidatos: par (fonte, destino) cuja última consulta não achou nada
/// parecido, ou achou parecido com outros arquivos; mais todo par com
/// pacote, até o que já semeia (a tela filtra por estado). Par cujo irmão
/// foi achado e não tem pacote já existe no destino: fica de fora.
pub(crate) fn candidates<'a>(
    sources: &'a [Source],
    destinations: &[String],
    checks: &[CrossSeedCheck],
    launches: &'a [Launch],
) -> Vec<Candidate<'a>> {
    let by_check: HashMap<(&str, &str), &CrossSeedCheck> = checks
        .iter()
        .map(|c| ((c.hash.as_str(), c.indexer.as_str()), c))
        .collect();
    let by_launch: HashMap<(&str, &str), &Launch> = launches
        .iter()
        .map(|l| ((l.hash.as_str(), l.indexer.as_str()), l))
        .collect();
    let mut out = Vec::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for source in sources {
        for destination in destinations {
            if *destination == source.indexer {
                continue;
            }
            let key = (source.hash.as_str(), destination.as_str());
            let check = by_check.get(&key);
            let launch = by_launch.get(&key).copied();
            let open = check.is_some_and(|c| {
                c.found.is_none()
                    && matches!(
                        c.outcome,
                        CrossSeedOutcome::NotFound | CrossSeedOutcome::LayoutDiffers
                    )
            });
            if launch.is_none() && !open {
                continue;
            }
            seen.insert(key);
            out.push(Candidate {
                hash: source.hash.clone(),
                indexer: destination.clone(),
                source: Some(source),
                outcome: check.map(|c| c.outcome),
                launch,
            });
        }
    }
    for launch in launches {
        let key = (launch.hash.as_str(), launch.indexer.as_str());
        if seen.contains(&key) {
            continue;
        }
        out.push(Candidate {
            hash: launch.hash.clone(),
            indexer: launch.indexer.clone(),
            source: None,
            outcome: by_check.get(&key).map(|c| c.outcome),
            launch: Some(launch),
        });
    }
    out
}

/// O estado que a tela mostra: o do pacote, ou "a preparar" sem pacote.
fn state_name(launch: Option<&Launch>) -> &'static str {
    launch.map_or("a_preparar", |l| l.state.as_str())
}

/// Só o pacote montado entrega o `.torrent`: o de um preparo anterior fica
/// no banco até o novo terminar.
fn serves_torrent(launch: &Launch) -> bool {
    launch.has_torrent
        && matches!(
            launch.state,
            LaunchState::Pronto | LaunchState::Enviado | LaunchState::Semeando
        )
}

/// O que a tela sabe da obra: título e tipo.
struct Titles {
    movies: HashMap<i64, String>,
    series: HashMap<i64, String>,
}

impl Titles {
    async fn load(store: &Store) -> Result<Self, acervo_store::StoreError> {
        let (movies, series) = tokio::try_join!(store.movies(), store.series_list())?;
        Ok(Self {
            movies: movies
                .into_iter()
                .map(|m| (m.id, crate::events::label(&m.movie.title, m.movie.year)))
                .collect(),
            series: series
                .into_iter()
                .map(|s| (s.id, crate::events::label(&s.series.title, s.series.year)))
                .collect(),
        })
    }

    fn of(&self, origin: &Origin) -> (&'static str, Option<&String>) {
        match origin {
            Origin::Movie(id) => ("filme", self.movies.get(id)),
            Origin::Series(id) => ("serie", self.series.get(id)),
        }
    }
}

fn item(candidate: &Candidate<'_>, titles: &Titles, sizes: &HashMap<String, u64>) -> Value {
    let launch = candidate.launch;
    let site = Site::from_indexer(&candidate.indexer);
    let (kind, title) = candidate.source.map_or((None, None), |s| {
        let (kind, title) = titles.of(&s.origin);
        (Some(kind), title.cloned())
    });
    // Sem a fonte, o nome do release vem do texto já gerado.
    let release = candidate.source.map(|s| s.name.clone()).or_else(|| {
        launch
            .and_then(|l| l.fields.get("release"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let title = title.or_else(|| {
        launch
            .and_then(|l| l.fields.get("titulo"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    json!({
        "hash": candidate.hash,
        "indexer": candidate.indexer,
        "destino": site.map(Site::label),
        "site": site,
        "origem": candidate.source.map(|s| s.indexer.as_str()),
        "no_cliente": candidate.source.is_some(),
        "tipo": kind,
        "titulo": title,
        "release": release,
        "tamanho": sizes.get(&candidate.hash),
        "estado": state_name(launch),
        "desfecho": candidate.outcome.map(CrossSeedOutcome::as_str),
        "parecido_no_destino": candidate.outcome == Some(CrossSeedOutcome::LayoutDiffers),
        "progresso": launch.and_then(|l| l.progress.as_deref()),
        "erro": launch.and_then(|l| l.error.as_deref()),
        "tem_torrent": launch.is_some_and(serves_torrent),
        "atualizado_em": launch.map(|l| l.updated_at.as_str()),
        "enviado_em": launch.and_then(|l| l.sent_at.as_deref()),
    })
}

/// Os torrents do cliente; fora do ar, a lista sai só com os pacotes.
async fn client_torrents(web: &Web) -> Result<Vec<TorrentInfo>, String> {
    let client = crate::grab::qbit(&web.config())
        .await
        .map_err(|e| format!("{e:#}"))?;
    client.torrents().await.map_err(|e| e.to_string())
}

async fn list(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter_live(&web, &headers, &Method::GET).await?;
    let (records, checks, launches, titles) = tokio::try_join!(
        store.indexers(),
        store.cross_seed_checks(),
        store.launches(),
        Titles::load(store)
    )
    .map_err(|e| fail(bad(e)))?;
    let destinations: Vec<String> = records
        .into_iter()
        .filter(|r| r.enabled && Site::from_indexer(&r.name).is_some())
        .map(|r| r.name)
        .collect();
    let (torrents, warning) = match client_torrents(&web).await {
        Ok(torrents) => (torrents, None),
        Err(error) => (
            Vec::new(),
            Some(format!("cliente de download inalcançável: {error}")),
        ),
    };
    let owners = crate::crossseed::owners(store)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    let sources = crate::crossseed::sources(&torrents, &web.config().library.category, &owners);
    let sizes: HashMap<String, u64> = torrents
        .iter()
        .map(|t| (t.hash.to_ascii_lowercase(), t.size))
        .collect();
    let items: Vec<Value> = candidates(&sources, &destinations, &checks, &launches)
        .iter()
        .map(|c| item(c, &titles, &sizes))
        .collect();
    ok(&json!({ "itens": items, "aviso": warning }))
}

/// O andamento dos pares em preparo. A tela pergunta isto a cada poucos
/// segundos no lugar da lista, que loga no qBittorrent e recarrega o
/// catálogo a cada chamada: aqui é uma consulta só, ao banco.
async fn progress(State(web): Shared, headers: HeaderMap) -> WebResult {
    let store = enter_live(&web, &headers, &Method::GET).await?;
    let preparing = store.launches_preparing().await.map_err(|e| fail(bad(e)))?;
    let items: Vec<Value> = preparing
        .into_iter()
        .map(|(hash, indexer, progress)| {
            json!({ "hash": hash, "indexer": indexer, "progresso": progress })
        })
        .collect();
    ok(&json!({ "itens": items }))
}

// ---------------------------------------------------------------- um par

/// A entrada das rotas, e antes delas o resgate do par `preparando` sem job
/// ([`super::heal`]): sem ele `preparar` e `descartar` recusariam o par e a
/// tela consultaria o andamento para sempre. Falha do resgate só vai ao log.
async fn enter_live<'a>(
    web: &'a Web,
    headers: &HeaderMap,
    method: &Method,
) -> Result<&'a Store, Response> {
    let store = enter(web, headers, method).await?;
    if let Err(error) = super::heal(store).await {
        tracing::warn!("pacote sem job não resgatado: {error:#}");
    }
    Ok(store)
}

/// O par da rota: infohash (v1 ou v2, em hexadecimal) e um destino de
/// lançamento.
fn pair(hash: &str, indexer: &str) -> Result<(String, Site), WebError> {
    let hash = hash.to_ascii_lowercase();
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad("infohash inválido"));
    }
    let site = Site::from_indexer(indexer)
        .ok_or_else(|| bad(format!("`{indexer}` não é destino de lançamento")))?;
    Ok((hash, site))
}

fn conflict(message: impl Into<String>) -> Response {
    fail(WebError(StatusCode::CONFLICT, message.into()))
}

fn not_found(message: impl Into<String>) -> Response {
    fail(WebError(StatusCode::NOT_FOUND, message.into()))
}

async fn detail(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
) -> WebResult {
    let store = enter_live(&web, &headers, &Method::GET).await?;
    let (hash, site) = pair(&hash, &indexer).map_err(fail)?;
    let launch = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?;
    let Some(launch) = launch else {
        return ok(&json!({
            "hash": hash, "indexer": indexer, "destino": site.label(), "site": site,
            "estado": state_name(None), "progresso": null, "erro": null,
            "report": {}, "fields": {}, "screens": [], "tem_torrent": false,
            "criado_em": null, "atualizado_em": null, "enviado_em": null,
        }));
    };
    ok(&json!({
        "hash": hash,
        "indexer": indexer,
        "destino": site.label(),
        "site": site,
        "estado": launch.state.as_str(),
        "progresso": launch.progress,
        "erro": launch.error,
        "report": launch.report,
        "fields": launch.fields,
        "screens": launch.screens,
        "tem_torrent": serves_torrent(&launch),
        "criado_em": launch.created_at,
        "atualizado_em": launch.updated_at,
        "enviado_em": launch.sent_at,
    }))
}

async fn prepare(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
) -> WebResult {
    let store = enter_live(&web, &headers, &Method::POST).await?;
    let (hash, site) = pair(&hash, &indexer).map_err(fail)?;
    let enabled = store
        .indexers()
        .await
        .map_err(|e| fail(bad(e)))?
        .iter()
        .any(|r| r.enabled && r.name == indexer);
    if !enabled {
        return Err(fail(bad(format!(
            "o {} não está cadastrado ou está desativado",
            site.label()
        ))));
    }
    if let Some(launch) = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?
    {
        match launch.state {
            LaunchState::Preparando => return Err(conflict("o pacote já está sendo preparado")),
            LaunchState::Semeando => {
                return Err(conflict(
                    "o publicado já foi achado e está semeando: não há o que preparar",
                ));
            }
            _ => {}
        }
    }
    let claim = super::claim(&hash, &indexer)
        .ok_or_else(|| conflict("o pacote já está sendo preparado"))?;
    store
        .upsert_launch_state(
            &hash,
            &indexer,
            LaunchState::Preparando,
            Some(super::QUEUED),
            None,
        )
        .await
        .map_err(|e| fail(bad(e)))?;
    super::reset(store, &hash, &indexer)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    tokio::spawn(super::run(Arc::clone(&web), hash, indexer, claim));
    ok(&json!({ "ok": true, "estado": LaunchState::Preparando.as_str() }))
}

/// `filename` em ASCII (o que todo navegador lê) e `filename*` em UTF-8
/// percent-encoded (RFC 6266), para nome com acento.
fn content_disposition(name: &str) -> String {
    let file = format!("{name}.torrent");
    let ascii: String = file
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '/')) || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in file.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

async fn download(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (hash, _) = pair(&hash, &indexer).map_err(fail)?;
    let launch = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?
        .filter(serves_torrent)
        .ok_or_else(|| not_found("o pacote não tem .torrent pronto"))?;
    let bytes = store
        .launch_torrent(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| not_found("o pacote não tem .torrent pronto"))?;
    let name = launch
        .fields
        .get("release")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| hash.clone(), str::to_owned);
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    crate::api::secure_headers(headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-bittorrent"),
    );
    let disposition = HeaderValue::from_str(&content_disposition(&name))
        .map_err(|_| fail(bad("nome do release ilegível")))?;
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

/// O cross-seed já achou o irmão do par no destino.
fn already_there(checks: &[CrossSeedCheck], hash: &str, indexer: &str) -> bool {
    checks
        .iter()
        .any(|c| c.hash == hash && c.indexer == indexer && c.found.is_some())
}

async fn sent(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
) -> WebResult {
    let store = enter_live(&web, &headers, &Method::POST).await?;
    let (hash, _) = pair(&hash, &indexer).map_err(fail)?;
    let launch = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| not_found("prepare o pacote antes de enviá-lo"))?;
    match launch.state {
        LaunchState::Preparando => return Err(conflict("o pacote ainda está sendo preparado")),
        LaunchState::Semeando => return Err(conflict("o publicado já foi achado e está semeando")),
        _ => {}
    }
    // O release já está no destino (o cross-seed achou o irmão): enviar seria
    // duplicar, e o par nunca voltaria a ser procurado.
    let checks = store.cross_seed_checks().await.map_err(|e| fail(bad(e)))?;
    if already_there(&checks, &hash, &indexer) {
        return Err(conflict(
            "o release já existe no destino: não há o que enviar",
        ));
    }
    store
        .mark_launch_sent(&hash, &indexer, &crate::decide::now_rfc3339())
        .await
        .map_err(|e| fail(bad(e)))?;
    // O par volta a ser procurado primeiro, e urgente: o publicado é o
    // próprio arquivo do disco.
    store
        .reset_cross_seed_pair(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?;
    ok(&json!({ "ok": true, "estado": LaunchState::Enviado.as_str() }))
}

async fn discard(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
) -> WebResult {
    let store = enter_live(&web, &headers, &Method::POST).await?;
    let (hash, _) = pair(&hash, &indexer).map_err(fail)?;
    let launch = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?;
    match launch.as_ref().map(|l| l.state) {
        // O job gravaria o desfecho por cima do descarte.
        Some(LaunchState::Preparando) => {
            return Err(conflict("o pacote ainda está sendo preparado"));
        }
        // Já foi ao site: descartar tiraria o par do cross-seed urgente e da
        // tag de semear, e o publicado continua lá.
        Some(_) if launch.as_ref().is_some_and(crate::crossseed::sent_to_site) => {
            return Err(conflict(
                "o pacote já foi enviado ao site: não dá para descartar",
            ));
        }
        Some(_) => {
            store
                .set_launch_state(&hash, &indexer, LaunchState::Descartado)
                .await
                .map_err(|e| fail(bad(e)))?;
        }
        // Candidato sem pacote: o descarte é o pacote.
        None => {
            store
                .upsert_launch_state(&hash, &indexer, LaunchState::Descartado, None, None)
                .await
                .map_err(|e| fail(bad(e)))?;
        }
    }
    ok(&json!({ "ok": true, "estado": LaunchState::Descartado.as_str() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonte(hash: &str, indexer: &str) -> Source {
        Source {
            hash: hash.into(),
            name: format!("Filme.{hash}.1080p"),
            save_path: "/d".into(),
            indexer: indexer.into(),
            origin: Origin::Movie(1),
        }
    }

    fn check(hash: &str, indexer: &str, outcome: CrossSeedOutcome, found: bool) -> CrossSeedCheck {
        CrossSeedCheck {
            hash: hash.into(),
            indexer: indexer.into(),
            checked_at: "2026-10-14T12:00:00Z".into(),
            attempts: 1,
            found: found.then(|| "irmao".into()),
            outcome,
        }
    }

    fn launch(hash: &str, indexer: &str, state: LaunchState) -> Launch {
        Launch {
            hash: hash.into(),
            indexer: indexer.into(),
            state,
            progress: None,
            report: json!({}),
            fields: json!({ "release": "Filme.Antigo", "titulo": "Filme Antigo" }),
            screens: json!([]),
            has_torrent: true,
            error: None,
            created_at: "2026-10-01T00:00:00Z".into(),
            updated_at: "2026-10-01T00:00:00Z".into(),
            sent_at: None,
        }
    }

    const BJ: &str = "bjsharecustom";
    const AMIGOS: &str = "amigosshare";

    #[test]
    fn par_com_irmao_achado_nao_aceita_envio() {
        let checks = [
            check("a", BJ, CrossSeedOutcome::Found, true),
            check("b", BJ, CrossSeedOutcome::NotFound, false),
        ];
        assert!(already_there(&checks, "a", BJ));
        assert!(!already_there(&checks, "a", AMIGOS));
        assert!(!already_there(&checks, "b", BJ));
        assert!(!already_there(&checks, "c", BJ));
    }

    #[test]
    fn candidato_sai_da_consulta_sem_achado_ou_do_pacote() {
        let sources = [
            fonte("a", AMIGOS),
            fonte("b", AMIGOS),
            fonte("c", AMIGOS),
            fonte("d", AMIGOS),
            fonte("e", AMIGOS),
            fonte("f", BJ),
        ];
        let destinations = [BJ.to_owned(), AMIGOS.to_owned()];
        let checks = [
            check("a", BJ, CrossSeedOutcome::NotFound, false),
            check("b", BJ, CrossSeedOutcome::LayoutDiffers, false),
            // Busca que falhou ou não foi feita não diz que falta no destino.
            check("c", BJ, CrossSeedOutcome::Error, false),
            check("d", BJ, CrossSeedOutcome::Skipped, false),
            // Já existe no destino.
            check("e", BJ, CrossSeedOutcome::Found, true),
            // Origem é o próprio destino: nem consulta haveria.
            check("f", BJ, CrossSeedOutcome::NotFound, false),
        ];
        let launches = [
            // Pacote de par com erro de busca: aparece pelo pacote.
            launch("c", BJ, LaunchState::Falhou),
            // Par já semeando: o irmão achado não tira o pacote da lista.
            launch("e", BJ, LaunchState::Semeando),
            // Origem saiu do cliente: o pacote ainda aparece.
            launch("z", BJ, LaunchState::Enviado),
        ];
        let found = candidates(&sources, &destinations, &checks, &launches);
        let pairs: Vec<(&str, &str, bool)> = found
            .iter()
            .map(|c| (c.hash.as_str(), c.indexer.as_str(), c.source.is_some()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("a", BJ, true),
                ("b", BJ, true),
                ("c", BJ, true),
                ("e", BJ, true),
                ("z", BJ, false),
            ]
        );
        assert_eq!(found[1].outcome, Some(CrossSeedOutcome::LayoutDiffers));
        assert_eq!(found[2].launch.unwrap().state, LaunchState::Falhou);
        assert!(found[0].launch.is_none());
    }

    #[test]
    fn item_marca_parecido_no_destino_e_nao_tem_segredo() {
        let sources = [fonte("b", AMIGOS)];
        let checks = [check("b", BJ, CrossSeedOutcome::LayoutDiffers, false)];
        let mut pronto = launch("b", BJ, LaunchState::Pronto);
        pronto.progress = Some("x".into());
        let launches = [pronto];
        let found = candidates(&sources, &[BJ.to_owned()], &checks, &launches);
        let titles = Titles {
            movies: HashMap::from([(1, "Filme (2024)".to_owned())]),
            series: HashMap::new(),
        };
        let sizes = HashMap::from([("b".to_owned(), 42_u64)]);
        let value = item(&found[0], &titles, &sizes);
        assert_eq!(value["estado"], "pronto");
        assert_eq!(value["parecido_no_destino"], true);
        assert_eq!(value["destino"], "BJ");
        assert_eq!(value["origem"], AMIGOS);
        assert_eq!(value["titulo"], "Filme (2024)");
        assert_eq!(value["tipo"], "filme");
        assert_eq!(value["tamanho"], 42);
        assert_eq!(value["tem_torrent"], true);
        assert!(value.get("torrent").is_none());

        // Sem pacote: "a preparar"; sem a fonte, título e release do texto.
        let orfao = [launch("z", BJ, LaunchState::Enviado)];
        let found = candidates(&[], &[BJ.to_owned()], &[], &orfao);
        let value = item(&found[0], &titles, &HashMap::new());
        assert_eq!(value["titulo"], "Filme Antigo");
        assert_eq!(value["release"], "Filme.Antigo");
        assert_eq!(value["no_cliente"], false);
        assert_eq!(state_name(None), "a_preparar");
    }

    #[test]
    fn torrent_so_sai_de_pacote_pronto() {
        for (state, served) in [
            (LaunchState::Pronto, true),
            (LaunchState::Enviado, true),
            (LaunchState::Semeando, true),
            (LaunchState::Preparando, false),
            (LaunchState::Falhou, false),
            (LaunchState::Inelegivel, false),
            (LaunchState::Descartado, false),
        ] {
            assert_eq!(serves_torrent(&launch("a", BJ, state)), served, "{state:?}");
        }
        let mut sem = launch("a", BJ, LaunchState::Pronto);
        sem.has_torrent = false;
        assert!(!serves_torrent(&sem));
    }

    #[test]
    fn par_da_rota_exige_infohash_e_destino_de_lancamento() {
        let hash = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        let (normalized, site) = pair(hash, BJ).ok().unwrap();
        assert_eq!(normalized, hash.to_ascii_lowercase());
        assert_eq!(site, Site::Bj);
        assert!(pair(hash, "outro").is_err());
        assert!(pair("xyz", AMIGOS).is_err());
        assert!(pair(&"g".repeat(40), AMIGOS).is_err());
        assert!(pair(&"a".repeat(64), AMIGOS).is_ok());
    }

    #[test]
    fn nome_do_download_e_a_pasta_raiz() {
        assert_eq!(
            content_disposition("Filme.2024.1080p"),
            "attachment; filename=\"Filme.2024.1080p.torrent\"; \
             filename*=UTF-8''Filme.2024.1080p.torrent"
        );
        let header = content_disposition("Coração \"x\"");
        assert!(
            header.starts_with("attachment; filename=\"Cora__o _x_.torrent\""),
            "{header}"
        );
        assert!(header.ends_with("filename*=UTF-8''Cora%C3%A7%C3%A3o%20%22x%22.torrent"));
        assert!(HeaderValue::from_str(&header).is_ok());
    }

    const KEY: &str = "chave-de-teste-0123456789";

    async fn call(http: &reqwest::Client, method: reqwest::Method, url: &str) -> (u16, Value) {
        let response = http
            .request(method, url)
            .header("X-Api-Key", KEY)
            .header("X-Acervo", "1")
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta, pela API.
    async fn prepara_baixa_envia_e_descarta_pela_api() {
        let Some(db) = acervo_store::testing::TestDb::new("para_lancar_api").await else {
            return;
        };
        let store = &db.store;
        let settings = Arc::new(
            crate::settings::Settings::load(store.clone())
                .await
                .unwrap(),
        );
        settings
            .save_section(crate::config::SERVIDOR, json!({ "api_key": KEY }))
            .await
            .unwrap();
        let web = Arc::new(Web {
            settings,
            database: crate::serve::Database::connected(store.clone()),
            catalog: crate::api::Catalog::default(),
            searches: tokio::sync::Mutex::default(),
            series_searches: tokio::sync::Mutex::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}/ui/api/biblioteca/para-lancar",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, router(web)).await });
        let http = reqwest::Client::new();
        let get = reqwest::Method::GET;
        let post = reqwest::Method::POST;
        let pronto = "a".repeat(40);
        let novo = "b".repeat(40);
        let url = |hash: &str, action: &str| format!("{base}/{hash}/{BJ}{action}");

        // Sem chave, nada; sem o cabeçalho da tela, ação que muda estado não.
        assert_eq!(http.get(&base).send().await.unwrap().status().as_u16(), 401);
        let response = http
            .post(url(&novo, "/descartar"))
            .header("X-Api-Key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 403);

        // Sem cliente de download, a lista sai só com os pacotes e um aviso.
        let (status, body) = call(&http, get.clone(), &base).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["itens"], json!([]));
        assert!(
            body["aviso"].as_str().unwrap().contains("inalcançável"),
            "{body}"
        );

        // Destino que não é de lançamento; destino não cadastrado.
        let (status, _) = call(
            &http,
            post.clone(),
            &format!("{base}/{novo}/outro/preparar"),
        )
        .await;
        assert_eq!(status, 422);
        let (status, body) = call(&http, post.clone(), &url(&novo, "/preparar")).await;
        assert_eq!(status, 422);
        assert!(
            body["erro"].as_str().unwrap().contains("cadastrado"),
            "{body}"
        );

        store
            .insert_indexer(&acervo_store::IndexerRecord {
                name: BJ.into(),
                kind: "cardigann".into(),
                definition: None,
                url: None,
                settings: std::collections::BTreeMap::new(),
                enabled: true,
                priority: acervo_store::DEFAULT_INDEXER_PRIORITY,
                ratio_mode: false,
                added_at: None,
            })
            .await
            .unwrap();

        // O preparo roda em fundo e, sem cliente, termina em `falhou` com o
        // motivo.
        let (status, body) = call(&http, post.clone(), &url(&novo, "/preparar")).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["estado"], "preparando");
        let mut detail = Value::Null;
        for _ in 0..100 {
            (_, detail) = call(&http, get.clone(), &url(&novo, "")).await;
            if detail["estado"] != "preparando" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(detail["estado"], "falhou", "{detail}");
        assert!(
            detail["erro"]
                .as_str()
                .unwrap()
                .contains("cliente de download"),
            "{detail}"
        );
        assert_eq!(detail["tem_torrent"], false);

        // O andamento lista só o que está em preparo, sem relatório nem texto.
        let (status, body) = call(&http, get.clone(), &format!("{base}/andamento")).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["itens"], json!([]));
        // Com job vivo (a posse do par está tomada) o andamento aparece.
        let live = crate::launch::claim(&novo, BJ).unwrap();
        store
            .upsert_launch_state(
                &novo,
                BJ,
                LaunchState::Preparando,
                Some("gerando o .torrent (40%)"),
                None,
            )
            .await
            .unwrap();
        let (_, body) = call(&http, get.clone(), &format!("{base}/andamento")).await;
        assert_eq!(
            body["itens"],
            json!([{ "hash": novo, "indexer": BJ, "progresso": "gerando o .torrent (40%)" }])
        );
        // Sem job nenhum (o que gravou o estado falhou antes de disparar, ou o
        // desfecho não gravou) o par não fica `preparando` para sempre.
        drop(live);
        let (_, body) = call(&http, get.clone(), &format!("{base}/andamento")).await;
        assert_eq!(body["itens"], json!([]), "{body}");
        let (_, detail) = call(&http, get.clone(), &url(&novo, "")).await;
        assert_eq!(detail["estado"], "falhou", "{detail}");
        assert!(detail["erro"].as_str().unwrap().contains("interrompido"));
        // `preparar` aceita o par que estava preso.
        let preso = "e".repeat(40);
        store
            .upsert_launch_state(&preso, BJ, LaunchState::Preparando, Some("na fila"), None)
            .await
            .unwrap();
        let (status, body) = call(&http, post.clone(), &url(&preso, "/preparar")).await;
        assert_eq!(status, 200, "{body}");
        for _ in 0..100 {
            let (_, detail) = call(&http, get.clone(), &url(&preso, "")).await;
            if detail["estado"] != "preparando" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // `descartar` também.
        let preso = "f".repeat(40);
        store
            .upsert_launch_state(&preso, BJ, LaunchState::Preparando, Some("na fila"), None)
            .await
            .unwrap();
        let (status, body) = call(&http, post.clone(), &url(&preso, "/descartar")).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            store.launch(&preso, BJ).await.unwrap().unwrap().state,
            LaunchState::Descartado
        );
        store
            .upsert_launch_state(&novo, BJ, LaunchState::Falhou, None, Some("teste"))
            .await
            .unwrap();

        // Pacote pronto: o `.torrent` sai com nome e cabeçalhos de segurança.
        store
            .upsert_launch_state(&pronto, BJ, LaunchState::Pronto, None, None)
            .await
            .unwrap();
        store
            .save_launch_fields(&pronto, BJ, &json!({ "release": "Filme.2024.1080p" }))
            .await
            .unwrap();
        let bytes = b"d8:announce30:https://t.exemplo/PASSKEY123e".to_vec();
        store
            .save_launch_torrent(&pronto, BJ, &bytes)
            .await
            .unwrap();
        let (status, body) = call(&http, get.clone(), &url(&pronto, "")).await;
        assert_eq!(status, 200);
        assert_eq!(body["tem_torrent"], true);
        assert!(!body.to_string().contains("PASSKEY123"));
        let (_, list) = call(&http, get.clone(), &base).await;
        assert!(!list.to_string().contains("PASSKEY123"));
        assert_eq!(list["itens"].as_array().unwrap().len(), 4, "{list}");
        let response = http
            .get(url(&pronto, "/torrent"))
            .header("X-Api-Key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let headers = response.headers().clone();
        assert_eq!(headers["content-type"], "application/x-bittorrent");
        assert!(
            headers["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment; filename=\"Filme.2024.1080p.torrent\"")
        );
        assert!(headers.contains_key("content-security-policy"));
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
        // O pacote que falhou não entrega o `.torrent`.
        let (status, _) = call(&http, get.clone(), &url(&novo, "/torrent")).await;
        assert_eq!(status, 404);

        // Enviado: estado, data e a consulta do par apagada (volta primeiro e
        // urgente ao cross-seed).
        store
            .record_cross_seed_check(
                &pronto,
                BJ,
                "2026-10-01T00:00:00Z",
                None,
                CrossSeedOutcome::NotFound,
            )
            .await
            .unwrap();
        let (status, body) = call(&http, post.clone(), &url(&pronto, "/enviado")).await;
        assert_eq!(status, 200, "{body}");
        let launch = store.launch(&pronto, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Enviado);
        assert!(launch.sent_at.is_some());
        assert!(store.cross_seed_checks().await.unwrap().is_empty());
        // Enviar o que não foi preparado não existe.
        let (status, _) = call(&http, post.clone(), &url(&"c".repeat(40), "/enviado")).await;
        assert_eq!(status, 404);

        // Descartar um candidato sem pacote cria o pacote descartado.
        let fresco = "d".repeat(40);
        let (status, _) = call(&http, post.clone(), &url(&fresco, "/descartar")).await;
        assert_eq!(status, 200);
        assert_eq!(
            store.launch(&fresco, BJ).await.unwrap().unwrap().state,
            LaunchState::Descartado
        );

        // Enviado (ou preparado de novo depois do envio) não se descarta.
        let (status, _) = call(&http, post.clone(), &url(&pronto, "/descartar")).await;
        assert_eq!(status, 409);
        assert_eq!(
            store.launch(&pronto, BJ).await.unwrap().unwrap().state,
            LaunchState::Enviado
        );

        // Semeando não se prepara de novo.
        store
            .set_launch_state(&pronto, BJ, LaunchState::Semeando)
            .await
            .unwrap();
        let (status, _) = call(&http, post.clone(), &url(&pronto, "/preparar")).await;
        assert_eq!(status, 409);
        db.drop().await;
    }
}

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
use acervo_core::DownloadHash;
use acervo_store::{CrossSeedCheck, CrossSeedOutcome, Launch, LaunchState, Store};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::rules::{Site, SourceFile};
use crate::crossseed::{Origin, Source};
use crate::web::{Shared, Web, WebError, WebResult, anyhow_bad, bad, enter, fail, ok};

/// O maior `.torrent` que a rota de "torrent do site" aceita.
const SITE_TORRENT_LIMIT: usize = 10 << 20;

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
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/mediainfo.txt",
            get(download_mediainfo),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/capturas/{file}",
            get(download_capture),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/enviado",
            post(sent),
        )
        .route(
            "/ui/api/biblioteca/para-lancar/{hash}/{indexer}/torrent-do-site",
            post(site_torrent).layer(DefaultBodyLimit::max(SITE_TORRENT_LIMIT)),
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
            "pasta_adaptada": null, "layout_adaptado": null,
            "status_tracker": null, "torrent_do_site": false,
        }));
    };
    // O tracker só importa depois do envio: antes não há torrent semeando.
    let tracker =
        if crate::crossseed::sent_to_site(&launch) || launch.state == LaunchState::Semeando {
            Some(super::seeded_tracker_status(&web.config(), store, &launch).await)
        } else {
            None
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
        "pasta_adaptada": launch.adapted_path,
        "layout_adaptado": super::adapted_layout(&launch),
        "status_tracker": tracker,
        "torrent_do_site": launch.site_hash.is_some(),
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
    // A posse vem antes de ler o estado: quem lê sem ela pode ver `pronto`
    // enquanto o torrent do site o passa a `enviado`.
    let claim = super::claim(&hash, &indexer)
        .ok_or_else(|| conflict("o pacote já está sendo preparado ou em outra operação"))?;
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
            // Preparar de novo refaria a pasta adaptada, de onde o publicado
            // semeia; e o publicado é o que foi enviado, não um pacote novo.
            _ if crate::crossseed::sent_to_site(&launch) => {
                return Err(conflict(
                    "o pacote já foi enviado ao site: não dá para preparar de novo (o publicado \
                     semeia destes arquivos)",
                ));
            }
            _ => {}
        }
    }
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
    attachment(&format!("{name}.torrent"))
}

fn attachment(file: &str) -> String {
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

/// O nome do vídeo sem extensão, lido do "Complete name" do relatório (que o
/// `MediaInfo` grava só com o nome do arquivo). Sem ele, o release.
fn mediainfo_stem(report: &str, release: Option<&str>) -> String {
    let from_report = report
        .lines()
        .find(|l| l.trim_start().starts_with("Complete name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, name)| name.trim())
        .map(|name| name.rsplit(['/', '\\']).next().unwrap_or(name))
        .map(|name| name.rsplit_once('.').map_or(name, |(stem, _)| stem))
        .filter(|stem| !stem.is_empty());
    from_report
        .or(release.map(str::trim).filter(|r| !r.is_empty()))
        .unwrap_or("mediainfo")
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect()
}

async fn download_mediainfo(
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
        .ok_or_else(|| not_found("o pacote não tem relatório do MediaInfo"))?;
    let report = launch
        .fields
        .get("mediainfo")
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(|| not_found("o pacote não tem relatório do MediaInfo"))?;
    let stem = mediainfo_stem(report, launch.fields.get("release").and_then(Value::as_str));
    let mut response = Response::new(Body::from(format!("{}\n", report.trim_end())));
    let headers = response.headers_mut();
    crate::api::secure_headers(headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    let disposition = HeaderValue::from_str(&attachment(&format!("{stem}.txt")))
        .map_err(|_| fail(bad("nome do vídeo ilegível")))?;
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

/// `1`, `1.png` ou `1.jpg` (até 4) do fim da rota das capturas. A extensão é
/// só um apelido: o formato devolvido é o que coube no limite do Amigos.
fn capture_number(file: &str) -> Option<usize> {
    let digits = file
        .strip_suffix(".png")
        .or_else(|| file.strip_suffix(".jpg"))
        .unwrap_or(file);
    let number: usize = digits.parse().ok()?;
    // `+1` e `01` também passam por `parse`: só a forma canônica vale.
    (digits == number.to_string() && (1..=super::SCREEN_COUNT).contains(&number)).then_some(number)
}

/// O nome do vídeo sem extensão, sem nada que vire caminho ou controle.
fn capture_stem(video: &str) -> String {
    let name = video.rsplit(['/', '\\']).next().unwrap_or(video);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem: String = stem
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    if stem.trim().is_empty() {
        "captura".to_owned()
    } else {
        stem
    }
}

/// O vídeo principal da origem no disco: o torrent no cliente e o maior vídeo
/// dele, como no preparo.
async fn source_video(web: &Web, hash: &str) -> Result<(String, std::path::PathBuf), Response> {
    let client = crate::grab::qbit(&web.config())
        .await
        .map_err(|e| fail(WebError(StatusCode::BAD_GATEWAY, format!("{e:#}"))))?;
    let torrents = client
        .torrents()
        .await
        .map_err(|e| fail(WebError(StatusCode::BAD_GATEWAY, e.to_string())))?;
    let torrent = torrents
        .iter()
        .find(|t| t.hash.eq_ignore_ascii_case(hash))
        .ok_or_else(|| not_found("o torrent de origem não está mais no cliente"))?;
    let files = client
        .files(&DownloadHash::new(torrent.hash.clone()))
        .await
        .map_err(|e| fail(WebError(StatusCode::BAD_GATEWAY, e.to_string())))?;
    let files: Vec<SourceFile> = files
        .iter()
        .map(|f| SourceFile {
            path: f.name.clone(),
            size: f.size,
        })
        .collect();
    let main = super::main_video(&files).ok_or_else(|| not_found("o pacote não tem vídeo"))?;
    Ok((
        main.path.clone(),
        std::path::Path::new(&torrent.save_path).join(&main.path),
    ))
}

/// Uma das 4 capturas do pacote, tirada na hora do vídeo principal da origem,
/// na resolução real e sem escala. Nada vai a disco: a imagem sai do ffmpeg
/// direto para a resposta. É o que se anexa no formulário do site, no lugar
/// da miniatura ou da cópia reduzida do `ImgBB`. Sai PNG, ou JPEG se o PNG
/// passar do limite de 5 MiB do Amigos; `Content-Type` e a extensão do nome
/// dizem o que foi gerado, qualquer que seja a extensão pedida.
async fn download_capture(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer, file)): Path<(String, String, String)>,
) -> WebResult {
    let store = enter(&web, &headers, &Method::GET).await?;
    let (hash, _) = pair(&hash, &indexer).map_err(fail)?;
    let number = capture_number(&file).ok_or_else(|| {
        not_found(format!(
            "capturas vão de 1 a {} (com .png ou .jpg opcional)",
            super::SCREEN_COUNT
        ))
    })?;
    let launch = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| not_found("o pacote não existe"))?;
    let (relative, video) = source_video(&web, &hash).await?;
    if !std::fs::metadata(&video).is_ok_and(|m| m.is_file()) {
        return Err(not_found("o vídeo de origem não está no disco"));
    }
    // A duração do preparo; pacote de antes dela (ou sem ela) relê o vídeo.
    let recorded = launch
        .report
        .pointer("/capturas/duracao")
        .and_then(Value::as_f64);
    let duration = match recorded {
        Some(duration) => Some(duration),
        None => crate::mediainfo::probe_full(&video)
            .await
            .and_then(|media| media.duration_seconds),
    };
    let at = duration
        .filter(|d| d.is_finite() && *d > 0.0)
        .and_then(|d| super::screen_time(d, number))
        .ok_or_else(|| fail(bad("o ffprobe não disse a duração do vídeo")))?;
    let (image, format) = crate::mediainfo::screenshot(&video, at)
        .await
        .map_err(|error| {
            tracing::warn!(indexer, number, "captura para baixar: {error:#}");
            fail(WebError(
                StatusCode::BAD_GATEWAY,
                "não deu para tirar a captura (detalhe no log do hub)".into(),
            ))
        })?;
    let mut response = Response::new(Body::from(image));
    let headers = response.headers_mut();
    crate::api::secure_headers(headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.mime()),
    );
    let name = format!(
        "{}.captura-{number}.{}",
        capture_stem(&relative),
        format.extension()
    );
    let disposition = HeaderValue::from_str(&attachment(&name))
        .map_err(|_| fail(bad("nome do vídeo ilegível")))?;
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
    let _claim = own_pair(&hash, &indexer)?;
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
    mark_sent(store, &hash, &indexer).await?;
    // Semear antes da aprovação: o site só libera com seeder. Falha aqui não
    // desfaz o envio; vira erro do pacote e aviso na tela.
    let warning = seed_generated(&web, store, &launch).await;
    ok(&json!({
        "ok": true,
        "estado": LaunchState::Enviado.as_str(),
        "aviso": warning,
    }))
}

/// A posse do par (hash, destino) para uma rota que muda o pacote: preparar,
/// descartar, enviado e torrent do site disputam a mesma, e a que chega depois
/// recebe 409 em vez de rodar por cima. É a mesma trava do job de preparo.
fn own_pair(hash: &str, indexer: &str) -> Result<super::Claim, Response> {
    super::claim(hash, indexer).ok_or_else(|| {
        conflict(
            "outra operação está em andamento neste pacote (preparo, descarte, envio ou \
             .torrent do site): tente de novo em instantes",
        )
    })
}

/// Passa o pacote a `enviado`. O par volta a ser procurado primeiro, e
/// urgente: o publicado é o próprio arquivo do disco.
async fn mark_sent(store: &Store, hash: &str, indexer: &str) -> Result<(), Response> {
    store
        .mark_launch_sent(hash, indexer, &crate::decide::now_rfc3339())
        .await
        .map_err(|e| fail(bad(e)))?;
    store
        .reset_cross_seed_pair(hash, indexer)
        .await
        .map_err(|e| fail(bad(e)))?;
    Ok(())
}

/// Adiciona o `.torrent` gerado do pacote ao cliente, parado. Devolve o aviso
/// da tela quando não deu, depois de gravar o motivo em `error`.
async fn seed_generated(web: &Web, store: &Store, launch: &Launch) -> Option<String> {
    let bytes = match store.launch_torrent(&launch.hash, &launch.indexer).await {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(anyhow::anyhow!("o pacote não tem .torrent para semear")),
        Err(error) => Err(error.into()),
    };
    seed_or_warn(web, store, launch, bytes).await.1
}

/// Semeia o `.torrent` (ou o erro de obtê-lo) e fecha o resultado: sucesso
/// limpa o erro antigo do pacote; falha o grava e vira o aviso da tela. O
/// estado do pacote já foi gravado antes, e a falha nunca o desfaz.
async fn seed_or_warn(
    web: &Web,
    store: &Store,
    launch: &Launch,
    bytes: anyhow::Result<Vec<u8>>,
) -> (Option<super::Seeded>, Option<String>) {
    let seeded = async { super::seed_package(&web.config(), launch, bytes?).await }.await;
    let message = match seeded {
        Ok(seeded) => {
            if launch.error.is_some()
                && let Err(error) = store
                    .set_launch_error(&launch.hash, &launch.indexer, None)
                    .await
            {
                tracing::warn!(indexer = launch.indexer, "erro antigo não limpo: {error}");
            }
            return (Some(seeded), None);
        }
        Err(error) => format!(
            "marcado como enviado, mas o .torrent não entrou no cliente para semear: {error:#}"
        ),
    };
    tracing::warn!(indexer = launch.indexer, "{message}");
    if let Err(error) = store
        .set_launch_error(&launch.hash, &launch.indexer, Some(&message))
        .await
    {
        tracing::warn!(
            indexer = launch.indexer,
            "erro do pacote não gravado: {error}"
        );
    }
    (None, Some(message))
}

/// Recebe o `.torrent` que o site entregou (ele pode trocar o hash do gerado)
/// e o semeia no lugar. O corpo é segredo (leva o announce): nada dele é logado
/// nem devolvido.
async fn site_torrent(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> WebResult {
    let store = enter_live(&web, &headers, &Method::POST).await?;
    let (hash, _) = pair(&hash, &indexer).map_err(fail)?;
    let _claim = own_pair(&hash, &indexer)?;
    let bytes = body.map_err(|rejection| {
        let status = rejection.status();
        let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
            "o .torrent passa de 10 MiB"
        } else {
            "não consegui ler o corpo da requisição"
        };
        fail(WebError(status, message.into()))
    })?;
    let launch = store
        .launch(&hash, &indexer)
        .await
        .map_err(|e| fail(bad(e)))?
        .ok_or_else(|| not_found("prepare o pacote antes de enviar o .torrent do site"))?;
    if !matches!(launch.state, LaunchState::Pronto | LaunchState::Enviado) {
        return Err(conflict(
            "o pacote não está pronto nem enviado: não há o que semear",
        ));
    }
    if launch.sent_at.is_none() {
        let checks = store.cross_seed_checks().await.map_err(|e| fail(bad(e)))?;
        if already_there(&checks, &hash, &indexer) {
            return Err(conflict(
                "o release já existe no destino: não há o que enviar",
            ));
        }
    }
    let Some(site_layout) = acervo_clients::layout(&bytes) else {
        return Err(fail(WebError(
            StatusCode::BAD_REQUEST,
            "o corpo não é um .torrent válido".into(),
        )));
    };
    let site_hash = acervo_clients::info_hash(&bytes).ok_or_else(|| {
        fail(WebError(
            StatusCode::BAD_REQUEST,
            "o corpo não é um .torrent válido".into(),
        ))
    })?;
    let expected = super::package_layout(&web.config(), &launch)
        .await
        .map_err(|e| fail(anyhow_bad(&e)))?;
    if !crate::crossseed::same_layout(&expected, &site_layout) {
        return Err(fail(WebError(
            StatusCode::BAD_REQUEST,
            format!(
                "o .torrent do site não tem os arquivos do pacote: {}",
                super::layout_diff(&expected, &site_layout)
            ),
        )));
    }
    // O estado que impede a limpeza da pasta (enviado, com o hash do site) vai
    // ao banco antes de qualquer coisa no cliente: uma queda entre os dois
    // deixa o pacote protegido, nunca um torrent semeando de pasta descartável.
    store
        .save_launch_site_hash(&hash, &indexer, &site_hash)
        .await
        .map_err(|e| fail(bad(e)))?;
    if launch.sent_at.is_none() {
        mark_sent(store, &hash, &indexer).await?;
    }
    // Falha ao adicionar não desfaz o envio: vira erro do pacote e aviso.
    let (seeded, warning) = seed_or_warn(&web, store, &launch, Ok(bytes.to_vec())).await;
    ok(&json!({
        "ok": true,
        "estado": LaunchState::Enviado.as_str(),
        "adicionado": seeded == Some(super::Seeded::Added),
        "aviso": warning,
    }))
}

async fn discard(
    State(web): Shared,
    headers: HeaderMap,
    Path((hash, indexer)): Path<(String, String)>,
) -> WebResult {
    let store = enter_live(&web, &headers, &Method::POST).await?;
    let (hash, _) = pair(&hash, &indexer).map_err(fail)?;
    let _claim = own_pair(&hash, &indexer)?;
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
        // O irmão semeia da pasta do pacote: descartar a deixaria órfã.
        Some(LaunchState::Semeando) => {
            return Err(conflict(
                "o pacote está semeando no site: não dá para descartar",
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
    // A pasta adaptada sai junto, pela limpeza segura; o que ficar vira aviso.
    let warning = match &launch {
        Some(launch) if launch.adapted_path.is_some() => {
            super::discard_folder(&web.config(), store, launch).await
        }
        _ => None,
    };
    ok(&json!({
        "ok": true,
        "estado": LaunchState::Descartado.as_str(),
        "aviso": warning,
    }))
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
            adapted_path: None,
            adapted_layout: None,
            site_hash: None,
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

    #[test]
    fn nome_do_txt_sai_do_complete_name_sem_extensao() {
        let relatorio = "General\nUnique ID : 1\nComplete name                            : Filme.2024.1080p.WEB-DL.mkv\nFormat : Matroska\n";
        assert_eq!(
            mediainfo_stem(relatorio, Some("Outro")),
            "Filme.2024.1080p.WEB-DL"
        );
        // Sem "Complete name": o release; sem nenhum dos dois, um nome fixo.
        assert_eq!(
            mediainfo_stem("General\n", Some(" Release.X ")),
            "Release.X"
        );
        assert_eq!(mediainfo_stem("General\n", None), "mediainfo");
        // Caminho que escapasse do relatório não vira diretório no nome.
        assert_eq!(
            mediainfo_stem("Complete name : /srv/x/Filme.mkv\n", None),
            "Filme"
        );
        assert_eq!(
            attachment("Coração.txt"),
            "attachment; filename=\"Cora__o.txt\"; filename*=UTF-8''Cora%C3%A7%C3%A3o.txt"
        );
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
        // O relatório do MediaInfo: `.txt` com nome do vídeo, mesma auth e
        // cabeçalhos; sem relatório, 404.
        let (status, _) = call(&http, get.clone(), &url(&pronto, "/mediainfo.txt")).await;
        assert_eq!(status, 404);
        let relatorio =
            "General\nComplete name                : Filme Çá.2024.mkv\nFormat : Matroska";
        store
            .save_launch_fields(
                &pronto,
                BJ,
                &json!({ "release": "Filme.2024.1080p", "mediainfo": relatorio }),
            )
            .await
            .unwrap();
        let sem_chave = http
            .get(url(&pronto, "/mediainfo.txt"))
            .send()
            .await
            .unwrap();
        assert_ne!(sem_chave.status().as_u16(), 200);
        let response = http
            .get(url(&pronto, "/mediainfo.txt"))
            .header("X-Api-Key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let headers = response.headers().clone();
        assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
        let disposition = headers["content-disposition"].to_str().unwrap();
        assert!(
            disposition.starts_with("attachment; filename=\"Filme __.2024.txt\""),
            "{disposition}"
        );
        assert!(disposition.ends_with("filename*=UTF-8''Filme%20%C3%87%C3%A1.2024.txt"));
        assert!(headers.contains_key("content-security-policy"));
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(response.text().await.unwrap(), format!("{relatorio}\n"));
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

        // Enviado não se prepara de novo: o publicado semeia destes arquivos (e
        // da pasta adaptada, que preparar apagaria).
        let (status, body) = call(&http, post.clone(), &url(&pronto, "/preparar")).await;
        assert_eq!(status, 409, "{body}");
        assert!(body["erro"].as_str().unwrap().contains("enviado"), "{body}");

        // Descartar um pacote com pasta adaptada sem a origem para conferir:
        // descarta, a pasta fica e a resposta avisa.
        let adaptado = "9".repeat(40);
        let pasta = std::env::temp_dir().join(format!("acervo-descarte-{}", std::process::id()));
        let pasta_texto = pasta.to_str().unwrap().to_owned();
        std::fs::create_dir_all(&pasta).unwrap();
        store
            .upsert_launch_state(&adaptado, BJ, LaunchState::Pronto, None, None)
            .await
            .unwrap();
        store
            .save_launch_adaptation(&adaptado, BJ, Some(&pasta_texto), None)
            .await
            .unwrap();
        let (status, body) = call(&http, post.clone(), &url(&adaptado, "/descartar")).await;
        let _ = std::fs::remove_dir_all(&pasta);
        assert_eq!(status, 200, "{body}");
        assert!(
            body["aviso"].as_str().unwrap().contains(&pasta_texto),
            "{body}"
        );
        let launch = store.launch(&adaptado, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Descartado);
        assert_eq!(launch.adapted_path.as_deref(), Some(pasta_texto.as_str()));
        let (_, detail) = call(&http, get.clone(), &url(&adaptado, "")).await;
        assert_eq!(detail["pasta_adaptada"], pasta_texto.as_str());

        // Pasta adaptada que já não existe: não há aviso falso de "ficou no
        // disco" (nem o cliente é consultado) e a coluna é zerada.
        let sumido = "8".repeat(40);
        store
            .upsert_launch_state(&sumido, BJ, LaunchState::Pronto, None, None)
            .await
            .unwrap();
        store
            .save_launch_adaptation(&sumido, BJ, Some("/media/lancamentos/bj/Filme"), None)
            .await
            .unwrap();
        let (status, body) = call(&http, post.clone(), &url(&sumido, "/descartar")).await;
        assert_eq!(status, 200, "{body}");
        assert!(
            body.get("aviso").is_none_or(serde_json::Value::is_null),
            "{body}"
        );
        let launch = store.launch(&sumido, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Descartado);
        assert_eq!(launch.adapted_path, None);

        // Semeando não se prepara de novo.
        store
            .set_launch_state(&pronto, BJ, LaunchState::Semeando)
            .await
            .unwrap();
        let (status, _) = call(&http, post.clone(), &url(&pronto, "/preparar")).await;
        assert_eq!(status, 409);
        db.drop().await;
    }

    /// O `.torrent` de `root` com os `files`; `salt` muda o hash sem mudar os
    /// arquivos. O announce leva a passkey, que nada pode devolver.
    fn torrent(root: &str, files: &[(&str, u64)], salt: &str) -> Vec<u8> {
        let announce = "https://t.exemplo/announce?passkey=PASSKEY123";
        let mut out = format!("d8:announce{}:{announce}4:infod5:filesl", announce.len());
        for (path, size) in files {
            let _ = write!(out, "d6:lengthi{size}e4:pathl{}:{path}ee", path.len());
        }
        let _ = write!(
            out,
            "e4:name{}:{root}6:pieces{}:{salt}ee",
            root.len(),
            salt.len()
        );
        out.into_bytes()
    }

    /// Um qBittorrent de mentira: login, o que há no cliente, os arquivos da
    /// origem e os trackers de cada hash.
    async fn qbit(
        origin: &str,
        present: &[&str],
        adds: u64,
        add_status: u16,
    ) -> wiremock::MockServer {
        qbit_slow(origin, present, adds, add_status, std::time::Duration::ZERO).await
    }

    /// O mesmo, com o `add` demorando `delay` para responder.
    async fn qbit_slow(
        origin: &str,
        present: &[&str],
        adds: u64,
        add_status: u16,
        delay: std::time::Duration,
    ) -> wiremock::MockServer {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let mut torrents = vec![json!({
            "hash": origin, "name": "Raiz", "state": "stalledUP",
            "save_path": "/downloads/filmes", "category": "acervo", "progress": 1.0,
        })];
        torrents.extend(present.iter().map(|hash| {
            json!({
                "hash": hash, "name": "Raiz", "state": "stoppedUP",
                "save_path": "/downloads/filmes", "category": "acervo", "progress": 1.0,
            })
        }));
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(torrents))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "index": 0, "name": "Raiz/Filme.mkv", "size": 100, "progress": 1.0, "priority": 1 },
                { "index": 1, "name": "Raiz/Filme.srt", "size": 5, "progress": 1.0, "priority": 1 },
            ])))
            .mount(&server)
            .await;
        // Parado, na categoria do acervo, na pasta da origem e com as duas
        // tags: a da conferência e a de semear do destino.
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .and(body_string_contains("name=\"stopped\"\r\n\r\ntrue"))
            .and(body_string_contains("name=\"category\"\r\n\r\nacervo"))
            .and(body_string_contains(
                "name=\"savepath\"\r\n\r\n/downloads/filmes",
            ))
            .and(body_string_contains(format!(
                "name=\"tags\"\r\n\r\nacervo:cross-seed,acervo:semear:{BJ}"
            )))
            .respond_with(
                ResponseTemplate::new(add_status)
                    .set_body_string("Ok.")
                    .set_delay(delay),
            )
            .expect(adds)
            .mount(&server)
            .await;
        // O torrent que já está no cliente é adotado (tag, parar e conferir): o
        // resto dos POSTs o cliente aceita, e o tracker é o do destino.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(10)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/trackers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "url": "https://t.exemplo/announce?passkey=PASSKEY123", "status": 2, "msg": "" },
            ])))
            .with_priority(10)
            .mount(&server)
            .await;
        server
    }

    async fn serve(db: &acervo_store::testing::TestDb) -> (Arc<Web>, String) {
        let settings = Arc::new(
            crate::settings::Settings::load(db.store.clone())
                .await
                .unwrap(),
        );
        settings
            .save_section(crate::config::SERVIDOR, json!({ "api_key": KEY }))
            .await
            .unwrap();
        let web = Arc::new(Web {
            settings,
            database: crate::serve::Database::connected(db.store.clone()),
            catalog: crate::api::Catalog::default(),
            searches: tokio::sync::Mutex::default(),
            series_searches: tokio::sync::Mutex::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}/ui/api/biblioteca/para-lancar",
            listener.local_addr().unwrap()
        );
        let app = router(Arc::clone(&web));
        tokio::spawn(async move { axum::serve(listener, app).await });
        (web, base)
    }

    async fn point_to(web: &Web, server: &wiremock::MockServer) {
        web.settings
            .save_section(
                crate::config::QBITTORRENT,
                json!({ "url": server.uri(), "username": "u", "password": "s" }),
            )
            .await
            .unwrap();
    }

    async fn send_torrent(http: &reqwest::Client, url: &str, body: Vec<u8>) -> (u16, String) {
        let response = http
            .post(url)
            .header("X-Api-Key", KEY)
            .header("X-Acervo", "1")
            .header("Content-Type", "application/x-bittorrent")
            .body(body)
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.text().await.unwrap())
    }

    async fn insert_bj(store: &Store) {
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
    }

    async fn ready(store: &Store, hash: &str, generated: &[u8]) {
        store
            .upsert_launch_state(hash, BJ, LaunchState::Pronto, None, None)
            .await
            .unwrap();
        store
            .save_launch_torrent(hash, BJ, generated)
            .await
            .unwrap();
    }

    const FILES: [(&str, u64); 2] = [("Filme.mkv", 100), ("Filme.srt", 5)];

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta, pela API.
    async fn enviado_semeia_o_torrent_gerado_parado_sem_duplicar_e_avisa_se_falhar() {
        let Some(db) = acervo_store::testing::TestDb::new("para_lancar_semear").await else {
            return;
        };
        let store = &db.store;
        let (web, base) = serve(&db).await;
        insert_bj(store).await;
        let http = reqwest::Client::new();
        let post = reqwest::Method::POST;
        let generated = torrent("Raiz", &FILES, "");
        let generated_hash = acervo_clients::info_hash(&generated).unwrap();
        let origin = "5".repeat(40);
        let url = |hash: &str, action: &str| format!("{base}/{hash}/{BJ}{action}");

        // O cliente não tem o gerado: entra uma vez, parado e com as tags.
        let server = qbit(&origin, &[], 1, 200).await;
        point_to(&web, &server).await;
        ready(store, &origin, &generated).await;
        let (status, body) = call(&http, post.clone(), &url(&origin, "/enviado")).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["aviso"], Value::Null, "{body}");
        let launch = store.launch(&origin, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Enviado);
        assert_eq!(launch.error, None);
        assert!(!body.to_string().contains("PASSKEY123"));
        drop(server);

        // O gerado já está no cliente (mesmo hash): não adiciona de novo,
        // nem em um segundo "enviado" do mesmo pacote.
        let server = qbit(&origin, &[&generated_hash], 0, 200).await;
        point_to(&web, &server).await;
        let (status, body) = call(&http, post.clone(), &url(&origin, "/enviado")).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["aviso"], Value::Null, "{body}");
        drop(server);

        // O cliente recusa: o pacote continua enviado, com o erro gravado e o
        // aviso na resposta (e no detalhe).
        let other = "6".repeat(40);
        let server = qbit(&other, &[], 1, 500).await;
        point_to(&web, &server).await;
        ready(store, &other, &generated).await;
        let (status, body) = call(&http, post.clone(), &url(&other, "/enviado")).await;
        assert_eq!(status, 200, "{body}");
        let aviso = body["aviso"].as_str().unwrap();
        assert!(aviso.contains("não entrou no cliente"), "{body}");
        assert!(!body.to_string().contains("PASSKEY123"));
        let launch = store.launch(&other, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Enviado);
        assert!(launch.sent_at.is_some());
        assert_eq!(launch.error.as_deref(), Some(aviso));
        drop(server);

        // Sem cliente configurado também é aviso, não falha do envio.
        let none = "7".repeat(40);
        web.settings
            .save_section(crate::config::QBITTORRENT, json!({}))
            .await
            .unwrap();
        ready(store, &none, &generated).await;
        let (status, body) = call(&http, post.clone(), &url(&none, "/enviado")).await;
        assert_eq!(status, 200, "{body}");
        assert!(
            body["aviso"].as_str().unwrap().contains("cliente"),
            "{body}"
        );
        assert_eq!(
            store.launch(&none, BJ).await.unwrap().unwrap().state,
            LaunchState::Enviado
        );
        db.drop().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta, pela API.
    async fn torrent_do_site_confere_o_layout_o_limite_e_mostra_o_tracker_sem_url() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        let Some(db) = acervo_store::testing::TestDb::new("para_lancar_torrent_do_site").await
        else {
            return;
        };
        let store = &db.store;
        let (web, base) = serve(&db).await;
        insert_bj(store).await;
        let http = reqwest::Client::new();
        let origin = "1".repeat(40);
        let url = |action: &str| format!("{base}/{origin}/{BJ}{action}");
        let generated = torrent("Raiz", &FILES, "");
        ready(store, &origin, &generated).await;
        let route = url("/torrent-do-site");

        // Sem chave e sem o cabeçalho da tela, nada.
        assert_eq!(
            http.post(&route)
                .body("x")
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            401
        );
        let sem_cabecalho = http
            .post(&route)
            .header("X-Api-Key", KEY)
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(sem_cabecalho.status().as_u16(), 403);

        let server = qbit(&origin, &[], 1, 200).await;
        point_to(&web, &server).await;

        // Lixo, e layout diferente: 400 dizendo o que difere, sem passkey, e
        // nada muda no pacote.
        let (status, body) = send_torrent(&http, &route, b"nao e torrent".to_vec()).await;
        assert_eq!(status, 400, "{body}");
        let diferente = torrent("Raiz", &[("Filme.mkv", 99), ("Extra.nfo", 1)], "x");
        let (status, body) = send_torrent(&http, &route, diferente).await;
        assert_eq!(status, 400, "{body}");
        assert!(body.contains("Raiz/Filme.srt"), "{body}");
        assert!(body.contains("Raiz/Extra.nfo"), "{body}");
        assert!(
            body.contains("Raiz/Filme.mkv (pacote 100 B, torrent 99 B)"),
            "{body}"
        );
        assert!(
            !body.contains("PASSKEY123") && !body.contains("t.exemplo"),
            "{body}"
        );
        let launch = store.launch(&origin, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Pronto);
        assert_eq!(launch.sent_at, None);
        assert_eq!(launch.site_hash, None);

        // Acima de 10 MiB, 413 com a mensagem; o limite não vale só para o
        // corpo inteiro lido.
        let (status, body) = send_torrent(&http, &route, vec![b'd'; (10 << 20) + 1]).await;
        assert_eq!(status, 413, "{body}");
        assert!(body.contains("10 MiB"), "{body}");

        // Layout igual (o hash muda): entra parado, o pacote passa a enviado
        // e o hash do site fica guardado; o announce não volta.
        let do_site = torrent("Raiz", &FILES, "site");
        let site_hash = acervo_clients::info_hash(&do_site).unwrap();
        assert_ne!(site_hash, acervo_clients::info_hash(&generated).unwrap());
        let (status, body) = send_torrent(&http, &route, do_site).await;
        assert_eq!(status, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["estado"], "enviado");
        assert_eq!(value["adicionado"], true);
        assert!(
            !body.contains("PASSKEY123") && !body.contains("t.exemplo"),
            "{body}"
        );
        let launch = store.launch(&origin, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Enviado);
        assert!(launch.sent_at.is_some());
        assert_eq!(launch.site_hash.as_deref(), Some(site_hash.as_str()));
        drop(server);

        // Status do tracker no detalhe: do torrent do site quando ele está no
        // cliente; texto, sem URL nem passkey.
        let server = qbit(&origin, &[&site_hash], 0, 200).await;
        point_to(&web, &server).await;
        let trackers = |status: i64, msg: &str| {
            ResponseTemplate::new(200).set_body_json(json!([
                { "url": "** [DHT] **", "status": 0, "msg": "" },
                { "url": "https://t.exemplo/announce?passkey=PASSKEY123", "status": status, "msg": msg },
            ]))
        };
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/trackers"))
            .and(query_param("hash", site_hash.as_str()))
            .respond_with(trackers(4, "Torrent not registered with this tracker"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/trackers"))
            .and(query_param("hash", site_hash.as_str()))
            .respond_with(trackers(2, ""))
            .mount(&server)
            .await;
        let (status, detail) = call(&http, reqwest::Method::GET, &url("")).await;
        assert_eq!(status, 200, "{detail}");
        assert_eq!(detail["status_tracker"], "não registrado");
        assert_eq!(detail["torrent_do_site"], true);
        assert!(!detail.to_string().contains("PASSKEY123"));
        assert!(!detail.to_string().contains("t.exemplo"));
        let (_, detail) = call(&http, reqwest::Method::GET, &url("")).await;
        assert_eq!(detail["status_tracker"], "funcionando");
        drop(server);

        // O mesmo `.torrent` do site, de novo: já está no cliente, não duplica.
        let server = qbit(&origin, &[&site_hash], 0, 200).await;
        point_to(&web, &server).await;
        let (status, body) = send_torrent(&http, &route, torrent("Raiz", &FILES, "site")).await;
        assert_eq!(status, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["adicionado"], false);

        // Pacote que ainda não está pronto nem enviado não recebe `.torrent`.
        let falhou = "2".repeat(40);
        store
            .upsert_launch_state(&falhou, BJ, LaunchState::Falhou, None, Some("x"))
            .await
            .unwrap();
        let (status, _) = send_torrent(
            &http,
            &format!("{base}/{falhou}/{BJ}/torrent-do-site"),
            torrent("Raiz", &FILES, ""),
        )
        .await;
        assert_eq!(status, 409);
        db.drop().await;
    }

    #[tokio::test]
    async fn torrent_do_site_grava_o_estado_antes_de_adicionar_e_serializa_o_par() {
        use std::time::Duration;

        let Some(db) = acervo_store::testing::TestDb::new("para_lancar_serializa").await else {
            return;
        };
        let store = &db.store;
        let (web, base) = serve(&db).await;
        insert_bj(store).await;
        let http = reqwest::Client::new();
        let origin = "3".repeat(40);
        let url = |action: &str| format!("{base}/{origin}/{BJ}{action}");
        let generated = torrent("Raiz", &FILES, "");
        ready(store, &origin, &generated).await;
        let route = url("/torrent-do-site");

        // O cliente demora a aceitar o `.torrent`: na janela, o banco já diz
        // `enviado` com o hash do site, e preparar, descartar e enviado do mesmo
        // par levam 409 sem mexer em nada.
        let do_site = torrent("Raiz", &FILES, "site");
        let site_hash = acervo_clients::info_hash(&do_site).unwrap();
        let server = qbit_slow(&origin, &[], 1, 200, Duration::from_millis(900)).await;
        point_to(&web, &server).await;
        let upload = tokio::spawn({
            let (http, route) = (http.clone(), route.clone());
            async move { send_torrent(&http, &route, do_site).await }
        });
        let mut persisted = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let launch = store.launch(&origin, BJ).await.unwrap().unwrap();
            if launch.state == LaunchState::Enviado {
                assert_eq!(launch.site_hash.as_deref(), Some(site_hash.as_str()));
                assert!(launch.sent_at.is_some());
                persisted = true;
                break;
            }
        }
        assert!(
            persisted,
            "o estado devia estar gravado antes de o add voltar"
        );
        assert!(
            !upload.is_finished(),
            "o add ainda devia estar em andamento"
        );
        for action in ["/preparar", "/descartar", "/enviado", "/torrent-do-site"] {
            let (status, body) = if action == "/torrent-do-site" {
                send_torrent(&http, &url(action), torrent("Raiz", &FILES, "site")).await
            } else {
                let (status, body) = call(&http, reqwest::Method::POST, &url(action)).await;
                (status, body.to_string())
            };
            assert_eq!(status, 409, "{action}: {body}");
        }
        let (status, body) = upload.await.unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"adicionado\":true"), "{body}");
        // Sem a posse do par solta, as rotas voltam a responder.
        let (status, _) = call(&http, reqwest::Method::POST, &url("/descartar")).await;
        assert_eq!(status, 409, "enviado ao site não se descarta");
        drop(server);

        // O cliente recusa o `.torrent`: o estado gravado fica (enviado, com o
        // hash do site), o erro vai ao pacote e à resposta, sem a passkey.
        let other = "4".repeat(40);
        ready(store, &other, &generated).await;
        let server = qbit(&other, &[], 1, 500).await;
        point_to(&web, &server).await;
        let route = format!("{base}/{other}/{BJ}/torrent-do-site");
        let (status, body) = send_torrent(&http, &route, torrent("Raiz", &FILES, "site")).await;
        assert_eq!(status, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["adicionado"], false);
        let aviso = value["aviso"].as_str().unwrap();
        assert!(aviso.contains("não entrou no cliente"), "{body}");
        assert!(!body.contains("PASSKEY123") && !body.contains("t.exemplo"));
        let launch = store.launch(&other, BJ).await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Enviado);
        assert_eq!(launch.site_hash.as_deref(), Some(site_hash.as_str()));
        assert_eq!(launch.error.as_deref(), Some(aviso));
        db.drop().await;
    }

    #[test]
    fn numero_da_captura_so_aceita_1_a_4_em_forma_canonica() {
        for (file, want) in [
            ("1.png", Some(1)),
            ("4.png", Some(4)),
            ("0.png", None),
            ("5.png", None),
            ("01.png", None),
            ("+1.png", None),
            ("1.jpg", Some(1)),
            ("3.jpg", Some(3)),
            ("1", Some(1)),
            ("4", Some(4)),
            ("5", None),
            ("01", None),
            ("1.gif", None),
            ("1.png.jpg", None),
            ("../1.png", None),
            ("", None),
        ] {
            assert_eq!(capture_number(file), want, "{file}");
        }
        assert_eq!(capture_stem("Raiz/Sub/Filme Çá.2024.mkv"), "Filme Çá.2024");
        assert_eq!(capture_stem("Raiz\\a\nb.mkv"), "a_b");
        assert_eq!(capture_stem(".mkv"), "captura");
    }

    /// Um qBittorrent de mentira com um torrent em `save_path` e estes arquivos.
    async fn qbit_with_files(hash: &str, save_path: &str, files: &[&str]) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "hash": hash, "name": "Raiz", "state": "stalledUP",
                "save_path": save_path, "category": "acervo", "progress": 1.0,
            }])))
            .mount(&server)
            .await;
        let list: Vec<Value> = files
            .iter()
            .enumerate()
            .map(|(index, name)| {
                json!({ "index": index, "name": name, "size": 100, "progress": 1.0, "priority": 1 })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(list))
            .mount(&server)
            .await;
        server
    }

    /// Um `.mkv` de 4 s e 320x240, ou `None` sem ffmpeg na máquina.
    fn small_video(path: &std::path::Path) -> Option<()> {
        let ffmpeg = std::env::var("ACERVO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        std::process::Command::new(ffmpeg)
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=4:size=320x240:rate=10")
            .args(["-c:v", "mpeg4"])
            .arg(path)
            .status()
            .is_ok_and(|s| s.success())
            .then_some(())
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta, pela API.
    async fn captura_para_baixar_sai_na_hora_em_resolucao_original_sem_gravar_nada() {
        let Some(db) = acervo_store::testing::TestDb::new("para_lancar_capturas").await else {
            return;
        };
        let store = &db.store;
        let (web, base) = serve(&db).await;
        let http = reqwest::Client::new();
        let hash = "c".repeat(40);
        let url = |n: &str| format!("{base}/{hash}/{BJ}/capturas/{n}");
        let get = |n: &str| http.get(url(n)).header("X-Api-Key", KEY);

        // Auth: sem a chave, nada.
        assert_eq!(
            http.get(url("1.png"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            401
        );
        // Sem pacote, 404; número fora de 1 a 4, 404.
        let (status, _) = call(&http, reqwest::Method::GET, &url("1.png")).await;
        assert_eq!(status, 404);
        store
            .upsert_launch_state(&hash, BJ, LaunchState::Pronto, None, None)
            .await
            .unwrap();
        for n in ["0.png", "5.png", "01.png", "5.jpg", "x.png", "1.gif"] {
            let (status, _) = call(&http, reqwest::Method::GET, &url(n)).await;
            assert_eq!(status, 404, "{n}");
        }
        // Cliente de download fora do ar: 502, não 500.
        let (status, _) = call(&http, reqwest::Method::GET, &url("1.png")).await;
        assert_eq!(status, 502);

        // Torrent que saiu do cliente; torrent sem vídeo: 404.
        let server = qbit_with_files(&"9".repeat(40), "/nada", &["Raiz/Filme.mkv"]).await;
        point_to(&web, &server).await;
        let (status, _) = call(&http, reqwest::Method::GET, &url("1.png")).await;
        assert_eq!(status, 404);
        let server = qbit_with_files(&hash, "/nada", &["Raiz/Filme.nfo", "Raiz/Filme.srt"]).await;
        point_to(&web, &server).await;
        let (status, body) = call(&http, reqwest::Method::GET, &url("1.png")).await;
        assert_eq!(status, 404);
        assert!(body.to_string().contains("não tem vídeo"), "{body}");
        // Vídeo que não está no disco: 404.
        let server = qbit_with_files(&hash, "/nada", &["Raiz/Filme.mkv"]).await;
        point_to(&web, &server).await;
        let (status, _) = call(&http, reqwest::Method::GET, &url("1.png")).await;
        assert_eq!(status, 404);

        // Com ffmpeg na máquina: o quadro de verdade.
        let dir = std::env::temp_dir().join(format!("acervo-capturas-{}", std::process::id()));
        let content = dir.join("Raiz");
        std::fs::create_dir_all(&content).unwrap();
        let video = content.join("Filme Çá.mkv");
        if small_video(&video).is_some() {
            let server = qbit_with_files(
                &hash,
                dir.to_str().unwrap(),
                &["Raiz/Filme Çá.mkv", "Raiz/Sample/amostra.mkv"],
            )
            .await;
            point_to(&web, &server).await;
            let before: Vec<_> = std::fs::read_dir(&content).unwrap().flatten().collect();
            // Os 100 bytes de cada arquivo empatam o "maior": vale o primeiro
            // pelo caminho, `Raiz/Filme Çá.mkv`.
            let duration = crate::mediainfo::probe_full(&video)
                .await
                .and_then(|m| m.duration_seconds)
                .expect("ffprobe");
            for number in 1..=super::super::SCREEN_COUNT {
                let response = get(&format!("{number}.png")).send().await.unwrap();
                assert_eq!(response.status().as_u16(), 200, "captura {number}");
                let headers = response.headers().clone();
                assert_eq!(headers["content-type"], "image/png");
                let disposition = headers["content-disposition"].to_str().unwrap();
                assert!(
                    disposition.starts_with(&format!(
                        "attachment; filename=\"Filme __.captura-{number}.png\""
                    )),
                    "{disposition}"
                );
                assert!(disposition.ends_with(&format!(
                    "filename*=UTF-8''Filme%20%C3%87%C3%A1.captura-{number}.png"
                )));
                assert!(headers.contains_key("content-security-policy"));
                assert_eq!(headers["x-content-type-options"], "nosniff");
                assert_eq!(headers["cache-control"], "no-store");
                let png = response.bytes().await.unwrap();
                assert!(png.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]));
                // IHDR: a largura e a altura reais, sem escala.
                assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 320);
                assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 240);
                // Mesmo instante do preparo: o mesmo quadro, byte a byte.
                let at = super::super::screen_time(duration, number).unwrap();
                let (expected, format) = crate::mediainfo::screenshot(&video, at).await.unwrap();
                assert_eq!(format, crate::mediainfo::CaptureFormat::Png);
                assert_eq!(png.as_ref(), expected.as_slice(), "captura {number}");
                // Sem extensão ou com .jpg: a mesma imagem, no formato gerado.
                for alias in [number.to_string(), format!("{number}.jpg")] {
                    let response = get(&alias).send().await.unwrap();
                    assert_eq!(response.status().as_u16(), 200, "{alias}");
                    assert_eq!(response.headers()["content-type"], "image/png");
                    assert!(
                        response.headers()["content-disposition"]
                            .to_str()
                            .unwrap()
                            .contains(&format!(".captura-{number}.png\""))
                    );
                    assert_eq!(
                        response.bytes().await.unwrap().as_ref(),
                        expected.as_slice()
                    );
                }
            }
            // A duração gravada no laudo manda: 400 s de duração levam ao fim
            // do vídeo de 4 s, que não tem quadro, e a rota diz que falhou.
            store
                .save_launch_report(&hash, BJ, &json!({ "capturas": { "duracao": 4000.0 } }))
                .await
                .unwrap();
            let (status, body) = call(&http, reqwest::Method::GET, &url("1.png")).await;
            assert_eq!(status, 502, "{body}");
            assert!(!body.to_string().contains(dir.to_str().unwrap()), "{body}");
            // Nada foi gravado ao lado do vídeo.
            let after: Vec<_> = std::fs::read_dir(&content).unwrap().flatten().collect();
            assert_eq!(before.len(), after.len());
        }
        let _ = std::fs::remove_dir_all(&dir);
        db.drop().await;
    }
}

//! Tempo real: o barramento de tópicos e o canal de eventos (SSE) que avisa
//! a tela de que algo mudou.
//!
//! Quem muda algo — uma requisição da interface, o fim de uma tarefa, o job
//! de lançamento — emite tópicos no [`Bus`]. Cada conexão em
//! `GET /ui/api/eventos` junta o que chega dentro de [`DEBOUNCE`] numa só
//! mensagem `mudanca`; a tela invalida as consultas daquele tópico.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::{self, StreamExt};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::api::secure_headers;
use crate::tasks::Cancel;

pub const QUEUE: &str = "fila";
pub const MOVIES: &str = "filmes";
pub const SERIES: &str = "series";
pub const LAUNCH: &str = "lancar";
pub const TASKS: &str = "tarefas";
pub const INDEXERS: &str = "indexadores";
pub const SETTINGS: &str = "configuracoes";
pub const DELETE: &str = "apagar";
pub const DISCOVER: &str = "descobrir";
pub const HISTORY: &str = "historico";

/// Todos os tópicos, na ordem em que saem na mensagem.
pub const ALL: [&str; 10] = [
    QUEUE, MOVIES, SERIES, LAUNCH, TASKS, INDEXERS, SETTINGS, DELETE, DISCOVER, HISTORY,
];

/// Mudanças no mesmo tópico dentro desta janela viram uma só mensagem.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// Intervalo do comentário `: ping`: o túnel e o Access fecham conexão ociosa.
const KEEP_ALIVE: Duration = Duration::from_secs(20);

/// Capacidade folgada: assinante lento que perde mensagens recebe todos os
/// tópicos (`Lagged`) em vez de cair.
const CAPACITY: usize = 256;

/// O barramento: clones emitem e assinam o mesmo canal.
#[derive(Debug, Clone)]
pub struct Bus {
    sender: broadcast::Sender<&'static str>,
    /// Desligamento do serviço: sem ele, as conexões abertas seguram o
    /// `graceful_shutdown` do servidor para sempre.
    down: Cancel,
}

impl Default for Bus {
    fn default() -> Self {
        Self {
            sender: broadcast::channel(CAPACITY).0,
            down: Cancel::default(),
        }
    }
}

impl Bus {
    /// Avisa que `topics` mudaram. Sem ninguém ouvindo, não faz nada.
    pub fn emit(&self, topics: &[&'static str]) {
        for topic in topics {
            // `Err` é só "sem assinantes".
            let _ = self.sender.send(topic);
        }
    }

    /// Encerra as conexões abertas; vale para sempre.
    pub fn close(&self) {
        self.down.cancel();
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<&'static str> {
        self.sender.subscribe()
    }
}

/// Os tópicos que o fim de uma tarefa muda: o dela, e sempre `tarefas`.
#[must_use]
pub fn task_topics(id: &str) -> &'static [&'static str] {
    match id {
        "importacao" => &[TASKS, QUEUE, MOVIES, SERIES, HISTORY],
        "busca" | "rss" => &[TASKS, QUEUE, HISTORY],
        "limpeza" => &[TASKS, QUEUE],
        "cross-seed" => &[TASKS, LAUNCH],
        "disco" | "metadados" | "cena" => &[TASKS, MOVIES, SERIES],
        _ => &[TASKS],
    }
}

/// Os tópicos que uma requisição que muda estado afeta, pelo prefixo da
/// rota. Rota fora do mapa (entrar, sair) não emite nada.
#[must_use]
pub fn route_topics(path: &str) -> &'static [&'static str] {
    // Do mais específico para o mais geral; o prefixo vale em fronteira de
    // segmento.
    const MAP: [(&str, &[&str]); 18] = [
        ("/ui/api/biblioteca/filmes", &[MOVIES, QUEUE, HISTORY]),
        ("/ui/api/biblioteca/series", &[SERIES, QUEUE, HISTORY]),
        ("/ui/api/biblioteca/fila", &[QUEUE, MOVIES, SERIES, HISTORY]),
        ("/ui/api/biblioteca/historico", &[HISTORY]),
        ("/ui/api/biblioteca/bloqueados", &[QUEUE]),
        ("/ui/api/biblioteca/regras", &[SETTINGS]),
        ("/ui/api/biblioteca/notificacoes", &[SETTINGS]),
        ("/ui/api/biblioteca/para-apagar", &[DELETE, MOVIES, SERIES]),
        ("/ui/api/biblioteca/para-lancar", &[LAUNCH]),
        ("/ui/api/descobrir", &[DISCOVER]),
        ("/ui/api/indexadores", &[INDEXERS]),
        ("/ui/api/catalogo", &[INDEXERS]),
        ("/ui/api/tarefas", &[TASKS]),
        ("/ui/api/filmes/buscar", &[TASKS, MOVIES]),
        ("/ui/api/filmes", &[MOVIES, QUEUE, HISTORY]),
        ("/ui/api/configuracoes", &[SETTINGS]),
        // A busca dos que faltam e o calendário vivem nos filmes e séries.
        ("/ui/api/biblioteca/faltando", &[MOVIES, SERIES]),
        ("/ui/api/biblioteca/calendario", &[MOVIES, SERIES]),
    ];
    MAP.iter()
        .find(|(prefix, _)| {
            path.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .map_or(&[], |(_, topics)| *topics)
}

/// Middleware: resposta 2xx a requisição que muda estado em `/ui/api/*`
/// emite os tópicos da rota, já depois da resposta pronta.
pub async fn emit_after(State(bus): State<Bus>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let changes = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) && path.starts_with("/ui/api/");
    let response = next.run(request).await;
    if changes && response.status().is_success() {
        bus.emit(route_topics(&path));
    }
    response
}

/// Espera a próxima mensagem: o primeiro tópico que chega abre uma janela de
/// [`DEBOUNCE`], e tudo o que cair nela sai junto, sem repetir. `None` quando
/// o barramento fecha.
async fn next_batch(
    receiver: &mut broadcast::Receiver<&'static str>,
    window: Duration,
) -> Option<Vec<&'static str>> {
    let mut seen = BTreeSet::new();
    let mut take = |received: Result<&'static str, RecvError>| match received {
        Ok(topic) => {
            seen.insert(topic);
            true
        }
        // Perdeu mensagens: recarrega tudo.
        Err(RecvError::Lagged(_)) => {
            seen.extend(ALL);
            true
        }
        Err(RecvError::Closed) => false,
    };
    if !take(receiver.recv().await) {
        return None;
    }
    let deadline = Instant::now() + window;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            message = receiver.recv() => {
                if !take(message) {
                    break;
                }
            }
        }
    }
    Some(
        ALL.into_iter()
            .filter(|topic| seen.contains(topic))
            .collect(),
    )
}

/// A resposta de `GET /ui/api/eventos`: `ola` ao conectar e um `mudanca` a
/// cada lote de tópicos.
pub fn stream(bus: &Bus) -> Response {
    // Assina antes de responder: nada que mude depois daqui se perde.
    let receiver = bus.subscribe();
    let hello =
        stream::once(async { Ok::<_, Infallible>(Event::default().event("ola").data("{}")) });
    let changes = stream::unfold(
        (receiver, bus.down.clone()),
        |(mut receiver, down)| async move {
            let topics = tokio::select! {
                batch = next_batch(&mut receiver, DEBOUNCE) => batch?,
                () = down.cancelled() => return None,
            };
            let event = Event::default()
                .event("mudanca")
                .data(json!({ "topicos": topics }).to_string());
            Some((Ok(event), (receiver, down)))
        },
    );
    let mut response = Sse::new(hello.chain(changes))
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE).text("ping"))
        .into_response();
    let headers = response.headers_mut();
    secure_headers(headers);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::routing::{get, post};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn debounce_junta_o_que_cai_na_janela() {
        let bus = Bus::default();
        let mut receiver = bus.subscribe();
        bus.emit(&[MOVIES, QUEUE]);
        bus.emit(&[QUEUE]);
        bus.emit(&[QUEUE, MOVIES]);
        let batch = next_batch(&mut receiver, DEBOUNCE).await;
        // Sem repetição, na ordem dos tópicos.
        assert_eq!(batch, Some(vec![QUEUE, MOVIES]));

        // O que chega durante a janela entra nela; o que chega depois, não.
        let late = bus.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            late.emit(&[HISTORY]);
            tokio::time::sleep(Duration::from_millis(400)).await;
            late.emit(&[SERIES]);
        });
        bus.emit(&[QUEUE]);
        assert_eq!(
            next_batch(&mut receiver, DEBOUNCE).await,
            Some(vec![QUEUE, HISTORY])
        );
        assert_eq!(
            next_batch(&mut receiver, DEBOUNCE).await,
            Some(vec![SERIES])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn assinante_lento_recebe_todos_os_topicos() {
        let bus = Bus::default();
        let mut receiver = bus.subscribe();
        for _ in 0..=CAPACITY {
            bus.emit(&[QUEUE]);
        }
        assert_eq!(
            next_batch(&mut receiver, DEBOUNCE).await,
            Some(ALL.to_vec())
        );
    }

    fn app(bus: &Bus) -> Router {
        Router::new()
            .route(
                "/ui/api/biblioteca/filmes",
                post(|| async { "ok" }).get(|| async { "ok" }),
            )
            .route(
                "/ui/api/biblioteca/fila/1",
                axum::routing::delete(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route("/ui/api/entrar", post(|| async { "ok" }))
            .route("/fora", post(|| async { "ok" }))
            .route("/ui/api/sessao", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                bus.clone(),
                emit_after,
            ))
    }

    /// Sobe o roteador numa porta livre e devolve um cliente para ele.
    async fn serve(app: Router) -> impl Fn(Method, &str) -> reqwest::RequestBuilder {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let http = reqwest::Client::new();
        move |method, path| http.request(method, format!("{base}{path}"))
    }

    fn drain(receiver: &mut broadcast::Receiver<&'static str>) -> Vec<&'static str> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[tokio::test]
    async fn middleware_emite_para_post_e_nada_para_get() {
        let bus = Bus::default();
        let mut receiver = bus.subscribe();
        let call = serve(app(&bus)).await;

        call(Method::GET, "/ui/api/biblioteca/filmes")
            .send()
            .await
            .unwrap();
        call(Method::GET, "/ui/api/sessao").send().await.unwrap();
        assert!(drain(&mut receiver).is_empty());

        call(Method::POST, "/ui/api/biblioteca/filmes")
            .send()
            .await
            .unwrap();
        assert_eq!(drain(&mut receiver), [MOVIES, QUEUE, HISTORY]);

        // Resposta que não é 2xx, rota fora da interface e rota sem tópico.
        call(Method::DELETE, "/ui/api/biblioteca/fila/1")
            .send()
            .await
            .unwrap();
        call(Method::POST, "/fora").send().await.unwrap();
        call(Method::POST, "/ui/api/entrar").send().await.unwrap();
        assert!(drain(&mut receiver).is_empty());
    }

    #[test]
    fn prefixo_vale_so_em_fronteira_de_segmento() {
        assert_eq!(route_topics("/ui/api/tarefas/busca/rodar"), [TASKS]);
        assert_eq!(route_topics("/ui/api/filmes/buscar"), [TASKS, MOVIES]);
        assert_eq!(
            route_topics("/ui/api/filmes/3/pegar"),
            [MOVIES, QUEUE, HISTORY]
        );
        assert!(route_topics("/ui/api/tarefasx").is_empty());
        assert_eq!(
            route_topics("/ui/api/biblioteca/para-lancar/ab/bj/descartar"),
            [LAUNCH]
        );
    }

    #[test]
    fn tarefas_emitem_os_topicos_do_mapa() {
        assert_eq!(
            task_topics("importacao"),
            [TASKS, QUEUE, MOVIES, SERIES, HISTORY]
        );
        assert_eq!(task_topics("rss"), [TASKS, QUEUE, HISTORY]);
        assert_eq!(task_topics("cross-seed"), [TASKS, LAUNCH]);
        assert_eq!(task_topics("qualquer"), [TASKS]);
    }
}

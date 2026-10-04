//! "Mandar ao qBittorrent" da busca manual: o resultado vai ao cliente numa
//! categoria própria, fora das que a limpeza gerencia e de qualquer grab do
//! acervo — o torrent é de quem o mandou.

// Handler devolve a resposta de erro pronta, como em `web`.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use acervo_clients::{AddOptions, NewTorrent, QbitError};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::web::{Shared, Web, WebError, WebResult, anyhow_bad, bad, enter, fail, ok};

pub fn router(web: Arc<Web>) -> Router {
    Router::new()
        .route("/ui/api/busca/enviar", post(send))
        .with_state(web)
}

#[derive(Deserialize)]
struct Body {
    indexador: String,
    link: String,
}

/// `POST /ui/api/busca/enviar {indexador, link}`: baixa pelo indexador (o
/// mesmo caminho do download que os gerenciadores usam), manda ao cliente
/// já iniciado e devolve o hash.
async fn send(State(web): Shared, headers: HeaderMap, Json(body): Json<Body>) -> WebResult {
    let store = enter(&web, &headers, &Method::POST).await?;
    let config = web.config();
    let link = url::Url::parse(body.link.trim()).map_err(|_| fail(bad("link inválido")))?;
    let known = web.catalog.capabilities(&body.indexador).is_ok();
    if !known && link.scheme() != "magnet" {
        return Err(fail(WebError(
            StatusCode::NOT_FOUND,
            "indexador desconhecido".into(),
        )));
    }
    let (torrent, hash) = crate::grab::resolve(&web.catalog, &body.indexador, &link)
        .await
        .map_err(|error| fail(WebError(StatusCode::BAD_GATEWAY, format!("{error:#}"))))?;
    let magnet = matches!(torrent, NewTorrent::Magnet(_));
    let client = crate::grab::qbit(&config)
        .await
        .map_err(|error| fail(anyhow_bad(&error)))?;
    let category = config.library.manual_category.trim().to_owned();
    client
        .ensure_category(&category)
        .await
        .map_err(|error| fail(bad(format!("criando a categoria no qBittorrent: {error}"))))?;
    let added = client
        .add(
            torrent,
            &AddOptions {
                category: category.clone(),
                save_path: None,
                stopped: false,
                stop_after_metadata: false,
                tags: Vec::new(),
            },
        )
        .await;
    match added {
        Ok(()) => {}
        Err(QbitError::AddRefused) => {
            return Err(fail(WebError(
                StatusCode::CONFLICT,
                "o torrent já está no cliente".into(),
            )));
        }
        Err(error) => {
            return Err(fail(bad(format!(
                "mandando o torrent ao qBittorrent: {error}"
            ))));
        }
    }
    if known {
        crate::stats::grabbed(store, &body.indexador).await;
    }
    tracing::info!(
        indexer = body.indexador,
        categoria = category,
        "mandado à mão ao cliente"
    );
    ok(&json!({
        "ok": true,
        "hash": hash,
        "categoria": category,
        "magnet": magnet,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use acervo_api::{Catalog, Entry};
    use acervo_indexers::{CardigannClient, CardigannDefinition};
    use serde_json::Value;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::settings::Settings;

    const YAML: &str = include_str!("../../acervo-indexers/tests/fixtures/cardigann-public.yml");

    /// Um `.torrent` mínimo, com infohash calculável.
    const TORRENT: &[u8] = b"d8:announce16:http://t.invalid4:infod6:lengthi1e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";

    async fn qbit() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("set-cookie", "SID=abc; path=/")
                    .set_body_string("Ok."),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/createCategory"))
            .and(body_string_contains("category=manual"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário de ponta a ponta.
    async fn manda_torrent_e_magnet_na_categoria_manual_iniciados() {
        let Some(db) = acervo_store::testing::TestDb::new("busca_enviar").await else {
            return;
        };
        let tracker = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/42"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(TORRENT.to_vec(), "application/x-bittorrent"),
            )
            .expect(1)
            .mount(&tracker)
            .await;
        let client = qbit().await;
        // Sem `stopped` nem `paused`: entra andando, na categoria manual e
        // sem a tag da fila do acervo.
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .and(body_string_contains("manual"))
            .and(|request: &wiremock::Request| {
                let body = String::from_utf8_lossy(&request.body);
                !body.contains("stopped")
                    && !body.contains("paused")
                    && !body.contains("acervo:fila")
            })
            .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
            .expect(2)
            .mount(&client)
            .await;

        let settings = Arc::new(Settings::load(db.store.clone()).await.unwrap());
        settings
            .save_section(
                crate::config::QBITTORRENT,
                serde_json::json!({ "url": client.uri(), "username": "u", "password": "p" }),
            )
            .await
            .unwrap();
        settings
            .save_section(
                crate::config::SERVIDOR,
                serde_json::json!({ "api_key": "chave-de-teste-0123456789" }),
            )
            .await
            .unwrap();
        let yaml = YAML.replace("https://tracker.invalid/", &format!("{}/", tracker.uri()));
        let indexer = CardigannClient::new(
            CardigannDefinition::from_yaml_v11(&yaml).unwrap(),
            0,
            BTreeMap::new(),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let capabilities = indexer.capabilities().clone();
        let catalog = Catalog::new([Entry {
            indexer: Arc::new(indexer),
            capabilities,
        }])
        .unwrap();
        let web = Arc::new(Web {
            settings,
            database: crate::serve::Database::connected(db.store.clone()),
            catalog,
            accounts: None,
            searches: tokio::sync::Mutex::default(),
            series_searches: tokio::sync::Mutex::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(web)).await });
        let http = reqwest::Client::new();
        let call = |body: Value| {
            http.post(format!("{base}/ui/api/busca/enviar"))
                .header("X-Api-Key", "chave-de-teste-0123456789")
                .header("X-Acervo", "1")
                .json(&body)
                .send()
        };

        let response = call(serde_json::json!({
            "indexador": "arquivo-publico",
            "link": format!("{}/download/42", tracker.uri()),
        }))
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["categoria"], "manual");
        assert_eq!(body["magnet"], false);
        assert_eq!(
            body["hash"].as_str().unwrap(),
            acervo_clients::info_hash(TORRENT).unwrap()
        );

        let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        let body: Value =
            call(serde_json::json!({ "indexador": "arquivo-publico", "link": magnet }))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(body["magnet"], true);
        assert_eq!(body["hash"], "0123456789abcdef0123456789abcdef01234567");

        // Os dois envios contam como grab do indexador.
        let since = OffsetDateTimeDay::today();
        let stats = db.store.indexer_stats(&since).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].grabs, 2);

        let response =
            call(serde_json::json!({ "indexador": "outro", "link": "https://x.invalid/1" }))
                .await
                .unwrap();
        assert_eq!(response.status().as_u16(), 404);
        db.drop().await;
    }

    struct OffsetDateTimeDay;

    impl OffsetDateTimeDay {
        fn today() -> String {
            let date = time::OffsetDateTime::now_utc().date();
            format!(
                "{:04}-{:02}-{:02}",
                date.year(),
                u8::from(date.month()),
                date.day()
            )
        }
    }
}

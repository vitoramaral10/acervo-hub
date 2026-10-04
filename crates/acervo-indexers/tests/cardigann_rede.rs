//! Rede do executor Cardigann: o desafio do Cloudflare, vencido pelo
//! `FlareSolverr` ou recusado com o motivo, e o proxy por indexador.

use std::collections::BTreeMap;
use std::time::Duration;

use acervo_indexers::{
    CardigannClient, CardigannDefinition, Challenge, FlareSolverr, Indexer, IndexerError, Network,
    Proxy, SearchQuery,
};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PUBLIC_YAML: &str = include_str!("fixtures/cardigann-public.yml");
const PUBLIC_HTML: &str = include_str!("fixtures/cardigann-results.html");
const COOKIE_YAML: &str = include_str!("fixtures/cardigann-cookie.yml");
const COOKIE_HTML: &str = include_str!("fixtures/cardigann-cookie.html");

/// A página que o Cloudflare devolve no lugar do site.
const CHALLENGE: &str = r#"<!DOCTYPE html><html><head><title>Just a moment...</title></head>
<body><script>window._cf_chl_opt={cvId: '3'};</script>
<script src="/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=1"></script></body></html>"#;

const AGENT: &str = "Navegador/1.0 (desafio)";

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8")
}

fn challenge() -> ResponseTemplate {
    ResponseTemplate::new(403)
        .insert_header("cf-mitigated", "challenge")
        .insert_header("server", "cloudflare")
        .set_body_raw(CHALLENGE, "text/html; charset=utf-8")
}

fn client(yaml: &str, base: &str, settings: &[(&str, &str)], network: Network) -> CardigannClient {
    let yaml = yaml.replace("https://tracker.invalid/", &format!("{base}/"));
    CardigannClient::new(
        CardigannDefinition::from_yaml_v11(&yaml).unwrap(),
        0,
        settings
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>(),
        Duration::from_secs(5),
    )
    .unwrap()
    .with_network(network)
    .unwrap()
}

fn solver(server: &MockServer, challenge: Challenge) -> Network {
    Network {
        proxy: None,
        flaresolverr: Some(FlareSolverr::new(&server.uri(), Duration::from_secs(30)).unwrap()),
        challenge,
    }
}

/// O `FlareSolverr` que resolve: devolve o cookie `cf_clearance` e o
/// user-agent do navegador.
async fn mount_solver(server: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/v1"))
        .and(body_partial_json(json!({ "cmd": "request.get", "maxTimeout": 30_000 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "message": "Challenge solved!",
            "solution": {
                "url": "https://tracker.invalid/",
                "status": 200,
                "cookies": [
                    { "name": "cf_clearance", "value": "liberado", "domain": "tracker.invalid", "path": "/" }
                ],
                "userAgent": AGENT,
                "response": "<html></html>"
            }
        })))
        .expect(times)
        .mount(server)
        .await;
}

/// O site atrás do Cloudflare: sem o cookie do desafio e o user-agent que o
/// ganhou, só o desafio.
async fn mount_protected_search(server: &MockServer, searches: u64) {
    Mock::given(path("/browse"))
        .and(header_regex("cookie", "cf_clearance=liberado"))
        .and(header("user-agent", AGENT))
        .respond_with(html(PUBLIC_HTML))
        .expect(searches)
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(path("/browse"))
        .respond_with(challenge())
        .with_priority(2)
        .mount(server)
        .await;
}

#[tokio::test]
async fn sem_flaresolverr_o_desafio_vira_erro_claro() {
    let site = MockServer::start().await;
    mount_protected_search(&site, 0).await;
    let error = client(PUBLIC_YAML, &site.uri(), &[], Network::default())
        .search(&SearchQuery::general("filme"))
        .await
        .unwrap_err();
    assert!(matches!(error, IndexerError::Challenge { .. }), "{error}");
    assert!(
        error.to_string().contains("configure o FlareSolverr"),
        "{error}"
    );
}

#[tokio::test]
async fn flaresolverr_vence_o_desafio_e_os_cookies_valem_ate_o_proximo() {
    let site = MockServer::start().await;
    let flaresolverr = MockServer::start().await;
    mount_solver(&flaresolverr, 1).await;
    mount_protected_search(&site, 2).await;
    let client = client(
        PUBLIC_YAML,
        &site.uri(),
        &[],
        solver(&flaresolverr, Challenge::Auto),
    );

    let first = client.search(&SearchQuery::general("filme")).await.unwrap();
    assert!(!first.is_empty());
    // A segunda busca já sai com o cookie e o user-agent: o FlareSolverr
    // não é chamado de novo.
    let second = client.search(&SearchQuery::general("outro")).await.unwrap();
    assert!(!second.is_empty());
}

#[tokio::test]
async fn desligado_no_indexador_o_flaresolverr_nao_e_chamado() {
    let site = MockServer::start().await;
    let flaresolverr = MockServer::start().await;
    mount_solver(&flaresolverr, 0).await;
    mount_protected_search(&site, 0).await;
    let error = client(
        PUBLIC_YAML,
        &site.uri(),
        &[],
        solver(&flaresolverr, Challenge::Never),
    )
    .search(&SearchQuery::general("filme"))
    .await
    .unwrap_err();
    assert!(error.to_string().contains("desligado"), "{error}");
}

#[tokio::test]
async fn sempre_resolve_antes_da_primeira_requisicao() {
    let site = MockServer::start().await;
    let flaresolverr = MockServer::start().await;
    mount_solver(&flaresolverr, 1).await;
    // Sem o mock do desafio: só passa quem já chega com o cookie.
    Mock::given(path("/browse"))
        .and(header_regex("cookie", "cf_clearance=liberado"))
        .and(header("user-agent", AGENT))
        .respond_with(html(PUBLIC_HTML))
        .expect(2)
        .mount(&site)
        .await;
    let client = client(
        PUBLIC_YAML,
        &site.uri(),
        &[],
        solver(&flaresolverr, Challenge::Always),
    );
    client.search(&SearchQuery::general("filme")).await.unwrap();
    client.search(&SearchQuery::general("outro")).await.unwrap();
}

#[tokio::test]
async fn flaresolverr_que_falha_e_erro_de_desafio() {
    let site = MockServer::start().await;
    let flaresolverr = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "status": "error",
            "message": "Error: Error solving the challenge. Timeout after 30.0 seconds.",
        })))
        .expect(1)
        .mount(&flaresolverr)
        .await;
    mount_protected_search(&site, 0).await;
    let error = client(
        PUBLIC_YAML,
        &site.uri(),
        &[],
        solver(&flaresolverr, Challenge::Auto),
    )
    .search(&SearchQuery::general("filme"))
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("não resolveu o desafio"),
        "{error}"
    );
}

#[tokio::test]
async fn forbidden_que_nao_e_desafio_segue_como_antes() {
    let site = MockServer::start().await;
    let flaresolverr = MockServer::start().await;
    mount_solver(&flaresolverr, 0).await;
    Mock::given(path("/browse"))
        .respond_with(ResponseTemplate::new(403).set_body_raw("<html>proibido</html>", "text/html"))
        .mount(&site)
        .await;
    let error = client(
        PUBLIC_YAML,
        &site.uri(),
        &[],
        solver(&flaresolverr, Challenge::Auto),
    )
    .search(&SearchQuery::general("filme"))
    .await
    .unwrap_err();
    assert!(
        matches!(error, IndexerError::Status { status, .. } if status == 403),
        "{error}"
    );
}

#[tokio::test]
async fn cookie_de_login_vai_junto_do_cookie_do_desafio() {
    const SESSION: &str = "sessao=abc";
    let site = MockServer::start().await;
    let flaresolverr = MockServer::start().await;
    mount_solver(&flaresolverr, 1).await;
    for (route, body) in [
        ("/index.php", r#"<a href="/logout.php?auth=abc">Sair</a>"#),
        ("/torrents.php", COOKIE_HTML),
    ] {
        Mock::given(path(route))
            .and(header_regex(
                "cookie",
                "^sessao=abc; cf_clearance=liberado$",
            ))
            .and(header("user-agent", AGENT))
            .respond_with(html(body))
            .expect(1)
            .with_priority(1)
            .mount(&site)
            .await;
        Mock::given(path(route))
            .respond_with(challenge())
            .with_priority(2)
            .mount(&site)
            .await;
    }
    // O teste de sessão e a busca saem com os dois cookies (as expectativas
    // dos mocks conferem no fim).
    client(
        COOKIE_YAML,
        &site.uri(),
        &[("cookie", SESSION)],
        solver(&flaresolverr, Challenge::Auto),
    )
    .search(&SearchQuery::general("filme"))
    .await
    .unwrap();
}

#[tokio::test]
async fn proxy_leva_a_busca_e_o_download() {
    // O servidor de teste faz o papel do proxy HTTP: recebe a requisição em
    // forma absoluta, para um host que nem resolve.
    let proxy = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/browse"))
        .and(header("host", "tracker.invalid"))
        .respond_with(html(PUBLIC_HTML))
        .expect(1)
        .mount(&proxy)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/1.torrent"))
        .and(header("host", "tracker.invalid"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"d8:announce3:urle".to_vec(), "application/x-bittorrent"),
        )
        .expect(1)
        .mount(&proxy)
        .await;
    let network = Network {
        proxy: Some(Proxy::new(&proxy.uri(), None, None).unwrap()),
        ..Network::default()
    };
    let client = client(PUBLIC_YAML, "http://tracker.invalid", &[], network);
    let releases = client.search(&SearchQuery::general("filme")).await.unwrap();
    assert!(!releases.is_empty());
    let link = url::Url::parse("http://tracker.invalid/download/1.torrent").unwrap();
    assert_eq!(client.download(&link).await.unwrap(), b"d8:announce3:urle");
}

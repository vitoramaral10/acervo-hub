use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use acervo_indexers::{
    AggregateSearch, CardigannClient, CardigannDefinition, Indexer, IndexerError, SearchQuery,
};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const YAML: &str = include_str!("fixtures/cardigann-public.yml");
const HTML: &str = include_str!("fixtures/cardigann-results.html");

fn client(yaml: &str) -> CardigannClient {
    CardigannClient::new(
        CardigannDefinition::from_yaml_v11(yaml).unwrap(),
        0,
        BTreeMap::new(),
        Duration::from_secs(5),
    )
    .unwrap()
}

fn yaml_at(server: &MockServer) -> String {
    YAML.replace("https://tracker.invalid", &server.uri())
}

fn response(html: &str) -> ResponseTemplate {
    // `set_body_string` fixa `text/plain` por cima de qualquer header anterior.
    ResponseTemplate::new(200).set_body_raw(html, "text/html; charset=utf-8")
}

#[test]
fn carrega_metadados_caps_e_mapeamentos_de_categoria_v11() {
    let definition = CardigannDefinition::from_yaml_v11(YAML).unwrap();
    assert_eq!(definition.id(), "arquivo-publico");
    assert_eq!(definition.name(), "Arquivo público");
    assert_eq!(definition.language(), "pt-BR");
    assert!(definition.description().contains("HTML"));
    assert_eq!(definition.links()[0].as_str(), "https://tracker.invalid/");
    assert!(definition.capabilities().tv.supported_params.contains("ep"));
    assert!(
        definition
            .capabilities()
            .movie
            .supported_params
            .contains("tmdbid")
    );
    assert_eq!(definition.capabilities().categories.len(), 4);
}

#[test]
fn recursos_nao_implementados_falham_no_load_sem_expor_yaml() {
    let cases = [
        format!("{YAML}\nlogin:\n  method: post\n"),
        format!("{YAML}\nlogin:\n  method: form\n  path: login\n"),
        YAML.replace("method: get", "method: post"),
        YAML.replace("method: get", "response: {type: json}"),
        YAML.replace("UTF-8", "windows-1252"),
        YAML.replace("type: public", "type: private"),
        YAML.replace("name: trim", "name: regexp"),
        YAML.replace("name: trim", "name: timeago"),
        YAML.replace("{{ .Keywords }}", "{{ .Config.unknown }}"),
        YAML.replace("{{ .Keywords }}", "{{ printf .Keywords }}"),
        YAML.replace("path: browse", "path: '{{ .Keywords }}'"),
        YAML.replace("path: browse", "path: https://other.invalid/search"),
        YAML.replace("path: browse", "path: //other.invalid/search"),
        YAML.replace("TV/HD", "unknown-category"),
        YAML.replace("requestDelay: 0", "requestDelay: -.inf"),
        YAML.replace("table.results > tbody > tr.release", "tr[[secret-value"),
        YAML.replace(
            "table.results > tbody > tr.release",
            "tr:contains(secret-value) td",
        ),
        YAML.replace("args: ['_', '.']", "args: ['(?=x)', '.']")
            .replace("name: replace", "name: re_replace"),
    ];
    for yaml in cases {
        let error = CardigannDefinition::from_yaml_v11(&yaml).unwrap_err();
        assert!(
            matches!(
                error,
                IndexerError::Definition { .. } | IndexerError::UnsupportedDefinitionKey { .. }
            ),
            "{error}"
        );
        assert!(!format!("{error:?} {error}").contains("segredo-fixture"));
        assert!(!format!("{error:?} {error}").contains("secret-value"));
    }
}

#[test]
fn links_e_configuracao_sao_validados_e_debug_omite_segredos() {
    for bad_url in [
        "file:///secret/",
        "https://user:secret@tracker.invalid/",
        "https://tracker.invalid/?passkey=secret",
        "https://tracker.invalid/#secret",
    ] {
        let error =
            CardigannDefinition::from_yaml_v11(&YAML.replace("https://tracker.invalid/", bad_url))
                .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    let definition = CardigannDefinition::from_yaml_v11(YAML).unwrap();
    assert!(!format!("{definition:?}").contains("segredo-fixture"));
    assert!(!format!("{definition:?}").contains("tracker.invalid"));
    assert!(!format!("{:?}", client(YAML)).contains("segredo-fixture"));
    for (overrides, index) in [
        (
            BTreeMap::from([("verified".into(), "secret-value".into())]),
            0,
        ),
        (
            BTreeMap::from([("unknown".into(), "secret-value".into())]),
            0,
        ),
        (BTreeMap::new(), 9),
    ] {
        let error = CardigannClient::new(
            CardigannDefinition::from_yaml_v11(YAML).unwrap(),
            index,
            overrides,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret-value"));
    }
}

#[tokio::test]
async fn consulta_tv_codifica_uma_vez_e_converte_html_em_releases() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/browse"))
        .and(query_param("q", "Uma Série & 100% + / S02E03"))
        .and(query_param("tvdb", "42"))
        .and(query_param("token", "segredo-fixture"))
        .and(query_param("verified", "true"))
        .and(query_param("order", "seeders"))
        .respond_with(response(HTML))
        .expect(1)
        .mount(&server)
        .await;
    let results = client(&yaml_at(&server))
        .search(
            &SearchQuery::tv("Uma Série & 100% + /")
                .with_episode(2, "3")
                .with_tvdb_id(42)
                .with_categories([5000]),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Uma.Série.S02E03 & Extras");
    assert_eq!(results[0].size, 1_610_612_736);
    assert_eq!(results[0].seeders, Some(12));
    assert_eq!(results[0].leechers, Some(0));
    assert_eq!(results[0].categories, [5040]);
    assert_eq!(results[0].guid, format!("{}/details/42", server.uri()));
    assert_eq!(
        results[0].download_url.as_str(),
        format!("{}/download/42?passkey=segredo-download", server.uri())
    );
    assert_eq!(results[1].categories, [2020, 5070]);
    assert!(!format!("{results:?}").contains("segredo-download"));
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests[0]
            .url
            .query_pairs()
            .any(|(name, _)| name == "imdb")
    );
}

#[tokio::test]
async fn consulta_filme_com_overrides_ids_e_limite_local() {
    let server = MockServer::start().await;
    Mock::given(path("/browse"))
        .and(query_param("q", "Um Filme 2026"))
        .and(query_param("imdb", "0123456"))
        .and(query_param("tmdb", "123"))
        .and(query_param("token", "override & +"))
        .respond_with(response(HTML))
        .expect(1)
        .mount(&server)
        .await;
    let definition = CardigannDefinition::from_yaml_v11(&yaml_at(&server)).unwrap();
    let client = CardigannClient::new(
        definition,
        0,
        BTreeMap::from([("token".into(), "override & +".into())]),
        Duration::from_secs(5),
    )
    .unwrap();
    let results = client
        .search(
            &SearchQuery::movie("Um Filme")
                .with_year(2026)
                .with_imdb_id("tt0123456")
                .with_tmdb_id(123)
                .with_categories([2000])
                .with_limit(1),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].download_url.scheme(), "magnet");
    assert_eq!(results[0].size, 2_147_483_648);
    assert_eq!(results[0].categories, [2040]);
}

#[tokio::test]
async fn consulta_incompativel_nao_faz_http() {
    let server = MockServer::start().await;
    let client = client(&yaml_at(&server));
    for query in [
        SearchQuery::general("q").with_offset(1),
        SearchQuery::general("q").with_categories([123_456]),
    ] {
        assert!(matches!(
            client.search(&query).await,
            Err(IndexerError::UnsupportedQuery { .. })
        ));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn linha_ruim_e_pulada_mas_pagina_inteira_ilegivel_e_erro() {
    // Uma linha ruim sai; as outras ficam — como na referência.
    for (html, remaining) in [
        (HTML.replace("1.5 GiB", "NaN"), 2),
        (
            HTML.replace(
                "/download/42?passkey=segredo-download",
                "javascript:secret-value",
            ),
            2,
        ),
        // Categoria sem mapeamento não descarta o release: ele só fica sem
        // categoria, e cai em qualquer busca que filtre por uma.
        (HTML.replace("cat=7", "cat=999"), 3),
    ] {
        let server = MockServer::start().await;
        Mock::given(path("/browse"))
            .respond_with(response(&html))
            .mount(&server)
            .await;
        let results = client(&yaml_at(&server))
            .search(&SearchQuery::general("q"))
            .await
            .unwrap();
        assert_eq!(results.len(), remaining);
        assert!(!format!("{results:?}").contains("secret-value"));
    }

    // Nenhuma linha rende: o site mudou de layout, e isso não é "nada
    // encontrado".
    let server = MockServer::start().await;
    Mock::given(path("/browse"))
        .respond_with(response(
            &HTML.replace("class=\"title\"", "class=\"missing\""),
        ))
        .mount(&server)
        .await;
    let error = client(&yaml_at(&server))
        .search(&SearchQuery::general("q"))
        .await
        .unwrap_err();
    assert!(matches!(error, IndexerError::InvalidRelease { .. }));
}

#[tokio::test]
async fn redirects_e_erros_http_nao_vazam_settings() {
    for status in [302, 403, 429, 500] {
        let server = MockServer::start().await;
        Mock::given(path("/browse"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("Location", "https://secret-value.invalid/"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = client(&yaml_at(&server))
            .search(&SearchQuery::general("q"))
            .await
            .unwrap_err();
        assert!(matches!(error, IndexerError::Status { .. }));
        assert!(!format!("{error:?} {error}").contains("segredo-fixture"));
        assert!(!format!("{error:?} {error}").contains("secret-value"));
    }
}

#[tokio::test]
async fn resposta_json_ou_utf8_invalido_e_recusada() {
    for template in [
        ResponseTemplate::new(200).set_body_json(serde_yaml_ng::Value::Null),
        ResponseTemplate::new(200).set_body_raw(vec![0xff, 0xfe], "text/html"),
    ] {
        let server = MockServer::start().await;
        Mock::given(path("/browse"))
            .respond_with(template)
            .mount(&server)
            .await;
        assert!(matches!(
            client(&yaml_at(&server))
                .search(&SearchQuery::general("q"))
                .await,
            Err(IndexerError::UnexpectedDocument { .. })
        ));
    }
}

#[tokio::test]
async fn clones_compartilham_request_delay_e_funcionam_no_agregador() {
    let server = MockServer::start().await;
    Mock::given(path("/browse"))
        .respond_with(response(HTML))
        .expect(2)
        .mount(&server)
        .await;
    let client = client(&yaml_at(&server).replace("requestDelay: 0", "requestDelay: 0.15"));
    let aggregate = AggregateSearch::new(vec![
        std::sync::Arc::new(client.clone()),
        std::sync::Arc::new(client),
    ]);
    let start = Instant::now();
    let results = aggregate.search(&SearchQuery::general("q")).await;
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert!(results.failures.is_empty());
    assert_eq!(results.releases.len(), 6);
}

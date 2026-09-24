//! Contrato do executor Cardigann com tracker privado: login por cookie e por
//! formulário, sessão que cai no meio da busca, várias páginas e download.
//!
//! As definições de teste reproduzem a estrutura das definições reais que o
//! executor precisa rodar — templates aninhados, `$raw`, `:contains` com
//! template, campos auxiliares, `case`, `dateparse` —, com nomes genéricos.

use std::collections::BTreeMap;
use std::time::Duration;

use acervo_indexers::{CardigannClient, CardigannDefinition, Indexer, IndexerError, SearchQuery};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use wiremock::matchers::{
    body_string_contains, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COOKIE_YAML: &str = include_str!("fixtures/cardigann-cookie.yml");
const COOKIE_HTML: &str = include_str!("fixtures/cardigann-cookie.html");
const FORM_YAML: &str = include_str!("fixtures/cardigann-formulario.yml");
const FORM_PAGE0: &str = include_str!("fixtures/cardigann-formulario-pagina0.html");
const FORM_PAGE1: &str = include_str!("fixtures/cardigann-formulario-pagina1.html");

const COOKIE: &str = "sessao=abc; outra=1";
const LOGGED_IN: &str = r#"<a href="account-logout.php">Sair</a>"#;

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8")
}

fn client(yaml: &str, server: &MockServer, settings: &[(&str, &str)]) -> CardigannClient {
    let yaml = yaml.replace("https://tracker.invalid/", &format!("{}/", server.uri()));
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
}

fn form_client(server: &MockServer) -> CardigannClient {
    client(
        FORM_YAML,
        server,
        &[("username", "usuario"), ("password", "senha secreta")],
    )
}

async fn mount_form_login(server: &MockServer, logins: u64) {
    Mock::given(method("POST"))
        .and(path("/account-login.php"))
        .and(body_string_contains("username=usuario"))
        .and(body_string_contains("password=senha+secreta"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "/")
                .insert_header("Set-Cookie", "sessao=xyz; Path=/"),
        )
        .expect(logins)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(LOGGED_IN))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/index.php"))
        .and(header("cookie", "sessao=xyz"))
        .respond_with(html(LOGGED_IN))
        .expect(logins)
        .mount(server)
        .await;
}

async fn mount_form_pages(server: &MockServer) {
    Mock::given(path("/torrents-search.php"))
        .and(query_param("page", "2"))
        .respond_with(html(FORM_PAGE1))
        .expect(0)
        .named("página depois da última")
        .mount(server)
        .await;
    Mock::given(path("/torrents-search.php"))
        .and(header("cookie", "sessao=xyz"))
        .and(query_param_is_missing("page"))
        .and(query_param("search", "Um%Filme"))
        .and(query_param("free", "2"))
        .and(query_param("sort", "id"))
        .respond_with(html(FORM_PAGE0))
        .mount(server)
        .await;
    Mock::given(path("/torrents-search.php"))
        .and(header("cookie", "sessao=xyz"))
        .and(query_param("page", "1"))
        .respond_with(html(FORM_PAGE1))
        .mount(server)
        .await;
}

#[tokio::test]
async fn cookie_vai_em_todo_request_e_a_consulta_sai_como_o_site_espera() {
    let server = MockServer::start().await;
    Mock::given(path("/index.php"))
        .and(header("cookie", COOKIE))
        .respond_with(html(r#"<a href="/logout.php?auth=abc">Sair</a>"#))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/torrents.php"))
        .and(header("cookie", COOKIE))
        // `keywordsfilters` tirou o S01E02 do termo; `$raw` virou um par por
        // categoria do tracker que cobre TV.
        .and(query_param("searchstr", "Uma Série"))
        .and(query_param("filter_cat[2]", "1"))
        .and(query_param("filter_cat[14]", "1"))
        .and(query_param_is_missing("freetorrent"))
        .respond_with(html(COOKIE_HTML))
        .expect(1)
        .mount(&server)
        .await;

    let results = client(COOKIE_YAML, &server, &[("cookie", COOKIE)])
        .search(
            &SearchQuery::tv("Uma Série")
                .with_episode(1, "2")
                .with_categories([5000]),
        )
        .await
        .unwrap();

    // Grupo + o episódio pedido. O S01E03 fica fora pelo `:contains` do
    // seletor, "Outra Coisa" pelo `andmatch` e "Livros" pela categoria.
    let titles: Vec<_> = results
        .iter()
        .map(|release| release.title.as_str())
        .collect();
    assert_eq!(
        titles,
        ["Uma Série Original 2026", "Uma Série Original 2026"]
    );
    assert_eq!(results[0].size, 1_610_612_736);
    assert_eq!(results[0].grabs, Some(10));
    assert_eq!(results[0].seeders, Some(12));
    assert_eq!(results[0].leechers, Some(3));
    assert_eq!(results[0].categories, [5000]);
    assert_eq!(
        results[0].guid,
        format!("{}/torrents.php?id=5", server.uri())
    );
    assert!(
        results[0]
            .download_url
            .as_str()
            .contains("torrent_pass=segredo-download")
    );
    assert_eq!(results[1].size, 700 * 1024 * 1024);
    assert!(!format!("{results:?}").contains("segredo-download"));
}

#[tokio::test]
async fn busca_por_imdb_usa_o_id_e_nao_o_termo() {
    let server = MockServer::start().await;
    Mock::given(path("/index.php"))
        .respond_with(html(r#"<a href="/logout.php?auth=abc">Sair</a>"#))
        .mount(&server)
        .await;
    Mock::given(path("/torrents.php"))
        .and(query_param("searchstr", "tt0123456%"))
        .and(query_param("filter_cat[1]", "1"))
        .respond_with(html(
            r#"<a href="/logout.php?auth=abc">Sair</a><table class="torrent_table"></table>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let results = client(COOKIE_YAML, &server, &[("cookie", COOKIE)])
        .search(
            &SearchQuery::movie("Um Filme")
                .with_imdb_id("0123456")
                .with_categories([2000]),
        )
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn cookie_vazio_ou_invalido_falha_no_login_sem_buscar() {
    for cookie in ["", "a=1\nb=2"] {
        let server = MockServer::start().await;
        Mock::given(path("/torrents.php"))
            .respond_with(html(COOKIE_HTML))
            .expect(0)
            .mount(&server)
            .await;
        let error = client(COOKIE_YAML, &server, &[("cookie", cookie)])
            .search(&SearchQuery::general("x"))
            .await
            .unwrap_err();
        assert!(matches!(error, IndexerError::Login { .. }), "{error}");
    }
}

#[tokio::test]
async fn formulario_loga_uma_vez_e_junta_as_paginas() {
    let server = MockServer::start().await;
    mount_form_login(&server, 1).await;
    mount_form_pages(&server).await;
    let client = form_client(&server);

    let results = client
        .search(&SearchQuery::general("Um Filme 2026"))
        .await
        .unwrap();
    // A sessão sobrevive entre buscas: sem login de novo.
    client
        .search(&SearchQuery::general("Um Filme 2026"))
        .await
        .unwrap();

    let titles: Vec<_> = results
        .iter()
        .map(|release| release.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Um Filme Original 2026 1080p Dublado",
            "Anime Legal",
            "Uma Série S01E02"
        ]
    );
    assert_eq!(results[0].categories, [2000]);
    assert_eq!(results[1].categories, [5070]);
    assert_eq!(results[2].categories, [5000]);
    assert_eq!(results[0].size, 1_610_612_736);
    assert_eq!(results[0].seeders, Some(1204));
    assert_eq!(results[1].leechers, None);
    assert_eq!(
        results[0].published.unwrap().format(&Rfc3339).unwrap(),
        "2026-09-24T09:05:07Z"
    );
    // Sem data na linha, `default: now`.
    assert!(results[1].published.unwrap() > OffsetDateTime::now_utc() - time::Duration::minutes(1));
}

#[tokio::test]
async fn tamanho_de_pagina_aprendido_poupa_a_segunda_pagina() {
    let server = MockServer::start().await;
    mount_form_login(&server, 1).await;
    // Primeira busca: página 0 com 3 linhas, página 1 com 1 → aprende que a
    // página cheia tem 3.
    mount_form_pages(&server).await;
    let client = form_client(&server);
    client
        .search(&SearchQuery::general("Um Filme"))
        .await
        .unwrap();

    // Segunda busca, outro termo: a página 0 vem com 1 linha, menos que 3. É a
    // última — a página 1 não pode ser pedida.
    Mock::given(path("/torrents-search.php"))
        .and(query_param_is_missing("page"))
        .and(query_param("search", "Outra"))
        .respond_with(html(FORM_PAGE1))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/torrents-search.php"))
        .and(query_param("page", "1"))
        .and(query_param("search", "Outra"))
        .respond_with(html(FORM_PAGE1))
        .expect(0)
        .mount(&server)
        .await;
    let results = client.search(&SearchQuery::general("Outra")).await.unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn sessao_que_cai_no_meio_refaz_o_login_uma_vez() {
    let server = MockServer::start().await;
    mount_form_login(&server, 2).await;
    Mock::given(path("/torrents-search.php"))
        .and(query_param_is_missing("page"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/account-login.php"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_form_pages(&server).await;

    let results = form_client(&server)
        .search(&SearchQuery::general("Um Filme"))
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
}

#[tokio::test]
async fn credencial_recusada_e_erro_de_login_sem_vazar_senha() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account-login.php"))
        .respond_with(html(
            r#"<div class="alert-primary">Usuário ou senha incorretos</div>"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path("/torrents-search.php"))
        .respond_with(html(FORM_PAGE0))
        .expect(0)
        .mount(&server)
        .await;

    let error = form_client(&server)
        .search(&SearchQuery::general("x"))
        .await
        .unwrap_err();
    assert!(matches!(error, IndexerError::Login { .. }));
    let shown = format!("{error:?} {error}");
    assert!(!shown.contains("senha secreta") && !shown.contains("senha+secreta"));
}

#[tokio::test]
async fn pagina_de_teste_sem_marcador_de_sessao_e_login_falho() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/account-login.php"))
        .respond_with(html("ok"))
        .mount(&server)
        .await;
    Mock::given(path("/index.php"))
        .respond_with(html("<form>entre</form>"))
        .mount(&server)
        .await;
    let error = form_client(&server)
        .search(&SearchQuery::general("x"))
        .await
        .unwrap_err();
    assert!(matches!(error, IndexerError::Login { .. }));
}

#[tokio::test]
async fn download_usa_a_sessao_e_so_aceita_torrent_da_propria_origem() {
    let server = MockServer::start().await;
    mount_form_login(&server, 1).await;
    Mock::given(path("/download.php"))
        .and(query_param("id", "10"))
        .and(header("cookie", "sessao=xyz"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"d8:announce3:urle".to_vec(), "application/x-bittorrent"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/download.php"))
        .and(query_param("id", "11"))
        .respond_with(html("<html>faça login</html>"))
        .mount(&server)
        .await;
    let client = form_client(&server);
    assert!(client.proxies_downloads());

    let base = url::Url::parse(&server.uri()).unwrap();
    let torrent = client
        .download(
            &base
                .join("/download.php?id=10&passkey=segredo-download")
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(torrent, b"d8:announce3:urle");

    assert!(matches!(
        client
            .download(&base.join("/download.php?id=11").unwrap())
            .await,
        Err(IndexerError::UnexpectedDocument { .. })
    ));
    assert!(matches!(
        client
            .download(&url::Url::parse("https://outro.invalid/download.php?id=10").unwrap())
            .await,
        Err(IndexerError::UnsupportedQuery { .. })
    ));
}

#[test]
fn privado_sem_login_e_recusado() {
    let yaml = FORM_YAML.split("login:").next().unwrap().to_owned()
        + "search:"
        + FORM_YAML.split("\nsearch:").nth(1).unwrap();
    assert!(matches!(
        CardigannDefinition::from_yaml_v11(&yaml),
        Err(IndexerError::Definition { .. })
    ));
}

#[test]
fn chave_desconhecida_e_nomeada_sem_expor_valores() {
    let yaml = FORM_YAML.replace(
        "requestDelay: 0",
        "requestDelay: 0\ncaptcha: segredo-fixture",
    );
    let error = CardigannDefinition::from_yaml_v11(&yaml).unwrap_err();
    assert!(matches!(&error, IndexerError::UnsupportedDefinitionKey { key } if key == "captcha"));
    assert!(!error.to_string().contains("segredo-fixture"));
}

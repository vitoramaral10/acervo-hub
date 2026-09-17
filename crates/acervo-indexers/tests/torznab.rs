use std::time::Duration;

use acervo_indexers::{IndexerError, SearchQuery, TorznabClient};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> TorznabClient {
    TorznabClient::new(
        "tracker",
        &format!("{}/torznab/api", server.uri()),
        Some("chave-de-teste".into()),
        Duration::from_secs(5),
        Duration::ZERO,
    )
    .unwrap()
}

#[tokio::test]
async fn capacidades_expoem_limites_modos_e_categorias() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .and(query_param("t", "caps"))
        .and(query_param("apikey", "chave-de-teste"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <caps>
              <limits max="100" default="50" />
              <searching>
                <search available="yes" supportedParams="q" />
                <tv-search available="yes" supportedParams="q,tvdbid,season,ep" />
                <movie-search available="no" supportedParams="q,imdbid" />
              </searching>
              <categories>
                <category id="5000" name="TV">
                  <subcat id="5040" name="TV/HD" />
                </category>
              </categories>
            </caps>"#,
        ))
        .mount(&server)
        .await;

    let caps = client(&server).capabilities().await.unwrap();

    assert_eq!(caps.max_results, Some(100));
    assert_eq!(caps.default_results, Some(50));
    assert!(caps.general.available);
    assert!(caps.tv.supported_params.contains("tvdbid"));
    assert!(!caps.movie.available);
    assert_eq!(caps.categories.len(), 2);
    assert_eq!(caps.categories[1].parent, Some(5000));
}

#[tokio::test]
async fn busca_de_episodio_envia_o_contrato_e_le_o_rss() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .and(query_param("t", "tvsearch"))
        .and(query_param("q", "Uma Série"))
        .and(query_param("season", "2"))
        .and(query_param("ep", "3"))
        .and(query_param("tvdbid", "12345"))
        .and(query_param("cat", "5000,5040"))
        .and(query_param("limit", "25"))
        .and(query_param("extended", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
              <channel>
                <item>
                  <title>Uma.Serie.S02E03.1080p</title>
                  <guid isPermaLink="false">release-1</guid>
                  <link>https://tracker.invalid/download/release-1</link>
                  <comments>https://tracker.invalid/details/release-1</comments>
                  <pubDate>Sun, 06 Sep 2026 17:29:23 +0000</pubDate>
                  <enclosure url="https://tracker.invalid/download/release-1" length="2147483648" type="application/x-bittorrent" />
                  <torznab:attr name="size" value="2147483648" />
                  <torznab:attr name="category" value="5000" />
                  <torznab:attr name="category" value="5040" />
                  <torznab:attr name="category" value="5040" />
                  <torznab:attr name="seeders" value="12" />
                  <torznab:attr name="leechers" value="3" />
                  <torznab:attr name="tag" value="freeleech" />
                </item>
              </channel>
            </rss>"#,
        ))
        .mount(&server)
        .await;

    let query = SearchQuery::tv("Uma Série")
        .with_episode(2, "3")
        .with_tvdb_id(12345)
        .with_categories([5040, 5000, 5040])
        .with_limit(25);
    let releases = client(&server).search(&query).await.unwrap();

    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].guid, "release-1");
    assert_eq!(releases[0].size, 2_147_483_648);
    assert_eq!(releases[0].seeders, Some(12));
    assert_eq!(releases[0].categories, vec![5000, 5040]);
    assert_eq!(releases[0].tags, vec!["freeleech"]);
}

#[tokio::test]
async fn atributos_newznab_e_torznab_entram_no_mesmo_modelo() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<rss xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
              <channel><item>
                <title>Filme.2026.2160p</title>
                <guid>release-2</guid>
                <link>/download/release-2</link>
                <newznab:attr name="size" value="42" />
                <newznab:attr name="category" value="2045" />
                <newznab:attr name="grabs" value="9" />
              </item></channel>
            </rss>"#,
        ))
        .mount(&server)
        .await;

    let releases = client(&server)
        .search(&SearchQuery::movie("Filme"))
        .await
        .unwrap();

    assert_eq!(releases[0].size, 42);
    assert_eq!(releases[0].grabs, Some(9));
    assert_eq!(releases[0].download_url.path(), "/download/release-2");
}

#[tokio::test]
async fn erro_xml_com_http_200_continua_sendo_erro() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<error code="100" description="Incorrect credentials" />"#),
        )
        .mount(&server)
        .await;

    let error = client(&server)
        .search(&SearchQuery::general("teste"))
        .await
        .unwrap_err();

    assert!(matches!(error, IndexerError::Api { code, .. } if code == "100"));
}

#[tokio::test]
async fn release_sem_tamanho_invalida_a_resposta() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r"<rss><channel><item>
              <title>Sem.Tamanho</title><guid>x</guid><link>/x</link>
            </item></channel></rss>",
        ))
        .mount(&server)
        .await;

    let error = client(&server)
        .search(&SearchQuery::general("teste"))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        IndexerError::InvalidRelease { field: "size", .. }
    ));
}

#[tokio::test]
async fn pagina_html_nao_vira_resultado_vazio() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<!DOCTYPE html><html><body>Entrar</body></html>"),
        )
        .mount(&server)
        .await;

    let error = client(&server)
        .search(&SearchQuery::general("teste"))
        .await
        .unwrap_err();

    assert!(matches!(error, IndexerError::UnexpectedDocument { .. }));
}

#[test]
fn debug_do_cliente_nao_expoe_chave_nem_endpoint() {
    let client = TorznabClient::new(
        "tracker",
        "https://tracker.invalid/api?apikey=na-url",
        Some("chave-secreta".into()),
        Duration::from_secs(5),
        Duration::ZERO,
    )
    .unwrap();

    let debug = format!("{client:?}");
    assert!(!debug.contains("chave-secreta"));
    assert!(!debug.contains("na-url"));
    assert!(!debug.contains("tracker.invalid"));
}

#[tokio::test]
async fn clones_compartilham_o_mesmo_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/torznab/api"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<rss><channel /></rss>"))
        .mount(&server)
        .await;

    let first = TorznabClient::new(
        "tracker",
        &format!("{}/torznab/api", server.uri()),
        None,
        Duration::from_secs(5),
        Duration::from_millis(40),
    )
    .unwrap();
    let second = first.clone();
    let start = tokio::time::Instant::now();

    first.search(&SearchQuery::general("um")).await.unwrap();
    second.search(&SearchQuery::general("dois")).await.unwrap();

    assert!(tokio::time::Instant::now() - start >= Duration::from_millis(30));
}

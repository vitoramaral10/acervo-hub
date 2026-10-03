//! Contrato com a API do Jellyfin (12.1).
//!
//! Cobre o que a tarefa de assistidos lê: a chave no cabeçalho
//! `MediaBrowser`, os filmes pela rota `/Items?userId=` com os dados do
//! usuário, e o `ProviderIds` que casa com o catálogo. Não substitui uma
//! volta contra o servidor de verdade.

use std::time::Duration;

use acervo_clients::jellyfin::{JellyfinClient, JellyfinEpisode, JellyfinError, JellyfinMovie};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAVE: &str = "0123456789abcdef0123456789abcdef";
const AUTH: &str = "MediaBrowser Token=\"0123456789abcdef0123456789abcdef\"";

fn cliente(server: &MockServer) -> JellyfinClient {
    JellyfinClient::new(&server.uri(), CHAVE, Duration::from_secs(5)).unwrap()
}

/// Um item como o `/Items` manda, com os campos de verdade em volta.
fn filme(nome: &str, ano: u16, provedores: &Value, dados: &Value) -> Value {
    json!({
        "Name": nome,
        "ServerId": "f1e2d3c4b5a6",
        "Id": "a1b2c3d4e5f60718293a4b5c6d7e8f90",
        "HasSubtitles": true,
        "Container": "mkv",
        "PremiereDate": format!("{ano}-05-01T00:00:00.0000000Z"),
        "CriticRating": 90,
        "OfficialRating": "R",
        "CommunityRating": 7.9,
        "RunTimeTicks": 72_000_000_000_i64,
        "ProductionYear": ano,
        "ProviderIds": provedores,
        "IsFolder": false,
        "Type": "Movie",
        "UserData": dados,
        "VideoType": "VideoFile",
        "ImageTags": { "Primary": "abc" },
        "BackdropImageTags": [],
        "LocationType": "FileSystem",
        "MediaType": "Video"
    })
}

#[tokio::test]
async fn usuarios_com_a_chave_no_cabecalho() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Users"))
        .and(header("authorization", AUTH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "Name": "vitor", "ServerId": "s", "Id": "u1", "HasPassword": true,
              "Policy": { "IsAdministrator": true } },
            { "Name": "ana", "ServerId": "s", "Id": "u2", "HasPassword": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let users = cliente(&server).users().await.unwrap();
    let nomes: Vec<_> = users
        .iter()
        .map(|u| (u.id.as_str(), u.name.as_str()))
        .collect();
    assert_eq!(nomes, [("u1", "vitor"), ("u2", "ana")]);
}

#[tokio::test]
async fn filmes_do_usuario_com_assistido_data_favorito_e_tmdb() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(header("authorization", AUTH))
        .and(query_param("userId", "u1"))
        .and(query_param("includeItemTypes", "Movie"))
        .and(query_param("recursive", "true"))
        .and(query_param("fields", "ProviderIds"))
        .and(query_param("enableUserData", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [
                filme(
                    "O Retorno",
                    2024,
                    &json!({ "Tmdb": "1104", "Imdb": "tt0000001" }),
                    &json!({
                        "PlaybackPositionTicks": 0, "PlayCount": 2, "IsFavorite": false,
                        "LastPlayedDate": "2026-09-30T21:14:08.1234567Z", "Played": true,
                        "Key": "1104", "ItemId": "a1b2"
                    }),
                ),
                filme(
                    "Sem Provedor",
                    2020,
                    &json!({}),
                    &json!({ "PlayCount": 0, "IsFavorite": true, "Played": false, "Key": "k" }),
                ),
                // Id não numérico não casa com nada.
                filme(
                    "Torto",
                    2019,
                    &json!({ "Tmdb": "abc" }),
                    &json!({ "Played": true, "IsFavorite": false }),
                ),
            ],
            "TotalRecordCount": 3,
            "StartIndex": 0
        })))
        .expect(1)
        .mount(&server)
        .await;

    let movies = cliente(&server).movies("u1").await.unwrap();
    assert_eq!(
        movies,
        [
            JellyfinMovie {
                name: "O Retorno".into(),
                year: Some(2024),
                tmdb_id: Some(1104),
                played: true,
                last_played: Some("2026-09-30T21:14:08.1234567Z".into()),
                favorite: false,
            },
            JellyfinMovie {
                name: "Sem Provedor".into(),
                year: Some(2020),
                tmdb_id: None,
                played: false,
                last_played: None,
                favorite: true,
            },
            JellyfinMovie {
                name: "Torto".into(),
                year: Some(2019),
                tmdb_id: None,
                played: true,
                last_played: None,
                favorite: false,
            },
        ]
    );
}

/// Uma série como o `/Items` manda.
fn serie(id: &str, provedores: &Value, favorita: bool) -> Value {
    json!({
        "Name": "Uma Série",
        "ServerId": "f1e2d3c4b5a6",
        "Id": id,
        "IsFolder": true,
        "Type": "Series",
        "ProviderIds": provedores,
        "UserData": { "PlayCount": 0, "IsFavorite": favorita, "Played": false, "Key": "k" }
    })
}

/// Um episódio como o `/Items` manda; sem `ProviderIds` da série.
fn episodio(serie_id: &str, temporada: u16, numero: u16, fim: Option<u16>, dados: &Value) -> Value {
    let mut item = json!({
        "Name": "Episódio",
        "ServerId": "f1e2d3c4b5a6",
        "Id": format!("e{temporada}{numero}"),
        "Container": "mkv",
        "RunTimeTicks": 27_000_000_000_i64,
        "IndexNumber": numero,
        "ParentIndexNumber": temporada,
        "IsFolder": false,
        "Type": "Episode",
        "SeriesName": "Uma Série",
        "SeriesId": serie_id,
        "SeasonId": "s1",
        "UserData": dados,
        "MediaType": "Video"
    });
    if let Some(fim) = fim {
        item["IndexNumberEnd"] = json!(fim);
    }
    item
}

async fn monta_series_e_episodios(server: &MockServer, series: Value, episodios: Value) {
    for (tipo, itens) in [("Series", series), ("Episode", episodios)] {
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(header("authorization", AUTH))
            .and(query_param("userId", "u1"))
            .and(query_param("includeItemTypes", tipo))
            .and(query_param("recursive", "true"))
            .and(query_param("fields", "ProviderIds"))
            .and(query_param("enableUserData", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": itens,
                "TotalRecordCount": 2,
                "StartIndex": 0
            })))
            .expect(1)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn episodios_juntam_com_a_serie_pelo_series_id() {
    let server = MockServer::start().await;
    monta_series_e_episodios(
        &server,
        json!([
            serie("sa", &json!({ "Tmdb": "1396", "Tvdb": "81189" }), false),
            serie("sb", &json!({ "Tmdb": "2316" }), true),
        ]),
        json!([
            episodio(
                "sa",
                1,
                2,
                None,
                &json!({
                    "PlayCount": 1, "IsFavorite": true,
                    "LastPlayedDate": "2026-09-30T21:14:08.1234567Z", "Played": true
                }),
            ),
            episodio(
                "sb",
                3,
                4,
                None,
                &json!({ "Played": false, "IsFavorite": false })
            ),
            // Série que a busca não trouxe.
            episodio("sumida", 1, 1, None, &json!({ "Played": true })),
        ]),
    )
    .await;

    let episodes = cliente(&server).episodes("u1").await.unwrap();
    assert_eq!(
        episodes,
        [
            JellyfinEpisode {
                series_tmdb_id: Some(1396),
                series_tvdb_id: Some(81189),
                season: Some(1),
                number: Some(2),
                index_end: None,
                played: true,
                last_played: Some("2026-09-30T21:14:08.1234567Z".into()),
                favorite: true,
                series_favorite: false,
            },
            JellyfinEpisode {
                series_tmdb_id: Some(2316),
                series_tvdb_id: None,
                season: Some(3),
                number: Some(4),
                index_end: None,
                played: false,
                last_played: None,
                favorite: false,
                series_favorite: true,
            },
            JellyfinEpisode {
                series_tmdb_id: None,
                series_tvdb_id: None,
                season: Some(1),
                number: Some(1),
                index_end: None,
                played: true,
                last_played: None,
                favorite: false,
                series_favorite: false,
            },
        ]
    );
}

#[tokio::test]
async fn episodio_multi_episodio_traz_o_fim_do_intervalo() {
    let server = MockServer::start().await;
    monta_series_e_episodios(
        &server,
        json!([serie("sa", &json!({ "Tvdb": "81189" }), false)]),
        json!([episodio("sa", 2, 5, Some(6), &json!({ "Played": true }))]),
    )
    .await;

    let episodes = cliente(&server).episodes("u1").await.unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(
        (
            episodes[0].season,
            episodes[0].number,
            episodes[0].index_end
        ),
        (Some(2), Some(5), Some(6))
    );
    assert_eq!(episodes[0].series_tvdb_id, Some(81189));
}

#[tokio::test]
async fn episodio_de_serie_sem_provider_ids_fica_sem_ids() {
    let server = MockServer::start().await;
    // Uma série sem o campo e outra com `ProviderIds` vazio.
    let mut sem_campo = serie("sa", &json!({}), true);
    sem_campo.as_object_mut().unwrap().remove("ProviderIds");
    monta_series_e_episodios(
        &server,
        json!([sem_campo, serie("sb", &json!({}), false)]),
        json!([
            episodio("sa", 1, 1, None, &json!({ "Played": false })),
            episodio("sb", 1, 1, None, &json!({ "Played": false })),
        ]),
    )
    .await;

    let episodes = cliente(&server).episodes("u1").await.unwrap();
    assert!(
        episodes
            .iter()
            .all(|e| e.series_tmdb_id.is_none() && e.series_tvdb_id.is_none())
    );
    // O favorito da série não depende dos ids.
    assert_eq!(
        episodes
            .iter()
            .map(|e| e.series_favorite)
            .collect::<Vec<_>>(),
        [true, false]
    );
}

#[tokio::test]
async fn varredura_da_biblioteca_e_chave_recusada() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Library/Refresh"))
        .and(header("authorization", AUTH))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Users"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let client = cliente(&server);
    client.refresh_library().await.unwrap();
    let erro = client.users().await.unwrap_err();
    assert!(
        matches!(erro, JellyfinError::Status { status, .. } if status == 401),
        "{erro}"
    );
}

#[test]
fn chave_fica_fora_do_debug() {
    let client =
        JellyfinClient::new("http://jellyfin:8096", CHAVE, Duration::from_secs(5)).unwrap();
    assert!(!format!("{client:?}").contains(CHAVE));
}

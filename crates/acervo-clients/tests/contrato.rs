// O torrent real tem ~45 campos, e o `json!` expande um nível de macro por
// campo: sem isto o teste não compila.
#![recursion_limit = "256"]

//! Contrato com a `WebUI` API v2 do qBittorrent.
//!
//! Vale o mesmo aviso do adaptador das `*arr`: isto não substitui um ciclo
//! contra o cliente de verdade. Cobre o que dá para cobrir sem ele — o login
//! que responde 200 mesmo quando recusa, o campo `private` que só existe a
//! partir da 5.0, e a remoção, que é a única chamada aqui capaz de apagar
//! arquivo.

use std::path::PathBuf;
use std::time::Duration;

use acervo_clients::{QbitClient, client_path, state_from_qbit};
use acervo_core::{DownloadHash, DownloadState};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Um torrent como `torrents/info` o reporta, com os campos que o produto
/// realmente manda. `private` fica de fora: é o formato anterior à 5.0.
fn torrent(hash: &str, state: &str, save_path: &str) -> Value {
    json!({
        "added_on": 1_757_800_000_i64,
        "amount_left": 0,
        "auto_tmm": false,
        "availability": -1,
        "category": "tv-sonarr",
        "completed": 2_147_483_648_i64,
        "completion_on": 1_757_810_000_i64,
        "content_path": format!("{save_path}/Uma.Serie.S02E03.1080p"),
        "dl_limit": 0,
        "dlspeed": 0,
        "downloaded": 2_147_483_648_i64,
        "eta": 8_640_000,
        "f_l_piece_prio": false,
        "force_start": false,
        "hash": hash,
        "last_activity": 1_757_900_000_i64,
        "magnet_uri": format!("magnet:?xt=urn:btih:{hash}"),
        "max_ratio": -1,
        "max_seeding_time": -1,
        "name": "Uma.Serie.S02E03.1080p",
        "num_complete": 12,
        "num_incomplete": 3,
        "num_leechs": 1,
        "num_seeds": 5,
        "priority": 0,
        "progress": 1,
        "ratio": 2.5,
        "ratio_limit": -2,
        "save_path": save_path,
        "seeding_time": 432_000,
        "seeding_time_limit": -2,
        "seen_complete": 1_757_890_000_i64,
        "seq_dl": false,
        "size": 2_147_483_648_i64,
        "state": state,
        "super_seeding": false,
        "tags": "",
        "time_active": 500_000,
        "total_size": 2_147_483_648_i64,
        "tracker": "https://tracker.example.invalid/announce",
        "trackers_count": 1,
        "up_limit": 0,
        "uploaded": 5_368_709_120_i64,
        "uploaded_session": 1_073_741_824_i64,
        "upspeed": 12_345
    })
}

async fn monta_login(server: &MockServer, corpo: &str) {
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        // Sem Referer a proteção de CSRF da WebUI responde 403 com credencial
        // correta: o cabeçalho é parte do contrato, não zelo.
        .and(header("referer", server.uri() + "/"))
        .and(body_string_contains("username=vigia"))
        .respond_with(ResponseTemplate::new(200).set_body_string(corpo))
        .mount(server)
        .await;
}

async fn sessao(server: &MockServer) -> QbitClient {
    monta_login(server, "Ok.").await;
    QbitClient::login(&server.uri(), "vigia", "segredo", Duration::from_secs(5))
        .await
        .expect("login aceito")
}

#[tokio::test]
async fn credencial_errada_responde_200_e_ainda_assim_e_recusa() {
    let server = MockServer::start().await;
    monta_login(&server, "Fails.").await;

    // Tratar isto como sucesso daria uma sessão sem cookie, e toda chamada
    // seguinte voltaria vazia — o cliente pareceria não ter torrent nenhum,
    // que é o estado em que todo seed vira candidato a remoção.
    let erro = QbitClient::login(&server.uri(), "vigia", "errada", Duration::from_secs(5))
        .await
        .unwrap_err();

    assert!(erro.to_string().contains("Login recusado") || erro.to_string().contains("recusado"));
}

#[tokio::test]
async fn qbittorrent_5_responde_204_no_login_certo_e_401_no_errado() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .and(body_string_contains("password=segredo"))
        .respond_with(
            ResponseTemplate::new(204).insert_header("Set-Cookie", "SID=abc; HttpOnly; path=/"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .and(body_string_contains("password=errada"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Fails."))
        .mount(&server)
        .await;

    QbitClient::login(&server.uri(), "vigia", "segredo", Duration::from_secs(5))
        .await
        .expect("204 é sucesso no qBittorrent 5.1+");
    let erro = QbitClient::login(&server.uri(), "vigia", "errada", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(erro.to_string().contains("recusado"), "{erro}");
}

#[tokio::test]
async fn listagem_traz_o_que_a_decisao_precisa() {
    let server = MockServer::start().await;
    let cliente = sessao(&server).await;

    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            torrent("a1b2c3d4e5f6", "uploading", "/media/downloads/tv"),
            torrent("ffeeddccbbaa", "stoppedUP", "/media/downloads/filmes"),
        ])))
        .mount(&server)
        .await;

    let torrents = cliente.torrents().await.expect("listagem");

    assert_eq!(torrents.len(), 2);
    assert_eq!(state_from_qbit(&torrents[0].state), DownloadState::Seeding);
    assert_eq!(state_from_qbit(&torrents[1].state), DownloadState::Paused);
    assert_eq!(torrents[0].seeded_for(), Duration::from_secs(432_000));

    // Versão sem o campo: privado por omissão. A assimetria custa disco; a
    // inversa custa hit&run.
    assert!(torrents[0].is_private());
}

#[tokio::test]
async fn versao_nova_declara_o_torrent_publico() {
    let server = MockServer::start().await;
    let cliente = sessao(&server).await;

    let mut publico = torrent("a1b2c3d4e5f6", "uploading", "/media/downloads/tv");
    publico["private"] = json!(false);

    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([publico])))
        .mount(&server)
        .await;

    let torrents = cliente.torrents().await.unwrap();

    assert!(!torrents[0].is_private());
}

#[tokio::test]
async fn arquivos_saem_com_caminho_relativo_ao_save_path() {
    let server = MockServer::start().await;
    let cliente = sessao(&server).await;

    let t = torrent("a1b2c3d4e5f6", "uploading", "/media/downloads/tv");

    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([t])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(query_param("hash", "a1b2c3d4e5f6"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "availability": 0,
                "index": 0,
                "is_seed": true,
                "name": "Uma.Serie.S02E03.1080p/uma.serie.s02e03.mkv",
                "piece_range": [0, 2047],
                "priority": 1,
                "progress": 1,
                "size": 2_147_000_000_i64
            },
            {
                "availability": 0,
                "index": 1,
                "is_seed": true,
                "name": "Uma.Serie.S02E03.1080p/Sample/sample.mkv",
                "piece_range": [2048, 2060],
                "priority": 0,
                "progress": 1,
                "size": 483_648
            }
        ])))
        .mount(&server)
        .await;

    let torrents = cliente.torrents().await.unwrap();
    let hash = DownloadHash::new("a1b2c3d4e5f6");
    let arquivos = cliente.files(&hash).await.expect("arquivos");

    assert_eq!(arquivos.len(), 2);
    // O caminho que sai daqui é o que o **cliente** enxerga; traduzir para o
    // caminho do host é trabalho do mapa de caminhos, não deste adaptador.
    assert_eq!(
        client_path(&torrents[0], &arquivos[0]),
        PathBuf::from("/media/downloads/tv/Uma.Serie.S02E03.1080p/uma.serie.s02e03.mkv")
    );
}

#[tokio::test]
async fn remocao_manda_os_hashes_juntos_e_diz_se_apaga_arquivo() {
    let server = MockServer::start().await;
    let cliente = sessao(&server).await;

    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .and(header("referer", server.uri() + "/"))
        .and(body_string_contains("hashes=a1b2c3d4e5f6%7Cffeeddccbbaa"))
        .and(body_string_contains("deleteFiles=true"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    cliente
        .delete(
            &[
                DownloadHash::new("a1b2c3d4e5f6"),
                DownloadHash::new("FFEEDDCCBBAA"),
            ],
            true,
        )
        .await
        .expect("remoção aceita");

    drop(server);
}

#[tokio::test]
async fn remover_lista_vazia_nao_chama_o_cliente() {
    let server = MockServer::start().await;
    let cliente = sessao(&server).await;

    // Sem mock para `torrents/delete`: qualquer requisição a ela falharia o
    // teste. Uma lista vazia que virasse POST apagaria tudo em versões que
    // tratam `hashes=` vazio como "todos".
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    cliente.delete(&[], true).await.expect("nada a fazer");

    drop(server);
}

#[tokio::test]
async fn sessao_expirada_no_meio_do_ciclo_vira_erro() {
    let server = MockServer::start().await;
    let cliente = sessao(&server).await;

    // Cookie expirado responde 403. Se isso virasse lista vazia, o ciclo
    // concluiria que nenhum seed existe.
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let erro = cliente.torrents().await.unwrap_err();

    assert!(erro.to_string().contains("403"), "erro inesperado: {erro}");
}

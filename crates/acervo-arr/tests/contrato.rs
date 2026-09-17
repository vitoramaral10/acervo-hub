//! Contrato com a API v3, exercitado contra um servidor que responde o que
//! Sonarr e Radarr respondem.
//!
//! Isto não prova que **a sua instância** se comporta assim — só um ciclo real
//! prova. Prova a outra metade: que, diante da resposta documentada, o
//! adaptador extrai o que a reconciliação precisa, pede os parâmetros certos e
//! falha quando tem que falhar. É onde adaptador quebra: nome de parâmetro,
//! paginação, campo que mudou de forma.

use std::time::Duration;

use acervo_arr::{ArrClient, ArrKind};
use acervo_core::InstanceName;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAVE: &str = "chave-de-teste";

fn cliente(server: &MockServer, kind: ArrKind) -> ArrClient {
    ArrClient::new(
        InstanceName::new("teste"),
        &server.uri(),
        CHAVE,
        kind,
        Duration::from_secs(5),
    )
    .expect("cliente válido")
}

/// Um registro de fila do Sonarr com os campos que ele realmente manda —
/// inclusive os aninhados, que o adaptador ignora e não pode quebrar por causa
/// deles.
fn registro_sonarr(id: i64, series_id: i64, download_id: &str) -> Value {
    json!({
        "id": id,
        "seriesId": series_id,
        "episodeId": 9001,
        "seasonNumber": 2,
        "series": {
            "id": series_id,
            "title": "Uma Série",
            "seasons": [{"seasonNumber": 2, "monitored": true}],
            "statistics": {"episodeFileCount": 10, "sizeOnDisk": 1_234_567_890_i64}
        },
        "episode": {"id": 9001, "episodeNumber": 3, "title": "Um Episódio"},
        "languages": [{"id": 1, "name": "English"}],
        "quality": {
            "quality": {"id": 9, "name": "HDTV-1080p", "resolution": 1080},
            "revision": {"version": 1, "real": 0, "isRepack": false}
        },
        "customFormats": [],
        "customFormatScore": 0,
        "size": 2_147_483_648_i64,
        "title": "Uma.Serie.S02E03.1080p",
        "sizeleft": 0,
        "timeleft": "00:00:00",
        "added": "2026-09-14T03:11:42Z",
        "status": "completed",
        "trackedDownloadStatus": "warning",
        "trackedDownloadState": "importPending",
        "statusMessages": [{"title": "Uma.Serie.S02E03.1080p", "messages": ["Série não encontrada"]}],
        "downloadId": download_id,
        "protocol": "torrent",
        "downloadClient": "qBittorrent",
        "indexer": "Um Indexador (Prowlarr)",
        "outputPath": "/media/downloads/Uma.Serie.S02E03.1080p",
        "episodeHasFile": false
    })
}

async fn monta_obras(server: &MockServer, rota: &str, quantas: usize) {
    let obras: Vec<Value> = (0..quantas)
        .map(|i| json!({"id": i + 1, "title": format!("Obra {i}")}))
        .collect();

    Mock::given(method("GET"))
        .and(path(rota))
        .and(header("X-Api-Key", CHAVE))
        .respond_with(ResponseTemplate::new(200).set_body_json(obras))
        .mount(server)
        .await;
}

#[tokio::test]
async fn fila_real_do_sonarr_vira_itens_do_dominio() {
    let server = MockServer::start().await;

    // O `query_param` é metade do teste: se o nome do parâmetro que inclui
    // órfãos estiver errado, a requisição não casa e o teste falha por 404 —
    // que é exatamente o modo de falha que se quer detectar aqui, já que numa
    // instância real o parâmetro errado seria ignorado em silêncio e a fila
    // voltaria sem os órfãos.
    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .and(header("X-Api-Key", CHAVE))
        .and(query_param("includeUnknownSeriesItems", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 1,
            "pageSize": 200,
            "sortKey": "timeleft",
            "sortDirection": "ascending",
            "totalRecords": 3,
            "records": [
                registro_sonarr(11, 0, "A1B2C3D4E5F6"),
                registro_sonarr(12, 42, "FFEEDDCCBBAA"),
                registro_sonarr(13, 0, ""),
            ]
        })))
        .mount(&server)
        .await;

    monta_obras(&server, "/api/v3/series", 128).await;

    let snap = cliente(&server, ArrKind::Series)
        .snapshot()
        .await
        .expect("instância respondeu");

    assert_eq!(snap.known_works, 128);
    assert_eq!(snap.queue.len(), 3);

    // `seriesId: 0` é como a v3 diz "sem dono" — não vem `null`.
    assert!(snap.queue[0].is_orphaned());
    assert_eq!(
        snap.queue[0].download.as_ref().unwrap().as_str(),
        "a1b2c3d4e5f6"
    );

    assert!(!snap.queue[1].is_orphaned());

    // Órfão sem hash continua sendo órfão: some da fila, mas não há torrent
    // para casar no cliente.
    assert!(snap.queue[2].is_orphaned());
    assert!(snap.queue[2].download.is_none());
}

#[tokio::test]
async fn instancia_de_filmes_pede_o_parametro_de_filmes() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .and(query_param("includeUnknownMovieItems", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRecords": 1,
            "records": [{"id": 5, "title": "Um Filme", "movieId": 0, "downloadId": "ABC"}]
        })))
        .mount(&server)
        .await;

    monta_obras(&server, "/api/v3/movie", 7).await;

    let snap = cliente(&server, ArrKind::Movie).snapshot().await.unwrap();

    assert_eq!(snap.known_works, 7);
    assert!(snap.queue[0].is_orphaned());
}

#[tokio::test]
async fn paginacao_percorre_todas_as_paginas() {
    let server = MockServer::start().await;

    let pagina_cheia: Vec<Value> = (0..200).map(|i| registro_sonarr(i, 0, "")).collect();
    let pagina_curta: Vec<Value> = (200..250).map(|i| registro_sonarr(i, 0, "")).collect();

    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRecords": 250, "records": pagina_cheia
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRecords": 250, "records": pagina_curta
        })))
        .mount(&server)
        .await;

    monta_obras(&server, "/api/v3/series", 1).await;

    let snap = cliente(&server, ArrKind::Series).snapshot().await.unwrap();

    // Truncar a fila aqui seria pior que falhar: item que ficou de fora vira
    // "não está em fila nenhuma" na etapa seguinte, e seed legítimo entra na
    // lista de remoção.
    assert_eq!(snap.queue.len(), 250);
}

#[tokio::test]
async fn resposta_sem_total_records_encerra_pela_pagina_curta() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "records": [registro_sonarr(1, 0, "AA")]
        })))
        .mount(&server)
        .await;

    monta_obras(&server, "/api/v3/series", 1).await;

    let snap = cliente(&server, ArrKind::Series).snapshot().await.unwrap();

    assert_eq!(snap.queue.len(), 1);
}

#[tokio::test]
async fn fila_vazia_nao_e_erro() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRecords": 0, "records": []
        })))
        .mount(&server)
        .await;

    monta_obras(&server, "/api/v3/series", 300).await;

    let snap = cliente(&server, ArrKind::Series).snapshot().await.unwrap();

    assert!(snap.queue.is_empty());
    assert_eq!(snap.known_works, 300);
}

#[tokio::test]
async fn erro_na_contagem_de_obras_derruba_o_instantaneo_inteiro() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalRecords": 0, "records": []
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v3/series"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    // Fila boa e inventário quebrado não pode virar "instância viva com zero
    // obras": nesse estado toda a fila parece órfã.
    let erro = cliente(&server, ArrKind::Series)
        .snapshot()
        .await
        .unwrap_err();

    assert_eq!(erro.short(), "HTTP 500 Internal Server Error");
}

#[tokio::test]
async fn pagina_de_login_do_proxy_reverso_nao_vira_fila_vazia() {
    let server = MockServer::start().await;

    // Sessão expirada num proxy à frente da instância devolve 200 com HTML.
    // Se isso desserializasse para fila vazia, o ciclo seguiria achando que a
    // instância está viva e sem nada em fila.
    Mock::given(method("GET"))
        .and(path("/api/v3/queue"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<!DOCTYPE html><html><body>Entrar</body></html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;

    monta_obras(&server, "/api/v3/series", 1).await;

    assert!(cliente(&server, ArrKind::Series).snapshot().await.is_err());
}

#[tokio::test]
async fn remocao_pede_para_nao_rebaixar_nem_procurar_substituto() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/api/v3/queue/77"))
        .and(header("X-Api-Key", CHAVE))
        .and(query_param("removeFromClient", "false"))
        .and(query_param("blocklist", "false"))
        .and(query_param("skipRedownload", "true"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    cliente(&server, ArrKind::Series)
        .remove_queue_item(acervo_core::QueueItemId(77), false)
        .await
        .expect("remoção aceita");

    // `expect(1)` acima falha no drop se a requisição não tiver saído com
    // exatamente esses parâmetros.
    drop(server);
}

#[tokio::test]
async fn remocao_recusada_propaga_o_status() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/api/v3/queue/77"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let erro = cliente(&server, ArrKind::Series)
        .remove_queue_item(acervo_core::QueueItemId(77), true)
        .await
        .unwrap_err();

    assert_eq!(erro.short(), "HTTP 404 Not Found");
}

#[tokio::test]
async fn instancia_fora_do_ar_falha_em_vez_de_travar() {
    // Endereço reservado para documentação: não roteia, então o cliente bate
    // no timeout em vez de receber recusa imediata.
    let cliente = ArrClient::new(
        InstanceName::new("morta"),
        "http://192.0.2.1:7878",
        CHAVE,
        ArrKind::Series,
        Duration::from_millis(300),
    )
    .unwrap();

    let erro = cliente.snapshot().await.unwrap_err();

    // O ponto não é o texto: é que o timeout do adaptador fecha antes de
    // qualquer coisa lá fora decidir por ele.
    assert!(
        matches!(erro.short().as_str(), "timeout" | "conexão recusada"),
        "erro inesperado: {erro}"
    );
}

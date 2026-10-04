//! Numeração de cena, do XEM: há séries em que o release sai com uma
//! temporada e um número e o catálogo (TVDB, e o TMDB que o segue) chama o
//! mesmo episódio de outro jeito. A tarefa `cena` baixa, uma vez por dia,
//! a lista de séries com mapa e o mapa de cada uma que está no catálogo; a
//! decisão, a importação e o verificar disco traduzem com ele.
//!
//! O mapa vem pela numeração do TVDB; o catálogo segue o TMDB, que na
//! quase totalidade das séries numera igual. Falha aqui não afeta o resto:
//! sem mapa, o release é lido como vem.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use acervo_store::{SceneMapping, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::config::Config;

/// Os tvdb ids da lista `havemap`. O XEM manda os ids como texto; número
/// também vale.
///
/// # Errors
///
/// Resposta que não é de sucesso.
pub fn parse_havemap(value: &Value) -> Result<HashSet<u32>> {
    if value["result"] != "success" {
        bail!(
            "o XEM recusou a lista de séries: {}",
            value["message"].as_str().unwrap_or("sem mensagem")
        );
    }
    Ok(value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| match id {
            Value::String(text) => text.trim().parse().ok(),
            Value::Number(number) => number.as_u64().and_then(|n| u32::try_from(n).ok()),
            _ => None,
        })
        .collect())
}

/// Os pares de `map/all`. Cada par de cena leva todos os alvos do TVDB
/// (`tvdb`, e `tvdb_2`, `tvdb_3`… no episódio duplo); só sai o par de cena
/// cujo único alvo é ele mesmo, que não diz nada. Resposta de falha ("sem
/// mapa para esta série") é mapa vazio. Sai na ordem do catálogo: cena,
/// depois alvo.
#[must_use]
pub fn parse_all(value: &Value) -> Vec<SceneMapping> {
    let pair = |side: &Value| -> Option<(u16, u16)> {
        Some((
            u16::try_from(side["season"].as_u64()?).ok()?,
            u16::try_from(side["episode"].as_u64()?).ok()?,
        ))
    };
    if value["result"] != "success" {
        return Vec::new();
    }
    let mut groups: BTreeMap<(u16, u16), BTreeSet<(u16, u16)>> = BTreeMap::new();
    for entry in value["data"].as_array().into_iter().flatten() {
        let Some(scene) = pair(&entry["scene"]) else {
            continue;
        };
        let targets = entry
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| *key == "tvdb" || key.starts_with("tvdb_"))
            .filter_map(|(_, side)| pair(side));
        groups.entry(scene).or_default().extend(targets);
    }
    groups
        .into_iter()
        .filter(|(scene, targets)| !targets.is_empty() && targets.iter().ne([scene]))
        .flat_map(|((scene_season, scene_episode), targets)| {
            targets
                .into_iter()
                .map(move |(season, episode)| SceneMapping {
                    scene_season,
                    scene_episode,
                    season,
                    episode,
                })
        })
        .collect()
}

async fn get(http: &reqwest::Client, url: url::Url) -> Result<Value> {
    let shown = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
    let response = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("consultando o XEM ({shown})"))?;
    if !response.status().is_success() {
        bail!("o XEM respondeu {} em {shown}", response.status());
    }
    response
        .json()
        .await
        .with_context(|| format!("resposta ilegível do XEM ({shown})"))
}

fn endpoint(base: &str, path: &str, query: &[(&str, &str)]) -> Result<url::Url> {
    let mut url = url::Url::parse(&format!("{}/{path}", base.trim_end_matches('/')))
        .context("endereço do XEM inválido")?;
    url.query_pairs_mut().extend_pairs(query);
    Ok(url)
}

/// Os tvdb ids que o XEM tem mapa.
///
/// # Errors
///
/// XEM inalcançável ou resposta de falha.
pub async fn havemap(http: &reqwest::Client, base: &str) -> Result<HashSet<u32>> {
    let url = endpoint(base, "map/havemap", &[("origin", "tvdb")])?;
    parse_havemap(&get(http, url).await?)
}

/// O mapa de uma série, pelo tvdb id.
///
/// # Errors
///
/// XEM inalcançável.
pub async fn mappings(http: &reqwest::Client, base: &str, tvdb: u32) -> Result<Vec<SceneMapping>> {
    let id = tvdb.to_string();
    let url = endpoint(base, "map/all", &[("id", id.as_str()), ("origin", "tvdb")])?;
    Ok(parse_all(&get(http, url).await?))
}

/// O que a tarefa `cena` fez.
#[derive(Debug, Default, Serialize)]
pub struct SceneReport {
    /// Séries do catálogo com mapa no XEM.
    pub com_mapa: usize,
    /// Séries cujo mapa mudou.
    pub atualizadas: Vec<String>,
    pub falhas: Vec<(String, String)>,
}

/// Baixa a lista do XEM e o mapa de cada série do catálogo que está nela;
/// série que saiu da lista perde o mapa guardado.
///
/// # Errors
///
/// Banco ilegível ou lista do XEM inalcançável. Falha numa série fica no
/// relato; as outras seguem.
pub async fn refresh(config: &Config, store: &Store) -> Result<SceneReport> {
    let http = reqwest::Client::builder()
        .timeout(config.http_timeout())
        .user_agent("acervo-hub")
        .build()?;
    let base = &config.server.xem_url;
    let list = store.series_list().await?;
    let mut report = SceneReport::default();
    if list.is_empty() {
        return Ok(report);
    }
    let known = havemap(&http, base).await?;
    for entry in &list {
        let label = entry.series.title.clone();
        let wanted = match entry.series.tvdb_id.filter(|id| known.contains(id)) {
            Some(tvdb) => {
                report.com_mapa += 1;
                match mappings(&http, base, tvdb).await {
                    Ok(found) => found,
                    Err(error) => {
                        report.falhas.push((label, format!("{error:#}")));
                        continue;
                    }
                }
            }
            None => Vec::new(),
        };
        if wanted != entry.scene {
            store.set_scene_mappings(entry.id, &wanted).await?;
            report.atualizadas.push(label);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn lista_aceita_id_em_texto_ou_numero_e_falha_e_erro() {
        let ids = parse_havemap(&json!({
            "result": "success", "data": ["12345", 678, "lixo"], "message": ""
        }))
        .unwrap();
        assert_eq!(ids, HashSet::from([12345, 678]));
        assert!(parse_havemap(&json!({ "result": "failure", "data": [] })).is_err());
    }

    #[test]
    fn mapa_guarda_so_o_que_difere() {
        let value = json!({
            "result": "success",
            "data": [
                { "scene": { "season": 1, "episode": 1, "absolute": 1 },
                  "tvdb": { "season": 1, "episode": 1, "absolute": 1 } },
                { "scene": { "season": 1, "episode": 13, "absolute": 13 },
                  "tvdb": { "season": 2, "episode": 1, "absolute": 13 } },
                { "scene": { "season": 1, "episode": 14 } },
            ],
            "message": "",
        });
        assert_eq!(
            parse_all(&value),
            [SceneMapping {
                scene_season: 1,
                scene_episode: 13,
                season: 2,
                episode: 1,
            }]
        );
        assert!(parse_all(&json!({ "result": "failure", "data": [] })).is_empty());
    }

    #[test]
    fn episodio_duplo_guarda_os_dois_alvos_com_a_identidade() {
        let map = |ss, se, s, e| SceneMapping {
            scene_season: ss,
            scene_episode: se,
            season: s,
            episode: e,
        };
        let value = json!({
            "result": "success",
            "data": [
                { "scene": { "season": 1, "episode": 1 },
                  "tvdb": { "season": 1, "episode": 1 },
                  "tvdb_2": { "season": 1, "episode": 2 } },
                { "scene": { "season": 1, "episode": 2 },
                  "tvdb": { "season": 1, "episode": 3 } },
                { "scene": { "season": 1, "episode": 3 },
                  "tvdb": { "season": 1, "episode": 3 } },
            ],
        });
        assert_eq!(
            parse_all(&value),
            [map(1, 1, 1, 1), map(1, 1, 1, 2), map(1, 2, 1, 3)]
        );
    }

    #[tokio::test]
    async fn consulta_o_xem_pelo_endereco_configurado() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/map/havemap"))
            .and(query_param("origin", "tvdb"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "result": "success", "data": ["42"] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/map/all"))
            .and(query_param("id", "42"))
            .and(query_param("origin", "tvdb"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": "success",
                "data": [{ "scene": { "season": 3, "episode": 1 },
                           "tvdb": { "season": 2, "episode": 10 } }],
            })))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let base = format!("{}/", server.uri());
        assert_eq!(havemap(&http, &base).await.unwrap(), HashSet::from([42]));
        let found = mappings(&http, &base, 42).await.unwrap();
        assert_eq!(
            found,
            [SceneMapping {
                scene_season: 3,
                scene_episode: 1,
                season: 2,
                episode: 10,
            }]
        );
        // Fora do ar: erro, não pânico.
        let down = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&down)
            .await;
        assert!(havemap(&http, &down.uri()).await.is_err());
    }
}

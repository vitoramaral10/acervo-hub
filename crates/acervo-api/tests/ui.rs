//! Contrato da API da interface web, por HTTP de verdade.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acervo_api::{Admin, Catalog, DefinitionView, Entry, SettingView, router_with_admin};
use acervo_indexers::{
    Capabilities, Category, Indexer, IndexerError, Release, SearchQuery, SearchSupport,
};
use async_trait::async_trait;
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};

const KEY: &str = "chave-da-interface-0123456789";

#[derive(Debug)]
struct Private {
    name: String,
    cookie: String,
}

#[async_trait]
impl Indexer for Private {
    fn name(&self) -> &str {
        &self.name
    }

    async fn search(&self, _query: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        if self.cookie != "bom" {
            return Err(IndexerError::Login {
                indexer: self.name.clone(),
                reason: "o site recusou o cookie (vencido?); copie um novo do navegador logado",
            });
        }
        Ok(vec![Release {
            indexer: self.name.clone(),
            guid: "g".into(),
            title: "Série <b>.S01E01".into(),
            download_url: url::Url::parse("https://privado.invalid/dl/1?pass=x").unwrap(),
            info_url: Some(url::Url::parse("https://privado.invalid/t/1").unwrap()),
            size: 1024,
            published: None,
            seeders: Some(3),
            leechers: Some(1),
            grabs: None,
            categories: vec![5040],
            tags: Vec::new(),
        }])
    }

    fn proxies_downloads(&self) -> bool {
        true
    }
}

fn caps() -> Capabilities {
    Capabilities {
        general: SearchSupport {
            available: true,
            supported_params: ["q".to_owned()].into(),
        },
        categories: vec![Category {
            id: 5040,
            name: "TV/HD".into(),
            parent: Some(5000),
        }],
        ..Capabilities::default()
    }
}

fn entry(cookie: &str) -> Entry {
    Entry {
        indexer: Arc::new(Private {
            name: "privado".into(),
            cookie: cookie.into(),
        }),
        capabilities: caps(),
    }
}

/// Admin falso: guarda o que recebeu e remonta o indexador com o cookie novo.
#[derive(Debug, Default)]
struct FakeAdmin {
    saved: Mutex<BTreeMap<String, String>>,
    disabled: Mutex<Vec<String>>,
}

#[async_trait]
impl Admin for FakeAdmin {
    fn settings(&self, indexer: &str) -> Option<Vec<SettingView>> {
        (indexer == "privado").then(|| {
            vec![SettingView {
                name: "cookie".into(),
                label: "Cookie".into(),
                kind: "text",
                options: Vec::new(),
                secret: true,
                is_set: self.saved.lock().unwrap().contains_key("cookie"),
                value: None,
            }]
        })
    }

    async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        if indexer != "privado" {
            return Err("indexador sem settings editáveis".into());
        }
        let cookie = values.get("cookie").cloned().unwrap_or_default();
        self.saved.lock().unwrap().extend(values);
        Ok(entry(&cookie))
    }

    fn origin(&self, indexer: &str) -> Option<&'static str> {
        match indexer {
            "privado" => Some("config"),
            "novo" => Some("interface"),
            _ => None,
        }
    }

    fn disabled(&self) -> Vec<(String, &'static str)> {
        self.disabled
            .lock()
            .unwrap()
            .iter()
            .map(|name| (name.clone(), "config"))
            .collect()
    }

    fn definitions(&self) -> Vec<DefinitionView> {
        vec![
            DefinitionView {
                id: "novo".into(),
                name: "Novo".into(),
                description: String::new(),
                language: "pt-BR".into(),
                private: false,
                supported: true,
                reason: None,
                added: false,
            },
            DefinitionView {
                id: "api-json".into(),
                name: "Tracker JSON".into(),
                description: String::new(),
                language: "en-US".into(),
                private: true,
                supported: false,
                reason: Some("chave `response` não suportada".into()),
                added: false,
            },
        ]
    }

    fn definition_settings(&self, definition: &str) -> Option<Vec<SettingView>> {
        (definition == "novo").then(Vec::new)
    }

    async fn add(
        &self,
        definition: &str,
        _values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        if definition != "novo" {
            return Err("definição desconhecida".into());
        }
        Ok(Entry {
            indexer: Arc::new(Private {
                name: "novo".into(),
                cookie: "bom".into(),
            }),
            capabilities: caps(),
        })
    }

    async fn remove(&self, indexer: &str) -> Result<(), String> {
        if indexer == "privado" {
            return Err("indexador do config.toml".into());
        }
        Ok(())
    }

    async fn set_enabled(&self, indexer: &str, enabled: bool) -> Result<Option<Entry>, String> {
        let mut disabled = self.disabled.lock().unwrap();
        if enabled {
            disabled.retain(|name| name != indexer);
            Ok(Some(entry("bom")))
        } else {
            disabled.push(indexer.to_owned());
            Ok(None)
        }
    }

    fn apps(&self) -> Value {
        json!({ "instancias": [{ "nome": "sonarr", "tipo": "series" }] })
    }

    async fn sync(
        &self,
        indexers: Vec<(String, Capabilities)>,
        apply: bool,
    ) -> Result<Value, String> {
        Ok(json!({ "aplicado": apply, "indexadores": indexers.len() }))
    }

    fn last_cycle(&self) -> Option<Value> {
        None
    }

    async fn simulate_cycle(&self) -> Result<Value, String> {
        Ok(json!({ "modo": "simulacao" }))
    }
}

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let catalog = Catalog::new([entry("vencido")]).unwrap();
    let admin: Arc<dyn Admin> = Arc::new(FakeAdmin::default());
    let app = router_with_admin(catalog, KEY, Some(admin));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn login(base: &str) -> String {
    let response = http()
        .post(format!("{base}/ui/api/entrar"))
        .header("X-Acervo", "1")
        .json(&json!({ "chave": KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let cookie = response.headers()[SET_COOKIE].to_str().unwrap().to_owned();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    cookie.split(';').next().unwrap().to_owned()
}

async fn get(base: &str, path: &str, cookie: &str) -> (u16, Value) {
    let response = http()
        .get(format!("{base}{path}"))
        .header(COOKIE, cookie)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn pagina_carrega_com_csp_e_api_exige_sessao() {
    let base = serve().await;
    let page = http().get(format!("{base}/")).send().await.unwrap();
    assert_eq!(page.status().as_u16(), 200);
    let csp = page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"));
    assert!(page.text().await.unwrap().contains("/ui/app.js"));
    assert_eq!(
        http()
            .get(format!("{base}/ui/app.js"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );

    let (status, _) = get(&base, "/ui/api/indexadores", "").await;
    assert_eq!(status, 401);
    let (status, _) = get(&base, "/ui/api/indexadores", "acervo_sessao=errada").await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn entrar_exige_a_chave_certa_e_o_cabecalho_da_interface() {
    let base = serve().await;
    let without_header = http()
        .post(format!("{base}/ui/api/entrar"))
        .json(&json!({ "chave": KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(without_header.status().as_u16(), 403);
    let wrong = http()
        .post(format!("{base}/ui/api/entrar"))
        .header("X-Acervo", "1")
        .json(&json!({ "chave": "errada" }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 401);
    assert!(wrong.headers().get(SET_COOKIE).is_none());

    let cookie = login(&base).await;
    let (status, _) = get(&base, "/ui/api/sessao", &cookie).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn acao_sem_cabecalho_anti_csrf_e_recusada_mesmo_com_sessao() {
    let base = serve().await;
    let cookie = login(&base).await;
    let response = http()
        .post(format!("{base}/ui/api/indexadores/privado/testar"))
        .header(COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
}

#[tokio::test]
async fn cookie_vencido_aparece_na_saude_e_trocar_pela_tela_conserta() {
    let base = serve().await;
    let cookie = login(&base).await;

    // Uma busca com cookie vencido registra a falha na saúde do indexador.
    let (status, body) = get(&base, "/ui/api/busca?q=serie", &cookie).await;
    assert_eq!(status, 502);
    assert!(body["erro"].as_str().unwrap().contains("falharam"));
    let (_, list) = get(&base, "/ui/api/indexadores", &cookie).await;
    let indexer = &list["indexadores"][0];
    assert_eq!(indexer["nome"], "privado");
    assert_eq!(indexer["editavel"], true);
    assert_eq!(indexer["saude"]["falhas_seguidas"], 1);
    assert!(
        indexer["saude"]["ultimo_erro"]
            .as_str()
            .unwrap()
            .contains("vencido")
    );

    // O formulário não recebe o valor do segredo.
    let (_, settings) = get(&base, "/ui/api/indexadores/privado/settings", &cookie).await;
    assert_eq!(settings["settings"][0]["secret"], true);
    assert_eq!(settings["settings"][0]["value"], Value::Null);

    // Salvar troca o indexador e testa na hora.
    let saved: Value = http()
        .put(format!("{base}/ui/api/indexadores/privado/settings"))
        .header(COOKIE, &cookie)
        .header("X-Acervo", "1")
        .json(&json!({ "cookie": "bom" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["teste"], json!({ "ok": true, "resultados": 1 }));

    let (_, list) = get(&base, "/ui/api/indexadores", &cookie).await;
    assert_eq!(list["indexadores"][0]["saude"]["falhas_seguidas"], 0);

    // Busca agora funciona, e o download sai pela sessão, não direto.
    let (status, body) = get(&base, "/ui/api/busca?q=serie&cat=5000", &cookie).await;
    assert_eq!(status, 200);
    let release = &body["resultados"][0];
    assert_eq!(release["titulo"], "Série <b>.S01E01");
    assert!(
        release["download"]
            .as_str()
            .unwrap()
            .starts_with("/ui/baixar?indexador=privado&link=")
    );
    assert_eq!(release["detalhes"], "https://privado.invalid/t/1");
}

#[tokio::test]
async fn indexador_sem_admin_ou_desconhecido_nao_edita() {
    let base = serve().await;
    let cookie = login(&base).await;
    let (status, _) = get(&base, "/ui/api/indexadores/outro/settings", &cookie).await;
    assert_eq!(status, 404);
    let response = http()
        .put(format!("{base}/ui/api/indexadores/outro/settings"))
        .header(COOKIE, &cookie)
        .header("X-Acervo", "1")
        .json(&json!({ "cookie": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 422);
}

async fn send(
    base: &str,
    method: reqwest::Method,
    path: &str,
    cookie: &str,
    body: Value,
) -> (u16, Value) {
    let response = http()
        .request(method, format!("{base}{path}"))
        .header(COOKIE, cookie)
        .header("X-Acervo", "1")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn catalogo_lista_suportadas_e_recusadas_com_motivo() {
    let base = serve().await;
    let cookie = login(&base).await;
    let (status, body) = get(&base, "/ui/api/catalogo", &cookie).await;
    assert_eq!(status, 200);
    let list = body["definicoes"].as_array().unwrap();
    assert_eq!(list[0]["supported"], true);
    assert_eq!(list[1]["supported"], false);
    assert!(list[1]["reason"].as_str().unwrap().contains("response"));
}

#[tokio::test]
async fn adicionar_testa_e_entra_no_catalogo_e_remover_tira() {
    let base = serve().await;
    let cookie = login(&base).await;

    let (status, body) = send(
        &base,
        reqwest::Method::POST,
        "/ui/api/indexadores",
        &cookie,
        json!({ "definicao": "novo", "settings": {} }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["nome"], "novo");
    assert_eq!(body["teste"]["ok"], true);
    let (_, list) = get(&base, "/ui/api/indexadores", &cookie).await;
    let names: Vec<_> = list["indexadores"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["nome"].clone())
        .collect();
    assert!(names.contains(&json!("novo")));
    // Aparece servido pela API Torznab também.
    let (status, _) = get(&base, &format!("/novo/api?t=caps&apikey={KEY}"), "").await;
    assert_eq!(status, 200);

    // Indexador do config não se remove pela tela.
    let (status, _) = send(
        &base,
        reqwest::Method::DELETE,
        "/ui/api/indexadores/privado",
        &cookie,
        json!({}),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _) = send(
        &base,
        reqwest::Method::DELETE,
        "/ui/api/indexadores/novo",
        &cookie,
        json!({}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = get(&base, &format!("/novo/api?t=caps&apikey={KEY}"), "").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn desativar_tira_de_circulacao_e_mantem_na_lista() {
    let base = serve().await;
    let cookie = login(&base).await;
    let (status, _) = send(
        &base,
        reqwest::Method::PUT,
        "/ui/api/indexadores/privado/ativo",
        &cookie,
        json!({ "ativo": false }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = get(&base, &format!("/privado/api?t=caps&apikey={KEY}"), "").await;
    assert_eq!(status, 404);
    let (_, list) = get(&base, "/ui/api/indexadores", &cookie).await;
    let privado = list["indexadores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["nome"] == "privado")
        .unwrap()
        .clone();
    assert_eq!(privado["ativo"], false);

    let (status, _) = send(
        &base,
        reqwest::Method::PUT,
        "/ui/api/indexadores/privado/ativo",
        &cookie,
        json!({ "ativo": true }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = get(&base, &format!("/privado/api?t=caps&apikey={KEY}"), "").await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn sincronizar_recebe_os_indexadores_servidos() {
    let base = serve().await;
    let cookie = login(&base).await;
    let (status, body) = send(
        &base,
        reqwest::Method::POST,
        "/ui/api/aplicativos/sincronizar",
        &cookie,
        json!({ "aplicar": false }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "aplicado": false, "indexadores": 1 }));
}

#[tokio::test]
async fn credencial_de_indexador_desativado_e_salva_sem_erro() {
    let base = serve().await;
    let cookie = login(&base).await;
    send(
        &base,
        reqwest::Method::PUT,
        "/ui/api/indexadores/privado/ativo",
        &cookie,
        json!({ "ativo": false }),
    )
    .await;
    let (status, body) = send(
        &base,
        reqwest::Method::PUT,
        "/ui/api/indexadores/privado/settings",
        &cookie,
        json!({ "cookie": "bom" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
    assert!(
        body["teste"]["erro"]
            .as_str()
            .unwrap()
            .contains("desativado")
    );
}

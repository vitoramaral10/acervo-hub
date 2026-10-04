//! Recursos do executor Cardigann além do HTML/GET básico: resposta JSON, POST,
//! caminho com template, login por formulário/get/oneurl, bloco `download`,
//! `rows.after`, `dateheaders`, codificações e as definições de produção.
//!
//! As definições abaixo são escritas à mão, com nomes genéricos: reproduzem a
//! estrutura que o motor de referência lê, não o HTML nem o nome de nenhum site.

use std::collections::BTreeMap;
use std::time::Duration;

use acervo_indexers::{
    CardigannClient, CardigannDefinition, Indexer, IndexerError, ResolvedDownload, SearchQuery,
};
use wiremock::matchers::{
    body_string_contains, header, header_regex, method, path, query_param, query_param_is_missing,
};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const HEAD: &str = "id: fixture-recursos
name: Fixture
description: Definição escrita à mão para o teste
language: pt-BR
type: public
encoding: UTF-8
requestDelay: 0
links:
  - https://tracker.invalid/
caps:
  categorymappings:
    - {id: 10, cat: Movies, desc: Filmes}
    - {id: 20, cat: TV, desc: Séries}
  modes:
    search: [q]
";

fn definition(yaml: &str, server: &MockServer) -> CardigannDefinition {
    let yaml = yaml.replace("https://tracker.invalid/", &format!("{}/", server.uri()));
    CardigannDefinition::from_yaml_v11(&yaml).unwrap_or_else(|error| panic!("{error}"))
}

fn client(yaml: &str, server: &MockServer, settings: &[(&str, &str)]) -> CardigannClient {
    CardigannClient::new(
        definition(yaml, server),
        0,
        settings
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>(),
        Duration::from_secs(5),
    )
    .unwrap()
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8")
}

fn json(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/json")
}

/// A query crua, como o servidor a recebeu: confere os bytes percent-encoded.
struct RawQuery(&'static str);

impl Match for RawQuery {
    fn matches(&self, request: &Request) -> bool {
        request.url.query() == Some(self.0)
    }
}

fn load_error(yaml: &str) -> String {
    match CardigannDefinition::from_yaml_v11(yaml) {
        Ok(_) => panic!("a definição deveria ser recusada"),
        Err(error) => error.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Resposta JSON.

const JSON_YAML: &str = "
settings:
  - {name: apikey, type: password, label: Chave, default: chave-fixture}
  - {name: freeleech, type: checkbox, label: Livre, default: false}
search:
  paths:
    - path: api/lista
      response:
        type: json
        noResultsMessage: Nada encontrado
  headers:
    Authorization: [\"Bearer {{ .Config.apikey }}\"]
  inputs:
    $raw: \"{{ range .Categories }}&cat[]={{.}}{{end}}\"
    q: \"{{ .Keywords }}\"
    livre: \"{{ if .Config.freeleech }}1{{ else }}{{ end }}\"
  rows:
    selector: \"dados.itens{{ if .Config.freeleech }}:not(promo.down:contains(1)){{ else }}{{ end }}\"
    attribute: attrs
    count:
      selector: dados.total
  fields:
    title:
      selector: nome
    details:
      selector: links.pagina
    download:
      selector: links.arquivo
    size:
      selector: tamanho
    seeders:
      selector: sem
    leechers:
      selector: peers.leech
      optional: true
      default: 0
    category:
      selector: tipo
    date:
      selector: criado
";

const JSON_BODY: &str = r#"{"dados":{"total":3,"itens":[
  {"attrs":{"nome":"Filme Um 1080p","links":{"pagina":"/t/1","arquivo":"/d/1.torrent"},
            "tamanho":1073741824,"sem":12,"tipo":"10","criado":"2026-09-24T10:00:00Z"},
   "promo":{"down":0}},
  {"attrs":{"nome":"Serie Dois","links":{"pagina":"/t/2","arquivo":"/d/2.torrent"},
            "tamanho":"500 MB","sem":3,"peers":{"leech":4},"tipo":"20"},
   "promo":{"down":1}},
  {"semattrs":true}
]}}"#;

#[tokio::test]
async fn json_le_linhas_atributo_campos_cabecalho_com_template_e_raw() {
    let server = MockServer::start().await;
    Mock::given(path("/api/lista"))
        .and(header("authorization", "Bearer chave-fixture"))
        .and(query_param("q", "filme"))
        .and(query_param("cat[]", "10"))
        .and(query_param_is_missing("livre"))
        .respond_with(json(JSON_BODY))
        .expect(1)
        .mount(&server)
        .await;

    let results = client(&format!("{HEAD}{JSON_YAML}"), &server, &[])
        .search(&SearchQuery::general("filme").with_categories([2000, 5000]))
        .await
        .unwrap();

    // A terceira linha não tem `attrs` e é pulada; as outras duas rendem.
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Filme Um 1080p");
    assert_eq!(results[0].size, 1_073_741_824);
    assert_eq!(results[0].seeders, Some(12));
    assert_eq!(results[0].leechers, Some(0));
    assert_eq!(results[0].categories, [2000]);
    assert_eq!(
        results[0].published.unwrap().unix_timestamp(),
        1_790_244_000
    );
    assert_eq!(
        results[0].download_url.as_str(),
        format!("{}/d/1.torrent", server.uri())
    );
    assert_eq!(
        results[0].info_url.as_ref().unwrap().as_str(),
        format!("{}/t/1", server.uri())
    );
    assert_eq!(results[1].title, "Serie Dois");
    assert_eq!(results[1].size, 500 * 1024 * 1024);
    assert_eq!(results[1].leechers, Some(4));
    assert_eq!(results[1].categories, [5000]);
}

#[tokio::test]
async fn json_seletor_de_linhas_com_template_e_filtro_not_contains() {
    let server = MockServer::start().await;
    Mock::given(path("/api/lista"))
        .and(query_param("livre", "1"))
        .respond_with(json(JSON_BODY))
        .mount(&server)
        .await;

    let results = client(
        &format!("{HEAD}{JSON_YAML}"),
        &server,
        &[("freeleech", "true")],
    )
    .search(&SearchQuery::general("filme"))
    .await
    .unwrap();

    // `:not(promo.down:contains(1))` tira a segunda linha.
    let titles: Vec<_> = results.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["Filme Um 1080p"]);
}

#[tokio::test]
async fn json_sem_resultado_por_mensagem_contagem_e_corpo_nao_json() {
    let server = MockServer::start().await;
    let search = |server: &MockServer| {
        let client = client(&format!("{HEAD}{JSON_YAML}"), server, &[]);
        async move { client.search(&SearchQuery::general("x")).await }
    };

    // `noResultsMessage` presente no corpo.
    Mock::given(path("/api/lista"))
        .respond_with(json("Nada encontrado"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    assert!(search(&server).await.unwrap().is_empty());

    // `rows.count` abaixo de 1: nem lê as linhas.
    Mock::given(path("/api/lista"))
        .respond_with(json(r#"{"dados":{"total":0,"itens":"lixo"}}"#))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    assert!(search(&server).await.unwrap().is_empty());

    // Corpo que não é JSON, ou sem o caminho das linhas, é erro: o site mudou.
    for body in ["<html>manutenção</html>", r#"{"dados":{"total":1}}"#] {
        Mock::given(path("/api/lista"))
            .respond_with(json(body))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert!(matches!(
            search(&server).await,
            Err(IndexerError::UnexpectedDocument { .. })
        ));
    }
}

#[tokio::test]
async fn json_multiple_seletor_do_pai_e_campos_com_append() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: api/grupos
      response: {{type: json}}
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  rows:
    selector: grupos
    attribute: filhos
    multiple: true
    missingAttributeEqualsNoResults: true
  fields:
    title:
      selector: nome
    _grupo:
      selector: ..nome_grupo
    title|append:
      text: ' [{{{{ .Result._grupo }}}}]'
    download:
      selector: link
    size:
      selector: bytes
    category:
      selector: tipo
      case:
        'True': '10'
        '*': '20'
"
    );
    let server = MockServer::start().await;
    Mock::given(path("/api/grupos"))
        .respond_with(json(
            r#"{"grupos":[
                {"nome_grupo":"G1","filhos":{"a":{"nome":"T1","link":"/a.torrent","bytes":10,"tipo":true},
                                              "b":{"nome":"T2","link":"/b.torrent","bytes":20,"tipo":"x"},
                                              "c":"nao e objeto"}},
                {"nome_grupo":"G2"}
            ]}"#,
        ))
        .mount(&server)
        .await;

    let results = client(&yaml, &server, &[])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    let titles: Vec<_> = results.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["T1 [G1]", "T2 [G1]"]);
    // O `case` compara com o texto .NET do JSON: `True`, e `*` pega o resto.
    assert_eq!(results[0].categories, [2000]);
    assert_eq!(results[1].categories, [5000]);
}

#[test]
fn json_recusa_o_que_nao_le_com_motivo() {
    let base = format!("{HEAD}{JSON_YAML}");
    assert!(load_error(&base.replace("type: json", "type: xml")).contains("XML"));
    assert!(load_error(&base.replace("selector: nome", "selector: 'a[*].b'")).contains("JSON"));
    assert!(
        load_error(&base.replace("selector: nome", "{attribute: href, selector: nome}"))
            .contains("JSON")
    );
}

// ---------------------------------------------------------------------------
// POST e caminho com template.

#[tokio::test]
async fn post_na_busca_manda_formulario_e_o_caminho_vai_codificado() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: 'buscar/{{{{ .Keywords }}}}/pagina'
      method: post
      inputs:
        ordem: seeders
  inputs:
    termo: '{{{{ .Keywords }}}}'
    vazio: ''
  rows:
    selector: li.item
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/buscar/uma%20s%C3%A9rie%20%26%20mais/pagina"))
        .and(body_string_contains("termo=uma+s%C3%A9rie+%26+mais"))
        .and(body_string_contains("ordem=seeders"))
        .and(query_param_is_missing("termo"))
        .respond_with(html(
            r#"<ul><li class="item"><a href="/d/1.torrent">Um</a></li></ul>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let results = client(&yaml, &server, &[])
        .search(&SearchQuery::general("uma série & mais"))
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Um");
}

#[test]
fn caminho_com_template_continua_preso_a_origem_do_link() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: 'https://outro.invalid/{{{{ .Keywords }}}}'
  rows: {{selector: tr}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    assert!(load_error(&yaml).contains("origem"));
}

#[tokio::test]
async fn rota_por_categoria_com_negacao_e_inheritinputs() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: filmes
      categories: [10]
      inputs: {{so_filmes: '1'}}
    - path: tudo-menos-filmes
      categories: ['!', 10]
      inheritinputs: false
      inputs: {{so_dela: '1'}}
  inputs:
    q: '{{{{ .Keywords }}}}'
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    let server = MockServer::start().await;
    Mock::given(path("/filmes"))
        .and(query_param("q", "x"))
        .and(query_param("so_filmes", "1"))
        .respond_with(html(r#"<li><a href="/f.torrent">Filme</a></li>"#))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/tudo-menos-filmes"))
        .and(query_param("so_dela", "1"))
        .and(query_param_is_missing("q"))
        .respond_with(html(r#"<li><a href="/s.torrent">Serie</a></li>"#))
        .expect(1)
        .mount(&server)
        .await;
    let indexer = client(&yaml, &server, &[]);

    // Sem categoria, as duas rotas valem.
    let all = indexer.search(&SearchQuery::general("x")).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn raw_no_caminho_ordem_declarada_sitelink_e_query_type() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: lista
      inputs:
        $raw: '{{{{ range .Categories }}}}cat[]={{{{.}}}}&{{{{end}}}}'
        depois: '2'
  inputs:
    antes: '1'
    tipo: '{{{{ .Query.Type }}}}'
    q: '{{{{ .Keywords }}}}'
    douban: '{{{{ .Query.DoubanID }}}}'
  rows: {{selector: li}}
  fields:
    _id: {{selector: a, attribute: data-id}}
    title: {{selector: a}}
    details: {{text: '{{{{ .Config.sitelink }}}}t/{{{{ .Result._id }}}}'}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
    category: {{text: 10}}
"
    );
    let server = MockServer::start().await;
    Mock::given(path("/lista"))
        // Na ordem em que foram declarados; o id do Douban, nulo, não entra.
        .and(RawQuery("antes=1&tipo=search&q=x&cat%5B%5D=10&depois=2"))
        .respond_with(html(
            r#"<li><a data-id="9" href="/9.torrent">Nove</a></li>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let results = client(&yaml, &server, &[])
        .search(&SearchQuery::general("x").with_categories([2000]))
        .await
        .unwrap();
    assert_eq!(
        results[0].info_url.as_ref().unwrap().as_str(),
        format!("{}/t/9", server.uri())
    );
}

#[test]
fn select_com_default_fora_das_opcoes_fica_sem_default_e_exige_escolha() {
    let yaml = format!(
        "{HEAD}
settings:
  - name: ordem
    type: select
    label: Ordem
    default: nao-existe
    options:
      a: criado
      b: tamanho
search:
  paths:
    - path: lista
  inputs: {{ordem: '{{{{ .Config.ordem }}}}', q: '{{{{ .Keywords }}}}'}}
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    let build = |overrides: &[(&str, &str)]| {
        CardigannClient::new(
            CardigannDefinition::from_yaml_v11(&yaml).unwrap(),
            0,
            overrides
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            Duration::from_secs(5),
        )
    };
    assert!(matches!(
        build(&[]),
        Err(IndexerError::Definition {
            section: "settings",
            ..
        })
    ));
    assert!(build(&[("ordem", "nao-existe")]).is_err());
    assert!(build(&[("ordem", "b")]).is_ok());
}

#[test]
fn definicao_com_resposta_html_e_json_na_mesma_busca_e_recusada() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: a
    - path: b
      response: {{type: json}}
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    assert!(load_error(&yaml).contains("html e json"));
}

// ---------------------------------------------------------------------------
// Login.

const FORM_LOGIN_YAML: &str = "
type: private
settings:
  - {name: username, type: text, label: Usuário}
  - {name: password, type: password, label: Senha}
login:
  method: form
  path: entrar
  form: form#login
  cookies: [\"JAVA=OK\"]
  inputs:
    usuario: \"{{ .Config.username }}\"
    senha: \"{{ .Config.password }}\"
  selectorinputs:
    csrf:
      selector: meta[name=csrf]
      attribute: content
  getselectorinputs:
    via:
      selector: meta[name=via]
      attribute: content
  captcha:
    type: image
    selector: img.captcha
  error:
    - selector: div.erro
  test:
    path: conta
    selector: a.sair
search:
  paths:
    - path: lista
  inputs: {q: \"{{ .Keywords }}\"}
  rows: {selector: li}
  fields:
    title: {selector: a}
    download: {selector: a, attribute: href}
    size: {text: 1 MB}
";

const LANDING: &str = r#"<html><head><meta name="csrf" content="csrf-lido"><meta name="via" content="v1"></head><body>
<form id="login" action="/sessao" method="post">
  <input type="hidden" name="_token" value="tok123">
  <input type="text" name="usuario">
  <input type="password" name="senha">
  <input type="checkbox" name="lembrar" value="1" checked>
  <input type="checkbox" name="outro" value="1">
  <input type="hidden" name="desligado" value="x" disabled>
  <input type="submit" name="ok" value="Entrar">
</form></body></html>"#;

fn form_yaml() -> String {
    format!("{HEAD}{FORM_LOGIN_YAML}").replace("type: public\n", "")
}

#[tokio::test]
async fn login_por_formulario_junta_campos_da_pagina_e_so_envia_o_que_o_navegador_enviaria() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/entrar"))
        .and(header_regex("cookie", "JAVA=OK"))
        .respond_with(html(LANDING))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sessao"))
        .and(query_param("via", "v1"))
        .and(body_string_contains("_token=tok123"))
        .and(body_string_contains("usuario=ana"))
        .and(body_string_contains("senha=s%C3%A9rio+1"))
        .and(body_string_contains("csrf=csrf-lido"))
        .and(body_string_contains("lembrar=1"))
        .and(body_string_contains("ok=Entrar"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/conta")
                .insert_header("set-cookie", "sessao=abc; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/conta"))
        .respond_with(html(r#"<a class="sair" href="/sair">Sair</a>"#))
        .mount(&server)
        .await;
    Mock::given(path("/lista"))
        .and(header_regex("cookie", "sessao=abc"))
        .respond_with(html(
            r#"<a class="sair" href="/sair">Sair</a><li><a href="/d/1.torrent">Um</a></li>"#,
        ))
        .mount(&server)
        .await;

    let results = client(
        &form_yaml(),
        &server,
        &[("username", "ana"), ("password", "sério 1")],
    )
    .search(&SearchQuery::general("x"))
    .await
    .unwrap();
    assert_eq!(results.len(), 1);

    // Os campos desmarcados e desabilitados não vão no corpo.
    let requests = server.received_requests().await.unwrap();
    let login = requests
        .iter()
        .find(|request| request.url.path() == "/sessao")
        .unwrap();
    let body = String::from_utf8_lossy(&login.body);
    assert!(!body.contains("outro="), "{body}");
    assert!(!body.contains("desligado="), "{body}");
}

#[tokio::test]
async fn login_por_formulario_com_captcha_na_pagina_para_sem_enviar_credencial() {
    let server = MockServer::start().await;
    Mock::given(path("/entrar"))
        .respond_with(html(
            &LANDING.replace("</form>", r#"<img class="captcha" src="/c.png"></form>"#),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = client(
        &form_yaml(),
        &server,
        &[("username", "ana"), ("password", "x")],
    )
    .search(&SearchQuery::general("x"))
    .await
    .unwrap_err();
    assert!(matches!(error, IndexerError::Login { .. }));
    assert!(error.to_string().contains("captcha"), "{error}");
}

#[tokio::test]
async fn login_por_formulario_recusado_pelo_seletor_de_erro_e_form_ausente() {
    let server = MockServer::start().await;
    Mock::given(path("/entrar"))
        .respond_with(html(LANDING))
        .mount(&server)
        .await;
    Mock::given(path("/sessao"))
        .respond_with(html(r#"<div class="erro">Senha incorreta</div>"#))
        .mount(&server)
        .await;
    let settings = [("username", "ana"), ("password", "x")];
    let error = client(&form_yaml(), &server, &settings)
        .search(&SearchQuery::general("x"))
        .await
        .unwrap_err();
    assert!(matches!(error, IndexerError::Login { .. }), "{error}");

    let other = MockServer::start().await;
    Mock::given(path("/entrar"))
        .respond_with(html("<html><body>sem formulário</body></html>"))
        .mount(&other)
        .await;
    let error = client(&form_yaml(), &other, &settings)
        .search(&SearchQuery::general("x"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("formulário"), "{error}");
}

#[tokio::test]
async fn login_get_e_oneurl_aceitam_resposta_json_e_levam_a_sessao() {
    let yaml_get = format!(
        "{HEAD}
type: private
settings:
  - {{name: apikey, type: password, label: Chave}}
login:
  method: get
  path: api/entrar
  inputs:
    chave: '{{{{ .Config.apikey }}}}'
  test:
    path: api/eu
    selector: a.sair
search:
  paths:
    - path: api/lista
      response: {{type: json}}
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  rows: {{selector: itens}}
  fields:
    title: {{selector: nome}}
    download: {{selector: link}}
    size: {{selector: bytes}}
"
    )
    .replace("type: public\n", "");
    let server = MockServer::start().await;
    Mock::given(path("/api/entrar"))
        .and(query_param("chave", "k 1"))
        .respond_with(json(r#"{"ok":true}"#).insert_header("set-cookie", "s=1; Path=/"))
        .expect(1)
        .mount(&server)
        .await;
    // A página de teste responde JSON: o marcador é de HTML e não se aplica.
    Mock::given(path("/api/eu"))
        .respond_with(json(r#"{"eu":true}"#))
        .mount(&server)
        .await;
    Mock::given(path("/api/lista"))
        .and(header_regex("cookie", "s=1"))
        .respond_with(json(
            r#"{"itens":[{"nome":"A","link":"/a.torrent","bytes":5}]}"#,
        ))
        .mount(&server)
        .await;
    let results = client(&yaml_get, &server, &[("apikey", "k 1")])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    assert_eq!(results[0].title, "A");

    let yaml_one = yaml_get
        .replace("method: get", "method: oneurl")
        .replace("path: api/entrar", "path: api/entrar/")
        .replace(
            "chave: '{{ .Config.apikey }}'",
            "oneurl: '{{ .Config.apikey }}'",
        );
    let other = MockServer::start().await;
    Mock::given(path("/api/entrar/tok"))
        .respond_with(json("{}").insert_header("set-cookie", "s=1; Path=/"))
        .expect(1)
        .mount(&other)
        .await;
    Mock::given(path("/api/eu"))
        .respond_with(json("{}"))
        .mount(&other)
        .await;
    Mock::given(path("/api/lista"))
        .respond_with(json(
            r#"{"itens":[{"nome":"B","link":"/b.torrent","bytes":5}]}"#,
        ))
        .mount(&other)
        .await;
    let results = client(&yaml_one, &other, &[("apikey", "tok")])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    assert_eq!(results[0].title, "B");
}

#[test]
fn login_recusa_captcha_que_nao_e_de_imagem_e_metodo_desconhecido() {
    let base = form_yaml();
    assert!(load_error(&base.replace("type: image", "type: recaptcha")).contains("captcha"));
    assert!(load_error(&base.replace("method: form", "method: magia")).contains("método"));
    // Seletor de formulário inválido é erro de carga, não da primeira busca.
    assert!(load_error(&base.replace("form#login", "form[[")).contains("seletor"));
}

// ---------------------------------------------------------------------------
// Bloco `download`.

fn download_yaml(download: &str) -> String {
    format!(
        "{HEAD}
{download}
search:
  paths:
    - path: lista
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    )
}

const TORRENT: &[u8] = b"d8:announce3:urle";

fn bencode() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(TORRENT.to_vec(), "application/x-bittorrent")
}

#[tokio::test]
async fn download_selectors_tenta_na_ordem_e_cai_no_magnet() {
    let yaml = download_yaml(
        "download:
  selectors:
    - selector: a.torrent
      attribute: href
      filters:
        - name: prepend
          args: '/arquivos'
    - selector: a.magnet
      attribute: href",
    );
    let server = MockServer::start().await;
    // Página 1: o primeiro seletor leva a um .torrent de verdade.
    Mock::given(path("/detalhe/1"))
        .respond_with(html(
            r#"<a class="torrent" href="/1.torrent">t</a><a class="magnet" href="magnet:?xt=urn:btih:aaaa">m</a>"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path("/arquivos/1.torrent"))
        .respond_with(bencode())
        .mount(&server)
        .await;
    // Página 2: o link do primeiro seletor devolve HTML; vale o magnet.
    Mock::given(path("/detalhe/2"))
        .respond_with(html(
            r#"<a class="torrent" href="/2.torrent">t</a><a class="magnet" href="magnet:?xt=urn:btih:bbbb&dn=Dois">m</a>"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path("/arquivos/2.torrent"))
        .respond_with(html("<html>sessão expirada</html>"))
        .mount(&server)
        .await;
    // Página 3: o magnet é o único.
    Mock::given(path("/detalhe/3"))
        .respond_with(html(
            r#"<a class="magnet" href="magnet:?xt=urn:btih:cccc">m</a>"#,
        ))
        .mount(&server)
        .await;

    let indexer = client(&yaml, &server, &[]);
    assert!(indexer.proxies_downloads());
    let base = url::Url::parse(&server.uri()).unwrap();

    let first = indexer
        .resolve_download(&base.join("/detalhe/1").unwrap())
        .await
        .unwrap();
    assert_eq!(first, ResolvedDownload::Torrent(TORRENT.to_vec()));

    let second = indexer
        .resolve_download(&base.join("/detalhe/2").unwrap())
        .await
        .unwrap();
    assert!(matches!(&second, ResolvedDownload::Magnet(url) if url.as_str().contains("bbbb")));
    // O `download` do trait não devolve magnet como se fosse arquivo.
    assert!(matches!(
        indexer.download(&base.join("/detalhe/3").unwrap()).await,
        Err(IndexerError::UnsupportedQuery { .. })
    ));
    // Um magnet recebido já está resolvido.
    let magnet = url::Url::parse("magnet:?xt=urn:btih:dddd").unwrap();
    assert_eq!(
        indexer.resolve_download(&magnet).await.unwrap(),
        ResolvedDownload::Magnet(magnet)
    );
    // A origem do link continua valendo.
    assert!(matches!(
        indexer
            .resolve_download(&url::Url::parse("https://outro.invalid/detalhe/1").unwrap())
            .await,
        Err(IndexerError::UnsupportedQuery { .. })
    ));
}

#[tokio::test]
async fn download_nao_segue_link_de_outra_origem_e_testlinktorrent_false_nao_tenta_o_seguinte() {
    let selectors = "download:
  selectors:
    - selector: a.torrent
      attribute: href";
    let server = MockServer::start().await;
    Mock::given(path("/detalhe/1"))
        .respond_with(html(
            r#"<a class="torrent" href="https://outro.invalid/x.torrent">t</a>"#,
        ))
        .mount(&server)
        .await;
    let base = url::Url::parse(&server.uri()).unwrap();
    let error = client(&download_yaml(selectors), &server, &[])
        .resolve_download(&base.join("/detalhe/1").unwrap())
        .await
        .unwrap_err();
    assert!(
        matches!(error, IndexerError::UnexpectedDocument { .. }),
        "{error}"
    );
    // Nem a página de detalhe saiu da origem: só ela foi pedida.
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // O primeiro link devolve HTML e o segundo o arquivo. Conferindo o link
    // (padrão), o segundo seletor salva a busca; com `testlinktorrent: false`
    // o primeiro link é o que vale, e o erro dele é o resultado.
    let two = "download:
  selectors:
    - selector: a.um
      attribute: href
    - selector: a.dois
      attribute: href";
    let other = MockServer::start().await;
    Mock::given(path("/detalhe/1"))
        .respond_with(html(
            r#"<a class="um" href="/um">1</a><a class="dois" href="/dois">2</a>"#,
        ))
        .mount(&other)
        .await;
    Mock::given(path("/um"))
        .respond_with(html("<html>não é arquivo</html>"))
        .mount(&other)
        .await;
    Mock::given(path("/dois"))
        .respond_with(bencode())
        .mount(&other)
        .await;
    let base = url::Url::parse(&other.uri()).unwrap();
    let link = base.join("/detalhe/1").unwrap();
    let torrent = client(&download_yaml(two), &other, &[])
        .resolve_download(&link)
        .await
        .unwrap();
    assert_eq!(torrent, ResolvedDownload::Torrent(TORRENT.to_vec()));
    let unchecked = format!("testlinktorrent: false\n{}", download_yaml(two));
    assert!(matches!(
        client(&unchecked, &other, &[])
            .resolve_download(&link)
            .await,
        Err(IndexerError::UnexpectedDocument { .. })
    ));
}

#[tokio::test]
async fn download_infohash_monta_magnet_e_before_com_post_alimenta_o_seletor() {
    let hash = "0123456789abcdef0123456789abcdef01234567";
    let yaml = download_yaml(
        "download:
  infohash:
    hash:
      selector: span.hash
    title:
      selector: h1",
    );
    let server = MockServer::start().await;
    Mock::given(path("/detalhe/1"))
        .respond_with(html(&format!(
            r#"<h1>Meu Título</h1><span class="hash">{hash}</span>"#
        )))
        .mount(&server)
        .await;
    let base = url::Url::parse(&server.uri()).unwrap();
    let magnet = client(&yaml, &server, &[])
        .resolve_download(&base.join("/detalhe/1").unwrap())
        .await
        .unwrap();
    assert!(
        matches!(&magnet, ResolvedDownload::Magnet(url)
            if url.as_str() == format!("magnet:?xt=urn:btih:{hash}&dn=Meu+T%C3%ADtulo")),
        "{magnet:?}"
    );

    // `before`: um POST antes, com `.DownloadUri.Query.*`; o seletor lê a resposta dele.
    let yaml = download_yaml(
        "download:
  method: post
  before:
    path: pre/{{ .DownloadUri.Query.id }}
    method: post
    inputs:
      id: '{{ .DownloadUri.Query.id }}'
  selectors:
    - selector: a.go
      attribute: href
      usebeforeresponse: true",
    );
    let other = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/pre/7"))
        .and(body_string_contains("id=7"))
        .respond_with(html(r#"<a class="go" href="/baixar/7">go</a>"#))
        .expect(1)
        .mount(&other)
        .await;
    // O arquivo final também sai por POST (`download.method`).
    Mock::given(method("POST"))
        .and(path("/baixar/7"))
        .respond_with(bencode())
        .expect(1)
        .mount(&other)
        .await;
    let base = url::Url::parse(&other.uri()).unwrap();
    let torrent = client(&yaml, &other, &[])
        .resolve_download(&base.join("/detalhe?id=7").unwrap())
        .await
        .unwrap();
    assert_eq!(torrent, ResolvedDownload::Torrent(TORRENT.to_vec()));
}

#[tokio::test]
async fn busca_sem_campo_download_usa_o_infohash_como_magnet() {
    let hash = "0123456789abcdef0123456789abcdef01234567";
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: lista
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    infohash: {{selector: a, attribute: data-hash}}
    size: {{text: 1 MB}}
"
    );
    let server = MockServer::start().await;
    Mock::given(path("/lista"))
        .respond_with(html(&format!(
            r#"<li><a data-hash="{hash}">Filme</a></li>"#
        )))
        .mount(&server)
        .await;
    let results = client(&yaml, &server, &[])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    assert_eq!(
        results[0].download_url.as_str(),
        format!("magnet:?xt=urn:btih:{hash}&dn=Filme")
    );
}

// ---------------------------------------------------------------------------
// Linhas.

#[tokio::test]
async fn rows_after_funde_linhas_e_dateheaders_busca_a_data_no_cabecalho() {
    let after = format!(
        "{HEAD}
search:
  paths:
    - path: lista
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  rows:
    selector: tr.item
    after: 1
  fields:
    title: {{selector: 'td:nth-child(1) a'}}
    download: {{selector: 'td:nth-child(1) a', attribute: href}}
    size: {{selector: 'td:nth-child(3)'}}
    seeders: {{selector: 'td:nth-child(4)'}}
"
    );
    let server = MockServer::start().await;
    Mock::given(path("/lista"))
        .respond_with(html(
            r#"<table>
              <tr class="item"><td><a href="/1.torrent">Um</a></td><td>x</td></tr>
              <tr class="item"><td>2 GB</td><td>7</td></tr>
              <tr class="item"><td><a href="/2.torrent">Dois</a></td><td>x</td></tr>
              <tr class="item"><td>3 GB</td><td>9</td></tr>
            </table>"#,
        ))
        .mount(&server)
        .await;
    let results = client(&after, &server, &[])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].size, 2 * 1024 * 1024 * 1024);
    assert_eq!(results[0].seeders, Some(7));
    assert_eq!(results[1].title, "Dois");
    assert_eq!(results[1].seeders, Some(9));

    let headers = format!(
        "{HEAD}
search:
  paths:
    - path: lista
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  rows:
    selector: tr.item
    dateheaders:
      selector: td.dia
      filters:
        - name: dateparse
          args: yyyy-MM-dd
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    let other = MockServer::start().await;
    Mock::given(path("/lista"))
        .respond_with(html(
            r#"<table>
              <tr><td class="dia">2026-09-24</td></tr>
              <tr class="item"><td><a href="/1.torrent">Um</a></td></tr>
              <tr class="item"><td><a href="/2.torrent">Dois</a></td></tr>
              <tr><td class="dia">2026-09-23</td></tr>
              <tr class="item"><td><a href="/3.torrent">Tres</a></td></tr>
            </table>"#,
        ))
        .mount(&other)
        .await;
    let results = client(&headers, &other, &[])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    let days: Vec<_> = results
        .iter()
        .map(|r| r.published.unwrap().date().to_string())
        .collect();
    assert_eq!(days, ["2026-09-24", "2026-09-24", "2026-09-23"]);
}

#[tokio::test]
async fn categorydesc_default_de_categoria_e_seletor_de_campo_com_template() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: lista
  inputs:
    q: '{{{{ .Keywords }}}}'
    cats: '{{{{ join .Categories \",\" }}}}'
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
    _modo: {{text: b}}
    categorydesc: {{selector: 'span.{{{{ .Result._modo }}}}'}}
"
    )
    .replace(
        "{id: 10, cat: Movies, desc: Filmes}",
        "{id: 10, cat: Movies, desc: Filmes, default: true}",
    );
    let server = MockServer::start().await;
    // Sem categoria na busca, vale a marcada como `default`.
    Mock::given(path("/lista"))
        .and(query_param("cats", "10"))
        .respond_with(html(
            r#"<li><a href="/1.torrent">Um</a><span class="a">Séries</span><span class="b">séries</span></li>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let results = client(&yaml, &server, &[])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap();
    // `categorydesc` acha a categoria pela descrição, sem distinguir caixa.
    assert_eq!(results[0].categories, [5000]);
}

#[tokio::test]
async fn search_error_trata_pagina_de_erro_do_site_como_erro() {
    let yaml = format!(
        "{HEAD}
search:
  paths:
    - path: lista
  inputs: {{q: '{{{{ .Keywords }}}}'}}
  error:
    - selector: ':root:contains(\"Erro interno\")'
  rows: {{selector: li}}
  fields:
    title: {{selector: a}}
    download: {{selector: a, attribute: href}}
    size: {{text: 1 MB}}
"
    );
    let server = MockServer::start().await;
    Mock::given(path("/lista"))
        .respond_with(html("<html><body><h1>Erro interno</h1></body></html>"))
        .mount(&server)
        .await;
    let error = client(&yaml, &server, &[])
        .search(&SearchQuery::general("x"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, IndexerError::UnexpectedDocument { .. }),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Codificação.

#[tokio::test]
async fn windows_1251_na_consulta_na_resposta_e_nos_filtros() {
    let yaml = HEAD.replace("UTF-8", "windows-1251")
        + "search:
  paths:
    - path: lista
  inputs:
    q: '{{ .Keywords }}'
  rows: {selector: li}
  fields:
    title: {selector: a}
    download: {selector: a, attribute: href}
    size: {text: 1 MB}
";
    let server = MockServer::start().await;
    // "Привет" em windows-1251, na query e no corpo.
    let mut body = b"<li><a href=\"/1.torrent\">".to_vec();
    body.extend_from_slice(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
    body.extend_from_slice(b"</a></li>");
    Mock::given(path("/lista"))
        .and(RawQuery("q=%CF%F0%E8%E2%E5%F2"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html"))
        .expect(1)
        .mount(&server)
        .await;
    let results = client(&yaml, &server, &[])
        .search(&SearchQuery::general("Привет"))
        .await
        .unwrap();
    assert_eq!(results[0].title, "Привет");
}

#[test]
fn codificacao_de_varios_bytes_continua_recusada() {
    let yaml = format!("{HEAD}{JSON_YAML}").replace("UTF-8", "gbk");
    assert!(load_error(&yaml).contains("codificação"));
}

// ---------------------------------------------------------------------------
// Definições de produção.

#[test]
fn as_definicoes_de_producao_continuam_carregando() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../definicoes");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "AVISO: {} não existe; teste das definições de produção pulado",
            dir.display()
        );
        return;
    };
    let mut loaded = Vec::new();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "yml") {
            continue;
        }
        let yaml = std::fs::read_to_string(&path).unwrap();
        let definition = CardigannDefinition::from_yaml_v11(&yaml)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        loaded.push(definition.id().to_owned());
    }
    loaded.sort();
    assert_eq!(loaded, ["amigosshare", "bjsharecustom"]);
}

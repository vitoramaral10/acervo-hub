//! Por onde as requisições de um indexador saem: o proxy, se ele usa um, e o
//! `FlareSolverr` diante do desafio do Cloudflare.
//!
//! O desafio é reconhecido pela resposta (403 ou 503 com as marcas dele) e a
//! requisição é refeita pelo `FlareSolverr`, como a referência faz: ele abre a
//! página num navegador de verdade e devolve os cookies e o user-agent que
//! passaram. Esses dois valem para o indexador até um novo desafio.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, SERVER};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::IndexerError;

/// Proxy de saída: `http://`, `https://` ou `socks5://`. A credencial, se
/// houver, vai na própria URL, que nunca chega a Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct Proxy {
    url: Url,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Proxy")
            .field("scheme", &self.url.scheme())
            .field("credential", &(!self.url.username().is_empty()))
            .finish_non_exhaustive()
    }
}

impl Proxy {
    /// # Errors
    ///
    /// URL ilegível, esquema fora de `http`, `https`, `socks5` e `socks5h`,
    /// ou credencial que não cabe na URL.
    pub fn new(
        url: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, IndexerError> {
        let mut url = Url::parse(url.trim()).map_err(|_| invalid("proxy: URL inválida"))?;
        if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") {
            return Err(invalid("proxy: use http://, https:// ou socks5://"));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(invalid("proxy: a URL não tem host"));
        }
        if let Some(username) = username.map(str::trim).filter(|value| !value.is_empty()) {
            url.set_username(username)
                .map_err(|()| invalid("proxy: usuário inválido"))?;
            url.set_password(password.filter(|value| !value.is_empty()))
                .map_err(|()| invalid("proxy: senha inválida"))?;
        }
        Ok(Self { url })
    }

    pub(crate) fn reqwest(&self) -> Result<reqwest::Proxy, IndexerError> {
        reqwest::Proxy::all(self.url.clone()).map_err(IndexerError::Build)
    }

    /// O proxy como o `FlareSolverr` o pede: endereço sem credencial, e a
    /// credencial em campos à parte.
    fn flaresolverr(&self) -> serde_json::Value {
        let mut bare = self.url.clone();
        let _ = bare.set_username("");
        let _ = bare.set_password(None);
        let decode = |text: &str| {
            url::form_urlencoded::parse(format!("v={}", text.replace('+', "%2B")).as_bytes())
                .next()
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        };
        let username = (!self.url.username().is_empty()).then(|| decode(self.url.username()));
        let password = self.url.password().map(decode);
        json!({
            "url": bare.as_str().trim_end_matches('/'),
            "username": username,
            "password": password,
        })
    }
}

/// O `FlareSolverr`: `{url}/v1` e quanto ele pode levar num desafio.
#[derive(Clone, PartialEq, Eq)]
pub struct FlareSolverr {
    endpoint: Url,
    timeout: Duration,
}

impl std::fmt::Debug for FlareSolverr {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FlareSolverr")
            .field("endpoint", &self.endpoint.as_str())
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl FlareSolverr {
    /// # Errors
    ///
    /// URL que não é HTTP(S) ou timeout zero.
    pub fn new(url: &str, timeout: Duration) -> Result<Self, IndexerError> {
        let base = Url::parse(url.trim()).map_err(|_| invalid("FlareSolverr: URL inválida"))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(invalid("FlareSolverr: use http:// ou https://"));
        }
        if timeout.is_zero() {
            return Err(invalid(
                "FlareSolverr: o timeout precisa ser maior que zero",
            ));
        }
        let endpoint = Url::parse(&format!("{}/v1", base.as_str().trim_end_matches('/')))
            .map_err(|_| invalid("FlareSolverr: URL inválida"))?;
        Ok(Self { endpoint, timeout })
    }
}

/// O que fazer quando o site pede o desafio do Cloudflare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Challenge {
    /// Nunca chama o `FlareSolverr`: o desafio vira erro.
    Never,
    /// Chama o `FlareSolverr` quando a resposta for o desafio.
    #[default]
    Auto,
    /// Além do automático, resolve o desafio antes da primeira requisição:
    /// para o site que sempre o pede, poupa a ida que voltaria barrada.
    Always,
}

/// Rede de um indexador.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Network {
    pub proxy: Option<Proxy>,
    pub flaresolverr: Option<FlareSolverr>,
    pub challenge: Challenge,
}

/// Lê o corpo do 403 ou 503 até este tamanho para procurar o desafio.
pub(crate) const CHALLENGE_PAGE: usize = 2 * 1024 * 1024;

/// A resposta é o desafio do Cloudflare (ou do DDoS-Guard), que só um
/// navegador passa? Só 403 e 503 contam; neles, o cabeçalho `cf-mitigated`,
/// o título "Just a moment..." ou o script `__cf_chl` denunciam.
#[must_use]
pub(crate) fn is_challenge(status: StatusCode, headers: &HeaderMap, body: &str) -> bool {
    if status != StatusCode::FORBIDDEN && status != StatusCode::SERVICE_UNAVAILABLE {
        return false;
    }
    if headers.contains_key("cf-mitigated") {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    if lower.contains("<title>just a moment...</title>") || lower.contains("__cf_chl") {
        return true;
    }
    // Os títulos genéricos só valem vindo do próprio Cloudflare.
    let behind_guard = headers
        .get(SERVER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|server| {
            matches!(
                server.to_ascii_lowercase().as_str(),
                "cloudflare" | "cloudflare-nginx" | "ddos-guard"
            )
        });
    behind_guard
        && (lower.contains("<title>attention required! | cloudflare</title>")
            || lower.contains("<title>ddos-guard</title>"))
}

/// O que o `FlareSolverr` devolveu de útil: os cookies que passaram no
/// desafio e o user-agent do navegador que os ganhou.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Clearance {
    pub cookies: Vec<(String, String)>,
    pub user_agent: Option<String>,
}

impl std::fmt::Debug for Clearance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Clearance")
            .field("cookies", &self.cookies.len())
            .field("user_agent", &self.user_agent)
            .finish()
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    status: String,
    solution: Option<Solution>,
}

#[derive(Deserialize)]
struct Solution {
    #[serde(default)]
    cookies: Vec<Cookie>,
    #[serde(default, rename = "userAgent")]
    user_agent: Option<String>,
}

#[derive(Deserialize)]
struct Cookie {
    name: String,
    value: String,
}

/// Uma ida ao `FlareSolverr` para a requisição barrada: `request.get`, ou
/// `request.post` com o corpo de formulário. Outro tipo de envio ele não
/// refaz.
pub(crate) async fn solve(
    solver: &FlareSolverr,
    http: &reqwest::Client,
    indexer: &str,
    request: &reqwest::Request,
    proxy: Option<&Proxy>,
) -> Result<Clearance, IndexerError> {
    let failed = |reason: &'static str| IndexerError::Challenge {
        indexer: indexer.to_owned(),
        reason,
    };
    let max_timeout = u64::try_from(solver.timeout.as_millis()).unwrap_or(u64::MAX);
    let mut command = match *request.method() {
        reqwest::Method::GET => json!({
            "cmd": "request.get",
            "url": request.url().as_str(),
            "maxTimeout": max_timeout,
        }),
        reqwest::Method::POST => {
            let form = request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("application/x-www-form-urlencoded"));
            let body = request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
            match (form, body) {
                (true, Some(body)) => json!({
                    "cmd": "request.post",
                    "url": request.url().as_str(),
                    "postData": body,
                    "maxTimeout": max_timeout,
                }),
                _ => {
                    return Err(failed(
                        "o desafio do Cloudflare caiu num envio que o FlareSolverr não refaz",
                    ));
                }
            }
        }
        _ => {
            return Err(failed(
                "o desafio do Cloudflare caiu num envio que o FlareSolverr não refaz",
            ));
        }
    };
    if let Some(proxy) = proxy {
        command["proxy"] = proxy.flaresolverr();
    }
    let response = http
        .post(solver.endpoint.clone())
        .timeout(solver.timeout + Duration::from_secs(5))
        .json(&command)
        .send()
        .await
        .map_err(|_| failed("o FlareSolverr não respondeu"))?;
    // 500 é a resposta dele para "não resolvi": o corpo diz o resto.
    let status = response.status();
    if status != StatusCode::OK && status != StatusCode::INTERNAL_SERVER_ERROR {
        return Err(failed("o FlareSolverr respondeu com erro HTTP"));
    }
    let answer: Answer = response
        .json()
        .await
        .map_err(|_| failed("o FlareSolverr devolveu uma resposta ilegível"))?;
    let solution = answer
        .solution
        .filter(|_| answer.status.eq_ignore_ascii_case("ok"))
        .ok_or_else(|| failed("o FlareSolverr não resolveu o desafio do Cloudflare"))?;
    if solution.cookies.is_empty() {
        return Err(failed("o FlareSolverr não devolveu cookies"));
    }
    Ok(Clearance {
        cookies: solution
            .cookies
            .into_iter()
            .filter(|cookie| !cookie.name.is_empty())
            .map(|cookie| (cookie.name, cookie.value))
            .collect(),
        user_agent: solution.user_agent.filter(|agent| !agent.trim().is_empty()),
    })
}

fn invalid(reason: &'static str) -> IndexerError {
    IndexerError::Definition {
        section: "rede",
        reason,
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn desafio_pelas_marcas_so_em_403_e_503() {
        let page = "<html><head><title>Just a moment...</title></head></html>";
        assert!(is_challenge(StatusCode::FORBIDDEN, &HeaderMap::new(), page));
        assert!(is_challenge(
            StatusCode::SERVICE_UNAVAILABLE,
            &HeaderMap::new(),
            page
        ));
        assert!(!is_challenge(StatusCode::OK, &HeaderMap::new(), page));
        assert!(!is_challenge(
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            page
        ));
        assert!(is_challenge(
            StatusCode::FORBIDDEN,
            &headers(&[("cf-mitigated", "challenge")]),
            ""
        ));
        assert!(is_challenge(
            StatusCode::FORBIDDEN,
            &HeaderMap::new(),
            "<script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=1\"></script>\
             <script>window.__CF_chl_opt={}</script>"
        ));
    }

    #[test]
    fn forbidden_comum_nao_e_desafio() {
        let page = "<html><head><title>Acesso negado</title></head><body>faça login</body></html>";
        assert!(!is_challenge(
            StatusCode::FORBIDDEN,
            &HeaderMap::new(),
            page
        ));
        // O título genérico só vale com o servidor do Cloudflare.
        let generic = "<title>Attention Required! | Cloudflare</title>";
        assert!(!is_challenge(
            StatusCode::FORBIDDEN,
            &HeaderMap::new(),
            generic
        ));
        assert!(is_challenge(
            StatusCode::FORBIDDEN,
            &headers(&[("server", "cloudflare")]),
            generic
        ));
    }

    #[test]
    fn proxy_aceita_os_esquemas_e_guarda_a_credencial_fora_do_debug() {
        let proxy = Proxy::new(
            "socks5://proxy.invalid:1080",
            Some("usuario"),
            Some("s3nh@"),
        )
        .unwrap();
        let debug = format!("{proxy:?}");
        assert!(!debug.contains("s3nh") && !debug.contains("usuario"));
        let for_solver = proxy.flaresolverr();
        assert_eq!(for_solver["url"], "socks5://proxy.invalid:1080");
        assert_eq!(for_solver["username"], "usuario");
        assert_eq!(for_solver["password"], "s3nh@");
        assert!(Proxy::new("http://proxy.invalid:3128", None, None).is_ok());
        assert!(Proxy::new("ftp://proxy.invalid", None, None).is_err());
        assert!(Proxy::new("nada", None, None).is_err());
    }

    #[test]
    fn flaresolverr_monta_o_endpoint_v1() {
        let solver =
            FlareSolverr::new("http://flaresolverr:8191/", Duration::from_secs(60)).unwrap();
        assert_eq!(solver.endpoint.as_str(), "http://flaresolverr:8191/v1");
        assert!(FlareSolverr::new("http://x", Duration::ZERO).is_err());
        assert!(FlareSolverr::new("ftp://x", Duration::from_secs(1)).is_err());
    }
}

//! Envio das capturas ao `ImgBB` (`POST /1/upload`, multipart `key` +
//! `image`), que devolve o link direto e a miniatura para o `[img]` da
//! descrição.
//!
//! A chave é segredo: vai só no corpo do multipart, nunca na URL, e nenhum
//! erro daqui a carrega. O erro do reqwest traz a URL e às vezes o corpo; ele
//! é trocado por mensagem própria, e a mensagem que o serviço devolve passa
//! por [`scrub`] antes de sair.

use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};

/// Onde o `ImgBB` recebe imagens.
pub(crate) const UPLOAD_URL: &str = "https://api.imgbb.com/1/upload";

/// Um PNG 4K tem dezenas de MiB: a subida leva mais que uma consulta comum.
const TIMEOUT: Duration = Duration::from_secs(120);

/// O maior arquivo que a API do `ImgBB` aceita (32 MB).
const MAX_BYTES: usize = 32 << 20;

/// Uma captura hospedada: o link direto (para o `[img]`) e a miniatura.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Screen {
    pub(crate) url: String,
    pub(crate) thumb: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ImgbbError {
    #[error("o ImgBB não respondeu a tempo")]
    Timeout,
    #[error("o ImgBB está inalcançável")]
    Unreachable,
    #[error("o ImgBB recusou a chave: confira-a em Configurações → Imagens")]
    InvalidKey,
    #[error("o ImgBB respondeu {status}: {message}")]
    Status { status: u16, message: String },
    #[error("o ImgBB respondeu num formato inesperado")]
    Format,
    #[error("a captura tem {mib} MiB e passa do limite de 32 MB do ImgBB")]
    TooLarge { mib: usize },
}

/// O cliente do `ImgBB`; `base` muda só nos testes.
#[derive(Debug, Clone)]
pub(crate) struct Imgbb {
    http: reqwest::Client,
    base: String,
}

#[derive(Deserialize)]
struct Uploaded {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    url: String,
    #[serde(default)]
    thumb: Option<Link>,
}

#[derive(Deserialize)]
struct Link {
    url: String,
}

#[derive(Deserialize)]
struct Failure {
    #[serde(default)]
    error: Option<FailureDetail>,
}

#[derive(Deserialize)]
struct FailureDetail {
    #[serde(default)]
    message: String,
}

/// Tira a chave de um texto que vai a log ou à tela.
pub(crate) fn scrub(text: &str, key: &str) -> String {
    if key.is_empty() {
        text.to_owned()
    } else {
        text.replace(key, "<chave>")
    }
}

impl Imgbb {
    /// # Errors
    ///
    /// O TLS do sistema não sobe (não acontece com rustls).
    pub(crate) fn new() -> anyhow::Result<Self> {
        Self::with_base(UPLOAD_URL)
    }

    pub(crate) fn with_base(base: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|_| anyhow::anyhow!("não deu para montar o cliente HTTP do ImgBB"))?;
        Ok(Self {
            http,
            base: base.to_owned(),
        })
    }

    /// Sobe uma imagem (`mime` é o `image/png` ou `image/jpeg` dela) e devolve
    /// os links.
    ///
    /// # Errors
    ///
    /// Serviço lento ou fora do ar, chave recusada, resposta fora do formato.
    /// Nenhum erro carrega a chave.
    pub(crate) async fn upload(
        &self,
        key: &str,
        image: Vec<u8>,
        name: &str,
        mime: &str,
    ) -> Result<Screen, ImgbbError> {
        // Recusa antes de subir: o serviço só diria o mesmo depois de receber
        // dezenas de MiB.
        if image.len() > MAX_BYTES {
            return Err(ImgbbError::TooLarge {
                mib: image.len() >> 20,
            });
        }
        let image = Part::bytes(image)
            .file_name(name.to_owned())
            .mime_str(mime)
            .map_err(|_| ImgbbError::Format)?;
        let form = Form::new().text("key", key.to_owned()).part("image", image);
        let response = self
            .http
            .post(&self.base)
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ImgbbError::Timeout
                } else {
                    ImgbbError::Unreachable
                }
            })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            if error.is_timeout() {
                ImgbbError::Timeout
            } else {
                ImgbbError::Unreachable
            }
        })?;
        if !status.is_success() {
            let message = serde_json::from_slice::<Failure>(&body)
                .ok()
                .and_then(|f| f.error)
                .map(|e| e.message)
                .unwrap_or_default();
            // A API responde 400 com "Invalid API v1 key." para chave errada.
            if status == reqwest::StatusCode::FORBIDDEN
                || message.to_ascii_lowercase().contains("key")
            {
                return Err(ImgbbError::InvalidKey);
            }
            let message: String = scrub(&message, key).chars().take(200).collect();
            return Err(ImgbbError::Status {
                status: status.as_u16(),
                message: if message.is_empty() {
                    "sem mensagem".into()
                } else {
                    message
                },
            });
        }
        let uploaded: Uploaded = serde_json::from_slice(&body).map_err(|_| ImgbbError::Format)?;
        let url = uploaded.data.url;
        if !url.starts_with("https://") {
            return Err(ImgbbError::Format);
        }
        let thumb = uploaded
            .data
            .thumb
            .map(|t| t.url)
            .filter(|t| t.starts_with("https://"))
            .unwrap_or_else(|| url.clone());
        Ok(Screen { url, thumb })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn captura_acima_do_limite_nem_sobe() {
        // Sem mock montado: qualquer requisição seria um erro de teste.
        let imgbb = Imgbb::with_base("http://127.0.0.1:9/1/upload").unwrap();
        let error = imgbb
            .upload(KEY, vec![0; MAX_BYTES + 1], "captura-1.png", "image/png")
            .await
            .unwrap_err();
        assert_eq!(error, ImgbbError::TooLarge { mib: 32 });
        assert!(error.to_string().contains("32 MB"));
    }

    #[tokio::test]
    async fn sobe_o_png_com_a_chave_no_corpo_e_devolve_link_e_miniatura() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains(KEY))
            .and(body_string_contains(
                "name=\"image\"; filename=\"captura-1.png\"",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "url": "https://i.ibb.co/abc/captura-1.png",
                    "thumb": { "url": "https://i.ibb.co/abc/captura-1-t.png" }
                },
                "success": true, "status": 200
            })))
            .expect(1)
            .mount(&server)
            .await;
        let imgbb = Imgbb::with_base(&format!("{}/1/upload", server.uri())).unwrap();
        let screen = imgbb
            // Bytes em ASCII: o matcher de corpo só lê texto.
            .upload(KEY, b"png".to_vec(), "captura-1.png", "image/png")
            .await
            .unwrap();
        assert_eq!(screen.url, "https://i.ibb.co/abc/captura-1.png");
        assert_eq!(screen.thumb, "https://i.ibb.co/abc/captura-1-t.png");
        // A chave não vai na URL.
        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].url.as_str().contains(KEY));
    }

    #[tokio::test]
    async fn erro_do_servico_nunca_mostra_a_chave() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "status_code": 400,
                "error": { "message": "Invalid API v1 key.", "code": 100 }
            })))
            .mount(&server)
            .await;
        let imgbb = Imgbb::with_base(&server.uri()).unwrap();
        let error = imgbb
            .upload(KEY, vec![1], "c.png", "image/png")
            .await
            .unwrap_err();
        assert_eq!(error, ImgbbError::InvalidKey);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "error": { "message": format!("falhou com {KEY}") }
            })))
            .mount(&server)
            .await;
        let imgbb = Imgbb::with_base(&server.uri()).unwrap();
        let error = imgbb
            .upload(KEY, vec![1], "c.png", "image/png")
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("500"), "{text}");
        assert!(!text.contains(KEY), "{text}");
    }

    #[tokio::test]
    async fn servico_fora_do_ar_vira_mensagem_sem_url() {
        // Porta fechada: o erro do reqwest traria a URL.
        let imgbb = Imgbb::with_base(&format!("http://127.0.0.1:9/1/upload?key={KEY}")).unwrap();
        let error = imgbb
            .upload(KEY, vec![1], "c.png", "image/png")
            .await
            .unwrap_err();
        assert_eq!(error, ImgbbError::Unreachable);
        assert!(!error.to_string().contains(KEY));
    }

    #[tokio::test]
    async fn resposta_sem_link_https_e_formato_inesperado() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "data": { "url": "javascript:x" } })),
            )
            .mount(&server)
            .await;
        let imgbb = Imgbb::with_base(&server.uri()).unwrap();
        let error = imgbb
            .upload(KEY, vec![1], "c.png", "image/png")
            .await
            .unwrap_err();
        assert_eq!(error, ImgbbError::Format);
    }

    #[test]
    fn scrub_troca_a_chave() {
        assert_eq!(scrub(&format!("x {KEY} y"), KEY), "x <chave> y");
        assert_eq!(scrub("x", ""), "x");
    }
}

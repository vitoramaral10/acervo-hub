//! Requisição ao site do tracker com a sessão do indexador, fora da busca e do
//! download: o envio de torrent pelo formulário do site e as chamadas que ele
//! faz antes (busca no IMDB, leitura do `MediaInfo`, upload de capturas).
//!
//! Só a origem do indexador é alcançável, como no download: a sessão nunca
//! sai para outro site.

use std::fmt::Write;

/// O que a requisição manda no corpo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteBody {
    Empty,
    /// JSON já serializado.
    Json(String),
    Multipart(Vec<SitePart>),
}

/// Uma parte de `multipart/form-data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePart {
    pub name: String,
    pub value: SitePartValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SitePartValue {
    Text(String),
    File {
        file_name: String,
        content_type: String,
        bytes: Vec<u8>,
    },
}

impl SitePart {
    #[must_use]
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: SitePartValue::Text(value.into()),
        }
    }

    #[must_use]
    pub fn file(
        name: impl Into<String>,
        file_name: impl Into<String>,
        content_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            name: name.into(),
            value: SitePartValue::File {
                file_name: file_name.into(),
                content_type: content_type.into(),
                bytes,
            },
        }
    }
}

/// Método e caminho (relativo à origem do indexador) de uma requisição.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteRequest {
    pub post: bool,
    pub path: String,
    pub body: SiteBody,
}

impl SiteRequest {
    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            post: false,
            path: path.into(),
            body: SiteBody::Empty,
        }
    }

    #[must_use]
    pub fn json(path: impl Into<String>, json: impl Into<String>) -> Self {
        Self {
            post: true,
            path: path.into(),
            body: SiteBody::Json(json.into()),
        }
    }

    #[must_use]
    pub fn multipart(path: impl Into<String>, parts: Vec<SitePart>) -> Self {
        Self {
            post: true,
            path: path.into(),
            body: SiteBody::Multipart(parts),
        }
    }
}

/// O que o site respondeu, sem seguir redirect: quem chama lê o `Location`
/// (o envio responde com o redirect para a página do torrent criado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteResponse {
    pub status: u16,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

impl SiteResponse {
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Fronteira do multipart: não aparece em nenhum conteúdo que o acervo envia
/// (texto é escapado; arquivo binário teria de conter esta sequência exata).
pub(crate) const BOUNDARY: &str = "----acervo-hub-site-boundary-7f3a9c";

/// Corpo `multipart/form-data` em bytes, para a requisição poder ser refeita
/// depois do desafio do Cloudflare (stream não se clona).
pub(crate) fn encode_multipart(parts: &[SitePart]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        let name = quote(&part.name);
        let mut head = String::new();
        let _ = write!(head, "--{BOUNDARY}\r\n");
        match &part.value {
            SitePartValue::Text(value) => {
                let _ = write!(
                    head,
                    "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
                );
                body.extend_from_slice(head.as_bytes());
                body.extend_from_slice(value.as_bytes());
            }
            SitePartValue::File {
                file_name,
                content_type,
                bytes,
            } => {
                let _ = write!(
                    head,
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
                    quote(file_name)
                );
                body.extend_from_slice(head.as_bytes());
                body.extend_from_slice(bytes);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

/// Aspas e quebras de linha não podem fechar o cabeçalho da parte.
fn quote(value: &str) -> String {
    value
        .replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_com_texto_e_arquivo() {
        let body = encode_multipart(&[
            SitePart::text("name", "Filme (2024)"),
            SitePart::file(
                "torrent",
                "a\"b.torrent",
                "application/x-bittorrent",
                vec![0, 159, 255],
            ),
        ]);
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nFilme (2024)\r\n"
        )));
        assert!(text.contains("name=\"torrent\"; filename=\"a%22b.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\n"));
        assert!(body.windows(3).any(|w| w == [0, 159, 255]));
        assert!(text.ends_with(&format!("--{BOUNDARY}--\r\n")));
    }
}

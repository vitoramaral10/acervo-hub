//! Hash do torrent: a chave que cruza os grabs com o cliente de download.

use std::fmt;

/// Hash do torrent no cliente de download — a chave que cruza fila e cliente.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DownloadHash(String);

impl DownloadHash {
    /// Normaliza para minúsculas: o catálogo e o cliente divergem no caixa.
    #[must_use]
    pub fn new(raw: impl AsRef<str>) -> Self {
        Self(raw.as_ref().trim().to_ascii_lowercase())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DownloadHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_normaliza_caixa_e_espaco() {
        assert_eq!(DownloadHash::new("  ABC123 "), DownloadHash::new("abc123"));
    }
}

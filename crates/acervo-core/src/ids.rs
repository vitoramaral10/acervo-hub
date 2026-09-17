//! Identificadores. Newtypes para que o compilador recuse trocar um pelo outro.

use std::fmt;

macro_rules! numeric_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

numeric_id!(
    /// Uma obra: série ou filme.
    WorkId
);
numeric_id!(
    /// Uma unidade baixável: episódio, ou o filme inteiro.
    ItemId
);
numeric_id!(
    /// Item de fila, no espaço de ids da instância que o reportou.
    QueueItemId
);

/// Hash do torrent no cliente de download — a chave que cruza fila e cliente.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DownloadHash(String);

impl DownloadHash {
    /// Normaliza para minúsculas: as instâncias `*arr` e o cliente divergem no caixa.
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

/// Uma instância `*arr` da qual se leu um inventário.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceName(String);

impl InstanceName {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstanceName {
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

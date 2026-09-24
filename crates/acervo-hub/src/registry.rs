//! Indexadores adicionados e desativados pela interface web.
//!
//! O `config.toml` segue somente leitura: o que a tela muda vive aqui e vale
//! por cima dele. Settings e credenciais desses indexadores ficam no arquivo
//! de credenciais, por id — um lugar só para segredo.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::TorznabIndexer;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Ids ou nomes desativados, de qualquer origem.
    #[serde(default)]
    pub disabled: BTreeSet<String>,
    #[serde(default)]
    pub added: Vec<Added>,
    /// Indexadores do `config.toml` removidos pela tela. O arquivo segue
    /// intacto; o nome aqui o tira do ar e da lista até ser adicionado de novo.
    #[serde(default)]
    pub removed: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Added {
    /// Definição do catálogo, pelo id.
    Cardigann {
        definition: String,
        /// Arquivo da definição, guardado na hora de adicionar: evita varrer o
        /// catálogo inteiro a cada subida.
        path: std::path::PathBuf,
    },
    /// Endpoint Torznab qualquer. A chave dele fica nas credenciais.
    Torznab(TorznabIndexer),
}

impl Added {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Cardigann { definition, .. } => definition,
            Self::Torznab(spec) => &spec.name,
        }
    }
}

/// Lê o registro; ausente é vazio.
///
/// # Errors
///
/// Arquivo existente e ilegível ou fora do formato.
pub fn load(path: &Path) -> Result<Registry> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("interpretando o registro em `{}`", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(error) => {
            Err(error).with_context(|| format!("lendo o registro em `{}`", path.display()))
        }
    }
}

/// # Errors
///
/// Falha de escrita.
pub fn save(path: &Path, registry: &Registry) -> Result<()> {
    let text = toml::to_string(registry).context("serializando o registro")?;
    crate::credentials::write_atomic(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ida_e_volta_com_os_dois_tipos() {
        let dir = std::env::temp_dir().join(format!("acervo-registro-{}", std::process::id()));
        let path = dir.join("indexadores.toml");
        assert_eq!(load(&path).unwrap(), Registry::default());

        let registry = Registry {
            disabled: ["velho".to_owned()].into(),
            added: vec![
                Added::Cardigann {
                    definition: "publico".into(),
                    path: "/defs/publico.yml".into(),
                },
                Added::Torznab(TorznabIndexer {
                    name: "outro".into(),
                    url: "http://x/api".into(),
                    api_key: None,
                    request_interval_seconds: 2.0,
                }),
            ],
            removed: ["do-arquivo".to_owned()].into(),
        };
        save(&path, &registry).unwrap();
        assert_eq!(load(&path).unwrap(), registry);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

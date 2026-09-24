//! Catálogo de definições Cardigann que a interface oferece para adicionar.
//!
//! Lido uma vez na subida: são centenas de YAML, e o resultado — quais rodam e
//! por que os outros não — não muda sem trocar os arquivos.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use acervo_indexers::{CardigannDefinition, DefinitionHeader};

#[derive(Debug, Clone)]
pub struct Known {
    pub path: PathBuf,
    pub header: DefinitionHeader,
    /// `None` se roda; senão, o motivo da recusa.
    pub refusal: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Definitions {
    by_id: BTreeMap<String, Known>,
}

impl Definitions {
    /// Varre os diretórios na ordem dada; o primeiro que tiver um id vence.
    /// Diretório ilegível é pulado com aviso: catálogo incompleto não impede
    /// servir os indexadores já configurados.
    #[must_use]
    pub fn scan(dirs: &[PathBuf]) -> Self {
        let mut by_id = BTreeMap::new();
        for dir in dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "catálogo de definições ilegível");
                    continue;
                }
            };
            let mut paths: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|ext| ext == "yml" || ext == "yaml")
                })
                .collect();
            paths.sort();
            for path in paths {
                if let Some(known) = Self::read(&path) {
                    by_id.entry(known.header.id.clone()).or_insert(known);
                }
            }
        }
        tracing::info!(definicoes = by_id.len(), "catálogo de definições lido");
        Self { by_id }
    }

    fn read(path: &Path) -> Option<Known> {
        let yaml = std::fs::read_to_string(path).ok()?;
        let header = DefinitionHeader::peek(&yaml)?;
        let refusal = CardigannDefinition::from_yaml_v11(&yaml)
            .err()
            .map(|error| error.to_string());
        Some(Known {
            path: path.to_owned(),
            header,
            refusal,
        })
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Known> {
        self.by_id.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Known> {
        self.by_id.values()
    }

    /// Registra uma definição que veio da config, para que ela também
    /// apareça no catálogo mesmo fora dos diretórios.
    pub fn include(&mut self, path: &Path) {
        if let Some(known) = Self::read(path) {
            self.by_id.entry(known.header.id.clone()).or_insert(known);
        }
    }
}

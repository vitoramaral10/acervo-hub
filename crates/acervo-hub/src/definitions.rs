//! Definições Cardigann locais e em uso no banco. O catálogo remoto é baixado
//! sob demanda, guardado em memória e usado apenas ao cadastrar um indexador.
//! Diretórios locais e arquivos fixados têm precedência.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use acervo_indexers::{CardigannDefinition, DefinitionHeader, SettingInfo};
use acervo_store::DefinitionRow;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// De onde uma definição veio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Arquivo de um diretório local, ou o arquivo fixado num cadastro.
    Local(PathBuf),
    /// Baixada do repositório para o banco.
    Database(Arc<str>),
}

#[derive(Debug, Clone)]
pub struct Known {
    pub source: Source,
    pub header: DefinitionHeader,
    /// `None` se roda; senão, o motivo da recusa.
    pub refusal: Option<String>,
    pub settings: Vec<SettingInfo>,
}

impl Known {
    /// O YAML da definição: do arquivo, lido agora, ou do banco.
    ///
    /// # Errors
    ///
    /// Arquivo que sumiu ou ficou ilegível desde a varredura.
    pub fn yaml(&self) -> Result<String> {
        match &self.source {
            Source::Local(path) => read(path),
            Source::Database(yaml) => Ok(yaml.to_string()),
        }
    }

    /// O arquivo, quando a definição vem de um.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            Source::Local(path) => Some(path),
            Source::Database(_) => None,
        }
    }

    #[must_use]
    pub fn is_local(&self) -> bool {
        matches!(self.source, Source::Local(_))
    }
}

/// A definição que um cadastro usa, já lida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub yaml: String,
    pub sha: String,
    /// Vem de diretório local (ou de arquivo fixado num cadastro): o
    /// repositório nunca a troca.
    pub local: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Definitions {
    by_id: BTreeMap<String, Known>,
}

/// SHA-256 em hexadecimal.
#[must_use]
pub fn sha(yaml: &str) -> String {
    Sha256::digest(yaml.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("lendo a definição `{}`", path.display()))
}

/// Os YAML de um diretório, em ordem de nome. Ilegível é pulado com aviso:
/// catálogo incompleto não impede consultar os indexadores já configurados.
fn files(dir: &Path) -> Vec<(PathBuf, String)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "catálogo de definições ilegível");
            return Vec::new();
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
    paths
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(&path).ok().map(|yaml| (path, yaml)))
        .collect()
}

impl Definitions {
    /// Junta as duas fontes na ordem de precedência; o primeiro que tiver um
    /// id vence.
    #[must_use]
    pub fn assemble(local: &[PathBuf], database: &[DefinitionRow]) -> Self {
        let mut definitions = Self {
            by_id: BTreeMap::new(),
        };
        for dir in local {
            for (path, yaml) in files(dir) {
                definitions.offer(Source::Local(path), &yaml);
            }
        }
        for row in database {
            definitions.offer(Source::Database(Arc::from(row.yaml.as_str())), &row.yaml);
        }
        tracing::info!(
            definicoes = definitions.by_id.len(),
            "catálogo de definições montado"
        );
        definitions
    }

    /// Oferece uma definição: entra se ninguém mais forte já tem o id.
    /// Devolve se entrou.
    pub fn offer(&mut self, source: Source, yaml: &str) -> bool {
        let Some(header) = DefinitionHeader::peek(yaml) else {
            return false;
        };
        if self.by_id.contains_key(&header.id) {
            return false;
        }
        let (refusal, settings) = match CardigannDefinition::from_yaml_v11(yaml) {
            Ok(parsed) => (None, parsed.settings().clone()),
            Err(error) => (Some(error.to_string()), Vec::new()),
        };
        self.by_id.insert(
            header.id.clone(),
            Known {
                source,
                header,
                refusal,
                settings,
            },
        );
        true
    }

    /// Completa os ids ausentes, sem substituir as fontes prioritárias.
    pub fn extend(&mut self, other: &Self) {
        for (id, known) in &other.by_id {
            self.by_id
                .entry(id.clone())
                .or_insert_with(|| known.clone());
        }
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Known> {
        self.by_id.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Known> {
        self.by_id.values()
    }

    /// Registra a definição fixada num cadastro, para que ela também
    /// apareça no catálogo mesmo fora dos diretórios.
    pub fn include(&mut self, path: &Path) {
        if let Ok(yaml) = read(path) {
            self.offer(Source::Local(path.to_owned()), &yaml);
        }
    }

    /// A definição de um cadastro Cardigann. Arquivo fixado é customizado e
    /// vale como está; senão vale a precedência pelo id.
    ///
    /// # Errors
    ///
    /// Arquivo ilegível ou id que nenhuma fonte tem.
    pub fn resolve(&self, id: &str, pinned: Option<&str>) -> Result<Resolved> {
        let pinned = pinned.map(Path::new);
        if let Some(path) = pinned {
            let yaml = read(path)?;
            return Ok(Resolved {
                sha: sha(&yaml),
                yaml,
                local: true,
            });
        }
        if let Some(known) = self.get(id) {
            let yaml = known.yaml()?;
            return Ok(Resolved {
                sha: sha(&yaml),
                yaml,
                local: known.is_local(),
            });
        }
        anyhow::bail!("a definição `{id}` não está em nenhum catálogo")
    }
}

/// Teto do arquivo compactado do repositório: o download inteiro fica em
/// memória antes de ser aberto.
const MAX_ARCHIVE: usize = 64 * 1024 * 1024;
/// Teto de uma definição: o mesmo do executor.
const MAX_DEFINITION: u64 = 1024 * 1024;

/// Baixa o `.tar.gz` do repositório e devolve as definições v11, cada uma
/// com o nome do arquivo sem extensão.
///
/// # Errors
///
/// Rede, HTTP diferente de 200, arquivo grande demais ou ilegível.
pub async fn download(url: &str, timeout: std::time::Duration) -> Result<Vec<(String, String)>> {
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("acervo-hub")
        .build()?;
    let mut response = http
        .get(url)
        .send()
        .await
        .context("baixando o repositório de definições")?;
    anyhow::ensure!(
        response.status().is_success(),
        "o repositório de definições respondeu HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("baixando o repositório de definições")?
    {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= MAX_ARCHIVE,
            "o arquivo do repositório passa de {} MiB",
            MAX_ARCHIVE / 1024 / 1024
        );
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || extract_v11(&bytes))
        .await
        .context("abrindo o arquivo do repositório")?
}

/// As definições de `definitions/v11/` de um `.tar.gz` do repositório (com
/// o diretório de topo que o arquivo da branch traz). Só `.yml`, só desse
/// diretório, nada abaixo dele.
///
/// # Errors
///
/// Arquivo que não é `.tar.gz`, ou entrada ilegível.
pub fn extract_v11(archive: &[u8]) -> Result<Vec<(String, String)>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut found = Vec::new();
    for entry in tar.entries().context("o arquivo não é um .tar.gz")? {
        let mut entry = entry.context("entrada ilegível no arquivo")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .context("caminho ilegível no arquivo")?
            .into_owned();
        let parts: Vec<_> = path.components().collect();
        // <topo>/definitions/v11/<id>.yml
        let [_, first, second, file] = parts.as_slice() else {
            continue;
        };
        if first.as_os_str() != "definitions" || second.as_os_str() != "v11" {
            continue;
        }
        let file = Path::new(file.as_os_str());
        let (Some(id), Some("yml")) = (
            file.file_stem().and_then(|stem| stem.to_str()),
            file.extension().and_then(|ext| ext.to_str()),
        ) else {
            continue;
        };
        if entry.size() > MAX_DEFINITION {
            tracing::warn!(definicao = id, "definição grande demais, pulada");
            continue;
        }
        let mut yaml = String::new();
        if entry.read_to_string(&mut yaml).is_err() {
            tracing::warn!(definicao = id, "definição fora de UTF-8, pulada");
            continue;
        }
        found.push((id.to_owned(), yaml));
    }
    anyhow::ensure!(
        !found.is_empty(),
        "o arquivo do repositório não tem nenhuma definição em definitions/v11"
    );
    found.sort();
    Ok(found)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Uma definição mínima que o executor carrega.
    pub(crate) fn yaml(id: &str, name: &str) -> String {
        format!(
            "---
id: {id}
name: {name}
description: definição de teste
language: pt-BR
type: public
encoding: UTF-8
links:
  - https://tracker.invalid/
caps:
  categorymappings:
    - {{id: 1, cat: Movies, desc: Filmes}}
  modes:
    search: [q]
search:
  paths:
    - path: busca
  inputs:
    q: '{{{{ .Keywords }}}}'
  rows:
    selector: tr
  fields:
    title:
      selector: a
    download:
      selector: a
      attribute: href
    size:
      selector: td.size
"
        )
    }

    fn row(id: &str, name: &str) -> DefinitionRow {
        let yaml = yaml(id, name);
        DefinitionRow {
            id: id.into(),
            sha: sha(&yaml),
            yaml,
            updated_at: String::new(),
        }
    }

    /// Um diretório temporário com os arquivos dados, apagado no fim.
    pub(crate) struct Dir(pub PathBuf);

    impl Dir {
        pub(crate) fn new(name: &str, files: &[(&str, String)]) -> Self {
            let dir = std::env::temp_dir().join(format!("acervo-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, content) in files {
                std::fs::write(dir.join(file), content).unwrap();
            }
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn local_vence_o_banco_e_primeiro_diretorio_vence() {
        let local = Dir::new("precedencia-local", &[("a.yml", yaml("a", "A local"))]);
        let other = Dir::new("precedencia-outro", &[("a.yml", yaml("a", "A outro"))]);
        let definitions = Definitions::assemble(
            &[local.0.clone(), other.0.clone()],
            &[row("a", "A banco"), row("b", "B banco")],
        );
        assert_eq!(definitions.get("a").unwrap().header.name, "A local");
        assert_eq!(definitions.get("b").unwrap().header.name, "B banco");
        assert!(definitions.get("a").unwrap().is_local());
        assert!(matches!(
            definitions.get("b").unwrap().source,
            Source::Database(_)
        ));
        assert!(definitions.iter().all(|known| known.refusal.is_none()));
    }

    #[test]
    fn arquivo_fixado_vence_e_sem_fixar_vale_o_id() {
        let local = Dir::new("fixado-local", &[("b.yml", yaml("b", "B local"))]);
        let mut definitions = Definitions::assemble(&[], &[row("b", "B banco")]);
        let pinned = local.0.join("b.yml");
        definitions.include(&pinned);
        let resolved = definitions.resolve("b", pinned.to_str()).unwrap();
        assert!(resolved.yaml.contains("B local") && resolved.local);
        assert_eq!(resolved.sha, sha(&yaml("b", "B local")));
        let resolved = definitions.resolve("b", None).unwrap();
        assert!(resolved.yaml.contains("B banco") && !resolved.local);
        assert!(definitions.resolve("zzz", None).is_err());
    }

    fn tar_gz(files: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, content.as_bytes())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extrai_so_as_definicoes_v11() {
        let archive = tar_gz(&[
            ("Indexers-master/definitions/v11/um.yml", "id: um"),
            ("Indexers-master/definitions/v11/dois.yml", "id: dois"),
            ("Indexers-master/definitions/v10/um.yml", "id: velho"),
            ("Indexers-master/definitions/v11/leia.md", "nada"),
            ("Indexers-master/definitions/v11/sub/tres.yml", "id: tres"),
            ("Indexers-master/README.md", "nada"),
        ]);
        let found = extract_v11(&archive).unwrap();
        assert_eq!(
            found,
            [
                ("dois".to_owned(), "id: dois".to_owned()),
                ("um".to_owned(), "id: um".to_owned()),
            ]
        );
        assert!(extract_v11(b"isto nem e gzip").is_err());
        assert!(extract_v11(&tar_gz(&[("x/README.md", "nada")])).is_err());
    }
}

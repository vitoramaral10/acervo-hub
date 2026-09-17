//! Filesystem: tradução de caminho container→host e leitura de `stat(2)`.
//!
//! O cliente de download reporta caminhos como **ele** os vê, de dentro do
//! container. Quem roda a reconciliação vê outra árvore. Comparar por
//! `basename` para contornar isso dá falso negativo — nome de arquivo se repete
//! entre temporadas e entre obras. A tradução é por caminho completo.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use acervo_core::{Allocated, Apparent, FileFacts};

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("caminho `{path}` não casa com nenhum mapeamento configurado")]
    Unmapped { path: PathBuf },

    #[error("não foi possível ler `{path}`: {source}")]
    Stat {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Tradução de prefixo de caminho, do que o cliente vê para o que o host vê.
#[derive(Debug, Clone, Default)]
pub struct PathMap {
    /// Ordenado por prefixo mais longo primeiro, para que o mapeamento mais
    /// específico vença quando dois se sobrepõem.
    rules: Vec<(PathBuf, PathBuf)>,
}

impl PathMap {
    #[must_use]
    pub fn new(rules: impl IntoIterator<Item = (PathBuf, PathBuf)>) -> Self {
        let mut rules: Vec<_> = rules.into_iter().collect();
        rules.sort_by_key(|(from, _)| std::cmp::Reverse(from.as_os_str().len()));
        Self { rules }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Traduz um caminho do cliente para o host.
    ///
    /// Sem mapeamento nenhum configurado, o caminho passa inalterado — é o caso
    /// de quem roda tudo no mesmo namespace de filesystem.
    ///
    /// # Errors
    ///
    /// [`FsError::Unmapped`] quando há mapeamentos, mas nenhum casa. Não casar
    /// é erro, não passagem direta: um caminho não traduzido apontaria para o
    /// lugar errado do host, e `stat` ali responderia sobre outro arquivo.
    pub fn to_host(&self, client_path: &Path) -> Result<PathBuf, FsError> {
        if self.rules.is_empty() {
            return Ok(client_path.to_path_buf());
        }

        for (from, to) in &self.rules {
            if let Ok(rest) = client_path.strip_prefix(from) {
                return Ok(to.join(rest));
            }
        }

        Err(FsError::Unmapped {
            path: client_path.to_path_buf(),
        })
    }
}

/// Lê os fatos de `stat(2)` de um arquivo, já traduzido para o host.
///
/// # Errors
///
/// [`FsError::Unmapped`] ou [`FsError::Stat`]. Falha nunca vira "sem link":
/// não saber se a biblioteca aponta para o inode é motivo para não decidir.
pub fn facts_for(client_path: &Path, map: &PathMap) -> Result<FileFacts, FsError> {
    let host = map.to_host(client_path)?;
    let meta = fs::metadata(&host).map_err(|source| FsError::Stat {
        path: host.clone(),
        source,
    })?;
    let modified = meta.modified().map_err(|source| FsError::Stat {
        path: host.clone(),
        source,
    })?;

    Ok(FileFacts {
        path: host,
        links: meta.nlink(),
        allocated: Allocated::from_blocks(meta.blocks()),
        apparent: Apparent::from_bytes(meta.size()),
        modified,
    })
}

/// Soma o espaço alocado sob as raízes, contando cada inode uma vez.
///
/// A deduplicação por `(dev, ino)` não é otimização: sem ela, um acervo em que
/// biblioteca e downloads compartilham hardlink conta o mesmo arquivo duas
/// vezes e infla a medida — que é justamente a base da trava proporcional.
///
/// Erros de leitura de entrada individual são registrados e pulados; a medida
/// segue com o que deu para ler.
#[must_use]
pub fn measure_roots(roots: &[PathBuf]) -> Allocated {
    let mut seen = HashSet::new();
    let mut total = Allocated::ZERO;
    let mut pending: Vec<PathBuf> = roots.to_vec();

    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(path = %dir.display(), %err, "raiz ilegível, pulando");
                continue;
            }
        };

        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };

            if meta.is_dir() {
                pending.push(entry.path());
            } else if seen.insert((meta.dev(), meta.ino())) {
                total = total + Allocated::from_blocks(meta.blocks());
            }
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> PathMap {
        PathMap::new([
            (PathBuf::from("/media"), PathBuf::from("/mnt/acervo")),
            (
                PathBuf::from("/media/downloads"),
                PathBuf::from("/mnt/rapido/downloads"),
            ),
        ])
    }

    #[test]
    fn prefixo_mais_especifico_vence() {
        let m = map();
        assert_eq!(
            m.to_host(Path::new("/media/downloads/a.mkv")).unwrap(),
            PathBuf::from("/mnt/rapido/downloads/a.mkv")
        );
        assert_eq!(
            m.to_host(Path::new("/media/series/b.mkv")).unwrap(),
            PathBuf::from("/mnt/acervo/series/b.mkv")
        );
    }

    #[test]
    fn caminho_fora_do_mapa_e_erro_e_nao_passagem_direta() {
        let erro = map().to_host(Path::new("/outro/lugar/c.mkv")).unwrap_err();
        assert!(matches!(erro, FsError::Unmapped { .. }));
    }

    #[test]
    fn sem_mapeamento_o_caminho_passa_inalterado() {
        let m = PathMap::default();
        assert_eq!(
            m.to_host(Path::new("/media/x.mkv")).unwrap(),
            PathBuf::from("/media/x.mkv")
        );
    }

    #[test]
    fn arquivo_inexistente_vira_erro_de_stat() {
        let m = PathMap::default();
        let erro = facts_for(Path::new("/nao/existe/mesmo.mkv"), &m).unwrap_err();
        assert!(matches!(erro, FsError::Stat { .. }));
    }

    #[test]
    fn raiz_inexistente_mede_zero_sem_estourar() {
        assert_eq!(
            measure_roots(&[PathBuf::from("/nao/existe/mesmo")]),
            Allocated::ZERO
        );
    }
}

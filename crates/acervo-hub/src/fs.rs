//! Leitura do disco: stat(2), espaço livre e tamanho das raízes.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use acervo_core::{Allocated, Apparent, FileFacts};

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("não foi possível ler `{path}`: {source}")]
    Stat {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Lê os fatos de `stat(2)` de um arquivo.
///
/// # Errors
///
/// [`FsError::Stat`]. Falha nunca vira "sem link":
/// não saber se a biblioteca aponta para o inode é motivo para não decidir.
pub fn facts_for(host: &Path) -> Result<FileFacts, FsError> {
    let host = host.to_path_buf();
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

/// Bytes livres para quem não é root no sistema de arquivos de `path`.
///
/// # Errors
///
/// Caminho inexistente ou ilegível.
pub fn free_space(path: &Path) -> std::io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
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

    #[test]
    fn arquivo_inexistente_vira_erro_de_stat() {
        let erro = facts_for(Path::new("/nao/existe/mesmo.mkv")).unwrap_err();
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

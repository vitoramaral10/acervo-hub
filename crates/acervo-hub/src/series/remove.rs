//! Remoção de episódio, temporada ou seleção: apaga os hardlinks, tira os
//! arquivos do catálogo e marca os episódios `deleted`. Arquivo
//! multi-episódio sai inteiro, e todos os episódios dele ficam `deleted`.
//! Depois, cada torrent que tinha arquivo com o inode de um apagado é
//! conferido: se nenhum arquivo dele continua com outro link, ele sai do
//! cliente com os dados. Pacote com outro episódio ainda na biblioteca fica,
//! e torrent de grab ainda baixando também.
//!
//! O `deleted` é gravado antes de apagar: falha no meio deixa arquivo a
//! mais, nunca episódio de volta à busca.

use std::collections::{BTreeSet, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use acervo_core::DownloadHash;
use acervo_store::{GrabState, Skip, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::events::{self, Event, Kind};

/// Um arquivo de torrent no disco: dispositivo, inode e quantos links tem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnDisk {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
}

/// A regra do torrent, sem IO: sai quando tinha arquivo com o inode de um
/// apagado e nenhum arquivo dele tem mais outro link. Arquivo que não está
/// no disco (prioridade zero, nunca baixado) não conta.
#[must_use]
pub fn torrent_goes(files: &[OnDisk], deleted: &HashSet<(u64, u64)>) -> bool {
    files.iter().any(|f| deleted.contains(&(f.dev, f.ino))) && files.iter().all(|f| f.nlink <= 1)
}

/// Junta o `stat` dos arquivos de um torrent. Só o que não existe
/// (`NotFound`) fica de fora; qualquer outro erro (permissão, disco fora)
/// devolve `None`: sem saber os links, o torrent fica.
#[must_use]
pub fn on_disk(stats: impl IntoIterator<Item = std::io::Result<OnDisk>>) -> Option<Vec<OnDisk>> {
    let mut files = Vec::new();
    for stat in stats {
        match stat {
            Ok(file) => files.push(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(files)
}

/// O `stat` de cada caminho, pela regra de [`on_disk`]. Bloqueia.
pub(crate) fn stat_all(paths: &[PathBuf]) -> Option<Vec<OnDisk>> {
    on_disk(paths.iter().map(|path| {
        std::fs::metadata(path).map(|meta| OnDisk {
            dev: meta.dev(),
            ino: meta.ino(),
            nlink: meta.nlink(),
        })
    }))
}

/// O que a remoção fez.
#[derive(Debug, Default, Serialize)]
pub struct Removal {
    pub arquivos: Vec<String>,
    pub episodios: usize,
    pub torrents: Vec<String>,
    /// Torrent que devia sair e não saiu; fica para o ciclo de limpeza.
    pub aviso: Option<String>,
}

/// Apaga episódios da série (os arquivos deles, se tiverem) e os marca
/// `deleted`. Episódio sem arquivo só ganha o `skip`.
///
/// # Errors
///
/// Série desconhecida, episódio de outra série, falha ao apagar ou de
/// escrita.
pub async fn delete_episodes(
    config: &Config,
    store: &Store,
    series_id: i64,
    episode_ids: &[i64],
) -> Result<Removal> {
    let entry = store
        .series(series_id)
        .await?
        .context("série fora do catálogo")?;
    let asked: HashSet<i64> = episode_ids.iter().copied().collect();
    if asked
        .iter()
        .any(|id| !entry.episodes.iter().any(|e| e.id == *id))
    {
        bail!("há episódio que não é desta série");
    }
    let file_ids: BTreeSet<i64> = entry
        .episodes
        .iter()
        .filter(|e| asked.contains(&e.id))
        .filter_map(|e| e.file_id)
        .collect();
    // Os episódios que dividem arquivo com um apagado perdem o arquivo
    // junto, e ficam `deleted` também: senão voltariam à busca.
    let mut affected: Vec<i64> = entry
        .episodes
        .iter()
        .filter(|e| asked.contains(&e.id) || e.file_id.is_some_and(|f| file_ids.contains(&f)))
        .map(|e| e.id)
        .collect();
    affected.sort_unstable();

    // Antes de apagar: se algo falhar no meio, nenhum episódio volta à busca.
    store
        .set_skip(&affected, Some(Skip::Deleted), &now_rfc3339())
        .await?;

    let map = config.path_map();
    let mut removal = Removal {
        episodios: affected.len(),
        ..Removal::default()
    };
    let mut inodes = HashSet::new();
    for file in entry.files.iter().filter(|f| file_ids.contains(&f.id)) {
        let path = PathBuf::from(&entry.series.path).join(&file.file.relative_path);
        // O vídeo e as legendas dele: também elas são hardlink do torrent.
        let hosts = std::iter::once(file.file.relative_path.clone())
            .chain(super::subtitle_paths(&entry, file.id))
            .map(|relative| map.to_host(&PathBuf::from(&entry.series.path).join(relative)))
            .collect::<Result<Vec<PathBuf>, _>>()?;
        let gone = tokio::task::spawn_blocking(move || super::remove_files(&hosts))
            .await?
            .with_context(|| format!("apagando `{}`", path.display()))?;
        inodes.extend(gone);
        store.delete_episode_file(file.id).await?;
        let covered: Vec<i64> = entry
            .episodes
            .iter()
            .filter(|e| e.file_id == Some(file.id))
            .map(|e| e.id)
            .collect();
        events::record(
            store,
            Event {
                source_title: file.file.scene_name.clone(),
                quality: Some(file.file.quality.quality),
                message: Some(file.file.relative_path.clone()),
                poster: entry.series.poster.clone(),
                ..Event::series(
                    Kind::FileDeleted,
                    entry.id,
                    super::label(&entry, &covered),
                    &covered,
                )
            },
        )
        .await;
        removal.arquivos.push(file.file.relative_path.clone());
    }
    if !inodes.is_empty() {
        match orphaned_torrents(config, store, &inodes).await {
            Ok(found) if found.is_empty() => {}
            Ok(found) => match crate::library::delete_downloads(config, found).await {
                Ok(names) => removal.torrents = names,
                Err(error) => removal.aviso = Some(error),
            },
            Err(error) => removal.aviso = Some(format!("{error:#}")),
        }
    }
    if let Some(warning) = &removal.aviso {
        tracing::warn!(
            serie = entry.series.title,
            "torrent não apagado; fica para o ciclo de limpeza: {warning}"
        );
    }
    Ok(removal)
}

/// Os torrents que a regra manda apagar, depois de os hardlinks saírem.
/// Torrent de grab de série ainda baixando nunca entra: os arquivos dele
/// ainda vão virar episódio.
async fn orphaned_torrents(
    config: &Config,
    store: &Store,
    deleted: &HashSet<(u64, u64)>,
) -> Result<Vec<(DownloadHash, String)>> {
    let busy: HashSet<String> = store
        .series_grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| g.hash)
        .collect();
    let map = config.path_map();
    let qbit = crate::grab::qbit(config).await?;
    let mut candidates: Vec<(DownloadHash, String, Vec<PathBuf>)> = Vec::new();
    for torrent in qbit.torrents().await.context("listando os torrents")? {
        if busy.contains(&torrent.hash.to_ascii_lowercase()) {
            continue;
        }
        let hash = DownloadHash::new(&torrent.hash);
        let files = qbit
            .files(&hash)
            .await
            .context("listando os arquivos de um torrent")?;
        let paths = files
            .iter()
            .filter_map(|file| {
                map.to_host(&acervo_clients::client_path(&torrent, file))
                    .ok()
            })
            .collect();
        candidates.push((hash, torrent.name, paths));
    }
    let deleted = deleted.clone();
    Ok(tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|(_, _, paths)| {
                stat_all(paths).is_some_and(|files| torrent_goes(&files, &deleted))
            })
            .map(|(hash, name, _)| (hash, name))
            .collect()
    })
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(ino: u64, nlink: u64) -> OnDisk {
        OnDisk { dev: 1, ino, nlink }
    }

    #[test]
    fn torrent_sai_quando_nenhum_arquivo_tem_outro_link() {
        let deleted = HashSet::from([(1, 10)]);
        // Avulso: o único arquivo era o apagado, agora com um link só.
        assert!(torrent_goes(&[disk(10, 1)], &deleted));
        // Pacote com outro episódio ainda na biblioteca: fica.
        assert!(!torrent_goes(&[disk(10, 1), disk(11, 2)], &deleted));
        // Pacote em que todos os outros também já saíram: vai.
        assert!(torrent_goes(&[disk(10, 1), disk(11, 1)], &deleted));
        // Torrent que nada tinha com o apagado: fica, mesmo sem links.
        assert!(!torrent_goes(&[disk(12, 1)], &deleted));
        // Arquivo nunca baixado não está no disco: não entra na lista.
        assert!(!torrent_goes(&[], &deleted));
    }

    #[test]
    fn so_o_que_nao_existe_fica_de_fora_do_stat() {
        use std::io::{Error, ErrorKind};
        let read = on_disk([Ok(disk(10, 1)), Err(Error::from(ErrorKind::NotFound))]);
        assert_eq!(read, Some(vec![disk(10, 1)]));
        // Sem permissão: não se sabe os links, e o torrent fica.
        let read = on_disk([
            Ok(disk(10, 1)),
            Err(Error::from(ErrorKind::PermissionDenied)),
        ]);
        assert_eq!(read, None);
        let deleted = HashSet::from([(1, 10)]);
        assert!(!read.is_some_and(|files| torrent_goes(&files, &deleted)));
    }

    #[test]
    fn regra_vale_no_disco_de_verdade() {
        let root = std::env::temp_dir().join(format!("acervo-serie-rm-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let download = root.join("baixado.mkv");
        let library = root.join("biblioteca.mkv");
        std::fs::write(&download, b"video").unwrap();
        std::fs::hard_link(&download, &library).unwrap();
        let meta = std::fs::metadata(&library).unwrap();
        let deleted = HashSet::from([(meta.dev(), meta.ino())]);
        let read = |path: &std::path::Path| {
            let meta = std::fs::metadata(path).unwrap();
            OnDisk {
                dev: meta.dev(),
                ino: meta.ino(),
                nlink: meta.nlink(),
            }
        };
        assert!(!torrent_goes(&[read(&download)], &deleted));
        std::fs::remove_file(&library).unwrap();
        assert!(torrent_goes(&[read(&download)], &deleted));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

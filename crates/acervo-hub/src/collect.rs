//! Coleta o inventário de um ciclo: a fila do acervo, o cliente de download
//! e o disco.

use std::collections::HashSet;

use acervo_clients::{QbitClient, TorrentInfo, client_path, state_from_qbit};

use acervo_core::{Allocated, Download, DownloadHash, FileFacts, Inventory, UnreadableDownload};
use acervo_store::{Grab, GrabState, SeriesGrab, Store};
use anyhow::{Context, Result};

use crate::config::Config;

/// Tudo que um ciclo precisa: o que foi lido e por onde agir.
#[derive(Debug)]
pub struct Session {
    pub inventory: Inventory,
    pub qbit: QbitClient,
}

/// Lê a fila do acervo, o cliente de download e os fatos do disco. Sem os
/// grabs, um download em andamento pareceria sem dono: falha de leitura
/// impede o ciclo antes de planejar qualquer remoção.
///
/// # Errors
///
/// Falha ao ler os grabs, autenticar ou consultar o cliente de download.
pub async fn collect(config: &Config, store: &Store) -> Result<Session> {
    let qbit_config = config.janitor()?;
    let qbit = QbitClient::login(
        &qbit_config.url,
        &qbit_config.username,
        &qbit_config.password,
        crate::config::HTTP_TIMEOUT,
    )
    .await
    .context("autenticando no qBittorrent")?;

    let mut inventory = Inventory::new(Allocated::ZERO);

    inventory.queued_hashes = internal_hashes_from(store)
        .await
        .context("lendo os grabs em andamento")?;
    tracing::info!(fila = inventory.queued_hashes.len(), "fila interna lida");

    let torrents = qbit.torrents().await.context("listando torrents")?;
    tracing::info!(total = torrents.len(), "torrents no cliente");

    for torrent in torrents {
        match read_download(&qbit, &torrent).await {
            Ok(download) => inventory.downloads.push(download),
            Err(unreadable) => {
                tracing::warn!(
                    torrent = %unreadable.name,
                    caminho = %unreadable.path.display(),
                    motivo = %unreadable.reason,
                    "arquivos ilegíveis; torrent fica fora de qualquer decisão"
                );
                inventory.unreadable.push(unreadable);
            }
        }
    }

    inventory.library_size = acervo_fs::measure_roots(&config.library.roots);
    tracing::info!(biblioteca = %inventory.library_size, "biblioteca medida");

    Ok(Session { inventory, qbit })
}

async fn internal_hashes_from(store: &Store) -> Result<HashSet<DownloadHash>> {
    let grabs = store.grabs().await?;
    let series_grabs = store.series_grabs().await?;
    Ok(internal_hashes(&grabs, &series_grabs))
}

/// Os hashes dos grabs em andamento, de filme e de série. O mesmo torrent
/// pode aparecer mais de uma vez: para a limpeza basta saber que está na fila.
fn internal_hashes(grabs: &[Grab], series_grabs: &[SeriesGrab]) -> HashSet<DownloadHash> {
    let movies = grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| DownloadHash::new(&g.hash));
    let episodes = series_grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| DownloadHash::new(&g.hash));
    movies.chain(episodes).collect()
}

/// Monta um [`Download`] com os fatos de disco de cada arquivo.
///
/// Basta **um** arquivo ilegível para o torrent inteiro sair do inventário: um
/// `stat` que falha não diz "sem hardlink", diz "não sei" — e o cálculo de
/// espaço recuperável e de vínculo com a biblioteca depende de todos.
async fn read_download(
    qbit: &QbitClient,
    torrent: &TorrentInfo,
) -> Result<Download, UnreadableDownload> {
    let hash = DownloadHash::new(&torrent.hash);
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

    let files = qbit.files(&hash).await.map_err(|err| UnreadableDownload {
        hash: hash.clone(),
        name: torrent.name.clone(),
        path: std::path::PathBuf::from(&torrent.save_path),
        reason: err.to_string(),
    })?;

    let mut facts: Vec<FileFacts> = Vec::with_capacity(files.len());
    for file in &files {
        let path = client_path(torrent, file);
        let fact = match acervo_fs::facts_for(&path) {
            Ok(fact) => fact,
            // Download incompleto cujo arquivo o cliente ainda não criou: não
            // há nada no disco, e o torrent segue na decisão — um órfão pausado
            // que nunca começou precisa poder sair da fila. Em torrent
            // completo, arquivo ausente é caminho errado, e caminho errado
            // faria um seed parecer sem hardlink: esse continua sendo erro.
            Err(acervo_fs::FsError::Stat { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound && torrent.progress < 1.0 =>
            {
                continue;
            }
            Err(err) => {
                return Err(UnreadableDownload {
                    hash: hash.clone(),
                    name: torrent.name.clone(),
                    path: path.clone(),
                    reason: err.to_string(),
                });
            }
        };
        facts.push(fact);
    }

    Ok(Download {
        hash,
        name: torrent.name.clone(),
        state: state_from_qbit(&torrent.state),
        private: torrent.is_private(),
        category: torrent.category.clone(),
        ratio: torrent.ratio,
        seeded_for: torrent.seeded_for(),
        idle_for: Some(torrent.idle_for(now_epoch)),
        files: facts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grab(id: i64, hash: &str, state: GrabState) -> Grab {
        Grab {
            id,
            movie_id: 10 + id,
            hash: hash.into(),
            title: format!("filme {id}"),
            indexer: "idx".into(),
            quality: acervo_parser::Quality::Unknown,
            size: 1,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces: None,
        }
    }

    fn series_grab(id: i64, hash: &str, state: GrabState) -> SeriesGrab {
        SeriesGrab {
            id,
            series_id: 20 + id,
            episode_ids: vec![1],
            hash: hash.into(),
            title: format!("série {id}"),
            indexer: "idx".into(),
            quality: acervo_parser::Quality::Unknown,
            size: 1,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state,
            message: None,
            finished_at: None,
        }
    }

    #[test]
    fn fila_interna_so_leva_grab_em_andamento() {
        let snap = internal_hashes(
            &[
                grab(1, "AA", GrabState::Downloading),
                grab(2, "bb", GrabState::Imported),
                grab(3, "cc", GrabState::Failed),
            ],
            &[
                series_grab(1, "dd", GrabState::Downloading),
                series_grab(2, "ee", GrabState::Failed),
            ],
        );

        assert_eq!(
            snap,
            HashSet::from([DownloadHash::new("aa"), DownloadHash::new("dd")])
        );
    }

    #[test]
    fn hashes_duplicados_contam_uma_vez() {
        let hashes = internal_hashes(
            &[grab(1, "AA", GrabState::Downloading)],
            &[series_grab(1, "aa", GrabState::Downloading)],
        );
        assert_eq!(hashes, HashSet::from([DownloadHash::new("aa")]));
    }
}

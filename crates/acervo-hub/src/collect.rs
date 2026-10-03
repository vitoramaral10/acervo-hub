//! Coleta o inventário de um ciclo: instâncias, cliente de download e disco.

use std::collections::HashMap;

use acervo_arr::ArrClient;
use acervo_clients::{QbitClient, TorrentInfo, client_path, state_from_qbit};
use acervo_core::{
    Allocated, Download, DownloadHash, FileFacts, InstanceName, InstanceSnapshot, Inventory,
    QueueItem, QueueItemId, UnreachableInstance, UnreadableDownload, WorkId,
};
use acervo_fs::PathMap;
use acervo_store::{Grab, GrabState, SeriesGrab, Store};
use anyhow::{Context, Result};

use crate::config::Config;

/// Tudo que um ciclo precisa: o que foi lido e por onde agir.
#[derive(Debug)]
pub struct Session {
    pub inventory: Inventory,
    pub arrs: HashMap<InstanceName, ArrClient>,
    pub qbit: QbitClient,
}

/// Nome da instância interna: a fila do próprio acervo.
const INTERNAL: &str = "acervo";

/// Lê o mundo.
///
/// Instância que falha **não** interrompe a coleta: ela é registrada como
/// inalcançável e o planejador aborta depois, com o motivo em mãos. Já o
/// cliente de download fora do ar é falha dura — sem ele não há o que
/// reconciliar, e prosseguir faria toda a fila parecer sem torrent.
///
/// # Errors
///
/// Falha ao autenticar ou consultar o cliente de download, ou ao montar algum
/// cliente HTTP.
///
/// A fila do próprio acervo entra como mais uma instância. Se o catálogo não
/// puder ser lido, ela é registrada como inalcançável, como qualquer outra:
/// sem os grabs, todo download em andamento pareceria sem dono.
pub async fn collect(config: &Config, store: &Store) -> Result<Session> {
    let qbit_config = config.janitor()?;
    let qbit = QbitClient::login(
        &qbit_config.url,
        &qbit_config.username,
        &qbit_config.password,
        config.http_timeout(),
    )
    .await
    .context("autenticando no qBittorrent")?;

    let mut inventory = Inventory::new(Allocated::ZERO);
    let mut arrs = HashMap::new();

    match internal_snapshot_from(store).await {
        Ok(snapshot) => {
            tracing::info!(
                fila = snapshot.queue.len(),
                obras = snapshot.known_works,
                "fila interna lida"
            );
            inventory.snapshots.push(snapshot);
        }
        Err(reason) => {
            tracing::warn!(erro = %reason, "catálogo não respondeu");
            inventory.unreachable.push(UnreachableInstance {
                instance: InstanceName::new(INTERNAL),
                reason,
            });
        }
    }

    for spec in &config.instances {
        let name = InstanceName::new(spec.name.clone());
        let client = ArrClient::new(
            name.clone(),
            &spec.url,
            &spec.api_key,
            spec.kind.into(),
            config.http_timeout(),
        )
        .with_context(|| format!("montando o cliente da instância `{name}`"))?;

        match client.snapshot().await {
            Ok(snapshot) => {
                tracing::info!(
                    instancia = %name,
                    fila = snapshot.queue.len(),
                    obras = snapshot.known_works,
                    "instância lida"
                );
                inventory.snapshots.push(snapshot);
            }
            Err(err) => {
                tracing::warn!(instancia = %name, erro = %err, "instância não respondeu");
                inventory.unreachable.push(UnreachableInstance {
                    instance: name.clone(),
                    reason: err.short(),
                });
            }
        }

        arrs.insert(name, client);
    }

    let torrents = qbit.torrents().await.context("listando torrents")?;
    tracing::info!(total = torrents.len(), "torrents no cliente");

    let map = config.path_map();
    for torrent in torrents {
        match read_download(&qbit, &torrent, &map).await {
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

    Ok(Session {
        inventory,
        arrs,
        qbit,
    })
}

async fn internal_snapshot_from(store: &Store) -> Result<InstanceSnapshot, String> {
    let grabs = store.grabs().await.map_err(|e| e.to_string())?;
    let series_grabs = store.series_grabs().await.map_err(|e| e.to_string())?;
    let movies = store.movies().await.map_err(|e| e.to_string())?;
    let series = store.series_list().await.map_err(|e| e.to_string())?;
    Ok(internal_snapshot(
        &grabs,
        &series_grabs,
        movies.len() + series.len(),
    ))
}

/// A fila interna: um item por grab em andamento, de filme e de série.
///
/// Nenhum item é órfão — o grab sempre aponta para uma obra do catálogo. O id
/// do item é o do grab; o de série vai negativo para não colidir com o de
/// filme, que vivem em tabelas separadas.
fn internal_snapshot(
    grabs: &[Grab],
    series_grabs: &[SeriesGrab],
    known_works: usize,
) -> InstanceSnapshot {
    let instance = InstanceName::new(INTERNAL);
    let movies = grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| (g.id, &g.hash, &g.title, g.movie_id));
    let episodes = series_grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| (-g.id, &g.hash, &g.title, g.series_id));

    let queue = movies
        .chain(episodes)
        .map(|(id, hash, title, work)| QueueItem {
            id: QueueItemId(id),
            instance: instance.clone(),
            title: title.clone(),
            download: Some(DownloadHash::new(hash)),
            work: Some(WorkId(work)),
        })
        .collect();

    InstanceSnapshot {
        instance,
        queue,
        known_works,
    }
}

/// Monta um [`Download`] com os fatos de disco de cada arquivo.
///
/// Basta **um** arquivo ilegível para o torrent inteiro sair do inventário: um
/// `stat` que falha não diz "sem hardlink", diz "não sei" — e o cálculo de
/// espaço recuperável e de vínculo com a biblioteca depende de todos.
async fn read_download(
    qbit: &QbitClient,
    torrent: &TorrentInfo,
    map: &PathMap,
) -> Result<Download, UnreadableDownload> {
    let hash = DownloadHash::new(&torrent.hash);

    let files = qbit.files(&hash).await.map_err(|err| UnreadableDownload {
        hash: hash.clone(),
        name: torrent.name.clone(),
        path: std::path::PathBuf::from(&torrent.save_path),
        reason: err.to_string(),
    })?;

    let mut facts: Vec<FileFacts> = Vec::with_capacity(files.len());
    for file in &files {
        let path = client_path(torrent, file);
        let fact = match acervo_fs::facts_for(&path, map) {
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
        let snap = internal_snapshot(
            &[
                grab(1, "AA", GrabState::Downloading),
                grab(2, "bb", GrabState::Imported),
                grab(3, "cc", GrabState::Failed),
            ],
            &[
                series_grab(1, "dd", GrabState::Downloading),
                series_grab(2, "ee", GrabState::Failed),
            ],
            7,
        );

        assert_eq!(snap.instance, InstanceName::new("acervo"));
        assert_eq!(snap.known_works, 7);
        let hashes: Vec<_> = snap
            .queue
            .iter()
            .map(|i| i.download.as_ref().unwrap().as_str())
            .collect();
        assert_eq!(hashes, ["aa", "dd"]);
    }

    #[test]
    fn ids_de_filme_e_serie_nao_colidem_e_nenhum_item_e_orfao() {
        let snap = internal_snapshot(
            &[grab(1, "aa", GrabState::Downloading)],
            &[series_grab(1, "bb", GrabState::Downloading)],
            2,
        );

        assert_eq!(snap.queue[0].id, QueueItemId(1));
        assert_eq!(snap.queue[1].id, QueueItemId(-1));
        assert_eq!(snap.queue[0].work, Some(WorkId(11)));
        assert_eq!(snap.queue[1].work, Some(WorkId(21)));
        assert!(snap.queue.iter().all(|i| !i.is_orphaned()));
    }
}

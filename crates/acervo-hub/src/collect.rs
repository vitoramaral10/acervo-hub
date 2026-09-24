//! Coleta o inventário de um ciclo: instâncias, cliente de download e disco.

use std::collections::HashMap;

use acervo_arr::ArrClient;
use acervo_clients::{QbitClient, TorrentInfo, client_path, state_from_qbit};
use acervo_core::{
    Allocated, Download, DownloadHash, FileFacts, InstanceName, Inventory, UnreachableInstance,
    UnreadableDownload,
};
use acervo_fs::PathMap;
use anyhow::{Context, Result};

use crate::config::Config;

/// Tudo que um ciclo precisa: o que foi lido e por onde agir.
#[derive(Debug)]
pub struct Session {
    pub inventory: Inventory,
    pub arrs: HashMap<InstanceName, ArrClient>,
    pub qbit: QbitClient,
}

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
pub async fn collect(config: &Config) -> Result<Session> {
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

    let roots: Vec<_> = config
        .library
        .roots
        .iter()
        .map(|r| crate::config::expand_tilde(r))
        .collect();
    inventory.library_size = acervo_fs::measure_roots(&roots);
    tracing::info!(biblioteca = %inventory.library_size, "biblioteca medida");

    Ok(Session {
        inventory,
        arrs,
        qbit,
    })
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
        let fact = acervo_fs::facts_for(&path, map).map_err(|err| UnreadableDownload {
            hash: hash.clone(),
            name: torrent.name.clone(),
            path: path.clone(),
            reason: err.to_string(),
        })?;
        facts.push(fact);
    }

    Ok(Download {
        hash,
        name: torrent.name.clone(),
        state: state_from_qbit(&torrent.state),
        private: torrent.is_private(),
        ratio: torrent.ratio,
        seeded_for: torrent.seeded_for(),
        files: facts,
    })
}

//! Grab e importação: o acervo pega o release que a decisão escolheu e, quando
//! o download termina, liga o arquivo na pasta do filme.
//!
//! O que é pego vai ao cliente numa categoria própria, que o gerenciador de
//! filmes não importa. A importação só cria: hardlink do arquivo baixado com o
//! nome que o gerenciador daria, na pasta do filme. Filme que já tem arquivo
//! fica de fora — upgrade, que troca um arquivo por outro, ainda é dele. Depois
//! de ligar, o gerenciador relê a pasta e adota o arquivo como dele.

use std::path::{Path, PathBuf};

use acervo_api::Catalog;
use acervo_clients::{AddOptions, NewTorrent, QbitClient, client_path, info_hash, magnet_hash};
use acervo_store::{Grab, GrabState, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::naming::movie_file_stem;
use crate::shadow::{Decider, label, movie_client, now_rfc3339, summarize};

/// Extensões de vídeo que a importação aceita.
const VIDEO: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "ts", "m2ts", "wmv", "mov", "webm",
];

/// O que um grab escolheu.
#[derive(Debug, Serialize)]
pub struct Picked {
    pub titulo: String,
    pub indexador: String,
    pub qualidade: &'static str,
    pub tamanho: u64,
}

/// Resultado de um grab, pego ou só planejado.
#[derive(Debug, Serialize)]
pub struct GrabReport {
    pub filme: String,
    pub releases: usize,
    pub escolhido: Option<Picked>,
    /// Motivo de rejeição e quantos releases ele barrou.
    pub motivos: Vec<(String, usize)>,
    pub aplicado: bool,
}

async fn qbit(config: &Config) -> Result<QbitClient> {
    let spec = config
        .qbittorrent
        .as_ref()
        .context("seção `[qbittorrent]` ausente: o grab manda o torrent ao cliente")?;
    QbitClient::login(
        &spec.url,
        &spec.username,
        &spec.password,
        config.http_timeout(),
    )
    .await
    .context("entrando no qBittorrent")
}

/// Busca o filme, decide e, com `apply`, manda o escolhido ao cliente.
///
/// # Errors
///
/// Filme fora do catálogo ou que já tem arquivo, busca que falhou em todos os
/// indexadores, `.torrent` inválido, cliente inalcançável.
pub async fn grab(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    apply: bool,
) -> Result<GrabReport> {
    let decider = Decider::load(config, store, catalog).await?;
    let movie = decider
        .target(movie_id)
        .context("filme fora do catálogo ou sem perfil de qualidade")?;
    if movie.file.is_some() {
        bail!("o filme já tem arquivo; upgrade ainda é do gerenciador de filmes");
    }
    let outcome = decider
        .decide(catalog, movie)
        .await
        .map_err(|error| anyhow::anyhow!("busca falhou: {error}"))?;
    let mut report = GrabReport {
        filme: label(movie),
        releases: outcome.releases.len(),
        escolhido: None,
        motivos: summarize(&outcome.decisions, movie.id),
        aplicado: false,
    };
    let Some((decision, release)) = outcome.pick() else {
        return Ok(report);
    };
    let quality = decision
        .parsed
        .as_ref()
        .map_or(acervo_parser::Quality::Unknown, |p| p.quality.quality);
    report.escolhido = Some(Picked {
        titulo: release.title.clone(),
        indexador: release.indexer.clone(),
        qualidade: quality.name(),
        tamanho: release.size,
    });
    if !apply {
        return Ok(report);
    }

    let (torrent, hash) = if release.download_url.scheme() == "magnet" {
        let link = release.download_url.to_string();
        let hash = magnet_hash(&link).context("link magnet sem infohash")?;
        (NewTorrent::Magnet(link), hash)
    } else {
        let bytes = catalog
            .download(&release.indexer, &release.download_url)
            .await
            .map_err(|error| anyhow::anyhow!("baixando o .torrent: {error}"))?;
        let hash = info_hash(&bytes).context("o indexador não devolveu um .torrent válido")?;
        (NewTorrent::File(bytes), hash)
    };
    let client = qbit(config).await?;
    client
        .ensure_category(&config.movies.category)
        .await
        .context("criando a categoria no qBittorrent")?;
    client
        .add(
            torrent,
            &AddOptions {
                category: config.movies.category.clone(),
                save_path: None,
            },
        )
        .await
        .context("mandando o torrent ao qBittorrent")?;
    store
        .record_grab(&Grab {
            id: 0,
            movie_id,
            hash,
            title: release.title.clone(),
            indexer: release.indexer.clone(),
            quality,
            size: release.size,
            grabbed_at: now_rfc3339(),
            state: GrabState::Downloading,
            message: None,
            imported_path: None,
            finished_at: None,
        })
        .await
        .context("registrando o grab")?;
    report.aplicado = true;
    Ok(report)
}

/// Um download do acervo na importação.
#[derive(Debug, Serialize)]
pub struct ImportLine {
    pub filme: String,
    pub release: String,
    /// `baixando`, `importaria`, `importado` ou `falhou`.
    pub estado: &'static str,
    pub detalhe: Option<String>,
    /// Caminho do arquivo na pasta do filme, como o gerenciador vê.
    pub destino: Option<String>,
}

/// O arquivo principal do torrent: o maior vídeo que não é amostra.
fn main_video(files: &[acervo_clients::TorrentFile]) -> Option<&acervo_clients::TorrentFile> {
    files
        .iter()
        .filter(|file| {
            let path = Path::new(&file.name);
            let video = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()));
            let sample = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_ascii_lowercase().contains("sample"));
            video && !sample
        })
        .max_by_key(|file| file.size)
}

/// Liga `source` em `target`, criando a pasta. Ligação que já existe para o
/// mesmo arquivo não é erro; arquivo diferente no destino é.
fn link(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando `{}`", parent.display()))?;
    }
    let from =
        std::fs::metadata(source).with_context(|| format!("lendo `{}`", source.display()))?;
    match std::fs::metadata(target) {
        Ok(existing) if existing.dev() == from.dev() && existing.ino() == from.ino() => {
            return Ok(());
        }
        Ok(_) => bail!("já existe outro arquivo em `{}`", target.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("lendo `{}`", target.display()));
        }
    }
    std::fs::hard_link(source, target).map_err(|error| {
        if error.raw_os_error() == Some(18) {
            // EXDEV: download e biblioteca em discos diferentes. Copiar
            // dobraria o espaço sem avisar; melhor parar.
            anyhow::anyhow!("download e biblioteca estão em discos diferentes: hardlink impossível")
        } else {
            anyhow::Error::new(error).context(format!("ligando `{}`", target.display()))
        }
    })
}

/// Importa os downloads do acervo que terminaram. Sem `apply`, só diz o que
/// faria.
///
/// # Errors
///
/// Catálogo ilegível ou cliente inalcançável. Falha de um download fica na
/// linha dele; os outros seguem.
#[allow(clippy::too_many_lines)]
pub async fn import_downloads(
    config: &Config,
    store: &Store,
    apply: bool,
) -> Result<Vec<ImportLine>> {
    let pending: Vec<Grab> = store
        .grabs()
        .await?
        .into_iter()
        .filter(|g| g.state == GrabState::Downloading)
        .collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let client = qbit(config).await?;
    let movies = store.movies().await?;
    let map = config.path_map();
    let mut lines = Vec::new();
    for grab in pending {
        let Some(entry) = movies.iter().find(|m| m.id == grab.movie_id) else {
            continue;
        };
        let movie = &entry.movie;
        let filme = match movie.year {
            Some(year) => format!("{} ({year})", movie.title),
            None => movie.title.clone(),
        };
        let mut line = ImportLine {
            filme,
            release: grab.title.clone(),
            estado: "baixando",
            detalhe: None,
            destino: None,
        };
        let outcome: Result<Option<String>, String> = async {
            let torrent = client
                .torrent(&grab.hash)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("o torrent sumiu do cliente")?;
            if torrent.progress < 1.0 {
                line.detalhe = Some(format!("{:.0}%", torrent.progress * 100.0));
                return Ok(None);
            }
            if movie.file.is_some() {
                return Err("o filme ganhou arquivo por outro caminho".into());
            }
            let hash = acervo_core::DownloadHash::new(grab.hash.clone());
            let files = client.files(&hash).await.map_err(|e| e.to_string())?;
            let video = main_video(&files).ok_or("nenhum vídeo no torrent")?;
            let extension = Path::new(&video.name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mkv")
                .to_ascii_lowercase();
            let destination =
                PathBuf::from(&movie.path).join(format!("{}.{extension}", movie_file_stem(movie)));
            let source_host = map
                .to_host(&client_path(&torrent, video))
                .map_err(|e| e.to_string())?;
            let destination_host = map.to_host(&destination).map_err(|e| e.to_string())?;
            let shown = destination.display().to_string();
            if !apply {
                return Ok(Some(shown));
            }
            tokio::task::spawn_blocking(move || link(&source_host, &destination_host))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("{e:#}"))?;
            Ok(Some(shown))
        }
        .await;

        let at = now_rfc3339();
        match outcome {
            Ok(None) => {
                if apply {
                    store
                        .update_grab(
                            grab.id,
                            GrabState::Downloading,
                            line.detalhe.as_deref(),
                            None,
                            &at,
                        )
                        .await?;
                }
            }
            Ok(Some(destination)) if !apply => {
                line.estado = "importaria";
                line.destino = Some(destination);
            }
            Ok(Some(destination)) => {
                store
                    .update_grab(grab.id, GrabState::Imported, None, Some(&destination), &at)
                    .await?;
                line.estado = "importado";
                line.destino = Some(destination);
                // O gerenciador adota o arquivo e para de procurar o filme.
                if let Some((_, source_id)) = &entry.origin
                    && let Err(error) = async {
                        movie_client(config)?
                            .rescan_movie(*source_id)
                            .await
                            .map_err(anyhow::Error::from)
                    }
                    .await
                {
                    line.detalhe = Some(format!(
                        "importado, mas o gerenciador não releu a pasta: {error:#}"
                    ));
                }
            }
            Err(error) => {
                line.estado = "falhou";
                line.detalhe = Some(error.clone());
                if apply {
                    store
                        .update_grab(grab.id, GrabState::Failed, Some(&error), None, &at)
                        .await?;
                }
            }
        }
        lines.push(line);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> acervo_clients::TorrentFile {
        serde_json::from_value(serde_json::json!({ "name": name, "size": size })).unwrap()
    }

    #[test]
    fn video_principal_e_o_maior_que_nao_e_amostra() {
        let files = [
            file("Filme.2020/Sample/filme-sample.mkv", 50),
            file("Filme.2020/Filme.2020.1080p.mkv", 4_000),
            file("Filme.2020/Filme.2020.nfo", 1),
            file("Filme.2020/Extras/bastidores.mp4", 900),
        ];
        assert_eq!(
            main_video(&files).unwrap().name,
            "Filme.2020/Filme.2020.1080p.mkv"
        );
        assert!(main_video(&[file("x.nfo", 1), file("x-sample.mkv", 9)]).is_none());
    }

    #[test]
    fn ligar_cria_a_pasta_e_repetir_nao_e_erro() {
        let dir = std::env::temp_dir().join(format!("acervo-grab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("baixado.mkv");
        std::fs::write(&source, b"video").unwrap();
        let target = dir.join("Filme (2020)").join("Filme (2020).mkv");

        link(&source, &target).unwrap();
        link(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"video");

        let other = dir.join("outro.mkv");
        std::fs::write(&other, b"outro").unwrap();
        assert!(link(&other, &target).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

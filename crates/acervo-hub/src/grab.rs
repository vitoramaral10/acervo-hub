//! Grab e importação: o acervo pega o release que a decisão escolheu e, quando
//! o download termina, liga o arquivo na pasta do filme.
//!
//! O que é pego vai ao cliente numa categoria própria, que o gerenciador de
//! filmes não importa. A importação liga (hardlink) o arquivo baixado com o
//! nome que o gerenciador daria, na pasta do filme; num upgrade, troca o
//! antigo. Filme que ainda é do gerenciador é relido por ele depois.
//!
//! Download que o cliente dá como perdido vai para a lista de bloqueio e o
//! filme é buscado de novo (com a busca automática ligada). Problema na
//! importação (arquivo no caminho, disco diferente) não é culpa do release:
//! o download fica na fila, com o aviso, até dar certo.

use std::path::{Path, PathBuf};

use acervo_api::Catalog;
use acervo_clients::{AddOptions, NewTorrent, QbitClient, client_path, info_hash, magnet_hash};
use acervo_store::{Grab, GrabState, MovieFile, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::events::{self, Event, Kind};
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

pub(crate) async fn qbit(config: &Config) -> Result<QbitClient> {
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

/// Manda um release ao cliente e registra o grab. `replaces` é o arquivo que
/// o filme tem hoje, se tiver: a importação o troca pelo novo.
///
/// # Errors
///
/// `.torrent` inválido, cliente inalcançável ou falha ao registrar.
pub async fn send(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    release: &acervo_indexers::Release,
    quality: acervo_parser::Quality,
    replaces: Option<String>,
) -> Result<()> {
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
            hash: hash.clone(),
            title: release.title.clone(),
            indexer: release.indexer.clone(),
            quality,
            size: release.size,
            grabbed_at: now_rfc3339(),
            state: GrabState::Downloading,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces,
        })
        .await
        .context("registrando o grab")?;
    let movie = store.movies().await?.into_iter().find(|m| m.id == movie_id);
    events::record(
        store,
        Event {
            source_title: Some(release.title.clone()),
            quality: Some(quality),
            indexer: Some(release.indexer.clone()),
            download_id: Some(hash),
            poster: movie.as_ref().and_then(|m| m.extras.poster.clone()),
            ..Event::new(
                Kind::Grabbed,
                Some(movie_id),
                movie.map_or_else(String::new, |m| events::label(&m.movie.title, m.movie.year)),
            )
        },
    )
    .await;
    Ok(())
}

/// Manda um release escolhido na mão (busca interativa) ao cliente.
///
/// # Errors
///
/// Filme fora do catálogo, `.torrent` inválido ou cliente inalcançável.
pub async fn send_chosen(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    movie_id: i64,
    release: &acervo_indexers::Release,
) -> Result<()> {
    let replaces = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == movie_id)
        .context("filme fora do catálogo")?
        .movie
        .file
        .map(|f| f.relative_path);
    let quality = acervo_parser::parse_quality(&release.title).quality;
    send(config, store, catalog, movie_id, release, quality, replaces).await
}

/// Busca o filme, decide e, com `apply`, manda o escolhido ao cliente. Filme
/// com arquivo só é pego se a decisão aprovar como upgrade.
///
/// # Errors
///
/// Filme fora do catálogo, busca que falhou em todos os indexadores,
/// `.torrent` inválido, cliente inalcançável.
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
    let replaces = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == movie_id)
        .and_then(|m| m.movie.file.map(|f| f.relative_path));
    send(config, store, catalog, movie_id, release, quality, replaces).await?;
    report.aplicado = true;
    Ok(report)
}

/// Um download do acervo na importação.
#[derive(Debug, Serialize)]
pub struct ImportLine {
    pub filme: String,
    pub release: String,
    /// `baixando`, `importaria`, `importado`, `atencao` (importação
    /// travada, tenta de novo) ou `falhou`.
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

/// Põe o arquivo novo no lugar. Num upgrade com o mesmo nome, liga num nome
/// temporário e renomeia por cima do antigo (troca atômica); com nome
/// diferente, liga o novo e só então apaga o antigo.
fn install(source: &Path, target: &Path, old: Option<&Path>) -> Result<()> {
    match old {
        Some(old) if old == target => {
            let temporary = target.with_extension("acervo-novo");
            match std::fs::remove_file(&temporary) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error).context("limpando o temporário");
                }
                _ => {}
            }
            link(source, &temporary)?;
            std::fs::rename(&temporary, target)
                .with_context(|| format!("trocando `{}`", target.display()))
        }
        old => {
            link(source, target)?;
            if let Some(old) = old {
                match std::fs::remove_file(old) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(error)
                            .with_context(|| format!("apagando o antigo `{}`", old.display()));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

/// O registro do arquivo importado, com o que o nome do release diz e as
/// faixas que o `ffprobe` leu.
fn imported_file(
    grab: &Grab,
    relative_path: String,
    size: u64,
    probe: Option<crate::mediainfo::Probe>,
) -> MovieFile {
    let parsed = acervo_parser::parse_movie_title(&grab.title);
    let from_name: Vec<acervo_parser::Language> = acervo_parser::parse_languages(&grab.title)
        .into_iter()
        .filter(|l| *l != acervo_parser::Language::Unknown)
        .collect();
    let languages = probe
        .as_ref()
        .map(|p| p.audio_languages.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or(from_name);
    MovieFile {
        relative_path,
        size,
        quality: acervo_parser::parse_quality(&grab.title),
        languages: languages.into_iter().map(|l| l.name().to_owned()).collect(),
        release_group: acervo_parser::parse_release_group(&grab.title),
        edition: parsed.and_then(|p| p.edition),
        scene_name: Some(grab.title.clone()),
        date_added: Some(now_rfc3339()),
        id: None,
        media_info: probe.map(|p| p.media_info),
    }
}

/// Por que um download não importou.
enum Failure {
    /// O release é o problema: o cliente perdeu ou deu erro. Bloqueia e
    /// busca de novo.
    Download(String),
    /// A importação é o problema: tenta de novo na próxima rodada.
    Import(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Import(message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Import(message.to_owned())
    }
}

/// Desiste de um download: marca como falho, bloqueia o release se pedido
/// e registra o evento.
///
/// # Errors
///
/// Falha de escrita.
pub async fn give_up(
    store: &Store,
    grab: &Grab,
    movie: &acervo_store::CatalogMovie,
    reason: &str,
    blocklist: bool,
    kind: Kind,
) -> Result<()> {
    let at = now_rfc3339();
    store
        .update_grab(grab.id, GrabState::Failed, Some(reason), None, &at)
        .await?;
    if blocklist {
        store
            .block(&acervo_store::Blocked {
                id: 0,
                movie_id: Some(grab.movie_id),
                source_title: grab.title.clone(),
                indexer: Some(grab.indexer.clone()),
                quality: Some(grab.quality),
                size: Some(grab.size),
                hash: Some(grab.hash.clone()),
                at: at.clone(),
                message: Some(reason.to_owned()),
            })
            .await?;
    }
    events::record(
        store,
        Event {
            source_title: Some(grab.title.clone()),
            quality: Some(grab.quality),
            indexer: Some(grab.indexer.clone()),
            download_id: Some(grab.hash.clone()),
            message: Some(reason.to_owned()),
            poster: movie.extras.poster.clone(),
            ..Event::new(
                kind,
                Some(movie.id),
                events::label(&movie.movie.title, movie.movie.year),
            )
        },
    )
    .await;
    Ok(())
}

/// Busca o filme de novo depois de uma falha, se a busca automática estiver
/// ligada.
async fn search_again(config: &Config, store: &Store, catalog: Option<&Catalog>, movie_id: i64) {
    let Some(catalog) = catalog else {
        return;
    };
    if !crate::automatic::enabled(store).await.unwrap_or(false) {
        return;
    }
    match grab(config, store, catalog, movie_id, true).await {
        Ok(report) => tracing::info!(
            filme = report.filme,
            pegou = report.escolhido.as_ref().map(|p| p.titulo.as_str()),
            "nova busca depois de falha"
        ),
        Err(error) => tracing::info!(filme = movie_id, "nova busca depois de falha: {error:#}"),
    }
}

/// Tira um download da fila: apaga do cliente (com os arquivos) se pedido,
/// bloqueia o release se pedido e busca outro se pedido.
///
/// # Errors
///
/// Download desconhecido, cliente inalcançável ou falha de escrita.
pub async fn remove_download(
    config: &Config,
    store: &Store,
    catalog: &Catalog,
    grab_id: i64,
    remove_from_client: bool,
    blocklist: bool,
    search: bool,
) -> Result<()> {
    let grab = store
        .grabs()
        .await?
        .into_iter()
        .find(|g| g.id == grab_id)
        .context("download desconhecido")?;
    let movie = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == grab.movie_id)
        .context("o filme do download saiu do catálogo")?;
    if remove_from_client {
        qbit(config)
            .await?
            .delete(&[acervo_core::DownloadHash::new(grab.hash.clone())], true)
            .await
            .context("apagando do qBittorrent")?;
    }
    let (reason, kind) = if blocklist {
        ("marcado como falho na tela", Kind::Failed)
    } else {
        ("tirado da fila na tela", Kind::Ignored)
    };
    give_up(store, &grab, &movie, reason, blocklist, kind).await?;
    if search {
        match self::grab(config, store, catalog, movie.id, true).await {
            Ok(_) => {}
            Err(error) => tracing::info!(filme = movie.id, "nova busca: {error:#}"),
        }
    }
    Ok(())
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
    catalog: Option<&Catalog>,
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
        let outcome: Result<Option<(String, String, u64, PathBuf)>, Failure> = async {
            let torrent = client
                .torrent(&grab.hash)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| Failure::Download("o torrent sumiu do cliente".into()))?;
            if matches!(torrent.state.as_str(), "error" | "missingFiles") {
                return Err(Failure::Download(format!(
                    "o cliente marcou o torrent com `{}`",
                    torrent.state
                )));
            }
            if torrent.progress < 1.0 {
                line.detalhe = Some(format!("{:.0}%", torrent.progress * 100.0));
                return Ok(None);
            }
            // Arquivo que não é o que o grab ia trocar: alguém importou por
            // outro caminho no meio.
            let current = movie.file.as_ref().map(|f| f.relative_path.as_str());
            if current.is_some() && current != grab.replaces.as_deref() {
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
            let relative = format!(
                "{}.{extension}",
                movie_file_stem(movie, entry.extras.metadata_title.as_deref())
            );
            let destination = PathBuf::from(&movie.path).join(&relative);
            let old_host = match current {
                Some(old) => Some(
                    map.to_host(&PathBuf::from(&movie.path).join(old))
                        .map_err(|e| e.to_string())?,
                ),
                None => None,
            };
            let size = video.size;
            let source_host = map
                .to_host(&client_path(&torrent, video))
                .map_err(|e| e.to_string())?;
            let destination_host = map.to_host(&destination).map_err(|e| e.to_string())?;
            let shown = destination.display().to_string();
            if !apply {
                return Ok(Some((shown, relative, size, destination_host)));
            }
            let installed = destination_host.clone();
            tokio::task::spawn_blocking(move || {
                install(&source_host, &installed, old_host.as_deref())
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
            Ok(Some((shown, relative, size, destination_host)))
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
            Ok(Some((destination, _, _, _))) if !apply => {
                line.estado = "importaria";
                line.destino = Some(destination);
            }
            Ok(Some((destination, relative, size, host))) => {
                let probe = crate::mediainfo::probe(&host).await;
                store
                    .set_movie_file(entry.id, Some(&imported_file(&grab, relative, size, probe)))
                    .await?;
                store
                    .update_grab(grab.id, GrabState::Imported, None, Some(&destination), &at)
                    .await?;
                line.estado = "importado";
                line.destino = Some(destination.clone());
                events::record(
                    store,
                    Event {
                        source_title: Some(grab.title.clone()),
                        quality: Some(grab.quality),
                        indexer: Some(grab.indexer.clone()),
                        download_id: Some(grab.hash.clone()),
                        message: Some(destination),
                        poster: entry.extras.poster.clone(),
                        ..Event::new(
                            if grab.replaces.is_some() {
                                Kind::Upgraded
                            } else {
                                Kind::Imported
                            },
                            Some(entry.id),
                            events::label(&movie.title, movie.year),
                        )
                    },
                )
                .await;
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
            Err(Failure::Download(error)) => {
                line.estado = "falhou";
                line.detalhe = Some(error.clone());
                if apply {
                    give_up(store, &grab, entry, &error, true, Kind::Failed).await?;
                    search_again(config, store, catalog, entry.id).await;
                }
            }
            Err(Failure::Import(error)) => {
                line.estado = "atencao";
                line.detalhe = Some(error.clone());
                if apply {
                    let message = format!("importação: {error}");
                    store
                        .update_grab(grab.id, GrabState::Downloading, Some(&message), None, &at)
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
    fn upgrade_troca_o_arquivo_antigo() {
        let dir = std::env::temp_dir().join(format!("acervo-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Filme")).unwrap();
        let old = dir.join("Filme").join("Filme.mkv");
        std::fs::write(&old, b"antigo").unwrap();
        let new = dir.join("novo.mkv");
        std::fs::write(&new, b"novo").unwrap();
        // Mesmo nome: troca no lugar.
        install(&new, &old, Some(&old)).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"novo");
        assert!(!dir.join("Filme").join("Filme.acervo-novo").exists());
        // Nome diferente: liga o novo e apaga o antigo.
        let other = dir.join("outro.mp4");
        std::fs::write(&other, b"outro").unwrap();
        let target = dir.join("Filme").join("Filme.mp4");
        install(&other, &target, Some(&old)).unwrap();
        assert!(!old.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"outro");
        std::fs::remove_dir_all(dir).unwrap();
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

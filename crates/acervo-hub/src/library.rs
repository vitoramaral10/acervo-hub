//! O catálogo como dono dos filmes: status e disponibilidade calculados das
//! datas, metadados vindos do TMDB e filmes adicionados pelo próprio acervo.
//! Título, datas e status de todo filme são mantidos a partir do TMDB.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use acervo_core::DownloadHash;
use acervo_metadata::{MovieMetadata, Tmdb};
use acervo_parser::{Language, clean_movie_title};
use acervo_store::{CatalogMovie, Movie, MovieExtras, Store};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::events::{self, Event, Kind};
use crate::naming::formatted_name;

fn date(text: Option<&str>) -> Option<Date> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(text?.get(..10)?, &format).ok()
}

/// `announced`, `inCinemas` ou `released`, como o gerenciador calcula: saiu
/// em digital ou físico, ou está há 90 dias no cinema sem nenhum dos dois.
#[must_use]
pub fn status(movie: &Movie, today: Date) -> &'static str {
    let cinema = date(movie.in_cinemas.as_deref());
    let digital = date(movie.digital_release.as_deref());
    let physical = date(movie.physical_release.as_deref());
    if digital.is_some_and(|d| d <= today) || physical.is_some_and(|d| d <= today) {
        return "released";
    }
    match cinema {
        Some(c) if c <= today => {
            if digital.is_none() && physical.is_none() && c + Duration::days(90) <= today {
                "released"
            } else {
                "inCinemas"
            }
        }
        _ => "announced",
    }
}

/// Já passou do lançamento, mais `delay_days` de carência. Todo filme segue
/// a regra de "lançado": o primeiro lançamento digital ou físico, ou 90 dias
/// depois da estreia se não houver nenhum; sem data nenhuma, indisponível.
/// Não há mais disponibilidade mínima por filme, para não buscar o que ainda
/// não saiu.
#[must_use]
pub fn is_available(movie: &Movie, today: Date, delay_days: i64) -> bool {
    let cinema = date(movie.in_cinemas.as_deref());
    let digital = date(movie.digital_release.as_deref());
    let physical = date(movie.physical_release.as_deref());
    let when = match (digital, physical) {
        (Some(d), Some(p)) => Some(d.min(p)),
        (Some(d), None) => Some(d),
        (None, Some(p)) => Some(p),
        (None, None) => cinema.map(|c| c + Duration::days(90)),
    };
    when.is_some_and(|when| when + Duration::days(delay_days) <= today)
}

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// Passa para o filme o que veio da base de metadados, recalculando status
/// e disponibilidade. O que é escolha de quem usa (monitorado, pasta) fica
/// como está.
fn apply(movie: &mut Movie, meta: &MovieMetadata) {
    movie.title = meta
        .localized_title
        .clone()
        .unwrap_or_else(|| meta.title.clone());
    movie.original_title = Some(meta.original_title.clone());
    movie.original_language = Some(
        Language::from_iso639_1(&meta.original_language)
            .name()
            .to_owned(),
    );
    movie.imdb_id.clone_from(&meta.imdb_id);
    movie.year = meta.year;
    movie.runtime = meta.runtime;
    movie.in_cinemas.clone_from(&meta.in_cinemas);
    movie.digital_release.clone_from(&meta.digital_release);
    movie.physical_release.clone_from(&meta.physical_release);
    movie.overview.clone_from(&meta.overview);
    movie.clean_title = Some(clean_movie_title(&meta.title));
    movie.alternate_titles.clone_from(&meta.alternate_titles);
    let today = today();
    movie.status = Some(status(movie, today).to_owned());
    movie.available = is_available(movie, today, 0);
}

fn extras(meta: &MovieMetadata) -> MovieExtras {
    MovieExtras {
        metadata_title: Some(meta.title.clone()),
        poster: meta.poster.clone(),
        fanart: meta.fanart.clone(),
        refreshed_at: Some(now_rfc3339()),
    }
}

/// Resultado de uma atualização de metadados.
#[derive(Debug, Default, Serialize)]
pub struct RefreshReport {
    pub conferidos: usize,
    pub atualizados: Vec<String>,
    pub falhas: Vec<(String, String)>,
}

/// Atualiza os metadados dos filmes que não foram conferidos nas últimas
/// `stale_hours` horas (zero: todos).
///
/// # Errors
///
/// Catálogo ilegível. Falha num filme fica no relato; os outros seguem.
pub async fn refresh(store: &Store, tmdb: &Tmdb, stale_hours: i64) -> Result<RefreshReport> {
    let cutoff = OffsetDateTime::now_utc() - Duration::hours(stale_hours);
    let mut report = RefreshReport::default();
    for entry in store.movies().await? {
        let fresh = entry
            .extras
            .refreshed_at
            .as_deref()
            .and_then(|at| {
                OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok()
            })
            .is_some_and(|at| at > cutoff);
        if stale_hours > 0 && fresh {
            continue;
        }
        report.conferidos += 1;
        let label = entry.movie.title.clone();
        let meta = match tmdb.movie(entry.movie.tmdb_id).await {
            Ok(meta) => meta,
            Err(error) => {
                // O 404 é o caso comum (filme apagado ou fundido no TMDB):
                // dito em português, sem o código. O filme fica no catálogo;
                // remover é decisão de quem lê o relato.
                let message = match error {
                    acervo_metadata::MetadataError::NotFound(id) => {
                        format!("não existe mais no TMDB (id {id})")
                    }
                    other => other.to_string(),
                };
                report.falhas.push((label, message));
                continue;
            }
        };
        let mut movie = entry.movie.clone();
        apply(&mut movie, &meta);
        if movie != entry.movie {
            store.update_movie(entry.id, &movie).await?;
            report.atualizados.push(label);
        }
        store.set_extras(entry.id, &extras(&meta)).await?;
    }
    Ok(report)
}

/// O que é preciso para adicionar um filme.
#[derive(Debug, Clone)]
pub struct AddRequest {
    pub tmdb_id: u32,
    /// Pasta raiz, como o gerenciador a vê (`/media/movies`).
    pub root_folder: String,
    pub monitored: bool,
}

/// Um filme novo, montado a partir do TMDB, sem gravar.
///
/// # Errors
///
/// Filme inexistente ou TMDB inalcançável.
pub async fn lookup(tmdb: &Tmdb, tmdb_id: u32) -> Result<(Movie, MovieExtras)> {
    let meta = tmdb
        .movie(tmdb_id)
        .await
        .with_context(|| format!("buscando o filme {tmdb_id} no TMDB"))?;
    let mut movie = Movie {
        tmdb_id,
        imdb_id: None,
        title: String::new(),
        original_title: None,
        original_language: None,
        year: None,
        status: None,
        monitored: false,
        path: String::new(),
        added: None,
        file: None,
        runtime: 0,
        secondary_year: None,
        clean_title: None,
        alternate_titles: Vec::new(),
        available: false,
        in_cinemas: None,
        digital_release: None,
        physical_release: None,
        overview: None,
    };
    apply(&mut movie, &meta);
    Ok((movie, extras(&meta)))
}

/// Adiciona o filme ao catálogo, como dono. Devolve o id.
///
/// # Errors
///
/// Filme já no catálogo, TMDB inalcançável ou falha de escrita.
pub async fn add(store: &Store, tmdb: &Tmdb, request: &AddRequest) -> Result<i64> {
    if store
        .movies()
        .await?
        .iter()
        .any(|m| m.movie.tmdb_id == request.tmdb_id)
    {
        bail!("o filme {} já está no catálogo", request.tmdb_id);
    }
    let (mut movie, extras) = lookup(tmdb, request.tmdb_id).await?;
    let folder = formatted_name(
        extras.metadata_title.as_deref().unwrap_or(&movie.title),
        movie.year,
        movie.imdb_id.as_deref(),
    );
    movie.path = format!("{}/{folder}", request.root_folder.trim_end_matches('/'));
    movie.monitored = request.monitored;
    movie.added = Some(now_rfc3339());
    movie.available = is_available(&movie, today(), 0);
    let id = store.add_movie(&movie, &extras).await?;
    events::record(
        store,
        Event {
            poster: extras.poster.clone(),
            ..Event::new(
                Kind::MovieAdded,
                Some(id),
                events::label(&movie.title, movie.year),
            )
        },
    )
    .await;
    Ok(id)
}

/// O que se muda num filme. Campo ausente fica como está.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MovieEdit {
    #[serde(default, rename = "monitorado")]
    pub monitored: Option<bool>,
}

/// Muda um filme.
///
/// # Errors
///
/// Filme desconhecido ou falha de escrita.
pub async fn edit(store: &Store, id: i64, change: &MovieEdit) -> Result<CatalogMovie> {
    let entry = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == id)
        .context("filme fora do catálogo")?;
    let mut movie = entry.movie.clone();
    if let Some(monitored) = change.monitored {
        movie.monitored = monitored;
    }
    movie.available = is_available(&movie, today(), 0);
    store.update_movie(id, &movie).await?;
    Ok(CatalogMovie { movie, ..entry })
}

/// Apaga a pasta de um filme. Ela tem de estar direto numa pasta raiz, e não
/// ser a própria raiz.
///
/// # Errors
///
/// Pasta fora das raízes ou falha ao apagar.
pub async fn delete_folder(config: &Config, movie_path: &str) -> Result<()> {
    let folder = Path::new(movie_path);
    let inside_root = config.library.root_folders.iter().any(|root| {
        folder
            .parent()
            .is_some_and(|p| p == Path::new(root.trim_end_matches('/')))
    });
    if !inside_root {
        bail!("a pasta `{movie_path}` não está direto numa pasta raiz; nada apagado");
    }
    let host = config.path_map().to_host(folder)?;
    tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&host) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    })
    .await??;
    Ok(())
}

fn removal_message(folder: &str, removed: &Result<Vec<String>, String>) -> String {
    match removed {
        Ok(names) if names.is_empty() => {
            format!("pasta apagada: {folder}; nenhum download no cliente")
        }
        Ok(names) => format!(
            "pasta apagada: {folder}; download apagado com os dados: {}",
            names.join(", ")
        ),
        Err(error) => format!(
            "pasta apagada: {folder}; download não apagado ({error}), fica para o ciclo de limpeza"
        ),
    }
}

/// Torrents que semeiam os arquivos da pasta do filme. O arquivo da
/// biblioteca é hardlink do download, então o mesmo inode aparece nos dois
/// lados — vale para o que o Radarr baixou e para o que o acervo baixou, e
/// não depende de nome nem de histórico.
async fn downloads_of(config: &Config, movie_path: &str) -> Result<Vec<(DownloadHash, String)>> {
    let map = config.path_map();
    let folder = map.to_host(Path::new(movie_path))?;
    let inodes = tokio::task::spawn_blocking(move || linked_inodes(&folder)).await??;
    if inodes.is_empty() {
        return Ok(Vec::new());
    }
    let qbit = crate::grab::qbit(config).await?;
    let mut candidates: Vec<(DownloadHash, String, Vec<PathBuf>)> = Vec::new();
    for torrent in qbit.torrents().await.context("listando os torrents")? {
        let hash = DownloadHash::new(&torrent.hash);
        let files = qbit
            .files(&hash)
            .await
            .context("listando os arquivos de um torrent")?;
        // Caminho fora do mapa não é deste acervo: não pode ser o filme.
        let paths = files
            .iter()
            .filter_map(|file| {
                map.to_host(&acervo_clients::client_path(&torrent, file))
                    .ok()
            })
            .collect();
        candidates.push((hash, torrent.name, paths));
    }
    Ok(tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|(_, _, paths)| {
                paths.iter().any(|path| {
                    std::fs::metadata(path)
                        .is_ok_and(|meta| inodes.contains(&(meta.dev(), meta.ino())))
                })
            })
            .map(|(hash, name, _)| (hash, name))
            .collect()
    })
    .await?)
}

/// Inodes dos arquivos da pasta que têm outro link — só esses podem ser de
/// um download. Pasta inexistente não tem nenhum.
fn linked_inodes(folder: &Path) -> std::io::Result<HashSet<(u64, u64)>> {
    let mut inodes = HashSet::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            other => other?,
        };
        for entry in entries {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file() && meta.nlink() > 1 {
                inodes.insert((meta.dev(), meta.ino()));
            }
        }
    }
    Ok(inodes)
}

async fn delete_downloads(
    config: &Config,
    found: Vec<(DownloadHash, String)>,
) -> Result<Vec<String>, String> {
    let (hashes, names): (Vec<_>, Vec<_>) = found.into_iter().unzip();
    let qbit = crate::grab::qbit(config)
        .await
        .map_err(|error| format!("{error:#}"))?;
    qbit.delete(&hashes, true)
        .await
        .map_err(|error| format!("apagando no qBittorrent: {error}"))?;
    Ok(names)
}

/// Apaga só o arquivo do filme; o filme fica no catálogo, sem arquivo (e,
/// monitorado, volta a ser procurado).
///
/// # Errors
///
/// Filme sem arquivo ou falha ao apagar.
pub async fn delete_file(config: &Config, store: &Store, id: i64) -> Result<()> {
    let entry = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == id)
        .context("filme fora do catálogo")?;
    let file = entry
        .movie
        .file
        .clone()
        .context("o filme não tem arquivo")?;
    let path = Path::new(&entry.movie.path).join(&file.relative_path);
    let host = config.path_map().to_host(&path)?;
    tokio::task::spawn_blocking(move || match std::fs::remove_file(&host) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    })
    .await??;
    store.set_movie_file(id, None).await?;
    events::record(
        store,
        Event {
            source_title: file.scene_name.clone(),
            quality: Some(file.quality.quality),
            message: Some(file.relative_path.clone()),
            poster: entry.extras.poster.clone(),
            ..Event::new(
                Kind::FileDeleted,
                Some(id),
                events::label(&entry.movie.title, entry.movie.year),
            )
        },
    )
    .await;
    Ok(())
}

/// Tira o filme do catálogo.
///
/// Com `delete_files`, apaga a pasta **e** o download no qBittorrent, com os
/// dados, na hora — privado ou não, por escolha do usuário. Sem isso, o
/// torrent seguiria semeando até o ciclo de limpeza vencer a carência. Se o
/// cliente falhar, o filme sai mesmo assim e o ciclo limpa depois.
///
/// # Errors
///
/// Filme desconhecido, pasta fora das raízes ou falha de escrita.
pub async fn remove(config: &Config, store: &Store, id: i64, delete_files: bool) -> Result<()> {
    remove_because(config, store, id, delete_files, delete_files, None).await
}

/// [`remove`], com o motivo à frente da mensagem do evento no histórico —
/// para quem remove sozinho dizer por quê. Sem `delete_seed`, a pasta
/// sai mas o torrent fica semeando até o ciclo de limpeza: quem remove
/// sozinho não pode pular a carência de tracker privado.
///
/// # Errors
///
/// Os de [`remove`].
pub async fn remove_because(
    config: &Config,
    store: &Store,
    id: i64,
    delete_files: bool,
    delete_seed: bool,
    reason: Option<&str>,
) -> Result<()> {
    let entry = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == id)
        .context("filme fora do catálogo")?;
    // Antes de apagar a pasta: depois dela, não há mais inode para casar.
    let downloads = if delete_files && delete_seed {
        downloads_of(config, &entry.movie.path)
            .await
            .map_err(|error| format!("{error:#}"))
    } else {
        Ok(Vec::new())
    };
    if delete_files {
        delete_folder(config, &entry.movie.path).await?;
    }
    store.delete_movie(id).await?;
    let removed = match downloads {
        Ok(found) if found.is_empty() => Ok(Vec::new()),
        Ok(found) => delete_downloads(config, found).await,
        Err(error) => Err(error),
    };
    if let Err(error) = &removed {
        tracing::warn!(
            filme = entry.movie.title,
            %error,
            "download não apagado; fica para o ciclo de limpeza"
        );
    }
    tracing::info!(
        filme = entry.movie.title,
        arquivos = delete_files,
        downloads = removed.as_ref().map_or(0, Vec::len),
        "filme removido"
    );
    events::record(
        store,
        Event {
            message: match (
                reason,
                delete_files.then(|| {
                    if delete_seed {
                        removal_message(&entry.movie.path, &removed)
                    } else {
                        format!(
                            "pasta apagada: {}; o download segue semeando até o ciclo de limpeza",
                            entry.movie.path
                        )
                    }
                }),
            ) {
                (Some(reason), Some(message)) => Some(format!("{reason}; {message}")),
                (Some(reason), None) => Some(reason.to_owned()),
                (None, message) => message,
            },
            poster: entry.extras.poster.clone(),
            ..Event::new(
                Kind::MovieDeleted,
                None,
                events::label(&entry.movie.title, entry.movie.year),
            )
        },
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn filme_sumido_do_tmdb_vira_falha_legivel_e_fica_no_catalogo() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let Some(db) = acervo_store::testing::TestDb::new("refresh_404").await else {
            return;
        };
        let mut gone = movie(None, None, None);
        gone.tmdb_id = 1_553_031;
        gone.title = "Untitled Cancelled Film".into();
        db.store
            .add_movie(&gone, &acervo_store::MovieExtras::default())
            .await
            .unwrap();
        let server = MockServer::start().await;
        Mock::given(path("/3/movie/1553031"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "status_message": "The resource you requested could not be found."
            })))
            .mount(&server)
            .await;
        let tmdb = Tmdb::with_base(
            &format!("{}/3/", server.uri()),
            "chave",
            "pt-BR",
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let report = refresh(&db.store, &tmdb, 0).await.unwrap();
        assert_eq!(
            report.falhas,
            [(
                "Untitled Cancelled Film".to_owned(),
                "não existe mais no TMDB (id 1553031)".to_owned()
            )]
        );
        assert_eq!(db.store.movies().await.unwrap().len(), 1);
        db.drop().await;
    }

    #[test]
    fn so_arquivo_com_outro_link_conta_como_download() {
        let root = std::env::temp_dir().join(format!("acervo-inodes-{}", std::process::id()));
        let movie = root.join("filme");
        let download = root.join("download");
        std::fs::create_dir_all(movie.join("extras")).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        std::fs::write(download.join("filme.mkv"), b"video").unwrap();
        std::fs::hard_link(download.join("filme.mkv"), movie.join("filme.mkv")).unwrap();
        std::fs::write(movie.join("extras/poster.jpg"), b"sozinho").unwrap();

        let inodes = linked_inodes(&movie).unwrap();
        let meta = std::fs::metadata(download.join("filme.mkv")).unwrap();
        assert_eq!(inodes, HashSet::from([(meta.dev(), meta.ino())]));
        assert!(linked_inodes(&root.join("nao-existe")).unwrap().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn movie(cinema: Option<&str>, digital: Option<&str>, physical: Option<&str>) -> Movie {
        Movie {
            tmdb_id: 1,
            imdb_id: None,
            title: "Filme".into(),
            original_title: None,
            original_language: None,
            year: None,
            status: None,
            monitored: true,
            path: "/filmes/Filme".into(),
            added: None,
            file: None,
            runtime: 0,
            secondary_year: None,
            clean_title: None,
            alternate_titles: Vec::new(),
            available: false,
            in_cinemas: cinema.map(str::to_owned),
            digital_release: digital.map(str::to_owned),
            physical_release: physical.map(str::to_owned),
            overview: None,
        }
    }

    fn day(text: &str) -> Date {
        date(Some(text)).unwrap()
    }

    #[test]
    fn status_como_a_referencia() {
        let today = day("2026-09-25");
        assert_eq!(status(&movie(None, None, None), today), "announced");
        assert_eq!(
            status(&movie(Some("2026-10-01"), None, None), today),
            "announced"
        );
        assert_eq!(
            status(&movie(Some("2026-09-01"), None, None), today),
            "inCinemas"
        );
        // 90 dias de cinema sem digital nem físico contam como lançado.
        assert_eq!(
            status(&movie(Some("2026-06-01"), None, None), today),
            "released"
        );
        assert_eq!(
            status(&movie(Some("2026-09-01"), Some("2026-10-30"), None), today),
            "inCinemas"
        );
        assert_eq!(
            status(&movie(None, Some("2026-09-20"), None), today),
            "released"
        );
    }

    #[test]
    fn disponibilidade_so_pela_regra_de_lancado() {
        let today = day("2026-09-25");
        // Só anunciado, sem data nenhuma: indisponível.
        assert!(!is_available(&movie(None, None, None), today, 0));
        // Cinema recente, sem digital nem físico: ainda não.
        assert!(!is_available(
            &movie(Some("2026-08-26"), None, None),
            today,
            0
        ));
        assert!(!is_available(
            &movie(Some("2026-09-20"), None, None),
            today,
            0
        ));
        // Cinema há mais de 90 dias: lançado.
        assert!(is_available(
            &movie(Some("2026-06-17"), None, None),
            today,
            0
        ));
        // Digital passada vale; a menor entre digital e física manda.
        assert!(is_available(
            &movie(None, Some("2026-09-24"), None),
            today,
            0
        ));
        assert!(is_available(
            &movie(Some("2026-01-01"), None, Some("2026-09-24")),
            today,
            0
        ));
        assert!(is_available(
            &movie(None, Some("2026-09-24"), Some("2026-12-01")),
            today,
            0
        ));
        // Digital futura não vale, mesmo com cinema antigo.
        assert!(!is_available(
            &movie(Some("2026-01-01"), Some("2026-10-30"), None),
            today,
            0
        ));
        // A carência empurra a data.
        assert!(!is_available(
            &movie(None, Some("2026-09-24"), None),
            today,
            2
        ));
        assert!(is_available(
            &movie(None, Some("2026-09-23"), None),
            today,
            2
        ));
    }
}

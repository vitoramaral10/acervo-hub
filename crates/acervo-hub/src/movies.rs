//! Catálogo de filmes: a visão da tela e a conferência de cada arquivo
//! contra o disco.

use std::collections::HashMap;
use std::path::Path;

use acervo_store::{CatalogMovie, Store};
use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;

/// Estado do arquivo no disco, visto daqui.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disk {
    Ok,
    /// O catálogo diz que existe e o disco não tem.
    Missing,
    /// Existe, com outro tamanho: foi trocado ou está incompleto.
    SizeDiffers,
    /// Não deu para olhar: caminho fora do mapa ou erro de leitura.
    Unreadable,
}

#[derive(Debug, Serialize)]
pub struct FileView {
    pub nome: String,
    pub tamanho: u64,
    pub qualidade: &'static str,
    pub idiomas: Vec<String>,
    pub grupo: Option<String>,
    pub edicao: Option<String>,
    pub release: Option<String>,
    pub adicionado: Option<String>,
    pub disco: Disk,
    pub disco_detalhe: Option<String>,
}

/// A última busca do filme.
#[derive(Debug, Serialize)]
pub struct LastSearchView {
    pub quando: String,
    pub releases: usize,
    pub escolhido: Option<String>,
    pub qualidade: Option<&'static str>,
    pub motivos: Vec<(String, usize)>,
    pub erro: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MovieView {
    pub id: i64,
    pub tmdb: u32,
    pub imdb: Option<String>,
    pub titulo: String,
    pub titulo_original: Option<String>,
    pub ano: Option<u16>,
    /// Pôster no tamanho de grade, direto do TMDB.
    pub poster: Option<String>,
    pub sinopse: Option<String>,
    pub status: Option<String>,
    pub monitorado: bool,
    pub perfil: Option<String>,
    pub tags: Vec<i64>,
    /// O filme ainda é do gerenciador (veio da importação).
    pub do_radarr: bool,
    pub pasta: String,
    pub adicionado: Option<String>,
    pub arquivo: Option<FileView>,
    pub ultima_busca: Option<LastSearchView>,
    /// O download mais recente que o acervo pegou para o filme.
    pub download: Option<DownloadView>,
}

#[derive(Debug, Serialize)]
pub struct DownloadView {
    pub estado: acervo_store::GrabState,
    pub release: String,
    pub mensagem: Option<String>,
    pub pego_em: String,
}

fn disk(path: &Path, expected: u64, map: &acervo_fs::PathMap) -> (Disk, Option<String>) {
    match acervo_fs::facts_for(path, map) {
        Ok(facts) if facts.apparent.as_u64() == expected => (Disk::Ok, None),
        Ok(facts) => (
            Disk::SizeDiffers,
            Some(format!(
                "{} bytes no disco, {expected} no catálogo",
                facts.apparent.as_u64()
            )),
        ),
        Err(acervo_fs::FsError::Stat { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            (Disk::Missing, None)
        }
        Err(error) => (Disk::Unreadable, Some(error.to_string())),
    }
}

fn view(
    entry: CatalogMovie,
    map: &acervo_fs::PathMap,
    search: Option<acervo_store::SearchRun>,
    download: Option<DownloadView>,
) -> MovieView {
    let movie = entry.movie;
    let arquivo = movie.file.map(|file| {
        let full = Path::new(&movie.path).join(&file.relative_path);
        let (disco, disco_detalhe) = disk(&full, file.size, map);
        FileView {
            nome: file.relative_path,
            tamanho: file.size,
            qualidade: file.quality.quality.name(),
            idiomas: file.languages,
            grupo: file.release_group,
            edicao: file.edition,
            release: file.scene_name,
            adicionado: file.date_added,
            disco,
            disco_detalhe,
        }
    });
    MovieView {
        id: entry.id,
        tmdb: movie.tmdb_id,
        imdb: movie.imdb_id,
        titulo: movie.title,
        titulo_original: movie.original_title,
        ano: movie.year,
        poster: entry
            .extras
            .poster
            .map(|url| url.replacen("/t/p/original/", "/t/p/w342/", 1)),
        sinopse: movie.overview,
        status: movie.status,
        monitorado: movie.monitored,
        perfil: movie.quality_profile,
        tags: movie.tags,
        do_radarr: entry.origin.is_some(),
        pasta: movie.path,
        adicionado: movie.added,
        arquivo,
        ultima_busca: search.map(|run| LastSearchView {
            quando: run.at,
            releases: run.releases,
            qualidade: run.pick.as_ref().map(|p| p.quality.name()),
            escolhido: run.pick.map(|p| p.title),
            motivos: run.rejections,
            erro: run.error,
        }),
        download,
    }
}

/// O catálogo inteiro, com o estado de cada arquivo no disco.
///
/// # Errors
///
/// Catálogo ilegível.
pub async fn list(config: &Config, store: &Store) -> Result<Vec<MovieView>> {
    let map = config.path_map();
    let mut searches: HashMap<i64, acervo_store::SearchRun> = store
        .latest_searches()
        .await?
        .into_iter()
        .map(|run| (run.movie_id, run))
        .collect();
    let movies = store.movies().await?;
    let mut downloads: HashMap<i64, DownloadView> = HashMap::new();
    // Do mais novo ao mais velho: o primeiro de cada filme fica.
    for grab in store.grabs().await? {
        downloads.entry(grab.movie_id).or_insert(DownloadView {
            estado: grab.state,
            release: grab.title,
            mensagem: grab.message,
            pego_em: grab.grabbed_at,
        });
    }
    // O `stat` de cada arquivo é disco: fora do executor assíncrono.
    tokio::task::spawn_blocking(move || -> Result<Vec<MovieView>> {
        Ok(movies
            .into_iter()
            .map(|entry| {
                let search = searches.remove(&entry.id);
                let download = downloads.remove(&entry.id);
                view(entry, &map, search, download)
            })
            .collect())
    })
    .await
    .context("leitura interrompida")?
}

/// `movies check`: relata o que o disco não confirma. Devolve quantos.
///
/// # Errors
///
/// Catálogo ilegível.
pub async fn check(config: &Config, store: &Store) -> Result<usize> {
    let movies = list(config, store).await?;
    let with_file = movies.iter().filter(|m| m.arquivo.is_some()).count();
    let mut problems = 0;
    for movie in &movies {
        let Some(file) = &movie.arquivo else {
            continue;
        };
        if file.disco != Disk::Ok {
            problems += 1;
            println!(
                "  {:?}  {} ({}) — {}{}",
                file.disco,
                movie.titulo,
                movie.ano.map_or_else(String::new, |y| y.to_string()),
                file.nome,
                file.disco_detalhe
                    .as_ref()
                    .map_or_else(String::new, |d| format!(" [{d}]"))
            );
        }
    }
    println!(
        "{} filmes no catálogo, {with_file} com arquivo; {problems} arquivos que o disco não confirma",
        movies.len()
    );
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disco_confirma_falta_e_tamanho() {
        let dir = std::env::temp_dir().join(format!("acervo-filmes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("filme.mkv");
        std::fs::write(&file, b"12345").unwrap();
        let map = acervo_fs::PathMap::new([]);
        assert_eq!(disk(&file, 5, &map).0, Disk::Ok);
        assert_eq!(disk(&file, 6, &map).0, Disk::SizeDiffers);
        assert_eq!(disk(&dir.join("outro.mkv"), 5, &map).0, Disk::Missing);
        let elsewhere = acervo_fs::PathMap::new([("/nada".into(), "/nada".into())]);
        assert_eq!(disk(&file, 5, &elsewhere).0, Disk::Unreadable);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

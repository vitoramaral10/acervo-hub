//! Catálogo de filmes: espelho do gerenciador de filmes em produção e
//! conferência de cada arquivo contra o disco.
//!
//! Nesta etapa o gerenciador continua dono de tudo; o catálogo só copia o que
//! ele sabe, para as etapas seguintes decidirem em sombra ao lado dele. Como o
//! `sync`, a importação sem `--apply` só mostra o que mudaria.

use std::collections::HashMap;
use std::path::Path;

use acervo_arr::{ArrClient, ArrKind, RemoteMovie, RemoteProfileItem, RemoteQualityProfile};
use acervo_core::InstanceName;
use acervo_parser::{Quality, QualityModel, Revision};
use acervo_store::{
    CatalogMovie, Import, ImportSummary, Movie, MovieFile, ProfileItem, QualityDefinition,
    QualityProfile, Store,
};
use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{Config, InstanceKind};

#[derive(Debug, Serialize)]
pub struct InstanceImport {
    pub nome: String,
    pub erro: Option<String>,
    pub resumo: Option<ImportSummary>,
}

/// Espelha cada gerenciador de filmes da configuração no catálogo.
///
/// Instância que não responde fica de fora com o erro no relato; as outras
/// seguem. Nada no catálogo muda por causa de uma instância fora do ar.
///
/// # Errors
///
/// Falha de escrita no catálogo.
pub async fn import(
    config: &Config,
    store: &Store,
    apply: bool,
    print: bool,
) -> Result<Vec<InstanceImport>> {
    let mut report = Vec::new();
    for spec in config
        .instances
        .iter()
        .filter(|spec| matches!(spec.kind, InstanceKind::Movie))
    {
        let fetched = fetch(config, &spec.name, &spec.url, &spec.api_key).await;
        let entry = match fetched {
            Err(error) => InstanceImport {
                nome: spec.name.clone(),
                erro: Some(format!("{error:#}")),
                resumo: None,
            },
            Ok(import) => {
                let summary = store
                    .import(&import, apply)
                    .await
                    .context("gravando no catálogo")?;
                InstanceImport {
                    nome: spec.name.clone(),
                    erro: None,
                    resumo: Some(summary),
                }
            }
        };
        if print {
            print_import(&entry);
        }
        report.push(entry);
    }
    Ok(report)
}

fn print_import(entry: &InstanceImport) {
    println!("{}:", entry.nome);
    if let Some(error) = &entry.erro {
        println!("  fora do ar: {error}");
        return;
    }
    let Some(summary) = &entry.resumo else {
        return;
    };
    for (verb, names) in [
        ("criar", &summary.created),
        ("atualizar", &summary.updated),
        ("remover", &summary.removed),
    ] {
        for name in names {
            println!("  {verb:<9} {name}");
        }
    }
    println!(
        "  {} novos, {} atualizados, {} removidos, {} iguais; {} perfis{}",
        summary.created.len(),
        summary.updated.len(),
        summary.removed.len(),
        summary.unchanged,
        summary.profiles,
        if summary.applied {
            ""
        } else {
            " — simulação: rode com --apply para gravar"
        }
    );
}

async fn fetch(config: &Config, name: &str, url: &str, api_key: &str) -> Result<Import> {
    let client = ArrClient::new(
        InstanceName::new(name.to_owned()),
        url,
        api_key,
        ArrKind::Movie,
        config.http_timeout(),
    )?;
    let (profiles, movies, definitions) = tokio::try_join!(
        client.quality_profiles(),
        client.movies(),
        client.quality_definitions()
    )?;
    let names: HashMap<i64, String> = profiles.iter().map(|p| (p.id, p.name.clone())).collect();
    Ok(Import {
        source: format!("radarr:{name}"),
        profiles: profiles.iter().map(profile).collect(),
        definitions: definitions
            .iter()
            .filter_map(|d| {
                Some(QualityDefinition {
                    quality: Quality::from_id(d.quality.id)?,
                    min_size: d.min_size,
                    max_size: d.max_size,
                    preferred_size: d.preferred_size,
                })
            })
            .collect(),
        movies: movies
            .into_iter()
            .map(|remote| (remote.id, movie(remote, &names)))
            .collect(),
    })
}

fn quality_by_id(id: u8) -> Quality {
    Quality::from_id(id).unwrap_or(Quality::Unknown)
}

fn profile(remote: &RemoteQualityProfile) -> QualityProfile {
    fn item(remote: &RemoteProfileItem) -> ProfileItem {
        match &remote.quality {
            Some(quality) => ProfileItem {
                name: quality
                    .name
                    .clone()
                    .unwrap_or_else(|| quality_by_id(quality.id).name().to_owned()),
                qualities: vec![quality_by_id(quality.id)],
                allowed: remote.allowed,
            },
            None => ProfileItem {
                name: remote.name.clone().unwrap_or_default(),
                qualities: remote
                    .items
                    .iter()
                    .filter_map(|inner| inner.quality.as_ref().map(|q| quality_by_id(q.id)))
                    .collect(),
                allowed: remote.allowed,
            },
        }
    }
    // O corte aponta para o id de uma qualidade, ou para o de um grupo.
    let cutoff = remote.cutoff.and_then(|cutoff| {
        remote.items.iter().position(|item| {
            item.id == Some(cutoff)
                || item
                    .quality
                    .as_ref()
                    .is_some_and(|q| i64::from(q.id) == cutoff)
        })
    });
    QualityProfile {
        name: remote.name.clone(),
        upgrade_allowed: remote.upgrade_allowed,
        cutoff,
        language: remote.language.as_ref().map(|l| l.name.clone()),
        items: remote.items.iter().map(item).collect(),
        min_format_score: remote.min_format_score,
        cutoff_format_score: remote.cutoff_format_score,
    }
}

fn blank_is_none(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

fn movie(remote: RemoteMovie, profiles: &HashMap<i64, String>) -> Movie {
    let file = remote.movie_file.map(|file| MovieFile {
        relative_path: file.relative_path,
        size: file.size,
        quality: QualityModel {
            quality: file
                .quality
                .as_ref()
                .map_or(Quality::Unknown, |q| quality_by_id(q.quality.id)),
            revision: file
                .quality
                .as_ref()
                .and_then(|q| q.revision.as_ref())
                .map_or_else(Revision::default, |r| Revision {
                    version: r.version,
                    real: r.real,
                    is_repack: r.is_repack,
                }),
        },
        languages: file.languages.into_iter().map(|l| l.name).collect(),
        release_group: blank_is_none(file.release_group),
        edition: blank_is_none(file.edition),
        scene_name: blank_is_none(file.scene_name),
        date_added: file.date_added,
    });
    Movie {
        tmdb_id: remote.tmdb_id,
        imdb_id: blank_is_none(remote.imdb_id),
        title: remote.title,
        original_title: blank_is_none(remote.original_title),
        original_language: remote.original_language.map(|l| l.name),
        year: remote.year.filter(|year| *year != 0),
        status: remote.status,
        minimum_availability: remote.minimum_availability,
        monitored: remote.monitored,
        quality_profile: remote
            .quality_profile_id
            .and_then(|id| profiles.get(&id).cloned()),
        path: remote.path,
        added: remote.added,
        file,
        runtime: remote.runtime,
        secondary_year: remote.secondary_year.filter(|year| *year != 0),
        clean_title: blank_is_none(remote.clean_title),
        alternate_titles: remote
            .alternate_titles
            .into_iter()
            .map(|t| t.title)
            .collect(),
        available: remote.is_available,
    }
}

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

/// A última decisão em sombra do filme.
#[derive(Debug, Serialize)]
pub struct ShadowView {
    pub quando: String,
    pub releases: usize,
    pub pegaria: Option<String>,
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
    pub status: Option<String>,
    pub monitorado: bool,
    pub perfil: Option<String>,
    pub pasta: String,
    pub adicionado: Option<String>,
    pub arquivo: Option<FileView>,
    pub sombra: Option<ShadowView>,
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
    shadow: Option<acervo_store::ShadowRun>,
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
        status: movie.status,
        monitorado: movie.monitored,
        perfil: movie.quality_profile,
        pasta: movie.path,
        adicionado: movie.added,
        arquivo,
        sombra: shadow.map(|run| ShadowView {
            quando: run.at,
            releases: run.releases,
            qualidade: run.pick.as_ref().map(|p| p.quality.name()),
            pegaria: run.pick.map(|p| p.title),
            motivos: run.rejections,
            erro: run.error,
        }),
    }
}

/// O catálogo inteiro, com o estado de cada arquivo no disco.
///
/// # Errors
///
/// Catálogo ilegível.
pub async fn list(config: &Config, store: &Store) -> Result<Vec<MovieView>> {
    let map = config.path_map();
    let mut shadows: HashMap<i64, acervo_store::ShadowRun> = store
        .latest_shadow_runs()
        .await?
        .into_iter()
        .map(|run| (run.movie_id, run))
        .collect();
    let movies = store.movies().await?;
    // O `stat` de cada arquivo é disco: fora do executor assíncrono.
    tokio::task::spawn_blocking(move || -> Result<Vec<MovieView>> {
        Ok(movies
            .into_iter()
            .map(|entry| {
                let shadow = shadows.remove(&entry.id);
                view(entry, &map, shadow)
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

    fn remote_profile() -> RemoteQualityProfile {
        serde_json::from_str(
            r#"{"id": 1, "name": "Any", "upgradeAllowed": false, "cutoff": 1001,
                "language": {"id": -2, "name": "Original"},
                "items": [
                    {"quality": {"id": 1, "name": "SDTV"}, "items": [], "allowed": true},
                    {"id": 1001, "name": "WEB 1080p", "allowed": true, "items": [
                        {"quality": {"id": 3, "name": "WEBDL-1080p"}, "items": [], "allowed": true},
                        {"quality": {"id": 15, "name": "WEBRip-1080p"}, "items": [], "allowed": true}]},
                    {"quality": {"id": 7, "name": "Bluray-1080p"}, "items": [], "allowed": false}
                ]}"#,
        )
        .unwrap()
    }

    #[test]
    fn perfil_com_grupo_e_corte_no_grupo() {
        let converted = profile(&remote_profile());
        assert_eq!(converted.cutoff, Some(1));
        assert_eq!(
            converted.items[1].qualities,
            [Quality::WebDl1080p, Quality::WebRip1080p]
        );
        assert!(!converted.items[2].allowed);
        assert_eq!(converted.language.as_deref(), Some("Original"));
    }

    #[test]
    fn campo_vazio_vira_ausente() {
        let remote: RemoteMovie = serde_json::from_str(
            r#"{"id": 7, "tmdbId": 42, "title": "Filme", "path": "/media/movies/Filme (2020)",
                "imdbId": "", "year": 0, "qualityProfileId": 1,
                "movieFile": {"relativePath": "Filme (2020).mkv", "size": 10, "edition": "",
                    "releaseGroup": "", "languages": [{"id": 30, "name": "Portuguese (Brazil)"}],
                    "quality": {"quality": {"id": 3}, "revision": {"version": 2, "real": 0, "isRepack": true}}}}"#,
        )
        .unwrap();
        let converted = movie(remote, &HashMap::from([(1, "Any".to_owned())]));
        assert_eq!(converted.imdb_id, None);
        assert_eq!(converted.year, None);
        assert_eq!(converted.quality_profile.as_deref(), Some("Any"));
        let file = converted.file.unwrap();
        assert_eq!(file.edition, None);
        assert_eq!(file.release_group, None);
        assert_eq!(file.quality.quality, Quality::WebDl1080p);
        assert!(file.quality.revision.is_repack);
    }

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

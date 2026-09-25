//! O catálogo como dono dos filmes: status e disponibilidade calculados das
//! datas, metadados vindos do TMDB e filmes adicionados pelo próprio acervo.
//!
//! Filme importado do gerenciador (com origem) continua sendo dele: aqui só
//! se completa o que ele não expõe (título em inglês, pôster). Filme sem
//! origem — adicionado aqui ou adotado no corte — tem título, datas e status
//! mantidos a partir do TMDB.

use std::path::Path;

use acervo_metadata::{MovieMetadata, Tmdb};
use acervo_parser::{Language, clean_movie_title};
use acervo_store::{CatalogMovie, Movie, MovieExtras, Store};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Date, Duration, OffsetDateTime};

use crate::config::Config;
use crate::events::{self, Event, Kind};
use crate::naming::formatted_name;
use crate::shadow::{movie_client, now_rfc3339};

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

/// Já passou da disponibilidade mínima, mais `delay_days` de carência. A
/// regra da referência: "anunciado" vale sempre; "no cinema" vale da
/// estreia; "lançado" vale do primeiro lançamento digital ou físico, ou de
/// 90 dias depois da estreia se não houver nenhum.
#[must_use]
pub fn is_available(movie: &Movie, today: Date, delay_days: i64) -> bool {
    let cinema = date(movie.in_cinemas.as_deref());
    let when = match movie.minimum_availability.as_deref() {
        Some("tba" | "announced") | None => return true,
        Some("inCinemas") if cinema.is_some() => cinema,
        _ => {
            let digital = date(movie.digital_release.as_deref());
            let physical = date(movie.physical_release.as_deref());
            match (digital, physical) {
                (Some(d), Some(p)) => Some(d.min(p)),
                (Some(d), None) => Some(d),
                (None, Some(p)) => Some(p),
                (None, None) => cinema.map(|c| c + Duration::days(90)),
            }
        }
    };
    when.is_some_and(|when| when + Duration::days(delay_days) <= today)
}

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// Passa para o filme o que veio da base de metadados, recalculando status
/// e disponibilidade. O que é escolha de quem usa (monitorado, perfil,
/// pasta, disponibilidade mínima, tags) fica como está.
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
                report.falhas.push((label, error.to_string()));
                continue;
            }
        };
        if entry.origin.is_none() {
            let mut movie = entry.movie.clone();
            apply(&mut movie, &meta);
            if movie != entry.movie {
                store.update_movie(entry.id, &movie).await?;
                report.atualizados.push(label);
            }
        }
        store.set_extras(entry.id, &extras(&meta)).await?;
    }
    Ok(report)
}

/// O que é preciso para adicionar um filme.
#[derive(Debug, Clone)]
pub struct AddRequest {
    pub tmdb_id: u32,
    /// Nome do perfil de qualidade.
    pub quality_profile: String,
    /// Pasta raiz, como o gerenciador a vê (`/media/movies`).
    pub root_folder: String,
    pub monitored: bool,
    pub minimum_availability: String,
    pub tags: Vec<i64>,
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
        minimum_availability: Some("released".into()),
        monitored: false,
        quality_profile: None,
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
        tags: Vec::new(),
    };
    apply(&mut movie, &meta);
    Ok((movie, extras(&meta)))
}

/// Adiciona o filme ao catálogo, como dono. Devolve o id.
///
/// # Errors
///
/// Filme já no catálogo, perfil desconhecido, TMDB inalcançável ou falha de
/// escrita.
pub async fn add(store: &Store, tmdb: &Tmdb, request: &AddRequest) -> Result<i64> {
    if store
        .movies()
        .await?
        .iter()
        .any(|m| m.movie.tmdb_id == request.tmdb_id)
    {
        bail!("o filme {} já está no catálogo", request.tmdb_id);
    }
    if !store
        .profiles()
        .await?
        .iter()
        .any(|p| p.name == request.quality_profile)
    {
        bail!(
            "perfil de qualidade `{}` não existe",
            request.quality_profile
        );
    }
    let (mut movie, extras) = lookup(tmdb, request.tmdb_id).await?;
    let folder = formatted_name(
        extras.metadata_title.as_deref().unwrap_or(&movie.title),
        movie.year,
        movie.imdb_id.as_deref(),
    );
    movie.path = format!("{}/{folder}", request.root_folder.trim_end_matches('/'));
    movie.quality_profile = Some(request.quality_profile.clone());
    movie.monitored = request.monitored;
    movie.minimum_availability = Some(request.minimum_availability.clone());
    movie.tags.clone_from(&request.tags);
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

/// Adiciona onde as decisões estão sendo tomadas: no gerenciador, enquanto
/// ele for o dono das regras (e aí a importação traz o filme para cá), ou
/// direto no catálogo. Devolve o id no catálogo.
///
/// # Errors
///
/// Filme já no catálogo ou excluído, perfil desconhecido, TMDB ou
/// gerenciador inalcançável.
pub async fn add_anywhere(
    config: &Config,
    store: &Store,
    tmdb: &Tmdb,
    request: &AddRequest,
    search: bool,
) -> Result<i64> {
    if crate::rules::owner(config, store).await? != "radarr" {
        return add(store, tmdb, request).await;
    }
    if store
        .movies()
        .await?
        .iter()
        .any(|m| m.movie.tmdb_id == request.tmdb_id)
    {
        bail!("o filme {} já está no catálogo", request.tmdb_id);
    }
    let profile = store
        .profiles()
        .await?
        .into_iter()
        .find(|p| p.name == request.quality_profile)
        .with_context(|| {
            format!(
                "perfil de qualidade `{}` não existe",
                request.quality_profile
            )
        })?;
    let source_profile = profile.source_id.context(
        "o perfil não existe no Radarr; perfis novos só valem depois que o acervo assume",
    )?;
    let (movie, _) = lookup(tmdb, request.tmdb_id).await?;
    let client = movie_client(config)?;
    client
        .post_json(
            "api/v3/movie",
            &json!({
                "tmdbId": request.tmdb_id,
                "title": movie.title,
                "year": movie.year,
                "qualityProfileId": source_profile,
                "rootFolderPath": request.root_folder,
                "monitored": request.monitored,
                "minimumAvailability": request.minimum_availability,
                "tags": request.tags,
                "addOptions": { "searchForMovie": search },
            }),
        )
        .await
        .context("adicionando o filme no gerenciador")?;
    crate::movies::import(config, store, true, false).await?;
    let entry = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.movie.tmdb_id == request.tmdb_id)
        .context("o gerenciador aceitou o filme, mas a importação não o trouxe")?;
    events::record(
        store,
        Event {
            poster: entry.extras.poster.clone(),
            message: Some("adicionado no gerenciador".into()),
            ..Event::new(
                Kind::MovieAdded,
                Some(entry.id),
                events::label(&entry.movie.title, entry.movie.year),
            )
        },
    )
    .await;
    Ok(entry.id)
}

/// O que se muda num filme. Campo ausente fica como está.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MovieEdit {
    #[serde(default, rename = "monitorado")]
    pub monitored: Option<bool>,
    /// Nome do perfil.
    #[serde(default, rename = "perfil")]
    pub quality_profile: Option<String>,
    #[serde(default, rename = "disponibilidade_minima")]
    pub minimum_availability: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<i64>>,
}

const AVAILABILITIES: &[&str] = &["announced", "inCinemas", "released"];

/// Muda um filme. Filme que ainda é do gerenciador muda lá primeiro — senão a
/// próxima importação desfaria a mudança.
///
/// # Errors
///
/// Filme ou perfil desconhecido, disponibilidade inválida, gerenciador que
/// recusa ou falha de escrita.
pub async fn edit(
    config: &Config,
    store: &Store,
    id: i64,
    change: &MovieEdit,
) -> Result<CatalogMovie> {
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
    let profiles = store.profiles().await?;
    if let Some(name) = &change.quality_profile {
        if !profiles.iter().any(|p| p.name == *name) {
            bail!("perfil de qualidade `{name}` não existe");
        }
        movie.quality_profile = Some(name.clone());
    }
    if let Some(minimum) = &change.minimum_availability {
        if !AVAILABILITIES.contains(&minimum.as_str()) {
            bail!("disponibilidade mínima `{minimum}` inválida");
        }
        movie.minimum_availability = Some(minimum.clone());
    }
    if let Some(tags) = &change.tags {
        movie.tags.clone_from(tags);
    }
    movie.available = is_available(&movie, today(), 0);
    if let Some((_, source_id)) = &entry.origin {
        let client = movie_client(config)?;
        let path = format!("api/v3/movie/{source_id}");
        let mut remote = client
            .get_json(&path, &[])
            .await
            .context("lendo o filme no gerenciador")?;
        remote["monitored"] = json!(movie.monitored);
        if let Some(minimum) = &movie.minimum_availability {
            remote["minimumAvailability"] = json!(minimum);
        }
        remote["tags"] = json!(movie.tags);
        if let Some(profile) = movie
            .quality_profile
            .as_ref()
            .and_then(|name| profiles.iter().find(|p| p.name == *name))
            .and_then(|p| p.source_id)
        {
            remote["qualityProfileId"] = json!(profile);
        }
        client
            .put_json(&path, &[], &remote)
            .await
            .context("mudando o filme no gerenciador")?;
    }
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
    let inside_root = config.movies.root_folders.iter().any(|root| {
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

/// Apaga só o arquivo do filme; o filme fica no catálogo, sem arquivo (e,
/// monitorado, volta a ser procurado). Filme do gerenciador apaga por lá.
///
/// # Errors
///
/// Filme sem arquivo, gerenciador que recusa ou falha ao apagar.
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
    if let (Some(_), Some(file_id)) = (&entry.origin, file.id) {
        movie_client(config)?
            .delete_path(&format!("api/v3/moviefile/{file_id}"), &[])
            .await
            .context("apagando o arquivo no gerenciador")?;
    } else {
        let path = Path::new(&entry.movie.path).join(&file.relative_path);
        let host = config.path_map().to_host(&path)?;
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&host) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        })
        .await??;
    }
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

/// Tira o filme do catálogo; com `delete_files`, apaga a pasta; com
/// `exclude`, nenhuma lista o traz de volta. Filme do gerenciador sai de lá
/// também (e é ele quem apaga os arquivos).
///
/// # Errors
///
/// Filme desconhecido, pasta fora das raízes, gerenciador que recusa ou
/// falha de escrita.
pub async fn remove(
    config: &Config,
    store: &Store,
    id: i64,
    delete_files: bool,
    exclude: bool,
) -> Result<()> {
    let entry = store
        .movies()
        .await?
        .into_iter()
        .find(|m| m.id == id)
        .context("filme fora do catálogo")?;
    if let Some((_, source_id)) = &entry.origin {
        movie_client(config)?
            .delete_path(
                &format!("api/v3/movie/{source_id}"),
                &[
                    ("deleteFiles", if delete_files { "true" } else { "false" }),
                    ("addImportExclusion", if exclude { "true" } else { "false" }),
                ],
            )
            .await
            .context("removendo o filme do gerenciador")?;
    } else if delete_files {
        delete_folder(config, &entry.movie.path).await?;
    }
    if exclude {
        store
            .add_exclusions(&[acervo_store::Exclusion {
                tmdb_id: entry.movie.tmdb_id,
                title: entry.movie.title.clone(),
                year: entry.movie.year,
            }])
            .await?;
    }
    store.delete_movie(id).await?;
    tracing::info!(
        filme = entry.movie.title,
        arquivos = delete_files,
        "filme removido"
    );
    events::record(
        store,
        Event {
            message: delete_files.then(|| format!("pasta apagada: {}", entry.movie.path)),
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

    fn movie(
        minimum: &str,
        cinema: Option<&str>,
        digital: Option<&str>,
        physical: Option<&str>,
    ) -> Movie {
        Movie {
            tmdb_id: 1,
            imdb_id: None,
            title: "Filme".into(),
            original_title: None,
            original_language: None,
            year: None,
            status: None,
            minimum_availability: Some(minimum.into()),
            monitored: true,
            quality_profile: None,
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
            tags: Vec::new(),
        }
    }

    fn day(text: &str) -> Date {
        date(Some(text)).unwrap()
    }

    #[test]
    fn status_como_a_referencia() {
        let today = day("2026-09-25");
        assert_eq!(
            status(&movie("released", None, None, None), today),
            "announced"
        );
        assert_eq!(
            status(&movie("released", Some("2026-10-01"), None, None), today),
            "announced"
        );
        assert_eq!(
            status(&movie("released", Some("2026-09-01"), None, None), today),
            "inCinemas"
        );
        // 90 dias de cinema sem digital nem físico contam como lançado.
        assert_eq!(
            status(&movie("released", Some("2026-06-01"), None, None), today),
            "released"
        );
        assert_eq!(
            status(
                &movie("released", Some("2026-09-01"), Some("2026-10-30"), None),
                today
            ),
            "inCinemas"
        );
        assert_eq!(
            status(&movie("released", None, Some("2026-09-20"), None), today),
            "released"
        );
    }

    #[test]
    fn disponibilidade_como_a_referencia() {
        let today = day("2026-09-25");
        assert!(is_available(
            &movie("announced", None, None, None),
            today,
            0
        ));
        assert!(!is_available(
            &movie("released", None, None, None),
            today,
            0
        ));
        assert!(is_available(
            &movie("inCinemas", Some("2026-09-20"), None, None),
            today,
            0
        ));
        assert!(!is_available(
            &movie("released", Some("2026-09-20"), None, None),
            today,
            0
        ));
        assert!(is_available(
            &movie("released", Some("2026-01-01"), None, Some("2026-09-24")),
            today,
            0
        ));
        // A carência empurra a data.
        assert!(!is_available(
            &movie("released", None, Some("2026-09-24"), None),
            today,
            2
        ));
        // Sem digital nem físico: 90 dias depois do cinema.
        assert!(is_available(
            &movie("released", Some("2026-06-01"), None, None),
            today,
            0
        ));
    }
}

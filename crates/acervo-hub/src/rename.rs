//! Renomear: põe os arquivos da biblioteca no nome que a importação daria
//! hoje — o episódio que entrou como `TBA` e ganhou título, a série que
//! passou a usar pasta de temporada, o filme cujo título mudou.
//!
//! O plano é puro. A execução só usa `rename(2)` (com `RENAME_NOREPLACE`
//! onde o sistema de arquivos aceita): o inode é o mesmo, então o hardlink
//! com o download continua e o torrent não percebe nada. Destino que já
//! existe é erro, nada sai da pasta da série ou do filme, a pasta de
//! temporada nasce quando falta e a que ficou vazia sai. As legendas do
//! arquivo vão junto.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use acervo_store::{CatalogMovie, CatalogSeries, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::events::{self, Event, Kind};
use crate::naming::movie_file_stem;
use crate::series::naming::{TBA, episode_path};

/// Uma legenda que acompanha o vídeo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubtitleMove {
    /// Id no catálogo; `None` é a legenda do disco que o catálogo não tinha,
    /// registrada (como do disco) depois de renomeada.
    #[serde(skip)]
    pub id: Option<i64>,
    pub de: String,
    pub para: String,
}

/// As legendas que seguem o vídeo de `from` para `to`: as do catálogo deste
/// arquivo e as do disco que o catálogo não conhece (de dono nenhum) e
/// começam pelo stem dele.
fn subtitle_moves<'a>(
    from: &str,
    to: &str,
    own: impl Iterator<Item = (i64, &'a str)>,
    registered: &HashSet<&str>,
    disk: &[String],
) -> Vec<SubtitleMove> {
    let mut moves: Vec<SubtitleMove> = own
        .filter_map(|(id, path)| {
            Some(SubtitleMove {
                id: Some(id),
                de: path.to_owned(),
                para: crate::subtitles::follow(from, to, path)?,
            })
        })
        .collect();
    moves.extend(
        disk.iter()
            .filter(|path| !registered.contains(path.as_str()))
            .filter_map(|path| {
                Some(SubtitleMove {
                    id: None,
                    de: path.clone(),
                    para: crate::subtitles::follow(from, to, path)?,
                })
            }),
    );
    moves
}

/// O dono das legendas de um passo.
#[derive(Debug, Clone, Copy)]
enum Owner {
    EpisodeFile(i64),
    Movie(i64),
}

/// Um arquivo fora do nome esperado, e o que aconteceu com ele.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rename {
    /// Id do arquivo de episódio; num filme, o do filme.
    pub arquivo_id: i64,
    /// Relativo à pasta da série ou do filme.
    pub de: String,
    pub para: String,
    pub legendas: Vec<SubtitleMove>,
    pub feito: bool,
    pub erro: Option<String>,
}

fn extension(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mkv")
}

/// O caminho que a importação daria a um arquivo de série hoje: o formato
/// de [`episode_path`], com a pasta de temporada conforme a série, o título
/// de cada episódio (ou [`TBA`], se ainda não veio), a qualidade e a
/// extensão do arquivo. `None` se o arquivo não cobre episódio nenhum.
#[must_use]
pub fn expected_episode_path(entry: &CatalogSeries, file_id: i64) -> Option<String> {
    let file = entry.files.iter().find(|f| f.id == file_id)?;
    let mut episodes: Vec<&acervo_store::Episode> = entry
        .episodes
        .iter()
        .filter(|e| e.file_id == Some(file_id))
        .map(|e| &e.episode)
        .collect();
    if episodes.is_empty() {
        return None;
    }
    episodes.sort_by_key(|e| (e.season, e.number));
    let named: Vec<(u16, u16, &str)> = episodes
        .iter()
        .map(|e| {
            let title = e
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(TBA);
            (e.season, e.number, title)
        })
        .collect();
    Some(episode_path(
        crate::series::folder_title(&entry.series),
        entry.series.season_folder,
        &named,
        file.file.quality,
        extension(&file.file.relative_path),
    ))
}

/// Os arquivos da série fora do nome esperado. `disk` são as legendas da
/// pasta da série, relativas a ela.
#[must_use]
pub fn series_plan(entry: &CatalogSeries, disk: &[String]) -> Vec<Rename> {
    let registered: HashSet<&str> = entry
        .subtitles
        .iter()
        .map(|s| s.subtitle.relative_path.as_str())
        .collect();
    entry
        .files
        .iter()
        .filter_map(|file| {
            let to = expected_episode_path(entry, file.id)?;
            let from = &file.file.relative_path;
            (to != *from).then(|| Rename {
                arquivo_id: file.id,
                de: from.clone(),
                legendas: subtitle_moves(
                    from,
                    &to,
                    entry
                        .subtitles
                        .iter()
                        .filter(|s| s.owner == file.id)
                        .map(|s| (s.id, s.subtitle.relative_path.as_str())),
                    &registered,
                    disk,
                ),
                para: to,
                feito: false,
                erro: None,
            })
        })
        .collect()
}

/// `TBA` como palavra no nome do arquivo (não na pasta).
fn has_tba(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.split(|c: char| !c.is_alphanumeric()).any(|w| w == TBA)
}

/// Só o que a tarefa de metadados renomeia sozinha: arquivo com `TBA` no
/// nome cujo título já chegou (o nome novo não tem mais `TBA`).
#[must_use]
pub fn titled_since(plan: Vec<Rename>) -> Vec<Rename> {
    plan.into_iter()
        .filter(|r| has_tba(&r.de) && !has_tba(&r.para))
        .collect()
}

/// O nome esperado do arquivo do filme, na pasta dele. `disk` são as
/// legendas da pasta, relativas a ela.
#[must_use]
pub fn movie_plan(entry: &CatalogMovie, disk: &[String]) -> Vec<Rename> {
    let Some(file) = &entry.movie.file else {
        return Vec::new();
    };
    let to = format!(
        "{}.{}",
        movie_file_stem(&entry.movie, entry.extras.metadata_title.as_deref()),
        extension(&file.relative_path).to_ascii_lowercase()
    );
    if to == file.relative_path {
        return Vec::new();
    }
    vec![Rename {
        arquivo_id: entry.id,
        de: file.relative_path.clone(),
        legendas: subtitle_moves(
            &file.relative_path,
            &to,
            entry
                .subtitles
                .iter()
                .map(|s| (s.id, s.subtitle.relative_path.as_str())),
            &entry
                .subtitles
                .iter()
                .map(|s| s.subtitle.relative_path.as_str())
                .collect(),
            disk,
        ),
        para: to,
        feito: false,
        erro: None,
    }]
}

/// Caminho relativo que não sobe nem salta: só nomes comuns.
fn plain(relative: &str) -> Result<&Path> {
    let path = Path::new(relative);
    if relative.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("caminho `{relative}` sai da pasta");
    }
    Ok(path)
}

/// O ancestral mais fundo que existe, resolvido: é por ele que se sabe se
/// um caminho ainda por criar fica dentro da raiz, mesmo com link
/// simbólico no meio.
fn resolved_ancestor(path: &Path) -> Result<PathBuf> {
    let mut current = path;
    loop {
        if let Ok(found) = current.canonicalize() {
            return Ok(found);
        }
        current = current
            .parent()
            .with_context(|| format!("`{}` sem ancestral", path.display()))?;
    }
}

/// Renomeia `from` para `to` (relativos a `root`) sem sobrescrever e sem
/// sair de `root`, criando a pasta do destino se faltar. Bloqueia.
///
/// # Errors
///
/// Caminho que sai da raiz, origem que não é arquivo, destino que já
/// existe, discos diferentes ou falha do sistema de arquivos.
pub fn move_within(root: &Path, from: &str, to: &str) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("lendo a pasta `{}`", root.display()))?;
    let source = root.join(plain(from)?);
    let target = root.join(plain(to)?);
    let meta = std::fs::symlink_metadata(&source)
        .with_context(|| format!("lendo `{}`", source.display()))?;
    if !meta.is_file() {
        bail!("`{from}` não é um arquivo comum");
    }
    if !resolved_ancestor(source.parent().unwrap_or(&root))?.starts_with(&root) {
        bail!("`{from}` está fora da pasta");
    }
    let parent = target.parent().unwrap_or(&root);
    if !resolved_ancestor(parent)?.starts_with(&root) {
        bail!("`{to}` sairia da pasta");
    }
    std::fs::create_dir_all(parent).with_context(|| format!("criando `{}`", parent.display()))?;
    if !parent.canonicalize()?.starts_with(&root) {
        bail!("`{to}` sairia da pasta");
    }
    let flags = rustix::fs::RenameFlags::NOREPLACE;
    match rustix::fs::renameat_with(rustix::fs::CWD, &source, rustix::fs::CWD, &target, flags) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::EXIST) => bail!("já existe `{to}`; nada sobrescrito"),
        Err(rustix::io::Errno::XDEV) => {
            bail!("`{from}` e `{to}` estão em sistemas de arquivos diferentes")
        }
        // Sistema de arquivos sem `RENAME_NOREPLACE`: confere antes.
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
            if std::fs::symlink_metadata(&target).is_ok() {
                bail!("já existe `{to}`; nada sobrescrito");
            }
            std::fs::rename(&source, &target)
                .with_context(|| format!("renomeando `{from}` para `{to}`"))
        }
        Err(error) => Err(std::io::Error::from(error))
            .with_context(|| format!("renomeando `{from}` para `{to}`")),
    }
}

/// Tira a pasta de `relative` (relativo a `root`) se ela ficou vazia, e não
/// é a própria raiz. Bloqueia; falha não importa.
pub fn prune(root: &Path, relative: &str) {
    let Some(dir) = Path::new(relative).parent() else {
        return;
    };
    if dir.as_os_str().is_empty() || plain(&dir.to_string_lossy()).is_err() {
        return;
    }
    // `remove_dir` só tira pasta vazia.
    let _ = std::fs::remove_dir(root.join(dir));
}

/// Aplica um passo: o vídeo, as legendas, o catálogo e a limpeza da pasta.
/// `update` grava o caminho novo do vídeo; se falhar, o vídeo volta.
async fn run_step<F, Fut>(store: &Store, root: &Path, owner: Owner, step: &mut Rename, update: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<bool, acervo_store::StoreError>>,
{
    let (dir, from, to) = (root.to_path_buf(), step.de.clone(), step.para.clone());
    let moved = tokio::task::spawn_blocking(move || move_within(&dir, &from, &to))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    if let Err(error) = moved {
        step.erro = Some(format!("{error:#}"));
        return;
    }
    match update(step.para.clone()).await {
        Ok(true) => {}
        outcome => {
            // O catálogo não acompanhou: o arquivo volta ao nome antigo.
            let (dir, from, to) = (root.to_path_buf(), step.para.clone(), step.de.clone());
            let back = tokio::task::spawn_blocking(move || move_within(&dir, &from, &to)).await;
            let why = match outcome {
                Err(error) => format!("gravando o nome novo: {error}"),
                _ => "o arquivo saiu do catálogo no meio".into(),
            };
            step.erro = Some(match back {
                Ok(Ok(())) => why,
                _ => format!("{why}; e não voltou ao nome antigo"),
            });
            return;
        }
    }
    step.feito = true;
    let mut problems = Vec::new();
    for subtitle in &step.legendas {
        let (dir, from, to) = (
            root.to_path_buf(),
            subtitle.de.clone(),
            subtitle.para.clone(),
        );
        let moved = tokio::task::spawn_blocking(move || move_within(&dir, &from, &to))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
        if let Err(error) = moved {
            problems.push(format!("legenda `{}`: {error:#}", subtitle.de));
            continue;
        }
        if let Err(error) = record_subtitle(store, owner, subtitle).await {
            // O catálogo não acompanhou: a legenda volta ao nome antigo.
            let (dir, from, to) = (
                root.to_path_buf(),
                subtitle.para.clone(),
                subtitle.de.clone(),
            );
            let back = tokio::task::spawn_blocking(move || move_within(&dir, &from, &to)).await;
            problems.push(match back {
                Ok(Ok(())) => format!("legenda `{}`: {error}", subtitle.de),
                _ => format!(
                    "legenda `{}`: {error}; e não voltou ao nome antigo",
                    subtitle.de
                ),
            });
        }
    }
    if !problems.is_empty() {
        step.erro = Some(problems.join("; "));
    }
    let dir = root.to_path_buf();
    let olds: Vec<String> = std::iter::once(step.de.clone())
        .chain(step.legendas.iter().map(|s| s.de.clone()))
        .collect();
    let _ = tokio::task::spawn_blocking(move || {
        for old in olds {
            prune(&dir, &old);
        }
    })
    .await;
}

/// Grava no catálogo o nome novo de uma legenda; a do disco entra agora,
/// como do disco.
async fn record_subtitle(
    store: &Store,
    owner: Owner,
    subtitle: &SubtitleMove,
) -> Result<(), acervo_store::StoreError> {
    if let Some(id) = subtitle.id {
        return if store.set_subtitle_path(id, &subtitle.para).await? {
            Ok(())
        } else {
            Err(acervo_store::StoreError::Corrupt(
                "a legenda saiu do catálogo no meio".into(),
            ))
        };
    }
    let (language, forced) = crate::subtitles::language(&subtitle.para);
    let record = acervo_store::Subtitle {
        relative_path: subtitle.para.clone(),
        language: language.map(str::to_owned),
        forced,
        origin: acervo_store::SubtitleOrigin::Disk,
    };
    match owner {
        Owner::EpisodeFile(id) => store.add_episode_subtitle(id, &record).await?,
        Owner::Movie(id) => store.add_movie_subtitle(id, &record).await?,
    };
    Ok(())
}

/// As legendas da pasta (no host), relativas a ela. Pasta ilegível: nenhuma
/// (o renomear em si é que vai dizer o porquê).
pub async fn disk_subtitles(folder: &str) -> Vec<String> {
    let root = (Path::new(folder)).to_path_buf();
    tokio::task::spawn_blocking(move || crate::verify::scan(&root, &[]))
        .await
        .ok()
        .and_then(Result::ok)
        .map(|f| f.subtitles.into_iter().map(|s| s.relative).collect())
        .unwrap_or_default()
}

/// Renomeia os arquivos da série do plano. Cada passo diz se foi feito e,
/// se não, por quê; um não segura os outros.
///
/// # Errors
///
/// Banco inalcançável ou falha ao renomear.
pub async fn apply_series(
    store: &Store,
    entry: &CatalogSeries,
    mut plan: Vec<Rename>,
) -> Result<Vec<Rename>> {
    let root = (Path::new(&entry.series.path)).to_path_buf();
    for step in &mut plan {
        let file_id = step.arquivo_id;
        run_step(
            store,
            &root,
            Owner::EpisodeFile(file_id),
            step,
            |path| async move { store.set_episode_file_path(entry.id, file_id, &path).await },
        )
        .await;
        if step.feito {
            let covered: Vec<i64> = entry
                .episodes
                .iter()
                .filter(|e| e.file_id == Some(file_id))
                .map(|e| e.id)
                .collect();
            events::record(
                store,
                Event {
                    message: Some(format!("{} → {}", step.de, step.para)),
                    poster: entry.series.poster.clone(),
                    ..Event::series(
                        Kind::Renamed,
                        entry.id,
                        crate::series::label(entry, &covered),
                        &covered,
                    )
                },
            )
            .await;
        }
    }
    Ok(plan)
}

/// Renomeia o arquivo do filme do plano.
///
/// # Errors
///
/// Banco inalcançável ou falha ao renomear.
pub async fn apply_movie(
    store: &Store,
    entry: &CatalogMovie,
    mut plan: Vec<Rename>,
) -> Result<Vec<Rename>> {
    let root = (Path::new(&entry.movie.path)).to_path_buf();
    for step in &mut plan {
        run_step(
            store,
            &root,
            Owner::Movie(entry.id),
            step,
            |path| async move { store.set_movie_file_path(entry.id, &path).await },
        )
        .await;
        if step.feito {
            events::record(
                store,
                Event {
                    message: Some(format!("{} → {}", step.de, step.para)),
                    poster: entry.extras.poster.clone(),
                    ..Event::new(
                        Kind::Renamed,
                        Some(entry.id),
                        events::label(&entry.movie.title, entry.movie.year),
                    )
                },
            )
            .await;
        }
    }
    Ok(plan)
}

/// Depois da atualização de metadados: renomeia sozinho, em cada série, só
/// os arquivos com `TBA` cujo título chegou. Devolve quantos.
///
/// # Errors
///
/// Catálogo ilegível. Falha num arquivo fica no log; os outros seguem.
pub async fn titled_files(store: &Store) -> Result<usize> {
    let mut done = 0;
    for entry in store.series_list().await? {
        if titled_since(series_plan(&entry, &[])).is_empty() {
            continue;
        }
        let disk = disk_subtitles(&entry.series.path).await;
        let plan = titled_since(series_plan(&entry, &disk));
        let result = match apply_series(store, &entry, plan).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(serie = entry.series.title, "renomear: {error:#}");
                continue;
            }
        };
        for step in result {
            if let Some(error) = &step.erro {
                tracing::warn!(
                    serie = entry.series.title,
                    arquivo = step.de,
                    "renomear: {error}"
                );
            }
            if step.feito {
                done += 1;
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use acervo_parser::{Quality, QualityModel, Revision};
    use acervo_store::{
        CatalogEpisode, CatalogEpisodeFile, CatalogSubtitle, Episode, EpisodeFile, Series, Subtitle,
    };

    use super::*;

    fn series(season_folder: bool) -> Series {
        Series {
            tmdb_id: 1,
            tvdb_id: None,
            imdb_id: None,
            title: "A Série".into(),
            original_title: None,
            metadata_title: Some("The Show".into()),
            original_language: None,
            year: None,
            status: None,
            overview: None,
            network: None,
            runtime: 0,
            poster: None,
            fanart: None,
            path: "/series/The Show".into(),
            season_folder,
            monitor_new: true,
            added: None,
            refreshed_at: None,
            alternate_titles: Vec::new(),
        }
    }

    fn entry(season_folder: bool, title: Option<&str>, path: &str) -> CatalogSeries {
        CatalogSeries {
            id: 1,
            series: series(season_folder),
            episodes: vec![CatalogEpisode {
                id: 10,
                episode: Episode {
                    season: 1,
                    number: 2,
                    tmdb_id: None,
                    title: title.map(str::to_owned),
                    air_date: None,
                    overview: None,
                    runtime: 0,
                },
                skip: None,
                skipped_at: None,
                file_id: Some(100),
            }],
            files: vec![CatalogEpisodeFile {
                id: 100,
                file: EpisodeFile {
                    relative_path: path.into(),
                    size: 1,
                    quality: QualityModel {
                        quality: Quality::WebDl1080p,
                        revision: Revision::default(),
                    },
                    languages: Vec::new(),
                    release_group: None,
                    scene_name: None,
                    date_added: None,
                },
            }],
            priority: false,
            scene: Vec::new(),
            subtitles: vec![CatalogSubtitle {
                id: 7,
                owner: 100,
                subtitle: Subtitle {
                    relative_path: path.replace(".mkv", ".pt-BR.srt"),
                    language: Some("pt-BR".into()),
                    forced: false,
                    origin: acervo_store::SubtitleOrigin::Import,
                },
            }],
        }
    }

    #[test]
    fn caminho_esperado_com_titulo_tba_e_pasta_de_temporada() {
        let tba = "Season 1/The Show - S01E02 - TBA WEBDL-1080p.mkv";
        let named = "Season 1/The Show - S01E02 - Piloto WEBDL-1080p.mkv";
        assert_eq!(
            expected_episode_path(&entry(true, None, tba), 100).as_deref(),
            Some(tba)
        );
        // Já no nome: nada a fazer.
        assert!(series_plan(&entry(true, None, tba), &[]).is_empty());
        // O título chegou: renomeia, com a legenda junto.
        let plan = series_plan(
            &entry(true, Some("Piloto"), tba),
            &[
                "Season 1/The Show - S01E02 - TBA WEBDL-1080p.pt-BR.srt".into(),
                "Season 1/The Show - S01E02 - TBA WEBDL-1080p.en.srt".into(),
                "Season 1/outra.srt".into(),
            ],
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].para, named);
        assert_eq!(
            plan[0].legendas,
            [
                SubtitleMove {
                    id: Some(7),
                    de: "Season 1/The Show - S01E02 - TBA WEBDL-1080p.pt-BR.srt".into(),
                    para: "Season 1/The Show - S01E02 - Piloto WEBDL-1080p.pt-BR.srt".into(),
                },
                // A do disco que o catálogo não tinha vai junto, sem id.
                SubtitleMove {
                    id: None,
                    de: "Season 1/The Show - S01E02 - TBA WEBDL-1080p.en.srt".into(),
                    para: "Season 1/The Show - S01E02 - Piloto WEBDL-1080p.en.srt".into(),
                },
            ]
        );
        assert_eq!(titled_since(plan).len(), 1);
        // Sem pasta de temporada: o arquivo sobe para a pasta da série.
        let plan = series_plan(&entry(false, Some("Piloto"), named), &[]);
        assert_eq!(plan[0].para, "The Show - S01E02 - Piloto WEBDL-1080p.mkv");
        // Mudança só de pasta não é das que a tarefa faz sozinha.
        assert!(titled_since(plan).is_empty());
        // Arquivo sem episódio não tem nome esperado.
        let mut orphan = entry(true, None, tba);
        orphan.episodes.clear();
        assert!(series_plan(&orphan, &[]).is_empty());
    }

    #[test]
    fn renomeia_no_mesmo_inode_sem_sobrescrever_nem_sair_da_pasta() {
        let base = std::env::temp_dir().join(format!("acervo-renomear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("The Show");
        let download = base.join("download");
        std::fs::create_dir_all(root.join("Season 1")).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        // O torrent e a biblioteca: o mesmo inode.
        std::fs::write(download.join("ep.mkv"), b"video").unwrap();
        std::fs::hard_link(download.join("ep.mkv"), root.join("Season 1/ep TBA.mkv")).unwrap();
        let before = std::fs::metadata(download.join("ep.mkv")).unwrap();

        // Sobe para a raiz da série: a pasta de temporada vazia sai.
        move_within(&root, "Season 1/ep TBA.mkv", "ep Piloto.mkv").unwrap();
        prune(&root, "Season 1/ep TBA.mkv");
        assert!(!root.join("Season 1").exists());
        let after = std::fs::metadata(root.join("ep Piloto.mkv")).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.nlink(), 2);
        assert_eq!(std::fs::read(download.join("ep.mkv")).unwrap(), b"video");

        // Volta para uma pasta de temporada que ainda não existe.
        move_within(&root, "ep Piloto.mkv", "Season 1/ep Piloto.mkv").unwrap();
        assert!(root.join("Season 1/ep Piloto.mkv").exists());

        // Destino que existe: erro, e nada muda.
        std::fs::write(root.join("outro.mkv"), b"outro").unwrap();
        let error = move_within(&root, "Season 1/ep Piloto.mkv", "outro.mkv").unwrap_err();
        assert!(format!("{error:#}").contains("já existe"), "{error:#}");
        assert_eq!(std::fs::read(root.join("outro.mkv")).unwrap(), b"outro");
        assert!(root.join("Season 1/ep Piloto.mkv").exists());

        // Nunca sai da pasta da série, nem por `..`, nem por link simbólico.
        assert!(move_within(&root, "Season 1/ep Piloto.mkv", "../fora.mkv").is_err());
        assert!(move_within(&root, "Season 1/ep Piloto.mkv", "/tmp/fora.mkv").is_err());
        std::os::unix::fs::symlink(&download, root.join("atalho")).unwrap();
        assert!(move_within(&root, "Season 1/ep Piloto.mkv", "atalho/fora.mkv").is_err());
        assert!(!download.join("fora.mkv").exists());
        assert!(root.join("Season 1/ep Piloto.mkv").exists());
        // A pasta com arquivo não sai.
        prune(&root, "Season 1/qualquer.mkv");
        assert!(root.join("Season 1").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}

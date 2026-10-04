//! Verificar disco (importação manual): confere a pasta de uma série ou de
//! um filme contra o catálogo. O vídeo que o catálogo não conhece e casa
//! sem ambiguidade é ligado; o que o catálogo conhece e sumiu do disco sai
//! dele; a legenda solta ao lado de um vídeo conhecido entra.
//!
//! **Nunca apaga, move nem renomeia nada no disco.** Pasta que não existe
//! não é "tudo sumiu" — pode ser disco desmontado —, e nada é feito.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use acervo_parser::{Language, parse_episode_path};
use acervo_store::{CatalogMovie, CatalogSeries, EpisodeFile, MovieFile, Store, Subtitle};
use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::decide::now_rfc3339;
use crate::grab::VIDEO;
use crate::series::grab::is_sample_or_extra;

/// Um arquivo da pasta, relativo a ela.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDisk {
    pub relative: String,
    pub size: u64,
}

/// O que há na pasta: vídeos e legendas, sem amostra nem extra.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Folder {
    pub videos: Vec<OnDisk>,
    pub subtitles: Vec<OnDisk>,
    /// Caminhos do catálogo que o disco confirmou não existir.
    pub missing: HashSet<String>,
}

fn is_video(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()))
}

/// Lê a pasta (no host), sem seguir link simbólico, e confere cada caminho
/// do catálogo. Só `NotFound` conta como sumido; outro erro é dúvida, e o
/// arquivo fica. Bloqueia.
///
/// # Errors
///
/// Pasta que não existe ou não se lê.
pub fn scan(root: &Path, known: &[String]) -> Result<Folder> {
    let meta = std::fs::metadata(root).with_context(|| {
        format!(
            "a pasta `{}` não está no disco; nada conferido",
            root.display()
        )
    })?;
    if !meta.is_dir() {
        bail!("`{}` não é uma pasta", root.display());
    }
    let mut folder = Folder::default();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let dir = root.join(&relative);
        for item in std::fs::read_dir(&dir).with_context(|| format!("lendo `{}`", dir.display()))? {
            let item = item?;
            let kind = item.file_type()?;
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = relative.join(&name);
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let text = path.to_string_lossy().into_owned();
            if is_sample_or_extra(&text) {
                continue;
            }
            let entry = OnDisk {
                relative: text,
                size: item.metadata()?.len(),
            };
            if is_video(&name) {
                folder.videos.push(entry);
            } else if crate::subtitles::subtitle_extension(&name).is_some() {
                folder.subtitles.push(entry);
            }
        }
    }
    folder.videos.sort_by(|a, b| a.relative.cmp(&b.relative));
    folder.subtitles.sort_by(|a, b| a.relative.cmp(&b.relative));
    for path in known {
        if let Err(error) = std::fs::symlink_metadata(root.join(path))
            && error.kind() == std::io::ErrorKind::NotFound
        {
            folder.missing.insert(path.clone());
        }
    }
    Ok(folder)
}

/// Um vídeo novo, ligado (ou a ligar) a episódios.
#[derive(Debug, Clone, Serialize)]
pub struct NewVideo {
    pub arquivo: String,
    pub tamanho: u64,
    /// Ids dos episódios; num filme, vazio.
    pub episodios: Vec<i64>,
    /// `S01E02`; num filme, vazio.
    pub codigo: String,
    pub qualidade: &'static str,
    /// Algum episódio estava dispensado: volta a "Tenho", sem o `skip`.
    pub estava_dispensado: bool,
    pub feito: bool,
    pub erro: Option<String>,
}

/// Vídeo que ficou como está.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unrecognized {
    pub arquivo: String,
    pub motivo: String,
}

/// Arquivo do catálogo que não está mais no disco.
#[derive(Debug, Clone, Serialize)]
pub struct Gone {
    pub arquivo_id: i64,
    pub arquivo: String,
    pub episodios: Vec<i64>,
    pub codigo: String,
    /// Sem arquivo e sem `skip`, volta a ser buscado.
    pub volta_a_busca: bool,
    pub feito: bool,
    pub erro: Option<String>,
}

/// Legenda solta ao lado de um vídeo conhecido.
#[derive(Debug, Clone, Serialize)]
pub struct NewSubtitle {
    pub arquivo: String,
    /// O vídeo de quem ela é.
    pub video: String,
    pub idioma: Option<&'static str>,
    pub forcada: bool,
    pub feito: bool,
    pub erro: Option<String>,
}

/// O plano (ou o que foi feito) de uma pasta.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Check {
    pub aplicado: bool,
    pub novos: Vec<NewVideo>,
    pub nao_reconhecidos: Vec<Unrecognized>,
    pub sumidos: Vec<Gone>,
    pub legendas: Vec<NewSubtitle>,
}

impl Check {
    /// Há o que fazer.
    #[must_use]
    pub fn any(&self) -> bool {
        !self.novos.is_empty() || !self.sumidos.is_empty() || !self.legendas.is_empty()
    }
}

/// As legendas soltas: as que o catálogo não tem e cujo nome começa pelo
/// stem de um dos `videos` seguido de ponto.
fn loose_subtitles(disk: &[OnDisk], known: &HashSet<&str>, videos: &[&str]) -> Vec<NewSubtitle> {
    disk.iter()
        .filter(|s| !known.contains(s.relative.as_str()))
        .filter_map(|s| {
            let video = videos.iter().find(|video| {
                let stem = video.rsplit_once('.').map_or(**video, |(stem, _)| stem);
                s.relative
                    .strip_prefix(stem)
                    .is_some_and(|rest| rest.starts_with('.'))
            })?;
            let (idioma, forcada) = crate::subtitles::language(&s.relative);
            Some(NewSubtitle {
                arquivo: s.relative.clone(),
                video: (*video).to_owned(),
                idioma,
                forcada,
                feito: false,
                erro: None,
            })
        })
        .collect()
}

/// O que a conferência de uma série pode fazer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// Tira do catálogo o arquivo que sumiu do disco.
    pub remove_gone: bool,
    /// Liga vídeo a episódio dispensado (e tira o `skip`). Só o verificar
    /// manual: a volta automática nunca devolve um dispensado.
    pub link_skipped: bool,
}

impl Mode {
    /// O verificar da tela.
    pub const MANUAL: Self = Self {
        remove_gone: true,
        link_skipped: true,
    };
    /// A volta da importação: só liga o novo, nunca o dispensado.
    pub const AUTOMATIC: Self = Self {
        remove_gone: false,
        link_skipped: false,
    };
}

/// Os episódios que um vídeo novo diz ser, se todos estão no catálogo e
/// sem arquivo (e, sem `link_skipped`, sem `skip`); senão, por que não. O
/// arquivo na pasta da série está na numeração do catálogo: o par vale como
/// o nome diz, sem numeração de cena.
fn claim(
    entry: &CatalogSeries,
    relative: &str,
    link_skipped: bool,
    gone: &HashSet<i64>,
) -> Result<Vec<i64>, String> {
    let parsed = parse_episode_path(relative)
        .filter(|p| !p.episodes.is_empty())
        .ok_or("o nome não diz temporada e episódio")?;
    let mut numbered: Vec<(u16, u16)> = parsed
        .episodes
        .iter()
        .map(|n| (parsed.season, *n))
        .collect();
    numbered.sort_unstable();
    numbered.dedup();
    let matched: Vec<&acervo_store::CatalogEpisode> = entry
        .episodes
        .iter()
        .filter(|e| numbered.contains(&(e.episode.season, e.episode.number)))
        .collect();
    if matched.len() != numbered.len() {
        let missing: Vec<(u16, u16)> = numbered
            .iter()
            .copied()
            .filter(|(s, n)| {
                !matched
                    .iter()
                    .any(|e| e.episode.season == *s && e.episode.number == *n)
            })
            .collect();
        return Err(format!(
            "episódio fora do catálogo: {}",
            crate::series::episode_code(&missing)
        ));
    }
    // O arquivo que sai do catálogo nesta conferência não ocupa o episódio.
    if let Some(taken) = matched
        .iter()
        .find(|e| e.file_id.is_some_and(|id| !gone.contains(&id)))
    {
        return Err(format!(
            "{} já tem arquivo",
            crate::series::episode_code(&[(taken.episode.season, taken.episode.number)])
        ));
    }
    if !link_skipped && let Some(skipped) = matched.iter().find(|e| e.skip.is_some()) {
        return Err(format!(
            "{} está dispensado: só o verificar da tela o liga",
            crate::series::episode_code(&[(skipped.episode.season, skipped.episode.number)])
        ));
    }
    Ok(matched.iter().map(|e| e.id).collect())
}

/// O plano de uma série, sem IO.
#[must_use]
pub fn series_plan(entry: &CatalogSeries, folder: &Folder, mode: Mode) -> Check {
    let mut check = Check::default();
    let known: HashSet<&str> = entry
        .files
        .iter()
        .map(|f| f.file.relative_path.as_str())
        .collect();
    if mode.remove_gone {
        check.sumidos = gone_files(entry, folder);
    }
    let gone: HashSet<i64> = check.sumidos.iter().map(|g| g.arquivo_id).collect();
    // Cada vídeo novo e os episódios que ele diz.
    let mut claims: Vec<(&OnDisk, Vec<i64>)> = Vec::new();
    for video in folder
        .videos
        .iter()
        .filter(|v| !known.contains(v.relative.as_str()))
    {
        match claim(entry, &video.relative, mode.link_skipped, &gone) {
            Ok(ids) => claims.push((video, ids)),
            Err(motivo) => check.nao_reconhecidos.push(Unrecognized {
                arquivo: video.relative.clone(),
                motivo,
            }),
        }
    }
    // Dois vídeos para o mesmo episódio: nenhum dos dois.
    let mut count: HashMap<i64, usize> = HashMap::new();
    for (_, ids) in &claims {
        for id in ids {
            *count.entry(*id).or_default() += 1;
        }
    }
    for (video, ids) in claims {
        if ids.iter().any(|id| count[id] > 1) {
            check.nao_reconhecidos.push(Unrecognized {
                arquivo: video.relative.clone(),
                motivo: "mais de um arquivo para o mesmo episódio".into(),
            });
            continue;
        }
        let numbers: Vec<(u16, u16)> = entry
            .episodes
            .iter()
            .filter(|e| ids.contains(&e.id))
            .map(|e| (e.episode.season, e.episode.number))
            .collect();
        check.novos.push(NewVideo {
            arquivo: video.relative.clone(),
            tamanho: video.size,
            codigo: crate::series::episode_code(&numbers),
            estava_dispensado: entry
                .episodes
                .iter()
                .any(|e| ids.contains(&e.id) && e.skip.is_some()),
            qualidade: episode_quality(&video.relative).quality.name(),
            episodios: ids,
            feito: false,
            erro: None,
        });
    }
    // As legendas do arquivo que sai do catálogo saem junto: no disco, elas
    // voltam a ser soltas.
    let known_subtitles: HashSet<&str> = entry
        .subtitles
        .iter()
        .filter(|s| check.sumidos.iter().all(|g| g.arquivo_id != s.owner))
        .map(|s| s.subtitle.relative_path.as_str())
        .collect();
    let videos: Vec<&str> = entry
        .files
        .iter()
        .map(|f| f.file.relative_path.as_str())
        .filter(|p| !folder.missing.contains(*p))
        .chain(check.novos.iter().map(|n| n.arquivo.as_str()))
        .collect();
    check.legendas = loose_subtitles(&folder.subtitles, &known_subtitles, &videos);
    check
}

/// Os arquivos da série que o disco confirmou terem sumido.
fn gone_files(entry: &CatalogSeries, folder: &Folder) -> Vec<Gone> {
    entry
        .files
        .iter()
        .filter(|f| folder.missing.contains(&f.file.relative_path))
        .map(|file| {
            let episodes: Vec<&acervo_store::CatalogEpisode> = entry
                .episodes
                .iter()
                .filter(|e| e.file_id == Some(file.id))
                .collect();
            Gone {
                arquivo_id: file.id,
                arquivo: file.file.relative_path.clone(),
                episodios: episodes.iter().map(|e| e.id).collect(),
                codigo: crate::series::episode_code(
                    &episodes
                        .iter()
                        .map(|e| (e.episode.season, e.episode.number))
                        .collect::<Vec<_>>(),
                ),
                volta_a_busca: episodes.iter().any(|e| e.skip.is_none()),
                feito: false,
                erro: None,
            }
        })
        .collect()
}

/// A qualidade pelo nome do arquivo; sem ela, pelo caminho inteiro.
fn episode_quality(relative: &str) -> acervo_parser::QualityModel {
    let file = relative.rsplit('/').next().unwrap_or(relative);
    let quality = acervo_parser::parse_episode_quality(file);
    if quality.quality == acervo_parser::Quality::Unknown {
        acervo_parser::parse_episode_quality(relative)
    } else {
        quality
    }
}

/// O plano de um filme, sem IO: o arquivo do catálogo que sumiu sai; sem
/// arquivo (ou com o dele sumido), o maior vídeo da pasta é ligado.
#[must_use]
pub fn movie_plan(entry: &CatalogMovie, folder: &Folder) -> Check {
    let mut check = Check::default();
    let current = entry.movie.file.as_ref().map(|f| f.relative_path.as_str());
    let gone = current.is_some_and(|c| folder.missing.contains(c));
    if let (true, Some(path)) = (gone, current) {
        check.sumidos.push(Gone {
            arquivo_id: entry.id,
            arquivo: path.to_owned(),
            episodios: Vec::new(),
            codigo: String::new(),
            volta_a_busca: entry.movie.monitored,
            feito: false,
            erro: None,
        });
    }
    if current.is_none() || gone {
        if let Some(video) = folder.videos.iter().max_by_key(|v| v.size) {
            let file = video.relative.rsplit('/').next().unwrap_or(&video.relative);
            check.novos.push(NewVideo {
                arquivo: video.relative.clone(),
                tamanho: video.size,
                episodios: Vec::new(),
                codigo: String::new(),
                qualidade: acervo_parser::parse_quality(file).quality.name(),
                estava_dispensado: false,
                feito: false,
                erro: None,
            });
        }
        for video in folder
            .videos
            .iter()
            .filter(|v| check.novos.iter().all(|n| n.arquivo != v.relative))
        {
            check.nao_reconhecidos.push(Unrecognized {
                arquivo: video.relative.clone(),
                motivo: "há um vídeo maior na pasta".into(),
            });
        }
    }
    // Tirar o arquivo sumido tira as legendas registradas do filme.
    let known: HashSet<&str> = entry
        .subtitles
        .iter()
        .filter(|_| !gone)
        .map(|s| s.subtitle.relative_path.as_str())
        .collect();
    let videos: Vec<&str> = current
        .filter(|_| !gone)
        .into_iter()
        .chain(check.novos.iter().map(|n| n.arquivo.as_str()))
        .collect();
    check.legendas = loose_subtitles(&folder.subtitles, &known, &videos);
    check
}

/// Os idiomas de áudio: o `ffprobe`; sem ele, os do nome.
async fn languages(host: &Path, from_name: Vec<Language>) -> Vec<String> {
    crate::mediainfo::probe(host)
        .await
        .map(|p| p.audio_languages)
        .filter(|l| !l.is_empty())
        .unwrap_or(from_name)
        .into_iter()
        .filter(|l| *l != Language::Unknown)
        .map(|l| l.name().to_owned())
        .collect()
}

/// Lê a pasta da série e, com `apply`, liga os novos, tira os sumidos e
/// registra as legendas soltas, no que `mode` deixa. O disco não é tocado.
///
/// # Errors
///
/// Série desconhecida, pasta fora do mapa, ausente ou ilegível.
pub async fn series(
    config: &Config,
    store: &Store,
    series_id: i64,
    apply: bool,
    mode: Mode,
) -> Result<Check> {
    let entry = store
        .series(series_id)
        .await?
        .context("série fora do catálogo")?;
    let root = config.path_map().to_host(Path::new(&entry.series.path))?;
    let known: Vec<String> = entry
        .files
        .iter()
        .map(|f| f.file.relative_path.clone())
        .collect();
    let dir = root.clone();
    let folder = tokio::task::spawn_blocking(move || scan(&dir, &known)).await??;
    let mut check = series_plan(&entry, &folder, mode);
    if !apply {
        return Ok(check);
    }
    check.aplicado = true;
    let mut ids: HashMap<String, i64> = entry
        .files
        .iter()
        .map(|f| (f.file.relative_path.clone(), f.id))
        .collect();
    // Primeiro o que sumiu: o episódio dele fica livre para o vídeo novo.
    for gone in &mut check.sumidos {
        match store.delete_episode_file(gone.arquivo_id).await {
            Ok(_) => gone.feito = true,
            Err(error) => gone.erro = Some(error.to_string()),
        }
    }
    for new in &mut check.novos {
        let host = root.join(&new.arquivo);
        let file_name = new.arquivo.rsplit('/').next().unwrap_or(&new.arquivo);
        let parsed = acervo_parser::parse_episode_title(file_name);
        let record = EpisodeFile {
            relative_path: new.arquivo.clone(),
            size: new.tamanho,
            quality: episode_quality(&new.arquivo),
            languages: languages(
                &host,
                parsed
                    .as_ref()
                    .map(|p| p.languages.clone())
                    .unwrap_or_default(),
            )
            .await,
            release_group: acervo_parser::parse_episode_release_group(file_name),
            scene_name: None,
            date_added: Some(now_rfc3339()),
        };
        match store
            .add_episode_file(entry.id, &record, &new.episodios)
            .await
        {
            Ok((id, _)) => {
                ids.insert(new.arquivo.clone(), id);
                // O usuário pôs o arquivo lá: o `skip` deixa de valer.
                if new.estava_dispensado
                    && let Err(error) = store.set_skip(&new.episodios, None, &now_rfc3339()).await
                {
                    new.erro = Some(format!("ligado, mas o dispensado ficou: {error}"));
                }
                new.feito = true;
            }
            Err(error) => new.erro = Some(error.to_string()),
        }
    }
    for subtitle in &mut check.legendas {
        let Some(&file_id) = ids.get(&subtitle.video) else {
            subtitle.erro = Some("o vídeo dela não entrou no catálogo".into());
            continue;
        };
        let record = Subtitle {
            relative_path: subtitle.arquivo.clone(),
            language: subtitle.idioma.map(str::to_owned),
            forced: subtitle.forcada,
            origin: acervo_store::SubtitleOrigin::Disk,
        };
        match store.add_episode_subtitle(file_id, &record).await {
            Ok(_) => subtitle.feito = true,
            Err(error) => subtitle.erro = Some(error.to_string()),
        }
    }
    Ok(check)
}

/// Lê a pasta do filme e, com `apply`, faz o plano de [`movie_plan`] no
/// catálogo. O disco não é tocado.
///
/// # Errors
///
/// Pasta fora do mapa, ausente ou ilegível.
pub async fn movie(
    config: &Config,
    store: &Store,
    entry: &CatalogMovie,
    apply: bool,
) -> Result<Check> {
    let root = config.path_map().to_host(Path::new(&entry.movie.path))?;
    let known: Vec<String> = entry
        .movie
        .file
        .iter()
        .map(|f| f.relative_path.clone())
        .collect();
    let dir = root.clone();
    let folder = tokio::task::spawn_blocking(move || scan(&dir, &known)).await??;
    let mut check = movie_plan(entry, &folder);
    if !apply {
        return Ok(check);
    }
    check.aplicado = true;
    for gone in &mut check.sumidos {
        // As legendas registradas saem do catálogo junto.
        match store.set_movie_file(entry.id, None).await {
            Ok(()) => gone.feito = true,
            Err(error) => gone.erro = Some(error.to_string()),
        }
    }
    let mut linked = entry
        .movie
        .file
        .as_ref()
        .filter(|_| check.sumidos.is_empty())
        .map(|f| f.relative_path.clone());
    for new in &mut check.novos {
        let host = root.join(&new.arquivo);
        let file_name = new.arquivo.rsplit('/').next().unwrap_or(&new.arquivo);
        let from_name = acervo_parser::parse_languages(file_name);
        let record = MovieFile {
            relative_path: new.arquivo.clone(),
            size: new.tamanho,
            quality: acervo_parser::parse_quality(file_name),
            languages: languages(&host, from_name).await,
            release_group: acervo_parser::parse_release_group(file_name),
            edition: acervo_parser::parse_movie_title(file_name).and_then(|p| p.edition),
            scene_name: None,
            date_added: Some(now_rfc3339()),
        };
        match store.set_movie_file(entry.id, Some(&record)).await {
            Ok(()) => {
                new.feito = true;
                linked = Some(new.arquivo.clone());
            }
            Err(error) => new.erro = Some(error.to_string()),
        }
    }
    for subtitle in &mut check.legendas {
        if linked.as_deref() != Some(subtitle.video.as_str()) {
            subtitle.erro = Some("o vídeo dela não entrou no catálogo".into());
            continue;
        }
        let record = Subtitle {
            relative_path: subtitle.arquivo.clone(),
            language: subtitle.idioma.map(str::to_owned),
            forced: subtitle.forcada,
            origin: acervo_store::SubtitleOrigin::Disk,
        };
        match store.add_movie_subtitle(entry.id, &record).await {
            Ok(_) => subtitle.feito = true,
            Err(error) => subtitle.erro = Some(error.to_string()),
        }
    }
    Ok(check)
}

/// A conferência de um filme na lista de todos.
#[derive(Debug, Serialize)]
pub struct MovieCheck {
    pub filme_id: i64,
    pub filme: String,
    #[serde(flatten)]
    pub check: Check,
    /// Por que não se conferiu (pasta ausente, fora do mapa).
    pub aviso: Option<String>,
}

/// [`movie`] para todos os filmes. Só entram na resposta os que têm o que
/// fazer ou não puderam ser conferidos — fora os sem arquivo e sem pasta,
/// que é o normal de quem ainda não baixou.
///
/// # Errors
///
/// Catálogo ilegível.
pub async fn all_movies(config: &Config, store: &Store, apply: bool) -> Result<Vec<MovieCheck>> {
    let mut out = Vec::new();
    for entry in store.movies().await? {
        let filme = crate::events::label(&entry.movie.title, entry.movie.year);
        match movie(config, store, &entry, apply).await {
            Ok(check) if check.any() || !check.nao_reconhecidos.is_empty() => {
                out.push(MovieCheck {
                    filme_id: entry.id,
                    filme,
                    check,
                    aviso: None,
                });
            }
            Ok(_) => {}
            Err(_) if entry.movie.file.is_none() => {}
            Err(error) => out.push(MovieCheck {
                filme_id: entry.id,
                filme,
                check: Check::default(),
                aviso: Some(format!("{error:#}")),
            }),
        }
    }
    Ok(out)
}

/// A marca de mudança de uma pasta de série: o `mtime` mais novo entre ela
/// e as pastas logo abaixo (as de temporada). Bloqueia.
#[must_use]
pub fn folder_stamp(root: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(root).ok()?.modified().ok()?;
    for item in std::fs::read_dir(root).ok()?.flatten() {
        if let Ok(meta) = item.metadata()
            && meta.is_dir()
            && let Ok(modified) = meta.modified()
        {
            newest = newest.max(modified);
        }
    }
    Some(newest)
}

/// Uma volta automática, sem aplicar remoção: liga os vídeos novos sem
/// ambiguidade e as legendas soltas, só nas séries cuja pasta mudou desde a
/// última volta (`stamps`, guardado por quem chama). Devolve quantos
/// arquivos ligou.
///
/// # Errors
///
/// Catálogo ilegível. Falha numa série fica no log; as outras seguem.
pub async fn new_files(
    config: &Config,
    store: &Store,
    stamps: &mut HashMap<i64, SystemTime>,
) -> Result<usize> {
    let map = config.path_map();
    let mut linked = 0;
    for entry in store.series_list().await? {
        let Ok(root) = map.to_host(Path::new(&entry.series.path)) else {
            continue;
        };
        let stamp = tokio::task::spawn_blocking(move || folder_stamp(&root)).await?;
        let Some(stamp) = stamp else {
            continue;
        };
        if stamps.get(&entry.id) == Some(&stamp) {
            continue;
        }
        match series(config, store, entry.id, true, Mode::AUTOMATIC).await {
            Ok(check) => {
                stamps.insert(entry.id, stamp);
                for new in check.novos.iter().filter(|n| n.feito) {
                    tracing::info!(
                        serie = entry.series.title,
                        arquivo = new.arquivo,
                        "verificar disco: ligado"
                    );
                    linked += 1;
                }
                linked += check.legendas.iter().filter(|s| s.feito).count();
            }
            Err(error) => tracing::warn!(serie = entry.series.title, "verificar disco: {error:#}"),
        }
    }
    Ok(linked)
}

#[cfg(test)]
mod tests {
    use acervo_parser::{Quality, QualityModel, Revision};
    use acervo_store::{CatalogEpisode, CatalogEpisodeFile, Episode, Series, Skip};

    use super::*;

    fn episode(
        id: i64,
        season: u16,
        number: u16,
        file_id: Option<i64>,
        skip: Option<Skip>,
    ) -> CatalogEpisode {
        CatalogEpisode {
            id,
            episode: Episode {
                season,
                number,
                tmdb_id: None,
                title: None,
                air_date: None,
                overview: None,
                runtime: 0,
            },
            skip,
            skipped_at: None,
            file_id,
        }
    }

    fn entry() -> CatalogSeries {
        CatalogSeries {
            id: 1,
            series: Series {
                tmdb_id: 1,
                tvdb_id: None,
                imdb_id: None,
                title: "Show".into(),
                original_title: None,
                metadata_title: None,
                original_language: None,
                year: None,
                status: None,
                overview: None,
                network: None,
                runtime: 0,
                poster: None,
                fanart: None,
                path: "/series/Show".into(),
                season_folder: true,
                monitor_new: true,
                added: None,
                refreshed_at: None,
                alternate_titles: Vec::new(),
            },
            episodes: vec![
                episode(11, 1, 1, Some(100), None),
                episode(12, 1, 2, None, None),
                episode(13, 1, 3, None, Some(Skip::Deleted)),
                episode(14, 1, 4, None, None),
                episode(15, 1, 5, Some(101), Some(Skip::Unwanted)),
            ],
            files: vec![
                CatalogEpisodeFile {
                    id: 100,
                    file: file("Season 1/Show - S01E01.mkv"),
                },
                CatalogEpisodeFile {
                    id: 101,
                    file: file("Season 1/Show - S01E05.mkv"),
                },
            ],
            priority: false,
            scene: Vec::new(),
            subtitles: Vec::new(),
        }
    }

    fn file(path: &str) -> EpisodeFile {
        EpisodeFile {
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
        }
    }

    fn disk(path: &str) -> OnDisk {
        OnDisk {
            relative: path.into(),
            size: 10,
        }
    }

    #[test]
    fn liga_o_que_casa_sem_ambiguidade_e_tira_o_que_sumiu() {
        let folder = Folder {
            videos: vec![
                disk("Season 1/Show - S01E01.mkv"),
                // Novo, sem arquivo: liga.
                disk("Season 1/Show.S01E02.1080p.WEB-DL.mkv"),
                // Dispensado também pode receber o arquivo que o usuário pôs.
                disk("Season 1/Show.S01E03.720p.HDTV.mkv"),
                // Dois para o mesmo episódio: nenhum.
                disk("Season 1/Show.S01E04.720p.mkv"),
                disk("extra/Show.S01E04.1080p.mkv"),
                // Episódio que já tem arquivo.
                disk("Show.S01E01.REPACK.mkv"),
                // Sem número.
                disk("bastidores.mkv"),
                // Fora do catálogo.
                disk("Show.S02E01.mkv"),
            ],
            subtitles: vec![
                disk("Season 1/Show - S01E01.pt-BR.srt"),
                disk("Season 1/Show.S01E02.1080p.WEB-DL.eng.srt"),
                disk("Season 1/sozinha.srt"),
            ],
            missing: HashSet::from(["Season 1/Show - S01E05.mkv".to_owned()]),
        };
        let check = series_plan(&entry(), &folder, Mode::MANUAL);
        let novos: Vec<(&str, &[i64], bool)> = check
            .novos
            .iter()
            .map(|n| {
                (
                    n.arquivo.as_str(),
                    n.episodios.as_slice(),
                    n.estava_dispensado,
                )
            })
            .collect();
        assert_eq!(
            novos,
            [
                ("Season 1/Show.S01E02.1080p.WEB-DL.mkv", &[12][..], false),
                ("Season 1/Show.S01E03.720p.HDTV.mkv", &[13][..], true),
            ]
        );
        assert_eq!(check.novos[0].qualidade, "WEBDL-1080p");
        let unknown: Vec<&str> = check
            .nao_reconhecidos
            .iter()
            .map(|u| u.arquivo.as_str())
            .collect();
        assert_eq!(
            unknown,
            [
                "Show.S01E01.REPACK.mkv",
                "bastidores.mkv",
                "Show.S02E01.mkv",
                "Season 1/Show.S01E04.720p.mkv",
                "extra/Show.S01E04.1080p.mkv",
            ]
        );
        assert_eq!(check.sumidos.len(), 1);
        assert_eq!(check.sumidos[0].arquivo_id, 101);
        assert_eq!(check.sumidos[0].episodios, [15]);
        // Dispensado não volta à busca por perder o arquivo.
        assert!(!check.sumidos[0].volta_a_busca);
        let subs: Vec<(&str, &str, Option<&str>)> = check
            .legendas
            .iter()
            .map(|s| (s.arquivo.as_str(), s.video.as_str(), s.idioma))
            .collect();
        assert_eq!(
            subs,
            [
                (
                    "Season 1/Show - S01E01.pt-BR.srt",
                    "Season 1/Show - S01E01.mkv",
                    Some("pt-BR")
                ),
                (
                    "Season 1/Show.S01E02.1080p.WEB-DL.eng.srt",
                    "Season 1/Show.S01E02.1080p.WEB-DL.mkv",
                    Some("en")
                ),
            ]
        );
    }

    #[test]
    fn volta_automatica_nao_liga_dispensado_nem_tira_e_cena_nao_traduz() {
        let folder = Folder {
            videos: vec![
                disk("Season 1/Show.S01E02.1080p.WEB-DL.mkv"),
                disk("Season 1/Show.S01E03.720p.HDTV.mkv"),
            ],
            subtitles: Vec::new(),
            missing: HashSet::from(["Season 1/Show - S01E05.mkv".to_owned()]),
        };
        let check = series_plan(&entry(), &folder, Mode::AUTOMATIC);
        assert_eq!(check.novos.len(), 1);
        assert_eq!(check.novos[0].episodios, [12]);
        assert!(check.nao_reconhecidos[0].motivo.contains("dispensado"));
        assert!(check.sumidos.is_empty());

        // O arquivo da biblioteca já está na numeração do catálogo: o mapa de
        // cena (S01E02 de cena é o E04) não vale aqui.
        let mut mapped = entry();
        mapped.scene = vec![acervo_store::SceneMapping {
            scene_season: 1,
            scene_episode: 2,
            season: 1,
            episode: 4,
        }];
        let check = series_plan(&mapped, &folder, Mode::AUTOMATIC);
        assert_eq!(check.novos[0].episodios, [12]);
    }

    #[test]
    fn legenda_do_arquivo_sumido_volta_a_ser_solta() {
        let mut series = entry();
        series.subtitles = vec![acervo_store::CatalogSubtitle {
            id: 1,
            owner: 101,
            subtitle: Subtitle {
                relative_path: "Season 1/Show - S01E05.pt-BR.srt".into(),
                language: Some("pt-BR".into()),
                forced: false,
                origin: acervo_store::SubtitleOrigin::Import,
            },
        }];
        // O vídeo some do catálogo e outro, de mesmo stem, entra no lugar:
        // a legenda é registrada de novo, para ele.
        let folder = Folder {
            videos: vec![disk("Season 1/Show - S01E05.mp4")],
            subtitles: vec![disk("Season 1/Show - S01E05.pt-BR.srt")],
            missing: HashSet::from(["Season 1/Show - S01E05.mkv".to_owned()]),
        };
        let check = series_plan(&series, &folder, Mode::MANUAL);
        assert_eq!(check.sumidos.len(), 1);
        assert_eq!(check.legendas.len(), 1);
        assert_eq!(check.legendas[0].video, "Season 1/Show - S01E05.mp4");
        // Sem tirar o sumido, a legenda ainda é dele.
        let check = series_plan(&series, &folder, Mode::AUTOMATIC);
        assert!(check.legendas.is_empty());

        // No filme: o arquivo sumido leva as legendas; a que está no disco
        // volta como solta do vídeo que entra.
        let mut film = movie(Some("Filme.mkv"));
        film.subtitles = vec![acervo_store::CatalogSubtitle {
            id: 2,
            owner: 5,
            subtitle: Subtitle {
                relative_path: "Filme.2020.1080p.pt-BR.srt".into(),
                language: Some("pt-BR".into()),
                forced: false,
                origin: acervo_store::SubtitleOrigin::Import,
            },
        }];
        let folder = Folder {
            videos: vec![disk("Filme.2020.1080p.mkv")],
            subtitles: vec![disk("Filme.2020.1080p.pt-BR.srt")],
            missing: HashSet::from(["Filme.mkv".to_owned()]),
        };
        let check = movie_plan(&film, &folder);
        assert_eq!(check.sumidos.len(), 1);
        assert_eq!(check.legendas.len(), 1);
    }

    fn movie(file: Option<&str>) -> CatalogMovie {
        CatalogMovie {
            id: 5,
            movie: acervo_store::Movie {
                tmdb_id: 5,
                imdb_id: None,
                title: "Filme".into(),
                original_title: None,
                original_language: None,
                year: Some(2020),
                status: None,
                monitored: true,
                path: "/filmes/Filme (2020)".into(),
                added: None,
                file: file.map(|path| MovieFile {
                    relative_path: path.into(),
                    size: 1,
                    quality: QualityModel {
                        quality: Quality::WebDl1080p,
                        revision: Revision::default(),
                    },
                    languages: Vec::new(),
                    release_group: None,
                    edition: None,
                    scene_name: None,
                    date_added: None,
                }),
                runtime: 0,
                secondary_year: None,
                clean_title: None,
                alternate_titles: Vec::new(),
                in_cinemas: None,
                digital_release: None,
                physical_release: None,
                overview: None,
            },
            extras: acervo_store::MovieExtras::default(),
            priority: false,
            subtitles: Vec::new(),
        }
    }

    #[test]
    fn filme_liga_o_maior_video_e_tira_o_que_sumiu() {
        let videos = vec![
            OnDisk {
                relative: "Filme.2020.1080p.BluRay.mkv".into(),
                size: 9,
            },
            OnDisk {
                relative: "Filme.2020.720p.WEB-DL.mkv".into(),
                size: 5,
            },
        ];
        let subtitles = vec![disk("Filme.2020.1080p.BluRay.por.srt")];
        // Sem arquivo no catálogo: o maior entra, com a legenda dele.
        let folder = Folder {
            videos: videos.clone(),
            subtitles: subtitles.clone(),
            missing: HashSet::new(),
        };
        let check = movie_plan(&movie(None), &folder);
        assert_eq!(check.novos.len(), 1);
        assert_eq!(check.novos[0].arquivo, "Filme.2020.1080p.BluRay.mkv");
        assert_eq!(check.novos[0].qualidade, "Bluray-1080p");
        assert_eq!(check.nao_reconhecidos.len(), 1);
        assert_eq!(check.legendas.len(), 1);
        assert_eq!(check.legendas[0].idioma, Some("pt"));
        assert!(check.sumidos.is_empty());
        // Com o arquivo no lugar: nada a fazer.
        let folder = Folder {
            videos: videos.clone(),
            subtitles: Vec::new(),
            missing: HashSet::new(),
        };
        assert!(!movie_plan(&movie(Some("Filme.2020.720p.WEB-DL.mkv")), &folder).any());
        // O do catálogo sumiu: sai, e o que está na pasta entra no lugar.
        let folder = Folder {
            videos,
            subtitles: Vec::new(),
            missing: HashSet::from(["antigo.mkv".to_owned()]),
        };
        let check = movie_plan(&movie(Some("antigo.mkv")), &folder);
        assert_eq!(check.sumidos.len(), 1);
        assert!(check.sumidos[0].volta_a_busca);
        assert_eq!(check.novos[0].arquivo, "Filme.2020.1080p.BluRay.mkv");
    }

    #[test]
    fn varredura_le_a_pasta_sem_tocar_e_pasta_ausente_e_erro() {
        let root = std::env::temp_dir().join(format!("acervo-verificar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Season 1/Sample")).unwrap();
        std::fs::write(root.join("Season 1/a.mkv"), b"12345").unwrap();
        std::fs::write(root.join("Season 1/a.pt-BR.srt"), b"1").unwrap();
        std::fs::write(root.join("Season 1/Sample/a.mkv"), b"1").unwrap();
        std::fs::write(root.join("Season 1/a.nfo"), b"1").unwrap();
        let folder = scan(&root, &["Season 1/a.mkv".into(), "Season 1/b.mkv".into()]).unwrap();
        assert_eq!(
            folder.videos,
            [OnDisk {
                relative: "Season 1/a.mkv".into(),
                size: 5
            }]
        );
        assert_eq!(folder.subtitles.len(), 1);
        assert_eq!(folder.missing, HashSet::from(["Season 1/b.mkv".to_owned()]));
        assert!(folder_stamp(&root).is_some());
        assert!(scan(&root.join("nada"), &[]).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

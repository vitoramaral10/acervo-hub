//! Conferência do disco (tarefa `disco`): o arquivo que o catálogo diz estar
//! na biblioteca e sumiu do disco — disco apagado, pasta removida à mão —
//! volta pelo mesmo torrent, se ele ainda está no cliente. O grab que o
//! trouxe volta a "baixando", o torrent volta à fila do acervo e é
//! verificado de novo; o que falta, ele baixa, e a importação liga como da
//! primeira vez. Sem o torrent, o registro só sai do catálogo, e a busca dos
//! que faltam o pega.
//!
//! **Nunca apaga torrent nem arquivo do disco.** Só `NotFound` conta como
//! sumido; outro erro de leitura é dúvida, e o arquivo fica. Raiz da
//! biblioteca ausente é disco desmontado, não "tudo sumiu": nada é feito
//! naquele tipo (filmes ou séries). Na hora agendada, sumiço em massa (mais
//! da metade, e ao menos [`MASS_MIN`]) também para; só "rodar agora"
//! confirma o reparo.
//!
//! Num pacote de temporada, só voltam os episódios cujo arquivo o catálogo
//! tinha e sumiu: o grab passa a querer só esses, e os outros arquivos do
//! torrent — episódio que está no disco, assistido ou dispensado — ficam com
//! prioridade zero.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use acervo_clients::QbitClient;
use acervo_store::{
    CatalogEpisodeFile, CatalogMovie, CatalogSeries, Grab, GrabState, SeriesGrab, Store,
};
use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::events::{self, Event, Kind};
use crate::grab::{QUEUE_TAG, qbit, watch};
use crate::tasks::Trigger;

/// Abaixo disto, sumiço nenhum é "em massa": numa biblioteca pequena, metade
/// pode ser só um ou dois arquivos.
pub const MASS_MIN: usize = 5;

/// A mensagem do grab que volta a baixar.
const REDOWNLOAD: &str = "o arquivo sumiu do disco; baixando de novo o mesmo torrent";

/// A mensagem do registro que sai do catálogo sem torrent.
const FORGOTTEN: &str = "o arquivo sumiu do disco e o torrent não está no cliente; volta à busca";

/// O que o disco disse de um arquivo do catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    /// `NotFound`: sumiu.
    Missing,
    /// Outro erro de leitura, ou caminho fora do mapa: dúvida, fica.
    Unknown,
}

/// Um arquivo do catálogo, para a decisão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub presence: Presence,
    /// Os hashes dos grabs importados que podem tê-lo trazido, do preferido
    /// ao último.
    pub torrents: Vec<String>,
}

/// O que fazer com um arquivo sumido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// O torrent ainda está no cliente: volta a baixar, pelo hash.
    Redownload(String),
    /// Sem torrent: o registro sai do catálogo, e a busca o pega.
    Forget,
}

/// Por que a conferência não fez nada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Uma raiz da biblioteca não existe: disco desmontado?
    NoRoot,
    /// Sumiço em massa na hora agendada.
    MassLoss { missing: usize, total: usize },
}

/// A decisão de uma conferência.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub refused: Option<Refusal>,
    /// Para cada arquivo sumido (o índice em `records`), o que fazer.
    pub actions: Vec<(usize, Action)>,
    pub missing: usize,
    pub unknown: usize,
}

/// A decisão, sem IO. `roots_present`: toda raiz da biblioteca deste tipo
/// existe; `client`, os hashes que o cliente tem, em minúsculas. Raiz
/// ausente recusa sempre; sumiço em massa, só na hora agendada.
#[must_use]
pub fn plan(
    roots_present: bool,
    records: &[Record],
    client: &HashSet<String>,
    trigger: Trigger,
) -> Plan {
    let missing: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.presence == Presence::Missing)
        .map(|(i, _)| i)
        .collect();
    let mut plan = Plan {
        missing: missing.len(),
        unknown: records
            .iter()
            .filter(|r| r.presence == Presence::Unknown)
            .count(),
        ..Plan::default()
    };
    if records.is_empty() {
        return plan;
    }
    if !roots_present {
        plan.refused = Some(Refusal::NoRoot);
        return plan;
    }
    let total = records.len();
    if trigger == Trigger::Scheduled && missing.len() >= MASS_MIN && missing.len() * 2 > total {
        plan.refused = Some(Refusal::MassLoss {
            missing: missing.len(),
            total,
        });
        return plan;
    }
    plan.actions = missing
        .into_iter()
        .map(|i| {
            let action = records[i]
                .torrents
                .iter()
                .find(|hash| client.contains(&hash.to_ascii_lowercase()))
                .map_or(Action::Forget, |hash| Action::Redownload(hash.clone()));
            (i, action)
        })
        .collect();
    plan
}

/// A raiz da biblioteca de uma pasta de obra (como o cliente vê): a raiz
/// configurada mais longa que a contém; fora de todas, a pasta de cima.
fn root_of(folder: &str, roots: &[&str]) -> PathBuf {
    let folder = Path::new(folder);
    roots
        .iter()
        .map(Path::new)
        .filter(|root| !root.as_os_str().is_empty() && folder.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
        .map_or_else(
            || folder.parent().unwrap_or(folder).to_path_buf(),
            Path::to_path_buf,
        )
}

/// O que o disco diz de um caminho no host. Bloqueia.
fn presence(path: &Path) -> Presence {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Presence::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Presence::Missing,
        Err(_) => Presence::Unknown,
    }
}

/// Lê, no host, as raízes e os arquivos. `None` é caminho fora do mapa, que
/// vira [`Presence::Unknown`]. Devolve se todas as raízes existem (como
/// pasta) e a presença de cada arquivo, na ordem.
///
/// # Errors
///
/// A leitura não terminou (tarefa de bloqueio abortada).
async fn read_disk(
    roots: Vec<PathBuf>,
    files: Vec<Option<PathBuf>>,
) -> Result<(bool, Vec<Presence>)> {
    let (absent, seen) = tokio::task::spawn_blocking(move || {
        let absent: Vec<PathBuf> = roots
            .into_iter()
            .filter(|root| !std::fs::metadata(root).is_ok_and(|m| m.is_dir()))
            .collect();
        let seen: Vec<Presence> = files
            .iter()
            .map(|file| file.as_deref().map_or(Presence::Unknown, presence))
            .collect();
        (absent, seen)
    })
    .await
    .context("lendo o disco")?;
    for root in &absent {
        tracing::warn!(
            raiz = %root.display(),
            "conferência do disco: a raiz da biblioteca não existe; disco desmontado?"
        );
    }
    Ok((absent.is_empty(), seen))
}

/// Uma linha do relatório: um arquivo sumido e o que se fez.
#[derive(Debug, Clone, Serialize)]
pub struct Line {
    /// "Filme (2020)" ou "Série S01E02".
    pub item: String,
    /// Relativo à pasta da obra.
    pub arquivo: String,
    /// `baixando` (o mesmo torrent de novo), `sem_torrent` (volta à busca)
    /// ou `erro`.
    pub estado: &'static str,
    pub release: Option<String>,
    pub erro: Option<String>,
}

/// A conferência de um tipo (filmes ou séries).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub conferidos: usize,
    pub ilegiveis: usize,
    /// Por que nada foi feito, se foi o caso.
    pub recusado: Option<String>,
    pub sumidos: Vec<Line>,
}

impl Report {
    fn refuse(&mut self, refusal: &Refusal, missing: usize) {
        let why = match refusal {
            Refusal::NoRoot => {
                format!(
                    "{missing} sumidos, mas a raiz da biblioteca não existe (disco desmontado?); nada feito"
                )
            }
            Refusal::MassLoss { missing, total } => format!(
                "{missing} de {total} sumidos: o disco pode estar desmontado; nada feito — \
                 rodar a tarefa à mão confirma o reparo"
            ),
        };
        tracing::warn!(motivo = why, "conferência do disco recusada");
        self.recusado = Some(why);
    }

    /// Uma linha, como as das outras tarefas: "3 sumidos: 2 baixando de
    /// novo, 1 sem torrent".
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = if let Some(why) = &self.recusado {
            why.clone()
        } else if self.sumidos.is_empty() {
            match self.conferidos {
                0 => "nenhum arquivo no catálogo".to_owned(),
                1 => "1 conferido, nada sumiu".to_owned(),
                n => format!("{n} conferidos, nada sumiu"),
            }
        } else {
            let by = |estado: &str| self.sumidos.iter().filter(|l| l.estado == estado).count();
            let parts: Vec<String> = [
                (by("baixando"), "baixando de novo"),
                (by("sem_torrent"), "sem torrent"),
                (by("erro"), "com erro"),
            ]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
            let gone = if self.sumidos.len() == 1 {
                "1 sumido".to_owned()
            } else {
                format!("{} sumidos", self.sumidos.len())
            };
            format!("{gone}: {}", parts.join(", "))
        };
        if self.ilegiveis > 0 {
            summary = format!("{summary}; {} ilegíveis", self.ilegiveis);
        }
        summary
    }

    /// Nada recusado e nada com erro.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.recusado.is_none() && self.sumidos.iter().all(|l| l.estado != "erro")
    }
}

/// O relatório da tarefa.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HealReport {
    pub filmes: Report,
    pub series: Report,
}

/// Uma conferência: filmes, depois séries. O que voltou a baixar entra na
/// fila do acervo, que o inicia quando cabe.
///
/// # Errors
///
/// Cliente inalcançável ou catálogo ilegível.
pub async fn run(config: &Config, store: &Store, trigger: Trigger) -> Result<HealReport> {
    let client = qbit(config).await?;
    let hashes: HashSet<String> = client
        .torrents()
        .await?
        .into_iter()
        .map(|t| t.hash.to_ascii_lowercase())
        .collect();
    let report = HealReport {
        filmes: movies(config, store, &client, &hashes, trigger).await?,
        series: series(config, store, &client, &hashes, trigger).await?,
    };
    let again = report
        .filmes
        .sumidos
        .iter()
        .chain(&report.series.sumidos)
        .any(|l| l.estado == "baixando");
    if again && let Err(error) = crate::grab::start_queued(config, store, &client).await {
        tracing::warn!("fila de downloads: {error:#}");
    }
    Ok(report)
}

/// Os grabs importados de um filme, do preferido ao último: o que trouxe o
/// arquivo de agora (o nome do release é o `scene_name` dele) primeiro, e
/// depois do mais novo ao mais velho.
fn movie_torrents<'a>(entry: &CatalogMovie, grabs: &'a [Grab]) -> Vec<&'a Grab> {
    let scene = entry
        .movie
        .file
        .as_ref()
        .and_then(|f| f.scene_name.as_deref());
    let mut found: Vec<&Grab> = grabs
        .iter()
        .filter(|g| g.movie_id == entry.id && g.state == GrabState::Imported)
        .collect();
    // Estável: dentro de cada grupo, a ordem do banco (mais novo primeiro).
    found.sort_by_key(|g| Some(g.title.as_str()) != scene);
    found
}

/// Volta o torrent à fila do acervo, parado, e manda verificá-lo: o que
/// sumiu do disco deixa de contar como baixado. A fila o inicia quando
/// couber.
async fn requeue(client: &QbitClient, hash: &str) -> Result<()> {
    client.stop(&[hash]).await?;
    client.add_tag(&[hash], QUEUE_TAG).await?;
    client.recheck(&[hash]).await?;
    // Um grab novo do mesmo torrent: o acompanhamento recomeça, com a
    // verificação já pedida.
    let mut watch = watch();
    watch.forget(hash);
    watch.mark_rechecked(hash);
    Ok(())
}

/// As legendas do filme que ainda estão no disco, para voltarem ao catálogo
/// depois de o arquivo sair (sair leva as legendas junto).
async fn surviving_subtitles(
    map: &acervo_fs::PathMap,
    entry: &CatalogMovie,
) -> Vec<acervo_store::Subtitle> {
    let candidates: Vec<(acervo_store::Subtitle, PathBuf)> = entry
        .subtitles
        .iter()
        .filter_map(|s| {
            map.to_host(&PathBuf::from(&entry.movie.path).join(&s.subtitle.relative_path))
                .ok()
                .map(|host| (s.subtitle.clone(), host))
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|(_, host)| presence(host) == Presence::Present)
            .map(|(subtitle, _)| subtitle)
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Tira o arquivo do filme do catálogo, ficando as legendas que ainda estão
/// no disco.
async fn forget_movie_file(
    store: &Store,
    map: &acervo_fs::PathMap,
    entry: &CatalogMovie,
) -> Result<()> {
    let kept = surviving_subtitles(map, entry).await;
    store.set_movie_file(entry.id, None).await?;
    for subtitle in &kept {
        if let Err(error) = store.add_movie_subtitle(entry.id, subtitle).await {
            tracing::warn!(
                legenda = subtitle.relative_path,
                "legenda fora do catálogo: {error}"
            );
        }
    }
    Ok(())
}

/// Os filmes: cada um com arquivo no catálogo e sem download em andamento
/// (esse vai trazer o arquivo de qualquer jeito).
async fn movies(
    config: &Config,
    store: &Store,
    client: &QbitClient,
    hashes: &HashSet<String>,
    trigger: Trigger,
) -> Result<Report> {
    let map = config.path_map();
    let grabs = store.grabs().await?;
    let busy: HashSet<i64> = grabs
        .iter()
        .filter(|g| g.state == GrabState::Downloading)
        .map(|g| g.movie_id)
        .collect();
    let entries: Vec<CatalogMovie> = store
        .movies()
        .await?
        .into_iter()
        .filter(|m| m.movie.file.is_some() && !busy.contains(&m.id))
        .collect();
    let configured: Vec<&str> = config
        .library
        .root_folders
        .iter()
        .map(String::as_str)
        .collect();
    let mut roots: Vec<PathBuf> = Vec::new();
    for entry in &entries {
        match map.to_host(&root_of(&entry.movie.path, &configured)) {
            Ok(root) if !roots.contains(&root) => roots.push(root),
            Ok(_) => {}
            Err(error) => tracing::warn!(filme = entry.movie.title, "raiz fora do mapa: {error}"),
        }
    }
    let files: Vec<Option<PathBuf>> = entries
        .iter()
        .map(|entry| {
            let file = entry.movie.file.as_ref()?;
            map.to_host(&PathBuf::from(&entry.movie.path).join(&file.relative_path))
                .inspect_err(|error| {
                    tracing::warn!(filme = entry.movie.title, "arquivo fora do mapa: {error}");
                })
                .ok()
        })
        .collect();
    let (roots_present, seen) = read_disk(roots, files).await?;
    let candidates: Vec<Vec<&Grab>> = entries.iter().map(|e| movie_torrents(e, &grabs)).collect();
    let records: Vec<Record> = seen
        .iter()
        .zip(&candidates)
        .map(|(presence, grabs)| Record {
            presence: *presence,
            torrents: grabs.iter().map(|g| g.hash.clone()).collect(),
        })
        .collect();
    let decided = plan(roots_present, &records, hashes, trigger);
    let mut report = Report {
        conferidos: records.len(),
        ilegiveis: decided.unknown,
        ..Report::default()
    };
    if let Some(refusal) = &decided.refused {
        report.refuse(refusal, decided.missing);
        return Ok(report);
    }
    for (index, action) in decided.actions {
        let grab = match &action {
            Action::Redownload(hash) => candidates[index].iter().find(|g| &g.hash == hash).copied(),
            Action::Forget => None,
        };
        let line = movie_line(store, client, &map, &entries[index], grab).await;
        report.sumidos.push(line);
    }
    Ok(report)
}

/// Um filme sumido: volta pelo torrent de `grab` ou, sem ele, sai do
/// catálogo. A linha diz o que se fez; erro fica nela.
async fn movie_line(
    store: &Store,
    client: &QbitClient,
    map: &acervo_fs::PathMap,
    entry: &CatalogMovie,
    grab: Option<&Grab>,
) -> Line {
    let mut line = Line {
        item: events::label(&entry.movie.title, entry.movie.year),
        arquivo: entry
            .movie
            .file
            .as_ref()
            .map(|f| f.relative_path.clone())
            .unwrap_or_default(),
        estado: if grab.is_some() {
            "baixando"
        } else {
            "sem_torrent"
        },
        release: grab.map(|g| g.title.clone()),
        erro: None,
    };
    let done = match grab {
        Some(grab) => heal_movie(store, client, map, entry, grab).await,
        None => forget_movie(store, map, entry).await,
    };
    if let Err(error) = done {
        tracing::warn!(filme = line.item, "conferência do disco: {error:#}");
        line.estado = "erro";
        line.erro = Some(format!("{error:#}"));
    } else {
        tracing::info!(
            filme = line.item,
            arquivo = line.arquivo,
            estado = line.estado,
            release = line.release,
            "arquivo sumido do disco"
        );
    }
    line
}

/// O filme volta pelo mesmo torrent: primeiro o banco, depois o cliente.
/// No banco, o arquivo sai do catálogo antes de o grab voltar a "baixando":
/// grab em andamento num filme com outro arquivo é, para a importação, grab
/// que perdeu o motivo — e o torrent dele sairia do cliente. E o cliente por
/// último: o torrent nunca fica na fila do acervo sem grab que o reclame
/// (torrent assim a fila apagaria).
async fn heal_movie(
    store: &Store,
    client: &QbitClient,
    map: &acervo_fs::PathMap,
    entry: &CatalogMovie,
    grab: &Grab,
) -> Result<()> {
    forget_movie_file(store, map, entry).await?;
    store
        .record_grab(&Grab {
            state: GrabState::Downloading,
            message: Some(REDOWNLOAD.into()),
            imported_path: None,
            finished_at: None,
            // O arquivo já saiu: não há o que trocar.
            replaces: None,
            ..grab.clone()
        })
        .await?;
    requeue(client, &grab.hash).await?;
    events::record(
        store,
        Event {
            source_title: Some(grab.title.clone()),
            quality: Some(grab.quality),
            indexer: Some(grab.indexer.clone()),
            download_id: Some(grab.hash.clone()),
            message: Some(REDOWNLOAD.into()),
            poster: entry.extras.poster.clone(),
            ..Event::new(
                Kind::Grabbed,
                Some(entry.id),
                events::label(&entry.movie.title, entry.movie.year),
            )
        },
    )
    .await;
    Ok(())
}

/// Sem torrent: o arquivo só sai do catálogo, e a busca o pega.
async fn forget_movie(store: &Store, map: &acervo_fs::PathMap, entry: &CatalogMovie) -> Result<()> {
    forget_movie_file(store, map, entry).await?;
    events::record(
        store,
        Event {
            message: Some(FORGOTTEN.into()),
            poster: entry.extras.poster.clone(),
            ..Event::new(
                Kind::FileDeleted,
                Some(entry.id),
                events::label(&entry.movie.title, entry.movie.year),
            )
        },
    )
    .await;
    Ok(())
}

/// Um arquivo de série a conferir, com os episódios dele que voltariam a
/// Quero (os sem `skip`).
#[derive(Debug, Clone)]
struct SeriesFile<'a> {
    entry: &'a CatalogSeries,
    file: &'a CatalogEpisodeFile,
    episodes: Vec<i64>,
}

/// Os arquivos de série a conferir, sem IO: os que o catálogo tem e cobrem
/// algum episódio sem `skip`. Arquivo só de episódio dispensado não é
/// assunto da conferência; episódio assistido nem tem arquivo.
fn series_files(entries: &[CatalogSeries]) -> Vec<SeriesFile<'_>> {
    entries
        .iter()
        .flat_map(|entry| {
            entry.files.iter().filter_map(move |file| {
                let episodes: Vec<i64> = entry
                    .episodes
                    .iter()
                    .filter(|e| e.file_id == Some(file.id) && e.skip.is_none())
                    .map(|e| e.id)
                    .collect();
                (!episodes.is_empty()).then_some(SeriesFile {
                    entry,
                    file,
                    episodes,
                })
            })
        })
        .collect()
}

/// Os grabs importados da série que foram buscar algum episódio do
/// arquivo, do preferido ao último: o do release que deu nome ao arquivo
/// primeiro, e depois do mais novo ao mais velho.
fn series_torrents<'a>(file: &SeriesFile<'_>, grabs: &'a [SeriesGrab]) -> Vec<&'a SeriesGrab> {
    let scene = file.file.file.scene_name.as_deref();
    let mut found: Vec<&SeriesGrab> = grabs
        .iter()
        .filter(|g| {
            g.series_id == file.entry.id
                && g.state == GrabState::Imported
                && g.episode_ids.iter().any(|e| file.episodes.contains(e))
        })
        .collect();
    found.sort_by_key(|g| Some(g.title.as_str()) != scene);
    found
}

/// Um torrent de série que volta a baixar: o grab, os episódios que ele
/// passa a querer (só os dos arquivos sumidos) e esses arquivos.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeriesHeal {
    series_id: i64,
    grab_id: i64,
    hash: String,
    episodes: Vec<i64>,
    /// Índices em [`series_files`].
    files: Vec<usize>,
}

/// Junta por torrent os arquivos que voltam a baixar, sem IO: um pacote com
/// três episódios sumidos é um torrent só, querendo os três.
fn group_series(
    files: &[SeriesFile<'_>],
    actions: &[(usize, Action)],
    grabs: &[SeriesGrab],
) -> Vec<SeriesHeal> {
    let mut heals: Vec<SeriesHeal> = Vec::new();
    for (index, action) in actions {
        let Action::Redownload(hash) = action else {
            continue;
        };
        let Some(grab) = grabs.iter().find(|g| &g.hash == hash) else {
            continue;
        };
        let file = &files[*index];
        let at = if let Some(at) = heals.iter().position(|h| &h.hash == hash) {
            at
        } else {
            heals.push(SeriesHeal {
                series_id: file.entry.id,
                grab_id: grab.id,
                hash: hash.clone(),
                episodes: Vec::new(),
                files: Vec::new(),
            });
            heals.len() - 1
        };
        let heal = &mut heals[at];
        heal.files.push(*index);
        for episode in &file.episodes {
            if !heal.episodes.contains(episode) {
                heal.episodes.push(*episode);
            }
        }
    }
    for heal in &mut heals {
        heal.episodes.sort_unstable();
    }
    heals
}

/// As séries: cada arquivo do catálogo com episódio sem `skip`.
#[allow(clippy::too_many_lines)] // A sequência da conferência; dividir só espalharia.
async fn series(
    config: &Config,
    store: &Store,
    client: &QbitClient,
    hashes: &HashSet<String>,
    trigger: Trigger,
) -> Result<Report> {
    let map = config.path_map();
    let grabs = store.series_grabs().await?;
    let entries = store.series_list().await?;
    let files = series_files(&entries);
    let configured = [config.library.series_root.as_str()];
    let mut roots: Vec<PathBuf> = Vec::new();
    for entry in &entries {
        if !files.iter().any(|f| f.entry.id == entry.id) {
            continue;
        }
        match map.to_host(&root_of(&entry.series.path, &configured)) {
            Ok(root) if !roots.contains(&root) => roots.push(root),
            Ok(_) => {}
            Err(error) => tracing::warn!(serie = entry.series.title, "raiz fora do mapa: {error}"),
        }
    }
    let paths: Vec<Option<PathBuf>> = files
        .iter()
        .map(|f| {
            map.to_host(&PathBuf::from(&f.entry.series.path).join(&f.file.file.relative_path))
                .inspect_err(|error| {
                    tracing::warn!(
                        serie = f.entry.series.title,
                        "arquivo fora do mapa: {error}"
                    );
                })
                .ok()
        })
        .collect();
    let (roots_present, seen) = read_disk(roots, paths).await?;
    let records: Vec<Record> = files
        .iter()
        .zip(&seen)
        .map(|(file, presence)| Record {
            presence: *presence,
            torrents: series_torrents(file, &grabs)
                .iter()
                .map(|g| g.hash.clone())
                .collect(),
        })
        .collect();
    let decided = plan(roots_present, &records, hashes, trigger);
    let mut report = Report {
        conferidos: records.len(),
        ilegiveis: decided.unknown,
        ..Report::default()
    };
    if let Some(refusal) = &decided.refused {
        report.refuse(refusal, decided.missing);
        return Ok(report);
    }
    let line = |file: &SeriesFile<'_>, estado, release: Option<&str>| Line {
        item: crate::series::label(file.entry, &file.episodes),
        arquivo: file.file.file.relative_path.clone(),
        estado,
        release: release.map(str::to_owned),
        erro: None,
    };
    for heal in group_series(&files, &decided.actions, &grabs) {
        let Some(grab) = grabs.iter().find(|g| g.id == heal.grab_id) else {
            continue;
        };
        let Some(entry) = entries.iter().find(|e| e.id == heal.series_id) else {
            continue;
        };
        let done = heal_series(store, client, entry, grab, &heal, &files).await;
        for &index in &heal.files {
            let mut line = line(&files[index], "baixando", Some(&grab.title));
            if let Err(error) = &done {
                line.estado = "erro";
                line.erro = Some(format!("{error:#}"));
            }
            report.sumidos.push(line);
        }
        match done {
            Ok(()) => tracing::info!(
                serie = crate::series::label(entry, &heal.episodes),
                release = grab.title,
                "arquivos sumidos do disco: baixando de novo o mesmo torrent"
            ),
            Err(error) => tracing::warn!(
                serie = crate::series::label(entry, &heal.episodes),
                "conferência do disco: {error:#}"
            ),
        }
    }
    for (index, action) in &decided.actions {
        if *action != Action::Forget {
            continue;
        }
        let file = &files[*index];
        let mut line = line(file, "sem_torrent", None);
        match forget_series(store, file).await {
            Ok(()) => tracing::info!(
                serie = line.item,
                arquivo = line.arquivo,
                "arquivo sumido do disco, sem torrent: volta à busca"
            ),
            Err(error) => {
                tracing::warn!(serie = line.item, "conferência do disco: {error:#}");
                line.estado = "erro";
                line.erro = Some(format!("{error:#}"));
            }
        }
        report.sumidos.push(line);
    }
    Ok(report)
}

/// O pacote (ou avulso) volta pelo mesmo torrent, querendo só os episódios
/// dos arquivos sumidos: primeiro o banco, depois o cliente — os outros
/// arquivos do torrent com prioridade zero, e a verificação. No banco, os
/// arquivos saem do catálogo antes de o grab voltar a "baixando": com eles
/// ainda lá, a importação daria o grab por importado de novo.
async fn heal_series(
    store: &Store,
    client: &QbitClient,
    entry: &CatalogSeries,
    grab: &SeriesGrab,
    heal: &SeriesHeal,
    files: &[SeriesFile<'_>],
) -> Result<()> {
    for &index in &heal.files {
        store.delete_episode_file(files[index].file.id).await?;
    }
    // O mesmo hash de um grab que já acabou reaproveita o registro, com os
    // episódios de agora.
    store
        .record_series_grab(&SeriesGrab {
            episode_ids: heal.episodes.clone(),
            state: GrabState::Downloading,
            message: Some(REDOWNLOAD.into()),
            finished_at: None,
            ..grab.clone()
        })
        .await?;
    client.stop(&[&grab.hash]).await?;
    client.add_tag(&[&grab.hash], QUEUE_TAG).await?;
    // Do pacote, só baixa o que sumiu: episódio no disco, assistido ou
    // dispensado fica com prioridade zero. A fila escolhe de novo antes de
    // iniciar, então falha aqui só avisa.
    if let Err(error) = crate::series::grab::apply_selection(
        client,
        &grab.hash,
        entry,
        &heal.episodes,
        &grab.title,
        true,
    )
    .await
    {
        tracing::warn!(release = grab.title, "escolha de arquivos: {error:#}");
    }
    client.recheck(&[&grab.hash]).await?;
    {
        let mut watch = watch();
        watch.forget(&grab.hash);
        watch.mark_rechecked(&grab.hash);
    }
    events::record(
        store,
        Event {
            source_title: Some(grab.title.clone()),
            quality: Some(grab.quality),
            indexer: Some(grab.indexer.clone()),
            download_id: Some(grab.hash.clone()),
            message: Some(REDOWNLOAD.into()),
            poster: entry.series.poster.clone(),
            ..Event::series(
                Kind::Grabbed,
                entry.id,
                crate::series::label(entry, &heal.episodes),
                &heal.episodes,
            )
        },
    )
    .await;
    Ok(())
}

/// Sem torrent: o arquivo só sai do catálogo (as legendas dele vão junto),
/// e os episódios voltam a Quero.
async fn forget_series(store: &Store, file: &SeriesFile<'_>) -> Result<()> {
    store.delete_episode_file(file.file.id).await?;
    events::record(
        store,
        Event {
            message: Some(FORGOTTEN.into()),
            poster: file.entry.series.poster.clone(),
            ..Event::series(
                Kind::FileDeleted,
                file.entry.id,
                crate::series::label(file.entry, &file.episodes),
                &file.episodes,
            )
        },
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use acervo_parser::{Quality, QualityModel, Revision};
    use acervo_store::{CatalogEpisode, Episode, EpisodeFile, Series, Skip};

    use super::*;
    use crate::series::grab::choose_files;

    /// Os hashes do cliente, em minúsculas, como [`plan`] os compara.
    fn client_set(hashes: &[&str]) -> HashSet<String> {
        hashes.iter().map(|h| h.to_ascii_lowercase()).collect()
    }

    fn record(presence: Presence, torrents: &[&str]) -> Record {
        Record {
            presence,
            torrents: torrents.iter().map(|t| (*t).to_owned()).collect(),
        }
    }

    fn present(n: usize) -> Vec<Record> {
        (0..n).map(|_| record(Presence::Present, &["x"])).collect()
    }

    #[test]
    fn perda_parcial_volta_pelo_torrent_ou_sai_do_catalogo() {
        let mut records = present(8);
        records.push(record(Presence::Missing, &["aa"]));
        records.push(record(Presence::Missing, &["bb"]));
        let plan = plan(true, &records, &client_set(&["AA"]), Trigger::Scheduled);
        assert_eq!(plan.refused, None);
        assert_eq!(plan.missing, 2);
        assert_eq!(
            plan.actions,
            [
                (8, Action::Redownload("aa".into())),
                // O torrent não está mais no cliente: só sai do catálogo.
                (9, Action::Forget),
            ]
        );
    }

    #[test]
    fn o_preferido_fora_do_cliente_cede_ao_seguinte() {
        let records = [record(Presence::Missing, &["novo", "velho"])];
        let plan = plan(true, &records, &client_set(&["velho"]), Trigger::Scheduled);
        assert_eq!(plan.actions, [(0, Action::Redownload("velho".into()))]);
        // Nenhum no cliente.
        let plan = super::plan(true, &records, &HashSet::new(), Trigger::Scheduled);
        assert_eq!(plan.actions, [(0, Action::Forget)]);
    }

    #[test]
    fn sumico_em_massa_na_hora_agendada_nao_faz_nada() {
        let mut records = present(4);
        records.extend((0..6).map(|_| record(Presence::Missing, &["aa"])));
        let client = client_set(&["aa"]);
        let plan = plan(true, &records, &client, Trigger::Scheduled);
        assert_eq!(
            plan.refused,
            Some(Refusal::MassLoss {
                missing: 6,
                total: 10
            })
        );
        assert!(plan.actions.is_empty());
        // Metade exata não é mais da metade.
        let mut half = present(5);
        half.extend((0..5).map(|_| record(Presence::Missing, &["aa"])));
        assert_eq!(
            super::plan(true, &half, &client, Trigger::Scheduled)
                .actions
                .len(),
            5
        );
        // Biblioteca pequena: menos de cinco sumidos nunca é em massa.
        let small: Vec<Record> = (0..4).map(|_| record(Presence::Missing, &["aa"])).collect();
        let plan = super::plan(true, &small, &client, Trigger::Scheduled);
        assert_eq!(plan.refused, None);
        assert_eq!(plan.actions.len(), 4);
    }

    #[test]
    fn rodar_agora_confirma_o_reparo_em_massa() {
        let records: Vec<Record> = (0..10)
            .map(|_| record(Presence::Missing, &["aa"]))
            .collect();
        let plan = plan(true, &records, &client_set(&["aa"]), Trigger::Manual);
        assert_eq!(plan.refused, None);
        assert_eq!(plan.actions.len(), 10);
        assert!(
            plan.actions
                .iter()
                .all(|(_, a)| *a == Action::Redownload("aa".into()))
        );
    }

    #[test]
    fn raiz_ausente_recusa_ate_a_mao() {
        let records = [record(Presence::Missing, &["aa"])];
        for trigger in [Trigger::Scheduled, Trigger::Manual] {
            let plan = plan(false, &records, &client_set(&["aa"]), trigger);
            assert_eq!(plan.refused, Some(Refusal::NoRoot));
            assert!(plan.actions.is_empty());
        }
        // Sem registro nenhum, não há o que recusar.
        assert_eq!(
            plan(false, &[], &HashSet::new(), Trigger::Manual).refused,
            None
        );
    }

    #[test]
    fn ilegivel_nao_conta_como_sumido() {
        let records = [
            record(Presence::Unknown, &["aa"]),
            record(Presence::Present, &["aa"]),
        ];
        let plan = plan(true, &records, &client_set(&["aa"]), Trigger::Manual);
        assert_eq!(plan.missing, 0);
        assert_eq!(plan.unknown, 1);
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn raiz_e_a_configurada_mais_longa_ou_a_pasta_de_cima() {
        let roots = ["/media", "/media/movies", ""];
        assert_eq!(
            root_of("/media/movies/Filme (2020)", &roots),
            PathBuf::from("/media/movies")
        );
        assert_eq!(
            root_of("/outro/Filme (2020)", &roots),
            PathBuf::from("/outro")
        );
        // Prefixo de texto não é pasta: `/media/movies2` não está em
        // `/media/movies`.
        assert_eq!(
            root_of("/media/movies2/Filme", &["/media/movies"]),
            PathBuf::from("/media/movies2")
        );
    }

    #[test]
    fn resumo_conta_como_as_outras_tarefas() {
        let line = |estado| Line {
            item: "Filme (2020)".into(),
            arquivo: "Filme (2020).mkv".into(),
            estado,
            release: None,
            erro: None,
        };
        let report = Report {
            conferidos: 10,
            ilegiveis: 0,
            recusado: None,
            sumidos: vec![line("baixando"), line("baixando"), line("sem_torrent")],
        };
        assert_eq!(
            report.summary(),
            "3 sumidos: 2 baixando de novo, 1 sem torrent"
        );
        assert!(report.ok());
        let quiet = Report {
            conferidos: 10,
            ..Report::default()
        };
        assert_eq!(quiet.summary(), "10 conferidos, nada sumiu");
        let mut refused = Report::default();
        refused.refuse(
            &Refusal::MassLoss {
                missing: 8,
                total: 10,
            },
            8,
        );
        assert!(refused.summary().contains("desmontado"));
        assert!(refused.summary().contains("à mão"));
        assert!(!refused.ok());
    }

    fn episode(id: i64, number: u16, file_id: Option<i64>, skip: Option<Skip>) -> CatalogEpisode {
        CatalogEpisode {
            id,
            episode: Episode {
                season: 1,
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

    fn episode_file(id: i64, path: &str, scene: &str) -> CatalogEpisodeFile {
        CatalogEpisodeFile {
            id,
            file: EpisodeFile {
                relative_path: path.into(),
                size: 1,
                quality: QualityModel {
                    quality: Quality::WebDl1080p,
                    revision: Revision::default(),
                },
                languages: Vec::new(),
                release_group: None,
                scene_name: Some(scene.into()),
                date_added: None,
            },
        }
    }

    const PACK: &str = "Show.S01.1080p.WEB-DL";

    /// S01 inteira por um pacote: E01 assistido (sem arquivo), E02 no
    /// disco, E03 e E04 sumidos, E05 dispensado com arquivo.
    fn show() -> CatalogSeries {
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
                path: "/media/series/Show".into(),
                season_folder: true,
                monitor_new: true,
                added: None,
                refreshed_at: None,
                alternate_titles: Vec::new(),
            },
            episodes: vec![
                episode(11, 1, None, Some(Skip::Watched)),
                episode(12, 2, Some(102), None),
                episode(13, 3, Some(103), None),
                episode(14, 4, Some(104), None),
                episode(15, 5, Some(105), Some(Skip::Unwanted)),
            ],
            files: vec![
                episode_file(102, "Season 1/Show - S01E02.mkv", PACK),
                episode_file(103, "Season 1/Show - S01E03.mkv", PACK),
                episode_file(104, "Season 1/Show - S01E04.mkv", PACK),
                episode_file(105, "Season 1/Show - S01E05.mkv", PACK),
            ],
            priority: false,
            scene: Vec::new(),
            subtitles: Vec::new(),
        }
    }

    fn pack_grab(id: i64, hash: &str, state: GrabState) -> SeriesGrab {
        SeriesGrab {
            id,
            series_id: 1,
            episode_ids: vec![11, 12, 13, 14, 15],
            hash: hash.into(),
            title: PACK.into(),
            indexer: "idx".into(),
            quality: Quality::WebDl1080p,
            size: 10,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state,
            message: None,
            finished_at: Some("2026-01-02T00:00:00Z".into()),
        }
    }

    #[test]
    fn pacote_volta_so_com_os_sumidos_e_assistido_fica_de_fora() {
        let entries = [show()];
        let grabs = [pack_grab(7, "pack", GrabState::Imported)];
        let files = series_files(&entries);
        // O arquivo só de episódio dispensado (E05) não é conferido; o
        // assistido (E01) nem tem arquivo.
        let ids: Vec<i64> = files.iter().map(|f| f.file.id).collect();
        assert_eq!(ids, [102, 103, 104]);
        let presence = [Presence::Present, Presence::Missing, Presence::Missing];
        let records: Vec<Record> = files
            .iter()
            .zip(presence)
            .map(|(file, presence)| Record {
                presence,
                torrents: series_torrents(file, &grabs)
                    .iter()
                    .map(|g| g.hash.clone())
                    .collect(),
            })
            .collect();
        let plan = plan(true, &records, &client_set(&["pack"]), Trigger::Scheduled);
        let heals = group_series(&files, &plan.actions, &grabs);
        assert_eq!(
            heals,
            [SeriesHeal {
                series_id: 1,
                grab_id: 7,
                hash: "pack".into(),
                episodes: vec![13, 14],
                files: vec![1, 2],
            }]
        );

        // No cliente, o pacote só baixa E03 e E04: E01 (assistido), E02 (no
        // disco) e E05 (dispensado) ficam com prioridade zero.
        let torrent: Vec<acervo_clients::TorrentFile> = (1..=5)
            .map(|n| {
                serde_json::from_value(serde_json::json!({
                    "index": n - 1,
                    "name": format!("{PACK}/Show.S01E{n:02}.1080p.WEB-DL.mkv"),
                    "size": 100,
                }))
                .unwrap()
            })
            .collect();
        let wanted: HashSet<i64> = heals[0].episodes.iter().copied().collect();
        let choices = choose_files(
            &torrent,
            &crate::series::grab::numbers(&entries[0]),
            &wanted,
            PACK,
            &[],
        );
        let priorities: Vec<u8> = choices.iter().map(|c| c.priority).collect();
        assert_eq!(priorities, [0, 0, 1, 1, 0]);
    }

    #[test]
    fn grab_em_andamento_ou_falho_nao_e_candidato() {
        let entries = [show()];
        let files = series_files(&entries);
        let grabs = [
            pack_grab(7, "baixando", GrabState::Downloading),
            pack_grab(8, "falho", GrabState::Failed),
        ];
        assert!(series_torrents(&files[1], &grabs).is_empty());
    }

    #[test]
    fn o_grab_que_deu_nome_ao_arquivo_vem_primeiro() {
        let entries = [show()];
        let files = series_files(&entries);
        let mut other = pack_grab(9, "avulso", GrabState::Imported);
        other.title = "Show.S01E03.720p".into();
        other.episode_ids = vec![13];
        // O mais novo primeiro, como o banco devolve.
        let grabs = [other, pack_grab(7, "pack", GrabState::Imported)];
        let found: Vec<&str> = series_torrents(&files[1], &grabs)
            .iter()
            .map(|g| g.hash.as_str())
            .collect();
        assert_eq!(found, ["pack", "avulso"]);
    }
}

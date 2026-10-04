//! Séries: a obra, os episódios, os arquivos e os grabs.
//!
//! O episódio não tem "monitorado". Ele tem [`Skip`], o motivo de não ser
//! buscado. Sem arquivo e sem `skip`, é procurado assim que vai ao ar. Assim,
//! apagar ou assistir um episódio nunca o devolve à busca por engano: o
//! motivo fica gravado, e só [`Store::set_skip`] com `None` o traz de volta.

use std::collections::{HashMap, HashSet};

use acervo_parser::{QualityModel, Revision};
use deadpool_postgres::GenericClient;
use serde::Serialize;
use tokio_postgres::Row;

use crate::{
    CatalogSubtitle, GrabState, Result, Store, StoreError, narrow, quality, small, subtitle_from,
};

/// Por que um episódio não é buscado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// Nunca se quis: temporada antiga, especial, ou desmarcado na tela.
    Unwanted,
    /// O arquivo foi apagado na tela.
    Deleted,
    /// Assistido no Jellyfin, e o arquivo saiu por isso.
    Watched,
}

impl Skip {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unwanted => "unwanted",
            Self::Deleted => "deleted",
            Self::Watched => "watched",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "unwanted" => Ok(Self::Unwanted),
            "deleted" => Ok(Self::Deleted),
            "watched" => Ok(Self::Watched),
            other => Err(StoreError::Corrupt(format!("skip `{other}`"))),
        }
    }
}

/// Uma série, como a base de metadados e a tela a descrevem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub tmdb_id: u32,
    pub tvdb_id: Option<u32>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    /// Título em inglês: o que dá nome à pasta e ao arquivo.
    pub metadata_title: Option<String>,
    pub original_language: Option<String>,
    /// Ano da estreia.
    pub year: Option<u16>,
    /// `continuing`, `ended` ou `upcoming`.
    pub status: Option<String>,
    pub overview: Option<String>,
    pub network: Option<String>,
    /// Minutos por episódio; zero é desconhecido.
    pub runtime: u32,
    pub poster: Option<String>,
    pub fanart: Option<String>,
    /// Pasta da série, como o serviço a vê.
    pub path: String,
    /// Arquivos dentro de `Season N/`; sem isso, soltos na pasta da série.
    pub season_folder: bool,
    /// Episódio novo que a base anunciar entra sem `skip` (buscado); com
    /// `false`, entra como [`Skip::Unwanted`].
    pub monitor_new: bool,
    /// RFC 3339.
    pub added: Option<String>,
    /// Quando os metadados vieram da base pela última vez (RFC 3339).
    pub refreshed_at: Option<String>,
    /// Títulos alternativos e traduções, aceitos no casamento do release.
    pub alternate_titles: Vec<String>,
}

/// Um episódio, como a base de metadados o anuncia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// Zero é a dos especiais.
    pub season: u16,
    pub number: u16,
    pub tmdb_id: Option<u32>,
    pub title: Option<String>,
    /// `AAAA-MM-DD`; `None` é sem data anunciada.
    pub air_date: Option<String>,
    pub overview: Option<String>,
    /// Minutos; zero é desconhecido.
    pub runtime: u32,
}

/// Um arquivo de vídeo na pasta da série. Cobre um ou mais episódios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeFile {
    /// Relativo à pasta da série (`Season 1/…mkv`, ou solto).
    pub relative_path: String,
    pub size: u64,
    pub quality: QualityModel,
    pub languages: Vec<String>,
    pub release_group: Option<String>,
    /// Nome do release de onde o arquivo veio.
    pub scene_name: Option<String>,
    pub date_added: Option<String>,
}

/// Um episódio do catálogo: o que a base diz e o que o acervo sabe dele.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEpisode {
    pub id: i64,
    pub episode: Episode,
    pub skip: Option<Skip>,
    /// Quando o `skip` foi posto (RFC 3339).
    pub skipped_at: Option<String>,
    /// Id em [`CatalogSeries::files`].
    pub file_id: Option<i64>,
}

/// Um arquivo do catálogo, com o id local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEpisodeFile {
    pub id: i64,
    pub file: EpisodeFile,
}

/// Uma série do catálogo, inteira: episódios por temporada e número, arquivos
/// por caminho.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSeries {
    pub id: i64,
    pub series: Series,
    pub episodes: Vec<CatalogEpisode>,
    pub files: Vec<CatalogEpisodeFile>,
    /// Passa na frente na fila e na busca. Muda só por
    /// [`Store::set_series_priority`].
    pub priority: bool,
    /// Numeração de cena da série, por temporada e episódio de cena.
    pub scene: Vec<SceneMapping>,
    /// Legendas dos arquivos, com `owner` = id em [`CatalogSeries::files`].
    pub subtitles: Vec<CatalogSubtitle>,
}

/// Um par da numeração de cena (XEM): o release sai como
/// `scene_season`/`scene_episode`, e no catálogo é `season`/`episode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SceneMapping {
    pub scene_season: u16,
    pub scene_episode: u16,
    pub season: u16,
    pub episode: u16,
}

/// O que [`Store::sync_episodes`] mudou.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EpisodeSync {
    /// Episódios novos, por id.
    pub added: Vec<i64>,
    /// Episódios cujo título, data, sinopse ou duração mudou.
    pub updated: usize,
    /// Episódios que a base deixou de anunciar e saíram (os sem arquivo).
    pub removed: usize,
    /// Os que a base deixou de anunciar mas ficaram, por terem arquivo.
    pub orphaned: Vec<i64>,
}

/// Um release de série mandado ao cliente de download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesGrab {
    pub id: i64,
    pub series_id: i64,
    /// Os episódios que o grab foi buscar. Num pacote, só os que faltavam:
    /// os outros arquivos ficam com prioridade zero no cliente.
    pub episode_ids: Vec<i64>,
    /// Infohash, em hex minúsculo.
    pub hash: String,
    pub title: String,
    pub indexer: String,
    pub quality: acervo_parser::Quality,
    /// Bytes que de fato baixam (só os arquivos escolhidos).
    pub size: u64,
    /// RFC 3339, em UTC.
    pub grabbed_at: String,
    pub state: GrabState,
    pub message: Option<String>,
    pub finished_at: Option<String>,
}

impl Store {
    /// Liga ou desliga a prioridade da série. `false` se ela não existe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_series_priority(&self, id: i64, priority: bool) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE series SET priority = $2 WHERE id = $1",
                &[&id, &priority],
            )
            .await?
            > 0)
    }

    /// Troca a numeração de cena da série pela de agora, numa transação. Um
    /// par de cena pode ter vários alvos (episódio duplo); a linha repetida
    /// inteira entra uma vez só.
    ///
    /// # Errors
    ///
    /// Série inexistente ou falha de escrita.
    pub async fn set_scene_mappings(
        &self,
        series_id: i64,
        mappings: &[SceneMapping],
    ) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "DELETE FROM scene_mappings WHERE series_id = $1",
            &[&series_id],
        )
        .await?;
        let mut seen = HashSet::new();
        for m in mappings {
            if !seen.insert(*m) {
                continue;
            }
            tx.execute(
                "INSERT INTO scene_mappings (series_id, scene_season, scene_episode, season,
                     episode)
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &series_id,
                    &i32::from(m.scene_season),
                    &i32::from(m.scene_episode),
                    &i32::from(m.season),
                    &i32::from(m.episode),
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Troca só o caminho de um arquivo de episódio (renomeado no disco).
    /// `false` se o arquivo não é da série.
    ///
    /// # Errors
    ///
    /// Falha de escrita, ou caminho que já é de outro arquivo da série.
    pub async fn set_episode_file_path(
        &self,
        series_id: i64,
        file_id: i64,
        relative_path: &str,
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE episode_files SET relative_path = $3 WHERE id = $2 AND series_id = $1",
                &[&series_id, &file_id, &relative_path],
            )
            .await?
            > 0)
    }

    /// Cadastra a série com os episódios que a base anuncia, numa transação.
    /// Cada episódio entra com `skip` de [`default_skip`].
    ///
    /// # Errors
    ///
    /// Série já cadastrada (mesmo `tmdb_id`) ou falha de escrita.
    pub async fn add_series(&self, series: &Series, episodes: &[Episode]) -> Result<i64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let id = insert_series(&tx, series).await?;
        write_titles(&tx, id, series).await?;
        for episode in episodes {
            insert_episode(&tx, id, episode, default_skip(series, episode)).await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    /// Todas as séries, por título, cada uma inteira.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn series_list(&self) -> Result<Vec<CatalogSeries>> {
        read_series(&self.pool.get().await?, None).await
    }

    /// Uma série, inteira.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn series(&self, id: i64) -> Result<Option<CatalogSeries>> {
        Ok(read_series(&self.pool.get().await?, Some(id)).await?.pop())
    }

    /// Regrava só o que vem da base de metadados e os títulos alternativos,
    /// nunca o que é de quem usa (pasta, pasta de temporada, `monitor_new`,
    /// prioridade, data de entrada): a atualização parte de uma leitura que
    /// pode ter envelhecido. Não toca nos episódios. `false` se a série saiu
    /// do catálogo no meio.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn update_series_metadata(&self, id: i64, series: &Series) -> Result<bool> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let tv_id = series.tvdb_id.map(i64::from);
        let year = series.year.map(i32::from);
        let runtime = i32::try_from(series.runtime).unwrap_or(i32::MAX);
        let updated = tx
            .execute(
                "UPDATE series SET tvdb_id = $2, imdb_id = $3, title = $4, original_title = $5,
                     original_language = $6, year = $7, status = $8, overview = $9,
                     network = $10, runtime = $11, poster = $12, fanart = $13,
                     refreshed_at = $14, metadata_title = $15
                 WHERE id = $1",
                &[
                    &id,
                    &tv_id,
                    &series.imdb_id,
                    &series.title,
                    &series.original_title,
                    &series.original_language,
                    &year,
                    &series.status,
                    &series.overview,
                    &series.network,
                    &runtime,
                    &series.poster,
                    &series.fanart,
                    &series.refreshed_at,
                    &series.metadata_title,
                ],
            )
            .await?;
        if updated == 0 {
            return Ok(false);
        }
        write_titles(&tx, id, series).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Troca o que é escolha de quem usa: `monitor_new` e a pasta de
    /// temporada. `false` se a série não existe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_series_options(
        &self,
        id: i64,
        monitor_new: bool,
        season_folder: bool,
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE series SET monitor_new = $2, season_folder = $3 WHERE id = $1",
                &[&id, &monitor_new, &season_folder],
            )
            .await?
            > 0)
    }

    /// Apaga a série, e em cascata os episódios, arquivos e grabs. O
    /// histórico fica, sem o vínculo.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn delete_series(&self, id: i64) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute("DELETE FROM series WHERE id = $1", &[&id])
            .await?
            > 0)
    }

    /// Acerta os episódios com o que a base anuncia agora, numa transação:
    /// - novo: entra com `skip` de [`default_skip`];
    /// - existente: título, data, sinopse, duração e `tmdb_id` são
    ///   regravados; `skip`, `skipped_at` e `file_id` nunca mudam aqui;
    /// - sumido da base: sai só se não tem arquivo, não tem `skip` e é de
    ///   temporada que veio com episódios no anúncio. Com arquivo ou com
    ///   `skip` (a decisão do usuário), ou de temporada que não veio inteira
    ///   (404 transitório da base, temporada pulada), fica e vai para
    ///   `orphaned`.
    ///
    /// # Errors
    ///
    /// Série inexistente ou falha de escrita.
    pub async fn sync_episodes(&self, series_id: i64, episodes: &[Episode]) -> Result<EpisodeSync> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let series = read_series(&tx, Some(series_id))
            .await?
            .pop()
            .ok_or_else(|| StoreError::Corrupt(format!("série {series_id} não existe")))?
            .series;

        let rows = tx
            .query(
                &format!(
                    "SELECT id, file_id, skip IS NOT NULL, {EPISODE_FIELDS}
                     FROM episodes WHERE series_id = $1"
                ),
                &[&series_id],
            )
            .await?;
        let mut existing = HashMap::new();
        for row in &rows {
            let episode = episode_from(row, 3)?;
            let key = (episode.season, episode.number);
            existing.insert(
                key,
                (
                    row.try_get::<_, i64>(0)?,
                    // Com arquivo ou com `skip`: nunca sai por sumir da base.
                    row.try_get::<_, Option<i64>>(1)?.is_some() || row.try_get::<_, bool>(2)?,
                    episode,
                ),
            );
        }

        let mut sync = EpisodeSync::default();
        let mut announced = HashSet::new();
        for episode in episodes {
            let key = (episode.season, episode.number);
            announced.insert(key);
            let Some((id, _, before)) = existing.get(&key) else {
                let skip = default_skip(&series, episode);
                sync.added
                    .push(insert_episode(&tx, series_id, episode, skip).await?);
                continue;
            };
            if before == episode {
                continue;
            }
            tx.execute(
                "UPDATE episodes SET tmdb_id = $2, title = $3, air_date = $4, overview = $5,
                     runtime = $6
                 WHERE id = $1",
                &[
                    id,
                    &episode.tmdb_id.map(i64::from),
                    &episode.title,
                    &episode.air_date,
                    &episode.overview,
                    &i32::try_from(episode.runtime).unwrap_or(i32::MAX),
                ],
            )
            .await?;
            if (
                &before.title,
                &before.air_date,
                &before.overview,
                before.runtime,
            ) != (
                &episode.title,
                &episode.air_date,
                &episode.overview,
                episode.runtime,
            ) {
                sync.updated += 1;
            }
        }

        let seasons: HashSet<u16> = episodes.iter().map(|e| e.season).collect();
        let mut gone = Vec::new();
        for (key, (id, kept, _)) in &existing {
            if announced.contains(key) {
                continue;
            }
            if *kept || !seasons.contains(&key.0) {
                sync.orphaned.push(*id);
            } else {
                gone.push(*id);
            }
        }
        sync.orphaned.sort_unstable();
        tx.execute("DELETE FROM episodes WHERE id = ANY($1)", &[&gone])
            .await?;
        sync.removed = gone.len();
        tx.commit().await?;
        Ok(sync)
    }

    /// Põe ou tira o motivo de não buscar, em lote. `None` devolve o episódio
    /// à busca. Devolve quantos mudaram.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_skip(&self, episode_ids: &[i64], skip: Option<Skip>, at: &str) -> Result<u64> {
        let client = self.pool.get().await?;
        let skip = skip.map(Skip::as_str);
        Ok(client
            .execute(
                "UPDATE episodes
                 SET skip = $2::TEXT,
                     skipped_at = CASE WHEN $2::TEXT IS NULL THEN NULL ELSE $3::TEXT END
                 WHERE id = ANY($1) AND skip IS DISTINCT FROM $2::TEXT",
                &[&episode_ids, &skip, &at],
            )
            .await?)
    }

    /// Registra um arquivo e o liga aos episódios que ele cobre, numa
    /// transação. Se algum desses episódios já tinha outro arquivo, o
    /// vínculo passa para o novo; o arquivo velho que ficar sem episódio
    /// nenhum sai do catálogo e volta em `replaced` (o disco é com quem chama).
    ///
    /// # Errors
    ///
    /// Episódio de outra série ou falha de escrita. Caminho que já está no
    /// catálogo é o mesmo arquivo: o registro é regravado no lugar.
    pub async fn add_episode_file(
        &self,
        series_id: i64,
        file: &EpisodeFile,
        episode_ids: &[i64],
    ) -> Result<(i64, Vec<CatalogEpisodeFile>)> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let linked = link_file(&tx, series_id, file, episode_ids).await?;
        tx.commit().await?;
        Ok(linked)
    }

    /// Tira o arquivo do catálogo. Os episódios dele ficam sem arquivo; o
    /// `skip` de cada um é com quem chama. Devolve o que saiu.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn delete_episode_file(&self, file_id: i64) -> Result<Option<CatalogEpisodeFile>> {
        let client = self.pool.get().await?;
        client
            .query_opt(
                &format!("DELETE FROM episode_files WHERE id = $1 RETURNING {FILE_COLUMNS}"),
                &[&file_id],
            )
            .await?
            .as_ref()
            .map(file_from)
            .transpose()
    }

    /// Grava um grab e os episódios dele. O `id` do argumento é ignorado.
    ///
    /// # Errors
    ///
    /// Hash de um grab ainda em andamento, episódio de outra série ou falha
    /// de escrita. Hash de um grab que já acabou (importado ou falho) é
    /// reaproveitado.
    pub async fn record_series_grab(&self, grab: &SeriesGrab) -> Result<i64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        episodes_of_series(&tx, grab.series_id, &grab.episode_ids).await?;
        let id: i64 = tx
            .query_opt(
                "INSERT INTO series_grabs (series_id, hash, title, indexer, quality, size,
                     grabbed_at, state, message, finished_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (hash) DO UPDATE SET series_id = EXCLUDED.series_id,
                     title = EXCLUDED.title, indexer = EXCLUDED.indexer,
                     quality = EXCLUDED.quality, size = EXCLUDED.size,
                     grabbed_at = EXCLUDED.grabbed_at, state = EXCLUDED.state,
                     message = EXCLUDED.message, finished_at = EXCLUDED.finished_at
                 WHERE series_grabs.state <> 'downloading'
                 RETURNING id",
                &[
                    &grab.series_id,
                    &grab.hash,
                    &grab.title,
                    &grab.indexer,
                    &i16::from(grab.quality.id()),
                    &i64::try_from(grab.size).unwrap_or(i64::MAX),
                    &grab.grabbed_at,
                    &grab.state.as_str(),
                    &grab.message,
                    &grab.finished_at,
                ],
            )
            .await?
            .ok_or_else(|| {
                StoreError::Corrupt(format!("o release {} já está baixando", grab.hash))
            })?
            .try_get(0)?;
        // Hash repetido de um grab que já acabou (importado ou falho) é o
        // mesmo release pego de novo: o registro é reaproveitado, com os
        // episódios de agora.
        tx.execute(
            "DELETE FROM series_grab_episodes WHERE grab_id = $1",
            &[&id],
        )
        .await?;
        tx.execute(
            "INSERT INTO series_grab_episodes (grab_id, episode_id)
             SELECT $1, unnest($2::BIGINT[]) ON CONFLICT DO NOTHING",
            &[&id, &grab.episode_ids],
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Todos os grabs de série, do mais novo ao mais velho.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn series_grabs(&self) -> Result<Vec<SeriesGrab>> {
        let client = self.pool.get().await?;
        let mut episodes: HashMap<i64, Vec<i64>> = HashMap::new();
        for row in client
            .query(
                "SELECT grab_id, episode_id FROM series_grab_episodes ORDER BY grab_id, episode_id",
                &[],
            )
            .await?
        {
            episodes
                .entry(row.try_get(0)?)
                .or_default()
                .push(row.try_get(1)?);
        }
        client
            .query(
                "SELECT id, series_id, hash, title, indexer, quality, size, grabbed_at, state,
                        message, finished_at
                 FROM series_grabs ORDER BY grabbed_at DESC, id DESC",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                let id = row.try_get(0)?;
                Ok(SeriesGrab {
                    id,
                    series_id: row.try_get(1)?,
                    episode_ids: episodes.remove(&id).unwrap_or_default(),
                    hash: row.try_get(2)?,
                    title: row.try_get(3)?,
                    indexer: row.try_get(4)?,
                    quality: quality(row.try_get(5)?)?,
                    size: u64::try_from(row.try_get::<_, i64>(6)?).unwrap_or(0),
                    grabbed_at: row.try_get(7)?,
                    state: GrabState::parse(row.try_get(8)?)?,
                    message: row.try_get(9)?,
                    finished_at: row.try_get(10)?,
                })
            })
            .collect()
    }

    /// Muda o estado de um grab.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn update_series_grab(
        &self,
        id: i64,
        state: GrabState,
        message: Option<&str>,
        finished_at: Option<&str>,
    ) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE series_grabs SET state = $2, message = $3, finished_at = $4
                 WHERE id = $1",
                &[&id, &state.as_str(), &message, &finished_at],
            )
            .await?;
        Ok(())
    }
}

/// Um release que a busca de uma série pegou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesPick {
    pub title: String,
    pub indexer: String,
    pub quality: acervo_parser::Quality,
    pub size: u64,
    /// Os episódios que ele foi buscar.
    pub episode_ids: Vec<i64>,
}

/// Uma busca dos episódios que faltam de uma série.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesSearch {
    pub series_id: i64,
    /// RFC 3339.
    pub at: String,
    /// O que se pediu aos indexadores, como `S01` ou `S02E05`.
    pub queries: Vec<String>,
    pub releases: usize,
    pub picks: Vec<SeriesPick>,
    /// Motivo de rejeição e quantos releases ele barrou.
    pub rejections: Vec<(String, usize)>,
    pub error: Option<String>,
}

#[derive(serde::Deserialize, Serialize)]
struct StoredPick {
    title: String,
    indexer: String,
    quality: u8,
    size: u64,
    episode_ids: Vec<i64>,
}

fn json_error(what: &str) -> impl Fn(serde_json::Error) -> StoreError + '_ {
    move |e| StoreError::Corrupt(format!("{what}: {e}"))
}

impl Store {
    /// Grava uma busca de série.
    ///
    /// # Errors
    ///
    /// Falha de escrita, ou série fora do catálogo.
    pub async fn record_series_search(&self, run: &SeriesSearch) -> Result<()> {
        let picks: Vec<StoredPick> = run
            .picks
            .iter()
            .map(|p| StoredPick {
                title: p.title.clone(),
                indexer: p.indexer.clone(),
                quality: p.quality.id(),
                size: p.size,
                episode_ids: p.episode_ids.clone(),
            })
            .collect();
        let picks = serde_json::to_value(&picks).map_err(json_error("escolhidos da busca"))?;
        let queries = serde_json::to_value(&run.queries).map_err(json_error("consultas"))?;
        let rejections =
            serde_json::to_value(&run.rejections).map_err(json_error("rejeições da busca"))?;
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO series_searches (series_id, at, queries, releases, picks,
                     rejections, error)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &run.series_id,
                    &run.at,
                    &queries,
                    &i64::try_from(run.releases).unwrap_or(i64::MAX),
                    &picks,
                    &rejections,
                    &run.error,
                ],
            )
            .await?;
        Ok(())
    }

    /// A busca mais recente de cada série, da mais velha à mais nova.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn latest_series_searches(&self) -> Result<Vec<SeriesSearch>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT DISTINCT ON (series_id) series_id, at, queries, releases, picks,
                        rejections, error
                 FROM series_searches
                 ORDER BY series_id, at DESC, id DESC",
                &[],
            )
            .await?;
        let mut runs = rows
            .iter()
            .map(|row| {
                let picks: Vec<StoredPick> = serde_json::from_value(row.try_get(4)?)
                    .map_err(json_error("escolhidos da busca"))?;
                Ok(SeriesSearch {
                    series_id: row.try_get(0)?,
                    at: row.try_get(1)?,
                    queries: serde_json::from_value(row.try_get(2)?)
                        .map_err(json_error("consultas"))?,
                    releases: usize::try_from(row.try_get::<_, i64>(3)?).unwrap_or(0),
                    picks: picks
                        .into_iter()
                        .map(|p| {
                            Ok(SeriesPick {
                                title: p.title,
                                indexer: p.indexer,
                                quality: quality(i16::from(p.quality))?,
                                size: p.size,
                                episode_ids: p.episode_ids,
                            })
                        })
                        .collect::<Result<_>>()?,
                    rejections: serde_json::from_value(row.try_get(5)?)
                        .map_err(json_error("rejeições da busca"))?,
                    error: row.try_get(6)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        runs.sort_by(|a, b| a.at.cmp(&b.at));
        Ok(runs)
    }
}

/// Colunas de `episode_files`, na ordem que [`file_from`] lê.
const FILE_COLUMNS: &str = "id, relative_path, size, quality, revision_version, revision_real,
    is_repack, languages, release_group, scene_name, date_added";

/// Colunas de `episodes` que a base de metadados descreve, na ordem que
/// [`episode_from`] lê.
const EPISODE_FIELDS: &str = "season, number, tmdb_id, title, air_date, overview, runtime";

async fn insert_series(client: &impl GenericClient, series: &Series) -> Result<i64> {
    let tmdb_id = i64::from(series.tmdb_id);
    let tv_id = series.tvdb_id.map(i64::from);
    let year = series.year.map(i32::from);
    let runtime = i32::try_from(series.runtime).unwrap_or(i32::MAX);
    Ok(client
        .query_one(
            "INSERT INTO series (tmdb_id, tvdb_id, imdb_id, title, original_title,
                 original_language, year, status, overview, network, runtime, poster,
                 fanart, path, season_folder, monitor_new, added, refreshed_at,
                 metadata_title)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, $18, $19)
             RETURNING id",
            &[
                &tmdb_id,
                &tv_id,
                &series.imdb_id,
                &series.title,
                &series.original_title,
                &series.original_language,
                &year,
                &series.status,
                &series.overview,
                &series.network,
                &runtime,
                &series.poster,
                &series.fanart,
                &series.path,
                &series.season_folder,
                &series.monitor_new,
                &series.added,
                &series.refreshed_at,
                &series.metadata_title,
            ],
        )
        .await?
        .try_get(0)?)
}

/// Apaga os títulos alternativos de antes e grava os de agora.
async fn write_titles(client: &impl GenericClient, id: i64, series: &Series) -> Result<()> {
    client
        .execute("DELETE FROM series_titles WHERE series_id = $1", &[&id])
        .await?;
    for title in &series.alternate_titles {
        client
            .execute(
                "INSERT INTO series_titles (series_id, title) VALUES ($1, $2)",
                &[&id, title],
            )
            .await?;
    }
    Ok(())
}

/// O corpo de [`Store::add_episode_file`], dentro da transação de quem chama.
async fn link_file(
    tx: &impl GenericClient,
    series_id: i64,
    file: &EpisodeFile,
    episode_ids: &[i64],
) -> Result<(i64, Vec<CatalogEpisodeFile>)> {
    if episode_ids.is_empty() {
        return Err(StoreError::Corrupt("arquivo sem episódio".into()));
    }
    let linked = episodes_of_series(tx, series_id, episode_ids).await?;

    let languages = serde_json::to_value(&file.languages)
        .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?;
    let file_id: i64 = tx
        .query_one(
            "INSERT INTO episode_files (series_id, relative_path, size, quality,
                 revision_version, revision_real, is_repack, languages, release_group,
                 scene_name, date_added)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (series_id, relative_path) DO UPDATE SET
                 size = EXCLUDED.size, quality = EXCLUDED.quality,
                 revision_version = EXCLUDED.revision_version,
                 revision_real = EXCLUDED.revision_real, is_repack = EXCLUDED.is_repack,
                 languages = EXCLUDED.languages, release_group = EXCLUDED.release_group,
                 scene_name = EXCLUDED.scene_name, date_added = EXCLUDED.date_added
             RETURNING id",
            &[
                &series_id,
                &file.relative_path,
                &i64::try_from(file.size).unwrap_or(i64::MAX),
                &small(file.quality.quality.id()),
                &small(file.quality.revision.version),
                &small(file.quality.revision.real),
                &file.quality.revision.is_repack,
                &languages,
                &file.release_group,
                &file.scene_name,
                &file.date_added,
            ],
        )
        .await?
        .try_get(0)?;
    tx.execute(
        "UPDATE episodes SET file_id = $1 WHERE id = ANY($2)",
        &[&file_id, &episode_ids],
    )
    .await?;

    // Mesmo caminho é o mesmo arquivo, regravado no lugar: nunca volta em
    // `replaced`, senão quem chama apagaria do disco o arquivo novo.
    let mut old: Vec<i64> = linked
        .into_iter()
        .flatten()
        .filter(|id| *id != file_id)
        .collect();
    old.sort_unstable();
    old.dedup();
    let mut replaced = tx
        .query(
            &format!(
                "DELETE FROM episode_files f WHERE f.id = ANY($1)
                   AND NOT EXISTS (SELECT 1 FROM episodes e WHERE e.file_id = f.id)
                 RETURNING {FILE_COLUMNS}"
            ),
            &[&old],
        )
        .await?
        .iter()
        .map(file_from)
        .collect::<Result<Vec<_>>>()?;
    replaced.sort_by(|a, b| a.file.relative_path.cmp(&b.file.relative_path));
    Ok((file_id, replaced))
}

async fn insert_episode(
    client: &impl GenericClient,
    series_id: i64,
    episode: &Episode,
    skip: Option<Skip>,
) -> Result<i64> {
    Ok(client
        .query_one(
            "INSERT INTO episodes (series_id, season, number, tmdb_id, title, air_date,
                 overview, runtime, skip)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING id",
            &[
                &series_id,
                &i32::from(episode.season),
                &i32::from(episode.number),
                &episode.tmdb_id.map(i64::from),
                &episode.title,
                &episode.air_date,
                &episode.overview,
                &i32::try_from(episode.runtime).unwrap_or(i32::MAX),
                &skip.map(Skip::as_str),
            ],
        )
        .await?
        .try_get(0)?)
}

/// Confere que todos os episódios são da série e devolve o `file_id` de
/// cada um.
async fn episodes_of_series(
    client: &impl GenericClient,
    series_id: i64,
    episode_ids: &[i64],
) -> Result<Vec<Option<i64>>> {
    let wanted: HashSet<i64> = episode_ids.iter().copied().collect();
    let rows = client
        .query(
            "SELECT file_id FROM episodes WHERE series_id = $1 AND id = ANY($2)",
            &[&series_id, &episode_ids],
        )
        .await?;
    if rows.len() != wanted.len() {
        return Err(StoreError::Corrupt(format!(
            "há episódio que não é da série {series_id}"
        )));
    }
    rows.iter().map(|row| Ok(row.try_get(0)?)).collect()
}

/// O episódio da linha, com as colunas de [`EPISODE_FIELDS`] a partir de `first`.
fn episode_from(row: &Row, first: usize) -> Result<Episode> {
    let tmdb_id: Option<i64> = row.try_get(first + 2)?;
    Ok(Episode {
        season: narrow(row.try_get(first)?, "temporada")?,
        number: narrow(row.try_get(first + 1)?, "episódio")?,
        tmdb_id: tmdb_id
            .map(|id| u32::try_from(id).map_err(|_| StoreError::Corrupt(format!("tmdb {id}"))))
            .transpose()?,
        title: row.try_get(first + 3)?,
        air_date: row.try_get(first + 4)?,
        overview: row.try_get(first + 5)?,
        runtime: narrow(row.try_get(first + 6)?, "duração")?,
    })
}

/// O arquivo da linha, com as colunas de [`FILE_COLUMNS`].
fn file_from(row: &Row) -> Result<CatalogEpisodeFile> {
    let revision = |index: usize| -> Result<u8> {
        u8::try_from(row.try_get::<_, i16>(index)?)
            .map_err(|_| StoreError::Corrupt("revisão".into()))
    };
    let languages: serde_json::Value = row.try_get(7)?;
    Ok(CatalogEpisodeFile {
        id: row.try_get(0)?,
        file: EpisodeFile {
            relative_path: row.try_get(1)?,
            size: u64::try_from(row.try_get::<_, i64>(2)?).unwrap_or(0),
            quality: QualityModel {
                quality: quality(row.try_get(3)?)?,
                revision: Revision {
                    version: revision(4)?,
                    real: revision(5)?,
                    is_repack: row.try_get(6)?,
                },
            },
            languages: serde_json::from_value(languages)
                .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?,
            release_group: row.try_get(8)?,
            scene_name: row.try_get(9)?,
            date_added: row.try_get(10)?,
        },
    })
}

/// A série da linha de `read_series`, sem os títulos alternativos.
fn series_from(row: &Row) -> Result<Series> {
    let tmdb: i64 = row.try_get(1)?;
    let other_db: Option<i64> = row.try_get(2)?;
    Ok(Series {
        tmdb_id: u32::try_from(tmdb).map_err(|_| StoreError::Corrupt(format!("tmdb {tmdb}")))?,
        tvdb_id: other_db
            .map(|v| u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("tvdb {v}"))))
            .transpose()?,
        imdb_id: row.try_get(3)?,
        title: row.try_get(4)?,
        original_title: row.try_get(5)?,
        original_language: row.try_get(6)?,
        year: row
            .try_get::<_, Option<i32>>(7)?
            .map(|y| narrow(y, "ano"))
            .transpose()?,
        status: row.try_get(8)?,
        overview: row.try_get(9)?,
        network: row.try_get(10)?,
        runtime: narrow(row.try_get(11)?, "duração")?,
        poster: row.try_get(12)?,
        fanart: row.try_get(13)?,
        path: row.try_get(14)?,
        season_folder: row.try_get(15)?,
        monitor_new: row.try_get(16)?,
        added: row.try_get(17)?,
        refreshed_at: row.try_get(18)?,
        metadata_title: row.try_get(19)?,
        alternate_titles: Vec::new(),
    })
}

async fn read_series(client: &impl GenericClient, id: Option<i64>) -> Result<Vec<CatalogSeries>> {
    let rows = client
        .query(
            "SELECT id, tmdb_id, tvdb_id, imdb_id, title, original_title, original_language,
                    year, status, overview, network, runtime, poster, fanart, path,
                    season_folder, monitor_new, added, refreshed_at, metadata_title, priority
             FROM series
             WHERE ($1::BIGINT IS NULL OR id = $1)
             ORDER BY lower(title), id",
            &[&id],
        )
        .await?;
    let mut all = Vec::with_capacity(rows.len());
    let mut index = HashMap::new();
    for row in &rows {
        let id: i64 = row.try_get(0)?;
        index.insert(id, all.len());
        all.push(CatalogSeries {
            id,
            series: series_from(row)?,
            episodes: Vec::new(),
            files: Vec::new(),
            priority: row.try_get(20)?,
            scene: Vec::new(),
            subtitles: Vec::new(),
        });
    }

    for row in client
        .query(
            "SELECT series_id, title FROM series_titles
             WHERE ($1::BIGINT IS NULL OR series_id = $1) ORDER BY id",
            &[&id],
        )
        .await?
    {
        if let Some(&at) = index.get(&row.try_get::<_, i64>(0)?) {
            all[at].series.alternate_titles.push(row.try_get(1)?);
        }
    }
    for row in client
        .query(
            &format!(
                "SELECT series_id, id, skip, skipped_at, file_id, {EPISODE_FIELDS} FROM episodes
                 WHERE ($1::BIGINT IS NULL OR series_id = $1) ORDER BY season, number"
            ),
            &[&id],
        )
        .await?
    {
        if let Some(&at) = index.get(&row.try_get::<_, i64>(0)?) {
            all[at].episodes.push(CatalogEpisode {
                id: row.try_get(1)?,
                skip: row
                    .try_get::<_, Option<String>>(2)?
                    .map(|s| Skip::parse(&s))
                    .transpose()?,
                skipped_at: row.try_get(3)?,
                file_id: row.try_get(4)?,
                episode: episode_from(&row, 5)?,
            });
        }
    }
    for row in client
        .query(
            &format!(
                "SELECT {FILE_COLUMNS}, series_id FROM episode_files
                 WHERE ($1::BIGINT IS NULL OR series_id = $1) ORDER BY relative_path"
            ),
            &[&id],
        )
        .await?
    {
        if let Some(&at) = index.get(&row.try_get::<_, i64>(11)?) {
            all[at].files.push(file_from(&row)?);
        }
    }
    read_scene_and_subtitles(client, id, &index, &mut all).await?;
    Ok(all)
}

/// A numeração de cena e as legendas das séries de `read_series`.
async fn read_scene_and_subtitles(
    client: &impl GenericClient,
    id: Option<i64>,
    index: &HashMap<i64, usize>,
    all: &mut [CatalogSeries],
) -> Result<()> {
    for row in client
        .query(
            "SELECT series_id, scene_season, scene_episode, season, episode FROM scene_mappings
             WHERE ($1::BIGINT IS NULL OR series_id = $1)
             ORDER BY scene_season, scene_episode, season, episode",
            &[&id],
        )
        .await?
    {
        if let Some(&at) = index.get(&row.try_get::<_, i64>(0)?) {
            all[at].scene.push(SceneMapping {
                scene_season: narrow(row.try_get(1)?, "temporada de cena")?,
                scene_episode: narrow(row.try_get(2)?, "episódio de cena")?,
                season: narrow(row.try_get(3)?, "temporada")?,
                episode: narrow(row.try_get(4)?, "episódio")?,
            });
        }
    }
    for row in client
        .query(
            "SELECT s.id, s.relative_path, s.language, s.forced, s.origin, s.episode_file_id,
                    f.series_id
             FROM subtitle_files s JOIN episode_files f ON f.id = s.episode_file_id
             WHERE ($1::BIGINT IS NULL OR f.series_id = $1)
             ORDER BY s.relative_path",
            &[&id],
        )
        .await?
    {
        if let Some(&at) = index.get(&row.try_get::<_, i64>(6)?) {
            all[at].subtitles.push(subtitle_from(&row, 5)?);
        }
    }
    Ok(())
}

/// O `skip` de um episódio que entra no catálogo: especiais (temporada 0) e,
/// com `monitor_new` desligado, qualquer um entram como [`Skip::Unwanted`];
/// o resto entra sem `skip`, inclusive o já exibido: se falta, é buscado.
/// Monitorar só parte da série é decisão da tela, com [`Store::set_skip`].
#[must_use]
pub fn default_skip(series: &Series, episode: &Episode) -> Option<Skip> {
    (episode.season == 0 || !series.monitor_new).then_some(Skip::Unwanted)
}

#[cfg(test)]
mod tests {
    use acervo_parser::Quality;

    use super::*;
    use crate::testing::TestDb;
    use crate::{Blocked, NewHistory};

    fn series(tmdb_id: u32, title: &str) -> Series {
        Series {
            tmdb_id,
            tvdb_id: Some(tmdb_id + 1),
            imdb_id: Some(format!("tt{tmdb_id:07}")),
            title: title.into(),
            original_title: Some(format!("{title} original")),
            metadata_title: Some(format!("{title} (EN)")),
            original_language: Some("English".into()),
            year: Some(2019),
            status: Some("continuing".into()),
            overview: Some("Sinopse".into()),
            network: Some("Rede".into()),
            runtime: 45,
            poster: Some("/poster.jpg".into()),
            fanart: None,
            path: format!("/series/{title}"),
            season_folder: true,
            monitor_new: true,
            added: Some("2026-01-01T00:00:00Z".into()),
            refreshed_at: Some("2026-01-02T00:00:00Z".into()),
            alternate_titles: vec![format!("{title} alternativo"), format!("{title} 2")],
        }
    }

    fn episode(season: u16, number: u16) -> Episode {
        Episode {
            season,
            number,
            tmdb_id: Some(u32::from(season) * 100 + u32::from(number)),
            title: Some(format!("Episódio {number}")),
            air_date: Some("2026-02-01".into()),
            overview: None,
            runtime: 45,
        }
    }

    fn file(path: &str) -> EpisodeFile {
        EpisodeFile {
            relative_path: path.into(),
            size: 1_000_000_000,
            quality: QualityModel {
                quality: Quality::WebDl1080p,
                revision: Revision {
                    version: 2,
                    real: 1,
                    is_repack: true,
                },
            },
            languages: vec!["English".into(), "Portuguese (Brazil)".into()],
            release_group: Some("GRUPO".into()),
            scene_name: Some("Serie.S01E01.1080p.WEB-DL-GRUPO".into()),
            date_added: Some("2026-02-02T00:00:00Z".into()),
        }
    }

    fn grab(series_id: i64, hash: &str, episode_ids: Vec<i64>, at: &str) -> SeriesGrab {
        SeriesGrab {
            id: 999,
            series_id,
            episode_ids,
            hash: hash.into(),
            title: "Serie.S01.1080p.WEB-DL-GRUPO".into(),
            indexer: "tracker".into(),
            quality: Quality::WebDl1080p,
            size: 5_000_000_000,
            grabbed_at: at.into(),
            state: GrabState::Downloading,
            message: None,
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn guarda_e_le_a_serie() {
        let Some(db) = TestDb::new("series_le").await else {
            return;
        };
        let store = &db.store;
        let zeta = series(20, "Zeta");
        let alfa = series(10, "alfa");
        let zeta_id = store
            .add_series(&zeta, &[episode(1, 2), episode(1, 1)])
            .await
            .unwrap();
        store.add_series(&alfa, &[]).await.unwrap();
        assert!(store.add_series(&zeta, &[]).await.is_err());

        let read = store.series(zeta_id).await.unwrap().unwrap();
        assert_eq!(read.series, zeta);
        assert_eq!(read.series.alternate_titles, ["Zeta alternativo", "Zeta 2"]);
        let numbers: Vec<_> = read.episodes.iter().map(|e| e.episode.number).collect();
        assert_eq!(numbers, [1, 2]);
        assert_eq!(read.episodes[0].episode, episode(1, 1));
        assert!(
            read.episodes
                .iter()
                .all(|e| e.skip.is_none() && e.file_id.is_none())
        );
        assert!(store.series(zeta_id + 100).await.unwrap().is_none());

        let list = store.series_list().await.unwrap();
        let titles: Vec<_> = list.iter().map(|s| s.series.title.as_str()).collect();
        assert_eq!(titles, ["alfa", "Zeta"]);

        // A atualização de metadados parte de uma leitura velha: o que é de
        // quem usa fica como a tela deixou.
        assert!(
            store
                .set_series_options(zeta_id, false, false)
                .await
                .unwrap()
        );
        assert!(store.set_series_priority(zeta_id, true).await.unwrap());
        let mut changed = zeta.clone();
        changed.title = "Zeta Nova".into();
        changed.metadata_title = None;
        changed.alternate_titles = vec!["Outro".into()];
        changed.path = "/outra/pasta".into();
        changed.added = None;
        assert!(changed.monitor_new && changed.season_folder);
        assert!(
            store
                .update_series_metadata(zeta_id, &changed)
                .await
                .unwrap()
        );
        let read = store.series(zeta_id).await.unwrap().unwrap();
        assert_eq!(
            read.series,
            Series {
                path: zeta.path.clone(),
                added: zeta.added.clone(),
                monitor_new: false,
                season_folder: false,
                ..changed.clone()
            }
        );
        assert!(read.priority);
        assert_eq!(read.episodes.len(), 2);
        assert!(
            !store
                .update_series_metadata(zeta_id + 100, &changed)
                .await
                .unwrap()
        );
        assert!(
            !store
                .set_series_options(zeta_id + 100, true, true)
                .await
                .unwrap()
        );

        db.drop().await;
    }

    #[tokio::test]
    async fn poda_buscas_de_serie_mantendo_a_ultima() {
        let Some(db) = TestDb::new("series_poda").await else {
            return;
        };
        let store = &db.store;
        let id = store.add_series(&series(10, "Um"), &[]).await.unwrap();
        let run = |at: &str| SeriesSearch {
            series_id: id,
            at: at.into(),
            queries: vec!["S01".into()],
            releases: 0,
            picks: Vec::new(),
            rejections: Vec::new(),
            error: None,
        };
        for at in ["2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z"] {
            store.record_series_search(&run(at)).await.unwrap();
        }
        assert_eq!(
            store.prune_searches("2026-02-01T00:00:00Z").await.unwrap(),
            1
        );
        let left = store.latest_series_searches().await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].at, "2026-01-02T00:00:00Z");
        db.drop().await;
    }

    #[tokio::test]
    async fn aplica_o_skip_padrao() {
        let Some(db) = TestDb::new("series_skip_padrao").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(&series(10, "Um"), &[episode(0, 1), episode(1, 1)])
            .await
            .unwrap();
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(read.episodes[0].skip, Some(Skip::Unwanted));
        assert_eq!(read.episodes[1].skip, None);

        let mut unmonitored = series(11, "Dois");
        unmonitored.monitor_new = false;
        let id = store
            .add_series(&unmonitored, &[episode(1, 1)])
            .await
            .unwrap();
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(read.episodes[0].skip, Some(Skip::Unwanted));

        // Episódio novo no sync segue a mesma regra.
        let sync = store
            .sync_episodes(id, &[episode(1, 1), episode(1, 2)])
            .await
            .unwrap();
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(sync.added, [read.episodes[1].id]);
        assert_eq!(read.episodes[1].skip, Some(Skip::Unwanted));

        db.drop().await;
    }

    #[tokio::test]
    async fn sincroniza_episodios() {
        let Some(db) = TestDb::new("series_sync").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(
                &series(10, "Um"),
                &[episode(1, 1), episode(1, 2), episode(1, 3), episode(1, 4)],
            )
            .await
            .unwrap();
        let read = store.series(id).await.unwrap().unwrap();
        let ids: Vec<i64> = read.episodes.iter().map(|e| e.id).collect();
        store
            .set_skip(&ids[0..1], Some(Skip::Watched), "2026-03-01T00:00:00Z")
            .await
            .unwrap();
        // O 3 tem arquivo; o 4 não.
        store
            .add_episode_file(id, &file("Season 1/e3.mkv"), &ids[2..3])
            .await
            .unwrap();

        let mut changed = episode(1, 1);
        changed.title = Some("Outro título".into());
        let sync = store
            .sync_episodes(id, &[changed.clone(), episode(1, 5)])
            .await
            .unwrap();
        // 1 mudou; 2 e 4 saem (sem arquivo nem `skip`); 3 fica órfão; 5 é
        // novo.
        assert_eq!((sync.updated, sync.removed), (1, 2));
        assert_eq!(sync.orphaned, [ids[2]]);
        assert_eq!(sync.added.len(), 1);

        let read = store.series(id).await.unwrap().unwrap();
        let numbers: Vec<_> = read.episodes.iter().map(|e| e.episode.number).collect();
        assert_eq!(numbers, [1, 3, 5]);
        assert_eq!(read.episodes[0].episode, changed);
        assert_eq!(read.episodes[0].skip, Some(Skip::Watched));
        assert_eq!(
            read.episodes[0].skipped_at.as_deref(),
            Some("2026-03-01T00:00:00Z")
        );
        assert!(read.episodes[1].file_id.is_some());
        assert_eq!(read.episodes[2].skip, None);

        let again = store
            .sync_episodes(id, &[changed.clone(), episode(1, 5)])
            .await
            .unwrap();
        assert_eq!((again.updated, again.removed), (0, 0));

        // Dispensado que some da base fica: o `skip` é decisão do usuário.
        let watched = read.episodes[0].id;
        let sync = store.sync_episodes(id, &[episode(1, 5)]).await.unwrap();
        assert_eq!(sync.removed, 0);
        assert_eq!(sync.orphaned, [watched, ids[2]]);

        // Temporada que não veio no anúncio (404 transitório, temporada
        // pulada) fica inteira, mesmo sem arquivo nem `skip`.
        store
            .sync_episodes(id, &[changed.clone(), episode(1, 5), episode(2, 1)])
            .await
            .unwrap();
        let sync = store
            .sync_episodes(id, &[changed.clone(), episode(1, 5)])
            .await
            .unwrap();
        assert_eq!(sync.removed, 0);
        let read = store.series(id).await.unwrap().unwrap();
        let s2 = read
            .episodes
            .iter()
            .find(|e| e.episode.season == 2)
            .unwrap();
        assert!(s2.file_id.is_none() && s2.skip.is_none());
        assert!(sync.orphaned.contains(&s2.id));
        // Na temporada que veio, o sem arquivo e sem `skip` sai.
        let sync = store
            .sync_episodes(id, &[changed, episode(2, 2)])
            .await
            .unwrap();
        assert_eq!(sync.removed, 2);
        let read = store.series(id).await.unwrap().unwrap();
        let left: Vec<_> = read
            .episodes
            .iter()
            .map(|e| (e.episode.season, e.episode.number))
            .collect();
        assert_eq!(left, [(1, 1), (1, 3), (2, 2)]);
        assert!(store.sync_episodes(id + 100, &[]).await.is_err());

        db.drop().await;
    }

    #[tokio::test]
    async fn muda_o_skip_em_lote() {
        let Some(db) = TestDb::new("series_set_skip").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(
                &series(10, "Um"),
                &[episode(1, 1), episode(1, 2), episode(1, 3)],
            )
            .await
            .unwrap();
        let ids: Vec<i64> = store
            .series(id)
            .await
            .unwrap()
            .unwrap()
            .episodes
            .iter()
            .map(|e| e.id)
            .collect();

        let at = "2026-03-01T00:00:00Z";
        assert_eq!(
            store
                .set_skip(&ids[..2], Some(Skip::Deleted), at)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .set_skip(&ids[..2], Some(Skip::Deleted), "depois")
                .await
                .unwrap(),
            0
        );
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(read.episodes[0].skip, Some(Skip::Deleted));
        assert_eq!(read.episodes[0].skipped_at.as_deref(), Some(at));
        assert_eq!(read.episodes[2].skip, None);

        assert_eq!(store.set_skip(&ids, None, at).await.unwrap(), 2);
        let read = store.series(id).await.unwrap().unwrap();
        assert!(
            read.episodes
                .iter()
                .all(|e| e.skip.is_none() && e.skipped_at.is_none())
        );

        db.drop().await;
    }

    #[tokio::test]
    async fn liga_arquivo_multi_episodio_e_troca() {
        let Some(db) = TestDb::new("series_arquivos").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(
                &series(10, "Um"),
                &[episode(1, 1), episode(1, 2), episode(1, 3)],
            )
            .await
            .unwrap();
        let other = store
            .add_series(&series(11, "Dois"), &[episode(1, 1)])
            .await
            .unwrap();
        let ids: Vec<i64> = store
            .series(id)
            .await
            .unwrap()
            .unwrap()
            .episodes
            .iter()
            .map(|e| e.id)
            .collect();
        let foreign = store.series(other).await.unwrap().unwrap().episodes[0].id;

        let (first, replaced) = store
            .add_episode_file(id, &file("Season 1/e1e2.mkv"), &ids[..2])
            .await
            .unwrap();
        assert!(replaced.is_empty());
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(read.episodes[0].file_id, Some(first));
        assert_eq!(read.episodes[1].file_id, Some(first));
        assert_eq!(read.episodes[2].file_id, None);
        assert_eq!(read.files.len(), 1);
        assert_eq!(read.files[0].file, file("Season 1/e1e2.mkv"));

        // Outro arquivo leva só o episódio 2: o primeiro ainda cobre o 1.
        let (second, replaced) = store
            .add_episode_file(id, &file("Season 1/e2.mkv"), &ids[1..2])
            .await
            .unwrap();
        assert!(replaced.is_empty());
        // Agora o 1 também passa para o terceiro: o primeiro fica vazio e sai.
        let (third, replaced) = store
            .add_episode_file(id, &file("Season 1/e1.mkv"), &ids[..1])
            .await
            .unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].id, first);
        assert_eq!(replaced[0].file.relative_path, "Season 1/e1e2.mkv");
        let read = store.series(id).await.unwrap().unwrap();
        let paths: Vec<_> = read
            .files
            .iter()
            .map(|f| f.file.relative_path.as_str())
            .collect();
        assert_eq!(paths, ["Season 1/e1.mkv", "Season 1/e2.mkv"]);
        assert_eq!(read.episodes[0].file_id, Some(third));
        assert_eq!(read.episodes[1].file_id, Some(second));

        let wrong = store
            .add_episode_file(id, &file("Season 1/x.mkv"), &[ids[2], foreign])
            .await;
        assert!(matches!(wrong, Err(StoreError::Corrupt(_))));
        assert_eq!(store.series(id).await.unwrap().unwrap().files.len(), 2);

        // Upgrade com o mesmo nome: o registro é regravado, e não volta em
        // `replaced` (quem chama apagaria do disco o arquivo novo).
        let mut upgraded = file("Season 1/e1.mkv");
        upgraded.size = 9;
        let (again, replaced) = store
            .add_episode_file(id, &upgraded, &ids[..1])
            .await
            .unwrap();
        assert_eq!(again, third);
        assert!(replaced.is_empty());
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(read.files.len(), 2);
        assert_eq!(read.files[0].file.size, 9);

        db.drop().await;
    }

    #[tokio::test]
    async fn apaga_arquivo_de_episodio() {
        let Some(db) = TestDb::new("series_apaga_arquivo").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(&series(10, "Um"), &[episode(1, 1), episode(1, 2)])
            .await
            .unwrap();
        let ids: Vec<i64> = store
            .series(id)
            .await
            .unwrap()
            .unwrap()
            .episodes
            .iter()
            .map(|e| e.id)
            .collect();
        let (file_id, _) = store
            .add_episode_file(id, &file("Season 1/e1e2.mkv"), &ids)
            .await
            .unwrap();

        let gone = store.delete_episode_file(file_id).await.unwrap().unwrap();
        assert_eq!(gone.id, file_id);
        assert_eq!(gone.file, file("Season 1/e1e2.mkv"));
        let read = store.series(id).await.unwrap().unwrap();
        assert!(read.files.is_empty());
        assert!(read.episodes.iter().all(|e| e.file_id.is_none()));
        assert!(store.delete_episode_file(file_id).await.unwrap().is_none());

        db.drop().await;
    }

    #[tokio::test]
    async fn grava_le_e_atualiza_grab() {
        let Some(db) = TestDb::new("series_grabs").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(&series(10, "Um"), &[episode(1, 1), episode(1, 2)])
            .await
            .unwrap();
        let other = store
            .add_series(&series(11, "Dois"), &[episode(1, 1)])
            .await
            .unwrap();
        let ids: Vec<i64> = store
            .series(id)
            .await
            .unwrap()
            .unwrap()
            .episodes
            .iter()
            .map(|e| e.id)
            .collect();
        let foreign = store.series(other).await.unwrap().unwrap().episodes[0].id;

        let old = store
            .record_series_grab(&grab(id, "aaaa", ids.clone(), "2026-01-01T00:00:00Z"))
            .await
            .unwrap();
        let new = store
            .record_series_grab(&grab(id, "bbbb", ids[..1].to_vec(), "2026-01-02T00:00:00Z"))
            .await
            .unwrap();
        assert_ne!(old, 999);
        assert!(
            store
                .record_series_grab(&grab(id, "aaaa", vec![], "2026-01-03T00:00:00Z"))
                .await
                .is_err()
        );
        let wrong = store
            .record_series_grab(&grab(id, "cccc", vec![foreign], "2026-01-03T00:00:00Z"))
            .await;
        assert!(matches!(wrong, Err(StoreError::Corrupt(_))));

        let grabs = store.series_grabs().await.unwrap();
        assert_eq!(grabs.len(), 2);
        assert_eq!(
            grabs[0],
            SeriesGrab {
                id: new,
                ..grab(id, "bbbb", ids[..1].to_vec(), "2026-01-02T00:00:00Z")
            }
        );
        assert_eq!(grabs[1].id, old);
        assert_eq!(grabs[1].episode_ids, ids);

        store
            .update_series_grab(
                old,
                GrabState::Failed,
                Some("sem seeds"),
                Some("2026-01-04T00:00:00Z"),
            )
            .await
            .unwrap();
        let grabs = store.series_grabs().await.unwrap();
        assert_eq!(grabs[1].state, GrabState::Failed);
        assert_eq!(grabs[1].message.as_deref(), Some("sem seeds"));
        assert_eq!(
            grabs[1].finished_at.as_deref(),
            Some("2026-01-04T00:00:00Z")
        );

        // O mesmo release pego de novo depois de acabar reaproveita o registro,
        // com os episódios de agora.
        let again = store
            .record_series_grab(&grab(id, "aaaa", ids[1..].to_vec(), "2026-01-05T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(again, old);
        let reused = store
            .series_grabs()
            .await
            .unwrap()
            .into_iter()
            .find(|g| g.id == old)
            .unwrap();
        assert_eq!(reused.state, GrabState::Downloading);
        assert_eq!(reused.episode_ids, ids[1..]);

        db.drop().await;
    }

    #[tokio::test]
    async fn apagar_a_serie_leva_tudo_e_poupa_o_historico() {
        let Some(db) = TestDb::new("series_apaga").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(&series(10, "Um"), &[episode(1, 1)])
            .await
            .unwrap();
        let ids = vec![store.series(id).await.unwrap().unwrap().episodes[0].id];
        store
            .add_episode_file(id, &file("Season 1/e1.mkv"), &ids)
            .await
            .unwrap();
        store
            .record_series_grab(&grab(id, "aaaa", ids.clone(), "2026-01-01T00:00:00Z"))
            .await
            .unwrap();
        store
            .record_history(&NewHistory {
                movie_id: None,
                series_id: Some(id),
                episode_ids: ids.clone(),
                movie_title: "Um".into(),
                event: "grabbed".into(),
                at: "2026-01-01T00:00:00Z".into(),
                source_title: None,
                quality: None,
                indexer: None,
                download_id: None,
                data: serde_json::json!({}),
            })
            .await
            .unwrap();
        let blocked = Blocked {
            id: 0,
            movie_id: None,
            series_id: Some(id),
            source_title: "Um.S01".into(),
            indexer: None,
            quality: None,
            size: None,
            hash: None,
            at: "2026-01-01T00:00:00Z".into(),
            message: None,
        };
        store.block(&blocked).await.unwrap();
        assert_eq!(store.blocklist().await.unwrap()[0].series_id, Some(id));
        let page = store.history(None, None, 10, 0).await.unwrap();
        assert_eq!(page.events[0].series_id, Some(id));
        assert_eq!(page.events[0].episode_ids, ids);

        assert!(store.delete_series(id).await.unwrap());
        assert!(!store.delete_series(id).await.unwrap());
        assert!(store.series(id).await.unwrap().is_none());
        assert!(store.series_grabs().await.unwrap().is_empty());
        let page = store.history(None, None, 10, 0).await.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.events[0].series_id, None);
        assert_eq!(page.events[0].episode_ids, ids);

        db.drop().await;
    }

    #[tokio::test]
    async fn busca_e_historico_por_serie() {
        let Some(db) = TestDb::new("series_busca").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(&series(1, "Um"), &[episode(1, 1), episode(1, 2)])
            .await
            .unwrap();
        let other = store.add_series(&series(2, "Dois"), &[]).await.unwrap();
        let ids: Vec<i64> = store
            .series(id)
            .await
            .unwrap()
            .unwrap()
            .episodes
            .iter()
            .map(|e| e.id)
            .collect();
        let run = |at: &str, picks: Vec<SeriesPick>| SeriesSearch {
            series_id: id,
            at: at.into(),
            queries: vec!["S01".into()],
            releases: 12,
            picks,
            rejections: vec![("NothingWanted".into(), 9)],
            error: None,
        };
        store
            .record_series_search(&run("2026-01-01T00:00:00Z", Vec::new()))
            .await
            .unwrap();
        let newer = run(
            "2026-01-02T00:00:00Z",
            vec![SeriesPick {
                title: "Um.S01.1080p.WEB-DL".into(),
                indexer: "tracker".into(),
                quality: Quality::WebDl1080p,
                size: 9,
                episode_ids: ids.clone(),
            }],
        );
        store.record_series_search(&newer).await.unwrap();
        assert_eq!(store.latest_series_searches().await.unwrap(), [newer]);

        for (series_id, event) in [(id, "grabbed"), (other, "grabbed"), (id, "imported")] {
            store
                .record_history(&NewHistory {
                    movie_id: None,
                    series_id: Some(series_id),
                    episode_ids: ids.clone(),
                    movie_title: "Um S01E01".into(),
                    event: event.into(),
                    at: "2026-01-01T00:00:00Z".into(),
                    source_title: None,
                    quality: None,
                    indexer: None,
                    download_id: None,
                    data: serde_json::json!({}),
                })
                .await
                .unwrap();
        }
        let page = store.series_history(id, None, 10, 0).await.unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.events[0].event, "imported");
        let page = store
            .series_history(id, Some("grabbed"), 10, 0)
            .await
            .unwrap();
        assert_eq!(page.total, 1);

        store.delete_series(id).await.unwrap();
        assert!(store.latest_series_searches().await.unwrap().is_empty());
        db.drop().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário só: prioridade, cena, caminho e legendas.
    async fn prioridade_cena_caminho_e_legendas_da_serie() {
        let Some(db) = TestDb::new("series_extras").await else {
            return;
        };
        let store = &db.store;
        let id = store
            .add_series(&series(10, "Um"), &[episode(1, 1), episode(2, 1)])
            .await
            .unwrap();
        let read = store.series(id).await.unwrap().unwrap();
        assert!(!read.priority);
        assert!(read.scene.is_empty() && read.subtitles.is_empty());

        assert!(store.set_series_priority(id, true).await.unwrap());
        assert!(!store.set_series_priority(id + 100, true).await.unwrap());
        assert!(store.series(id).await.unwrap().unwrap().priority);

        let map = |ss, se, s, e| SceneMapping {
            scene_season: ss,
            scene_episode: se,
            season: s,
            episode: e,
        };
        store
            .set_scene_mappings(
                id,
                &[
                    map(1, 13, 2, 1),
                    map(1, 1, 1, 2),
                    map(1, 13, 2, 1),
                    map(1, 1, 1, 1),
                ],
            )
            .await
            .unwrap();
        let read = store.series(id).await.unwrap().unwrap();
        // O duplo guarda os dois alvos; a linha repetida entra uma vez; ordem
        // por cena e alvo.
        assert_eq!(
            read.scene,
            [map(1, 1, 1, 1), map(1, 1, 1, 2), map(1, 13, 2, 1)]
        );
        // Trocar substitui tudo.
        store
            .set_scene_mappings(id, &[map(3, 1, 2, 5)])
            .await
            .unwrap();
        assert_eq!(
            store.series(id).await.unwrap().unwrap().scene,
            [map(3, 1, 2, 5)]
        );

        let ids: Vec<i64> = read.episodes.iter().map(|e| e.id).collect();
        let (file_id, _) = store
            .add_episode_file(id, &file("Season 1/e1 TBA.mkv"), &ids[..1])
            .await
            .unwrap();
        let sub = crate::Subtitle {
            relative_path: "Season 1/e1 TBA.pt-BR.srt".into(),
            language: Some("pt-BR".into()),
            forced: false,
            origin: crate::SubtitleOrigin::Disk,
        };
        let sub_id = store.add_episode_subtitle(file_id, &sub).await.unwrap();
        // O mesmo caminho é regravado no lugar.
        assert_eq!(
            store.add_episode_subtitle(file_id, &sub).await.unwrap(),
            sub_id
        );
        assert!(
            store
                .set_episode_file_path(id, file_id, "Season 1/e1 Piloto.mkv")
                .await
                .unwrap()
        );
        assert!(
            !store
                .set_episode_file_path(id + 100, file_id, "x.mkv")
                .await
                .unwrap()
        );
        assert!(
            store
                .set_subtitle_path(sub_id, "Season 1/e1 Piloto.pt-BR.srt")
                .await
                .unwrap()
        );
        let read = store.series(id).await.unwrap().unwrap();
        assert_eq!(read.files[0].file.relative_path, "Season 1/e1 Piloto.mkv");
        assert_eq!(read.subtitles.len(), 1);
        assert_eq!(read.subtitles[0].owner, file_id);
        assert_eq!(
            read.subtitles[0].subtitle.origin,
            crate::SubtitleOrigin::Disk
        );
        assert_eq!(
            read.subtitles[0].subtitle.relative_path,
            "Season 1/e1 Piloto.pt-BR.srt"
        );
        // O arquivo sai e leva a legenda.
        store.delete_episode_file(file_id).await.unwrap();
        assert!(
            store
                .series(id)
                .await
                .unwrap()
                .unwrap()
                .subtitles
                .is_empty()
        );

        // A série sai e leva a cena.
        store.delete_series(id).await.unwrap();
        db.drop().await;
    }
}

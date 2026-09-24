//! Catálogo persistente: filmes, o arquivo de cada um e os perfis de
//! qualidade, num arquivo SQLite.
//!
//! Nesta etapa o catálogo é um espelho do gerenciador de filmes em produção,
//! importado pela API dele. A importação roda numa transação: a simulação faz
//! o mesmo trabalho e desfaz no fim, então o que ela relata é exatamente o que
//! a aplicação faria.

use std::collections::BTreeSet;
use std::path::Path;

use acervo_parser::{Quality, QualityModel, Revision};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("banco de dados: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("criando o diretório de `{path}`: {source}")]
    Directory {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("registro inconsistente no banco: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Um degrau do perfil: uma qualidade, ou um grupo delas que vale o mesmo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileItem {
    pub name: String,
    pub qualities: Vec<Quality>,
    pub allowed: bool,
}

/// Perfil de qualidade. `items` vai do pior ao melhor, como na referência.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityProfile {
    pub name: String,
    pub upgrade_allowed: bool,
    /// Posição em `items` a partir da qual não se busca mais upgrade.
    pub cutoff: Option<usize>,
    pub language: Option<String>,
    pub items: Vec<ProfileItem>,
    pub min_format_score: i32,
    pub cutoff_format_score: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieFile {
    /// Relativo à pasta do filme.
    pub relative_path: String,
    pub size: u64,
    pub quality: QualityModel,
    pub languages: Vec<String>,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    /// Nome do release de onde o arquivo veio.
    pub scene_name: Option<String>,
    pub date_added: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movie {
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub title: String,
    pub original_title: Option<String>,
    pub original_language: Option<String>,
    pub year: Option<u16>,
    /// `announced`, `inCinemas` ou `released`.
    pub status: Option<String>,
    pub minimum_availability: Option<String>,
    pub monitored: bool,
    /// Nome do perfil de qualidade.
    pub quality_profile: Option<String>,
    /// Pasta do filme, como o gerenciador a vê.
    pub path: String,
    pub added: Option<String>,
    pub file: Option<MovieFile>,
    /// Minutos; zero é desconhecido.
    pub runtime: u32,
    /// Ano alternativo (estreia em outro país), aceito no casamento.
    pub secondary_year: Option<u16>,
    /// Título limpo da base de metadados, a forma com que o release é
    /// comparado.
    pub clean_title: Option<String>,
    /// Títulos alternativos e traduções.
    pub alternate_titles: Vec<String>,
    /// Já passou da disponibilidade mínima.
    pub available: bool,
}

/// Um filme do catálogo, com o id local e de onde veio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMovie {
    pub id: i64,
    pub movie: Movie,
    /// Instância de origem e o id do filme lá, se veio de importação.
    pub origin: Option<(String, i64)>,
}

/// Tamanhos por minuto de filme, em megabytes, de uma qualidade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityDefinition {
    pub quality: Quality,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}

/// O que a decisão em sombra pegaria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowPick {
    pub title: String,
    pub indexer: String,
    pub quality: Quality,
    pub size: u64,
}

/// Uma busca em sombra por um filme: o que pegaria, ou por que não.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowRun {
    pub movie_id: i64,
    /// RFC 3339, em UTC.
    pub at: String,
    pub releases: usize,
    pub pick: Option<ShadowPick>,
    /// Motivo de rejeição e quantos releases ele barrou.
    pub rejections: Vec<(String, usize)>,
    /// A busca falhou antes de decidir.
    pub error: Option<String>,
}

/// Tudo o que uma instância tem, para espelhar.
#[derive(Debug, Clone)]
pub struct Import {
    /// Nome da origem, como `radarr:filmes`. Filme que some da origem sai do
    /// catálogo só se tiver vindo dela.
    pub source: String,
    pub profiles: Vec<QualityProfile>,
    pub definitions: Vec<QualityDefinition>,
    /// Id do filme na origem, e o filme.
    pub movies: Vec<(i64, Movie)>,
}

/// O que uma importação muda. Nomes como "Título (Ano)".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportSummary {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    pub profiles: usize,
    pub applied: bool,
}

const MIGRATIONS: &[&str] = &[
    r"
    CREATE TABLE quality_profiles (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        upgrade_allowed INTEGER NOT NULL,
        cutoff INTEGER,
        language TEXT,
        items TEXT NOT NULL
    );
    CREATE TABLE movies (
        id INTEGER PRIMARY KEY,
        tmdb_id INTEGER NOT NULL UNIQUE,
        imdb_id TEXT,
        title TEXT NOT NULL,
        original_title TEXT,
        original_language TEXT,
        year INTEGER,
        status TEXT,
        minimum_availability TEXT,
        monitored INTEGER NOT NULL,
        quality_profile_id INTEGER REFERENCES quality_profiles(id),
        path TEXT NOT NULL,
        added TEXT,
        source TEXT,
        source_id INTEGER
    );
    CREATE TABLE movie_files (
        movie_id INTEGER PRIMARY KEY REFERENCES movies(id) ON DELETE CASCADE,
        relative_path TEXT NOT NULL,
        size INTEGER NOT NULL,
        quality INTEGER NOT NULL,
        revision_version INTEGER NOT NULL,
        revision_real INTEGER NOT NULL,
        is_repack INTEGER NOT NULL,
        languages TEXT NOT NULL,
        release_group TEXT,
        edition TEXT,
        scene_name TEXT,
        date_added TEXT
    );
",
    r"
    ALTER TABLE movies ADD COLUMN runtime INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE movies ADD COLUMN secondary_year INTEGER;
    ALTER TABLE movies ADD COLUMN clean_title TEXT;
    ALTER TABLE movies ADD COLUMN available INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE quality_profiles ADD COLUMN min_format_score INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE quality_profiles ADD COLUMN cutoff_format_score INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE movie_titles (
        movie_id INTEGER NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
        title TEXT NOT NULL
    );
    CREATE INDEX movie_titles_by_movie ON movie_titles(movie_id);
    CREATE TABLE quality_definitions (
        quality INTEGER PRIMARY KEY,
        min_size REAL,
        max_size REAL,
        preferred_size REAL
    );
",
    r"
    CREATE TABLE shadow_runs (
        id INTEGER PRIMARY KEY,
        movie_id INTEGER NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
        at TEXT NOT NULL,
        releases INTEGER NOT NULL,
        pick_title TEXT,
        pick_indexer TEXT,
        pick_quality INTEGER,
        pick_size INTEGER,
        rejections TEXT NOT NULL,
        error TEXT
    );
    CREATE INDEX shadow_runs_by_movie ON shadow_runs(movie_id, at);
",
];

#[derive(Debug)]
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Abre (ou cria) o banco e aplica as migrações pendentes.
    ///
    /// # Errors
    ///
    /// Diretório impossível de criar, arquivo que não é SQLite ou migração que
    /// falha.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Directory {
                path: parent.display().to_string(),
                source,
            })?;
        }
        Self::setup(Connection::open(path)?)
    }

    /// # Errors
    ///
    /// Migração que falha.
    pub fn open_in_memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(mut connection: Connection) -> Result<Self> {
        // WAL: a interface lê enquanto uma importação escreve.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tx = connection.transaction()?;
        for (index, migration) in (1_i64..).zip(MIGRATIONS).skip_while(|(i, _)| *i <= version) {
            tx.execute_batch(migration)?;
            tx.pragma_update(None, "user_version", index)?;
        }
        tx.commit()?;
        Ok(Self { connection })
    }

    /// Todos os filmes, por título.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub fn movies(&self) -> Result<Vec<CatalogMovie>> {
        read_movies(&self.connection)
    }

    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub fn profiles(&self) -> Result<Vec<QualityProfile>> {
        let mut statement = self.connection.prepare(
            "SELECT name, upgrade_allowed, cutoff, language, items, min_format_score,
                    cutoff_format_score
             FROM quality_profiles ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i32>(5)?,
                row.get::<_, i32>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (
                name,
                upgrade_allowed,
                cutoff,
                language,
                items,
                min_format_score,
                cutoff_format_score,
            ) = row?;
            Ok(QualityProfile {
                items: decode_items(&items)?,
                cutoff: cutoff.and_then(|c| usize::try_from(c).ok()),
                name,
                upgrade_allowed,
                language,
                min_format_score,
                cutoff_format_score,
            })
        })
        .collect()
    }

    /// Tamanhos por qualidade.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou qualidade desconhecida.
    pub fn quality_definitions(&self) -> Result<Vec<QualityDefinition>> {
        let mut statement = self.connection.prepare(
            "SELECT quality, min_size, max_size, preferred_size FROM quality_definitions ORDER BY quality",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, u8>(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.map(|row| {
            let (id, min_size, max_size, preferred_size) = row?;
            Ok(QualityDefinition {
                quality: quality(id)?,
                min_size,
                max_size,
                preferred_size,
            })
        })
        .collect()
    }

    /// Grava uma busca em sombra.
    ///
    /// # Errors
    ///
    /// Falha de escrita, ou filme que não está no catálogo.
    pub fn record_shadow(&mut self, run: &ShadowRun) -> Result<()> {
        let rejections = serde_json::to_string(&run.rejections).unwrap_or_else(|_| "[]".into());
        self.connection.execute(
            "INSERT INTO shadow_runs (movie_id, at, releases, pick_title, pick_indexer,
                 pick_quality, pick_size, rejections, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run.movie_id,
                run.at,
                i64::try_from(run.releases).unwrap_or(i64::MAX),
                run.pick.as_ref().map(|p| p.title.clone()),
                run.pick.as_ref().map(|p| p.indexer.clone()),
                run.pick.as_ref().map(|p| p.quality.id()),
                run.pick
                    .as_ref()
                    .map(|p| i64::try_from(p.size).unwrap_or(i64::MAX)),
                rejections,
                run.error,
            ],
        )?;
        Ok(())
    }

    /// A busca em sombra mais recente de cada filme.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub fn latest_shadow_runs(&self) -> Result<Vec<ShadowRun>> {
        let mut statement = self.connection.prepare(
            "SELECT movie_id, at, releases, pick_title, pick_indexer, pick_quality, pick_size,
                    rejections, error
             FROM shadow_runs r
             WHERE id = (SELECT id FROM shadow_runs WHERE movie_id = r.movie_id
                         ORDER BY at DESC, id DESC LIMIT 1)
             ORDER BY at",
        )?;
        let mut rows = statement.query([])?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next()? {
            let pick = match (
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<u8>>(5)?,
            ) {
                (Some(title), Some(id)) => Some(ShadowPick {
                    title,
                    indexer: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    quality: quality(id)?,
                    size: u64::try_from(row.get::<_, Option<i64>>(6)?.unwrap_or(0)).unwrap_or(0),
                }),
                _ => None,
            };
            let rejections: String = row.get(7)?;
            runs.push(ShadowRun {
                movie_id: row.get(0)?,
                at: row.get(1)?,
                releases: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                pick,
                rejections: serde_json::from_str(&rejections)
                    .map_err(|e| StoreError::Corrupt(format!("rejeições da sombra: {e}")))?,
                error: row.get(8)?,
            });
        }
        Ok(runs)
    }

    /// Espelha a origem: cria, atualiza e remove o que for preciso. Com
    /// `apply` falso, faz tudo numa transação e desfaz.
    ///
    /// # Errors
    ///
    /// Falha de escrita; nada fica pela metade.
    pub fn import(&mut self, import: &Import, apply: bool) -> Result<ImportSummary> {
        let tx = self.connection.transaction()?;
        let mut summary = ImportSummary {
            applied: apply,
            profiles: import.profiles.len(),
            ..ImportSummary::default()
        };
        for profile in &import.profiles {
            upsert_profile(&tx, profile)?;
        }
        for definition in &import.definitions {
            tx.execute(
                "INSERT INTO quality_definitions (quality, min_size, max_size, preferred_size)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(quality) DO UPDATE SET min_size = excluded.min_size,
                     max_size = excluded.max_size, preferred_size = excluded.preferred_size",
                params![
                    definition.quality.id(),
                    definition.min_size,
                    definition.max_size,
                    definition.preferred_size
                ],
            )?;
        }

        let existing = read_movies(&tx)?;
        let seen: BTreeSet<u32> = import.movies.iter().map(|(_, m)| m.tmdb_id).collect();
        for (source_id, movie) in &import.movies {
            let current = existing.iter().find(|c| c.movie.tmdb_id == movie.tmdb_id);
            let origin = Some((import.source.clone(), *source_id));
            match current {
                Some(current) if current.movie == *movie && current.origin == origin => {
                    summary.unchanged += 1;
                }
                Some(current) => {
                    write_movie(&tx, Some(current.id), movie, &import.source, *source_id)?;
                    summary.updated.push(label(movie));
                }
                None => {
                    write_movie(&tx, None, movie, &import.source, *source_id)?;
                    summary.created.push(label(movie));
                }
            }
        }
        for gone in existing.iter().filter(|c| {
            c.origin.as_ref().is_some_and(|(s, _)| *s == import.source)
                && !seen.contains(&c.movie.tmdb_id)
        }) {
            tx.execute("DELETE FROM movies WHERE id = ?1", params![gone.id])?;
            summary.removed.push(label(&gone.movie));
        }

        if apply {
            tx.commit()?;
        } else {
            tx.rollback()?;
        }
        Ok(summary)
    }
}

fn label(movie: &Movie) -> String {
    match movie.year {
        Some(year) => format!("{} ({year})", movie.title),
        None => movie.title.clone(),
    }
}

#[derive(Serialize, Deserialize)]
struct StoredItem {
    name: String,
    qualities: Vec<u8>,
    allowed: bool,
}

fn encode_items(items: &[ProfileItem]) -> String {
    let stored: Vec<_> = items
        .iter()
        .map(|item| StoredItem {
            name: item.name.clone(),
            qualities: item.qualities.iter().map(|q| q.id()).collect(),
            allowed: item.allowed,
        })
        .collect();
    serde_json::to_string(&stored).unwrap_or_else(|_| "[]".into())
}

fn decode_items(text: &str) -> Result<Vec<ProfileItem>> {
    let stored: Vec<StoredItem> = serde_json::from_str(text)
        .map_err(|e| StoreError::Corrupt(format!("itens de perfil: {e}")))?;
    stored
        .into_iter()
        .map(|item| {
            Ok(ProfileItem {
                qualities: item
                    .qualities
                    .into_iter()
                    .map(quality)
                    .collect::<Result<_>>()?,
                name: item.name,
                allowed: item.allowed,
            })
        })
        .collect()
}

fn quality(id: u8) -> Result<Quality> {
    Quality::from_id(id).ok_or_else(|| StoreError::Corrupt(format!("qualidade {id}")))
}

fn upsert_profile(tx: &Transaction<'_>, profile: &QualityProfile) -> Result<()> {
    tx.execute(
        "INSERT INTO quality_profiles (name, upgrade_allowed, cutoff, language, items,
             min_format_score, cutoff_format_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(name) DO UPDATE SET upgrade_allowed = excluded.upgrade_allowed,
             cutoff = excluded.cutoff, language = excluded.language, items = excluded.items,
             min_format_score = excluded.min_format_score,
             cutoff_format_score = excluded.cutoff_format_score",
        params![
            profile.name,
            profile.upgrade_allowed,
            profile.cutoff.and_then(|c| i64::try_from(c).ok()),
            profile.language,
            encode_items(&profile.items),
            profile.min_format_score,
            profile.cutoff_format_score,
        ],
    )?;
    Ok(())
}

fn write_movie(
    tx: &Transaction<'_>,
    id: Option<i64>,
    movie: &Movie,
    source: &str,
    source_id: i64,
) -> Result<()> {
    let profile_id: Option<i64> = match &movie.quality_profile {
        Some(name) => tx
            .query_row(
                "SELECT id FROM quality_profiles WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    let values = params![
        movie.tmdb_id,
        movie.imdb_id,
        movie.title,
        movie.original_title,
        movie.original_language,
        movie.year,
        movie.status,
        movie.minimum_availability,
        movie.monitored,
        profile_id,
        movie.path,
        movie.added,
        source,
        source_id,
        movie.runtime,
        movie.secondary_year,
        movie.clean_title,
        movie.available,
    ];
    let id = if let Some(id) = id {
        tx.execute(
            "UPDATE movies SET tmdb_id = ?1, imdb_id = ?2, title = ?3, original_title = ?4,
                 original_language = ?5, year = ?6, status = ?7, minimum_availability = ?8,
                 monitored = ?9, quality_profile_id = ?10, path = ?11, added = ?12,
                 source = ?13, source_id = ?14, runtime = ?15, secondary_year = ?16,
                 clean_title = ?17, available = ?18
             WHERE id = ?19",
            rusqlite::params_from_iter(values.iter().copied().chain([&id as &dyn rusqlite::ToSql])),
        )?;
        id
    } else {
        tx.execute(
            "INSERT INTO movies (tmdb_id, imdb_id, title, original_title, original_language,
                 year, status, minimum_availability, monitored, quality_profile_id, path,
                 added, source, source_id, runtime, secondary_year, clean_title, available)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18)",
            values,
        )?;
        tx.last_insert_rowid()
    };

    tx.execute("DELETE FROM movie_titles WHERE movie_id = ?1", params![id])?;
    for title in &movie.alternate_titles {
        tx.execute(
            "INSERT INTO movie_titles (movie_id, title) VALUES (?1, ?2)",
            params![id, title],
        )?;
    }
    tx.execute("DELETE FROM movie_files WHERE movie_id = ?1", params![id])?;
    if let Some(file) = &movie.file {
        tx.execute(
            "INSERT INTO movie_files (movie_id, relative_path, size, quality, revision_version,
                 revision_real, is_repack, languages, release_group, edition, scene_name, date_added)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                file.relative_path,
                i64::try_from(file.size).unwrap_or(i64::MAX),
                file.quality.quality.id(),
                file.quality.revision.version,
                file.quality.revision.real,
                file.quality.revision.is_repack,
                serde_json::to_string(&file.languages).unwrap_or_else(|_| "[]".into()),
                file.release_group,
                file.edition,
                file.scene_name,
                file.date_added,
            ],
        )?;
    }
    Ok(())
}

fn read_movies(connection: &Connection) -> Result<Vec<CatalogMovie>> {
    let mut statement = connection.prepare(
        "SELECT m.id, m.tmdb_id, m.imdb_id, m.title, m.original_title, m.original_language,
                m.year, m.status, m.minimum_availability, m.monitored, p.name, m.path, m.added,
                m.source, m.source_id,
                f.relative_path, f.size, f.quality, f.revision_version, f.revision_real,
                f.is_repack, f.languages, f.release_group, f.edition, f.scene_name, f.date_added,
                m.runtime, m.secondary_year, m.clean_title, m.available
         FROM movies m
         LEFT JOIN quality_profiles p ON p.id = m.quality_profile_id
         LEFT JOIN movie_files f ON f.movie_id = m.id
         ORDER BY m.title COLLATE NOCASE, m.year",
    )?;
    let mut rows = statement.query([])?;
    let mut movies = Vec::new();
    while let Some(row) = rows.next()? {
        let file = match row.get::<_, Option<String>>(15)? {
            Some(relative_path) => {
                let languages: String = row.get(21)?;
                Some(MovieFile {
                    relative_path,
                    size: u64::try_from(row.get::<_, i64>(16)?).unwrap_or(0),
                    quality: QualityModel {
                        quality: quality(row.get(17)?)?,
                        revision: Revision {
                            version: row.get(18)?,
                            real: row.get(19)?,
                            is_repack: row.get(20)?,
                        },
                    },
                    languages: serde_json::from_str(&languages)
                        .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?,
                    release_group: row.get(22)?,
                    edition: row.get(23)?,
                    scene_name: row.get(24)?,
                    date_added: row.get(25)?,
                })
            }
            None => None,
        };
        let origin = match (
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<i64>>(14)?,
        ) {
            (Some(source), Some(id)) => Some((source, id)),
            _ => None,
        };
        movies.push(CatalogMovie {
            id: row.get(0)?,
            origin,
            movie: Movie {
                tmdb_id: row.get(1)?,
                imdb_id: row.get(2)?,
                title: row.get(3)?,
                original_title: row.get(4)?,
                original_language: row.get(5)?,
                year: row.get(6)?,
                status: row.get(7)?,
                minimum_availability: row.get(8)?,
                monitored: row.get(9)?,
                quality_profile: row.get(10)?,
                path: row.get(11)?,
                added: row.get(12)?,
                file,
                runtime: row.get(26)?,
                secondary_year: row.get(27)?,
                clean_title: row.get(28)?,
                available: row.get(29)?,
                alternate_titles: Vec::new(),
            },
        });
    }
    let mut titles =
        connection.prepare("SELECT movie_id, title FROM movie_titles ORDER BY rowid")?;
    let mut rows = titles.query([])?;
    while let Some(row) = rows.next()? {
        let (id, title): (i64, String) = (row.get(0)?, row.get(1)?);
        if let Some(entry) = movies.iter_mut().find(|m| m.id == id) {
            entry.movie.alternate_titles.push(title);
        }
    }
    Ok(movies)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> QualityProfile {
        QualityProfile {
            name: "Any".into(),
            upgrade_allowed: false,
            cutoff: Some(1),
            language: Some("Original".into()),
            items: vec![
                ProfileItem {
                    name: "WEB 1080p".into(),
                    qualities: vec![Quality::WebDl1080p, Quality::WebRip1080p],
                    allowed: true,
                },
                ProfileItem {
                    name: "Bluray-1080p".into(),
                    qualities: vec![Quality::Bluray1080p],
                    allowed: true,
                },
            ],
            min_format_score: 0,
            cutoff_format_score: 0,
        }
    }

    fn movie(tmdb_id: u32, title: &str, with_file: bool) -> Movie {
        Movie {
            tmdb_id,
            imdb_id: Some(format!("tt{tmdb_id:07}")),
            title: title.into(),
            original_title: None,
            original_language: Some("English".into()),
            year: Some(2020),
            status: Some("released".into()),
            minimum_availability: Some("announced".into()),
            monitored: true,
            quality_profile: Some("Any".into()),
            path: format!("/filmes/{title} (2020)"),
            added: Some("2026-01-01T00:00:00Z".into()),
            file: with_file.then(|| MovieFile {
                relative_path: format!("{title} (2020).mkv"),
                size: 4_000_000_000,
                quality: QualityModel {
                    quality: Quality::WebDl1080p,
                    revision: Revision::default(),
                },
                languages: vec!["Portuguese (Brazil)".into(), "English".into()],
                release_group: Some("GRUPO".into()),
                edition: None,
                scene_name: Some(format!("{title}.2020.1080p.WEB-DL-GRUPO")),
                date_added: Some("2026-01-02T00:00:00Z".into()),
            }),
            runtime: 110,
            secondary_year: None,
            clean_title: Some(title.to_lowercase()),
            alternate_titles: vec![format!("{title} alternativo")],
            available: true,
        }
    }

    fn import(movies: Vec<(i64, Movie)>) -> Import {
        Import {
            source: "radarr:filmes".into(),
            profiles: vec![profile()],
            definitions: vec![QualityDefinition {
                quality: Quality::WebDl1080p,
                min_size: Some(5.0),
                max_size: Some(400.0),
                preferred_size: None,
            }],
            movies,
        }
    }

    #[test]
    fn simulacao_nao_grava_e_relata_o_mesmo_que_a_aplicacao() {
        let mut store = Store::open_in_memory().unwrap();
        let first = import(vec![
            (1, movie(10, "Um", true)),
            (2, movie(20, "Dois", false)),
        ]);

        let simulated = store.import(&first, false).unwrap();
        assert!(store.movies().unwrap().is_empty());
        let applied = store.import(&first, true).unwrap();
        assert_eq!(simulated.created, applied.created);
        assert_eq!(applied.created, ["Um (2020)", "Dois (2020)"]);

        let movies = store.movies().unwrap();
        assert_eq!(movies.len(), 2);
        let um = movies.iter().find(|m| m.movie.tmdb_id == 10).unwrap();
        assert_eq!(um.movie, movie(10, "Um", true));
        assert_eq!(um.origin, Some(("radarr:filmes".into(), 1)));
        assert_eq!(store.profiles().unwrap(), [profile()]);
        assert_eq!(store.quality_definitions().unwrap().len(), 1);
    }

    #[test]
    fn reimportar_atualiza_mantem_e_remove_so_o_que_veio_da_origem() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .import(
                &import(vec![
                    (1, movie(10, "Um", false)),
                    (2, movie(20, "Dois", false)),
                ]),
                true,
            )
            .unwrap();

        let again = store
            .import(&import(vec![(1, movie(10, "Um", true))]), true)
            .unwrap();
        assert_eq!(again.updated, ["Um (2020)"]);
        assert_eq!(again.removed, ["Dois (2020)"]);
        assert!(again.created.is_empty());

        let same = store
            .import(&import(vec![(1, movie(10, "Um", true))]), true)
            .unwrap();
        assert_eq!(same.unchanged, 1);
        assert!(same.updated.is_empty() && same.removed.is_empty());
    }

    #[test]
    fn banco_da_versao_1_migra_sem_perder_filme() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0]).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection
            .execute(
                "INSERT INTO movies (tmdb_id, title, monitored, path) VALUES (10, 'Um', 1, '/f/Um')",
                [],
            )
            .unwrap();
        let store = Store::setup(connection).unwrap();
        let movies = store.movies().unwrap();
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].movie.runtime, 0);
        assert!(!movies[0].movie.available);
        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, i64::try_from(MIGRATIONS.len()).unwrap());
    }

    #[test]
    fn sombra_guarda_a_ultima_de_cada_filme() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .import(&import(vec![(1, movie(10, "Um", false))]), true)
            .unwrap();
        let id = store.movies().unwrap()[0].id;
        let run = |at: &str, pick: bool| ShadowRun {
            movie_id: id,
            at: at.into(),
            releases: 3,
            pick: pick.then(|| ShadowPick {
                title: "Um.2020.1080p.WEB-DL-GRUPO".into(),
                indexer: "tracker".into(),
                quality: Quality::WebDl1080p,
                size: 4_000_000_000,
            }),
            rejections: vec![("MinimumSeeders".into(), 2)],
            error: None,
        };
        store
            .record_shadow(&run("2026-01-01T00:00:00Z", false))
            .unwrap();
        store
            .record_shadow(&run("2026-01-02T00:00:00Z", true))
            .unwrap();
        assert_eq!(
            store.latest_shadow_runs().unwrap(),
            [run("2026-01-02T00:00:00Z", true)]
        );
    }

    #[test]
    fn arquivo_em_disco_sobrevive_a_reabertura() {
        let dir = std::env::temp_dir().join(format!("acervo-store-{}", std::process::id()));
        let path = dir.join("sub").join("acervo.db");
        {
            let mut store = Store::open(&path).unwrap();
            store
                .import(&import(vec![(1, movie(10, "Um", true))]), true)
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.movies().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

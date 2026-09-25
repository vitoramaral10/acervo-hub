//! Catálogo persistente: filmes, o arquivo de cada um, os perfis de
//! qualidade e as contas da interface, num banco Postgres.
//!
//! Nesta etapa o catálogo é um espelho do gerenciador de filmes em produção,
//! importado pela API dele. A importação roda numa transação: a simulação faz
//! o mesmo trabalho e desfaz no fim, então o que ela relata é exatamente o que
//! a aplicação faria.

mod accounts;

use std::collections::BTreeSet;

use acervo_parser::{Quality, QualityModel, Revision};
use deadpool_postgres::{GenericClient, Manager, ManagerConfig, Pool, RecyclingMethod};
use serde::{Deserialize, Serialize};
use tokio_postgres::{NoTls, Row};

pub use accounts::SESSION_DAYS;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("banco de dados: {0}")]
    Postgres(#[from] tokio_postgres::Error),

    #[error("conexão com o banco: {0}")]
    Pool(#[from] deadpool_postgres::PoolError),

    #[error("endereço do banco inválido: {0}")]
    Url(String),

    #[error("registro inconsistente no banco: {0}")]
    Corrupt(String),

    #[error("senha: {0}")]
    Password(String),
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
    /// Id do perfil no gerenciador de onde ele veio.
    pub source_id: Option<i64>,
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
    /// Id estável do arquivo: o do gerenciador, se veio de lá. Quem guarda
    /// esse id (o app de legendas guarda) percebe troca de arquivo por ele.
    pub id: Option<i64>,
    /// Faixas de áudio, vídeo e legenda, como o gerenciador as leu.
    pub media_info: Option<serde_json::Value>,
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
    /// Estreia no cinema, `AAAA-MM-DD`.
    pub in_cinemas: Option<String>,
    pub digital_release: Option<String>,
    pub physical_release: Option<String>,
    pub overview: Option<String>,
    /// Ids da tabela de tags.
    pub tags: Vec<i64>,
}

/// O que só a base de metadados sabe e o gerenciador não expõe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovieExtras {
    /// Título em inglês: o que dá nome à pasta e ao arquivo.
    pub metadata_title: Option<String>,
    pub poster: Option<String>,
    pub fanart: Option<String>,
    /// Quando os metadados vieram da base pela última vez (RFC 3339).
    pub refreshed_at: Option<String>,
}

/// Uma tag, como a API v3 a mostra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

/// Um filme do catálogo, com o id local e de onde veio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMovie {
    pub id: i64,
    pub movie: Movie,
    /// Instância de origem e o id do filme lá, se veio de importação. Sem
    /// origem, o acervo é o dono do filme.
    pub origin: Option<(String, i64)>,
    pub extras: MovieExtras,
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

/// Em que pé está um download que o acervo mandou ao cliente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrabState {
    /// No cliente, ainda sem importar.
    Downloading,
    /// Arquivo ligado na pasta do filme.
    Imported,
    /// Desistiu-se dele; `message` diz por quê.
    Failed,
}

impl GrabState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Imported => "imported",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "downloading" => Ok(Self::Downloading),
            "imported" => Ok(Self::Imported),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::Corrupt(format!("estado de grab `{other}`"))),
        }
    }
}

/// Um release mandado ao cliente de download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grab {
    pub id: i64,
    pub movie_id: i64,
    /// Infohash, em hex minúsculo: é por ele que o torrent é achado no cliente.
    pub hash: String,
    pub title: String,
    pub indexer: String,
    pub quality: Quality,
    pub size: u64,
    /// RFC 3339, em UTC.
    pub grabbed_at: String,
    pub state: GrabState,
    pub message: Option<String>,
    /// Onde o arquivo foi ligado, como o gerenciador vê.
    pub imported_path: Option<String>,
    pub finished_at: Option<String>,
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
    /// Tags da origem, com os ids de lá.
    pub tags: Vec<Tag>,
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

/// Cada migração roda uma vez, na ordem; a posição é a versão.
const MIGRATIONS: &[&str] = &[
    r"
    CREATE TABLE quality_profiles (
        id BIGSERIAL PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        upgrade_allowed BOOLEAN NOT NULL,
        cutoff INTEGER,
        language TEXT,
        items JSONB NOT NULL,
        min_format_score INTEGER NOT NULL DEFAULT 0,
        cutoff_format_score INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE movies (
        id BIGSERIAL PRIMARY KEY,
        tmdb_id BIGINT NOT NULL UNIQUE,
        imdb_id TEXT,
        title TEXT NOT NULL,
        original_title TEXT,
        original_language TEXT,
        year INTEGER,
        status TEXT,
        minimum_availability TEXT,
        monitored BOOLEAN NOT NULL,
        quality_profile_id BIGINT REFERENCES quality_profiles(id),
        path TEXT NOT NULL,
        added TEXT,
        source TEXT,
        source_id BIGINT,
        runtime INTEGER NOT NULL DEFAULT 0,
        secondary_year INTEGER,
        clean_title TEXT,
        available BOOLEAN NOT NULL DEFAULT FALSE
    );
    CREATE TABLE movie_files (
        movie_id BIGINT PRIMARY KEY REFERENCES movies(id) ON DELETE CASCADE,
        relative_path TEXT NOT NULL,
        size BIGINT NOT NULL,
        quality SMALLINT NOT NULL,
        revision_version SMALLINT NOT NULL,
        revision_real SMALLINT NOT NULL,
        is_repack BOOLEAN NOT NULL,
        languages JSONB NOT NULL,
        release_group TEXT,
        edition TEXT,
        scene_name TEXT,
        date_added TEXT
    );
    CREATE TABLE movie_titles (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
        title TEXT NOT NULL
    );
    CREATE INDEX movie_titles_by_movie ON movie_titles(movie_id);
    CREATE TABLE quality_definitions (
        quality SMALLINT PRIMARY KEY,
        min_size DOUBLE PRECISION,
        max_size DOUBLE PRECISION,
        preferred_size DOUBLE PRECISION
    );
    CREATE TABLE shadow_runs (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
        at TEXT NOT NULL,
        releases BIGINT NOT NULL,
        pick_title TEXT,
        pick_indexer TEXT,
        pick_quality SMALLINT,
        pick_size BIGINT,
        rejections JSONB NOT NULL,
        error TEXT
    );
    CREATE INDEX shadow_runs_by_movie ON shadow_runs(movie_id, at);
    CREATE TABLE users (
        name TEXT PRIMARY KEY,
        password_hash TEXT NOT NULL,
        created_at TIMESTAMPTZ NOT NULL DEFAULT now()
    );
    CREATE TABLE sessions (
        token_hash BYTEA PRIMARY KEY,
        user_name TEXT NOT NULL REFERENCES users(name) ON DELETE CASCADE,
        created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
        expires_at TIMESTAMPTZ NOT NULL
    );
    CREATE INDEX sessions_by_expiry ON sessions(expires_at);
",
    r"
    CREATE TABLE grabs (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
        hash TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL,
        indexer TEXT NOT NULL,
        quality SMALLINT NOT NULL,
        size BIGINT NOT NULL,
        grabbed_at TEXT NOT NULL,
        state TEXT NOT NULL,
        message TEXT,
        imported_path TEXT,
        finished_at TEXT
    );
    CREATE INDEX grabs_by_movie ON grabs(movie_id, grabbed_at);
",
    r"
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
",
    r"
    ALTER TABLE movies
        ADD COLUMN in_cinemas TEXT,
        ADD COLUMN digital_release TEXT,
        ADD COLUMN physical_release TEXT,
        ADD COLUMN overview TEXT,
        ADD COLUMN tags JSONB NOT NULL DEFAULT '[]',
        ADD COLUMN metadata_title TEXT,
        ADD COLUMN poster TEXT,
        ADD COLUMN fanart TEXT,
        ADD COLUMN metadata_refreshed_at TEXT;
    CREATE TABLE tags (
        id BIGSERIAL PRIMARY KEY,
        label TEXT NOT NULL UNIQUE
    );
",
    r"
    ALTER TABLE quality_profiles ADD COLUMN source_id BIGINT;
    ALTER TABLE movie_files ADD COLUMN file_id BIGINT, ADD COLUMN media_info JSONB;
    CREATE SEQUENCE movie_file_ids START 1000000;
    ALTER TABLE movies DROP CONSTRAINT movies_quality_profile_id_fkey,
        ADD CONSTRAINT movies_quality_profile_id_fkey FOREIGN KEY (quality_profile_id)
            REFERENCES quality_profiles(id) ON UPDATE CASCADE;
    ALTER TABLE movie_files DROP CONSTRAINT movie_files_movie_id_fkey,
        ADD CONSTRAINT movie_files_movie_id_fkey FOREIGN KEY (movie_id)
            REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE;
    ALTER TABLE movie_titles DROP CONSTRAINT movie_titles_movie_id_fkey,
        ADD CONSTRAINT movie_titles_movie_id_fkey FOREIGN KEY (movie_id)
            REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE;
    ALTER TABLE shadow_runs DROP CONSTRAINT shadow_runs_movie_id_fkey,
        ADD CONSTRAINT shadow_runs_movie_id_fkey FOREIGN KEY (movie_id)
            REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE;
    ALTER TABLE grabs DROP CONSTRAINT grabs_movie_id_fkey,
        ADD CONSTRAINT grabs_movie_id_fkey FOREIGN KEY (movie_id)
            REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE;
",
];

/// Chave do lock consultivo que serializa as migrações: o serviço e um
/// comando avulso podem subir juntos.
const MIGRATION_LOCK: i64 = 0x6163_6572_766f; // "acervo"

/// Conexões com o banco. Clonar é barato e divide o mesmo pool.
#[derive(Clone)]
pub struct Store {
    pool: Pool,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A configuração do pool carrega a senha do banco.
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Conecta pelo endereço (`postgres://usuário:senha@host/banco`) e aplica
    /// as migrações pendentes.
    ///
    /// # Errors
    ///
    /// Endereço inválido, banco inalcançável ou migração que falha.
    pub async fn connect(url: &str) -> Result<Self> {
        let config: tokio_postgres::Config = url
            .parse()
            .map_err(|e: tokio_postgres::Error| StoreError::Url(e.to_string()))?;
        Self::with_config(config).await
    }

    /// Como [`Store::connect`], a partir da configuração já montada — é assim
    /// que os testes isolam cada um no seu schema.
    ///
    /// # Errors
    ///
    /// Banco inalcançável ou migração que falha.
    pub async fn with_config(config: tokio_postgres::Config) -> Result<Self> {
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(8)
            .build()
            .map_err(|e| StoreError::Url(e.to_string()))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK])
            .await?;
        tx.batch_execute("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")
            .await?;
        let version: i32 = tx
            .query_opt("SELECT version FROM schema_version", &[])
            .await?
            .map_or(Ok(0), |row| row.try_get(0))?;
        let skip = usize::try_from(version).unwrap_or(0);
        for (index, migration) in (1_i32..).zip(MIGRATIONS).skip(skip) {
            tx.batch_execute(migration).await?;
            tx.execute("DELETE FROM schema_version", &[]).await?;
            tx.execute(
                "INSERT INTO schema_version (version) VALUES ($1)",
                &[&index],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Todos os filmes, por título.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn movies(&self) -> Result<Vec<CatalogMovie>> {
        read_movies(&self.pool.get().await?).await
    }

    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn profiles(&self) -> Result<Vec<QualityProfile>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT name, upgrade_allowed, cutoff, language, items, min_format_score,
                        cutoff_format_score, source_id
                 FROM quality_profiles ORDER BY name",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                let items: serde_json::Value = row.try_get(4)?;
                Ok(QualityProfile {
                    name: row.try_get(0)?,
                    upgrade_allowed: row.try_get(1)?,
                    cutoff: row
                        .try_get::<_, Option<i32>>(2)?
                        .and_then(|c| usize::try_from(c).ok()),
                    language: row.try_get(3)?,
                    items: decode_items(items)?,
                    min_format_score: row.try_get(5)?,
                    cutoff_format_score: row.try_get(6)?,
                    source_id: row.try_get(7)?,
                })
            })
            .collect()
    }

    /// Os perfis com o id do catálogo, que é o que a API v3 mostra.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn profile_ids(&self) -> Result<Vec<(i64, String)>> {
        let client = self.pool.get().await?;
        client
            .query("SELECT id, name FROM quality_profiles ORDER BY id", &[])
            .await?
            .iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
            .collect()
    }

    /// Tamanhos por qualidade.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou qualidade desconhecida.
    pub async fn quality_definitions(&self) -> Result<Vec<QualityDefinition>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT quality, min_size, max_size, preferred_size
                 FROM quality_definitions ORDER BY quality",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok(QualityDefinition {
                    quality: quality(row.try_get(0)?)?,
                    min_size: row.try_get(1)?,
                    max_size: row.try_get(2)?,
                    preferred_size: row.try_get(3)?,
                })
            })
            .collect()
    }

    /// Grava uma busca em sombra.
    ///
    /// # Errors
    ///
    /// Falha de escrita, ou filme que não está no catálogo.
    pub async fn record_shadow(&self, run: &ShadowRun) -> Result<()> {
        let client = self.pool.get().await?;
        let rejections = serde_json::to_value(&run.rejections)
            .map_err(|e| StoreError::Corrupt(format!("rejeições da sombra: {e}")))?;
        let pick = run.pick.as_ref();
        client
            .execute(
                "INSERT INTO shadow_runs (movie_id, at, releases, pick_title, pick_indexer,
                     pick_quality, pick_size, rejections, error)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &run.movie_id,
                    &run.at,
                    &i64::try_from(run.releases).unwrap_or(i64::MAX),
                    &pick.map(|p| p.title.as_str()),
                    &pick.map(|p| p.indexer.as_str()),
                    &pick.map(|p| i16::from(p.quality.id())),
                    &pick.map(|p| i64::try_from(p.size).unwrap_or(i64::MAX)),
                    &rejections,
                    &run.error,
                ],
            )
            .await?;
        Ok(())
    }

    /// A busca em sombra mais recente de cada filme.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn latest_shadow_runs(&self) -> Result<Vec<ShadowRun>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT DISTINCT ON (movie_id) movie_id, at, releases, pick_title, pick_indexer,
                        pick_quality, pick_size, rejections, error
                 FROM shadow_runs
                 ORDER BY movie_id, at DESC, id DESC",
                &[],
            )
            .await?;
        let mut runs = rows
            .iter()
            .map(|row| {
                let pick = match (
                    row.try_get::<_, Option<String>>(3)?,
                    row.try_get::<_, Option<i16>>(5)?,
                ) {
                    (Some(title), Some(id)) => Some(ShadowPick {
                        title,
                        indexer: row.try_get::<_, Option<String>>(4)?.unwrap_or_default(),
                        quality: quality(id)?,
                        size: u64::try_from(row.try_get::<_, Option<i64>>(6)?.unwrap_or(0))
                            .unwrap_or(0),
                    }),
                    _ => None,
                };
                let rejections: serde_json::Value = row.try_get(7)?;
                Ok(ShadowRun {
                    movie_id: row.try_get(0)?,
                    at: row.try_get(1)?,
                    releases: usize::try_from(row.try_get::<_, i64>(2)?).unwrap_or(0),
                    pick,
                    rejections: serde_json::from_value(rejections)
                        .map_err(|e| StoreError::Corrupt(format!("rejeições da sombra: {e}")))?,
                    error: row.try_get(8)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        runs.sort_by(|a, b| a.at.cmp(&b.at));
        Ok(runs)
    }

    /// Adiciona um filme de que o acervo é dono. Devolve o id.
    ///
    /// # Errors
    ///
    /// Filme já no catálogo (mesmo TMDB) ou falha de escrita.
    pub async fn add_movie(&self, movie: &Movie, extras: &MovieExtras) -> Result<i64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let id = write_movie(&tx, None, movie, None).await?;
        write_extras(&tx, id, extras).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Regrava um filme do acervo, mantendo a origem que ele tiver.
    ///
    /// # Errors
    ///
    /// Filme inexistente ou falha de escrita.
    pub async fn update_movie(&self, id: i64, movie: &Movie) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let origin: Option<(Option<String>, Option<i64>)> = tx
            .query_opt("SELECT source, source_id FROM movies WHERE id = $1", &[&id])
            .await?
            .map(|row| Ok::<_, StoreError>((row.try_get(0)?, row.try_get(1)?)))
            .transpose()?;
        let Some((source, source_id)) = origin else {
            return Err(StoreError::Corrupt(format!("filme {id} não existe")));
        };
        let origin = source.as_deref().zip(source_id);
        write_movie(&tx, Some(id), movie, origin).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Grava o que só a base de metadados sabe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_extras(&self, id: i64, extras: &MovieExtras) -> Result<()> {
        write_extras(&self.pool.get().await?, id, extras).await
    }

    /// O acervo passa a ser dono de todos os filmes: importar do gerenciador
    /// deixa de mexer neles. Filmes e perfis ganham o id que tinham lá — é
    /// por ele que os apps de pedidos e de legendas os conhecem —, e o que o
    /// acervo tinha criado vai para depois do maior. Devolve quantos filmes
    /// vieram da origem.
    ///
    /// # Errors
    ///
    /// Falha de escrita; nada fica pela metade.
    pub async fn adopt_all(&self) -> Result<u64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let adopted = tx
            .execute("SELECT 1 FROM movies WHERE source IS NOT NULL", &[])
            .await?;
        for (table, sequence_owner) in [
            ("quality_profiles", "quality_profiles"),
            ("movies", "movies"),
        ] {
            tx.batch_execute(&format!(
                "UPDATE {table} SET id = -id;
                 UPDATE {table} SET id = source_id WHERE source_id IS NOT NULL;
                 UPDATE {table} t SET id = sub.new_id
                 FROM (SELECT id, (SELECT COALESCE(MAX(id), 0) FROM {table} WHERE id > 0)
                              + ROW_NUMBER() OVER (ORDER BY id DESC) AS new_id
                       FROM {table} WHERE id < 0) sub
                 WHERE t.id = sub.id;
                 SELECT setval(pg_get_serial_sequence('{sequence_owner}', 'id'),
                     GREATEST((SELECT COALESCE(MAX(id), 0) FROM {table}), 1));"
            ))
            .await?;
        }
        tx.batch_execute(
            "UPDATE movies SET source = NULL, source_id = NULL;
             UPDATE quality_profiles SET source_id = NULL;",
        )
        .await?;
        tx.commit().await?;
        Ok(adopted)
    }

    /// Tira um filme do catálogo, com o registro do arquivo, títulos, sombra
    /// e grabs. O arquivo no disco não é tocado aqui. `false` se não existia.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn delete_movie(&self, id: i64) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute("DELETE FROM movies WHERE id = $1", &[&id])
            .await?
            > 0)
    }

    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn tags(&self) -> Result<Vec<Tag>> {
        let client = self.pool.get().await?;
        client
            .query("SELECT id, label FROM tags ORDER BY id", &[])
            .await?
            .iter()
            .map(|row| {
                Ok(Tag {
                    id: row.try_get(0)?,
                    label: row.try_get(1)?,
                })
            })
            .collect()
    }

    /// Cria a tag, ou devolve a que já tem esse rótulo.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn create_tag(&self, label: &str) -> Result<Tag> {
        let client = self.pool.get().await?;
        let label = label.trim().to_lowercase();
        let row = client
            .query_one(
                "INSERT INTO tags (label) VALUES ($1)
                 ON CONFLICT (label) DO UPDATE SET label = excluded.label
                 RETURNING id",
                &[&label],
            )
            .await?;
        Ok(Tag {
            id: row.try_get(0)?,
            label,
        })
    }

    /// # Errors
    ///
    /// Rótulo já usado por outra tag ou falha de escrita.
    pub async fn rename_tag(&self, id: i64, label: &str) -> Result<Option<Tag>> {
        let client = self.pool.get().await?;
        let label = label.trim().to_lowercase();
        let changed = client
            .execute("UPDATE tags SET label = $2 WHERE id = $1", &[&id, &label])
            .await?;
        Ok((changed > 0).then_some(Tag { id, label }))
    }

    /// Espelha as tags do gerenciador com os mesmos ids: quem guardou o id
    /// de uma tag (o app de pedidos guarda) continua achando a mesma.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn mirror_tags(&self, tags: &[Tag]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        write_tags(&tx, tags).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Uma configuração guardada pela interface.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        let client = self.pool.get().await?;
        Ok(client
            .query_opt("SELECT value FROM settings WHERE key = $1", &[&key])
            .await?
            .map(|row| row.try_get(0))
            .transpose()?)
    }

    /// Grava uma configuração; `None` apaga.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_setting(&self, key: &str, value: Option<&str>, at: &str) -> Result<()> {
        let client = self.pool.get().await?;
        match value {
            Some(value) => {
                client
                    .execute(
                        "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3)
                         ON CONFLICT (key) DO UPDATE SET value = excluded.value,
                             updated_at = excluded.updated_at",
                        &[&key, &value, &at],
                    )
                    .await?;
            }
            None => {
                client
                    .execute("DELETE FROM settings WHERE key = $1", &[&key])
                    .await?;
            }
        }
        Ok(())
    }

    /// Registra um release mandado ao cliente. Devolve o id.
    ///
    /// # Errors
    ///
    /// Falha de escrita, filme fora do catálogo ou hash já registrado.
    pub async fn record_grab(&self, grab: &Grab) -> Result<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "INSERT INTO grabs (movie_id, hash, title, indexer, quality, size, grabbed_at,
                     state, message)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 RETURNING id",
                &[
                    &grab.movie_id,
                    &grab.hash,
                    &grab.title,
                    &grab.indexer,
                    &i16::from(grab.quality.id()),
                    &i64::try_from(grab.size).unwrap_or(i64::MAX),
                    &grab.grabbed_at,
                    &grab.state.as_str(),
                    &grab.message,
                ],
            )
            .await?;
        Ok(row.try_get(0)?)
    }

    /// Todos os grabs, do mais novo ao mais velho.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn grabs(&self) -> Result<Vec<Grab>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT id, movie_id, hash, title, indexer, quality, size, grabbed_at, state,
                        message, imported_path, finished_at
                 FROM grabs ORDER BY grabbed_at DESC, id DESC",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(Grab {
                    id: row.try_get(0)?,
                    movie_id: row.try_get(1)?,
                    hash: row.try_get(2)?,
                    title: row.try_get(3)?,
                    indexer: row.try_get(4)?,
                    quality: quality(row.try_get(5)?)?,
                    size: u64::try_from(row.try_get::<_, i64>(6)?).unwrap_or(0),
                    grabbed_at: row.try_get(7)?,
                    state: GrabState::parse(row.try_get(8)?)?,
                    message: row.try_get(9)?,
                    imported_path: row.try_get(10)?,
                    finished_at: row.try_get(11)?,
                })
            })
            .collect()
    }

    /// Encerra um grab: importado (com o caminho) ou desistido (com o
    /// motivo). `message` sem mudar o estado serve de anotação ("ainda
    /// baixando: 40%").
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn update_grab(
        &self,
        id: i64,
        state: GrabState,
        message: Option<&str>,
        imported_path: Option<&str>,
        at: &str,
    ) -> Result<()> {
        let client = self.pool.get().await?;
        let finished = (state != GrabState::Downloading).then_some(at);
        client
            .execute(
                "UPDATE grabs SET state = $2, message = $3,
                     imported_path = COALESCE($4, imported_path),
                     finished_at = COALESCE($5, finished_at)
                 WHERE id = $1",
                &[&id, &state.as_str(), &message, &imported_path, &finished],
            )
            .await?;
        Ok(())
    }

    /// Espelha a origem: cria, atualiza e remove o que for preciso. Com
    /// `apply` falso, faz tudo numa transação e desfaz.
    ///
    /// # Errors
    ///
    /// Falha de escrita; nada fica pela metade.
    pub async fn import(&self, import: &Import, apply: bool) -> Result<ImportSummary> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut summary = ImportSummary {
            applied: apply,
            profiles: import.profiles.len(),
            ..ImportSummary::default()
        };
        for profile in &import.profiles {
            upsert_profile(&tx, profile).await?;
        }
        write_tags(&tx, &import.tags).await?;
        for definition in &import.definitions {
            tx.execute(
                "INSERT INTO quality_definitions (quality, min_size, max_size, preferred_size)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (quality) DO UPDATE SET min_size = excluded.min_size,
                     max_size = excluded.max_size, preferred_size = excluded.preferred_size",
                &[
                    &i16::from(definition.quality.id()),
                    &definition.min_size,
                    &definition.max_size,
                    &definition.preferred_size,
                ],
            )
            .await?;
        }

        let existing = read_movies(&tx).await?;
        let seen: BTreeSet<u32> = import.movies.iter().map(|(_, m)| m.tmdb_id).collect();
        for (source_id, movie) in &import.movies {
            let current = existing.iter().find(|c| c.movie.tmdb_id == movie.tmdb_id);
            let origin = Some((import.source.clone(), *source_id));
            match current {
                Some(current) if current.movie == *movie && current.origin == origin => {
                    summary.unchanged += 1;
                }
                Some(current) => {
                    write_movie(
                        &tx,
                        Some(current.id),
                        movie,
                        Some((&import.source, *source_id)),
                    )
                    .await?;
                    summary.updated.push(label(movie));
                }
                None => {
                    write_movie(&tx, None, movie, Some((&import.source, *source_id))).await?;
                    summary.created.push(label(movie));
                }
            }
        }
        for gone in existing.iter().filter(|c| {
            c.origin.as_ref().is_some_and(|(s, _)| *s == import.source)
                && !seen.contains(&c.movie.tmdb_id)
        }) {
            tx.execute("DELETE FROM movies WHERE id = $1", &[&gone.id])
                .await?;
            summary.removed.push(label(&gone.movie));
        }

        if apply {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
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

fn encode_items(items: &[ProfileItem]) -> serde_json::Value {
    let stored: Vec<_> = items
        .iter()
        .map(|item| StoredItem {
            name: item.name.clone(),
            qualities: item.qualities.iter().map(|q| q.id()).collect(),
            allowed: item.allowed,
        })
        .collect();
    serde_json::to_value(stored).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))
}

fn decode_items(value: serde_json::Value) -> Result<Vec<ProfileItem>> {
    let stored: Vec<StoredItem> = serde_json::from_value(value)
        .map_err(|e| StoreError::Corrupt(format!("itens de perfil: {e}")))?;
    stored
        .into_iter()
        .map(|item| {
            Ok(ProfileItem {
                qualities: item
                    .qualities
                    .into_iter()
                    .map(|id| quality(i16::from(id)))
                    .collect::<Result<_>>()?,
                name: item.name,
                allowed: item.allowed,
            })
        })
        .collect()
}

fn quality(id: i16) -> Result<Quality> {
    u8::try_from(id)
        .ok()
        .and_then(Quality::from_id)
        .ok_or_else(|| StoreError::Corrupt(format!("qualidade {id}")))
}

fn small(value: u8) -> i16 {
    i16::from(value)
}

fn narrow<T: TryFrom<i32>>(value: i32, what: &str) -> Result<T> {
    T::try_from(value).map_err(|_| StoreError::Corrupt(format!("{what} {value}")))
}

async fn upsert_profile(client: &impl GenericClient, profile: &QualityProfile) -> Result<()> {
    client
        .execute(
            "INSERT INTO quality_profiles (name, upgrade_allowed, cutoff, language, items,
                 min_format_score, cutoff_format_score, source_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (name) DO UPDATE SET upgrade_allowed = excluded.upgrade_allowed,
                 cutoff = excluded.cutoff, language = excluded.language, items = excluded.items,
                 min_format_score = excluded.min_format_score,
                 cutoff_format_score = excluded.cutoff_format_score,
                 source_id = excluded.source_id",
            &[
                &profile.name,
                &profile.upgrade_allowed,
                &profile.cutoff.and_then(|c| i32::try_from(c).ok()),
                &profile.language,
                &encode_items(&profile.items),
                &profile.min_format_score,
                &profile.cutoff_format_score,
                &profile.source_id,
            ],
        )
        .await?;
    Ok(())
}

async fn write_movie(
    client: &impl GenericClient,
    id: Option<i64>,
    movie: &Movie,
    source: Option<(&str, i64)>,
) -> Result<i64> {
    let profile_id: Option<i64> = match &movie.quality_profile {
        Some(name) => client
            .query_opt("SELECT id FROM quality_profiles WHERE name = $1", &[name])
            .await?
            .map(|row| row.try_get(0))
            .transpose()?,
        None => None,
    };
    let tmdb_id = i64::from(movie.tmdb_id);
    let year = movie.year.map(i32::from);
    let runtime = i32::try_from(movie.runtime).unwrap_or(i32::MAX);
    let secondary_year = movie.secondary_year.map(i32::from);
    let source_id = source.map(|(_, id)| id);
    let source = source.map(|(name, _)| name);
    let tags =
        serde_json::to_value(&movie.tags).map_err(|e| StoreError::Corrupt(format!("tags: {e}")))?;
    let values: [&(dyn tokio_postgres::types::ToSql + Sync); 23] = [
        &tmdb_id,
        &movie.imdb_id,
        &movie.title,
        &movie.original_title,
        &movie.original_language,
        &year,
        &movie.status,
        &movie.minimum_availability,
        &movie.monitored,
        &profile_id,
        &movie.path,
        &movie.added,
        &source,
        &source_id,
        &runtime,
        &secondary_year,
        &movie.clean_title,
        &movie.available,
        &movie.in_cinemas,
        &movie.digital_release,
        &movie.physical_release,
        &movie.overview,
        &tags,
    ];
    let id: i64 = if let Some(id) = id {
        let mut params = values.to_vec();
        params.push(&id);
        client
            .execute(
                "UPDATE movies SET tmdb_id = $1, imdb_id = $2, title = $3, original_title = $4,
                     original_language = $5, year = $6, status = $7, minimum_availability = $8,
                     monitored = $9, quality_profile_id = $10, path = $11, added = $12,
                     source = $13, source_id = $14, runtime = $15, secondary_year = $16,
                     clean_title = $17, available = $18, in_cinemas = $19,
                     digital_release = $20, physical_release = $21, overview = $22, tags = $23
                 WHERE id = $24",
                &params,
            )
            .await?;
        id
    } else {
        client
            .query_one(
                "INSERT INTO movies (tmdb_id, imdb_id, title, original_title, original_language,
                     year, status, minimum_availability, monitored, quality_profile_id, path,
                     added, source, source_id, runtime, secondary_year, clean_title, available,
                     in_cinemas, digital_release, physical_release, overview, tags)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18, $19, $20, $21, $22, $23)
                 RETURNING id",
                &values,
            )
            .await?
            .try_get(0)?
    };

    write_children(client, id, movie).await?;
    Ok(id)
}

/// Títulos alternativos e arquivo do filme: apaga os de antes e grava os de
/// agora.
async fn write_children(client: &impl GenericClient, id: i64, movie: &Movie) -> Result<()> {
    client
        .execute("DELETE FROM movie_titles WHERE movie_id = $1", &[&id])
        .await?;
    for title in &movie.alternate_titles {
        client
            .execute(
                "INSERT INTO movie_titles (movie_id, title) VALUES ($1, $2)",
                &[&id, title],
            )
            .await?;
    }
    client
        .execute("DELETE FROM movie_files WHERE movie_id = $1", &[&id])
        .await?;
    if let Some(file) = &movie.file {
        let languages = serde_json::to_value(&file.languages)
            .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?;
        client
            .execute(
                "INSERT INTO movie_files (movie_id, relative_path, size, quality,
                     revision_version, revision_real, is_repack, languages, release_group,
                     edition, scene_name, date_added, file_id, media_info)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     COALESCE($13, nextval('movie_file_ids')), $14)",
                &[
                    &id,
                    &file.relative_path,
                    &i64::try_from(file.size).unwrap_or(i64::MAX),
                    &small(file.quality.quality.id()),
                    &small(file.quality.revision.version),
                    &small(file.quality.revision.real),
                    &file.quality.revision.is_repack,
                    &languages,
                    &file.release_group,
                    &file.edition,
                    &file.scene_name,
                    &file.date_added,
                    &file.id,
                    &file.media_info,
                ],
            )
            .await?;
    }
    Ok(())
}

async fn write_tags(client: &impl GenericClient, tags: &[Tag]) -> Result<()> {
    for tag in tags {
        client
            .execute(
                "INSERT INTO tags (id, label) VALUES ($1, $2)
                 ON CONFLICT (id) DO UPDATE SET label = excluded.label",
                &[&tag.id, &tag.label.to_lowercase()],
            )
            .await?;
    }
    client
        .execute(
            "SELECT setval(pg_get_serial_sequence('tags', 'id'),
                 GREATEST((SELECT COALESCE(MAX(id), 0) FROM tags), 1))",
            &[],
        )
        .await?;
    Ok(())
}

async fn write_extras(client: &impl GenericClient, id: i64, extras: &MovieExtras) -> Result<()> {
    client
        .execute(
            "UPDATE movies SET metadata_title = $2, poster = $3, fanart = $4,
                 metadata_refreshed_at = $5
             WHERE id = $1",
            &[
                &id,
                &extras.metadata_title,
                &extras.poster,
                &extras.fanart,
                &extras.refreshed_at,
            ],
        )
        .await?;
    Ok(())
}

fn read_file(row: &Row) -> Result<Option<MovieFile>> {
    let Some(relative_path) = row.try_get::<_, Option<String>>(15)? else {
        return Ok(None);
    };
    let languages: serde_json::Value = row.try_get(21)?;
    let revision = |index: usize| -> Result<u8> {
        u8::try_from(row.try_get::<_, i16>(index)?)
            .map_err(|_| StoreError::Corrupt("revisão".into()))
    };
    Ok(Some(MovieFile {
        relative_path,
        size: u64::try_from(row.try_get::<_, i64>(16)?).unwrap_or(0),
        quality: QualityModel {
            quality: quality(row.try_get(17)?)?,
            revision: Revision {
                version: revision(18)?,
                real: revision(19)?,
                is_repack: row.try_get(20)?,
            },
        },
        languages: serde_json::from_value(languages)
            .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?,
        release_group: row.try_get(22)?,
        edition: row.try_get(23)?,
        scene_name: row.try_get(24)?,
        date_added: row.try_get(25)?,
        id: row.try_get(39)?,
        media_info: row.try_get(40)?,
    }))
}

async fn read_movies(client: &impl GenericClient) -> Result<Vec<CatalogMovie>> {
    let rows = client
        .query(
            "SELECT m.id, m.tmdb_id, m.imdb_id, m.title, m.original_title, m.original_language,
                    m.year, m.status, m.minimum_availability, m.monitored, p.name, m.path,
                    m.added, m.source, m.source_id,
                    f.relative_path, f.size, f.quality, f.revision_version, f.revision_real,
                    f.is_repack, f.languages, f.release_group, f.edition, f.scene_name,
                    f.date_added,
                    m.runtime, m.secondary_year, m.clean_title, m.available,
                    m.in_cinemas, m.digital_release, m.physical_release, m.overview, m.tags,
                    m.metadata_title, m.poster, m.fanart, m.metadata_refreshed_at,
                    f.file_id, f.media_info
             FROM movies m
             LEFT JOIN quality_profiles p ON p.id = m.quality_profile_id
             LEFT JOIN movie_files f ON f.movie_id = m.id
             ORDER BY lower(m.title), m.year",
            &[],
        )
        .await?;
    let mut movies = Vec::with_capacity(rows.len());
    for row in &rows {
        let origin = match (
            row.try_get::<_, Option<String>>(13)?,
            row.try_get::<_, Option<i64>>(14)?,
        ) {
            (Some(source), Some(id)) => Some((source, id)),
            _ => None,
        };
        let tmdb_id: i64 = row.try_get(1)?;
        movies.push(CatalogMovie {
            id: row.try_get(0)?,
            origin,
            movie: Movie {
                tmdb_id: u32::try_from(tmdb_id)
                    .map_err(|_| StoreError::Corrupt(format!("tmdb {tmdb_id}")))?,
                imdb_id: row.try_get(2)?,
                title: row.try_get(3)?,
                original_title: row.try_get(4)?,
                original_language: row.try_get(5)?,
                year: row
                    .try_get::<_, Option<i32>>(6)?
                    .map(|y| narrow(y, "ano"))
                    .transpose()?,
                status: row.try_get(7)?,
                minimum_availability: row.try_get(8)?,
                monitored: row.try_get(9)?,
                quality_profile: row.try_get(10)?,
                path: row.try_get(11)?,
                added: row.try_get(12)?,
                file: read_file(row)?,
                runtime: narrow(row.try_get(26)?, "duração")?,
                secondary_year: row
                    .try_get::<_, Option<i32>>(27)?
                    .map(|y| narrow(y, "ano"))
                    .transpose()?,
                clean_title: row.try_get(28)?,
                available: row.try_get(29)?,
                alternate_titles: Vec::new(),
                in_cinemas: row.try_get(30)?,
                digital_release: row.try_get(31)?,
                physical_release: row.try_get(32)?,
                overview: row.try_get(33)?,
                tags: serde_json::from_value(row.try_get(34)?)
                    .map_err(|e| StoreError::Corrupt(format!("tags: {e}")))?,
            },
            extras: MovieExtras {
                metadata_title: row.try_get(35)?,
                poster: row.try_get(36)?,
                fanart: row.try_get(37)?,
                refreshed_at: row.try_get(38)?,
            },
        });
    }
    let titles = client
        .query("SELECT movie_id, title FROM movie_titles ORDER BY id", &[])
        .await?;
    for row in &titles {
        let (id, title): (i64, String) = (row.try_get(0)?, row.try_get(1)?);
        if let Some(entry) = movies.iter_mut().find(|m| m.id == id) {
            entry.movie.alternate_titles.push(title);
        }
    }
    Ok(movies)
}

/// Banco de teste: cada teste ganha um schema próprio, apagado no fim.
///
/// Os testes que precisam de banco leem `ACERVO_TEST_DATABASE_URL`; sem ela,
/// avisam e passam — o CI a define com um Postgres de serviço.
#[doc(hidden)]
pub mod testing {
    use super::{Store, StoreError};

    #[derive(Debug)]
    pub struct TestDb {
        pub store: Store,
        schema: String,
        url: String,
    }

    impl TestDb {
        /// `None` quando não há banco de teste configurado.
        ///
        /// # Panics
        ///
        /// Banco configurado mas inalcançável.
        pub async fn new(name: &str) -> Option<Self> {
            let Ok(url) = std::env::var("ACERVO_TEST_DATABASE_URL") else {
                eprintln!("ACERVO_TEST_DATABASE_URL ausente: teste de banco pulado");
                return None;
            };
            let schema = format!("teste_{name}_{}", std::process::id());
            let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                .await
                .expect("conectando ao banco de teste");
            tokio::spawn(connection);
            client
                .batch_execute(&format!(
                    "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"
                ))
                .await
                .expect("criando o schema de teste");
            let mut config: tokio_postgres::Config = url
                .parse()
                .map_err(|e: tokio_postgres::Error| StoreError::Url(e.to_string()))
                .expect("endereço de teste");
            config.options(format!("-c search_path={schema}"));
            let store = Store::with_config(config).await.expect("migrando");
            Some(Self { store, schema, url })
        }

        /// Apaga o schema. Chamar no fim do teste.
        ///
        /// # Panics
        ///
        /// Banco inalcançável.
        pub async fn drop(self) {
            let (client, connection) = tokio_postgres::connect(&self.url, tokio_postgres::NoTls)
                .await
                .expect("conectando ao banco de teste");
            tokio::spawn(connection);
            client
                .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
                .await
                .expect("apagando o schema de teste");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TestDb;
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
            source_id: Some(7),
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
                id: Some(i64::from(tmdb_id) + 500),
                media_info: Some(serde_json::json!({ "audioLanguages": "Portuguese/English" })),
            }),
            runtime: 110,
            secondary_year: None,
            clean_title: Some(title.to_lowercase()),
            alternate_titles: vec![format!("{title} alternativo"), format!("{title} 2")],
            available: true,
            in_cinemas: Some("2020-01-10".into()),
            digital_release: None,
            physical_release: Some("2020-04-01".into()),
            overview: Some("Sinopse".into()),
            tags: vec![1],
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
            tags: Vec::new(),
        }
    }

    #[tokio::test]
    async fn simulacao_nao_grava_e_relata_o_mesmo_que_a_aplicacao() {
        let Some(db) = TestDb::new("simulacao").await else {
            return;
        };
        let store = &db.store;
        let first = import(vec![
            (1, movie(10, "Um", true)),
            (2, movie(20, "Dois", false)),
        ]);

        let simulated = store.import(&first, false).await.unwrap();
        assert!(store.movies().await.unwrap().is_empty());
        let applied = store.import(&first, true).await.unwrap();
        assert_eq!(simulated.created, applied.created);
        assert_eq!(applied.created, ["Um (2020)", "Dois (2020)"]);

        let movies = store.movies().await.unwrap();
        assert_eq!(movies.len(), 2);
        let um = movies.iter().find(|m| m.movie.tmdb_id == 10).unwrap();
        assert_eq!(um.movie, movie(10, "Um", true));
        assert_eq!(um.origin, Some(("radarr:filmes".into(), 1)));
        assert_eq!(store.profiles().await.unwrap(), [profile()]);
        assert_eq!(store.quality_definitions().await.unwrap().len(), 1);
        db.drop().await;
    }

    #[tokio::test]
    async fn reimportar_atualiza_mantem_e_remove_so_o_que_veio_da_origem() {
        let Some(db) = TestDb::new("reimportar").await else {
            return;
        };
        let store = &db.store;
        store
            .import(
                &import(vec![
                    (1, movie(10, "Um", false)),
                    (2, movie(20, "Dois", false)),
                ]),
                true,
            )
            .await
            .unwrap();

        let again = store
            .import(&import(vec![(1, movie(10, "Um", true))]), true)
            .await
            .unwrap();
        assert_eq!(again.updated, ["Um (2020)"]);
        assert_eq!(again.removed, ["Dois (2020)"]);
        assert!(again.created.is_empty());

        let same = store
            .import(&import(vec![(1, movie(10, "Um", true))]), true)
            .await
            .unwrap();
        assert_eq!(same.unchanged, 1);
        assert!(same.updated.is_empty() && same.removed.is_empty());
        db.drop().await;
    }

    #[tokio::test]
    async fn migrar_de_novo_nao_refaz_nada() {
        let Some(db) = TestDb::new("migrar").await else {
            return;
        };
        db.store
            .import(&import(vec![(1, movie(10, "Um", true))]), true)
            .await
            .unwrap();
        db.store.migrate().await.unwrap();
        assert_eq!(db.store.movies().await.unwrap().len(), 1);
        db.drop().await;
    }

    #[tokio::test]
    async fn filme_do_acervo_sobrevive_ao_espelho_e_adotar_solta_os_da_origem() {
        let Some(db) = TestDb::new("adotar").await else {
            return;
        };
        let store = &db.store;
        store
            .import(&import(vec![(50, movie(10, "Um", false))]), true)
            .await
            .unwrap();
        let extras = MovieExtras {
            metadata_title: Some("One".into()),
            poster: Some("https://img/p.jpg".into()),
            fanart: None,
            refreshed_at: Some("2026-01-01T00:00:00Z".into()),
        };
        let own = store
            .add_movie(&movie(30, "Três", false), &extras)
            .await
            .unwrap();
        // Reimportar a origem não remove o que o acervo adicionou.
        let again = store
            .import(&import(vec![(50, movie(10, "Um", false))]), true)
            .await
            .unwrap();
        assert!(again.removed.is_empty());
        let movies = store.movies().await.unwrap();
        let tres = movies.iter().find(|m| m.id == own).unwrap();
        assert_eq!(tres.origin, None);
        assert_eq!(tres.extras, extras);
        assert_eq!(tres.movie, movie(30, "Três", false));

        let mut changed = movie(30, "Três", false);
        changed.monitored = false;
        store.update_movie(own, &changed).await.unwrap();
        assert!(
            !store
                .movies()
                .await
                .unwrap()
                .iter()
                .find(|m| m.id == own)
                .unwrap()
                .movie
                .monitored
        );

        assert_eq!(store.adopt_all().await.unwrap(), 1);
        let movies = store.movies().await.unwrap();
        assert!(movies.iter().all(|m| m.origin.is_none()));
        // O que veio da origem fica com o id de lá; o do acervo vai depois.
        let um = movies.iter().find(|m| m.movie.tmdb_id == 10).unwrap();
        let tres = movies.iter().find(|m| m.movie.tmdb_id == 30).unwrap();
        assert_eq!(um.id, 50);
        assert_eq!(tres.id, 51);
        assert_eq!(tres.extras, extras);
        assert_eq!(store.profile_ids().await.unwrap(), [(7, "Any".to_owned())]);
        assert_eq!(um.movie.quality_profile.as_deref(), Some("Any"));
        // O próximo filme novo não colide.
        let next = store
            .add_movie(&movie(40, "Quatro", false), &MovieExtras::default())
            .await
            .unwrap();
        assert_eq!(next, 52);
        assert!(store.delete_movie(tres.id).await.unwrap());
        assert!(!store.delete_movie(tres.id).await.unwrap());
        db.drop().await;
    }

    #[tokio::test]
    async fn tags_espelhadas_mantem_o_id_e_as_novas_continuam_depois() {
        let Some(db) = TestDb::new("tags").await else {
            return;
        };
        let store = &db.store;
        store
            .mirror_tags(&[Tag {
                id: 5,
                label: "Pedidos".into(),
            }])
            .await
            .unwrap();
        let new = store.create_tag("Kids").await.unwrap();
        assert!(new.id > 5);
        assert_eq!(store.create_tag("kids").await.unwrap().id, new.id);
        assert_eq!(
            store.rename_tag(5, "pedido").await.unwrap().unwrap().label,
            "pedido"
        );
        assert_eq!(
            store.tags().await.unwrap(),
            [
                Tag {
                    id: 5,
                    label: "pedido".into()
                },
                Tag {
                    id: new.id,
                    label: "kids".into()
                }
            ]
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn configuracao_grava_troca_e_apaga() {
        let Some(db) = TestDb::new("settings").await else {
            return;
        };
        let store = &db.store;
        assert_eq!(store.setting("tmdb.key").await.unwrap(), None);
        store
            .set_setting("tmdb.key", Some("um"), "t1")
            .await
            .unwrap();
        store
            .set_setting("tmdb.key", Some("dois"), "t2")
            .await
            .unwrap();
        assert_eq!(
            store.setting("tmdb.key").await.unwrap().as_deref(),
            Some("dois")
        );
        store.set_setting("tmdb.key", None, "t3").await.unwrap();
        assert_eq!(store.setting("tmdb.key").await.unwrap(), None);
        db.drop().await;
    }

    #[tokio::test]
    async fn grab_nasce_baixando_e_termina_importado() {
        let Some(db) = TestDb::new("grabs").await else {
            return;
        };
        let store = &db.store;
        store
            .import(&import(vec![(1, movie(10, "Um", false))]), true)
            .await
            .unwrap();
        let movie_id = store.movies().await.unwrap()[0].id;
        let mut grab = Grab {
            id: 0,
            movie_id,
            hash: "c12fe1c06bba254a9dc9f519b335aa7c1367a88a".into(),
            title: "Um.2020.1080p.WEB-DL-GRUPO".into(),
            indexer: "tracker".into(),
            quality: Quality::WebDl1080p,
            size: 4_000_000_000,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state: GrabState::Downloading,
            message: None,
            imported_path: None,
            finished_at: None,
        };
        grab.id = store.record_grab(&grab).await.unwrap();
        assert_eq!(store.grabs().await.unwrap(), [grab.clone()]);
        // O mesmo torrent duas vezes é recusado.
        assert!(store.record_grab(&grab).await.is_err());

        store
            .update_grab(
                grab.id,
                GrabState::Imported,
                None,
                Some("/filmes/Um (2020)/Um (2020).mkv"),
                "2026-01-01T02:00:00Z",
            )
            .await
            .unwrap();
        let done = &store.grabs().await.unwrap()[0];
        assert_eq!(done.state, GrabState::Imported);
        assert_eq!(
            done.imported_path.as_deref(),
            Some("/filmes/Um (2020)/Um (2020).mkv")
        );
        assert_eq!(done.finished_at.as_deref(), Some("2026-01-01T02:00:00Z"));
        db.drop().await;
    }

    #[tokio::test]
    async fn sombra_guarda_a_ultima_de_cada_filme() {
        let Some(db) = TestDb::new("sombra").await else {
            return;
        };
        let store = &db.store;
        store
            .import(&import(vec![(1, movie(10, "Um", false))]), true)
            .await
            .unwrap();
        let id = store.movies().await.unwrap()[0].id;
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
            .await
            .unwrap();
        store
            .record_shadow(&run("2026-01-02T00:00:00Z", true))
            .await
            .unwrap();
        assert_eq!(
            store.latest_shadow_runs().await.unwrap(),
            [run("2026-01-02T00:00:00Z", true)]
        );
        db.drop().await;
    }
}

//! Catálogo persistente: filmes, o arquivo de cada um, downloads, histórico,
//! configuração e as contas da interface, num banco Postgres. O acervo é o
//! único dono dos filmes: eles entram pela tela.

mod accounts;
mod config;
mod definitions;
mod discover;
mod manage;
mod marks;
mod series;
mod tasks;

use std::time::Duration;

use acervo_parser::{Quality, QualityModel, Revision};
use deadpool_postgres::{GenericClient, Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use serde::Serialize;
use tokio_postgres::{NoTls, Row};

pub use accounts::SESSION_DAYS;
pub use config::IndexerRecord;
pub use definitions::{DefinitionRow, IndexerDayStats, StatsDelta};
pub use discover::{DiscoverKind, HiddenGenre, HiddenTitle, HiddenWeek};
pub use manage::{Blocked, FailReason, HistoryEvent, HistoryPage, NewHistory};
pub use marks::{DeletionMark, MarkTarget};
pub use series::{
    CatalogEpisode, CatalogEpisodeFile, CatalogSeries, Episode, EpisodeFile, EpisodeSync,
    SceneMapping, Series, SeriesGrab, SeriesImport, SeriesPick, SeriesSearch, Skip, default_skip,
};
pub use tasks::{NewTaskRun, TaskRun};

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
    pub monitored: bool,
    /// Pasta do filme, como o cliente de download a vê.
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
    /// Estreia no cinema, `AAAA-MM-DD`.
    pub in_cinemas: Option<String>,
    pub digital_release: Option<String>,
    pub physical_release: Option<String>,
    pub overview: Option<String>,
}

/// O que só a base de metadados sabe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovieExtras {
    /// Título em inglês: o que dá nome à pasta e ao arquivo.
    pub metadata_title: Option<String>,
    pub poster: Option<String>,
    pub fanart: Option<String>,
    /// Quando os metadados vieram da base pela última vez (RFC 3339).
    pub refreshed_at: Option<String>,
}

/// Um filme do catálogo, com o id local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMovie {
    pub id: i64,
    pub movie: Movie,
    pub extras: MovieExtras,
    /// Passa na frente na fila e na busca. Muda só por
    /// [`Store::set_movie_priority`].
    pub priority: bool,
    /// As legendas ao lado do arquivo, com `owner` = id do filme.
    pub subtitles: Vec<CatalogSubtitle>,
}

/// Uma legenda importada ao lado do vídeo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitle {
    /// De onde veio: só a da importação é do torrent com certeza.
    pub origin: SubtitleOrigin,
    /// Relativo à pasta do filme ou da série, como o vídeo.
    pub relative_path: String,
    /// `pt-BR`, `pt`, `en`...; `None` se o nome não dizia.
    pub language: Option<String>,
    pub forced: bool,
}

/// De onde uma legenda veio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SubtitleOrigin {
    /// Hardlink feito pela importação, de um arquivo do torrent.
    #[serde(rename = "importacao")]
    Import,
    /// Achada no disco (verificar, renomear): pode ter sido posta à mão.
    #[serde(rename = "disco")]
    Disk,
}

impl SubtitleOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Import => "importacao",
            Self::Disk => "disco",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        match text {
            "importacao" => Ok(Self::Import),
            "disco" => Ok(Self::Disk),
            other => Err(StoreError::Corrupt(format!("origem de legenda `{other}`"))),
        }
    }
}

/// Uma legenda do catálogo: `owner` é o filme ou o arquivo de episódio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSubtitle {
    pub id: i64,
    pub owner: i64,
    pub subtitle: Subtitle,
}

/// O que a busca pegou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPick {
    pub title: String,
    pub indexer: String,
    pub quality: Quality,
    pub size: u64,
}

/// Uma busca por um filme: o que pegou, ou por que não.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRun {
    pub movie_id: i64,
    /// RFC 3339, em UTC.
    pub at: String,
    pub releases: usize,
    pub pick: Option<SearchPick>,
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
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Imported => "imported",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
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
    /// Onde o arquivo foi ligado, como o cliente de download vê.
    pub imported_path: Option<String>,
    pub finished_at: Option<String>,
    /// Arquivo que o filme tinha quando o grab saiu: um upgrade o troca.
    /// Relativo à pasta do filme.
    pub replaces: Option<String>,
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
    r"
    ALTER TABLE grabs ADD COLUMN replaces TEXT;
",
    r"
    CREATE TABLE history (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT REFERENCES movies(id) ON DELETE SET NULL ON UPDATE CASCADE,
        movie_title TEXT NOT NULL,
        event TEXT NOT NULL,
        at TEXT NOT NULL,
        source_title TEXT,
        quality SMALLINT,
        indexer TEXT,
        download_id TEXT,
        data JSONB NOT NULL DEFAULT '{}'
    );
    CREATE INDEX history_by_at ON history(at DESC);
    CREATE INDEX history_by_movie ON history(movie_id, at);
    CREATE TABLE blocklist (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
        source_title TEXT NOT NULL,
        indexer TEXT,
        quality SMALLINT,
        size BIGINT,
        hash TEXT,
        at TEXT NOT NULL,
        message TEXT
    );
    CREATE INDEX blocklist_by_movie ON blocklist(movie_id);
    CREATE TABLE exclusions (
        tmdb_id BIGINT PRIMARY KEY,
        title TEXT NOT NULL,
        year INTEGER
    );
    CREATE TABLE custom_formats (
        id BIGSERIAL PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        specifications JSONB NOT NULL,
        include_when_renaming BOOLEAN NOT NULL DEFAULT FALSE
    );
    ALTER TABLE quality_profiles ADD COLUMN format_scores JSONB NOT NULL DEFAULT '{}';
    CREATE TABLE import_lists (
        id BIGSERIAL PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        settings JSONB NOT NULL,
        enabled BOOLEAN NOT NULL,
        monitor BOOLEAN NOT NULL,
        search_on_add BOOLEAN NOT NULL,
        quality_profile_id BIGINT REFERENCES quality_profiles(id) ON UPDATE CASCADE,
        root_folder TEXT NOT NULL,
        minimum_availability TEXT NOT NULL,
        tags JSONB NOT NULL DEFAULT '[]',
        last_sync TEXT,
        last_error TEXT
    );
",
    // A busca dos que faltam deixou de ser simulação: a tabela vira `searches`.
    r"
    ALTER TABLE shadow_runs RENAME TO searches;
    ALTER SEQUENCE shadow_runs_id_seq RENAME TO searches_id_seq;
    ALTER INDEX shadow_runs_pkey RENAME TO searches_pkey;
    ALTER INDEX shadow_runs_by_movie RENAME TO searches_by_movie;
    ALTER TABLE searches RENAME CONSTRAINT shadow_runs_movie_id_fkey TO searches_movie_id_fkey;
",
    // Histórico das tarefas de fundo do serviço. Grava-se no fim de cada
    // execução; `detail` é o relatório dela, quando a tarefa tem um.
    r"
    CREATE TABLE task_runs (
        id BIGSERIAL PRIMARY KEY,
        task TEXT NOT NULL,
        started_at TEXT NOT NULL,
        finished_at TEXT NOT NULL,
        ok BOOLEAN NOT NULL,
        summary TEXT NOT NULL,
        detail JSONB
    );
    CREATE INDEX task_runs_by_task ON task_runs(task, id);
",
    // A configuração sai do arquivo e vem para o banco: uma linha por seção,
    // editada pela tela; os indexadores, todos como cadastro; e os strikes da
    // limpeza, que antes moravam num JSON ao lado.
    r"
    CREATE TABLE config_sections (
        name TEXT PRIMARY KEY,
        value JSONB NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE indexers (
        name TEXT PRIMARY KEY,
        kind TEXT NOT NULL CHECK (kind IN ('torznab', 'cardigann')),
        definition TEXT,
        url TEXT,
        settings JSONB NOT NULL DEFAULT '{}',
        enabled BOOLEAN NOT NULL DEFAULT TRUE,
        added_at TEXT
    );
    CREATE TABLE strikes (
        key TEXT PRIMARY KEY,
        count INTEGER NOT NULL CHECK (count > 0)
    );
",
    // A disponibilidade mínima por filme deixou de existir: todo filme vale pela
    // regra de lançado (ver `is_available` no hub).
    r"
    ALTER TABLE movies DROP COLUMN minimum_availability;
",
    // Sai a API v3 de filmes e o que só ela lia: perfis guardados, tags, a
    // origem no gerenciador, o id e as faixas do arquivo no formato dela; e
    // as tabelas que sobraram do espelho do Radarr. Os tamanhos por
    // qualidade (`quality_definitions`) ficam: o motor de decisão os usa.
    r"
    DROP TABLE import_lists;
    DROP TABLE exclusions;
    DROP TABLE custom_formats;
    ALTER TABLE movies
        DROP COLUMN quality_profile_id,
        DROP COLUMN tags,
        DROP COLUMN source,
        DROP COLUMN source_id;
    DROP TABLE quality_profiles;
    DROP TABLE tags;
    ALTER TABLE movie_files DROP COLUMN file_id, DROP COLUMN media_info;
    DROP SEQUENCE movie_file_ids;
",
    // Séries. O episódio não tem `monitored`: tem `skip`, o motivo de não
    // ser buscado (`unwanted`, `deleted`, `watched`). Sem arquivo e sem
    // `skip`, ele é procurado assim que vai ao ar. Um arquivo pode cobrir
    // vários episódios (multi-episódio); um grab, vários arquivos (pacote).
    r"
    CREATE TABLE series (
        id BIGSERIAL PRIMARY KEY,
        tmdb_id BIGINT NOT NULL UNIQUE,
        tvdb_id BIGINT,
        imdb_id TEXT,
        title TEXT NOT NULL,
        original_title TEXT,
        metadata_title TEXT,
        original_language TEXT,
        year INTEGER,
        status TEXT,
        overview TEXT,
        network TEXT,
        runtime INTEGER NOT NULL DEFAULT 0,
        poster TEXT,
        fanart TEXT,
        path TEXT NOT NULL,
        season_folder BOOLEAN NOT NULL,
        monitor_new BOOLEAN NOT NULL,
        added TEXT,
        refreshed_at TEXT
    );
    CREATE TABLE series_titles (
        id BIGSERIAL PRIMARY KEY,
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        title TEXT NOT NULL
    );
    CREATE INDEX series_titles_by_series ON series_titles(series_id);
    CREATE TABLE episode_files (
        id BIGSERIAL PRIMARY KEY,
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        relative_path TEXT NOT NULL,
        size BIGINT NOT NULL,
        quality SMALLINT NOT NULL,
        revision_version SMALLINT NOT NULL,
        revision_real SMALLINT NOT NULL,
        is_repack BOOLEAN NOT NULL,
        languages JSONB NOT NULL,
        release_group TEXT,
        scene_name TEXT,
        date_added TEXT,
        UNIQUE (series_id, relative_path)
    );
    CREATE TABLE episodes (
        id BIGSERIAL PRIMARY KEY,
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        season INTEGER NOT NULL,
        number INTEGER NOT NULL,
        tmdb_id BIGINT,
        title TEXT,
        air_date TEXT,
        overview TEXT,
        runtime INTEGER NOT NULL DEFAULT 0,
        skip TEXT CHECK (skip IN ('unwanted', 'deleted', 'watched')),
        skipped_at TEXT,
        file_id BIGINT REFERENCES episode_files(id) ON DELETE SET NULL,
        UNIQUE (series_id, season, number)
    );
    CREATE INDEX episodes_by_file ON episodes(file_id);
    CREATE TABLE series_grabs (
        id BIGSERIAL PRIMARY KEY,
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        hash TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL,
        indexer TEXT NOT NULL,
        quality SMALLINT NOT NULL,
        size BIGINT NOT NULL,
        grabbed_at TEXT NOT NULL,
        state TEXT NOT NULL,
        message TEXT,
        finished_at TEXT
    );
    CREATE INDEX series_grabs_by_series ON series_grabs(series_id, grabbed_at);
    CREATE TABLE series_grab_episodes (
        grab_id BIGINT NOT NULL REFERENCES series_grabs(id) ON DELETE CASCADE,
        episode_id BIGINT NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
        PRIMARY KEY (grab_id, episode_id)
    );
    CREATE INDEX series_grab_episodes_by_episode ON series_grab_episodes(episode_id);
    ALTER TABLE history
        ADD COLUMN series_id BIGINT REFERENCES series(id) ON DELETE SET NULL,
        ADD COLUMN episode_ids JSONB NOT NULL DEFAULT '[]';
    CREATE INDEX history_by_series ON history(series_id, at);
    ALTER TABLE blocklist
        ADD COLUMN series_id BIGINT REFERENCES series(id) ON DELETE CASCADE;
    CREATE INDEX blocklist_by_series ON blocklist(series_id);
",
    // A busca de cada série: o que se consultou, o que se pegou (uma busca
    // de série pode pegar vários releases) e por que o resto ficou.
    r"
    CREATE TABLE series_searches (
        id BIGSERIAL PRIMARY KEY,
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        at TEXT NOT NULL,
        queries JSONB NOT NULL,
        releases BIGINT NOT NULL,
        picks JSONB NOT NULL,
        rejections JSONB NOT NULL,
        error TEXT
    );
    CREATE INDEX series_searches_by_series ON series_searches(series_id, at);
",
    // Prioridade: o filme ou a série que passa na frente na fila do acervo,
    // na do cliente e na busca dos que faltam.
    r"
    ALTER TABLE movies ADD COLUMN priority BOOLEAN NOT NULL DEFAULT FALSE;
    ALTER TABLE series ADD COLUMN priority BOOLEAN NOT NULL DEFAULT FALSE;
",
    // Numeração de cena (XEM): o par (temporada, episódio) com que o release
    // sai, e o do catálogo. Só os pares que diferem; sem linha, é o mesmo.
    r"
    CREATE TABLE scene_mappings (
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        scene_season INTEGER NOT NULL,
        scene_episode INTEGER NOT NULL,
        season INTEGER NOT NULL,
        episode INTEGER NOT NULL,
        PRIMARY KEY (series_id, scene_season, scene_episode)
    );
",
    // Legendas importadas ao lado do vídeo: de um filme ou de um arquivo de
    // episódio, nunca dos dois. Saem com o dono; o disco é com quem chama.
    r"
    CREATE TABLE subtitle_files (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
        episode_file_id BIGINT REFERENCES episode_files(id) ON DELETE CASCADE,
        relative_path TEXT NOT NULL,
        language TEXT,
        forced BOOLEAN NOT NULL DEFAULT FALSE,
        CHECK ((movie_id IS NULL) <> (episode_file_id IS NULL)),
        UNIQUE (movie_id, relative_path),
        UNIQUE (episode_file_id, relative_path)
    );
    CREATE INDEX subtitle_files_by_episode_file ON subtitle_files(episode_file_id);
",
    // De onde a legenda veio: da importação (hardlink do torrent) ou do
    // disco (posta à mão, achada pelo verificar ou pelo renomear). Só a da
    // importação o upgrade apaga sem olhar os links.
    r"
    ALTER TABLE subtitle_files
        ADD COLUMN origin TEXT NOT NULL DEFAULT 'importacao'
            CHECK (origin IN ('importacao', 'disco'));
",
    // Numeração de cena 1:N: um par de cena pode ser mais de um episódio do
    // catálogo (o duplo), então a chave leva o alvo. O mapa se refaz na
    // próxima volta da tarefa `cena`.
    r"
    DROP TABLE scene_mappings;
    CREATE TABLE scene_mappings (
        series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        scene_season INTEGER NOT NULL,
        scene_episode INTEGER NOT NULL,
        season INTEGER NOT NULL,
        episode INTEGER NOT NULL,
        PRIMARY KEY (series_id, scene_season, scene_episode, season, episode)
    );
",
    // As definições Cardigann baixadas do repositório oficial: o serviço não
    // tem diretório gravável. `sha` é o SHA-256 do YAML.
    //
    // E a estatística de cada indexador por dia (UTC): consultas que saíram,
    // quantas falharam, quantas foram 429, os grabs e o tempo somado.
    r"
    CREATE TABLE definitions (
        id TEXT PRIMARY KEY,
        yaml TEXT NOT NULL,
        sha TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE indexer_stats (
        indexer TEXT NOT NULL,
        day DATE NOT NULL,
        queries INTEGER NOT NULL DEFAULT 0,
        failures INTEGER NOT NULL DEFAULT 0,
        rate_limited INTEGER NOT NULL DEFAULT 0,
        grabs INTEGER NOT NULL DEFAULT 0,
        total_ms BIGINT NOT NULL DEFAULT 0,
        PRIMARY KEY (indexer, day)
    );
",
    // Saem os gerenciadores *arr, a sincronização com eles e o passo da
    // limpeza que cruzava a fila deles: a seção e os campos que só eles
    // usavam. A configuração recusa campo e seção desconhecidos, e o serviço
    // não subiria com eles no banco.
    //
    // Saem também os tamanhos por qualidade editáveis, que viraram tabela
    // fixa no código, e a disponibilidade guardada do filme, que se calcula
    // das datas.
    r"
    DELETE FROM config_sections WHERE name = 'gerenciadores';
    UPDATE config_sections SET value = value - 'public_url' WHERE name = 'servidor';
    UPDATE config_sections SET value = value - 'skip_orphan_if_missing_in_client'
        WHERE name = 'limpeza';
    DROP TABLE quality_definitions;
    ALTER TABLE movies DROP COLUMN available;
",
    // O motivo do bloqueio vira coluna: a expiração e a poda olhavam o texto
    // da mensagem, que é só para a tela. As linhas que já existem ganham o
    // motivo pelo texto com que foram gravadas; o resto é `other`, que não
    // expira.
    r"
    ALTER TABLE blocklist ADD COLUMN reason TEXT NOT NULL DEFAULT 'other'
        CHECK (reason IN ('no_seeds', 'unregistered', 'client_error', 'missing_files',
            'vanished', 'other'));
    UPDATE blocklist SET reason = CASE message
        WHEN 'sem seeds há 30 min' THEN 'no_seeds'
        WHEN 'o tracker não reconhece mais o torrent' THEN 'unregistered'
        WHEN 'o cliente marcou o torrent com `error`' THEN 'client_error'
        WHEN 'o cliente não acha os arquivos do torrent' THEN 'missing_files'
        WHEN 'o torrent sumiu do cliente' THEN 'vanished'
        ELSE 'other'
    END;
    CREATE INDEX blocklist_by_reason ON blocklist(reason, at);
",
    // Assistido deixa de sair sozinho: o usuário marca o que quer apagar (um
    // filme, uma temporada ou a série inteira, `season` nulo) e confirma na
    // tela "Para apagar". A marca sai com o filme ou a série. Sai também a
    // tarefa `assistidos`: o intervalo gravado dela (a configuração recusa
    // tarefa desconhecida e o serviço não subiria) e o histórico, cujas
    // remoções seguem no histórico de atividade.
    r"
    CREATE TABLE deletion_marks (
        id BIGSERIAL PRIMARY KEY,
        movie_id BIGINT REFERENCES movies(id) ON DELETE CASCADE,
        series_id BIGINT REFERENCES series(id) ON DELETE CASCADE,
        season INTEGER CHECK (season >= 0),
        marked_at TEXT NOT NULL,
        CHECK ((movie_id IS NULL) <> (series_id IS NULL)),
        CHECK (season IS NULL OR series_id IS NOT NULL)
    );
    CREATE UNIQUE INDEX deletion_marks_by_movie ON deletion_marks(movie_id)
        WHERE movie_id IS NOT NULL;
    CREATE UNIQUE INDEX deletion_marks_by_series ON deletion_marks(series_id, COALESCE(season, -1))
        WHERE series_id IS NOT NULL;
    UPDATE config_sections
        SET value = jsonb_set(value, '{intervalos}', (value -> 'intervalos') - 'assistidos')
        WHERE name = 'tarefas' AND jsonb_typeof(value -> 'intervalos') = 'object';
    DELETE FROM task_runs WHERE task = 'assistidos';
",
    // Preferências do Descobrir independem da presença da obra no catálogo.
    r"
    CREATE TABLE discover_hidden_titles (
        kind TEXT NOT NULL CHECK (kind IN ('filme','serie')),
        tmdb_id INTEGER NOT NULL,
        title TEXT NOT NULL DEFAULT '',
        hidden_at TEXT NOT NULL,
        PRIMARY KEY (kind, tmdb_id)
    );
    CREATE TABLE discover_hidden_weeks (
        year INTEGER NOT NULL,
        week INTEGER NOT NULL CHECK (week BETWEEN 1 AND 53),
        hidden_at TEXT NOT NULL,
        PRIMARY KEY (year, week)
    );
    CREATE TABLE discover_hidden_genres (
        genre_id INTEGER PRIMARY KEY,
        name TEXT NOT NULL DEFAULT '',
        hidden_at TEXT NOT NULL
    );
",
];

/// Quanto uma consulta pode levar. A maior consulta real (o catálogo de
/// séries inteiro, com episódios e arquivos) leva menos de um segundo; o
/// teto existe para uma conexão presa não segurar uma tarefa para sempre.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Quanto se espera para abrir uma conexão com o banco.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Quanto se espera por uma conexão do pool, e para criar ou reciclar uma.
const POOL_WAIT: Duration = Duration::from_secs(30);

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
    pub async fn with_config(mut config: tokio_postgres::Config) -> Result<Self> {
        // As opções que vieram (o `search_path` dos testes) ficam; o teto de
        // cada consulta vai junto.
        let options = match config.get_options() {
            Some(options) if !options.trim().is_empty() => format!(
                "{options} -c statement_timeout={}",
                STATEMENT_TIMEOUT.as_millis()
            ),
            _ => format!("-c statement_timeout={}", STATEMENT_TIMEOUT.as_millis()),
        };
        config.options(options);
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(CONNECT_TIMEOUT);
        }
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(8)
            .runtime(Runtime::Tokio1)
            .wait_timeout(Some(POOL_WAIT))
            .create_timeout(Some(POOL_WAIT))
            .recycle_timeout(Some(POOL_WAIT))
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

    /// Grava uma busca.
    ///
    /// # Errors
    ///
    /// Falha de escrita, ou filme que não está no catálogo.
    pub async fn record_search(&self, run: &SearchRun) -> Result<()> {
        let client = self.pool.get().await?;
        let rejections = serde_json::to_value(&run.rejections)
            .map_err(|e| StoreError::Corrupt(format!("rejeições da busca: {e}")))?;
        let pick = run.pick.as_ref();
        client
            .execute(
                "INSERT INTO searches (movie_id, at, releases, pick_title, pick_indexer,
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

    /// A busca mais recente de cada filme.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou registro inconsistente.
    pub async fn latest_searches(&self) -> Result<Vec<SearchRun>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT DISTINCT ON (movie_id) movie_id, at, releases, pick_title, pick_indexer,
                        pick_quality, pick_size, rejections, error
                 FROM searches
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
                    (Some(title), Some(id)) => Some(SearchPick {
                        title,
                        indexer: row.try_get::<_, Option<String>>(4)?.unwrap_or_default(),
                        quality: quality(id)?,
                        size: u64::try_from(row.try_get::<_, Option<i64>>(6)?.unwrap_or(0))
                            .unwrap_or(0),
                    }),
                    _ => None,
                };
                let rejections: serde_json::Value = row.try_get(7)?;
                Ok(SearchRun {
                    movie_id: row.try_get(0)?,
                    at: row.try_get(1)?,
                    releases: usize::try_from(row.try_get::<_, i64>(2)?).unwrap_or(0),
                    pick,
                    rejections: serde_json::from_value(rejections)
                        .map_err(|e| StoreError::Corrupt(format!("rejeições da busca: {e}")))?,
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
        let id = insert_movie(&tx, movie).await?;
        write_extras(&tx, id, extras).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Regrava só o que vem da base de metadados — títulos, datas, status,
    /// duração, sinopse —, nunca o que é de quem usa (monitorado, pasta,
    /// prioridade) nem o arquivo: a atualização roda junto com a tela e a
    /// importação, e uma leitura velha não pode desfazer o que elas
    /// gravaram. `false` se o filme saiu do catálogo no meio.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn update_movie_metadata(&self, id: i64, movie: &Movie) -> Result<bool> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let year = movie.year.map(i32::from);
        let runtime = i32::try_from(movie.runtime).unwrap_or(i32::MAX);
        let secondary_year = movie.secondary_year.map(i32::from);
        let updated = tx
            .execute(
                "UPDATE movies SET imdb_id = $2, title = $3, original_title = $4,
                     original_language = $5, year = $6, status = $7, runtime = $8,
                     secondary_year = $9, clean_title = $10, in_cinemas = $11,
                     digital_release = $12, physical_release = $13, overview = $14
                 WHERE id = $1",
                &[
                    &id,
                    &movie.imdb_id,
                    &movie.title,
                    &movie.original_title,
                    &movie.original_language,
                    &year,
                    &movie.status,
                    &runtime,
                    &secondary_year,
                    &movie.clean_title,
                    &movie.in_cinemas,
                    &movie.digital_release,
                    &movie.physical_release,
                    &movie.overview,
                ],
            )
            .await?;
        if updated == 0 {
            return Ok(false);
        }
        write_titles(&tx, id, movie).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Liga ou desliga o monitorado do filme. `false` se ele não existe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_movie_monitored(&self, id: i64, monitored: bool) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE movies SET monitored = $2 WHERE id = $1",
                &[&id, &monitored],
            )
            .await?
            > 0)
    }

    /// Troca o arquivo do filme no catálogo (`None` tira). O disco não é
    /// tocado aqui.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_movie_file(&self, movie_id: i64, file: Option<&MovieFile>) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute("DELETE FROM movie_files WHERE movie_id = $1", &[&movie_id])
            .await?;
        tx.execute(
            "DELETE FROM subtitle_files WHERE movie_id = $1",
            &[&movie_id],
        )
        .await?;
        if let Some(file) = file {
            insert_file(&tx, movie_id, file).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Liga ou desliga a prioridade do filme. `false` se ele não existe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_movie_priority(&self, id: i64, priority: bool) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE movies SET priority = $2 WHERE id = $1",
                &[&id, &priority],
            )
            .await?
            > 0)
    }

    /// Troca só o caminho do arquivo do filme (renomeado no disco). `false`
    /// se o filme não tem arquivo.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_movie_file_path(&self, movie_id: i64, relative_path: &str) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE movie_files SET relative_path = $2 WHERE movie_id = $1",
                &[&movie_id, &relative_path],
            )
            .await?
            > 0)
    }

    /// Os hashes dos grabs em andamento (de filme ou de série) cuja obra é
    /// prioritária.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn priority_hashes(&self) -> Result<std::collections::HashSet<String>> {
        let client = self.pool.get().await?;
        client
            .query(
                "SELECT g.hash FROM grabs g JOIN movies m ON m.id = g.movie_id
                 WHERE g.state = 'downloading' AND m.priority
                 UNION
                 SELECT g.hash FROM series_grabs g JOIN series s ON s.id = g.series_id
                 WHERE g.state = 'downloading' AND s.priority",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok(row.try_get(0)?))
            .collect()
    }

    /// Registra uma legenda ao lado do vídeo do filme; o mesmo caminho é
    /// regravado no lugar. Devolve o id.
    ///
    /// # Errors
    ///
    /// Filme inexistente ou falha de escrita.
    pub async fn add_movie_subtitle(&self, movie_id: i64, subtitle: &Subtitle) -> Result<i64> {
        insert_movie_subtitle(&self.pool.get().await?, movie_id, subtitle).await
    }

    /// Registra uma legenda de um arquivo de episódio. Devolve o id.
    ///
    /// # Errors
    ///
    /// Arquivo inexistente ou falha de escrita.
    pub async fn add_episode_subtitle(&self, file_id: i64, subtitle: &Subtitle) -> Result<i64> {
        insert_episode_subtitle(&self.pool.get().await?, file_id, subtitle).await
    }

    /// Troca o caminho de uma legenda (renomeada no disco).
    ///
    /// # Errors
    ///
    /// Falha de escrita, ou caminho já usado pelo mesmo dono.
    pub async fn set_subtitle_path(&self, id: i64, relative_path: &str) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute(
                "UPDATE subtitle_files SET relative_path = $2 WHERE id = $1",
                &[&id, &relative_path],
            )
            .await?
            > 0)
    }

    /// Grava o que só a base de metadados sabe. Filme que saiu do catálogo
    /// não é erro: não há o que gravar.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn set_extras(&self, id: i64, extras: &MovieExtras) -> Result<()> {
        write_extras(&self.pool.get().await?, id, extras).await
    }

    /// Tira um filme do catálogo, com o registro do arquivo, títulos, buscas
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
            .query_opt(
                "INSERT INTO grabs (movie_id, hash, title, indexer, quality, size, grabbed_at,
                     state, message, replaces)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (hash) DO UPDATE SET movie_id = EXCLUDED.movie_id,
                     title = EXCLUDED.title, indexer = EXCLUDED.indexer,
                     quality = EXCLUDED.quality, size = EXCLUDED.size,
                     grabbed_at = EXCLUDED.grabbed_at, state = EXCLUDED.state,
                     message = EXCLUDED.message, replaces = EXCLUDED.replaces,
                     imported_path = NULL, finished_at = NULL
                 WHERE grabs.state <> 'downloading'
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
                    &grab.replaces,
                ],
            )
            .await?
            .ok_or_else(|| {
                StoreError::Corrupt(format!("o release {} já está baixando", grab.hash))
            })?;
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
                        message, imported_path, finished_at, replaces
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
                    replaces: row.try_get(12)?,
                })
            })
            .collect()
    }

    /// Encerra um grab: importado (com o caminho) ou desistido (com o
    /// motivo). `message` sem mudar o estado serve de anotação ("ainda
    /// baixando: 40%").
    ///
    /// Só mexe em grab ainda `downloading`: dois fluxos (a importação, a
    /// tela, a troca na fila) podem decidir sobre o mesmo grab ao mesmo
    /// tempo, e o segundo não pode desfazer o que o primeiro fechou — nem
    /// ressuscitar um falho com uma anotação. Devolve se a linha mudou;
    /// `false` é "outro fluxo já fechou este grab".
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
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        set_grab(&client, id, state, message, imported_path, at).await
    }

    /// Desiste de um grab e, com `block`, bloqueia o release, numa
    /// transação: nunca o grab falho sem o bloqueio pedido, nem o bloqueio
    /// de um grab que outro fluxo já fechou. `false` (e nada gravado) se o
    /// grab já não estava `downloading`.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn fail_grab(
        &self,
        id: i64,
        message: &str,
        block: Option<&Blocked>,
        at: &str,
    ) -> Result<bool> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        if !set_grab(&tx, id, GrabState::Failed, Some(message), None, at).await? {
            return Ok(false);
        }
        if let Some(block) = block {
            manage::insert_block(&tx, block).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// A importação de um filme, numa transação: o arquivo novo no lugar do
    /// antigo, as legendas dele e o grab importado (em `imported_path`). Se
    /// algo cai no meio, nada fica: a volta seguinte refaz tudo, em vez de
    /// achar o arquivo novo no catálogo e descartar o grab como se outro
    /// caminho o tivesse importado. `false` (e nada gravado) se o grab já
    /// não estava `downloading`.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn import_movie(
        &self,
        movie_id: i64,
        grab_id: i64,
        file: &MovieFile,
        subtitles: &[Subtitle],
        imported_path: &str,
        at: &str,
    ) -> Result<bool> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        if !set_grab(
            &tx,
            grab_id,
            GrabState::Imported,
            None,
            Some(imported_path),
            at,
        )
        .await?
        {
            return Ok(false);
        }
        tx.execute("DELETE FROM movie_files WHERE movie_id = $1", &[&movie_id])
            .await?;
        tx.execute(
            "DELETE FROM subtitle_files WHERE movie_id = $1",
            &[&movie_id],
        )
        .await?;
        insert_file(&tx, movie_id, file).await?;
        for subtitle in subtitles {
            insert_movie_subtitle(&tx, movie_id, subtitle).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// O corpo de [`Store::update_grab`], dentro da transação de quem chama, se
/// houver.
async fn set_grab(
    client: &impl GenericClient,
    id: i64,
    state: GrabState,
    message: Option<&str>,
    imported_path: Option<&str>,
    at: &str,
) -> Result<bool> {
    let finished = (state != GrabState::Downloading).then_some(at);
    Ok(client
        .execute(
            "UPDATE grabs SET state = $2, message = $3,
                 imported_path = COALESCE($4, imported_path),
                 finished_at = COALESCE($5, finished_at)
             WHERE id = $1 AND state = 'downloading'",
            &[&id, &state.as_str(), &message, &imported_path, &finished],
        )
        .await?
        > 0)
}

/// O corpo de [`Store::add_movie_subtitle`], dentro da transação de quem
/// chama, se houver.
async fn insert_movie_subtitle(
    client: &impl GenericClient,
    movie_id: i64,
    subtitle: &Subtitle,
) -> Result<i64> {
    Ok(client
        .query_one(
            "INSERT INTO subtitle_files (movie_id, relative_path, language, forced, origin)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (movie_id, relative_path) DO UPDATE SET
                 language = EXCLUDED.language, forced = EXCLUDED.forced,
                 origin = EXCLUDED.origin
             RETURNING id",
            &[
                &movie_id,
                &subtitle.relative_path,
                &subtitle.language,
                &subtitle.forced,
                &subtitle.origin.as_str(),
            ],
        )
        .await?
        .try_get(0)?)
}

/// Uma legenda de arquivo de episódio, dentro da transação de quem chama,
/// se houver. O corpo de [`Store::add_episode_subtitle`].
pub(crate) async fn insert_episode_subtitle(
    client: &impl GenericClient,
    file_id: i64,
    subtitle: &Subtitle,
) -> Result<i64> {
    Ok(client
        .query_one(
            "INSERT INTO subtitle_files (episode_file_id, relative_path, language, forced,
                 origin)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (episode_file_id, relative_path) DO UPDATE SET
                 language = EXCLUDED.language, forced = EXCLUDED.forced,
                 origin = EXCLUDED.origin
             RETURNING id",
            &[
                &file_id,
                &subtitle.relative_path,
                &subtitle.language,
                &subtitle.forced,
                &subtitle.origin.as_str(),
            ],
        )
        .await?
        .try_get(0)?)
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

async fn insert_movie(client: &impl GenericClient, movie: &Movie) -> Result<i64> {
    let tmdb_id = i64::from(movie.tmdb_id);
    let year = movie.year.map(i32::from);
    let runtime = i32::try_from(movie.runtime).unwrap_or(i32::MAX);
    let secondary_year = movie.secondary_year.map(i32::from);
    let id: i64 = client
        .query_one(
            "INSERT INTO movies (tmdb_id, imdb_id, title, original_title, original_language,
                 year, status, monitored, path, added, runtime, secondary_year,
                 clean_title, in_cinemas, digital_release, physical_release, overview)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17)
             RETURNING id",
            &[
                &tmdb_id,
                &movie.imdb_id,
                &movie.title,
                &movie.original_title,
                &movie.original_language,
                &year,
                &movie.status,
                &movie.monitored,
                &movie.path,
                &movie.added,
                &runtime,
                &secondary_year,
                &movie.clean_title,
                &movie.in_cinemas,
                &movie.digital_release,
                &movie.physical_release,
                &movie.overview,
            ],
        )
        .await?
        .try_get(0)?;
    write_titles(client, id, movie).await?;
    if let Some(file) = &movie.file {
        insert_file(client, id, file).await?;
    }
    Ok(id)
}

/// Títulos alternativos do filme: apaga os de antes e grava os de agora.
async fn write_titles(client: &impl GenericClient, id: i64, movie: &Movie) -> Result<()> {
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
    Ok(())
}

async fn insert_file(client: &impl GenericClient, movie_id: i64, file: &MovieFile) -> Result<()> {
    let languages = serde_json::to_value(&file.languages)
        .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?;
    client
        .execute(
            "INSERT INTO movie_files (movie_id, relative_path, size, quality,
                 revision_version, revision_real, is_repack, languages, release_group,
                 edition, scene_name, date_added)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            &[
                &movie_id,
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
            ],
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

/// O arquivo da linha de `read_movies`, cujas colunas vêm com prefixo `f_`.
fn read_file(row: &Row) -> Result<Option<MovieFile>> {
    let Some(relative_path) = row.try_get::<_, Option<String>>("f_relative_path")? else {
        return Ok(None);
    };
    let languages: serde_json::Value = row.try_get("f_languages")?;
    let revision = |column: &str| -> Result<u8> {
        u8::try_from(row.try_get::<_, i16>(column)?)
            .map_err(|_| StoreError::Corrupt("revisão".into()))
    };
    Ok(Some(MovieFile {
        relative_path,
        size: u64::try_from(row.try_get::<_, i64>("f_size")?).unwrap_or(0),
        quality: QualityModel {
            quality: quality(row.try_get("f_quality")?)?,
            revision: Revision {
                version: revision("f_revision_version")?,
                real: revision("f_revision_real")?,
                is_repack: row.try_get("f_is_repack")?,
            },
        },
        languages: serde_json::from_value(languages)
            .map_err(|e| StoreError::Corrupt(format!("idiomas: {e}")))?,
        release_group: row.try_get("f_release_group")?,
        edition: row.try_get("f_edition")?,
        scene_name: row.try_get("f_scene_name")?,
        date_added: row.try_get("f_date_added")?,
    }))
}

async fn read_movies(client: &impl GenericClient) -> Result<Vec<CatalogMovie>> {
    // Colunas lidas pelo nome: o arquivo vem com prefixo `f_`, para não
    // colidir com as do filme.
    let rows = client
        .query(
            "SELECT m.id, m.tmdb_id, m.imdb_id, m.title, m.original_title, m.original_language,
                    m.year, m.status, m.monitored, m.path, m.added,
                    m.runtime, m.secondary_year, m.clean_title,
                    m.in_cinemas, m.digital_release, m.physical_release, m.overview,
                    m.metadata_title, m.poster, m.fanart, m.metadata_refreshed_at, m.priority,
                    f.relative_path AS f_relative_path, f.size AS f_size,
                    f.quality AS f_quality, f.revision_version AS f_revision_version,
                    f.revision_real AS f_revision_real, f.is_repack AS f_is_repack,
                    f.languages AS f_languages, f.release_group AS f_release_group,
                    f.edition AS f_edition, f.scene_name AS f_scene_name,
                    f.date_added AS f_date_added
             FROM movies m
             LEFT JOIN movie_files f ON f.movie_id = m.id
             ORDER BY lower(m.title), m.year",
            &[],
        )
        .await?;
    let mut movies = Vec::with_capacity(rows.len());
    for row in &rows {
        let tmdb_id: i64 = row.try_get("tmdb_id")?;
        movies.push(CatalogMovie {
            id: row.try_get("id")?,
            movie: Movie {
                tmdb_id: u32::try_from(tmdb_id)
                    .map_err(|_| StoreError::Corrupt(format!("tmdb {tmdb_id}")))?,
                imdb_id: row.try_get("imdb_id")?,
                title: row.try_get("title")?,
                original_title: row.try_get("original_title")?,
                original_language: row.try_get("original_language")?,
                year: row
                    .try_get::<_, Option<i32>>("year")?
                    .map(|y| narrow(y, "ano"))
                    .transpose()?,
                status: row.try_get("status")?,
                monitored: row.try_get("monitored")?,
                path: row.try_get("path")?,
                added: row.try_get("added")?,
                file: read_file(row)?,
                runtime: narrow(row.try_get("runtime")?, "duração")?,
                secondary_year: row
                    .try_get::<_, Option<i32>>("secondary_year")?
                    .map(|y| narrow(y, "ano"))
                    .transpose()?,
                clean_title: row.try_get("clean_title")?,
                alternate_titles: Vec::new(),
                in_cinemas: row.try_get("in_cinemas")?,
                digital_release: row.try_get("digital_release")?,
                physical_release: row.try_get("physical_release")?,
                overview: row.try_get("overview")?,
            },
            extras: MovieExtras {
                metadata_title: row.try_get("metadata_title")?,
                poster: row.try_get("poster")?,
                fanart: row.try_get("fanart")?,
                refreshed_at: row.try_get("metadata_refreshed_at")?,
            },
            priority: row.try_get("priority")?,
            subtitles: Vec::new(),
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
    let subtitles = client
        .query(
            &format!(
                "SELECT {SUBTITLE_COLUMNS}, movie_id FROM subtitle_files
                 WHERE movie_id IS NOT NULL ORDER BY relative_path"
            ),
            &[],
        )
        .await?;
    for row in &subtitles {
        if let Some(entry) = movies.iter_mut().find(|m| m.id == row.get::<_, i64>(5)) {
            entry.subtitles.push(subtitle_from(row, 5)?);
        }
    }
    Ok(movies)
}

/// Colunas de `subtitle_files`, na ordem que [`subtitle_from`] lê; o dono
/// vem logo depois, na coluna `owner`.
pub(crate) const SUBTITLE_COLUMNS: &str = "id, relative_path, language, forced, origin";

pub(crate) fn subtitle_from(row: &Row, owner: usize) -> Result<CatalogSubtitle> {
    Ok(CatalogSubtitle {
        id: row.try_get(0)?,
        owner: row.try_get(owner)?,
        subtitle: Subtitle {
            relative_path: row.try_get(1)?,
            language: row.try_get(2)?,
            forced: row.try_get(3)?,
            origin: SubtitleOrigin::parse(row.try_get(4)?)?,
        },
    })
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
        /// Um banco parado logo antes da primeira migração cujo texto contém
        /// `marker`, com `setup` (SQL) rodado nele; depois, migrado até o fim
        /// como o serviço faria ao subir. `None` quando não há banco de teste
        /// configurado.
        ///
        /// # Panics
        ///
        /// Banco configurado mas inalcançável, `marker` sem migração ou
        /// `setup` que falha.
        pub async fn before(name: &str, marker: &str, setup: &str) -> Option<Self> {
            let Ok(url) = std::env::var("ACERVO_TEST_DATABASE_URL") else {
                eprintln!("ACERVO_TEST_DATABASE_URL ausente: teste de banco pulado");
                return None;
            };
            let schema = format!("teste_{name}_{}", std::process::id());
            let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                .await
                .expect("conectando ao banco de teste");
            tokio::spawn(connection);
            let before = super::MIGRATIONS
                .iter()
                .position(|m| m.contains(marker))
                .expect("migração do marcador");
            let mut sql = format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema};
                 SET search_path = {schema};"
            );
            for migration in &super::MIGRATIONS[..before] {
                sql.push_str(migration);
            }
            sql.push_str("CREATE TABLE schema_version (version INTEGER NOT NULL);");
            sql.push_str("INSERT INTO schema_version VALUES (");
            sql.push_str(&before.to_string());
            sql.push_str(");");
            sql.push_str(setup);
            client
                .batch_execute(&sql)
                .await
                .expect("montando o banco antigo");
            let mut config: tokio_postgres::Config = url.parse().expect("endereço de teste");
            config.options(format!("-c search_path={schema}"));
            let store = Store::with_config(config).await.expect("migrando");
            Some(Self { store, schema, url })
        }

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
    use std::fmt::Write as _;

    use super::testing::TestDb;
    use super::*;

    fn movie(tmdb_id: u32, title: &str, with_file: bool) -> Movie {
        Movie {
            tmdb_id,
            imdb_id: Some(format!("tt{tmdb_id:07}")),
            title: title.into(),
            original_title: None,
            original_language: Some("English".into()),
            year: Some(2020),
            status: Some("released".into()),
            monitored: true,
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
            alternate_titles: vec![format!("{title} alternativo"), format!("{title} 2")],
            in_cinemas: Some("2020-01-10".into()),
            digital_release: None,
            physical_release: Some("2020-04-01".into()),
            overview: Some("Sinopse".into()),
        }
    }

    /// Um filme adicionado como a tela faz.
    async fn added(store: &Store, movie: Movie) -> i64 {
        store
            .add_movie(&movie, &MovieExtras::default())
            .await
            .unwrap()
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
        let movie_id = added(store, movie(10, "Um", false)).await;
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
            replaces: Some("Um (2020).mkv".into()),
        };
        grab.id = store.record_grab(&grab).await.unwrap();
        assert_eq!(store.grabs().await.unwrap(), [grab.clone()]);
        // O mesmo torrent duas vezes é recusado.
        assert!(store.record_grab(&grab).await.is_err());
        // Já o que falhou dá lugar ao mesmo release pego de novo.
        store
            .update_grab(
                grab.id,
                GrabState::Failed,
                Some("erro"),
                None,
                "2026-01-01T01:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(store.record_grab(&grab).await.unwrap(), grab.id);
        assert_eq!(store.grabs().await.unwrap(), [grab.clone()]);

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
        // Importado também dá lugar: o filme perdeu o arquivo e o mesmo
        // release voltou a ser o escolhido.
        assert_eq!(store.record_grab(&grab).await.unwrap(), grab.id);
        assert_eq!(
            store.grabs().await.unwrap()[0].state,
            GrabState::Downloading
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn grab_fechado_por_outro_fluxo_nao_e_regravado() {
        let Some(db) = TestDb::new("grab_cas").await else {
            return;
        };
        let store = &db.store;
        let movie_id = added(store, movie(10, "Um", false)).await;
        let grab = Grab {
            id: 0,
            movie_id,
            hash: "aa".into(),
            title: "Um.2020.1080p.WEB-DL-GRUPO".into(),
            indexer: "tracker".into(),
            quality: Quality::WebDl1080p,
            size: 1,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state: GrabState::Downloading,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces: None,
        };
        let id = store.record_grab(&grab).await.unwrap();
        let blocked = Blocked {
            id: 0,
            movie_id: Some(movie_id),
            series_id: None,
            source_title: grab.title.clone(),
            indexer: None,
            quality: None,
            size: None,
            hash: Some("aa".into()),
            at: "2026-01-01T01:00:00Z".into(),
            message: Some("sem seeds".into()),
            reason: FailReason::NoSeeds,
        };
        // Baixando: a anotação grava.
        assert!(
            store
                .update_grab(id, GrabState::Downloading, Some("40%"), None, "t")
                .await
                .unwrap()
        );
        // A tela desiste primeiro.
        assert!(
            store
                .fail_grab(id, "tirado da fila na tela", None, "2026-01-01T01:00:00Z")
                .await
                .unwrap()
        );
        // A importação, que leu o grab antes, chega depois: nada grava — nem
        // a falha com bloqueio, nem a anotação que o ressuscitaria, nem o
        // arquivo.
        assert!(
            !store
                .fail_grab(id, "sem seeds", Some(&blocked), "2026-01-01T02:00:00Z")
                .await
                .unwrap()
        );
        assert!(
            !store
                .update_grab(id, GrabState::Downloading, Some("importação: x"), None, "t")
                .await
                .unwrap()
        );
        let file = movie(10, "Um", true).file.unwrap();
        assert!(
            !store
                .import_movie(movie_id, id, &file, &[], "Um.mkv", "t")
                .await
                .unwrap()
        );
        let read = &store.grabs().await.unwrap()[0];
        assert_eq!(read.state, GrabState::Failed);
        assert_eq!(read.message.as_deref(), Some("tirado da fila na tela"));
        assert_eq!(read.finished_at.as_deref(), Some("2026-01-01T01:00:00Z"));
        assert!(store.blocklist().await.unwrap().is_empty());
        assert_eq!(store.movies().await.unwrap()[0].movie.file, None);

        // Baixando, a falha grava o grab e o bloqueio juntos.
        let again = store
            .record_grab(&Grab {
                hash: "bb".into(),
                ..grab.clone()
            })
            .await
            .unwrap();
        assert!(
            store
                .fail_grab(again, "sem seeds", Some(&blocked), "2026-01-01T03:00:00Z")
                .await
                .unwrap()
        );
        let blocks = store.blocklist().await.unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].reason, FailReason::NoSeeds);
        db.drop().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Um cenário só: a falha no meio e a importação inteira.
    async fn importacao_do_filme_e_uma_transacao() {
        let Some(db) = TestDb::new("importa_filme").await else {
            return;
        };
        let store = &db.store;
        let movie_id = added(store, movie(10, "Um", true)).await;
        let old = store.movies().await.unwrap()[0].movie.file.clone().unwrap();
        let subtitle = |path: &str| Subtitle {
            origin: SubtitleOrigin::Import,
            relative_path: path.into(),
            language: Some("pt-BR".into()),
            forced: false,
        };
        store
            .add_movie_subtitle(movie_id, &subtitle("Um (2020).pt-BR.srt"))
            .await
            .unwrap();
        let id = store
            .record_grab(&Grab {
                id: 0,
                movie_id,
                hash: "aa".into(),
                title: "Um.2020.2160p.WEB-DL-GRUPO".into(),
                indexer: "tracker".into(),
                quality: Quality::WebDl2160p,
                size: 1,
                grabbed_at: "2026-01-01T00:00:00Z".into(),
                state: GrabState::Downloading,
                message: None,
                imported_path: None,
                finished_at: None,
                replaces: Some(old.relative_path.clone()),
            })
            .await
            .unwrap();
        let new = MovieFile {
            relative_path: "Um (2020) novo.mkv".into(),
            quality: QualityModel {
                quality: Quality::WebDl2160p,
                revision: Revision::default(),
            },
            ..old.clone()
        };
        // Uma legenda que o banco recusa no meio: o arquivo novo, já
        // gravado na transação, não fica, e o grab segue baixando — a volta
        // seguinte refaz, em vez de achar o arquivo novo e descartar o grab.
        store
            .pool
            .get()
            .await
            .unwrap()
            .batch_execute(
                "CREATE FUNCTION recusa() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN
                     IF NEW.relative_path = ''quebra.srt'' THEN
                         RAISE EXCEPTION ''legenda recusada'';
                     END IF;
                     RETURN NEW;
                 END';
                 CREATE TRIGGER recusa BEFORE INSERT ON subtitle_files
                     FOR EACH ROW EXECUTE FUNCTION recusa();",
            )
            .await
            .unwrap();
        let imported_path = "Um (2020)/Um (2020) novo.mkv";
        assert!(
            store
                .import_movie(
                    movie_id,
                    id,
                    &new,
                    &[subtitle("Um (2020) novo.pt-BR.srt"), subtitle("quebra.srt")],
                    imported_path,
                    "2026-01-01T01:00:00Z",
                )
                .await
                .is_err()
        );
        let entry = &store.movies().await.unwrap()[0];
        assert_eq!(entry.movie.file.as_ref(), Some(&old));
        assert_eq!(entry.subtitles.len(), 1);
        assert_eq!(
            entry.subtitles[0].subtitle.relative_path,
            "Um (2020).pt-BR.srt"
        );
        let grab = &store.grabs().await.unwrap()[0];
        assert_eq!(grab.state, GrabState::Downloading);
        assert_eq!(grab.imported_path, None);

        // Inteira: arquivo, legendas e grab de uma vez.
        assert!(
            store
                .import_movie(
                    movie_id,
                    id,
                    &new,
                    &[subtitle("Um (2020) novo.pt-BR.srt")],
                    imported_path,
                    "2026-01-01T02:00:00Z",
                )
                .await
                .unwrap()
        );
        let entry = &store.movies().await.unwrap()[0];
        assert_eq!(entry.movie.file.as_ref(), Some(&new));
        let paths: Vec<&str> = entry
            .subtitles
            .iter()
            .map(|s| s.subtitle.relative_path.as_str())
            .collect();
        assert_eq!(paths, ["Um (2020) novo.pt-BR.srt"]);
        let grab = &store.grabs().await.unwrap()[0];
        assert_eq!(grab.state, GrabState::Imported);
        assert_eq!(grab.imported_path.as_deref(), Some(imported_path));
        assert_eq!(grab.finished_at.as_deref(), Some("2026-01-01T02:00:00Z"));
        db.drop().await;
    }

    #[tokio::test]
    async fn migracao_do_motivo_preenche_pelo_texto_gravado() {
        let Some(db) = TestDb::before(
            "motivo",
            "ADD COLUMN reason TEXT",
            "INSERT INTO blocklist (source_title, at, message) VALUES
                 ('a', '2026-01-01T00:00:00Z', 'sem seeds há 30 min'),
                 ('b', '2026-01-01T00:00:00Z', 'o tracker não reconhece mais o torrent'),
                 ('c', '2026-01-01T00:00:00Z', 'o cliente marcou o torrent com `error`'),
                 ('d', '2026-01-01T00:00:00Z', 'o cliente não acha os arquivos do torrent'),
                 ('e', '2026-01-01T00:00:00Z', 'o torrent sumiu do cliente'),
                 ('f', '2026-01-01T00:00:00Z', 'marcado como falho na tela'),
                 ('g', '2026-01-01T00:00:00Z', NULL);",
        )
        .await
        else {
            return;
        };
        let mut read: Vec<(String, FailReason, Option<String>)> = db
            .store
            .blocklist()
            .await
            .unwrap()
            .into_iter()
            .map(|b| (b.source_title, b.reason, b.message))
            .collect();
        read.sort_by(|a, b| a.0.cmp(&b.0));
        let reasons: Vec<(&str, FailReason)> =
            read.iter().map(|(t, r, _)| (t.as_str(), *r)).collect();
        assert_eq!(
            reasons,
            [
                ("a", FailReason::NoSeeds),
                ("b", FailReason::Unregistered),
                ("c", FailReason::ClientError),
                ("d", FailReason::MissingFiles),
                ("e", FailReason::Vanished),
                ("f", FailReason::Other),
                ("g", FailReason::Other),
            ]
        );
        // O texto fica, para a tela.
        assert_eq!(read[0].2.as_deref(), Some("sem seeds há 30 min"));
        // E a poda já olha o motivo nas linhas antigas.
        assert_eq!(
            db.store
                .delete_blocks(&[FailReason::NoSeeds], "2026-02-01T00:00:00Z")
                .await
                .unwrap(),
            1
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn busca_guarda_a_ultima_de_cada_filme() {
        let Some(db) = TestDb::new("buscas").await else {
            return;
        };
        let store = &db.store;
        let id = added(store, movie(10, "Um", false)).await;
        let run = |at: &str, pick: bool| SearchRun {
            movie_id: id,
            at: at.into(),
            releases: 3,
            pick: pick.then(|| SearchPick {
                title: "Um.2020.1080p.WEB-DL-GRUPO".into(),
                indexer: "tracker".into(),
                quality: Quality::WebDl1080p,
                size: 4_000_000_000,
            }),
            rejections: vec![("MinimumSeeders".into(), 2)],
            error: None,
        };
        store
            .record_search(&run("2026-01-01T00:00:00Z", false))
            .await
            .unwrap();
        store
            .record_search(&run("2026-01-02T00:00:00Z", true))
            .await
            .unwrap();
        assert_eq!(
            store.latest_searches().await.unwrap(),
            [run("2026-01-02T00:00:00Z", true)]
        );
        db.drop().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // As duas podas da limpeza, lado a lado.
    async fn poda_buscas_velhas_e_bloqueios_vencidos() {
        let Some(db) = TestDb::new("poda").await else {
            return;
        };
        let store = &db.store;
        let um = added(store, movie(10, "Um", false)).await;
        let dois = added(store, movie(20, "Dois", false)).await;
        let run = |movie_id: i64, at: &str| SearchRun {
            movie_id,
            at: at.into(),
            releases: 1,
            pick: None,
            rejections: Vec::new(),
            error: None,
        };
        // Um: três velhas e uma nova; Dois: só velhas — a mais recente fica.
        for at in [
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            "2026-03-01T00:00:00Z",
        ] {
            store.record_search(&run(um, at)).await.unwrap();
        }
        for at in ["2026-01-01T00:00:00Z", "2026-01-05T00:00:00Z"] {
            store.record_search(&run(dois, at)).await.unwrap();
        }
        // Duas no mesmo instante: a de id maior é a mais recente.
        store
            .record_search(&run(dois, "2026-01-05T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(
            store.prune_searches("2026-02-01T00:00:00Z").await.unwrap(),
            5
        );
        let left = store.latest_searches().await.unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(
            store.prune_searches("2026-02-01T00:00:00Z").await.unwrap(),
            0
        );

        let block = |reason: FailReason, message: &str, at: &str| Blocked {
            id: 0,
            movie_id: Some(um),
            series_id: None,
            source_title: "Um.2020.1080p".into(),
            indexer: None,
            quality: None,
            size: None,
            hash: None,
            at: at.into(),
            message: Some(message.into()),
            reason,
        };
        store
            .block(&block(
                FailReason::NoSeeds,
                "sem seeds",
                "2026-01-01T00:00:00Z",
            ))
            .await
            .unwrap();
        store
            .block(&block(
                FailReason::NoSeeds,
                "sem seeds",
                "2026-03-01T00:00:00Z",
            ))
            .await
            .unwrap();
        store
            .block(&block(
                FailReason::Other,
                "marcado como falho na tela",
                "2026-01-01T00:00:00Z",
            ))
            .await
            .unwrap();
        // A poda olha o motivo, não o texto: a mensagem mudada não escapa.
        store
            .block(&block(
                FailReason::NoSeeds,
                "texto de outra versão",
                "2026-01-01T00:00:00Z",
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .delete_blocks(
                    &[FailReason::NoSeeds, FailReason::Unregistered],
                    "2026-02-01T00:00:00Z"
                )
                .await
                .unwrap(),
            2
        );
        let left: Vec<_> = store
            .blocklist()
            .await
            .unwrap()
            .into_iter()
            .map(|b| (b.reason, b.at))
            .collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&(FailReason::NoSeeds, "2026-03-01T00:00:00Z".into())));
        assert!(left.contains(&(FailReason::Other, "2026-01-01T00:00:00Z".into())));
        db.drop().await;
    }

    #[tokio::test]
    async fn migracao_renomeia_as_buscas_sem_perder_dados() {
        let Some(url) = std::env::var("ACERVO_TEST_DATABASE_URL").ok() else {
            eprintln!("ACERVO_TEST_DATABASE_URL ausente: teste de banco pulado");
            return;
        };
        let schema = format!("teste_renomeia_{}", std::process::id());
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        // Banco parado na versão anterior à migração, com uma busca gravada.
        let before = MIGRATIONS
            .iter()
            .position(|m| m.contains("RENAME TO searches"))
            .unwrap();
        let mut setup = format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema};"
        );
        for migration in &MIGRATIONS[..before] {
            setup.push_str(migration);
        }
        let _ = write!(
            setup,
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES ({before});
             INSERT INTO movies (tmdb_id, title, path, monitored, minimum_availability)
                 VALUES (1, 'Um', '/filmes/Um', true, 'released');
             INSERT INTO shadow_runs (movie_id, at, releases, rejections)
                 SELECT id, '2026-01-01T00:00:00Z', 7, '[]' FROM movies;"
        );
        client.batch_execute(&setup).await.unwrap();

        let mut config: tokio_postgres::Config = url.parse().unwrap();
        config.options(format!("-c search_path={schema}"));
        let store = Store::with_config(config).await.unwrap();
        let searches = store.latest_searches().await.unwrap();
        assert_eq!(searches.len(), 1);
        assert_eq!(searches[0].releases, 7);
        let old = client
            .query_one(
                &format!("SELECT to_regclass('{schema}.shadow_runs')::text"),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(old.get::<_, Option<String>>(0), None);
        client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn filme_volta_como_foi_gravado() {
        let Some(db) = TestDb::new("ida_e_volta").await else {
            return;
        };
        let store = &db.store;
        let extras = MovieExtras {
            metadata_title: Some("One".into()),
            poster: Some("https://image.tmdb.org/t/p/original/p.jpg".into()),
            fanart: None,
            refreshed_at: Some("2026-01-03T00:00:00Z".into()),
        };
        let with_file = movie(10, "Um", true);
        let id = store.add_movie(&with_file, &extras).await.unwrap();
        let without = added(store, movie(20, "Dois", false)).await;
        let movies = store.movies().await.unwrap();
        assert_eq!(movies.len(), 2);
        assert_eq!(
            movies[0],
            CatalogMovie {
                id: without,
                movie: movie(20, "Dois", false),
                extras: MovieExtras::default(),
                priority: false,
                subtitles: Vec::new(),
            }
        );
        assert_eq!(
            movies[1],
            CatalogMovie {
                id,
                movie: with_file.clone(),
                extras: extras.clone(),
                priority: false,
                subtitles: Vec::new(),
            }
        );
        // A atualização de metadados parte de uma leitura velha: grava só o
        // que é da base, nunca monitorado, pasta, prioridade nem arquivo.
        assert!(store.set_movie_monitored(id, false).await.unwrap());
        assert!(store.set_movie_priority(id, true).await.unwrap());
        let stale = Movie {
            title: "Um Novo".into(),
            year: Some(2021),
            monitored: true,
            path: "/outro/lugar".into(),
            file: None,
            alternate_titles: vec!["Outro".into()],
            overview: Some("Sinopse nova".into()),
            ..with_file.clone()
        };
        assert!(store.update_movie_metadata(id, &stale).await.unwrap());
        let movies = store.movies().await.unwrap();
        let read = movies.iter().find(|m| m.id == id).unwrap();
        assert_eq!(read.movie.title, "Um Novo");
        assert_eq!(read.movie.year, Some(2021));
        assert_eq!(read.movie.overview.as_deref(), Some("Sinopse nova"));
        assert_eq!(read.movie.alternate_titles, ["Outro"]);
        assert!(!read.movie.monitored);
        assert_eq!(read.movie.path, with_file.path);
        assert_eq!(read.movie.file, with_file.file);
        assert!(read.priority);
        assert_eq!(read.extras, extras);
        // O filme que saiu no meio é pulado, sem erro.
        assert!(!store.update_movie_metadata(999, &stale).await.unwrap());
        assert!(!store.set_movie_monitored(999, true).await.unwrap());
        store.set_extras(999, &extras).await.unwrap();
        db.drop().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Montar o banco antigo e conferir tudo o que saiu.
    async fn migracao_tira_a_api_v3_sem_perder_filmes() {
        let Some(url) = std::env::var("ACERVO_TEST_DATABASE_URL").ok() else {
            eprintln!("ACERVO_TEST_DATABASE_URL ausente: teste de banco pulado");
            return;
        };
        let schema = format!("teste_sem_v3_{}", std::process::id());
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        // Banco parado na versão anterior, com perfil, tag, origem no
        // gerenciador e arquivo com id e faixas.
        let before = MIGRATIONS
            .iter()
            .position(|m| m.contains("DROP TABLE quality_profiles"))
            .unwrap();
        let mut setup = format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema};"
        );
        for migration in &MIGRATIONS[..before] {
            setup.push_str(migration);
        }
        let _ = write!(
            setup,
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES ({before});
             INSERT INTO quality_profiles (name, upgrade_allowed, items)
                 VALUES ('Any', false, '[]');
             INSERT INTO tags (label) VALUES ('pedido');
             INSERT INTO custom_formats (name, specifications) VALUES ('x', '[]');
             INSERT INTO exclusions (tmdb_id, title) VALUES (9, 'Fora');
             INSERT INTO import_lists (name, kind, settings, enabled, monitor, search_on_add,
                     quality_profile_id, root_folder, minimum_availability)
                 SELECT 'lista', 'tmdb', '{{}}', true, true, false, id, '/filmes', 'released'
                 FROM quality_profiles;
             INSERT INTO movies (tmdb_id, imdb_id, title, path, monitored, quality_profile_id,
                     source, source_id, runtime, available, in_cinemas, overview, tags,
                     metadata_title, poster)
                 SELECT 10, 'tt0000010', 'Um', '/filmes/Um (2020)', true, id, 'radarr', 77,
                     110, true, '2020-01-10', 'Sinopse', '[1]', 'One', 'p.jpg'
                 FROM quality_profiles;
             INSERT INTO movie_titles (movie_id, title) SELECT id, 'Um 2' FROM movies;
             INSERT INTO movie_files (movie_id, relative_path, size, quality, revision_version,
                     revision_real, is_repack, languages, release_group, scene_name, file_id,
                     media_info)
                 SELECT id, 'Um (2020).mkv', 4000, 3, 1, 0, false, '[\"English\"]', 'GRUPO',
                     'Um.2020.1080p.WEB-DL-GRUPO', 501, '{{\"audioLanguages\": \"eng\"}}'
                 FROM movies;"
        );
        client.batch_execute(&setup).await.unwrap();

        let mut config: tokio_postgres::Config = url.parse().unwrap();
        config.options(format!("-c search_path={schema}"));
        let store = Store::with_config(config).await.unwrap();
        let movies = store.movies().await.unwrap();
        assert_eq!(movies.len(), 1);
        let entry = &movies[0];
        assert_eq!(entry.movie.tmdb_id, 10);
        assert_eq!(entry.movie.title, "Um");
        assert_eq!(entry.movie.path, "/filmes/Um (2020)");
        assert!(entry.movie.monitored);
        assert_eq!(entry.movie.runtime, 110);
        assert_eq!(entry.movie.in_cinemas.as_deref(), Some("2020-01-10"));
        assert_eq!(entry.movie.alternate_titles, ["Um 2"]);
        assert_eq!(entry.extras.metadata_title.as_deref(), Some("One"));
        let file = entry.movie.file.as_ref().unwrap();
        assert_eq!(file.relative_path, "Um (2020).mkv");
        assert_eq!(file.size, 4000);
        assert_eq!(file.quality.quality, Quality::WebDl1080p);
        assert_eq!(file.languages, ["English"]);
        assert_eq!(file.release_group.as_deref(), Some("GRUPO"));
        for gone in [
            "quality_profiles",
            "tags",
            "custom_formats",
            "exclusions",
            "import_lists",
            "movie_file_ids",
        ] {
            let row = client
                .query_one(&format!("SELECT to_regclass('{schema}.{gone}')::text"), &[])
                .await
                .unwrap();
            assert_eq!(row.get::<_, Option<String>>(0), None, "{gone}");
        }
        let columns = client
            .query(
                "SELECT table_name || '.' || column_name FROM information_schema.columns
                 WHERE table_schema = $1 AND table_name <> 'episodes' AND column_name IN
                     ('quality_profile_id', 'tags', 'source', 'source_id', 'file_id', 'media_info')",
                &[&schema],
            )
            .await
            .unwrap();
        assert!(
            columns.is_empty(),
            "{:?}",
            columns
                .iter()
                .map(|r| r.get::<_, String>(0))
                .collect::<Vec<_>>()
        );
        client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn guarda_historico_e_bloqueio() {
        let Some(db) = TestDb::new("gerenciador").await else {
            return;
        };
        let store = &db.store;
        let movie_id = added(store, movie(10, "Um", false)).await;

        let event = |at: &str| NewHistory {
            movie_id: Some(movie_id),
            series_id: None,
            episode_ids: vec![],
            movie_title: "Um (2020)".into(),
            event: "grabbed".into(),
            at: at.into(),
            source_title: Some("Um.2020.1080p.WEB-DL".into()),
            quality: Some(Quality::WebDl1080p),
            indexer: Some("tracker".into()),
            download_id: None,
            data: serde_json::json!({ "origem": "tela" }),
        };
        store
            .record_history(&event("2026-01-02T00:00:00Z"))
            .await
            .unwrap();
        store
            .record_history(&event("2026-01-01T00:00:00Z"))
            .await
            .unwrap();
        let page = store.history(Some(movie_id), None, 10, 0).await.unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.events[0].at, "2026-01-02T00:00:00Z");
        // O filme sai; o histórico fica, sem o id.
        store.delete_movie(movie_id).await.unwrap();
        let page = store.history(None, Some("grabbed"), 10, 0).await.unwrap();
        assert_eq!(page.total, 2);
        assert!(page.events.iter().all(|e| e.movie_id.is_none()));

        let blocked = store
            .block(&Blocked {
                id: 0,
                movie_id: None,
                series_id: None,
                source_title: "Ruim.2020.CAM".into(),
                indexer: None,
                quality: None,
                size: Some(1),
                hash: None,
                at: "2026-01-01T00:00:00Z".into(),
                message: Some("falhou".into()),
                reason: FailReason::Other,
            })
            .await
            .unwrap();
        assert_eq!(store.blocklist().await.unwrap().len(), 1);
        assert!(store.unblock(blocked).await.unwrap());

        db.drop().await;
    }

    #[tokio::test]
    async fn prioridade_caminho_e_legendas_do_filme() {
        let Some(db) = TestDb::new("filme_extras").await else {
            return;
        };
        let store = &db.store;
        let id = added(store, movie(10, "Um", true)).await;
        let other = added(store, movie(20, "Dois", false)).await;
        assert!(!store.movies().await.unwrap()[1].priority);
        assert!(store.set_movie_priority(id, true).await.unwrap());
        assert!(!store.set_movie_priority(9999, true).await.unwrap());
        let movies = store.movies().await.unwrap();
        let entry = movies.iter().find(|m| m.id == id).unwrap();
        assert!(entry.priority);

        // Só o grab em andamento de obra prioritária.
        let grab = |movie_id: i64, hash: &str| Grab {
            id: 0,
            movie_id,
            hash: hash.into(),
            title: "Um.2020.1080p.WEB-DL-GRUPO".into(),
            indexer: "tracker".into(),
            quality: Quality::WebDl1080p,
            size: 1,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state: GrabState::Downloading,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces: None,
        };
        store.record_grab(&grab(id, "aa")).await.unwrap();
        store.record_grab(&grab(other, "bb")).await.unwrap();
        let done = store.record_grab(&grab(id, "cc")).await.unwrap();
        store
            .update_grab(
                done,
                GrabState::Imported,
                None,
                None,
                "2026-01-02T00:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(
            store.priority_hashes().await.unwrap(),
            std::collections::HashSet::from(["aa".to_owned()])
        );

        let sub = Subtitle {
            relative_path: "Um (2020).pt-BR.srt".into(),
            language: Some("pt-BR".into()),
            forced: false,
            origin: SubtitleOrigin::Import,
        };
        let sub_id = store.add_movie_subtitle(id, &sub).await.unwrap();
        assert!(
            store
                .set_movie_file_path(id, "Um (2020) novo.mkv")
                .await
                .unwrap()
        );
        assert!(!store.set_movie_file_path(other, "x.mkv").await.unwrap());
        let movies = store.movies().await.unwrap();
        let entry = movies.iter().find(|m| m.id == id).unwrap();
        assert_eq!(
            entry.movie.file.as_ref().unwrap().relative_path,
            "Um (2020) novo.mkv"
        );
        assert_eq!(entry.subtitles.len(), 1);
        assert_eq!(entry.subtitles[0].id, sub_id);
        assert_eq!(entry.subtitles[0].subtitle, sub);
        // Mudar o monitorado e os metadados não perde a legenda.
        store.set_movie_monitored(id, false).await.unwrap();
        store.update_movie_metadata(id, &entry.movie).await.unwrap();
        let movies = store.movies().await.unwrap();
        assert_eq!(
            movies.iter().find(|m| m.id == id).unwrap().subtitles.len(),
            1
        );
        // Trocar o arquivo leva as legendas do antigo.
        store.set_movie_file(id, None).await.unwrap();
        let movies = store.movies().await.unwrap();
        assert!(
            movies
                .iter()
                .find(|m| m.id == id)
                .unwrap()
                .subtitles
                .is_empty()
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn migracao_da_prioridade_nasce_falsa_nos_que_existem() {
        let Some(url) = std::env::var("ACERVO_TEST_DATABASE_URL").ok() else {
            eprintln!("ACERVO_TEST_DATABASE_URL ausente: teste de banco pulado");
            return;
        };
        let schema = format!("teste_prioridade_{}", std::process::id());
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        let before = MIGRATIONS
            .iter()
            .position(|m| m.contains("ADD COLUMN priority"))
            .unwrap();
        let mut setup = format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema};"
        );
        for migration in &MIGRATIONS[..before] {
            setup.push_str(migration);
        }
        let _ = write!(
            setup,
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES ({before});
             INSERT INTO movies (tmdb_id, title, path, monitored) VALUES (1, 'Um', '/f/Um', true);
             INSERT INTO series (tmdb_id, title, path, season_folder, monitor_new)
                 VALUES (2, 'Dois', '/s/Dois', true, true);"
        );
        client.batch_execute(&setup).await.unwrap();
        let mut config: tokio_postgres::Config = url.parse().unwrap();
        config.options(format!("-c search_path={schema}"));
        let store = Store::with_config(config).await.unwrap();
        let movies = store.movies().await.unwrap();
        assert!(!movies[0].priority);
        let series = store.series_list().await.unwrap();
        assert!(!series[0].priority);
        assert!(series[0].scene.is_empty() && series[0].subtitles.is_empty());
        client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
    }
}

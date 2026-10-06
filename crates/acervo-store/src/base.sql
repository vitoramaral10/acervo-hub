-- Schema consolidado na versão 35. Novas migrações começam na versão 36.
CREATE TABLE movies (
    id BIGSERIAL PRIMARY KEY,
    tmdb_id BIGINT NOT NULL UNIQUE,
    imdb_id TEXT,
    title TEXT NOT NULL,
    original_title TEXT,
    original_language TEXT,
    year INTEGER,
    status TEXT,
    monitored BOOLEAN NOT NULL,
    path TEXT NOT NULL,
    added TEXT,
    runtime INTEGER NOT NULL DEFAULT 0,
    clean_title TEXT,
    in_cinemas TEXT,
    digital_release TEXT,
    physical_release TEXT,
    overview TEXT,
    metadata_title TEXT,
    poster TEXT,
    fanart TEXT,
    metadata_refreshed_at TEXT,
    priority BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE movie_files (
    movie_id BIGINT PRIMARY KEY REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
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
    movie_id BIGINT NOT NULL REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
    title TEXT NOT NULL
);

CREATE TABLE searches (
    id BIGSERIAL PRIMARY KEY,
    movie_id BIGINT NOT NULL REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
    at TEXT NOT NULL,
    releases BIGINT NOT NULL,
    pick_title TEXT,
    pick_indexer TEXT,
    pick_quality SMALLINT,
    pick_size BIGINT,
    rejections JSONB NOT NULL,
    error TEXT
);

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

CREATE TABLE grabs (
    id BIGSERIAL PRIMARY KEY,
    movie_id BIGINT NOT NULL REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
    hash TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    indexer TEXT NOT NULL,
    quality SMALLINT NOT NULL,
    size BIGINT NOT NULL,
    grabbed_at TEXT NOT NULL,
    state TEXT NOT NULL,
    message TEXT,
    imported_path TEXT,
    finished_at TEXT,
    replaces TEXT
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE task_runs (
    id BIGSERIAL PRIMARY KEY,
    task TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    ok BOOLEAN NOT NULL,
    summary TEXT NOT NULL,
    detail JSONB
);

CREATE TABLE config_sections (
    name TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE indexers (
    name TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind = 'cardigann'),
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
    refreshed_at TEXT,
    priority BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE series_titles (
    id BIGSERIAL PRIMARY KEY,
    series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    title TEXT NOT NULL
);

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
    skip TEXT CHECK (skip IN ('unwanted', 'deleted')),
    skipped_at TEXT,
    file_id BIGINT REFERENCES episode_files(id) ON DELETE SET NULL,
    UNIQUE (series_id, season, number)
);

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

CREATE TABLE series_grab_episodes (
    grab_id BIGINT NOT NULL REFERENCES series_grabs(id) ON DELETE CASCADE,
    episode_id BIGINT NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    PRIMARY KEY (grab_id, episode_id)
);

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
    data JSONB NOT NULL DEFAULT '{}',
    series_id BIGINT REFERENCES series(id) ON DELETE SET NULL,
    episode_ids JSONB NOT NULL DEFAULT '[]'
);

CREATE TABLE blocklist (
    id BIGSERIAL PRIMARY KEY,
    movie_id BIGINT REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
    source_title TEXT NOT NULL,
    indexer TEXT,
    quality SMALLINT,
    size BIGINT,
    hash TEXT,
    at TEXT NOT NULL,
    message TEXT,
    series_id BIGINT REFERENCES series(id) ON DELETE CASCADE,
    reason TEXT NOT NULL DEFAULT 'other'
        CHECK (reason IN ('no_seeds', 'unregistered', 'client_error', 'missing_files',
            'vanished', 'other'))
);

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

CREATE TABLE subtitle_files (
    id BIGSERIAL PRIMARY KEY,
    movie_id BIGINT REFERENCES movies(id) ON DELETE CASCADE ON UPDATE CASCADE,
    episode_file_id BIGINT REFERENCES episode_files(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    language TEXT,
    forced BOOLEAN NOT NULL DEFAULT FALSE,
    origin TEXT NOT NULL DEFAULT 'importacao' CHECK (origin IN ('importacao', 'disco')),
    CONSTRAINT subtitle_files_check CHECK ((movie_id IS NULL) <> (episode_file_id IS NULL)),
    UNIQUE (movie_id, relative_path),
    UNIQUE (episode_file_id, relative_path)
);

CREATE TABLE scene_mappings (
    series_id BIGINT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    scene_season INTEGER NOT NULL,
    scene_episode INTEGER NOT NULL,
    season INTEGER NOT NULL,
    episode INTEGER NOT NULL,
    PRIMARY KEY (series_id, scene_season, scene_episode, season, episode)
);

CREATE TABLE definitions (
    id TEXT PRIMARY KEY,
    yaml TEXT NOT NULL,
    sha TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE deletion_marks (
    id BIGSERIAL PRIMARY KEY,
    movie_id BIGINT REFERENCES movies(id) ON DELETE CASCADE,
    series_id BIGINT REFERENCES series(id) ON DELETE CASCADE,
    season INTEGER CHECK (season >= 0),
    marked_at TEXT NOT NULL,
    CONSTRAINT deletion_marks_check CHECK ((movie_id IS NULL) <> (series_id IS NULL)),
    CONSTRAINT deletion_marks_check1 CHECK (season IS NULL OR series_id IS NOT NULL)
);

CREATE TABLE discover_hidden_titles (
    kind TEXT NOT NULL CHECK (kind IN ('filme','serie')),
    tmdb_id INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    hidden_at TEXT NOT NULL,
    PRIMARY KEY (kind, tmdb_id)
);

CREATE INDEX movie_titles_by_movie ON movie_titles(movie_id);
CREATE INDEX searches_by_movie ON searches(movie_id, at);
CREATE INDEX sessions_by_expiry ON sessions(expires_at);
CREATE INDEX grabs_by_movie ON grabs(movie_id, grabbed_at);
CREATE INDEX history_by_at ON history(at DESC);
CREATE INDEX history_by_movie ON history(movie_id, at);
CREATE INDEX blocklist_by_movie ON blocklist(movie_id);
CREATE INDEX task_runs_by_task ON task_runs(task, id);
CREATE INDEX series_titles_by_series ON series_titles(series_id);
CREATE INDEX episodes_by_file ON episodes(file_id);
CREATE INDEX series_grabs_by_series ON series_grabs(series_id, grabbed_at);
CREATE INDEX series_grab_episodes_by_episode ON series_grab_episodes(episode_id);
CREATE INDEX history_by_series ON history(series_id, at);
CREATE INDEX blocklist_by_series ON blocklist(series_id);
CREATE INDEX series_searches_by_series ON series_searches(series_id, at);
CREATE INDEX subtitle_files_by_episode_file ON subtitle_files(episode_file_id);
CREATE INDEX blocklist_by_reason ON blocklist(reason, at);
CREATE UNIQUE INDEX deletion_marks_by_movie ON deletion_marks(movie_id)
    WHERE movie_id IS NOT NULL;
CREATE UNIQUE INDEX deletion_marks_by_series ON deletion_marks(series_id, COALESCE(season, -1))
    WHERE series_id IS NOT NULL;

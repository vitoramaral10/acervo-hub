//! Parser de nome de release.
//!
//! Porte do parser do gerenciador de filmes que este projeto substitui
//! (GPL-3.0, a mesma licença): mesmos padrões, mesma ordem de decisão. A
//! fidelidade foi conferida contra a leitura dele em títulos reais; os casos
//! da referência ficam nos testes de unidade. Onde este porte diverge de
//! propósito, o código diz por quê.
//!
//! O parser de episódio (`episode.rs`) é o porte do equivalente do gerenciador
//! de séries, conferido em títulos reais; qualidade, idioma e grupo
//! de episódio são variantes à parte, porque ele não lê igual ao de filmes.
//!
//! Erro de parser não derruba nada: importa o arquivo errado em silêncio. Por
//! isso ele é o crate mais testado do projeto.

mod common;
mod episode;
mod episode_table;
mod group;
mod language;
mod movie;
mod quality;
mod quality_episode;
mod repeat;
mod title;

pub use episode::{ParsedEpisode, SeriesTitleInfo, parse_episode_path, parse_episode_title};
pub use group::{parse_episode_release_group, parse_release_group};
pub use language::{Language, parse_episode_languages, parse_languages};
pub use movie::{ParsedMovie, parse_movie_title};
pub use quality::{
    Modifier, Quality, QualityModel, Revision, Source, parse_quality, parse_quality_name,
};
pub use quality_episode::parse_episode_quality;
pub use title::{clean_movie_title, clean_series_title, normalize_movie_title, remove_accents};

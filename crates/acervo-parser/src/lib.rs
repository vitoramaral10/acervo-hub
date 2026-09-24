//! Parser de nome de release.
//!
//! Porte do parser do gerenciador de filmes que este projeto substitui
//! (GPL-3.0, a mesma licença): mesmos padrões, mesma ordem de decisão. A
//! fidelidade se mede contra a leitura dele num corpus de títulos reais — o
//! teste `corpus`, ignorado por padrão. Onde este porte diverge de propósito,
//! o código diz por quê.
//!
//! Erro de parser não derruba nada: importa o arquivo errado em silêncio. Por
//! isso ele é o crate mais testado do projeto.

mod common;
mod group;
mod language;
mod movie;
mod quality;

pub use group::parse_release_group;
pub use language::{Language, parse_languages};
pub use movie::{ParsedMovie, parse_movie_title};
pub use quality::{
    Modifier, Quality, QualityModel, Revision, Source, parse_quality, parse_quality_name,
};

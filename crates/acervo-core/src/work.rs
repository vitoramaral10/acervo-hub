//! A obra e sua unidade baixável.
//!
//! Aqui mora a tese que colapsa dois serviços num só: séries e filmes diferem
//! apenas na aridade da árvore. Um filme é uma obra com exatamente um item.

use crate::ids::{ItemId, WorkId};

/// O tipo de mídia. É o único discriminador entre o que hoje são dois serviços.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Series,
    Movie,
}

/// A posição de um item dentro da obra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ordinal {
    Episode {
        season: u16,
        number: u16,
    },
    /// O filme inteiro: obra de item único.
    Feature,
}

impl Ordinal {
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self {
            Self::Episode { .. } => Kind::Series,
            Self::Feature => Kind::Movie,
        }
    }
}

/// Identificadores externos. Nenhum é obrigatório: obras entram no acervo por
/// caminhos diferentes e nem todo provedor conhece todo título.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalIds {
    pub tvdb: Option<u32>,
    pub tmdb: Option<u32>,
    pub imdb: Option<String>,
}

impl ExternalIds {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tvdb.is_none() && self.tmdb.is_none() && self.imdb.is_none()
    }
}

/// Uma série ou um filme no acervo.
#[derive(Debug, Clone)]
pub struct Work {
    pub id: WorkId,
    pub kind: Kind,
    pub title: String,
    pub ids: ExternalIds,
    pub monitored: bool,
}

/// A unidade que se busca, se baixa e se importa.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: ItemId,
    pub work: WorkId,
    pub ordinal: Ordinal,
    pub monitored: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinal_determina_o_tipo_da_obra() {
        assert_eq!(Ordinal::Feature.kind(), Kind::Movie);
        assert_eq!(
            Ordinal::Episode {
                season: 1,
                number: 2
            }
            .kind(),
            Kind::Series
        );
    }
}

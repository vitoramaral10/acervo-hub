//! Tamanhos em disco, com a distinção que o `du` esconde.
//!
//! Clientes de torrent pré-alocam arquivos esparsos: o `st_size` fica do tamanho
//! final do download desde o primeiro byte. Somar `st_size` de download
//! incompleto superestima em ordem de grandeza — uma medição real de 33 torrents
//! pausados deu 288 GB aparentes contra 14 GB de fato alocados.
//!
//! Por isso [`Allocated`] e [`Apparent`] são tipos distintos e **não há conversão
//! entre eles**. Só [`Allocated`] soma para "espaço que volta ao apagar".

use std::fmt;
use std::iter::Sum;
use std::ops::Add;

/// Bytes de fato ocupados no disco: `st_blocks * 512`.
///
/// É a única medida válida para decidir quanto espaço uma remoção libera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Allocated(u64);

/// Tamanho nominal do arquivo: `st_size`. Serve para exibir, nunca para decidir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Apparent(u64);

impl Allocated {
    pub const ZERO: Self = Self(0);

    /// Constrói a partir da contagem de blocos de 512 bytes do `stat(2)`.
    #[must_use]
    pub const fn from_blocks(blocks: u64) -> Self {
        Self(blocks.saturating_mul(512))
    }

    #[must_use]
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Fração que `self` representa de `total`. Devolve `0.0` se `total` é zero.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn fraction_of(self, total: Self) -> f64 {
        if total.0 == 0 {
            0.0
        } else {
            self.0 as f64 / total.0 as f64
        }
    }
}

impl Apparent {
    #[must_use]
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl Add for Allocated {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Sum for Allocated {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

impl fmt::Display for Allocated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", human(self.0))
    }
}

impl fmt::Display for Apparent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (aparente)", human(self.0))
    }
}

#[allow(clippy::cast_precision_loss)]
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocos_viram_bytes() {
        assert_eq!(Allocated::from_blocks(2).as_u64(), 1024);
    }

    #[test]
    fn arquivo_esparso_aloca_menos_que_aparenta() {
        // O caso real: torrent pré-alocado de 10 GiB com 1 GiB baixado.
        let aparente = Apparent::from_bytes(10 * 1024 * 1024 * 1024);
        let alocado = Allocated::from_blocks(2 * 1024 * 1024);
        assert!(alocado.as_u64() < aparente.as_u64());
    }

    #[test]
    fn soma_satura_em_vez_de_estourar() {
        let quase = Allocated::from_bytes(u64::MAX);
        assert_eq!((quase + Allocated::from_bytes(10)).as_u64(), u64::MAX);
    }

    #[test]
    fn fracao_de_total_zero_nao_divide_por_zero() {
        assert!(
            (Allocated::from_bytes(10).fraction_of(Allocated::ZERO) - 0.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn formata_em_unidade_legivel() {
        assert_eq!(Allocated::from_bytes(512).to_string(), "512 B");
        assert_eq!(Allocated::from_bytes(1536).to_string(), "1.5 KiB");
    }
}

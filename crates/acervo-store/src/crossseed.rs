//! Cross-seed: as consultas já feitas a cada indexador por um torrent do
//! cliente, para não buscar o mesmo de novo a cada volta. Achado ou não,
//! fica gravado; o que não achou volta a ser procurado depois de um tempo
//! (o outro tracker pode subir o release dias depois).

use crate::{Result, Store};

/// Uma consulta de cross-seed de um torrent a um indexador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossSeedCheck {
    /// Infohash do torrent de origem, em minúsculas.
    pub hash: String,
    pub indexer: String,
    /// RFC 3339, em UTC: a última consulta.
    pub checked_at: String,
    /// Quantas vezes foi consultado.
    pub attempts: i32,
    /// Infohash do torrent irmão adicionado, se achou.
    pub found: Option<String>,
}

impl Store {
    /// Todas as consultas de cross-seed gravadas.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn cross_seed_checks(&self) -> Result<Vec<CrossSeedCheck>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT hash, indexer, checked_at, attempts, found FROM cross_seed_checks",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok(CrossSeedCheck {
                    hash: row.try_get(0)?,
                    indexer: row.try_get(1)?,
                    checked_at: row.try_get(2)?,
                    attempts: row.try_get(3)?,
                    found: row.try_get(4)?,
                })
            })
            .collect()
    }

    /// Grava uma consulta: soma uma tentativa e guarda o irmão, se achou.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn record_cross_seed_check(
        &self,
        hash: &str,
        indexer: &str,
        checked_at: &str,
        found: Option<&str>,
    ) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO cross_seed_checks (hash, indexer, checked_at, attempts, found)
                 VALUES ($1, $2, $3, 1, $4)
                 ON CONFLICT (hash, indexer) DO UPDATE
                 SET checked_at = EXCLUDED.checked_at,
                     attempts = cross_seed_checks.attempts + 1,
                     found = COALESCE(EXCLUDED.found, cross_seed_checks.found)",
                &[&hash.to_ascii_lowercase(), &indexer, &checked_at, &found],
            )
            .await?;
        Ok(())
    }

    /// Esquece o irmão recusado na conferência: o par volta a ser
    /// procurado no próximo intervalo, e a tentativa já contou.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn forget_cross_seed(&self, found: &str) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE cross_seed_checks SET found = NULL WHERE found = $1",
                &[&found.to_ascii_lowercase()],
            )
            .await?;
        Ok(())
    }
}

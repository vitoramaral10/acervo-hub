//! Cross-seed: as consultas já feitas a cada indexador por um torrent do
//! cliente, para não buscar o mesmo de novo a cada volta. Achado ou não,
//! fica gravado; o que não achou volta a ser procurado depois de um tempo
//! (o outro tracker pode subir o release dias depois).

use serde::{Deserialize, Serialize};

use crate::{Result, Store, StoreError};

/// Como terminou uma consulta de cross-seed. Alimenta a tela "Para lançar":
/// `NotFound` e `LayoutDiffers` fazem do par um candidato a lançamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossSeedOutcome {
    /// Achou e adicionou (ou já estava no cliente) o irmão.
    Found,
    /// Buscou e nenhum release de tamanho parecido existe.
    NotFound,
    /// Há release de tamanho parecido, mas com arquivos diferentes.
    LayoutDiffers,
    /// A busca ou o download do `.torrent` falhou.
    Error,
    /// Sem consulta possível (pacote parcial, sem busca montável).
    Skipped,
}

impl CrossSeedOutcome {
    /// O texto gravado na coluna `outcome`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::NotFound => "not_found",
            Self::LayoutDiffers => "layout_differs",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "found" => Self::Found,
            "not_found" => Self::NotFound,
            "layout_differs" => Self::LayoutDiffers,
            "error" => Self::Error,
            "skipped" => Self::Skipped,
            other => {
                return Err(StoreError::Corrupt(format!(
                    "desfecho de cross-seed desconhecido: {other}"
                )));
            }
        })
    }
}

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
    /// Como terminou a última consulta.
    pub outcome: CrossSeedOutcome,
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
                "SELECT hash, indexer, checked_at, attempts, found, outcome FROM cross_seed_checks",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                let outcome: String = row.try_get(5)?;
                Ok(CrossSeedCheck {
                    hash: row.try_get(0)?,
                    indexer: row.try_get(1)?,
                    checked_at: row.try_get(2)?,
                    attempts: row.try_get(3)?,
                    found: row.try_get(4)?,
                    outcome: CrossSeedOutcome::parse(&outcome)?,
                })
            })
            .collect()
    }

    /// Grava uma consulta: soma uma tentativa, guarda o irmão, se achou, e o
    /// desfecho da volta (que sempre substitui o anterior). Consulta que deu
    /// erro (busca falhou, `.torrent` recusado, conta presa no tracker) não
    /// gasta tentativa: não diz nada sobre existir ou não no destino.
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
        outcome: CrossSeedOutcome,
    ) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO cross_seed_checks (hash, indexer, checked_at, attempts, found, outcome)
                 VALUES ($1, $2, $3, CASE WHEN $5 = 'error' THEN 0 ELSE 1 END, $4, $5)
                 ON CONFLICT (hash, indexer) DO UPDATE
                 SET checked_at = EXCLUDED.checked_at,
                     attempts = cross_seed_checks.attempts + EXCLUDED.attempts,
                     found = COALESCE(EXCLUDED.found, cross_seed_checks.found),
                     outcome = EXCLUDED.outcome",
                &[
                    &hash.to_ascii_lowercase(),
                    &indexer,
                    &checked_at,
                    &found,
                    &outcome.as_str(),
                ],
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

    /// Apaga a consulta de um par, para ele voltar a ser procurado do zero
    /// (sem a contagem de tentativas). Só apaga se não achou irmão: par com
    /// `found` preenchido guarda um torrent que o cliente ainda tem.
    /// Devolve se apagou.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn reset_cross_seed_pair(&self, hash: &str, indexer: &str) -> Result<bool> {
        let client = self.pool.get().await?;
        let deleted = client
            .execute(
                "DELETE FROM cross_seed_checks
                 WHERE hash = $1 AND indexer = $2 AND found IS NULL",
                &[&hash.to_ascii_lowercase(), &indexer],
            )
            .await?;
        Ok(deleted > 0)
    }
}

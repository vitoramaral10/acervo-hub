//! As definições Cardigann guardadas ao cadastrar indexadores.

use crate::{Result, Store};

/// Uma definição guardada no banco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionRow {
    pub id: String,
    pub yaml: String,
    /// SHA-256 do YAML, em hexadecimal.
    pub sha: String,
    pub updated_at: String,
}

impl Store {
    /// Todas as definições guardadas, por id.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn definitions(&self) -> Result<Vec<DefinitionRow>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, yaml, sha, updated_at FROM definitions ORDER BY id",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok(DefinitionRow {
                    id: row.try_get(0)?,
                    yaml: row.try_get(1)?,
                    sha: row.try_get(2)?,
                    updated_at: row.try_get(3)?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cadastro_guarda_so_a_definicao_escolhida_e_conflito_nao_a_troca() {
        let Some(db) = crate::testing::TestDb::new("definicao_escolhida").await else {
            return;
        };
        let record = crate::IndexerRecord {
            name: "um".into(),
            kind: "cardigann".into(),
            definition: None,
            url: None,
            settings: std::collections::BTreeMap::new(),
            enabled: true,
            added_at: None,
        };
        let row = DefinitionRow {
            id: "um".into(),
            yaml: "id: um".into(),
            sha: "s1".into(),
            updated_at: "t0".into(),
        };
        assert!(
            db.store
                .insert_indexer_with_definition(&record, Some(&row))
                .await
                .unwrap()
        );
        let newer = DefinitionRow {
            yaml: "mudou".into(),
            ..row.clone()
        };
        assert!(
            !db.store
                .insert_indexer_with_definition(&record, Some(&newer))
                .await
                .unwrap()
        );
        assert_eq!(db.store.definitions().await.unwrap(), [row]);
        db.drop().await;
    }
}

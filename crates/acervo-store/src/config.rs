//! Configuração do serviço: as seções editadas pela tela, os indexadores
//! cadastrados e os strikes da limpeza.
//!
//! O banco guarda cada seção como JSON e não conhece o formato dela: quem
//! valida é o binário, que é quem sabe o que cada campo quer dizer.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{Result, Store, StoreError};

/// Um indexador cadastrado.
#[derive(Clone, PartialEq, Eq)]
pub struct IndexerRecord {
    /// O nome servido em `/<nome>/api`; num Cardigann, o id da definição.
    pub name: String,
    /// `torznab` ou `cardigann`.
    pub kind: String,
    /// Arquivo YAML da definição, num Cardigann.
    pub definition: Option<String>,
    /// Endpoint, num Torznab; num Cardigann, o link da definição escolhido
    /// (ausente é o primeiro).
    pub url: Option<String>,
    /// Settings da definição, ou `api_key` e `request_interval_seconds` de um
    /// Torznab. Guarda segredo.
    pub settings: BTreeMap<String, String>,
    pub enabled: bool,
    pub added_at: Option<String>,
}

impl std::fmt::Debug for IndexerRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Settings carregam senha, cookie e chave: só os nomes aparecem.
        formatter
            .debug_struct("IndexerRecord")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("definition", &self.definition)
            .field("enabled", &self.enabled)
            .field("settings", &self.settings.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn settings_value(settings: &BTreeMap<String, String>) -> Value {
    Value::Object(
        settings
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect(),
    )
}

fn settings_map(value: Value) -> Result<BTreeMap<String, String>> {
    let Value::Object(map) = value else {
        return Err(StoreError::Corrupt(
            "settings de indexador fora de um objeto".into(),
        ));
    };
    Ok(map
        .into_iter()
        .map(|(name, value)| match value {
            Value::String(text) => (name, text),
            other => (name, other.to_string()),
        })
        .collect())
}

fn read_indexer(row: &tokio_postgres::Row) -> Result<IndexerRecord> {
    Ok(IndexerRecord {
        name: row.try_get(0)?,
        kind: row.try_get(1)?,
        definition: row.try_get(2)?,
        url: row.try_get(3)?,
        settings: settings_map(row.try_get(4)?)?,
        enabled: row.try_get(5)?,
        added_at: row.try_get(6)?,
    })
}

const INSERT_INDEXER: &str =
    "INSERT INTO indexers (name, kind, definition, url, settings, enabled, added_at)
     VALUES ($1, $2, $3, $4, $5, $6, $7)
     ON CONFLICT (name) DO NOTHING";

fn count(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

impl Store {
    /// Todas as seções gravadas, pelo nome.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn config_sections(&self) -> Result<Vec<(String, Value)>> {
        let client = self.pool.get().await?;
        let rows = client
            .query("SELECT name, value FROM config_sections ORDER BY name", &[])
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
            .collect()
    }

    /// Grava uma seção por cima da anterior.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn save_config_section(&self, name: &str, value: &Value, at: &str) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO config_sections (name, value, updated_at) VALUES ($1, $2, $3)
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value,
                     updated_at = excluded.updated_at",
                &[&name, value, &at],
            )
            .await?;
        Ok(())
    }

    /// Os indexadores cadastrados, por nome.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou settings fora do formato.
    pub async fn indexers(&self) -> Result<Vec<IndexerRecord>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT name, kind, definition, url, settings, enabled, added_at
                 FROM indexers ORDER BY name",
                &[],
            )
            .await?;
        rows.iter().map(read_indexer).collect()
    }

    /// Cadastra um indexador. `false` se o nome já existe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn insert_indexer(&self, record: &IndexerRecord) -> Result<bool> {
        let client = self.pool.get().await?;
        let inserted = client
            .execute(
                INSERT_INDEXER,
                &[
                    &record.name,
                    &record.kind,
                    &record.definition,
                    &record.url,
                    &settings_value(&record.settings),
                    &record.enabled,
                    &record.added_at,
                ],
            )
            .await?;
        Ok(inserted == 1)
    }

    /// Regrava um indexador existente. `false` se ele não existe.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn update_indexer(&self, record: &IndexerRecord) -> Result<bool> {
        let client = self.pool.get().await?;
        let updated = client
            .execute(
                "UPDATE indexers SET kind = $2, definition = $3, url = $4, settings = $5,
                     enabled = $6
                 WHERE name = $1",
                &[
                    &record.name,
                    &record.kind,
                    &record.definition,
                    &record.url,
                    &settings_value(&record.settings),
                    &record.enabled,
                ],
            )
            .await?;
        Ok(updated == 1)
    }

    /// Apaga o cadastro. `false` se ele não existia.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn delete_indexer(&self, name: &str) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute("DELETE FROM indexers WHERE name = $1", &[&name])
            .await?
            == 1)
    }

    /// Os strikes da limpeza, por chave.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn strikes(&self) -> Result<Vec<(String, u32)>> {
        let client = self.pool.get().await?;
        let rows = client
            .query("SELECT key, count FROM strikes ORDER BY key", &[])
            .await?;
        rows.iter()
            .map(|row| {
                let count: i32 = row.try_get(1)?;
                let count = u32::try_from(count)
                    .map_err(|_| StoreError::Corrupt("strike com contagem negativa".into()))?;
                Ok((row.try_get(0)?, count))
            })
            .collect()
    }

    /// Troca todos os strikes pelos dados, numa transação: o ciclo grava o
    /// ledger inteiro de uma vez, como gravava o arquivo, e uma queda no meio
    /// deixa o anterior intacto.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn replace_strikes(&self, strikes: &[(String, u32)]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute("DELETE FROM strikes", &[]).await?;
        for (key, value) in strikes.iter().filter(|(_, value)| *value > 0) {
            tx.execute(
                "INSERT INTO strikes (key, count) VALUES ($1, $2)",
                &[key, &count(*value)],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing::TestDb;

    fn record(name: &str) -> IndexerRecord {
        IndexerRecord {
            name: name.into(),
            kind: "torznab".into(),
            definition: None,
            url: Some("http://x/api".into()),
            settings: [("api_key".to_owned(), "segredo".to_owned())].into(),
            enabled: true,
            added_at: Some("2026-01-01T00:00:00Z".into()),
        }
    }

    #[tokio::test]
    async fn secoes_indexadores_e_strikes_ida_e_volta() {
        let Some(db) = TestDb::new("configuracao").await else {
            return;
        };
        let store = &db.store;
        assert!(store.config_sections().await.unwrap().is_empty());
        store
            .save_config_section("servidor", &json!({ "a": 1 }), "t1")
            .await
            .unwrap();
        store
            .save_config_section("servidor", &json!({ "a": 2 }), "t2")
            .await
            .unwrap();
        assert_eq!(
            store.config_sections().await.unwrap(),
            [("servidor".to_owned(), json!({ "a": 2 }))]
        );

        assert!(store.insert_indexer(&record("um")).await.unwrap());
        assert!(!store.insert_indexer(&record("um")).await.unwrap());
        let mut changed = record("um");
        changed.enabled = false;
        changed.settings.insert("api_key".into(), "outra".into());
        assert!(store.update_indexer(&changed).await.unwrap());
        assert_eq!(store.indexers().await.unwrap(), [changed]);
        assert!(!format!("{:?}", record("um")).contains("segredo"));
        assert!(store.delete_indexer("um").await.unwrap());
        assert!(!store.delete_indexer("um").await.unwrap());

        store
            .replace_strikes(&[("a".into(), 2), ("b".into(), 1)])
            .await
            .unwrap();
        store.replace_strikes(&[("b".into(), 3)]).await.unwrap();
        assert_eq!(store.strikes().await.unwrap(), [("b".to_owned(), 3)]);
        db.drop().await;
    }
}

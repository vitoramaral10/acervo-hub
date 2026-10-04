//! Persistência dos strikes entre ciclos, na tabela `strikes`.

use acervo_janitor::StrikeLedger;
use acervo_store::Store;
use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// Lê o ledger; sem strikes, um vazio.
///
/// Registro ilegível é erro, não recomeço silencioso: perder a contagem faz um
/// download sem dono antigo voltar à estaca zero, e ninguém perceberia pelo
/// log.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn load(store: &Store) -> Result<StrikeLedger> {
    let rows = store.strikes().await.context("lendo os strikes")?;
    // O ledger serializa como o mapa chave → contagem que o banco guarda.
    let map: Map<String, Value> = rows
        .into_iter()
        .map(|(key, count)| (key, Value::from(count)))
        .collect();
    serde_json::from_value(Value::Object(map)).context("interpretando os strikes")
}

/// Grava o ledger inteiro, numa transação: uma queda no meio deixa o
/// anterior intacto.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn save(store: &Store, ledger: &StrikeLedger) -> Result<()> {
    let Value::Object(map) = serde_json::to_value(ledger).context("serializando os strikes")?
    else {
        anyhow::bail!("strikes fora do formato de mapa");
    };
    let rows: Vec<(String, u32)> = map
        .into_iter()
        .map(|(key, count)| {
            let count = count
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .context("contagem de strike fora da faixa")?;
            Ok((key, count))
        })
        .collect::<Result<_>>()?;
    store
        .replace_strikes(&rows)
        .await
        .context("gravando os strikes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use acervo_core::DownloadHash;
    use acervo_janitor::StrikeKey;

    fn chave() -> StrikeKey {
        StrikeKey::for_download(&DownloadHash::new("aa"))
    }

    #[tokio::test]
    async fn strikes_sobrevivem_a_ida_e_volta_do_banco() {
        let Some(db) = acervo_store::testing::TestDb::new("strikes").await else {
            return;
        };
        assert!(load(&db.store).await.unwrap().is_empty());
        let mut antes = StrikeLedger::new();
        antes.strike(chave());
        antes.strike(chave());
        save(&db.store, &antes).await.unwrap();
        assert_eq!(load(&db.store).await.unwrap().count(&chave()), 2);

        // O ciclo seguinte esquece o que não viu.
        save(&db.store, &StrikeLedger::new()).await.unwrap();
        assert!(load(&db.store).await.unwrap().is_empty());
        db.drop().await;
    }
}

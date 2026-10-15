//! Pacotes de "Para lançar": o que o acervo prepara (checagem de regras,
//! texto, capturas e `.torrent`) para o usuário enviar à mão a um tracker.
//! Um pacote é o par (torrent de origem, indexador de destino).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_postgres::Row;

use crate::{Result, Store, StoreError};

/// Em que ponto está o pacote de um par.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchState {
    /// Um job em fundo está montando o pacote.
    Preparando,
    /// Alguma regra do destino reprova o release; não há pacote.
    Inelegivel,
    /// Pacote montado, esperando o envio do usuário.
    Pronto,
    /// Falha técnica ao montar; `error` diz qual.
    Falhou,
    /// O usuário disse que enviou ao site.
    Enviado,
    /// O cross-seed achou o publicado e está semeando.
    Semeando,
    /// O usuário desistiu do par.
    Descartado,
}

impl LaunchState {
    /// O texto gravado na coluna `state`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparando => "preparando",
            Self::Inelegivel => "inelegivel",
            Self::Pronto => "pronto",
            Self::Falhou => "falhou",
            Self::Enviado => "enviado",
            Self::Semeando => "semeando",
            Self::Descartado => "descartado",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "preparando" => Self::Preparando,
            "inelegivel" => Self::Inelegivel,
            "pronto" => Self::Pronto,
            "falhou" => Self::Falhou,
            "enviado" => Self::Enviado,
            "semeando" => Self::Semeando,
            "descartado" => Self::Descartado,
            other => {
                return Err(StoreError::Corrupt(format!(
                    "estado de lançamento desconhecido: {other}"
                )));
            }
        })
    }
}

/// O pacote de um par. Não carrega o `.torrent`: ele tem o announce com a
/// passkey e só sai por [`Store::launch_torrent`], para a rota de download.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Launch {
    /// Infohash do torrent de origem, em minúsculas.
    pub hash: String,
    /// Indexador de destino (`IndexerRecord.name`).
    pub indexer: String,
    pub state: LaunchState,
    /// Andamento legível enquanto `preparando`.
    pub progress: Option<String>,
    /// Checagens e releases do destino.
    pub report: Value,
    /// Texto gerado.
    pub fields: Value,
    /// `[{url, thumb}]`.
    pub screens: Value,
    /// Se o `.torrent` já foi gerado.
    pub has_torrent: bool,
    pub error: Option<String>,
    /// RFC 3339, em UTC.
    pub created_at: String,
    /// RFC 3339, em UTC.
    pub updated_at: String,
    /// RFC 3339, em UTC: quando o usuário marcou como enviado.
    pub sent_at: Option<String>,
}

/// Agora, em RFC 3339 UTC, como o resto do schema guarda datas.
const NOW: &str = "to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";

const COLUMNS: &str = "hash, indexer, state, progress, report, fields, screens, \
                       torrent IS NOT NULL, error, created_at, updated_at, sent_at";

fn launch_from(row: &Row) -> Result<Launch> {
    let state: String = row.try_get(2)?;
    Ok(Launch {
        hash: row.try_get(0)?,
        indexer: row.try_get(1)?,
        state: LaunchState::parse(&state)?,
        progress: row.try_get(3)?,
        report: row.try_get(4)?,
        fields: row.try_get(5)?,
        screens: row.try_get(6)?,
        has_torrent: row.try_get(7)?,
        error: row.try_get(8)?,
        created_at: row.try_get(9)?,
        updated_at: row.try_get(10)?,
        sent_at: row.try_get(11)?,
    })
}

impl Store {
    /// Todos os pacotes, do mais recente ao mais antigo.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn launches(&self) -> Result<Vec<Launch>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                &format!("SELECT {COLUMNS} FROM launches ORDER BY updated_at DESC, hash, indexer"),
                &[],
            )
            .await?;
        rows.iter().map(launch_from).collect()
    }

    /// Os pares em preparo com o andamento de cada um, sem relatório, texto
    /// nem capturas: é o que a tela pergunta a cada poucos segundos.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn launches_preparing(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT hash, indexer, progress FROM launches
                 WHERE state = 'preparando' ORDER BY updated_at DESC, hash, indexer",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect()
    }

    /// O pacote de um par, se existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn launch(&self, hash: &str, indexer: &str) -> Result<Option<Launch>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                &format!("SELECT {COLUMNS} FROM launches WHERE hash = $1 AND indexer = $2"),
                &[&hash.to_ascii_lowercase(), &indexer],
            )
            .await?;
        row.as_ref().map(launch_from).transpose()
    }

    /// Cria o pacote do par ou troca estado, andamento e erro do que existe;
    /// relatório, texto, capturas e `.torrent` ficam como estavam.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn upsert_launch_state(
        &self,
        hash: &str,
        indexer: &str,
        state: LaunchState,
        progress: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                &format!(
                    "INSERT INTO launches (hash, indexer, state, progress, error, created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, {NOW}, {NOW})
                     ON CONFLICT (hash, indexer) DO UPDATE
                     SET state = EXCLUDED.state,
                         progress = EXCLUDED.progress,
                         error = EXCLUDED.error,
                         updated_at = EXCLUDED.updated_at"
                ),
                &[
                    &hash.to_ascii_lowercase(),
                    &indexer,
                    &state.as_str(),
                    &progress,
                    &error,
                ],
            )
            .await?;
        Ok(())
    }

    /// Só troca o estado, sem mexer no andamento nem no erro (descartar,
    /// semeando). Devolve se o par existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn set_launch_state(
        &self,
        hash: &str,
        indexer: &str,
        state: LaunchState,
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        let updated = client
            .execute(
                &format!(
                    "UPDATE launches SET state = $3, updated_at = {NOW}
                     WHERE hash = $1 AND indexer = $2"
                ),
                &[&hash.to_ascii_lowercase(), &indexer, &state.as_str()],
            )
            .await?;
        Ok(updated > 0)
    }

    /// Passa a `falhou` o par que ainda está `preparando`, com o motivo; se o
    /// estado já mudou (o job terminou entre a leitura e esta chamada), não
    /// toca em nada. Devolve se trocou.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn fail_launch_if_preparing(
        &self,
        hash: &str,
        indexer: &str,
        error: &str,
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        let updated = client
            .execute(
                &format!(
                    "UPDATE launches
                     SET state = 'falhou', progress = NULL, error = $3, updated_at = {NOW}
                     WHERE hash = $1 AND indexer = $2 AND state = 'preparando'"
                ),
                &[&hash.to_ascii_lowercase(), &indexer, &error],
            )
            .await?;
        Ok(updated > 0)
    }

    /// Grava o relatório de checagens. Devolve se o par existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn save_launch_report(
        &self,
        hash: &str,
        indexer: &str,
        report: &Value,
    ) -> Result<bool> {
        self.save_launch_json("report", hash, indexer, report).await
    }

    /// Grava o texto gerado. Devolve se o par existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn save_launch_fields(
        &self,
        hash: &str,
        indexer: &str,
        fields: &Value,
    ) -> Result<bool> {
        self.save_launch_json("fields", hash, indexer, fields).await
    }

    /// Grava as capturas (`[{url, thumb}]`). Devolve se o par existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn save_launch_screens(
        &self,
        hash: &str,
        indexer: &str,
        screens: &Value,
    ) -> Result<bool> {
        self.save_launch_json("screens", hash, indexer, screens)
            .await
    }

    /// `column` vem só das funções acima, nunca de fora.
    async fn save_launch_json(
        &self,
        column: &str,
        hash: &str,
        indexer: &str,
        value: &Value,
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        let updated = client
            .execute(
                &format!(
                    "UPDATE launches SET {column} = $3, updated_at = {NOW}
                     WHERE hash = $1 AND indexer = $2"
                ),
                &[&hash.to_ascii_lowercase(), &indexer, value],
            )
            .await?;
        Ok(updated > 0)
    }

    /// Grava o `.torrent` gerado. Devolve se o par existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn save_launch_torrent(
        &self,
        hash: &str,
        indexer: &str,
        bytes: &[u8],
    ) -> Result<bool> {
        let client = self.pool.get().await?;
        let updated = client
            .execute(
                &format!(
                    "UPDATE launches SET torrent = $3, updated_at = {NOW}
                     WHERE hash = $1 AND indexer = $2"
                ),
                &[&hash.to_ascii_lowercase(), &indexer, &bytes],
            )
            .await?;
        Ok(updated > 0)
    }

    /// O `.torrent` gerado do par: tem o announce com a passkey, só a rota
    /// de download o lê.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn launch_torrent(&self, hash: &str, indexer: &str) -> Result<Option<Vec<u8>>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT torrent FROM launches WHERE hash = $1 AND indexer = $2",
                &[&hash.to_ascii_lowercase(), &indexer],
            )
            .await?;
        Ok(row
            .map(|row| row.try_get::<_, Option<Vec<u8>>>(0))
            .transpose()?
            .flatten())
    }

    /// Marca como enviado ao site, em `at` (RFC 3339, UTC). Devolve se o par
    /// existe.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    pub async fn mark_launch_sent(&self, hash: &str, indexer: &str, at: &str) -> Result<bool> {
        let client = self.pool.get().await?;
        let updated = client
            .execute(
                &format!(
                    "UPDATE launches SET state = 'enviado', sent_at = $3, updated_at = {NOW}
                     WHERE hash = $1 AND indexer = $2"
                ),
                &[&hash.to_ascii_lowercase(), &indexer, &at],
            )
            .await?;
        Ok(updated > 0)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing::TestDb;

    #[test]
    fn estado_vai_e_volta_em_minusculas() {
        for state in [
            LaunchState::Preparando,
            LaunchState::Inelegivel,
            LaunchState::Pronto,
            LaunchState::Falhou,
            LaunchState::Enviado,
            LaunchState::Semeando,
            LaunchState::Descartado,
        ] {
            let text = serde_json::to_value(state).unwrap();
            assert_eq!(text, json!(state.as_str()));
            assert_eq!(LaunchState::parse(state.as_str()).unwrap(), state);
        }
        assert!(LaunchState::parse("Pronto").is_err());
    }

    #[tokio::test]
    async fn so_o_pacote_preparando_vira_falhou_pelo_resgate() {
        let Some(db) = TestDb::new("launch_resgate").await else {
            return;
        };
        let store = &db.store;
        store
            .upsert_launch_state("abcd", "bj", LaunchState::Preparando, Some("na fila"), None)
            .await
            .unwrap();
        store
            .upsert_launch_state("ef01", "bj", LaunchState::Pronto, None, None)
            .await
            .unwrap();
        assert!(
            store
                .fail_launch_if_preparing("ABCD", "bj", "parou")
                .await
                .unwrap()
        );
        let launch = store.launch("abcd", "bj").await.unwrap().unwrap();
        assert_eq!(launch.state, LaunchState::Falhou);
        assert_eq!(launch.progress, None);
        assert_eq!(launch.error.as_deref(), Some("parou"));
        // Quem já terminou não é pisado; par que não existe também não.
        assert!(
            !store
                .fail_launch_if_preparing("ef01", "bj", "parou")
                .await
                .unwrap()
        );
        assert!(
            !store
                .fail_launch_if_preparing("0000", "bj", "parou")
                .await
                .unwrap()
        );
        assert_eq!(
            store.launch("ef01", "bj").await.unwrap().unwrap().state,
            LaunchState::Pronto
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn pacote_nasce_preparando_e_o_estado_troca_sem_perder_o_resto() {
        let Some(db) = TestDb::new("launch_estado").await else {
            return;
        };
        let store = &db.store;
        assert!(
            store
                .launch("abcd", "bjsharecustom")
                .await
                .unwrap()
                .is_none()
        );
        store
            .upsert_launch_state(
                "ABCD",
                "bjsharecustom",
                LaunchState::Preparando,
                Some("na fila"),
                None,
            )
            .await
            .unwrap();
        let launch = store
            .launch("abcd", "bjsharecustom")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(launch.hash, "abcd");
        assert_eq!(launch.state, LaunchState::Preparando);
        assert_eq!(launch.progress.as_deref(), Some("na fila"));
        assert_eq!(launch.report, json!({}));
        assert_eq!(launch.fields, json!({}));
        assert_eq!(launch.screens, json!([]));
        assert!(!launch.has_torrent);
        assert_eq!(launch.sent_at, None);
        assert!(launch.created_at.ends_with('Z'));

        store
            .save_launch_report("abcd", "bjsharecustom", &json!({ "checks": [] }))
            .await
            .unwrap();
        store
            .upsert_launch_state(
                "abcd",
                "bjsharecustom",
                LaunchState::Falhou,
                None,
                Some("sem chave"),
            )
            .await
            .unwrap();
        let launch = store
            .launch("ABCD", "bjsharecustom")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(launch.state, LaunchState::Falhou);
        assert_eq!(launch.progress, None);
        assert_eq!(launch.error.as_deref(), Some("sem chave"));
        assert_eq!(launch.report, json!({ "checks": [] }));
        assert_eq!(store.launches().await.unwrap().len(), 1);
        db.drop().await;
    }

    #[tokio::test]
    async fn relatorio_texto_e_capturas_ficam_no_pacote() {
        let Some(db) = TestDb::new("launch_dados").await else {
            return;
        };
        let store = &db.store;
        // Sem o par, nada é gravado.
        assert!(
            !store
                .save_launch_report("abcd", "amigosshare", &json!({}))
                .await
                .unwrap()
        );
        assert!(
            !store
                .save_launch_fields("abcd", "amigosshare", &json!({}))
                .await
                .unwrap()
        );
        assert!(
            !store
                .save_launch_screens("abcd", "amigosshare", &json!([]))
                .await
                .unwrap()
        );
        assert!(
            !store
                .save_launch_torrent("abcd", "amigosshare", b"x")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_launch_sent("abcd", "amigosshare", "t")
                .await
                .unwrap()
        );
        assert!(
            !store
                .set_launch_state("abcd", "amigosshare", LaunchState::Descartado)
                .await
                .unwrap()
        );

        store
            .upsert_launch_state("abcd", "amigosshare", LaunchState::Preparando, None, None)
            .await
            .unwrap();
        let report = json!({ "checks": [{ "rule": "3.2", "status": "ok" }] });
        let fields = json!({ "titulo": "Filme (Movie)" });
        let screens = json!([{ "url": "https://i/1.png", "thumb": "https://i/1t.png" }]);
        assert!(
            store
                .save_launch_report("abcd", "amigosshare", &report)
                .await
                .unwrap()
        );
        assert!(
            store
                .save_launch_fields("abcd", "amigosshare", &fields)
                .await
                .unwrap()
        );
        assert!(
            store
                .save_launch_screens("abcd", "amigosshare", &screens)
                .await
                .unwrap()
        );
        let launch = store.launch("abcd", "amigosshare").await.unwrap().unwrap();
        assert_eq!(launch.report, report);
        assert_eq!(launch.fields, fields);
        assert_eq!(launch.screens, screens);
        assert_eq!(launch.state, LaunchState::Preparando);
        db.drop().await;
    }

    #[tokio::test]
    async fn torrent_so_sai_pela_funcao_propria_e_o_debug_nao_o_mostra() {
        let Some(db) = TestDb::new("launch_torrent").await else {
            return;
        };
        let store = &db.store;
        store
            .upsert_launch_state("abcd", "amigosshare", LaunchState::Preparando, None, None)
            .await
            .unwrap();
        assert_eq!(
            store.launch_torrent("abcd", "amigosshare").await.unwrap(),
            None
        );
        let bytes = b"d8:announce30:https://tracker/PASSKEYSECRETOe";
        assert!(
            store
                .save_launch_torrent("ABCD", "amigosshare", bytes)
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .launch_torrent("abcd", "amigosshare")
                .await
                .unwrap()
                .as_deref(),
            Some(bytes.as_slice())
        );
        let launch = store.launch("abcd", "amigosshare").await.unwrap().unwrap();
        assert!(launch.has_torrent);
        let shown = format!("{launch:?}");
        assert!(!shown.contains("PASSKEY"));
        assert!(!serde_json::to_string(&launch).unwrap().contains("PASSKEY"));
        assert_eq!(
            store.launch_torrent("outro", "amigosshare").await.unwrap(),
            None
        );
        db.drop().await;
    }

    #[tokio::test]
    async fn envio_e_descarte_trocam_o_estado() {
        let Some(db) = TestDb::new("launch_envio").await else {
            return;
        };
        let store = &db.store;
        store
            .upsert_launch_state("abcd", "bjsharecustom", LaunchState::Pronto, None, None)
            .await
            .unwrap();
        store
            .upsert_launch_state("ef01", "bjsharecustom", LaunchState::Pronto, None, None)
            .await
            .unwrap();
        assert!(
            store
                .mark_launch_sent("ABCD", "bjsharecustom", "2026-10-14T12:00:00Z")
                .await
                .unwrap()
        );
        let sent = store
            .launch("abcd", "bjsharecustom")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sent.state, LaunchState::Enviado);
        assert_eq!(sent.sent_at.as_deref(), Some("2026-10-14T12:00:00Z"));
        assert!(
            store
                .set_launch_state("abcd", "bjsharecustom", LaunchState::Semeando)
                .await
                .unwrap()
        );
        assert!(
            store
                .set_launch_state("ef01", "bjsharecustom", LaunchState::Descartado)
                .await
                .unwrap()
        );
        let all = store.launches().await.unwrap();
        let state = |hash: &str| all.iter().find(|l| l.hash == hash).unwrap().state;
        assert_eq!(state("abcd"), LaunchState::Semeando);
        assert_eq!(state("ef01"), LaunchState::Descartado);
        // O envio marcado fica, mesmo depois de semeando.
        assert_eq!(
            all.iter()
                .find(|l| l.hash == "abcd")
                .unwrap()
                .sent_at
                .as_deref(),
            Some("2026-10-14T12:00:00Z")
        );
        // O banco recusa estado fora da lista.
        let client = store.pool.get().await.unwrap();
        assert!(
            client
                .execute("UPDATE launches SET state = 'qualquer'", &[])
                .await
                .is_err()
        );
        drop(client);
        db.drop().await;
    }
}

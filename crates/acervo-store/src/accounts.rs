//! Contas da interface: usuário com senha em argon2id e sessões com prazo.
//!
//! A sessão é um token aleatório que só o navegador guarda; o banco fica com
//! o SHA-256 dele, então quem lê o banco não consegue se passar por ninguém.

use std::fmt::Write as _;
use std::sync::OnceLock;

use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use sha2::{Digest, Sha256};

use crate::{Result, Store, StoreError};

/// Quanto uma sessão dura depois da entrada.
pub const SESSION_DAYS: i32 = 30;

/// Senha mais curta que isto é recusada.
const MIN_PASSWORD: usize = 8;

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| StoreError::Password(e.to_string()))
}

fn password_matches(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Hash de uma senha que ninguém tem: usuário inexistente custa o mesmo
/// tempo que senha errada, e o tempo de resposta não revela quem existe.
fn decoy() -> &'static str {
    static DECOY: OnceLock<String> = OnceLock::new();
    DECOY.get_or_init(|| hash_password("usuario-inexistente").unwrap_or_default())
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn new_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

impl Store {
    /// Cria o usuário ou troca a senha dele. Trocar a senha encerra as
    /// sessões abertas.
    ///
    /// # Errors
    ///
    /// Nome vazio, senha curta ou falha de escrita.
    pub async fn set_password(&self, name: &str, password: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Password("nome de usuário vazio".into()));
        }
        if password.chars().count() < MIN_PASSWORD {
            return Err(StoreError::Password(format!(
                "a senha precisa de pelo menos {MIN_PASSWORD} caracteres"
            )));
        }
        let owned = password.to_owned();
        let hash = tokio::task::spawn_blocking(move || hash_password(&owned))
            .await
            .map_err(|e| StoreError::Password(e.to_string()))??;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "INSERT INTO users (name, password_hash) VALUES ($1, $2)
             ON CONFLICT (name) DO UPDATE SET password_hash = excluded.password_hash",
            &[&name, &hash],
        )
        .await?;
        tx.execute("DELETE FROM sessions WHERE user_name = $1", &[&name])
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove o usuário e as sessões dele. `false` se não existia.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn remove_user(&self, name: &str) -> Result<bool> {
        let client = self.pool.get().await?;
        Ok(client
            .execute("DELETE FROM users WHERE name = $1", &[&name])
            .await?
            > 0)
    }

    /// Nomes dos usuários, em ordem.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn users(&self) -> Result<Vec<String>> {
        let client = self.pool.get().await?;
        client
            .query("SELECT name FROM users ORDER BY name", &[])
            .await?
            .iter()
            .map(|row| Ok(row.try_get(0)?))
            .collect()
    }

    /// Confere usuário e senha e, se baterem, abre uma sessão. Devolve o
    /// token, que vai para o cookie.
    ///
    /// # Errors
    ///
    /// Falha de leitura ou escrita — senha errada não é erro, é `None`.
    pub async fn login(&self, name: &str, password: &str) -> Result<Option<String>> {
        let client = self.pool.get().await?;
        let stored: Option<String> = client
            .query_opt(
                "SELECT password_hash FROM users WHERE name = $1",
                &[&name.trim()],
            )
            .await?
            .map(|row| row.try_get(0))
            .transpose()?;
        let known = stored.is_some();
        let hash = stored.unwrap_or_else(|| decoy().to_owned());
        let owned = password.to_owned();
        let matches = tokio::task::spawn_blocking(move || password_matches(&owned, &hash))
            .await
            .map_err(|e| StoreError::Password(e.to_string()))?;
        if !(known && matches) {
            return Ok(None);
        }
        client
            .execute("DELETE FROM sessions WHERE expires_at < now()", &[])
            .await?;
        let token = new_token();
        client
            .execute(
                "INSERT INTO sessions (token_hash, user_name, expires_at)
                 VALUES ($1, $2, now() + make_interval(days => $3))",
                &[&token_hash(&token), &name.trim(), &SESSION_DAYS],
            )
            .await?;
        Ok(Some(token))
    }

    /// Dono da sessão, se ela existe e não venceu.
    ///
    /// # Errors
    ///
    /// Falha de leitura.
    pub async fn session_user(&self, token: &str) -> Result<Option<String>> {
        let client = self.pool.get().await?;
        client
            .query_opt(
                "SELECT user_name FROM sessions WHERE token_hash = $1 AND expires_at > now()",
                &[&token_hash(token)],
            )
            .await?
            .map(|row| row.try_get(0))
            .transpose()
            .map_err(StoreError::from)
    }

    /// Encerra a sessão. Token desconhecido não é erro.
    ///
    /// # Errors
    ///
    /// Falha de escrita.
    pub async fn logout(&self, token: &str) -> Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "DELETE FROM sessions WHERE token_hash = $1",
                &[&token_hash(token)],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::TestDb;

    #[tokio::test]
    async fn entrada_confere_senha_e_sessao_morre_ao_sair() {
        let Some(db) = TestDb::new("contas").await else {
            return;
        };
        let store = &db.store;
        store.set_password("ana", "senha-longa").await.unwrap();
        assert_eq!(store.users().await.unwrap(), ["ana"]);

        assert!(
            store
                .login("ana", "errada-mas-longa")
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.login("bia", "senha-longa").await.unwrap().is_none());
        let token = store.login("ana", "senha-longa").await.unwrap().unwrap();
        assert_eq!(
            store.session_user(&token).await.unwrap().as_deref(),
            Some("ana")
        );
        assert!(store.session_user("outro").await.unwrap().is_none());

        store.logout(&token).await.unwrap();
        assert!(store.session_user(&token).await.unwrap().is_none());
        db.drop().await;
    }

    #[tokio::test]
    async fn trocar_a_senha_derruba_as_sessoes() {
        let Some(db) = TestDb::new("trocasenha").await else {
            return;
        };
        let store = &db.store;
        store.set_password("ana", "senha-longa").await.unwrap();
        let token = store.login("ana", "senha-longa").await.unwrap().unwrap();
        store.set_password("ana", "outra-senha").await.unwrap();
        assert!(store.session_user(&token).await.unwrap().is_none());
        assert!(store.login("ana", "senha-longa").await.unwrap().is_none());
        assert!(store.login("ana", "outra-senha").await.unwrap().is_some());
        assert!(store.set_password("ana", "curta").await.is_err());
        assert!(store.remove_user("ana").await.unwrap());
        assert!(store.users().await.unwrap().is_empty());
        db.drop().await;
    }
}

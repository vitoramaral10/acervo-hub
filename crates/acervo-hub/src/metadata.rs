//! Fonte de metadados de filmes: o TMDB, com a chave guardada pela tela.

use acervo_metadata::Tmdb;
use acervo_store::Store;
use anyhow::{Context, Result};

/// Onde a chave do TMDB fica na tabela de configurações.
pub const TMDB_KEY: &str = "tmdb.api_key";

/// Língua do título localizado — a mesma que a biblioteca mostra.
pub const LANGUAGE: &str = "pt-BR";

/// O cliente do TMDB, se a chave já foi configurada.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn tmdb(store: &Store) -> Result<Option<Tmdb>> {
    let Some(key) = store.setting(TMDB_KEY).await? else {
        return Ok(None);
    };
    Ok(Some(Tmdb::new(
        &key,
        LANGUAGE,
        crate::config::HTTP_TIMEOUT,
    )?))
}

/// Como [`tmdb`], mas sem chave é erro.
///
/// # Errors
///
/// Sem chave configurada ou banco inalcançável.
pub async fn require_tmdb(store: &Store) -> Result<Tmdb> {
    tmdb(store)
        .await?
        .context("chave do TMDB não configurada: defina em Configurações, na interface")
}

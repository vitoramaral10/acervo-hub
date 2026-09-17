//! Persistência dos strikes entre execuções.

use std::path::Path;

use acervo_janitor::StrikeLedger;
use anyhow::{Context, Result};

/// Lê o ledger, ou devolve um vazio se o arquivo ainda não existe.
///
/// Arquivo corrompido é erro, não recomeço silencioso: perder a contagem faz um
/// órfão antigo voltar à estaca zero, e ninguém perceberia pelo log.
///
/// # Errors
///
/// Falha de leitura ou JSON inválido.
pub fn load(path: &Path) -> Result<StrikeLedger> {
    if !path.exists() {
        return Ok(StrikeLedger::new());
    }

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("lendo os strikes em `{}`", path.display()))?;

    serde_json::from_str(&text)
        .with_context(|| format!("interpretando os strikes em `{}`", path.display()))
}

/// Grava o ledger de forma atômica.
///
/// Escreve num temporário e renomeia: `rename` no mesmo filesystem é atômico,
/// então uma interrupção no meio da gravação deixa o arquivo anterior intacto
/// em vez de um JSON truncado que a próxima execução recusaria.
///
/// # Errors
///
/// Falha ao criar o diretório, escrever ou renomear.
pub fn save(path: &Path, ledger: &StrikeLedger) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando `{}`", parent.display()))?;
    }

    let text = serde_json::to_string_pretty(ledger).context("serializando os strikes")?;
    let temp = path.with_extension("json.tmp");

    std::fs::write(&temp, text).with_context(|| format!("escrevendo `{}`", temp.display()))?;
    std::fs::rename(&temp, path)
        .with_context(|| format!("renomeando para `{}`", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use acervo_core::{DownloadHash, InstanceName, QueueItem, QueueItemId};
    use acervo_janitor::StrikeKey;

    fn chave() -> StrikeKey {
        StrikeKey::for_item(&QueueItem {
            id: QueueItemId(1),
            instance: InstanceName::new("filmes"),
            title: "exemplo".into(),
            download: Some(DownloadHash::new("aa")),
            work: None,
        })
    }

    #[test]
    fn strikes_sobrevivem_a_ida_e_volta_do_disco() {
        let dir = std::env::temp_dir().join(format!("acervo-hub-teste-{}", std::process::id()));
        let path = dir.join("strikes.json");

        let mut antes = StrikeLedger::new();
        antes.strike(chave());
        antes.strike(chave());
        save(&path, &antes).unwrap();

        let depois = load(&path).unwrap();
        assert_eq!(depois.count(&chave()), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn arquivo_ausente_comeca_vazio() {
        let ausente = std::env::temp_dir().join("acervo-hub-nao-existe/strikes.json");
        assert!(load(&ausente).unwrap().is_empty());
    }
}

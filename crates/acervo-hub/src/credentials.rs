//! Credenciais trocadas pela interface web.
//!
//! O `config.toml` segue somente leitura — em container ele é montado assim —
//! e estas credenciais valem por cima dele, indexador por indexador. Arquivo
//! próprio, gravado de forma atômica e com permissão 600: é segredo.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

/// Por indexador, por setting.
pub type Overrides = BTreeMap<String, BTreeMap<String, String>>;

/// Lê o arquivo; ausente é vazio, não erro — nada foi trocado ainda.
///
/// # Errors
///
/// Arquivo existente e ilegível ou fora do formato.
pub fn load(path: &Path) -> Result<Overrides> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("interpretando as credenciais em `{}`", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Overrides::new()),
        Err(error) => {
            Err(error).with_context(|| format!("lendo as credenciais em `{}`", path.display()))
        }
    }
}

/// Grava por cima com arquivo temporário + `rename`: uma queda no meio não
/// deixa o arquivo pela metade, e o temporário já nasce com 600.
///
/// # Errors
///
/// Diretório inexistente e não criável, ou falha de escrita.
pub fn save(path: &Path, overrides: &Overrides) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("criando o diretório de estado `{}`", dir.display()))?;
    let text = toml::to_string(overrides).context("serializando as credenciais")?;
    write_atomic(path, &text)
}

/// Grava texto com arquivo temporário + `rename`, permissão 600.
///
/// # Errors
///
/// Diretório inexistente e não criável, ou falha de escrita.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("criando o diretório de estado `{}`", dir.display()))?;
    let temporary = path.with_extension("tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("gravando `{}`", temporary.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("substituindo `{}`", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grava_com_600_e_le_de_volta() {
        let dir = std::env::temp_dir().join(format!("acervo-credenciais-{}", std::process::id()));
        let path = dir.join("credenciais.toml");
        assert!(load(&path).unwrap().is_empty());

        let mut overrides = Overrides::new();
        overrides
            .entry("privado".into())
            .or_default()
            .insert("cookie".into(), "a=1; b=\"2\"".into());
        save(&path, &overrides).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load(&path).unwrap(), overrides);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

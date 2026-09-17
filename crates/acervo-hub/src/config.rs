//! Configuração em TOML.
//!
//! Todas as structs usam `deny_unknown_fields`. Um campo escrito errado tem de
//! ser erro barulhento: silenciosamente ignorado, ele faria uma trava de
//! segurança cair no padrão sem ninguém notar — que é o pior dos dois mundos,
//! porque a configuração *parece* estar valendo.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use acervo_arr::ArrKind;
use acervo_core::Allocated;
use acervo_janitor::{Guards, Mode, Policy};
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub qbittorrent: QbitConfig,
    pub instances: Vec<InstanceConfig>,
    /// Caminho como o cliente vê → caminho no host.
    #[serde(default)]
    pub paths: BTreeMap<String, String>,
    #[serde(default)]
    pub library: LibraryConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub state: StateConfig,
    #[serde(default = "default_timeout_seconds")]
    pub http_timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QbitConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceConfig {
    pub name: String,
    pub kind: InstanceKind,
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceKind {
    Series,
    Movie,
}

impl From<InstanceKind> for ArrKind {
    fn from(kind: InstanceKind) -> Self {
        match kind {
            InstanceKind::Series => Self::Series,
            InstanceKind::Movie => Self::Movie,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryConfig {
    /// Raízes do acervo, no host. Servem para medir o tamanho total, que é a
    /// base da trava proporcional.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateConfig {
    /// Onde os strikes sobrevivem entre execuções.
    #[serde(default = "default_ledger_path")]
    pub ledger: PathBuf,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            ledger: default_ledger_path(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default = "default_strikes")]
    pub orphan_strikes: u32,
    #[serde(default)]
    pub delete_private_orphans: bool,
    #[serde(default = "default_true")]
    pub skip_orphan_if_missing_in_client: bool,
    /// `None` (omitido ou `0`) apaga sem carência de seed.
    #[serde(default = "default_private_grace")]
    pub private_seed_grace_hours: u64,
    #[serde(default = "default_recent_grace")]
    pub recent_change_grace_hours: u64,
    #[serde(default = "default_max_batch_gib")]
    pub max_batch_gib: u64,
    #[serde(default = "default_max_fraction")]
    pub max_batch_fraction: f64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            orphan_strikes: default_strikes(),
            delete_private_orphans: false,
            skip_orphan_if_missing_in_client: true,
            private_seed_grace_hours: default_private_grace(),
            recent_change_grace_hours: default_recent_grace(),
            max_batch_gib: default_max_batch_gib(),
            max_batch_fraction: default_max_fraction(),
        }
    }
}

impl PolicyConfig {
    #[must_use]
    pub fn to_policy(&self, mode: Mode) -> Policy {
        Policy {
            mode,
            orphan_strikes: self.orphan_strikes,
            delete_private_orphans: self.delete_private_orphans,
            skip_orphan_if_missing_in_client: self.skip_orphan_if_missing_in_client,
            private_seed_grace: (self.private_seed_grace_hours > 0)
                .then(|| Duration::from_secs(self.private_seed_grace_hours * 3600)),
            guards: Guards {
                recent_change_grace: Duration::from_secs(self.recent_change_grace_hours * 3600),
                max_batch: Allocated::from_bytes(self.max_batch_gib * 1024 * 1024 * 1024),
                max_batch_fraction: self.max_batch_fraction,
            },
        }
    }
}

impl Config {
    /// Lê e valida o arquivo de configuração.
    ///
    /// # Errors
    ///
    /// Arquivo ausente, TOML inválido, campo desconhecido ou configuração
    /// incoerente.
    pub fn load(path: &Path) -> Result<Self> {
        warn_if_world_readable(path);

        let text = std::fs::read_to_string(path)
            .with_context(|| format!("lendo a configuração em `{}`", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("interpretando a configuração em `{}`", path.display()))?;

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.instances.is_empty(),
            "nenhuma instância configurada: sem fila para cruzar, todo download \
             pareceria fora de fila"
        );
        anyhow::ensure!(
            !self.library.roots.is_empty(),
            "`library.roots` está vazio: sem medir a biblioteca, a trava \
             proporcional não tem denominador"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.policy.max_batch_fraction),
            "`policy.max_batch_fraction` precisa ficar entre 0.0 e 1.0"
        );

        Ok(())
    }

    #[must_use]
    pub fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.http_timeout_seconds)
    }

    #[must_use]
    pub fn path_map(&self) -> acervo_fs::PathMap {
        acervo_fs::PathMap::new(
            self.paths
                .iter()
                .map(|(from, to)| (PathBuf::from(from), PathBuf::from(to))),
        )
    }
}

/// O arquivo guarda chave de API e senha em texto puro.
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;

    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{mode:o}"),
            "configuração legível por outros usuários e contém segredo; use chmod 600"
        );
    }
}

/// Expande `~` no início do caminho.
#[must_use]
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    std::env::var_os("HOME")
        .map_or_else(|| path.to_path_buf(), |home| PathBuf::from(home).join(rest))
}

const fn default_timeout_seconds() -> u64 {
    30
}
const fn default_strikes() -> u32 {
    3
}
const fn default_true() -> bool {
    true
}
const fn default_private_grace() -> u64 {
    120
}
const fn default_recent_grace() -> u64 {
    24
}
const fn default_max_batch_gib() -> u64 {
    300
}
const fn default_max_fraction() -> f64 {
    0.30
}
fn default_ledger_path() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/strikes.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMA: &str = r#"
        [qbittorrent]
        url = "http://localhost:8080"
        username = "u"
        password = "p"

        [library]
        roots = ["/acervo"]

        [[instances]]
        name = "filmes"
        kind = "movie"
        url = "http://localhost:7878"
        api_key = "k"
    "#;

    fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn configuracao_minima_assume_padroes_conservadores() {
        let c = parse(MINIMA).unwrap();
        let p = c.policy.to_policy(Mode::DryRun);

        assert_eq!(p.orphan_strikes, 3);
        assert!(!p.delete_private_orphans);
        assert!(p.private_seed_grace.is_some());
        assert!((p.guards.max_batch_fraction - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn campo_escrito_errado_e_erro_e_nao_silencio() {
        let torto = MINIMA.to_string() + "\n[policy]\norphan_strikez = 99\n";
        let erro = parse(&torto).unwrap_err().to_string();
        assert!(erro.contains("orphan_strikez"), "erro veio: {erro}");
    }

    #[test]
    fn sem_raiz_de_biblioteca_a_configuracao_e_recusada() {
        let sem_raiz = MINIMA.replace(r#"roots = ["/acervo"]"#, "roots = []");
        assert!(parse(&sem_raiz).is_err());
    }

    #[test]
    fn sem_instancia_a_configuracao_e_recusada() {
        let sem_instancia = MINIMA.split("[[instances]]").next().unwrap().to_string();
        assert!(parse(&sem_instancia).is_err());
    }

    #[test]
    fn carencia_zero_desliga_a_carencia() {
        let texto = MINIMA.to_string() + "\n[policy]\nprivate_seed_grace_hours = 0\n";
        let p = parse(&texto).unwrap().policy.to_policy(Mode::Apply);
        assert!(p.private_seed_grace.is_none());
    }

    #[test]
    fn fracao_fora_da_faixa_e_recusada() {
        let texto = MINIMA.to_string() + "\n[policy]\nmax_batch_fraction = 1.5\n";
        assert!(parse(&texto).is_err());
    }
}

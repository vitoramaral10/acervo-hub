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
    /// Só o ciclo de limpeza usa; `serve` sobe sem ele.
    #[serde(default)]
    pub qbittorrent: Option<QbitConfig>,
    #[serde(default)]
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
    /// Superfície Torznab. Só `serve` usa.
    #[serde(default)]
    pub server: Option<ServerConfig>,
    #[serde(default)]
    pub indexers: Vec<IndexerConfig>,
    /// Banco do catálogo de filmes e das contas da interface.
    #[serde(default)]
    pub database: Option<DatabaseConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// `postgres://usuário:senha@host:5432/banco`.
    pub url: String,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // O endereço carrega a senha do banco.
        formatter
            .debug_struct("DatabaseConfig")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Chave que os consumidores mandam em `apikey`. Vale para todos os
    /// indexadores servidos.
    pub api_key: String,
    /// Como os gerenciadores alcançam este serviço — o endereço que `sync`
    /// cadastra neles. Em Compose, o nome do serviço na rede interna.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Diretórios de definições Cardigann que a interface oferece para
    /// adicionar — por exemplo, o `Definitions` do agregador atual. O
    /// primeiro que tiver um id vence.
    #[serde(default)]
    pub catalogs: Vec<PathBuf>,
    /// De quantos em quantos minutos o serviço reimporta o catálogo de filmes
    /// e roda a decisão em sombra. Sem valor, não roda sozinho. Roda aqui, e
    /// não num processo à parte, para dividir sessão e consultas guardadas com
    /// o que os gerenciadores pedem.
    #[serde(default)]
    pub shadow_interval_minutes: Option<u64>,
    /// Quantos filmes cada rodada de sombra busca.
    #[serde(default = "default_shadow_limit")]
    pub shadow_limit: usize,
}

fn default_shadow_limit() -> usize {
    5
}

/// Um indexador servido. `kind` decide de onde vêm as capacidades: a
/// definição Cardigann as declara; um endpoint Torznab as anuncia em `t=caps`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IndexerConfig {
    Torznab(TorznabIndexer),
    Cardigann(CardigannIndexer),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TorznabIndexer {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Intervalo mínimo entre duas requisições ao mesmo indexador.
    #[serde(default = "default_request_interval")]
    pub request_interval_seconds: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardigannIndexer {
    /// Arquivo YAML da definição, no formato v11.
    pub definition: PathBuf,
    /// Qual dos `links` da definição usar.
    #[serde(default)]
    pub link: usize,
    /// Sobrescreve os `settings` declarados na definição.
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
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
    /// Credenciais trocadas pela interface web. Ficam fora do `config.toml`
    /// para que ele possa ser montado somente leitura; valem por cima dele.
    #[serde(default = "default_credentials_path")]
    pub credentials: PathBuf,
    /// Indexadores adicionados e desativados pela interface web.
    #[serde(default = "default_registry_path")]
    pub registry: PathBuf,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            ledger: default_ledger_path(),
            credentials: default_credentials_path(),
            registry: default_registry_path(),
        }
    }
}

impl StateConfig {
    /// Resultado do último ciclo de limpeza, ao lado do ledger de strikes.
    #[must_use]
    pub fn last_cycle(&self) -> PathBuf {
        expand_tilde(&self.ledger).with_file_name("ultimo-ciclo.json")
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
    /// Categorias do cliente que os *arr usam. Só seed nelas pode ser apagado
    /// por perda de hardlink; vazia, essa regra não apaga nada.
    #[serde(default)]
    pub managed_categories: Vec<String>,
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
            managed_categories: Vec::new(),
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
            managed_categories: self.managed_categories.clone(),
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

        anyhow::ensure!(
            (0.0..=1.0).contains(&config.policy.max_batch_fraction),
            "`policy.max_batch_fraction` precisa ficar entre 0.0 e 1.0"
        );
        Ok(config)
    }

    /// O que o ciclo de limpeza exige além do que o arquivo já garante.
    ///
    /// # Errors
    ///
    /// Sem cliente de download, sem instância ou sem raiz de biblioteca.
    pub fn janitor(&self) -> Result<&QbitConfig> {
        let qbit = self
            .qbittorrent
            .as_ref()
            .context("seção `[qbittorrent]` ausente: o ciclo age pelo cliente de download")?;
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

        Ok(qbit)
    }

    /// O que `serve` exige.
    ///
    /// Conecta ao banco do catálogo e das contas, aplicando as migrações.
    ///
    /// # Errors
    ///
    /// Sem `[database]`, ou banco inalcançável.
    pub async fn store(&self) -> Result<acervo_store::Store> {
        let database = self
            .database
            .as_ref()
            .context("seção `[database]` ausente: catálogo e contas precisam do Postgres")?;
        acervo_store::Store::connect(&database.url)
            .await
            .context("conectando ao banco")
    }

    /// # Errors
    ///
    /// Sem `[server]`, chave curta demais ou nenhum indexador.
    pub fn server(&self) -> Result<&ServerConfig> {
        let server = self
            .server
            .as_ref()
            .context("seção `[server]` ausente: `serve` precisa de endereço e chave")?;
        // A superfície devolve links de download de tracker, com passkey.
        // Chave curta é chave adivinhável.
        anyhow::ensure!(
            server.api_key.len() >= 16,
            "`server.api_key` precisa de ao menos 16 caracteres"
        );
        for indexer in &self.indexers {
            if let IndexerConfig::Torznab(spec) = indexer {
                anyhow::ensure!(
                    spec.request_interval_seconds.is_finite()
                        && (0.0..=3600.0).contains(&spec.request_interval_seconds),
                    "`request_interval_seconds` de `{}` precisa ficar entre 0 e 3600",
                    spec.name
                );
            }
        }
        Ok(server)
    }

    /// O que `sync` exige além de `serve`: o endereço público e as instâncias.
    ///
    /// # Errors
    ///
    /// Sem `server.public_url`, URL inválida ou nenhuma instância.
    pub fn sync(&self) -> Result<(&ServerConfig, String)> {
        let server = self.server()?;
        let public_url = server
            .public_url
            .as_deref()
            .context("`server.public_url` ausente: `sync` precisa saber como os gerenciadores alcançam este serviço")?;
        let parsed = url::Url::parse(public_url)
            .with_context(|| format!("`server.public_url` inválida: `{public_url}`"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.query().is_none()
                && parsed.username().is_empty(),
            "`server.public_url` precisa ser HTTP(S), sem query e sem credencial"
        );
        anyhow::ensure!(
            !self.instances.is_empty(),
            "nenhuma `[[instances]]` configurada: não haveria onde cadastrar"
        );
        Ok((server, public_url.trim_end_matches('/').to_owned()))
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
fn default_bind() -> String {
    // Só a própria máquina, a menos que se diga o contrário. Em container, a
    // configuração diz `0.0.0.0:9797`.
    "127.0.0.1:9797".into()
}
const fn default_request_interval() -> f64 {
    2.0
}
fn default_ledger_path() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/strikes.json")
}
fn default_credentials_path() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/credenciais.toml")
}
fn default_registry_path() -> PathBuf {
    PathBuf::from("~/.local/state/acervo-hub/indexadores.toml")
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

    fn load(text: &str) -> Result<Config> {
        let path = std::env::temp_dir().join(format!(
            "acervo-hub-config-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, text)?;
        let config = Config::load(&path);
        std::fs::remove_file(&path)?;
        config
    }

    fn parse(text: &str) -> Result<Config> {
        let config = load(text)?;
        config.janitor()?;
        Ok(config)
    }

    const SERVIDOR: &str = r#"
        [server]
        api_key = "0123456789abcdef"

        [[indexers]]
        kind = "torznab"
        name = "agregador"
        url = "http://localhost:9696/1/api"
        api_key = "k"

        [[indexers]]
        kind = "cardigann"
        definition = "/etc/acervo-hub/definicoes/publico.yml"
        settings = { token = "t" }
    "#;

    #[test]
    fn exemplo_versionado_serve_aos_dois_modos() {
        let config = load(include_str!("../../../config.example.toml")).unwrap();
        config.janitor().unwrap();
        config.server().unwrap();
        let (_, public_url) = config.sync().unwrap();
        assert_eq!(public_url, "http://acervo-hub-indexadores:9797");
    }

    #[test]
    fn sync_exige_endereco_publico_valido() {
        let exemplo = include_str!("../../../config.example.toml");
        let sem = exemplo.replace("public_url = \"http://acervo-hub-indexadores:9797\"\n", "");
        assert!(load(&sem).unwrap().sync().is_err());
        let com_credencial = exemplo.replace("http://acervo-hub-indexadores", "http://u:p@acervo");
        assert!(load(&com_credencial).unwrap().sync().is_err());
    }

    #[test]
    fn servidor_sobe_sem_a_configuracao_do_ciclo() {
        let config = load(SERVIDOR).unwrap();
        assert_eq!(config.server().unwrap().bind, "127.0.0.1:9797");
        assert_eq!(config.indexers.len(), 2);
        assert!(config.janitor().is_err());
    }

    #[test]
    fn campo_errado_em_indexador_tambem_e_erro() {
        let torto = SERVIDOR.replace("api_key = \"k\"", "apikey = \"k\"");
        assert!(load(&torto).is_err());
        let tipo = SERVIDOR.replace("kind = \"torznab\"", "kind = \"newznab\"");
        assert!(load(&tipo).is_err());
    }

    #[test]
    fn chave_curta_ou_sem_indexador_e_recusada() {
        let curta = SERVIDOR.replace("0123456789abcdef", "curta");
        assert!(load(&curta).unwrap().server().is_err());
        // Sem indexador sobe: eles podem ser adicionados pela interface.
        let vazio = SERVIDOR.split("[[indexers]]").next().unwrap();
        assert!(load(vazio).unwrap().server().is_ok());
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
        let erro = format!("{:#}", parse(&torto).unwrap_err());
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

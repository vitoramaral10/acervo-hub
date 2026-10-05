//! A configuração do serviço, em memória.
//!
//! Vem do banco, uma seção por linha de `config_sections`, e se edita pela
//! tela; fora dele só ficam o endereço do banco e o de escuta, no ambiente.
//! Seção ausente vale o padrão. Cada seção é um JSON com
//! `deny_unknown_fields`: campo escrito errado é erro barulhento, não uma
//! trava de segurança caindo no padrão em silêncio.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use acervo_core::Allocated;
use acervo_janitor::{Guards, Policy};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SERVIDOR: &str = "servidor";
pub const QBITTORRENT: &str = "qbittorrent";
pub const JELLYFIN: &str = "jellyfin";
pub const BIBLIOTECA: &str = "biblioteca";
pub const LIMPEZA: &str = "limpeza";
pub const TAREFAS: &str = "tarefas";

/// Campos secretos de cada seção: nunca voltam pela API, e vazio ao salvar
/// mantém o valor guardado.
#[must_use]
pub fn secrets(section: &str) -> &'static [&'static str] {
    match section {
        SERVIDOR => &["api_key", "proxy_password"],
        JELLYFIN => &["api_key"],
        QBITTORRENT => &["password"],
        _ => &[],
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// Chave da interface, catálogo de definições, rede e timeout HTTP.
    pub server: ServerConfig,
    /// Cliente de download. Sem URL, não há cliente: o grab e a limpeza
    /// ficam parados.
    pub qbittorrent: QbitConfig,
    /// Servidor de mídia que diz o que já foi assistido. Sem URL, a tarefa de
    /// apagar assistidos fica parada.
    pub jellyfin: JellyfinConfig,
    pub library: LibraryConfig,
    pub policy: PolicyConfig,
    pub tasks: TasksConfig,
}

/// Onde o XEM responde, se a seção não disser outro.
pub const XEM_URL: &str = "https://thexem.info";

/// O arquivo compactado da branch principal do repositório oficial de
/// definições: um download só, em vez de uma requisição por arquivo à API.
pub const DEFINITIONS_URL: &str =
    "https://codeload.github.com/Prowlarr/Indexers/tar.gz/refs/heads/master";

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Chave que a interface aceita em `X-Api-Key`, para quem a usa sem
    /// sessão. Vazia, nenhuma chave vale.
    pub api_key: String,
    /// Diretórios locais de definições Cardigann — as customizadas. Valem
    /// acima de tudo e nunca são trocadas pelas do repositório. O primeiro
    /// que tiver um id vence.
    #[serde(rename = "catalogos")]
    pub catalogs: Vec<PathBuf>,
    /// Diretórios de reserva — o catálogo antigo, copiado do agregador de
    /// referência. Valem abaixo das definições baixadas para o banco.
    #[serde(rename = "catalogos_reserva")]
    pub reserve_catalogs: Vec<PathBuf>,
    /// De onde a tarefa `definicoes` baixa o arquivo `.tar.gz` do repositório
    /// de definições; dentro dele, as de `definitions/v11`.
    #[serde(rename = "definicoes_url")]
    pub definitions_url: String,
    /// Timeout de cada chamada HTTP, em segundos.
    pub http_timeout_seconds: u64,
    /// Endereço base do XEM, de onde vem a numeração de cena das séries.
    pub xem_url: String,
    /// O `FlareSolverr`, que vence o desafio do Cloudflare. Sem ele, o desafio
    /// é erro.
    pub flaresolverr_url: Option<String>,
    /// Quanto o `FlareSolverr` pode levar num desafio, em segundos.
    pub flaresolverr_timeout_s: u64,
    /// Proxy dos indexadores marcados para usá-lo: `http://`, `https://` ou
    /// `socks5://`, sem credencial — ela tem campo próprio.
    pub proxy_url: Option<String>,
    pub proxy_username: String,
    pub proxy_password: String,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A chave dá acesso a links com passkey.
        formatter
            .debug_struct("ServerConfig")
            .field("catalogs", &self.catalogs)
            .field("reserve_catalogs", &self.reserve_catalogs)
            .field("definitions_url", &self.definitions_url)
            .field("http_timeout_seconds", &self.http_timeout_seconds)
            .field("xem_url", &self.xem_url)
            .field("flaresolverr_url", &self.flaresolverr_url)
            .field("flaresolverr_timeout_s", &self.flaresolverr_timeout_s)
            .field("proxy_url", &self.proxy_url)
            .field("proxy_username", &self.proxy_username)
            .finish_non_exhaustive()
    }
}

impl ServerConfig {
    /// Definições, `FlareSolverr` e proxy: URLs e faixas.
    fn validate_network(&self) -> Result<(), String> {
        check_url("servidor: endereço das definições", &self.definitions_url)?;
        if let Some(url) = &self.flaresolverr_url {
            check_url("servidor: endereço do FlareSolverr", url)?;
        }
        if !(1..=300).contains(&self.flaresolverr_timeout_s) {
            return Err(
                "servidor: o timeout do FlareSolverr precisa ficar entre 1 e 300 segundos".into(),
            );
        }
        if let Some(url) = &self.proxy_url {
            let parsed = url::Url::parse(url).map_err(|_| "servidor: proxy: URL inválida")?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err("servidor: proxy: use http://, https:// ou socks5://".into());
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(
                    "servidor: proxy: tire usuário e senha da URL; eles têm campo próprio".into(),
                );
            }
        }
        Ok(())
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            catalogs: Vec::new(),
            reserve_catalogs: Vec::new(),
            definitions_url: DEFINITIONS_URL.into(),
            http_timeout_seconds: 30,
            xem_url: XEM_URL.into(),
            flaresolverr_url: None,
            flaresolverr_timeout_s: 60,
            proxy_url: None,
            proxy_username: String::new(),
            proxy_password: String::new(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QbitConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for QbitConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QbitConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JellyfinConfig {
    /// Como o serviço alcança o Jellyfin; em Compose, `http://jellyfin:8096`.
    pub url: String,
    pub api_key: String,
    /// Carência depois da última vez que alguém assistiu: dá tempo de
    /// marcar como favorito o que é para ficar.
    pub delete_watched_after_minutes: u64,
}

impl Default for JellyfinConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            api_key: String::new(),
            delete_watched_after_minutes: 60,
        }
    }
}

impl std::fmt::Debug for JellyfinConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A chave dá acesso de administrador ao servidor de mídia.
        formatter
            .debug_struct("JellyfinConfig")
            .field("url", &self.url)
            .field(
                "delete_watched_after_minutes",
                &self.delete_watched_after_minutes,
            )
            .finish_non_exhaustive()
    }
}

/// A categoria padrão da busca manual.
pub const MANUAL_CATEGORY: &str = "manual";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibraryConfig {
    /// Raízes do acervo, no host. Servem para medir o tamanho total, que é a
    /// base da trava proporcional.
    pub roots: Vec<PathBuf>,
    /// Pastas raiz dos filmes, como o cliente de download as vê. A tela
    /// oferece-as ao adicionar filme; apagar pasta só vale dentro delas.
    pub root_folders: Vec<String>,
    /// Pasta raiz das séries, como o cliente de download a vê: cada série
    /// nova nasce nela, e apagar pasta de série só vale dentro dela.
    pub series_root: String,
    /// Categoria do cliente de download para o que o acervo pega. Separada de
    /// qualquer outra: a fila e a limpeza só mexem no que é dela.
    pub category: String,
    /// Categoria do que a busca manual manda ao cliente. Fica fora das
    /// gerenciadas pela limpeza e de qualquer grab do acervo: o torrent é
    /// de quem o mandou.
    #[serde(rename = "categoria_manual")]
    pub manual_category: String,
    /// Caminho como o cliente vê → caminho no host.
    pub paths: BTreeMap<String, String>,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            root_folders: vec!["/media/movies".into()],
            series_root: "/media/series".into(),
            category: "acervo".into(),
            manual_category: MANUAL_CATEGORY.into(),
            paths: BTreeMap::new(),
        }
    }
}

impl LibraryConfig {
    /// A categoria da busca manual: preenchida, diferente da do acervo e
    /// fora das gerenciadas pela limpeza.
    fn validate_manual(&self, managed: &[String]) -> Result<(), String> {
        let manual = self.manual_category.trim();
        if manual.is_empty() {
            return Err("biblioteca: informe a categoria da busca manual".into());
        }
        if manual == self.category.trim() {
            return Err(
                "biblioteca: a categoria da busca manual precisa ser outra que a do acervo".into(),
            );
        }
        if managed.iter().any(|c| c.trim() == manual) {
            return Err(format!(
                "biblioteca: a categoria da busca manual ({manual}) está entre as gerenciadas \
                 pela limpeza; tire-a de lá ou escolha outra"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub orphan_strikes: u32,
    pub delete_private_orphans: bool,
    /// Zero apaga sem carência de seed.
    pub private_seed_grace_hours: u64,
    /// Ratio que libera o seed privado sem vínculo antes do teto. Zero desliga.
    pub seed_ratio_alvo: f64,
    /// Horas sem transferência que liberam o seed privado sem vínculo antes do
    /// teto. Zero desliga.
    pub seed_ocioso_horas: u64,
    pub recent_change_grace_hours: u64,
    pub max_batch_gib: u64,
    pub max_batch_fraction: f64,
    /// Categorias do cliente cujos downloads a limpeza gerencia — a do
    /// acervo e as que sobraram de antes dele. Só seed nelas pode ser apagado
    /// por perda de hardlink; vazia, essa regra não apaga nada.
    pub managed_categories: Vec<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            orphan_strikes: 3,
            delete_private_orphans: false,
            private_seed_grace_hours: 120,
            seed_ratio_alvo: 1.0,
            seed_ocioso_horas: 24,
            recent_change_grace_hours: 24,
            max_batch_gib: 300,
            max_batch_fraction: 0.30,
            managed_categories: Vec::new(),
        }
    }
}

impl PolicyConfig {
    #[must_use]
    pub fn to_policy(&self) -> Policy {
        Policy {
            orphan_strikes: self.orphan_strikes,
            delete_private_orphans: self.delete_private_orphans,
            private_seed_grace: (self.private_seed_grace_hours > 0)
                .then(|| Duration::from_secs(self.private_seed_grace_hours * 3600)),
            private_seed_ratio: (self.seed_ratio_alvo > 0.0).then_some(self.seed_ratio_alvo),
            private_seed_idle: (self.seed_ocioso_horas > 0)
                .then(|| Duration::from_secs(self.seed_ocioso_horas * 3600)),
            managed_categories: self.managed_categories.clone(),
            guards: Guards {
                recent_change_grace: Duration::from_secs(self.recent_change_grace_hours * 3600),
                max_batch: Allocated::from_bytes(self.max_batch_gib * 1024 * 1024 * 1024),
                max_batch_fraction: self.max_batch_fraction,
            },
        }
    }
}

/// Ids das tarefas de fundo com o intervalo padrão, em minutos. Zero desliga
/// o agendamento; "rodar agora" continua valendo.
pub const TASK_DEFAULTS: [(&str, u64); 9] = [
    // Sem intervalo, só pelo botão.
    ("busca", 0),
    ("rss", 30),
    ("importacao", 5),
    ("metadados", 360),
    ("limpeza", 60),
    ("assistidos", 15),
    // A numeração de cena muda pouco: uma vez por dia.
    ("cena", 24 * 60),
    // As definições do repositório oficial, uma vez por dia.
    ("definicoes", 24 * 60),
    // A conferência do disco: o que sumiu da biblioteca volta pelo mesmo
    // torrent. Ler o catálogo inteiro no disco a cada 6 h basta.
    ("disco", 6 * 60),
];

/// Teto de qualquer intervalo: 30 dias.
const MAX_INTERVAL: u64 = 30 * 24 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// Minutos entre execuções, por id de tarefa. Id ausente vale o padrão.
    pub intervalos: BTreeMap<String, u64>,
    /// Quantos filmes cada rodada agendada da busca dos que faltam busca.
    pub search_limit: usize,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            intervalos: TASK_DEFAULTS
                .iter()
                .map(|(id, minutes)| ((*id).to_owned(), *minutes))
                .collect(),
            search_limit: 5,
        }
    }
}

impl TasksConfig {
    /// Minutos entre execuções da tarefa.
    #[must_use]
    pub fn minutes(&self, id: &str) -> u64 {
        self.intervalos.get(id).copied().unwrap_or_else(|| {
            TASK_DEFAULTS
                .iter()
                .find(|(known, _)| *known == id)
                .map_or(0, |(_, minutes)| *minutes)
        })
    }

    #[must_use]
    pub fn interval(&self, id: &str) -> Duration {
        Duration::from_secs(self.minutes(id) * 60)
    }
}

/// Uma URL HTTP(S) sem credencial nem query, ou a mensagem do porquê não.
fn check_url(field: &str, value: &str) -> Result<(), String> {
    let parsed = url::Url::parse(value).map_err(|_| format!("{field}: URL inválida"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("{field}: use http:// ou https://"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "{field}: tire usuário e senha da URL; eles têm campo próprio"
        ));
    }
    Ok(())
}

impl Config {
    /// Monta a configuração das seções gravadas; a que falta vale o padrão.
    ///
    /// # Errors
    ///
    /// Seção desconhecida ou fora do formato.
    pub fn from_sections(rows: impl IntoIterator<Item = (String, Value)>) -> Result<Self> {
        let mut config = Self::default();
        for (name, value) in rows {
            config = config
                .with_section(&name, value)
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("lendo a seção `{name}` do banco"))?;
        }
        Ok(config)
    }

    /// A seção como ela é gravada, segredo incluso.
    ///
    /// # Errors
    ///
    /// Seção desconhecida.
    pub fn section(&self, name: &str) -> Result<Value, String> {
        let value = match name {
            SERVIDOR => serde_json::to_value(&self.server),
            QBITTORRENT => serde_json::to_value(&self.qbittorrent),
            JELLYFIN => serde_json::to_value(&self.jellyfin),
            BIBLIOTECA => serde_json::to_value(&self.library),
            LIMPEZA => serde_json::to_value(&self.policy),
            TAREFAS => serde_json::to_value(&self.tasks),
            other => return Err(format!("seção desconhecida: {other}")),
        };
        value.map_err(|e| e.to_string())
    }

    /// Esta configuração com uma seção trocada, sem validar o conjunto.
    ///
    /// # Errors
    ///
    /// Seção desconhecida, campo desconhecido ou valor do tipo errado.
    pub fn with_section(&self, name: &str, value: Value) -> Result<Self, String> {
        fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
            serde_json::from_value(value).map_err(|e| format!("valor inválido: {e}"))
        }
        let mut config = self.clone();
        match name {
            SERVIDOR => config.server = parse(value)?,
            QBITTORRENT => config.qbittorrent = parse(value)?,
            JELLYFIN => config.jellyfin = parse(value)?,
            BIBLIOTECA => config.library = parse(value)?,
            LIMPEZA => config.policy = parse(value)?,
            TAREFAS => config.tasks = parse(value)?,
            other => return Err(format!("seção desconhecida: {other}")),
        }
        Ok(config)
    }

    /// Confere o que o banco não confere: faixas, URLs, campos obrigatórios.
    /// A mensagem vai para a tela.
    ///
    /// # Errors
    ///
    /// O primeiro problema encontrado.
    pub fn validate(&self) -> Result<(), String> {
        let server = &self.server;
        // A chave abre a interface inteira, credenciais de tracker inclusas.
        // Chave curta é chave adivinhável.
        if !server.api_key.is_empty() && server.api_key.chars().count() < 16 {
            return Err("servidor: a chave de API precisa de ao menos 16 caracteres".into());
        }
        check_url("servidor: endereço do XEM", &server.xem_url)?;
        server.validate_network()?;
        if !(1..=600).contains(&server.http_timeout_seconds) {
            return Err("servidor: o timeout HTTP precisa ficar entre 1 e 600 segundos".into());
        }
        if !self.qbittorrent.url.is_empty() {
            check_url("cliente de download: URL", &self.qbittorrent.url)?;
        }
        if !self.jellyfin.url.is_empty() {
            check_url("Jellyfin: URL", &self.jellyfin.url)?;
            if self.jellyfin.api_key.is_empty() {
                return Err("Jellyfin: informe a chave de API".into());
            }
        }
        let library = &self.library;
        if library.root_folders.iter().any(|f| f.trim().is_empty()) {
            return Err("biblioteca: pasta raiz em branco".into());
        }
        if !library.series_root.starts_with('/') {
            return Err("biblioteca: a pasta raiz das séries precisa ser absoluta".into());
        }
        if library.category.trim().is_empty() {
            return Err("biblioteca: informe a categoria do cliente de download".into());
        }
        library.validate_manual(&self.policy.managed_categories)?;
        if library
            .paths
            .iter()
            .any(|(from, to)| !from.starts_with('/') || !to.starts_with('/'))
        {
            return Err("biblioteca: os dois lados de cada caminho precisam ser absolutos".into());
        }
        let policy = &self.policy;
        if !(0.0..=1.0).contains(&policy.max_batch_fraction) {
            return Err("limpeza: a fração máxima do lote precisa ficar entre 0 e 1".into());
        }
        if !policy.seed_ratio_alvo.is_finite() || policy.seed_ratio_alvo < 0.0 {
            return Err("limpeza: o ratio alvo não pode ser negativo".into());
        }
        if policy.orphan_strikes == 0 {
            return Err("limpeza: são precisos ao menos 1 strike".into());
        }
        for (id, minutes) in &self.tasks.intervalos {
            if !TASK_DEFAULTS.iter().any(|(known, _)| known == id) {
                return Err(format!("tarefas: tarefa desconhecida: {id}"));
            }
            if *minutes > MAX_INTERVAL {
                return Err(format!(
                    "tarefas: o intervalo de {id} passa de 30 dias ({MAX_INTERVAL} minutos)"
                ));
            }
        }
        // Ler os releases recentes de todos os indexadores mais de uma vez a
        // cada 5 minutos só gasta consulta de tracker.
        if (1..5).contains(&self.tasks.minutes("rss")) {
            return Err("tarefas: o RSS roda no mínimo a cada 5 minutos (0 desliga)".into());
        }
        if !(1..=1000).contains(&self.tasks.search_limit) {
            return Err("tarefas: o limite da busca precisa ficar entre 1 e 1000".into());
        }
        Ok(())
    }

    /// O cliente de download, se configurado.
    #[must_use]
    pub fn qbittorrent(&self) -> Option<&QbitConfig> {
        (!self.qbittorrent.url.is_empty()).then_some(&self.qbittorrent)
    }

    /// O Jellyfin, se configurado.
    #[must_use]
    pub fn jellyfin(&self) -> Option<&JellyfinConfig> {
        (!self.jellyfin.url.is_empty()).then_some(&self.jellyfin)
    }

    /// O que o ciclo de limpeza exige.
    ///
    /// # Errors
    ///
    /// Sem cliente de download ou sem raiz de biblioteca.
    pub fn janitor(&self) -> Result<&QbitConfig> {
        let qbit = self.qbittorrent().context(
            "cliente de download não configurado: o ciclo age pelo qBittorrent \
             (Configurações → Cliente de download)",
        )?;
        anyhow::ensure!(
            !self.library.roots.is_empty(),
            "nenhuma raiz de biblioteca configurada: sem medir a biblioteca, a \
             trava proporcional não tem denominador"
        );
        Ok(qbit)
    }

    #[must_use]
    pub fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.server.http_timeout_seconds)
    }

    #[must_use]
    pub fn path_map(&self) -> acervo_fs::PathMap {
        acervo_fs::PathMap::new(
            self.library
                .paths
                .iter()
                .map(|(from, to)| (PathBuf::from(from), PathBuf::from(to))),
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid() -> Config {
        let mut config = Config::default();
        config.server.api_key = "0123456789abcdef".into();
        config.qbittorrent = QbitConfig {
            url: "http://localhost:8080".into(),
            username: "u".into(),
            password: "p".into(),
        };
        config.library.roots.push("/acervo".into());
        config
    }

    #[test]
    fn sem_secoes_valem_os_padroes_conservadores() {
        let config = Config::from_sections([]).unwrap();
        assert!(config.validate().is_ok());
        let p = config.policy.to_policy();
        assert_eq!(p.orphan_strikes, 3);
        assert!(!p.delete_private_orphans);
        assert!(p.private_seed_grace.is_some());
        assert!((p.guards.max_batch_fraction - 0.30).abs() < f64::EPSILON);
        assert_eq!(config.tasks.minutes("busca"), 0);
        assert_eq!(config.tasks.minutes("rss"), 30);
        assert_eq!(config.tasks.minutes("limpeza"), 60);
        assert_eq!(config.tasks.minutes("assistidos"), 15);
        assert_eq!(config.tasks.minutes("cena"), 1440);
        assert_eq!(config.server.xem_url, "https://thexem.info");
        assert_eq!(config.tasks.search_limit, 5);
        assert_eq!(config.library.root_folders, ["/media/movies"]);
        assert_eq!(config.library.series_root, "/media/series");
        assert_eq!(config.library.category, "acervo");
        assert_eq!(config.library.manual_category, "manual");
        assert_eq!(config.tasks.minutes("definicoes"), 1440);
        assert_eq!(config.tasks.minutes("disco"), 360);
        assert_eq!(config.server.definitions_url, DEFINITIONS_URL);
        assert_eq!(config.server.flaresolverr_timeout_s, 60);
        assert!(config.server.flaresolverr_url.is_none() && config.server.proxy_url.is_none());
        assert_eq!(config.http_timeout(), Duration::from_secs(30));
        assert!(config.qbittorrent().is_none() && config.jellyfin().is_none());
        assert!(config.janitor().is_err());
    }

    #[test]
    fn campo_escrito_errado_e_erro_e_nao_silencio() {
        let erro = Config::default()
            .with_section(LIMPEZA, json!({ "orphan_strikez": 99 }))
            .unwrap_err();
        assert!(erro.contains("orphan_strikez"), "erro veio: {erro}");
        assert!(Config::default().with_section("outra", json!({})).is_err());
    }

    #[test]
    fn secao_parcial_completa_com_o_padrao() {
        let config = Config::default()
            .with_section(TAREFAS, json!({ "intervalos": { "busca": 120 } }))
            .unwrap();
        assert_eq!(config.tasks.minutes("busca"), 120);
        assert_eq!(config.tasks.minutes("importacao"), 5);
        assert_eq!(config.tasks.search_limit, 5);
    }

    #[test]
    fn ida_e_volta_de_cada_secao() {
        let config = valid();
        let mut back = Config::default();
        for name in [
            SERVIDOR,
            QBITTORRENT,
            JELLYFIN,
            BIBLIOTECA,
            LIMPEZA,
            TAREFAS,
        ] {
            back = back
                .with_section(name, config.section(name).unwrap())
                .unwrap();
        }
        assert_eq!(back, config);
    }

    #[test]
    fn limpeza_exige_cliente_e_raiz() {
        assert!(valid().janitor().is_ok());
        let mut sem_raiz = valid();
        sem_raiz.library.roots.clear();
        assert!(sem_raiz.janitor().is_err());
        let mut sem_cliente = valid();
        sem_cliente.qbittorrent.url.clear();
        assert!(sem_cliente.janitor().is_err());
    }

    #[test]
    fn limpeza_antiga_sem_os_campos_de_seed_carrega_com_padroes() {
        let antiga = serde_json::json!({
            "orphan_strikes": 2,
            "private_seed_grace_hours": 72,
            "managed_categories": ["tv-sonarr"],
        });
        let config = Config::from_sections([("limpeza".to_string(), antiga)]).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.policy.private_seed_grace_hours, 72);
        assert!((config.policy.seed_ratio_alvo - 1.0).abs() < f64::EPSILON);
        assert_eq!(config.policy.seed_ocioso_horas, 24);
        let p = config.policy.to_policy();
        assert_eq!(p.private_seed_ratio, Some(1.0));
        assert_eq!(p.private_seed_idle, Some(Duration::from_secs(24 * 3600)));
    }

    #[test]
    fn validacao_recusa_com_mensagem() {
        let erro = |change: fn(&mut Config)| {
            let mut config = valid();
            change(&mut config);
            config.validate().unwrap_err()
        };
        assert!(erro(|c| c.server.api_key = "curta".into()).contains("16"));
        assert!(erro(|c| c.policy.max_batch_fraction = 1.5).contains("fração"));
        assert!(erro(|c| c.policy.seed_ratio_alvo = -1.0).contains("ratio"));
        assert!(erro(|c| c.qbittorrent.url = "http://u:p@x".into()).contains("usuário"));
        assert!(erro(|c| c.qbittorrent.url = "nada".into()).contains("URL"));
        assert!(
            erro(|c| {
                c.tasks.intervalos.insert("rss".into(), 2);
            })
            .contains("RSS")
        );
        assert!(
            erro(|c| {
                c.tasks.intervalos.insert("outra".into(), 2);
            })
            .contains("desconhecida")
        );
        assert!(erro(|c| c.jellyfin.url = "http://j:8096".into()).contains("chave"));
        assert!(erro(|c| c.library.series_root = "series".into()).contains("séries"));
        assert!(erro(|c| c.server.xem_url = "ftp://xem".into()).contains("XEM"));
        assert!(erro(|c| c.server.flaresolverr_url = Some("nada".into())).contains("FlareSolverr"));
        assert!(erro(|c| c.server.flaresolverr_timeout_s = 0).contains("FlareSolverr"));
        assert!(erro(|c| c.server.proxy_url = Some("ftp://p:21".into())).contains("socks5"));
        assert!(
            erro(|c| c.server.proxy_url = Some("socks5://u:s@p:1080".into())).contains("usuário")
        );
        assert!(erro(|c| c.library.manual_category = "acervo".into()).contains("manual"));
        assert!(
            erro(|c| c.policy.managed_categories = vec!["manual".into()]).contains("gerenciadas")
        );
        let mut proxied = valid();
        proxied.server.proxy_url = Some("socks5://proxy:1080".into());
        proxied.server.flaresolverr_url = Some("http://flaresolverr:8191".into());
        assert!(proxied.validate().is_ok());
        assert!(valid().validate().is_ok());
    }

    #[test]
    fn segredo_nao_aparece_em_depuracao() {
        let mut config = valid();
        config.jellyfin.api_key = "chave-jellyfin".into();
        let text = format!("{config:?}");
        assert!(!text.contains("chave-jellyfin"));
        assert!(!text.contains("0123456789abcdef"));
        assert!(!text.contains("password: \"p\""));
    }
}

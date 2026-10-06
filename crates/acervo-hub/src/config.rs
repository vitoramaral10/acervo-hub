//! A configuração do serviço, em memória.
//!
//! Vem do banco, uma seção por linha de `config_sections`, e se edita pela
//! tela; fora dele só ficam o endereço do banco e o de escuta, no ambiente.
//! Seção ausente vale o padrão. Cada seção é um JSON com
//! `deny_unknown_fields`: campo escrito errado é erro barulhento, não uma
//! trava de segurança caindo no padrão em silêncio.

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
    /// Servidor de mídia que diz o que já foi assistido. Sem URL, a tela
    /// "Para apagar" não sugere assistidos.
    pub jellyfin: JellyfinConfig,
    pub library: LibraryConfig,
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

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JellyfinConfig {
    /// Como o serviço alcança o Jellyfin; em Compose, `http://jellyfin:8096`.
    pub url: String,
    pub api_key: String,
}

impl std::fmt::Debug for JellyfinConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A chave dá acesso de administrador ao servidor de mídia.
        formatter
            .debug_struct("JellyfinConfig")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

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
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            root_folders: vec!["/media/movies".into()],
            series_root: "/media/series".into(),
            category: "acervo".into(),
        }
    }
}

/// Intervalos fixos das tarefas, em minutos.
pub const TASK_DEFAULTS: [(&str, u64); 7] = [
    ("busca", 120),
    ("rss", 30),
    ("importacao", 5),
    ("metadados", 360),
    ("limpeza", 60),
    ("cena", 1440),
    ("disco", 360),
];
pub const SEARCH_LIMIT: usize = 10;
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
pub const SUGGESTION_GRACE_MINUTES: u64 = 60;

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
        server.validate_network()?;
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

    /// Política fixa; só a categoria acompanha a biblioteca.
    #[must_use]
    pub fn cleanup_policy(&self) -> Policy {
        Policy {
            orphan_strikes: 3,
            delete_private_orphans: true,
            private_seed_grace: Some(Duration::from_secs(120 * 3600)),
            private_seed_ratio: Some(1.0),
            private_seed_idle: Some(Duration::from_secs(24 * 3600)),
            managed_categories: vec![self.library.category.clone()],
            guards: Guards {
                recent_change_grace: Duration::from_secs(24 * 3600),
                max_batch: Allocated::from_bytes(300 * 1024 * 1024 * 1024),
                max_batch_fraction: 0.3,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configuracao_reduzida_preserva_os_padroes_e_recusa_campos_removidos() {
        let config = Config::from_sections([]).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(crate::config::HTTP_TIMEOUT, Duration::from_secs(60));
        assert_eq!(config.library.category, "acervo");
        assert_eq!(config.cleanup_policy().managed_categories, ["acervo"]);
        assert!(config.cleanup_policy().delete_private_orphans);
        for (section, value) in [
            ("servidor", json!({"http_timeout_seconds": 60})),
            ("biblioteca", json!({"paths": {}})),
            ("jellyfin", json!({"carencia_sugestao_minutos": 60})),
            ("limpeza", json!({})),
            ("tarefas", json!({})),
        ] {
            assert!(config.with_section(section, value).is_err());
        }
        for section in [SERVIDOR, QBITTORRENT, JELLYFIN, BIBLIOTECA] {
            assert_eq!(
                config
                    .with_section(section, config.section(section).unwrap())
                    .unwrap(),
                config
            );
        }
    }
}

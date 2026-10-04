//! O handle da configuração: o instantâneo em memória e o caminho único para
//! mudá-lo.
//!
//! Quem usa pega o instantâneo na hora em que roda ([`Settings::get`]), e
//! não guarda: mudança feita na tela vale para a próxima tarefa e a próxima
//! requisição, sem reiniciar. Quem precisa reagir à mudança — o agendador,
//! que recalcula os intervalos — assina ([`Settings::subscribe`]).

use std::sync::Arc;

use acervo_store::Store;
use anyhow::Result;
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::config::{self, Config, GERENCIADORES, SERVIDOR, TAREFAS};

pub struct Settings {
    store: Store,
    current: watch::Sender<Arc<Config>>,
    /// Serializa as gravações: duas seções salvas juntas partem cada uma do
    /// instantâneo que a outra deixou.
    write: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Settings")
            .field("current", &self.get())
            .finish_non_exhaustive()
    }
}

/// Segredo "em branco": vazio, ausente, ou o `{"definida": …}` que a própria
/// API devolveu e a tela mandou de volta.
fn blank(value: &Value) -> bool {
    match value {
        Value::Null | Value::Object(_) => true,
        Value::String(text) => text.trim().is_empty(),
        _ => false,
    }
}

fn hide(object: &mut Map<String, Value>, field: &str) {
    if let Some(value) = object.get_mut(field) {
        let defined = !blank(value);
        *value = serde_json::json!({ "definida": defined });
    }
}

/// A seção como a API a devolve: cada segredo vira `{"definida": bool}`.
#[must_use]
pub fn mask(section: &str, mut value: Value) -> Value {
    let secrets = config::secrets(section);
    match &mut value {
        Value::Object(object) => {
            for field in secrets {
                hide(object, field);
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Value::Object(object) = item {
                    for field in secrets {
                        hide(object, field);
                    }
                }
            }
        }
        _ => {}
    }
    value
}

/// O que foi enviado por cima do que está gravado. Campo ausente mantém o
/// atual; segredo em branco também — ele nunca volta à tela, então o campo
/// vazio não pode apagá-lo.
fn merge(section: &str, current: Value, incoming: Value) -> Result<Value, String> {
    let secrets = config::secrets(section);
    if section == GERENCIADORES {
        let Value::Array(items) = incoming else {
            return Err("gerenciadores: envie uma lista".into());
        };
        let current = match current {
            Value::Array(items) => items,
            _ => Vec::new(),
        };
        let mut merged = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(mut item) = item else {
                return Err("gerenciadores: cada item precisa ser um objeto".into());
            };
            // O mesmo gerenciador, pelo nome: a chave guardada continua.
            let saved = current
                .iter()
                .find(|old| old.get("name") == item.get("name"))
                .and_then(Value::as_object);
            for field in secrets {
                if item.get(*field).is_none_or(blank) {
                    match saved.and_then(|old| old.get(*field)) {
                        Some(value) => item.insert((*field).to_owned(), value.clone()),
                        None => item.remove(*field),
                    };
                }
            }
            merged.push(Value::Object(item));
        }
        return Ok(Value::Array(merged));
    }
    let Value::Object(incoming) = incoming else {
        return Err(format!("{section}: envie um objeto"));
    };
    let mut merged = match current {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    for (field, value) in incoming {
        if secrets.contains(&field.as_str()) && blank(&value) {
            continue;
        }
        // Intervalos vão por tarefa: mandar só o da busca não pode
        // devolver as outras ao padrão.
        if section == TAREFAS
            && field == "intervalos"
            && let (Some(Value::Object(saved)), Value::Object(sent)) =
                (merged.get_mut(&field), &value)
        {
            saved.extend(sent.clone());
            continue;
        }
        merged.insert(field, value);
    }
    // Endereço em branco é "sem endereço", não uma URL vazia.
    if section == SERVIDOR {
        for field in ["public_url", "flaresolverr_url", "proxy_url"] {
            if merged
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|url| url.trim().is_empty())
            {
                merged.insert(field.into(), Value::Null);
            }
        }
    }
    Ok(Value::Object(merged))
}

impl Settings {
    /// Lê a configuração do banco.
    ///
    /// # Errors
    ///
    /// Banco inalcançável, ou seção gravada fora do formato.
    pub async fn load(store: Store) -> Result<Self> {
        let config = Config::from_sections(store.config_sections().await?)?;
        Ok(Self::new(store, config))
    }

    #[must_use]
    pub fn new(store: Store, config: Config) -> Self {
        Self {
            store,
            current: watch::Sender::new(Arc::new(config)),
            write: tokio::sync::Mutex::new(()),
        }
    }

    /// O instantâneo de agora.
    #[must_use]
    pub fn get(&self) -> Arc<Config> {
        Arc::clone(&self.current.borrow())
    }

    /// Avisa a cada instantâneo novo.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<Config>> {
        self.current.subscribe()
    }

    /// Uma seção como a API a devolve, sem segredo.
    ///
    /// # Errors
    ///
    /// Seção desconhecida.
    pub fn view(&self, section: &str) -> Result<Value, String> {
        Ok(mask(section, self.get().section(section)?))
    }

    /// Valida, grava e troca o instantâneo. Devolve a seção como a API a
    /// devolve. Inválida, nada muda.
    ///
    /// # Errors
    ///
    /// Seção desconhecida, valor inválido (a mensagem diz qual, em
    /// português) ou falha ao gravar.
    pub async fn save_section(&self, section: &str, incoming: Value) -> Result<Value, String> {
        let _guard = self.write.lock().await;
        let current = self.get();
        let merged = merge(section, current.section(section)?, incoming)?;
        let next = current
            .with_section(section, merged)
            .map_err(|error| format!("{section}: {error}"))?;
        next.validate()?;
        let value = next.section(section)?;
        self.store
            .save_config_section(section, &value, &crate::decide::now_rfc3339())
            .await
            .map_err(|error| format!("gravando a configuração: {error}"))?;
        self.current.send_replace(Arc::new(next));
        tracing::info!(secao = section, "configuração salva pela interface");
        Ok(mask(section, value))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn salvar_troca_o_instantaneo_e_segredo_vazio_mantem() {
        let Some(db) = acervo_store::testing::TestDb::new("handle").await else {
            return;
        };
        let settings = Settings::load(db.store.clone()).await.unwrap();
        let mut changes = settings.subscribe();
        let before = settings.get();
        assert!(before.qbittorrent().is_none());

        let view = settings
            .save_section(
                "qbittorrent",
                json!({ "url": "http://qbit:8080", "username": "u", "password": "s1" }),
            )
            .await
            .unwrap();
        assert_eq!(view["password"], json!({ "definida": true }));
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        // O instantâneo antigo segue inteiro para quem já o tinha.
        assert!(before.qbittorrent().is_none());
        assert_eq!(settings.get().qbittorrent.password, "s1");

        // Senha em branco (ou o `definida` devolvido) mantém; URL troca.
        for blank in [json!(""), json!({ "definida": true }), Value::Null] {
            settings
                .save_section(
                    "qbittorrent",
                    json!({ "url": "http://outro:8080", "password": blank }),
                )
                .await
                .unwrap();
        }
        let now = settings.get();
        assert_eq!(now.qbittorrent.password, "s1");
        assert_eq!(now.qbittorrent.url, "http://outro:8080");
        assert_eq!(now.qbittorrent.username, "u", "campo ausente mantém");

        // Inválido: nada muda, nem no banco.
        changes.mark_unchanged();
        let error = settings
            .save_section("limpeza", json!({ "max_batch_fraction": 2.0 }))
            .await
            .unwrap_err();
        assert!(error.contains("fração"), "{error}");
        assert!(!changes.has_changed().unwrap());
        assert!((settings.get().policy.max_batch_fraction - 0.30).abs() < f64::EPSILON);
        assert!(
            settings
                .save_section("limpeza", json!({ "orphan_strikez": 1 }))
                .await
                .is_err()
        );

        // Gerenciadores: a chave segue o nome.
        settings
            .save_section(
                "gerenciadores",
                json!([{ "name": "series", "kind": "series", "url": "http://s:8989", "api_key": "k1" }]),
            )
            .await
            .unwrap();
        let view = settings
            .save_section(
                "gerenciadores",
                json!([{ "name": "series", "kind": "series", "url": "http://s2:8989", "api_key": "" }]),
            )
            .await
            .unwrap();
        assert_eq!(view[0]["api_key"], json!({ "definida": true }));
        assert_eq!(settings.get().instances[0].api_key, "k1");
        let renamed = settings
            .save_section(
                "gerenciadores",
                json!([{ "name": "outro", "kind": "series", "url": "http://s2:8989" }]),
            )
            .await
            .unwrap_err();
        assert!(renamed.contains("chave"), "{renamed}");

        // Intervalo de uma tarefa só: as outras ficam como estavam.
        settings
            .save_section("tarefas", json!({ "intervalos": { "rss": 45 } }))
            .await
            .unwrap();
        settings
            .save_section("tarefas", json!({ "intervalos": { "busca": 120 } }))
            .await
            .unwrap();
        assert_eq!(settings.get().tasks.minutes("rss"), 45);
        assert_eq!(settings.get().tasks.minutes("busca"), 120);

        // O banco guarda o mesmo que a memória.
        let reloaded = Settings::load(db.store.clone()).await.unwrap();
        assert_eq!(*reloaded.get(), *settings.get());
        db.drop().await;
    }

    #[test]
    fn mascara_cada_segredo() {
        let view = mask(
            "jellyfin",
            json!({ "url": "http://j", "api_key": "x", "delete_watched_after_minutes": 1 }),
        );
        assert_eq!(view["api_key"], json!({ "definida": true }));
        assert_eq!(view["url"], "http://j");
        let view = mask("servidor", json!({ "api_key": "" }));
        assert_eq!(view["api_key"], json!({ "definida": false }));
    }
}

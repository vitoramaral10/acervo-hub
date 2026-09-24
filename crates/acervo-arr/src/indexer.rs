//! Indexadores Torznab cadastrados numa instância, pela API v3.
//!
//! O cadastro existente é lido e reescrito como JSON cru: a instância guarda
//! campos que este crate não conhece (prioridade, tags, seed ratio, ajustes
//! que alguém fez na interface), e uma atualização não pode apagá-los.

use serde_json::{Value, json};

use crate::{ArrClient, ArrError};

/// O que a sincronização decide sobre um indexador Torznab.
#[derive(Clone, PartialEq, Eq)]
pub struct TorznabSpec {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub categories: Vec<u32>,
    /// Só o gerenciador de séries tem o campo; `None` não o envia.
    pub anime_categories: Option<Vec<u32>>,
}

impl std::fmt::Debug for TorznabSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TorznabSpec")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("categories", &self.categories)
            .field("anime_categories", &self.anime_categories)
            .finish()
    }
}

/// Um indexador como a instância o devolve.
#[derive(Debug, Clone)]
pub struct RemoteIndexer {
    pub id: u64,
    pub name: String,
    pub implementation: String,
    raw: Value,
}

impl RemoteIndexer {
    /// Lê um item de `GET /api/v3/indexer`. Sem `id` ou `name`, não é um
    /// cadastro que se possa atualizar ou remover.
    #[must_use]
    pub fn from_raw(raw: Value) -> Option<Self> {
        Some(Self {
            id: raw.get("id")?.as_u64()?,
            name: raw.get("name")?.as_str()?.to_owned(),
            implementation: raw
                .get("implementation")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            raw,
        })
    }

    fn field(&self, name: &str) -> Option<&Value> {
        self.raw
            .get("fields")?
            .as_array()?
            .iter()
            .find(|field| field.get("name").and_then(Value::as_str) == Some(name))?
            .get("value")
    }

    /// O cadastro já corresponde à especificação?
    ///
    /// A chave não entra na comparação: a instância a devolve mascarada.
    #[must_use]
    pub fn matches(&self, spec: &TorznabSpec) -> bool {
        let numbers = |name: &str| -> Vec<u64> {
            let mut values: Vec<u64> = self
                .field(name)
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            values.sort_unstable();
            values
        };
        let sorted = |values: &[u32]| {
            let mut values: Vec<u64> = values.iter().copied().map(u64::from).collect();
            values.sort_unstable();
            values
        };
        self.implementation == "Torznab"
            && self.field("baseUrl").and_then(Value::as_str) == Some(spec.base_url.as_str())
            && self.field("apiPath").and_then(Value::as_str) == Some("/api")
            && numbers("categories") == sorted(&spec.categories)
            && spec
                .anime_categories
                .as_ref()
                .is_none_or(|anime| numbers("animeCategories") == sorted(anime))
    }

    /// Corpo de atualização: o cadastro existente com os campos gerenciados
    /// trocados. O resto — habilitações, prioridade, tags — fica como estava.
    fn updated(&self, spec: &TorznabSpec) -> Value {
        let mut body = self.raw.clone();
        body["name"] = json!(spec.name);
        set_fields(&mut body, spec);
        body
    }
}

fn set_fields(body: &mut Value, spec: &TorznabSpec) {
    let mut managed = vec![
        ("baseUrl", json!(spec.base_url)),
        ("apiPath", json!("/api")),
        ("apiKey", json!(spec.api_key)),
        ("categories", json!(spec.categories)),
    ];
    if let Some(anime) = &spec.anime_categories {
        managed.push(("animeCategories", json!(anime)));
    }
    if !body.get("fields").is_some_and(Value::is_array) {
        body["fields"] = json!([]);
    }
    let fields = body["fields"].as_array_mut().expect("garantido acima");
    for (name, value) in managed {
        match fields
            .iter_mut()
            .find(|field| field.get("name").and_then(Value::as_str) == Some(name))
        {
            Some(field) => field["value"] = value,
            None => fields.push(json!({ "name": name, "value": value })),
        }
    }
}

fn created(spec: &TorznabSpec) -> Value {
    let mut body = json!({
        "name": spec.name,
        "implementation": "Torznab",
        "configContract": "TorznabSettings",
        "protocol": "torrent",
        "enableRss": true,
        "enableAutomaticSearch": true,
        "enableInteractiveSearch": true,
        "priority": 25,
        "tags": [],
        "fields": [],
    });
    set_fields(&mut body, spec);
    body
}

impl ArrClient {
    /// Indexadores cadastrados.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn indexers(&self) -> Result<Vec<RemoteIndexer>, ArrError> {
        let path = "api/v3/indexer";
        let raw: Vec<Value> = self.get(self.url(path)?, &[], path).await?;
        Ok(raw
            .into_iter()
            .filter_map(RemoteIndexer::from_raw)
            .collect())
    }

    /// Cadastra um indexador Torznab.
    ///
    /// `forceSave` pula o teste que a instância faria ao salvar: um tracker
    /// fora do ar no momento da sincronização não pode impedir o cadastro.
    ///
    /// # Errors
    ///
    /// Falha de rede ou status não-2xx.
    pub async fn create_indexer(&self, spec: &TorznabSpec) -> Result<(), ArrError> {
        let path = "api/v3/indexer";
        let response = self
            .http
            .post(self.url(path)?)
            .query(&[("forceSave", "true")])
            .json(&created(spec))
            .send()
            .await
            .map_err(|source| self.transport(source))?;
        self.check_status(response, path).map(|_| ())
    }

    /// Atualiza um indexador, preservando o que não é gerenciado.
    ///
    /// # Errors
    ///
    /// Falha de rede ou status não-2xx.
    pub async fn update_indexer(
        &self,
        existing: &RemoteIndexer,
        spec: &TorznabSpec,
    ) -> Result<(), ArrError> {
        let path = format!("api/v3/indexer/{}", existing.id);
        let response = self
            .http
            .put(self.url(&path)?)
            .query(&[("forceSave", "true")])
            .json(&existing.updated(spec))
            .send()
            .await
            .map_err(|source| self.transport(source))?;
        self.check_status(response, &path).map(|_| ())
    }

    /// # Errors
    ///
    /// Falha de rede ou status não-2xx.
    pub async fn delete_indexer(&self, id: u64) -> Result<(), ArrError> {
        let path = format!("api/v3/indexer/{id}");
        let response = self
            .http
            .delete(self.url(&path)?)
            .send()
            .await
            .map_err(|source| self.transport(source))?;
        self.check_status(response, &path).map(|_| ())
    }

    fn transport(&self, source: reqwest::Error) -> ArrError {
        ArrError::Transport {
            instance: self.name.clone(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TorznabSpec {
        TorznabSpec {
            name: "publico (acervo-hub)".into(),
            base_url: "http://acervo:9797/publico".into(),
            api_key: "chave".into(),
            categories: vec![5040, 5000],
            anime_categories: Some(vec![5070]),
        }
    }

    fn remote(raw: Value) -> RemoteIndexer {
        RemoteIndexer {
            id: 7,
            name: "publico (acervo-hub)".into(),
            implementation: "Torznab".into(),
            raw,
        }
    }

    #[test]
    fn atualizacao_preserva_o_que_nao_e_gerenciado() {
        let existing = remote(json!({
            "id": 7,
            "name": "publico (acervo-hub)",
            "enableRss": false,
            "priority": 10,
            "tags": [3],
            "fields": [
                {"name": "baseUrl", "value": "http://antigo/publico"},
                {"name": "apiKey", "value": "********"},
                {"name": "minimumSeeders", "value": 5},
            ],
        }));
        let body = existing.updated(&spec());
        assert_eq!(body["enableRss"], json!(false));
        assert_eq!(body["priority"], json!(10));
        assert_eq!(body["tags"], json!([3]));
        let fields = body["fields"].as_array().unwrap();
        let value = |name: &str| {
            fields
                .iter()
                .find(|field| field["name"] == name)
                .map(|field| field["value"].clone())
        };
        assert_eq!(value("baseUrl"), Some(json!("http://acervo:9797/publico")));
        assert_eq!(value("apiKey"), Some(json!("chave")));
        assert_eq!(value("minimumSeeders"), Some(json!(5)));
        assert_eq!(value("categories"), Some(json!([5040, 5000])));
    }

    #[test]
    fn comparacao_ignora_a_chave_mascarada_e_a_ordem_das_categorias() {
        let current = remote(json!({
            "fields": [
                {"name": "baseUrl", "value": "http://acervo:9797/publico"},
                {"name": "apiPath", "value": "/api"},
                {"name": "apiKey", "value": "********"},
                {"name": "categories", "value": [5000, 5040]},
                {"name": "animeCategories", "value": [5070]},
            ],
        }));
        assert!(current.matches(&spec()));
        let mut other = spec();
        other.categories = vec![5000];
        assert!(!current.matches(&other));
    }

    #[test]
    fn debug_nao_mostra_a_chave() {
        assert!(!format!("{:?}", spec()).contains("chave"));
    }
}

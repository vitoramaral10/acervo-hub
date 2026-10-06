//! Contratos e adaptadores de teste; o servidor usa tipos concretos em produção.

use crate::api::{DefinitionCatalog, Entry, SettingView};
use crate::serve::{Database, HubAdmin};
use async_trait::async_trait;
use std::collections::BTreeMap;

/// O que a interface administra e só o binário sabe fazer: a configuração,
/// o catálogo de definições e as tarefas de fundo.
///
/// Mensagens de erro vão para a tela e não podem conter valor de setting.
#[async_trait]
pub trait Admin: Send + Sync + std::fmt::Debug {
    /// Settings editáveis do indexador; `None` se ele não tem nenhum.
    fn settings(&self, indexer: &str) -> Option<Vec<SettingView>>;

    /// Monta o indexador com os valores novos e os persiste. Campo ausente
    /// mantém o valor atual — é assim que segredo não precisa voltar à tela.
    ///
    /// # Errors
    ///
    /// Setting desconhecido, valor inválido ou falha ao gravar.
    async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String>;

    /// Se o indexador está cadastrado, e como a tela o rotula.
    fn origin(&self, indexer: &str) -> Option<&'static str>;

    /// Indexadores cadastrados fora do catálogo servido — desativados, ou
    /// que não subiram —, ainda listados.
    fn disabled(&self) -> Vec<(String, &'static str)>;

    /// Todas as definições conhecidas, suportadas ou não.
    async fn definitions(&self) -> Result<DefinitionCatalog, String>;

    /// Settings que uma definição pede para ser adicionada.
    fn definition_settings(&self, definition: &str) -> Option<Vec<SettingView>>;

    /// Adiciona um indexador a partir de uma definição do catálogo.
    ///
    /// # Errors
    ///
    /// Definição desconhecida ou não suportada, id já em uso, settings
    /// inválidos, falha ao gravar.
    async fn add(
        &self,
        definition: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String>;

    /// Remove um indexador cadastrado.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido ou falha ao gravar.
    async fn remove(&self, indexer: &str) -> Result<(), String>;

    /// Ativa ou desativa. Ativar devolve o indexador montado, para entrar no
    /// catálogo servido.
    ///
    /// # Errors
    ///
    /// Indexador desconhecido ou falha ao montar ou gravar.
    async fn set_enabled(&self, indexer: &str, enabled: bool) -> Result<Option<Entry>, String>;

    /// As tarefas de fundo do serviço — busca, RSS, importação, metadados,
    /// limpeza —, com intervalo, última e próxima execução de cada uma.
    fn tasks(&self) -> serde_json::Value;

    /// As últimas execuções das tarefas, da mais nova para a mais velha, com
    /// o detalhe de cada uma.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn task_history(&self) -> Result<serde_json::Value, String>;

    /// "Rodar agora": dispara a tarefa e responde na hora. Com ela já
    /// rodando, não dispara outra. `None` se a tarefa não existe.
    fn run_task(&self, id: &str) -> Option<serde_json::Value>;

    /// O catálogo de filmes, com o estado de cada arquivo no disco.
    ///
    /// # Errors
    ///
    /// Catálogo ilegível.
    async fn movies(&self) -> Result<serde_json::Value, String>;

    /// Dispara em segundo plano a busca de todos os filmes que faltam, e pega
    /// o escolhido de cada um. Responde na hora; com uma busca já rodando,
    /// não dispara outra.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    fn search_missing(&self) -> Result<serde_json::Value, String>;

    /// O andamento da busca dos que faltam: se roda, quantos de quantos.
    fn missing_status(&self) -> serde_json::Value;

    /// Busca e decide um filme do catálogo; com `apply`, manda o escolhido
    /// ao cliente de download.
    ///
    /// # Errors
    ///
    /// Filme desconhecido ou que já tem arquivo, busca que falhou, cliente
    /// inalcançável.
    async fn grab_movie(&self, movie_id: i64, apply: bool) -> Result<serde_json::Value, String>;

    /// As configurações guardadas pela tela. Segredo nunca volta: só se está
    /// definido.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn configuration(&self) -> Result<serde_json::Value, String>;

    /// Grava configurações. Chave ausente mantém o valor, `null` apaga, texto
    /// grava — depois de testar, quando dá para testar.
    ///
    /// # Errors
    ///
    /// Valor recusado no teste ou banco inalcançável.
    async fn save_configuration(
        &self,
        values: BTreeMap<String, Option<String>>,
    ) -> Result<serde_json::Value, String>;

    /// Uma seção da configuração do serviço. Segredo volta só como
    /// `{"definida": bool}`.
    ///
    /// # Errors
    ///
    /// Seção desconhecida.
    fn config_section(&self, _section: &str) -> Result<serde_json::Value, String> {
        Err("este serviço não tem configuração editável".into())
    }

    /// Valida e grava uma seção; devolve como ela ficou, no formato de
    /// [`Self::config_section`]. Segredo vazio ou ausente mantém o atual.
    ///
    /// # Errors
    ///
    /// Seção desconhecida, valor inválido (a mensagem diz qual) ou falha ao
    /// gravar.
    async fn save_config_section(
        &self,
        _section: &str,
        _value: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("este serviço não tem configuração editável".into())
    }
}

/// Contas da interface: confere usuário e senha e guarda as sessões.
///
/// Mensagens de erro vão para o log, não para a tela.
#[async_trait]
pub trait Accounts: Send + Sync + std::fmt::Debug {
    /// Token de uma sessão nova, se usuário e senha batem.
    ///
    /// # Errors
    ///
    /// Banco inalcançável — senha errada é `Ok(None)`.
    async fn login(&self, user: &str, password: &str) -> Result<Option<String>, String>;

    /// Dono da sessão, se ela existe e não venceu.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn session_user(&self, token: &str) -> Result<Option<String>, String>;

    /// Encerra a sessão.
    ///
    /// # Errors
    ///
    /// Banco inalcançável.
    async fn logout(&self, token: &str) -> Result<(), String>;
}

#[async_trait::async_trait]
impl Accounts for Database {
    async fn login(&self, user: &str, password: &str) -> Result<Option<String>, String> {
        Database::login(self, user, password).await
    }
    async fn session_user(&self, token: &str) -> Result<Option<String>, String> {
        Database::session_user(self, token).await
    }
    async fn logout(&self, token: &str) -> Result<(), String> {
        Database::logout(self, token).await
    }
}
#[async_trait::async_trait]
impl Admin for HubAdmin {
    fn settings(&self, indexer: &str) -> Option<Vec<SettingView>> {
        HubAdmin::settings(self, indexer)
    }
    async fn update(
        &self,
        indexer: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        HubAdmin::update(self, indexer, values).await
    }
    fn origin(&self, indexer: &str) -> Option<&'static str> {
        HubAdmin::origin(self, indexer)
    }
    fn disabled(&self) -> Vec<(String, &'static str)> {
        HubAdmin::disabled(self)
    }
    async fn definitions(&self) -> Result<DefinitionCatalog, String> {
        HubAdmin::definitions(self).await
    }
    fn definition_settings(&self, definition: &str) -> Option<Vec<SettingView>> {
        HubAdmin::definition_settings(self, definition)
    }
    async fn add(
        &self,
        definition: &str,
        values: BTreeMap<String, String>,
    ) -> Result<Entry, String> {
        HubAdmin::add(self, definition, values).await
    }
    async fn remove(&self, indexer: &str) -> Result<(), String> {
        HubAdmin::remove(self, indexer).await
    }
    async fn set_enabled(&self, indexer: &str, enabled: bool) -> Result<Option<Entry>, String> {
        HubAdmin::set_enabled(self, indexer, enabled).await
    }
    fn tasks(&self) -> serde_json::Value {
        HubAdmin::tasks(self)
    }
    async fn task_history(&self) -> Result<serde_json::Value, String> {
        HubAdmin::task_history(self).await
    }
    fn run_task(&self, id: &str) -> Option<serde_json::Value> {
        HubAdmin::run_task(self, id)
    }
    async fn movies(&self) -> Result<serde_json::Value, String> {
        HubAdmin::movies(self).await
    }
    fn search_missing(&self) -> Result<serde_json::Value, String> {
        HubAdmin::search_missing(self)
    }
    fn missing_status(&self) -> serde_json::Value {
        HubAdmin::missing_status(self)
    }
    async fn grab_movie(&self, movie_id: i64, apply: bool) -> Result<serde_json::Value, String> {
        HubAdmin::grab_movie(self, movie_id, apply).await
    }
    async fn configuration(&self) -> Result<serde_json::Value, String> {
        HubAdmin::configuration(self).await
    }
    async fn save_configuration(
        &self,
        values: BTreeMap<String, Option<String>>,
    ) -> Result<serde_json::Value, String> {
        HubAdmin::save_configuration(self, values).await
    }
    fn config_section(&self, section: &str) -> Result<serde_json::Value, String> {
        HubAdmin::config_section(self, section)
    }
    async fn save_config_section(
        &self,
        section: &str,
        value: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        HubAdmin::save_config_section(self, section, value).await
    }
}

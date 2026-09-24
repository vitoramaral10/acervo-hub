//! `sync`: cadastra os indexadores servidos nos gerenciadores de série e de
//! filme — o que o agregador atual faz pela tela de "apps".
//!
//! Só toca o que é seu: indexador cujo nome termina em ` (acervo-hub)`. O
//! resto — inclusive os que o agregador atual cadastrou — fica intocado, para
//! que os dois convivam durante a migração. Como no ciclo de limpeza, o plano
//! é o mesmo nos dois modos; `--apply` só decide se ele é executado.

use std::collections::HashMap;

use acervo_api::Entry;
use acervo_arr::{ArrClient, ArrKind, RemoteIndexer, TorznabSpec};
use acervo_core::InstanceName;
use acervo_indexers::Capabilities;
use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;

/// Marca dos indexadores que este serviço gerencia.
pub const SUFFIX: &str = " (acervo-hub)";

/// Anime tem campo próprio no gerenciador de séries.
const ANIME: u32 = 5070;

/// Folga para o cadastro, que espera o teste do indexador pela instância.
const SAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

#[derive(Debug)]
pub enum Action {
    Create(TorznabSpec),
    Update(RemoteIndexer, TorznabSpec),
    Delete(RemoteIndexer),
    Keep(String),
}

impl Action {
    fn describe(&self) -> String {
        let categories = |spec: &TorznabSpec| {
            spec.categories
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        match self {
            Self::Create(spec) => format!("criar     {} [{}]", spec.name, categories(spec)),
            Self::Update(_, spec) => format!("atualizar {} [{}]", spec.name, categories(spec)),
            Self::Delete(remote) => format!("remover   {}", remote.name),
            Self::Keep(name) => format!("manter    {name}"),
        }
    }
}

/// O que cada instância deve ter, a partir das capacidades de cada indexador.
///
/// Instância de séries recebe as categorias de TV; de filmes, as de filme.
/// Indexador sem nenhuma categoria da faixa não é cadastrado ali: cadastrado,
/// ele seria consultado em toda busca para não devolver nada.
#[must_use]
pub fn desired(
    kind: ArrKind,
    indexers: &[(String, Capabilities)],
    public_url: &str,
    api_key: &str,
) -> Vec<TorznabSpec> {
    let range = match kind {
        ArrKind::Series => 5000..6000,
        ArrKind::Movie => 2000..3000,
    };
    indexers
        .iter()
        .filter_map(|(name, caps)| {
            let mut categories: Vec<u32> = caps
                .categories
                .iter()
                .map(|category| category.id)
                .filter(|id| range.contains(id))
                .collect();
            categories.sort_unstable();
            categories.dedup();
            if categories.is_empty() {
                return None;
            }
            let anime_categories = match kind {
                ArrKind::Series => Some(
                    categories
                        .iter()
                        .copied()
                        .filter(|id| *id == ANIME)
                        .collect(),
                ),
                ArrKind::Movie => None,
            };
            Some(TorznabSpec {
                name: format!("{name}{SUFFIX}"),
                base_url: format!("{public_url}/{name}"),
                api_key: api_key.to_owned(),
                categories,
                anime_categories,
            })
        })
        .collect()
}

/// Compara o cadastrado com o desejado. Indexador sem a marca nunca aparece.
#[must_use]
pub fn plan(existing: Vec<RemoteIndexer>, desired: Vec<TorznabSpec>) -> Vec<Action> {
    let mut managed: HashMap<String, RemoteIndexer> = existing
        .into_iter()
        .filter(|remote| remote.name.ends_with(SUFFIX))
        .map(|remote| (remote.name.clone(), remote))
        .collect();
    let mut actions: Vec<Action> = desired
        .into_iter()
        .map(|spec| match managed.remove(&spec.name) {
            Some(remote) if remote.matches(&spec) => Action::Keep(spec.name),
            Some(remote) => Action::Update(remote, spec),
            None => Action::Create(spec),
        })
        .collect();
    let mut stale: Vec<RemoteIndexer> = managed.into_values().collect();
    stale.sort_by(|left, right| left.name.cmp(&right.name));
    actions.extend(stale.into_iter().map(Action::Delete));
    actions
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub aplicado: bool,
    pub instancias: Vec<InstanceSync>,
    pub falhas: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceSync {
    pub nome: String,
    pub tipo: &'static str,
    pub erro: Option<String>,
    pub acoes: Vec<ActionSync>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionSync {
    pub acao: &'static str,
    pub indexador: String,
    pub categorias: Vec<u32>,
    pub falha: Option<String>,
}

impl Action {
    fn sync_line(&self) -> ActionSync {
        let (acao, indexador, categorias) = match self {
            Self::Create(spec) => ("criar", spec.name.clone(), spec.categories.clone()),
            Self::Update(_, spec) => ("atualizar", spec.name.clone(), spec.categories.clone()),
            Self::Delete(remote) => ("remover", remote.name.clone(), Vec::new()),
            Self::Keep(name) => ("manter", name.clone(), Vec::new()),
        };
        ActionSync {
            acao,
            indexador,
            categorias,
            falha: None,
        }
    }
}

/// Planeja e, com `apply`, executa, para os indexadores dados.
///
/// Instância que não responde é relatada e pulada: as outras seguem.
///
/// # Errors
///
/// Configuração incompleta.
pub async fn execute(
    config: &Config,
    indexers: &[(String, Capabilities)],
    apply: bool,
    print: bool,
) -> Result<SyncReport> {
    let (server, public_url) = config.sync()?;
    let mut report = SyncReport {
        aplicado: apply,
        instancias: Vec::new(),
        falhas: 0,
    };
    for spec in &config.instances {
        let client = ArrClient::new(
            InstanceName::new(spec.name.clone()),
            &spec.url,
            &spec.api_key,
            spec.kind.into(),
            // Ao salvar, a instância testa o indexador antes de responder — e
            // o teste é uma busca real, que num tracker de várias páginas com
            // intervalo entre elas passa fácil de um minuto.
            config.http_timeout().max(SAVE_TIMEOUT),
        )
        .with_context(|| format!("montando o cliente da instância `{}`", spec.name))?;
        let mut instance = InstanceSync {
            nome: spec.name.clone(),
            tipo: match client.kind() {
                ArrKind::Series => "series",
                ArrKind::Movie => "filmes",
            },
            erro: None,
            acoes: Vec::new(),
        };
        let existing = match client.indexers().await {
            Ok(existing) => existing,
            Err(error) => {
                if print {
                    println!(
                        "{}: inalcançável ({}), nada feito",
                        spec.name,
                        error.short()
                    );
                }
                instance.erro = Some(error.short());
                report.falhas += 1;
                report.instancias.push(instance);
                continue;
            }
        };
        let wanted = desired(client.kind(), indexers, &public_url, &server.api_key);
        for action in plan(existing, wanted) {
            if print {
                println!("{}: {}", spec.name, action.describe());
            }
            let mut line = action.sync_line();
            if apply {
                let result = match &action {
                    Action::Create(spec) => client.create_indexer(spec).await,
                    Action::Update(remote, spec) => client.update_indexer(remote, spec).await,
                    Action::Delete(remote) => client.delete_indexer(remote.id).await,
                    Action::Keep(_) => Ok(()),
                };
                if let Err(error) = result {
                    if print {
                        println!("{}:   falhou: {}", spec.name, error.short());
                    }
                    line.falha = Some(error.short());
                    report.falhas += 1;
                }
            }
            instance.acoes.push(line);
        }
        report.instancias.push(instance);
    }
    if print && !apply {
        println!("Simulação: nada foi alterado. Use `sync --apply` para executar.");
    }
    Ok(report)
}

/// `sync` da CLI: os indexadores servidos, como `serve` os montaria.
///
/// # Errors
///
/// Configuração incompleta ou nenhum indexador utilizável.
pub async fn run(config: &Config, apply: bool) -> Result<usize> {
    config.sync()?;
    let indexers: Vec<(String, Capabilities)> = crate::serve::entries(config)
        .await?
        .into_iter()
        .map(
            |Entry {
                 indexer,
                 capabilities,
             }| (indexer.name().to_owned(), capabilities),
        )
        .collect();
    Ok(execute(config, &indexers, apply, true).await?.falhas)
}

#[cfg(test)]
mod tests {
    use acervo_indexers::Category;

    use super::*;

    fn caps(ids: &[u32]) -> Capabilities {
        Capabilities {
            categories: ids
                .iter()
                .map(|id| Category {
                    id: *id,
                    name: id.to_string(),
                    parent: None,
                })
                .collect(),
            ..Capabilities::default()
        }
    }

    fn indexers() -> Vec<(String, Capabilities)> {
        vec![
            ("misto".into(), caps(&[2000, 2040, 5000, 5040, 5070, 8000])),
            ("so-filmes".into(), caps(&[2000])),
        ]
    }

    #[test]
    fn cada_instancia_recebe_so_a_sua_faixa() {
        let series = desired(ArrKind::Series, &indexers(), "http://acervo:9797", "k");
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].name, "misto (acervo-hub)");
        assert_eq!(series[0].base_url, "http://acervo:9797/misto");
        assert_eq!(series[0].categories, [5000, 5040, 5070]);
        assert_eq!(series[0].anime_categories, Some(vec![5070]));

        let movies = desired(ArrKind::Movie, &indexers(), "http://acervo:9797", "k");
        let names: Vec<_> = movies.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, ["misto (acervo-hub)", "so-filmes (acervo-hub)"]);
        assert_eq!(movies[0].categories, [2000, 2040]);
        assert_eq!(movies[0].anime_categories, None);
    }

    fn remote(json: &str) -> RemoteIndexer {
        let client_side: Vec<RemoteIndexer> = serde_json::from_str::<Vec<serde_json::Value>>(json)
            .unwrap()
            .into_iter()
            .map(|raw| RemoteIndexer::from_raw(raw).unwrap())
            .collect();
        client_side.into_iter().next().unwrap()
    }

    #[test]
    fn plano_cria_atualiza_mantem_remove_e_ignora_o_que_nao_e_seu() {
        let wanted = desired(ArrKind::Movie, &indexers(), "http://acervo:9797", "k");
        let existing = vec![
            // Igual ao desejado: fica.
            remote(
                r#"[{"id": 1, "name": "misto (acervo-hub)", "implementation": "Torznab", "fields": [
                    {"name": "baseUrl", "value": "http://acervo:9797/misto"},
                    {"name": "apiPath", "value": "/api"},
                    {"name": "categories", "value": [2040, 2000]}]}]"#,
            ),
            // Não gerenciado: invisível ao plano.
            remote(
                r#"[{"id": 2, "name": "Tracker (Prowlarr)", "implementation": "Torznab", "fields": []}]"#,
            ),
            // Gerenciado e que saiu da configuração: removido.
            remote(
                r#"[{"id": 3, "name": "antigo (acervo-hub)", "implementation": "Torznab", "fields": []}]"#,
            ),
        ];
        let actions = plan(existing, wanted);
        let described: Vec<_> = actions.iter().map(Action::describe).collect();
        assert_eq!(
            described,
            [
                "manter    misto (acervo-hub)",
                "criar     so-filmes (acervo-hub) [2000]",
                "remover   antigo (acervo-hub)",
            ]
        );
    }

    #[test]
    fn url_diferente_vira_atualizacao() {
        let wanted = desired(ArrKind::Movie, &indexers()[..1], "http://novo:9797", "k");
        let existing = vec![remote(
            r#"[{"id": 1, "name": "misto (acervo-hub)", "implementation": "Torznab", "fields": [
                {"name": "baseUrl", "value": "http://acervo:9797/misto"},
                {"name": "apiPath", "value": "/api"},
                {"name": "categories", "value": [2000, 2040]}]}]"#,
        )];
        assert!(matches!(plan(existing, wanted)[0], Action::Update(..)));
    }
}

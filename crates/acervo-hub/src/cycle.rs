//! Um ciclo de limpeza completo, com relatório estruturado: o relatório vira
//! o detalhe da execução da tarefa `limpeza` no histórico.

use std::collections::BTreeMap;
use std::time::SystemTime;

use acervo_janitor::{Action, reconcile};
use anyhow::Result;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::Config;
use crate::{apply, collect, ledger};

#[derive(Debug, Clone, Serialize)]
pub struct CycleReport {
    pub quando: String,
    pub torrents: usize,
    pub ilegiveis: Vec<Unreadable>,
    pub biblioteca: String,
    /// Motivo, quando uma trava abortou o ciclo sem alterar nada.
    pub abortado: Option<String>,
    pub acoes: Vec<ActionLine>,
    pub espaco: String,
    pub pulados: Vec<SkippedLine>,
    pub executadas: Option<usize>,
    pub falharam: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Unreadable {
    pub nome: String,
    pub motivo: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionLine {
    pub tipo: &'static str,
    pub titulo: String,
    pub detalhe: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedLine {
    pub motivo: String,
    pub quantos: usize,
}

impl CycleReport {
    /// Uma linha para a lista de tarefas, e se o ciclo terminou bem. Ciclo
    /// abortado conta como erro na tela: o lote passou de uma trava de
    /// tamanho e alguém precisa olhar.
    #[must_use]
    pub fn summary(&self) -> (bool, String) {
        if let Some(reason) = &self.abortado {
            return (false, format!("abortado por trava: {reason}"));
        }
        if self.acoes.is_empty() {
            return (true, "nada a limpar".into());
        }
        let actions = match self.acoes.len() {
            1 => "1 ação".to_owned(),
            n => format!("{n} ações"),
        };
        let failed = self.falharam.unwrap_or(0);
        let line = if failed > 0 {
            format!("{actions}, {} liberados, {failed} falharam", self.espaco)
        } else {
            format!("{actions}, {} liberados", self.espaco)
        };
        (failed == 0, line)
    }
}

impl CycleReport {
    /// O que foi lido, antes de qualquer decisão.
    fn header(inventory: &acervo_core::Inventory) -> Self {
        Self {
            quando: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            torrents: inventory.downloads.len(),
            ilegiveis: inventory
                .unreadable
                .iter()
                .map(|unreadable| Unreadable {
                    nome: unreadable.name.clone(),
                    motivo: unreadable.reason.clone(),
                })
                .collect(),
            biblioteca: inventory.library_size.to_string(),
            abortado: None,
            acoes: Vec::new(),
            espaco: "0 B".into(),
            pulados: Vec::new(),
            executadas: None,
            falharam: None,
        }
    }
}

fn action_line(action: &Action) -> ActionLine {
    match action {
        Action::DeleteUnlinked { name, reclaim, .. } => ActionLine {
            tipo: "apagar-torrent",
            titulo: name.clone(),
            detalhe: format!("libera {reclaim}"),
        },
        Action::StrikeUnowned {
            name,
            strikes,
            limit,
            ..
        } => ActionLine {
            tipo: "strike-sem-dono",
            titulo: name.clone(),
            detalhe: format!("strike {strikes}/{limit}"),
        },
        Action::DeleteUnowned {
            name,
            delete_files,
            reclaim,
            ..
        } => ActionLine {
            tipo: "apagar-sem-dono",
            titulo: name.clone(),
            detalhe: if *delete_files {
                format!("remove do cliente e apaga os arquivos, libera {reclaim}")
            } else {
                "remove do cliente e preserva os arquivos".into()
            },
        },
    }
}

/// Roda um ciclo e aplica o plano.
///
/// # Errors
///
/// Configuração do ciclo incompleta, cliente de download fora do ar ou falha
/// ao ler ou gravar os strikes.
pub async fn run(config: &Config, store: &acervo_store::Store) -> Result<CycleReport> {
    let session = collect::collect(config, store).await?;
    let mut result = CycleReport::header(&session.inventory);
    let inventory = &session.inventory;

    let mut strikes = ledger::load(store).await?;
    let plan = match reconcile(
        inventory,
        &config.policy.to_policy(),
        &mut strikes,
        SystemTime::now(),
    ) {
        Ok(plan) => plan,
        Err(abort) => {
            // O lote passou de uma trava de tamanho: nenhuma ação se aplica.
            tracing::warn!("ciclo abortado: {abort}");
            result.abortado = Some(abort.to_string());
            return Ok(result);
        }
    };
    result.acoes = plan.actions.iter().map(action_line).collect();
    result.espaco = plan.reclaim.to_string();
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for skipped in &plan.skipped {
        *by_reason.entry(skipped.reason.to_string()).or_default() += 1;
    }
    result.pulados = by_reason
        .into_iter()
        .map(|(motivo, quantos)| SkippedLine { motivo, quantos })
        .collect();

    ledger::save(store, &strikes).await?;
    let outcome = apply::execute(&session, &plan).await;
    result.executadas = Some(outcome.done);
    result.falharam = Some(outcome.failed);
    Ok(result)
}

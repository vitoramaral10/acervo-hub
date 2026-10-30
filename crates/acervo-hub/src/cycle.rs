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
    /// O que saiu da tela Para apagar, e o que segue esperando o tracker.
    pub marcas: crate::marks::Due,
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
        let mut parts = Vec::new();
        let failed = self.falharam.unwrap_or(0);
        if !self.acoes.is_empty() {
            let actions = match self.acoes.len() {
                1 => "1 ação".to_owned(),
                n => format!("{n} ações"),
            };
            parts.push(if failed > 0 {
                format!("{actions}, {} liberados, {failed} falharam", self.espaco)
            } else {
                format!("{actions}, {} liberados", self.espaco)
            });
        }
        let mut marks_failed = 0;
        if let Some(purge) = &self.marcas.apagado {
            marks_failed = purge.falhas.len();
            if !purge.apagados.is_empty() {
                parts.push(format!("{} marcado(s) apagado(s)", purge.apagados.len()));
            }
            if marks_failed > 0 {
                parts.push(format!("{marks_failed} marcado(s) com falha"));
            }
        }
        if !self.marcas.esperando.is_empty() {
            parts.push(format!(
                "{} marcado(s) esperando o seed",
                self.marcas.esperando.len()
            ));
        }
        if let Some(error) = &self.marcas.erro {
            parts.push(format!("marcados não conferidos: {error}"));
        }
        if parts.is_empty() {
            return (true, "nada a limpar".into());
        }
        (
            failed == 0 && marks_failed == 0 && self.marcas.erro.is_none(),
            parts.join("; "),
        )
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
            marcas: crate::marks::Due::default(),
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
        &config.cleanup_policy(),
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

    // As marcas da tela Para apagar saem quando o tracker deixa. Falha aqui
    // não desfaz a limpeza dos torrents: vira aviso e tenta no próximo ciclo.
    match crate::marks::purge_due(config, store, inventory).await {
        Ok(due) => {
            if !due.esperando.is_empty() {
                tracing::info!(marcas = ?due.esperando, "marcados esperando o seed");
            }
            result.marcas = due;
        }
        Err(error) => {
            tracing::warn!("marcados não conferidos: {error:#}");
            result.marcas.erro = Some(format!("{error:#}"));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::{Due, Purge, Purged};

    fn report() -> CycleReport {
        CycleReport {
            quando: String::new(),
            torrents: 0,
            ilegiveis: Vec::new(),
            biblioteca: String::new(),
            abortado: None,
            acoes: Vec::new(),
            espaco: "0 B".into(),
            pulados: Vec::new(),
            executadas: Some(0),
            falharam: Some(0),
            marcas: Due::default(),
        }
    }

    #[test]
    fn resumo_conta_os_marcados() {
        assert_eq!(report().summary(), (true, "nada a limpar".into()));
        let mut r = report();
        r.marcas = Due {
            apagado: Some(Purge {
                apagados: vec![Purged {
                    chave: "filme-1".into(),
                    titulo: "Tenet (2020)".into(),
                    tamanho: 1,
                }],
                ..Purge::default()
            }),
            esperando: vec!["O Grinch (2018): o torrent ainda deve seed ao tracker".into()],
            erro: None,
        };
        assert_eq!(
            r.summary(),
            (
                true,
                "1 marcado(s) apagado(s); 1 marcado(s) esperando o seed".into()
            )
        );
    }

    #[test]
    fn marcas_nao_conferidas_nao_viram_nada_a_limpar() {
        let mut r = report();
        r.marcas.erro = Some("banco fora".into());
        assert_eq!(
            r.summary(),
            (false, "marcados não conferidos: banco fora".into())
        );
    }
}

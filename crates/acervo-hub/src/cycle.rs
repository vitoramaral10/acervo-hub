//! Um ciclo de limpeza completo, com relatório estruturado.
//!
//! A CLI imprime; a interface mostra o mesmo relatório. O caminho de código é
//! um só nos dois modos — o modo só decide se o último passo acontece.

use std::collections::BTreeMap;
use std::time::SystemTime;

use acervo_janitor::{Action, Mode, reconcile};
use anyhow::{Context, Result};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::{self, Config};
use crate::{apply, collect, ledger, report};

#[derive(Debug, Clone, Serialize)]
pub struct CycleReport {
    pub quando: String,
    pub modo: &'static str,
    pub instancias: Vec<InstanceLine>,
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
pub struct InstanceLine {
    pub nome: String,
    pub fila: Option<usize>,
    pub obras: Option<usize>,
    pub erro: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Unreadable {
    pub nome: String,
    pub motivo: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionLine {
    pub tipo: &'static str,
    pub instancia: Option<String>,
    pub titulo: String,
    pub detalhe: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedLine {
    pub motivo: String,
    pub quantos: usize,
}

impl CycleReport {
    /// Saída do processo: 3 é ciclo abortado por trava, que não é falha.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        if self.abortado.is_some() {
            3
        } else {
            u8::from(self.falharam.unwrap_or(0) > 0)
        }
    }
}

impl CycleReport {
    /// O que foi lido, antes de qualquer decisão.
    fn header(inventory: &acervo_core::Inventory, mode: Mode) -> Self {
        let mut instancias: Vec<InstanceLine> = inventory
            .snapshots
            .iter()
            .map(|snapshot| InstanceLine {
                nome: snapshot.instance.to_string(),
                fila: Some(snapshot.queue.len()),
                obras: Some(snapshot.known_works),
                erro: None,
            })
            .collect();
        instancias.extend(
            inventory
                .unreachable
                .iter()
                .map(|unreachable| InstanceLine {
                    nome: unreachable.instance.to_string(),
                    fila: None,
                    obras: None,
                    erro: Some(unreachable.reason.clone()),
                }),
        );
        Self {
            quando: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            modo: if mode == Mode::DryRun {
                "simulacao"
            } else {
                "aplicado"
            },
            instancias,
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
        Action::StrikeOrphan {
            instance,
            title,
            strikes,
            limit,
            ..
        } => ActionLine {
            tipo: "strike",
            instancia: Some(instance.to_string()),
            titulo: title.clone(),
            detalhe: format!("strike {strikes}/{limit}"),
        },
        Action::RemoveOrphan {
            instance,
            title,
            delete_files,
            ..
        } => ActionLine {
            tipo: "remover-da-fila",
            instancia: Some(instance.to_string()),
            titulo: title.clone(),
            detalhe: if *delete_files {
                "remove da fila e apaga os arquivos".into()
            } else {
                "remove da fila e preserva os arquivos".into()
            },
        },
        Action::DeleteUnlinked { name, reclaim, .. } => ActionLine {
            tipo: "apagar-torrent",
            instancia: None,
            titulo: name.clone(),
            detalhe: format!("libera {reclaim}"),
        },
    }
}

/// Roda um ciclo. `print` imprime o relato como a CLI sempre fez.
///
/// # Errors
///
/// Configuração do ciclo incompleta, cliente de download fora do ar ou falha
/// ao gravar os strikes.
pub async fn run(config: &Config, mode: Mode, print: bool) -> Result<CycleReport> {
    let session = collect::collect(config).await?;
    if print {
        report::inventory(&session.inventory);
    }
    let mut result = CycleReport::header(&session.inventory, mode);
    let inventory = &session.inventory;

    let ledger_path = config::expand_tilde(&config.state.ledger);
    let mut strikes = ledger::load(&ledger_path)?;
    let plan = match reconcile(
        inventory,
        &config.policy.to_policy(mode),
        &mut strikes,
        SystemTime::now(),
    ) {
        Ok(plan) => plan,
        Err(abort) => {
            // Abortar é resultado esperado, não defeito: a leitura do mundo não
            // estava confiável.
            tracing::warn!("ciclo abortado: {abort}");
            if print {
                println!("Ciclo abortado: {abort}");
                println!("Nenhuma alteração foi feita.");
            }
            result.abortado = Some(abort.to_string());
            return Ok(result);
        }
    };
    if print {
        report::plan(&plan);
    }
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

    if mode == Mode::DryRun {
        // Simulação não avança strike: se avançasse, repetir a simulação
        // levaria o item ao limite sem ninguém ter decidido nada.
        if print {
            println!("Simulação: strikes não foram gravados.");
        }
        return Ok(result);
    }

    ledger::save(&ledger_path, &strikes)
        .with_context(|| format!("gravando os strikes em `{}`", ledger_path.display()))?;
    let outcome = apply::execute(&session, &plan).await;
    if print {
        println!(
            "Executadas {} ações, {} falharam.",
            outcome.done, outcome.failed
        );
    }
    result.executadas = Some(outcome.done);
    result.falharam = Some(outcome.failed);
    Ok(result)
}

/// Grava o relatório como o "último ciclo" que a interface mostra. Falha aqui
/// não derruba o ciclo: o trabalho já foi feito, só o relato ficou sem cópia.
pub fn record(config: &Config, report: &CycleReport) {
    let path = config.state.last_cycle();
    let written = serde_json::to_string_pretty(report)
        .map_err(anyhow::Error::from)
        .and_then(|text| crate::credentials::write_atomic(&path, &text));
    if let Err(error) = written {
        tracing::warn!(path = %path.display(), "relatório do ciclo não gravado: {error:#}");
    }
}

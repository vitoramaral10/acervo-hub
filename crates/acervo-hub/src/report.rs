//! Relato do plano, em texto.

use std::collections::BTreeMap;

use acervo_core::Inventory;
use acervo_janitor::{Action, Mode, Plan};

/// Imprime o que foi lido, antes de qualquer decisão.
pub fn inventory(inv: &Inventory) {
    println!("Inventário");
    for s in &inv.snapshots {
        println!(
            "  {:<12} {:>4} na fila, {:>5} obras conhecidas",
            s.instance.to_string(),
            s.queue.len(),
            s.known_works
        );
    }
    for u in &inv.unreachable {
        println!(
            "  {:<12} INALCANÇÁVEL — {}",
            u.instance.to_string(),
            u.reason
        );
    }
    println!(
        "  {:<12} {} torrents legíveis, {} ilegíveis, biblioteca {}",
        "cliente",
        inv.downloads.len(),
        inv.unreadable.len(),
        inv.library_size
    );

    for u in &inv.unreadable {
        println!("    ilegível: {} — {}", u.name, u.reason);
    }
    println!();
}

/// Imprime o plano.
pub fn plan(plan: &Plan) {
    let rotulo = if plan.mode == Mode::DryRun {
        "Plano (simulação — nada será alterado)"
    } else {
        "Plano (será aplicado)"
    };
    println!("{rotulo}");

    if plan.actions.is_empty() {
        println!("  nada a fazer");
    }

    for action in &plan.actions {
        match action {
            Action::StrikeOrphan {
                instance,
                title,
                strikes,
                limit,
                ..
            } => println!("  strike {strikes}/{limit}  [{instance}] {title}"),
            Action::RemoveOrphan {
                instance,
                title,
                delete_files,
                ..
            } => {
                let sufixo = if *delete_files {
                    "e apaga os arquivos"
                } else {
                    "e preserva os arquivos"
                };
                println!("  remove da fila  [{instance}] {title} — {sufixo}");
            }
            Action::DeleteUnlinked { name, reclaim, .. } => {
                println!("  apaga torrent   {name} — libera {reclaim}");
            }
        }
    }

    println!("\n  espaço a liberar: {}", plan.reclaim);

    if !plan.skipped.is_empty() {
        let mut por_motivo: BTreeMap<String, usize> = BTreeMap::new();
        for s in &plan.skipped {
            *por_motivo.entry(s.reason.to_string()).or_default() += 1;
        }
        println!("\n  pulados:");
        for (motivo, quantos) in por_motivo {
            println!("    {quantos:>4}  {motivo}");
        }
    }
    println!();
}

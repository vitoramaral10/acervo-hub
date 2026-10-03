//! Relato do plano, em texto.

use std::collections::BTreeMap;

use acervo_core::Inventory;
use acervo_janitor::{Action, Plan};

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
    println!("Plano (será aplicado)");

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
            Action::StrikeUnowned {
                name,
                strikes,
                limit,
                ..
            } => println!("  strike {strikes}/{limit}  [sem dono] {name}"),
            Action::DeleteUnowned {
                name,
                delete_files,
                reclaim,
                ..
            } => {
                let sufixo = if *delete_files {
                    format!("e apaga os arquivos, libera {reclaim}")
                } else {
                    "e preserva os arquivos".into()
                };
                println!("  apaga torrent   {name} (sem dono) — {sufixo}");
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

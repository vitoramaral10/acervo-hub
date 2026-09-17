//! O planejador. Função pura: inventário + política + strikes → plano.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use acervo_core::{Allocated, Download, Inventory, QueueItem};

use crate::plan::{Abort, Action, Plan, SkipReason, Skipped};
use crate::policy::Policy;
use crate::strike::{StrikeKey, StrikeLedger};

/// Planeja um ciclo de reconciliação.
///
/// Devolve [`Abort`] quando a leitura do mundo não é confiável — e aí nenhuma
/// ação é devolvida, nem as que pareciam seguras. Esta é a diferença entre
/// "não apagou o que devia" e "apagou o que não devia".
///
/// # Errors
///
/// Ver [`Abort`]: instância fora do ar, inventário vazio, ou lote acima das
/// travas de tamanho.
pub fn reconcile(
    inv: &Inventory,
    policy: &Policy,
    ledger: &mut StrikeLedger,
    now: SystemTime,
) -> Result<Plan, Abort> {
    check_inventory_is_trustworthy(inv)?;

    let mut actions = Vec::new();
    let mut skipped = Vec::new();
    let mut reclaim = Allocated::ZERO;

    plan_orphaned_queue(
        inv,
        policy,
        ledger,
        &mut actions,
        &mut skipped,
        &mut reclaim,
    );
    plan_unlinked_downloads(inv, policy, now, &mut actions, &mut skipped, &mut reclaim);

    check_batch_is_within_limits(reclaim, inv.library_size, policy)?;

    Ok(Plan {
        mode: policy.mode,
        actions,
        skipped,
        reclaim,
    })
}

/// Travas de leitura: rodam antes de qualquer decisão.
fn check_inventory_is_trustworthy(inv: &Inventory) -> Result<(), Abort> {
    if let Some(down) = inv.unreachable.first() {
        return Err(Abort::InstanceUnreachable {
            instance: down.instance.clone(),
            reason: down.reason.clone(),
        });
    }

    for snapshot in &inv.snapshots {
        if snapshot.known_works == 0 {
            return Err(Abort::EmptyInventory {
                instance: snapshot.instance.clone(),
            });
        }
    }

    Ok(())
}

/// Travas de tamanho: rodam depois, sobre o lote já montado.
fn check_batch_is_within_limits(
    reclaim: Allocated,
    library: Allocated,
    policy: &Policy,
) -> Result<(), Abort> {
    if library == Allocated::ZERO && reclaim > Allocated::ZERO {
        return Err(Abort::LibraryUnmeasured { reclaim });
    }

    if reclaim > policy.guards.max_batch {
        return Err(Abort::BatchTooLarge {
            reclaim,
            limit: policy.guards.max_batch,
        });
    }

    let fraction = reclaim.fraction_of(library);
    if fraction > policy.guards.max_batch_fraction {
        return Err(Abort::BatchFractionTooLarge {
            fraction,
            limit: policy.guards.max_batch_fraction,
        });
    }

    Ok(())
}

/// Passo 1: item de fila sem obra dona.
fn plan_orphaned_queue(
    inv: &Inventory,
    policy: &Policy,
    ledger: &mut StrikeLedger,
    actions: &mut Vec<Action>,
    skipped: &mut Vec<Skipped>,
    reclaim: &mut Allocated,
) {
    let by_hash = inv.downloads_by_hash();
    let mut seen = Vec::new();

    for item in inv.queue_items().filter(|i| i.is_orphaned()) {
        let download = item.download.as_ref().and_then(|h| by_hash.get(h).copied());

        if download.is_none() && policy.skip_orphan_if_missing_in_client {
            skipped.push(Skipped {
                what: item.title.clone(),
                reason: SkipReason::MissingInClient,
            });
            continue;
        }

        let key = StrikeKey::for_item(item);
        let strikes = ledger.strike(key.clone());
        seen.push(key);

        if strikes < policy.orphan_strikes {
            actions.push(Action::StrikeOrphan {
                item: item.id,
                instance: item.instance.clone(),
                title: item.title.clone(),
                strikes,
                limit: policy.orphan_strikes,
            });
            continue;
        }

        actions.push(removal_for(item, download, policy));
        if let Some(d) = download {
            if delete_files_for(d, policy) {
                *reclaim = *reclaim + d.reclaimable();
            }
        }
    }

    ledger.retain_only(&seen);
}

fn removal_for(item: &QueueItem, download: Option<&Download>, policy: &Policy) -> Action {
    Action::RemoveOrphan {
        item: item.id,
        instance: item.instance.clone(),
        title: item.title.clone(),
        download: item.download.clone(),
        delete_files: download.is_some_and(|d| delete_files_for(d, policy)),
    }
}

fn delete_files_for(download: &Download, policy: &Policy) -> bool {
    !download.private || policy.delete_private_orphans
}

/// Passo 2: seed fora de fila que perdeu o vínculo com a biblioteca.
///
/// Não há sobreposição com o passo 1, por construção: o que está em fila é
/// pulado aqui, e só seed entra na avaliação.
fn plan_unlinked_downloads(
    inv: &Inventory,
    policy: &Policy,
    now: SystemTime,
    actions: &mut Vec<Action>,
    skipped: &mut Vec<Skipped>,
    reclaim: &mut Allocated,
) {
    let queued: HashSet<_> = inv.hashes_in_any_queue().into_iter().collect();

    for download in &inv.downloads {
        let skip = evaluate_download(download, &queued, policy, now);

        if let Some(reason) = skip {
            skipped.push(Skipped {
                what: download.name.clone(),
                reason,
            });
            continue;
        }

        *reclaim = *reclaim + download.reclaimable();
        actions.push(Action::DeleteUnlinked {
            download: download.hash.clone(),
            name: download.name.clone(),
            reclaim: download.reclaimable(),
        });
    }
}

/// `None` significa "pode apagar".
fn evaluate_download(
    download: &Download,
    queued: &HashSet<&acervo_core::DownloadHash>,
    policy: &Policy,
    now: SystemTime,
) -> Option<SkipReason> {
    if queued.contains(&download.hash) {
        return Some(SkipReason::InQueue);
    }
    if !download.state.is_seeding() {
        return Some(SkipReason::NotSeeding);
    }
    if download.has_library_link() {
        return Some(SkipReason::StillLinked);
    }
    if modified_within(download, now, policy.guards.recent_change_grace) {
        return Some(SkipReason::RecentlyModified);
    }
    if let Some(grace) = policy.private_seed_grace {
        if download.private && download.seeded_for < grace {
            return Some(SkipReason::SeedGrace);
        }
    }
    None
}

/// Arquivo com data no futuro conta como recente: relógio torto não autoriza
/// remoção.
fn modified_within(download: &Download, now: SystemTime, grace: Duration) -> bool {
    download
        .last_modified()
        .is_some_and(|m| now.duration_since(m).map_or(true, |age| age < grace))
}

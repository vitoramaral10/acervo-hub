//! Execução do plano.

use acervo_janitor::{Action, Plan};

use crate::collect::Session;

/// Quantas ações foram executadas e quantas falharam.
#[derive(Debug, Default, Clone, Copy)]
pub struct Outcome {
    pub done: usize,
    pub failed: usize,
}

/// Executa as ações destrutivas do plano.
///
/// Uma falha **não** interrompe as demais: o plano já foi inteiramente
/// validado pelas travas, e parar no meio deixaria o acervo num estado parcial
/// que o próximo ciclo teria de redescobrir. Cada falha é registrada e contada.
///
/// `StrikeOrphan` não aparece aqui: strike não é ação remota, é estado local,
/// e já foi persistido pelo ledger.
pub async fn execute(session: &Session, plan: &Plan) -> Outcome {
    let mut outcome = Outcome::default();

    for action in &plan.actions {
        let result = match action {
            Action::StrikeOrphan { .. } => continue,
            Action::RemoveOrphan {
                item,
                instance,
                title,
                delete_files,
                ..
            } => {
                let Some(arr) = session.arrs.get(instance) else {
                    tracing::error!(%instance, "instância do plano não está na sessão");
                    outcome.failed += 1;
                    continue;
                };
                tracing::info!(%instance, %title, apaga_arquivos = delete_files, "removendo da fila");
                arr.remove_queue_item(*item, *delete_files)
                    .await
                    .map_err(|e| e.to_string())
            }
            Action::DeleteUnlinked {
                download,
                name,
                reclaim,
            } => {
                tracing::info!(torrent = %name, libera = %reclaim, "apagando torrent sem vínculo");
                session
                    .qbit
                    .delete(std::slice::from_ref(download), true)
                    .await
                    .map_err(|e| e.to_string())
            }
        };

        match result {
            Ok(()) => outcome.done += 1,
            Err(err) => {
                tracing::error!(erro = %err, "ação falhou");
                outcome.failed += 1;
            }
        }
    }

    outcome
}

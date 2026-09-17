//! Cenários de reconciliação tirados de falhas reais em produção.
//!
//! Cada teste aqui é a forma executável de um incidente. Quando um deles
//! quebrar, o incidente voltou.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use acervo_core::{
    Allocated, Apparent, Download, DownloadHash, DownloadState, FileFacts, InstanceName,
    InstanceSnapshot, Inventory, QueueItem, QueueItemId, UnreachableInstance,
};
use acervo_janitor::{Abort, Action, Mode, Policy, SkipReason, StrikeLedger, reconcile};

const HORA: Duration = Duration::from_secs(3600);
const GIB: u64 = 1024 * 1024 * 1024;

fn agora() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn arquivo(links: u64, bytes: u64, idade: Duration) -> FileFacts {
    FileFacts {
        path: PathBuf::from("/media/downloads/exemplo.mkv"),
        links,
        allocated: Allocated::from_bytes(bytes),
        apparent: Apparent::from_bytes(bytes),
        modified: agora() - idade,
    }
}

/// Seed antigo, fora de qualquer fila. O que varia entre os testes é o `links`.
fn seed(hash: &str, links: u64, bytes: u64) -> Download {
    Download {
        hash: DownloadHash::new(hash),
        name: format!("seed {hash}"),
        state: DownloadState::Seeding,
        private: false,
        ratio: 0.0,
        seeded_for: 500 * HORA,
        files: vec![arquivo(links, bytes, 90 * HORA)],
    }
}

fn snapshot(nome: &str, fila: Vec<QueueItem>) -> InstanceSnapshot {
    InstanceSnapshot {
        instance: InstanceName::new(nome),
        queue: fila,
        known_works: 134,
    }
}

fn item(id: i64, instancia: &str, hash: Option<&str>, obra: Option<i64>) -> QueueItem {
    QueueItem {
        id: QueueItemId(id),
        instance: InstanceName::new(instancia),
        title: format!("item {id}"),
        download: hash.map(DownloadHash::new),
        work: QueueItem::normalize_work(obra),
    }
}

fn politica_aplicando() -> Policy {
    Policy {
        mode: Mode::Apply,
        private_seed_grace: None,
        ..Policy::default()
    }
}

#[test]
fn acervo_inteiro_com_hardlink_nao_gera_nenhuma_remocao() {
    // O incidente: uma regra de tempo de seed colocou 129 de 148 seeds na mira
    // num único ciclo — ~1,5 TB de tracker privado. Todos tinham hardlink na
    // biblioteca, ou seja, liberariam zero byte. Só não aconteceu porque a
    // primeira volta foi em dry run.
    let mut inv = Inventory::new(Allocated::from_bytes(1743 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = (0..129)
        .map(|i| seed(&format!("hash{i}"), 2, 12 * GIB))
        .collect();

    let plano = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect("inventário íntegro não aborta");

    assert_eq!(plano.destructive().count(), 0);
    assert_eq!(plano.reclaim, Allocated::ZERO);
    assert_eq!(plano.skipped.len(), 129);
    assert!(
        plano
            .skipped
            .iter()
            .all(|s| s.reason == SkipReason::StillLinked)
    );
}

#[test]
fn instancia_fora_do_ar_aborta_o_ciclo_inteiro() {
    // O incidente: uma instância travou e o ciclo passou dois dias falhando.
    // O ponto do teste não é que ele falha — é que ele falha ANTES de decidir
    // qualquer coisa. Sem a fila dessa instância, o seed abaixo pareceria
    // órfão e seria apagado.
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot("series", vec![]));
    inv.unreachable.push(UnreachableInstance {
        instance: InstanceName::new("filmes"),
        reason: "timeout".into(),
    });
    inv.downloads = vec![seed("orfao", 1, 40 * GIB)];

    let erro = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect_err("instância fora do ar tem de abortar");

    assert!(matches!(erro, Abort::InstanceUnreachable { .. }));
}

#[test]
fn instancia_meio_viva_aborta_o_ciclo() {
    // Responde rápido e diz não conhecer nenhuma obra. Sem esta trava, todo o
    // acervo vira órfão de uma vez.
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(InstanceSnapshot {
        instance: InstanceName::new("filmes"),
        queue: vec![item(1, "filmes", Some("aa"), None)],
        known_works: 0,
    });

    let erro = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect_err("inventário vazio tem de abortar");

    assert!(matches!(erro, Abort::EmptyInventory { .. }));
}

#[test]
fn orfao_pausado_acumula_strikes_e_so_depois_sai() {
    // Órfão de fila é item sem obra dona, esteja pausado ou não — o caso que
    // uma regra de "travado" nunca pegava, porque pausado não trava.
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot(
        "filmes",
        vec![item(1, "filmes", Some("aa"), Some(0))],
    ));
    inv.downloads = vec![Download {
        state: DownloadState::Paused,
        ..seed("aa", 1, 4 * GIB)
    }];

    let politica = politica_aplicando();
    let mut ledger = StrikeLedger::new();

    for ciclo in 1..=2 {
        let plano = reconcile(&inv, &politica, &mut ledger, agora()).expect("sem abort");
        assert!(
            matches!(plano.actions.as_slice(), [Action::StrikeOrphan { strikes, .. }] if *strikes == ciclo),
            "ciclo {ciclo} devia só marcar strike, veio {:?}",
            plano.actions
        );
        assert_eq!(plano.reclaim, Allocated::ZERO);
    }

    let plano = reconcile(&inv, &politica, &mut ledger, agora()).expect("sem abort");
    assert!(matches!(
        plano.actions.as_slice(),
        [Action::RemoveOrphan {
            delete_files: true,
            ..
        }]
    ));
    assert_eq!(plano.reclaim, Allocated::from_bytes(4 * GIB));
}

#[test]
fn orfao_privado_sai_da_fila_mas_preserva_os_arquivos_por_padrao() {
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot(
        "filmes",
        vec![item(1, "filmes", Some("aa"), None)],
    ));
    inv.downloads = vec![Download {
        private: true,
        ..seed("aa", 1, 4 * GIB)
    }];

    let politica = Policy {
        orphan_strikes: 1,
        ..politica_aplicando()
    };

    let plano = reconcile(&inv, &politica, &mut StrikeLedger::new(), agora()).expect("sem abort");

    assert!(matches!(
        plano.actions.as_slice(),
        [Action::RemoveOrphan {
            delete_files: false,
            ..
        }]
    ));
    assert_eq!(plano.reclaim, Allocated::ZERO, "não apagou, não liberou");
}

#[test]
fn item_em_fila_nunca_e_avaliado_como_seed_solto() {
    // A separação entre os dois passos: quem está em fila é caso do passo 1.
    // Sem isso, um download legítimo em andamento entraria na limpeza.
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot(
        "filmes",
        vec![item(1, "filmes", Some("aa"), Some(42))],
    ));
    inv.downloads = vec![seed("aa", 1, 4 * GIB)];

    let plano = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect("sem abort");

    assert!(plano.is_empty());
    assert_eq!(plano.skipped.len(), 1);
    assert_eq!(plano.skipped[0].reason, SkipReason::InQueue);
}

#[test]
fn arquivo_mexido_na_ultima_janela_e_intocavel() {
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![Download {
        files: vec![arquivo(1, 4 * GIB, HORA)],
        ..seed("aa", 1, 4 * GIB)
    }];

    let plano = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect("sem abort");

    assert!(plano.is_empty());
    assert_eq!(plano.skipped[0].reason, SkipReason::RecentlyModified);
}

#[test]
fn lote_grande_demais_aborta_em_vez_de_apagar_parte() {
    // Trava proporcional: 40 GB sem vínculo numa biblioteca de 100 GB são 40%,
    // acima do teto de 30%. Aborta o ciclo inteiro, não apaga "só um pouco".
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![seed("aa", 1, 40 * GIB)];

    let erro = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect_err("lote acima do teto tem de abortar");

    assert!(matches!(erro, Abort::BatchFractionTooLarge { .. }));
}

#[test]
fn dry_run_produz_o_mesmo_plano_do_modo_real() {
    // O modo não é um ramo de código: é um campo do plano. Se divergisse, o
    // dry run deixaria de valer como validação.
    let mut inv = Inventory::new(Allocated::from_bytes(1000 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![seed("aa", 1, 40 * GIB)];

    let seco = reconcile(
        &inv,
        &Policy {
            mode: Mode::DryRun,
            ..politica_aplicando()
        },
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect("sem abort");
    let real = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect("sem abort");

    assert_eq!(seco.actions, real.actions);
    assert_eq!(seco.reclaim, real.reclaim);
    assert!(seco.mode.is_dry_run());
    assert!(!real.mode.is_dry_run());
}

#[test]
fn biblioteca_que_mede_zero_aborta_em_vez_de_liberar_a_trava() {
    // Raiz não montada mede zero, e zero no denominador faria qualquer lote
    // parecer 0% do acervo — desligando a trava proporcional exatamente no
    // cenário em que tudo parece órfão.
    let mut inv = Inventory::new(Allocated::ZERO);
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![seed("aa", 1, 4 * GIB)];

    let erro = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect_err("biblioteca sem medida tem de abortar");

    assert!(matches!(erro, Abort::LibraryUnmeasured { .. }));
}

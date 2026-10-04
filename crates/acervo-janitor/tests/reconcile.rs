//! Cenários de reconciliação tirados de falhas reais em produção.
//!
//! Cada teste aqui é a forma executável de um incidente. Quando um deles
//! quebrar, o incidente voltou.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use acervo_core::{
    Allocated, Apparent, Download, DownloadHash, DownloadState, FileFacts, InstanceName,
    InstanceSnapshot, Inventory, QueueItem, QueueItemId, UnreachableInstance, WorkId,
};
use acervo_janitor::{Abort, Action, Policy, SkipReason, StrikeLedger, reconcile};

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
        category: "tv-sonarr".into(),
        ratio: 0.0,
        seeded_for: 500 * HORA,
        idle_for: None,
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
        work: obra.map(WorkId),
    }
}

fn politica_aplicando() -> Policy {
    Policy {
        private_seed_grace: None,
        managed_categories: vec!["tv-sonarr".into(), "radarr".into()],
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
fn item_em_fila_nunca_e_avaliado_como_seed_solto() {
    // Quem está na fila do acervo é download em andamento: sem esta
    // separação, ele entraria na limpeza.
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

#[test]
fn download_manual_sem_hardlink_nunca_e_apagado() {
    // Curso, ISO, qualquer coisa baixada à mão: não está na biblioteca porque
    // nunca esteve, e não é a limpeza que decide o destino dela.
    let mut manual = seed("manual", 1, 50 * GIB);
    manual.category = "cursos".into();
    let mut inv = Inventory::new(Allocated::from_bytes(1743 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![manual];

    let plano = reconcile(
        &inv,
        &politica_aplicando(),
        &mut StrikeLedger::new(),
        agora(),
    )
    .expect("sem abort");

    assert!(plano.actions.is_empty());
    assert_eq!(plano.skipped[0].reason, SkipReason::UnmanagedCategory);
}

#[test]
fn sem_categorias_gerenciadas_a_regra_de_hardlink_nao_apaga_nada() {
    let mut inv = Inventory::new(Allocated::from_bytes(1743 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![seed("orfao", 1, GIB)];
    let politica = Policy {
        managed_categories: Vec::new(),
        ..politica_aplicando()
    };

    let plano = reconcile(&inv, &politica, &mut StrikeLedger::new(), agora()).expect("sem abort");

    assert!(plano.actions.is_empty());
}

/// Download incompleto, sem fila e sem hardlink: o que sobra quando o dono sai
/// de cena. Antigo o bastante para passar da carência.
fn sem_dono(hash: &str, bytes: u64) -> Download {
    Download {
        state: DownloadState::Paused,
        seeded_for: Duration::ZERO,
        ..seed(hash, 1, bytes)
    }
}

fn inventario_com(downloads: Vec<Download>, fila: Vec<QueueItem>) -> Inventory {
    let mut inv = Inventory::new(Allocated::from_bytes(1000 * GIB));
    inv.snapshots.push(snapshot("acervo", fila));
    inv.downloads = downloads;
    inv
}

fn strikes_de_sem_dono(plano: &acervo_janitor::Plan) -> Vec<u32> {
    plano
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::StrikeUnowned { strikes, .. } => Some(*strikes),
            _ => None,
        })
        .collect()
}

#[test]
fn torrent_de_grab_em_andamento_nunca_e_candidato_nem_parado() {
    // O incidente a evitar: grab em andamento parado no cliente (sem espaço,
    // tag da fila) lido como "sem dono" e apagado no terceiro ciclo.
    let inv = inventario_com(
        vec![sem_dono("emandamento", 10 * GIB)],
        vec![item(1, "acervo", Some("emandamento"), Some(7))],
    );
    let mut ledger = StrikeLedger::new();

    for _ in 0..5 {
        let plano = reconcile(&inv, &politica_aplicando(), &mut ledger, agora())
            .expect("inventário íntegro não aborta");
        assert!(plano.actions.is_empty());
    }
    assert!(ledger.is_empty());
}

#[test]
fn incompleto_sem_grab_leva_strike_e_so_sai_no_enesimo_ciclo() {
    let inv = inventario_com(vec![sem_dono("largado", 10 * GIB)], vec![]);
    let politica = politica_aplicando();
    let mut ledger = StrikeLedger::new();

    for ciclo in 1..politica.orphan_strikes {
        let plano = reconcile(&inv, &politica, &mut ledger, agora()).unwrap();
        assert_eq!(strikes_de_sem_dono(&plano), [ciclo]);
        assert_eq!(plano.destructive().count(), 0);
        assert_eq!(plano.reclaim, Allocated::ZERO);
    }

    let plano = reconcile(&inv, &politica, &mut ledger, agora()).unwrap();
    assert!(matches!(
        plano.actions.as_slice(),
        [Action::DeleteUnowned {
            delete_files: true,
            ..
        }]
    ));
    assert_eq!(plano.reclaim, Allocated::from_bytes(10 * GIB));
}

#[test]
fn incompleto_que_volta_a_ter_grab_perde_os_strikes() {
    let politica = politica_aplicando();
    let mut ledger = StrikeLedger::new();
    let solto = inventario_com(vec![sem_dono("volta", GIB)], vec![]);
    let dono = inventario_com(
        vec![sem_dono("volta", GIB)],
        vec![item(1, "acervo", Some("volta"), Some(7))],
    );

    reconcile(&solto, &politica, &mut ledger, agora()).unwrap();
    reconcile(&solto, &politica, &mut ledger, agora()).unwrap();
    reconcile(&dono, &politica, &mut ledger, agora()).unwrap();
    assert!(ledger.is_empty());

    // Recomeça do um: os dois strikes de antes não valem mais.
    let plano = reconcile(&solto, &politica, &mut ledger, agora()).unwrap();
    assert_eq!(strikes_de_sem_dono(&plano), [1]);
}

#[test]
fn categoria_fora_das_gerenciadas_nunca_e_candidata() {
    let mut manual = sem_dono("manual", 10 * GIB);
    manual.category = "manual".into();
    let inv = inventario_com(vec![manual], vec![]);
    let mut ledger = StrikeLedger::new();

    for _ in 0..5 {
        let plano = reconcile(&inv, &politica_aplicando(), &mut ledger, agora()).unwrap();
        assert!(plano.actions.is_empty());
    }
    assert!(ledger.is_empty());
}

#[test]
fn incompleto_com_hardlink_nunca_e_candidato() {
    let mut ligado = sem_dono("ligado", 10 * GIB);
    ligado.files = vec![arquivo(2, 10 * GIB, 90 * HORA)];
    let inv = inventario_com(vec![ligado], vec![]);
    let mut ledger = StrikeLedger::new();

    for _ in 0..5 {
        let plano = reconcile(&inv, &politica_aplicando(), &mut ledger, agora()).unwrap();
        assert!(plano.actions.is_empty());
        assert!(
            plano
                .skipped
                .iter()
                .any(|s| s.reason == SkipReason::StillLinked)
        );
    }
    assert!(ledger.is_empty());
}

#[test]
fn incompleto_mexido_na_janela_de_carencia_e_intocavel() {
    let mut recente = sem_dono("recente", 10 * GIB);
    recente.files = vec![arquivo(1, 10 * GIB, HORA)];
    let inv = inventario_com(vec![recente], vec![]);
    let mut ledger = StrikeLedger::new();

    let plano = reconcile(&inv, &politica_aplicando(), &mut ledger, agora()).unwrap();
    assert!(plano.actions.is_empty());
    assert!(ledger.is_empty());
}

#[test]
fn privado_sem_dono_sai_do_cliente_sem_apagar_arquivo() {
    let mut privado = sem_dono("privado", 10 * GIB);
    privado.private = true;
    let inv = inventario_com(vec![privado], vec![]);
    let politica = Policy {
        orphan_strikes: 1,
        delete_private_orphans: false,
        ..politica_aplicando()
    };

    let plano = reconcile(&inv, &politica, &mut StrikeLedger::new(), agora()).unwrap();

    assert!(matches!(
        plano.actions.as_slice(),
        [Action::DeleteUnowned {
            delete_files: false,
            ..
        }]
    ));
    // Sem apagar arquivo, nada é liberado e nada entra na conta das travas.
    assert_eq!(plano.reclaim, Allocated::ZERO);
}

#[test]
fn lote_de_downloads_sem_dono_grande_demais_aborta() {
    // 20 incompletos de 20 GiB: 400 GiB passam do teto absoluto de 300 GiB.
    let inv = inventario_com(
        (0..20)
            .map(|i| sem_dono(&format!("h{i}"), 20 * GIB))
            .collect(),
        vec![],
    );
    let politica = Policy {
        orphan_strikes: 1,
        ..politica_aplicando()
    };

    let erro = reconcile(&inv, &politica, &mut StrikeLedger::new(), agora())
        .expect_err("lote acima do teto tem de abortar");

    assert!(matches!(erro, Abort::BatchTooLarge { .. }));
}

// ---- seed privado sem vínculo: ratio alvo, ociosidade ou teto

fn horas(n: u64) -> Duration {
    Duration::from_secs(n * 3600)
}

/// Privado, sem hardlink, com os números de seed que o teste quer.
fn privado(ratio: f64, seeded_h: u64, idle_h: Option<u64>) -> Download {
    Download {
        private: true,
        ratio,
        seeded_for: horas(seeded_h),
        idle_for: idle_h.map(horas),
        ..seed("pv", 1, 4 * GIB)
    }
}

/// Política com os padrões de seed (ratio 1.0, 24 h ocioso, teto de 120 h).
fn politica_de_seed() -> Policy {
    Policy {
        managed_categories: vec!["tv-sonarr".into()],
        ..Policy::default()
    }
}

/// Roda a limpeza e diz se o seed saiu.
fn sai(download: Download, politica: &Policy) -> bool {
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![download];
    let plano = reconcile(&inv, politica, &mut StrikeLedger::new(), agora()).expect("sem abort");
    if plano.actions.is_empty() {
        assert!(
            plano
                .skipped
                .iter()
                .any(|s| s.reason == SkipReason::SeedGrace),
            "ficou por outro motivo: {:?}",
            plano.skipped
        );
        false
    } else {
        true
    }
}

#[test]
fn privado_com_ratio_alvo_sai_mesmo_com_uma_hora_de_seed() {
    assert!(sai(privado(1.0, 1, Some(0)), &politica_de_seed()));
}

#[test]
fn privado_ocioso_ha_25_horas_sai() {
    assert!(sai(privado(0.1, 30, Some(25)), &politica_de_seed()));
}

#[test]
fn privado_ocioso_ha_23_horas_com_ratio_baixo_fica() {
    assert!(!sai(privado(0.5, 30, Some(23)), &politica_de_seed()));
}

#[test]
fn ociosidade_desconhecida_nao_conta_como_ociosa() {
    assert!(!sai(privado(0.5, 30, None), &politica_de_seed()));
}

#[test]
fn privado_no_teto_de_120_horas_sai() {
    assert!(sai(privado(0.1, 120, Some(0)), &politica_de_seed()));
}

#[test]
fn com_as_duas_condicoes_desligadas_so_o_teto_vale() {
    let politica = Policy {
        private_seed_ratio: None,
        private_seed_idle: None,
        ..politica_de_seed()
    };
    assert!(!sai(privado(5.0, 119, Some(100)), &politica));
    assert!(sai(privado(0.0, 120, Some(0)), &politica));
}

#[test]
fn publico_continua_sem_carencia() {
    let publico = Download {
        private: false,
        ..privado(0.0, 0, Some(0))
    };
    assert!(sai(publico, &politica_de_seed()));
}

#[test]
fn privado_com_hardlink_nunca_sai() {
    let mut inv = Inventory::new(Allocated::from_bytes(100 * GIB));
    inv.snapshots.push(snapshot("filmes", vec![]));
    inv.downloads = vec![Download {
        files: vec![arquivo(2, 4 * GIB, 90 * HORA)],
        ..privado(9.0, 500, Some(500))
    }];
    let plano =
        reconcile(&inv, &politica_de_seed(), &mut StrikeLedger::new(), agora()).expect("sem abort");
    assert!(plano.actions.is_empty());
}

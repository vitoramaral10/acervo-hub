//! A obrigação de seed que o tracker impõe, torrent a torrent.
//!
//! Alguns trackers (o Amigos, por exemplo) exigem que todo download concluído
//! semeie por um tempo **ou** até uma ratio, e que o que o usuário enviou ao
//! site semeie por mais tempo. Isso é independente do modo "construir ratio":
//! sem a tag de semear, a limpeza e os apagamentos automáticos ainda têm de
//! esperar a obrigação ser cumprida.
//!
//! A obrigação vem do indexador de onde o torrent veio:
//!
//! - download (grab de filme ou de série, mesmo o já removido do catálogo
//!   (`seed_debts`), torrent irmão achado pelo
//!   cross-seed): `seed_min_hours` OU `seed_min_ratio`;
//! - lançamento (o `.torrent` gerado, o do site e o irmão achado pelo
//!   cross-seed do par enviado): `upload_seed_hours`.
//!
//! Quando o mesmo hash aparece mais de uma vez (download e lançamento, ou
//! dois indexadores), as obrigações se somam: o torrent só sai quando todas se
//! cumprem (`SeedObligation::and`). Indexador sem registro ou sem obrigação
//! configurada não deve nada.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use acervo_clients::TorrentInfo;
use acervo_core::SeedObligation;
use acervo_store::{CrossSeedCheck, Grab, IndexerRecord, Launch, LaunchState, SeriesGrab, Store};
use anyhow::{Context, Result};

const HOUR: u64 = 3600;

fn hours(value: i32) -> Option<Duration> {
    u64::try_from(value)
        .ok()
        .filter(|h| *h > 0)
        .map(|h| Duration::from_secs(h * HOUR))
}

/// O que o indexador exige de um download concluído: tempo e/ou ratio, basta
/// um. `None` se não configurou nenhum.
pub(crate) fn download_obligation(record: &IndexerRecord) -> Option<SeedObligation> {
    let min_seed = record.seed_min_hours.and_then(hours);
    let min_ratio = record.seed_min_ratio.filter(|r| *r > 0.0);
    (min_seed.is_some() || min_ratio.is_some()).then_some(SeedObligation {
        min_seed,
        min_ratio,
    })
}

/// O que o indexador exige do torrent que o usuário lançou nele: só tempo.
pub(crate) fn upload_obligation(record: &IndexerRecord) -> Option<SeedObligation> {
    let min_seed = record.upload_seed_hours.and_then(hours)?;
    Some(SeedObligation {
        min_seed: Some(min_seed),
        min_ratio: None,
    })
}

/// Os hashes de um lançamento enviado, já resolvidos para a montagem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SentLaunch {
    pub indexer: String,
    /// Infohash do `.torrent` gerado, quando dá para lê-lo.
    pub generated: Option<String>,
    /// Infohash do `.torrent` baixado do site.
    pub site: Option<String>,
    /// Infohash do irmão achado pelo cross-seed do par.
    pub found: Option<String>,
}

/// A obrigação de seed de cada torrent do cliente, lida uma vez por coleta.
#[derive(Debug, Clone, Default)]
pub(crate) struct Obligations {
    by_hash: HashMap<String, SeedObligation>,
    launched: HashSet<String>,
}

impl Obligations {
    /// A obrigação do torrent; o hash vale em qualquer caixa.
    pub(crate) fn of(&self, hash: &str) -> Option<SeedObligation> {
        self.by_hash.get(&hash.to_ascii_lowercase()).copied()
    }

    /// O torrent é um lançamento: tem a tag de semear preservada mesmo com o
    /// modo ratio desligado.
    #[cfg(test)]
    pub(crate) fn is_launched(&self, hash: &str) -> bool {
        self.launched.contains(&hash.to_ascii_lowercase())
    }

    /// Todos os hashes de lançamento, em minúsculas.
    pub(crate) fn launched(&self) -> &HashSet<String> {
        &self.launched
    }
}

fn record<'a>(records: &'a [IndexerRecord], name: &str) -> Option<&'a IndexerRecord> {
    records.iter().find(|r| r.name == name)
}

/// Monta as obrigações a partir do que o banco guarda. Pura, para testar sem
/// banco. Cada papel do hash soma a sua obrigação às que já havia.
pub(crate) fn build(
    records: &[IndexerRecord],
    grabs: &[Grab],
    series_grabs: &[SeriesGrab],
    checks: &[CrossSeedCheck],
    debts: &[(String, String)],
    sent: &[SentLaunch],
) -> Obligations {
    let mut out = Obligations::default();
    let put = |hash: &str, obligation: Option<SeedObligation>, map: &mut Obligations| {
        if let Some(obligation) = obligation {
            map.by_hash
                .entry(hash.to_ascii_lowercase())
                .and_modify(|o| *o = o.and(obligation))
                .or_insert(obligation);
        }
    };
    let downloads = grabs
        .iter()
        .map(|g| (&g.hash, &g.indexer))
        .chain(series_grabs.iter().map(|g| (&g.hash, &g.indexer)));
    for (hash, indexer) in downloads {
        let obligation = record(records, indexer).and_then(download_obligation);
        put(hash, obligation, &mut out);
    }
    // Grabs de filme ou série removidos do catálogo: o torrent pode seguir no
    // cliente e a obrigação não some com a linha.
    for (hash, indexer) in debts {
        let obligation = record(records, indexer).and_then(download_obligation);
        put(hash, obligation, &mut out);
    }
    for check in checks {
        if let Some(found) = &check.found {
            let obligation = record(records, &check.indexer).and_then(download_obligation);
            put(found, obligation, &mut out);
        }
    }
    for launch in sent {
        let obligation = record(records, &launch.indexer).and_then(upload_obligation);
        for hash in [&launch.generated, &launch.site, &launch.found]
            .into_iter()
            .flatten()
        {
            put(hash, obligation, &mut out);
            out.launched.insert(hash.to_ascii_lowercase());
        }
    }
    out
}

/// Lê do banco tudo que decide a obrigação de cada torrent.
///
/// # Errors
///
/// Banco inalcançável: a limpeza não pode rodar sem saber o que o tracker
/// exige.
pub(crate) async fn load(store: &Store) -> Result<Obligations> {
    let records = store.indexers().await.context("lendo os indexadores")?;
    let grabs = store.grabs().await.context("lendo os grabs de filme")?;
    let series_grabs = store
        .series_grabs()
        .await
        .context("lendo os grabs de série")?;
    let checks = store
        .cross_seed_checks()
        .await
        .context("lendo os cross-seeds")?;
    let debts = store.seed_debts().await.context("lendo o seed devido")?;
    let launches = store.launches().await.context("lendo os lançamentos")?;
    let mut sent = Vec::new();
    for launch in launches.iter().filter(|l| is_sent(l)) {
        let generated = store
            .launch_torrent(&launch.hash, &launch.indexer)
            .await
            .context("lendo o torrent do lançamento")?
            .and_then(|bytes| acervo_clients::info_hash(&bytes));
        let found = checks
            .iter()
            .find(|c| c.hash == launch.hash && c.indexer == launch.indexer)
            .and_then(|c| c.found.clone());
        sent.push(SentLaunch {
            indexer: launch.indexer.clone(),
            generated,
            site: launch.site_hash.clone(),
            found,
        });
    }
    Ok(build(
        &records,
        &grabs,
        &series_grabs,
        &checks,
        &debts,
        &sent,
    ))
}

fn is_sent(launch: &Launch) -> bool {
    launch.sent_at.is_some() && launch.state != LaunchState::Descartado
}

/// A obrigação ainda não foi cumprida por um torrent já completo. Incompleto
/// não deve nada: a obrigação só começa quando o download conclui.
pub(crate) fn owed(obligation: Option<SeedObligation>, torrent: &TorrentInfo) -> bool {
    torrent.progress >= 1.0
        && obligation.is_some_and(|o| !o.met(torrent.seeded_for(), torrent.ratio))
}

/// O torrent de um grab do `indexer` ainda deve seed ao tracker. Banco que
/// falha conta como "deve": na dúvida o torrent fica.
pub(crate) async fn download_owed(store: &Store, indexer: &str, torrent: &TorrentInfo) -> bool {
    if torrent.progress < 1.0 {
        return false;
    }
    match store.indexers().await {
        Ok(records) => owed(
            record(&records, indexer).and_then(download_obligation),
            torrent,
        ),
        Err(error) => {
            tracing::warn!(
                indexer,
                "obrigação de seed ilegível; torrent completo fica no cliente: {error}"
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acervo_store::GrabState;

    const H: Duration = Duration::from_secs(HOUR);

    fn indexer(name: &str) -> IndexerRecord {
        IndexerRecord {
            name: name.into(),
            kind: "cardigann".into(),
            definition: None,
            url: None,
            settings: std::collections::BTreeMap::default(),
            enabled: true,
            priority: acervo_store::DEFAULT_INDEXER_PRIORITY,
            ratio_mode: false,
            seed_min_hours: None,
            seed_min_ratio: None,
            upload_seed_hours: None,
            added_at: None,
        }
    }

    fn amigos() -> IndexerRecord {
        IndexerRecord {
            seed_min_hours: Some(72),
            seed_min_ratio: Some(1.0),
            upload_seed_hours: Some(168),
            ..indexer("amigos")
        }
    }

    fn grab(hash: &str, indexer: &str, state: GrabState) -> Grab {
        Grab {
            id: 1,
            movie_id: 1,
            hash: hash.into(),
            title: "filme".into(),
            indexer: indexer.into(),
            quality: acervo_parser::Quality::Unknown,
            size: 1,
            grabbed_at: "2026-01-01T00:00:00Z".into(),
            state,
            message: None,
            imported_path: None,
            finished_at: None,
            replaces: None,
        }
    }

    fn check(hash: &str, indexer: &str, found: Option<&str>) -> CrossSeedCheck {
        CrossSeedCheck {
            hash: hash.into(),
            indexer: indexer.into(),
            checked_at: "2026-01-01T00:00:00Z".into(),
            attempts: 1,
            found: found.map(str::to_owned),
            outcome: acervo_store::CrossSeedOutcome::Found,
        }
    }

    #[test]
    fn download_pede_tempo_ou_ratio() {
        let o = download_obligation(&amigos()).unwrap();
        assert_eq!(o.min_seed, Some(72 * H));
        assert_eq!(o.min_ratio, Some(1.0));
    }

    #[test]
    fn download_so_com_ratio_ou_so_com_horas() {
        let so_ratio = IndexerRecord {
            seed_min_ratio: Some(2.0),
            ..indexer("a")
        };
        let o = download_obligation(&so_ratio).unwrap();
        assert_eq!((o.min_seed, o.min_ratio), (None, Some(2.0)));
        let so_horas = IndexerRecord {
            seed_min_hours: Some(24),
            ..indexer("a")
        };
        let o = download_obligation(&so_horas).unwrap();
        assert_eq!((o.min_seed, o.min_ratio), (Some(24 * H), None));
    }

    #[test]
    fn indexador_sem_configuracao_nao_deve_nada() {
        assert_eq!(download_obligation(&indexer("bj")), None);
        assert_eq!(upload_obligation(&indexer("bj")), None);
    }

    #[test]
    fn lancamento_so_exige_tempo() {
        let o = upload_obligation(&amigos()).unwrap();
        assert_eq!(o.min_seed, Some(168 * H));
        assert_eq!(o.min_ratio, None);
        // Só a obrigação de download configurada não vira a do envio.
        let so_download = IndexerRecord {
            upload_seed_hours: None,
            ..amigos()
        };
        assert_eq!(upload_obligation(&so_download), None);
    }

    #[test]
    fn grab_vale_por_indexador_em_qualquer_estado() {
        let records = [amigos(), indexer("bj")];
        let out = build(
            &records,
            &[
                grab("AA", "amigos", GrabState::Downloading),
                grab("bb", "amigos", GrabState::Imported),
                grab("cc", "amigos", GrabState::Failed),
                grab("dd", "bj", GrabState::Imported),
                grab("ee", "sumiu", GrabState::Imported),
            ],
            &[],
            &[],
            &[],
            &[],
        );
        assert_eq!(out.of("aa"), download_obligation(&amigos()));
        assert_eq!(out.of("AA"), download_obligation(&amigos()));
        assert!(out.of("bb").is_some());
        assert_eq!(out.of("cc"), download_obligation(&amigos()));
        assert_eq!(out.of("dd"), None);
        assert_eq!(out.of("ee"), None);
    }

    #[test]
    fn seed_devido_de_filme_removido_do_catalogo_segue_valendo() {
        let debts = [("OLD".to_owned(), "amigos".to_owned())];
        let out = build(&[amigos()], &[], &[], &[], &debts, &[]);
        assert_eq!(out.of("old"), download_obligation(&amigos()));
        assert!(!out.is_launched("old"));
    }

    #[test]
    fn lancamento_soma_ao_seed_devido() {
        let debts = [("site".to_owned(), "amigos".to_owned())];
        let sent = SentLaunch {
            indexer: "amigos".into(),
            generated: None,
            site: Some("site".into()),
            found: None,
        };
        let out = build(&[amigos()], &[], &[], &[], &debts, &[sent]);
        assert_eq!(out.of("site"), upload_obligation(&amigos()));
    }

    #[test]
    fn irmao_do_cross_seed_herda_a_obrigacao_de_download() {
        let out = build(
            &[amigos()],
            &[],
            &[],
            &[
                check("origem", "amigos", Some("IRMAO")),
                check("outro", "amigos", None),
            ],
            &[],
            &[],
        );
        assert_eq!(out.of("irmao"), download_obligation(&amigos()));
        assert_eq!(out.of("origem"), None);
        assert!(!out.is_launched("irmao"));
    }

    #[test]
    fn lancamento_soma_ao_download_do_mesmo_hash() {
        let sent = SentLaunch {
            indexer: "amigos".into(),
            generated: Some("GERADO".into()),
            site: Some("site".into()),
            found: Some("irmao".into()),
        };
        let out = build(
            &[amigos()],
            &[grab("gerado", "amigos", GrabState::Imported)],
            &[],
            &[check("origem", "amigos", Some("irmao"))],
            &[],
            &[sent],
        );
        let envio = upload_obligation(&amigos());
        for hash in ["gerado", "SITE", "irmao"] {
            assert_eq!(out.of(hash), envio, "{hash}");
            assert!(out.is_launched(hash), "{hash}");
        }
        assert!(!out.is_launched("origem"));
    }

    #[test]
    fn lancamento_sem_obrigacao_de_envio_mantem_a_de_download_e_a_marca() {
        let sem_envio = IndexerRecord {
            upload_seed_hours: None,
            ..amigos()
        };
        let sent = SentLaunch {
            indexer: "amigos".into(),
            generated: None,
            site: None,
            found: Some("irmao".into()),
        };
        let out = build(
            std::slice::from_ref(&sem_envio),
            &[],
            &[],
            &[check("origem", "amigos", Some("irmao"))],
            &[],
            &[sent],
        );
        assert_eq!(out.of("irmao"), download_obligation(&sem_envio));
        assert!(out.is_launched("irmao"));
    }

    #[test]
    fn owed_so_vale_para_completo_e_pendente() {
        let torrent = |progress: f64, seeding: i64, ratio: f64| {
            serde_json::from_value::<TorrentInfo>(serde_json::json!({
                "hash": "h", "name": "n", "state": "uploading", "save_path": "/",
                "progress": progress, "seeding_time": seeding, "ratio": ratio,
            }))
            .unwrap()
        };
        let o = download_obligation(&amigos());
        assert!(owed(o, &torrent(1.0, 3600, 0.1)));
        assert!(!owed(o, &torrent(1.0, 72 * 3600, 0.1)));
        assert!(!owed(o, &torrent(1.0, 3600, 1.0)));
        assert!(!owed(o, &torrent(0.5, 0, 0.0)));
        assert!(!owed(None, &torrent(1.0, 0, 0.0)));
    }

    #[test]
    fn mesmo_hash_em_dois_indexadores_vale_a_soma() {
        let mut longo = amigos();
        longo.name = "outro".into();
        longo.seed_min_hours = Some(240);
        longo.seed_min_ratio = None;
        let out = build(
            &[amigos(), longo],
            &[grab("h", "amigos", GrabState::Imported)],
            &[],
            &[],
            &[("h".to_owned(), "outro".to_owned())],
            &[],
        );
        assert_eq!(
            out.of("h"),
            Some(SeedObligation {
                min_seed: Some(Duration::from_secs(240 * HOUR)),
                min_ratio: None,
            })
        );
    }
}

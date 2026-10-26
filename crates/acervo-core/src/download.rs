//! O download no cliente de torrent, e os fatos de filesystem que decidem se
//! apagá-lo libera espaço.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::ids::DownloadHash;
use crate::size::{Allocated, Apparent};

/// Prefixo da tag que segura o torrent semeando: a limpeza automática não
/// apaga torrent terminado com uma tag começando por ele (o modo "construir
/// ratio" do indexador a aplica e tira; posta à mão, também vale). Remoção
/// pedida na tela apaga mesmo assim.
pub const SEED_TAG_PREFIX: &str = "acervo:semear:";

/// A tag de semear de um indexador. Vírgula separa tags no qBittorrent: vira hífen.
#[must_use]
pub fn seed_tag(indexer: &str) -> String {
    format!("{SEED_TAG_PREFIX}{}", indexer.replace(',', "-"))
}

/// Estado do torrent no cliente.
///
/// `Paused` é um estado próprio, e **não** é sinônimo de órfão: a maior parte
/// dos pausados é download desejado esperando liberar espaço. Sem grab em
/// andamento e sem hardlink, o item pode entrar na regra de download sem dono.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadState {
    Downloading,
    Paused,
    Seeding,
    Errored,
    /// O cliente não acha os arquivos (`missingFiles`): disco fora ou caminho
    /// errado. Não diz que o dado sumiu, diz que não dá para ver — e sem ver
    /// o dado, "sem hardlink" é falso.
    MissingFiles,
    Unknown,
}

impl DownloadState {
    /// Só torrent em seeding entra na avaliação de limpeza. Download em
    /// andamento ou ainda não importado nunca corre risco.
    #[must_use]
    pub const fn is_seeding(self) -> bool {
        matches!(self, Self::Seeding)
    }
}

/// Fatos de `stat(2)` sobre um arquivo do download.
#[derive(Debug, Clone)]
pub struct FileFacts {
    pub path: PathBuf,
    /// `st_nlink`. Maior que 1 significa que a biblioteca compartilha o inode.
    pub links: u64,
    /// `st_blocks * 512`.
    pub allocated: Allocated,
    /// `st_size`, guardado só para exibição.
    pub apparent: Apparent,
    pub modified: SystemTime,
}

impl FileFacts {
    /// Se a biblioteca aponta para o mesmo inode, apagar o torrent libera zero.
    #[must_use]
    pub const fn is_shared(&self) -> bool {
        self.links > 1
    }
}

/// O que o tracker exige de seed num download concluído. Cada limite é
/// opcional e basta alcançar **um** deles para a obrigação estar cumprida
/// ("72 h de seed OU ratio 1.0").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedObligation {
    /// Tempo mínimo de seed. `None` não exige tempo.
    pub min_seed: Option<Duration>,
    /// Ratio mínimo. `None` não exige ratio.
    pub min_ratio: Option<f64>,
}

impl SeedObligation {
    /// Cumprida quando qualquer limite definido foi alcançado. Sem nenhum
    /// limite definido não há o que cumprir.
    #[must_use]
    pub fn met(&self, seeded_for: Duration, ratio: f64) -> bool {
        if self.min_seed.is_none() && self.min_ratio.is_none() {
            return true;
        }
        self.min_seed.is_some_and(|min| seeded_for >= min)
            || self.min_ratio.is_some_and(|min| ratio >= min)
    }

    /// Junta as obrigações de dois trackers sobre o mesmo torrent: o
    /// resultado só se cumpre quando as duas se cumprem. Um limite só serve de
    /// saída se as duas o têm, e vale o maior. Tempo de um e só ratio do outro
    /// não cabe num "OU"; aí fica o maior tempo, que sempre acaba liberando.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        let both = |a: Option<Duration>, b: Option<Duration>| a.zip(b).map(|(a, b)| a.max(b));
        let min_seed = both(self.min_seed, other.min_seed);
        let min_ratio = self.min_ratio.zip(other.min_ratio).map(|(a, b)| a.max(b));
        if min_seed.is_none() && min_ratio.is_none() {
            return Self {
                min_seed: self.min_seed.max(other.min_seed),
                min_ratio: None,
            };
        }
        Self {
            min_seed,
            min_ratio,
        }
    }
}

/// Um torrent no cliente de download.
#[derive(Debug, Clone)]
pub struct Download {
    pub hash: DownloadHash,
    pub name: String,
    pub state: DownloadState,
    /// Tracker privado. Muda a política: hit&run custa acesso ao tracker.
    pub private: bool,
    /// Categoria no cliente. Só as categorias gerenciadas entram na limpeza;
    /// as outras são downloads manuais, fora da conta do acervo.
    pub category: String,
    /// Tags no cliente, já separadas.
    pub tags: Vec<String>,
    /// Baixou por inteiro. Torrent parado ou com erro a meio caminho não semeia.
    pub complete: bool,
    pub ratio: f64,
    pub seeded_for: Duration,
    /// Tempo desde a última transferência. `None` é desconhecido, e
    /// desconhecido nunca conta como ocioso.
    pub idle_for: Option<Duration>,
    /// Seed que o tracker exige deste torrent (download concluído ou
    /// lançamento). `None`: o tracker não exige nada.
    pub obligation: Option<SeedObligation>,
    pub files: Vec<FileFacts>,
}

impl Download {
    /// Algum arquivo ainda é compartilhado com a biblioteca.
    ///
    /// Esta é a invariante central da limpeza: **só sai o que já saiu da
    /// biblioteca**. Ratio e tempo de seed são critério secundário, nunca
    /// gatilho.
    #[must_use]
    pub fn has_library_link(&self) -> bool {
        self.files.iter().any(FileFacts::is_shared)
    }

    /// Tem tag de semear e já terminou: fica no cliente, seja qual for o
    /// resto. Incompleto com a tag não semeia nada e segue a regra comum.
    #[must_use]
    pub fn keeps_seeding(&self) -> bool {
        self.complete && self.tags.iter().any(|t| t.starts_with(SEED_TAG_PREFIX))
    }

    /// Deve seed ao tracker: terminou, tem obrigação e ela ainda não foi
    /// cumprida. A obrigação só começa quando o download conclui; incompleto
    /// não deve nada.
    #[must_use]
    pub fn owes_seed(&self) -> bool {
        self.complete
            && self
                .obligation
                .is_some_and(|o| !o.met(self.seeded_for, self.ratio))
    }

    /// Espaço que volta ao apagar: só os arquivos sem outro link.
    #[must_use]
    pub fn reclaimable(&self) -> Allocated {
        self.files
            .iter()
            .filter(|f| !f.is_shared())
            .map(|f| f.allocated)
            .sum()
    }

    /// O arquivo mexido mais recentemente. `None` se o download não tem arquivos.
    #[must_use]
    pub fn last_modified(&self) -> Option<SystemTime> {
        self.files.iter().map(|f| f.modified).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn juntar_obrigacoes_so_libera_quando_as_duas_se_cumprem() {
        let amigos = SeedObligation {
            min_seed: Some(72 * H),
            min_ratio: Some(1.0),
        };
        let longa = SeedObligation {
            min_seed: Some(168 * H),
            min_ratio: Some(0.5),
        };
        let so_tempo = SeedObligation {
            min_seed: Some(96 * H),
            min_ratio: None,
        };
        let so_ratio = SeedObligation {
            min_seed: None,
            min_ratio: Some(2.0),
        };
        assert_eq!(
            amigos.and(longa),
            SeedObligation {
                min_seed: Some(168 * H),
                min_ratio: Some(1.0)
            }
        );
        assert_eq!(
            amigos.and(so_tempo),
            SeedObligation {
                min_seed: Some(96 * H),
                min_ratio: None
            }
        );
        assert_eq!(
            amigos.and(so_ratio),
            SeedObligation {
                min_seed: None,
                min_ratio: Some(2.0)
            }
        );
        assert_eq!(
            so_tempo.and(so_ratio),
            SeedObligation {
                min_seed: Some(96 * H),
                min_ratio: None
            }
        );
        assert_eq!(so_tempo.and(amigos), amigos.and(so_tempo));
    }

    fn facts(links: u64, blocks: u64) -> FileFacts {
        FileFacts {
            path: PathBuf::from("/media/downloads/exemplo.mkv"),
            links,
            allocated: Allocated::from_blocks(blocks),
            apparent: Apparent::from_bytes(blocks * 512),
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    fn download(files: Vec<FileFacts>) -> Download {
        Download {
            hash: DownloadHash::new("abc"),
            name: "exemplo".into(),
            state: DownloadState::Seeding,
            private: true,
            category: "tv-sonarr".into(),
            tags: Vec::new(),
            complete: true,
            ratio: 0.0,
            seeded_for: Duration::ZERO,
            idle_for: None,
            obligation: None,
            files,
        }
    }

    #[test]
    fn seed_com_hardlink_na_biblioteca_nao_libera_nada() {
        // O caso medido em produção: 1743 GB de seeds, todos compartilhados.
        // Apagar qualquer um libera zero byte e só custa ratio.
        let d = download(vec![facts(2, 1000)]);
        assert!(d.has_library_link());
        assert_eq!(d.reclaimable(), Allocated::ZERO);
    }

    #[test]
    fn seed_sem_hardlink_libera_o_que_ocupa() {
        let d = download(vec![facts(1, 1000)]);
        assert!(!d.has_library_link());
        assert_eq!(d.reclaimable(), Allocated::from_blocks(1000));
    }

    #[test]
    fn download_misto_conta_so_a_parte_exclusiva() {
        let d = download(vec![facts(2, 1000), facts(1, 40)]);
        assert!(d.has_library_link());
        assert_eq!(d.reclaimable(), Allocated::from_blocks(40));
    }

    #[test]
    fn tag_de_semear_por_indexador_e_reconhecida_pelo_prefixo() {
        assert_eq!(seed_tag("amigos-share"), "acervo:semear:amigos-share");
        assert_eq!(seed_tag("a,b"), "acervo:semear:a-b");
        let com_tags = |tags: Vec<String>| Download {
            tags,
            ..download(Vec::new())
        };
        assert!(com_tags(vec!["acervo:fila".into(), seed_tag("x")]).keeps_seeding());
        assert!(!com_tags(vec!["acervo:fila".into()]).keeps_seeding());
        assert!(!com_tags(vec!["acervo:semear".into()]).keeps_seeding());
        let incompleto = Download {
            complete: false,
            ..com_tags(vec![seed_tag("x")])
        };
        assert!(!incompleto.keeps_seeding());
    }

    const H: Duration = Duration::from_secs(3600);

    fn amigos() -> SeedObligation {
        SeedObligation {
            min_seed: Some(72 * H),
            min_ratio: Some(1.0),
        }
    }

    #[test]
    fn obrigacao_cumpre_por_tempo_ou_por_ratio() {
        assert!(!amigos().met(10 * H, 0.2));
        assert!(amigos().met(72 * H, 0.2));
        assert!(amigos().met(10 * H, 1.0));
    }

    #[test]
    fn obrigacao_so_de_tempo_ignora_o_ratio() {
        let o = SeedObligation {
            min_seed: Some(168 * H),
            min_ratio: None,
        };
        assert!(!o.met(100 * H, 3.0));
        assert!(o.met(168 * H, 0.0));
    }

    #[test]
    fn obrigacao_sem_limite_esta_sempre_cumprida() {
        let o = SeedObligation {
            min_seed: None,
            min_ratio: None,
        };
        assert!(o.met(Duration::ZERO, 0.0));
    }

    #[test]
    fn deve_seed_so_completo_com_obrigacao_pendente() {
        let base = Download {
            obligation: Some(amigos()),
            seeded_for: 10 * H,
            ratio: 0.2,
            ..download(Vec::new())
        };
        assert!(base.owes_seed());
        let cumprido = Download {
            seeded_for: 72 * H,
            ..base.clone()
        };
        assert!(!cumprido.owes_seed());
        let incompleto = Download {
            complete: false,
            ..base.clone()
        };
        assert!(!incompleto.owes_seed());
        let sem_obrigacao = Download {
            obligation: None,
            ..base
        };
        assert!(!sem_obrigacao.owes_seed());
    }

    #[test]
    fn pausado_nao_e_seeding() {
        assert!(!DownloadState::Paused.is_seeding());
        assert!(DownloadState::Seeding.is_seeding());
    }
}

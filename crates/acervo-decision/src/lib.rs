//! Decisão de release de filme: a que filme o release pertence, se ele serve,
//! e em que ordem os que servem são preferidos.
//!
//! Porte do motor do gerenciador de filmes que este projeto substitui, na
//! mesma ordem: ler o nome, casar com o filme, agregar os idiomas, avaliar as
//! especificações e ordenar. Nada aqui faz IO — a decisão recebe a biblioteca,
//! os perfis e os releases prontos, e é conferida contra a do gerenciador num
//! corpus de buscas reais (teste `corpus`, ignorado por padrão).
//!
//! Fica de fora, por depender do estado do gerenciador e não do release:
//! fila, histórico, lista de bloqueio e espaço livre. Custom formats também:
//! nenhum perfil em uso lhes dá nota, e nota zero não muda decisão.

use acervo_parser::{Language, ParsedMovie, Quality, QualityModel, parse_movie_title};

mod languages;
mod mapping;
mod rank;
mod specs;

pub use rank::compare;

/// Tamanhos por minuto de filme, em megabytes, de uma qualidade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityDefinition {
    pub quality: Quality,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}

/// O que fazer com PROPER e REPACK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Propers {
    PreferAndUpgrade,
    DoNotUpgrade,
    DoNotPrefer,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub definitions: Vec<QualityDefinition>,
    /// Teto global de tamanho, em megabytes; zero é sem teto.
    pub maximum_size_mb: u64,
    pub allow_hardcoded_subs: bool,
    /// Termos separados por vírgula que liberam legenda embutida.
    pub whitelisted_hardcoded_subs: String,
    pub propers: Propers,
    pub prefer_indexer_flags: bool,
    /// Folga que precisa sobrar no disco depois do download, em megabytes.
    pub minimum_free_space_mb: u64,
    pub skip_free_space_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            definitions: Vec::new(),
            maximum_size_mb: 0,
            allow_hardcoded_subs: false,
            whitelisted_hardcoded_subs: String::new(),
            propers: Propers::PreferAndUpgrade,
            prefer_indexer_flags: false,
            minimum_free_space_mb: 100,
            skip_free_space_check: false,
        }
    }
}

impl Settings {
    fn definition(&self, quality: Quality) -> Option<&QualityDefinition> {
        self.definitions.iter().find(|d| d.quality == quality)
    }
}

/// Um degrau do perfil: uma qualidade, ou um grupo que vale o mesmo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileItem {
    pub name: String,
    pub qualities: Vec<Quality>,
    pub allowed: bool,
}

/// Perfil de qualidade; `items` do pior ao melhor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub items: Vec<ProfileItem>,
    /// Posição em `items` do corte.
    pub cutoff: Option<usize>,
    pub upgrade_allowed: bool,
    /// `Any`, `Original` ou um idioma.
    pub language: Language,
    pub min_format_score: i32,
    pub cutoff_format_score: i32,
}

impl Profile {
    /// Posição da qualidade no perfil; fora dele é -1, abaixo de tudo.
    fn index(&self, quality: Quality) -> i64 {
        self.items
            .iter()
            .position(|item| item.qualities.contains(&quality))
            .and_then(|i| i64::try_from(i).ok())
            .unwrap_or(-1)
    }

    fn first_allowed(&self) -> i64 {
        self.items
            .iter()
            .position(|item| item.allowed)
            .and_then(|i| i64::try_from(i).ok())
            .unwrap_or(-1)
    }

    fn last_allowed(&self) -> i64 {
        self.items
            .iter()
            .rposition(|item| item.allowed)
            .and_then(|i| i64::try_from(i).ok())
            .unwrap_or(-1)
    }

    /// O corte efetivo: sem upgrade, o primeiro permitido já basta.
    fn effective_cutoff(&self) -> i64 {
        if self.upgrade_allowed {
            self.cutoff
                .and_then(|c| i64::try_from(c).ok())
                .unwrap_or_else(|| self.last_allowed())
        } else {
            self.first_allowed()
        }
    }
}

/// O arquivo que o filme já tem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingFile {
    pub quality: QualityModel,
    pub release_group: Option<String>,
    /// Dias desde que entrou na biblioteca.
    pub age_days: u32,
}

/// Um filme da biblioteca, com o que a decisão precisa dele.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: i64,
    /// Título usado pela agregação de idiomas (o da base de metadados).
    pub title: String,
    /// Títulos já limpos com que o release é comparado: o principal, o
    /// original, os alternativos e as traduções.
    pub clean_titles: Vec<String>,
    pub year: Option<u16>,
    pub secondary_year: Option<u16>,
    pub tmdb_id: u32,
    pub imdb_id: Option<String>,
    pub original_language: Language,
    /// Minutos; zero é desconhecido.
    pub runtime: u32,
    pub monitored: bool,
    pub available: bool,
    pub profile: Profile,
    pub file: Option<ExistingFile>,
    /// Qualidade de cada download deste filme já na fila.
    pub queued: Vec<QualityModel>,
    /// Espaço livre onde o filme mora, em bytes; `None` pula a verificação.
    pub free_space: Option<u64>,
}

/// Um indexador, com o que pesa na decisão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexer {
    pub name: String,
    /// Menor é melhor; o padrão da referência é 25.
    pub priority: i32,
    pub minimum_seeders: u32,
    /// Idiomas que "MULTI" significa neste indexador.
    pub multi_languages: Vec<Language>,
}

/// Um resultado de busca, como o indexador o descreve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub title: String,
    pub indexer: String,
    pub size: u64,
    pub seeders: Option<u32>,
    /// Seeders mais leechers.
    pub peers: Option<u32>,
    pub imdb_id: Option<u32>,
    pub tmdb_id: Option<u32>,
    /// Idiomas que o indexador informa, se informa.
    pub languages: Vec<Language>,
    pub container: Option<String>,
    /// Bits de flag do indexador (freeleech, internal...), no formato da
    /// referência.
    pub flags: u32,
}

/// Por que um release não serve. O nome de cada variante é o motivo da
/// referência, para comparar decisão com decisão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    UnableToParse,
    UnknownMovie,
    WrongMovie,
    QualityNotWanted(Quality),
    WantedLanguage {
        wanted: Language,
        found: Vec<Language>,
    },
    BelowMinimumSize {
        size: u64,
        minimum: u64,
    },
    AboveMaximumSize {
        size: u64,
        maximum: u64,
    },
    MaximumSizeExceeded {
        size: u64,
        maximum: u64,
    },
    Sample,
    HardcodeSubtitles(String),
    Raw,
    MinimumSeeders {
        seeders: u32,
        minimum: u32,
    },
    DiskCutoffMet,
    DiskHigherPreference,
    DiskHigherRevision,
    DiskCustomFormatScore,
    DiskUpgradesNotAllowed,
    QualityUpgradesDisabled,
    RepackDisabled,
    RepackUnknownReleaseGroup,
    RepackReleaseGroupDoesNotMatch,
    PropersDisabled,
    ProperForOldFile,
    MovieNotMonitored,
    Availability,
    CustomFormatMinimumScore,
    QueueCutoffMet,
    QueueHigherPreference,
    QueueHigherRevision,
    QueueCustomFormatScore,
    QueueUpgradesNotAllowed,
    QueuePropersDisabled,
    MinimumFreeSpace {
        remaining: i64,
    },
}

impl Rejection {
    /// O motivo, sem os detalhes: o que a comparação com a referência usa.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::UnableToParse => "UnableToParse",
            Self::UnknownMovie => "UnknownMovie",
            Self::WrongMovie => "WrongMovie",
            Self::QualityNotWanted(_) => "QualityNotWanted",
            Self::WantedLanguage { .. } => "WantedLanguage",
            Self::BelowMinimumSize { .. } => "BelowMinimumSize",
            Self::AboveMaximumSize { .. } => "AboveMaximumSize",
            Self::MaximumSizeExceeded { .. } => "MaximumSizeExceeded",
            Self::Sample => "Sample",
            Self::HardcodeSubtitles(_) => "HardcodeSubtitles",
            Self::Raw => "Raw",
            Self::MinimumSeeders { .. } => "MinimumSeeders",
            Self::DiskCutoffMet => "DiskCutoffMet",
            Self::DiskHigherPreference => "DiskHigherPreference",
            Self::DiskHigherRevision => "DiskHigherRevision",
            Self::DiskCustomFormatScore => "DiskCustomFormatScore",
            Self::DiskUpgradesNotAllowed => "DiskUpgradesNotAllowed",
            Self::QualityUpgradesDisabled => "QualityUpgradesDisabled",
            Self::RepackDisabled => "RepackDisabled",
            Self::RepackUnknownReleaseGroup => "RepackUnknownReleaseGroup",
            Self::RepackReleaseGroupDoesNotMatch => "RepackReleaseGroupDoesNotMatch",
            Self::PropersDisabled => "PropersDisabled",
            Self::ProperForOldFile => "ProperForOldFile",
            Self::MovieNotMonitored => "MovieNotMonitored",
            Self::Availability => "Availability",
            Self::CustomFormatMinimumScore => "CustomFormatMinimumScore",
            Self::QueueCutoffMet => "QueueCutoffMet",
            Self::QueueHigherPreference => "QueueHigherPreference",
            Self::QueueHigherRevision => "QueueHigherRevision",
            Self::QueueCustomFormatScore => "QueueCustomFormatScore",
            Self::QueueUpgradesNotAllowed => "QueueUpgradesNotAllowed",
            Self::QueuePropersDisabled => "QueuePropersDisabled",
            Self::MinimumFreeSpace { .. } => "MinimumFreeSpace",
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = |languages: &[Language]| {
            languages
                .iter()
                .map(|l| l.name())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Self::UnableToParse => write!(f, "nome ilegível"),
            Self::UnknownMovie => write!(f, "não casa com nenhum filme da biblioteca"),
            Self::WrongMovie => write!(f, "é de outro filme"),
            Self::QualityNotWanted(q) => write!(f, "{} não é aceita pelo perfil", q.name()),
            Self::WantedLanguage { wanted, found } => {
                write!(f, "pede {}, tem {}", wanted.name(), names(found))
            }
            Self::BelowMinimumSize { size, minimum } => {
                write!(f, "{size} bytes, abaixo do mínimo de {minimum}")
            }
            Self::AboveMaximumSize { size, maximum } => {
                write!(f, "{size} bytes, acima do máximo de {maximum}")
            }
            Self::MaximumSizeExceeded { size, maximum } => {
                write!(f, "{size} bytes, acima do teto global de {maximum}")
            }
            Self::Sample => write!(f, "é amostra"),
            Self::HardcodeSubtitles(subs) => write!(f, "legenda embutida: {subs}"),
            Self::Raw => write!(f, "disco bruto (Blu-ray ou DVD)"),
            Self::MinimumSeeders { seeders, minimum } => {
                write!(f, "{seeders} seeders, mínimo {minimum}")
            }
            Self::DiskCutoffMet => write!(f, "o arquivo atual já atinge o corte"),
            Self::DiskHigherPreference => write!(f, "o arquivo atual é igual ou melhor"),
            Self::DiskHigherRevision => write!(f, "o arquivo atual tem revisão igual ou maior"),
            Self::DiskCustomFormatScore => {
                write!(
                    f,
                    "o arquivo atual tem nota de custom format igual ou maior"
                )
            }
            Self::DiskUpgradesNotAllowed | Self::QualityUpgradesDisabled => {
                write!(f, "o perfil não permite upgrade")
            }
            Self::RepackDisabled => write!(f, "repack desativado"),
            Self::RepackUnknownReleaseGroup => write!(f, "repack sem grupo conhecido"),
            Self::RepackReleaseGroupDoesNotMatch => write!(f, "repack de outro grupo"),
            Self::PropersDisabled | Self::QueuePropersDisabled => write!(f, "proper desativado"),
            Self::ProperForOldFile => write!(f, "proper para arquivo antigo"),
            Self::MovieNotMonitored => write!(f, "filme não monitorado"),
            Self::Availability => write!(f, "filme ainda não disponível"),
            Self::CustomFormatMinimumScore => write!(f, "nota de custom format abaixo do mínimo"),
            Self::QueueCutoffMet => write!(f, "o que já está na fila atinge o corte"),
            Self::QueueHigherPreference => write!(f, "o que já está na fila é igual ou melhor"),
            Self::QueueHigherRevision => {
                write!(f, "o que já está na fila tem revisão igual ou maior")
            }
            Self::QueueCustomFormatScore => {
                write!(
                    f,
                    "o que já está na fila tem nota de custom format igual ou maior"
                )
            }
            Self::QueueUpgradesNotAllowed => {
                write!(f, "já há download na fila e o perfil não permite upgrade")
            }
            Self::MinimumFreeSpace { remaining } => {
                write!(
                    f,
                    "sobrariam {remaining} bytes no disco, abaixo da folga mínima"
                )
            }
        }
    }
}

/// Busca pedida por alguém (interativa ou manual) pula disponibilidade e
/// monitoramento; a automática não.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    UserInvoked,
    Automatic,
}

/// A decisão sobre um release.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Posição do release na lista recebida.
    pub release: usize,
    pub parsed: Option<ParsedMovie>,
    /// Id do filme com que casou.
    pub movie: Option<i64>,
    /// Idiomas depois da agregação.
    pub languages: Vec<Language>,
    pub rejections: Vec<Rejection>,
}

impl Decision {
    #[must_use]
    pub fn approved(&self) -> bool {
        self.rejections.is_empty()
    }
}

/// O motor: a biblioteca, os indexadores e as configurações.
#[derive(Debug, Clone, Copy)]
pub struct Engine<'a> {
    pub library: &'a [Target],
    pub indexers: &'a [Indexer],
    pub settings: &'a Settings,
}

impl Engine<'_> {
    fn movie(&self, id: i64) -> Option<&Target> {
        self.library.iter().find(|m| m.id == id)
    }

    fn indexer(&self, name: &str) -> Option<&Indexer> {
        self.indexers.iter().find(|i| i.name == name)
    }

    /// Decide os resultados de uma busca pelo filme `movie`, já na ordem de
    /// preferência: os que casaram com algum filme primeiro, do melhor ao
    /// pior; depois os que não casaram, na ordem recebida.
    #[must_use]
    pub fn search(&self, movie: i64, releases: &[Release], mode: Mode) -> Vec<Decision> {
        let decisions: Vec<Decision> = releases
            .iter()
            .enumerate()
            .map(|(index, release)| self.decide(index, release, Some(movie), mode))
            .collect();
        rank::prioritize(self, releases, decisions)
    }

    /// Decide releases recentes sem filme buscado, como a sincronização de
    /// RSS da referência: cada release é casado com a biblioteca inteira pelo
    /// título e ano. Na ordem de preferência.
    #[must_use]
    pub fn rss(&self, releases: &[Release]) -> Vec<Decision> {
        let decisions: Vec<Decision> = releases
            .iter()
            .enumerate()
            .map(|(index, release)| self.decide(index, release, None, Mode::Automatic))
            .collect();
        rank::prioritize(self, releases, decisions)
    }

    fn decide(
        &self,
        index: usize,
        release: &Release,
        searched: Option<i64>,
        mode: Mode,
    ) -> Decision {
        let parsed =
            parse_movie_title(&release.title).filter(|p| !p.primary_title().trim().is_empty());
        let Some(parsed) = parsed else {
            return Decision {
                release: index,
                languages: acervo_parser::parse_languages(&release.title),
                parsed: None,
                movie: None,
                rejections: vec![Rejection::UnableToParse],
            };
        };
        let matched = mapping::find(self.library, &parsed, release, searched);
        let Some(target) = matched.and_then(|id| self.movie(id)) else {
            return Decision {
                release: index,
                languages: parsed.languages.clone(),
                parsed: Some(parsed),
                movie: None,
                rejections: vec![Rejection::UnknownMovie],
            };
        };
        let languages =
            languages::aggregate(&parsed, release, target, self.indexer(&release.indexer));
        let rejections =
            specs::evaluate(self, release, &parsed, &languages, target, searched, mode);
        Decision {
            release: index,
            movie: Some(target.id),
            parsed: Some(parsed),
            languages,
            rejections,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acervo_parser::Revision;

    fn profile() -> Profile {
        let item = |quality: Quality| ProfileItem {
            name: quality.name().into(),
            qualities: vec![quality],
            allowed: true,
        };
        Profile {
            name: "Any".into(),
            items: vec![
                item(Quality::WebDl720p),
                item(Quality::WebDl1080p),
                item(Quality::WebDl2160p),
            ],
            cutoff: Some(0),
            upgrade_allowed: false,
            language: Language::Original,
            min_format_score: 0,
            cutoff_format_score: 0,
        }
    }

    fn movie(id: i64, title: &str, imdb: &str) -> Target {
        Target {
            id,
            title: title.into(),
            clean_titles: vec![acervo_parser::clean_movie_title(title)],
            year: Some(2025),
            secondary_year: None,
            tmdb_id: u32::try_from(id).unwrap() * 100,
            imdb_id: Some(imdb.into()),
            original_language: Language::English,
            runtime: 100,
            monitored: true,
            available: true,
            profile: profile(),
            file: None,
            queued: Vec::new(),
            free_space: None,
        }
    }

    fn release(title: &str) -> Release {
        Release {
            title: title.into(),
            indexer: "tracker".into(),
            size: 4_000_000_000,
            seeders: Some(10),
            peers: Some(12),
            imdb_id: None,
            tmdb_id: None,
            languages: Vec::new(),
            container: None,
            flags: 0,
        }
    }

    fn decide(library: &[Target], releases: &[Release]) -> Vec<Decision> {
        let settings = Settings::default();
        let indexers = [Indexer {
            name: "tracker".into(),
            priority: 25,
            minimum_seeders: 1,
            multi_languages: Vec::new(),
        }];
        Engine {
            library,
            indexers: &indexers,
            settings: &settings,
        }
        .search(library[0].id, releases, Mode::UserInvoked)
    }

    fn reasons(decision: &Decision) -> Vec<&'static str> {
        decision.rejections.iter().map(Rejection::reason).collect()
    }

    #[test]
    fn dual_audio_passa_e_so_dublado_nao() {
        let library = [movie(1, "The Housemaid", "tt0000001")];
        let decisions = decide(
            &library,
            &[
                release("The Housemaid 2025 1080p WEB-DL Dual-Audio Brazilian Original"),
                release("The.Housemaid.2025.1080p.WEB-DL.DUBLADO"),
            ],
        );
        let by = |i: usize| decisions.iter().find(|d| d.release == i).unwrap();
        assert!(by(0).approved(), "{:?}", by(0).rejections);
        assert_eq!(by(0).languages, [Language::PortugueseBr, Language::English]);
        assert_eq!(reasons(by(1)), ["WantedLanguage"]);
    }

    #[test]
    fn prefere_qualidade_maior_e_ignora_outro_filme() {
        let library = [
            movie(1, "The Housemaid", "tt0000001"),
            movie(2, "Other", "tt0000002"),
        ];
        let mut other = release("The Housemaid 2025 2160p WEB-DL");
        other.imdb_id = Some(2);
        let decisions = decide(
            &library,
            &[
                release("The Housemaid 2025 720p WEB-DL"),
                release("The Housemaid 2025 1080p WEB-DL"),
                other,
                release("Something Else 2025 1080p WEB-DL"),
            ],
        );
        // O filme buscado primeiro, do melhor ao pior; o id de outro filme
        // casa com ele e é recusado; o que não casa vai para o fim.
        let order: Vec<usize> = decisions.iter().map(|d| d.release).collect();
        assert_eq!(order, [1, 0, 2, 3]);
        assert_eq!(reasons(&decisions[2]), ["WrongMovie"]);
        assert_eq!(reasons(&decisions[3]), ["UnknownMovie"]);
    }

    #[test]
    fn perfil_sem_upgrade_recusa_quem_ja_tem_arquivo() {
        let mut target = movie(1, "The Housemaid", "tt0000001");
        target.file = Some(ExistingFile {
            quality: QualityModel {
                quality: Quality::WebDl720p,
                revision: Revision::default(),
            },
            release_group: Some("GRP".into()),
            age_days: 30,
        });
        let decisions = decide(&[target], &[release("The Housemaid 2025 2160p WEB-DL-GRP")]);
        assert_eq!(
            reasons(&decisions[0]),
            ["QualityUpgradesDisabled", "DiskCutoffMet"]
        );
    }

    #[test]
    fn poucos_seeders_e_disco_cheio() {
        let mut target = movie(1, "The Housemaid", "tt0000001");
        target.free_space = Some(1_000_000_000);
        let mut few = release("The Housemaid 2025 1080p WEB-DL");
        few.seeders = Some(0);
        let decisions = decide(&[target], &[few, release("The Housemaid 2025 720p WEB-DL")]);
        let by = |i: usize| decisions.iter().find(|d| d.release == i).unwrap();
        // Espaço livre só é olhado quando o resto aprovou.
        assert_eq!(reasons(by(0)), ["MinimumSeeders"]);
        assert_eq!(reasons(by(1)), ["MinimumFreeSpace"]);
    }
}

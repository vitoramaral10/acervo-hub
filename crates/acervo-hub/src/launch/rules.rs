//! As regras de envio do BJ-Share e do Amigos-Share, como funções puras.
//!
//! [`check`] recebe os fatos já levantados (sonda de cada vídeo, arquivos do
//! torrent, releases que o destino já tem) e devolve um [`Report`]: uma
//! [`Check`] por regra, com o número da regra do site e o motivo concreto em
//! português. Nada aqui lê disco, rede ou banco: o preparo reúne os [`Facts`]
//! e grava o `Report` em `launches.report`.
//!
//! Os arquivos nunca mudam (princípio 1 da spec): o que precisaria de
//! alteração para passar vira `Fail`, não conserto. `Warn` não bloqueia, só
//! pede o julgamento do usuário (duplicidade, sobretudo).

use std::collections::HashSet;

use acervo_parser::{
    Language, Modifier, Quality, Source, parse_episode_languages, parse_episode_quality,
    parse_episode_title, parse_languages, parse_quality,
};
use serde::{Deserialize, Serialize};

use crate::mediainfo::{AudioFacts, Hdr, MediaFacts, VideoFacts};

use super::adapt::{self, Extraction, Obstacle};
use super::subtitle_sniff::{Completeness, Sniff, SniffLanguage, Sniffs};

/// Diferença de tamanho (relativa ao release do destino) abaixo da qual dois
/// releases de mesma resolução, fonte e áudio contam como o mesmo arquivo.
const DUPLICATE_SIZE_TOLERANCE: f64 = 0.10;
/// No BJ, acima disso a diferença de tamanho já não pede nem aviso.
const BJ_SIMILAR_SIZE_TOLERANCE: f64 = 0.30;
/// Máximo de legendas que acompanham um filme no BJ (4.2.1.10).
const BJ_MAX_SUBTITLES: usize = 4;

const VIDEO_EXTENSIONS: [&str; 5] = ["mkv", "mp4", "avi", "m2ts", "ts"];
const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "ass", "ssa", "sub", "idx"];
const ARCHIVE_EXTENSIONS: [&str; 7] = ["rar", "zip", "7z", "tar", "gz", "bz2", "xz"];

/// Os destinos de lançamento, chaveados pelo nome do indexador.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Site {
    Bj,
    Amigos,
}

impl Site {
    /// `IndexerRecord.name`: `bjsharecustom` e `amigosshare`. Outro nome não
    /// é destino de lançamento.
    #[must_use]
    pub fn from_indexer(name: &str) -> Option<Self> {
        match name {
            "bjsharecustom" => Some(Self::Bj),
            "amigosshare" => Some(Self::Amigos),
            _ => None,
        }
    }

    /// Como a mensagem chama o site.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bj => "BJ",
            Self::Amigos => "Amigos",
        }
    }
}

/// O que o torrent de origem contém.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Movie,
    Episode,
    Season,
}

/// Estado da série no catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesStatus {
    Ended,
    Continuing,
}

/// Um arquivo do torrent de origem (caminho relativo, já com a pasta raiz).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub size: u64,
}

/// Um vídeo do torrent com o que o `ffprobe` leu dele.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbedVideo {
    /// Mesmo caminho de [`SourceFile::path`].
    pub path: String,
    pub media: MediaFacts,
}

/// Como o áudio do release se apresenta: é o que separa um duplicado de um
/// release diferente (um Dual já contém o dublado e o legendado).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioKind {
    /// Áudio PT e áudio original no mesmo arquivo.
    Dual,
    /// Só áudio PT, numa obra de outro idioma.
    Dublado,
    /// Obra falada em português, com áudio PT.
    Nacional,
    /// Sem áudio PT: depende de legenda.
    Legendado,
}

impl AudioKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Dual => "Dual",
            Self::Dublado => "dublado",
            Self::Nacional => "nacional",
            Self::Legendado => "legendado",
        }
    }
}

/// Codec de vídeo, só os que as regras distinguem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
    H265,
    Av1,
}

impl Codec {
    /// Pelo `codec_name` do `ffprobe`.
    fn from_ffprobe(name: &str) -> Option<Self> {
        match name {
            "h264" => Some(Self::H264),
            "hevc" => Some(Self::H265),
            "av1" => Some(Self::Av1),
            _ => None,
        }
    }

    /// Pelo nome do release (`x264`, `H.265`, `HEVC`...).
    fn from_title(title: &str) -> Option<Self> {
        let lower = title.to_lowercase();
        let squashed: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
        if squashed.contains("x265") || squashed.contains("h265") || squashed.contains("hevc") {
            Some(Self::H265)
        } else if squashed.contains("x264") || squashed.contains("h264") || squashed.contains("avc")
        {
            Some(Self::H264)
        } else if squashed.contains("av1") {
            Some(Self::Av1)
        } else {
            None
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::Av1 => "AV1",
        }
    }
}

/// Um release que o destino já tem para a mesma obra, com a qualidade que o
/// parser tirou do título dele.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DestinationRelease {
    pub title: String,
    pub size: u64,
    #[serde(with = "quality_name")]
    pub quality: Quality,
    pub audio: AudioKind,
    pub codec: Option<Codec>,
    /// Temporada que o título declara quando é um pacote de temporada
    /// completa de uma só temporada (`Serie.S01.1080p...`); `None` para filme,
    /// episódio e pacote de várias temporadas. Relatórios antigos não têm o
    /// campo.
    #[serde(default)]
    pub season_pack: Option<u16>,
}

impl DestinationRelease {
    /// Parseia o título do destino (filme ou episódio) para qualidade, tipo de
    /// áudio e codec.
    #[must_use]
    pub fn new(title: &str, size: u64, kind: Kind) -> Self {
        let (quality, languages) = if kind == Kind::Movie {
            (parse_quality(title).quality, parse_languages(title))
        } else {
            (
                parse_episode_quality(title).quality,
                parse_episode_languages(title),
            )
        };
        let portuguese = languages.iter().copied().any(is_portuguese);
        let others = languages
            .iter()
            .any(|l| !is_portuguese(*l) && *l != Language::Unknown);
        let dual_word = title
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|t| matches!(t, "dual" | "multi"));
        let audio = if dual_word || (portuguese && others) {
            AudioKind::Dual
        } else if portuguese {
            AudioKind::Dublado
        } else {
            AudioKind::Legendado
        };
        let season_pack = if kind == Kind::Movie {
            None
        } else {
            parse_episode_title(title)
                .filter(|e| e.full_season && !e.multi_season && e.season > 0)
                .map(|e| e.season)
        };
        Self {
            title: title.to_owned(),
            size,
            quality,
            audio,
            codec: Codec::from_title(title),
            season_pack,
        }
    }
}

/// `Quality` vai ao JSON pelo nome que a API v3 usa (`WEBDL-1080p`).
mod quality_name {
    use acervo_parser::Quality;
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S: Serializer>(quality: &Quality, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(quality.name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Quality, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(Quality::ALL
            .into_iter()
            .find(|q| q.name() == name)
            .unwrap_or(Quality::Unknown))
    }
}

/// Tudo que as regras pedem, já levantado.
#[derive(Debug, Clone)]
pub struct Facts {
    pub kind: Kind,
    /// Nome da pasta raiz do torrent (ou do arquivo, se for avulso).
    pub release_name: String,
    pub files: Vec<SourceFile>,
    /// Sonda de cada vídeo do torrent.
    pub videos: Vec<ProbedVideo>,
    /// Idioma original da obra (`Language::from_name` do que o banco guarda).
    pub original_language: Option<Language>,
    pub series_status: Option<SeriesStatus>,
    /// Os pares (temporada, episódio) exibidos da temporada no catálogo.
    pub season_episodes: Option<Vec<(u16, u16)>>,
    /// Episódios da temporada no catálogo que ainda não foram ao ar (data
    /// futura ou ausente). Com algum, a temporada ainda está em exibição.
    pub season_pending: u32,
    pub destination: Vec<DestinationRelease>,
    /// Conteúdo das legendas em arquivo sem idioma no nome (o mesmo nome-base
    /// do vídeo), lido do disco pelo preparo.
    pub sniffs: Sniffs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

/// O resultado de uma regra.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Número da regra no site (`4.2.1.14.1`).
    pub rule: String,
    pub status: Status,
    pub message: String,
}

impl Check {
    fn ok(rule: &str, message: impl Into<String>) -> Self {
        Self {
            rule: rule.to_owned(),
            status: Status::Ok,
            message: message.into(),
        }
    }

    /// Aviso e falha citam a regra no fim da mensagem, como o site a numera.
    fn warn(rule: &str, message: impl Into<String>) -> Self {
        Self {
            rule: rule.to_owned(),
            status: Status::Warn,
            message: format!("{} ({rule})", message.into()),
        }
    }

    fn fail(rule: &str, message: impl Into<String>) -> Self {
        Self {
            rule: rule.to_owned(),
            status: Status::Fail,
            message: format!("{} ({rule})", message.into()),
        }
    }

    fn with(rule: &str, status: Status, message: impl Into<String>) -> Self {
        match status {
            Status::Ok => Self::ok(rule, message),
            Status::Warn => Self::warn(rule, message),
            Status::Fail => Self::fail(rule, message),
        }
    }
}

/// Se o pacote pode seguir: qualquer `Fail` o torna inelegível.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Elegivel,
    Inelegivel,
}

/// Checagens, releases do destino que as motivaram e o veredito. É o JSON de
/// `launches.report`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub checks: Vec<Check>,
    pub destination: Vec<DestinationRelease>,
    pub verdict: Verdict,
    /// Os `.srt` que a pasta adaptada vai criar (só BJ; vazio no resto). Laudos
    /// gravados antes da adaptação não têm o campo.
    #[serde(default)]
    pub adaptations: Vec<Extraction>,
}

// Consultas que só os testes fazem: o preparo grava o relatório inteiro.
#[cfg(test)]
impl Report {
    /// O pior status entre as checagens da regra; `None` se a regra não se
    /// aplicou ao pacote.
    #[must_use]
    pub fn status_of(&self, rule: &str) -> Option<Status> {
        self.checks
            .iter()
            .filter(|c| c.rule == rule)
            .map(|c| c.status)
            .max()
    }

    #[must_use]
    pub fn has_warnings(&self) -> bool {
        self.checks.iter().any(|c| c.status == Status::Warn)
    }
}

/// Aplica as regras do site aos fatos.
#[must_use]
pub fn check(site: Site, facts: &Facts) -> Report {
    let ctx = Context::new(facts, site);
    let mut checks = Vec::new();
    if facts.videos.is_empty() {
        checks.push(Check::fail(
            "sonda",
            "nenhum vídeo foi lido pelo ffprobe: sem a sonda não há como checar o arquivo",
        ));
    }
    match site {
        Site::Amigos => amigos(&ctx, &mut checks),
        Site::Bj => bj(&ctx, &mut checks),
    }
    let verdict = if checks.iter().any(|c| c.status == Status::Fail) {
        Verdict::Inelegivel
    } else {
        Verdict::Elegivel
    };
    Report {
        checks,
        destination: facts.destination.clone(),
        verdict,
        adaptations: ctx.adaptations,
    }
}

/// Os fatos mais o que se deriva deles uma vez só.
struct Context<'a> {
    facts: &'a Facts,
    /// O maior vídeo: dele vêm resolução, HDR e codec.
    main: Option<&'a ProbedVideo>,
    quality: Quality,
    resolution: u16,
    audio: Option<AudioKind>,
    codec: Option<Codec>,
    size: u64,
    /// Legendas PT embutidas que o BJ vai ganhar em arquivo (pasta adaptada).
    adaptations: Vec<Extraction>,
}

impl<'a> Context<'a> {
    fn new(facts: &'a Facts, site: Site) -> Self {
        let size_of = |path: &str| {
            facts
                .files
                .iter()
                .find(|f| f.path == path)
                .map_or(0, |f| f.size)
        };
        let main = facts.videos.iter().max_by_key(|v| size_of(&v.path));
        let quality = if facts.kind == Kind::Movie {
            parse_quality(&facts.release_name).quality
        } else {
            parse_episode_quality(&facts.release_name).quality
        };
        let video = main.and_then(|v| v.media.video.as_ref());
        // Pelo arquivo, não pelo nome (princípio 2).
        let resolution = video
            .map(resolution_of)
            .filter(|r| *r > 0)
            .unwrap_or_else(|| quality.resolution());
        let audio = main.map(|v| audio_kind(&v.media, facts.original_language));
        let codec = video.and_then(|v| Codec::from_ffprobe(&v.codec));
        Self {
            facts,
            main,
            quality,
            resolution,
            audio,
            codec,
            size: facts.files.iter().map(|f| f.size).sum(),
            // Obra nacional dispensa a legenda: nada a extrair.
            adaptations: if site == Site::Bj && !facts.original_language.is_some_and(is_portuguese)
            {
                adapt::extractions(&facts.files, &facts.videos, &facts.sniffs)
            } else {
                Vec::new()
            },
        }
    }

    fn video(&self) -> Option<&VideoFacts> {
        self.main.and_then(|v| v.media.video.as_ref())
    }

    /// Arquivos de vídeo do torrent, pela extensão.
    fn video_count(&self) -> usize {
        self.facts
            .files
            .iter()
            .filter(|f| has_extension(&f.path, &VIDEO_EXTENSIONS))
            .count()
    }

    fn resolution_label(&self) -> String {
        if self.resolution == 0 {
            "resolução desconhecida".to_owned()
        } else {
            format!("{}p", self.resolution)
        }
    }
}

// ---------------------------------------------------------------------------
// Amigos
// ---------------------------------------------------------------------------

fn amigos(ctx: &Context<'_>, checks: &mut Vec<Check>) {
    checks.push(archives(ctx, "3.2", "o Amigos"));
    checks.push(portuguese_available(ctx));
    checks.push(site_reference(ctx, "5.6"));
    checks.extend(amigos_series_scope(ctx));
    checks.push(duplicate(ctx, Site::Amigos, "4.2.1"));
    checks.extend(inferior_source(ctx));
}

/// 3.3: obra que não é falada em português precisa de áudio ou legenda PT.
fn portuguese_available(ctx: &Context<'_>) -> Check {
    const RULE: &str = "3.3";
    let facts = ctx.facts;
    let Some(original) = facts.original_language else {
        return Check::warn(
            RULE,
            "idioma original da obra desconhecido: confira à mão se precisa de áudio ou legenda PT",
        );
    };
    if is_portuguese(original) {
        return Check::ok(RULE, "obra falada em português: não precisa de legenda");
    }
    if facts.videos.is_empty() {
        return Check::fail(
            RULE,
            "sem vídeo lido, não dá para conferir áudio ou legenda PT",
        );
    }
    // Cada vídeo precisa do seu: legenda de outro episódio não vale.
    let missing =
        videos_without_portuguese(&facts.files, &facts.videos, &facts.sniffs, Site::Amigos);
    if missing.is_empty() {
        let audio = facts
            .videos
            .iter()
            .all(|v| v.media.audio.iter().any(is_portuguese_audio));
        if audio {
            return Check::ok(RULE, "tem áudio em português");
        }
        let relied = sibling_reliance(&facts.files, &facts.videos, &facts.sniffs, Site::Amigos);
        if let Some(doubt) = relied.iter().find(|r| r.standing != Standing::Confirmed) {
            let note = describe_sibling(doubt.path, doubt.sniff, Site::Amigos);
            return Check::warn(
                RULE,
                if doubt.standing == Standing::European {
                    format!(
                        "a legenda em português é pt-PT, e a regra pede português brasileiro e/ou áudio dublado em português: {note}"
                    )
                } else {
                    note
                },
            );
        }
        match relied.first() {
            Some(found) => Check::ok(
                RULE,
                format!(
                    "sem áudio PT, mas tem legenda em português: {}",
                    describe_sibling(found.path, found.sniff, Site::Amigos)
                ),
            ),
            None => Check::ok(RULE, "sem áudio PT, mas tem legenda em português"),
        }
    } else {
        Check::fail(
            RULE,
            format!(
                "obra em {} com {} de {} vídeos sem áudio nem legenda em português: o Amigos exige um dos dois em cada vídeo ({}){}",
                language_pt(original),
                missing.len(),
                facts.videos.len(),
                list_names(&missing),
                sibling_hint(&facts.files, &facts.sniffs, &missing, Site::Amigos),
            ),
        )
    }
}

/// Os nomes, no máximo oito, para a mensagem.
fn list_names(videos: &[&ProbedVideo]) -> String {
    let mut names: Vec<&str> = videos.iter().take(8).map(|v| file_name(&v.path)).collect();
    let more = videos.len().saturating_sub(names.len());
    let tail = format!("e mais {more}");
    if more > 0 {
        names.push(&tail);
    }
    names.join(", ")
}

/// Os vídeos sem português: sem faixa de áudio PT e sem legenda PT (embutida,
/// só no Amigos; ou em arquivo ao lado, associada ao vídeo pelo nome-base e,
/// no BJ, na mesma pasta). A legenda em arquivo sem idioma no nome vale pelo
/// que o conteúdo diz (`sniffs`): PT-BR completo, português não confirmado ou
/// inconclusivo (com aviso) e, no Amigos, o pt-PT completo (com aviso).
pub(super) fn videos_without_portuguese<'a>(
    files: &[SourceFile],
    videos: &'a [ProbedVideo],
    sniffs: &Sniffs,
    site: Site,
) -> Vec<&'a ProbedVideo> {
    videos
        .iter()
        .filter(|video| {
            let audio = video.media.audio.iter().any(is_portuguese_audio);
            let embedded = site == Site::Amigos
                && video
                    .media
                    .subtitles
                    .iter()
                    .any(|s| is_portuguese_subtitle(s.language, s.raw_language.as_deref()));
            !(audio || embedded || has_external_portuguese(files, sniffs, &video.path, site))
        })
        .collect()
}

/// Há legenda PT em arquivo associada ao vídeo: pelo sufixo de idioma do
/// nome (`.pt-BR`, `.por`, `.forced`) ou, sem sufixo, pelo conteúdo.
fn has_external_portuguese(files: &[SourceFile], sniffs: &Sniffs, video: &str, site: Site) -> bool {
    has_external_by_name(files, video, site == Site::Bj, site == Site::Bj)
        || siblings(files, sniffs, video, site == Site::Bj)
            .iter()
            .any(|(_, s)| standing(s).counts(site))
}

/// O quanto o conteúdo de um `.srt` sem idioma no nome confirma a legenda PT
/// completa que a regra pede. Do melhor ao pior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    /// PT-BR e completa.
    Confirmed,
    /// Português completo, mas a variante não foi confirmada.
    Unconfirmed,
    /// Português (BR ou sem variante), sem dar para dizer se é completa.
    Doubtful,
    /// pt-PT completo.
    European,
    /// Forçada, de outro idioma ou pt-PT sem confirmação de completude.
    Rejected,
}

impl Standing {
    /// Conta como legenda PT presente (com aviso, salvo `Confirmed`). O pt-PT
    /// completo só no Amigos; no BJ a regra exige PT-BR.
    fn counts(self, site: Site) -> bool {
        match self {
            Self::Confirmed | Self::Unconfirmed | Self::Doubtful => true,
            Self::European => site == Site::Amigos,
            Self::Rejected => false,
        }
    }
}

fn standing(sniff: &Sniff) -> Standing {
    let portuguese = matches!(sniff.language, SniffLanguage::PtBr | SniffLanguage::Pt);
    match sniff.completeness() {
        Completeness::Complete => match sniff.language {
            SniffLanguage::PtBr => Standing::Confirmed,
            SniffLanguage::Pt => Standing::Unconfirmed,
            SniffLanguage::PtPt => Standing::European,
            _ => Standing::Rejected,
        },
        Completeness::Inconclusive if portuguese => Standing::Doubtful,
        Completeness::Forced | Completeness::Inconclusive => Standing::Rejected,
    }
}

/// Um vídeo cuja legenda PT é só um `.srt` sem idioma no nome.
struct Reliance<'a> {
    video: &'a ProbedVideo,
    path: &'a str,
    sniff: &'a Sniff,
    standing: Standing,
}

/// Os vídeos que dependem do `.srt` sem idioma no nome para ter português:
/// sem áudio PT, sem legenda PT embutida (no Amigos) e sem arquivo PT pelo
/// sufixo do nome. Com a melhor legenda lida de cada um que conta no site.
fn sibling_reliance<'a>(
    files: &'a [SourceFile],
    videos: &'a [ProbedVideo],
    sniffs: &'a Sniffs,
    site: Site,
) -> Vec<Reliance<'a>> {
    videos
        .iter()
        .filter(|v| {
            let audio = v.media.audio.iter().any(is_portuguese_audio);
            let embedded = site == Site::Amigos
                && v.media
                    .subtitles
                    .iter()
                    .any(|s| is_portuguese_subtitle(s.language, s.raw_language.as_deref()));
            !(audio
                || embedded
                || has_external_by_name(files, &v.path, site == Site::Bj, site == Site::Bj))
        })
        .filter_map(|video| {
            siblings(files, sniffs, &video.path, site == Site::Bj)
                .into_iter()
                .map(|(path, sniff)| Reliance {
                    video,
                    path,
                    sniff,
                    standing: standing(sniff),
                })
                .filter(|r| r.standing.counts(site))
                .min_by_key(|r| r.standing)
        })
        .collect()
}

/// Os vídeos que ainda precisam da faixa PT embutida extraída para `.srt`:
/// sem áudio PT, sem arquivo PT pelo nome e sem `.srt` ao lado que seja
/// PT-BR completo confirmado. Só o PT-BR confirmado dispensa a extração.
pub(super) fn videos_needing_extraction<'a>(
    files: &[SourceFile],
    videos: &'a [ProbedVideo],
    sniffs: &Sniffs,
) -> Vec<&'a ProbedVideo> {
    videos
        .iter()
        .filter(|video| {
            let audio = video.media.audio.iter().any(is_portuguese_audio);
            !(audio
                || has_external_by_name(files, &video.path, true, true)
                || siblings(files, sniffs, &video.path, true)
                    .iter()
                    .any(|(_, s)| standing(s) == Standing::Confirmed))
        })
        .collect()
}

/// As legendas em arquivo sem idioma no nome (`<nome do vídeo>.srt`) que o
/// preparo leu, com o que se achou nelas; na mesma pasta se `same_folder`.
fn siblings<'a>(
    files: &'a [SourceFile],
    sniffs: &'a Sniffs,
    video: &str,
    same_folder: bool,
) -> Vec<(&'a str, &'a Sniff)> {
    let video_stem = stem(file_name(video)).to_lowercase();
    files
        .iter()
        .filter(|f| {
            is_subtitle_file(&f.path)
                && stem(file_name(&f.path)).to_lowercase() == video_stem
                && (!same_folder || parent(&f.path) == parent(video))
        })
        .filter_map(|f| sniffs.get(&f.path).map(|s| (f.path.as_str(), s)))
        .collect()
}

/// Os arquivos de legenda que o preparo precisa ler: os `.srt`/`.ass`/`.ssa`
/// com o mesmo nome-base de um vídeo (sem sufixo de idioma), e o vídeo
/// de cada um.
pub(super) fn sniff_targets<'a>(
    files: &'a [SourceFile],
    videos: &'a [ProbedVideo],
) -> Vec<(&'a str, &'a ProbedVideo)> {
    files
        .iter()
        .filter(|f| has_extension(&f.path, &["srt", "ass", "ssa"]))
        .filter_map(|f| {
            let name = stem(file_name(&f.path)).to_lowercase();
            videos
                .iter()
                .find(|v| stem(file_name(&v.path)).to_lowercase() == name)
                .map(|v| (f.path.as_str(), v))
        })
        .collect()
}

/// A primeira legenda sem idioma no nome ao lado do vídeo que foi lida.
fn sibling_sniff<'a>(
    files: &'a [SourceFile],
    sniffs: &'a Sniffs,
    video: &str,
    site: Site,
) -> Option<(&'a str, &'a Sniff)> {
    siblings(files, sniffs, video, site == Site::Bj)
        .into_iter()
        .next()
}

/// Complemento da mensagem de reprovação: o que o `.srt` ao lado, sem
/// idioma no nome, se revelou ser. Vazio se nenhum foi lido.
fn sibling_hint(
    files: &[SourceFile],
    sniffs: &Sniffs,
    missing: &[&ProbedVideo],
    site: Site,
) -> String {
    let notes: Vec<String> = missing
        .iter()
        .filter_map(|v| {
            sibling_sniff(files, sniffs, &v.path, site).map(|(path, sniff)| {
                let note = describe_sibling(path, sniff, site);
                if missing.len() > 1 {
                    format!("{}: {note}", file_name(&v.path))
                } else {
                    note
                }
            })
        })
        .take(3)
        .collect();
    if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join("; "))
    }
}

/// `o .srt ao lado é forçado (28 falas em 1 h 43 min) e em pt-PT`,
/// `o .srt ao lado é PT-BR completo (995 falas)`.
fn describe_sibling(path: &str, sniff: &Sniff, site: Site) -> String {
    let ext = extension(path).unwrap_or_else(|| "srt".to_owned());
    let lead = format!("o .{ext} ao lado");
    let cues = sniff.cues;
    let language = match sniff.language {
        SniffLanguage::PtBr => "em PT-BR",
        SniffLanguage::PtPt => "em pt-PT",
        SniffLanguage::Pt => "em português",
        SniffLanguage::English => "em inglês",
        SniffLanguage::Spanish => "em espanhol",
        SniffLanguage::Unknown => "de idioma não identificado",
    };
    let completeness = sniff.completeness();
    if completeness == Completeness::Forced {
        let span = sniff
            .video_secs
            .filter(|s| *s > 0.0)
            .map_or_else(String::new, |s| format!(" em {}", duration_label(s)));
        return format!("{lead} é forçado ({cues} falas{span}) e {language}");
    }
    let rule = match site {
        Site::Amigos => "a regra pede PT-BR",
        Site::Bj => "o BJ exige PT-BR",
    };
    match (sniff.language, completeness) {
        (SniffLanguage::English, _) => {
            format!("{lead} é em inglês ({cues} falas), não em português")
        }
        (SniffLanguage::Spanish, _) => {
            format!("{lead} é em espanhol ({cues} falas), não em português")
        }
        (SniffLanguage::Unknown, _) => {
            format!("{lead} não teve o idioma identificado ({cues} falas)")
        }
        (_, Completeness::Inconclusive) => {
            let share = sniff.coverage().map_or_else(String::new, |c| {
                format!(", {}% do filme", (c * 100.0).round())
            });
            let european = if sniff.language == SniffLanguage::PtPt {
                " (parece pt-PT)"
            } else {
                ""
            };
            format!(
                "não deu para confirmar se o .{ext} ao lado é completo ou forçado ({cues} falas{share}){european}: confira"
            )
        }
        (SniffLanguage::PtBr, _) => format!("{lead} é PT-BR completo ({cues} falas)"),
        (SniffLanguage::Pt, _) => format!(
            "{lead} parece português completo, mas a variante não foi confirmada: {rule}, confira"
        ),
        (_, _) => format!("{lead} é pt-PT completo ({cues} falas): {rule}"),
    }
}

/// `1 h 43 min`, `52 min`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn duration_label(secs: f64) -> String {
    let minutes = (secs / 60.0).round() as u64;
    if minutes >= 60 {
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    }
}

/// Há legenda PT em arquivo pelo sufixo de idioma do nome, mesmo nome-base
/// do vídeo, e na mesma pasta se `same_folder`. Com `full_only` (BJ), a
/// forçada não conta: ela não é a legenda exigida.
fn has_external_by_name(
    files: &[SourceFile],
    video: &str,
    same_folder: bool,
    full_only: bool,
) -> bool {
    let video_stem = stem(file_name(video)).to_lowercase();
    files.iter().any(|f| {
        if !is_subtitle_file(&f.path) || !subtitle_name_is_portuguese(&f.path) {
            return false;
        }
        if full_only && subtitle_name_is_forced(&f.path) {
            return false;
        }
        if same_folder && parent(&f.path) != parent(video) {
            return false;
        }
        let sub_stem = stem(file_name(&f.path)).to_lowercase();
        sub_stem
            .strip_prefix(&video_stem)
            .is_some_and(|rest| rest.starts_with(['.', '_', '-', ' ']))
    })
}

pub(super) fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// 3.11 / 4.15: série em andamento só em episódio avulso; encerrada só em
/// temporada completa.
fn amigos_series_scope(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "3.11 / 4.15";
    let facts = ctx.facts;
    if facts.kind == Kind::Movie {
        return None;
    }
    let Some(status) = facts.series_status else {
        return Some(Check::warn(
            RULE,
            "status da série (em andamento ou encerrada) desconhecido: confira à mão",
        ));
    };
    Some(match (status, facts.kind) {
        (SeriesStatus::Continuing, Kind::Season) => Check::fail(
            RULE,
            "série em andamento: o Amigos só aceita episódio avulso, não a temporada completa",
        ),
        (SeriesStatus::Continuing, _) => {
            Check::ok(RULE, "série em andamento: episódio avulso é o aceito")
        }
        (SeriesStatus::Ended, Kind::Episode) => Check::fail(
            RULE,
            "série encerrada: o Amigos só aceita temporada completa, não episódio avulso",
        ),
        (SeriesStatus::Ended, _) => coverage_check(
            RULE,
            season_coverage(ctx),
            "série encerrada com temporada completa",
        ),
    })
}

/// 4.14: fonte inferior a WEB-DL some quando já há WEB-DL Dual na mesma
/// resolução. A restrição à mesma resolução vem da 4.2.1 (resolução
/// diferente não é duplicidade).
fn inferior_source(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.14";
    let source = ctx.quality.source();
    if !is_below_web_dl(source) {
        return None;
    }
    let dual_web_dl = ctx.facts.destination.iter().find(|d| {
        d.quality.source() == Source::WebDl && d.audio == AudioKind::Dual && same_resolution(ctx, d)
    });
    Some(match dual_web_dl {
        Some(found) => Check::fail(
            RULE,
            format!(
                "fonte abaixo de WEB-DL ({}) e o Amigos já tem WEB-DL Dual em {}: \"{}\"",
                source_label(source),
                ctx.resolution_label(),
                found.title
            ),
        ),
        None => Check::ok(
            RULE,
            format!(
                "fonte abaixo de WEB-DL ({}), mas não há WEB-DL Dual na mesma resolução",
                source_label(source)
            ),
        ),
    })
}

// ---------------------------------------------------------------------------
// BJ
// ---------------------------------------------------------------------------

fn bj(ctx: &Context<'_>, checks: &mut Vec<Check>) {
    checks.extend(container(ctx));
    checks.extend(codec_choice(ctx));
    checks.push(web_or_bluray(ctx));
    checks.extend(minimum_resolution(ctx));
    checks.extend(hdr_only_at_top(ctx));
    checks.push(original_plus_portuguese(ctx));
    checks.extend(portuguese_subtitle_file(ctx));
    checks.extend(movie_contents(ctx));
    checks.push(site_reference(ctx, "4.1.2 / 4.1.3"));
    checks.extend(season_package(ctx));
    checks.extend(dual_already_released(ctx));
    checks.push(duplicate(ctx, Site::Bj, "4.2.1.2"));
    checks.push(archives(ctx, "4.1.7", "o BJ"));
}

/// 4.1.10.2: só MKV.
fn container(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.1.10.2";
    let bad = ctx
        .facts
        .videos
        .iter()
        .find(|v| !v.media.container.contains("matroska"))?;
    Some(Check::fail(
        RULE,
        format!(
            "{} está em {}: o BJ exige container MKV",
            file_name(&bad.path),
            bad.media.container
        ),
    ))
}

/// 4.1.10.2: H.264 até 1080p SDR; H.265/AV1 para 4K ou HDR.
///
/// Codec que o `ffprobe` nomeia fora da lista (`XviD`, MPEG-2...) em release
/// encodado é Fail: sem isso o pacote ficaria sem checagem de codec e
/// elegível. Remux guarda o codec do disco, então só nele a lacuna é tolerada.
fn codec_choice(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.1.10.2";
    let video = ctx.video()?;
    let Some(codec) = ctx.codec else {
        if ctx.quality.modifier() == Modifier::Remux {
            return None;
        }
        return Some(Check::fail(
            RULE,
            format!(
                "codec de vídeo \"{}\" fora do que o BJ aceita (H.264 até 1080p SDR; H.265 ou AV1 só em 4K ou HDR)",
                video.codec
            ),
        ));
    };
    let resolution = ctx.resolution;
    if matches!(codec, Codec::H265 | Codec::Av1)
        && resolution > 0
        && resolution <= 1080
        && video.hdr == Hdr::Sdr
    {
        let x264_there = ctx
            .facts
            .destination
            .iter()
            .find(|d| d.codec == Some(Codec::H264) && same_resolution(ctx, d));
        let label = codec.label();
        return Some(match x264_there {
            Some(found) => Check::fail(
                RULE,
                format!(
                    "{label} em {resolution}p SDR é proibido quando o BJ já tem x264 na mesma resolução: \"{}\"",
                    found.title
                ),
            ),
            None => Check::warn(
                RULE,
                format!(
                    "{label} em {resolution}p SDR só vale se a fonte não tem x264; confirme que não há"
                ),
            ),
        });
    }
    if codec == Codec::H264 && resolution >= 2160 {
        return Some(Check::warn(
            RULE,
            "4K em H.264: o BJ pede H.265 ou AV1 para 4K; confirme que não há opção melhor na fonte",
        ));
    }
    Some(Check::ok(
        RULE,
        format!("MKV em {} é aceito", codec.label()),
    ))
}

/// 4.2.1.14: só WEB-DL, Blu-ray e Remux.
fn web_or_bluray(ctx: &Context<'_>) -> Check {
    const RULE: &str = "4.2.1.14";
    let source = ctx.quality.source();
    match source {
        Source::WebDl | Source::Bluray => {
            Check::ok(RULE, format!("fonte {} é aceita", source_label(source)))
        }
        Source::Unknown => Check::fail(
            RULE,
            "não consegui identificar a fonte pelo nome do release: o BJ só aceita WEB-DL, Blu-ray e Remux",
        ),
        other => Check::fail(
            RULE,
            format!(
                "fonte {}: o BJ só aceita WEB-DL, Blu-ray e Remux (as demais só como raridade, que o usuário avalia à parte)",
                source_label(other)
            ),
        ),
    }
}

/// 4.2.1.14: resolução abaixo de 1080p não entra se o destino já tem 1080p
/// ou mais.
fn minimum_resolution(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.2.1.14";
    if ctx.resolution == 0 {
        return None;
    }
    let better = ctx
        .facts
        .destination
        .iter()
        .find(|d| d.quality.resolution() >= 1080)?;
    Some(if ctx.resolution < 1080 {
        Check::fail(
            RULE,
            format!(
                "{}p, mas o BJ já tem {}p: abaixo de 1080p não é aceito quando existe versão 1080p ou maior",
                ctx.resolution,
                better.quality.resolution()
            ),
        )
    } else {
        Check::ok(RULE, "resolução de 1080p ou mais")
    })
}

/// 4.2.1.9: HDR/Dolby Vision só na maior resolução do site para a obra.
fn hdr_only_at_top(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.2.1.9";
    let video = ctx.video()?;
    if video.hdr == Hdr::Sdr {
        return None;
    }
    let top = ctx
        .facts
        .destination
        .iter()
        .map(|d| d.quality.resolution())
        .max()
        .unwrap_or(0);
    Some(if ctx.resolution < top {
        Check::fail(
            RULE,
            format!(
                "{} em {}p, mas o BJ já tem {top}p: HDR só na maior resolução disponível",
                hdr_label(video.hdr),
                ctx.resolution
            ),
        )
    } else {
        Check::ok(
            RULE,
            format!(
                "{} na maior resolução do BJ ({}p)",
                hdr_label(video.hdr),
                ctx.resolution
            ),
        )
    })
}

/// 4.2.1.14.1: só o áudio original e uma dublagem PT; duas dublagens
/// oficiais (`MULTi`) o usuário confirma, então é aviso.
fn original_plus_portuguese(ctx: &Context<'_>) -> Check {
    const RULE: &str = "4.2.1.14.1";
    let Some(original) = ctx.facts.original_language else {
        return Check::warn(
            RULE,
            "idioma original da obra desconhecido: confira se o áudio é só original + PT",
        );
    };
    let outcome = worst_per_video(ctx, |media| {
        let (foreign, languages) = audio_languages(media, original);
        if !foreign.is_empty() {
            return (
                Status::Fail,
                format!(
                    "áudio em {}: o BJ aceita só o original + PT",
                    join_pt(&languages)
                ),
            );
        }
        let pt = media
            .audio
            .iter()
            .filter(|a| is_portuguese_audio(a))
            .count();
        let orig = media
            .audio
            .iter()
            .filter(|a| same_language(a.language, original))
            .count();
        let duplicated = if is_portuguese(original) {
            pt > 2
        } else {
            pt >= 2 || orig >= 2
        };
        if duplicated {
            (
                Status::Warn,
                "duas faixas no mesmo idioma: só vale com duas dublagens oficiais (MULTi); confirme".to_owned(),
            )
        } else {
            (Status::Ok, "áudio é só o original + PT".to_owned())
        }
    });
    outcome.into_check(RULE, ctx)
}

/// 4.2.1.5 / 4.2.1.4: sem áudio PT, a legenda PT tem de ser um arquivo
/// separado, na mesma pasta do vídeo. Legenda PT só embutida e em texto não
/// reprova: a pasta adaptada a extrai (`Ok` + `Warn` 4.2.1.6); em imagem, sim.
fn portuguese_subtitle_file(ctx: &Context<'_>) -> Vec<Check> {
    const RULE: &str = "4.2.1.5 / 4.2.1.4";
    let facts = ctx.facts;
    // A regra é para vídeo estrangeiro não dublado; obra nacional não precisa
    // de legenda PT, mesmo com o áudio sem tag de idioma (Amigos 3.3).
    if facts.original_language.is_some_and(is_portuguese) {
        return vec![Check::ok(
            RULE,
            "obra falada em português: não precisa de legenda",
        )];
    }
    if facts.videos.is_empty() {
        return vec![Check::fail(RULE, "sem vídeo lido")];
    }
    // Cada vídeo precisa de áudio PT ou de legenda PT em arquivo próprio, na
    // mesma pasta e com o mesmo nome-base.
    let missing = videos_without_portuguese(&facts.files, &facts.videos, &facts.sniffs, Site::Bj);
    if missing.is_empty() {
        return vec![bj_sibling_check(RULE, facts)];
    }
    let unfixable: Vec<&ProbedVideo> = missing
        .iter()
        .copied()
        .filter(|v| adapt::pick(&v.media.subtitles).is_err())
        .collect();
    if unfixable.is_empty() {
        return vec![
            Check::ok(
                RULE,
                format!(
                    "a legenda PT embutida será extraída para arquivo separado ({} de {} vídeos)",
                    missing.len(),
                    facts.videos.len(),
                ),
            ),
            Check::warn(
                "4.2.1.6",
                "confira que a legenda é oficial (não de tradutor)",
            ),
        ];
    }
    let obstacles: Vec<Obstacle> = unfixable
        .iter()
        .filter_map(|v| adapt::pick(&v.media.subtitles).err())
        .collect();
    let hint = if let Some(Obstacle::Image(codec)) =
        obstacles.iter().find(|o| matches!(o, Obstacle::Image(_)))
    {
        format!(
            " (a legenda PT embutida é imagem, {codec}: não dá para extrair para texto sem OCR)"
        )
    } else if let Some(Obstacle::UnknownCodec(codec)) = obstacles
        .iter()
        .find(|o| matches!(o, Obstacle::UnknownCodec(_)))
    {
        format!(" (a legenda PT embutida usa o codec {codec}, que não se sabe extrair para texto)")
    } else if unfixable.iter().any(|v| {
        v.media.subtitles.iter().any(|s| {
            !s.forced
                && adapt::is_commentary(s)
                && is_portuguese_subtitle(s.language, s.raw_language.as_deref())
        })
    }) && unfixable.iter().all(|v| {
        !v.media.subtitles.iter().any(|s| {
            !s.forced
                && !adapt::is_commentary(s)
                && is_portuguese_subtitle(s.language, s.raw_language.as_deref())
        })
    }) {
        " (só há legenda PT de comentários: a completa, do diálogo, tem de estar em arquivo separado)"
            .to_owned()
    } else if unfixable.iter().any(|v| {
        v.media
            .subtitles
            .iter()
            .any(|s| is_portuguese_subtitle(s.language, s.raw_language.as_deref()))
    }) {
        " (só há legenda PT forçada embutida: a completa tem de estar em arquivo separado)"
            .to_owned()
    } else {
        String::new()
    };
    vec![Check::fail(
        RULE,
        format!(
            "{} de {} vídeos sem áudio PT e sem legenda PT em arquivo separado, na mesma pasta e com o mesmo nome do vídeo: {}{hint}{}",
            unfixable.len(),
            facts.videos.len(),
            list_names(&unfixable),
            sibling_hint(&facts.files, &facts.sniffs, &unfixable, Site::Bj),
        ),
    )]
}

/// 4.2.1.5 quando todo vídeo tem português: `Ok`, ou `Warn` se algum depende
/// de um `.srt` ao lado que o conteúdo não confirma como PT-BR completo.
fn bj_sibling_check(rule: &'static str, facts: &Facts) -> Check {
    let relied = sibling_reliance(&facts.files, &facts.videos, &facts.sniffs, Site::Bj);
    if let Some(doubt) = relied.iter().find(|r| r.standing != Standing::Confirmed) {
        let mut note = describe_sibling(doubt.path, doubt.sniff, Site::Bj);
        if adapt::pick(&doubt.video.media.subtitles).is_ok() {
            note.push_str("; a legenda PT embutida será extraída para arquivo separado também");
        }
        return Check::warn(rule, note);
    }
    match relied.first() {
        Some(found) => Check::ok(
            rule,
            format!(
                "áudio PT ou legenda PT em arquivo separado: {}",
                describe_sibling(found.path, found.sniff, Site::Bj)
            ),
        ),
        None => Check::ok(rule, "áudio PT ou legenda PT em arquivo separado"),
    }
}

/// 4.2.1.10: filme com um só vídeo e no máximo quatro legendas.
fn movie_contents(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.2.1.10";
    if ctx.facts.kind != Kind::Movie {
        return None;
    }
    let videos = ctx.video_count();
    if videos > 1 {
        return Some(Check::fail(
            RULE,
            format!(
                "{videos} vídeos no torrent: filme leva um só (use Coleções para relacionar filmes)"
            ),
        ));
    }
    let external = ctx
        .facts
        .files
        .iter()
        .filter(|f| is_subtitle_file(&f.path))
        .count();
    // O limite fala dos arquivos que acompanham o vídeo; faixa embutida no
    // contêiner não é arquivo (WEB-DL multilegenda traz dezenas delas).
    // Os `.srt` que a pasta adaptada vai criar contam: existirão ao lado.
    let extracted = ctx.adaptations.len();
    let subtitles = external + extracted;
    Some(if subtitles > BJ_MAX_SUBTITLES {
        let detail = if extracted > 0 {
            format!(" ({external} em arquivo e {extracted} a extrair)")
        } else {
            String::new()
        };
        Check::fail(
            RULE,
            format!("{subtitles} legendas{detail}: o BJ aceita no máximo {BJ_MAX_SUBTITLES}"),
        )
    } else {
        Check::ok(RULE, "um vídeo e até quatro legendas")
    })
}

/// 4.2.1.13: só temporada completa avulsa; com a temporada completa já
/// lançada, o episódio avulso de mesmo formato, qualidade e áudio é excluído.
fn season_package(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.2.1.13";
    match ctx.facts.kind {
        Kind::Season => {}
        Kind::Episode => return Some(episode_beside_season(ctx)),
        Kind::Movie => return None,
    }
    let seasons = seasons_in(ctx);
    if seasons.len() > 1 {
        return Some(Check::fail(
            RULE,
            format!(
                "pacote com {} temporadas: o BJ só aceita temporada completa avulsa",
                seasons.len()
            ),
        ));
    }
    // Cobrir os exibidos não basta: com episódio por sair o pacote é parcial.
    if ctx.facts.season_pending > 0 {
        return Some(Check::fail(
            RULE,
            format!(
                "temporada ainda em exibição: {} episódios do catálogo sem ir ao ar; o BJ só aceita temporada completa",
                ctx.facts.season_pending
            ),
        ));
    }
    Some(coverage_check(
        RULE,
        season_coverage(ctx),
        "temporada completa e única",
    ))
}

/// 4.2.1.13 para episódio avulso: Warn se o destino já tem o pacote da mesma
/// temporada na mesma resolução, fonte e áudio. É aviso e não falha porque o
/// pacote só aparece se a busca o trouxe e o formato pode diferir em detalhes
/// que o título não mostra.
fn episode_beside_season(ctx: &Context<'_>) -> Check {
    const RULE: &str = "4.2.1.13";
    let season = parse_episode_title(&ctx.facts.release_name).map(|e| e.season);
    let pack = season.and_then(|season| {
        ctx.audio.and_then(|audio| {
            ctx.facts.destination.iter().find(|d| {
                d.season_pack == Some(season)
                    && same_resolution(ctx, d)
                    && same_source(ctx.quality, d.quality)
                    && comparable_audio(audio, d)
            })
        })
    });
    match pack {
        Some(pack) => Check::warn(
            RULE,
            format!(
                "o BJ já tem a temporada completa \"{}\" no mesmo formato, qualidade e áudio: episódio avulso é excluído",
                pack.title
            ),
        ),
        None => Check::ok(RULE, "nenhuma temporada completa igual no BJ"),
    }
}

/// 4.1.20: com um Dual lançado, só entram Duais. Vale para qualquer
/// resolução, como o texto da regra.
fn dual_already_released(ctx: &Context<'_>) -> Option<Check> {
    const RULE: &str = "4.1.20";
    let audio = ctx.audio?;
    let dual = ctx
        .facts
        .destination
        .iter()
        .find(|d| d.audio == AudioKind::Dual)?;
    Some(if audio == AudioKind::Dual {
        Check::ok(RULE, "o BJ já tem Dual e este também é Dual")
    } else if audio == AudioKind::Nacional {
        Check::ok(RULE, "obra nacional: a 4.1.20 não se aplica")
    } else {
        Check::fail(
            RULE,
            format!(
                "o BJ já tem Dual (\"{}\"): depois dele só entram versões Dual, e este é {}",
                dual.title,
                audio.label()
            ),
        )
    })
}

// ---------------------------------------------------------------------------
// Compartilhadas
// ---------------------------------------------------------------------------

/// Arquivo compactado (Amigos 3.2, BJ 4.1.7).
fn archives(ctx: &Context<'_>, rule: &str, site: &str) -> Check {
    match ctx.facts.files.iter().find(|f| is_archive(&f.path)) {
        Some(file) => Check::fail(
            rule,
            format!(
                "{} é arquivo compactado: {site} não aceita release compactado",
                file_name(&file.path)
            ),
        ),
        None => Check::ok(rule, "nenhum arquivo compactado"),
    }
}

/// Referência a site no nome da pasta ou dos arquivos (Amigos 5.6, BJ
/// 4.1.2 / 4.1.3).
fn site_reference(ctx: &Context<'_>, rule: &str) -> Check {
    let names = std::iter::once(ctx.facts.release_name.as_str())
        .chain(ctx.facts.files.iter().map(|f| f.path.as_str()));
    for path in names {
        if let Some(part) = path_with_site_reference(path) {
            return Check::fail(
                rule,
                format!(
                    "\"{part}\" cita um site no nome: o release não pode ser alterado, então não é elegível"
                ),
            );
        }
    }
    Check::ok(rule, "nome sem referência a site")
}

/// Duplicidade contra os releases do destino (Amigos 4.2.1, BJ 4.2.1.2).
///
/// Só compara releases de mesma resolução e fonte: outra resolução ou fonte
/// não é duplicidade. Entre esses, o Dual contém o dublado e o legendado
/// (Amigos); mesmo áudio com tamanho a menos de [`DUPLICATE_SIZE_TOLERANCE`]
/// e sem codec diferente é duplicado provável; o resto é aviso.
fn duplicate(ctx: &Context<'_>, site: Site, rule: &str) -> Check {
    let Some(audio) = ctx.audio else {
        return Check::warn(rule, "sem vídeo lido, não dá para comparar com o destino");
    };
    if ctx.facts.destination.is_empty() {
        return Check::ok(
            rule,
            format!("o {} não tem outro release desta obra", site.label()),
        );
    }
    let same: Vec<&DestinationRelease> = ctx
        .facts
        .destination
        .iter()
        .filter(|d| same_resolution(ctx, d) && same_source(ctx.quality, d.quality))
        .collect();
    if site == Site::Amigos
        && matches!(audio, AudioKind::Dublado | AudioKind::Legendado)
        && let Some(dual) = same.iter().find(|d| d.audio == AudioKind::Dual)
    {
        return Check::fail(
            rule,
            format!(
                "o {} já tem Dual em {} ({}), que contém o {}: \"{}\"",
                site.label(),
                ctx.resolution_label(),
                source_label(ctx.quality.source()),
                audio.label(),
                dual.title
            ),
        );
    }
    let mut warning: Option<Check> = None;
    for dest in same.iter().filter(|d| comparable_audio(audio, d)) {
        let diff = size_difference(ctx.size, dest.size);
        let codec_differs = matches!((ctx.codec, dest.codec), (Some(a), Some(b)) if a != b);
        if diff < DUPLICATE_SIZE_TOLERANCE && !codec_differs {
            return Check::fail(
                rule,
                format!(
                    "duplicado provável: mesma resolução ({}), fonte ({}) e áudio ({}), tamanho {:.1}% diferente de \"{}\"",
                    ctx.resolution_label(),
                    source_label(ctx.quality.source()),
                    audio.label(),
                    diff * 100.0,
                    dest.title
                ),
            );
        }
        let worth_warning = match site {
            Site::Amigos => true,
            Site::Bj => codec_differs || diff < BJ_SIMILAR_SIZE_TOLERANCE,
        };
        if worth_warning && warning.is_none() {
            let why = if codec_differs {
                format!(
                    "codec {} contra {}",
                    ctx.codec.map_or("desconhecido", Codec::label),
                    dest.codec.map_or("desconhecido", Codec::label)
                )
            } else {
                format!("tamanho {:.0}% diferente", diff * 100.0)
            };
            warning = Some(Check::warn(
                rule,
                format!(
                    "avaliar duplicidade: mesma resolução, fonte e áudio de \"{}\", mas {why}",
                    dest.title
                ),
            ));
        }
    }
    warning.unwrap_or_else(|| {
        Check::ok(
            rule,
            format!(
                "nada parecido no {} (resolução, fonte e áudio)",
                site.label()
            ),
        )
    })
}

// ---------------------------------------------------------------------------
// Ajudantes
// ---------------------------------------------------------------------------

/// Se o release do destino tem o mesmo tipo de áudio. O título do destino
/// nunca é lido como Nacional (só Dual, Dublado ou Legendado); obra nacional
/// se compara com qualquer release que não seja Dual, que é o que o mesmo
/// filme brasileiro aparece lá.
fn comparable_audio(audio: AudioKind, dest: &DestinationRelease) -> bool {
    if audio == AudioKind::Nacional {
        dest.audio != AudioKind::Dual
    } else {
        dest.audio == audio
    }
}

enum Coverage {
    Complete,
    /// Faltam episódios exibidos; `aired` é o total esperado.
    Incomplete {
        missing: Vec<(u16, u16)>,
        aired: usize,
    },
    /// Um vídeo cujo nome não diz que episódio é: não dá para conferir.
    Unparsed {
        file: String,
    },
    Unknown,
}

/// Pastas que guardam extras, não episódios.
const EXTRA_FOLDERS: &[&str] = &[
    "extras",
    "extra",
    "featurettes",
    "bonus",
    "special",
    "specials",
    "behind the scenes",
    "deleted scenes",
];

/// Amostra: o nome diz, ou é muito menor que a mediana dos vídeos.
fn is_sample(name: &str, size: u64, median: u64) -> bool {
    name.to_lowercase().contains("sample") || size.saturating_mul(5) < median
}

/// Mediana dos tamanhos (a superior, nos pares).
fn median(mut sizes: Vec<u64>) -> u64 {
    sizes.sort_unstable();
    sizes.get(sizes.len() / 2).copied().unwrap_or(0)
}

/// Os episódios que os vídeos do torrent cobrem, contra os exibidos da
/// temporada no catálogo. Compara o conjunto parseado dos nomes: amostra e
/// extras não contam, e episódio repetido não cobre um ausente.
fn season_coverage(ctx: &Context<'_>) -> Coverage {
    let Some(expected) = &ctx.facts.season_episodes else {
        return Coverage::Unknown;
    };
    let videos: Vec<&SourceFile> = ctx
        .facts
        .files
        .iter()
        .filter(|f| has_extension(&f.path, &VIDEO_EXTENSIONS))
        .collect();
    // A mediana não leva em conta as amostras declaradas no nome.
    let median = median(
        videos
            .iter()
            .filter(|f| !file_name(&f.path).to_lowercase().contains("sample"))
            .map(|f| f.size)
            .collect(),
    );
    let mut have: HashSet<(u16, u16)> = HashSet::new();
    for video in videos {
        let name = file_name(&video.path);
        let in_extras = parent(&video.path)
            .split('/')
            .any(|dir| EXTRA_FOLDERS.contains(&dir.to_lowercase().as_str()));
        if in_extras || is_sample(name, video.size, median) {
            continue;
        }
        let Some(parsed) = parse_episode_title(name) else {
            return Coverage::Unparsed {
                file: name.to_owned(),
            };
        };
        if parsed.special || parsed.season == 0 {
            continue;
        }
        if parsed.episodes.is_empty() {
            return Coverage::Unparsed {
                file: name.to_owned(),
            };
        }
        have.extend(parsed.episodes.iter().map(|e| (parsed.season, *e)));
    }
    let mut missing: Vec<(u16, u16)> = expected
        .iter()
        .copied()
        .filter(|pair| !have.contains(pair))
        .collect();
    missing.sort_unstable();
    if missing.is_empty() {
        Coverage::Complete
    } else {
        Coverage::Incomplete {
            missing,
            aired: expected.len(),
        }
    }
}

/// O resultado da cobertura como `Check`.
fn coverage_check(rule: &'static str, coverage: Coverage, complete: &str) -> Check {
    match coverage {
        Coverage::Incomplete { missing, aired } => {
            let shown: Vec<String> = missing
                .iter()
                .take(8)
                .map(|(s, e)| format!("S{s:02}E{e:02}"))
                .collect();
            let more = missing.len().saturating_sub(shown.len());
            let more = if more > 0 {
                format!(" e mais {more}")
            } else {
                String::new()
            };
            Check::fail(
                rule,
                format!(
                    "temporada incompleta: faltam {}{more} dos {aired} episódios exibidos",
                    shown.join(", ")
                ),
            )
        }
        Coverage::Unparsed { file } => Check::fail(
            rule,
            format!(
                "não deu para saber o episódio de {file}: o nome precisa trazer SxxExx para conferir a temporada"
            ),
        ),
        Coverage::Unknown => Check::warn(
            rule,
            "episódios da temporada desconhecidos no catálogo: confira se está completa",
        ),
        Coverage::Complete => Check::ok(rule, complete),
    }
}

/// Temporadas distintas que os nomes dos vídeos declaram.
fn seasons_in(ctx: &Context<'_>) -> Vec<u16> {
    let mut seasons: Vec<u16> = Vec::new();
    for file in ctx
        .facts
        .files
        .iter()
        .filter(|f| has_extension(&f.path, &VIDEO_EXTENSIONS))
    {
        if let Some(episode) = parse_episode_title(file_name(&file.path))
            && episode.season > 0
            && !seasons.contains(&episode.season)
        {
            seasons.push(episode.season);
        }
    }
    seasons
}

/// O pior resultado entre os vídeos; com mais de um vídeo a mensagem diz de
/// qual arquivo veio.
struct Outcome {
    status: Status,
    file: Option<String>,
    message: String,
}

impl Outcome {
    fn merge(&mut self, status: Status, file: &str, message: String) {
        if status > self.status || self.message.is_empty() {
            self.status = status;
            self.file = Some(file.to_owned());
            self.message = message;
        }
    }

    fn into_check(self, rule: &str, ctx: &Context<'_>) -> Check {
        let message = match self.file {
            Some(file) if self.status != Status::Ok && ctx.facts.videos.len() > 1 => {
                format!("{file}: {}", self.message)
            }
            _ => self.message,
        };
        Check::with(rule, self.status, message)
    }
}

fn worst_per_video(ctx: &Context<'_>, judge: impl Fn(&MediaFacts) -> (Status, String)) -> Outcome {
    let mut outcome = Outcome {
        status: Status::Ok,
        file: None,
        message: String::new(),
    };
    for video in &ctx.facts.videos {
        let (status, message) = judge(&video.media);
        outcome.merge(status, file_name(&video.path), message);
    }
    if ctx.facts.videos.is_empty() {
        outcome.message.push_str("sem vídeo lido");
        outcome.status = Status::Fail;
    }
    outcome
}

/// Faixas de áudio que não são o original nem PT, e todos os idiomas do
/// arquivo (sem repetir, na ordem das faixas) para a mensagem.
fn audio_languages(media: &MediaFacts, original: Language) -> (Vec<Language>, Vec<Language>) {
    let mut all: Vec<Language> = Vec::new();
    let mut foreign: Vec<Language> = Vec::new();
    for track in &media.audio {
        let language = if is_portuguese_audio(track) {
            Language::Portuguese
        } else {
            family(track.language)
        };
        if language == Language::Unknown {
            continue;
        }
        if !all.contains(&language) {
            all.push(language);
        }
        if !is_portuguese(language)
            && !same_language(language, original)
            && !foreign.contains(&language)
        {
            foreign.push(language);
        }
    }
    (foreign, all)
}

fn audio_kind(media: &MediaFacts, original: Option<Language>) -> AudioKind {
    let portuguese = media.audio.iter().any(is_portuguese_audio);
    if !portuguese {
        // Áudio sem tag de idioma (`und`) numa obra nacional é o original, não
        // um vídeo legendado.
        let untagged =
            !media.audio.is_empty() && media.audio.iter().all(|a| a.language == Language::Unknown);
        if untagged && original.is_some_and(is_portuguese) {
            return AudioKind::Nacional;
        }
        return AudioKind::Legendado;
    }
    if original.is_some_and(is_portuguese) {
        return AudioKind::Nacional;
    }
    let other = media
        .audio
        .iter()
        .any(|a| !is_portuguese_audio(a) && a.language != Language::Unknown);
    if other {
        AudioKind::Dual
    } else {
        AudioKind::Dublado
    }
}

/// Resolução nominal pela imagem: filme em tela larga tem menos linhas que
/// o nome (1920x800 é 1080p), então a largura também conta.
fn resolution_of(video: &VideoFacts) -> u16 {
    let (w, h) = (video.width, video.height);
    if w >= 3000 || h >= 1600 {
        2160
    } else if w >= 1800 || h >= 900 {
        1080
    } else if w >= 1200 || h >= 600 {
        720
    } else if w > 0 || h > 0 {
        480
    } else {
        0
    }
}

fn same_resolution(ctx: &Context<'_>, dest: &DestinationRelease) -> bool {
    let theirs = dest.quality.resolution();
    ctx.resolution > 0 && theirs > 0 && ctx.resolution == theirs
}

/// Mesma fonte; Remux é outra coisa que o Blu-ray reencodado.
fn same_source(a: Quality, b: Quality) -> bool {
    a.source() == b.source()
        && a.source() != Source::Unknown
        && (a.modifier() == Modifier::Remux) == (b.modifier() == Modifier::Remux)
}

/// `WEBRip` é inferior ao WEB-DL (a ordem do parser os inverte).
fn is_below_web_dl(source: Source) -> bool {
    !matches!(source, Source::WebDl | Source::Bluray)
}

fn size_difference(ours: u64, theirs: u64) -> f64 {
    if theirs == 0 {
        return f64::INFINITY;
    }
    #[allow(clippy::cast_precision_loss)]
    let (ours, theirs) = (ours as f64, theirs as f64);
    (ours - theirs).abs() / theirs
}

fn is_portuguese(language: Language) -> bool {
    matches!(language, Language::Portuguese | Language::PortugueseBr)
}

/// O ffprobe não distingue pt-BR de pt-PT: vale `por`, `pt`, `pob`, `pt-BR`.
fn is_portuguese_audio(track: &AudioFacts) -> bool {
    is_portuguese_tag(track.language, track.raw_language.as_deref())
}

pub(super) fn is_portuguese_subtitle(language: Language, raw: Option<&str>) -> bool {
    is_portuguese_tag(language, raw)
}

fn is_portuguese_tag(language: Language, raw: Option<&str>) -> bool {
    is_portuguese(language)
        || raw.is_some_and(|r| {
            let r = r.to_ascii_lowercase();
            r.starts_with("pt") || r == "por" || r == "pob"
        })
}

/// Variantes regionais contam como o mesmo idioma.
fn family(language: Language) -> Language {
    match language {
        Language::PortugueseBr => Language::Portuguese,
        Language::SpanishLatino => Language::Spanish,
        Language::Flemish => Language::Dutch,
        other => other,
    }
}

fn same_language(a: Language, b: Language) -> bool {
    family(a) == family(b)
}

fn language_pt(language: Language) -> String {
    match family(language) {
        Language::English => "inglês".to_owned(),
        Language::Spanish => "espanhol".to_owned(),
        Language::Portuguese => "português".to_owned(),
        Language::French => "francês".to_owned(),
        Language::German => "alemão".to_owned(),
        Language::Italian => "italiano".to_owned(),
        Language::Japanese => "japonês".to_owned(),
        Language::Korean => "coreano".to_owned(),
        Language::Chinese => "chinês".to_owned(),
        Language::Russian => "russo".to_owned(),
        other => other.name().to_lowercase(),
    }
}

/// "inglês, espanhol e português".
fn join_pt(languages: &[Language]) -> String {
    let names: Vec<String> = languages.iter().map(|l| language_pt(*l)).collect();
    match names.split_last() {
        None => String::new(),
        Some((only, [])) => only.clone(),
        Some((last, rest)) => format!("{} e {last}", rest.join(", ")),
    }
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Unknown => "desconhecida",
        Source::Cam => "CAM",
        Source::Telesync => "TS",
        Source::Telecine => "TC",
        Source::Workprint => "workprint",
        Source::Dvd => "DVD",
        Source::Tv => "HDTV",
        Source::WebDl => "WEB-DL",
        Source::WebRip => "WEBRip",
        Source::Bluray => "Blu-ray",
    }
}

const fn hdr_label(hdr: Hdr) -> &'static str {
    match hdr {
        Hdr::Sdr => "SDR",
        Hdr::Hdr10 => "HDR10",
        Hdr::Hlg => "HLG",
        Hdr::DolbyVision => "Dolby Vision",
    }
}

pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn extension(path: &str) -> Option<String> {
    let name = file_name(path);
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

fn has_extension(path: &str, list: &[&str]) -> bool {
    extension(path).is_some_and(|ext| list.contains(&ext.as_str()))
}

fn is_subtitle_file(path: &str) -> bool {
    has_extension(path, &SUBTITLE_EXTENSIONS)
}

/// `.rar`, `.zip`, `.7z`, `.r00`, `.r01`...
fn is_archive(path: &str) -> bool {
    let Some(ext) = extension(path) else {
        return false;
    };
    ARCHIVE_EXTENSIONS.contains(&ext.as_str())
        || (ext.len() == 3 && ext.starts_with('r') && ext[1..].chars().all(|c| c.is_ascii_digit()))
}

/// Marcas que vêm depois do idioma no nome da legenda: `Filme.pt.forced.srt`.
const SUBTITLE_QUALIFIERS: &[&str] = &["forced", "forcada", "sdh", "hi", "cc", "default"];

/// O nome marca a legenda como forçada (`Filme.pt-BR.forced.srt`).
fn subtitle_name_is_forced(path: &str) -> bool {
    let name = file_name(path).to_lowercase();
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    stem.split(|c: char| !c.is_alphanumeric())
        .any(|t| matches!(t, "forced" | "forcada"))
}

/// Idioma da legenda em arquivo, pelo nome: `Filme.pt-BR.srt`, `Filme.por.srt`.
///
/// Só vale o sufixo de idioma (o último trecho antes da extensão, descontadas
/// marcas como `forced`): o título em português também tem "por" e "brasil"
/// (`Tudo.Por.Amor.2020.eng.srt` é legenda em inglês).
fn subtitle_name_is_portuguese(path: &str) -> bool {
    let name = file_name(path).to_lowercase();
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    let mut tokens: Vec<&str> = stem
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    while tokens
        .last()
        .is_some_and(|t| SUBTITLE_QUALIFIERS.contains(t))
    {
        tokens.pop();
    }
    // `pt-BR` quebra em dois trechos.
    if let [.., "pt", "br"] = tokens.as_slice() {
        return true;
    }
    tokens.last().is_some_and(|token| {
        matches!(
            *token,
            "pt" | "por"
                | "pob"
                | "ptbr"
                | "portuguese"
                | "portugues"
                | "português"
                | "brazilian"
                | "brasil"
        )
    })
}

/// A parte do caminho que cita um site, se houver. O nome do arquivo é lido
/// sem a extensão.
fn path_with_site_reference(path: &str) -> Option<&str> {
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    for (index, part) in parts.iter().enumerate() {
        let name = if index == last && extension(part).is_some_and(|e| e.len() <= 4) {
            part.rsplit_once('.').map_or(*part, |(stem, _)| stem)
        } else {
            part
        };
        if name_cites_site(name) {
            return Some(part);
        }
    }
    None
}

fn name_cites_site(name: &str) -> bool {
    let lower = name.to_lowercase();
    let squashed: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    if squashed.contains("bjshare") || squashed.contains("amigosshare") {
        return true;
    }
    if lower.contains("www.") {
        return true;
    }
    // `.com` só conta como domínio quando termina a palavra (`.network` não).
    // Seguido de `.`, só vale `.com.br`: em `Casamento.Com.Data.Marcada` o
    // "com" é a preposição do título, separada por pontos.
    [".com", ".net", ".org"].iter().any(|tld| {
        lower.match_indices(tld).any(|(at, _)| {
            let rest = &lower[at + tld.len()..];
            match rest.chars().next() {
                None => true,
                Some('.') => rest
                    .strip_prefix(".br")
                    .is_some_and(|after| after.chars().next().is_none_or(|c| !c.is_alphanumeric())),
                Some(c) => !c.is_alphanumeric(),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn video(codec: &str, width: u32, height: u32, hdr: Hdr) -> VideoFacts {
        VideoFacts {
            codec: codec.to_owned(),
            width,
            height,
            pix_fmt: None,
            bit_depth: Some(8),
            hdr,
        }
    }

    fn audio(language: Language, raw: &str) -> AudioFacts {
        AudioFacts {
            language,
            raw_language: Some(raw.to_owned()),
            codec: "eac3".to_owned(),
            channels: Some(6),
            title: None,
        }
    }

    fn por() -> AudioFacts {
        audio(Language::Portuguese, "por")
    }

    fn eng() -> AudioFacts {
        audio(Language::English, "eng")
    }

    fn media(container: &str, video: VideoFacts, audio: Vec<AudioFacts>) -> MediaFacts {
        MediaFacts {
            container: container.to_owned(),
            duration_seconds: Some(6720.0),
            video: Some(video),
            audio,
            subtitles: Vec::new(),
        }
    }

    fn file(path: &str, size: u64) -> SourceFile {
        SourceFile {
            path: path.to_owned(),
            size,
        }
    }

    /// Filme 1080p WEB-DL H.264, MKV, áudio PT + inglês, obra em inglês, 10 GB
    /// e nada no destino: passa em todas as regras dos dois sites.
    fn movie() -> Facts {
        let path =
            "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP/Filme.2024.1080p.WEB-DL.DUAL.x264-GRP.mkv";
        Facts {
            kind: Kind::Movie,
            release_name: "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP".to_owned(),
            files: vec![file(path, 10 * GB)],
            videos: vec![ProbedVideo {
                path: path.to_owned(),
                media: media(
                    "matroska,webm",
                    video("h264", 1920, 1080, Hdr::Sdr),
                    vec![por(), eng()],
                ),
            }],
            original_language: Some(Language::English),
            series_status: None,
            season_episodes: None,
            season_pending: 0,
            destination: Vec::new(),
            sniffs: Sniffs::new(),
        }
    }

    fn set_video(facts: &mut Facts, f: impl FnOnce(&mut MediaFacts)) {
        f(&mut facts.videos[0].media);
    }

    fn rename(facts: &mut Facts, name: &str) {
        facts.release_name = name.to_owned();
    }

    fn season(episodes: u32) -> Facts {
        let mut facts = movie();
        facts.kind = Kind::Season;
        facts.release_name = "Serie.S01.1080p.WEB-DL.DUAL.x264-GRP".to_owned();
        facts.files.clear();
        facts.videos.clear();
        for n in 1..=episodes {
            let path =
                format!("Serie.S01.1080p.WEB-DL.DUAL.x264-GRP/Serie.S01E{n:02}.1080p.WEB-DL.mkv");
            facts.files.push(file(&path, GB));
            facts.videos.push(ProbedVideo {
                path,
                media: media(
                    "matroska,webm",
                    video("h264", 1920, 1080, Hdr::Sdr),
                    vec![por(), eng()],
                ),
            });
        }
        facts
    }

    /// Os `n` primeiros episódios exibidos da temporada 1.
    fn aired(n: u16) -> Vec<(u16, u16)> {
        (1..=n).map(|e| (1, e)).collect()
    }

    /// Troca o nome de um vídeo (em `files` e na sonda).
    fn rename_video(facts: &mut Facts, index: usize, name: &str) {
        let folder = parent(&facts.files[index].path).to_owned();
        let path = format!("{folder}/{name}");
        facts.files[index].path.clone_from(&path);
        facts.videos[index].path = path;
    }

    fn dest(title: &str, size: u64) -> DestinationRelease {
        DestinationRelease::new(title, size, Kind::Movie)
    }

    fn status(site: Site, facts: &Facts, rule: &str) -> Option<Status> {
        check(site, facts).status_of(rule)
    }

    #[test]
    fn indexador_vira_site() {
        assert_eq!(Site::from_indexer("bjsharecustom"), Some(Site::Bj));
        assert_eq!(Site::from_indexer("amigosshare"), Some(Site::Amigos));
        assert_eq!(Site::from_indexer("outro"), None);
    }

    #[test]
    fn filme_certo_e_elegivel_nos_dois_sites() {
        for site in [Site::Amigos, Site::Bj] {
            let report = check(site, &movie());
            assert_eq!(report.verdict, Verdict::Elegivel, "{site:?}: {report:?}");
            assert!(!report.has_warnings(), "{site:?}: {report:?}");
        }
    }

    #[test]
    fn veredito_e_inelegivel_com_qualquer_falha() {
        let mut facts = movie();
        facts.files.push(file("Filme/dados.rar", GB));
        assert_eq!(check(Site::Amigos, &facts).verdict, Verdict::Inelegivel);
    }

    #[test]
    fn sem_video_lido_e_falha() {
        let mut facts = movie();
        facts.videos.clear();
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("sonda"), Some(Status::Fail));
        assert_eq!(report.verdict, Verdict::Inelegivel);
    }

    #[test]
    fn dublagem_com_codigo_pt_br_conta_como_portugues() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![audio(Language::Unknown, "pt-br"), eng()];
        });
        assert_eq!(status(Site::Bj, &facts, "4.2.1.14.1"), Some(Status::Ok));
    }

    // --- Relatório ---

    #[test]
    fn relatorio_vai_e_volta_pelo_json() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme 2024 1080p WEB-DL Dual x264", 10 * GB)];
        let report = check(Site::Bj, &facts);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["verdict"], "inelegivel");
        assert_eq!(json["destination"][0]["quality"], "WEBDL-1080p");
        assert_eq!(json["destination"][0]["audio"], "dual");
        let back: Report = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn release_do_destino_e_parseado() {
        let dual = dest("Filme.2024.1080p.WEB-DL.DUAL.x265-GRP", GB);
        assert_eq!(dual.quality, Quality::WebDl1080p);
        assert_eq!(dual.audio, AudioKind::Dual);
        assert_eq!(dual.codec, Some(Codec::H265));
        let dublado = dest("Filme.2024.1080p.WEB-DL.DUBLADO.x264-GRP", GB);
        assert_eq!(dublado.audio, AudioKind::Dublado);
        let legendado = dest("Filme.2024.2160p.BluRay.x265-GRP", GB);
        assert_eq!(legendado.audio, AudioKind::Legendado);
        assert_eq!(legendado.quality, Quality::Bluray2160p);
    }

    // --- Amigos ---

    #[test]
    fn amigos_3_2_compactado() {
        let mut facts = movie();
        assert_eq!(status(Site::Amigos, &facts, "3.2"), Some(Status::Ok));
        facts.files.push(file("Filme.2024/Filme.r00", GB));
        assert_eq!(status(Site::Amigos, &facts, "3.2"), Some(Status::Fail));
    }

    #[test]
    fn amigos_3_3_obra_estrangeira_precisa_de_portugues() {
        let mut facts = movie();
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Ok));
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        let report = check(Site::Amigos, &facts);
        assert_eq!(report.status_of("3.3"), Some(Status::Fail));
        assert!(report.checks.iter().any(|c| {
            c.message
                .contains("inglês com 1 de 1 vídeos sem áudio nem legenda em português")
        }));
    }

    #[test]
    fn amigos_3_3_legenda_em_arquivo_basta() {
        let mut facts = movie();
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        facts.files.push(file(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP/Filme.2024.1080p.WEB-DL.DUAL.x264-GRP.pt-BR.srt",
            1000,
        ));
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Ok));
    }

    #[test]
    fn amigos_3_3_titulo_em_portugues_nao_faz_legenda_pt() {
        let mut facts = movie();
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        facts.files.push(file(
            "Tudo.Por.Amor.2020.1080p.WEB-DL/Tudo.Por.Amor.2020.1080p.WEB-DL.eng.srt",
            1000,
        ));
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
    }

    #[test]
    fn bj_4_2_1_5_titulo_em_portugues_nao_faz_legenda_pt() {
        let mut facts = movie();
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        facts.files.push(file(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP/Brasil.Por.Amor.eng.srt",
            1000,
        ));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
    }

    #[test]
    fn idioma_da_legenda_vem_do_sufixo() {
        for ok in [
            "Filme.pt.srt",
            "Filme.PT-BR.srt",
            "Filme.por.forced.srt",
            "Filme.2020.Portuguese.srt",
            "pt.srt",
            "Filme.ptbr.sdh.ass",
        ] {
            assert!(subtitle_name_is_portuguese(ok), "{ok}");
        }
        for no in [
            "Tudo.Por.Amor.2020.eng.srt",
            "Brasil.Total.en.srt",
            "Filme.eng.pt.Documentario.srt",
            "Filme.srt",
        ] {
            assert!(!subtitle_name_is_portuguese(no), "{no}");
        }
    }

    #[test]
    fn amigos_3_3_obra_nacional_dispensa_legenda() {
        let mut facts = movie();
        facts.original_language = Some(Language::Portuguese);
        set_video(&mut facts, |m| m.audio = vec![por()]);
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Ok));
    }

    #[test]
    fn amigos_5_6_referencia_a_site() {
        let mut facts = movie();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Ok));
        facts.files[0].path = "Filme.2024-BJ-Share/Filme.2024.mkv".to_owned();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Fail));
    }

    #[test]
    fn referencia_a_site_ignora_palavra_que_so_comeca_igual() {
        let mut facts = movie();
        facts.files[0].path = "Filme.2024.Network.Edition/Filme.mkv".to_owned();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Ok));
        facts.files[0].path = "Filme.2024/www.site.com.Filme.mkv".to_owned();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Fail));
        facts.files[0].path = "Filme.2024/Filme.AmigosShare.mkv".to_owned();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Fail));
    }

    #[test]
    fn referencia_a_site_ignora_com_como_preposicao_do_titulo() {
        let mut facts = movie();
        facts.files[0].path = "Casamento.Com.Data.Marcada.2023.1080p.WEB-DL/Filme.mkv".to_owned();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Ok));
        facts.files[0].path = "Filme.2024/site.com.br.Filme.mkv".to_owned();
        assert_eq!(status(Site::Amigos, &facts, "5.6"), Some(Status::Fail));
    }

    #[test]
    fn amigos_obra_nacional_duplicada_no_destino_e_detectada() {
        let mut facts = movie();
        facts.original_language = Some(Language::Portuguese);
        set_video(&mut facts, |m| m.audio = vec![por()]);
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.Nacional.x264-OUTRO", 10 * GB)];
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Fail));
    }

    #[test]
    fn amigos_serie_em_andamento_so_episodio() {
        let mut facts = season(3);
        facts.series_status = Some(SeriesStatus::Continuing);
        facts.season_episodes = Some(aired(3));
        assert_eq!(
            status(Site::Amigos, &facts, "3.11 / 4.15"),
            Some(Status::Fail)
        );
        facts.kind = Kind::Episode;
        assert_eq!(
            status(Site::Amigos, &facts, "3.11 / 4.15"),
            Some(Status::Ok)
        );
    }

    #[test]
    fn amigos_serie_encerrada_so_temporada_completa() {
        let mut facts = season(3);
        facts.series_status = Some(SeriesStatus::Ended);
        facts.season_episodes = Some(aired(3));
        assert_eq!(
            status(Site::Amigos, &facts, "3.11 / 4.15"),
            Some(Status::Ok)
        );
        facts.kind = Kind::Episode;
        assert_eq!(
            status(Site::Amigos, &facts, "3.11 / 4.15"),
            Some(Status::Fail)
        );
    }

    #[test]
    fn amigos_temporada_que_nao_cobre_os_episodios_exibidos() {
        let mut facts = season(3);
        facts.series_status = Some(SeriesStatus::Ended);
        facts.season_episodes = Some(aired(10));
        let report = check(Site::Amigos, &facts);
        assert_eq!(report.status_of("3.11 / 4.15"), Some(Status::Fail));
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.message.contains("faltam S01E04, S01E05"))
        );
    }

    #[test]
    fn amigos_filme_nao_tem_regra_de_serie() {
        assert_eq!(status(Site::Amigos, &movie(), "3.11 / 4.15"), None);
    }

    #[test]
    fn amigos_duplicado_provavel_com_mesmo_audio_e_tamanho_parecido() {
        let mut facts = movie();
        facts.destination = vec![dest(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO",
            10 * GB + GB / 33,
        )];
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Fail));
    }

    #[test]
    fn amigos_dual_no_destino_barra_dublado_e_legendado() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO", 4 * GB)];
        rename(&mut facts, "Filme.2024.1080p.WEB-DL.DUBLADO.x264-GRP");
        set_video(&mut facts, |m| m.audio = vec![por()]);
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Fail));
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        facts.files.push(file(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP/Filme.2024.1080p.WEB-DL.DUAL.x264-GRP.pt-BR.srt",
            1000,
        ));
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Fail));
    }

    #[test]
    fn amigos_dual_novo_com_dublado_no_destino_nao_e_duplicado() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUBLADO.x264-OUTRO", 10 * GB)];
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Ok));
    }

    #[test]
    fn amigos_codec_diferente_vira_aviso() {
        let mut facts = movie();
        // Tamanho idêntico, mas o destino é x265: avisa em vez de barrar.
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x265-OUTRO", 10 * GB)];
        let report = check(Site::Amigos, &facts);
        assert_eq!(report.status_of("4.2.1"), Some(Status::Warn));
        assert_eq!(report.verdict, Verdict::Elegivel);
    }

    #[test]
    fn amigos_tamanho_diferente_com_mesmo_resto_vira_aviso() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO", 4 * GB)];
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Warn));
    }

    #[test]
    fn amigos_resolucao_ou_fonte_diferente_nao_e_duplicidade() {
        let mut facts = movie();
        facts.destination = vec![
            dest("Filme.2024.720p.WEB-DL.DUAL.x264-OUTRO", 10 * GB),
            dest("Filme.2024.1080p.BluRay.DUAL.x264-OUTRO", 10 * GB),
        ];
        assert_eq!(status(Site::Amigos, &facts, "4.2.1"), Some(Status::Ok));
    }

    #[test]
    fn amigos_4_14_fonte_inferior_com_web_dl_dual() {
        let mut facts = movie();
        rename(&mut facts, "Filme.2024.1080p.WEBRip.x264-GRP");
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO", 10 * GB)];
        assert_eq!(status(Site::Amigos, &facts, "4.14"), Some(Status::Fail));
    }

    #[test]
    fn amigos_4_14_fonte_inferior_sem_web_dl_dual_passa() {
        let mut facts = movie();
        rename(&mut facts, "Filme.2024.1080p.WEBRip.x264-GRP");
        assert_eq!(status(Site::Amigos, &facts, "4.14"), Some(Status::Ok));
        // WEB-DL não é inferior: a regra nem se aplica.
        assert_eq!(status(Site::Amigos, &movie(), "4.14"), None);
    }

    // --- BJ ---

    #[test]
    fn bj_4_1_10_2_container_mkv() {
        let mut facts = movie();
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Ok));
        set_video(&mut facts, |m| {
            m.container = "mov,mp4,m4a,3gp,3g2,mj2".to_owned();
        });
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Fail));
    }

    #[test]
    fn bj_h265_ate_1080p_sdr_e_aviso() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.video = Some(video("hevc", 1920, 1080, Hdr::Sdr));
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.1.10.2"), Some(Status::Warn));
        assert_eq!(report.verdict, Verdict::Elegivel);
    }

    #[test]
    fn bj_h265_ate_1080p_sdr_falha_se_o_destino_tem_x264() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.video = Some(video("hevc", 1920, 1080, Hdr::Sdr));
        });
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.x264-OUTRO", 3 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Fail));
    }

    #[test]
    fn bj_av1_ate_1080p_sdr_e_aviso_e_falha_se_o_destino_tem_x264() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.video = Some(video("av1", 1920, 1080, Hdr::Sdr));
        });
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Warn));
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.x264-OUTRO", 3 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Fail));
        // Em 4K o AV1 é o esperado.
        set_video(&mut facts, |m| {
            m.video = Some(video("av1", 3840, 2160, Hdr::Sdr));
        });
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Ok));
    }

    #[test]
    fn bj_codec_fora_da_lista_em_release_encodado_falha() {
        let mut facts = movie();
        rename(&mut facts, "Filme.2024.1080p.BDRip.XviD-GRP");
        set_video(&mut facts, |m| {
            m.video = Some(video("mpeg4", 1920, 1080, Hdr::Sdr));
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.1.10.2"), Some(Status::Fail));
        assert_eq!(report.verdict, Verdict::Inelegivel);
        // Remux guarda o codec do disco: sem checagem de codec, como antes.
        rename(&mut facts, "Filme.2024.1080p.BluRay.Remux-GRP");
        set_video(&mut facts, |m| {
            m.video = Some(video("mpeg2video", 1920, 1080, Hdr::Sdr));
        });
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), None);
    }

    #[test]
    fn bj_4k_em_h264_e_aviso_e_em_h265_passa() {
        let mut facts = movie();
        rename(&mut facts, "Filme.2024.2160p.WEB-DL.DUAL.x264-GRP");
        set_video(&mut facts, |m| {
            m.video = Some(video("h264", 3840, 2160, Hdr::Sdr));
        });
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Warn));
        set_video(&mut facts, |m| {
            m.video = Some(video("hevc", 3840, 2160, Hdr::Sdr));
        });
        assert_eq!(status(Site::Bj, &facts, "4.1.10.2"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_2_1_14_fonte_aceita_e_recusada() {
        let mut facts = movie();
        assert_eq!(status(Site::Bj, &facts, "4.2.1.14"), Some(Status::Ok));
        rename(&mut facts, "Filme.2024.1080p.HDTV.x264-GRP");
        assert_eq!(status(Site::Bj, &facts, "4.2.1.14"), Some(Status::Fail));
        rename(&mut facts, "Filme.2024.1080p.BluRay.x264-GRP");
        assert_eq!(status(Site::Bj, &facts, "4.2.1.14"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_2_1_14_resolucao_minima() {
        let mut facts = movie();
        rename(&mut facts, "Filme.2024.720p.WEB-DL.DUAL.x264-GRP");
        set_video(&mut facts, |m| {
            m.video = Some(video("h264", 1280, 720, Hdr::Sdr));
        });
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO", 10 * GB)];
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.14"), Some(Status::Fail));
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.message.contains("abaixo de 1080p"))
        );
        // Sem 1080p no destino, 720p passa.
        facts.destination = vec![dest("Filme.2024.720p.WEB-DL.LEG.x264-OUTRO", 3 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.14"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_2_1_9_hdr_so_na_maior_resolucao() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.video = Some(video("hevc", 1920, 1080, Hdr::Hdr10));
        });
        facts.destination = vec![dest("Filme.2024.2160p.WEB-DL.DUAL.x265-OUTRO", 20 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.9"), Some(Status::Fail));
        facts.destination = vec![dest("Filme.2024.720p.WEB-DL.DUAL.x264-OUTRO", 3 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.9"), Some(Status::Ok));
        // SDR: a regra não se aplica.
        assert_eq!(status(Site::Bj, &movie(), "4.2.1.9"), None);
    }

    #[test]
    fn bj_4_2_1_14_1_audio_alem_do_original_e_pt() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng(), audio(Language::Spanish, "spa"), por()];
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.14.1"), Some(Status::Fail));
        assert!(report.checks.iter().any(|c| c.message
            == "áudio em inglês, espanhol e português: o BJ aceita só o original + PT (4.2.1.14.1)"));
    }

    #[test]
    fn bj_4_2_1_14_1_original_mais_pt_passa_e_duas_dublagens_avisam() {
        let mut facts = movie();
        assert_eq!(status(Site::Bj, &facts, "4.2.1.14.1"), Some(Status::Ok));
        set_video(&mut facts, |m| {
            m.audio = vec![eng(), por(), audio(Language::PortugueseBr, "pob")];
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.14.1"), Some(Status::Warn));
        assert_eq!(report.verdict, Verdict::Elegivel);
    }

    #[test]
    fn bj_4_2_1_5_sem_audio_pt_precisa_de_legenda_em_arquivo() {
        let mut facts = movie();
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        // Nem embutida nem em arquivo.
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
        // Só embutida, em texto: a pasta adaptada a extrai.
        set_video(&mut facts, |m| m.subtitles = vec![embedded_pt()]);
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.5 / 4.2.1.4"), Some(Status::Ok));
        assert_eq!(report.status_of("4.2.1.6"), Some(Status::Warn));
        assert_eq!(report.verdict, Verdict::Elegivel);
        assert!(report.checks.iter().any(|c| {
            c.message
                .contains("a legenda PT embutida será extraída para arquivo separado")
        }));
        assert_eq!(report.adaptations.len(), 1);
        // Só embutida, em imagem: continua reprovando.
        set_video(&mut facts, |m| {
            m.subtitles = vec![crate::mediainfo::SubtitleFacts {
                kind: crate::mediainfo::SubtitleKind::Image,
                codec: "hdmv_pgs_subtitle".to_owned(),
                ..embedded_pt()
            }];
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.5 / 4.2.1.4"), Some(Status::Fail));
        assert!(report.checks.iter().any(|c| c.message.contains("é imagem")));
        assert!(report.adaptations.is_empty());
        set_video(&mut facts, |m| m.subtitles = vec![embedded_pt()]);
        // Em arquivo, na mesma pasta.
        facts.files.push(file(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP/Filme.2024.1080p.WEB-DL.DUAL.x264-GRP.pt-BR.srt",
            1000,
        ));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Ok)
        );
    }

    /// Temporada de dois episódios, ambos só com áudio inglês, obra em inglês.
    fn two_english_episodes() -> Facts {
        let mut facts = season(2);
        facts.original_language = Some(Language::English);
        for video in &mut facts.videos {
            video.media.audio = vec![eng()];
        }
        facts
    }

    fn embedded_pt() -> crate::mediainfo::SubtitleFacts {
        crate::mediainfo::SubtitleFacts {
            index: 0,
            kind: crate::mediainfo::SubtitleKind::Text,
            language: Language::Portuguese,
            raw_language: Some("por".to_owned()),
            codec: "subrip".to_owned(),
            forced: false,
            title: None,
        }
    }

    #[test]
    fn amigos_3_3_legenda_de_um_episodio_nao_vale_para_o_pacote() {
        let mut facts = two_english_episodes();
        let folder = parent(&facts.files[0].path).to_owned();
        // Legenda PT só do E01.
        facts.files.push(file(
            &format!("{folder}/Serie.S01E01.1080p.WEB-DL.pt-BR.srt"),
            1000,
        ));
        let report = check(Site::Amigos, &facts);
        assert_eq!(report.status_of("3.3"), Some(Status::Fail));
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.message.contains("Serie.S01E02.1080p.WEB-DL.mkv")
                    && !c.message.contains("Serie.S01E01.1080p.WEB-DL.mkv"))
        );
        // Legenda embutida só no E01: o E02 continua sem.
        let mut facts = two_english_episodes();
        facts.videos[0].media.subtitles = vec![embedded_pt()];
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        // Cada um com a sua (embutida num, em arquivo no outro): passa. O
        // sufixo `.forced` e o idioma `.por` também associam.
        facts.files.push(file(
            "Serie.S01.1080p.WEB-DL.DUAL.x264-GRP/Subs/Serie.S01E02.1080p.WEB-DL.por.forced.srt",
            1000,
        ));
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_2_1_5_legenda_precisa_ser_do_video_e_da_mesma_pasta() {
        let mut facts = two_english_episodes();
        let folder = parent(&facts.files[0].path).to_owned();
        facts.files.push(file(
            &format!("{folder}/Serie.S01E01.1080p.WEB-DL.pt-BR.srt"),
            1000,
        ));
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.5 / 4.2.1.4"), Some(Status::Fail));
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.message.contains("1 de 2") && c.message.contains("S01E02"))
        );
        // A do E02, mas em outra pasta: não vale.
        facts
            .files
            .push(file("Subs/Serie.S01E02.1080p.WEB-DL.pt-BR.srt", 1000));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
        // Embutida em imagem também não vale no BJ.
        facts.videos[1].media.subtitles = vec![crate::mediainfo::SubtitleFacts {
            kind: crate::mediainfo::SubtitleKind::Image,
            codec: "hdmv_pgs_subtitle".to_owned(),
            ..embedded_pt()
        }];
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
        // A forçada em arquivo não é a legenda exigida.
        facts.files.push(file(
            &format!("{folder}/Serie.S01E02.1080p.WEB-DL.pt.forced.srt"),
            1000,
        ));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
        facts.files.push(file(
            &format!("{folder}/Serie.S01E02.1080p.WEB-DL.pt.srt"),
            1000,
        ));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Ok)
        );
    }

    #[test]
    fn bj_legenda_forcada_externa_nao_impede_extrair_a_completa_embutida() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = vec![embedded_pt()];
        });
        let stem = stem(file_name(&facts.videos[0].path)).to_owned();
        let folder = parent(&facts.videos[0].path).to_owned();
        facts
            .files
            .push(file(&format!("{folder}/{stem}.pt-BR.forced.srt"), 1000));
        assert_eq!(
            videos_without_portuguese(&facts.files, &facts.videos, &facts.sniffs, Site::Bj).len(),
            1
        );
        assert!(
            videos_without_portuguese(&facts.files, &facts.videos, &facts.sniffs, Site::Amigos)
                .is_empty()
        );
    }

    #[test]
    fn legenda_de_outro_episodio_de_nome_parecido_nao_associa() {
        let mut facts = season(1);
        facts.original_language = Some(Language::English);
        facts.videos[0].media.audio = vec![eng()];
        let folder = parent(&facts.files[0].path).to_owned();
        // `...E01` não é prefixo de `...E010`.
        facts.files[0].path = format!("{folder}/Serie.S01E01.mkv");
        facts.videos[0].path.clone_from(&facts.files[0].path);
        facts
            .files
            .push(file(&format!("{folder}/Serie.S01E010.pt.srt"), 1000));
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
    }

    #[test]
    fn bj_4_2_1_5_legenda_em_outra_pasta_nao_vale() {
        let mut facts = movie();
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        facts.files.push(file("Legendas/Filme.pt-BR.srt", 1000));
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
    }

    #[test]
    fn bj_4_2_1_5_com_audio_pt_passa() {
        assert_eq!(
            status(Site::Bj, &movie(), "4.2.1.5 / 4.2.1.4"),
            Some(Status::Ok)
        );
    }

    #[test]
    fn bj_4_2_1_5_legenda_embutida_so_forcada_continua_reprovando() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = vec![crate::mediainfo::SubtitleFacts {
                forced: true,
                ..embedded_pt()
            }];
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.5 / 4.2.1.4"), Some(Status::Fail));
        assert!(report.checks.iter().any(|c| c.message.contains("forçada")));
    }

    #[test]
    fn bj_4_2_1_5_legenda_embutida_so_de_comentarios_reprova() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = vec![crate::mediainfo::SubtitleFacts {
                title: Some("Comentários do diretor".to_owned()),
                ..embedded_pt()
            }];
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.5 / 4.2.1.4"), Some(Status::Fail));
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.message.contains("legenda PT de comentários")),
            "{:?}",
            report.checks
        );
        assert!(report.adaptations.is_empty());
    }

    #[test]
    fn bj_4_2_1_5_obra_nacional_nao_gera_adaptacao() {
        let mut facts = movie();
        facts.original_language = Some(Language::Portuguese);
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = vec![embedded_pt()];
        });
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.5 / 4.2.1.4"), Some(Status::Ok));
        assert!(report.adaptations.is_empty());
        assert_eq!(report.status_of("4.2.1.6"), None);
    }

    #[test]
    fn amigos_com_legenda_embutida_nao_lista_adaptacao() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = vec![embedded_pt()];
        });
        assert!(check(Site::Amigos, &facts).adaptations.is_empty());
    }

    #[test]
    fn bj_4_2_1_10_conta_as_legendas_que_serao_extraidas() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = vec![
                embedded_pt(),
                crate::mediainfo::SubtitleFacts {
                    index: 1,
                    forced: true,
                    ..embedded_pt()
                },
            ];
        });
        // 2 a extrair + 3 em arquivo (não PT, para a faixa embutida ser necessária).
        let folder = parent(&facts.files[0].path).to_owned();
        for n in 0..3 {
            facts
                .files
                .push(file(&format!("{folder}/Filme.{n}.eng.srt"), 100));
        }
        let report = check(Site::Bj, &facts);
        assert_eq!(report.adaptations.len(), 2);
        assert_eq!(report.status_of("4.2.1.10"), Some(Status::Fail));
        facts.files.pop();
        assert_eq!(status(Site::Bj, &facts, "4.2.1.10"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_2_1_10_filme_com_dois_videos_ou_muitas_legendas() {
        let mut facts = movie();
        assert_eq!(status(Site::Bj, &facts, "4.2.1.10"), Some(Status::Ok));
        facts.files.push(file("Filme/Extra.mkv", GB));
        assert_eq!(status(Site::Bj, &facts, "4.2.1.10"), Some(Status::Fail));
        let mut facts = movie();
        for n in 0..5 {
            facts
                .files
                .push(file(&format!("Filme/Filme.{n}.pt.srt"), 100));
        }
        assert_eq!(status(Site::Bj, &facts, "4.2.1.10"), Some(Status::Fail));
    }

    #[test]
    fn bj_4_2_1_10_legendas_embutidas_nao_contam_no_limite() {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.subtitles = (0..25)
                .map(|_| crate::mediainfo::SubtitleFacts {
                    index: 0,
                    kind: crate::mediainfo::SubtitleKind::Text,
                    language: Language::Portuguese,
                    raw_language: None,
                    codec: "subrip".to_owned(),
                    forced: false,
                    title: None,
                })
                .collect();
        });
        assert_eq!(status(Site::Bj, &facts, "4.2.1.10"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_1_2_referencia_a_site() {
        let mut facts = movie();
        assert_eq!(status(Site::Bj, &facts, "4.1.2 / 4.1.3"), Some(Status::Ok));
        rename(&mut facts, "Filme.2024.1080p.WEB-DL.bjshare");
        assert_eq!(
            status(Site::Bj, &facts, "4.1.2 / 4.1.3"),
            Some(Status::Fail)
        );
    }

    #[test]
    fn bj_4_2_1_13_temporada_completa_e_incompleta() {
        let mut facts = season(3);
        facts.season_episodes = Some(aired(3));
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
        facts.season_episodes = Some(aired(8));
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Fail));
    }

    #[test]
    fn bj_4_2_1_13_episodio_repetido_nao_compensa_o_ausente() {
        let mut facts = season(3);
        facts.season_episodes = Some(aired(3));
        // Três vídeos, mas o terceiro é cópia do primeiro: falta o E03.
        rename_video(&mut facts, 2, "Serie.S01E01.1080p.WEB-DL.copia.mkv");
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.13"), Some(Status::Fail));
        assert!(report.checks.iter().any(|c| c.message.contains("S01E03")));
    }

    #[test]
    fn temporada_ignora_amostra_e_extras() {
        let mut facts = season(3);
        facts.season_episodes = Some(aired(3));
        let folder = parent(&facts.files[0].path).to_owned();
        // Amostra pelo nome e pelo tamanho; extra em pasta própria; especial.
        facts.files.push(file(
            &format!("{folder}/Sample/Serie.S01E09.sample.mkv"),
            10,
        ));
        facts
            .files
            .push(file(&format!("{folder}/Serie.S01E10.1080p.mkv"), 1000));
        facts
            .files
            .push(file(&format!("{folder}/Extras/Making.Of.mkv"), GB));
        facts
            .files
            .push(file(&format!("{folder}/Serie.S00E01.Especial.mkv"), GB));
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
        // Já a amostra no lugar de um episódio não o cobre.
        facts.files[2].size = 1000;
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Fail));
    }

    #[test]
    fn temporada_com_episodio_duplo_cobre_a_faixa() {
        let mut facts = season(3);
        facts.season_episodes = Some(aired(4));
        rename_video(&mut facts, 2, "Serie.S01E03-E04.1080p.WEB-DL.mkv");
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
    }

    #[test]
    fn temporada_com_nome_que_nao_parseia_falha_dizendo_o_arquivo() {
        let mut facts = season(3);
        facts.season_episodes = Some(aired(3));
        rename_video(&mut facts, 1, "Qualquer coisa.mkv");
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.13"), Some(Status::Fail));
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.message.contains("Qualquer coisa.mkv"))
        );
    }

    #[test]
    fn bj_4_2_1_13_temporada_ainda_em_exibicao_nao_e_completa() {
        let mut facts = season(5);
        // 5 de 10 foram ao ar e o pacote cobre os 5: ainda assim é parcial.
        facts.season_episodes = Some(aired(5));
        facts.season_pending = 5;
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Fail));
        facts.season_pending = 0;
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
    }

    #[test]
    fn bj_4_2_1_13_pacote_com_mais_de_uma_temporada() {
        let mut facts = season(2);
        facts.season_episodes = Some(aired(2));
        facts
            .files
            .push(file("Serie/Serie.S02E01.1080p.WEB-DL.mkv", GB));
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Fail));
    }

    #[test]
    fn bj_4_1_20_destino_com_dual_barra_quem_nao_e_dual() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme.2024.720p.WEB-DL.DUAL.x264-OUTRO", 3 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.1.20"), Some(Status::Ok));
        set_video(&mut facts, |m| m.audio = vec![eng()]);
        facts.files.push(file(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-GRP/Filme.pt.srt",
            100,
        ));
        assert_eq!(status(Site::Bj, &facts, "4.1.20"), Some(Status::Fail));
    }

    #[test]
    fn bj_4_2_1_2_duplicado_e_aviso_de_similar() {
        let mut facts = movie();
        facts.destination = vec![dest(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO",
            10 * GB + GB / 33,
        )];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.2"), Some(Status::Fail));
        // 20% a menos: parecido o bastante para o usuário julgar.
        facts.destination = vec![dest(
            "Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO",
            8 * GB + GB / 3,
        )];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.2"), Some(Status::Warn));
        // 3 vezes maior: outro nível de qualidade, nada a dizer.
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x264-OUTRO", 30 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.2"), Some(Status::Ok));
    }

    #[test]
    fn bj_codec_diferente_no_duplicado_vira_aviso() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUAL.x265-OUTRO", 10 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.2"), Some(Status::Warn));
    }

    #[test]
    fn bj_dual_x_dublado_no_destino_so_cai_na_4_1_20() {
        let mut facts = movie();
        facts.destination = vec![dest("Filme.2024.1080p.WEB-DL.DUBLADO.x264-OUTRO", 10 * GB)];
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.2"), Some(Status::Ok));
        assert_eq!(report.status_of("4.1.20"), None);
    }

    /// Filme brasileiro com uma só faixa de áudio sem tag de idioma (`und`).
    fn nacional_sem_tag() -> Facts {
        let mut facts = movie();
        facts.original_language = Some(Language::Portuguese);
        set_video(&mut facts, |m| m.audio = vec![audio(Language::Unknown, "")]);
        facts
    }

    #[test]
    fn bj_4_2_1_5_obra_nacional_com_audio_sem_tag_nao_precisa_de_legenda() {
        let facts = nacional_sem_tag();
        assert_eq!(
            status(Site::Bj, &facts, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Ok)
        );
        // Estrangeira com o mesmo áudio sem tag continua exigindo a legenda.
        let mut estrangeira = nacional_sem_tag();
        estrangeira.original_language = Some(Language::English);
        assert_eq!(
            status(Site::Bj, &estrangeira, "4.2.1.5 / 4.2.1.4"),
            Some(Status::Fail)
        );
    }

    #[test]
    fn bj_4_1_20_obra_nacional_com_audio_sem_tag_nao_e_legendada() {
        let mut facts = nacional_sem_tag();
        facts.destination = vec![dest("Filme.2024.720p.WEB-DL.DUAL.x264-OUTRO", 3 * GB)];
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.1.20"), Some(Status::Ok));
        assert!(report.verdict == Verdict::Elegivel, "{:?}", report.checks);
    }

    #[test]
    fn bj_4_1_20_mensagem_de_obra_nacional_nao_diz_que_e_dual() {
        let mut facts = movie();
        facts.original_language = Some(Language::Portuguese);
        facts.destination = vec![dest("Filme.2024.720p.WEB-DL.DUAL.x264-OUTRO", 3 * GB)];
        let report = check(Site::Bj, &facts);
        let check = report.checks.iter().find(|c| c.rule == "4.1.20").unwrap();
        assert_eq!(check.status, Status::Ok);
        assert!(check.message.contains("obra nacional"), "{}", check.message);
        assert!(!check.message.contains("também é Dual"));
    }

    /// Episódio 1080p WEB-DL Dual, o mesmo formato do `movie()`.
    fn episode() -> Facts {
        let mut facts = movie();
        facts.kind = Kind::Episode;
        facts.release_name = "Serie.S01E03.1080p.WEB-DL.DUAL.x264-GRP".to_owned();
        facts
    }

    fn dest_ep(title: &str, size: u64) -> DestinationRelease {
        DestinationRelease::new(title, size, Kind::Episode)
    }

    #[test]
    fn bj_4_2_1_13_episodio_avulso_com_a_temporada_no_destino_avisa() {
        let mut facts = episode();
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
        facts.destination = vec![dest_ep("Serie.S01.1080p.WEB-DL.DUAL.x264-OUTRO", 40 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Warn));
        // Outra temporada, outra resolução ou outro áudio: não é o mesmo pacote.
        facts.destination = vec![dest_ep("Serie.S02.1080p.WEB-DL.DUAL.x264-OUTRO", 40 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
        facts.destination = vec![dest_ep("Serie.S01.720p.WEB-DL.DUAL.x264-OUTRO", 20 * GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
        facts.destination = vec![dest_ep(
            "Serie.S01.1080p.WEB-DL.DUBLADO.x264-OUTRO",
            40 * GB,
        )];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
        // Outro episódio avulso no destino não conta como pacote.
        facts.destination = vec![dest_ep("Serie.S01E04.1080p.WEB-DL.DUAL.x264-OUTRO", GB)];
        assert_eq!(status(Site::Bj, &facts, "4.2.1.13"), Some(Status::Ok));
    }

    #[test]
    fn release_do_destino_marca_o_pacote_de_uma_temporada() {
        let pack = dest_ep("Serie.S01.1080p.WEB-DL.DUAL.x264-GRP", GB);
        assert_eq!(pack.season_pack, Some(1));
        assert_eq!(
            dest_ep("Serie.S01E01.1080p.WEB-DL.x264-GRP", GB).season_pack,
            None
        );
        assert_eq!(
            dest_ep("Serie.S01-S03.1080p.WEB-DL.x264-GRP", GB).season_pack,
            None
        );
        assert_eq!(
            dest("Filme.2024.1080p.WEB-DL.x264-GRP", GB).season_pack,
            None
        );
        // Relatório gravado antes do campo existir ainda lê.
        let mut json = serde_json::to_value(&pack).unwrap();
        json.as_object_mut().unwrap().remove("season_pack");
        let old: DestinationRelease = serde_json::from_value(json).unwrap();
        assert_eq!(old.season_pack, None);
    }

    #[test]
    fn bj_4_1_7_compactado() {
        let mut facts = movie();
        assert_eq!(status(Site::Bj, &facts, "4.1.7"), Some(Status::Ok));
        facts.files.push(file("Filme/Filme.zip", GB));
        assert_eq!(status(Site::Bj, &facts, "4.1.7"), Some(Status::Fail));
    }

    #[test]
    fn varios_videos_citam_o_arquivo_que_falhou() {
        let mut facts = season(2);
        facts.videos[1].media.audio = vec![eng(), audio(Language::Spanish, "spa")];
        let report = check(Site::Bj, &facts);
        assert_eq!(report.status_of("4.2.1.14.1"), Some(Status::Fail));
        assert!(report.checks.iter().any(|c| {
            c.message
                .starts_with("Serie.S01E02.1080p.WEB-DL.mkv: áudio em")
        }));
    }

    /// Filme estrangeiro sem áudio PT, com um `.srt` sem sufixo de idioma ao
    /// lado do vídeo, que o preparo leu como `sniff`.
    fn srt_sem_sufixo(language: SniffLanguage, cues: usize) -> Facts {
        let mut facts = movie();
        set_video(&mut facts, |m| {
            m.audio = vec![eng()];
            m.subtitles = Vec::new();
        });
        let srt = format!(
            "{}.srt",
            &facts.videos[0].path[..facts.videos[0].path.len() - 4]
        );
        facts.files.push(file(&srt, 90_000));
        facts.sniffs.insert(
            srt,
            Sniff {
                language,
                cues,
                covered_secs: f64::from(u32::try_from(cues).unwrap()) * 2.0,
                video_secs: Some(6180.0),
            },
        );
        facts
    }

    fn message(site: Site, facts: &Facts, rule: &str) -> String {
        check(site, facts)
            .checks
            .iter()
            .find(|c| c.rule == rule)
            .map(|c| c.message.clone())
            .unwrap_or_default()
    }

    const BJ_SUB: &str = "4.2.1.5 / 4.2.1.4";

    #[test]
    fn srt_sem_sufixo_pt_br_completo_vale_nos_dois_sites() {
        let facts = srt_sem_sufixo(SniffLanguage::PtBr, 995);
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Ok));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Ok));
        assert!(
            message(Site::Amigos, &facts, "3.3")
                .contains("o .srt ao lado é PT-BR completo (995 falas)")
        );
        assert!(
            message(Site::Bj, &facts, BJ_SUB)
                .contains("o .srt ao lado é PT-BR completo (995 falas)")
        );
    }

    #[test]
    fn srt_sem_sufixo_pt_sem_variante_confirmada_e_aviso_que_conta_como_presente() {
        let facts = srt_sem_sufixo(SniffLanguage::Pt, 995);
        let esperado =
            "o .srt ao lado parece português completo, mas a variante não foi confirmada";
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Warn));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Warn));
        let bj = message(Site::Bj, &facts, BJ_SUB);
        assert!(
            bj.contains(esperado) && bj.contains("o BJ exige PT-BR, confira"),
            "{bj}"
        );
        assert!(message(Site::Amigos, &facts, "3.3").contains(esperado));
    }

    #[test]
    fn srt_sem_sufixo_inconclusivo_avisa_e_conta_so_em_pt_ou_pt_br() {
        // 300 falas / 600 s em 6180 s: 10% de cobertura, nem completa nem forçada.
        for language in [SniffLanguage::PtBr, SniffLanguage::Pt] {
            let facts = srt_sem_sufixo(language, 300);
            for (site, rule) in [(Site::Amigos, "3.3"), (Site::Bj, BJ_SUB)] {
                assert_eq!(status(site, &facts, rule), Some(Status::Warn));
                let text = message(site, &facts, rule);
                assert!(
                    text.contains(
                        "não deu para confirmar se o .srt ao lado é completo ou forçado (300 falas, 10% do filme): confira"
                    ),
                    "{text}"
                );
            }
        }
        // pt-PT inconclusivo não conta, nos dois sites.
        let facts = srt_sem_sufixo(SniffLanguage::PtPt, 300);
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
    }

    #[test]
    fn srt_sem_sufixo_forcado_nunca_conta_em_pt_br() {
        let facts = srt_sem_sufixo(SniffLanguage::PtBr, 160);
        // 160 falas de 2 s em 6180 s: 5% de cobertura.
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
        assert!(message(Site::Bj, &facts, BJ_SUB).contains("é forçado (160 falas em 1 h 43 min)"));
    }

    #[test]
    fn srt_nao_confirmado_com_faixa_embutida_avisa_que_extrai() {
        let mut facts = srt_sem_sufixo(SniffLanguage::Pt, 995);
        set_video(&mut facts, |m| {
            m.subtitles = vec![embedded_pt()];
        });
        let bj = message(Site::Bj, &facts, BJ_SUB);
        assert!(bj.contains("a legenda PT embutida será extraída"), "{bj}");
        assert_eq!(check(Site::Bj, &facts).adaptations.len(), 1);
        // Com o PT-BR confirmado, o .srt basta e não há o que extrair.
        let mut facts = srt_sem_sufixo(SniffLanguage::PtBr, 995);
        set_video(&mut facts, |m| {
            m.subtitles = vec![embedded_pt()];
        });
        assert!(check(Site::Bj, &facts).adaptations.is_empty());
    }

    #[test]
    fn srt_sem_sufixo_forcado_pt_pt_reprova_com_a_explicacao() {
        let facts = srt_sem_sufixo(SniffLanguage::PtPt, 28);
        let esperado = "o .srt ao lado é forçado (28 falas em 1 h 43 min) e em pt-PT";
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
        assert!(message(Site::Amigos, &facts, "3.3").contains(esperado));
        assert!(message(Site::Bj, &facts, BJ_SUB).contains(esperado));
    }

    #[test]
    fn srt_sem_sufixo_pt_pt_completo_e_aviso_no_amigos_e_falha_no_bj() {
        let facts = srt_sem_sufixo(SniffLanguage::PtPt, 900);
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Warn));
        let amigos = message(Site::Amigos, &facts, "3.3");
        assert!(
            amigos.contains("pt-PT") && amigos.contains("pede português brasileiro"),
            "{amigos}"
        );
        assert!(
            amigos.contains("o .srt ao lado é pt-PT completo (900 falas)"),
            "{amigos}"
        );
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
        assert!(
            message(Site::Bj, &facts, BJ_SUB)
                .contains("pt-PT completo (900 falas): o BJ exige PT-BR")
        );
    }

    #[test]
    fn srt_sem_sufixo_em_outro_idioma_ou_sem_leitura_nao_vale() {
        let facts = srt_sem_sufixo(SniffLanguage::English, 900);
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
        assert!(message(Site::Bj, &facts, BJ_SUB).contains("é em inglês (900 falas)"));
        // Sem leitura (arquivo grande ou ilegível), nada a dizer sobre o conteúdo.
        let mut facts = srt_sem_sufixo(SniffLanguage::PtBr, 900);
        facts.sniffs.clear();
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Fail));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
    }

    #[test]
    fn srt_sem_sufixo_em_outra_pasta_nao_vale_no_bj() {
        let mut facts = srt_sem_sufixo(SniffLanguage::PtBr, 995);
        let from = facts.files.pop().unwrap();
        let sniff = facts.sniffs.remove(&from.path).unwrap();
        let moved = format!("Outra/{}", file_name(&from.path));
        facts.files.push(file(&moved, from.size));
        facts.sniffs.insert(moved, sniff);
        assert_eq!(status(Site::Amigos, &facts, "3.3"), Some(Status::Ok));
        assert_eq!(status(Site::Bj, &facts, BJ_SUB), Some(Status::Fail));
    }

    #[test]
    fn so_o_srt_de_mesmo_nome_do_video_e_lido() {
        let mut facts = srt_sem_sufixo(SniffLanguage::PtBr, 10);
        let folder = parent(&facts.videos[0].path).to_owned();
        facts
            .files
            .push(file(&format!("{folder}/Outro.nome.srt"), 10));
        facts.files.push(file(
            &format!("{folder}/Filme.2024.1080p.WEB-DL.DUAL.x264-GRP.por.srt"),
            10,
        ));
        let targets = sniff_targets(&facts.files, &facts.videos);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0, facts.files[1].path);
    }
}

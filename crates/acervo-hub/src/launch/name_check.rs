//! Confere o que o NOME do arquivo diz (codec e canais do áudio, codec e
//! resolução do vídeo, HDR) contra o que o `ffprobe` leu: o Amigos (4.0.1) e o
//! BJ (4.1.1) validam o nome contra o `MediaInfo` e proíbem renomear, então o
//! pacote que diverge é inelegível antes do envio.
//!
//! [`NameClaims::parse`] só lê o nome (puro); [`findings`] cruza com o arquivo.

use crate::mediainfo::{AudioFacts, Hdr, MediaFacts, VideoFacts};

use super::rules::{Status, hdr_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DtsKind {
    Plain,
    Hd,
    Ma,
    X,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioClaim {
    Ac3,
    Eac3,
    Aac,
    Dts(DtsKind),
    TrueHd,
    Flac,
    Opus,
}

impl AudioClaim {
    /// Como a mensagem cita o que o nome diz.
    const fn label(self) -> &'static str {
        match self {
            Self::Ac3 => "DD (AC-3)",
            Self::Eac3 => "DDP (E-AC-3)",
            Self::Aac => "AAC",
            Self::Dts(DtsKind::Plain) => "DTS",
            Self::Dts(DtsKind::Hd) => "DTS-HD",
            Self::Dts(DtsKind::Ma) => "DTS-HD MA",
            Self::Dts(DtsKind::X) => "DTS-X",
            Self::TrueHd => "TrueHD",
            Self::Flac => "FLAC",
            Self::Opus => "Opus",
        }
    }

    /// `codec_name` do ffprobe que satisfaz a afirmação.
    const fn codec(self) -> &'static str {
        match self {
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Aac => "aac",
            Self::Dts(_) => "dts",
            Self::TrueHd => "truehd",
            Self::Flac => "flac",
            Self::Opus => "opus",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoClaim {
    H264,
    Hevc,
    Av1,
}

impl VideoClaim {
    const fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "H.265",
            Self::Av1 => "AV1",
        }
    }

    const fn codec(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HdrClaim {
    /// `HDR`: qualquer faixa dinâmica alta.
    Any,
    /// `HDR10`: HDR10 (ou Dolby Vision com camada HDR10).
    Hdr10,
    /// `DV`/`DoVi`.
    DolbyVision,
}

impl HdrClaim {
    const fn label(self) -> &'static str {
        match self {
            Self::Any => "HDR",
            Self::Hdr10 => "HDR10",
            Self::DolbyVision => "DV (Dolby Vision)",
        }
    }

    const fn accepts(self, hdr: Hdr) -> bool {
        match self {
            Self::Any => !matches!(hdr, Hdr::Sdr),
            Self::Hdr10 => matches!(hdr, Hdr::Hdr10 | Hdr::DolbyVision),
            Self::DolbyVision => matches!(hdr, Hdr::DolbyVision),
        }
    }
}

/// O que o nome do arquivo afirma sobre o conteúdo.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NameClaims {
    audio: Vec<AudioClaim>,
    /// Quantidade de canais e como o nome escreveu (`6`, `5.1`).
    channels: Vec<(u32, String)>,
    atmos: bool,
    video: Option<VideoClaim>,
    resolution: Option<u16>,
    hdr: Vec<HdrClaim>,
}

/// Separador é tudo que não é letra, dígito ou `+` (`DD+` é um token só).
fn segments(name: &str) -> Vec<String> {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '+')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn single_digit(segment: Option<&String>) -> Option<u32> {
    let mut chars = segment?.chars();
    let digit = chars.next()?.to_digit(10)?;
    chars.next().is_none().then_some(digit)
}

/// `5.1` → 6, `7.1` → 8, `2.0` → 2; só layouts que existem.
fn layout(front: u32, lfe: u32) -> Option<(u32, String)> {
    (matches!(front, 1 | 2 | 5 | 6 | 7) && lfe <= 1)
        .then(|| (front + lfe, format!("{front}.{lfe}")))
}

/// Canais depois de uma palavra de codec: dígito colado nela (`ddp5` + `1`) ou
/// dois segmentos de um dígito (`5` `1`). Devolve quantos segmentos seguintes
/// foram usados.
fn channels_after(glued: &str, segs: &[String], next: usize) -> Option<((u32, String), usize)> {
    if glued.is_empty() {
        let front = single_digit(segs.get(next))?;
        let lfe = single_digit(segs.get(next + 1))?;
        return layout(front, lfe).map(|l| (l, 2));
    }
    let front = glued.parse::<u32>().ok().filter(|d| *d < 10)?;
    let lfe = single_digit(segs.get(next))?;
    layout(front, lfe).map(|l| (l, 1))
}

/// Separa `ddp5` em (`ddp`, `5`).
fn split_digits(segment: &str) -> (&str, &str) {
    let at = segment
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(segment.len());
    segment.split_at(at)
}

impl NameClaims {
    /// Lê os tokens de áudio e vídeo de um nome de arquivo (ou de qualquer
    /// texto): separadores são ponto, espaço, hífen, sublinhado, colchete etc.
    #[must_use]
    pub fn parse(name: &str) -> Self {
        let segs = segments(name);
        let mut claims = Self::default();
        let mut i = 0;
        while i < segs.len() {
            i += claims.read(&segs, i);
        }
        claims
    }

    fn push_audio(&mut self, claim: AudioClaim) {
        if !self.audio.contains(&claim) {
            self.audio.push(claim);
        }
    }

    fn push_channels(&mut self, channels: (u32, String)) {
        if !self.channels.iter().any(|c| c.0 == channels.0) {
            self.channels.push(channels);
        }
    }

    fn push_hdr(&mut self, claim: HdrClaim) {
        if !self.hdr.contains(&claim) {
            self.hdr.push(claim);
        }
    }

    /// Lê o token em `i` e devolve quantos segmentos consumiu (no mínimo 1).
    fn read(&mut self, segs: &[String], i: usize) -> usize {
        let seg = segs[i].as_str();
        let next = segs.get(i + 1).map(String::as_str);
        let after = segs.get(i + 2).map(String::as_str);
        // Vídeo, HDR e resolução: tokens inteiros.
        match seg {
            "h264" | "x264" | "avc" => self.video = self.video.or(Some(VideoClaim::H264)),
            "h265" | "x265" | "hevc" => self.video = self.video.or(Some(VideoClaim::Hevc)),
            "av1" => self.video = self.video.or(Some(VideoClaim::Av1)),
            "h" if next == Some("264") => {
                self.video = self.video.or(Some(VideoClaim::H264));
                return 2;
            }
            "h" if next == Some("265") => {
                self.video = self.video.or(Some(VideoClaim::Hevc));
                return 2;
            }
            "720p" | "1080p" | "2160p" => {
                let height = seg.trim_end_matches('p').parse().ok();
                self.resolution = self.resolution.or(height);
            }
            "hdr" => self.push_hdr(HdrClaim::Any),
            "hdr10" | "hdr10+" | "hdr10plus" => self.push_hdr(HdrClaim::Hdr10),
            "dv" | "dovi" => self.push_hdr(HdrClaim::DolbyVision),
            "dolby" if next == Some("vision") => {
                self.push_hdr(HdrClaim::DolbyVision);
                return 2;
            }
            "atmos" => self.atmos = true,
            "ac3" => return self.audio_with_channels(AudioClaim::Ac3, segs, i, 1),
            "eac3" => return self.audio_with_channels(AudioClaim::Eac3, segs, i, 1),
            "e" if next == Some("ac") && after == Some("3") => {
                return self.audio_with_channels(AudioClaim::Eac3, segs, i, 3);
            }
            "e" if next == Some("ac3") => {
                return self.audio_with_channels(AudioClaim::Eac3, segs, i, 2);
            }
            "ac" if next == Some("3") => {
                return self.audio_with_channels(AudioClaim::Ac3, segs, i, 2);
            }
            _ => return self.read_audio_word(segs, i),
        }
        1
    }

    /// Palavras de áudio, com canais colados (`ddp5.1`) ou soltos (`dd.5.1`).
    fn read_audio_word(&mut self, segs: &[String], i: usize) -> usize {
        let (word, glued) = split_digits(&segs[i]);
        let next = segs.get(i + 1).map(String::as_str);
        let mut used = 1;
        let claim = match word {
            "dd" => AudioClaim::Ac3,
            "ddp" | "dd+" | "ddp+" => AudioClaim::Eac3,
            "ddpa" | "dd+a" => {
                self.atmos = true;
                AudioClaim::Eac3
            }
            "aac" => AudioClaim::Aac,
            "truehd" => AudioClaim::TrueHd,
            "true" if next == Some("hd") => {
                used = 2;
                AudioClaim::TrueHd
            }
            "flac" => AudioClaim::Flac,
            "opus" => AudioClaim::Opus,
            "dtsx" => AudioClaim::Dts(DtsKind::X),
            "dtshd" => AudioClaim::Dts(DtsKind::Hd),
            "dts" => match next {
                Some("hd") if glued.is_empty() => {
                    used = 2;
                    if segs.get(i + 2).map(String::as_str) == Some("ma") {
                        used = 3;
                        AudioClaim::Dts(DtsKind::Ma)
                    } else {
                        AudioClaim::Dts(DtsKind::Hd)
                    }
                }
                Some("x") if glued.is_empty() => {
                    used = 2;
                    AudioClaim::Dts(DtsKind::X)
                }
                _ => AudioClaim::Dts(DtsKind::Plain),
            },
            _ => {
                // Canais soltos (`5.1`) sem palavra de codec.
                if glued.is_empty()
                    && let Some((channels, used)) = channels_after("", segs, i)
                {
                    self.push_channels(channels);
                    return used;
                }
                return 1;
            }
        };
        self.audio_with_channels_glued(claim, glued, segs, i, used)
    }

    fn audio_with_channels(
        &mut self,
        claim: AudioClaim,
        segs: &[String],
        i: usize,
        used: usize,
    ) -> usize {
        self.audio_with_channels_glued(claim, "", segs, i, used)
    }

    /// Registra o codec e, se os canais vêm logo depois, também eles.
    fn audio_with_channels_glued(
        &mut self,
        claim: AudioClaim,
        glued: &str,
        segs: &[String],
        i: usize,
        used: usize,
    ) -> usize {
        self.push_audio(claim);
        match channels_after(glued, segs, i + used) {
            Some((channels, more)) => {
                self.push_channels(channels);
                used + more
            }
            None => used,
        }
    }
}

/// Nome legível do `codec_name` do ffprobe.
fn codec_label(codec: &str) -> String {
    match codec {
        "ac3" => "AC-3 (Dolby Digital)".to_owned(),
        "eac3" => "E-AC-3 (Dolby Digital Plus)".to_owned(),
        "aac" => "AAC".to_owned(),
        "dts" => "DTS".to_owned(),
        "truehd" => "TrueHD".to_owned(),
        "flac" => "FLAC".to_owned(),
        "opus" => "Opus".to_owned(),
        "h264" => "H.264 (AVC)".to_owned(),
        "hevc" => "H.265 (HEVC)".to_owned(),
        "av1" => "AV1".to_owned(),
        "" => "desconhecido".to_owned(),
        other => other.to_uppercase(),
    }
}

fn join_pt(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => format!("{} e {last}", head.join(", ")),
    }
}

fn distinct(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn profile_matches(kind: DtsKind, profile: &str) -> bool {
    let profile = profile.to_lowercase();
    match kind {
        DtsKind::Plain => true,
        DtsKind::Hd => profile.contains("hd") || profile.contains("ma"),
        DtsKind::Ma => profile.contains("ma"),
        DtsKind::X => {
            profile.contains("dts:x") || profile.contains("dts-x") || profile.ends_with(" x")
        }
    }
}

fn audio_finding(claim: AudioClaim, audio: &[AudioFacts]) -> Option<String> {
    if audio.is_empty() {
        return Some(format!(
            "o nome diz {}, mas o arquivo não tem faixa de áudio",
            claim.label()
        ));
    }
    let same_codec: Vec<&AudioFacts> = audio.iter().filter(|a| a.codec == claim.codec()).collect();
    if same_codec.is_empty() {
        let actual = distinct(audio.iter().map(|a| codec_label(&a.codec)));
        return Some(format!(
            "o nome diz {}, mas o áudio é {}",
            claim.label(),
            join_pt(&actual)
        ));
    }
    if let AudioClaim::Dts(kind) = claim {
        // Sem `profile` no ffprobe basta o codec dts.
        let profiles: Vec<&str> = same_codec
            .iter()
            .filter_map(|a| a.profile.as_deref())
            .collect();
        if !profiles.is_empty() && !profiles.iter().any(|p| profile_matches(kind, p)) {
            return Some(format!(
                "o nome diz {}, mas a faixa DTS é {}",
                claim.label(),
                join_pt(&distinct(profiles.iter().map(|p| (*p).to_owned())))
            ));
        }
    }
    None
}

fn channels_finding(wanted: &(u32, String), audio: &[AudioFacts]) -> Option<String> {
    let counts: Vec<u32> = audio.iter().filter_map(|a| a.channels).collect();
    // Sem contagem de canais lida não há como contradizer o nome.
    if counts.is_empty() || counts.contains(&wanted.0) {
        return None;
    }
    let actual = distinct(counts.iter().map(|c| format!("{c} canais")));
    Some(format!(
        "o nome diz {} ({} canais), mas as faixas têm {}",
        wanted.1,
        wanted.0,
        join_pt(&actual)
    ))
}

fn resolution_matches(height_claim: u16, video: &VideoFacts) -> bool {
    let (w, h) = (video.width, video.height);
    match height_claim {
        2160 => w >= 3800 || h >= 2160 || (h >= 2000 && w > h),
        1080 => (1800..=2200).contains(&w) || (1000..=1100).contains(&h),
        720 => (1200..=1400).contains(&w) || (680..=740).contains(&h),
        _ => true,
    }
}

fn video_findings(claims: &NameClaims, video: &VideoFacts) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(claim) = claims.video
        && video.codec != claim.codec()
    {
        out.push(format!(
            "o nome diz {}, mas o vídeo é {}",
            claim.label(),
            codec_label(&video.codec)
        ));
    }
    if let Some(height) = claims.resolution
        && !resolution_matches(height, video)
    {
        out.push(format!(
            "o nome diz {height}p, mas o vídeo tem {}x{}",
            video.width, video.height
        ));
    }
    for claim in &claims.hdr {
        if !claim.accepts(video.hdr) {
            out.push(format!(
                "o nome diz {}, mas o vídeo é {}",
                claim.label(),
                hdr_label(video.hdr)
            ));
        }
    }
    out
}

/// As divergências (`Fail`) e dúvidas (`Warn`) entre o nome do arquivo e o que
/// o `ffprobe` leu dele; vazio quando o nome confere ou não afirma nada.
#[must_use]
pub fn findings(file_name: &str, media: &MediaFacts) -> Vec<(Status, String)> {
    let claims = NameClaims::parse(file_name);
    let mut out: Vec<(Status, String)> = Vec::new();
    if let Some(video) = &media.video {
        out.extend(
            video_findings(&claims, video)
                .into_iter()
                .map(|m| (Status::Fail, m)),
        );
    }
    for claim in &claims.audio {
        if let Some(message) = audio_finding(*claim, &media.audio) {
            out.push((Status::Fail, message));
        }
    }
    for wanted in &claims.channels {
        if let Some(message) = channels_finding(wanted, &media.audio) {
            out.push((Status::Fail, message));
        }
    }
    // O ffprobe não expõe o objeto Atmos (JOC) de forma confiável: só o título
    // da faixa o confirma.
    let atmos_in_title = media.audio.iter().any(|a| {
        a.title
            .as_deref()
            .is_some_and(|t| t.to_lowercase().contains("atmos"))
    });
    if claims.atmos && !atmos_in_title {
        out.push((
            Status::Warn,
            "o nome diz Atmos, que o ffprobe não consegue confirmar; confira no MediaInfo"
                .to_owned(),
        ));
    }
    out
}

/// Codecs de áudio na grafia de release padrão, do mais longo ao mais curto
/// (`DTS-HD.MA` antes de `DTS`). Comparados em maiúsculas; canais opcionais
/// (`5.1`, `.5.1`, `2.0`) podem vir colados ou depois de um ponto.
const STANDARD_AUDIO: [&str; 16] = [
    "DTS-HD.MA",
    "DTS-HD",
    "DTS-X",
    "DTS",
    "TRUEHD",
    "DDPA",
    "DDP",
    "DD+",
    "DD",
    "EAC3",
    "AC3",
    "AAC",
    "FLAC",
    "OPUS",
    "LPCM",
    "PCM",
];

/// Codecs de vídeo na grafia exata que o formulário do Amigos aceita. `h264`
/// e `h265` minúsculos entram por serem comuns em WEB-DL.
const STANDARD_VIDEO: [&str; 13] = [
    "H.264", "H.265", "H264", "H265", "h264", "h265", "x264", "x265", "AVC", "HEVC", "AV1", "VC-1",
    "MPEG-2",
];

fn boundary_before(name: &str, at: usize) -> bool {
    name[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
}

/// Posições (byte) onde `token` aparece em `name` com borda à esquerda e sem
/// letra colada à direita (dígito pode: `DDP5.1`, `AAC2.0`).
fn token_at_boundaries(name: &str, token: &str, digit_ok: bool) -> bool {
    name.match_indices(token).any(|(at, _)| {
        boundary_before(name, at)
            && name[at + token.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphabetic() && (digit_ok || !c.is_ascii_digit()))
    })
}

/// O nome tem um codec de áudio na grafia padrão (sem diferenciar maiúsculas).
fn has_standard_audio(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    STANDARD_AUDIO
        .iter()
        .any(|token| token_at_boundaries(&upper, token, true))
}

/// O nome sem a extensão (`.mkv`).
fn without_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if (2..=4).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && !ext.bytes().all(|b| b.is_ascii_digit()) =>
        {
            stem
        }
        _ => name,
    }
}

/// O nome tem um codec de vídeo na grafia exata do padrão, que o site
/// reconhece. Codec colado num `-GRUPO` que ainda é seguido de mais segmentos
/// com ponto é o grupo de um release refeito (`x264-DON.DUAL-outro`,
/// `X265-IAMABLE.DUAL-Lannister`): o site não o lê. Colado no grupo final
/// (`H.264-SJB.mkv`), conta.
fn has_standard_video(name: &str) -> bool {
    let stem = without_extension(name);
    STANDARD_VIDEO.iter().any(|token| {
        stem.match_indices(token).any(|(at, _)| {
            let rest = &stem[at + token.len()..];
            boundary_before(stem, at)
                && match rest.chars().next() {
                    None => true,
                    Some('-') => !rest.contains('.'),
                    Some(c) => !c.is_ascii_alphanumeric(),
                }
        })
    })
}

/// O codec de vídeo existe, mas colado no grupo de um release refeito.
fn glued_video_codec(name: &str) -> Option<&'static str> {
    STANDARD_VIDEO
        .into_iter()
        .find(|token| token_at_boundaries(name, token, false))
}

/// `X264`/`X265` em maiúsculas: o site só aceita o `x` minúsculo.
fn uppercase_x_codec(name: &str) -> Option<&'static str> {
    ["X264", "X265"]
        .into_iter()
        .find(|token| token_at_boundaries(name, token, false))
}

/// O que falta ao nome do arquivo para passar no "padrão de nome" do Amigos
/// (4.0.1): codec de áudio e codec de vídeo na grafia de release padrão.
/// Lê só o nome, sem o `ffprobe`; vazio quando o nome está no padrão.
#[must_use]
pub fn standard_name_gaps(file_name: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    if !has_standard_audio(file_name) {
        gaps.push("falta o codec de áudio (ex.: DDP5.1, AAC2.0, DTS-HD.MA.5.1)".to_owned());
    }
    if !has_standard_video(file_name) {
        gaps.push(match (uppercase_x_codec(file_name), glued_video_codec(file_name)) {
            (Some(token), _) => format!(
                "falta o codec de vídeo na grafia padrão (o nome usa '{token}' em maiúsculas; o padrão é '{}')",
                token.to_lowercase()
            ),
            (None, Some(token)) => format!(
                "falta o codec de vídeo que o site reconhece: '{token}' está colado no grupo de um release refeito (como '{token}-GRUPO.DUAL-OUTRO'), e o site só lê o codec no fim do nome ou antes de um ponto"
            ),
            (None, None) => "falta o codec de vídeo (ex.: H.264, H.265, x265)".to_owned(),
        });
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn track(codec: &str, channels: u32, profile: Option<&str>) -> AudioFacts {
        AudioFacts {
            language: acervo_parser::Language::English,
            raw_language: Some("eng".to_owned()),
            codec: codec.to_owned(),
            channels: Some(channels),
            title: None,
            profile: profile.map(str::to_owned),
        }
    }

    fn media(video: VideoFacts, audio: Vec<AudioFacts>) -> MediaFacts {
        MediaFacts {
            container: "matroska,webm".to_owned(),
            duration_seconds: Some(6000.0),
            video: Some(video),
            audio,
            subtitles: Vec::new(),
        }
    }

    fn worst(name: &str, media: &MediaFacts) -> Option<Status> {
        findings(name, media).into_iter().map(|f| f.0).max()
    }

    fn std_video() -> VideoFacts {
        video("h264", 1920, 1080, Hdr::Sdr)
    }

    #[test]
    fn parser_le_tokens_de_audio() {
        let claims = NameClaims::parse("Filme 2024 1080p WEB-DL DD+ 5 1 x264-GRP.mkv");
        assert_eq!(claims.audio, vec![AudioClaim::Eac3]);
        assert_eq!(claims.channels, vec![(6, "5.1".to_owned())]);
        assert_eq!(claims.video, Some(VideoClaim::H264));
        assert_eq!(claims.resolution, Some(1080));

        for (name, audio, channels) in [
            ("A.DD.5.1.H.264-G", AudioClaim::Ac3, 6),
            ("A.DD5.1.H.264-G", AudioClaim::Ac3, 6),
            ("A.AC3.5.1-G", AudioClaim::Ac3, 6),
            ("A.AC-3.2.0-G", AudioClaim::Ac3, 2),
            ("A.DDP5.1-G", AudioClaim::Eac3, 6),
            ("A.DDP.7.1-G", AudioClaim::Eac3, 8),
            ("A.DD+5.1-G", AudioClaim::Eac3, 6),
            ("A.E-AC-3.5.1-G", AudioClaim::Eac3, 6),
            ("A.EAC3.5.1-G", AudioClaim::Eac3, 6),
            ("A.AAC2.0-G", AudioClaim::Aac, 2),
            ("A.TrueHD.7.1-G", AudioClaim::TrueHd, 8),
            ("A.FLAC.2.0-G", AudioClaim::Flac, 2),
            ("A.Opus.2.0-G", AudioClaim::Opus, 2),
            ("A.DTS-HD.MA.5.1-G", AudioClaim::Dts(DtsKind::Ma), 6),
            ("A.DTS-HD.5.1-G", AudioClaim::Dts(DtsKind::Hd), 6),
            ("A.DTS-X.7.1-G", AudioClaim::Dts(DtsKind::X), 8),
            ("A.DTS5.1-G", AudioClaim::Dts(DtsKind::Plain), 6),
            ("a dd 5 1 h 264", AudioClaim::Ac3, 6),
        ] {
            let claims = NameClaims::parse(name);
            assert_eq!(claims.audio, vec![audio], "{name}");
            assert_eq!(
                claims.channels.first().map(|c| c.0),
                Some(channels),
                "{name}"
            );
        }
    }

    #[test]
    fn parser_le_video_e_hdr() {
        assert_eq!(NameClaims::parse("A.H.265-G").video, Some(VideoClaim::Hevc));
        assert_eq!(NameClaims::parse("A.HEVC-G").video, Some(VideoClaim::Hevc));
        assert_eq!(NameClaims::parse("A.x265-G").video, Some(VideoClaim::Hevc));
        assert_eq!(NameClaims::parse("A.AVC-G").video, Some(VideoClaim::H264));
        assert_eq!(NameClaims::parse("A.AV1-G").video, Some(VideoClaim::Av1));
        assert_eq!(
            NameClaims::parse("A.2160p.HDR10.DV.x265-G").hdr,
            vec![HdrClaim::Hdr10, HdrClaim::DolbyVision]
        );
        assert_eq!(
            NameClaims::parse("A.DoVi-G").hdr,
            vec![HdrClaim::DolbyVision]
        );
        assert_eq!(NameClaims::parse("A.HDR-G").hdr, vec![HdrClaim::Any]);
    }

    #[test]
    fn parser_ignora_o_resto() {
        // Ano, grupo e título não viram afirmação.
        assert_eq!(
            NameClaims::parse("Man.Of.War.2026.WEB-DL-playWEB.mkv"),
            NameClaims::default()
        );
        assert_eq!(NameClaims::parse(""), NameClaims::default());
        let atmos = NameClaims::parse("A.TrueHD.7.1.Atmos-G");
        assert!(atmos.atmos);
        assert!(NameClaims::parse("A.DDPA5.1-G").atmos);
    }

    #[test]
    fn man_of_war_dd_com_eac3_falha() {
        let m = media(std_video(), vec![track("eac3", 6, None)]);
        let name = "Man.Of.War.2026.1080p.AMZN.WEB-DL.DD.5.1.H.264-playWEB.mkv";
        let found = findings(name, &m);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, Status::Fail);
        assert!(
            found[0]
                .1
                .contains("o nome diz DD (AC-3), mas o áudio é E-AC-3 (Dolby Digital Plus)"),
            "{}",
            found[0].1
        );
    }

    #[test]
    fn audio_que_confere_passa() {
        let eac3 = media(std_video(), vec![track("eac3", 6, None)]);
        assert_eq!(worst("A.1080p.DDP5.1.H.264-G.mkv", &eac3), None);
        let ac3 = media(std_video(), vec![track("ac3", 6, None)]);
        assert_eq!(worst("A.1080p.DD5.1.H.264-G.mkv", &ac3), None);
        let aac = media(std_video(), vec![track("aac", 2, None)]);
        assert_eq!(worst("A.1080p.AAC2.0.H.264-G.mkv", &aac), None);
        // Qualquer faixa serve.
        let dual = media(
            std_video(),
            vec![track("eac3", 6, None), track("ac3", 2, None)],
        );
        assert_eq!(worst("A.1080p.DD.2.0.x264-G.mkv", &dual), None);
    }

    #[test]
    fn canais_divergentes_falham() {
        let m = media(std_video(), vec![track("aac", 2, None)]);
        let found = findings("A.1080p.AAC5.1.x264-G.mkv", &m);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Status::Fail);
        assert!(found[0].1.contains("5.1 (6 canais)"), "{}", found[0].1);
        assert!(found[0].1.contains("2 canais"));
    }

    #[test]
    fn dts_confere_perfil_quando_exposto() {
        let hd = media(std_video(), vec![track("dts", 6, Some("DTS-HD MA"))]);
        assert_eq!(worst("A.DTS-HD.MA.5.1.x264-G.mkv", &hd), None);
        assert_eq!(worst("A.DTS.5.1.x264-G.mkv", &hd), None);
        let core = media(std_video(), vec![track("dts", 6, Some("DTS"))]);
        assert_eq!(
            worst("A.DTS-HD.MA.5.1.x264-G.mkv", &core),
            Some(Status::Fail)
        );
        assert_eq!(worst("A.DTS-X.5.1.x264-G.mkv", &core), Some(Status::Fail));
        let sem_perfil = media(std_video(), vec![track("dts", 6, None)]);
        assert_eq!(worst("A.DTS-HD.MA.5.1.x264-G.mkv", &sem_perfil), None);
        assert_eq!(
            worst(
                "A.DTS-HD.MA.5.1.x264-G.mkv",
                &media(std_video(), vec![track("ac3", 6, None)])
            ),
            Some(Status::Fail)
        );
    }

    #[test]
    fn atmos_no_maximo_aviso() {
        let m = media(std_video(), vec![track("eac3", 6, None)]);
        assert_eq!(
            worst("A.1080p.DDP5.1.Atmos.x264-G.mkv", &m),
            Some(Status::Warn)
        );
        let truehd = media(std_video(), vec![track("truehd", 8, None)]);
        assert_eq!(
            worst("A.TrueHD.7.1.Atmos.x264-G.mkv", &truehd),
            Some(Status::Warn)
        );
        let mut titled = track("eac3", 6, None);
        titled.title = Some("English Dolby Atmos".to_owned());
        assert_eq!(
            worst(
                "A.DDP5.1.Atmos.x264-G.mkv",
                &media(std_video(), vec![titled])
            ),
            None
        );
    }

    #[test]
    fn codec_de_video() {
        let hevc = media(video("hevc", 1920, 1080, Hdr::Sdr), vec![]);
        let found = findings("A.1080p.WEB-DL.H.264-G.mkv", &hevc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Status::Fail);
        assert!(
            found[0]
                .1
                .contains("o nome diz H.264, mas o vídeo é H.265 (HEVC)")
        );
        assert_eq!(worst("A.1080p.WEB-DL.x265-G.mkv", &hevc), None);
        assert_eq!(worst("A.1080p.WEB-DL.HEVC-G.mkv", &hevc), None);
    }

    #[test]
    fn resolucao_aceita_cortes() {
        let scope4k = media(video("hevc", 3840, 1600, Hdr::Sdr), vec![]);
        assert_eq!(worst("A.2160p.WEB-DL.x265-G.mkv", &scope4k), None);
        let scope = media(video("h264", 1920, 800, Hdr::Sdr), vec![]);
        assert_eq!(worst("A.1080p.WEB-DL.x264-G.mkv", &scope), None);
        let hd = media(video("h264", 1280, 720, Hdr::Sdr), vec![]);
        assert_eq!(worst("A.720p.WEB-DL.x264-G.mkv", &hd), None);
        assert_eq!(worst("A.1080p.WEB-DL.x264-G.mkv", &hd), Some(Status::Fail));
        assert_eq!(
            worst("A.2160p.WEB-DL.x264-G.mkv", &scope),
            Some(Status::Fail)
        );
        // 2160p com altura de corte largo, largura abaixo de 3800.
        let cut = media(video("hevc", 3600, 2000, Hdr::Sdr), vec![]);
        assert_eq!(worst("A.2160p.x265-G.mkv", &cut), None);
    }

    #[test]
    fn hdr_do_nome_contra_o_arquivo() {
        let sdr = media(video("hevc", 3840, 2160, Hdr::Sdr), vec![]);
        let found = findings("A.2160p.DV.x265-G.mkv", &sdr);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Status::Fail);
        assert!(found[0].1.contains("Dolby Vision"));
        assert!(found[0].1.contains("SDR"));
        assert_eq!(worst("A.2160p.HDR.x265-G.mkv", &sdr), Some(Status::Fail));
        let hdr10 = media(video("hevc", 3840, 2160, Hdr::Hdr10), vec![]);
        assert_eq!(worst("A.2160p.HDR10.x265-G.mkv", &hdr10), None);
        assert_eq!(worst("A.2160p.DV.x265-G.mkv", &hdr10), Some(Status::Fail));
        let dv = media(video("hevc", 3840, 2160, Hdr::DolbyVision), vec![]);
        assert_eq!(worst("A.2160p.HDR.DV.x265-G.mkv", &dv), None);
        assert_eq!(worst("A.2160p.HDR10.x265-G.mkv", &dv), None);
        // Sem token HDR, arquivo HDR passa.
        assert_eq!(worst("A.2160p.x265-G.mkv", &hdr10), None);
    }

    #[test]
    fn nome_sem_tokens_passa() {
        let m = media(
            video("av1", 640, 360, Hdr::Hdr10),
            vec![track("opus", 2, None)],
        );
        assert!(findings("Filme.mkv", &m).is_empty());
        assert!(findings("Filme.2024.WEB-DL-GRUPO.mkv", &m).is_empty());
    }

    #[test]
    fn padrao_de_nome_do_amigos() {
        let gaps = |n: &str| standard_name_gaps(n);
        for ok in [
            "Man.Of.War.2026.1080p.AMZN.WEB-DL.DD.5.1.H.264-playWEB.mkv",
            "2DIE4.24.Horas.no.Limite.2026.2160p.iT.WEB-DL.DDP.5.1.Atmos.DV.HDR.H.265-GHK.mkv",
            "Escalada.Mortal.2022.1080p.AMZN.WEB-DL.DDP5.1.H.264-DUAL-C76.mkv",
            "The.Ark.S03E08.1080p.AMZN.WEB-DL.DD+5.1.H.264-playWEB",
            "A.2024.BluRay.DTS-HD.MA.5.1.x265-G.mkv",
            "A.2024.WEB-DL.AAC2.0.h264-G.mkv",
            "A.2024.WEB-DL.TrueHD.7.1.Atmos.HEVC-G.mkv",
        ] {
            assert!(gaps(ok).is_empty(), "{ok}: {:?}", gaps(ok));
        }
        let toy = gaps("Toy.Story.1995.2160p.UHD.BluRay.X265-IAMABLE.DUAL-Lannister.mkv");
        assert_eq!(toy.len(), 2, "{toy:?}");
        assert!(toy[0].contains("codec de áudio"), "{toy:?}");
        assert!(
            toy[1].contains("o nome usa 'X265' em maiúsculas; o padrão é 'x265'"),
            "{toy:?}"
        );
        // Release refeito: o codec colado no grupo original, seguido de
        // `.DUAL-OUTRO`, o site não lê (visto no envio real do torrent 73347).
        let redone = gaps(
            "Ele.nao.esta.tao.a.Fim.de.Voce.2009.1080p.BluRay.DD5.1.x264-DON.DUAL-xprince00.mkv",
        );
        assert_eq!(redone.len(), 1, "{redone:?}");
        assert!(
            redone[0].contains("'x264' está colado no grupo"),
            "{redone:?}"
        );
        assert_eq!(
            gaps("Resident.Evil.Afterlife.2010.1080p.BluRay.DTS.x264-Japhson.DUAL-ASM").len(),
            1
        );
        let big = gaps("Operacao.Big.Hero.2015.1080p.BluRay.x264-shaggy");
        assert_eq!(big.len(), 1, "{big:?}");
        assert!(big[0].contains("codec de áudio"));
        // Atmos sozinho não é codec; AC3 dentro de outra palavra não conta.
        assert_eq!(gaps("A.Atmos.x264-G").len(), 1);
        assert_eq!(gaps("A.DDX.x264-G").len(), 1);
    }
}

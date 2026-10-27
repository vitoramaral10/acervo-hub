//! Os idiomas de áudio de um arquivo de vídeo, lidos pelo `ffprobe`: valem
//! mais que o nome do release para dizer em que idiomas o arquivo está.
//!
//! Sem `ffprobe` no caminho (ou se ele falha), os idiomas vêm do nome: é
//! informação a mais, não condição para importar.
//!
//! `probe_full` lê o resto (container, duração, vídeo, HDR, áudio e legendas
//! embutidas) para a checagem de "Para lançar", e `screenshot` tira um quadro
//! com o `ffmpeg`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use acervo_parser::Language;
use serde_json::Value;

/// O que se leu do arquivo.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    /// Idiomas das faixas de áudio, sem repetição e sem desconhecidos.
    pub audio_languages: Vec<Language>,
}

/// Binário do `ffprobe`: `ACERVO_FFPROBE` ou o do caminho.
fn binary() -> String {
    std::env::var("ACERVO_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

/// Lê as faixas do arquivo (caminho como o host vê).
pub async fn probe(path: &Path) -> Option<Probe> {
    let value = run_ffprobe(path, &["-show_streams"]).await?;
    Some(from_ffprobe(&value))
}

fn streams<'a>(value: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> {
    value["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |s| s["codec_type"] == kind)
        // Capa embutida vem como vídeo.
        .filter(|s| s["disposition"]["attached_pic"].as_i64() != Some(1))
}

fn language(stream: &Value) -> Option<&str> {
    stream["tags"]["language"]
        .as_str()
        .filter(|l| !l.is_empty() && *l != "und")
}

/// Converte a saída do `ffprobe` (`-show_streams`).
#[must_use]
pub fn from_ffprobe(value: &Value) -> Probe {
    let mut audio_languages: Vec<Language> = Vec::new();
    for code in streams(value, "audio").filter_map(language) {
        let language = Language::from_iso639_2(code);
        if language != Language::Unknown && !audio_languages.contains(&language) {
            audio_languages.push(language);
        }
    }
    Probe { audio_languages }
}

/// Como o vídeo declara a faixa dinâmica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hdr {
    Sdr,
    Hdr10,
    Hlg,
    DolbyVision,
}

/// O vídeo principal do arquivo.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoFacts {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub pix_fmt: Option<String>,
    /// 8, 10 ou 12; `None` quando nem `bits_per_raw_sample` nem o `pix_fmt` dizem.
    pub bit_depth: Option<u8>,
    pub hdr: Hdr,
    /// Quadros por segundo (`avg_frame_rate`); `None` sem a medida.
    pub frame_rate: Option<f64>,
    /// Bitrate do vídeo em bit/s: o `bit_rate` do stream ou, no MKV, a
    /// estatística `BPS` que o mkvmerge grava (a que o `MediaInfo` mostra).
    pub bit_rate: Option<u64>,
}

/// Uma faixa de áudio.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFacts {
    /// Idioma do acervo; `Unknown` quando a faixa não declara ou o código é estranho.
    pub language: Language,
    /// Código como veio no arquivo (`por`, `eng`, `pob`), minúsculo.
    pub raw_language: Option<String>,
    pub codec: String,
    pub channels: Option<u32>,
    pub title: Option<String>,
    /// `profile` do ffprobe (`DTS-HD MA`, `LC`...), quando a faixa o traz.
    pub profile: Option<String>,
}

/// Se a legenda é texto (dá para extrair para `.srt`) ou imagem (PGS,
/// `VobSub`: só o OCR a converteria).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleKind {
    Text,
    Image,
    /// Codec que o acervo não conhece: não se presume que seja texto.
    Unknown,
}

impl SubtitleKind {
    /// Classifica pelo `codec_name` do ffprobe.
    #[must_use]
    pub fn of_codec(codec: &str) -> Self {
        match codec {
            "subrip" | "srt" | "ass" | "ssa" | "mov_text" | "webvtt" => Self::Text,
            "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub" => Self::Image,
            _ => Self::Unknown,
        }
    }
}

/// Uma legenda embutida.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleFacts {
    /// Índice absoluto do stream no arquivo (`-map 0:<índice>`).
    pub index: u32,
    pub kind: SubtitleKind,
    pub language: Language,
    pub raw_language: Option<String>,
    pub codec: String,
    /// Disposição `forced` do arquivo ou, na falta dela, título que diz
    /// "forçada" (muita release marca só pelo título).
    pub forced: bool,
    pub title: Option<String>,
}

/// Tudo que a checagem de lançamento pede do arquivo.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFacts {
    /// `format_name` do ffprobe (`matroska,webm`, `mov,mp4,m4a,3gp,3g2,mj2`).
    pub container: String,
    pub duration_seconds: Option<f64>,
    /// `None` quando o arquivo não tem vídeo (a checagem trata como falha).
    pub video: Option<VideoFacts>,
    pub audio: Vec<AudioFacts>,
    pub subtitles: Vec<SubtitleFacts>,
}

/// Lê o arquivo inteiro (`-show_streams -show_format`).
pub async fn probe_full(path: &Path) -> Option<MediaFacts> {
    let value = run_ffprobe(path, &["-show_streams", "-show_format"]).await?;
    Some(facts_from_ffprobe(&value))
}

/// Roda o `ffprobe` e devolve o JSON; erro e demora viram `None` com aviso.
async fn run_ffprobe(path: &Path, show: &[&str]) -> Option<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new(binary())
            .args(["-v", "quiet", "-print_format", "json"])
            .args(show)
            .arg(path)
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(Ok(output)) => {
            tracing::warn!(arquivo = %path.display(), status = %output.status, "ffprobe falhou");
            return None;
        }
        Ok(Err(error)) => {
            tracing::warn!("ffprobe indisponível: {error}");
            return None;
        }
        Err(_) => {
            tracing::warn!(arquivo = %path.display(), "ffprobe demorou demais");
            return None;
        }
    };
    serde_json::from_slice(&output.stdout).ok()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|t| !t.is_empty()).map(str::to_owned)
}

/// O título da faixa traz "forced", "forçada(s)" ou "foreign" como palavra.
fn title_says_forced(title: Option<&str>) -> bool {
    title.is_some_and(|title| {
        title
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|t| {
                matches!(
                    t,
                    "forced" | "forcada" | "forcadas" | "forçada" | "forçadas" | "foreign"
                )
            })
    })
}

fn raw_language(stream: &Value) -> Option<String> {
    language(stream).map(str::to_ascii_lowercase)
}

/// `pob` (português do Brasil) é o código que alguns muxers gravam; o
/// parser só conhece `por`.
fn language_of(raw: Option<&str>) -> Language {
    match raw {
        Some("pob") => Language::Portuguese,
        Some(code) => Language::from_iso639_2(code),
        None => Language::Unknown,
    }
}

fn bit_depth(stream: &Value) -> Option<u8> {
    if let Some(bits) = stream["bits_per_raw_sample"]
        .as_str()
        .and_then(|b| b.parse::<u8>().ok())
        .filter(|b| *b > 0)
    {
        return Some(bits);
    }
    let pix_fmt = stream["pix_fmt"].as_str()?;
    // yuv420p10le, yuv444p12be, p010le; sem sufixo de profundidade é 8 bits.
    if pix_fmt.starts_with("p010") {
        return Some(10);
    }
    if pix_fmt.starts_with("p012") {
        return Some(12);
    }
    for bits in [16u8, 14, 12, 10, 9] {
        if pix_fmt.contains(&format!("p{bits}")) || pix_fmt.contains(&format!("{bits}le")) {
            return Some(bits);
        }
    }
    Some(8)
}

/// `bit_rate` do stream, ou a tag `BPS` (`BPS-eng` em mkvmerge antigo).
fn bit_rate(stream: &Value) -> Option<u64> {
    let tags = &stream["tags"];
    [&stream["bit_rate"], &tags["BPS"], &tags["BPS-eng"]]
        .into_iter()
        .find_map(|v| v.as_str()?.trim().parse::<u64>().ok())
        .filter(|bps| *bps > 0)
}

/// `24000/1001` → 23.976; `0/0` (sem medida) → `None`.
fn frame_rate(stream: &Value) -> Option<f64> {
    let (num, den) = stream["avg_frame_rate"].as_str()?.split_once('/')?;
    let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
    (num > 0.0 && den > 0.0).then(|| num / den)
}

fn hdr(stream: &Value) -> Hdr {
    let dolby_vision = stream["side_data_list"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|d| {
            d["side_data_type"]
                .as_str()
                .is_some_and(|t| t.contains("DOVI configuration record"))
        });
    if dolby_vision {
        return Hdr::DolbyVision;
    }
    match stream["color_transfer"].as_str() {
        Some("smpte2084") => Hdr::Hdr10,
        Some("arib-std-b67") => Hdr::Hlg,
        _ => Hdr::Sdr,
    }
}

/// Converte a saída do `ffprobe` (`-show_streams -show_format`).
#[must_use]
pub fn facts_from_ffprobe(value: &Value) -> MediaFacts {
    let video = streams(value, "video").next().map(|s| VideoFacts {
        codec: s["codec_name"].as_str().unwrap_or_default().to_owned(),
        width: s["width"]
            .as_u64()
            .and_then(|w| u32::try_from(w).ok())
            .unwrap_or(0),
        height: s["height"]
            .as_u64()
            .and_then(|h| u32::try_from(h).ok())
            .unwrap_or(0),
        pix_fmt: text(&s["pix_fmt"]),
        bit_depth: bit_depth(s),
        hdr: hdr(s),
        frame_rate: frame_rate(s),
        bit_rate: bit_rate(s),
    });
    let audio = streams(value, "audio")
        .map(|s| {
            let raw = raw_language(s);
            AudioFacts {
                language: language_of(raw.as_deref()),
                raw_language: raw,
                codec: s["codec_name"].as_str().unwrap_or_default().to_owned(),
                channels: s["channels"].as_u64().and_then(|c| u32::try_from(c).ok()),
                title: text(&s["tags"]["title"]),
                profile: text(&s["profile"]),
            }
        })
        .collect();
    let subtitles = streams(value, "subtitle")
        .map(|s| {
            let raw = raw_language(s);
            let codec = s["codec_name"].as_str().unwrap_or_default().to_owned();
            SubtitleFacts {
                // O ffprobe sempre grava `index`; sem ele o `-map` falharia
                // alto no ffmpeg em vez de pegar a faixa errada.
                index: s["index"]
                    .as_u64()
                    .and_then(|i| u32::try_from(i).ok())
                    .unwrap_or(0),
                kind: SubtitleKind::of_codec(&codec),
                language: language_of(raw.as_deref()),
                raw_language: raw,
                codec,
                forced: s["disposition"]["forced"].as_i64() == Some(1)
                    || title_says_forced(text(&s["tags"]["title"]).as_deref()),
                title: text(&s["tags"]["title"]),
            }
        })
        .collect();
    MediaFacts {
        container: value["format"]["format_name"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        // O ffprobe grava a duração como texto ("6720.032000").
        duration_seconds: value["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| d.is_finite() && *d > 0.0),
        video,
        audio,
        subtitles,
    }
}

/// Binário do `ffmpeg`: `ACERVO_FFMPEG` ou o do caminho.
fn ffmpeg_binary() -> String {
    std::env::var("ACERVO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Um ffmpeg por vez, no preparo, na rota de download e no envio: um quadro
/// 4K ocupa dezenas de MiB e o hub divide a máquina com o resto. Quem chega
/// depois espera a vez.
static ONE_AT_A_TIME: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// O maior arquivo que o upload de imagem do BJ aceita (`maxFilesize: 32` do
/// Dropzone; aqui 32 MB decimais, a leitura mais estrita).
pub const LIMITE_CAPTURA_BJ: usize = 32_000_000;

/// O maior arquivo que o formulário do Amigos aceita numa imagem (5 MB; aqui
/// 5 MiB, a leitura segura). Passou disso, o site recusa o anexo.
pub const LIMITE_CAPTURA: usize = 5 * 1024 * 1024;

/// Qualidades do `-q:v` do mjpeg tentadas em ordem (2 é a mais alta usual).
/// Poucas, porque cada tentativa é um ffmpeg inteiro.
const QUALIDADES_JPEG: [u8; 5] = [2, 3, 4, 6, 9];

/// O formato de uma captura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFormat {
    Png,
    Jpg,
}

impl CaptureFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
        }
    }

    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpg => "image/jpeg",
        }
    }
}

/// Uma tentativa de codificar o quadro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    Png,
    /// PNG na compressão padrão do ffmpeg, para o BJ: no 4K com meia CPU a
    /// máxima custa ~65 s a mais por quadro para encolher ~16%, e só o limite
    /// do Amigos precisa disso.
    PngQuick,
    Jpeg(u8),
}

/// A decisão pura: dado o que acabou de sair e o tamanho, a próxima tentativa,
/// ou `None` quando coube (ou quando não há mais o que tentar). PNG sem perda
/// primeiro; passou do limite, JPEG na mesma resolução, subindo a compressão.
fn next_attempt(done: Attempt, size: usize) -> Option<Attempt> {
    if size <= LIMITE_CAPTURA {
        return None;
    }
    match done {
        Attempt::Png | Attempt::PngQuick => Some(Attempt::Jpeg(QUALIDADES_JPEG[0])),
        Attempt::Jpeg(q) => QUALIDADES_JPEG
            .iter()
            .copied()
            .find(|candidate| *candidate > q)
            .map(Attempt::Jpeg),
    }
}

/// Um quadro do vídeo na resolução original (as regras pedem a resolução
/// real; nunca se escala). Sai em PNG, com a compressão máxima e sem perda;
/// se passar de [`LIMITE_CAPTURA`], em JPEG de qualidade alta na mesma
/// resolução, a mais alta que couber. Se nem a mais comprimida couber,
/// devolve a menor e registra o aviso. Devolve os bytes e o formato gerado.
///
/// `-ss` antes de `-i` busca pelo índice, sem decodificar o arquivo até lá.
/// `-threads 1` antes de `-i` limita o decodificador (depois do `-i` só
/// valeria para o encoder): sem ele o ffmpeg sobe uma thread por núcleo do
/// host, ignorando a cota de CPU do container, e a memória estoura.
///
/// `-pix_fmt rgb24` (PNG) e `yuvj420p` (JPEG) forçam 8 bits por canal, sem
/// escala: fonte de 10 bits faria o encoder escolher `rgb48be` (16 bits), um
/// PNG 4K de ~40 MB que passa do limite e ocupa dezenas de MiB a mais na
/// memória do hub.
pub async fn screenshot(path: &Path, at_seconds: f64) -> anyhow::Result<(Vec<u8>, CaptureFormat)> {
    let _turn = ONE_AT_A_TIME.acquire().await?;
    let mut attempt = Attempt::Png;
    let mut smallest: Option<(Vec<u8>, CaptureFormat)> = None;
    loop {
        let bytes = encode_frame(path, at_seconds, attempt).await?;
        let format = match attempt {
            Attempt::Png | Attempt::PngQuick => CaptureFormat::Png,
            Attempt::Jpeg(_) => CaptureFormat::Jpg,
        };
        let next = next_attempt(attempt, bytes.len());
        if smallest
            .as_ref()
            .is_none_or(|(best, _)| bytes.len() < best.len())
        {
            smallest = Some((bytes, format));
        }
        match next {
            Some(next) => attempt = next,
            None => break,
        }
    }
    let (bytes, format) = smallest.expect("ao menos uma tentativa");
    if bytes.len() > LIMITE_CAPTURA {
        tracing::warn!(
            bytes = bytes.len(),
            "captura acima do limite de 5 MiB do Amigos, mesmo no JPEG mais comprimido"
        );
    }
    Ok((bytes, format))
}

/// Um quadro em PNG sem perda na resolução original, sem o recuo para JPEG de
/// [`screenshot`]: é o que vai ao BJ, que aceita até [`LIMITE_CAPTURA_BJ`].
/// Passou disso (um PNG 4K de 8 bits fica bem abaixo), é erro.
pub async fn screenshot_png(path: &Path, at_seconds: f64) -> anyhow::Result<Vec<u8>> {
    let _turn = ONE_AT_A_TIME.acquire().await?;
    let bytes = encode_frame(path, at_seconds, Attempt::PngQuick).await?;
    anyhow::ensure!(
        bytes.len() <= LIMITE_CAPTURA_BJ,
        "captura em PNG com {} bytes, acima do limite de 32 MB do BJ",
        bytes.len()
    );
    Ok(bytes)
}

async fn encode_frame(path: &Path, at_seconds: f64, attempt: Attempt) -> anyhow::Result<Vec<u8>> {
    // `-compression_level 100` é cortado no máximo do zlib (9); `-pred mixed`
    // escolhe o filtro por linha e rende uns 5% a mais.
    let (encoder, pix_fmt): (Vec<String>, &str) = match attempt {
        Attempt::Png => (
            ["-c:v", "png", "-compression_level", "100", "-pred", "mixed"]
                .map(str::to_owned)
                .into(),
            "rgb24",
        ),
        Attempt::PngQuick => (["-c:v", "png"].map(str::to_owned).into(), "rgb24"),
        Attempt::Jpeg(q) => (
            ["-c:v", "mjpeg", "-q:v", &q.to_string()]
                .map(str::to_owned)
                .into(),
            "yuvj420p",
        ),
    };
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-threads", "1", "-ss"])
            .arg(format!("{at_seconds:.3}"))
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-threads", "2", "-f", "image2pipe"])
            .args(encoder)
            .args(["-pix_fmt", pix_fmt, "-"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("ffmpeg demorou demais para tirar o quadro"))?
    .map_err(|error| anyhow::anyhow!("ffmpeg indisponível: {error}"))?;
    if !output.status.success() {
        // O stderr do ffmpeg cita o caminho do arquivo, nada de segredo.
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "ffmpeg falhou ({}): {}",
            output.status,
            stderr.lines().last().unwrap_or("sem mensagem")
        );
    }
    let signature_ok = match attempt {
        Attempt::Png | Attempt::PngQuick => output.stdout.starts_with(&PNG_SIGNATURE),
        Attempt::Jpeg(_) => output.stdout.starts_with(&[0xff, 0xd8, 0xff]),
    };
    if !signature_ok {
        anyhow::bail!("ffmpeg não devolveu a imagem (posição além do fim do vídeo?)");
    }
    Ok(output.stdout)
}

/// Binário do `mediainfo`: `ACERVO_MEDIAINFO` ou o do caminho.
fn mediainfo_binary() -> String {
    std::env::var("ACERVO_MEDIAINFO").unwrap_or_else(|_| "mediainfo".into())
}

/// O relatório do `MediaInfo` (`--Output=TEXT`) do vídeo, que o formulário do
/// Amigos exige. Roda na pasta do vídeo com só o nome do arquivo como
/// argumento: assim o "Complete name" sai sem o caminho do servidor.
pub async fn mediainfo_report(video: &Path) -> anyhow::Result<String> {
    report_with(&mediainfo_binary(), video).await
}

async fn report_with(binary: &str, video: &Path) -> anyhow::Result<String> {
    let name = video
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("o vídeo {} não tem nome de arquivo", video.display()))?;
    let dir = match video.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    // Pasta ausente faria o spawn falhar com `NotFound`, igual a binário ausente.
    anyhow::ensure!(
        dir.is_dir(),
        "a pasta do vídeo sumiu: {}",
        dir.file_name().map_or_else(
            || "(raiz)".into(),
            |nome| nome.to_string_lossy().into_owned()
        )
    );
    // O CLI trata como opção todo argumento que começa com '-' e não aceita
    // `--`: o nome vem do torrent, então ganha o `./`.
    let dash = name.as_encoded_bytes().first() == Some(&b'-');
    let arg = if dash {
        Path::new(".").join(name)
    } else {
        name.into()
    };
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(binary)
            .current_dir(dir)
            .arg("--Output=TEXT")
            .arg(&arg)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("o mediainfo demorou demais para ler o vídeo"))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "o mediainfo não está instalado ({binary} não existe); sem ele não há \
                 relatório para o formulário"
            )
        } else {
            anyhow::anyhow!("mediainfo indisponível: {error}")
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "o mediainfo falhou ({}): {}",
            output.status,
            stderr.lines().last().unwrap_or("sem mensagem")
        );
    }
    let report = String::from_utf8(output.stdout)
        .map_err(|_| anyhow::anyhow!("o relatório do mediainfo não é UTF-8"))?;
    let report = report.trim().to_owned();
    anyhow::ensure!(
        !report.is_empty(),
        "o mediainfo devolveu um relatório vazio"
    );
    Ok(if dash {
        strip_dot_slash(&report)
    } else {
        report
    })
}

/// Tira o `./` que `report_with` pôs no "Complete name" (o `MediaInfo` repete o
/// caminho como recebeu).
fn strip_dot_slash(report: &str) -> String {
    report
        .lines()
        .map(|line| match line.split_once(" : ") {
            Some((key, value)) if key.trim_end() == "Complete name" => {
                format!("{key} : {}", value.strip_prefix("./").unwrap_or(value))
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A legenda de texto embutida, como `.srt` em UTF-8: o stream `stream_index`
/// (índice absoluto do arquivo) convertido pelo `ffmpeg`.
pub async fn extract_subtitle_srt(path: &Path, stream_index: u32) -> anyhow::Result<Vec<u8>> {
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-i"])
            .arg(path)
            .arg("-map")
            .arg(format!("0:{stream_index}"))
            .args(["-c:s", "srt", "-f", "srt", "-"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("ffmpeg demorou demais para extrair a legenda"))?
    .map_err(|error| anyhow::anyhow!("ffmpeg indisponível: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "ffmpeg falhou ({}): {}",
            output.status,
            stderr.lines().last().unwrap_or("sem mensagem")
        );
    }
    check_srt(output.stdout, &output.stderr)
}

/// Confere a saída do ffmpeg antes de ela virar arquivo de lançamento. Com
/// `-loglevel error`, qualquer coisa no stderr é falha: o ffmpeg sai com 0 mesmo
/// descartando em silêncio as falas que não são UTF-8. Também recusa a saída
/// vazia ou sem nenhuma fala.
fn check_srt(stdout: Vec<u8>, stderr: &[u8]) -> anyhow::Result<Vec<u8>> {
    let stderr = String::from_utf8_lossy(stderr);
    if let Some(line) = stderr.lines().rev().find(|l| !l.trim().is_empty()) {
        anyhow::bail!("o ffmpeg reclamou ao extrair a legenda, que pode estar incompleta: {line}");
    }
    // O srt do ffmpeg é UTF-8; se vier outra coisa, não vira arquivo de lançamento.
    let text = std::str::from_utf8(&stdout)
        .map_err(|_| anyhow::anyhow!("a legenda extraída não é UTF-8 válido"))?;
    if !text.contains(" --> ") {
        anyhow::bail!("a faixa de legenda não tem falas");
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn converte_saida_do_ffprobe() {
        let probe = from_ffprobe(&json!({
            "streams": [
                {"codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 1606,
                 "pix_fmt": "yuv420p10le", "avg_frame_rate": "24000/1001",
                 "color_transfer": "smpte2084", "bit_rate": "14105340"},
                {"codec_type": "audio", "codec_name": "ac3", "channels": 6, "bit_rate": "384000",
                 "tags": {"language": "por"}},
                {"codec_type": "audio", "codec_name": "eac3", "channels": 6,
                 "tags": {"language": "eng"}},
                {"codec_type": "subtitle", "codec_name": "subrip", "tags": {"language": "por"}},
                {"codec_type": "subtitle", "codec_name": "subrip", "tags": {"language": "und"}},
                {"codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}}
            ]
        }));
        assert_eq!(
            probe.audio_languages,
            [Language::Portuguese, Language::English]
        );
    }

    #[test]
    fn le_quadros_por_segundo() {
        let rate = |r: &str| frame_rate(&json!({ "avg_frame_rate": r }));
        assert!(rate("24000/1001").is_some_and(|f| (f - 23.976).abs() < 0.001));
        assert_eq!(rate("50/1"), Some(50.0));
        assert_eq!(rate("0/0"), None);
        assert_eq!(frame_rate(&json!({})), None);
    }

    #[test]
    fn le_o_bitrate_do_video() {
        assert_eq!(
            bit_rate(&json!({ "bit_rate": "14105340" })),
            Some(14_105_340)
        );
        assert_eq!(
            bit_rate(&json!({ "tags": { "BPS": "5998123" } })),
            Some(5_998_123)
        );
        assert_eq!(
            bit_rate(&json!({ "tags": { "BPS-eng": "6000000" } })),
            Some(6_000_000)
        );
        assert_eq!(bit_rate(&json!({})), None);
    }

    /// Amostra: filme 4K HEVC com HDR10, uma faixa PT e uma original.
    fn amostra_hdr10() -> Value {
        json!({
            "streams": [
                {"codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 1600,
                 "pix_fmt": "yuv420p10le", "bits_per_raw_sample": "10",
                 "color_transfer": "smpte2084", "disposition": {"default": 1}},
                {"codec_type": "audio", "codec_name": "eac3", "channels": 6,
                 "tags": {"language": "por", "title": "Dublado"}},
                {"codec_type": "audio", "codec_name": "truehd", "channels": 8,
                 "tags": {"language": "eng"}}
            ],
            "format": {"format_name": "matroska,webm", "duration": "6720.032000"}
        })
    }

    #[test]
    fn extrai_container_duracao_e_hdr10() {
        let fatos = facts_from_ffprobe(&amostra_hdr10());
        assert_eq!(fatos.container, "matroska,webm");
        assert_eq!(fatos.duration_seconds, Some(6720.032));
        let video = fatos.video.expect("tem vídeo");
        assert_eq!(video.codec, "hevc");
        assert_eq!((video.width, video.height), (3840, 1600));
        assert_eq!(video.bit_depth, Some(10));
        assert_eq!(video.hdr, Hdr::Hdr10);
        assert_eq!(fatos.audio.len(), 2);
        assert_eq!(fatos.audio[0].language, Language::Portuguese);
        assert_eq!(fatos.audio[0].title.as_deref(), Some("Dublado"));
        assert_eq!(fatos.audio[1].channels, Some(8));
        assert!(fatos.subtitles.is_empty());
    }

    #[test]
    fn reconhece_dolby_vision_pelo_registro_de_configuracao() {
        let fatos = facts_from_ffprobe(&json!({
            "streams": [
                {"codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 2160,
                 "pix_fmt": "yuv420p10le", "color_transfer": "smpte2084",
                 "side_data_list": [
                     {"side_data_type": "DOVI configuration record", "dv_profile": 8}
                 ]}
            ],
            "format": {"format_name": "matroska,webm", "duration": "3600.0"}
        }));
        // Dolby Vision vence o HDR10 de base.
        assert_eq!(fatos.video.unwrap().hdr, Hdr::DolbyVision);
    }

    #[test]
    fn reconhece_hlg() {
        let fatos = facts_from_ffprobe(&json!({
            "streams": [{"codec_type": "video", "codec_name": "hevc", "width": 1920,
                         "height": 1080, "pix_fmt": "yuv420p10le",
                         "color_transfer": "arib-std-b67"}],
            "format": {"format_name": "mpegts"}
        }));
        assert_eq!(fatos.video.unwrap().hdr, Hdr::Hlg);
        assert_eq!(fatos.duration_seconds, None);
    }

    #[test]
    fn sdr_com_tres_audios_e_legendas_embutidas() {
        let fatos = facts_from_ffprobe(&json!({
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
                 "pix_fmt": "yuv420p", "color_transfer": "bt709"},
                {"codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}},
                {"codec_type": "audio", "codec_name": "ac3", "channels": 6,
                 "tags": {"language": "pob"}},
                {"codec_type": "audio", "codec_name": "aac", "channels": 2,
                 "tags": {"language": "ENG"}},
                {"codec_type": "audio", "codec_name": "aac", "channels": 2,
                 "tags": {"language": "und"}},
                {"codec_type": "subtitle", "codec_name": "subrip",
                 "tags": {"language": "por", "title": "Forçada"},
                 "disposition": {"forced": 1}},
                {"codec_type": "subtitle", "codec_name": "ass",
                 "tags": {"language": "eng"}, "disposition": {"forced": 0}}
            ],
            "format": {"format_name": "matroska,webm", "duration": "2640.5"}
        }));
        let video = fatos.video.expect("tem vídeo");
        assert_eq!(video.hdr, Hdr::Sdr);
        assert_eq!(video.bit_depth, Some(8));
        assert_eq!(fatos.audio.len(), 3);
        assert_eq!(fatos.audio[0].language, Language::Portuguese);
        assert_eq!(fatos.audio[0].raw_language.as_deref(), Some("pob"));
        assert_eq!(fatos.audio[1].language, Language::English);
        assert_eq!(fatos.audio[1].raw_language.as_deref(), Some("eng"));
        assert_eq!(fatos.audio[2].language, Language::Unknown);
        assert_eq!(fatos.audio[2].raw_language, None);
        assert_eq!(fatos.subtitles.len(), 2);
        assert_eq!(fatos.subtitles[0].language, Language::Portuguese);
        assert!(fatos.subtitles[0].forced);
        assert_eq!(fatos.subtitles[0].title.as_deref(), Some("Forçada"));
        assert_eq!(fatos.subtitles[1].codec, "ass");
        assert!(!fatos.subtitles[1].forced);
    }

    #[test]
    fn legenda_forcada_so_pelo_titulo_conta_como_forcada() {
        let fatos = facts_from_ffprobe(&json!({
            "streams": [
                {"codec_type": "subtitle", "codec_name": "subrip", "index": 3,
                 "tags": {"language": "por", "title": "Forçada"}, "disposition": {"forced": 0}},
                {"codec_type": "subtitle", "codec_name": "subrip", "index": 4,
                 "tags": {"language": "por", "title": "Forced Narrative"}},
                {"codec_type": "subtitle", "codec_name": "subrip", "index": 5,
                 "tags": {"language": "por", "title": "Completa"}}
            ],
            "format": {"format_name": "matroska,webm"}
        }));
        let forced: Vec<bool> = fatos.subtitles.iter().map(|s| s.forced).collect();
        assert_eq!(forced, [true, true, false]);
    }

    #[test]
    fn extracao_recusa_saida_com_aviso_do_ffmpeg_mesmo_com_falas() {
        let srt = b"1\n00:00:00,100 --> 00:00:01,000\nOla\n".to_vec();
        let error = check_srt(
            srt,
            b"[srt] Invalid UTF-8 in decoded subtitles text; maybe missing -sub_charenc option\n",
        )
        .unwrap_err();
        assert!(format!("{error}").contains("Invalid UTF-8"), "{error}");
    }

    #[test]
    fn extracao_recusa_legenda_vazia_ou_sem_falas() {
        for empty in [&b""[..], b"\n"] {
            let error = check_srt(empty.to_vec(), b"").unwrap_err();
            assert!(format!("{error}").contains("não tem falas"), "{error}");
        }
    }

    #[test]
    fn extracao_aceita_srt_utf8_com_falas_e_sem_aviso() {
        let srt = "1\n00:00:00,100 --> 00:00:01,000\nAção rápida\n"
            .as_bytes()
            .to_vec();
        assert_eq!(check_srt(srt.clone(), b"").unwrap(), srt);
    }

    #[test]
    fn legenda_guarda_o_indice_absoluto_do_stream() {
        let fatos = facts_from_ffprobe(&json!({
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "h264", "width": 1,
                 "height": 1},
                {"index": 1, "codec_type": "audio", "codec_name": "aac"},
                {"index": 2, "codec_type": "audio", "codec_name": "aac"},
                {"index": 3, "codec_type": "subtitle", "codec_name": "subrip",
                 "tags": {"language": "por"}},
                {"index": 4, "codec_type": "subtitle", "codec_name": "hdmv_pgs_subtitle",
                 "tags": {"language": "eng"}}
            ],
            "format": {"format_name": "matroska,webm"}
        }));
        let indices: Vec<u32> = fatos.subtitles.iter().map(|s| s.index).collect();
        assert_eq!(indices, [3, 4]);
    }

    #[test]
    fn classifica_legenda_em_texto_imagem_ou_desconhecida() {
        for codec in ["subrip", "ass", "ssa", "mov_text", "webvtt"] {
            assert_eq!(SubtitleKind::of_codec(codec), SubtitleKind::Text, "{codec}");
        }
        for codec in ["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub"] {
            assert_eq!(
                SubtitleKind::of_codec(codec),
                SubtitleKind::Image,
                "{codec}"
            );
        }
        assert_eq!(SubtitleKind::of_codec("eia_608"), SubtitleKind::Unknown);
        assert_eq!(SubtitleKind::of_codec(""), SubtitleKind::Unknown);
    }

    #[test]
    fn legenda_do_ffprobe_traz_a_classificacao_do_codec() {
        let fatos = facts_from_ffprobe(&json!({
            "streams": [
                {"index": 2, "codec_type": "subtitle", "codec_name": "ass"},
                {"index": 3, "codec_type": "subtitle", "codec_name": "dvd_subtitle"}
            ],
            "format": {"format_name": "matroska,webm"}
        }));
        assert_eq!(fatos.subtitles[0].kind, SubtitleKind::Text);
        assert_eq!(fatos.subtitles[1].kind, SubtitleKind::Image);
    }

    #[test]
    fn profundidade_vem_do_pix_fmt_sem_bits_por_amostra() {
        for (pix_fmt, esperado) in [
            ("yuv420p", 8),
            ("yuv420p10le", 10),
            ("yuv444p12le", 12),
            ("p010le", 10),
        ] {
            let fatos = facts_from_ffprobe(&json!({
                "streams": [{"codec_type": "video", "codec_name": "hevc", "width": 1,
                             "height": 1, "pix_fmt": pix_fmt}],
                "format": {"format_name": "x"}
            }));
            assert_eq!(fatos.video.unwrap().bit_depth, Some(esperado), "{pix_fmt}");
        }
    }

    #[test]
    fn decisao_do_formato_segue_o_tamanho_ate_caber() {
        let cabe = LIMITE_CAPTURA;
        let passa = LIMITE_CAPTURA + 1;
        // Coube: para onde está.
        assert_eq!(next_attempt(Attempt::Png, cabe), None);
        assert_eq!(next_attempt(Attempt::Jpeg(3), 1), None);
        // PNG grande demais: JPEG na qualidade mais alta.
        assert_eq!(
            next_attempt(Attempt::Png, passa),
            Some(Attempt::Jpeg(QUALIDADES_JPEG[0]))
        );
        // JPEG grande demais: a próxima qualidade da lista, até acabar.
        let mut tentativa = Attempt::Png;
        let mut vistas = Vec::new();
        while let Some(next) = next_attempt(tentativa, passa) {
            vistas.push(next);
            tentativa = next;
        }
        let esperado: Vec<_> = QUALIDADES_JPEG.map(Attempt::Jpeg).into();
        assert_eq!(vistas, esperado);
        assert!(QUALIDADES_JPEG.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn formato_da_captura_tem_extensao_e_mime() {
        assert_eq!(CaptureFormat::Png.extension(), "png");
        assert_eq!(CaptureFormat::Png.mime(), "image/png");
        assert_eq!(CaptureFormat::Jpg.extension(), "jpg");
        assert_eq!(CaptureFormat::Jpg.mime(), "image/jpeg");
    }

    /// Largura e altura do primeiro marcador SOF de um JPEG.
    fn jpeg_dimensions(jpeg: &[u8]) -> (u32, u32) {
        let mut i = 2;
        while i + 9 < jpeg.len() {
            assert_eq!(jpeg[i], 0xff, "marcador em {i}");
            let marker = jpeg[i + 1];
            let length = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
            if matches!(marker, 0xc0..=0xc3) {
                let height = u32::from(u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]));
                let width = u32::from(u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]));
                return (width, height);
            }
            i += 2 + length;
        }
        panic!("JPEG sem SOF");
    }

    /// Um vídeo de um quadro feito pelo ffmpeg a partir de `filtro`; `None`
    /// quando o ffmpeg não existe ou não sabe fazê-lo.
    fn video_de_um_quadro(dir: &Path, filtro: &str) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let video = dir.join("quadro.mkv");
        std::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(filtro)
            .args(["-frames:v", "1", "-c:v", "ffv1"])
            .arg(&video)
            .status()
            .is_ok_and(|s| s.success())
            .then_some(video)
    }

    #[tokio::test]
    async fn quadro_de_ruido_4k_passa_de_5_mib_em_png_e_sai_em_jpeg_na_resolucao_cheia() {
        if !binario_existe(&ffmpeg_binary()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("acervo-captura-ruido-{}", std::process::id()));
        let filtro = "nullsrc=s=3840x2160,geq=random(1)*255:128:128";
        let Some(video) = video_de_um_quadro(&dir, filtro) else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };

        let (bytes, formato) = screenshot(&video, 0.0).await.expect("quadro");
        // O PNG desse ruído não cabe (mais de 10 MB): tem de ter caído no JPEG.
        assert_eq!(formato, CaptureFormat::Jpg);
        assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));
        assert!(bytes.len() <= LIMITE_CAPTURA, "{} bytes", bytes.len());
        assert_eq!(jpeg_dimensions(&bytes), (3840, 2160), "sem escala");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn quadro_liso_fica_em_png_na_resolucao_cheia() {
        if !binario_existe(&ffmpeg_binary()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("acervo-captura-liso-{}", std::process::id()));
        let Some(video) = video_de_um_quadro(&dir, "color=c=gray:s=1920x1080") else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };

        let (bytes, formato) = screenshot(&video, 0.0).await.expect("quadro");
        assert_eq!(formato, CaptureFormat::Png);
        assert!(bytes.starts_with(&PNG_SIGNATURE));
        assert!(bytes.len() <= LIMITE_CAPTURA);
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 1920);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 1080);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn binario_existe(nome: &str) -> bool {
        std::process::Command::new(nome)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[tokio::test]
    async fn tira_um_quadro_de_video_gerado_pelo_ffmpeg() {
        if !binario_existe(&ffmpeg_binary()) || !binario_existe(&binary()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("acervo-mediainfo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("amostra.mkv");
        let gerado = std::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=2:size=320x240:rate=10")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status();
        if !gerado.is_ok_and(|s| s.success()) {
            // ffmpeg sem libx264: não há o que testar aqui.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let fatos = probe_full(&video).await.expect("ffprobe leu o arquivo");
        assert!(fatos.container.contains("matroska"));
        let video_facts = fatos.video.expect("tem vídeo");
        assert_eq!((video_facts.width, video_facts.height), (320, 240));
        assert!(
            fatos
                .duration_seconds
                .is_some_and(|d| (1.5..2.5).contains(&d))
        );

        let (png, formato) = screenshot(&video, 1.0).await.expect("quadro");
        assert_eq!(formato, CaptureFormat::Png);
        assert!(png.starts_with(&PNG_SIGNATURE));
        // Além do fim não sai quadro: erro claro, não PNG vazio.
        assert!(screenshot(&video, 600.0).await.is_err());
        // O PNG rápido do BJ também é PNG.
        let rapido = screenshot_png(&video, 1.0).await.expect("quadro");
        assert!(rapido.starts_with(&PNG_SIGNATURE));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn quadro_de_fonte_de_dez_bits_sai_em_oito_bits_por_canal() {
        if !binario_existe(&ffmpeg_binary()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("acervo-captura-10bit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("dez-bits.mkv");
        // FFV1 guarda yuv420p10le sem depender de libx265.
        let gerado = std::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=1:size=320x240:rate=5")
            .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p10le"])
            .arg(&video)
            .status();
        if !gerado.is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let (png, _) = screenshot(&video, 0.0).await.expect("quadro");
        // IHDR: 8 bytes de assinatura, 8 de cabeçalho do chunk, 4 + 4 de
        // largura e altura; depois a profundidade (byte 24) e o tipo de cor.
        assert_eq!(png[24], 8, "profundidade de cor do PNG");
        assert_eq!(png[25], 2, "RGB sem alfa");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn extrai_de_volta_a_legenda_srt_embutida_no_mkv() {
        if !binario_existe(&ffmpeg_binary()) || !binario_existe(&binary()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("acervo-legenda-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("entrada.srt");
        std::fs::write(
            &srt,
            "1\n00:00:00,100 --> 00:00:01,000\nOlá, ação!\n\n2\n00:00:01,200 --> 00:00:01,900\nSegunda fala\n",
        )
        .unwrap();
        let video = dir.join("com-legenda.mkv");
        let gerado = std::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=2:size=160x120:rate=10")
            .arg("-i")
            .arg(&srt)
            .args(["-map", "0:v", "-map", "1:s", "-c:v", "mpeg4", "-c:s", "srt"])
            .arg(&video)
            .status();
        if !gerado.is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let fatos = probe_full(&video).await.expect("ffprobe leu o arquivo");
        assert_eq!(fatos.subtitles.len(), 1);
        let legenda = &fatos.subtitles[0];
        assert_eq!(legenda.kind, SubtitleKind::Text);
        assert_eq!(legenda.index, 1);

        let bytes = extract_subtitle_srt(&video, legenda.index)
            .await
            .expect("legenda extraída");
        let texto = String::from_utf8(bytes).unwrap();
        assert!(texto.contains("Olá, ação!"), "{texto}");
        assert!(texto.contains("Segunda fala"), "{texto}");
        assert!(texto.contains("00:00:00,100 --> 00:00:01,000"), "{texto}");
        // Stream que não existe: erro claro, não arquivo vazio.
        assert!(extract_subtitle_srt(&video, 9).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Binário falso: imprime o diretório de trabalho e os argumentos.
    #[cfg(unix)]
    fn mediainfo_falso(dir: &Path, corpo: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("mediainfo-falso.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{corpo}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn relatorio_roda_na_pasta_do_video_com_so_o_nome() {
        let dir = std::env::temp_dir().join(format!("acervo-mi-falso-{}", std::process::id()));
        let pasta = dir.join("Filme 2024");
        std::fs::create_dir_all(&pasta).unwrap();
        let script = mediainfo_falso(
            &dir,
            "echo \"cwd=$(pwd)\"; echo \"n=$#\"; for a in \"$@\"; do echo \"arg=$a\"; done",
        );
        let video = pasta.join("Filme 2024.mkv");
        let relatorio = report_with(script.to_str().unwrap(), &video).await.unwrap();
        let cwd = std::fs::canonicalize(&pasta).unwrap();
        assert!(
            relatorio.contains(&format!("cwd={}", cwd.display())),
            "{relatorio}"
        );
        assert!(relatorio.contains("n=2"), "{relatorio}");
        assert!(relatorio.contains("arg=--Output=TEXT"), "{relatorio}");
        assert!(relatorio.contains("arg=Filme 2024.mkv"), "{relatorio}");
        assert!(!relatorio.contains("arg=/"), "{relatorio}");

        // Falha do binário: erro com a última linha do stderr.
        let ruim = mediainfo_falso(&dir, "echo 'arquivo ilegível' >&2; exit 3");
        let erro = report_with(ruim.to_str().unwrap(), &video)
            .await
            .unwrap_err()
            .to_string();
        assert!(erro.contains("arquivo ilegível"), "{erro}");

        // Saída vazia não vira relatório.
        let vazio = mediainfo_falso(&dir, "exit 0");
        assert!(report_with(vazio.to_str().unwrap(), &video).await.is_err());

        // Nome que começa com '-' não pode virar opção do CLI; o "./" sai do relatório.
        let traco = pasta.join("--Filme.mkv");
        let eco = mediainfo_falso(
            &dir,
            "for a in \"$@\"; do echo \"arg=$a\"; done; echo \"Complete name : $2\"",
        );
        let relatorio = report_with(eco.to_str().unwrap(), &traco).await.unwrap();
        assert!(relatorio.contains("arg=./--Filme.mkv"), "{relatorio}");
        assert!(
            relatorio.contains("Complete name : --Filme.mkv"),
            "{relatorio}"
        );

        // Pasta ausente: não é "mediainfo não instalado".
        let erro = report_with(eco.to_str().unwrap(), &dir.join("sumiu/Filme.mkv"))
            .await
            .unwrap_err()
            .to_string();
        assert!(erro.contains("pasta do vídeo sumiu"), "{erro}");
        assert!(!erro.contains("instalado"), "{erro}");

        // Binário inexistente: mensagem clara.
        let inexistente = dir.join("nao-existe");
        let erro = report_with(inexistente.to_str().unwrap(), &video)
            .await
            .unwrap_err()
            .to_string();
        assert!(erro.contains("não está instalado"), "{erro}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn relatorio_real_do_mediainfo_traz_o_nome_sem_caminho() {
        if !binario_existe(&ffmpeg_binary()) || !binario_existe_com("mediainfo", "--Version") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("acervo-mi-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("real.mkv");
        let gerado = std::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=duration=1:size=160x120:rate=10")
            .args(["-c:v", "mpeg4"])
            .arg(&video)
            .status();
        if gerado.is_ok_and(|s| s.success()) {
            let relatorio = mediainfo_report(&video).await.unwrap();
            let linha = relatorio
                .lines()
                .find(|l| l.starts_with("Complete name"))
                .unwrap_or_default();
            assert!(linha.trim_end().ends_with(": real.mkv"), "{relatorio}");
            assert!(!relatorio.contains(&dir.to_string_lossy().into_owned()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn binario_existe_com(nome: &str, flag: &str) -> bool {
        std::process::Command::new(nome)
            .arg(flag)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }
}

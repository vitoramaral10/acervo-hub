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
}

/// Uma legenda embutida.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleFacts {
    pub language: Language,
    pub raw_language: Option<String>,
    pub codec: String,
    /// Disposição `forced` do arquivo.
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
            }
        })
        .collect();
    let subtitles = streams(value, "subtitle")
        .map(|s| {
            let raw = raw_language(s);
            SubtitleFacts {
                language: language_of(raw.as_deref()),
                raw_language: raw,
                codec: s["codec_name"].as_str().unwrap_or_default().to_owned(),
                forced: s["disposition"]["forced"].as_i64() == Some(1),
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

/// Um quadro do vídeo, em PNG, na resolução original (as regras pedem a
/// resolução real). `-ss` antes de `-i` busca pelo índice, sem decodificar o
/// arquivo até lá. `-threads 1` antes de `-i` limita o decodificador (depois
/// do `-i` só valeria para o encoder): sem ele o ffmpeg sobe uma thread por
/// núcleo do host, ignorando a cota de CPU do container, e a memória estoura.
///
/// `-pix_fmt rgb24` força 8 bits por canal: fonte de 10 bits faria o encoder
/// escolher `rgb48be` (16 bits), um PNG 4K de ~40 MB que passa do limite do
/// `ImgBB` e ocupa dezenas de MiB a mais na memória do hub.
pub async fn screenshot(path: &Path, at_seconds: f64) -> anyhow::Result<Vec<u8>> {
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(ffmpeg_binary())
            .args(["-nostdin", "-loglevel", "error", "-threads", "1", "-ss"])
            .arg(format!("{at_seconds:.3}"))
            .arg("-i")
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-threads",
                "2",
                "-f",
                "image2pipe",
                "-c:v",
                "png",
                "-pix_fmt",
                "rgb24",
                "-",
            ])
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
    if !output.stdout.starts_with(&PNG_SIGNATURE) {
        anyhow::bail!("ffmpeg não devolveu um PNG (posição além do fim do vídeo?)");
    }
    Ok(output.stdout)
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

        let png = screenshot(&video, 1.0).await.expect("quadro");
        assert!(png.starts_with(&PNG_SIGNATURE));
        // Além do fim não sai quadro: erro claro, não PNG vazio.
        assert!(screenshot(&video, 600.0).await.is_err());

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

        let png = screenshot(&video, 0.0).await.expect("quadro");
        // IHDR: 8 bytes de assinatura, 8 de cabeçalho do chunk, 4 + 4 de
        // largura e altura; depois a profundidade (byte 24) e o tipo de cor.
        assert_eq!(png[24], 8, "profundidade de cor do PNG");
        assert_eq!(png[25], 2, "RGB sem alfa");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

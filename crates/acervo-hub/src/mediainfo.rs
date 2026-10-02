//! Os idiomas de áudio de um arquivo de vídeo, lidos pelo `ffprobe`: valem
//! mais que o nome do release para dizer em que idiomas o arquivo está.
//!
//! Sem `ffprobe` no caminho (ou se ele falha), os idiomas vêm do nome: é
//! informação a mais, não condição para importar.

use std::path::Path;
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
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new(binary())
            .args(["-v", "quiet", "-print_format", "json", "-show_streams"])
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
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
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
}

//! As faixas de um arquivo de vídeo, lidas pelo `ffprobe`, no formato
//! `mediaInfo` da API v3 — o que o app de legendas lê para saber que áudio e
//! que legendas o arquivo já tem.
//!
//! Sem `ffprobe` no caminho (ou se ele falha), o arquivo fica sem faixas: é
//! informação a mais, não condição para importar.

use std::path::Path;
use std::time::Duration;

use acervo_parser::Language;
use serde_json::{Value, json};

/// O que se leu do arquivo.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    /// No formato da API v3.
    pub media_info: Value,
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
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                "-show_format",
            ])
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

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn video_codec(stream: &Value) -> &'static str {
    match stream["codec_name"].as_str().unwrap_or_default() {
        "h264" => "h264",
        "hevc" => "h265",
        "av1" => "AV1",
        "vp9" => "VP9",
        "vp8" => "VP8",
        "mpeg2video" => "MPEG2",
        "mpeg4" => "XviD",
        "vc1" => "VC1",
        _ => "",
    }
}

fn audio_codec(stream: &Value) -> String {
    let profile = stream["profile"].as_str().unwrap_or_default();
    match stream["codec_name"].as_str().unwrap_or_default() {
        "aac" => "AAC".into(),
        "ac3" => "AC3".into(),
        "eac3" if profile.contains("Atmos") => "EAC3 Atmos".into(),
        "eac3" => "EAC3".into(),
        "dts" if profile.contains("MA") => "DTS-HD MA".into(),
        "dts" if profile.contains("HRA") || profile.contains("HD") => "DTS-HD HRA".into(),
        "dts" if profile.contains('X') => "DTS-X".into(),
        "dts" => "DTS".into(),
        "truehd" if profile.contains("Atmos") => "TrueHD Atmos".into(),
        "truehd" => "TrueHD".into(),
        "flac" => "FLAC".into(),
        "opus" => "Opus".into(),
        "mp3" => "MP3".into(),
        "mp2" => "MP2".into(),
        "vorbis" => "Vorbis".into(),
        codec if codec.starts_with("pcm") => "PCM".into(),
        other => other.to_uppercase(),
    }
}

/// 6 canais são 5.1; 8, 7.1.
fn channels(stream: &Value) -> f64 {
    match stream["channels"].as_u64().unwrap_or(0) {
        6 => 5.1,
        7 => 6.1,
        8 => 7.1,
        #[allow(clippy::cast_precision_loss)]
        other => other as f64,
    }
}

fn dynamic_range(stream: &Value) -> (&'static str, &'static str) {
    let side = stream["side_data_list"].as_array().into_iter().flatten();
    let mut dolby = false;
    let mut plus = false;
    for data in side {
        let kind = data["side_data_type"].as_str().unwrap_or_default();
        dolby |= kind.contains("DOVI");
        plus |= kind.contains("2094-40");
    }
    let transfer = stream["color_transfer"].as_str().unwrap_or_default();
    match (dolby, plus, transfer) {
        (true, _, "smpte2084") => ("HDR", "DV HDR10"),
        (true, _, _) => ("HDR", "DV"),
        (false, true, _) => ("HDR", "HDR10Plus"),
        (false, false, "smpte2084") => ("HDR", "HDR10"),
        (false, false, "arib-std-b67") => ("HDR", "HLG"),
        _ => ("", ""),
    }
}

fn run_time(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

fn fps(stream: &Value) -> f64 {
    let rate = stream["avg_frame_rate"]
        .as_str()
        .filter(|r| *r != "0/0")
        .or_else(|| stream["r_frame_rate"].as_str())
        .unwrap_or("0/1");
    let (num, den) = rate.split_once('/').unwrap_or((rate, "1"));
    let (num, den): (f64, f64) = (num.parse().unwrap_or(0.0), den.parse().unwrap_or(1.0));
    if den == 0.0 {
        return 0.0;
    }
    (num / den * 1000.0).round() / 1000.0
}

/// Converte a saída do `ffprobe` (`-show_streams -show_format`).
#[must_use]
pub fn from_ffprobe(value: &Value) -> Probe {
    let video = streams(value, "video").next();
    let audio: Vec<&Value> = streams(value, "audio").collect();
    let subtitles: Vec<&str> = streams(value, "subtitle").filter_map(language).collect();
    let audio_codes: Vec<&str> = audio.iter().filter_map(|s| language(s)).collect();
    let first_audio = audio.first();
    let (range, range_type) = video.map_or(("", ""), dynamic_range);
    let bit_depth = video
        .and_then(|v| {
            number(&v["bits_per_raw_sample"]).or_else(|| {
                v["pix_fmt"]
                    .as_str()
                    .map(|f| if f.contains("10") { 10.0 } else { 8.0 })
            })
        })
        .unwrap_or(0.0);
    let duration = number(&value["format"]["duration"])
        .or_else(|| video.and_then(|v| number(&v["duration"])))
        .unwrap_or(0.0);
    let mut audio_languages: Vec<Language> = Vec::new();
    for code in &audio_codes {
        let language = Language::from_iso639_2(code);
        if language != Language::Unknown && !audio_languages.contains(&language) {
            audio_languages.push(language);
        }
    }
    let interlaced = video
        .and_then(|v| v["field_order"].as_str())
        .is_some_and(|f| !matches!(f, "progressive" | "unknown"));
    Probe {
        media_info: json!({
            "audioBitrate": first_audio.and_then(|a| number(&a["bit_rate"])).unwrap_or(0.0),
            "audioChannels": first_audio.map_or(0.0, |a| channels(a)),
            "audioCodec": first_audio.map(|a| audio_codec(a)).unwrap_or_default(),
            "audioLanguages": audio_codes.join("/"),
            "audioStreamCount": audio.len(),
            "videoBitDepth": bit_depth,
            "videoBitrate": video.and_then(|v| number(&v["bit_rate"])).unwrap_or(0.0),
            "videoCodec": video.map(video_codec).unwrap_or_default(),
            "videoDynamicRange": range,
            "videoDynamicRangeType": range_type,
            "videoFps": video.map_or(0.0, fps),
            "resolution": video
                .map(|v| format!("{}x{}", v["width"].as_u64().unwrap_or(0), v["height"].as_u64().unwrap_or(0)))
                .unwrap_or_default(),
            "runTime": run_time(duration),
            "scanType": if interlaced { "Interlaced" } else { "Progressive" },
            "subtitles": subtitles.join("/"),
        }),
        audio_languages,
    }
}

#[cfg(test)]
mod tests {
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
            ],
            "format": {"duration": "6428.4"}
        }));
        let info = &probe.media_info;
        assert_eq!(info["videoCodec"], "h265");
        assert_eq!(info["resolution"], "3840x1606");
        assert_eq!(info["videoBitDepth"], 10.0);
        assert_eq!(info["videoFps"], 23.976);
        assert_eq!(info["videoDynamicRangeType"], "HDR10");
        assert_eq!(info["audioCodec"], "AC3");
        assert_eq!(info["audioChannels"], 5.1);
        assert_eq!(info["audioLanguages"], "por/eng");
        assert_eq!(info["audioStreamCount"], 2);
        assert_eq!(info["subtitles"], "por");
        assert_eq!(info["runTime"], "1:47:08");
        assert_eq!(
            probe.audio_languages,
            [Language::Portuguese, Language::English]
        );
    }
}

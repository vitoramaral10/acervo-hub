//! Os textos do envio (`Fields`): título, ficha, dados técnicos e a descrição
//! em `BBCode`, como funções puras.
//!
//! [`build_fields`] recebe o que o preparo já levantou (TMDB em pt-BR, sonda
//! do vídeo principal, arquivos do torrent, URLs das capturas) e devolve os
//! campos que o usuário copia para o formulário do site, mais os avisos para
//! o `Report`. Nada aqui lê disco, rede ou banco. O release nunca é alterado:
//! `release` repete o nome da pasta raiz como veio.
//!
//! A descrição junta sinopse, ficha, técnico e capturas e não cita site algum
//! (Amigos 1.16): nem os links do `IMDb` e do `TMDB`, que ficam só nos campos
//! próprios, nem o nome do release, que pode trazer o do grupo ou de um site.

use std::fmt::Write as _;

use acervo_parser::{Language, Modifier, Source, parse_episode_quality, parse_quality};
use serde::{Deserialize, Serialize};

use super::rules::{Check, Kind, Site, SourceFile, Status};
use crate::mediainfo::{AudioFacts, Hdr, MediaFacts, SubtitleFacts};

/// Quantos nomes do elenco entram na ficha.
const MAX_CAST: usize = 8;

const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "ass", "ssa", "sub", "idx"];

/// O que o torrent é, com a numeração que o título e a sinopse pedem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Movie,
    Episode { season: u16, episode: u16 },
    Season { season: u16 },
}

impl Work {
    /// O mesmo tipo das regras (que não carrega a numeração).
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self {
            Self::Movie => Kind::Movie,
            Self::Episode { .. } => Kind::Episode,
            Self::Season { .. } => Kind::Season,
        }
    }
}

/// A ficha da obra, em português (TMDB pt-BR). Listas vazias saem da ficha.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credits {
    /// Direção do filme ou criação da série.
    pub directors: Vec<String>,
    pub cast: Vec<String>,
    pub genres: Vec<String>,
    /// Nomes dos países, já em português.
    pub countries: Vec<String>,
}

/// Tudo que o texto precisa saber do pacote.
#[derive(Debug, Clone)]
pub struct Input {
    /// Título em português; na série, o da série.
    pub title: String,
    pub original_title: Option<String>,
    /// Obra falada em português: não leva título original.
    pub national: bool,
    pub year: Option<u16>,
    pub imdb_id: Option<String>,
    pub tmdb_id: u32,
    pub work: Work,
    /// Sinopse pt-BR do TMDB: do filme, ou do episódio/temporada quando houver,
    /// senão a da série. `None` (ou vazia) gera o aviso e deixa o campo vazio.
    pub synopsis: Option<String>,
    pub credits: Credits,
    /// Nome da pasta raiz do torrent (ou do arquivo, se for avulso).
    pub release_name: String,
    /// Arquivos do torrent: delas saem as legendas externas.
    pub files: Vec<SourceFile>,
    /// Tamanho total do torrent, em bytes.
    pub size: u64,
    /// Sonda do vídeo principal.
    pub media: MediaFacts,
    /// URLs das capturas, na ordem.
    pub screens: Vec<String>,
}

/// Os textos prontos para copiar; é o JSON de `launches.fields`. Todos são
/// texto: vazio quando o dado não existe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fields {
    #[serde(rename = "titulo")]
    pub title: String,
    pub release: String,
    #[serde(rename = "ano")]
    pub year: String,
    pub imdb: String,
    pub tmdb: String,
    #[serde(rename = "duracao")]
    pub duration: String,
    #[serde(rename = "tamanho")]
    pub size: String,
    #[serde(rename = "sinopse")]
    pub synopsis: String,
    #[serde(rename = "ficha")]
    pub sheet: String,
    #[serde(rename = "tecnico")]
    pub technical: String,
    #[serde(rename = "descricao")]
    pub description: String,
    /// Relatório do `mediainfo --Output=TEXT` do vídeo principal; vazio no BJ
    /// se o `MediaInfo` falhou. Preenchido pelo passo de texto do job.
    #[serde(rename = "mediainfo", default)]
    pub mediainfo: String,
}

/// Os campos e os avisos que entram no `Report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub fields: Fields,
    pub warnings: Vec<Check>,
}

/// Monta os campos do envio para o `site`.
#[must_use]
pub fn build_fields(site: Site, input: &Input) -> Built {
    let synopsis = input
        .synopsis
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    let mut warnings = Vec::new();
    if synopsis.is_empty() {
        warnings.push(missing_synopsis(site));
    }
    let duration = input
        .media
        .duration_seconds
        .map(format_duration)
        .unwrap_or_default();
    let sheet = sheet(input);
    let technical = technical(input, &duration);
    let description = description(input, &synopsis, &sheet, &technical);
    let fields = Fields {
        title: title(input),
        release: input.release_name.clone(),
        year: input.year.map(|y| y.to_string()).unwrap_or_default(),
        imdb: imdb_url(input.imdb_id.as_deref()),
        tmdb: tmdb_url(input.work, input.tmdb_id),
        duration,
        size: format_size(input.size),
        synopsis,
        sheet,
        technical,
        description,
        mediainfo: String::new(),
    };
    Built { fields, warnings }
}

fn missing_synopsis(site: Site) -> Check {
    let (rule, message) = match site {
        Site::Bj => (
            "4.1.6",
            "sinopse em PT não encontrada: escreva à mão; tradutor automático não é aceito no BJ",
        ),
        Site::Amigos => (
            "5.4.2",
            "sinopse em PT não encontrada: escreva à mão, ela é obrigatória na descrição",
        ),
    };
    Check {
        rule: rule.to_owned(),
        status: Status::Warn,
        message: format!("{message} ({rule})"),
    }
}

/// "Título em português (Título original)"; sem o original se for igual ou se
/// a obra é nacional; sem ano (Amigos 4.5).
fn title(input: &Input) -> String {
    let mut title = input.title.trim().to_owned();
    let original = input
        .original_title
        .as_deref()
        .map(str::trim)
        .filter(|o| !o.is_empty() && !o.eq_ignore_ascii_case(&title));
    if let Some(original) = original
        && !input.national
    {
        let _ = write!(title, " ({original})");
    }
    match input.work {
        Work::Movie => {}
        Work::Episode { season, episode } => {
            let _ = write!(title, " - S{season:02}E{episode:02}");
        }
        Work::Season { season } => {
            let _ = write!(title, " - S{season:02}");
        }
    }
    title
}

fn imdb_url(id: Option<&str>) -> String {
    id.map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| format!("https://www.imdb.com/title/{id}/"))
        .unwrap_or_default()
}

fn tmdb_url(work: Work, id: u32) -> String {
    if id == 0 {
        return String::new();
    }
    let media = if work == Work::Movie { "movie" } else { "tv" };
    format!("https://www.themoviedb.org/{media}/{id}")
}

/// "1 h 52 min", "2 h", "45 min"; arredonda para o minuto mais próximo.
fn format_duration(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let minutes = (seconds / 60.0).round().max(0.0) as u64;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// "4,37 GiB", "712,5 MiB": vírgula decimal, como o resto da tela.
#[allow(clippy::cast_precision_loss)]
fn format_size(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes == 0 {
        return String::new();
    }
    let mib = bytes as f64 / MIB;
    let text = if mib >= 1024.0 {
        format!("{:.2} GiB", mib / 1024.0)
    } else {
        format!("{mib:.1} MiB")
    };
    text.replace('.', ",")
}

fn sheet(input: &Input) -> String {
    let credits = &input.credits;
    let mut lines: Vec<String> = Vec::new();
    let role = if input.work == Work::Movie {
        "Direção"
    } else {
        "Criação"
    };
    if !credits.directors.is_empty() {
        lines.push(format!("{role}: {}", credits.directors.join(", ")));
    }
    if !credits.cast.is_empty() {
        let cast: Vec<&str> = credits
            .cast
            .iter()
            .take(MAX_CAST)
            .map(String::as_str)
            .collect();
        lines.push(format!("Elenco: {}", cast.join(", ")));
    }
    if !credits.genres.is_empty() {
        lines.push(format!("Gêneros: {}", credits.genres.join(", ")));
    }
    if !credits.countries.is_empty() {
        let label = if credits.countries.len() == 1 {
            "País"
        } else {
            "Países"
        };
        lines.push(format!("{label}: {}", credits.countries.join(", ")));
    }
    // O original entra sempre na ficha (Amigos 5.4.2), mesmo igual ao título.
    let original = input
        .original_title
        .as_deref()
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .unwrap_or(input.title.trim());
    lines.push(format!("Título original: {original}"));
    if let Some(year) = input.year {
        lines.push(format!("Ano: {year}"));
    }
    lines.join("\n")
}

fn technical(input: &Input, duration: &str) -> String {
    let media = &input.media;
    let mut lines: Vec<String> = Vec::new();
    if let Some(container) = container_label(&media.container) {
        lines.push(format!("Formato: {container}"));
    }
    if let Some(video) = &media.video {
        if video.width > 0 && video.height > 0 {
            lines.push(format!("Resolução: {}x{}", video.width, video.height));
        }
        if let Some(quality) = quality_label(input) {
            lines.push(format!("Qualidade: {quality}"));
        }
        let mut parts = vec![video_codec_label(&video.codec)];
        if video.hdr != Hdr::Sdr {
            parts.push(hdr_label(video.hdr).to_owned());
        }
        if let Some(bits) = video.bit_depth {
            parts.push(format!("{bits} bits"));
        }
        lines.push(format!("Vídeo: {}", parts.join(", ")));
    } else if let Some(quality) = quality_label(input) {
        lines.push(format!("Qualidade: {quality}"));
    }
    if media.audio.is_empty() {
        lines.push("Áudio: sem faixa de áudio".to_owned());
    }
    for (index, track) in media.audio.iter().enumerate() {
        lines.push(format!("Áudio {}: {}", index + 1, audio_line(track)));
    }
    lines.push(format!("Legendas: {}", subtitles_line(media, &input.files)));
    if !duration.is_empty() {
        lines.push(format!("Duração: {duration}"));
    }
    lines.join("\n")
}

/// Fonte do release pelo nome (o arquivo não diz se é WEB-DL ou Blu-ray).
fn quality_label(input: &Input) -> Option<String> {
    let quality = if input.work == Work::Movie {
        parse_quality(&input.release_name).quality
    } else {
        parse_episode_quality(&input.release_name).quality
    };
    let label = match quality.source() {
        Source::Unknown => return None,
        Source::Cam => "CAM",
        Source::Telesync => "TS",
        Source::Telecine => "TC",
        Source::Workprint => "Workprint",
        Source::Dvd => "DVD",
        Source::Tv => "HDTV",
        Source::WebDl => "WEB-DL",
        Source::WebRip => "WEBRip",
        Source::Bluray => "Blu-ray",
    };
    Some(if quality.modifier() == Modifier::Remux {
        format!("{label} Remux")
    } else {
        label.to_owned()
    })
}

/// `format_name` do ffprobe vira a sigla que o site pede.
fn container_label(format_name: &str) -> Option<String> {
    let first = format_name.split(',').next()?.trim();
    Some(
        match first {
            "" => return None,
            "matroska" => "MKV",
            "mov" => "MP4",
            "avi" => "AVI",
            "mpegts" => "MPEG-TS",
            other => return Some(other.to_ascii_uppercase()),
        }
        .to_owned(),
    )
}

fn video_codec_label(codec: &str) -> String {
    match codec {
        "h264" => "H.264".to_owned(),
        "hevc" => "H.265 (HEVC)".to_owned(),
        "av1" => "AV1".to_owned(),
        "mpeg4" => "MPEG-4".to_owned(),
        "vc1" => "VC-1".to_owned(),
        "" => "codec desconhecido".to_owned(),
        other => other.to_ascii_uppercase(),
    }
}

fn audio_codec_label(codec: &str) -> String {
    match codec {
        "ac3" => "AC3".to_owned(),
        "eac3" => "E-AC3".to_owned(),
        "truehd" => "TrueHD".to_owned(),
        "opus" => "Opus".to_owned(),
        "" => "codec desconhecido".to_owned(),
        other => other.to_ascii_uppercase(),
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

fn channels_label(channels: u32) -> String {
    match channels {
        1 => "1.0".to_owned(),
        2 => "2.0".to_owned(),
        3 => "2.1".to_owned(),
        6 => "5.1".to_owned(),
        7 => "6.1".to_owned(),
        8 => "7.1".to_owned(),
        other => format!("{other} canais"),
    }
}

/// "Português, AC3, 5.1".
fn audio_line(track: &AudioFacts) -> String {
    let mut parts = vec![
        language_label(track.language, track.raw_language.as_deref()),
        audio_codec_label(&track.codec),
    ];
    if let Some(channels) = track.channels {
        parts.push(channels_label(channels));
    }
    parts.join(", ")
}

/// "Português (embutida), Inglês (embutida, forçada), Português (arquivo)".
fn subtitles_line(media: &MediaFacts, files: &[SourceFile]) -> String {
    let mut entries: Vec<String> = Vec::new();
    let mut add = |entry: String| {
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    };
    for SubtitleFacts {
        language,
        raw_language,
        forced,
        ..
    } in &media.subtitles
    {
        let language = language_label(*language, raw_language.as_deref());
        add(if *forced {
            format!("{language} (embutida, forçada)")
        } else {
            format!("{language} (embutida)")
        });
    }
    for file in files.iter().filter(|f| is_subtitle_file(&f.path)) {
        let language = subtitle_file_language(&file.path)
            .map_or_else(|| "idioma não identificado".to_owned(), str::to_owned);
        add(format!("{language} (arquivo)"));
    }
    if entries.is_empty() {
        "sem legendas".to_owned()
    } else {
        entries.join(", ")
    }
}

fn language_label(language: Language, raw: Option<&str>) -> String {
    match language {
        Language::Portuguese | Language::PortugueseBr => "Português".to_owned(),
        Language::English => "Inglês".to_owned(),
        Language::Spanish | Language::SpanishLatino => "Espanhol".to_owned(),
        Language::French => "Francês".to_owned(),
        Language::German => "Alemão".to_owned(),
        Language::Italian => "Italiano".to_owned(),
        Language::Japanese => "Japonês".to_owned(),
        Language::Korean => "Coreano".to_owned(),
        Language::Chinese => "Chinês".to_owned(),
        Language::Russian => "Russo".to_owned(),
        Language::Unknown => raw.map_or_else(
            || "idioma não informado".to_owned(),
            str::to_ascii_uppercase,
        ),
        other => other.name().to_owned(),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_subtitle_file(path: &str) -> bool {
    file_name(path)
        .rsplit_once('.')
        .is_some_and(|(_, ext)| SUBTITLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Idioma da legenda em arquivo, pelo nome: `Filme.pt-BR.srt`, `Filme.eng.srt`.
///
/// Só os dois últimos tokens antes da extensão contam, de trás para a frente
/// (`.pt-BR`, `.por.forced`): o resto é o nome do release, onde `de`, `it` ou
/// `en` são palavras do título (`Cidade.de.Deus`, `It.Follows`), não idioma.
fn subtitle_file_language(path: &str) -> Option<&'static str> {
    let name = file_name(path).to_lowercase();
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    let tokens: Vec<&str> = stem
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    // O primeiro token é o início do título, nunca o sufixo de idioma.
    let start = tokens.len().saturating_sub(2).max(1);
    let tail = tokens.get(start..).unwrap_or_default();
    tail.iter().rev().find_map(|token| {
        Some(match *token {
            "pt" | "por" | "pob" | "ptbr" | "portuguese" | "portugues" | "português"
            | "brazilian" | "brasil" => "Português",
            "en" | "eng" | "english" => "Inglês",
            "es" | "spa" | "spanish" | "espanhol" => "Espanhol",
            "fr" | "fre" | "fra" | "french" => "Francês",
            "de" | "ger" | "deu" | "german" => "Alemão",
            "it" | "ita" | "italian" => "Italiano",
            _ => return None,
        })
    })
}

/// Texto de fora (TMDB) entra na descrição sem poder abrir tag `BBCode`.
fn bbcode_safe(text: &str) -> String {
    text.replace('[', "(").replace(']', ")")
}

fn description(input: &Input, synopsis: &str, sheet: &str, technical: &str) -> String {
    let mut sections: Vec<String> = vec![format!("[b]{}[/b]", bbcode_safe(&title(input)))];
    if !synopsis.is_empty() {
        sections.push(format!("[b]Sinopse[/b]\n{}", bbcode_safe(synopsis)));
    }
    sections.push(format!("[b]Ficha[/b]\n{}", bbcode_safe(sheet)));
    sections.push(format!(
        "[b]Informações técnicas[/b]\n{}",
        bbcode_safe(technical)
    ));
    let screens: Vec<String> = input
        .screens
        .iter()
        .filter(|url| !url.trim().is_empty())
        .map(|url| format!("[img]{}[/img]", url.trim()))
        .collect();
    if !screens.is_empty() {
        sections.push(format!("[b]Capturas[/b]\n{}", screens.join("\n")));
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mediainfo::VideoFacts;

    fn video(hdr: Hdr) -> VideoFacts {
        VideoFacts {
            codec: "hevc".into(),
            width: 1920,
            height: 1080,
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: Some(10),
            hdr,
        }
    }

    fn audio(language: Language, raw: &str, codec: &str, channels: u32) -> AudioFacts {
        AudioFacts {
            language,
            raw_language: Some(raw.into()),
            codec: codec.into(),
            channels: Some(channels),
            title: None,
        }
    }

    fn subtitle(language: Language, raw: &str, forced: bool) -> SubtitleFacts {
        SubtitleFacts {
            index: 0,
            kind: crate::mediainfo::SubtitleKind::Text,
            language,
            raw_language: Some(raw.into()),
            codec: "subrip".into(),
            forced,
            title: None,
        }
    }

    fn media() -> MediaFacts {
        MediaFacts {
            container: "matroska,webm".into(),
            duration_seconds: Some(6720.0),
            video: Some(video(Hdr::Sdr)),
            audio: vec![audio(Language::Portuguese, "por", "ac3", 6)],
            subtitles: Vec::new(),
        }
    }

    fn input() -> Input {
        Input {
            title: "Um Sonho de Liberdade".into(),
            original_title: Some("The Shawshank Redemption".into()),
            national: false,
            year: Some(1994),
            imdb_id: Some("tt0111161".into()),
            tmdb_id: 278,
            work: Work::Movie,
            synopsis: Some("Dois homens presos criam uma amizade.".into()),
            credits: Credits {
                directors: vec!["Frank Darabont".into()],
                cast: (1..=10).map(|n| format!("Ator {n}")).collect(),
                genres: vec!["Drama".into(), "Crime".into()],
                countries: vec!["Estados Unidos".into()],
            },
            release_name: "Um.Sonho.de.Liberdade.1994.1080p.WEB-DL.DUAL-GRUPO".into(),
            files: vec![SourceFile {
                path: "Um.Sonho.de.Liberdade.1994.1080p.WEB-DL.DUAL-GRUPO/filme.mkv".into(),
                size: 4_700_000_000,
            }],
            size: 4_700_000_000,
            media: media(),
            screens: vec![
                "https://i.ibb.co/a/1.png".into(),
                "https://i.ibb.co/b/2.png".into(),
            ],
        }
    }

    fn build(input: &Input) -> Fields {
        build_fields(Site::Bj, input).fields
    }

    #[test]
    fn titulo_com_original_diferente() {
        assert_eq!(
            build(&input()).title,
            "Um Sonho de Liberdade (The Shawshank Redemption)"
        );
    }

    #[test]
    fn titulo_sem_original_quando_igual_ou_ausente() {
        let mut igual = input();
        igual.title = "Inception".into();
        igual.original_title = Some("inception".into());
        assert_eq!(build(&igual).title, "Inception");
        let mut ausente = input();
        ausente.original_title = None;
        assert_eq!(build(&ausente).title, "Um Sonho de Liberdade");
    }

    #[test]
    fn titulo_de_obra_nacional_nao_leva_original() {
        let mut nacional = input();
        nacional.title = "Cidade de Deus".into();
        nacional.original_title = Some("Cidade de Deus: Edição".into());
        nacional.national = true;
        let fields = build(&nacional);
        assert_eq!(fields.title, "Cidade de Deus");
        // A ficha ainda traz o original (Amigos 5.4.2).
        assert!(
            fields
                .sheet
                .contains("Título original: Cidade de Deus: Edição")
        );
    }

    #[test]
    fn titulo_nunca_leva_ano() {
        let fields = build(&input());
        assert!(!fields.title.contains("1994"));
        assert_eq!(fields.year, "1994");
    }

    #[test]
    fn titulo_de_episodio_leva_temporada_e_episodio() {
        let mut episodio = input();
        episodio.work = Work::Episode {
            season: 1,
            episode: 2,
        };
        episodio.release_name = "Serie.S01E02.1080p.WEB-DL".into();
        let fields = build(&episodio);
        assert_eq!(
            fields.title,
            "Um Sonho de Liberdade (The Shawshank Redemption) - S01E02"
        );
        assert_eq!(fields.tmdb, "https://www.themoviedb.org/tv/278");
        assert!(fields.sheet.starts_with("Criação: Frank Darabont"));
    }

    #[test]
    fn titulo_de_temporada_leva_so_a_temporada() {
        let mut temporada = input();
        temporada.work = Work::Season { season: 12 };
        assert!(build(&temporada).title.ends_with(" - S12"));
        assert_eq!(temporada.work.kind(), Kind::Season);
    }

    #[test]
    fn links_do_imdb_e_do_tmdb_de_filme() {
        let fields = build(&input());
        assert_eq!(fields.imdb, "https://www.imdb.com/title/tt0111161/");
        assert_eq!(fields.tmdb, "https://www.themoviedb.org/movie/278");
        assert_eq!(fields.release, input().release_name);
        let mut sem_ids = input();
        sem_ids.imdb_id = None;
        sem_ids.tmdb_id = 0;
        let fields = build(&sem_ids);
        assert!(fields.imdb.is_empty() && fields.tmdb.is_empty());
    }

    #[test]
    fn duracao_formatada_em_horas_e_minutos() {
        assert_eq!(format_duration(6720.0), "1 h 52 min");
        assert_eq!(format_duration(7200.0), "2 h");
        assert_eq!(format_duration(2700.4), "45 min");
        assert_eq!(format_duration(3599.0), "1 h");
        assert_eq!(build(&input()).duration, "1 h 52 min");
        let mut sem_duracao = input();
        sem_duracao.media.duration_seconds = None;
        let fields = build(&sem_duracao);
        assert!(fields.duration.is_empty());
        assert!(!fields.technical.contains("Duração"));
    }

    #[test]
    fn tamanho_com_virgula_decimal() {
        assert_eq!(format_size(4_700_000_000), "4,38 GiB");
        assert_eq!(format_size(747_110_400), "712,5 MiB");
        assert_eq!(format_size(0), "");
    }

    #[test]
    fn ficha_limita_o_elenco_e_ordena_os_campos() {
        let sheet = build(&input()).sheet;
        let lines: Vec<&str> = sheet.lines().collect();
        assert_eq!(lines[0], "Direção: Frank Darabont");
        assert_eq!(
            lines[1],
            "Elenco: Ator 1, Ator 2, Ator 3, Ator 4, Ator 5, Ator 6, Ator 7, Ator 8"
        );
        assert_eq!(lines[2], "Gêneros: Drama, Crime");
        assert_eq!(lines[3], "País: Estados Unidos");
        assert_eq!(lines[4], "Título original: The Shawshank Redemption");
        assert_eq!(lines[5], "Ano: 1994");
    }

    #[test]
    fn tecnico_basico_de_sdr() {
        let tecnico = build(&input()).technical;
        assert!(tecnico.contains("Formato: MKV"));
        assert!(tecnico.contains("Resolução: 1920x1080"));
        assert!(tecnico.contains("Qualidade: WEB-DL"));
        assert!(tecnico.contains("Vídeo: H.265 (HEVC), 10 bits"));
        assert!(!tecnico.contains("SDR"));
        assert!(tecnico.contains("Áudio 1: Português, AC3, 5.1"));
        assert!(tecnico.contains("Legendas: sem legendas"));
        assert!(tecnico.contains("Duração: 1 h 52 min"));
    }

    #[test]
    fn tecnico_com_hdr_dolby_vision_e_varias_faixas() {
        let mut uhd = input();
        uhd.release_name = "Filme.2160p.BluRay.REMUX-GRUPO".into();
        uhd.media.video = Some(VideoFacts {
            width: 3840,
            height: 2160,
            ..video(Hdr::DolbyVision)
        });
        uhd.media.audio = vec![
            audio(Language::Portuguese, "por", "eac3", 6),
            audio(Language::English, "eng", "truehd", 8),
            audio(Language::Unknown, "xyz", "dts", 2),
        ];
        uhd.media.subtitles = vec![
            subtitle(Language::Portuguese, "por", false),
            subtitle(Language::English, "eng", true),
            subtitle(Language::Portuguese, "por", false),
        ];
        uhd.files.push(SourceFile {
            path: "Pasta/Legendas/Filme.pt-BR.srt".into(),
            size: 10,
        });
        uhd.files.push(SourceFile {
            path: "Pasta/Filme.srt".into(),
            size: 10,
        });
        let tecnico = build(&uhd).technical;
        assert!(tecnico.contains("Resolução: 3840x2160"));
        assert!(tecnico.contains("Qualidade: Blu-ray Remux"));
        assert!(tecnico.contains("Vídeo: H.265 (HEVC), Dolby Vision, 10 bits"));
        assert!(tecnico.contains("Áudio 1: Português, E-AC3, 5.1"));
        assert!(tecnico.contains("Áudio 2: Inglês, TrueHD, 7.1"));
        assert!(tecnico.contains("Áudio 3: XYZ, DTS, 2.0"));
        assert!(tecnico.contains(
            "Legendas: Português (embutida), Inglês (embutida, forçada), \
             Português (arquivo), idioma não identificado (arquivo)"
        ));
    }

    #[test]
    fn idioma_da_legenda_em_arquivo_vem_do_sufixo_e_nao_do_titulo() {
        for (path, esperado) in [
            (
                "P/Cidade.de.Deus.2002.1080p.BluRay.pt-BR.srt",
                Some("Português"),
            ),
            (
                "P/It.Follows.2014.1080p.WEB-DL.pt-BR.srt",
                Some("Português"),
            ),
            ("P/Filme.2024.por.forced.srt", Some("Português")),
            ("P/Movie.2024.eng.srt", Some("Inglês")),
            ("P/Cidade.de.Deus.2002.1080p.BluRay.srt", None),
            ("P/It.Follows.srt", None),
        ] {
            assert_eq!(subtitle_file_language(path), esperado, "{path}");
        }
    }

    #[test]
    fn tecnico_com_hdr10_e_hlg() {
        for (hdr, nome) in [(Hdr::Hdr10, "HDR10"), (Hdr::Hlg, "HLG")] {
            let mut entrada = input();
            entrada.media.video = Some(video(hdr));
            assert!(
                build(&entrada)
                    .technical
                    .contains(&format!("Vídeo: H.265 (HEVC), {nome}, 10 bits"))
            );
        }
    }

    #[test]
    fn sinopse_ausente_gera_aviso_por_site() {
        let mut sem = input();
        sem.synopsis = Some("   ".into());
        let bj = build_fields(Site::Bj, &sem);
        assert!(bj.fields.synopsis.is_empty());
        assert!(!bj.fields.description.contains("Sinopse"));
        assert_eq!(bj.warnings.len(), 1);
        assert_eq!(bj.warnings[0].rule, "4.1.6");
        assert_eq!(bj.warnings[0].status, Status::Warn);
        assert!(bj.warnings[0].message.contains("tradutor automático"));
        let amigos = build_fields(Site::Amigos, &sem);
        assert_eq!(amigos.warnings[0].rule, "5.4.2");
        assert!(build_fields(Site::Bj, &input()).warnings.is_empty());
    }

    #[test]
    fn descricao_junta_secoes_e_capturas() {
        let descricao = build(&input()).description;
        assert!(descricao.starts_with("[b]Um Sonho de Liberdade (The Shawshank Redemption)[/b]"));
        assert!(descricao.contains("[b]Sinopse[/b]\nDois homens presos criam uma amizade."));
        assert!(descricao.contains("[b]Ficha[/b]\nDireção: Frank Darabont"));
        assert!(descricao.contains("[b]Informações técnicas[/b]\nFormato: MKV"));
        assert!(descricao.contains("[b]Capturas[/b]\n[img]https://i.ibb.co/a/1.png[/img]\n[img]https://i.ibb.co/b/2.png[/img]"));
    }

    #[test]
    fn descricao_nao_cita_nenhum_site() {
        let mut entrada = input();
        entrada.release_name = "Filme.1994.1080p.WEB-DL-bj-share".into();
        let fields = build(&entrada);
        let descricao = fields.description.to_lowercase();
        // As capturas são as únicas URLs; o resto não nomeia site nenhum.
        let sem_imagens = descricao
            .split("[img]")
            .next()
            .unwrap_or_default()
            .to_owned();
        for palavra in [
            "bj-share",
            "bjshare",
            "bj share",
            "amigos",
            "share",
            "imdb",
            "themoviedb",
            "tmdb",
            "http",
            "www.",
            ".com",
            ".net",
            ".org",
        ] {
            assert!(
                !sem_imagens.contains(palavra),
                "descrição cita {palavra:?}: {sem_imagens}"
            );
        }
        // O release fica só no campo próprio, intacto.
        assert_eq!(fields.release, "Filme.1994.1080p.WEB-DL-bj-share");
    }

    #[test]
    fn texto_de_fora_nao_abre_tag_bbcode() {
        let mut entrada = input();
        entrada.synopsis = Some("Veja [img]http://x/y.png[/img] agora".into());
        let descricao = build(&entrada).description;
        assert!(!descricao.contains("[img]http://x"));
        assert!(descricao.contains("(img)http://x/y.png(/img)"));
    }

    #[test]
    fn campos_serializam_com_chaves_em_portugues() {
        let fields = build(&input());
        let json = serde_json::to_value(&fields).unwrap();
        let mut chaves: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        chaves.sort_unstable();
        assert_eq!(
            chaves,
            [
                "ano",
                "descricao",
                "duracao",
                "ficha",
                "imdb",
                "mediainfo",
                "release",
                "sinopse",
                "tamanho",
                "tecnico",
                "titulo",
                "tmdb"
            ]
        );
        assert_eq!(serde_json::from_value::<Fields>(json).unwrap(), fields);
    }
}

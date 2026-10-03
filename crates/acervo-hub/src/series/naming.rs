//! Nome do arquivo de um episódio, no formato do gerenciador anterior:
//! `[Season {N}/]{Título} - S{TT}E{EE}[-E{EE}…] - {Título do episódio} {Qualidade}.{ext}`.
//! Multi-episódio no estilo "prefixed range", títulos unidos por ` + `. As
//! trocas de caractere são as dos filmes.

use std::fmt::Write as _;

use acervo_parser::QualityModel;

use crate::naming::file_safe;

/// Título de episódio que a base ainda não anunciou, depois da espera.
pub const TBA: &str = "TBA";

/// Um nome de arquivo, como o sistema de arquivos aguenta: com folga para a
/// extensão e a pasta da temporada.
const MAX_STEM_BYTES: usize = 200;

/// `{Quality Full}`: o nome da qualidade, com `Proper`/`Repack` e `REAL`.
#[must_use]
pub fn quality_full(quality: QualityModel) -> String {
    let mut out = quality.quality.name().to_owned();
    if quality.revision.version > 1 {
        out.push_str(if quality.revision.is_repack {
            " Repack"
        } else {
            " Proper"
        });
    }
    if quality.revision.real > 0 {
        out.push_str(" REAL");
    }
    out
}

/// A pasta da temporada; a 0 é a dos especiais.
#[must_use]
pub fn season_folder(season: u16) -> String {
    if season == 0 {
        "Specials".into()
    } else {
        format!("Season {season}")
    }
}

/// `S01E01-E03`: o primeiro e o último de cada sequência.
fn code(season: u16, numbers: &[u16]) -> String {
    let mut sorted = numbers.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = format!("S{season:02}");
    for (i, (first, last)) in super::runs(&sorted).into_iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        let _ = write!(out, "E{first:02}");
        if last != first {
            let _ = write!(out, "-E{last:02}");
        }
    }
    out
}

/// "Parte (1)" e "Parte (2)" viram "Parte"; títulos iguais, um só; o resto
/// se une por ` + `.
fn joined_titles(titles: &[&str]) -> String {
    let base = |t: &str| -> String {
        let t = t.trim();
        match t.rsplit_once(" (") {
            Some((head, tail))
                if tail.ends_with(')')
                    && tail[..tail.len() - 1].bytes().all(|b| b.is_ascii_digit())
                    && tail.len() > 1 =>
            {
                head.to_owned()
            }
            _ => t.to_owned(),
        }
    };
    if titles.len() > 1 {
        let first = base(titles[0]);
        if titles.iter().all(|t| base(t) == first) {
            return first;
        }
    }
    let mut unique: Vec<&str> = Vec::new();
    for title in titles {
        let title = title.trim();
        if !unique.contains(&title) {
            unique.push(title);
        }
    }
    unique.join(" + ")
}

/// Corta no limite de bytes, sem partir caractere.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// O caminho do arquivo, relativo à pasta da série. `episodes` são
/// `(temporada, número, título)`, de uma temporada só; título ausente já
/// vem como [`TBA`].
#[must_use]
pub fn episode_path(
    series_title: &str,
    season_folder_on: bool,
    episodes: &[(u16, u16, &str)],
    quality: QualityModel,
    extension: &str,
) -> String {
    let season = episodes.first().map_or(1, |e| e.0);
    let numbers: Vec<u16> = episodes.iter().map(|e| e.1).collect();
    let mut ordered: Vec<&(u16, u16, &str)> = episodes.iter().collect();
    ordered.sort_by_key(|e| e.1);
    let titles: Vec<&str> = ordered.iter().map(|e| e.2).collect();
    let head = format!(
        "{} - {} - ",
        file_safe(series_title),
        code(season, &numbers)
    );
    let tail = format!(" {}", quality_full(quality));
    let room = MAX_STEM_BYTES.saturating_sub(head.len() + tail.len());
    let title = file_safe(&joined_titles(&titles));
    let title = truncate(&title, room).trim_end();
    let stem = format!("{head}{title}{tail}");
    let name = format!("{stem}.{}", extension.to_ascii_lowercase());
    if season_folder_on {
        format!("{}/{name}", season_folder(season))
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use acervo_parser::{Quality, Revision};

    use super::*;

    fn quality(quality: Quality) -> QualityModel {
        QualityModel {
            quality,
            revision: Revision::default(),
        }
    }

    #[test]
    fn episodio_simples_com_pasta_da_temporada() {
        assert_eq!(
            episode_path(
                "The Show",
                true,
                &[(1, 2, "Pilot")],
                quality(Quality::WebDl1080p),
                "MKV"
            ),
            "Season 1/The Show - S01E02 - Pilot WEBDL-1080p.mkv"
        );
    }

    #[test]
    fn multi_episodio_em_faixa_prefixada() {
        assert_eq!(
            episode_path(
                "The Show",
                true,
                &[(2, 2, "Two"), (2, 1, "One"), (2, 3, "Three")],
                quality(Quality::Hdtv720p),
                "mkv"
            ),
            "Season 2/The Show - S02E01-E03 - One + Two + Three HDTV-720p.mkv"
        );
        // Partes do mesmo título viram uma.
        assert_eq!(
            episode_path(
                "The Show",
                true,
                &[(1, 1, "Finale (1)"), (1, 2, "Finale (2)")],
                quality(Quality::WebDl1080p),
                "mkv"
            ),
            "Season 1/The Show - S01E01-E02 - Finale WEBDL-1080p.mkv"
        );
    }

    #[test]
    fn sem_pasta_de_temporada_e_com_troca_de_caracteres() {
        let proper = QualityModel {
            quality: Quality::Bluray1080p,
            revision: Revision {
                version: 2,
                real: 0,
                is_repack: false,
            },
        };
        assert_eq!(
            episode_path(
                "Marvel's Agents: S.H.I.E.L.D.",
                false,
                &[(1, 5, "What? Who/Why")],
                proper,
                "mp4"
            ),
            "Marvel's Agents - S.H.I.E.L.D. - S01E05 - What! Who+Why Bluray-1080p Proper.mp4"
        );
        assert_eq!(
            episode_path("Show", true, &[(0, 1, TBA)], quality(Quality::Sdtv), "avi"),
            "Specials/Show - S00E01 - TBA SDTV.avi"
        );
    }

    #[test]
    fn titulo_longo_e_cortado() {
        let long = "palavra ".repeat(60);
        let path = episode_path(
            "Show",
            false,
            &[(1, 1, &long)],
            quality(Quality::WebDl1080p),
            "mkv",
        );
        assert!(path.len() < 220, "{}", path.len());
        assert!(path.ends_with(" WEBDL-1080p.mkv"));
    }
}

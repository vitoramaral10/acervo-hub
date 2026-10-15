//! O plano da pasta adaptada (legenda separada para o BJ), como funções puras.
//!
//! O BJ exige a legenda PT-BR em arquivo ao lado do vídeo (4.2.1.5) e proíbe
//! muxá-la (4.2.1.4). Release sem áudio PT cuja legenda PT só existe embutida
//! e em texto é adaptado: uma pasta nova com hardlink de tudo o que o torrent
//! de origem tem, mais um `.srt` extraído por vídeo. O vídeo continua intacto.
//!
//! Este módulo só decide **o que** fazer: [`plan`] devolve a pasta destino, os
//! hardlinks e as extrações. Quem toca em disco (hardlink, `ffmpeg`) é o
//! preparo; o layout final só se fecha depois, com o tamanho dos `.srt`
//! extraídos ([`AdaptationPlan::final_layout`]).
//!
//! Todo caminho relativo daqui é relativo ao **pai** da pasta adaptada, ou
//! seja, começa pelo nome dela (`<nome>/<arquivo>`): é a forma do
//! `launches.adapted_layout`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mediainfo::{SubtitleFacts, SubtitleKind};

use super::rules::{
    ProbedVideo, Site, SourceFile, file_name, is_portuguese_subtitle, parent, stem,
    videos_needing_extraction, videos_without_portuguese,
};
use super::subtitle_sniff::Sniffs;

/// Pasta, dentro de `<raiz>`, onde ficam as pastas adaptadas.
pub(crate) const ADAPTED_DIR: &str = "lancamentos";

/// Uma legenda a extrair de um vídeo para um `.srt` ao lado dele.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extraction {
    /// Caminho do vídeo na origem (com a pasta raiz do torrent).
    pub video: String,
    /// Índice absoluto do stream no vídeo (`-map 0:<índice>`).
    pub stream: u32,
    /// Caminho do `.srt` a criar, relativo ao pai da pasta adaptada.
    pub output: String,
    /// `true` para o `<stem>.pt-BR.forced.srt`.
    pub forced: bool,
}

/// Um hardlink: o arquivo da origem e onde ele aparece na pasta adaptada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub from: PathBuf,
    /// Relativo ao pai da pasta adaptada.
    pub to: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptationPlan {
    /// `<raiz>/lancamentos/bj/<nome>`.
    pub target: PathBuf,
    pub links: Vec<Link>,
    pub extractions: Vec<Extraction>,
}

impl AdaptationPlan {
    /// O layout esperado da pasta adaptada: `(caminho, tamanho)`, com `None`
    /// no `.srt`, cujo tamanho só se sabe depois de extrair. Em ordem de
    /// caminho.
    #[must_use]
    pub fn expected_layout(&self) -> Vec<(String, Option<u64>)> {
        let mut layout: Vec<(String, Option<u64>)> = self
            .links
            .iter()
            .map(|l| (l.to.clone(), Some(l.size)))
            .chain(self.extractions.iter().map(|e| (e.output.clone(), None)))
            .collect();
        layout.sort();
        layout
    }

    /// O layout final, com o tamanho de cada `.srt` já extraído (`size_of`
    /// recebe o `output` da extração). `None` se algum `.srt` não tem tamanho.
    #[must_use]
    pub fn final_layout(
        &self,
        size_of: impl Fn(&str) -> Option<u64>,
    ) -> Option<Vec<(String, u64)>> {
        self.expected_layout()
            .into_iter()
            .map(|(path, size)| Some((path.clone(), size.or_else(|| size_of(&path))?)))
            .collect()
    }
}

/// O que impede de extrair a legenda PT embutida de um vídeo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obstacle {
    /// Não há legenda PT embutida (ou só a forçada, que não substitui a completa).
    NoFullTrack,
    /// A legenda PT completa é imagem (PGS, `VobSub`, DVB): só OCR a converteria.
    Image(String),
    /// Codec que o acervo não sabe se é texto.
    UnknownCodec(String),
}

/// As faixas PT escolhidas num vídeo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pick<'a> {
    pub full: &'a SubtitleFacts,
    pub forced: Option<&'a SubtitleFacts>,
}

/// Escolhe a faixa PT completa (não forçada) e, se houver, a forçada. Entre
/// várias, vale a de título "Brasil"/"Brazil"/"BR" ou idioma `pob`; senão a
/// primeira. Só faixa de texto serve, e faixa de comentários nunca é a
/// legenda completa: sem outra de diálogo, é [`Obstacle::NoFullTrack`].
///
/// # Errors
/// [`Obstacle`] quando não há faixa completa em texto para extrair.
pub fn pick(subtitles: &[SubtitleFacts]) -> Result<Pick<'_>, Obstacle> {
    let portuguese = |forced: bool| {
        subtitles
            .iter()
            .filter(move |s| s.forced == forced)
            .filter(|s| is_portuguese_subtitle(s.language, s.raw_language.as_deref()))
            .filter(|s| !is_commentary(s))
    };
    let full: Vec<&SubtitleFacts> = portuguese(false).collect();
    // Texto antes de imagem: um filme com PGS e SubRip em PT adapta pelo SubRip.
    let texts: Vec<&SubtitleFacts> = full
        .iter()
        .copied()
        .filter(|s| s.kind == SubtitleKind::Text)
        .collect();
    let Some(chosen) = preferred(&texts) else {
        return Err(match full.first() {
            None => Obstacle::NoFullTrack,
            Some(s) if s.kind == SubtitleKind::Image => Obstacle::Image(s.codec.clone()),
            Some(s) => Obstacle::UnknownCodec(s.codec.clone()),
        });
    };
    let forced_texts: Vec<&SubtitleFacts> = portuguese(true)
        .filter(|s| s.kind == SubtitleKind::Text)
        .collect();
    Ok(Pick {
        full: chosen,
        forced: preferred(&forced_texts),
    })
}

/// A faixa brasileira (título ou idioma `pob`) ou, na falta, a primeira.
fn preferred<'a>(tracks: &[&'a SubtitleFacts]) -> Option<&'a SubtitleFacts> {
    tracks
        .iter()
        .copied()
        .find(|s| is_brazilian(s))
        .or_else(|| tracks.first().copied())
}

/// Título de faixa de comentários do diretor ("Comentários", "Commentary",
/// "Director"): não é a legenda do diálogo.
pub(crate) fn is_commentary(track: &SubtitleFacts) -> bool {
    track.title.as_deref().is_some_and(|title| {
        let title = title.to_lowercase();
        title.contains("coment") || title.contains("comment") || title.contains("director")
    })
}

fn is_brazilian(track: &SubtitleFacts) -> bool {
    if track
        .raw_language
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case("pob"))
    {
        return true;
    }
    track.title.as_deref().is_some_and(|title| {
        title
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|t| matches!(t, "brasil" | "brazil" | "brazilian" | "br" | "ptbr"))
    })
}

/// A pasta raiz da origem; para arquivo único, o nome sem a extensão. `None`
/// sem arquivos ou com vários arquivos sem pasta raiz comum.
#[must_use]
pub fn folder_name(files: &[SourceFile]) -> Option<String> {
    let first = files.first()?;
    match first.path.split_once('/') {
        Some((root, _)) => files
            .iter()
            .all(|f| f.path.split_once('/').is_some_and(|(r, _)| r == root))
            .then(|| root.to_owned()),
        None => (files.len() == 1).then(|| stem(&first.path).to_owned()),
    }
}

/// As extrações de que o pacote precisa: um par `.srt` (completa e, se houver,
/// forçada) por vídeo sem áudio PT e sem legenda PT em arquivo. Vídeo cuja
/// legenda não dá para extrair fica de fora (é o [`Obstacle`] que a regra
/// reporta). Vazio sem pasta raiz identificável.
#[must_use]
pub fn extractions(
    files: &[SourceFile],
    videos: &[ProbedVideo],
    sniffs: &Sniffs,
) -> Vec<Extraction> {
    let Some(name) = folder_name(files) else {
        return Vec::new();
    };
    let single = !files[0].path.contains('/');
    let mut out = Vec::new();
    for video in videos_needing_extraction(files, videos, sniffs) {
        let Ok(picked) = pick(&video.media.subtitles) else {
            continue;
        };
        let place = if single {
            format!("{name}/{}", video.path)
        } else {
            video.path.clone()
        };
        let base = format!("{}/{}", parent(&place), stem(file_name(&place)));
        let full = free_output(files, &out, &name, single, &base, "pt-BR", "srt");
        out.push(Extraction {
            video: video.path.clone(),
            stream: picked.full.index,
            output: full,
            forced: false,
        });
        if let Some(forced) = picked.forced {
            let output = free_output(files, &out, &name, single, &base, "pt-BR.forced", "srt");
            out.push(Extraction {
                video: video.path.clone(),
                stream: forced.index,
                output,
                forced: true,
            });
        }
    }
    out
}

/// `<base>.<tag>.<ext>`, ou, se o nome já existe na origem (ou já foi
/// reservado para outra extração), `<base>.2.<tag>.<ext>`, `.3`...: a
/// extração nunca sobrescreve um arquivo. O sufixo de idioma continua no
/// fim do nome, então a legenda segue sendo reconhecida como PT.
fn free_output(
    files: &[SourceFile],
    taken: &[Extraction],
    name: &str,
    single: bool,
    base: &str,
    tag: &str,
    ext: &str,
) -> String {
    let used = |candidate: &str| {
        let candidate = candidate.to_lowercase();
        taken.iter().any(|e| e.output.to_lowercase() == candidate)
            || files.iter().any(|f| {
                let path = if single {
                    format!("{name}/{}", f.path)
                } else {
                    f.path.clone()
                };
                path.to_lowercase() == candidate
            })
    };
    let first = format!("{base}.{tag}.{ext}");
    if !used(&first) {
        return first;
    }
    (2..=u32::MAX)
        .map(|n| format!("{base}.{n}.{tag}.{ext}"))
        .find(|candidate| !used(candidate))
        .unwrap_or(first)
}

/// O plano da pasta adaptada, ou `None` se não há o que adaptar: outro site
/// que não o BJ, nenhum vídeo que precise de `.srt`, vídeo cuja legenda não é
/// extraível (a regra já reprova), ou origem que não bate (`content_path` fora
/// da pasta raiz, `save_path` sem pai).
///
/// `source_files` leva a pasta raiz no caminho; `content_path` é o caminho
/// absoluto do conteúdo (pasta ou arquivo) e `save_path` o diretório de
/// download. A pasta destino é `<pai do save_path>/lancamentos/bj/<nome>`.
///
/// Não considera a língua original da obra: quem decide se a obra nacional
/// dispensa a legenda é a regra, e o preparo só chama `plan` quando
/// `Report.adaptations` não está vazio.
#[must_use]
pub fn plan(
    source_files: &[SourceFile],
    videos: &[ProbedVideo],
    sniffs: &Sniffs,
    content_path: &Path,
    save_path: &Path,
    site: Site,
) -> Option<AdaptationPlan> {
    if site != Site::Bj {
        return None;
    }
    let name = folder_name(source_files)?;
    let single = !source_files[0].path.contains('/');
    // O conteúdo tem de ser a pasta raiz (ou o arquivo) que os caminhos dizem.
    let expected = if single {
        file_name(&source_files[0].path)
    } else {
        &name
    };
    if content_path.file_name()?.to_str()? != expected {
        return None;
    }
    let extractions = extractions(source_files, videos, sniffs);
    // Todo vídeo sem português precisa ter virado extração.
    let missing = videos_without_portuguese(source_files, videos, sniffs, Site::Bj);
    if extractions.is_empty()
        || missing
            .iter()
            .any(|v| !extractions.iter().any(|e| e.video == v.path))
    {
        return None;
    }
    let target = save_path.parent()?.join(ADAPTED_DIR).join("bj").join(&name);
    let links = source_files
        .iter()
        .map(|f| {
            if single {
                Link {
                    from: content_path.to_path_buf(),
                    to: format!("{name}/{}", f.path),
                    size: f.size,
                }
            } else {
                let rest = f.path.split_once('/').map_or("", |(_, rest)| rest);
                Link {
                    from: content_path.join(rest),
                    to: f.path.clone(),
                    size: f.size,
                }
            }
        })
        .collect();
    Some(AdaptationPlan {
        target,
        links,
        extractions,
    })
}

#[cfg(test)]
mod tests {
    use acervo_parser::Language;

    use super::*;
    use crate::launch::subtitle_sniff::{Sniff, SniffLanguage};
    use crate::mediainfo::{AudioFacts, MediaFacts};

    const GB: u64 = 1_000_000_000;

    fn sub(index: u32, raw: &str, codec: &str, forced: bool, title: Option<&str>) -> SubtitleFacts {
        SubtitleFacts {
            index,
            kind: SubtitleKind::of_codec(codec),
            language: if raw == "eng" {
                Language::English
            } else {
                Language::Portuguese
            },
            raw_language: Some(raw.to_owned()),
            codec: codec.to_owned(),
            forced,
            title: title.map(str::to_owned),
        }
    }

    fn text(index: u32, raw: &str) -> SubtitleFacts {
        sub(index, raw, "subrip", false, None)
    }

    fn english_audio() -> AudioFacts {
        AudioFacts {
            language: Language::English,
            raw_language: Some("eng".to_owned()),
            codec: "eac3".to_owned(),
            channels: Some(6),
            title: None,
        }
    }

    fn probed(path: &str, subtitles: Vec<SubtitleFacts>) -> ProbedVideo {
        ProbedVideo {
            path: path.to_owned(),
            media: MediaFacts {
                container: "matroska,webm".to_owned(),
                duration_seconds: Some(6000.0),
                video: None,
                audio: vec![english_audio()],
                subtitles,
            },
        }
    }

    fn file(path: &str, size: u64) -> SourceFile {
        SourceFile {
            path: path.to_owned(),
            size,
        }
    }

    fn plan_bj(
        files: &[SourceFile],
        videos: &[ProbedVideo],
        content: &str,
    ) -> Option<AdaptationPlan> {
        plan(
            files,
            videos,
            &Sniffs::new(),
            Path::new(content),
            Path::new("/media/downloads"),
            Site::Bj,
        )
    }

    #[test]
    fn arquivo_unico_vai_para_dentro_da_pasta_nova() {
        let files = [file("Filme.2024.mkv", 5 * GB)];
        let videos = [probed("Filme.2024.mkv", vec![text(3, "por")])];
        let plan = plan_bj(&files, &videos, "/media/downloads/Filme.2024.mkv").expect("plano");
        assert_eq!(plan.target, Path::new("/media/lancamentos/bj/Filme.2024"));
        assert_eq!(
            plan.links,
            vec![Link {
                from: PathBuf::from("/media/downloads/Filme.2024.mkv"),
                to: "Filme.2024/Filme.2024.mkv".to_owned(),
                size: 5 * GB,
            }]
        );
        assert_eq!(
            plan.extractions,
            vec![Extraction {
                video: "Filme.2024.mkv".to_owned(),
                stream: 3,
                output: "Filme.2024/Filme.2024.pt-BR.srt".to_owned(),
                forced: false,
            }]
        );
        assert_eq!(
            plan.expected_layout(),
            vec![
                ("Filme.2024/Filme.2024.mkv".to_owned(), Some(5 * GB)),
                ("Filme.2024/Filme.2024.pt-BR.srt".to_owned(), None),
            ]
        );
    }

    #[test]
    fn pacote_de_temporada_extrai_um_srt_por_episodio_e_liga_tudo() {
        let files = [
            file("Serie.S01/Serie.S01E01.mkv", GB),
            file("Serie.S01/Serie.S01E02.mkv", GB),
            file("Serie.S01/Legendas/Serie.S01E02.eng.srt", 100),
            file("Serie.S01/poster.jpg", 50),
        ];
        let videos = [
            probed("Serie.S01/Serie.S01E01.mkv", vec![text(2, "por")]),
            probed("Serie.S01/Serie.S01E02.mkv", vec![text(4, "pob")]),
        ];
        let plan = plan_bj(&files, &videos, "/media/downloads/Serie.S01").expect("plano");
        assert_eq!(plan.target, Path::new("/media/lancamentos/bj/Serie.S01"));
        // Tudo da origem ganha hardlink, na mesma estrutura.
        let tos: Vec<&str> = plan.links.iter().map(|l| l.to.as_str()).collect();
        assert_eq!(
            tos,
            [
                "Serie.S01/Serie.S01E01.mkv",
                "Serie.S01/Serie.S01E02.mkv",
                "Serie.S01/Legendas/Serie.S01E02.eng.srt",
                "Serie.S01/poster.jpg",
            ]
        );
        assert_eq!(
            plan.links[2].from,
            Path::new("/media/downloads/Serie.S01/Legendas/Serie.S01E02.eng.srt")
        );
        let saidas: Vec<(&str, u32)> = plan
            .extractions
            .iter()
            .map(|e| (e.output.as_str(), e.stream))
            .collect();
        assert_eq!(
            saidas,
            [
                ("Serie.S01/Serie.S01E01.pt-BR.srt", 2),
                ("Serie.S01/Serie.S01E02.pt-BR.srt", 4),
            ]
        );
    }

    #[test]
    fn video_que_ja_tem_legenda_pt_em_arquivo_nao_e_extraido() {
        let files = [
            file("Serie.S01/Serie.S01E01.mkv", GB),
            file("Serie.S01/Serie.S01E01.pt-BR.srt", 100),
            file("Serie.S01/Serie.S01E02.mkv", GB),
        ];
        let videos = [
            probed("Serie.S01/Serie.S01E01.mkv", vec![text(2, "por")]),
            probed("Serie.S01/Serie.S01E02.mkv", vec![text(2, "por")]),
        ];
        let plan = plan_bj(&files, &videos, "/media/downloads/Serie.S01").expect("plano");
        assert_eq!(plan.extractions.len(), 1);
        assert_eq!(plan.extractions[0].video, "Serie.S01/Serie.S01E02.mkv");
    }

    #[test]
    fn faixa_forcada_gera_o_segundo_srt() {
        let files = [file("Filme/Filme.mkv", 4 * GB)];
        let videos = [probed(
            "Filme/Filme.mkv",
            vec![
                sub(5, "por", "subrip", true, None),
                text(3, "por"),
                sub(6, "eng", "subrip", false, None),
            ],
        )];
        let plan = plan_bj(&files, &videos, "/media/downloads/Filme").expect("plano");
        let saidas: Vec<(&str, u32, bool)> = plan
            .extractions
            .iter()
            .map(|e| (e.output.as_str(), e.stream, e.forced))
            .collect();
        assert_eq!(
            saidas,
            [
                ("Filme/Filme.pt-BR.srt", 3, false),
                ("Filme/Filme.pt-BR.forced.srt", 5, true),
            ]
        );
    }

    #[test]
    fn so_faixa_forcada_nao_substitui_a_completa() {
        let files = [file("Filme/Filme.mkv", GB)];
        let videos = [probed(
            "Filme/Filme.mkv",
            vec![sub(5, "por", "subrip", true, None)],
        )];
        assert_eq!(pick(&videos[0].media.subtitles), Err(Obstacle::NoFullTrack));
        assert!(plan_bj(&files, &videos, "/media/downloads/Filme").is_none());
    }

    #[test]
    fn varias_faixas_pt_preferem_titulo_brasil_ou_idioma_pob() {
        let por_pt = sub(2, "por", "subrip", false, Some("Português de Portugal"));
        let por_br = sub(3, "por", "subrip", false, Some("Português (Brasil)"));
        let tracks = [por_pt.clone(), por_br];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 3);

        let tracks = [por_pt.clone(), sub(4, "pob", "ass", false, None)];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 4);

        let tracks = [por_pt.clone(), sub(5, "por", "subrip", false, Some("BR"))];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 5);

        // Sem nenhuma brasileira, a primeira.
        let tracks = [por_pt, sub(6, "por", "subrip", false, Some("Comentários"))];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 2);
    }

    #[test]
    fn faixa_de_comentarios_nunca_e_a_legenda_completa() {
        let comments = sub(2, "por", "subrip", false, Some("Comentários do diretor"));
        let dialogue = sub(3, "por", "subrip", false, Some("Português"));
        let tracks = [comments.clone(), dialogue];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 3);

        // Comentário com "Brasil" no título não ganha da faixa de diálogo.
        let br_comments = sub(4, "por", "subrip", false, Some("Comentários (Brasil)"));
        let tracks = [br_comments, sub(5, "por", "subrip", false, None)];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 5);

        // Só comentários (inclusive forçada de comentários): inelegível.
        assert_eq!(pick(&[comments]), Err(Obstacle::NoFullTrack));
        let director = sub(6, "por", "subrip", false, Some("Director's track"));
        assert_eq!(pick(&[director]), Err(Obstacle::NoFullTrack));
        let commentary = sub(7, "por", "subrip", false, Some("Commentary"));
        let only_forced_comments = sub(8, "por", "subrip", true, Some("Comentários"));
        assert_eq!(
            pick(&[commentary, only_forced_comments]),
            Err(Obstacle::NoFullTrack)
        );
    }

    #[test]
    fn titulo_com_br_dentro_de_outra_palavra_nao_conta() {
        let a = sub(2, "por", "subrip", false, Some("Abrir legenda"));
        let b = sub(3, "por", "subrip", false, Some("Brasil"));
        assert_eq!(pick(&[a, b]).expect("faixa").full.index, 3);
    }

    #[test]
    fn so_imagem_nao_gera_plano() {
        let files = [file("Filme/Filme.mkv", GB)];
        let videos = [probed(
            "Filme/Filme.mkv",
            vec![sub(3, "por", "hdmv_pgs_subtitle", false, None)],
        )];
        assert_eq!(
            pick(&videos[0].media.subtitles),
            Err(Obstacle::Image("hdmv_pgs_subtitle".to_owned()))
        );
        assert!(plan_bj(&files, &videos, "/media/downloads/Filme").is_none());
    }

    #[test]
    fn texto_vence_imagem_quando_as_duas_existem() {
        let tracks = [
            sub(3, "por", "hdmv_pgs_subtitle", false, None),
            text(4, "por"),
        ];
        assert_eq!(pick(&tracks).expect("faixa").full.index, 4);
    }

    #[test]
    fn nome_com_caracteres_especiais_vira_srt_ao_lado_do_video() {
        let name = "Amélie [2001] (Dir. Cut) #1 & Cia.mkv";
        let path = format!("Amélie [2001] (Dir. Cut)/{name}");
        let files = [file(&path, 3 * GB)];
        let videos = [probed(&path, vec![text(2, "por")])];
        let plan =
            plan_bj(&files, &videos, "/media/downloads/Amélie [2001] (Dir. Cut)").expect("plano");
        assert_eq!(
            plan.target,
            Path::new("/media/lancamentos/bj/Amélie [2001] (Dir. Cut)")
        );
        assert_eq!(
            plan.extractions[0].output,
            "Amélie [2001] (Dir. Cut)/Amélie [2001] (Dir. Cut) #1 & Cia.pt-BR.srt"
        );
    }

    #[test]
    fn amigos_nao_gera_plano() {
        let files = [file("Filme/Filme.mkv", GB)];
        let videos = [probed("Filme/Filme.mkv", vec![text(3, "por")])];
        assert!(
            plan(
                &files,
                &videos,
                &Sniffs::new(),
                Path::new("/media/downloads/Filme"),
                Path::new("/media/downloads"),
                Site::Amigos,
            )
            .is_none()
        );
    }

    #[test]
    fn origem_inconsistente_nao_gera_plano() {
        let files = [file("Filme/Filme.mkv", GB)];
        let videos = [probed("Filme/Filme.mkv", vec![text(3, "por")])];
        // content_path fora da pasta raiz.
        assert!(plan_bj(&files, &videos, "/media/downloads").is_none());
        // Vários arquivos sem pasta raiz comum.
        let files = [file("A.mkv", GB), file("B.mkv", GB)];
        assert!(plan_bj(&files, &[], "/media/downloads/A.mkv").is_none());
    }

    #[test]
    fn layout_final_completa_o_tamanho_dos_srt() {
        let files = [file("Filme/Filme.mkv", 4 * GB)];
        let videos = [probed("Filme/Filme.mkv", vec![text(3, "por")])];
        let plan = plan_bj(&files, &videos, "/media/downloads/Filme").expect("plano");
        assert_eq!(plan.final_layout(|_| None), None);
        let sizes: std::collections::HashMap<String, u64> =
            [("Filme/Filme.pt-BR.srt".to_owned(), 70_000)].into();
        assert_eq!(
            plan.final_layout(|p| sizes.get(p).copied()),
            Some(vec![
                ("Filme/Filme.mkv".to_owned(), 4 * GB),
                ("Filme/Filme.pt-BR.srt".to_owned(), 70_000),
            ])
        );
    }

    fn sniffed(language: SniffLanguage, cues: usize) -> Sniff {
        Sniff {
            language,
            cues,
            covered_secs: f64::from(u32::try_from(cues).unwrap()) * 2.0,
            video_secs: Some(6000.0),
        }
    }

    fn plan_with_sniff(
        files: &[SourceFile],
        videos: &[ProbedVideo],
        sniff: Sniff,
    ) -> Option<AdaptationPlan> {
        let sniffs: Sniffs = [("Filme/Filme.srt".to_owned(), sniff)].into();
        plan(
            files,
            videos,
            &sniffs,
            Path::new("/media/downloads/Filme"),
            Path::new("/media/downloads"),
            Site::Bj,
        )
    }

    #[test]
    fn so_srt_pt_br_completo_confirmado_dispensa_a_extracao() {
        let files = [file("Filme/Filme.mkv", GB), file("Filme/Filme.srt", 90_000)];
        let videos = [probed("Filme/Filme.mkv", vec![text(3, "por")])];
        // PT-BR completo: nada a extrair.
        let confirmed = sniffed(SniffLanguage::PtBr, 995);
        assert!(plan_with_sniff(&files, &videos, confirmed).is_none());
        // Qualquer outro caso extrai a faixa embutida normalmente.
        for other in [
            sniffed(SniffLanguage::Pt, 995),
            sniffed(SniffLanguage::PtPt, 995),
            sniffed(SniffLanguage::PtBr, 28),
            sniffed(SniffLanguage::PtBr, 300),
            sniffed(SniffLanguage::English, 995),
        ] {
            let plan = plan_with_sniff(&files, &videos, other).expect("extrai");
            assert_eq!(plan.extractions.len(), 1, "{other:?}");
            assert_eq!(plan.extractions[0].output, "Filme/Filme.pt-BR.srt");
            assert_eq!(plan.extractions[0].stream, 3);
        }
    }

    #[test]
    fn nome_ja_existente_na_origem_nao_e_sobrescrito() {
        // A forçada já existe na origem (o BJ não a conta como a completa).
        let files = [
            file("Filme/Filme.mkv", GB),
            file("Filme/Filme.pt-BR.forced.srt", 500),
        ];
        let videos = [probed(
            "Filme/Filme.mkv",
            vec![text(3, "por"), sub(5, "por", "subrip", true, None)],
        )];
        let plan = plan_bj(&files, &videos, "/media/downloads/Filme").expect("plano");
        let saidas: Vec<(&str, bool)> = plan
            .extractions
            .iter()
            .map(|e| (e.output.as_str(), e.forced))
            .collect();
        assert_eq!(
            saidas,
            [
                ("Filme/Filme.pt-BR.srt", false),
                ("Filme/Filme.2.pt-BR.forced.srt", true),
            ]
        );
        // Ocupado também fora de caixa, e o novo nome segue sendo PT pelo sufixo.
        let files = [
            file("Filme/Filme.mkv", GB),
            file("Filme/FILME.PT-BR.FORCED.SRT", 500),
            file("Filme/Filme.2.pt-BR.forced.srt", 500),
        ];
        let plan = plan_bj(&files, &videos, "/media/downloads/Filme").expect("plano");
        assert_eq!(plan.extractions[1].output, "Filme/Filme.3.pt-BR.forced.srt");
    }
}

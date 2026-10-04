//! Legendas que vêm como arquivo separado no torrent: o nome que ganham ao
//! lado do vídeo e o idioma que o nome delas diz. Tudo puro; quem liga e
//! grava é a importação.
//!
//! O nome é `<stem do vídeo>.<idioma>[.forced].<ext>`, o que o Jellyfin lê:
//! o idioma só entra quando o nome original o diz no fim (`.por.srt`,
//! `2_English.srt`, `Portuguese (Brazil).srt`), normalizado para `pt-BR`,
//! `pt` ou `en`. Dois arquivos no mesmo nome ganham número (`.pt-BR.1.srt`).

use std::collections::HashSet;
use std::path::Path;

/// A legenda veio do torrent: registrada pela importação, ou com outro link
/// (o hardlink com o download). Só essa o upgrade apaga ou troca; a posta à
/// mão fica.
#[must_use]
pub fn from_torrent(origin: Option<acervo_store::SubtitleOrigin>, nlink: u64) -> bool {
    origin == Some(acervo_store::SubtitleOrigin::Import) || nlink > 1
}

/// Apaga a legenda antiga se ela veio do torrent ([`from_torrent`]).
/// `true` se ela não está mais lá (apagada, ou já não existia). Bloqueia.
///
/// # Errors
///
/// Falha ao ler ou apagar.
pub fn remove_if_from_torrent(
    host: &Path,
    origin: Option<acervo_store::SubtitleOrigin>,
) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(host) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
        Ok(meta) if meta.is_file() && from_torrent(origin, meta.nlink()) => {
            match std::fs::remove_file(host) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(true),
            }
        }
        Ok(_) => Ok(false),
    }
}

/// Liga a legenda nova em `target`. Destino livre: liga. Já é o mesmo
/// arquivo: nada. Ocupado por uma do torrent (`origin` é a registrada ali):
/// liga num temporário e renomeia por cima, como o `install` do vídeo.
/// Ocupado por uma posta à mão: não toca, e devolve `false`. Bloqueia.
///
/// # Errors
///
/// Falha ao ler, ligar ou renomear.
pub fn place(
    source: &Path,
    target: &Path,
    origin: Option<acervo_store::SubtitleOrigin>,
) -> anyhow::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let existing = match std::fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::grab::link(source, target)?;
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
        Ok(meta) => meta,
    };
    let from = std::fs::metadata(source)?;
    if existing.dev() == from.dev() && existing.ino() == from.ino() {
        return Ok(true);
    }
    if !existing.is_file() || !from_torrent(origin, existing.nlink()) {
        return Ok(false);
    }
    let temporary = target.with_extension("acervo-novo");
    match std::fs::remove_file(&temporary) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    crate::grab::link(source, &temporary)?;
    std::fs::rename(&temporary, target)?;
    Ok(true)
}

/// Extensões de legenda que a importação leva. `.sub` vai com o `.idx`.
pub(crate) const SUBTITLE: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "vtt"];

/// A extensão, em minúsculas, se for de legenda.
pub(crate) fn subtitle_extension(name: &str) -> Option<String> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    SUBTITLE.contains(&extension.as_str()).then_some(extension)
}

/// Marcas que podem vir depois do idioma sem dizer idioma.
const MODIFIERS: &[&str] = &[
    "forced", "forcada", "forçada", "forcado", "forçado", "sdh", "cc", "hi", "default", "full",
    "completa", "completo",
];

fn is_forced(token: &str) -> bool {
    matches!(
        token,
        "forced" | "forcada" | "forçada" | "forcado" | "forçado"
    )
}

/// O idioma de um token, normalizado.
fn language_of(token: &str) -> Option<&'static str> {
    match token {
        "pt-br" | "pt_br" | "ptbr" | "pob" | "pb" | "pt-bra" | "brazilian" | "brazil"
        | "brasil" | "brasileiro" | "portuguese-brazil" | "portugues-brasil" => Some("pt-BR"),
        "pt" | "pt-pt" | "por" | "portuguese" | "portugues" | "português" => Some("pt"),
        "en" | "eng" | "english" | "en-us" | "en-gb" | "ingles" | "inglês" => Some("en"),
        _ => None,
    }
}

/// O idioma e se é forçada, pelo fim do nome do arquivo (sem a extensão).
/// Só o fim conta: idioma no meio do nome é palavra do título.
#[must_use]
pub fn language(name: &str) -> (Option<&'static str>, bool) {
    let file = name.rsplit('/').next().unwrap_or(name);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let lower = stem.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(['.', ' ', '_', '[', ']', '(', ')', ','])
        .filter(|t| !t.is_empty())
        .collect();
    let mut forced = false;
    let mut rest = tokens.as_slice();
    while let Some((last, before)) = rest.split_last() {
        if is_forced(last) {
            forced = true;
        } else if !(MODIFIERS.contains(last) || last.bytes().all(|b| b.is_ascii_digit())) {
            break;
        }
        rest = before;
    }
    let Some((last, before)) = rest.split_last() else {
        return (None, forced);
    };
    // "pt.BR": o idioma vem em dois pedaços.
    let found = language_of(last).or_else(|| match (before.last(), *last) {
        (Some(&("portuguese" | "portugues" | "pt")), "br") => Some("pt-BR"),
        _ => None,
    });
    (found, forced)
}

/// Uma legenda como vai ficar ao lado do vídeo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// Só o nome do arquivo, sem pasta.
    pub name: String,
    pub language: Option<&'static str>,
    pub forced: bool,
}

/// Os nomes das legendas de um vídeo, na ordem recebida. `taken` são os
/// nomes que já existem ao lado dele e não podem ser repetidos.
#[must_use]
pub fn names(video_stem: &str, subtitles: &[&str], taken: &HashSet<String>) -> Vec<Named> {
    let mut used: HashSet<String> = taken.clone();
    subtitles
        .iter()
        .map(|original| {
            let (language, forced) = language(original);
            let extension = subtitle_extension(original).unwrap_or_else(|| "srt".into());
            let mut middle = String::new();
            if let Some(language) = language {
                middle.push('.');
                middle.push_str(language);
            }
            if forced {
                middle.push_str(".forced");
            }
            let mut name = format!("{video_stem}{middle}.{extension}");
            let mut n = 1;
            while used.contains(&name) {
                name = format!("{video_stem}{middle}.{n}.{extension}");
                n += 1;
            }
            used.insert(name.clone());
            Named {
                name,
                language,
                forced,
            }
        })
        .collect()
}

/// O caminho de uma legenda depois que o vídeo dela mudou de nome: o que
/// vem depois do stem do vídeo fica. `None` se a legenda não segue o nome
/// do vídeo (aí não é renomeada).
#[must_use]
pub fn follow(old_video: &str, new_video: &str, subtitle: &str) -> Option<String> {
    let stem = |path: &str| -> String {
        match path.rsplit_once('.') {
            Some((stem, _)) if !stem.ends_with('/') => stem.to_owned(),
            _ => path.to_owned(),
        }
    };
    let rest = subtitle.strip_prefix(&stem(old_video))?;
    rest.starts_with('.')
        .then(|| format!("{}{rest}", stem(new_video)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idioma_pelo_fim_do_nome() {
        for (name, expected) in [
            ("Show.S01E10.por.srt", Some("pt")),
            ("Movie.2020.pt-BR.srt", Some("pt-BR")),
            ("Movie.2020.PT_BR.srt", Some("pt-BR")),
            ("Subs/2_English.srt", Some("en")),
            ("Subs/Portuguese (Brazil).srt", Some("pt-BR")),
            ("Subs/3_Portuguese.srt", Some("pt")),
            ("Movie.eng.SDH.srt", Some("en")),
            ("Movie.pob.ass", Some("pt-BR")),
            ("Movie.pt.BR.srt", Some("pt-BR")),
            ("Movie.2020.1080p.WEB-DL-GRUPO.srt", None),
            ("Amor.en.la.ciudad.2020.srt", None),
            ("legenda.srt", None),
        ] {
            assert_eq!(language(name).0, expected, "{name}");
        }
        assert_eq!(language("Movie.pt-BR.forced.srt"), (Some("pt-BR"), true));
        assert_eq!(language("Movie.Forced.eng.srt"), (Some("en"), false));
        assert_eq!(language("forced.srt"), (None, true));
    }

    #[test]
    fn nome_ao_lado_do_video_e_numero_na_colisao() {
        let named = names(
            "Filme (2020) {imdb-tt1}",
            &[
                "Subs/Portuguese (Brazil).srt",
                "Subs/pt-BR.SRT",
                "Subs/pt-BR.forced.srt",
                "Subs/sem idioma.srt",
                "Subs/vob.por.idx",
                "Subs/vob.por.sub",
            ],
            &HashSet::new(),
        );
        let got: Vec<&str> = named.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            got,
            [
                "Filme (2020) {imdb-tt1}.pt-BR.srt",
                "Filme (2020) {imdb-tt1}.pt-BR.1.srt",
                "Filme (2020) {imdb-tt1}.pt-BR.forced.srt",
                "Filme (2020) {imdb-tt1}.srt",
                "Filme (2020) {imdb-tt1}.pt.idx",
                "Filme (2020) {imdb-tt1}.pt.sub",
            ]
        );
        assert_eq!(named[2].language, Some("pt-BR"));
        assert!(named[2].forced);
        // O que já está lá conta como tomado.
        let taken = HashSet::from(["V.en.srt".to_owned()]);
        assert_eq!(names("V", &["x.eng.srt"], &taken)[0].name, "V.en.1.srt");
        assert!(subtitle_extension("a.VTT").is_some());
        assert!(subtitle_extension("a.mkv").is_none());
    }

    #[test]
    fn so_a_legenda_do_torrent_e_trocada_ou_apagada() {
        use acervo_store::SubtitleOrigin::{Disk, Import};
        assert!(from_torrent(Some(Import), 1));
        assert!(from_torrent(Some(Disk), 2));
        assert!(from_torrent(None, 2));
        assert!(!from_torrent(Some(Disk), 1));
        assert!(!from_torrent(None, 1));

        let dir = std::env::temp_dir().join(format!("acervo-legendas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let new = dir.join("nova.srt");
        std::fs::write(&new, b"nova").unwrap();
        // Posta à mão (um link só, sem registro de importação): fica.
        let manual = dir.join("Filme.pt-BR.srt");
        std::fs::write(&manual, b"minha").unwrap();
        assert!(!place(&new, &manual, Some(Disk)).unwrap());
        assert_eq!(std::fs::read(&manual).unwrap(), b"minha");
        assert!(!remove_if_from_torrent(&manual, None).unwrap());
        assert!(manual.exists());
        // Do torrent (registrada pela importação): trocada por cima.
        assert!(place(&new, &manual, Some(Import)).unwrap());
        assert_eq!(std::fs::read(&manual).unwrap(), b"nova");
        assert!(!dir.join("Filme.pt-BR.acervo-novo").exists());
        // Mesmo arquivo: nada a fazer.
        assert!(place(&new, &manual, None).unwrap());
        // Destino livre: liga.
        let free = dir.join("Filme.en.srt");
        assert!(place(&new, &free, None).unwrap());
        // Com outro link, é do torrent mesmo sem registro: sai.
        assert!(remove_if_from_torrent(&free, None).unwrap());
        assert!(!free.exists());
        assert!(remove_if_from_torrent(&free, None).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legenda_segue_o_novo_nome_do_video() {
        assert_eq!(
            follow(
                "Season 1/Show - S01E01 - TBA WEBDL-1080p.mkv",
                "Season 1/Show - S01E01 - Piloto WEBDL-1080p.mkv",
                "Season 1/Show - S01E01 - TBA WEBDL-1080p.pt-BR.srt"
            )
            .as_deref(),
            Some("Season 1/Show - S01E01 - Piloto WEBDL-1080p.pt-BR.srt")
        );
        assert_eq!(follow("a/v.mkv", "b/w.mkv", "a/outra.srt"), None);
        // Prefixo que não é o stem inteiro não conta.
        assert_eq!(follow("a/v.mkv", "b/w.mkv", "a/v2.srt"), None);
    }
}

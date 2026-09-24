//! Peças compartilhadas pelos parsers: compilação dos padrões e limpeza de
//! prefixo de site, sufixo de tracker e extensão de arquivo.

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

/// Compila um padrão que é constante do código. Padrão inválido é bug, não
/// entrada ruim: os testes compilam todos.
pub(crate) fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("padrão inválido `{pattern}`: {error}"))
}

/// `fancy-regex` devolve erro quando o backtracking passa do limite. Aqui
/// isso conta como "não casou": é o que o .NET faz ao estourar o tempo de um
/// padrão, e nenhum título de release chega perto do limite.
pub(crate) fn is_match(regex: &Regex, text: &str) -> bool {
    regex.is_match(text).unwrap_or(false)
}

pub(crate) fn captures<'t>(regex: &Regex, text: &'t str) -> Option<Captures<'t, str>> {
    regex.captures(text).ok().flatten()
}

/// Última ocorrência, como o `Matches(...).Last()` do .NET.
pub(crate) fn last_captures<'t>(regex: &Regex, text: &'t str) -> Option<Captures<'t, str>> {
    regex.captures_iter(text).map_while(Result::ok).last()
}

pub(crate) fn group<'t>(captures: &Captures<'t, str>, name: &str) -> Option<&'t str> {
    captures.name(name).map(|m| m.as_str())
}

/// Substitui toda ocorrência por `with`, como o `Regex.Replace` do .NET.
pub(crate) fn replace_all(regex: &Regex, text: &str, with: &str) -> String {
    regex
        .replace_all(text, |_: &Captures<'_, str>| with.to_owned())
        .into_owned()
}

static WEBSITE_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^(?:(?:\[|\()\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?<!Naruto-Kun\.)(?:[a-z]{2,6}\.[a-z]{2,6}|xn--[a-z0-9-]{4,}|[a-z]{2,})\b(?:\s*(?:\]|\))|[ -]{2,})[ -]*",
    )
});

static WEBSITE_POSTFIX: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?:\[\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?:xn--[a-z0-9-]{4,}|[a-z]{2,6})\b(?:\s*\])$",
    )
});

static CLEAN_TORRENT_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\[(?:ettv|rartv|rarbg|cttv|publichd)\]$"));

pub(crate) fn strip_website_prefix(text: &str) -> String {
    replace_all(&WEBSITE_PREFIX, text, "")
}

pub(crate) fn strip_website_postfix(text: &str) -> String {
    replace_all(&WEBSITE_POSTFIX, text, "")
}

pub(crate) fn strip_torrent_suffix(text: &str) -> String {
    replace_all(&CLEAN_TORRENT_SUFFIX, text, "")
}

/// Extensões de mídia e o que cada uma diz da qualidade quando o nome não diz
/// nada. Mesma tabela do gerenciador de filmes.
pub(crate) const MEDIA_EXTENSIONS: &[(&str, crate::Quality)] = {
    use crate::Quality::{Bluray720p, Dvd, Sdtv, Unknown, WebDl720p};
    &[
        (".webm", Unknown),
        (".m4v", Sdtv),
        (".3gp", Sdtv),
        (".nsv", Sdtv),
        (".ty", Sdtv),
        (".strm", Sdtv),
        (".rm", Sdtv),
        (".rmvb", Sdtv),
        (".m3u", Sdtv),
        (".ifo", Sdtv),
        (".mov", Sdtv),
        (".qt", Sdtv),
        (".divx", Sdtv),
        (".xvid", Sdtv),
        (".bivx", Sdtv),
        (".nrg", Sdtv),
        (".pva", Sdtv),
        (".wmv", Sdtv),
        (".asf", Sdtv),
        (".asx", Sdtv),
        (".ogm", Sdtv),
        (".ogv", Sdtv),
        (".m2v", Sdtv),
        (".avi", Sdtv),
        (".bin", Sdtv),
        (".dat", Sdtv),
        (".dvr-ms", Sdtv),
        (".mpg", Sdtv),
        (".mpeg", Sdtv),
        (".mp4", Sdtv),
        (".avc", Sdtv),
        (".vp3", Sdtv),
        (".svq3", Sdtv),
        (".nuv", Sdtv),
        (".viv", Sdtv),
        (".dv", Sdtv),
        (".fli", Sdtv),
        (".flv", Sdtv),
        (".wpl", Sdtv),
        (".img", Dvd),
        (".iso", Dvd),
        (".vob", Dvd),
        (".mkv", WebDl720p),
        (".mk3d", WebDl720p),
        (".ts", Sdtv),
        (".wtv", Sdtv),
        (".m2ts", Bluray720p),
    ]
};

/// Qualidade sugerida pela extensão; extensão desconhecida é `Unknown`.
pub(crate) fn quality_for_extension(extension: &str) -> crate::Quality {
    MEDIA_EXTENSIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map_or(crate::Quality::Unknown, |(_, quality)| *quality)
}

/// Extensão como o `Path.GetExtension` a entende: do último ponto em diante,
/// desde que depois da última barra e sem terminar no ponto.
pub(crate) fn path_extension(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => &name[dot..],
        _ => "",
    }
}

static FILE_EXTENSION: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\.[a-z0-9]{2,4}$"));

/// Tira a extensão só se ela for de mídia (ou de usenet): "Filme.2020.DUAL"
/// não perde o "DUAL".
pub(crate) fn remove_file_extension(title: &str) -> String {
    FILE_EXTENSION
        .replace_all(title, |captures: &Captures<'_, str>| {
            let extension = captures[0].to_lowercase();
            let known = MEDIA_EXTENSIONS.iter().any(|(e, _)| *e == extension)
                || extension == ".par2"
                || extension == ".nzb";
            if known {
                String::new()
            } else {
                captures[0].to_owned()
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensao_so_sai_se_for_de_midia() {
        assert_eq!(
            remove_file_extension("Filme.2020.1080p.mkv"),
            "Filme.2020.1080p"
        );
        assert_eq!(remove_file_extension("Filme.2020.DUAL"), "Filme.2020.DUAL");
        assert_eq!(path_extension("/a/b/Filme.2020.mkv"), ".mkv");
        assert_eq!(path_extension("sem-extensao"), "");
    }

    #[test]
    fn prefixo_de_site() {
        assert_eq!(
            strip_website_prefix("[ www.site.com ] - Filme.2020.1080p"),
            "Filme.2020.1080p"
        );
    }
}

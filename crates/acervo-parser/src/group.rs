//! Grupo de release: o `-GRUPO` do fim do nome, com as exceções conhecidas.

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::common::{
    captures, group, is_match, last_captures, regex, remove_episode_file_extension,
    remove_file_extension, replace_all, strip_torrent_suffix, strip_website_prefix,
};
use crate::episode::pre_substitute;

/// O que termina em "-X" e não é grupo: fonte, resolução, áudio, idioma, id.
const NOT_A_GROUP: &str = r"(?:WEB-(?:DL|Rip)|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|-ENG|-JAP|-GER|-FRA|-FRE|-ITA|-HDRip|\d{1,2}-bit|[ ._]\d{4}-\d{2}|-\d{2}|tmdb(?:id)?-\d+|tt\d{7,8})";

/// A referência tem `(?<!X(?:\k<part2>)?)` depois do grupo: nem "termina em
/// X" nem "termina em X seguido da segunda parte". `fancy-regex` não aceita
/// backreference em lookbehind de tamanho variável, então a segunda condição
/// vira um lookbehind antes da segunda parte — que é o mesmo lugar.
static RELEASE_GROUP: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"(?i)-(?<releasegroup>[a-z0-9]+(?:(?<!{NOT_A_GROUP})(?<part2>-[a-z0-9]+))?(?!.+?(?:480p|576p|720p|1080p|2160p)))(?<!{NOT_A_GROUP})(?:\b|[-._ ]|$)|[-._ ]\[(?<bracketed>[a-z0-9]+)\]$"
    ))
});

static INVALID_RELEASE_GROUP: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)^([se]\d+|[0-9a-f]{8})$"));

static ANIME_RELEASE_GROUP: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)^(?:\[(?<subgroup>(?!\s).+?(?<!\s))\](?:_|-|\s|\.)?)"));

static EXCEPTION_EXACT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(?<releasegroup>KRaLiMaRKo|E\.N\.D|D\-Z0N3|Koten_Gars|BluDragon|ZØNEHD|HQMUX|VARYG|YIFY|YTS(.(MX|LT|AG))?|TMd|Eml HDTeam|LMain|DarQ|BEN THE MEN|TAoE|QxR|126811)\b",
    )
});

static EXCEPTION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?<=[._ \[])(?<releasegroup>(Silence|afm72|Panda|Ghost|MONOLITH|Tigole|Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|GiLG|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|RZeroX|FreetheFish|Anna|Bandi|Qman|theincognito|HDO|DusIctv|DHD|CtrlHD|-ZR-|ADC|XZVN|RH|Kametsu|Celdra)(?=\]|\)))",
    )
});

static CLEAN_RELEASE_GROUP: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(-(RP|1|NZBGeek|Obfuscated|Obfuscation|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$",
    )
});

/// O que termina em "-X" e não é grupo, na lista do gerenciador de séries.
const EPISODE_NOT_A_GROUP: &str = r"(?:WEB-DL|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|[ ._]\d{4}-\d{2}|-\d{2})";

/// Como `RELEASE_GROUP`, com a lista de séries.
static EPISODE_RELEASE_GROUP: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"(?i)-(?<releasegroup>[a-z0-9]+(?:(?<!{EPISODE_NOT_A_GROUP})(?<part2>-[a-z0-9]+))?(?!.+?(?:480p|576p|720p|1080p|2160p)))(?<!{EPISODE_NOT_A_GROUP})(?:\b|[-._ ]|$)|[-._ ]\[(?<bracketed>[a-z0-9]+)\]$"
    ))
});

static EPISODE_EXCEPTION_EXACT: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?<releasegroup>(?:D\-Z0N3|Fight-BB|VARYG|E\.N\.D|KRaLiMaRKo|BluDragon|DarQ|KCRT|BEN[_. ]THE[_. ]MEN)\b)",
    )
});

static EPISODE_EXCEPTION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)(?<=[._ \[])(?<releasegroup>(Silence|afm72|Panda|Ghost|MONOLITH|Tigole|Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|RZeroX)(?=\]|\)))",
    )
});

/// Também tira o "S01E02" e o que vem antes dele.
static EPISODE_CLEAN_RELEASE_GROUP: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)^(.*?[-._ ](S\d+E\d+)[-._ ])|(-(RP|1|NZBGeek|Obfuscated|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$",
    )
});

/// Grupo de release do título, se houver um que se possa afirmar.
#[must_use]
pub fn parse_release_group(title: &str) -> Option<String> {
    let title = remove_file_extension(title.trim());
    let title = strip_website_prefix(&title);
    let title = strip_torrent_suffix(&title);

    if let Some(anime) = captures(&ANIME_RELEASE_GROUP, &title) {
        return group(&anime, "subgroup").map(str::to_owned);
    }

    let title = replace_all(&CLEAN_RELEASE_GROUP, &title, "");

    for exception in [&*EXCEPTION_EXACT, &*EXCEPTION] {
        if let Some(found) = last_captures(exception, &title) {
            return group(&found, "releasegroup").map(str::to_owned);
        }
    }

    let found = last_captures(&RELEASE_GROUP, &title)?;
    let name = group(&found, "releasegroup").or_else(|| group(&found, "bracketed"))?;
    if name.parse::<i64>().is_ok() || is_match(&INVALID_RELEASE_GROUP, name) {
        return None;
    }
    Some(name.to_owned())
}

/// Grupo de release de um título de episódio. O gerenciador de séries tem
/// listas e ordem próprias (exceção antes da exata, outra limpeza de sufixo,
/// e as substituições de anime antes de tudo), então não dá para reusar
/// `parse_release_group`.
#[must_use]
pub fn parse_episode_release_group(title: &str) -> Option<String> {
    let title = remove_episode_file_extension(title.trim());
    let title = pre_substitute(&title, true);
    let title = strip_website_prefix(&title);
    let title = strip_torrent_suffix(&title);

    if let Some(anime) = captures(&ANIME_RELEASE_GROUP, &title) {
        return group(&anime, "subgroup").map(str::to_owned);
    }

    let title = replace_all(&EPISODE_CLEAN_RELEASE_GROUP, &title, "");

    for exception in [&*EPISODE_EXCEPTION, &*EPISODE_EXCEPTION_EXACT] {
        if let Some(found) = last_captures(exception, &title) {
            return group(&found, "releasegroup").map(str::to_owned);
        }
    }

    let found = last_captures(&EPISODE_RELEASE_GROUP, &title)?;
    let name = group(&found, "releasegroup").or_else(|| group(&found, "bracketed"))?;
    if name.parse::<i64>().is_ok() || is_match(&INVALID_RELEASE_GROUP, name) {
        return None;
    }
    Some(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grupos() {
        for (title, expected) in [
            (
                "Movie.2020.1080p.BluRay.DTS-HD.MA.x264-GRUPO",
                Some("GRUPO"),
            ),
            ("Movie.2020.1080p.WEB-DL-GRUPO", Some("GRUPO")),
            ("Movie 2020 1080p-GR-UPO", Some("GR-UPO")),
            ("Movie.2020.720p [ABC]", Some("ABC")),
            ("Movie.2020.1080p.WEB-DL", None),
            ("Movie.2020.1080p.x264-GRUPO-Obfuscated", Some("GRUPO")),
            ("[Sub] Movie 2020", Some("Sub")),
            ("Movie.2020.1080p.YTS.MX", Some("YTS.MX")),
            ("Movie.2020.1080p-1234", None),
        ] {
            assert_eq!(parse_release_group(title).as_deref(), expected, "{title}");
        }
    }
}

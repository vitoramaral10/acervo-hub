//! A tabela `ReportTitleRegex` do gerenciador de séries, na mesma ordem, e as
//! substituições que rodam antes dela.
//!
//! Os padrões saem do original com duas mudanças mecânicas. Nome de grupo
//! repetido ganha sufixo (`episode`, `episode_2`): o porte junta os dois sob o
//! nome-base. E o grupo que se repete com captura dentro (`(?:...)+`) é
//! marcado com `⟦ ⟧`: o .NET guarda todas as capturas de cada repetição, e o
//! `fancy-regex` só a última, então o leitor refaz as repetições (ver
//! `repeat.rs`).

pub(super) const REPORT_TITLE: [&str; 98] = [
    // Diário com ano no título e hora depois da data (formato do Plex DVR).
    r"^^(?<title>.+?\((?<titleyear>\d{4})\))[-_. ]+(?<airyear>19[4-9]\d|20\d\d)(?<sep>[-_]?)(?<airmonth>0\d|1[0-2])\k<sep>(?<airday>[0-2]\d|3[01])[-_. ]\d{2}[-_. ]\d{2}[-_. ]\d{2}",
    // Diário sem título (2018-10-12, 20181012), padrão estrito para não casar à toa.
    r"^(?<airyear>19[6-9]\d|20\d\d)(?<sep>[-_]?)(?<airmonth>0\d|1[0-2])\k<sep>(?<airday>[0-2]\d|3[01])(?!\d)",
    // Multiparte sem título (S01E05.S01E06).
    r"^⟦\W*S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))⟦e{1,2}(?<episode>\d{1,3}(?!\d+))⟧+⟧{2,}",
    // Multiparte sem título (1x05.1x06).
    r"^⟦\W*(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))⟦x{1,2}(?<episode>\d{1,3}(?!\d+))⟧+⟧{2,}",
    // Sem título, múltiplo (S01E04E05, 1x04x05).
    r"^(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))⟦(?:[-_]|[ex]){1,2}(?<episode>\d{2,3}(?!\d+))⟧{2,})",
    // Episódio dividido (S01E05a, S01E05b).
    r"^(?<title>.+?)(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?[ex])(?<episode>\d{2,3}(?!\d+))(?<splitepisode>[a-d])(?:[ _.])))",
    // Sem título, único (S01E05, 1x05).
    r"^(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?[ex])(?<episode>\d{2,3}(?!\d+))))",
    // Anime: [Grupo] Título absoluto (Temporada+Episódio).
    r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+))(?:[-_. ])+\((?:S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))))(?:v\d+)?(?:\)(?!\d+)).*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: [Grupo] Título absoluto - Temporada+Episódio.
    r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+))(?:[-_. ](?<![()\[!]))+(?:S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:(?:[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))))(?:v\d+)?(?:[_. ](?!\d+)).*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: [Grupo] Título Temporada+Episódio.
    r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)(?:[-_\W](?<![()\[!]))+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))⟧+)(?:v\d+)?(?:[_. ](?!\d+)).*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: [Grupo] Título Episode 01.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>.+?)[-_. ]+?(?:Episode)⟦[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+))⟧+.*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: [Grupo] Título absoluto + Temporada+Episódio.
    r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)⟦(?:[-_\W](?<![()\[!]))+(?<absoluteepisode>\d{2,3}(\.\d{1,2})?)⟧+(?:_|-|\s|\.)+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:\-|[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))⟧+).*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.)",
    // Anime: [Grupo] Título Temporada+Episódio + absoluto.
    r"^(?:\[(?<subgroup>.+?)\](?:_|-|\s|\.)?)(?<title>.+?)(?:[-_\W](?<![()\[!]))+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:\-|[ex]|\W[ex]){1,2}(?<episode>\d{2}(?!\d+))⟧+)⟦(?:_|-|\s|\.)+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+|\-[a-z]))⟧+.*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: [Grupo] Título com número no fim, lote separado por til.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?[^-]+?)(?:(?<![-_. ]|\b[0]\d+) - )[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))\s?~\s?(?<absoluteepisode_2>\d{2,3}(\.\d{1,2})?(?!\d+))(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título com a temporada entre parênteses e absoluto.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^-]+?)[_. ]+?\(Season[_. ](?<season>\d+)\)[-_. ]+?⟦[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título com número de 3 dígitos no fim e subtítulo, absoluto.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^]]+?)⟦[-_. ]{3}?(?<absoluteepisode>\d{2}(\.\d{1,2})?(?!-?\d+|-[a-z]+))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título com número no fim, absoluto.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^-]+?)(?:(?<![-_. ]|\b[0]\d+) - )⟦[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título com número no fim e S## (temporada inteira).
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)[-_. ]+(?:S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?![ex]?\d+))).+?(?:$|\.mkv)",
    // Anime: [Grupo] Título com número no fim, absoluto.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>[^-]+?)(?:(?<![-_. ]|\b[0]\d+)[_ ]+)⟦[-_. ]?(?<absoluteepisode>\d{3}(\.\d{1,2})?(?!\d+|-[a-z]+))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título - absoluto.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)(?:(?<!\b[0]\d+))⟦[. ]-[. ](?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título absoluto - absoluto (lote sem separador pleno entre título e número).
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)(?:(?<!\b[0]\d+))(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))[. ]-[. ](?<absoluteepisode_2>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Anime: [Grupo] Título absoluto.
    r"^\[(?<subgroup>.+?)\][-_. ]?(?<title>.+?)[-_. ]+\(?⟦[-_. ]?#?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|-[a-z]+))⟧+\)?(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])?(?:$|\.mkv)",
    // Multiepisódio repetido (S01E05 - S01E06).
    r"^(?<title>.+?)⟦(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))⟦(?:e|[-_. ]e){1,2}(?<episode>\d{1,3}(?!\d+))⟧+⟧{2,}",
    // Multiepisódio repetido (1x05 - 1x06).
    r"^(?<title>.+?)⟦(?:[-_\W](?<![()\[!]))+(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))⟦x{1,2}(?<episode>\d{1,3}(?!\d+))⟧+⟧{2,}",
    // Único com título (S01E05) seguido de ".5 [SP]".
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+|(?:[ex]|\W[ex]|_|-){1,2}\d+))(?<special>\.5[ .]\[SP\]))",
    // Único com título (S01E05) e informação entre colchetes depois.
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+|(?:[ex]|\W[ex]|_|-){1,2}\d+))).+?(?:\[.+?\])(?!\\)",
    // Anime: Título Temporada Episódio + absoluto [Grupo].
    r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:[ex]|\W[ex]|-){1,2}(?<episode>(?<!\d+)\d{2}(?!\d+))⟧+)[-_. (]+?⟦[-_. ]?(?<absoluteepisode>(?<!\d+)\d{3}(\.\d{1,2})?(?!\d+|[pi]))⟧+.+?\[(?<subgroup>.+?)\](?:$|\.mkv)",
    // Multiepisódio com título (S01E05E06, S01E05-06, S01E05 E06) e informação entre colchetes depois.
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]|_){1,2}(?<episode>\d{2,3}(?!\d+))⟦(?:\-|[ex]|\W[ex]|_){1,2}(?<episode_2>\d{2,3}(?!\d+))⟧+).+?(?:\[.+?\])(?!\\)",
    // Anime: Título Episode absoluto [Grupo] [Hash]?.
    r"^(?<title>.+?)[-_. ]Episode⟦[-_. ]+(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))⟧+(?:.+?)\[(?<subgroup>.+?)\].*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título absoluto [Grupo] [Hash].
    r"^(?<title>.+?)⟦(?:_|-|\s|\.)+(?<absoluteepisode>\d{3}(\.\d{1,2})(?!\d+))⟧+(?:.+?)\[(?<subgroup>.+?)\].*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título absoluto (Ano) [Grupo].
    r"^(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2}(?!\d+))[-_. ](\(\d{4}\))[-_. ]\[(?<subgroup>.+?)\]",
    // Anime: Título com número no fim, absoluto e hash.
    r"^(?<title>[^-]+?)(?:(?<![-_. ]|\b[0]\d+) - )⟦[-_. ]?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?.*?(?<hash>[(\[]\w{8}[)\]])(?:$|\.mkv)",
    // Anime: Título absoluto [Hash].
    r"^(?<title>.+?)⟦(?:_|-|\s|\.)+(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))⟧+(?:[-_. ]+(?<special>special|ova|ovd))?[-_. ]+.*?(?<hash>[(\[]\w{8}[)\]])$",
    // Data de exibição E temporada/episódio: fica só com temporada/episódio.
    r"^(?<title>.+?)?\W*(?<airdate>\d{4}\W+[0-1][0-9]\W+[0-3][0-9])(?!\W+[0-3][0-9])[-_. ](?:s?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+)))(?:[ex](?<episode>(?<!\d+)(?:\d{1,3})(?!\d+)))",
    // Data de exibição E temporada/episódio.
    r"^(?<title>.+?)?\W*(?<airyear>\d{4})\W+(?<airmonth>[0-1][0-9])\W+(?<airday>[0-3][0-9])(?!\W+[0-3][0-9]).+?(?:s?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+)))(?:[ex](?<episode>(?<!\d+)(?:\d{1,3})(?!\d+)))",
    // Episódio absoluto E data de exibição (TJET Wrestling).
    r"^(?<title>.+?)?[-_. ](?:e\d{2,3}(?!\d+))[-_. ](?<airyear>\d{4})\W+(?<airmonth>[0-1][0-9])\W+(?<airday>[0-3][0-9])(?!\W+[0-3][0-9]).+?(?:\[[a-z]+\])",
    // Vários títulos e temporada/episódio depois do último (Título1 / Título2 / S1E1-2 of 6).
    r"^⟦(?<title>.*?)[ ._]\/[ ._]⟧+\(?S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)?E?[ ._]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))(?:-(?<episode_2>(?<!\d+)\d{1,2}(?!\d+)))?(?:[ ._]of[ ._](?<episodecount>\d{1,2}))?\)?[ ._][\(\[]",
    // Multiepisódio com título (S01E99-100, S01E05-06).
    r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))E(?<episode>\d{2,3}(?!\d+))⟦-(?<episode_2>\d{2,3}(?!\d+))⟧+(?:[-_. ]|$)",
    // Multiepisódio com título (S01E05-06, S01E05-6).
    r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))E(?<episode>\d{1,2}(?!\d+))⟦-(?<episode_2>\d{1,2}(?!\d+))⟧+(?:[-_. ]|$)",
    // Com título, único (S01E05, 1x05) e múltiplo (S01E05E06, S01E05-06, S01E05 E06).
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[ex]|\W[ex]){1,2}(?<episode>\d{2,3}(?!\d+))⟦(?:\-|[ex]|\W[ex]|_){1,2}(?<episode_2>\d{2,3}(?!\d+))⟧*)(?:[-_. ]|$)",
    // Com título, temporada de 4 dígitos (S2016E05, S2016E05E06, S2016E05-06).
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{4})(?!\d+))(?:e|\We|_){1,2}(?<episode>\d{2,4}(?!\d+))⟦(?:\-|e|\We|_){1,2}(?<episode_2>\d{2,3}(?!\d+))⟧*)\W?(?!\\)",
    // Com título, temporada de 4 dígitos (2016x05, 2016x05x06, 2016x05-06).
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]))+(?<season>(?<!\d+)(?:\d{4})(?!\d+))(?:x|\Wx){1,2}(?<episode>\d{2,4}(?!\d+))⟦(?:\-|x|\Wx|_){1,2}(?<episode_2>\d{2,3}(?!\d+))⟧*)\W?(?!\\)",
    // Com título, único (s01.05).
    r"^(?<title>.+?)(?:[-_\W](?<![()\[!]))+S(?<season>(?<!\d+)(?:\d{2})(?!\d+))(?:\.)(?<episode>\d{2,3}(?!\d+))(?:[-_. ]|$)",
    // Pacote de várias temporadas.
    r"^(?<title>.+?)(Complete Series)?[-_. ]+(?:S|(?:Season|Saison|Series|Stagione)[_. ])(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:[-_. ]{1}|[-_. ]{3})(?:S|(?:Season|Saison|Series|Stagione)[_. ])?(?<season_2>(?<!\d+)(?:\d{1,2})(?!\d+))",
    // Pacote parcial de temporada.
    r"^(?<title>.+?)(?:\W+S(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))\W+⟦(?:(?:Part|Vol)\W?|(?<!\d+\W+)e|p)(?<seasonpart>\d{1,2}(?!\d+))⟧+)",
    // Anime: absoluto de 4 dígitos.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>.+?)[-_. ]+?(?<absoluteepisode>\d{4}(\.\d{1,2})?(?!\d+))",
    // Anime: Título absoluto de 4 dígitos [Grupo].
    r"^(?<title>.+?)[-_. ]+(?<absoluteepisode>(?<!\d+)\d{4}(?!\d+))[-_. ]\[(?<subgroup>.+?)\]",
    // Minissérie com ano no título, temporada 1, episódios Part01, Part 01, Part.1.
    r"^(?<title>.+?\d{4})(?:\W+⟦(?:Part\W?|e)(?<episode>\d{1,2}(?!\d+))⟧+)",
    // Minissérie, temporada 1, vários episódios E1-E2.
    r"^(?<title>.+?)(?:[-._ ][e])(?<episode>\d{2,3}(?!\d+))⟦(?:\-?[e])(?<episode_2>\d{2,3}(?!\d+))⟧+",
    // Data de exibição e parte (2018.04.28.Part.2).
    r"^(?<title>.+?)?\W*(?<airyear>\d{4})[-_. ]+(?<airmonth>[0-1][0-9])[-_. ]+(?<airday>[0-3][0-9])(?![-_. ]+[0-3][0-9])[-_. ]+Part[-_. ]?(?<part>[1-9])",
    // Minissérie, temporada 1, episódios Part01, Part 01, Part.1.
    r"^(?<title>.+?)(?:\W+⟦(?:(?<!\()Part\W?|(?<!\d+\W+)e)(?<episode>\d{1,2}(?!\d+|\)))⟧+)",
    // Minissérie, temporada 1, episódios Part One/Two/.../Nine.
    r"^(?<title>.+?)(?:\W+(?:Part[-._ ](?<episode>One|Two|Three|Four|Five|Six|Seven|Eight|Nine)(?>[-._ ])))",
    // Minissérie, temporada 1, episódios XofY.
    r"^(?<title>.+?)(?:\W+⟦(?<episode>(?<!\d+)\d{1,2}(?!\d+))of\d+⟧+)",
    // Season 01 Episode 03.
    r#"(?:.*(?:\"|^))(?<title>.*?)(?:[-_\W](?<![()\[]))+(?:\W?Season\W?)(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)+(?:Episode\W)⟦[-_. ]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))⟧+"#,
    // Multiepisódio entre colchetes (Título [S01E11E12] ou [S01E11-12]).
    r"(?:.*(?:^))(?<title>.*?)[-._ ]+\[S(?<season>(?<!\d+)\d{2}(?!\d+))⟦[E-]{1,2}(?<episode>(?<!\d+)\d{2}(?!\d+))⟧+\]",
    // Multiepisódio entre parênteses (Título (S01E11E12) ou (S01E11-12)).
    r"(?:.*(?:^))(?<title>.*?)[-._ ]+\(S(?<season>(?<!\d+)\d{2}(?!\d+))⟦[E-]{1,2}(?<episode>(?<!\d+)\d{2}(?!\d+))⟧+\)",
    // Multiepisódio sem espaço entre título e temporada (S01E11E12).
    r"(?:.*(?:^))(?<title>.*?)S(?<season>(?<!\d+)\d{2}(?!\d+))⟦E(?<episode>(?<!\d+)\d{2}(?!\d+))⟧+",
    // Multiepisódio com números simples (S6.E1-E2, S6.E1E2, S6E1E2).
    r"^(?<title>.+?)[-_. ]S(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))⟦[-_. ]?[ex]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))⟧+",
    // Vários títulos, cada um seguido de temporada e episódio entre parênteses.
    r"^(?<title>.*?)[ ._]\(S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)?E?[ ._]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))(?:-(?<episode_2>(?<!\d+)\d{1,2}(?!\d+)))?\)(?:[ ._]\/[ ._])(?<title_2>.*?)[ ._]\(",
    // Único: S1E1, S1-E1, S1.Ep1, S01.Ep.01.
    r#"(?:.*(?:\"|^))(?<title>.*?)(?:\W?|_)S(?<season>(?<!\d+)\d{1,2}(?!\d+))(?:\W|_)?Ep?[ ._]?(?<episode>(?<!\d+)\d{1,2}(?!\d+))"#,
    // Temporada de 3 dígitos (S010E05).
    r#"(?:.*(?:\"|^))(?<title>.*?)(?:\W?|_)S(?<season>(?<!\d+)\d{3}(?!\d+))(?:\W|_)?E(?<episode>(?<!\d+)\d{1,2}(?!\d+))"#,
    // Episódio de 5 dígitos com título.
    r"^(?:(?<title>.+?)(?:_|-|\s|\.)+)(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+)))(?:(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>(?<!\d+)\d{5}(?!\d+)))",
    // Multiepisódio de 5 dígitos com título.
    r"^(?:(?<title>.+?)(?:_|-|\s|\.)+)(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+)))⟦(?:[-_. ]{1,3}ep){1,2}(?<episode>(?<!\d+)\d{5}(?!\d+))⟧+",
    // Temporada e episódio separados (S01 - E01).
    r"^(?<title>.+?)(?:_|-|\s|\.)+S(?<season>\d{2}(?!\d+))(\W-\W)E(?<episode>(?<!\d+)\d{2}(?!\d+))(?!\\)",
    // Temporada e episódio entre colchetes ([02x01], [02x01x02]).
    r"^(?<title>.+?)?(?:[-_\W](?<![()\[!]))+\[(?:s)?(?<season>(?<!\d+)\d{1,2})(?:(?:[ex])(?<episode>\d{2}))⟦(?:[-ex]){1,2}(?<episode_2>\d{2})⟧*\].+?(?:\.|$)",
    // Anime: Título com temporada - absoluto (Título S01 - EP14).
    r"^(?<title>.+?S\d{1,2})[-_. ]{3,}(?:EP)?(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+|[-]))",
    // Anime: títulos franceses com episódio único ([Grupo] Título - Episode 1).
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)[-_. ]+?(?:Episode[-_. ]+?)(?<absoluteepisode>\d{1}(\.\d{1,2})?(?!\d+))",
    // Anime: absoluto entre colchetes.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>.+?)[-_. ]+?\[(?<absoluteepisode>\d{2,3}(\.\d{1,2})?(?!\d+))\]",
    // Programas de variedades japoneses com data na frente.
    r"^(?<airyear>\d{2})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9])(?![-_. ]+[0-3][0-9])[-_. ](?<title>.+?)[-_. ](?:Season[-_. ]?(?<season>\d{1,2})[-_. ])?(?:ep|#)(?<episode>\d{2,3})",
    // Só temporada, seguida de ano.
    r"^(?<title>.+?)[-_. ]+?(?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{1,2}(?=[-_. ]\d{4}[-_. ]+))(?<extras>EXTRAS|SUBPACK)?(?!\\)",
    // Só temporada.
    r"^(?<title>.+?)[-_. ]+?(?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{1,2}(?![-_. ]?\d+))(?:[-_. ]|$)+(?<extras>EXTRAS|SUBPACK)?(?!\\)",
    // Só temporada de 4 dígitos.
    r"^(?<title>.+?)[-_. ]+?(?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{4}(?![-_. ]?\d+))(\W+|_|$)(?<extras>EXTRAS|SUBPACK)?(?!\\)",
    // Trackers espanhóis.
    r"^(?<title>.+?)(?:(?:[-_. ]+?Temporada.+?|\[.+?\])\[Cap)⟦[-_. ]+(?<season>(?<!\d+)\d{1,2})(?<episode>(?<!e|x)(?:[1-9][0-9]|[0][1-9]))⟧+(?:\])",
    // Numeração 103/113.
    r"^(?<title>.+?)?⟦(?:[_.-](?<![()\[!]))+(?<season>(?<!\d+)[1-9])(?<episode>[1-9][0-9]|[0][1-9])(?![a-z]|\d+)⟧+(?:[_.]|$)",
    // Episódio de 4 dígitos sem título, único e múltiplo.
    r"^(?:S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>\d{4}(?!\d+|i|p))⟧+)(\W+|_|$)(?!\\)",
    // Episódio de 4 dígitos com título, único e múltiplo.
    r"^(?<title>.+?)(?:(?:[-_\W](?<![()\[!]|\d{1,2}-))+S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:\-|[ex]|\W[ex]|_){1,2}(?<episode>\d{4}(?!\d+|i|p))⟧+)\W?(?!\\)",
    // Data de exibição (2018.04.28).
    r"^(?<title>.+?)?\W*(?<airyear>\d{4})[-_. ]+(?<airmonth>[0-1][0-9])[-_. ]+(?<airday>[0-3][0-9])(?![-_. ]+[0-3][0-9])",
    // Trackers turcos (01 BLM, 3. Blm, 04.Bolum).
    r"^(?<title>.+?)[_. ](?<absoluteepisode>\d{1,4})(?:[_. ]+)(?:BLM|B[oö]l[uü]m)",
    // Data de exibição (04.28.2018).
    r"^(?<title>.+?)?\W*(?<ambiguousairmonth>[0-1][0-9])[-_. ]+(?<ambiguousairday>[0-3][0-9])[-_. ]+(?<airyear>\d{4})(?!\d+)",
    // Data de exibição (28.04.2018).
    r"^(?<title>.+?)?\W*(?<ambiguousairday>[0-3][0-9])[-_. ]+(?<ambiguousairmonth>[0-1][0-9])[-_. ]+(?<airyear>\d{4})(?!\d+)",
    // Data de exibição (20180428).
    r"^(?<title>.+?)?\W*(?<!\d+)(?<airyear>\d{4})(?<airmonth>[0-1][0-9])(?<airday>[0-3][0-9])(?!\d+)",
    // Anime: Título absoluto (E195 ou E1206).
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)⟦(?:_|-|\s|\.)+(?:e|ep)(?<absoluteepisode>(\d{3}|\d{4})(\.\d{1,2})?)⟧+[-_. ].*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Numeração 1103/1113.
    r"^(?<title>.+?)?⟦(?:[-_. ](?<![()\[!]))*(?<!\d{1,2}-)(?<season>(?<!\d+|\(|\[|e|x)\d{2})(?<episode>(?<!e|x)(?:[1-9][0-9]|[0][1-9])(?!p|i|\d+|\)|\]|\W\d+|\W(?:e|ep|x)\d+))⟧+([-_. ]+|$)(?!\\)",
    // Títulos holandeses/flamengos.
    r"^(?<title>.+?)[-_. ](?:Se\.(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?afl\.)(?<episode>\d{1,3}(?!\d+))⟦(?:[-]|[-_ ]en[-_ ])(?<episode_2>\d{1,3}(?!\d+))⟧*))",
    // Episódio de um dígito (S01E1, S01E5E6).
    r"^(?<title>.*?)⟦(?:[-_\W](?<![()\[!]))+S?(?<season>(?<!\d+)\d{1,2}(?!\d+))⟦(?:\-|[ex]){1,2}(?<episode>\d{1})⟧+⟧+(\W+|_|$)(?!\\)",
    // iTunes: Season 1\05 Título (Qualidade).ext.
    r"^(?:Season(?:_|-|\s|\.)(?<season>(?<!\d+)\d{1,2}(?!\d+)))(?:_|-|\s|\.)(?<episode>(?<!\d+)\d{1,2}(?!\d+))",
    // iTunes: 1-05 Título (Qualidade).ext.
    r"^(?:(?<season>(?<!\d+)(?:\d{1,2})(?!\d+))(?:-(?<episode>\d{2,3}(?!\d+))))",
    // Anime, intervalo: Título absoluto (ep01-12).
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:_|\s|\.)+(?:e|ep)(?<absoluteepisode>\d{2,3}(\.\d{1,2})?)-(?<absoluteepisode_2>(?<!\d+)\d{1,2}(\.\d{1,2})?(?!\d+|-)).*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título absoluto (e66).
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)⟦(?:_|-|\s|\.)+(?:e|ep)(?<absoluteepisode>\d{2,4}(\.\d{1,2})?)⟧+[-_. ].*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título Episode absoluto.
    r"^(?<title>.+?)[-_. ](?:Episode)⟦[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+))⟧+.*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime, intervalo de 1 ou 2 dígitos (1-10).
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)[_. ]+(?<absoluteepisode>(?<!\d+)\d{1,2}(\.\d{1,2})?(?!\d+))-(?<absoluteepisode_2>(?<!\d+)\d{1,2}(\.\d{1,2})?(?!\d+|-)).*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título Episode/Episodio absoluto.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)[-_. ]+(?:Episode|Episodio)⟦[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,4}(\.\d{1,2})?(?!\d+|[ip]))⟧+.*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título [absoluto] da AniLibriaTV.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)(?:[-_. ]\[)⟦(?:-?)(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+|[ip]))⟧+(?:\][-_. ]).*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título absoluto.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)⟦[-_. ]+(?<absoluteepisode>(?<!\d+)\d{2,4}(\.\d{1,2})?(?!\d+|[ip]))⟧+.*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Anime: Título {absoluto}.
    r"^(?:\[(?<subgroup>.+?)\][-_. ]?)?(?<title>.+?)⟦(?:[-_\W](?<![()\[!]))+(?<absoluteepisode>(?<!\d+)\d{2,3}(\.\d{1,2})?(?!\d+|[ip]))⟧+.*?(?<hash>[(\[]\w{8}[)\]])?$",
    // Multiepisódio terrível do extant (extant.10708.hdtv-lol.mp4).
    r"^(?<title>.+?)[-_. ](?<season>[0]?\d?)(?:⟦(?<episode>\d{2})⟧{2}(?!\d+))[-_. ]",
    // Só temporada, para anime mal nomeado.
    r"^(?:\[(?<subgroup>.+?)\][-_. ])?(?<title>.+?)[-_. ]+?[\[(](?:S|Season|Saison|Series|Stagione)[-_. ]?(?<season>\d{1,2}(?![-_. ]?\d+))(?:[-_. )\]]|$)+(?<extras>EXTRAS|SUBPACK)?(?!\\)",
    // Sem título, episódio de um dígito (S1E1, 1x1).
    r"^(?:S?(?<season>(?<!\d+)(?:\d{1,2}|\d{4})(?!\d+))(?:(?:[-_ ]?[ex])(?<episode>\d{1}(?!\d+))))",
];

/// Substituições que rodam antes da tabela: normalizam releases de anime chinês,
/// coreano e espanhol para os formatos que a tabela entende. Cada uma traz o
/// padrão e o molde da troca (`${grupo}`).
pub(super) const PRE_SUBSTITUTION: [(&str, &str); 12] = [
    (r"\.E(\d{2,4})\.\d{6}\.(.*-NEXT)$", r".S01E$1.$2"),
    (
        r"^\[(?:(?<subgroup>[^\]]+?)(?:[\u4E00-\u9FCC]+)?)\]\[(?<title>[^\]]+?)(?:\s(?<chinesetitle>[\u4E00-\u9FCC][^\]]*?))\]\[(?:(?:[\u4E00-\u9FCC]+?)?(?<episode>\d{1,4})(?:[\u4E00-\u9FCC]+?)?)\]",
        r"[${subgroup}] ${title} - ${episode} - ",
    ),
    (
        r"^\[(?<subgroup>[^\]]*?(?:LoliHouse|ZERO|Lilith-Raws|Skymoon-Raws|orion origin)[^\]]*?)\](?<title>[^\[\]]+?)(?: - (?<episode>[0-9-]+)\s*|\[第?(?<episode_2>[0-9]+(?:-[0-9]+)?)话?(?:END|完)?\])\[",
        r"[${subgroup}][${title}][${episode}][",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:\s?★[^\[ -]+\s?)?\[?(?:(?<chinesetitle>(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*?)(?:\]\[|\s*[_/·]\s*)){0,2}(?<title>[^\[\]]+?)(?:\s(?:S?(?<!\d+)((0)(?<season>\d)|(?<season_2>[1-9]\d))(?!\d+)))\]?(?:\[\d{4}\])?\[第?(?<episode>[0-9]+(?:-[0-9]+)?)(?:话|集)?(?: ?END|完| ?Fin)?\]",
        r"[${subgroup}] ${title} S${season} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\]\[(?<chinesetitle>(?<![^a-zA-Z0-9])[^a-zA-Z0-9]+)(?<title>[^\]]+?)\](?:\[\d{4}\])?\[第?(?<episode>[0-9]+(?:-[0-9]+)?)(?:话|集)?(?: ?END|完| ?Fin)?\]",
        r"[${subgroup}] ${title} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:\s?★[^\[ -]+\s?)?\[?(?:(?<chinesetitle>(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*?)(?:\]\[|\s*[_/·]\s*)){0,2}(?<title>[^\]]+?)\]?(?:\[\d{4}\])?\[第?(?<episode>[0-9]{1,4}(?:-[0-9]{1,4})?)(?:话|集)?(?: ?END|完| ?Fin)?\]",
        r"[${subgroup}] ${title} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<chinesetitle>(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*?)(?:\s/\s))(?<title>[^\[\]]+?)(?:\s(?:S?(?<!\d+)((0)(?<season>\d)|(?<season_2>[1-9]\d))(?!\d+)))(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?)话?(?:END|完)?",
        r"[${subgroup}] ${title} S${season} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<title>[^\]]+?)(?:\s/\s))(?<chinesetitle>(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*?)(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?(?![a-z]))话?(?:END|完)?",
        r"[${subgroup}] ${title} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<chinesetitle>(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*?)(?:\s/\s))(?<title>[^\]]+?)(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?(?![a-z]))话?(?:END|完)?",
        r"[${subgroup}] ${title} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:(?<chinesubgroup>\[(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*\])+)\[(?<title>[^\]]+?)\](?<junk>\[[^\]]+\])*\[(?<episode>[0-9]+(?:-[0-9]+)?)( END| Fin)?\]",
        r"[${subgroup}] ${title} - ${episode} ",
    ),
    (
        r"^\[(?<subgroup>[^\]]+)\](?:\s)(?:(?<chinesetitle>(?=[^\]]*?[\u4E00-\u9FCC])[^\]]*?)(?:\s\|\s))(?<title>[^\]]+?)(?:[- ]+)(?<episode>[0-9]+(?:-[0-9]+)?(?![a-z]))话?(?:END|完)?",
        r"[${subgroup}] ${title} - ${episode} ",
    ),
    (
        r"^(?<title>.+?(?=[ ._-]\()).+?\((?<year>\d{4})\/(?<info>S[^\/]+)",
        r"${title} (${year}) - ${info} ",
    ),
];

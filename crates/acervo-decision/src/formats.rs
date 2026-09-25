//! Formatos personalizados: regras sobre o nome, o grupo, a edição, os
//! idiomas, a fonte, a resolução, o tamanho e as flags de um release (ou de
//! um arquivo), e a nota que cada perfil dá a cada formato.
//!
//! A regra de casamento é a da referência: dentro de um formato, toda
//! especificação obrigatória tem de casar, e de cada tipo de especificação
//! com não obrigatórias, pelo menos uma. `negate` inverte o resultado da
//! especificação antes disso.

use acervo_parser::{Language, Modifier, Quality, Source};
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};

/// O que uma especificação testa. Nomes e valores em português, como a tela
/// e o banco os guardam.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tipo", rename_all = "snake_case")]
pub enum Rule {
    /// Expressão regular sobre o nome do release (ou do arquivo).
    Titulo { valor: String },
    /// Expressão regular sobre o grupo.
    Grupo { valor: String },
    /// Expressão regular sobre a edição.
    Edicao { valor: String },
    /// Idioma presente; `Original` é o idioma original do filme e `Any`
    /// casa com qualquer um.
    Idioma { valor: String },
    /// `cam`, `telesync`, `telecine`, `workprint`, `dvd`, `tv`, `webdl`,
    /// `webrip` ou `bluray`.
    Fonte { valor: String },
    /// 480, 720, 1080, 2160...
    Resolucao { valor: u16 },
    /// `regional`, `screener`, `rawhd`, `brdisk` ou `remux`.
    Modificador { valor: String },
    /// Em gigabytes; o máximo é inclusivo.
    Tamanho { minimo: f64, maximo: f64 },
    /// Bit de flag do indexador (1 freeleech, 2 halfleech, 4 upload dobrado,
    /// 8 golden, 16 aprovado, 32 internal...).
    Flag { valor: u32 },
}

impl Rule {
    fn kind(&self) -> &'static str {
        match self {
            Self::Titulo { .. } => "titulo",
            Self::Grupo { .. } => "grupo",
            Self::Edicao { .. } => "edicao",
            Self::Idioma { .. } => "idioma",
            Self::Fonte { .. } => "fonte",
            Self::Resolucao { .. } => "resolucao",
            Self::Modificador { .. } => "modificador",
            Self::Tamanho { .. } => "tamanho",
            Self::Flag { .. } => "flag",
        }
    }
}

/// Uma especificação: a regra, com nome, negação e obrigatoriedade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatSpec {
    #[serde(rename = "nome", default)]
    pub name: String,
    #[serde(flatten)]
    pub rule: Rule,
    #[serde(rename = "negar", default)]
    pub negate: bool,
    #[serde(rename = "obrigatoria", default)]
    pub required: bool,
}

/// Nome de fonte como a tela o escreve.
#[must_use]
pub fn source_name(source: Source) -> &'static str {
    match source {
        Source::Unknown => "desconhecida",
        Source::Cam => "cam",
        Source::Telesync => "telesync",
        Source::Telecine => "telecine",
        Source::Workprint => "workprint",
        Source::Dvd => "dvd",
        Source::Tv => "tv",
        Source::WebDl => "webdl",
        Source::WebRip => "webrip",
        Source::Bluray => "bluray",
    }
}

#[must_use]
pub fn modifier_name(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::None => "nenhum",
        Modifier::Regional => "regional",
        Modifier::Screener => "screener",
        Modifier::RawHd => "rawhd",
        Modifier::BrDisk => "brdisk",
        Modifier::Remux => "remux",
    }
}

#[derive(Debug, Clone)]
enum Compiled {
    Regex(Regex),
    Plain,
}

/// Um formato pronto para avaliar: as expressões já compiladas.
#[derive(Debug, Clone)]
pub struct CustomFormat {
    pub id: i64,
    pub name: String,
    pub specs: Vec<FormatSpec>,
    compiled: Vec<Compiled>,
}

/// O que se sabe de um release ou arquivo para avaliar formatos.
#[derive(Debug, Clone, Copy)]
pub struct FormatInput<'a> {
    pub title: &'a str,
    pub release_group: Option<&'a str>,
    pub edition: Option<&'a str>,
    pub languages: &'a [Language],
    pub original_language: Language,
    pub quality: Quality,
    /// Bytes; zero é desconhecido e não casa com regra de tamanho.
    pub size: u64,
    pub flags: u32,
}

impl CustomFormat {
    /// # Errors
    ///
    /// Expressão regular inválida, com o nome da especificação.
    pub fn new(id: i64, name: String, specs: Vec<FormatSpec>) -> Result<Self, String> {
        let compiled = specs
            .iter()
            .map(|spec| match &spec.rule {
                Rule::Titulo { valor } | Rule::Grupo { valor } | Rule::Edicao { valor } => {
                    let pattern = if valor.starts_with("(?i)") {
                        valor.clone()
                    } else {
                        format!("(?i){valor}")
                    };
                    Regex::new(&pattern)
                        .map(Compiled::Regex)
                        .map_err(|e| format!("`{}`: expressão inválida — {e}", spec.name))
                }
                _ => Ok(Compiled::Plain),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            id,
            name,
            specs,
            compiled,
        })
    }

    fn spec_matches(&self, index: usize, input: &FormatInput<'_>) -> bool {
        let spec = &self.specs[index];
        let regex = |text: Option<&str>| match (&self.compiled[index], text) {
            (Compiled::Regex(regex), Some(text)) => regex.is_match(text).unwrap_or(false),
            _ => false,
        };
        let hit = match &spec.rule {
            Rule::Titulo { .. } => regex(Some(input.title)),
            Rule::Grupo { .. } => regex(input.release_group),
            Rule::Edicao { .. } => regex(input.edition),
            Rule::Idioma { valor } => match valor.as_str() {
                "Any" => !input.languages.is_empty(),
                "Original" => input.languages.contains(&input.original_language),
                name => Language::from_name(name).is_some_and(|l| input.languages.contains(&l)),
            },
            Rule::Fonte { valor } => source_name(input.quality.source()) == valor,
            Rule::Resolucao { valor } => input.quality.resolution() == *valor,
            Rule::Modificador { valor } => modifier_name(input.quality.modifier()) == valor,
            Rule::Tamanho { minimo, maximo } => {
                #[allow(clippy::cast_precision_loss)]
                let gigabytes = input.size as f64 / 1_073_741_824.0;
                input.size > 0 && gigabytes > *minimo && gigabytes <= *maximo
            }
            Rule::Flag { valor } => input.flags & valor != 0,
        };
        hit != spec.negate
    }

    /// O formato casa com o release?
    #[must_use]
    pub fn matches(&self, input: &FormatInput<'_>) -> bool {
        if self.specs.is_empty() {
            return false;
        }
        let mut kinds: Vec<&'static str> = self.specs.iter().map(|s| s.rule.kind()).collect();
        kinds.dedup();
        kinds.sort_unstable();
        kinds.dedup();
        kinds.into_iter().all(|kind| {
            let group: Vec<usize> = (0..self.specs.len())
                .filter(|&i| self.specs[i].rule.kind() == kind)
                .collect();
            let required_ok = group
                .iter()
                .filter(|&&i| self.specs[i].required)
                .all(|&i| self.spec_matches(i, input));
            let optional: Vec<usize> = group
                .iter()
                .copied()
                .filter(|&i| !self.specs[i].required)
                .collect();
            required_ok
                && (optional.is_empty() || optional.iter().any(|&i| self.spec_matches(i, input)))
        })
    }
}

/// Os formatos que casam, pelos ids.
#[must_use]
pub fn matching(formats: &[CustomFormat], input: &FormatInput<'_>) -> Vec<i64> {
    formats
        .iter()
        .filter(|f| f.matches(input))
        .map(|f| f.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(rule: Rule, required: bool, negate: bool) -> FormatSpec {
        FormatSpec {
            name: "s".into(),
            rule,
            negate,
            required,
        }
    }

    fn input(title: &str) -> FormatInput<'_> {
        FormatInput {
            title,
            release_group: Some("GRP"),
            edition: None,
            languages: &[Language::English],
            original_language: Language::English,
            quality: Quality::WebDl1080p,
            size: 4_000_000_000,
            flags: 0,
        }
    }

    #[test]
    fn titulo_casa_sem_diferenciar_caixa() {
        let hevc = CustomFormat::new(
            1,
            "HEVC".into(),
            vec![spec(
                Rule::Titulo {
                    valor: r"\bx265\b|\bHEVC\b".into(),
                },
                false,
                false,
            )],
        )
        .unwrap();
        assert!(hevc.matches(&input("Movie 2025 1080p WEB-DL hevc-GRP")));
        assert!(!hevc.matches(&input("Movie 2025 1080p WEB-DL x264-GRP")));
    }

    #[test]
    fn obrigatoria_e_negacao() {
        // 1080p obrigatório e nada de x264.
        let format = CustomFormat::new(
            2,
            "1080p sem x264".into(),
            vec![
                spec(Rule::Resolucao { valor: 1080 }, true, false),
                spec(
                    Rule::Titulo {
                        valor: r"x264".into(),
                    },
                    false,
                    true,
                ),
            ],
        )
        .unwrap();
        assert!(format.matches(&input("Movie 2025 1080p WEB-DL x265")));
        assert!(!format.matches(&input("Movie 2025 1080p WEB-DL x264")));
    }

    #[test]
    fn um_de_cada_tipo_basta() {
        let format = CustomFormat::new(
            3,
            "WEB".into(),
            vec![
                spec(
                    Rule::Fonte {
                        valor: "webdl".into(),
                    },
                    false,
                    false,
                ),
                spec(
                    Rule::Fonte {
                        valor: "webrip".into(),
                    },
                    false,
                    false,
                ),
            ],
        )
        .unwrap();
        assert!(format.matches(&input("x")));
    }

    #[test]
    fn expressao_invalida_e_recusada() {
        let error = CustomFormat::new(
            4,
            "ruim".into(),
            vec![spec(Rule::Titulo { valor: "(".into() }, false, false)],
        )
        .unwrap_err();
        assert!(error.contains("expressão inválida"));
    }

    #[test]
    fn le_o_formato_do_banco() {
        let specs: Vec<FormatSpec> = serde_json::from_str(
            r#"[{"nome": "Dual", "tipo": "titulo", "valor": "\\bDUAL\\b"},
                {"nome": "Grande", "tipo": "tamanho", "minimo": 1.5, "maximo": 30, "obrigatoria": true}]"#,
        )
        .unwrap();
        assert_eq!(specs.len(), 2);
        assert!(specs[1].required);
    }
}

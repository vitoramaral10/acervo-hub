//! As regras de decisão: tetos, folga no disco, propers, legendas embutidas,
//! carência, prioridade e seeders por indexador, atraso. Ficam no banco, e a
//! tela as edita. Os tamanhos por qualidade não são regra: são a tabela fixa
//! [`QUALITY_DEFINITIONS`].

use std::collections::BTreeMap;

use acervo_decision::{Delay, QualityDefinition, Settings};
use acervo_parser::Quality;
use acervo_store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::decide::now_rfc3339;

/// Onde as regras ficam na tabela de configurações.
pub const RULES_KEY: &str = "decisao.regras";

/// Prioridade e seeders mínimos de um indexador.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexerRules {
    /// Menor é melhor; 25 é o padrão.
    pub prioridade: i32,
    pub seeders_minimos: u32,
}

impl Default for IndexerRules {
    fn default() -> Self {
        Self {
            prioridade: 25,
            seeders_minimos: 1,
        }
    }
}

/// Tamanho por minuto de filme ou episódio, em megabytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sizes {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub preferred: Option<f64>,
}

/// Sem limite de tamanho por minuto: só o mínimo zero.
const ANY_SIZE: Sizes = Sizes {
    min: Some(0.0),
    max: None,
    preferred: None,
};

/// Até 100 MB por minuto, preferindo 95.
const UP_TO_100: Sizes = Sizes {
    min: Some(0.0),
    max: Some(100.0),
    preferred: Some(95.0),
};

/// Os tamanhos por minuto de cada qualidade. Os valores que estavam no banco quando a
/// tabela deixou de ser editável — as qualidades de fonte bruta e de 2160p
/// sem teto, o resto até 100 MB/min.
pub(crate) const QUALITY_DEFINITIONS: [(Quality, Sizes); 30] = [
    (Quality::Unknown, UP_TO_100),
    (Quality::Sdtv, UP_TO_100),
    (Quality::Dvd, UP_TO_100),
    (Quality::WebDl1080p, UP_TO_100),
    (Quality::Hdtv720p, UP_TO_100),
    (Quality::WebDl720p, UP_TO_100),
    (Quality::Bluray720p, UP_TO_100),
    (Quality::Bluray1080p, ANY_SIZE),
    (Quality::WebDl480p, UP_TO_100),
    (Quality::Hdtv1080p, UP_TO_100),
    (Quality::RawHd, ANY_SIZE),
    (Quality::WebRip480p, UP_TO_100),
    (Quality::WebRip720p, UP_TO_100),
    (Quality::WebRip1080p, UP_TO_100),
    (Quality::Hdtv2160p, ANY_SIZE),
    (Quality::WebRip2160p, ANY_SIZE),
    (Quality::WebDl2160p, ANY_SIZE),
    (Quality::Bluray2160p, ANY_SIZE),
    (Quality::Bluray480p, UP_TO_100),
    (Quality::Bluray576p, UP_TO_100),
    (Quality::BrDisk, ANY_SIZE),
    (Quality::DvdR, UP_TO_100),
    (Quality::Workprint, UP_TO_100),
    (Quality::Cam, UP_TO_100),
    (Quality::Telesync, UP_TO_100),
    (Quality::Telecine, UP_TO_100),
    (Quality::DvdScr, UP_TO_100),
    (Quality::Regional, UP_TO_100),
    (Quality::Remux1080p, ANY_SIZE),
    (Quality::Remux2160p, ANY_SIZE),
];

/// A tabela no formato que o motor de decisão lê.
fn quality_definitions() -> Vec<QualityDefinition> {
    QUALITY_DEFINITIONS
        .iter()
        .map(|(quality, sizes)| QualityDefinition {
            quality: *quality,
            min_size: sizes.min,
            max_size: sizes.max,
            preferred_size: sizes.preferred,
        })
        .collect()
}

/// O que fazer com PROPER e REPACK. Não há upgrade de arquivo: só se
/// prefere, ou não, a revisão nova entre os releases da mesma busca.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Propers {
    #[default]
    #[serde(rename = "preferir")]
    Prefer,
    #[serde(rename = "nao_preferir")]
    DoNotPrefer,
}

/// Espera antes de pegar automaticamente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DelayRules {
    pub minutos: u32,
    pub pular_se_melhor_qualidade: bool,
}

/// As regras, no formato do banco e da tela.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecisionRules {
    /// Teto de tamanho, em megabytes; zero é sem teto.
    pub tamanho_maximo_mb: u64,
    pub aceitar_legenda_embutida: bool,
    /// Termos separados por vírgula que liberam legenda embutida.
    pub legendas_embutidas_liberadas: String,
    pub propers: Propers,
    pub preferir_flags_do_indexador: bool,
    /// Folga que a fila de downloads deixa sempre livre no disco do cliente.
    pub folga_minima_mb: u64,
    /// Quantos torrents do acervo baixam ao mesmo tempo; no mínimo 1.
    pub downloads_simultaneos: u32,
    /// Dias depois da data de disponibilidade.
    pub carencia_dias: i64,
    /// Pelo nome do indexador.
    pub indexadores: BTreeMap<String, IndexerRules>,
    pub atraso: DelayRules,
}

impl Default for DecisionRules {
    fn default() -> Self {
        Self {
            tamanho_maximo_mb: 0,
            aceitar_legenda_embutida: false,
            legendas_embutidas_liberadas: String::new(),
            propers: Propers::Prefer,
            preferir_flags_do_indexador: false,
            folga_minima_mb: 100,
            downloads_simultaneos: 5,
            carencia_dias: 0,
            indexadores: BTreeMap::new(),
            atraso: DelayRules::default(),
        }
    }
}

impl DecisionRules {
    /// Lê o texto guardado.
    ///
    /// # Errors
    ///
    /// JSON fora do formato das regras.
    pub fn parse(text: &str) -> serde_json::Result<Self> {
        serde_json::from_str(text)
    }

    /// As configurações que o motor lê, com os tamanhos por qualidade.
    #[must_use]
    pub fn settings(&self) -> Settings {
        Settings {
            definitions: quality_definitions(),
            maximum_size_mb: self.tamanho_maximo_mb,
            allow_hardcoded_subs: self.aceitar_legenda_embutida,
            whitelisted_hardcoded_subs: self.legendas_embutidas_liberadas.clone(),
            propers: match self.propers {
                Propers::Prefer => acervo_decision::Propers::DoNotUpgrade,
                Propers::DoNotPrefer => acervo_decision::Propers::DoNotPrefer,
            },
            prefer_indexer_flags: self.preferir_flags_do_indexador,
        }
    }

    #[must_use]
    pub const fn delay(&self) -> Delay {
        Delay {
            minutes: self.atraso.minutos,
            bypass_if_highest_quality: self.atraso.pular_se_melhor_qualidade,
        }
    }

    /// O limite de downloads simultâneos, nunca abaixo de 1.
    #[must_use]
    pub fn max_downloads(&self) -> usize {
        self.downloads_simultaneos.max(1) as usize
    }

    /// Prioridade e seeders de um indexador; padrão se não houver regra.
    #[must_use]
    pub fn indexer(&self, name: &str) -> IndexerRules {
        self.indexadores.get(name).copied().unwrap_or_default()
    }
}

/// As regras guardadas. Ilegíveis, valem as padrão — com aviso no log:
/// senão a regra editada na tela some sem ninguém saber por quê.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn stored(store: &Store) -> Result<DecisionRules> {
    let Some(text) = store.setting(RULES_KEY).await? else {
        return Ok(DecisionRules::default());
    };
    Ok(DecisionRules::parse(&text).unwrap_or_else(|error| {
        tracing::warn!("regras de decisão ilegíveis, valem as padrão: {error}");
        DecisionRules::default()
    }))
}

/// # Errors
///
/// Banco inalcançável.
pub async fn save(store: &Store, rules: &DecisionRules) -> Result<()> {
    let text = serde_json::to_string(rules)?;
    store
        .set_setting(RULES_KEY, Some(&text), &now_rfc3339())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regras_gravadas_depois_da_migracao_carregam() {
        // O formato gravado em produção, já com o `propers` normalizado.
        let rules = DecisionRules::parse(
            r#"{"tamanho_maximo_mb": 30000, "aceitar_legenda_embutida": true,
                "legendas_embutidas_liberadas": "", "propers": "preferir",
                "preferir_flags_do_indexador": true, "folga_minima_mb": 2048,
                "carencia_dias": 0,
                "indexadores": {"um": {"prioridade": 25, "seeders_minimos": 1}},
                "atraso": {"minutos": 1440, "pular_se_melhor_qualidade": true}}"#,
        );
        assert!(rules.is_ok(), "{rules:?}");
    }

    #[test]
    fn regras_recusam_campo_aposentado() {
        assert!(
            DecisionRules::parse(r#"{"folga_minima_mb": 2048, "pular_checagem_de_espaco": true}"#)
                .is_err()
        );
    }

    #[test]
    fn propers_so_aceitam_os_valores_atuais() {
        for old in ["preferir_e_atualizar", "nao_atualizar"] {
            assert!(DecisionRules::parse(&format!(r#"{{"propers": "{old}"}}"#)).is_err());
        }
        let rules = DecisionRules::parse(r#"{"propers": "preferir"}"#).expect("lê");
        assert_eq!(rules.propers, Propers::Prefer);
        assert_eq!(
            rules.settings().propers,
            acervo_decision::Propers::DoNotUpgrade
        );
        let rules = DecisionRules::parse(r#"{"propers": "nao_preferir"}"#).expect("lê");
        assert_eq!(rules.propers, Propers::DoNotPrefer);
        assert_eq!(
            serde_json::to_value(&rules).unwrap()["propers"],
            "nao_preferir"
        );
        assert_eq!(
            serde_json::to_value(DecisionRules::default()).unwrap()["propers"],
            "preferir"
        );
        assert!(DecisionRules::parse(r#"{"propers": "talvez"}"#).is_err());
    }

    #[test]
    fn tamanhos_cobrem_toda_qualidade_uma_vez() {
        for quality in Quality::ALL {
            assert_eq!(
                QUALITY_DEFINITIONS
                    .iter()
                    .filter(|(q, _)| *q == quality)
                    .count(),
                1,
                "{quality:?}"
            );
        }
        let sizes = |quality| {
            QUALITY_DEFINITIONS
                .iter()
                .find(|(q, _)| *q == quality)
                .unwrap()
                .1
        };
        assert_eq!(sizes(Quality::WebDl1080p), UP_TO_100);
        assert_eq!(sizes(Quality::Remux2160p), ANY_SIZE);
    }

    #[test]
    fn limite_de_downloads_tem_padrao_e_minimo() {
        let rules = DecisionRules::parse(r#"{"folga_minima_mb": 2048}"#).expect("lê");
        assert_eq!(rules.max_downloads(), 5);
        let rules = DecisionRules::parse(r#"{"downloads_simultaneos": 0}"#).expect("lê");
        assert_eq!(rules.max_downloads(), 1);
    }
}

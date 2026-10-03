//! As regras de decisão: tetos, folga no disco, propers, legendas embutidas,
//! carência, prioridade e seeders por indexador, atraso. Ficam no banco, e a
//! tela as edita.

use std::collections::BTreeMap;

use acervo_decision::{Delay, Propers, Settings};
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

/// Espera antes de pegar automaticamente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DelayRules {
    pub minutos: u32,
    pub pular_se_melhor_qualidade: bool,
}

/// As regras, no formato do banco e da tela.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionRules {
    /// Teto de tamanho, em megabytes; zero é sem teto.
    pub tamanho_maximo_mb: u64,
    pub aceitar_legenda_embutida: bool,
    /// Termos separados por vírgula que liberam legenda embutida.
    pub legendas_embutidas_liberadas: String,
    /// `preferir_e_atualizar`, `nao_atualizar` ou `nao_preferir`.
    pub propers: String,
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
            propers: "preferir_e_atualizar".into(),
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
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// As configurações que o motor lê, com os tamanhos por qualidade.
    #[must_use]
    pub fn settings(&self, definitions: Vec<acervo_store::QualityDefinition>) -> Settings {
        Settings {
            definitions: definitions
                .into_iter()
                .map(|d| acervo_decision::QualityDefinition {
                    quality: d.quality,
                    min_size: d.min_size,
                    max_size: d.max_size,
                    preferred_size: d.preferred_size,
                })
                .collect(),
            maximum_size_mb: self.tamanho_maximo_mb,
            allow_hardcoded_subs: self.aceitar_legenda_embutida,
            whitelisted_hardcoded_subs: self.legendas_embutidas_liberadas.clone(),
            propers: match self.propers.as_str() {
                "nao_atualizar" => Propers::DoNotUpgrade,
                "nao_preferir" => Propers::DoNotPrefer,
                _ => Propers::PreferAndUpgrade,
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

/// As regras guardadas.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn stored(store: &Store) -> Result<DecisionRules> {
    Ok(store
        .setting(RULES_KEY)
        .await?
        .as_deref()
        .and_then(DecisionRules::parse)
        .unwrap_or_default())
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
    fn regras_gravadas_com_campo_aposentado_ainda_leem() {
        // `pular_checagem_de_espaco` existia nas regras antigas.
        let rules =
            DecisionRules::parse(r#"{"folga_minima_mb": 2048, "pular_checagem_de_espaco": true}"#)
                .expect("lê");
        assert_eq!(rules.folga_minima_mb, 2048);
    }

    #[test]
    fn limite_de_downloads_tem_padrao_e_minimo() {
        let rules = DecisionRules::parse(r#"{"folga_minima_mb": 2048}"#).expect("lê");
        assert_eq!(rules.max_downloads(), 5);
        let rules = DecisionRules::parse(r#"{"downloads_simultaneos": 0}"#).expect("lê");
        assert_eq!(rules.max_downloads(), 1);
    }
}

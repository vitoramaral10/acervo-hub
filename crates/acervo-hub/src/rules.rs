//! As regras de decisão: tetos, folga no disco, propers, legendas embutidas,
//! carência, prioridade e seeders por indexador, atraso — e de quem elas são.
//!
//! Enquanto o gerenciador de filmes for o dono ("radarr"), as regras, os
//! perfis, os formatos e os tamanhos vêm dele a cada leitura e a tela só os
//! mostra. Depois que o acervo assume ("acervo"), nada disso é mais lido de
//! lá: vale o que está no banco, e a tela edita.

use std::collections::BTreeMap;

use acervo_decision::{Delay, FormatSpec, Propers, Rule, Settings};
use acervo_store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, InstanceKind};
use crate::shadow::now_rfc3339;

/// Onde as regras ficam na tabela de configurações.
pub const RULES_KEY: &str = "decisao.regras";
/// Quem é o dono das regras: `radarr` ou `acervo`.
pub const OWNER_KEY: &str = "regras.dono";

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
    /// Pula a espera a partir desta nota de formatos.
    pub pular_acima_da_nota: Option<i32>,
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
    /// Folga que precisa sobrar no disco depois do download.
    pub folga_minima_mb: u64,
    pub pular_checagem_de_espaco: bool,
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
            pular_checagem_de_espaco: false,
            carencia_dias: 0,
            indexadores: BTreeMap::new(),
            atraso: DelayRules::default(),
        }
    }
}

/// O formato antigo, de quando só se guardava a resposta crua do gerenciador.
#[derive(Deserialize)]
struct Legacy {
    indexer_config: Value,
    media_config: Value,
    indexers: BTreeMap<String, (i32, u32)>,
}

impl DecisionRules {
    /// Converte a configuração do gerenciador.
    #[must_use]
    pub fn from_manager(
        indexer: &Value,
        media: &Value,
        indexers: BTreeMap<String, (i32, u32)>,
        delay: Option<&Value>,
    ) -> Self {
        Self {
            tamanho_maximo_mb: indexer["maximumSize"].as_u64().unwrap_or(0),
            aceitar_legenda_embutida: indexer["allowHardcodedSubs"].as_bool().unwrap_or(false),
            legendas_embutidas_liberadas: indexer["whitelistedHardcodedSubs"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            propers: match media["downloadPropersAndRepacks"].as_str() {
                Some("doNotUpgrade") => "nao_atualizar",
                Some("doNotPrefer") => "nao_preferir",
                _ => "preferir_e_atualizar",
            }
            .into(),
            preferir_flags_do_indexador: indexer["preferIndexerFlags"].as_bool().unwrap_or(false),
            folga_minima_mb: media["minimumFreeSpaceWhenImporting"]
                .as_u64()
                .unwrap_or(100),
            pular_checagem_de_espaco: media["skipFreeSpaceCheckWhenImporting"]
                .as_bool()
                .unwrap_or(false),
            carencia_dias: indexer["availabilityDelay"].as_i64().unwrap_or(0),
            indexadores: indexers
                .into_iter()
                .map(|(name, (prioridade, seeders_minimos))| {
                    (
                        name,
                        IndexerRules {
                            prioridade,
                            seeders_minimos,
                        },
                    )
                })
                .collect(),
            atraso: delay.map_or_else(DelayRules::default, |d| DelayRules {
                minutos: d["torrentDelay"]
                    .as_u64()
                    .and_then(|m| u32::try_from(m).ok())
                    .unwrap_or(0),
                pular_se_melhor_qualidade: d["bypassIfHighestQuality"].as_bool().unwrap_or(false),
                pular_acima_da_nota: d["bypassIfAboveCustomFormatScore"]
                    .as_bool()
                    .unwrap_or(false)
                    .then(|| {
                        d["minimumCustomFormatScore"]
                            .as_i64()
                            .and_then(|s| i32::try_from(s).ok())
                            .unwrap_or(0)
                    }),
            }),
        }
    }

    /// Lê o texto guardado, no formato novo ou no antigo.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        if value.get("indexer_config").is_some() {
            let legacy: Legacy = serde_json::from_value(value).ok()?;
            return Some(Self::from_manager(
                &legacy.indexer_config,
                &legacy.media_config,
                legacy.indexers,
                None,
            ));
        }
        serde_json::from_value(value).ok()
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
            minimum_free_space_mb: self.folga_minima_mb,
            skip_free_space_check: self.pular_checagem_de_espaco,
        }
    }

    #[must_use]
    pub const fn delay(&self) -> Delay {
        Delay {
            minutes: self.atraso.minutos,
            bypass_if_highest_quality: self.atraso.pular_se_melhor_qualidade,
            bypass_if_above_score: self.atraso.pular_acima_da_nota,
        }
    }

    /// Prioridade e seeders de um indexador; padrão se não houver regra.
    #[must_use]
    pub fn indexer(&self, name: &str) -> IndexerRules {
        self.indexadores.get(name).copied().unwrap_or_default()
    }
}

/// Prioridade e seeders mínimos de cada indexador daqui, lidos do cadastro
/// deles no gerenciador (`<nome> (acervo-hub)`).
fn manager_indexers(indexers: &[acervo_arr::RemoteIndexer]) -> BTreeMap<String, (i32, u32)> {
    indexers
        .iter()
        .filter_map(|i| {
            let name = i.name.strip_suffix(crate::sync::SUFFIX)?;
            Some((
                name.to_owned(),
                (
                    i.priority()
                        .and_then(|p| i32::try_from(p).ok())
                        .unwrap_or(25),
                    i.minimum_seeders()
                        .and_then(|s| u32::try_from(s).ok())
                        .unwrap_or(1),
                ),
            ))
        })
        .collect()
}

/// Lê as regras do gerenciador.
///
/// # Errors
///
/// Gerenciador inalcançável.
pub async fn from_manager(client: &acervo_arr::ArrClient) -> Result<DecisionRules> {
    let (indexer_config, media_config, remote_indexers, delays) = tokio::try_join!(
        client.indexer_config(),
        client.media_management_config(),
        client.indexers(),
        client.get_json("api/v3/delayprofile", &[]),
    )?;
    // O perfil de espera padrão é o sem tags.
    let delay = delays.as_array().and_then(|all| {
        all.iter()
            .find(|d| d["tags"].as_array().is_none_or(Vec::is_empty))
            .cloned()
    });
    Ok(DecisionRules::from_manager(
        &indexer_config,
        &media_config,
        manager_indexers(&remote_indexers),
        delay.as_ref(),
    ))
}

/// Quem manda nas regras. Sem dono gravado: o gerenciador, se houver um
/// configurado; senão, o acervo.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn owner(config: &Config, store: &Store) -> Result<&'static str> {
    Ok(match store.setting(OWNER_KEY).await?.as_deref() {
        Some("acervo") => "acervo",
        Some("radarr") => "radarr",
        _ if config
            .instances
            .iter()
            .any(|spec| matches!(spec.kind, InstanceKind::Movie)) =>
        {
            "radarr"
        }
        _ => "acervo",
    })
}

/// O corte: o acervo assume. Antes, uma última cópia de tudo do gerenciador
/// (filmes, perfis, formatos, tamanhos e regras); depois, os filmes passam a
/// ser do acervo (com os ids de lá) e nada mais é importado.
///
/// # Errors
///
/// Gerenciador inalcançável (com um configurado) ou banco inalcançável.
pub async fn take_over(config: &Config, store: &Store) -> Result<()> {
    if owner(config, store).await? == "radarr" {
        let client = crate::shadow::movie_client(config)?;
        let rules = from_manager(&client).await?;
        crate::movies::import(config, store, true, false).await?;
        save(store, &rules).await?;
    }
    store.adopt_all().await?;
    store
        .set_setting(OWNER_KEY, Some("acervo"), &now_rfc3339())
        .await?;
    Ok(())
}

/// Devolve as regras ao gerenciador: a próxima importação volta a trazer
/// os filmes dele (os ids são os mesmos) e as regras.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn give_back(store: &Store) -> Result<()> {
    store
        .set_setting(OWNER_KEY, Some("radarr"), &now_rfc3339())
        .await?;
    Ok(())
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

/// Converte uma especificação de formato do gerenciador. As que não têm
/// equivalente aqui ficam de fora, com aviso no log.
fn manager_spec(spec: &Value) -> Option<FormatSpec> {
    let field = |name: &str| {
        spec["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["name"] == name)
            .map_or(Value::Null, |f| f["value"].clone())
    };
    let value = field("value");
    let rule = match spec["implementation"].as_str()? {
        "ReleaseTitleSpecification" => Rule::Titulo {
            valor: value.as_str()?.to_owned(),
        },
        "ReleaseGroupSpecification" => Rule::Grupo {
            valor: value.as_str()?.to_owned(),
        },
        "EditionSpecification" => Rule::Edicao {
            valor: value.as_str()?.to_owned(),
        },
        "LanguageSpecification" => {
            let id = value.as_i64()?;
            let name = match id {
                -2 => "Original",
                -1 => "Any",
                _ => acervo_parser::Language::ALL
                    .into_iter()
                    .find(|l| i64::from(l.id()) == id)?
                    .name(),
            };
            Rule::Idioma {
                valor: name.to_owned(),
            }
        }
        "SourceSpecification" => Rule::Fonte {
            valor: match value.as_i64()? {
                1 => "cam",
                2 => "telesync",
                3 => "telecine",
                4 => "workprint",
                5 => "dvd",
                6 => "tv",
                7 => "webdl",
                8 => "webrip",
                9 => "bluray",
                _ => "desconhecida",
            }
            .into(),
        },
        "ResolutionSpecification" => Rule::Resolucao {
            valor: u16::try_from(value.as_i64()?).ok()?,
        },
        "QualityModifierSpecification" => Rule::Modificador {
            valor: match value.as_i64()? {
                1 => "regional",
                2 => "screener",
                3 => "rawhd",
                4 => "brdisk",
                5 => "remux",
                _ => "nenhum",
            }
            .into(),
        },
        "SizeSpecification" => Rule::Tamanho {
            minimo: field("min").as_f64().unwrap_or(0.0),
            maximo: field("max").as_f64().unwrap_or(0.0),
        },
        "IndexerFlagSpecification" => Rule::Flag {
            valor: u32::try_from(value.as_i64()?).ok()?,
        },
        other => {
            tracing::warn!(
                especificacao = other,
                "especificação de formato sem equivalente; ignorada"
            );
            return None;
        }
    };
    Some(FormatSpec {
        name: spec["name"].as_str().unwrap_or_default().to_owned(),
        rule,
        negate: spec["negate"].as_bool().unwrap_or(false),
        required: spec["required"].as_bool().unwrap_or(false),
    })
}

/// Os formatos do gerenciador, no formato do banco.
#[must_use]
pub fn manager_formats(formats: &Value) -> Vec<acervo_store::CustomFormat> {
    formats
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|format| {
            let specs: Vec<FormatSpec> = format["specifications"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(manager_spec)
                .collect();
            Some(acervo_store::CustomFormat {
                id: format["id"].as_i64()?,
                name: format["name"].as_str()?.to_owned(),
                specifications: serde_json::to_value(specs).ok()?,
                include_when_renaming: format["includeCustomFormatWhenRenaming"]
                    .as_bool()
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// As notas de formato de cada perfil do gerenciador, pelo id do perfil.
#[must_use]
pub fn manager_scores(profiles: &Value) -> BTreeMap<i64, BTreeMap<i64, i32>> {
    profiles
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|profile| {
            let scores = profile["formatItems"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    let score = i32::try_from(item["score"].as_i64()?).ok()?;
                    (score != 0).then_some((item["format"].as_i64()?, score))
                })
                .collect();
            Some((profile["id"].as_i64()?, scores))
        })
        .collect()
}

/// Os formatos do banco, prontos para o motor. Formato com expressão
/// inválida fica de fora, com aviso.
#[must_use]
pub fn compiled_formats(
    formats: &[acervo_store::CustomFormat],
) -> Vec<acervo_decision::CustomFormat> {
    formats
        .iter()
        .filter_map(|format| {
            let specs: Vec<FormatSpec> = match serde_json::from_value(format.specifications.clone())
            {
                Ok(specs) => specs,
                Err(error) => {
                    tracing::warn!(formato = format.name, "especificações ilegíveis: {error}");
                    return None;
                }
            };
            match acervo_decision::CustomFormat::new(format.id, format.name.clone(), specs) {
                Ok(compiled) => Some(compiled),
                Err(error) => {
                    tracing::warn!(formato = format.name, "{error}");
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn le_o_formato_antigo() {
        let text = json!({
            "indexer_config": {"maximumSize": 0, "availabilityDelay": 2},
            "media_config": {"minimumFreeSpaceWhenImporting": 10240, "downloadPropersAndRepacks": "preferAndUpgrade"},
            "indexers": {"bjshare": [10, 3]}
        })
        .to_string();
        let rules = DecisionRules::parse(&text).unwrap();
        assert_eq!(rules.folga_minima_mb, 10240);
        assert_eq!(rules.carencia_dias, 2);
        assert_eq!(rules.indexer("bjshare").seeders_minimos, 3);
        assert_eq!(rules.indexer("outro").prioridade, 25);
    }

    #[test]
    fn converte_formato_do_gerenciador() {
        let formats = manager_formats(&json!([{
            "id": 2, "name": "Dual Audio PT-BR", "includeCustomFormatWhenRenaming": false,
            "specifications": [
                {"name": "Dual", "implementation": "ReleaseTitleSpecification", "negate": false,
                 "required": false, "fields": [{"name": "value", "value": "\\bDUAL\\b"}]},
                {"name": "Grande", "implementation": "SizeSpecification", "negate": false,
                 "required": true, "fields": [{"name": "min", "value": 1}, {"name": "max", "value": 40}]}
            ]
        }]));
        assert_eq!(formats.len(), 1);
        let compiled = compiled_formats(&formats);
        assert_eq!(compiled[0].specs.len(), 2);
        assert!(compiled[0].specs[1].required);
    }

    #[test]
    fn notas_dos_perfis() {
        let scores = manager_scores(&json!([
            {"id": 1, "formatItems": [{"format": 2, "score": 10}, {"format": 1, "score": 0}]}
        ]));
        assert_eq!(scores[&1].len(), 1);
        assert_eq!(scores[&1][&2], 10);
    }
}

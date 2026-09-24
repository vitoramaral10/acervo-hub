//! Carrega um diretório de definições Cardigann reais e resume o resultado.
//!
//! Não roda por padrão: o corpus não é versionado aqui, porque as definições
//! vêm de outro projeto. Aponte para um diretório com os `.yml`:
//!
//! ```sh
//! ACERVO_CARDIGANN_CORPUS=/caminho/Definitions \
//!     cargo test -p acervo-indexers --test corpus -- --ignored --nocapture
//! ```
//!
//! O que o teste garante é que nenhuma definição derruba o loader: cada uma
//! ou carrega, ou é recusada com erro de definição. A contagem por motivo é
//! o mapa do que falta implementar, em ordem de quanto destrava.

use std::collections::BTreeMap;

use acervo_indexers::{CardigannDefinition, IndexerError};

#[test]
#[ignore = "exige ACERVO_CARDIGANN_CORPUS apontando para definições reais"]
fn corpus_real_carrega_ou_e_recusado_sem_panico() {
    let Some(dir) = std::env::var_os("ACERVO_CARDIGANN_CORPUS") else {
        panic!("defina ACERVO_CARDIGANN_CORPUS");
    };
    let mut loaded = Vec::new();
    let mut refused: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0;

    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "yml") {
            continue;
        }
        total += 1;
        let yaml = std::fs::read_to_string(&path).unwrap();
        let outcome = std::panic::catch_unwind(|| CardigannDefinition::from_yaml_v11(&yaml))
            .unwrap_or_else(|_| panic!("{}: o loader entrou em pânico", path.display()));
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let reason = match outcome {
            Ok(definition) => {
                loaded.push(definition.id().to_owned());
                continue;
            }
            Err(IndexerError::Definition { section, reason }) => format!("{section}: {reason}"),
            Err(IndexerError::UnsupportedDefinitionKey { key }) => format!("chave `{key}`"),
            Err(other) => panic!("{}: erro fora do contrato: {other}", path.display()),
        };
        refused.entry(reason).or_default().push(stem);
    }

    let mut reasons: Vec<_> = refused.into_iter().collect();
    reasons.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    println!("{total} definições, {} carregadas", loaded.len());
    for (reason, mut files) in reasons {
        files.sort();
        let sample = files.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
        println!("{:>5}  {reason}  [{sample}]", files.len());
    }
    loaded.sort();
    println!("carregadas: {}", loaded.join(", "));
}

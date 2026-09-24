//! Fidelidade contra a referência, num corpus de títulos reais.
//!
//! O corpus é um JSON com a leitura que o gerenciador de filmes faz de cada
//! título (o endpoint `/api/v3/parse`), e fica fora do repositório: títulos
//! reais carregam nome de tracker privado. Rode com
//!
//! ```text
//! ACERVO_PARSER_CORPUS=corpus-filmes.json cargo test -p acervo-parser --test corpus -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;

use acervo_parser::parse_movie_title;
use serde::Deserialize;

#[derive(Deserialize)]
struct Entry {
    title: String,
    radarr: Option<Reference>,
}

#[derive(Deserialize)]
struct Reference {
    titulos: Vec<String>,
    ano: Option<u16>,
    qualidade: String,
    versao: u8,
    real: u8,
    repack: bool,
    idiomas: Vec<String>,
    grupo: Option<String>,
    edicao: Option<String>,
    imdb: Option<String>,
    tmdb: Option<u32>,
}

fn blank_is_none(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_owned)
}

#[test]
#[ignore = "precisa do corpus fora do repositório (ACERVO_PARSER_CORPUS)"]
fn corpus() {
    let path = std::env::var("ACERVO_PARSER_CORPUS").expect("ACERVO_PARSER_CORPUS");
    let entries: Vec<Entry> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let mut total = 0usize;
    let mut differences: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for entry in &entries {
        let Some(reference) = &entry.radarr else {
            continue;
        };
        total += 1;
        let Some(parsed) = parse_movie_title(&entry.title) else {
            differences
                .entry("não leu")
                .or_default()
                .push(entry.title.clone());
            continue;
        };
        let mut compare = |field: &'static str, ours: String, theirs: String| {
            if ours != theirs {
                differences.entry(field).or_default().push(format!(
                    "{}\n      nosso: {ours}\n      ref.:  {theirs}",
                    entry.title
                ));
            }
        };
        compare(
            "títulos",
            format!("{:?}", parsed.titles),
            format!("{:?}", reference.titulos),
        );
        compare(
            "ano",
            format!("{:?}", parsed.year),
            format!("{:?}", reference.ano.filter(|y| *y != 0)),
        );
        compare(
            "qualidade",
            parsed.quality.quality.name().to_owned(),
            reference.qualidade.clone(),
        );
        compare(
            "revisão",
            format!(
                "{}/{}/{}",
                parsed.quality.revision.version,
                parsed.quality.revision.real,
                parsed.quality.revision.is_repack
            ),
            format!(
                "{}/{}/{}",
                reference.versao, reference.real, reference.repack
            ),
        );
        compare(
            "idiomas",
            format!(
                "{:?}",
                parsed
                    .languages
                    .iter()
                    .map(|l| l.name())
                    .collect::<Vec<_>>()
            ),
            format!("{:?}", reference.idiomas),
        );
        compare(
            "grupo",
            format!("{:?}", parsed.release_group),
            format!("{:?}", blank_is_none(reference.grupo.as_deref())),
        );
        compare(
            "edição",
            format!("{:?}", parsed.edition),
            format!("{:?}", blank_is_none(reference.edicao.as_deref())),
        );
        compare(
            "imdb",
            format!("{:?}", parsed.imdb_id),
            format!("{:?}", blank_is_none(reference.imdb.as_deref())),
        );
        compare(
            "tmdb",
            format!("{:?}", parsed.tmdb_id),
            format!("{:?}", reference.tmdb.filter(|id| *id != 0)),
        );
    }

    println!("{total} títulos com leitura de referência");
    for (field, cases) in &differences {
        println!("\n{field}: {} divergências", cases.len());
        for case in cases.iter().take(8) {
            println!("   {case}");
        }
    }
    let wrong: usize = differences.values().map(Vec::len).sum();
    assert_eq!(wrong, 0, "divergências contra a referência");
}

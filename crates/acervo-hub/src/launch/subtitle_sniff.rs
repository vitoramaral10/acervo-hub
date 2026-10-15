//! Classificador de conteúdo de legenda em arquivo (SRT, ASS/SSA, VTT).
//!
//! Função pura: recebe os bytes do arquivo e a duração do vídeo e devolve o
//! idioma, a contagem de falas e se a legenda é forçada. Serve para o
//! `.srt` sem sufixo de idioma no nome (`Filme.2026.1080p.WEB-DL-GRP.srt`),
//! que o nome sozinho não diz o que é. Nada aqui lê disco.
//!
//! # Idioma
//!
//! Por frequência de palavras funcionais, só as que não se repetem entre os
//! idiomas (`que`, `de`, `para`, `como` e `é` ficam de fora: PT e ES dividem).
//! O idioma vencedor precisa de [`MIN_HITS`] acertos e de [`DOMINANCE`] vezes
//! o do segundo colocado; senão é [`SniffLanguage::Unknown`].
//!
//! # Variante PT-BR x PT-PT
//!
//! Só por marcadores **exclusivos** de cada lado. BR: `você`, `ônibus`,
//! `legal`, `a gente`, `tá`... PT: `autocarro`, `rapariga`, `casa de banho`,
//! `telemóvel`, `pequeno-almoço`, `comboio`, `fixe`, `miúdo`, `estás`,
//! `estou a` / `está a` + infinitivo, `tu` + pretérito em `-aste`/`-este` e o
//! enclítico `-te` no fim da fala (`amava-te!`). Ficam de fora os que o BR
//! também escreve: `rapaz`, `vê-lo`, `fazê-lo`, `-o`/`-a`/`-lhe`/`-lo`/`-la`.
//!
//! A variante só é dada como achada com [`VARIANT_MIN_HITS`] marcadores do
//! lado e pelo menos [`VARIANT_DOMINANCE`] vezes os do outro. Senão é
//! [`SniffLanguage::Pt`]: português cuja variante **não foi confirmada**, que
//! as regras não aceitam como PT-BR sem aviso.
//!
//! # Completa ou forçada
//!
//! Legenda forçada só traz as falas em outro idioma: poucas falas e pouca
//! cobertura (soma das durações das falas / duração do vídeo). O resultado é
//! de três estados ([`Completeness`]), com a duração do vídeo:
//!
//! - **Completa**: cobertura >= [`COMPLETE_MIN_COVERAGE`] (25%) **e** pelo
//!   menos uma fala a cada [`COMPLETE_SECS_PER_CUE`] (30 s).
//! - **Forçada**: cobertura < [`FORCED_MAX_COVERAGE`] (8%) **ou** menos de
//!   uma fala a cada [`FORCED_SECS_PER_CUE`] (120 s).
//! - **Inconclusiva**: o meio-termo (filme quase mudo, legenda parcial...).
//!
//! Sem a duração só a contagem fala: >= [`NO_DURATION_COMPLETE_CUES`] falas é
//! completa, < [`NO_DURATION_FORCED_CUES`] é forçada, o resto inconclusivo.
//!
//! Calibrado com dois casos reais: `Man.Of.War` (~995 falas em ~1 h 40, uma
//! a cada ~6 s, cobertura bem acima de 25%: completa) e `Motor.City` (28
//! falas em 1 h 43, uma a cada ~220 s, cobertura ~1%: forçada). Uma contagem
//! sozinha não basta: forçada com 150+ falas curtas num filme de 2 h tem
//! cobertura de 4%.

use std::collections::HashMap;

/// Legendas sondadas, pelo caminho do arquivo (o mesmo de `SourceFile::path`).
pub type Sniffs = HashMap<String, Sniff>;

/// Maior arquivo de legenda que o preparo lê (UTF-8 de filme sai em ~100 KiB).
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Cobertura (falas / vídeo) mínima para a legenda ser completa...
pub const COMPLETE_MIN_COVERAGE: f64 = 0.25;
/// ...com ao menos uma fala a cada tantos segundos de vídeo.
pub const COMPLETE_SECS_PER_CUE: f64 = 30.0;
/// Cobertura abaixo disto: forçada.
pub const FORCED_MAX_COVERAGE: f64 = 0.08;
/// Menos de uma fala a cada tantos segundos de vídeo: forçada.
pub const FORCED_SECS_PER_CUE: f64 = 120.0;
/// Sem a duração do vídeo: tantas falas ou mais, completa...
pub const NO_DURATION_COMPLETE_CUES: usize = 800;
/// ...e menos que isto, forçada.
pub const NO_DURATION_FORCED_CUES: usize = 60;

/// Marcadores exclusivos de um lado para a variante ser dada como achada.
const VARIANT_MIN_HITS: usize = 3;
/// Quantas vezes o lado vencedor tem de superar o outro.
const VARIANT_DOMINANCE: usize = 2;

/// Mínimo de palavras funcionais do idioma para ele ser dado como achado.
const MIN_HITS: usize = 3;
/// Quantas vezes o idioma vencedor tem de superar o segundo.
const DOMINANCE: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SniffLanguage {
    PtBr,
    PtPt,
    /// Português, sem marcador que separe BR de PT.
    Pt,
    English,
    Spanish,
    /// Texto sem palavras funcionais suficientes para decidir.
    Unknown,
}

/// O que se leu da legenda.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sniff {
    pub language: SniffLanguage,
    /// Falas (blocos de texto com tempo).
    pub cues: usize,
    /// Soma das durações das falas, em segundos.
    pub covered_secs: f64,
    /// Duração do vídeo associado, se conhecida.
    pub video_secs: Option<f64>,
}

/// A legenda é a completa do filme, a forçada, ou não dá para saber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Forced,
    Inconclusive,
}

impl Sniff {
    /// Cobertura: soma das durações das falas / duração do vídeo.
    #[must_use]
    pub fn coverage(&self) -> Option<f64> {
        self.video_secs
            .filter(|secs| *secs > 0.0)
            .map(|secs| self.covered_secs / secs)
    }

    /// Completa, forçada ou inconclusiva, pelo critério do módulo.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn completeness(&self) -> Completeness {
        let (Some(secs), Some(coverage)) = (self.video_secs.filter(|s| *s > 0.0), self.coverage())
        else {
            return if self.cues >= NO_DURATION_COMPLETE_CUES {
                Completeness::Complete
            } else if self.cues < NO_DURATION_FORCED_CUES {
                Completeness::Forced
            } else {
                Completeness::Inconclusive
            };
        };
        let cues = self.cues as f64;
        if coverage < FORCED_MAX_COVERAGE || cues * FORCED_SECS_PER_CUE < secs {
            Completeness::Forced
        } else if coverage >= COMPLETE_MIN_COVERAGE && cues * COMPLETE_SECS_PER_CUE >= secs {
            Completeness::Complete
        } else {
            Completeness::Inconclusive
        }
    }
}

/// Lê a legenda. `video_secs` é a duração do vídeo a que ela pertence.
#[must_use]
pub fn sniff(bytes: &[u8], video_secs: Option<f64>) -> Sniff {
    let text = decode(bytes);
    let cues = parse_cues(&text);
    let covered_secs = cues.iter().map(|c| c.secs).sum();
    let mut body = String::new();
    for cue in &cues {
        body.push_str(&cue.text);
        body.push('\n');
    }
    Sniff {
        language: classify(&body),
        cues: cues.len(),
        covered_secs,
        video_secs,
    }
}

// ---------------------------------------------------------------------------
// Decodificação
// ---------------------------------------------------------------------------

/// UTF-8 (com ou sem BOM) ou UTF-16 com BOM; senão Windows-1252, que cobre
/// o Latin-1 e é o que as legendas antigas em PT trazem.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|b| windows_1252(*b)).collect(),
    }
}

fn decode_utf16(bytes: &[u8], word: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| word(*c)).collect();
    String::from_utf16_lossy(&units)
}

/// Os bytes 0x80..=0x9F do Windows-1252 (o resto é igual ao Latin-1).
fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        _ => char::from(byte),
    }
}

// ---------------------------------------------------------------------------
// Falas
// ---------------------------------------------------------------------------

struct Cue {
    secs: f64,
    text: String,
}

fn parse_cues(text: &str) -> Vec<Cue> {
    if text.contains("[Events]") {
        parse_ass(text)
    } else {
        parse_srt(text)
    }
}

fn flush(current: &mut Option<(f64, String)>, cues: &mut Vec<Cue>) {
    if let Some((secs, body)) = current.take() {
        let text = clean(&body);
        if !text.is_empty() {
            cues.push(Cue { secs, text });
        }
    }
}

/// SRT e VTT: um bloco é a linha de tempo (`a --> b`) e o texto até a linha
/// em branco. O número do bloco e o cabeçalho `WEBVTT` não têm `-->`.
fn parse_srt(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut current: Option<(f64, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some((start, end)) = line.split_once("-->") {
            flush(&mut current, &mut cues);
            // VTT põe configurações depois do fim (`... --> 00:05.000 line:90%`).
            let end = end.split_whitespace().next().unwrap_or("");
            let secs = match (timestamp(start), timestamp(end)) {
                (Some(a), Some(b)) => (b - a).max(0.0),
                _ => 0.0,
            };
            current = Some((secs, String::new()));
        } else if line.is_empty() {
            flush(&mut current, &mut cues);
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&mut current, &mut cues);
    cues
}

/// `00:01:02,345`, `00:01:02.345` ou `01:02.345` (VTT).
fn timestamp(raw: &str) -> Option<f64> {
    let raw = raw.trim().replace(',', ".");
    let mut total = 0.0;
    for part in raw.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(total)
}

/// ASS/SSA: linhas `Dialogue:` com os campos na ordem do `Format:` de
/// `[Events]`; o texto é o último e pode ter vírgulas.
fn parse_ass(text: &str) -> Vec<Cue> {
    let mut fields: Vec<String> = Vec::new();
    let mut in_events = false;
    let mut cues = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_events = line.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(format) = line.strip_prefix("Format:") {
            fields = format
                .split(',')
                .map(|f| f.trim().to_ascii_lowercase())
                .collect();
        } else if let Some(dialogue) = line.strip_prefix("Dialogue:") {
            let count = if fields.is_empty() { 10 } else { fields.len() };
            let parts: Vec<&str> = dialogue.splitn(count, ',').collect();
            let at = |name: &str, default: usize| {
                let index = fields.iter().position(|f| f == name).unwrap_or(default);
                parts.get(index).and_then(|p| timestamp(p))
            };
            let secs = match (at("start", 1), at("end", 2)) {
                (Some(a), Some(b)) => (b - a).max(0.0),
                _ => 0.0,
            };
            let body = parts.last().copied().unwrap_or("");
            let text = clean(body);
            if !text.is_empty() {
                cues.push(Cue { secs, text });
            }
        }
    }
    cues
}

/// Tira `<i>`, `{\an8}` e as quebras `\N` do ASS; o resto é o diálogo.
fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut skip_until: Option<char> = None;
    for c in raw.replace("\\N", " ").replace("\\n", " ").chars() {
        match (skip_until, c) {
            (Some(end), c) if c == end => skip_until = None,
            (Some(_), _) => {}
            (None, '<') => skip_until = Some('>'),
            (None, '{') => skip_until = Some('}'),
            (None, c) => out.push(c),
        }
    }
    out.trim().to_owned()
}

// ---------------------------------------------------------------------------
// Idioma
// ---------------------------------------------------------------------------

const PT_WORDS: &[&str] = &[
    "não", "você", "vocês", "isso", "isto", "está", "estão", "muito", "também", "então", "uma",
    "com", "ele", "ela", "eles", "elas", "são", "já", "nós", "eu", "os", "do", "da", "dos", "das",
    "ao", "pelo", "pela", "sim", "obrigado", "obrigada", "foi", "tem", "mais", "só", "pra", "meu",
    "minha", "seu", "sua", "dele", "dela",
];

const EN_WORDS: &[&str] = &[
    "the", "you", "and", "is", "what", "it", "to", "of", "that", "this", "are", "was", "have",
    "with", "not", "he", "she", "we", "they", "your", "my", "i", "don", "know", "just", "like",
    "for", "but", "can", "there", "will", "about", "been", "would",
];

const ES_WORDS: &[&str] = &[
    "pero", "usted", "ustedes", "muy", "también", "entonces", "qué", "el", "los", "las", "una",
    "es", "yo", "tú", "gracias", "señor", "estoy", "estás", "esto", "eso", "soy", "bien", "pues",
    "cómo", "dónde", "aquí", "hay", "del", "ella", "él", "sí", "tengo", "puedo", "con", "nosotros",
    "cuando", "fue",
];

const BR_WORDS: &[&str] = &[
    "você", "vocês", "ônibus", "legal", "tá", "né", "bacana", "celular", "moleque",
];

/// Palavras só do português europeu (o BR escreve outra).
const PT_PT_WORDS: &[&str] = &[
    "autocarro",
    "autocarros",
    "rapariga",
    "raparigas",
    "miúdo",
    "miúdos",
    "miúda",
    "miúdas",
    "fixe",
    "telemóvel",
    "telemóveis",
    "comboio",
    "comboios",
    "estás",
];

/// Pretéritos na 2.ª pessoa do singular (`tu fizeste`), com `tu` perto.
const PRETERITE_TU: &[&str] = &[
    "fizeste",
    "disseste",
    "tiveste",
    "estiveste",
    "puseste",
    "soubeste",
    "quiseste",
    "vieste",
    "foste",
    "viste",
    "comeste",
    "perdeste",
    "pensaste",
    "falaste",
    "chegaste",
    "gostaste",
    "deixaste",
    "mataste",
    "levaste",
];

/// Terminações `-aste` que não são verbo na 2.ª pessoa.
const NOT_ASTE: &[&str] = &["contraste", "desgaste", "devaste", "arrebaste"];

/// Infinitivos curtos que o teste de tamanho não pega (`estou a ver`).
const SHORT_INFINITIVES: &[&str] = &["ver", "ser", "ter", "dar", "ler", "vir", "ir", "pôr", "rir"];

fn count(tokens: &[String], list: &[&str]) -> usize {
    tokens.iter().filter(|t| list.contains(&t.as_str())).count()
}

fn classify(body: &str) -> SniffLanguage {
    let lower = body.to_lowercase();
    // Sem hífen: `amava-te` vira `amava` e `te`; o enclítico é contado à parte.
    let tokens: Vec<String> = lower
        .split(|c: char| !c.is_alphabetic())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();

    let mut pt = count(&tokens, PT_WORDS);
    // Terminações exclusivas: `-ção` / `-ções` (PT) contra `-ción` / `-ciones` (ES).
    pt += tokens
        .iter()
        .filter(|t| t.ends_with("ção") || t.ends_with("ções"))
        .count();
    let mut es = count(&tokens, ES_WORDS);
    es += tokens
        .iter()
        .filter(|t| t.ends_with("ción") || t.ends_with("ciones"))
        .count();
    let en = count(&tokens, EN_WORDS);

    let (best, best_hits, second_hits) = {
        let mut scores = [
            (SniffLanguage::Pt, pt),
            (SniffLanguage::English, en),
            (SniffLanguage::Spanish, es),
        ];
        scores.sort_by_key(|(_, hits)| std::cmp::Reverse(*hits));
        (scores[0].0, scores[0].1, scores[1].1)
    };
    if best_hits < MIN_HITS || best_hits < second_hits * DOMINANCE {
        return SniffLanguage::Unknown;
    }
    if best != SniffLanguage::Pt {
        return best;
    }
    variant(&lower, &tokens)
}

fn variant(lower: &str, tokens: &[String]) -> SniffLanguage {
    let mut br = count(tokens, BR_WORDS);
    br += lower.matches("a gente").count();
    let pt = european_markers(lower, tokens);

    if br >= VARIANT_MIN_HITS && br >= pt * VARIANT_DOMINANCE {
        SniffLanguage::PtBr
    } else if pt >= VARIANT_MIN_HITS && pt >= br * VARIANT_DOMINANCE {
        SniffLanguage::PtPt
    } else {
        SniffLanguage::Pt
    }
}

fn is_infinitive(token: &str) -> bool {
    SHORT_INFINITIVES.contains(&token)
        || (token.chars().count() >= 5
            && ["ar", "er", "ir", "or"]
                .iter()
                .any(|end| token.ends_with(end)))
}

/// Marcadores exclusivos do português europeu.
fn european_markers(lower: &str, tokens: &[String]) -> usize {
    let mut found = count(tokens, PT_PT_WORDS);
    found += lower.matches("casa de banho").count();
    found += lower.matches("pequeno-almoço").count();
    for (i, token) in tokens.iter().enumerate() {
        // `estou a fazer`, `está a ver`: o BR diz `estou fazendo`.
        if matches!(token.as_str(), "estou" | "está")
            && tokens.get(i + 1).is_some_and(|t| t == "a")
            && tokens.get(i + 2).is_some_and(|t| is_infinitive(t))
        {
            found += 1;
        }
        // `tu fizeste`: o pretérito de 2.ª pessoa com `tu` até 3 palavras antes.
        let preterite = PRETERITE_TU.contains(&token.as_str())
            || (token.len() >= 8 && token.ends_with("aste") && !NOT_ASTE.contains(&token.as_str()));
        if preterite && tokens[i.saturating_sub(3)..i].iter().any(|t| t == "tu") {
            found += 1;
        }
    }
    found + te_enclitics(lower)
}

/// Enclítico `-te` no fim da fala ou antes de pontuação: `amava-te!`,
/// `amo-te.`. É o único pronome ligado por hífen que o BR não escreve; `-o`,
/// `-a`, `-lo`, `-la`, `-lhe` e `-me`/`-se` ficam de fora porque o BR formal
/// também os usa (`vê-lo`, `fazê-lo`). O verbo antes tem de ter 3 letras ou
/// mais, o que afasta siglas e prefixos soltos.
fn te_enclitics(lower: &str) -> usize {
    let chars: Vec<char> = lower.chars().collect();
    let mut found = 0;
    for (i, c) in chars.iter().enumerate() {
        if *c != '-' || chars.get(i + 1) != Some(&'t') || chars.get(i + 2) != Some(&'e') {
            continue;
        }
        let verb = chars[..i]
            .iter()
            .rev()
            .take_while(|c| c.is_alphabetic())
            .count();
        let ends = chars.get(i + 3).is_none_or(|c| {
            matches!(
                c,
                '\n' | '.' | ',' | '!' | '?' | ';' | ':' | '…' | '"' | '\'' | ')'
            )
        });
        if verb >= 3 && ends {
            found += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::format_push_string
    )]

    use super::*;

    fn srt_time(secs: f64) -> String {
        let total = secs as u64;
        format!(
            "{:02}:{:02}:{:02},{:03}",
            total / 3600,
            total % 3600 / 60,
            total % 60,
            ((secs - total as f64) * 1000.0) as u64
        )
    }

    /// Um SRT com as `lines` repetidas em ciclo, `count` falas espalhadas
    /// por `span` segundos, cada uma com 2 s.
    fn srt(lines: &[&str], count: usize, span: f64) -> String {
        let mut out = String::new();
        for n in 0..count {
            let start = span * n as f64 / count as f64;
            out.push_str(&format!(
                "{}\n{} --> {}\n{}\n\n",
                n + 1,
                srt_time(start),
                srt_time(start + 2.0),
                lines[n % lines.len()]
            ));
        }
        out
    }

    const MAN_OF_WAR: &str = "Durante a noite, a Rússia iniciou o seu tão aguardado ataque à \
        Ucrânia, atingindo postos militares por todo o país. Isso parece ser realmente o pior \
        cenário possível. É a evidência daquilo que realmente preocupa o Putin, que é restaurar \
        o poder da Rússia às custas do povo ucraniano.";

    const HOUR_43: f64 = 6180.0;

    fn motor_city() -> String {
        let lines = [
            "Encontra-o. Encontra-o.",
            "Miller.",
            "DECISÃO DE LEVANTAMENTO DE LIBERDADE CONDICIONAL",
            "Sim.",
            "Eu amava-te!",
            "Não me toques, merda.",
        ];
        srt(&lines, 28, HOUR_43)
    }

    #[test]
    fn man_of_war_e_portugues_completo() {
        let text = srt(&[MAN_OF_WAR], 995, 6000.0);
        let sniff = sniff(text.as_bytes(), Some(6000.0));
        // Sem marcador de variante (texto neutro): português não confirmado.
        assert_eq!(sniff.language, SniffLanguage::Pt, "{sniff:?}");
        assert_eq!(sniff.cues, 995);
        assert_eq!(sniff.completeness(), Completeness::Complete);
        assert!(sniff.coverage().unwrap() > 0.3);
    }

    #[test]
    fn motor_city_e_forcada_em_pt_pt() {
        let text = motor_city();
        let sniff = sniff(text.as_bytes(), Some(HOUR_43));
        assert_eq!(sniff.language, SniffLanguage::PtPt, "{sniff:?}");
        assert_eq!(sniff.cues, 28);
        assert_eq!(sniff.completeness(), Completeness::Forced);
    }

    #[test]
    fn marcadores_do_brasil() {
        let text = srt(
            &[
                "Você viu o ônibus? Não, a gente pegou um táxi.",
                "Tá legal, isso é muito bom.",
            ],
            40,
            600.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(600.0)).language,
            SniffLanguage::PtBr
        );
    }

    #[test]
    fn marcadores_de_portugal() {
        let text = srt(
            &[
                "Tu estás na casa de banho? Não, apanhei o autocarro.",
                "Não percebo isso, pá.",
            ],
            40,
            600.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(600.0)).language,
            SniffLanguage::PtPt
        );
        let text = srt(&["Tu disseste que não, eu fiz isso com ele."], 40, 600.0);
        // `disseste` (-este) e `tu` pesam para PT-PT.
        assert_eq!(
            sniff(text.as_bytes(), Some(600.0)).language,
            SniffLanguage::PtPt
        );
    }

    #[test]
    fn ingles() {
        let text = srt(
            &[
                "What do you want? I don't know what the problem is.",
                "It is not the end.",
            ],
            200,
            6000.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(6000.0)).language,
            SniffLanguage::English
        );
    }

    #[test]
    fn espanhol_nao_vira_portugues() {
        let text = srt(
            &[
                "Pero usted no entiende, es muy difícil.",
                "¿Qué quieres? Estoy aquí, señor.",
                "La situación es mala, entonces vamos.",
            ],
            300,
            6000.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(6000.0)).language,
            SniffLanguage::Spanish
        );
    }

    #[test]
    fn latin1_e_decodificado() {
        let text = srt(
            &["Você não está aqui? Isso é uma decisão, então vá com ele."],
            50,
            900.0,
        );
        let latin1: Vec<u8> = text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).expect("cabe em Latin-1"))
            .collect();
        assert!(std::str::from_utf8(&latin1).is_err());
        let sniff = sniff(&latin1, Some(900.0));
        assert_eq!(sniff.language, SniffLanguage::PtBr);
        assert_eq!(sniff.cues, 50);
    }

    #[test]
    fn windows_1252_aspas_e_travessao() {
        assert_eq!(decode(&[0x93, b'a', 0x94, 0x96]), "“a”–");
    }

    #[test]
    fn utf8_com_bom_e_utf16() {
        let text = srt(&["Você não está aqui? Isso é uma decisão."], 20, 300.0);
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(text.as_bytes());
        assert_eq!(sniff(&bom, None).cues, 20);
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(sniff(&utf16, None).cues, 20);
    }

    #[test]
    fn ass_com_tags() {
        let mut ass = String::from(
            "[Script Info]\nTitle: x\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        );
        for n in 0..60 {
            ass.push_str(&format!(
                "Dialogue: 0,0:{:02}:00.00,0:{:02}:03.00,Default,,0,0,0,,{{\\an8}}{{\\i1}}Você não está aqui, isso é uma decisão.\\NEle disse que sim.{{\\i0}}\n",
                n % 60,
                n % 60
            ));
        }
        ass.push_str("Comment: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,não conta\n");
        let sniff = sniff(ass.as_bytes(), Some(3600.0));
        assert_eq!(sniff.cues, 60);
        assert_eq!(sniff.language, SniffLanguage::PtBr);
        assert!((sniff.covered_secs - 180.0).abs() < 0.001);
    }

    #[test]
    fn vtt_e_tags_de_srt() {
        let mut vtt = String::from("WEBVTT\n\n");
        for n in 0..30 {
            vtt.push_str(&format!(
                "00:{n:02}.000 --> 00:{n:02}.900 line:90%\n<i>Você não está aqui?</i> {{\\an8}}Isso é muito bom, então vá.\n\n"
            ));
        }
        let sniff = sniff(vtt.as_bytes(), Some(600.0));
        assert_eq!(sniff.cues, 30);
        assert_eq!(sniff.language, SniffLanguage::PtBr);
    }

    #[test]
    fn texto_curto_ou_sem_palavras_e_desconhecido() {
        assert_eq!(sniff(b"", None).language, SniffLanguage::Unknown);
        let text = srt(&["Miller.", "Putin."], 10, 100.0);
        assert_eq!(
            sniff(text.as_bytes(), None).language,
            SniffLanguage::Unknown
        );
    }

    fn with(cues: usize, covered_secs: f64, video_secs: Option<f64>) -> Sniff {
        Sniff {
            language: SniffLanguage::Pt,
            cues,
            covered_secs,
            video_secs,
        }
    }

    #[test]
    fn forcada_com_muitas_falas_curtas_em_duas_horas() {
        // 160 falas de 2 s em 2 h: passa de 150, mas cobre 4% do filme.
        let numbers = with(160, 320.0, Some(7200.0));
        assert_eq!(numbers.completeness(), Completeness::Forced);
        // Pelo conteúdo do arquivo, não só pelos números.
        let text = srt(
            &["Você viu o ônibus? Não, a gente pegou um táxi."],
            160,
            7200.0,
        );
        let sniff = sniff(text.as_bytes(), Some(7200.0));
        assert_eq!(sniff.cues, 160);
        assert_eq!(sniff.completeness(), Completeness::Forced);
    }

    #[test]
    fn completa_com_pouco_dialogo_mas_cobertura_alta_nunca_e_forcada() {
        // 150 falas longas (10 s) em 2 h: 21% de cobertura, 1 fala a cada 48 s.
        let sniff = with(150, 1500.0, Some(7200.0));
        assert_ne!(sniff.completeness(), Completeness::Forced);
        assert_eq!(sniff.completeness(), Completeness::Inconclusive);
        // Cobertura 33% e uma fala a cada 30 s: completa.
        let sniff = with(240, 2400.0, Some(7200.0));
        assert_eq!(sniff.completeness(), Completeness::Complete);
    }

    #[test]
    fn limites_da_completude() {
        let video = Some(6000.0);
        // Cobertura: < 8% forçada, 8% a 25% inconclusiva, >= 25% completa.
        assert_eq!(with(300, 479.0, video).completeness(), Completeness::Forced);
        assert_eq!(
            with(300, 480.0, video).completeness(),
            Completeness::Inconclusive
        );
        assert_eq!(
            with(300, 1499.0, video).completeness(),
            Completeness::Inconclusive
        );
        assert_eq!(
            with(300, 1500.0, video).completeness(),
            Completeness::Complete
        );
        // Contagem: < duração/120 (50) forçada mesmo com cobertura alta.
        assert_eq!(with(49, 3000.0, video).completeness(), Completeness::Forced);
        // Cobertura alta mas menos de 1 fala a cada 30 s (200): inconclusiva.
        assert_eq!(
            with(199, 3000.0, video).completeness(),
            Completeness::Inconclusive
        );
        assert_eq!(
            with(200, 3000.0, video).completeness(),
            Completeness::Complete
        );
    }

    #[test]
    fn sem_duracao_so_a_contagem_fala() {
        assert_eq!(with(28, 56.0, None).completeness(), Completeness::Forced);
        assert_eq!(with(59, 500.0, None).completeness(), Completeness::Forced);
        assert_eq!(
            with(60, 120.0, None).completeness(),
            Completeness::Inconclusive
        );
        assert_eq!(
            with(799, 2000.0, None).completeness(),
            Completeness::Inconclusive
        );
        assert_eq!(
            with(800, 1600.0, None).completeness(),
            Completeness::Complete
        );
        // Duração zero vale como desconhecida.
        assert_eq!(
            with(400, 800.0, Some(0.0)).completeness(),
            Completeness::Inconclusive
        );
    }

    #[test]
    fn pt_indeterminado_nao_e_pt_br() {
        // Neutra: sem marcador de nenhum lado.
        let text = srt(
            &["Não sei o que se passa. Ele está muito preocupado com isso."],
            100,
            3000.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(3000.0)).language,
            SniffLanguage::Pt
        );
    }

    #[test]
    fn brasileiro_formal_com_enclitico_nao_e_pt_pt() {
        let text = srt(
            &["Eu não posso vê-lo. Isso não foi uma boa decisão."],
            200,
            6000.0,
        );
        let language = sniff(text.as_bytes(), Some(6000.0)).language;
        assert_ne!(language, SniffLanguage::PtPt);
        assert!(matches!(language, SniffLanguage::Pt | SniffLanguage::PtBr));
        // Enclíticos compartilhados não contam nem para o lado de Portugal.
        let text = srt(
            &["Vamos fazê-lo. Encontra-o. Diz-lhe isso. Vê-la não é fácil, não."],
            100,
            3000.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(3000.0)).language,
            SniffLanguage::Pt
        );
    }

    #[test]
    fn variante_exige_tres_marcadores_e_o_dobro_do_outro_lado() {
        let neutral = "Não sei o que se passa. Ele está muito preocupado com isso.";
        let one = |extra: &str| {
            let text = srt(&[&format!("{neutral} {extra}")], 1, 60.0);
            sniff(text.as_bytes(), Some(60.0)).language
        };
        assert_eq!(one("Você viu o ônibus?"), SniffLanguage::Pt);
        assert_eq!(one("Você viu o ônibus? Tá legal."), SniffLanguage::PtBr);
        assert_eq!(one("Pegou o autocarro e o comboio?"), SniffLanguage::Pt);
        assert_eq!(
            one("Pegou o autocarro e o comboio? Foi fixe."),
            SniffLanguage::PtPt
        );
        // Quatro BR contra três PT: falta o dobro.
        assert_eq!(
            one("Você viu o ônibus? Tá legal. Pegou o autocarro e o comboio? Foi fixe."),
            SniffLanguage::Pt
        );
        // Três BR contra dois PT: falta o dobro.
        assert_eq!(
            one("Você viu o ônibus? Tá. Pegou o autocarro e o comboio?"),
            SniffLanguage::Pt
        );
    }

    #[test]
    fn marcadores_exclusivos_de_portugal() {
        let europe = |line: &str| {
            let text = srt(&[line], 30, 900.0);
            sniff(text.as_bytes(), Some(900.0)).language
        };
        assert_eq!(
            europe("Eu amava-te! Ele não está muito bem com isso."),
            SniffLanguage::PtPt
        );
        // `-te` no meio da fala, sem pontuação depois, não conta.
        assert_eq!(
            europe("Eu amava-te muito, ele não está bem com isso."),
            SniffLanguage::Pt
        );
        assert_eq!(
            europe("Ele está a fazer isso e eu estou a ver, não é muito."),
            SniffLanguage::PtPt
        );
        // `está a` sem infinitivo depois (`está a caminho`) não conta.
        assert_eq!(
            europe("Ele está a caminho, não é muito, com isso."),
            SniffLanguage::Pt
        );
        // `-aste` sem `tu` por perto não conta.
        assert_eq!(
            europe("Ele disseste isso, não é muito, com ele."),
            SniffLanguage::Pt
        );
        assert_eq!(
            europe("Tu disseste isso, não é muito, com ele."),
            SniffLanguage::PtPt
        );
        assert_eq!(
            europe("Ele vai à casa de banho, não é muito, com ele."),
            SniffLanguage::PtPt
        );
        assert_eq!(
            europe("Ele tomou o pequeno-almoço, não é muito, com ele."),
            SniffLanguage::PtPt
        );
    }

    #[test]
    fn rapaz_e_enclitico_generico_nao_marcam_portugal() {
        let text = srt(
            &["O rapaz não sabe fazê-lo. Encontra-o, vê-la, diz-lhe, eles são muito isso."],
            100,
            3000.0,
        );
        assert_eq!(
            sniff(text.as_bytes(), Some(3000.0)).language,
            SniffLanguage::Pt
        );
    }

    /// Arquivos reais, fora do repositório: `SNIFF_REAL_DIR` com
    /// `manofwar.srt` e `motorcity.srt`.
    /// `SNIFF_REAL_DIR=/tmp/sniff-real cargo test -p acervo-hub real -- --ignored --nocapture`
    #[test]
    #[ignore = "precisa dos .srt reais em SNIFF_REAL_DIR"]
    fn arquivos_reais() {
        let dir = std::env::var("SNIFF_REAL_DIR").expect("defina SNIFF_REAL_DIR");
        let read = |name: &str, secs: f64| {
            let bytes = std::fs::read(std::path::Path::new(&dir).join(name)).expect("ler o .srt");
            let sniff = sniff(&bytes, Some(secs));
            eprintln!(
                "{name}: {:?} {:?} falas={} cobertura={:?}",
                sniff.language,
                sniff.completeness(),
                sniff.cues,
                sniff.coverage()
            );
            sniff
        };
        let war = read("manofwar.srt", 6000.0);
        assert_eq!(war.language, SniffLanguage::PtBr, "{war:?}");
        assert_eq!(war.completeness(), Completeness::Complete, "{war:?}");
        let city = read("motorcity.srt", 6180.0);
        assert_eq!(city.completeness(), Completeness::Forced, "{city:?}");
    }
}

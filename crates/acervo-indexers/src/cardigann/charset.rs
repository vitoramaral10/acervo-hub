//! Codificações de texto das definições (`encoding:`).
//!
//! A referência lê as páginas e escreve consultas na codificação que a
//! definição declara. UTF-8 e as páginas de código de um byte que aparecem nas
//! definições reais (Latin-1, Windows 125x, ISO-8859-2, 874) são suportadas;
//! codificação de vários bytes (GBK, Big5, `Shift_JIS`...) fica de fora, porque
//! exigiria uma biblioteca de tabelas, e a definição que a pede é recusada na
//! carga. Os 128 códigos acima de ASCII vêm das tabelas abaixo; `U+FFFD` marca
//! o byte indefinido.

/// Codificação de texto de uma definição.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Charset {
    Utf8,
    /// Latin-1: os 256 primeiros pontos de código, byte a byte.
    Latin1,
    /// Os bytes 0x80..=0xFF de uma página de código de um byte.
    Single(&'static [char; 128]),
}

impl Charset {
    /// Codificação por nome (`windows-1251`, `ISO-8859-2`...), sem distinguir
    /// caixa. `None` para o que não é suportado.
    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label.trim().to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" => Self::Utf8,
            "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1" => Self::Latin1,
            "iso-8859-2" | "iso8859-2" | "latin2" => Self::Single(&ISO_8859_2),
            "windows-1250" | "cp1250" => Self::Single(&WINDOWS_1250),
            "windows-1251" | "cp1251" => Self::Single(&WINDOWS_1251),
            "windows-1252" | "cp1252" => Self::Single(&WINDOWS_1252),
            "windows-1255" | "cp1255" => Self::Single(&WINDOWS_1255),
            "windows-1256" | "cp1256" => Self::Single(&WINDOWS_1256),
            // O .NET e os navegadores leem TIS-620 como a página 874.
            "windows-874" | "cp874" | "tis-620" => Self::Single(&WINDOWS_874),
            _ => return None,
        })
    }

    /// Texto a partir dos bytes; `None` se não é UTF-8 válido (as páginas de
    /// código de um byte leem qualquer sequência).
    pub fn decode(self, bytes: &[u8]) -> Option<String> {
        match self {
            Self::Utf8 => String::from_utf8(bytes.to_vec()).ok(),
            Self::Latin1 => Some(bytes.iter().map(|byte| char::from(*byte)).collect()),
            Self::Single(table) => Some(
                bytes
                    .iter()
                    .map(|byte| match byte.checked_sub(0x80) {
                        Some(index) => table[usize::from(index)],
                        None => char::from(*byte),
                    })
                    .collect(),
            ),
        }
    }

    /// Bytes do texto; o que a página de código não tem vira `?`, como o
    /// fallback do .NET.
    pub fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Latin1 => text
                .chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
                .collect(),
            Self::Single(table) => text
                .chars()
                .map(|c| {
                    if c.is_ascii() {
                        return u8::try_from(u32::from(c)).unwrap_or(b'?');
                    }
                    table
                        .iter()
                        .position(|candidate| *candidate == c && c != '\u{fffd}')
                        .and_then(|index| u8::try_from(index + 0x80).ok())
                        .unwrap_or(b'?')
                })
                .collect(),
        }
    }

    /// Percent-encoding de texto, byte a byte na codificação: espaço vira `+`
    /// e `-_.!*()` passam, como o `UrlEncode` da referência.
    pub fn url_encode(self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        for byte in self.encode(text) {
            match byte {
                b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'!'
                | b'*'
                | b'('
                | b')' => output.push(char::from(byte)),
                b' ' => output.push('+'),
                other => {
                    let _ = std::fmt::Write::write_fmt(&mut output, format_args!("%{other:02X}"));
                }
            }
        }
        output
    }

    /// Inverso de `url_encode`: `+` é espaço e `%XX` é um byte da codificação.
    pub fn url_decode(self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'+' => decoded.push(b' '),
                b'%' if bytes
                    .get(index + 1..index + 3)
                    .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit)) =>
                {
                    let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("00");
                    decoded.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                    index += 2;
                }
                other => decoded.push(other),
            }
            index += 1;
        }
        match self {
            Self::Utf8 => String::from_utf8_lossy(&decoded).into_owned(),
            other => other.decode(&decoded).unwrap_or_default(),
        }
    }

    /// `application/x-www-form-urlencoded` de uma lista de pares: o corpo de
    /// um POST ou a query de um GET.
    pub fn form_encode(self, pairs: &[(String, String)]) -> String {
        let mut output = String::new();
        for (key, value) in pairs {
            if !output.is_empty() {
                output.push('&');
            }
            output.push_str(&self.form_component(key));
            output.push('=');
            output.push_str(&self.form_component(value));
        }
        output
    }

    /// Como `url_encode`, mas com a lista do formulário: só `*-._` passam e o
    /// resto é escapado.
    fn form_component(self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        for byte in self.encode(text) {
            match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                    output.push(char::from(byte));
                }
                b' ' => output.push('+'),
                other => {
                    let _ = std::fmt::Write::write_fmt(&mut output, format_args!("%{other:02X}"));
                }
            }
        }
        output
    }
}

const WINDOWS_1250: [char; 128] = [
    '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{fffd}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{fffd}', '\u{2030}', '\u{160}', '\u{2039}', '\u{15a}', '\u{164}', '\u{17d}', '\u{179}',
    '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{fffd}', '\u{2122}', '\u{161}', '\u{203a}', '\u{15b}', '\u{165}', '\u{17e}', '\u{17a}',
    '\u{a0}', '\u{2c7}', '\u{2d8}', '\u{141}', '\u{a4}', '\u{104}', '\u{a6}', '\u{a7}', '\u{a8}',
    '\u{a9}', '\u{15e}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{17b}', '\u{b0}', '\u{b1}',
    '\u{2db}', '\u{142}', '\u{b4}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{b8}', '\u{105}', '\u{15f}',
    '\u{bb}', '\u{13d}', '\u{2dd}', '\u{13e}', '\u{17c}', '\u{154}', '\u{c1}', '\u{c2}', '\u{102}',
    '\u{c4}', '\u{139}', '\u{106}', '\u{c7}', '\u{10c}', '\u{c9}', '\u{118}', '\u{cb}', '\u{11a}',
    '\u{cd}', '\u{ce}', '\u{10e}', '\u{110}', '\u{143}', '\u{147}', '\u{d3}', '\u{d4}', '\u{150}',
    '\u{d6}', '\u{d7}', '\u{158}', '\u{16e}', '\u{da}', '\u{170}', '\u{dc}', '\u{dd}', '\u{162}',
    '\u{df}', '\u{155}', '\u{e1}', '\u{e2}', '\u{103}', '\u{e4}', '\u{13a}', '\u{107}', '\u{e7}',
    '\u{10d}', '\u{e9}', '\u{119}', '\u{eb}', '\u{11b}', '\u{ed}', '\u{ee}', '\u{10f}', '\u{111}',
    '\u{144}', '\u{148}', '\u{f3}', '\u{f4}', '\u{151}', '\u{f6}', '\u{f7}', '\u{159}', '\u{16f}',
    '\u{fa}', '\u{171}', '\u{fc}', '\u{fd}', '\u{163}', '\u{2d9}',
];

const WINDOWS_1251: [char; 128] = [
    '\u{402}', '\u{403}', '\u{201a}', '\u{453}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20ac}', '\u{2030}', '\u{409}', '\u{2039}', '\u{40a}', '\u{40c}', '\u{40b}', '\u{40f}',
    '\u{452}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{fffd}', '\u{2122}', '\u{459}', '\u{203a}', '\u{45a}', '\u{45c}', '\u{45b}', '\u{45f}',
    '\u{a0}', '\u{40e}', '\u{45e}', '\u{408}', '\u{a4}', '\u{490}', '\u{a6}', '\u{a7}', '\u{401}',
    '\u{a9}', '\u{404}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{407}', '\u{b0}', '\u{b1}',
    '\u{406}', '\u{456}', '\u{491}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{451}', '\u{2116}',
    '\u{454}', '\u{bb}', '\u{458}', '\u{405}', '\u{455}', '\u{457}', '\u{410}', '\u{411}',
    '\u{412}', '\u{413}', '\u{414}', '\u{415}', '\u{416}', '\u{417}', '\u{418}', '\u{419}',
    '\u{41a}', '\u{41b}', '\u{41c}', '\u{41d}', '\u{41e}', '\u{41f}', '\u{420}', '\u{421}',
    '\u{422}', '\u{423}', '\u{424}', '\u{425}', '\u{426}', '\u{427}', '\u{428}', '\u{429}',
    '\u{42a}', '\u{42b}', '\u{42c}', '\u{42d}', '\u{42e}', '\u{42f}', '\u{430}', '\u{431}',
    '\u{432}', '\u{433}', '\u{434}', '\u{435}', '\u{436}', '\u{437}', '\u{438}', '\u{439}',
    '\u{43a}', '\u{43b}', '\u{43c}', '\u{43d}', '\u{43e}', '\u{43f}', '\u{440}', '\u{441}',
    '\u{442}', '\u{443}', '\u{444}', '\u{445}', '\u{446}', '\u{447}', '\u{448}', '\u{449}',
    '\u{44a}', '\u{44b}', '\u{44c}', '\u{44d}', '\u{44e}', '\u{44f}',
];

const WINDOWS_1252: [char; 128] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
    '\u{a0}', '\u{a1}', '\u{a2}', '\u{a3}', '\u{a4}', '\u{a5}', '\u{a6}', '\u{a7}', '\u{a8}',
    '\u{a9}', '\u{aa}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{af}', '\u{b0}', '\u{b1}',
    '\u{b2}', '\u{b3}', '\u{b4}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{b8}', '\u{b9}', '\u{ba}',
    '\u{bb}', '\u{bc}', '\u{bd}', '\u{be}', '\u{bf}', '\u{c0}', '\u{c1}', '\u{c2}', '\u{c3}',
    '\u{c4}', '\u{c5}', '\u{c6}', '\u{c7}', '\u{c8}', '\u{c9}', '\u{ca}', '\u{cb}', '\u{cc}',
    '\u{cd}', '\u{ce}', '\u{cf}', '\u{d0}', '\u{d1}', '\u{d2}', '\u{d3}', '\u{d4}', '\u{d5}',
    '\u{d6}', '\u{d7}', '\u{d8}', '\u{d9}', '\u{da}', '\u{db}', '\u{dc}', '\u{dd}', '\u{de}',
    '\u{df}', '\u{e0}', '\u{e1}', '\u{e2}', '\u{e3}', '\u{e4}', '\u{e5}', '\u{e6}', '\u{e7}',
    '\u{e8}', '\u{e9}', '\u{ea}', '\u{eb}', '\u{ec}', '\u{ed}', '\u{ee}', '\u{ef}', '\u{f0}',
    '\u{f1}', '\u{f2}', '\u{f3}', '\u{f4}', '\u{f5}', '\u{f6}', '\u{f7}', '\u{f8}', '\u{f9}',
    '\u{fa}', '\u{fb}', '\u{fc}', '\u{fd}', '\u{fe}', '\u{ff}',
];

const WINDOWS_1255: [char; 128] = [
    '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{fffd}', '\u{2039}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
    '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{fffd}', '\u{203a}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
    '\u{a0}', '\u{a1}', '\u{a2}', '\u{a3}', '\u{20aa}', '\u{a5}', '\u{a6}', '\u{a7}', '\u{a8}',
    '\u{a9}', '\u{d7}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{af}', '\u{b0}', '\u{b1}',
    '\u{b2}', '\u{b3}', '\u{b4}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{b8}', '\u{b9}', '\u{f7}',
    '\u{bb}', '\u{bc}', '\u{bd}', '\u{be}', '\u{bf}', '\u{5b0}', '\u{5b1}', '\u{5b2}', '\u{5b3}',
    '\u{5b4}', '\u{5b5}', '\u{5b6}', '\u{5b7}', '\u{5b8}', '\u{5b9}', '\u{fffd}', '\u{5bb}',
    '\u{5bc}', '\u{5bd}', '\u{5be}', '\u{5bf}', '\u{5c0}', '\u{5c1}', '\u{5c2}', '\u{5c3}',
    '\u{5f0}', '\u{5f1}', '\u{5f2}', '\u{5f3}', '\u{5f4}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
    '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{5d0}', '\u{5d1}', '\u{5d2}', '\u{5d3}',
    '\u{5d4}', '\u{5d5}', '\u{5d6}', '\u{5d7}', '\u{5d8}', '\u{5d9}', '\u{5da}', '\u{5db}',
    '\u{5dc}', '\u{5dd}', '\u{5de}', '\u{5df}', '\u{5e0}', '\u{5e1}', '\u{5e2}', '\u{5e3}',
    '\u{5e4}', '\u{5e5}', '\u{5e6}', '\u{5e7}', '\u{5e8}', '\u{5e9}', '\u{5ea}', '\u{fffd}',
    '\u{fffd}', '\u{200e}', '\u{200f}', '\u{fffd}',
];

const WINDOWS_1256: [char; 128] = [
    '\u{20ac}', '\u{67e}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{679}', '\u{2039}', '\u{152}', '\u{686}', '\u{698}', '\u{688}',
    '\u{6af}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{6a9}', '\u{2122}', '\u{691}', '\u{203a}', '\u{153}', '\u{200c}', '\u{200d}', '\u{6ba}',
    '\u{a0}', '\u{60c}', '\u{a2}', '\u{a3}', '\u{a4}', '\u{a5}', '\u{a6}', '\u{a7}', '\u{a8}',
    '\u{a9}', '\u{6be}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{af}', '\u{b0}', '\u{b1}',
    '\u{b2}', '\u{b3}', '\u{b4}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{b8}', '\u{b9}', '\u{61b}',
    '\u{bb}', '\u{bc}', '\u{bd}', '\u{be}', '\u{61f}', '\u{6c1}', '\u{621}', '\u{622}', '\u{623}',
    '\u{624}', '\u{625}', '\u{626}', '\u{627}', '\u{628}', '\u{629}', '\u{62a}', '\u{62b}',
    '\u{62c}', '\u{62d}', '\u{62e}', '\u{62f}', '\u{630}', '\u{631}', '\u{632}', '\u{633}',
    '\u{634}', '\u{635}', '\u{636}', '\u{d7}', '\u{637}', '\u{638}', '\u{639}', '\u{63a}',
    '\u{640}', '\u{641}', '\u{642}', '\u{643}', '\u{e0}', '\u{644}', '\u{e2}', '\u{645}',
    '\u{646}', '\u{647}', '\u{648}', '\u{e7}', '\u{e8}', '\u{e9}', '\u{ea}', '\u{eb}', '\u{649}',
    '\u{64a}', '\u{ee}', '\u{ef}', '\u{64b}', '\u{64c}', '\u{64d}', '\u{64e}', '\u{f4}', '\u{64f}',
    '\u{650}', '\u{f7}', '\u{651}', '\u{f9}', '\u{652}', '\u{fb}', '\u{fc}', '\u{200e}',
    '\u{200f}', '\u{6d2}',
];

const WINDOWS_874: [char; 128] = [
    '\u{20ac}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{2026}', '\u{fffd}', '\u{fffd}',
    '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
    '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
    '\u{a0}', '\u{e01}', '\u{e02}', '\u{e03}', '\u{e04}', '\u{e05}', '\u{e06}', '\u{e07}',
    '\u{e08}', '\u{e09}', '\u{e0a}', '\u{e0b}', '\u{e0c}', '\u{e0d}', '\u{e0e}', '\u{e0f}',
    '\u{e10}', '\u{e11}', '\u{e12}', '\u{e13}', '\u{e14}', '\u{e15}', '\u{e16}', '\u{e17}',
    '\u{e18}', '\u{e19}', '\u{e1a}', '\u{e1b}', '\u{e1c}', '\u{e1d}', '\u{e1e}', '\u{e1f}',
    '\u{e20}', '\u{e21}', '\u{e22}', '\u{e23}', '\u{e24}', '\u{e25}', '\u{e26}', '\u{e27}',
    '\u{e28}', '\u{e29}', '\u{e2a}', '\u{e2b}', '\u{e2c}', '\u{e2d}', '\u{e2e}', '\u{e2f}',
    '\u{e30}', '\u{e31}', '\u{e32}', '\u{e33}', '\u{e34}', '\u{e35}', '\u{e36}', '\u{e37}',
    '\u{e38}', '\u{e39}', '\u{e3a}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{e3f}',
    '\u{e40}', '\u{e41}', '\u{e42}', '\u{e43}', '\u{e44}', '\u{e45}', '\u{e46}', '\u{e47}',
    '\u{e48}', '\u{e49}', '\u{e4a}', '\u{e4b}', '\u{e4c}', '\u{e4d}', '\u{e4e}', '\u{e4f}',
    '\u{e50}', '\u{e51}', '\u{e52}', '\u{e53}', '\u{e54}', '\u{e55}', '\u{e56}', '\u{e57}',
    '\u{e58}', '\u{e59}', '\u{e5a}', '\u{e5b}', '\u{fffd}', '\u{fffd}', '\u{fffd}', '\u{fffd}',
];

const ISO_8859_2: [char; 128] = [
    '\u{80}', '\u{81}', '\u{82}', '\u{83}', '\u{84}', '\u{85}', '\u{86}', '\u{87}', '\u{88}',
    '\u{89}', '\u{8a}', '\u{8b}', '\u{8c}', '\u{8d}', '\u{8e}', '\u{8f}', '\u{90}', '\u{91}',
    '\u{92}', '\u{93}', '\u{94}', '\u{95}', '\u{96}', '\u{97}', '\u{98}', '\u{99}', '\u{9a}',
    '\u{9b}', '\u{9c}', '\u{9d}', '\u{9e}', '\u{9f}', '\u{a0}', '\u{104}', '\u{2d8}', '\u{141}',
    '\u{a4}', '\u{13d}', '\u{15a}', '\u{a7}', '\u{a8}', '\u{160}', '\u{15e}', '\u{164}', '\u{179}',
    '\u{ad}', '\u{17d}', '\u{17b}', '\u{b0}', '\u{105}', '\u{2db}', '\u{142}', '\u{b4}', '\u{13e}',
    '\u{15b}', '\u{2c7}', '\u{b8}', '\u{161}', '\u{15f}', '\u{165}', '\u{17a}', '\u{2dd}',
    '\u{17e}', '\u{17c}', '\u{154}', '\u{c1}', '\u{c2}', '\u{102}', '\u{c4}', '\u{139}', '\u{106}',
    '\u{c7}', '\u{10c}', '\u{c9}', '\u{118}', '\u{cb}', '\u{11a}', '\u{cd}', '\u{ce}', '\u{10e}',
    '\u{110}', '\u{143}', '\u{147}', '\u{d3}', '\u{d4}', '\u{150}', '\u{d6}', '\u{d7}', '\u{158}',
    '\u{16e}', '\u{da}', '\u{170}', '\u{dc}', '\u{dd}', '\u{162}', '\u{df}', '\u{155}', '\u{e1}',
    '\u{e2}', '\u{103}', '\u{e4}', '\u{13a}', '\u{107}', '\u{e7}', '\u{10d}', '\u{e9}', '\u{119}',
    '\u{eb}', '\u{11b}', '\u{ed}', '\u{ee}', '\u{10f}', '\u{111}', '\u{144}', '\u{148}', '\u{f3}',
    '\u{f4}', '\u{151}', '\u{f6}', '\u{f7}', '\u{159}', '\u{16f}', '\u{fa}', '\u{171}', '\u{fc}',
    '\u{fd}', '\u{163}', '\u{2d9}',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotulos_conhecidos_e_o_que_fica_de_fora() {
        for label in [
            "UTF-8",
            "iso-8859-1",
            "ISO-8859-2",
            "windows-1251",
            "tis-620",
        ] {
            assert!(Charset::from_label(label).is_some(), "{label}");
        }
        for label in ["shift_jis", "gbk", "big5", "euc-kr", ""] {
            assert!(Charset::from_label(label).is_none(), "{label}");
        }
    }

    #[test]
    fn cirilico_ida_e_volta_em_windows_1251() {
        let charset = Charset::from_label("windows-1251").unwrap();
        // "Привет" em windows-1251.
        let bytes = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(charset.decode(&bytes).as_deref(), Some("Привет"));
        assert_eq!(charset.encode("Привет"), bytes);
        assert_eq!(
            charset.url_encode("Привет мир"),
            "%CF%F0%E8%E2%E5%F2+%EC%E8%F0"
        );
        assert_eq!(
            charset.url_decode("%CF%F0%E8%E2%E5%F2+%EC%E8%F0"),
            "Привет мир"
        );
        // Fora da página de código vira `?`.
        assert_eq!(charset.encode("a漢"), b"a?");
    }

    #[test]
    fn latin1_latin2_e_utf8() {
        let latin1 = Charset::from_label("ISO-8859-1").unwrap();
        assert_eq!(
            latin1.decode(&[0x63, 0x61, 0x66, 0xE9]).as_deref(),
            Some("café")
        );
        assert_eq!(latin1.encode("café"), [0x63, 0x61, 0x66, 0xE9]);
        let latin2 = Charset::from_label("iso-8859-2").unwrap();
        // "Łódź" em ISO-8859-2.
        assert_eq!(
            latin2.decode(&[0xA3, 0xF3, 0x64, 0xBC]).as_deref(),
            Some("Łódź")
        );
        assert_eq!(latin2.encode("Łódź"), [0xA3, 0xF3, 0x64, 0xBC]);
        let utf8 = Charset::Utf8;
        assert!(utf8.decode(&[0xFF, 0xFE]).is_none());
        assert_eq!(utf8.url_encode("é ok"), "%C3%A9+ok");
    }

    #[test]
    fn formulario_codifica_chave_e_valor_na_codificacao() {
        let charset = Charset::from_label("windows-1251").unwrap();
        let pairs = [
            ("q".to_owned(), "дом 2".to_owned()),
            ("a b".to_owned(), "x&y".to_owned()),
        ];
        assert_eq!(charset.form_encode(&pairs), "q=%E4%EE%EC+2&a+b=x%26y");
        assert_eq!(
            Charset::Utf8.form_encode(&pairs[..1]),
            "q=%D0%B4%D0%BE%D0%BC+2"
        );
    }
}

//! Identidade de um torrent antes de mandá-lo ao cliente.
//!
//! O qBittorrent não devolve o hash de um torrent adicionado; ele é o
//! SHA-1 do dicionário `info` do `.torrent`, exatamente os bytes como vieram,
//! ou o `btih` de um link magnet. Com ele se acha o torrent depois.

use sha1::{Digest, Sha1};

/// Fim do valor bencode que começa em `at`, ou `None` se malformado.
fn skip(bytes: &[u8], at: usize) -> Option<usize> {
    match *bytes.get(at)? {
        b'i' => Some(at + bytes[at..].iter().position(|&b| b == b'e')? + 1),
        b'l' | b'd' => {
            let mut at = at + 1;
            while *bytes.get(at)? != b'e' {
                at = skip(bytes, at)?;
            }
            Some(at + 1)
        }
        b'0'..=b'9' => {
            let colon = at + bytes[at..].iter().position(|&b| b == b':')?;
            let length: usize = std::str::from_utf8(&bytes[at..colon]).ok()?.parse().ok()?;
            let end = colon.checked_add(1)?.checked_add(length)?;
            (end <= bytes.len()).then_some(end)
        }
        _ => None,
    }
}

/// Uma string bencode em `at`: o conteúdo e onde ela termina.
fn string(bytes: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let end = skip(bytes, at)?;
    let colon = at + bytes[at..end].iter().position(|&b| b == b':')?;
    Some((&bytes[colon + 1..end], end))
}

/// Um inteiro bencode em `at`: o valor e onde ele termina.
fn integer(bytes: &[u8], at: usize) -> Option<(i64, usize)> {
    if *bytes.get(at)? != b'i' {
        return None;
    }
    let end = skip(bytes, at)?;
    let value = std::str::from_utf8(&bytes[at + 1..end - 1])
        .ok()?
        .parse()
        .ok()?;
    Some((value, end))
}

/// As entradas de um dicionário bencode em `at`: chave e onde o valor começa.
fn entries(bytes: &[u8], at: usize) -> Option<Vec<(&[u8], usize)>> {
    if *bytes.get(at)? != b'd' {
        return None;
    }
    let mut out = Vec::new();
    let mut at = at + 1;
    while *bytes.get(at)? != b'e' {
        let (key, value_at) = string(bytes, at)?;
        out.push((key, value_at));
        at = skip(bytes, value_at)?;
    }
    Some(out)
}

/// Os itens de uma lista bencode em `at`: onde cada um começa.
fn items(bytes: &[u8], at: usize) -> Option<Vec<usize>> {
    if *bytes.get(at)? != b'l' {
        return None;
    }
    let mut out = Vec::new();
    let mut at = at + 1;
    while *bytes.get(at)? != b'e' {
        out.push(at);
        at = skip(bytes, at)?;
    }
    Some(out)
}

/// O valor da chave, preferindo a variante `.utf-8` quando o torrent a traz
/// (é a que o cliente usa para nomear o arquivo).
fn utf8_or(found: &[(&[u8], usize)], key: &str) -> Option<usize> {
    let utf8 = format!("{key}.utf-8");
    found
        .iter()
        .find(|(k, _)| *k == utf8.as_bytes())
        .or_else(|| found.iter().find(|(k, _)| *k == key.as_bytes()))
        .map(|(_, at)| *at)
}

/// Os arquivos de um `.torrent`, como o cliente os põe no disco: caminho
/// relativo à pasta de download (com a pasta raiz, se há mais de um) e
/// tamanho. É o que decide se dois torrents de trackers diferentes são o
/// mesmo conteúdo e podem semear dos mesmos arquivos.
#[must_use]
pub fn layout(torrent: &[u8]) -> Option<Vec<(String, u64)>> {
    let top = entries(torrent, 0)?;
    let info = top.iter().find(|(k, _)| *k == b"info")?.1;
    let info = entries(torrent, info)?;
    let name = String::from_utf8(string(torrent, utf8_or(&info, "name")?)?.0.to_vec()).ok()?;
    if let Some(at) = info
        .iter()
        .find(|(k, _)| *k == b"length")
        .map(|(_, at)| *at)
    {
        let (length, _) = integer(torrent, at)?;
        return Some(vec![(name, u64::try_from(length).ok()?)]);
    }
    let files = info.iter().find(|(k, _)| *k == b"files")?.1;
    let mut out = Vec::new();
    for file in items(torrent, files)? {
        let fields = entries(torrent, file)?;
        // Preenchimento do BEP 47: nunca vai ao disco nem à lista do cliente.
        let padding = fields
            .iter()
            .find(|(k, _)| *k == b"attr")
            .and_then(|(_, at)| string(torrent, *at))
            .is_some_and(|(attr, _)| attr.contains(&b'p'));
        if padding {
            continue;
        }
        let path_at = utf8_or(&fields, "path")?;
        let parts = items(torrent, path_at)?;
        // Preenchimento do BitComet, sem `attr`: o cliente também o esconde.
        let bitcomet = parts
            .last()
            .and_then(|at| string(torrent, *at))
            .is_some_and(|(last, _)| last.starts_with(b"_____padding_file_"));
        if bitcomet {
            continue;
        }
        let length = fields.iter().find(|(k, _)| *k == b"length")?.1;
        let (length, _) = integer(torrent, length)?;
        let mut path = name.clone();
        for part in parts {
            path.push('/');
            path.push_str(std::str::from_utf8(string(torrent, part)?.0).ok()?);
        }
        out.push((path, u64::try_from(length).ok()?));
    }
    Some(out)
}

/// O que prova que dois `.torrent` carregam o mesmo conteúdo: o tamanho da
/// peça e os hashes das peças (`pieces`), exatamente como vieram. Campos
/// soltos do `info` (como `source`, que muda o infohash) não entram.
#[must_use]
pub fn piece_digest(torrent: &[u8]) -> Option<(u64, Vec<u8>)> {
    let top = entries(torrent, 0)?;
    let info = top.iter().find(|(k, _)| *k == b"info")?.1;
    let info = entries(torrent, info)?;
    let length = info.iter().find(|(k, _)| *k == b"piece length")?.1;
    let (length, _) = integer(torrent, length)?;
    let pieces = info.iter().find(|(k, _)| *k == b"pieces")?.1;
    let (pieces, _) = string(torrent, pieces)?;
    Some((u64::try_from(length).ok()?, pieces.to_vec()))
}

/// A URL de `announce` do `.torrent`. É segredo (leva a passkey): quem a usa
/// não a loga nem a devolve.
#[must_use]
pub fn announce(torrent: &[u8]) -> Option<String> {
    let top = entries(torrent, 0)?;
    let at = top.iter().find(|(k, _)| *k == b"announce")?.1;
    let url = String::from_utf8(string(torrent, at)?.0.to_vec()).ok()?;
    (!url.is_empty()).then_some(url)
}

/// Infohash v1 (hex minúsculo) de um arquivo `.torrent`.
#[must_use]
pub fn info_hash(torrent: &[u8]) -> Option<String> {
    if torrent.first() != Some(&b'd') {
        return None;
    }
    let mut at = 1;
    while *torrent.get(at)? != b'e' {
        let (key, value_at) = string(torrent, at)?;
        let value_end = skip(torrent, value_at)?;
        if key == b"info" {
            return Some(hex(&Sha1::digest(&torrent[value_at..value_end])));
        }
        at = value_end;
    }
    None
}

/// Infohash (hex minúsculo) de um link magnet com `xt=urn:btih:`. Aceita as
/// duas grafias: 40 caracteres hex ou 32 em base32.
#[must_use]
pub fn magnet_hash(magnet: &str) -> Option<String> {
    let query = magnet.strip_prefix("magnet:?")?;
    let btih = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| {
            (key == "xt")
                .then(|| value.strip_prefix("urn:btih:"))
                .flatten()
        })?;
    match btih.len() {
        40 if btih.bytes().all(|b| b.is_ascii_hexdigit()) => Some(btih.to_ascii_lowercase()),
        32 => base32(btih).map(|bytes| hex(&bytes)),
        _ => None,
    }
}

fn base32(text: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut count = 0;
    let mut out = Vec::with_capacity(20);
    for c in text.bytes() {
        let value = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        bits = (bits << 5) | u64::from(value);
        count += 5;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(40), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_sai_do_nivel_de_cima_e_vazio_ou_ausente_e_none() {
        let info = b"4:infod6:lengthi1e4:name1:aee";
        let mut torrent = b"d8:announce22:http://t.x/a?k=SEGREDO".to_vec();
        torrent.extend_from_slice(info);
        assert_eq!(
            announce(&torrent).as_deref(),
            Some("http://t.x/a?k=SEGREDO")
        );
        assert_eq!(announce(b"d4:infod4:name1:aee"), None);
        assert_eq!(announce(b"d8:announce0:4:infod4:name1:aee"), None);
        assert_eq!(announce(b"lixo"), None);
    }

    #[test]
    fn piece_digest_ignora_o_resto_do_info_e_exige_os_dois_campos() {
        let a = b"d4:infod6:lengthi10e4:name1:a12:piece lengthi16384e6:pieces3:abc6:source1:xee";
        let b = b"d4:infod6:lengthi10e4:name1:a12:piece lengthi16384e6:pieces3:abc6:source1:yee";
        assert_eq!(piece_digest(a), Some((16384, b"abc".to_vec())));
        assert_eq!(piece_digest(a), piece_digest(b));
        assert_ne!(info_hash(a), info_hash(b));
        let other = b"d4:infod6:lengthi10e4:name1:a12:piece lengthi16384e6:pieces3:abdee";
        assert_ne!(piece_digest(a), piece_digest(other));
        let bigger = b"d4:infod6:lengthi10e4:name1:a12:piece lengthi32768e6:pieces3:abcee";
        assert_ne!(piece_digest(a), piece_digest(bigger));
        assert_eq!(
            piece_digest(b"d4:infod6:lengthi1e4:name1:a6:pieces1:xee"),
            None
        );
        assert_eq!(piece_digest(b"d4:infod12:piece lengthi1ee"), None);
        assert_eq!(piece_digest(b"lixo"), None);
    }

    #[test]
    fn hash_e_o_sha1_dos_bytes_do_info() {
        let info = b"d6:lengthi10e4:name5:a.mkv12:piece lengthi16384e6:pieces0:e";
        let mut torrent = b"d8:announce9:http://x/4:info".to_vec();
        torrent.extend_from_slice(info);
        torrent.extend_from_slice(b"7:comment3:olae");
        assert_eq!(info_hash(&torrent), Some(hex(&Sha1::digest(info))));
    }

    #[test]
    fn torrent_malformado_nao_tem_hash() {
        assert_eq!(info_hash(b"d4:infod4:name"), None);
        assert_eq!(info_hash(b"<html>login</html>"), None);
        assert_eq!(info_hash(b"d8:announce3:urle"), None);
        assert_eq!(info_hash(b"d4:info99999999999999999999:xe"), None);
    }

    #[test]
    fn layout_de_arquivo_unico_e_de_pasta() {
        let single = b"d4:infod6:lengthi10e4:name5:a.mkv12:piece lengthi16384e6:pieces0:ee";
        assert_eq!(layout(single), Some(vec![("a.mkv".to_owned(), 10)]));
        let multi = b"d4:infod5:filesld6:lengthi3e4:pathl3:sub5:b.mkveed6:lengthi4e4:pathl5:c.srteee4:name4:Raiz6:pieces0:ee";
        assert_eq!(
            layout(multi),
            Some(vec![
                ("Raiz/sub/b.mkv".to_owned(), 3),
                ("Raiz/c.srt".to_owned(), 4)
            ])
        );
        // Arquivo de preenchimento (BEP 47) fica de fora.
        let pad = b"d4:infod5:filesld6:lengthi3e4:pathl1:aeed4:attr1:p6:lengthi5e4:pathl4:.pad1:5eee4:name1:R6:pieces0:ee";
        assert_eq!(layout(pad), Some(vec![("R/a".to_owned(), 3)]));
        let bitcomet = b"d4:infod5:filesld6:lengthi3e4:pathl1:aeed6:lengthi5e4:pathl30:_____padding_file_0_if_you_seeeee4:name1:R6:pieces0:ee";
        assert_eq!(layout(bitcomet), Some(vec![("R/a".to_owned(), 3)]));
        // A variante `.utf-8` vence a outra.
        let utf8 = b"d4:infod6:lengthi1e4:name1:x10:name.utf-81:ye6:pieces0:e";
        assert_eq!(layout(utf8), Some(vec![("y".to_owned(), 1)]));
        assert_eq!(layout(b"<html>login</html>"), None);
    }

    #[test]
    fn magnet_em_hex_e_em_base32() {
        let hex_link = "magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A&dn=x";
        assert_eq!(
            magnet_hash(hex_link).as_deref(),
            Some("c12fe1c06bba254a9dc9f519b335aa7c1367a88a")
        );
        let base32_link = "magnet:?dn=x&xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK";
        assert_eq!(
            magnet_hash(base32_link).as_deref(),
            Some("c12fe1c06bba254a9dc9f519b335aa7c1367a88a")
        );
        assert_eq!(magnet_hash("https://tracker/x.torrent"), None);
    }
}

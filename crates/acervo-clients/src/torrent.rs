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

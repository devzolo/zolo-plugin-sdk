//! On-exe embedded plugin payload (`zolo build --plugins=embed`).
//!
//! `zolo build` appends this layout to the END of the linked executable
//! (PE/ELF/Mach-O tolerate trailing data — same trick as `.zex --standalone`):
//!
//!   [dll bytes…]* `TOC` [footer — `FOOTER_LEN` bytes, at EOF]
//!
//! footer: magic "ZPLGEMB1" (8) | version u32 LE | toc_len u32 LE | toc_offset u64 LE
//! TOC v1: count u32 LE, then per entry:
//!   name_len u16 LE | name (utf-8) | offset u64 LE (absolute file offset)
//!   | len u64 LE | hash u64 LE (fnv1a64 of the dll bytes)
//!
//! The hash names the runtime extraction dir (`~/.zolo/plugins-cache/<hash>-<len>/`)
//! so a new exe version never collides with a cached older dll. Both sides of
//! the format live here so zolo-native (writer) and zolo-runtime (reader)
//! can never drift.

pub const EMBED_MAGIC: &[u8; 8] = b"ZPLGEMB1";
pub const EMBED_VERSION: u32 = 1;
pub const FOOTER_LEN: usize = 24;

pub struct TocEntry {
    pub name: String,
    pub offset: u64,
    pub len: u64,
    pub hash: u64,
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn serialize_footer(toc_offset: u64, toc_len: u32) -> [u8; FOOTER_LEN] {
    let mut out = [0u8; FOOTER_LEN];
    out[..8].copy_from_slice(EMBED_MAGIC);
    out[8..12].copy_from_slice(&EMBED_VERSION.to_le_bytes());
    out[12..16].copy_from_slice(&toc_len.to_le_bytes());
    out[16..24].copy_from_slice(&toc_offset.to_le_bytes());
    out
}

pub fn parse_footer(tail: &[u8]) -> Option<(u64, u32)> {
    if tail.len() != FOOTER_LEN || &tail[..8] != EMBED_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(tail[8..12].try_into().ok()?);
    if version != EMBED_VERSION {
        return None;
    }
    let toc_len = u32::from_le_bytes(tail[12..16].try_into().ok()?);
    let toc_offset = u64::from_le_bytes(tail[16..24].try_into().ok()?);
    Some((toc_offset, toc_len))
}

pub fn serialize_toc(entries: &[TocEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&e.offset.to_le_bytes());
        out.extend_from_slice(&e.len.to_le_bytes());
        out.extend_from_slice(&e.hash.to_le_bytes());
    }
    out
}

pub fn parse_toc(bytes: &[u8]) -> Result<Vec<TocEntry>, String> {
    fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], String> {
        let end = pos.checked_add(n).filter(|&e| e <= bytes.len());
        let end = end.ok_or_else(|| "truncated embedded-plugin TOC".to_string())?;
        let out = &bytes[*pos..end];
        *pos = end;
        Ok(out)
    }
    let mut pos = 0usize;
    let count = u32::from_le_bytes(take(bytes, &mut pos, 4)?.try_into().unwrap());
    // Bound `count` against what the buffer could possibly hold BEFORE
    // allocating: a corrupted TOC with count=u32::MAX would otherwise ask
    // `Vec::with_capacity` for ~200GB and abort the process, defeating the
    // Result-based error contract.
    const MIN_ENTRY_LEN: usize = 26; // name_len(2) + empty name + offset(8) + len(8) + hash(8)
    if (count as usize).saturating_mul(MIN_ENTRY_LEN) > bytes.len().saturating_sub(4) {
        return Err("embedded-plugin TOC count exceeds buffer size".to_string());
    }
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_len = u16::from_le_bytes(take(bytes, &mut pos, 2)?.try_into().unwrap());
        let name = String::from_utf8(take(bytes, &mut pos, name_len as usize)?.to_vec())
            .map_err(|_| "embedded-plugin TOC name is not UTF-8".to_string())?;
        let offset = u64::from_le_bytes(take(bytes, &mut pos, 8)?.try_into().unwrap());
        let len = u64::from_le_bytes(take(bytes, &mut pos, 8)?.try_into().unwrap());
        let hash = u64::from_le_bytes(take(bytes, &mut pos, 8)?.try_into().unwrap());
        entries.push(TocEntry {
            name,
            offset,
            len,
            hash,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_round_trip() {
        let f = serialize_footer(0xDEAD_BEEF, 42);
        assert_eq!(f.len(), FOOTER_LEN);
        assert_eq!(parse_footer(&f), Some((0xDEAD_BEEF, 42)));
        // wrong magic → None
        let mut bad = f;
        bad[0] = b'X';
        assert_eq!(parse_footer(&bad), None);
        // short slice → None
        assert_eq!(parse_footer(&f[..10]), None);
    }

    #[test]
    fn toc_round_trip() {
        let entries = vec![
            TocEntry {
                name: "raylib".into(),
                offset: 100,
                len: 5000,
                hash: fnv1a64(b"abc"),
            },
            TocEntry {
                name: "base64".into(),
                offset: 5100,
                len: 7,
                hash: fnv1a64(b"xyzzy"),
            },
        ];
        let bytes = serialize_toc(&entries);
        let parsed = parse_toc(&bytes).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "raylib");
        assert_eq!(parsed[0].offset, 100);
        assert_eq!(parsed[1].len, 7);
        assert_eq!(parsed[1].hash, fnv1a64(b"xyzzy"));
        // truncated TOC → Err
        assert!(parse_toc(&bytes[..bytes.len() - 3]).is_err());
    }

    #[test]
    fn toc_with_absurd_count_errors() {
        // A corrupted TOC claiming u32::MAX entries must fail fast with Err,
        // not abort the process trying to reserve ~200GB up front.
        let mut bytes = u32::MAX.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 8]); // a few junk bytes
        assert!(parse_toc(&bytes).is_err());
    }

    #[test]
    fn toc_with_non_utf8_name_errors() {
        // Hand-build an otherwise-valid buffer whose name byte is not UTF-8:
        // count=1 | name_len=1 | name byte 0xFF | offset | len | hash.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.push(0xFF);
        bytes.extend_from_slice(&100u64.to_le_bytes());
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&fnv1a64(b"abc").to_le_bytes());
        assert!(parse_toc(&bytes).is_err());
    }

    #[test]
    fn fnv_is_stable() {
        // Pinned so the build side (zolo-native) and runtime side agree forever.
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
    }
}

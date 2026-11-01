//! Text decoding for opened Markdown files.
//!
//! Markdown is usually UTF-8, but real files also come with a UTF-8 BOM,
//! UTF-16 (Windows editors) or legacy 8-bit encodings. Reading everything with
//! `read_to_string` rejects those files. This module detects the common cases
//! and returns a `String` with the BOM removed.

use std::io;
use std::path::Path;

/// Reads a Markdown/text file, detecting UTF-8, UTF-16 (BOM) and legacy
/// 8-bit encodings.
pub fn read_text(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    decode(&bytes).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// Decodes file bytes into text.
fn decode(bytes: &[u8]) -> io::Result<String> {
    // BOM-marked encodings first.
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 after BOM"));
    }
    let utf16 = [
        (&[0xFF, 0xFE][..], encoding_rs::UTF_16LE),
        (&[0xFE, 0xFF][..], encoding_rs::UTF_16BE),
    ];
    if let Some((bom, encoding)) = utf16.into_iter().find(|(bom, _)| bytes.starts_with(bom)) {
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[bom.len()..]);
        if had_errors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid {} text", encoding.name()),
            ));
        }
        return Ok(text.into_owned());
    }

    // Plain UTF-8.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }

    // Legacy single-byte encodings: only accept if there are no NUL bytes,
    // a strong hint that this is not binary data.
    if bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a UTF-8/UTF-16 text file",
        ));
    }
    // WHATWG windows-1252 maps all 256 bytes (the five undefined slots become
    // C1 controls), so this decoding cannot fail.
    let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
    Ok(text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_utf8_is_unchanged() {
        assert_eq!(decode("café ✅".as_bytes()).unwrap(), "café ✅");
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"---\ntitle: Test\n---\n# Hi\n");
        let text = decode(&bytes).unwrap();
        assert!(text.starts_with("---\ntitle: Test"));
    }

    #[test]
    fn utf16_le_and_be_are_decoded() {
        let text = "# Café\n";
        let mut le = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode(&le).unwrap(), text);

        let mut be = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            be.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode(&be).unwrap(), text);
    }

    #[test]
    fn windows_1252_is_a_fallback() {
        // "café" in Windows-1252 (0xE9 instead of UTF-8's 0xC3 0xA9).
        assert_eq!(decode(b"caf\xE9").unwrap(), "café");
    }

    /// Windows-1252's undefined slots decode to C1 controls instead of failing.
    #[test]
    fn windows_1252_undefined_slots_do_not_fail() {
        assert_eq!(decode(b"a\x81b").unwrap(), "a\u{81}b");
    }

    #[test]
    fn binary_data_is_rejected() {
        assert!(decode(&[0, 1, 2, 0xFF]).is_err());
    }
}

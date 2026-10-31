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
pub fn decode(bytes: &[u8]) -> io::Result<String> {
    // BOM-marked encodings first.
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 after BOM"));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let (text, _, had_errors) = encoding_rs::UTF_16LE.decode(rest);
        if had_errors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid UTF-16LE text",
            ));
        }
        return Ok(text.into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (text, _, had_errors) = encoding_rs::UTF_16BE.decode(rest);
        if had_errors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid UTF-16BE text",
            ));
        }
        return Ok(text.into_owned());
    }

    // Plain UTF-8.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }

    // Legacy single-byte encodings: only accept if there are no NUL bytes
    // (a strong hint that this is not binary data) and every byte maps to a
    // character (Windows-1252 has a few undefined slots).
    if bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a UTF-8/UTF-16 text file",
        ));
    }
    match encoding_rs::WINDOWS_1252.decode_without_bom_handling_and_without_replacement(bytes) {
        Some(text) => Ok(text.into_owned()),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported text encoding (expected UTF-8, UTF-16 or Windows-1252)",
        )),
    }
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

    #[test]
    fn binary_data_is_rejected() {
        assert!(decode(&[0, 1, 2, 0xFF]).is_err());
    }
}

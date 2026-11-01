//! Text decoding and encoding for opened Markdown files.
//!
//! Markdown is usually UTF-8, but real files also come with a UTF-8 BOM,
//! UTF-16 (Windows editors) or legacy 8-bit encodings. Reading everything with
//! `read_to_string` rejects those files. This module detects the common cases,
//! returns a `String` with the BOM removed, and remembers the encoding so a
//! save can write the file back the same way.

use std::io;
use std::path::Path;

/// The encoding a file was read with. Saving uses the same encoding when the
/// text can be represented in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextEncoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl TextEncoding {
    /// Human-readable name for status messages.
    pub fn label(self) -> &'static str {
        match self {
            TextEncoding::Utf8 | TextEncoding::Utf8Bom => "UTF-8",
            TextEncoding::Utf16Le => "UTF-16 LE",
            TextEncoding::Utf16Be => "UTF-16 BE",
            TextEncoding::Windows1252 => "Windows-1252",
        }
    }
}

/// Reads a Markdown/text file, detecting UTF-8, UTF-16 (BOM) and legacy
/// 8-bit encodings.
pub fn read_text(path: &Path) -> io::Result<(String, TextEncoding)> {
    let bytes = std::fs::read(path)?;
    decode(&bytes).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// Encodes `text` for writing. Returns `None` when `encoding` cannot represent
/// every character (only Windows-1252 can fail this way).
pub fn encode(text: &str, encoding: TextEncoding) -> Option<Vec<u8>> {
    match encoding {
        TextEncoding::Utf8 => Some(text.as_bytes().to_vec()),
        TextEncoding::Utf8Bom => {
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(text.as_bytes());
            Some(bytes)
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let little = encoding == TextEncoding::Utf16Le;
            let mut bytes = if little {
                vec![0xFF, 0xFE]
            } else {
                vec![0xFE, 0xFF]
            };
            for unit in text.encode_utf16() {
                let pair = if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                };
                bytes.extend_from_slice(&pair);
            }
            Some(bytes)
        }
        TextEncoding::Windows1252 => {
            let (bytes, _, had_unmappable) = encoding_rs::WINDOWS_1252.encode(text);
            (!had_unmappable).then(|| bytes.into_owned())
        }
    }
}

/// Decodes file bytes into text and the encoding they were read with.
fn decode(bytes: &[u8]) -> io::Result<(String, TextEncoding)> {
    // BOM-marked encodings first.
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let text = String::from_utf8(rest.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 after BOM"))?;
        return Ok((text, TextEncoding::Utf8Bom));
    }
    let utf16 = [
        (
            &[0xFF, 0xFE][..],
            encoding_rs::UTF_16LE,
            TextEncoding::Utf16Le,
        ),
        (
            &[0xFE, 0xFF][..],
            encoding_rs::UTF_16BE,
            TextEncoding::Utf16Be,
        ),
    ];
    if let Some((bom, encoding, kind)) = utf16.into_iter().find(|(bom, ..)| bytes.starts_with(bom))
    {
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[bom.len()..]);
        if had_errors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid {} text", encoding.name()),
            ));
        }
        return Ok((text.into_owned(), kind));
    }

    // Plain UTF-8.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok((text.to_string(), TextEncoding::Utf8));
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
    Ok((text.into_owned(), TextEncoding::Windows1252))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(bytes: &[u8]) -> String {
        decode(bytes).unwrap().0
    }

    #[test]
    fn plain_utf8_is_unchanged() {
        assert_eq!(text_of("café ✅".as_bytes()), "café ✅");
    }

    #[test]
    fn utf8_bom_is_stripped_and_remembered() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"---\ntitle: Test\n---\n# Hi\n");
        let (text, encoding) = decode(&bytes).unwrap();
        assert!(text.starts_with("---\ntitle: Test"));
        assert_eq!(encoding, TextEncoding::Utf8Bom);
    }

    #[test]
    fn utf16_le_and_be_are_decoded() {
        let text = "# Café\n";
        let mut le = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            decode(&le).unwrap(),
            (text.to_string(), TextEncoding::Utf16Le)
        );

        let mut be = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            be.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(
            decode(&be).unwrap(),
            (text.to_string(), TextEncoding::Utf16Be)
        );
    }

    #[test]
    fn windows_1252_is_a_fallback() {
        // "café" in Windows-1252 (0xE9 instead of UTF-8's 0xC3 0xA9).
        assert_eq!(
            decode(b"caf\xE9").unwrap(),
            ("café".to_string(), TextEncoding::Windows1252)
        );
    }

    /// Windows-1252's undefined slots decode to C1 controls instead of failing.
    #[test]
    fn windows_1252_undefined_slots_do_not_fail() {
        assert_eq!(text_of(b"a\x81b"), "a\u{81}b");
    }

    #[test]
    fn binary_data_is_rejected() {
        assert!(decode(&[0, 1, 2, 0xFF]).is_err());
    }

    #[test]
    fn saving_keeps_the_original_encoding() {
        let text = "# Café\n";
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf8Bom,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Windows1252,
        ] {
            let bytes = encode(text, encoding).unwrap();
            assert_eq!(decode(&bytes).unwrap(), (text.to_string(), encoding));
        }
    }

    #[test]
    fn windows_1252_refuses_characters_it_cannot_represent() {
        assert_eq!(encode("中文", TextEncoding::Windows1252), None);
        assert!(encode("中文", TextEncoding::Utf8).is_some());
    }
}

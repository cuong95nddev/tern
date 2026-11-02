//! Columns: the editor counts characters; a server counts UTF-16 units
//! (the LSP default), UTF-8 bytes or characters, as `initialize` agreed.

/// How a server counts columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

impl Encoding {
    /// The `positionEncoding` a server chose (UTF-16 when it said none).
    pub fn from_lsp(kind: Option<&lsp_types::PositionEncodingKind>) -> Self {
        match kind.map(|k| k.as_str()) {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }

    fn units(self, c: char) -> u32 {
        match self {
            Self::Utf8 => c.len_utf8() as u32,
            Self::Utf16 => c.len_utf16() as u32,
            Self::Utf32 => 1,
        }
    }
}

/// Character column `col` of `line` in the server's units.
pub fn to_server(line: &str, col: u32, enc: Encoding) -> u32 {
    line.chars().take(col as usize).map(|c| enc.units(c)).sum()
}

/// The server's column `col` of `line` in characters (clamped to the line;
/// a column inside a character counts as that character).
pub fn from_server(line: &str, col: u32, enc: Encoding) -> u32 {
    let mut units = 0;
    for (i, c) in line.chars().enumerate() {
        if units >= col {
            return i as u32;
        }
        units += enc.units(c);
    }
    line.chars().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trip_with_astral_chars() {
        let line = "a😀é b";
        // Char column 4 is ' ' (a, 😀, é, ' '), after 1 + 2 + 1 UTF-16 units.
        assert_eq!(to_server(line, 3, Encoding::Utf16), 4);
        assert_eq!(to_server(line, 4, Encoding::Utf16), 5);
        assert_eq!(from_server(line, 5, Encoding::Utf16), 4);
        assert_eq!(to_server(line, 2, Encoding::Utf8), 5);
        assert_eq!(to_server(line, 2, Encoding::Utf32), 2);
        for c in 0..=5 {
            for enc in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
                assert_eq!(from_server(line, to_server(line, c, enc), enc), c, "{c} {enc:?}");
            }
        }
        // Past the end clamps to the line's length.
        assert_eq!(from_server(line, 99, Encoding::Utf16), 5);
    }
}

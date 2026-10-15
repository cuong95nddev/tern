//! Color resolution: alacritty `Color` → concrete RGB using the theme palette
//! plus any runtime overrides the program set via OSC 4/10/11.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

#[derive(Debug, Clone)]
pub struct Palette {
    /// 0..16 ANSI, 16..256 cube + grayscale.
    pub indexed: [Rgb; 256],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub dim_foreground: Rgb,
}

const fn rgb(hex: u32) -> Rgb {
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

impl Default for Palette {
    /// A dark theme tuned for Claude Code's palette.
    fn default() -> Self {
        let ansi: [u32; 16] = [
            0x1d1f21, 0xe0605f, 0x8cc265, 0xd9b45a, 0x5fa7e8, 0xc678dd, 0x56b6c2, 0xc5c8c6, //
            0x5c6370, 0xff7b72, 0xa5d6a7, 0xf2cc60, 0x79c0ff, 0xd2a8ff, 0x76e3ea, 0xffffff,
        ];
        let mut indexed = [rgb(0); 256];
        for (i, c) in ansi.iter().enumerate() {
            indexed[i] = rgb(*c);
        }
        // 6x6x6 cube.
        let steps = [0u8, 95, 135, 175, 215, 255];
        for r in 0..6 {
            for g in 0..6 {
                for b in 0..6 {
                    indexed[16 + r * 36 + g * 6 + b] = Rgb { r: steps[r], g: steps[g], b: steps[b] };
                }
            }
        }
        for i in 0..24 {
            let v = 8 + i as u8 * 10;
            indexed[232 + i] = Rgb { r: v, g: v, b: v };
        }
        Self {
            indexed,
            foreground: rgb(0xd8dade),
            background: rgb(0x15161a),
            cursor: rgb(0xe8e8e8),
            selection: rgb(0x3a4a66),
            dim_foreground: rgb(0x8a8f98),
        }
    }
}

impl Palette {
    pub fn named(&self, overrides: &Colors, name: NamedColor) -> Rgb {
        if let Some(c) = overrides[name] {
            return c;
        }
        match name {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => self.dim_foreground,
            n => {
                let idx = n as usize;
                if idx < 16 {
                    self.indexed[idx]
                } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&idx) {
                    dim(self.indexed[idx - NamedColor::DimBlack as usize])
                } else {
                    self.foreground
                }
            },
        }
    }

    pub fn indexed(&self, overrides: &Colors, idx: usize) -> Rgb {
        overrides[idx].unwrap_or_else(|| if idx < 256 { self.indexed[idx] } else { self.named_by_index(overrides, idx) })
    }

    fn named_by_index(&self, overrides: &Colors, idx: usize) -> Rgb {
        match idx {
            i if i == NamedColor::Foreground as usize => self.named(overrides, NamedColor::Foreground),
            i if i == NamedColor::Background as usize => self.named(overrides, NamedColor::Background),
            i if i == NamedColor::Cursor as usize => self.named(overrides, NamedColor::Cursor),
            _ => self.foreground,
        }
    }

    pub fn resolve(&self, overrides: &Colors, color: Color) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Named(n) => self.named(overrides, n),
            Color::Indexed(i) => self.indexed(overrides, i as usize),
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb { r: (c.r as u16 * 2 / 3) as u8, g: (c.g as u16 * 2 / 3) as u8, b: (c.b as u16 * 2 / 3) as u8 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_cube_and_named() {
        let p = Palette::default();
        let o = Colors::default();
        assert_eq!(p.resolve(&o, Color::Indexed(16)), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.resolve(&o, Color::Indexed(231)), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(p.resolve(&o, Color::Named(NamedColor::Background)), p.background);
    }

    #[test]
    fn overrides_win() {
        let p = Palette::default();
        let mut o = Colors::default();
        o[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(p.resolve(&o, Color::Named(NamedColor::Background)), Rgb { r: 1, g: 2, b: 3 });
    }
}

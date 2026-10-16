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

/// The 16 ANSI colors of the default (dark) palette.
pub const DARK_ANSI: [Rgb; 16] = [
    rgb(0x1d1f21),
    rgb(0xe0605f),
    rgb(0x8cc265),
    rgb(0xd9b45a),
    rgb(0x5fa7e8),
    rgb(0xc678dd),
    rgb(0x56b6c2),
    rgb(0xc5c8c6),
    rgb(0x5c6370),
    rgb(0xff7b72),
    rgb(0xa5d6a7),
    rgb(0xf2cc60),
    rgb(0x79c0ff),
    rgb(0xd2a8ff),
    rgb(0x76e3ea),
    rgb(0xffffff),
];

impl Default for Palette {
    /// A dark theme tuned for Claude Code's palette.
    fn default() -> Self {
        Self::new(
            DARK_ANSI,
            rgb(0xd8dade),
            // The app's content background (the theme's `background`), so a
            // terminal tab and a file tab sit on the same black.
            rgb(0x0a0a0a),
            rgb(0xe8e8e8),
            rgb(0x3a4a66),
            rgb(0x8a8f98),
        )
    }
}

impl Palette {
    /// A palette from its 16 ANSI colors and the special ones; the 256-color
    /// cube and grayscale ramp are the standard xterm ones.
    pub fn new(ansi: [Rgb; 16], foreground: Rgb, background: Rgb, cursor: Rgb, selection: Rgb, dim_foreground: Rgb) -> Self {
        let mut indexed = [rgb(0); 256];
        indexed[..16].copy_from_slice(&ansi);
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
        Self { indexed, foreground, background, cursor, selection, dim_foreground }
    }

    /// The 16 ANSI colors.
    pub fn ansi(&self) -> [Rgb; 16] {
        let mut a = [rgb(0); 16];
        a.copy_from_slice(&self.indexed[..16]);
        a
    }

    /// One `name #rrggbb` line per color: what [`Palette::from_text`] reads.
    /// The app writes it for the daemon, so programs asking for colors
    /// (OSC 4/10/11) get the theme's.
    pub fn to_text(&self) -> String {
        let hex = |c: Rgb| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
        let mut out = String::new();
        for (name, c) in [
            ("foreground", self.foreground),
            ("background", self.background),
            ("cursor", self.cursor),
            ("selection", self.selection),
            ("dim_foreground", self.dim_foreground),
        ] {
            out += &format!("{name} {}\n", hex(c));
        }
        for (i, c) in self.ansi().into_iter().enumerate() {
            out += &format!("{i} {}\n", hex(c));
        }
        out
    }

    /// Reads [`Palette::to_text`]'s format; a color it lacks or can't read
    /// keeps the default's.
    pub fn from_text(text: &str) -> Self {
        let d = Self::default();
        let (mut ansi, mut fg, mut bg, mut cursor, mut sel, mut dim) =
            (d.ansi(), d.foreground, d.background, d.cursor, d.selection, d.dim_foreground);
        for line in text.lines() {
            let Some((name, value)) = line.split_once(' ') else { continue };
            let Some(c) = parse_hex(value.trim()) else { continue };
            match name {
                "foreground" => fg = c,
                "background" => bg = c,
                "cursor" => cursor = c,
                "selection" => sel = c,
                "dim_foreground" => dim = c,
                n => {
                    if let Some(slot) = n.parse::<usize>().ok().and_then(|i| ansi.get_mut(i)) {
                        *slot = c;
                    }
                },
            }
        }
        Self::new(ansi, fg, bg, cursor, sel, dim)
    }
}

/// `#rrggbb` (or `rrggbb`) to a color.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok().map(rgb)
}

/// A palette kept in a file by another process (the app, for the daemon):
/// read again whenever the file changes, the default while there is none.
pub struct PaletteFile {
    path: std::path::PathBuf,
    cached: std::sync::Mutex<(Option<std::time::SystemTime>, std::sync::Arc<Palette>)>,
}

impl PaletteFile {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path, cached: std::sync::Mutex::new((None, std::sync::Arc::new(Palette::default()))) }
    }

    pub fn get(&self) -> std::sync::Arc<Palette> {
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        let mut cached = self.cached.lock().unwrap();
        if mtime != cached.0 {
            let palette = std::fs::read_to_string(&self.path).map(|t| Palette::from_text(&t)).unwrap_or_default();
            *cached = (mtime, std::sync::Arc::new(palette));
        }
        cached.1.clone()
    }
}

impl Palette {
    /// Whether the background is dark (relative luminance below one half).
    pub fn is_dark(&self) -> bool {
        let lin = |c: u8| {
            let c = c as f32 / 255.;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let b = self.background;
        0.2126 * lin(b.r) + 0.7152 * lin(b.g) + 0.0722 * lin(b.b) < 0.5
    }

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
    fn text_round_trips() {
        let mut ansi = DARK_ANSI;
        ansi[3] = rgb(0x123456);
        let p = Palette::new(ansi, rgb(0x010203), rgb(0xfafafa), rgb(0x111111), rgb(0x222222), rgb(0x333333));
        let q = Palette::from_text(&p.to_text());
        assert_eq!(q.ansi(), p.ansi());
        assert_eq!(
            (q.foreground, q.background, q.cursor, q.selection, q.dim_foreground),
            (p.foreground, p.background, p.cursor, p.selection, p.dim_foreground)
        );
        assert_eq!(q.indexed[200], p.indexed[200]);
    }

    #[test]
    fn bad_text_keeps_defaults() {
        let q = Palette::from_text("background nothex\n99 #ffffff\nforeground #ff0000\n");
        let d = Palette::default();
        assert_eq!(q.background, d.background);
        assert_eq!(q.foreground, rgb(0xff0000));
        assert_eq!(q.ansi(), d.ansi());
    }

    #[test]
    fn overrides_win() {
        let p = Palette::default();
        let mut o = Colors::default();
        o[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(p.resolve(&o, Color::Named(NamedColor::Background)), Rgb { r: 1, g: 2, b: 3 });
    }
}

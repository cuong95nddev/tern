//! Unicode placeholders: a program shows an image by printing U+10EEEE cells
//! whose foreground colour names the image, whose underline colour names the
//! placement, and whose combining diacritics give the row, the column and the
//! image id's high byte of the piece drawn there.

use alacritty_terminal::vte::ansi::Color;

/// The placeholder character.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics from the protocol's table: the n-th one means n.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B,
    0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486,
    0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9, 0x05AB,
    0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615, 0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D,
    0x065E, 0x06D6, 0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC,
    0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC,
    0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821,
    0x0822, 0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87,
    0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E,
    0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5, 0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC,
    0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7,
    0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8,
    0x2DF9, 0x2DFA, 0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4,
    0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7,
    0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187,
    0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The number a row/column diacritic stands for.
pub fn diacritic_value(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32)
}

/// One decoded placeholder cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderCell {
    pub image_id: u32,
    /// 0 when the cell names no placement.
    pub placement_id: u32,
    pub row: u32,
    pub col: u32,
}

/// An id carried by a colour: 24-bit colours give `r<<16|g<<8|b`, 256-colour
/// ones their index. The default colours carry none.
fn color_id(c: Color) -> Option<u32> {
    match c {
        Color::Spec(rgb) => Some(((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32),
        Color::Indexed(i) => Some(i as u32),
        Color::Named(_) => None,
    }
}

impl PlaceholderCell {
    /// Decodes a placeholder cell from its colours and combining marks. `prev`
    /// is the decoded cell just to its left on the same row, if that was a
    /// placeholder: a cell that leaves out its column (or row, or high byte)
    /// continues it.
    pub fn decode(fg: Color, underline: Option<Color>, marks: &[char], prev: Option<&PlaceholderCell>) -> Option<Self> {
        let low = color_id(fg)?;
        let placement_id = underline.and_then(color_id).unwrap_or(0);
        let mut values = marks.iter().filter_map(|&c| diacritic_value(c));
        let (row, col, high) = (values.next(), values.next(), values.next());
        let prev = prev.filter(|p| p.image_id & 0x00FF_FFFF == low && p.placement_id == placement_id);
        let row = row.or(prev.map(|p| p.row)).unwrap_or(0);
        let prev = prev.filter(|p| p.row == row);
        let col = col.or(prev.map(|p| p.col + 1)).unwrap_or(0);
        let high = high.or(prev.filter(|p| p.col + 1 == col).map(|p| p.image_id >> 24)).unwrap_or(0);
        let image_id = (high << 24) | low;
        (image_id != 0).then_some(Self { image_id, placement_id, row, col })
    }
}

/// The placement box in cells. A size of 0 is derived: from the other side
/// and the image's aspect ratio, or from the image's own pixel size.
pub fn box_cells(cols: u32, rows: u32, img_w: u32, img_h: u32, cell_w: f32, cell_h: f32) -> (u32, u32) {
    let (w, h) = (img_w.max(1) as f32, img_h.max(1) as f32);
    let up = |v: f32| (v.ceil() as u32).max(1);
    match (cols, rows) {
        (0, 0) => (up(w / cell_w), up(h / cell_h)),
        (c, 0) => (c, up(c as f32 * cell_w * h / w / cell_h)),
        (0, r) => (up(r as f32 * cell_h * w / h / cell_w), r),
        (c, r) => (c, r),
    }
}

/// Where the image goes inside a `cols`×`rows` box: scaled to fit with its
/// aspect ratio kept, centred. `(x, y, width, height)` from the box's corner.
pub fn fit(cols: u32, rows: u32, cell_w: f32, cell_h: f32, img_w: u32, img_h: u32) -> (f32, f32, f32, f32) {
    let (bw, bh) = (cols as f32 * cell_w, rows as f32 * cell_h);
    let (iw, ih) = (img_w.max(1) as f32, img_h.max(1) as f32);
    let scale = (bw / iw).min(bh / ih);
    let (w, h) = (iw * scale, ih * scale);
    ((bw - w) / 2., (bh - h) / 2., w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

    const R0: char = '\u{0305}';
    const R1: char = '\u{030D}';
    const R2: char = '\u{030E}';

    #[test]
    fn diacritic_table_is_sorted_and_complete() {
        assert_eq!(DIACRITICS.len(), 297);
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(diacritic_value(R0), Some(0));
        assert_eq!(diacritic_value(R2), Some(2));
        assert_eq!(diacritic_value('\u{1D244}'), Some(296));
        assert_eq!(diacritic_value('a'), None);
    }

    #[test]
    fn decodes_id_row_and_column() {
        let fg = Color::Spec(Rgb { r: 0, g: 1, b: 2 });
        let ul = Some(Color::Indexed(9));
        let c = PlaceholderCell::decode(fg, ul, &[R1, R2], None).unwrap();
        assert_eq!(c, PlaceholderCell { image_id: 0x0102, placement_id: 9, row: 1, col: 2 });
    }

    #[test]
    fn third_diacritic_is_the_high_byte() {
        let c = PlaceholderCell::decode(Color::Indexed(5), None, &[R0, R0, R1], None).unwrap();
        assert_eq!(c.image_id, (1 << 24) | 5);
    }

    #[test]
    fn inherits_from_the_left() {
        let fg = Color::Indexed(7);
        let first = PlaceholderCell::decode(fg, None, &[R1, R2, R1], None).unwrap();
        let bare = PlaceholderCell::decode(fg, None, &[], Some(&first)).unwrap();
        assert_eq!(bare, PlaceholderCell { image_id: (1 << 24) | 7, placement_id: 0, row: 1, col: 3 });
        let row_only = PlaceholderCell::decode(fg, None, &[R1], Some(&bare)).unwrap();
        assert_eq!((row_only.row, row_only.col), (1, 4));
        // Another image to the left: nothing is inherited.
        let other = PlaceholderCell::decode(Color::Indexed(8), None, &[], Some(&first)).unwrap();
        assert_eq!((other.image_id, other.row, other.col), (8, 0, 0));
    }

    #[test]
    fn default_colour_or_zero_id_is_not_an_image() {
        assert_eq!(PlaceholderCell::decode(Color::Named(NamedColor::Foreground), None, &[R0, R0], None), None);
        assert_eq!(PlaceholderCell::decode(Color::Indexed(0), None, &[R0, R0], None), None);
    }

    #[test]
    fn box_size_from_the_placement_or_the_image() {
        assert_eq!(box_cells(10, 4, 100, 50, 8., 16.), (10, 4));
        // Only columns: rows follow the aspect ratio (80 px wide -> 40 px tall -> 3 rows).
        assert_eq!(box_cells(10, 0, 100, 50, 8., 16.), (10, 3));
        // Only rows: 4 rows = 64 px tall -> 128 px wide -> 16 columns.
        assert_eq!(box_cells(0, 4, 100, 50, 8., 16.), (16, 4));
        // Neither: the image's own size, rounded up.
        assert_eq!(box_cells(0, 0, 100, 50, 8., 16.), (13, 4));
    }

    #[test]
    fn fit_keeps_aspect_and_centres() {
        // Box 80x64, image 100x50 -> scale 0.8 -> 80x40, centred vertically.
        assert_eq!(fit(10, 4, 8., 16., 100, 50), (0., 12., 80., 40.));
    }
}

//! The hu_lib.c port: the text-line widget and the scrolling-message
//! widget. The input-text widget (`hu_itext_t`, the net-game chat line)
//! is deliberately not here yet — it belongs to the chat layer and
//! needs the input handling that lands with the platform module.
//!
//! Drawing mechanics, ported 1:1:
//!
//! * characters are upper-cased before lookup (`toupper`);
//! * printable range is `'!'..='_'`; anything else (and the space)
//!   advances the pen by 4 pixels without drawing;
//! * a glyph that would poke past `SCREENWIDTH` stops the line — the
//!   glyph is not drawn at all (`HUlib_drawTextLine`);
//! * patches draw at origin semantics via [`crate::stbar::draw_patch`].
//!
//! The original erases text lines by copying the rows above/below the
//! status bar back from the backscreen (`R_VideoErase`). This engine
//! fully repaints the view every frame, so the HUD simply draws after
//! the view and needs no separate erase pass.

use crate::render::{IndexedFramebuffer, SCREEN_WIDTH};
use crate::stbar::draw_patch;

use super::{HU_FONTEND, HU_FONTSTART, HudFont};

/// Maximum line length in characters (`HU_MAXLINELENGTH`).
pub const HU_MAXLINELENGTH: usize = 80;

/// Maximum scrolling-text height in lines (`HU_MAXLINES`).
pub const HU_MAXLINES: usize = 4;

/// The pen advance for the space character and for out-of-range bytes
/// (`HUlib_drawTextLine`: `x += 4`).
const SPACE_ADVANCE: i32 = 4;

fn lookup_glyph(c: u8) -> Option<usize> {
    let c = c.to_ascii_uppercase();
    if c == b' ' || !(HU_FONTSTART..=HU_FONTEND).contains(&c) {
        None
    } else {
        Some((c - HU_FONTSTART) as usize)
    }
}

/// Width of `text` in pixels when drawn with `font` — same walk the
/// drawer does, so the number always matches what lands on screen.
pub fn text_width(font: &HudFont, text: &[u8]) -> i32 {
    let mut width = 0;
    for &raw in text {
        match lookup_glyph(raw) {
            Some(index) => width += i32::from(font.glyph(index).header.width),
            None => width += SPACE_ADVANCE,
        }
    }
    width
}

/// Draws `text` with `font` at pen position `(x, y)`, upper-casing and
/// clipping like `HUlib_drawTextLine`. Out-of-range bytes and spaces
/// advance the pen by 4 px.
pub fn draw_text(font: &HudFont, fb: &mut IndexedFramebuffer, text: &[u8], x: i32, y: i32) {
    let mut cx = x;
    for &raw in text {
        match lookup_glyph(raw) {
            Some(index) => {
                let glyph = font.glyph(index);
                let w = i32::from(glyph.header.width);
                if cx + w > SCREEN_WIDTH as i32 {
                    break;
                }
                draw_patch(fb, glyph, cx, y);
                cx += w;
            }
            None => {
                cx += SPACE_ADVANCE;
                if cx >= SCREEN_WIDTH as i32 {
                    break;
                }
            }
        }
    }
}

/// The text line widget (`hu_textline_t`). Stores raw bytes; upper-casing
/// happens at draw time like the original.
pub struct TextLine {
    /// Left edge of the line in screen pixels.
    pub x: i32,
    /// Baseline row of the glyphs.
    pub y: i32,
    line: [u8; HU_MAXLINELENGTH + 1],
    len: usize,
}

impl TextLine {
    /// `HUlib_initTextLine` (clears the line). The font lookup base is
    /// always `HU_FONTSTART` — this port serves the one HUD font.
    pub fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            line: [0; HU_MAXLINELENGTH + 1],
            len: 0,
        }
    }

    /// `HUlib_clearTextLine`.
    pub fn clear(&mut self) {
        self.len = 0;
        self.line[0] = 0;
    }

    /// `HUlib_addCharToTextLine`; returns false when the line is full.
    pub fn add_char(&mut self, ch: u8) -> bool {
        if self.len == HU_MAXLINELENGTH {
            return false;
        }
        self.line[self.len] = ch;
        self.len += 1;
        self.line[self.len] = 0;
        true
    }

    /// `HUlib_delCharFromTextLine`; returns false on an empty line.
    pub fn del_char(&mut self) -> bool {
        if self.len == 0 {
            return false;
        }
        self.len -= 1;
        self.line[self.len] = 0;
        true
    }

    /// Number of bytes in the line.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the line is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The raw bytes on the line (no trailing NUL).
    pub fn text(&self) -> &[u8] {
        &self.line[..self.len]
    }

    /// Pixel width of the current contents.
    pub fn width(&self, font: &HudFont) -> i32 {
        text_width(font, self.text())
    }

    /// `HUlib_drawTextLine` (no cursor — the input widget that used it
    /// is not ported yet).
    pub fn draw(&self, fb: &mut IndexedFramebuffer, font: &HudFont) {
        draw_text(font, fb, self.text(), self.x, self.y);
    }
}

/// The scrolling text widget (`hu_stext_t`): a ring of [`TextLine`]s.
/// The message widget is one such ring with `h = HU_MSGHEIGHT` (1).
pub struct ScrollText {
    lines: Vec<TextLine>,
    cl: usize,
}

impl ScrollText {
    /// `HUlib_initSText`. Line `i` sits at `y - i * line_step` (the
    /// original uses `SHORT(font[0]->height) + 1`).
    pub fn new(x: i32, y: i32, height: usize, line_step: i32) -> Self {
        assert!(height > 0, "scrolling text must have at least one line");
        let lines = (0..height)
            .map(|i| TextLine::new(x, y - i as i32 * line_step))
            .collect();
        Self { lines, cl: 0 }
    }

    /// Number of lines in the ring.
    pub fn height(&self) -> usize {
        self.lines.len()
    }

    /// Index of the current (newest) line.
    pub fn current_line(&self) -> usize {
        self.cl
    }

    /// Borrow a line by ring position.
    pub fn line(&self, index: usize) -> &TextLine {
        &self.lines[index]
    }

    /// `HUlib_addLineToSText`: advance the ring and clear the slot.
    pub fn add_line(&mut self) {
        self.cl = (self.cl + 1) % self.lines.len();
        self.lines[self.cl].clear();
    }

    /// `HUlib_addMessageToSText`: start a fresh line, then write
    /// `prefix` (no separator is added, like the original) and `msg`.
    pub fn add_message(&mut self, prefix: Option<&[u8]>, msg: &[u8]) {
        self.add_line();
        if let Some(prefix) = prefix {
            for &b in prefix {
                self.lines[self.cl].add_char(b);
            }
        }
        for &b in msg {
            self.lines[self.cl].add_char(b);
        }
    }

    /// `HUlib_drawSText`: newest line first, working backwards through
    /// the ring. The caller is responsible for not drawing when the
    /// widget is off (the original gates on `*s->on`).
    pub fn draw(&self, fb: &mut IndexedFramebuffer, font: &HudFont) {
        let mut idx = self.cl;
        for _ in 0..self.lines.len() {
            self.lines[idx].draw(fb, font);
            idx = if idx == 0 {
                self.lines.len() - 1
            } else {
                idx - 1
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::patch::{Patch, decode_patch};
    use std::sync::Arc;

    /// Builds raw patch bytes from per-column `(topdelta, pixels)` posts.
    fn build_patch_bytes(width: i16, height: i16, columns: &[Vec<(u8, Vec<u8>)>]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes()); // left offset
        data.extend_from_slice(&0i16.to_le_bytes()); // top offset
        let table_start = data.len();
        data.resize(table_start + columns.len() * 4, 0);
        for (i, posts) in columns.iter().enumerate() {
            let column_offset = data.len() as i32;
            data[table_start + i * 4..table_start + (i + 1) * 4]
                .copy_from_slice(&column_offset.to_le_bytes());
            for &(topdelta, ref pixels) in posts {
                data.push(topdelta);
                data.push(pixels.len() as u8);
                data.push(0);
                data.extend_from_slice(pixels);
                data.push(0);
            }
            data.push(0xff);
        }
        data
    }

    /// A `width`x`height` patch filled with `fill`.
    fn fill_patch(width: i16, height: i16, fill: u8) -> Patch {
        let column = vec![(0u8, vec![fill; height as usize])];
        let columns = vec![column; width as usize];
        decode_patch(&build_patch_bytes(width, height, &columns)).unwrap()
    }

    /// Font where glyph `!`+i is `glyph_width(i)` pixels wide, all pixels
    /// colored `color(i)`.
    fn synthetic_font<const N: usize>(widths: [i16; N], colors: [u8; N]) -> HudFont {
        let glyphs: [Arc<Patch>; super::super::HU_FONTSIZE] = std::array::from_fn(|i| {
            let wi = i % N;
            Arc::new(fill_patch(widths[wi], 7, colors[wi]))
        });
        HudFont::from_glyphs(glyphs)
    }

    #[test]
    fn add_char_stops_at_max_line_length() {
        let mut line = TextLine::new(0, 0);
        for i in 0..HU_MAXLINELENGTH {
            assert!(line.add_char(b'A'), "char {i} refused");
        }
        assert!(!line.add_char(b'A'));
        assert_eq!(line.len(), HU_MAXLINELENGTH);
    }

    #[test]
    fn del_char_on_empty_line_fails() {
        let mut line = TextLine::new(0, 0);
        assert!(!line.del_char());
        line.add_char(b'A');
        assert!(line.del_char());
        assert!(!line.del_char());
    }

    #[test]
    fn text_width_sums_glyphs_and_spaces() {
        // glyphs are 3 px wide in this fixture (N=1)
        let font = synthetic_font([3], [7]);
        assert_eq!(text_width(&font, b"ABC"), 9);
        assert_eq!(text_width(&font, b"A B"), 3 + 4 + 3);
        // out-of-range bytes advance like a space
        assert_eq!(text_width(&font, b"A{B"), 3 + 4 + 3);
        assert_eq!(text_width(&font, b""), 0);
    }

    #[test]
    fn text_width_matches_drawn_extent() {
        let font = synthetic_font([2, 5], [1, 2]);
        let fb_width = |text: &[u8]| {
            let mut fb = IndexedFramebuffer::new();
            draw_text(&font, &mut fb, text, 0, 0);
            // rightmost non-zero column
            (0..SCREEN_WIDTH as i32)
                .rev()
                .find(|&x| (0..7).any(|y| fb.get(x as usize, y) != Some(0)))
                .map(|x| x + 1)
                .unwrap_or(0)
        };
        // widths alternate 2,5,2,5...; 'A' -> glyph 0 (2px), 'B' -> glyph 1
        assert_eq!(fb_width(b"AB"), 2 + 5);
        assert_eq!(fb_width(b"ABA"), 2 + 5 + 2);
    }

    #[test]
    fn draw_stops_at_screen_right_edge() {
        let font = synthetic_font([4], [9]);
        let mut fb = IndexedFramebuffer::new();
        // 80 glyphs of 4 px = 320 px; a 81st would start at x=320 (off-screen)
        let text = vec![b'A'; 100];
        draw_text(&font, &mut fb, &text, 0, 0);
        assert_eq!(fb.get(319, 0), Some(9));
        // 79 glyphs fit exactly, so 79*4 = 316; the 80th starts at 316 and
        // fits; the 81st starts at 320 and is rejected
        let text79 = vec![b'A'; 79];
        let mut fb79 = IndexedFramebuffer::new();
        draw_text(&font, &mut fb79, &text79, 0, 0);
        assert_eq!(fb79.get(315, 0), Some(9));
        assert_eq!(fb79.get(316, 0), Some(0));
    }

    #[test]
    fn glyph_that_would_overflow_is_not_drawn() {
        let font = synthetic_font([4], [9]);
        let mut fb = IndexedFramebuffer::new();
        // start 2 px from the right edge: glyph needs 4 px, must be dropped
        draw_text(&font, &mut fb, b"A", SCREEN_WIDTH as i32 - 2, 0);
        assert_eq!(fb.get(318, 0), Some(0));
        assert_eq!(fb.get(319, 0), Some(0));
        // a space instead advances the pen and stays invisible
        draw_text(&font, &mut fb, b" A", SCREEN_WIDTH as i32 - 4, 0);
        assert_eq!(fb.get(316, 0), Some(0));
    }

    #[test]
    fn scroll_text_ring_wraps_and_clears() {
        let mut st = ScrollText::new(0, 0, 2, 8);
        st.add_message(None, b"one");
        st.add_message(None, b"two");
        st.add_message(None, b"three");
        // h=2 ring: after three messages the newest lives in slot 1
        assert_eq!(st.current_line(), 1);
        assert_eq!(st.line(1).text(), b"three");
        assert_eq!(st.line(0).text(), b"two");
    }

    #[test]
    fn prefix_is_written_without_separator() {
        let mut st = ScrollText::new(0, 0, 1, 8);
        st.add_message(Some(b"Green: "), b"hello");
        assert_eq!(st.line(0).text(), b"Green: hello");
    }
}

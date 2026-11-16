//! The DOOM heads-up display: the message widget and the level title,
//! ported from `hu_stuff.c` / `hu_lib.c` (linuxdoom-1.10).
//!
//! What is (and is not) here:
//!
//! * the **message widget** — a one-line scrolling-text ring drawn at
//!   the top-left corner (`HU_MSGX`, `HU_MSGY`). A new player message
//!   replaces the old one and lives for `HU_MSGTIMEOUT` tics (140 = 4
//!   seconds at 35 Hz); when the counter expires the message is hidden
//!   until the next one arrives.
//! * the **level title** — the map name, drawn only while the automap
//!   is up (`HUlib_drawTextLine` from `HU_Drawer` gated on
//!   `automapactive`), at `HU_TITLEY = 167 - font height`.
//! * the **HUD font** — `STCFN033..STCFN095`, one patch per printable
//!   character from `'!'` (33) through `'_'` (95). Note the lump names
//!   use the decimal glyph code (`STCFN033`, not `STCFN!`).
//!
//! Deliberately not in this module: the net-game chat line (`hu_itext_t`)
//! and chat macros handling — those land with the input/netcode layer.
//! The vanilla erase mechanics (`HUlib_eraseTextLine` + `R_VideoErase`)
//! are not ported either: this engine fully repaints the view every
//! frame, so the HUD is simply drawn after the view and no rows need to
//! be scrubbed.
//!
//! Layout constants are byte-for-byte `#define` ports from
//! `hu_stuff.h`. All coordinates are framebuffer pixels.

mod messages;
mod widgets;

pub use messages::{
    CHAT_MACROS, HudMessage, MAPNAMES_DOOM2, MAPNAMES_EPISODE, MAPNAMES_PLUTONIA, MAPNAMES_TNT,
    MapNameSet, PLAYER_NAMES, marked_spot_message,
};
pub use widgets::{HU_MAXLINELENGTH, HU_MAXLINES, ScrollText, TextLine, text_width};

use crate::errors::{EngineError, EngineResult};
use crate::gfx::patch::{Patch, decode_patch};
use crate::render::IndexedFramebuffer;
use crate::resources::ResourceManager;
use std::sync::Arc;

/// First printable character in the HUD font (`HU_FONTSTART`).
pub const HU_FONTSTART: u8 = b'!';
/// Last printable character in the HUD font (`HU_FONTEND`).
pub const HU_FONTEND: u8 = b'_';
/// Number of glyphs in the HUD font (`HU_FONTSIZE`).
pub const HU_FONTSIZE: usize = (HU_FONTEND - HU_FONTSTART) as usize + 1; // 63

/// Maximum message width in characters (`HU_MSGWIDTH`).
pub const HU_MSGWIDTH: usize = 64;
/// Message widget x position (`HU_MSGX`).
pub const HU_MSGX: i32 = 0;
/// Message widget y position (`HU_MSGY`).
pub const HU_MSGY: i32 = 0;
/// Message widget height in lines (`HU_MSGHEIGHT`).
pub const HU_MSGHEIGHT: usize = 1;
/// How long a message stays up, in tics (`HU_MSGTIMEOUT` = 4*TICRATE).
pub const HU_MSGTIMEOUT: i32 = 4 * crate::stbar::TICRATE;
/// Level title x position (`HU_TITLEX`).
pub const HU_TITLEX: i32 = 0;
/// Row the level title sits on: `167 - SHORT(hu_font[0]->height)`
/// (`HU_TITLEY` — 167, the last row above the status bar, unlike
/// `ST_Y - 1` it does not move with the bar visibility).
pub const HU_TITLE_BASELINE: i32 = 167;

/// The decoded HUD font: `STCFN033..STCFN095`, indexed by
/// `glyph - HU_FONTSTART`.
pub struct HudFont {
    glyphs: [Arc<Patch>; HU_FONTSIZE],
}

impl HudFont {
    fn load(resources: &ResourceManager) -> EngineResult<Self> {
        let mut glyphs = Vec::with_capacity(HU_FONTSIZE);
        for i in 0..HU_FONTSIZE {
            // vanilla: sprintf(buffer, "STCFN%.3d", HU_FONTSTART + i)
            let name = format!("STCFN{:03}", HU_FONTSTART as usize + i);
            let index = resources
                .find_lump(&name)
                .ok_or_else(|| EngineError::Resource(format!("HUD font lump {name} not found")))?;
            let patch = resources.decode_cached::<Patch>(index, |bytes| {
                decode_patch(bytes).map_err(EngineError::from)
            })?;
            glyphs.push(patch);
        }
        let glyphs: [Arc<Patch>; HU_FONTSIZE] = glyphs
            .try_into()
            .map_err(|_| EngineError::Resource("HUD font glyph table size mismatch".to_string()))?;
        Ok(Self { glyphs })
    }

    /// Glyph `index` (`c - HU_FONTSTART`).
    pub fn glyph(&self, index: usize) -> &Patch {
        &self.glyphs[index]
    }

    /// Height of the glyphs in pixels (all STCFN glyphs share a height).
    pub fn glyph_height(&self) -> i32 {
        i32::from(self.glyphs[0].header.height)
    }

    /// Width of `text` in pixels, upper-casing like the renderer.
    pub fn text_width(&self, text: &[u8]) -> i32 {
        widgets::text_width(self, text)
    }

    #[cfg(test)]
    pub(crate) fn from_glyphs(glyphs: [Arc<Patch>; HU_FONTSIZE]) -> Self {
        Self { glyphs }
    }
}

/// Plain-data inputs for one frame of the HUD, filled in by the game
/// layer. Nothing engine-internal crosses this boundary.
pub struct HudState<'a> {
    /// A new player message for this tic (vanilla `plr->message`). The
    /// game layer must clear its side after handing it over; the HUD
    /// shows the message until [`HU_MSGTIMEOUT`] expires. Pass the
    /// already-prefixed text — nothing is prepended here.
    pub message: Option<&'a str>,
    /// The `showMessages` config flag. When false, ordinary messages
    /// are dropped; `force_message` still lets one through (vanilla
    /// `message_dontfuckwithme`, used by the messages on/off toggle).
    pub show_messages: bool,
    /// One-shot override to show `message` even when `show_messages`
    /// is false. A forced message also sets the priority lock
    /// (vanilla `message_nottobefuckedwith`): ordinary messages are
    /// suppressed until it times out. The game layer is expected to
    /// reset this after the message is consumed.
    pub force_message: bool,
    /// True while the automap is up — the level title is drawn only
    /// then, like the original.
    pub automap_active: bool,
    /// Resolved level title (see [`MapNameSet::title`]). Drawn at
    /// `(HU_TITLEX, HU_TITLE_BASELINE - font height)` when
    /// `automap_active`.
    pub title: &'a str,
}

/// The retained HUD state: the font, the message widget and the
/// message timeout counter. Build once with [`Hud::load`], then drive it
/// with [`Hud::tick`] / [`Hud::render`] (or [`render_hud`]).
pub struct Hud {
    font: HudFont,
    message: ScrollText,
    message_on: bool,
    message_counter: i32,
    /// Vanilla `message_nottobefuckedwith`: a priority message (a chat
    /// message, the messages on/off confirmation) is up. Ordinary
    /// messages are suppressed until it times out.
    priority_locked: bool,
}

impl Hud {
    /// Decodes the HUD font (`STCFN033..STCFN095`) through the resource
    /// manager. The message widget starts empty and hidden.
    pub fn load(resources: &ResourceManager) -> EngineResult<Self> {
        let font = HudFont::load(resources)?;
        Ok(Self::new(font))
    }

    fn new(font: HudFont) -> Self {
        let line_step = font.glyph_height() + 1;
        Self {
            message: ScrollText::new(HU_MSGX, HU_MSGY, HU_MSGHEIGHT, line_step),
            font,
            message_on: false,
            message_counter: 0,
            priority_locked: false,
        }
    }

    /// Whether the message widget is currently showing text.
    pub fn message_on(&self) -> bool {
        self.message_on
    }

    /// Ticks remaining on the current message (0 when hidden).
    pub fn message_counter(&self) -> i32 {
        self.message_counter
    }

    /// `HU_Ticker`: count the visible message down, then promote a new
    /// message into the widget. `show_messages == false` drops ordinary
    /// messages unless `force_message` is set.
    ///
    /// A forced message also sets the priority lock (vanilla
    /// `message_nottobefuckedwith`): ordinary messages arriving while it
    /// is displayed are dropped until it times out. The game layer can
    /// keep re-offering a suppressed message on later tics — exactly how
    /// vanilla keeps `plr->message` set until `HU_Ticker` consumes it.
    pub fn tick(&mut self, state: &HudState<'_>) {
        if self.message_counter > 0 {
            self.message_counter -= 1;
            if self.message_counter == 0 {
                self.message_on = false;
                self.priority_locked = false;
            }
        }
        if state.show_messages || state.force_message {
            if let Some(msg) = state.message {
                // a priority message bypasses the lock; an ordinary one
                // does not get through while a priority message is up
                if state.force_message || !self.priority_locked {
                    self.message.add_message(None, msg.as_bytes());
                    self.message_on = true;
                    self.message_counter = HU_MSGTIMEOUT;
                    if state.force_message {
                        self.priority_locked = true;
                    }
                }
            }
        }
    }

    /// `HU_Drawer`: draws the message (if up) and the level title (if
    /// the automap is active). Does not tick — call [`Hud::tick`] once
    /// per game tic.
    pub fn render(&self, fb: &mut IndexedFramebuffer, state: &HudState<'_>) {
        if self.message_on {
            self.message.draw(fb, &self.font);
        }
        if state.automap_active {
            let y = HU_TITLE_BASELINE - self.font.glyph_height();
            widgets::draw_text(&self.font, fb, state.title.as_bytes(), HU_TITLEX, y);
        }
    }
}

/// Ticks the HUD and draws it — the one-call entry the game loop uses.
pub fn render_hud(fb: &mut IndexedFramebuffer, hud: &mut Hud, state: &HudState<'_>) {
    hud.tick(state);
    hud.render(fb, state);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Synthetic font: every glyph is 3 px wide, 7 px tall, colored by
    /// its index + 1 (glyph `!` = 1, glyph `<` = 32, ...).
    fn synthetic_font() -> HudFont {
        let glyphs: [Arc<Patch>; HU_FONTSIZE] =
            std::array::from_fn(|i| Arc::new(fill_patch(3, 7, 1 + i as u8)));
        HudFont::from_glyphs(glyphs)
    }

    fn synthetic_hud() -> Hud {
        Hud::new(synthetic_font())
    }

    fn state<'a>(message: Option<&'a str>, title: &'a str) -> HudState<'a> {
        HudState {
            message,
            show_messages: true,
            force_message: false,
            automap_active: false,
            title,
        }
    }

    #[test]
    fn layout_constants_match_vanilla() {
        assert_eq!(HU_FONTSTART, b'!');
        assert_eq!(HU_FONTEND, b'_');
        assert_eq!(HU_FONTSIZE, 63);
        assert_eq!(HU_MSGWIDTH, 64);
        assert_eq!(HU_MSGX, 0);
        assert_eq!(HU_MSGY, 0);
        assert_eq!(HU_MSGHEIGHT, 1);
        assert_eq!(HU_MSGTIMEOUT, 140);
        assert_eq!(HU_TITLEX, 0);
        assert_eq!(HU_TITLE_BASELINE, 167);
        assert_eq!(HU_MAXLINELENGTH, 80);
        assert_eq!(HU_MAXLINES, 4);
    }

    #[test]
    fn fresh_hud_shows_nothing() {
        let hud = synthetic_hud();
        assert!(!hud.message_on());
        assert_eq!(hud.message_counter(), 0);
    }

    #[test]
    fn message_stays_up_for_msgtimeout_then_expires() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("hello"), ""));
        assert!(hud.message_on());
        assert_eq!(hud.message_counter(), HU_MSGTIMEOUT);
        // count down; message stays visible while the counter is nonzero
        for _ in 0..HU_MSGTIMEOUT - 1 {
            hud.tick(&state(None, ""));
        }
        assert!(hud.message_on());
        assert_eq!(hud.message_counter(), 1);
        hud.tick(&state(None, ""));
        assert!(!hud.message_on());
        assert_eq!(hud.message_counter(), 0);
    }

    #[test]
    fn new_message_replaces_old_and_resets_timeout() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("first"), ""));
        for _ in 0..50 {
            hud.tick(&state(None, ""));
        }
        assert_eq!(hud.message_counter(), HU_MSGTIMEOUT - 50);
        hud.tick(&state(Some("second"), ""));
        assert_eq!(hud.message_counter(), HU_MSGTIMEOUT);
        assert_eq!(
            hud.message.line(hud.message.current_line()).text(),
            b"second"
        );
    }

    #[test]
    fn messages_hidden_when_show_messages_off() {
        let mut hud = synthetic_hud();
        let mut s = state(Some("hello"), "");
        s.show_messages = false;
        hud.tick(&s);
        assert!(!hud.message_on());
        // ...unless the one-shot force flag is set (message_dontfuckwithme)
        s.force_message = true;
        hud.tick(&s);
        assert!(hud.message_on());
    }

    #[test]
    fn expired_message_draws_nothing() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("hi"), ""));
        for _ in 0..HU_MSGTIMEOUT {
            hud.tick(&state(None, ""));
        }
        let mut fb = IndexedFramebuffer::new();
        hud.render(&mut fb, &state(None, ""));
        assert_eq!(fb.as_slice(), &[0u8; crate::render::SCREEN_PIXELS]);
    }

    #[test]
    fn message_pixels_land_at_top_row() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("A"), ""));
        let mut fb = IndexedFramebuffer::new();
        hud.render(&mut fb, &state(None, ""));
        // glyph 'A' is 0x41, index 0x41 - 0x21 = 0x20 = 32, color 33
        let color = 1 + (b'A' - HU_FONTSTART);
        assert_eq!(fb.get(0, 0), Some(color));
        assert_eq!(fb.get(2, 6), Some(color));
        assert_eq!(fb.get(3, 0), Some(0));
    }

    #[test]
    fn message_draws_multiple_glyphs_in_order() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("AB"), ""));
        let mut fb = IndexedFramebuffer::new();
        hud.render(&mut fb, &state(None, ""));
        let a = 1 + (b'A' - HU_FONTSTART);
        let b = 1 + (b'B' - HU_FONTSTART);
        assert_eq!(fb.get(0, 0), Some(a));
        assert_eq!(fb.get(3, 0), Some(b)); // 3 px glyphs
        assert_eq!(fb.get(5, 0), Some(b));
    }

    #[test]
    fn level_title_only_draws_on_automap() {
        let hud = synthetic_hud();
        let mut fb = IndexedFramebuffer::new();
        hud.render(&mut fb, &state(None, "E1M1: HANGAR"));
        assert_eq!(fb.as_slice(), &[0u8; crate::render::SCREEN_PIXELS]);

        let mut s = state(None, "E1M1: HANGAR");
        s.automap_active = true;
        hud.render(&mut fb, &s);
        // y = 167 - 7 = 160; glyph color for 'E' (0x45, index 0x24) = 37
        let e = 1 + (b'E' - HU_FONTSTART);
        assert_eq!(fb.get(0, 160), Some(e));
        // row above the glyph area stays clean
        assert_eq!(fb.get(0, 159), Some(0));
    }

    #[test]
    fn render_hud_ticks_and_draws() {
        let mut hud = synthetic_hud();
        let mut fb = IndexedFramebuffer::new();
        let s = state(Some("hello"), "E1M1: HANGAR");
        render_hud(&mut fb, &mut hud, &s);
        assert!(hud.message_on());
        assert_ne!(fb.get(0, 0), Some(0));
        // but the title is not drawn (automap off)
        assert_eq!(fb.get(0, 160), Some(0));
    }

    #[test]
    fn priority_message_blocks_ordinary_until_timeout() {
        let mut hud = synthetic_hud();
        // a forced (priority) message goes up
        let mut forced = state(Some("chat!"), "");
        forced.force_message = true;
        hud.tick(&forced);
        assert!(hud.message_on());
        assert_eq!(
            hud.message.line(hud.message.current_line()).text(),
            b"chat!"
        );
        // an ordinary message arriving while it is up is suppressed
        hud.tick(&state(Some("pickup"), ""));
        assert_eq!(
            hud.message.line(hud.message.current_line()).text(),
            b"chat!"
        );
        // after the priority message times out, ordinary messages flow again
        for _ in 0..HU_MSGTIMEOUT {
            hud.tick(&state(None, ""));
        }
        assert!(!hud.message_on());
        hud.tick(&state(Some("pickup"), ""));
        assert_eq!(
            hud.message.line(hud.message.current_line()).text(),
            b"pickup"
        );
    }

    #[test]
    fn priority_message_shows_even_when_messages_off() {
        let mut hud = synthetic_hud();
        let mut s = state(Some("Messages ON"), "");
        s.show_messages = false;
        s.force_message = true;
        hud.tick(&s);
        assert!(hud.message_on());
    }

    #[test]
    fn lowercase_input_is_drawn_uppercased() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("a"), ""));
        let mut fb = IndexedFramebuffer::new();
        hud.render(&mut fb, &state(None, ""));
        let upper = 1 + (b'A' - HU_FONTSTART);
        assert_eq!(fb.get(0, 0), Some(upper));
    }

    #[test]
    fn loading_missing_lumps_fails_cleanly() {
        let rm = ResourceManager::default();
        let err = Hud::load(&rm).err().unwrap();
        assert!(err.to_string().contains("STCFN033"));
    }

    #[test]
    fn message_round_trips_through_the_scroll_widget() {
        let mut hud = synthetic_hud();
        hud.tick(&state(Some("Picked up a clip."), ""));
        assert_eq!(
            hud.message.line(hud.message.current_line()).text(),
            b"Picked up a clip."
        );
    }
}

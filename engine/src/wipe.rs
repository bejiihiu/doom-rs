//! The screen wipe / pixel melt, ported from `f_wipe.c` (linuxdoom-1.10).
//!
//! The original shipped two effects: `wipe_ColorXForm` (a gradual per-pixel
//! color fade, 8-bit only) and `wipe_Melt` (the "screen melt" every DOOM
//! player knows). Only the melt is ported: the color fade is never used by
//! the game loop (`d_main.c` always calls `wipe_ScreenWipe(wipe_Melt, ...)`)
//! and the game no longer has an 8-bit-per-pixel color transition to fade
//! between, so there is nothing to port.
//!
//! Mechanics, ported 1:1:
//!
//! * the wipe runs on **2-pixel-wide columns** (SCREENWIDTH/2 "short"
//!   columns). Both the start and the end screen are rearranged into
//!   column-major order of 16-bit pixel pairs before the first tick runs —
//!   purely an indexing optimization, no pixels change meaning;
//! * each column gets its own melt offset `y`. Column 0 starts at
//!   `-(rng % 16)` and every following column starts at
//!   `y[i-1] + (rng % 3) - 1`, clamped to `<= 0`, and the value `-16` is
//!   nudged to `-15` (vanilla's exact clamps, including the `== -16` check).
//!   Note vanilla evaluates this for all 320 entries but `wipe_doMelt` only
//!   ever uses the first 160 — we keep the full table and the full 320
//!   [`crate::rng::DoomRng`] draws at init, so demos that consumed the vanilla
//!   sequence stay bit-exact;
//! * per tic, a delayed column counts up toward 0, an active column
//!   advances by `y + 1` while `y < 16` (the melt accelerates) and by 8
//!   afterwards; the step is clipped so it never runs past the bottom of
//!   the screen. Pixels from the end screen are written from row `y` down,
//!   the untouched start-screen rows are shifted down behind them;
//! * the melt finishes when every column has reached the bottom; the
//!   column state is freed (vanilla `wipe_exitMelt`) and the next
//!   transition starts from scratch.
//!
//! The framebuffer is the single source of truth; the start/end screens
//! are private snapshot buffers (the original's `screens[2]`/`screens[3]`).

use crate::render::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_PIXELS, SCREEN_WIDTH};
use crate::rng::DoomRng;

/// Vanilla `SCREENHEIGHT` in melt units (the melt works on pixel rows).
const HEIGHT: i32 = SCREEN_HEIGHT as i32;

/// Vanilla operates on little-endian shorts: 2 screen pixels per unit.
const SHORTS_PER_ROW: usize = SCREEN_WIDTH / 2;

/// Number of "short" columns the melt actually scrolls (`width / 2` in
/// `wipe_doMelt`). The remaining entries of the position table are
/// initialized for parity with the original but never touched again.
const MELT_COLUMNS: usize = SCREEN_WIDTH / 2;

/// Reads short `index` from `buf`, little-endian, like the x86 `short*`
/// pokes in the original.
#[inline]
fn read_short_le(buf: &[u8], index: usize) -> u16 {
    u16::from_le_bytes([buf[index * 2], buf[index * 2 + 1]])
}

/// Writes short `index` into `buf`, little-endian.
#[inline]
fn write_short_le(buf: &mut [u8], index: usize, value: u16) {
    buf[index * 2] = value as u8;
    buf[index * 2 + 1] = (value >> 8) as u8;
}

/// Rearranges a row-major 320x200 8-bit image into "shitty column-major"
/// order, exactly like `wipe_shittyColMajorXform`: element `x*HEIGHT + y`
/// of the destination is the row-major pixel pair `(y, x)`.
fn column_major_xform(src: &[u8; SCREEN_PIXELS]) -> Vec<u16> {
    let mut dest = vec![0u16; SCREEN_PIXELS / 2];
    for y in 0..SCREEN_HEIGHT {
        for x in 0..SHORTS_PER_ROW {
            dest[x * SCREEN_HEIGHT + y] = read_short_le(src, y * SHORTS_PER_ROW + x);
        }
    }
    dest
}

/// The pixel-melt screen transition (`wipe_Melt`).
///
/// The wipe is driven by the frame loop, three phases per transition:
///
/// 1. [`ScreenWipe::capture_start`] — snapshot the *current* screen (the
///    frame the player is looking at when the transition is triggered);
/// 2. the new screen is drawn into the framebuffer;
/// 3. [`ScreenWipe::capture_end`] — snapshot the freshly drawn screen and
///    restore the start screen into the framebuffer (so the melt starts
///    from the old frame and drips the new one in), then call
///    [`ScreenWipe::update`] every frame until it returns `true`.
pub struct ScreenWipe {
    /// Raw snapshot of the start screen (vanilla `wipe_scr_start`,
    /// `screens[2]`).
    start: [u8; SCREEN_PIXELS],
    /// Raw snapshot of the end screen (vanilla `wipe_scr_end`,
    /// `screens[3]`).
    end: [u8; SCREEN_PIXELS],
    /// Start screen in column-major short order (`wipe_shittyColMajorXform`).
    start_cols: Vec<u16>,
    /// End screen in column-major short order.
    end_cols: Vec<u16>,
    /// Per-column melt offsets in rows; negative means "still waiting
    /// at the top". Length 320 (vanilla allocates `width`), only the
    /// first [`MELT_COLUMNS`] are used by the melt proper.
    y: [i32; SCREEN_WIDTH],
    /// Whether the melt has been initialized (vanilla's `go`).
    running: bool,
}

impl Default for ScreenWipe {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenWipe {
    /// A fresh, unused wipe. Vanilla's static `go = 0`.
    pub fn new() -> Self {
        Self {
            start: [0; SCREEN_PIXELS],
            end: [0; SCREEN_PIXELS],
            start_cols: vec![0; SCREEN_PIXELS / 2],
            end_cols: vec![0; SCREEN_PIXELS / 2],
            y: [0; SCREEN_WIDTH],
            running: false,
        }
    }

    /// Snapshot the framebuffer as the wipe's start screen
    /// (`wipe_StartScreen`). Call this when a screen-state change is
    /// detected, *before* the new frame is drawn.
    pub fn capture_start(&mut self, fb: &IndexedFramebuffer) {
        self.start.copy_from_slice(fb.as_slice());
    }

    /// Snapshot the framebuffer (now holding the finished new frame) as
    /// the wipe's end screen, then restore the start screen into
    /// the framebuffer (`wipe_EndScreen`). Call after drawing everything
    /// the new state shows.
    pub fn capture_end(&mut self, fb: &mut IndexedFramebuffer) {
        self.end.copy_from_slice(fb.as_slice());
        fb.as_mut_slice().copy_from_slice(&self.start);
    }

    /// Whether a melt is in flight (between the first [`update`](Self::update)
    /// and the frame that reports it complete).
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Advance the melt by `ticks` frames and return `true` when it is
    /// finished (`wipe_ScreenWipe(wipe_Melt, ...)`, including the hidden
    /// first-call init).
    ///
    /// Negative `ticks` are treated as no time passing (the vanilla
    /// `while (ticks--)` loop would run for two billion iterations, which
    /// helps nobody). The framebuffer is updated in place.
    pub fn update(&mut self, fb: &mut IndexedFramebuffer, rng: &mut DoomRng, ticks: i32) -> bool {
        if !self.running {
            self.init_melt(fb, rng);
        }

        // vanilla: done = true; while (ticks--) { ... if (all columns
        // reached the bottom) done = true }
        let mut done = true;
        let mut remaining = ticks.max(0);
        while remaining > 0 {
            remaining -= 1;
            for i in 0..MELT_COLUMNS {
                if self.y[i] < 0 {
                    // not ready to scroll yet
                    self.y[i] += 1;
                    done = false;
                } else if self.y[i] < HEIGHT {
                    // accelerate: y+1 until row 16, then a steady 8 rows/tic
                    let mut dy = if self.y[i] < 16 { self.y[i] + 1 } else { 8 };
                    if self.y[i] + dy >= HEIGHT {
                        dy = HEIGHT - self.y[i];
                    }
                    let from_row = self.y[i] as usize;
                    for j in 0..dy as usize {
                        let v = self.end_cols[i * SCREEN_HEIGHT + from_row + j];
                        write_short_le(fb.as_mut_slice(), (from_row + j) * SHORTS_PER_ROW + i, v);
                    }
                    self.y[i] += dy;
                    let to_row = self.y[i] as usize;
                    for j in 0..SCREEN_HEIGHT - to_row {
                        let v = self.start_cols[i * SCREEN_HEIGHT + j];
                        write_short_le(fb.as_mut_slice(), (to_row + j) * SHORTS_PER_ROW + i, v);
                    }
                    done = false;
                }
            }
        }

        if done {
            // vanilla wipe_exitMelt: drop the column state
            self.running = false;
            self.start_cols.clear();
            self.end_cols.clear();
        }
        done
    }

    /// Vanilla `wipe_initMelt`: restore the start screen, rebuild the
    /// column-major copies and roll the per-column melt offsets.
    fn init_melt(&mut self, fb: &mut IndexedFramebuffer, rng: &mut DoomRng) {
        // copy start screen to main screen (vanilla does this here)
        fb.as_mut_slice().copy_from_slice(&self.start);

        self.start_cols = column_major_xform(&self.start);
        self.end_cols = column_major_xform(&self.end);

        // setup initial column positions (y < 0 => not ready to scroll yet)
        self.y[0] = -((rng.next() % 16) as i32);
        for i in 1..SCREEN_WIDTH {
            let r = (rng.next() % 3) as i32 - 1;
            self.y[i] = self.y[i - 1] + r;
            if self.y[i] > 0 {
                self.y[i] = 0;
            } else if self.y[i] == -16 {
                self.y[i] = -15;
            }
        }
        self.running = true;
    }

    /// Per-column melt offsets (test introspection).
    #[cfg(test)]
    pub(crate) fn column_offsets(&self) -> &[i32; SCREEN_WIDTH] {
        &self.y
    }

    /// The number of random draws [`init_melt`](Self::init_melt) consumes
    /// (vanilla burns 320 `M_Random()` calls entering a wipe).
    #[cfg(test)]
    const RNG_DRAWS_ON_INIT: usize = SCREEN_WIDTH;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a framebuffer filled with a deterministic pattern:
    /// byte = (x * 7 + y * 13 + salt) & 0xFF.
    fn pattern_fb(salt: u8) -> IndexedFramebuffer {
        let mut fb = IndexedFramebuffer::new();
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                fb.set(x, y, ((x * 7 + y * 13 + salt as usize) & 0xff) as u8);
            }
        }
        fb
    }

    #[test]
    fn capture_end_restores_start_screen() {
        let mut wipe = ScreenWipe::new();
        let start = pattern_fb(0x11);
        wipe.capture_start(&start);

        let mut fb = pattern_fb(0x77);
        wipe.capture_end(&mut fb);
        assert_eq!(fb, start);
    }

    #[test]
    fn column_major_xform_matches_vanilla_layout() {
        // 4 px wide, 2 px tall image of distinct bytes:
        //   row 0: 01 02 03 04
        //   row 1: 11 12 13 14
        let mut src = [0u8; SCREEN_PIXELS];
        src[0] = 0x01;
        src[1] = 0x02;
        src[2] = 0x03;
        src[3] = 0x04;
        src[SCREEN_WIDTH] = 0x11;
        src[SCREEN_WIDTH + 1] = 0x12;
        src[SCREEN_WIDTH + 2] = 0x13;
        src[SCREEN_WIDTH + 3] = 0x14;
        let cols = column_major_xform(&src);
        // short column 0: shorts 0x0201, 0x1211 (bytes 0x01 0x02 / 0x11 0x12)
        assert_eq!(cols[0], 0x0201);
        assert_eq!(cols[1], 0x1211);
        // short column 1: shorts 0x0403, 0x1413
        assert_eq!(cols[SCREEN_HEIGHT], 0x0403);
        assert_eq!(cols[SCREEN_HEIGHT + 1], 0x1413);
    }

    #[test]
    fn init_melt_consumes_exactly_320_rng_draws() {
        let mut wipe = ScreenWipe::new();
        wipe.capture_start(&pattern_fb(1));
        let mut fb = pattern_fb(2);
        wipe.capture_end(&mut fb);
        let mut rng = DoomRng::new();
        wipe.update(&mut fb, &mut rng, 1);
        assert_eq!(rng.index(), ScreenWipe::RNG_DRAWS_ON_INIT % 256);
    }

    #[test]
    fn y_offsets_match_vanilla_formula() {
        // seed 0: first M_Random() bytes are 8, 109, 220, 222, ...
        let mut rng = DoomRng::new();
        let y0 = -((rng.next() % 16) as i32);
        assert_eq!(y0, -8);
        let mut expected = [0i32; SCREEN_WIDTH];
        expected[0] = y0;
        for i in 1..SCREEN_WIDTH {
            let r = (rng.next() % 3) as i32 - 1;
            expected[i] = expected[i - 1] + r;
            if expected[i] > 0 {
                expected[i] = 0;
            } else if expected[i] == -16 {
                expected[i] = -15;
            }
        }

        let mut wipe = ScreenWipe::new();
        wipe.capture_start(&pattern_fb(1));
        let mut fb = pattern_fb(2);
        wipe.capture_end(&mut fb);
        let mut fresh = DoomRng::new();
        wipe.update(&mut fb, &mut fresh, 0); // ticks=0: init only, no advance
        assert_eq!(wipe.column_offsets(), &expected);
    }

    #[test]
    fn y_offsets_are_clamped() {
        for seed in 0..256 {
            let mut rng = DoomRng::with_seed(seed);
            let mut y = [0i32; SCREEN_WIDTH];
            y[0] = -((rng.next() % 16) as i32);
            for i in 1..SCREEN_WIDTH {
                let r = (rng.next() % 3) as i32 - 1;
                y[i] = y[i - 1] + r;
                if y[i] > 0 {
                    y[i] = 0;
                } else if y[i] == -16 {
                    y[i] = -15;
                }
                // invariants the clamps guarantee
                assert!(y[i] <= 0, "seed {seed} col {i}: positive {y}", y = y[i]);
                assert!(y[i] >= -15, "seed {seed} col {i}: too low {y}", y = y[i]);
            }
        }
    }

    #[test]
    fn melt_completes_at_the_bottom_and_shows_end_screen() {
        let start = pattern_fb(0xAA);
        let end = pattern_fb(0xBB);
        let mut wipe = ScreenWipe::new();
        wipe.capture_start(&start);
        let mut fb = end.clone();
        wipe.capture_end(&mut fb);
        assert_eq!(fb, start, "start screen restored before the melt");

        let mut rng = DoomRng::new();
        let mut frames = 0;
        loop {
            assert!(frames < 2000, "melt never finished");
            if wipe.update(&mut fb, &mut rng, 1) {
                break;
            }
            frames += 1;
        }
        assert_eq!(fb, end, "after the wipe the screen shows the new frame");
        assert!(!wipe.is_running());
    }

    #[test]
    fn melt_is_deterministic_for_a_seed() {
        let run = || {
            let start = pattern_fb(1);
            let end = pattern_fb(2);
            let mut wipe = ScreenWipe::new();
            wipe.capture_start(&start);
            let mut fb = end;
            wipe.capture_end(&mut fb);
            let mut rng = DoomRng::with_seed(42);
            for _ in 0..30 {
                wipe.update(&mut fb, &mut rng, 1);
            }
            fb
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn delayed_columns_wait_at_the_top() {
        // seed 0 gives y[0] = -8: column 0 must stay untouched for 8 tics
        let start = pattern_fb(1);
        let end = pattern_fb(2);
        let mut wipe = ScreenWipe::new();
        wipe.capture_start(&start);
        let mut fb = end;
        wipe.capture_end(&mut fb);
        let mut rng = DoomRng::new();

        // one tic: nothing has reached row 0 of the end screen yet for col 0
        wipe.update(&mut fb, &mut rng, 1);
        assert_eq!(wipe.column_offsets()[0], -7);
        // but other columns may already be dripping — just check col 0
        for _ in 0..7 {
            wipe.update(&mut fb, &mut rng, 1);
        }
        assert_eq!(wipe.column_offsets()[0], 0);
    }

    #[test]
    fn multiple_ticks_per_update_match_repeated_updates() {
        let start = pattern_fb(3);
        let end = pattern_fb(4);
        let mut a = ScreenWipe::new();
        a.capture_start(&start);
        let mut fb_a = end.clone();
        a.capture_end(&mut fb_a);
        let mut rng_a = DoomRng::new();
        a.update(&mut fb_a, &mut rng_a, 5);

        let mut b = ScreenWipe::new();
        b.capture_start(&start);
        let mut fb_b = end;
        b.capture_end(&mut fb_b);
        let mut rng_b = DoomRng::new();
        for _ in 0..5 {
            b.update(&mut fb_b, &mut rng_b, 1);
        }
        assert_eq!(fb_a, fb_b);
        assert_eq!(rng_a.index(), rng_b.index());
    }

    #[test]
    fn negative_ticks_do_not_advance_the_melt() {
        let start = pattern_fb(5);
        let end = pattern_fb(6);
        let mut wipe = ScreenWipe::new();
        wipe.capture_start(&start);
        let mut fb = end;
        wipe.capture_end(&mut fb);
        let mut rng = DoomRng::new();
        let fb2 = fb.clone();
        wipe.update(&mut fb, &mut rng, -3);
        // only the init happened; the frame still shows the start screen
        assert_eq!(fb, fb2);
    }

    #[test]
    fn done_wipe_resets_state_for_reuse() {
        let start = pattern_fb(7);
        let mut wipe = ScreenWipe::new();
        wipe.capture_start(&start);
        let mut fb = pattern_fb(8);
        wipe.capture_end(&mut fb);
        let mut rng = DoomRng::new();
        loop {
            if wipe.update(&mut fb, &mut rng, 1) {
                break;
            }
        }
        assert!(!wipe.is_running());
        // the melt state is dropped — vectors are cleared
        assert_eq!(wipe.start_cols.len(), 0);
        assert_eq!(wipe.end_cols.len(), 0);
    }

    #[test]
    fn half_screen_alignment_uses_shorts() {
        // the melt scrolls 2-pixel columns; an uneven (i=1) column touches
        // bytes 2 and 3 of a row, never byte 0 or byte 4
        let mut src = [0u8; SCREEN_PIXELS];
        src.fill(0x50);
        let cols = column_major_xform(&src);
        assert_eq!(cols.len(), SCREEN_PIXELS / 2);
        // every transformed short is a pair of 0x50 pixels
        assert!(cols.iter().all(|&v| v == 0x5050));
    }
}

//! The F8 debug overlay: frame rate and frame-time statistics over a
//! sliding window of the last 60 rendered frames.
//!
//! Purely a render-side tool for whoever is staring at the engine: it
//! never feeds the simulation, the RNG, demo playback, recording or
//! save/load. `advance`/`tick_input` hand it the same `elapsed_ms`
//! wall-clock delta they already receive, so no new timer system exists
//! for this to hook into.
//!
//! The overlay draws on every frame, above everything else (above the
//! menu, above the wipe), as three solid black bands in the top-left
//! corner with white-ish STCFN text: the current FPS, the average frame
//! time in milliseconds and the standard deviation of the frame time
//! ("variance" in ms — one decimal, same as the rest). With an empty
//! window nothing is drawn at all; there is nothing meaningful to report.

use crate::hud::{HudFont, draw_text, text_width};
use crate::render::{IndexedFramebuffer, SCREEN_WIDTH};

/// How many frames the rolling window holds.
pub(crate) const FRAME_WINDOW: usize = 60;
/// Height of one black band in pixels (fits the 7 px STCFN glyphs).
const BAND_HEIGHT: i32 = 10;
/// Top offset of the first band, per the layout (left-top, 2 px in).
const BAND_TOP: i32 = 2;
/// Left offset of the text inside the bands.
const TEXT_X: i32 = 2;
/// Vertical offset of the text inside a band.
const TEXT_DY: i32 = 1;

/// Sliding-window frame-time statistics (population variance).
///
/// Feed it one sample per rendered frame with [`FrameTimeStats::record`];
/// every statistic then reflects the last [`FRAME_WINDOW`] frames. When
/// the window carries no samples yet, the statistic getters return `None`
/// and callers are expected to draw nothing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameTimeStats {
    ring: [u64; FRAME_WINDOW],
    /// Number of samples currently in the ring (never more than
    /// [`FRAME_WINDOW`]).
    len: usize,
    /// Write position of the next sample.
    next: usize,
    sum: u64,
    sum_sq: u128,
}

impl Default for FrameTimeStats {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameTimeStats {
    pub(crate) fn new() -> Self {
        Self {
            ring: [0; FRAME_WINDOW],
            len: 0,
            next: 0,
            sum: 0,
            sum_sq: 0,
        }
    }

    /// Records one frame's wall-clock delta in milliseconds.
    pub(crate) fn record(&mut self, dt_ms: u64) {
        let old = std::mem::replace(&mut self.ring[self.next], dt_ms);
        let dt_sq = (dt_ms as u128) * (dt_ms as u128);
        if self.len < FRAME_WINDOW {
            self.len += 1;
            self.sum += dt_ms;
            self.sum_sq += dt_sq;
        } else {
            let old_sq = (old as u128) * (old as u128);
            // subtracting an evicted sample can transiently hurt the
            // running sums more than the new sample adds back — that's
            // fine, they stay exact integers until the very last bit
            self.sum = self.sum - old + dt_ms;
            self.sum_sq = self.sum_sq - old_sq + dt_sq;
        }
        self.next = (self.next + 1) % FRAME_WINDOW;
    }

    /// Number of samples currently in the window.
    pub(crate) fn count(&self) -> usize {
        self.len
    }

    /// Mean frame time in milliseconds, or `None` on an empty window.
    pub(crate) fn avg_ms(&self) -> Option<f64> {
        (self.len > 0).then(|| self.sum as f64 / self.len as f64)
    }

    /// Population variance of the frame time in ms², or `None` on an
    /// empty window. Clamped at zero — the `E[x²] − E[x]²` form can go
    /// slightly negative on catastrophic cancellation.
    pub(crate) fn variance_ms2(&self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        let n = self.len as f64;
        let mean = self.sum as f64 / n;
        let var = self.sum_sq as f64 / n - mean * mean;
        Some(var.max(0.0))
    }

    /// Standard deviation of the frame time in milliseconds, or `None`
    /// on an empty window.
    pub(crate) fn stddev_ms(&self) -> Option<f64> {
        self.variance_ms2().map(f64::sqrt)
    }

    /// Frames per second derived from the mean frame time. A mean of
    /// zero (a run of zero-length frames) reports `0.0` rather than
    /// dividing by zero.
    pub(crate) fn fps(&self) -> Option<f64> {
        self.avg_ms()
            .map(|avg| if avg > 0.0 { 1000.0 / avg } else { 0.0 })
    }
}

/// The toggle state and statistics for the debug overlay.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FpsOverlay {
    /// Whether the overlay is drawn. Toggled by the F8 key.
    pub(crate) enabled: bool,
    stats: FrameTimeStats,
}

impl FpsOverlay {
    /// Adds one frame's wall-clock delta to the statistics window. Safe
    /// to call every frame regardless of whether the overlay is enabled.
    pub(crate) fn record_frame(&mut self, dt_ms: u64) {
        self.stats.record(dt_ms);
    }

    /// Flips the overlay on or off (the F8 key).
    pub(crate) fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    /// Whether the overlay is currently drawn.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Draws the overlay when enabled; draws nothing with an empty window.
    pub(crate) fn draw(&self, fb: &mut IndexedFramebuffer, font: &HudFont) {
        if !self.enabled {
            return;
        }
        // no samples yet: skip the telemetry entirely (spec: empty window
        // -> nothing drawn)
        if self.stats.count() == 0 {
            return;
        }
        let Some(avg_ms) = self.stats.avg_ms() else {
            return;
        };
        let fps = self.stats.fps().unwrap_or(0.0);
        let stddev = self.stats.stddev_ms().unwrap_or(0.0);
        let lines = [
            format!("FPS {fps:.1}"),
            format!("FT {avg_ms:.1}ms"),
            format!("VAR {stddev:.1}ms"),
        ];
        for (index, line) in lines.iter().enumerate() {
            let y = BAND_TOP + index as i32 * BAND_HEIGHT;
            let width = text_width(font, line.as_bytes());
            // solid black band, 2 px of margin to the right of the text
            let band_width = (TEXT_X + width + 2).clamp(0, SCREEN_WIDTH as i32);
            for band_y in y..(y + BAND_HEIGHT) {
                let Ok(band_y) = usize::try_from(band_y) else {
                    continue;
                };
                if let Some(row) = fb.row_mut(band_y) {
                    for px in row.iter_mut().take(band_width as usize) {
                        *px = 0; // solid black, not index-transparency
                    }
                }
            }
            draw_text(font, fb, line.as_bytes(), TEXT_X, y + TEXT_DY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_window_is_empty() {
        let stats = FrameTimeStats::new();
        assert_eq!(stats.count(), 0);
        assert_eq!(stats.avg_ms(), None);
        assert_eq!(stats.variance_ms2(), None);
        assert_eq!(stats.stddev_ms(), None);
        assert_eq!(stats.fps(), None);
    }

    #[test]
    fn first_frame_is_not_empty_and_has_zero_variance() {
        let mut stats = FrameTimeStats::new();
        stats.record(16);
        assert_eq!(stats.count(), 1);
        assert_eq!(stats.avg_ms(), Some(16.0));
        assert_eq!(stats.variance_ms2(), Some(0.0));
        assert_eq!(stats.stddev_ms(), Some(0.0));
        assert_eq!(stats.fps(), Some(62.5));
    }

    #[test]
    fn avg_and_variance_of_known_samples() {
        let mut stats = FrameTimeStats::new();
        for dt in [10, 20, 30] {
            stats.record(dt);
        }
        assert_eq!(stats.avg_ms(), Some(20.0));
        // population variance: E[x^2] - E[x]^2 = (100+400+900)/3 - 400
        let var = stats.variance_ms2().expect("variance of samples");
        assert!((var - 200.0 / 3.0).abs() < 1e-9, "var = {var}");
        assert!((stats.stddev_ms().unwrap() - (200.0_f64 / 3.0).sqrt()).abs() < 1e-9);
        assert!((stats.fps().unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn variance_of_steady_window() {
        let mut stats = FrameTimeStats::new();
        for dt in [16, 16, 16, 17, 17] {
            stats.record(dt);
        }
        let avg = stats.avg_ms().unwrap();
        assert!((avg - 16.4).abs() < 1e-9);
        let var = stats.variance_ms2().unwrap();
        assert!((var - 0.24).abs() < 1e-9, "var = {var}");
    }

    #[test]
    fn window_wraps_and_evicts_the_oldest_sample() {
        let mut stats = FrameTimeStats::new();
        for _ in 0..FRAME_WINDOW {
            stats.record(16);
        }
        assert_eq!(stats.count(), FRAME_WINDOW);
        stats.record(100); // one of the 16s gets evicted
        assert_eq!(stats.count(), FRAME_WINDOW);
        let avg = stats.avg_ms().unwrap();
        // 59 surviving 16 ms samples and the fresh 100 ms one
        let expected = (59.0 * 16.0 + 100.0) / FRAME_WINDOW as f64;
        assert!((avg - expected).abs() < 1e-9, "avg = {avg}");
    }

    #[test]
    fn zero_length_frames_report_zero_fps_without_dividing_by_zero() {
        let mut stats = FrameTimeStats::new();
        stats.record(0);
        assert_eq!(stats.avg_ms(), Some(0.0));
        assert_eq!(stats.fps(), Some(0.0));
        assert_eq!(stats.stddev_ms(), Some(0.0));
    }

    #[test]
    fn huge_samples_do_not_cancel_into_a_negative_variance() {
        let mut stats = FrameTimeStats::new();
        stats.record(1_000_000_000);
        stats.record(1_000_000_001);
        let var = stats.variance_ms2().expect("variance");
        assert!(var >= 0.0, "var = {var}");
    }

    #[test]
    fn toggle_flips_the_overlay() {
        let mut overlay = FpsOverlay::default();
        assert!(!overlay.enabled());
        overlay.toggle();
        assert!(overlay.enabled());
        overlay.toggle();
        assert!(!overlay.enabled());
    }

    #[test]
    fn empty_window_has_no_statistics() {
        // the overlay itself draws nothing with an empty window; the
        // statistics getters are the contract that emptiness can't crash
        let stats = FrameTimeStats::new();
        assert_eq!(stats.count(), 0);
        assert!(stats.avg_ms().is_none());
        assert!(stats.variance_ms2().is_none());
        assert!(stats.stddev_ms().is_none());
        assert!(stats.fps().is_none());
    }
}

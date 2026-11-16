//! `I_GetTime` for the net layer: time in 1/35-second tics since startup.
//!
//! The engine never reads the wall clock directly; the platform shell hands
//! in a [`TimeSource`] (or explicit tic counts) so net sync stays
//! deterministic and headless-testable. [`InstantTimeSource`] is the
//! production clock built on [`std::time::Instant`] — the direct equivalent
//! of vanilla's `gettimeofday`-based `I_GetTime`.

use crate::game::TICRATE;

/// `I_GetTime`: tics (at `TICRATE` Hz) since some unspecified epoch.
pub trait TimeSource {
    /// Current time in tics.
    fn now_tics(&self) -> i32;

    /// Advance the internal clock by `tics`. Production clocks
    /// ([`InstantTimeSource`], [`CountingTime`]) keep their own cadence and
    /// ignore this; the deterministic [`ManualTime`] honors it, so tests
    /// can step the simulation one tic at a time.
    fn advance(&self, _tics: i32) {}
}

/// The real-time [`TimeSource`] backed by [`std::time::Instant`].
///
/// Ticks never go backwards and start at 0 when the source is created
/// (like vanilla, where the first call bases the epoch on then).
#[derive(Debug, Clone, Copy)]
pub struct InstantTimeSource {
    base: std::time::Instant,
}

impl InstantTimeSource {
    /// Creates a source whose epoch is now.
    pub fn new() -> Self {
        Self {
            base: std::time::Instant::now(),
        }
    }
}

impl Default for InstantTimeSource {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeSource for InstantTimeSource {
    fn now_tics(&self) -> i32 {
        let el = self.base.elapsed();
        let micros = el.as_secs() as i64 * 1_000_000 + i64::from(el.subsec_micros());
        (micros * i64::from(TICRATE) / 1_000_000) as i32
    }
}

/// A manually driven [`TimeSource`] for deterministic tests.
///
/// Ticks only advance when [`ManualTime::advance`] is called. It is public
/// so engine users can exercise the net layer in headless harnesses; game
/// code should use [`InstantTimeSource`].
#[derive(Debug)]
pub struct ManualTime {
    tics: std::cell::Cell<i32>,
}

impl ManualTime {
    /// A source sitting at tic 0.
    pub fn new() -> Self {
        Self {
            tics: std::cell::Cell::new(0),
        }
    }

    /// A source starting at an arbitrary tic.
    pub fn at(tics: i32) -> Self {
        Self {
            tics: std::cell::Cell::new(tics),
        }
    }
}

impl Default for ManualTime {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeSource for ManualTime {
    fn now_tics(&self) -> i32 {
        self.tics.get()
    }

    fn advance(&self, tics: i32) {
        self.tics.set(self.tics.get() + tics);
    }
}

/// A [`TimeSource`] that increments on every read.
///
/// Used by the net tests to drive the `TryRunTics` wait loop out of its
/// 20-tic bailout: with a frozen clock the loop would never see time pass.
/// Each read returns the current value and then advances by one tic.
#[derive(Debug)]
pub struct CountingTime {
    tics: std::cell::Cell<i32>,
}

impl CountingTime {
    /// A source starting at tic 0.
    pub fn new() -> Self {
        Self {
            tics: std::cell::Cell::new(0),
        }
    }
}

impl Default for CountingTime {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeSource for CountingTime {
    fn now_tics(&self) -> i32 {
        let t = self.tics.get();
        self.tics.set(t + 1);
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_time_advances_only_on_demand() {
        let t = ManualTime::new();
        assert_eq!(t.now_tics(), 0);
        t.advance(10);
        assert_eq!(t.now_tics(), 10);
        assert_eq!(t.now_tics(), 10); // no auto-increment
        t.advance(5);
        assert_eq!(t.now_tics(), 15);
    }

    #[test]
    fn counting_time_increments_per_read() {
        let t = CountingTime::new();
        assert_eq!(t.now_tics(), 0);
        assert_eq!(t.now_tics(), 1);
        assert_eq!(t.now_tics(), 2);
    }

    #[test]
    fn instant_time_counts_tics() {
        let t = InstantTimeSource::new();
        let a = t.now_tics();
        let b = t.now_tics();
        assert!(b >= a);
        assert!(a < TICRATE); // the test can't take a full second
    }
}

//! Music playback: parse, synthesize, and serve samples.
//!
//! [`MusicEngine`] owns a rendered PCM buffer and a playback cursor. The
//! engine deliberately does **not** know anything about output hardware:
//! the platform layer (or the sound mixer) pulls mono `f32` samples through
//! [`MusicEngine::pull_samples`] on whatever clock it runs on, and the
//! track progresses at its own authentic rate. This keeps the music module
//! backend-agnostic and deterministic, same as the rest of the engine.
//!
//! ## Rendering strategy
//!
//! Rendering happens eagerly in [`MusicEngine::play_mus`]: the whole track
//! is synthesized to a `Vec<f32>` up front via [`opl_audio::render`]. A
//! DOOM track is small (well under 64 KiB of MUS data, a handful of
//! minutes of audio), and the OPL2 emulator is cheap; paying the whole
//! synthesis cost once keeps the mixing path allocation-free afterwards.
//!
//! ## Timing
//!
//! DMX ticks are 1/140th of a second. The conversion in [`crate::music::midi`]
//! encodes the song as MIDI with `division = 140`, `bpm = 60`, so one
//! rendered sample tick equals one DMX tick and the music plays at exactly
//! the speed the original engine produced. The output runs at whatever
//! `sample_rate` the engine was created with; pitch does not depend on it.
//!
//! ## Looping
//!
//! Doom loops its title and game music, so the engine loops by default
//! ([`MusicEngine::set_looping`]). With looping off, the track stops at the
//! end and [`MusicEngine::is_playing`] reports `false`.

use opl_audio::{RenderOpts, render};

use crate::music::error::MusicError;
use crate::music::genmidi::GenmidiBank;
use crate::music::midi::{MUS_BPM, MUS_DIVISION, mus_to_midi};
use crate::music::mus::parse_mus;

/// Volume used to scale the rendered PCM, in the 0-127 range Doom stores
/// its music volume in.
pub const VOLUME_MAX: u8 = 127;

/// The default output sample rate for [`MusicEngine::default`].
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// A music player: parses one track at a time and serves it as samples.
///
/// The engine is single-buffered: playing a new track replaces the
/// current one. All methods are cheap; the only expensive call is
/// [`MusicEngine::play_mus`], which synthesizes the track.
#[derive(Debug)]
pub struct MusicEngine {
    sample_rate: u32,
    volume: u8,
    looping: bool,
    samples: Option<Vec<f32>>,
    cursor: usize,
    playing: bool,
}

impl Default for MusicEngine {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLE_RATE)
    }
}

impl MusicEngine {
    /// Creates a music engine rendering at `sample_rate` Hz.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            volume: VOLUME_MAX,
            looping: true,
            samples: None,
            cursor: 0,
            playing: false,
        }
    }

    /// The sample rate the engine renders at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Current output volume, 0-127.
    pub fn volume(&self) -> u8 {
        self.volume
    }

    /// Sets the output volume, clamped to 0-127.
    pub fn set_volume(&mut self, volume: u8) {
        self.volume = volume.min(VOLUME_MAX);
    }

    /// Whether the track loops when it reaches the end.
    pub fn looping(&self) -> bool {
        self.looping
    }

    /// Sets whether the track loops (the default) or stops at the end.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Whether a track is currently loaded and audible.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Number of samples in the loaded track, or 0 when nothing is loaded.
    pub fn total_samples(&self) -> usize {
        self.samples.as_ref().map_or(0, Vec::len)
    }

    /// Sample position within the loaded track.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Duration of the loaded track in seconds, or 0 when nothing is loaded.
    pub fn duration_seconds(&self) -> f64 {
        self.total_samples() as f64 / f64::from(self.sample_rate)
    }

    /// Renders a MUS lump to PCM, builds the playback state from it, and
    /// starts playback from the beginning.
    ///
    /// The track is synthesized with [`GenmidiBank`]; pass a bank parsed
    /// from the `GENMIDI` lump, or [`GenmidiBank::fallback_bank`] via
    /// [`MusicEngine::play_mus_fallback`] when the lump is missing.
    pub fn play_mus(&mut self, mus_data: &[u8], bank: &GenmidiBank) -> Result<(), MusicError> {
        let song = parse_mus(mus_data)?;
        let opl_bank = bank.to_opl_bank();
        let pcm = synthesize(&song, Some(bank), &opl_bank, self.sample_rate)?;
        self.samples = Some(pcm);
        self.cursor = 0;
        self.playing = true;
        Ok(())
    }

    /// Renders a MUS lump with the built-in generic instrument bank.
    ///
    /// Sounds nothing like DOOM's AdLib score — the instruments are one
    /// bright, sustaining voice shared by every program — but keeps the
    /// music audible for WADs that do not ship a `GENMIDI` lump.
    pub fn play_mus_fallback(&mut self, mus_data: &[u8]) -> Result<(), MusicError> {
        let song = parse_mus(mus_data)?;
        let opl_bank = GenmidiBank::fallback_bank();
        let pcm = synthesize(&song, None, &opl_bank, self.sample_rate)?;
        self.samples = Some(pcm);
        self.cursor = 0;
        self.playing = true;
        Ok(())
    }

    /// Stops playback and unloads the track.
    pub fn stop(&mut self) {
        self.samples = None;
        self.cursor = 0;
        self.playing = false;
    }

    /// Fills `out` with the next samples of the track.
    ///
    /// The whole buffer is always filled: samples past the end of the
    /// track are silence. When looping is enabled the track wraps instead
    /// of stopping. Returns the number of samples that came from the
    /// rendered track (the remainder, if any, is silence).
    pub fn pull_samples(&mut self, out: &mut [f32]) -> usize {
        out.fill(0.0);
        let Some(samples) = &self.samples else {
            return 0;
        };
        if !self.playing || samples.is_empty() {
            return 0;
        }

        let gain = f32::from(self.volume) / f32::from(VOLUME_MAX);
        let mut written = 0usize;
        let mut index = 0usize;
        while index < out.len() {
            if self.cursor >= samples.len() {
                if self.looping {
                    self.cursor = 0;
                } else {
                    self.playing = false;
                    break;
                }
            }
            let available = samples.len() - self.cursor;
            let take = available.min(out.len() - index);
            for (dst, src) in out[index..index + take]
                .iter_mut()
                .zip(&samples[self.cursor..self.cursor + take])
            {
                *dst = src * gain;
            }
            self.cursor += take;
            index += take;
            written += take;
        }
        written
    }
}

/// Renders a parsed song to mono PCM.
///
/// `note_bank` selects the instrument set used for the note remapping in
/// the MIDI conversion; pass the parsed bank to get fixed-pitch and base
/// note offset handling, or `None` to play raw notes. `opl_bank` is the
/// instrument bank the OPL emulator plays from.
fn synthesize(
    song: &crate::music::mus::MusSong,
    note_bank: Option<&GenmidiBank>,
    opl_bank: &opl_audio::OplBank,
    sample_rate: u32,
) -> Result<Vec<f32>, MusicError> {
    let conversion = mus_to_midi(song, note_bank);
    if conversion.events.is_empty() {
        return Err(MusicError::EmptyTrack);
    }
    let pcm = render(
        &conversion.events,
        opl_bank,
        MUS_DIVISION,
        MUS_BPM,
        RenderOpts {
            sample_rate,
            master_gain: 1.0,
        },
    )
    .map_err(|err| MusicError::Render(err.to_string()))?;
    Ok(pcm.samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_second_mus() -> Vec<u8> {
        let score: &[u8] = &[
            0x40, 0x00, 0x00, // program 0
            0x90, 0x3C, // note on, last flag
            0x81, 0x0C, // delay 140 ticks (1 second)
            0x00, 0x3C, // note off
            0x60, // finish
        ];
        let song_len = score.len() as u16;
        let mut data = Vec::new();
        data.extend_from_slice(b"MUS\x1A");
        data.extend_from_slice(&song_len.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(score);
        data
    }

    #[test]
    fn new_engine_is_silent_and_stopped() {
        let engine = MusicEngine::new(48_000);
        assert!(!engine.is_playing());
        assert_eq!(engine.total_samples(), 0);
        assert_eq!(engine.duration_seconds(), 0.0);
        assert_eq!(engine.volume(), VOLUME_MAX);
        assert!(engine.looping());
    }

    #[test]
    fn volume_is_clamped() {
        let mut engine = MusicEngine::new(48_000);
        engine.set_volume(200);
        assert_eq!(engine.volume(), VOLUME_MAX);
        engine.set_volume(42);
        assert_eq!(engine.volume(), 42);
    }

    #[test]
    fn pull_samples_into_silent_engine_writes_zeros() {
        let mut engine = MusicEngine::new(48_000);
        let mut out = vec![1.0f32; 64];
        assert_eq!(engine.pull_samples(&mut out), 0);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stop_unloads_the_track() {
        let mut engine = MusicEngine::default();
        engine.stop();
        assert!(!engine.is_playing());
        assert_eq!(engine.total_samples(), 0);
    }

    #[test]
    fn bad_mus_data_is_an_error_not_a_panic() {
        let mut engine = MusicEngine::new(48_000);
        let err = engine
            .play_mus_fallback(b"this is not a MUS file")
            .unwrap_err();
        assert!(matches!(
            err,
            MusicError::Truncated { .. } | MusicError::BadMagic { .. }
        ));
        assert!(!engine.is_playing());
    }

    #[test]
    #[ignore = "renders audio through the OPL emulator; slow, run on demand"]
    fn play_mus_fallback_renders_audible_samples() {
        let mut engine = MusicEngine::new(48_000);
        engine.play_mus_fallback(&one_second_mus()).unwrap();
        assert!(engine.is_playing());
        assert!(engine.duration_seconds() > 0.5);

        let mut out = vec![0.0f32; 48_000];
        let written = engine.pull_samples(&mut out);
        assert_eq!(written, out.len());
        let peak = out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.001, "rendered track should not be silent");

        // pulling again past the end of a non-looping track stops playback
        engine.set_looping(false);
        let mut sink = vec![0.0f32; 10 * 48_000];
        engine.pull_samples(&mut sink);
        assert!(!engine.is_playing());
    }
}

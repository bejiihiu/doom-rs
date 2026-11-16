//! Music: the DMX MUS format and OPL FM synthesis.
//!
//! Vanilla DOOM hands music off to an external `sndserv` process; this
//! module brings the whole chain inside the engine instead:
//!
//! 1. [`mus`] parses MUS lumps — the compact, single-track MIDI dialect
//!    the id/DMX sound library used for its scores.
//! 2. [`genmidi`] parses the `GENMIDI` lump, the OPL2 instrument bank the
//!    DMX driver programmed the chip with. When the lump is absent, a
//!    small generic fallback bank is used so the music stays audible.
//! 3. [`midi`] converts MUS events to a MIDI-like timed event stream,
//!    applying the instrument-aware note remapping the DMX OPL driver did
//!    (fixed-pitch instruments, base note offsets, percussion channel).
//! 4. [`engine`] renders the stream through an OPL2 emulator and serves
//!    mono `f32` samples on demand.
//!
//! The module is backend-agnostic. [`MusicEngine`] owns the rendered PCM
//! and a playback cursor; the platform layer (or the sound mixer) pulls
//! samples through [`MusicEngine::pull_samples`] on its own clock, so the
//! music plays at the authentic DMX rate (140 ticks per second, i.e.
//! `division = 140`, `bpm = 60`) regardless of the output device's sample
//! rate. Nothing here is tied to cpal or to the game tic rate.
//!
//! ## Status and limitations
//!
//! - Double-voice GENMIDI instruments are rendered with their first voice
//!   only; [`opl_audio::OplBank`] cannot express two voices per note.
//! - Percussion is selected by drum note (35-81) on the percussion
//!   channel, matching GM conventions and the note-keyed drum patches of
//!   the OPL player; the DMX "play middle C" trick does not translate.
//! - MUS `\x0F` system events ("Event") are silently dropped, as DMX did.

mod engine;
mod error;
mod genmidi;
mod midi;
mod mus;

pub use engine::{DEFAULT_SAMPLE_RATE, MusicEngine, VOLUME_MAX};
pub use error::MusicError;
pub use genmidi::{
    DRUM_NOTE_MIN, FLAG_DOUBLE_VOICE, FLAG_FIXED_PITCH, FLAG_UNKNOWN, GENMIDI_MAGIC, GenmidiBank,
    GenmidiInstrument, GenmidiOperator, GenmidiVoice, NUM_ENTRIES, NUM_MELODIC, NUM_PERCUSSION,
};
pub use midi::{Conversion, DEFAULT_NOTE_VOLUME, MUS_BPM, MUS_DIVISION, TICK_SECONDS, mus_to_midi};
pub use mus::{
    MUS_MAGIC, MUS_TICKS_PER_SECOND, MusEvent, MusEventKind, MusSong, PERCUSSION_CHANNEL,
    SystemEvent, parse_mus,
};

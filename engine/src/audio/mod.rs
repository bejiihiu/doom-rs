//! Audio: sound effects (music lives elsewhere).
//!
//! Ported from `s_sound.c` (game-side sound model) and `i_sound.c`
//! (mixing), plus a cpal output backend and a WAD-lump decoder for the
//! DMX digitized sound format. Module map:
//!
//! - [`dmx`] - DMX sound lump decoder: format tag 3, 8-bit unsigned PCM at
//!   the rate stored in the header (almost always 11025 Hz), decoded to
//!   centered 16-bit samples. Both the officially padded layout and the
//!   naive unpadded one are accepted.
//! - [`mixer`] - the mixing core: a fixed set of mono slots mixed down to
//!   stereo, the vanilla `x^2` separation law, `0..=127` volume range,
//!   clamping sums, and rate-agnostic stepping with linear interpolation on
//!   the resample path (bit-exact at the native 11025 Hz rate).
//! - [`s_sound`] - the game-side model: channel allocation with priority
//!   eviction (`S_getChannel`), distance attenuation and stereo separation
//!   (`S_AdjustSoundParams`), per-tic pruning (`S_UpdateSounds`), singularity
//!   handling and the chainsaw restart set.
//! - [`bank`] - sound lump loading/caching, vanilla's pre-cache pass.
//! - [`backend`] - output backends: [`backend::CpalOutput`] (default
//!   device, real playback) and [`backend::NullOutput`] (in-memory, drives
//!   the mixer on demand; used by tests). Both share the same
//!   `fill_buffer` path.
//! - [`error`] - [`AudioError`], structured decode errors plus cpal/engine
//!   passthroughs.
//!
//! ## Architecture notes
//!
//! - No global state; everything hangs off [`SoundEngine`]. The backend
//!   and the engine share the mixer through [`SharedMixer`].
//! - The engine side is single-threaded and runs at game-tic rate; the
//!   audio callback thread only touches the mixer (short lock) and never
//!   calls back into game code. A fully decoupled audio thread (lock-free
//!   command queue) is a possible later evolution, not a requirement yet.
//! - The engine never panics on a poisoned mixer lock; it logs and
//!   continues (silence beats a deadlocked game).
//! - Doom logic interacts through plain data: [`SoundOrigin`] /
//!   [`Listener`] are built from the mobj the sound starts at (identity =
//!   `id`, e.g. thinker index<<32|generation packed by the caller).
//!
//! ```no_run
//! use doom_engine::audio::{AudioConfig, Listener, SoundEngine, SoundOrigin};
//! use doom_engine::info::SfxId;
//! use doom_engine::rng::DoomRng;
//! use doom_engine::fixed::Fixed;
//! use doom_engine::tables::Angle;
//!
//! let mut engine = SoundEngine::new(AudioConfig::default());
//! // engine.preload_sounds(&resources)?;
//! let mut rng = DoomRng::new();
//! let listener = Listener {
//!     x: Fixed::ZERO,
//!     y: Fixed::ZERO,
//!     angle: Angle::ZERO,
//!     origin_id: None,
//! };
//! let shooter = SoundOrigin::new(Fixed::ZERO, Fixed::ZERO, 1);
//! let _ = engine.start_sound(Some(shooter), SfxId::sfx_pistol, &mut rng, &listener);
//! ```

mod backend;
mod bank;
mod dmx;
mod error;
mod mixer;
mod s_sound;

pub use backend::{CpalOutput, NullOutput};
pub use bank::{PreloadReport, SoundBank};
pub use dmx::{
    DMX_FORMAT_TAG, DMX_HEADER_LEN, DMX_TYPICAL_SAMPLE_RATE, DmxSound, decode_dmx_sound,
};
pub use error::{AudioError, AudioResult};
pub use mixer::{
    MAX_PITCH, MAX_VOLUME, MixSlotInfo, Mixer, NORM_PITCH, NORM_SEP, pitch_to_ratio, sep_to_stereo,
};
pub use s_sound::{
    AdjustedParams, AudioConfig, ChannelInfo, DEFAULT_NUM_CHANNELS, Listener, RESTART_SFX,
    S_STEREO_SWING, SharedMixer, SoundEngine, SoundOrigin, StartOutcome, adjust_sound_params,
};

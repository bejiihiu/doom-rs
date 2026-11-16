//! The game-side sound model, ported from `s_sound.c` (linuxdoom-1.10).
//!
//! Ported mechanics (1:1 with the reference):
//!
//! - **Channel allocation** (`S_getChannel`): free channel > channel whose
//!   origin matches > first channel with `priority >= new priority` (vanilla
//!   comparator; note smaller number = more important in doom's table). If
//!   none qualifies the sound is dropped ("Sorry, Charlie").
//! - **Distance attenuation** (`S_AdjustSoundParams`): approximate
//!   euclidean distance (`adx + ady - min/2`), `S_CLIPPING_DIST` (1200 mu)
//!   hard cut, `S_CLOSE_DIST` (160 mu) full volume, linear falloff between;
//!   map 8 (the DM boss arena) clamps instead of cutting and floors the
//!   volume at 15.
//! - **Stereo separation**: `sep = 128 - (S_STEREO_SWING * finesine[angle])
//!   >> FRACBITS` with 13-bit fine angle from a `BAM32` angle; same-spot
//!   sources collapse to `NORM_SEP`.
//! - **Pitch perturbation**: see-sounds ±8, everything except itemup/tink
//!   ±16, clamped to `0..=255`, driven by the caller-provided [`DoomRng`]
//!   so demo replays stay in sync.
//! - **Stop semantics** (`S_StopSound` / `S_UpdateSounds`): stop the first
//!   channel from an origin; per-tic pruning of finished sounds and
//!   distance-based volume/sep recomputation.
//! - **Singularity**: sfx flagged `singularity` (item pickups, monster
//!   alert/see sounds) never runs twice at once - the new start is dropped
//!   while any channel plays the same id. 1.10 never read the flag; the
//!   behavior here matches the `sounds.h` contract "only one at a time".
//! - **Restart sounds** (`addsfx()` in `i_sound.c`, Linux): the
//!   chainsaw/stationary/pistol family resets the channel already playing
//!   the same id instead of stacking a second instance.
//!
//! Intentional, documented deviations from linuxdoom-1.10:
//!
//! - The 1.10 Linux port assigned `NORM_PRIORITY` to every non-linked
//!   sound, dead-ending the priority table and making the eviction rule
//!   pick channels by scan order. Here the S_sfx table drives priorities
//!   (linked variants use their own row's values, as 1.10 does), which is
//!   the rule the table was tuned for. The eviction comparator and order
//!   are unchanged.
//! - `usefulness` (zone-memory lump caching) is not ported; lumps live in
//!   the resource manager's typed cache instead.
//! - Music (`S_ChangeMusic` & friends) is not ported; see the music agent.
//! - `gamemap` is an explicit parameter ([`SoundEngine::set_map_number`])
//!   instead of a global; `players[consoleplayer].mo` is an opaque
//!   [`SoundOrigin`] + [`Listener::origin_id`].
//! - Config default `snd_channels` in the shipped doom.cfg is 3 and the
//!   mixer hardcoded 8; we unify both to [`DEFAULT_NUM_CHANNELS`] (8) and
//!   take the value from the caller's [`AudioConfig`].

use std::sync::{Arc, Mutex};

use crate::bsp::point_to_angle_2;
use crate::errors::EngineResult;
use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::info::{
    NORM_PITCH, S_ATTENUATOR, S_CLIPPING_DIST, S_CLOSE_DIST, S_MAX_VOLUME, SFX, SfxId,
};
use crate::math::aprox_distance;
use crate::rng::DoomRng;
use crate::tables::{Angle, FINE_SINE};

use super::bank::{PreloadReport, SoundBank};
use super::mixer::{MixSlotInfo, Mixer, NORM_SEP};

/// Vanilla `S_STEREO_SWING`: stereo swing amplitude (96 map-units
/// worth of separation at finesine peak).
pub const S_STEREO_SWING: i32 = 96 * FRACUNIT;
/// Default mixing channel count; doom.cfg shipped 3 but the mixer
/// hardcoded 8 - both are unified here (task decision).
pub const DEFAULT_NUM_CHANNELS: usize = 8;
/// Sfx ids whose playing channel is reset instead of duplicated when the
/// same id starts again (`i_sound.c addsfx()`).
pub const RESTART_SFX: [SfxId; 6] = [
    SfxId::sfx_sawup,
    SfxId::sfx_sawidl,
    SfxId::sfx_sawful,
    SfxId::sfx_sawhit,
    SfxId::sfx_stnmov,
    SfxId::sfx_pistol,
];

/// Shared handle so the output backend and the engine see the same mixer.
pub type SharedMixer = Arc<Mutex<Mixer>>;

/// Identifies a sound's origin: both the identity used for dedup
/// (`id`) and the position used for the vanilla same-spot `NORM_SEP`
/// special case and the distance computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundOrigin {
    /// Map x in 16.16 fixed point.
    pub x: Fixed,
    /// Map y in 16.16 fixed point.
    pub y: Fixed,
    /// Opaque caller identity (e.g. thinker index<<32 | generation).
    /// Two mobjs on the same spot keep their own ids, exactly like two
    /// distinct pointers would in vanilla.
    pub id: u64,
}

impl SoundOrigin {
    /// Creates an origin from map coordinates and a caller id.
    pub const fn new(x: Fixed, y: Fixed, id: u64) -> Self {
        Self { x, y, id }
    }
}

/// The listener (console player's camera) for distance/pan math.
#[derive(Clone, Copy, Debug)]
pub struct Listener {
    /// Camera x in 16.16 fixed point.
    pub x: Fixed,
    /// Camera y in 16.16 fixed point.
    pub y: Fixed,
    /// Camera facing direction (BAM32).
    pub angle: Angle,
    /// Identity of the listener's own origin, if any. Sounds started
    /// with this id are not distance-attenuated (vanilla compared against
    /// `players[consoleplayer].mo`).
    pub origin_id: Option<u64>,
}

impl Listener {
    /// A listener at the origin, facing east, no id.
    pub const ZERO: Listener = Listener {
        x: Fixed::ZERO,
        y: Fixed::ZERO,
        angle: Angle::ZERO,
        origin_id: None,
    };
}

/// Engine configuration, `S_Init` territory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioConfig {
    /// Mixing channel count; becomes both the logical channel count and
    /// the mixer slot count.
    pub num_channels: usize,
    /// `snd_SfxVolume` normalized to vanilla's `0..=127` scale.
    /// doom.cfg ships `8` on a `0..=15` menu scale - wire it in there.
    pub sfx_volume: i32,
    /// `gamemap`; map 8 is the DM boss arena in vanilla.
    pub map_number: i32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            num_channels: DEFAULT_NUM_CHANNELS,
            sfx_volume: S_MAX_VOLUME,
            map_number: 1,
        }
    }
}

/// What came out of a `start_sound*` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartOutcome {
    /// Sound is playing on the returned channel.
    Started {
        /// Logical channel index.
        channel: usize,
        /// Slot generation of this play (bump per play).
        generation: u64,
    },
    /// Distance-clipped: inaudible from the listener's position.
    Inaudible,
    /// The linked variant's volume went below 1 (S_StartSoundAtVolume
    /// early-out for linked sounds).
    VolumeZero,
    /// `singularity` sfx already playing; the new start was dropped.
    SuppressedSingularity,
    /// Every channel was busy at a higher-or-equal priority.
    NoChannelAvailable,
    /// The sfx data is not loaded; call [`SoundEngine::preload_sounds`]
    /// (or [`SoundEngine::sounds_mut`] to inspect the bank) first.
    MissingSound,
}

/// Snapshot of one logical channel; for diagnostics and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Logical channel index.
    pub channel: usize,
    /// Playing sfx.
    pub sfx_id: SfxId,
    /// Origin the sound started from, if any.
    pub origin: Option<SoundOrigin>,
    /// Channel priority (table value).
    pub priority: i32,
    /// Last applied base volume.
    pub volume: i32,
    /// Last applied separation.
    pub sep: i32,
    /// Last applied pitch.
    pub pitch: i32,
    /// Slot generation of the current play.
    pub generation: u64,
}

/// Result of [`adjust_sound_params`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdjustedParams {
    /// Distance-attenuated volume (`0..=127`).
    pub volume: i32,
    /// Stereo separation (`0..=255`, 128 = center).
    pub sep: i32,
    /// False when the sound is inaudible from this listener position.
    pub audible: bool,
}

/// Vanilla `S_AdjustSoundParams`.
///
/// `max_volume` is the global `snd_SfxVolume`; `map_number` is `gamemap`
/// (map 8 trades the hard cut for a floor of 15).
pub fn adjust_sound_params(
    listener: &Listener,
    source: &SoundOrigin,
    max_volume: i32,
    map_number: i32,
) -> AdjustedParams {
    // clamp at i32::MAX: anything past S_CLIPPING_DIST means inaudible
    // anyway, so the exact overflow value never matters
    let clamp_fix = |raw: i64| Fixed::from_raw(raw.unsigned_abs().min(i32::MAX as u64) as i32);
    let adx = clamp_fix(listener.x.to_raw() as i64 - source.x.to_raw() as i64);
    let ady = clamp_fix(listener.y.to_raw() as i64 - source.y.to_raw() as i64);
    // vanilla approx_dist: adx + ady - min/2 on fixed point
    let approx_dist = aprox_distance(adx, ady);

    if map_number != 8 && approx_dist > Fixed::from_raw(S_CLIPPING_DIST) {
        return AdjustedParams {
            volume: 0,
            sep: NORM_SEP,
            audible: false,
        };
    }

    // angle of source to listener, folded to the listener's facing
    let angle = point_to_angle_2(listener.x, listener.y, source.x, source.y);
    // wrapping_sub == vanilla's if/else (the else branch lost one BAM
    // unit, irrelevant at fine-angle granularity)
    let delta = Angle::from_raw(angle.to_raw().wrapping_sub(listener.angle.to_raw()));
    let fine = delta.fine_angle();

    // stereo separation
    let sin = FINE_SINE[fine];
    let swing = (i64::from(S_STEREO_SWING) * i64::from(sin.to_raw())) >> FRACBITS;
    let sep = NORM_SEP - (swing >> FRACBITS) as i32;

    // volume falloff
    let volume = if approx_dist < Fixed::from_raw(S_CLOSE_DIST) {
        max_volume
    } else if map_number == 8 {
        let dist = approx_dist.min(Fixed::from_raw(S_CLIPPING_DIST));
        15 + ((max_volume - 15) * ((S_CLIPPING_DIST - dist.to_raw()) >> FRACBITS)) / S_ATTENUATOR
    } else {
        (max_volume * ((S_CLIPPING_DIST - approx_dist.to_raw()) >> FRACBITS)) / S_ATTENUATOR
    };

    AdjustedParams {
        volume,
        sep,
        audible: volume > 0,
    }
}

/// Channel state of one logical channel (vanilla `channel_t`).
#[derive(Clone, Copy)]
struct ActiveSfx {
    sfx_id: SfxId,
    origin: Option<SoundOrigin>,
    priority: i32,
    volume: i32,
    sep: i32,
    pitch: i32,
    generation: u64,
}

/// The sound engine: owns the logical channels, the mixer and the bank.
///
/// No global state; the output backend shares the mixer through
/// [`SoundEngine::mixer`].
pub struct SoundEngine {
    channels: Vec<Option<ActiveSfx>>,
    mixer: SharedMixer,
    sounds: SoundBank,
    sfx_volume: i32,
    map_number: i32,
}

impl std::fmt::Debug for SoundEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundEngine")
            .field(
                "channels",
                &self.channels.iter().filter(|c| c.is_some()).count(),
            )
            .field("sfx_volume", &self.sfx_volume)
            .field("map_number", &self.map_number)
            .finish()
    }
}

impl SoundEngine {
    /// Creates the engine; `mixer` channels and logical channels share
    /// the same count (see [`AudioConfig`]).
    pub fn new(config: AudioConfig) -> Self {
        let num_channels = config.num_channels.max(1);
        Self {
            channels: vec![None; num_channels],
            mixer: Arc::new(Mutex::new(Mixer::new(num_channels))),
            sounds: SoundBank::new(),
            sfx_volume: config.sfx_volume.clamp(0, S_MAX_VOLUME),
            map_number: config.map_number,
        }
    }

    /// Shared mixer handle for the output backend.
    pub fn mixer(&self) -> SharedMixer {
        Arc::clone(&self.mixer)
    }

    /// Preloads all non-linked sfx entries (vanilla `I_InitSound`
    /// pre-cache pass). See [`SoundBank::preload`].
    pub fn preload_sounds(
        &mut self,
        resources: &crate::resources::ResourceManager,
    ) -> EngineResult<PreloadReport> {
        self.sounds.preload(resources)
    }

    /// Access to the sound bank (e.g. for one-off lazy loads).
    pub fn sounds(&self) -> &SoundBank {
        &self.sounds
    }

    /// Mutable access to the sound bank.
    pub fn sounds_mut(&mut self) -> &mut SoundBank {
        &mut self.sounds
    }

    /// Sets `snd_SfxVolume`, normalized to `0..=127`.
    ///
    /// # Errors
    ///
    /// [`super::error::AudioError::InvalidVolume`] when out of range
    /// (vanilla `I_Error`'d; we return an error instead).
    pub fn set_sfx_volume(&mut self, volume: i32) -> Result<(), super::error::AudioError> {
        if !(0..=S_MAX_VOLUME).contains(&volume) {
            return Err(super::error::AudioError::InvalidVolume(volume));
        }
        self.sfx_volume = volume;
        Ok(())
    }

    /// Current `snd_SfxVolume`.
    pub fn sfx_volume(&self) -> i32 {
        self.sfx_volume
    }

    /// Sets `gamemap` (map 8 gets the floor-of-15 treatment).
    pub fn set_map_number(&mut self, map_number: i32) {
        self.map_number = map_number;
    }

    /// Current `gamemap`.
    pub fn map_number(&self) -> i32 {
        self.map_number
    }

    /// Number of logical channels.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// Number of busy channels.
    pub fn active_channels(&self) -> usize {
        self.channels.iter().filter(|c| c.is_some()).count()
    }

    /// Snapshot of a channel, for tests and diagnostics.
    pub fn channel_info(&self, channel: usize) -> Option<ChannelInfo> {
        let active = self.channels.get(channel)?.as_ref()?;
        Some(ChannelInfo {
            channel,
            sfx_id: active.sfx_id,
            origin: active.origin,
            priority: active.priority,
            volume: active.volume,
            sep: active.sep,
            pitch: active.pitch,
            generation: active.generation,
        })
    }

    /// Mixer slot snapshot (positions, per-ear volumes), for diagnostics.
    pub fn mix_slot_info(&self, channel: usize) -> Option<MixSlotInfo> {
        self.mixer.lock().ok()?.slot_info(channel)
    }

    /// Vanilla `S_StartSound` (starts at the configured sfx volume).
    pub fn start_sound(
        &mut self,
        origin: Option<SoundOrigin>,
        sfx_id: SfxId,
        rng: &mut DoomRng,
        listener: &Listener,
    ) -> StartOutcome {
        self.start_sound_at_volume(origin, sfx_id, self.sfx_volume, rng, listener)
    }

    /// Vanilla `S_StartSoundAtVolume`.
    ///
    /// Like vanilla, `volume` is the base for the distance scaling but the
    /// attenuation formula re-derives the final value from `snd_SfxVolume`,
    /// so an explicit `volume` only survives for local/menu origins (the
    /// same behavior as the original).
    pub fn start_sound_at_volume(
        &mut self,
        origin: Option<SoundOrigin>,
        sfx_id: SfxId,
        mut volume: i32,
        rng: &mut DoomRng,
        listener: &Listener,
    ) -> StartOutcome {
        let sfx = SFX[sfx_id as usize];

        // initialize sound parameters (link handling)
        let mut pitch = NORM_PITCH;
        let priority = sfx.priority;
        if sfx.link.is_some() {
            pitch = sfx.pitch;
            // sfx->priority for linked sounds is their own row value
            // (chgun is 64 either way)
            volume += sfx.volume;
            if volume < 1 {
                return StartOutcome::VolumeZero;
            }
            if volume > self.sfx_volume {
                volume = self.sfx_volume;
            }
        }

        // check whether it is audible, and if not, modify the params
        let mut sep = NORM_SEP;
        let is_local = match (origin, listener.origin_id) {
            (Some(source), Some(id)) => source.id == id,
            (None, _) => true, // menu/global sound: centered, never attenuated
            (Some(_), None) => false,
        };
        if let Some(source) = origin
            && !is_local
        {
            let adjusted = adjust_sound_params(listener, &source, self.sfx_volume, self.map_number);
            if !adjusted.audible {
                return StartOutcome::Inaudible;
            }
            volume = adjusted.volume;
            sep = adjusted.sep;
        }
        if let Some(source) = origin {
            // same-spot special case, even after adjustment
            if listener.x == source.x && listener.y == source.y {
                sep = NORM_SEP;
            }
        }

        // singularity: refuse to stack the same singular sfx; costs no rng
        if sfx.singularity
            && self
                .channels
                .iter()
                .any(|c| c.as_ref().is_some_and(|a| a.sfx_id == sfx_id))
        {
            return StartOutcome::SuppressedSingularity;
        }

        // hacks to vary the sfx pitches
        if (SfxId::sfx_sawup as usize..=SfxId::sfx_sawhit as usize).contains(&(sfx_id as usize)) {
            pitch += 8 - i32::from(rng.next() & 15);
            pitch = pitch.clamp(0, 255);
        } else if sfx_id != SfxId::sfx_itemup && sfx_id != SfxId::sfx_tink {
            pitch += 16 - i32::from(rng.next() & 31);
            pitch = pitch.clamp(0, 255);
        }

        // kill old sound from this origin (vanilla `S_StopSound(origin)`)
        self.stop_sound(origin);

        // restart sounds reset the channel already playing the same id
        let restart_slot = RESTART_SFX
            .contains(&sfx_id)
            .then(|| {
                self.channels
                    .iter()
                    .position(|c| c.as_ref().is_some_and(|a| a.sfx_id == sfx_id))
            })
            .flatten();

        // try to find a channel
        let cnum = match restart_slot {
            Some(channel) => {
                self.stop_channel(channel);
                channel
            }
            None => match self.get_channel(origin, priority) {
                Some(channel) => channel,
                None => return StartOutcome::NoChannelAvailable,
            },
        };

        let sound = match self.sounds.get(sfx_id) {
            Some(sound) => sound,
            None => {
                tracing::warn!("audio: sound {} not loaded, cannot start", sfx_id.name());
                return StartOutcome::MissingSound;
            }
        };

        // note: sounds are preloaded; a missing entry means the caller
        // never ran preload - report it instead of silently dropping
        let generation = {
            let mut mixer = match self.mixer.lock() {
                Ok(mixer) => mixer,
                Err(poisoned) => poisoned.into_inner(),
            };
            match mixer.start_on(cnum, sound, volume, sep, pitch) {
                Ok(generation) => generation,
                Err(err) => {
                    tracing::error!(
                        "audio: failed to start sound {} on channel {}: {err}",
                        sfx_id.name(),
                        cnum
                    );
                    return StartOutcome::MissingSound;
                }
            }
        };

        self.channels[cnum] = Some(ActiveSfx {
            sfx_id,
            origin,
            priority,
            volume,
            sep,
            pitch,
            generation,
        });

        StartOutcome::Started {
            channel: cnum,
            generation,
        }
    }

    /// Vanilla `S_StopSound`: stops the first channel from an origin.
    pub fn stop_sound(&mut self, origin: Option<SoundOrigin>) {
        for cnum in 0..self.channels.len() {
            if self.channels[cnum]
                .as_ref()
                .is_some_and(|c| match (c.origin, origin) {
                    (Some(a), Some(b)) => a.id == b.id,
                    (None, None) => true,
                    _ => false,
                })
            {
                self.stop_channel(cnum);
                break;
            }
        }
    }

    /// Stops every playing channel (per-level sound reset; vanilla
    /// `S_Start` kills all sounds at level start).
    pub fn stop_all_sounds(&mut self) {
        for cnum in 0..self.channels.len() {
            self.stop_channel(cnum);
        }
    }

    /// Vanilla `S_UpdateSounds`: called once per game tic.
    ///
    /// Frees channels whose sound ended, re-applies distance attenuation
    /// to non-local origins and stops sounds that became inaudible.
    pub fn update_sounds(&mut self, listener: &Listener) {
        // prune finished sounds first, so the param pass below sees a
        // consistent picture
        let finished = {
            let mut mixer = match self.mixer.lock() {
                Ok(mixer) => mixer,
                Err(poisoned) => poisoned.into_inner(),
            };
            mixer.take_finished()
        };
        for (slot, generation) in finished {
            if self.channels[slot]
                .as_ref()
                .is_some_and(|c| c.generation == generation)
            {
                self.channels[slot] = None;
            }
        }

        for cnum in 0..self.channels.len() {
            let Some(active) = self.channels[cnum] else {
                continue;
            };
            let sfx = SFX[active.sfx_id as usize];

            if sfx.link.is_some() {
                // linked variant whose volume underflowed is stopped
                let volume = self.sfx_volume + sfx.volume;
                if volume < 1 {
                    self.stop_channel(cnum);
                    continue;
                }
            }

            let Some(origin) = active.origin else {
                continue; // local/menu sound: params stay as started
            };
            if listener.origin_id == Some(origin.id) {
                continue; // listener's own sound: params stay as started
            }

            let adjusted = adjust_sound_params(listener, &origin, self.sfx_volume, self.map_number);
            if !adjusted.audible {
                self.stop_channel(cnum);
                continue;
            }

            if let Ok(mut mixer) = self.mixer.lock() {
                mixer.set_params(cnum, adjusted.volume, adjusted.sep);
            }
            if let Some(active) = &mut self.channels[cnum] {
                active.volume = adjusted.volume;
                active.sep = adjusted.sep;
            }
        }
    }

    /// Vanilla `S_getChannel`: free slot, same-origin slot, eviction.
    fn get_channel(&mut self, origin: Option<SoundOrigin>, priority: i32) -> Option<usize> {
        // find an open channel (or a same-origin one to reuse)
        for cnum in 0..self.channels.len() {
            match &self.channels[cnum] {
                None => return Some(cnum),
                Some(active) => {
                    if let (Some(want), Some(have)) = (origin, active.origin) {
                        if want.id == have.id {
                            self.stop_channel(cnum);
                            return Some(cnum);
                        }
                    }
                }
            }
        }

        // none available: kick the first channel at lower-or-equal rank
        for cnum in 0..self.channels.len() {
            if self.channels[cnum]
                .as_ref()
                .is_some_and(|c| c.priority >= priority)
            {
                self.stop_channel(cnum);
                return Some(cnum);
            }
        }

        // sorry, Charlie
        None
    }

    /// Vanilla `S_StopChannel`.
    fn stop_channel(&mut self, cnum: usize) {
        if self.channels[cnum].is_none() {
            return;
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.stop(cnum);
        }
        self.channels[cnum] = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::ResourceManager;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    /// A DMX lump with `count` copies of `fill` (unpadded layout).
    fn dmx_bytes(count: usize, fill: u8) -> Vec<u8> {
        let mut data = Vec::with_capacity(8 + count);
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&11025u16.to_le_bytes());
        data.extend_from_slice(&(count as u32).to_le_bytes());
        data.extend_from_slice(&vec![fill; count]);
        data
    }

    /// Builds a manager whose WAD carries a single `dspistol` lump; everything
    /// else falls back to it during preload.
    fn manager_with_pistol(pistol_len: usize) -> ResourceManager {
        let wad = WadFile::from_bytes(build_wad(
            b"IWAD",
            &[("DSPISTOL", &dmx_bytes(pistol_len, 128))],
        ))
        .unwrap();
        ResourceManager::new(wad, Vec::new())
    }

    fn test_engine(map_number: i32, sfx_volume: i32) -> SoundEngine {
        let mut engine = SoundEngine::new(AudioConfig {
            num_channels: DEFAULT_NUM_CHANNELS,
            sfx_volume,
            map_number,
        });
        let report = engine.preload_sounds(&manager_with_pistol(64)).unwrap();
        assert_eq!(report.loaded, SFX_COUNT - 2);
        engine
    }

    use crate::info::SFX_COUNT;

    fn origin(id: u64, x: i32, y: i32) -> SoundOrigin {
        SoundOrigin::new(Fixed::from_int(x), Fixed::from_int(y), id)
    }

    fn listener() -> Listener {
        Listener {
            x: Fixed::ZERO,
            y: Fixed::ZERO,
            angle: Angle::ZERO,
            origin_id: Some(9999),
        }
    }

    #[test]
    fn config_defaults_match_task() {
        let cfg = AudioConfig::default();
        assert_eq!(cfg.num_channels, 8);
        assert_eq!(cfg.sfx_volume, S_MAX_VOLUME);
        assert_eq!(cfg.map_number, 1);
    }

    #[test]
    fn start_sound_plays_and_reports_channel() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        let outcome = engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        assert!(matches!(outcome, StartOutcome::Started { channel, .. } if channel < 8));
        assert_eq!(engine.active_channels(), 1);
    }

    #[test]
    fn same_origin_restart_replaces_rather_than_stacks() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        let first = engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let second = engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_bfg,
            &mut rng,
            &listener(),
        );
        let first_ch = match first {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("first start failed: {other:?}"),
        };
        // bfg (table priority 64) must land on the same channel the pistol freed
        assert!(matches!(second, StartOutcome::Started { channel, .. } if channel == first_ch));
        assert_eq!(engine.active_channels(), 1);
    }

    #[test]
    fn channel_eviction_prefers_first_low_priority_channel() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        // fill all channels with bfg (priority 64) from distinct origins
        // (pistol is in the chainsaw restart set, it would collapse to one)
        for id in 0..8 {
            let outcome = engine.start_sound(
                Some(origin(id, 100 + id as i32, 0)),
                SfxId::sfx_bfg,
                &mut rng,
                &listener(),
            );
            assert!(matches!(outcome, StartOutcome::Started { .. }));
        }
        // telept has table priority 32; every channel has >= 32, so channel 0 gets kicked
        let outcome = engine.start_sound(
            Some(origin(100, 500, 500)),
            SfxId::sfx_telept,
            &mut rng,
            &listener(),
        );
        assert!(matches!(outcome, StartOutcome::Started { channel: 0, .. }));
    }

    #[test]
    fn channel_eviction_reports_when_nothing_qualifies() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        // fill all channels with plpain (priority 96, not singular), distinct origins
        for id in 0..8 {
            let outcome = engine.start_sound(
                Some(origin(id, 100 + id as i32, 0)),
                SfxId::sfx_plpain,
                &mut rng,
                &listener(),
            );
            assert!(matches!(outcome, StartOutcome::Started { .. }));
        }
        // telept (32) can still kick one (all channels >= 32)
        let outcome = engine.start_sound(
            Some(origin(100, 500, 500)),
            SfxId::sfx_telept,
            &mut rng,
            &listener(),
        );
        assert!(matches!(outcome, StartOutcome::Started { channel: 0, .. }));
        // but a *new* posact (120) would not fit now that channel 0 holds telept (32)...
        // (channels 1..=7 hold 120) — refill everything with telept first
        let mut engine2 = test_engine(1, 127);
        for id in 0..8 {
            let outcome = engine2.start_sound(
                Some(origin(id, 100 + id as i32, 0)),
                SfxId::sfx_telept,
                &mut rng,
                &listener(),
            );
            assert!(matches!(outcome, StartOutcome::Started { .. }));
        }
        // telept (32) everywhere means nothing qualifies for posact (120): drop it
        let outcome = engine2.start_sound(
            Some(origin(100, 500, 500)),
            SfxId::sfx_posact,
            &mut rng,
            &listener(),
        );
        assert_eq!(outcome, StartOutcome::NoChannelAvailable);
    }

    #[test]
    fn restart_sfx_resets_same_id_channel() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        engine.start_sound(
            Some(origin(1, 50, 50)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        engine.start_sound(
            Some(origin(2, -60, 40)),
            SfxId::sfx_sawup,
            &mut rng,
            &listener(),
        );
        // pistol restarts the channel playing pistol even though sawup is free-standing
        let before = engine.active_channels();
        engine.start_sound(
            Some(origin(3, 500, 500)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        assert_eq!(
            engine.active_channels(),
            before,
            "pistol restart must not add a channel"
        );
        let pistol_count = (0..engine.num_channels())
            .filter(|&c| {
                engine
                    .channel_info(c)
                    .map(|i| i.sfx_id == SfxId::sfx_pistol)
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(pistol_count, 1);
    }

    #[test]
    fn singularity_sfx_does_not_stack() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        let first = engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_itemup,
            &mut rng,
            &listener(),
        );
        let second = engine.start_sound(
            Some(origin(2, 20, 20)),
            SfxId::sfx_itemup,
            &mut rng,
            &listener(),
        );
        assert!(matches!(first, StartOutcome::Started { .. }));
        assert_eq!(second, StartOutcome::SuppressedSingularity);
        assert_eq!(engine.active_channels(), 1);
    }

    #[test]
    fn distant_origin_is_inaudible() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        // S_CLIPPING_DIST = 1200 mu; put the source at 1300
        let outcome = engine.start_sound(
            Some(origin(1, 1300, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        assert_eq!(outcome, StartOutcome::Inaudible);

        // on map 8 the same shot is quiet but audible
        let mut engine8 = test_engine(8, 127);
        let outcome = engine8.start_sound(
            Some(origin(1, 1300, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        assert!(matches!(outcome, StartOutcome::Started { .. }));
    }

    #[test]
    fn close_origin_plays_at_full_volume() {
        let mut engine = test_engine(1, 100);
        let mut rng = DoomRng::new();
        let outcome = engine.start_sound(
            Some(origin(1, 50, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let channel = match outcome {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("expected start, got {other:?}"),
        };
        assert_eq!(engine.channel_info(channel).unwrap().volume, 100);
    }

    #[test]
    fn volume_falls_off_linearly_with_distance() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        // 600 mu away: halfway between close (160) and clip (1200)
        let outcome = engine.start_sound(
            Some(origin(1, 600, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let channel = match outcome {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("expected start, got {other:?}"),
        };
        // (1200-600)/(1200-160) * 127 = ~77
        let volume = engine.channel_info(channel).unwrap().volume;
        assert_eq!(volume, (127 * (1200 - 600)) / 1040);
    }

    #[test]
    fn straight_ahead_source_is_centered() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        // listener faces angle 0 (east); source due east => delta 0 => sep 128
        let outcome = engine.start_sound(
            Some(origin(1, 100, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let channel = match outcome {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("expected start, got {other:?}"),
        };
        assert_eq!(engine.channel_info(channel).unwrap().sep, NORM_SEP);
    }

    #[test]
    fn flank_sources_pan_to_their_side() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        // source due "north" (angle +90 deg) while listener faces east => delta +90deg
        let north = engine.start_sound(
            Some(origin(1, 0, 100)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let south = engine.start_sound(
            Some(origin(2, 0, -100)),
            SfxId::sfx_bfg,
            &mut rng,
            &listener(),
        );
        let north_ch = match north {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("{other:?}"),
        };
        let south_ch = match south {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("{other:?}"),
        };
        let north_sep = engine.channel_info(north_ch).unwrap().sep;
        let south_sep = engine.channel_info(south_ch).unwrap().sep;
        assert_ne!(north_sep, NORM_SEP);
        assert_ne!(south_sep, NORM_SEP);
        assert_ne!(north_sep, south_sep);
    }

    #[test]
    fn same_spot_source_is_centered_even_off_axis() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        let mut lst = listener();
        lst.x = Fixed::from_int(42);
        lst.y = Fixed::from_int(42);
        lst.angle = Angle::from_raw(0x1234_5678);
        let outcome =
            engine.start_sound(Some(origin(7, 42, 42)), SfxId::sfx_pistol, &mut rng, &lst);
        let channel = match outcome {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("{other:?}"),
        };
        assert_eq!(engine.channel_info(channel).unwrap().sep, NORM_SEP);
    }

    #[test]
    fn chgun_plays_linked_data_with_perturbed_pitch() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        let outcome = engine.start_sound(
            Some(origin(1, 50, 0)),
            SfxId::sfx_chgun,
            &mut rng,
            &listener(),
        );
        let channel = match outcome {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("chgun should start, got {other:?}"),
        };
        let info = engine.channel_info(channel).unwrap();
        // 150 ± (16 - rng&31) => [135, 166]; verify it left the linked base
        assert!(
            (135..=166).contains(&info.pitch),
            "linked pitch out of range: {}",
            info.pitch
        );
        assert_eq!(info.volume, 127);
    }

    #[test]
    fn saw_range_perturbs_within_tighter_bounds() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        let outcome = engine.start_sound(
            Some(origin(1, 50, 0)),
            SfxId::sfx_sawhit,
            &mut rng,
            &listener(),
        );
        let channel = match outcome {
            StartOutcome::Started { channel, .. } => channel,
            other => panic!("{other:?}"),
        };
        let info = engine.channel_info(channel).unwrap();
        // NORM_PITCH(128) + 8 - (rng & 15) => [121, 136]
        assert!(
            (121..=136).contains(&info.pitch),
            "saw pitch out of range: {}",
            info.pitch
        );
    }

    #[test]
    fn stop_sound_stops_first_matching_origin() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_bfg,
            &mut rng,
            &listener(),
        );
        // same origin again: only one channel carries it anyway
        assert_eq!(engine.active_channels(), 1);
        engine.stop_sound(Some(origin(1, 10, 10)));
        assert_eq!(engine.active_channels(), 0);
    }

    #[test]
    fn stop_all_sounds_clears_everything() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        for id in 0..4 {
            engine.start_sound(
                Some(origin(id, id as i32 * 50, 0)),
                SfxId::sfx_bfg,
                &mut rng,
                &listener(),
            );
        }
        assert_eq!(engine.active_channels(), 4);
        engine.stop_all_sounds();
        assert_eq!(engine.active_channels(), 0);
    }

    #[test]
    fn finished_sound_is_pruned_by_update_sounds() {
        let wad =
            WadFile::from_bytes(build_wad(b"IWAD", &[("DSPISTOL", &dmx_bytes(4, 128))])).unwrap();
        let resources = ResourceManager::new(wad, Vec::new());
        let mut engine = SoundEngine::new(AudioConfig {
            num_channels: 8,
            sfx_volume: 127,
            map_number: 1,
        });
        engine.preload_sounds(&resources).unwrap();
        let mut rng = DoomRng::new();
        engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        assert_eq!(engine.active_channels(), 1);
        // run enough samples through the mixer to finish the 4-sample sound
        {
            let mixer = engine.mixer();
            let mut mixer = mixer.lock().unwrap();
            let mut dst = [0i16; 16];
            mixer.mix_into(&mut dst, 11025);
        }
        engine.update_sounds(&listener());
        assert_eq!(engine.active_channels(), 0);
    }

    #[test]
    fn listener_moving_away_stops_distant_sound() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        engine.start_sound(
            Some(origin(1, 100, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        assert_eq!(engine.active_channels(), 1);
        // teleport the listener to the far end of the map
        let mut far = listener();
        far.x = Fixed::from_int(1500);
        engine.update_sounds(&far);
        assert_eq!(engine.active_channels(), 0);
    }

    #[test]
    fn volume_updates_as_listener_approaches() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        engine.start_sound(
            Some(origin(1, 600, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let before = engine.channel_info(0).unwrap().volume;
        // listener steps right next to the source (still a non-local origin)
        let mut near = listener();
        near.x = Fixed::from_int(550);
        engine.update_sounds(&near);
        let after = engine.channel_info(0).unwrap().volume;
        assert!(
            after > before,
            "closer listener should mean louder sound ({after} <= {before})"
        );
    }

    #[test]
    fn set_sfx_volume_validates_range() {
        let mut engine = test_engine(1, 127);
        assert!(engine.set_sfx_volume(0).is_ok());
        assert!(engine.set_sfx_volume(127).is_ok());
        assert!(engine.set_sfx_volume(128).is_err());
        assert!(engine.set_sfx_volume(-1).is_err());
    }

    #[test]
    fn set_params_keeps_mixer_in_sync_with_update_sounds() {
        let mut engine = test_engine(1, 127);
        let mut rng = DoomRng::new();
        engine.start_sound(
            Some(origin(1, 600, 0)),
            SfxId::sfx_pistol,
            &mut rng,
            &listener(),
        );
        let far_vol = engine.mix_slot_info(0).unwrap();
        let mut near = listener();
        near.x = Fixed::from_int(550);
        engine.update_sounds(&near);
        let near_vol = engine.mix_slot_info(0).unwrap();
        // one ear or the other must have gained volume
        assert!(
            near_vol.left_vol + near_vol.right_vol > far_vol.left_vol + far_vol.right_vol,
            "update_sounds must re-drive mixer volumes"
        );
    }

    #[test]
    fn map8_keeps_sound_audible_at_clip_distance() {
        // regression for the gamemap == 8 rule: no hard cut, floor of 15
        let params = adjust_sound_params(
            &Listener::ZERO,
            &SoundOrigin::new(Fixed::from_int(1500), Fixed::ZERO, 1),
            100,
            8,
        );
        assert!(params.audible);
        assert!(params.volume >= 1);
    }

    #[test]
    fn volume_zero_link_is_reported() {
        let mut engine = test_engine(1, 0); // snd_SfxVolume 0
        let mut rng = DoomRng::new();
        // chgun's linked volume is 0 => 0 + 0 = 0 => VolumeZero early-out
        let outcome = engine.start_sound(
            Some(origin(1, 10, 10)),
            SfxId::sfx_chgun,
            &mut rng,
            &listener(),
        );
        assert_eq!(outcome, StartOutcome::VolumeZero);
    }
}

//! The mixing core, ported from `i_sound.c` (`I_UpdateSound` and the
//! `addsfx()` volume math) with two deliberate, documented upgrades:
//!
//! - **Arbitrary output rates.** Vanilla opened `/dev/dsp` at 11025 Hz and
//!   the step accumulator only handled pitch. Here the step also folds in
//!   the source rate vs. the device rate, so a 11025 Hz DMX lump plays at
//!   the device's native rate with linear interpolation on the resample
//!   path. At `output_rate == spec_rate` the fraction is always 0 and the
//!   mix is bit-identical to the vanilla stepping path.
//! - **Modern output backends.** Instead of writing `/dev/dsp`, samples go
//!   into a caller-provided interleaved stereo buffer; the backend (see
//!   [`super::backend`]) owns the device.
//!
//! Everything else is vanilla: the `x^2` separation law, the `0..=127`
//! volume range, summing with clamping to `±32768`, no interpolation at
//! same-rate playback, and the `2^((pitch-128)/64)` pitch law from
//! `steptable` (which turns out to have been load-bearing on Linux after
//! all - `addsfx` installed it as `channelstep`).
//!
//! Sample positions are tracked in `f64` source-sample units; integer
//! stepping at fractional rates would drift, floats do not.

use std::sync::Arc;

use super::dmx::DmxSound;
use super::error::{AudioError, AudioResult};

/// Hard cap on pitch values (`u8` in vanilla, clamped the same way here).
pub const MAX_PITCH: i32 = 255;
/// Vanilla `NORM_PITCH`: unity pitch.
pub const NORM_PITCH: i32 = 128;
/// Vanilla `NORM_SEP`: center stereo separation.
pub const NORM_SEP: i32 = 128;
/// Maximum per-channel volume (vanilla `S_MAX_VOLUME`).
pub const MAX_VOLUME: i32 = 127;

/// Pitch byte -> playback rate multiplier, the vanilla `steptable` law:
/// `2^((pitch - 128) / 64)`. Pitch 128 is 1.0, 150 is ~1.27, 0 is 0.25.
pub fn pitch_to_ratio(pitch: i32) -> f64 {
    let pitch = pitch.clamp(0, MAX_PITCH);
    2f64.powf(f64::from(pitch - NORM_PITCH) / 64.0)
}

/// Splits `volume`/`sep` into per-ear volumes using the vanilla `x^2`
/// separation law from `addsfx()`:
///
/// ```text
/// sep += 1
/// left  = volume - ((volume * sep * sep) >> 16)
/// sep  -= 257
/// right = volume - ((volume * sep * sep) >> 16)
/// ```
///
/// `sep = 0` is hard left, `sep = 255` hard right, `sep = 128` the
/// (slightly asymmetric, as shipped) center.
pub fn sep_to_stereo(volume: i32, sep: i32) -> (i32, i32) {
    let volume = volume.clamp(0, MAX_VOLUME);
    let mut s = sep + 1;
    let left = volume - ((volume * s * s) >> 16);
    s -= 257;
    let right = volume - ((volume * s * s) >> 16);
    (left.clamp(0, MAX_VOLUME), right.clamp(0, MAX_VOLUME))
}

/// One mixing slot; mirrors vanilla's `NUM_CHANNELS` channel state.
#[derive(Clone)]
struct MixSlot {
    /// Active sound, if any.
    sound: Option<Arc<DmxSound>>,
    /// Source-sample position with fractional part.
    position: f64,
    /// `2^((pitch-128)/64)`.
    pitch_ratio: f64,
    /// Left ear volume 0..=127.
    left_vol: i32,
    /// Right ear volume 0..=127.
    right_vol: i32,
    /// Pitch byte for diagnostics.
    pitch: i32,
    /// Bumped on every `start_on`; lets callers detect slot reuse.
    generation: u64,
}
impl MixSlot {
    fn empty() -> Self {
        Self {
            sound: None,
            position: 0.0,
            pitch_ratio: 1.0,
            left_vol: 0,
            right_vol: 0,
            pitch: NORM_PITCH,
            generation: 0,
        }
    }

    fn is_playing(&self) -> bool {
        self.sound.is_some()
    }
}

/// Snapshot of a slot for tests and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixSlotInfo {
    /// Slot generation; changes every time a sound starts on the slot.
    pub generation: u64,
    /// Pitch byte (0..=255).
    pub pitch: i32,
    /// Left ear volume 0..=127.
    pub left_vol: i32,
    /// Right ear volume 0..=127.
    pub right_vol: i32,
    /// Position in source samples.
    pub position: f64,
    /// Number of source samples in the active sound.
    pub sound_len: usize,
}

/// The mixing core: a fixed set of mono slots mixed down to stereo.
///
/// Rate-agnostic: the caller passes the output rate on every
/// [`Mixer::mix_into`] call, so the same mixer serves tests at 11025 Hz
/// and devices at 48000 Hz.
pub struct Mixer {
    slots: Vec<MixSlot>,
    generation: u64,
    finished: Vec<(usize, u64)>,
}

impl std::fmt::Debug for Mixer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mixer")
            .field("slots", &self.slots.len())
            .field(
                "active",
                &self.slots.iter().filter(|s| s.is_playing()).count(),
            )
            .field("generation", &self.generation)
            .finish()
    }
}

impl Mixer {
    /// Creates a mixer with `num_channels` mixing slots.
    pub fn new(num_channels: usize) -> Self {
        Self {
            slots: vec![MixSlot::empty(); num_channels],
            generation: 0,
            finished: Vec::new(),
        }
    }

    /// Number of mixing slots (`num_channels` in vanilla).
    pub fn num_channels(&self) -> usize {
        self.slots.len()
    }

    /// Number of currently playing slots.
    pub fn active_channels(&self) -> usize {
        self.slots.iter().filter(|s| s.is_playing()).count()
    }

    /// True when nothing is playing.
    pub fn is_idle(&self) -> bool {
        self.active_channels() == 0
    }

    /// Starts `sound` on `slot`.
    ///
    /// The slot must exist and must currently be free; the sound engine is
    /// responsible for picking a free-or-lowest-priority slot (the vanilla
    /// `S_getChannel` logic). Returns the slot generation so callers can
    /// match completion events against this play instance.
    ///
    /// # Errors
    ///
    /// [`AudioError::InvalidVolume`] if `volume` is outside `0..=127`,
    /// [`AudioError::ChannelOutOfRange`] if `slot` does not exist.
    pub fn start_on(
        &mut self,
        slot: usize,
        sound: Arc<DmxSound>,
        volume: i32,
        sep: i32,
        pitch: i32,
    ) -> AudioResult<u64> {
        if !(0..=MAX_VOLUME).contains(&volume) {
            return Err(AudioError::InvalidVolume(volume));
        }
        if slot >= self.slots.len() {
            return Err(AudioError::ChannelOutOfRange {
                channel: slot,
                num_channels: self.slots.len(),
            });
        }
        self.generation = self.generation.checked_add(1).unwrap_or(1);
        let generation = self.generation;
        let (left_vol, right_vol) = sep_to_stereo(volume, sep);
        let slot = &mut self.slots[slot];
        slot.sound = Some(sound);
        slot.position = 0.0;
        slot.pitch_ratio = pitch_to_ratio(pitch);
        slot.left_vol = left_vol;
        slot.right_vol = right_vol;
        slot.pitch = pitch.clamp(0, MAX_PITCH);
        slot.generation = generation;
        Ok(generation)
    }

    /// Stops `slot` immediately; no completion event is emitted (vanilla
    /// `I_StopSound` semantics). A free slot is ignored.
    pub fn stop(&mut self, slot: usize) {
        if let Some(slot) = self.slots.get_mut(slot) {
            slot.sound = None;
        }
    }

    /// Re-applies `volume`/`sep` to a playing slot; a no-op on a free slot.
    ///
    /// Vanilla `I_UpdateSoundParams` was a no-op on Linux; the sound
    /// server used it, and our per-tic distance attenuation needs it.
    pub fn set_params(&mut self, slot: usize, volume: i32, sep: i32) {
        let Some(slot) = self.slots.get_mut(slot) else {
            return;
        };
        if slot.sound.is_none() {
            return;
        }
        let volume = volume.clamp(0, MAX_VOLUME);
        let (left_vol, right_vol) = sep_to_stereo(volume, sep);
        slot.left_vol = left_vol;
        slot.right_vol = right_vol;
    }

    /// True while `slot` has an active sound.
    pub fn is_playing(&self, slot: usize) -> bool {
        self.slots.get(slot).is_some_and(MixSlot::is_playing)
    }

    /// Drains the completion events recorded during mixing.
    ///
    /// A slot re-started after finishing reports with the generation it
    /// had when the sound ended, so stale events can be told apart from
    /// events belonging to a newer play on the same slot.
    pub fn take_finished(&mut self) -> Vec<(usize, u64)> {
        std::mem::take(&mut self.finished)
    }

    /// Snapshot of `slot` for tests and diagnostics.
    pub fn slot_info(&self, slot: usize) -> Option<MixSlotInfo> {
        let slot = self.slots.get(slot)?;
        Some(MixSlotInfo {
            generation: slot.generation,
            pitch: slot.pitch,
            left_vol: slot.left_vol,
            right_vol: slot.right_vol,
            position: slot.position,
            sound_len: slot.sound.as_ref().map_or(0, |s| s.samples.len()),
        })
    }

    /// Mixes into `dst` as interleaved stereo frames at `output_rate`.
    ///
    /// Every element of `dst` is written; frames are `dst.len() / 2`. A
    /// trailing odd element is silenced rather than panicking. Saturating
    /// sum-to-16-bit clamping is applied like the vanilla `I_UpdateSound`.
    pub fn mix_into(&mut self, dst: &mut [i16], output_rate: u32) {
        // zero first so inactive slots contribute silence
        dst.fill(0);

        let frames = dst.len() / 2;
        if frames == 0 || output_rate == 0 {
            return;
        }

        for slot_index in 0..self.slots.len() {
            let Some(sound) = self.slots[slot_index].sound.clone() else {
                continue;
            };
            let len = sound.samples.len();
            if len == 0 {
                let generation = self.slots[slot_index].generation;
                self.slots[slot_index].sound = None;
                self.finished.push((slot_index, generation));
                continue;
            }

            let spec_rate = f64::from(sound.sample_rate.max(1));
            let step = spec_rate * self.slots[slot_index].pitch_ratio / f64::from(output_rate);
            let left_vol = self.slots[slot_index].left_vol;
            let right_vol = self.slots[slot_index].right_vol;
            let mut position = self.slots[slot_index].position;
            let mut slot_finished = false;

            for frame in 0..frames {
                let index = position as usize;
                if index >= len {
                    slot_finished = true;
                    break;
                }
                let frac = position - index as f64;
                let s0 = i32::from(sound.samples[index]);
                let s1 = sound
                    .samples
                    .get(index + 1)
                    .copied()
                    .map(i32::from)
                    .unwrap_or(0);
                let sample =
                    (f64::from(s0) + (f64::from(s1) - f64::from(s0)) * frac).round() as i32;

                // vanilla vol_lookup math, inlined: sample * vol / 127
                let left = sample * left_vol / MAX_VOLUME;
                let right = sample * right_vol / MAX_VOLUME;
                let dl = i32::from(dst[frame * 2]) + left;
                let dr = i32::from(dst[frame * 2 + 1]) + right;
                dst[frame * 2] = dl.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                dst[frame * 2 + 1] = dr.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;

                position += step;
                if position >= len as f64 {
                    slot_finished = true;
                    break;
                }
            }

            let slot = &mut self.slots[slot_index];
            slot.position = position;
            if slot_finished {
                let generation = slot.generation;
                slot.sound = None;
                self.finished.push((slot_index, generation));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(samples: &[i16]) -> Arc<DmxSound> {
        Arc::new(DmxSound {
            sample_rate: 11025,
            samples: samples.to_vec(),
        })
    }

    fn loud_square() -> Arc<DmxSound> {
        // constant positive signal, easy to reason about
        sound(&[1000; 512])
    }

    #[test]
    fn sep_to_stereo_endpoints_and_center() {
        // sep 0 -> hard left, sep 255 -> hard right, sep 128 ~ center
        assert_eq!(sep_to_stereo(127, 0), (127, 0));
        assert_eq!(sep_to_stereo(127, 255), (0, 127));
        let (l, r) = sep_to_stereo(127, 128);
        assert!(l >= 90 && r >= 90, "center-ish volumes, got ({l}, {r})");
        // vanilla keeps a slight asymmetry at center (129^2 vs 128^2)
        assert_eq!(sep_to_stereo(127, 128), (95, 96));
        // zero volume silences both ears
        assert_eq!(sep_to_stereo(0, 128), (0, 0));
    }

    #[test]
    fn pitch_to_ratio_matches_vanilla_steptable_law() {
        assert_eq!(pitch_to_ratio(128), 1.0);
        assert!((pitch_to_ratio(150) - 2f64.powf(22.0 / 64.0)).abs() < 1e-12);
        assert!((pitch_to_ratio(0) - 0.25).abs() < 1e-12);
        assert!((pitch_to_ratio(255) - 2f64.powf(127.0 / 64.0)).abs() < 1e-12);
    }

    #[test]
    fn start_on_rejects_out_of_range_volume() {
        let mut mixer = Mixer::new(1);
        assert!(matches!(
            mixer.start_on(0, loud_square(), 128, 128, 128),
            Err(AudioError::InvalidVolume(128))
        ));
        assert!(matches!(
            mixer.start_on(0, loud_square(), -1, 128, 128),
            Err(AudioError::InvalidVolume(-1))
        ));
    }

    #[test]
    fn same_rate_playback_is_bit_exact() {
        let mut mixer = Mixer::new(1);
        let sfx = sound(&[100, 200, -100, 300, -300, 0, 42, -42]);
        // volume 127: scaling is identity (sample * 127 / 127)
        mixer.start_on(0, sfx, 127, 0, 128).unwrap(); // sep 0 -> left only
        let mut dst = [0i16; 16];
        mixer.mix_into(&mut dst, 11025);
        // sep 0 => left = full volume, right = 0
        let expected_l = [100i16, 200, -100, 300, -300, 0, 42, -42];
        for (frame, want) in expected_l.iter().enumerate() {
            assert_eq!(dst[frame * 2], *want, "frame {frame} left");
            assert_eq!(dst[frame * 2 + 1], 0, "frame {frame} right");
        }
    }

    #[test]
    fn sound_finishes_and_reports_generation() {
        let mut mixer = Mixer::new(1);
        let generation = mixer
            .start_on(0, sound(&[1, 2, 3, 4]), 127, 128, 128)
            .unwrap();
        let mut finished = Vec::new();
        // keep mixing until the sound ends
        for _ in 0..8 {
            let mut dst = [0i16; 8];
            mixer.mix_into(&mut dst, 11025);
            finished.extend(mixer.take_finished());
        }
        assert_eq!(finished, vec![(0, generation)]);
        assert!(!mixer.is_playing(0));
        assert!(mixer.is_idle());
    }

    #[test]
    fn stop_silences_immediately_without_finish_event() {
        let mut mixer = Mixer::new(1);
        mixer.start_on(0, loud_square(), 127, 0, 128).unwrap();
        mixer.stop(0);
        assert!(!mixer.is_playing(0));
        assert!(mixer.take_finished().is_empty());
        let mut dst = [123i16; 8];
        mixer.mix_into(&mut dst, 11025);
        assert_eq!(dst, [0; 8]);
    }

    #[test]
    fn mixing_sums_channels_and_clamps_to_i16() {
        let mut mixer = Mixer::new(2);
        // two identical full-scale channels on the left ear must clamp
        mixer.start_on(0, loud_square(), 127, 0, 128).unwrap();
        mixer.start_on(1, loud_square(), 127, 0, 128).unwrap();
        let mut dst = [0i16; 4];
        mixer.mix_into(&mut dst, 11025);
        assert_eq!(dst[0], 2000.clamp(-32768, 32767) as i16);
        // and negative clamping works the same way
        let mut mixer = Mixer::new(2);
        mixer.start_on(0, sound(&[-30000; 8]), 127, 0, 128).unwrap();
        mixer.start_on(1, sound(&[-30000; 8]), 127, 0, 128).unwrap();
        let mut dst = [0i16; 4];
        mixer.mix_into(&mut dst, 11025);
        assert_eq!(dst[0], -32768);
    }

    #[test]
    fn higher_pitch_finishes_sooner() {
        let mut slow = Mixer::new(1);
        let mut fast = Mixer::new(1);
        slow.start_on(0, sound(&[7; 100]), 127, 128, 128).unwrap();
        fast.start_on(0, sound(&[7; 100]), 127, 128, 192).unwrap();
        let mut slow_done_at = None;
        let mut fast_done_at = None;
        for frame in 0..400 {
            let mut dst = [0i16; 2];
            slow.mix_into(&mut dst, 11025);
            fast.mix_into(&mut dst, 11025);
            if !slow.take_finished().is_empty() && slow_done_at.is_none() {
                slow_done_at = Some(frame);
            }
            if !fast.take_finished().is_empty() && fast_done_at.is_none() {
                fast_done_at = Some(frame);
            }
        }
        let slow_done = slow_done_at.expect("slow sound must finish");
        let fast_done = fast_done_at.expect("fast sound must finish");
        assert!(
            fast_done < slow_done,
            "pitch 192 should finish before pitch 128"
        );
    }

    #[test]
    fn upsampled_output_is_not_silent_and_terminates() {
        let mut mixer = Mixer::new(1);
        mixer
            .start_on(0, sound(&[5000; 100]), 127, 64, 128)
            .unwrap();
        let mut saw_nonzero = false;
        let mut finished = false;
        for _ in 0..40 {
            let mut dst = vec![0i16; 480 * 2]; // 10 ms at 48000
            mixer.mix_into(&mut dst, 48000);
            if dst.iter().any(|&s| s != 0) {
                saw_nonzero = true;
            }
            if !mixer.take_finished().is_empty() {
                finished = true;
                break;
            }
        }
        assert!(saw_nonzero);
        assert!(finished);
    }

    #[test]
    fn set_params_reweights_ears_mid_play() {
        let mut mixer = Mixer::new(1);
        mixer.start_on(0, sound(&[1000; 64]), 127, 0, 128).unwrap(); // hard left
        mixer.set_params(0, 127, 255); // hard right
        let mut dst = [0i16; 4];
        mixer.mix_into(&mut dst, 11025);
        assert_eq!(dst[0], 0);
        assert_eq!(dst[1], 1000);
    }

    #[test]
    fn set_params_on_free_slot_is_a_no_op() {
        let mut mixer = Mixer::new(1);
        mixer.set_params(0, 127, 0);
        assert_eq!(mixer.slot_info(0).unwrap().left_vol, 0);
    }

    #[test]
    fn empty_sound_finishes_on_first_mix() {
        let mut mixer = Mixer::new(1);
        mixer.start_on(0, sound(&[]), 127, 128, 128).unwrap();
        let mut dst = [0i16; 2];
        mixer.mix_into(&mut dst, 11025);
        assert_eq!(mixer.take_finished().len(), 1);
        assert!(mixer.is_idle());
    }

    #[test]
    fn odd_sized_dst_does_not_panic() {
        let mut mixer = Mixer::new(1);
        mixer.start_on(0, loud_square(), 127, 128, 128).unwrap();
        let mut dst = [0i16; 5];
        mixer.mix_into(&mut dst, 11025);
        assert_eq!(dst[4], 0); // trailing odd element stays silent
    }

    #[test]
    fn generation_bumps_per_play() {
        let mut mixer = Mixer::new(1);
        let g1 = mixer.start_on(0, sound(&[1]), 127, 128, 128).unwrap();
        mixer.stop(0);
        let g2 = mixer.start_on(0, sound(&[1]), 127, 128, 128).unwrap();
        assert_ne!(g1, g2);
    }
}

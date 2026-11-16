//! Output backends for the mixer.
//!
//! Two backends share the exact same per-callback mixing path
//! (`fill_buffer`):
//!
//! - [`CpalOutput`] opens the default output device (via cpal) and plays
//!   the mixer continuously. Not exercised by the test suite; a manual
//!   smoke test lives at the bottom of this file.
//! - [`NullOutput`] never touches a device: it drives the mixer on demand
//!   and counts RPC-style calls, so tests can verify the whole
//!   engine -> mixer -> output pipeline deterministically. This is a test
//!   output endpoint, not a gameplay stub.
//!
//! Neither does any work on the game thread beyond locking the mixer for
//! the duration of one buffer fill.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};

use super::error::{AudioError, AudioResult};
use super::s_sound::SharedMixer;

/// Opens the default output device and starts the mixer stream.
///
/// The device's default configuration drives the sample rate (no forced
/// rate) and the channel count (typically 2; mono downmix and 5.1-style
/// >stereo layouts are handled by `fill_buffer`).
///
/// # Errors
///
/// [`AudioError::NoOutputDevice`] when the machine has no default output,
/// [`AudioError::Backend`] for any device/stream failure,
/// [`AudioError::UnsupportedSampleFormat`] for sample formats other than
/// `F32`/`I16`/`U16`.
pub struct CpalOutput {
    // kept alive so playback continues; dropping `Stream` stops it
    _stream: cpal::Stream,
    sample_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
}

impl std::fmt::Debug for CpalOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpalOutput")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("sample_format", &self.sample_format)
            .finish_non_exhaustive()
    }
}

impl CpalOutput {
    /// Creates and starts the output stream.
    pub fn new(mixer: SharedMixer) -> AudioResult<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;

        let supported = device.default_output_config()?;
        let sample_format = supported.sample_format();
        let sample_rate = supported.sample_rate();
        let config = supported.config();
        let channels = config.channels;
        let device_channels = usize::from(channels);

        let mut scratch = Vec::new();
        let mut mixer = mixer; // moved into the callback
        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill_buffer(
                        data,
                        &mut scratch,
                        &mut mixer,
                        device_channels,
                        sample_rate,
                        |s| f32::from(s) / 32768.0,
                    );
                },
                log_stream_error,
                None,
            )?,
            SampleFormat::I16 => device.build_output_stream(
                config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    fill_buffer(
                        data,
                        &mut scratch,
                        &mut mixer,
                        device_channels,
                        sample_rate,
                        |s| s,
                    );
                },
                log_stream_error,
                None,
            )?,
            SampleFormat::U16 => device.build_output_stream(
                config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    fill_buffer(
                        data,
                        &mut scratch,
                        &mut mixer,
                        device_channels,
                        sample_rate,
                        |s| (i32::from(s) + 32768) as u16,
                    );
                },
                log_stream_error,
                None,
            )?,
            other => return Err(AudioError::UnsupportedSampleFormat(other.to_string())),
        };

        stream.play()?;
        Ok(Self {
            _stream: stream,
            sample_rate,
            channels,
            sample_format,
        })
    }

    /// The device's sample rate in hertz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The device's channel count (before mono downmix).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// The device's sample format.
    pub fn sample_format(&self) -> SampleFormat {
        self.sample_format
    }
}

fn log_stream_error(err: cpal::Error) {
    tracing::error!("audio output stream error: {err}");
}

/// Fills `data` (interleaved device frames) from the mixer.
///
/// Handles arbitrary device channel counts: channel 0 gets the left mix,
/// channel 1 the right mix; mono devices get the averaged downmix; extra
/// channels get silence. A trailing partial frame is written with the
/// channels present rather than panicking.
fn fill_buffer<T: SizedSample>(
    data: &mut [T],
    scratch: &mut Vec<i16>,
    mixer: &mut SharedMixer,
    device_channels: usize,
    output_rate: u32,
    convert: fn(i16) -> T,
) {
    if device_channels == 0 {
        return;
    }

    let frames = data.len() / device_channels;
    if frames == 0 {
        return;
    }

    scratch.clear();
    scratch.resize(frames * 2, 0);
    {
        let mut guard = mixer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.mix_into(scratch, output_rate);
    }

    for (i, frame) in data.chunks_mut(device_channels).enumerate() {
        let left = scratch[i * 2];
        let right = scratch[i * 2 + 1];
        match frame.len() {
            0 => {}
            1 => {
                let mixed = ((i32::from(left) + i32::from(right)) / 2)
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                    as i16;
                frame[0] = convert(mixed);
            }
            _ => {
                frame[0] = convert(left);
                frame[1] = convert(right);
                for ch in frame.iter_mut().skip(2) {
                    *ch = convert(0);
                }
            }
        }
    }
}

/// An in-memory output endpoint used by tests.
///
/// Drives the shared mixer on demand and records how often the "device
/// callback" ran; it never touches real hardware. The mixing path it uses
/// is the same `fill_buffer` cpal calls into, so tests exercise the real
/// audio output code, not a reimplementation.
#[derive(Debug)]
pub struct NullOutput {
    mixer: SharedMixer,
    output_rate: u32,
    callback_calls: u64,
    frames_mixed: u64,
}

impl NullOutput {
    /// Creates the endpoint at `output_rate` hertz.
    pub fn new(mixer: SharedMixer, output_rate: u32) -> Self {
        Self {
            mixer,
            output_rate,
            callback_calls: 0,
            frames_mixed: 0,
        }
    }

    /// The configured output rate in hertz.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Number of simulated device callback invocations.
    pub fn callback_calls(&self) -> u64 {
        self.callback_calls
    }

    /// Total stereo frames produced so far.
    pub fn frames_mixed(&self) -> u64 {
        self.frames_mixed
    }

    /// Runs one simulated callback and returns the interleaved stereo
    /// buffer it would have handed to the device.
    pub fn mix_chunk(&mut self, frames: usize) -> Vec<i16> {
        let mut scratch = vec![0i16; frames * 2];
        {
            let mut guard = self
                .mixer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.mix_into(&mut scratch, self.output_rate);
        }
        self.callback_calls += 1;
        self.frames_mixed += frames as u64;
        scratch
    }

    /// Convenience: maps the interleaved stereo chunk to normalized f32
    /// samples, the same conversion an `F32` device gets in its callback.
    pub fn mix_chunk_f32(&mut self, frames: usize) -> Vec<f32> {
        self.mix_chunk(frames)
            .into_iter()
            .map(|s| f32::from(s) / 32768.0)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dmx::DmxSound;
    use crate::audio::mixer::Mixer;
    use std::sync::{Arc, Mutex};

    fn shared_mixer_with(sound: Arc<DmxSound>, volume: i32, sep: i32) -> SharedMixer {
        let mut mixer = Mixer::new(1);
        mixer
            .start_on(0, sound, volume, sep, 128)
            .expect("test sound must start");
        Arc::new(Mutex::new(mixer))
    }

    fn dc_sound(value: i16, len: usize) -> Arc<DmxSound> {
        Arc::new(DmxSound {
            sample_rate: 11025,
            samples: vec![value; len],
        })
    }

    #[test]
    fn null_output_drives_mixer_and_counts_calls() {
        let mixer = shared_mixer_with(dc_sound(1000, 64), 127, 0);
        let mut output = NullOutput::new(mixer, 11025);
        let chunk = output.mix_chunk(8);
        assert_eq!(chunk.len(), 16);
        assert_eq!(chunk[0], 1000);
        assert_eq!(chunk[1], 0); // sep 0: right ear silent
        assert_eq!(output.callback_calls(), 1);
        assert_eq!(output.frames_mixed(), 8);
        let _ = output.mix_chunk_f32(2);
        assert_eq!(output.callback_calls(), 2);
    }

    #[test]
    fn mono_device_gets_averaged_downmix() {
        let mut mixer = shared_mixer_with(dc_sound(1000, 64), 127, 255); // hard right
        let mut data = vec![0i16; 4]; // 4 mono frames
        let mut scratch = Vec::new();
        fill_buffer(&mut data, &mut scratch, &mut mixer, 1, 11025, |s| s);
        // right-only signal downmixed: left=0, right=1000 -> avg 500
        assert_eq!(data, vec![500, 500, 500, 500]);
    }

    #[test]
    fn surround_device_silences_extra_channels() {
        let mut mixer = shared_mixer_with(dc_sound(1000, 64), 127, 128);
        let mut data = vec![7i16; 10]; // 2 frames of 5 channels
        let mut scratch = Vec::new();
        fill_buffer(&mut data, &mut scratch, &mut mixer, 5, 11025, |s| s);
        // ch0/ch1 carry the mix, ch2..ch4 are silenced
        assert_eq!(data[2], 0);
        assert_eq!(data[3], 0);
        assert_eq!(data[4], 0);
        assert_eq!(data[7], 0);
        assert_eq!(data[8], 0);
        assert_eq!(data[9], 0);
    }

    #[test]
    fn f32_conversion_scales_full_range() {
        let mut mixer = shared_mixer_with(dc_sound(32767, 4), 127, 0);
        let mut data = vec![0f32; 2];
        let mut scratch = Vec::new();
        fill_buffer(&mut data, &mut scratch, &mut mixer, 2, 11025, |s| {
            f32::from(s) / 32768.0
        });
        assert!((data[0] - 32767.0 / 32768.0).abs() < 1e-9);
        assert_eq!(data[1], 0.0);
    }

    #[test]
    fn u16_conversion_offsets_by_origin() {
        let mut mixer = shared_mixer_with(dc_sound(-32768, 4), 127, 0);
        let mut data = vec![0u16; 2];
        let mut scratch = Vec::new();
        fill_buffer(&mut data, &mut scratch, &mut mixer, 2, 11025, |s| {
            (i32::from(s) + 32768) as u16
        });
        assert_eq!(data[0], 0);
        assert_eq!(data[1], 32768);
    }

    #[test]
    #[ignore = "opens the default audio device; run manually with --ignored"]
    fn cpal_output_smoke() {
        let mixer = Arc::new(Mutex::new(Mixer::new(1)));
        let output = CpalOutput::new(mixer).expect("default output device should open");
        assert!(output.sample_rate() > 0);
        assert!(output.channels() > 0);
    }
}

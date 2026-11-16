//! Output audio for the platform shell.
//!
//! One cpal stream plays both halves of the engine's sound:
//!
//! - the SFX mixer, drained every callback through its shared handle
//!   ([`doom_engine::audio::SharedMixer`]), and
//! - the music engine, which is pull-based and knows nothing about
//!   devices, fed here through a small ring buffer the frame loop keeps
//!   topped up ([`pump_music`]).
//!
//! Everything that can fail at device setup returns `Err(String)` and the
//! caller treats it as "no audio": the game must keep running in silence,
//! never crash. The mixer and the ring are lock-protected like the
//! engine's own backend; a poisoned lock is recovered into the inner
//! value rather than panicking the game thread or the device callback.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SizedSample, default_host};

use doom_engine::audio::SharedMixer;
use doom_engine::game::Game;

/// Mono music samples the engine hands out live at this rate; the pump
/// resamples on the fly when the device runs at something else.
pub const MUSIC_RATE: u32 = doom_engine::music::DEFAULT_SAMPLE_RATE;

/// Cap on buffered music, in seconds of device output. A hiccup at a
/// frame boundary must not be able to starve the device.
const FEED_CAP_SECONDS: f64 = 2.0;

/// Ring of mono music samples waiting to reach the device. Drained by
/// the audio callback, refilled by the frame loop at up to device rate.
pub struct MusicFeed {
    samples: VecDeque<f32>,
    cap: usize,
}

impl MusicFeed {
    fn new(device_rate: u32) -> Self {
        let cap = ((f64::from(device_rate) * FEED_CAP_SECONDS).round() as usize).max(1024);
        Self {
            samples: VecDeque::with_capacity(cap),
            cap,
        }
    }

    fn push(&mut self, sample: f32) {
        if self.samples.len() >= self.cap {
            // drop the oldest frame: staying in sync beats preserving it
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    fn pop(&mut self) -> f32 {
        self.samples.pop_front().unwrap_or(0.0)
    }
}

/// A running output stream and the handle used to feed it music.
pub struct AudioOutput {
    // dropping the stream stops playback; kept alive for the process lifetime
    _stream: cpal::Stream,
    /// Ring fed by [`pump_music`] and drained in the device callback.
    pub feed: Arc<Mutex<MusicFeed>>,
    /// Device sample rate in hertz (music is resampled to this).
    pub device_rate: u32,
    /// Device channel count (1, 2 or more; extras get silence).
    pub channels: u16,
    /// Device sample format in use.
    pub sample_format: SampleFormat,
}

impl fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioOutput")
            .field("device_rate", &self.device_rate)
            .field("channels", &self.channels)
            .field("sample_format", &self.sample_format)
            .finish_non_exhaustive()
    }
}

/// Pulls `music_samples` worth of music out of the game and into the
/// feed. Called once per frame with the wall-clock milliseconds that
/// elapsed since the previous call.
///
/// `credit` keeps the fractional sample position across frames so a
/// 59.7 Hz frame rate does not slowly drift away from 48 kHz audio.
/// Catch-up after a long stall is capped at one second of audio.
pub fn pump_music(
    game: &mut Game,
    audio: &AudioOutput,
    elapsed_ms: u64,
    music_credit: &mut f64,
    music_buf: &mut Vec<f32>,
) {
    *music_credit += elapsed_ms as f64 * f64::from(MUSIC_RATE) / 1000.0;
    let mut needed = music_credit.floor() as usize;
    if needed == 0 {
        return;
    }
    *music_credit -= needed as f64;
    needed = needed.min(MUSIC_RATE as usize);

    music_buf.clear();
    music_buf.resize(needed, 0.0);
    // always fills `out`; silence when nothing is playing, which is the
    // correct thing to push into the ring between songs
    game.music_engine().pull_samples(music_buf);

    let mut feed = audio
        .feed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if audio.device_rate == MUSIC_RATE {
        for &sample in music_buf.iter() {
            feed.push(sample);
        }
        return;
    }

    // chunked linear resample; phase restarts per frame, which can lose at
    // most one interpolation point at the frame boundary - inaudible here
    let step = f64::from(MUSIC_RATE) / f64::from(audio.device_rate);
    let produced = (music_buf.len() as f64 / step).floor() as usize;
    for i in 0..produced {
        let pos = i as f64 * step;
        let index = pos as usize;
        let frac = (pos - index as f64) as f32;
        let a = music_buf[index];
        let b = music_buf.get(index + 1).copied().unwrap_or(a);
        feed.push(a + (b - a) * frac);
    }
}

/// Opens the default output device and starts playing immediately.
///
/// Returns an error (caller decides how to degrade) when there is no
/// device, the device's default format is unsupported, or the stream
/// cannot be built. Never panics.
pub fn start(game: &mut Game) -> Result<AudioOutput, String> {
    let host = default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default audio output device".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|err| format!("could not query the audio device configuration: {err}"))?;

    let sample_format = supported.sample_format();
    let config = supported.config();
    let device_rate = supported.sample_rate();
    let channels = config.channels;
    let device_channels = usize::from(channels);
    if device_channels == 0 {
        return Err("audio device reports zero output channels".to_string());
    }

    let mixer = game.sound_mixer();
    let feed = Arc::new(Mutex::new(MusicFeed::new(device_rate)));

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            config,
            device_channels,
            device_rate,
            mixer.clone(),
            Arc::clone(&feed),
            |s| s,
        ),
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            config,
            device_channels,
            device_rate,
            mixer.clone(),
            Arc::clone(&feed),
            |s| {
                (s * 32767.0)
                    .round()
                    .clamp(i32::from(i16::MIN) as f32, i32::from(i16::MAX) as f32)
                    as i16
            },
        ),
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            config,
            device_channels,
            device_rate,
            mixer.clone(),
            Arc::clone(&feed),
            |s| {
                ((s * 32767.0)
                    .round()
                    .clamp(i32::from(i16::MIN) as f32, i32::from(i16::MAX) as f32)
                    + 32768.0) as u16
            },
        ),
        other => {
            return Err(format!(
                "unsupported sample format {other}; expected F32/I16/U16"
            ));
        }
    }
    .map_err(|err| format!("could not open the audio stream: {err}"))?;

    stream
        .play()
        .map_err(|err| format!("could not start the audio stream: {err}"))?;

    Ok(AudioOutput {
        _stream: stream,
        feed,
        device_rate,
        channels,
        sample_format,
    })
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    device_channels: usize,
    device_rate: u32,
    mixer: SharedMixer,
    feed: Arc<Mutex<MusicFeed>>,
    convert: fn(f32) -> T,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + 'static,
{
    let mut scratch = Vec::new();
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            fill_buffer(
                data,
                device_channels,
                device_rate,
                &mixer,
                &feed,
                &mut scratch,
                convert,
            );
        },
        |err| tracing::error!("audio stream error: {err}"),
        None,
    )
}

/// Fills `data` (interleaved device frames) from the SFX mixer and the
/// music ring. Same layout rules as the engine's own backend: mono
/// devices get the averaged downmix, extra channels get silence.
fn fill_buffer<T: SizedSample>(
    data: &mut [T],
    device_channels: usize,
    device_rate: u32,
    mixer: &SharedMixer,
    feed: &Arc<Mutex<MusicFeed>>,
    scratch: &mut Vec<i16>,
    convert: fn(f32) -> T,
) {
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
        guard.mix_into(scratch, device_rate);
    }

    let mut music = feed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for (index, frame) in data.chunks_mut(device_channels).enumerate() {
        let left = f32::from(scratch[index * 2]) / 32768.0;
        let right = f32::from(scratch[index * 2 + 1]) / 32768.0;
        // one mono music sample feeds both sides equally
        let music_sample = music.pop();
        match frame.len() {
            0 => {}
            1 => {
                frame[0] = convert(((left + right) * 0.5 + music_sample).clamp(-1.0, 1.0));
            }
            _ => {
                frame[0] = convert((left + music_sample).clamp(-1.0, 1.0));
                frame[1] = convert((right + music_sample).clamp(-1.0, 1.0));
                for channel in frame.iter_mut().skip(2) {
                    *channel = convert(0.0);
                }
            }
        }
    }
}

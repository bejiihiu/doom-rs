//! DMX digitized sound decoder.
//!
//! Ported from the lump layout the linuxdoom-1.10 `i_sound.c` `getsfx()`
//! consumed (it skipped a fixed 8-byte base header) and the format
//! description recovered by the community after the DMX library itself was
//! never released:
//!
//! | Offset | Type       | Meaning                                        |
//! |--------|------------|------------------------------------------------|
//! | 0x00   | u16 LE     | format tag, must be 3 (digitized)              |
//! | 0x02   | u16 LE     | sample rate (almost always 11025, some 22050)  |
//! | 0x04   | u32 LE     | sample count + 32 (official padded files)      |
//! | 0x08   | 16 bytes   | pre-roll pad, filled with the first sample     |
//! | 0x18   | samples    | raw 8-bit unsigned PCM                         |
//! | end-16 | 16 bytes   | post-roll pad, filled with the last sample     |
//!
//! Vanilla ignored the pad bytes entirely and played them as audio; we
//! skip them. Officially padded files are told apart from files written by
//! naive third-party tools by checking whether the 16 pre-pad bytes are all
//! equal to the first sample and the 16 trailing bytes are all equal to the
//! last sample - the layout the official IWADs follow. Everything else is
//! taken to be the unpadded layout (header directly followed by samples,
//! header length field equal to the sample count).
//!
//! Samples are stored as centered 16-bit values: `(byte - 128) << 8`.
//! That peaks at +32512 / -32768, identical to vanilla's
//! `vol_lookup[i][j] = i*(j-128)*256/127` at i=127.

use crate::swap;

use super::error::{AudioError, AudioResult};

/// DMX format tag for digitized sounds (the only format we decode).
pub const DMX_FORMAT_TAG: u16 = 3;
/// Size of the base header: tag, sample rate, length field.
pub const DMX_HEADER_LEN: usize = 8;
/// Pad bytes before the samples in the official layout.
pub const DMX_PRE_PAD: usize = 16;
/// Pad bytes after the samples in the official layout.
pub const DMX_POST_PAD: usize = 16;
/// `(DMX_PRE_PAD + DMX_POST_PAD)`, i.e. how many bytes the header length
/// field counts beyond the raw sample count in padded files.
pub const DMX_PAD_TOTAL: usize = DMX_PRE_PAD + DMX_POST_PAD;
/// Vanilla `SAMPLERATE`; virtually every lump in the IWADs uses this.
pub const DMX_TYPICAL_SAMPLE_RATE: u32 = 11025;

/// A decoded DMX sound effect.
///
/// `samples` are centered 16-bit PCM (see the module docs). The rate is
/// taken from the header, not assumed - a handful of lumps are 22050 Hz.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmxSound {
    /// Sample rate in hertz, 1..=65535.
    pub sample_rate: u32,
    /// Centered 16-bit PCM samples, one byte of source per sample.
    pub samples: Vec<i16>,
}

impl DmxSound {
    /// Duration of the sample in source frames.
    pub fn len_frames(&self) -> usize {
        self.samples.len()
    }

    /// True when the sound carries no samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Decodes a DMX digitized sound lump.
///
/// Accepts both the officially padded layout (16 pad bytes around the
/// samples, header length field = sample count + 32) and the unpadded
/// variant produced by naive tools (samples directly after the 8-byte
/// header, header length field = sample count).
///
/// # Errors
///
/// Returns [`AudioError`] on truncated data, a wrong format tag, a zero
/// sample rate or a header/lump size mismatch. Corrupt lumps never panic.
pub fn decode_dmx_sound(data: &[u8]) -> AudioResult<DmxSound> {
    if data.len() < DMX_HEADER_LEN {
        return Err(AudioError::DmxTruncated {
            needed: DMX_HEADER_LEN,
            available: data.len(),
        });
    }

    let format = swap::read_u16(data, 0)?;
    if format != DMX_FORMAT_TAG {
        return Err(AudioError::DmxFormatTag(format));
    }

    let sample_rate = swap::read_u16(data, 2)?;
    if sample_rate == 0 {
        return Err(AudioError::DmxBadSampleRate);
    }

    let header_length = swap::read_u32(data, 4)?;
    let available = data.len() - DMX_HEADER_LEN;

    let body: &[u8] = match classify_layout(data, header_length, available)? {
        Layout::Padded { samples } => samples,
        Layout::Unpadded { samples } => samples,
    };

    let samples = body
        .iter()
        .map(|&byte| ((i32::from(byte) - 128) << 8) as i16)
        .collect();

    Ok(DmxSound {
        sample_rate: u32::from(sample_rate),
        samples,
    })
}

/// Which of the two accepted layouts a lump uses.
enum Layout<'a> {
    /// Officially padded: pads before and after `samples`.
    Padded {
        /// The raw sample bytes (pads stripped, never empty-checked here).
        samples: &'a [u8],
    },
    /// Unpadded: header directly followed by the samples.
    Unpadded {
        /// The raw sample bytes.
        samples: &'a [u8],
    },
}

/// Decides between the padded and the unpadded layout.
///
/// Both layouts satisfy `available == header_length` (padded: length
/// counts pads + samples, unpadded: length IS the sample count), so the
/// edge-filled pads are what disambiguates them.
fn classify_layout<'a>(
    data: &'a [u8],
    header_length: u32,
    available: usize,
) -> AudioResult<Layout<'a>> {
    if u64::from(header_length) == available as u64 {
        let padded_candidate = padded_slice(data, header_length);
        if let Some(samples) = padded_candidate {
            return Ok(Layout::Padded { samples });
        }
        // Not edge-filled pads: treat as the naive unpadded layout.
        let samples = &data[DMX_HEADER_LEN..DMX_HEADER_LEN + available];
        return Ok(Layout::Unpadded { samples });
    }

    Err(AudioError::DmxSizeMismatch {
        header_length,
        available,
    })
}

/// Returns the sample slice if the lump looks like an officially padded
/// DMX sound (edge-filled pre/post pads), else `None`.
fn padded_slice(data: &[u8], header_length: u32) -> Option<&[u8]> {
    // header_length counts samples + 32 pads; too small to hold pads => no.
    let sample_count = (header_length as usize).checked_sub(DMX_PAD_TOTAL)?;
    let pad_start = DMX_HEADER_LEN;
    let samples_start = pad_start + DMX_PRE_PAD;
    let samples_end = samples_start.checked_add(sample_count)?;
    // trailing pad must close the lump exactly
    let pad_end = samples_end.checked_add(DMX_POST_PAD)?;
    if pad_end != data.len() {
        return None;
    }

    // an officially padded file with zero samples is legal; the pads are
    // meaningless there, so skip the edge-fill validation
    if sample_count == 0 {
        return Some(&data[samples_start..samples_start]);
    }

    let first = *data.get(samples_start)?;
    let last = *data.get(samples_end - 1)?;

    let pre_ok = data[pad_start..samples_start].iter().all(|&b| b == first);
    let post_ok = data[samples_end..pad_end].iter().all(|&b| b == last);

    if pre_ok && post_ok {
        Some(&data[samples_start..samples_end])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an officially padded DMX lump around `samples`.
    fn padded_lump(rate: u16, samples: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&DMX_FORMAT_TAG.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&((samples.len() as u32) + DMX_PAD_TOTAL as u32).to_le_bytes());
        let first = samples[0];
        let last = samples[samples.len() - 1];
        data.extend_from_slice(&[first; DMX_PRE_PAD]);
        data.extend_from_slice(samples);
        data.extend_from_slice(&[last; DMX_POST_PAD]);
        data
    }

    /// Builds an unpadded DMX lump around `samples`.
    fn unpadded_lump(rate: u16, samples: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&DMX_FORMAT_TAG.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        data.extend_from_slice(samples);
        data
    }

    #[test]
    fn decodes_padded_lump_and_strips_pads() {
        let samples = [10u8, 20, 128, 200, 250];
        let data = padded_lump(11025, &samples);
        let sound = decode_dmx_sound(&data).unwrap();
        assert_eq!(sound.sample_rate, 11025);
        assert_eq!(sound.samples.len(), 5);
        assert_eq!(sound.samples[0], ((10i32 - 128) << 8) as i16);
        assert_eq!(sound.samples[2], 0);
        assert_eq!(sound.samples[4], ((250i32 - 128) << 8) as i16);
    }

    #[test]
    fn decodes_unpadded_lump() {
        let samples = [0u8, 64, 128, 192, 255];
        let data = unpadded_lump(11025, &samples);
        let sound = decode_dmx_sound(&data).unwrap();
        assert_eq!(sound.samples.len(), 5);
        assert_eq!(sound.samples[0], -32768);
        assert_eq!(sound.samples[4], (127i32 << 8) as i16);
    }

    #[test]
    fn padded_detection_requires_edge_filled_pads() {
        // 32 trailing "content" bytes after the 8-byte header, but the
        // would-be pads are not edge-filled, so this must decode as the
        // unpadded layout instead of silently skipping 32 bytes of audio.
        let samples: Vec<u8> = (0u8..=71).collect(); // 72 bytes > 32
        let mut data = Vec::new();
        data.extend_from_slice(&DMX_FORMAT_TAG.to_le_bytes());
        data.extend_from_slice(&11025u16.to_le_bytes());
        data.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        data.extend_from_slice(&samples);
        let sound = decode_dmx_sound(&data).unwrap();
        assert_eq!(sound.samples.len(), 72);
    }

    #[test]
    fn accepts_22050_rate() {
        let data = padded_lump(22050, &[128u8, 128, 128]);
        let sound = decode_dmx_sound(&data).unwrap();
        assert_eq!(sound.sample_rate, 22050);
    }

    #[test]
    fn rejects_non_dmx_format_tag() {
        let mut data = padded_lump(11025, &[1u8, 2, 3]);
        data[0] = 0;
        data[1] = 0; // tag 0 = PC speaker, not supported
        let err = decode_dmx_sound(&data).unwrap_err();
        assert!(matches!(err, AudioError::DmxFormatTag(0)));
    }

    #[test]
    fn rejects_truncated_header() {
        let err = decode_dmx_sound(&[3, 0, 1]).unwrap_err();
        assert!(matches!(
            err,
            AudioError::DmxTruncated {
                needed: DMX_HEADER_LEN,
                available: 3
            }
        ));
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let data = padded_lump(0, &[1u8, 2, 3]);
        let err = decode_dmx_sound(&data).unwrap_err();
        assert!(matches!(err, AudioError::DmxBadSampleRate));
    }

    #[test]
    fn rejects_header_lump_size_mismatch() {
        let mut data = padded_lump(11025, &[7u8, 8, 9, 10, 11]);
        // bump the length field so it no longer matches the lump size
        let len_field = (u32::from_le_bytes(data[4..8].try_into().unwrap()) + 16).to_le_bytes();
        data[4..8].copy_from_slice(&len_field);
        let err = decode_dmx_sound(&data).unwrap_err();
        assert!(matches!(err, AudioError::DmxSizeMismatch { .. }));
    }

    #[test]
    fn sample_conversion_matches_vanilla_peak_values() {
        let data = unpadded_lump(11025, &[0u8, 128, 255]);
        let sound = decode_dmx_sound(&data).unwrap();
        assert_eq!(sound.samples[0], -32768); // (0-128)<<8
        assert_eq!(sound.samples[1], 0); // (128-128)<<8
        assert_eq!(sound.samples[2], 32512); // (255-128)<<8, vanilla max at vol 127
    }
}

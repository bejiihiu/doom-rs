//! The MUS music format, as used by the DMX library.
//!
//! MUS is a compact single-track MIDI dialect. The file starts with a
//! 16-byte header followed by a list of instrument patch numbers, then the
//! song data: a stream of events, each optionally followed by a delay.
//!
//! ## Header layout (all little-endian)
//!
//! | Offset | Size | Field                                      |
//! | ------ | ---- | ------------------------------------------ |
//! | 0      | 4    | Magic, `MUS\x1A`                           |
//! | 4      | 2    | Song data length in bytes                  |
//! | 6      | 2    | Offset of the song data, from file start   |
//! | 8      | 2    | Primary channel count (MUS channels 0-8)   |
//! | 10     | 2    | Secondary channel count (MUS channels 10+) |
//! | 12     | 2    | Number of u16 instrument patches that follow |
//! | 14     | 2    | Reserved, should be zero                   |
//!
//! The header's size limit (16 bit fields) caps songs at 64 KiB.
//!
//! ## Event stream
//!
//! The first byte of an event carries three fields:
//!
//! - bit 7: `last` — a delay follows this event
//! - bits 4-6: event type
//! - bits 0-3: MIDI channel (0-15). Channel 15 is the percussion channel;
//!   the OPL engine maps it to MIDI channel 9.
//!
//! A delay is a variable-length quantity: read bytes while the high bit is
//! set, `delay = delay * 128 + (byte & 0x7F)` for each of them. The delay of
//! an event is the number of ticks that pass *after* the event, before the
//! next one; it is stored on the event itself here for convenience and is
//! applied to the next event at parse time.
//!
//! ## Timing
//!
//! DMX uses a 140 Hz time division: one tick is 1/140th of a second. The
//! Doom engine and most tooling use this rate; other DMX-based games may
//! use 70 Hz. The converter in [`crate::music::midi`] encodes this as a
//! MIDI `division` of 140 with a tempo of 60 bpm, which yields the exact
//! same tick duration.

use crate::music::error::MusicError;

/// Magic bytes at the start of a MUS file.
pub const MUS_MAGIC: &[u8; 4] = b"MUS\x1A";

/// DMX time division: one MUS tick is this many fractions of a second.
pub const MUS_TICKS_PER_SECOND: u32 = 140;

/// Number of ticks one delay byte step covers.
pub const DELAY_BASE: u32 = 128;

/// Fixed header size before the instrument patch list.
const HEADER_SIZE: usize = 16;

/// MUS channel reserved for percussion.
pub const PERCUSSION_CHANNEL: u8 = 15;

/// Lowest percussion patch number in the header's instrument list.
pub const PERCUSSION_PATCH_MIN: u16 = 135;

/// Highest percussion patch number in the header's instrument list.
pub const PERCUSSION_PATCH_MAX: u16 = 181;

/// System event codes (event type 3), as defined by DMX.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SystemEvent {
    /// All sounds off (notes silence immediately).
    AllSoundsOff = 10,
    /// All notes off (notes fade out).
    AllNotesOff = 11,
    /// Mono mode: one note per channel.
    Mono = 12,
    /// Poly mode: multiple notes per channel.
    Poly = 13,
    /// Reset all controllers on the channel.
    ResetAllControllers = 14,
    /// "Event" — never implemented by DMX; parsed but carries no action.
    Event = 15,
}

impl SystemEvent {
    /// Decodes a raw system-event code.
    fn from_code(code: u8) -> Result<Self, MusicError> {
        match code {
            10 => Ok(Self::AllSoundsOff),
            11 => Ok(Self::AllNotesOff),
            12 => Ok(Self::Mono),
            13 => Ok(Self::Poly),
            14 => Ok(Self::ResetAllControllers),
            15 => Ok(Self::Event),
            other => Err(MusicError::BadValue {
                context: "system event",
                detail: format!("unknown system event code {other}"),
            }),
        }
    }
}

/// The payload of a single MUS event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MusEventKind {
    /// Note released on the channel.
    ReleaseNote {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Note number, 0-127.
        note: u8,
    },
    /// Note started on the channel.
    PlayNote {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Note number, 0-127.
        note: u8,
        /// Volume for this note. MUS allows omitting the byte, in which
        /// case the channel's previous note volume is used.
        volume: Option<u8>,
    },
    /// Pitch bend applied to the whole channel. 128 is centered;
    /// 0 is one tone down, 255 is one tone up.
    PitchBend {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Raw bend value, 0-255.
        bend: u8,
    },
    /// A controller with no associated value.
    SystemEvent {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Decoded system event code.
        code: SystemEvent,
    },
    /// Set controller `controller` to `value` on the channel.
    ///
    /// Controller 0 changes the instrument (patch number); values 1-9 are
    /// the documented MUS controller definitions.
    Controller {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Controller number, 0-127.
        controller: u8,
        /// Controller value, 0-127.
        value: u8,
    },
    /// Marks the end of a musical measure. Carries no musical meaning
    /// for playback and is dropped by the MIDI converter.
    EndOfMeasure {
        /// MIDI channel, 0-15.
        channel: u8,
    },
    /// End of the song data. Doom loops the track from here.
    Finish,
    /// Event type 7, reserved/unused. Carries a single data byte that
    /// must be consumed to keep the stream aligned; otherwise ignored.
    Unused {
        /// MIDI channel, 0-15.
        channel: u8,
        /// The ignored data byte.
        data: u8,
    },
}

/// A MUS event with its delta time.
///
/// `ticks_from_last` is the number of 140 Hz ticks that elapsed between the
/// previous event and this one. It is accumulated while parsing: the delay
/// bytes that follow an event belong to the *next* event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MusEvent {
    /// Ticks elapsed since the previous event (0 for the first event
    /// unless the file starts with a delay, which is kept as-is).
    pub ticks_from_last: u32,
    /// What the event actually is.
    pub kind: MusEventKind,
}

/// A parsed MUS song.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusSong {
    /// Primary channel count from the header.
    pub primary_channels: u16,
    /// Secondary channel count from the header.
    pub secondary_channels: u16,
    /// Instrument patch list from the header.
    ///
    /// Values 0-127 are standard MIDI melodic patches; values 135-181 are
    /// the standard MIDI percussion patches (notes 35-81 on MIDI channel 9).
    pub instruments: Vec<u16>,
    /// All events in file order, with delta times filled in.
    pub events: Vec<MusEvent>,
}

impl MusSong {
    /// Total length of the song in 140 Hz ticks.
    pub fn total_ticks(&self) -> u64 {
        self.events
            .iter()
            .map(|event| u64::from(event.ticks_from_last))
            .sum()
    }

    /// Length of the song in seconds at the DMX time division.
    pub fn duration_seconds(&self) -> f64 {
        self.total_ticks() as f64 / f64::from(MUS_TICKS_PER_SECOND)
    }

    /// Maps a header instrument patch to a drum note, if it is one of the
    /// percussion patches (135-181, corresponding to notes 35-81).
    pub fn percussion_note(patch: u16) -> Option<u8> {
        if (PERCUSSION_PATCH_MIN..=PERCUSSION_PATCH_MAX).contains(&patch) {
            Some((patch - 100) as u8)
        } else {
            None
        }
    }
}

/// Parses a MUS file.
///
/// The input must contain exactly one MUS lump; trailing garbage after the
/// song data is tolerated (lmps in the wild sometimes have padding), but a
/// score that ends before a `Finish` event is an error.
pub fn parse_mus(data: &[u8]) -> Result<MusSong, MusicError> {
    if data.len() < HEADER_SIZE {
        return Err(MusicError::Truncated {
            needed: HEADER_SIZE,
            available: data.len(),
        });
    }

    if &data[..4] != MUS_MAGIC {
        return Err(MusicError::BadMagic {
            expected: "MUS\\x1A",
            found: data[..4].to_vec(),
        });
    }

    let read_u16 = |offset: usize| -> Result<u16, MusicError> {
        Ok(u16::from_le_bytes([
            *data.get(offset).ok_or(MusicError::Truncated {
                needed: offset + 2,
                available: data.len(),
            })?,
            *data.get(offset + 1).ok_or(MusicError::Truncated {
                needed: offset + 2,
                available: data.len(),
            })?,
        ]))
    };

    let song_len = usize::from(read_u16(4)?);
    let song_offset = usize::from(read_u16(6)?);
    let primary_channels = read_u16(8)?;
    let secondary_channels = read_u16(10)?;
    let instrument_count = usize::from(read_u16(12)?);

    // the instrument list sits between the fixed header and the score;
    // if it does not fit we are looking at corrupt data
    let instruments_end = HEADER_SIZE
        .checked_add(
            instrument_count
                .checked_mul(2)
                .ok_or(MusicError::BadValue {
                    context: "header",
                    detail: "instrument count overflows".into(),
                })?,
        )
        .ok_or(MusicError::BadValue {
            context: "header",
            detail: "instrument list end overflows".into(),
        })?;
    if instruments_end > song_offset {
        return Err(MusicError::BadValue {
            context: "header",
            detail: format!(
                "instrument list ({instrument_count} entries) ends at {instruments_end}, \
                 which is past the score offset {song_offset}"
            ),
        });
    }

    let song_end = song_offset
        .checked_add(song_len)
        .ok_or(MusicError::BadValue {
            context: "header",
            detail: "score offset + length overflows".into(),
        })?;
    if song_end > data.len() {
        return Err(MusicError::BadValue {
            context: "header",
            detail: format!(
                "score of {song_len} byte(s) at offset {song_offset} ends at {song_end}, \
                 past the end of the {len}-byte input",
                len = data.len()
            ),
        });
    }

    let mut instruments = Vec::with_capacity(instrument_count);
    for i in 0..instrument_count {
        // instruments_end <= song_offset is checked above, so this slice
        // is in bounds
        let at = HEADER_SIZE + i * 2;
        instruments.push(u16::from_le_bytes([data[at], data[at + 1]]));
    }

    let events = parse_score(&data[song_offset..song_end])?;

    Ok(MusSong {
        primary_channels,
        secondary_channels,
        instruments,
        events,
    })
}

/// Parses the event stream out of the score region.
///
/// The delay bytes that follow an event are accumulated into
/// `ticks_from_last` for the *next* event, matching how DMX applies them
/// during playback.
fn parse_score(score: &[u8]) -> Result<Vec<MusEvent>, MusicError> {
    let mut events = Vec::new();
    let mut pos = 0usize;
    let mut pending_delay = 0u32;

    loop {
        let event_byte = *score.get(pos).ok_or(MusicError::Unterminated)?;
        pos += 1;

        let last = event_byte & 0x80 != 0;
        let event_type = (event_byte >> 4) & 0x07;
        let channel = event_byte & 0x0F;

        let mut take = |count: usize| -> Result<&[u8], MusicError> {
            let end = pos.checked_add(count).ok_or(MusicError::BadValue {
                context: "score",
                detail: "event data offset overflows".into(),
            })?;
            let bytes = score.get(pos..end).ok_or(MusicError::Unterminated)?;
            pos = end;
            Ok(bytes)
        };

        let kind = match event_type {
            0 => MusEventKind::ReleaseNote {
                channel,
                note: take(1)?[0] & 0x7F,
            },
            1 => {
                let note_byte = take(1)?[0];
                let note = note_byte & 0x7F;
                let volume = if note_byte & 0x80 != 0 {
                    Some(take(1)?[0] & 0x7F)
                } else {
                    None
                };
                MusEventKind::PlayNote {
                    channel,
                    note,
                    volume,
                }
            }
            2 => MusEventKind::PitchBend {
                channel,
                bend: take(1)?[0],
            },
            3 => MusEventKind::SystemEvent {
                channel,
                code: SystemEvent::from_code(take(1)?[0] & 0x7F)?,
            },
            4 => {
                let controller = take(1)?[0] & 0x7F;
                let value = take(1)?[0] & 0x7F;
                MusEventKind::Controller {
                    channel,
                    controller,
                    value,
                }
            }
            5 => MusEventKind::EndOfMeasure { channel },
            6 => MusEventKind::Finish,
            7 => MusEventKind::Unused {
                channel,
                data: take(1)?[0],
            },
            // the type field is three bits, so every value is already
            // handled above; this arm is unreachable by construction
            _ => unreachable!("3-bit event type cannot escape the match"),
        };

        events.push(MusEvent {
            ticks_from_last: pending_delay,
            kind,
        });
        pending_delay = 0;

        if last {
            pending_delay = decode_delay(score, &mut pos)?;
        }

        if matches!(kind, MusEventKind::Finish) {
            return Ok(events);
        }
    }
}

/// Decodes a variable-length delay starting at `pos`.
///
/// Repeatedly read bytes while the high bit is set; each contributes
/// `delay = delay * 128 + (byte & 0x7F)`.
fn decode_delay(score: &[u8], pos: &mut usize) -> Result<u32, MusicError> {
    let mut delay = 0u32;
    loop {
        let byte = *score.get(*pos).ok_or(MusicError::Unterminated)?;
        *pos += 1;
        delay = delay
            .checked_mul(DELAY_BASE)
            .and_then(|delay| delay.checked_add(u32::from(byte & 0x7F)))
            .ok_or(MusicError::BadValue {
                context: "score",
                detail: "delay overflows 32 bits".into(),
            })?;
        if byte & 0x80 == 0 {
            return Ok(delay);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic MUS file around the given events, computing the
    /// header for us.
    fn build_mus(instruments: &[u16], score: &[u8]) -> Vec<u8> {
        let song_len = score.len() as u16;
        let song_offset = (HEADER_SIZE + instruments.len() * 2) as u16;
        let mut data = Vec::new();
        data.extend_from_slice(MUS_MAGIC);
        data.extend_from_slice(&song_len.to_le_bytes());
        data.extend_from_slice(&song_offset.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // primary channels
        data.extend_from_slice(&0u16.to_le_bytes()); // secondary channels
        data.extend_from_slice(&(instruments.len() as u16).to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // reserved
        for instrument in instruments {
            data.extend_from_slice(&instrument.to_le_bytes());
        }
        data.extend_from_slice(score);
        data
    }

    fn simple_score() -> Vec<u8> {
        // program change (via controller 0) to patch 0, note on, note off, finish
        vec![
            0x40, 0x00, 0x00, // controller 0 (instrument) -> patch 0, channel 0
            0x90, 0xBC,
            0x40, // play note ch0, last flag, note 60 with volume flag set, vel 64
            0x23, // delay 35
            0x00, 0x3C, // release note ch0 note 60
            0x60, // finish
        ]
    }

    #[test]
    fn parses_header_and_instruments() {
        let data = build_mus(&[0, 32, 135], &simple_score());
        let song = parse_mus(&data).unwrap();
        assert_eq!(song.instruments, vec![0, 32, 135]);
        assert_eq!(song.primary_channels, 1);
        assert_eq!(song.secondary_channels, 0);
    }

    #[test]
    fn parses_all_event_kinds() {
        let score = vec![
            0x00, 0x40, // release note note 64
            0x10, 0x41, // play note note 65, no volume byte
            0x11, 0xC2, 0x7F, // play note note 66, volume 127
            0x21, 0x80, // pitch bend 128 (centered) on channel 1
            0x31, 0x0A, // system event all sounds off on channel 1
            0x31, 0x0B, // all notes off
            0x31, 0x0C, // mono
            0x31, 0x0D, // poly
            0x31, 0x0E, // reset all controllers
            0x31, 0x0F, // event (never implemented)
            0x41, 0x00, 0x7F, // controller 0 (instrument) -> 127
            0x41, 0x03, 0x64, // controller 3 (volume) -> 100
            0x50, // end of measure channel 0
            0x70, 0xAB, // unused event, data byte 0x2B
            0x60, // finish
        ];
        let song = parse_mus(&build_mus(&[], &score)).unwrap();
        let kinds: Vec<_> = song.events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                MusEventKind::ReleaseNote {
                    channel: 0,
                    note: 64
                },
                MusEventKind::PlayNote {
                    channel: 0,
                    note: 65,
                    volume: None
                },
                MusEventKind::PlayNote {
                    channel: 1,
                    note: 66,
                    volume: Some(127)
                },
                MusEventKind::PitchBend {
                    channel: 1,
                    bend: 128
                },
                MusEventKind::SystemEvent {
                    channel: 1,
                    code: SystemEvent::AllSoundsOff
                },
                MusEventKind::SystemEvent {
                    channel: 1,
                    code: SystemEvent::AllNotesOff
                },
                MusEventKind::SystemEvent {
                    channel: 1,
                    code: SystemEvent::Mono
                },
                MusEventKind::SystemEvent {
                    channel: 1,
                    code: SystemEvent::Poly
                },
                MusEventKind::SystemEvent {
                    channel: 1,
                    code: SystemEvent::ResetAllControllers
                },
                MusEventKind::SystemEvent {
                    channel: 1,
                    code: SystemEvent::Event
                },
                MusEventKind::Controller {
                    channel: 1,
                    controller: 0,
                    value: 127
                },
                MusEventKind::Controller {
                    channel: 1,
                    controller: 3,
                    value: 100
                },
                MusEventKind::EndOfMeasure { channel: 0 },
                MusEventKind::Unused {
                    channel: 0,
                    data: 0xAB
                },
                MusEventKind::Finish,
            ]
        );
    }

    #[test]
    fn multibyte_delay_accumulates_correctly() {
        // delay 0x82 0x05 = (2 * 128) + 5 = 261
        let score = vec![
            0x91, 0x3C, // note on with last flag (channel 1)
            0x82, 0x05, // delay 261
            0x60, // finish
        ];
        let song = parse_mus(&build_mus(&[], &score)).unwrap();
        assert_eq!(song.events[0].ticks_from_last, 0);
        assert_eq!(song.events[1].ticks_from_last, 261);
    }

    #[test]
    fn delay_of_first_event_is_kept() {
        // a leading delay before the first event is preserved verbatim;
        // DMX would just wait that long before the first note
        let score = vec![
            0x91, 0x3C, 0x03, // note on + delay 3
            0x60,
        ];
        let song = parse_mus(&build_mus(&[], &score)).unwrap();
        assert_eq!(song.events[1].ticks_from_last, 3);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut data = build_mus(&[], &simple_score());
        data[0] = b'X';
        let err = parse_mus(&data).unwrap_err();
        assert!(matches!(err, MusicError::BadMagic { .. }));
    }

    #[test]
    fn truncated_header_is_rejected() {
        let err = parse_mus(b"MUS\x1A\x01").unwrap_err();
        assert!(matches!(err, MusicError::Truncated { .. }));
    }

    #[test]
    fn score_past_end_of_file_is_rejected() {
        let mut data = build_mus(&[], &simple_score());
        // claim a much larger score than we have
        data[4..6].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let err = parse_mus(&data).unwrap_err();
        assert!(matches!(err, MusicError::BadValue { .. }));
    }

    #[test]
    fn instruments_past_score_offset_is_rejected() {
        let mut data = build_mus(&[0], &simple_score());
        // move the score offset back into the header
        data[6..8].copy_from_slice(&4u16.to_le_bytes());
        let err = parse_mus(&data).unwrap_err();
        assert!(matches!(err, MusicError::BadValue { .. }));
    }

    #[test]
    fn missing_finish_is_an_error() {
        let score = vec![0x10, 0x3C]; // note on, then the buffer just ends
        let err = parse_mus(&build_mus(&[], &score)).unwrap_err();
        assert!(matches!(err, MusicError::Unterminated));
    }

    #[test]
    fn event_data_running_past_score_is_an_error() {
        // play-note-with-volume claims a volume byte that is not there
        let score = vec![0x11, 0xBC]; // ch1, note 60, vol flag set, but buffer ends
        let err = parse_mus(&build_mus(&[], &score)).unwrap_err();
        assert!(matches!(err, MusicError::Unterminated));
    }

    #[test]
    fn huge_delay_does_not_overflow() {
        // five 0xFF.. continuation bytes would overflow u32 if unchecked
        let score = vec![
            0x90, 0x3C, // note on with last flag
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F, // delay ~ 2^35 -> overflow
            0x60,
        ];
        let err = parse_mus(&build_mus(&[], &score)).unwrap_err();
        assert!(matches!(err, MusicError::BadValue { .. }));
    }

    #[test]
    fn unknown_system_event_code_is_rejected() {
        let score = vec![0x30, 0x09, 0x60]; // code 9 is not in the DMX table
        let err = parse_mus(&build_mus(&[], &score)).unwrap_err();
        assert!(matches!(err, MusicError::BadValue { .. }));
    }

    #[test]
    fn duration_math_uses_dmx_time_division() {
        let score = vec![
            0x91, 0x3C, 0x81, 0x0C, // note on + delay 140 (1 second)
            0x60,
        ];
        let song = parse_mus(&build_mus(&[], &score)).unwrap();
        assert_eq!(song.total_ticks(), 140);
        assert!((song.duration_seconds() - 1.0).abs() < f64::EPSILON);
        assert_eq!(MUS_TICKS_PER_SECOND, 140);
    }

    #[test]
    fn percussion_patch_maps_to_drum_note() {
        assert_eq!(MusSong::percussion_note(135), Some(35));
        assert_eq!(MusSong::percussion_note(181), Some(81));
        assert_eq!(MusSong::percussion_note(134), None);
        assert_eq!(MusSong::percussion_note(0), None);
        assert_eq!(MusSong::percussion_note(182), None);
    }

    #[test]
    fn instrument_list_is_bounded_by_score_offset() {
        // a lump with a bogus huge instrument count must not read out of bounds
        let mut data = build_mus(&[], &simple_score());
        data[12..14].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let err = parse_mus(&data).unwrap_err();
        assert!(matches!(err, MusicError::BadValue { .. }));
    }
}

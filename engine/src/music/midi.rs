//! Conversion from MUS events to a MIDI-like event stream.
//!
//! [`opl_audio::render`] consumes [`opl_audio::TimedEvent`]s timed in MIDI
//! ticks defined by a `division` (ticks per quarter note) and a `bpm`.
//! DMX ticks are 1/140th of a second, so the natural encoding is
//! `division = 140` with `bpm = 60`: one MIDI tick is exactly one DMX
//! tick and no rescaling is needed anywhere.
//!
//! MUS channels are kept as-is, except that the MUS percussion channel 15
//! maps to MIDI channel 9, which is the channel drum patches are keyed
//! on for the OPL player.
//!
//! ## Instrument-aware note remapping
//!
//! The MUS format does not carry instrument definitions, only the patch
//! numbers listed in the header. When a [`GenmidiBank`] is supplied, the
//! converter plays the instruments the way the DMX OPL driver would:
//!
//! - fixed-pitch instruments always sound their `fixed_note`, regardless
//!   of the key pressed;
//! - other instruments get their voice-0 base note offset added and are
//!   folded back into the 0-95 range in octave steps.
//!
//! Without a bank (fallback playback) notes pass through untouched.
//!
//! Program changes with a patch ≥ 128 on the percussion channel are
//! dropped: DMX selects the drum patch from the note number, which is
//! also how [`opl_audio`] keys its drum patches, and a program number
//! ≥ 128 cannot be represented in MIDI anyway.

use opl_audio::{MidiMsg, TimedEvent};

use crate::music::genmidi::GenmidiBank;
use crate::music::mus::{MusEventKind, MusSong, PERCUSSION_CHANNEL, SystemEvent};

/// MIDI ticks per quarter note used for the conversion.
pub const MUS_DIVISION: u16 = 140;

/// Tempo in beats per minute used for the conversion.
///
/// With [`MUS_DIVISION`] this yields a tick duration of
/// `60 / (60 * 140) = 1/140` seconds — exactly one DMX tick.
pub const MUS_BPM: u32 = 60;

/// Length of one MIDI tick in seconds, for sanity checks.
pub const TICK_SECONDS: f64 = 60.0 / (MUS_BPM as f64 * MUS_DIVISION as f64);

/// Default volume used for notes that do not carry their own volume byte.
pub const DEFAULT_NOTE_VOLUME: u8 = 127;

/// MUS controller number that changes the instrument.
const CONTROLLER_PROGRAM: u8 = 0;

/// MUS controller numbers that have a direct MIDI counterpart.
///
/// The table mirrors the DMX controller definitions; anything not listed
/// here is dropped by the converter.
const CONTROLLER_MAP: [(u8, u8); 9] = [
    (1, 0),  // bank select
    (2, 1),  // modulation
    (3, 7),  // channel volume
    (4, 10), // pan
    (5, 11), // expression
    (6, 91), // reverb depth
    (7, 93), // chorus depth
    (8, 64), // hold pedal
    (9, 67), // soft pedal
];

/// Result of converting a MUS song to a MIDI-like event stream.
#[derive(Clone, Debug, Default)]
pub struct Conversion {
    /// The converted events, in file order.
    pub events: Vec<TimedEvent>,
    /// Number of events skipped by the converter (end of measure,
    /// unused events, unmapped controllers, ...).
    pub skipped: u32,
}

/// Maps a MUS channel to the MIDI channel used for playback.
pub fn midi_channel(mus_channel: u8) -> u8 {
    if mus_channel == PERCUSSION_CHANNEL {
        9
    } else {
        mus_channel
    }
}

/// Converts a MUS song to timed MIDI events.
///
/// `bank`, when supplied, enables instrument-aware note remapping
/// (fixed pitch and base note offset). Pass `None` to let notes pass
/// through unchanged.
pub fn mus_to_midi(song: &MusSong, bank: Option<&GenmidiBank>) -> Conversion {
    let mut events = Vec::with_capacity(song.events.len());
    let mut skipped = 0u32;

    // current program per MUS channel; MUS has no program change event,
    // programs are set through controller 0
    let mut programs = [0u8; 16];
    // last note volume per channel, for play-note events without a volume
    let mut note_volumes = [DEFAULT_NOTE_VOLUME; 16];

    for event in &song.events {
        let delta = event.ticks_from_last;
        let mut push = |msg: MidiMsg| {
            events.push(TimedEvent {
                delta_ticks: delta,
                msg,
            });
        };

        match event.kind {
            MusEventKind::ReleaseNote { channel, note } => {
                push(MidiMsg::NoteOff {
                    ch: midi_channel(channel),
                    note: remap_note(channel, note, &programs, bank),
                });
            }
            MusEventKind::PlayNote {
                channel,
                note,
                volume,
            } => {
                let volume = volume.unwrap_or(note_volumes[usize::from(channel)]);
                note_volumes[usize::from(channel)] = volume;
                push(MidiMsg::NoteOn {
                    ch: midi_channel(channel),
                    note: remap_note(channel, note, &programs, bank),
                    vel: volume,
                });
            }
            MusEventKind::PitchBend { channel, bend } => {
                // MUS bend: 128 is center, 0 one tone down, 255 one tone up.
                // MIDI bend is signed 14-bit; scale so the extremes land
                // on roughly ± one tone (8191 / 64 = 128 steps).
                let value = (i32::from(bend) - 128) * 64;
                push(MidiMsg::PitchBend {
                    ch: midi_channel(channel),
                    value: value as i16,
                });
            }
            MusEventKind::SystemEvent { channel, code } => {
                let cc = match code {
                    SystemEvent::AllSoundsOff => 120,
                    SystemEvent::AllNotesOff => 123,
                    SystemEvent::Mono => 126,
                    SystemEvent::Poly => 127,
                    SystemEvent::ResetAllControllers => 121,
                    // never implemented by DMX; nothing to send
                    SystemEvent::Event => {
                        skipped += 1;
                        continue;
                    }
                };
                push(MidiMsg::Controller {
                    ch: midi_channel(channel),
                    cc,
                    val: 0,
                });
            }
            MusEventKind::Controller {
                channel,
                controller,
                value,
            } => {
                if controller == CONTROLLER_PROGRAM {
                    programs[usize::from(channel)] = value;
                    // drum patches are keyed by note on the percussion
                    // channel; a program number would be meaningless there
                    if midi_channel(channel) != 9 {
                        push(MidiMsg::ProgramChange {
                            ch: midi_channel(channel),
                            program: value,
                        });
                    } else {
                        skipped += 1;
                    }
                    continue;
                }
                match CONTROLLER_MAP.iter().find(|(mus, _)| *mus == controller) {
                    Some(&(_, midi)) => push(MidiMsg::Controller {
                        ch: midi_channel(channel),
                        cc: midi,
                        val: value,
                    }),
                    None => skipped += 1,
                }
            }
            MusEventKind::EndOfMeasure { .. } | MusEventKind::Unused { .. } => {
                skipped += 1;
            }
            MusEventKind::Finish => {
                // the finish event carries no musical meaning for the
                // renderer; the engine loops from the top instead
                skipped += 1;
            }
        }
    }

    Conversion { events, skipped }
}

/// Applies the instrument-aware note remapping for a melodic channel.
///
/// The percussion channel keeps the requested note: drum patches are
/// selected by it, and DMX's fixed "middle C" playback trick does not
/// translate to note-keyed drum banks.
fn remap_note(mus_channel: u8, note: u8, programs: &[u8; 16], bank: Option<&GenmidiBank>) -> u8 {
    if midi_channel(mus_channel) == 9 {
        return note;
    }
    let Some(bank) = bank else {
        return note;
    };
    let program = programs[usize::from(mus_channel)];
    bank.melodic(program).note_for(note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::genmidi::{GenmidiBank, NUM_ENTRIES};
    use crate::music::mus::parse_mus;

    /// Builds a one-instrument-override synthetic GENMIDI bank.
    fn bank_with(program: u8, flags: u16, fixed_note: u8, offset: i16) -> GenmidiBank {
        let mut data = Vec::new();
        data.extend_from_slice(crate::music::genmidi::GENMIDI_MAGIC);
        for index in 0..NUM_ENTRIES {
            let (flags, fixed_note, offset) = if index == usize::from(program) {
                (flags, fixed_note, offset)
            } else {
                (0, 0, 0)
            };
            data.extend_from_slice(&flags.to_le_bytes());
            data.push(128);
            data.push(fixed_note);
            for _ in 0..2 {
                data.extend_from_slice(&[0, 0xF0, 0x05, 0, 0, 0x20]);
                data.push(0);
                data.extend_from_slice(&[0, 0x0F, 0x75, 0, 0, 0x3F]);
                data.push(0);
                data.extend_from_slice(&offset.to_le_bytes());
            }
        }
        GenmidiBank::parse(&data).unwrap()
    }

    /// `opl_audio::MidiMsg` does not implement `PartialEq`, so compare by
    /// debug output — good enough for assertions and keeps the helper
    /// trivial.
    fn assert_msg(actual: MidiMsg, expected: MidiMsg) {
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    }

    fn parse_score(score: &[u8]) -> MusSong {
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
        parse_mus(&data).unwrap()
    }

    #[test]
    fn tick_math_is_exactly_one_over_140_second() {
        assert_eq!(MUS_DIVISION, 140);
        assert_eq!(MUS_BPM, 60);
        assert!((TICK_SECONDS - 1.0 / 140.0).abs() < f64::EPSILON);
    }

    #[test]
    fn percussion_channel_maps_to_midi_9() {
        assert_eq!(midi_channel(15), 9);
        for ch in 0..15 {
            assert_eq!(midi_channel(ch), ch);
        }
    }

    #[test]
    fn delta_ticks_are_preserved() {
        let song = parse_score(&[
            0x90, 0x3C, // note on + last flag
            0x81, 0x0C, // delay 140 (1*128 + 12)
            0x00, 0x3C, // release note
            0x60, // finish
        ]);
        let out = mus_to_midi(&song, None);
        assert_eq!(out.events[0].delta_ticks, 0);
        assert_eq!(out.events[1].delta_ticks, 140);
        assert_msg(out.events[1].msg, MidiMsg::NoteOff { ch: 0, note: 60 });
        assert_eq!(out.skipped, 1); // finish
    }

    #[test]
    fn controller_zero_becomes_program_change() {
        let song = parse_score(&[
            0x40, 0x00, 0x07, // controller 0 -> program 7
            0x60,
        ]);
        let out = mus_to_midi(&song, None);
        assert_msg(
            out.events[0].msg,
            MidiMsg::ProgramChange { ch: 0, program: 7 },
        );
    }

    #[test]
    fn percussion_program_change_is_dropped() {
        let song = parse_score(&[
            0x4F, 0x00, 0x7F, // controller 0 on percussion channel
            0x60,
        ]);
        let out = mus_to_midi(&song, None);
        assert_eq!(out.events.len(), 0);
        assert_eq!(out.skipped, 2); // the program change + finish
    }

    #[test]
    fn controllers_map_to_midi_cc() {
        let song = parse_score(&[
            0x40, 0x03, 0x40, // volume -> CC7
            0x40, 0x04, 0x40, // pan -> CC10
            0x40, 0x08, 0x01, // hold pedal -> CC64
            0x40, 0x7E, 0x01, // unmapped controller -> skipped
            0x60,
        ]);
        let out = mus_to_midi(&song, None);
        let ccs: Vec<u8> = out
            .events
            .iter()
            .map(|e| match e.msg {
                MidiMsg::Controller { cc, .. } => cc,
                _ => panic!("expected controller"),
            })
            .collect();
        assert_eq!(ccs, vec![7, 10, 64]);
        assert_eq!(out.skipped, 2); // unmapped controller + finish
    }

    #[test]
    fn system_events_map_to_all_notes_off_and_friends() {
        for (code, expected_cc) in [(10, 120), (11, 123), (12, 126), (13, 127), (14, 121)] {
            // build through the parser so the mapping is exercised end to end
            let song = parse_score(&[0x30, code, 0x60]);
            let out = mus_to_midi(&song, None);
            assert_msg(
                out.events[0].msg,
                MidiMsg::Controller {
                    ch: 0,
                    cc: expected_cc,
                    val: 0,
                },
            );
        }
    }

    #[test]
    fn system_event_15_is_skipped() {
        let song = parse_score(&[0x30, 0x0F, 0x60]);
        let out = mus_to_midi(&song, None);
        assert!(out.events.is_empty());
        assert_eq!(out.skipped, 2);
    }

    #[test]
    fn note_without_volume_reuses_channel_volume() {
        let song = parse_score(&[
            0x10, 0xBC, 0x40, // play note note 60, volume 64
            0x00, 0xBC, // release note
            0x10, 0x3C, // play note note 60, NO volume byte
            0x60,
        ]);
        let out = mus_to_midi(&song, None);
        assert_msg(
            out.events[0].msg,
            MidiMsg::NoteOn {
                ch: 0,
                note: 60,
                vel: 64,
            },
        );
        assert_msg(
            out.events[2].msg,
            MidiMsg::NoteOn {
                ch: 0,
                note: 60,
                vel: 64,
            },
        );
    }

    #[test]
    fn pitch_bend_scales_to_14_bit() {
        let song = parse_score(&[
            0x20, 128, // centered
            0x20, 0, // one tone down
            0x20, 255, // one tone up
            0x60,
        ]);
        let out = mus_to_midi(&song, None);
        let values: Vec<i16> = out
            .events
            .iter()
            .map(|e| match e.msg {
                MidiMsg::PitchBend { value, .. } => value,
                _ => panic!("expected pitch bend"),
            })
            .collect();
        assert_eq!(values, vec![0, -8192, 8128]);
    }

    #[test]
    fn fixed_pitch_instrument_substitutes_its_note() {
        // program 0 = fixed pitch at note 42
        let bank = bank_with(0, 0x0001, 42, 0);
        let song = parse_score(&[
            0x40, 0x00, 0x00, // program 0
            0x10, 0x3C, // note 60
            0x00, 0x2A, // release (note 42 after remap)
            0x60,
        ]);
        let out = mus_to_midi(&song, Some(&bank));
        assert_msg(
            out.events[1].msg,
            MidiMsg::NoteOn {
                ch: 0,
                note: 42,
                vel: DEFAULT_NOTE_VOLUME,
            },
        );
        assert_msg(out.events[2].msg, MidiMsg::NoteOff { ch: 0, note: 42 });
    }

    #[test]
    fn base_note_offset_is_applied_and_folded() {
        // program 0 = -12 offset
        let bank = bank_with(0, 0, 0, -12);
        let song = parse_score(&[
            0x40, 0x00, 0x00, // program 0
            0x10, 0x3C, // note 60 -> 48
            0x60,
        ]);
        let out = mus_to_midi(&song, Some(&bank));
        assert_msg(
            out.events[1].msg,
            MidiMsg::NoteOn {
                ch: 0,
                note: 48,
                vel: DEFAULT_NOTE_VOLUME,
            },
        );
    }

    #[test]
    fn drum_channel_notes_are_not_remapped() {
        // fixed-pitch melodic patch should not touch the percussion channel
        let bank = bank_with(0, 0x0001, 42, -12);
        let song = parse_score(&[
            0x50 | 0x0F, // end of measure on percussion channel (0x5F)
            0x1F,
            0x3C, // note on, percussion channel
            0x60,
        ]);
        let out = mus_to_midi(&song, Some(&bank));
        assert_msg(
            out.events[0].msg,
            MidiMsg::NoteOn {
                ch: 9,
                note: 60,
                vel: DEFAULT_NOTE_VOLUME,
            },
        );
    }

    #[test]
    fn conversion_counts_skipped_events() {
        let song = parse_score(&[
            0x50, // end of measure
            0x70, 0x01, // unused
            0x60, // finish
        ]);
        let out = mus_to_midi(&song, None);
        assert_eq!(out.events.len(), 0);
        assert_eq!(out.skipped, 3);
    }
}

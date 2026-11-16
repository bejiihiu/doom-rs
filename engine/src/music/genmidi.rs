//! The GENMIDI lump: the DMX OPL instrument bank.
//!
//! GENMIDI is the instrument database the DMX library uses to program an
//! OPL2/OPL3 chip. It lives in the WAD as the `GENMIDI` lump and holds
//! 175 entries: 128 melodic (General MIDI programs 0-127) followed by
//! 47 percussion instruments, one per GM drum note (35-81).
//!
//! ## Lump layout
//!
//! | Offset | Size    | Field                                  |
//! | ------ | ------- | -------------------------------------- |
//! | 0      | 8       | Magic, `#OPL_II#`                      |
//! | 8      | 175*36  | Instrument records                      |
//! | 6308   | 175*32  | Optional 32-byte name strings          |
//!
//! Each 36-byte instrument record is:
//!
//! | Offset | Size | Field                                        |
//! | ------ | ---- | -------------------------------------------- |
//! | 0      | 2    | Flags, little-endian                         |
//! | 2      | 1    | Fine tune (128 = center; voice 2 only)       |
//! | 3      | 1    | Note played by fixed-pitch instruments        |
//! | 4      | 16   | Voice 0                                      |
//! | 20     | 16   | Voice 1 (only used when `FLAG_2VOICE` is set) |
//!
//! Flags:
//!
//! - `0x0001` — fixed pitch: always play `fixed_note`, ignore the key
//! - `0x0002` — unknown (used by one instrument in the Doom lump, ignored
//!   by DMX itself)
//! - `0x0004` — double voice: play both voices at once
//!
//! Each 16-byte voice is a modulator operator (6 bytes), a feedback byte,
//! a carrier operator (6 bytes), an unused byte and a signed 16-bit
//! little-endian base note offset. The operator bytes map onto the OPL
//! chip's registers almost directly; see [`GenmidiOperator::to_opl`].
//!
//! ## Notes on the conversion to an OPL bank
//!
//! [`opl_audio::OplBank`] plays a *single* 2-operator patch per program,
//! so double-voice instruments are rendered with their first voice only.
//! This matches what hardware with a single FM bank would produce and is
//! the documented trade-off of this path; a future OPL3 stereo voice per
//! instrument pair would be needed for the full effect.
//!
//! Percussion patches are registered per drum note (35-81), following how
//! General MIDI and [`opl_audio`] address channel-10 drums. The base note
//! offset and fixed-pitch semantics of melodic instruments are applied by
//! the MIDI converter, which tracks the current instrument per channel.

use opl_audio::{OplBank, OplOperator, OplPatch};

use crate::music::error::MusicError;

/// Magic bytes at the start of a GENMIDI lump.
pub const GENMIDI_MAGIC: &[u8; 8] = b"#OPL_II#";

/// Number of melodic instruments in the lump.
pub const NUM_MELODIC: usize = 128;

/// Number of percussion instruments in the lump.
pub const NUM_PERCUSSION: usize = 47;

/// Total number of instrument entries in the lump.
pub const NUM_ENTRIES: usize = NUM_MELODIC + NUM_PERCUSSION;

/// Lowest GM drum note covered by the percussion instruments.
pub const DRUM_NOTE_MIN: u8 = 35;

/// Size of one operator record in bytes.
const OPERATOR_SIZE: usize = 6;
/// Size of one voice record in bytes.
const VOICE_SIZE: usize = 16;
/// Size of one instrument record in bytes.
const INSTRUMENT_SIZE: usize = 36;
/// Size of one name field in bytes.
const NAME_SIZE: usize = 32;
/// Size of the fixed header.
const HEADER_SIZE: usize = 8;

/// Fixed pitch instrument: always plays [`GenmidiInstrument::fixed_note`].
pub const FLAG_FIXED_PITCH: u16 = 0x0001;
/// Unknown flag, set on one instrument in the Doom lump and unused by DMX.
pub const FLAG_UNKNOWN: u16 = 0x0002;
/// Double voice: both voices are played simultaneously.
pub const FLAG_DOUBLE_VOICE: u16 = 0x0004;

/// One operator of a 2-operator FM patch, as stored in GENMIDI.
///
/// The six bytes map nearly one-to-one onto the OPL chip's registers;
/// [`GenmidiOperator::to_opl`] performs the unpacking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GenmidiOperator {
    /// Tremolo / vibrato / sustain / KSR / multiplier byte.
    pub tremolo: u8,
    /// Attack rate (high nibble) / decay rate (low nibble).
    pub attack: u8,
    /// Sustain level (high nibble) / release rate (low nibble).
    pub sustain: u8,
    /// Waveform select (OPL2 uses the low two bits).
    pub waveform: u8,
    /// Key-scale level, stored in the high two bits.
    pub scale: u8,
    /// Output level, stored in the low six bits.
    pub level: u8,
}

impl GenmidiOperator {
    /// Unpacks the GENMIDI bytes into an [`OplOperator`].
    ///
    /// GENMIDI splits the chip's combined KSL/TL level byte into two
    /// separate bytes; the KSL depth lives in the high bits of `scale`
    /// and the total level in the low six bits of `level`. The envelope
    /// generator type (bit 5 of the tremolo byte) selects between
    /// sustaining (`1`) and percussive (`0`) envelopes.
    pub fn to_opl(&self) -> OplOperator {
        OplOperator {
            mult: self.tremolo & 0x0F,
            am: self.tremolo & 0x80 != 0,
            vib: self.tremolo & 0x40 != 0,
            egt: self.tremolo & 0x20 != 0,
            ksr: self.tremolo & 0x10 != 0,
            attack: (self.attack >> 4) & 0x0F,
            decay: self.attack & 0x0F,
            sustain: (self.sustain >> 4) & 0x0F,
            release: self.sustain & 0x0F,
            waveform: self.waveform & 0x03,
            ksl: (self.scale & 0xC0) >> 6,
            total_level: self.level & 0x3F,
        }
    }
}

/// One voice (modulator + carrier pair) of an instrument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GenmidiVoice {
    /// The modulating operator.
    pub modulator: GenmidiOperator,
    /// Feedback/connection byte: bit 0 is the connection mode
    /// (0 = FM, 1 = AM), bits 1-3 the modulator self-feedback.
    pub feedback: u8,
    /// The carrier operator.
    pub carrier: GenmidiOperator,
    /// Note offset applied to the MIDI note number (signed,
    /// e.g. -12 for one octave down). Ignored for fixed-pitch
    /// instruments.
    pub base_note_offset: i16,
}

impl GenmidiVoice {
    /// Converts the voice into an [`OplPatch`].
    pub fn to_opl_patch(&self) -> OplPatch {
        OplPatch {
            modulator: self.modulator.to_opl(),
            carrier: self.carrier.to_opl(),
            feedback: (self.feedback >> 1) & 0x07,
            connection: self.feedback & 0x01,
        }
    }
}

/// One instrument entry of the GENMIDI bank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenmidiInstrument {
    /// Raw flags bitfield (see the module docs).
    pub flags: u16,
    /// Fine tuning for the second voice of double-voice instruments.
    /// 128 is center; it does not affect single-voice playback.
    pub fine_tuning: u8,
    /// Note played by fixed-pitch instruments (flags & [`FLAG_FIXED_PITCH`]).
    pub fixed_note: u8,
    /// The two voices; only `voices[0]` is used unless the instrument is
    /// double-voiced.
    pub voices: [GenmidiVoice; 2],
    /// Optional name from the trailing name block, ASCII-trimmed.
    pub name: Option<String>,
}

impl GenmidiInstrument {
    /// Whether this instrument always plays the same note.
    pub fn is_fixed_pitch(&self) -> bool {
        self.flags & FLAG_FIXED_PITCH != 0
    }

    /// Whether this instrument plays both voices at once.
    pub fn is_double_voice(&self) -> bool {
        self.flags & FLAG_DOUBLE_VOICE != 0
    }

    /// Returns the note this instrument should sound for the requested
    /// MIDI note, applying fixed-pitch and base-offset semantics.
    ///
    /// Fixed-pitch instruments always return `fixed_note`. Other
    /// instruments get `note + base_note_offset`, folded back into the
    /// 0-95 range the OPL frequency table covers, in octave steps, as
    /// the original DMX driver does.
    pub fn note_for(&self, note: u8) -> u8 {
        if self.is_fixed_pitch() {
            return self.fixed_note.min(127);
        }
        let mut folded = i32::from(note) + i32::from(self.voices[0].base_note_offset);
        while folded < 0 {
            folded += 12;
        }
        while folded > 95 {
            folded -= 12;
        }
        folded as u8
    }
}

/// A parsed GENMIDI bank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenmidiBank {
    instruments: [GenmidiInstrument; NUM_ENTRIES],
}

impl GenmidiBank {
    /// Parses a GENMIDI lump.
    ///
    /// The header magic is required, as are all 175 instrument records.
    /// The trailing name block is optional: it is parsed when enough bytes
    /// are present and ignored otherwise, matching what DMX accepts.
    pub fn parse(data: &[u8]) -> Result<Self, MusicError> {
        if data.len() < HEADER_SIZE {
            return Err(MusicError::Truncated {
                needed: HEADER_SIZE,
                available: data.len(),
            });
        }
        if &data[..HEADER_SIZE] != GENMIDI_MAGIC {
            return Err(MusicError::BadMagic {
                expected: "#OPL_II#",
                found: data[..HEADER_SIZE].to_vec(),
            });
        }

        let records_end = HEADER_SIZE + NUM_ENTRIES * INSTRUMENT_SIZE;
        if data.len() < records_end {
            return Err(MusicError::Truncated {
                needed: records_end,
                available: data.len(),
            });
        }

        // names are optional; grab as many complete ones as are present
        let mut names: [Option<String>; NUM_ENTRIES] = [const { None }; NUM_ENTRIES];
        let mut at = records_end;
        for slot in &mut names {
            if data.len() < at + NAME_SIZE {
                break;
            }
            let raw = &data[at..at + NAME_SIZE];
            let trimmed: Vec<u8> = raw.iter().copied().take_while(|&b| b != 0).collect();
            *slot = Some(String::from_utf8_lossy(&trimmed).trim_end().to_owned());
            at += NAME_SIZE;
        }

        let mut offset = HEADER_SIZE;
        let mut instruments = Vec::with_capacity(NUM_ENTRIES);
        for name in &names {
            let record = &data[offset..offset + INSTRUMENT_SIZE];
            instruments.push(parse_instrument(record, name.clone()));
            offset += INSTRUMENT_SIZE;
        }
        // length was checked above, so the collect cannot fail
        let instruments: [GenmidiInstrument; NUM_ENTRIES] = instruments
            .try_into()
            .expect("parsed exactly NUM_ENTRIES records");

        Ok(Self { instruments })
    }

    /// The 128 melodic instruments (General MIDI programs 0-127).
    pub fn melodic(&self, program: u8) -> &GenmidiInstrument {
        &self.instruments[usize::from(program)]
    }

    /// The percussion instrument for a GM drum note (35-81).
    pub fn percussion(&self, note: u8) -> Option<&GenmidiInstrument> {
        if (DRUM_NOTE_MIN..=81).contains(&note) {
            Some(&self.instruments[NUM_MELODIC + usize::from(note - DRUM_NOTE_MIN)])
        } else {
            None
        }
    }

    /// Builds the [`OplBank`] used for playback.
    ///
    /// Melodic instruments become programs 0-127; percussion instruments
    /// become channel-10 drum patches keyed by note (35-81). Note that
    /// the bank cannot express double voices, so only the first voice of
    /// double-voice instruments is loaded (see the module docs).
    pub fn to_opl_bank(&self) -> OplBank {
        let mut bank = OplBank::new();
        for (index, instrument) in self.instruments[..NUM_MELODIC].iter().enumerate() {
            bank.insert_program(index as u8, instrument.voices[0].to_opl_patch());
        }
        for (index, instrument) in self.instruments[NUM_MELODIC..].iter().enumerate() {
            let note = DRUM_NOTE_MIN + index as u8;
            bank.insert_drum(note, instrument.voices[0].to_opl_patch());
        }
        bank
    }

    /// A generic fallback bank used when the `GENMIDI` lump is missing.
    ///
    /// The IWADs ship a GENMIDI lump, but custom WADs sometimes drop it.
    /// Rather than fail playback, we synthesize one plain, documented
    /// patch — a bright, sustaining two-operator voice with instant
    /// attack — and use it for every melodic program and drum note.
    /// This is *not* a General MIDI emulation: every instrument sounds
    /// the same, and percussion is melodic-sounding FM rather than
    /// actual drums. It exists so the music is audible, not so it is
    /// accurate.
    pub fn fallback_bank() -> OplBank {
        let op = |mult: u8, total_level: u8| OplOperator {
            mult,
            total_level,
            attack: 15,
            decay: 0,
            sustain: 0,
            release: 7,
            waveform: 0,
            ksr: false,
            egt: true,
            am: false,
            vib: false,
            ksl: 0,
        };
        let patch = OplPatch {
            modulator: op(1, 20),
            carrier: op(1, 0),
            feedback: 0,
            connection: 0,
        };
        let mut bank = OplBank::new();
        for program in 0..=127 {
            bank.insert_program(program, patch.clone());
        }
        for note in DRUM_NOTE_MIN..=81 {
            bank.insert_drum(note, patch.clone());
        }
        bank
    }
}

fn parse_instrument(record: &[u8], name: Option<String>) -> GenmidiInstrument {
    debug_assert_eq!(record.len(), INSTRUMENT_SIZE);
    let flags = u16::from_le_bytes([record[0], record[1]]);
    let fine_tuning = record[2];
    let fixed_note = record[3];
    GenmidiInstrument {
        flags,
        fine_tuning,
        fixed_note,
        voices: [
            parse_voice(&record[4..4 + VOICE_SIZE]),
            parse_voice(&record[4 + VOICE_SIZE..]),
        ],
        name,
    }
}

fn parse_voice(record: &[u8]) -> GenmidiVoice {
    debug_assert_eq!(record.len(), VOICE_SIZE);
    GenmidiVoice {
        modulator: parse_operator(&record[..OPERATOR_SIZE]),
        feedback: record[OPERATOR_SIZE],
        carrier: parse_operator(&record[OPERATOR_SIZE + 1..2 * OPERATOR_SIZE + 1]),
        base_note_offset: i16::from_le_bytes([record[14], record[15]]),
    }
}

fn parse_operator(record: &[u8]) -> GenmidiOperator {
    debug_assert_eq!(record.len(), OPERATOR_SIZE);
    GenmidiOperator {
        tremolo: record[0],
        attack: record[1],
        sustain: record[2],
        waveform: record[3],
        scale: record[4],
        level: record[5],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic GENMIDI lump from per-entry byte overrides.
    fn build_genmidi(
        flags: u16,
        fine_tuning: u8,
        fixed_note: u8,
        base_offset: i16,
        names: bool,
    ) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(GENMIDI_MAGIC);
        for _ in 0..NUM_ENTRIES {
            data.extend_from_slice(&flags.to_le_bytes());
            data.push(fine_tuning);
            data.push(fixed_note);
            for voice in 0..2 {
                // modulator: deterministic, easy to check
                data.extend_from_slice(&[0x41, 0xF0, 0x05, 0x01, 0x40, 0x20]);
                data.push(0x0E + voice as u8); // feedback: FM, fb 7, connection bit clear
                // carrier
                data.extend_from_slice(&[0x01, 0x0F, 0x75, 0x00, 0xC0, 0x3F]);
                data.push(0x00); // unused
                data.extend_from_slice(&base_offset.to_le_bytes());
            }
        }
        if names {
            for index in 0..NUM_ENTRIES {
                let mut name = [0u8; NAME_SIZE];
                let text = format!("inst{index}");
                name[..text.len()].copy_from_slice(text.as_bytes());
                data.extend_from_slice(&name);
            }
        }
        data
    }

    #[test]
    fn parses_a_complete_lump_with_names() {
        let data = build_genmidi(FLAG_DOUBLE_VOICE, 130, 60, -12, true);
        let bank = GenmidiBank::parse(&data).unwrap();
        let first = bank.melodic(0);
        assert_eq!(first.flags, FLAG_DOUBLE_VOICE);
        assert_eq!(first.fine_tuning, 130);
        assert_eq!(first.fixed_note, 60);
        assert_eq!(first.voices[0].base_note_offset, -12);
        assert_eq!(first.name.as_deref(), Some("inst0"));
        assert_eq!(bank.melodic(127).name.as_deref(), Some("inst127"));
        assert_eq!(
            bank.percussion(35).unwrap().name.as_deref(),
            Some("inst128")
        );
    }

    #[test]
    fn names_are_optional() {
        let data = build_genmidi(0, 128, 0, 0, false);
        let bank = GenmidiBank::parse(&data).unwrap();
        assert_eq!(bank.melodic(0).name, None);
        assert_eq!(bank.percussion(69).unwrap().name, None);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut data = build_genmidi(0, 128, 0, 0, false);
        data[0] = b'X';
        let err = GenmidiBank::parse(&data).unwrap_err();
        assert!(matches!(err, MusicError::BadMagic { .. }));
    }

    #[test]
    fn truncated_records_are_rejected() {
        let data = build_genmidi(0, 128, 0, 0, false);
        let records_end = HEADER_SIZE + NUM_ENTRIES * INSTRUMENT_SIZE;
        let err = GenmidiBank::parse(&data[..records_end - 1]).unwrap_err();
        assert!(matches!(err, MusicError::Truncated { .. }));
    }

    #[test]
    fn operator_bytes_unpack_into_opl_registers() {
        let op = GenmidiOperator {
            tremolo: 0xFF,  // AM, vib, EGT, KSR, mult 15
            attack: 0xA5,   // attack 10, decay 5
            sustain: 0x37,  // sustain 3, release 7
            waveform: 0x07, // OPL2 keeps the low two bits
            scale: 0xC0,    // KSL 3 in the high bits
            level: 0x3F,    // total level 63
        };
        let opl = op.to_opl();
        assert_eq!(opl.mult, 15);
        assert!(opl.am && opl.vib && opl.egt && opl.ksr);
        assert_eq!(opl.attack, 10);
        assert_eq!(opl.decay, 5);
        assert_eq!(opl.sustain, 3);
        assert_eq!(opl.release, 7);
        assert_eq!(opl.waveform, 3);
        assert_eq!(opl.ksl, 3);
        assert_eq!(opl.total_level, 63);
    }

    #[test]
    fn voice_feedback_byte_unpacks_connection_and_feedback() {
        let fm7 = GenmidiVoice {
            feedback: 0x0E, // FM, feedback 7
            ..GenmidiVoice::default()
        };
        let patch = fm7.to_opl_patch();
        assert_eq!(patch.connection, 0);
        assert_eq!(patch.feedback, 7);
        let am7 = GenmidiVoice {
            feedback: 0x0F, // AM, feedback 7
            ..GenmidiVoice::default()
        };
        let patch = am7.to_opl_patch();
        assert_eq!(patch.connection, 1);
        assert_eq!(patch.feedback, 7);
        let fm1 = GenmidiVoice {
            feedback: 0x02, // FM, feedback 1
            ..GenmidiVoice::default()
        };
        let patch = fm1.to_opl_patch();
        assert_eq!(patch.connection, 0);
        assert_eq!(patch.feedback, 1);
    }

    #[test]
    fn fixed_pitch_instruments_ignore_the_key() {
        let data = build_genmidi(FLAG_FIXED_PITCH, 128, 42, 7, false);
        let bank = GenmidiBank::parse(&data).unwrap();
        assert_eq!(bank.melodic(0).note_for(0), 42);
        assert_eq!(bank.melodic(0).note_for(127), 42);
    }

    #[test]
    fn base_note_offset_folds_into_opl_range() {
        let data = build_genmidi(0, 128, 0, -12, false);
        let bank = GenmidiBank::parse(&data).unwrap();
        assert_eq!(bank.melodic(0).note_for(60), 48);
        // offset larger than one octave folds down in octave steps
        let low = GenmidiBank::parse(&build_genmidi(0, 128, 0, -72, false)).unwrap();
        let note = low.melodic(0).note_for(60);
        assert_eq!(note, 0);
        // a note driven below zero by the offset wraps back up by an octave
        let wrap = GenmidiBank::parse(&build_genmidi(0, 128, 0, -12, false)).unwrap();
        assert_eq!(wrap.melodic(0).note_for(0), 0);
        // huge positive offset folds into range as well
        let high = GenmidiBank::parse(&build_genmidi(0, 128, 0, 200, false)).unwrap();
        let note = high.melodic(0).note_for(60);
        assert!(note <= 95);
        assert_eq!((note as i32 - 60 - 200) % 12, 0);
    }

    #[test]
    fn opl_bank_covers_melodic_and_percussion() {
        let data = build_genmidi(0, 128, 0, 0, false);
        let bank = GenmidiBank::parse(&data).unwrap();
        let opl = bank.to_opl_bank();
        assert!(opl.program(0).is_some());
        assert!(opl.program(127).is_some());
        assert!(opl.drum(35).is_some());
        assert!(opl.drum(81).is_some());
        assert!(opl.drum(82).is_none());
    }

    #[test]
    fn percussion_lookup_uses_drum_note_range() {
        let data = build_genmidi(0, 128, 0, 0, false);
        let bank = GenmidiBank::parse(&data).unwrap();
        let inst = bank.percussion(40).unwrap();
        assert_eq!(inst.flags, 0);
        assert!(bank.percussion(34).is_none());
        assert!(bank.percussion(82).is_none());
    }

    #[test]
    fn fallback_bank_is_fully_populated() {
        let bank = GenmidiBank::fallback_bank();
        assert!(bank.program(0).is_some());
        assert!(bank.program(127).is_some());
        assert!(bank.drum(35).is_some());
        assert!(bank.drum(81).is_some());
    }

    #[test]
    fn instrument_count_matches_spec() {
        assert_eq!(NUM_MELODIC + NUM_PERCUSSION, NUM_ENTRIES);
        assert_eq!(NUM_ENTRIES, 175);
        assert_eq!(INSTRUMENT_SIZE, 36);
        assert_eq!(VOICE_SIZE, 16);
        assert_eq!(OPERATOR_SIZE, 6);
        assert_eq!(
            HEADER_SIZE + NUM_ENTRIES * INSTRUMENT_SIZE + NUM_ENTRIES * NAME_SIZE,
            11908
        );
    }
}

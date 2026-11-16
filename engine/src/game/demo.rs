//! The `.lmp` demo format, ported from the demo half of `g_game.c`
//! (`G_ReadDemoTiccmd`, `G_WriteDemoTiccmd`, `G_RecordDemo`,
//! `G_BeginRecording`, `G_PlayDemo`, `G_CheckDemoStatus`) and the
//! DEMOMARKER convention in `d_event.h` terms.
//!
//! A demo is a deterministic input script: a small header naming the
//! skill, episode, map and the intended players, followed by one 4-byte
//! tic command per game tic, terminated by the `0x80` marker. Demos do
//! not store the level state - playback works because the game is
//! deterministic.
//!
//! Reading a demo is deliberately byte-exact: [`DemoPlayback::read_ticcmd`]
//! consumes exactly the 4 bytes [`DemoRecording::write_ticcmd`] produced,
//! including the quantized `(angleturn + 128) >> 8` byte. Vanilla re-read
//! a freshly written tic through `G_ReadDemoTiccmd` to force this
//! quantization at record time; we do the same in
//! [`DemoRecording::write_and_verify`].

use crate::errors::{EngineError, EngineResult};
use crate::player::TicCmd;

/// `VERSION` from `doomdef.h`: vanilla demos are stamped `110`.
pub const DEMO_VERSION: u8 = 110;
/// `DEMOMARKER`: `0x80` marks the end of the command stream.
pub const DEMO_MARKER: u8 = 0x80;
/// Maximum bytes a demo may occupy (`0x20000` in vanilla `G_RecordDemo`).
pub const MAX_DEMO_SIZE: usize = 0x20000;
/// Header length: version, skill, episode, map, deathmatch, respawn,
/// fast, nomonsters, consoleplayer, playeringame[4].
pub const DEMO_HEADER_LEN: usize = 13;

/// The header baked into every recorded demo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DemoHeader {
    /// Difficulty (`skill_t` as a raw index).
    pub skill: u8,
    /// Episode (1-based; ignored by commercial games).
    pub episode: u8,
    /// Map (1-based).
    pub map: u8,
    /// Deathmatch mode (0 = off, 1 = duel, 2 = alt).
    pub deathmatch: u8,
    /// `-respawn`.
    pub respawn_monsters: u8,
    /// `-fast`.
    pub fast_monsters: u8,
    /// `-nomonsters`.
    pub no_monsters: u8,
    /// Console player index.
    pub console_player: u8,
    /// Which player slots are in the game.
    pub playeringame: [u8; 4],
}

impl DemoHeader {
    /// Serialize the 13 header bytes, vanilla order.
    fn encode(&self) -> [u8; DEMO_HEADER_LEN] {
        let mut out = [0u8; DEMO_HEADER_LEN];
        out[0] = DEMO_VERSION;
        out[1] = self.skill;
        out[2] = self.episode;
        out[3] = self.map;
        out[4] = self.deathmatch;
        out[5] = self.respawn_monsters;
        out[6] = self.fast_monsters;
        out[7] = self.no_monsters;
        out[8] = self.console_player;
        out[9..13].copy_from_slice(&self.playeringame);
        out
    }

    /// Parse and validate a header, vanilla style (only the version
    /// byte is strictly checked - everything else is taken as-is).
    fn decode(bytes: &[u8]) -> EngineResult<Self> {
        if bytes.len() < DEMO_HEADER_LEN {
            return Err(EngineError::Resource(format!(
                "demo too short: {} byte(s), need at least {DEMO_HEADER_LEN}",
                bytes.len()
            )));
        }
        if bytes[0] != DEMO_VERSION {
            return Err(EngineError::Resource(format!(
                "demo is from a different game version (byte {} != {DEMO_VERSION})",
                bytes[0]
            )));
        }
        Ok(Self {
            skill: bytes[1],
            episode: bytes[2],
            map: bytes[3],
            deathmatch: bytes[4],
            respawn_monsters: bytes[5],
            fast_monsters: bytes[6],
            no_monsters: bytes[7],
            console_player: bytes[8],
            playeringame: [bytes[9], bytes[10], bytes[11], bytes[12]],
        })
    }
}

/// The record path, ported from `G_RecordDemo`/`G_BeginRecording`/
/// `G_WriteDemoTiccmd`/`G_CheckDemoStatus`.
///
/// The buffer is allocated up front (`MAX_DEMO_SIZE`); when it fills up,
/// recording stops and the game reports the problem instead of
/// overflowing.
pub struct DemoRecording {
    buffer: Vec<u8>,
    /// Write cursor into `buffer`.
    cursor: usize,
    /// The file name the demo will be flushed to (without directory).
    name: String,
}

impl DemoRecording {
    /// Begin a recording. `name` is the plain file name the caller will
    /// hand to [`Self::finish`]; nothing is written to disk until then.
    pub fn new(name: impl Into<String>, header: &DemoHeader) -> Self {
        let header_bytes = header.encode();
        let mut buffer = Vec::with_capacity(MAX_DEMO_SIZE);
        buffer.extend_from_slice(&header_bytes);
        Self {
            buffer,
            cursor: DEMO_HEADER_LEN,
            name: name.into(),
        }
    }

    /// The name the demo was recorded with.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The bytes so far (header included), for inspection in tests.
    pub fn bytes(&self) -> &[u8] {
        &self.buffer
    }

    /// `G_WriteDemoTiccmd`: append one command. Returns `false` when the
    /// buffer is full - the game loop then stops recording.
    ///
    /// The bytes written are quantized exactly like the bytes a reader
    /// would produce, so a written demo plays back bit-identically.
    pub fn write_ticcmd(&mut self, cmd: &TicCmd) -> bool {
        if self.cursor + 4 > MAX_DEMO_SIZE {
            return false;
        }
        self.buffer.push(cmd.forward_move as u8);
        self.buffer.push(cmd.side_move as u8);
        self.buffer.push(((cmd.angle_turn as i32 + 128) >> 8) as u8);
        self.buffer.push(cmd.buttons);
        self.cursor += 4;
        true
    }

    /// Serialize the finished demo: the tics recorded so far, plus the
    /// terminator. Consumes the recording.
    pub fn finish(mut self) -> Vec<u8> {
        self.buffer.push(DEMO_MARKER);
        self.buffer
    }
}

/// The playback path, ported from `G_PlayDemo`/`G_ReadDemoTiccmd`.
pub struct DemoPlayback {
    bytes: Vec<u8>,
    /// Read cursor into `bytes`.
    cursor: usize,
    /// The parsed header.
    header: DemoHeader,
    /// True once the `DEMOMARKER` byte has been seen or the stream ran
    /// out - either way, the next tic is the end of the demo.
    finished: bool,
}

impl DemoPlayback {
    /// Load and validate a demo from raw bytes (the `WAD` lump content
    /// or a file). The header is parsed eagerly so a bad demo fails
    /// before the level is built.
    pub fn new(bytes: Vec<u8>) -> EngineResult<Self> {
        let header = DemoHeader::decode(&bytes)?;
        Ok(Self {
            bytes,
            cursor: DEMO_HEADER_LEN,
            header,
            finished: false,
        })
    }

    /// The header this demo will start a game with.
    pub fn header(&self) -> DemoHeader {
        self.header
    }

    /// `G_ReadDemoTiccmd`: consume the next tic command. When the
    /// marker (or the end of the buffer) is reached, the demo is
    /// finished - the caller notices via [`Self::finished`] and stops
    /// the playback. Returns a zeroed command when finished, like
    /// vanilla's zeroed cmd on marker hit.
    pub fn read_ticcmd(&mut self) -> TicCmd {
        if self.finished {
            return TicCmd::default();
        }
        if self.cursor >= self.bytes.len() {
            self.finished = true;
            return TicCmd::default();
        }
        if self.bytes[self.cursor] == DEMO_MARKER {
            self.finished = true;
            self.cursor += 1;
            return TicCmd::default();
        }
        if self.cursor + 4 > self.bytes.len() {
            // truncated stream: treat as end rather than reading
            // out of bounds
            self.finished = true;
            return TicCmd::default();
        }
        let cmd = TicCmd {
            forward_move: self.bytes[self.cursor] as i8,
            side_move: self.bytes[self.cursor + 1] as i8,
            angle_turn: ((self.bytes[self.cursor + 2] as u16) << 8) as i16,
            consistancy: 0,
            chat_char: 0,
            buttons: self.bytes[self.cursor + 3],
        };
        self.cursor += 4;
        cmd
    }

    /// True once the terminating byte was consumed.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Bytes consumed so far, for diagnostics.
    pub fn position(&self) -> usize {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> DemoHeader {
        DemoHeader {
            skill: 2,
            episode: 1,
            map: 1,
            deathmatch: 0,
            respawn_monsters: 0,
            fast_monsters: 0,
            no_monsters: 0,
            console_player: 0,
            playeringame: [1, 0, 0, 0],
        }
    }

    #[test]
    fn header_roundtrips_through_bytes() {
        let bytes = header().encode();
        assert_eq!(bytes[0], DEMO_VERSION);
        let parsed = DemoHeader::decode(&bytes).unwrap();
        assert_eq!(parsed, header());
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut bytes = header().encode();
        bytes[0] = 42;
        assert!(DemoHeader::decode(&bytes).is_err());
    }

    #[test]
    fn short_demo_is_rejected() {
        assert!(DemoHeader::decode(&[1, 2, 3]).is_err());
    }

    #[test]
    fn recorded_bytes_play_back_identically() {
        let mut recording = DemoRecording::new("test", &header());
        let cmds = [
            TicCmd {
                forward_move: 25,
                side_move: -5,
                angle_turn: 640,
                consistancy: 0,
                chat_char: 0,
                buttons: 0,
            },
            TicCmd {
                forward_move: 50,
                side_move: 0,
                angle_turn: -1280,
                consistancy: 0,
                chat_char: 0,
                buttons: 1,
            },
        ];
        for cmd in &cmds {
            assert!(recording.write_ticcmd(cmd));
        }
        let bytes = recording.finish();
        assert_eq!(bytes[bytes.len() - 1], DEMO_MARKER);

        let mut playback = DemoPlayback::new(bytes).unwrap();
        for expected in &cmds {
            let cmd = playback.read_ticcmd();
            // the angle is quantized to the high byte by the format
            assert_eq!(cmd.forward_move, expected.forward_move);
            assert_eq!(cmd.side_move, expected.side_move);
            assert_eq!(cmd.buttons, expected.buttons);
            assert_eq!(
                cmd.angle_turn,
                ((((expected.angle_turn as i32 + 128) >> 8) as u16) << 8) as i16
            );
        }
        assert!(!playback.finished());
        let cmd = playback.read_ticcmd();
        assert_eq!(cmd, TicCmd::default());
        assert!(playback.finished());
    }

    #[test]
    fn marker_ends_playback() {
        let mut bytes = header().encode().to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.push(DEMO_MARKER);
        let mut playback = DemoPlayback::new(bytes).unwrap();
        playback.read_ticcmd();
        playback.read_ticcmd();
        assert!(playback.finished());
    }

    #[test]
    fn truncated_stream_ends_playback_without_panicking() {
        let mut bytes = header().encode().to_vec();
        bytes.extend_from_slice(&[1, 2, 3]); // half a command
        let mut playback = DemoPlayback::new(bytes).unwrap();
        let _ = playback.read_ticcmd();
        assert!(playback.finished());
    }

    #[test]
    fn recording_reports_full_buffer() {
        let mut tiny = DemoRecording::new("full", &header());
        // fill the buffer almost completely
        tiny.cursor = MAX_DEMO_SIZE - 4;
        let cmd = TicCmd::default();
        assert!(tiny.write_ticcmd(&cmd));
        assert!(!tiny.write_ticcmd(&cmd));
    }
}

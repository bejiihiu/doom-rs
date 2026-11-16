//! Local `.lmp` demo parser.
//!
//! The byte layout is the one ported in `engine/src/game/demo.rs`
//! (`G_ReadDemoTiccmd`/`G_RecordDemo`/`G_BeginRecording` and the
//! `DEMOMARKER` terminator from `g_game.c`); this module re-encodes it
//! from raw bytes because the engine's reader is not sufficient for an
//! analyzer:
//!
//! - the engine reader hard-rejects any `version != 110` byte, but 1.9
//!   demos (version 109) are structurally identical and worth analyzing;
//! - the engine header type is not nameable outside the engine crate,
//!   and it gives no access to the raw per-tic bytes;
//! - an analyzer has to distinguish "ends with DEMOMARKER" from
//!   "truncated" / "missing terminator" and to see exactly which
//!   offset the stream ended at.
//!
//! [`doom_engine::game::DEMO_MARKER`] and [`doom_engine::game::DEMO_VERSION`]
//! are reused so the tool never drifts from the engine's notion of the
//! wire format; the 109 constant is local (the engine does not know it).

use doom_engine::game::{DEMO_MARKER, DEMO_VERSION};
use doom_engine::player::TicCmd;

/// Version byte of DOOM 1.9 demos. `linuxdoom-1.10` uses 110; both share
/// the same layout.
pub const DEMO_VERSION_109: u8 = 109;
/// Header length in bytes: version, skill, episode, map, deathmatch,
/// respawn, fast, nomonsters, consoleplayer, `playeringame[4]`.
pub const DEMO_HEADER_LEN: usize = 13;
/// Vanilla `MAX_DEMO_SIZE` (`0x20000`): the recording buffer's capacity.
/// The playback side has no size check, so a larger demo is playable;
/// the analyzer reports it as a warning, not an error.
pub const MAX_DEMO_SIZE: usize = 0x20000;
/// Maximum accepted file size (64 MiB). Demos are normally kilobytes;
/// this only guards against accidentally feeding a huge file to the tool.
pub const MAX_FILE_SIZE: usize = 64 * 1024 * 1024;

/// `BT_ATTACK` (`d_event.h`) - fire bit.
pub const BT_ATTACK: u8 = 1;
/// `BT_USE` (`d_event.h`) - use bit.
pub const BT_USE: u8 = 2;
/// `BT_CHANGE` (`d_event.h`) - the weapon bits are valid.
pub const BT_CHANGE: u8 = 4;
/// `BT_WEAPONMASK` (`d_event.h`) - the three weapon-number bits.
pub const BT_WEAPONMASK: u8 = 8 + 16 + 32;
/// `BT_WEAPONSHIFT` (`d_event.h`) - position of the weapon bits.
pub const BT_WEAPONSHIFT: u8 = 3;
/// `BT_SPECIAL` (`d_event.h`) - game event, not a real button.
pub const BT_SPECIAL: u8 = 128;
/// `BT_SPECIALMASK` (`d_event.h`) - mask for the special sub-command.
pub const BT_SPECIALMASK: u8 = 3;
/// `BTS_PAUSE` (`d_event.h`) - pause the game.
pub const BTS_PAUSE: u8 = 1;
/// `BTS_SAVEGAME` (`d_event.h`) - save the game.
pub const BTS_SAVEGAME: u8 = 2;
/// `BTS_SAVEMASK` (`d_event.h`) - the save-slot bits.
pub const BTS_SAVEMASK: u8 = 4 + 8 + 16;
/// `BTS_SAVESHIFT` (`d_event.h`) - position of the save-slot bits.
pub const BTS_SAVESHIFT: u8 = 2;
/// Bit 6 of `buttons` is unused by vanilla.
pub const BT_UNDEFINED_0X40: u8 = 0x40;

/// How the tic stream terminated, if it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEnd {
    /// A `DEMOMARKER` byte at a tic boundary (the vanilla terminator).
    Marker { offset: usize },
    /// Fewer than 4 bytes remain at a tic boundary.
    Truncated { offset: usize, leftover: usize },
    /// End of file with no `DEMOMARKER` seen (anything after the last
    /// complete tic is a dangling partial stream).
    EndOfFile,
}

/// The decoded 13 header bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DemoHeader {
    /// Version byte (`VERSION` in `doomdef.h`): 109 or 110.
    pub version: u8,
    /// Skill index, `gameskill` 0..=4 ("I'm too young to die" .. "Nightmare!").
    pub skill: u8,
    /// Episode, 1-based; commercial games store 1.
    pub episode: u8,
    /// Map, 1-based.
    pub map: u8,
    /// `deathmatch`: 0 = off, 1 = duel, 2 = altdeath.
    pub deathmatch: u8,
    /// `respawnparm` (-respawn).
    pub respawn_monsters: u8,
    /// `fastparm` (-fast).
    pub fast_monsters: u8,
    /// `nomonsters` (-nomonsters).
    pub no_monsters: u8,
    /// `consoleplayer` index.
    pub console_player: u8,
    /// `playeringame[MAXPLAYERS]`, one byte each (0/1 in vanilla).
    pub playeringame: [u8; 4],
}

impl DemoHeader {
    /// Decode the header; only the length is enforced here. Version and
    /// field ranges are validated by the `validate` command, not by the
    /// parser, so that an analyzer can look at broken demos too.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < DEMO_HEADER_LEN {
            return Err(format!(
                "demo too short: {} byte(s), need at least {DEMO_HEADER_LEN}",
                bytes.len()
            ));
        }
        Ok(Self {
            version: bytes[0],
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

    /// Number of player slots flagged in `playeringame`.
    pub fn player_count(&self) -> usize {
        self.playeringame.iter().filter(|&&p| p != 0).count()
    }

    /// Indices of the slots flagged in `playeringame`.
    pub fn active_players(&self) -> Vec<usize> {
        self.playeringame
            .iter()
            .enumerate()
            .filter(|(_, p)| **p != 0)
            .map(|(i, _)| i)
            .collect()
    }

    /// Vanilla skill name for the level index.
    pub fn skill_name(&self) -> &'static str {
        match self.skill {
            0 => "I'm too young to die",
            1 => "Hey, not too rough",
            2 => "Hurt me plenty",
            3 => "Ultra-Violence",
            4 => "Nightmare!",
            _ => "unknown",
        }
    }

    /// Vanilla deathmatch mode name.
    pub fn deathmatch_name(&self) -> &'static str {
        match self.deathmatch {
            0 => "off",
            1 => "duel",
            2 => "altdeath",
            _ => "unknown",
        }
    }

    /// The episode/map pair, rendered both ways the demo could mean.
    ///
    /// Demos do not store the game mode, so a `MAP05` reading is always
    /// ambiguous with `E1M5`. Commercial games ignore the episode byte
    /// (it is 1 in practice); episode games keep maps to 1..=9.
    pub fn level_labels(&self) -> (String, Option<String>) {
        let episode = self.episode;
        let map = self.map;
        if map == 0 {
            return ("invalid (map 0)".to_string(), None);
        }
        if episode == 0 {
            // commercial demos don't carry an episode byte
            return (format!("MAP{map:02} (commercial)"), None);
        }
        if map > 9 || episode > 4 {
            // episode byte is ignored by commercial games; MAP{map} is
            // the only sane reading
            return (format!("MAP{map:02} (commercial)"), None);
        }
        (
            format!("E{episode}M{map} (episode)"),
            Some(format!("MAP{map:02} (commercial)")),
        )
    }
}

/// One decoded tic plus its raw wire bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicRecord {
    /// Index of this tic within the stream (0-based).
    pub index: usize,
    /// The four bytes as they appear in the file.
    pub raw: [u8; 4],
    /// The command as the engine would read it: `angle_turn` is the raw
    /// byte shifted into the high half of an `i16`, like
    /// `G_ReadDemoTiccmd` produces.
    pub cmd: TicCmd,
}

/// The parsed demo.
#[derive(Clone, Debug)]
pub struct ParsedDemo {
    /// The decoded header.
    pub header: DemoHeader,
    /// Every complete tic in the stream, in order.
    pub tics: Vec<TicRecord>,
    /// How the stream ended.
    pub end: StreamEnd,
    /// Bytes after the terminating marker (trailing garbage). Empty for a
    /// well-formed demo and for a truncated one.
    pub trailing_bytes: Vec<u8>,
    /// Total size of the input, bytes.
    pub total_len: usize,
}

/// Parse a whole demo image. Tolerates broken demos (they come back as
/// [`ParsedDemo`] with a non-[`StreamEnd::Marker`] end) so every command
/// can report on them; only an unreadably short buffer is an error.
pub fn parse(bytes: &[u8]) -> Result<ParsedDemo, String> {
    let header = DemoHeader::parse(bytes)?;

    let mut tics = Vec::new();
    let mut pos = DEMO_HEADER_LEN;
    let end = loop {
        let Some(&first) = bytes.get(pos) else {
            break StreamEnd::EndOfFile;
        };
        if first == DEMO_MARKER {
            let marker_offset = pos;
            return Ok(ParsedDemo {
                header,
                tics,
                end: StreamEnd::Marker {
                    offset: marker_offset,
                },
                trailing_bytes: bytes[pos + 1..].to_vec(),
                total_len: bytes.len(),
            });
        }
        let remaining = bytes.len() - pos;
        if remaining < 4 {
            break StreamEnd::Truncated {
                offset: pos,
                leftover: remaining,
            };
        }
        let raw = [bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]];
        let cmd = TicCmd {
            forward_move: raw[0] as i8,
            side_move: raw[1] as i8,
            angle_turn: ((raw[2] as u16) << 8) as i16,
            consistancy: 0,
            chat_char: 0,
            buttons: raw[3],
        };
        tics.push(TicRecord {
            index: tics.len(),
            raw,
            cmd,
        });
        pos += 4;
    };

    Ok(ParsedDemo {
        header,
        tics,
        end,
        trailing_bytes: Vec::new(),
        total_len: bytes.len(),
    })
}

/// Parse a demo file from disk. Enforces a generous size cap to keep the
/// tool safe against accidentally being fed huge files.
pub fn load(file: &std::path::Path) -> Result<ParsedDemo, String> {
    let meta =
        std::fs::metadata(file).map_err(|e| format!("cannot stat {}: {e}", file.display()))?;
    if meta.len() > MAX_FILE_SIZE as u64 {
        return Err(format!(
            "file too large: {} bytes (cap is {})",
            meta.len(),
            MAX_FILE_SIZE
        ));
    }
    let bytes = std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    parse(&bytes)
}

/// `true` when the byte is a vanilla-supported demo version (109 = DOOM 1.9,
/// `DEMO_VERSION` = 110 = linuxdoom-1.10).
pub fn is_known_version(version: u8) -> bool {
    version == DEMO_VERSION_109 || version == DEMO_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes() -> [u8; DEMO_HEADER_LEN] {
        [110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0]
    }

    #[test]
    fn parses_minimal_demo() {
        let demo = parse(&header_bytes()).unwrap();
        assert_eq!(demo.header.version, 110);
        assert_eq!(demo.header.skill, 2);
        assert_eq!(demo.tics.len(), 0);
        assert_eq!(demo.end, StreamEnd::EndOfFile);
    }

    #[test]
    fn rejects_short_file() {
        assert!(parse(&[1, 2, 3]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn parses_tics_until_marker() {
        let mut bytes = header_bytes().to_vec();
        bytes.extend_from_slice(&[50, 0, 0x40, 1]); // forward 50, attack, angle 0x40
        bytes.extend_from_slice(&[10, 0x80, 0x80, 3]); // side 128, angle 0x80 mid-tic: fine
        bytes.push(DEMO_MARKER);
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.tics.len(), 2);
        assert_eq!(demo.tics[0].cmd.forward_move, 50);
        assert_eq!(demo.tics[0].cmd.buttons, 1);
        assert_eq!(demo.tics[0].cmd.angle_turn, 0x4000);
        assert_eq!(demo.tics[1].cmd.side_move, -128);
        assert_eq!(demo.tics[1].cmd.angle_turn, (0x80u16 << 8) as i16);
        assert_eq!(demo.end, StreamEnd::Marker { offset: 13 + 8 });
        assert_eq!(demo.trailing_bytes.len(), 0);
    }

    #[test]
    fn forward_move_0x80_at_a_tic_boundary_ends_the_stream() {
        // Vanilla ambiguity kept on purpose: DEMOMARKER (0x80) doubles as
        // forward_move == -128. The aligned scan in g_game.c stops the
        // demo at that point, so the parser does the same.
        let mut bytes = header_bytes().to_vec();
        bytes.extend_from_slice(&[0x80, 50, 0, 0]); // would be "forward -128"
        bytes.extend_from_slice(&[50, 0, 0, 1]); // never reached
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.tics.len(), 0);
        assert_eq!(demo.end, StreamEnd::Marker { offset: 13 });
        assert_eq!(demo.trailing_bytes.len(), 7);
    }

    #[test]
    fn marker_at_a_tic_boundary_ends_the_stream() {
        let mut bytes = header_bytes().to_vec();
        bytes.push(DEMO_MARKER); // marker as the very first byte after the header
        bytes.extend_from_slice(&[50, 0, 0, 0]); // this is trailing garbage, not a tic
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.tics.len(), 0);
        assert_eq!(demo.end, StreamEnd::Marker { offset: 13 });
        assert_eq!(demo.trailing_bytes.len(), 4);
    }

    #[test]
    fn truncated_tic_is_reported_not_misparsed() {
        let mut bytes = header_bytes().to_vec();
        bytes.extend_from_slice(&[1, 2]); // half a tic
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.tics.len(), 0);
        assert_eq!(
            demo.end,
            StreamEnd::Truncated {
                offset: 13,
                leftover: 2
            }
        );
    }

    #[test]
    fn angle_byte_is_shifted_like_vanilla() {
        let mut bytes = header_bytes().to_vec();
        bytes.extend_from_slice(&[0, 0, 0x40, 0]);
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.tics[0].cmd.angle_turn, (0x40u16 << 8) as i16);
        assert_eq!(demo.tics[0].raw, [0, 0, 0x40, 0]);
    }

    #[test]
    fn version_109_is_parsed() {
        let mut bytes = header_bytes();
        bytes[0] = DEMO_VERSION_109;
        let demo = parse(&bytes).unwrap();
        assert!(is_known_version(demo.header.version));
        assert_eq!(DEMO_VERSION, 110); // engine constant sanity check
    }

    #[test]
    fn player_helpers() {
        let mut bytes = header_bytes();
        bytes[9..13].copy_from_slice(&[1, 1, 0, 1]);
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.header.player_count(), 3);
        assert_eq!(demo.header.active_players(), vec![0, 1, 3]);
    }

    #[test]
    fn level_labels() {
        let mut bytes = header_bytes();
        bytes[2] = 1; // episode
        bytes[3] = 3; // map
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.header.level_labels().0, "E1M3 (episode)");

        let mut bytes = header_bytes();
        bytes[3] = 25; // map > 9 -> commercial reading
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.header.level_labels().0, "MAP25 (commercial)");

        let mut bytes = header_bytes();
        bytes[2] = 0; // commercial marker
        bytes[3] = 5;
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.header.level_labels().0, "MAP05 (commercial)");

        let mut bytes = header_bytes();
        bytes[3] = 0; // map 0 is invalid
        let demo = parse(&bytes).unwrap();
        assert_eq!(demo.header.level_labels().0, "invalid (map 0)");
    }
}

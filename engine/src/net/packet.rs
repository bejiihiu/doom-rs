//! The wire format for net game packets, ported from `d_net.h` (`doomdata_t`),
//! `d_ticcmd.h` (`ticcmd_t`) and the checksum/encoding code in `i_net.c`
//! (`PacketSend`/`PacketGet`).
//!
//! Layout of a packet on the wire (all multi-byte fields big-endian, like the
//! original `htonl`/`htons` pass in `PacketSend`):
//!
//! ```text
//! offset  size  field
//! 0       4     checksum (big-endian); low 28 bits = payload checksum,
//!               high 4 bits = NCMD_* flag bits
//! 4       1     retransmitfrom (only valid with NCMD_RETRANSMIT)
//! 5       1     starttic (low 8 bits of the tic number)
//! 6       1     player (player number, PL_DRONE bit set by drones)
//! 7       1     numtics (how many ticcmds follow, 0..=12)
//! 8       8*n   ticcmds, one per tic (see below)
//! ```
//!
//! A ticcmd is 8 bytes: one signed byte `forwardmove`, one signed byte
//! `sidemove`, a big-endian 16-bit `angleturn`, a big-endian 16-bit
//! `consistancy`, then `chatchar` and `buttons` raw.
//!
//! Only the used tics go on the wire; setup/exit packets carry `numtics == 0`
//! and are 8 bytes long. [`NetPacket::wire_size`] is the ported `NetbufferSize`.
//!
//! Checksum: the DOS/Windows builds of DOOM compile `NetbufferChecksum` in;
//! the Linux build (`NORMALUNIX`) compiles it out and always returns 0
//! because the sum is endianness-dependent. Since this port defines both
//! peers as doom-rs, the algorithm is ported over the little-endian native
//! layout of the packet (which is what DOS did), making it symmetric and
//! interoperable between any two doom-rs instances regardless of host
//! endianness. Interop with vanilla NORMALUNIX builds is out of scope;
//! they send `checksum = flags` and accept everything.

use crate::player::TicCmd;

/// `BACKUPTICS` from `d_net.h`: size of the tic ring buffer.
pub const BACKUPTICS: i32 = 12;
/// `BACKUPTICS` as a usize, for indexing the fixed-size arrays.
pub(crate) const CMD_BUF: usize = BACKUPTICS as usize;

/// `MAXNETNODES` from `d_net.h`: rooms in the node list (console is node 0).
pub const MAXNETNODES: usize = 8;
/// `RESENDCOUNT` from `d_net.c`: how long a retransmit request suppresses
/// further ones for the same node.
pub const RESENDCOUNT: i32 = 10;

/// `NCMD_EXIT`: the player left the game.
pub const NCMD_EXIT: u32 = 0x8000_0000;
/// `NCMD_RETRANSMIT`: request/announce retransmission of missed tics.
pub const NCMD_RETRANSMIT: u32 = 0x4000_0000;
/// `NCMD_SETUP`: setup packet (game settings, see [`crate::net::NetGameSettings`]).
pub const NCMD_SETUP: u32 = 0x2000_0000;
/// `NCMD_KILL`: the game was killed (machine/driver trouble).
pub const NCMD_KILL: u32 = 0x1000_0000;
/// `NCMD_CHECKSUM`: mask for the payload-checksum bits in the checksum word.
pub const NCMD_CHECKSUM: u32 = 0x0fff_ffff;

/// `PL_DRONE` from `d_net.c`: bit flag in `doomdata_t.player` set by the
/// (long-dead) three-screen drone computers. Stripped when unpacking the
/// player number.
pub const PL_DRONE: u8 = 0x80;

/// `VERSION` from `doomdef.h`: the packet-format version sent in setup
/// packets by linuxdoom-1.10. Peers with a different version refuse to play.
pub const DOOM_VERSION: u8 = 110;

/// The DOOM port number from `i_net.c`: `IPPORT_USERRESERVED + 0x1d` (5000 + 29).
pub const DOOMPORT: u16 = 5029;

/// Errors in packet encode/decode and tic math.
#[derive(Debug, thiserror::Error)]
pub enum PacketError {
    /// The datagram is shorter than a header.
    #[error("packet too short: {0} byte(s)")]
    TooShort(usize),
    /// The `numtics` byte exceeds `BACKUPTICS` (corrupt packet).
    #[error("numtics {0} exceeds BACKUPTICS ({BACKUPTICS})")]
    TooManyTics(u8),
    /// The datagram length does not match the size implied by `numtics`.
    #[error("bad packet length: {got} byte(s) on the wire, expected {want} for numtics {num_tics}")]
    BadLength {
        /// Bytes actually received.
        got: usize,
        /// Bytes implied by the `numtics` field.
        want: usize,
        /// The `numtics` field that produced `want`.
        num_tics: u8,
    },
}

/// The packet a DOOM peer sends and receives (`doomdata_t`).
///
/// Only `numtics` entries of [`NetPacket::cmds`] are meaningful; the rest is
/// ignored (and zeroed on the wire). The struct is `Copy` so the rebound
/// (node 0 loopback) can hand a datagram around by value, like vanilla's
/// `reboundstore`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetPacket {
    /// Payload checksum in the low 28 bits, `NCMD_*` flags in the high bits.
    pub checksum: u32,
    /// First tic the peer wants re-sent (only valid with `NCMD_RETRANSMIT`).
    pub retransmit_from: u8,
    /// Low byte of the tic number the first ticcmd belongs to.
    pub start_tic: u8,
    /// Player number, possibly with [`PL_DRONE`] set.
    pub player: u8,
    /// How many ticcmds follow.
    pub num_tics: u8,
    /// The tic commands, only the first `num_tics` are on the wire.
    pub cmds: [TicCmd; CMD_BUF],
}

impl Default for NetPacket {
    fn default() -> Self {
        Self {
            checksum: 0,
            retransmit_from: 0,
            start_tic: 0,
            player: 0,
            num_tics: 0,
            cmds: [TicCmd::default(); CMD_BUF],
        }
    }
}

impl NetPacket {
    /// Wire size in bytes for a given tic count (`NetbufferSize`).
    pub fn size_for(num_tics: u8) -> usize {
        8 + num_tics as usize * 8
    }

    /// Wire size in bytes for this packet (`NetbufferSize`).
    pub fn wire_size(&self) -> usize {
        Self::size_for(self.num_tics)
    }

    /// The `NCMD_*` flag bits of the checksum word.
    pub fn flags(&self) -> u32 {
        self.checksum & !NCMD_CHECKSUM
    }

    /// Whether one of the `NCMD_*` flags is set.
    pub fn has_flag(&self, flag: u32) -> bool {
        self.checksum & flag != 0
    }

    /// Sets `NCMD_*` flag bits on top of the payload checksum.
    pub fn with_flags(&mut self, flags: u32) {
        self.checksum = (self.checksum & NCMD_CHECKSUM) | flags;
    }

    /// Serializes to the wire format (`PacketSend`).
    pub fn encode_wire(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.wire_size());
        out.extend_from_slice(&self.checksum.to_be_bytes());
        out.push(self.retransmit_from);
        out.push(self.start_tic);
        out.push(self.player);
        out.push(self.num_tics);
        for cmd in self.cmds.iter().take(self.num_tics as usize) {
            out.push(cmd.forward_move as u8);
            out.push(cmd.side_move as u8);
            out.extend_from_slice(&cmd.angle_turn.to_be_bytes());
            out.extend_from_slice(&cmd.consistancy.to_be_bytes());
            out.push(cmd.chat_char);
            out.push(cmd.buttons);
        }
        out
    }

    /// Parses a datagram coming off the wire (`PacketGet`).
    ///
    /// Failing length/`numtics` checks returns an error; callers drop the
    /// packet, like vanilla's `HGetPacket` does.
    pub fn decode_wire(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < 8 {
            return Err(PacketError::TooShort(bytes.len()));
        }
        let num_tics = bytes[7];
        if num_tics as i32 > BACKUPTICS {
            return Err(PacketError::TooManyTics(num_tics));
        }
        let want = Self::size_for(num_tics);
        if bytes.len() != want {
            return Err(PacketError::BadLength {
                got: bytes.len(),
                want,
                num_tics,
            });
        }
        let mut packet = NetPacket {
            checksum: u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            retransmit_from: bytes[4],
            start_tic: bytes[5],
            player: bytes[6],
            num_tics,
            ..NetPacket::default()
        };
        for (i, slot) in packet.cmds.iter_mut().enumerate().take(num_tics as usize) {
            let o = 8 + i * 8;
            *slot = TicCmd {
                forward_move: bytes[o] as i8,
                side_move: bytes[o + 1] as i8,
                angle_turn: i16::from_be_bytes([bytes[o + 2], bytes[o + 3]]),
                consistancy: i16::from_be_bytes([bytes[o + 4], bytes[o + 5]]),
                chat_char: bytes[o + 6],
                buttons: bytes[o + 7],
            };
        }
        Ok(packet)
    }

    /// The packet in the little-endian native layout DOS used when computing
    /// the checksum. Only the first [`NetPacket::wire_size`] bytes carry data.
    fn native_bytes(&self) -> [u8; 8 + CMD_BUF * 8] {
        let mut out = [0u8; 8 + CMD_BUF * 8];
        out[0..4].copy_from_slice(&self.checksum.to_le_bytes());
        out[4] = self.retransmit_from;
        out[5] = self.start_tic;
        out[6] = self.player;
        out[7] = self.num_tics;
        for (i, cmd) in self.cmds.iter().take(self.num_tics as usize).enumerate() {
            let o = 8 + i * 8;
            out[o] = cmd.forward_move as u8;
            out[o + 1] = cmd.side_move as u8;
            out[o + 2..o + 4].copy_from_slice(&cmd.angle_turn.to_le_bytes());
            out[o + 4..o + 6].copy_from_slice(&cmd.consistancy.to_le_bytes());
            out[o + 6] = cmd.chat_char;
            out[o + 7] = cmd.buttons;
        }
        out
    }
}

/// `NetbufferChecksum`: the DOS/Windows checksum algorithm.
///
/// Sums the 32-bit little-endian words of the packet starting at
/// `retransmitfrom`, weighting each by `index + 1`, seeded with `0x1234567`,
/// masked to the `NCMD_CHECKSUM` bits. See the module docs for why this is
/// defined over the little-endian layout.
pub fn checksum(packet: &NetPacket) -> u32 {
    let native = packet.native_bytes();
    let size = packet.wire_size();
    let mut c: u32 = 0x0123_4567;
    for (i, word) in native[4..size].chunks_exact(4).enumerate() {
        let w = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        c = c.wrapping_add(w.wrapping_mul((i + 1) as u32));
    }
    c & NCMD_CHECKSUM
}

/// Computes the checksum and stamps it plus the given `NCMD_*` flag bits into
/// the packet (what `HSendPacket` does before transmitting).
pub fn stamp_checksum(packet: &mut NetPacket, flags: u32) {
    packet.checksum = checksum(packet) | flags;
}

/// `ExpandTics`: expands the 8-bit `starttic`/`retransmitfrom` byte to a full
/// tic number, given the current `maketic`.
///
/// Tics are sent mod 256; the receiver figures out the upper bits by picking
/// the interpretation closest to `maketic`. Trivially total (the vanilla
/// `I_Error` branch is unreachable: one of the three cases always matches).
pub fn expand_tics(low: u8, maketic: i32) -> i32 {
    let low = i32::from(low);
    let delta = low - (maketic & 0xff);
    if (-64..=64).contains(&delta) {
        (maketic & !0xff) + low
    } else if delta > 64 {
        (maketic & !0xff) - 256 + low
    } else {
        (maketic & !0xff) + 256 + low
    }
}

/// The game settings a net game is arbitrated on, carried in setup packets.
///
/// Layout (as vanilla packs them, `D_ArbitrateNetStart`): `retransmitfrom`
/// holds `skill & 15` in the low nibble plus `deathmatch << 6`, the
/// `nomonsters` bit (`0x20`) and the `respawn` bit (`0x10`); `starttic`
/// holds `episode * 64 + map` (truncated to a byte — episode 4 of Ultimate
/// Doom wraps to 0, a vanilla quirk kept for fidelity).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetGameSettings {
    /// `startskill`: 1..=5 (packed into the low nibble).
    pub skill: i32,
    /// `deathmatch`: 0 = cooperation, 1 = duel, 2 = alt-death.
    pub deathmatch: i32,
    /// `nomonsters`: `-nomonsters`.
    pub nomonsters: bool,
    /// `respawnparm`: `-respawn`.
    pub respawn: bool,
    /// `startepisode`: 1..=4.
    pub episode: i32,
    /// `startmap`: 1..=32.
    pub map: i32,
}

impl NetGameSettings {
    /// Packs the settings into the two bytes of a setup packet.
    pub fn encode(&self) -> (u8, u8) {
        let mut retransmit = (self.skill & 15) as u8;
        retransmit |= ((self.deathmatch & 3) as u8) << 6;
        if self.nomonsters {
            retransmit |= 0x20;
        }
        if self.respawn {
            retransmit |= 0x10;
        }
        let start_tic = ((self.episode * 64 + self.map) & 0xff) as u8;
        (retransmit, start_tic)
    }

    /// Unpacks a setup packet back into settings.
    pub fn decode(packet: &NetPacket) -> Self {
        NetGameSettings {
            skill: (packet.retransmit_from & 15) as i32,
            deathmatch: ((packet.retransmit_from & 0xc0) >> 6) as i32,
            nomonsters: packet.retransmit_from & 0x20 != 0,
            respawn: packet.retransmit_from & 0x10 != 0,
            episode: (packet.start_tic >> 6) as i32,
            map: (packet.start_tic & 0x3f) as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_CMD: TicCmd = TicCmd {
        forward_move: 0,
        side_move: 0,
        angle_turn: 0,
        consistancy: 0,
        chat_char: 0,
        buttons: 0,
    };

    fn cmd(forward_move: i8, angle_turn: i16, consistancy: i16) -> TicCmd {
        TicCmd {
            forward_move,
            side_move: 0,
            angle_turn,
            consistancy,
            chat_char: 0,
            buttons: 0,
        }
    }

    // -- wire size ----------------------------------------------------------

    #[test]
    fn packet_size_matches_vanilla_layout() {
        assert_eq!(NetPacket::size_for(0), 8);
        assert_eq!(NetPacket::size_for(1), 16);
        assert_eq!(NetPacket::size_for(12), 104);
    }

    #[test]
    fn flags_and_checksum_bits_do_not_overlap() {
        for flag in [NCMD_EXIT, NCMD_RETRANSMIT, NCMD_SETUP, NCMD_KILL] {
            assert_eq!(flag & NCMD_CHECKSUM, 0);
        }
        let mut p = NetPacket::default();
        stamp_checksum(&mut p, NCMD_SETUP | NCMD_RETRANSMIT);
        assert_ne!(p.checksum & NCMD_SETUP, 0);
        assert_ne!(p.checksum & NCMD_RETRANSMIT, 0);
        assert_eq!(p.checksum & NCMD_KILL, 0);
        // re-stamping with different flags replaces them
        stamp_checksum(&mut p, NCMD_EXIT);
        assert_eq!(p.checksum & NCMD_SETUP, 0);
        assert_eq!(p.checksum & NCMD_RETRANSMIT, 0);
        assert_ne!(p.checksum & NCMD_EXIT, 0);
    }

    // -- encode/decode roundtrip --------------------------------------------

    #[test]
    fn wire_roundtrip_all_fields() {
        let mut p = NetPacket {
            checksum: 0xdead_beef,
            retransmit_from: 0x5a,
            start_tic: 0xa5,
            player: 3,
            num_tics: 3,
            ..NetPacket::default()
        };
        p.cmds[0] = TicCmd {
            forward_move: -5,
            side_move: 7,
            angle_turn: 0x0102,
            consistancy: -1234,
            chat_char: b'x',
            buttons: 0xfe,
        };
        p.cmds[1] = cmd(0, -32768, 32767);
        p.cmds[2] = ZERO_CMD;
        let wire = p.encode_wire();
        assert_eq!(wire.len(), 8 + 3 * 8);
        let back = NetPacket::decode_wire(&wire).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn wire_layout_matches_packetsend_byte_order() {
        let mut p = NetPacket {
            checksum: 0x1122_3344,
            retransmit_from: 0x05,
            start_tic: 0x06,
            player: 0x07,
            num_tics: 1,
            ..NetPacket::default()
        };
        p.cmds[0] = TicCmd {
            forward_move: 0x21,
            side_move: 0xe0u8 as i8,
            angle_turn: 0x0201,
            consistancy: 0x0403,
            chat_char: 0x08,
            buttons: 0x09,
        };
        let wire = p.encode_wire();
        assert_eq!(
            wire,
            [
                0x11, 0x22, 0x33, 0x44, // checksum big-endian
                0x05, // retransmitfrom
                0x06, // starttic
                0x07, // player
                0x01, // numtics
                0x21, // forwardmove (raw byte)
                0xe0, // sidemove (raw byte)
                0x02, 0x01, // angleturn big-endian
                0x04, 0x03, // consistancy big-endian
                0x08, // chatchar
                0x09, // buttons
            ]
        );
    }

    #[test]
    fn decode_rejects_bad_packets() {
        // too short for the header
        assert!(matches!(
            NetPacket::decode_wire(&[0; 7]),
            Err(PacketError::TooShort(7))
        ));
        // numtics larger than BACKUPTICS
        let mut wire = vec![0u8; 8];
        wire[7] = 13;
        assert!(matches!(
            NetPacket::decode_wire(&wire),
            Err(PacketError::TooManyTics(13))
        ));
        // declared length does not match actual length
        let mut wire = vec![0u8; 16];
        wire[7] = 2; // implies 24 bytes
        assert!(matches!(
            NetPacket::decode_wire(&wire),
            Err(PacketError::BadLength {
                got: 16,
                want: 24,
                num_tics: 2
            })
        ));
        // a packet with numtics==12 and exactly 104 bytes is the maximum
        let wire = vec![0u8; 104];
        let mut wire = wire;
        wire[7] = 12;
        assert!(NetPacket::decode_wire(&wire).is_ok());
    }

    // -- checksum: known vectors derived from the vanilla algorithm ---------

    #[test]
    fn checksum_zero_tics_zero_payload() {
        let p = NetPacket::default(); // numtics == 0
        assert_eq!(checksum(&p), 0x0123_4567);
    }

    #[test]
    fn checksum_one_forwardmove() {
        let mut p = NetPacket {
            num_tics: 1,
            ..NetPacket::default()
        };
        p.cmds[0] = cmd(1, 0, 0);
        // words: [0x01000000, 0x00000001, 0]
        // c = 0x01234567 + 0x01000000*1 + 1*2
        assert_eq!(checksum(&p), 0x0223_4569);
    }

    #[test]
    fn checksum_angleturn_le16() {
        let mut p = NetPacket {
            num_tics: 1,
            ..NetPacket::default()
        };
        p.cmds[0] = cmd(0, 0x0201, 0);
        // words: [0x01000000, 0x02010000, 0]
        // c = 0x01234567 + 0x01000000*1 + 0x02010000*2
        assert_eq!(checksum(&p), 0x0625_4567);
    }

    #[test]
    fn checksum_consistancy_le16() {
        let mut p = NetPacket {
            num_tics: 1,
            ..NetPacket::default()
        };
        p.cmds[0] = cmd(0, 0, 0x00ff);
        // words: [0x01000000, 0, 0x000000ff]
        // c = 0x01234567 + 0x01000000*1 + 0xff*3
        assert_eq!(checksum(&p), 0x0223_4864);
    }

    #[test]
    fn checksum_ignores_flag_bits_in_the_word() {
        // vanilla computes over the packet with the checksum word excluded,
        // so the NCMD_* flag bits in `checksum` never feed the sum
        let mut a = NetPacket {
            num_tics: 1,
            ..NetPacket::default()
        };
        a.cmds[0] = cmd(1, 0, 0);
        let mut b = a;
        b.checksum = 0xdead_beef;
        assert_eq!(checksum(&a), checksum(&b));
    }

    #[test]
    fn stamp_roundtrip_survives_wire() {
        let mut p = NetPacket {
            num_tics: 2,
            start_tic: 42,
            player: 2,
            ..NetPacket::default()
        };
        p.cmds[0] = cmd(3, 0x1111, 0x2222);
        p.cmds[1] = cmd(-3, -0x1111, -0x2222);
        stamp_checksum(&mut p, NCMD_RETRANSMIT);
        let wire = p.encode_wire();
        let back = NetPacket::decode_wire(&wire).unwrap();
        assert_eq!(checksum(&back), back.checksum & NCMD_CHECKSUM);
        assert!(back.has_flag(NCMD_RETRANSMIT));
    }

    // -- expand_tics ---------------------------------------------------------

    #[test]
    fn expand_tics_same_century() {
        // low == maketic byte within +-64 -> same century
        assert_eq!(expand_tics(100, 100), 100);
        assert_eq!(expand_tics(50, 100), 50);
        assert_eq!(expand_tics(150, 100), 150);
        // near the 256 boundary: maketic = 300 (0x12c), byte = 0x2c
        assert_eq!(expand_tics(0x2c, 300), 300);
        assert_eq!(expand_tics(0x2d, 300), 301);
        assert_eq!(expand_tics(0x2b, 300), 299);
    }

    #[test]
    fn expand_tics_previous_century() {
        // low = 250, maketic = 260 (byte 4): delta = 246 > 64 -> century-256
        assert_eq!(expand_tics(250, 260), 250);
        // low = 10, maketic = 250: delta = 10-250 = -240 < -64 -> century+256
        assert_eq!(expand_tics(10, 250), 266);
        // exact boundary deltas
        assert_eq!(expand_tics(64, 0), 64); // delta == 64 -> first branch
        assert_eq!(expand_tics(65, 0), -191); // delta == 65 -> previous century
        assert_eq!(expand_tics(65, 1), 65); // delta == 64 -> first branch
        assert_eq!(expand_tics(192, 0), 192 - 256);
        assert_eq!(expand_tics(192, 1), -64); // delta == 191, (1 & !0xff) - 256 + 192
        // delta == -64 stays in the first branch, -65 jumps a century
        assert_eq!(expand_tics(0, 64), 0);
        assert_eq!(expand_tics(0, 65), 256);
    }

    // -- setup payload -------------------------------------------------------

    #[test]
    fn setup_payload_roundtrip() {
        for settings in [
            NetGameSettings {
                skill: 1,
                deathmatch: 0,
                nomonsters: false,
                respawn: false,
                episode: 1,
                map: 1,
            },
            NetGameSettings {
                skill: 5,
                deathmatch: 2,
                nomonsters: true,
                respawn: true,
                episode: 3,
                map: 9,
            },
            NetGameSettings {
                skill: 4,
                deathmatch: 1,
                nomonsters: false,
                respawn: true,
                episode: 2,
                map: 4,
            },
        ] {
            let (retransmit, start_tic) = settings.encode();
            let p = NetPacket {
                retransmit_from: retransmit,
                start_tic,
                ..NetPacket::default()
            };
            assert_eq!(NetGameSettings::decode(&p), settings);
        }
    }

    #[test]
    fn setup_payload_bit_layout() {
        let s = NetGameSettings {
            skill: 3,
            deathmatch: 2, // alt-death -> 0x80
            nomonsters: true,
            respawn: true,
            episode: 2,
            map: 3,
        };
        let (retransmit, start_tic) = s.encode();
        assert_eq!(retransmit, 3 | (2 << 6) | 0x20 | 0x10);
        assert_eq!(start_tic, 2 * 64 + 3);
    }

    #[test]
    fn setup_payload_episode_4_wraps_like_vanilla() {
        // startepisode * 64 + startmap for episode 4 does not fit in a byte;
        // vanilla truncates and the receiver decodes episode 0 — kept verbatim
        let s = NetGameSettings {
            skill: 2,
            deathmatch: 0,
            nomonsters: false,
            respawn: false,
            episode: 4,
            map: 9,
        };
        let (_, start_tic) = s.encode();
        assert_eq!(start_tic, 9);
        let p = NetPacket {
            start_tic,
            ..NetPacket::default()
        };
        let back = NetGameSettings::decode(&p);
        assert_eq!(back.episode, 0);
        assert_eq!(back.map, 9);
    }

    #[test]
    fn linuxdoom_constants() {
        assert_eq!(DOOM_VERSION, 110);
        assert_eq!(MAXNETNODES, 8);
        assert_eq!(RESENDCOUNT, 10);
        assert_eq!(BACKUPTICS, 12);
        assert_eq!(DOOMPORT, 5029);
    }
}

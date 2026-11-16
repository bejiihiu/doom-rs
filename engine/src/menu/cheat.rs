//! The `m_cheat.c` port: cheat sequence checking.
//!
//! The original lives in three places: the generic sequence checker
//! (`m_cheat.c`), the cheat tables and the "who may cheat" gating
//! (`ST_Responder` in `st_stuff.c`, `AM_Responder` in `am_map.c`). The
//! tables are ported here byte-for-byte, together with the scramble
//! table. The sequences themselves are the vanilla ones; they are *not*
//! cryptographically meaningful, they just keep casual players from
//! stumbling onto the codes by accident.
//!
//! What the port does differently, all deliberate:
//!
//! * The vanilla checker mutates the shared `cheatseq_t` to capture
//!   parameter bytes (`idclev`/`idmus`). This port keeps the parameter
//!   bytes in the `CheatSequence` value instead of poking at the shared
//!   table, so `&self`-style callers could have several tables alive.
//! * The input layer in the original lower-cases every letter before it
//!   reaches `cht_CheckCheat` (see `xlatekey` in `i_video.c`). This port
//!   lower-cases ASCII letters inside the checker instead, so the cheat
//!   works regardless of which layer hands us the key. Non-letter bytes
//!   are matched as-is.
//! * `idclev` on commercial games was dead in linuxdoom-1.10 (`epsd = 0`
//!   is set, then `epsd < 1` rejects it). The episode is ignored for
//!   commercial games anyway, so the `epsd < 1` check is applied only to
//!   non-commercial game modes. That is the only behavioral deviation.
//! * A single keypress can complete more than one sequence in the
//!   original (the power-up chain is not part of the `else if` ladder);
//!   this port reports every completed sequence as a [`CheatEvent`].
//!   When more than one event comes back the last one wins the player's
//!   message slot, exactly like the vanilla chain.
//!
//! Typing a space in the middle of a cheat does *not* skip it — the
//! space just fails to match and resets the sequence, like the original.
//!
//! Where the original writes `plyr->message` directly, this module emits
//! a plain-data [`CheatEvent`] and lets the game layer decide what to
//! do (fatal exclusions like the netgame gate are applied here, though,
//! because they are part of the matcher, not of the reaction).

use crate::menu::state::GameMode;

/// `SCRAMBLE(a)` from `m_cheat.h` — the bit shuffle the stored cheat
/// bytes are obfuscated with.
const fn scramble(a: u8) -> u8 {
    ((a & 1) << 7)
        .wrapping_add((a & 2) << 5)
        .wrapping_add(a & 4)
        .wrapping_add((a & 8) << 1)
        .wrapping_add((a & 16) >> 1)
        .wrapping_add(a & 32)
        .wrapping_add((a & 64) >> 5)
        .wrapping_add((a & 128) >> 7)
}

/// `cheat_xlate_table` — built once at compile time instead of the
/// vanilla runtime `firsttime` flag.
const fn build_xlate_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = scramble(i as u8);
        i += 1;
    }
    table
}

const XLATE: [u8; 256] = build_xlate_table();

/// The end-of-sequence marker (`0xff`).
const SEQ_END: u8 = 0xff;
/// The parameter-capture byte (`1`). Bytes after it (up to `SEQ_END`)
/// that are `0` in the static sequence are filled in by the player at
/// runtime; in this port they live in `captured`.
const SEQ_PARAM: u8 = 1;

/// One cheat sequence and its in-progress match state.
///
/// The static sequence bytes are shared (`&'static [u8]`); only the
/// position and the captured parameter bytes are per-instance.
pub struct CheatSequence {
    sequence: &'static [u8],
    pos: usize,
    captured: Vec<u8>,
}

impl CheatSequence {
    const fn new(sequence: &'static [u8]) -> Self {
        Self {
            sequence,
            pos: 0,
            captured: Vec::new(),
        }
    }

    /// `cht_CheckCheat`. Returns true exactly once per completed cheat
    /// (the position resets, like the original).
    pub fn check(&mut self, key: u8) -> bool {
        let key = key.to_ascii_lowercase();
        // paranoia: a corrupt static would lose us, not trap us
        if self.pos >= self.sequence.len() {
            self.pos = 0;
            self.captured.clear();
        }
        let slot = self.sequence[self.pos];
        if slot == 0 {
            // parameter slot: capture the raw (lower-cased) key
            self.captured.push(key);
            self.pos += 1;
        } else if XLATE[key as usize] == slot {
            self.pos += 1;
        } else {
            self.pos = 0;
            self.captured.clear();
        }
        // skip the parameter marker / finish on the end marker
        if self.pos < self.sequence.len() {
            match self.sequence[self.pos] {
                SEQ_PARAM => self.pos += 1,
                SEQ_END => {
                    self.pos = 0;
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// `cht_GetParam`: the bytes captured after the parameter marker,
    /// space-separated and lower-cased by [`CheatSequence::check`].
    /// Drains them — the original zeroes the sequence slots.
    pub fn take_param(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.captured)
    }
}

/// The power-up a `behold<x>` cheat grants. The order matches the
/// `powertype_t` enum in `doomdef.h` (`pw_invulnerability .. pw_infrared`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheatPowerUp {
    Invulnerability,
    Strength,
    Invisibility,
    IronFeet,
    AllMap,
    Infrared,
}

/// A completed cheat, as plain data. The game layer applies the effect
/// (and shows the message); nothing here touches game state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheatEvent {
    /// `iddqd` — toggle god mode.
    GodMode,
    /// `idkfa` — full ammo, armor, weapons and keys.
    AmmoFull,
    /// `idfa` — full ammo, armor and weapons, no keys.
    AmmoNoKeys,
    /// `idspispopd` / `idclip` — toggle no-clip.
    NoClip,
    /// `beholdv` / `beholds` / `beholdi` / `beholdr` / `beholda` /
    /// `beholdl`.
    PowerUp(CheatPowerUp),
    /// `behold` — show the power-up cheat list.
    BeholdMenu,
    /// `idchoppers` — chainsaw + invulnerability.
    Choppers,
    /// `idmypos` — print player position.
    MyPos,
    /// `idclev<ep><map>` — change level. Parameters are already
    /// validated against the game mode.
    Warp { episode: u8, map: u8 },
    /// `idmus<n><n>` — change music. The two digits are carried raw;
    /// resolving them to a track number needs the game mode, so the
    /// game layer does it (commercial `mus_runnin` range vs the
    /// episode-based range for DOOM 1).
    Music { digits: [u8; 2] },
    /// `iddt` — toggle the automap (hold/keys style is the automap's
    /// business).
    Automap,
}

/// Context the cheat gating depends on. `ST_Responder` refuses cheats
/// entirely in network games (except `idclev`); `AM_Responder` gates
/// `iddt` on not being deathmatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheatContext {
    pub gamemode: GameMode,
    pub netgame: bool,
    pub deathmatch: bool,
}

// The vanilla sequences, byte for byte (st_stuff.c, am_map.c).
const SEQ_MUS: &[u8] = &[0xb2, 0x26, 0xb6, 0xae, 0xea, SEQ_PARAM, 0, 0, SEQ_END];
const SEQ_CHOPPERS: &[u8] = &[
    0xb2, 0x26, 0xe2, 0x32, 0xf6, 0x2a, 0x2a, 0xa6, 0x6a, 0xea, SEQ_END,
];
const SEQ_GOD: &[u8] = &[0xb2, 0x26, 0x26, 0xaa, 0x26, SEQ_END];
const SEQ_AMMO: &[u8] = &[0xb2, 0x26, 0xf2, 0x66, 0xa2, SEQ_END];
const SEQ_AMMO_NOKEY: &[u8] = &[0xb2, 0x26, 0x66, 0xa2, SEQ_END];
const SEQ_NOCLIP: &[u8] = &[
    0xb2, 0x26, 0xea, 0x2a, 0xb2, 0xea, 0x2a, 0xf6, 0x2a, 0x26, SEQ_END,
];
const SEQ_COMMERCIAL_NOCLIP: &[u8] = &[0xb2, 0x26, 0xe2, 0x36, 0xb2, 0x2a, SEQ_END];
const SEQ_BEHOLD_V: &[u8] = &[
    0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, 0x6e, SEQ_END,
];
const SEQ_BEHOLD_S: &[u8] = &[
    0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, 0xea, SEQ_END,
];
const SEQ_BEHOLD_I: &[u8] = &[
    0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, 0xb2, SEQ_END,
];
const SEQ_BEHOLD_R: &[u8] = &[
    0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, 0x6a, SEQ_END,
];
const SEQ_BEHOLD_A: &[u8] = &[
    0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, 0xa2, SEQ_END,
];
const SEQ_BEHOLD_L: &[u8] = &[
    0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, 0x36, SEQ_END,
];
const SEQ_BEHOLD_BARE: &[u8] = &[0xb2, 0x26, 0x62, 0xa6, 0x32, 0xf6, 0x36, 0x26, SEQ_END];
const SEQ_CLEV: &[u8] = &[0xb2, 0x26, 0xe2, 0x36, 0xa6, 0x6e, SEQ_PARAM, 0, 0, SEQ_END];
const SEQ_MYPOS: &[u8] = &[0xb2, 0x26, 0xb6, 0xba, 0x2a, 0xf6, 0xea, SEQ_END];
const SEQ_AMAP: &[u8] = &[0xb2, 0x26, 0x26, 0x2e, SEQ_END];

/// Every cheat the port recognises, matching `st_stuff.c` (`ST_Responder`)
/// and `am_map.c` (`AM_Responder`).
pub struct CheatTable {
    god: CheatSequence,
    ammo: CheatSequence,
    ammonokey: CheatSequence,
    mus: CheatSequence,
    noclip: CheatSequence,
    commercial_noclip: CheatSequence,
    powerup: [CheatSequence; 7],
    choppers: CheatSequence,
    clev: CheatSequence,
    mypos: CheatSequence,
    amap: CheatSequence,
}

impl Default for CheatTable {
    fn default() -> Self {
        Self::new()
    }
}

impl CheatTable {
    pub fn new() -> Self {
        Self {
            god: CheatSequence::new(SEQ_GOD),
            ammo: CheatSequence::new(SEQ_AMMO),
            ammonokey: CheatSequence::new(SEQ_AMMO_NOKEY),
            mus: CheatSequence::new(SEQ_MUS),
            noclip: CheatSequence::new(SEQ_NOCLIP),
            commercial_noclip: CheatSequence::new(SEQ_COMMERCIAL_NOCLIP),
            powerup: [
                CheatSequence::new(SEQ_BEHOLD_V),
                CheatSequence::new(SEQ_BEHOLD_S),
                CheatSequence::new(SEQ_BEHOLD_I),
                CheatSequence::new(SEQ_BEHOLD_R),
                CheatSequence::new(SEQ_BEHOLD_A),
                CheatSequence::new(SEQ_BEHOLD_L),
                CheatSequence::new(SEQ_BEHOLD_BARE),
            ],
            choppers: CheatSequence::new(SEQ_CHOPPERS),
            clev: CheatSequence::new(SEQ_CLEV),
            mypos: CheatSequence::new(SEQ_MYPOS),
            amap: CheatSequence::new(SEQ_AMAP),
        }
    }

    /// Feeds one key. Returns the cheats that completed with this key —
    /// several can complete on the same keypress in the original (they
    /// fall out of the `if` ladder and the power-up chain separately);
    /// the game layer may show the last one as the player's message.
    ///
    /// Gating mirrors the original: no cheats at all in a network game
    /// *except* `idclev`, and `iddt` is not deathmatch-only... it is
    /// deathmatch-*gated* — it does not work in deathmatch.
    pub fn check(&mut self, key: u8, ctx: CheatContext) -> Vec<CheatEvent> {
        let mut events = Vec::new();

        if !ctx.netgame {
            if self.god.check(key) {
                events.push(CheatEvent::GodMode);
            }
            if self.ammonokey.check(key) {
                events.push(CheatEvent::AmmoNoKeys);
            }
            if self.ammo.check(key) {
                events.push(CheatEvent::AmmoFull);
            }
            if self.mus.check(key) {
                let param = self.mus.take_param();
                events.push(CheatEvent::Music {
                    digits: [
                        param.first().copied().unwrap_or(b'0'),
                        param.get(1).copied().unwrap_or(b'0'),
                    ],
                });
            }
            if self.noclip.check(key) || self.commercial_noclip.check(key) {
                events.push(CheatEvent::NoClip);
            }
            const POWERUPS: [CheatPowerUp; 6] = [
                CheatPowerUp::Invulnerability,
                CheatPowerUp::Strength,
                CheatPowerUp::Invisibility,
                CheatPowerUp::IronFeet,
                CheatPowerUp::AllMap,
                CheatPowerUp::Infrared,
            ];
            for (i, powerup) in POWERUPS.iter().enumerate() {
                if self.powerup[i].check(key) {
                    events.push(CheatEvent::PowerUp(*powerup));
                }
            }
            if self.powerup[6].check(key) {
                events.push(CheatEvent::BeholdMenu);
            }
            if self.choppers.check(key) {
                events.push(CheatEvent::Choppers);
            }
            if self.mypos.check(key) {
                events.push(CheatEvent::MyPos);
            }
        }

        // idclev is intentionally outside the netgame gate, like vanilla.
        if self.clev.check(key) {
            let param = self.clev.take_param();
            let digit = |i: usize| param.get(i).copied().unwrap_or(b'0').saturating_sub(b'0');
            let valid = |d: u8| d.is_ascii_digit();
            if !valid(param.first().copied().unwrap_or(0))
                || !valid(param.get(1).copied().unwrap_or(0))
            {
                return events;
            }
            let (episode, map) = match ctx.gamemode {
                GameMode::Commercial => (0, digit(0) * 10 + digit(1)),
                _ => (digit(0), digit(1)),
            };
            // vanilla: commercial has no usable episode and map is capped
            // at 34; other modes need a real episode and a 1..=9 map
            let ok = match ctx.gamemode {
                GameMode::Commercial => (1..=34).contains(&map),
                // the 1.10 `epsd < 1` bail does not apply to commercial
                GameMode::Retail => (1..=4).contains(&episode) && (1..=9).contains(&map),
                GameMode::Registered => (1..=3).contains(&episode) && (1..=9).contains(&map),
                GameMode::Shareware => (1..=1).contains(&episode) && (1..=9).contains(&map),
            };
            if ok {
                events.push(CheatEvent::Warp { episode, map });
            }
        }

        // iddt lives in AM_Responder: not in deathmatch.
        if !ctx.deathmatch && self.amap.check(key) {
            events.push(CheatEvent::Automap);
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(gamemode: GameMode) -> CheatContext {
        CheatContext {
            gamemode,
            netgame: false,
            deathmatch: false,
        }
    }

    fn type_seq(table: &mut CheatTable, ctx: CheatContext, seq: &str) -> Vec<CheatEvent> {
        let mut out = Vec::new();
        for &b in seq.as_bytes() {
            out.extend(table.check(b, ctx));
        }
        out
    }

    #[test]
    fn scramble_matches_vanilla() {
        // spot-check the values that appear in the sequences
        assert_eq!(scramble(b'i'), 0xb2);
        assert_eq!(scramble(b'd'), 0x26);
        assert_eq!(scramble(b'q'), 0xaa);
        assert_eq!(scramble(b'k'), 0xf2);
        assert_eq!(scramble(b'f'), 0x66);
        assert_eq!(scramble(b'a'), 0xa2);
        assert_eq!(scramble(b'p'), 0x2a);
        assert_eq!(scramble(b's'), 0xea);
        assert_eq!(scramble(b'o'), 0xf6);
        assert_eq!(scramble(b'h'), 0x32);
    }

    #[test]
    fn every_sequence_byte_scrambles_to_itself_sanely() {
        // XLATE must be a permutation of the 8-bit range? no — it is not
        // a permutation (it is a bit shuffle, but not bijective on bytes
        // with odd parity). Just check it agrees with scramble().
        for (i, &v) in XLATE.iter().enumerate() {
            assert_eq!(v, scramble(i as u8));
        }
    }

    #[test]
    fn vanilla_cheat_strings_all_match() {
        // the plaintext comments next to the scrambled tables
        let cases: &[(&str, &[CheatEvent])] = &[
            ("iddqd", &[CheatEvent::GodMode]),
            ("idfa", &[CheatEvent::AmmoNoKeys]),
            ("idkfa", &[CheatEvent::AmmoFull]),
            ("idspispopd", &[CheatEvent::NoClip]),
            ("idclip", &[CheatEvent::NoClip]),
            ("idmypos", &[CheatEvent::MyPos]),
            ("idchoppers", &[CheatEvent::Choppers]),
            ("idbehold", &[CheatEvent::BeholdMenu]),
            // completing the prefix fires the bare "idbehold" prompt first,
            // then the letter fires the power-up — both events, last wins the
            // message slot, exactly like the vanilla if-chain
            (
                "idbeholdv",
                &[
                    CheatEvent::BeholdMenu,
                    CheatEvent::PowerUp(CheatPowerUp::Invulnerability),
                ][..],
            ),
            (
                "idbeholds",
                &[
                    CheatEvent::BeholdMenu,
                    CheatEvent::PowerUp(CheatPowerUp::Strength),
                ][..],
            ),
            (
                "idbeholdi",
                &[
                    CheatEvent::BeholdMenu,
                    CheatEvent::PowerUp(CheatPowerUp::Invisibility),
                ][..],
            ),
            (
                "idbeholdr",
                &[
                    CheatEvent::BeholdMenu,
                    CheatEvent::PowerUp(CheatPowerUp::IronFeet),
                ][..],
            ),
            (
                "idbeholda",
                &[
                    CheatEvent::BeholdMenu,
                    CheatEvent::PowerUp(CheatPowerUp::AllMap),
                ][..],
            ),
            (
                "idbeholdl",
                &[
                    CheatEvent::BeholdMenu,
                    CheatEvent::PowerUp(CheatPowerUp::Infrared),
                ][..],
            ),
            ("iddt", &[CheatEvent::Automap]),
        ];
        for (typed, expect) in cases {
            let mut table = CheatTable::new();
            let events = type_seq(&mut table, ctx(GameMode::Retail), typed);
            assert_eq!(events.as_slice(), *expect, "cheat {typed}");
        }
    }

    #[test]
    fn cheat_is_case_insensitive() {
        for typed in ["IDDQD", "IdDqD", "iddqd"] {
            let mut table = CheatTable::new();
            assert_eq!(
                type_seq(&mut table, ctx(GameMode::Retail), typed),
                vec![CheatEvent::GodMode]
            );
        }
    }

    #[test]
    fn noise_between_letters_resets_progress() {
        let mut table = CheatTable::new();
        // a stray letter breaks the match, the cheat has to be retyped
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "iddqxd").is_empty());
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Retail), "iddqd"),
            vec![CheatEvent::GodMode]
        );
    }

    #[test]
    fn space_does_not_skip_letters() {
        // vanilla does not ignore spaces: they fail the match and reset
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "idd qd").is_empty());
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "id dqd").is_empty());
    }

    #[test]
    fn repeated_sequence_after_success_works_again() {
        let mut table = CheatTable::new();
        for _ in 0..3 {
            assert_eq!(
                type_seq(&mut table, ctx(GameMode::Retail), "iddqd"),
                vec![CheatEvent::GodMode]
            );
        }
    }

    #[test]
    fn mismatching_case_mixed_into_partial_match_recovers() {
        // type "id" + junk that fails, then the full cheat again
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "idqx").is_empty());
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Retail), "iddqd"),
            vec![CheatEvent::GodMode]
        );
    }

    #[test]
    fn clev_warp_retail() {
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Retail), "idclev32"),
            vec![CheatEvent::Warp { episode: 3, map: 2 }]
        );
    }

    #[test]
    fn clev_warp_shareware_episode_limit() {
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Shareware), "idclev11"),
            vec![CheatEvent::Warp { episode: 1, map: 1 }]
        );
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Shareware), "idclev21").is_empty());
    }

    #[test]
    fn clev_warp_registered_episode_limit() {
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Registered), "idclev39"),
            vec![CheatEvent::Warp { episode: 3, map: 9 }]
        );
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Registered), "idclev49").is_empty());
    }

    #[test]
    fn clev_warp_commercial_map_range() {
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Commercial), "idclev15"),
            vec![CheatEvent::Warp {
                episode: 0,
                map: 15
            }]
        );
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Commercial), "idclev34"),
            vec![CheatEvent::Warp {
                episode: 0,
                map: 34
            }]
        );
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Commercial), "idclev35").is_empty());
        // map 00 is not a level
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Commercial), "idclev00").is_empty());
    }

    #[test]
    fn clev_warp_retail_caps() {
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Retail), "idclev49"),
            vec![CheatEvent::Warp { episode: 4, map: 9 }]
        );
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "idclev59").is_empty());
        let mut table = CheatTable::new();
        // episode 0 is not valid in retail/registered/shareware
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "idclev09").is_empty());
    }

    #[test]
    fn mus_cheat_captures_raw_digits() {
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Retail), "idmus03"),
            vec![CheatEvent::Music { digits: *b"03" }]
        );
    }

    #[test]
    fn clev_param_slots_capture_raw_keys() {
        // parameters are captured, not scramble-matched: digits pass raw
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, ctx(GameMode::Retail), "idclev12"),
            vec![CheatEvent::Warp { episode: 1, map: 2 }]
        );
    }

    #[test]
    fn cheats_blocked_in_netgame_except_clev() {
        let net = CheatContext {
            gamemode: GameMode::Retail,
            netgame: true,
            deathmatch: false,
        };
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, net, "iddqd").is_empty());
        assert!(type_seq(&mut table, net, "idfa").is_empty());
        assert!(type_seq(&mut table, net, "idbehold").is_empty());
        // idclev is not gated on netgame in the original
        assert_eq!(
            type_seq(&mut table, net, "idclev12"),
            vec![CheatEvent::Warp { episode: 1, map: 2 }]
        );
    }

    #[test]
    fn iddt_blocked_in_deathmatch_only() {
        let dm = CheatContext {
            gamemode: GameMode::Retail,
            netgame: false,
            deathmatch: true,
        };
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, dm, "iddt").is_empty());
        let plain = ctx(GameMode::Retail);
        let mut table = CheatTable::new();
        assert_eq!(
            type_seq(&mut table, plain, "iddt"),
            vec![CheatEvent::Automap]
        );
    }

    #[test]
    fn partial_param_then_end_of_input_does_not_fire() {
        let mut table = CheatTable::new();
        assert!(type_seq(&mut table, ctx(GameMode::Retail), "idclev3").is_empty());
    }

    #[test]
    fn garbage_before_cheat_does_not_matter() {
        let mut table = CheatTable::new();
        let mut events = Vec::new();
        for &b in b"hello world!" {
            events.extend(table.check(b, ctx(GameMode::Retail)));
        }
        events.extend(type_seq(&mut table, ctx(GameMode::Retail), "iddqd"));
        assert_eq!(events, vec![CheatEvent::GodMode]);
    }
}

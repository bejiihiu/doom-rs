//! Sound and music tables, ported from `sounds.c` (`S_sfx` / `S_music`) and
//! `sounds.h` (`sfxenum_t` / `musicenum_t`), plus the mixing constants from
//! `s_sound.c`.
//!
//! SFX lumps in the WAD are named `ds` + the sfx name, uppercased
//! (vanilla `sprintf(namebuf, "ds%s", sfx->name)`, i_sound.c). Music lumps
//! are `d_` + the music name (vanilla `sprintf(namebuf, "d_%s", music->name)`,
//! s_sound.c). [`SfxId::lump_name`] / [`MusicId::lump_name`] build those.
//!
//! `data`, `lumpnum` and `usefulness` / `handle` are runtime state in
//! vanilla (`sfxinfo_t`, `musicinfo_t`); they live in the sound subsystem,
//! not in these static tables.

use crate::fixed::{FRACBITS, FRACUNIT};

/// Maximum volume of a sound (vanilla `S_MAX_VOLUME`, s_sound.c).
pub const S_MAX_VOLUME: i32 = 127;
/// When to clip out sounds (vanilla `S_CLIPPING_DIST`, s_sound.c).
pub const S_CLIPPING_DIST: i32 = 1200 * FRACUNIT;
/// Distance from origin when sounds should be at maximum volume
/// (vanilla `S_CLOSE_DIST`, s_sound.c).
pub const S_CLOSE_DIST: i32 = 160 * FRACUNIT;
/// One over the attenuator (vanilla `S_ATTENUATOR`, s_sound.c).
pub const S_ATTENUATOR: i32 = (S_CLIPPING_DIST - S_CLOSE_DIST) >> FRACBITS;
/// Normal pitch (vanilla `NORM_PITCH`, s_sound.c). A linked sfx may override it.
pub const NORM_PITCH: i32 = 128;
/// Normal priority (vanilla `NORM_PRIORITY`, s_sound.c).
pub const NORM_PRIORITY: i32 = 64;
/// Pitch perturbation unit (vanilla `S_PITCH_PERTURB`, s_sound.c).
pub const S_PITCH_PERTURB: i32 = 1;

/// Total number of music entries (vanilla `NUMMUSIC`), dummy included.
pub const MUSIC_COUNT: usize = 68;
/// Total number of sfx entries (vanilla `NUMSFX`), dummy included.
pub const SFX_COUNT: usize = 109;

#[repr(u16)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MusicId {
    mus_None = 0,
    mus_e1m1 = 1,
    mus_e1m2 = 2,
    mus_e1m3 = 3,
    mus_e1m4 = 4,
    mus_e1m5 = 5,
    mus_e1m6 = 6,
    mus_e1m7 = 7,
    mus_e1m8 = 8,
    mus_e1m9 = 9,
    mus_e2m1 = 10,
    mus_e2m2 = 11,
    mus_e2m3 = 12,
    mus_e2m4 = 13,
    mus_e2m5 = 14,
    mus_e2m6 = 15,
    mus_e2m7 = 16,
    mus_e2m8 = 17,
    mus_e2m9 = 18,
    mus_e3m1 = 19,
    mus_e3m2 = 20,
    mus_e3m3 = 21,
    mus_e3m4 = 22,
    mus_e3m5 = 23,
    mus_e3m6 = 24,
    mus_e3m7 = 25,
    mus_e3m8 = 26,
    mus_e3m9 = 27,
    mus_inter = 28,
    mus_intro = 29,
    mus_bunny = 30,
    mus_victor = 31,
    mus_introa = 32,
    mus_runnin = 33,
    mus_stalks = 34,
    mus_countd = 35,
    mus_betwee = 36,
    mus_doom = 37,
    mus_the_da = 38,
    mus_shawn = 39,
    mus_ddtblu = 40,
    mus_in_cit = 41,
    mus_dead = 42,
    mus_stlks2 = 43,
    mus_theda2 = 44,
    mus_doom2 = 45,
    mus_ddtbl2 = 46,
    mus_runni2 = 47,
    mus_dead2 = 48,
    mus_stlks3 = 49,
    mus_romero = 50,
    mus_shawn2 = 51,
    mus_messag = 52,
    mus_count2 = 53,
    mus_ddtbl3 = 54,
    mus_ampie = 55,
    mus_theda3 = 56,
    mus_adrian = 57,
    mus_messg2 = 58,
    mus_romer2 = 59,
    mus_tense = 60,
    mus_shawn3 = 61,
    mus_openin = 62,
    mus_evil = 63,
    mus_ultima = 64,
    mus_read_m = 65,
    mus_dm2ttl = 66,
    mus_dm2int = 67,
}

impl MusicId {
    /// Total number of music entries.
    pub const COUNT: usize = MUSIC_COUNT;

    /// Vanilla identifier, e.g. `mus_e1m1`.
    pub const fn name(self) -> &'static str {
        MUSIC_NAMES[self as usize]
    }

    /// The static [`MusicInfo`] row for this id.
    pub const fn info(self) -> &'static MusicInfo {
        &MUSIC[self as usize]
    }

    /// WAD lump for this track: `d_` + name, uppercased
    /// (vanilla `sprintf(namebuf, "d_%s", music->name)`).
    /// [`MusicId::mus_None`] has no lump.
    pub const fn lump_name(self) -> Option<[u8; 8]> {
        let name = match self.info().name {
            Some(name) => name,
            None => return None,
        };
        let bytes = name.as_bytes();
        let mut out = [0u8; 8];
        out[0] = b'D';
        out[1] = b'_';
        let mut i = 0;
        while i < bytes.len() && i < 6 {
            let mut c = bytes[i];
            if c >= b'a' && c <= b'z' {
                c -= 32;
            }
            out[2 + i] = c;
            i += 1;
        }
        Some(out)
    }
}

const MUSIC_NAMES: [&str; MUSIC_COUNT] = [
    "mus_None",
    "mus_e1m1",
    "mus_e1m2",
    "mus_e1m3",
    "mus_e1m4",
    "mus_e1m5",
    "mus_e1m6",
    "mus_e1m7",
    "mus_e1m8",
    "mus_e1m9",
    "mus_e2m1",
    "mus_e2m2",
    "mus_e2m3",
    "mus_e2m4",
    "mus_e2m5",
    "mus_e2m6",
    "mus_e2m7",
    "mus_e2m8",
    "mus_e2m9",
    "mus_e3m1",
    "mus_e3m2",
    "mus_e3m3",
    "mus_e3m4",
    "mus_e3m5",
    "mus_e3m6",
    "mus_e3m7",
    "mus_e3m8",
    "mus_e3m9",
    "mus_inter",
    "mus_intro",
    "mus_bunny",
    "mus_victor",
    "mus_introa",
    "mus_runnin",
    "mus_stalks",
    "mus_countd",
    "mus_betwee",
    "mus_doom",
    "mus_the_da",
    "mus_shawn",
    "mus_ddtblu",
    "mus_in_cit",
    "mus_dead",
    "mus_stlks2",
    "mus_theda2",
    "mus_doom2",
    "mus_ddtbl2",
    "mus_runni2",
    "mus_dead2",
    "mus_stlks3",
    "mus_romero",
    "mus_shawn2",
    "mus_messag",
    "mus_count2",
    "mus_ddtbl3",
    "mus_ampie",
    "mus_theda3",
    "mus_adrian",
    "mus_messg2",
    "mus_romer2",
    "mus_tense",
    "mus_shawn3",
    "mus_openin",
    "mus_evil",
    "mus_ultima",
    "mus_read_m",
    "mus_dm2ttl",
    "mus_dm2int",
];

/// One row of vanilla `S_music` (`musicinfo_t`).
///
/// `data` / `lumpnum` / `handle` are runtime state and live in the sound
/// subsystem, not here. `mus_None` (index 0) is a dummy entry with no lump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MusicInfo {
    /// Lump stem without the `d_` prefix; `None` for vanilla `mus_None`.
    pub name: Option<&'static str>,
}

/// The full vanilla `S_music` table, indexed by [`MusicId`].
pub static MUSIC: [MusicInfo; MUSIC_COUNT] = [
    MusicInfo { name: None },         // mus_None
    MusicInfo { name: Some("e1m1") }, // mus_e1m1
    MusicInfo { name: Some("e1m2") }, // mus_e1m2
    MusicInfo { name: Some("e1m3") }, // mus_e1m3
    MusicInfo { name: Some("e1m4") }, // mus_e1m4
    MusicInfo { name: Some("e1m5") }, // mus_e1m5
    MusicInfo { name: Some("e1m6") }, // mus_e1m6
    MusicInfo { name: Some("e1m7") }, // mus_e1m7
    MusicInfo { name: Some("e1m8") }, // mus_e1m8
    MusicInfo { name: Some("e1m9") }, // mus_e1m9
    MusicInfo { name: Some("e2m1") }, // mus_e2m1
    MusicInfo { name: Some("e2m2") }, // mus_e2m2
    MusicInfo { name: Some("e2m3") }, // mus_e2m3
    MusicInfo { name: Some("e2m4") }, // mus_e2m4
    MusicInfo { name: Some("e2m5") }, // mus_e2m5
    MusicInfo { name: Some("e2m6") }, // mus_e2m6
    MusicInfo { name: Some("e2m7") }, // mus_e2m7
    MusicInfo { name: Some("e2m8") }, // mus_e2m8
    MusicInfo { name: Some("e2m9") }, // mus_e2m9
    MusicInfo { name: Some("e3m1") }, // mus_e3m1
    MusicInfo { name: Some("e3m2") }, // mus_e3m2
    MusicInfo { name: Some("e3m3") }, // mus_e3m3
    MusicInfo { name: Some("e3m4") }, // mus_e3m4
    MusicInfo { name: Some("e3m5") }, // mus_e3m5
    MusicInfo { name: Some("e3m6") }, // mus_e3m6
    MusicInfo { name: Some("e3m7") }, // mus_e3m7
    MusicInfo { name: Some("e3m8") }, // mus_e3m8
    MusicInfo { name: Some("e3m9") }, // mus_e3m9
    MusicInfo {
        name: Some("inter"),
    }, // mus_inter
    MusicInfo {
        name: Some("intro"),
    }, // mus_intro
    MusicInfo {
        name: Some("bunny"),
    }, // mus_bunny
    MusicInfo {
        name: Some("victor"),
    }, // mus_victor
    MusicInfo {
        name: Some("introa"),
    }, // mus_introa
    MusicInfo {
        name: Some("runnin"),
    }, // mus_runnin
    MusicInfo {
        name: Some("stalks"),
    }, // mus_stalks
    MusicInfo {
        name: Some("countd"),
    }, // mus_countd
    MusicInfo {
        name: Some("betwee"),
    }, // mus_betwee
    MusicInfo { name: Some("doom") }, // mus_doom
    MusicInfo {
        name: Some("the_da"),
    }, // mus_the_da
    MusicInfo {
        name: Some("shawn"),
    }, // mus_shawn
    MusicInfo {
        name: Some("ddtblu"),
    }, // mus_ddtblu
    MusicInfo {
        name: Some("in_cit"),
    }, // mus_in_cit
    MusicInfo { name: Some("dead") }, // mus_dead
    MusicInfo {
        name: Some("stlks2"),
    }, // mus_stlks2
    MusicInfo {
        name: Some("theda2"),
    }, // mus_theda2
    MusicInfo {
        name: Some("doom2"),
    }, // mus_doom2
    MusicInfo {
        name: Some("ddtbl2"),
    }, // mus_ddtbl2
    MusicInfo {
        name: Some("runni2"),
    }, // mus_runni2
    MusicInfo {
        name: Some("dead2"),
    }, // mus_dead2
    MusicInfo {
        name: Some("stlks3"),
    }, // mus_stlks3
    MusicInfo {
        name: Some("romero"),
    }, // mus_romero
    MusicInfo {
        name: Some("shawn2"),
    }, // mus_shawn2
    MusicInfo {
        name: Some("messag"),
    }, // mus_messag
    MusicInfo {
        name: Some("count2"),
    }, // mus_count2
    MusicInfo {
        name: Some("ddtbl3"),
    }, // mus_ddtbl3
    MusicInfo {
        name: Some("ampie"),
    }, // mus_ampie
    MusicInfo {
        name: Some("theda3"),
    }, // mus_theda3
    MusicInfo {
        name: Some("adrian"),
    }, // mus_adrian
    MusicInfo {
        name: Some("messg2"),
    }, // mus_messg2
    MusicInfo {
        name: Some("romer2"),
    }, // mus_romer2
    MusicInfo {
        name: Some("tense"),
    }, // mus_tense
    MusicInfo {
        name: Some("shawn3"),
    }, // mus_shawn3
    MusicInfo {
        name: Some("openin"),
    }, // mus_openin
    MusicInfo { name: Some("evil") }, // mus_evil
    MusicInfo {
        name: Some("ultima"),
    }, // mus_ultima
    MusicInfo {
        name: Some("read_m"),
    }, // mus_read_m
    MusicInfo {
        name: Some("dm2ttl"),
    }, // mus_dm2ttl
    MusicInfo {
        name: Some("dm2int"),
    }, // mus_dm2int
];

#[repr(u16)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SfxId {
    sfx_None = 0,
    sfx_pistol = 1,
    sfx_shotgn = 2,
    sfx_sgcock = 3,
    sfx_dshtgn = 4,
    sfx_dbopn = 5,
    sfx_dbcls = 6,
    sfx_dbload = 7,
    sfx_plasma = 8,
    sfx_bfg = 9,
    sfx_sawup = 10,
    sfx_sawidl = 11,
    sfx_sawful = 12,
    sfx_sawhit = 13,
    sfx_rlaunc = 14,
    sfx_rxplod = 15,
    sfx_firsht = 16,
    sfx_firxpl = 17,
    sfx_pstart = 18,
    sfx_pstop = 19,
    sfx_doropn = 20,
    sfx_dorcls = 21,
    sfx_stnmov = 22,
    sfx_swtchn = 23,
    sfx_swtchx = 24,
    sfx_plpain = 25,
    sfx_dmpain = 26,
    sfx_popain = 27,
    sfx_vipain = 28,
    sfx_mnpain = 29,
    sfx_pepain = 30,
    sfx_slop = 31,
    sfx_itemup = 32,
    sfx_wpnup = 33,
    sfx_oof = 34,
    sfx_telept = 35,
    sfx_posit1 = 36,
    sfx_posit2 = 37,
    sfx_posit3 = 38,
    sfx_bgsit1 = 39,
    sfx_bgsit2 = 40,
    sfx_sgtsit = 41,
    sfx_cacsit = 42,
    sfx_brssit = 43,
    sfx_cybsit = 44,
    sfx_spisit = 45,
    sfx_bspsit = 46,
    sfx_kntsit = 47,
    sfx_vilsit = 48,
    sfx_mansit = 49,
    sfx_pesit = 50,
    sfx_sklatk = 51,
    sfx_sgtatk = 52,
    sfx_skepch = 53,
    sfx_vilatk = 54,
    sfx_claw = 55,
    sfx_skeswg = 56,
    sfx_pldeth = 57,
    sfx_pdiehi = 58,
    sfx_podth1 = 59,
    sfx_podth2 = 60,
    sfx_podth3 = 61,
    sfx_bgdth1 = 62,
    sfx_bgdth2 = 63,
    sfx_sgtdth = 64,
    sfx_cacdth = 65,
    sfx_skldth = 66,
    sfx_brsdth = 67,
    sfx_cybdth = 68,
    sfx_spidth = 69,
    sfx_bspdth = 70,
    sfx_vildth = 71,
    sfx_kntdth = 72,
    sfx_pedth = 73,
    sfx_skedth = 74,
    sfx_posact = 75,
    sfx_bgact = 76,
    sfx_dmact = 77,
    sfx_bspact = 78,
    sfx_bspwlk = 79,
    sfx_vilact = 80,
    sfx_noway = 81,
    sfx_barexp = 82,
    sfx_punch = 83,
    sfx_hoof = 84,
    sfx_metal = 85,
    sfx_chgun = 86,
    sfx_tink = 87,
    sfx_bdopn = 88,
    sfx_bdcls = 89,
    sfx_itmbk = 90,
    sfx_flame = 91,
    sfx_flamst = 92,
    sfx_getpow = 93,
    sfx_bospit = 94,
    sfx_boscub = 95,
    sfx_bossit = 96,
    sfx_bospn = 97,
    sfx_bosdth = 98,
    sfx_manatk = 99,
    sfx_mandth = 100,
    sfx_sssit = 101,
    sfx_ssdth = 102,
    sfx_keenpn = 103,
    sfx_keendt = 104,
    sfx_skeact = 105,
    sfx_skesit = 106,
    sfx_skeatk = 107,
    sfx_radio = 108,
}

impl SfxId {
    /// Total number of sfx entries.
    pub const COUNT: usize = SFX_COUNT;

    /// Vanilla identifier, e.g. `sfx_pistol`.
    pub const fn name(self) -> &'static str {
        SFX_NAMES[self as usize]
    }

    /// The static [`SfxInfo`] row for this id.
    pub const fn info(self) -> &'static SfxInfo {
        &SFX[self as usize]
    }

    /// WAD lump for this sfx: `DS` + name, uppercased
    /// (vanilla `sprintf(namebuf, "ds%s", sfx->name)`), NUL-padded.
    /// `sfx_None` maps to `DSNONE` - it has no real lump either way.
    pub const fn lump_name(self) -> [u8; 8] {
        let bytes = self.info().name.as_bytes();
        let mut out = [0u8; 8];
        out[0] = b'D';
        out[1] = b'S';
        let mut i = 0;
        while i < bytes.len() && i < 6 {
            let mut c = bytes[i];
            if c >= b'a' && c <= b'z' {
                c -= 32;
            }
            out[2 + i] = c;
            i += 1;
        }
        out
    }
}

const SFX_NAMES: [&str; SFX_COUNT] = [
    "sfx_None",
    "sfx_pistol",
    "sfx_shotgn",
    "sfx_sgcock",
    "sfx_dshtgn",
    "sfx_dbopn",
    "sfx_dbcls",
    "sfx_dbload",
    "sfx_plasma",
    "sfx_bfg",
    "sfx_sawup",
    "sfx_sawidl",
    "sfx_sawful",
    "sfx_sawhit",
    "sfx_rlaunc",
    "sfx_rxplod",
    "sfx_firsht",
    "sfx_firxpl",
    "sfx_pstart",
    "sfx_pstop",
    "sfx_doropn",
    "sfx_dorcls",
    "sfx_stnmov",
    "sfx_swtchn",
    "sfx_swtchx",
    "sfx_plpain",
    "sfx_dmpain",
    "sfx_popain",
    "sfx_vipain",
    "sfx_mnpain",
    "sfx_pepain",
    "sfx_slop",
    "sfx_itemup",
    "sfx_wpnup",
    "sfx_oof",
    "sfx_telept",
    "sfx_posit1",
    "sfx_posit2",
    "sfx_posit3",
    "sfx_bgsit1",
    "sfx_bgsit2",
    "sfx_sgtsit",
    "sfx_cacsit",
    "sfx_brssit",
    "sfx_cybsit",
    "sfx_spisit",
    "sfx_bspsit",
    "sfx_kntsit",
    "sfx_vilsit",
    "sfx_mansit",
    "sfx_pesit",
    "sfx_sklatk",
    "sfx_sgtatk",
    "sfx_skepch",
    "sfx_vilatk",
    "sfx_claw",
    "sfx_skeswg",
    "sfx_pldeth",
    "sfx_pdiehi",
    "sfx_podth1",
    "sfx_podth2",
    "sfx_podth3",
    "sfx_bgdth1",
    "sfx_bgdth2",
    "sfx_sgtdth",
    "sfx_cacdth",
    "sfx_skldth",
    "sfx_brsdth",
    "sfx_cybdth",
    "sfx_spidth",
    "sfx_bspdth",
    "sfx_vildth",
    "sfx_kntdth",
    "sfx_pedth",
    "sfx_skedth",
    "sfx_posact",
    "sfx_bgact",
    "sfx_dmact",
    "sfx_bspact",
    "sfx_bspwlk",
    "sfx_vilact",
    "sfx_noway",
    "sfx_barexp",
    "sfx_punch",
    "sfx_hoof",
    "sfx_metal",
    "sfx_chgun",
    "sfx_tink",
    "sfx_bdopn",
    "sfx_bdcls",
    "sfx_itmbk",
    "sfx_flame",
    "sfx_flamst",
    "sfx_getpow",
    "sfx_bospit",
    "sfx_boscub",
    "sfx_bossit",
    "sfx_bospn",
    "sfx_bosdth",
    "sfx_manatk",
    "sfx_mandth",
    "sfx_sssit",
    "sfx_ssdth",
    "sfx_keenpn",
    "sfx_keendt",
    "sfx_skeact",
    "sfx_skesit",
    "sfx_skeatk",
    "sfx_radio",
];

/// One row of vanilla `S_sfx` (`sfxinfo_t`).
///
/// `lumpnum` / `usefulness` / `data` are runtime state and live in the
/// sound subsystem, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SfxInfo {
    /// Lump stem without the `DS` prefix (vanilla `name`), e.g. `pistol`.
    pub name: &'static str,
    /// Only one instance at a time (vanilla `singularity`).
    pub singularity: bool,
    /// Channel priority (vanilla `priority`); higher wins
    /// (`NORM_PRIORITY` is the baseline).
    pub priority: i32,
    /// If set, this is a pitch-shifted variant of another sfx
    /// (vanilla `link`, e.g. `chgun` -> `pistol`). The linked pitch/volume
    /// only apply when `link` is set.
    pub link: Option<SfxId>,
    /// Pitch of the linked variant (vanilla `pitch`); `-1` = vanilla default
    /// (`NORM_PITCH`).
    pub pitch: i32,
    /// Volume adjustment of the linked variant (vanilla `volume`); `-1` =
    /// vanilla default.
    pub volume: i32,
}

/// The full vanilla `S_sfx` table, indexed by [`SfxId`].
pub static SFX: [SfxInfo; SFX_COUNT] = [
    SfxInfo {
        name: "none",
        singularity: false,
        priority: 0,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_None
    SfxInfo {
        name: "pistol",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pistol
    SfxInfo {
        name: "shotgn",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_shotgn
    SfxInfo {
        name: "sgcock",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sgcock
    SfxInfo {
        name: "dshtgn",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dshtgn
    SfxInfo {
        name: "dbopn",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dbopn
    SfxInfo {
        name: "dbcls",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dbcls
    SfxInfo {
        name: "dbload",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dbload
    SfxInfo {
        name: "plasma",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_plasma
    SfxInfo {
        name: "bfg",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bfg
    SfxInfo {
        name: "sawup",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sawup
    SfxInfo {
        name: "sawidl",
        singularity: false,
        priority: 118,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sawidl
    SfxInfo {
        name: "sawful",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sawful
    SfxInfo {
        name: "sawhit",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sawhit
    SfxInfo {
        name: "rlaunc",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_rlaunc
    SfxInfo {
        name: "rxplod",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_rxplod
    SfxInfo {
        name: "firsht",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_firsht
    SfxInfo {
        name: "firxpl",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_firxpl
    SfxInfo {
        name: "pstart",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pstart
    SfxInfo {
        name: "pstop",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pstop
    SfxInfo {
        name: "doropn",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_doropn
    SfxInfo {
        name: "dorcls",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dorcls
    SfxInfo {
        name: "stnmov",
        singularity: false,
        priority: 119,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_stnmov
    SfxInfo {
        name: "swtchn",
        singularity: false,
        priority: 78,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_swtchn
    SfxInfo {
        name: "swtchx",
        singularity: false,
        priority: 78,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_swtchx
    SfxInfo {
        name: "plpain",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_plpain
    SfxInfo {
        name: "dmpain",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dmpain
    SfxInfo {
        name: "popain",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_popain
    SfxInfo {
        name: "vipain",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_vipain
    SfxInfo {
        name: "mnpain",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_mnpain
    SfxInfo {
        name: "pepain",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pepain
    SfxInfo {
        name: "slop",
        singularity: false,
        priority: 78,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_slop
    SfxInfo {
        name: "itemup",
        singularity: true,
        priority: 78,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_itemup
    SfxInfo {
        name: "wpnup",
        singularity: true,
        priority: 78,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_wpnup
    SfxInfo {
        name: "oof",
        singularity: false,
        priority: 96,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_oof
    SfxInfo {
        name: "telept",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_telept
    SfxInfo {
        name: "posit1",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_posit1
    SfxInfo {
        name: "posit2",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_posit2
    SfxInfo {
        name: "posit3",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_posit3
    SfxInfo {
        name: "bgsit1",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bgsit1
    SfxInfo {
        name: "bgsit2",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bgsit2
    SfxInfo {
        name: "sgtsit",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sgtsit
    SfxInfo {
        name: "cacsit",
        singularity: true,
        priority: 98,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_cacsit
    SfxInfo {
        name: "brssit",
        singularity: true,
        priority: 94,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_brssit
    SfxInfo {
        name: "cybsit",
        singularity: true,
        priority: 92,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_cybsit
    SfxInfo {
        name: "spisit",
        singularity: true,
        priority: 90,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_spisit
    SfxInfo {
        name: "bspsit",
        singularity: true,
        priority: 90,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bspsit
    SfxInfo {
        name: "kntsit",
        singularity: true,
        priority: 90,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_kntsit
    SfxInfo {
        name: "vilsit",
        singularity: true,
        priority: 90,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_vilsit
    SfxInfo {
        name: "mansit",
        singularity: true,
        priority: 90,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_mansit
    SfxInfo {
        name: "pesit",
        singularity: true,
        priority: 90,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pesit
    SfxInfo {
        name: "sklatk",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sklatk
    SfxInfo {
        name: "sgtatk",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sgtatk
    SfxInfo {
        name: "skepch",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skepch
    SfxInfo {
        name: "vilatk",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_vilatk
    SfxInfo {
        name: "claw",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_claw
    SfxInfo {
        name: "skeswg",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skeswg
    SfxInfo {
        name: "pldeth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pldeth
    SfxInfo {
        name: "pdiehi",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pdiehi
    SfxInfo {
        name: "podth1",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_podth1
    SfxInfo {
        name: "podth2",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_podth2
    SfxInfo {
        name: "podth3",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_podth3
    SfxInfo {
        name: "bgdth1",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bgdth1
    SfxInfo {
        name: "bgdth2",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bgdth2
    SfxInfo {
        name: "sgtdth",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sgtdth
    SfxInfo {
        name: "cacdth",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_cacdth
    SfxInfo {
        name: "skldth",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skldth
    SfxInfo {
        name: "brsdth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_brsdth
    SfxInfo {
        name: "cybdth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_cybdth
    SfxInfo {
        name: "spidth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_spidth
    SfxInfo {
        name: "bspdth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bspdth
    SfxInfo {
        name: "vildth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_vildth
    SfxInfo {
        name: "kntdth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_kntdth
    SfxInfo {
        name: "pedth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_pedth
    SfxInfo {
        name: "skedth",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skedth
    SfxInfo {
        name: "posact",
        singularity: true,
        priority: 120,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_posact
    SfxInfo {
        name: "bgact",
        singularity: true,
        priority: 120,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bgact
    SfxInfo {
        name: "dmact",
        singularity: true,
        priority: 120,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_dmact
    SfxInfo {
        name: "bspact",
        singularity: true,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bspact
    SfxInfo {
        name: "bspwlk",
        singularity: true,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bspwlk
    SfxInfo {
        name: "vilact",
        singularity: true,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_vilact
    SfxInfo {
        name: "noway",
        singularity: false,
        priority: 78,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_noway
    SfxInfo {
        name: "barexp",
        singularity: false,
        priority: 60,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_barexp
    SfxInfo {
        name: "punch",
        singularity: false,
        priority: 64,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_punch
    SfxInfo {
        name: "hoof",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_hoof
    SfxInfo {
        name: "metal",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_metal
    SfxInfo {
        name: "chgun",
        singularity: false,
        priority: 64,
        link: Some(SfxId::sfx_pistol),
        pitch: 150,
        volume: 0,
    }, // sfx_chgun
    SfxInfo {
        name: "tink",
        singularity: false,
        priority: 60,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_tink
    SfxInfo {
        name: "bdopn",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bdopn
    SfxInfo {
        name: "bdcls",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bdcls
    SfxInfo {
        name: "itmbk",
        singularity: false,
        priority: 100,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_itmbk
    SfxInfo {
        name: "flame",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_flame
    SfxInfo {
        name: "flamst",
        singularity: false,
        priority: 32,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_flamst
    SfxInfo {
        name: "getpow",
        singularity: false,
        priority: 60,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_getpow
    SfxInfo {
        name: "bospit",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bospit
    SfxInfo {
        name: "boscub",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_boscub
    SfxInfo {
        name: "bossit",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bossit
    SfxInfo {
        name: "bospn",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bospn
    SfxInfo {
        name: "bosdth",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_bosdth
    SfxInfo {
        name: "manatk",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_manatk
    SfxInfo {
        name: "mandth",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_mandth
    SfxInfo {
        name: "sssit",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_sssit
    SfxInfo {
        name: "ssdth",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_ssdth
    SfxInfo {
        name: "keenpn",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_keenpn
    SfxInfo {
        name: "keendt",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_keendt
    SfxInfo {
        name: "skeact",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skeact
    SfxInfo {
        name: "skesit",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skesit
    SfxInfo {
        name: "skeatk",
        singularity: false,
        priority: 70,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_skeatk
    SfxInfo {
        name: "radio",
        singularity: false,
        priority: 60,
        link: None,
        pitch: -1,
        volume: -1,
    }, // sfx_radio
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_counts_match_vanilla() {
        assert_eq!(MUSIC_COUNT, 68);
        assert_eq!(SFX_COUNT, 109);
        assert_eq!(MUSIC.len(), MUSIC_COUNT);
        assert_eq!(SFX.len(), SFX_COUNT);
    }

    #[test]
    fn mixing_constants_match_s_sound_c() {
        assert_eq!(S_MAX_VOLUME, 127);
        assert_eq!(S_CLIPPING_DIST, 1200 * FRACUNIT);
        assert_eq!(S_CLOSE_DIST, 160 * FRACUNIT);
        assert_eq!(S_ATTENUATOR, 1040); // (1200-160)*FRACUNIT >> FRACBITS
        assert_eq!(NORM_PITCH, 128);
        assert_eq!(NORM_PRIORITY, 64);
        assert_eq!(S_PITCH_PERTURB, 1);
    }

    #[test]
    fn sfx_rows_are_valid() {
        for (i, sfx) in SFX.iter().enumerate() {
            assert!(
                (0..=127).contains(&sfx.priority),
                "sfx {i} priority out of range"
            );
            assert!(
                sfx.pitch == -1 || (0..=255).contains(&sfx.pitch),
                "sfx {i} pitch out of range"
            );
            assert!(
                sfx.volume == -1 || (0..=127).contains(&sfx.volume),
                "sfx {i} volume out of range"
            );
            assert!(!sfx.name.is_empty(), "sfx {i} empty name");
            assert!(sfx.name.len() <= 6, "sfx {i} name too long");
        }
        assert_eq!(SFX[SfxId::sfx_None as usize].name, "none");
        assert_eq!(SFX[SfxId::sfx_pistol as usize].priority, 64);
        // vanilla: chgun is a pitch-shifted variant of the pistol
        let chgun = &SFX[SfxId::sfx_chgun as usize];
        assert_eq!(chgun.link, Some(SfxId::sfx_pistol));
        assert_eq!(chgun.pitch, 150);
        assert_eq!(chgun.volume, 0);
    }

    #[test]
    fn sfx_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for sfx in SFX.iter() {
            assert!(seen.insert(sfx.name), "duplicate sfx name {}", sfx.name);
        }
    }

    #[test]
    fn music_rows_are_valid() {
        assert!(MUSIC[MusicId::mus_None as usize].name.is_none());
        let mut seen = std::collections::HashSet::new();
        for (i, music) in MUSIC.iter().enumerate() {
            if let Some(name) = music.name {
                assert!((1..=6).contains(&name.len()), "music {i} bad name len");
                assert!(seen.insert(name), "music {i} duplicate name {name}");
            }
        }
    }

    #[test]
    fn lump_names_match_vanilla_format() {
        assert_eq!(&SfxId::sfx_pistol.lump_name(), b"DSPISTOL");
        assert_eq!(&SfxId::sfx_chgun.lump_name(), b"DSCHGUN\0");
        assert_eq!(&SfxId::sfx_None.lump_name(), b"DSNONE\0\0");
        assert_eq!(MusicId::mus_None.lump_name(), None);
        assert_eq!(MusicId::mus_e1m1.lump_name(), Some(*b"D_E1M1\0\0"));
        assert_eq!(MusicId::mus_runnin.lump_name(), Some(*b"D_RUNNIN"));
        assert_eq!(MusicId::mus_dm2int.lump_name(), Some(*b"D_DM2INT"));
    }

    #[test]
    fn names_lookup_roundtrips() {
        assert_eq!(SfxId::sfx_pistol.name(), "sfx_pistol");
        assert_eq!(MusicId::mus_e1m1.name(), "mus_e1m1");
        assert_eq!(MusicId::mus_None.name(), "mus_None");
        assert_eq!(SfxId::sfx_radio.name(), "sfx_radio");
    }
}

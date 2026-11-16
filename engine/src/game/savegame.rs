//! The save game block, ported from `p_saveg.c` (`P_ArchivePlayers`,
//! `P_ArchiveWorld`, `P_ArchiveThinkers`, `P_ArchiveSpecials` and their
//! un-archiving counterparts).
//!
//! Format semantics mirror the original: the file is self-contained,
//! little-endian, one pass to write and one pass to read. Unlike the C
//! original, whose format was `memcpy` of native structs (and therefore
//! unrecoverable across platforms), this format carries explicit field
//! widths and validates everything on the way in - a truncated or
//! corrupt save is an error, never a buffer overrun.
//!
//! The block is laid out as follows:
//!
//! ```text
//!   magic          8 bytes   "DOOMSAV\0"
//!   save_version   1 byte    1
//!   description    24 bytes  zero-padded display name (SAVESTRINGSIZE)
//!   game_mode      1 byte
//!   gameskill      1 byte
//!   gameepisode    1 byte
//!   gamemap        1 byte
//!   deathmatch     1 byte    0 off / 1 duel / 2 alt (vanilla 0/1/2)
//!   respawn_monst. 1 byte
//!   fast_monsters  1 byte
//!   no_monsters    1 byte
//!   consoleplayer  1 byte
//!   playeringame   4 bytes
//!   leveltime      4 bytes   i32
//!   gametic        4 bytes   i32
//!   total_kills    4 bytes
//!   total_items    4 bytes
//!   total_secret   4 bytes
//!   level_timer    4 bytes   i32, -1 = disabled
//!   rng_index      1 byte
//!   player_count   1 byte    players actually saved (in-game slots)
//!   players        player_count * sizeof(player)
//!   sector_count   4 bytes   u32
//!   sectors        sector_count * sizeof(SaveSector)
//!   line_count     4 bytes   u32
//!   lines          line_count * sizeof(SaveLine)
//!   side_count     4 bytes   u32
//!   sides          side_count * sizeof(SaveSidedef)
//!   thinker_count  4 bytes   u32
//!   thinkers       thinker_count * sizeof(thinker)
//!   button_count   4 bytes   u32
//!   buttons        button_count * sizeof(SaveButton)
//!   terminator     1 byte    0x1d (vanilla consistency marker)
//! ```
//!
//! Player/thinker cross references are stored as *indices into the
//! thinker array* (or -1), so they survive any slot recycling in the
//! arena. [`SaveBlock::bin_eq`] exists for the roundtrip test: writing a
//! decoded-then-rewritten block reproduces the original bytes exactly.

use crate::errors::{EngineError, EngineResult};
use crate::player::NUM_AMMO;
use crate::player::NUM_CARDS;
use crate::player::NUM_POWERS;
use crate::player::NUM_PSPRITES;
use crate::player::NUM_WEAPONS;

/// Magic bytes at the start of every save game.
pub const SAVE_MAGIC: [u8; 8] = *b"DOOMSAV\0";
/// Bump when the layout changes in an incompatible way.
pub const SAVE_VERSION: u8 = 1;
/// Display-name length in the save file (`SAVESTRINGSIZE`).
pub const SAVE_STRING_SIZE: usize = 24;
/// Vanilla terminator byte.
pub const SAVE_TERMINATOR: u8 = 0x1d;

const TC_MOBJ: u8 = 1;
const TC_DOOR: u8 = 2;
const TC_FLOOR: u8 = 3;
const TC_PLAT: u8 = 4;
const TC_CEILING: u8 = 5;
const TC_LIGHTFLASH: u8 = 6;
const TC_STROBE: u8 = 7;
const TC_GLOW: u8 = 8;
const TC_FIREFLICKER: u8 = 9;

/// One archived player. Field-for-field a match of what the original's
/// `memcpy(player_t)` carried, minus the pointer fields (avatar link,
/// message, attacker) which are rebuilt on load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePlayer {
    /// `PST_LIVE/DEAD/REBORN` as a raw index.
    pub playerstate: u8,
    pub health: i32,
    pub armorpoints: i32,
    pub armortype: i32,
    pub powers: [i32; NUM_POWERS],
    pub cards: [u8; NUM_CARDS],
    pub backpack: u8,
    pub frags: [i32; 4],
    pub readyweapon: u8,
    /// `0xff` for the vanilla `wp_nochange` sentinel.
    pub pendingweapon: u8,
    pub weaponowned: [u8; NUM_WEAPONS],
    pub ammo: [i32; NUM_AMMO],
    pub maxammo: [i32; NUM_AMMO],
    pub attackdown: u8,
    pub usedown: u8,
    pub cheats: u32,
    pub refire: i32,
    pub killcount: i32,
    pub itemcount: i32,
    pub secretcount: i32,
    pub damagecount: i32,
    pub bonuscount: i32,
    pub extralight: i32,
    pub fixedcolormap: i32,
    pub colormap: i32,
    pub didsecret: u8,
    pub viewz: i32,
    pub viewheight: i32,
    pub deltaviewheight: i32,
    pub bob: i32,
    /// Overlay psprites (`weapon` first, `flash` second).
    pub psprites: [SavePsp; NUM_PSPRITES],
}

impl Default for SavePlayer {
    fn default() -> Self {
        Self {
            playerstate: 0,
            health: 0,
            armorpoints: 0,
            armortype: 0,
            powers: [0; NUM_POWERS],
            cards: [0; NUM_CARDS],
            backpack: 0,
            frags: [0; 4],
            readyweapon: 0,
            pendingweapon: 0xff,
            weaponowned: [0; NUM_WEAPONS],
            ammo: [0; NUM_AMMO],
            maxammo: [0; NUM_AMMO],
            attackdown: 0,
            usedown: 0,
            cheats: 0,
            refire: 0,
            killcount: 0,
            itemcount: 0,
            secretcount: 0,
            damagecount: 0,
            bonuscount: 0,
            extralight: 0,
            fixedcolormap: 0,
            colormap: 0,
            didsecret: 0,
            viewz: 0,
            viewheight: 0,
            deltaviewheight: 0,
            bob: 0,
            psprites: [SavePsp::default(); NUM_PSPRITES],
        }
    }
}

/// One archived pspdef (`weapon`/`flash`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SavePsp {
    /// `0xffff` for the inactive (`NULL`) state.
    pub state: u16,
    pub tics: i32,
    pub sx: i32,
    pub sy: i32,
}

/// An archived mobj (`P_ArchiveThinkers`' `tc_mobj`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveMobj {
    pub mobj_type: u16,
    pub state: u16,
    pub tics: i32,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub angle: u32,
    pub momx: i32,
    pub momy: i32,
    pub momz: i32,
    pub flags: u32,
    pub health: i32,
    pub movedir: i32,
    pub movecount: i32,
    pub reactiontime: i32,
    pub threshold: i32,
    pub lastlook: i32,
    /// Which player slot this avatar belongs to, `-1` when not a player.
    pub player_index: i8,
    /// DoomEd spawn data (vanilla `spawnpoint`).
    pub spawn_x: i16,
    pub spawn_y: i16,
    pub spawn_angle: i16,
    pub spawn_type: i16,
    pub spawn_flags: i16,
    /// Cross references: positions in the thinker array, `-1` for none.
    pub target: i32,
    pub tracer: i32,
}

impl Default for SaveMobj {
    fn default() -> Self {
        Self {
            mobj_type: 0,
            state: 0,
            tics: 0,
            x: 0,
            y: 0,
            z: 0,
            angle: 0,
            momx: 0,
            momy: 0,
            momz: 0,
            flags: 0,
            health: 0,
            movedir: 0,
            movecount: 0,
            reactiontime: 0,
            threshold: 0,
            lastlook: 0,
            player_index: -1,
            spawn_x: 0,
            spawn_y: 0,
            spawn_angle: 0,
            spawn_type: 0,
            spawn_flags: 0,
            target: -1,
            tracer: -1,
        }
    }
}

/// An archived door thinker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveDoor {
    pub kind: u8,
    pub sector: i32,
    pub topheight: i32,
    pub speed: i32,
    pub direction: i32,
    pub topwait: i32,
    pub topcountdown: i32,
}

/// An archived floor mover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveFloor {
    pub kind: u8,
    pub crush: u8,
    pub sector: i32,
    pub direction: i32,
    pub new_special: i16,
    pub texture: [u8; 8],
    pub floor_dest_height: i32,
    pub speed: i32,
}

/// An archived plat thinker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePlat {
    pub kind: u8,
    pub sector: i32,
    pub speed: i32,
    pub low: i32,
    pub high: i32,
    pub wait: i32,
    pub count: i32,
    pub status: u8,
    pub oldstatus: u8,
    pub crush: u8,
    pub tag: i16,
}

/// An archived ceiling thinker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveCeiling {
    pub kind: u8,
    pub sector: i32,
    pub bottomheight: i32,
    pub topheight: i32,
    pub speed: i32,
    pub crush: u8,
    pub direction: i32,
    pub tag: i16,
    pub olddirection: i32,
}

/// An archived light flash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveLightFlash {
    pub sector: i32,
    pub count: i32,
    pub maxlight: i32,
    pub minlight: i32,
    pub maxtime: i32,
    pub mintime: i32,
}

/// An archived strobe flash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStrobeFlash {
    pub sector: i32,
    pub count: i32,
    pub minlight: i32,
    pub maxlight: i32,
    pub darktime: i32,
    pub brighttime: i32,
}

/// An archived glow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveGlow {
    pub sector: i32,
    pub minlight: i32,
    pub maxlight: i32,
    pub direction: i32,
}

/// An archived fire flicker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveFireFlicker {
    pub sector: i32,
    pub count: i32,
    pub maxlight: i32,
    pub minlight: i32,
}

/// An archived special thinker (`P_ArchiveSpecials`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveSpecial {
    Door(SaveDoor),
    Floor(SaveFloor),
    Plat(SavePlat),
    Ceiling(SaveCeiling),
    LightFlash(SaveLightFlash),
    StrobeFlash(SaveStrobeFlash),
    Glow(SaveGlow),
    FireFlicker(SaveFireFlicker),
}

/// One thinker entry in the block, tagged by class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveThinker {
    Mobj(SaveMobj),
    Special(SaveSpecial),
}

impl SaveThinker {
    fn class_byte(&self) -> u8 {
        match self {
            SaveThinker::Mobj(_) => TC_MOBJ,
            SaveThinker::Special(SaveSpecial::Door(_)) => TC_DOOR,
            SaveThinker::Special(SaveSpecial::Floor(_)) => TC_FLOOR,
            SaveThinker::Special(SaveSpecial::Plat(_)) => TC_PLAT,
            SaveThinker::Special(SaveSpecial::Ceiling(_)) => TC_CEILING,
            SaveThinker::Special(SaveSpecial::LightFlash(_)) => TC_LIGHTFLASH,
            SaveThinker::Special(SaveSpecial::StrobeFlash(_)) => TC_STROBE,
            SaveThinker::Special(SaveSpecial::Glow(_)) => TC_GLOW,
            SaveThinker::Special(SaveSpecial::FireFlicker(_)) => TC_FIREFLICKER,
        }
    }
}

/// An archived sector (`P_ArchiveWorld` first half). Heights carry
/// full 16.16 precision, not the original's whole units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveSector {
    pub floor_height: i32,
    pub ceiling_height: i32,
    pub floor_pic: [u8; 8],
    pub ceiling_pic: [u8; 8],
    pub light_level: i32,
    pub special: i32,
    pub tag: i32,
}

impl Default for SaveSector {
    fn default() -> Self {
        Self {
            floor_height: 0,
            ceiling_height: 0,
            floor_pic: *b"FLOOR4_8",
            ceiling_pic: *b"CEIL3_5\0",
            light_level: 0,
            special: 0,
            tag: 0,
        }
    }
}

/// An archived linedef. `side0`/`side1` say whether each sidedef exists;
/// the side records follow in order (front, back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveLine {
    pub flags: i32,
    pub special: i32,
    pub tag: i32,
    pub side0: u8,
    pub side1: u8,
}

/// An archived sidedef (`P_ArchiveWorld` second half).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveSidedef {
    pub texture_offset: i32,
    pub row_offset: i32,
    pub top: [u8; 8],
    pub bottom: [u8; 8],
    pub mid: [u8; 8],
}

/// An archived pressed button (`P_ArchiveSpecials` button list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveButton {
    pub line: i32,
    pub where_: u8,
    pub revert_to: [u8; 8],
    pub timer: i32,
    pub sector: i32,
}

/// A complete, decoded save. Plain data only - the `Game` layer
/// matches this against its live state when loading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveBlock {
    pub game_mode: u8,
    pub gameskill: u8,
    pub gameepisode: u8,
    pub gamemap: u8,
    pub deathmatch: u8,
    pub respawn_monsters: u8,
    pub fast_monsters: u8,
    pub no_monsters: u8,
    pub consoleplayer: u8,
    pub playeringame: [u8; 4],
    pub leveltime: i32,
    pub gametic: i32,
    pub total_kills: i32,
    pub total_items: i32,
    pub total_secret: i32,
    /// `-1` = disabled, vanilla's absent `levelTimer`.
    pub level_timer: i32,
    /// `rndindex`/`prndindex` for deterministic replay.
    pub rng_index: u8,
    pub players: Vec<SavePlayer>,
    pub sectors: Vec<SaveSector>,
    pub lines: Vec<SaveLine>,
    pub sides: Vec<SaveSidedef>,
    pub thinkers: Vec<SaveThinker>,
    pub buttons: Vec<SaveButton>,
}

/// Why a save game could not be loaded.
#[derive(Clone, Debug, thiserror::Error)]
pub enum SaveGameError {
    /// Not a save game at all (bad magic or version).
    #[error("not a save game (magic/version mismatch)")]
    BadMagic,
    /// Ran off the end of the data.
    #[error("truncated save data at byte {0}")]
    Truncated(usize),
    /// A consistency marker was missing or wrong.
    #[error("missing terminator (corrupt save game)")]
    MissingTerminator,
    /// A thinker class byte was not one of the known kinds.
    #[error("unknown thinker class {0} in save game")]
    UnknownThinkerClass(u8),
    /// A state or sector index pointed outside the tables.
    #[error("out of range index {what} ({index}) in save game")]
    OutOfRange { what: &'static str, index: i32 },
    /// The data doesn't match the level being played.
    #[error("save game does not match the level: {0}")]
    MapMismatch(String),
}

impl From<SaveGameError> for EngineError {
    fn from(err: SaveGameError) -> Self {
        EngineError::Resource(format!("save game: {err}"))
    }
}

// ------------------------------------------------------------------
// writer helpers
// ------------------------------------------------------------------

fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_name(out: &mut Vec<u8>, name: &[u8; 8]) {
    out.extend_from_slice(name);
}

// ------------------------------------------------------------------
// reader helpers
// ------------------------------------------------------------------

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> EngineResult<&'a [u8]> {
        if self.at + n > self.bytes.len() {
            return Err(SaveGameError::Truncated(self.at).into());
        }
        let out = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }

    fn u8(&mut self) -> EngineResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> EngineResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> EngineResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> EngineResult<i32> {
        Ok(self.u32()? as i32)
    }

    fn name(&mut self) -> EngineResult<[u8; 8]> {
        let b = self.take(8)?;
        Ok(b.try_into().expect("8 bytes from take(8)"))
    }
}

impl SaveBlock {
    /// Serialize the block into a self-contained byte vector.
    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(0x4000);

        out.extend_from_slice(&SAVE_MAGIC);
        out.push(SAVE_VERSION);
        // description placeholder: the caller patches the 24 bytes
        // after write() if it wants one; games set it through
        // `write_with_description`
        out.extend_from_slice(&[0u8; SAVE_STRING_SIZE]);

        out.push(self.game_mode);
        out.push(self.gameskill);
        out.push(self.gameepisode);
        out.push(self.gamemap);
        out.push(self.deathmatch);
        out.push(self.respawn_monsters);
        out.push(self.fast_monsters);
        out.push(self.no_monsters);
        out.push(self.consoleplayer);
        out.extend_from_slice(&self.playeringame);

        put_i32(&mut out, self.leveltime);
        put_i32(&mut out, self.gametic);
        put_i32(&mut out, self.total_kills);
        put_i32(&mut out, self.total_items);
        put_i32(&mut out, self.total_secret);
        put_i32(&mut out, self.level_timer);
        out.push(self.rng_index);

        // players
        out.push(self.players.len() as u8);
        for player in &self.players {
            write_player(&mut out, player);
        }

        // sectors
        put_u32(&mut out, self.sectors.len() as u32);
        for sector in &self.sectors {
            put_i32(&mut out, sector.floor_height);
            put_i32(&mut out, sector.ceiling_height);
            put_name(&mut out, &sector.floor_pic);
            put_name(&mut out, &sector.ceiling_pic);
            put_i32(&mut out, sector.light_level);
            put_i32(&mut out, sector.special);
            put_i32(&mut out, sector.tag);
        }

        // lines
        put_u32(&mut out, self.lines.len() as u32);
        for line in &self.lines {
            put_i32(&mut out, line.flags);
            put_i32(&mut out, line.special);
            put_i32(&mut out, line.tag);
            out.push(line.side0);
            out.push(line.side1);
        }

        // sides
        put_u32(&mut out, self.sides.len() as u32);
        for side in &self.sides {
            put_i32(&mut out, side.texture_offset);
            put_i32(&mut out, side.row_offset);
            put_name(&mut out, &side.top);
            put_name(&mut out, &side.bottom);
            put_name(&mut out, &side.mid);
        }

        // thinkers
        put_u32(&mut out, self.thinkers.len() as u32);
        for thinker in &self.thinkers {
            out.push(thinker.class_byte());
            match thinker {
                SaveThinker::Mobj(mobj) => {
                    put_u16(&mut out, mobj.mobj_type);
                    put_u16(&mut out, mobj.state);
                    put_i32(&mut out, mobj.tics);
                    put_i32(&mut out, mobj.x);
                    put_i32(&mut out, mobj.y);
                    put_i32(&mut out, mobj.z);
                    put_u32(&mut out, mobj.angle);
                    put_i32(&mut out, mobj.momx);
                    put_i32(&mut out, mobj.momy);
                    put_i32(&mut out, mobj.momz);
                    put_u32(&mut out, mobj.flags);
                    put_i32(&mut out, mobj.health);
                    put_i32(&mut out, mobj.movedir);
                    put_i32(&mut out, mobj.movecount);
                    put_i32(&mut out, mobj.reactiontime);
                    put_i32(&mut out, mobj.threshold);
                    put_i32(&mut out, mobj.lastlook);
                    out.push(mobj.player_index as u8);
                    out.extend_from_slice(&mobj.spawn_x.to_le_bytes());
                    out.extend_from_slice(&mobj.spawn_y.to_le_bytes());
                    out.extend_from_slice(&mobj.spawn_angle.to_le_bytes());
                    out.extend_from_slice(&mobj.spawn_type.to_le_bytes());
                    out.extend_from_slice(&mobj.spawn_flags.to_le_bytes());
                    put_i32(&mut out, mobj.target);
                    put_i32(&mut out, mobj.tracer);
                }
                SaveThinker::Special(special) => match special {
                    SaveSpecial::Door(door) => {
                        put_i32(&mut out, door.kind as i32);
                        put_i32(&mut out, door.sector);
                        put_i32(&mut out, door.topheight);
                        put_i32(&mut out, door.speed);
                        put_i32(&mut out, door.direction);
                        put_i32(&mut out, door.topwait);
                        put_i32(&mut out, door.topcountdown);
                    }
                    SaveSpecial::Floor(floor) => {
                        put_i32(&mut out, floor.kind as i32);
                        out.push(floor.crush);
                        put_i32(&mut out, floor.sector);
                        put_i32(&mut out, floor.direction);
                        out.extend_from_slice(&floor.new_special.to_le_bytes());
                        put_name(&mut out, &floor.texture);
                        put_i32(&mut out, floor.floor_dest_height);
                        put_i32(&mut out, floor.speed);
                    }
                    SaveSpecial::Plat(plat) => {
                        put_i32(&mut out, plat.kind as i32);
                        put_i32(&mut out, plat.sector);
                        put_i32(&mut out, plat.speed);
                        put_i32(&mut out, plat.low);
                        put_i32(&mut out, plat.high);
                        put_i32(&mut out, plat.wait);
                        put_i32(&mut out, plat.count);
                        out.push(plat.status);
                        out.push(plat.oldstatus);
                        out.push(plat.crush);
                        out.extend_from_slice(&plat.tag.to_le_bytes());
                    }
                    SaveSpecial::Ceiling(ceiling) => {
                        put_i32(&mut out, ceiling.kind as i32);
                        put_i32(&mut out, ceiling.sector);
                        put_i32(&mut out, ceiling.bottomheight);
                        put_i32(&mut out, ceiling.topheight);
                        put_i32(&mut out, ceiling.speed);
                        out.push(ceiling.crush);
                        put_i32(&mut out, ceiling.direction);
                        out.extend_from_slice(&ceiling.tag.to_le_bytes());
                        put_i32(&mut out, ceiling.olddirection);
                    }
                    SaveSpecial::LightFlash(flash) => {
                        put_i32(&mut out, flash.sector);
                        put_i32(&mut out, flash.count);
                        put_i32(&mut out, flash.maxlight);
                        put_i32(&mut out, flash.minlight);
                        put_i32(&mut out, flash.maxtime);
                        put_i32(&mut out, flash.mintime);
                    }
                    SaveSpecial::StrobeFlash(strobe) => {
                        put_i32(&mut out, strobe.sector);
                        put_i32(&mut out, strobe.count);
                        put_i32(&mut out, strobe.minlight);
                        put_i32(&mut out, strobe.maxlight);
                        put_i32(&mut out, strobe.darktime);
                        put_i32(&mut out, strobe.brighttime);
                    }
                    SaveSpecial::Glow(glow) => {
                        put_i32(&mut out, glow.sector);
                        put_i32(&mut out, glow.minlight);
                        put_i32(&mut out, glow.maxlight);
                        put_i32(&mut out, glow.direction);
                    }
                    SaveSpecial::FireFlicker(flicker) => {
                        put_i32(&mut out, flicker.sector);
                        put_i32(&mut out, flicker.count);
                        put_i32(&mut out, flicker.maxlight);
                        put_i32(&mut out, flicker.minlight);
                    }
                },
            }
        }

        // buttons
        put_u32(&mut out, self.buttons.len() as u32);
        for button in &self.buttons {
            put_i32(&mut out, button.line);
            out.push(button.where_);
            put_name(&mut out, &button.revert_to);
            put_i32(&mut out, button.timer);
            put_i32(&mut out, button.sector);
        }

        out.push(SAVE_TERMINATOR);
        out
    }

    /// Serialize with a display name written into the description field.
    pub fn write_with_description(&self, description: &str) -> Vec<u8> {
        let mut bytes = self.write();
        let mut desc = [0u8; SAVE_STRING_SIZE];
        let src = description.as_bytes();
        let len = src.len().min(SAVE_STRING_SIZE);
        desc[..len].copy_from_slice(&src[..len]);
        bytes[SAVE_MAGIC.len() + 1..SAVE_MAGIC.len() + 1 + SAVE_STRING_SIZE].copy_from_slice(&desc);
        bytes
    }

    /// Decode a block, checking structure only. Semantic validation
    /// (state ids, sector indices, ...) happens in the game layer.
    pub fn read(bytes: &[u8]) -> EngineResult<Self> {
        let mut reader = Reader::new(bytes);

        let magic = reader.take(SAVE_MAGIC.len())?;
        if magic != SAVE_MAGIC {
            return Err(SaveGameError::BadMagic.into());
        }
        let version = reader.u8()?;
        if version != SAVE_VERSION {
            return Err(SaveGameError::BadMagic.into());
        }

        // skip the description; the game layer reads it with
        // `read_description` when listing slots
        reader.take(SAVE_STRING_SIZE)?;

        let game_mode = reader.u8()?;
        let gameskill = reader.u8()?;
        let gameepisode = reader.u8()?;
        let gamemap = reader.u8()?;
        let deathmatch = reader.u8()?;
        let respawn_monsters = reader.u8()?;
        let fast_monsters = reader.u8()?;
        let no_monsters = reader.u8()?;
        let consoleplayer = reader.u8()?;
        let mut playeringame = [0u8; 4];
        playeringame.copy_from_slice(reader.take(4)?);

        let leveltime = reader.i32()?;
        let gametic = reader.i32()?;
        let total_kills = reader.i32()?;
        let total_items = reader.i32()?;
        let total_secret = reader.i32()?;
        let level_timer = reader.i32()?;
        let rng_index = reader.u8()?;

        let player_count = reader.u8()? as usize;
        let mut players = Vec::with_capacity(player_count);
        for _ in 0..player_count {
            players.push(read_player(&mut reader)?);
        }

        let sector_count = reader.u32()? as usize;
        let mut sectors = Vec::with_capacity(sector_count);
        for _ in 0..sector_count {
            sectors.push(SaveSector {
                floor_height: reader.i32()?,
                ceiling_height: reader.i32()?,
                floor_pic: reader.name()?,
                ceiling_pic: reader.name()?,
                light_level: reader.i32()?,
                special: reader.i32()?,
                tag: reader.i32()?,
            });
        }

        let line_count = reader.u32()? as usize;
        let mut lines = Vec::with_capacity(line_count);
        for _ in 0..line_count {
            lines.push(SaveLine {
                flags: reader.i32()?,
                special: reader.i32()?,
                tag: reader.i32()?,
                side0: reader.u8()?,
                side1: reader.u8()?,
            });
        }

        let side_count = reader.u32()? as usize;
        let mut sides = Vec::with_capacity(side_count);
        for _ in 0..side_count {
            sides.push(SaveSidedef {
                texture_offset: reader.i32()?,
                row_offset: reader.i32()?,
                top: reader.name()?,
                bottom: reader.name()?,
                mid: reader.name()?,
            });
        }

        let thinker_count = reader.u32()? as usize;
        let mut thinkers = Vec::with_capacity(thinker_count);
        for _ in 0..thinker_count {
            let class = reader.u8()?;
            thinkers.push(read_thinker(&mut reader, class)?);
        }

        let button_count = reader.u32()? as usize;
        let mut buttons = Vec::with_capacity(button_count);
        for _ in 0..button_count {
            buttons.push(SaveButton {
                line: reader.i32()?,
                where_: reader.u8()?,
                revert_to: reader.name()?,
                timer: reader.i32()?,
                sector: reader.i32()?,
            });
        }

        let terminator = reader.u8()?;
        if terminator != SAVE_TERMINATOR {
            return Err(SaveGameError::MissingTerminator.into());
        }
        if reader.at != bytes.len() {
            return Err(SaveGameError::Truncated(reader.at).into());
        }

        Ok(Self {
            game_mode,
            gameskill,
            gameepisode,
            gamemap,
            deathmatch,
            respawn_monsters,
            fast_monsters,
            no_monsters,
            consoleplayer,
            playeringame,
            leveltime,
            gametic,
            total_kills,
            total_items,
            total_secret,
            level_timer,
            rng_index,
            players,
            sectors,
            lines,
            sides,
            thinkers,
            buttons,
        })
    }

    /// Read the display name out of a raw save file, if any.
    pub fn read_description(bytes: &[u8]) -> Option<String> {
        let start = SAVE_MAGIC.len() + 1;
        let end = start + SAVE_STRING_SIZE;
        let desc = bytes.get(start..end)?;
        let end = desc.iter().position(|&b| b == 0).unwrap_or(desc.len());
        Some(String::from_utf8_lossy(&desc[..end]).to_string())
    }

    /// True when writing `self` reproduces `bytes` exactly. The
    /// roundtrip test leans on this.
    pub fn bin_eq(&self, bytes: &[u8]) -> bool {
        self.write() == bytes
    }
}

fn write_player(out: &mut Vec<u8>, player: &SavePlayer) {
    out.push(player.playerstate);
    put_i32(out, player.health);
    put_i32(out, player.armorpoints);
    put_i32(out, player.armortype);
    for power in player.powers {
        put_i32(out, power);
    }
    out.extend_from_slice(&player.cards);
    out.push(player.backpack);
    for frag in player.frags {
        put_i32(out, frag);
    }
    out.push(player.readyweapon);
    out.push(player.pendingweapon);
    out.extend_from_slice(&player.weaponowned);
    for ammo in player.ammo {
        put_i32(out, ammo);
    }
    for maxammo in player.maxammo {
        put_i32(out, maxammo);
    }
    out.push(player.attackdown);
    out.push(player.usedown);
    put_u32(out, player.cheats);
    put_i32(out, player.refire);
    put_i32(out, player.killcount);
    put_i32(out, player.itemcount);
    put_i32(out, player.secretcount);
    put_i32(out, player.damagecount);
    put_i32(out, player.bonuscount);
    put_i32(out, player.extralight);
    put_i32(out, player.fixedcolormap);
    put_i32(out, player.colormap);
    out.push(player.didsecret);
    put_i32(out, player.viewz);
    put_i32(out, player.viewheight);
    put_i32(out, player.deltaviewheight);
    put_i32(out, player.bob);
    for psp in player.psprites {
        put_u16(out, psp.state);
        put_i32(out, psp.tics);
        put_i32(out, psp.sx);
        put_i32(out, psp.sy);
    }
}

fn read_player(reader: &mut Reader<'_>) -> EngineResult<SavePlayer> {
    let playerstate = reader.u8()?;
    let health = reader.i32()?;
    let armorpoints = reader.i32()?;
    let armortype = reader.i32()?;
    let mut powers = [0; NUM_POWERS];
    for power in &mut powers {
        *power = reader.i32()?;
    }
    let mut cards = [0; NUM_CARDS];
    cards.copy_from_slice(reader.take(NUM_CARDS)?);
    let backpack = reader.u8()?;
    let mut frags = [0; 4];
    for frag in &mut frags {
        *frag = reader.i32()?;
    }
    let readyweapon = reader.u8()?;
    let pendingweapon = reader.u8()?;
    let mut weaponowned = [0; NUM_WEAPONS];
    weaponowned.copy_from_slice(reader.take(NUM_WEAPONS)?);
    let mut ammo = [0; NUM_AMMO];
    for slot in &mut ammo {
        *slot = reader.i32()?;
    }
    let mut maxammo = [0; NUM_AMMO];
    for slot in &mut maxammo {
        *slot = reader.i32()?;
    }
    let attackdown = reader.u8()?;
    let usedown = reader.u8()?;
    let cheats = reader.u32()?;
    let refire = reader.i32()?;
    let killcount = reader.i32()?;
    let itemcount = reader.i32()?;
    let secretcount = reader.i32()?;
    let damagecount = reader.i32()?;
    let bonuscount = reader.i32()?;
    let extralight = reader.i32()?;
    let fixedcolormap = reader.i32()?;
    let colormap = reader.i32()?;
    let didsecret = reader.u8()?;
    let viewz = reader.i32()?;
    let viewheight = reader.i32()?;
    let deltaviewheight = reader.i32()?;
    let bob = reader.i32()?;
    let mut psprites = [SavePsp::default(); NUM_PSPRITES];
    for psp in &mut psprites {
        psp.state = reader.u16()?;
        psp.tics = reader.i32()?;
        psp.sx = reader.i32()?;
        psp.sy = reader.i32()?;
    }

    Ok(SavePlayer {
        playerstate,
        health,
        armorpoints,
        armortype,
        powers,
        cards,
        backpack,
        frags,
        readyweapon,
        pendingweapon,
        weaponowned,
        ammo,
        maxammo,
        attackdown,
        usedown,
        cheats,
        refire,
        killcount,
        itemcount,
        secretcount,
        damagecount,
        bonuscount,
        extralight,
        fixedcolormap,
        colormap,
        didsecret,
        viewz,
        viewheight,
        deltaviewheight,
        bob,
        psprites,
    })
}

fn read_thinker(reader: &mut Reader<'_>, class: u8) -> EngineResult<SaveThinker> {
    Ok(match class {
        TC_MOBJ => SaveThinker::Mobj(SaveMobj {
            mobj_type: reader.u16()?,
            state: reader.u16()?,
            tics: reader.i32()?,
            x: reader.i32()?,
            y: reader.i32()?,
            z: reader.i32()?,
            angle: reader.u32()?,
            momx: reader.i32()?,
            momy: reader.i32()?,
            momz: reader.i32()?,
            flags: reader.u32()?,
            health: reader.i32()?,
            movedir: reader.i32()?,
            movecount: reader.i32()?,
            reactiontime: reader.i32()?,
            threshold: reader.i32()?,
            lastlook: reader.i32()?,
            player_index: reader.u8()? as i8,
            spawn_x: reader.u16()? as i16,
            spawn_y: reader.u16()? as i16,
            spawn_angle: reader.u16()? as i16,
            spawn_type: reader.u16()? as i16,
            spawn_flags: reader.u16()? as i16,
            target: reader.i32()?,
            tracer: reader.i32()?,
        }),
        TC_DOOR => SaveThinker::Special(SaveSpecial::Door(SaveDoor {
            kind: reader.i32()? as u8,
            sector: reader.i32()?,
            topheight: reader.i32()?,
            speed: reader.i32()?,
            direction: reader.i32()?,
            topwait: reader.i32()?,
            topcountdown: reader.i32()?,
        })),
        TC_FLOOR => SaveThinker::Special(SaveSpecial::Floor(SaveFloor {
            kind: reader.i32()? as u8,
            crush: reader.u8()?,
            sector: reader.i32()?,
            direction: reader.i32()?,
            new_special: reader.u16()? as i16,
            texture: reader.name()?,
            floor_dest_height: reader.i32()?,
            speed: reader.i32()?,
        })),
        TC_PLAT => SaveThinker::Special(SaveSpecial::Plat(SavePlat {
            kind: reader.i32()? as u8,
            sector: reader.i32()?,
            speed: reader.i32()?,
            low: reader.i32()?,
            high: reader.i32()?,
            wait: reader.i32()?,
            count: reader.i32()?,
            status: reader.u8()?,
            oldstatus: reader.u8()?,
            crush: reader.u8()?,
            tag: reader.u16()? as i16,
        })),
        TC_CEILING => SaveThinker::Special(SaveSpecial::Ceiling(SaveCeiling {
            kind: reader.i32()? as u8,
            sector: reader.i32()?,
            bottomheight: reader.i32()?,
            topheight: reader.i32()?,
            speed: reader.i32()?,
            crush: reader.u8()?,
            direction: reader.i32()?,
            tag: reader.u16()? as i16,
            olddirection: reader.i32()?,
        })),
        TC_LIGHTFLASH => SaveThinker::Special(SaveSpecial::LightFlash(SaveLightFlash {
            sector: reader.i32()?,
            count: reader.i32()?,
            maxlight: reader.i32()?,
            minlight: reader.i32()?,
            maxtime: reader.i32()?,
            mintime: reader.i32()?,
        })),
        TC_STROBE => SaveThinker::Special(SaveSpecial::StrobeFlash(SaveStrobeFlash {
            sector: reader.i32()?,
            count: reader.i32()?,
            minlight: reader.i32()?,
            maxlight: reader.i32()?,
            darktime: reader.i32()?,
            brighttime: reader.i32()?,
        })),
        TC_GLOW => SaveThinker::Special(SaveSpecial::Glow(SaveGlow {
            sector: reader.i32()?,
            minlight: reader.i32()?,
            maxlight: reader.i32()?,
            direction: reader.i32()?,
        })),
        TC_FIREFLICKER => SaveThinker::Special(SaveSpecial::FireFlicker(SaveFireFlicker {
            sector: reader.i32()?,
            count: reader.i32()?,
            maxlight: reader.i32()?,
            minlight: reader.i32()?,
        })),
        other => return Err(SaveGameError::UnknownThinkerClass(other).into()),
    })
}

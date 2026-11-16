//! The per-player state: a port of `player_t` (`d_player.h`, including the
//! `ticcmd_t` from `d_ticcmd.h`) plus the small enums it leans on.
//!
//! A player avatar is a regular [`crate::mobj::Mobj`] just like a monster,
//! with `mobj_type == MobjType::MT_PLAYER`; the linked [`Player`] carries
//! the rest (view position, inventory, cheats, psprites, ...). The link is
//! by index both ways: the avatar stores its player index in
//! `mobj.player`, the player stores the avatar's handle in `mo`.
//!
//! Spawning players is intentionally not done here: that path needs
//! psprite setup, the status bar and the HUD, which land with the game
//! loop implementation. The split mirrors vanilla, where `P_SpawnPlayer`
//! also lives at the boundary of those systems.

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::StateId;
use crate::thinker::ThinkerId;

/// Vanilla `MAXPLAYERS`.
pub const MAXPLAYERS: usize = 4;
/// Vanilla `MAXHEALTH`.
pub const MAXHEALTH: i32 = 100;
/// Vanilla `VIEWHEIGHT` (41 units).
pub const VIEWHEIGHT: Fixed = Fixed::from_raw(41 * FRACUNIT);
/// Vanilla `NUMWEAPONS`.
pub const NUM_WEAPONS: usize = 9;
/// Vanilla `NUMAMMO`.
pub const NUM_AMMO: usize = 4;
/// Vanilla `NUMCARDS`.
pub const NUM_CARDS: usize = 6;
/// Vanilla `NUMPOWERS`.
pub const NUM_POWERS: usize = 6;
/// Vanilla `NUMPSPRITES`.
pub const NUM_PSPRITES: usize = 2;

/// Vanilla `CF_*` cheat bits (`d_player.h`).
pub const CF_NOCLIP: u32 = 1;
/// See [`CF_NOCLIP`].
pub const CF_GODMODE: u32 = 2;
/// See [`CF_NOCLIP`].
pub const CF_NOMOMENTUM: u32 = 4;

/// Vanilla `playerstate_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlayerState {
    /// Playing or camping.
    Live,
    /// Dead on the ground, view follows the killer.
    Dead,
    /// Ready to restart/respawn.
    Reborn,
}

/// Vanilla `weapontype_t` (without the `wp_nochange` sentinel, which is
/// modeled as `pendingweapon: Option<Weapon>`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Weapon {
    Fist = 0,
    Pistol = 1,
    Shotgun = 2,
    Chaingun = 3,
    MissileLauncher = 4,
    PlasmaRifle = 5,
    Bfg = 6,
    Chainsaw = 7,
    SuperShotgun = 8,
}

impl Weapon {
    /// Total number of weapons (vanilla `NUMWEAPONS`).
    pub const COUNT: usize = NUM_WEAPONS;

    /// Array index of this weapon.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Vanilla `ammotype_t`, without the `am_noammo` sentinel (chainsaw and
/// fist simply don't consume anything).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum AmmoKind {
    Clip = 0,
    Shell = 1,
    Cell = 2,
    Missile = 3,
}

impl AmmoKind {
    /// Total number of ammunition kinds (vanilla `NUMAMMO`).
    pub const COUNT: usize = NUM_AMMO;

    /// Array index of this ammo kind.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Vanilla `card_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Card {
    Blue = 0,
    Yellow = 1,
    Red = 2,
    BlueSkull = 3,
    YellowSkull = 4,
    RedSkull = 5,
}

impl Card {
    /// Total number of key cards/skulls (vanilla `NUMCARDS`).
    pub const COUNT: usize = NUM_CARDS;

    /// Array index of this key.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Vanilla `powertype_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Power {
    Invulnerability = 0,
    Strength = 1,
    Invisibility = 2,
    IronFeet = 3,
    AllMap = 4,
    Infrared = 5,
}

impl Power {
    /// Total number of power up artifacts (vanilla `NUMPOWERS`).
    pub const COUNT: usize = NUM_POWERS;

    /// Array index of this power up.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Vanilla `psprnum_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum PspNumber {
    Weapon = 0,
    Flash = 1,
}

impl PspNumber {
    /// Total number of psprites (vanilla `NUMPSPRITES`).
    pub const COUNT: usize = NUM_PSPRITES;

    /// Array index of this sprite slot.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The data sampled per tick (vanilla `ticcmd_t`, `d_ticcmd.h`).
///
/// Kept as plain small integers like the wire format; fixed-point
/// conversion happens at the consumers (movement, aiming).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct TicCmd {
    /// `*2048` for move.
    pub forward_move: i8,
    /// `*2048` for move.
    pub side_move: i8,
    /// `<<16` for the angle delta.
    pub angle_turn: i16,
    /// Consistency check for net play.
    pub consistancy: i16,
    /// `+` chat character.
    pub chat_char: u8,
    /// Held buttons.
    pub buttons: u8,
}

/// One overlay sprite (vanilla `pspdef_t`, `p_pspr.h`).
#[derive(Clone, Copy, Default, Debug)]
pub struct PspDef {
    /// Current state, or `None` for the vanilla NULL state pointer
    /// (sprite not active).
    pub state: Option<StateId>,
    /// Tics remaining in the current state.
    pub tics: i32,
    /// X origin of the sprite, fixed point.
    pub sx: Fixed,
    /// Y origin of the sprite, fixed point.
    pub sy: Fixed,
}

/// Vanilla `player_t`. Straight field-for-field port of `d_player.h`;
/// indices instead of pointers throughout (`mo`/`attacker` are thinker
/// handles, the avatar is `mobj_type == MobjType::MT_PLAYER`).
#[derive(Debug)]
pub struct Player {
    /// The mobj that is this player's body (vanilla `mo`).
    pub mo: Option<ThinkerId>,
    /// Live/dead/reborn (vanilla `playerstate`).
    pub playerstate: PlayerState,
    /// Last sampled ticcmd (vanilla `cmd`).
    pub cmd: TicCmd,
    /// View origin z (vanilla `viewz`).
    pub viewz: Fixed,
    /// Base height above floor for viewz (vanilla `viewheight`).
    pub viewheight: Fixed,
    /// Bobbing/squat speed (vanilla `deltaviewheight`).
    pub deltaviewheight: Fixed,
    /// Bounded/scaled total momentum for bobbing (vanilla `bob`).
    pub bob: Fixed,

    /// Health used between levels; `mo.health` is authoritative during
    /// play (vanilla `health`).
    pub health: i32,
    /// Points of armor (vanilla `armorpoints`).
    pub armorpoints: i32,
    /// Armor type, 0 = none, 1 = green, 2 = blue (vanilla `armortype`).
    pub armortype: i32,

    /// Power up tic counters; strength is effectively a flag
    /// (vanilla `powers`).
    pub powers: [i32; NUM_POWERS],
    /// Key cards and skull keys (vanilla `cards`).
    pub cards: [bool; NUM_CARDS],
    /// True once the backpack has been picked up (vanilla `backpack`).
    pub backpack: bool,

    /// Kills of other players, one per player slot (vanilla `frags`).
    pub frags: [i32; MAXPLAYERS],
    /// Currently held weapon (vanilla `readyweapon`).
    pub readyweapon: Weapon,
    /// Weapon switch target; `None` is the vanilla `wp_nochange`
    /// sentinel.
    pub pendingweapon: Option<Weapon>,
    /// Weapon ownership bits (vanilla `weaponowned`).
    pub weaponowned: [bool; NUM_WEAPONS],
    /// Current ammo per kind (vanilla `ammo`).
    pub ammo: [i32; NUM_AMMO],
    /// Max ammo per kind (vanilla `maxammo`).
    pub maxammo: [i32; NUM_AMMO],

    /// True if the attack button was down last tic (vanilla `attackdown`).
    pub attackdown: bool,
    /// True if the use button was down last tic (vanilla `usedown`).
    pub usedown: bool,

    /// Cheat bit flags, see `CF_*` (vanilla `cheats`).
    pub cheats: u32,
    /// Refired shots are less accurate (vanilla `refire`).
    pub refire: i32,

    /// Kill count for intermission stats (vanilla `killcount`).
    pub killcount: i32,
    /// Item count for intermission stats (vanilla `itemcount`).
    pub itemcount: i32,
    /// Secret count for intermission stats (vanilla `secretcount`).
    pub secretcount: i32,

    /// Message to show to the player; the HUD layer owns the actual
    /// rendering. Vanilla keeps arbitrary strings here.
    pub message: Option<String>,

    /// Screen flash strength from damage (vanilla `damagecount`).
    pub damagecount: i32,
    /// Screen flash strength from bonus pickup (vanilla `bonuscount`).
    pub bonuscount: i32,

    /// Who inflicted the damage, if any (vanilla `attacker`).
    pub attacker: Option<ThinkerId>,

    /// Bonus light from weapon flashes (vanilla `extralight`).
    pub extralight: i32,
    /// Fixed colormap override; 0 = normal (vanilla `fixedcolormap`).
    pub fixedcolormap: i32,
    /// Player skin color shift, 0-3 (vanilla `colormap`).
    pub colormap: i32,

    /// Overlay view sprites (gun, muzzle flash) (vanilla `psprites`).
    pub psprites: [PspDef; NUM_PSPRITES],

    /// True if the secret level has been found (vanilla `didsecret`).
    pub didsecret: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maxplayers_and_viewheight_match_the_original() {
        assert_eq!(MAXPLAYERS, 4);
        assert_eq!(MAXHEALTH, 100);
        assert_eq!(VIEWHEIGHT.to_raw(), 41 * crate::fixed::FRACUNIT);
    }

    #[test]
    fn enum_indices_match_the_vanilla_order() {
        assert_eq!(Weapon::Fist.index(), 0);
        assert_eq!(Weapon::SuperShotgun.index(), 8);
        assert_eq!(Weapon::COUNT, NUM_WEAPONS);
        assert_eq!(AmmoKind::Missile.index(), 3);
        assert_eq!(AmmoKind::COUNT, NUM_AMMO);
        assert_eq!(Card::RedSkull.index(), 5);
        assert_eq!(Card::COUNT, NUM_CARDS);
        assert_eq!(Power::Infrared.index(), 5);
        assert_eq!(Power::COUNT, NUM_POWERS);
        assert_eq!(PspNumber::Flash.index(), 1);
        assert_eq!(PspNumber::COUNT, NUM_PSPRITES);
    }

    #[test]
    fn ticcmd_defaults_to_zero() {
        let cmd = TicCmd::default();
        assert_eq!(cmd.forward_move, 0);
        assert_eq!(cmd.side_move, 0);
        assert_eq!(cmd.angle_turn, 0);
        assert_eq!(cmd.consistancy, 0);
        assert_eq!(cmd.chat_char, 0);
        assert_eq!(cmd.buttons, 0);
    }

    #[test]
    fn pspr_default_is_inactive() {
        let pspr = PspDef::default();
        assert_eq!(pspr.state, None);
        assert_eq!(pspr.tics, 0);
        assert_eq!(pspr.sx, Fixed::default());
        assert_eq!(pspr.sy, Fixed::default());
    }
}

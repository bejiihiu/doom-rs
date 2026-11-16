//! The face (mugshot) state machine, ported from `st_stuff.c`
//! (`ST_updateFaceWidget`, `ST_calcPainOffset`).
//!
//! The machine is driven once per tic with plain-data inputs — no mobj
//! pointers, the game layer computes the angles and timers. It returns
//! the face index into the `faces` patch array (see
//! [`super::ST_NUMFACES`]), which the bar then draws through a regular
//! multi-icon widget.
//!
//! Precedence, ported from the original: dead > evil grin (weapon
//! pickup while a bonus is active) > turn towards attacker / ouch >
//! hurt yourself > rapid firing > invulnerability > straight/looking
//! around.

use crate::tables::Angle;

/// Tics per second (the original hardcodes this everywhere).
pub const TICRATE: i32 = 35;

/// N/256*100 probability the normal face state changes; part of the
/// original source, kept for reference, unused by the machine.
pub const ST_FACEPROBABILITY: i32 = 96;

/// A health change beyond this in a tic counts as "much pain".
pub const ST_MUCHPAIN: i32 = 20;

// Face timing, in tics.
/// How long the evil grin lasts after a weapon pickup.
pub const ST_EVILGRINCOUNT: i32 = 2 * TICRATE;
/// How long the straight-ahead/looking face lasts.
pub const ST_STRAIGHTFACECOUNT: i32 = TICRATE / 2;
/// How long the turn-towards-attacker/ouch face lasts.
pub const ST_TURNCOUNT: i32 = TICRATE;
/// Unused by the original (it uses `ST_TURNCOUNT` for the ouch face);
/// kept for completeness.
pub const ST_OUCHCOUNT: i32 = TICRATE;
/// Rapid-fire face kicks in after this many tics of holding attack.
pub const ST_RAMPAGEDELAY: i32 = 2 * TICRATE;

// The face patch array layout: 5 pain levels x 8 face types, plus God and
// dead faces.
pub const ST_NUMPAINFACES: i32 = 5;
pub const ST_NUMSTRAIGHTFACES: i32 = 3;
pub const ST_NUMTURNFACES: i32 = 2;
pub const ST_NUMSPECIALFACES: i32 = 3;
/// Faces per pain level (3 straight + 2 turning + ouch + evil grin + rampage).
pub const ST_FACESTRIDE: i32 = ST_NUMSTRAIGHTFACES + ST_NUMTURNFACES + ST_NUMSPECIALFACES;
/// Number of faces outside the pain-level grid (God + dead).
pub const ST_NUMEXTRAFACES: i32 = 2;
/// Total number of face patches.
pub const ST_NUMFACES: i32 = ST_FACESTRIDE * ST_NUMPAINFACES + ST_NUMEXTRAFACES;

pub const ST_TURNOFFSET: i32 = ST_NUMSTRAIGHTFACES;
pub const ST_OUCHOFFSET: i32 = ST_TURNOFFSET + ST_NUMTURNFACES;
pub const ST_EVILGRINOFFSET: i32 = ST_OUCHOFFSET + 1;
pub const ST_RAMPAGEOFFSET: i32 = ST_EVILGRINOFFSET + 1;
/// Face shown while invulnerability/god mode is active.
pub const ST_GODFACE: i32 = ST_NUMPAINFACES * ST_FACESTRIDE;
/// Face shown when the player is dead.
pub const ST_DEADFACE: i32 = ST_GODFACE + 1;

/// Number of weapon types, mirrors `NUMWEAPONS` in `doomdef.h`.
pub const NUMWEAPONS: usize = 9;

/// `ST_calcPainOffset`: maps `health` (0..=100) onto the pain-level
/// stride — 0 when unhurt up to `4 * ST_FACESTRIDE` at zero health.
pub fn pain_offset(health: i32) -> i32 {
    let clamped = health.clamp(0, 100);
    ST_FACESTRIDE * (((100 - clamped) * ST_NUMPAINFACES) / 101)
}

/// Plain-data inputs for one face tic, filled in by the game layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceInputs {
    /// Current health.
    pub health: i32,
    /// `plyr->damagecount`: tics left in the current damage flash.
    pub damage_count: i32,
    /// `plyr->bonuscount`: tics left in the current bonus flash.
    pub bonus_count: i32,
    /// `plyr->attackdown`: the fire button is held.
    pub attack_down: bool,
    /// God mode *or* invulnerability — the original lumps both into one
    /// check for this face.
    pub invulnerability: bool,
    /// The player's view angle.
    pub player_angle: Angle,
    /// Bearing to the attacker as an absolute BAM angle, or `None` when
    /// the player was hurt by something without a position/attacker
    /// (the original's `plyr->attacker == plyr->mo` / NULL case).
    pub attacker_angle: Option<Angle>,
    /// Weapon ownership, all `NUMWEAPONS` entries; compared against the
    /// previous tic to detect a fresh weapon pickup (evil grin).
    pub weapon_owned: [bool; NUMWEAPONS],
    /// A fresh random value in `0..=255` for this tic (`M_Random`).
    pub random: i32,
}

/// The retained face-machine state (`st_faceindex`, `st_facecount`,
/// `priority`, `lastattackdown`, `oldweaponsowned`, `st_oldhealth`).
///
/// `new()` matches `ST_initData`: face index 0 (unhurt straight face),
/// priority 0.
pub struct FaceMachine {
    priority: i32,
    face_index: i32,
    face_count: i32,
    last_attack_down: i32,
    old_weapon_owned: [bool; NUMWEAPONS],
    old_health: i32,
}

impl Default for FaceMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl FaceMachine {
    pub fn new() -> Self {
        Self {
            priority: 0,
            face_index: 0,
            face_count: 0,
            last_attack_down: -1,
            old_weapon_owned: [false; NUMWEAPONS],
            old_health: -1,
        }
    }

    /// The face index selected by the last `tick`.
    pub fn face_index(&self) -> i32 {
        self.face_index
    }

    /// Advances the machine one tic and returns the selected face index.
    ///
    /// Mirrors `ST_updateFaceWidget` + the `st_oldhealth` bookkeeping
    /// that vanilla does in `ST_Ticker` (folded in here so the machine
    /// is self-contained).
    pub fn tick(&mut self, inputs: &FaceInputs) -> i32 {
        let pain = pain_offset(inputs.health);

        // dead — beats everything
        if self.priority < 10 && inputs.health == 0 {
            self.priority = 9;
            self.face_index = ST_DEADFACE;
            self.face_count = 1;
        }

        // picking up a weapon while the bonus flash is active: evil grin
        if self.priority < 9 && inputs.bonus_count != 0 {
            let mut do_evil_grin = false;
            for i in 0..NUMWEAPONS {
                if self.old_weapon_owned[i] != inputs.weapon_owned[i] {
                    do_evil_grin = true;
                    self.old_weapon_owned[i] = inputs.weapon_owned[i];
                }
            }
            if do_evil_grin {
                self.priority = 8;
                self.face_count = ST_EVILGRINCOUNT;
                self.face_index = pain + ST_EVILGRINOFFSET;
            }
        }

        // being attacked — turn towards the attacker, or ouch
        if self.priority < 8 && inputs.damage_count != 0 {
            if let Some(badguyangle) = inputs.attacker_angle {
                self.priority = 7;

                if inputs.health - self.old_health > ST_MUCHPAIN {
                    self.face_count = ST_TURNCOUNT;
                    self.face_index = pain + ST_OUCHOFFSET;
                } else {
                    let player_angle = inputs.player_angle;
                    // whether right or left; the raw u32 comparisons match the
                    // original's angle_t comparisons
                    let (diffang, turn_right) = if badguyangle.to_raw() > player_angle.to_raw() {
                        let diff = badguyangle - player_angle;
                        (diff, diff.to_raw() > Angle::ANG180.to_raw())
                    } else {
                        let diff = player_angle - badguyangle;
                        (diff, diff.to_raw() <= Angle::ANG180.to_raw())
                    };

                    self.face_count = ST_TURNCOUNT;
                    self.face_index = pain;

                    if diffang.to_raw() < Angle::ANG45.to_raw() {
                        // head-on
                        self.face_index += ST_RAMPAGEOFFSET;
                    } else if turn_right {
                        // turn face right
                        self.face_index += ST_TURNOFFSET;
                    } else {
                        // turn face left
                        self.face_index += ST_TURNOFFSET + 1;
                    }
                }
            }
        }

        // getting hurt because of your own damn stupidity
        if self.priority < 7 && inputs.damage_count != 0 {
            if inputs.health - self.old_health > ST_MUCHPAIN {
                self.priority = 7;
                self.face_count = ST_TURNCOUNT;
                self.face_index = pain + ST_OUCHOFFSET;
            } else {
                self.priority = 6;
                self.face_count = ST_TURNCOUNT;
                self.face_index = pain + ST_RAMPAGEOFFSET;
            }
        }

        // rapid firing
        if self.priority < 6 {
            if inputs.attack_down {
                if self.last_attack_down == -1 {
                    self.last_attack_down = ST_RAMPAGEDELAY;
                } else {
                    self.last_attack_down -= 1;
                    if self.last_attack_down == 0 {
                        self.priority = 5;
                        self.face_index = pain + ST_RAMPAGEOFFSET;
                        self.face_count = 1;
                        self.last_attack_down = 1;
                    }
                }
            } else {
                self.last_attack_down = -1;
            }
        }

        // invulnerability
        if self.priority < 5 && inputs.invulnerability {
            self.priority = 4;
            self.face_index = ST_GODFACE;
            self.face_count = 1;
        }

        // face count timed out: back to a (pseudo-random) straight face
        if self.face_count == 0 {
            self.face_index = pain + (inputs.random % 3);
            self.face_count = ST_STRAIGHTFACECOUNT;
            self.priority = 0;
        }

        self.face_count -= 1;
        self.old_health = inputs.health;

        self.face_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> FaceInputs {
        FaceInputs {
            health: 100,
            damage_count: 0,
            bonus_count: 0,
            attack_down: false,
            invulnerability: false,
            player_angle: Angle::ZERO,
            attacker_angle: None,
            weapon_owned: [false; NUMWEAPONS],
            random: 0,
        }
    }

    #[test]
    fn pain_offset_matches_vanilla_integer_math() {
        assert_eq!(pain_offset(100), 0);
        assert_eq!(pain_offset(99), 0);
        assert_eq!(pain_offset(79), ST_FACESTRIDE);
        assert_eq!(pain_offset(60), ST_FACESTRIDE);
        assert_eq!(pain_offset(59), 2 * ST_FACESTRIDE);
        assert_eq!(pain_offset(20), 3 * ST_FACESTRIDE);
        assert_eq!(pain_offset(1), 4 * ST_FACESTRIDE);
        assert_eq!(pain_offset(0), 4 * ST_FACESTRIDE);
        // out-of-range health is clamped, not wrapped
        assert_eq!(pain_offset(150), pain_offset(100));
        assert_eq!(pain_offset(-5), pain_offset(0));
    }

    #[test]
    fn dead_face_beats_everything() {
        let mut machine = FaceMachine::default();
        let mut inp = inputs();
        inp.health = 0;
        assert_eq!(machine.tick(&inp), ST_DEADFACE);
        // stays dead on later ticks
        assert_eq!(machine.tick(&inp), ST_DEADFACE);
    }

    #[test]
    fn god_face_when_invulnerable() {
        let mut machine = FaceMachine::default();
        let mut inp = inputs();
        inp.invulnerability = true;
        assert_eq!(machine.tick(&inp), ST_GODFACE);
    }

    #[test]
    fn self_inflicted_damage_gives_rampage_face() {
        let mut machine = FaceMachine::default();
        // warmup so old_health matches before the damage tick
        machine.tick(&inputs());
        let mut inp = inputs();
        inp.damage_count = 10;
        // no attacker: hurt by your own doing
        assert_eq!(machine.tick(&inp), ST_RAMPAGEOFFSET);
    }

    #[test]
    fn much_pain_gives_ouch_face() {
        let mut machine = FaceMachine::default();
        // warmup at low health, then a big positive delta in one tic
        let mut inp = inputs();
        inp.health = 60;
        machine.tick(&inp);
        inp.health = 100; // soulsphere style +40 with the damage flash on
        inp.damage_count = 10;
        inp.attacker_angle = Some(Angle::ZERO);
        assert_eq!(machine.tick(&inp), pain_offset(100) + ST_OUCHOFFSET);
    }

    #[test]
    fn much_pain_self_infliction_gives_ouch_face() {
        let mut machine = FaceMachine::default();
        let mut inp = inputs();
        inp.health = 60;
        machine.tick(&inp);
        inp.health = 100;
        inp.damage_count = 10; // no attacker: self damage branch
        assert_eq!(machine.tick(&inp), pain_offset(100) + ST_OUCHOFFSET);
    }

    #[test]
    fn turn_towards_attacker_on_the_left() {
        let mut machine = FaceMachine::default();
        machine.tick(&inputs()); // warmup, old_health = 100
        let mut inp = inputs();
        inp.damage_count = 10;
        inp.attacker_angle = Some(Angle::ANG90); // bearing 90 deg left
        // diffang = ANG90, between ANG45 and ANG180, so the face turns
        assert_eq!(machine.tick(&inp), ST_TURNOFFSET + 1);
    }

    #[test]
    fn turn_towards_attacker_head_on() {
        let mut machine = FaceMachine::default();
        machine.tick(&inputs());
        let mut inp = inputs();
        inp.damage_count = 10;
        inp.attacker_angle = Some(Angle::from_raw(Angle::ANG45.to_raw() - 1));
        // diffang just under ANG45: head-on rampage face
        assert_eq!(machine.tick(&inp), ST_RAMPAGEOFFSET);
    }

    #[test]
    fn turn_towards_attacker_on_the_right() {
        let mut machine = FaceMachine::default();
        machine.tick(&inputs());
        let mut inp = inputs();
        inp.damage_count = 10;
        // player facing east, attacker due south of the player:
        // badguy <= player, diff = ANG90, diff <= ANG180 -> turn right
        inp.player_angle = Angle::ANG90;
        inp.attacker_angle = Some(Angle::ZERO);
        assert_eq!(machine.tick(&inp), ST_TURNOFFSET);
    }

    #[test]
    fn evil_grin_on_weapon_pickup_while_bonus() {
        let mut machine = FaceMachine::default();
        machine.tick(&inputs()); // warmup caches old_weapon_owned = all false
        let mut inp = inputs();
        inp.bonus_count = 10;
        inp.weapon_owned[2] = true; // shotgun
        assert_eq!(machine.tick(&inp), ST_EVILGRINOFFSET);
        // same weapon_owned again: no re-trigger, the grin face holds
        let mut inp = inputs();
        inp.bonus_count = 10;
        inp.weapon_owned[2] = true;
        assert_eq!(machine.tick(&inp), ST_EVILGRINOFFSET);
    }

    #[test]
    fn straight_face_after_face_count_times_out() {
        let mut machine = FaceMachine::default();
        let mut inp = inputs();
        inp.random = 1;
        // fresh machine: face_count == 0, so the timeout branch fires
        assert_eq!(machine.tick(&inp), 1);
    }

    #[test]
    fn rampage_face_after_holding_attack() {
        let mut machine = FaceMachine::default();
        let mut inp = inputs();
        inp.attack_down = true;
        let mut last = 0;
        for _ in 0..ST_RAMPAGEDELAY + 1 {
            last = machine.tick(&inp);
        }
        assert_eq!(last, ST_RAMPAGEOFFSET);
    }

    #[test]
    fn invulnerability_loses_to_this_tics_self_damage() {
        let mut machine = FaceMachine::default();
        machine.tick(&inputs());
        let mut inp = inputs();
        inp.damage_count = 10;
        inp.invulnerability = true;
        // the self-damage branch claims priority 6 first (checks for
        // invulnerability need priority < 5), exactly like vanilla
        assert_eq!(machine.tick(&inp), ST_RAMPAGEOFFSET);
    }

    #[test]
    fn face_index_is_always_in_range() {
        let mut machine = FaceMachine::default();
        // walk a long stretch of tics with a deterministic LCG feeding
        // the random input; the face index must always be a valid patch
        let mut state = 0x1234_5678u32;
        for _ in 0..300 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let mut inp = inputs();
            inp.random = (state >> 24) as i32 % 256;
            inp.damage_count = if (state & 7) < 2 { 5 } else { 0 };
            inp.attack_down = (state & 0x10) != 0;
            machine.tick(&inp);
            assert!((0..ST_NUMFACES).contains(&machine.face_index()));
        }
    }
}

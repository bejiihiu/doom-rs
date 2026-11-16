//! The end-of-episode / end-of-game finale, ported from `f_finale.c`
//! (linuxdoom-1.10).
//!
//! What is here:
//!
//! * **finale text** (stage 0, `F_TextWrite`) — the scrolling credits for
//!   every DOOM 1 episode (`E1TEXT..E4TEXT`) and every DOOM II exit map
//!   (`C1TEXT..C6TEXT`), tiled with the episode's flat and typed out
//!   `TEXTSPEED` pixels per tic with the HUD font;
//! * **art screens** (stage 1, `F_Drawer` stage 1) — the HELP2/CREDIT,
//!   VICTORY2, ENDPIC patches and the E3 "bunny scroll" (`F_BunnyScroll`)
//!   with the day-by-day END0..END6 frames;
//! * **the cast call** (stage 2, `F_StartCast`/`F_CastTicker`/`F_CastResponder`/`F_CastDrawer`) —
//!   present in this source, and reachable exactly like the original:
//!   after a DOOM II MAP30 finale, once any player button is pressed.
//!   It is *not* reachable for DOOM 1 episodes (the original never enters
//!   it there either).
//!
//! The P1TEXT/P2TEXT/T1TEXT... strings in `d_englsh.h` are deliberately
//! not ported: `f_finale.c` never references them (they belong to the
//! Plutonia/TNT expansions, which this source base does not wire up).
//!
//! The text assets and the state indices below are byte-exact ports;
//! every `S_*` index was counted from the `statenum_t` order in `info.h`
//! (the source has no explicit enumerator values). The state/mobjinfo
//! tables themselves are **not** in this module — they land with the
//! info tables. The cast call talks to them through [`crate::finale::CastTables`]:
//! the game layer (or, in tests, a synthetic fixture) implements one
//! `actor()` and one `state()` method and the entire 1.10 cast logic —
//! attack cycles, melee/missile alternation, death on key press — runs
//! on top. The sound remapping table (`switch (st)` in `F_CastTicker`)
//! is provided as a default [`crate::finale::CastTables::state_sfx`] implementation, so
//! it lives here, next to the logic that uses it.
//!
//! The framebuffer is the output; the game layer owns palette
//! application, wipes (this module only *requests* them via
//! [`crate::finale::FinaleEvent::ForceWipe`]) and the sound system (via
//! [`crate::finale::FinaleEvent::Music`]/[`crate::finale::FinaleEvent::Sound`]).

use crate::errors::{EngineError, EngineResult};
use crate::gfx::flat::Flat;
use crate::gfx::patch::{Patch, decode_patch};
use crate::hud::{HudFont, text_width};
use crate::render::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::resources::ResourceManager;
use crate::stbar::draw_patch;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// The finale texts (`d_englsh.h`, used by `f_finale.c`)
// ---------------------------------------------------------------------------

/// `E1TEXT` in the original.
pub const E1TEXT: &str = "Once you beat the big badasses and\nclean out the moon base you're supposed\nto win, aren't you? Aren't you? Where's\nyour fat reward and ticket home? What\nthe hell is this? It's not supposed to\nend this way!\n\nIt stinks like rotten meat, but looks\nlike the lost Deimos base.  Looks like\nyou're stuck on The Shores of Hell.\nThe only way out is through.\n\nTo continue the DOOM experience, play\nThe Shores of Hell and its amazing\nsequel, Inferno!\n";

/// `E2TEXT` in the original.
pub const E2TEXT: &str = "You've done it! The hideous cyber-\ndemon lord that ruled the lost Deimos\nmoon base has been slain and you\nare triumphant! But ... where are\nyou? You clamber to the edge of the\nmoon and look down to see the awful\ntruth.\n\nDeimos floats above Hell itself!\nYou've never heard of anyone escaping\nfrom Hell, but you'll make the bastards\nsorry they ever heard of you! Quickly,\nyou rappel down to  the surface of\nHell.\n\nNow, it's on to the final chapter of\nDOOM! -- Inferno.";

/// `E3TEXT` in the original.
pub const E3TEXT: &str = "The loathsome spiderdemon that\nmasterminded the invasion of the moon\nbases and caused so much death has had\nits ass kicked for all time.\n\nA hidden doorway opens and you enter.\nYou've proven too tough for Hell to\ncontain, and now Hell at last plays\nfair -- for you emerge from the door\nto see the green fields of Earth!\nHome at last.\n\nYou wonder what's been happening on\nEarth while you were battling evil\nunleashed. It's good that no Hell-\nspawn could have come through that\ndoor with you ...";

/// `E4TEXT` in the original.
pub const E4TEXT: &str = "the spider mastermind must have sent forth\nits legions of hellspawn before your\nfinal confrontation with that terrible\nbeast from hell.  but you stepped forward\nand brought forth eternal damnation and\nsuffering upon the horde as a true hero\nwould in the face of something so evil.\n\nbesides, someone was gonna pay for what\nhappened to daisy, your pet rabbit.\n\nbut now, you see spread before you more\npotential pain and gibbitude as a nation\nof demons run amok among our cities.\n\nnext stop, hell on earth!";

/// `C1TEXT` in the original.
pub const C1TEXT: &str = "YOU HAVE ENTERED DEEPLY INTO THE INFESTED\nSTARPORT. BUT SOMETHING IS WRONG. THE\nMONSTERS HAVE BROUGHT THEIR OWN REALITY\nWITH THEM, AND THE STARPORT'S TECHNOLOGY\nIS BEING SUBVERTED BY THEIR PRESENCE.\n\nAHEAD, YOU SEE AN OUTPOST OF HELL, A\nFORTIFIED ZONE. IF YOU CAN GET PAST IT,\nYOU CAN PENETRATE INTO THE HAUNTED HEART\nOF THE STARBASE AND FIND THE CONTROLLING\nSWITCH WHICH HOLDS EARTH'S POPULATION\nHOSTAGE.";

/// `C2TEXT` in the original.
pub const C2TEXT: &str = "YOU HAVE WON! YOUR VICTORY HAS ENABLED\nHUMANKIND TO EVACUATE EARTH AND ESCAPE\nTHE NIGHTMARE.  NOW YOU ARE THE ONLY\nHUMAN LEFT ON THE FACE OF THE PLANET.\nCANNIBAL MUTATIONS, CARNIVOROUS ALIENS,\nAND EVIL SPIRITS ARE YOUR ONLY NEIGHBORS.\nYOU SIT BACK AND WAIT FOR DEATH, CONTENT\nTHAT YOU HAVE SAVED YOUR SPECIES.\n\nBUT THEN, EARTH CONTROL BEAMS DOWN A\nMESSAGE FROM SPACE: \"SENSORS HAVE LOCATED\nTHE SOURCE OF THE ALIEN INVASION. IF YOU\nGO THERE, YOU MAY BE ABLE TO BLOCK THEIR\nENTRY.  THE ALIEN BASE IS IN THE HEART OF\nYOUR OWN HOME CITY, NOT FAR FROM THE\nSTARPORT.\" SLOWLY AND PAINFULLY YOU GET\nUP AND RETURN TO THE FRAY.";

/// `C3TEXT` in the original.
pub const C3TEXT: &str = "YOU ARE AT THE CORRUPT HEART OF THE CITY,\nSURROUNDED BY THE CORPSES OF YOUR ENEMIES.\nYOU SEE NO WAY TO DESTROY THE CREATURES'\nENTRYWAY ON THIS SIDE, SO YOU CLENCH YOUR\nTEETH AND PLUNGE THROUGH IT.\n\nTHERE MUST BE A WAY TO CLOSE IT ON THE\nOTHER SIDE. WHAT DO YOU CARE IF YOU'VE\nGOT TO GO THROUGH HELL TO GET TO IT?";

/// `C4TEXT` in the original.
pub const C4TEXT: &str = "THE HORRENDOUS VISAGE OF THE BIGGEST\nDEMON YOU'VE EVER SEEN CRUMBLES BEFORE\nYOU, AFTER YOU PUMP YOUR ROCKETS INTO\nHIS EXPOSED BRAIN. THE MONSTER SHRIVELS\nUP AND DIES, ITS THRASHING LIMBS\nDEVASTATING UNTOLD MILES OF HELL'S\nSURFACE.\n\nYOU'VE DONE IT. THE INVASION IS OVER.\nEARTH IS SAVED. HELL IS A WRECK. YOU\nWONDER WHERE BAD FOLKS WILL GO WHEN THEY\nDIE, NOW. WIPING THE SWEAT FROM YOUR\nFOREHEAD YOU BEGIN THE LONG TREK BACK\nHOME. REBUILDING EARTH OUGHT TO BE A\nLOT MORE FUN THAN RUINING IT WAS.\n";

/// `C5TEXT` in the original.
pub const C5TEXT: &str = "CONGRATULATIONS, YOU'VE FOUND THE SECRET\nLEVEL! LOOKS LIKE IT'S BEEN BUILT BY\nHUMANS, RATHER THAN DEMONS. YOU WONDER\nWHO THE INMATES OF THIS CORNER OF HELL\nWILL BE.";

/// `C6TEXT` in the original.
pub const C6TEXT: &str = "CONGRATULATIONS, YOU'VE FOUND THE\nSUPER SECRET LEVEL!  YOU'D BETTER\nBLAZE THROUGH THIS ONE!\n";

/// Vanilla `TEXTSPEED`: tics per revealed character.
pub const TEXTSPEED: i32 = 3;
/// Vanilla `TEXTWAIT`: tics to keep a fully printed text on screen
/// before the art screen appears.
pub const TEXTWAIT: i32 = 250;

// ---------------------------------------------------------------------------
// Cast call constants
// ---------------------------------------------------------------------------

// State indices, counted from the `statenum_t` order in `info.h` (all
// enumerators are implicit). Only the ones the finale compares or switches
// on are listed; the rest of the table is the info-tables port'
// business. If `info.h` ever gained explicit values these would be wrong,
// which is why they are written out here with the names attached.
const S_NULL: usize = 0;
const S_PLAY_ATK1: usize = 292;
const S_POSS_ATK2: usize = 323;
const S_SPOS_ATK2: usize = 356;
const S_VILE_ATK2: usize = 394;
const S_SKEL_FIST2: usize = 474;
const S_SKEL_FIST4: usize = 476;
const S_SKEL_MISS2: usize = 478;
const S_FATT_ATK2: usize = 515;
const S_FATT_ATK5: usize = 518;
const S_FATT_ATK8: usize = 521;
const S_CPOS_ATK2: usize = 555;
const S_CPOS_ATK3: usize = 556;
const S_CPOS_ATK4: usize = 557;
const S_TROO_ATK3: usize = 592;
const S_SARG_ATK2: usize = 624;
const S_HEAD_ATK2: usize = 643;
const S_BOSS_ATK2: usize = 676;
const S_BOS2_ATK2: usize = 705;
const S_SKULL_ATK2: usize = 728;
const S_SPID_ATK2: usize = 754;
const S_SPID_ATK3: usize = 755;
const S_BSPI_ATK2: usize = 786;
const S_CYBER_ATK2: usize = 823;
const S_CYBER_ATK4: usize = 825;
const S_CYBER_ATK6: usize = 827;
const S_PAIN_ATK3: usize = 848;

/// `castorder[]` from `f_finale.c`, in order of appearance.
/// The `MT_*` value each entry points at in the original enum is not
/// carried here: the cast call never compares them, it only looks the
/// actor up in [`crate::finale::CastTables`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastActorKind {
    /// `MT_POSSESSED`
    Zombieman,
    /// `MT_SHOTGUY`
    ShotgunGuy,
    /// `MT_CHAINGUY`
    HeavyWeaponDude,
    /// `MT_TROOP`
    Imp,
    /// `MT_SERGEANT`
    Demon,
    /// `MT_SKULL`
    LostSoul,
    /// `MT_HEAD`
    Cacodemon,
    /// `MT_KNIGHT`
    HellKnight,
    /// `MT_BRUISER`
    BaronOfHell,
    /// `MT_BABY`
    Arachnotron,
    /// `MT_PAIN`
    PainElemental,
    /// `MT_UNDEAD`
    Revenant,
    /// `MT_FATSO`
    Mancubus,
    /// `MT_VILE`
    ArchVile,
    /// `MT_SPIDER`
    SpiderMastermind,
    /// `MT_CYBORG`
    Cyberdemon,
    /// `MT_PLAYER`
    OurHero,
}

/// One cast-order entry: the on-screen name and the actor to puppeteer.
#[derive(Clone, Copy, Debug)]
pub struct CastOrderEntry {
    /// `CC_*` from `d_englsh.h`.
    pub name: &'static str,
    /// The actor to drive.
    pub kind: CastActorKind,
}

/// The `castorder[]` table, verbatim and in order.
pub const CAST_ORDER: [CastOrderEntry; 17] = [
    CastOrderEntry {
        name: "ZOMBIEMAN",
        kind: CastActorKind::Zombieman,
    },
    CastOrderEntry {
        name: "SHOTGUN GUY",
        kind: CastActorKind::ShotgunGuy,
    },
    CastOrderEntry {
        name: "HEAVY WEAPON DUDE",
        kind: CastActorKind::HeavyWeaponDude,
    },
    CastOrderEntry {
        name: "IMP",
        kind: CastActorKind::Imp,
    },
    CastOrderEntry {
        name: "DEMON",
        kind: CastActorKind::Demon,
    },
    CastOrderEntry {
        name: "LOST SOUL",
        kind: CastActorKind::LostSoul,
    },
    CastOrderEntry {
        name: "CACODEMON",
        kind: CastActorKind::Cacodemon,
    },
    CastOrderEntry {
        name: "HELL KNIGHT",
        kind: CastActorKind::HellKnight,
    },
    CastOrderEntry {
        name: "BARON OF HELL",
        kind: CastActorKind::BaronOfHell,
    },
    CastOrderEntry {
        name: "ARACHNOTRON",
        kind: CastActorKind::Arachnotron,
    },
    CastOrderEntry {
        name: "PAIN ELEMENTAL",
        kind: CastActorKind::PainElemental,
    },
    CastOrderEntry {
        name: "REVENANT",
        kind: CastActorKind::Revenant,
    },
    CastOrderEntry {
        name: "MANCUBUS",
        kind: CastActorKind::Mancubus,
    },
    CastOrderEntry {
        name: "ARCH-VILE",
        kind: CastActorKind::ArchVile,
    },
    CastOrderEntry {
        name: "THE SPIDER MASTERMIND",
        kind: CastActorKind::SpiderMastermind,
    },
    CastOrderEntry {
        name: "THE CYBERDEMON",
        kind: CastActorKind::Cyberdemon,
    },
    CastOrderEntry {
        name: "OUR HERO",
        kind: CastActorKind::OurHero,
    },
];

/// The bits of `mobjinfo[index]` the cast call reads.
///
/// `*_state` values are indices into the states table; `S_NULL` (0)
/// means the actor has no such state, exactly like the original stores
/// there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CastActorInfo {
    /// `sprites[]` frame to start the actor's idle animation from.
    pub seestate: usize,
    /// `S_NULL` (0) when the actor cannot melee.
    pub meleestate: usize,
    /// `S_NULL` (0) when the actor cannot fire a missile.
    pub missilestate: usize,
    /// Frame to switch to on player key press.
    pub deathstate: usize,
    /// Played when the actor enters its seestate (skipped when unset,
    /// like the vanilla `seesound == 0` check).
    pub seesound: Option<FinaleSound>,
    /// Played when the actor is "killed" on key press.
    pub deathsound: Option<FinaleSound>,
}

/// The bits of `states[index]` the cast call reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CastStateFrame {
    /// Index into the sprites table.
    pub sprite: usize,
    /// Frame letter plus the `FF_*` flags (`frame & FF_FRAMEMASK`
    /// picks the rotation patch).
    pub frame: i32,
    /// How many tics this frame lasts; `-1` means "until told otherwise"
    /// (the cast call substitutes 15).
    pub tics: i32,
    /// Index of the next state; 0 (`S_NULL`) ends the sequence.
    pub nextstate: usize,
}

/// How the finale reaches the state/mobjinfo tables for the cast call.
///
/// Implemented by the game layer once the info tables exist; in tests,
/// by a synthetic fixture. The default [`state_sfx`](CastTables::state_sfx)
/// already carries the `switch (st)` sound remapping from `F_CastTicker`.
pub trait CastTables {
    /// `mobjinfo[castorder[castnum].type]`.
    fn actor(&self, kind: CastActorKind) -> CastActorInfo;
    /// `states[index]`.
    fn state(&self, index: usize) -> CastStateFrame;
    /// The sound remapping switch from `F_CastTicker`, keyed on the
    /// index of the state *being entered*. Provided with the 1.10
    /// mapping; override only to redirect sounds.
    fn state_sfx(&self, index: usize) -> Option<FinaleSound> {
        Some(match index {
            S_PLAY_ATK1 => FinaleSound::Dshtgn,
            S_POSS_ATK2 => FinaleSound::Pistol,
            S_SPOS_ATK2 => FinaleSound::Shotgn,
            S_VILE_ATK2 => FinaleSound::Vilatk,
            S_SKEL_FIST2 => FinaleSound::Skeswg,
            S_SKEL_FIST4 => FinaleSound::Skepch,
            S_SKEL_MISS2 => FinaleSound::Skeatk,
            S_FATT_ATK8 | S_FATT_ATK5 | S_FATT_ATK2 => FinaleSound::Firsht,
            S_CPOS_ATK2 | S_CPOS_ATK3 | S_CPOS_ATK4 => FinaleSound::Shotgn,
            S_TROO_ATK3 => FinaleSound::Claw,
            S_SARG_ATK2 => FinaleSound::Sgtatk,
            S_BOSS_ATK2 | S_BOS2_ATK2 | S_HEAD_ATK2 => FinaleSound::Firsht,
            S_SKULL_ATK2 => FinaleSound::Sklatk,
            S_SPID_ATK2 | S_SPID_ATK3 => FinaleSound::Shotgn,
            S_BSPI_ATK2 => FinaleSound::Plasma,
            S_CYBER_ATK2 | S_CYBER_ATK4 | S_CYBER_ATK6 => FinaleSound::Rlaunc,
            S_PAIN_ATK3 => FinaleSound::Sklatk,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Effects reported to the game layer
// ---------------------------------------------------------------------------

/// Sounds the finale asks the game layer to play (the vanilla
/// `S_StartSound` calls). The mapping to `sfx_*` ids belongs to the
/// sound layer, exactly like [`crate::menu::MenuSound`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FinaleSound {
    /// `sfx_pistol`
    Pistol,
    /// `sfx_shotgn`
    Shotgn,
    /// `sfx_claw`
    Claw,
    /// `sfx_sgtatk`
    Sgtatk,
    /// `sfx_vilatk`
    Vilatk,
    /// `sfx_dshtgn`
    Dshtgn,
    /// `sfx_skeswg`
    Skeswg,
    /// `sfx_skepch`
    Skepch,
    /// `sfx_skeatk`
    Skeatk,
    /// `sfx_firsht`
    Firsht,
    /// `sfx_sklatk`
    Sklatk,
    /// `sfx_rlaunc`
    Rlaunc,
    /// `sfx_plasma`
    Plasma,
    /// `sfx_posit1` - cast call, zombieman/shotgun guy entrance
    Posit1,
    /// `sfx_posit2` - cast call, heavy weapon dude entrance
    Posit2,
    /// `sfx_vilsit` - cast call, arch-vile entrance
    Vilsit,
    /// `sfx_skesit` - cast call, revenant entrance
    Skesit,
    /// `sfx_mansit` - cast call, mancubus entrance
    Mansit,
    /// `sfx_bgsit1` - cast call, imp entrance
    Bgsit1,
    /// `sfx_sgtsit` - cast call, demon entrance
    Sgtsit,
    /// `sfx_cacsit` - cast call, cacodemon entrance
    Cacsit,
    /// `sfx_brssit` - cast call, baron entrance
    Brssit,
    /// `sfx_kntsit` - cast call, hell knight entrance
    Kntsit,
    /// `sfx_spisit` - cast call, spider mastermind entrance
    Spisit,
    /// `sfx_bspsit` - cast call, arachnotron entrance
    Bspsit,
    /// `sfx_cybsit` - cast call, cyberdemon entrance
    Cybsit,
    /// `sfx_pesit` - cast call, pain elemental entrance
    Pesit,
    /// `sfx_podth1` - cast call, zombieman death
    Podth1,
    /// `sfx_podth2` - cast call, shotgun guy death
    Podth2,
    /// `sfx_vildth` - cast call, arch-vile death
    Vildth,
    /// `sfx_skedth` - cast call, revenant death
    Skedth,
    /// `sfx_mandth` - cast call, mancubus death
    Mandth,
    /// `sfx_bgdth1` - cast call, imp death
    Bgdth1,
    /// `sfx_sgtdth` - cast call, demon death
    Sgtdth,
    /// `sfx_cacdth` - cast call, cacodemon death
    Cacdth,
    /// `sfx_brsdth` - cast call, baron death
    Brsdth,
    /// `sfx_kntdth` - cast call, hell knight death
    Kntdth,
    /// `sfx_firxpl` - cast call, lost soul death
    Firxpl,
    /// `sfx_spidth` - cast call, spider mastermind death
    Spidth,
    /// `sfx_bspdth` - cast call, arachnotron death
    Bspdth,
    /// `sfx_cybdth` - cast call, cyberdemon death
    Cybdth,
    /// `sfx_pedth` - cast call, pain elemental death
    Pedth,
    /// `sfx_pldeth` - cast call, our hero death
    Pldeth,
}

/// Music changes the finale triggers (`S_ChangeMusic(..., true)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FinaleMusic {
    /// `mus_victor` — DOOM 1 end-of-episode finale.
    Victor,
    /// `mus_read_m` — the default and the DOOM II finale.
    ReadM,
    /// `mus_evil` — the cast call.
    Evil,
    /// `mus_bunny` — the E3 art screen (the evil rabbit soundtrack).
    Bunny,
}

/// Fire-and-forget finale decisions as plain data; the game layer
/// applies them. Drained with [`Finale::drain_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinaleEvent {
    /// `S_ChangeMusic(..., true)`
    Music(FinaleMusic),
    /// `S_StartSound`/`S_StartSound(NULL, ...)`
    Sound(FinaleSound),
    /// `gameaction = ga_worlddone` — DOOM II: any button after 50 tics
    /// on a non-MAP30 finale exits to the next level.
    WorldDone,
    /// `wipegamestate = -1` — force a screen wipe on the next state
    /// transition (text → art, and into the cast call).
    ForceWipe,
}

/// Which finale runs, and its content resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinaleKind {
    /// DOOM 1 episodes (shareware/registered/retail). `episode` is
    /// 1-based (1..=4); `retail` switches the E1 art page to the
    /// CREDIT lump like Ultimate DOOM.
    Episode {
        /// 1-based episode number.
        episode: i32,
        /// Ultimate DOOM (retail) art page variant.
        retail: bool,
    },
    /// DOOM II (and the packs). `map` is the 1-based map number; the
    /// finale exists for maps 6, 11, 15, 20, 30 and 31.
    Commercial {
        /// 1-based map number.
        map: i32,
    },
}

/// The stage of the finale animation (`finalestage` in the original).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinaleStage {
    /// 0 — the typed-out text.
    Text,
    /// 1 — the art screen. Never entered for DOOM II finales (vanilla
    /// returns early before the stage can advance).
    Art,
    /// 2 — the cast call (DOOM II MAP30 only).
    Cast,
}

/// Per-tic plain-data input for [`Finale::tick`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FinaleInput {
    /// Whether any player is holding any button this tic (vanilla
    /// `players[i].cmd.buttons` for some `i`). The finale only looks
    /// at this after 50 tics of a DOOM II finale.
    pub any_button: bool,
}

/// The cast call state (`castnum`/`caststate`/`casttics`/... in the
/// original). Driven through [`crate::finale::CastTables`]; the game layer owns the
/// tables.
#[derive(Clone, Copy, Debug)]
pub struct CastCall {
    castnum: usize,
    caststate: usize,
    casttics: i32,
    castdeath: bool,
    castframes: i32,
    castonmelee: bool,
    castattacking: bool,
}

impl CastCall {
    fn new(tables: &dyn CastTables) -> Self {
        let info = tables.actor(CAST_ORDER[0].kind);
        let frame = tables.state(info.seestate);
        Self {
            castnum: 0,
            caststate: info.seestate,
            casttics: frame.tics,
            castdeath: false,
            castframes: 0,
            castonmelee: false,
            castattacking: false,
        }
    }

    /// `F_CastTicker`.
    fn tick(&mut self, tables: &dyn CastTables) -> Vec<FinaleEvent> {
        let mut events = Vec::new();

        self.casttics -= 1;
        if self.casttics > 0 {
            return events;
        }

        let current = tables.state(self.caststate);
        // the S_PLAY_ATK1 "gross hack": skip the attack bookkeeping
        // entirely when the player squirrel is about to swing
        let mut jumped_to_stop = false;
        if current.tics == -1 || current.nextstate == S_NULL {
            // switch from a death state to the next monster
            self.castnum += 1;
            self.castdeath = false;
            if self.castnum >= CAST_ORDER.len() {
                self.castnum = 0;
            }
            let actor = tables.actor(CAST_ORDER[self.castnum].kind);
            if let Some(sfx) = actor.seesound {
                events.push(FinaleEvent::Sound(sfx));
            }
            self.caststate = actor.seestate;
            self.castframes = 0;
        } else if self.caststate == S_PLAY_ATK1 {
            jumped_to_stop = true;
        } else {
            // just advance to the next state in the animation
            let st = current.nextstate;
            self.caststate = st;
            self.castframes += 1;
            if let Some(sfx) = tables.state_sfx(st) {
                events.push(FinaleEvent::Sound(sfx));
            }
        }

        if !jumped_to_stop && self.castframes == 12 {
            // go into the attack frame
            self.castattacking = true;
            let actor = tables.actor(CAST_ORDER[self.castnum].kind);
            self.caststate = if self.castonmelee {
                actor.meleestate
            } else {
                actor.missilestate
            };
            self.castonmelee = !self.castonmelee;
            if self.caststate == S_NULL {
                let actor = tables.actor(CAST_ORDER[self.castnum].kind);
                self.caststate = if self.castonmelee {
                    actor.meleestate
                } else {
                    actor.missilestate
                };
            }
        }

        let stop_attacking = jumped_to_stop
            || (self.castattacking
                && (self.castframes == 24
                    || self.caststate == tables.actor(CAST_ORDER[self.castnum].kind).seestate));
        if stop_attacking {
            self.castattacking = false;
            self.castframes = 0;
            self.caststate = tables.actor(CAST_ORDER[self.castnum].kind).seestate;
        }

        self.casttics = tables.state(self.caststate).tics;
        if self.casttics == -1 {
            self.casttics = 15;
        }
        events
    }

    /// `F_CastResponder`: a key press sends the actor into its death
    /// frame. Returns true when the key was consumed (always, for
    /// keydowns), plus the death sound to play.
    fn on_keydown(&mut self, tables: &dyn CastTables) -> (bool, Option<FinaleSound>) {
        if self.castdeath {
            return (true, None);
        }
        self.castdeath = true;
        let actor = tables.actor(CAST_ORDER[self.castnum].kind);
        self.caststate = actor.deathstate;
        self.casttics = tables.state(actor.deathstate).tics;
        self.castframes = 0;
        self.castattacking = false;
        (true, actor.deathsound)
    }
}

// ---------------------------------------------------------------------------
// The finale state machine
// ---------------------------------------------------------------------------

/// The `F_StartFinale` / `F_Ticker` / `F_Drawer` state machine.
///
/// Built with [`Finale::new`] (content resolution + first music event),
/// ticked with [`Finale::tick`], drawn with [`Finale::draw`]. Events
/// accumulate and are drained with [`Finale::drain_events`] — call it
/// after every tick and every draw.
pub struct Finale {
    kind: FinaleKind,
    text: &'static str,
    flat: &'static str,
    stage: FinaleStage,
    finalecount: i32,
    events: Vec<FinaleEvent>,
    cast: Option<CastCall>,
    /// Static `laststage` in `F_BunnyScroll`.
    bunny_laststage: i32,
}

impl Finale {
    /// `F_StartFinale` with the game-state twiddling left to the game
    /// layer (`gameaction`/`gamestate`/`viewactive`/`automapactive`).
    ///
    /// Content resolution is the vanilla `switch`: DOOM 1 episodes pick
    /// `FLOOR4_8`/`SFLR6_1`/`MFLR8_4`/`MFLR8_3` + the matching text by
    /// episode, DOOM II maps 6/11/20/30/15/31 pick their `SLIME16`/
    /// `RROCKxx` + `C1TEXT..C6TEXT`. The `default:` branches (unknown
    /// episode/map) fall back to the E1/`F_SKY1` content like the
    /// original's indetermined branch — the original left the pointers
    /// dangling there, which is not something a port can do.
    pub fn new(kind: FinaleKind) -> Self {
        let (flat, text, music) = match kind {
            FinaleKind::Episode { episode, .. } => match episode {
                1 => ("FLOOR4_8", E1TEXT, FinaleMusic::Victor),
                2 => ("SFLR6_1", E2TEXT, FinaleMusic::Victor),
                3 => ("MFLR8_4", E3TEXT, FinaleMusic::Victor),
                4 => ("MFLR8_3", E4TEXT, FinaleMusic::Victor),
                // the original's `default:` read uninitialized pointers
                _ => ("FLOOR4_8", E1TEXT, FinaleMusic::Victor),
            },
            FinaleKind::Commercial { map } => match map {
                6 => ("SLIME16", C1TEXT, FinaleMusic::ReadM),
                11 => ("RROCK14", C2TEXT, FinaleMusic::ReadM),
                20 => ("RROCK07", C3TEXT, FinaleMusic::ReadM),
                30 => ("RROCK17", C4TEXT, FinaleMusic::ReadM),
                15 => ("RROCK13", C5TEXT, FinaleMusic::ReadM),
                31 => ("RROCK19", C6TEXT, FinaleMusic::ReadM),
                // the original's "Indeterminate" default
                _ => ("F_SKY1", C1TEXT, FinaleMusic::ReadM),
            },
        };
        Self {
            kind,
            text,
            flat,
            stage: FinaleStage::Text,
            finalecount: 0,
            events: vec![FinaleEvent::Music(music)],
            cast: None,
            bunny_laststage: 0,
        }
    }

    /// Current stage (`finalestage`).
    pub fn stage(&self) -> FinaleStage {
        self.stage
    }

    /// `finalecount`.
    pub fn finale_count(&self) -> i32 {
        self.finalecount
    }

    /// The resolved text (`finaletext`).
    pub fn text(&self) -> &'static str {
        self.text
    }

    /// The resolved flat name (`finaleflat`).
    pub fn flat(&self) -> &'static str {
        self.flat
    }

    /// Which content was selected (see [`Finale::new`]).
    pub fn kind(&self) -> FinaleKind {
        self.kind
    }

    /// Drains the accumulated effects. Call after every [`tick`](Self::tick)
    /// and [`draw`](Self::draw); the order is the vanilla call order.
    pub fn drain_events(&mut self) -> Vec<FinaleEvent> {
        std::mem::take(&mut self.events)
    }

    /// `F_Ticker`.
    pub fn tick(&mut self, input: &FinaleInput, tables: &dyn CastTables) {
        // check for skipping (commercial only)
        if let FinaleKind::Commercial { map } = self.kind {
            if self.finalecount > 50 && input.any_button {
                // go on to the next level (or start the cast call on MAP30)
                if map == 30 {
                    self.start_cast(tables);
                } else {
                    self.events.push(FinaleEvent::WorldDone);
                }
            }
        }

        // advance animation
        self.finalecount += 1;

        if self.stage == FinaleStage::Cast {
            let mut new_events = Vec::new();
            if let Some(cast) = &mut self.cast {
                new_events = cast.tick(tables);
            }
            self.events.append(&mut new_events);
            return;
        }

        if matches!(self.kind, FinaleKind::Commercial { .. }) {
            return;
        }

        if self.stage == FinaleStage::Text
            && self.finalecount > self.text.len() as i32 * TEXTSPEED + TEXTWAIT
        {
            self.finalecount = 0;
            self.stage = FinaleStage::Art;
            self.events.push(FinaleEvent::ForceWipe); // force a wipe
            if matches!(self.kind, FinaleKind::Episode { episode: 3, .. }) {
                self.events.push(FinaleEvent::Music(FinaleMusic::Bunny));
            }
        }
    }

    /// `F_Responder` (key press path).
    pub fn on_keydown(&mut self, tables: &dyn CastTables) -> bool {
        if self.stage == FinaleStage::Cast {
            if let Some(cast) = &mut self.cast {
                let (consumed, sound) = cast.on_keydown(tables);
                if let Some(sfx) = sound {
                    self.events.push(FinaleEvent::Sound(sfx));
                }
                return consumed;
            }
            return true;
        }
        false
    }

    /// `F_StartCast`.
    fn start_cast(&mut self, tables: &dyn CastTables) {
        // force a screen wipe
        self.events.push(FinaleEvent::ForceWipe);
        self.cast = Some(CastCall::new(tables));
        self.stage = FinaleStage::Cast;
        self.events.push(FinaleEvent::Music(FinaleMusic::Evil));
    }

    /// `F_Drawer`.
    pub fn draw(
        &mut self,
        fb: &mut IndexedFramebuffer,
        font: &HudFont,
        resources: &ResourceManager,
        sprites: &crate::render::SpriteDefinitions,
        tables: &dyn CastTables,
    ) -> EngineResult<()> {
        match self.stage {
            FinaleStage::Cast => self.draw_cast(fb, font, resources, sprites, tables),
            FinaleStage::Text => self.draw_text(fb, font, resources),
            FinaleStage::Art => self.draw_art(fb, resources),
        }
    }

    /// `F_TextWrite`: tile the flat, then type the text out.
    fn draw_text(
        &self,
        fb: &mut IndexedFramebuffer,
        font: &HudFont,
        resources: &ResourceManager,
    ) -> EngineResult<()> {
        // erase the entire screen to a tiled background
        let flat_lump = resources
            .find_lump(self.flat)
            .ok_or_else(|| EngineError::Resource(format!("finale flat {} not found", self.flat)))?;
        let flat = resources.decode_cached::<Flat>(flat_lump, |bytes| {
            Flat::parse(bytes).map_err(EngineError::from)
        })?;
        let pixels = flat.pixels();

        for y in 0..SCREEN_HEIGHT {
            let src = &pixels[(y & 63) * 64..(y & 63) * 64 + 64];
            let row = fb.row_mut(y).expect("y is bounded by SCREEN_HEIGHT");
            let mut x = 0;
            while x < SCREEN_WIDTH {
                let end = (x + 64).min(SCREEN_WIDTH);
                row[x..end].copy_from_slice(&src[..end - x]);
                x += 64;
            }
        }

        // draw some of the text onto the screen
        let mut cx = 10i32;
        let mut cy = 10i32;
        let mut count = (self.finalecount - 10) / TEXTSPEED;
        if count < 0 {
            count = 0;
        }

        for &c in self.text.as_bytes() {
            if count <= 0 {
                break;
            }
            count -= 1;
            if c == b'\n' {
                cx = 10;
                cy += 11;
                continue;
            }
            let glyph = c.to_ascii_uppercase() as i32 - i32::from(crate::hud::HU_FONTSTART);
            if glyph < 0 || glyph >= crate::hud::HU_FONTSIZE as i32 {
                // printable but outside the font (or the vanilla c == 63
                // reads one past hu_font[]; skipped here)
                cx += 4;
                continue;
            }
            let font_glyph = font.glyph(glyph as usize);
            let w = i32::from(font_glyph.header.width);
            if cx + w > SCREEN_WIDTH as i32 {
                break;
            }
            draw_patch(fb, font_glyph, cx, cy);
            cx += w;
        }

        Ok(())
    }

    /// `F_Drawer` stage 1: the per-episode art pages.
    fn draw_art(
        &mut self,
        fb: &mut IndexedFramebuffer,
        resources: &ResourceManager,
    ) -> EngineResult<()> {
        match self.kind {
            FinaleKind::Episode { episode: 1, retail } => {
                // retail ultimate DOOM shows the CREDIT page instead
                self.draw_art_patch(fb, resources, if retail { "CREDIT" } else { "HELP2" })
            }
            FinaleKind::Episode { episode: 2, .. } => {
                self.draw_art_patch(fb, resources, "VICTORY2")
            }
            FinaleKind::Episode { episode: 3, .. } => self.draw_bunny_scroll(fb, resources),
            FinaleKind::Episode { episode: 4, .. } => self.draw_art_patch(fb, resources, "ENDPIC"),
            // unreachable for DOOM II (stage never becomes Art); the
            // original's switch has no default either — nothing is drawn
            FinaleKind::Commercial { .. } | FinaleKind::Episode { .. } => Ok(()),
        }
    }

    fn draw_art_patch(
        &self,
        fb: &mut IndexedFramebuffer,
        resources: &ResourceManager,
        name: &str,
    ) -> EngineResult<()> {
        let lump = resources
            .find_lump(name)
            .ok_or_else(|| EngineError::Resource(format!("finale art lump {name} not found")))?;
        let patch = resources
            .decode_cached::<Patch>(lump, |bytes| decode_patch(bytes).map_err(EngineError::from))?;
        draw_patch(fb, &patch, 0, 0);
        Ok(())
    }

    /// `F_BunnyScroll` — the E3 "bunny scroll" (the carrot is a lie).
    fn draw_bunny_scroll(
        &mut self,
        fb: &mut IndexedFramebuffer,
        resources: &ResourceManager,
    ) -> EngineResult<()> {
        let load_patch = |name: &str| -> EngineResult<Arc<Patch>> {
            let lump = resources.find_lump(name).ok_or_else(|| {
                EngineError::Resource(format!("bunny scroll lump {name} not found"))
            })?;
            resources.decode_cached::<Patch>(lump, |bytes| {
                decode_patch(bytes).map_err(EngineError::from)
            })
        };
        let p1 = load_patch("PFUB2")?;
        let p2 = load_patch("PFUB1")?;

        // 319.5-ish scroll position, clamped into 0..=320 like the original
        let scrolled =
            (SCREEN_WIDTH as i32 - (self.finalecount - 230) / 2).clamp(0, SCREEN_WIDTH as i32);

        for x in 0..SCREEN_WIDTH as i32 {
            if x + scrolled < SCREEN_WIDTH as i32 {
                draw_patch_column(fb, &p1, x, x + scrolled);
            } else {
                draw_patch_column(fb, &p2, x, x + scrolled - SCREEN_WIDTH as i32);
            }
        }

        if self.finalecount < 1130 {
            return Ok(());
        }
        // ENDx frames land centered, same as the original
        const END_X: i32 = (SCREEN_WIDTH as i32 - 13 * 8) / 2;
        const END_Y: i32 = (SCREEN_HEIGHT as i32 - 8 * 8) / 2;
        if self.finalecount < 1180 {
            // the night before the invasion
            let lump = resources.find_lump("END0").ok_or_else(|| {
                EngineError::Resource("bunny scroll lump END0 not found".to_string())
            })?;
            let patch = resources.decode_cached::<Patch>(lump, |bytes| {
                decode_patch(bytes).map_err(EngineError::from)
            })?;
            draw_patch(fb, &patch, END_X, END_Y);
            self.bunny_laststage = 0;
            return Ok(());
        }

        let stage = ((self.finalecount - 1180) / 5).min(6);
        if stage > self.bunny_laststage {
            self.events.push(FinaleEvent::Sound(FinaleSound::Pistol));
            self.bunny_laststage = stage;
        }
        let name = format!("END{stage}");
        let lump = resources
            .find_lump(&name)
            .ok_or_else(|| EngineError::Resource(format!("bunny scroll lump {name} not found")))?;
        let patch = resources
            .decode_cached::<Patch>(lump, |bytes| decode_patch(bytes).map_err(EngineError::from))?;
        draw_patch(fb, &patch, END_X, END_Y);
        Ok(())
    }

    /// `F_CastDrawer` — BOSSBACK, the actor's name and the current frame.
    fn draw_cast(
        &self,
        fb: &mut IndexedFramebuffer,
        font: &HudFont,
        resources: &ResourceManager,
        sprites: &crate::render::SpriteDefinitions,
        tables: &dyn CastTables,
    ) -> EngineResult<()> {
        let boss_lump = resources.find_lump("BOSSBACK").ok_or_else(|| {
            EngineError::Resource("finale cast background BOSSBACK not found".to_string())
        })?;
        let boss_back = resources.decode_cached::<Patch>(boss_lump, |bytes| {
            decode_patch(bytes).map_err(EngineError::from)
        })?;
        draw_patch(fb, &boss_back, 0, 0);

        let cast = self.cast.as_ref().expect("cast stage implies cast state");
        let entry = &CAST_ORDER[cast.castnum];
        cast_print(fb, font, entry.name);

        // draw the current frame in the middle of the screen
        let state = tables.state(cast.caststate);
        let frame_index = (state.frame & 0x7fff) as usize; // FF_FRAMEMASK
        let sprite = sprites.sprite(state.sprite).ok_or_else(|| {
            EngineError::Resource(format!(
                "cast state sprite index {} out of range",
                state.sprite
            ))
        })?;
        let frame = sprite.frame(frame_index).ok_or_else(|| {
            EngineError::Resource(format!("cast state frame index {frame_index} out of range"))
        })?;
        let lump = frame.lump[0];
        let flip = frame.flip[0];
        let patch = resources
            .decode_cached::<Patch>(lump, |bytes| decode_patch(bytes).map_err(EngineError::from))?;
        if flip {
            draw_patch_flipped(fb, &patch, 160, 170);
        } else {
            draw_patch(fb, &patch, 160, 170);
        }
        Ok(())
    }
}

/// `F_CastPrint`: center the cast member's name at `y = 180`.
fn cast_print(fb: &mut IndexedFramebuffer, font: &HudFont, text: &str) {
    let width = text_width(font, text.as_bytes());
    let x = 160 - width / 2;
    // uppercase-draw the name, HU_FONT-start range and all
    let mut cx = x;
    for &raw in text.as_bytes() {
        let glyph = raw.to_ascii_uppercase() as i32 - i32::from(crate::hud::HU_FONTSTART);
        if !(0..crate::hud::HU_FONTSIZE as i32).contains(&glyph) {
            cx += 4;
            continue;
        }
        let font_glyph = font.glyph(glyph as usize);
        draw_patch(fb, font_glyph, cx, 180);
        cx += i32::from(font_glyph.header.width);
    }
}

/// `V_DrawPatchFlipped` — same as [`draw_patch`] but the columns are
/// mirrored horizontally. Used for flipped cast sprites.
fn draw_patch_flipped(fb: &mut IndexedFramebuffer, patch: &Patch, x: i32, y: i32) {
    let base_x = x - i32::from(patch.header.left_offset);
    let top_y = y - i32::from(patch.header.top_offset);
    let w = patch.columns.len() as i32;
    for screen_col in 0..w {
        let src = &patch.columns[(w - 1 - screen_col) as usize];
        draw_column_posts(fb, src, base_x + screen_col, top_y);
    }
}

/// Draws column `column` of `patch` at screen column `x` — the
/// `F_DrawPatchCol` primitive the bunny scroll uses. Deliberately
/// applies no origin offset, exactly like the original: the bunny
/// pics are raw column dumps and their posts carry absolute rows.
fn draw_patch_column(fb: &mut IndexedFramebuffer, patch: &Patch, x: i32, column: i32) {
    let Some(col) = patch.column(column as usize) else {
        return; // the original reads out of bounds here; clip instead
    };
    draw_column_posts(fb, col, x, 0);
}

/// Shared post painter: walks one column's posts, clipping per pixel.
fn draw_column_posts(
    fb: &mut IndexedFramebuffer,
    column: &crate::gfx::patch::PatchColumn,
    x: i32,
    top_y: i32,
) {
    for post in &column.posts {
        let row = top_y + i32::from(post.top_delta);
        for (i, &value) in post.pixels.iter().enumerate() {
            let py = row + i as i32;
            if x >= 0 && x < SCREEN_WIDTH as i32 && py >= 0 && py < SCREEN_HEIGHT as i32 {
                fb.set(x as usize, py as usize, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::patch::{Patch, decode_patch};
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    // ---------- fixtures ----------

    /// Builds raw patch bytes from per-column `(topdelta, pixels)` posts.
    fn build_patch_bytes(width: i16, height: i16, columns: &[Vec<(u8, Vec<u8>)>]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes()); // left offset
        data.extend_from_slice(&0i16.to_le_bytes()); // top offset
        let table_start = data.len();
        data.resize(table_start + columns.len() * 4, 0);
        for (i, posts) in columns.iter().enumerate() {
            let column_offset = data.len() as i32;
            data[table_start + i * 4..table_start + (i + 1) * 4]
                .copy_from_slice(&column_offset.to_le_bytes());
            for &(topdelta, ref pixels) in posts {
                data.push(topdelta);
                data.push(pixels.len() as u8);
                data.push(0);
                data.extend_from_slice(pixels);
                data.push(0);
            }
            data.push(0xff);
        }
        data
    }

    /// A full-column `width`x`height` patch filled with `fill`, as raw bytes.
    fn fill_patch_bytes(width: i16, height: i16, fill: u8) -> Vec<u8> {
        let column = vec![(0u8, vec![fill; height as usize])];
        let columns = vec![column; width as usize];
        build_patch_bytes(width, height, &columns)
    }

    /// A `width`x`height` patch filled with `fill`.
    fn fill_patch(width: i16, height: i16, fill: u8) -> Patch {
        decode_patch(&fill_patch_bytes(width, height, fill)).unwrap()
    }

    /// Font where every glyph is 3 px wide, 7 px tall, colored by
    /// `index + 1` (glyph `!` = 1).
    fn synthetic_font() -> HudFont {
        let glyphs: [Arc<Patch>; crate::hud::HU_FONTSIZE] =
            std::array::from_fn(|i| Arc::new(fill_patch(3, 7, 1 + i as u8)));
        HudFont::from_glyphs(glyphs)
    }

    /// Same, but the flat is named after one of the finale's real flats.
    fn manager_with_flat(
        flat_name: &str,
        flat_fill: u8,
        patches: &[(&str, &[u8])],
    ) -> ResourceManager {
        let flat = vec![flat_fill; 4096];
        let mut lumps: Vec<(String, &[u8])> = vec![(flat_name.to_string(), &flat)];
        for (name, data) in patches {
            lumps.push((name.to_string(), data));
        }
        let wad = build_wad(
            b"IWAD",
            &lumps
                .iter()
                .map(|(n, d)| (n.as_str(), *d))
                .collect::<Vec<_>>(),
        );
        let file = WadFile::from_bytes(wad).unwrap();
        ResourceManager::new(file, Vec::new())
    }

    /// Synthetic cast tables: every actor idles in a self-looping frame,
    /// attacks in a self-looping missile frame, dies in a one-shot frame.
    struct TestTables;
    impl CastTables for TestTables {
        fn actor(&self, _kind: CastActorKind) -> CastActorInfo {
            CastActorInfo {
                seestate: 1,
                meleestate: 0, // none
                missilestate: 2,
                deathstate: 3,
                seesound: Some(FinaleSound::Pistol),
                deathsound: Some(FinaleSound::Sklatk),
            }
        }
        fn state(&self, index: usize) -> CastStateFrame {
            match index {
                1 => CastStateFrame {
                    sprite: 0,
                    frame: 0,
                    tics: 1,
                    nextstate: 1,
                },
                2 => CastStateFrame {
                    sprite: 0,
                    frame: 0,
                    tics: 1,
                    nextstate: 2,
                },
                3 => CastStateFrame {
                    sprite: 0,
                    frame: 0,
                    tics: 2,
                    nextstate: S_NULL,
                },
                _ => CastStateFrame {
                    sprite: 0,
                    frame: 0,
                    tics: 1,
                    nextstate: S_NULL,
                },
            }
        }
    }

    /// Ticks a freshly created DOOM II MAP30 finale into the cast call.
    fn enter_cast(f: &mut Finale, tables: &TestTables) {
        for _ in 0..51 {
            f.tick(&FinaleInput::default(), tables);
        }
        f.tick(&FinaleInput { any_button: true }, tables);
        assert_eq!(f.stage(), FinaleStage::Cast);
        f.drain_events();
    }

    #[test]
    fn cast_order_is_the_vanilla_table() {
        assert_eq!(CAST_ORDER.len(), 17);
        let names: Vec<&str> = CAST_ORDER.iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            [
                "ZOMBIEMAN",
                "SHOTGUN GUY",
                "HEAVY WEAPON DUDE",
                "IMP",
                "DEMON",
                "LOST SOUL",
                "CACODEMON",
                "HELL KNIGHT",
                "BARON OF HELL",
                "ARACHNOTRON",
                "PAIN ELEMENTAL",
                "REVENANT",
                "MANCUBUS",
                "ARCH-VILE",
                "THE SPIDER MASTERMIND",
                "THE CYBERDEMON",
                "OUR HERO",
            ]
        );
    }

    #[test]
    fn state_sfx_mapping_matches_vanilla() {
        assert_eq!(TestTables.state_sfx(S_PLAY_ATK1), Some(FinaleSound::Dshtgn));
        assert_eq!(TestTables.state_sfx(S_POSS_ATK2), Some(FinaleSound::Pistol));
        assert_eq!(TestTables.state_sfx(S_CPOS_ATK3), Some(FinaleSound::Shotgn));
        assert_eq!(TestTables.state_sfx(S_BSPI_ATK2), Some(FinaleSound::Plasma));
        assert_eq!(TestTables.state_sfx(9999), None);
    }

    #[test]
    fn episode_one_selects_e1_content() {
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        assert_eq!(f.flat(), "FLOOR4_8");
        assert_eq!(f.text(), E1TEXT);
        assert_eq!(f.stage(), FinaleStage::Text);
        assert_eq!(
            f.drain_events(),
            vec![FinaleEvent::Music(FinaleMusic::Victor)]
        );
    }

    #[test]
    fn episode_two_three_and_four_select_their_content() {
        let f2 = Finale::new(FinaleKind::Episode {
            episode: 2,
            retail: false,
        });
        assert_eq!((f2.flat(), f2.text()), ("SFLR6_1", E2TEXT));
        let f3 = Finale::new(FinaleKind::Episode {
            episode: 3,
            retail: false,
        });
        assert_eq!((f3.flat(), f3.text()), ("MFLR8_4", E3TEXT));
        let f4 = Finale::new(FinaleKind::Episode {
            episode: 4,
            retail: true,
        });
        assert_eq!((f4.flat(), f4.text()), ("MFLR8_3", E4TEXT));
    }

    #[test]
    fn unknown_episode_falls_back_to_e1() {
        let f = Finale::new(FinaleKind::Episode {
            episode: 9,
            retail: false,
        });
        assert_eq!(f.text(), E1TEXT);
    }

    #[test]
    fn commercial_maps_select_doom2_content() {
        let expectations = [
            (6, "SLIME16", C1TEXT),
            (11, "RROCK14", C2TEXT),
            (20, "RROCK07", C3TEXT),
            (30, "RROCK17", C4TEXT),
            (15, "RROCK13", C5TEXT),
            (31, "RROCK19", C6TEXT),
        ];
        for (map, flat, text) in expectations {
            let mut f = Finale::new(FinaleKind::Commercial { map });
            assert_eq!((f.flat(), f.text()), (flat, text), "map {map}");
            assert_eq!(
                f.drain_events(),
                vec![FinaleEvent::Music(FinaleMusic::ReadM)],
                "map {map}"
            );
        }
    }

    #[test]
    fn unknown_commercial_map_falls_back_to_indetermined() {
        let f = Finale::new(FinaleKind::Commercial { map: 1 });
        assert_eq!((f.flat(), f.text()), ("F_SKY1", C1TEXT));
    }

    #[test]
    fn texts_match_the_original_byte_for_byte() {
        // spot-check the tricky cases: escaped quotes in C2TEXT and the
        // trailing newlines in E1TEXT / C6TEXT / C4TEXT
        assert!(C2TEXT.contains("\"SENSORS HAVE LOCATED\nTHE SOURCE"));
        assert!(E1TEXT.ends_with('\n'));
        assert!(C4TEXT.ends_with('\n'));
        assert!(C6TEXT.ends_with('\n'));
        assert!(!E2TEXT.ends_with('\n'));
        assert!(!E3TEXT.ends_with('\n'));
        assert_eq!(C1TEXT.lines().count(), 12);
    }

    #[test]
    fn text_stage_counts_up_and_advances_to_art() {
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        f.drain_events();
        let target = E1TEXT.len() as i32 * TEXTSPEED + TEXTWAIT;
        for _ in 0..target {
            f.tick(&FinaleInput::default(), &TestTables);
            assert_eq!(f.stage(), FinaleStage::Text);
        }
        f.tick(&FinaleInput::default(), &TestTables);
        assert_eq!(f.stage(), FinaleStage::Art);
        assert_eq!(f.finale_count(), 0);
        assert_eq!(f.drain_events(), vec![FinaleEvent::ForceWipe]);
    }

    #[test]
    fn episode_three_switches_to_bunny_music_and_wipes() {
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 3,
            retail: false,
        });
        f.drain_events();
        let target = E3TEXT.len() as i32 * TEXTSPEED + TEXTWAIT + 1;
        for _ in 0..target {
            f.tick(&FinaleInput::default(), &TestTables);
        }
        assert_eq!(f.stage(), FinaleStage::Art);
        assert_eq!(
            f.drain_events(),
            vec![
                FinaleEvent::ForceWipe,
                FinaleEvent::Music(FinaleMusic::Bunny)
            ]
        );
    }

    #[test]
    fn commercial_never_advances_to_art() {
        let mut f = Finale::new(FinaleKind::Commercial { map: 6 });
        f.drain_events();
        for _ in 0..10_000 {
            f.tick(&FinaleInput::default(), &TestTables);
        }
        assert_eq!(f.stage(), FinaleStage::Text);
    }

    #[test]
    fn commercial_skip_needs_more_than_fifty_tics() {
        let mut f = Finale::new(FinaleKind::Commercial { map: 6 });
        f.drain_events();
        // the original checks `finalecount > 50` before incrementing,
        // so the button must be held on the 52nd tic to skip
        for _ in 0..51 {
            f.tick(&FinaleInput { any_button: true }, &TestTables);
        }
        assert!(f.drain_events().is_empty(), "no skip at t<=51");
        f.tick(&FinaleInput { any_button: true }, &TestTables); // t=52
        assert_eq!(
            f.drain_events(),
            vec![FinaleEvent::WorldDone],
            "button after 50 tics skips the episode"
        );
    }

    #[test]
    fn commercial_map30_starts_the_cast_call() {
        let mut f = Finale::new(FinaleKind::Commercial { map: 30 });
        f.drain_events();
        for _ in 0..51 {
            f.tick(&FinaleInput::default(), &TestTables);
        }
        f.tick(&FinaleInput { any_button: true }, &TestTables);
        assert_eq!(f.stage(), FinaleStage::Cast);
        assert_eq!(
            f.drain_events(),
            vec![
                FinaleEvent::ForceWipe,
                FinaleEvent::Music(FinaleMusic::Evil)
            ]
        );
    }

    #[test]
    fn on_keydown_only_matters_in_the_cast() {
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        assert!(!f.on_keydown(&TestTables));
    }

    #[test]
    fn cast_call_walks_the_animation() {
        let tables = TestTables;
        let mut f = Finale::new(FinaleKind::Commercial { map: 30 });
        f.drain_events();
        enter_cast(&mut f, &tables);

        // seestate self-loops: every tic advances; after the 12th the
        // puppet switches into the missile frame
        for _ in 0..12 {
            f.tick(&FinaleInput::default(), &tables);
        }
        assert_eq!(
            f.cast.as_ref().unwrap().caststate,
            2,
            "missile frame attack"
        );

        // after the attack animation (24 frames total) it returns to seestate
        for _ in 0..12 {
            f.tick(&FinaleInput::default(), &tables);
        }
        assert_eq!(f.cast.as_ref().unwrap().caststate, 1, "back to seestate");
    }

    #[test]
    fn cast_call_keypress_kills_the_actor() {
        let tables = TestTables;
        let mut f = Finale::new(FinaleKind::Commercial { map: 30 });
        f.drain_events();
        enter_cast(&mut f, &tables);

        assert!(f.on_keydown(&tables));
        assert_eq!(f.cast.as_ref().unwrap().caststate, 3, "death frame");
        assert_eq!(
            f.drain_events(),
            vec![FinaleEvent::Sound(FinaleSound::Sklatk)]
        );
        // second keypress is swallowed with no extra effect
        assert!(f.on_keydown(&tables));
        assert!(f.drain_events().is_empty());

        // the death anim is 2 tics then S_NULL -> next monster in lineup
        for _ in 0..3 {
            f.tick(&FinaleInput::default(), &tables);
        }
        assert_eq!(f.cast.as_ref().unwrap().castnum, 1);
        assert_eq!(
            f.drain_events(),
            vec![FinaleEvent::Sound(FinaleSound::Pistol)],
            "seesound of the next actor"
        );
    }

    #[test]
    fn cast_call_wraps_around_the_lineup() {
        let tables = TestTables;
        let mut f = Finale::new(FinaleKind::Commercial { map: 30 });
        f.drain_events();
        enter_cast(&mut f, &tables);
        // kill all 17 actors, one after another
        for expected in 1..=17 {
            f.on_keydown(&tables);
            for _ in 0..3 {
                f.tick(&FinaleInput::default(), &tables);
            }
            let cast = f.cast.as_ref().unwrap();
            assert_eq!(cast.castnum, expected % 17, "after kill {expected}");
        }
    }

    #[test]
    fn text_draw_tiles_flat_and_types_out_glyphs() {
        let help = fill_patch_bytes(2, 2, 0x33);
        let rm = manager_with_flat("FLOOR4_8", 0x21, &[("HELP2", &help)]);
        let font = synthetic_font();
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        f.finalecount = 13; // count == 1 -> one glyph
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        // flat everywhere else
        assert_eq!(fb.get(0, 0), Some(0x21));
        assert_eq!(fb.get(200, 150), Some(0x21));
        // first glyph of E1TEXT is 'O' -> glyph index 0x4F - 0x21 = 46, color 47
        let o_color = 1 + (b'O' - crate::hud::HU_FONTSTART);
        assert_eq!(fb.get(10, 10), Some(o_color));
        assert_eq!(fb.get(12, 16), Some(o_color));
        assert_eq!(fb.get(13, 10), Some(0x21), "second glyph not drawn yet");
    }

    #[test]
    fn text_draw_reveals_more_with_time() {
        let help = fill_patch_bytes(2, 2, 0x33);
        let rm = manager_with_flat("FLOOR4_8", 0x21, &[("HELP2", &help)]);
        let font = synthetic_font();
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        f.finalecount = 16; // count == 2 -> two glyphs ('O', 'n')
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        assert_ne!(fb.get(13, 10), Some(0x21), "second glyph landed");
    }

    #[test]
    fn text_draw_respects_newlines() {
        let help = fill_patch_bytes(2, 2, 0x33);
        let rm = manager_with_flat("FLOOR4_8", 0x21, &[("HELP2", &help)]);
        let font = synthetic_font();
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        // E1TEXT's first line ends at the '\n' after "badasses and"; a
        // reveal long enough to cross it draws into the second text row
        f.finalecount = 10 + 3 * 60;
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        // second line starts at y = 10 + 11; the flat only filled row 10..16 on line 1
        assert_ne!(fb.get(10, 21), Some(0x21), "glyphs on the second line");
    }

    #[test]
    fn art_stage_draws_episode_patch() {
        let help = fill_patch_bytes(2, 2, 0x33);
        let rm = manager_with_flat("FLOOR4_8", 0x21, &[("HELP2", &help)]);
        let font = synthetic_font();
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 1,
            retail: false,
        });
        f.stage = FinaleStage::Art;
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        assert_eq!(fb.get(0, 0), Some(0x33));
        assert_eq!(fb.get(1, 1), Some(0x33));
    }

    #[test]
    fn bunny_scroll_at_start_shows_the_scrolled_p1_half() {
        let p1 = fill_patch_bytes(320, 200, 0x44); // PFUB2
        let p2 = fill_patch_bytes(320, 200, 0x42); // PFUB1
        let rm = manager_with_flat("MFLR8_4", 0x21, &[("PFUB1", &p2), ("PFUB2", &p1)]);
        let font = synthetic_font();
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 3,
            retail: false,
        });
        f.stage = FinaleStage::Art;

        // finalecount = 0 -> scroll offset clamps to 320: the screen shows
        // the PFUB1 (already-scrolled) half end to end
        f.finalecount = 0;
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        assert_eq!(fb.get(0, 0), Some(0x42));
        assert_eq!(fb.get(319, 199), Some(0x42));

        // finalecount = 430 -> scrolled = 320 - (430-230)/2 = 220: the left
        // 100 columns show PFUB2, the rest is still PFUB1
        f.finalecount = 430;
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        assert_eq!(fb.get(0, 0), Some(0x44), "PFUB2 column 220 lands at x=0");
        assert_eq!(fb.get(99, 199), Some(0x44), "PFUB2 reaches x=99");
        assert_eq!(fb.get(100, 100), Some(0x42), "PFUB1 takes over at x=100");
        assert_eq!(fb.get(319, 0), Some(0x42));
    }

    #[test]
    fn bunny_scroll_draws_end_frame_on_the_last_night() {
        let p1 = fill_patch_bytes(320, 200, 0x44);
        let p2 = fill_patch_bytes(320, 200, 0x42);
        let end0 = fill_patch_bytes(104, 64, 0x99);
        let rm = manager_with_flat(
            "MFLR8_4",
            0x21,
            &[("PFUB1", &p1), ("PFUB2", &p2), ("END0", &end0)],
        );
        let font = synthetic_font();
        let mut f = Finale::new(FinaleKind::Episode {
            episode: 3,
            retail: false,
        });
        f.stage = FinaleStage::Art;
        f.finalecount = 1150;
        let mut fb = IndexedFramebuffer::new();
        f.draw(
            &mut fb,
            &font,
            &rm,
            &crate::render::SpriteDefinitions::default(),
            &TestTables,
        )
        .unwrap();
        // ENDx at (108, 68)
        assert_eq!(fb.get(108, 68), Some(0x99));
        assert_eq!(fb.get(211, 131), Some(0x99));
        assert_eq!(
            fb.get(0, 0),
            Some(0x42),
            "far half still visible at the top-left"
        );
    }

    #[test]
    fn cast_draw_paints_background_name_and_actor() {
        let bossback = fill_patch_bytes(2, 2, 0x77);
        let poss = fill_patch_bytes(5, 5, 0x55);
        let rm = manager_with_flat(
            "RROCK17",
            0x21,
            &[
                ("BOSSBACK", &bossback),
                ("S_START", &[]),
                ("POSSA0", &poss),
                ("S_END", &[]),
            ],
        );
        let font = synthetic_font();
        let sprites = crate::render::SpriteDefinitions::build(&[*b"POSS"], &rm).unwrap();
        let tables = TestTables;
        let mut f = Finale::new(FinaleKind::Commercial { map: 30 });
        f.drain_events();
        enter_cast(&mut f, &tables);
        let mut fb = IndexedFramebuffer::new();
        f.draw(&mut fb, &font, &rm, &sprites, &tables).unwrap();
        assert_eq!(fb.get(0, 0), Some(0x77), "bossback in the corner");
        assert_eq!(fb.get(160, 170), Some(0x55), "the actor sprite");
        // ZOMBIEMAN is wider than 3px, so the glyphs at y=180 cover some center columns
        assert_ne!(fb.get(160, 185), Some(0x77), "the name is printed");
    }

    #[test]
    fn flipped_patch_draws_columns_mirrored() {
        // asymmetric patch: columns colored 1, 2, 3
        let patch = fill_patch(3, 1, 0);
        let mut patch = patch;
        for (i, col) in patch.columns.iter_mut().enumerate() {
            for post in col.posts.iter_mut() {
                post.pixels.fill(i as u8 + 10);
            }
        }
        let mut fb = IndexedFramebuffer::new();
        draw_patch_flipped(&mut fb, &patch, 0, 0);
        assert_eq!(fb.get(0, 0), Some(12));
        assert_eq!(fb.get(1, 0), Some(11));
        assert_eq!(fb.get(2, 0), Some(10));
    }
}

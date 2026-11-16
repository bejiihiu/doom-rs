//! The DOOM II MAP30 cast-call cast tables, wired to the ported
//! `STATES[]`/`MOBJ_INFO[]` tables (the `info` module).
//!
//! Vanilla drove the cast call from the same `states[]` and `mobjinfo[]`
//! rows the game uses - the finale just needed a trait so the info data
//! could be injected. This is that injection: zero copies, straight
//! indices into the static tables.

use crate::finale::{CastActorInfo, CastActorKind, CastStateFrame, CastTables, FinaleSound};
use crate::info::{MOBJ_INFO, MobjType, STATES, SfxId};

impl CastActorKind {
    const fn mobj_type(self) -> MobjType {
        match self {
            CastActorKind::Zombieman => MobjType::MT_POSSESSED,
            CastActorKind::ShotgunGuy => MobjType::MT_SHOTGUY,
            CastActorKind::HeavyWeaponDude => MobjType::MT_CHAINGUY,
            CastActorKind::Imp => MobjType::MT_TROOP,
            CastActorKind::Demon => MobjType::MT_SERGEANT,
            CastActorKind::LostSoul => MobjType::MT_SKULL,
            CastActorKind::Cacodemon => MobjType::MT_HEAD,
            CastActorKind::HellKnight => MobjType::MT_KNIGHT,
            CastActorKind::BaronOfHell => MobjType::MT_BRUISER,
            CastActorKind::Arachnotron => MobjType::MT_BABY,
            CastActorKind::PainElemental => MobjType::MT_PAIN,
            CastActorKind::Revenant => MobjType::MT_UNDEAD,
            CastActorKind::Mancubus => MobjType::MT_FATSO,
            CastActorKind::ArchVile => MobjType::MT_VILE,
            CastActorKind::SpiderMastermind => MobjType::MT_SPIDER,
            CastActorKind::Cyberdemon => MobjType::MT_CYBORG,
            CastActorKind::OurHero => MobjType::MT_PLAYER,
        }
    }
}

/// Map an sfx id to the finale's sound enum. Every id the cast call can
/// reach has a variant (the creature voice lines included); anything
/// else is a programming error and yields `None` so the game stays
/// silent rather than playing a wrong sound.
fn finale_sound(sfx: SfxId) -> Option<FinaleSound> {
    Some(match sfx {
        SfxId::sfx_pistol => FinaleSound::Pistol,
        SfxId::sfx_shotgn => FinaleSound::Shotgn,
        SfxId::sfx_claw => FinaleSound::Claw,
        SfxId::sfx_sgtatk => FinaleSound::Sgtatk,
        SfxId::sfx_vilatk => FinaleSound::Vilatk,
        SfxId::sfx_dshtgn => FinaleSound::Dshtgn,
        SfxId::sfx_skeswg => FinaleSound::Skeswg,
        SfxId::sfx_skepch => FinaleSound::Skepch,
        SfxId::sfx_skeatk => FinaleSound::Skeatk,
        SfxId::sfx_firsht => FinaleSound::Firsht,
        SfxId::sfx_sklatk => FinaleSound::Sklatk,
        SfxId::sfx_rlaunc => FinaleSound::Rlaunc,
        SfxId::sfx_plasma => FinaleSound::Plasma,
        SfxId::sfx_posit1 => FinaleSound::Posit1,
        SfxId::sfx_posit2 => FinaleSound::Posit2,
        SfxId::sfx_vilsit => FinaleSound::Vilsit,
        SfxId::sfx_skesit => FinaleSound::Skesit,
        SfxId::sfx_mansit => FinaleSound::Mansit,
        SfxId::sfx_bgsit1 => FinaleSound::Bgsit1,
        SfxId::sfx_sgtsit => FinaleSound::Sgtsit,
        SfxId::sfx_cacsit => FinaleSound::Cacsit,
        SfxId::sfx_brssit => FinaleSound::Brssit,
        SfxId::sfx_kntsit => FinaleSound::Kntsit,
        SfxId::sfx_spisit => FinaleSound::Spisit,
        SfxId::sfx_bspsit => FinaleSound::Bspsit,
        SfxId::sfx_cybsit => FinaleSound::Cybsit,
        SfxId::sfx_pesit => FinaleSound::Pesit,
        SfxId::sfx_podth1 => FinaleSound::Podth1,
        SfxId::sfx_podth2 => FinaleSound::Podth2,
        SfxId::sfx_vildth => FinaleSound::Vildth,
        SfxId::sfx_skedth => FinaleSound::Skedth,
        SfxId::sfx_mandth => FinaleSound::Mandth,
        SfxId::sfx_bgdth1 => FinaleSound::Bgdth1,
        SfxId::sfx_sgtdth => FinaleSound::Sgtdth,
        SfxId::sfx_cacdth => FinaleSound::Cacdth,
        SfxId::sfx_brsdth => FinaleSound::Brsdth,
        SfxId::sfx_kntdth => FinaleSound::Kntdth,
        SfxId::sfx_firxpl => FinaleSound::Firxpl,
        SfxId::sfx_spidth => FinaleSound::Spidth,
        SfxId::sfx_bspdth => FinaleSound::Bspdth,
        SfxId::sfx_cybdth => FinaleSound::Cybdth,
        SfxId::sfx_pedth => FinaleSound::Pedth,
        SfxId::sfx_pldeth => FinaleSound::Pldeth,
        _ => return None,
    })
}

/// The game's cast-call table: a zero-sized handle that reads straight
/// out of the static info tables.
pub struct GameCastTables;

impl GameCastTables {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GameCastTables {
    fn default() -> Self {
        Self::new()
    }
}

impl CastTables for GameCastTables {
    fn actor(&self, kind: CastActorKind) -> CastActorInfo {
        let info = MOBJ_INFO[kind.mobj_type() as usize];
        CastActorInfo {
            seestate: info.seestate as usize,
            meleestate: info.meleestate as usize,
            missilestate: info.missilestate as usize,
            deathstate: info.deathstate as usize,
            seesound: finale_sound(info.seesound),
            deathsound: finale_sound(info.deathsound),
        }
    }

    fn state(&self, index: usize) -> CastStateFrame {
        let state = &STATES[index];
        CastStateFrame {
            sprite: state.sprite as usize,
            frame: state.frame as i32,
            tics: i32::from(state.tics),
            nextstate: state.next as usize,
        }
    }
}

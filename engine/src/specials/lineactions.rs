//! The line-special interpreter, ported from `p_spec.c`, `p_switch.c`
//! (`P_UseSpecialLine`) and `p_switch.c`'s button handling.
//!
//! These are the dispatch tables `P_CrossSpecialLine`,
//! `P_UseSpecialLine` and `P_ShootSpecialLine` were in the original.
//! The game loop calls [`handle_line_activation`] when a thing crosses,
//! uses or shoots a special line; everything the action does comes out
//! as plain-data events on the context.
//!
//! ## Coverage
//!
//! Every special used by the three tables in the original is here:
//! the `use` table (`p_switch.c`), the `cross` table (`p_spec.c`,
//! triggers and retriggers) and the `shoot` table (`p_spec.c`).
//! Unmatched numbers and unimplemented activation paths return quietly
//! (a `tracing::warn!`, never a panic) — an editor-authored special
//! number this engine doesn't know must not crash a running level.
//!
//! ## Exit specials
//!
//! Line specials 52 and 124 (exit / secret exit, walk-over), and 11 and
//! 51 (exit / secret exit, switch use) emit
//! [`crate::specials::SpecialEvent::ExitLevel`] with the `secret` flag
//! set. The exit *handling* (wiping to the intermission, the secret-exit
//! map change) is the game loop's, not this layer's — the event is the
//! hand-off. `G_ExitLevel` in the original also fires from boss deaths;
//! that path goes through [`crate::enemy::GameEvent::ExitLevel`] from
//! the AI layer instead.
//!
//! ## `P_SpawnSpecials` / `P_UpdateSpecials`
//!
//! [`spawn_specials`] ports `P_SpawnSpecials` (the map-load scan: sector
//! specials 1/2/3/4/8/9/10/12/13/14/17, the scroll line 48 registry).
//! The animated flats/textures half of `P_UpdateSpecials` needs the
//! render layer's translation tables and stays there; what the game loop
//! owns — the level timer, the scroll lines and the pressed buttons — is
//! [`update_specials`].
//!
//! ## `P_PlayerInSpecialSector`
//!
//! [`player_in_special_sector`] ports the per-tic sector damage (and the
//! secret sector accounting). Sector specials 5/7/16/4/9/11 are handled;
//! unknown sector specials are logged and ignored (vanilla aborted with
//! `I_Error`).

use crate::fixed::Fixed;
use crate::map::types::ML_SECRET;
use crate::player::{CF_GODMODE, Power};
use crate::specials::{
    ButtonList, CeilingKind, DoorKind, FloorKind, PlatKind, SpecialContext, SpecialEvent,
    StairKind, SwitchList, change_switch_texture, ev_build_stairs, ev_ceiling_crush_stop,
    ev_do_ceiling, ev_do_donut, ev_do_door, ev_do_floor, ev_do_locked_door, ev_do_plat,
    ev_light_turn_on, ev_start_light_strobing, ev_stop_plat, ev_teleport, ev_turn_tag_lights_off,
    ev_vertical_door, spawn_door_close_in_30, spawn_door_raise_in_5_mins, spawn_fire_flicker,
    spawn_glowing_light, spawn_light_flash, spawn_strobe_flash,
};
use crate::specials::{FASTDARK, SLOWDARK};
use crate::thinker::ThinkerId;

/// Which situation is activating the line — vanilla's three call sites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineAction {
    /// `P_UseSpecialLine`: the player (or a monster) pressed use.
    /// `side` is the line's 0/1 side the thing is on (vanilla required
    /// front sides except for an unused sliding-door special).
    Use {
        /// Which side activated it (0 = front, 1 = back).
        side: i32,
        /// The thing activating the line.
        thing: ThinkerId,
    },
    /// `P_CrossSpecialLine`: a thing's origin crossed the line.
    Cross {
        /// Which side the thing was on before the crossing.
        side: i32,
        /// The thing crossing the line.
        thing: ThinkerId,
    },
    /// `P_ShootSpecialLine`: a damaging shot hit the line.
    Shoot {
        /// The thing that shot the line.
        thing: ThinkerId,
    },
}

/// The switch machinery a dispatch needs: the registered switch texture
/// table and the pressed-button list (both game-owned in vanilla).
///
/// The line-action tables swap switch textures on activation; threading
/// this through keeps `SpecialContext` free of game-loop state.
#[derive(Debug)]
pub struct SwitchState<'a> {
    /// The episode-filtered switch texture table.
    pub list: &'a SwitchList,
    /// The pressed buttons waiting to revert.
    pub buttons: &'a mut ButtonList,
}

/// Vanilla `P_UseSpecialLine` semantics: front sides only (except the
/// unused 124), monsters only touch the manual doors and never touch
/// secret doors, everything else returns quietly. The return value
/// mirrors vanilla: `true` means "the line can be passed" (monsters use
/// it to tell whether a door gave way).
pub fn use_special_line(
    ctx: &mut SpecialContext,
    switches: &mut SwitchState<'_>,
    line: usize,
    side: i32,
    thing: ThinkerId,
) -> bool {
    // vanilla only lets 124 ("sliding door open&close", unused) through
    // on the back side; every other special refuses them
    if side != 0 && ctx.map.linedefs[line].special != 124 {
        return false;
    }

    let special = ctx.map.linedefs[line].special;
    let thing_is_player = ctx
        .store
        .mobj(thing)
        .map(|m| m.player.is_some())
        .unwrap_or(false);

    if !thing_is_player {
        // monsters never open secret doors
        if (ctx.map.linedefs[line].flags & ML_SECRET) != 0 {
            return false;
        }
        match special {
            // only the manual doors
            1 | 32 | 33 | 34 => {}
            _ => return false,
        }
    }

    // the switch action helper: texture swap only when the EV call
    // said it did something, exactly like vanilla's
    // `if (EV_...) P_ChangeSwitchTexture(line, useAgain)`
    let switch =
        |ctx: &mut SpecialContext, switches: &mut SwitchState<'_>, fired: bool, use_again: bool| {
            if fired {
                change_switch_texture(
                    ctx.map,
                    switches.buttons,
                    switches.list,
                    &mut ctx.events,
                    line,
                    use_again,
                );
            }
        };

    match special {
        // MANUALS (no texture swap)
        1 | 26 | 27 | 28 | 31 | 32 | 33 | 34 | 117 | 118 => {
            ev_vertical_door(ctx, line, thing);
        }

        // SWITCHES (one-shot)
        7 => {
            let fired = ev_build_stairs(ctx, line, StairKind::Build8);
            switch(ctx, switches, fired, false);
        }
        9 => {
            let fired = ev_do_donut(ctx, line);
            switch(ctx, switches, fired, false);
        }
        11 => {
            change_switch_texture(
                ctx.map,
                switches.buttons,
                switches.list,
                &mut ctx.events,
                line,
                false,
            );
            ctx.events.push(SpecialEvent::ExitLevel { secret: false });
        }
        14 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseAndChange, 32);
            switch(ctx, switches, fired, false);
        }
        15 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseAndChange, 24);
            switch(ctx, switches, fired, false);
        }
        18 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloorToNearest);
            switch(ctx, switches, fired, false);
        }
        20 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseToNearestAndChange, 0);
            switch(ctx, switches, fired, false);
        }
        21 => {
            let fired = ev_do_plat(ctx, line, PlatKind::DownWaitUpStay, 0);
            switch(ctx, switches, fired, false);
        }
        23 => {
            let fired = ev_do_floor(ctx, line, FloorKind::LowerFloorToLowest);
            switch(ctx, switches, fired, false);
        }
        29 => {
            let fired = ev_do_door(ctx, line, DoorKind::Normal);
            switch(ctx, switches, fired, false);
        }
        41 => {
            let fired = ev_do_ceiling(ctx, line, CeilingKind::LowerToFloor);
            switch(ctx, switches, fired, false);
        }
        49 => {
            let fired = ev_do_ceiling(ctx, line, CeilingKind::CrushAndRaise);
            switch(ctx, switches, fired, false);
        }
        50 => {
            let fired = ev_do_door(ctx, line, DoorKind::Close);
            switch(ctx, switches, fired, false);
        }
        51 => {
            change_switch_texture(
                ctx.map,
                switches.buttons,
                switches.list,
                &mut ctx.events,
                line,
                false,
            );
            ctx.events.push(SpecialEvent::ExitLevel { secret: true });
        }
        55 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloorCrush);
            switch(ctx, switches, fired, false);
        }
        71 => {
            let fired = ev_do_floor(ctx, line, FloorKind::TurboLower);
            switch(ctx, switches, fired, false);
        }
        101 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloor);
            switch(ctx, switches, fired, false);
        }
        102 => {
            let fired = ev_do_floor(ctx, line, FloorKind::LowerFloor);
            switch(ctx, switches, fired, false);
        }
        103 => {
            let fired = ev_do_door(ctx, line, DoorKind::Open);
            switch(ctx, switches, fired, false);
        }
        111 => {
            let fired = ev_do_door(ctx, line, DoorKind::BlazeRaise);
            switch(ctx, switches, fired, false);
        }
        112 => {
            let fired = ev_do_door(ctx, line, DoorKind::BlazeOpen);
            switch(ctx, switches, fired, false);
        }
        113 => {
            let fired = ev_do_door(ctx, line, DoorKind::BlazeClose);
            switch(ctx, switches, fired, false);
        }
        122 => {
            let fired = ev_do_plat(ctx, line, PlatKind::BlazeDWUS, 0);
            switch(ctx, switches, fired, false);
        }
        127 => {
            let fired = ev_build_stairs(ctx, line, StairKind::Turbo16);
            switch(ctx, switches, fired, false);
        }
        131 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloorTurbo);
            switch(ctx, switches, fired, false);
        }
        133 | 135 | 137 => {
            let fired = ev_do_locked_door(ctx, line, DoorKind::BlazeOpen, thing);
            switch(ctx, switches, fired, false);
        }
        140 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloor512);
            switch(ctx, switches, fired, false);
        }

        // BUTTONS (reverting)
        42 => {
            let fired = ev_do_door(ctx, line, DoorKind::Close);
            switch(ctx, switches, fired, true);
        }
        43 => {
            let fired = ev_do_ceiling(ctx, line, CeilingKind::LowerToFloor);
            switch(ctx, switches, fired, true);
        }
        45 => {
            let fired = ev_do_floor(ctx, line, FloorKind::LowerFloor);
            switch(ctx, switches, fired, true);
        }
        60 => {
            let fired = ev_do_floor(ctx, line, FloorKind::LowerFloorToLowest);
            switch(ctx, switches, fired, true);
        }
        61 => {
            let fired = ev_do_door(ctx, line, DoorKind::Open);
            switch(ctx, switches, fired, true);
        }
        62 => {
            let fired = ev_do_plat(ctx, line, PlatKind::DownWaitUpStay, 0);
            switch(ctx, switches, fired, true);
        }
        63 => {
            let fired = ev_do_door(ctx, line, DoorKind::Normal);
            switch(ctx, switches, fired, true);
        }
        64 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloor);
            switch(ctx, switches, fired, true);
        }
        65 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloorCrush);
            switch(ctx, switches, fired, true);
        }
        66 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseAndChange, 24);
            switch(ctx, switches, fired, true);
        }
        67 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseAndChange, 32);
            switch(ctx, switches, fired, true);
        }
        68 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseToNearestAndChange, 0);
            switch(ctx, switches, fired, true);
        }
        69 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloorToNearest);
            switch(ctx, switches, fired, true);
        }
        70 => {
            let fired = ev_do_floor(ctx, line, FloorKind::TurboLower);
            switch(ctx, switches, fired, true);
        }
        99 | 134 | 136 => {
            let fired = ev_do_locked_door(ctx, line, DoorKind::BlazeOpen, thing);
            switch(ctx, switches, fired, true);
        }
        114 => {
            let fired = ev_do_door(ctx, line, DoorKind::BlazeRaise);
            switch(ctx, switches, fired, true);
        }
        115 => {
            let fired = ev_do_door(ctx, line, DoorKind::BlazeOpen);
            switch(ctx, switches, fired, true);
        }
        116 => {
            let fired = ev_do_door(ctx, line, DoorKind::BlazeClose);
            switch(ctx, switches, fired, true);
        }
        123 => {
            let fired = ev_do_plat(ctx, line, PlatKind::BlazeDWUS, 0);
            switch(ctx, switches, fired, true);
        }
        132 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloorTurbo);
            switch(ctx, switches, fired, true);
        }
        138 => {
            ev_light_turn_on(ctx, line, 255);
            switch(ctx, switches, true, true);
        }
        139 => {
            ev_light_turn_on(ctx, line, 35);
            switch(ctx, switches, true, true);
        }

        _ => {
            // unknown or unhandled special: quietly consumed, like vanilla
            // falling out of the switch (which returned true anyway)
            if special != 0 {
                tracing::warn!("use_special_line: unhandled line special {special} on line {line}");
            }
        }
    }

    // vanilla falls out of the switch and returns true here
    true
}

/// Vanilla `P_CrossSpecialLine` semantics: missiles and non-player
/// monsters can only trip the teleport/plat/door retrigger families;
/// trigger specials fire once and clear themselves.
pub fn cross_special_line(ctx: &mut SpecialContext, line: usize, side: i32, thing: ThinkerId) {
    let special = ctx.map.linedefs[line].special;
    let mobj = ctx.store.mobj(thing);
    let (thing_is_player, thing_type) = match mobj {
        Some(mobj) => (mobj.player.is_some(), mobj.mobj_type),
        None => {
            tracing::warn!("cross_special_line: stale thing handle {thing:?}");
            return;
        }
    };

    if !thing_is_player {
        // missiles never trip specials
        use crate::info::MobjType;
        match thing_type {
            MobjType::MT_ROCKET
            | MobjType::MT_PLASMA
            | MobjType::MT_BFG
            | MobjType::MT_TROOPSHOT
            | MobjType::MT_HEADSHOT
            | MobjType::MT_BRUISERSHOT => return,
            _ => {}
        }

        // only these may be triggered by monsters
        let ok = matches!(special, 39 | 97 | 125 | 126 | 4 | 10 | 88);
        if !ok {
            return;
        }
    }

    // trigger specials fire once and clear themselves
    let clear = |ctx: &mut SpecialContext| {
        ctx.map.linedefs[line].special = 0;
    };

    match special {
        // TRIGGERS (one-shot)
        2 => {
            ev_do_door(ctx, line, DoorKind::Open);
            clear(ctx);
        }
        3 => {
            ev_do_door(ctx, line, DoorKind::Close);
            clear(ctx);
        }
        4 => {
            ev_do_door(ctx, line, DoorKind::Normal);
            clear(ctx);
        }
        5 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloor);
            clear(ctx);
        }
        6 => {
            ev_do_ceiling(ctx, line, CeilingKind::FastCrushAndRaise);
            clear(ctx);
        }
        8 => {
            ev_build_stairs(ctx, line, StairKind::Build8);
            clear(ctx);
        }
        10 => {
            ev_do_plat(ctx, line, PlatKind::DownWaitUpStay, 0);
            clear(ctx);
        }
        12 => {
            ev_light_turn_on(ctx, line, 0);
            clear(ctx);
        }
        13 => {
            ev_light_turn_on(ctx, line, 255);
            clear(ctx);
        }
        16 => {
            ev_do_door(ctx, line, DoorKind::Close30ThenOpen);
            clear(ctx);
        }
        17 => {
            ev_start_light_strobing(ctx, line);
            clear(ctx);
        }
        19 => {
            ev_do_floor(ctx, line, FloorKind::LowerFloor);
            clear(ctx);
        }
        22 => {
            ev_do_plat(ctx, line, PlatKind::RaiseToNearestAndChange, 0);
            clear(ctx);
        }
        25 => {
            ev_do_ceiling(ctx, line, CeilingKind::CrushAndRaise);
            clear(ctx);
        }
        30 => {
            ev_do_floor(ctx, line, FloorKind::RaiseToTexture);
            clear(ctx);
        }
        35 => {
            ev_light_turn_on(ctx, line, 35);
            clear(ctx);
        }
        36 => {
            ev_do_floor(ctx, line, FloorKind::TurboLower);
            clear(ctx);
        }
        37 => {
            ev_do_floor(ctx, line, FloorKind::LowerAndChange);
            clear(ctx);
        }
        38 => {
            ev_do_floor(ctx, line, FloorKind::LowerFloorToLowest);
            clear(ctx);
        }
        39 => {
            ev_teleport(ctx, line, side, thing);
            clear(ctx);
        }
        40 => {
            ev_do_ceiling(ctx, line, CeilingKind::RaiseToHighest);
            ev_do_floor(ctx, line, FloorKind::LowerFloorToLowest);
            clear(ctx);
        }
        44 => {
            ev_do_ceiling(ctx, line, CeilingKind::LowerAndCrush);
            clear(ctx);
        }
        52 => {
            // vanilla does NOT clear the special here (the level ends)
            ctx.events.push(SpecialEvent::ExitLevel { secret: false });
        }
        53 => {
            ev_do_plat(ctx, line, PlatKind::PerpetualRaise, 0);
            clear(ctx);
        }
        54 => {
            ev_stop_plat(ctx, line);
            clear(ctx);
        }
        56 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloorCrush);
            clear(ctx);
        }
        57 => {
            ev_ceiling_crush_stop(ctx, line);
            clear(ctx);
        }
        58 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloor24);
            clear(ctx);
        }
        59 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloor24AndChange);
            clear(ctx);
        }
        100 => {
            ev_build_stairs(ctx, line, StairKind::Turbo16);
            clear(ctx);
        }
        104 => {
            ev_turn_tag_lights_off(ctx, line);
            clear(ctx);
        }
        108 => {
            ev_do_door(ctx, line, DoorKind::BlazeRaise);
            clear(ctx);
        }
        109 => {
            ev_do_door(ctx, line, DoorKind::BlazeOpen);
            clear(ctx);
        }
        110 => {
            ev_do_door(ctx, line, DoorKind::BlazeClose);
            clear(ctx);
        }
        119 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloorToNearest);
            clear(ctx);
        }
        121 => {
            ev_do_plat(ctx, line, PlatKind::BlazeDWUS, 0);
            clear(ctx);
        }
        124 => {
            // vanilla does NOT clear the special here (the level ends)
            ctx.events.push(SpecialEvent::ExitLevel { secret: true });
        }
        125 => {
            // teleport monsters only; players walk through
            if !thing_is_player {
                ev_teleport(ctx, line, side, thing);
                clear(ctx);
            }
        }
        130 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloorTurbo);
            clear(ctx);
        }
        141 => {
            ev_do_ceiling(ctx, line, CeilingKind::SilentCrushAndRaise);
            clear(ctx);
        }

        // RETRIGGERS (never cleared)
        72 => {
            ev_do_ceiling(ctx, line, CeilingKind::LowerAndCrush);
        }
        73 => {
            ev_do_ceiling(ctx, line, CeilingKind::CrushAndRaise);
        }
        74 => {
            ev_ceiling_crush_stop(ctx, line);
        }
        75 => {
            ev_do_door(ctx, line, DoorKind::Close);
        }
        76 => {
            ev_do_door(ctx, line, DoorKind::Close30ThenOpen);
        }
        77 => {
            ev_do_ceiling(ctx, line, CeilingKind::FastCrushAndRaise);
        }
        79 => {
            ev_light_turn_on(ctx, line, 35);
        }
        80 => {
            ev_light_turn_on(ctx, line, 0);
        }
        81 => {
            ev_light_turn_on(ctx, line, 255);
        }
        82 => {
            ev_do_floor(ctx, line, FloorKind::LowerFloorToLowest);
        }
        83 => {
            ev_do_floor(ctx, line, FloorKind::LowerFloor);
        }
        84 => {
            ev_do_floor(ctx, line, FloorKind::LowerAndChange);
        }
        86 => {
            ev_do_door(ctx, line, DoorKind::Open);
        }
        87 => {
            ev_do_plat(ctx, line, PlatKind::PerpetualRaise, 0);
        }
        88 => {
            ev_do_plat(ctx, line, PlatKind::DownWaitUpStay, 0);
        }
        89 => {
            ev_stop_plat(ctx, line);
        }
        90 => {
            ev_do_door(ctx, line, DoorKind::Normal);
        }
        91 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloor);
        }
        92 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloor24);
        }
        93 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloor24AndChange);
        }
        94 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloorCrush);
        }
        95 => {
            ev_do_plat(ctx, line, PlatKind::RaiseToNearestAndChange, 0);
        }
        96 => {
            ev_do_floor(ctx, line, FloorKind::RaiseToTexture);
        }
        97 => {
            ev_teleport(ctx, line, side, thing);
        }
        98 => {
            ev_do_floor(ctx, line, FloorKind::TurboLower);
        }
        105 => {
            ev_do_door(ctx, line, DoorKind::BlazeRaise);
        }
        106 => {
            ev_do_door(ctx, line, DoorKind::BlazeOpen);
        }
        107 => {
            ev_do_door(ctx, line, DoorKind::BlazeClose);
        }
        120 => {
            ev_do_plat(ctx, line, PlatKind::BlazeDWUS, 0);
        }
        126 => {
            if !thing_is_player {
                ev_teleport(ctx, line, side, thing);
            }
        }
        128 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloorToNearest);
        }
        129 => {
            ev_do_floor(ctx, line, FloorKind::RaiseFloorTurbo);
        }

        _ => {
            // unmatched: quietly consumed
            if ctx.map.linedefs[line].special != 0 {
                tracing::warn!(
                    "cross_special_line: unhandled line special {} on line {line}",
                    ctx.map.linedefs[line].special
                );
            }
        }
    }
}

/// Vanilla `P_ShootSpecialLine` semantics: only 46 may be tripped by
/// monsters. The switch texture swap only happens when the EV call
/// succeeded, like vanilla.
pub fn shoot_special_line(
    ctx: &mut SpecialContext,
    switches: &mut SwitchState<'_>,
    line: usize,
    thing: ThinkerId,
) {
    let special = ctx.map.linedefs[line].special;
    let thing_is_player = ctx
        .store
        .mobj(thing)
        .map(|m| m.player.is_some())
        .unwrap_or(false);

    if !thing_is_player && special != 46 {
        // monsters may only open the 46 door
        return;
    }

    match special {
        24 => {
            let fired = ev_do_floor(ctx, line, FloorKind::RaiseFloor);
            if fired {
                change_switch_texture(
                    ctx.map,
                    switches.buttons,
                    switches.list,
                    &mut ctx.events,
                    line,
                    false,
                );
            }
        }
        46 => {
            let fired = ev_do_door(ctx, line, DoorKind::Open);
            if fired {
                change_switch_texture(
                    ctx.map,
                    switches.buttons,
                    switches.list,
                    &mut ctx.events,
                    line,
                    true,
                );
            }
        }
        47 => {
            let fired = ev_do_plat(ctx, line, PlatKind::RaiseToNearestAndChange, 0);
            if fired {
                change_switch_texture(
                    ctx.map,
                    switches.buttons,
                    switches.list,
                    &mut ctx.events,
                    line,
                    false,
                );
            }
        }
        _ => {
            if special != 0 {
                tracing::warn!(
                    "shoot_special_line: unhandled line special {special} on line {line}"
                );
            }
        }
    }
}

/// One entry point for the game loop: runs the activation matching
/// vanilla's call sites. The return value is the `P_UseSpecialLine`
/// result for [`LineAction::Use`]; for the crossing and shooting paths
/// (vanilla `void`) it is always `true`.
pub fn handle_line_activation(
    ctx: &mut SpecialContext,
    switches: &mut SwitchState<'_>,
    line: usize,
    action: LineAction,
) -> bool {
    match action {
        LineAction::Use { side, thing } => use_special_line(ctx, switches, line, side, thing),
        LineAction::Cross { side, thing } => {
            cross_special_line(ctx, line, side, thing);
            true
        }
        LineAction::Shoot { thing } => {
            shoot_special_line(ctx, switches, line, thing);
            true
        }
    }
}

/// Vanilla `P_SpawnSpecials`: the map-load scan for sector specials and
/// the scroll-line registry. Returns what the game loop needs to keep
/// per-level (secret count, the scroll lines, the level timer).
///
/// `level_timer` mirrors vanilla's `-timer`/`-avg` handling: `Some`
/// ticks start the countdown, `None` disables the timer entirely.
/// The countdown itself lives in [`update_specials`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpawnSpecialsReport {
    /// Vanilla `totalsecret`: sector specials 9 found at map load.
    pub total_secrets: i32,
    /// Vanilla `linespeciallist`: lines with special 48 (the autoscroll
    /// first-column effect), in linedef order.
    pub scroll_lines: Vec<usize>,
    /// The level timer the game loop asked for, in ticks (`None` = off).
    pub level_timer: Option<i32>,
}

/// Vanilla `P_SpawnSpecials`.
pub fn spawn_specials(ctx: &mut SpecialContext, level_timer: Option<i32>) -> SpawnSpecialsReport {
    let mut report = SpawnSpecialsReport {
        level_timer,
        ..SpawnSpecialsReport::default()
    };

    // sector specials that spawn thinkers
    for i in 0..ctx.map.sectors.len() {
        let special = ctx.map.sectors[i].special;
        if special == 0 {
            continue;
        }
        match special {
            1 => spawn_light_flash(ctx, i),
            2 => spawn_strobe_flash(ctx, i, FASTDARK, false),
            3 => spawn_strobe_flash(ctx, i, SLOWDARK, false),
            4 => {
                // strobe + death slime: the sector keeps special 4 for the
                // slime damage, the strobe is a separate thinker
                spawn_strobe_flash(ctx, i, FASTDARK, false);
                ctx.map.sectors[i].special = 4;
            }
            8 => spawn_glowing_light(ctx, i),
            9 => report.total_secrets += 1,
            10 => spawn_door_close_in_30(ctx, i),
            12 => spawn_strobe_flash(ctx, i, SLOWDARK, true),
            13 => spawn_strobe_flash(ctx, i, FASTDARK, true),
            14 => spawn_door_raise_in_5_mins(ctx, i),
            17 => spawn_fire_flicker(ctx, i),
            _ => {}
        }
    }

    // line specials 48: first-column autoscroll
    for (i, ldef) in ctx.map.linedefs.iter().enumerate() {
        if ldef.special == 48 {
            report.scroll_lines.push(i);
        }
    }

    report
}

/// Vanilla `P_UpdateSpecials`: the per-tic specials tick. What the
/// game loop owns — the level timer, the scroll lines and the pressed
/// buttons. The animated flats/textures (`animdefs`/`flattranslation`)
/// feed the render layer's translation tables and are not here.
///
/// `level_timer` counts ticks down; reaching zero emits
/// [`SpecialEvent::ExitLevel`] (vanilla `G_ExitLevel`).
pub fn update_specials(
    ctx: &mut SpecialContext,
    buttons: &mut crate::specials::ButtonList,
    scroll_lines: &[usize],
    level_timer: &mut Option<i32>,
) {
    // LEVEL TIMER
    if let Some(countdown) = level_timer {
        *countdown -= 1;
        if *countdown == 0 {
            *level_timer = None;
            ctx.events.push(SpecialEvent::ExitLevel { secret: false });
        }
    }

    // ANIMATE LINE SPECIALS (special 48: scroll the first column)
    for &line in scroll_lines {
        if let Some(front) = ctx.map.linedefs[line].sidenum[0] {
            ctx.map.sidedefs[front].texture_offset += Fixed::from_raw(crate::fixed::FRACUNIT);
        }
    }

    // DO BUTTONS
    crate::specials::update_buttons(ctx.map, buttons, &mut ctx.events);
}

/// Vanilla `P_PlayerInSpecialSector`: the per-tic check for the
/// sector the player's avatar stands in. `player` is the players-array
/// index. Out-of-body players (no avatar) are ignored.
///
/// Damage comes out as [`SpecialEvent::Damage`] on the avatar; the
/// secret-search sector clears itself. Vanilla aborted on unknown sector
/// specials; they are logged and ignored here.
pub fn player_in_special_sector(ctx: &mut SpecialContext, player: usize) {
    let Some(avatar) = ctx.players[player].mo else {
        return;
    };
    let Some(sector) = ctx.store.mobj(avatar).and_then(|m| m.sector) else {
        return;
    };
    let mobj_z = match ctx.store.mobj(avatar) {
        Some(mobj) => mobj.z,
        None => return,
    };
    if mobj_z != ctx.map.sectors[sector].floor_height {
        // falling, not all the way down yet
        return;
    }

    let ironfeet = ctx.players[player].powers[Power::IronFeet.index()] != 0;
    let phase = ctx.leveltime & 0x1f;

    match ctx.map.sectors[sector].special {
        5 => {
            // HELLSLIME DAMAGE
            if !ironfeet && phase == 0 {
                ctx.events.push(SpecialEvent::Damage {
                    target: avatar,
                    inflictor: None,
                    source: None,
                    damage: 10,
                    blood: None,
                });
            }
        }
        7 => {
            // NUKAGE DAMAGE
            if !ironfeet && phase == 0 {
                ctx.events.push(SpecialEvent::Damage {
                    target: avatar,
                    inflictor: None,
                    source: None,
                    damage: 5,
                    blood: None,
                });
            }
        }
        16 | 4 => {
            // SUPER HELLSLIME / STROBE HURT
            let vulnerable = !ironfeet || ctx.rng.next() < 5;
            if vulnerable && phase == 0 {
                ctx.events.push(SpecialEvent::Damage {
                    target: avatar,
                    inflictor: None,
                    source: None,
                    damage: 20,
                    blood: None,
                });
            }
        }
        9 => {
            // SECRET SECTOR
            ctx.players[player].secretcount += 1;
            ctx.map.sectors[sector].special = 0;
        }
        11 => {
            // EXIT SUPER DAMAGE! (for E1M8 finale)
            ctx.players[player].cheats &= !CF_GODMODE;
            if phase == 0 {
                ctx.events.push(SpecialEvent::Damage {
                    target: avatar,
                    inflictor: None,
                    source: None,
                    damage: 20,
                    blood: None,
                });
            }
            if ctx.players[player].health <= 10 {
                ctx.events.push(SpecialEvent::ExitLevel { secret: false });
            }
        }
        special => {
            tracing::warn!("P_PlayerInSpecialSector: unknown special {special}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;
    use crate::info::MobjType;
    use crate::map::Map;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{
        spawn_player, spawn_thing, split_map, split_map_custom, test_player,
    };
    use crate::specials::{SpecialContext, SpecialEvent, run_thinkers};
    use crate::thinker::Thinker;

    fn fixed(units: i32) -> Fixed {
        Fixed::from_raw(units * FRACUNIT)
    }

    fn name8(name: &str) -> [u8; 8] {
        let mut out = [0u8; 8];
        for (dst, src) in out.iter_mut().zip(name.as_bytes().iter().take(8)) {
            *dst = src.to_ascii_uppercase();
        }
        out
    }

    struct World {
        map: Map,
        store: MobjStore,
        players: Vec<crate::player::Player>,
        rng: DoomRng,
    }

    /// No sector carries tag 0 or 55, so EV_* tag scans find nothing
    /// unless a test tags a sector itself.
    fn world(north_floor: i16, north_ceil: i16, north_tag: i16) -> World {
        let mut map = split_map(north_floor, north_ceil, north_tag);
        map.sectors[0].tag = 55;
        map.sectors[1].tag = north_tag;
        World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        }
    }

    fn ctx<'a>(w: &'a mut World) -> SpecialContext<'a> {
        SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng)
    }

    fn switches() -> (SwitchList, ButtonList) {
        (SwitchList::new(1), ButtonList::new())
    }

    #[test]
    fn every_special_number_fires_without_panicking() {
        // busy sweep: every special value 0..=255 goes through all three
        // activation paths (the table defaults must swallow unknowns)
        for special in 0..=255i16 {
            let mut w = world(0, 128, 99);
            w.map.sectors[0].tag = 55;
            w.map.sectors[1].tag = 99;
            w.map.linedefs[2].special = special;
            w.map.sidedefs[2].midtexture = name8("SW1GRAY");
            let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
            let monster = spawn_thing(
                &mut w.store,
                &w.map,
                &mut w.rng,
                MobjType::MT_POSSESSED,
                128,
                192,
            );
            let mut ctx = ctx(&mut w);
            let (list, mut buttons) = switches();
            let mut sw = SwitchState {
                list: &list,
                buttons: &mut buttons,
            };
            let _ = handle_line_activation(
                &mut ctx,
                &mut sw,
                2,
                LineAction::Use {
                    side: 0,
                    thing: avatar,
                },
            );
            let _ = handle_line_activation(
                &mut ctx,
                &mut sw,
                2,
                LineAction::Cross {
                    side: 0,
                    thing: avatar,
                },
            );
            let _ =
                handle_line_activation(&mut ctx, &mut sw, 2, LineAction::Shoot { thing: avatar });
            let _ = handle_line_activation(
                &mut ctx,
                &mut sw,
                2,
                LineAction::Use {
                    side: 0,
                    thing: monster,
                },
            );
            let _ = handle_line_activation(
                &mut ctx,
                &mut sw,
                2,
                LineAction::Cross {
                    side: 0,
                    thing: monster,
                },
            );
            let _ =
                handle_line_activation(&mut ctx, &mut sw, 2, LineAction::Shoot { thing: monster });
        }
    }

    #[test]
    fn use_exit_switch_fires_exit_and_swaps_texture() {
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 11;
        w.map.sidedefs[4].midtexture = name8("SW1EXIT");
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        assert!(handle_line_activation(
            &mut ctx,
            &mut sw,
            4,
            LineAction::Use {
                side: 0,
                thing: avatar
            }
        ));
        assert!(
            ctx.events
                .iter()
                .any(|e| matches!(e, SpecialEvent::ExitLevel { secret: false }))
        );
        assert_eq!(w.map.linedefs[4].special, 0, "exit switch is one-shot");
    }

    #[test]
    fn use_secret_exit_switch_fires_secret_event() {
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 51;
        w.map.sidedefs[4].midtexture = name8("SW1EXIT");
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        assert!(handle_line_activation(
            &mut ctx,
            &mut sw,
            4,
            LineAction::Use {
                side: 0,
                thing: avatar
            }
        ));
        assert!(
            ctx.events
                .iter()
                .any(|e| matches!(e, SpecialEvent::ExitLevel { secret: true }))
        );
    }

    #[test]
    fn walkover_exit_and_secret_exit() {
        for (special, secret) in [(52i16, false), (124i16, true)] {
            let mut w = world(0, 128, 99);
            w.map.linedefs[4].special = special;
            let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
            let mut ctx = ctx(&mut w);
            let (list, mut buttons) = switches();
            let mut sw = SwitchState {
                list: &list,
                buttons: &mut buttons,
            };

            let _ = handle_line_activation(
                &mut ctx,
                &mut sw,
                4,
                LineAction::Cross {
                    side: 0,
                    thing: avatar,
                },
            );
            assert!(
                ctx.events
                    .iter()
                    .any(|e| matches!(e, SpecialEvent::ExitLevel { secret: s } if *s == secret)),
                "special {special} must fire ExitLevel {{ secret: {secret} }}"
            );
            // exits do not clear the line special (the level ends anyway)
            assert_eq!(ctx.map.linedefs[4].special, special);
        }
    }

    #[test]
    fn monster_uses_manual_door_but_nothing_else() {
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 1; // manual door raise
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        assert!(handle_line_activation(
            &mut ctx,
            &mut sw,
            4,
            LineAction::Use {
                side: 0,
                thing: monster
            }
        ));
        run_thinkers(&mut ctx);
        assert!(
            ctx.store
                .thinkers()
                .any(|(_, t)| matches!(t, Thinker::Door(_)))
        );

        // but a switch line refuses monsters
        w.map.linedefs[4].special = 63;
        let mut ctx = SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng);
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };
        assert!(!handle_line_activation(
            &mut ctx,
            &mut sw,
            4,
            LineAction::Use {
                side: 0,
                thing: monster
            }
        ));
    }

    #[test]
    fn cross_one_shot_clears_but_retrigger_keeps() {
        for (special, expect_cleared) in [(4i16, true), (90i16, false)] {
            let mut w = world(0, 128, 99);
            w.map.linedefs[4].special = special;
            let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
            let mut ctx = ctx(&mut w);

            cross_special_line(&mut ctx, 4, 0, avatar);
            if expect_cleared {
                assert_eq!(ctx.map.linedefs[4].special, 0);
            } else {
                assert_eq!(ctx.map.linedefs[4].special, special);
            }
        }
    }

    #[test]
    fn monster_cross_only_the_ok_specials() {
        for special in [5i16, 91i16, 52i16, 2i16] {
            let mut w = world(0, 128, 99);
            w.map.linedefs[4].special = special;
            let monster = spawn_thing(
                &mut w.store,
                &w.map,
                &mut w.rng,
                MobjType::MT_POSSESSED,
                128,
                192,
            );
            let mut ctx = ctx(&mut w);

            cross_special_line(&mut ctx, 4, 0, monster);
            assert!(
                ctx.pending_thinkers.is_empty(),
                "monster must not trigger special {special}"
            );
        }

        // but 4 (door raise) is in the monster ok list
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 4;
        w.map.linedefs[4].tag = 99;
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        let mut ctx = ctx(&mut w);
        cross_special_line(&mut ctx, 4, 0, monster);
        assert_eq!(ctx.pending_thinkers.len(), 1);
    }

    #[test]
    fn multi_line_tag_fans_out_to_every_tagged_sector() {
        // both sectors carry tag 88: one crossing, two doors
        let mut w = world(0, 128, 88);
        w.map.sectors[0].tag = 88;
        w.map.sectors[1].tag = 88;
        w.map.linedefs[4].special = 4;
        w.map.linedefs[4].tag = 88;
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        let mut ctx = ctx(&mut w);

        cross_special_line(&mut ctx, 4, 0, avatar);
        let doors: Vec<_> = ctx
            .pending_thinkers
            .iter()
            .filter(|t| matches!(t, Thinker::Door(_)))
            .collect();
        assert_eq!(
            doors.len(),
            2,
            "the tag fan-out must hit both tagged sectors"
        );
        assert_eq!(ctx.map.linedefs[4].special, 0);
    }

    #[test]
    fn shoot_24_raises_floor_and_consumes_one_shot_switch() {
        let mut w = world(0, 128, 0); // tag 0 sectors (both) get the floor action
        w.map.linedefs[4].special = 24;
        w.map.sidedefs[4].midtexture = name8("SW1GRAY");
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        shoot_special_line(&mut ctx, &mut sw, 4, avatar);
        assert_eq!(
            ctx.map.linedefs[4].special, 0,
            "the one-shot switch clears its special"
        );
        assert_eq!(w_sidedef_name(ctx.map, 4), name8("SW2GRAY"));
        assert!(
            ctx.pending_thinkers
                .iter()
                .any(|t| matches!(t, Thinker::FloorMover(_))),
            "a floor mover must be queued"
        );
    }

    fn w_sidedef_name(map: &Map, sidedef: usize) -> [u8; 8] {
        map.sidedefs[sidedef].midtexture
    }

    #[test]
    fn shoot_46_opens_door_as_button() {
        let mut w = world(0, 128, 0); // tag 0: the north sector answers
        w.map.linedefs[4].special = 46;
        w.map.sidedefs[4].midtexture = name8("SW1GRAY");
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        shoot_special_line(&mut ctx, &mut sw, 4, avatar);
        assert_eq!(
            ctx.map.linedefs[4].special, 46,
            "buttons keep their special"
        );
        assert_eq!(w_sidedef_name(ctx.map, 4), name8("SW2GRAY"));
        assert_eq!(buttons.len(), 1, "46 is a button (use_again)");
        assert!(
            ctx.pending_thinkers
                .iter()
                .any(|t| matches!(t, Thinker::Door(_))),
            "the door must open"
        );
    }

    #[test]
    fn shoot_by_monster_only_46() {
        let mut w = world(0, 128, 0);
        w.map.linedefs[4].special = 24;
        w.map.sidedefs[4].midtexture = name8("SW1GRAY");
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        shoot_special_line(&mut ctx, &mut sw, 4, monster);
        assert!(
            ctx.pending_thinkers.is_empty(),
            "monsters may not raise floors"
        );
        assert_eq!(ctx.map.linedefs[4].special, 24);
        assert_eq!(w_sidedef_name(ctx.map, 4), name8("SW1GRAY"));
    }

    #[test]
    fn locked_switch_does_not_fire_without_the_card() {
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 133; // BlzOpenDoor BLUE
        w.map.sidedefs[4].midtexture = name8("SW1GRAY");
        w.map.sectors[1].tag = 99;
        w.map.linedefs[4].tag = 99;
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        // vanilla falls out of the switch and returns true even when the
        // locked door refuses; the observable contract is "no texture
        // swap, special kept" (players never consume the return value;
        // monsters are gated out before the switch)
        assert!(handle_line_activation(
            &mut ctx,
            &mut sw,
            4,
            LineAction::Use {
                side: 0,
                thing: avatar
            }
        ));
        assert_eq!(
            w_sidedef_name(ctx.map, 4),
            name8("SW1GRAY"),
            "locked failed: texture unchanged"
        );
        assert_eq!(
            ctx.map.linedefs[4].special, 133,
            "locked failed: special stays"
        );
    }

    #[test]
    fn locked_switch_fires_with_the_card() {
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 133;
        w.map.sidedefs[4].midtexture = name8("SW1GRAY");
        w.map.sectors[1].tag = 99;
        w.map.linedefs[4].tag = 99;
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 192);
        ctx_card(&mut w, 0, crate::player::Card::Blue);
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let mut sw = SwitchState {
            list: &list,
            buttons: &mut buttons,
        };

        assert!(handle_line_activation(
            &mut ctx,
            &mut sw,
            4,
            LineAction::Use {
                side: 0,
                thing: avatar
            }
        ));
        assert_eq!(w_sidedef_name(ctx.map, 4), name8("SW2GRAY"));
        assert_eq!(
            ctx.map.linedefs[4].special, 0,
            "one-shot switch clears its special"
        );
        assert!(
            ctx.store.thinkers().count() >= 1
                || ctx
                    .pending_thinkers
                    .iter()
                    .any(|t| matches!(t, Thinker::Door(_))),
            "the blaze door must spawn"
        );
    }

    fn ctx_card(w: &mut World, player: usize, card: crate::player::Card) {
        w.players[player].cards[card.index()] = true;
    }

    #[test]
    fn spawn_specials_scans_sector_specials_and_scroll_lines() {
        let mut map = split_map_custom("FLOOR4_8", 4, "FLOOR4_8", 9);
        map.linedefs[4].special = 48; // scroll line
        let mut w = World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        };
        let mut ctx = ctx(&mut w);

        let report = spawn_specials(&mut ctx, None);
        assert_eq!(report.total_secrets, 1, "sector 9 is a secret");
        assert_eq!(report.scroll_lines, vec![4]);
        assert_eq!(report.level_timer, None);
        // sector 4 (strobe + slime damage) got a strobe thinker
        assert!(
            ctx.pending_thinkers
                .iter()
                .any(|t| matches!(t, Thinker::StrobeFlash(_))),
            "sector special 4 must spawn a strobe"
        );
        // vanilla zeroes the special in the spawn and P_SpawnSpecials sets
        // it back for special 4 (the slime damage needs it)
        assert_eq!(ctx.map.sectors[0].special, 4);
        // the secret sector keeps its special (it clears on first touch)
        assert_eq!(ctx.map.sectors[1].special, 9);
    }

    #[test]
    fn spawn_specials_close_30_and_raise_5_spawn_doors() {
        let map = split_map_custom("FLOOR4_8", 10, "FLOOR4_8", 14);
        let mut w = World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        };
        let mut ctx = ctx(&mut w);

        let _ = spawn_specials(&mut ctx, None);
        let doors: Vec<_> = ctx
            .pending_thinkers
            .iter()
            .filter(|t| matches!(t, Thinker::Door(_)))
            .collect();
        assert_eq!(doors.len(), 2);
        // the sector specials are consumed by the spawn
        assert_eq!(ctx.map.sectors[0].special, 0);
        assert_eq!(ctx.map.sectors[1].special, 0);
    }

    #[test]
    fn update_specials_scrolls_lines_and_fires_the_timer() {
        let mut w = world(0, 128, 99);
        w.map.linedefs[4].special = 48;
        let mut ctx = ctx(&mut w);
        let (list, mut buttons) = switches();
        let _ = list;

        // scroll line: texture offset advances by one unit per call
        update_specials(&mut ctx, &mut buttons, &[4], &mut None);
        assert_eq!(ctx.map.sidedefs[4].texture_offset, fixed(1));

        // timer fires exactly once at zero
        let mut timer = Some(1);
        update_specials(&mut ctx, &mut buttons, &[], &mut timer);
        assert_eq!(timer, None);
        assert!(
            ctx.events
                .iter()
                .any(|e| matches!(e, SpecialEvent::ExitLevel { secret: false })),
            "the level timer expiry must exit the level"
        );
    }

    #[test]
    fn player_sector_5_damages_without_ironfeet_only() {
        for (ironfeet, expect_damage) in [(0i32, true), (1i32, false)] {
            let mut w = world(0, 128, 99);
            w.map.sectors[0].special = 5;
            let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
            ctx_powers(&mut w, 0, ironfeet);
            let mut ctx = ctx(&mut w);
            ctx.leveltime = 0; // phase 0

            player_in_special_sector(&mut ctx, 0);
            let damaged = ctx
                .events
                .iter()
                .any(|e| matches!(e, SpecialEvent::Damage { target, damage: 10, .. } if *target == avatar));
            assert_eq!(damaged, expect_damage, "ironfeet={ironfeet}");
        }
    }

    fn ctx_powers(w: &mut World, player: usize, ironfeet: i32) {
        w.players[player].powers[crate::player::Power::IronFeet.index()] = ironfeet;
    }

    #[test]
    fn player_sector_9_counts_the_secret_and_clears() {
        let mut w = world(0, 128, 99);
        w.map.sectors[0].special = 9;
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        let _ = avatar;
        let mut ctx = ctx(&mut w);

        player_in_special_sector(&mut ctx, 0);
        assert_eq!(ctx.players[0].secretcount, 1);
        assert_eq!(ctx.map.sectors[0].special, 0);
    }

    #[test]
    fn player_sector_11_strips_godmode_and_exits_at_low_health() {
        let mut w = world(0, 128, 99);
        w.map.sectors[0].special = 11;
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        let _ = avatar;
        w.players[0].cheats |= crate::player::CF_GODMODE;
        w.players[0].health = 8;
        let mut ctx = ctx(&mut w);
        ctx.leveltime = 0;

        player_in_special_sector(&mut ctx, 0);
        assert_eq!(
            ctx.players[0].cheats & crate::player::CF_GODMODE,
            0,
            "god is taken away"
        );
        assert!(
            ctx.events
                .iter()
                .any(|e| matches!(e, SpecialEvent::ExitLevel { secret: false })),
            "E1M8 finale must exit when health <= 10"
        );
    }
}

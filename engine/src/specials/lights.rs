//! Sector light effects, ported from `p_lights.c`.
//!
//! Four thinker kinds (fire flicker, light flash, strobe, glow) plus the
//! two line-trigger actions (turn tag lights off, turn tag lights on) and
//! the strobe-trigger used by line special 17.
//!
//! The light thinkers mutate `sectors[..].light_level` and claim nothing
//! else — lights never set the vanilla `specialdata` slot. A sector with a
//! door thinker cannot gain a *triggered* strobe (`ev_start_light_strobing`
//! checks busy), matching vanilla.
//!
//! Constants are the vanilla ones from `p_spec.h`.

use crate::specials::util::{
    find_min_surrounding_light, find_sector_from_line_tag, get_next_sector, sector_lines,
};
use crate::specials::{PassCtx, SpecialContext, sector_busy};
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

/// Vanilla `GLOWSPEED` (light units per tic for the breathing glow).
pub const GLOWSPEED: i32 = 8;
/// Vanilla `STROBEBRIGHT` (tics a strobe stays bright).
pub const STROBEBRIGHT: i32 = 5;
/// Vanilla `FASTDARK` (dark-phase length for the fast strobe).
pub const FASTDARK: i32 = 15;
/// Vanilla `SLOWDARK` (dark-phase length for the slow strobe).
pub const SLOWDARK: i32 = 35;

/// Vanilla `fireflicker_t`: the torch flicker (sector special 17).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FireFlicker {
    /// Index of the flickering sector.
    pub sector: usize,
    /// Tics until the next light step.
    pub count: i32,
    /// The base (brightest) light level; restored on bright tics.
    pub maxlight: i32,
    /// Dark floor; equals the darkest neighbor + 16.
    pub minlight: i32,
}

/// Vanilla `lightflash_t`: the "broken tube" flash (sector special 1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LightFlash {
    /// Index of the flashing sector.
    pub sector: usize,
    /// Tics until the next light step.
    pub count: i32,
    /// The brightest light level.
    pub maxlight: i32,
    /// The darkest light level (darkest neighbor).
    pub minlight: i32,
    /// Max tics the flash stays bright before going dark.
    pub maxtime: i32,
    /// Max tics the flash stays dark before going bright.
    pub mintime: i32,
}

/// Vanilla `strobe_t`: the periodic strobe (sector specials 2, 3, 4, 12,
/// 13 and the triggered strobe from line special 17).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StrobeFlash {
    /// Index of the strobing sector.
    pub sector: usize,
    /// Tics until the next light step.
    pub count: i32,
    /// The dark light level (darkest neighbor, or 0 if none is darker).
    pub minlight: i32,
    /// The bright light level.
    pub maxlight: i32,
    /// Tics spent dark (`FASTDARK` or `SLOWDARK`).
    pub darktime: i32,
    /// Tics spent bright (`STROBEBRIGHT`).
    pub brighttime: i32,
}

/// Vanilla `glow_t`: the slow breathing glow (sector special 8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Glow {
    /// Index of the glowing sector.
    pub sector: usize,
    /// The dark floor.
    pub minlight: i32,
    /// The bright ceiling.
    pub maxlight: i32,
    /// -1 while dimming, +1 while brightening.
    pub direction: i32,
}

/// Vanilla `P_SpawnFireFlicker`: mark the sector as a fire flicker and
/// spawn its thinker. Clears the sector special.
pub fn spawn_fire_flicker(ctx: &mut SpecialContext, sector: usize) {
    // nothing special about it during gameplay
    ctx.map.sectors[sector].special = 0;

    let maxlight = i32::from(ctx.map.sectors[sector].light_level);
    let minlight = find_min_surrounding_light(ctx.map, sector, maxlight) + 16;

    ctx.pending_thinkers.push(Thinker::FireFlicker(FireFlicker {
        sector,
        maxlight,
        minlight,
        count: 4,
    }));
}

/// Vanilla `P_SpawnLightFlash`: spawn a broken-light flash thinker.
/// Clears the sector special.
pub fn spawn_light_flash(ctx: &mut SpecialContext, sector: usize) {
    // nothing special about it during gameplay
    ctx.map.sectors[sector].special = 0;

    let maxlight = i32::from(ctx.map.sectors[sector].light_level);
    let minlight = find_min_surrounding_light(ctx.map, sector, maxlight);

    let maxtime = 64;
    let mintime = 7;
    let count = (i32::from(ctx.rng.next()) & maxtime) + 1;

    ctx.pending_thinkers.push(Thinker::LightFlash(LightFlash {
        sector,
        count,
        maxlight,
        minlight,
        maxtime,
        mintime,
    }));
}

/// Vanilla `P_SpawnStrobeFlash`: spawn a strobe thinker.
/// Clears the sector special.
pub fn spawn_strobe_flash(ctx: &mut SpecialContext, sector: usize, dark_time: i32, in_sync: bool) {
    let maxlight = i32::from(ctx.map.sectors[sector].light_level);
    let mut minlight = find_min_surrounding_light(ctx.map, sector, maxlight);

    if minlight == maxlight {
        minlight = 0;
    }

    // nothing special about it during gameplay
    ctx.map.sectors[sector].special = 0;

    let count = if in_sync {
        1
    } else {
        (i32::from(ctx.rng.next()) & 7) + 1
    };

    ctx.pending_thinkers.push(Thinker::StrobeFlash(StrobeFlash {
        sector,
        count,
        minlight,
        maxlight,
        darktime: dark_time,
        brighttime: STROBEBRIGHT,
    }));
}

/// Vanilla `P_SpawnGlowingLight`: spawn a breathing-glow thinker.
/// Clears the sector special.
pub fn spawn_glowing_light(ctx: &mut SpecialContext, sector: usize) {
    let maxlight = i32::from(ctx.map.sectors[sector].light_level);
    let minlight = find_min_surrounding_light(ctx.map, sector, maxlight);

    // nothing special about it during gameplay
    ctx.map.sectors[sector].special = 0;

    ctx.pending_thinkers.push(Thinker::Glow(Glow {
        sector,
        minlight,
        maxlight,
        direction: -1,
    }));
}

/// Vanilla `T_FireFlicker`.
pub(crate) fn run_fire_flicker(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut flick = match reg.get(id) {
        Some(Thinker::FireFlicker(flick)) => *flick,
        _ => return,
    };

    flick.count -= 1;
    if flick.count != 0 {
        return write_back(reg, id, Thinker::FireFlicker(flick));
    }

    let amount = (i32::from(ctx.rng.next()) & 3) * 16;
    let current = i32::from(ctx.map.sectors[flick.sector].light_level);
    if current - amount < flick.minlight {
        ctx.map.sectors[flick.sector].light_level = flick.minlight as i16;
    } else {
        ctx.map.sectors[flick.sector].light_level = (flick.maxlight - amount) as i16;
    }
    flick.count = 4;

    write_back(reg, id, Thinker::FireFlicker(flick));
}

/// Vanilla `T_LightFlash`.
pub(crate) fn run_light_flash(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut flash = match reg.get(id) {
        Some(Thinker::LightFlash(flash)) => *flash,
        _ => return,
    };

    flash.count -= 1;
    if flash.count != 0 {
        return write_back(reg, id, Thinker::LightFlash(flash));
    }

    let current = i32::from(ctx.map.sectors[flash.sector].light_level);
    if current == flash.maxlight {
        ctx.map.sectors[flash.sector].light_level = flash.minlight as i16;
        flash.count = (i32::from(ctx.rng.next()) & flash.mintime) + 1;
    } else {
        ctx.map.sectors[flash.sector].light_level = flash.maxlight as i16;
        flash.count = (i32::from(ctx.rng.next()) & flash.maxtime) + 1;
    }

    write_back(reg, id, Thinker::LightFlash(flash));
}

/// Vanilla `T_StrobeFlash`.
pub(crate) fn run_strobe_flash(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut flash = match reg.get(id) {
        Some(Thinker::StrobeFlash(flash)) => *flash,
        _ => return,
    };

    flash.count -= 1;
    if flash.count != 0 {
        return write_back(reg, id, Thinker::StrobeFlash(flash));
    }

    let current = i32::from(ctx.map.sectors[flash.sector].light_level);
    if current == flash.minlight {
        ctx.map.sectors[flash.sector].light_level = flash.maxlight as i16;
        flash.count = flash.brighttime;
    } else {
        ctx.map.sectors[flash.sector].light_level = flash.minlight as i16;
        flash.count = flash.darktime;
    }

    write_back(reg, id, Thinker::StrobeFlash(flash));
}

/// Vanilla `T_Glow`.
pub(crate) fn run_glow(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut glow = match reg.get(id) {
        Some(Thinker::Glow(glow)) => *glow,
        _ => return,
    };

    match glow.direction {
        -1 => {
            // DOWN
            let current = i32::from(ctx.map.sectors[glow.sector].light_level) - GLOWSPEED;
            ctx.map.sectors[glow.sector].light_level = current as i16;
            if current <= glow.minlight {
                ctx.map.sectors[glow.sector].light_level += GLOWSPEED as i16;
                glow.direction = 1;
            }
        }
        1 => {
            // UP
            let current = i32::from(ctx.map.sectors[glow.sector].light_level) + GLOWSPEED;
            ctx.map.sectors[glow.sector].light_level = current as i16;
            if current >= glow.maxlight {
                ctx.map.sectors[glow.sector].light_level -= GLOWSPEED as i16;
                glow.direction = -1;
            }
        }
        _ => {}
    }

    write_back(reg, id, Thinker::Glow(glow));
}

/// Vanilla `ev_start_light_strobing`: spawn a slow non-sync strobe in every
/// sector tagged like the line. Sectors with an active mover are skipped,
/// like vanilla's `specialdata` check.
pub fn ev_start_light_strobing(ctx: &mut SpecialContext, line: usize) {
    let tag = ctx.map.linedefs[line].tag;
    let mut cursor = None;
    while let Some(sector) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(sector);
        if sector_busy(ctx.store, &ctx.pending_thinkers, sector) {
            continue;
        }
        spawn_strobe_flash(ctx, sector, SLOWDARK, false);
    }
}

/// Vanilla `ev_turn_tag_lights_off`: set every sector tagged like the line
/// to the darkest light level among its neighbors (its own level if it
/// stands alone).
pub fn ev_turn_tag_lights_off(ctx: &mut SpecialContext, line: usize) {
    let tag = ctx.map.linedefs[line].tag;
    for sector in 0..ctx.map.sectors.len() {
        if ctx.map.sectors[sector].tag != tag {
            continue;
        }
        let mut min = i32::from(ctx.map.sectors[sector].light_level);
        for index in sector_lines(ctx.map, sector) {
            if let Some(other) = get_next_sector(ctx.map, index, sector) {
                let light = i32::from(ctx.map.sectors[other].light_level);
                if light < min {
                    min = light;
                }
            }
        }
        ctx.map.sectors[sector].light_level = min as i16;
    }
}

/// Vanilla `ev_light_turn_on`. `bright == 0` searches for the brightest
/// surrounding sector instead.
///
/// The search mutates `bright` and, like vanilla, does not reset it
/// between tagged sectors — the first sector's result leaks into later
/// ones, and a search that finds nothing leaves the sector black. Kept
/// 1:1 with the original on purpose.
pub fn ev_light_turn_on(ctx: &mut SpecialContext, line: usize, bright: i32) {
    let tag = ctx.map.linedefs[line].tag;
    let mut bright = bright;
    for sector in 0..ctx.map.sectors.len() {
        if ctx.map.sectors[sector].tag != tag {
            continue;
        }
        // bright = 0 means search for the highest light level
        // surrounding the sector
        if bright == 0 {
            for index in sector_lines(ctx.map, sector) {
                if let Some(other) = get_next_sector(ctx.map, index, sector) {
                    let light = i32::from(ctx.map.sectors[other].light_level);
                    if light > bright {
                        bright = light;
                    }
                }
            }
        }
        ctx.map.sectors[sector].light_level = bright as i16;
    }
}

fn write_back(reg: &mut ThinkerRegistry, id: ThinkerId, thinker: Thinker) {
    if let Some(slot) = reg.get_mut(id) {
        *slot = thinker;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{split_map, test_pass, test_player};
    use crate::specials::{DoorKind, SpecialContext, VerticalDoor, run_thinkers};

    fn ctx<'a>(
        w: &'a mut (
            crate::map::Map,
            MobjStore,
            Vec<crate::player::Player>,
            DoomRng,
        ),
    ) -> SpecialContext<'a> {
        SpecialContext::new(&mut w.0, &mut w.1, &mut w.2, &mut w.3)
    }

    fn world() -> (
        crate::map::Map,
        MobjStore,
        Vec<crate::player::Player>,
        DoomRng,
    ) {
        (
            split_map(0, 128, 5),
            MobjStore::new(),
            vec![test_player()],
            DoomRng::with_seed(0),
        )
    }

    #[test]
    fn spawn_light_flash_clears_special_and_registers_thinker() {
        let mut w = world();
        w.0.sectors[1].special = 1; // flickering lights
        w.0.sectors[1].light_level = 200;
        w.0.sectors[0].light_level = 176;

        let mut ctx = ctx(&mut w);
        spawn_light_flash(&mut ctx, 1);
        assert_eq!(ctx.map.sectors[1].special, 0);
        assert_eq!(ctx.pending_thinkers.len(), 1);

        run_thinkers(&mut ctx);
        let found = ctx
            .store
            .thinkers()
            .any(|(_, t)| matches!(t, Thinker::LightFlash(f) if f.sector == 1 && f.maxlight == 200 && f.minlight == 176));
        assert!(found, "light flash thinker must be registered");
    }

    #[test]
    fn spawn_fire_flicker_uses_surrounding_min_plus_16() {
        let mut w = world();
        w.0.sectors[1].special = 17;
        w.0.sectors[1].light_level = 200;
        w.0.sectors[0].light_level = 100;

        let mut ctx = ctx(&mut w);
        spawn_fire_flicker(&mut ctx, 1);
        assert_eq!(ctx.map.sectors[1].special, 0);
        let Thinker::FireFlicker(flick) = ctx.pending_thinkers[0] else {
            panic!("expected a fire flicker thinker");
        };
        assert_eq!(flick.maxlight, 200);
        assert_eq!(flick.minlight, 116, "darkest neighbor (100) + 16");
    }

    #[test]
    fn light_flash_toggles_and_resets_count() {
        let mut map = split_map(0, 128, 5);
        map.sectors[1].light_level = 255;
        let mut rng = DoomRng::with_seed(0);
        let mut events = Vec::new();
        let mut pass = test_pass(&mut map, &mut rng, &mut events);

        let mut reg = ThinkerRegistry::new();
        let id = reg.add(Thinker::LightFlash(LightFlash {
            sector: 1,
            count: 1, // fires immediately
            maxlight: 255,
            minlight: 35,
            maxtime: 64,
            mintime: 7,
        }));
        run_light_flash(&mut reg, id, &mut pass);
        assert_eq!(pass.map.sectors[1].light_level, 35, "bright -> dark");
        let flash = match reg.get(id) {
            Some(Thinker::LightFlash(f)) => *f,
            _ => panic!("thinker vanished"),
        };
        assert!((1..=8).contains(&flash.count), "dark count is (rng&7)+1");

        // it fires again when the count expires: dark -> bright
        let mut reg2 = ThinkerRegistry::new();
        let id2 = reg2.add(Thinker::LightFlash(LightFlash {
            sector: 1,
            count: 1,
            maxlight: 255,
            minlight: 35,
            maxtime: 64,
            mintime: 7,
        }));
        pass.map.sectors[1].light_level = 35;
        run_light_flash(&mut reg2, id2, &mut pass);
        assert_eq!(pass.map.sectors[1].light_level, 255, "dark -> bright");
        let flash = match reg2.get(id2) {
            Some(Thinker::LightFlash(f)) => *f,
            _ => panic!("thinker vanished"),
        };
        assert!(
            (1..=65).contains(&flash.count),
            "bright count is (rng&63)+1"
        );
    }

    #[test]
    fn strobe_toggles_between_min_and_max_with_fixed_times() {
        let mut map = split_map(0, 128, 5);
        map.sectors[1].light_level = 35;
        let mut rng = DoomRng::with_seed(0);
        let mut events = Vec::new();
        let mut pass = test_pass(&mut map, &mut rng, &mut events);

        let mut reg = ThinkerRegistry::new();
        let id = reg.add(Thinker::StrobeFlash(StrobeFlash {
            sector: 1,
            count: 1,
            minlight: 35,
            maxlight: 255,
            darktime: SLOWDARK,
            brighttime: STROBEBRIGHT,
        }));
        run_strobe_flash(&mut reg, id, &mut pass);
        assert_eq!(pass.map.sectors[1].light_level, 255, "dark -> bright");
        let flash = match reg.get(id) {
            Some(Thinker::StrobeFlash(f)) => *f,
            _ => panic!("thinker vanished"),
        };
        assert_eq!(flash.count, STROBEBRIGHT);

        // the next toggle happens when the countdown runs out
        let mut reg2 = ThinkerRegistry::new();
        let id2 = reg2.add(Thinker::StrobeFlash(StrobeFlash {
            sector: 1,
            count: 1,
            minlight: 35,
            maxlight: 255,
            darktime: SLOWDARK,
            brighttime: STROBEBRIGHT,
        }));
        run_strobe_flash(&mut reg2, id2, &mut pass);
        assert_eq!(pass.map.sectors[1].light_level, 35, "bright -> dark");
        let flash = match reg2.get(id2) {
            Some(Thinker::StrobeFlash(f)) => *f,
            _ => panic!("thinker vanished"),
        };
        assert_eq!(flash.count, SLOWDARK);
    }

    #[test]
    fn glow_breathes_between_bounds_in_glowspeed_steps() {
        let mut map = split_map(0, 128, 5);
        map.sectors[1].light_level = 40;
        let mut rng = DoomRng::with_seed(0);
        let mut events = Vec::new();
        let mut pass = test_pass(&mut map, &mut rng, &mut events);

        let mut reg = ThinkerRegistry::new();
        let id = reg.add(Thinker::Glow(Glow {
            sector: 1,
            minlight: 35,
            maxlight: 255,
            direction: -1,
        }));
        run_glow(&mut reg, id, &mut pass);
        // 40 - 8 = 32 <= minlight -> clamped back up, direction flips
        assert_eq!(pass.map.sectors[1].light_level, 40);
        let glow = match reg.get(id) {
            Some(Thinker::Glow(g)) => *g,
            _ => panic!("thinker vanished"),
        };
        assert_eq!(glow.direction, 1);

        // and dimming back down again after reaching the top later
        pass.map.sectors[1].light_level = 255;
        let mut glow = match reg.get(id) {
            Some(Thinker::Glow(g)) => *g,
            _ => panic!("thinker vanished"),
        };
        glow.direction = 1;
        if let Some(slot) = reg.get_mut(id) {
            *slot = Thinker::Glow(glow);
        }
        run_glow(&mut reg, id, &mut pass);
        // 255 + 8 > maxlight -> step back down and flip
        assert_eq!(pass.map.sectors[1].light_level, 255);
        let glow = match reg.get(id) {
            Some(Thinker::Glow(g)) => *g,
            _ => panic!("thinker vanished"),
        };
        assert_eq!(glow.direction, -1);
    }

    #[test]
    fn turn_tag_lights_off_sets_the_neighbor_minimum() {
        let mut w = world();
        w.0.sectors[1].tag = 42;
        w.0.sectors[1].light_level = 200;
        w.0.sectors[0].light_level = 100;

        let mut ctx = ctx(&mut w);
        ctx.map.linedefs[0].tag = 42; // any line with the tag
        ev_turn_tag_lights_off(&mut ctx, 0);
        assert_eq!(ctx.map.sectors[1].light_level, 100);
    }

    #[test]
    fn light_turn_on_searches_surrounding_brightness() {
        let mut w = world();
        w.0.sectors[1].tag = 42;
        w.0.sectors[1].light_level = 35;
        w.0.sectors[0].light_level = 200;

        let mut ctx = ctx(&mut w);
        ctx.map.linedefs[2].tag = 42; // north wall
        // bright == 0: search for the brightest neighbor
        ev_light_turn_on(&mut ctx, 2, 0);
        assert_eq!(ctx.map.sectors[1].light_level, 200);

        // explicit brightness wins over the search
        ctx.map.sectors[1].light_level = 35;
        ev_light_turn_on(&mut ctx, 2, 255);
        assert_eq!(ctx.map.sectors[1].light_level, 255);
    }

    #[test]
    fn light_turn_on_with_no_bright_neighbor_goes_dark() {
        // vanilla quirk: a found-nothing search leaves bright == 0,
        // so the sector lights go black
        let mut w = world();
        w.0.sectors[1].tag = 42;
        w.0.sectors[1].light_level = 200;
        w.0.sectors[0].light_level = 0;

        let mut ctx = ctx(&mut w);
        ctx.map.linedefs[2].tag = 42;
        ev_light_turn_on(&mut ctx, 2, 0);
        assert_eq!(
            ctx.map.sectors[1].light_level, 0,
            "vanilla sets 0 here (quirk)"
        );
    }

    #[test]
    fn start_light_strobing_skips_sectors_with_a_door_queued() {
        let mut w = world();
        w.0.sectors[0].tag = 42;
        w.0.sectors[1].tag = 42;

        let mut ctx = ctx(&mut w);
        ctx.map.linedefs[2].tag = 42; // the tag both sectors carry
        // a door queued for sector 1 this tic
        ctx.pending_thinkers.push(Thinker::Door(VerticalDoor {
            kind: DoorKind::Normal,
            sector: 1,
            topheight: crate::fixed::Fixed::ZERO,
            speed: crate::fixed::Fixed::ONE,
            direction: 1,
            topwait: 150,
            topcountdown: 0,
        }));

        ev_start_light_strobing(&mut ctx, 2);

        // sector 0 gets the strobe, sector 1 (busy) does not
        let strobes: Vec<usize> = ctx
            .pending_thinkers
            .iter()
            .filter_map(|t| match t {
                Thinker::StrobeFlash(s) => Some(s.sector),
                _ => None,
            })
            .collect();
        assert_eq!(strobes, vec![0]);
    }
}

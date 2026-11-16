//! The monster noise alert: vanilla's `P_NoiseAlert` + `P_RecursiveSound`
//! (p_enemy.c).
//!
//! When the player fires, `A_Look` monsters in earshot get woken through
//! the *sector* `soundtarget`: a BFS/DFS flood from the shooter's sector
//! across every two-sided line that is not fully closed, fanning out
//! through open space and through `ML_SOUNDBLOCK` lines at most one deep.
//! Each visited sector records the emitter's latest sound source; the
//! actual wake-up happens lazily in `A_Look`.
//!
//! The flood is kept off `Map` (it is immutable) by storing the per-sector
//! `soundtarget` here, next to the per-call scratch. The depth map
//! (`soundtraversed` + `validcount` in the original) is likewise a local
//! per-`P_NoiseAlert` map instead of sector stamps — the original reuses
//! its global `validcount` between subsystems, which never happens here.
//!
//! `P_RecursiveSound` walks a sector's line list; our `Map` has no
//! per-sector line index, so one is built per alert (alerts are rare — a
//! flood is cheaper than the repeated scans it prevents).
//!
//! Depth rule, faithfully: a sector can be re-entered when this path has
//! not visited it yet *or* reaches it with a higher remaining block budget
//! (soundtraversed must be `> soundblocks + 1`). The flood always keeps the
//! smallest depth a sector has been reached at, matching the original.

use crate::map::Map;
use crate::map::types::{ML_SOUNDBLOCK, ML_TWOSIDED};
use crate::mobj::MobjStore;
use crate::thinker::ThinkerId;
use std::collections::HashMap;

/// The sector `soundtarget` table. Own one per level; share it across all
/// actors of the level (gameplay layer, like the original's sectors).
#[derive(Debug, Default)]
pub struct SoundFlood {
    /// Per-sector sound source (vanilla `sector_t::soundtarget`).
    targets: Vec<Option<ThinkerId>>,
}

impl SoundFlood {
    /// An empty flood for a level with `sector_count` sectors.
    pub fn new(sector_count: usize) -> Self {
        SoundFlood {
            targets: vec![None; sector_count],
        }
    }

    /// Number of sectors tracked.
    pub fn sector_count(&self) -> usize {
        self.targets.len()
    }

    /// The sound source a monster standing in `sector` would hear, or
    /// `None`. Vanilla `sector->soundtarget`.
    pub fn sound_target(&self, sector: usize) -> Option<ThinkerId> {
        self.targets.get(sector).copied().flatten()
    }

    /// Clear every sector's sound target (vanilla level re-init).
    pub fn clear(&mut self) {
        self.targets.fill(None);
    }

    fn ensure_sized(&mut self, sector_count: usize) {
        if self.targets.len() != sector_count {
            self.targets.resize(sector_count, None);
        }
    }
}

/// Vanilla `P_NoiseAlert`.
///
/// Records `target` as the sound source for every sector reachable from
/// `emitter`'s sector through openings, subject to the sound-block rules.
/// `emitter` is the thing making the noise (usually the player's avatar).
/// Returns `false` when the emitter has no usable sector (vanilla would
/// walk through `NULL`).
pub fn noise_alert(
    map: &Map,
    store: &MobjStore,
    flood: &mut SoundFlood,
    target: ThinkerId,
    emitter: ThinkerId,
) -> bool {
    let Some(emitter) = store.mobj(emitter) else {
        tracing::warn!("P_NoiseAlert: stale emitter handle");
        return false;
    };
    let Some(sector) = emitter.sector else {
        tracing::warn!("P_NoiseAlert: emitter in a degenerate subsector");
        return false;
    };

    flood.ensure_sized(map.sectors.len());

    // sector -> lines index, rebuilt per alert; alerts are rare
    let mut sector_lines: Vec<Vec<usize>> = vec![Vec::new(); map.sectors.len()];
    for (index, linedef) in map.linedefs.iter().enumerate() {
        for side_index in linedef.sidenum.iter().flatten().copied() {
            let sidedef = &map.sidedefs[side_index];
            sector_lines[sidedef.sector].push(index);
        }
    }

    // vanilla soundtarget is private global state; the flood is one call
    let mut traversed: HashMap<usize, u32> = HashMap::new();
    recursive_sound(map, flood, &sector_lines, &mut traversed, target, sector, 0);
    true
}

/// Vanilla `P_RecursiveSound`, iterative recursion in spirit (kept
/// recursive like the original, the depth is bounded by the number of
/// sectors).
#[allow(clippy::too_many_arguments)]
fn recursive_sound(
    map: &Map,
    flood: &mut SoundFlood,
    sector_lines: &[Vec<usize>],
    traversed: &mut HashMap<usize, u32>,
    target: ThinkerId,
    sec: usize,
    soundblocks: u32,
) {
    let depth = soundblocks + 1;
    // already flooded at this or a shallower depth?
    if let Some(&seen) = traversed.get(&sec) {
        if seen <= depth {
            return; // already flooded
        }
    }
    traversed.insert(sec, depth);
    flood.targets[sec] = Some(target);

    for &line_index in &sector_lines[sec] {
        let linedef = &map.linedefs[line_index];
        if linedef.flags & ML_TWOSIDED == 0 {
            continue;
        }

        // P_LineOpening: only fully open lines let the sound through
        let opening = crate::movement::line_opening(map, line_index);
        if opening.range.to_raw() <= 0 {
            continue; // closed door
        }

        // sides[sidenum[0]].sector == sec ? other = sidenum[1] : other = sidenum[0]
        let side0 = linedef.sidenum[0].map(|i| map.sidedefs[i].sector);
        let side1 = linedef.sidenum[1].map(|i| map.sidedefs[i].sector);
        let other = match (side0, side1) {
            (Some(front), Some(back)) => {
                if front == sec {
                    back
                } else {
                    front
                }
            }
            // two-sided line with a missing sidedef can't happen after load
            _ => continue,
        };

        if linedef.flags & ML_SOUNDBLOCK != 0 {
            if soundblocks == 0 {
                recursive_sound(map, flood, sector_lines, traversed, target, other, 1);
            }
        } else {
            recursive_sound(
                map,
                flood,
                sector_lines,
                traversed,
                target,
                other,
                soundblocks,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::testutil::{open_map, players, spawn_player, split_map};
    use crate::map::types::{ML_BLOCKING, ML_SOUNDBLOCK, ML_TWOSIDED};
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    #[test]
    fn noise_alert_floods_open_areas() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let mut flood = SoundFlood::new(map.sectors.len());

        assert!(noise_alert(&map, &store, &mut flood, player, player));
        assert_eq!(flood.sound_target(0), Some(player));
        assert_eq!(flood.sound_target(1), Some(player));
    }

    #[test]
    fn closed_door_blocks_the_flood() {
        // north floor 0 ceiling 0: fully closed divider
        let map = split_map(0, 0, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let mut flood = SoundFlood::new(map.sectors.len());

        assert!(noise_alert(&map, &store, &mut flood, player, player));
        assert_eq!(flood.sound_target(0), Some(player));
        assert_eq!(flood.sound_target(1), None);
    }

    #[test]
    fn one_sided_wall_blocks_the_flood() {
        let map = split_map(0, 128, ML_BLOCKING, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let mut flood = SoundFlood::new(map.sectors.len());

        assert!(noise_alert(&map, &store, &mut flood, player, player));
        assert_eq!(flood.sound_target(0), Some(player));
        assert_eq!(flood.sound_target(1), None);
    }

    #[test]
    fn soundblock_line_passes_one_deep() {
        // an ML_SOUNDBLOCK divider still passes sound one level deep, it
        // just doesn't propagate beyond it (our fixture has two sectors, so
        // the neighbour still hears it)
        let map = split_map(0, 128, ML_TWOSIDED | ML_SOUNDBLOCK, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let mut flood = SoundFlood::new(map.sectors.len());

        assert!(noise_alert(&map, &store, &mut flood, player, player));
        assert_eq!(flood.sound_target(0), Some(player));
        assert_eq!(flood.sound_target(1), Some(player));
    }

    #[test]
    fn sound_target_reports_none_when_never_flooded() {
        let map = open_map();
        let flood = SoundFlood::new(map.sectors.len());
        assert_eq!(flood.sound_target(0), None);
        assert_eq!(flood.sector_count(), 2);
    }

    #[test]
    fn clear_wipes_the_flood() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let mut flood = SoundFlood::new(map.sectors.len());

        noise_alert(&map, &store, &mut flood, player, player);
        assert_eq!(flood.sound_target(0), Some(player));
        flood.clear();
        assert_eq!(flood.sound_target(0), None);
        assert_eq!(flood.sound_target(1), None);
    }
}

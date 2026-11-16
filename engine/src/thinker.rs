//! The thinker registry: the vanilla THINKERS list from `p_tick.c` and
//! `d_think.h`, re-engineered as a flat arena with generational handles.
//!
//! ## Vanilla semantics (kept exactly)
//!
//! The original engine keeps a doubly linked circular list with a sentinel
//! node (`thinkercap`). The rules that matter for gameplay:
//!
//! - `P_AddThinker` appends at the **tail** of the list. `P_RunThinkers`
//!   walks head to tail, so thinkers execute in **insertion order**.
//! - `P_RemoveThinker` does not unlink anything; it just poisons the
//!   thinker's function pointer (`(actionf_v)-1`). The actual unlink happens
//!   lazily, at the thinker's turn in `P_RunThinkers`.
//! - A thinker flagged for removal **before its turn** is unlinked and
//!   never executed. A thinker flagged **after** its turn (by a later
//!   thinker in the same pass) stays in the list until the next pass.
//! - `P_AddThinker` called by a thinker during its own `P_RunThinkers`
//!   pass appends at the tail, so the new thinker **is executed in the
//!   same pass**.
//!
//! ## What the registry does differently
//!
//! No `Z_Malloc`/`Z_Free`, no function pointers, no raw list surgery.
//! Thinkers live in a dense slot arena keyed by a generational [`crate::thinker::ThinkerId`]
//! (index + generation, like slotmap). Stale handles can never alias a
//! recycled slot: freeing bumps the slot's generation. The vanilla
//! "list" is an explicit `order` vector holding the live handles; removing
//! a thinker is a `Vec::remove` from that vector, which is `O(n)` - fine at
//! DOOM's scale (tens of thinkers per level) and free of unsafe pointer
//! fixups.
//!
//! Removal goes through [`crate::thinker::ThinkerRegistry::remove`] (the vanilla lazy
//! flag) or is swept inside [`crate::thinker::ThinkerRegistry::run`] at the flagged
//! thinker's turn - exactly like the original unlinking in `P_RunThinkers`.
//! A thinker that flags itself for removal is dropped right after its own
//! turn, not re-executed.
//!
//! ## Heterogeneous thinkers
//!
//! Vanilla keeps mobjs, plats, doors, crushers and light strobing in one
//! list, and the interleaving of those kinds is observable (a missile and
//! a door think in the same tick, in list order). To preserve that
//! forever, thinkers are stored as the [`crate::thinker::Thinker`] enum and new kinds are
//! added as variants; there is exactly one ordered list.

use std::fmt;

use crate::mobj::Mobj;
use crate::specials::{
    Ceiling, FireFlicker, FloorMover, Glow, LightFlash, Plat, StrobeFlash, VerticalDoor,
};

/// A handle into the registry, like a `slotmap` key: a slot index plus
/// the generation that slot had when the handle was handed out.
///
/// Handles are cheap to copy and compare. A handle that refers to a
/// thinker removed and swept since it was issued is *stale*: all registry
/// operations on it treat it as absent instead of aliasing the slot that
/// may have been recycled for someone else.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThinkerId {
    index: u32,
    generation: u32,
}

impl ThinkerId {
    /// Arena slot index (number of allocations ever made for this slot,
    /// capped at 2^32 - 1 before the arena would need to grow past that).
    pub fn index(self) -> usize {
        self.index as usize
    }

    /// Generation the slot had when this handle was handed out.
    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl fmt::Debug for ThinkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ThinkerId({}#{})", self.index, self.generation)
    }
}

/// One thinker in the list.
///
/// Every variant is a plain data payload; behavior lives in the gameplay
/// layer, not on the thinker itself (the registry is just the list).
/// New thinker kinds are added here as the corresponding `p_*.c` jobs get
/// ported, so the list stays a single interleaved sequence like vanilla.
#[derive(Debug)]
pub enum Thinker {
    /// Vanilla `P_MobjThinker`: the per-tic behavior of every gameplay
    /// object (monsters, projectiles, items, the player's avatar).
    Mobj(Mobj),
    /// Vanilla `T_VerticalDoor` (`p_doors.c`): doors that slide up and
    /// down (raised, opened, closed, or close-after-30-seconds ones).
    Door(VerticalDoor),
    /// Vanilla `T_LightFlash` (`p_lights.c`): the "broken light" flicker
    /// used by sector special 1.
    LightFlash(LightFlash),
    /// Vanilla `T_StrobeFlash` (`p_lights.c`): periodic strobe lights
    /// (sector specials 2, 3, 4, 12, 13 and the tagged-line trigger).
    StrobeFlash(StrobeFlash),
    /// Vanilla `T_FireFlicker` (`p_lights.c`): the torch-style fire
    /// flicker (sector special 17).
    FireFlicker(FireFlicker),
    /// Vanilla `T_Glow` (`p_lights.c`): the slow breathing glow
    /// (sector special 8).
    Glow(Glow),
    /// Vanilla `T_MoveFloor` (`p_floor.c`): floors moving up/down
    /// (raise/lower specials, stairs, donuts).
    FloorMover(FloorMover),
    /// Vanilla `T_PlatRaise` (`p_plats.c`): elevator platforms going
    /// up/down, perpetual raise, stasis on command.
    Plat(Plat),
    /// Vanilla `T_MoveCeiling` (`p_ceilng.c`): ceiling crushers and
    /// one-way ceiling movers.
    Ceiling(Ceiling),
}

struct Slot {
    generation: u32,
    value: Option<Thinker>,
    /// The vanilla `function.acv == (actionf_v)(-1)` poison flag: set by
    /// [`crate::thinker::ThinkerRegistry::remove`], acted on at the thinker's turn.
    removed: bool,
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("generation", &self.generation)
            .field("occupied", &self.value.is_some())
            .field("removed", &self.removed)
            .finish()
    }
}

/// The vanilla THINKERS list.
///
/// Thinkers are added with [`ThinkerRegistry::add`] (appends at the end,
/// like `P_AddThinker`), removed with [`crate::thinker::ThinkerRegistry::remove`] (the
/// lazy `-1` flag), executed with [`crate::thinker::ThinkerRegistry::run`] (like
/// `P_RunThinkers`) and swept lazily at their own turn.
#[derive(Debug, Default)]
pub struct ThinkerRegistry {
    slots: Vec<Slot>,
    /// The "list": handles in execution (insertion) order.
    order: Vec<ThinkerId>,
    /// Indices of vacant slots available for reuse (stack).
    free: Vec<u32>,
}

impl ThinkerRegistry {
    /// Creates an empty registry (vanilla `P_InitThinkers`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a thinker at the end of the list and returns its handle.
    ///
    /// Mirrors `P_AddThinker`, which appends at the tail; execution order
    /// in [`crate::thinker::ThinkerRegistry::run`] is therefore insertion order.
    pub fn add(&mut self, thinker: Thinker) -> ThinkerId {
        let index = match self.free.pop() {
            Some(index) => {
                let slot = &mut self.slots[index as usize];
                slot.value = Some(thinker);
                slot.removed = false;
                index
            }
            None => {
                self.slots.push(Slot {
                    generation: 0,
                    value: Some(thinker),
                    removed: false,
                });
                (self.slots.len() - 1) as u32
            }
        };
        let id = ThinkerId {
            index,
            generation: self.slots[index as usize].generation,
        };
        self.order.push(id);
        id
    }

    /// Number of thinkers in the list, including those flagged for removal
    /// but not yet collected (vanilla keeps those linked until the next
    /// `P_RunThinkers` too).
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True when no thinkers are in the list.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// True when `id` refers to a thinker currently allocated in this
    /// registry (possibly flagged for removal; see [`ThinkerRegistry::is_removed`]).
    pub fn is_valid(&self, id: ThinkerId) -> bool {
        match self.slots.get(id.index as usize) {
            Some(slot) => slot.generation == id.generation && slot.value.is_some(),
            None => false,
        }
    }

    /// True when `id` is valid and has been flagged for removal but not
    /// yet collected (the zombie state vanilla leaves between
    /// `P_RemoveThinker` and the next `P_RunThinkers`).
    pub fn is_removed(&self, id: ThinkerId) -> bool {
        self.is_valid(id) && self.slots[id.index as usize].removed
    }

    /// Borrows a thinker. Flagged-for-removal thinkers are unobservable,
    /// like vanilla's `-1` function pointer, and come back as `None`.
    pub fn get(&self, id: ThinkerId) -> Option<&Thinker> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation || slot.removed {
            return None;
        }
        slot.value.as_ref()
    }

    /// Mutably borrows a thinker. Flagged-for-removal thinkers come back
    /// as `None`, same as [`ThinkerRegistry::get`].
    pub fn get_mut(&mut self, id: ThinkerId) -> Option<&mut Thinker> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation || slot.removed {
            return None;
        }
        slot.value.as_mut()
    }

    /// Flags a thinker for removal (vanilla `P_RemoveThinker`).
    ///
    /// Nothing is unlinked or freed here; the thinker is collected at its
    /// turn in the next [`crate::thinker::ThinkerRegistry::run`] call. Unknown or stale
    /// handles are ignored with a warning instead of corrupting memory.
    pub fn remove(&mut self, id: ThinkerId) {
        match self.slots.get_mut(id.index as usize) {
            Some(slot) if slot.generation == id.generation => {
                if slot.value.is_some() && !slot.removed {
                    slot.removed = true;
                }
            }
            _ => tracing::warn!("P_RemoveThinker: stale thinker handle {id:?} ignored"),
        }
    }

    /// Drops every thinker and resets the registry (a level switch).
    ///
    /// All handles issued before the call are permanently invalidated by
    /// bumping every slot's generation, so a stale handle can never
    /// alias a slot recycled after the reset.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.generation = slot.generation.wrapping_add(1);
            slot.value = None;
            slot.removed = false;
        }
        // Every slot is vacant now, so reuse all of them instead of
        // growing a fresh arena on the next level.
        self.free = (0..self.slots.len() as u32).rev().collect();
        self.order.clear();
    }

    /// Iterates the list in execution order, skipping flagged-for-removal
    /// thinkers. Handy for render passes and collision queries.
    pub fn iter(&self) -> impl Iterator<Item = (ThinkerId, &Thinker)> + '_ {
        self.order.iter().filter_map(|&id| {
            let slot = &self.slots[id.index as usize];
            match slot.value.as_ref() {
                Some(value) if !slot.removed => Some((id, value)),
                _ => None,
            }
        })
    }

    /// Executes every thinker in insertion order (vanilla `P_RunThinkers`).
    ///
    /// The callback receives the registry (so thinkers can spawn/remove
    /// each other, like vanilla's global state) and the thinker being
    /// run. Semantics match the original pass:
    ///
    /// - a thinker flagged before its turn is unlinked and **not** run;
    /// - a thinker that flags itself while running is collected right
    ///   after its turn instead of next pass;
    /// - thinkers added during the pass are run in the same pass, after
    ///   the already-queued ones (they were appended at the tail).
    ///
    /// Calling [`ThinkerRegistry::clear`] from inside the callback tears
    /// the list down; the pass stops instead of iterating freed slots.
    /// Calling `remove` on a *later* thinker just flags it; calling it on
    /// an *earlier* one takes effect on the next pass, like vanilla.
    pub fn run(&mut self, mut f: impl FnMut(&mut ThinkerRegistry, ThinkerId)) {
        let mut i = 0;
        while i < self.order.len() {
            let id = self.order[i];
            if self.slots[id.index as usize].removed {
                self.unlink_at(i);
                continue;
            }
            f(self, id);
            if self.order.get(i) == Some(&id) {
                if self.slots[id.index as usize].removed {
                    self.unlink_at(i);
                } else {
                    i += 1;
                }
            } else {
                // The registry was cleared mid-pass; nothing sane to
                // resume, so stop instead of stepping on recycled slots.
                break;
            }
        }
    }

    /// Unlinks the thinker at `order` position `order_pos` and frees its
    /// slot (bumping the generation). The callback of `run` never sees
    /// the unlinked thinker again.
    fn unlink_at(&mut self, order_pos: usize) {
        let id = self.order.remove(order_pos);
        let slot = &mut self.slots[id.index as usize];
        slot.value = None;
        slot.removed = false;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mobj_id_at(reg: &ThinkerRegistry, index: usize) -> Option<ThinkerId> {
        reg.iter()
            .find(|(id, _)| id.index() == index)
            .map(|(id, _)| id)
    }

    #[test]
    fn thinkers_execute_in_insertion_order() {
        // vanilla: P_AddThinker appends at the tail, P_RunThinkers walks
        // head to tail -> execution order is insertion order.
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let c = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));

        let mut executed = Vec::new();
        reg.run(|_, id| executed.push(id));

        assert_eq!(executed, vec![a, b, c]);
    }

    #[test]
    fn flagged_thinker_does_not_execute() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let c = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));

        reg.remove(b);
        // vanilla keeps the -1 thinker linked until its own turn.
        assert_eq!(reg.len(), 3);

        let mut executed = Vec::new();
        reg.run(|_, id| executed.push(id));

        assert_eq!(executed, vec![a, c]);
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_valid(b));
        assert!(reg.get(b).is_none());
    }

    #[test]
    fn thinker_removed_by_an_earlier_thinker_is_skipped_in_the_same_pass() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let c = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));

        let mut executed = Vec::new();
        reg.run(|reg, id| {
            executed.push(id);
            if id == a {
                // A kills B before B's turn in the same pass.
                reg.remove(b);
            }
        });

        assert_eq!(executed, vec![a, c]);
        assert_eq!(reg.len(), 2);
        assert!(reg.is_valid(a));
        assert!(!reg.is_valid(b));
        assert!(reg.is_valid(c));
    }

    #[test]
    fn thinker_removed_by_a_later_thinker_waits_for_the_next_pass() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));

        let mut executed = Vec::new();
        reg.run(|reg, id| {
            executed.push(id);
            if id == b {
                // B flags A *after* A's turn; vanilla keeps it until the
                // next P_RunThinkers.
                reg.remove(a);
            }
        });
        assert_eq!(executed, vec![a, b]);
        // A is a zombie: still linked, unobservable via get().
        assert_eq!(reg.len(), 2);
        assert!(reg.get(a).is_none());
        assert!(reg.is_valid(a));
        assert!(reg.is_removed(a));

        let mut executed = Vec::new();
        reg.run(|_, id| executed.push(id));
        assert_eq!(executed, vec![b]);
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_valid(a));
    }

    #[test]
    fn thinker_that_removes_itself_is_collected_right_after_its_turn() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));

        let mut executed = Vec::new();
        reg.run(|reg, id| {
            executed.push(id);
            if id == a {
                reg.remove(a);
            }
        });

        assert_eq!(executed, vec![a, b]);
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_valid(a));
        assert!(reg.get(a).is_none());
    }

    #[test]
    fn thinker_added_during_a_pass_runs_in_the_same_pass_at_the_tail() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));

        let mut executed = Vec::new();
        reg.run(|reg, id| {
            executed.push(id);
            if id == a {
                let c = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
                // The newly added thinker is appended at the tail and runs
                // in this same pass, after everything already queued.
                assert_eq!(executed, vec![a]);
                let _ = c;
            }
        });

        // a executed first, then b, then the thinker a added.
        assert_eq!(executed.len(), 3);
        assert_eq!(executed[0], a);
        assert_eq!(executed[1], b);
        assert_eq!(executed[2].generation(), 0);
        assert_ne!(executed[2], a);
        assert_ne!(executed[2], b);
    }

    #[test]
    fn stale_handles_do_not_alias_recycled_slots() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        reg.remove(a);
        reg.run(|_, _| {}); // sweep a

        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        assert_eq!(b.index(), a.index(), "slot should be recycled");
        assert_ne!(b.generation(), a.generation(), "generation must bump");

        // The stale handle does not see the new occupant.
        assert!(!reg.is_valid(a));
        assert!(reg.get(a).is_none());
        assert!(reg.is_valid(b));
    }

    #[test]
    fn interleaving_removals_during_the_pass() {
        let mut reg = ThinkerRegistry::new();
        let ids: Vec<_> = (0..6)
            .map(|_| reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture())))
            .collect();

        let mut executed = Vec::new();
        reg.run(|reg, id| {
            executed.push(id);
            match executed.len() {
                // after 1st: remove #3 (not yet run)
                1 => reg.remove(ids[3]),
                // after 3rd (ids[2] ran): flag ids[1], an already-executed
                // thinker. It stays linked as a zombie and is swept on the
                // next pass, like vanilla.
                3 => reg.remove(ids[1]),
                _ => {}
            }
        });

        assert_eq!(executed, vec![ids[0], ids[1], ids[2], ids[4], ids[5]]);
        // ids[3] was swept in-pass; ids[1] ran, is a zombie and waits
        // for the next pass. len counts zombies, like vanilla's list.
        assert_eq!(reg.len(), 5);

        let mut executed = Vec::new();
        reg.run(|_, id| executed.push(id));
        assert_eq!(executed, vec![ids[0], ids[2], ids[4], ids[5]]);
        assert_eq!(reg.len(), 4);
    }

    #[test]
    fn clear_invalidates_all_handles() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        assert_eq!(reg.len(), 2);

        reg.clear();
        assert_eq!(reg.len(), 0);
        assert!(!reg.is_valid(a));
        assert!(!reg.is_valid(b));

        let c = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        assert_eq!(c.index(), 0, "vacated slots are reused");
        assert_ne!(c.generation(), a.generation());
    }

    #[test]
    fn iteration_skips_zombies() {
        let mut reg = ThinkerRegistry::new();
        let a = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        let b = reg.add(Thinker::Mobj(super::super::mobj::Mobj::test_fixture()));
        reg.remove(a);

        let ids: Vec<_> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![b]);
        assert!(mobj_id_at(&reg, b.index()).is_some());
    }

    #[test]
    fn remove_of_unknown_handle_is_ignored() {
        let mut reg = ThinkerRegistry::new();
        let bogus = ThinkerId {
            index: 42,
            generation: 7,
        };
        reg.remove(bogus);
        assert_eq!(reg.len(), 0);
    }
}

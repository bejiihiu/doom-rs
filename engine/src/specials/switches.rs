//! Wall switches and buttons, ported from `p_switch.c`.
//!
//! A switch line, when used, swaps its front-side texture between a
//! predefined pair (`alphSwitchList`). One-shot switches flip line special
//! to 0 so they never fire again; buttons swap, then swap back after a
//! second — vanilla keeps the pressed buttons in `buttonlist[16]` and
//! reverts the texture in `P_UpdateSpecials`.
//!
//! ## Texture identity
//!
//! Vanilla resolves texture names to indices (`R_TextureNumForName`) and
//! compares indices. The sidedefs here store raw 8-byte names instead, so
//! [`SwitchList`] compares name bytes directly; the pairing rule
//! (`index ^ 1`) is unchanged.
//!
//! ## "Linked" switch lines
//!
//! Boom-style switch lines sharing a tag and toggling together do not
//! exist in linuxdoom-1.10 — wall texture fan-out by tag is not a thing
//! there (the tag fan-out applies to *sectors*, on doors and lights).
//! [`SpecialEvent::SwitchToggled`] carries the line's tag so a
//! compatibility layer can add it later; nothing else honors the tag.

use crate::info::{BUTTONTIME, MAXBUTTONS, MAXSWITCHES, SWITCH_LIST, SfxId};
use crate::map::Map;
use crate::specials::SpecialEvent;

/// Converts a texture name to the raw 8-byte form used by sidedefs
/// (uppercase, NUL-padded, truncated) — the same encoding the loader
/// stores.
fn name8(name: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (dst, src) in out.iter_mut().zip(name.as_bytes().iter().take(8)) {
        *dst = src.to_ascii_uppercase();
    }
    out
}

/// The `P_InitSwitchList` result: texture name pairs filtered by game
/// episode. Build once at startup (vanilla: at game init).
#[derive(Debug, Clone)]
pub struct SwitchList {
    /// Flat name table, pairs adjacent (`switchlist[i]` / `switchlist[i^1]`).
    flat: Vec<[u8; 8]>,
}

impl SwitchList {
    /// Vanilla `P_InitSwitchList`. `episode` is 1 for shareware, 2 for
    /// registered, 3 for commercial — the same gate vanilla applies (a
    /// switch whose table episode is higher than the game episode is not
    /// registered). Unknown texture names are still registered: vanilla
    /// also registered them unconditionally (the `R_CheckTextureNumForName`
    /// guard sits behind `#if 0`).
    pub fn new(episode: u16) -> Self {
        let mut flat = Vec::with_capacity(MAXSWITCHES * 2);
        for entry in SWITCH_LIST {
            if entry.episode <= episode {
                flat.push(name8(entry.name1));
                flat.push(name8(entry.name2));
            }
        }
        Self { flat }
    }

    /// Vanilla `R_TextureNumForName`-equivalent lookup: returns the index
    /// of `name` in the flat table, if the game has it switched.
    fn find(&self, name: &[u8; 8]) -> Option<usize> {
        self.flat.iter().position(|candidate| candidate == name)
    }

    /// True when there is at least one registered pair (zero-length switch
    /// list happens on a hypothetical episode below 1).
    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    /// Number of registered pairs.
    pub fn len(&self) -> usize {
        self.flat.len() / 2
    }
}

impl Default for SwitchList {
    /// Shareware switches, mirroring vanilla's `episode = 1` default.
    fn default() -> Self {
        Self::new(1)
    }
}

/// Vanilla `bwhere_e`: which texture slot on the sidedef gets swapped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonWhere {
    /// Upper ("top") texture.
    Top,
    /// Middle texture.
    Middle,
    /// Lower ("bottom") texture.
    Bottom,
}

/// One pressed button, waiting for its texture to revert.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Button {
    /// Index into [`Map::linedefs`].
    pub line: usize,
    /// Which texture slot swaps back.
    pub where_: ButtonWhere,
    /// The texture to restore when the timer expires.
    pub revert_to: [u8; 8],
    /// Tics left before the texture reverts.
    pub timer: i32,
    /// Front sector of the line (sound origin).
    pub sector: usize,
}

/// Vanilla `buttonlist[MAXBUTTONS]`: the pressed buttons. Per-level state,
/// owned by the game loop (not the [`crate::specials::SpecialContext`]).
#[derive(Debug)]
pub struct ButtonList {
    slots: [Option<Button>; MAXBUTTONS],
}

impl ButtonList {
    /// Creates an empty list (vanilla `memset(buttonlist, 0, ...)`).
    pub fn new() -> Self {
        Self {
            slots: [None; MAXBUTTONS],
        }
    }

    /// Vanilla `P_StartButton`. Does nothing when the same line already
    /// has a pressed button; logs and drops when all slots are taken
    /// (vanilla aborts with `I_Error` there — crashing the level because
    /// four players pressed four buttons one tick apart is worse than a
    /// stuck texture).
    fn start(&mut self, button: Button) {
        if self.slots.iter().flatten().any(|b| b.line == button.line) {
            // already pressed
            return;
        }
        for slot in &mut self.slots {
            if slot.is_none() {
                *slot = Some(button);
                return;
            }
        }
        tracing::warn!("P_StartButton: no button slots left!");
    }

    /// Iterates the pressed buttons (test support).
    pub fn pressed(&self) -> impl Iterator<Item = &Button> {
        self.slots.iter().flatten()
    }

    /// Number of pressed buttons.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// True when no buttons are pressed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ButtonList {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `P_ChangeSwitchTexture`. Swaps the front-side texture of
/// `line` to its switched partner when it is a registered switch
/// texture. One-shot switches clear the line special first, buttons
/// re-arm and revert after [`BUTTONTIME`] tics.
///
/// `use_again` mirrors vanilla's flag (0 = one-shot, 1 = button).
/// Returns true when a registered switch texture was found and swapped.
pub fn change_switch_texture(
    map: &mut Map,
    buttons: &mut ButtonList,
    switches: &SwitchList,
    events: &mut Vec<SpecialEvent>,
    line: usize,
    use_again: bool,
) -> bool {
    // vanilla: one-shot switches clear their special first; the exit-switch
    // sound check right after always sees 0 in 1.10 (the exit switch is
    // one-shot everywhere), so `sfx_swtchx` is unreachable there. Kept
    // 1:1 on purpose — including the order.
    if !use_again {
        map.linedefs[line].special = 0;
    }

    let tag = map.linedefs[line].tag;
    let Some(front) = map.linedefs[line].sidenum[0] else {
        tracing::warn!("switch line {line} has no front side; ignoring");
        return false;
    };
    let sector = map.sidedefs[front].sector;

    let (tex_top, tex_mid, tex_bot) = {
        let side = &map.sidedefs[front];
        (side.toptexture, side.midtexture, side.bottomtexture)
    };

    let mut sound = SfxId::sfx_swtchn;
    // exit switch? (unreachable in 1.10 — see above; kept for parity)
    if map.linedefs[line].special == 11 {
        sound = SfxId::sfx_swtchx;
    }

    // find which slot (if any) holds a registered switch texture
    let slot = switches
        .find(&tex_top)
        .map(|index| (index, ButtonWhere::Top))
        .or_else(|| {
            switches
                .find(&tex_mid)
                .map(|index| (index, ButtonWhere::Middle))
        })
        .or_else(|| {
            switches
                .find(&tex_bot)
                .map(|index| (index, ButtonWhere::Bottom))
        });

    let Some((index, where_)) = slot else {
        // not a switch texture; vanilla silently does nothing
        return false;
    };

    // vanilla plays the sound at the *first* button's sound origin
    // (`buttonlist[0].soundorg`) — a 1.10 bug; the line's own sector is
    // the position a listener wants here
    events.push(SpecialEvent::SectorSound { sector, sfx: sound });

    let flipped = switches.flat[index ^ 1];
    {
        let side = &mut map.sidedefs[front];
        match where_ {
            ButtonWhere::Top => side.toptexture = flipped,
            ButtonWhere::Middle => side.midtexture = flipped,
            ButtonWhere::Bottom => side.bottomtexture = flipped,
        }
    }

    if use_again {
        buttons.start(Button {
            line,
            where_,
            revert_to: switches.flat[index],
            timer: BUTTONTIME,
            sector,
        });
    }

    events.push(SpecialEvent::SwitchToggled {
        line,
        tag,
        use_again,
    });

    true
}

/// Vanilla `P_UpdateSpecials`' button pass: tick every pressed button
/// and revert the texture of the ones whose timer expired. Call once per
/// tic from the specials tick (agent 30 wires this into `P_UpdateSpecials`).
pub fn update_buttons(map: &mut Map, buttons: &mut ButtonList, events: &mut Vec<SpecialEvent>) {
    for slot in &mut buttons.slots {
        let Some(button) = slot else {
            continue;
        };
        let mut button = *button;
        button.timer -= 1;
        if button.timer != 0 {
            *slot = Some(button);
            continue;
        }

        // time to revert
        if let Some(front) = map.linedefs[button.line].sidenum[0] {
            let side = &mut map.sidedefs[front];
            match button.where_ {
                ButtonWhere::Top => side.toptexture = button.revert_to,
                ButtonWhere::Middle => side.midtexture = button.revert_to,
                ButtonWhere::Bottom => side.bottomtexture = button.revert_to,
            }
        }
        events.push(SpecialEvent::SectorSound {
            sector: button.sector,
            sfx: SfxId::sfx_swtchn,
        });
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Map;
    use crate::specials::testutil::split_map;

    fn map_with_switch_line() -> Map {
        let mut map = split_map(0, 64, 0);
        // line 2 is the one-sided north wall facing sector 1; put a plain
        // switch texture in its front sidedef (sidedef index 2)
        map.sidedefs[2].midtexture = name8("SW1GRAY");
        map.linedefs[2].special = 63; // raise door (button)
        map.linedefs[2].tag = 7;
        map
    }

    #[test]
    fn switch_list_episode_gating() {
        let shareware = SwitchList::new(1);
        let registered = SwitchList::new(2);
        let commercial = SwitchList::new(3);

        assert!(shareware.find(&name8("SW1GRAY")).is_some());
        // registered-only switch (episode 2)
        assert!(shareware.find(&name8("SW1BLUE")).is_none());
        assert!(registered.find(&name8("SW1BLUE")).is_some());
        // commercial-only switch (episode 3)
        assert!(registered.find(&name8("SW1PANEL")).is_none());
        assert!(commercial.find(&name8("SW1PANEL")).is_some());

        assert_eq!(shareware.len(), 19); // the 19 shareware pairs
        assert_eq!(registered.len(), 29);
        assert_eq!(commercial.len(), 40);
    }

    #[test]
    fn one_shot_switch_swaps_texture_clears_special_no_button() {
        let mut map = map_with_switch_line();
        let mut buttons = ButtonList::new();
        let switches = SwitchList::new(1);
        let mut events = Vec::new();

        map.linedefs[2].special = 29; // raise door, one-shot

        let changed =
            change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, false);

        assert!(changed);
        assert_eq!(map.sidedefs[2].midtexture, name8("SW2GRAY"));
        assert_eq!(
            map.linedefs[2].special, 0,
            "one-shot switch clears the special"
        );
        assert!(buttons.is_empty(), "one-shot does not press a button");

        // sound + toggle events
        assert!(events.iter().any(|e| matches!(
            e,
            SpecialEvent::SectorSound {
                sector: 1,
                sfx: SfxId::sfx_swtchn
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            SpecialEvent::SwitchToggled {
                line: 2,
                tag: 7,
                use_again: false
            }
        )));
    }

    #[test]
    fn button_switch_reverts_texture_after_cooldown() {
        let mut map = map_with_switch_line(); // special 63 = button
        let mut buttons = ButtonList::new();
        let switches = SwitchList::new(1);
        let mut events = Vec::new();

        let changed =
            change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, true);
        assert!(changed);
        assert_eq!(map.sidedefs[2].midtexture, name8("SW2GRAY"));
        assert_eq!(map.linedefs[2].special, 63, "button keeps its special");
        assert_eq!(buttons.len(), 1);

        // press it a second time while pressed: no double-registration
        let changed =
            change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, true);
        assert!(changed, "texture still swaps every press");
        assert_eq!(buttons.len(), 1, "button is not double-registered");

        // tick BUTTONTIME times: the texture reverts to SW1GRAY
        for _ in 0..BUTTONTIME {
            update_buttons(&mut map, &mut buttons, &mut events);
        }
        assert_eq!(map.sidedefs[2].midtexture, name8("SW1GRAY"));
        assert!(buttons.is_empty());
        assert!(events.iter().any(|e| matches!(
            e,
            SpecialEvent::SectorSound {
                sector: 1,
                sfx: SfxId::sfx_swtchn
            }
        )));
    }

    #[test]
    fn button_timer_only_decrements_while_pressed() {
        let mut map = map_with_switch_line();
        let mut buttons = ButtonList::new();
        let switches = SwitchList::new(1);
        let mut events = Vec::new();

        change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, true);
        // one tic: timer 35 -> 34
        update_buttons(&mut map, &mut buttons, &mut events);
        assert_eq!(map.sidedefs[2].midtexture, name8("SW2GRAY"));
        assert_eq!(buttons.len(), 1);
    }

    #[test]
    fn pressing_switch_with_a_non_switch_texture_is_a_noop() {
        let mut map = map_with_switch_line();
        map.sidedefs[2].midtexture = name8("WALL1");
        let mut buttons = ButtonList::new();
        let switches = SwitchList::new(1);
        let mut events = Vec::new();

        let changed =
            change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, true);
        assert!(!changed);
        assert_eq!(map.sidedefs[2].midtexture, name8("WALL1"));
        assert!(buttons.is_empty());
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SpecialEvent::SwitchToggled { .. }))
        );
    }

    #[test]
    fn exit_switch_sound_stays_swtchn_for_one_shots() {
        // vanilla 1.10 zeroes the special before the exit-switch sound
        // check — the swtchx branch is unreachable for one-shots
        let mut map = map_with_switch_line();
        map.linedefs[2].special = 11; // exit
        map.sidedefs[2].midtexture = name8("SW1EXIT");
        let mut buttons = ButtonList::new();
        let switches = SwitchList::new(1);
        let mut events = Vec::new();

        let changed =
            change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, false);
        assert!(changed);
        assert!(events.iter().any(|e| matches!(
            e,
            SpecialEvent::SectorSound {
                sector: 1,
                sfx: SfxId::sfx_swtchn
            }
        )));
        assert!(!events.iter().any(|e| matches!(
            e,
            SpecialEvent::SectorSound {
                sector: 1,
                sfx: SfxId::sfx_swtchx
            }
        )));
    }

    #[test]
    fn no_button_slots_left_drops_the_press_without_crashing() {
        let mut map = map_with_switch_line();
        let mut buttons = ButtonList::new();
        let switches = SwitchList::new(1);
        let mut events = Vec::new();

        // fill all MAXBUTTONS slots with distinct lines
        for index in 0..MAXBUTTONS {
            buttons.start(Button {
                line: index,
                where_: ButtonWhere::Middle,
                revert_to: name8("SW1GRAY"),
                timer: 10,
                sector: 0,
            });
        }
        // one extra line for the 17th button
        map.linedefs[2].special = 63;
        let changed =
            change_switch_texture(&mut map, &mut buttons, &switches, &mut events, 2, true);
        assert!(changed, "the texture still swaps");
        assert_eq!(buttons.len(), MAXBUTTONS, "the 17th press is dropped");
    }
}

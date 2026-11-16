//! Menu level definitions and the item tables, ported from the menu
//! setup section of `m_menu.c`.
//!
//! Each vanilla `menu_t` becomes a [`MenuLevel`]; the `menuitem_t`
//! arrays become the const tables below. The `lastOn` fields of the
//! vanilla structs live in [`crate::menu::Menu`] instead, since they
//! are per-session state, not static data.
//!
//! Item status, straight from `menuitem_t.status`:
//!
//! * `-1` → [`ItemStatus::Spacer`] — skipped by the cursor, no letters;
//! * `0` → [`ItemStatus::Disabled`] — the cursor may land on it, but
//!   Enter does nothing (used for empty save slots);
//! * `1` → [`ItemStatus::Ok`] — selectable;
//! * `2` → [`ItemStatus::Arrows`] — Left/Right adjust, Enter = Right.

/// Which menu is on screen. The variants are in vanilla table order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuLevel {
    Main,
    Episode,
    NewGame,
    Options,
    Sound,
    LoadGame,
    SaveGame,
    ReadThis1,
    ReadThis2,
}

impl MenuLevel {
    /// Number of variants — sizes the per-level `lastOn` table.
    pub const COUNT: usize = 9;

    /// Vanilla `prevMenu` chain, with the commercial-mode rewire of the
    /// new-game menu (`NewDef.prevMenu = &MainDef` in `M_Init`) folded
    /// in. `None` only for the main menu.
    pub fn prev_menu(self, commercial: bool) -> Option<MenuLevel> {
        match self {
            MenuLevel::Main => None,
            MenuLevel::Episode => Some(MenuLevel::Main),
            MenuLevel::NewGame => {
                if commercial {
                    Some(MenuLevel::Main)
                } else {
                    Some(MenuLevel::Episode)
                }
            }
            MenuLevel::Options | MenuLevel::LoadGame | MenuLevel::SaveGame => Some(MenuLevel::Main),
            MenuLevel::Sound => Some(MenuLevel::Options),
            MenuLevel::ReadThis1 => Some(MenuLevel::Main),
            MenuLevel::ReadThis2 => Some(MenuLevel::ReadThis1),
        }
    }
}

/// `menuitem_t.status` as an enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Spacer,
    Disabled,
    Ok,
    Arrows,
}

/// What an item does. The payload is the `choice` argument the vanilla
/// `routine(int choice)` gets: for `Arrows` items Left = 0 / Right = 1,
/// for the rest it is the item index. `None` is the vanilla `routine == 0`
/// (the spacer slots).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    None,
    // top level
    NewGame,
    Options,
    LoadGame,
    LoadSelect,
    SaveGame,
    SaveSelect,
    ReadThis,
    ReadThis2,
    FinishReadThis,
    QuitDoom,
    // episode / skill
    Episode,
    ChooseSkill,
    // options
    EndGame,
    ChangeMessages,
    ChangeDetail,
    SizeDisplay,
    ChangeSensitivity,
    Sound,
    // sound volume
    SfxVolume,
    MusicVolume,
}

/// One entry in a menu. `name` is the patch lump drawn for the item, or
/// `""` for none (the load/save menus draw text instead).
#[derive(Clone, Copy, Debug)]
pub struct MenuItem {
    pub status: ItemStatus,
    pub name: &'static str,
    pub(crate) action: MenuAction,
    /// Hotkey for the item in this menu (`0` = none). Vanilla matches it
    /// case-sensitively against the raw key — the input layer
    /// lower-cases letters, so lower-case keys are what land here.
    pub alpha_key: u8,
}

const fn item(
    status: ItemStatus,
    name: &'static str,
    action: MenuAction,
    alpha_key: u8,
) -> MenuItem {
    MenuItem {
        status,
        name,
        action,
        alpha_key,
    }
}

use ItemStatus::*;

/// `MainDef` — the escape-key menu. Commercial mode drops the
/// "read this!" entry (`MainMenu[readthis] = MainMenu[quitdoom]`,
/// `numitems--`), so the effective length is 5 there.
pub(crate) const MAIN_MENU: [MenuItem; 6] = [
    item(Ok, "M_NGAME", MenuAction::NewGame, b'n'),
    item(Ok, "M_OPTION", MenuAction::Options, b'o'),
    item(Ok, "M_LOADG", MenuAction::LoadGame, b'l'),
    item(Ok, "M_SAVEG", MenuAction::SaveGame, b's'),
    item(Ok, "M_RDTHIS", MenuAction::ReadThis, b'r'),
    item(Ok, "M_QUITG", MenuAction::QuitDoom, b'q'),
];

/// `EpiDef`. Shareware and registered builds drop the fourth episode
/// (`EpiDef.numitems--`), so the effective length is 3 there.
/// `ep4` stays in the table for retail.
pub(crate) const EPISODE_MENU: [MenuItem; 4] = [
    item(Ok, "M_EPI1", MenuAction::Episode, b'k'),
    item(Ok, "M_EPI2", MenuAction::Episode, b't'),
    item(Ok, "M_EPI3", MenuAction::Episode, b'i'),
    item(Ok, "M_EPI4", MenuAction::Episode, b't'),
];

/// `NewDef` — the five skill levels.
pub(crate) const NEWGAME_MENU: [MenuItem; 5] = [
    item(Ok, "M_JKILL", MenuAction::ChooseSkill, b'i'),
    item(Ok, "M_ROUGH", MenuAction::ChooseSkill, b'h'),
    item(Ok, "M_HURT", MenuAction::ChooseSkill, b'h'),
    item(Ok, "M_ULTRA", MenuAction::ChooseSkill, b'u'),
    item(Ok, "M_NMARE", MenuAction::ChooseSkill, b'n'),
];

/// `OptionsDef`. The `-1` entries are spacers the cursor skips.
pub(crate) const OPTIONS_MENU: [MenuItem; 8] = [
    item(Ok, "M_ENDGAM", MenuAction::EndGame, b'e'),
    item(Ok, "M_MESSG", MenuAction::ChangeMessages, b'm'),
    item(Ok, "M_DETAIL", MenuAction::ChangeDetail, b'g'),
    item(Arrows, "M_SCRNSZ", MenuAction::SizeDisplay, b's'),
    item(Spacer, "", MenuAction::None, 0),
    item(Arrows, "M_MSENS", MenuAction::ChangeSensitivity, b'm'),
    item(Spacer, "", MenuAction::None, 0),
    item(Ok, "M_SVOL", MenuAction::Sound, b's'),
];

/// `SoundDef`.
pub(crate) const SOUND_MENU: [MenuItem; 4] = [
    item(Arrows, "M_SFXVOL", MenuAction::SfxVolume, b's'),
    item(Spacer, "", MenuAction::None, 0),
    item(Arrows, "M_MUSVOL", MenuAction::MusicVolume, b'm'),
    item(Spacer, "", MenuAction::None, 0),
];

/// `LoadDef`. The slot's availability (its `status`) is resolved at
/// runtime from the slot names the game layer hands in — an empty slot
/// is `Disabled`, matching `LoadMenu[i].status = 0`.
pub(crate) const LOAD_MENU: [MenuItem; 6] = [
    item(Ok, "", MenuAction::LoadSelect, b'1'),
    item(Ok, "", MenuAction::LoadSelect, b'2'),
    item(Ok, "", MenuAction::LoadSelect, b'3'),
    item(Ok, "", MenuAction::LoadSelect, b'4'),
    item(Ok, "", MenuAction::LoadSelect, b'5'),
    item(Ok, "", MenuAction::LoadSelect, b'6'),
];

/// `SaveMenu` — always fully armed.
pub(crate) const SAVE_MENU: [MenuItem; 6] = [
    item(Ok, "", MenuAction::SaveSelect, b'1'),
    item(Ok, "", MenuAction::SaveSelect, b'2'),
    item(Ok, "", MenuAction::SaveSelect, b'3'),
    item(Ok, "", MenuAction::SaveSelect, b'4'),
    item(Ok, "", MenuAction::SaveSelect, b'5'),
    item(Ok, "", MenuAction::SaveSelect, b'6'),
];

/// `ReadDef1` / `ReadDef2` — one invisible hotkey item each (the
/// "read this!" pages are patches, not menu entries).
pub(crate) const READ1_MENU: [MenuItem; 1] = [item(Ok, "", MenuAction::ReadThis2, 0)];
pub(crate) const READ2_MENU: [MenuItem; 1] = [item(Ok, "", MenuAction::FinishReadThis, 0)];

/// Fresh item table for `level`, with the per-gamemode length folded in.
/// `count` is the effective number of entries; the rest of the table is
/// padding that must not be indexed.
pub(crate) fn level_items(level: MenuLevel) -> &'static [MenuItem] {
    match level {
        MenuLevel::Main => &MAIN_MENU,
        MenuLevel::Episode => &EPISODE_MENU,
        MenuLevel::NewGame => &NEWGAME_MENU,
        MenuLevel::Options => &OPTIONS_MENU,
        MenuLevel::Sound => &SOUND_MENU,
        MenuLevel::LoadGame => &LOAD_MENU,
        MenuLevel::SaveGame => &SAVE_MENU,
        MenuLevel::ReadThis1 => &READ1_MENU,
        MenuLevel::ReadThis2 => &READ2_MENU,
    }
}

/// Vanilla `x`/`y` of each menu. ReadThis1 gets the commercial-mode
/// patch (330,165) when `commercial` is set — see [`crate::menu::Menu`].
pub(crate) const fn level_xy(level: MenuLevel) -> (i32, i32) {
    match level {
        // MainDef.y is 64; M_Init adds 8 for commercial (handled by the
        // Menu because it also affects nothing else)
        MenuLevel::Main => (97, 64),
        MenuLevel::Episode => (48, 63),
        MenuLevel::NewGame => (48, 63),
        MenuLevel::Options => (60, 37),
        MenuLevel::Sound => (80, 64),
        MenuLevel::LoadGame | MenuLevel::SaveGame => (80, 54),
        MenuLevel::ReadThis1 => (280, 185),
        MenuLevel::ReadThis2 => (330, 175),
    }
}

/// Default `lastOn` per level. `NewDef` defaults to `hurtme`
/// (`M_HURT`, "hurt me plenty", index 2) in the original.
pub(crate) const fn default_last_on(level: MenuLevel) -> usize {
    match level {
        MenuLevel::NewGame => 2, // hurtme
        _ => 0,
    }
}

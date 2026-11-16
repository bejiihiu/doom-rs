//! The `m_menu.c` state machine: current level, cursor, dispatch,
//! the message prompt system and the save-game string editor.
//!
//! Everything the menu needs from the outside comes in through
//! [`MenuContext`] (updated with [`Menu::set_context`]) and
//! [`SaveSlotInfo`] (through [`Menu::set_save_names`]); everything the
//! game needs out of the menu leaves as [`MenuEvent`] / [`MenuSound`]
//! values. Nothing here touches game state, the filesystem or the video
//! layer directly — the game loop applies the events.

use super::levels::{
    self, EPISODE_MENU, ItemStatus, MAIN_MENU, MenuAction, MenuItem, MenuLevel, level_items,
    level_xy,
};
use super::strings;
use crate::hud::HudFont;

/// Vanilla game modes (`gamemode_t` in `doomdef.h`, minus the
/// `indetermined` failure mode — the engine does not boot without an
/// IWAD).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameMode {
    Shareware,
    Registered,
    Commercial,
    Retail,
}

impl GameMode {
    /// `commercial` in the original `doomdef.h` sense.
    pub fn is_commercial(self) -> bool {
        matches!(self, GameMode::Commercial)
    }
}

/// Skill levels (`skill_t` in `doomdef.h`), as plain indices.
pub const SKILL_BABY: u8 = 0;
pub const SKILL_EASY: u8 = 1;
pub const SKILL_MEDIUM: u8 = 2;
pub const SKILL_HARD: u8 = 3;
pub const SKILL_NIGHTMARE: u8 = 4;

/// Save-game slots vanilla supports (`load_end`), and the description length.
pub const SAVE_SLOTS: usize = 6;
pub const SAVESTRINGSIZE: usize = 24;
pub const LINEHEIGHT: i32 = 16;
/// `SKULLXOFF` — the skull cursor sits 32 px left of the item column.
pub const SKULLXOFF: i32 = -32;

/// Quicksave slot sentinel: no slot picked yet (`quickSaveSlot == -1`).
pub const QUICKSAVE_NONE: i32 = -1;
/// Quicksave slot sentinel: the player is expected to pick a slot now
/// (`quickSaveSlot == -2`).
pub const QUICKSAVE_PICK: i32 = -2;

/// What the game layer hands the menu about its world. Mirrors the
/// vanilla globals `M_Responder` reads (`netgame`, `usergame`,
/// `gamestate`, `gamemode`, `demoplayback`, `gametic`, `language`).
#[derive(Clone, Debug)]
pub struct MenuContext {
    pub gamemode: GameMode,
    /// `netgame` — blocks almost every action except `idclev`.
    pub netgame: bool,
    /// `usergame` — false until the player is in an actual game (i.e.
    /// not on the title screen).
    pub usergame: bool,
    /// True while `gamestate == GS_LEVEL` (drives whether saving is
    /// offered at all).
    pub in_level: bool,
    /// `demoplayback` — allows starting a new game from the menu even
    /// in a netgame.
    pub demoplayback: bool,
    /// `gametic` — only used to pick the nagging quit message.
    pub gametic: u64,
    /// `language == english` — only affects the quit message.
    pub language_english: bool,
}

impl MenuContext {
    /// A neutral single-player context: commercial (linuxdoom-1.10 is a
    /// DOOM II build), not in a game, English.
    pub fn commercial() -> Self {
        Self {
            gamemode: GameMode::Commercial,
            netgame: false,
            usergame: false,
            in_level: false,
            demoplayback: false,
            gametic: 0,
            language_english: true,
        }
    }
}

/// The key input the menu consumes. Letters arrive lower-cased, like
/// every real input layer (`xlatekey` in the original forces A-Z to
/// lower case before the menu ever sees them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Backspace,
    /// A printable byte (`!` through `~`, plus space).
    Printable(u8),
}

impl MenuKey {
    /// Maps the vanilla `KEY_*` codes (`doomdef.h`) onto menu keys.
    /// Useful for the input layer; not used by the menu itself.
    pub const fn from_doom_key(code: u8) -> Option<MenuKey> {
        match code {
            0xad => Some(MenuKey::Up),
            0xaf => Some(MenuKey::Down),
            0xac => Some(MenuKey::Left),
            0xae => Some(MenuKey::Right),
            27 => Some(MenuKey::Escape),
            13 => Some(MenuKey::Enter),
            127 => Some(MenuKey::Backspace),
            0x20..=0x7e => Some(MenuKey::Printable(code)),
            _ => None,
        }
    }
}

/// Sounds the menu asks the game layer to play (the vanilla
/// `S_StartSound` calls). The mapping to actual `sfx_*` ids is the
/// sound layer's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuSound {
    /// `sfx_pstop` — cursor move (up/down/hotkey).
    PStop,
    /// `sfx_stnmov` — slider adjust / left-right key.
    StnMov,
    /// `sfx_pistol` — item confirm.
    Pistol,
    /// `sfx_swtchx` — close menu / confirm dialog.
    SwitchOff,
    /// `sfx_swtchn` — open menu / back.
    SwitchOn,
    /// `sfx_oof` — denied (not playing yet, etc).
    Oof,
}

/// Fire-and-forget menu actions, as plain data. The game layer applies
/// them; the menu only reports intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    /// `I_Quit` — the player confirmed the quit prompt.
    Quit,
    /// `D_StartTitle` — the player confirmed the end-game prompt.
    StartTitle,
    /// `G_DeferedInitNew` — start a new game.
    NewGame { episode: u8, map: u8, skill: u8 },
    /// Load savegame slot `0..6` (`G_LoadGame`).
    LoadGame { slot: u8 },
    /// Write savegame slot `0..6` with this description
    /// (`G_SaveGame`).
    SaveGame { slot: u8, name: String },
    /// `S_SetSfxVolume` — 0..=15.
    SfxVolume(i32),
    /// `S_SetMusicVolume` — 0..=15.
    MusicVolume(i32),
    /// Mouse sensitivity — 0..=9.
    MouseSensitivity(i32),
    /// Detail level: 0 = high detail, 1 = low detail.
    DetailLevel(i32),
    /// `screenblocks` — view size in 3..=11 (status bar takes 10).
    ScreenBlocks(i32),
    /// Show player messages. The game layer also flips its own
    /// message latch (vanilla `showMessages`) and confirms via the
    /// forced "Messages ON/OFF" player message.
    ShowMessages(bool),
}

/// What one keypress did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuKeyResult {
    /// True when the key was swallowed by the menu (vanilla's
    /// `true`/`false` from `M_Responder`).
    pub consumed: bool,
    /// Actions to apply, in order.
    pub events: Vec<MenuEvent>,
    /// Sounds to play, in order.
    pub sounds: Vec<MenuSound>,
}

impl MenuKeyResult {
    fn unconsumed() -> Self {
        Self::default()
    }
    fn consumed() -> Self {
        Self {
            consumed: true,
            events: Vec::new(),
            sounds: Vec::new(),
        }
    }
}

/// One of the load/save slots, as the game layer reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveSlotInfo {
    /// Slot description (the first `SAVESTRINGSIZE` bytes of the save
    /// file in the original). Ignored for unavailable slots.
    pub name: String,
    /// `false` for slots that were never written — Enter and friend
    /// refuse them (vanilla `LoadMenu[i].status = 0`).
    pub available: bool,
}

impl Default for SaveSlotInfo {
    fn default() -> Self {
        Self {
            name: strings::EMPTY_SLOT.to_string(),
            available: false,
        }
    }
}

/// What a displayed message expects back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageResponse {
    /// "press a key" prompts — any key dismisses.
    AnyKey,
    /// Quit prompt — `y` quits.
    Quit,
    /// End-game prompt — `y` returns to the title screen.
    EndGame,
    /// Quicksave prompt — `y` overwrites the slot.
    QuickSave,
    /// Quickload prompt — `y` loads the slot.
    QuickLoad,
    /// Nightmare skill prompt — `y` starts the game anyway.
    Nightmare,
}

/// An up dialog (`M_StartMessage` / `M_StopMessage`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuMessage {
    /// The text; rendered line by line at `\n` separators.
    pub text: String,
    /// True while the prompt only accepts `y`/`n`/`space`/Escape.
    pub needs_input: bool,
    response: MessageResponse,
}

/// The save-game string editor (`saveStringEnter` block in
/// `M_Responder`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SaveInput {
    slot: usize,
    char_index: usize,
    old_string: String,
}

/// The menu itself: level, cursor, values, message prompt and the
/// save-string editor. Build with [`Menu::new`], drive with
/// [`Menu::tick`] / [`Menu::handle_key`], draw with [`Menu::render`].
#[derive(Clone, Debug)]
pub struct Menu {
    context: MenuContext,
    level: MenuLevel,
    item_on: usize,
    last_on: [usize; MenuLevel::COUNT],
    active: bool,
    skull_anim_counter: i32,
    which_skull: usize,
    message: Option<MenuMessage>,
    message_last_menu_active: bool,
    save_input: Option<SaveInput>,
    save_slots: [SaveSlotInfo; SAVE_SLOTS],
    quick_save_slot: i32,
    /// Selected episode, 0-based (`epi` in the original).
    epi: u8,
    // the option-menu values. Vanilla keeps them in globals with
    // defaults from doom.cfg; here the game layer passes the config
    // values at construction and the events carry the new ones out.
    show_messages: bool,
    detail_level: i32,
    /// `screenblocks - 3` — the on-screen slider position (0..=8).
    screen_size: i32,
    screen_blocks: i32,
    mouse_sensitivity: i32,
    sfx_volume: i32,
    music_volume: i32,
    // commercial-mode menu rewires from M_Init
    main_menu: [MenuItem; 6],
    main_count: usize,
    main_y: i32,
    epi_count: usize,
    read1_x: i32,
    read1_y: i32,
    read1_item: MenuItem,
}

/// The values the game layer seeds from `doom.cfg` at startup.
#[derive(Clone, Debug)]
pub struct MenuOptions {
    pub mouse_sensitivity: i32,
    pub show_messages: bool,
    pub detail_level: i32,
    /// `screenblocks` (3..=11 in vanilla config files).
    pub screen_blocks: i32,
    pub sfx_volume: i32,
    pub music_volume: i32,
}

impl MenuOptions {
    /// The vanilla defaults (`M_Init`/`m_misc.c`): sensitivity 5,
    /// messages on, high detail, screenblocks 9, both volumes 8/8.
    pub fn defaults() -> Self {
        Self {
            mouse_sensitivity: 5,
            show_messages: true,
            detail_level: 0,
            screen_blocks: 9,
            sfx_volume: 8,
            music_volume: 8,
        }
    }
}

impl Menu {
    /// `M_Init`: applies the per-gamemode menu surgery (commercial drops
    /// "read this!" from the main menu and shifts it down; shareware and
    /// registered drop the fourth episode; the commercial read-this page
    /// becomes the credit page).
    pub fn new(context: MenuContext, options: MenuOptions) -> Self {
        let commercial = context.gamemode.is_commercial();
        let (main_count, main_y) = if commercial {
            (5, 72)
        } else {
            (6, level_xy(MenuLevel::Main).1)
        };
        let epi_count = match context.gamemode {
            GameMode::Retail => 4,
            GameMode::Commercial => 4, // commercial never reaches the episode menu
            GameMode::Shareware | GameMode::Registered => 3,
        };
        let (read1_x, read1_y) = if commercial {
            (330, 165) // commercial M_Init rewire
        } else {
            level_xy(MenuLevel::ReadThis1)
        };
        // commercial: ReadMenu1[0].routine = M_FinishReadThis — the
        // credit page returns straight to the main menu
        let read1_item = if commercial {
            MenuItem {
                status: ItemStatus::Ok,
                name: "",
                action: MenuAction::FinishReadThis,
                alpha_key: 0,
            }
        } else {
            levels::READ1_MENU[0]
        };
        let screen_blocks = options.screen_blocks.clamp(3, 11);
        let mut last_on = [0usize; MenuLevel::COUNT];
        last_on[MenuLevel::NewGame as usize] = levels::default_last_on(MenuLevel::NewGame);
        // vanilla M_Init: commercial copies the quit entry over the
        // read-this one and drops the last entry
        let mut main_menu = MAIN_MENU;
        if commercial {
            main_menu[4] = main_menu[5];
        }
        Self {
            context,
            level: MenuLevel::Main,
            item_on: last_on[MenuLevel::Main as usize],
            last_on,
            active: false,
            skull_anim_counter: 10,
            which_skull: 0,
            message: None,
            message_last_menu_active: false,
            save_input: None,
            save_slots: Default::default(),
            quick_save_slot: QUICKSAVE_NONE,
            epi: 0,
            show_messages: options.show_messages,
            detail_level: options.detail_level & 1,
            screen_size: screen_blocks - 3,
            screen_blocks,
            mouse_sensitivity: options.mouse_sensitivity.clamp(0, 9),
            sfx_volume: options.sfx_volume.clamp(0, 15),
            music_volume: options.music_volume.clamp(0, 15),
            main_menu,
            main_count,
            main_y,
            epi_count,
            read1_x,
            read1_y,
            read1_item,
        }
    }

    // --- bookkeeping exposed to the game layer -------------------------

    /// `menuactive`.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The level currently on screen.
    pub fn level(&self) -> MenuLevel {
        self.level
    }

    /// Cursor position within the level (`itemOn`).
    pub fn item_on(&self) -> usize {
        self.item_on
    }

    /// `whichSkull` — which of the two skull patches to draw.
    pub fn which_skull(&self) -> usize {
        self.which_skull
    }

    /// The message currently up, if any.
    pub fn message(&self) -> Option<&MenuMessage> {
        self.message.as_ref()
    }

    /// The save-slot descriptions as the game layer last reported them.
    pub fn save_names(&self) -> &[SaveSlotInfo; SAVE_SLOTS] {
        &self.save_slots
    }

    /// The quicksave slot (`-1` = none picked, `-2` = pick-one-now).
    pub fn quick_save_slot(&self) -> i32 {
        self.quick_save_slot
    }

    /// Current option values — for the game layer to re-apply after a
    /// load or on startup (e.g. `screenblocks`).
    pub fn screen_blocks(&self) -> i32 {
        self.screen_blocks
    }
    pub fn detail_level(&self) -> i32 {
        self.detail_level
    }
    pub fn mouse_sensitivity(&self) -> i32 {
        self.mouse_sensitivity
    }
    pub fn sfx_volume(&self) -> i32 {
        self.sfx_volume
    }
    pub fn music_volume(&self) -> i32 {
        self.music_volume
    }
    pub fn show_messages(&self) -> bool {
        self.show_messages
    }

    /// Replaces the context (use when `netgame`/`usergame`/`in_level`
    /// flip, or on startup once the IWAD is known).
    pub fn set_context(&mut self, context: MenuContext) {
        self.context = context;
    }

    /// The context the menu is running with.
    pub fn context(&self) -> &MenuContext {
        &self.context
    }

    /// Hands the menu the save-slot descriptions; call after scanning
    /// the save directory (the original does the same in
    /// `M_ReadSaveStrings`). Names are truncated to
    /// `SAVESTRINGSIZE - 1` bytes, descriptions of missing slots are
    /// dropped to [`SaveSlotInfo::default`].
    pub fn set_save_names(&mut self, slots: &[SaveSlotInfo]) {
        for (i, slot) in self.save_slots.iter_mut().enumerate() {
            *slot = slots.get(i).cloned().unwrap_or_default();
            if slot.name.len() > SAVESTRINGSIZE - 1 {
                slot.name.truncate(SAVESTRINGSIZE - 1);
            }
        }
    }

    // --- items / levels -------------------------------------------------

    fn items(&self) -> &[MenuItem] {
        match self.level {
            MenuLevel::Main => &self.main_menu[..self.main_count],
            MenuLevel::Episode => &EPISODE_MENU[..self.epi_count],
            MenuLevel::ReadThis1 => std::slice::from_ref(&self.read1_item),
            other => level_items(other),
        }
    }

    /// The effective status of `index` in the current level — the load
    /// menu resolves slot availability at runtime.
    fn item_status(&self, index: usize) -> ItemStatus {
        if self.level == MenuLevel::LoadGame && !self.save_slots[index].available {
            return ItemStatus::Disabled;
        }
        self.items()[index].status
    }

    fn level_xy_effective(&self) -> (i32, i32) {
        match self.level {
            MenuLevel::Main => (level_xy(MenuLevel::Main).0, self.main_y),
            MenuLevel::ReadThis1 => (self.read1_x, self.read1_y),
            other => level_xy(other),
        }
    }

    /// The item count the Enter/arrow handlers walk in the current level.
    fn item_count(&self) -> usize {
        self.items().len()
    }

    // --- M_Ticker --------------------------------------------------------

    /// `M_Ticker`: advance the skull cursor animation; also latches
    /// `gametic` for the quit-nag picker. Call once per game tic.
    pub fn tick(&mut self, gametic: u64) {
        self.context.gametic = gametic;
        if self.skull_anim_counter <= 1 {
            self.which_skull ^= 1;
            self.skull_anim_counter = 8;
        } else {
            self.skull_anim_counter -= 1;
        }
    }

    // --- M_StartControlPanel / M_ClearMenus / M_SetupNextMenu -----------

    /// `M_StartControlPanel`: open the menu on the main page (no-op if
    /// already open).
    pub fn start_control_panel(&mut self) {
        if self.active {
            return;
        }
        self.active = true;
        self.level = MenuLevel::Main;
        self.item_on = self.last_on[MenuLevel::Main as usize];
    }

    /// `M_ClearMenus`: close the menu.
    pub fn clear_menus(&mut self) {
        self.active = false;
    }

    /// `M_SetupNextMenu`: switch to `level`, restoring its cursor.
    fn setup_next_menu(&mut self, level: MenuLevel) {
        self.level = level;
        self.item_on = self.last_on[level as usize];
    }

    // --- M_StartMessage / M_StopMessage ---------------------------------

    fn start_message(&mut self, text: String, needs_input: bool, response: MessageResponse) {
        self.message_last_menu_active = self.active;
        self.message = Some(MenuMessage {
            text,
            needs_input,
            response,
        });
        self.active = true;
    }

    // --- the message prompt ----------------------------------------------

    fn handle_message_key(&mut self, key: MenuKey, result: &mut MenuKeyResult) -> bool {
        let Some(message) = self.message.take() else {
            return false;
        };
        let accepted = match key {
            MenuKey::Printable(b' ') | MenuKey::Printable(b'y') | MenuKey::Printable(b'n') => true,
            MenuKey::Escape => true,
            _ => !message.needs_input,
        };
        let ch = match key {
            MenuKey::Printable(b) => b,
            MenuKey::Escape => b'\x1b',
            MenuKey::Enter => b'\r',
            MenuKey::Backspace => 0x7f,
            MenuKey::Up => 0xad,
            MenuKey::Down => 0xaf,
            MenuKey::Left => 0xac,
            MenuKey::Right => 0xae,
        };
        if message.needs_input && !accepted {
            self.message = Some(message);
            return false;
        }
        // note: with needs_input == false every key is accepted, so the
        // !accepted case cannot happen here; the check above is the only gate
        self.active = self.message_last_menu_active;
        result.sounds.push(MenuSound::SwitchOff);
        // vanilla sets menuactive = false unconditionally after the
        // routine runs — even for "no" answers. Kept on purpose.
        match message.response {
            MessageResponse::AnyKey => {}
            MessageResponse::Quit => {
                if ch == b'y' {
                    result.events.push(MenuEvent::Quit);
                }
            }
            MessageResponse::EndGame => {
                if ch == b'y' {
                    self.last_on[self.level as usize] = self.item_on;
                    result.events.push(MenuEvent::StartTitle);
                }
            }
            MessageResponse::QuickSave => {
                if ch == b'y' {
                    self.commit_quick_save(result);
                }
            }
            MessageResponse::QuickLoad => {
                if ch == b'y' {
                    self.commit_quick_load(result);
                }
            }
            MessageResponse::Nightmare => {
                if ch == b'y' {
                    let episode = self.epi + 1;
                    result.events.push(MenuEvent::NewGame {
                        episode,
                        map: 1,
                        skill: SKILL_NIGHTMARE,
                    });
                }
            }
        }
        self.active = false;
        true
    }

    fn commit_quick_save(&mut self, result: &mut MenuKeyResult) {
        if self.quick_save_slot < 0 {
            return;
        }
        self.emit_save(self.quick_save_slot as usize, result);
    }

    fn commit_quick_load(&mut self, result: &mut MenuKeyResult) {
        if self.quick_save_slot < 0 {
            return;
        }
        self.clear_menus();
        result.events.push(MenuEvent::LoadGame {
            slot: self.quick_save_slot as u8,
        });
    }

    fn emit_save(&mut self, slot: usize, result: &mut MenuKeyResult) {
        let name = self.save_slots[slot].name.clone();
        if self.quick_save_slot == QUICKSAVE_PICK {
            self.quick_save_slot = slot as i32;
        }
        self.clear_menus();
        result.events.push(MenuEvent::SaveGame {
            slot: slot as u8,
            name,
        });
    }

    // --- save-string editing ----------------------------------------------

    fn handle_save_input(
        &mut self,
        key: MenuKey,
        font: &HudFont,
        result: &mut MenuKeyResult,
    ) -> bool {
        let Some(input) = self.save_input.take() else {
            return false;
        };
        let slot = input.slot;
        match key {
            MenuKey::Backspace => {
                let mut input = input;
                if input.char_index > 0 {
                    input.char_index -= 1;
                    self.save_slots[slot].name.truncate(input.char_index);
                }
                self.save_input = Some(input);
            }
            MenuKey::Escape => {
                self.save_slots[slot].name = input.old_string;
                self.save_input = None;
            }
            MenuKey::Enter => {
                self.save_input = None;
                if !self.save_slots[slot].name.is_empty() {
                    result.sounds.push(MenuSound::SwitchOff);
                    self.emit_save(slot, result);
                }
            }
            MenuKey::Printable(raw) => {
                let mut input = input;
                let ch = raw.to_ascii_uppercase();
                let in_font =
                    ch == b' ' || (crate::hud::HU_FONTSTART..=crate::hud::HU_FONTEND).contains(&ch);
                if in_font
                    && (32..=127).contains(&ch)
                    && input.char_index < SAVESTRINGSIZE - 1
                    && crate::hud::text_width(font, self.save_slots[slot].name.as_bytes())
                        < (SAVESTRINGSIZE as i32 - 2) * 8
                {
                    self.save_slots[slot].name.push(ch as char);
                    input.char_index += 1;
                }
                self.save_input = Some(input);
            }
            // swallow everything else like vanilla does
            _ => {}
        }
        result.consumed = true;
        true
    }

    // --- item actions ------------------------------------------------------

    fn new_game(&mut self, result: &mut MenuKeyResult) {
        if self.context.netgame && !self.context.demoplayback {
            self.start_message(
                strings::NEWGAME_NET.to_string(),
                false,
                MessageResponse::AnyKey,
            );
            return;
        }
        let next = if self.context.gamemode.is_commercial() {
            MenuLevel::NewGame
        } else {
            MenuLevel::Episode
        };
        self.setup_next_menu(next);
        let _ = result;
    }

    fn episode(&mut self, choice: usize) {
        if self.context.gamemode == GameMode::Shareware && choice != 0 {
            // shareware: episodes 2+ lead to the order-now ad screen
            self.start_message(
                strings::SWSTRING.to_string(),
                false,
                MessageResponse::AnyKey,
            );
            self.setup_next_menu(MenuLevel::ReadThis1);
            return;
        }
        let mut choice = choice;
        if self.context.gamemode == GameMode::Registered && choice > 2 {
            // 4th episode forces the first one ("Ultimate DOOM required")
            choice = 0;
        }
        self.epi = choice as u8;
        self.setup_next_menu(MenuLevel::NewGame);
    }

    fn choose_skill(&mut self, choice: usize, result: &mut MenuKeyResult) {
        if choice as u8 == SKILL_NIGHTMARE {
            self.start_message(
                strings::NIGHTMARE.to_string(),
                true,
                MessageResponse::Nightmare,
            );
            return;
        }
        let episode = self.epi + 1;
        self.clear_menus();
        result.events.push(MenuEvent::NewGame {
            episode,
            map: 1,
            skill: choice as u8,
        });
    }

    fn load_game_entry(&mut self) {
        if self.context.netgame {
            self.start_message(strings::LOADNET.to_string(), false, MessageResponse::AnyKey);
            return;
        }
        self.setup_next_menu(MenuLevel::LoadGame);
    }

    fn load_select(&mut self, choice: usize, result: &mut MenuKeyResult) {
        self.clear_menus();
        result
            .events
            .push(MenuEvent::LoadGame { slot: choice as u8 });
    }

    fn save_game_entry(&mut self, result: &mut MenuKeyResult) {
        if !self.context.usergame {
            self.start_message(
                strings::SAVEDEAD.to_string(),
                false,
                MessageResponse::AnyKey,
            );
            return;
        }
        if !self.context.in_level {
            return;
        }
        self.setup_next_menu(MenuLevel::SaveGame);
        let _ = result;
    }

    fn save_select(&mut self, choice: usize) {
        let slot = choice;
        // entering editor: remember the old description, strip the
        // placeholder when the slot was never written
        let old_string = self.save_slots[slot].name.clone();
        if self.save_slots[slot].name == strings::EMPTY_SLOT {
            self.save_slots[slot].name.clear();
        }
        let char_index = self.save_slots[slot].name.len();
        self.save_input = Some(SaveInput {
            slot,
            char_index,
            old_string,
        });
    }

    fn sound_volume_entry(&mut self) {
        self.setup_next_menu(MenuLevel::Sound);
    }

    fn adjust_sfx_volume(&mut self, choice: usize, result: &mut MenuKeyResult) {
        match choice {
            0 => self.sfx_volume = (self.sfx_volume - 1).max(0),
            _ => self.sfx_volume = (self.sfx_volume + 1).min(15),
        }
        result.events.push(MenuEvent::SfxVolume(self.sfx_volume));
    }

    fn adjust_music_volume(&mut self, choice: usize, result: &mut MenuKeyResult) {
        match choice {
            0 => self.music_volume = (self.music_volume - 1).max(0),
            _ => self.music_volume = (self.music_volume + 1).min(15),
        }
        result
            .events
            .push(MenuEvent::MusicVolume(self.music_volume));
    }

    fn change_sensitivity(&mut self, choice: usize, result: &mut MenuKeyResult) {
        match choice {
            0 => self.mouse_sensitivity = (self.mouse_sensitivity - 1).max(0),
            _ => self.mouse_sensitivity = (self.mouse_sensitivity + 1).min(9),
        }
        result
            .events
            .push(MenuEvent::MouseSensitivity(self.mouse_sensitivity));
    }

    fn change_detail(&mut self, result: &mut MenuKeyResult) {
        self.detail_level ^= 1;
        result
            .events
            .push(MenuEvent::DetailLevel(self.detail_level));
    }

    fn change_messages(&mut self, result: &mut MenuKeyResult) {
        self.show_messages = !self.show_messages;
        result
            .events
            .push(MenuEvent::ShowMessages(self.show_messages));
    }

    fn size_display(&mut self, choice: usize, result: &mut MenuKeyResult) {
        match choice {
            0 => {
                if self.screen_size > 0 {
                    self.screen_blocks -= 1;
                    self.screen_size -= 1;
                }
            }
            _ => {
                if self.screen_size < 8 {
                    self.screen_blocks += 1;
                    self.screen_size += 1;
                }
            }
        }
        result
            .events
            .push(MenuEvent::ScreenBlocks(self.screen_blocks));
    }

    fn end_game_prompt(&mut self, result: &mut MenuKeyResult) {
        if !self.context.usergame {
            result.sounds.push(MenuSound::Oof);
            return;
        }
        if self.context.netgame {
            self.start_message(strings::NETEND.to_string(), false, MessageResponse::AnyKey);
            return;
        }
        self.start_message(strings::ENDGAME.to_string(), true, MessageResponse::EndGame);
    }

    fn quit_doom(&mut self) {
        let text = strings::quit_message(self.context.gametic, self.context.language_english);
        self.start_message(text, true, MessageResponse::Quit);
    }

    fn read_this(&mut self) {
        self.setup_next_menu(MenuLevel::ReadThis1);
    }

    fn finish_read_this(&mut self) {
        self.setup_next_menu(MenuLevel::Main);
    }

    /// Dispatches an item action. `choice` is the item index (or 0/1
    /// for arrow items: 0 = left, 1 = right).
    fn run_action(&mut self, action: MenuAction, choice: usize, result: &mut MenuKeyResult) {
        match action {
            MenuAction::None => {}
            MenuAction::NewGame => self.new_game(result),
            MenuAction::Options => self.setup_next_menu(MenuLevel::Options),
            MenuAction::LoadGame => self.load_game_entry(),
            MenuAction::LoadSelect => self.load_select(choice, result),
            MenuAction::SaveGame => self.save_game_entry(result),
            MenuAction::SaveSelect => self.save_select(choice),
            MenuAction::ReadThis => self.read_this(),
            MenuAction::ReadThis2 => self.setup_next_menu(MenuLevel::ReadThis2),
            MenuAction::FinishReadThis => self.finish_read_this(),
            MenuAction::QuitDoom => self.quit_doom(),
            MenuAction::Episode => self.episode(choice),
            MenuAction::ChooseSkill => self.choose_skill(choice, result),
            MenuAction::EndGame => self.end_game_prompt(result),
            MenuAction::ChangeMessages => self.change_messages(result),
            MenuAction::ChangeDetail => self.change_detail(result),
            MenuAction::SizeDisplay => self.size_display(choice, result),
            MenuAction::ChangeSensitivity => self.change_sensitivity(choice, result),
            MenuAction::Sound => self.sound_volume_entry(),
            MenuAction::SfxVolume => self.adjust_sfx_volume(choice, result),
            MenuAction::MusicVolume => self.adjust_music_volume(choice, result),
        }
    }

    // --- F-key entry points (called by the input layer / game loop) ------

    /// `KEY_F2` — open the save menu through the usual guards.
    pub fn open_save(&mut self) -> MenuKeyResult {
        self.start_control_panel();
        let mut result = MenuKeyResult::consumed();
        self.save_game_entry(&mut result);
        result.sounds.push(MenuSound::SwitchOn);
        result
    }

    /// `KEY_F3` — open the load menu through the usual guards.
    pub fn open_load(&mut self) -> MenuKeyResult {
        self.start_control_panel();
        self.load_game_entry();
        MenuKeyResult {
            consumed: true,
            events: Vec::new(),
            sounds: vec![MenuSound::SwitchOn],
        }
    }

    /// `KEY_F4` — open the sound volume menu, cursor on the sfx slider.
    pub fn open_sound(&mut self) -> MenuKeyResult {
        self.start_control_panel();
        self.level = MenuLevel::Sound;
        self.item_on = 0;
        MenuKeyResult {
            consumed: true,
            events: Vec::new(),
            sounds: vec![MenuSound::SwitchOn],
        }
    }

    /// `KEY_F1` — open the read-this page (page 2 on retail, page 1
    /// otherwise), like `M_Responder`.
    pub fn open_help(&mut self) -> MenuKeyResult {
        self.start_control_panel();
        self.level = if self.context.gamemode == GameMode::Retail {
            MenuLevel::ReadThis2
        } else {
            MenuLevel::ReadThis1
        };
        self.item_on = 0;
        MenuKeyResult {
            consumed: true,
            events: Vec::new(),
            sounds: vec![MenuSound::SwitchOn],
        }
    }

    /// `KEY_F5` — toggle detail level.
    pub fn toggle_detail(&mut self) -> MenuKeyResult {
        let mut result = MenuKeyResult::consumed();
        self.change_detail(&mut result);
        result.sounds.push(MenuSound::SwitchOn);
        result
    }

    /// `KEY_F7` — the end-game prompt (same as the options entry).
    pub fn end_game(&mut self) -> MenuKeyResult {
        let mut result = MenuKeyResult::consumed();
        self.end_game_prompt(&mut result);
        result.sounds.push(MenuSound::SwitchOn);
        result
    }

    /// `KEY_F8` — toggle player messages.
    pub fn toggle_messages(&mut self) -> MenuKeyResult {
        let mut result = MenuKeyResult::consumed();
        self.change_messages(&mut result);
        result.sounds.push(MenuSound::SwitchOn);
        result
    }

    /// `KEY_F10` — the quit prompt (same as the main-menu entry).
    pub fn quit_prompt(&mut self) -> MenuKeyResult {
        let mut result = MenuKeyResult::consumed();
        result.sounds.push(MenuSound::SwitchOn);
        self.quit_doom();
        result
    }

    /// `KEY_F6` — the quicksave flow.
    pub fn quick_save(&mut self) -> MenuKeyResult {
        let mut result = MenuKeyResult::consumed();
        if !self.context.usergame {
            result.sounds.push(MenuSound::Oof);
            return result;
        }
        if !self.context.in_level {
            return result;
        }
        if self.quick_save_slot < 0 {
            self.start_control_panel();
            self.level = MenuLevel::SaveGame;
            self.item_on = self.last_on[MenuLevel::SaveGame as usize];
            self.quick_save_slot = QUICKSAVE_PICK;
            return result;
        }
        let name = self.save_slots[self.quick_save_slot as usize].name.clone();
        self.start_message(
            strings::quick_save_prompt(&name),
            true,
            MessageResponse::QuickSave,
        );
        result
    }

    /// `KEY_F9` — the quickload flow.
    pub fn quick_load(&mut self) -> MenuKeyResult {
        let result = MenuKeyResult::consumed();
        if self.context.netgame {
            self.start_message(
                strings::QLOADNET.to_string(),
                false,
                MessageResponse::AnyKey,
            );
            return result;
        }
        if self.quick_save_slot < 0 {
            self.start_message(
                strings::QSAVESPOT.to_string(),
                false,
                MessageResponse::AnyKey,
            );
            return result;
        }
        let name = self.save_slots[self.quick_save_slot as usize].name.clone();
        self.start_message(
            strings::quick_load_prompt(&name),
            true,
            MessageResponse::QuickLoad,
        );
        result
    }

    // --- M_Responder --------------------------------------------------------

    /// Feeds one key to the menu. Returns what happened; when the menu
    /// is closed and the key is not Escape, `consumed` is false and the
    /// game layer takes over (same contract as vanilla's `M_Responder`
    /// return value).
    pub fn handle_key(&mut self, font: &HudFont, key: MenuKey) -> MenuKeyResult {
        let mut result = MenuKeyResult::unconsumed();

        // save-game string editing intercepts everything
        if self.save_input.is_some() {
            self.handle_save_input(key, font, &mut result);
            result.consumed = true;
            return result;
        }

        // a message prompt is up: it owns the keyboard
        if self.message.is_some() {
            let consumed = self.handle_message_key(key, &mut result);
            result.consumed = consumed;
            return result;
        }

        // menu is down: only Escape opens it
        if !self.active {
            match key {
                MenuKey::Escape => {
                    self.start_control_panel();
                    result.sounds.push(MenuSound::SwitchOn);
                    result.consumed = true;
                }
                _ => return MenuKeyResult::unconsumed(),
            }
            return result;
        }

        let count = self.item_count();
        match key {
            MenuKey::Down => {
                loop {
                    self.item_on = if self.item_on + 1 > count - 1 {
                        0
                    } else {
                        self.item_on + 1
                    };
                    if self.item_status(self.item_on) != ItemStatus::Spacer {
                        break;
                    }
                }
                result.sounds.push(MenuSound::PStop);
                result.consumed = true;
            }
            MenuKey::Up => {
                loop {
                    self.item_on = if self.item_on == 0 {
                        count - 1
                    } else {
                        self.item_on - 1
                    };
                    if self.item_status(self.item_on) != ItemStatus::Spacer {
                        break;
                    }
                }
                result.sounds.push(MenuSound::PStop);
                result.consumed = true;
            }
            MenuKey::Left => {
                if self.item_status(self.item_on) == ItemStatus::Arrows {
                    result.sounds.push(MenuSound::StnMov);
                    let action = self.items()[self.item_on].action;
                    self.run_action(action, 0, &mut result);
                }
                result.consumed = true;
            }
            MenuKey::Right => {
                if self.item_status(self.item_on) == ItemStatus::Arrows {
                    result.sounds.push(MenuSound::StnMov);
                    let action = self.items()[self.item_on].action;
                    self.run_action(action, 1, &mut result);
                }
                result.consumed = true;
            }
            MenuKey::Enter => {
                let status = self.item_status(self.item_on);
                if status != ItemStatus::Spacer && status != ItemStatus::Disabled {
                    self.last_on[self.level as usize] = self.item_on;
                    let action = self.items()[self.item_on].action;
                    if status == ItemStatus::Arrows {
                        result.sounds.push(MenuSound::StnMov);
                        self.run_action(action, 1, &mut result);
                    } else {
                        result.sounds.push(MenuSound::Pistol);
                        self.run_action(action, self.item_on, &mut result);
                    }
                }
                result.consumed = true;
            }
            MenuKey::Escape => {
                self.last_on[self.level as usize] = self.item_on;
                self.clear_menus();
                result.sounds.push(MenuSound::SwitchOff);
                result.consumed = true;
            }
            MenuKey::Backspace => {
                self.last_on[self.level as usize] = self.item_on;
                if let Some(prev) = self.level.prev_menu(self.context.gamemode.is_commercial()) {
                    self.setup_next_menu(prev);
                    result.sounds.push(MenuSound::SwitchOn);
                }
                result.consumed = true;
            }
            MenuKey::Printable(ch) => {
                // hotkey search, wrapping around like vanilla
                let lower = ch.to_ascii_lowercase();
                let found = (1..count)
                    .map(|off| (self.item_on + off) % count)
                    .chain(std::iter::once(self.item_on))
                    .find(|&i| {
                        self.items()[i].alpha_key == lower && self.items()[i].alpha_key != 0
                    });
                if let Some(i) = found {
                    self.item_on = i;
                    result.sounds.push(MenuSound::PStop);
                    result.consumed = true;
                }
            }
        }
        result
    }
}

/// Accessors for the renderer — the render half of the port lives in
/// [`super::render`].
impl Menu {
    pub(crate) fn level_items(&self) -> &[MenuItem] {
        self.items()
    }

    pub(crate) fn level_xy(&self) -> (i32, i32) {
        self.level_xy_effective()
    }

    pub(crate) fn item_on_value(&self) -> usize {
        self.item_on
    }

    pub(crate) fn which_skull_value(&self) -> usize {
        self.which_skull
    }

    pub(crate) fn save_input_slot(&self) -> Option<usize> {
        self.save_input.as_ref().map(|i| i.slot)
    }

    pub(crate) fn screen_size_value(&self) -> i32 {
        self.screen_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::patch::{Patch, decode_patch};
    use std::sync::Arc;

    // --- synthetic font: every glyph 8 px wide, 7 px tall ------------------

    fn build_patch_bytes(width: i16, height: i16, fill: u8) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        let table_start = data.len();
        data.resize(table_start + width as usize * 4, 0);
        for col in 0..width as usize {
            let column_offset = data.len() as i32;
            data[table_start + col * 4..table_start + (col + 1) * 4]
                .copy_from_slice(&column_offset.to_le_bytes());
            data.push(0);
            data.push(height as u8);
            data.push(0);
            data.extend_from_slice(&vec![fill; height as usize]);
            data.push(0);
            data.push(0xff);
        }
        data
    }

    fn synthetic_font() -> HudFont {
        let glyphs: [Arc<Patch>; crate::hud::HU_FONTSIZE] = std::array::from_fn(|i| {
            Arc::new(decode_patch(&build_patch_bytes(8, 7, 1 + i as u8)).unwrap())
        });
        HudFont::from_glyphs(glyphs)
    }

    // --- fixtures -----------------------------------------------------------

    fn retail_menu() -> Menu {
        Menu::new(
            MenuContext {
                gamemode: GameMode::Retail,
                netgame: false,
                usergame: true,
                in_level: true,
                demoplayback: false,
                gametic: 0,
                language_english: true,
            },
            MenuOptions::defaults(),
        )
    }

    fn commercial_menu() -> Menu {
        let mut ctx = MenuContext::commercial();
        ctx.usergame = true;
        ctx.in_level = true;
        Menu::new(ctx, MenuOptions::defaults())
    }

    fn key(menu: &mut Menu, font: &HudFont, key: MenuKey) -> MenuKeyResult {
        menu.handle_key(font, key)
    }

    fn open_menu(menu: &mut Menu, font: &HudFont) {
        let r = key(menu, font, MenuKey::Escape);
        assert!(r.consumed);
        assert!(menu.is_active());
    }

    fn enter(menu: &mut Menu, font: &HudFont) -> MenuKeyResult {
        key(menu, font, MenuKey::Enter)
    }

    fn type_text(menu: &mut Menu, font: &HudFont, text: &str) -> MenuKeyResult {
        let mut out = MenuKeyResult::unconsumed();
        for &b in text.as_bytes() {
            out = key(menu, font, MenuKey::Printable(b));
        }
        out
    }

    // --- basics ---------------------------------------------------------------

    #[test]
    fn fresh_menu_is_closed() {
        let menu = retail_menu();
        assert!(!menu.is_active());
        assert_eq!(menu.level(), MenuLevel::Main);
    }

    #[test]
    fn escape_opens_and_closes_menu() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        let r = key(&mut menu, &font, MenuKey::Escape);
        assert!(r.consumed);
        assert!(!menu.is_active());
        assert!(r.sounds.contains(&MenuSound::SwitchOff));
        // escape again reopens
        open_menu(&mut menu, &font);
    }

    #[test]
    fn keys_pass_through_when_menu_is_closed() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        for k in [MenuKey::Up, MenuKey::Enter, MenuKey::Printable(b'x')] {
            assert!(!key(&mut menu, &font, k).consumed);
        }
    }

    // --- cursor movement ------------------------------------------------------

    #[test]
    fn down_wraps_at_the_bottom() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        // retail main menu has 6 entries, cursor starts at 0
        for _ in 0..6 {
            key(&mut menu, &font, MenuKey::Down);
        }
        assert_eq!(menu.item_on(), 0);
    }

    #[test]
    fn up_wraps_at_the_top() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Up);
        assert_eq!(menu.item_on(), 5);
    }

    #[test]
    fn spacers_are_skipped_by_cursor() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        // jump to the options menu (hotkey), then walk down:
        // endgame(0) detail... messages(1) detail(2) scrnsize(3) -> mousesens(5) (skip 4)
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Options);
        key(&mut menu, &font, MenuKey::Down); // messages = 1
        key(&mut menu, &font, MenuKey::Down); // detail = 2
        key(&mut menu, &font, MenuKey::Down); // scrnsize = 3
        key(&mut menu, &font, MenuKey::Down); // mousesens = 5 (4 is a spacer)
        assert_eq!(menu.item_on(), 5);
        key(&mut menu, &font, MenuKey::Down); // m_svolv = 7 (6 is a spacer)
        assert_eq!(menu.item_on(), 7);
    }

    #[test]
    fn up_skips_spacer_backwards() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's')); // scrnsize = 3 (hotkey wraps)
        key(&mut menu, &font, MenuKey::Up); // wraps to sound(7)... 6 spacer skipped?
        // from 3 going up: 2 (detail)
        assert_eq!(menu.item_on(), 2);
    }

    // --- main menu entries ------------------------------------------------------

    #[test]
    fn retail_main_menu_has_six_entries() {
        let menu = retail_menu();
        assert_eq!(menu.level_items().len(), 6);
    }

    #[test]
    fn commercial_main_menu_drops_readthis() {
        let menu = commercial_menu();
        assert_eq!(menu.level_items().len(), 5);
        assert_eq!(menu.level_items()[4].name, "M_QUITG");
    }

    #[test]
    fn enter_options_opens_options_menu() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Options);
    }

    #[test]
    fn enter_read_this_opens_page1() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'r'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::ReadThis1);
        // and the "press to continue" item hands us page 2...
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::ReadThis2);
        // ...whose only item returns to the main menu
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Main);
    }

    #[test]
    fn hotkey_wraps_around_backwards() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'q')); // quitdoom = 5
        assert_eq!(menu.item_on(), 5);
        key(&mut menu, &font, MenuKey::Printable(b'n')); // wraps past end
        assert_eq!(menu.item_on(), 0);
    }

    // --- new game flow ------------------------------------------------------

    #[test]
    fn commercial_newgame_goes_straight_to_skill_select() {
        let font = synthetic_font();
        let mut menu = commercial_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::NewGame);
    }

    #[test]
    fn retail_newgame_goes_through_episode_select() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Episode);
    }

    #[test]
    fn picking_episode_then_skill_emits_new_game() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b't')); // episode 2 (index 1)... hotkey 't' wraps to first 't' = ep2
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::NewGame);
        // hurt me plenty (index 2) is the default cursor
        let r = enter(&mut menu, &font);
        assert_eq!(
            r.events,
            vec![MenuEvent::NewGame {
                episode: 2,
                map: 1,
                skill: 2
            }]
        );
        assert!(!menu.is_active());
    }

    #[test]
    fn nightmare_skill_requires_confirmation() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        enter(&mut menu, &font); // episode 1
        key(&mut menu, &font, MenuKey::Printable(b'n')); // nightmare (wraps to index 4)
        let r = enter(&mut menu, &font);
        // no event yet: the nightmare prompt is up
        assert!(r.events.is_empty());
        assert_eq!(menu.message().map(|m| m.needs_input), Some(true));
        // "n" declines, no new game
        let r = key(&mut menu, &font, MenuKey::Printable(b'n'));
        assert!(r.events.is_empty());
        // reopen is not needed to retrigger: menu closed after prompt
        assert!(!menu.is_active());
    }

    #[test]
    fn nightmare_prompt_y_starts_nightmare() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        enter(&mut menu, &font); // episode 1
        key(&mut menu, &font, MenuKey::Printable(b'n')); // nightmare
        enter(&mut menu, &font); // prompt
        let r = key(&mut menu, &font, MenuKey::Printable(b'y'));
        assert_eq!(
            r.events,
            vec![MenuEvent::NewGame {
                episode: 1,
                map: 1,
                skill: SKILL_NIGHTMARE
            }]
        );
    }

    #[test]
    fn shareware_episode_two_shows_ad_screen_then_help_page() {
        let font = synthetic_font();
        let mut ctx = MenuContext::commercial();
        ctx.gamemode = GameMode::Shareware;
        ctx.usergame = true;
        let mut menu = Menu::new(ctx, MenuOptions::defaults());
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Episode);
        // shareware only lists 3 episodes
        assert_eq!(menu.level_items().len(), 3);
        key(&mut menu, &font, MenuKey::Down); // episode 2
        enter(&mut menu, &font);
        // ad screen instead of the skill menu
        let msg = menu.message().expect("shareware ad message");
        assert!(!msg.needs_input);
        // dismiss with any key: back to... menu closes per vanilla
        key(&mut menu, &font, MenuKey::Printable(b'x'));
        assert_eq!(menu.level(), MenuLevel::ReadThis1);
    }

    #[test]
    fn registered_episode_four_forces_first() {
        let font = synthetic_font();
        let mut ctx = MenuContext::commercial();
        ctx.gamemode = GameMode::Registered;
        ctx.usergame = true;
        let mut menu = Menu::new(ctx, MenuOptions::defaults());
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'n'));
        enter(&mut menu, &font);
        // registered lists 3 episodes; 4th item is gone
        assert_eq!(menu.level_items().len(), 3);
    }

    #[test]
    fn quit_prompt_y_emits_quit() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'q'));
        enter(&mut menu, &font);
        assert!(menu.message().is_some());
        let r = key(&mut menu, &font, MenuKey::Printable(b'y'));
        assert_eq!(r.events, vec![MenuEvent::Quit]);
    }

    #[test]
    fn quit_prompt_n_closes_without_event() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'q'));
        enter(&mut menu, &font);
        let r = key(&mut menu, &font, MenuKey::Printable(b'n'));
        assert!(r.events.is_empty());
        assert!(!menu.is_active()); // vanilla quirk: answering closes the menu
    }

    #[test]
    fn prompt_is_not_closed_by_random_keys_when_needs_input() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'q'));
        enter(&mut menu, &font);
        for k in [
            MenuKey::Up,
            MenuKey::Down,
            MenuKey::Enter,
            MenuKey::Printable(b'x'),
        ] {
            assert!(!key(&mut menu, &font, k).consumed);
            assert!(menu.message().is_some());
        }
    }

    // --- options ------------------------------------------------------------

    #[test]
    fn detail_toggle_flips_and_emits() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'g')); // detail item
        let r = enter(&mut menu, &font);
        assert_eq!(r.events, vec![MenuEvent::DetailLevel(1)]);
        assert_eq!(menu.detail_level(), 1);
        let r = enter(&mut menu, &font);
        assert_eq!(r.events, vec![MenuEvent::DetailLevel(0)]);
    }

    #[test]
    fn messages_toggle_flips_and_emits() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'm'));
        let r = enter(&mut menu, &font);
        assert_eq!(r.events, vec![MenuEvent::ShowMessages(false)]);
        assert!(!menu.show_messages());
    }

    #[test]
    fn sensitivity_slider_clamps() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'm')); // first 'm' -> messages? hotkey order...
        // navigate directly to mousesens (index 5)
        menu.item_on = 5;
        for _ in 0..20 {
            let r = key(&mut menu, &font, MenuKey::Right);
            assert_eq!(
                r.events.last(),
                Some(&MenuEvent::MouseSensitivity(menu.mouse_sensitivity()))
            );
        }
        assert_eq!(menu.mouse_sensitivity(), 9);
        for _ in 0..20 {
            key(&mut menu, &font, MenuKey::Left);
        }
        assert_eq!(menu.mouse_sensitivity(), 0);
    }

    #[test]
    fn screen_blocks_slider_clamps_at_bounds() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        menu.item_on = 3; // scrnsize
        for _ in 0..20 {
            key(&mut menu, &font, MenuKey::Right);
        }
        assert_eq!(menu.screen_blocks(), 11);
        for _ in 0..20 {
            key(&mut menu, &font, MenuKey::Left);
        }
        assert_eq!(menu.screen_blocks(), 3);
    }

    #[test]
    fn sound_volume_sliders_adjust() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's')); // scrnsize? hotkey wraps: 's' hits scrnsize(3) or m_svolv(7)?
        // use direct path: navigate to sound item and enter
        menu.level = MenuLevel::Options;
        menu.item_on = 7; // m_svolv
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Sound);
        // sfx slider at index 0
        let r = key(&mut menu, &font, MenuKey::Right);
        assert_eq!(r.events, vec![MenuEvent::SfxVolume(9)]);
        let r = key(&mut menu, &font, MenuKey::Left);
        assert_eq!(r.events, vec![MenuEvent::SfxVolume(8)]);
        // music slider at index 2
        menu.item_on = 2;
        let r = key(&mut menu, &font, MenuKey::Right);
        assert_eq!(r.events, vec![MenuEvent::MusicVolume(9)]);
    }

    #[test]
    fn backspace_moves_to_previous_menu() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'o'));
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::Options);
        key(&mut menu, &font, MenuKey::Backspace);
        assert_eq!(menu.level(), MenuLevel::Main);
        // backspace on the main menu stays put but is consumed
        key(&mut menu, &font, MenuKey::Backspace);
        assert_eq!(menu.level(), MenuLevel::Main);
    }

    // --- save / load ----------------------------------------------------------

    fn armed_save_menu() -> Menu {
        let mut menu = retail_menu();
        menu.set_save_names(&[
            SaveSlotInfo {
                name: "E1M1 clear".into(),
                available: true,
            },
            SaveSlotInfo {
                name: "empty slot".into(),
                available: false,
            },
            SaveSlotInfo {
                name: "EMPTY SLOT".into(),
                available: true,
            },
            SaveSlotInfo::default(),
            SaveSlotInfo::default(),
            SaveSlotInfo::default(),
        ]);
        menu
    }

    #[test]
    fn save_requires_usergame_and_level() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        menu.set_context(MenuContext {
            usergame: false,
            ..MenuContext {
                gamemode: GameMode::Retail,
                netgame: false,
                usergame: false,
                in_level: true,
                demoplayback: false,
                gametic: 0,
                language_english: true,
            }
        });
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's')); // savegame item
        enter(&mut menu, &font);
        // SAVEDEAD message instead of the menu
        assert!(menu.message().is_some());
    }

    #[test]
    fn entering_save_slot_starts_string_editor() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's')); // save item
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::SaveGame);
        enter(&mut menu, &font); // pick slot 1 ("E1M1 clear")
        assert_eq!(menu.save_input_slot(), Some(0));
        // old string is preserved until commit
        assert_eq!(menu.save_names()[0].name, "E1M1 clear");
    }

    #[test]
    fn save_editor_types_and_commits() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's'));
        enter(&mut menu, &font);
        menu.item_on = 1; // slot 2, "empty slot" available=false for load but savable
        enter(&mut menu, &font);
        assert_eq!(menu.save_input_slot(), Some(1));
        // EMPTY slot opened: starts blank
        assert_eq!(menu.save_names()[1].name, "");
        type_text(&mut menu, &font, "my run");
        assert_eq!(menu.save_names()[1].name, "MY RUN"); // upper-cased
        let r = key(&mut menu, &font, MenuKey::Enter);
        assert_eq!(
            r.events,
            vec![MenuEvent::SaveGame {
                slot: 1,
                name: "MY RUN".into()
            }]
        );
        assert!(!menu.is_active());
    }

    #[test]
    fn save_editor_escape_restores_old_string() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's'));
        enter(&mut menu, &font);
        menu.item_on = 0;
        enter(&mut menu, &font);
        type_text(&mut menu, &font, "xyz");
        key(&mut menu, &font, MenuKey::Escape);
        assert_eq!(menu.save_names()[0].name, "E1M1 clear");
    }

    #[test]
    fn save_editor_backspace_deletes() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's'));
        enter(&mut menu, &font);
        menu.item_on = 0;
        enter(&mut menu, &font);
        type_text(&mut menu, &font, "!");
        assert_eq!(menu.save_names()[0].name, "E1M1 clear!");
        key(&mut menu, &font, MenuKey::Backspace);
        assert_eq!(menu.save_names()[0].name, "E1M1 clear");
    }

    #[test]
    fn save_editor_enforces_width_limit() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's'));
        enter(&mut menu, &font);
        menu.item_on = 1;
        enter(&mut menu, &font);
        // glyphs are 8 px wide; limit is (SAVESTRINGSIZE - 2) * 8 = 176 px => 22 chars
        type_text(&mut menu, &font, &"x".repeat(70));
        assert_eq!(menu.save_names()[1].name.len(), 22);
    }

    #[test]
    fn empty_save_string_does_not_commit() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b's'));
        enter(&mut menu, &font);
        menu.item_on = 1;
        enter(&mut menu, &font);
        let r = key(&mut menu, &font, MenuKey::Enter);
        assert!(r.events.is_empty()); // refused: no filename
        assert!(menu.save_input_slot().is_none());
    }

    #[test]
    fn unavailable_load_slot_is_not_selectable() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'l')); // load item
        enter(&mut menu, &font);
        assert_eq!(menu.level(), MenuLevel::LoadGame);
        menu.item_on = 1; // unavailable slot
        let r = enter(&mut menu, &font);
        assert!(r.events.is_empty());
        // but the available one loads
        menu.item_on = 0;
        let r = enter(&mut menu, &font);
        assert_eq!(r.events, vec![MenuEvent::LoadGame { slot: 0 }]);
    }

    #[test]
    fn netgame_blocks_load_with_notice() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        let mut ctx = menu.context().clone();
        ctx.netgame = true;
        menu.set_context(ctx);
        open_menu(&mut menu, &font);
        key(&mut menu, &font, MenuKey::Printable(b'l'));
        enter(&mut menu, &font);
        let msg = menu.message().expect("netgame notice");
        assert!(msg.text.contains("net game"));
        assert_eq!(menu.level(), MenuLevel::Main); // no level switch
    }

    // --- quicksave / quickload -------------------------------------------------

    #[test]
    fn quicksave_without_usergame_is_denied() {
        let mut menu = retail_menu();
        let mut ctx = menu.context().clone();
        ctx.usergame = false;
        menu.set_context(ctx);
        let r = menu.quick_save();
        assert!(r.events.is_empty());
        assert!(r.sounds.contains(&MenuSound::Oof));
    }

    #[test]
    fn quicksave_without_slot_picks_one() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        let r = menu.quick_save();
        assert!(r.events.is_empty());
        assert_eq!(menu.level(), MenuLevel::SaveGame);
        assert_eq!(menu.quick_save_slot(), QUICKSAVE_PICK);
        // pick slot 3, name it, commit: quick slot sticks
        menu.item_on = 2;
        enter(&mut menu, &font);
        type_text(&mut menu, &font, "quick one");
        key(&mut menu, &font, MenuKey::Enter);
        assert_eq!(menu.quick_save_slot(), 2);
    }

    #[test]
    fn quicksave_with_slot_prompts_and_saves() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        menu.quick_save_slot = 0;
        let r = menu.quick_save();
        assert!(r.events.is_empty());
        let msg = menu.message().expect("quicksave prompt");
        assert!(msg.text.contains("E1M1 clear"));
        let r = key(&mut menu, &font, MenuKey::Printable(b'y'));
        assert_eq!(
            r.events,
            vec![MenuEvent::SaveGame {
                slot: 0,
                name: "E1M1 clear".into()
            }]
        );
    }

    #[test]
    fn quickload_without_slot_shows_notice() {
        let mut menu = armed_save_menu();
        let r = menu.quick_load();
        let msg = menu.message().expect("no-slot notice");
        assert!(msg.text.contains("quicksave slot"));
        assert!(r.events.is_empty());
    }

    #[test]
    fn quickload_with_slot_prompts_and_loads() {
        let font = synthetic_font();
        let mut menu = armed_save_menu();
        menu.quick_save_slot = 2;
        let _ = menu.quick_load();
        assert!(menu.message().is_some());
        let r = key(&mut menu, &font, MenuKey::Printable(b'y'));
        assert_eq!(r.events, vec![MenuEvent::LoadGame { slot: 2 }]);
    }

    // --- endgame / f-keys ------------------------------------------------------

    #[test]
    fn endgame_requires_confirmation() {
        let font = synthetic_font();
        let mut menu = retail_menu();
        let _ = menu.end_game();
        assert_eq!(menu.message().map(|m| m.needs_input), Some(true));
        let r = key(&mut menu, &font, MenuKey::Printable(b'y'));
        assert_eq!(r.events, vec![MenuEvent::StartTitle]);
        assert!(r.sounds.contains(&MenuSound::SwitchOff));
    }

    #[test]
    fn endgame_denied_in_netgame() {
        let mut menu = retail_menu();
        let mut ctx = menu.context().clone();
        ctx.netgame = true;
        menu.set_context(ctx);
        menu.end_game();
        let msg = menu.message().expect("netgame endgame notice");
        assert!(msg.text.contains("netgame"));
    }

    #[test]
    fn f_keys_open_the_expected_pages() {
        let mut menu = retail_menu();
        menu.open_help();
        assert_eq!(menu.level(), MenuLevel::ReadThis2); // retail F1
        let mut menu = commercial_menu();
        menu.open_help();
        assert_eq!(menu.level(), MenuLevel::ReadThis1); // non-retail F1
        let mut menu = retail_menu();
        menu.open_sound();
        assert_eq!(menu.level(), MenuLevel::Sound);
        assert_eq!(menu.item_on(), 0);
        menu.open_load();
        assert_eq!(menu.level(), MenuLevel::LoadGame);
        menu.open_save();
        assert_eq!(menu.level(), MenuLevel::SaveGame);
    }

    #[test]
    fn toggle_messages_and_detail_emit_events() {
        let mut menu = retail_menu();
        let r = menu.toggle_messages();
        assert_eq!(r.events, vec![MenuEvent::ShowMessages(false)]);
        let r = menu.toggle_detail();
        assert_eq!(r.events, vec![MenuEvent::DetailLevel(1)]);
    }

    // --- skull animation --------------------------------------------------------

    #[test]
    fn skull_toggles_after_ten_tics() {
        let mut menu = retail_menu();
        let start = menu.which_skull();
        for _ in 0..10 {
            menu.tick(0);
        }
        assert_ne!(menu.which_skull(), start);
        for _ in 0..9 {
            menu.tick(0);
        }
        assert_eq!(menu.which_skull(), start);
    }

    #[test]
    fn tick_latches_gametic() {
        let mut menu = retail_menu();
        menu.tick(1234);
        assert_eq!(menu.context().gametic, 1234);
    }

    // --- quit message -----------------------------------------------------------

    #[test]
    fn quit_message_is_deterministic_per_gametic() {
        let a = strings::quit_message(0, true);
        let b = strings::quit_message(0, true);
        assert_eq!(a, b);
        assert_ne!(a, strings::quit_message(1, true));
        assert!(a.contains("(press y to quit)"));
        // non-English gets the plain QUITMSG
        assert!(strings::quit_message(0, false).starts_with("are you sure"));
    }
}

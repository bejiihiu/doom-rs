//! The game-loop core: global state, the ticker, the level bring-up, the
//! player think pass, the level-exit flow and the platform-facing API.
//! This is the port of `g_game.c`, `p_tick.c`, `p_user.c`, `p_setup.c`
//! and the application-shell parts of `d_main.c` that belong to the game
//! layer (not the windowing layer).
//!
//! Order of a tic, exactly like vanilla's `P_Ticker`:
//!
//! 1. Thinker reborns (dead players who asked to respawn).
//! 2. `gameaction` processing (`G_Ticker`'s `while (gameaction != ga_nothing)`).
//! 3. Ticcmd building for every playing slot (demo playback reads from
//!    the demo instead of the input).
//! 4. `BT_SPECIAL` buttons (pause/save).
//! 5. The state machine for the current `gamestate`:
//!    `GS_LEVEL` - `P_PlayerThink` for every playing slot, then the mobj
//!    thinkers, then the special thinkers, then `P_UpdateSpecials`, then
//!    `P_RespawnSpecials`, then the status bar / HUD / automap ticks.
//!    `GS_INTERMISSION` / `GS_FINALE` - their tickers, with events.
//!    `GS_DEMOSCREEN` - the page ticker.
//! 6. `leveltime += 1`. `gametic` is bumped by the outer frame loop.
//!
//! The event flow out of the simulation: gameplay code reports plain-data
//! events ([`enemy::GameEvent`], [`MoveEvent`], [`InterEvent`],
//! [`SpecialEvent`], [`IntermissionEvent`], [`FinaleEvent`], [`MenuEvent`]);
//! this module drains them in production order and applies them. Demo
//! determinism is preserved because all RNG consumption order is
//! identical to the original.

use std::path::PathBuf;

use crate::args::{DeathmatchMode, EngineArgs, NetArgs, Warp};
use crate::audio::{AudioConfig, Listener, SoundEngine, SoundOrigin};
use crate::automap::{AutomapInput, AutomapKey, AutomapResponse, AutomapState, PlayerMarker};
use crate::errors::{EngineError, EngineResult};
use crate::fixed::{FRACUNIT, Fixed};
use crate::hud::HudMessage;
use crate::info::{MOBJ_INFO, MobjType, STATES, StateAction, StateId};
use crate::inter::{self, InterContext, InterEvent};
use crate::intermission::{
    Intermission, IntermissionEvent, IntermissionInput, IntermissionPlayer, IntermissionSetup,
};
use crate::map::{Map, ThingSpawn};
use crate::menu::{
    CheatEvent, CheatTable, GameMode, Menu, MenuAssets, MenuContext, MenuEvent, MenuKey, MenuSound,
};
use crate::mobj::{MobjStore, Skill, SpawnOutcome, SpawnRules, SpawnSpec, SpawnZ};
use crate::movement::{self, MoveContext, MoveEvent};
use crate::music::MusicEngine;
use crate::player::{self, AmmoKind, MAXPLAYERS, Player, PlayerState, Power, TicCmd, Weapon};
use crate::render::IndexedFramebuffer;
use crate::resources::ResourceManager;
use crate::rng::DoomRng;
use crate::specials::{self, ButtonList, LineAction, SpecialContext, SpecialEvent, SwitchState};
use crate::stbar::{FaceInputs, FaceMachine, StatusBar, StatusState};
use crate::tables::{Angle, FINE_COSINE, FINE_SINE};
use crate::thinker::{Thinker, ThinkerId};
use crate::weapons;

use super::demo::{DemoPlayback, DemoRecording};
use super::input::{
    BT_ATTACK, BT_CHANGE, BT_SPECIAL, BT_SPECIALMASK, BT_USE, BT_WEAPONMASK, BT_WEAPONSHIFT,
    BTS_PAUSE, BTS_SAVEGAME, BTS_SAVEMASK, BTS_SAVESHIFT, TURBO_THRESHOLD, TicBuilder, TicCmdInput,
};
use super::render::SKY_FLAT;
use super::savegame::SaveBlock;

/// Vanilla `TICRATE`.
pub const TICRATE: i32 = 35;
/// Vanilla `MAXINTERMISSION...` - not needed, here for completeness.
pub const BACKUPTICS: i32 = 12;
/// `BODYQUESIZE` - corpse queue for respawn flushes.
const BODY_QUE_SIZE: usize = 32;
/// Vanilla `VIEWHEIGHT` in fixed point (41 units).
const VIEWHEIGHT: Fixed = player::VIEWHEIGHT;
/// Vanilla `CF_NOCLIP`.
const CF_NOCLIP: u32 = crate::player::CF_NOCLIP;
/// Vanilla `CF_GODMODE`.
const CF_GODMODE: u32 = crate::player::CF_GODMODE;
/// Vanilla `CF_NOMOMENTUM`.
const CF_NOMOMENTUM: u32 = crate::player::CF_NOMOMENTUM;
/// Vanilla `ANG90` in fine-angle units.
const ANG90: Angle = Angle::ANG90;
/// `MAXBOB` - 16 pixels of bob, in fixed point.
const MAXBOB: Fixed = Fixed::from_raw(0x100000);
/// `FINEANGLES` in fine units (vanilla `FINEANGLES`).
const FINE_ANGLES: usize = 8192;
/// `FINEMASK` in fine units.
const FINE_MASK: usize = FINE_ANGLES - 1;
/// `S_SARG_RUN1 ..= S_SARG_PAIN2` state range (fast-monster tics).
const S_SARG_RUN1: usize = StateId::S_SARG_RUN1 as usize;
/// See [`S_SARG_RUN1`].
const S_SARG_PAIN2: usize = StateId::S_SARG_PAIN2 as usize;
/// `ANG5` - five degrees in BAM units.
const ANG5: Angle = Angle::from_raw(ANG90.to_raw() / 18);

/// Par times for the DOOM 1 episodes (`pars` in `g_game.c`).
pub const PARS: [[i32; 10]; 4] = [
    [0; 10],
    [0, 30, 75, 120, 90, 165, 180, 180, 30, 165],
    [0, 90, 90, 90, 120, 90, 360, 240, 30, 170],
    [0, 90, 45, 90, 150, 90, 90, 165, 30, 135],
];

/// Par times for DOOM 2 (`cpars` in `g_game.c`).
pub const CPARS: [i32; 32] = [
    30, 90, 120, 120, 90, 150, 120, 120, 270, 90, // 1-10
    210, 150, 150, 150, 210, 150, 420, 150, 210, 150, // 11-20
    240, 150, 180, 150, 150, 300, 330, 420, 300, 180, // 21-30
    120, 30, // 31-32
];

/// The current gamestate, vanilla `gamestate_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameState {
    /// Playing or watching a level.
    Level,
    /// Between-maps stat screen.
    Intermission,
    /// End-of-episode text / art / cast call.
    Finale,
    /// Demo screen (title pic) - the loop's attract mode.
    DemoScreen,
}

impl GameState {
    /// The vanilla name of the state, for logging.
    pub const fn name(self) -> &'static str {
        match self {
            GameState::Level => "GS_LEVEL",
            GameState::Intermission => "GS_INTERMISSION",
            GameState::Finale => "GS_FINALE",
            GameState::DemoScreen => "GS_DEMOSCREEN",
        }
    }
}

/// The deferred action machinery, vanilla `gameaction_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameAction {
    Nothing,
    NewGame,
    LoadLevel,
    Completed,
    Victory,
    WorldDone,
    PlayDemo,
}

/// Where a net game is in its startup lifecycle.
///
/// Vanilla runs the setup arbitration (`D_ArbitrateNetStart`) inside
/// `D_CheckNetGame` before the world exists. We keep the same order but
/// drive it from the frame loop so the window stays alive and Esc can
/// abort the wait - see the `-net` docs in `args`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetPhase {
    /// `D_ArbitrateNetStart` is still running; the world is not loaded.
    WaitingForPlayers,
    /// Setup finished; the world runs through the net tic path.
    Playing,
}

/// The key bindings, in Doom key-code space. Defaults are the linuxdoom
/// `NORMALUNIX` set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyBindings {
    /// `key_right` (arrow right by default).
    pub key_right: u8,
    /// `key_left`.
    pub key_left: u8,
    /// `key_up`.
    pub key_up: u8,
    /// `key_down`.
    pub key_down: u8,
    /// `key_strafeleft`.
    pub key_strafeleft: u8,
    /// `key_straferight`.
    pub key_straferight: u8,
    /// `key_fire`.
    pub key_fire: u8,
    /// `key_use`.
    pub key_use: u8,
    /// `key_strafe`.
    pub key_strafe: u8,
    /// `key_speed`.
    pub key_speed: u8,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            key_right: keys::RIGHTARROW,
            key_left: keys::LEFTARROW,
            key_up: keys::UPARROW,
            key_down: keys::DOWNARROW,
            key_strafeleft: b',',
            key_straferight: b'.',
            key_fire: keys::RCTRL,
            key_use: b' ',
            key_strafe: keys::RALT,
            key_speed: keys::RSHIFT,
        }
    }
}

/// Startup configuration for [`Game`]. Mirrors what the command line and
/// `doom.cfg` provide in the original.
#[derive(Clone, Debug)]
pub struct GameConfig {
    /// The game mode the IWAD was identified as.
    pub game_mode: GameMode,
    /// Directory the save games are written to / read from.
    pub save_dir: PathBuf,
    /// Sound effect volume, `0..=15`.
    pub sfx_volume: i32,
    /// Music volume, `0..=15`.
    pub music_volume: i32,
    /// `0` = high detail, `1` = low detail.
    pub detail_level: i32,
    /// View size in vanilla `screenblocks` units, `3..=11`.
    pub screen_blocks: i32,
    /// Player messages on/off.
    pub show_messages: bool,
    /// Mouse sensitivity, `0..=9`.
    pub mouse_sensitivity: i32,
    /// Controller bindings.
    pub keys: KeyBindings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            game_mode: GameMode::Commercial,
            save_dir: PathBuf::from("."),
            sfx_volume: 8,
            music_volume: 8,
            detail_level: 0,
            screen_blocks: 9,
            show_messages: true,
            mouse_sensitivity: 5,
            keys: KeyBindings::default(),
        }
    }
}

/// The input events the platform layer feeds the engine, one per OS
/// event. Keyboard state is tracked by the engine from `KeyDown`/`KeyUp`;
/// mouse motion and joystick axes are held state sampled per tic; the
/// double-click and weapon-change logic all lives inside
/// [`TicBuilder`](super::input::TicBuilder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// A key (or button) was pressed, in Doom key-code space.
    KeyDown(u8),
    /// A key (or button) was released.
    KeyUp(u8),
    /// Momentary mouse motion in mickeys (unscaled).
    MouseMove { x: i32, y: i32 },
    /// Mouse buttons bitfield (`1` fire, `2` strafe, `4` forward).
    MouseButtons(u8),
    /// Joystick axes (held state).
    JoystickAxis { x: i32, y: i32 },
    /// Joystick buttons bitfield (`1` fire, `2` strafe, `4` use, `8` speed).
    JoystickButtons(u8),
}

/// Doom key codes the platform feeds into [`InputEvent`]. Vanilla
/// `doomdef.h` values.
pub mod keys {
    /// `KEY_TAB` - toggle the automap.
    pub const TAB: u8 = 0x09;
    /// `KEY_ESCAPE`.
    pub const ESCAPE: u8 = 27;
    /// `KEY_ENTER`.
    pub const ENTER: u8 = 13;
    /// `KEY_PAUSE`.
    pub const PAUSE: u8 = 0xff;
    /// `KEY_BACKSPACE`.
    pub const BACKSPACE: u8 = 127;
    /// `KEY_LEFTARROW`.
    pub const LEFTARROW: u8 = 0xac;
    /// `KEY_RIGHTARROW`.
    pub const RIGHTARROW: u8 = 0xae;
    /// `KEY_UPARROW`.
    pub const UPARROW: u8 = 0xad;
    /// `KEY_DOWNARROW`.
    pub const DOWNARROW: u8 = 0xaf;
    /// `KEY_F1`.
    pub const F1: u8 = 0x80 + 0x3b;
    /// `KEY_F2`.
    pub const F2: u8 = 0x80 + 0x3c;
    /// `KEY_F3`.
    pub const F3: u8 = 0x80 + 0x3d;
    /// `KEY_F4`.
    pub const F4: u8 = 0x80 + 0x3e;
    /// `KEY_F5`.
    pub const F5: u8 = 0x80 + 0x3f;
    /// `KEY_F6`.
    pub const F6: u8 = 0x80 + 0x40;
    /// `KEY_F8`. Not bound by the original game; used for the frame-stats overlay.
    pub const F8: u8 = 0x80 + 0x42;
    /// `KEY_F9`.
    pub const F9: u8 = 0x80 + 0x43;
    /// `KEY_F10`.
    pub const F10: u8 = 0x80 + 0x44;
    /// `KEY_F11`.
    pub const F11: u8 = 0x80 + 0x57;
    /// `KEY_F12`.
    pub const F12: u8 = 0x80 + 0x58;
    /// `KEY_RSHIFT`.
    pub const RSHIFT: u8 = 0x80 + 0x36;
    /// `KEY_RCTRL`.
    pub const RCTRL: u8 = 0x80 + 0x1d;
    /// `KEY_RALT`.
    pub const RALT: u8 = 0x80 + 0x38;
}

/// Vanilla `SAVEGAMENAME`.
const SAVEGAME_NAME: &str = "doomsav";
/// The `p_saveg`-compatible save slot count, for the menu.
pub const SAVE_SLOTS: usize = crate::menu::SAVE_SLOTS;

/// The big one: the whole running game. Owns every subsystem.
pub struct Game {
    // -- doomstat globals ----------------------------------------------
    pub(crate) gamestate: GameState,
    pub(crate) game_action: GameAction,
    pub(crate) gameskill: Skill,
    pub(crate) gameepisode: u8,
    pub(crate) gamemap: u8,
    pub(crate) gamemode: GameMode,
    pub(crate) consoleplayer: usize,
    pub(crate) displayplayer: usize,
    pub(crate) netgame: bool,
    pub(crate) deathmatch: bool,
    pub(crate) deathmatch_mode: DeathmatchMode,
    pub(crate) respawnmonsters: bool,
    pub(crate) fastparm: bool,
    pub(crate) nomonsters: bool,
    pub(crate) usergame: bool,
    pub(crate) paused: bool,
    pub(crate) demoplayback: bool,
    pub(crate) demorecording: bool,
    pub(crate) singledemo: bool,
    pub(crate) timingdemo: bool,
    pub(crate) secretexit: bool,
    pub(crate) level_timer: Option<i32>,
    // -- tics -----------------------------------------------------------
    pub(crate) gametic: i32,
    pub(crate) leveltime: i32,
    pub(crate) levelstarttic: i32,
    // -- players --------------------------------------------------------
    pub(crate) players: Vec<Player>,
    pub(crate) playeringame: [bool; MAXPLAYERS],
    // -- world ----------------------------------------------------------
    pub(crate) map: Option<Map>,
    pub(crate) store: MobjStore,
    pub(crate) rng: DoomRng,
    pub(crate) total_kills: i32,
    pub(crate) total_items: i32,
    pub(crate) total_secret: i32,
    pub(crate) scroll_lines: Vec<usize>,
    pub(crate) buttons: ButtonList,
    pub(crate) switches: specials::SwitchList,
    pub(crate) sector_sound_org: Vec<(Fixed, Fixed)>,
    pub(crate) body_que: [Option<ThinkerId>; BODY_QUE_SIZE],
    pub(crate) body_que_slot: usize,
    pub(crate) pending_thinkers: Vec<Thinker>,
    pub(crate) sound_flood: crate::enemy::SoundFlood,
    pub(crate) brain: crate::enemy::BrainState,
    // -- resources / audio ----------------------------------------------
    pub(crate) resources: ResourceManager,
    pub(crate) sound: SoundEngine,
    pub(crate) music: MusicEngine,
    pub(crate) genmidi: Option<crate::music::GenmidiBank>,
    pub(crate) music_volume: i32,
    pub(crate) sfx_volume: i32,
    pub(crate) map_number: i32,
    // -- render ----------------------------------------------------------
    pub(crate) render: super::render::RenderAssets,
    pub(crate) sky_texture: usize,
    pub(crate) sky_picnum: Option<u32>,
    pub(crate) sky_flat: [u8; 8],
    // -- st / hud / menu / automap ---------------------------------------
    pub(crate) font: crate::hud::HudFont,
    pub(crate) hud: crate::hud::Hud,
    pub(crate) status_bar: StatusBar,
    pub(crate) face: FaceMachine,
    pub(crate) menu: Menu,
    pub(crate) menu_assets: MenuAssets,
    pub(crate) automap: AutomapState,
    pub(crate) fps_overlay: crate::fps_overlay::FpsOverlay,
    // -- intermission / finale -------------------------------------------
    pub(crate) intermission: Option<Intermission>,
    pub(crate) finale: Option<crate::finale::Finale>,
    // -- demos -------------------------------------------------------------
    pub(crate) demo_playback: Option<DemoPlayback>,
    pub(crate) demo_recording: Option<DemoRecording>,
    pub(crate) demoname: String,
    pub(crate) record_pending: Option<String>,
    // -- wipes -----------------------------------------------------------
    pub(crate) wipe: Option<crate::wipe::ScreenWipe>,
    pub(crate) wipe_needed: bool,
    pub(crate) last_state: Option<GameState>,
    // -- input -----------------------------------------------------------
    pub(crate) gamekeydown: [bool; 256],
    pub(crate) mouse_buttons: u8,
    pub(crate) joy_buttons: u8,
    pub(crate) joy_x: i32,
    pub(crate) joy_y: i32,
    pub(crate) mouse_dx: i32,
    pub(crate) mouse_dy: i32,
    pub(crate) builder: TicBuilder,
    // -- cheats ----------------------------------------------------------
    pub(crate) cheats: CheatTable,
    // -- config ---------------------------------------------------------
    pub(crate) detail_level: i32,
    pub(crate) screen_blocks: i32,
    pub(crate) show_messages: bool,
    pub(crate) mouse_sensitivity: i32,
    pub(crate) message_force: bool,
    pub(crate) save_dir: PathBuf,
    /// `-turbo`, applied to the move table at startup.
    pub(crate) bindings: KeyBindings,
    // -- output ----------------------------------------------------------
    pub(crate) quit_requested: bool,
    // -- tick pacing ------------------------------------------------------
    pub(crate) tic_accumulator_ms: u64,
    // -- deferred actions -------------------------------------------------
    pub(crate) deferred: Option<(Skill, u8, u8)>,
    /// A map load failed; hard-stop the game on the next tick boundary.
    pub(crate) load_error: Option<String>,
    /// The 0-based map index to enter after the intermission.
    pub(crate) next_level_zero_based: i32,
    /// `savedescription` from the save menu (empty = default).
    pub(crate) savedescription: String,
    // -- net --------------------------------------------------------------
    /// The net engine (sockets, node list, tic buffers). `Some` iff `-net`
    /// was given. The platform never sees it; the tic governor and the
    /// wait-for-players screen do.
    pub(crate) net_engine: Option<crate::net::NetEngine>,
    /// Monotonic clock for the tic governor (vanilla's `I_GetTime`).
    /// Boxed so tests can inject a deterministic source.
    pub(crate) net_time: Box<dyn crate::net::TimeSource>,
    /// `consistancy[][]` from vanilla's `G_Ticker`; see the `net` module.
    pub(crate) consistency: crate::net::ConsistencyCheck,
    /// Startup lifecycle for a net game.
    pub(crate) net_phase: NetPhase,
    /// The per-frame platform input handed to `build_ticcmd` - needed
    /// because the net engine builds tics from `hooks.build_ticcmd()` with
    /// no arguments.
    pub(crate) net_platform_input: TicCmdInput,
    /// Mirror of the net engine's `maketic`: counts ticcmds built by the
    /// net path, used to read the consistency slot like vanilla's
    /// `consistancy[consoleplayer][maketic%BACKUPTICS]`.
    pub(crate) net_maketic: i32,
    /// The setup packet this node sends (key player) or receives
    /// (everyone else) at the end of the arbitration.
    pub(crate) net_settings: Option<crate::net::NetGameSettings>,
    /// Esc was pressed while waiting for peers; the ticket checks this
    /// through `NetGameHooks::check_abort_requested`.
    pub(crate) net_abort_requested: bool,
    /// Whether this is a network game (`-net` was given). Unlike
    /// `net_engine`, this flag stays set for the whole game — the tic
    /// governor borrows the engine out for every governor call.
    pub(crate) net_playing: bool,
    /// `ticdup` from `-dup` (1..=9); used for tic duplication and the
    /// consistency gate. 1 outside of net games.
    pub(crate) net_ticdup: i32,
    /// A tic failed inside the governor (`NetGameHooks::run_game_tic`
    /// cannot return an error); latched here and turned into a
    /// platform-visible failure after `try_run_tics` returns.
    pub(crate) net_fatal: Option<EngineError>,
}

impl Game {
    /// `D_DoomMain` + `G_InitNew` at the end: builds everything, then
    /// starts the requested mode of play from the arguments.
    pub fn new(
        config: GameConfig,
        resources: ResourceManager,
        args: &EngineArgs,
    ) -> EngineResult<Self> {
        // audio first: preload needs the resources
        let sfx_volume = config.sfx_volume.clamp(0, 15);
        let music_volume = config.music_volume.clamp(0, 15);
        let mut sound = SoundEngine::new(AudioConfig {
            num_channels: 8,
            sfx_volume: sfx_volume * 8,
            map_number: 0,
        });
        sound.preload_sounds(&resources)?;

        // genmidi bank (optional)
        let genmidi = resources
            .find_lump("GENMIDI")
            .and_then(|index| resources.lump_data_index(index).ok())
            .and_then(|lump| crate::music::GenmidiBank::parse(lump).ok())
            .map(Some)
            .unwrap_or_else(|| {
                tracing::warn!("GENMIDI lump missing or corrupt; using the fallback bank");
                None
            });

        let mut music = MusicEngine::new(crate::music::DEFAULT_SAMPLE_RATE);
        music.set_volume((music_volume * 8) as u8);

        // renderer assets
        let assets = super::render::RenderAssets::build(&resources)?;

        // ST/HU/menu assets
        let hud = crate::hud::Hud::load(&resources)?;
        let font = crate::hud::HudFont::load(&resources)?;
        let menu_assets = MenuAssets::load(&resources)?;
        let status_bar = StatusBar::load(&resources, 0)?;
        let fps_overlay = crate::fps_overlay::FpsOverlay::default();
        let automap = AutomapState::load(&resources).unwrap_or_else(|err| {
            tracing::warn!("automap assets not fully available: {err}; marks will not render");
            AutomapState::new()
        });

        let game_mode = config.game_mode;
        let menu = Menu::new(
            MenuContext {
                gamemode: game_mode,
                netgame: false,
                usergame: false,
                in_level: false,
                demoplayback: false,
                gametic: 0,
                language_english: true,
            },
            crate::menu::MenuOptions {
                mouse_sensitivity: config.mouse_sensitivity,
                show_messages: config.show_messages,
                detail_level: config.detail_level,
                screen_blocks: config.screen_blocks,
                sfx_volume,
                music_volume,
            },
        );

        // both branches are 1 for now: the vanilla switch table marks the
        // shareware/registered split per episode and that lands with the
        // real episode tables
        let switches = specials::SwitchList::new(1);

        let mut game = Game {
            gamestate: GameState::DemoScreen,
            game_action: GameAction::Nothing,
            gameskill: Skill::Medium,
            gameepisode: 1,
            gamemap: 1,
            gamemode: game_mode,
            consoleplayer: 0,
            displayplayer: 0,
            netgame: false,
            deathmatch: args.deathmatch != DeathmatchMode::Off,
            deathmatch_mode: args.deathmatch,
            respawnmonsters: args.respawn,
            fastparm: args.fast,
            nomonsters: args.no_monsters,
            usergame: false,
            paused: false,
            demoplayback: false,
            demorecording: false,
            singledemo: false,
            timingdemo: false,
            secretexit: false,
            level_timer: None,
            gametic: 0,
            leveltime: 0,
            levelstarttic: 0,
            // vanilla zero-inits players[] to PST_LIVE (d_main.c); the
            // PST_REBORN state is only set by G_InitNew / G_PlayerReborn,
            // and the spawn path flips it back to PST_LIVE. starting as
            // Reborn would make the reborn pass fire on the very first tic
            // and clobber the deferred NewGame action with LoadLevel.
            players: (0..MAXPLAYERS)
                .map(|_| {
                    let mut player = player_reborn_default();
                    player.playerstate = PlayerState::Live;
                    player
                })
                .collect(),
            playeringame: [true, false, false, false],
            map: None,
            store: MobjStore::new(),
            rng: DoomRng::new(),
            total_kills: 0,
            total_items: 0,
            total_secret: 0,
            scroll_lines: Vec::new(),
            buttons: ButtonList::new(),
            switches,
            sector_sound_org: Vec::new(),
            body_que: [None; BODY_QUE_SIZE],
            body_que_slot: 0,
            pending_thinkers: Vec::new(),
            sound_flood: crate::enemy::SoundFlood::new(0),
            brain: crate::enemy::BrainState::new(),
            resources,
            sound,
            music,
            genmidi,
            music_volume,
            sfx_volume,
            map_number: 0,
            render: assets,
            sky_texture: 0,
            sky_picnum: None,
            sky_flat: SKY_FLAT,
            font,
            hud,
            status_bar,
            fps_overlay,
            face: FaceMachine::new(),
            menu,
            menu_assets,
            automap,
            intermission: None,
            finale: None,
            demo_playback: None,
            demo_recording: None,
            demoname: String::new(),
            record_pending: None,
            wipe: None,
            wipe_needed: false,
            last_state: None,
            gamekeydown: [false; 256],
            mouse_buttons: 0,
            joy_buttons: 0,
            joy_x: 0,
            joy_y: 0,
            mouse_dx: 0,
            mouse_dy: 0,
            builder: TicBuilder::new(args.turbo),
            cheats: CheatTable::new(),
            detail_level: config.detail_level & 1,
            screen_blocks: config.screen_blocks.clamp(3, 11),
            show_messages: config.show_messages,
            mouse_sensitivity: config.mouse_sensitivity.clamp(0, 9),
            message_force: false,
            save_dir: config.save_dir,
            bindings: config.keys,
            quit_requested: false,
            tic_accumulator_ms: 0,
            deferred: None,
            load_error: None,
            next_level_zero_based: 0,
            savedescription: String::new(),
            net_engine: None,
            net_time: Box::new(crate::net::InstantTimeSource::new()),
            consistency: crate::net::ConsistencyCheck::new(),
            net_phase: NetPhase::Playing,
            net_platform_input: TicCmdInput::NONE,
            net_maketic: 0,
            net_settings: None,
            net_abort_requested: false,
            net_fatal: None,
            net_playing: false,
            net_ticdup: 1,
        };

        // network game: bind sockets now, arbitrate setup before the
        // world starts - vanilla's D_CheckNetGame happens before
        // G_InitNew for the same reason
        if let Some(net_args) = args.net.clone() {
            game.init_net(&net_args, args)?;
        }

        // start the mode of play requested by the command line
        if game.net_engine.is_none() {
            if let Some(demo) = &args.play_demo {
                game.defer_play_demo(demo);
            } else if let Some(demo) = &args.time_demo {
                game.timingdemo = true;
                game.defer_play_demo(demo);
            } else {
                let autostart =
                    args.skill.is_some() || args.warp.is_some() || args.record.is_some();
                if autostart {
                    let skill = args
                        .skill
                        .and_then(Skill::from_level)
                        .unwrap_or(Skill::Medium);
                    let (episode, map) = warp_to_episode_map(game.gamemode, args.warp);
                    game.defer_new_game(skill, episode, map);
                }
                if let Some(name) = &args.record {
                    game.start_recording(name);
                }
            }
        } else {
            // the world starts once the setup arbitration finishes and the
            // players know the shared skill/episode/map
            tracing::info!("net: waiting for players (esc to abort)");
        }

        // save games already on disk feed the menu's slot list
        refresh_save_slots(&mut game);

        Ok(game)
    }

    /// Bind sockets and prepare the net game (`-net`). The world starts
    /// once the setup arbitration completes - vanilla's `D_CheckNetGame`
    /// happens before `G_InitNew` for the same reason. ESC aborts the wait.
    fn init_net(&mut self, net_args: &NetArgs, args: &EngineArgs) -> EngineResult<()> {
        if args.play_demo.is_some() || args.time_demo.is_some() || args.record.is_some() {
            return Err(EngineError::Config(
                "-record/-playdemo/-timedemo are not supported in a net game".into(),
            ));
        }
        let (console_player, peers) = match net_args {
            NetArgs::Vanilla {
                console_player,
                hosts,
            } => ((*console_player).saturating_sub(1), hosts.clone()),
            NetArgs::SharedList { hosts } => {
                if hosts.is_empty() {
                    return Err(EngineError::Config(
                        "-net expects at least one node address".into(),
                    ));
                }
                let slot = hosts
                    .iter()
                    .position(|addr| addr.port() == args.net_port)
                    .ok_or_else(|| {
                        EngineError::Config(format!(
                            "-net: this machine is not in the node list (no entry bound on port \
                             {}); pick a unique -port and list it here",
                            args.net_port
                        ))
                    })?;
                let peers = hosts
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != slot)
                    .map(|(_, addr)| *addr)
                    .collect();
                (slot, peers)
            }
        };
        // the key player sends these; everyone else applies what it receives
        let (episode, map) = warp_to_episode_map(self.gamemode, args.warp);
        let settings = crate::net::NetGameSettings {
            // our skill levels are 1..=5; 3 is -skill 3 ("medium"), the default
            skill: i32::from(args.skill.unwrap_or(3)),
            deathmatch: i32::from(args.deathmatch_level()),
            nomonsters: args.no_monsters,
            respawn: args.respawn,
            // the setup packet packs episode*64+map into one byte
            episode: i32::from(episode).clamp(0, 3),
            map: i32::from(map).clamp(1, 63),
        };
        let config = crate::net::NetConfig {
            console_player,
            ticdup: args.tic_dup,
            extratics: args.extratics,
            hosts: peers,
            port: args.net_port,
        };
        let engine = crate::net::NetEngine::start(config).map_err(EngineError::from)?;
        self.net_engine = Some(engine);
        self.net_settings = Some(settings);
        self.net_phase = NetPhase::WaitingForPlayers;
        self.consoleplayer = console_player;
        self.displayplayer = console_player;
        self.net_playing = true;
        self.net_ticdup = args.tic_dup.clamp(1, 9);
        Ok(())
    }

    /// Whether this is a network game (`-net` was given).
    pub fn is_net_game(&self) -> bool {
        self.net_playing
    }

    /// `D_ArbitrateNetStart`, one step per rendered frame. The key player
    /// sends the setup packet until every peer acknowledged; everyone else
    /// waits for the key player's setup packet. `None` = still waiting.
    fn net_arbitration_step(&mut self) -> EngineResult<Option<crate::net::NetGameSettings>> {
        let settings = self
            .net_settings
            .ok_or_else(|| EngineError::Resource("net settings missing".into()))?;
        let mut engine = self
            .net_engine
            .take()
            .ok_or_else(|| EngineError::Resource("net engine missing".into()))?;
        let result = engine.arbitration_step(self, &settings);
        self.net_engine = Some(engine);
        Ok(result?)
    }

    /// The arbitration finished: apply the shared settings and start the
    /// game (vanilla's `D_CheckNetGame` tail, then `G_InitNew`).
    fn finish_net_game(&mut self, settings: crate::net::NetGameSettings) {
        self.gameskill =
            Skill::from_level(settings.skill.clamp(1, 5) as u8).unwrap_or(Skill::Medium);
        self.deathmatch_mode = match settings.deathmatch {
            1 => DeathmatchMode::Duel,
            2 => DeathmatchMode::Alt,
            _ => DeathmatchMode::Off,
        };
        self.deathmatch = settings.deathmatch != 0;
        self.nomonsters = settings.nomonsters;
        self.respawnmonsters = settings.respawn;
        // mark the in-game players through the hooks (vanilla's numplayers loop)
        if let Some(engine) = self.net_engine.take() {
            engine.finish_check_net_game(self);
            self.net_engine = Some(engine);
        }
        self.netgame = true;
        self.net_phase = NetPhase::Playing;
        // vanilla: the menu refuses almost everything in a netgame
        self.menu.set_context(MenuContext {
            gamemode: self.gamemode,
            netgame: true,
            usergame: true,
            in_level: true,
            demoplayback: false,
            gametic: self.gametic as u64,
            language_english: true,
        });
        let episode = settings.episode.clamp(1, 4) as u8;
        let map = settings.map.clamp(1, 63) as u8;
        tracing::info!("net: synchronized, starting E{episode}M{map}");
        self.defer_new_game(self.gameskill, episode, map);
    }

    /// Leave the network game politely (vanilla's `D_QuitNetGame`): tell the
    /// surviving nodes we are out, a few times, so nobody waits on us.
    /// No-op outside a net game or before the world started.
    pub fn net_quit(&mut self) {
        let Some(mut engine) = self.net_engine.take() else {
            return;
        };
        engine.quit_net_game(self);
        // the engine is consumed here; the game is on its way out anyway
        tracing::info!("net: left the game");
    }

    /// One frame in a net game: pump arbitration until everyone synced,
    /// then hand the tic governor the wall-clock pacing it needs. The
    /// normal `elapsed_ms` accumulator is unused - the governor paces from
    /// the monotonic clock, like vanilla's `TryRunTics`.
    pub(crate) fn advance_net(
        &mut self,
        input: &TicCmdInput,
        elapsed_ms: u64,
        fb: &mut IndexedFramebuffer,
    ) -> EngineResult<()> {
        self.net_platform_input = *input;
        // the governor ignores wall time, but the overlay is wall-clock;
        // record the platform delta so net games get local telemetry too
        self.fps_overlay.record_frame(elapsed_ms);

        if self.net_phase == NetPhase::WaitingForPlayers {
            if let Some(settings) = self.net_arbitration_step()? {
                self.finish_net_game(settings);
            }
            super::render::render_frame(fb, self)?;
            self.update_sounds();
            return Ok(());
        }

        // take the engine and the clock out so the hooks (which borrow the
        // whole game) can be passed to the governor; both go back right after
        let mut engine = self
            .net_engine
            .take()
            .ok_or_else(|| EngineError::Resource("net engine missing".into()))?;
        let time = std::mem::replace(
            &mut self.net_time,
            Box::new(crate::net::InstantTimeSource::new()),
        );
        let outcome = crate::net::try_run_tics(&mut engine, time.as_ref(), self);
        self.net_time = time;
        self.net_engine = Some(engine);

        if let Some(fatal) = self.net_fatal.take() {
            return Err(fatal);
        }
        outcome.map_err(EngineError::from)?;

        super::render::render_frame(fb, self)?;
        self.update_sounds();
        Ok(())
    }

    /// Whether the player asked to quit. The platform shell exits its
    /// main loop when this turns true.
    pub fn should_quit(&self) -> bool {
        self.quit_requested
    }

    /// Whether the F8 debug overlay (frame stats) is currently enabled.
    /// Purely a render-side flag; it never influences the simulation.
    pub fn show_fps(&self) -> bool {
        self.fps_overlay.enabled()
    }

    /// The current gamestate (vanillas `gamestate`).
    pub fn gamestate(&self) -> GameState {
        self.gamestate
    }

    /// Current tic counter (vanilla `gametic`).
    pub fn gametic(&self) -> i32 {
        self.gametic
    }

    /// Current tic counter within the level (vanilla `leveltime`).
    pub fn leveltime(&self) -> i32 {
        self.leveltime
    }

    /// Total kills of the level so far (for the intermission).
    pub fn total_kills(&self) -> i32 {
        self.total_kills
    }

    /// Total items of the level so far.
    pub fn total_items(&self) -> i32 {
        self.total_items
    }

    /// Total secrets found so far.
    pub fn secrets_found(&self) -> i32 {
        self.total_secret
    }

    /// One frame, driven by the platform: one game tic plus a rendered
    /// frame, then the sound update. This is the headless/test path -
    /// demos use it exclusively; net games error out (they are driven by
    /// the net tic governor through [`Game::advance`]).
    pub fn tick_input(
        &mut self,
        input: &TicCmdInput,
        fb: &mut IndexedFramebuffer,
    ) -> EngineResult<()> {
        if self.net_playing {
            return Err(EngineError::Config(
                "tick_input is the headless single-player path; net games run through advance()"
                    .into(),
            ));
        }
        let merged = self.sample_local_input(input);
        self.run_tic(&merged)?;
        // no wall clock on this path (headless/demo tics); use the nominal
        // tic length so the overlay still gets sensible samples
        self.fps_overlay.record_frame(1000 / TICRATE as u64);
        super::render::render_frame(fb, self)?;
        self.update_sounds();
        Ok(())
    }

    /// One frame with wall-clock pacing: `elapsed_ms` milliseconds of
    /// game time are accumulated and as many tics as fit run (at most 4,
    /// so the engine never spirals). Exactly one frame is rendered at
    /// the end, like vanilla's `D_DoomLoop`. In a net game this routes to
    /// the net tic governor instead (`TryRunTics`); `elapsed_ms` is then
    /// unused.
    pub fn advance(
        &mut self,
        input: &TicCmdInput,
        elapsed_ms: u64,
        fb: &mut IndexedFramebuffer,
    ) -> EngineResult<()> {
        if self.net_playing {
            return self.advance_net(input, elapsed_ms, fb);
        }
        const MS_PER_TIC: u64 = 1000 / TICRATE as u64;
        let merged = self.sample_local_input(input);
        self.tic_accumulator_ms = self.tic_accumulator_ms.saturating_add(elapsed_ms);
        let mut ran = 0;
        while self.tic_accumulator_ms >= MS_PER_TIC && ran < 4 {
            self.tic_accumulator_ms -= MS_PER_TIC;
            self.run_tic(&merged)?;
            ran += 1;
        }
        self.fps_overlay.record_frame(elapsed_ms);
        super::render::render_frame(fb, self)?;
        self.update_sounds();
        Ok(())
    }

    /// Feed one OS-level input event to the engine. Returns `true` when
    /// the event was swallowed (menu, cheat, automap, ...). The held
    /// state that feeds [`TicCmdInput`] is tracked internally and merged
    /// with the platform's per-tic input at [`Game::tick_input`] time.
    pub fn handle_event(&mut self, event: &InputEvent) -> bool {
        // while the net setup runs the screen is modal: Esc aborts the
        // sync, everything else is swallowed
        if self.net_phase == NetPhase::WaitingForPlayers {
            if let InputEvent::KeyDown(key) = *event {
                if key == keys::ESCAPE {
                    self.gamekeydown[keys::ESCAPE as usize] = true;
                    self.net_abort_requested = true;
                }
            }
            return true;
        }
        match *event {
            InputEvent::KeyDown(key) => self.key_down(key),
            InputEvent::KeyUp(key) => {
                if (key as usize) < 256 {
                    self.gamekeydown[key as usize] = false;
                }
                false
            }
            InputEvent::MouseMove { x, y } => {
                // vanilla applies the sensitivity when sampling, not here
                self.mouse_dx = self.mouse_dx.saturating_add(x);
                self.mouse_dy = self.mouse_dy.saturating_add(y);
                true
            }
            InputEvent::MouseButtons(buttons) => {
                self.mouse_buttons = buttons;
                true
            }
            InputEvent::JoystickAxis { x, y } => {
                self.joy_x = x;
                self.joy_y = y;
                true
            }
            InputEvent::JoystickButtons(buttons) => {
                self.joy_buttons = buttons;
                true
            }
        }
    }

    /// Whether the automap is currently up.
    pub fn automap_active(&self) -> bool {
        self.automap.is_active()
    }

    /// Shared handle to the SFX mixer, for the platform's output backend.
    /// The game mixes sound into this mixer every frame; handing the same
    /// handle to a device stream plays it as it would a console port.
    pub fn sound_mixer(&self) -> crate::audio::SharedMixer {
        self.sound.mixer()
    }

    /// The music engine, for the platform to drain at its own clock.
    /// The game decides *which* track plays; the platform decides *when*
    /// the samples reach the output device (`pull_samples`).
    pub fn music_engine(&mut self) -> &mut crate::music::MusicEngine {
        &mut self.music
    }

    /// Direct access to the player's current place in the world - for
    /// tooling and tests (agent 39's determinism harness).
    pub fn player_position(&self) -> Option<(Fixed, Fixed, Angle)> {
        let avatar = self.players[self.consoleplayer].mo?;
        let mo = self.store.mobj(avatar)?;
        Some((mo.x, mo.y, mo.angle))
    }

    // ==================================================================
    // input
    // ==================================================================

    /// Fold the platform's held-key state and momentary mouse deltas
    /// into the local tic command. Consumes the mouse deltas like
    /// vanilla's `mousex = mousey = 0` reset.
    pub(crate) fn sample_local_input(&mut self, input: &TicCmdInput) -> TicCmdInput {
        // merge platform-held state with the OS-event mirror
        let mut merged = *input;
        let held = |k: u8| self.gamekeydown[k as usize];
        let b = &self.bindings;
        merged.forward |= held(b.key_up);
        merged.backward |= held(b.key_down);
        merged.turn_left |= held(b.key_left);
        merged.turn_right |= held(b.key_right);
        merged.strafe_left |= held(b.key_strafeleft);
        merged.strafe_right |= held(b.key_straferight);
        merged.strafe |= held(b.key_strafe)
            || (self.mouse_buttons & crate::game::input::MOUSE_STRAFE) != 0
            || (self.joy_buttons & crate::game::input::JOY_STRAFE) != 0;
        merged.speed |=
            held(b.key_speed) || (self.joy_buttons & crate::game::input::JOY_SPEED) != 0;
        merged.attack |= held(b.key_fire)
            || (self.mouse_buttons & crate::game::input::MOUSE_FIRE) != 0
            || (self.joy_buttons & crate::game::input::JOY_FIRE) != 0;
        merged.use_button |=
            held(b.key_use) || (self.joy_buttons & crate::game::input::JOY_USE) != 0;
        merged.mouse_buttons |= self.mouse_buttons;
        merged.joy_buttons |= self.joy_buttons;
        // mouse motion accumulated since the last tic, scaled like
        // vanilla (`mousex*(sensitivity+5)/10`), and consumed
        let scale = |v: i32| v * (self.mouse_sensitivity + 5) / 10;
        merged.mouse_x += scale(self.mouse_dx);
        merged.mouse_y += scale(self.mouse_dy);
        merged.joy_x += self.joy_x;
        merged.joy_y += self.joy_y;
        self.mouse_dx = 0;
        self.mouse_dy = 0;
        merged
    }

    fn key_down(&mut self, key: u8) -> bool {
        // menu first, like vanilla's M_Responder
        if self.menu_key(key) {
            return true;
        }
        if key as usize >= 256 {
            return false;
        }
        // demo loop / title: any key pops up the menu
        if self.game_action == GameAction::Nothing
            && (self.demoplayback || self.gamestate == GameState::DemoScreen)
            && !self.singledemo
        {
            self.menu.start_control_panel();
            return true;
        }
        match self.gamestate {
            GameState::Level => {
                // F12: spy mode (cycle the display player)
                if key == keys::F12 && !self.deathmatch {
                    loop {
                        self.displayplayer = (self.displayplayer + 1) % MAXPLAYERS;
                        if self.playeringame[self.displayplayer]
                            || self.displayplayer == self.consoleplayer
                        {
                            break;
                        }
                    }
                    return true;
                }
                // cheats (ST_Responder analog)
                if self.cheat_key(key) {
                    return true;
                }
                // automap (AM_Responder analog)
                if self.automap_key(key, true) {
                    return true;
                }
            }
            GameState::Finale => {
                // F_Responder: the cast call kills on keypress
                if let Some(finale) = self.finale.as_mut() {
                    if finale.on_keydown(&crate::game::GameCastTables::new()) {
                        // play any kill sound the tables asked for
                        let events = finale.drain_events();
                        self.apply_finale_events(events);
                        return true;
                    }
                }
            }
            _ => {}
        }
        if key == keys::PAUSE {
            self.builder.request_pause();
            return true;
        }
        self.gamekeydown[key as usize] = true;
        false
    }

    fn menu_key(&mut self, key: u8) -> bool {
        // F-keys map to menu entry points, the rest goes through
        // handle_key as a MenuKey
        match key {
            keys::F1 => {
                let _ = self.menu.open_help();
                return true;
            }
            keys::F2 => {
                let _ = self.menu.open_save();
                return true;
            }
            keys::F3 => {
                let _ = self.menu.open_load();
                return true;
            }
            keys::F4 => {
                let _ = self.menu.open_sound();
                return true;
            }
            keys::F5 => {
                let _ = self.menu.toggle_detail();
                return true;
            }
            keys::F6 => {
                let _ = self.menu.quick_save();
                return true;
            }
            keys::F8 => {
                // local debug tool; has no business with demos or net games,
                // it just flips a render flag
                self.fps_overlay.toggle();
                return true;
            }
            keys::F9 => {
                let _ = self.menu.quick_load();
                return true;
            }
            keys::F10 => {
                let _ = self.menu.quit_prompt();
                return true;
            }
            _ => {}
        }
        let Some(menu_key) = MenuKey::from_doom_key(key) else {
            return false;
        };
        let result = self.menu.handle_key(&self.font, menu_key);
        let consumed = result.consumed;
        self.apply_menu_events(result.events);
        self.play_menu_sounds(&result.sounds);
        consumed
    }

    fn cheat_key(&mut self, key: u8) -> bool {
        let ctx = crate::menu::CheatContext {
            gamemode: self.gamemode,
            netgame: self.netgame,
            deathmatch: self.deathmatch,
        };
        let events = self.cheats.check(key, ctx);
        if events.is_empty() {
            return false;
        }
        let console = self.consoleplayer;
        for event in events {
            match event {
                CheatEvent::GodMode => {
                    let player = &mut self.players[console];
                    player.cheats ^= CF_GODMODE;
                    player.message = Some(
                        if player.cheats & CF_GODMODE != 0 {
                            HudMessage::StDqModeOn.as_str()
                        } else {
                            HudMessage::StDqModeOff.as_str()
                        }
                        .to_string(),
                    );
                }
                CheatEvent::AmmoFull => self.give_ammo_full(console, true),
                CheatEvent::AmmoNoKeys => self.give_ammo_full(console, false),
                CheatEvent::NoClip => {
                    let player = &mut self.players[console];
                    player.cheats ^= CF_NOCLIP;
                    player.message = Some(
                        if player.cheats & CF_NOCLIP != 0 {
                            HudMessage::StNoClipOn.as_str()
                        } else {
                            HudMessage::StNoClipOff.as_str()
                        }
                        .to_string(),
                    );
                }
                CheatEvent::PowerUp(which) => {
                    let player = &mut self.players[console];
                    let slot = match which {
                        crate::menu::CheatPowerUp::Invulnerability => {
                            Power::Invulnerability as usize
                        }
                        crate::menu::CheatPowerUp::Strength => Power::Strength as usize,
                        crate::menu::CheatPowerUp::Invisibility => Power::Invisibility as usize,
                        crate::menu::CheatPowerUp::IronFeet => Power::IronFeet as usize,
                        crate::menu::CheatPowerUp::AllMap => Power::AllMap as usize,
                        crate::menu::CheatPowerUp::Infrared => Power::Infrared as usize,
                    };
                    let duration = match which {
                        crate::menu::CheatPowerUp::Invulnerability => 30 * TICRATE,
                        crate::menu::CheatPowerUp::Invisibility => 60 * TICRATE,
                        crate::menu::CheatPowerUp::IronFeet => 60 * TICRATE,
                        crate::menu::CheatPowerUp::Infrared => 120 * TICRATE,
                        crate::menu::CheatPowerUp::AllMap => 1,
                        crate::menu::CheatPowerUp::Strength => 1,
                    };
                    player.powers[slot] = if player.powers[slot] > 0 { 0 } else { duration };
                    player.message = Some(HudMessage::StBeholdX.as_str().to_string());
                }
                CheatEvent::BeholdMenu => {
                    self.players[console].message = Some(HudMessage::StBehold.as_str().to_string());
                }
                CheatEvent::Choppers => {
                    let player = &mut self.players[console];
                    player.weaponowned[Weapon::Chainsaw as usize] = true;
                    player.powers[Power::Invulnerability as usize] = 30 * TICRATE;
                    player.pendingweapon = Some(Weapon::Chainsaw);
                    player.message = Some(HudMessage::StChoppers.as_str().to_string());
                }
                CheatEvent::MyPos => {
                    let msg = match self.player_position() {
                        Some((x, y, angle)) => {
                            let degrees = ((angle.to_raw() as u64 * 360) >> 32) as i32;
                            format!(
                                "Position ({}, {}) - {} degrees",
                                x.to_int(),
                                y.to_int(),
                                degrees
                            )
                        }
                        None => "Position unknown".to_string(),
                    };
                    self.players[console].message = Some(msg);
                }
                CheatEvent::Warp { episode, map } => {
                    self.defer_new_game(self.gameskill, episode, map);
                }
                CheatEvent::Automap => {
                    self.automap.cycle_iddt();
                }
                CheatEvent::Music { digits } => {
                    self.change_music_by_digits(digits);
                    self.players[console].message =
                        Some(HudMessage::StMusicChange.as_str().to_string());
                }
            }
        }
        true
    }

    fn give_ammo_full(&mut self, console: usize, with_keys: bool) {
        let commercial = self.gamemode.is_commercial();
        let not_shareware = self.gamemode != GameMode::Shareware;
        let player = &mut self.players[console];
        player.armorpoints = 200;
        player.armortype = 2;
        for kind in 0..AmmoKind::COUNT {
            player.ammo[kind] = player.maxammo[kind];
        }
        for index in 0..weapons::WEAPON_INFO.len().min(player::NUM_WEAPONS) {
            if (index == Weapon::PlasmaRifle as usize || index == Weapon::Bfg as usize)
                && !not_shareware
            {
                continue;
            }
            if index == Weapon::SuperShotgun as usize && !commercial {
                continue;
            }
            player.weaponowned[index] = true;
        }
        if with_keys {
            for index in 0..crate::player::NUM_CARDS {
                player.cards[index] = true;
            }
        }
        player.message = Some(
            if with_keys {
                HudMessage::StKfaAdded.as_str()
            } else {
                HudMessage::StFaAdded.as_str()
            }
            .to_string(),
        );
    }

    fn change_music_by_digits(&mut self, digits: [u8; 2]) {
        let a = digits[0].saturating_sub(b'0');
        let b = digits[1].saturating_sub(b'0');
        if a > 9 || b > 9 {
            return;
        }
        let music_id = if self.gamemode.is_commercial() {
            let index = (a * 10 + b) as usize;
            if index == 0 || index > 32 {
                return;
            }
            // mus_runnin is a contiguous run of 32 tracks from id 33
            crate::info::ALL_MUSIC[32 + index]
        } else {
            let episode = match self.gameepisode {
                0 => 1,
                e => e,
            };
            let index = (episode as usize - 1) * 9 + b as usize;
            if a == 0 || b == 0 || b > 9 {
                return;
            }
            if index >= 27 {
                return;
            }
            crate::info::ALL_MUSIC[1 + index]
        };
        self.change_music(music_id);
    }

    fn automap_key(&mut self, key: u8, down: bool) -> bool {
        let mapped = match key {
            keys::TAB => None, // toggle handled separately
            b'=' => Some(AutomapKey::ZoomIn),
            b'-' => Some(AutomapKey::ZoomOut),
            b'0' => Some(AutomapKey::GoBig),
            b'f' => Some(AutomapKey::ToggleFollow),
            b'g' => Some(AutomapKey::ToggleGrid),
            b'm' => Some(AutomapKey::Mark),
            b'c' => Some(AutomapKey::ClearMarks),
            keys::LEFTARROW => Some(AutomapKey::PanLeft),
            keys::RIGHTARROW => Some(AutomapKey::PanRight),
            keys::UPARROW => Some(AutomapKey::PanUp),
            keys::DOWNARROW => Some(AutomapKey::PanDown),
            _ => return false,
        };
        match (key, down) {
            (keys::TAB, true) => {
                let response = if self.automap.is_active() {
                    self.automap.stop();
                    AutomapResponse::default()
                } else {
                    let console = self.consoleplayer;
                    let me = self.players[console]
                        .mo
                        .and_then(|id| self.store.mobj(id))
                        .map(|mo| PlayerMarker {
                            x: mo.x,
                            y: mo.y,
                            angle: mo.angle,
                            invisible: self.players[console].powers[Power::Invisibility as usize]
                                > 0,
                            is_local: true,
                        });
                    match (me, self.map.as_ref()) {
                        (Some(me), Some(map)) => {
                            self.automap.start(map, me);
                            AutomapResponse::default()
                        }
                        _ => AutomapResponse::default(),
                    }
                };
                let _ = response;
                true
            }
            (_, _) => {
                let Some(mapped) = mapped else { return false };
                let input = if down {
                    AutomapInput::KeyDown(mapped)
                } else {
                    AutomapInput::KeyUp(mapped)
                };
                let response = self.automap.handle_input(input);
                if let Some(message) = response.message {
                    self.players[self.consoleplayer].message = Some(message);
                }
                response.handled
            }
        }
    }

    // ==================================================================
    // audio helpers
    // ==================================================================

    /// `S_StartSound(NULL, sfx)` - full volume, no position.
    fn sound_start_global(&mut self, sfx: crate::info::SfxId) {
        let listener = self.listener();
        let _ = self.sound.start_sound(None, sfx, &mut self.rng, &listener);
    }

    /// `S_StartSound(&sector->soundorg, sfx)`.
    fn sound_start_sector(&mut self, sector: usize, sfx: crate::info::SfxId) {
        let listener = self.listener();
        let origin = self.sector_sound_origin(sector);
        let _ = self
            .sound
            .start_sound(Some(origin), sfx, &mut self.rng, &listener);
    }

    /// `S_StartSound(mobj, sfx)`.
    fn sound_start_actor(&mut self, actor: ThinkerId, sfx: crate::info::SfxId) {
        let listener = self.listener();
        let origin = self.actor_sound_origin(actor);
        let _ = self
            .sound
            .start_sound(Some(origin), sfx, &mut self.rng, &listener);
    }

    fn listener(&self) -> Listener {
        let console = self.consoleplayer;
        let Some(handle) = self.players[console].mo else {
            return Listener {
                x: Fixed::ZERO,
                y: Fixed::ZERO,
                angle: Angle::ZERO,
                origin_id: None,
            };
        };
        let Some(mo) = self.store.mobj(handle) else {
            return Listener {
                x: Fixed::ZERO,
                y: Fixed::ZERO,
                angle: Angle::ZERO,
                origin_id: None,
            };
        };
        let id = (u64::from(handle.generation()) << 32) | handle.index() as u64;
        Listener {
            x: mo.x,
            y: mo.y,
            angle: mo.angle,
            origin_id: Some(id),
        }
    }

    fn actor_sound_origin(&self, actor: ThinkerId) -> SoundOrigin {
        match self.store.mobj(actor) {
            Some(mo) => SoundOrigin::new(
                mo.x,
                mo.y,
                (u64::from(actor.generation()) << 32) | actor.index() as u64,
            ),
            None => SoundOrigin::new(Fixed::ZERO, Fixed::ZERO, 0),
        }
    }

    fn sector_sound_origin(&self, sector: usize) -> SoundOrigin {
        let (x, y) = self
            .sector_sound_org
            .get(sector)
            .copied()
            .unwrap_or((Fixed::ZERO, Fixed::ZERO));
        SoundOrigin::new(x, y, 0x8000_0000_0000_0000 | sector as u64)
    }

    /// `S_UpdateSounds(player->mo)`, once per frame.
    fn update_sounds(&mut self) {
        let listener = self.listener();
        self.sound.update_sounds(&listener);
    }

    /// `S_ChangeMusic(mnum, looping)` with the level music computed
    /// from the vanilla tables.
    fn change_music_by_level(&mut self) {
        let music_id = if self.gamemode.is_commercial() {
            let index = self.gamemap as usize;
            if index == 0 {
                crate::info::MusicId::mus_runnin
            } else {
                crate::info::ALL_MUSIC[crate::info::MusicId::mus_runnin as usize + index - 1]
            }
        } else if self.gameepisode < 4 {
            let index = (self.gameepisode as usize - 1) * 9 + self.gamemap as usize - 1;
            crate::info::ALL_MUSIC[crate::info::MusicId::mus_e1m1 as usize + index.min(26)]
        } else {
            const SP_MUS: [crate::info::MusicId; 9] = [
                crate::info::MusicId::mus_e3m4,
                crate::info::MusicId::mus_e3m2,
                crate::info::MusicId::mus_e3m3,
                crate::info::MusicId::mus_e1m5,
                crate::info::MusicId::mus_e2m7,
                crate::info::MusicId::mus_e2m4,
                crate::info::MusicId::mus_e2m6,
                crate::info::MusicId::mus_e2m5,
                crate::info::MusicId::mus_e1m9,
            ];
            SP_MUS[(self.gamemap as usize).saturating_sub(1).min(8)]
        };
        self.change_music(music_id);
    }

    fn change_music(&mut self, music_id: crate::info::MusicId) {
        let Some(name) = music_id.lump_name() else {
            return;
        };
        let Some(index) = self.resources.find_lump(&String::from_utf8_lossy(&name)) else {
            tracing::warn!(
                "music lump for {:?} not found; keeping silence",
                music_id.name()
            );
            return;
        };
        let Ok(lump) = self.resources.lump_data_index(index) else {
            return;
        };
        let result = match &self.genmidi {
            Some(bank) => self.music.play_mus(lump, bank),
            None => self.music.play_mus_fallback(lump),
        };
        if let Err(err) = result {
            tracing::warn!("music for {:?} failed to play: {err}", music_id.name());
        }
    }

    fn play_menu_sounds(&mut self, sounds: &[MenuSound]) {
        for sound in sounds {
            let sfx = match sound {
                MenuSound::PStop => crate::info::SfxId::sfx_pstop,
                MenuSound::StnMov => crate::info::SfxId::sfx_stnmov,
                MenuSound::Pistol => crate::info::SfxId::sfx_pistol,
                MenuSound::SwitchOff => crate::info::SfxId::sfx_swtchx,
                MenuSound::SwitchOn => crate::info::SfxId::sfx_swtchn,
                MenuSound::Oof => crate::info::SfxId::sfx_oof,
            };
            self.sound_start_global(sfx);
        }
    }

    // ==================================================================
    // menu event application
    // ==================================================================

    fn apply_menu_events(&mut self, events: Vec<MenuEvent>) {
        for event in events {
            match event {
                MenuEvent::Quit => self.quit_requested = true,
                MenuEvent::StartTitle => {
                    // vanilla D_StartTitle: goto the demo loop
                    self.gamestate = GameState::DemoScreen;
                    self.game_action = GameAction::Nothing;
                    self.wipe_needed = true;
                }
                MenuEvent::NewGame {
                    episode,
                    map,
                    skill,
                } => {
                    let skill = Skill::from_level(skill + 1).unwrap_or(Skill::Medium);
                    self.defer_new_game(skill, episode, map);
                }
                MenuEvent::LoadGame { slot } => {
                    self.load_game_slot(slot as usize);
                }
                MenuEvent::SaveGame { slot, name } => {
                    self.save_game_slot(slot as usize, &name);
                }
                MenuEvent::SfxVolume(volume) => {
                    self.sfx_volume = volume;
                    let _ = self.sound.set_sfx_volume(volume * 8);
                }
                MenuEvent::MusicVolume(volume) => {
                    self.music_volume = volume;
                    self.music.set_volume((volume * 8) as u8);
                }
                MenuEvent::MouseSensitivity(sensitivity) => {
                    self.mouse_sensitivity = sensitivity;
                }
                MenuEvent::DetailLevel(detail) => {
                    self.detail_level = detail & 1;
                }
                MenuEvent::ScreenBlocks(blocks) => {
                    self.screen_blocks = blocks.clamp(3, 11);
                }
                MenuEvent::ShowMessages(show) => {
                    self.show_messages = show;
                    self.message_force = true;
                    self.players[self.consoleplayer].message = Some(
                        if show {
                            HudMessage::MsgOn.as_str()
                        } else {
                            HudMessage::MsgOff.as_str()
                        }
                        .to_string(),
                    );
                }
            }
        }
    }
}

/// A fresh reborn player (vanilla `G_PlayerReborn`).
fn player_reborn_default() -> Player {
    let mut player = Player {
        mo: None,
        playerstate: PlayerState::Reborn,
        cmd: crate::player::TicCmd::default(),
        viewz: Fixed::default(),
        viewheight: VIEWHEIGHT,
        deltaviewheight: Fixed::default(),
        bob: Fixed::default(),
        health: player::MAXHEALTH,
        armorpoints: 0,
        armortype: 0,
        powers: [0; crate::player::NUM_POWERS],
        cards: [false; crate::player::NUM_CARDS],
        backpack: false,
        frags: [0; MAXPLAYERS],
        readyweapon: Weapon::Pistol,
        pendingweapon: Some(Weapon::Pistol),
        weaponowned: [false; crate::player::NUM_WEAPONS],
        ammo: [0; crate::player::NUM_AMMO],
        maxammo: inter::MAX_AMMO,
        attackdown: true,
        usedown: true,
        cheats: 0,
        refire: 0,
        killcount: 0,
        itemcount: 0,
        secretcount: 0,
        message: None,
        damagecount: 0,
        bonuscount: 0,
        attacker: None,
        extralight: 0,
        fixedcolormap: 0,
        colormap: 0,
        psprites: [crate::player::PspDef::default(); player::NUM_PSPRITES],
        didsecret: false,
    };
    player.weaponowned[Weapon::Fist as usize] = true;
    player.weaponowned[Weapon::Pistol as usize] = true;
    player.ammo[AmmoKind::Clip as usize] = 50;
    player
}

// ==================================================================
// game actions (vanilla `gameaction_t`)
// ==================================================================

impl Game {
    /// `G_DeferedInitNew` - the deferred new-game request (menu, cheat,
    /// `-warp` on the command line).
    fn defer_new_game(&mut self, skill: Skill, episode: u8, map: u8) {
        self.game_action = GameAction::NewGame;
        self.deferred = Some((skill, episode, map));
    }

    /// `G_DeferedPlayDemo` - the deferred demo playback request.
    fn defer_play_demo(&mut self, name: &str) {
        self.game_action = GameAction::PlayDemo;
        self.demoname = name.to_string();
        self.singledemo = true;
    }

    /// Begin demo recording with a pending new game (the demo header is
    /// written once the level is up).
    fn start_recording(&mut self, name: &str) {
        self.record_pending = Some(name.to_string());
    }

    /// `G_DoPlayDemo` - load the demo, verify the version, and start
    /// the game with its header settings.
    fn do_play_demo(&mut self) {
        self.game_action = GameAction::Nothing;

        // the demo can be a plain file or a WAD lump, like -playdemo
        let bytes = match std::fs::read(&self.demoname) {
            Ok(bytes) => bytes,
            Err(_) => {
                let upper = self.demoname.to_ascii_uppercase();
                match self.resources.find_lump(&upper) {
                    Some(index) => match self.resources.lump_data_index(index) {
                        Ok(bytes) => bytes.to_vec(),
                        Err(err) => {
                            tracing::error!("demo {} could not be read: {err}", self.demoname);
                            return;
                        }
                    },
                    None => {
                        tracing::error!("demo {} not found", self.demoname);
                        return;
                    }
                }
            }
        };

        let demo = match DemoPlayback::new(bytes) {
            Ok(demo) => demo,
            Err(err) => {
                tracing::error!("{err}");
                return;
            }
        };
        let header = demo.header();
        self.demo_playback = Some(demo);

        self.gameskill = Skill::from_level(header.skill + 1).unwrap_or(Skill::Medium);
        self.gameepisode = header.episode.max(1);
        self.gamemap = header.map.max(1);
        self.deathmatch_mode = match header.deathmatch {
            0 => DeathmatchMode::Off,
            1 => DeathmatchMode::Duel,
            _ => DeathmatchMode::Alt,
        };
        self.deathmatch = header.deathmatch != 0;
        self.respawnmonsters = header.respawn_monsters != 0;
        self.fastparm = header.fast_monsters != 0;
        self.nomonsters = header.no_monsters != 0;
        self.consoleplayer = (header.console_player as usize).min(MAXPLAYERS - 1);
        self.playeringame = header.playeringame.map(|v| v != 0);
        if self.playeringame[1] {
            self.netgame = true;
        }

        self.usergame = false;
        self.init_new(self.gameskill, self.gameepisode, self.gamemap);
        self.usergame = false;
        self.demoplayback = true;
    }

    /// `G_DoNewGame` - reset the one-way demo/net state and start fresh.
    fn do_new_game(&mut self) {
        self.demoplayback = false;
        if self.net_playing {
            // net game: the node list and the player list were set up by the
            // arbitration (finish_net_game); do not clobber them
        } else {
            self.playeringame = [true, false, false, false];
            self.netgame = false;
            self.consoleplayer = 0;
        }
        self.secretexit = false;
        if let Some((skill, episode, map)) = self.deferred.take() {
            self.init_new(skill, episode, map);
        } else {
            self.init_new(self.gameskill, self.gameepisode, self.gamemap);
        }
        self.game_action = GameAction::Nothing;
    }

    /// `G_InitNew` - clamp and apply the requested skill/episode/map,
    /// reseed the RNG and load the level.
    fn init_new(&mut self, skill: Skill, episode: u8, map: u8) {
        if self.paused {
            self.paused = false;
        }

        let mut skill = skill;
        if skill as u8 > Skill::Nightmare as u8 {
            skill = Skill::Nightmare;
        }

        let mut episode = episode;
        if episode < 1 {
            episode = 1;
        }
        match self.gamemode {
            GameMode::Retail => episode = episode.min(4),
            GameMode::Shareware => episode = episode.min(1),
            _ => episode = episode.min(3),
        }

        let mut map = map;
        if map < 1 {
            map = 1;
        }
        if map > 9 && !self.gamemode.is_commercial() {
            map = 9;
        }

        self.rng.reset(); // M_ClearRandom
        self.respawnmonsters = self.respawnmonsters || skill == Skill::Nightmare;
        // -fast on the nightmare transition is handled through the SARG
        // tic-doubling in the mobj pass (the state table is immutable);
        // projectile-speed doubling is not applied (documented in the
        // module docs)

        // force players to be initialized on first level load
        for i in 0..MAXPLAYERS {
            self.players[i].playerstate = PlayerState::Reborn;
        }

        self.usergame = true;
        self.paused = false;
        self.demoplayback = false;
        self.automap.stop();
        self.gameepisode = episode;
        self.gamemap = map;
        self.gameskill = skill;

        // the deferred demo recording starts with the first tic
        if let Some(name) = self.record_pending.take() {
            let header = crate::game::demo::DemoHeader {
                skill: skill as u8,
                episode,
                map,
                deathmatch: self.deathmatch_mode as u8,
                respawn_monsters: u8::from(self.respawnmonsters),
                fast_monsters: u8::from(self.fastparm),
                no_monsters: u8::from(self.nomonsters),
                console_player: self.consoleplayer as u8,
                playeringame: self.playeringame.map(u8::from),
            };
            self.demo_recording = Some(crate::game::demo::DemoRecording::new(&name, &header));
            self.demoname = name;
            self.demorecording = true;
        }

        self.do_load_level();
    }

    /// `G_DoLoadLevel` - wipe request, player reborns, the level scan,
    /// view setup.
    fn do_load_level(&mut self) {
        // sky texture by gamemode/episode/map
        self.levelstarttic = self.gametic;
        if self.last_state == Some(GameState::Level) {
            self.wipe_needed = true; // force a wipe when coming from a level
        }
        self.gamestate = GameState::Level;

        for i in 0..MAXPLAYERS {
            if self.playeringame[i] && self.players[i].playerstate == PlayerState::Dead {
                self.players[i].playerstate = PlayerState::Reborn;
            }
            self.players[i].frags = [0; MAXPLAYERS];
        }

        self.init_sky();
        self.setup_level(self.gameepisode, self.gamemap);
        self.displayplayer = self.consoleplayer;
        self.game_action = GameAction::Nothing;

        // clear the cmd building state (vanilla clears the same globals)
        self.builder.reset();
        self.gamekeydown = [false; 256];
        self.mouse_buttons = 0;
        self.joy_buttons = 0;
        self.joy_x = 0;
        self.joy_y = 0;
        self.mouse_dx = 0;
        self.mouse_dy = 0;
    }

    /// `R_InitSkyMap` + the sky texture pick from `G_DoLoadLevel`.
    fn init_sky(&mut self) {
        let commercial = self.gamemode.is_commercial();
        let name: &[u8; 8] = if commercial {
            if self.gamemap < 12 {
                b"SKY1\0\0\0\0"
            } else if self.gamemap < 21 {
                b"SKY2\0\0\0\0"
            } else {
                b"SKY3\0\0\0\0"
            }
        } else {
            match self.gameepisode {
                1 => b"SKY1\0\0\0\0",
                2 => b"SKY2\0\0\0\0",
                3 => b"SKY3\0\0\0\0",
                _ => b"SKY4\0\0\0\0",
            }
        };
        self.sky_texture = self.render.textures.lookup_name(name).unwrap_or(0);
        self.sky_picnum = self.render.flats.num_for_name(&SKY_FLAT);
    }

    /// `P_SetupLevel` - the map-load scan.
    fn setup_level(&mut self, episode: u8, map_num: u8) {
        let commercial = self.gamemode.is_commercial();
        self.total_kills = 0;
        self.total_items = 0;
        self.total_secret = 0;
        for i in 0..MAXPLAYERS {
            self.players[i].killcount = 0;
            self.players[i].itemcount = 0;
            self.players[i].secretcount = 0;
        }

        // the sentinel viewz forces onground=false until P_CalcHeight runs
        self.players[self.consoleplayer].viewz = Fixed::from_raw(1);

        // stop all playing sounds before the world goes away
        self.sound.stop_all_sounds();
        self.map_number = map_num as i32;
        self.sound.set_map_number(self.map_number);

        // find the map marker lump
        let mapname = if commercial {
            format!("MAP{map_num:02}")
        } else {
            format!("E{episode}M{map_num}")
        };
        let Some(marker) = self.resources.find_lump(&mapname) else {
            self.fatal_error(format!("map lump {mapname} not found"));
            return;
        };

        // the map lumps follow the marker in vanilla order
        let lump = |offset: usize| self.resources.lump_data_index(marker + offset);
        const ML_THINGS: usize = 1;
        const ML_LINEDEFS: usize = 2;
        const ML_SIDEDEFS: usize = 3;
        const ML_VERTEXES: usize = 4;
        const ML_SEGS: usize = 5;
        const ML_SSECTORS: usize = 6;
        const ML_NODES: usize = 7;
        const ML_SECTORS: usize = 8;
        const ML_REJECT: usize = 9;
        const ML_BLOCKMAP: usize = 10;

        let lumps = crate::map::MapLumps {
            vertexes: match lump(ML_VERTEXES) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: VERTEXES: {err}")),
            },
            linedefs: match lump(ML_LINEDEFS) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: LINEDEFS: {err}")),
            },
            sidedefs: match lump(ML_SIDEDEFS) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: SIDEDEFS: {err}")),
            },
            sectors: match lump(ML_SECTORS) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: SECTORS: {err}")),
            },
            segs: match lump(ML_SEGS) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: SEGS: {err}")),
            },
            subsectors: match lump(ML_SSECTORS) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: SSECTORS: {err}")),
            },
            nodes: match lump(ML_NODES) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: NODES: {err}")),
            },
            blockmap: match lump(ML_BLOCKMAP) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: BLOCKMAP: {err}")),
            },
            reject: match lump(ML_REJECT) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: REJECT: {err}")),
            },
            things: match lump(ML_THINGS) {
                Ok(data) => data,
                Err(err) => return self.fatal_error(format!("{mapname}: THINGS: {err}")),
            },
        };

        let map = match Map::load(&lumps) {
            Ok(map) => map,
            Err(err) => return self.fatal_error(format!("{mapname}: {err}")),
        };

        self.leveltime = 0;
        self.store.reset_for_level(&map);
        self.sound_flood = crate::enemy::SoundFlood::new(map.sectors.len());
        self.brain = crate::enemy::BrainState::new();
        let sound_orgs = compute_sound_orgs(&map);
        self.sector_sound_org = sound_orgs;

        self.map = Some(map);
        self.leveltime = 0;

        // P_LoadThings: spawns everything the THINGS lump holds
        self.load_things();

        // deathmatch respawn points, if any
        if self.deathmatch {
            for i in 0..MAXPLAYERS {
                if self.playeringame[i] {
                    self.players[i].mo = None;
                    self.spawn_deathmatch_player(i);
                }
            }
        }

        self.body_que_slot = 0;

        // P_SpawnSpecials + P_InitSwitchList
        let level_timer = self.level_timer;
        let report = self.with_special_ctx(|ctx| specials::spawn_specials(ctx, level_timer));
        self.total_secret = report.total_secrets;
        self.scroll_lines = report.scroll_lines;
        self.switches = specials::SwitchList::new(if commercial { 1 } else { episode as u16 });
        self.buttons.clear();

        // the automap resets its state for the new level
        if let Some(map) = self.map.as_ref() {
            self.automap.begin_level(map);
        }

        // the level music starts with the level
        self.change_music_by_level();
    }

    /// The map load aborted with an internal error. The platform gets a
    /// hard stop (vanilla `I_Error`).
    fn fatal_error(&mut self, message: String) {
        tracing::error!(message);
        self.load_error = Some(message);
    }

    /// `P_LoadThings` - iterate THINGS and spawn the actors.
    fn load_things(&mut self) {
        let commercial = self.gamemode.is_commercial();
        let Some(map) = self.map.as_ref() else { return };
        let things: Vec<ThingSpawn> = map.things.clone();

        for thing in &things {
            // DOOM II things in a non-commercial game: stop loading
            // everything after the first one (vanilla stops the loop
            // entirely, a quirk we preserve)
            if !commercial {
                let doom2_only = matches!(
                    thing.thing_type,
                    68 | 64 | 88 | 89 | 69 | 67 | 71 | 65 | 66 | 84
                );
                if doom2_only {
                    break;
                }
            }

            let rules = SpawnRules {
                skill: self.gameskill,
                netgame: self.netgame,
                deathmatch: self.deathmatch_mode,
                no_monsters: self.nomonsters,
            };
            let map = self.map.as_ref().expect("map loaded");
            let outcome = self.store.spawn_map_thing(map, &mut self.rng, rules, thing);
            match outcome {
                SpawnOutcome::PlayerStart { player } => {
                    if !self.deathmatch {
                        self.spawn_player(player, thing);
                    }
                }
                SpawnOutcome::DeathmatchStart { .. } => {}
                SpawnOutcome::Spawned {
                    id,
                    counts_kill,
                    counts_item,
                } => {
                    let _ = id;
                    if counts_kill {
                        self.total_kills += 1;
                    }
                    if counts_item {
                        self.total_items += 1;
                    }
                }
                SpawnOutcome::Skipped(_) => {}
            }
        }
    }

    /// `P_SpawnPlayer`.
    fn spawn_player(&mut self, player: usize, mthing: &ThingSpawn) {
        if !self.playeringame[player] {
            return;
        }
        if self.players[player].playerstate == PlayerState::Reborn {
            self.player_reborn(player);
        }

        let x = Fixed::from_int(i32::from(mthing.x));
        let y = Fixed::from_int(i32::from(mthing.y));
        let map = self.map.as_ref().expect("map loaded");
        let id = self.store.spawn_mobj(
            map,
            &mut self.rng,
            SpawnSpec {
                skill: self.gameskill,
                x,
                y,
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_PLAYER,
            },
        );

        if player > 0 {
            if let Some(mo) = self.store.mobj_mut(id) {
                mo.flags |= (player as u32) << crate::info::MF_TRANSSHIFT;
            }
        }
        if let Some(mo) = self.store.mobj_mut(id) {
            mo.angle = Angle::from_raw(
                Angle::ANG45
                    .to_raw()
                    .wrapping_mul((i32::from(mthing.angle) / 45) as u32),
            );
            mo.player = Some(player);
            mo.health = self.players[player].health;
        }

        {
            let p = &mut self.players[player];
            p.mo = Some(id);
            p.playerstate = PlayerState::Live;
            p.refire = 0;
            p.message = None;
            p.damagecount = 0;
            p.bonuscount = 0;
            p.extralight = 0;
            p.fixedcolormap = 0;
            p.viewheight = VIEWHEIGHT;

            if self.deathmatch {
                for index in 0..crate::player::NUM_CARDS {
                    p.cards[index] = true;
                }
            }
        }

        self.setup_psprites(player);
    }

    /// A player respawns at a deathmatch spot (vanilla
    /// `G_DeathMatchSpawnPlayer`).
    fn spawn_deathmatch_player(&mut self, player: usize) {
        let selections = self.store.deathmatch_starts().len();
        if selections < 4 {
            tracing::warn!("Only {selections} deathmatch spots, 4 required");
        }
        let mut spawned = false;
        for _ in 0..20 {
            let index = if selections > 0 {
                (self.rng.next() as usize) % selections
            } else {
                0
            };
            let Some(mthing) = self.store.deathmatch_starts().get(index).copied() else {
                continue;
            };
            if self.check_spot(player, &mthing) {
                let mut spawn = mthing;
                spawn.thing_type = (player + 1) as i16;
                self.spawn_player(player, &spawn);
                spawned = true;
                break;
            }
        }
        if !spawned {
            // no good spot: the player sticks at their start
            let start = self
                .store
                .player_start(player)
                .copied()
                .unwrap_or(ThingSpawn {
                    x: 0,
                    y: 0,
                    angle: 0,
                    thing_type: (player + 1) as i16,
                    flags: 0,
                });
            self.spawn_player(player, &start);
        }
    }

    /// `G_CheckSpot` with the corpse flush behaviour.
    fn check_spot(&mut self, player_num: usize, mthing: &ThingSpawn) -> bool {
        if self.players[player_num].mo.is_none() {
            // first spawn of the level: no corpse can block anyone
            for i in 0..player_num {
                let same = self.players[i]
                    .mo
                    .and_then(|id| self.store.mobj(id))
                    .map(|mo| {
                        mo.x == Fixed::from_int(i32::from(mthing.x))
                            && mo.y == Fixed::from_int(i32::from(mthing.y))
                    })
                    .unwrap_or(false);
                if same {
                    return false;
                }
            }
            return true;
        }

        let x = Fixed::from_int(i32::from(mthing.x));
        let y = Fixed::from_int(i32::from(mthing.y));
        let avatar = self.players[player_num].mo.expect("checked above");

        let fits = {
            let map = self.map.as_ref().expect("map loaded");
            let mut ctx = MoveContext::new(map, &mut self.store, &mut self.players, &mut self.rng);
            ctx.leveltime = self.leveltime;
            movement::check_position(&mut ctx, avatar, x, y).ok
        };
        if !fits {
            return false;
        }

        // flush an old corpse if needed
        if self.body_que_slot >= BODY_QUE_SIZE {
            if let Some(old) = self.body_que[self.body_que_slot % BODY_QUE_SIZE] {
                self.store.remove_mobj(old, self.leveltime);
            }
        }
        self.body_que[self.body_que_slot % BODY_QUE_SIZE] = Some(avatar);
        self.body_que_slot += 1;

        // spawn a teleport fog
        let floorz = {
            let map = self.map.as_ref().expect("map loaded");
            let subsector = crate::mobj::point_in_subsector(map, x, y);
            subsector
                .and_then(|ss| map.subsectors.get(ss))
                .and_then(|s| s.sector)
                .map(|s| map.sectors[s].floor_height)
                .unwrap_or(Fixed::ZERO)
        };
        let an = ((Angle::ANG45.to_raw() as u64 * (i32::from(mthing.angle) / 45) as u64)
            >> crate::tables::ANGLE_TO_FINE_SHIFT) as usize;
        let fine = an & crate::tables::FINE_MASK as usize;
        let fog = {
            let map = self.map.as_ref().expect("map loaded");
            self.store.spawn_mobj(
                map,
                &mut self.rng,
                SpawnSpec {
                    skill: self.gameskill,
                    x: x + Fixed::from_raw(20 * FINE_COSINE[fine].to_raw()),
                    y: y + Fixed::from_raw(20 * FINE_SINE[fine].to_raw()),
                    z: SpawnZ::At(floorz),
                    mobj_type: MobjType::MT_TFOG,
                },
            )
        };
        // don't start the sound on the very first frame
        if self.players[self.consoleplayer].viewz != Fixed::from_raw(1) {
            self.sound_start_actor(fog, crate::info::SfxId::sfx_telept);
        }
        true
    }

    /// `G_PlayerReborn` - reset a player for a fresh life. Persistent
    /// frag/kill/item/secret tallies survive.
    fn player_reborn(&mut self, player: usize) {
        let frags = self.players[player].frags;
        let killcount = self.players[player].killcount;
        let itemcount = self.players[player].itemcount;
        let secretcount = self.players[player].secretcount;

        let mut fresh = player_reborn_default();
        fresh.frags = frags;
        fresh.killcount = killcount;
        fresh.itemcount = itemcount;
        fresh.secretcount = secretcount;
        fresh.usedown = true;
        fresh.attackdown = true;
        self.players[player] = fresh;
    }

    /// `P_SetupPsprites`.
    fn setup_psprites(&mut self, player: usize) {
        let mut events = Vec::new();
        {
            let map = self.map.as_ref().expect("map loaded");
            let rules = self.game_rules();
            let mut ctx = crate::enemy::EnemyContext {
                map,
                store: &mut self.store,
                players: &mut self.players,
                rng: &mut self.rng,
                leveltime: self.leveltime,
                sky_flat: self.sky_flat,
                events: &mut events,
                rules,
                flood: &mut self.sound_flood,
                brain: &mut self.brain,
            };
            weapons::setup_psprites(&mut ctx, player, self.gamemode);
        }
        self.dispatch_game_events(events);
    }

    /// Vanilla game-state globals for the gameplay layers.
    fn game_rules(&self) -> crate::enemy::GameRules {
        crate::enemy::GameRules {
            skill: self.gameskill,
            fastparm: self.fastparm,
            netgame: self.netgame,
            commercial: self.gamemode.is_commercial(),
            episode: self.gameepisode,
            map: self.gamemap,
        }
    }
}

/// Sector `soundorg` - the midpoint of the sector's bounding box,
/// computed once per level.
fn compute_sound_orgs(map: &Map) -> Vec<(Fixed, Fixed)> {
    let mut orgs = Vec::with_capacity(map.sectors.len());
    for sector in 0..map.sectors.len() {
        let mut minx = Fixed::from_raw(i32::MAX);
        let mut maxx = Fixed::from_raw(i32::MIN);
        let mut miny = Fixed::from_raw(i32::MAX);
        let mut maxy = Fixed::from_raw(i32::MIN);
        for line in &map.linedefs {
            if line.frontsector == Some(sector) || line.backsector == Some(sector) {
                let v1 = &map.vertexes[line.v1];
                let v2 = &map.vertexes[line.v2];
                for vertex in [v1, v2] {
                    minx = minx.min(vertex.x);
                    maxx = maxx.max(vertex.x);
                    miny = miny.min(vertex.y);
                    maxy = maxy.max(vertex.y);
                }
            }
        }
        if minx > maxx {
            // sector with no lines touching it: origin at map origin
            orgs.push((Fixed::ZERO, Fixed::ZERO));
        } else {
            orgs.push((
                Fixed::from_raw((minx.to_raw() as i64 + maxx.to_raw() as i64) as i32 / 2),
                Fixed::from_raw((miny.to_raw() as i64 + maxy.to_raw() as i64) as i32 / 2),
            ));
        }
    }
    orgs
}

// ==================================================================
// the ticker
// ==================================================================

impl Game {
    /// `G_Ticker` - one game tic. Vanilla calls this from `D_DoomLoop`;
    /// here it is driven by [`Game::tick_input`]/[`Game::advance`].
    pub(crate) fn run_tic(&mut self, input: &TicCmdInput) -> EngineResult<()> {
        self.run_tic_world(None, input)
    }

    /// The net-ticker variant: the governor resolves every player's command
    /// from `netcmds` before the tic runs (the local one comes back through
    /// the node-0 rebound, like vanilla); the world just consumes them.
    pub(crate) fn run_tic_net(&mut self, cmds: [TicCmd; MAXPLAYERS]) -> EngineResult<()> {
        self.run_tic_world(Some(cmds), &TicCmdInput::NONE)
    }

    /// Single-player command resolution: demo playback for the console
    /// player, the input builder otherwise, inert commands for the other
    /// slots (which are not in play).
    fn build_sp_cmds(&mut self, input: &TicCmdInput) -> [TicCmd; MAXPLAYERS] {
        let mut cmds = [TicCmd::default(); MAXPLAYERS];
        for (i, slot) in cmds.iter_mut().enumerate() {
            if !self.playeringame[i] {
                *slot = TicCmd::default();
            } else if i == self.consoleplayer {
                if let Some(playback) = self.demo_playback.as_mut() {
                    let cmd = playback.read_ticcmd();
                    if playback.finished() {
                        self.end_demo_playback();
                    }
                    *slot = cmd;
                } else {
                    let cmd = self.builder.build(input);
                    if self.demorecording {
                        if let Some(recording) = self.demo_recording.as_mut() {
                            // 'q' ends the recording, like vanilla
                            if self.gamekeydown[b'q' as usize] {
                                self.end_demo_recording();
                            } else if !recording.write_ticcmd(&cmd) {
                                self.demorecording = false;
                                self.players[self.consoleplayer].message =
                                    Some("Demo buffer is full".to_string());
                            }
                        }
                    }
                    *slot = cmd;
                }
            } else {
                *slot = TicCmd::default();
            }
        }
        cmds
    }

    /// The tick body shared by both paths; `cmds[i]` is this tic's command
    /// for player i.
    fn run_tic_world(
        &mut self,
        net_cmds: Option<[TicCmd; MAXPLAYERS]>,
        input: &TicCmdInput,
    ) -> EngineResult<()> {
        if let Some(err) = self.load_error.take() {
            return Err(EngineError::Resource(err));
        }

        // vanilla ticks the menu ticker before the game ticker (M_Ticker)
        self.menu.tick(self.gametic as u64);

        // do player reborns if needed
        for i in 0..MAXPLAYERS {
            if self.playeringame[i] && self.players[i].playerstate == PlayerState::Reborn {
                self.do_reborn(i);
            }
        }

        // do things to change the game state
        while self.game_action != GameAction::Nothing {
            match self.game_action {
                GameAction::LoadLevel => self.do_load_level(),
                GameAction::NewGame => self.do_new_game(),
                GameAction::Completed => self.do_completed(),
                GameAction::Victory => self.start_finale(),
                GameAction::WorldDone => self.do_world_done(),
                GameAction::PlayDemo => self.do_play_demo(),
                GameAction::Nothing => {}
            }
        }

        // a failed level load aborts the tic instead of panicking on a
        // half-built world
        if self.load_error.is_some() {
            return Err(EngineError::Resource(
                self.load_error
                    .take()
                    .unwrap_or_else(|| "unknown load error".into()),
            ));
        }

        // get commands (after the state transitions, like vanilla - a
        // deferred NewGame can arm demo recording mid-tic)
        let net = net_cmds.is_some();
        let cmds = match net_cmds {
            Some(cmds) => cmds,
            None => self.build_sp_cmds(input),
        };
        for (i, cmd) in cmds.into_iter().enumerate() {
            if self.playeringame[i] {
                self.players[i].cmd = cmd;
            }
        }

        // turbo cheat (`%s is turbo!` when the player builds over the
        // threshold with -turbo); vanilla reads the freshly copied cmd
        for i in 0..MAXPLAYERS {
            if self.playeringame[i]
                && self.players[i].cmd.forward_move > TURBO_THRESHOLD as i8
                && (self.gametic & 31) == 0
                && ((self.gametic >> 5) & 3) as usize == i
            {
                self.players[self.consoleplayer].message =
                    Some(format!("{} is turbo!", crate::hud::PLAYER_NAMES[i]));
            }
        }

        // consistency check (vanilla's G_Ticker tail): only in net games,
        // only on non-duplicated tics
        if net {
            let gate = crate::net::ConsistencyGate {
                netgame: true,
                netdemo: false,
                ticdup: self.net_ticdup,
            };
            for i in 0..MAXPLAYERS {
                if !self.playeringame[i] {
                    continue;
                }
                let cmd = self.players[i].cmd;
                let source_x = self.players[i]
                    .mo
                    .and_then(|id| self.store.mobj(id))
                    .map(|mo| mo.x.to_raw())
                    .unwrap_or(self.rng.index() as i32);
                self.consistency
                    .check_and_store(&cmd, gate, self.gametic, i, source_x)?;
            }
        }

        // check for special buttons
        for i in 0..MAXPLAYERS {
            if !self.playeringame[i] {
                continue;
            }
            let buttons = self.players[i].cmd.buttons;
            if buttons & BT_SPECIAL != 0 {
                match buttons & BT_SPECIALMASK {
                    BTS_PAUSE => {
                        self.paused = !self.paused;
                    }
                    BTS_SAVEGAME => {
                        let slot = ((buttons & BTS_SAVEMASK) >> BTS_SAVESHIFT) as usize;
                        let name = if self.savedescription.is_empty() {
                            "NET GAME".to_string()
                        } else {
                            std::mem::take(&mut self.savedescription)
                        };
                        self.save_game_slot(slot, &name);
                    }
                    _ => {}
                }
            }
        }

        // do main actions
        match self.gamestate {
            GameState::Level => {
                self.p_ticker();
                self.st_ticker();
                self.am_ticker();
                self.hu_ticker();
            }
            GameState::Intermission => self.intermission_ticker(),
            GameState::Finale => self.finale_ticker(),
            GameState::DemoScreen => {}
        }

        // in a net game the tic governor owns the counter (hooks::set_gametic)
        if !net {
            self.gametic += 1;
        }
        Ok(())
    }

    /// `G_DoReborn` - a dead player asked to respawn. Single player
    /// reloads the level from scratch.
    fn do_reborn(&mut self, player_num: usize) {
        if !self.netgame {
            self.game_action = GameAction::LoadLevel;
        } else {
            // net respawn in place: corpse detached, fresh spawn
            if let Some(avatar) = self.players[player_num].mo {
                if let Some(mo) = self.store.mobj_mut(avatar) {
                    mo.player = None;
                }
            }
            if self.deathmatch {
                self.spawn_deathmatch_player(player_num);
            } else if self.check_spot(
                player_num,
                &self
                    .store
                    .player_start(player_num)
                    .copied()
                    .unwrap_or(ThingSpawn {
                        x: 0,
                        y: 0,
                        angle: 0,
                        thing_type: (player_num + 1) as i16,
                        flags: 0,
                    }),
            ) {
                if let Some(start) = self.store.player_start(player_num).copied() {
                    self.spawn_player(player_num, &start);
                }
            }
        }
    }

    /// `P_Ticker` - one tic of the world simulation.
    fn p_ticker(&mut self) {
        if self.paused {
            return;
        }

        // pause if in menu and at least one tic has been run
        if !self.netgame && self.menu.is_active() && !self.demoplayback {
            let viewz_sentinel = self.players[self.consoleplayer].viewz == Fixed::from_raw(1);
            if !viewz_sentinel {
                return;
            }
        }

        // players
        for i in 0..MAXPLAYERS {
            if self.playeringame[i] {
                self.player_think(i);
            }
        }

        // thinkers (mobjs run first; the specials pass follows - see the
        // module docs for the one-tic order deviation from vanilla's
        // interleaved thinker list)
        self.run_mobj_thinkers();
        self.run_special_pass();

        // P_UpdateSpecials: the level timer, the scroll lines and the
        // pressed buttons
        let mut timer = self.level_timer;
        let scroll = self.scroll_lines.clone();
        let resolver = |raw: &[u8; 8]| -> Option<Fixed> {
            self.render
                .textures
                .lookup_name(raw)
                .map(|i| self.render.textures.texture_height(i))
        };
        {
            let map = self.map.as_mut().expect("map loaded");
            let mut ctx =
                SpecialContext::new(map, &mut self.store, &mut self.players, &mut self.rng)
                    .with_texture_height(&resolver);
            ctx.leveltime = self.leveltime;
            ctx.skill = self.gameskill;
            ctx.boss_level = self.gamemap == 30 && self.gamemode.is_commercial();
            ctx.pending_thinkers = std::mem::take(&mut self.pending_thinkers);
            let buttons = &mut self.buttons;
            specials::update_specials(&mut ctx, buttons, &scroll, &mut timer);
            self.pending_thinkers = std::mem::take(&mut ctx.pending_thinkers);
            let events = std::mem::take(&mut ctx.events);
            self.dispatch_special_events(events);
        }
        self.level_timer = timer;

        // P_RespawnSpecials (-altdeath only)
        self.respawn_specials();

        // for par times
        self.leveltime += 1;
    }

    /// `P_PlayerThink`.
    fn player_think(&mut self, player_index: usize) {
        // fixme: do this in the cheat code
        let noclip = self.players[player_index].cheats & CF_NOCLIP != 0;
        if let Some(mo) = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj_mut(id))
        {
            if noclip {
                mo.flags |= crate::info::MF_NOCLIP;
            } else {
                mo.flags &= !crate::info::MF_NOCLIP;
            }
        }

        // force the chainsaw thrust, vanilla-style
        let mo_flags = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj(id))
            .map(|mo| mo.flags)
            .unwrap_or(0);
        if mo_flags & crate::info::MF_JUSTATTACKED != 0 {
            let cmd = &mut self.players[player_index].cmd;
            cmd.angle_turn = 0;
            cmd.forward_move = (0xc800 / 512) as i8;
            cmd.side_move = 0;
            if let Some(mo) = self.players[player_index]
                .mo
                .and_then(|id| self.store.mobj_mut(id))
            {
                mo.flags &= !crate::info::MF_JUSTATTACKED;
            }
        }

        if self.players[player_index].playerstate == PlayerState::Dead {
            self.death_think(player_index);
            return;
        }

        // move around (reactiontime gates teleport freezes)
        let reactiontime = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj(id))
            .map(|mo| mo.reactiontime)
            .unwrap_or(0);
        if reactiontime > 0 {
            if let Some(mo) = self.players[player_index]
                .mo
                .and_then(|id| self.store.mobj_mut(id))
            {
                mo.reactiontime -= 1;
            }
        } else {
            self.move_player(player_index);
        }

        self.calc_height(player_index);

        // sector specials (slime damage, secrets, ...)
        let sector_special = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj(id))
            .and_then(|mo| mo.sector)
            .map(|s| self.map.as_ref().expect("map loaded").sectors[s].special)
            .unwrap_or(0);
        if sector_special != 0 {
            let player = player_index;
            let resolver = |raw: &[u8; 8]| -> Option<Fixed> {
                self.render
                    .textures
                    .lookup_name(raw)
                    .map(|i| self.render.textures.texture_height(i))
            };
            let events;
            {
                let map = self.map.as_mut().expect("map loaded");
                let mut ctx = specials::SpecialContext::new(
                    map,
                    &mut self.store,
                    &mut self.players,
                    &mut self.rng,
                )
                .with_texture_height(&resolver);
                ctx.leveltime = self.leveltime;
                ctx.skill = self.gameskill;
                ctx.boss_level = self.gamemap == 30 && self.gamemode.is_commercial();
                ctx.pending_thinkers = std::mem::take(&mut self.pending_thinkers);
                specials::player_in_special_sector(&mut ctx, player);
                self.pending_thinkers = std::mem::take(&mut ctx.pending_thinkers);
                events = std::mem::take(&mut ctx.events);
            }
            self.dispatch_special_events(events);
        }

        // a special event has no other buttons
        if self.players[player_index].cmd.buttons & BT_SPECIAL != 0 {
            self.players[player_index].cmd.buttons = 0;
        }

        // weapon change
        let buttons = self.players[player_index].cmd.buttons;
        if buttons & BT_CHANGE != 0 {
            let bits = (buttons & BT_WEAPONMASK) >> BT_WEAPONSHIFT;
            let candidate = match bits {
                0 => Some(Weapon::Fist),
                1 => Some(Weapon::Pistol),
                2 => Some(Weapon::Shotgun),
                3 => Some(Weapon::Chaingun),
                4 => Some(Weapon::MissileLauncher),
                5 => Some(Weapon::PlasmaRifle),
                6 => Some(Weapon::Bfg),
                7 => Some(Weapon::Chainsaw),
                _ => None,
            };
            if let Some(mut newweapon) = candidate {
                // the chainsaw rides on the fist slot
                let owned = self.players[player_index].weaponowned;
                let ready = self.players[player_index].readyweapon;
                let strength = self.players[player_index].powers[Power::Strength as usize] > 0;
                if newweapon == Weapon::Fist
                    && owned[Weapon::Chainsaw as usize]
                    && !(ready == Weapon::Chainsaw && strength)
                {
                    newweapon = Weapon::Chainsaw;
                }
                // the super shotgun replaces the shotgun in DOOM II
                if self.gamemode.is_commercial()
                    && newweapon == Weapon::Shotgun
                    && owned[Weapon::SuperShotgun as usize]
                    && ready != Weapon::SuperShotgun
                {
                    newweapon = Weapon::SuperShotgun;
                }
                if owned[newweapon as usize] && newweapon != ready {
                    // no plasma or BFG in shareware, even by cheat
                    let shareware = self.gamemode == GameMode::Shareware;
                    if !(newweapon == Weapon::PlasmaRifle || newweapon == Weapon::Bfg) || !shareware
                    {
                        self.players[player_index].pendingweapon = Some(newweapon);
                    }
                }
            }
        }

        // check for use
        if self.players[player_index].cmd.buttons & BT_USE != 0 {
            if !self.players[player_index].usedown {
                self.use_lines(player_index);
                self.players[player_index].usedown = true;
            }
        } else {
            self.players[player_index].usedown = false;
        }

        // cycle psprites
        self.move_psprites(player_index);

        // strength counts up to diminish the fading
        let strength = &mut self.players[player_index].powers[Power::Strength as usize];
        if *strength > 0 {
            *strength += 1;
        }

        let invuln = &mut self.players[player_index].powers[Power::Invulnerability as usize];
        if *invuln > 0 {
            *invuln -= 1;
        }

        let invis = &mut self.players[player_index].powers[Power::Invisibility as usize];
        if *invis > 0 {
            *invis -= 1;
            if *invis == 0 {
                if let Some(mo) = self.players[player_index]
                    .mo
                    .and_then(|id| self.store.mobj_mut(id))
                {
                    mo.flags &= !crate::info::MF_SHADOW;
                }
            }
        }

        let infrared = &mut self.players[player_index].powers[Power::Infrared as usize];
        if *infrared > 0 {
            *infrared -= 1;
        }

        let ironfeet = &mut self.players[player_index].powers[Power::IronFeet as usize];
        if *ironfeet > 0 {
            *ironfeet -= 1;
        }

        if self.players[player_index].damagecount > 0 {
            self.players[player_index].damagecount -= 1;
        }
        if self.players[player_index].bonuscount > 0 {
            self.players[player_index].bonuscount -= 1;
        }

        // handle the fixed colormaps (invulnerability / light amplification)
        let powers = &self.players[player_index].powers;
        self.players[player_index].fixedcolormap = if powers[Power::Invulnerability as usize] > 0 {
            if powers[Power::Invulnerability as usize] > 4 * 32
                || (powers[Power::Invulnerability as usize] & 8) != 0
            {
                crate::gfx::palette::INVULNERABILITY_COLORMAP as i32
            } else {
                0
            }
        } else if powers[Power::Infrared as usize] > 0 {
            if powers[Power::Infrared as usize] > 4 * 32
                || (powers[Power::Infrared as usize] & 8) != 0
            {
                1
            } else {
                0
            }
        } else {
            0
        };
    }

    /// `P_CalcHeight` - the view bob and the smooth step-up tracking.
    fn calc_height(&mut self, player_index: usize) {
        let Some(avatar) = self.players[player_index].mo else {
            return;
        };
        let Some((momx, momy, mo_z, floorz, ceilingz)) = self
            .store
            .mobj(avatar)
            .map(|mo| (mo.momx, mo.momy, mo.z, mo.floorz, mo.ceilingz))
        else {
            return;
        };

        // regular movement bobbing
        let mut bob = momx * momx + momy * momy;
        bob = Fixed::from_raw(bob.to_raw() >> 2);
        if bob > MAXBOB {
            bob = MAXBOB;
        }

        let onground = mo_z <= floorz;
        if self.players[player_index].cheats & CF_NOMOMENTUM != 0 || !onground {
            self.players[player_index].viewz = mo_z + self.players[player_index].viewheight;
            return;
        }

        let angle = (FINE_ANGLES / 20) * ((self.leveltime as usize) & FINE_MASK);
        let bob2 = Fixed::from_raw(bob.to_raw() / 2) * FINE_SINE[angle & FINE_MASK];

        if self.players[player_index].playerstate == PlayerState::Live {
            let delta = self.players[player_index].deltaviewheight;
            self.players[player_index].viewheight += delta;

            if self.players[player_index].viewheight > VIEWHEIGHT {
                self.players[player_index].viewheight = VIEWHEIGHT;
                self.players[player_index].deltaviewheight = Fixed::ZERO;
            }

            if self.players[player_index].viewheight < Fixed::from_raw(VIEWHEIGHT.to_raw() / 2) {
                self.players[player_index].viewheight = Fixed::from_raw(VIEWHEIGHT.to_raw() / 2);
                if self.players[player_index].deltaviewheight <= Fixed::ZERO {
                    self.players[player_index].deltaviewheight = Fixed::from_raw(FRACUNIT);
                }
            }

            if self.players[player_index].deltaviewheight != Fixed::ZERO {
                let bumped =
                    self.players[player_index].deltaviewheight + Fixed::from_raw(FRACUNIT / 4);
                self.players[player_index].deltaviewheight = if bumped == Fixed::ZERO {
                    Fixed::from_raw(FRACUNIT)
                } else {
                    bumped
                };
            }
        }

        let mut viewz = mo_z + self.players[player_index].viewheight + bob2;
        if viewz > ceilingz - Fixed::from_raw(4 * FRACUNIT) {
            viewz = ceilingz - Fixed::from_raw(4 * FRACUNIT);
        }
        self.players[player_index].viewz = viewz;
    }

    /// `P_MovePlayer`.
    fn move_player(&mut self, player_index: usize) {
        let (angle_turn, forward_move, side_move) = {
            let cmd = &self.players[player_index].cmd;
            (cmd.angle_turn, cmd.forward_move, cmd.side_move)
        };

        let Some(avatar) = self.players[player_index].mo else {
            return;
        };

        // turn the body
        if angle_turn != 0 {
            if let Some(mo) = self.store.mobj_mut(avatar) {
                mo.angle = Angle::from_raw(
                    mo.angle
                        .to_raw()
                        .wrapping_add((angle_turn as i32 as u32) << 16),
                );
            }
        }

        let onground = self
            .store
            .mobj(avatar)
            .map(|mo| mo.z <= mo.floorz)
            .unwrap_or(true);

        if forward_move != 0 && onground {
            self.thrust(
                player_index,
                self.players[player_index]
                    .mo
                    .and_then(|id| self.store.mobj(id))
                    .map(|mo| mo.angle)
                    .unwrap_or(Angle::ZERO),
                i32::from(forward_move) * 2048,
            );
        }
        if side_move != 0 && onground {
            let angle = self.players[player_index]
                .mo
                .and_then(|id| self.store.mobj(id))
                .map(|mo| mo.angle)
                .unwrap_or(Angle::ZERO);
            self.thrust(player_index, angle - ANG90, i32::from(side_move) * 2048);
        }

        // entering the walking frames
        let onground_state = self
            .store
            .mobj(avatar)
            .map(|mo| mo.state)
            .unwrap_or(StateId::S_NULL);
        if (forward_move != 0 || side_move != 0) && onground_state == StateId::S_PLAY {
            self.set_mobj_state_and_run_actions(avatar, StateId::S_PLAY_RUN1);
        }
    }

    /// `P_Thrust`.
    fn thrust(&mut self, player_index: usize, angle: Angle, move_fixed: i32) {
        let fine = angle.fine_angle();
        let cos = FINE_COSINE[fine];
        let sin = FINE_SINE[fine];
        let move_fixed = Fixed::from_raw(move_fixed);
        if let Some(mo) = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj_mut(id))
        {
            mo.momx += move_fixed * cos;
            mo.momy += move_fixed * sin;
        }
    }

    /// `P_DeathThink`.
    fn death_think(&mut self, player_index: usize) {
        self.move_psprites(player_index);

        // fall to the ground
        if self.players[player_index].viewheight > Fixed::from_raw(6 * FRACUNIT) {
            self.players[player_index].viewheight -= Fixed::from_raw(FRACUNIT);
        }
        if self.players[player_index].viewheight < Fixed::from_raw(6 * FRACUNIT) {
            self.players[player_index].viewheight = Fixed::from_raw(6 * FRACUNIT);
        }
        self.players[player_index].deltaviewheight = Fixed::ZERO;

        let onground = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj(id))
            .map(|mo| mo.z <= mo.floorz)
            .unwrap_or(true);
        let _ = onground;
        self.calc_height(player_index);

        // face the killer
        let attacker = self.players[player_index].attacker;
        let mo_angle = self.players[player_index]
            .mo
            .and_then(|id| self.store.mobj(id))
            .map(|mo| (mo.x, mo.y, mo.angle));
        if let (Some(attacker_id), Some((x, y, angle))) = (attacker, mo_angle) {
            if Some(attacker_id) != self.players[player_index].mo {
                if let Some((ax, ay)) = self.store.mobj(attacker_id).map(|mo| (mo.x, mo.y)) {
                    let delta = crate::bsp::point_to_angle_2(x, y, ax, ay) - angle;
                    if delta.to_raw() < ANG5.to_raw()
                        || delta.to_raw() > 0u32.wrapping_sub(ANG5.to_raw())
                    {
                        // looking at the killer: face them and fade the flash
                        if let Some(mo) = self.players[player_index]
                            .mo
                            .and_then(|id| self.store.mobj_mut(id))
                        {
                            mo.angle = crate::bsp::point_to_angle_2(x, y, ax, ay);
                        }
                        if self.players[player_index].damagecount > 0 {
                            self.players[player_index].damagecount -= 1;
                        }
                    } else if delta.to_raw() < Angle::ANG180.to_raw() {
                        if let Some(mo) = self.players[player_index]
                            .mo
                            .and_then(|id| self.store.mobj_mut(id))
                        {
                            mo.angle += ANG5;
                        }
                    } else if let Some(mo) = self.players[player_index]
                        .mo
                        .and_then(|id| self.store.mobj_mut(id))
                    {
                        mo.angle -= ANG5;
                    }
                }
            } else if self.players[player_index].damagecount > 0 {
                self.players[player_index].damagecount -= 1;
            }
        } else if self.players[player_index].damagecount > 0 {
            self.players[player_index].damagecount -= 1;
        }

        if self.players[player_index].cmd.buttons & BT_USE != 0 {
            self.players[player_index].playerstate = PlayerState::Reborn;
        }
    }

    /// `P_UseLines` - the USE key raycast.
    fn use_lines(&mut self, player_index: usize) {
        let Some(avatar) = self.players[player_index].mo else {
            return;
        };
        let Some((x1, y1, angle)) = self.store.mobj(avatar).map(|mo| (mo.x, mo.y, mo.angle)) else {
            return;
        };
        let fine = angle.fine_angle();
        let x2 = x1 + Fixed::from_int(64) * FINE_COSINE[fine];
        let y2 = y1 + Fixed::from_int(64) * FINE_SINE[fine];

        let mut noway_sound: Option<ThinkerId> = None;
        let mut activation: Option<(usize, LineAction)> = None;

        {
            let map = self.map.as_ref().expect("map loaded");
            let mut ctx = MoveContext::new(map, &mut self.store, &mut self.players, &mut self.rng);
            ctx.leveltime = self.leveltime;
            movement::traverse_path(
                &mut ctx,
                crate::math::Point::new(x1, y1),
                crate::math::Point::new(x2, y2),
                movement::PT_ADDLINES,
                |ctx, intercept| {
                    if let crate::movement::InterceptPayload::Line(line) = intercept.payload {
                        if ctx.map.linedefs[line].special == 0 {
                            let opening = movement::line_opening(ctx.map, line);
                            if opening.range <= Fixed::ZERO {
                                noway_sound = Some(avatar);
                                return false; // can't use through a wall
                            }
                            return true; // not special; keep checking
                        }
                        let linedef = &ctx.map.linedefs[line];
                        let v1 = &ctx.map.vertexes[linedef.v1];
                        let side = if crate::math::point_on_divline_side(
                            x1,
                            y1,
                            &crate::math::Divline {
                                x: v1.x,
                                y: v1.y,
                                dx: linedef.dx,
                                dy: linedef.dy,
                            },
                        ) == 1
                        {
                            1
                        } else {
                            0
                        };
                        activation = Some((
                            line,
                            LineAction::Use {
                                side,
                                thing: avatar,
                            },
                        ));
                        false // can't use more than one special line in a row
                    } else {
                        true
                    }
                },
            );
        }

        if let Some(actor) = noway_sound {
            self.sound_start_actor(actor, crate::info::SfxId::sfx_noway);
        }
        if let Some((line, action)) = activation {
            self.handle_line_activation(line, action);
        }
    }

    /// `P_MovePsprites`.
    fn move_psprites(&mut self, player_index: usize) {
        let mut events = Vec::new();
        {
            let map = self.map.as_ref().expect("map loaded");
            let rules = self.game_rules();
            let mut ctx = crate::enemy::EnemyContext {
                map,
                store: &mut self.store,
                players: &mut self.players,
                rng: &mut self.rng,
                leveltime: self.leveltime,
                sky_flat: self.sky_flat,
                events: &mut events,
                rules,
                flood: &mut self.sound_flood,
                brain: &mut self.brain,
            };
            weapons::move_psprites(&mut ctx, player_index, self.gamemode);
        }
        self.dispatch_game_events(events);
    }

    /// Run the mobj thinkers (vanilla `P_MobjThinker`).
    fn run_mobj_thinkers(&mut self) {
        let ids: Vec<ThinkerId> = self
            .store
            .thinkers()
            .filter_map(|(id, thinker)| match thinker {
                Thinker::Mobj(_) => Some(id),
                _ => None,
            })
            .collect();

        for id in ids {
            self.mobj_thinker(id);
        }
    }

    /// One mobj's thinker turn.
    fn mobj_thinker(&mut self, id: ThinkerId) {
        let Some((momx, momy, momz, flags)) = self
            .store
            .mobj(id)
            .map(|mo| (mo.momx, mo.momy, mo.momz as Fixed, mo.flags))
        else {
            return; // gone before its turn
        };
        let _ = momz;

        // momentum movement
        if momx != Fixed::ZERO || momy != Fixed::ZERO || (flags & crate::info::MF_SKULLFLY) != 0 {
            let move_events;
            let actions;
            {
                let map = self.map.as_ref().expect("map loaded");
                let mut ctx =
                    MoveContext::new(map, &mut self.store, &mut self.players, &mut self.rng);
                ctx.leveltime = self.leveltime;
                let report = movement::xy_movement(&mut ctx, id);
                move_events = report.events;
                actions = report.actions;
            }
            for mv in move_events {
                self.dispatch_move_event(mv);
            }
            self.dispatch_state_actions(id, actions);
            if self.store.mobj(id).is_none() {
                return; // exploded against the sky or similar
            }
        }

        // z movement
        let needs_z = {
            match self.store.mobj(id) {
                Some(mo) => mo.z != mo.floorz || mo.momz != Fixed::ZERO,
                None => false,
            }
        };
        if needs_z {
            let move_events;
            let actions;
            {
                let map = self.map.as_ref().expect("map loaded");
                let mut ctx =
                    MoveContext::new(map, &mut self.store, &mut self.players, &mut self.rng);
                ctx.leveltime = self.leveltime;
                let report = movement::z_movement(&mut ctx, id);
                move_events = report.events;
                actions = report.actions;
            }
            for mv in move_events {
                self.dispatch_move_event(mv);
            }
            self.dispatch_state_actions(id, actions);
            if self.store.mobj(id).is_none() {
                return;
            }
        }

        // cycle through states
        let (tics, state) = match self.store.mobj(id) {
            Some(mo) => (mo.tics, mo.state),
            None => return,
        };

        if tics != -1 {
            let mut new_tics = tics - 1;
            // -fast doubles the demon tic decay (S_SARG_RUN1..=S_SARG_PAIN2),
            // which is what vanilla's tics halving achieves
            if self.fastparm && (S_SARG_RUN1..=S_SARG_PAIN2).contains(&(state as usize)) {
                new_tics -= 1;
            }
            if let Some(mo) = self.store.mobj_mut(id) {
                mo.tics = new_tics;
            }
            if new_tics == 0 {
                let next = STATES[state as usize].next;
                self.set_mobj_state_and_run_actions(id, next);
            }
        } else {
            // nightmare respawn bookkeeping
            let countkill = self
                .store
                .mobj(id)
                .map(|mo| mo.flags & crate::info::MF_COUNTKILL != 0)
                .unwrap_or(false);
            if !countkill {
                return;
            }
            if !self.respawnmonsters {
                return;
            }
            let movecount = {
                let Some(mo) = self.store.mobj_mut(id) else {
                    return;
                };
                mo.movecount += 1;
                mo.movecount
            };
            if movecount < 12 * TICRATE {
                return;
            }
            if (self.leveltime & 31) != 0 {
                return;
            }
            if self.rng.next() > 4 {
                return;
            }
            self.nightmare_respawn(id);
        }
    }

    /// `P_NightmareRespawn`.
    fn nightmare_respawn(&mut self, id: ThinkerId) {
        let (x, y, map_x, map_y, mobj_type, ambush) = {
            let Some(mo) = self.store.mobj(id) else {
                return;
            };
            let map_x = mo.spawnpoint.x;
            let map_y = mo.spawnpoint.y;
            (
                mo.x,
                mo.y,
                i32::from(map_x),
                i32::from(map_y),
                mo.mobj_type,
                mo.spawnpoint.flags & 8 != 0,
            )
        };
        let _ = (x, y);

        // something occupying its respawn position? then no respawn
        let spawn_x = Fixed::from_int(map_x);
        let spawn_y = Fixed::from_int(map_y);
        let fits = {
            let map = self.map.as_ref().expect("map loaded");
            let mut ctx = MoveContext::new(map, &mut self.store, &mut self.players, &mut self.rng);
            ctx.leveltime = self.leveltime;
            movement::check_position(&mut ctx, id, spawn_x, spawn_y).ok
        };
        if !fits {
            return;
        }

        // teleport fog at the old spot
        let old_z = self.store.mobj(id).map(|mo| mo.z).unwrap_or(Fixed::ZERO);
        let fog1 = self.store.spawn_mobj(
            self.map.as_ref().expect("map loaded"),
            &mut self.rng,
            SpawnSpec {
                skill: self.gameskill,
                x,
                y,
                z: SpawnZ::At(old_z),
                mobj_type: MobjType::MT_TFOG,
            },
        );
        self.sound_start_actor(fog1, crate::info::SfxId::sfx_telept);

        // teleport fog at the new spot
        let floorz = {
            let map = self.map.as_ref().expect("map loaded");
            let subsector = crate::mobj::point_in_subsector(map, spawn_x, spawn_y);
            subsector
                .and_then(|ss| map.subsectors.get(ss))
                .and_then(|s| s.sector)
                .map(|s| map.sectors[s].floor_height)
                .unwrap_or(Fixed::ZERO)
        };
        let fog2 = self.store.spawn_mobj(
            self.map.as_ref().expect("map loaded"),
            &mut self.rng,
            SpawnSpec {
                skill: self.gameskill,
                x: spawn_x,
                y: spawn_y,
                z: SpawnZ::At(floorz),
                mobj_type: MobjType::MT_TFOG,
            },
        );
        self.sound_start_actor(fog2, crate::info::SfxId::sfx_telept);

        // spawn the new monster and remove the old one
        let z = if MOBJ_INFO[mobj_type as usize].has_flag(crate::info::MF_SPAWNCEILING) {
            SpawnZ::Ceiling
        } else {
            SpawnZ::Floor
        };
        let new = self.store.spawn_mobj(
            self.map.as_ref().expect("map loaded"),
            &mut self.rng,
            SpawnSpec {
                skill: self.gameskill,
                x: spawn_x,
                y: spawn_y,
                z,
                mobj_type,
            },
        );
        if let Some(old_spawnpoint) = self.store.mobj(id).map(|mo| mo.spawnpoint) {
            if let Some(mo) = self.store.mobj_mut(new) {
                mo.spawnpoint = old_spawnpoint;
                mo.angle = Angle::from_raw(
                    Angle::ANG45
                        .to_raw()
                        .wrapping_mul((i32::from(mo.spawnpoint.angle) / 45) as u32),
                );
                if ambush {
                    mo.flags |= crate::info::MF_AMBUSH;
                }
                mo.reactiontime = 18;
            }
        }

        self.store.remove_mobj(id, self.leveltime);
    }

    /// The specials pass.
    fn run_special_pass(&mut self) {
        self.with_special_ctx(specials::run_thinkers);
    }

    /// `P_RespawnSpecials`.
    fn respawn_specials(&mut self) {
        let Some(map) = self.map.as_ref() else { return };
        let report = self.store.respawn_specials(
            map,
            &mut self.rng,
            self.gameskill,
            self.deathmatch_mode,
            self.leveltime,
        );
        if let Some(fog) = report.fog {
            if let Some(sfx) = report.sfx {
                self.sound_start_actor(fog, sfx);
            }
        }
    }

    /// Run a `set_mobj_state_and_run_actions` chain with a shared event
    /// buffer, then dispatch the accumulated events.
    fn set_mobj_state_and_run_actions(&mut self, actor: ThinkerId, state: StateId) {
        let mut events = Vec::new();
        {
            let map = self.map.as_ref().expect("map loaded");
            let rules = self.game_rules();
            let mut ctx = crate::enemy::EnemyContext {
                map,
                store: &mut self.store,
                players: &mut self.players,
                rng: &mut self.rng,
                leveltime: self.leveltime,
                sky_flat: self.sky_flat,
                events: &mut events,
                rules,
                flood: &mut self.sound_flood,
                brain: &mut self.brain,
            };
            crate::enemy::set_mobj_state_and_run_actions(&mut ctx, actor, state);
        }
        self.dispatch_game_events(events);
    }

    /// Dispatch state-chain actions (from the movement layer's reports).
    fn dispatch_state_actions(&mut self, actor: ThinkerId, actions: Vec<StateAction>) {
        if actions.is_empty() {
            return;
        }
        let mut events = Vec::new();
        {
            let map = self.map.as_ref().expect("map loaded");
            let rules = self.game_rules();
            let mut ctx = crate::enemy::EnemyContext {
                map,
                store: &mut self.store,
                players: &mut self.players,
                rng: &mut self.rng,
                leveltime: self.leveltime,
                sky_flat: self.sky_flat,
                events: &mut events,
                rules,
                flood: &mut self.sound_flood,
                brain: &mut self.brain,
            };
            for action in actions {
                crate::enemy::dispatch_action(&mut ctx, actor, action);
            }
        }
        self.dispatch_game_events(events);
    }

    /// `ST_Ticker` - the status bar face machine, one step per tic.
    fn st_ticker(&mut self) {
        // vanilla: st_randomnumber = M_Random() runs first, every tic,
        // unconditionally - the shared stream must stay in lockstep
        // between the machines even when there is no avatar to stare at
        let face_random = self.rng.next();
        let console = self.consoleplayer;
        if !self.playeringame[console] {
            return;
        }
        let Some(avatar) = self.players[console].mo else {
            return;
        };
        let Some((health, angle)) = self.store.mobj(avatar).map(|mo| (mo.health, mo.angle)) else {
            return;
        };
        let attacker_angle = self.players[console]
            .attacker
            .and_then(|id| self.store.mobj(id))
            .map(|attacker| {
                crate::bsp::point_to_angle_2(
                    self.store
                        .mobj(avatar)
                        .map(|mo| mo.x)
                        .unwrap_or(Fixed::ZERO),
                    self.store
                        .mobj(avatar)
                        .map(|mo| mo.y)
                        .unwrap_or(Fixed::ZERO),
                    attacker.x,
                    attacker.y,
                )
            });
        let inputs = FaceInputs {
            health,
            damage_count: self.players[console].damagecount,
            bonus_count: self.players[console].bonuscount,
            attack_down: self.players[console].attackdown,
            invulnerability: self.players[console].powers[Power::Invulnerability as usize] > 0,
            player_angle: angle,
            attacker_angle,
            weapon_owned: self.players[console].weaponowned,
            random: i32::from(face_random),
        };
        self.face.tick(&inputs);
    }

    /// `AM_Ticker`.
    fn am_ticker(&mut self) {
        let console = self.consoleplayer;
        if !self.playeringame[console] {
            return;
        }
        let Some(mo) = self.players[console].mo.and_then(|id| self.store.mobj(id)) else {
            return;
        };
        let marker = PlayerMarker {
            x: mo.x,
            y: mo.y,
            angle: mo.angle,
            invisible: self.players[console].powers[Power::Invisibility as usize] > 0,
            is_local: true,
        };
        let mut markers = Vec::new();
        for index in 0..MAXPLAYERS {
            if !self.playeringame[index] {
                continue;
            }
            let Some(mo) = self.players[index].mo.and_then(|id| self.store.mobj(id)) else {
                continue;
            };
            markers.push(PlayerMarker {
                x: mo.x,
                y: mo.y,
                angle: mo.angle,
                invisible: self.players[index].powers[Power::Invisibility as usize] > 0,
                is_local: index == console,
            });
        }
        let things: Vec<crate::automap::ThingMarker> = if self.automap.cheat_level() == 2 {
            self.store
                .thinkers()
                .filter_map(|(_, thinker)| match thinker {
                    Thinker::Mobj(mo) => Some(crate::automap::ThingMarker {
                        x: mo.x,
                        y: mo.y,
                        angle: mo.angle,
                    }),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        let view = crate::automap::AutomapView {
            me: marker,
            players: &markers,
            netgame: self.netgame,
            deathmatch: self.deathmatch,
            singledemo: self.singledemo,
            all_map: self.players[console].powers[Power::AllMap as usize] > 0,
            things: &things,
        };
        self.automap.tick(&view);
    }

    /// `HU_Ticker`.
    fn hu_ticker(&mut self) {
        let console = self.consoleplayer;
        let message = self.players[console].message.take();
        let state = crate::hud::HudState {
            message: message.as_deref(),
            show_messages: self.show_messages,
            force_message: std::mem::take(&mut self.message_force),
            automap_active: self.automap.is_active(),
            title: "",
        };
        self.hud.tick(&state);
    }

    /// `WI_Ticker` with event application.
    fn intermission_ticker(&mut self) {
        let mut attack = [false; MAXPLAYERS];
        let mut use_button = [false; MAXPLAYERS];
        for i in 0..MAXPLAYERS {
            if self.playeringame[i] {
                attack[i] = self.players[i].cmd.buttons & BT_ATTACK != 0;
                use_button[i] = self.players[i].cmd.buttons & BT_USE != 0;
            }
        }
        let input = IntermissionInput { attack, use_button };
        let mut world_done = false;
        if let Some(intermission) = self.intermission.as_mut() {
            intermission.tick(&mut self.rng, &input);
            for event in intermission.drain_events() {
                match event {
                    IntermissionEvent::Music(music) => {
                        let id = match music {
                            crate::intermission::IntermissionMusic::Dm2Int => {
                                crate::info::MusicId::mus_dm2int
                            }
                            crate::intermission::IntermissionMusic::Inter => {
                                crate::info::MusicId::mus_inter
                            }
                        };
                        self.change_music(id);
                    }
                    IntermissionEvent::Sfx(sfx) => {
                        let id = match sfx {
                            crate::intermission::IntermissionSfx::Pistol => {
                                crate::info::SfxId::sfx_pistol
                            }
                            crate::intermission::IntermissionSfx::Barexp => {
                                crate::info::SfxId::sfx_barexp
                            }
                            crate::intermission::IntermissionSfx::Pldeth => {
                                crate::info::SfxId::sfx_pldeth
                            }
                            crate::intermission::IntermissionSfx::Slop => {
                                crate::info::SfxId::sfx_slop
                            }
                            crate::intermission::IntermissionSfx::SgCock => {
                                crate::info::SfxId::sfx_sgcock
                            }
                        };
                        self.sound_start_global(id);
                    }
                    IntermissionEvent::WorldDone => world_done = true,
                }
            }
        }
        if world_done {
            self.world_done();
        }
    }

    /// `F_Ticker` with event application.
    fn finale_ticker(&mut self) {
        let any_button =
            (0..MAXPLAYERS).any(|i| self.playeringame[i] && self.players[i].cmd.buttons != 0);
        let cast_tables = crate::game::GameCastTables::new();
        let events = if let Some(finale) = self.finale.as_mut() {
            finale.tick(&crate::finale::FinaleInput { any_button }, &cast_tables);
            finale.drain_events()
        } else {
            Vec::new()
        };
        self.apply_finale_events(events);
    }

    /// Apply finale events (music, sounds, world done, forced wipes).
    fn apply_finale_events(&mut self, events: Vec<crate::finale::FinaleEvent>) {
        for event in events {
            match event {
                crate::finale::FinaleEvent::Music(music) => {
                    let id = match music {
                        crate::finale::FinaleMusic::Victor => crate::info::MusicId::mus_victor,
                        crate::finale::FinaleMusic::ReadM => crate::info::MusicId::mus_read_m,
                        crate::finale::FinaleMusic::Evil => crate::info::MusicId::mus_evil,
                        crate::finale::FinaleMusic::Bunny => crate::info::MusicId::mus_bunny,
                    };
                    self.change_music(id);
                }
                crate::finale::FinaleEvent::Sound(sound) => {
                    self.sound_start_global(finale_sound_sfx(sound));
                }
                crate::finale::FinaleEvent::WorldDone => {
                    self.world_done();
                }
                crate::finale::FinaleEvent::ForceWipe => {
                    self.wipe_needed = true;
                }
            }
        }
    }
}

/// The reverse of the cast table's SfxId → FinaleSound mapping.
pub(crate) fn finale_sound_sfx(sound: crate::finale::FinaleSound) -> crate::info::SfxId {
    use crate::finale::FinaleSound as F;
    use crate::info::SfxId as S;
    match sound {
        F::Pistol => S::sfx_pistol,
        F::Shotgn => S::sfx_shotgn,
        F::Claw => S::sfx_claw,
        F::Sgtatk => S::sfx_sgtatk,
        F::Vilatk => S::sfx_vilatk,
        F::Dshtgn => S::sfx_dshtgn,
        F::Skeswg => S::sfx_skeswg,
        F::Skepch => S::sfx_skepch,
        F::Skeatk => S::sfx_skeatk,
        F::Firsht => S::sfx_firsht,
        F::Sklatk => S::sfx_sklatk,
        F::Rlaunc => S::sfx_rlaunc,
        F::Plasma => S::sfx_plasma,
        F::Posit1 => S::sfx_posit1,
        F::Posit2 => S::sfx_posit2,
        F::Vilsit => S::sfx_vilsit,
        F::Skesit => S::sfx_skesit,
        F::Mansit => S::sfx_mansit,
        F::Bgsit1 => S::sfx_bgsit1,
        F::Sgtsit => S::sfx_sgtsit,
        F::Cacsit => S::sfx_cacsit,
        F::Brssit => S::sfx_brssit,
        F::Kntsit => S::sfx_kntsit,
        F::Spisit => S::sfx_spisit,
        F::Bspsit => S::sfx_bspsit,
        F::Cybsit => S::sfx_cybsit,
        F::Pesit => S::sfx_pesit,
        F::Podth1 => S::sfx_podth1,
        F::Podth2 => S::sfx_podth2,
        F::Vildth => S::sfx_vildth,
        F::Skedth => S::sfx_skedth,
        F::Mandth => S::sfx_mandth,
        F::Bgdth1 => S::sfx_bgdth1,
        F::Sgtdth => S::sfx_sgtdth,
        F::Cacdth => S::sfx_cacdth,
        F::Brsdth => S::sfx_brsdth,
        F::Kntdth => S::sfx_kntdth,
        F::Firxpl => S::sfx_firxpl,
        F::Spidth => S::sfx_spidth,
        F::Bspdth => S::sfx_bspdth,
        F::Cybdth => S::sfx_cybdth,
        F::Pedth => S::sfx_pedth,
        F::Pldeth => S::sfx_pldeth,
    }
}

// ==================================================================
// event dispatchers (gameplay effects leave the sim here)
// ==================================================================

impl Game {
    /// Build a specials context from this tic's world, run `f`, and
    /// drain the queued thinkers and events back out.
    fn with_special_ctx<R>(&mut self, f: impl FnOnce(&mut SpecialContext<'_>) -> R) -> R {
        let resolver = |raw: &[u8; 8]| -> Option<Fixed> {
            self.render
                .textures
                .lookup_name(raw)
                .map(|i| self.render.textures.texture_height(i))
        };
        let map = self.map.as_mut().expect("map loaded during play");
        let mut ctx = SpecialContext::new(map, &mut self.store, &mut self.players, &mut self.rng)
            .with_texture_height(&resolver);
        ctx.leveltime = self.leveltime;
        ctx.skill = self.gameskill;
        ctx.boss_level = self.gamemap == 30 && self.gamemode.is_commercial();
        ctx.pending_thinkers = std::mem::take(&mut self.pending_thinkers);
        let result = f(&mut ctx);
        self.pending_thinkers = std::mem::take(&mut ctx.pending_thinkers);
        let events = std::mem::take(&mut ctx.events);
        self.dispatch_special_events(events);
        result
    }

    /// Run one line-special activation (use/cross/shoot).
    fn handle_line_activation(&mut self, line: usize, action: LineAction) {
        let resolver = |raw: &[u8; 8]| -> Option<Fixed> {
            self.render
                .textures
                .lookup_name(raw)
                .map(|i| self.render.textures.texture_height(i))
        };
        {
            let map = self.map.as_mut().expect("map loaded");
            let mut ctx =
                SpecialContext::new(map, &mut self.store, &mut self.players, &mut self.rng)
                    .with_texture_height(&resolver);
            ctx.leveltime = self.leveltime;
            ctx.skill = self.gameskill;
            ctx.boss_level = self.gamemap == 30 && self.gamemode.is_commercial();
            ctx.pending_thinkers = std::mem::take(&mut self.pending_thinkers);
            let buttons = &mut self.buttons;
            let switches = &mut self.switches;
            {
                let mut state = SwitchState {
                    list: switches,
                    buttons,
                };
                let _ = specials::handle_line_activation(&mut ctx, &mut state, line, action);
            }
            self.pending_thinkers = std::mem::take(&mut ctx.pending_thinkers);
            let events = std::mem::take(&mut ctx.events);
            self.dispatch_special_events(events);
        }
    }

    /// A movement-layer event, routed to the gameplay layer.
    fn dispatch_move_event(&mut self, event: MoveEvent) {
        match event {
            MoveEvent::Damage {
                target,
                inflictor,
                source,
                damage,
            } => {
                self.damage_mobj(target, inflictor, source, damage);
            }
            MoveEvent::Pickup { special, toucher } => {
                self.touch_special_thing(special, toucher);
            }
            MoveEvent::CrossSpecial { line, side, thing } => {
                self.handle_line_activation(line, LineAction::Cross { side, thing });
            }
            MoveEvent::Sound { actor, sfx } => {
                self.sound_start_actor(actor, sfx);
            }
        }
    }

    /// An enemy-layer event, routed to the gameplay layer.
    fn dispatch_game_events(&mut self, events: Vec<crate::enemy::GameEvent>) {
        for event in events {
            match event {
                crate::enemy::GameEvent::Move(mv) => self.dispatch_move_event(mv),
                crate::enemy::GameEvent::Sound { actor, sfx } => match actor {
                    Some(actor) => self.sound_start_actor(actor, sfx),
                    None => self.sound_start_global(sfx),
                },
                crate::enemy::GameEvent::Damage {
                    target,
                    inflictor,
                    source,
                    damage,
                } => {
                    self.damage_mobj(target, inflictor, source, damage);
                }
                crate::enemy::GameEvent::FloorLowerToLowest { tag } => {
                    self.floor_event(tag, FloorEvent::LowerToLowest);
                }
                crate::enemy::GameEvent::FloorRaiseToTexture { tag } => {
                    self.floor_event(tag, FloorEvent::RaiseToTexture);
                }
                crate::enemy::GameEvent::DoorOpen { tag, blaze } => {
                    self.door_event(tag, blaze);
                }
                crate::enemy::GameEvent::ExitLevel => {
                    self.exit_level(false);
                }
                crate::enemy::GameEvent::UseSpecialLine { line, thing } => {
                    // a monster bumped a special line; vanilla calls
                    // P_UseSpecialLine(thing, line, 0)
                    self.handle_line_activation(line, LineAction::Use { side: 0, thing });
                }
                crate::enemy::GameEvent::ShootSpecial { line, thing } => {
                    self.handle_line_activation(line, LineAction::Shoot { thing });
                }
            }
        }
    }

    /// Handle an inter-layer event.
    fn dispatch_inter_events(&mut self, events: Vec<InterEvent>) {
        for event in events {
            match event {
                InterEvent::Sound { sfx } => self.sound_start_global(sfx),
                InterEvent::DropWeapon { player } => {
                    let mut events = Vec::new();
                    {
                        let rules = self.game_rules();
                        let map = self.map.as_ref().expect("map loaded");
                        let mut ctx = crate::enemy::EnemyContext {
                            map,
                            store: &mut self.store,
                            players: &mut self.players,
                            rng: &mut self.rng,
                            leveltime: self.leveltime,
                            sky_flat: self.sky_flat,
                            events: &mut events,
                            rules,
                            flood: &mut self.sound_flood,
                            brain: &mut self.brain,
                        };
                        weapons::drop_weapon(&mut ctx, player, self.gamemode);
                    }
                    self.dispatch_game_events(events);
                }
                InterEvent::StopAutomap => self.automap.stop(),
            }
        }
    }

    /// Handle a specials-layer event.
    fn dispatch_special_events(&mut self, events: Vec<SpecialEvent>) {
        for event in events {
            match event {
                SpecialEvent::GlobalSound { sfx } => self.sound_start_global(sfx),
                SpecialEvent::SectorSound { sector, sfx } => self.sound_start_sector(sector, sfx),
                SpecialEvent::ActorSound { actor, sfx } => self.sound_start_actor(actor, sfx),
                SpecialEvent::Move(mv) => self.dispatch_move_event(mv),
                SpecialEvent::Damage {
                    target,
                    inflictor,
                    source,
                    damage,
                    blood: _,
                } => {
                    self.damage_mobj(target, inflictor, source, damage);
                }
                SpecialEvent::GibCorpse { id, momx, momy } => self.gib_corpse(id, momx, momy),
                SpecialEvent::CrushRemove { id } => {
                    self.store.remove_mobj(id, self.leveltime);
                }
                SpecialEvent::SwitchToggled { .. } => {
                    // the toggle sound is emitted by the switch layer itself
                }
                SpecialEvent::ExitLevel { secret } => self.exit_level(secret),
            }
        }
    }

    /// Apply damage through the inter layer.
    pub(crate) fn damage_mobj(
        &mut self,
        target: ThinkerId,
        inflictor: Option<ThinkerId>,
        source: Option<ThinkerId>,
        damage: i32,
    ) {
        let mut game_events = Vec::new();
        let mut inter_events = Vec::new();
        {
            let rules = self.game_rules();
            let map = self.map.as_ref().expect("map loaded");
            let mut ctx = InterContext::new(
                map,
                &mut self.store,
                &mut self.players,
                &mut self.rng,
                &mut self.sound_flood,
                &mut self.brain,
                &mut game_events,
                &mut inter_events,
            );
            ctx.leveltime = self.leveltime;
            ctx.rules = rules;
            ctx.deathmatch = self.deathmatch_mode;
            ctx.consoleplayer = self.consoleplayer;
            ctx.automap_active = self.automap.is_active();
            inter::damage_mobj(&mut ctx, target, inflictor, source, damage);
        }
        self.dispatch_inter_events(inter_events);
        self.dispatch_game_events(game_events);
    }

    /// Apply a pickup through the inter layer.
    fn touch_special_thing(&mut self, special: ThinkerId, toucher: ThinkerId) {
        let mut game_events = Vec::new();
        let mut inter_events = Vec::new();
        {
            let rules = self.game_rules();
            let map = self.map.as_ref().expect("map loaded");
            let mut ctx = InterContext::new(
                map,
                &mut self.store,
                &mut self.players,
                &mut self.rng,
                &mut self.sound_flood,
                &mut self.brain,
                &mut game_events,
                &mut inter_events,
            );
            ctx.leveltime = self.leveltime;
            ctx.rules = rules;
            ctx.deathmatch = self.deathmatch_mode;
            ctx.consoleplayer = self.consoleplayer;
            ctx.automap_active = self.automap.is_active();
            inter::touch_special_thing(&mut ctx, special, toucher);
        }
        self.dispatch_inter_events(inter_events);
        self.dispatch_game_events(game_events);
    }

    /// The corpse-gib path a crushing sector triggers (vanilla's
    /// `PIT_ChangeSector` gib branch).
    fn gib_corpse(&mut self, id: ThinkerId, momx: Fixed, momy: Fixed) {
        let blood_spawn = self.store.mobj(id).map(|mo| (mo.x, mo.y, mo.z));
        self.set_mobj_state_and_run_actions(id, StateId::S_GIBS);
        if let Some((x, y, z)) = blood_spawn {
            let blood = self.store.spawn_mobj(
                self.map.as_ref().expect("map loaded"),
                &mut self.rng,
                SpawnSpec {
                    skill: self.gameskill,
                    x,
                    y,
                    z: SpawnZ::At(z),
                    mobj_type: MobjType::MT_BLOOD,
                },
            );
            if let Some(mo) = self.store.mobj_mut(blood) {
                mo.momx = momx;
                mo.momy = momy;
            }
        }
    }

    /// A tag-based floor event from the boss death (666/667).
    fn floor_event(&mut self, tag: i32, kind: FloorEvent) {
        if tag < 0 || tag > i32::from(i16::MAX) {
            return;
        }
        let floor_kind = match kind {
            FloorEvent::LowerToLowest => crate::specials::FloorKind::LowerFloorToLowest,
            FloorEvent::RaiseToTexture => crate::specials::FloorKind::RaiseToTexture,
        };
        self.with_special_ctx(|ctx| {
            let _ = crate::specials::ev_do_floor_tag(ctx, tag as i16, floor_kind);
        });
    }

    /// A tag-based door event from the boss/keener death.
    fn door_event(&mut self, tag: i32, blaze: bool) {
        if tag < 0 || tag > i32::from(i16::MAX) {
            return;
        }
        let kind = if blaze {
            crate::specials::DoorKind::BlazeOpen
        } else {
            crate::specials::DoorKind::Open
        };
        self.with_special_ctx(|ctx| {
            let _ = crate::specials::ev_do_door_tag(ctx, tag as i16, kind);
        });
    }
}

/// Which event a tag-based floor trigger latches onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FloorEvent {
    LowerToLowest,
    RaiseToTexture,
}

// ==================================================================
// the level exit flow (G_ExitLevel, G_DoCompleted, ...)
// ==================================================================

impl Game {
    /// `G_ExitLevel` / `G_SecretExitLevel` - the level is over.
    fn exit_level(&mut self, secret: bool) {
        // IF NO WOLF3D LEVELS, NO SECRET EXIT
        let secret = if secret
            && self.gamemode.is_commercial()
            && self.resources.find_lump("MAP31").is_none()
        {
            false
        } else {
            secret
        };
        self.secretexit = secret;
        self.game_action = GameAction::Completed;
    }

    /// `G_DoCompleted`.
    fn do_completed(&mut self) {
        let commercial = self.gamemode.is_commercial();

        self.game_action = GameAction::Nothing;

        // take away the cards and powers
        for i in 0..MAXPLAYERS {
            if self.playeringame[i] {
                let player = &mut self.players[i];
                player.powers = [0; crate::player::NUM_POWERS];
                player.cards = [false; crate::player::NUM_CARDS];
                player.extralight = 0;
                player.fixedcolormap = 0;
                player.damagecount = 0;
                player.bonuscount = 0;
                if let Some(mo) = player.mo.and_then(|id| self.store.mobj_mut(id)) {
                    mo.flags &= !crate::info::MF_SHADOW;
                }
            }
        }

        if self.automap.is_active() {
            self.automap.stop();
        }

        if !commercial {
            match self.gamemap {
                8 => {
                    self.game_action = GameAction::Victory;
                    return;
                }
                9 => {
                    // returning from the secret level
                    for i in 0..MAXPLAYERS {
                        self.players[i].didsecret = true;
                    }
                }
                _ => {}
            }
        }

        // the intermission stat screen
        let setup = self.build_wminfo();
        self.gamestate = GameState::Intermission;
        self.wipe_needed = true;
        match Intermission::start(&self.resources, setup, &mut self.rng) {
            Ok(intermission) => self.intermission = Some(intermission),
            Err(err) => {
                tracing::warn!(
                    "intermission assets failed to load ({err}); \
                     skipping to the next level"
                );
                self.world_done();
            }
        }
    }

    /// `F_StartFinale`.
    fn start_finale(&mut self) {
        self.game_action = GameAction::Nothing;
        self.gamestate = GameState::Finale;
        self.wipe_needed = true;
        let kind = if self.gamemode.is_commercial() {
            crate::finale::FinaleKind::Commercial {
                map: self.gamemap as i32,
            }
        } else {
            crate::finale::FinaleKind::Episode {
                episode: self.gameepisode as i32,
                retail: self.gamemode == GameMode::Retail,
            }
        };
        self.finale = Some(crate::finale::Finale::new(kind));
    }

    /// Build the `wbstartstruct_t` for the intermission.
    fn build_wminfo(&mut self) -> IntermissionSetup {
        let commercial = self.gamemode.is_commercial();
        let episode = self.gameepisode as i32;
        let map = self.gamemap as i32;
        let secretexit = self.secretexit;

        let next0: i32 = if commercial {
            match (secretexit, self.gamemap) {
                (true, 15) => 30,        // enters map31 (0-based)
                (true, 31) => 31,        // enters map32 (0-based)
                (_, 31) | (_, 32) => 15, // back to map16 (0-based)
                // vanilla: wminfo.next is 0-biased, unlike gamemap,
                // so next == gamemap enters the map after this one
                _ => map,
            }
        } else if secretexit {
            8 // entering the secret level (0-based)
        } else if self.gamemap == 9 {
            // returning from the secret level
            match self.gameepisode {
                1 => 3,
                2 => 5,
                3 => 6,
                _ => 2,
            }
        } else {
            map // vanilla: wminfo.next = gamemap (0-biased)
        };
        self.next_level_zero_based = next0;

        let partime = if commercial {
            35 * CPARS[(self.gamemap as usize)
                .saturating_sub(1)
                .min(CPARS.len() - 1)]
        } else {
            let ep = episode.max(1) as usize;
            let m = map.max(1) as usize;
            35 * PARS[ep.min(3)][m.min(9)]
        };

        let mut players = [IntermissionPlayer::default(); MAXPLAYERS];
        for (index, player) in players.iter_mut().enumerate() {
            player.in_game = self.playeringame[index];
            player.kills = self.players[index].killcount;
            player.items = self.players[index].itemcount;
            player.secret = self.players[index].secretcount;
            player.time_tics = self.leveltime;
            player.frags = self.players[index].frags;
        }

        IntermissionSetup {
            game_mode: self.gamemode,
            episode: episode - 1,
            last_level: map - 1,
            next_level: next0,
            did_secret: self.players[self.consoleplayer].didsecret,
            local_player: self.consoleplayer,
            players,
            max_kills: self.total_kills,
            max_items: self.total_items,
            max_secret: self.total_secret,
            par_time_tics: partime,
            deathmatch: self.deathmatch,
            netgame: self.netgame,
        }
    }

    /// `G_WorldDone`.
    fn world_done(&mut self) {
        if self.secretexit {
            self.players[self.consoleplayer].didsecret = true;
        }

        if self.gamemode.is_commercial() {
            match self.gamemap {
                15 | 31 => {
                    if self.secretexit {
                        self.start_finale();
                        return;
                    }
                }
                6 | 11 | 20 | 30 => {
                    self.start_finale();
                    return;
                }
                _ => {}
            }
        }

        self.game_action = GameAction::WorldDone;
    }

    /// `G_DoWorldDone` - load the next map and go back to the level.
    fn do_world_done(&mut self) {
        self.gamestate = GameState::Level;
        self.gamemap = (self.next_level_zero_based + 1) as u8;
        self.game_action = GameAction::Nothing;
        self.do_load_level();
    }
}

// ==================================================================
// status bar state, save slots, save/load
// ==================================================================

/// Convert the wire byte to a `GameMode`.
fn game_mode_wire(gamemode: GameMode) -> u8 {
    match gamemode {
        GameMode::Shareware => 0,
        GameMode::Registered => 1,
        GameMode::Commercial => 2,
        GameMode::Retail => 3,
    }
}

fn game_mode_from_wire(byte: u8) -> GameMode {
    match byte {
        0 => GameMode::Shareware,
        1 => GameMode::Registered,
        3 => GameMode::Retail,
        // everything else, including the original's "indetermined"
        _ => GameMode::Commercial,
    }
}

fn playerstate_wire(state: PlayerState) -> u8 {
    match state {
        PlayerState::Live => 0,
        PlayerState::Dead => 1,
        PlayerState::Reborn => 2,
    }
}

fn playerstate_from_wire(byte: u8) -> Option<PlayerState> {
    match byte {
        0 => Some(PlayerState::Live),
        1 => Some(PlayerState::Dead),
        2 => Some(PlayerState::Reborn),
        _ => None,
    }
}

fn button_where_wire(where_: specials::ButtonWhere) -> u8 {
    match where_ {
        specials::ButtonWhere::Top => 0,
        specials::ButtonWhere::Middle => 1,
        specials::ButtonWhere::Bottom => 2,
    }
}

fn button_where_from_wire(byte: u8) -> Option<specials::ButtonWhere> {
    match byte {
        0 => Some(specials::ButtonWhere::Top),
        1 => Some(specials::ButtonWhere::Middle),
        2 => Some(specials::ButtonWhere::Bottom),
        _ => None,
    }
}

fn door_kind_from_wire(byte: u8) -> Option<specials::DoorKind> {
    Some(match byte {
        0 => specials::DoorKind::Normal,
        1 => specials::DoorKind::Close30ThenOpen,
        2 => specials::DoorKind::Close,
        3 => specials::DoorKind::Open,
        4 => specials::DoorKind::RaiseIn5Mins,
        5 => specials::DoorKind::BlazeRaise,
        6 => specials::DoorKind::BlazeOpen,
        7 => specials::DoorKind::BlazeClose,
        _ => return None,
    })
}

fn floor_kind_from_wire(byte: u8) -> Option<specials::FloorKind> {
    Some(match byte {
        0 => specials::FloorKind::LowerFloor,
        1 => specials::FloorKind::LowerFloorToLowest,
        2 => specials::FloorKind::TurboLower,
        3 => specials::FloorKind::RaiseFloor,
        4 => specials::FloorKind::RaiseFloorToNearest,
        5 => specials::FloorKind::RaiseToTexture,
        6 => specials::FloorKind::LowerAndChange,
        7 => specials::FloorKind::RaiseFloor24,
        8 => specials::FloorKind::RaiseFloor24AndChange,
        9 => specials::FloorKind::RaiseFloorCrush,
        10 => specials::FloorKind::RaiseFloorTurbo,
        11 => specials::FloorKind::DonutRaise,
        12 => specials::FloorKind::RaiseFloor512,
        _ => return None,
    })
}

fn ceiling_kind_from_wire(byte: u8) -> Option<specials::CeilingKind> {
    Some(match byte {
        0 => specials::CeilingKind::LowerToFloor,
        1 => specials::CeilingKind::RaiseToHighest,
        2 => specials::CeilingKind::LowerAndCrush,
        3 => specials::CeilingKind::CrushAndRaise,
        4 => specials::CeilingKind::FastCrushAndRaise,
        5 => specials::CeilingKind::SilentCrushAndRaise,
        _ => return None,
    })
}

fn plat_kind_from_wire(byte: u8) -> Option<specials::PlatKind> {
    Some(match byte {
        0 => specials::PlatKind::PerpetualRaise,
        1 => specials::PlatKind::DownWaitUpStay,
        2 => specials::PlatKind::RaiseAndChange,
        3 => specials::PlatKind::RaiseToNearestAndChange,
        4 => specials::PlatKind::BlazeDWUS,
        _ => return None,
    })
}

fn plat_status_from_wire(byte: u8) -> Option<specials::PlatStatus> {
    Some(match byte {
        0 => specials::PlatStatus::Up,
        1 => specials::PlatStatus::Down,
        2 => specials::PlatStatus::Waiting,
        3 => specials::PlatStatus::InStasis,
        _ => return None,
    })
}

/// Whether the Warp command line maps onto (episode, map) for the
/// active game mode.
fn warp_to_episode_map(gamemode: GameMode, warp: Option<Warp>) -> (u8, u8) {
    let commercial = gamemode.is_commercial();
    match (warp, commercial) {
        (None, _) => (1, 1),
        (Some(Warp::Map(map)), true) => (1, map.clamp(1, 32) as u8),
        (Some(Warp::EpisodeMap { episode: _, map }), true) => (1, map.clamp(1, 32) as u8),
        (Some(Warp::Map(map)), false) => (1, map.clamp(1, 9) as u8),
        (Some(Warp::EpisodeMap { episode, map }), false) => {
            let episode_max = match gamemode {
                GameMode::Retail => 4,
                GameMode::Shareware => 1,
                _ => 3,
            };
            (episode.clamp(1, episode_max) as u8, map.clamp(1, 9) as u8)
        }
    }
}

/// `status_state` for the status bar - built from the player's world
/// view into plain data for the bar.
pub(super) fn status_state(game: &Game, display: usize) -> StatusState {
    let player = &game.players[display];
    let health = game.players[display]
        .mo
        .and_then(|id| game.store.mobj(id))
        .map(|mo| mo.health)
        .unwrap_or(0);
    let ready_ammo = weapons::WEAPON_INFO[player.readyweapon.index()]
        .ammo
        .map(|kind| player.ammo[kind.index()]);

    let frags = if game.deathmatch {
        let mut total = 0;
        for (index, other) in game.players.iter().enumerate() {
            if index != display {
                total += other.frags[display];
            } else {
                total -= other.frags[display];
            }
        }
        total
    } else {
        0
    };

    StatusState {
        health,
        armor_points: player.armorpoints,
        ready_weapon_ammo: ready_ammo,
        frags,
        weapon_owned: player.weaponowned,
        ammo: player.ammo,
        max_ammo: player.maxammo,
        cards: player.cards,
        face_index: game.face.face_index(),
        deathmatch: game.deathmatch,
        netgame: game.netgame,
        bar_visible: game.screen_blocks <= 10,
    }
}

/// Refresh the menu's save slot list from the save directory.
pub(super) fn refresh_save_slots(game: &mut Game) {
    let mut slots: [crate::menu::SaveSlotInfo; SAVE_SLOTS] =
        std::array::from_fn(|_| crate::menu::SaveSlotInfo::default());
    for (index, slot) in slots.iter_mut().enumerate() {
        let path = game.save_dir.join(format!("{SAVEGAME_NAME}{index}.dsg"));
        if path.is_file() {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let name = SaveBlock::read_description(&bytes)
                        .unwrap_or_else(|| "(unreadable)".to_string());
                    slot.name = name;
                    slot.available = true;
                }
                Err(_) => {
                    slot.name = "(unreadable)".to_string();
                    slot.available = false;
                }
            }
        }
    }
    game.menu.set_save_names(&slots);
}

impl Game {
    /// `G_SaveGame` - write the current level state to a slot.
    pub fn save_game_slot(&mut self, slot: usize, description: &str) {
        let block = self.capture_save_block();
        let bytes = block.write_with_description(description);
        let path = self.save_dir.join(format!("{SAVEGAME_NAME}{slot}.dsg"));
        if let Err(err) = crate::scratch::atomic_write(&path, &bytes) {
            tracing::error!("save game failed: {err}");
            return;
        }
        self.players[self.consoleplayer].message = Some(HudMessage::GgSaved.as_str().to_string());
        refresh_save_slots(self);
    }

    /// `G_LoadGame` - load a save slot and rebuild the world.
    pub fn load_game_slot(&mut self, slot: usize) {
        let path = self.save_dir.join(format!("{SAVEGAME_NAME}{slot}.dsg"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::error!("save slot {} is unreadable: {err}", slot);
                return;
            }
        };
        if let Err(err) = self.restore_save_block(&bytes) {
            tracing::error!("save slot {} rejected: {err}", slot);
            return;
        }
        self.paused = false;
    }

    /// End of a demo stream (marker byte). Single-demo mode quits the
    /// game; otherwise the engine goes back to the title screen.
    fn end_demo_playback(&mut self) {
        tracing::info!("demo {} finished", self.demoname);
        self.demo_playback = None;
        self.demoplayback = false;
        self.netgame = false;
        self.deathmatch = false;
        self.playeringame = [true, false, false, false];
        self.respawnmonsters = false;
        self.fastparm = false;
        self.nomonsters = false;

        if self.singledemo {
            // quit after playing a demo, like I_Quit
            self.quit_requested = true;
        } else {
            self.gamestate = GameState::DemoScreen;
            self.wipe_needed = true;
        }
    }

    /// Stop recording the demo (test hook).
    #[cfg(test)]
    pub(crate) fn stop_demo_recording(&mut self) {
        self.end_demo_recording();
    }

    /// Flush the demo recording to disk (marker byte included).
    pub(crate) fn end_demo_recording(&mut self) {
        let Some(recording) = self.demo_recording.take() else {
            return;
        };
        self.demorecording = false;
        let bytes = recording.finish();
        eprintln!("demo finish: {} bytes", bytes.len());
        let name = if self.demoname.ends_with(".lmp") {
            self.demoname.clone()
        } else {
            format!("{}.lmp", self.demoname)
        };
        let path = self.save_dir.join(&name);
        match crate::scratch::atomic_write(&path, &bytes) {
            Ok(()) => {
                tracing::info!("demo written to {}", path.display());
                self.players[self.consoleplayer].message = Some(format!("Demo recorded: {name}"));
            }
            Err(err) => tracing::error!("demo write failed: {err}"),
        }
    }

    /// Serialize the current world state into a [`SaveBlock`].
    fn capture_save_block(&self) -> SaveBlock {
        let mut block = SaveBlock {
            game_mode: game_mode_wire(self.gamemode),
            gameskill: self.gameskill as u8,
            gameepisode: self.gameepisode,
            gamemap: self.gamemap,
            deathmatch: self.deathmatch_mode as u8,
            respawn_monsters: u8::from(self.respawnmonsters),
            fast_monsters: u8::from(self.fastparm),
            no_monsters: u8::from(self.nomonsters),
            consoleplayer: self.consoleplayer as u8,
            playeringame: self.playeringame.map(u8::from),
            leveltime: self.leveltime,
            gametic: self.gametic,
            total_kills: self.total_kills,
            total_items: self.total_items,
            total_secret: self.total_secret,
            level_timer: self.level_timer.unwrap_or(-1),
            rng_index: self.rng.index() as u8,
            ..SaveBlock::default()
        };

        // players, in-game slots in order
        for (index, player) in self.players.iter().enumerate() {
            if !self.playeringame[index] {
                continue;
            }
            block.players.push(crate::game::savegame::SavePlayer {
                playerstate: playerstate_wire(player.playerstate),
                health: player.health,
                armorpoints: player.armorpoints,
                armortype: player.armortype,
                powers: player.powers,
                cards: player.cards.map(u8::from),
                backpack: u8::from(player.backpack),
                frags: player.frags,
                readyweapon: player.readyweapon as u8,
                pendingweapon: player.pendingweapon.map(|w| w as u8).unwrap_or(0xff),
                weaponowned: player.weaponowned.map(u8::from),
                ammo: player.ammo,
                maxammo: player.maxammo,
                attackdown: u8::from(player.attackdown),
                usedown: u8::from(player.usedown),
                cheats: player.cheats,
                refire: player.refire,
                killcount: player.killcount,
                itemcount: player.itemcount,
                secretcount: player.secretcount,
                damagecount: player.damagecount,
                bonuscount: player.bonuscount,
                extralight: player.extralight,
                fixedcolormap: player.fixedcolormap,
                colormap: player.colormap,
                didsecret: u8::from(player.didsecret),
                viewz: player.viewz.to_raw(),
                viewheight: player.viewheight.to_raw(),
                deltaviewheight: player.deltaviewheight.to_raw(),
                bob: player.bob.to_raw(),
                psprites: [
                    crate::game::savegame::SavePsp {
                        state: player.psprites[0].state.map(|s| s as u16).unwrap_or(0xffff),
                        tics: player.psprites[0].tics,
                        sx: player.psprites[0].sx.to_raw(),
                        sy: player.psprites[0].sy.to_raw(),
                    },
                    crate::game::savegame::SavePsp {
                        state: player.psprites[1].state.map(|s| s as u16).unwrap_or(0xffff),
                        tics: player.psprites[1].tics,
                        sx: player.psprites[1].sx.to_raw(),
                        sy: player.psprites[1].sy.to_raw(),
                    },
                ],
            });
        }

        // the world
        if let Some(map) = self.map.as_ref() {
            block.sectors = map
                .sectors
                .iter()
                .map(|sector| crate::game::savegame::SaveSector {
                    floor_height: sector.floor_height.to_raw(),
                    ceiling_height: sector.ceiling_height.to_raw(),
                    floor_pic: sector.floor_pic,
                    ceiling_pic: sector.ceiling_pic,
                    light_level: i32::from(sector.light_level),
                    special: i32::from(sector.special),
                    tag: i32::from(sector.tag),
                })
                .collect();
            block.lines = map
                .linedefs
                .iter()
                .map(|line| crate::game::savegame::SaveLine {
                    flags: i32::from(line.flags),
                    special: i32::from(line.special),
                    tag: i32::from(line.tag),
                    side0: u8::from(line.sidenum[0].is_some()),
                    side1: u8::from(line.sidenum[1].is_some()),
                })
                .collect();
            block.sides = map
                .linedefs
                .iter()
                .flat_map(|line| {
                    [line.sidenum[0], line.sidenum[1]]
                        .into_iter()
                        .flatten()
                        .map(|sidenum| {
                            let side = &map.sidedefs[sidenum];
                            crate::game::savegame::SaveSidedef {
                                texture_offset: side.texture_offset.to_raw(),
                                row_offset: side.row_offset.to_raw(),
                                top: side.toptexture,
                                bottom: side.bottomtexture,
                                mid: side.midtexture,
                            }
                        })
                })
                .collect();
        }

        // thinkers, in registry order; target/tracer become positions
        // in that same array
        let index_of: std::collections::HashMap<ThinkerId, usize> = self
            .store
            .thinkers()
            .enumerate()
            .map(|(index, (id, _))| (id, index))
            .collect();
        let ref_index =
            |id: ThinkerId| -> i32 { index_of.get(&id).copied().map(|i| i as i32).unwrap_or(-1) };

        for (_, thinker) in self.store.thinkers() {
            match thinker {
                Thinker::Mobj(mo) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Mobj(
                            crate::game::savegame::SaveMobj {
                                mobj_type: mo.mobj_type as u16,
                                state: mo.state as u16,
                                tics: mo.tics,
                                x: mo.x.to_raw(),
                                y: mo.y.to_raw(),
                                z: mo.z.to_raw(),
                                angle: mo.angle.to_raw(),
                                momx: mo.momx.to_raw(),
                                momy: mo.momy.to_raw(),
                                momz: mo.momz.to_raw(),
                                flags: mo.flags,
                                health: mo.health,
                                movedir: mo.movedir,
                                movecount: mo.movecount,
                                reactiontime: mo.reactiontime,
                                threshold: mo.threshold,
                                lastlook: mo.lastlook,
                                player_index: mo.player.map(|p| p as i8).unwrap_or(-1),
                                spawn_x: mo.spawnpoint.x,
                                spawn_y: mo.spawnpoint.y,
                                spawn_angle: mo.spawnpoint.angle,
                                spawn_type: mo.spawnpoint.thing_type,
                                spawn_flags: mo.spawnpoint.flags,
                                target: mo.target.map(ref_index).unwrap_or(-1),
                                tracer: mo.tracer.map(ref_index).unwrap_or(-1),
                            },
                        ));
                }
                Thinker::Door(door) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::Door(
                                crate::game::savegame::SaveDoor {
                                    kind: door.kind as u8,
                                    sector: door.sector as i32,
                                    topheight: door.topheight.to_raw(),
                                    speed: door.speed.to_raw(),
                                    direction: door.direction,
                                    topwait: door.topwait,
                                    topcountdown: door.topcountdown,
                                },
                            ),
                        ));
                }
                Thinker::FloorMover(floor) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::Floor(
                                crate::game::savegame::SaveFloor {
                                    kind: floor.kind as u8,
                                    crush: u8::from(floor.crush),
                                    sector: floor.sector as i32,
                                    direction: floor.direction,
                                    new_special: floor.new_special,
                                    texture: floor.texture,
                                    floor_dest_height: floor.floor_dest_height.to_raw(),
                                    speed: floor.speed.to_raw(),
                                },
                            ),
                        ));
                }
                Thinker::Plat(plat) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::Plat(
                                crate::game::savegame::SavePlat {
                                    kind: plat.kind as u8,
                                    sector: plat.sector as i32,
                                    speed: plat.speed.to_raw(),
                                    low: plat.low.to_raw(),
                                    high: plat.high.to_raw(),
                                    wait: plat.wait,
                                    count: plat.count,
                                    status: plat.status as u8,
                                    oldstatus: plat.oldstatus as u8,
                                    crush: u8::from(plat.crush),
                                    tag: plat.tag,
                                },
                            ),
                        ));
                }
                Thinker::Ceiling(ceiling) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::Ceiling(
                                crate::game::savegame::SaveCeiling {
                                    kind: ceiling.kind as u8,
                                    sector: ceiling.sector as i32,
                                    bottomheight: ceiling.bottomheight.to_raw(),
                                    topheight: ceiling.topheight.to_raw(),
                                    speed: ceiling.speed.to_raw(),
                                    crush: u8::from(ceiling.crush),
                                    direction: ceiling.direction,
                                    tag: ceiling.tag,
                                    olddirection: ceiling.olddirection,
                                },
                            ),
                        ));
                }
                Thinker::LightFlash(flash) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::LightFlash(
                                crate::game::savegame::SaveLightFlash {
                                    sector: flash.sector as i32,
                                    count: flash.count,
                                    maxlight: flash.maxlight,
                                    minlight: flash.minlight,
                                    maxtime: flash.maxtime,
                                    mintime: flash.mintime,
                                },
                            ),
                        ));
                }
                Thinker::StrobeFlash(strobe) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::StrobeFlash(
                                crate::game::savegame::SaveStrobeFlash {
                                    sector: strobe.sector as i32,
                                    count: strobe.count,
                                    minlight: strobe.minlight,
                                    maxlight: strobe.maxlight,
                                    darktime: strobe.darktime,
                                    brighttime: strobe.brighttime,
                                },
                            ),
                        ));
                }
                Thinker::Glow(glow) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::Glow(
                                crate::game::savegame::SaveGlow {
                                    sector: glow.sector as i32,
                                    minlight: glow.minlight,
                                    maxlight: glow.maxlight,
                                    direction: glow.direction,
                                },
                            ),
                        ));
                }
                Thinker::FireFlicker(flicker) => {
                    block
                        .thinkers
                        .push(crate::game::savegame::SaveThinker::Special(
                            crate::game::savegame::SaveSpecial::FireFlicker(
                                crate::game::savegame::SaveFireFlicker {
                                    sector: flicker.sector as i32,
                                    count: flicker.count,
                                    maxlight: flicker.maxlight,
                                    minlight: flicker.minlight,
                                },
                            ),
                        ));
                }
            }
        }

        // pressed buttons (waiting to revert)
        for button in self.buttons.pressed() {
            block.buttons.push(crate::game::savegame::SaveButton {
                line: button.line as i32,
                where_: button_where_wire(button.where_),
                revert_to: button.revert_to,
                timer: button.timer,
                sector: button.sector as i32,
            });
        }

        block
    }

    /// Load a save block and rebuild the world. Semantic checks on the
    /// incoming data happen against the freshly-loaded level.
    fn restore_save_block(&mut self, bytes: &[u8]) -> EngineResult<()> {
        let block = SaveBlock::read(bytes)?;

        let skill = crate::mobj::Skill::from_level(block.gameskill + 1).ok_or_else(|| {
            crate::game::savegame::SaveGameError::OutOfRange {
                what: "gameskill",
                index: i32::from(block.gameskill),
            }
        })?;
        let episode = block.gameepisode.max(1);
        let map_num = block.gamemap.max(1);

        self.playeringame = block.playeringame.map(|v| v != 0);
        self.deathmatch_mode = match block.deathmatch {
            0 => DeathmatchMode::Off,
            1 => DeathmatchMode::Duel,
            _ => DeathmatchMode::Alt,
        };
        self.deathmatch = block.deathmatch != 0;
        self.respawnmonsters = block.respawn_monsters != 0;
        self.fastparm = block.fast_monsters != 0;
        self.nomonsters = block.no_monsters != 0;
        self.consoleplayer = (block.consoleplayer as usize).min(MAXPLAYERS - 1);
        self.secretexit = false;
        self.gamemode = game_mode_from_wire(block.game_mode);

        // re-init the world the same way a new game does
        self.init_new(skill, episode, map_num);

        // semantic fit against the freshly-loaded level
        let map_ref = self.map.as_ref().expect("init_new loaded the map");
        if block.sectors.len() != map_ref.sectors.len() {
            return Err(crate::game::savegame::SaveGameError::MapMismatch(format!(
                "save has {} sectors, level has {}",
                block.sectors.len(),
                map_ref.sectors.len()
            ))
            .into());
        }
        if block.lines.len() != map_ref.linedefs.len() {
            return Err(crate::game::savegame::SaveGameError::MapMismatch(format!(
                "save has {} lines, level has {}",
                block.lines.len(),
                map_ref.linedefs.len()
            ))
            .into());
        }
        let side_count = map_ref
            .linedefs
            .iter()
            .map(|line| line.sidenum.iter().filter(|s| s.is_some()).count())
            .sum::<usize>();
        if block.sides.len() != side_count {
            return Err(crate::game::savegame::SaveGameError::MapMismatch(format!(
                "save has {} sides, level has {side_count}",
                block.sides.len()
            ))
            .into());
        }

        self.leveltime = block.leveltime;
        self.gametic = block.gametic.max(self.gametic);
        self.total_kills = block.total_kills;
        self.total_items = block.total_items;
        self.total_secret = block.total_secret;
        self.level_timer = if block.level_timer < 0 {
            None
        } else {
            Some(block.level_timer)
        };
        self.rng = DoomRng::with_seed(block.rng_index as usize);

        // world: sectors and lines/sidedefs
        {
            let map = self.map.as_mut().expect("map loaded");
            for (index, save) in block.sectors.iter().enumerate() {
                let sector = &mut map.sectors[index];
                sector.floor_height = Fixed::from_raw(save.floor_height);
                sector.ceiling_height = Fixed::from_raw(save.ceiling_height);
                sector.floor_pic = save.floor_pic;
                sector.ceiling_pic = save.ceiling_pic;
                sector.light_level =
                    save.light_level.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                sector.special = save.special.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                sector.tag = save.tag.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
            let mut sides = block.sides.into_iter();
            for (index, save) in block.lines.iter().enumerate() {
                let line = &mut map.linedefs[index];
                line.flags = save.flags.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                line.special = save.special.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                line.tag = save.tag.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                if save.side0 != 0 {
                    let Some(sidedef) = line.sidenum[0] else {
                        return Err(crate::game::savegame::SaveGameError::MapMismatch(format!(
                            "line {index} lost its front side"
                        ))
                        .into());
                    };
                    let save = sides.next().expect("sides checked");
                    apply_sidedef(&mut map.sidedefs[sidedef], &save);
                }
                if save.side1 != 0 {
                    let Some(sidedef) = line.sidenum[1] else {
                        return Err(crate::game::savegame::SaveGameError::MapMismatch(format!(
                            "line {index} lost its back side"
                        ))
                        .into());
                    };
                    let save = sides.next().expect("sides checked");
                    apply_sidedef(&mut map.sidedefs[sidedef], &save);
                }
            }
        }

        // players
        {
            let mut iter = block.players.into_iter();
            for i in 0..MAXPLAYERS {
                if !self.playeringame[i] {
                    continue;
                }
                let Some(save) = iter.next() else {
                    return Err(crate::game::savegame::SaveGameError::MapMismatch(
                        "save holds fewer players than the level has slots".into(),
                    )
                    .into());
                };
                self.restore_player(i, &save)?;
            }
        }

        // thinkers: drop the fresh spawn, rebuild in save order
        self.store
            .reset_for_level(self.map.as_ref().expect("map loaded"));
        let handles = self.restore_thinkers(&block.thinkers)?;

        // relink player avatars (vanilla links via the mobj side)
        for i in 0..MAXPLAYERS {
            if !self.playeringame[i] {
                continue;
            }
            let found = handles.iter().find(|handle| {
                handle
                    .and_then(|id| self.store.mobj(id))
                    .and_then(|mo| mo.player)
                    == Some(i)
            });
            self.players[i].mo = found.copied().flatten();
        }

        // buttons waiting to revert
        let mut restored = Vec::new();
        for save in &block.buttons {
            if save.line < 0
                || save.line as usize >= self.map.as_ref().expect("map loaded").linedefs.len()
            {
                tracing::warn!(
                    "save game references button line {} out of range; dropped",
                    save.line
                );
                continue;
            }
            let Some(where_) = button_where_from_wire(save.where_) else {
                tracing::warn!(
                    "save game references unknown button slot {}; dropped",
                    save.where_
                );
                continue;
            };
            restored.push(specials::Button {
                line: save.line as usize,
                where_,
                revert_to: save.revert_to,
                timer: save.timer,
                sector: save.sector.max(0) as usize,
            });
        }
        self.buttons.clear();
        self.buttons.set_pressed(restored);

        self.paused = false;
        Ok(())
    }

    /// Restore one player record.
    fn restore_player(
        &mut self,
        slot: usize,
        save: &crate::game::savegame::SavePlayer,
    ) -> EngineResult<()> {
        let player = &mut self.players[slot];
        player.playerstate = playerstate_from_wire(save.playerstate).ok_or_else(|| {
            crate::game::savegame::SaveGameError::OutOfRange {
                what: "playerstate",
                index: i32::from(save.playerstate),
            }
        })?;
        player.health = save.health;
        player.armorpoints = save.armorpoints;
        player.armortype = save.armortype;
        player.powers = save.powers;
        player.cards = save.cards.map(|c| c != 0);
        player.backpack = save.backpack != 0;
        player.frags = save.frags;
        player.readyweapon = weapon_from_wire(save.readyweapon)?;
        player.pendingweapon = match save.pendingweapon {
            0xff => None,
            byte => Some(weapon_from_wire(byte)?),
        };
        player.weaponowned = save.weaponowned.map(|w| w != 0);
        player.ammo = save.ammo;
        player.maxammo = save.maxammo;
        player.attackdown = save.attackdown != 0;
        player.usedown = save.usedown != 0;
        player.cheats = save.cheats;
        player.refire = save.refire;
        player.killcount = save.killcount;
        player.itemcount = save.itemcount;
        player.secretcount = save.secretcount;
        player.damagecount = save.damagecount;
        player.bonuscount = save.bonuscount;
        player.extralight = save.extralight;
        player.fixedcolormap = save.fixedcolormap;
        player.colormap = save.colormap;
        player.didsecret = save.didsecret != 0;
        player.viewz = Fixed::from_raw(save.viewz);
        player.viewheight = Fixed::from_raw(save.viewheight);
        player.deltaviewheight = Fixed::from_raw(save.deltaviewheight);
        player.bob = Fixed::from_raw(save.bob);
        // attacker/message pointers are meaningless across loads
        player.attacker = None;
        player.message = None;
        for (index, psp) in save.psprites.iter().enumerate() {
            player.psprites[index] = crate::player::PspDef {
                state: match psp.state {
                    0xffff => None,
                    raw => Some(state_id_from_wire(raw)?),
                },
                tics: psp.tics,
                sx: Fixed::from_raw(psp.sx),
                sy: Fixed::from_raw(psp.sy),
            };
        }
        // avatar re-link happens after the thinker pass
        player.mo = None;
        Ok(())
    }

    /// Rebuild the thinker list from the save, preserving registry order.
    /// Returns one handle per saved thinker slot (mobjs and specials).
    fn restore_thinkers(
        &mut self,
        thinkers: &[crate::game::savegame::SaveThinker],
    ) -> EngineResult<Vec<Option<ThinkerId>>> {
        let mut handles: Vec<Option<ThinkerId>> = Vec::with_capacity(thinkers.len());
        for save in thinkers {
            let handle = match save {
                crate::game::savegame::SaveThinker::Mobj(save) => {
                    let mobj_type =
                        MobjType::from_index(save.mobj_type as usize).ok_or_else(|| {
                            crate::game::savegame::SaveGameError::OutOfRange {
                                what: "mobj_type",
                                index: i32::from(save.mobj_type),
                            }
                        })?;
                    let state = state_id_from_wire(save.state)?;
                    let map = self.map.as_ref().expect("map loaded");
                    let id = self.store.spawn_mobj(
                        map,
                        &mut self.rng,
                        SpawnSpec {
                            skill: self.gameskill,
                            x: Fixed::from_raw(save.x),
                            y: Fixed::from_raw(save.y),
                            z: SpawnZ::At(Fixed::from_raw(save.z)),
                            mobj_type,
                        },
                    );
                    if let Some(mo) = self.store.mobj_mut(id) {
                        mo.state = state;
                        mo.tics = save.tics;
                        mo.sprite = STATES[state as usize].sprite;
                        mo.frame = STATES[state as usize].frame;
                        mo.angle = Angle::from_raw(save.angle);
                        mo.momx = Fixed::from_raw(save.momx);
                        mo.momy = Fixed::from_raw(save.momy);
                        mo.momz = Fixed::from_raw(save.momz);
                        mo.flags = save.flags;
                        mo.health = save.health;
                        mo.movedir = save.movedir;
                        mo.movecount = save.movecount;
                        mo.reactiontime = save.reactiontime;
                        mo.threshold = save.threshold;
                        mo.lastlook = save.lastlook;
                        mo.player = if save.player_index >= 0
                            && (save.player_index as usize) < MAXPLAYERS
                        {
                            Some(save.player_index as usize)
                        } else {
                            None
                        };
                        mo.spawnpoint = ThingSpawn {
                            x: save.spawn_x,
                            y: save.spawn_y,
                            angle: save.spawn_angle,
                            thing_type: save.spawn_type,
                            flags: save.spawn_flags,
                        };
                    }
                    Some(id)
                }
                crate::game::savegame::SaveThinker::Special(save) => {
                    let sector_count = self.map.as_ref().expect("map loaded").sectors.len();
                    let thinker = match save {
                        crate::game::savegame::SaveSpecial::Door(door) => {
                            let kind = door_kind_from_wire(door.kind).ok_or_else(|| {
                                crate::game::savegame::SaveGameError::OutOfRange {
                                    what: "door kind",
                                    index: i32::from(door.kind),
                                }
                            })?;
                            let sector = sector_index(door.sector, sector_count)?;
                            Thinker::Door(specials::VerticalDoor {
                                kind,
                                sector,
                                topheight: Fixed::from_raw(door.topheight),
                                speed: Fixed::from_raw(door.speed),
                                direction: door.direction,
                                topwait: door.topwait,
                                topcountdown: door.topcountdown,
                            })
                        }
                        crate::game::savegame::SaveSpecial::Floor(floor) => {
                            let kind = floor_kind_from_wire(floor.kind).ok_or_else(|| {
                                crate::game::savegame::SaveGameError::OutOfRange {
                                    what: "floor kind",
                                    index: i32::from(floor.kind),
                                }
                            })?;
                            let sector = sector_index(floor.sector, sector_count)?;
                            Thinker::FloorMover(specials::FloorMover {
                                kind,
                                crush: floor.crush != 0,
                                sector,
                                direction: floor.direction,
                                new_special: floor.new_special,
                                texture: floor.texture,
                                floor_dest_height: Fixed::from_raw(floor.floor_dest_height),
                                speed: Fixed::from_raw(floor.speed),
                            })
                        }
                        crate::game::savegame::SaveSpecial::Plat(plat) => {
                            let kind = plat_kind_from_wire(plat.kind).ok_or_else(|| {
                                crate::game::savegame::SaveGameError::OutOfRange {
                                    what: "plat kind",
                                    index: i32::from(plat.kind),
                                }
                            })?;
                            let status = plat_status_from_wire(plat.status).ok_or_else(|| {
                                crate::game::savegame::SaveGameError::OutOfRange {
                                    what: "plat status",
                                    index: i32::from(plat.status),
                                }
                            })?;
                            let oldstatus =
                                plat_status_from_wire(plat.oldstatus).ok_or_else(|| {
                                    crate::game::savegame::SaveGameError::OutOfRange {
                                        what: "plat oldstatus",
                                        index: i32::from(plat.oldstatus),
                                    }
                                })?;
                            let sector = sector_index(plat.sector, sector_count)?;
                            Thinker::Plat(specials::Plat {
                                kind,
                                sector,
                                speed: Fixed::from_raw(plat.speed),
                                low: Fixed::from_raw(plat.low),
                                high: Fixed::from_raw(plat.high),
                                wait: plat.wait,
                                count: plat.count,
                                status,
                                oldstatus,
                                crush: plat.crush != 0,
                                tag: plat.tag,
                            })
                        }
                        crate::game::savegame::SaveSpecial::Ceiling(ceiling) => {
                            let kind = ceiling_kind_from_wire(ceiling.kind).ok_or_else(|| {
                                crate::game::savegame::SaveGameError::OutOfRange {
                                    what: "ceiling kind",
                                    index: i32::from(ceiling.kind),
                                }
                            })?;
                            let sector = sector_index(ceiling.sector, sector_count)?;
                            Thinker::Ceiling(specials::Ceiling {
                                kind,
                                sector,
                                bottomheight: Fixed::from_raw(ceiling.bottomheight),
                                topheight: Fixed::from_raw(ceiling.topheight),
                                speed: Fixed::from_raw(ceiling.speed),
                                crush: ceiling.crush != 0,
                                direction: ceiling.direction,
                                tag: ceiling.tag,
                                olddirection: ceiling.olddirection,
                            })
                        }
                        crate::game::savegame::SaveSpecial::LightFlash(flash) => {
                            let sector = sector_index(flash.sector, sector_count)?;
                            Thinker::LightFlash(specials::LightFlash {
                                sector,
                                count: flash.count,
                                maxlight: flash.maxlight,
                                minlight: flash.minlight,
                                maxtime: flash.maxtime,
                                mintime: flash.mintime,
                            })
                        }
                        crate::game::savegame::SaveSpecial::StrobeFlash(strobe) => {
                            let sector = sector_index(strobe.sector, sector_count)?;
                            Thinker::StrobeFlash(specials::StrobeFlash {
                                sector,
                                count: strobe.count,
                                minlight: strobe.minlight,
                                maxlight: strobe.maxlight,
                                darktime: strobe.darktime,
                                brighttime: strobe.brighttime,
                            })
                        }
                        crate::game::savegame::SaveSpecial::Glow(glow) => {
                            let sector = sector_index(glow.sector, sector_count)?;
                            Thinker::Glow(specials::Glow {
                                sector,
                                minlight: glow.minlight,
                                maxlight: glow.maxlight,
                                direction: glow.direction,
                            })
                        }
                        crate::game::savegame::SaveSpecial::FireFlicker(flicker) => {
                            let sector = sector_index(flicker.sector, sector_count)?;
                            Thinker::FireFlicker(specials::FireFlicker {
                                sector,
                                count: flicker.count,
                                maxlight: flicker.maxlight,
                                minlight: flicker.minlight,
                            })
                        }
                    };
                    let id = self.store.registry_mut().add(thinker);
                    Some(id)
                }
            };
            handles.push(handle);
        }

        // cross references, second pass
        for (index, save) in thinkers.iter().enumerate() {
            let crate::game::savegame::SaveThinker::Mobj(save) = save else {
                continue;
            };
            let target = if save.target >= 0 {
                handles.get(save.target as usize).copied().flatten()
            } else {
                None
            };
            let tracer = if save.tracer >= 0 {
                handles.get(save.tracer as usize).copied().flatten()
            } else {
                None
            };
            if let Some(id) = handles[index] {
                if let Some(mo) = self.store.mobj_mut(id) {
                    mo.target = target;
                    mo.tracer = tracer;
                }
            }
        }

        Ok(handles)
    }
}

fn apply_sidedef(side: &mut crate::map::Sidedef, save: &crate::game::savegame::SaveSidedef) {
    side.texture_offset = Fixed::from_raw(save.texture_offset);
    side.row_offset = Fixed::from_raw(save.row_offset);
    side.toptexture = save.top;
    side.bottomtexture = save.bottom;
    side.midtexture = save.mid;
}

fn sector_index(index: i32, count: usize) -> EngineResult<usize> {
    if index < 0 || index as usize >= count {
        return Err(crate::game::savegame::SaveGameError::OutOfRange {
            what: "sector",
            index,
        }
        .into());
    }
    Ok(index as usize)
}

fn state_id_from_wire(raw: u16) -> EngineResult<StateId> {
    StateId::from_index(raw as usize).ok_or_else(|| {
        crate::game::savegame::SaveGameError::OutOfRange {
            what: "state",
            index: i32::from(raw),
        }
        .into()
    })
}

fn weapon_from_wire(raw: u8) -> EngineResult<Weapon> {
    Ok(match raw {
        0 => Weapon::Fist,
        1 => Weapon::Pistol,
        2 => Weapon::Shotgun,
        3 => Weapon::Chaingun,
        4 => Weapon::MissileLauncher,
        5 => Weapon::PlasmaRifle,
        6 => Weapon::Bfg,
        7 => Weapon::Chainsaw,
        8 => Weapon::SuperShotgun,
        _ => {
            return Err(crate::game::savegame::SaveGameError::OutOfRange {
                what: "weapon",
                index: i32::from(raw),
            }
            .into());
        }
    })
}

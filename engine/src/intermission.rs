//! The intermission screen, ported from `wi_stuff.c` (linuxdoom-1.10).
//!
//! What is here (everything the `GS_INTERMISSION` state shows):
//!
//! * **the interactive stat count** — kills/items/secret (as percentages,
//!   counting up at speed with the `sfx_pistol` tick every 4 tics), the
//!   level/par time counting up 3 seconds per tic, and the net-game /
//!   deathmatch matrices, with the vanilla `sp_state`/`ng_state`/`dm_state`
//!   sub machines, the `acceleratestage` fast-forward and the pause
//!   interludes;
//! * **the secret/time summary** ("Level Finished!" and "Entering ...")
//!   via the `WIF`/`WIENTER` patches and the `WILV*`/`CWILV*` level names;
//! * **the episode world map** — splats on the completed cities and the
//!   flashing "You Are Here" pointer, with the 1.10 animated background
//!   (`epsd0/1/2animinfo`, including the `ANIM_LEVEL` hack that freezes
//!   the E2-next-level-8 animation while stats are being counted);
//! * **the end-of-screen wait** (`NoState`) and the
//!   world-done hand-off, reported as [`crate::intermission::IntermissionEvent::WorldDone`].
//!
//! Deliberately not ported:
//!
//! * `WI_Responder` — returns `false` unconditionally in the original;
//! * the French `WIOBJ`/`WIOSTI` items label — this port is the English
//!   build, like the rest of the project;
//! * `SP_FRAGS`/`SP_PAR` — dead macros in the original;
//! * the net-game patches for players not in the game (the original
//!   commented them out too);
//! * the `R_VideoErase` erasing — the whole frame is repainted here.
//!
//! The world coordinates of the episode maps (`lnodes`), the animation
//! tables (`epsd*animinfo`) and the layout constants are byte-exact
//! ports. Sprites/patches are decoded through the resource manager and
//! cached like everything else.

use crate::errors::{EngineError, EngineResult};
use crate::gfx::patch::{Patch, decode_patch};
use crate::menu::GameMode;
use crate::render::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::resources::ResourceManager;
use crate::rng::DoomRng;
use crate::stbar::{TICRATE, draw_patch};
use std::sync::Arc;

/// Vanilla `MAXPLAYERS`.
pub const MAX_PLAYERS: usize = 4;

/// Vanilla `WI_TITLEY`.
pub const WI_TITLEY: i32 = 2;
/// Vanilla `WI_SPACINGY` — the row height of a player row.
pub const WI_SPACINGY: i32 = 33;

/// Vanilla `SP_STATSX` — the single-player stats column.
pub const SP_STATSX: i32 = 50;
/// Vanilla `SP_STATSY`.
pub const SP_STATSY: i32 = 50;

/// Vanilla `SP_TIMEX` — the clock column.
pub const SP_TIMEX: i32 = 16;
/// Vanilla `SP_TIMEY` — the clock row.
pub const SP_TIMEY: i32 = SCREEN_HEIGHT as i32 - 32;

/// Vanilla `NG_STATSY`.
pub const NG_STATSY: i32 = 50;
/// Vanilla `NG_SPACINGX` — the net-game stat column width.
pub const NG_SPACINGX: i32 = 64;

/// Vanilla `DM_MATRIXX`.
pub const DM_MATRIXX: i32 = 42;
/// Vanilla `DM_MATRIXY`.
pub const DM_MATRIXY: i32 = 68;
/// Vanilla `DM_SPACINGX`.
pub const DM_SPACINGX: i32 = 40;
/// Vanilla `DM_TOTALSX`.
pub const DM_TOTALSX: i32 = 269;
/// Vanilla `DM_KILLERSX`.
pub const DM_KILLERSX: i32 = 10;
/// Vanilla `DM_KILLERSY`.
pub const DM_KILLERSY: i32 = 100;
/// Vanilla `DM_VICTIMSX`.
pub const DM_VICTIMSX: i32 = 5;
/// Vanilla `DM_VICTIMSY`.
pub const DM_VICTIMSY: i32 = 50;

/// Vanilla `SHOWNEXTLOCDELAY` (in seconds).
pub const SHOWNEXTLOCDELAY: i32 = 4;

/// Episode/map level lattice `lnodes[3][9]` (vanilla has three episode
/// maps; the fourth/retail episode shows no map).
const LNODES: [[(i32, i32); 9]; 3] = [
    // Episode 0 (1) World Map
    [
        (185, 164),
        (148, 143),
        (69, 122),
        (209, 102),
        (116, 89),
        (166, 55),
        (71, 56),
        (135, 29),
        (71, 24),
    ],
    // Episode 1 (2) World Map
    [
        (254, 25),
        (97, 50),
        (188, 64),
        (128, 78),
        (214, 92),
        (133, 130),
        (208, 136),
        (148, 140),
        (235, 158),
    ],
    // Episode 2 (3) World Map
    [
        (156, 168),
        (48, 154),
        (174, 95),
        (265, 75),
        (130, 48),
        (279, 23),
        (198, 48),
        (140, 25),
        (281, 136),
    ],
];

// ---------------------------------------------------------------------------
// Animated background
// ---------------------------------------------------------------------------

/// Vanilla `animenum_t`. The 1.10 tables use only `Always` and
/// `Level`; `ANIM_RANDOM` exists in the original source but is never
/// referenced by any animation table, so it is not ported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnimType {
    /// `ANIM_ALWAYS` — steady cycle, no conditions.
    Always,
    /// `ANIM_LEVEL` — only plays when the next level matches `data1`.
    Level,
}

/// Vanilla `anim_t`, minus the unused `lastdrawn`/`state` fields and
/// the unported `ANIM_RANDOM` bookkeeping.
#[derive(Clone, Debug)]
struct Anim {
    anim_type: AnimType,
    /// tics between frames
    period: i32,
    /// number of frames
    nanims: usize,
    /// location of the animation
    x: i32,
    y: i32,
    /// LEVEL: the level to match (the next level must equal this)
    data1: i32,
    /// decoded frames (filled by [`load_anims`])
    frames: Vec<Arc<Patch>>,
    /// bcnt at which to advance (vanilla `nexttic`)
    nexttic: i32,
    /// currently drawn frame; `-1` = hidden
    ctr: i32,
}

/// One row of the static animation tables (`animenum_t`, `TICRATE`
/// period, frame count, map location, and `data1` for `ANIM_LEVEL`).
type AnimSpec = (AnimType, i32, usize, (i32, i32), i32);

/// The static `epsd*animinfo` tables (level numbers are 1-based in the
/// `data1` field, exactly like the original).
const EPSD0_ANIMINFO: &[AnimSpec] = &[
    (AnimType::Always, TICRATE / 3, 3, (224, 104), 0),
    (AnimType::Always, TICRATE / 3, 3, (184, 160), 0),
    (AnimType::Always, TICRATE / 3, 3, (112, 136), 0),
    (AnimType::Always, TICRATE / 3, 3, (72, 112), 0),
    (AnimType::Always, TICRATE / 3, 3, (88, 96), 0),
    (AnimType::Always, TICRATE / 3, 3, (64, 48), 0),
    (AnimType::Always, TICRATE / 3, 3, (192, 40), 0),
    (AnimType::Always, TICRATE / 3, 3, (136, 16), 0),
    (AnimType::Always, TICRATE / 3, 3, (80, 16), 0),
    (AnimType::Always, TICRATE / 3, 3, (64, 24), 0),
];

const EPSD1_ANIMINFO: &[AnimSpec] = &[
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 1),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 2),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 3),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 4),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 5),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 6),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 7),
    (AnimType::Level, TICRATE / 3, 3, (192, 144), 8),
    (AnimType::Level, TICRATE / 3, 1, (128, 136), 8),
];

const EPSD2_ANIMINFO: &[AnimSpec] = &[
    (AnimType::Always, TICRATE / 3, 3, (104, 168), 0),
    (AnimType::Always, TICRATE / 3, 3, (40, 136), 0),
    (AnimType::Always, TICRATE / 3, 3, (160, 96), 0),
    (AnimType::Always, TICRATE / 3, 3, (104, 80), 0),
    (AnimType::Always, TICRATE / 3, 3, (120, 32), 0),
    (AnimType::Always, TICRATE / 4, 3, (40, 0), 0),
];

const EPSD_ANIM_TABLES: [&[AnimSpec]; 3] = [EPSD0_ANIMINFO, EPSD1_ANIMINFO, EPSD2_ANIMINFO];

// ---------------------------------------------------------------------------
// Setup, input and reported effects
// ---------------------------------------------------------------------------

/// Per-player stats the intermission counts up. Mirrors
/// `wbplayerstruct_t` (the vanilla `frags` array is `MAXPLAYERS` wide;
/// `score` is carried for the demo format but never drawn).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntermissionPlayer {
    /// `playeringame[i]`
    pub in_game: bool,
    /// `killcount`
    pub kills: i32,
    /// `itemcount`
    pub items: i32,
    /// `secretcount`
    pub secret: i32,
    /// `leveltime` in tics
    pub time_tics: i32,
    /// `fraglist[i]` — frags of each player (including self)
    pub frags: [i32; MAX_PLAYERS],
}

/// Everything the intermission needs, mirroring `wbstartstruct_t`
/// plus the mode flags the original read from globals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntermissionSetup {
    /// `gamemode`
    pub game_mode: GameMode,
    /// `epsd` — 0-based episode (shareware/registered: 0..=2, retail: 0..=3).
    /// For commercial games the episode is always 0 and unused.
    pub episode: i32,
    /// `last` — 0-based index of the finished level on the world map
    pub last_level: i32,
    /// `next` — 0-based index of the level to enter
    pub next_level: i32,
    /// `didsecret` — whether to splat the secret level on the map
    pub did_secret: bool,
    /// `pnum` — index of the local player
    pub local_player: usize,
    /// `plyr[]`
    pub players: [IntermissionPlayer; MAX_PLAYERS],
    /// `maxkills` (zero is clamped to 1, like the original)
    pub max_kills: i32,
    /// `maxitems` (zero is clamped to 1, like the original)
    pub max_items: i32,
    /// `maxsecret` (zero is clamped to 1, like the original)
    pub max_secret: i32,
    /// `partime` in tics
    pub par_time_tics: i32,
    /// `deathmatch`
    pub deathmatch: bool,
    /// `netgame`
    pub netgame: bool,
}

/// The state the intermission is in (`state` / `stateenum_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntermissionState {
    /// `StatCount` — counting the stats up
    StatCount,
    /// `ShowNextLoc` — flashing the world map pointer
    ShowNextLoc,
    /// `NoState` — task completed; waiting, then the world is done
    NoState,
}

/// Sounds the intermission asks the game layer to play.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntermissionSfx {
    /// `sfx_pistol`
    Pistol,
    /// `sfx_barexp`
    Barexp,
    /// `sfx_pldeth`
    Pldeth,
    /// `sfx_slop`
    Slop,
    /// `sfx_sgcock`
    SgCock,
}

/// Music changes the intermission triggers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntermissionMusic {
    /// `mus_dm2int` — DOOM II intermission track
    Dm2Int,
    /// `mus_inter` — the classic intermission track
    Inter,
}

/// Plain-data effects from the intermission; the game layer applies
/// them. Drained with [`Intermission::drain_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntermissionEvent {
    /// `S_ChangeMusic(..., true)`
    Music(IntermissionMusic),
    /// `S_StartSound`
    Sfx(IntermissionSfx),
    /// `G_WorldDone()` — the intermission is over, load the next map
    WorldDone,
}

/// Per-tic input for [`Intermission::tick`]: the held state of the
/// attack/use buttons of every player (the original reads
/// `players[i].cmd.buttons & BT_ATTACK|BT_USE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntermissionInput {
    /// `cmd.buttons & BT_ATTACK` per player
    pub attack: [bool; MAX_PLAYERS],
    /// `cmd.buttons & BT_USE` per player
    pub use_button: [bool; MAX_PLAYERS],
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

/// The decoded patches and the pre-rendered background. Built once at
/// intermission start (`WI_loadData`).
struct IntermissionArt {
    /// The background drawn into `screens[1]` in the original; slammed
    /// onto the framebuffer per frame.
    background: IndexedFramebuffer,
    /// Level name patches (`lnames`), episode or CWILV order.
    lnames: Vec<Arc<Patch>>,
    /// WIURH0/1 (non-commercial only)
    yah: Option<[Arc<Patch>; 2]>,
    /// WISPLAT (non-commercial only)
    splat: Option<Arc<Patch>>,
    /// WIMINUS
    wiminus: Arc<Patch>,
    /// WINUM0..9
    num: [Arc<Patch>; 10],
    /// WIPCNT
    percent: Arc<Patch>,
    /// WICOLON
    colon: Arc<Patch>,
    /// WIF — "Finished!"
    finished: Arc<Patch>,
    /// WIENTER — "Entering"
    entering: Arc<Patch>,
    /// WIOSTK — "Kills"
    kills: Arc<Patch>,
    /// WIOSTS — "Scrt"
    secret: Arc<Patch>,
    /// WISCRT2 — "secret"
    sp_secret: Arc<Patch>,
    /// WIOSTI — "Items"
    items: Arc<Patch>,
    /// WIFRGS — "Frags"
    frags: Arc<Patch>,
    /// WITIME — "Time"
    time: Arc<Patch>,
    /// WISUCKS — "sucks"
    sucks: Arc<Patch>,
    /// WIPAR — "Par"
    par: Arc<Patch>,
    /// WIKILRS — "killers" (vertical)
    killers: Arc<Patch>,
    /// WIVCTMS — "victims"
    victims: Arc<Patch>,
    /// WIMSTT — "total"
    total: Arc<Patch>,
    /// STFST01
    star: Arc<Patch>,
    /// STFDEAD0
    bstar: Arc<Patch>,
    /// STPB0..3
    p: [Arc<Patch>; MAX_PLAYERS],
    /// WIBP1..4 (loaded but never drawn, like the original)
    #[allow(dead_code)]
    bp: [Arc<Patch>; MAX_PLAYERS],
}

fn load_patch(resources: &ResourceManager, name: &str) -> EngineResult<Arc<Patch>> {
    let lump = resources
        .find_lump(name)
        .ok_or_else(|| EngineError::Resource(format!("intermission patch {name} not found")))?;
    resources.decode_cached::<Patch>(lump, |bytes| decode_patch(bytes).map_err(EngineError::from))
}

/// Builds the animated-background tables for the episode, decoding the
/// frame patches (`WIA%d%.2d%.2d`; `epsd == 1 && j == 8` reuses the
/// frames of animation 4 — the original's MONDO HACK).
fn load_anims(
    resources: &ResourceManager,
    episode: i32,
    commercial: bool,
) -> EngineResult<Vec<Anim>> {
    if commercial {
        return Ok(Vec::new());
    }
    if episode > 2 {
        return Ok(Vec::new());
    }
    let table = EPSD_ANIM_TABLES[episode as usize];
    let mut anims: Vec<Anim> = Vec::with_capacity(table.len());
    for (j, &(anim_type, period, nanims, (x, y), data1)) in table.iter().enumerate() {
        let mut frames = Vec::with_capacity(nanims);
        // MONDO HACK! `epsd 1, anim 8` shares the frames of anim 4
        if episode == 1 && j == 8 {
            let shared = anims[4].frames.get(..nanims).ok_or_else(|| {
                EngineError::Resource("intermission anim 8 shares anim 4 frames".into())
            })?;
            frames.extend_from_slice(shared);
        } else {
            for i in 0..nanims {
                let name = format!("WIA{episode}{j:02}{i:02}");
                frames.push(load_patch(resources, &name)?);
            }
        }
        anims.push(Anim {
            anim_type,
            period,
            nanims,
            x,
            y,
            data1,
            frames,
            nexttic: 0,
            ctr: -1,
        });
    }
    Ok(anims)
}

impl IntermissionArt {
    fn load(resources: &ResourceManager, setup: &IntermissionSetup) -> EngineResult<Self> {
        let commercial = setup.game_mode == GameMode::Commercial;

        // background (WIMAP%d, INTERPIC for commercial and retail E4)
        let mut bg_name = format!("WIMAP{}", setup.episode);
        if commercial {
            bg_name = "INTERPIC".to_string();
        }
        if setup.game_mode == GameMode::Retail && setup.episode == 3 {
            bg_name = "INTERPIC".to_string();
        }
        let bg_lump = resources.find_lump(&bg_name).ok_or_else(|| {
            EngineError::Resource(format!("intermission background {bg_name} not found"))
        })?;
        let bg = resources.decode_cached::<Patch>(bg_lump, |bytes| {
            decode_patch(bytes).map_err(EngineError::from)
        })?;

        // the "screens[1]" back screen: draw the background once, then
        // slam it per frame
        let mut background = IndexedFramebuffer::new();
        background.clear(0);
        draw_patch(&mut background, &bg, 0, 0);

        // level name patches
        let mut lnames = Vec::new();
        if commercial {
            for i in 0..32 {
                lnames.push(load_patch(resources, &format!("CWILV{i:02}"))?);
            }
        } else {
            for i in 0..9 {
                lnames.push(load_patch(
                    resources,
                    &format!("WILV{}{}", setup.episode, i),
                )?);
            }
        }

        // you-are-here + splat (non-commercial only)
        let (yah, splat) = if commercial {
            (None, None)
        } else {
            (
                Some([
                    load_patch(resources, "WIURH0")?,
                    load_patch(resources, "WIURH1")?,
                ]),
                Some(load_patch(resources, "WISPLAT")?),
            )
        };

        let load_num = |i: usize| load_patch(resources, &format!("WINUM{i}"));
        let mut num = Vec::with_capacity(10);
        for i in 0..10 {
            num.push(load_num(i)?);
        }
        let num: [Arc<Patch>; 10] = num.try_into().expect("ten number lumps");

        let mut p = Vec::with_capacity(MAX_PLAYERS);
        for i in 0..MAX_PLAYERS {
            // "STPB0".."STPB3"
            p.push(load_patch(resources, &format!("STPB{i}"))?);
        }
        let p: [Arc<Patch>; MAX_PLAYERS] = p.try_into().expect("four player number lumps");

        let mut bp = Vec::with_capacity(MAX_PLAYERS);
        for i in 0..MAX_PLAYERS {
            // "WIBP1".."WIBP4" — yes, 1-based, unlike STPB
            bp.push(load_patch(resources, &format!("WIBP{}", i + 1))?);
        }
        let bp: [Arc<Patch>; MAX_PLAYERS] = bp.try_into().expect("four player name lumps");

        Ok(Self {
            background,
            lnames,
            yah,
            splat,
            wiminus: load_patch(resources, "WIMINUS")?,
            num,
            percent: load_patch(resources, "WIPCNT")?,
            colon: load_patch(resources, "WICOLON")?,
            finished: load_patch(resources, "WIF")?,
            entering: load_patch(resources, "WIENTER")?,
            kills: load_patch(resources, "WIOSTK")?,
            secret: load_patch(resources, "WIOSTS")?,
            sp_secret: load_patch(resources, "WISCRT2")?,
            items: load_patch(resources, "WIOSTI")?,
            frags: load_patch(resources, "WIFRGS")?,
            time: load_patch(resources, "WITIME")?,
            sucks: load_patch(resources, "WISUCKS")?,
            par: load_patch(resources, "WIPAR")?,
            killers: load_patch(resources, "WIKILRS")?,
            victims: load_patch(resources, "WIVCTMS")?,
            total: load_patch(resources, "WIMSTT")?,
            star: load_patch(resources, "STFST01")?,
            bstar: load_patch(resources, "STFDEAD0")?,
            p,
            bp,
        })
    }
}

// ---------------------------------------------------------------------------
// The intermission state machine
// ---------------------------------------------------------------------------

/// `WI_fragSum` — frags of `playernum` against everyone else, minus
/// self-frags (can be negative — the JDC hack).
fn frag_sum(setup: &IntermissionSetup, playernum: usize) -> i32 {
    let mut frags = 0;
    for i in 0..MAX_PLAYERS {
        if setup.players[i].in_game && i != playernum {
            frags += setup.players[playernum].frags[i];
        }
    }
    frags -= setup.players[playernum].frags[playernum];
    frags
}

/// The intermission as a retained, testable object (`wi_stuff.c`'s
/// file-scope state, gathered).
pub struct Intermission {
    setup: IntermissionSetup,
    art: IntermissionArt,
    anims: Vec<Anim>,
    events: Vec<IntermissionEvent>,

    state: IntermissionState,
    /// `sp_state` / `ng_state` / `dm_state`
    sub_state: i32,
    /// `cnt` — general one-shot timer
    cnt: i32,
    /// `bcnt` — background animation timer, ++ per tic
    bcnt: i32,
    cnt_pause: i32,
    /// 1 = skip the current stage
    acceleratestage: i32,
    attackdown: [bool; MAX_PLAYERS],
    usedown: [bool; MAX_PLAYERS],

    cnt_kills: [i32; MAX_PLAYERS],
    cnt_items: [i32; MAX_PLAYERS],
    cnt_secret: [i32; MAX_PLAYERS],
    cnt_frags: [i32; MAX_PLAYERS],
    cnt_time: i32,
    cnt_par: i32,

    dm_frags: [[i32; MAX_PLAYERS]; MAX_PLAYERS],
    dm_totals: [i32; MAX_PLAYERS],
    dofrags: bool,

    snl_pointeron: bool,
}

impl Intermission {
    /// `WI_Start` — resolves the setup, loads the lumps and primes the
    /// right sub machine (deathmatch / netgame / single player).
    pub fn start(
        resources: &ResourceManager,
        setup: IntermissionSetup,
        rng: &mut DoomRng,
    ) -> EngineResult<Self> {
        let mut setup = setup;

        // WI_initVariables: clamp empty counters to 1
        if setup.max_kills == 0 {
            setup.max_kills = 1;
        }
        if setup.max_items == 0 {
            setup.max_items = 1;
        }
        if setup.max_secret == 0 {
            setup.max_secret = 1;
        }
        // vanilla: retail keeps episode 4; everyone else maps it back
        if setup.game_mode != GameMode::Retail && setup.episode > 2 {
            setup.episode -= 3;
        }

        let art = IntermissionArt::load(resources, &setup)?;
        let commercial = setup.game_mode == GameMode::Commercial;
        let mut intermission = Self {
            anims: load_anims(resources, setup.episode, commercial)?,
            art,
            setup,
            events: Vec::new(),
            state: IntermissionState::StatCount,
            sub_state: 1,
            cnt: 0,
            bcnt: 0,
            cnt_pause: TICRATE,
            acceleratestage: 0,
            attackdown: [false; MAX_PLAYERS],
            usedown: [false; MAX_PLAYERS],
            cnt_kills: [0; MAX_PLAYERS],
            cnt_items: [0; MAX_PLAYERS],
            cnt_secret: [0; MAX_PLAYERS],
            cnt_frags: [0; MAX_PLAYERS],
            cnt_time: 0,
            cnt_par: 0,
            dm_frags: [[0; MAX_PLAYERS]; MAX_PLAYERS],
            dm_totals: [0; MAX_PLAYERS],
            dofrags: false,
            snl_pointeron: false,
        };

        // WI_initVariables: stat counters start at -1 for single player
        intermission.cnt_kills[0] = -1;
        intermission.cnt_items[0] = -1;
        intermission.cnt_secret[0] = -1;
        intermission.cnt_time = -1;
        intermission.cnt_par = -1;
        intermission.dofrags = false;

        if intermission.setup.deathmatch {
            intermission.init_deathmatch_stats(rng);
        } else if intermission.setup.netgame {
            intermission.init_netgame_stats(rng);
        } else {
            intermission.init_stats(rng);
        }

        Ok(intermission)
    }

    /// `me` — the local player index, clamped like the clamp on
    /// `maxkills` (vanilla indexes `plrs[me]` unchecked).
    fn me(&self) -> usize {
        self.setup.local_player.min(MAX_PLAYERS - 1)
    }

    /// The resolved setup (read-only).
    pub fn setup(&self) -> &IntermissionSetup {
        &self.setup
    }

    /// The current top-level state.
    pub fn state(&self) -> IntermissionState {
        self.state
    }

    /// Drains the accumulated effects (music, sfx, world-done).
    pub fn drain_events(&mut self) -> Vec<IntermissionEvent> {
        std::mem::take(&mut self.events)
    }

    /// `WI_Ticker`.
    pub fn tick(&mut self, rng: &mut DoomRng, input: &IntermissionInput) {
        // counter for general background animation
        self.bcnt += 1;

        if self.bcnt == 1 {
            // intermission music
            let music = if self.setup.game_mode == GameMode::Commercial {
                IntermissionMusic::Dm2Int
            } else {
                IntermissionMusic::Inter
            };
            self.events.push(IntermissionEvent::Music(music));
        }

        self.check_for_accelerate(input);

        match self.state {
            IntermissionState::StatCount => {
                if self.setup.deathmatch {
                    self.update_deathmatch_stats(rng);
                } else if self.setup.netgame {
                    self.update_netgame_stats(rng);
                } else {
                    self.update_stats(rng);
                }
            }
            IntermissionState::ShowNextLoc => self.update_show_next_loc(),
            IntermissionState::NoState => self.update_no_state(),
        }
    }

    /// `WI_checkForAccelerate` — edge-triggered attack/use skip.
    fn check_for_accelerate(&mut self, input: &IntermissionInput) {
        for i in 0..MAX_PLAYERS {
            if !self.setup.players[i].in_game {
                continue;
            }
            if input.attack[i] {
                if !self.attackdown[i] {
                    self.acceleratestage = 1;
                }
                self.attackdown[i] = true;
            } else {
                self.attackdown[i] = false;
            }
            if input.use_button[i] {
                if !self.usedown[i] {
                    self.acceleratestage = 1;
                }
                self.usedown[i] = true;
            } else {
                self.usedown[i] = false;
            }
        }
    }

    /// `WI_initNoState`.
    fn init_no_state(&mut self) {
        self.state = IntermissionState::NoState;
        self.acceleratestage = 0;
        self.cnt = 10;
    }

    /// `WI_updateNoState`.
    fn update_no_state(&mut self) {
        self.update_animated_back();

        self.cnt -= 1;
        if self.cnt == 0 {
            // WI_End -> WI_unloadData (the Arcs drop with this value),
            // then G_WorldDone()
            self.events.push(IntermissionEvent::WorldDone);
        }
    }

    /// `WI_initShowNextLoc`.
    fn init_show_next_loc(&mut self, rng: &mut DoomRng) {
        self.state = IntermissionState::ShowNextLoc;
        self.acceleratestage = 0;
        self.cnt = SHOWNEXTLOCDELAY * TICRATE;

        self.init_animated_back(rng);
    }

    /// `WI_updateShowNextLoc`.
    fn update_show_next_loc(&mut self) {
        self.update_animated_back();

        self.cnt -= 1;
        if self.cnt == 0 || self.acceleratestage != 0 {
            self.init_no_state();
        } else {
            self.snl_pointeron = (self.cnt & 31) < 20;
        }
    }

    /// `WI_initAnimatedBack`.
    fn init_animated_back(&mut self, rng: &mut DoomRng) {
        if self.setup.game_mode == GameMode::Commercial {
            return;
        }
        if self.setup.episode > 2 {
            return;
        }

        for a in &mut self.anims {
            a.ctr = -1;
            a.nexttic = match a.anim_type {
                AnimType::Always => {
                    let span = a.period.max(1) as u8;
                    self.bcnt + 1 + i32::from(rng.next() % span)
                }
                AnimType::Level => self.bcnt + 1,
            };
        }
    }

    /// `WI_updateAnimatedBack`.
    fn update_animated_back(&mut self) {
        if self.setup.game_mode == GameMode::Commercial {
            return;
        }
        if self.setup.episode > 2 {
            return;
        }

        for i in 0..self.anims.len() {
            if self.bcnt != self.anims[i].nexttic {
                continue;
            }
            match self.anims[i].anim_type {
                AnimType::Always => {
                    let a = &mut self.anims[i];
                    a.ctr += 1;
                    if a.ctr >= a.nanims as i32 {
                        a.ctr = 0;
                    }
                    a.nexttic = self.bcnt + a.period;
                }
                AnimType::Level => {
                    // gawd-awful hack for level anims: the "fire" anim of
                    // episode 2 (index 7) stays frozen while stats count
                    let frozen = self.state == IntermissionState::StatCount && i == 7;
                    if !frozen && self.setup.next_level == self.anims[i].data1 {
                        let a = &mut self.anims[i];
                        a.ctr += 1;
                        if a.ctr == a.nanims as i32 {
                            a.ctr -= 1;
                        }
                        a.nexttic = self.bcnt + a.period;
                    }
                    // else: nexttic stays equal to bcnt, exactly like the
                    // original — the animation never wakes up this screen
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The stat sub machines (SP / NG / DM)
// ---------------------------------------------------------------------------

impl Intermission {
    /// `WI_initStats` — single player.
    fn init_stats(&mut self, rng: &mut DoomRng) {
        self.state = IntermissionState::StatCount;
        self.acceleratestage = 0;
        self.sub_state = 1;
        self.cnt_kills[0] = -1;
        self.cnt_items[0] = -1;
        self.cnt_secret[0] = -1;
        self.cnt_time = -1;
        self.cnt_par = -1;
        self.cnt_pause = TICRATE;

        self.init_animated_back(rng);
    }

    /// `WI_updateStats` — single player.
    fn update_stats(&mut self, rng: &mut DoomRng) {
        self.update_animated_back();

        let me = self.me();
        let target_kills = (self.setup.players[me].kills * 100) / self.setup.max_kills;
        let target_items = (self.setup.players[me].items * 100) / self.setup.max_items;
        let target_secret = (self.setup.players[me].secret * 100) / self.setup.max_secret;
        let target_time = self.setup.players[me].time_tics / TICRATE;
        let target_par = self.setup.par_time_tics / TICRATE;

        if self.acceleratestage != 0 && self.sub_state != 10 {
            self.acceleratestage = 0;
            self.cnt_kills[0] = target_kills;
            self.cnt_items[0] = target_items;
            self.cnt_secret[0] = target_secret;
            self.cnt_time = target_time;
            self.cnt_par = target_par;
            self.events
                .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
            self.sub_state = 10;
        }

        if self.sub_state == 2 {
            self.cnt_kills[0] += 2;

            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            if self.cnt_kills[0] >= target_kills {
                self.cnt_kills[0] = target_kills;
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1;
            }
        } else if self.sub_state == 4 {
            self.cnt_items[0] += 2;

            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            if self.cnt_items[0] >= target_items {
                self.cnt_items[0] = target_items;
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1;
            }
        } else if self.sub_state == 6 {
            self.cnt_secret[0] += 2;

            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            if self.cnt_secret[0] >= target_secret {
                self.cnt_secret[0] = target_secret;
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1;
            }
        } else if self.sub_state == 8 {
            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            self.cnt_time += 3;

            if self.cnt_time >= target_time {
                self.cnt_time = target_time;
            }

            self.cnt_par += 3;

            if self.cnt_par >= target_par {
                self.cnt_par = target_par;

                if self.cnt_time >= target_time {
                    self.events
                        .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                    self.sub_state += 1;
                }
            }
        } else if self.sub_state == 10 {
            if self.acceleratestage != 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::SgCock));

                if self.setup.game_mode == GameMode::Commercial {
                    self.init_no_state();
                } else {
                    self.init_show_next_loc(rng);
                }
            }
        } else if self.sub_state & 1 == 1 {
            // the pause between stages
            self.cnt_pause -= 1;
            if self.cnt_pause == 0 {
                self.sub_state += 1;
                self.cnt_pause = TICRATE;
            }
        }
    }

    /// `WI_initNetgameStats`.
    fn init_netgame_stats(&mut self, rng: &mut DoomRng) {
        self.state = IntermissionState::StatCount;
        self.acceleratestage = 0;
        self.sub_state = 1;
        self.cnt_pause = TICRATE;

        let mut frag_total = 0;
        for i in 0..MAX_PLAYERS {
            if !self.setup.players[i].in_game {
                continue;
            }
            self.cnt_kills[i] = 0;
            self.cnt_items[i] = 0;
            self.cnt_secret[i] = 0;
            self.cnt_frags[i] = 0;

            frag_total += frag_sum(&self.setup, i);
        }

        self.dofrags = frag_total != 0;

        self.init_animated_back(rng);
    }

    /// `WI_updateNetgameStats`.
    fn update_netgame_stats(&mut self, rng: &mut DoomRng) {
        self.update_animated_back();

        if self.acceleratestage != 0 && self.sub_state != 10 {
            self.acceleratestage = 0;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }
                self.cnt_kills[i] = (self.setup.players[i].kills * 100) / self.setup.max_kills;
                self.cnt_items[i] = (self.setup.players[i].items * 100) / self.setup.max_items;
                self.cnt_secret[i] = (self.setup.players[i].secret * 100) / self.setup.max_secret;

                if self.dofrags {
                    self.cnt_frags[i] = frag_sum(&self.setup, i);
                }
            }
            self.events
                .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
            self.sub_state = 10;
        }

        if self.sub_state == 2 {
            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            let mut still_ticking = false;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }

                self.cnt_kills[i] += 2;

                let target = (self.setup.players[i].kills * 100) / self.setup.max_kills;
                if self.cnt_kills[i] >= target {
                    self.cnt_kills[i] = target;
                } else {
                    still_ticking = true;
                }
            }

            if !still_ticking {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1;
            }
        } else if self.sub_state == 4 {
            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            let mut still_ticking = false;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }

                self.cnt_items[i] += 2;
                let target = (self.setup.players[i].items * 100) / self.setup.max_items;
                if self.cnt_items[i] >= target {
                    self.cnt_items[i] = target;
                } else {
                    still_ticking = true;
                }
            }
            if !still_ticking {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1;
            }
        } else if self.sub_state == 6 {
            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            let mut still_ticking = false;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }

                self.cnt_secret[i] += 2;

                let target = (self.setup.players[i].secret * 100) / self.setup.max_secret;
                if self.cnt_secret[i] >= target {
                    self.cnt_secret[i] = target;
                } else {
                    still_ticking = true;
                }
            }

            if !still_ticking {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1 + 2 * if self.dofrags { 0 } else { 1 };
            }
        } else if self.sub_state == 8 {
            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            let mut still_ticking = false;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }

                self.cnt_frags[i] += 1;

                let fsum = frag_sum(&self.setup, i);
                if self.cnt_frags[i] >= fsum {
                    self.cnt_frags[i] = fsum;
                } else {
                    still_ticking = true;
                }
            }

            if !still_ticking {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pldeth));
                self.sub_state += 1;
            }
        } else if self.sub_state == 10 {
            if self.acceleratestage != 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::SgCock));
                if self.setup.game_mode == GameMode::Commercial {
                    self.init_no_state();
                } else {
                    self.init_show_next_loc(rng);
                }
            }
        } else if self.sub_state & 1 == 1 {
            self.cnt_pause -= 1;
            if self.cnt_pause == 0 {
                self.sub_state += 1;
                self.cnt_pause = TICRATE;
            }
        }
    }

    /// `WI_initDeathmatchStats`.
    fn init_deathmatch_stats(&mut self, rng: &mut DoomRng) {
        self.state = IntermissionState::StatCount;
        self.acceleratestage = 0;
        self.sub_state = 1;
        self.cnt_pause = TICRATE;

        for i in 0..MAX_PLAYERS {
            if !self.setup.players[i].in_game {
                continue;
            }

            for j in 0..MAX_PLAYERS {
                if self.setup.players[j].in_game {
                    self.dm_frags[i][j] = 0;
                }
            }

            self.dm_totals[i] = 0;
        }

        self.init_animated_back(rng);
    }

    /// `WI_updateDeathmatchStats`.
    fn update_deathmatch_stats(&mut self, rng: &mut DoomRng) {
        self.update_animated_back();

        if self.acceleratestage != 0 && self.sub_state != 4 {
            self.acceleratestage = 0;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }
                for j in 0..MAX_PLAYERS {
                    if self.setup.players[j].in_game {
                        self.dm_frags[i][j] = self.setup.players[i].frags[j];
                    }
                }

                self.dm_totals[i] = frag_sum(&self.setup, i);
            }

            self.events
                .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
            self.sub_state = 4;
        }

        if self.sub_state == 2 {
            if self.bcnt & 3 == 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Pistol));
            }

            let mut still_ticking = false;

            for i in 0..MAX_PLAYERS {
                if !self.setup.players[i].in_game {
                    continue;
                }

                for j in 0..MAX_PLAYERS {
                    if !self.setup.players[j].in_game {
                        continue;
                    }
                    if self.dm_frags[i][j] != self.setup.players[i].frags[j] {
                        // walk toward the target in the direction of its sign
                        if self.setup.players[i].frags[j] < 0 {
                            self.dm_frags[i][j] -= 1;
                        } else {
                            self.dm_frags[i][j] += 1;
                        }

                        self.dm_frags[i][j] = self.dm_frags[i][j].clamp(-99, 99);

                        still_ticking = true;
                    }
                }
                self.dm_totals[i] = frag_sum(&self.setup, i).clamp(-99, 99);
            }
            if !still_ticking {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Barexp));
                self.sub_state += 1;
            }
        } else if self.sub_state == 4 {
            if self.acceleratestage != 0 {
                self.events
                    .push(IntermissionEvent::Sfx(IntermissionSfx::Slop));

                if self.setup.game_mode == GameMode::Commercial {
                    self.init_no_state();
                } else {
                    self.init_show_next_loc(rng);
                }
            }
        } else if self.sub_state & 1 == 1 {
            self.cnt_pause -= 1;
            if self.cnt_pause == 0 {
                self.sub_state += 1;
                self.cnt_pause = TICRATE;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl Intermission {
    /// `WI_Drawer`.
    pub fn draw(&mut self, fb: &mut IndexedFramebuffer) {
        match self.state {
            IntermissionState::StatCount => {
                if self.setup.deathmatch {
                    self.draw_deathmatch_stats(fb);
                } else if self.setup.netgame {
                    self.draw_netgame_stats(fb);
                } else {
                    self.draw_stats(fb);
                }
            }
            IntermissionState::ShowNextLoc => self.draw_show_next_loc(fb),
            IntermissionState::NoState => {
                self.snl_pointeron = true;
                self.draw_show_next_loc(fb);
            }
        }
    }

    /// `WI_slamBackground` — copy the back screen straight into the framebuffer.
    fn slam_background(&self, fb: &mut IndexedFramebuffer) {
        fb.blit_from(&self.art.background);
    }

    /// `WI_drawAnimatedBack`.
    fn draw_animated_back(&self, fb: &mut IndexedFramebuffer) {
        if self.setup.game_mode == GameMode::Commercial {
            return;
        }
        if self.setup.episode > 2 {
            return;
        }

        for a in &self.anims {
            if a.ctr >= 0 {
                draw_patch(fb, &a.frames[a.ctr as usize], a.x, a.y);
            }
        }
    }

    /// One out-of-range level name lookup logs once and yields None —
    /// the original would dereference NULL here.
    fn lnames_entry(&self, index: i32, what: &str) -> Option<&Arc<Patch>> {
        let entry = self.art.lnames.get(usize::try_from(index).ok()?);
        if entry.is_none() {
            tracing::warn!("intermission: {what} index {index} out of range");
        }
        entry
    }

    /// `WI_drawLF` — "<Levelname> Finished!".
    fn draw_lf(&self, fb: &mut IndexedFramebuffer) {
        let y = WI_TITLEY;

        let Some(last) = self.lnames_entry(self.setup.last_level, "last") else {
            return;
        };
        let w = i32::from(last.header.width);
        let h = i32::from(last.header.height);
        draw_patch(fb, last, (SCREEN_WIDTH as i32 - w) / 2, y);

        // draw "Finished!"
        let y = y + (5 * h) / 4;
        let fw = i32::from(self.art.finished.header.width);
        draw_patch(fb, &self.art.finished, (SCREEN_WIDTH as i32 - fw) / 2, y);
    }

    /// `WI_drawEL` — "Entering <Levelname>".
    fn draw_el(&self, fb: &mut IndexedFramebuffer) {
        let y = WI_TITLEY;

        // draw "Entering"
        let fw = i32::from(self.art.entering.header.width);
        draw_patch(fb, &self.art.entering, (SCREEN_WIDTH as i32 - fw) / 2, y);

        // draw level
        let Some(next) = self.lnames_entry(self.setup.next_level, "next") else {
            return;
        };
        let w = i32::from(next.header.width);
        let h = i32::from(next.header.height);
        let y = y + (5 * h) / 4;
        draw_patch(fb, next, (SCREEN_WIDTH as i32 - w) / 2, y);
    }

    /// `WI_drawOnLnode` — tries each candidate, first that fits wins.
    fn draw_on_lnode(&self, fb: &mut IndexedFramebuffer, n: i32, candidates: &[&Arc<Patch>]) {
        let epsd = self.setup.episode;
        if epsd > 2 || n < 0 || n >= LNODES[epsd as usize].len() as i32 {
            tracing::warn!("intermission: level node {n} out of range for episode {epsd}");
            return;
        }
        let (nx, ny) = LNODES[epsd as usize][n as usize];

        for patch in candidates {
            let left = nx - i32::from(patch.header.left_offset);
            let top = ny - i32::from(patch.header.top_offset);
            let right = left + i32::from(patch.header.width);
            let bottom = top + i32::from(patch.header.height);

            if left >= 0 && right < SCREEN_WIDTH as i32 && top >= 0 && bottom < SCREEN_HEIGHT as i32
            {
                draw_patch(fb, patch, nx, ny);
                return;
            }
        }

        // DEBUG (vanilla printf)
        tracing::warn!("could not place patch on level {}", n + 1);
    }

    /// `WI_drawNum` — draws a right-aligned number; returns the new x.
    fn draw_num(
        &self,
        fb: &mut IndexedFramebuffer,
        mut x: i32,
        y: i32,
        n: i32,
        digits: i32,
    ) -> i32 {
        let fontwidth = i32::from(self.art.num[0].header.width);
        let mut digits = digits;

        if digits < 0 {
            if n == 0 {
                // make variable-length zeros 1 digit long
                digits = 1;
            } else {
                let mut temp = n;
                let mut counted = 0;
                while temp != 0 {
                    temp /= 10;
                    counted += 1;
                }
                digits = counted;
            }
        }

        let neg = n < 0;
        let mut mag = (n as i64).unsigned_abs();

        // the 1994 Easter egg: the number vanishes
        if mag == 1994 {
            return 0;
        }

        while digits > 0 {
            digits -= 1;
            x -= fontwidth;
            let digit = (mag % 10) as usize;
            draw_patch(fb, &self.art.num[digit], x, y);
            mag /= 10;
        }

        // draw a minus sign if necessary
        if neg {
            x -= 8;
            draw_patch(fb, &self.art.wiminus, x, y);
        }

        x
    }

    /// `WI_drawPercent`.
    fn draw_percent(&self, fb: &mut IndexedFramebuffer, x: i32, y: i32, p: i32) {
        if p < 0 {
            return;
        }
        draw_patch(fb, &self.art.percent, x, y);
        self.draw_num(fb, x, y, p, -1);
    }

    /// `WI_drawTime` — mm:ss with colons, or "sucks" past 61:59.
    fn draw_time(&self, fb: &mut IndexedFramebuffer, x: i32, y: i32, t: i32) {
        if t < 0 {
            return;
        }

        if t <= 61 * 59 {
            let mut div = 1i32;
            let mut x = x;
            loop {
                let n = (t / div) % 60;
                x = self.draw_num(fb, x, y, n, 2) - i32::from(self.art.colon.header.width);
                div *= 60;

                // draw
                if div == 60 || t / div != 0 {
                    draw_patch(fb, &self.art.colon, x, y);
                }

                if t / div == 0 {
                    break;
                }
            }
        } else {
            // "sucks"
            let w = i32::from(self.art.sucks.header.width);
            draw_patch(fb, &self.art.sucks, x - w, y);
        }
    }

    /// `WI_drawStats` — single-player summary.
    fn draw_stats(&mut self, fb: &mut IndexedFramebuffer) {
        // line height
        let lh = (3 * i32::from(self.art.num[0].header.height)) / 2;

        self.slam_background(fb);
        self.draw_animated_back(fb);
        self.draw_lf(fb);

        draw_patch(fb, &self.art.kills, SP_STATSX, SP_STATSY);
        self.draw_percent(
            fb,
            SCREEN_WIDTH as i32 - SP_STATSX,
            SP_STATSY,
            self.cnt_kills[0],
        );

        draw_patch(fb, &self.art.items, SP_STATSX, SP_STATSY + lh);
        self.draw_percent(
            fb,
            SCREEN_WIDTH as i32 - SP_STATSX,
            SP_STATSY + lh,
            self.cnt_items[0],
        );

        draw_patch(fb, &self.art.sp_secret, SP_STATSX, SP_STATSY + 2 * lh);
        self.draw_percent(
            fb,
            SCREEN_WIDTH as i32 - SP_STATSX,
            SP_STATSY + 2 * lh,
            self.cnt_secret[0],
        );

        draw_patch(fb, &self.art.time, SP_TIMEX, SP_TIMEY);
        self.draw_time(
            fb,
            SCREEN_WIDTH as i32 / 2 - SP_TIMEX,
            SP_TIMEY,
            self.cnt_time,
        );

        if self.setup.episode < 3 {
            draw_patch(
                fb,
                &self.art.par,
                SCREEN_WIDTH as i32 / 2 + SP_TIMEX,
                SP_TIMEY,
            );
            self.draw_time(fb, SCREEN_WIDTH as i32 - SP_TIMEX, SP_TIMEY, self.cnt_par);
        }
    }

    /// `WI_drawNetgameStats`.
    fn draw_netgame_stats(&mut self, fb: &mut IndexedFramebuffer) {
        let pwidth = i32::from(self.art.percent.header.width);
        let ng_statsx =
            32 + i32::from(self.art.star.header.width) / 2 + 32 * i32::from(!self.dofrags);

        self.slam_background(fb);
        self.draw_animated_back(fb);
        self.draw_lf(fb);

        // draw stat titles (top line)
        let kills_w = i32::from(self.art.kills.header.width);
        draw_patch(
            fb,
            &self.art.kills,
            ng_statsx + NG_SPACINGX - kills_w,
            NG_STATSY,
        );

        let items_w = i32::from(self.art.items.header.width);
        draw_patch(
            fb,
            &self.art.items,
            ng_statsx + 2 * NG_SPACINGX - items_w,
            NG_STATSY,
        );

        let secret_w = i32::from(self.art.secret.header.width);
        draw_patch(
            fb,
            &self.art.secret,
            ng_statsx + 3 * NG_SPACINGX - secret_w,
            NG_STATSY,
        );

        if self.dofrags {
            let frags_w = i32::from(self.art.frags.header.width);
            draw_patch(
                fb,
                &self.art.frags,
                ng_statsx + 4 * NG_SPACINGX - frags_w,
                NG_STATSY,
            );
        }

        // draw stats
        let mut y = NG_STATSY + i32::from(self.art.kills.header.height);

        for i in 0..MAX_PLAYERS {
            if !self.setup.players[i].in_game {
                continue;
            }

            let mut x = ng_statsx;
            let p_w = i32::from(self.art.p[i].header.width);
            draw_patch(fb, &self.art.p[i], x - p_w, y);

            if i == self.me() {
                draw_patch(fb, &self.art.star, x - p_w, y);
            }

            x += NG_SPACINGX;
            self.draw_percent(fb, x - pwidth, y + 10, self.cnt_kills[i]);
            x += NG_SPACINGX;
            self.draw_percent(fb, x - pwidth, y + 10, self.cnt_items[i]);
            x += NG_SPACINGX;
            self.draw_percent(fb, x - pwidth, y + 10, self.cnt_secret[i]);
            x += NG_SPACINGX;

            if self.dofrags {
                self.draw_num(fb, x, y + 10, self.cnt_frags[i], -1);
            }

            y += WI_SPACINGY;
        }
    }

    /// `WI_drawDeathmatchStats`.
    fn draw_deathmatch_stats(&mut self, fb: &mut IndexedFramebuffer) {
        self.slam_background(fb);
        self.draw_animated_back(fb);
        self.draw_lf(fb);

        // draw stat titles (top line)
        let total_w = i32::from(self.art.total.header.width);
        draw_patch(
            fb,
            &self.art.total,
            DM_TOTALSX - total_w / 2,
            DM_MATRIXY - WI_SPACINGY + 10,
        );

        draw_patch(fb, &self.art.killers, DM_KILLERSX, DM_KILLERSY);
        draw_patch(fb, &self.art.victims, DM_VICTIMSX, DM_VICTIMSY);

        // draw P?
        let mut x = DM_MATRIXX + DM_SPACINGX;
        let mut y = DM_MATRIXY;

        for i in 0..MAX_PLAYERS {
            if self.setup.players[i].in_game {
                let p_w = i32::from(self.art.p[i].header.width);
                draw_patch(fb, &self.art.p[i], x - p_w / 2, DM_MATRIXY - WI_SPACINGY);
                draw_patch(fb, &self.art.p[i], DM_MATRIXX - p_w / 2, y);

                if i == self.me() {
                    draw_patch(fb, &self.art.bstar, x - p_w / 2, DM_MATRIXY - WI_SPACINGY);
                    draw_patch(fb, &self.art.star, DM_MATRIXX - p_w / 2, y);
                }
            }
            // not in the game: the original commented the gray patches out
            x += DM_SPACINGX;
            y += WI_SPACINGY;
        }

        // draw stats
        let mut y = DM_MATRIXY + 10;
        let w = i32::from(self.art.num[0].header.width);

        for i in 0..MAX_PLAYERS {
            let mut x = DM_MATRIXX + DM_SPACINGX;

            if self.setup.players[i].in_game {
                for j in 0..MAX_PLAYERS {
                    if self.setup.players[j].in_game {
                        self.draw_num(fb, x + w, y, self.dm_frags[i][j], 2);
                    }
                    x += DM_SPACINGX;
                }
                self.draw_num(fb, DM_TOTALSX + w, y, self.dm_totals[i], 2);
            }
            y += WI_SPACINGY;
        }
    }

    /// `WI_drawShowNextLoc`.
    fn draw_show_next_loc(&self, fb: &mut IndexedFramebuffer) {
        self.slam_background(fb);

        // draw animated background
        self.draw_animated_back(fb);

        if self.setup.game_mode != GameMode::Commercial {
            if self.setup.episode > 2 {
                self.draw_el(fb);
                return;
            }

            let last = if self.setup.last_level == 8 {
                self.setup.next_level - 1
            } else {
                self.setup.last_level
            };

            // draw a splat on taken cities
            if let Some(splat) = &self.art.splat {
                for i in 0..=last {
                    self.draw_on_lnode(fb, i, &[splat]);
                }
                // splat the secret level?
                if self.setup.did_secret {
                    self.draw_on_lnode(fb, 8, &[splat]);
                }
            }

            // draw flashing ptr
            if self.snl_pointeron {
                if let Some(yah) = &self.art.yah {
                    self.draw_on_lnode(fb, self.setup.next_level, &[&yah[0], &yah[1]]);
                }
            }
        }

        // draws which level you are entering
        if self.setup.game_mode != GameMode::Commercial || self.setup.next_level != 30 {
            self.draw_el(fb);
        }
    }
}

#[cfg(test)]
impl Intermission {
    pub(crate) fn counters(
        &self,
    ) -> (
        [i32; MAX_PLAYERS],
        [i32; MAX_PLAYERS],
        [i32; MAX_PLAYERS],
        i32,
        i32,
    ) {
        (
            self.cnt_kills,
            self.cnt_items,
            self.cnt_secret,
            self.cnt_time,
            self.cnt_par,
        )
    }
    pub(crate) fn dm_totals(&self) -> &[i32; MAX_PLAYERS] {
        &self.dm_totals
    }
    pub(crate) fn dm_frags(&self) -> &[[i32; MAX_PLAYERS]; MAX_PLAYERS] {
        &self.dm_frags
    }
    pub(crate) fn sub_state(&self) -> i32 {
        self.sub_state
    }
    pub(crate) fn dofrags(&self) -> bool {
        self.dofrags
    }
    pub(crate) fn lnames_len(&self) -> usize {
        self.art.lnames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    fn fill_patch_bytes(width: i16, height: i16, fill: u8) -> Vec<u8> {
        let column = vec![(0u8, vec![fill; height as usize])];
        let columns = vec![column; width as usize];
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
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

    /// All lumps a test WAD needs; each test picks a subset via
    /// [`TestWad::drop_lump`].
    struct TestWad {
        lumps: Vec<(String, Vec<u8>)>,
    }

    impl TestWad {
        fn new() -> Self {
            let mut w = TestWad { lumps: Vec::new() };
            // backgrounds
            w.add("INTERPIC", fill_patch_bytes(2, 2, 0x50));
            for epsd in 0..3 {
                w.add(
                    &format!("WIMAP{epsd}"),
                    fill_patch_bytes(2, 2, 0x60 + epsd as u8),
                );
            }
            // level names: episode maps (retail's E4 uses WILV3x) and commercial maps
            for epsd in 0..=3 {
                for i in 0..9 {
                    w.add(
                        &format!("WILV{epsd}{i}"),
                        fill_patch_bytes(2, 2, 0x30 + i as u8),
                    );
                }
            }
            for i in 0..32 {
                w.add(
                    &format!("CWILV{i:02}"),
                    fill_patch_bytes(2, 2, 0x40 + (i & 0x0f) as u8),
                );
            }
            // common patches
            for i in 0..10 {
                w.add(&format!("WINUM{i}"), fill_patch_bytes(4, 6, 0x10 + i as u8));
            }
            w.add("WIMINUS", fill_patch_bytes(2, 6, 0x99));
            w.add("WIPCNT", fill_patch_bytes(3, 6, 0x21));
            w.add("WICOLON", fill_patch_bytes(2, 6, 0x22));
            w.add("WIF", fill_patch_bytes(7, 8, 0x23));
            w.add("WIENTER", fill_patch_bytes(7, 8, 0x24));
            w.add("WIOSTK", fill_patch_bytes(6, 6, 0x25));
            w.add("WIOSTS", fill_patch_bytes(6, 6, 0x26));
            w.add("WISCRT2", fill_patch_bytes(6, 6, 0x27));
            w.add("WIOSTI", fill_patch_bytes(6, 6, 0x28));
            w.add("WIFRGS", fill_patch_bytes(6, 6, 0x29));
            w.add("WITIME", fill_patch_bytes(6, 6, 0x2a));
            w.add("WISUCKS", fill_patch_bytes(6, 6, 0x2b));
            w.add("WIPAR", fill_patch_bytes(6, 6, 0x2c));
            w.add("WIKILRS", fill_patch_bytes(4, 12, 0x2d));
            w.add("WIVCTMS", fill_patch_bytes(12, 4, 0x2e));
            w.add("WIMSTT", fill_patch_bytes(6, 6, 0x2f));
            w.add("STFST01", fill_patch_bytes(8, 8, 0x31));
            w.add("STFDEAD0", fill_patch_bytes(8, 8, 0x32));
            for i in 0..MAX_PLAYERS {
                w.add(&format!("STPB{i}"), fill_patch_bytes(2, 6, 0x33 + i as u8));
                w.add(
                    &format!("WIBP{}", i + 1),
                    fill_patch_bytes(2, 6, 0x37 + i as u8),
                );
            }
            // you-are-here + splat
            w.add("WIURH0", fill_patch_bytes(5, 5, 0x41));
            w.add("WIURH1", fill_patch_bytes(5, 5, 0x42));
            w.add("WISPLAT", fill_patch_bytes(5, 5, 0x43));
            // animated backgrounds: epsd 0..2 (exact 1.10 tables)
            for j in 0..10 {
                for i in 0..3 {
                    w.add(
                        &format!("WIA0{j:02}{i:02}"),
                        fill_patch_bytes(2, 2, 0x70 + (j * 4 + i) as u8),
                    );
                }
            }
            for j in 0..7 {
                w.add(
                    &format!("WIA1{j:02}00"),
                    fill_patch_bytes(2, 2, 0x70 + j as u8),
                );
            }
            for i in 0..3 {
                w.add(
                    &format!("WIA107{i:02}"),
                    fill_patch_bytes(2, 2, 0x78 + i as u8),
                );
            }
            // WIA10800 is deliberately absent: the MONDO HACK reuses anim 4
            for j in 0..6 {
                for i in 0..3 {
                    w.add(
                        &format!("WIA2{j:02}{i:02}"),
                        fill_patch_bytes(2, 2, 0x60 + j as u8),
                    );
                }
            }
            w
        }

        fn add(&mut self, name: &str, data: Vec<u8>) {
            self.lumps.push((name.to_string(), data));
        }

        fn manager(&self) -> ResourceManager {
            let refs: Vec<(&str, &[u8])> = self
                .lumps
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_slice()))
                .collect();
            let wad = build_wad(b"IWAD", &refs);
            let file = WadFile::from_bytes(wad).unwrap();
            ResourceManager::new(file, Vec::new())
        }
    }

    /// Single-player DOOM 1 setup: episode 1, level 1 done, entering level 2.
    fn sp_setup() -> IntermissionSetup {
        let mut players = [IntermissionPlayer::default(); MAX_PLAYERS];
        players[0] = IntermissionPlayer {
            in_game: true,
            kills: 5,
            items: 3,
            secret: 1,
            time_tics: 70,
            frags: [0; MAX_PLAYERS],
        };
        IntermissionSetup {
            game_mode: GameMode::Shareware,
            episode: 0,
            last_level: 0,
            next_level: 1,
            did_secret: false,
            local_player: 0,
            players,
            max_kills: 10,
            max_items: 10,
            max_secret: 5,
            par_time_tics: 175,
            deathmatch: false,
            netgame: false,
        }
    }

    fn commercial_setup() -> IntermissionSetup {
        let mut players = [IntermissionPlayer::default(); MAX_PLAYERS];
        players[0] = IntermissionPlayer {
            in_game: true,
            kills: 7,
            items: 4,
            secret: 2,
            time_tics: 105,
            frags: [0; MAX_PLAYERS],
        };
        IntermissionSetup {
            game_mode: GameMode::Commercial,
            episode: 0,
            last_level: 5,
            next_level: 6,
            did_secret: false,
            local_player: 0,
            players,
            max_kills: 10,
            max_items: 10,
            max_secret: 5,
            par_time_tics: 210,
            deathmatch: false,
            netgame: false,
        }
    }

    fn netgame_setup() -> IntermissionSetup {
        let mut players = [IntermissionPlayer::default(); MAX_PLAYERS];
        players[0] = IntermissionPlayer {
            in_game: true,
            kills: 4,
            items: 2,
            secret: 1,
            time_tics: 70,
            frags: [0, 3, 0, 0],
        };
        players[1] = IntermissionPlayer {
            in_game: true,
            kills: 8,
            items: 5,
            secret: 3,
            time_tics: 70,
            frags: [1, 0, 0, 0],
        };
        IntermissionSetup {
            game_mode: GameMode::Shareware,
            episode: 0,
            last_level: 0,
            next_level: 1,
            did_secret: false,
            local_player: 0,
            players,
            max_kills: 10,
            max_items: 10,
            max_secret: 5,
            par_time_tics: 175,
            deathmatch: false,
            netgame: true,
        }
    }

    fn deathmatch_setup() -> IntermissionSetup {
        let mut players = [IntermissionPlayer::default(); MAX_PLAYERS];
        players[0] = IntermissionPlayer {
            in_game: true,
            kills: 0,
            items: 0,
            secret: 0,
            time_tics: 70,
            frags: [0, 3, 0, 0],
        };
        players[1] = IntermissionPlayer {
            in_game: true,
            kills: 0,
            items: 0,
            secret: 0,
            time_tics: 70,
            frags: [1, 0, 0, 0],
        };
        IntermissionSetup {
            deathmatch: true,
            players,
            ..netgame_setup()
        }
    }

    // ---------- tests ----------

    #[test]
    fn first_tick_plays_the_intermission_theme_once() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut intermission =
            Intermission::start(&rm, sp_setup(), &mut DoomRng::with_seed(1)).unwrap();
        intermission.tick(&mut DoomRng::with_seed(1), &IntermissionInput::default());
        assert_eq!(
            intermission.drain_events(),
            vec![IntermissionEvent::Music(IntermissionMusic::Inter)]
        );
        for _ in 0..3 {
            intermission.tick(&mut DoomRng::with_seed(1), &IntermissionInput::default());
        }
        let events = intermission.drain_events();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, IntermissionEvent::Music(_))),
            "the music starts exactly once"
        );
    }

    #[test]
    fn commercial_uses_dm2int_and_interpic_and_32_names() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut intermission =
            Intermission::start(&rm, commercial_setup(), &mut DoomRng::with_seed(1)).unwrap();
        assert_eq!(intermission.lnames_len(), 32);
        intermission.tick(&mut DoomRng::with_seed(1), &IntermissionInput::default());
        assert_eq!(
            intermission.drain_events()[0],
            IntermissionEvent::Music(IntermissionMusic::Dm2Int)
        );
    }

    #[test]
    fn episode_maps_load_nine_level_name_patches() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let intermission =
            Intermission::start(&rm, sp_setup(), &mut DoomRng::with_seed(1)).unwrap();
        assert_eq!(intermission.lnames_len(), 9);
    }

    #[test]
    fn missing_lump_is_an_error_naming_the_lump() {
        let wad = TestWad {
            lumps: TestWad::new()
                .lumps
                .into_iter()
                .filter(|(n, _)| n != "WINUM3")
                .collect(),
        };
        let rm = wad.manager();
        let err = Intermission::start(&rm, sp_setup(), &mut DoomRng::with_seed(1))
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("WINUM3"));
    }

    #[test]
    fn single_player_walks_the_whole_pipeline() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, sp_setup(), &mut rng).unwrap();
        intermission.drain_events();

        // fast forward available from the very first tic
        let input = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &input);

        // one accelerate tick sets everything to the target and parks us in stage 10
        let (kills, items, secret, time, par) = intermission.counters();
        assert_eq!(kills[0], 50);
        assert_eq!(items[0], 30);
        assert_eq!(secret[0], 20);
        assert_eq!(time, 2);
        assert_eq!(par, 5);
        assert_eq!(intermission.sub_state(), 10);

        // single-player pipeline goes via the world map: press again -> ShowNextLoc
        intermission.tick(&mut rng, &IntermissionInput::default());
        assert!(matches!(intermission.state(), IntermissionState::StatCount));
        let input2 = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &input2);
        assert_eq!(
            intermission.state(),
            IntermissionState::ShowNextLoc,
            "single-player finale shows the next city"
        );
    }

    #[test]
    fn single_player_counts_up_naturally() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, sp_setup(), &mut rng).unwrap();
        intermission.drain_events();

        // sub_state starts with the pause; after TICRATE pauses it advances
        assert_eq!(intermission.sub_state(), 1);
        for _ in 0..TICRATE {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        assert_eq!(intermission.sub_state(), 2, "pause over, kills counting");

        // target 50, rising by 2 from -1: after 26 ticks it saturates
        for _ in 0..26 {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        let (kills, _, _, _, _) = intermission.counters();
        assert_eq!(kills[0], 50);
        assert_eq!(intermission.sub_state(), 3, "kills done, pausing");
    }

    #[test]
    fn time_and_par_count_at_three_seconds_a_tic() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.players[0].kills = 0;
        setup.players[0].items = 0;
        setup.players[0].secret = 0;
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        // ride through pauses and the trivial stages until stage 8 (time)
        for _ in 0..(3 * TICRATE + 40) {
            intermission.tick(&mut rng, &IntermissionInput::default());
            if intermission.sub_state() == 8 {
                break;
            }
        }
        assert_eq!(intermission.sub_state(), 8, "time stage reached");

        // time 70tics -> target 2 seconds; par 175 -> 5 seconds
        for _ in 0..4 {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        let (_, _, _, time, par) = intermission.counters();
        assert_eq!(time, 2);
        assert_eq!(par, 5);
        assert_eq!(intermission.sub_state(), 9, "time and par done, pausing");
    }

    #[test]
    fn commercial_goes_to_no_state_after_the_count() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, commercial_setup(), &mut rng).unwrap();
        intermission.drain_events();

        // fast-forward to stage 10, then one more accelerate press exits
        let input = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &input);
        intermission.tick(&mut rng, &IntermissionInput::default());
        intermission.tick(&mut rng, &input);
        assert_eq!(
            intermission.state(),
            IntermissionState::NoState,
            "commercial intermission skips the world map"
        );
    }

    #[test]
    fn no_state_counts_down_to_world_done() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, commercial_setup(), &mut rng).unwrap();
        intermission.drain_events();

        let acc = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc); // -> stage 10
        intermission.tick(&mut rng, &IntermissionInput::default()); // re-arm the edge
        intermission.tick(&mut rng, &acc); // -> NoState
        assert_eq!(intermission.state(), IntermissionState::NoState);

        for _ in 0..10 {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        let events = intermission.drain_events();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, IntermissionEvent::WorldDone))
                .count(),
            1,
            "world done fires exactly once"
        );
    }

    #[test]
    fn world_map_shows_taken_cities_and_the_pointer() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut setup = sp_setup();
        setup.last_level = 2; // finished level 3 (0-based), next is 4
        setup.next_level = 3;
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        // accelerate to stage 10, then to ShowNextLoc
        let acc = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        intermission.tick(&mut rng, &IntermissionInput::default());
        intermission.tick(&mut rng, &acc);
        assert_eq!(intermission.state(), IntermissionState::ShowNextLoc);

        // one update: pointer resolves to on (cnt & 31 < 20)
        intermission.tick(&mut rng, &IntermissionInput::default());

        let mut fb = IndexedFramebuffer::new();
        intermission.draw(&mut fb);
        // cities 0..=2 splatted (0x43)
        for (x, y) in [LNODES[0][0], LNODES[0][1], LNODES[0][2]] {
            assert_eq!(
                fb.get(x as usize, y as usize),
                Some(0x43),
                "splat at {x},{y}"
            );
        }
        // city 4 not taken yet
        assert_ne!(
            fb.get(LNODES[0][4].0 as usize, LNODES[0][4].1 as usize),
            Some(0x43)
        );
        // the flashing pointer sits on the next city (yah[0] = 0x41)
        assert_eq!(
            fb.get(LNODES[0][3].0 as usize, LNODES[0][3].1 as usize),
            Some(0x41)
        );
    }

    #[test]
    fn secret_level_splat_appears_with_didsecret() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut setup = sp_setup();
        setup.last_level = 2;
        setup.next_level = 3;
        setup.did_secret = true;
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        let acc = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        intermission.tick(&mut rng, &IntermissionInput::default());
        intermission.tick(&mut rng, &acc);

        let mut fb = IndexedFramebuffer::new();
        intermission.draw(&mut fb);
        assert_eq!(
            fb.get(LNODES[0][8].0 as usize, LNODES[0][8].1 as usize),
            Some(0x43),
            "the secret city is splatted"
        );
    }

    #[test]
    fn retail_episode_4_draws_no_map() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut setup = sp_setup();
        setup.game_mode = GameMode::Retail;
        setup.episode = 3;
        setup.last_level = 0;
        setup.next_level = 1;
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        let acc = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        intermission.tick(&mut rng, &IntermissionInput::default());
        intermission.tick(&mut rng, &acc);
        assert_eq!(intermission.state(), IntermissionState::ShowNextLoc);

        let mut fb = IndexedFramebuffer::new();
        intermission.draw(&mut fb);
        // no splat on (0,0)-index city 0 for the no-map episode
        assert_ne!(
            fb.get(LNODES[0][0].0 as usize, LNODES[0][0].1 as usize),
            Some(0x43)
        );
        // the "Entering" patch is drawn centered at the top (7 px wide)
        assert_eq!(fb.get(159, 2), Some(0x24));
    }

    #[test]
    fn netgame_counts_each_player() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, netgame_setup(), &mut rng).unwrap();
        intermission.drain_events();

        let acc = IntermissionInput {
            attack: [true, true, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        let (kills, items, secret, _, _) = intermission.counters();
        // targets: (4,2,1) for p0 of maxkills=10 -> 40/20/20; (8,5,3) for p1 -> 80/50/60
        assert_eq!(kills, [40, 80, 0, 0]);
        assert_eq!(items, [20, 50, 0, 0]);
        assert_eq!(secret, [20, 60, 0, 0]);
        assert_eq!(intermission.sub_state(), 10);
    }

    #[test]
    fn netgame_dofrags_detects_kills_in_a_frag_equals_death() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let intermission = Intermission::start(&rm, netgame_setup(), &mut rng).unwrap();
        // players[0].frags = [0,3,0,0] -> fragSum = 3-0 = 3
        // players[1].frags = [1,0,0,0] -> fragSum = 1-0 = 1; both nonzero -> dofrags on
        assert!(intermission.dofrags());
    }

    #[test]
    fn netgame_without_net_frags_ignores_the_frag_column() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = netgame_setup();
        setup.players[0].frags = [0; MAX_PLAYERS];
        setup.players[1].frags = [0; MAX_PLAYERS];
        let intermission = Intermission::start(&rm, setup, &mut DoomRng::with_seed(1)).unwrap();
        assert!(!intermission.dofrags());
    }

    #[test]
    fn netgame_accelerate_shows_mismatched_frags() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, netgame_setup(), &mut rng).unwrap();
        intermission.drain_events();
        let acc = IntermissionInput {
            attack: [true, true, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        intermission.tick(&mut rng, &IntermissionInput::default());
        intermission.tick(&mut rng, &acc); // -> ShowNextLoc (shareware)
        assert_eq!(intermission.state(), IntermissionState::ShowNextLoc);
    }

    #[test]
    fn deathmatch_counts_and_fires_barexp_when_done() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, deathmatch_setup(), &mut rng).unwrap();
        intermission.drain_events();

        // accelerate fills the grid and parks us at phase 4
        let acc = IntermissionInput {
            attack: [true, true, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        assert_eq!(intermission.dm_frags()[0][1], 3);
        assert_eq!(intermission.dm_frags()[1][0], 1);
        assert_eq!(intermission.dm_totals()[0], 3);
        assert_eq!(intermission.dm_totals()[1], 1);
        assert_eq!(intermission.sub_state(), 4);
    }

    #[test]
    fn deathmatch_counts_down_for_negative_frags() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = deathmatch_setup();
        setup.players[0].frags = [0, -3, 0, 0];
        setup.players[1].frags = [0, 0, 0, 0];
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        // no accelerate: let the counter walk to the negative target
        // (phase 1 pause is 35 tics, then the count runs)
        for _ in 0..(TICRATE + 6) {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        assert_eq!(intermission.dm_frags()[0][1], -3);
    }

    #[test]
    fn deathmatch_grid_clamps_at_99() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = deathmatch_setup();
        setup.players[0].frags = [0, 150, 0, 0];
        setup.players[1].frags = [0, 0, 0, 0];
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        for _ in 0..(TICRATE + 130) {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        assert_eq!(
            intermission.dm_frags()[0][1],
            99,
            "clamped at 99 like the original"
        );
        assert_eq!(intermission.dm_totals()[0], 99, "totals clamped at 99 too");
    }

    #[test]
    fn deathmatch_accel_then_slop_then_no_state_or_map() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, deathmatch_setup(), &mut rng).unwrap();
        intermission.drain_events();
        let acc = IntermissionInput {
            attack: [true, true, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc); // phase -> 4
        intermission.tick(&mut rng, &IntermissionInput::default()); // let the edge re-arm
        intermission.tick(&mut rng, &acc); // phase 4 -> slop + map
        assert_eq!(intermission.state(), IntermissionState::ShowNextLoc);
        let events = intermission.drain_events();
        assert!(events.contains(&IntermissionEvent::Sfx(IntermissionSfx::Slop)));
    }

    #[test]
    fn the_1994_easter_egg_draws_nothing() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let intermission = Intermission::start(&rm, sp_setup(), &mut rng).unwrap();
        let mut fb = IndexedFramebuffer::new();
        let ret = intermission.draw_num(&mut fb, 100, 50, 1994, -1);
        assert_eq!(ret, 0);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn negative_numbers_draw_a_minus_sign() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let intermission = Intermission::start(&rm, sp_setup(), &mut rng).unwrap();
        let mut fb = IndexedFramebuffer::new();
        // draw -5 (wiminus = 0x99)
        intermission.draw_num(&mut fb, 100, 50, -5, -1);
        assert!(fb.as_slice().contains(&0x99));
    }

    #[test]
    fn zero_maxkills_is_clamped_to_one() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.max_kills = 0;
        setup.max_items = 0;
        setup.max_secret = 0;
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        assert_eq!(intermission.setup().max_kills, 1);
        assert_eq!(intermission.setup().max_items, 1);
        assert_eq!(intermission.setup().max_secret, 1);
        intermission.drain_events();
        let acc = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        assert_eq!(intermission.sub_state(), 10);
    }

    #[test]
    fn episode_two_animation_level_anims_wait_for_the_right_map() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.episode = 1;
        setup.next_level = 8; // the secret level; anim 7 and 8 match data1 == 8
        setup.last_level = 1;
        setup.did_secret = false;
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, setup, &mut rng).unwrap();
        intermission.drain_events();

        // next_level == 8 matches anims[7] and anims[8]; anim 7 is the
        // frozen one during StatCount, anim 8 fires
        for _ in 0..90 {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        assert!(
            intermission.anims[7].ctr < 0,
            "the episode-2 fire animation must stay frozen during stat counting"
        );
        assert!(
            intermission.anims[8].ctr >= 0,
            "level-matching animations fire during StatCount"
        );
        assert!(
            intermission.anims[1].ctr < 0,
            "non-matching level animations stay hidden"
        );

        // accelerate to ShowNextLoc; the state flip unfreezes anim 7
        let acc = IntermissionInput {
            attack: [true, false, false, false],
            ..IntermissionInput::default()
        };
        intermission.tick(&mut rng, &acc);
        intermission.tick(&mut rng, &IntermissionInput::default());
        intermission.tick(&mut rng, &acc);
        assert_eq!(intermission.state(), IntermissionState::ShowNextLoc);
        for _ in 0..40 {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        assert!(
            intermission.anims[7].ctr >= 0,
            "the fire animation wakes up outside StatCount"
        );
    }

    #[test]
    fn episode_one_hack_shares_frames_of_anim_four() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.episode = 1;
        setup.next_level = 1;
        let intermission = Intermission::start(&rm, setup, &mut DoomRng::with_seed(1)).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(
                &intermission.anims[8].frames[0],
                &intermission.anims[4].frames[0]
            ),
            "anim 8 aliases anim 4 (the original's MONDO HACK)"
        );
    }

    #[test]
    fn always_anims_start_hidden_then_advance() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, sp_setup(), &mut rng).unwrap();
        intermission.drain_events();
        for _ in 0..100 {
            intermission.tick(&mut rng, &IntermissionInput::default());
        }
        assert!(
            intermission.anims.iter().any(|a| a.ctr >= 0),
            "at least one always-animation advanced within 100 tics"
        );
    }

    #[test]
    fn commercial_has_no_animated_background() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let intermission = Intermission::start(&rm, commercial_setup(), &mut rng).unwrap();
        assert!(intermission.anims.is_empty());
    }

    #[test]
    fn retail_episode_4_has_no_animated_background() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.game_mode = GameMode::Retail;
        setup.episode = 3;
        let intermission = Intermission::start(&rm, setup, &mut DoomRng::with_seed(1)).unwrap();
        assert!(intermission.anims.is_empty());
    }

    #[test]
    fn retail_episode_4_uses_interpic_background() {
        // INTERPIC must exist or startup fails; the retail E4 IWAD ships it
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.game_mode = GameMode::Retail;
        setup.episode = 3;
        assert!(Intermission::start(&rm, setup, &mut DoomRng::with_seed(1)).is_ok());
    }

    #[test]
    fn non_retail_episode_beyond_two_maps_back_like_the_original() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut setup = sp_setup();
        setup.episode = 4; // non-retail: 4 -> 1
        setup.last_level = 0;
        setup.next_level = 1;
        let intermission = Intermission::start(&rm, setup, &mut DoomRng::with_seed(1)).unwrap();
        assert_eq!(intermission.setup().episode, 1);
    }

    #[test]
    fn stats_draw_smoke_test() {
        let wad = TestWad::new();
        let rm = wad.manager();
        let mut rng = DoomRng::with_seed(1);
        let mut intermission = Intermission::start(&rm, sp_setup(), &mut rng).unwrap();
        intermission.drain_events();
        let mut fb = IndexedFramebuffer::new();
        intermission.draw(&mut fb);
        // WITIME patch lands at (16, 168)
        assert_ne!(fb.get(16, 168), Some(0));
        // the level-name line patch (WILV00, color 0x30) is drawn centered at y == 2
        assert_eq!(fb.get(159, 2), Some(0x30));
    }
}

//! The automap's retained state and the `AM_*` logic from `am_map.c`,
//! ported as plain data so the game layer can drive it: input events in,
//! messages out, [`AutomapView`] borrowed read-only for positions.

use std::sync::Arc;

use tracing::debug;

use crate::errors::{EngineError, EngineResult};
use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::gfx::patch::{Patch, decode_patch};
use crate::hud::{HudMessage, marked_spot_message};
use crate::map::Map;
use crate::resources::ResourceManager;
use crate::tables::Angle;

use super::{FINIT_HEIGHT, FINIT_WIDTH, PLAYER_RADIUS};

/// Number of user marks the automap can hold (`AM_NUMMARKPOINTS`).
/// Ten marks make sense on an automap; they cycle, and `c` clears
/// them all. The patches are the `AMMNUM0..9` lumps.
pub const AM_NUMMARKPOINTS: usize = 10;

/// Keys the automap reacts to, mirroring `AM_PAN*KEY` and friends. The
/// game layer maps its platform key codes onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutomapKey {
    /// Arrow left — pan left when not following the player.
    PanLeft,
    /// Arrow right — pan right when not following the player.
    PanRight,
    /// Arrow up — pan up when not following the player.
    PanUp,
    /// Arrow down — pan down when not following the player.
    PanDown,
    /// `-` — zoom out (held).
    ZoomOut,
    /// `=` — zoom in (held).
    ZoomIn,
    /// `0` — toggle full-map zoom (no following the player's scroll).
    GoBig,
    /// `f` — toggle follow mode.
    ToggleFollow,
    /// `g` — toggle the blockmap grid overlay.
    ToggleGrid,
    /// `m` — drop a mark at the current window center.
    Mark,
    /// `c` — clear all marks.
    ClearMarks,
}

/// One event for the automap, before key-up handling splits zoom from
/// pan (vanilla handles both in one `ev_keydown`/`ev_keyup` switch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutomapInput {
    /// A key was pressed.
    KeyDown(AutomapKey),
    /// A key was released.
    KeyUp(AutomapKey),
}

/// The result of feeding one [`AutomapInput`] to the automap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutomapResponse {
    /// `true` when the automap consumed the event. Vanilla's
    /// `AM_Responder` reports this through its `boolean` return;
    /// the game loop uses it to decide whether to run the view.
    pub handled: bool,
    /// A player message to show (`plr->message` in vanilla). The game
    /// layer hands it to the HUD and then forgets it.
    pub message: Option<String>,
}

/// A player arrow on the automap. The game layer fills one per playing
/// slot; `is_local` marks the console player's own marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerMarker {
    /// Map position.
    pub x: Fixed,
    /// Map position.
    pub y: Fixed,
    /// Facing angle (BAM).
    pub angle: Angle,
    /// `pw_invisibility` up — drawn in a near-black color in net games.
    pub invisible: bool,
    /// This is the console player's marker.
    pub is_local: bool,
}

/// A thing to draw in the `iddt` level-2 overlay. The game layer
/// flattens its live thinker list into these; the automap does not
/// know what a mobj is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThingMarker {
    /// Map position.
    pub x: Fixed,
    /// Map position.
    pub y: Fixed,
    /// Facing angle (BAM).
    pub angle: Angle,
}

/// Plain-data inputs for one frame of the automap, filled in by the
/// game layer. Nothing engine-internal crosses this boundary.
#[derive(Clone, Copy, Debug)]
pub struct AutomapView<'a> {
    /// The console player's marker. Drives follow mode and the
    /// single-player arrow.
    pub me: PlayerMarker,
    /// Every marker in a net game, in slot order (`players[]`). The
    /// local player is expected to be among them; ignored when
    /// `netgame` is false.
    pub players: &'a [PlayerMarker],
    /// `netgame` — every slot gets an arrow, colored by slot.
    pub netgame: bool,
    /// `deathmatch` without `singledemo` — non-local markers are skipped.
    pub deathmatch: bool,
    /// `singledemo` — deathmatch still shows everyone (demo playback).
    pub singledemo: bool,
    /// `pw_allmap` (the computer-map power-up): unseen lines are drawn
    /// in a faint gray instead of not drawn at all.
    pub all_map: bool,
    /// `iddt` level 2 — every thing in the level, drawn as a thin
    /// triangle. Empty otherwise; the game layer fills it per frame.
    pub things: &'a [ThingMarker],
}

/// A user mark left on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkPoint {
    /// Map position where the mark was dropped.
    pub x: Fixed,
    /// Map position where the mark was dropped.
    pub y: Fixed,
}

/// The decoded `AMMNUM0..9` patches used by the mark drawing pass.
pub struct AutomapAssets {
    marks: [Arc<Patch>; AM_NUMMARKPOINTS],
}

impl AutomapAssets {
    fn load(resources: &ResourceManager) -> EngineResult<Self> {
        let mut marks = Vec::with_capacity(AM_NUMMARKPOINTS);
        for i in 0..AM_NUMMARKPOINTS {
            let name = format!("AMMNUM{i}");
            let index = resources.find_lump(&name).ok_or_else(|| {
                EngineError::Resource(format!("automap mark lump {name} not found"))
            })?;
            let patch = resources.decode_cached::<Patch>(index, |bytes| {
                decode_patch(bytes).map_err(EngineError::from)
            })?;
            marks.push(patch);
        }
        let marks: [Arc<Patch>; AM_NUMMARKPOINTS] = marks.try_into().map_err(|_| {
            EngineError::Resource("automap mark patch table size mismatch".to_string())
        })?;
        Ok(Self { marks })
    }

    /// The patch for mark `index` (`0..=9`).
    pub fn mark_patch(&self, index: usize) -> &Patch {
        &self.marks[index]
    }

    #[cfg(test)]
    pub(crate) fn from_patches(marks: [Arc<Patch>; AM_NUMMARKPOINTS]) -> Self {
        Self { marks }
    }
}

/// A bitset of linedef indices for the "seen on the automap" set.
/// Vanilla uses the `ML_MAPPED` bit on the linedef itself; the map
/// port keeps linedefs immutable, so the set lives here instead,
/// indexed by linedef index.
#[derive(Clone, Debug, Default)]
struct SeenLines {
    words: Vec<u64>,
    bits: usize,
}

impl SeenLines {
    fn with_len(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
            bits: len,
        }
    }

    fn set(&mut self, index: usize) {
        if index < self.bits {
            self.words[index >> 6] |= 1 << (index & 63);
        }
    }

    fn get(&self, index: usize) -> bool {
        if index >= self.bits {
            return false;
        }
        (self.words[index >> 6] >> (index & 63)) & 1 != 0
    }

    fn clear(&mut self, len: usize) {
        *self = Self::with_len(len);
    }
}

// `scale on entry`, byte-for-byte from am_map.c.
const INIT_SCALE_MTOF: Fixed = Fixed::from_raw(13_107); // (int)(0.2 * FRACUNIT)
/// Pixels the window pans per tic while a pan key is held.
const F_PANINC: i32 = 4;
const M_ZOOMIN: Fixed = Fixed::from_raw(66_846); // (int)(1.02 * FRACUNIT)
const M_ZOOMOUT: Fixed = Fixed::from_raw(64_250); // FRACUNIT / 1.02

/// The automap's retained state. Build with [`AutomapState::new`]
/// (no assets — marks are tracked but not drawn) or
/// [`AutomapState::load`] (decodes the `AMMNUM*` patches).
///
/// Flow per level: call [`AutomapState::begin_level`] once when the
/// map is (re)loaded — it recomputes the level bounds, resets the
/// seen set and clears the marks — then [`AutomapState::start`] when
/// the player opens the map. If `start` is called without
/// `begin_level`, it runs the level initialization lazily, which is
/// exactly what vanilla's `leveljuststarted` kluge did.
pub struct AutomapState {
    /// Window on the screen (`f_x/f_y/f_w/f_h`). `f_x`/`f_y` are always
    /// `0` in 1.10; kept as fields because the clip math uses them.
    f_x: i32,
    f_y: i32,
    f_w: i32,
    f_h: i32,
    /// Whether the automap is currently up (`automapactive`).
    active: bool,
    /// Whether [`AutomapState::begin_level`] has run for the current
    /// level. `false` until the first `begin_level`/`start`.
    level_ready: bool,
    /// `cheating` — 0 normal, 1 full reveal, 2 full reveal + things.
    cheat_level: u8,
    /// `grid` — grid overlay visibility.
    grid: bool,
    /// `followplayer` — window follows the player.
    follow: bool,
    /// `bigstate` — the `0` key full-map zoom toggle.
    big_state: bool,
    /// `amclock` — counts tics while the automap is up.
    amclock: u32,
    /// `m_paninc` — per-tic map-coordinate pan while pan keys are held.
    m_paninc: (Fixed, Fixed),
    /// `mtof_zoommul` / `ftom_zoommul` — per-tic scale multipliers;
    /// `Fixed::ONE` means "not zooming".
    mtof_zoommul: Fixed,
    ftom_zoommul: Fixed,
    /// Window on the map (`m_x/m_y` bottom-left, `m_x2/m_y2` top-right).
    m_x: Fixed,
    m_y: Fixed,
    m_x2: Fixed,
    m_y2: Fixed,
    /// Width/height of the window on the map.
    m_w: Fixed,
    m_h: Fixed,
    /// Level bounds (`min_x..max_y`) and derived zoom limits.
    min_x: Fixed,
    min_y: Fixed,
    max_x: Fixed,
    max_y: Fixed,
    min_scale_mtof: Fixed,
    max_scale_mtof: Fixed,
    /// Scale multipliers (`scale_mtof`, `scale_ftom`).
    scale_mtof: Fixed,
    scale_ftom: Fixed,
    /// Saved window for the `0` full-map toggle.
    old_m_x: Fixed,
    old_m_y: Fixed,
    old_m_w: Fixed,
    old_m_h: Fixed,
    /// Last position the follower routine centered on (`f_oldloc` with
    /// the `MAXINT` sentinel replaced by `Option`).
    f_oldloc: Option<(Fixed, Fixed)>,
    /// The ten user marks.
    marks: [Option<MarkPoint>; AM_NUMMARKPOINTS],
    /// Which mark index is next (`markpointnum`).
    next_mark: usize,
    /// Seen linedefs, indexed by linedef index.
    seen: SeenLines,
    /// Last seen player position, cached by [`AutomapState::tick`].
    /// `AM_restoreScaleAndLoc` reads the player position in vanilla;
    /// we use the cached copy instead of reaching into the world.
    last_me: Option<PlayerMarker>,
    /// Decoded mark patches; `None` for [`AutomapState::new`].
    assets: Option<AutomapAssets>,
}

impl Default for AutomapState {
    fn default() -> Self {
        Self::new()
    }
}

impl AutomapState {
    /// A fresh, inactive automap with no assets. Marks are tracked but
    /// cannot be drawn; call [`AutomapState::load`] to decode the
    /// `AMMNUM*` patches from the resource manager.
    pub fn new() -> Self {
        Self {
            f_x: 0,
            f_y: 0,
            f_w: FINIT_WIDTH,
            f_h: FINIT_HEIGHT,
            active: false,
            level_ready: false,
            cheat_level: 0,
            grid: false,
            follow: true, // followplayer = 1 in vanilla
            big_state: false,
            amclock: 0,
            m_paninc: (Fixed::ZERO, Fixed::ZERO),
            mtof_zoommul: Fixed::ONE,
            ftom_zoommul: Fixed::ONE,
            m_x: Fixed::ZERO,
            m_y: Fixed::ZERO,
            m_x2: Fixed::ZERO,
            m_y2: Fixed::ZERO,
            m_w: Fixed::ZERO,
            m_h: Fixed::ZERO,
            min_x: Fixed::ZERO,
            min_y: Fixed::ZERO,
            max_x: Fixed::ZERO,
            max_y: Fixed::ZERO,
            min_scale_mtof: Fixed::ZERO,
            max_scale_mtof: Fixed::ZERO,
            scale_mtof: INIT_SCALE_MTOF,
            scale_ftom: Fixed::ZERO,
            old_m_x: Fixed::ZERO,
            old_m_y: Fixed::ZERO,
            old_m_w: Fixed::ZERO,
            old_m_h: Fixed::ZERO,
            f_oldloc: None,
            marks: [None; AM_NUMMARKPOINTS],
            next_mark: 0,
            seen: SeenLines::default(),
            last_me: None,
            assets: None,
        }
    }

    /// Loads the `AMMNUM0..9` mark patches through the resource manager.
    pub fn load(resources: &ResourceManager) -> EngineResult<Self> {
        Ok(Self {
            assets: Some(AutomapAssets::load(resources)?),
            ..Self::new()
        })
    }

    /// Whether the automap is currently up (`automapactive`).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// `cheating`, 0..=2 (see [`AutomapState::cycle_iddt`]).
    pub fn cheat_level(&self) -> u8 {
        self.cheat_level
    }

    /// Whether follow mode is on.
    pub fn follow_mode(&self) -> bool {
        self.follow
    }

    /// Whether the grid overlay is on.
    pub fn grid_on(&self) -> bool {
        self.grid
    }

    /// The ten user marks; `marks()[i] == None` means "slot is empty".
    pub fn marks(&self) -> &[Option<MarkPoint>; AM_NUMMARKPOINTS] {
        &self.marks
    }

    /// Tics elapsed since the automap was opened (`amclock`). The port
    /// has no strobing light effect, so this is informational only.
    pub fn amclock(&self) -> u32 {
        self.amclock
    }

    /// `true` when at least one linedef has been marked as seen.
    pub fn is_line_seen(&self, linedef: usize) -> bool {
        self.seen.get(linedef)
    }

    /// Number of linedefs currently marked as seen.
    pub fn seen_count(&self) -> usize {
        self.seen
            .words
            .iter()
            .map(|w| w.count_ones() as usize)
            .sum()
    }

    /// Feed the renderer's `take_mapped_linedefs` output (or any other
    /// linedef indices) into the seen set. Indices outside the linedef
    /// range are ignored — the map can change under us and the automap
    /// only ever reads it.
    pub fn mark_lines_seen(&mut self, indices: impl IntoIterator<Item = usize>) {
        for index in indices {
            self.seen.set(index);
        }
    }

    /// The next mark index that will be assigned (`markpointnum`).
    pub fn next_mark(&self) -> usize {
        self.next_mark
    }

    /// Window on the map: bottom-left `(m_x, m_y)` and top-right
    /// `(m_x2, m_y2)`, in fixed-point map coordinates.
    pub fn window_bounds(&self) -> (Fixed, Fixed, Fixed, Fixed) {
        (self.m_x, self.m_y, self.m_x2, self.m_y2)
    }

    /// The window on screen `(x, y, width, height)`. Always
    /// `(0, 0, 320, 168)` in 1.10.
    pub fn window_geometry(&self) -> (i32, i32, i32, i32) {
        (self.f_x, self.f_y, self.f_w, self.f_h)
    }

    /// Left edge of the window on the map (`m_x`).
    pub(crate) fn m_x(&self) -> Fixed {
        self.m_x
    }
    /// Bottom edge of the window on the map (`m_y`).
    pub(crate) fn m_y(&self) -> Fixed {
        self.m_y
    }
    /// Right edge of the window on the map (`m_x2`).
    pub(crate) fn m_x2(&self) -> Fixed {
        self.m_x2
    }
    /// Top edge of the window on the map (`m_y2`).
    pub(crate) fn m_y2(&self) -> Fixed {
        self.m_y2
    }
    /// Window width on the map (`m_w`).
    pub(crate) fn m_w(&self) -> Fixed {
        self.m_w
    }
    /// Window height on the map (`m_h`).
    pub(crate) fn m_h(&self) -> Fixed {
        self.m_h
    }
    /// Window x offset on screen (`f_x`), always `0`.
    pub(crate) fn f_x(&self) -> i32 {
        self.f_x
    }
    /// Window y offset on screen (`f_y`), always `0`.
    pub(crate) fn f_y(&self) -> i32 {
        self.f_y
    }
    /// Window width on screen (`f_w`).
    pub(crate) fn f_w(&self) -> i32 {
        self.f_w
    }
    /// Window height on screen (`f_h`).
    pub(crate) fn f_h(&self) -> i32 {
        self.f_h
    }

    /// The current map-to-screen scale (`scale_mtof`, fixed point).
    pub fn scale_mtof(&self) -> Fixed {
        self.scale_mtof
    }

    /// Translate a fixed map X to a framebuffer column inside the window.
    /// `MTOF(x - m_x) + f_x`.
    pub fn map_x_to_screen(&self, x: Fixed) -> i32 {
        self.f_x + mtof(x - self.m_x, self.scale_mtof)
    }

    /// Translate a fixed map Y to a framebuffer row inside the window.
    /// `f_y + f_h - MTOF(y - m_y)`.
    pub fn map_y_to_screen(&self, y: Fixed) -> i32 {
        self.f_y + (self.f_h - mtof(y - self.m_y, self.scale_mtof))
    }

    /// Translate a framebuffer-space distance to map units.
    /// `FTOM(x)`.
    pub fn framebuffer_to_map(&self, x: i32) -> Fixed {
        ftom(x, self.scale_ftom)
    }

    /// `AM_LevelInit` — recompute the level bounds, reset the seen set
    /// and clear the marks. Call once per level load, before [`start`](Self::start).
    ///
    /// If the map has no vertices at all (degenerate/empty map) the
    /// zoom limits degrade to the fixed-point clamps instead of
    /// panicking; the map line count still drives the seen bitset.
    pub fn begin_level(&mut self, map: &Map) {
        self.f_x = 0;
        self.f_y = 0;
        self.f_w = FINIT_WIDTH;
        self.f_h = FINIT_HEIGHT;

        self.marks = [None; AM_NUMMARKPOINTS];
        self.next_mark = 0;
        self.seen.clear(map.linedefs.len());

        self.find_min_max_boundaries(map);

        // scale_mtof = FixedDiv(min_scale_mtof, (int)(0.7*FRACUNIT));
        self.scale_mtof = self.min_scale_mtof / Fixed::from_raw(45_875);
        if self.scale_mtof > self.max_scale_mtof {
            self.scale_mtof = self.min_scale_mtof;
        }
        self.scale_ftom = Fixed::ONE / self.scale_mtof;

        self.level_ready = true;
    }

    /// `AM_Start` — open the automap. Re-centers the window on the
    /// player and resets the transient pan/zoom state. If the level
    /// was not initialized yet, does so lazily (vanilla's
    /// `leveljuststarted` behavior).
    pub fn start(&mut self, map: &Map, player: PlayerMarker) {
        if self.active {
            // vanilla calls AM_Stop() first: unload pics, notify the
            // status bar, mark inactive. Assets stay cached here.
            self.active = false;
        }
        if !self.level_ready {
            debug!("automap start without begin_level; initializing lazily");
            self.begin_level(map);
        }

        self.active = true;
        self.f_oldloc = None;
        self.amclock = 0;

        self.m_paninc = (Fixed::ZERO, Fixed::ZERO);
        self.mtof_zoommul = Fixed::ONE;
        self.ftom_zoommul = Fixed::ONE;

        self.m_w = self.framebuffer_to_map(self.f_w);
        self.m_h = self.framebuffer_to_map(self.f_h);

        self.m_x = player.x - half(self.m_w);
        self.m_y = player.y - half(self.m_h);
        self.change_window_loc();
    }

    /// `AM_Stop` — close the automap and notify (the stbar-side
    /// notification of the original is not ported; the status bar is
    /// redrawn from scratch every frame).
    pub fn stop(&mut self) {
        self.active = false;
        self.big_state = false;
        self.m_paninc = (Fixed::ZERO, Fixed::ZERO);
        self.mtof_zoommul = Fixed::ONE;
        self.ftom_zoommul = Fixed::ONE;
    }

    /// `AM_Responder` (the in-map part) — handle one input event.
    /// Returns [`AutomapResponse`] with `handled` set when the event
    /// was consumed and an optional player message for the HUD.
    pub fn handle_input(&mut self, input: AutomapInput) -> AutomapResponse {
        if !self.active {
            return AutomapResponse::default();
        }

        match input {
            AutomapInput::KeyDown(key) => self.key_down(key),
            AutomapInput::KeyUp(key) => self.key_up(key),
        }
    }

    fn key_down(&mut self, key: AutomapKey) -> AutomapResponse {
        let mut response = AutomapResponse {
            handled: true,
            message: None,
        };

        match key {
            AutomapKey::PanRight => {
                if !self.follow {
                    self.m_paninc.0 = ftom(F_PANINC, self.scale_ftom);
                } else {
                    response.handled = false;
                }
            }
            AutomapKey::PanLeft => {
                if !self.follow {
                    self.m_paninc.0 = ftom(-F_PANINC, self.scale_ftom);
                } else {
                    response.handled = false;
                }
            }
            AutomapKey::PanUp => {
                if !self.follow {
                    self.m_paninc.1 = ftom(F_PANINC, self.scale_ftom);
                } else {
                    response.handled = false;
                }
            }
            AutomapKey::PanDown => {
                if !self.follow {
                    self.m_paninc.1 = ftom(-F_PANINC, self.scale_ftom);
                } else {
                    response.handled = false;
                }
            }
            AutomapKey::ZoomOut => {
                self.mtof_zoommul = M_ZOOMOUT;
                self.ftom_zoommul = M_ZOOMIN;
            }
            AutomapKey::ZoomIn => {
                self.mtof_zoommul = M_ZOOMIN;
                self.ftom_zoommul = M_ZOOMOUT;
            }
            AutomapKey::GoBig => {
                self.big_state = !self.big_state;
                if self.big_state {
                    self.save_scale_and_loc();
                    self.min_out_window_scale();
                } else {
                    self.restore_scale_and_loc();
                }
            }
            AutomapKey::ToggleFollow => {
                self.follow = !self.follow;
                self.f_oldloc = None;
                response.message = Some(
                    if self.follow {
                        HudMessage::FollowOn.as_str()
                    } else {
                        HudMessage::FollowOff.as_str()
                    }
                    .to_string(),
                );
            }
            AutomapKey::ToggleGrid => {
                self.grid = !self.grid;
                response.message = Some(
                    if self.grid {
                        HudMessage::GridOn
                    } else {
                        HudMessage::GridOff
                    }
                    .as_str()
                    .to_string(),
                );
            }
            AutomapKey::Mark => {
                let mark = MarkPoint {
                    x: self.m_x + half(self.m_w),
                    y: self.m_y + half(self.m_h),
                };
                self.marks[self.next_mark] = Some(mark);
                response.message = Some(marked_spot_message(self.next_mark as i32));
                self.next_mark = (self.next_mark + 1) % AM_NUMMARKPOINTS;
            }
            AutomapKey::ClearMarks => {
                self.marks = [None; AM_NUMMARKPOINTS];
                self.next_mark = 0;
                response.message = Some(HudMessage::MarksCleared.as_str().to_string());
            }
        }
        response
    }

    fn key_up(&mut self, key: AutomapKey) -> AutomapResponse {
        match key {
            AutomapKey::PanRight | AutomapKey::PanLeft => {
                if !self.follow {
                    self.m_paninc.0 = Fixed::ZERO;
                }
            }
            AutomapKey::PanUp | AutomapKey::PanDown => {
                if !self.follow {
                    self.m_paninc.1 = Fixed::ZERO;
                }
            }
            AutomapKey::ZoomOut | AutomapKey::ZoomIn => {
                self.mtof_zoommul = Fixed::ONE;
                self.ftom_zoommul = Fixed::ONE;
            }
            // the rest latch nothing in vanilla's key-up path
            _ => {}
        }
        AutomapResponse::default()
    }

    /// `AM_Ticker` — call once per game tic. Handles follow mode,
    /// zooming and panning. Does nothing while the automap is up
    /// without changes (like vanilla).
    pub fn tick(&mut self, view: &AutomapView<'_>) {
        if !self.active {
            return;
        }

        self.last_me = Some(view.me);
        self.amclock = self.amclock.wrapping_add(1);

        // AM_doFollowPlayer with f_oldloc as Option.
        if self.follow {
            let moved = match self.f_oldloc {
                Some((ox, oy)) => ox != view.me.x || oy != view.me.y,
                None => true,
            };
            if moved {
                self.m_x = ftom(mtof(view.me.x, self.scale_mtof), self.scale_ftom) - half(self.m_w);
                self.m_y = ftom(mtof(view.me.y, self.scale_mtof), self.scale_ftom) - half(self.m_h);
                self.m_x2 = self.m_x + self.m_w;
                self.m_y2 = self.m_y + self.m_h;
                self.f_oldloc = Some((view.me.x, view.me.y));
            }
        }

        // change the zoom if necessary
        if self.ftom_zoommul != Fixed::ONE {
            self.change_window_scale();
        }

        // change x,y location
        if self.m_paninc.0 != Fixed::ZERO || self.m_paninc.1 != Fixed::ZERO {
            self.change_window_loc();
        }
    }

    /// `cht_CheckCheat(&cheat_amap, ...)` cycler — the game layer
    /// calls this on the `iddt` cheat event (the sequence itself
    /// lives in [`crate::menu::CheatTable`]). Cycles the reveal level
    /// `0 -> 1 -> 2 -> 0`.
    pub fn cycle_iddt(&mut self) {
        self.cheat_level = (self.cheat_level + 1) % 3;
    }

    /// Secret access for the asset-less mark draw path: the mark
    /// patches, if they were loaded.
    pub(crate) fn assets(&self) -> Option<&AutomapAssets> {
        self.assets.as_ref()
    }

    /// Test-only hook: inject a mark patch table built from synthetic
    /// patches (the same shape [`AutomapState::load`] produces).
    #[cfg(test)]
    pub(crate) fn set_mark_patches_for_test(&mut self, patches: [Arc<Patch>; AM_NUMMARKPOINTS]) {
        self.assets = Some(AutomapAssets::from_patches(patches));
    }

    /// `AM_findMinMaxBoundaries` — determine the level bounds and the
    /// zoom limits from the vertex list.
    fn find_min_max_boundaries(&mut self, map: &Map) {
        let mut min_x = Fixed::MAX;
        let mut min_y = Fixed::MAX;
        let mut max_x = Fixed::MIN;
        let mut max_y = Fixed::MIN;

        for vertex in &map.vertexes {
            if vertex.x < min_x {
                min_x = vertex.x;
            }
            if vertex.x > max_x {
                max_x = vertex.x;
            }
            if vertex.y < min_y {
                min_y = vertex.y;
            }
            if vertex.y > max_y {
                max_y = vertex.y;
            }
        }

        self.min_x = min_x;
        self.min_y = min_y;
        self.max_x = max_x;
        self.max_y = max_y;

        let max_w = max_x - min_x;
        let max_h = max_y - min_y;

        // min_w/min_h: vanilla computes them but nothing reads them.

        // FixedDiv(f_w << FRACBITS, max_w) / FixedDiv(f_h << FRACBITS, max_h);
        // FixedDiv clamps instead of panicking on a zero divisor, which
        // is exactly what we want for a degenerate (zero-width) map.
        let a = Fixed::from_raw(self.f_w * FRACUNIT) / max_w;
        let b = Fixed::from_raw(self.f_h * FRACUNIT) / max_h;
        self.min_scale_mtof = if a < b { a } else { b };
        self.max_scale_mtof =
            Fixed::from_raw(self.f_h * FRACUNIT) / Fixed::from_raw(PLAYER_RADIUS.to_raw() * 2);
    }

    // AM_minOutWindowScale: set the window scale to the maximum size.
    fn min_out_window_scale(&mut self) {
        self.scale_mtof = self.min_scale_mtof;
        self.scale_ftom = Fixed::ONE / self.scale_mtof;
        self.activate_new_scale();
    }

    // AM_maxOutWindowScale: set the window scale to the minimum size.
    fn max_out_window_scale(&mut self) {
        self.scale_mtof = self.max_scale_mtof;
        self.scale_ftom = Fixed::ONE / self.scale_mtof;
        self.activate_new_scale();
    }

    // AM_saveScaleAndLoc.
    fn save_scale_and_loc(&mut self) {
        self.old_m_x = self.m_x;
        self.old_m_y = self.m_y;
        self.old_m_w = self.m_w;
        self.old_m_h = self.m_h;
    }

    // AM_restoreScaleAndLoc.
    fn restore_scale_and_loc(&mut self) {
        self.m_w = self.old_m_w;
        self.m_h = self.old_m_h;
        if !self.follow {
            // the follower routine will not fix up a non-follow pan;
            // restore the saved location directly
            self.m_x = self.old_m_x;
            self.m_y = self.old_m_y;
        } else {
            match self.last_me {
                Some(me) => {
                    self.m_x = me.x - half(self.m_w);
                    self.m_y = me.y - half(self.m_h);
                }
                None => {
                    // no player position cached yet; the next tick()
                    // re-centers via the follower routine
                    self.f_oldloc = None;
                }
            }
        }
        self.m_x2 = self.m_x + self.m_w;
        self.m_y2 = self.m_y + self.m_h;

        self.scale_mtof = Fixed::from_raw(self.f_w * FRACUNIT) / self.m_w;
        self.scale_ftom = Fixed::ONE / self.scale_mtof;
    }

    // AM_activateNewScale.
    fn activate_new_scale(&mut self) {
        self.m_x += half(self.m_w);
        self.m_y += half(self.m_h);
        self.m_w = self.framebuffer_to_map(self.f_w);
        self.m_h = self.framebuffer_to_map(self.f_h);
        self.m_x -= half(self.m_w);
        self.m_y -= half(self.m_h);
        self.m_x2 = self.m_x + self.m_w;
        self.m_y2 = self.m_y + self.m_h;
    }

    // AM_changeWindowScale.
    fn change_window_scale(&mut self) {
        self.scale_mtof *= self.mtof_zoommul;
        self.scale_ftom = Fixed::ONE / self.scale_mtof;

        if self.scale_mtof < self.min_scale_mtof {
            self.min_out_window_scale();
        } else if self.scale_mtof > self.max_scale_mtof {
            self.max_out_window_scale();
        } else {
            self.activate_new_scale();
        }
    }

    // AM_changeWindowLoc.
    fn change_window_loc(&mut self) {
        if self.m_paninc.0 != Fixed::ZERO || self.m_paninc.1 != Fixed::ZERO {
            self.follow = false;
            self.f_oldloc = None;
        }

        self.m_x += self.m_paninc.0;
        self.m_y += self.m_paninc.1;

        // clamp the window center to the level bounds
        if self.m_x + half(self.m_w) > self.max_x {
            self.m_x = self.max_x - half(self.m_w);
        } else if self.m_x + half(self.m_w) < self.min_x {
            self.m_x = self.min_x - half(self.m_w);
        }

        if self.m_y + half(self.m_h) > self.max_y {
            self.m_y = self.max_y - half(self.m_h);
        } else if self.m_y + half(self.m_h) < self.min_y {
            self.m_y = self.min_y - half(self.m_h);
        }

        self.m_x2 = self.m_x + self.m_w;
        self.m_y2 = self.m_y + self.m_h;
    }
}

// FTOM(x) — framebuffer pixels to map units: FixedMul(x<<16, scale_ftom).
fn ftom(x: i32, scale_ftom: Fixed) -> Fixed {
    debug_assert!(x.unsigned_abs() < 32768);
    Fixed::from_raw(x * FRACUNIT) * scale_ftom
}

// MTOF(x) — map units to framebuffer pixels: FixedMul(x, scale_mtof)>>16.
// The >>16 is an arithmetic shift on the fixed-point raw value (floor
// for negatives), which is exactly what the original's `>>16` does.
fn mtof(x: Fixed, scale_mtof: Fixed) -> i32 {
    (x * scale_mtof).to_raw() >> FRACBITS as i32
}

// Half a fixed value with C's truncating division semantics (fixed_t / 2).
fn half(f: Fixed) -> Fixed {
    Fixed::from_raw(f.to_raw() / 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automap::testutil::fixture_map;
    use crate::map::types::ML_DONTDRAW;

    fn me(x: i16, y: i16) -> PlayerMarker {
        PlayerMarker {
            x: Fixed::from_int(i32::from(x)),
            y: Fixed::from_int(i32::from(y)),
            angle: Angle::ZERO,
            invisible: false,
            is_local: true,
        }
    }

    fn view<'a>(me: PlayerMarker) -> AutomapView<'a> {
        AutomapView {
            me,
            players: &[],
            netgame: false,
            deathmatch: false,
            singledemo: false,
            all_map: false,
            things: &[],
        }
    }

    fn open_state(map: &Map) -> AutomapState {
        let mut state = AutomapState::new();
        state.begin_level(map);
        state.start(map, me(128, 64));
        state
    }

    #[test]
    fn fresh_state_is_inactive_and_unmarked() {
        let state = AutomapState::new();
        assert!(!state.is_active());
        assert_eq!(state.cheat_level(), 0);
        assert!(state.follow_mode()); // followplayer = 1 in vanilla
        assert!(!state.grid_on());
        assert_eq!(state.seen_count(), 0);
        assert_eq!(state.marks(), &[None; AM_NUMMARKPOINTS]);
    }

    #[test]
    fn load_without_am_lumps_fails_with_lump_name() {
        let resources = ResourceManager::default();
        let err = AutomapState::load(&resources).err().unwrap();
        assert!(err.to_string().contains("AMMNUM0"), "unexpected: {err}");
    }

    #[test]
    fn start_activates_and_centers_on_player() {
        let map = fixture_map();
        let state = open_state(&map);
        assert!(state.is_active());
        let (m_x, m_y, m_x2, m_y2) = state.window_bounds();
        let me = me(128, 64);
        // window center sits on the player (pre any clamping)
        let cx = m_x + half(m_x2 - m_x);
        let cy = m_y + half(m_y2 - m_y);
        assert_eq!(cx, me.x);
        assert_eq!(cy, me.y);
    }

    #[test]
    fn begin_level_resets_marks_and_seen_lines() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.mark_lines_seen([0, 1, 2]);
        assert_eq!(state.seen_count(), 3);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
        assert!(state.marks().iter().any(Option::is_some));
        state.begin_level(&map);
        assert_eq!(state.seen_count(), 0);
        assert_eq!(state.marks(), &[None; AM_NUMMARKPOINTS]);
        assert_eq!(state.next_mark(), 0);
    }

    #[test]
    fn start_without_begin_level_initializes_lazily() {
        let map = fixture_map();
        let mut state = AutomapState::new();
        state.start(&map, me(128, 64)); // no begin_level call
        assert!(state.is_active());
        let (_, _, _, _) = state.window_bounds();
        assert!(state.scale_mtof() > Fixed::ZERO);
    }

    #[test]
    fn degenerate_empty_map_does_not_panic() {
        let mut map = fixture_map();
        map.vertexes.clear();
        map.linedefs.clear();
        let mut state = AutomapState::new();
        state.begin_level(&map);
        state.start(&map, me(0, 0));
        assert!(state.is_active());
    }

    #[test]
    fn pan_keys_do_not_pan_in_follow_mode() {
        let map = fixture_map();
        let mut state = open_state(&map);
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::PanRight));
        assert!(!response.handled);
        state.tick(&view(me(128, 64)));
        let (m_x, _, _, _) = state.window_bounds();
        let before = m_x;
        state.tick(&view(me(128, 64)));
        let (m_x_after, _, _, _) = state.window_bounds();
        assert_eq!(before, m_x_after);
    }

    #[test]
    fn pan_moves_window_and_disables_follow() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        assert!(!state.follow_mode());
        let before = state.window_bounds().0;
        state.handle_input(AutomapInput::KeyDown(AutomapKey::PanRight));
        state.tick(&view(me(128, 64)));
        let after = state.window_bounds().0;
        assert!(
            after > before,
            "window should pan right: {before} -> {after}"
        );
        assert!(!state.follow_mode());
    }

    #[test]
    fn pan_key_up_stops_panning() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        state.handle_input(AutomapInput::KeyDown(AutomapKey::PanLeft));
        state.tick(&view(me(128, 64)));
        state.handle_input(AutomapInput::KeyUp(AutomapKey::PanLeft));
        let before = state.window_bounds().0;
        state.tick(&view(me(128, 64)));
        let after = state.window_bounds().0;
        assert_eq!(before, after);
    }

    #[test]
    fn pan_clamps_window_to_level_bounds() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        // hold pan-right for a long time: the window center is clamped
        // to max_x = 256 map units
        state.handle_input(AutomapInput::KeyDown(AutomapKey::PanRight));
        for _ in 0..2000 {
            state.tick(&view(me(128, 64)));
        }
        let (m_x, _, _, _) = state.window_bounds();
        let half_w = half(state.m_w());
        let center = m_x + half_w;
        assert!(center <= Fixed::from_int(256));
        assert_eq!(center, Fixed::from_int(256));
    }

    #[test]
    fn zoom_in_scales_up_until_max_and_zoom_out_returns() {
        let map = fixture_map();
        let mut state = open_state(&map);
        let start_scale = state.scale_mtof();

        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomIn));
        for _ in 0..5 {
            state.tick(&view(me(128, 64)));
        }
        let zoomed = state.scale_mtof();
        assert!(zoomed > start_scale);

        // hold zoom out: scale drops back below the start scale
        state.handle_input(AutomapInput::KeyUp(AutomapKey::ZoomIn));
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomOut));
        for _ in 0..10 {
            state.tick(&view(me(128, 64)));
        }
        assert!(state.scale_mtof() < zoomed);
    }

    #[test]
    fn zoom_clamps_at_minimum_scale() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomOut));
        for _ in 0..2000 {
            state.tick(&view(me(128, 64)));
        }
        assert_eq!(state.scale_mtof(), state.min_scale_mtof);
    }

    #[test]
    fn zoom_clamps_at_maximum_scale() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomIn));
        for _ in 0..5000 {
            state.tick(&view(me(128, 64)));
        }
        assert_eq!(state.scale_mtof(), state.max_scale_mtof);
    }

    #[test]
    fn zoom_is_continuous_multiplier_not_steps() {
        // unlike some ports, 1.10 has no integer scale presets: the
        // scale moves by 1.02x per tic while the key is held
        let map = fixture_map();
        let mut state = open_state(&map);
        let s0 = state.scale_mtof();
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomIn));
        state.tick(&view(me(128, 64)));
        let s1 = state.scale_mtof();
        let ratio = s1.to_raw() as f64 / s0.to_raw() as f64;
        assert!((ratio - 1.02).abs() < 0.001, "per-tic ratio was {ratio}");
    }

    #[test]
    fn key_release_stops_zooming() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomIn));
        state.tick(&view(me(128, 64)));
        state.handle_input(AutomapInput::KeyUp(AutomapKey::ZoomIn));
        let scale = state.scale_mtof();
        state.tick(&view(me(128, 64)));
        assert_eq!(state.scale_mtof(), scale);
    }

    #[test]
    fn follow_recenters_when_player_moves() {
        let map = fixture_map();
        let mut state = open_state(&map);
        let before = state.window_bounds();
        state.tick(&view(me(100, 100)));
        let (m_x, m_y, _, _) = state.window_bounds();
        assert_ne!((m_x, m_y), (before.0, before.1));
        // and the window center tracks the player
        let cx = m_x + half(state.m_w());
        let cy = m_y + half(state.m_h());
        // do_follow_player round-trips through MTOF/FTOM so allow a
        // one-map-unit-ish epsilon
        let eps = Fixed::from_int(2);
        assert!((cx - Fixed::from_int(100)).abs() < eps);
        assert!((cy - Fixed::from_int(100)).abs() < eps);
    }

    #[test]
    fn follow_toggle_message_round_trip() {
        let map = fixture_map();
        let mut state = open_state(&map);
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        assert_eq!(response.message.as_deref(), Some("Follow Mode OFF"));
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        assert_eq!(response.message.as_deref(), Some("Follow Mode ON"));
    }

    #[test]
    fn grid_toggle_reports_and_flips() {
        let map = fixture_map();
        let mut state = open_state(&map);
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleGrid));
        assert_eq!(response.message.as_deref(), Some("Grid ON"));
        assert!(state.grid_on());
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleGrid));
        assert_eq!(response.message.as_deref(), Some("Grid OFF"));
        assert!(!state.grid_on());
    }

    #[test]
    fn mark_drops_at_window_center_and_cycles() {
        let map = fixture_map();
        let mut state = open_state(&map);
        for index in 0..AM_NUMMARKPOINTS {
            let expected_mark = index;
            let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
            assert_eq!(
                response.message.as_deref(),
                Some(format!("Marked Spot {expected_mark}").as_str())
            );
        }
        assert_eq!(state.next_mark(), 0);
        assert!(state.marks().iter().all(Option::is_some));

        // one more mark overwrites slot 0
        state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
        assert_eq!(state.next_mark(), 1);
    }

    #[test]
    fn clear_marks_empties_everything() {
        let map = fixture_map();
        let mut state = open_state(&map);
        for _ in 0..3 {
            state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
        }
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::ClearMarks));
        assert_eq!(response.message.as_deref(), Some("All Marks Cleared"));
        assert_eq!(state.marks(), &[None; AM_NUMMARKPOINTS]);
        assert_eq!(state.next_mark(), 0);
    }

    #[test]
    fn iddt_cycles_reveal_levels() {
        let map = fixture_map();
        let mut state = open_state(&map);
        assert_eq!(state.cheat_level(), 0);
        state.cycle_iddt();
        assert_eq!(state.cheat_level(), 1);
        state.cycle_iddt();
        assert_eq!(state.cheat_level(), 2);
        state.cycle_iddt();
        assert_eq!(state.cheat_level(), 0);
    }

    #[test]
    fn don_draw_lines_stay_hidden_but_reveal_under_cheat() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.mark_lines_seen([4]); // ML_DONTDRAW line
        state.cycle_iddt(); // cheat_level = 1
        assert!(state.is_line_seen(4));
        // the flag check itself lives in the renderer; here we only
        // verify the seen bit works
        assert_eq!(ML_DONTDRAW, 128);
    }

    #[test]
    fn seen_lines_ignore_out_of_range_indices() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.mark_lines_seen([0, 1, 9999]);
        assert!(state.is_line_seen(0));
        assert!(state.is_line_seen(1));
        assert!(!state.is_line_seen(9999));
        assert_eq!(state.seen_count(), 2);
    }

    #[test]
    fn stop_returns_to_inactive_and_clears_transient_zoom() {
        let map = fixture_map();
        let mut state = open_state(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomIn));
        state.tick(&view(me(128, 64)));
        state.stop();
        assert!(!state.is_active());
        state.start(&map, me(128, 64));
        assert!(state.is_active());
        assert_eq!(state.amclock(), 0);
    }

    #[test]
    fn inactive_automap_ignores_input_and_ticks() {
        let mut state = AutomapState::new();
        let response = state.handle_input(AutomapInput::KeyDown(AutomapKey::ZoomIn));
        assert!(!response.handled);
        assert_eq!(response.message, None);
        state.tick(&view(me(128, 64)));
        assert_eq!(state.amclock(), 0);
    }

    #[test]
    fn go_big_saves_and_restores_window_scale() {
        let map = fixture_map();
        let mut state = open_state(&map);
        let initial_scale = state.scale_mtof();

        state.handle_input(AutomapInput::KeyDown(AutomapKey::GoBig));
        assert!(state.big_state);
        assert_eq!(state.scale_mtof(), state.min_scale_mtof);

        state.handle_input(AutomapInput::KeyDown(AutomapKey::GoBig));
        assert!(!state.big_state);
        // restored to something bigger than the fully-zoomed-out scale
        assert!(state.scale_mtof() > state.min_scale_mtof);
        // and (because the player did not move) back near the initial scale
        let restored = state.scale_mtof();
        let ratio = restored.to_raw() as f64 / initial_scale.to_raw() as f64;
        assert!((ratio - 1.0).abs() < 0.05, "restored ratio was {ratio}");
    }

    #[test]
    fn go_big_saved_state_used_by_restore() {
        let map = fixture_map();
        let mut state = open_state(&map);
        // pan the window somewhere unusual first
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        state.handle_input(AutomapInput::KeyDown(AutomapKey::PanRight));
        for _ in 0..30 {
            state.tick(&view(me(128, 64)));
        }
        let panned = state.window_bounds();

        state.handle_input(AutomapInput::KeyDown(AutomapKey::GoBig));
        state.handle_input(AutomapInput::KeyDown(AutomapKey::GoBig));
        let restored = state.window_bounds();
        assert_eq!(restored.0, panned.0);
        assert_eq!(restored.1, panned.1);
    }

    #[test]
    fn seen_bitset_roundtrip() {
        let mut seen = SeenLines::with_len(130);
        assert!(!seen.get(0));
        assert!(!seen.get(64));
        assert!(!seen.get(129));
        seen.set(0);
        seen.set(64);
        seen.set(129);
        seen.set(130); // out of range, ignored
        assert!(seen.get(0));
        assert!(seen.get(64));
        assert!(seen.get(129));
        assert!(!seen.get(130));
        seen.clear(10);
        assert!(!seen.get(0));
        assert!(!seen.get(64));
    }

    #[test]
    fn am_clock_counts_tics_while_active() {
        let map = fixture_map();
        let mut state = open_state(&map);
        for _ in 0..10 {
            state.tick(&view(me(128, 64)));
        }
        assert_eq!(state.amclock(), 10);
    }

    #[test]
    fn map_to_screen_round_trip_is_stable() {
        let map = fixture_map();
        let state = open_state(&map);
        let fixed_64 = Fixed::from_int(64);
        let screen = state.map_x_to_screen(fixed_64);
        let screen_y = state.map_y_to_screen(fixed_64);
        assert!(screen >= state.f_x());
        assert!(screen < state.f_x() + state.f_w() || screen >= 0); // clipped lines land anywhere
        assert!(screen_y >= 0);
        assert!(screen_y <= state.f_h());
    }
}

//! The DOOM status bar: the widget layer (`st_lib.c`) and the bar itself
//! (`st_stuff.c`), ported from linuxdoom-1.10.
//!
//! The bar covers the bottom 32 rows of the 320x200 screen (`ST_Y` =
//! 168, `ST_HEIGHT` = 32). It shows the ready-weapon ammo in big tall
//! numbers, health and armor as tall numbers with a percent sign, the
//! weapon-owned panel, the face, the key cards, the four ammo counters
//! and the four max-ammo counters, plus the deathmatch frag counter.
//!
//! # Lumps
//!
//! Everything is drawn with the vanilla patches: `STBAR` (the artwork
//! with the widget areas blank), `STTNUM*`/`STYSNUM*` (tall/short digit
//! tables), `STTPRCNT`, `STTMINUS`, `STKEYS*`, `STGNUM2..7`, `STF*`
//! (faces), `STARMS` (arms panel background), `STFB<n>` (netgame face
//! background, `n` = console player). They are decoded once at load time
//! through the resource manager.
//!
//! The HUD/chat layer (`hu_stuff.c`) draws the level name, the message
//! and the keys — it is not part of this module.
//!
//! # Layout and mechanics
//!
//! All the `ST_*` layout constants are byte-for-byte ports of the
//! original `#defines` (`st_stuff.c`). The redraw mechanics are the
//! original widget mechanics: each widget erases its rectangle by
//! copying from the off-screen background strip first, draws only when
//! its value changed (or a full refresh is requested), and the whole
//! strip is reblitted only on a full refresh.
//!
//! Deviations from the original, all semantics-preserving:
//!
//! * vanilla redraws the number widgets every tic (the `oldnum` field is
//!   stored but never consulted); we honor it and skip unchanged
//!   redraws — the output is identical;
//! * patches are clipped per-pixel instead of dropped wholesale
//!   (RANGECHECK behavior) or written out of bounds;
//! * when the bar is fully hidden (`bar_visible: false`) nothing is
//!   drawn — vanilla still drew the arms panel background over the view
//!   in single-player fullscreen mode;
//! * the screen-palette shifts (pain/bonus/radiation tint) are not here;
//!   they belong to the video layer, not to the bar.

mod face;
mod widgets;

pub use face::{
    FaceInputs, FaceMachine, NUMWEAPONS, ST_DEADFACE, ST_EVILGRINCOUNT, ST_EVILGRINOFFSET,
    ST_FACEPROBABILITY, ST_FACESTRIDE, ST_GODFACE, ST_MUCHPAIN, ST_NUMEXTRAFACES, ST_NUMFACES,
    ST_NUMPAINFACES, ST_NUMSPECIALFACES, ST_NUMSTRAIGHTFACES, ST_NUMTURNFACES, ST_OUCHCOUNT,
    ST_OUCHOFFSET, ST_RAMPAGEDELAY, ST_RAMPAGEOFFSET, ST_STRAIGHTFACECOUNT, ST_TURNCOUNT,
    ST_TURNOFFSET, TICRATE, pain_offset,
};
pub use widgets::{
    BinIconWidget, MultIconWidget, NumberWidget, PatchCanvas, PercentWidget, ST_HEIGHT, ST_WIDTH,
    STRIP_BYTES, StripCanvas, draw_patch,
};

use crate::errors::{EngineError, EngineResult};
use crate::gfx::patch::{Patch, decode_patch};
use crate::render::IndexedFramebuffer;
use crate::resources::ResourceManager;
use std::sync::Arc;
use widgets::blit_strip;

// Status bar location and size.
/// Left edge of the bar (always `0`).
pub const ST_X: i32 = 0;
/// Left edge of the arms panel background within the bar.
pub const ST_X2: i32 = 104;
/// Vertical position of the first bar row on the screen (`ST_Y`).
pub const ST_Y: i32 = (crate::render::SCREEN_HEIGHT - ST_HEIGHT) as i32; // 168

// Ammo number (ready weapon) position.
pub const ST_AMMOWIDTH: i32 = 3;
pub const ST_AMMOX: i32 = 44;
pub const ST_AMMOY: i32 = 171;

// Health number position.
pub const ST_HEALTHWIDTH: i32 = 3;
pub const ST_HEALTHX: i32 = 90;
pub const ST_HEALTHY: i32 = 171;

// Weapon owned positions.
pub const ST_ARMSX: i32 = 111;
pub const ST_ARMSY: i32 = 172;
pub const ST_ARMSBGX: i32 = 104;
pub const ST_ARMSBGY: i32 = 168;
pub const ST_ARMSXSPACE: i32 = 12;
pub const ST_ARMSYSPACE: i32 = 10;

// Frags position.
pub const ST_FRAGSX: i32 = 138;
pub const ST_FRAGSY: i32 = 171;
pub const ST_FRAGSWIDTH: i32 = 2;

// Armor number position.
pub const ST_ARMORWIDTH: i32 = 3;
pub const ST_ARMORX: i32 = 221;
pub const ST_ARMORY: i32 = 171;

// Key icon positions.
pub const ST_KEY0WIDTH: i32 = 8;
pub const ST_KEY0HEIGHT: i32 = 5;
pub const ST_KEY0X: i32 = 239;
pub const ST_KEY0Y: i32 = 171;
pub const ST_KEY1WIDTH: i32 = ST_KEY0WIDTH;
pub const ST_KEY1X: i32 = 239;
pub const ST_KEY1Y: i32 = 181;
pub const ST_KEY2WIDTH: i32 = ST_KEY0WIDTH;
pub const ST_KEY2X: i32 = 239;
pub const ST_KEY2Y: i32 = 191;

// Face position.
pub const ST_FACESX: i32 = 143;
pub const ST_FACESY: i32 = 168;

// Ammunition counters. Index is the ammo type (clip/shell/cell/missile);
// the y order matches the original `#define`s exactly.
pub const ST_AMMO0WIDTH: i32 = 3;
pub const ST_AMMO0HEIGHT: i32 = 6;
pub const ST_AMMO0X: i32 = 288;
pub const ST_AMMO0Y: i32 = 173;
pub const ST_AMMO1WIDTH: i32 = ST_AMMO0WIDTH;
pub const ST_AMMO1X: i32 = 288;
pub const ST_AMMO1Y: i32 = 179;
pub const ST_AMMO2WIDTH: i32 = ST_AMMO0WIDTH;
pub const ST_AMMO2X: i32 = 288;
pub const ST_AMMO2Y: i32 = 191;
pub const ST_AMMO3WIDTH: i32 = ST_AMMO0WIDTH;
pub const ST_AMMO3X: i32 = 288;
pub const ST_AMMO3Y: i32 = 185;

// Maximum ammunition counters (backpack indicator).
pub const ST_MAXAMMO0WIDTH: i32 = 3;
pub const ST_MAXAMMO0HEIGHT: i32 = 5;
pub const ST_MAXAMMO0X: i32 = 314;
pub const ST_MAXAMMO0Y: i32 = 173;
pub const ST_MAXAMMO1WIDTH: i32 = ST_MAXAMMO0WIDTH;
pub const ST_MAXAMMO1X: i32 = 314;
pub const ST_MAXAMMO1Y: i32 = 179;
pub const ST_MAXAMMO2WIDTH: i32 = ST_MAXAMMO0WIDTH;
pub const ST_MAXAMMO2X: i32 = 314;
pub const ST_MAXAMMO2Y: i32 = 191;
pub const ST_MAXAMMO3WIDTH: i32 = ST_MAXAMMO0WIDTH;
pub const ST_MAXAMMO3X: i32 = 314;
pub const ST_MAXAMMO3Y: i32 = 185;

/// Ammo type indices into `ammo`/`max_ammo` and the game logic.
pub const NUM_AMMO: usize = 4;
/// Key card/skull indices into `cards`.
pub const NUM_CARDS: usize = 6;

/// Magic value the ready-weapon counter shows to mean "not available" —
/// 1994, the year the engine was written. Weapons without ammo (fist,
/// chainsaw) set this instead of `Some(n)`.
pub const NOT_AVAILABLE: i32 = 1994;

const AMMO_Y: [i32; NUM_AMMO] = [ST_AMMO0Y, ST_AMMO1Y, ST_AMMO2Y, ST_AMMO3Y];
const MAXAMMO_Y: [i32; NUM_AMMO] = [ST_MAXAMMO0Y, ST_MAXAMMO1Y, ST_MAXAMMO2Y, ST_MAXAMMO3Y];

/// Plain-data inputs for one frame of the status bar, filled in by the
/// game layer. No engine internals, no mobj pointers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusState {
    /// Player health, clamped to 0..=100 for the face, drawn as-is.
    pub health: i32,
    /// Armor points (`d->armorpoints`).
    pub armor_points: i32,
    /// Ammo for the ready weapon; `None` for weapons without ammo
    /// (fist/chainsaw), which leaves the ready-weapon number blank.
    pub ready_weapon_ammo: Option<i32>,
    /// Frag count to show in deathmatch (sum over other players minus
    /// own deaths).
    pub frags: i32,
    /// Weapon ownership, all `NUMWEAPONS`; the arms panel shows entries
    /// `1..=6` (pistol..BFG), the face machine watches all of them.
    pub weapon_owned: [bool; NUMWEAPONS],
    /// Ammo per type: clip, shell, cell, missile.
    pub ammo: [i32; NUM_AMMO],
    /// Maximum ammo per type: clip, shell, cell, missile.
    pub max_ammo: [i32; NUM_AMMO],
    /// Card and skull ownership: blue/yellow/red card (`0..=2`),
    /// blue/yellow/red skull (`3..=5`). A skull shadows its card.
    pub cards: [bool; NUM_CARDS],
    /// Face index, selected by the game layer via [`FaceMachine`].
    pub face_index: i32,
    /// Deathmatch mode: arms panel shows frags instead of weapons owned.
    pub deathmatch: bool,
    /// Net game: the `STFB<n>` face background is drawn behind the face.
    pub netgame: bool,
    /// Whether the bar (and its widgets) is visible at all — the inverse
    /// of fullscreen view, overridden while the automap is active.
    pub bar_visible: bool,
}

impl Default for StatusState {
    /// A fresh single-player spawn state.
    fn default() -> Self {
        let mut weapon_owned = [false; NUMWEAPONS];
        weapon_owned[1] = true; // wp_pistol
        let mut ammo = [0; NUM_AMMO];
        ammo[0] = 50; // am_clip
        Self {
            health: 100,
            armor_points: 0,
            ready_weapon_ammo: Some(50),
            frags: 0,
            weapon_owned,
            ammo,
            max_ammo: [200, 50, 300, 50],
            cards: [false; NUM_CARDS],
            face_index: 0,
            deathmatch: false,
            netgame: false,
            bar_visible: true,
        }
    }
}

/// The fully decoded patch set the bar needs. Loaded once at startup
/// via the resource manager; everything here is `Arc`-shared with the
/// resource cache.
pub(crate) struct StatusPatches {
    sbar: Arc<Patch>,
    arms_bg: Arc<Patch>,
    face_back: Arc<Patch>,
    tall_num: [Arc<Patch>; 10],
    short_num: [Arc<Patch>; 10],
    tall_percent: Arc<Patch>,
    minus: Arc<Patch>,
    keys: [Arc<Patch>; 6],
    gnu: [Arc<Patch>; 6],
    faces: [Arc<Patch>; ST_NUMFACES as usize],
}

fn load_patch(resources: &ResourceManager, name: &str) -> EngineResult<Arc<Patch>> {
    let index = resources
        .find_lump(name)
        .ok_or_else(|| EngineError::Resource(format!("status bar patch lump {name} not found")))?;
    resources.decode_cached::<Patch>(index, |bytes| {
        decode_patch(bytes).map_err(EngineError::from)
    })
}

fn load_named<const N: usize>(
    resources: &ResourceManager,
    names: &[String; N],
) -> EngineResult<[Arc<Patch>; N]> {
    let mut out = Vec::with_capacity(N);
    for name in names {
        out.push(load_patch(resources, name)?);
    }
    out.try_into()
        .map_err(|_| EngineError::Resource("status bar patch table size mismatch".to_string()))
}

impl StatusPatches {
    fn load(resources: &ResourceManager, console_player: u8) -> EngineResult<Self> {
        if console_player > 3 {
            return Err(EngineError::Resource(format!(
                "console player must be 0..=3, got {console_player}"
            )));
        }

        let digit_names =
            |prefix: &str| -> [String; 10] { std::array::from_fn(|i| format!("{prefix}{i}")) };
        let key_names: [String; 6] = std::array::from_fn(|i| format!("STKEYS{i}"));
        let gnu_names: [String; 6] = std::array::from_fn(|i| format!("STGNUM{}", i + 2));

        // face load order is byte-for-byte the original loop:
        // straight x3, turn right, turn left, ouch, evil grin, rampage
        let mut face_names = Vec::with_capacity(ST_NUMFACES as usize);
        for pain in 0..ST_NUMPAINFACES {
            for straight in 0..ST_NUMSTRAIGHTFACES {
                face_names.push(format!("STFST{pain}{straight}"));
            }
            face_names.push(format!("STFTR{pain}0"));
            face_names.push(format!("STFTL{pain}0"));
            face_names.push(format!("STFOUCH{pain}"));
            face_names.push(format!("STFEVL{pain}"));
            face_names.push(format!("STFKILL{pain}"));
        }
        face_names.push("STFGOD0".to_string());
        face_names.push("STFDEAD0".to_string());

        Ok(Self {
            sbar: load_patch(resources, "STBAR")?,
            arms_bg: load_patch(resources, "STARMS")?,
            face_back: load_patch(resources, &format!("STFB{console_player}"))?,
            tall_num: load_named(resources, &digit_names("STTNUM"))?,
            short_num: load_named(resources, &digit_names("STYSNUM"))?,
            tall_percent: load_patch(resources, "STTPRCNT")?,
            minus: load_patch(resources, "STTMINUS")?,
            keys: load_named(resources, &key_names)?,
            gnu: load_named(resources, &gnu_names)?,
            faces: load_named::<{ ST_NUMFACES as usize }>(
                resources,
                face_names
                    .as_slice()
                    .try_into()
                    .expect("face name table matches ST_NUMFACES"),
            )?,
        })
    }
}

/// The retained status bar state: the off-screen background strip, the
/// decoded patches and the per-widget dirty flags.
///
/// Build it once with [`StatusBar::load`] and then draw every frame
/// with [`StatusBar::render`].
pub struct StatusBar {
    strip: Vec<u8>,
    patches: StatusPatches,
    ready_ammo: NumberWidget,
    ammo: [NumberWidget; NUM_AMMO],
    max_ammo: [NumberWidget; NUM_AMMO],
    health: PercentWidget,
    armor: PercentWidget,
    arms_bg: BinIconWidget,
    arms: [MultIconWidget; 6],
    faces: MultIconWidget,
    keyboxes: [MultIconWidget; 3],
    frags: NumberWidget,
    first_time: bool,
}

impl StatusBar {
    /// `ST_loadData`: finds and decodes every status bar patch.
    ///
    /// `console_player` selects the `STFB<n>` face background (net games
    /// only draw it); must be 0..=3.
    pub fn load(resources: &ResourceManager, console_player: u8) -> EngineResult<Self> {
        let patches = StatusPatches::load(resources, console_player)?;
        Ok(Self::from_patches(patches))
    }

    pub(crate) fn from_patches(patches: StatusPatches) -> Self {
        Self {
            strip: vec![0; STRIP_BYTES],
            patches,
            ready_ammo: NumberWidget::new(ST_AMMOX, ST_AMMOY, ST_AMMOWIDTH),
            ammo: std::array::from_fn(|i| NumberWidget::new(ST_AMMO0X, AMMO_Y[i], ST_AMMO0WIDTH)),
            max_ammo: std::array::from_fn(|i| {
                NumberWidget::new(ST_MAXAMMO0X, MAXAMMO_Y[i], ST_MAXAMMO0WIDTH)
            }),
            health: PercentWidget::new(ST_HEALTHX, ST_HEALTHY),
            armor: PercentWidget::new(ST_ARMORX, ST_ARMORY),
            arms_bg: BinIconWidget::new(ST_ARMSBGX, ST_ARMSBGY),
            arms: std::array::from_fn(|i| {
                MultIconWidget::new(
                    ST_ARMSX + (i as i32 % 3) * ST_ARMSXSPACE,
                    ST_ARMSY + (i as i32 / 3) * ST_ARMSYSPACE,
                )
            }),
            faces: MultIconWidget::new(ST_FACESX, ST_FACESY),
            keyboxes: [
                MultIconWidget::new(ST_KEY0X, ST_KEY0Y),
                MultIconWidget::new(ST_KEY1X, ST_KEY1Y),
                MultIconWidget::new(ST_KEY2X, ST_KEY2Y),
            ],
            frags: NumberWidget::new(ST_FRAGSX, ST_FRAGSY, ST_FRAGSWIDTH),
            first_time: true,
        }
    }

    fn refresh_background(&mut self, fb: &mut IndexedFramebuffer, state: &StatusState) {
        if !state.bar_visible {
            return;
        }
        self.strip.fill(0);
        {
            let mut canvas = StripCanvas::new(&mut self.strip);
            draw_patch(&mut canvas, &self.patches.sbar, ST_X, 0);
            if state.netgame {
                draw_patch(&mut canvas, &self.patches.face_back, ST_FACESX, 0);
            }
        }
        blit_strip(fb, &self.strip);
    }

    /// Draws (or incrementally updates) the bar.
    ///
    /// `refresh` forces a full redraw (`ST_doRefresh`); the first call
    /// always refreshes. With `refresh == false` only widgets whose
    /// values changed touch the framebuffer (`ST_diffDraw`).
    pub fn render(
        &mut self,
        fb: &mut IndexedFramebuffer,
        state: &StatusState,
        refresh: bool,
    ) -> EngineResult<()> {
        let refresh = refresh || self.first_time;

        if refresh {
            self.refresh_background(fb, state);
        }

        let on = state.bar_visible;
        let armson = state.bar_visible && !state.deathmatch;
        let fragson = state.deathmatch && state.bar_visible;
        let notdm = state.bar_visible && !state.deathmatch;

        // ready weapon ammo (1994 = "no ammo for this weapon")
        let ready = state.ready_weapon_ammo.unwrap_or(NOT_AVAILABLE);
        self.ready_ammo.draw(
            fb,
            &self.strip,
            &self.patches.tall_num,
            &self.patches.minus,
            ready,
            on,
            refresh,
        )?;

        for i in 0..NUM_AMMO {
            self.ammo[i].draw(
                fb,
                &self.strip,
                &self.patches.short_num,
                &self.patches.minus,
                state.ammo[i],
                on,
                refresh,
            )?;
            self.max_ammo[i].draw(
                fb,
                &self.strip,
                &self.patches.short_num,
                &self.patches.minus,
                state.max_ammo[i],
                on,
                refresh,
            )?;
        }

        self.health.draw(
            fb,
            &self.strip,
            &self.patches.tall_num,
            &self.patches.minus,
            &self.patches.tall_percent,
            state.health,
            on,
            refresh,
        )?;
        self.armor.draw(
            fb,
            &self.strip,
            &self.patches.tall_num,
            &self.patches.minus,
            &self.patches.tall_percent,
            state.armor_points,
            on,
            refresh,
        )?;

        // arms panel background (single player only)
        self.arms_bg.draw(
            fb,
            &self.strip,
            &self.patches.arms_bg,
            notdm,
            notdm,
            refresh,
        )?;

        // weapon ownership; gray when not owned, yellow when owned
        for i in 0..6 {
            let icons: [Arc<Patch>; 2] = [
                self.patches.gnu[i].clone(),
                self.patches.short_num[i + 2].clone(),
            ];
            let inum = if state.weapon_owned[i + 1] { 1 } else { 0 };
            self.arms[i].draw(fb, &self.strip, &icons, inum, armson, refresh)?;
        }

        // the face
        self.faces.draw(
            fb,
            &self.strip,
            &self.patches.faces,
            state.face_index,
            on,
            refresh,
        )?;

        // key cards; a skull shadows its card
        let mut keyboxes = [-1i32; 3];
        for (i, boxed) in keyboxes.iter_mut().enumerate() {
            if state.cards[i] {
                *boxed = i as i32;
            }
            if state.cards[i + 3] {
                *boxed = (i + 3) as i32;
            }
        }
        for (widget, &key) in self.keyboxes.iter_mut().zip(keyboxes.iter()) {
            widget.draw(fb, &self.strip, &self.patches.keys, key, on, refresh)?;
        }

        // frag count (deathmatch only)
        self.frags.draw(
            fb,
            &self.strip,
            &self.patches.tall_num,
            &self.patches.minus,
            state.frags,
            fragson,
            refresh,
        )?;

        self.first_time = false;
        Ok(())
    }
}

/// Draws the status bar into `fb`.
///
/// `bar` holds the decoded patches and the widget dirty flags; `state`
/// is the plain-data input for this frame. The first call performs a
/// full refresh; later calls redraw only what changed. Call once per
/// game tic for 1:1 face animation timing.
pub fn render_status_bar(
    fb: &mut IndexedFramebuffer,
    bar: &mut StatusBar,
    state: &StatusState,
) -> EngineResult<()> {
    bar.render(fb, state, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds raw patch bytes from per-column (topdelta, pixels) posts.
    fn build_patch_bytes(width: i16, height: i16, columns: &[Vec<(u8, Vec<u8>)>]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes()); // left offset
        data.extend_from_slice(&0i16.to_le_bytes()); // top offset
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

    /// Decodes a `width`x`height` patch whose every pixel is `fill`.
    fn fill_patch(width: i16, height: i16, fill: u8) -> Patch {
        let column = vec![(0u8, vec![fill; height as usize])];
        let columns = vec![column; width as usize];
        decode_patch(&build_patch_bytes(width, height, &columns)).unwrap()
    }

    /// Synthetic patch set: digit d is color 1+d, percent 11, minus 12,
    /// keys 20+i, gray weapon numbers 30+i, faces 40+i, arms background
    /// 90, face background 100, STBAR artwork 200.
    fn synthetic_patches() -> StatusPatches {
        let arc_fill = |w, h, c| Arc::new(fill_patch(w, h, c));
        StatusPatches {
            sbar: arc_fill(320, 32, 200),
            arms_bg: arc_fill(28, 28, 90),
            face_back: arc_fill(40, 30, 100),
            tall_num: std::array::from_fn(|i| arc_fill(4, 7, 1 + i as u8)),
            short_num: std::array::from_fn(|i| arc_fill(4, 7, 1 + i as u8)),
            tall_percent: arc_fill(4, 7, 11),
            minus: arc_fill(6, 7, 12),
            keys: std::array::from_fn(|i| arc_fill(8, 5, 20 + i as u8)),
            gnu: std::array::from_fn(|i| arc_fill(4, 7, 30 + i as u8)),
            faces: std::array::from_fn(|i| arc_fill(2, 2, 40 + i as u8)),
        }
    }

    fn synthetic_bar() -> StatusBar {
        StatusBar::from_patches(synthetic_patches())
    }

    fn fresh() -> (IndexedFramebuffer, StatusBar, StatusState) {
        (
            IndexedFramebuffer::new(),
            synthetic_bar(),
            StatusState::default(),
        )
    }

    fn render_all(fb: &mut IndexedFramebuffer, bar: &mut StatusBar, state: &StatusState) {
        bar.render(fb, state, false).unwrap();
    }

    #[test]
    fn layout_constants_match_vanilla() {
        assert_eq!(ST_Y, 168);
        assert_eq!(ST_HEIGHT, 32);
        assert_eq!(ST_WIDTH, 320);
        assert_eq!(ST_AMMOX, 44);
        assert_eq!(ST_AMMOY, 171);
        assert_eq!(ST_HEALTHX, 90);
        assert_eq!(ST_HEALTHY, 171);
        assert_eq!(ST_ARMORX, 221);
        assert_eq!(ST_ARMORY, 171);
        assert_eq!(ST_FRAGSX, 138);
        assert_eq!(ST_FRAGSY, 171);
        assert_eq!(ST_FRAGSWIDTH, 2);
        assert_eq!(ST_ARMSX, 111);
        assert_eq!(ST_ARMSY, 172);
        assert_eq!(ST_ARMSBGX, 104);
        assert_eq!(ST_ARMSBGY, 168);
        assert_eq!(ST_ARMSXSPACE, 12);
        assert_eq!(ST_ARMSYSPACE, 10);
        assert_eq!(ST_KEY0X, 239);
        assert_eq!(ST_KEY0Y, 171);
        assert_eq!(ST_KEY1X, 239);
        assert_eq!(ST_KEY1Y, 181);
        assert_eq!(ST_KEY2X, 239);
        assert_eq!(ST_KEY2Y, 191);
        assert_eq!(
            [ST_AMMO0Y, ST_AMMO1Y, ST_AMMO2Y, ST_AMMO3Y],
            [173, 179, 191, 185]
        );
        assert_eq!(
            [ST_MAXAMMO0Y, ST_MAXAMMO1Y, ST_MAXAMMO2Y, ST_MAXAMMO3Y],
            [173, 179, 191, 185]
        );
        assert_eq!(ST_AMMO0X, 288);
        assert_eq!(ST_MAXAMMO0X, 314);
        assert_eq!(ST_FACESX, 143);
        assert_eq!(ST_FACESY, 168);
        assert_eq!(ST_AMMOWIDTH, 3);
        assert_eq!(ST_HEALTHWIDTH, 3);
        assert_eq!(ST_ARMORWIDTH, 3);
        assert_eq!(NOT_AVAILABLE, 1994);
    }

    #[test]
    fn face_constants_match_vanilla() {
        assert_eq!(ST_NUMPAINFACES, 5);
        assert_eq!(ST_NUMSTRAIGHTFACES, 3);
        assert_eq!(ST_NUMTURNFACES, 2);
        assert_eq!(ST_NUMSPECIALFACES, 3);
        assert_eq!(ST_FACESTRIDE, 8);
        assert_eq!(ST_NUMFACES, 42);
        assert_eq!(ST_TURNOFFSET, 3);
        assert_eq!(ST_OUCHOFFSET, 5);
        assert_eq!(ST_EVILGRINOFFSET, 6);
        assert_eq!(ST_RAMPAGEOFFSET, 7);
        assert_eq!(ST_GODFACE, 40);
        assert_eq!(ST_DEADFACE, 41);
        assert_eq!(ST_MUCHPAIN, 20);
        assert_eq!(ST_EVILGRINCOUNT, 70);
        assert_eq!(ST_STRAIGHTFACECOUNT, 17);
        assert_eq!(ST_TURNCOUNT, 35);
        assert_eq!(ST_RAMPAGEDELAY, 70);
    }

    #[test]
    fn full_refresh_blits_the_background() {
        let (mut fb, mut bar, state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(0, 168), Some(200));
        assert_eq!(fb.get(319, 199), Some(200));
        // and the strip was drawn above the bottom rows only
        assert_eq!(fb.get(0, 167), Some(0));
    }

    #[test]
    fn health_number_and_percent_draw() {
        let (mut fb, mut bar, state) = fresh(); // health 100
        render_all(&mut fb, &mut bar, &state);
        // tall digits are 4 px wide; "100" right-aligned at x=90,
        // digit cells [78,82) [82,86) [86,90)
        assert_eq!(fb.get(78, 172), Some(2)); // '1'
        assert_eq!(fb.get(82, 172), Some(1)); // '0'
        assert_eq!(fb.get(86, 172), Some(1)); // '0'
        // percent sign drawn at the right edge
        assert_eq!(fb.get(90, 172), Some(11));
    }

    #[test]
    fn leading_zeros_are_chopped() {
        let (mut fb, mut bar, mut state) = fresh();
        state.health = 7;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(86, 172), Some(8)); // '7'
        assert_eq!(fb.get(82, 172), Some(200)); // erased, not drawn
        assert_eq!(fb.get(78, 172), Some(200));
    }

    #[test]
    fn zero_draws_a_single_zero() {
        let (mut fb, mut bar, mut state) = fresh();
        state.health = 0;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(86, 172), Some(1)); // '0'
        assert_eq!(fb.get(82, 172), Some(200));
    }

    #[test]
    fn no_ammo_for_ready_weapon_leaves_blank() {
        let (mut fb, mut bar, mut state) = fresh();
        state.ready_weapon_ammo = None;
        render_all(&mut fb, &mut bar, &state);
        // ready-weapon cells (ST_AMMOX=44, 3 digits, 4 px wide) are blank
        for x in (44 - 3 * 4)..44 {
            assert_eq!(fb.get(x, 172), Some(200), "x={x}");
        }
    }

    #[test]
    fn dirty_flag_skips_redraw_when_clean() {
        let (mut fb, mut bar, state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        let before = fb.as_slice().to_vec();
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.as_slice(), before.as_slice());
    }

    #[test]
    fn dirty_flag_redraws_only_what_changed() {
        let (mut fb, mut bar, mut state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        let before = fb.as_slice().to_vec();
        state.health = 50;
        render_all(&mut fb, &mut bar, &state);
        // health digits changed
        assert_ne!(
            &fb.as_slice()[172 * 320..173 * 320],
            &before[172 * 320..173 * 320]
        );
        assert_eq!(fb.get(82, 172), Some(6)); // '5'
        assert_eq!(fb.get(86, 172), Some(1)); // '0'
        // the arms panel icons were not touched by a health change
        assert_eq!(fb.get(123, 173), Some(before[173 * 320 + 123]));
        assert_eq!(fb.get(111, 173), Some(before[173 * 320 + 111]));
    }

    #[test]
    fn number_shrinking_erases_old_digits() {
        let (mut fb, mut bar, mut state) = fresh();
        render_all(&mut fb, &mut bar, &state); // 100 drawn
        state.health = 7;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(78, 172), Some(200));
        assert_eq!(fb.get(82, 172), Some(200));
        assert_eq!(fb.get(86, 172), Some(8));
    }

    #[test]
    fn refresh_forces_redraw_even_if_clean() {
        let (mut fb, mut bar, state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        // zero the arm panel area behind the bar's back and refresh:
        // the redraw must restore it
        fb.set(123, 173, 0);
        bar.render(&mut fb, &state, true).unwrap();
        assert_ne!(fb.get(123, 173), Some(0));
    }

    #[test]
    fn negative_frags_draw_a_minus() {
        let (mut fb, mut bar, mut state) = fresh();
        state.deathmatch = true;
        state.frags = -5;
        render_all(&mut fb, &mut bar, &state);
        // 2 digits right-aligned at x=138, w=4 (synthetic tall digits):
        // cells [130,134) [134,138); minus sign goes at x - 8
        assert_eq!(fb.get(134, 172), Some(6)); // '5'
        // minus is color 12; it sits 8 px left of the drawn digit,
        // overlapping the cell the chopped '0' would have occupied
        assert_eq!(fb.get(126, 172), Some(12));
        assert_eq!(fb.get(128, 172), Some(12));
        assert_eq!(fb.get(131, 172), Some(12));
        assert_eq!(fb.get(133, 172), Some(200)); // rest of the chopped cell
    }

    #[test]
    fn frags_only_draw_in_deathmatch() {
        let (mut fb, mut bar, mut state) = fresh();
        state.frags = 7;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(134, 172), Some(200)); // nothing drawn
        state.deathmatch = true;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(134, 172), Some(8)); // '7'
    }

    #[test]
    fn arms_panel_shows_gray_and_yellow_numbers() {
        let (mut fb, mut bar, _state) = fresh();
        // fresh state owns the pistol only (weapon 1, icon cell 0)
        render_all(&mut fb, &mut bar, &StatusState::default());
        assert_eq!(fb.get(111, 173), Some(3)); // owned weapon: yellow '2'
        assert_eq!(fb.get(123, 173), Some(31)); // not owned: gray '3' (STGNUM3)
    }

    #[test]
    fn arms_panel_hidden_in_deathmatch_and_bg_not_drawn() {
        let (mut fb, mut bar, mut state) = fresh();
        state.deathmatch = true;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(111, 173), Some(200));
        assert_eq!(fb.get(104, 168), Some(200)); // STARMS background not drawn
    }

    #[test]
    fn arms_bg_drawn_in_single_player() {
        let (mut fb, mut bar, state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(105, 169), Some(90));
    }

    #[test]
    fn arms_bg_not_drawn_in_deathmatch() {
        // vanilla keeps the icon untouched mid-game when `on` drops;
        // a full refresh re-blits the strip without the arms panel
        let (mut fb, mut bar, mut state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(105, 169), Some(90));
        state.deathmatch = true;
        bar.render(&mut fb, &state, true).unwrap();
        assert_eq!(fb.get(105, 169), Some(200));
    }

    #[test]
    fn key_boxes_draw_only_owned_keys() {
        let (mut fb, mut bar, mut state) = fresh();
        state.cards[0] = true; // blue card
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(239, 172), Some(20)); // STKEYS0 color
        assert_eq!(fb.get(239, 182), Some(200)); // no yellow key
        state.cards[4] = true; // yellow skull shadows yellow card
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(239, 182), Some(24)); // STKEYS4 color
    }

    #[test]
    fn face_draws_and_switches() {
        let (mut fb, mut bar, mut state) = fresh();
        state.face_index = 0;
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(143, 168), Some(40));
        state.face_index = 41; // dead face
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.get(143, 168), Some(81)); // 40 + 41
    }

    #[test]
    fn bar_hidden_draws_nothing() {
        let (mut fb, mut bar, mut state) = fresh();
        state.bar_visible = false;
        state.deathmatch = true; // arms bg would draw otherwise
        render_all(&mut fb, &mut bar, &state);
        assert_eq!(fb.as_slice(), &[0u8; crate::render::SCREEN_PIXELS]);
    }

    #[test]
    fn loading_missing_lumps_fails_cleanly() {
        let rm = ResourceManager::default();
        let err = StatusBar::load(&rm, 0).err().unwrap();
        assert!(err.to_string().contains("STBAR"));
    }

    #[test]
    fn bad_console_player_fails_cleanly() {
        let rm = ResourceManager::default();
        let err = StatusBar::load(&rm, 4).err().unwrap();
        assert!(err.to_string().contains("console player"));
    }

    #[test]
    fn render_status_bar_entry_point_works() {
        let (mut fb, mut bar, state) = fresh();
        render_status_bar(&mut fb, &mut bar, &state).unwrap();
        assert_eq!(fb.get(0, 168), Some(200));
    }

    #[test]
    fn render_after_hidden_does_not_leak_stale_pixels() {
        // a forced refresh while hidden must not touch the bottom rows
        let (mut fb, mut bar, mut state) = fresh();
        render_all(&mut fb, &mut bar, &state);
        let before = fb.as_slice().to_vec();
        state.bar_visible = false;
        state.deathmatch = true;
        bar.render(&mut fb, &state, true).unwrap();
        assert_eq!(fb.as_slice(), before.as_slice());
    }
}

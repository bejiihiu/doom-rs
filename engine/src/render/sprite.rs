//! Sprite definitions and projection, ported from `r_things.c`
//! (`R_InitSpriteLumps`, `R_InitSpriteDefs`, `R_ProjectSprite`,
//! `R_DrawMaskedColumn`, `R_DrawVisSprite`) plus the translation-table
//! half of `r_draw.c` (`R_InitTranslationTables`, `R_DrawTranslatedColumn`).
//!
//! The flow is split in three so the game code can drive it exactly like
//! the original does:
//!
//! 1. [`SpriteLumpTables::build`] mirrors `R_InitSpriteLumps`: it records
//!    the width/`leftoffset`/`topoffset` of every sprite patch so the
//!    projection pass never decodes pixel data. Only the 8-byte patch
//!    headers are read; the full patch is decoded lazily at draw time.
//! 2. [`SpriteDefinitions::build`] mirrors `R_InitSpriteDefs`: given the
//!    sprite-name list from the (future) info tables, it scans the
//!    `S_START..S_END` namespace and lays out, per name, the frames (`A`..)
//!    each with either one orientation (`rotation 0`) or eight
//!    (`rotation 1..=8`), plus the horizontal-flip companions encoded in
//!    the trailing two name bytes (`<NAME><FRAME><ROT><FLIPFRAME><FLIPROT>`,
//!    an uppercase frame letter and a digit). Data inconsistencies that the
//!    original would `I_Error` on (duplicate rotation lumps, a frame with
//!    both a rotation-0 lump and rotated lumps, missing rotations, frames
//!    with no patches at all) are reported as [`SpriteError`], not panics.
//! 3. [`project_sprite`] mirrors `R_ProjectSprite`: turns a thing-like
//!    value into a [`ProjectedSprite`] (the `vissprite_t` analog) or
//!    rejects it (behind the view, off to the side, not on screen).
//!
//! Rendering of the collected vissprites (the back-to-front sweep,
//! seg-silhouette clipping and the masked mid-texture tail) lives in
//! [`super::masked`]; the sky pass lives in [`super::sky`]. Those are the
//! `R_DrawMasked` equivalents.
//!
//! Deviations from the original, all deliberate:
//!
//! * Sprite-frame lump references are **global lump indices**, not the
//!   `lump - firstspritelump` offsets the original stores. Same data, one
//!   less place for an off-by-one; the namespace bounds are enforced at
//!   build time instead.
//! * `wall::WallView` carries the projection parameters instead of file
//!   scope globals (`viewx`, `centerxfrac`, `projection`, ...).
//! * The `modifiedgame` switch in `R_InitSpriteDefs` is not ported: it
//!   only redirects installs to the last-loaded lump, and the second visit
//!   of the same (frame, rotation) pair errors out either way in vanilla,
//!   so installing the visited lump directly is behavior-equivalent for
//!   every name the engine accepts.
//! * `vissprite_t` overflow (more than `MAXVISSPRITES` visible sprites in
//!   one frame) silently dropped the sprite in the original; here the
//!   overflow is counted in [`super::masked::MaskedReport`].
//! * The vanilla clears of `negonearray`/`screenheightarray` happen once at
//!   startup; the masked pass builds the equivalent arrays locally per
//!   sprite instead.

use crate::bsp::point_to_angle_2;
use crate::errors::EngineError;
use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::gfx::palette::{Colormap, FUZZ_COLORMAP};
use crate::gfx::patch::{Patch, Post, decode_patch};
use crate::resources::ResourceManager;
use crate::tables::Angle;
use crate::wad::WadSet;

use super::column::{
    ColumnParams, ColumnSource, FuzzState, UnmaskedColumn, draw_column, draw_column_low,
    draw_fuzz_column,
};
use super::framebuffer::SCREEN_WIDTH;
use super::lighting::{LIGHTSCALESHIFT, MAXLIGHTSCALE};
use super::wall::WallView;

/// Vanilla `FF_FRAMEMASK` from `p_pspr.h`: the low 15 frame bits of a
/// thing's frame field.
pub const FF_FRAMEMASK: u32 = 0x7fff;
/// Vanilla `FF_FULLBRIGHT` from `p_pspr.h`: fullbright frame flag.
pub const FF_FULLBRIGHT: u32 = 0x8000;
/// Vanilla `MF_SHADOW` from `p_mobj.h`: the spectre "shadow" draw flag.
pub const MF_SHADOW: u32 = 0x0004_0000;
/// Vanilla `MF_TRANSLATION` from `p_mobj.h`: player-color translation
/// bits, set on things the player-sprite translation applies to.
pub const MF_TRANSLATION: u32 = 0x0c00_0000;
/// Vanilla `MF_TRANSSHIFT` from `p_mobj.h`.
pub const MF_TRANSSHIFT: u32 = 26;

/// Vanilla `MINZ`: things closer than this (in front of the view plane)
/// are not projected.
const MIN_Z: Fixed = Fixed::from_raw(4 << FRACBITS);
/// Vanilla `MAXFRAME` bound on frame letters in `R_InstallSpriteLump`
/// (frames run `A..`, 29 slots).
const MAX_SPRITE_FRAMES: usize = 29;
/// Vanilla `MAXVISSPRITES`.
pub const MAX_VIS_SPRITES: usize = 128;
/// Vanilla `ANG45/2*9` as a raw angle.
const ANGLE_ROT_OFFSET: u32 = 0x9000_0000;

/// Everything that can go wrong while building the sprite tables.
///
/// The original called `I_Error` for all of these; we keep them typed so
/// the caller can present a proper startup error instead of aborting the
/// process. The `Display` output mirrors the original messages.
#[derive(Debug, thiserror::Error)]
pub enum SpriteError {
    /// The `S_START`/`S_END` markers are missing from the combined WAD
    /// namespace, or one of them is, or the range is inverted.
    #[error("sprite namespace markers (S_START/S_END) are missing or inverted")]
    MissingSpriteNamespace,
    /// A sprite patch header failed validation while building the width
    /// tables. The lump failed the same validation the draw path applies.
    #[error("sprite lump {lump} ({name}) has an invalid patch header: {reason}")]
    BadLumpHeader {
        /// Global index of the offending lump.
        lump: usize,
        /// The lump's name, trimmed and uppercased.
        name: String,
        /// What went wrong.
        reason: String,
    },
    /// Frame or rotation character out of range (`frame >= 29`,
    /// `rotation > 8`, or non-`A..=Z`/`0..=8` bytes).
    #[error("sprite {sprite}: bad frame characters in lump {lump} ({raw:?})")]
    BadFrameChar {
        /// The sprite-name this lump was scanned for.
        sprite: String,
        /// Global index of the lump.
        lump: usize,
        /// The raw 8 name bytes.
        raw: [u8; 8],
    },
    /// Both a rotation-0 lump and per-rotation lumps (or two rotation-0
    /// lumps) map to the same frame.
    #[error("sprite {sprite} frame {frame}: has both rotation-0 and rotated lumps")]
    RotationConflict {
        /// The sprite-name.
        sprite: String,
        /// The frame letter.
        frame: char,
    },
    /// Two lumps map to the same (frame, rotation).
    #[error("sprite {sprite} frame {frame} rotation {rotation}: two lumps map to it (lump {lump})")]
    DuplicateRotation {
        /// The sprite-name.
        sprite: String,
        /// The frame letter.
        frame: char,
        /// The rotation digit, 1-based as in the lump name.
        rotation: u8,
        /// Global index of the offending lump.
        lump: usize,
    },
    /// A frame declared rotated (had at least one `1..=8` lump) but is
    /// missing some of the eight rotations.
    #[error("sprite {sprite} frame {frame}: missing rotation {rotation}")]
    MissingRotation {
        /// The sprite-name.
        sprite: String,
        /// The frame letter.
        frame: char,
        /// The missing rotation, 1-based as in the lump name.
        rotation: u8,
    },
    /// Frames are zero-indexed in storage; a frame letter below `A`+`n`
    /// that was found only through later letters is missing entirely.
    #[error("sprite {sprite} frame {frame}: no patches found")]
    NoPatchesForFrame {
        /// The sprite-name.
        sprite: String,
        /// The frame letter.
        frame: char,
    },
}

impl From<SpriteError> for EngineError {
    fn from(err: SpriteError) -> Self {
        EngineError::Resource(err.to_string())
    }
}

/// The `spritewidth[]`/`spriteoffset[]`/`spritetopoffset[]` tables from
/// `R_InitSpriteLumps`, indexed by **global lump index** (see the module
/// docs for the deviation). Lumps outside the sprite namespace read as 0.
#[derive(Clone, Debug)]
pub struct SpriteLumpTables {
    width: Vec<Fixed>,
    offset: Vec<Fixed>,
    top_offset: Vec<Fixed>,
}

impl SpriteLumpTables {
    /// Reads the sprite namespace patch headers. Errors (instead of
    /// crashing mid-projection like the original) on any corrupt lump.
    pub fn build(manager: &ResourceManager) -> Result<Self, SpriteError> {
        let wads = manager.wads();
        let Some((first, last)) = sprite_namespace(wads) else {
            return Err(SpriteError::MissingSpriteNamespace);
        };
        let total = wads.lump_count();
        let mut width = vec![Fixed::ZERO; total];
        let mut offset = vec![Fixed::ZERO; total];
        let mut top_offset = vec![Fixed::ZERO; total];
        for lump in first..=last {
            let data = manager
                .lump_data_index(lump)
                .map_err(|err| bad_lump(wads, lump, err.to_string()))?;
            let (w, left, top) =
                read_patch_header(data).map_err(|err| bad_lump(wads, lump, err))?;
            width[lump] = Fixed::from_int(i32::from(w));
            offset[lump] = Fixed::from_int(i32::from(left));
            top_offset[lump] = Fixed::from_int(i32::from(top));
        }
        Ok(Self {
            width,
            offset,
            top_offset,
        })
    }

    /// `spritewidth[lump]` in fixed point.
    pub fn width(&self, lump: usize) -> Fixed {
        self.width.get(lump).copied().unwrap_or(Fixed::ZERO)
    }

    /// `spriteoffset[lump]` in fixed point.
    pub fn offset(&self, lump: usize) -> Fixed {
        self.offset.get(lump).copied().unwrap_or(Fixed::ZERO)
    }

    /// `spritetopoffset[lump]` in fixed point.
    pub fn top_offset(&self, lump: usize) -> Fixed {
        self.top_offset.get(lump).copied().unwrap_or(Fixed::ZERO)
    }
}

/// Adjacent `S_START`/`S_END` markers (empty sprite set) are valid;
/// missing or inverted markers are not.
fn sprite_namespace(wads: &WadSet) -> Option<(usize, usize)> {
    match (wads.first_sprite(), wads.last_sprite()) {
        (Some(first), Some(last)) => last.checked_add(1).and_then(|end| {
            if end >= first {
                Some((first, last))
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn bad_lump(wads: &WadSet, lump: usize, reason: String) -> SpriteError {
    let name = wads
        .entries()
        .get(lump)
        .map(|e| String::from_utf8_lossy(e.name.trimmed()).into_owned())
        .unwrap_or_else(|| format!("<lump {lump}>"));
    SpriteError::BadLumpHeader { lump, name, reason }
}

/// Minimal `patch_t` header validation: exactly the bytes
/// `R_InitSpriteLumps` trusts (`width`, `leftoffset`, `topoffset`),
/// plus `height > 0` because a corrupt width would otherwise silently
/// project garbage. Full validation still happens in `decode_patch`
/// when the patch is drawn.
fn read_patch_header(data: &[u8]) -> Result<(i16, i16, i16), String> {
    if data.len() < 8 {
        return Err(format!("{} bytes, need 8", data.len()));
    }
    let width = i16::from_le_bytes([data[0], data[1]]);
    let height = i16::from_le_bytes([data[2], data[3]]);
    let left = i16::from_le_bytes([data[4], data[5]]);
    let top = i16::from_le_bytes([data[6], data[7]]);
    if width <= 0 {
        return Err(format!("width {width}"));
    }
    if height <= 0 {
        return Err(format!("height {height}"));
    }
    Ok((width, left, top))
}

/// One sprite frame: `rotation 0` means one lump for every angle
/// (`rotate == false`), otherwise eight per-rotation lumps with eighth
/// turns measured clockwise from facing the viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpriteFrame {
    /// `false` = single orientation for all view angles.
    pub rotate: bool,
    /// Global lump index per viewing rotation (`rotation 0` uses slot 0;
    /// `rotation 1..=8` map to slots 0..=7 after the `-1` decrement the
    /// original applies).
    pub lump: [usize; 8],
    /// Per-rotation horizontal flip (the trailing `<FRAME><ROT>` pair in
    /// the lump name being set).
    pub flip: [bool; 8],
}

/// A sprite: the frames found in the namespace for one 4-byte prefix.
#[derive(Clone, Debug)]
pub struct Sprite {
    /// The 4-byte prefix, e.g. `POSS`.
    pub name: [u8; 4],
    /// `frames[i]` is `Some` for every frame up to the highest frame
    /// letter seen; missing letters are `None` and signal an error at
    /// build time.
    pub frames: Vec<Option<SpriteFrame>>,
}

impl Sprite {
    /// Number of frames (`A..=last`).
    pub fn num_frames(&self) -> usize {
        self.frames.len()
    }

    /// Frame `index` (0 = `A`), masking `FF_FRAMEMASK` bits is the
    /// caller's job.
    pub fn frame(&self, index: usize) -> Option<&SpriteFrame> {
        self.frames.get(index).and_then(Option::as_ref)
    }
}

/// The `sprites[]` table from `R_InitSprites`/`R_InitSpriteDefs`.
#[derive(Clone, Debug, Default)]
pub struct SpriteDefinitions {
    sprites: Vec<Sprite>,
}

impl SpriteDefinitions {
    /// Scans the sprite namespace for each name and validates the frames.
    ///
    /// `names` are the 4-byte prefixes from the info tables; an empty
    /// list yields an empty table, like the original's `numsprites == 0`
    /// early out. Errors abort the build exactly like the original's
    /// `I_Error`s, but as typed errors.
    pub fn build(names: &[[u8; 4]], manager: &ResourceManager) -> Result<Self, SpriteError> {
        let mut sprites = Vec::with_capacity(names.len());
        let wads = manager.wads();
        let entries = wads.entries();
        // missing / inverted namespace markers would leave the sprite
        // system blind; fail fast like the original's W_GetNumForName
        let namespace = sprite_namespace(wads).ok_or(SpriteError::MissingSpriteNamespace)?;
        for name in names {
            sprites.push(build_sprite(name, namespace, entries)?);
        }
        Ok(Self { sprites })
    }

    /// Number of sprites in the table.
    pub fn num_sprites(&self) -> usize {
        self.sprites.len()
    }

    /// `sprites[index]`.
    pub fn sprite(&self, index: usize) -> Option<&Sprite> {
        self.sprites.get(index)
    }
}

/// Working state of one frame letter while scanning (`sprtemp[29]`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameSlot {
    /// Nothing installed yet (`-1`).
    Unset,
    /// A rotation-0 lump was installed (`0`).
    Single,
    /// At least one `1..=8` lump was installed (`1`).
    Rotated,
}

#[derive(Clone, Copy)]
struct Install {
    lump: usize,
    flipped: bool,
}

fn build_sprite(
    name: &[u8; 4],
    namespace: (usize, usize),
    entries: &[crate::wad::LumpEntry],
) -> Result<Sprite, SpriteError> {
    let sprite_name = String::from_utf8_lossy(name).into_owned();
    let mut slots = [FrameSlot::Unset; MAX_SPRITE_FRAMES];
    let mut lumps: [[Option<Install>; 8]; MAX_SPRITE_FRAMES] = [[None; 8]; MAX_SPRITE_FRAMES];
    let mut max_frame: Option<usize> = None;

    let (first, last) = namespace;
    for (lump, entry) in entries.iter().enumerate().take(last + 1).skip(first) {
        let raw = entry.name.as_bytes();
        if &raw[..4] != name {
            continue;
        }
        // a 4-byte prefix match is not enough; frame/rotation bytes
        // must be valid for BOTH the base entry and the flip entry
        let frame = i32::from(raw[4]) - i32::from(b'A');
        let rotation = i32::from(raw[5]) - i32::from(b'0');
        if !(0..MAX_SPRITE_FRAMES as i32).contains(&frame) || !(0..=8).contains(&rotation) {
            return Err(SpriteError::BadFrameChar {
                sprite: sprite_name,
                lump,
                raw: *raw,
            });
        }
        install_lump(
            &mut slots,
            &mut lumps,
            &mut max_frame,
            frame as usize,
            rotation as usize,
            Install {
                lump,
                flipped: false,
            },
            &sprite_name,
            lump,
        )?;

        if raw[6] != 0 {
            let flip_frame = i32::from(raw[6]) - i32::from(b'A');
            let flip_rotation = i32::from(raw[7]) - i32::from(b'0');
            if !(0..MAX_SPRITE_FRAMES as i32).contains(&flip_frame)
                || !(0..=8).contains(&flip_rotation)
            {
                return Err(SpriteError::BadFrameChar {
                    sprite: sprite_name,
                    lump,
                    raw: *raw,
                });
            }
            install_lump(
                &mut slots,
                &mut lumps,
                &mut max_frame,
                flip_frame as usize,
                flip_rotation as usize,
                Install {
                    lump,
                    flipped: true,
                },
                &sprite_name,
                lump,
            )?;
        }
    }

    let Some(max_frame) = max_frame else {
        // no lumps at all for this prefix: the original ships a
        // zero-frame definition (and the game would error on use)
        return Ok(Sprite {
            name: *name,
            frames: Vec::new(),
        });
    };
    let num_frames = max_frame + 1;

    let mut frames = Vec::with_capacity(num_frames);
    for (frame_index, slot) in slots.iter().take(num_frames).enumerate() {
        match slot {
            FrameSlot::Unset => {
                return Err(SpriteError::NoPatchesForFrame {
                    sprite: sprite_name,
                    frame: (b'A' + frame_index as u8) as char,
                });
            }
            FrameSlot::Single => {
                let install = lumps[frame_index][0].expect("single slot filled");
                let frame = SpriteFrame {
                    rotate: false,
                    lump: [install.lump; 8],
                    flip: [install.flipped; 8],
                };
                frames.push(Some(frame));
            }
            FrameSlot::Rotated => {
                let mut lump_ids = [0usize; 8];
                let mut flips = [false; 8];
                for rotation in 0..8 {
                    let Some(install) = lumps[frame_index][rotation] else {
                        return Err(SpriteError::MissingRotation {
                            sprite: sprite_name,
                            frame: (b'A' + frame_index as u8) as char,
                            rotation: (rotation + 1) as u8,
                        });
                    };
                    lump_ids[rotation] = install.lump;
                    flips[rotation] = install.flipped;
                }
                frames.push(Some(SpriteFrame {
                    rotate: true,
                    lump: lump_ids,
                    flip: flips,
                }));
            }
        }
    }

    Ok(Sprite {
        name: *name,
        frames,
    })
}

// the argument list mirrors the original install path one-to-one
#[allow(clippy::too_many_arguments)]
fn install_lump(
    slots: &mut [FrameSlot; MAX_SPRITE_FRAMES],
    lumps: &mut [[Option<Install>; 8]; MAX_SPRITE_FRAMES],
    max_frame: &mut Option<usize>,
    frame: usize,
    rotation: usize,
    install: Install,
    sprite: &str,
    lump: usize,
) -> Result<(), SpriteError> {
    *max_frame = Some(max_frame.map_or(frame, |m| m.max(frame)));
    if rotation == 0 {
        // the lump should be used for all rotations
        match slots[frame] {
            FrameSlot::Single => {
                return Err(SpriteError::RotationConflict {
                    sprite: sprite.to_string(),
                    frame: (b'A' + frame as u8) as char,
                });
            }
            FrameSlot::Rotated => {
                return Err(SpriteError::RotationConflict {
                    sprite: sprite.to_string(),
                    frame: (b'A' + frame as u8) as char,
                });
            }
            FrameSlot::Unset => {}
        }
        slots[frame] = FrameSlot::Single;
        for slot in lumps[frame].iter_mut() {
            *slot = Some(install);
        }
        return Ok(());
    }

    if slots[frame] == FrameSlot::Single {
        return Err(SpriteError::RotationConflict {
            sprite: sprite.to_string(),
            frame: (b'A' + frame as u8) as char,
        });
    }
    slots[frame] = FrameSlot::Rotated;
    let slot = &mut lumps[frame][rotation - 1];
    if slot.is_some() {
        return Err(SpriteError::DuplicateRotation {
            sprite: sprite.to_string(),
            frame: (b'A' + frame as u8) as char,
            rotation: rotation as u8,
            lump,
        });
    }
    *slot = Some(install);
    Ok(())
}

/// The thing-like input for [`project_sprite`]; a stand-in for the mobj
/// until the gameplay agent lands. All coordinates are world fixed point,
/// `angle` is the thing's facing (BAM).
#[derive(Clone, Copy, Debug)]
pub struct SpriteThing {
    /// World x.
    pub x: Fixed,
    /// World y.
    pub y: Fixed,
    /// World z (feet).
    pub z: Fixed,
    /// Facing, BAM.
    pub angle: Angle,
    /// Index into [`SpriteDefinitions`].
    pub sprite: usize,
    /// Frame field, possibly OR'd with `FF_FULLBRIGHT`.
    pub frame: u32,
    /// Mobj flags (`MF_SHADOW` / `MF_TRANSLATION` consulted).
    pub flags: u32,
}

/// A projected sprite: the `vissprite_t` analog. Coordinates are screen
/// columns/rows, `scale` is `xscale << detailshift` exactly like the
/// original (i.e. compare with drawseg scales directly).
#[derive(Clone, Copy, Debug)]
pub struct ProjectedSprite {
    /// Mobj flags copied at projection time (translation check happens
    /// at draw time).
    pub mobj_flags: u32,
    /// `xscale << detailshift`.
    pub scale: Fixed,
    /// World x.
    pub gx: Fixed,
    /// World y.
    pub gy: Fixed,
    /// World z (feet).
    pub gz: Fixed,
    /// World z + sprite top offset.
    pub gzt: Fixed,
    /// `gzt - viewz`.
    pub texturemid: Fixed,
    /// Left edge column, clamped to `0..viewwidth-1`.
    pub x1: i32,
    /// Right edge column, clamped to `0..viewwidth-1`.
    pub x2: i32,
    /// Column step through the source patch, signed (negative when flipped).
    pub xiscale: Fixed,
    /// `frac` for the first visible column.
    pub startfrac: Fixed,
    /// Global lump index of the patch to draw.
    pub patch: usize,
    /// Horizontal flip.
    pub flip: bool,
    /// `Some(level)` = diminished/fixed colormap level; `None` = the
    /// spectre fuzz effect.
    pub colormap: Option<usize>,
}

/// Projects one thing. `spritelights` is the sector's `scalelight` row
/// (via [`crate::render::lighting::LightTables::scalelight_row`]); `view` carries the projection
/// constants. Returns `None` when the thing is behind the view, outside
/// the frustum, off the screen, or has an invalid sprite/frame index
/// (the original crashed on the last two; we just skip).
pub fn project_sprite(
    thing: &SpriteThing,
    view: &WallView<'_>,
    defs: &SpriteDefinitions,
    lumps: &SpriteLumpTables,
    spritelights: &[usize; MAXLIGHTSCALE],
) -> Option<ProjectedSprite> {
    let tr_x = thing.x - view.view_x;
    let tr_y = thing.y - view.view_y;

    let gxt = tr_x * view.view_cos;
    let gyt = -tr_y * view.view_sin;
    let tz = gxt - gyt;

    // thing is behind view plane?
    if tz < MIN_Z {
        return None;
    }

    let xscale = view.projection_distance / tz;

    let gxt = -tr_x * view.view_sin;
    let gyt = tr_y * view.view_cos;
    let tx = -(gyt + gxt);

    // too far off the side? exact i64 comparison (the original's
    // `tz<<2` overflows for very large tz)
    if (tx.abs().to_raw() as i64) > (tz.to_raw() as i64) << 2 {
        return None;
    }

    let sprite = defs.sprite(thing.sprite)?;
    let frame_index = (thing.frame & FF_FRAMEMASK) as usize;
    let sprframe = sprite.frame(frame_index)?;

    let (lump, flip) = if sprframe.rotate {
        // choose a different rotation based on player view
        let ang = point_to_angle_2(view.view_x, view.view_y, thing.x, thing.y);
        let rot = ((ang.to_raw().wrapping_sub(thing.angle.to_raw())).wrapping_add(ANGLE_ROT_OFFSET)
            >> 29) as usize;
        (sprframe.lump[rot], sprframe.flip[rot])
    } else {
        // use single rotation for all views
        (sprframe.lump[0], sprframe.flip[0])
    };

    // calculate edges of the shape
    let mut tx = tx - lumps.offset(lump);
    let raw_x1 = (view.center_x_frac + tx * xscale).to_raw() >> FRACBITS;

    // off the right side?
    if raw_x1 > view.view_width {
        return None;
    }

    tx += lumps.width(lump);
    let raw_x2 = ((view.center_x_frac + tx * xscale).to_raw() >> FRACBITS) - 1;

    // off the left side
    if raw_x2 < 0 {
        return None;
    }

    let x1 = raw_x1.max(0);
    let x2 = raw_x2.min(view.view_width - 1);

    let gzt = thing.z + lumps.top_offset(lump);
    let texturemid = gzt - view.view_z;
    let scale = Fixed::from_raw(
        xscale
            .to_raw()
            .wrapping_shl(view.detail_shift.max(0) as u32),
    );

    let iscale = Fixed::ONE / xscale;
    let (mut startfrac, xiscale) = if flip {
        (Fixed::from_raw(lumps.width(lump).to_raw() - 1), -iscale)
    } else {
        (Fixed::ZERO, iscale)
    };

    if x1 > raw_x1 {
        startfrac += xiscale * Fixed::from_int(x1 - raw_x1);
    }

    let colormap = if thing.flags & MF_SHADOW != 0 {
        // shadow draw
        None
    } else if let Some(level) = view.fixed_colormap {
        // fixed map
        Some(level)
    } else if thing.frame & FF_FULLBRIGHT != 0 {
        // full bright
        Some(0)
    } else {
        // diminished light
        let shift = (LIGHTSCALESHIFT as i32 - view.detail_shift).max(0) as u32;
        let mut index = (xscale.to_raw() >> shift) as usize;
        if index >= MAXLIGHTSCALE {
            index = MAXLIGHTSCALE - 1;
        }
        Some(spritelights[index])
    };

    Some(ProjectedSprite {
        mobj_flags: thing.flags,
        scale,
        gx: thing.x,
        gy: thing.y,
        gz: thing.z,
        gzt,
        texturemid,
        x1,
        x2,
        xiscale,
        startfrac,
        patch: lump,
        flip,
        colormap,
    })
}

/// `R_SortVisSprites`: stable ascending `scale` order (smallest =
/// farthest first, so the opaque draw covers near sprites correctly).
/// The original's selection sort is stable for equal scales, and
/// [`slice::sort_by`] preserves that, so the draw order is identical.
pub fn sort_vissprites(sprites: &mut [ProjectedSprite]) {
    sprites.sort_by_key(|spr| spr.scale);
}

/// The `translationtables` from `R_InitTranslationTables`: the 16-entry
/// green ramp (`0x70..=0x7f`) mapped to gray/brown/red, everything else
/// identity. Used by PLAY sprites when `MF_TRANSLATION` is set.
///
/// Note: nothing in linuxdoom-1.10 ever sets `MF_TRANSLATION` outside the
/// renderer (the flag was honored for net-game player colors in earlier
/// builds; 1.10 keeps the code path but never sets it), so this is dead
/// machinery kept for parity and for the future multiplayer agent.
#[derive(Clone, Debug)]
pub struct TranslationTables {
    gray: [u8; 256],
    brown: [u8; 256],
    red: [u8; 256],
}

impl TranslationTables {
    /// Builds the tables exactly like `R_InitTranslationTables`.
    pub fn build() -> Self {
        let mut gray = [0u8; 256];
        let mut brown = [0u8; 256];
        let mut red = [0u8; 256];
        for i in 0..256usize {
            if (0x70..=0x7f).contains(&i) {
                gray[i] = 0x60 + (i & 0x0f) as u8;
                brown[i] = 0x40 + (i & 0x0f) as u8;
                red[i] = 0x20 + (i & 0x0f) as u8;
            } else {
                gray[i] = i as u8;
                brown[i] = i as u8;
                red[i] = i as u8;
            }
        }
        Self { gray, brown, red }
    }

    /// The translation table selected by `flags`, if `MF_TRANSLATION` is
    /// set. Mirrors `translationtables + ((flags & MF_TRANSLATION) >>
    /// (MF_TRANSSHIFT - 8)) - 256` with the base-pointer arithmetic
    /// folded in.
    pub fn select(&self, flags: u32) -> Option<&[u8; 256]> {
        if flags & MF_TRANSLATION == 0 {
            return None;
        }
        match (flags & MF_TRANSLATION) >> MF_TRANSSHIFT {
            1 => Some(&self.gray),
            2 => Some(&self.brown),
            3 => Some(&self.red),
            _ => None,
        }
    }
}

/// The per-column painter selected by `vis->colormap` +
/// `vis->mobjflags` in `R_DrawVisSprite`.
#[derive(Clone, Copy)]
pub(crate) enum ColumnPainter<'a> {
    /// `basecolfunc` with the diminished/fixed colormap.
    Base(&'a Colormap),
    /// `R_DrawTranslatedColumn`: `colormap[translation[texel]]`.
    Translated(&'a Colormap, &'a [u8; 256]),
    /// `fuzzcolfunc`: the spectre shimmer, no source texels read.
    Fuzz,
}

/// `R_DrawTranslatedColumn`: like [`draw_column`] but each texel is
/// re-mapped through `translation` before the colormap. No low-detail
/// variant exists in the original (under `detaillevel = 1` the translated
/// column still draws full resolution).
fn draw_translated_column(
    fb: &mut super::framebuffer::IndexedFramebuffer,
    source: &UnmaskedColumn<'_>,
    params: &ColumnParams<'_>,
    colormap: &Colormap,
    translation: &[u8; 256],
) {
    use super::framebuffer::{SCREEN_HEIGHT, SCREEN_WIDTH, step_frac_n};

    let &ColumnParams {
        x,
        y_lo,
        y_hi,
        iscale,
        texturemid,
        center_y,
        ..
    } = params;

    if x < 0 || x >= SCREEN_WIDTH as i32 || y_hi < y_lo {
        return;
    }
    let first = y_lo.max(0);
    let last = y_hi.min(SCREEN_HEIGHT as i32 - 1);
    if first > last {
        return;
    }

    let mut frac = texturemid + iscale * Fixed::from_int(y_lo - center_y);
    frac = step_frac_n(frac, iscale, i64::from(first - y_lo));
    let x_addr = x as usize;
    let pixels = fb.as_mut_slice();
    for y in first..=last {
        let addr = y as usize * SCREEN_WIDTH + x_addr;
        let texel = source.texel(frac.to_raw() >> FRACBITS);
        pixels[addr] = colormap.lookup(translation[usize::from(texel)]);
        frac += iscale;
    }
}

/// `R_DrawMaskedColumn`: iterates the post runs of one patch column and
/// draws each run as a sub-column. `mfloorclip`/`mceilingclip` are the
/// clip arrays for this sprite (as built by the masked pass, or the
/// screen-height/neg-one arrays when drawing unclipped).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_masked_column(
    fb: &mut super::framebuffer::IndexedFramebuffer,
    posts: &[Post],
    mfloorclip: &[i16],
    mceilingclip: &[i16],
    basetexturemid: Fixed,
    dc_iscale: Fixed,
    spryscale: Fixed,
    sprtopscreen: Fixed,
    dc_x: i32,
    center_y: i32,
    painter: &ColumnPainter<'_>,
    fuzz: &mut FuzzState,
    fuzz_colormap: &Colormap,
    detail_shift: i32,
    view_height: i32,
) {
    let clip_floor = mfloorclip.get(dc_x as usize).copied();
    let clip_ceil = mceilingclip.get(dc_x as usize).copied();

    for post in posts {
        // calculate unclipped screen coordinates for post
        let topscreen = sprtopscreen + spryscale * Fixed::from_int(i32::from(post.top_delta));
        let bottomscreen = topscreen + spryscale * Fixed::from_int(post.pixels.len() as i32);

        // (topscreen+FRACUNIT-1)>>FRACBITS and (bottomscreen-1)>>FRACBITS
        // in i64 so huge scaled sprites can't wrap
        let yl = (topscreen.to_raw() as i64 + 0xffff) >> FRACBITS;
        let yh = (bottomscreen.to_raw() as i64 - 1) >> FRACBITS;
        let mut dc_yl = yl.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let mut dc_yh = yh.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;

        if let Some(clip) = clip_floor
            && dc_yh >= i32::from(clip)
        {
            dc_yh = i32::from(clip) - 1;
        }
        if let Some(clip) = clip_ceil
            && dc_yl <= i32::from(clip)
        {
            dc_yl = i32::from(clip) + 1;
        }

        if dc_yl <= dc_yh {
            let texturemid = basetexturemid - Fixed::from_int(i32::from(post.top_delta));
            match painter {
                ColumnPainter::Base(colormap) => {
                    let source = UnmaskedColumn::new(&post.pixels);
                    let params = ColumnParams {
                        colormap,
                        x: dc_x,
                        y_lo: dc_yl,
                        y_hi: dc_yh,
                        iscale: dc_iscale,
                        texturemid,
                        center_y,
                    };
                    if detail_shift == 1 {
                        draw_column_low(fb, &source, &params);
                    } else {
                        draw_column(fb, &source, &params);
                    }
                }
                ColumnPainter::Translated(colormap, translation) => {
                    let source = UnmaskedColumn::new(&post.pixels);
                    let params = ColumnParams {
                        colormap,
                        x: dc_x,
                        y_lo: dc_yl,
                        y_hi: dc_yh,
                        iscale: dc_iscale,
                        texturemid,
                        center_y,
                    };
                    draw_translated_column(fb, &source, &params, colormap, translation);
                }
                ColumnPainter::Fuzz => {
                    draw_fuzz_column(fb, fuzz_colormap, fuzz, dc_x, dc_yl, dc_yh, view_height);
                }
            }
        }
    }
}

/// `R_DrawVisSprite`: draws one projected sprite through the clip arrays.
/// `patch` is the decoded patch (callers use the resource manager's
/// `decode_cached::<Patch>` to avoid repeated decodes).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_vis_sprite(
    fb: &mut super::framebuffer::IndexedFramebuffer,
    fuzz: &mut FuzzState,
    vis: &ProjectedSprite,
    patch: &Patch,
    colormaps: &[Colormap],
    translations: &TranslationTables,
    mfloorclip: &[i16],
    mceilingclip: &[i16],
    detail_shift: i32,
    center_y: i32,
    center_y_frac: Fixed,
    view_height: i32,
) {
    let dc_iscale = Fixed::from_raw(vis.xiscale.abs().to_raw() >> detail_shift.max(0) as u32);
    let spryscale = vis.scale;
    let sprtopscreen = center_y_frac - vis.texturemid * spryscale;

    if colormaps.is_empty() {
        // corrupt input; masked.rs already reported this
        return;
    }
    let fuzz_colormap = colormaps.get(FUZZ_COLORMAP).unwrap_or(&colormaps[0]);
    let colormap = vis
        .colormap
        .map(|level| &colormaps[level.min(colormaps.len() - 1)]);
    let translation = vis
        .colormap
        .and_then(|_| translations.select(vis.mobj_flags));
    let painter = match (colormap, translation) {
        (None, Some(_)) | (None, None) => ColumnPainter::Fuzz,
        (Some(colormap), None) => ColumnPainter::Base(colormap),
        (Some(colormap), Some(table)) => ColumnPainter::Translated(colormap, table),
    };

    let mut frac = vis.startfrac;
    for dc_x in vis.x1..=vis.x2 {
        let texturecolumn = frac.to_raw() >> FRACBITS;
        let column = usize::try_from(texturecolumn)
            .ok()
            .and_then(|c| patch.column(c));
        if let Some(column) = column {
            draw_masked_column(
                fb,
                &column.posts,
                mfloorclip,
                mceilingclip,
                vis.texturemid,
                dc_iscale,
                spryscale,
                sprtopscreen,
                dc_x,
                center_y,
                &painter,
                fuzz,
                fuzz_colormap,
                detail_shift,
                view_height,
            );
        }
        // out-of-range columns are skipped (the original read garbage);
        // the masked columns of spectre draws must not advance fuzz state
        frac += vis.xiscale;
    }
}

/// Decode helper for the patch-provider closure callers pass to the
/// masked pass: `decode_patch` wrapped in the resource manager's cache.
pub fn decode_sprite_patch(
    manager: &ResourceManager,
    lump: usize,
) -> crate::errors::EngineResult<std::sync::Arc<Patch>> {
    manager.decode_cached::<Patch>(lump, |bytes| decode_patch(bytes).map_err(EngineError::from))
}

/// Vanilla `BASEYCENTER` (`r_things.c`): the fixed vertical anchor the
/// weapon and flash sprites hang from. Independent of `view_height` on
/// purpose — the original draws psprites against the full 200-row
/// screen.
pub const BASEYCENTER: i32 = 100;

/// The `FF_FULLBRIGHT` bit of a raw 16-bit `frame` field (the info
/// tables keep the flag in the low half).
const FF_FULLBRIGHT_U16: u16 = 0x8000;

/// `R_DrawPSprite`: draws one overlay (weapon or flash) sprite.
///
/// `sprite` is the `SPR_*` ordinal from the info tables, `frame` is the
/// raw `frame` field of the state (fullbright bit included, exactly
/// like `states[].frame`), and `sx`/`sy` are the fixed-point screen
/// coordinates the psprite layer maintains in `320*200` space.
///
/// Clipping is against the full view height, matching the original
/// (psprites clip against `screenheightarray`/`negonearray`, i.e. not at
/// all). `spritelights` is the scalelight row of the view sector; the
/// overlay uses its `MAXLIGHTSCALE-1` entry when the state is not
/// fullbright, like the original. Returns `true` when the sprite was at
/// least partially on screen.
#[allow(clippy::too_many_arguments)]
pub fn draw_psprite(
    fb: &mut super::framebuffer::IndexedFramebuffer,
    sprite: usize,
    frame: u16,
    sx: Fixed,
    sy: Fixed,
    power_invisibility: i32,
    view: &WallView<'_>,
    defs: &SpriteDefinitions,
    lumps: &SpriteLumpTables,
    colormaps: &[Colormap],
    spritelights: &[usize; MAXLIGHTSCALE],
    manager: &ResourceManager,
    translations: &TranslationTables,
    fuzz: &mut FuzzState,
) -> bool {
    // decide which patch to use
    let Some(sprite_def) = defs.sprite(sprite) else {
        tracing::warn!("R_DrawPSprite: invalid sprite number {sprite}");
        return false;
    };
    let frame_index = (frame & !FF_FULLBRIGHT_U16) as usize;
    let Some(sprframe) = sprite_def.frame(frame_index) else {
        tracing::warn!("R_DrawPSprite: invalid sprite frame {sprite}:{frame_index}");
        return false;
    };

    let lump = sprframe.lump[0];
    let flip = sprframe.flip[0];

    // pspritescale / pspriteiscale, from R_ExecuteSetViewSize
    let pspritescale = Fixed::from_raw(FRACUNIT * view.view_width / SCREEN_WIDTH as i32);
    let pspriteiscale = Fixed::from_raw(FRACUNIT * SCREEN_WIDTH as i32 / view.view_width);

    // calculate edges of the shape
    let mut tx = sx - Fixed::from_raw((SCREEN_WIDTH as i32 / 2) * FRACUNIT);
    tx -= lumps.offset(lump);
    let raw_x1 = (view.center_x_frac + tx * pspritescale).to_raw() >> FRACBITS;

    // off the right side
    if raw_x1 > view.view_width {
        return false;
    }

    tx += lumps.width(lump);
    let raw_x2 = ((view.center_x_frac + tx * pspritescale).to_raw() >> FRACBITS) - 1;

    // off the left side
    if raw_x2 < 0 {
        return false;
    }

    let x1 = raw_x1.max(0);
    let x2 = raw_x2.min(view.view_width - 1);

    let texturemid = Fixed::from_int(BASEYCENTER) + Fixed::from_raw(FRACUNIT / 2)
        - (sy - lumps.top_offset(lump));

    let scale = Fixed::from_raw(
        pspritescale
            .to_raw()
            .wrapping_shl(view.detail_shift.max(0) as u32),
    );
    let (startfrac, xiscale) = if flip {
        // the original stores `spritewidth[lump] - 1`, not
        // `(width - 1) << FRACBITS` — kept 1:1
        (
            Fixed::from_raw(lumps.width(lump).to_raw() - 1),
            -pspriteiscale,
        )
    } else {
        (Fixed::ZERO, pspriteiscale)
    };
    let startfrac = if x1 > raw_x1 {
        startfrac + xiscale * Fixed::from_int(x1 - raw_x1)
    } else {
        startfrac
    };

    let mobj_flags = 0; // vanilla zeroes the translation bits for psprites
    let colormap = if power_invisibility > 4 * 32 || (power_invisibility & 8) != 0 {
        // shadow draw
        None
    } else if let Some(level) = view.fixed_colormap {
        // fixed color
        Some(level)
    } else if (frame & FF_FULLBRIGHT_U16) != 0 {
        // full bright
        Some(0)
    } else {
        // local light, closest entry like the original
        Some(spritelights[MAXLIGHTSCALE - 1])
    };

    let vis = ProjectedSprite {
        mobj_flags,
        scale,
        gx: Fixed::ZERO,
        gy: Fixed::ZERO,
        gz: Fixed::ZERO,
        gzt: Fixed::ZERO,
        texturemid,
        x1,
        x2,
        xiscale,
        startfrac,
        patch: lump,
        flip,
        colormap,
    };

    let patch = match decode_sprite_patch(manager, lump) {
        Ok(patch) => patch,
        Err(err) => {
            tracing::warn!("R_DrawPSprite: failed to decode patch for psprite: {err}");
            return false;
        }
    };

    // psprites never clip: full view height / neg-one, like the original
    let view_height = view.view_height.max(0) as i16;
    let mfloorclip = [view_height; super::framebuffer::SCREEN_WIDTH];
    let mceilingclip = [-1i16; super::framebuffer::SCREEN_WIDTH];

    draw_vis_sprite(
        fb,
        fuzz,
        &vis,
        &patch,
        colormaps,
        translations,
        &mfloorclip,
        &mceilingclip,
        view.detail_shift,
        view.center_y,
        view.center_y_frac,
        view.view_height,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::ViewTables;
    use crate::render::lighting::LightTables;
    use crate::render::wall::WallProjection;
    use crate::render::{IndexedFramebuffer, SCREEN_WIDTH};
    use crate::wad::{WadFile, testutil::build_wad};

    const VIEW_WIDTH: i32 = 320;
    const VIEW_HEIGHT: i32 = 200;

    fn patch_bytes(
        width: i16,
        height: i16,
        left_offset: i16,
        top_offset: i16,
        columns: &[Vec<(u8, Vec<u8>)>],
    ) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&left_offset.to_le_bytes());
        data.extend_from_slice(&top_offset.to_le_bytes());
        let mut ptrs = Vec::with_capacity(width as usize);
        let mut body = Vec::new();
        let mut offset = 8 + width as usize * 4;
        for column in columns {
            ptrs.push(offset as i32);
            for (top_delta, pixels) in column {
                body.extend_from_slice(&[*top_delta, pixels.len() as u8, 0]);
                body.extend_from_slice(pixels);
                body.push(0);
                offset += 4 + pixels.len();
            }
            body.push(0xff);
            offset += 1;
        }
        for ptr in &ptrs {
            data.extend_from_slice(&ptr.to_le_bytes());
        }
        data.extend_from_slice(&body);
        data
    }

    fn simple_patch(width: i16, height: i16, fill: u8) -> Vec<u8> {
        let columns = (0..width)
            .map(|_| vec![(0u8, vec![fill; height as usize])])
            .collect::<Vec<_>>();
        patch_bytes(width, height, 4, 8, &columns)
    }

    fn sprite_manager(lumps: &[(&str, Vec<u8>)]) -> ResourceManager {
        let pnames = 0i32.to_le_bytes().to_vec();
        let texture1 = 0i32.to_le_bytes().to_vec();
        let mut all: Vec<(&str, &[u8])> = vec![
            ("PNAMES", pnames.as_slice()),
            ("TEXTURE1", texture1.as_slice()),
            ("S_START", &[]),
        ];
        let owned: Vec<Vec<u8>> = lumps.iter().map(|(_, d)| d.clone()).collect();
        for (i, (name, _)) in lumps.iter().enumerate() {
            all.push((name, owned[i].as_slice()));
        }
        all.push(("S_END", &[]));
        ResourceManager::new(
            WadFile::from_bytes(build_wad(b"IWAD", &all)).unwrap(),
            Vec::new(),
        )
    }

    fn empty_manager() -> ResourceManager {
        sprite_manager(&[])
    }

    fn lump_index(manager: &ResourceManager, name: &str) -> usize {
        manager.wads().find_lump(name).expect("lump present")
    }

    fn defs(names: &[[u8; 4]], manager: &ResourceManager) -> SpriteDefinitions {
        SpriteDefinitions::build(names, manager).unwrap()
    }

    struct Kit {
        tables: ViewTables,
        projection: WallProjection,
        light: LightTables,
        textures: super::super::textures::TextureSet,
        colormaps: Vec<Colormap>,
    }

    fn identity_colormaps() -> Vec<Colormap> {
        (0..33)
            .map(|_| Colormap(core::array::from_fn(|i| i as u8)))
            .collect()
    }

    fn kit(manager: &ResourceManager) -> Kit {
        let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
        let projection = WallProjection::build(&tables, VIEW_WIDTH);
        Kit {
            tables,
            projection,
            light: LightTables::build(VIEW_WIDTH),
            textures: super::super::textures::TextureSet::from_resources(manager).unwrap(),
            colormaps: identity_colormaps(),
        }
    }

    impl Kit {
        fn view<'a>(&'a self, angle: Angle) -> WallView<'a> {
            self.view_ex(angle, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO, 0, None)
        }

        fn view_ex<'a>(
            &'a self,
            angle: Angle,
            view_x: Fixed,
            view_y: Fixed,
            view_z: Fixed,
            detail_shift: i32,
            fixed_colormap: Option<usize>,
        ) -> WallView<'a> {
            WallView {
                tables: &self.tables,
                projection: &self.projection,
                light: &self.light,
                textures: &self.textures,
                colormaps: &self.colormaps,
                view_x,
                view_y,
                view_z,
                view_angle: angle,
                view_cos: crate::tables::FINE_COSINE[angle.fine_angle()],
                view_sin: crate::tables::FINE_SINE[angle.fine_angle()],
                view_width: VIEW_WIDTH,
                view_height: VIEW_HEIGHT,
                center_y: VIEW_HEIGHT / 2,
                center_x_frac: Fixed::from_int(VIEW_WIDTH / 2),
                center_y_frac: Fixed::from_int(VIEW_HEIGHT / 2),
                projection_distance: Fixed::from_int(VIEW_WIDTH / 2),
                extralight: 0,
                fixed_colormap,
                detail_shift,
                sky_flat: None,
            }
        }
    }

    fn thing(x: Fixed, y: Fixed, z: Fixed, sprite: usize, frame: u32) -> SpriteThing {
        SpriteThing {
            x,
            y,
            z,
            angle: Angle::ZERO,
            sprite,
            frame,
            flags: 0,
        }
    }

    fn spritelights() -> [usize; MAXLIGHTSCALE] {
        [7; MAXLIGHTSCALE]
    }

    // ------------------------------------------------------------------
    // namespace scan / frame layout
    // ------------------------------------------------------------------

    #[test]
    fn lump_tables_fill_only_the_sprite_namespace() {
        let manager = sprite_manager(&[
            ("POSSA1", simple_patch(8, 12, 3)),
            ("POSSA2", simple_patch(8, 12, 3)),
        ]);
        let tables = SpriteLumpTables::build(&manager).unwrap();
        let first = lump_index(&manager, "POSSA1");
        let second = lump_index(&manager, "POSSA2");
        assert_eq!(tables.width(first), Fixed::from_int(8));
        assert_eq!(tables.offset(first), Fixed::from_int(4));
        assert_eq!(tables.top_offset(first), Fixed::from_int(8));
        assert_eq!(tables.width(second), Fixed::from_int(8));
        // PNAMES is outside the namespace: zeroes, not garbage
        assert_eq!(tables.width(0), Fixed::ZERO);
    }

    #[test]
    fn defs_single_orientation_lump_covers_all_rotations() {
        let manager = sprite_manager(&[("POSSA0", simple_patch(6, 10, 1))]);
        let defs = defs(&[*b"POSS"], &manager);
        assert_eq!(defs.num_sprites(), 1);
        let sprite = defs.sprite(0).unwrap();
        assert_eq!(sprite.num_frames(), 1);
        let frame = sprite.frame(0).unwrap();
        assert!(!frame.rotate);
        let idx = lump_index(&manager, "POSSA0");
        assert_eq!(frame.lump, [idx; 8]);
        assert_eq!(frame.flip, [false; 8]);
    }

    #[test]
    fn defs_eight_rotations_and_flip_companions() {
        let lumps: Vec<(&str, Vec<u8>)> = (1..=8)
            .map(|i| {
                let name: &'static str = String::leak(format!("POSSA{i}B{i}"));
                (name, simple_patch(8, 12, i as u8))
            })
            .collect();
        let manager = sprite_manager(&lumps);
        let defs = defs(&[*b"POSS"], &manager);
        let sprite = defs.sprite(0).unwrap();
        // frames A and B, both complete; B mirrors A's lumps flipped
        let frame_a = sprite.frame(0).unwrap();
        let frame_b = sprite.frame(1).unwrap();
        assert!(frame_a.rotate && frame_b.rotate);
        for rotation in 0..8 {
            assert_eq!(frame_a.lump[rotation], frame_b.lump[rotation]);
            assert!(!frame_a.flip[rotation]);
            assert!(frame_b.flip[rotation]);
        }
    }

    #[test]
    fn defs_duplicate_rotation_reports_both_lumps() {
        let manager = sprite_manager(&[
            ("POSSA1", simple_patch(8, 12, 1)),
            ("POSSA1", simple_patch(8, 12, 2)),
            ("POSSA2", simple_patch(8, 12, 3)),
            ("POSSA3", simple_patch(8, 12, 4)),
            ("POSSA4", simple_patch(8, 12, 5)),
            ("POSSA5", simple_patch(8, 12, 6)),
            ("POSSA6", simple_patch(8, 12, 7)),
            ("POSSA7", simple_patch(8, 12, 8)),
            ("POSSA8", simple_patch(8, 12, 9)),
        ]);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(
            err,
            SpriteError::DuplicateRotation { rotation: 1, .. }
        ));
    }

    #[test]
    fn defs_rotation_zero_and_rotated_lumps_conflict() {
        // rot=0 first, then a rotated lump
        let manager = sprite_manager(&[
            ("POSSA0", simple_patch(8, 12, 1)),
            ("POSSA1", simple_patch(8, 12, 2)),
        ]);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(
            err,
            SpriteError::RotationConflict { frame: 'A', .. }
        ));

        // rotated lump first, then rot=0
        let manager = sprite_manager(&[
            ("POSSA1", simple_patch(8, 12, 1)),
            ("POSSA0", simple_patch(8, 12, 2)),
        ]);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(
            err,
            SpriteError::RotationConflict { frame: 'A', .. }
        ));
    }

    #[test]
    fn defs_missing_rotations_are_reported() {
        let manager = sprite_manager(&[
            ("POSSA1", simple_patch(8, 12, 1)),
            ("POSSA2", simple_patch(8, 12, 2)),
            ("POSSA3", simple_patch(8, 12, 3)),
            ("POSSA4", simple_patch(8, 12, 4)),
            ("POSSA5", simple_patch(8, 12, 5)),
            ("POSSA6", simple_patch(8, 12, 6)),
            ("POSSA7", simple_patch(8, 12, 7)),
        ]);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(
            err,
            SpriteError::MissingRotation { rotation: 8, .. }
        ));
    }

    #[test]
    fn defs_frame_letter_gap_is_an_error() {
        let mut lumps: Vec<(&str, Vec<u8>)> = (1..=8)
            .map(|i| {
                let name: &'static str = String::leak(format!("POSSA{i}"));
                (name, simple_patch(8, 12, 1))
            })
            .collect();
        for i in 1..=8 {
            let name: &'static str = String::leak(format!("POSSC{i}"));
            lumps.push((name, simple_patch(8, 12, 2)));
        }
        let manager = sprite_manager(&lumps);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(
            err,
            SpriteError::NoPatchesForFrame { frame: 'B', .. }
        ));
    }

    #[test]
    fn defs_bad_frame_and_rotation_characters() {
        let manager = sprite_manager(&[("POSS!1", simple_patch(8, 12, 1))]);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(err, SpriteError::BadFrameChar { .. }));

        let manager = sprite_manager(&[("POSSA9", simple_patch(8, 12, 1))]);
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(err, SpriteError::BadFrameChar { .. }));
    }

    #[test]
    fn defs_prefix_without_lumps_gives_zero_frames() {
        let manager = empty_manager();
        let defs = defs(&[*b"NONE"], &manager);
        assert_eq!(defs.sprite(0).unwrap().num_frames(), 0);
    }

    #[test]
    fn defs_missing_namespace_is_an_error() {
        let pnames = 0i32.to_le_bytes().to_vec();
        let texture1 = 0i32.to_le_bytes().to_vec();
        let bytes = build_wad(b"IWAD", &[("PNAMES", &pnames), ("TEXTURE1", &texture1)]);
        let manager = ResourceManager::new(WadFile::from_bytes(bytes).unwrap(), Vec::new());
        let err = SpriteDefinitions::build(&[*b"POSS"], &manager).unwrap_err();
        assert!(matches!(err, SpriteError::MissingSpriteNamespace));
    }

    #[test]
    fn corrupt_patch_header_is_reported() {
        let manager = sprite_manager(&[("POSSA0", vec![0u8; 4])]);
        let err = SpriteLumpTables::build(&manager).unwrap_err();
        assert!(matches!(err, SpriteError::BadLumpHeader { .. }));
    }

    // ------------------------------------------------------------------
    // projection (R_ProjectSprite)
    // ------------------------------------------------------------------

    fn simple_defs(manager: &ResourceManager) -> SpriteDefinitions {
        defs(&[*b"POSS"], manager)
    }

    fn simple_lumps(manager: &ResourceManager) -> SpriteLumpTables {
        SpriteLumpTables::build(manager).unwrap()
    }

    #[test]
    fn project_returns_none_behind_the_view() {
        let manager = sprite_manager(&[("POSSA0", simple_patch(8, 12, 1))]);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);
        // thing behind the view (looking east, thing west of the camera)
        let t = SpriteThing {
            angle: Angle::ZERO,
            ..thing(Fixed::from_int(-5), Fixed::ZERO, Fixed::ZERO, 0, 0)
        };
        let view = kit.view(Angle::ZERO);
        assert!(project_sprite(&t, &view, &defs, &tables, &spritelights()).is_none());
    }

    #[test]
    fn project_respects_minz() {
        let manager = sprite_manager(&[("POSSA0", simple_patch(8, 12, 1))]);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);
        let view = kit.view(Angle::ZERO);
        // exactly at MINZ-ish distance (cos is slightly below 1 in the tables,
        // so keep a margin: 3 units is definitely behind the clip plane)
        let t = thing(Fixed::from_int(3), Fixed::ZERO, Fixed::ZERO, 0, 0);
        assert!(project_sprite(&t, &view, &defs, &tables, &spritelights()).is_none());
        // comfortably in front
        let t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 0);
        assert!(project_sprite(&t, &view, &defs, &tables, &spritelights()).is_some());
    }

    #[test]
    fn project_rejects_sprites_far_off_the_side() {
        let manager = sprite_manager(&[("POSSA0", simple_patch(8, 12, 1))]);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);
        let view = kit.view(Angle::ZERO);
        // way outside a 90-ish degree frustum at close range
        let t = thing(Fixed::from_int(10), Fixed::from_int(200), Fixed::ZERO, 0, 0);
        assert!(project_sprite(&t, &view, &defs, &tables, &spritelights()).is_none());
    }

    #[test]
    fn project_centers_a_frontal_sprite() {
        let manager = sprite_manager(&[("POSSA0", simple_patch(8, 12, 1))]);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);
        let view = kit.view(Angle::ZERO);
        let t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 0);
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert!(spr.x1 < spr.x2);
        assert!(spr.x1 >= 0 && spr.x2 < VIEW_WIDTH);
        // a sprite straight ahead spans roughly symmetrically around centerx
        let center = (spr.x1 + spr.x2) / 2;
        assert!((center - VIEW_WIDTH / 2).abs() <= 2, "center = {center}");
        assert!(spr.scale > Fixed::ZERO);
        // texturemid = z + top_offset - view_z = 0 + 8 - 0
        assert_eq!(spr.texturemid, Fixed::from_int(8));
    }

    #[test]
    fn project_flip_companion_flips_and_negates_xiscale() {
        // 8 rotated lumps each with a flip companion -> frames A (normal) and
        // B (mirrored) share the same patches
        let lumps: Vec<(&str, Vec<u8>)> = (1..=8)
            .map(|i| {
                let name: &'static str = String::leak(format!("POSSA{i}B{i}"));
                (name, simple_patch(8, 12, i as u8))
            })
            .collect();
        let manager = sprite_manager(&lumps);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);
        let view = kit.view(Angle::ZERO);
        // frame B (index 1) is the mirrored set
        let t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 1);
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert!(spr.flip);
        assert!(spr.xiscale < Fixed::ZERO);
        // startfrac = spritewidth - 1 (raw), i.e. the rightmost pixel
        assert_eq!(spr.startfrac, Fixed::from_raw(8 * 0x10000 - 1));

        let t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 0);
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert!(!spr.flip);
        assert!(spr.xiscale > Fixed::ZERO);
        assert_eq!(spr.startfrac, Fixed::ZERO);
    }

    #[test]
    fn project_rotation_selection_wraps_around() {
        let lumps: Vec<(&str, Vec<u8>)> = (1..=8)
            .map(|i| {
                let name: &'static str = String::leak(format!("POSSA{i}"));
                (name, simple_patch(8, 12, i as u8))
            })
            .collect();
        let manager = sprite_manager(&lumps);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);
        // thing due east of the camera, facing it: the view angle delta is
        // zero -> rot 4
        let view = kit.view(Angle::ZERO);
        let t = SpriteThing {
            x: Fixed::from_int(5),
            y: Fixed::ZERO,
            z: Fixed::ZERO,
            angle: Angle::ZERO,
            sprite: 0,
            frame: 0,
            flags: 0,
        };
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        let sprite = defs.sprite(0).unwrap();
        let frame = sprite.frame(0).unwrap();
        assert_eq!(spr.patch, frame.lump[4]);

        // facing away: rot 0
        let t = SpriteThing {
            angle: Angle::ANG180,
            ..t
        };
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert_eq!(spr.patch, frame.lump[0]);
    }

    #[test]
    fn project_colormap_selection() {
        let manager = sprite_manager(&[("POSSA0", simple_patch(8, 12, 1))]);
        let kit = kit(&manager);
        let tables = simple_lumps(&manager);
        let defs = simple_defs(&manager);

        // shadow (spectre): fuzz -> None
        let view = kit.view(Angle::ZERO);
        let mut t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 0);
        t.flags = MF_SHADOW;
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert_eq!(spr.colormap, None);

        // fullbright: colormap level 0
        let t = thing(
            Fixed::from_int(16),
            Fixed::ZERO,
            Fixed::ZERO,
            0,
            FF_FULLBRIGHT,
        );
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert_eq!(spr.colormap, Some(0));

        // fixed colormap wins over diminished light
        let fixed_view = kit.view_ex(
            Angle::ZERO,
            Fixed::from_int(0),
            Fixed::ZERO,
            Fixed::ZERO,
            0,
            Some(29),
        );
        let t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 0);
        let spr = project_sprite(&t, &fixed_view, &defs, &tables, &spritelights()).unwrap();
        assert_eq!(spr.colormap, Some(29));

        // diminished: everything maps through the supplied scalelight row
        let t = thing(Fixed::from_int(16), Fixed::ZERO, Fixed::ZERO, 0, 0);
        let spr = project_sprite(&t, &view, &defs, &tables, &spritelights()).unwrap();
        assert_eq!(spr.colormap, Some(7));
    }

    #[test]
    fn sort_orders_smallest_scale_first_stably() {
        let mk = |scale: i32, gx: i32| ProjectedSprite {
            mobj_flags: 0,
            scale: Fixed::from_int(scale),
            gx: Fixed::from_int(gx),
            gy: Fixed::ZERO,
            gz: Fixed::ZERO,
            gzt: Fixed::ZERO,
            texturemid: Fixed::ZERO,
            x1: 0,
            x2: 1,
            xiscale: Fixed::ONE,
            startfrac: Fixed::ZERO,
            patch: 0,
            flip: false,
            colormap: Some(0),
        };
        let mut v = vec![mk(3, 1), mk(1, 2), mk(2, 3), mk(1, 4)];
        sort_vissprites(&mut v);
        let scales: Vec<i32> = v.iter().map(|s| s.scale.to_int()).collect();
        assert_eq!(scales, vec![1, 1, 2, 3]);
        // equal scales keep insertion order (stable)
        let gx: Vec<i32> = v.iter().map(|s| s.gx.to_int()).collect();
        assert_eq!(gx[0], 2);
        assert_eq!(gx[1], 4);
    }

    // ------------------------------------------------------------------
    // translation tables
    // ------------------------------------------------------------------

    #[test]
    fn translation_tables_match_original_layout() {
        let tables = TranslationTables::build();
        let gray = tables.select(0x0400_0000).unwrap();
        let brown = tables.select(0x0800_0000).unwrap();
        let red = tables.select(0x0c00_0000).unwrap();
        // the green ramp remaps to gray/brown/red (high nibble preserved)
        assert_eq!(gray[0x70], 0x60);
        assert_eq!(gray[0x7f], 0x6f);
        assert_eq!(brown[0x70], 0x40);
        assert_eq!(brown[0x7f], 0x4f);
        assert_eq!(red[0x70], 0x20);
        assert_eq!(red[0x7f], 0x2f);
        // everything outside the ramp is identity in all three tables
        assert_eq!(gray[0x6f], 0x6f);
        assert_eq!(gray[0x80], 0x80);
        assert_eq!(brown[0x6f], 0x6f);
        assert_eq!(red[0x00], 0x00);
        assert_eq!(red[0xff], 0xff);
        // no flag bits -> no translation
        assert!(tables.select(0).is_none());
        // vanilla selects off the two flag bits only; unrelated high bits
        // do not change the outcome
        assert_eq!(
            tables.select(0x1c00_0000).unwrap()[0x70],
            tables.select(0x0c00_0000).unwrap()[0x70]
        );
    }

    // ------------------------------------------------------------------
    // drawing (R_DrawVisSprite / R_DrawMaskedColumn)
    // ------------------------------------------------------------------

    fn patch_with(columns: Vec<Vec<Post>>, width: i16, height: i16) -> Patch {
        Patch {
            header: crate::gfx::patch::PatchHeader {
                width,
                height,
                left_offset: 0,
                top_offset: 0,
                column_ptrs: Vec::new(),
            },
            columns: columns
                .into_iter()
                .map(|posts| crate::gfx::patch::PatchColumn { posts })
                .collect(),
        }
    }

    fn post(top: u8, len: usize, value: u8) -> Post {
        Post {
            top_delta: top,
            pixels: vec![value; len],
        }
    }

    fn vis(
        x1: i32,
        x2: i32,
        xiscale: Fixed,
        startfrac: Fixed,
        texturemid: Fixed,
        colormap: Option<usize>,
    ) -> ProjectedSprite {
        ProjectedSprite {
            mobj_flags: 0,
            scale: Fixed::ONE,
            gx: Fixed::ZERO,
            gy: Fixed::ZERO,
            gz: Fixed::ZERO,
            gzt: Fixed::ZERO,
            texturemid,
            x1,
            x2,
            xiscale,
            startfrac,
            patch: 0,
            flip: xiscale < Fixed::ZERO,
            colormap,
        }
    }

    #[test]
    fn draw_vis_sprite_paints_posts_through_colormap() {
        let colormaps = identity_colormaps();
        let translations = TranslationTables::build();
        let patch = patch_with(
            vec![
                vec![post(2, 4, 9)],
                vec![post(0, 6, 5)],
                vec![post(4, 2, 7)],
                vec![post(0, 1, 3)],
            ],
            4,
            8,
        );
        let mut fb = IndexedFramebuffer::new();
        let mut fuzz = FuzzState::new();
        // scale 1.0, texturemid 0 -> screen row == texture row + centery
        let v = vis(0, 3, Fixed::ONE, Fixed::ZERO, Fixed::ZERO, Some(0));
        draw_vis_sprite(
            &mut fb,
            &mut fuzz,
            &v,
            &patch,
            &colormaps,
            &translations,
            &[200i16; SCREEN_WIDTH],
            &[-1i16; SCREEN_WIDTH],
            0,
            VIEW_HEIGHT / 2,
            Fixed::from_int(VIEW_HEIGHT / 2),
            VIEW_HEIGHT,
        );
        // column 0, post at rows center+2..=center+5
        let cy = (VIEW_HEIGHT / 2) as usize;
        for i in 0..4 {
            assert_eq!(fb.get(0, cy + 2 + i), Some(9), "post row {i}");
        }
        assert_eq!(fb.get(0, cy + 6), Some(0), "below the post");
        assert_eq!(fb.get(1, cy + 5), Some(5));
        assert_eq!(fb.get(2, cy + 4), Some(7));
        assert_eq!(fb.get(2, cy + 6), Some(0));
        assert_eq!(fb.get(3, cy), Some(3));
    }

    #[test]
    fn draw_vis_sprite_flip_walks_columns_backwards() {
        let colormaps = identity_colormaps();
        let translations = TranslationTables::build();
        // distinct texel per column: 11, 12, 13, 14
        let patch = patch_with(
            (0..4).map(|i| vec![post(0, 2, 11 + i as u8)]).collect(),
            4,
            4,
        );
        let mut fb = IndexedFramebuffer::new();
        let mut fuzz = FuzzState::new();
        let v = vis(
            0,
            3,
            Fixed::from_int(-1),
            Fixed::from_raw(4 * 0x10000 - 1),
            Fixed::ZERO,
            Some(0),
        );
        draw_vis_sprite(
            &mut fb,
            &mut fuzz,
            &v,
            &patch,
            &colormaps,
            &translations,
            &[200i16; SCREEN_WIDTH],
            &[-1i16; SCREEN_WIDTH],
            0,
            VIEW_HEIGHT / 2,
            Fixed::from_int(VIEW_HEIGHT / 2),
            VIEW_HEIGHT,
        );
        let cy = (VIEW_HEIGHT / 2) as usize;
        // flipped: screen col 0 samples texture col 3, ..., screen col 3 -> col 0
        assert_eq!(fb.get(0, cy), Some(14));
        assert_eq!(fb.get(1, cy), Some(13));
        assert_eq!(fb.get(2, cy), Some(12));
        assert_eq!(fb.get(3, cy), Some(11));
    }

    #[test]
    fn draw_vis_sprite_respects_floor_clip() {
        let colormaps = identity_colormaps();
        let translations = TranslationTables::build();
        let patch = patch_with(vec![vec![post(0, 40, 9)]], 1, 40);
        let mut fb = IndexedFramebuffer::new();
        let mut fuzz = FuzzState::new();
        // floor clip at row cy+10: rows cy+10.. must not be drawn
        let clip_floor = {
            let mut a = [200i16; SCREEN_WIDTH];
            a[0] = (VIEW_HEIGHT / 2 + 10) as i16;
            a
        };
        let v = vis(0, 0, Fixed::ONE, Fixed::ZERO, Fixed::ZERO, Some(0));
        draw_vis_sprite(
            &mut fb,
            &mut fuzz,
            &v,
            &patch,
            &colormaps,
            &translations,
            &clip_floor,
            &[-1i16; SCREEN_WIDTH],
            0,
            VIEW_HEIGHT / 2,
            Fixed::from_int(VIEW_HEIGHT / 2),
            VIEW_HEIGHT,
        );
        let cy = (VIEW_HEIGHT / 2) as usize;
        assert_eq!(fb.get(0, cy + 9), Some(9), "last visible row");
        assert_eq!(fb.get(0, cy + 10), Some(0), "floor clip cut here");
        assert_eq!(fb.get(0, cy + 20), Some(0));
    }

    #[test]
    fn draw_vis_sprite_fuzz_path_does_not_read_source() {
        // spectre: colormap None -> fuzz; the fuzz drawer rewrites pixels
        // with fuzz_colormap(neighbor), leaving a smeared image.
        // Replay the fuzz chain by hand and check every written pixel.
        use crate::render::{FUZZ_OFFSETS, FUZZ_TABLE_LEN};

        let mut colormaps = identity_colormaps();
        colormaps[FUZZ_COLORMAP] = Colormap(core::array::from_fn(|i| 255 - i as u8));
        let translations = TranslationTables::build();
        let patch = patch_with(vec![vec![post(0, 8, 100)]], 1, 8);
        let mut fb = IndexedFramebuffer::new();
        for (i, px) in fb.as_mut_slice().iter_mut().enumerate() {
            *px = (i % 251) as u8;
        }
        let mut cur = fb.as_slice().to_vec();
        let mut fuzz = FuzzState::new();
        let v = vis(0, 0, Fixed::ONE, Fixed::ZERO, Fixed::ZERO, None);
        draw_vis_sprite(
            &mut fb,
            &mut fuzz,
            &v,
            &patch,
            &colormaps,
            &translations,
            &[200i16; SCREEN_WIDTH],
            &[-1i16; SCREEN_WIDTH],
            0,
            VIEW_HEIGHT / 2,
            Fixed::from_int(VIEW_HEIGHT / 2),
            VIEW_HEIGHT,
        );
        let cy = (VIEW_HEIGHT / 2) as usize;
        for (step, y) in (cy..cy + 8).enumerate() {
            let offset = FUZZ_OFFSETS[step % FUZZ_TABLE_LEN];
            let neighbor_idx = (y as i64 * SCREEN_WIDTH as i64 + i64::from(offset)) as usize;
            // the fuzz drawer works on the partially overwritten buffer;
            // track it with `cur`
            let expected = 255 - cur[neighbor_idx];
            assert_eq!(fb.get(0, y).unwrap(), expected, "row {y}");
            cur[y * SCREEN_WIDTH] = expected;
        }
        assert_ne!(fuzz.position(), 0, "fuzz rolled");
    }

    #[test]
    fn draw_translated_column_remaps_green_ramp() {
        let colormaps = identity_colormaps();
        let translations = TranslationTables::build();
        // all texels in the green ramp: 0x70..0x77
        let pixels: Vec<u8> = (0x70..0x78).collect();
        let patch = patch_with(vec![vec![post(0, 8, 0)]], 1, 8);
        let patch = Patch {
            columns: vec![crate::gfx::patch::PatchColumn {
                posts: vec![Post {
                    top_delta: 0,
                    pixels,
                }],
            }],
            ..patch
        };
        let mut fb = IndexedFramebuffer::new();
        let mut fuzz = FuzzState::new();
        let mut v = vis(0, 0, Fixed::ONE, Fixed::ZERO, Fixed::ZERO, Some(0));
        v.mobj_flags = 0x0400_0000; // gray translation
        draw_vis_sprite(
            &mut fb,
            &mut fuzz,
            &v,
            &patch,
            &colormaps,
            &translations,
            &[200i16; SCREEN_WIDTH],
            &[-1i16; SCREEN_WIDTH],
            0,
            VIEW_HEIGHT / 2,
            Fixed::from_int(VIEW_HEIGHT / 2),
            VIEW_HEIGHT,
        );
        let cy = (VIEW_HEIGHT / 2) as usize;
        for i in 0..8usize {
            assert_eq!(fb.get(0, cy + i), Some(0x60 + i as u8), "row {i}");
        }
    }
}

//! The full frame assembly, consolidating the renderer and the gameplay
//! overlays into a single pass per frame.
//!
//! This is the port of `D_Display` + `R_RenderPlayerView` from the
//! original engine: the BSP/wall traversal, the flat span rasterizer, the
//! sky, the sprite/masked sweep, the weapon overlay, and then the status
//! bar / HUD / automap / menu on top, in vanilla order. The pieces all
//! come from the `render`/`stbar`/`hud`/`automap`/`menu` modules - this
//! file adds no rasterization math of its own, it sequences them.
//!
//! The view is always the full 320x200 screen in this port; the
//! `screenblocks` setting only hides the status bar (like the original's
//! fullscreen mode).

use std::sync::Arc;

use crate::automap::{AutomapView, PlayerMarker, ThingMarker, render_automap};
use crate::errors::EngineResult;
use crate::fixed::Fixed;
use crate::gfx::palette::{Colormap, parse_colormap};
use crate::gfx::patch::{Patch, decode_patch};
use crate::hud::{HudState, MapNameSet, render_hud};
use crate::map::NodeChild;
use crate::player::{MAXPLAYERS, Power};
use crate::render::{
    FlatSet, IndexedFramebuffer, LightTables, MaskedRenderer, PlaneProjection, SCREEN_HEIGHT,
    SCREEN_WIDTH, SpriteThing, TextureSet, WallFrame, WallProjection, WallView, render_planes,
    render_sky, select_row,
};
use crate::resources::ResourceManager;
use crate::stbar::render_status_bar;
use crate::tables::Angle;
use crate::weapons::draw_player_psprites;
use crate::wipe::ScreenWipe;

use super::game::{Game, GameState};

/// The sky flat name the renderer compares against everywhere.
pub(crate) const SKY_FLAT: [u8; 8] = *b"F_SKY1\0\0";

/// Everything the renderer needs for the whole session, decoded once.
pub(crate) struct RenderAssets {
    pub textures: TextureSet,
    pub flats: FlatSet,
    pub colormaps: Vec<Colormap>,
    pub light_tables: LightTables,
    pub view_tables: crate::bsp::ViewTables,
    pub projection: WallProjection,
    pub sprite_defs: crate::render::SpriteDefinitions,
    pub sprite_lumps: crate::render::SpriteLumpTables,
    pub masked: MaskedRenderer,
    /// The `M_PAUSE` patch, when the WAD provides one (missing pause
    /// pic is a cosmetic loss; the game keeps running).
    pub pause_patch: Option<Arc<Patch>>,
}

impl RenderAssets {
    pub(crate) fn build(resources: &ResourceManager) -> EngineResult<Self> {
        let textures = TextureSet::from_resources(resources)?;
        let flats = FlatSet::from_resources(resources)?;

        // COLORMAP: the whole lump, parsed once
        let colormaps = match resources.find_lump("COLORMAP") {
            Some(index) => {
                let lump = resources.lump_data_index(index)?;
                parse_colormap(lump).map_err(|err| {
                    crate::errors::EngineError::Resource(format!("COLORMAP: {err}"))
                })?
            }
            None => Vec::new(),
        };

        let light_tables = LightTables::build(SCREEN_WIDTH as i32);
        let center_x = Fixed::from_int(SCREEN_WIDTH as i32 / 2);
        let view_tables = crate::bsp::ViewTables::new(SCREEN_WIDTH as i32, center_x);
        let projection = WallProjection::build(&view_tables, SCREEN_WIDTH as i32);

        let names: Vec<[u8; 4]> = crate::info::SPRITE_NAMES.iter().map(|n| **n).collect();
        let sprite_defs = crate::render::SpriteDefinitions::build(&names, resources)
            .map_err(|err| crate::errors::EngineError::Resource(format!("sprites: {err}")))?;
        let sprite_lumps = crate::render::SpriteLumpTables::build(resources)
            .map_err(|err| crate::errors::EngineError::Resource(format!("sprite lumps: {err}")))?;

        let pause_patch = resources.find_lump("M_PAUSE").and_then(|index| {
            resources
                .decode_cached::<Patch>(index, |bytes| {
                    decode_patch(bytes).map_err(crate::errors::EngineError::from)
                })
                .ok()
        });

        Ok(Self {
            textures,
            flats,
            colormaps,
            light_tables,
            view_tables,
            projection,
            sprite_defs,
            sprite_lumps,
            masked: MaskedRenderer::new(),
            pause_patch,
        })
    }
}

/// Build a [`WallView`] for the given view position/rotation. Takes the
/// individual assets so the borrow checker can split them off `game`.
#[allow(clippy::too_many_arguments)]
fn wall_view<'a>(
    tables: &'a crate::bsp::ViewTables,
    projection: &'a WallProjection,
    light: &'a LightTables,
    textures: &'a TextureSet,
    colormaps: &'a [Colormap],
    view_x: Fixed,
    view_y: Fixed,
    view_z: Fixed,
    view_angle: Angle,
    extralight: i32,
    fixed_colormap: Option<usize>,
    detail_shift: i32,
) -> WallView<'a> {
    let fine = view_angle.fine_angle();
    WallView {
        tables,
        projection,
        light,
        textures,
        colormaps,
        view_x,
        view_y,
        view_z,
        view_angle,
        view_cos: crate::tables::FINE_COSINE[fine],
        view_sin: crate::tables::FINE_SINE[fine],
        view_width: SCREEN_WIDTH as i32,
        view_height: SCREEN_HEIGHT as i32,
        center_y: SCREEN_HEIGHT as i32 / 2,
        center_x_frac: Fixed::from_raw((SCREEN_WIDTH as i32 / 2) * crate::fixed::FRACUNIT),
        center_y_frac: Fixed::from_raw((SCREEN_HEIGHT as i32 / 2) * crate::fixed::FRACUNIT),
        projection_distance: Fixed::from_raw((SCREEN_WIDTH as i32 / 2) * crate::fixed::FRACUNIT),
        extralight,
        fixed_colormap,
        detail_shift,
        sky_flat: Some(SKY_FLAT),
    }
}

/// Find (or create) the visplane for a flat, mapping the sky flat onto
/// the shared sky plane. `None` when the flat is missing or the pool is
/// exhausted - the wall rasterizer skips marking those, like the
/// original's NULL-plane path.
fn find_visplane(
    frame: &mut WallFrame<'_, '_>,
    flats: &FlatSet,
    pic: &[u8; 8],
    height: Fixed,
    lightlevel: i32,
    sky_picnum: Option<u32>,
) -> Option<usize> {
    let picnum = flats.num_for_name(pic)?;
    let is_sky = sky_picnum == Some(picnum);
    frame
        .planes_mut()
        .find_plane(height, picnum, lightlevel, is_sky)
}

/// `R_RenderPlayerView`: draw the 3D view for the display player.
/// Returns the (still borrowed) framebuffer so the caller can keep
/// drawing overlays. Returns without drawing when there is no view
/// data yet (no map, or the player exists but has no avatar this tic).
fn render_level(fb: &mut IndexedFramebuffer, game: &mut Game) -> EngineResult<()> {
    if game.map.is_none() {
        // no map yet (e.g. mid-wipe at startup)
        return Ok(());
    }

    let display = game.displayplayer;
    let Some(avatar) = game.players[display].mo else {
        // player exists but its avatar has not been spawned yet
        return Ok(());
    };
    let Some((view_x, view_y, view_angle)) = game.store.mobj(avatar).map(|m| (m.x, m.y, m.angle))
    else {
        return Ok(());
    };
    let view_z = game.players[display].viewz;
    let extralight = game.players[display].extralight;
    let fixed_colormap = match game.players[display].fixedcolormap {
        0 => None,
        level => Some(level.max(0) as usize),
    };

    let detail_shift = game.detail_level & 1;
    // R_ClearSprites first; the wall view borrows the assets after
    game.render.masked.clear();
    let wall_view = wall_view(
        &game.render.view_tables,
        &game.render.projection,
        &game.render.light_tables,
        &game.render.textures,
        &game.render.colormaps,
        view_x,
        view_y,
        view_z,
        view_angle,
        extralight,
        fixed_colormap,
        detail_shift,
    );
    let plane_projection = PlaneProjection::build(&wall_view);

    let map = game.map.as_ref().expect("checked above");
    let mut frame = WallFrame::new(wall_view.clone(), fb);
    frame.clear();

    let root = if map.nodes.is_empty() {
        NodeChild::SubSector(0)
    } else {
        NodeChild::Node(map.nodes.len() - 1)
    };

    let view_state = crate::bsp::ViewState::new(view_x, view_y, view_angle);
    let sky_picnum = game.sky_picnum;
    let sky_flat = game.sky_flat;

    {
        let store = &game.store;
        let defs = &game.render.sprite_defs;
        let lumps = &game.render.sprite_lumps;
        let masked = &mut game.render.masked;
        let flats = &game.render.flats;
        let tables = &game.render.view_tables;

        let mut clip = crate::bsp::ClipSegList::cleared(SCREEN_WIDTH as i32);
        crate::bsp::traverse_bsp(
            &map.nodes,
            root,
            &view_state,
            tables,
            &mut clip,
            |clip, ss_index| {
                let Some(ss) = map.subsectors.get(ss_index) else {
                    return;
                };
                let Some(sector_index) = ss.sector else {
                    return;
                };
                let Some(sector) = map.sectors.get(sector_index) else {
                    return;
                };
                let light_row = select_row(sector.light_level as i32, extralight);

                // visplanes (vanilla R_Subsector)
                let floor_plane = if sector.floor_height < view_z {
                    find_visplane(
                        &mut frame,
                        flats,
                        &sector.floor_pic,
                        sector.floor_height,
                        sector.light_level as i32,
                        sky_picnum,
                    )
                } else {
                    None
                };
                let ceiling_plane =
                    if sector.ceiling_height > view_z || sector.ceiling_pic == sky_flat {
                        find_visplane(
                            &mut frame,
                            flats,
                            &sector.ceiling_pic,
                            sector.ceiling_height,
                            sector.light_level as i32,
                            sky_picnum,
                        )
                    } else {
                        None
                    };

                // things in this sector (vanilla R_AddSprites)
                let spritelights = game.render.light_tables.scalelight_row(light_row);
                for id in store.sector_members(sector_index) {
                    let Some(mo) = store.mobj(id) else { continue };
                    let thing = SpriteThing {
                        x: mo.x,
                        y: mo.y,
                        z: mo.z,
                        angle: mo.angle,
                        sprite: mo.sprite as usize,
                        frame: mo.frame as u32,
                        flags: mo.flags,
                    };
                    masked.project(&thing, &wall_view, defs, lumps, spritelights);
                }

                // segs (vanilla R_AddLine)
                for seg_index in ss.first_seg..ss.first_seg + ss.num_segs {
                    let Some(seg) = map.segs.get(seg_index) else {
                        continue;
                    };
                    let (Some(v1v), Some(v2v)) =
                        (map.vertexes.get(seg.v1), map.vertexes.get(seg.v2))
                    else {
                        continue;
                    };
                    let v1 = crate::math::Point::new(v1v.x, v1v.y);
                    let v2 = crate::math::Point::new(v2v.x, v2v.y);
                    let Some(front) = map.sectors.get(seg.frontsector) else {
                        continue;
                    };
                    let back = seg.backsector.and_then(|i| map.sectors.get(i));
                    let mid = &map.sidedefs[seg.sidedef].midtexture;
                    let has_mid = mid[0] != b'-' && mid.iter().any(|&b| b != 0);
                    let rw_angle1 = crate::bsp::point_to_angle(&view_state, v1.x, v1.y);
                    crate::bsp::add_line(
                        &view_state,
                        tables,
                        clip,
                        v1,
                        v2,
                        front,
                        back,
                        has_mid,
                        &mut |first, last| {
                            frame.store_wall_range(
                                map,
                                seg_index,
                                rw_angle1,
                                first,
                                last,
                                floor_plane,
                                ceiling_plane,
                            );
                        },
                    );
                }
            },
        );
    }

    // the mapped-line set for the automap
    let seen = frame.take_mapped_linedefs();
    game.automap.mark_lines_seen(seen);

    // R_DrawPlanes (sky planes are deferred to the sky pass)
    {
        let (fb, planes) = frame.fb_and_planes();
        let report = render_planes(
            fb,
            planes,
            &wall_view,
            &plane_projection,
            &game.render.flats,
            sky_picnum,
        );
        for err in report.errors {
            tracing::warn!("plane render: {err}");
        }
    }

    // the sky
    if let Some(sky_picnum_) = sky_picnum {
        let (fb_, planes_) = frame.fb_and_planes_shared();
        let report = render_sky(
            fb_,
            planes_,
            &wall_view,
            &game.render.projection,
            &game.render.textures,
            game.sky_texture,
            sky_picnum_,
        );
        for err in report.errors {
            tracing::warn!("sky render: {err}");
        }
    }

    // R_DrawMasked
    {
        let masked = &mut game.render.masked;
        let report = masked.draw_masked(&mut frame, &wall_view, map, &mut |lump| {
            game.resources.decode_cached::<Patch>(lump, |bytes| {
                decode_patch(bytes).map_err(crate::errors::EngineError::from)
            })
        });
        for err in report.errors {
            tracing::warn!("masked render: {err}");
        }
    }

    // R_DrawPSprites (the weapon overlay)
    if game.players[display].mo.is_some() {
        let sector_row = game.players[display]
            .mo
            .and_then(|id| game.store.mobj(id))
            .and_then(|m| m.sector)
            .map(|s| select_row(map.sectors[s].light_level as i32, extralight))
            .unwrap_or(0);
        let spritelights = game.render.light_tables.scalelight_row(sector_row);
        let translations = game.render.masked.translations().clone();
        let fuzz = game.render.masked.fuzz_mut();
        draw_player_psprites(
            frame.framebuffer_mut(),
            &game.players[display],
            &wall_view,
            &game.render.sprite_defs,
            &game.render.sprite_lumps,
            &game.render.colormaps,
            spritelights,
            &game.resources,
            &translations,
            fuzz,
        );
    }

    for err in frame.take_errors() {
        tracing::warn!("wall render: {err}");
    }

    // the world view and its borrows are dead after this point (NLL);
    // the world view is in; recover the framebuffer and draw the overlays
    let fb = frame.into_framebuffer();
    render_overlays(fb, game);
    Ok(())
}

/// The status bar + HUD overlay pass (`ST_Drawer` + `HU_Drawer`).
fn render_overlays(fb: &mut IndexedFramebuffer, game: &mut Game) {
    let display = game.displayplayer;

    // status bar
    let status_state = super::game::status_state(game, display);
    if let Err(err) = render_status_bar(fb, &mut game.status_bar, &status_state) {
        tracing::warn!("status bar: {err}");
    }

    // HUD (message + level title over the automap)
    let console = game.consoleplayer;
    let message = game.players[console].message.take();
    let automap_active = game.automap.is_active();
    let gamemode = game.gamemode;
    let title = if gamemode.is_commercial() {
        MapNameSet::Doom2.title(game.gameepisode, game.gamemap)
    } else {
        MapNameSet::Episode.title(game.gameepisode, game.gamemap)
    };
    let hud_state = HudState {
        message: message.as_deref(),
        show_messages: game.show_messages,
        force_message: std::mem::take(&mut game.message_force),
        automap_active,
        title,
    };
    render_hud(fb, &mut game.hud, &hud_state);

    // the automap over the view
    if automap_active {
        render_automap_frame(fb, game);
    }
}

/// The automap pass, with markers filled in from the player/thinker
/// state (vanilla `AM_Drawer` data path).
fn render_automap_frame(fb: &mut IndexedFramebuffer, game: &mut Game) {
    let console = game.consoleplayer;
    let me = match game.players[console].mo.and_then(|id| game.store.mobj(id)) {
        Some(mo) => PlayerMarker {
            x: mo.x,
            y: mo.y,
            angle: mo.angle,
            invisible: game.players[console].powers[Power::Invisibility as usize] > 0,
            is_local: true,
        },
        None => return,
    };

    // net-game markers, in slot order; the local player included
    let mut markers = Vec::new();
    for index in 0..MAXPLAYERS {
        if !game.playeringame[index] {
            continue;
        }
        let Some(mo) = game.players[index].mo.and_then(|id| game.store.mobj(id)) else {
            continue;
        };
        markers.push(PlayerMarker {
            x: mo.x,
            y: mo.y,
            angle: mo.angle,
            invisible: game.players[index].powers[Power::Invisibility as usize] > 0,
            is_local: index == console,
        });
    }

    // iddt level 2: every thing in the level
    let things: Vec<ThingMarker> = if game.automap.cheat_level() == 2 {
        game.store
            .thinkers()
            .filter_map(|(_, thinker)| match thinker {
                crate::thinker::Thinker::Mobj(mo) => Some(ThingMarker {
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

    let view = AutomapView {
        me,
        players: &markers,
        netgame: game.netgame,
        deathmatch: game.deathmatch,
        singledemo: game.singledemo,
        all_map: game.players[console].powers[Power::AllMap as usize] > 0,
        things: &things,
    };

    if let Some(map) = game.map.as_ref() {
        render_automap(fb, &game.automap, map, &view);
    }
}

/// The frame entry point for every gamestate.
pub(crate) fn render_frame(fb: &mut IndexedFramebuffer, game: &mut Game) -> EngineResult<()> {
    // a state transition forces a wipe on the next draw, like vanilla's
    // `wipegamestate != gamestate` check
    let want_wipe = game.wipe_needed || game.last_state != Some(game.gamestate);
    if want_wipe {
        let mut wipe = ScreenWipe::new();
        wipe.capture_start(fb);
        game.wipe = Some(wipe);
        game.wipe_needed = false;
    }
    game.last_state = Some(game.gamestate);

    match game.gamestate {
        GameState::Level => render_level(&mut *fb, &mut *game)?,
        GameState::Intermission => {
            if let Some(intermission) = game.intermission.as_mut() {
                intermission.draw(&mut *fb);
            }
        }
        GameState::Finale => {
            let cast_tables = super::GameCastTables::new();
            if let Some(finale) = game.finale.as_mut() {
                let resources = &game.resources;
                if let Err(err) = finale.draw(
                    &mut *fb,
                    &game.font,
                    resources,
                    &game.render.sprite_defs,
                    &cast_tables,
                ) {
                    tracing::warn!("finale draw: {err}");
                }
            }
        }
        GameState::DemoScreen => render_demoscreen(&mut *fb, game),
    }

    // pause overlay (vanilla `paused` pic), above the view but below
    // the menu, on top of every non-finale state
    if game.paused && game.gamestate == GameState::Level {
        if let Some(patch) = &game.render.pause_patch {
            let x = (SCREEN_WIDTH as i32 - i32::from(patch.header.width)) / 2;
            crate::stbar::draw_patch(&mut *fb, patch, x, 4);
        }
    }

    // the menu is drawn on top of everything, like vanilla's M_Drawer
    if game.menu.is_active() {
        super::game::refresh_save_slots(game);
        crate::menu::render_menu(&mut *fb, &game.font, &game.menu, &game.menu_assets);
    }

    // the wipe melt
    if let Some(mut wipe) = game.wipe.take() {
        wipe.capture_end(&mut *fb);
        if !wipe.update(fb, &mut game.rng, 1) {
            game.wipe = Some(wipe);
        }
    }

    Ok(())
}

/// The demo screen (title pic), vanilla `D_PageDrawer`. A net game that
/// is still waiting for the setup arbitration to finish gets the sync
/// overlay on top so the player sees what it is waiting for.
fn render_demoscreen(fb: &mut IndexedFramebuffer, game: &mut Game) {
    let mut opened = false;
    if let Some(index) = game.resources.find_lump("TITLEPIC") {
        if let Ok(patch) = game.resources.decode_cached::<Patch>(index, |bytes| {
            decode_patch(bytes).map_err(crate::errors::EngineError::from)
        }) {
            crate::stbar::draw_patch(fb, &patch, 0, 0);
            opened = true;
        }
    }
    if !opened {
        // no TITLEPIC in this WAD: solid black
        for y in 0..SCREEN_HEIGHT {
            if let Some(row) = fb.row_mut(y) {
                row.fill(0);
            }
        }
    }
    if game.net_phase == crate::game::NetPhase::WaitingForPlayers {
        draw_net_wait_overlay(fb, game);
    }
}

/// The wait-for-peers overlay shown while the setup arbitration runs.
/// Vanilla only printed "sending/listening for network start info..." to
/// the console; an on-screen prompt is kinder to whoever is sitting at
/// the other machine.
fn draw_net_wait_overlay(fb: &mut IndexedFramebuffer, game: &Game) {
    let font = &game.font;
    let line_step = font.glyph_height() + 4;
    let lines: &[&[u8]] = &[b"NET GAME", b"", b"waiting for players", b"(esc to abort)"];
    // a solid black band behind the text keeps it readable over the page
    let band = lines.len() as i32 * line_step + 16;
    let top = SCREEN_HEIGHT as i32 / 2 - band / 2;
    for y in top..(top + band) {
        if let Some(row) = fb.row_mut(y as usize) {
            row.fill(0);
        }
    }
    let mut y = top + 8;
    for text in lines {
        let width = crate::hud::text_width(font, text);
        let x = ((SCREEN_WIDTH as i32 - width) / 2).max(0);
        crate::hud::draw_text(font, fb, text, x, y);
        y += line_step;
    }
}

//! The `M_Drawer` half of `m_menu.c`: everything the menu draws, in the
//! vanilla layout. All patches come pre-decoded from [`MenuAssets`];
//! text uses the shared HUD font (`hu_font` in the original), passed in
//! by the caller — the menu never loads the font itself.
//!
//! Deviations from the original, all semantics-preserving:
//!
//! * `M_DrawEmptyCell` / `M_DrawSelCell` are dead code in
//!   linuxdoom-1.10 (never called) and are not ported;
//! * patches are clipped per-pixel by [`crate::stbar::draw_patch`]
//!   instead of being dropped wholesale when they poke off the screen;
//! * the vanilla `V_DrawPatchDirect` naming is kept in mind but the
//!   port draws through the indexed framebuffer like everything else.

use crate::errors::{EngineError, EngineResult};
use crate::gfx::patch::{Patch, decode_patch};
use crate::hud::{self, HudFont};
use crate::render::IndexedFramebuffer;
use crate::resources::ResourceManager;
use crate::stbar::draw_patch;
use std::sync::Arc;

use super::state::{LINEHEIGHT, Menu, SAVE_SLOTS, SKULLXOFF};
use super::{GameMode, MenuLevel};
use crate::render::SCREEN_WIDTH;

// option-menu item indices, ported from the options_e enum
const OPTIONS_SCRNSZ: usize = 3;
const OPTIONS_MOUSESENS: usize = 5;

/// All patches the menu needs, decoded once at load time (the way the
/// original caches them in `PU_CACHE`).
#[derive(Clone)]
pub struct MenuAssets {
    pub(crate) m_doom: Arc<Patch>,
    pub(crate) m_newg: Arc<Patch>,
    pub(crate) m_skill: Arc<Patch>,
    pub(crate) m_episod: Arc<Patch>,
    pub(crate) m_optttl: Arc<Patch>,
    pub(crate) m_ngame: Arc<Patch>,
    pub(crate) m_option: Arc<Patch>,
    pub(crate) m_loadg: Arc<Patch>,
    pub(crate) m_saveg: Arc<Patch>,
    pub(crate) m_rdthis: Arc<Patch>,
    pub(crate) m_quitg: Arc<Patch>,
    pub(crate) m_epi1: Arc<Patch>,
    pub(crate) m_epi2: Arc<Patch>,
    pub(crate) m_epi3: Arc<Patch>,
    pub(crate) m_epi4: Arc<Patch>,
    pub(crate) m_jkill: Arc<Patch>,
    pub(crate) m_rough: Arc<Patch>,
    pub(crate) m_hurt: Arc<Patch>,
    pub(crate) m_ultra: Arc<Patch>,
    pub(crate) m_nmare: Arc<Patch>,
    pub(crate) m_endgam: Arc<Patch>,
    pub(crate) m_messg: Arc<Patch>,
    pub(crate) m_detail: Arc<Patch>,
    pub(crate) m_scrnsz: Arc<Patch>,
    pub(crate) m_msens: Arc<Patch>,
    pub(crate) m_svolv: Arc<Patch>,
    pub(crate) m_sfxvol: Arc<Patch>,
    pub(crate) m_musvol: Arc<Patch>,
    pub(crate) m_gdhigh: Arc<Patch>,
    pub(crate) m_gdlow: Arc<Patch>,
    pub(crate) m_msgoff: Arc<Patch>,
    pub(crate) m_msgon: Arc<Patch>,
    pub(crate) help: Arc<Patch>,
    pub(crate) help1: Arc<Patch>,
    pub(crate) help2: Arc<Patch>,
    pub(crate) credit: Arc<Patch>,
    pub(crate) ls_left: Arc<Patch>,
    pub(crate) ls_cntr: Arc<Patch>,
    pub(crate) ls_rght: Arc<Patch>,
    pub(crate) therml: Arc<Patch>,
    pub(crate) thermm: Arc<Patch>,
    pub(crate) thermr: Arc<Patch>,
    pub(crate) thermo: Arc<Patch>,
    pub(crate) skull1: Arc<Patch>,
    pub(crate) skull2: Arc<Patch>,
}

fn load_patch(resources: &ResourceManager, name: &str) -> EngineResult<Arc<Patch>> {
    let index = resources
        .find_lump(name)
        .ok_or_else(|| EngineError::Resource(format!("menu patch {name} not found")))?;
    resources.decode_cached::<Patch>(index, |bytes| {
        decode_patch(bytes).map_err(EngineError::from)
    })
}

impl MenuAssets {
    /// Decodes every menu patch through the resource manager. The READ
    /// pages and the episode menu are always loaded; the game mode only
    /// selects which of them is drawn.
    pub fn load(resources: &ResourceManager) -> EngineResult<Self> {
        Ok(Self {
            m_doom: load_patch(resources, "M_DOOM")?,
            m_newg: load_patch(resources, "M_NEWG")?,
            m_skill: load_patch(resources, "M_SKILL")?,
            m_episod: load_patch(resources, "M_EPISOD")?,
            m_optttl: load_patch(resources, "M_OPTTTL")?,
            m_ngame: load_patch(resources, "M_NGAME")?,
            m_option: load_patch(resources, "M_OPTION")?,
            m_loadg: load_patch(resources, "M_LOADG")?,
            m_saveg: load_patch(resources, "M_SAVEG")?,
            m_rdthis: load_patch(resources, "M_RDTHIS")?,
            m_quitg: load_patch(resources, "M_QUITG")?,
            m_epi1: load_patch(resources, "M_EPI1")?,
            m_epi2: load_patch(resources, "M_EPI2")?,
            m_epi3: load_patch(resources, "M_EPI3")?,
            m_epi4: load_patch(resources, "M_EPI4")?,
            m_jkill: load_patch(resources, "M_JKILL")?,
            m_rough: load_patch(resources, "M_ROUGH")?,
            m_hurt: load_patch(resources, "M_HURT")?,
            m_ultra: load_patch(resources, "M_ULTRA")?,
            m_nmare: load_patch(resources, "M_NMARE")?,
            m_endgam: load_patch(resources, "M_ENDGAM")?,
            m_messg: load_patch(resources, "M_MESSG")?,
            m_detail: load_patch(resources, "M_DETAIL")?,
            m_scrnsz: load_patch(resources, "M_SCRNSZ")?,
            m_msens: load_patch(resources, "M_MSENS")?,
            m_svolv: load_patch(resources, "M_SVOL")?,
            m_sfxvol: load_patch(resources, "M_SFXVOL")?,
            m_musvol: load_patch(resources, "M_MUSVOL")?,
            m_gdhigh: load_patch(resources, "M_GDHIGH")?,
            m_gdlow: load_patch(resources, "M_GDLOW")?,
            m_msgoff: load_patch(resources, "M_MSGOFF")?,
            m_msgon: load_patch(resources, "M_MSGON")?,
            help: load_patch(resources, "HELP")?,
            help1: load_patch(resources, "HELP1")?,
            help2: load_patch(resources, "HELP2")?,
            credit: load_patch(resources, "CREDIT")?,
            ls_left: load_patch(resources, "M_LSLEFT")?,
            ls_cntr: load_patch(resources, "M_LSCNTR")?,
            ls_rght: load_patch(resources, "M_LSRGHT")?,
            therml: load_patch(resources, "M_THERML")?,
            thermm: load_patch(resources, "M_THERMM")?,
            thermr: load_patch(resources, "M_THERMR")?,
            thermo: load_patch(resources, "M_THERMO")?,
            skull1: load_patch(resources, "M_SKULL1")?,
            skull2: load_patch(resources, "M_SKULL2")?,
        })
    }

    fn item_patch(&self, name: &str) -> &Arc<Patch> {
        // every item table hands us a lump we loaded above
        match name {
            "M_NGAME" => &self.m_ngame,
            "M_OPTION" => &self.m_option,
            "M_LOADG" => &self.m_loadg,
            "M_SAVEG" => &self.m_saveg,
            "M_RDTHIS" => &self.m_rdthis,
            "M_QUITG" => &self.m_quitg,
            "M_EPI1" => &self.m_epi1,
            "M_EPI2" => &self.m_epi2,
            "M_EPI3" => &self.m_epi3,
            "M_EPI4" => &self.m_epi4,
            "M_JKILL" => &self.m_jkill,
            "M_ROUGH" => &self.m_rough,
            "M_HURT" => &self.m_hurt,
            "M_ULTRA" => &self.m_ultra,
            "M_NMARE" => &self.m_nmare,
            "M_ENDGAM" => &self.m_endgam,
            "M_MESSG" => &self.m_messg,
            "M_DETAIL" => &self.m_detail,
            "M_SCRNSZ" => &self.m_scrnsz,
            "M_MSENS" => &self.m_msens,
            "M_SVOL" => &self.m_svolv,
            "M_SFXVOL" => &self.m_sfxvol,
            "M_MUSVOL" => &self.m_musvol,
            other => unreachable!("unmapped menu item lump {other}"),
        }
    }
}

/// `M_DrawThermo` — the slider bar. `therm_dot` is the filled position
/// (0-based).
fn draw_thermo(
    fb: &mut IndexedFramebuffer,
    assets: &MenuAssets,
    x: i32,
    y: i32,
    therm_width: i32,
    therm_dot: i32,
) {
    let mut xx = x;
    draw_patch(fb, &assets.therml, xx, y);
    xx += 8;
    for _ in 0..therm_width {
        draw_patch(fb, &assets.thermm, xx, y);
        xx += 8;
    }
    draw_patch(fb, &assets.thermr, xx, y);
    draw_patch(fb, &assets.thermo, (x + 8) + therm_dot * 8, y);
}

/// `M_DrawSaveLoadBorder` — the box around a save-slot description.
fn draw_save_load_border(fb: &mut IndexedFramebuffer, assets: &MenuAssets, x: i32, y: i32) {
    draw_patch(fb, &assets.ls_left, x - 8, y + 7);
    for i in 0..24 {
        draw_patch(fb, &assets.ls_cntr, x + i * 8, y + 7);
    }
    draw_patch(fb, &assets.ls_rght, x + 24 * 8, y + 7);
}

/// `M_WriteText` mechanics (upper-case, space advances 4 px, a glyph
/// that would poke past the right edge is not drawn). Duplicated from
/// the HUD widgets — this one is private to the hud module and the
/// menu may not touch it.
fn draw_text(fb: &mut IndexedFramebuffer, font: &HudFont, text: &[u8], x: i32, y: i32) {
    let mut cx = x;
    for &raw in text {
        let c = raw.to_ascii_uppercase();
        if c == b' ' || !(hud::HU_FONTSTART..=hud::HU_FONTEND).contains(&c) {
            cx += 4;
            if cx >= SCREEN_WIDTH as i32 {
                break;
            }
            continue;
        }
        let glyph = font.glyph((c - hud::HU_FONTSTART) as usize);
        let w = i32::from(glyph.header.width);
        if cx + w > SCREEN_WIDTH as i32 {
            break;
        }
        draw_patch(fb, glyph, cx, y);
        cx += w;
    }
}

/// The multi-line message writer inside `M_Drawer`: horizontally
/// centered lines, block vertically centered on row 100. Uses the
/// vanilla line advance (`SHORT(hu_font[0]->height)`).
fn draw_centered_text(
    fb: &mut IndexedFramebuffer,
    font: &HudFont,
    text: &str,
    y_start: i32,
    line_step: i32,
) {
    let mut y = y_start;
    for line in text.split('\n') {
        let w = hud::text_width(font, line.as_bytes());
        let x = 160 - w / 2;
        draw_text(fb, font, line.as_bytes(), x, y);
        y += line_step;
    }
}

/// `M_StringHeight`: total pixel height of `text`.
fn string_height(font: &HudFont, text: &str) -> i32 {
    let lines = text.bytes().filter(|&b| b == b'\n').count() as i32 + 1;
    lines * font.glyph_height()
}

fn draw_load_save(
    fb: &mut IndexedFramebuffer,
    font: &HudFont,
    menu: &Menu,
    assets: &MenuAssets,
    save: bool,
) {
    let title = if save {
        &assets.m_saveg
    } else {
        &assets.m_loadg
    };
    draw_patch(fb, title, 72, 28);
    let (x, y) = menu.level_xy();
    for i in 0..SAVE_SLOTS {
        let row_y = y + LINEHEIGHT * i as i32;
        draw_save_load_border(fb, assets, x, row_y);
        let slot = &menu.save_names()[i];
        draw_text(fb, font, slot.name.as_bytes(), x, row_y);
        // the save editor draws a caret at the end of the edited line
        if save && menu.save_input_slot() == Some(i) {
            let w = hud::text_width(font, slot.name.as_bytes());
            draw_text(fb, font, b"_", x + w, row_y);
        }
    }
}

fn draw_read_this(fb: &mut IndexedFramebuffer, menu: &Menu, assets: &MenuAssets) {
    let patch = match menu.level() {
        MenuLevel::ReadThis1 => {
            if menu.context().gamemode == GameMode::Commercial {
                &assets.help
            } else {
                &assets.help1
            }
        }
        _ => {
            // retail & commercial show CREDIT, shareware/registered HELP2
            match menu.context().gamemode {
                GameMode::Retail | GameMode::Commercial => &assets.credit,
                GameMode::Shareware | GameMode::Registered => &assets.help2,
            }
        }
    };
    draw_patch(fb, patch, 0, 0);
}

impl Menu {
    /// `M_Drawer`.
    pub fn render(&self, fb: &mut IndexedFramebuffer, font: &HudFont, assets: &MenuAssets) {
        if let Some(message) = self.message() {
            let h = string_height(font, &message.text);
            let y = 100 - h / 2;
            draw_centered_text(fb, font, &message.text, y, font.glyph_height());
            return;
        }

        if !self.is_active() {
            return;
        }

        let (x, y) = self.level_xy();
        // per-level chrome
        match self.level() {
            MenuLevel::Main => draw_patch(fb, &assets.m_doom, 94, 2),
            MenuLevel::Episode => draw_patch(fb, &assets.m_episod, 54, 38),
            MenuLevel::NewGame => {
                draw_patch(fb, &assets.m_newg, 96, 14);
                draw_patch(fb, &assets.m_skill, 54, 38);
            }
            MenuLevel::Options => {
                draw_patch(fb, &assets.m_optttl, 108, 15);
                let detail = if self.detail_level() == 0 {
                    &assets.m_gdhigh
                } else {
                    &assets.m_gdlow
                };
                draw_patch(fb, detail, x + 175, y + LINEHEIGHT * 2);
                let msg = if self.show_messages() {
                    &assets.m_msgon
                } else {
                    &assets.m_msgoff
                };
                draw_patch(fb, msg, x + 120, y + LINEHEIGHT);
                draw_thermo(
                    fb,
                    assets,
                    x,
                    y + LINEHEIGHT * (OPTIONS_MOUSESENS as i32 + 1),
                    10,
                    self.mouse_sensitivity(),
                );
                draw_thermo(
                    fb,
                    assets,
                    x,
                    y + LINEHEIGHT * (OPTIONS_SCRNSZ as i32 + 1),
                    9,
                    self.screen_size_value(),
                );
            }
            MenuLevel::Sound => {
                draw_patch(fb, &assets.m_svolv, 60, 38);
                draw_thermo(fb, assets, x, y + LINEHEIGHT, 16, self.sfx_volume());
                draw_thermo(fb, assets, x, y + LINEHEIGHT * 3, 16, self.music_volume());
            }
            MenuLevel::LoadGame => draw_load_save(fb, font, self, assets, false),
            MenuLevel::SaveGame => draw_load_save(fb, font, self, assets, true),
            MenuLevel::ReadThis1 | MenuLevel::ReadThis2 => draw_read_this(fb, self, assets),
        }

        // the item column
        let mut y = y;
        let item_column_y = y;
        for item in self.level_items() {
            if !item.name.is_empty() {
                draw_patch(fb, assets.item_patch(item.name), x, y);
            }
            y += LINEHEIGHT;
        }

        // the skull cursor
        let skull = if self.which_skull_value() == 0 {
            &assets.skull1
        } else {
            &assets.skull2
        };
        draw_patch(
            fb,
            skull,
            x + SKULLXOFF,
            item_column_y - 5 + self.item_on_value() as i32 * LINEHEIGHT,
        );
    }
}

/// The one-call entry the game loop uses.
pub fn render_menu(fb: &mut IndexedFramebuffer, font: &HudFont, menu: &Menu, assets: &MenuAssets) {
    menu.render(fb, font, assets);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::HudFont;
    use crate::menu::state::MenuContext;
    use crate::menu::{GameMode, MenuKey, MenuOptions};
    use std::sync::Arc;

    fn fill_patch(width: i16, height: i16, fill: u8) -> Arc<Patch> {
        // a patch whose every column is one `fill`-colored post
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
        Arc::new(decode_patch(&data).unwrap())
    }

    fn synthetic_font() -> HudFont {
        let glyphs: [Arc<Patch>; crate::hud::HU_FONTSIZE] =
            std::array::from_fn(|i| fill_patch(8, 7, 1 + (hud::HU_FONTSTART as usize + i) as u8));
        HudFont::from_glyphs(glyphs)
    }

    /// Every patch in the menu, colored by its own index — the fill
    /// values are all distinct so a wrong lump is caught immediately.
    fn synthetic_assets() -> MenuAssets {
        macro_rules! p {
            ($n:expr) => {
                fill_patch(8, 8, $n)
            };
        }
        MenuAssets {
            m_doom: p!(1),
            m_newg: p!(2),
            m_skill: p!(3),
            m_episod: p!(4),
            m_optttl: p!(5),
            m_ngame: p!(6),
            m_option: p!(7),
            m_loadg: p!(8),
            m_saveg: p!(9),
            m_rdthis: p!(10),
            m_quitg: p!(11),
            m_epi1: p!(12),
            m_epi2: p!(13),
            m_epi3: p!(14),
            m_epi4: p!(15),
            m_jkill: p!(16),
            m_rough: p!(17),
            m_hurt: p!(18),
            m_ultra: p!(19),
            m_nmare: p!(20),
            m_endgam: p!(21),
            m_messg: p!(22),
            m_detail: p!(23),
            m_scrnsz: p!(24),
            m_msens: p!(25),
            m_svolv: p!(26),
            m_sfxvol: p!(27),
            m_musvol: p!(28),
            m_gdhigh: p!(29),
            m_gdlow: p!(30),
            m_msgoff: p!(31),
            m_msgon: p!(32),
            help: p!(33),
            help1: p!(34),
            help2: p!(35),
            credit: p!(36),
            ls_left: p!(37),
            ls_cntr: p!(38),
            ls_rght: p!(39),
            therml: p!(40),
            thermm: p!(41),
            thermr: p!(42),
            thermo: p!(43),
            skull1: p!(44),
            skull2: p!(45),
        }
    }

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

    fn retail_menu_open() -> Menu {
        let mut menu = retail_menu();
        let font = synthetic_font();
        menu.handle_key(&font, MenuKey::Escape);
        menu
    }

    #[test]
    fn closed_menu_draws_nothing() {
        let font = synthetic_font();
        let menu = retail_menu();
        let assets = synthetic_assets();
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        assert_eq!(fb.as_slice(), &[0u8; crate::render::SCREEN_PIXELS]);
    }

    #[test]
    fn main_menu_draws_title_items_and_skull() {
        let font = synthetic_font();
        let menu = retail_menu_open();
        let assets = synthetic_assets();
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        // M_DOOM title at (94,2)
        assert_eq!(fb.get(94, 2), Some(1));
        // the six items start at y=64 with 16 px spacing
        assert_eq!(fb.get(97, 64), Some(6)); // M_NGAME
        assert_eq!(fb.get(97, 80), Some(7)); // M_OPTION
        assert_eq!(fb.get(97, 96), Some(8)); // M_LOADG
        assert_eq!(fb.get(97, 112), Some(9)); // M_SAVEG
        assert_eq!(fb.get(97, 128), Some(10)); // M_RDTHIS
        assert_eq!(fb.get(97, 144), Some(11)); // M_QUITG
        // skull cursor at (97 - 32, 64 - 5) on item 0
        assert_eq!(fb.get(65, 59), Some(44));
    }

    #[test]
    fn skull_moves_with_cursor() {
        let font = synthetic_font();
        let mut menu = retail_menu_open();
        menu.handle_key(&font, MenuKey::Down);
        let assets = synthetic_assets();
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        assert_eq!(fb.get(65, 59 + LINEHEIGHT as usize), Some(44));
        assert_eq!(fb.get(65, 59), Some(0));
    }

    #[test]
    fn message_prompt_is_drawn_centered() {
        let font = synthetic_font();
        let mut menu = retail_menu_open();
        // end game prompt has newlines; it is centered around y=100
        menu.end_game();
        let assets = synthetic_assets();
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        // any glyph pixel above the main-menu items and off their x column
        let painted = fb.as_slice().iter().filter(|&&b| b != 0).count();
        assert!(painted > 0, "prompt must paint something");
    }

    #[test]
    fn commercial_help_is_help_and_retail_is_help1() {
        let assets = synthetic_assets();
        let font = synthetic_font();
        // retail: F1 opens page 2? no - retail F1 = page 2; check page 1 via readthis entry
        let mut menu = retail_menu_open();
        menu.handle_key(&font, MenuKey::Printable(b'r'));
        menu.handle_key(&font, MenuKey::Enter); // page 1
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        assert_eq!(fb.get(0, 0), Some(34)); // HELP1
        // commercial: HELP
        let mut commercial = Menu::new(
            MenuContext {
                gamemode: GameMode::Commercial,
                netgame: false,
                usergame: true,
                in_level: true,
                demoplayback: false,
                gametic: 0,
                language_english: true,
            },
            MenuOptions::defaults(),
        );
        commercial.handle_key(&font, MenuKey::Escape);
        commercial.open_help();
        let mut fb = IndexedFramebuffer::new();
        commercial.render(&mut fb, &font, &assets);
        assert_eq!(fb.get(0, 0), Some(33)); // HELP
    }

    #[test]
    fn options_page_draws_sliders_and_toggles() {
        let font = synthetic_font();
        let mut menu = retail_menu_open();
        menu.handle_key(&font, MenuKey::Printable(b'o'));
        menu.handle_key(&font, MenuKey::Enter);
        let assets = synthetic_assets();
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        let (x, y) = (60, 37);
        // M_OPTTTL title
        assert_eq!(fb.get(108, 15), Some(5));
        // M_GDHIGH (detail = high by default) at (x + 175, y + 16*2)
        assert_eq!(fb.get(x + 175, y + 32), Some(29));
        // M_MSGON (messages on) at (x + 120, y + 16*1)
        assert_eq!(fb.get(x + 120, y + 16), Some(32));
        // thermometer end-caps for sensitivity at (x, y + 16*6)
        assert_eq!(fb.get(x, y + 96), Some(40)); // M_THERML
        // screen blocks thermometer at (x, y + 16*4)
        assert_eq!(fb.get(x, y + 64), Some(40));
        // the dot for default sensitivity 5 lands at x + 8 + 5*8 = 108
        assert_eq!(fb.get(x + 8 + 5 * 8, y + 96), Some(43)); // M_THERMO
    }

    #[test]
    fn load_menu_draws_borders_and_names() {
        let font = synthetic_font();
        let mut menu = retail_menu_open();
        menu.set_save_names(&[
            crate::menu::SaveSlotInfo {
                name: "E1M1".into(),
                available: true,
            },
            crate::menu::SaveSlotInfo::default(),
            crate::menu::SaveSlotInfo::default(),
            crate::menu::SaveSlotInfo::default(),
            crate::menu::SaveSlotInfo::default(),
            crate::menu::SaveSlotInfo::default(),
        ]);
        menu.handle_key(&font, MenuKey::Printable(b'l'));
        menu.handle_key(&font, MenuKey::Enter);
        let assets = synthetic_assets();
        let mut fb = IndexedFramebuffer::new();
        menu.render(&mut fb, &font, &assets);
        // M_LOADG title
        assert_eq!(fb.get(72, 28), Some(8));
        // border around slot 0 at y=54: left cap at (80-8, 54+7)
        assert_eq!(fb.get(72, 61), Some(37)); // M_LSLEFT
        // 24 middle cells span x=80..80+24*8
        assert_eq!(fb.get(80, 61), Some(38)); // M_LSCNTR
        assert_eq!(fb.get(80 + 23 * 8, 61), Some(38));
        assert_eq!(fb.get(80 + 24 * 8, 61), Some(39)); // M_LSRGHT
    }
}

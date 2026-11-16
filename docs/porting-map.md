# Porting Map — linuxdoom-1.10 → doom-engine

File-by-file disposition of the original C source tree into the new Rust
engine. Module paths in the table are the landed modules (flat, direct
children of the crate root); see `docs/architecture.md` §3 for the full
as-built map. Target modules live in the `doom-engine` library crate
(`engine/`, crate name `doom-engine`, lib name `doom_engine`).

Columns: **Original file** | **Target module** | **Decision / notes**.

Only four outcomes exist:

- **port** — rewrite in Rust with the same behavior; the original remains the
  behavioral reference.
- **port & change** — keep the behavior, change the mechanism (e.g. pointers →
  arena indices, function-pointer unions → enums).
- **drop** — remove; superseded by Rust/stdlib or by a different decision; the
  original behavior it provided is not needed.
- **split/merge** — content reorganized across modules; each part is listed.

Every count below is exhaustive — all 124 C source files are accounted for,
grouped by subsystem. Non-source files (build system, readmes) are covered in
group 0.

---

## 0. Build system and repo meta (not source, 10 files)

| Original file | Target | Notes |
|---|---|---|
| `Makefile` | — | **drop**. Replaced by Cargo workspace; per-target quirks (`-lm`, X11 SHM linkage) vanish with the platform layer. |
| `ChangeLog` | — | **drop**. Historical; superseded by git history. |
| `FILES`, `FILES2` | — | **drop**. File inventory from the original packaging; this document is the inventory now. |
| `TODO` | — | **drop**. Stale upstream notes. |
| `README.asm`, `README.b`, `README.book`, `README.gl` | — | **drop**. Platform READMEs for SunOS/Irix/GL ports; not applicable. |
| `README.sound` | `docs/` | **port (as doc)**. Useful historical sound notes; fold relevant parts into the sound module docs if needed. |
| `CVS/` (directory) | — | **drop**. Version-control metadata from the original import. |

## 1. Core primitives, types, fixed-point math (11 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `doomtype.h` | `info`, `player`, `menu` (split) | **port & change**. `boolean`/`byte`/`MININT` typedef soup removed; Rust `bool`, `u8`, `i32::MIN`. Enums became typed `#[repr]` items on the landed modules listed in the target column. |
| `doomdef.h` | `info`, `player`, `menu` (split) | **port & change**. Gamemode/mission/language/skill/card/weapon/ammo/power/key enums, `MAXPLAYERS`, `TICRATE`, `SCREENWIDTH/HEIGHT`, `RANGECHECK`→ debug assertions, `SNDSERV` — dropped (no sndserver). Constants become typed enums with `repr` where wire format matters. |
| `doomdef.c` | `info`, `player`, `menu` (split) | **port & change**. The `char*` banner/title strings dropped — the shell has its own startup logs; remaining logic is trivial. |
| `doomstat.c` | `game` | **port & change**. P_/Net/etc. globals collapse into the `Game` state structs (`game::game`, `game::hooks`); volume vars live in the audio layer. |
| `doomstat.h` | `game` | **port & change**. As above; `MAXNETNODES` lives in `net`. |
| `m_fixed.h` | `fixed` | **port & change**. `fixed_t` → `Fixed(i32)` newtype; `FRACBITS`/`FRACUNIT` as associated consts. |
| `m_fixed.c` | `fixed` | **port**. `FixedMul` via i64 intermediate; `FixedDiv` with original saturating semantics; `FixedDiv2` kept as internal step. Unit tests for overflow edges. |
| `m_swap.h` | — | **drop**. `SHORT/LONG` byte-swap macros replaced by `from_le_bytes`/`to_le_bytes`. Endianness helper module only if needed. |
| `m_swap.c` | — | **drop**. As above. |
| `m_random.h` | `rng` | **port & change**. `rndtable[256]` committed as-is; `int rndindex` → state in a `Prng` struct; `P_Random` (game) vs `M_Random` (cosmetic) split preserved — demos depend on the split. |
| `m_random.c` | `rng` | **port**. Same LUT, same wrap (`&0xff`), `M_ClearRandom`. |
| `m_bbox.h` | `math` | **port**. `M_ClearBox`, `M_AddToBox` as free functions on `[Fixed;4]`/bbox type; `BOXRIGHT/BOXLEFT/BOXTOP/BOXBOTTOM` consts. |
| `m_bbox.c` | `math` | **port**. As above; used by BSP and blockmap queries. |
| `tables.h` | `tables` | **port & change**. `FINEANGLES`, `FINEMASK`, `ANGLETOFINESHIFT`, `ANG45/90/180/270`, `SLOPERANGE/SLOPEBITS/DBITS`, BAM `angle_t` → `Angle(u32)`; `finesine`/`finecosine`/`finetangent`/`tantoangle` declared here are data in `tables.c` (→ build-time generated `const` arrays). |
| `tables.c` | `tables` | **port & change**. LUT generation: either committed generated consts or a build-script generator + golden test against the committed values. Also `SlopeDiv` (port as-is, it is load-bearing for `R_PointToAngle`). |

## 2. Static data tables and text (10 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `info.h` | `info` | **port & change**. `state_t`/`mobjinfo_t`/`spritedef_t` structures → generated Rust const arrays; `NUMSPRITES`, state indices become `enum`s (`MobjType`, `SprNum`) where practical. Function pointers (`actionf_p1`) → action enum consumed by the ticker dispatch. |
| `info.c` | `info` | **port & change**. ~4.5k lines of generated tables: sprnames, states, mobjinfo. Mechanical port; validated by round-trip tests (spawn every mobj type, tick it). Keep order identical. |
| `sounds.h` | `info` | **port & change**. `sfxinfo_t`/`musicinfo_t` → Rust structs; prefixes table folded in. |
| `sounds.c` | `info` | **port**. `S_sfx[]` and `S_music[]` (MUS lump names: `mus_e1m1`…) ported as const data; lump lookup by name unchanged. |
| `d_items.h` | `weapons` | **port & change**. `weaponinfo`/`ammotype_t` tables, backpack rules — gameplay-relevant data; merged into game tables module. |
| `d_items.c` | `weapons` | **port**. `weaponinfo[]` with ammo/maxammo rules (incl. backpack multipliers). |
| `dstrings.h` | `hud`, `menu`, `finale` (split) | **port & change**. Message string constants; long-term i18n story unspecified — keep English strings in one table. |
| `dstrings.c` | `hud`, `menu`, `finale` (split) | **port**. As above. |
| `d_englsh.h` | `hud`, `menu`, `finale` (split) | **port & change**. English message strings (menu text, intermission). Lives as `hud::messages`, `menu::strings` and the finale text tables - never merged into one file on purpose. |
| `d_french.h` | `hud`, `menu`, `finale` (split) | **port & change**. French strings; same treatment. Translation completeness is not a blocker for 1.0. |

## 3. Application bootstrap, CLI, config, globals (10 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `i_main.c` | `doom-rs` binary (shell) | **port & change**. Trivial `main()`; replaced by a clap-parsed shell CLI: platform flags first, everything else passes through to the engine's vanilla-style parser. |
| `d_main.c` | `doom-rs` + `game` | **port & change**. `D_DoomMain`/`D_DoomLoop` → shell entry + engine (`game::Game`); `D_DoomLoop` tick/present orchestration ports; `D_Display`/`D_PageDrawer`/demo-advance sequence port; response-file handling, `IdentifyVersion` (IWAD probing), `-shdev/-regdev/-comdev` dev flags **drop** (id-internal dev paths). `-turbo`, `-warp`, `-skill`, `-episode`, `-record`, `-playdemo`, `-timedemo`, `-loadgame`, `-file`, `-nomonsters`, `-respawn`, `-fast`, `-altdeath`, `-deathmatch`, `-timer`, `-avg`, `-statcopy` → keep the gameplay ones; `-statcopy` profiler dots **drop**; `-devparm` logs "development mode ON"; frame stats live behind F8 (any build). |
| `d_main.h` | — | **drop**. Declarations only; replaced by module APIs. |
| `doomdef.h` | `info`, `player`, `menu` (split) | see group 1. |
| `doomdef.c` | `info`, `player`, `menu` (split) | see group 1. |
| `doomstat.c` | `game` | see group 1. |
| `doomstat.h` | `game` | see group 1. |
| `m_argv.h` | `doom-rs` (shell) | **port & change**. Tiny; replaced by a minimal `Args` struct (we keep manual parsing for exact compat, no clap dependency for the game itself). |
| `m_argv.c` | `doom-rs` (shell) | **port**. `M_CheckParm` semantics preserved (exact `-flag` match); `@responsefile` expansion kept — cheap, occasionally used. |
| `m_misc.h` | `config` | **port**. Default config machinery (key bindings, mouse sensitivity, sound vols, gamma, screensize/detail). |
| `m_misc.c` | `config` | **port & change**. `default.cfg` parser kept (format preserved); `M_SaveDefaults` on quit; `M_ScreenShot` (pcx writer) → replaced by PNG/PPM screenshot via the render module (no PCX encoder; document the change). Gamma slider, cheat dispatch to `m_cheat` kept. |

## 4. WAD and zone memory (4 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `w_wad.c` | `wad` | **port & change**. Directory parse (IWAD/PWAD magic, `numlumps`, `infotableofs`) with full bounds validation; multi-file stacking; backwards-scan lookup (last wins); 8-byte name compare. `W_Reload`/reloadable-lump hack **dropped** (see architecture.md). `W_Profile` debug dump **dropped**. `W_CacheLump*` splits: lookup stays here (`WadDirectory`), caching lives in the resource manager. |
| `w_wad.h` | `wad` | **port & change**. `lumpinfo_t` → `LumpInfo`; the `lumpcache` global is superseded by the resource manager's decode-once cache (`resources` module); `PU_*` tags not ported. |
| `z_zone.c` | — | **drop**. Zone allocator (rover, purgable blocks, `Z_CheckHeap`) replaced by Rust ownership + the resource manager's decode-once cache (`resources` module). Debug heap dump (`Z_DumpHeap`) → keep as a debug utility inside `resources` if benchmarks/QA ask for it, otherwise drop. |
| `z_zone.h` | — | **drop**. `PU_STATIC/PU_LEVEL/PU_CACHE` tags are not ported: lifetime is ownership, not tags; the `Thinker` registry owns thinkers directly. |

## 5. Map format and level loading (4 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `doomdata.h` | `map` | **port & change**. On-disk structs (`mapvertex_t…mapthing_t`, `ML_*` markers, `ML_BLOCKING`… line flags, `NF_SUBSECTOR`) → packed/parsed structs with explicit little-endian reads; runtime structs use i32 fixed where original did. `ML_*` flags → `bitflags`-style newtype (no extra dependency; a simple `u16` + consts). |
| `p_setup.h` | `map` | **port**. Lump order in a map (label, THINGS, LINEDEFS, SIDEDEFS, VERTEXES, SEGS, SSECTORS, NODES, SECTORS, REJECT, BLOCKMAP) is validated up front by `MapLumps`; level load goes through `Map::load`. |
| `p_setup.c` | `map` | **port & change**. `P_LoadVertexes/Segs/Subsectors/Sectors/Nodes/Things/LineDefs/SideDefs/BlockMap` port directly with endian safety; pointer fixups (`line->frontsector` etc.) become indices; `P_GroupLines` ports (sidenum → `SideDefId`, sector link resolution, bbox precompute, slopetype); BSP vs node split handling (`*_nodes` vs DeePBSP) — parse standard format; non-standard GL nodes **out of scope**. Things get `ThingType` validated against skill flags. |
| `p_local.h` | — | **drop**. Internal cross-includes of the playloop; replaced by module imports. `P_SpawnMapThing`, `P_LoadThings` protos absorbed by `map`. |

## 6. Software renderer (22 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `r_main.c` | `render` | **port & change**. `R_PointToAngle(2)`, `R_PointToDist`, `R_ScaleFromGlobalAngle` → `bsp`/`render`; `viewangletox`/`xtoviewangle`/`clipangle`/`validcount` → `bsp::view` (`ViewTables`: clipangle and friends) and `render::wall` (`WallProjection`); validcount became per-call sets; `R_InitTextureMapping/LightTables/Tables` → init fns; `R_SetViewSize`/`R_ExecuteSetViewSize` → `PlaneProjection::build` per view; `R_RenderPlayerView` → `game::render::render_frame`. |
| `r_main.h` | `render` | **port & change**. SINGLETONS → state in `RenderState`. `colfunc`/`spanfunc` fn pointers → enum dispatch (`ColumnDrawer::Normal/Fuzz/Translated/Low`). |
| `r_bsp.c` | `bsp` | **port**. `R_Subsector/R_RenderBSPNode/R_CheckBBox/R_AddLine` port directly; `solidsegs`/`cliprange_t` → sorted `Vec<ClipRange>` in clip state; `drawsegs`/`ds_p` → `Vec<DrawSeg>`; sentinel init (`-0x7fffffff…viewwidth`) kept. |
| `r_bsp.h` | `bsp` | **port**. Clip range types, drawseg visibility helpers. |
| `r_segs.c` | `render` | **port**. `R_StoreWallRange`, scale calculation, `R_RenderWallRange`, `R_RenderSegLoop` (the per-column textured-unclipped-part loop: pegging, `rw_light`, masked middle for 2-sided, `colfrac` stepping). Highest-risk hot loop; port carefully, validate against golden frames. |
| `r_segs.h` | `render` | **port**. |
| `r_plane.c` | `render` | **port & change**. Visplane find/merge, `R_FindPlane`, `R_DrawPlanes` (span math: `yslope`, `xtoviewangle`, `distscale`, `finecosine`, zlight selection), sky plane special-case. `visplanes[]` array → `Vec` with the original 128 hard cap (error on overflow, as original). |
| `r_plane.h` | `render` | **port**. `R_MapPlane` span descriptor. |
| `r_things.c` | `render` | **port & change**. `R_InitSprites/R_ClearSprites/R_ProjectSprite/R_AddSprites/R_DrawMaskedYugoslavia` — `vissprite_t` → `VisSprite`; xsort by scale (`R_SortVisSprites` — uses `skill` ordering quirks, port exactly); fuzz for `MF_SHADOW`; `R_DrawMasked` (drawsegs masked textures → vissprites → psprite) ordering kept; translation table for player sprites; `R_DrawPSprite` for weapon viewmodels. |
| `r_things.h` | `render` | **port**. `MAXVISSPRITES=128` kept as const. |
| `r_draw.c` | `render` | **port**. `R_DrawColumn`, `R_DrawColumnLow`, `R_DrawFuzzColumn` (with 50-entry `fuzzoffset`), `R_DrawSpan`, `R_DrawSpanLow`. `R_DrawTranslatedColumn`/`R_InitTranslationTables` (player-sprite colorramp translation for net games) **not ported** - cosmetic, out of scope. `R_FillBackScreen`/`R_DrawViewBorder` not needed - the view fully repaints every frame. Bounds are clamped per pixel; panics forbidden. |
| `r_draw.h` | `render` | **port**. |
| `r_data.c` | `gfx` | **port & change**. Texture/flat/sprite/colormap loading: `R_InitTextures` (TEXTURE1/2+PNAMES composite), `R_InitFlats`, `R_InitSpriteLumps` (name parsing `NNNNFxFx`, `R_InitSpriteDefs`, rotation/flip logic), `R_InitColormaps`, `R_FlatNumForName`, `R_TextureNumForName`. `R_PrecacheLevel` **not ported** - textures and flats are built lazily on first use (decode-once cache). Compositing algorithm preserved (post lists, masks); all offsets bounds-checked. |
| `r_data.h` | `gfx` | **port**. `texture_t`/`patch_t`/`flatmapping_t` descriptors. |
| `r_sky.c` | `render` | **port**. `R_InitSkyMap` (SKY1.. skytexture + `skytexturemid`); one sky texture per map; column-aligned with world angle. |
| `r_sky.h` | `render` | **port**. |
| `r_defs.h` | `map`, `render` | **split**. Geometry structs (vertex/sector/side/line/subsector/seg/node) → `map`; render structs (`drawseg_t`, `visplane_t`, `vissprite_t`, `spritedef_t`, `spriteframe_t`) → `render`. Delete unused `patch_t` here — patch lives in `gfx`. |
| `r_state.h` | `render` | **port & change**. All externs → fields of the render state structures (`WallFrame`, `PlaneSet`, view/projection structs). |
| `r_local.h` | — | **drop**. Aggregator header; replaced by module imports. |
| `v_video.c` | `render` | **split**. `gammatable` → `render::gamma`; screen buffer management → `render::framebuffer` (owned `IndexedFramebuffer`, palette applied at output, gamma at present time in the shell); patch drawing stays in `gfx`; palette upload happens in the shell (softbuffer). |
| `v_video.h` | `render` | **port & change**. `SCREENWIDTH/HEIGHT` → `render::framebuffer::{SCREEN_WIDTH, SCREEN_HEIGHT}`. |
| `d_textur.h` | `gfx` | **port & change**. `mappatch_t`/`maptexture_t`/`maptexturedef_t` (TEXTURE1 on-disk) → parsed structs in `gfx`. |

## 7. Gameplay core (23 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `g_game.c` | `game` | **port & change**. `G_BuildTiccmd` (two-stage keyboard turn, mouse double-click use, `dclicktime`, weapon keys), `G_Ticker`/`G_PlayerTicker`, demo record/playback (`G_RecordDemo`/`G_DoPlayDemo`/`G_TimeDemo` + `G_Read/WriteDemoTiccmd`), `G_InitNew`/`G_DeferedInitNew`/`G_DoCompleted`/`G_DoWorldDone`/intermission handoff, `G_LoadGame/G_SaveGame/G_DoLoadGame/G_DoSaveGame`, `G_PlayerReborn`, `G_DeathMatchSpawnPlayer`, `G_ScreenShot`. `consistency` checksum → port for net compat. Save format per architecture.md §10 (not raw .dsg). |
| `g_game.h` | `game` | **port & change**. `gameaction_t`, `gamestate_t` (→ `GameState`), `wminfo`, `statcopy` **dropped**. |
| `d_event.h` | `game` + shell input | **split**. Event enum (`KeyDown`/`KeyUp`/`Mouse`/`Joy`) shared; `buttoncode_t`, `MAXEVENTS` ring → `game`. Shell translates winit events into this type. |
| `d_ticcmd.h` | `player` | **port**. `Ticcmd { forwardmove: i8, sidemove: i8, angleturn: i16, consistency: i16, chatchar: u8, buttons: u8 }`; `BT_*` masks. Serialization order identical for demo compat. |
| `d_player.h` | `player` | **port & change**. `player_t` (view bob, powers, cards, frags, weapon/ammo, pspr, message, centering) → `Player`; `playerstate_t`, `cheat_t` (CF_GOD/NOCLIP). `player_t.cmd` embedded → stored per tic in `Ticcmd` history instead of the player (saves megabytes of redundancy). |
| `d_think.h` | `thinker` | **port & change**. Thinker list → enum + arena (architecture.md §4.2); `think_t` union gone. |
| `d_items.c/h` | `weapons` | see group 2. |
| `m_cheat.h` | `menu` | **port**. Cheat table (idclev, idkfa, idclip, idbehold*, idbeholds/s, idbeholdi…, idbeholdv/h/i, idbeholdr) — sequences + mask with wildcards. |
| `m_cheat.c` | `menu` | **port**. Wildcard matcher, console print fallback (cheats echo to HUD message like original). |
| `p_tick.h` | `game` | **port**. |
| `p_tick.c` | `game` | **port & change**. `P_Ticker` (pause/menu gate, player think, thinker run, `P_UpdateSpecials`, `P_RespawnSpecials`, `leveltime++`), thinker add/run/remove — lazy unlink semantics preserved. |
| `p_mobj.h` | `mobj` | **port**. `mobj_t` (~50 fields), `mapthing_t` respawn table, `MF_*` flags, `MobjThinker` kind. |
| `p_mobj.c` | `mobj` | **port & change**. `P_SpawnMobj`, `P_RemoveMobj` (unlink from sector lists — doubly-linked sector mobj list becomes `SectorId → Vec<MobjId>` or intrusive-equivalent indices), `P_XYMovement`/`P_ZMovement` (momentum, friction, `STOPSPEED`, gravity, floor/ceiling collision, `P_CheckMissileSpawn`, missile explode, `P_ExplodeMissile`), `P_NightmareRespawn`, `P_RespawnSpecials`, `P_SpawnPlayer`, `P_SpawnMapThing` (things filter by skill/density/deathmatch, ambush flags). |
| `p_map.c` | `movement` | **split**. Movement/collision: `P_CheckPosition` (blockmap traversers `PIT_CheckLine`, `PIT_CheckThing`, `PIT_StompThing` for lost souls/spawns), `P_SlideMove` + `P_HitSlideLine` (slide along walls), `P_XYMovement` helpers; hitscan/aim: `P_AimLineAttack`, `PTR_AimTraverse`, `P_LineAttack`, `PTR_ShootTraverse`, `P_UseLines` (`PTR_UseTraverse`), `PIT_RadiusAttack` (splash damage), `PIT_ChangeSector` (crushing/floor mover blocking). All port; the `validcount` guards became per-call sets. |
| `p_sight.c` | `enemy` | **port**. `P_CheckSight` — BSP-based LOS with `REJECT` matrix short-circuit; it computes its own bbox/angle spans (no shared table with `r_bsp.c`); module-local port, `validcount` guards became a per-call set. |
| `p_maputl.c` | `movement` | **port & change**. `P_LineOpening` (open/ceiling/floor heights per window), `P_UnsetThingPosition/SetThingPosition/P_BlockThingsIterator/P_BlockLinesIterator` (blockmap grid walk; `bmaporgx/y`, negative coords handling), `P_DelSegnomonicTest` gone (unused), `PIT_AddThingIntercepts`, `P_PathTraverse`, `P_UseLines` support, `P_FindAboveHighestCeiling`-adjacent helpers → here or `specials` (sector-height search helpers like `P_FindLowestFloorSurrounding` live with specials). |
| `p_user.c` | `player` | **port**. `P_CalcHeight` (view bob, squat, step-up), `P_XYMovement`-player counterpart (`P_MovePlayer`), `P_PlayerThink` (weapon select, centering tic, `P_CalcSwing` hook, dead view follow, `player->mo->z` crouch differences in BOOM not supported — classic rules only). |
| `p_enemy.c` | `enemy` | **port & change**. Monster AI wholesale: `A_Look/A_Chase/P_Move/P_TryWalk/P_NewChaseDir` (dirty trick: `DI_NODIR` table reuse, port exactly), all attack actions (`A_PosAttack`…`A_BrainSpit`), `P_CheckMeleeRange/P_CheckMissileRange` (range tables), `P_Move` direction logic incl. `DI_EAST…` and door opening (`P_UseSpecialLine` with junk line — port the junk-line idiom as a scratch `LineDef`), infighting (`A_TargetPlayer` equivalents inside chase code), nightmare respawn. `A_BossDeath` (boss death floors) stays here; `A_Explode` splash ports. |
| `p_pspr.c` | `weapons` | **port**. Player psprites: `P_SetupPsprites`, `P_MovePsprites`, `P_BringUpWeapon`, `P_CheckAmmo` (weapon-specific ammo incl. super shotgun), `P_FireWeapon`, `P_DropWeapon`, `A_*` weapon actions, `A_BFGSpray` (spawn 40 tracers — uses fixed RNG streams, port exactly), `P_CalcSwing`, `P_BulletSlope` (vertical aim for screen blocks), weapon priority when firing multiple. |
| `p_pspr.h` | `weapons` | **port**. `pspdef_t`, `ps_*` flags, pending weapon change states. |
| `p_inter.h` | `inter` | **port**. `P_KillMobj`, `P_DamageMobj` decls. |
| `p_inter.c` | `inter` | **port**. `P_KillMobj` (corpse sprite, drops, kill count/intermission, boss→exit checks hook), `P_DamageMobj` (armor save, pain states, infighting trigger, god/invulnerability checks). Pickup logic (`P_TouchSpecialThing`) ports; dehacked-name ammo constants go to `weapons`. |
| `p_saveg.h` | `game` | **port**. |
| `p_saveg.c` | `game` | **port & change**. `P_ArchivePlayers/World/Thinkers/Specials/Sectors` structure → binary writer with version tag; `PADSAVEP` alignment moot (indices, not raw structs); archive order documented; `T_*` type tags only for thinkers — matched by thinker enum variant tag. Round-trip property tests required (save → load → state equality on a scripted session). |

## 8. Sector movers and linedef specials (9 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `p_doors.c` | `specials` | **port**. `T_VerticalDoor` (open/close/blaze/close30, `vld_t` phases), `T_SlidingDoor` (Strife-style, used by DOOM II), `P_SpawnDoorCloseIn30/60`, `P_InitSlidingDoorFrames`, `EV_DoDoor`, `EV_DoLockedDoor` (card check via `P_CheckCards` in `inter` — dependency note), `P_NextDoor`. |
| `p_floor.c` | `specials` | **port**. `T_MoveFloor` (raise/lower/turbo, crush damage, `raiseFloor32` chains), `EV_DoFloor`, `EV_DoDonut` lives in p_spec but dispatched via switch - **port to `specials`**. |
| `p_plats.c` | `specials` | **port**. `T_PlatRaise` (downWaitUpStay etc.), `EV_DoPlat`, `P_ActivateInStasis`, `EV_StopPlat`, `EV_DoDonut` helper shared. |
| `p_ceilng.c` | `specials` | **port**. `T_MoveCeiling` (fastCrushAndRaise, crushAndRaise, lowerAndCrush, lowerToFloor), `EV_DoCeiling`, `EV_CeilingCrushStop`, active ceiling list. |
| `p_lights.c` | `specials` | **port**. `T_FireFlicker`, `T_LightFlash`, `T_StrobeFlash`, `T_Glow` (+ `EV_StartLightStrobing`, `EV_TurnTagLightsOff`, `EV_LightTurnOn`). Spawn hooks called from level load (`P_SpawnSpecials`). |
| `p_switch.c` | `specials` | **port**. `P_InitSwitchList` (both DOOM and DOOM II switch textures incl. sw_xtable with SFX ids), switch activation by special (`P_SpecialLine` for one-use/retriggerable), `P_ChangeSwitchTexture` for one-shot buttons. |
| `p_telept.c` | `specials` | **port**. `EV_Teleport`, fog TFOG spawn at both ends, source-sector `MF_NOCLIP` trick preserved behaviorally. |
| `p_spec.c` | `specials` | **port & change**. Landed as one module (`lineactions.rs`): `P_Find*FloorSurrounding`-style query helpers live in `specials::movers`/`util`; the line-special dispatch tables (use/cross/shoot) live in `specials::lineactions`; `P_SpawnSpecials`/`P_UpdateSpecials` live there too. `P_InitPicAnims`/`P_AnimateSurfaces` (flat/texture animation) are **not ported** — animated surfaces run on identity tables (see docs/compatibility.md, "Animated flats and textures are not ported"). |
| `p_spec.h` | `specials` | **port & change**. Mover struct defs move next to their modules; shared consts (`FLOORSPEED` etc.) stay here. |

## 9. In-game UI and screens (18 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `st_stuff.c` | `stbar` | **port**. Status bar full port: face anim (invulnerability/pain/evil-eyes), health/armor/ammo number drawing, `ST_*` states, chat bind to `hu_stuff` queue, mugshot on pain/death, key cards, weapons. `-nobar` equivalent flag supported. |
| `st_lib.h` | `stbar` | **port**. `st_number_t`-style glyph drawing helpers (`STlib_*`) — small widget layer kept. |
| `st_lib.c` | `stbar` | **port**. Number/string drawing on the backscreen. |
| `st_stuff.h` | `stbar` | **port**. `st_statusbar_t` state. |
| `hu_stuff.c` | `hud` | **port & change**. HUD: message queue, chat char queue (`HU_queueChatChar`), level name on entry, `AM`/"live stats" (time/secret/arms), cheat echo, `HU_Move`, `HU_TargetName`. Font table `hu_font` (not in C, generated from patches at init) — generate from WAD lumps at runtime like original. |
| `hu_stuff.h` | `hud` | **port**. |
| `hu_lib.c` | `hud` | **port**. `hu_textline_t` erase/draw — backscreen-erasing widget library; used by chat and HUD. |
| `hu_lib.h` | `hud` | **port**. |
| `m_menu.c` | `menu` | **port & change**. Full menu tree (main/options/new-game/episodes/skill/load/save/end-game/quit/gamma/sound) — biggest UI file; replace message strings with table; mouse cursor support ports; slider widgets port. `M_ScreenShot` reference dropped (handled in `config`). |
| `m_menu.h` | `menu` | **port**. `menuactive`, skull animation timers. |
| `am_map.c` | `automap` | **port & change**. Automap: pan/zoom/follow, marks (`AM_addMark`), grid, line drawing via VGA-ish plot into view (scale shifts), `cheater` overlay; color scheme same lump names (`AMMNUM` etc.); mark count 10 (max marks) kept. |
| `am_map.h` | `automap` | **port**. |
| `wi_stuff.c` | `intermission` | **port & change**. Intermission screens: DOOM I (episode end) and DOOM II (single-episode) — the full intermission state machine (`WI_EndLevel`, stats counting anim, `WI_Drawer`), background NUKE/bunny page draw. Depends on WAD `INTERPIC`/`DMENINT*`/`WIN*` lumps; the hardcoded epi/map name tables (`mapnames[]`) port as data. |
| `wi_stuff.h` | `intermission` | **port**. |
| `f_finale.c` | `finale` | **port & change**. Episode final text scroll + cast call (`F_StartCast`, `F_CastTicker` with fixed sprite order), bunny scroll (`F_BunnyScroll`), `F_TextWrite`. `V_DrawPatchFlipped` for finale mirror patch ports. |
| `f_finale.h` | `finale` | **port**. `F_*` states driven by `gamestate` GS_FINALE. |
| `f_wipe.c` | `wipe` | **port**. Pixel melt: 320 columns descending at staggered speeds, `wipe_Melt` implementation, `wipe_StartScreen/EndScreen/ScreenWipe` state machine driven from the shell's frame loop; timing via constants, deterministic for a fixed RNG seed. |
| `f_wipe.h` | `wipe` | **port**. |

## 10. Sound system (4 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `s_sound.c` | `audio` | **port & change**. Positional SFX subsystem: `S_Start` (channel init, 8 channels default — keep), `S_StartSound(origin, id)` (priority, distance clipping at 1200 units, `S_CLOSE_DIST` = 160, stereo separation by angle, pitch perturb ±2 for mobj sounds), `S_AdjustSoundParams`, `S_UpdateSounds` per tic, `S_StopSound`, `S_PauseSound/S_ResumeSound`, `S_StartMusic/S_StopMusic/S_ChangeMusVolume`, external listener-switch rules. `S_Init` takes `(snd_SfxVolume, snd_MusicVolume)`. Sound origins are mobj pointers → `MobjId` (or raw sound source ids). |
| `s_sound.h` | `audio` | **port**. Channel/sfx bookkeeping types; `SfxId` in the `info` module keys the SFX table. |
| `i_sound.c` | `audio` + shell | **split**. (1) Real-time mixer: 8-bit unsigned PCM @ 11025 Hz, nearest resample → device rate, stereo with per-channel `vol/sep`, clipping to i16 — full port, self-contained, no OS code. (2) Linux 8-bit driver hooks, `/dev/dsp` path, sound-timer install, IRQ-based submit — **drop**, replaced by cpal pull-stream. (3) `I_SoundIsPlaying`, `I_StopSound`, `I_SetChannels` → mixer API. (4) music calls → the `music` module (`opl-audio`; MUS parser in-tree; DMX envelope synthesis). (5) `I_UpdateSound` semantics kept: the shell pumps music samples per frame and the SFX mix happens inside the cpal callback. |
| `i_sound.h` | `audio` | **port & change**. `sndDevice_t`, `sound_lump` info — replaced by the channel model in `audio::mixer` and SFX ids from `info`. |

## 11. Platform layer and networking (9 files)

| Original file | Target module | Decision / notes |
|---|---|---|
| `i_system.c` | `doom-rs` (shell) | **split**. Timing: `I_GetTime` (70 Hz wall base) → shell wall-clock (`Game::advance(elapsed_ms)`; the net layer takes a `TimeSource`). `I_ZoneBase` gone with the zone allocator. `I_BaseTiccmd` → the shell builds ticcmds from input. `I_Quit`, `I_Error` (fatal abort) → shell error path (`EngineError` → process exit with cleanup). `I_AllocLow`, `I_WaitVBL`, `I_Tactile` (unused) → **drop**. `I_GetHeapSize` → diagnostics only. |
| `i_system.h` | `doom-rs` | **rewrite**. Traits not needed - the shell is concrete: winit `ApplicationHandler` + softbuffer surface + one cpal stream. |
| `i_video.c` | `doom-rs` (shell) | **drop (replaced)**. X11/MIT-SHM window code, keysym translation, `XShmPutImage`, 8-bit PseudoColor palette upload, `-2/-3/-4` scale flags, `-grabmouse` → all replaced by winit+softbuffer (keyboard map in shell; palette applied at present time). Gamma handled in `render::gamma`. `I_ReadScreen` → not ported (headless tests capture the indexed framebuffer directly). Reference behavior kept only in spirit. |
| `i_video.h` | `doom-rs` | **drop (replaced)**. No trait - the shell calls winit/softbuffer directly; the engine renders into a shell-owned `IndexedFramebuffer`. |
| `i_net.c` | `net` + shell | **split**. `I_InitNetwork` (command-line net launch: `-net` / `DOOMPORT 5029`, `doomcom` setup, consoleplayer assignment, `numnodes`) → shell net bootstrap using `std::net::UdpSocket`; `PacketSend/PacketGet`, `UDPsocket`, `GetLocalAddress` (broadcast addr) → `net`. Port the setup-arbitration packets (`NCMD_SETUP`) with checksums so vanilla-compatible LAN play is possible; `doomcom` structure → `NetConfig`. |
| `i_net.h` | `doom-rs` | **port**. Command-line net params, `DOOMCOM_ID`. |
| `d_net.c` | `net` | **port & change**. `TryRunTics`, `NetUpdate`, `GetPackets`, `HSendPacket/HGetPacket` (with the unchecksummed `NORMALUNIX` shortcut **dropped** — always checksum), `ExpandTics`, `D_ArbitrateNetStart`, `D_CheckNetGame`, `D_QuitNetGame` (with `NCMD_EXIT` handshake), `CheckAbort` (ESC to abort net wait), `reboundpacket` loopback for single player, `resendcount`/`remoteresend`/`resendto` resend logic, `ticdup`, `maxsend`. `NetbufferChecksum` → `u32` wrapping sum (not `&NCMD_CHECKSUM` masking beyond protocol). Protocol constants (`NCMD_*`, `BACKUPTICS=12`, `MAXNETNODES=8`) preserved. |
| `d_net.h` | `net` | **port**. `netcmd_t`/`doomcom_t` equivalents. |
| `i_main.c` | `doom-rs` | see group 3 (trivial entry; listed here for completeness). |

---

## Coverage check

Counts per group: 11 (core) + 10 (tables) + 10 (app) + 4 (wad/zone) + 4 (map)
+ 22 (renderer) + 23 (gameplay) + 9 (specials) + 18 (UI) + 4 (sound) + 9 (platform/net)
= **124** source files. All files accounted for; every file assigned to exactly
one group.

## Key decisions carried into the port

- **Workspace layout**: virtual workspace — `engine/` (library `doom_engine`),
  `doom-rs/` (binary shell), `tools/{wad-inspector,asset-inspector,demo-analyzer}`.
  Tools are workspace members, path-depend on `doom-engine`.
- **winit + softbuffer** for window and present; **cpal** for audio output;
  **criterion** for benchmarks. (See `docs/architecture.md` §14.)
- **Resource loading** is synchronous and decode-once (lump data decoded at
  most once per session, cached per (type, lump); no background threads,
  no precache pass). The engine stays synchronous and deterministic.
- **Fixed-point + BAM + LUT trig only**; no floats in the simulation, ever.
  Floats exist only in the audio output path (mixer position, OPL render).
- **Arena (index) data model**, not pointers; `validcount` became per-call
  sets where it is still needed; the thinker list is an enum + generational
  arena.
- **Determinism boundary**: only `Ticcmd` sequences enter the simulation;
  wall-clock read lives at the shell boundary (`Game::advance(elapsed_ms)`);
  the net layer injects a `TimeSource` in tests; the golden determinism
  tests (`engine/tests/determinism.rs`) verify it.
- **`z_zone` is deleted**, not ported: Rust ownership + the resource
  manager's decode-once cache replaces it.
- **X11/MIT-SHM code is deleted**, not ported: winit/softbuffer replaces it.
- **Dev-only paths** (`-shdev`, `-regdev`, `-comdev`, `-wart`, `-statcopy`,
  reloadable `~` lumps) are dropped.
- **Savegame format** is a new versioned binary (index-based), not a `.dsg`
  byte clone; demos (`.lmp`) are byte-compatible by construction.

## Decisions locked in

1. **OPL for music**: `opl-audio` 0.1.0 (pure Rust, OPL2), in-process
   synthesis via `MusicEngine`; GENMIDI bank; a documented fallback bank
   for WADs without `GENMIDI`.
2. **Savegame container**: `DOOMSAV\0` magic + version byte + description,
   index-based sections (players, world, thinkers, specials, sectors);
   atomic write through the scratch dir. Vanilla `.dsg` compatibility is
   out of scope.
3. **Netcode transport**: trait (`NetTransport`) with a UDP implementation
   (port 5029, vanilla packet format) shipped; internet matchmaking is
   out of scope for 1.0.
4. **Async**: none anywhere; threads only if ever needed, never inside
   the engine.
5. **Resource cache**: decode-once cache keyed by (type, lump) with
   `CacheStats`; no purge-on-demand, no LRU - the working set is bounded
   by the IWAD.
6. **LAN packet checksum**: the vanilla DOS checksum algorithm is kept;
   the Linux zero-checksum shortcut was dropped (interoperability with
   the source that actually shipped).

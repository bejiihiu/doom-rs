# DOOM-rs Engine Architecture

Modern rewrite of the DOOM engine (originally id Software, 1993, GPL) in Rust.
The reference for behavior is the original C source tree (`linuxdoom-1.10`);
wherever this document and the original disagree, the original is the spec.
The goal is behavioral fidelity first — same simulate-then-render pipeline,
same fixed-point math, same gametic model — with a modern, safe, modular
Rust implementation on top.

This document describes the target architecture. It is the map; each module
is expected to land incrementally and converge to this shape.

---

## 1. Repository and workspace layout

Virtual Cargo workspace at the repository root. There is no root package;
members live flat at the top level.

```
doominrust
├── Cargo.toml              # virtual workspace manifest, resolver = "3"
├── engine/                 # doom-engine: the engine as a library (lib name: doom_engine)
│   ├── Cargo.toml
│   └── src/
├── doom-rs/                # doom-rs: the runnable binary (thin shell)
│   ├── Cargo.toml
│   └── src/                # main.rs, app.rs, input.rs, audio.rs
├── tools/
│   ├── wad-inspector/      # CLI: dump WAD directory, validate lumps
│   ├── asset-inspector/    # CLI: render PLAYPAL/patches/flats/sprites to PNG
│   └── demo-analyzer/      # CLI: parse .lmp demo files, dump ticcmds
└── docs/
```

Rationale:

- **The engine is a library, the binary is a shell.** All game logic lives in
  `doom-engine` and never names a windowing/audio/OS API directly. The shell
  wires traits to real backends. This keeps the engine headless-testable
  (snapshot tests, determinism runs, benches) without a display.
- **Tools are separate binaries** sharing `doom-engine` for parsing, so the
  inspectors are honest consumers of the same code the game uses.

Dependencies (deliberate, minimal):

| Crate                       | Role                                        | Notes                                                                 |
| --------------------------- | ------------------------------------------- | --------------------------------------------------------------------- |
| `winit`                     | window creation, keyboard/mouse events      | cross-platform, no unsafe needed                                      |
| `softbuffer`                | present the framebuffer as a window surface | no GPU dependency; exact fit for a 320×200 paletted renderer          |
| `cpal`                      | audio stream callback                       | the mixer is ours; cpal only moves buffers                            |
| `criterion`                 | benchmarks                                  | fixed-point mul, table lookup, WAD parse, BSP walk, full frame render |
| `thiserror`                 | error types                                 | engine error enums, no `String` errors deep in the stack              |
| `tracing`                   | diagnostics/logging                         | structured logs instead of `printf`                                   |
| `opl-audio` (or equivalent) | OPL FM emulation for MUS music              | open decision, see §14                                                |

No `tokio` for the engine. The shell may use plain threads (see §5); adding an
async runtime is only justified if resource loading actually benefits.

## 2. High-level view

```
                 doom-rs (binary)
  ┌──────────┬───────────┬───────────┬─────────────┐
  │ CLI      │ winit     │ cpal      │ std::fs     │
  │ + config │ + softbuf │ callback  │ (WAD/demo/  │
  │          │           │           │  savegames) │
  └──────────┴───────────┴───────────┴─────────────┘
                    ▲ platform traits
  ┌────────────────────────────────────────────────┐
  │              doom-engine (library)             │
  │                                                │
  │  core ── fixed point, angles, LUT trig, PRNG   │
  │  wad  ── WAD directory, lump IO, cache         │
  │  map  ── map lump parsing, runtime geometry    │
  │  bsp  ── BSP traversal, clipping               │
  │  render ─ software renderer (columns, spans)   │
  │  game ── ticcmd, P_Ticker, thinkers, mobjs     │
  │  special ─ linedef actions (doors/floors/...)  │
  │  ui   ── status bar, HUD, menu, automap        │
  │  sound ─ SFX channel mixer, positional audio   │
  │  net  ── d_net/i_net: ticcmd sync over UDP     │
  │  save ── savegame + demo (.lmp) IO             │
  └────────────────────────────────────────────────┘
```

## 3. Module map inside `doom_engine`

Module names are short and flat; `doom_engine::render::bsp` rather than
`doom_engine::renderer::bsp::builder`. Grouped by subsystem:

| Module             | Replaces                                                                  | Responsibility                                                                                                                                                   |
| ------------------ | ------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `core::fixed`      | m_fixed.c                                                                 | 16.16 fixed point: `Fixed`, `FixedMul`, `FixedDiv`, overflow semantics                                                                                           |
| `core::angle`      | tables.c (angle parts), r_main.c (angle utils)                            | 32-bit BAM (`Angle`), `ANG90`…, `FINEANGLES`, finesine/finecosine/finetangent/tantoangle LUTs, `SlopeDiv`, `PointToAngle`                                        |
| `core::gamma`      | v_video.c (gammatable)                                                    | 5×256 gamma LUTs, applied at palette upload                                                                                                                      |
| `core::random`     | m_random.c                                                                | deterministic LUT PRNG: `P_Random` (gameplay) / `M_Random` (cosmetic), `M_ClearRandom`                                                                           |
| `core::math`       | m_bbox.c, m_swap.c, r_main.c (side tests)                                 | bbox ops, `PointOnSide`/`PointOnSegSide`, `PointToDist`, endian helpers                                                                                          |
| `core::types`      | doomtype.h, doomdef.h/c                                                   | shared enums: gamemode, mission, skill, language, cards, weapons, ammo, powers, keys                                                                             |
| `core::error`      | (scattered `I_Error`)                                                     | `EngineError` enum via `thiserror`; fatal vs recoverable                                                                                                         |
| `wad`              | w_wad.c                                                                   | WAD header/directory parse, IWAD/PWAD stacking (last lump wins), name lookup (8-byte, case-insensitive), lump IO, validation                                     |
| `resource`         | z_zone.c, W_CacheLump\*                                                   | `ResourceManager`: purgable cache with level/static tags; replaces the zone allocator with Rust ownership + an explicit purgable cache                           |
| `gfx::pallete`     | r_data.c (colormaps), v_video.c                                           | PLAYPAL/COLORMAP parse, gamma application, translation tables (player sprites)                                                                                   |
| `gfx::patch`       | r_data.c                                                                  | patch_t (column posts), composite textures from TEXTURE1/TEXTURE2 + PNAMES, flats table, sprite lump index                                                       |
| `map`              | p_setup.c, p_local.h, doomdata.h                                          | map lump parsing (THINGS…BLOCKMAP/REJECT), geometry structs, runtime line sidedef/sector resolution, blockmap, reject matrix                                     |
| `bsp`              | r_bsp.c                                                                   | solid/pass clipping (`ClipRange` list), `AddLine`, `CheckBBox`, recursive `RenderBSPNode`                                                                        |
| `render`           | r\_\*.c, r_defs.h, r_state.h, v_video.c                                   | framebuffer (`screens[5]` → owned `Framebuffer`), columns, spans, view setup, `drawsegs`, `visplanes`, `vissprites`, sky, palette upload path                    |
| `render::view`     | r_main.c (R_RenderPlayerView)                                             | per-frame setup: view origin/angle/bob, fixed colormap, clear buffers                                                                                            |
| `game::state`      | doomstat.c/h, d_main.c (gamestate)                                        | `GState`: gamestate/gameaction/gametic/leveltime, wipe state, demo state, global flags                                                                           |
| `game::ticcmd`     | d_ticcmd.h, G_BuildTiccmd                                                 | `Ticcmd` build from input, buttons masks, consistency checks                                                                                                     |
| `game::ticker`     | g_game.c, p_tick.c                                                        | `P_Ticker`, thinker registry (add/run/remove), `G_Ticker`, gametic advance                                                                                       |
| `game::player`     | d_player.h, p_user.c                                                      | `Player`, view bob calculation, weapon psprites state                                                                                                            |
| `game::mobj`       | p_mobj.c/h, d_think.h                                                     | `Mobj` (actors), spawn/lifecycle/momentum, `degenmobj` (sound origins)                                                                                           |
| `game::collision`  | p_map.c, p_maputl.c, p_sight.c                                            | `P_CheckPosition`, `P_SlideMove`, hitscan, `PIT_*`/`PTR_*` traversers, line opening, blockmap iterators                                                          |
| `game::ai`         | p_enemy.c                                                                 | monster AI: look/chase/attack, all `A_*` action functions                                                                                                        |
| `game::weapons`    | p_pspr.c                                                                  | pspr (player sprite) logic, all player-weapon actions, `A_BFGSpray`, etc.                                                                                        |
| `game::items`      | p_inter.c, d_items.c                                                      | damage, kills, pickup logic, keys, powers, weapon/ammo tables                                                                                                    |
| `game::save`       | p_saveg.c                                                                 | savegame (.dsg-compatible layout where practical) and demo (.lmp) read/write                                                                                     |
| `special::*`       | p_doors, p_floor, p_plats, p_ceilng, p_lights, p_switch, p_telept, p_spec | moving sectors (doors, floors, plats, ceilings), lighting effects, switches, teleports, linedef action dispatch (`EV_*`)                                         |
| `special::anim`    | p_spec.c (pic anims)                                                      | flat/texture animation tables                                                                                                                                    |
| `ui::statusbar`    | st_stuff.c, st_lib.c                                                      | status bar: health/armor/ammo, face, keys, numbers                                                                                                               |
| `ui::hud`          | hu_stuff.c, hu_lib.c                                                      | HUD: message lines, chat, level name, time/secrets/items/kill counters                                                                                           |
| `ui::menu`         | m_menu.c                                                                  | menu screens, options, new game, save/load                                                                                                                       |
| `ui::automap`      | am_map.c                                                                  | automap: pan/zoom/follow/marks                                                                                                                                   |
| `ui::intermission` | wi_stuff.c                                                                | intermission screens + stats                                                                                                                                     |
| `ui::finale`       | f_finale.c                                                                | end-of-episode text, cast call                                                                                                                                   |
| `ui::wipe`         | f_wipe.c                                                                  | melt wipe transitions                                                                                                                                            |
| `sound`            | s_sound.c                                                                 | positional SFX: channel allocation, distance attenuation, separation, pitch, linked sounds                                                                       |
| `sound::mixer`     | i_sound.c (mixer part)                                                    | 8-bit PCM → device-rate mix into stereo i16; no OS dependency                                                                                                    |
| `net`              | d_net.c, i_net.c                                                          | ticcmd exchange: `NetUpdate`, `TryRunTics`, `ExpandTics`, UDP socket adapter behind a trait                                                                      |
| `tables`           | info.c/h, sounds.c/h, tables.c/h, dstrings.c/h, d_englsh.h, d_french.h    | generated static tables: states, mobjinfo, sprnames, sfxinfo, musicinfo, messages                                                                                |
| `app`              | i_main.c, d_main.c, m_argv.c, m_misc.c                                    | startup: CLI parse, config (default.cfg), IWAD detection, `D_DoomMain`/`D_DoomLoop` equivalents — lives in the **shell**, not the engine, except reusable pieces |

`app` is intentionally borderline: pure startup logic (CLI, IWAD probing)
belongs in `doom-rs`; only things the engine needs (e.g. config defaults as
data) stay in `doom-engine`.

## 4. Data model

### 4.1 Arena storage, index-based references

The C pointer web (`vertex_t *`, `line_t *`, `sector_t *`) becomes
**index-based arenas** inside `map`:

```rust
pub struct Map {
    pub vertices: Vec<Vertex>,
    pub segs: Vec<Seg>,
    pub subsectors: Vec<Subsector>,
    pub nodes: Vec<Node>,
    pub sectors: Vec<Sector>,
    pub sidedefs: Vec<SideDef>,
    pub lines: Vec<LineDef>,
    pub blockmap: Vec<i16>,
    pub reject: Vec<u8>,
}
```

`VertexId(u32)`, `SectorId(u16)`, etc. This is faster than pointer chasing
(cache-friendly), trivially serializable for savegames, and removes the
aliasing problems the C original worked around with `validcount`/`Z_ChangeTag`.
`validcount` becomes an epoch counter on items that need it (`Sector::sound_traversed`,
line side tests) or is simply replaced by borrow discipline.

### 4.2 Thinkers

The C doubly-linked thinker list with a union of function pointers becomes a
closed enum dispatched by the ticker:

```rust
pub enum ThinkerKind {
    Mobj(MobjId),
    Door(DoorThinker),
    Floor(FloorThinker),
    Plat(PlatThinker),
    Ceiling(CeilingThinker),
    LightFlicker(..), /* ... */
    None, // pending removal
}
```

A `Vec<ThinkerKind>` plus a free-list, drained each tic. Removal is lazy and
deterministic (same semantics as the original: the function pointer is set to
a sentinel and the entry is unlinked on its turn).

### 4.3 Game state ownership

All mutable globals from `doomstat.h` collapse into typed structs
(`GState`, `RenderState`, `WadCache`, `SfxState`, …) owned by a top-level
`Engine` value. No `static mut`, no global singletons. Interior mutability
only in the audio mixer ring (atomics) and the resource cache (`Mutex` where
the loader thread touches it).

### 4.4 Framebuffer

`screens[0]` (320×200 paletted bytes) is owned as `Framebuffer` in the render
module. The shell converts to RGB(A) via the current palette (+gamma) at
present time. Background screens used by border drawing/HUD erase live in the
same buffer; the `dirtybox`/`V_CopyRect` machinery is reduced to what the
HUD/status bar actually needs.

## 5. Threads and where work happens

DOOM-rs uses **one thread of simulation** and as few support threads as the
platform forces on us.

| Thread                           | Runs                                                                                                        | Notes                                                                            |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| **main thread**                  | window events → input queue; game tic stepping (35 Hz); rendering; palette upload; present via softbuffer   | the only thread that mutates game state                                          |
| **audio callback thread** (cpal) | pulls mixed stereo i16 frames from a ring buffer; on underrun outputs silence                               | never touches engine state; reads from an SPSC ring fed by the mixer             |
| **resource loader thread(s)**    | background precache of textures/flats/sprites/colormaps at level load; async palette/patch decode if useful | fills the same `ResourceManager` cache behind a mutex; never reorders game logic |
| (optional) **net socket**        | none — UDP is polled inline inside the game loop, as in the original                                        | non-blocking socket; no extra thread                                             |

Rules:

- The **simulation never blocks on I/O or audio**. Audio is pull-based on a
  second thread; screen present is the main thread's final act of the frame.
- **Resource loading is the only async-relevant workload**, and it is
  deliberately placed at the shell boundary: `doom-rs` spawns the loader, the
  engine exposes a _synchronous_ `precache_level()` API whose individual steps
  are safe to call from any thread. No `async` inside the engine; no `tokio`
  unless measurement proves startup needs it (the WAD directory parse is a few
  milliseconds; the value of async is in image decode, which is trivial at
  320×200).
- **Rendering stays on the main thread.** The frame is ~64k pixels; the
  bottleneck is fill rate, not synchronization. Splitting columns across
  threads would make output order nondeterministic and buys nothing.
- Everything else (BSP walk, clipping, spans, sound mixing) is sequential,
  matching the original's data dependencies (`solidsegs` clip list is
  order-sensitive by design).

## 6. The 35 Hz gametic model

TICRATE = 35. The original's `TryRunTics` (d_net.c) is preserved in behavior,
with the platform timer behind a `FrameClock` trait provided by the shell:

```
realtics   = clock.tics_since_last_call() / ticdup
available  = lowtic - gametic / ticdup        (lowtic = min nettics, or next maketic single-player)
counts     = clamp(realtics+1 vs available, …) — exactly the original's rules
for count in 0..counts:
    for _ in 0..ticdup:
        M_Ticker(); G_Ticker(); gametic += 1;
    NetUpdate();  // send/receive ticcmds inline
```

Determinism contract:

- The **only input to the simulation** is the sequence of `Ticcmd`s.
  `G_BuildTiccmd` snapshots input state into `netcmds[consoleplayer][maketic % BACKUPTICS]`;
  every peer (and every replay) consumes the same sequence.
- All game state transitions happen inside `G_Ticker`/`P_Ticker`. Rendering is
  a pure read of that state. HUD/menu tick at tic boundaries, not at frame boundaries.
- `ticdup` (1/2/3) duplicates each tic for slow machines; input bits
  (`BT_SPECIAL`) are masked on duplicated tics, exactly as the original.
- `gametic` is the authority for all "every N tics" logic (damage bits,
  flicker, strobe timing, demo timing). `leveltime` resets per map.
- Wall-clock time **never** enters the engine. The shell converts
  `Instant` → tick counts; tests inject a manual clock.

Consequences:

- A `.lmp` demo plus the IWAD reproduces a session bit-for-bit, including
  `players[].mo->x` at every tic — this is our primary determinism test
  (see `docs/architecture.md` §11 for the test plan).
- Network sync inherits the model: exchange ticcmds, run identical sims,
  compare `consistency` hashes. DOOM's `consistency` (low 16 bits of input
  sums + protocol checksum) is ported as-is for compat.

## 7. Fixed point and angle arithmetic

DOOM's math is integer-exact by design. DOOM-rs keeps that:

- **`Fixed` (16.16)**: `FixedMul` = `(a as i64 * b as i64) >> 16`; `FixedDiv`
  with the original's saturating behavior (`|a|>>14 >= |b|` → `±MAXINT/i32::MIN`
  by sign of `a^b`). `FixedDiv2` for the intermediate step. No float anywhere
  in gameplay math; the only floats in the codebase are CLI parsing helpers.
- **`Angle` (BAM)**: 32-bit binary angular measurement. `ANG45 = 0x2000_0000`,
  `ANG90 = 0x4000_0000`, `ANG180 = 0x8000_0000`, `ANG270 = 0xC000_0000`.
  Addition/subtraction wraps naturally (`wrapping_add`).
- **LUTs**: `finesine[10240]` (also read as cosine via `&finesine[FINEANGLES/4..]`),
  `finetangent[4096]`, `tantoangle[2049]` — generated once at build time
  (or committed as const arrays; code generation preferred) and embedded.
  `FINEANGLES = 8192`, `ANGLETOFINESHIFT = 19`, `FINEMASK = 8191`,
  `SLOPERANGE = 2048`, `DBITS = FRACBITS - SLOPEBITS`.
- **`SlopeDiv`** kept as-is (`den < 512 → SLOPERANGE`, else `(num<<3)/(den>>8)`
  saturated).
- These LUTs are the single source of truth for trig; nothing calls `sin_cos`.
  This is what makes demos deterministic across platforms.

## 8. The software renderer

DOOM's renderer is a **visplane/seg pipeline** operating on a 320×200
paletted framebuffer. Pipeline per frame (`R_RenderPlayerView`):

```
1. R_SetupFrame        — view origin/angle (incl. view bob), extralight,
                         fixed colormap selection (invulnerability)
2. clear clip state    — solidsegs[] (MAXSEGS=32), drawsegs[] (MAXDRAWSEGS=256),
                         visplanes[] (MAXVISPLANES=128), vissprites[] (MAXVISSPRITES=128)
3. NetUpdate()         — flush any pending ticcmds (game I/O interleaved)
4. R_RenderBSPNode(root)
     walk tree front-to-back; in R_Subsector:
       a. floor/ceiling plane candidates (R_FindPlane; sky handled specially)
       b. R_AddSprites(sector)  — queue vissprites for mobjs in the sector
       c. for each seg: R_AddLine → angle clip → x1/x2 via viewangletox[]
          → decide solid vs pass → R_ClipSolidWallSegment / R_ClipPassWallSegment
          → R_StoreWallRange → segs.rs wall prep (scale, texture offsets)
          → column drawer calls (colfunc) writing into framebuffer
5. R_DrawPlanes()      — visplane span rendering: per-span yslope + xtoviewangle[]
                         + distscale[] + zlight[] → R_DrawSpan
6. R_DrawMasked()      — drawsegs with masked textures (2-sided midtextures:
                         e.g. grates), vissprites sorted far→near (x-scale sort),
                         then player weapon sprites (psprites)
7. NetUpdate()         — again; the original interleaves network flush between stages
```

Key structures and mechanics (see `porting-map` for file-by-file mapping):

- **`clipsegs` / `solidsegs`**: a sorted `Vec<ClipRange>` with sentinel entries
  at the ends; insertion/merge logic ported verbatim in behavior
  (`R_ClipSolidWallSegment`). This is the visibility oracle for walls.
- **`drawsegs[]`**: one per stored wall range; carries `sil_silhouette`
  (top/bottom), `bsilheight`/`tsilheight`, and three per-x lists
  (`sprtopclip`, `sprbottomclip`, `maskedtexturecol`) used to clip sprites
  against walls. Sprites clip vertically against these lists in `R_DrawSprite`.
- **`visplanes[]`**: keyed by (height, picnum, lightlevel); each tracks
  `minx..maxx` and `top[320]/bottom[320]` per-column bounds. Overflowing the
  table is a hard error (as in the original).
- **`vissprites[]`**: projected things (`R_ProjectSprite`): x1/x2, scale,
  `startfrac` for texture sampling, `texturemid`, colormap (for `MF_SHADOW`
  translucency / invisibility), mobj flags (`MF_SHADOW` → fuzz).
- **Columns**: `R_DrawColumn` (normal), `R_DrawFuzzColumn` (spectres: fixed
  random row jitter via `fuzzoffset[]` — use the original 50-entry table),
  `R_DrawTranslatedColumn` (player translation tables, `R_InitTranslationTables`).
  Low-res variants (`R_DrawColumnLow`, `R_DrawSpanLow`) for `-detail 2`.
- **Lighting**: `scalelight[16][48]` (by scale) and `zlight[16][128]` (by
  distance) index into `colormaps` (32 entries × 256 bytes). Light levels are
  stepped, not interpolated. `extralight` (gun flash) bumps the index.
- **Sky**: `skytexture` drawn as an infinitely-wide column range
  (`R_InitSkyMap`, `skytexture` + `skytexturemid`); floor of a sky-ceilinged
  sector renders the sky. Not 8-directional; single static texture per level.
- **Gamma**: `gammatable[5][256]` applied when uploading PLAYPAL to the device,
  not to the framebuffer.
- **Screens**: `screens[5]` (main, + 4 border/view backscreens) becomes one
  owned `Framebuffer` plus scratch buffers owned by the render state; the
  border redraw logic (`R_FillBackScreen`, `R_DrawViewBorder`) ports with it.

Resolution is fixed at 320×200 internally; the shell integer-scales to the
window (with a 4:3-corrected stretch option). No runtime resolution change —
the original's `visplane` arrays are sized for 320 columns and that is fine.

## 9. WAD, resources, and level lifecycle

- **Load order** (as the original): find IWAD (`DOOMWADDIR` env, cwd, and a
  search list) → `PWAD`s from `-file` on top → lump lookup is **last-wins**
  (scan directory backwards; a PWAD overrides an IWAD lump, e.g. to replace
  textures/maps).
- **Names**: 8-char ASCII, case-insensitive, NUL-padded; comparison on the two
  32-bit words of the 8-byte name, like the original's `v1`/`v2` trick, but
  via safe `u32` reads with explicit padding.
- **ResourceManager** replaces `Z_Malloc`/`PU_CACHE`: cache entries are
  `Arc<LumpData>`-like handles with a `Tag` (Static ≤100 / Purgeable ≥101).
  Level-tagged resources are dropped at level end (`R_PrecacheLevel` fills,
  level exit flushes). The zone allocator's fragmentation games and
  `Z_CheckHeap` debugging are not ported; Rust ownership covers them.
- **Reloadable lumps** (`~`-prefixed reload hack for dev maps): not ported;
  document `-file` reload as out of scope. If a contributor wants it, it is a
  small, isolated addition to the WAD layer.
- **Validation**: every lump read validates bounds against file size; every
  parser (TEXTURE1, TEXTURE2, patch, flat, PLAYPAL, COLORMAP, map lumps)
  checks counts and offsets. Malformed WAD → `EngineError::BadWad { .. }` with
  file and lump context, never a panic.
- **Map loading** (`p_setup.c` → `map` module): all map lumps are parsed with
  explicit endian handling (`i16::from_le_bytes` etc. — map data is little-endian
  on disk everywhere). `P_GroupLines` resolves sidedef→sector and front/back
  sector pointers at load; reject matrix is loaded for sight checks; blockmap
  is validated or rebuilt if absent (the original requires it; we parse what's
  there and error clearly otherwise).

## 10. Saves, demos, and `.lmp`

- **Demo format** (`demo.lmp`): port as-is — that is the compatibility API.
  Record/playback/timedemo all flow through `G_ReadDemoTiccmd`/`G_WriteDemoTiccmd`
  equivalents. Determinism testing depends on this.
- **Savegames**: the original `.dsg` is a raw memory dump with swizzled
  pointers. Pointers are replaced by arena indices in our model, so a
  byte-compatible `.dsg` is not meaningful. Decision: keep a **binary**
  savegame with a small header (`DOOMRS1 <skill> <episode> <map> <leveltime>…`)
  and a version tag; archive the same logical data (players, sectors, mobjs,
  specials, thinker list) as the original's `P_Archive*` family, but
  index-based. Cross-version saves are out of scope until v1.0 stabilizes.
- **Screenshots**: PPM/PNG dump from the headless path (test harness +
  `-screenshot` equivalent) is the basis of golden-frame tests.

## 11. Testing and determinism verification

- **Unit tests** exhaustively cover: fixed-point mul/div edge cases (overflow,
  `FixedMul(MAXINT, …)`), angle/LUT round-trips and boundary angles
  (`ANG180` flip cases in `R_PointToAngle`), `SlopeDiv` table fill correctness
  (compare generated LUTs against a floating reference at build time), WAD
  directory parsing on synthetic WADs, blockmap/reject handling, tickcmd
  packing.
- **Golden framebuffer tests**: run headless (`--headless --timedemo demo1`
  with fixed IWAD license-clean test WAD), hash the 320×200 framebuffer per
  N tics, compare against committed hashes. Catches physics drift immediately.
- **Consistency tests**: port the original's `consistency` accumulator; a
  fixed `.lmp` must produce identical per-tic values.
- **Criterion benches**: `FixedMul`, lump lookup (1000× random names), BSP
  traversal of a reference map, full frame render. Regressions gate releases.
- **Sanitizers**: `-Dunsafe_code = 'forbid'` by default in the engine;
  `unsafe` only in `softbuffer` present path if needed, always justified.

## 12. What is deliberately _not_ in the engine

- Window creation, GL/GPU anything, audio backends, OS key codes, file paths —
  all shell (`doom-rs`). The engine speaks: events in (`EngineEvent`),
  framebuffer out, ticcmds in/out.
- Multithreaded rendering, GPU shaders, WAD virtual FS abstraction beyond the
  trivial (`std::fs::File` via a `ReadAt` trait — keeps tests mockable).
- "Modern" HUD scaling, widescreen views, interpolated rendering between tics.
  These change presentation semantics and break demo comparability. Classic
  look, modern engine — that is the project's identity. (Widescreen is tracked
  as a possible post-1.0 experiment, outside the compatibility path.)

## 13. Platform layer design (doom-rs shell)

```
trait Video   { fn present(&mut self, fb: &Framebuffer, palette: &Palette) -> Result<()>; }
trait Audio   { fn stream(&mut self) -> cpal::Stream; }
trait Input   { fn poll(&mut self) -> SmallVec<EngineEvent>; }
trait Clock   { fn tics_since_start(&self) -> u64; }
trait Storage { fn read(&self, path) -> io::Result<Vec<u8>>; fn write(...); }
```

- `Video` → `softbuffer`: upload RGB buffer scaled to window (integer scale,
  aspect-correct optional). `set_palette` path batches gamma application.
- `Input` → `winit` events translated to `EngineEvent::KeyDown/KeyUp/MouseMove/
MouseButton/Wheel/Joystick*`. The DOOM key model (`KEY_*`: arrows at
  0xAC–0xAF, `0x80+scancode` extended) is kept internally so demos and config
  stay compatible; translation happens at the boundary.
- `Clock` → `std::time::Instant` based, monotonic.
- `Storage` → `std::fs`; path resolution for IWAD (`DOOMWADDIR` → cwd →
  platform data dir) lives here.
- Headless mode (`--headless`) swaps in a null `Video` + manual `Clock`;
  used by CI, benches, and the determinism runner.

## 14. Decisions and open questions

**Decided**

- Workspace: virtual workspace — `engine/` (lib `doom_engine`),
  `doom-rs/` (bin), `tools/{wad-inspector,asset-inspector,demo-analyzer}`.
  No root package. Flat member layout (not `crates/`), per the original scaffold.
- `winit` + `softbuffer` for window/present. No GPU dependency; matches the
  320×200 paletted framebuffer model exactly. (Alternative considered: `pixels`;
  rejected — adds wgpu for no benefit at this resolution.)
- `cpal` for audio. We own the mixer (resample 11025 Hz → device rate, stereo
  separation, per-channel volume); cpal is only the output pipe.
- `criterion` for benches; `thiserror` for errors; `tracing` for logs.
- 8-bit unsigned PCM (11025 Hz) SFX as in the original; MUS music via OPL FM
  emulation (`opl-audio` or equivalent) rather than soundfonts — stays true to
  the original sound path.
- No async runtime in the engine; resource loading is the one background job,
  at the shell boundary (plain thread / loader worker), joining before a level
  is unpaused. Revisit only with measurements.
- Fixed 320×200 internal resolution; integer scaling in the shell.

**Open questions**

1. **OPL crate for music**: `opl-audio` (mature, pure Rust) vs driving a
   Rust port of Nuked OPL3 ourselves. Decide in the music task; do not block
   SFX work on it. Music may land after 1.0 if licensing/testing stalls.
2. **Savegame container**: binary with version tag (proposal above) vs RON/JSON
   for debuggability. Binary preferred for size and exactness; intermission
   stats CRC must round-trip regardless.
3. **Netcode transport**: keep raw UDP port 5029 with the original packet
   format (checksum, `retransmitfrom`, `ExpandTics`) for LAN play compatibility,
   or abstract `NetTransport` now and decide transport later? Current plan:
   define the trait, ship UDP. Internet play is not a 1.0 goal.
4. **Async for asset pipeline**: the asset-inspector tool may want parallel
   decode; `std::thread` scoped threads are enough there too. Tokio only if a
   real workload demands it.
5. **Resource cache eviction policy**: port `PU_CACHE` purge-on-demand semantics
   or replace with LRU? Current plan: keep purge-on-demand (predictable,
   matches original memory profile), add metrics.
6. **Widescreen / aspect ratio**: expose `widescreen` as an experimental
   shell-side presentation flag post-1.0? Not in the compat path; documented
   as rejected for 1.0.
7. **Joystick**: original had joystick via command-line driver; we support
   keyboard/mouse first, gamepad via winit's gilrs integration as stretch goal.

## 15. Build, code, and hygiene rules

- Edition 2024, stable toolchain, `rust-toolchain.toml` selects stable with
  rustfmt and clippy.
- `cargo fmt` + `clippy -- -D warnings` clean; CI on win/linux/mac.
- No `unsafe` without a documented reason and tests.
- Comments in code are informal English, human; user-facing strings are
  proper English.
- Behavior-deviations from the original are bugs, unless documented here or in
  the module's docs with rationale (e.g. dropping the `~`-reload hack).
- The engine never reads wall-clock time, never iterates a `HashMap` in a way
  that leaks order into state, and never uses floats for simulation.

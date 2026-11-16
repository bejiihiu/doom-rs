# DOOM-rs Engine Architecture

Modern rewrite of the DOOM engine (originally id Software, 1993, GPL) in Rust.
The reference for behavior is the original C source tree (`linuxdoom-1.10`);
wherever this document and the original disagree, the original is the spec.
The goal is behavioral fidelity first — same simulate-then-render pipeline,
same fixed-point math, same gametic model — with a modern, safe, modular
Rust implementation on top.

This document describes the architecture as built. The layout below is
the one that landed; see `docs/porting-map.md` for the file-by-file
disposition of the original C sources.

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
| `gilrs`                     | gamepad input                               | shell-side; axis/button mapping, deadzone 0.25                        |
| `clap`                      | command-line interface                      | derived parser; platform flags + documented engine passthrough        |
| `png`                       | PNG encode/decode                           | snapshot tests, asset tools                                           |
| `cachegirl`                 | atomic write staging                        | `doom.cfg` / save games: temp file + fsync + rename                   |
| `criterion`                 | benchmarks                                  | fixed-point mul/div, lump lookup, BSP walk, column/span draw, sprite project    |
| `thiserror`                 | error types                                 | engine error enums, no `String` errors deep in the stack              |
| `tracing`                   | diagnostics/logging                         | structured logs instead of `printf`                                   |
| `opl-audio`                 | OPL2 FM emulation for MUS music | pure Rust, in-process synthesis |

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
  │  fixed/tables/math ─ fixed point, BAM, PRNG       │
  │  wad/resources ── WAD directory, lump IO, cache   │
  │  map ── map lump parsing, runtime geometry        │
  │  bsp ── BSP traversal, clipping                   │
  │  render ─ software renderer (columns, spans)      │
  │  game ── ticcmd, P_Ticker, thinkers, mobjs        │
  │  specials ─ linedef actions (doors/floors/...)    │
  │  stbar/hud/menu ─ status bar, HUD, menu           │
  │  automap ── automap                               │
  │  audio/music ── SFX channel mixer, OPL2 music     │
  │  net ── d_net/i_net: ticcmd sync over UDP         │
  │  game also: savegame + demo (.lmp) IO             │
  └────────────────────────────────────────────────┘
```

## 3. Module map inside `doom_engine`

Module names are short and flat; everything is a direct child of the
crate root. Grouped roughly by pipeline stage:

| Module             | Replaces                                                                          | Responsibility                                                                                                                                  |
| ------------------ | --------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `fixed`            | m_fixed.c                                                                         | 16.16 fixed point: `Fixed`, saturating `FixedDiv`, exact i64-intermediate `FixedMul`                                                           |
| `tables`           | tables.c, r_main.c (angle utils)                                                  | `FINE_SINE`/`FINE_COSINE`/`FINE_TANGENT` LUTs (byte-for-byte), 32-bit BAM `Angle`                                                              |
| `math`             | m_bbox.c, p_map.c (side tests)                                                    | bounding boxes, `point_on_side`/`point_on_divline_side`, `box_on_side`, distance/angle helpers                                                 |
| `rng`              | m_random.c                                                                        | deterministic 256-byte LUT PRNG (`DoomRng`), port of `M_Random`/`P_Random`                                                                      |
| `swap`             | m_swap.c                                                                          | bounds-checked little-endian scalar reads/writes on byte slices                                                                                 |
| `args`             | m_argv.c                                                                          | classic DOOM CLI keys (`-iwad`, `-warp`, `-skill`, `-net`, ...), typed result                                                                   |
| `log`              | (shell init)                                                                      | `tracing` subscriber setup, `RUST_LOG` filtering                                                                                                 |
| `errors`           | scattered `I_Error` call sites                                                    | `EngineError` enum (`thiserror`) + `EngineResult`; one type at every boundary                                                                   |
| `config`           | m_misc.c (`default.cfg`)                                                          | `doom.cfg` parse/save (round-trip safe, raw-string storage), plain engine `Config`                                                              |
| `wad`              | w_wad.c                                                                           | WAD header/directory parse, IWAD/PWAD stacking (last wins), name lookup, validation                                                             |
| `resources`        | z_zone.c, W_CacheLump\*                                                           | `ResourceManager`: typed decode, once-per-(type, lump) cache with stats; no zone allocator                                                      |
| `scratch`          | (new - no C counterpart)                                                          | atomic file writes staged through `cachegirl`, crash-debris recovery at startup                                                                 |
| `gfx`              | r_data.c, v_video.c                                                               | palettes, colormaps, patches, `TEXTURE1`/`TEXTURE2` composites, flats, sprites                                                                  |
| `map`              | p_setup.c, p_maputl.c (blockmap part)                                             | map lump parsing + validation, geometry structs, runtime blockmap queries                                                                        |
| `bsp`              | r_bsp.c, r_main.c (clip-seg part)                                                 | BSP traversal, `ClipSegList` solid/pass clipping, `AddLine`, `CheckBBox`, `PointToAngle` family                                                 |
| `render`           | r_*.c, r_defs.h, r_state.h, v_video.c                                             | indexed framebuffer, columns/spans, gamma, zlight/scalelight tables, wall rasterizer, plane sweep, sprites, sky                                 |
| `stbar`            | st_stuff.c, st_lib.c                                                              | status bar: numbers, face state machine, layout constants byte-for-byte                                                                         |
| `hud`              | hu_stuff.c, hu_lib.c                                                              | HUD message widget, font loading, ported `d_englsh.h` message table                                                                             |
| `menu`             | m_menu.c, m_cheat.c                                                               | main/episode/options/sound/load/save menus, prompts, cheat-code matcher                                                                         |
| `automap`          | am_map.c                                                                          | follow/zoom/pan/marks, seen-line tracking (own bitset), iddt reveal levels                                                                      |
| `wipe`             | f_wipe.c                                                                          | pixel-melt screen transition (the only wipe the game used)                                                                                       |
| `finale`           | f_finale.c                                                                        | end-of-episode texts, art screens, DOOM II MAP30 cast call                                                                                      |
| `intermission`     | wi_stuff.c                                                                        | between-map stats screens, episode map, deathmatch matrix                                                                                        |
| `thinker`          | p_tick.c, d_think.h                                                               | thinker registry as a generational arena, insertion-order execution, vanilla lazy removal                                                        |
| `mobj`             | p_mobj.c, p_mobj.h                                                                | actors (`Mobj`), spawn/lifecycle/momentum, sector/blockmap thing lists, `MobjStore`                                                             |
| `player`           | d_player.h, p_user.c (state half), d_ticcmd.h                                     | per-player state (`Player`), `TicCmd`, weapon ownership/ammo/powers                                                                             |
| `info`             | info.c/h, sounds.c/h, p_switch.c                                                  | generated static tables: `STATES`, `MOBJ_INFO`, `sprnames`, `S_sfx`/`S_music`, switch lists                                                     |
| `enemy`            | p_enemy.c, p_sight.c                                                              | monster AI (look/chase/attacks), `P_CheckSight` (REJECT matrix + BSP slope window), noise-alert flood                                           |
| `movement`         | p_map.c, p_maputl.c, p_mobj.c (movement half)                                     | `P_CheckPosition`/`P_TryMove`/`P_SlideMove`, path traversal, momentum ticks, sky-missile hack                                                    |
| `inter`            | p_inter.c, d_items.c                                                              | damage/armor/thrust rules, deaths, pickups, keys, powers, item/kill tally                                                                       |
| `weapons`          | p_pspr.c                                                                          | `A_*` weapon callbacks, psprites, all nine weapons (fist through super shotgun)                                                                  |
| `specials`         | p_doors.c, p_floor.c, p_plats.c, p_ceilng.c, p_lights.c, p_switch.c, p_telept.c, p_spec.c | doors, floors/stairs/donuts, plats, ceilings/crushers, switches, lights, teleports, line-special dispatch, `P_SpawnSpecials`/`P_UpdateSpecials` |
| `audio`            | s_sound.c, i_sound.c (mixer part)                                                 | DMX lump decoder, channel priority/attenuation model, 8-bit-mixer port, cpal/null backends                                                       |
| `music`            | i_music.c (MUS part)                                                              | MUS parser, GENMIDI bank, in-process OPL2 synthesis, `pull_samples` cursor                                                                       |
| `game`             | g_game.c, p_tick.c, p_user.c, p_setup.c, p_saveg.c, d_main.c                       | game state, tic loop, level bring-up, demos, save games, frame assembly                                                                          |
| `net`              | d_net.c, i_net.c                                                                  | vanilla packet format, UDP transport behind a trait, `TryRunTics` governor, setup arbitration                                                    |

The startup path (`i_main.c`/`d_main.c` - CLI entry, IWAD probing, the
window, the present loop) lives in the `doom-rs` shell, not the engine.
Only reusable pieces (argument parsing, config data) stay in
`doom-engine`.

## 4. Data model

### 4.1 Arena storage, index-based references

The C pointer web (`vertex_t *`, `line_t *`, `sector_t *`) becomes
**index-based arenas** inside `map`:

```rust
pub struct Map {
    pub vertexes: Vec<Vertex>,
    pub linedefs: Vec<Linedef>,
    pub sidedefs: Vec<Sidedef>,
    pub sectors: Vec<Sector>,
    pub segs: Vec<Seg>,
    pub subsectors: Vec<SubSector>,
    pub nodes: Vec<Node>,
    pub blockmap: BlockMapGrid,
    pub reject: Vec<u8>,
}
```

Indices (not ids) reference everything: `Node` children point at other
nodes/subsectors by index, linedefs reference vertex/sidedef indices,
sectors are reached through sidedefs. This is faster than pointer chasing
(cache-friendly), trivially serializable for savegames, and removes the
aliasing problems the C original worked around with `validcount`/`Z_ChangeTag`.
`validcount` becomes per-call `HashSet`s where it still matters, or simply
borrow discipline.

### 4.2 Thinkers

The C doubly-linked thinker list with a union of function pointers becomes a
closed enum dispatched by the ticker:

```rust
pub enum Thinker {
    Mobj(Mobj),
    Door(VerticalDoor),
    LightFlash(LightFlash),
    StrobeFlash(StrobeFlash),
    FireFlicker(FireFlicker),
    Glow(Glow),
    FloorMover(FloorMover),
    Plat(Plat),
    Ceiling(Ceiling),
}
```

`ThinkerRegistry` keeps a `Vec` arena with generational handles, so a
stale id can never alias a recycled slot. Execution is insertion order,
and removal is lazy and deterministic (vanilla: the thinker is flagged,
and unlinked/freed on its turn). New thinker kinds are added as variants.

### 4.3 Game state ownership

All mutable globals from `doomstat.h` collapse into the per-subsystem
structs that `game::Game` owns (the mobj store, the player states, the
intermission/finale state, the save-game machinery, and so on). No
`static mut`, no global singletons. Interior mutability lives only in
`resources::ResourceManager` (a `RefCell` around the decode cache) and
the shell's audio rings.

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
| **audio callback thread** (cpal) | mixes SFX inline through the engine `SharedMixer`; pulls music from a ring buffer; silence on underrun      | never touches game state                                                         |
| **resource loading**             | synchronous at startup and on first use; WAD parse is a few ms                                               | no loader thread; decode-once cache in the resource manager                        |
| (optional) **net socket**        | none — UDP is polled inline inside the game loop, as in the original                                        | non-blocking socket; no extra thread                                             |

Rules:

- The **simulation never blocks on I/O or audio**. Audio is pull-based on a
  second thread; screen present is the main thread's final act of the frame.
- **There is no resource loader thread.** The WAD directory parse is a few
  milliseconds; everything else is decoded on demand through the resource
  manager's once-per-lump cache. If startup ever needs to go async it will
  be a shell-side concern, not an engine API. No `async` inside the
  engine; no `tokio`.
- **Rendering stays on the main thread.** The frame is ~64k pixels; the
  bottleneck is fill rate, not synchronization. Splitting columns across
  threads would make output order nondeterministic and buys nothing.
- Everything else (BSP walk, clipping, spans, sound mixing) is sequential,
  matching the original's data dependencies (`solidsegs` clip list is
  order-sensitive by design).

## 6. The 35 Hz gametic model

TICRATE = 35. The original's `TryRunTics` (d_net.c) is preserved in behavior;
the wall-clock read lives at the shell boundary - the shell feeds
`Game::advance(elapsed_ms)` with real frame times each frame, and the net
layer takes a `TimeSource` hook so tests can inject a manual clock:

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
- `ticdup` (1-9, `-dup`) duplicates each tic for slow links; input bits
  (`BT_SPECIAL`) are masked on duplicated tics, exactly as the original.
- `gametic` is the authority for all "every N tics" logic (damage bits,
  flicker, strobe timing, demo timing). `leveltime` resets per map.
- Wall-clock time **never** enters the simulation. The shell converts
  `Instant` → elapsed milliseconds; the net layer injects a `TimeSource`;
  tests inject a manual one.

Consequences:

- A `.lmp` demo plus the IWAD reproduces a session bit-for-bit, including
  `players[].mo->x` at every tic — this is our primary determinism test
  (see §11 for the test plan).
- Network sync inherits the model: exchange ticcmds, run identical sims,
  compare `consistency` hashes. DOOM's `consistency` (low 16 bits of input
  sums + protocol checksum) is ported as-is for compat.

## 7. Fixed point and angle arithmetic

DOOM's math is integer-exact by design. DOOM-rs keeps that:

- **`Fixed` (16.16)**: `FixedMul` = `(a as i64 * b as i64) >> 16`; `FixedDiv`
  with the original's saturating behavior (`|a|>>14 >= |b|` → `±MAXINT/i32::MIN`
  by sign of `a^b`). No float anywhere in gameplay math; floats exist only in
  the audio output path.
- **`Angle` (BAM)**: 32-bit binary angular measurement. `ANG45 = 0x2000_0000`,
  `ANG90 = 0x4000_0000`, `ANG180 = 0x8000_0000`, `ANG270 = 0xC000_0000`.
  Addition/subtraction wraps naturally (`wrapping_add`).
- **LUTs**: `finesine[10240]` (also read as cosine via `&finesine[FINEANGLES/4..]`),
  `finetangent[4096]`, `tantoangle[2049]` — committed as const arrays
  (byte-for-byte the original's tables.c literals), not build-time generated.
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

- **Load order** (as the original): IWAD resolution is `-iwad` → `DOOM_IWAD`
  env var → current-directory search list → `PWAD`s from `-file` on top →
  lump lookup is **last-wins** (scan directory backwards; a PWAD overrides an
  IWAD lump, e.g. to replace textures/maps).
- **Names**: 8-char ASCII, case-insensitive, NUL-padded; comparison on the two
  32-bit words of the 8-byte name, like the original's `v1`/`v2` trick, but
  via safe `u32` reads with explicit padding.
- **ResourceManager** replaces `Z_Malloc`/`PU_CACHE`: typed decode is cached
  once per (type, lump) pair behind shared `Arc`s; decode failures are not
  cached; `CacheStats` exposes hits/misses/decodes. There is no tag system or
  purge-on-demand - the working set is bounded by the IWAD. The zone
  allocator's fragmentation games and `Z_CheckHeap` debugging are not ported;
  Rust ownership covers them.
- **Reloadable lumps** (`~`-prefixed reload hack for dev maps): not ported;
  document `-file` reload as out of scope. If a contributor wants it, it is a
  small, isolated addition to the WAD layer.
- **Validation**: every lump read validates bounds against file size; every
  parser (TEXTURE1, TEXTURE2, patch, flat, PLAYPAL, COLORMAP, map lumps)
  checks counts and offsets. Malformed WAD → `EngineError::Wad` (`WadError`)
  with file and lump context, never a panic.
- **Map loading** (`p_setup.c` → `map` module): all map lumps are parsed with
  explicit endian handling (`i16::from_le_bytes` etc. — map data is little-endian
  on disk everywhere). `P_GroupLines` resolves sidedef→sector and front/back
  sector pointers at load; reject matrix is loaded for sight checks; blockmap
  is required and strictly validated - a missing or corrupt BLOCKMAP is a
  clear load error.

## 10. Saves, demos, and `.lmp`

- **Demo format** (`demo.lmp`): port as-is — that is the compatibility API.
  Record/playback/timedemo all flow through `G_ReadDemoTiccmd`/`G_WriteDemoTiccmd`
  equivalents. Determinism testing depends on this.
- **Savegames**: the original `.dsg` is a raw memory dump with swizzled
  pointers. Pointers are replaced by arena indices in our model, so a
  byte-compatible `.dsg` is not meaningful. Decision (locked in): keep a
  **binary** savegame - `DOOMSAV\0` magic + version byte + 24-byte description,
  then index-based sections (players, world, thinkers, specials, sectors) that
  archive the same logical data as the original's `P_Archive*` family. Atomic
  write through the scratch dir. Vanilla `.dsg` compat is out of scope.
- **Screenshots**: PPM/PNG dump from the headless path (test harness +
  `-screenshot` equivalent) is the basis of golden-frame tests.

## 11. Testing and determinism verification

- **Unit tests** exhaustively cover: fixed-point mul/div edge cases (overflow,
  `FixedMul(MAXINT, …)`), angle/LUT round-trips and boundary angles
  (`ANG180` flip cases in `R_PointToAngle`), LUTs verified against the
  original's tables.c literals at port time, WAD directory parsing on
  synthetic WADs, blockmap/reject handling, tickcmd packing.
- **Golden framebuffer tests**: `engine/tests/snapshot.rs` renders a scene
  into the indexed framebuffer and asserts the PNG (IHDR dims, bit depth,
  color type, round-trip decode). Catches renderer drift immediately.
- **Determinism tests**: `engine/tests/determinism.rs` runs the same scripted
  session twice tic-for-tic (state, kills, framebuffer checksum) and compares
  serialized save bytes - this is the primary compatibility gate.
- **Consistency tests**: the vanilla `consistency` accumulator is ported;
  net games abort on divergence.
- **Criterion benches**: fixed mul/div, WAD directory parse/lookup, BSP
  traversal, clip lists, column/span drawers, sprite projection, light table
  build. Regressions gate releases.
- **`#![forbid(unsafe_code)]`**: no unsafe anywhere in the workspace; the
  shell's present path is `softbuffer`, which needs none.

## 12. What is deliberately _not_ in the engine

- Window creation, GL/GPU anything, audio backends, OS key codes, file paths -
  all shell (`doom-rs`). The engine takes events in (`game::InputEvent`),
  renders into a shell-owned framebuffer, ticcmds in/out.
- Multithreaded rendering, GPU shaders, WAD virtual FS abstraction beyond the
  trivial (`std::fs::File` via a `ReadAt` trait — keeps tests mockable).
- "Modern" HUD scaling, widescreen views, interpolated rendering between tics.
  These change presentation semantics and break demo comparability. Classic
  look, modern engine — that is the project's identity. (Widescreen is tracked
  as a possible post-1.0 experiment, outside the compatibility path.)

## 13. Platform layer design (doom-rs shell)

The shell is concrete, not trait-based - one backend per concern, wired
directly in `doom-rs/src/app.rs`:

- **Video**: winit `ApplicationHandler` + softbuffer surface. The shell owns
  an `IndexedFramebuffer`, hands it to `Game::advance(&tic_input, elapsed_ms,
  &mut fb)` each frame, applies the palette (with `usegamma`) in a reusable
  `u32` buffer, nearest-upscales to the window (integer scale 1-8, default
  4, letterboxed, `--fullscreen` for borderless). Pacing is
  `ControlFlow::WaitUntil` with a 16 ms slot - no spin loop.
- **Input**: winit `KeyCode` + modifiers translated in `input.rs` to DOOM
  key codes (arrows/WASD move, Ctrl or LMB fire, Space or RMB use, Shift
  run, Alt/Q strafe, wheel cycles weapons, 1-9 weapons, Tab automap,
  Escape menu). The DOOM key model is kept internally; translation happens
  at the boundary. Gamepad via `gilrs`: right stick turns, left stick
  strafes/moves, D-pad = arrows, triggers fire/use, South=use, East=fire,
  West=speed, Start=menu, Select=automap, North=weapon cycle; deadzone
  0.25. Focus loss releases every held key.
- **Audio**: one cpal output stream; SFX mix in-callback through the
  engine's `SharedMixer` (resampling handled by the engine), music arrives
  via `MusicEngine::pull_samples` and is resampled on the way out. Missing
  device or unsupported format logs a warning and the game runs silent.
- **Storage**: plain `std::fs`. IWAD resolution order: `-iwad`, `DOOM_IWAD`,
  then the current directory (`doom2.wad`, `doom2f.wad`, `plutonia.wad`,
  `tnt.wad`, `doomu.wad`, `doom.wad`, `doom1.wad`). `doom.cfg` is read from
  and written to the current directory (or `-config PATH`).
- **Headless**: there is no `--headless` flag. The engine itself is
  window-free and headless-testable (tests, benches, the determinism
  runner); the binary always opens a window.

## 14. Decisions

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

**Decisions locked in (as built)**

1. **OPL for music**: `opl-audio` 0.1.0 (pure Rust, OPL2) - chosen; the
   engine renders whole songs in-process and serves mono samples through
   `MusicEngine::pull_samples`. GENMIDI is the instrument bank; a missing
   or corrupt `GENMIDI` lump falls back to a documented one-voice bank.
2. **Savegame container**: custom binary with magic + version tag
   (`DOOMSAV\0` v1); index-based, self-describing, round-trip stable
   within doom-rs; atomic write through the scratch dir. Vanilla `.dsg`
   is out of scope.
3. **Netcode transport**: raw UDP 5029, vanilla packet format (checksum,
   `retransmitfrom`, `ExpandTics`) behind `NetTransport`; UDP shipped, tests
   run on an in-memory transport. Internet play is not a 1.0 goal.
4. **Async for asset pipeline**: not needed; tools decode on the calling
   thread. No async runtime anywhere.
5. **Resource cache**: decode-once `LumpCache` keyed by (type, lump) with
   stats; no eviction - the working set is small and bounded by the IWAD.
6. **Widescreen / aspect ratio**: rejected for 1.0.
7. **Joystick/gamepad**: keyboard/mouse first; gamepad via `gilrs` in the
   shell (axis/button mapping, deadzone 0.25).

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

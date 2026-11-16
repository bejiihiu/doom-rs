# Compatibility

doom-rs targets **behavioral fidelity** with the original engine
(`linuxdoom-1.10`), not a re-imagining. This page tracks what that
fidelity covers today, and where it doesn't.

Everything below is currently ported and covered by the test suite,
unless noted otherwise. "Ported" means: same mechanics, same numeric
behavior (fixed-point 16.16 math, the original LUT trigonometry tables,
integer-exact slope math), verified against the 1.10 sources.

## Ported and verified

- **Game mechanics** — the full gameplay layer is ported: all monster
  AI and `A_*` callbacks, all player weapons (fist through BFG 2700,
  including the super shotgun), damage/armor/pickup rules, thrust,
  pain states, respawn rules, Nightmare item respawn.
- **Sector specials** — doors, floors (including donuts and stairs),
  elevator platforms, ceilings and crushers, switches and their
  textures, teleports, light effects (flicker/strobe/fire), boss-death
  floor lowering (tag 666/667), level exits (including secret exits).
- **The whole renderer** — BSP traversal, wall clipping, texture-mapped
  columns, flat spans, visplanes, vissprites and sprite sorting, masked
  two-sided mid-textures, the fuzz effect for spectres, sky rendering,
  the lighting (`scalelight`/`zlight`) tables, 320×200 paletted
  framebuffer with gamma correction.
- **Screens and UI** — status bar, HUD message widget, menu system
  (options, new game, load/save, cheat codes), automap
  (follow/zoom/marks/cheat overlay), intermission screens, episode
  finales and the DOOM II MAP30 cast call, pixel-melt transitions.
- **Demo format** — `.lmp` record/playback is byte-compatible by
  construction. The tic model is bit-deterministic: demos plus the
  IWAD reproduce a session exactly, on any platform.
- **Net play** — UDP, vanilla packet format (checksum, retransmit
  logic, node arbitration, consistency checks), tic duplication
  (`-dup`), up to 8 players. Broadcast discovery does not exist in the
  1.10 sources; nodes need explicit unicast addresses.
- **Audio** — SFX: all 109 sounds, distance attenuation, priorities,
  stereo separation, pitch perturbation. Music: MUS format parsed and
  rendered in-process through an OPL2 emulator with the GENMIDI
  instrument bank.
- **Game data** — both IWAD families: DOOM (shareware/registered/
  ultimate) and DOOM II (and Plutonia/TNT). Mission type is resolved
  from the filename.
- **Save games** — round-trip stable within doom-rs.
- **Command line** — the classic keys: `-iwad`, `-file`, `-warp`,
  `-skill`, `-nomonsters`, `-respawn`, `-fast`, `-deathmatch`,
  `-altdeath`, `-record`, `-playdemo`, `-timedemo`, `-dup`,
  `-extratic`, `-port`, `-net`, `-nosound`, `-nodeb`, `-devparm`,
  `-statcopy` (accepted, unused), `-turbo` (clamped 10–400).

## Not verified yet

- **Broader real-IWAD verification.** On Windows, the shareware
  `doom1.wad` boots through the title, menu and into E1M1, with muzzle
  flash, HUD and sound. Not yet verified: full playthroughs, episodes
  2–4, DOOM II / Plutonia / TNT, net play between machines, save/load
  roundtrip under a real IWAD, and Linux/macOS. All headless behavior
  (boot, level load, rendering, combat) is pinned by integration tests
  on a synthetic WAD.

## Known limitations

- **Animated flats and textures are not ported.** The frame-animation
  tables are wired as identity (F_SKY1 etc. are static). This affects
  lava/slime/computer-texture animations only.
- **Net demos are not supported.** `-playdemo`, `-timedemo` and
  `-record` with `-net` are rejected with an explanatory error.
- **Chat is not ported.** Player chat (`hu_stuff` chat lines and the
  chat macros) has no keyboard path yet; this also means no
  in-net-game messages from other players.
- **Savegames are not vanilla-compatible.** The format is a custom
  versioned binary (index-based, self-describing). It round-trips
  within doom-rs; vanilla `.dsg` files cannot be loaded.
- **Projectile speed doubling under `-fast`** is not ported (state
  tables are immutable by design). Fast monsters themselves (doubled
  thinker rate) are implemented.
- **In-game gamma cycling** (F11) is not ported; gamma comes from the
  config at startup.
- **Screenshots** (F1 under `-devparm`) are not ported.
- **Broadcast net discovery** does not exist in 1.10 and is not
  emulated. Explicit unicast lists (`-net <n> host[:port] ...`) work,
  including multiple instances on one machine via `-port`.
- **`-statcopy`** is accepted and ignored (the original used it for
  performance profiling output).
- **No source-port extensions.** No DEHACKED, no UMAPINFO, no GL nodes,
  no widescreen, no mouse vertical look. Extras that change gameplay
  semantics or break demo/net determinism are out of scope.
- **Sound channel count is fixed at 8.** The `snd_channels` config
  value is parsed but the mixer runs 8 slots regardless (the vanilla
  default for the Linux port was 3; we unified on 8).
- **`-dup`/`-extratic` net paths** are implemented but have not been
  run end-to-end between two machines.
- **Intermission/end screens** require the standard IWAD lumps
  (WIMAP/WILV/WI animation art); WADs that strip them fall back to
  skipping the intermission (a warning is logged).
- **ticdup > 1** duplicates input tics as in vanilla; duplicated tics
  strip `BT_SPECIAL` exactly as the original does.
- **Plutonia/TNT**: mission auto-detection covers `plutonia.wad` and
  `tnt.wad` by filename; Ultimate DOOM episode 4 is auto-detected as
  retail via `doomu.wad`.

## IWAD requirement

doom-rs requires a legally obtained IWAD. Shareware `doom1.wad` is
freely distributable by id Software; commercial IWADs (`doom.wad`,
`doom2.wad`, `plutonia.wad`, `tnt.wad`) require ownership. No IWAD
content is included in this repository.

## Vanilla behavior notes

- Fixed-point `16.16` math is kept bit-for-bit, including the original
  saturating `FixedDiv` semantics. No floats enter the simulation.
- `ticdup` duplicates each tic; input special bits are masked on
  duplicated tics, as in vanilla.
- The consistency check aborts a net game on divergence (the vanilla
  `I_Error` path), rather than desyncing silently.
- Demo replay is the primary compatibility target: a vanilla-recorded
  `.lmp` plus the IWAD should replay correctly. This is pinned by
  determinism integration tests on a synthetic IWAD; the pass with a
  real IWAD is part of the in-progress end-to-end verification.

//! Network play, ported from `d_net.c` (node-based P2P over UDP, broadcast
//! destinations supported) and `i_net.c` (the UDP backend), plus the
//! consistency check from `G_Ticker` in `g_game.c`.
//!
//! Scope: the transport and tic-sync half of a vanilla DOOM net game — the
//! wire format, the node list, the retransmit machinery, the tic governor
//! (`TryRunTics`) and the setup arbitration. It deliberately does **not**
//! know about your game objects; the game shell plugs into it through
//! [`NetGameHooks`].
//!
//! Lifecycle, mirroring vanilla:
//!
//! 1. `D_CheckNetGame` — [`NetEngine::start`]/[`NetEngine::with_transport`]
//!    bind sockets and set up the node list from `-net <console> <host>...`.
//! 2. `D_ArbitrateNetStart` — [`NetEngine::arbitrate_net_start`] blocks
//!    (use [`NetEngine::arbitration_step`] in tests) until every node
//!    either has the setup packet (listeners) or got an acknowledgement
//!    from every remote node (the key player). The key player
//!    (`console_player == 0`) sends the game settings; everyone else
//!    receives them.
//! 3. Steady state — the platform calls [`net_update`] once per frame and
//!    [`tics::try_run_tics`] once per frame; the game runner
//!    (a future wiring step) feeds `gametic`/ticks back through the hooks.
//! 4. `D_QuitNetGame` — [`NetEngine::quit_net_game`] says goodbye four
//!    times so nobody waits on a dead peer.
//!
//! The `.lmp` net-demo format is out of scope for this module (demos live
//! in `game::demo`); `netdemo` is passed to [`ConsistencyCheck`] as a plain
//! flag.
//!
//! Notable faithful-to-vanilla details:
//!
//! - Node 0 is the console; datagrams to node 0 are rebound, not sent.
//! - The checksum is the DOS/Windows algorithm over the little-endian
//!   native layout (linuxdoom's NORMALUNIX build compiles it out entirely;
//!   our peers are always doom-rs, so it is defined and symmetric).
//! - Tics travel mod 256; the high bytes are resolved by [`packet::expand_tics`].
//! - Only the in-game nodes gate the tic governor; a departed node does not
//!   stall the survivors.
//! - There is no `G_CmdForTic` in linuxdoom-1.10; the equivalent is reading
//!   `netcmds[player][(gametic / ticdup) % BACKUPTICS]`, exposed as
//!   [`NetEngine::netcmd`] with the slot computed by the caller.
//!
//! Deviations from vanilla are documented next to the code that makes them
//! (`(ip, port)` sender matching and the single shared socket, `SO_BROADCAST`,
//! corrupt-`player` guard, zero-initialized `nodeforplayer`, send errors
//! logged instead of ignored silently, `I_Error` paths as [`NetError`]
//! values, `maketic - realstart > BACKUPTICS` rejected instead of
//! truncated into the `numtics` byte).
//!
//! Module map:
//!
//! - [`packet`] — wire format, NCMD flags, checksum, setup payload.
//! - [`socket`] — the UDP backend (`NetTransport`, `UdpTransport`).
//! - [`engine`] — the node list and packet pump (`NetEngine`, `NetGameHooks`).
//! - [`tics`] — the tic governor, pace adaptation and consistency check.
//! - [`time`] — `I_GetTime` equivalents.

mod engine;
mod packet;
mod socket;
#[cfg(test)]
mod tests;
mod tics;
mod time;

pub use engine::{NetConfig, NetEngine, NetError, NetGameHooks, NetNode};
pub use packet::{
    BACKUPTICS, DOOM_VERSION, DOOMPORT, MAXNETNODES, NCMD_CHECKSUM, NCMD_EXIT, NCMD_KILL,
    NCMD_RETRANSMIT, NCMD_SETUP, NetGameSettings, NetPacket, PL_DRONE, RESENDCOUNT, checksum,
    expand_tics,
};
pub use socket::{NetTransport, UdpTransport};
pub use tics::{
    BT_SPECIAL, ConsistencyCheck, ConsistencyGate, TryRunTicsOutcome, decide_counts, try_run_tics,
};
pub use time::{CountingTime, InstantTimeSource, ManualTime, TimeSource};

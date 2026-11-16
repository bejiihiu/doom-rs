//! `d_net.c` / `i_net.c` — the node-based peer-to-peer net game engine.
//!
//! Vanilla model, faithfully kept: a "console" machine (this node) plus up
//! to 7 remote nodes (`MAXNETNODES = 8` total), all exchanging tic commands
//! over UDP. Node 0 is always the console; datagrams addressed to node 0
//! are rebound locally instead of going on the wire. There is no host/
//! client distinction after setup; every peer runs the same logic.
//!
//! `doomcom_t` and the `I_NetCmd`/`CMD_SEND`/`CMD_GET` driver plumbing are
//! folded into [`NetEngine`] — the DOS driver-call ABI has no meaning in
//! this port. Everything else is a direct line-by-line port:
//!
//! - [`NetEngine::net_update`] is `NetUpdate` (build local ticcmds, send
//!   them to every in-game node, then collect packets).
//! - [`NetEngine::h_send_packet`] / [`NetEngine::h_get_packet`] are
//!   `HSendPacket`/`HGetPacket`, including the node-0 rebound.
//! - [`NetEngine::get_packets`] is `GetPackets`: the retransmit bookkeeping,
//!   the out-of-order/duplicate/missed-packet logic and the `netcmds` ring.
//! - [`NetEngine::arbitration_step`] is one iteration of the
//!   `D_ArbitrateNetStart` loops; [`NetEngine::arbitrate_net_start`] is the
//!   blocking driver that loops it. The stepped form exists so tests can
//!   drive several nodes cooperatively in one thread.
//! - [`NetEngine::quit_net_game`] is `D_QuitNetGame`.
//!
//! What is *not* here: drawing, IWAD I/O, "is there a game at all". This
//! module is the transport+sync half; the (future) game-loop wiring passes
//! in a [`NetGameHooks`] implementation that owns `playeringame[]`,
//! the event pump, `G_Ticker` and friends. It also owns `gametic`, read
//! and written through the hooks — a single source of truth, unlike
//! vanilla's shared globals.
//!
//! Vanilla quirks kept on purpose (see also the module docs in `mod.rs`):
//!
//! - `NetUpdate` skips building new ticcmds once `maketic - gametic/ticdup`
//!   reaches `BACKUPTICS/2 - 1`; excess input is simply dropped.
//! - Retransmit bookkeeping counts *successful* retransmit packets too:
//!   `resendcount` only gates *new* retransmit requests, it decrements for
//!   every other packet.
//! - A duplicate packet (`realend == nettics`) is dropped before the
//!   out-of-order check; a packet with tics we already have *and* tics we
//!   don't have still delivers the new tail — vanilla handles the advance
//!   one tic at a time and never rewinds.
//! - An initial `netcmds` buffer is zeroed like vanilla's bss, so the first
//!   tics of a net game are deterministic.
//!
//! Deviations from vanilla (documented, deliberate):
//!
//! - Sender identity is matched by `(ip, port)`, not IP-only — see `socket`.
//! - `node_for_player` is zero-initialized; vanilla left it bss and read it
//!   before the first packet arrived (harmless there, an info leak here).
//! - A packet whose `player` has more than `MAXPLAYERS` worth of bits after
//!   masking `PL_DRONE` is dropped instead of writing out of bounds (vanilla
//!   indexed `netcmds[]` with it directly).
//! - Non-blocking UDP; send failures are logged and ignored (vanilla never
//!   checked `sendto` either), receive failures are logged and the datagram
//!   dropped instead of `I_Error`.
//! - `consoleplayer == 0` makes a node the "key player", exactly like
//!   vanilla: it sends setup, everyone else listens.

use std::io;
use std::net::SocketAddr;

use crate::player::{MAXPLAYERS, TicCmd};

use super::packet::{
    BACKUPTICS, CMD_BUF, DOOM_VERSION, MAXNETNODES, NCMD_EXIT, NCMD_KILL, NCMD_RETRANSMIT,
    NCMD_SETUP, NetGameSettings, NetPacket, PL_DRONE, RESENDCOUNT, checksum, expand_tics,
    stamp_checksum,
};
use super::socket::{NetTransport, UdpTransport};

/// Per-node state (`nodeingame[]`, `nettics[]`, `remoteresend[]`,
/// `resendto[]`, `resendcount[]` and the node's address).
#[derive(Debug, Clone)]
pub struct NetNode {
    /// Where to send to (the slot's `sendaddress[]` entry; `None` for node 0).
    pub address: Option<SocketAddr>,
    /// `nodeingame[]`: set false when the node leaves the game.
    pub in_game: bool,
    /// `nettics[]`: the maketic of the next tic expected from this node.
    pub net_tics: i32,
    /// `remoteresend[]`: set when this node is missing tics and asked for a
    /// retransmit; cleared when any newer packet arrives.
    pub remote_resend: bool,
    /// `resendto[]`: the tic the node wants to see next.
    pub resend_to: i32,
    /// `resendcount[]`: suppresses a flood of retransmit requests.
    pub resend_count: i32,
}

impl NetNode {
    fn with_address(address: Option<SocketAddr>) -> Self {
        Self {
            address,
            in_game: false,
            net_tics: 0,
            remote_resend: false,
            resend_to: 0,
            resend_count: 0,
        }
    }
}

/// `I_InitNetwork` results, minus sockets: how the node list is laid out.
#[derive(Clone, Debug)]
pub struct NetConfig {
    /// `-net <consoleplayer> <host> ...`: index of the console player.
    pub console_player: usize,
    /// `-dup N`: tic duplication for slow links (1..=9, clamped).
    pub ticdup: i32,
    /// `-extratic`: send a backup tic in every packet.
    pub extratics: bool,
    /// The remote nodes' addresses, in node order (node 0 is this console).
    pub hosts: Vec<SocketAddr>,
    /// The UDP port other nodes send to (`-port`). Use 0 in tests to get an
    /// ephemeral port.
    pub port: u16,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            console_player: 0,
            ticdup: 1,
            extratics: false,
            hosts: Vec::new(),
            port: super::packet::DOOMPORT,
        }
    }
}

// NetGameSettings comes from the packet module; it is re-exported at the
// net module root.

/// Errors from the net layer. The game shell maps these onto its own error
/// channel; they all correspond to vanilla `I_Error` exits.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// A socket operation failed (bind or create).
    #[error("network error: {0}")]
    Io(#[from] io::Error),
    /// A node reported the game killed (`NCMD_KILL`); vanilla `I_Error`.
    #[error("killed by network driver")]
    KilledByPeer,
    /// The peer's `VERSION` in a setup packet does not match ours; vanilla
    /// `I_Error("Different DOOM versions cannot play a net game!")`.
    #[error("different DOOM versions cannot play a net game (peer sent {theirs}, we are {ours})")]
    DifferentVersion {
        /// The `player` byte of the setup packet (peer's VERSION).
        theirs: u8,
        /// Our `VERSION`.
        ours: u8,
    },
    /// ESCAPE at the "network game synchronization aborted." prompt; vanilla
    /// `I_Error` from `CheckAbort`.
    #[error("network game synchronization aborted")]
    SyncAbort,
    /// `NetUpdate` produced more than `BACKUPTICS` pending tics; vanilla
    /// `I_Error("NetUpdate: netbuffer->numtics > BACKUPTICS")`.
    #[error("netbuffer->numtics ({num_tics}) > BACKUPTICS ({BACKUPTICS})")]
    NumticsOverflow {
        /// The count that overflowed.
        num_tics: i32,
    },
    /// `TryRunTics` saw fewer tics in a node than `gametic/ticdup`; vanilla
    /// `I_Error("TryRunTics: lowtic < gametic")`.
    #[error("TryRunTics: lowtic < gametic")]
    LowticBelowGametic,
    /// The tic governor is about to run a tic no node has sent yet; vanilla
    /// `I_Error("gametic>lowtic")`.
    #[error("gametic > lowtic")]
    GameticAboveLowtic,
    /// More nodes in the `-net` list than `MAXNETNODES`.
    #[error("too many nodes: {count} (max {MAXNETNODES})")]
    TooManyNodes {
        /// The number of nodes requested (hosts + console).
        count: usize,
    },
    /// The configured console player is not in the node list.
    #[error("consoleplayer {console_player} out of range (numnodes {num_nodes})")]
    ConsolePlayerOutOfRange {
        /// The configured console player.
        console_player: usize,
        /// The size of the node list (hosts + 1).
        num_nodes: usize,
    },
    /// A remote tic failed the consistency check; vanilla
    /// `I_Error("consistency failure (%i should be %i)")`.
    #[error("consistency failure ({actual} should be {expected})")]
    ConsistencyFailure {
        /// The player whose command disagreed.
        player: usize,
        /// The locally predicted value.
        expected: i16,
        /// The value carried by the ticcmd.
        actual: i16,
    },
    /// [`NetEngine::arbitration_step`] was called after arbitration finished.
    #[error("arbitration already finished")]
    ArbitrationFinished,
}

/// The game-facing seam of the net layer.
///
/// Implemented by the game shell; the net engine calls these instead of
/// reaching into `doomstat` globals like vanilla does. All vanilla touch
/// points are accounted for:
///
/// | vanilla                | hook                                   |
/// |------------------------|----------------------------------------|
/// | `I_StartTic` + `D_ProcessEvents` | [`NetGameHooks::pump_events`]  |
/// | `G_BuildTiccmd`        | [`NetGameHooks::build_ticcmd`]          |
/// | `G_Ticker`             | [`NetGameHooks::run_game_tic`]          |
/// | `M_Ticker`             | [`NetGameHooks::menu_ticker`]           |
/// | `D_DoAdvanceDemo`      | [`NetGameHooks::try_advance_demo`]      |
/// | `gametic` (d_net.c)    | [`NetGameHooks::gametic`] / [`NetGameHooks::set_gametic`] |
/// | `demoplayback`         | [`NetGameHooks::demo_playback`]         |
/// | `demorecording`        | [`NetGameHooks::demo_recording`]        |
/// | `advancedemo`          | [`NetGameHooks::advance_demo`]          |
/// | `singletics`           | [`NetGameHooks::singletics`]            |
/// | `usergame`             | [`NetGameHooks::user_game`]             |
/// | `playeringame[]`       | [`NetGameHooks::playeringame`] / [`NetGameHooks::set_playeringame`] |
/// | `players[x].message` + `playeringame[x] = false` | [`NetGameHooks::on_player_left`] |
/// | `G_CheckDemoStatus`    | [`NetGameHooks::on_demo_status_needed`] |
/// | `I_WaitVBL(1)`         | [`NetGameHooks::wait_vbl`]              |
/// | `CheckAbort`           | [`NetGameHooks::check_abort_requested`] |
pub trait NetGameHooks {
    /// Pumps the platform input queue, exactly like `I_StartTic` followed
    /// by `D_ProcessEvents`.
    fn pump_events(&mut self);
    /// Builds the console player's ticcmd for the current `maketic`.
    fn build_ticcmd(&mut self) -> TicCmd;
    /// Runs one game tic (`G_Ticker`).
    fn run_game_tic(&mut self);
    /// Runs the menu ticker (`M_Ticker`).
    fn menu_ticker(&mut self);
    /// Lets a pending demo advance (`D_DoAdvanceDemo`).
    fn try_advance_demo(&mut self);
    /// Whether demo playback is running (`demoplayback`).
    fn demo_playback(&self) -> bool;
    /// Whether a demo is being recorded (`demorecording`).
    fn demo_recording(&self) -> bool;
    /// Whether the demo can advance (`advancedemo`).
    fn advance_demo(&self) -> bool;
    /// Whether `-singletics` mode is on (synchronous tic mode).
    fn singletics(&self) -> bool;
    /// Whether we are in a user game, not the demo screen (`usergame`).
    fn user_game(&self) -> bool;
    /// Whether player `player` is in the game (`playeringame[]`).
    fn playeringame(&self, player: usize) -> bool;
    /// Marks player `player` as (in/not in) the game (`playeringame[]`).
    fn set_playeringame(&mut self, player: usize, in_game: bool);
    /// Vanilla `players[consoleplayer].message = "Player N left the game"`
    /// plus `playeringame[player] = false`.
    fn on_player_left(&mut self, player: usize);
    /// Vanilla `G_CheckDemoStatus()` after a net player leaves while a
    /// demo is being recorded.
    fn on_demo_status_needed(&mut self);
    /// Vanilla `I_WaitVBL(1)`.
    fn wait_vbl(&mut self);
    /// ESCAPE pressed at the sync abort prompt (`CheckAbort`).
    fn check_abort_requested(&mut self) -> bool;
    /// Current `gametic`. Owned by the game shell; read by the tic governor.
    fn gametic(&self) -> i32;
    /// Sets `gametic` (the tic governor increments it once per tic).
    fn set_gametic(&mut self, value: i32);
}

/// One arbitration iteration; see [`NetEngine::arbitration_step`].
#[derive(Clone, Copy, Debug)]
enum ArbitrationState {
    /// Not started.
    Idle,
    /// Key player: sent setup, collecting acknowledgements.
    Sending {
        /// `gotinfo[]` for the current pass.
        got_info: [bool; MAXNETNODES],
    },
    /// Non-key player: waiting for the key player's setup packet.
    Listening,
    /// Arbitration completed (a further `arbitration_step` is an error).
    Finished,
}

/// The net game engine: node list, sockets, tic buffers and sync logic.
///
/// Build with [`NetEngine::start`] (binds real UDP sockets) or
/// [`NetEngine::with_transport`] (inject a transport; used by tests).
/// Drive with [`NetEngine::net_update`], [`super::tics::try_run_tics`] and
/// [`NetEngine::arbitrate_net_start`]; see the module and `mod.rs` docs for
/// the overall lifecycle.
pub struct NetEngine {
    transport: Box<dyn NetTransport>,
    /// The `-net` node list. Index == node slot; slot 0 == this console.
    nodes: [NetNode; MAXNETNODES],
    /// Number of in-game nodes (`doomcom->numnodes`).
    num_nodes: usize,
    /// `doomcom->consoleplayer`.
    console_player: usize,
    /// `doomcom->ticdup`.
    ticdup: i32,
    /// `doomcom->extratics`.
    extratics: i32,
    /// `maxsend = BACKUPTICS/(2*ticdup)-1`, clamped to at least 1.
    /// Kept for fidelity; vanilla computes and never reads it in 1.10.
    pub maxsend: i32,
    /// `doomcom->numplayers` (console + remotes).
    num_players: usize,
    /// The packet buffer (`doomcom.data` / `netbuffer`).
    packet: NetPacket,
    /// The node the last received datagram came from (`doomcom->remotenode`).
    last_node: usize,
    /// `reboundstore`/`reboundpacket`: the node-0 loopback queue.
    rebound: Option<NetPacket>,
    /// `localcmds[BACKUPTICS]`.
    localcmds: [TicCmd; CMD_BUF],
    /// `netcmds[MAXPLAYERS][BACKUPTICS]`.
    netcmds: [[TicCmd; CMD_BUF]; MAXPLAYERS],
    /// `nodeforplayer[MAXPLAYERS]`; 0-initialized (vanilla read it
    /// uninitialized — harmless there, an info leak here).
    node_for_player: [i32; MAXPLAYERS],
    /// `maketic`.
    pub maketic: i32,
    /// `gametime` (tic index of the last `NetUpdate`).
    pub(crate) game_time: i32,
    /// `skiptics`: tics to absorb before building new ones.
    pub skip_tics: i32,
    /// `frameon` (frame-skip adaptation counter).
    pub frame_on: i32,
    /// `frameskip[4]` (the last four adaptation samples).
    pub frame_skip: [bool; 4],
    /// `oldnettics` (previous sample of `nettics[0]`).
    pub old_net_tics: i32,
    /// `oldentertics` (see vanilla: a function-`static`, i.e. global).
    pub old_enter_tics: i32,
    arbitration: ArbitrationState,
}

/// A compact diagnostic view (the transport is not printable).
impl std::fmt::Debug for NetEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetEngine")
            .field("num_nodes", &self.num_nodes)
            .field("console_player", &self.console_player)
            .field("ticdup", &self.ticdup)
            .field("extratics", &self.extratics)
            .field("maketic", &self.maketic)
            .field("game_time", &self.game_time)
            .field("maxsend", &self.maxsend)
            .field("nodes", &self.nodes)
            .finish()
    }
}

impl NetEngine {
    /// `I_InitNetwork` + the state init of `D_CheckNetGame`, without sockets
    /// (injected via [`NetEngine::with_transport`]).
    ///
    /// `config.hosts` are the remote nodes in node order; the console is
    /// always node 0. `config.console_player` must be in `0..num_nodes`
    /// (vanilla accepted garbage; this port fails fast).
    fn init(config: NetConfig, transport: Box<dyn NetTransport>) -> Result<Self, NetError> {
        let num_nodes = config.hosts.len() + 1;
        if num_nodes > MAXNETNODES {
            return Err(NetError::TooManyNodes { count: num_nodes });
        }
        if config.console_player >= num_nodes {
            return Err(NetError::ConsolePlayerOutOfRange {
                console_player: config.console_player,
                num_nodes,
            });
        }
        let ticdup = config.ticdup.clamp(1, 9);
        let mut nodes: [NetNode; MAXNETNODES] =
            std::array::from_fn(|_| NetNode::with_address(None));
        for (i, host) in config.hosts.iter().enumerate() {
            nodes[i + 1] = NetNode::with_address(Some(*host));
        }
        for node in nodes.iter_mut().take(num_nodes) {
            node.in_game = true;
        }
        let maxsend = (BACKUPTICS / (2 * ticdup) - 1).max(1);
        Ok(Self {
            transport,
            nodes,
            num_nodes,
            console_player: config.console_player,
            ticdup,
            extratics: i32::from(config.extratics),
            maxsend,
            num_players: num_nodes,
            packet: NetPacket::default(),
            last_node: 0,
            rebound: None,
            localcmds: [TicCmd::default(); CMD_BUF],
            netcmds: [[TicCmd::default(); CMD_BUF]; MAXPLAYERS],
            node_for_player: [0; MAXPLAYERS],
            maketic: 0,
            game_time: 0,
            skip_tics: 0,
            frame_on: 0,
            frame_skip: [false; 4],
            old_net_tics: 0,
            old_enter_tics: 0,
            arbitration: ArbitrationState::Idle,
        })
    }

    /// Binds real UDP sockets and creates the engine (`I_InitNetwork`).
    pub fn start(config: NetConfig) -> Result<Self, NetError> {
        let mut addresses = [None; MAXNETNODES];
        for (i, host) in config.hosts.iter().enumerate() {
            addresses[i + 1] = Some(*host);
        }
        let transport = UdpTransport::new(config.port, addresses)?;
        Self::init(config, Box::new(transport))
    }

    /// Creates the engine over an injected transport (tests, fake wire).
    pub fn with_transport(
        config: NetConfig,
        transport: Box<dyn NetTransport>,
    ) -> Result<Self, NetError> {
        Self::init(config, transport)
    }

    /// The node list (read-only).
    pub fn nodes(&self) -> &[NetNode; MAXNETNODES] {
        &self.nodes
    }

    /// The `ticdup` this game runs with.
    pub fn ticdup(&self) -> i32 {
        self.ticdup
    }

    /// Number of nodes in the game (`doomcom->numnodes`).
    pub fn num_nodes(&self) -> usize {
        self.num_nodes
    }

    /// The console player index (`doomcom->consoleplayer`).
    pub fn console_player(&self) -> usize {
        self.console_player
    }

    /// Number of players in the game (`doomcom->numplayers`).
    pub fn num_players(&self) -> usize {
        self.num_players
    }

    /// `nodeforplayer[]` (read-only).
    pub fn node_for_player(&self) -> &[i32; MAXPLAYERS] {
        &self.node_for_player
    }

    /// The tic command ring the game reads (`netcmds[player][tic]`).
    pub fn netcmd(&self, player: usize, tic_slot: usize) -> TicCmd {
        self.netcmds[player][tic_slot % CMD_BUF]
    }

    /// Mutable access to the duplicated-tic ring (used by `try_run_tics`
    /// to strip special buttons on duplicated tics).
    pub(crate) fn netcmds_mut(&mut self, player: usize, tic_slot: usize) -> &mut TicCmd {
        &mut self.netcmds[player][tic_slot % CMD_BUF]
    }

    /// Finishes `D_CheckNetGame`: marks players in the game through the
    /// hooks and traces the startup summary. Idempotent.
    pub fn finish_check_net_game(&self, hooks: &mut dyn NetGameHooks) {
        for i in 0..self.num_players {
            hooks.set_playeringame(i, true);
        }
        tracing::info!(
            "net: player {} of {} ({} nodes)",
            self.console_player + 1,
            self.num_players,
            self.num_nodes
        );
    }

    /// `HSendPacket`: stamps the checksum and sends the packet to `node`
    /// (node 0 rebounds locally).
    pub fn h_send_packet(&mut self, node: usize, flags: u32, hooks: &dyn NetGameHooks) {
        stamp_checksum(&mut self.packet, flags);

        if node == 0 {
            // the console is always node 0: loop back instead of transmitting
            self.rebound = Some(self.packet);
            return;
        }
        if hooks.demo_playback() {
            return;
        }
        tracing::trace!(
            "net: send ({} + {}, R {}) [{}] to node {}",
            expand_tics(self.packet.start_tic, self.maketic),
            self.packet.num_tics,
            if self.packet.checksum & NCMD_RETRANSMIT != 0 {
                expand_tics(self.packet.retransmit_from, self.maketic)
            } else {
                -1
            },
            self.packet.wire_size(),
            node
        );
        self.transport.send_packet(node, &self.packet.encode_wire());
    }

    /// `HGetPacket`: returns `Some(node)` when a datagram is waiting, after
    /// all vanilla validation (length vs `NetbufferSize`, checksum).
    pub fn h_get_packet(&mut self, hooks: &dyn NetGameHooks) -> Result<Option<usize>, NetError> {
        if let Some(rebound) = self.rebound.take() {
            self.packet = rebound;
            self.last_node = 0;
            return Ok(Some(0));
        }
        if hooks.demo_playback() {
            return Ok(None);
        }
        let Some((node, bytes)) = self.transport.recv_packet() else {
            return Ok(None);
        };
        if bytes.len() < 8 {
            tracing::trace!(
                "net: dropping short packet ({} bytes) from node {node}",
                bytes.len()
            );
            return Ok(None);
        }
        let num_tics = bytes[7];
        let want = super::packet::NetPacket::size_for(num_tics);
        if num_tics as i32 > BACKUPTICS || bytes.len() != want {
            tracing::trace!(
                "net: bad packet length {} (expected {} for numtics {})",
                bytes.len(),
                want,
                num_tics
            );
            return Ok(None);
        }
        let Ok(packet) = NetPacket::decode_wire(&bytes) else {
            tracing::trace!("net: undecodable packet from node {node}");
            return Ok(None);
        };
        if checksum(&packet) != (packet.checksum & super::packet::NCMD_CHECKSUM) {
            tracing::trace!("net: bad packet checksum from node {node}");
            return Ok(None);
        }
        self.packet = packet;
        self.last_node = node;
        Ok(Some(node))
    }

    /// `GetPackets`: drains pending datagrams into the `netcmds` ring.
    pub fn get_packets(&mut self, hooks: &mut dyn NetGameHooks) -> Result<(), NetError> {
        while let Some(node) = self.h_get_packet(hooks)? {
            if self.packet.has_flag(NCMD_SETUP) {
                // extra setup packet
                tracing::trace!("net: stale setup packet from node {node} dropped");
                continue;
            }

            let netconsole = (self.packet.player & !PL_DRONE) as usize;
            if netconsole >= MAXPLAYERS {
                tracing::trace!(
                    "net: dropping packet with out-of-range player {}",
                    self.packet.player
                );
                continue;
            }

            // to save bytes, only the low byte of tic numbers are sent;
            // figure out what the rest of the bytes are
            let real_start = expand_tics(self.packet.start_tic, self.maketic);
            let real_end = real_start + i32::from(self.packet.num_tics);

            // check for exiting the game
            if self.packet.has_flag(NCMD_EXIT) {
                if !self.nodes[node].in_game {
                    continue;
                }
                self.nodes[node].in_game = false;
                hooks.on_player_left(netconsole);
                if hooks.demo_recording() {
                    hooks.on_demo_status_needed();
                }
                continue;
            }

            // check for a remote game kill
            if self.packet.has_flag(NCMD_KILL) {
                return Err(NetError::KilledByPeer);
            }

            self.node_for_player[netconsole] = node as i32;

            // check for retransmit request
            if self.nodes[node].resend_count <= 0 && self.packet.has_flag(NCMD_RETRANSMIT) {
                self.nodes[node].resend_to = expand_tics(self.packet.retransmit_from, self.maketic);
                tracing::trace!(
                    "net: retransmit from {} requested by node {node}",
                    self.nodes[node].resend_to
                );
                self.nodes[node].resend_count = RESENDCOUNT;
            } else {
                self.nodes[node].resend_count -= 1;
            }

            // check for out of order / duplicated packet
            if real_end == self.nodes[node].net_tics {
                continue;
            }
            if real_end < self.nodes[node].net_tics {
                tracing::trace!(
                    "net: out of order packet ({real_start} + {})",
                    self.packet.num_tics
                );
                continue;
            }

            // check for a missed packet
            if real_start > self.nodes[node].net_tics {
                // stop processing until the other system resends the missed tics
                tracing::trace!(
                    "net: missed tics from {} ({} - {})",
                    node,
                    real_start,
                    self.nodes[node].net_tics
                );
                self.nodes[node].remote_resend = true;
                continue;
            }

            // update command store from the packet
            self.nodes[node].remote_resend = false;
            let mut src = (self.nodes[node].net_tics - real_start) as usize;
            while self.nodes[node].net_tics < real_end {
                let slot = (self.nodes[node].net_tics % BACKUPTICS) as usize;
                self.netcmds[netconsole][slot] = self.packet.cmds[src];
                self.nodes[node].net_tics += 1;
                src += 1;
            }
        }
        Ok(())
    }

    /// `NetUpdate`: builds ticcmds for the console player, hands them to
    /// every in-game node, then collects incoming datagrams.
    ///
    /// `now_tics` is `I_GetTime()` in tics; `gametic` comes from the hooks.
    pub fn net_update(
        &mut self,
        now_tics: i32,
        hooks: &mut dyn NetGameHooks,
    ) -> Result<(), NetError> {
        let nowtime = now_tics / self.ticdup;
        let mut newtics = nowtime - self.game_time;
        self.game_time = nowtime;

        if newtics <= 0 {
            // nothing new to update
            self.get_packets(hooks)?;
            return Ok(());
        }

        if self.skip_tics <= newtics {
            newtics -= self.skip_tics;
            self.skip_tics = 0;
        } else {
            self.skip_tics -= newtics;
            newtics = 0;
        }

        self.packet.player = self.console_player as u8;

        // build new ticcmds for the console player
        let gameticdiv = hooks.gametic() / self.ticdup;
        for _ in 0..newtics {
            hooks.pump_events();
            if self.maketic - gameticdiv >= BACKUPTICS / 2 - 1 {
                break; // can't hold any more
            }
            let cmd = hooks.build_ticcmd();
            self.localcmds[(self.maketic % BACKUPTICS) as usize] = cmd;
            self.maketic += 1;
        }

        if hooks.singletics() {
            return Ok(()); // singletic update is synchronous
        }

        // send the packet to the other nodes
        for i in 0..self.num_nodes {
            if !self.nodes[i].in_game {
                continue;
            }
            let realstart = self.nodes[i].resend_to;
            self.packet.start_tic = realstart as u8;
            let num_tics = self.maketic - realstart;
            if num_tics > BACKUPTICS {
                return Err(NetError::NumticsOverflow { num_tics });
            }
            self.packet.num_tics = num_tics as u8;
            self.nodes[i].resend_to = self.maketic - self.extratics;
            for j in 0..num_tics {
                self.packet.cmds[j as usize] =
                    self.localcmds[((realstart + j) % BACKUPTICS) as usize];
            }
            if self.nodes[i].remote_resend {
                self.packet.retransmit_from = self.nodes[i].net_tics as u8;
                self.h_send_packet(i, NCMD_RETRANSMIT, hooks);
            } else {
                self.packet.retransmit_from = 0;
                self.h_send_packet(i, 0, hooks);
            }
        }

        // listen for other packets
        self.get_packets(hooks)
    }

    /// `CheckAbort`: ESCAPE at the abort prompt. The hooks report whether
    /// the key was pressed; `CheckAbort`'s job ends here.
    pub fn check_abort(&self, hooks: &mut dyn NetGameHooks) -> Result<(), NetError> {
        if hooks.check_abort_requested() {
            Err(NetError::SyncAbort)
        } else {
            Ok(())
        }
    }

    /// One arbitration iteration (`D_ArbitrateNetStart`).
    ///
    /// - Key player (`console_player == 0`): sends the setup packet to
    ///   every node, then collects up to 10 acknowledgement packets,
    ///   marking `gotinfo` for each player byte `< MAXNETNODES`. Returns
    ///   `Some(settings)` once every remote node acknowledged.
    /// - Anyone else: waits for the setup packet. A packet that is not a
    ///   `NCMD_SETUP` packet is dropped (other nodes may already be in
    ///   game); returns `Some(settings)` once a valid setup packet arrives.
    ///
    /// ESCAPE aborts with [`NetError::SyncAbort`]. Callers loop this; the
    /// blocking [`NetEngine::arbitrate_net_start`] does exactly that.
    pub fn arbitration_step(
        &mut self,
        hooks: &mut dyn NetGameHooks,
        settings: &NetGameSettings,
    ) -> Result<Option<NetGameSettings>, NetError> {
        match self.arbitration {
            ArbitrationState::Idle => {
                self.arbitration = if self.console_player == 0 {
                    ArbitrationState::Sending {
                        got_info: [false; MAXNETNODES],
                    }
                } else {
                    ArbitrationState::Listening
                };
                // fall through to the active branch below
                self.arbitration_step(hooks, settings)
            }
            ArbitrationState::Listening => {
                self.check_abort(hooks)?;
                let Some(node) = self.h_get_packet(hooks)? else {
                    return Ok(None);
                };
                let _ = node;
                if !self.packet.has_flag(NCMD_SETUP) {
                    return Ok(None); // not a setup packet, keep waiting
                }
                if self.packet.player != DOOM_VERSION {
                    return Err(NetError::DifferentVersion {
                        theirs: self.packet.player,
                        ours: DOOM_VERSION,
                    });
                }
                let decoded = NetGameSettings::decode(&self.packet);
                Ok(Some(decoded))
            }
            ArbitrationState::Sending { mut got_info } => {
                // pull the flags out of `self` so the collect loop can use
                // `self` freely, put them back at the end
                let mut got_info = std::mem::replace(&mut got_info, [false; MAXNETNODES]);
                self.check_abort(hooks)?;
                let (retransmit, start_tic) = settings.encode();
                for i in 0..self.num_nodes {
                    self.packet.retransmit_from = retransmit;
                    self.packet.start_tic = start_tic;
                    self.packet.player = DOOM_VERSION;
                    self.packet.num_tics = 0;
                    self.h_send_packet(i, NCMD_SETUP, hooks);
                }
                for _ in 0..10 {
                    if self.h_get_packet(hooks)?.is_some() {
                        let who = (self.packet.player & 0x7f) as usize;
                        if who < MAXNETNODES {
                            got_info[who] = true;
                        }
                    } else {
                        break;
                    }
                }
                for i in 1..self.num_nodes {
                    if !got_info[i] {
                        self.arbitration = ArbitrationState::Sending { got_info };
                        return Ok(None);
                    }
                }
                self.arbitration = ArbitrationState::Finished;
                Ok(Some(*settings))
            }
            ArbitrationState::Finished => Err(NetError::ArbitrationFinished),
        }
    }

    /// `D_ArbitrateNetStart`: block until this node is done arbitrating.
    ///
    /// Loops [`NetEngine::arbitration_step`] forever; the only escapes are a
    /// completed arbitration or [`NetError::SyncAbort`]. Tests drive the
    /// stepped form instead — see `tests` in this module.
    pub fn arbitrate_net_start(
        &mut self,
        hooks: &mut dyn NetGameHooks,
        settings: &NetGameSettings,
    ) -> Result<NetGameSettings, NetError> {
        loop {
            if let Some(done) = self.arbitration_step(hooks, settings)? {
                return Ok(done);
            }
        }
    }

    /// `D_QuitNetGame`: sends `NCMD_EXIT` to the other nodes, four times
    /// each, so the survivors don't hang waiting for us.
    pub fn quit_net_game(&mut self, hooks: &mut dyn NetGameHooks) {
        if !hooks.user_game() || hooks.demo_playback() {
            return;
        }
        self.packet.player = self.console_player as u8;
        self.packet.num_tics = 0;
        for _ in 0..4 {
            for j in 1..self.num_nodes {
                if self.nodes[j].in_game {
                    self.h_send_packet(j, NCMD_EXIT, hooks);
                }
            }
            hooks.wait_vbl();
        }
    }

    /// The lowest `nettics` across in-game nodes (`lowtic`).
    pub fn lowtic(&self) -> i32 {
        let mut lowtic = i32::MAX;
        for node in self.nodes.iter().take(self.num_nodes) {
            if node.in_game && node.net_tics < lowtic {
                lowtic = node.net_tics;
            }
        }
        lowtic
    }

    /// How many nodes are in the game (`numplaying`).
    pub fn num_playing(&self) -> usize {
        self.nodes
            .iter()
            .take(self.num_nodes)
            .filter(|n| n.in_game)
            .count()
    }

    /// Local tics built but not yet acknowledged (`maketic` minus the
    /// console's own `nettics`). Test/diagnostic helper.
    pub fn tics_in_flight(&self) -> i32 {
        self.maketic - self.nodes[0].net_tics
    }
}

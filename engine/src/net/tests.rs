//! Tests for the net layer.
//!
//! Two styles, both deterministic:
//!
//! - Scripted wire: each engine gets a [`NetTransport`] backed by
//!   `Rc<RefCell<..>>` queues; the test shuffles datagrams between peers
//!   with [`pump`]. Packet loss, reordering and duplication are injected
//!   by hand — no sleeping, no flake.
//! - Real loopback UDP: two [`super::UdpTransport`] instances on
//!   `127.0.0.1` playing a short net game (the two-instance scenario a
//!   WAD-less environment can afford; ports are probed, not hardcoded).
//!
//! Everything rate-shaped is driven by [`CountingTime`]/[`ManualTime`],
//! not the wall clock.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr};
use std::rc::Rc;

use crate::player::{MAXPLAYERS, TicCmd};

use super::engine::{NetConfig, NetEngine, NetError, NetGameHooks};
use super::packet::{
    BACKUPTICS, DOOM_VERSION, NCMD_EXIT, NCMD_KILL, NCMD_RETRANSMIT, NCMD_SETUP, NetGameSettings,
    NetPacket, stamp_checksum,
};
use super::socket::NetTransport;
use super::tics::{
    ConsistencyCheck, ConsistencyGate, TryRunTicsOutcome, decide_counts, try_run_tics,
};
use super::time::{CountingTime, ManualTime};

// -- mock game hooks ---------------------------------------------------------

struct MockHooks {
    gametic: i32,
    playeringame: [bool; MAXPLAYERS],
    demo_playback: bool,
    demo_recording: bool,
    advance_demo: bool,
    singletics: bool,
    user_game: bool,
    build_ticcmd: TicCmd,
    pump_calls: u32,
    tic_calls: u32,
    menu_calls: u32,
    advance_calls: u32,
    player_left: Vec<usize>,
    demo_status_calls: u32,
    wait_vbl_calls: u32,
    abort: bool,
}

impl Default for MockHooks {
    fn default() -> Self {
        Self {
            gametic: 0,
            playeringame: [true, false, false, false],
            demo_playback: false,
            demo_recording: false,
            advance_demo: false,
            singletics: false,
            user_game: true,
            build_ticcmd: TicCmd::default(),
            pump_calls: 0,
            tic_calls: 0,
            menu_calls: 0,
            advance_calls: 0,
            player_left: Vec::new(),
            demo_status_calls: 0,
            wait_vbl_calls: 0,
            abort: false,
        }
    }
}

impl NetGameHooks for MockHooks {
    fn pump_events(&mut self) {
        self.pump_calls += 1;
    }
    fn build_ticcmd(&mut self) -> TicCmd {
        self.build_ticcmd
    }
    fn run_game_tic(&mut self) {
        self.tic_calls += 1;
    }
    fn menu_ticker(&mut self) {
        self.menu_calls += 1;
    }
    fn try_advance_demo(&mut self) {
        self.advance_calls += 1;
    }
    fn demo_playback(&self) -> bool {
        self.demo_playback
    }
    fn demo_recording(&self) -> bool {
        self.demo_recording
    }
    fn advance_demo(&self) -> bool {
        self.advance_demo
    }
    fn singletics(&self) -> bool {
        self.singletics
    }
    fn user_game(&self) -> bool {
        self.user_game
    }
    fn playeringame(&self, player: usize) -> bool {
        self.playeringame[player]
    }
    fn set_playeringame(&mut self, player: usize, in_game: bool) {
        self.playeringame[player] = in_game;
    }
    fn on_player_left(&mut self, player: usize) {
        self.playeringame[player] = false;
        self.player_left.push(player);
    }
    fn on_demo_status_needed(&mut self) {
        self.demo_status_calls += 1;
    }
    fn wait_vbl(&mut self) {
        self.wait_vbl_calls += 1;
    }
    fn check_abort_requested(&mut self) -> bool {
        self.abort
    }
    fn gametic(&self) -> i32 {
        self.gametic
    }
    fn set_gametic(&mut self, value: i32) {
        self.gametic = value;
    }
}

// -- scripted wire -----------------------------------------------------------

type OutQueue = Rc<RefCell<Vec<(usize, Vec<u8>)>>>;
type InQueue = Rc<RefCell<VecDeque<(usize, Vec<u8>)>>>;

struct ScriptedEnd {
    out: OutQueue,
    inb: InQueue,
    drop_next: Rc<Cell<u32>>,
    drop_all: Rc<Cell<bool>>,
}

impl NetTransport for ScriptedEnd {
    fn send_packet(&mut self, node: usize, data: &[u8]) {
        if self.drop_all.get() {
            return;
        }
        let pending = self.drop_next.get();
        if pending > 0 {
            self.drop_next.set(pending - 1);
            return;
        }
        self.out.borrow_mut().push((node, data.to_vec()));
    }
    fn recv_packet(&mut self) -> Option<(usize, Vec<u8>)> {
        self.inb.borrow_mut().pop_front()
    }
}

struct Peer {
    engine: NetEngine,
    hooks: MockHooks,
    out: OutQueue,
    inb: InQueue,
    drop_next: Rc<Cell<u32>>,
    drop_all: Rc<Cell<bool>>,
    slot_map: Vec<usize>,
}

fn fake_addr(id: usize) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 41000 + id as u16))
}

/// Builds `n` peers, console players 0..n, each with a scripted wire whose
/// slot map lines up with the peers' ids (slot j, j >= 1, points at the
/// peer that host list entry names).
fn make_peers(n: usize) -> Vec<Peer> {
    let mut peers = Vec::with_capacity(n);
    for k in 0..n {
        let out: OutQueue = Rc::new(RefCell::new(Vec::new()));
        let inb: InQueue = Rc::new(RefCell::new(VecDeque::new()));
        let drop_next = Rc::new(Cell::new(0));
        let drop_all = Rc::new(Cell::new(false));
        let slot_map: Vec<usize> = [usize::MAX]
            .into_iter()
            .chain((0..n).filter(|&j| j != k))
            .collect();
        let end = ScriptedEnd {
            out: out.clone(),
            inb: inb.clone(),
            drop_next: drop_next.clone(),
            drop_all: drop_all.clone(),
        };
        let hosts: Vec<SocketAddr> = (0..n).filter(|&j| j != k).map(fake_addr).collect();
        let config = NetConfig {
            console_player: k,
            ticdup: 1,
            extratics: false,
            hosts,
            port: 0,
        };
        let engine = NetEngine::with_transport(config, Box::new(end)).expect("scripted engine");
        let mut hooks = MockHooks {
            build_ticcmd: TicCmd {
                forward_move: (10 + k) as i8,
                side_move: 0,
                angle_turn: 100 * (k as i16 + 1),
                consistancy: 1000 + k as i16,
                chat_char: 0,
                buttons: 0,
            },
            ..MockHooks::default()
        };
        for slot in hooks.playeringame.iter_mut().take(n) {
            *slot = true;
        }
        peers.push(Peer {
            engine,
            hooks,
            out,
            inb,
            drop_next,
            drop_all,
            slot_map,
        });
    }
    peers
}

/// Delivers every queued datagram to the peer the sender's slot points at.
/// The packet is tagged with the sender's slot in the *receiver's* node
/// list (that's what `HGetPacket` reports as `doomcom->remotenode`).
fn pump(peers: &mut [Peer]) {
    let mut moves = Vec::new();
    for (i, peer) in peers.iter().enumerate() {
        for (slot, bytes) in peer.out.borrow_mut().drain(..) {
            moves.push((i, slot, bytes));
        }
    }
    for (sender, slot, bytes) in moves {
        let target = peers[sender].slot_map[slot];
        let rslot = peers[target]
            .slot_map
            .iter()
            .position(|&x| x == sender)
            .expect("sender must be in the receiver's node list");
        peers[target].inb.borrow_mut().push_back((rslot, bytes));
    }
}

fn peer_net_update(peers: &mut [Peer], now_tics: i32) {
    for peer in peers.iter_mut() {
        let Peer { engine, hooks, .. } = peer;
        engine
            .net_update(now_tics, hooks)
            .expect("net_update failed");
    }
}

/// Destructured engine+hooks for one peer; avoids indexing `peers[i]` twice.
fn get_packets(peer: &mut Peer) -> Result<(), NetError> {
    let Peer { engine, hooks, .. } = peer;
    engine.get_packets(hooks)
}

fn arbitration_step(
    peer: &mut Peer,
    settings: &NetGameSettings,
) -> Result<Option<NetGameSettings>, NetError> {
    let Peer { engine, hooks, .. } = peer;
    engine.arbitration_step(hooks, settings)
}

fn quit_net_game(peer: &mut Peer) {
    let Peer { engine, hooks, .. } = peer;
    engine.quit_net_game(hooks)
}

fn settings() -> NetGameSettings {
    NetGameSettings {
        skill: 4,
        deathmatch: 1,
        nomonsters: true,
        respawn: false,
        episode: 2,
        map: 3,
    }
}

fn setup_packet(settings: &NetGameSettings) -> NetPacket {
    let (retransmit, start_tic) = settings.encode();
    let mut p = NetPacket {
        player: DOOM_VERSION,
        num_tics: 0,
        retransmit_from: retransmit,
        start_tic,
        ..NetPacket::default()
    };
    stamp_checksum(&mut p, NCMD_SETUP);
    p
}

fn cmd_packet(start: u8, cmds: &[TicCmd], flags: u32) -> NetPacket {
    let mut p = NetPacket {
        start_tic: start,
        num_tics: cmds.len() as u8,
        ..NetPacket::default()
    };
    for (i, cmd) in cmds.iter().enumerate() {
        p.cmds[i] = *cmd;
    }
    stamp_checksum(&mut p, flags);
    p
}

fn tic(forward: i8) -> TicCmd {
    TicCmd {
        forward_move: forward,
        side_move: 0,
        angle_turn: 0,
        consistancy: 0,
        chat_char: 0,
        buttons: 0,
    }
}

// -- decide_counts -----------------------------------------------------------

#[test]
fn decide_counts_catches_up() {
    assert_eq!(decide_counts(0, 5), 1); // one frame ahead of a slow remote
    assert_eq!(decide_counts(3, 10), 4);
    assert_eq!(decide_counts(9, 10), 9);
}

#[test]
fn decide_counts_steady_state() {
    assert_eq!(decide_counts(10, 10), 10);
    assert_eq!(decide_counts(7, 10), 8); // realtics < availabletics-1 -> +1
    assert_eq!(decide_counts(0, 0), 1); // nothing available, still run one tic
}

#[test]
fn decide_counts_starved_and_clamped() {
    assert_eq!(decide_counts(0, -10), 1); // negative availabletics -> floor of 1
    assert_eq!(decide_counts(100, i32::MIN), 1); // saturating_sub cannot wrap
    assert_eq!(decide_counts(0, i32::MAX), 1); // realtics + 1
}

// -- single-node loopback ----------------------------------------------------

#[test]
fn solo_net_game_runs_ticcmds_through_the_rebound() {
    let mut peers = make_peers(1);
    let peer = &mut peers[0];
    // no time passed: nothing built
    peer.engine
        .net_update(0, &mut peer.hooks)
        .expect("net_update");
    assert_eq!(peer.hooks.pump_calls, 0);
    assert_eq!(peer.engine.maketic, 0);

    // one tic worth of time: build, rebound, collect
    peer.engine
        .net_update(1, &mut peer.hooks)
        .expect("net_update");
    assert_eq!(peer.engine.maketic, 1);
    assert_eq!(peer.engine.nodes()[0].net_tics, 1);
    assert_eq!(peer.engine.netcmd(0, 0).forward_move, 10);
    assert!(
        peer.out.borrow().is_empty(),
        "node 0 must not touch the wire"
    );
}

#[test]
fn solo_net_game_arbitrates_instantly_and_runs() {
    let mut peers = make_peers(1);
    let settings = settings();
    let peer = &mut peers[0];
    let done = peer
        .engine
        .arbitration_step(&mut peer.hooks, &settings)
        .expect("arbitration");
    assert_eq!(done, Some(settings));

    let time = CountingTime::new();
    let outcome = try_run_tics(&mut peer.engine, &time, &mut peer.hooks).expect("try_run_tics");
    assert_eq!(outcome, TryRunTicsOutcome::Completed { tics: 1 });
    assert_eq!(peer.hooks.gametic, 1);
    assert_eq!(peer.hooks.tic_calls, 1);
    assert_eq!(peer.hooks.menu_calls, 1);
    assert_eq!(peer.hooks.pump_calls, 2);
}

// -- ticcmd exchange ----------------------------------------------------------

#[test]
fn three_nodes_exchange_ticcmds() {
    let mut peers = make_peers(3);
    peer_net_update(&mut peers, 1); // build tic 0 and send it
    pump(&mut peers);
    peer_net_update(&mut peers, 1); // drain the inboxes (listen)
    peer_net_update(&mut peers, 2); // build tic 1 and send it
    pump(&mut peers);
    peer_net_update(&mut peers, 2); // drain the inboxes again

    for (k, peer) in peers.iter().enumerate() {
        for other in 0..3 {
            if other == k {
                continue;
            }
            // slot index of `other` in `peer`'s node list
            let node = if other < k { other + 1 } else { other };
            assert_eq!(
                peer.engine.nodes()[node].net_tics,
                2,
                "peer {k} should have 2 tics from peer {other}"
            );
        }
        assert_eq!(peer.engine.maketic, 2);
    }

    // peer 1's tics land in peer 0's netcmds under player index 1
    assert_eq!(peers[0].engine.netcmd(1, 0).forward_move, 11);
    assert_eq!(peers[0].engine.netcmd(1, 1).forward_move, 11);
    assert_eq!(peers[0].engine.netcmd(2, 0).forward_move, 12);
    // and peer 0's own tic is player 0
    assert_eq!(peers[0].engine.netcmd(0, 0).forward_move, 10);
    // the on-wire angle turn survives the roundtrip
    assert_eq!(peers[0].engine.netcmd(2, 0).angle_turn, 300);
    assert_eq!(peers[0].engine.netcmd(2, 0).consistancy, 1002);
}

#[test]
fn full_ring_wraps_after_backuptics() {
    let mut peers = make_peers(2);
    let base = peers[1].hooks.build_ticcmd;
    let head: Vec<TicCmd> = (0..BACKUPTICS).map(|_| base).collect();
    let mut pkt = cmd_packet(0, &head, 0);
    pkt.player = 1; // peer 1's console player index, baked into the checksum
    stamp_checksum(&mut pkt, 0);
    peers[0].inb.borrow_mut().push_back((1, pkt.encode_wire()));
    let mut tail = cmd_packet(BACKUPTICS as u8, &[base], 0);
    tail.player = 1;
    stamp_checksum(&mut tail, 0);
    peers[0].inb.borrow_mut().push_back((1, tail.encode_wire()));
    get_packets(&mut peers[0]).expect("get_packets");
    assert_eq!(peers[0].engine.nodes()[1].net_tics, BACKUPTICS + 1);
    // slot 0 was overwritten by tic number BACKUPTICS
    assert_eq!(peers[0].engine.netcmd(1, 0).forward_move, base.forward_move);
}

// -- retransmit flood suppression ---------------------------------------------

#[test]
fn retransmit_requests_are_suppressed_within_the_resend_window() {
    let mut peers = make_peers(2);
    let base = peers[1].hooks.build_ticcmd;
    macro_rules! push {
        ($p:expr) => {
            peers[0].inb.borrow_mut().push_back((1, $p.encode_wire()))
        };
    }
    // first retransmit request: accepted, window opens
    let mut req = cmd_packet(5, &[base], NCMD_RETRANSMIT);
    req.player = 1;
    req.retransmit_from = 5;
    stamp_checksum(&mut req, NCMD_RETRANSMIT);
    push!(req);
    get_packets(&mut peers[0]).expect("get_packets");
    assert_eq!(peers[0].engine.nodes()[1].resend_to, 5);
    assert_eq!(
        peers[0].engine.nodes()[1].resend_count,
        super::packet::RESENDCOUNT
    );

    // another request inside the window: decrements the counter, keeps the old tic
    let mut req2 = cmd_packet(6, &[base], NCMD_RETRANSMIT);
    req2.player = 1;
    req2.retransmit_from = 6;
    stamp_checksum(&mut req2, NCMD_RETRANSMIT);
    push!(req2);
    get_packets(&mut peers[0]).expect("get_packets");
    assert_eq!(
        peers[0].engine.nodes()[1].resend_to,
        5,
        "still the first request"
    );
    assert_eq!(
        peers[0].engine.nodes()[1].resend_count,
        super::packet::RESENDCOUNT - 1
    );

    // exhaust the window with ordinary packets
    for from in 7..(7 + super::packet::RESENDCOUNT - 1) {
        let mut p = cmd_packet(from as u8 % BACKUPTICS as u8, &[base], 0);
        p.player = 1;
        stamp_checksum(&mut p, 0);
        push!(p);
    }
    get_packets(&mut peers[0]).expect("get_packets");
    assert_eq!(peers[0].engine.nodes()[1].resend_count, 0);

    // window closed: the next request is accepted again
    let mut req3 = cmd_packet(9, &[base], NCMD_RETRANSMIT);
    req3.player = 1;
    req3.retransmit_from = 9;
    stamp_checksum(&mut req3, NCMD_RETRANSMIT);
    push!(req3);
    get_packets(&mut peers[0]).expect("get_packets");
    assert_eq!(peers[0].engine.nodes()[1].resend_to, 9);
    assert_eq!(
        peers[0].engine.nodes()[1].resend_count,
        super::packet::RESENDCOUNT
    );
}

// -- duplicate / out-of-order / missed packets --------------------------------

#[test]
fn duplicate_and_stale_packets_are_dropped() {
    let mut peers = make_peers(2);
    let base = peers[1].hooks.build_ticcmd;
    let pkt = cmd_packet(0, &[base, base], 0);
    peers[0].inb.borrow_mut().push_back((1, pkt.encode_wire()));
    get_packets(&mut peers[0]).expect("get_packets");
    assert_eq!(peers[0].engine.nodes()[1].net_tics, 2);

    // exact duplicate
    let dup = cmd_packet(0, &[base, base], 0);
    peers[0].inb.borrow_mut().push_back((1, dup.encode_wire()));
    // stale fragment (realend 1 < 2)
    let stale = cmd_packet(0, &[base], 0);
    peers[0]
        .inb
        .borrow_mut()
        .push_back((1, stale.encode_wire()));
    // gap (starts at 5, we have 2)
    let gap = cmd_packet(5, &[base], 0);
    peers[0].inb.borrow_mut().push_back((1, gap.encode_wire()));
    get_packets(&mut peers[0]).expect("get_packets");

    assert_eq!(
        peers[0].engine.nodes()[1].net_tics,
        2,
        "stale/dup tics must not advance"
    );
    assert!(
        peers[0].engine.nodes()[1].remote_resend,
        "missed tics must request a resend"
    );
}

#[test]
fn missing_tics_trigger_retransmit_and_recover() {
    let mut peers = make_peers(2);
    // peer 1's first packet is lost on the wire
    peers[1].drop_next.set(1);

    for t in 1..=5 {
        peer_net_update(&mut peers, t);
        pump(&mut peers);
    }
    peer_net_update(&mut peers, 5); // drain: B processes the last in-flight packet

    // peer 0 sent its tics; peer 1 dropped the first one, stalled on the
    // missing tic, asked for a retransmit and got everything in the end
    assert_eq!(peers[1].engine.nodes()[1].net_tics, peers[0].engine.maketic);
    assert_eq!(peers[1].engine.netcmd(0, 0).forward_move, 10);
    assert_eq!(
        peers[1]
            .engine
            .netcmd(0, peers[0].engine.maketic as usize - 1)
            .forward_move,
        10
    );
    assert!(!peers[1].engine.nodes()[1].remote_resend);
}

// -- arbitration ---------------------------------------------------------------

#[test]
fn three_player_setup_sequence() {
    let mut peers = make_peers(3);
    let settings = settings();

    // round 1: the key player sends setup to everyone
    let done = arbitration_step(&mut peers[0], &settings).expect("key step");
    assert_eq!(done, None);
    assert_eq!(peers[0].out.borrow().len(), 2); // one SETUP per remote node

    pump(&mut peers);

    // round 2: the remotes receive setup and are done
    for k in [1, 2] {
        let done = arbitration_step(&mut peers[k], &settings).expect("listener step");
        assert_eq!(done, Some(settings));
    }

    // the remotes start their games; their ticcmds ack the key player
    peer_net_update(&mut peers, 1);
    pump(&mut peers);

    // round 3: the key player collects the acks and finishes
    let done = arbitration_step(&mut peers[0], &settings).expect("key step");
    assert_eq!(done, Some(settings));
}

#[test]
fn listener_ignores_non_setup_packets() {
    let mut peers = make_peers(2);
    // a ticcmd packet from the key player (e.g. it started its game already)
    let stray = cmd_packet(0, &[tic(1)], 0);
    peers[1]
        .inb
        .borrow_mut()
        .push_back((0, stray.encode_wire()));
    let done = arbitration_step(&mut peers[1], &settings()).expect("listener step");
    assert_eq!(done, None, "non-setup packets must not satisfy arbitration");

    // then the real setup packet arrives
    let setup = setup_packet(&settings());
    peers[1]
        .inb
        .borrow_mut()
        .push_back((0, setup.encode_wire()));
    let done = arbitration_step(&mut peers[1], &settings()).expect("listener step");
    assert_eq!(done, Some(settings()));
}

#[test]
fn listener_rejects_foreign_versions() {
    let mut peers = make_peers(2);
    let mut setup = setup_packet(&settings());
    setup.player = 111; // not the vanilla VERSION
    stamp_checksum(&mut setup, super::packet::NCMD_SETUP); // re-stamp: checksum covers the player byte
    peers[1]
        .inb
        .borrow_mut()
        .push_back((0, setup.encode_wire()));
    let err = arbitration_step(&mut peers[1], &settings())
        .expect_err("a foreign version must be rejected");
    assert!(matches!(
        err,
        NetError::DifferentVersion {
            theirs: 111,
            ours: 110
        }
    ));
}

#[test]
fn arbitration_aborts_on_escape() {
    let mut peers = make_peers(2);
    peers[0].hooks.abort = true;
    let err = arbitration_step(&mut peers[0], &settings()).expect_err("escape must abort");
    assert!(matches!(err, NetError::SyncAbort));
}

#[test]
fn quit_net_game_sends_exit_and_survivors_mark_the_player_left() {
    let mut peers = make_peers(2);
    quit_net_game(&mut peers[0]);
    assert_eq!(peers[0].hooks.wait_vbl_calls, 4);

    pump(&mut peers);
    get_packets(&mut peers[1]).expect("get_packets");
    assert_eq!(peers[1].hooks.player_left, vec![0]);
    assert!(!peers[1].engine.nodes()[1].in_game);
    // the remaining EXIT packets of the same player are ignored
    pump(&mut peers);
    get_packets(&mut peers[1]).expect("get_packets");
    assert_eq!(peers[1].hooks.player_left, vec![0]);
}

#[test]
fn kill_packet_is_fatal() {
    let mut peers = make_peers(2);
    let pkt = cmd_packet(0, &[tic(1)], NCMD_KILL);
    peers[1].inb.borrow_mut().push_back((0, pkt.encode_wire()));
    let err = get_packets(&mut peers[1]).expect_err("NCMD_KILL must be fatal");
    assert!(matches!(err, NetError::KilledByPeer));
}

#[test]
fn tampered_checksum_is_rejected() {
    let mut peers = make_peers(2);
    let base = tic(4);
    let mut bytes = cmd_packet(0, &[base, base], 0).encode_wire();
    bytes[3] ^= 0xff; // flip a payload checksum byte without restamping
    peers[1].inb.borrow_mut().push_back((0, bytes));
    get_packets(&mut peers[1]).expect("get_packets");
    assert_eq!(peers[1].engine.nodes()[1].net_tics, 0);
}

// -- tic governor --------------------------------------------------------------

#[test]
fn try_run_tics_runs_catch_up_frames() {
    let mut peers = make_peers(1);
    let time = CountingTime::new();
    let peer = &mut peers[0];
    let outcome = try_run_tics(&mut peer.engine, &time, &mut peer.hooks).expect("try_run_tics");
    assert_eq!(outcome, TryRunTicsOutcome::Completed { tics: 1 });
    assert_eq!(peer.hooks.gametic, 1);
    assert_eq!(peer.hooks.tic_calls, 1);
}

#[test]
fn try_run_tics_bails_to_menu_when_starved_of_remote_tics() {
    let mut peers = make_peers(2);
    // peer 1 never sends; peer 0 runs out of tics and waits
    peers[1].drop_all.set(true);
    let time = CountingTime::new();
    let peer = &mut peers[0];
    let outcome = try_run_tics(&mut peer.engine, &time, &mut peer.hooks).expect("try_run_tics");
    assert_eq!(outcome, TryRunTicsOutcome::BailoutToMenu);
    assert_eq!(peers[0].hooks.tic_calls, 0);
    assert!(peers[0].hooks.menu_calls >= 1);
}

#[test]
fn try_run_tics_errors_when_lowtic_is_behind_gametic() {
    let mut peers = make_peers(2);
    peers[0].hooks.gametic = 350; // game tic far ahead of anything we saw
    let time = ManualTime::at(0); // frozen clock: only the lowtic check can stop the loop
    let peer = &mut peers[0];
    let err = try_run_tics(&mut peer.engine, &time, &mut peer.hooks).expect_err("must fail");
    assert!(matches!(err, NetError::LowticBelowGametic));
}

#[test]
fn duplicated_tic_strips_special_buttons() {
    // ticdup = 2: the first of each duplicated pair must have chat_char
    // and BT_SPECIAL buttons stripped from the netcmds ring
    let mut peer = make_peers(1).remove(0);
    let out = peer.out.clone();
    let inb = peer.inb.clone();
    let drop_next = peer.drop_next.clone();
    let drop_all = peer.drop_all.clone();
    peer.engine = NetEngine::with_transport(
        NetConfig {
            console_player: 0,
            ticdup: 2,
            extratics: false,
            hosts: vec![],
            port: 0,
        },
        Box::new(ScriptedEnd {
            out,
            inb,
            drop_next,
            drop_all,
        }),
    )
    .unwrap();
    peer.hooks.build_ticcmd.buttons = super::tics::BT_SPECIAL | 1;
    peer.hooks.build_ticcmd.chat_char = b'q';
    let time = CountingTime::new();
    let outcome = try_run_tics(&mut peer.engine, &time, &mut peer.hooks).expect("try_run_tics");
    assert_eq!(outcome, TryRunTicsOutcome::Completed { tics: 2 });

    let stripped = peer.engine.netcmd(0, 0);
    assert_eq!(stripped.buttons & super::tics::BT_SPECIAL, 0);
    assert_eq!(stripped.chat_char, 0);
    let kept = peer.engine.netcmd(0, 1);
    assert_eq!(
        kept.buttons & super::tics::BT_SPECIAL,
        super::tics::BT_SPECIAL
    );
    assert_eq!(kept.chat_char, b'q');
}

#[test]
fn skipped_tics_absorb_input() {
    let mut peers = make_peers(1);
    let peer = &mut peers[0];
    peer.engine.skip_tics = 3;
    peer.engine
        .net_update(3, &mut peer.hooks)
        .expect("net_update");
    assert_eq!(peer.engine.skip_tics, 0);
    assert_eq!(peer.engine.maketic, 0); // everything absorbed
    peer.engine
        .net_update(4, &mut peer.hooks)
        .expect("net_update");
    assert_eq!(peer.engine.maketic, 1);
}

#[test]
fn maxsend_matches_vanilla_formula() {
    for (ticdup, expected) in [(1, 5), (3, 1), (6, 1), (9, 1)] {
        let config = NetConfig {
            console_player: 0,
            ticdup,
            extratics: false,
            hosts: vec![],
            port: 0,
        };
        let engine = NetEngine::with_transport(
            config,
            Box::new(ScriptedEnd {
                out: Rc::new(RefCell::new(Vec::new())),
                inb: Rc::new(RefCell::new(VecDeque::new())),
                drop_next: Rc::new(Cell::new(0)),
                drop_all: Rc::new(Cell::new(false)),
            }),
        )
        .unwrap();
        assert_eq!(engine.maxsend, expected, "ticdup {ticdup}");
    }
}

// -- config validation -----------------------------------------------------------

#[test]
fn too_many_nodes_is_rejected() {
    let config = NetConfig {
        console_player: 0,
        ticdup: 1,
        extratics: false,
        hosts: (0..8).map(fake_addr).collect(),
        port: 0,
    };
    let err = NetEngine::with_transport(
        config,
        Box::new(ScriptedEnd {
            out: Rc::new(RefCell::new(Vec::new())),
            inb: Rc::new(RefCell::new(VecDeque::new())),
            drop_next: Rc::new(Cell::new(0)),
            drop_all: Rc::new(Cell::new(false)),
        }),
    )
    .expect_err("9 nodes must be rejected");
    assert!(matches!(err, NetError::TooManyNodes { count: 9 }));
}

#[test]
fn console_player_out_of_range_is_rejected() {
    let config = NetConfig {
        console_player: 3,
        ticdup: 1,
        extratics: false,
        hosts: vec![fake_addr(1)],
        port: 0,
    };
    let err = NetEngine::with_transport(
        config,
        Box::new(ScriptedEnd {
            out: Rc::new(RefCell::new(Vec::new())),
            inb: Rc::new(RefCell::new(VecDeque::new())),
            drop_next: Rc::new(Cell::new(0)),
            drop_all: Rc::new(Cell::new(false)),
        }),
    )
    .expect_err("console player 3 with 2 nodes must be rejected");
    assert!(matches!(
        err,
        NetError::ConsolePlayerOutOfRange {
            console_player: 3,
            num_nodes: 2
        }
    ));
}

// -- consistency check -----------------------------------------------------------

#[test]
fn consistency_check_stores_and_verifies() {
    let mut check = ConsistencyCheck::new();
    let cmd = TicCmd {
        consistancy: 55,
        ..TicCmd::default()
    };
    for gametic in 0..=BACKUPTICS {
        check
            .check_and_store(&cmd, ConsistencyGate::netgame(), gametic, 0, 1000)
            .expect("filling phase");
    }
    assert_eq!(check.predicted(0, 0), 1000);

    // a divergent ticcmd for a filled slot is caught once past BACKUPTICS
    // (the stored prediction is the source_x we fed, like mo->x)
    let mut evil = cmd;
    evil.consistancy = 56;
    let err = check
        .check_and_store(&evil, ConsistencyGate::netgame(), BACKUPTICS + 1, 0, 1001)
        .expect_err("consistency failure must be reported");
    assert!(matches!(
        err,
        NetError::ConsistencyFailure {
            player: 0,
            expected: 1000,
            actual: 56
        }
    ));
}

#[test]
fn consistency_check_truncates_to_short_like_vanilla() {
    let mut check = ConsistencyCheck::new();
    let cmd = TicCmd::default();
    check
        .check_and_store(&cmd, ConsistencyGate::netgame(), 0, 0, 0x1_2345)
        .expect("store");
    assert_eq!(check.predicted(0, 0), 0x2345); // int -> short truncation
}

#[test]
fn consistency_check_respects_gates() {
    let mut check = ConsistencyCheck::new();
    let cmd = TicCmd {
        consistancy: 55,
        ..TicCmd::default()
    };
    // not a net game: never checks
    check
        .check_and_store(
            &cmd,
            ConsistencyGate {
                netgame: false,
                netdemo: false,
                ticdup: 1,
            },
            100,
            0,
            1,
        )
        .expect("not netgame");
    // net demo: never checks
    check
        .check_and_store(
            &cmd,
            ConsistencyGate {
                netgame: true,
                netdemo: true,
                ticdup: 1,
            },
            100,
            0,
            1,
        )
        .expect("netdemo");
    // ticdup > 1, odd gametic: skipped
    check
        .check_and_store(
            &cmd,
            ConsistencyGate {
                netgame: true,
                netdemo: false,
                ticdup: 2,
            },
            101,
            0,
            1,
        )
        .expect("odd tic");
    assert_eq!(
        check.predicted(0, (101 / 2) as usize % BACKUPTICS as usize),
        0
    );
}

// -- quit path -------------------------------------------------------------------

#[test]
fn quit_net_game_is_silent_in_demo_playback() {
    let mut peers = make_peers(2);
    peers[0].hooks.demo_playback = true;
    quit_net_game(&mut peers[0]);
    assert!(peers[0].out.borrow().is_empty());
    assert_eq!(peers[0].hooks.wait_vbl_calls, 0);
}

#[test]
fn demo_exit_packet_marks_player_left_and_demo_status() {
    let mut peers = make_peers(2);
    peers[1].hooks.demo_recording = true;
    let pkt = cmd_packet(0, &[tic(1)], NCMD_EXIT);
    peers[1].inb.borrow_mut().push_back((0, pkt.encode_wire()));
    get_packets(&mut peers[1]).expect("get_packets");
    assert_eq!(peers[1].hooks.player_left, vec![0]);
    assert_eq!(peers[1].hooks.demo_status_calls, 1);
}

// -- real loopback UDP -------------------------------------------------------------

fn free_port() -> u16 {
    std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

#[test]
fn two_instances_play_over_real_udp() {
    let port_a = free_port();
    let port_b = free_port();
    let addr_a = SocketAddr::from((Ipv4Addr::LOCALHOST, port_a));
    let addr_b = SocketAddr::from((Ipv4Addr::LOCALHOST, port_b));

    let mut a = NetEngine::start(NetConfig {
        console_player: 0,
        ticdup: 1,
        extratics: false,
        hosts: vec![addr_b],
        port: port_a,
    })
    .expect("engine A");
    let mut b = NetEngine::start(NetConfig {
        console_player: 1,
        ticdup: 1,
        extratics: false,
        hosts: vec![addr_a],
        port: port_b,
    })
    .expect("engine B");
    let mut hooks_a = MockHooks {
        playeringame: [true, true, false, false],
        ..MockHooks::default()
    };
    let mut hooks_b = MockHooks {
        playeringame: [true, true, false, false],
        ..MockHooks::default()
    };

    let settings = settings();
    // arbitration: A is the key player (console_player == 0)
    let mut a_done = false;
    let mut b_done = false;
    for round in 0..2000 {
        if !a_done {
            a_done = a
                .arbitration_step(&mut hooks_a, &settings)
                .expect("A arbitration")
                .is_some();
        }
        if !b_done {
            b_done = b
                .arbitration_step(&mut hooks_b, &settings)
                .expect("B arbitration")
                .is_some();
        }
        // once both are running, exchange tics; the wire gives us delivery for free
        let t = 35 * (round + 1);
        let _ = a.net_update(t, &mut hooks_a);
        let _ = b.net_update(t, &mut hooks_b);
        if a_done && b_done && a.nodes()[1].net_tics > 0 && b.nodes()[1].net_tics > 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(a_done, "key player must finish arbitration");
    assert!(b_done, "listener must finish arbitration");
    assert!(a.nodes()[1].net_tics > 0, "A must receive B's tics");
    assert!(b.nodes()[1].net_tics > 0, "B must receive A's tics");
}

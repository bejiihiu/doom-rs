//! The UDP backend for the net layer, ported from `i_net.c`
//! (`UDPsocket`/`BindToLocalPort`/`PacketSend`/`PacketGet`).
//!
//! Vanilla opened two sockets: a non-blocking receive socket bound to the
//! DOOM port and an *unbound* send socket with an ephemeral port. That made
//! sender identity IP-only — there could be exactly one DOOM per LAN host.
//! This port uses a single bound socket for both directions instead, so the
//! source port of every datagram is the DOOM port and senders can be told
//! apart by `(ip, port)`. Multiple instances can then coexist on one host
//! (loopback included), which vanilla could not do.
//!
//! The send address list is matched against the sender of each datagram;
//! unknown senders are dropped. The socket is created with `SO_BROADCAST`
//! so a `-net` list entry like `255.255.255.255` works the way it did in
//! the DOS original (vanilla linuxdoom-1.10 never enabled it, and its
//! `-net` broadcast was an IPX-era feature).
//!
//! Send errors are logged and dropped, matching vanilla's silent `sendto`
//! failure (it never checked the return value). Receive errors other than
//! `WouldBlock` are logged and the datagram is dropped instead of
//! `I_Error`ing — a stray transient socket error should not kill a running
//! net game.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use super::packet::MAXNETNODES;

/// One end's view of the wire: send a datagram to a node index, or poll for
/// one. Implementations resolve the node index to a socket address however
/// they like; `HGetPacket` in the engine layer validates everything that
/// comes in.
pub trait NetTransport {
    /// Queues one datagram to `node` (a slot in the node list).
    fn send_packet(&mut self, node: usize, data: &[u8]);
    /// Returns `(node, datagram)` if one is waiting, `None` otherwise.
    /// Node indexes always refer to the node list the engine was built with;
    /// datagrams from unknown senders are dropped by the implementation.
    fn recv_packet(&mut self) -> Option<(usize, Vec<u8>)>;
}

/// The production transport: a non-blocking UDP socket, like `i_net.c` but
/// with both directions on one bound socket (see the module docs).
pub struct UdpTransport {
    socket: UdpSocket,
    send_addresses: [Option<SocketAddr>; MAXNETNODES],
}

impl UdpTransport {
    /// Creates the transport.
    ///
    /// `recv_port` is the port other nodes send to (`BindToLocalPort` with
    /// `htons(DOOMPORT)`); pass 0 to let the OS pick an ephemeral port
    /// (tests do this). `send_addresses` holds the *other* nodes' addresses
    /// in node order; slot 0 is this console (rebound locally, never sent
    /// to over the wire).
    pub fn new(
        recv_port: u16,
        send_addresses: [Option<SocketAddr>; MAXNETNODES],
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, recv_port))?;
        socket.set_nonblocking(true)?;
        // see the module docs: broadcast destinations need this on some platforms
        socket.set_broadcast(true)?;
        Ok(Self {
            socket,
            send_addresses,
        })
    }

    /// The address this endpoint is bound to (for tests and diagnostics).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Whether the `-net` list points at a broadcast address (any slot
    /// whose IP is a broadcast address); informational, unicast unaffected.
    pub fn uses_broadcast(&self) -> bool {
        self.send_addresses
            .iter()
            .flatten()
            .any(|a| matches!(a.ip(), IpAddr::V4(v4) if v4.is_broadcast()))
    }
}

impl NetTransport for UdpTransport {
    fn send_packet(&mut self, node: usize, data: &[u8]) {
        let Some(addr) = self.send_addresses.get(node).copied().flatten() else {
            return; // node 0 is the local console: rebound, never sent
        };
        // vanilla never checked sendto's return value; log and move on
        if let Err(e) = self.socket.send_to(data, addr) {
            tracing::warn!("net: send to node {node} ({addr}) failed: {e}");
        }
    }

    fn recv_packet(&mut self) -> Option<(usize, Vec<u8>)> {
        let mut buf = [0u8; 2048];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    match node_for_addr(&self.send_addresses, from) {
                        Some(node) => return Some((node, buf[..len].to_vec())),
                        None => {
                            // packet is not from one of the players
                            tracing::warn!("net: dropping packet from unknown sender {from}");
                            continue;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                Err(e) => {
                    tracing::error!("net: recv failed: {e}");
                    return None;
                }
            }
        }
    }
}

/// Maps a sender address to its node slot. Vanilla matched IP only
/// (`fromaddress.sin_addr.s_addr == sendaddress[i].sin_addr.s_addr`); we
/// match `(ip, port)` so several games on one host do not cross-talk.
fn node_for_addr(addresses: &[Option<SocketAddr>; MAXNETNODES], from: SocketAddr) -> Option<usize> {
    addresses.iter().position(
        |slot| matches!(slot, Some(addr) if addr.ip() == from.ip() && addr.port() == from.port()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_match_uses_ip_and_port() {
        let a: SocketAddr = "127.0.0.1:5029".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:5030".parse().unwrap();
        let mut addrs = [None; MAXNETNODES];
        addrs[2] = Some(a);
        addrs[3] = Some(b);
        assert_eq!(node_for_addr(&addrs, a), Some(2));
        assert_eq!(node_for_addr(&addrs, b), Some(3));
        // same IP, unknown port: not a player
        let c: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert_eq!(node_for_addr(&addrs, c), None);
        // different IP entirely
        let d: SocketAddr = "10.0.0.1:5029".parse().unwrap();
        assert_eq!(node_for_addr(&addrs, d), None);
    }

    #[test]
    fn udp_transport_loopback_roundtrip() {
        let recv_port0;
        let mut addrs0 = [None; MAXNETNODES];
        {
            let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
            recv_port0 = probe.local_addr().unwrap().port();
        }
        let recv_port1;
        let mut addrs1 = [None; MAXNETNODES];
        {
            let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
            recv_port1 = probe.local_addr().unwrap().port();
        }
        let a0: SocketAddr = (Ipv4Addr::LOCALHOST, recv_port0).into();
        let a1: SocketAddr = (Ipv4Addr::LOCALHOST, recv_port1).into();
        addrs0[1] = Some(a1); // engine 0 sends to engine 1 at slot 1
        addrs1[1] = Some(a0); // engine 1 sends to engine 0 at slot 1
        let mut t0 = UdpTransport::new(recv_port0, addrs0).unwrap();
        let mut t1 = UdpTransport::new(recv_port1, addrs1).unwrap();

        t0.send_packet(1, b"ping");
        // loopback delivery is practically instant; poll briefly
        let mut got = None;
        for _ in 0..1000 {
            if let Some((node, data)) = t1.recv_packet() {
                got = Some((node, data));
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
        let (node, data) = got.expect("loopback packet should arrive");
        assert_eq!(node, 1);
        assert_eq!(data, b"ping");

        t1.send_packet(1, b"pong");
        let mut got = None;
        for _ in 0..1000 {
            if let Some((node, data)) = t0.recv_packet() {
                got = Some((node, data));
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
        let (node, data) = got.expect("loopback packet should arrive");
        assert_eq!(node, 1);
        assert_eq!(data, b"pong");

        // nothing else waiting
        assert!(t0.recv_packet().is_none());
        assert!(t1.recv_packet().is_none());
    }

    #[test]
    fn udp_transport_reports_broadcast_flag() {
        let mut addrs = [None; MAXNETNODES];
        addrs[1] = Some("255.255.255.255:5029".parse().unwrap());
        let t = UdpTransport::new(0, addrs).unwrap();
        assert!(t.uses_broadcast());
        let addrs2 = [None; MAXNETNODES];
        let t2 = UdpTransport::new(0, addrs2).unwrap();
        assert!(!t2.uses_broadcast());
    }
}

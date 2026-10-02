//! The **WSJT-X UDP protocol**: sdroxide announcing its FT8/FT4 traffic the way
//! WSJT-X does, so the ecosystem built around it works unchanged.
//!
//! GridTracker, JTAlert, N1MM+ and Log4OM all listen for the same datagrams —
//! decodes, station status, and logged QSOs — on UDP port 2237. Speaking that
//! protocol means none of them needs to know sdroxide exists.
//!
//! The wire format is Qt's `QDataStream` (big-endian) behind a four-word header,
//! as defined by WSJT-X's `NetworkMessage.hpp`. All of it is built by pure
//! functions over a byte buffer in [`msg`], because UDP gives no feedback
//! whatsoever: a field of the wrong width doesn't fail, it just silently stops
//! a logger from seeing contacts. The tests are the only check there is.
//!
//! The clients talk back on the same socket: they answer the address our
//! datagrams came from. [`WsjtxUdp::poll`] reads what they send — Reply, Halt
//! Tx, Free Text, Replay, Highlight Callsign — and [`control::translate`]
//! decides what each is allowed to do, which is nothing beyond Halt Tx unless
//! the operator has switched control on.
//!
//! NATIVE ONLY — it binds a UDP socket.

pub mod control;
pub mod msg;
pub mod n1mm;

pub use n1mm::N1mmUdp;

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

use sdroxide_types::{QsoRecord, WsjtxConfig};
use tracing::{debug, info};

/// A configured sender. Every method is fire-and-forget: a send error is logged
/// and dropped, because a logger that isn't running must never stall the radio.
pub struct WsjtxUdp {
    sock: UdpSocket,
    dest: std::net::SocketAddr,
    id: String,
    addr: String,
}

impl WsjtxUdp {
    /// Bind a socket and resolve the destination. Fails only on a bad address
    /// or an unusable local port.
    pub fn start(cfg: &WsjtxConfig) -> Result<Self, String> {
        let host = if cfg.host.trim().is_empty() { "127.0.0.1" } else { cfg.host.trim() };
        let dest = (host, cfg.port)
            .to_socket_addrs()
            .map_err(|e| format!("{host}:{}: {e}", cfg.port))?
            .next()
            .ok_or_else(|| format!("{host}:{}: no address", cfg.port))?;
        let sock = UdpSocket::bind(if dest.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
            .map_err(|e| e.to_string())?;
        // Multicast groups are a supported destination in WSJT-X, and a
        // datagram sent to one needs a TTL that leaves this host.
        if dest.ip().is_multicast() {
            let _ = sock.set_multicast_ttl_v4(2);
        }
        // Read from the engine's tick, which must never wait on a client.
        sock.set_nonblocking(true).map_err(|e| e.to_string())?;
        let addr = format!("{host}:{}", cfg.port);
        info!(dest = %addr, id = %cfg.id, "WSJT-X UDP broadcast started");
        Ok(WsjtxUdp { sock, dest, id: cfg.id.clone(), addr })
    }

    /// The destination this sender was built for, so the engine can tell a
    /// config change that needs a rebuild from one that doesn't.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn send(&self, packet: Vec<u8>) {
        if let Err(e) = self.sock.send_to(&packet, self.dest) {
            debug!(dest = %self.addr, error = %e, "WSJT-X UDP send failed");
        }
    }

    /// "Still here" — clients use this to notice us and to time us out.
    pub fn heartbeat(&self, version: &str) {
        self.send(msg::heartbeat(&self.id, version));
    }

    /// One decoded message.
    pub fn decode(&self, d: &msg::DecodeInfo) {
        self.send(msg::decode(&self.id, d));
    }

    /// Where the station is and what it's doing.
    pub fn status(&self, s: &msg::StatusInfo) {
        self.send(msg::status(&self.id, s));
    }

    /// Clear the clients' decode windows (a fresh band or session).
    pub fn clear(&self) {
        self.send(msg::clear(&self.id));
    }

    /// A completed contact, in both the forms loggers accept: the structured
    /// message and the ADIF record. Which one a logger takes is its own choice;
    /// sending both is what WSJT-X does.
    ///
    /// The ADIF half is the bare record — the fields and `<EOR>`, nothing
    /// before them. This used to carry a whole file export, header line and
    /// all, which is a well-formed *file* and not what this message is defined
    /// to hold: a logger reading the datagram as the single record the protocol
    /// promises found a line of prose where the first tag should be
    /// (issue #341).
    pub fn qso_logged(&self, q: &QsoRecord) {
        self.send(msg::qso_logged(&self.id, q));
        self.send(msg::logged_adif(&self.id, &sdroxide_types::qso_to_adif_record(q)));
    }

    /// Whatever the clients have sent since the last call, oldest first.
    /// Never blocks.
    ///
    /// With a unicast destination only that host is listened to: the
    /// operator named the machine their logger runs on, and a datagram from
    /// any other is somebody else's. A multicast group has no one sender to
    /// expect, so there every source is read and the id test in
    /// [`control::translate`] is what remains.
    pub fn poll(&self) -> Vec<msg::Inbound> {
        /// A bound on one tick's work, whatever is queued behind it.
        const MAX_PER_POLL: usize = 64;
        let mut out = Vec::new();
        let mut buf = [0u8; 2048];
        for _ in 0..MAX_PER_POLL {
            let (n, from) = match self.sock.recv_from(&mut buf) {
                Ok(got) => got,
                // WouldBlock is the queue running dry. A Windows socket also
                // reports an ICMP port-unreachable from an earlier send here
                // (no logger listening yet), which is no reason to stop.
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    debug!(error = %e, "WSJT-X UDP receive failed");
                    continue;
                }
            };
            if !self.accepts_from(from) {
                debug!(%from, "WSJT-X UDP: datagram from an unexpected host dropped");
                continue;
            }
            match msg::parse(&buf[..n]) {
                Ok(m) => out.push(m),
                Err(e) => debug!(%from, error = %e, "WSJT-X UDP: unreadable datagram dropped"),
            }
        }
        out
    }

    fn accepts_from(&self, from: SocketAddr) -> bool {
        self.dest.ip().is_multicast() || from.ip() == self.dest.ip()
    }

    /// We're going away — clients drop us from their station lists.
    pub fn close(&self) {
        self.send(msg::close(&self.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A client on loopback, and a broadcast aimed at it.
    fn pair(client_ip: &str) -> (UdpSocket, WsjtxUdp) {
        let client = UdpSocket::bind((client_ip, 0)).unwrap();
        let cfg = WsjtxConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: client.local_addr().unwrap().port(),
            id: "WSJT-X".into(),
            ..WsjtxConfig::default()
        };
        (client, WsjtxUdp::start(&cfg).unwrap())
    }

    /// Where the broadcast sends from — what a client learns from our first
    /// datagram and answers to.
    fn learn_our_address(client: &UdpSocket, w: &WsjtxUdp) -> SocketAddr {
        w.heartbeat("test");
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 512];
        let (_, from) = client.recv_from(&mut buf).expect("our heartbeat");
        from
    }

    fn halt(id: &str) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0xadbc_cbdau32.to_be_bytes());
        p.extend_from_slice(&3u32.to_be_bytes());
        p.extend_from_slice(&8u32.to_be_bytes());
        p.extend_from_slice(&(id.len() as u32).to_be_bytes());
        p.extend_from_slice(id.as_bytes());
        p.push(1);
        p
    }

    /// Poll until something arrives or a second passes: loopback delivery is
    /// fast but not synchronous with `send_to` returning.
    fn poll_for(w: &WsjtxUdp) -> Vec<msg::Inbound> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let got = w.poll();
            if !got.is_empty() || Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_client_answering_our_address_is_heard() {
        let (client, w) = pair("127.0.0.1");
        let us = learn_our_address(&client, &w);
        client.send_to(&halt("WSJT-X"), us).unwrap();
        assert_eq!(
            poll_for(&w),
            vec![msg::Inbound::HaltTx { id: "WSJT-X".into(), auto_tx_only: true }]
        );
    }

    #[test]
    fn polling_an_empty_socket_returns_at_once() {
        let (_client, w) = pair("127.0.0.1");
        let t = Instant::now();
        assert!(w.poll().is_empty());
        assert!(t.elapsed() < Duration::from_millis(200), "poll blocked for {:?}", t.elapsed());
    }

    #[test]
    fn noise_on_the_port_is_dropped_and_what_follows_still_read() {
        let (client, w) = pair("127.0.0.1");
        let us = learn_our_address(&client, &w);
        client.send_to(b"not a datagram this protocol has", us).unwrap();
        client.send_to(&halt("WSJT-X"), us).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut got = Vec::new();
        while got.is_empty() && Instant::now() < deadline {
            got.extend(w.poll());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(got.len(), 1, "{got:?}");
    }

    /// Loopback is the whole of 127/8 on Linux, which is what lets a second
    /// "host" exist in a test.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_datagram_from_another_host_is_not_ours_to_obey() {
        let (client, w) = pair("127.0.0.1");
        let us = learn_our_address(&client, &w);
        let stranger = UdpSocket::bind("127.0.0.2:0").unwrap();
        stranger.send_to(&halt("WSJT-X"), us).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(w.poll().is_empty(), "a host other than the configured one was obeyed");
        // The configured host still is.
        client.send_to(&halt("WSJT-X"), us).unwrap();
        assert_eq!(poll_for(&w).len(), 1);
    }
}

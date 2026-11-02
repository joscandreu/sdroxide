//! A B2F session through an external VARA-compatible TNC.
//!
//! sdroxide contains no VARA modem. A TNC that does — xara (MIT-licensed, for
//! Linux, macOS and FreeBSD) or VARA itself — is already running, and this
//! drives it over its two TCP ports the way Pat and Winlink Express do:
//!
//! * the **command port** (8300 by default) takes CR-terminated lines —
//!   `MYCALL`, `BW2300`, `CONNECT <me> <gateway>`, `DISCONNECT`, `ABORT` — and
//!   answers `OK`/`WRONG`, plus notices of its own: `CONNECTED`,
//!   `DISCONNECTED`, `PTT ON`/`PTT OFF`, `BUFFER n`, `PENDING`, `BUSY`;
//! * the **data port**, one above, is the link's byte stream.
//!
//! A reader thread watches the command port. `PTT ON`/`PTT OFF` go straight to
//! the engine through a channel rather than through the worker, because a
//! VARA turnaround is a fraction of a second and the worker may be blocked in
//! a read; everything else is queued for the transport to act on.
//!
//! # Failure model
//!
//! Like [`crate::transport::Ax25Transport`], reads never time out on their
//! own: an HF link is legitimately silent for long stretches while the modem
//! retries. The TNC decides when the link is dead and says `DISCONNECTED`;
//! that is `Ok(0)` once the buffered bytes are read. A TNC that goes away (its
//! command socket closes) is `BrokenPipe`. The operator's abort is checked
//! between short waits everywhere this could block.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use crate::transport::{Transport, TransportError};

/// How long the transport waits before looking at the cancel flag again.
const POLL: Duration = Duration::from_millis(250);
/// How long a plain command has to be acknowledged.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a graceful hang-up may take before the link is aborted. VARA
/// sends what is still buffered first, which on a slow link takes a while.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(90);

/// Something the TNC said on its command port.
#[derive(Debug, Clone, PartialEq)]
pub enum TncEvent {
    Ok,
    Wrong,
    Connected {
        caller: String,
        answerer: String,
        detail: String,
    },
    Disconnected,
    Ptt(bool),
    Buffer(usize),
    Pending,
    CancelPending,
    Busy(bool),
    /// Anything else: `BITRATE`, `SN`, `REGISTERED`, `IAMALIVE`, a version…
    Other(String),
    /// The command port closed: the TNC has gone.
    Closed,
}

/// One command-port line, as the TNC sent it.
pub fn parse_line(line: &str) -> TncEvent {
    let l = line.trim();
    let mut words = l.split_whitespace();
    match (words.next().unwrap_or(""), words.next()) {
        ("OK", None) => TncEvent::Ok,
        ("WRONG", _) => TncEvent::Wrong,
        ("CONNECTED", Some(caller)) => {
            let answerer = words.next().unwrap_or("").to_string();
            let detail = words.collect::<Vec<_>>().join(" ");
            TncEvent::Connected { caller: caller.to_string(), answerer, detail }
        }
        ("DISCONNECTED", None) => TncEvent::Disconnected,
        ("PTT", Some("ON")) => TncEvent::Ptt(true),
        ("PTT", Some("OFF")) => TncEvent::Ptt(false),
        ("BUFFER", Some(n)) => {
            n.parse().map_or_else(|_| TncEvent::Other(l.into()), TncEvent::Buffer)
        }
        ("PENDING", None) => TncEvent::Pending,
        ("CANCELPENDING", None) => TncEvent::CancelPending,
        ("BUSY", Some("ON")) => TncEvent::Busy(true),
        ("BUSY", Some("OFF")) => TncEvent::Busy(false),
        _ => TncEvent::Other(l.to_string()),
    }
}

/// How to reach a TNC and whom to call through it.
#[derive(Debug, Clone)]
pub struct VaraDial {
    /// `host:port` of the command port; the data port is the next one up.
    pub address: String,
    pub mycall: String,
    pub gateway: String,
    /// `BW500`/`BW2300`/`BW2750` to send first, or `None` to leave the TNC's
    /// own setting (VARA FM has no bandwidth command).
    pub bandwidth_hz: Option<u32>,
    /// How long to wait for the gateway to answer.
    pub timeout: Duration,
    /// Calling a peer station rather than a gateway: `P2P SESSION` timing.
    pub p2p: bool,
    /// How long to wait for a busy channel to clear before giving up. Zero
    /// calls regardless.
    pub busy_wait: Duration,
}

/// How to put a TNC on watch for peer-to-peer calls.
#[derive(Debug, Clone)]
pub struct VaraListen {
    pub address: String,
    pub mycall: String,
    pub bandwidth_hz: Option<u32>,
}

/// The data port is the command port plus one, by the protocol's convention.
fn data_addr(cmd: SocketAddr) -> Result<SocketAddr, String> {
    let port = cmd
        .port()
        .checked_add(1)
        .ok_or("the command port is 65535; there is no data port above it")?;
    Ok(SocketAddr::new(cmd.ip(), port))
}

fn io_err(kind: ErrorKind, msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(kind, msg.into())
}

/// Read the command port, line by line, until it closes.
fn read_commands(stream: TcpStream, events: Sender<TncEvent>, ptt: Option<Sender<bool>>) {
    let mut keyed = false;
    let mut r = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\r', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_matches(|c: char| c == '\r' || c == '\n' || c == ' ');
                if line.is_empty() {
                    continue;
                }
                let ev = parse_line(line);
                if let TncEvent::Ptt(on) = ev {
                    keyed = on;
                    if let Some(p) = &ptt {
                        let _ = p.send(on);
                    }
                }
                if events.send(ev).is_err() {
                    break;
                }
            }
        }
    }
    // A TNC that dies with the transmitter keyed must not leave it keyed.
    if keyed && let Some(p) = &ptt {
        let _ = p.send(false);
    }
    let _ = events.send(TncEvent::Closed);
}

/// A B2F byte stream through a VARA-compatible TNC.
pub struct VaraTransport {
    cmd: TcpStream,
    data: TcpStream,
    events: Receiver<TncEvent>,
    gateway: String,
    cancel: Arc<AtomicBool>,
    /// The TNC said `DISCONNECTED`: hand back what is left, then `Ok(0)`.
    closed: bool,
    /// The TNC itself went away.
    gone: bool,
    /// A link is up (between `CONNECTED` and `DISCONNECTED`).
    linked: bool,
    /// The TNC was put in `LISTEN ON` by us.
    listening: bool,
    /// The channel is busy, from the TNC's last `BUSY` notice.
    busy: bool,
    /// What the TNC said along the way, for the session transcript.
    log: Vec<String>,
    /// Bytes the TNC still holds to send, from its last `BUFFER` notice.
    pub buffered: usize,
    /// [`VaraTransport::hang_up`] has run.
    hung_up: bool,
    /// The last `BITRATE` line logged, so a speed is noted when it changes
    /// rather than once a frame.
    last_bitrate: String,
}

impl std::fmt::Debug for VaraTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaraTransport").field("gateway", &self.gateway).finish_non_exhaustive()
    }
}

impl VaraTransport {
    /// Open both ports, set the TNC up, call the gateway and wait for the link.
    ///
    /// `ptt` receives `true`/`false` as the TNC keys and unkeys, from the
    /// moment the ports are open — the call itself transmits.
    /// Open both ports and start reading the command port. Nothing is sent.
    fn open(
        address: &str,
        cancel: Arc<AtomicBool>,
        ptt: Option<Sender<bool>>,
    ) -> Result<Self, TransportError> {
        let fail = |e: std::io::Error| TransportError::Connect(address.to_string(), e);
        let cmd_addr = address
            .to_socket_addrs()
            .map_err(fail)?
            .next()
            .ok_or_else(|| fail(io_err(ErrorKind::NotFound, "no address")))?;
        let data_addr =
            data_addr(cmd_addr).map_err(|e| fail(io_err(ErrorKind::InvalidInput, e)))?;
        let no_tnc = |e: std::io::Error| {
            fail(io_err(
                e.kind(),
                format!("is the VARA TNC (xara or VARA) running and listening there? {e}"),
            ))
        };
        let cmd = TcpStream::connect_timeout(&cmd_addr, ACK_TIMEOUT).map_err(no_tnc)?;
        let data = TcpStream::connect_timeout(&data_addr, ACK_TIMEOUT).map_err(no_tnc)?;
        cmd.set_nodelay(true).map_err(fail)?;
        data.set_nodelay(true).map_err(fail)?;
        data.set_read_timeout(Some(POLL)).map_err(fail)?;

        let (ev_tx, ev_rx) = unbounded();
        let reader = cmd.try_clone().map_err(fail)?;
        std::thread::Builder::new()
            .name("winlink-vara-cmd".into())
            .spawn(move || read_commands(reader, ev_tx, ptt))
            .map_err(fail)?;

        Ok(VaraTransport {
            cmd,
            data,
            events: ev_rx,
            gateway: String::new(),
            cancel,
            closed: false,
            gone: false,
            linked: false,
            listening: false,
            busy: false,
            log: Vec::new(),
            buffered: 0,
            hung_up: false,
            last_bitrate: String::new(),
        })
    }

    /// Open both ports, set the TNC up, call the gateway and wait for the link.
    ///
    /// `ptt` receives `true`/`false` as the TNC keys and unkeys, from the
    /// moment the ports are open — the call itself transmits.
    pub fn connect(
        dial: &VaraDial,
        cancel: Arc<AtomicBool>,
        ptt: Option<Sender<bool>>,
    ) -> Result<Self, TransportError> {
        let mut t = Self::open(&dial.address, cancel, ptt)?;
        t.gateway = dial.gateway.trim().to_uppercase();
        match t.call(dial) {
            Ok(()) => Ok(t),
            Err(e) => {
                // Do not leave a call in progress behind a failed dial.
                let _ = t.send_line("ABORT");
                t.closed = true;
                Err(TransportError::Connect(dial.gateway.clone(), e))
            }
        }
    }

    /// Open both ports and put the TNC on watch for peer-to-peer calls. Wait
    /// for one with [`VaraTransport::wait_call`].
    pub fn listen(
        how: &VaraListen,
        cancel: Arc<AtomicBool>,
        ptt: Option<Sender<bool>>,
    ) -> Result<Self, TransportError> {
        let mut t = Self::open(&how.address, cancel, ptt)?;
        let setup = (|| -> std::io::Result<()> {
            t.command(&format!("MYCALL {}", how.mycall.trim().to_uppercase()))?;
            if let Some(bw) = how.bandwidth_hz {
                t.command(&format!("BW{bw}"))?;
            }
            t.optional("P2P SESSION")?;
            t.command("LISTEN ON")?;
            Ok(())
        })();
        match setup {
            Ok(()) => {
                t.listening = true;
                Ok(t)
            }
            Err(e) => {
                t.closed = true;
                Err(TransportError::Connect(how.address.clone(), e))
            }
        }
    }

    /// Wait for a station to call us; returns its callsign once linked.
    ///
    /// Blocks until a call arrives, the operator stops listening (the cancel
    /// flag), or the TNC goes away.
    pub fn wait_call(&mut self) -> std::io::Result<String> {
        loop {
            if self.cancelled() {
                return Err(io_err(ErrorKind::Interrupted, "stopped listening"));
            }
            match self.next_event(POLL) {
                Some(TncEvent::Connected { caller, .. }) => {
                    self.gateway = caller.clone();
                    return Ok(caller);
                }
                Some(TncEvent::Closed) => {
                    return Err(io_err(ErrorKind::BrokenPipe, "the TNC closed its command port"));
                }
                _ => {}
            }
        }
    }

    /// The TNC's side of the conversation so far, for the transcript.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    #[cfg(test)]
    fn take_log_ref(&self) -> &[String] {
        &self.log
    }

    fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.log.push(format!("> {line}"));
        self.cmd.write_all(format!("{line}\r").as_bytes())
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Note what an event means for the transport's own state, and keep it in
    /// the transcript when it is worth reading.
    fn note(&mut self, ev: &TncEvent) {
        match ev {
            TncEvent::Buffer(n) => self.buffered = *n,
            TncEvent::Disconnected => {
                self.closed = true;
                self.linked = false;
            }
            TncEvent::Connected { .. } => self.linked = true,
            TncEvent::Busy(on) => self.busy = *on,
            TncEvent::Closed => self.gone = true,
            _ => {}
        }
        // Kept out of the transcript: the per-frame chatter. A speed change is
        // worth a line; the same speed again, a signal report every frame, the
        // busy detector and the keepalive are not.
        let quiet = match ev {
            TncEvent::Buffer(_) | TncEvent::Ptt(_) | TncEvent::Ok | TncEvent::Busy(_) => true,
            TncEvent::Other(o) if o.starts_with("IAMALIVE") || o.starts_with("SN ") => true,
            TncEvent::Other(o) if o.starts_with("BITRATE") => {
                let speed = o.trim_end_matches(" TX").trim_end_matches(" RX").to_string();
                std::mem::replace(&mut self.last_bitrate, speed.clone()) == speed
            }
            _ => false,
        };
        if !quiet {
            self.log.push(format!("< {}", describe(ev)));
        }
    }

    /// The next event, waiting at most `wait`.
    fn next_event(&mut self, wait: Duration) -> Option<TncEvent> {
        match self.events.recv_timeout(wait) {
            Ok(ev) => {
                self.note(&ev);
                Some(ev)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                self.gone = true;
                Some(TncEvent::Closed)
            }
        }
    }

    /// Send a command and wait for its `OK`. `WRONG` is an error naming it.
    fn command(&mut self, line: &str) -> std::io::Result<()> {
        self.send_line(line)?;
        let deadline = Instant::now() + ACK_TIMEOUT;
        while Instant::now() < deadline {
            match self.next_event(POLL) {
                Some(TncEvent::Ok) => return Ok(()),
                Some(TncEvent::Wrong) => {
                    return Err(io_err(ErrorKind::InvalidInput, format!("the TNC refused {line}")));
                }
                Some(TncEvent::Closed) => {
                    return Err(io_err(ErrorKind::BrokenPipe, "the TNC closed its command port"));
                }
                _ => {}
            }
        }
        Err(io_err(ErrorKind::TimedOut, format!("the TNC did not answer {line}")))
    }

    /// A command an older TNC may not know: `WRONG` is noted and passed over.
    fn optional(&mut self, line: &str) -> std::io::Result<()> {
        match self.command(line) {
            Err(e) if e.kind() == ErrorKind::InvalidInput => Ok(()),
            other => other,
        }
    }

    /// Hold the call while the channel is busy, for up to `wait`.
    ///
    /// VARA's busy detector says `BUSY ON` while it hears another signal in
    /// the passband. Calling over it spoils both links, and most likely our
    /// own call too, so wait for `BUSY OFF` and give up if it never comes.
    fn wait_until_clear(&mut self, wait: Duration) -> std::io::Result<()> {
        // Notices the TNC sent while it was being set up arrive first.
        self.next_event(POLL);
        self.drain_events();
        if !self.busy || wait.is_zero() {
            return Ok(());
        }
        self.log.push(format!("· the channel is busy; waiting up to {} s", wait.as_secs()));
        let deadline = Instant::now() + wait;
        while self.busy {
            if self.cancelled() {
                return Err(io_err(ErrorKind::Interrupted, "stopped"));
            }
            if Instant::now() >= deadline {
                return Err(io_err(
                    ErrorKind::WouldBlock,
                    format!(
                        "the channel stayed busy for {} s — another station is using it; not calling",
                        wait.as_secs()
                    ),
                ));
            }
            if let Some(TncEvent::Closed) = self.next_event(POLL) {
                return Err(io_err(ErrorKind::BrokenPipe, "the TNC closed its command port"));
            }
        }
        Ok(())
    }

    fn call(&mut self, dial: &VaraDial) -> std::io::Result<()> {
        let mycall = dial.mycall.trim().to_uppercase();
        let gateway = dial.gateway.trim().to_uppercase();
        if gateway.is_empty() {
            return Err(io_err(ErrorKind::InvalidInput, "no gateway to call"));
        }
        self.command(&format!("MYCALL {mycall}"))?;
        // Not answering calls while we are making one.
        self.command("LISTEN OFF")?;
        if let Some(bw) = dial.bandwidth_hz {
            self.command(&format!("BW{bw}")).map_err(|e| {
                io_err(
                    e.kind(),
                    format!("{e} — a VARA FM TNC has no bandwidth: choose \"TNC's setting\""),
                )
            })?;
        }
        // Peer stations get VARA's longer peer-to-peer turnaround.
        self.optional(if dial.p2p { "P2P SESSION" } else { "WINLINK SESSION" })?;
        self.wait_until_clear(dial.busy_wait)?;
        self.command(&format!("CONNECT {mycall} {gateway}"))?;

        let deadline = Instant::now() + dial.timeout;
        loop {
            if self.cancelled() {
                return Err(io_err(ErrorKind::Interrupted, "stopped"));
            }
            if Instant::now() >= deadline {
                return Err(io_err(ErrorKind::TimedOut, "the gateway did not answer"));
            }
            match self.next_event(POLL) {
                Some(TncEvent::Connected { .. }) => return Ok(()),
                Some(TncEvent::Disconnected) => {
                    return Err(io_err(ErrorKind::ConnectionRefused, "the gateway did not answer"));
                }
                Some(TncEvent::Closed) => {
                    return Err(io_err(ErrorKind::BrokenPipe, "the TNC closed its command port"));
                }
                _ => {}
            }
        }
    }

    /// Act on whatever the TNC has said, without waiting.
    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            self.note(&ev);
        }
    }
}

fn describe(ev: &TncEvent) -> String {
    match ev {
        TncEvent::Ok => "OK".into(),
        TncEvent::Wrong => "WRONG".into(),
        TncEvent::Connected { caller, answerer, detail } => {
            format!("CONNECTED {caller} {answerer} {detail}").trim_end().to_string()
        }
        TncEvent::Disconnected => "DISCONNECTED".into(),
        TncEvent::Ptt(on) => format!("PTT {}", if *on { "ON" } else { "OFF" }),
        TncEvent::Buffer(n) => format!("BUFFER {n}"),
        TncEvent::Pending => "PENDING".into(),
        TncEvent::CancelPending => "CANCELPENDING".into(),
        TncEvent::Busy(on) => format!("BUSY {}", if *on { "ON" } else { "OFF" }),
        TncEvent::Other(o) => o.clone(),
        TncEvent::Closed => "(the TNC closed its command port)".into(),
    }
}

impl Read for VaraTransport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.data.read(buf) {
                Ok(0) => return Ok(0),
                Ok(n) => return Ok(n),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e),
            }
            // Nothing on the data port this time round. Whatever the TNC said
            // meanwhile decides whether to wait again.
            self.drain_events();
            if self.closed {
                // The link is down and the data port has had a whole poll to
                // deliver its last bytes: nothing more is coming.
                return Ok(0);
            }
            if self.gone {
                return Err(io_err(ErrorKind::BrokenPipe, "the TNC closed its command port"));
            }
            if self.cancelled() {
                return Err(io_err(ErrorKind::Interrupted, "stopped by the operator"));
            }
        }
    }
}

impl Write for VaraTransport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.closed {
            return Err(io_err(ErrorKind::BrokenPipe, "the link is down"));
        }
        self.data.write(buf)
    }

    /// The TNC buffers what it is given and sends it as the link allows; there
    /// is nothing to flush on this side of the socket.
    fn flush(&mut self) -> std::io::Result<()> {
        self.data.flush()
    }
}

impl VaraTransport {
    /// Hang up gracefully — VARA sends what it still holds first — and abort
    /// if that does not finish in time or the operator has asked to stop.
    /// Runs once; dropping the transport does it if nobody has.
    pub fn hang_up(&mut self) {
        if std::mem::replace(&mut self.hung_up, true) {
            return;
        }
        self.drain_events();
        if self.linked && !self.closed && !self.gone {
            let _ = self.send_line("DISCONNECT");
            let deadline = Instant::now() + DISCONNECT_TIMEOUT;
            let mut aborted = false;
            while !self.closed && !self.gone && Instant::now() < deadline {
                if self.cancelled() && !aborted {
                    let _ = self.send_line("ABORT");
                    aborted = true;
                }
                self.next_event(POLL);
            }
            if !self.closed && !self.gone {
                let _ = self.send_line("ABORT");
            }
        }
        // A TNC we put on watch must not go on answering calls with nobody
        // behind it to take the mail.
        if self.listening && !self.gone {
            let _ = self.command("LISTEN OFF");
        }
        let _ = self.data.shutdown(Shutdown::Both);
        let _ = self.cmd.shutdown(Shutdown::Both);
    }
}

impl Drop for VaraTransport {
    fn drop(&mut self) {
        self.hang_up();
    }
}

impl Transport for VaraTransport {
    fn target_call(&self) -> &str {
        &self.gateway
    }

    fn describe(&self) -> String {
        format!("VARA to {}", self.gateway)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn command_port_lines_parse() {
        assert_eq!(parse_line("OK"), TncEvent::Ok);
        assert_eq!(parse_line("WRONG"), TncEvent::Wrong);
        assert_eq!(parse_line("PTT ON"), TncEvent::Ptt(true));
        assert_eq!(parse_line("PTT OFF"), TncEvent::Ptt(false));
        assert_eq!(parse_line("BUFFER 512"), TncEvent::Buffer(512));
        assert_eq!(parse_line("DISCONNECTED"), TncEvent::Disconnected);
        assert_eq!(
            parse_line("CONNECTED EA5ABC EA5XYZ-10 2300"),
            TncEvent::Connected {
                caller: "EA5ABC".into(),
                answerer: "EA5XYZ-10".into(),
                detail: "2300".into()
            }
        );
        assert_eq!(
            parse_line("BITRATE (4)  412 bps TX"),
            TncEvent::Other("BITRATE (4)  412 bps TX".into())
        );
        assert_eq!(parse_line("BUFFER x"), TncEvent::Other("BUFFER x".into()));
    }

    /// A TNC that answers as the script says. Listens on a command port and
    /// the one above it; returns the command port's address and a handle that
    /// yields the commands it received.
    pub(crate) struct FakeTnc {
        pub addr: String,
        pub got: Receiver<String>,
        pub data_peer: Receiver<TcpStream>,
        pub cmd_peer: Receiver<TcpStream>,
    }

    fn adjacent_listeners() -> (TcpListener, TcpListener) {
        for _ in 0..50 {
            let a = TcpListener::bind("127.0.0.1:0").unwrap();
            let p = a.local_addr().unwrap().port();
            if p == u16::MAX {
                continue;
            }
            if let Ok(b) = TcpListener::bind(("127.0.0.1", p + 1)) {
                return (a, b);
            }
        }
        panic!("no adjacent ports");
    }

    /// `answer` maps a received command to the lines to send back.
    pub(crate) fn fake_tnc(answer: fn(&str) -> Vec<&'static str>) -> FakeTnc {
        let (cmd_l, data_l) = adjacent_listeners();
        let addr = cmd_l.local_addr().unwrap().to_string();
        let (got_tx, got) = unbounded();
        let (data_tx, data_peer) = unbounded();
        let (cmd_tx, cmd_peer) = unbounded();
        std::thread::spawn(move || {
            let (d, _) = data_l.accept().unwrap();
            let _ = data_tx.send(d);
        });
        std::thread::spawn(move || {
            let (c, _) = cmd_l.accept().unwrap();
            let _ = cmd_tx.send(c.try_clone().unwrap());
            let mut w = c.try_clone().unwrap();
            let mut r = BufReader::new(c);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\r', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim().to_string();
                        for reply in answer(&line) {
                            let _ = w.write_all(format!("{reply}\r").as_bytes());
                        }
                        let _ = got_tx.send(line);
                    }
                }
            }
        });
        FakeTnc { addr, got, data_peer, cmd_peer }
    }

    fn dial(addr: &str) -> VaraDial {
        VaraDial {
            address: addr.into(),
            mycall: "ea5abc".into(),
            gateway: "ea5xyz-10".into(),
            bandwidth_hz: Some(2300),
            timeout: Duration::from_secs(5),
            p2p: false,
            busy_wait: Duration::from_secs(1),
        }
    }

    pub(crate) fn gateway_answers(cmd: &str) -> Vec<&'static str> {
        match cmd {
            c if c.starts_with("CONNECT ") => {
                vec!["OK", "PTT ON", "PTT OFF", "CONNECTED EA5ABC EA5XYZ-10 2300"]
            }
            "DISCONNECT" => vec!["OK", "DISCONNECTED"],
            _ => vec!["OK"],
        }
    }

    fn commands(t: &FakeTnc) -> Vec<String> {
        t.got.try_iter().collect()
    }

    #[test]
    fn a_call_sets_the_tnc_up_connects_and_carries_bytes_both_ways() {
        let tnc = fake_tnc(gateway_answers);
        let (ptt_tx, ptt_rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut t = VaraTransport::connect(&dial(&tnc.addr), cancel, Some(ptt_tx)).unwrap();
        let mut peer = tnc.data_peer.recv_timeout(Duration::from_secs(2)).unwrap();

        // The gateway's greeting reaches the session…
        peer.write_all(b"[WL2K-5.0-B2FWIHJM$]\r>\r").unwrap();
        let mut buf = [0u8; 64];
        let n = t.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"[WL2K-5.0-B2FWIHJM$]\r>\r");
        // …and the session's answer reaches the gateway.
        t.write_all(b"FF\r").unwrap();
        let mut got = [0u8; 3];
        peer.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"FF\r");

        assert_eq!(ptt_rx.try_iter().collect::<Vec<_>>(), vec![true, false]);
        drop(t);
        assert_eq!(
            commands(&tnc),
            [
                "MYCALL EA5ABC",
                "LISTEN OFF",
                "BW2300",
                "WINLINK SESSION",
                "CONNECT EA5ABC EA5XYZ-10",
                "DISCONNECT"
            ]
        );
    }

    #[test]
    fn the_link_dropping_is_end_of_stream_after_the_last_bytes() {
        let tnc = fake_tnc(gateway_answers);
        let mut t =
            VaraTransport::connect(&dial(&tnc.addr), Arc::new(AtomicBool::new(false)), None)
                .unwrap();
        let mut peer = tnc.data_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        peer.write_all(b"FQ\r").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        cmd.write_all(b"DISCONNECTED\r").unwrap();
        let mut all = Vec::new();
        t.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"FQ\r");
        // Already down: no DISCONNECT is sent on the way out.
        drop(t);
        assert!(!commands(&tnc).contains(&"DISCONNECT".to_string()));
    }

    #[test]
    fn a_gateway_that_never_answers_is_a_refusal_not_a_hang() {
        fn no_answer(cmd: &str) -> Vec<&'static str> {
            match cmd {
                c if c.starts_with("CONNECT ") => vec!["OK", "PENDING", "DISCONNECTED"],
                _ => vec!["OK"],
            }
        }
        let tnc = fake_tnc(no_answer);
        let e = VaraTransport::connect(&dial(&tnc.addr), Arc::new(AtomicBool::new(false)), None)
            .unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
    }

    #[test]
    fn a_vara_fm_tnc_refusing_the_bandwidth_says_what_to_change() {
        fn fm(cmd: &str) -> Vec<&'static str> {
            if cmd.starts_with("BW") { vec!["WRONG"] } else { vec!["OK"] }
        }
        let tnc = fake_tnc(fm);
        let e = VaraTransport::connect(&dial(&tnc.addr), Arc::new(AtomicBool::new(false)), None)
            .unwrap_err();
        assert!(e.to_string().contains("TNC's setting"), "{e}");
    }

    #[test]
    fn stopping_during_the_call_aborts_it() {
        fn slow(cmd: &str) -> Vec<&'static str> {
            match cmd {
                c if c.starts_with("CONNECT ") => vec!["OK", "PENDING"],
                _ => vec!["OK"],
            }
        }
        let tnc = fake_tnc(slow);
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            c2.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let e = VaraTransport::connect(&dial(&tnc.addr), cancel, None).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3), "abort took too long");
        assert!(e.to_string().contains("stopped"), "{e}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(commands(&tnc).last().map(String::as_str), Some("ABORT"));
    }

    #[test]
    fn a_tnc_that_dies_keyed_unkeys_the_transmitter() {
        let tnc = fake_tnc(gateway_answers);
        let (ptt_tx, ptt_rx) = unbounded();
        let t = VaraTransport::connect(
            &dial(&tnc.addr),
            Arc::new(AtomicBool::new(false)),
            Some(ptt_tx),
        )
        .unwrap();
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        cmd.write_all(b"PTT ON\r").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        cmd.shutdown(Shutdown::Both).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let seen: Vec<bool> = ptt_rx.try_iter().collect();
        assert_eq!(seen.last(), Some(&false), "{seen:?}");
        drop(t);
    }

    #[test]
    fn a_whole_forwarding_session_runs_through_the_tnc() {
        let tnc = fake_tnc(gateway_answers);
        let mut t =
            VaraTransport::connect(&dial(&tnc.addr), Arc::new(AtomicBool::new(false)), None)
                .unwrap();
        let mut peer = tnc.data_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        let want = crate::session::tests::inbound_message();
        peer.write_all(&crate::session::tests::scripted_cms(std::slice::from_ref(&want))).unwrap();

        let mut outcome = crate::session::SessionOutcome::default();
        crate::session::run_into(&mut t, &crate::session::tests::config(), &[], &mut outcome)
            .expect("the session completes over the TNC");
        assert_eq!(outcome.received.len(), 1);
        assert_eq!(outcome.received[0].mid, want.mid);
        assert_eq!(outcome.received[0].attachments, want.attachments);

        t.hang_up();
        let log = t.take_log();
        assert!(log.iter().any(|l| l == "> DISCONNECT"), "{log:?}");
        assert!(log.iter().any(|l| l == "< DISCONNECTED"), "{log:?}");
    }

    #[test]
    fn a_busy_channel_holds_the_call_and_gives_up_if_it_stays_busy() {
        fn busy(cmd: &str) -> Vec<&'static str> {
            match cmd {
                "LISTEN OFF" => vec!["OK", "BUSY ON"],
                c if c.starts_with("CONNECT ") => vec!["OK", "CONNECTED EA5ABC EA5XYZ-10 2300"],
                _ => vec!["OK"],
            }
        }
        let tnc = fake_tnc(busy);
        let e = VaraTransport::connect(&dial(&tnc.addr), Arc::new(AtomicBool::new(false)), None)
            .unwrap_err();
        assert!(e.to_string().contains("busy"), "{e}");
        assert!(
            !commands(&tnc).iter().any(|c| c.starts_with("CONNECT ")),
            "called over a busy channel"
        );
    }

    #[test]
    fn a_channel_that_clears_lets_the_call_go_ahead() {
        fn busy(cmd: &str) -> Vec<&'static str> {
            match cmd {
                "LISTEN OFF" => vec!["OK", "BUSY ON"],
                c if c.starts_with("CONNECT ") => vec!["OK", "CONNECTED EA5ABC EA5XYZ-10 2300"],
                "DISCONNECT" => vec!["OK", "DISCONNECTED"],
                _ => vec!["OK"],
            }
        }
        let tnc = fake_tnc(busy);
        let cmd = tnc.cmd_peer.clone();
        std::thread::spawn(move || {
            let mut c = cmd.recv_timeout(Duration::from_secs(2)).unwrap();
            std::thread::sleep(Duration::from_millis(500));
            c.write_all(b"BUSY OFF\r").unwrap();
        });
        let d = VaraDial { busy_wait: Duration::from_secs(5), ..dial(&tnc.addr) };
        let t = VaraTransport::connect(&d, Arc::new(AtomicBool::new(false)), None).unwrap();
        assert!(t.take_log_ref().iter().any(|l| l.contains("busy")));
    }

    #[test]
    fn a_peer_call_uses_peer_to_peer_timing() {
        let tnc = fake_tnc(gateway_answers);
        let d = VaraDial { p2p: true, ..dial(&tnc.addr) };
        let t = VaraTransport::connect(&d, Arc::new(AtomicBool::new(false)), None).unwrap();
        drop(t);
        let got = commands(&tnc);
        assert!(got.contains(&"P2P SESSION".to_string()), "{got:?}");
        assert!(!got.contains(&"WINLINK SESSION".to_string()), "{got:?}");
    }

    #[test]
    fn listening_answers_a_caller_and_stops_listening_on_the_way_out() {
        let tnc = fake_tnc(gateway_answers);
        let how = VaraListen {
            address: tnc.addr.clone(),
            mycall: "ea5abc".into(),
            bandwidth_hz: Some(2300),
        };
        let mut t = VaraTransport::listen(&how, Arc::new(AtomicBool::new(false)), None).unwrap();
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        cmd.write_all(b"CONNECTED EA5QQQ EA5ABC 2300\r").unwrap();
        assert_eq!(t.wait_call().unwrap(), "EA5QQQ");
        assert_eq!(t.target_call(), "EA5QQQ");
        t.hang_up();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            commands(&tnc),
            ["MYCALL EA5ABC", "BW2300", "P2P SESSION", "LISTEN ON", "DISCONNECT", "LISTEN OFF"]
        );
    }

    #[test]
    fn stopping_an_idle_listener_sends_no_disconnect() {
        let tnc = fake_tnc(gateway_answers);
        let how =
            VaraListen { address: tnc.addr.clone(), mycall: "EA5ABC".into(), bandwidth_hz: None };
        let stop = Arc::new(AtomicBool::new(false));
        let mut t = VaraTransport::listen(&how, Arc::clone(&stop), None).unwrap();
        stop.store(true, Ordering::SeqCst);
        assert_eq!(t.wait_call().unwrap_err().kind(), ErrorKind::Interrupted);
        drop(t);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(commands(&tnc), ["MYCALL EA5ABC", "P2P SESSION", "LISTEN ON", "LISTEN OFF"]);
    }

    #[test]
    fn no_tnc_listening_names_the_likely_cause() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        let e = VaraTransport::connect(&dial(&addr), Arc::new(AtomicBool::new(false)), None)
            .unwrap_err();
        assert!(e.to_string().contains("running"), "{e}");
    }
}

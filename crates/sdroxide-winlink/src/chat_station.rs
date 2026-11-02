//! The VARA chat station: a worker that holds the TNC in chat mode.
//!
//! While chat is enabled it stands by — the TNC in `CHAT ON`, listening —
//! shows the CQ frames it hears, answers stations that call, and calls,
//! talks and hangs up as the operator asks. What travels inside the link is
//! [`crate::varachat`]: VarAC's messages, or plain text for a station that
//! sends plain text.
//!
//! One thread, one TNC connection, and no blocking longer than a data-port
//! poll (a quarter of a second), so the operator's commands are taken
//! between polls. [`crate::manager`] starts and stops it like the Winlink
//! peer-to-peer listener, whose place on the TNC it takes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use sdroxide_types::{ChatCq, ChatFormat, ChatLine, ChatLineKind, ChatSaid, ChatState};

use crate::vara::{TncEvent, VaraListen, VaraTransport};
use crate::varachat::{self, Parser};

/// What the operator asks of the chat station.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatCmd {
    Call(String),
    Send(String),
    HangUp,
    Cq,
}

/// What the chat station reports.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    State {
        state: ChatState,
        peer: String,
        plain: bool,
    },
    Line(ChatLine),
    Heard(ChatCq),
    /// Why something asked for did not happen.
    Problem(String),
}

/// What the worker needs.
pub struct ChatCtx {
    pub how: VaraListen,
    pub ptt: Option<Sender<bool>>,
    pub stop: Arc<AtomicBool>,
    pub cmds: Receiver<ChatCmd>,
    pub events: Sender<ChatEvent>,
    pub format: ChatFormat,
    /// Sent after the opening report, when set: (name, QTH, locator).
    pub profile: Option<(String, String, String)>,
}

/// How long `<DISC>` may take to go before the link is closed regardless.
const DISC_DRAIN: Duration = Duration::from_secs(20);
/// How long to wait for a called station before giving up on it.
const CALL_TIMEOUT: Duration = Duration::from_secs(150);
/// How long to wait before trying a TNC that refused us again.
const RETRY: Duration = Duration::from_secs(10);

fn now_unix() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A QSO in progress.
struct Link {
    peer: String,
    parser: Parser,
    /// Partial plain-text line from the other station.
    plain_buf: String,
    opening_sent: bool,
    /// When `<DISC>` was sent.
    disc_at: Option<Instant>,
    /// The disconnect has been asked of the TNC.
    disconnecting: bool,
    /// When the link came up.
    since: Instant,
    /// Lines typed before the opening report went: VarAC expects the report
    /// first, so they wait for it.
    held: Vec<String>,
}

/// How long to wait for the TNC's signal report before opening without it.
const OPENING_WAIT: Duration = Duration::from_secs(8);

struct Station {
    ctx: ChatCtx,
    mycall: String,
    link: Option<Link>,
    calling: Option<(String, Instant)>,
}

impl Station {
    fn send(&self, e: ChatEvent) {
        let _ = self.ctx.events.send(e);
    }

    fn line(&self, kind: ChatLineKind, text: impl Into<String>) {
        self.send(ChatEvent::Line(ChatLine {
            at: now_unix(),
            kind,
            text: text.into(),
            ..ChatLine::default()
        }));
    }

    /// A note about the link, for the window to word.
    fn note(&self, said: ChatSaid) {
        self.send(ChatEvent::Line(ChatLine {
            at: now_unix(),
            kind: ChatLineKind::Note,
            said: vec![said],
            ..ChatLine::default()
        }));
    }

    fn state(&self, state: ChatState) {
        let (peer, plain) = match (&self.link, &self.calling) {
            (Some(l), _) => (l.peer.clone(), self.plain(l)),
            (None, Some((c, _))) => (c.clone(), false),
            _ => (String::new(), false),
        };
        self.send(ChatEvent::State { state, peer, plain });
    }

    /// Talking plain text on this link: by setting, or because the other
    /// station turned out to.
    fn plain(&self, l: &Link) -> bool {
        self.ctx.format == ChatFormat::Plain || l.parser.is_plain()
    }

    fn command(&mut self, t: &mut VaraTransport, c: ChatCmd) {
        match c {
            ChatCmd::Call(call) => {
                let call = call.trim().to_uppercase();
                if self.link.is_some() || self.calling.is_some() {
                    self.send(ChatEvent::Problem("a QSO is already in progress".into()));
                    return;
                }
                if call.is_empty() {
                    return;
                }
                match t.command(&format!("CONNECT {} {call}", self.mycall)) {
                    Ok(()) => {
                        self.note(ChatSaid::Calling(call.clone()));
                        self.calling = Some((call, Instant::now()));
                        self.state(ChatState::Calling);
                    }
                    Err(e) => self.send(ChatEvent::Problem(format!("calling {call}: {e}"))),
                }
            }
            ChatCmd::Send(text) => {
                let text = text.trim_end().to_string();
                let Some(l) = &self.link else {
                    self.send(ChatEvent::Problem("not in a QSO".into()));
                    return;
                };
                if text.is_empty() || l.disc_at.is_some() {
                    return;
                }
                if !self.plain(l) && !l.opening_sent {
                    // Shown now, sent right after the opening report.
                    if let Some(l) = &mut self.link {
                        l.held.push(text.clone());
                    }
                    self.line(ChatLineKind::Me, text);
                    return;
                }
                let bytes = if self.plain(l) {
                    varachat::plain_line(&text)
                } else {
                    varachat::frame(&text)
                };
                match t.write_data(&bytes) {
                    Ok(()) => self.line(ChatLineKind::Me, text),
                    Err(e) => self.send(ChatEvent::Problem(format!("sending: {e}"))),
                }
            }
            ChatCmd::HangUp => {
                if let Some((call, _)) = self.calling.take() {
                    let _ = t.command("ABORT");
                    t.link_reset();
                    self.note(ChatSaid::CallStopped(call));
                    self.state(ChatState::Standby);
                    return;
                }
                let plain = self.link.as_ref().is_some_and(|l| self.plain(l));
                let Some(l) = &mut self.link else { return };
                if !plain && l.disc_at.is_none() {
                    // VarAC's goodbye, then the link once it has gone.
                    let _ = t.write_data(&varachat::frame("<DISC>"));
                    l.disc_at = Some(Instant::now());
                    self.state(ChatState::Closing);
                } else if !l.disconnecting {
                    l.disconnecting = true;
                    let _ = t.command("DISCONNECT");
                    self.state(ChatState::Closing);
                }
            }
            ChatCmd::Cq => {
                if self.link.is_some() || self.calling.is_some() {
                    self.send(ChatEvent::Problem("finish the QSO before calling CQ".into()));
                    return;
                }
                let line = match self.ctx.how.bandwidth_hz {
                    Some(bw) => format!("CQFRAME {} {bw}", self.mycall),
                    None => format!("CQFRAME {}", self.mycall),
                };
                match t.command(&line) {
                    Ok(()) => self.note(ChatSaid::CqSent),
                    Err(e) => self.send(ChatEvent::Problem(format!("CQ: {e}"))),
                }
            }
        }
    }

    /// The opening report and profile to a VarAC station, once the TNC has
    /// said how well it hears them — or, `force`d after [`OPENING_WAIT`],
    /// without that (as `<R+00>`) rather than hold the QSO up. Lines typed
    /// meanwhile follow it.
    fn maybe_open(&mut self, t: &mut VaraTransport, force: bool) {
        let snr = match (t.last_snr, force) {
            (Some(s), _) => s,
            (None, true) => 0.0,
            (None, false) => return,
        };
        let plain = self.link.as_ref().is_none_or(|l| self.plain(l));
        let Some(l) = &mut self.link else { return };
        if l.opening_sent || plain {
            return;
        }
        l.opening_sent = true;
        let held = std::mem::take(&mut l.held);
        let _ = t.write_data(&varachat::frame(&varachat::opening(&self.mycall, snr)));
        if let Some((name, qth, loc)) = &self.ctx.profile
            && let Some(p) = varachat::profile(name, qth, loc)
        {
            let _ = t.write_data(&varachat::frame(&p));
        }
        for text in held {
            let _ = t.write_data(&varachat::frame(&text));
        }
    }

    fn event(&mut self, t: &mut VaraTransport, ev: TncEvent) {
        match ev {
            TncEvent::Connected { caller, answerer, .. } => {
                let ours = caller.eq_ignore_ascii_case(&self.mycall);
                let peer = if ours { answerer } else { caller };
                self.calling = None;
                t.set_peer(&peer);
                self.link = Some(Link {
                    peer: peer.clone(),
                    parser: Parser::default(),
                    plain_buf: String::new(),
                    opening_sent: false,
                    disc_at: None,
                    disconnecting: false,
                    since: Instant::now(),
                    held: Vec::new(),
                });
                self.note(if ours { ChatSaid::ConnectedTo(peer) } else { ChatSaid::Called(peer) });
                self.state(ChatState::Connected);
                self.maybe_open(t, false);
            }
            TncEvent::Disconnected => {
                if let Some(l) = self.link.take() {
                    if !l.plain_buf.trim().is_empty() {
                        self.line(ChatLineKind::Them, l.plain_buf.trim_end());
                    }
                    self.note(ChatSaid::LinkClosed(l.peer));
                } else if let Some((call, _)) = self.calling.take() {
                    self.note(ChatSaid::NoAnswer(call));
                }
                t.link_reset();
                self.state(ChatState::Standby);
            }
            TncEvent::Other(o) if o.starts_with("CQFRAME ") => {
                let mut w = o.split_whitespace().skip(1);
                let raw = w.next().unwrap_or("").to_string();
                let bw = w.next().unwrap_or("").to_string();
                if raw.is_empty() {
                    return;
                }
                let cq = varachat::decode_cq(&raw);
                self.send(ChatEvent::Heard(ChatCq {
                    raw,
                    call: cq.call,
                    offset_hz: cq.offset_hz,
                    beacon: cq.beacon,
                    bw,
                    at: now_unix(),
                }));
            }
            TncEvent::Other(o) if o.starts_with("SN ") => self.maybe_open(t, false),
            _ => {}
        }
    }

    fn received(&mut self, bytes: &[u8]) {
        let force_plain = self.ctx.format == ChatFormat::Plain;
        let Some(l) = &mut self.link else { return };
        let was_plain = l.parser.is_plain();
        let fed = if force_plain {
            varachat::Fed {
                messages: Vec::new(),
                plain: bytes.iter().map(|&b| char::from(b)).collect(),
            }
        } else {
            l.parser.feed(bytes)
        };
        let peer = l.peer.clone();
        let mut out: Vec<ChatLine> = Vec::new();
        let said =
            |kind, text, said| ChatLine { at: now_unix(), kind, text, from: peer.clone(), said };
        for m in fed.messages {
            let r = varachat::render(&m);
            if !r.text.is_empty() || !r.said.is_empty() {
                let kind = if r.note { ChatLineKind::Note } else { ChatLineKind::Them };
                out.push(said(kind, r.text, r.said));
            }
        }
        if !fed.plain.is_empty() {
            l.plain_buf.push_str(&varachat::clean(&fed.plain));
            while let Some(i) = l.plain_buf.find('\n') {
                let line: String = l.plain_buf.drain(..=i).collect();
                let line = line.trim_end().to_string();
                if !line.is_empty() {
                    out.push(said(ChatLineKind::Them, line, Vec::new()));
                }
            }
        }
        let now_plain = l.parser.is_plain();
        let turned_plain = !force_plain && now_plain && !was_plain;
        if turned_plain {
            // The state first, so the window knows how it is talking before
            // it shows what was said.
            self.state(ChatState::Connected);
            self.note(ChatSaid::PlainPeer(peer.clone()));
        }
        for l in out {
            self.send(ChatEvent::Line(l));
        }
    }

    /// Timers: a `<DISC>` that has gone (or taken too long), a call nobody
    /// answers.
    fn tick(&mut self, t: &mut VaraTransport) {
        let plain = self.link.as_ref().is_some_and(|l| self.plain(l));
        if let Some(l) = &mut self.link
            && !l.held.is_empty()
            && plain
        {
            // Held for an opening report that a plain-text station will
            // never get: send them as it talks.
            for text in std::mem::take(&mut l.held) {
                let _ = t.write_data(&varachat::plain_line(&text));
            }
        }
        if self.link.as_ref().is_some_and(|l| !l.opening_sent && l.since.elapsed() > OPENING_WAIT) {
            self.maybe_open(t, true);
        }
        if let Some(l) = &mut self.link
            && let Some(at) = l.disc_at
            && !l.disconnecting
            && ((t.buffered() == 0 && at.elapsed() > Duration::from_secs(1))
                || at.elapsed() > DISC_DRAIN)
        {
            l.disconnecting = true;
            let _ = t.command("DISCONNECT");
        }
        if let Some((call, since)) = &self.calling
            && since.elapsed() > CALL_TIMEOUT
        {
            let call = call.clone();
            self.calling = None;
            let _ = t.command("ABORT");
            t.link_reset();
            self.note(ChatSaid::NoAnswer(call));
            self.state(ChatState::Standby);
        }
    }
}

/// Stand by for chat until told to stop.
pub fn chat_loop(ctx: ChatCtx) {
    let mycall = ctx.how.mycall.trim().to_uppercase();
    let mut st = Station { ctx, mycall, link: None, calling: None };
    let stopped = |st: &Station| st.ctx.stop.load(Ordering::SeqCst);
    let mut buf = [0u8; 4096];
    while !stopped(&st) {
        let mut t = match VaraTransport::chat_standby(
            &st.ctx.how,
            Arc::clone(&st.ctx.stop),
            st.ctx.ptt.clone(),
        ) {
            Ok(t) => t,
            Err(e) => {
                st.send(ChatEvent::Problem(format!("chat: {e}")));
                st.send(ChatEvent::State {
                    state: ChatState::Off,
                    peer: String::new(),
                    plain: false,
                });
                let until = Instant::now() + RETRY;
                while Instant::now() < until && !stopped(&st) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                continue;
            }
        };
        st.link = None;
        st.calling = None;
        st.state(ChatState::Standby);
        loop {
            if stopped(&st) {
                break;
            }
            while let Ok(c) = st.ctx.cmds.try_recv() {
                st.command(&mut t, c);
            }
            let mut gone = false;
            while let Some(ev) = t.next_event(Duration::ZERO) {
                if ev == TncEvent::Closed {
                    gone = true;
                    break;
                }
                st.event(&mut t, ev);
            }
            if gone {
                st.send(ChatEvent::Problem("the TNC closed its command port".into()));
                break;
            }
            match t.read_some(&mut buf) {
                Ok(0) => {}
                Ok(n) => st.received(&buf[..n]),
                Err(e) => {
                    st.send(ChatEvent::Problem(format!("chat: {e}")));
                    break;
                }
            }
            st.tick(&mut t);
        }
        // Leaving: a QSO in progress is closed with the transport.
        drop(t);
        st.link = None;
        st.calling = None;
    }
    st.send(ChatEvent::State { state: ChatState::Off, peer: String::new(), plain: false });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vara::tests::{fake_tnc, gateway_answers};
    use std::io::{Read, Write};

    struct Run {
        cmds: Sender<ChatCmd>,
        events: Receiver<ChatEvent>,
        stop: Arc<AtomicBool>,
        handle: std::thread::JoinHandle<()>,
    }

    fn start(addr: &str, format: ChatFormat) -> Run {
        let (cmd_tx, cmds) = crossbeam_channel::unbounded();
        let (ev_tx, events) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = ChatCtx {
            how: VaraListen {
                address: addr.into(),
                mycall: "EA5ABC".into(),
                bandwidth_hz: Some(2300),
            },
            ptt: None,
            stop: Arc::clone(&stop),
            cmds,
            events: ev_tx,
            format,
            profile: Some(("José".into(), "Valencia".into(), "IM99".into())),
        };
        let handle = std::thread::spawn(move || chat_loop(ctx));
        Run { cmds: cmd_tx, events, stop, handle }
    }

    fn wait_for(r: &Run, what: &str, f: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
        let mut seen = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if let Ok(e) = r.events.recv_timeout(Duration::from_millis(50)) {
                let hit = f(&e);
                seen.push(e);
                if hit {
                    return seen;
                }
            }
        }
        panic!("timed out waiting for {what}: {seen:?}");
    }

    fn read_until(s: &mut std::net::TcpStream, want: &[u8]) -> Vec<u8> {
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut got = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && !got.windows(want.len()).any(|w| w == want) {
            let mut b = [0u8; 512];
            if let Ok(n) = s.read(&mut b) {
                got.extend_from_slice(&b[..n]);
            }
        }
        got
    }

    #[test]
    fn a_varac_qso_opens_talks_and_closes_politely() {
        let tnc = fake_tnc(gateway_answers);
        let r = start(&tnc.addr, ChatFormat::Auto);
        wait_for(&r, "standby", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Standby, .. })
        });
        let setup: Vec<String> = tnc.got.try_iter().collect();
        assert!(
            setup.contains(&"CHAT ON".to_string()) && setup.contains(&"LISTEN ON".to_string()),
            "{setup:?}"
        );

        r.cmds.send(ChatCmd::Call("ea5xyz".into())).unwrap();
        wait_for(&r, "connected", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Connected, .. })
        });
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut air = tnc.data_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        // The TNC reports how well it hears them: the opening report follows.
        cmd.write_all(b"SN 7.2\r").unwrap();
        let sent = read_until(&mut air, b"<LOC:IM99>");
        let sent = String::from_utf8_lossy(&sent);
        assert!(sent.starts_with("16 de EA5ABC <R+07>"), "{sent:?}");
        assert!(sent.contains("<NAME:José>\r\n<QTH:Valencia>\r\n<LOC:IM99>"), "{sent:?}");

        // Their report and a message, cut across modem frames.
        let mut theirs = varachat::frame("de EA5XYZ <R-03>");
        theirs.extend(varachat::frame("Hola José, 73"));
        air.write_all(&theirs[..10]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        air.write_all(&theirs[10..]).unwrap();
        let seen = wait_for(
            &r,
            "their message",
            |e| matches!(e, ChatEvent::Line(l) if l.kind == ChatLineKind::Them && l.text == "Hola José, 73"),
        );
        assert!(
            seen.iter().any(
                |e| matches!(e, ChatEvent::Line(l) if l.said == [ChatSaid::Report(-3)] && l.from == "EA5XYZ-10")
            ),
            "{seen:?}"
        );

        r.cmds.send(ChatCmd::Send("Gracias, 73".into())).unwrap();
        let sent = read_until(&mut air, b"Gracias, 73");
        assert!(String::from_utf8_lossy(&sent).ends_with("11 Gracias, 73"));

        // Goodbye: <DISC> on the link first, then the disconnect.
        r.cmds.send(ChatCmd::HangUp).unwrap();
        let sent = read_until(&mut air, b"<DISC>");
        assert!(String::from_utf8_lossy(&sent).ends_with("6 <DISC>"));
        wait_for(&r, "standby again", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Standby, .. })
        });
        assert!(tnc.got.try_iter().any(|l| l == "DISCONNECT"));

        r.stop.store(true, Ordering::SeqCst);
        r.handle.join().unwrap();
    }

    #[test]
    fn a_plain_text_caller_gets_plain_text_and_no_disc() {
        let tnc = fake_tnc(gateway_answers);
        let r = start(&tnc.addr, ChatFormat::Auto);
        wait_for(&r, "standby", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Standby, .. })
        });
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut air = tnc.data_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        cmd.write_all(b"CONNECTED EA5QQQ EA5ABC 2300\r").unwrap();
        wait_for(&r, "connected", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Connected, .. })
        });
        air.write_all(b"Hola, aqui EA5QQQ\rcon VARA Chat\r").unwrap();
        let seen = wait_for(
            &r,
            "second line",
            |e| matches!(e, ChatEvent::Line(l) if l.text == "con VARA Chat"),
        );
        assert!(seen.iter().any(|e| matches!(e, ChatEvent::State { plain: true, .. })));
        r.cmds.send(ChatCmd::Send("Recibido".into())).unwrap();
        let sent = read_until(&mut air, b"Recibido\r");
        assert!(sent.ends_with(b"Recibido\r"), "{sent:?}");
        r.cmds.send(ChatCmd::HangUp).unwrap();
        wait_for(&r, "standby", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Standby, .. })
        });
        r.stop.store(true, Ordering::SeqCst);
        r.handle.join().unwrap();
    }

    /// Typed before the TNC has given a signal report: the line waits, and
    /// goes after the opening report, as VarAC expects.
    #[test]
    fn a_line_typed_before_the_opening_follows_it() {
        let tnc = fake_tnc(gateway_answers);
        let r = start(&tnc.addr, ChatFormat::Auto);
        wait_for(&r, "standby", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Standby, .. })
        });
        r.cmds.send(ChatCmd::Call("EA5XYZ".into())).unwrap();
        wait_for(&r, "connected", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Connected, .. })
        });
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut air = tnc.data_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        r.cmds.send(ChatCmd::Send("early bird".into())).unwrap();
        wait_for(&r, "echo", |e| matches!(e, ChatEvent::Line(l) if l.text == "early bird"));
        std::thread::sleep(Duration::from_millis(300));
        cmd.write_all(b"SN -4.0\r").unwrap();
        let sent = read_until(&mut air, b"early bird");
        let sent = String::from_utf8_lossy(&sent);
        let open = sent.find("<R-04>").expect("opening sent");
        assert!(open < sent.find("early bird").unwrap(), "{sent:?}");
        r.stop.store(true, Ordering::SeqCst);
        r.handle.join().unwrap();
    }

    #[test]
    fn heard_cq_frames_are_reported_with_their_slot() {
        let tnc = fake_tnc(gateway_answers);
        let r = start(&tnc.addr, ChatFormat::Auto);
        wait_for(&r, "standby", |e| {
            matches!(e, ChatEvent::State { state: ChatState::Standby, .. })
        });
        let mut cmd = tnc.cmd_peer.recv_timeout(Duration::from_secs(2)).unwrap();
        cmd.write_all(b"CQFRAME F6JON-2 2300\r").unwrap();
        let seen = wait_for(&r, "the CQ", |e| matches!(e, ChatEvent::Heard(_)));
        match seen.last() {
            Some(ChatEvent::Heard(cq)) => {
                assert_eq!(
                    (cq.call.as_str(), cq.offset_hz, cq.beacon, cq.bw.as_str()),
                    ("F6JON", -1500, false, "2300")
                );
            }
            other => panic!("{other:?}"),
        }
        r.cmds.send(ChatCmd::Cq).unwrap();
        wait_for(
            &r,
            "CQ sent",
            |e| matches!(e, ChatEvent::Line(l) if l.said == [ChatSaid::CqSent]),
        );
        assert!(tnc.got.try_iter().any(|l| l == "CQFRAME EA5ABC 2300"));
        r.stop.store(true, Ordering::SeqCst);
        r.handle.join().unwrap();
    }
}

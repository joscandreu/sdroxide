//! A WSJT-X client talking back: what the engine does with Halt Tx, Free Text,
//! Highlight Callsign and a client's own heartbeat when they arrive on the
//! broadcast's socket.
//!
//! The rules themselves — which message may do what, how a Reply finds its
//! decode — are unit-tested in `sdroxide_wsjtx::control`. This is the other
//! half: that the engine reads the socket at all, applies what is permitted
//! through its own command path, refuses what is not, and forgets a band's
//! highlights when it leaves the band.

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdroxide_radio::{Complex32, EngineConfig, IqSource, Result, start_engine};
use sdroxide_types::{
    Command, DeviceCaps, DigiConfig, DigiStatus, Mode, QsoStep, RadioEvent, RxId, Vfo, WsjtxConfig,
};

/// Point the whole process's configuration at a scratch directory, once. See
/// `band_change.rs` for why this matters: `SetWsjtxConfig` and
/// `SetDigiConfig` are both persisted, and a test must never write the
/// operator's own files.
///
/// ⚠️ Every test in this binary must call this (through [`config`]) first.
fn isolate_config() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir =
            std::env::temp_dir().join(format!("sdroxide-wsjtx-control-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch config directory");
        // SAFETY: no engine exists yet in any test — see the note above.
        unsafe { std::env::set_var("SDROXIDE_CONFIG_DIR", &dir) }
    });
}

/// An engine configuration with a config scope of its own, so the tests in
/// this binary never share a `wsjtx.json`.
///
/// Not the primary engine: these tests set a callsign, and on the primary a
/// callsign brings up the station's network feeds — the Reverse Beacon
/// Network among them, a TCP login to the outside world that a test has no
/// business making, and one that on a machine with no route out never returns
/// while the engine waits on it. The callsign is station-wide, so one test's
/// would reach every engine started after it. The WSJT-X broadcast is per
/// radio and runs on every engine alike.
fn config(radio: u32) -> EngineConfig {
    isolate_config();
    EngineConfig {
        tx_ham_only: false,
        store: sdroxide_config::Store::radio(radio),
        primary: false,
        ..Default::default()
    }
}

const DIAL_20M: f64 = 14_074_000.0;
const DIAL_40M: f64 = 7_074_000.0;
const RATE: f64 = 48_000.0;
/// What the engine identifies itself as — and so what a client addresses.
const ID: &str = "sdroxide-test";

// Message type numbers, from WSJT-X's `NetworkMessage.hpp`.
const T_HEARTBEAT: u32 = 0;
const T_STATUS: u32 = 1;
const T_CLOSE: u32 = 6;
const T_HALT_TX: u32 = 8;
const T_FREE_TEXT: u32 = 9;
const T_HIGHLIGHT: u32 = 13;

/// A silent front end that tunes anywhere.
struct Silent {
    center: Arc<Mutex<f64>>,
}

impl IqSource for Silent {
    fn sample_rate(&self) -> f64 {
        RATE
    }
    fn center_hz(&self) -> f64 {
        *self.center.lock().unwrap()
    }
    fn set_center_hz(&mut self, hz: f64) -> Result<()> {
        *self.center.lock().unwrap() = hz;
        Ok(())
    }
    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        std::thread::sleep(Duration::from_millis(5));
        let n = buf.len().min(1024);
        buf[..n].fill(Complex32::new(0.0, 0.0));
        Ok(n)
    }
    fn describe(&self) -> String {
        "silent stand-in".into()
    }
}

fn caps() -> DeviceCaps {
    DeviceCaps {
        driver: "mock".into(),
        label: "mock".into(),
        rx_channels: 1,
        tx_channels: 1,
        sample_rates: vec![RATE],
        freq_ranges_rx: vec![(10_000.0, 60_000_000.0)],
        freq_ranges_tx: vec![(1_800_000.0, 54_000_000.0)],
        ..DeviceCaps::default()
    }
}

fn shutdown(mut h: sdroxide_radio::EngineHandles) {
    let thread = h.thread.take();
    drop(h.cmd_tx);
    if let Some(t) = thread {
        let _ = t.join();
    }
}

// ── Datagrams, as a client writes them ──────────────────────────────────────

fn datagram(kind: u32, id: &str, body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0xadbc_cbdau32.to_be_bytes());
    p.extend_from_slice(&3u32.to_be_bytes());
    p.extend_from_slice(&kind.to_be_bytes());
    qstring(&mut p, id);
    body(&mut p);
    p
}

fn qstring(p: &mut Vec<u8>, s: &str) {
    p.extend_from_slice(&(s.len() as u32).to_be_bytes());
    p.extend_from_slice(s.as_bytes());
}

fn halt(id: &str, auto_tx_only: bool) -> Vec<u8> {
    datagram(T_HALT_TX, id, |p| p.push(u8::from(auto_tx_only)))
}

fn free_text(text: &str) -> Vec<u8> {
    datagram(T_FREE_TEXT, ID, |p| {
        qstring(p, text);
        p.push(1); // send
    })
}

fn highlight(call: &str, rgb: [u8; 3]) -> Vec<u8> {
    datagram(T_HIGHLIGHT, ID, |p| {
        qstring(p, call);
        // Background: an RGB QColor. Foreground: invalid, i.e. leave it.
        p.push(1);
        for v in [
            0xFFFFu16,
            u16::from(rgb[0]) * 0x101,
            u16::from(rgb[1]) * 0x101,
            u16::from(rgb[2]) * 0x101,
            0,
        ] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        p.push(0);
        p.extend_from_slice(&[0u8; 10]);
        p.push(0); // every row, not the last only
    })
}

fn client_heartbeat(client: &str) -> Vec<u8> {
    datagram(T_HEARTBEAT, client, |p| {
        p.extend_from_slice(&3u32.to_be_bytes());
        qstring(p, "2.70");
        qstring(p, "");
    })
}

// ── The harness ─────────────────────────────────────────────────────────────

/// A client socket, and the address the engine broadcasts from — learned the
/// way a real client learns it, from the first datagram that arrives.
struct Client {
    sock: UdpSocket,
    engine: SocketAddr,
}

impl Client {
    fn send(&self, datagram: &[u8]) {
        self.sock.send_to(datagram, self.engine).unwrap();
    }

    /// Whether a Status datagram with "Tx enabled" set to `want` arrives
    /// within three seconds.
    fn status_tx_enabled(&self, want: bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut buf = [0u8; 2048];
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            self.sock.set_read_timeout(Some(left.max(Duration::from_millis(1)))).unwrap();
            let Ok(n) = self.sock.recv(&mut buf) else { return false };
            if n >= 12 && u32::from_be_bytes(buf[8..12].try_into().unwrap()) == T_STATUS {
                if let Some(enabled) = status_tx_enabled(&buf[..n])
                    && enabled == want
                {
                    return true;
                }
            }
        }
        false
    }
}

/// Read a Status datagram as far as its "Tx enabled" flag: header, dial, then
/// four strings (mode, DX call, report, TX mode) before it.
fn status_tx_enabled(b: &[u8]) -> Option<bool> {
    let mut at = 12;
    let skip_str = |at: &mut usize| -> Option<()> {
        let n = u32::from_be_bytes(b.get(*at..*at + 4)?.try_into().ok()?);
        *at += 4;
        if n != 0xFFFF_FFFF {
            *at += n as usize;
        }
        Some(())
    };
    skip_str(&mut at)?; // id
    at += 8; // dial
    for _ in 0..4 {
        skip_str(&mut at)?;
    }
    b.get(at).map(|&v| v != 0)
}

/// An engine on 20 m FT8 with a callsign set, broadcasting to a client.
fn engine_with_client(radio: u32, accept_control: bool) -> (sdroxide_radio::EngineHandles, Client) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let center = Arc::new(Mutex::new(DIAL_20M));
    let h = start_engine(Box::new(Silent { center }), caps(), config(radio));
    h.cmd_tx.send(Command::SetVfo { vfo: Vfo::A, hz: DIAL_20M }).unwrap();
    h.cmd_tx.send(Command::SetMode { rx: RxId::Main, mode: Mode::Ft8 }).unwrap();
    // Nothing goes out without a callsign, and a CQ that cannot be planned is
    // not a running CQ to halt.
    h.cmd_tx
        .send(Command::SetDigiConfig(DigiConfig {
            my_call: "AB1CD".into(),
            my_grid: "FN42".into(),
            ..DigiConfig::default()
        }))
        .unwrap();
    h.cmd_tx.send(Command::SetWsjtxConfig(wsjtx_cfg(&sock, accept_control))).unwrap();
    // Generous: an engine starting beside five others in this binary's
    // parallel run can take seconds to reach its first tick.
    sock.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut buf = [0u8; 2048];
    let (_, engine) = sock.recv_from(&mut buf).expect("the broadcast never started");
    (h, Client { sock, engine })
}

fn wsjtx_cfg(sock: &UdpSocket, accept_control: bool) -> WsjtxConfig {
    WsjtxConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: sock.local_addr().unwrap().port(),
        id: ID.into(),
        accept_control,
        ..WsjtxConfig::default()
    }
}

/// Wait up to three seconds for an event `f` picks out.
fn wait_for<T>(
    h: &sdroxide_radio::EngineHandles,
    within: Duration,
    mut f: impl FnMut(RadioEvent) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        while let Ok(ev) = h.event_rx.try_recv() {
            if let Some(t) = f(ev) {
                return Some(t);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn digi_settles(h: &sdroxide_radio::EngineHandles, mut f: impl FnMut(&DigiStatus) -> bool) -> bool {
    wait_for(h, Duration::from_secs(3), |ev| match ev {
        RadioEvent::Ft8Status(st) if f(&st) => Some(()),
        _ => None,
    })
    .is_some()
}

fn calling_cq(h: &sdroxide_radio::EngineHandles) {
    h.cmd_tx.send(Command::DigiCallCq).unwrap();
    assert!(
        digi_settles(h, |st| st.step == QsoStep::CallingCq),
        "the engine never started calling CQ"
    );
}

/// Halt Tx needs no permission: it can only take the transmitter off the air,
/// and a logger that can see us keying should be able to stop it.
#[test]
fn halt_stops_a_running_cq_even_without_control() {
    let (h, client) = engine_with_client(1, false);
    calling_cq(&h);

    client.send(&halt(ID, true));
    assert!(digi_settles(&h, |st| st.step == QsoStep::Idle), "Halt Tx did not stop the CQ");
    assert!(
        client.status_tx_enabled(false),
        "the clients should be told the station is no longer enabled to transmit"
    );
    shutdown(h);
}

/// A message addressed to another instance on the same port is not ours.
#[test]
fn a_halt_for_another_instance_is_ignored() {
    let (h, client) = engine_with_client(2, true);
    calling_cq(&h);

    client.send(&halt("WSJT-X - Rig 2", false));
    assert!(
        !digi_settles(&h, |st| st.step == QsoStep::Idle),
        "a Halt Tx addressed to another instance stopped this one"
    );
    h.cmd_tx.send(Command::DigiStopQso).unwrap();
    shutdown(h);
}

/// Free text keys the transmitter, so it waits on the operator's permission —
/// and takes effect the moment that is given, without restarting anything.
#[test]
fn free_text_is_sent_only_once_control_is_accepted() {
    let (h, client) = engine_with_client(3, false);

    client.send(&free_text("test 123"));
    assert!(
        !digi_settles(&h, |st| st.tx_pending_msg.as_deref() == Some("TEST 123")),
        "free text from a client was queued with control off"
    );

    h.cmd_tx.send(Command::SetWsjtxConfig(wsjtx_cfg(&client.sock, true))).unwrap();
    // Same destination and id: the socket stays, so the client's address
    // for us does too.
    std::thread::sleep(Duration::from_millis(200));
    client.send(&free_text("test 123"));
    assert!(
        digi_settles(&h, |st| st.tx_pending_msg.as_deref() == Some("TEST 123")),
        "free text from a client was not queued with control on"
    );
    h.cmd_tx.send(Command::DigiAbortTx).unwrap();
    h.cmd_tx.send(Command::DigiStopQso).unwrap();
    shutdown(h);
}

/// A highlight colours the decode list, and like the list it belongs to the
/// band it was made on.
#[test]
fn highlights_reach_the_screen_and_a_qsy_clears_them() {
    let (h, client) = engine_with_client(4, true);

    client.send(&highlight("w9xyz", [255, 0, 0]));
    let got = wait_for(&h, Duration::from_secs(3), |ev| match ev {
        RadioEvent::WsjtxHighlights(list) if !list.is_empty() => Some(list),
        _ => None,
    })
    .expect("the highlight never reached the screen");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].call, "W9XYZ");
    assert_eq!(got[0].bg, Some([255, 0, 0]));
    assert_eq!(got[0].fg, None);

    h.cmd_tx.send(Command::SetVfo { vfo: Vfo::A, hz: DIAL_40M }).unwrap();
    assert!(
        wait_for(&h, Duration::from_secs(3), |ev| match ev {
            RadioEvent::WsjtxHighlights(list) if list.is_empty() => Some(()),
            _ => None,
        })
        .is_some(),
        "a QSY should drop the highlights made on the band we left"
    );
    shutdown(h);
}

/// Without control a highlight is one more thing a client may not do.
#[test]
fn highlights_need_control() {
    let (h, client) = engine_with_client(5, false);
    client.send(&highlight("W9XYZ", [255, 0, 0]));
    assert!(
        wait_for(&h, Duration::from_secs(1), |ev| match ev {
            RadioEvent::WsjtxHighlights(list) if !list.is_empty() => Some(()),
            _ => None,
        })
        .is_none(),
        "a highlight was applied with control off"
    );
    shutdown(h);
}

/// The Servers tab says who is listening: a client's heartbeat names it and
/// its close removes it.
#[test]
fn clients_are_listed_by_their_heartbeat_and_dropped_by_their_close() {
    let (h, client) = engine_with_client(6, false);

    client.send(&client_heartbeat("JTAlert"));
    assert_eq!(
        wait_for(&h, Duration::from_secs(3), |ev| match ev {
            RadioEvent::WsjtxClients(c) if !c.is_empty() => Some(c),
            _ => None,
        }),
        Some(vec!["JTAlert".to_string()])
    );

    client.send(&datagram(T_CLOSE, "JTAlert", |_| {}));
    assert!(
        wait_for(&h, Duration::from_secs(3), |ev| match ev {
            RadioEvent::WsjtxClients(c) if c.is_empty() => Some(()),
            _ => None,
        })
        .is_some(),
        "a client that closed is still listed"
    );
    shutdown(h);
}

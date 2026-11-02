//! VARA chat through the engine, against a scripted TNC: standing by,
//! answering a VarAC CQ on its slot (the dial moves there and back), and the
//! QSO going through the TNC's ports.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use sdroxide_radio::{Complex32, EngineConfig, IqSource, Result, start_engine};
use sdroxide_types::{ChatState, Command, DeviceCaps, Mode, RadioEvent, RxId, WinlinkLane};

struct Silence;

impl IqSource for Silence {
    fn sample_rate(&self) -> f64 {
        192_000.0
    }
    fn center_hz(&self) -> f64 {
        14_105_000.0
    }
    fn set_center_hz(&mut self, _hz: f64) -> Result<()> {
        Ok(())
    }
    fn read(&mut self, buf: &mut [Complex32]) -> Result<usize> {
        let n = buf.len().min(1920);
        std::thread::sleep(Duration::from_secs_f64(n as f64 / 192_000.0));
        buf[..n].fill(Complex32::new(0.0, 0.0));
        Ok(n)
    }
    fn describe(&self) -> String {
        "silence".into()
    }
}

fn caps() -> DeviceCaps {
    DeviceCaps {
        driver: "mock".into(),
        label: "mock".into(),
        rx_channels: 1,
        sample_rates: vec![192_000.0],
        freq_ranges_rx: vec![(0.0, 1_000_000_000.0)],
        ..DeviceCaps::default()
    }
}

/// A TNC on two adjacent loopback ports: `OK` to everything, `CONNECTED` to a
/// `CONNECT`, and its command and data sockets handed to the test.
struct FakeTnc {
    addr: String,
    got: Receiver<String>,
    cmd: Receiver<TcpStream>,
    data: Receiver<TcpStream>,
}

fn fake_tnc() -> FakeTnc {
    let (cmd_l, data_l) = loop {
        let a = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = a.local_addr().unwrap().port();
        if p < u16::MAX
            && let Ok(b) = TcpListener::bind(("127.0.0.1", p + 1))
        {
            break (a, b);
        }
    };
    let addr = cmd_l.local_addr().unwrap().to_string();
    let (got_tx, got) = unbounded();
    let (cmd_tx, cmd) = unbounded();
    let (data_tx, data) = unbounded();
    std::thread::spawn(move || {
        let (d, _) = data_l.accept().unwrap();
        data_tx.send(d).unwrap();
    });
    std::thread::spawn(move || {
        let (c, _) = cmd_l.accept().unwrap();
        cmd_tx.send(c.try_clone().unwrap()).unwrap();
        let mut w = c.try_clone().unwrap();
        let mut r = BufReader::new(c);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\r', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    let reply: &[u8] = if line.starts_with("CONNECT ") {
                        b"OK\rSN 4.0\rCONNECTED EA5ABC F6JON 2300\r"
                    } else if line == "DISCONNECT" {
                        b"OK\rDISCONNECTED\r"
                    } else {
                        b"OK\r"
                    };
                    let _ = w.write_all(reply);
                    let _ = got_tx.send(line);
                }
            }
        }
    });
    FakeTnc { addr, got, cmd, data }
}

/// One test, because it redirects the config directory through the environment.
#[test]
fn a_varac_cq_is_answered_on_its_slot_and_the_dial_comes_back() {
    let root = std::env::temp_dir().join(format!("sdroxide-vara-chat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // SAFETY: the only test in this binary; nothing races the setter.
    unsafe { std::env::set_var("SDROXIDE_CONFIG_DIR", &root) };
    let tci_off = sdroxide_types::TciServerConfig { enabled: false, ..Default::default() };
    sdroxide_config::Store::radio(0).save_tci_server_config(&tci_off).unwrap();

    let mut h = start_engine(
        Box::new(Silence),
        caps(),
        EngineConfig { store: sdroxide_config::Store::radio(0), ..Default::default() },
    );
    let thread = h.thread.take();
    let events = h.event_rx.clone();
    let (mut dial, mut chat) = (0.0f64, ChatState::Off);
    let mut pump = |dial: &mut f64, chat: &mut ChatState| {
        for e in events.try_iter() {
            match e {
                RadioEvent::State(s) => *dial = s.active_freq_hz(),
                RadioEvent::WinlinkStatus(w) => *chat = w.chat.state,
                _ => {}
            }
        }
    };
    let mut wait = |what: &str, f: &dyn Fn(f64, ChatState) -> bool| {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            pump(&mut dial, &mut chat);
            if f(dial, chat) {
                return (dial, chat);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}: dial {dial}, chat {chat:?}");
    };

    h.cmd_tx.send(Command::SetMode { rx: RxId::Main, mode: Mode::Digu }).unwrap();
    h.cmd_tx.send(Command::SetVfo { vfo: sdroxide_types::Vfo::A, hz: 14_105_000.0 }).unwrap();
    let tnc = fake_tnc();
    let mut net = sdroxide_types::NetworkConfig::default();
    net.winlink.callsign = "EA5ABC".into();
    net.winlink.password = "unused".into();
    net.winlink.lane = WinlinkLane::Vara;
    net.winlink.vara.address = tnc.addr.clone();
    net.winlink.vara.chat.enabled = true;
    h.cmd_tx.send(Command::SetNetworkConfig(net)).unwrap();
    wait("standing by", &|d, c| c == ChatState::Standby && (d - 14_105_000.0).abs() < 1.0);
    let setup: Vec<String> = tnc.got.try_iter().collect();
    assert!(setup.contains(&"CHAT ON".to_string()), "{setup:?}");

    // F6JON-2 called CQ: it waits 1500 Hz down.
    h.cmd_tx.send(Command::VaraChatCall { call: "F6JON".into(), offset_hz: -1500 }).unwrap();
    wait("in QSO on the slot", &|d, c| c == ChatState::Connected && (d - 14_103_500.0).abs() < 1.0);
    assert!(tnc.got.try_iter().any(|l| l == "CONNECT EA5ABC F6JON"));

    // The opening report went out on the link.
    let mut air = tnc.data.recv_timeout(Duration::from_secs(2)).unwrap();
    air.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let mut sent = Vec::new();
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end && !String::from_utf8_lossy(&sent).contains("<R+04>") {
        let mut b = [0u8; 256];
        if let Ok(n) = air.read(&mut b) {
            sent.extend_from_slice(&b[..n]);
        }
    }
    assert!(String::from_utf8_lossy(&sent).starts_with("16 de EA5ABC <R+04>"), "{sent:?}");

    // The link drops: standing by again, back on the calling frequency.
    let mut cmd = tnc.cmd.recv_timeout(Duration::from_secs(2)).unwrap();
    cmd.write_all(b"DISCONNECTED\r").unwrap();
    wait("back home", &|d, c| c == ChatState::Standby && (d - 14_105_000.0).abs() < 1.0);

    drop(h.cmd_tx);
    if let Some(t) = thread {
        let _ = t.join();
    }
    let _ = std::fs::remove_dir_all(&root);
}

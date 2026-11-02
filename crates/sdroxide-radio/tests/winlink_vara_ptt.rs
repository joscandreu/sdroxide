//! Winlink over an external VARA TNC, through the engine: the TNC's `PTT ON`
//! and `PTT OFF` key and unkey this radio, a session that ends keyed unkeys
//! it, and a digital mode (whose own modem owns the transmitter) refuses the
//! call.
//!
//! The TNC is a scripted stand-in on two adjacent loopback ports, so nothing
//! here needs xara, VARA, a sound card or a radio.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use sdroxide_radio::{Complex32, EngineConfig, IqSource, Result, start_engine};
use sdroxide_types::{Command, DeviceCaps, Mode, RadioEvent, RxId, WinlinkLane};

#[derive(Default)]
struct Log {
    keyed: bool,
    keys: u32,
    unkeys: u32,
}

struct Rig {
    log: Arc<Mutex<Log>>,
}

impl IqSource for Rig {
    fn sample_rate(&self) -> f64 {
        192_000.0
    }
    fn center_hz(&self) -> f64 {
        14_100_000.0
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
        "vara rig".into()
    }
    fn tx_begin(&mut self, _center_hz: f64, rate: f64) -> Result<f64> {
        let mut l = self.log.lock().unwrap();
        l.keyed = true;
        l.keys += 1;
        Ok(rate)
    }
    fn tx_write(&mut self, _samples: &[Complex32]) -> Result<()> {
        Ok(())
    }
    fn tx_end(&mut self) -> Result<()> {
        let mut l = self.log.lock().unwrap();
        l.keyed = false;
        l.unkeys += 1;
        Ok(())
    }
}

fn caps() -> DeviceCaps {
    DeviceCaps {
        driver: "vara".into(),
        label: "vara".into(),
        rx_channels: 1,
        tx_channels: 1,
        sample_rates: vec![192_000.0],
        freq_ranges_rx: vec![(0.0, 1_000_000_000.0)],
        freq_ranges_tx: vec![(0.0, 1_000_000_000.0)],
        ..DeviceCaps::default()
    }
}

/// A TNC that answers every command `OK` and a `CONNECT` with `CONNECTED`, and
/// hands its command socket to the test for the notices it sends unasked.
struct FakeTnc {
    addr: String,
    cmd: Receiver<TcpStream>,
    got: Receiver<String>,
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
    let (cmd_tx, cmd) = unbounded();
    let (got_tx, got) = unbounded();
    std::thread::spawn(move || {
        // Held open for the session; nothing is sent on it here.
        let (d, _) = data_l.accept().unwrap();
        std::thread::sleep(Duration::from_secs(30));
        drop(d);
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
                        b"OK\rCONNECTED EA5ABC EA5XYZ 2300\r"
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
    FakeTnc { addr, cmd, got }
}

fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

/// One test, because it redirects the config directory through the environment,
/// which is process-global.
#[test]
fn a_vara_tnc_keys_and_unkeys_the_radio() {
    let root = std::env::temp_dir().join(format!("sdroxide-vara-ptt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // SAFETY: this is the only test in this binary; nothing races the setter.
    unsafe { std::env::set_var("SDROXIDE_CONFIG_DIR", &root) };
    let tci_off = sdroxide_types::TciServerConfig { enabled: false, ..Default::default() };
    sdroxide_config::Store::radio(0).save_tci_server_config(&tci_off).unwrap();

    let log = Arc::new(Mutex::new(Log::default()));
    let (producer, _consumer) = sdroxide_radio::rtrb::RingBuffer::<f32>::new(48_000);
    let cfg = EngineConfig {
        tx_ham_only: false,
        audio: Some(sdroxide_radio::AudioParams { producer, out_rate: 48_000.0 }),
        store: sdroxide_config::Store::radio(0),
        ..Default::default()
    };
    let mut h = start_engine(Box::new(Rig { log: Arc::clone(&log) }), caps(), cfg);
    let thread = h.thread.take();
    let events = h.event_rx.clone();
    let notices = move || -> Vec<String> {
        events
            .try_iter()
            .filter_map(|e| match e {
                RadioEvent::Notice(Some(n)) => Some(n),
                _ => None,
            })
            .collect()
    };

    let tnc = fake_tnc();
    let mut net = sdroxide_types::NetworkConfig::default();
    net.winlink.callsign = "EA5ABC".into();
    net.winlink.password = "unused".into();
    net.winlink.lane = WinlinkLane::Vara;
    net.winlink.vara.address = tnc.addr.clone();
    net.winlink.vara.gateway = "ea5xyz".into();
    h.cmd_tx.send(Command::SetNetworkConfig(net)).unwrap();

    // In a digital mode the engine's own modem owns the transmitter: refused.
    h.cmd_tx.send(Command::SetMode { rx: RxId::Main, mode: Mode::Ft8 }).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    notices();
    h.cmd_tx.send(Command::WinlinkConnect).unwrap();
    let mut said = Vec::new();
    wait("the refusal", 5, || {
        said.extend(notices());
        said.iter().any(|n| n.contains("DIGU"))
    });
    assert!(tnc.got.try_recv().is_err(), "a refused call still reached the TNC");

    // In DIGU it calls, and the TNC's PTT keys this radio.
    h.cmd_tx.send(Command::SetMode { rx: RxId::Main, mode: Mode::Digu }).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    h.cmd_tx.send(Command::WinlinkConnect).unwrap();
    let mut cmd = tnc.cmd.recv_timeout(Duration::from_secs(5)).expect("the TNC was dialled");
    wait("the CONNECT", 5, || tnc.got.try_iter().any(|l| l == "CONNECT EA5ABC EA5XYZ"));

    cmd.write_all(b"PTT ON\r").unwrap();
    wait("the TNC's PTT ON to key the radio", 3, || log.lock().unwrap().keyed);
    cmd.write_all(b"PTT OFF\r").unwrap();
    wait("the TNC's PTT OFF to unkey it", 3, || !log.lock().unwrap().keyed);

    // Keyed when the link drops: the session ends, and so must the over.
    cmd.write_all(b"PTT ON\r").unwrap();
    wait("keyed again", 3, || log.lock().unwrap().keyed);
    cmd.write_all(b"DISCONNECTED\r").unwrap();
    wait("the session ending to unkey the radio", 5, || !log.lock().unwrap().keyed);
    {
        let l = log.lock().unwrap();
        assert_eq!((l.keys, l.unkeys), (2, 2));
    }

    drop(h.cmd_tx);
    if let Some(t) = thread {
        let _ = t.join();
    }
    let _ = std::fs::remove_dir_all(&root);
}

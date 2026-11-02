//! Running the VARA TNC (xara) for the operator.
//!
//! The TNC is the operator's own build of xara. Nothing of it is part of
//! sdroxide: this module only puts together the command line the settings
//! describe, starts the program, reads what it prints, and notices when it
//! stops. [`crate::manager`] decides when to start, restart and stop it.
//!
//! The command line is xara's documented one:
//!
//! ```text
//! xara --call CALL --addr 127.0.0.1 --port 8300 --kiss-port off \
//!      [--audio BACKEND] --capture DEV --playback DEV --hf 2300 \
//!      --radio --ptt vox|rigctld:HOST:PORT --ptt-lead-ms 50 --drive-db -5 [extra…]
//! ```
//!
//! xara prints `xara 0.1.3: CALL 2300 Hz, cmd … data …, CAPTURE -> PLAYBACK`
//! once its ports are bound, which is what "ready" means here.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crossbeam_channel::{Receiver, unbounded};
use sdroxide_types::{ManagedTncConfig, TncDevice, TncPtt};

/// What `--list-devices` gives: capture devices, then playback devices.
pub type DeviceLists = (Vec<TncDevice>, Vec<TncDevice>);

/// Lines of the TNC's output kept to explain a failure.
const TAIL: usize = 8;

/// The local port the managed TNC should listen on, from the VARA route's
/// address. It runs on this machine, so the address has to say so.
pub fn local_port(address: &str) -> Result<u16, String> {
    let (host, port) = address
        .trim()
        .rsplit_once(':')
        .ok_or_else(|| format!("the TNC address {address:?} has no port"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(format!(
            "sdroxide runs the TNC on this machine, so the TNC address must be local \
             (127.0.0.1:8300), not {address}"
        ));
    }
    let port: u16 =
        port.parse().map_err(|_| format!("the TNC port in {address:?} is not a number"))?;
    if port == 0 || port == u16::MAX {
        return Err(format!("the TNC port must be 1–65534 (the data port is one above): {port}"));
    }
    Ok(port)
}

/// The command line (without the program) for these settings.
///
/// `rigctld` is sdroxide's own rigctld server, `host:port`, when it is on; the
/// rigctld keying method needs it.
pub fn tnc_args(
    m: &ManagedTncConfig,
    mycall: &str,
    address: &str,
    rigctld: Option<&str>,
) -> Result<Vec<String>, String> {
    if m.exe.trim().is_empty() {
        return Err("set the path to xara".into());
    }
    let call = mycall.trim().to_uppercase();
    if call.is_empty() {
        return Err("set the Winlink callsign: the TNC needs one to run".into());
    }
    if m.capture.trim().is_empty() || m.playback.trim().is_empty() {
        return Err("choose the TNC's capture and playback devices".into());
    }
    let port = local_port(address)?;
    let ptt = match m.ptt {
        TncPtt::Notices => "vox".to_string(),
        TncPtt::Rigctld => match rigctld {
            Some(a) => format!("rigctld:{a}"),
            None => {
                return Err("keying through rigctld needs sdroxide's rigctld server: turn it on \
                            in Settings"
                    .into());
            }
        },
    };
    let mut a: Vec<String> = vec![
        "--call".into(),
        call,
        "--addr".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--kiss-port".into(),
        if m.kiss_port == 0 { "off".into() } else { m.kiss_port.to_string() },
    ];
    if let Some(b) = m.audio.arg() {
        a.extend(["--audio".into(), b.into()]);
    }
    a.extend([
        "--capture".into(),
        m.capture.trim().to_string(),
        "--playback".into(),
        m.playback.trim().to_string(),
    ]);
    a.extend(m.mode.args().map(String::from));
    a.extend([
        "--radio".into(),
        "--ptt".into(),
        ptt,
        "--ptt-lead-ms".into(),
        m.ptt_lead_ms.min(1000).to_string(),
        "--drive-db".into(),
        format!("{}", m.drive_db.clamp(-30.0, 0.0)),
    ]);
    a.extend(m.extra_args.split_whitespace().map(str::to_string));
    Ok(a)
}

/// `xara --list-devices` output → (capture, playback).
pub fn parse_devices(out: &str) -> (Vec<TncDevice>, Vec<TncDevice>) {
    let (mut cap, mut play) = (Vec::new(), Vec::new());
    let mut into: Option<bool> = None; // Some(true) capture, Some(false) playback
    for line in out.lines() {
        if !line.starts_with(' ') {
            let l = line.trim_end();
            into = if l.ends_with(" capture:") {
                Some(true)
            } else if l.ends_with(" playback:") {
                Some(false)
            } else {
                None
            };
            continue;
        }
        let item = line.trim();
        if item.is_empty() {
            continue;
        }
        // `  id  (description)`, or just `  id`. Ids can contain single spaces
        // (CoreAudio UIDs do), so the split is on the two-space gap before the
        // last parenthesis.
        let dev = match item.rsplit_once("  (") {
            Some((id, desc)) if desc.ends_with(')') => TncDevice {
                id: id.trim().to_string(),
                desc: desc.trim_end_matches(')').to_string(),
            },
            _ => TncDevice { id: item.to_string(), desc: String::new() },
        };
        match into {
            Some(true) => cap.push(dev),
            Some(false) => play.push(dev),
            None => {}
        }
    }
    (cap, play)
}

/// The version from xara's start-up line, which it prints once its ports are
/// bound: `xara 0.1.3: EA5ABC 2300 Hz, cmd …`.
pub fn ready_version(line: &str) -> Option<String> {
    let rest = line.strip_prefix("xara ")?;
    let (version, tail) = rest.split_once(':')?;
    (version.chars().next()?.is_ascii_digit() && tail.contains(" cmd "))
        .then(|| version.to_string())
}

/// Ask the TNC which audio devices it can open. Blocking; run it off the
/// engine's thread.
pub fn list_devices(exe: &str, audio: sdroxide_types::TncAudio) -> Result<DeviceLists, String> {
    let mut cmd = Command::new(exe.trim());
    cmd.arg("--list-devices");
    if let Some(b) = audio.arg() {
        cmd.args(["--audio", b]);
    }
    no_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("cannot run {exe}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().last().unwrap_or("listing devices failed").to_string());
    }
    Ok(parse_devices(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(windows)]
fn no_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: a console program started from a GUI would otherwise
    // open a console window of its own.
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_console(_cmd: &mut Command) {}

/// What a running TNC process has done since it was last asked.
#[derive(Debug, PartialEq)]
pub enum TncEvent {
    /// Its ports are open; the version it gave.
    Ready(String),
    /// It stopped; why, as well as can be told.
    Exited(String),
}

/// The TNC process, and the threads reading its output.
pub struct TncProcess {
    child: Child,
    lines: Receiver<String>,
    pub started: Instant,
    pub ready: bool,
    tail: Vec<String>,
}

impl TncProcess {
    pub fn spawn(exe: &str, args: &[String]) -> Result<Self, String> {
        let mut cmd = Command::new(exe.trim());
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        no_console(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", exe.trim()))?;
        let (tx, lines) = unbounded();
        for stream in [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            let _ = std::thread::Builder::new().name("winlink-tnc-out".into()).spawn(move || {
                let mut r = BufReader::new(stream);
                let mut buf = Vec::new();
                while matches!(r.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                    let l = String::from_utf8_lossy(&buf).trim_end().to_string();
                    buf.clear();
                    if !l.is_empty() && tx.send(l).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(TncProcess { child, lines, started: Instant::now(), ready: false, tail: Vec::new() })
    }

    /// Read what it printed and check whether it is still running.
    pub fn pump(&mut self) -> Vec<TncEvent> {
        let mut ev = Vec::new();
        while let Ok(l) = self.lines.try_recv() {
            tracing::debug!("xara: {l}");
            if !self.ready
                && let Some(v) = ready_version(&l)
            {
                self.ready = true;
                ev.push(TncEvent::Ready(v));
            }
            self.tail.push(l);
            if self.tail.len() > TAIL {
                self.tail.remove(0);
            }
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            // Its last words usually say why; give the reader threads a moment.
            std::thread::sleep(std::time::Duration::from_millis(50));
            while let Ok(l) = self.lines.try_recv() {
                self.tail.push(l);
            }
            let why =
                self.tail.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
            ev.push(TncEvent::Exited(if why.is_empty() {
                format!("xara stopped ({status})")
            } else {
                format!("xara stopped ({status}): {why}")
            }));
        }
        ev
    }
}

impl Drop for TncProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::{TncAudio, TncMode};

    fn cfg() -> ManagedTncConfig {
        ManagedTncConfig {
            enabled: true,
            exe: "/opt/xara/bin/xara".into(),
            capture: "BlackHole2ch_UID".into(),
            playback: "BlackHole16ch_UID".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_command_line_is_xaras() {
        let a = tnc_args(&cfg(), "ea5abc", "127.0.0.1:8300", None).unwrap();
        assert_eq!(
            a.join(" "),
            "--call EA5ABC --addr 127.0.0.1 --port 8300 --kiss-port off --capture BlackHole2ch_UID \
             --playback BlackHole16ch_UID --hf 2300 --radio --ptt vox --ptt-lead-ms 50 --drive-db -5"
        );
        let m = ManagedTncConfig {
            audio: TncAudio::Pulse,
            mode: TncMode::FmWide,
            ptt: TncPtt::Rigctld,
            kiss_port: 8100,
            extra_args: "--tx-buffer-ms 80".into(),
            ..cfg()
        };
        let a = tnc_args(&m, "EA5ABC", "localhost:8400", Some("127.0.0.1:4532")).unwrap().join(" ");
        assert!(a.contains("--audio pulse"), "{a}");
        assert!(a.contains("--fm wide"), "{a}");
        assert!(a.contains("--ptt rigctld:127.0.0.1:4532"), "{a}");
        assert!(a.contains("--port 8400 --kiss-port 8100"), "{a}");
        assert!(a.ends_with("--tx-buffer-ms 80"), "{a}");
    }

    #[test]
    fn what_is_missing_is_named() {
        let e = |m: ManagedTncConfig, call: &str, addr: &str, rig: Option<&str>| {
            tnc_args(&m, call, addr, rig).unwrap_err()
        };
        assert!(
            e(ManagedTncConfig { exe: String::new(), ..cfg() }, "X", "127.0.0.1:8300", None)
                .contains("path")
        );
        assert!(e(cfg(), "", "127.0.0.1:8300", None).contains("callsign"));
        assert!(
            e(ManagedTncConfig { capture: String::new(), ..cfg() }, "X", "127.0.0.1:8300", None)
                .contains("capture")
        );
        assert!(e(cfg(), "X", "10.0.0.5:8300", None).contains("local"));
        assert!(
            e(ManagedTncConfig { ptt: TncPtt::Rigctld, ..cfg() }, "X", "127.0.0.1:8300", None)
                .contains("rigctld server")
        );
        assert!(e(cfg(), "X", "127.0.0.1:65535", None).contains("65534"));
    }

    #[test]
    fn device_lists_parse_including_ids_with_spaces() {
        let out = "coreaudio capture:\n  BlackHole2ch_UID  (BlackHole 2ch, virtual)\n  \
                   BuiltInMicrophoneDevice  (Micrófono del MacBook Air)\ncoreaudio playback:\n  \
                   AppleUSBAudioEngine:DisplayLink:USB-C Hybrid 4K video Docking Station:E8EA:3  (USB Audio)\n  \
                   default\n";
        let (cap, play) = parse_devices(out);
        assert_eq!(cap.len(), 2);
        assert_eq!(
            cap[0],
            TncDevice { id: "BlackHole2ch_UID".into(), desc: "BlackHole 2ch, virtual".into() }
        );
        assert_eq!(
            play[0].id,
            "AppleUSBAudioEngine:DisplayLink:USB-C Hybrid 4K video Docking Station:E8EA:3"
        );
        assert_eq!(play[1], TncDevice { id: "default".into(), desc: String::new() });
    }

    #[test]
    fn the_start_up_line_means_ready() {
        assert_eq!(
            ready_version(
                "xara 0.1.3: EA5ABC 2300 Hz, cmd 127.0.0.1:8300 data 127.0.0.1:8301, a -> b"
            ),
            Some("0.1.3".into())
        );
        assert_eq!(ready_version("xara: audio: TX path 72 ms"), None);
        assert_eq!(ready_version("xara 0.1.3: EA5ABC listening"), None);
    }

    /// A stand-in TNC: prints the start-up line, then fails after a moment.
    #[cfg(unix)]
    #[test]
    fn a_process_is_seen_ready_and_seen_to_stop() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("sdroxide-tnc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("xara");
        std::fs::write(
            &exe,
            "#!/bin/sh\necho \"xara 0.1.3: $2 2300 Hz, cmd 127.0.0.1:8300 data 127.0.0.1:8301, a -> b\" >&2\n\
             sleep 0.3\necho 'xara: playback device vanished' >&2\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let args = tnc_args(
            &ManagedTncConfig { exe: exe.display().to_string(), ..cfg() },
            "EA5ABC",
            "127.0.0.1:8300",
            None,
        )
        .unwrap();
        let mut p = TncProcess::spawn(&exe.display().to_string(), &args).unwrap();
        let mut seen = Vec::new();
        let end = Instant::now() + std::time::Duration::from_secs(5);
        while Instant::now() < end && !seen.iter().any(|e| matches!(e, TncEvent::Exited(_))) {
            seen.extend(p.pump());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(seen[0], TncEvent::Ready("0.1.3".into()));
        match &seen[1] {
            TncEvent::Exited(why) => assert!(why.contains("playback device vanished"), "{why}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

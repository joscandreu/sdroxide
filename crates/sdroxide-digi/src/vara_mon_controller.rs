//! `VaraMonController` — VARA HF traffic, monitored through the operator's own
//! `xara-monitor`.
//!
//! Nothing here decodes VARA. The controller runs the program the operator
//! pointed it at (see [`VaraMonConfig`]) as a child process:
//!
//! ```text
//! xara-monitor --stdin 48000 --json --hf <bw> --call <each known call> [--no-blind] [extra]
//! ```
//!
//! It writes the receiver's audio to the child's standard input as 16-bit mono
//! at 48 kHz, and reads back one JSON record per line from its standard output
//! — the monitor's `--json` format. A [`Tracker`] turns those records into the
//! traffic log, the heard list and the link in progress that the panel draws.
//!
//! # Why a child process
//!
//! Not bundling the decoder is the point. Which build of the monitor runs, and
//! with which features, is the operator's decision; sdroxide only knows its
//! command line and its output. The price is that the output format is not a
//! published interface, so the parser takes what it recognises, ignores what it
//! does not, and never fails on a field it was not expecting.
//!
//! # Threads
//!
//! The engine calls [`DigiEngine::on_rx_audio`] and [`DigiEngine::poll`] from
//! its real-time loop, so neither may block on the pipe. Audio goes through a
//! bounded channel to a writer thread, and blocks the monitor cannot take in
//! time are dropped and counted rather than stalling the radio. A reader thread
//! per output stream hands lines back through another channel.
//!
//! # Receive only
//!
//! It listens to other stations' links. Transmitting VARA is xara's modem,
//! which is a different program.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use sdroxide_dsp::MonoResampler;
use sdroxide_types::{
    DigiConfig, DigiStatus, Mode, QsoStep, TranscriptLine, VARA_MON_LINES_MAX,
    VARA_MON_STATIONS_MAX, VaraMonConfig, VaraMonKind, VaraMonLine, VaraMonLink, VaraMonState,
    VaraMonStation, VaraMonStatus,
};
use serde_json::Value;

use crate::DigiEngine;
use crate::controller::DigiAction;

/// The rate the monitor works at internally, so it has nothing to resample.
const PIPE_RATE: f64 = 48_000.0;
/// Samples per block sent down the pipe: 100 ms.
const BLOCK: usize = 4_800;
/// Blocks that may queue for the writer before new ones are dropped: 5 s.
const QUEUE_BLOCKS: usize = 50;
/// The oldest `xara-monitor` this was written against.
pub const TESTED_VERSION: (u32, u32, u32) = (0, 1, 3);
/// Most characters of one payload shown.
const PAYLOAD_SHOW_MAX: usize = 2_000;
/// A link nobody has heard from for this long is taken to have ended, even
/// without the QRT that says so — the monitor can miss the end of a link.
const LINK_STALE_S: i64 = 180;
/// Most error-output lines remembered to explain a failure.
const STDERR_KEEP: usize = 6;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What one output thread hands back.
enum Out {
    Line(String),
    Err(String),
}

/// The command line for a config, without the program itself. Public so the
/// panel's "what will run" hint and the tests agree with what is spawned.
pub fn monitor_args(cfg: &VaraMonConfig, my_call: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--stdin".into(),
        format!("{}", PIPE_RATE as u32),
        "--json".into(),
        "--hf".into(),
        cfg.bandwidth.hz().to_string(),
    ];
    let mut calls = cfg.call_list();
    let me = my_call.trim().to_ascii_uppercase();
    if !me.is_empty() && !calls.contains(&me) {
        calls.insert(0, me);
    }
    for c in calls {
        args.push("--call".into());
        args.push(c);
    }
    if !cfg.blind {
        args.push("--no-blind".into());
    }
    args.extend(cfg.extra_args.split_whitespace().map(str::to_string));
    args
}

/// `"0.1.3"` → `(0, 1, 3)`; anything after a `-` or `+` is ignored.
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.trim().split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

/// Bytes as the monitor sends them (one code point per byte), made safe to
/// show: line breaks kept, other control characters shown as a middle dot.
fn printable(data: &str) -> String {
    let mut out = String::with_capacity(data.len().min(PAYLOAD_SHOW_MAX));
    let mut prev_cr = false;
    for c in data.chars() {
        if out.len() >= PAYLOAD_SHOW_MAX {
            out.push('…');
            break;
        }
        match c {
            '\r' => out.push('\n'),
            '\n' if prev_cr => {}
            '\n' | '\t' => out.push(c),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
        prev_cr = c == '\r';
    }
    out
}

fn role(v: &Value, key: &str) -> Option<usize> {
    match v.get(key)?.as_str()? {
        "caller" => Some(0),
        "answerer" => Some(1),
        _ => None,
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn f32_of(v: &Value, key: &str) -> Option<f32> {
    v.get(key).and_then(Value::as_f64).map(|x| x as f32)
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// A callsign worth putting in the heard list — not empty, not an unresolved
/// `#HASH`.
fn named(call: &str) -> bool {
    !call.is_empty() && !call.starts_with('#')
}

/// The monitor's records, folded into what the panel shows.
#[derive(Default)]
pub(crate) struct Tracker {
    pub(crate) show_payload: bool,
    pub(crate) status: VaraMonStatus,
    /// The two ends of the current link as far as known: caller, answerer.
    /// A callsign, an unresolved `#HASH`, or empty.
    calls: [String; 2],
    /// When anything was last heard on the current link.
    link_seen: i64,
}

impl Tracker {
    pub(crate) fn new(show_payload: bool) -> Self {
        Tracker { show_payload, ..Default::default() }
    }

    pub(crate) fn clear(&mut self) {
        self.status.lines.clear();
        self.status.stations.clear();
        self.status.transmissions = 0;
        self.status.links = 0;
    }

    fn push_line(&mut self, line: VaraMonLine) {
        self.status.lines.push(line);
        let n = self.status.lines.len();
        if n > VARA_MON_LINES_MAX {
            self.status.lines.drain(..n - VARA_MON_LINES_MAX);
        }
    }

    fn line(&mut self, at: i64, kind: VaraMonKind, text: String, snr_db: Option<f32>) {
        self.push_line(VaraMonLine { at, kind, text, snr_db, payload: None });
    }

    fn station(&mut self, call: &str, at: i64) -> Option<&mut VaraMonStation> {
        if !named(call) {
            return None;
        }
        let list = &mut self.status.stations;
        let st = match list.iter().position(|s| s.call == call) {
            Some(i) => list.remove(i),
            None => VaraMonStation { call: call.to_string(), first_at: at, ..Default::default() },
        };
        list.insert(0, st);
        list.truncate(VARA_MON_STATIONS_MAX);
        list.first_mut()
    }

    /// A frame heard from `call`, at this SNR and (for data) speed level.
    fn heard(&mut self, call: &str, at: i64, snr: Option<f32>, level: Option<u8>) {
        let Some(st) = self.station(call, at) else { return };
        st.last_at = at;
        st.frames += 1;
        if let Some(s) = snr {
            st.last_snr_db = Some(s);
            st.best_snr_db = Some(st.best_snr_db.map_or(s, |b| b.max(s)));
        }
        if let Some(l) = level {
            st.top_level = st.top_level.max(l);
        }
    }

    /// One end of the link has a name now.
    fn learn(&mut self, who: usize, name: &str) {
        if name.is_empty() {
            return;
        }
        // A resolved callsign is never overwritten by the hash it came from.
        if !named(name) && named(&self.calls[who]) {
            return;
        }
        self.calls[who] = name.to_string();
        if let Some(link) = &mut self.status.link {
            if who == 0 {
                link.caller = name.to_string();
            } else {
                link.answerer = name.to_string();
            }
        }
        let [a, b] = self.calls.clone();
        if named(&a) && named(&b) {
            let now = self.link_seen;
            if let Some(st) = self.station(&a, now) {
                st.peer = b.clone();
            }
            if let Some(st) = self.station(&b, now) {
                st.peer = a;
            }
        }
    }

    fn ensure_link(&mut self, at: i64) {
        let stale = self.status.link.is_some() && at - self.link_seen > LINK_STALE_S;
        if stale {
            self.status.link = None;
            self.calls = Default::default();
        }
        if self.status.link.is_none() {
            self.status.link = Some(VaraMonLink {
                caller: self.calls[0].clone(),
                answerer: self.calls[1].clone(),
                started_at: at,
                ..Default::default()
            });
        }
        self.link_seen = at;
    }

    fn who(&self, r: Option<usize>) -> String {
        match r {
            Some(i) if !self.calls[i].is_empty() => self.calls[i].clone(),
            Some(0) => "caller".into(),
            Some(_) => "answerer".into(),
            None => "?".into(),
        }
    }

    /// One line of the monitor's output. Returns whether anything changed.
    pub(crate) fn ingest(&mut self, raw: &str, now: i64) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
            return false;
        };
        let at = v.get("unix").and_then(Value::as_f64).map_or(now, |u| u as i64);
        let text = str_of(&v, "text").trim().to_string();
        match str_of(&v, "t") {
            "start" => {
                self.status.version = str_of(&v, "version").to_string();
                self.status.state = VaraMonState::Running;
                match parse_version(&self.status.version) {
                    Some(have) if have < TESTED_VERSION => {
                        let (a, b, c) = TESTED_VERSION;
                        self.status.message = format!(
                            "xara-monitor {} is older than {a}.{b}.{c}, the version this panel was \
                             tested with; some records may not show.",
                            self.status.version
                        );
                    }
                    _ => self.status.message.clear(),
                }
            }
            "ctrl" => {
                let sender = role(&v, "sender");
                let key_kind = role(&v, "key_kind");
                let key_name = str_of(&v, "key_name").to_string();
                let snr = f32_of(&v, "snr");
                let connect = str_of(&v, "code_name") == "CONNECT" && sender == Some(0);
                if connect {
                    // A connect request is repeated until it is answered, so
                    // only one that names a different station starts a new link.
                    let other = self
                        .status
                        .link
                        .as_ref()
                        .is_some_and(|l| !l.answerer.is_empty() && l.answerer != key_name);
                    if other {
                        self.status.link = None;
                        self.calls = Default::default();
                    }
                }
                self.ensure_link(at);
                if let Some(k) = key_kind {
                    self.learn(k, &key_name);
                }
                if let Some(s) = sender {
                    let call = self.calls[s].clone();
                    self.heard(&call, at, snr, None);
                }
                let kind = if connect { VaraMonKind::Connect } else { VaraMonKind::Control };
                self.line(at, kind, text, snr);
            }
            "learned" => {
                if let Some(k) = role(&v, "key_kind") {
                    let name = str_of(&v, "key_name").to_string();
                    self.learn(k, &name);
                }
                self.line(at, VaraMonKind::Info, text, None);
            }
            "data" => {
                self.status.transmissions += 1;
                self.ensure_link(at);
                let level = u8::try_from(u64_of(&v, "level")).unwrap_or(u8::MAX);
                let sn = f32_of(&v, "sn");
                if let Some(link) = &mut self.status.link {
                    link.level = level;
                }
                if let Some(s) = role(&v, "sender") {
                    let call = self.calls[s].clone();
                    self.heard(&call, at, sn, Some(level));
                }
                self.line(at, VaraMonKind::Data, text, sn);
            }
            "header" => {
                self.ensure_link(at);
                let call = str_of(&v, "call").to_string();
                let answerer = str_of(&v, "answerer").to_string();
                self.learn(0, &call);
                self.learn(1, &answerer);
                self.line(at, VaraMonKind::Header, text, None);
            }
            "payload" => {
                self.ensure_link(at);
                let sender = role(&v, "sender");
                let len = u64_of(&v, "len");
                let status = str_of(&v, "status");
                if status == "data"
                    && let (Some(s), Some(link)) = (sender, &mut self.status.link)
                {
                    link.bytes[s] += len;
                }
                let from = self.who(sender);
                let to = self.who(sender.map(|s| 1 - s));
                let what = match status {
                    "data" => format!("{len} B"),
                    "repeat" => format!("{len} B (repeat)"),
                    "no_data" => "no data (terminator check failed)".into(),
                    "text_failed" => "text decompression failed".into(),
                    other => other.to_string(),
                };
                let payload = (self.show_payload && status == "data")
                    .then(|| printable(str_of(&v, "data")))
                    .filter(|p| !p.is_empty());
                self.push_line(VaraMonLine {
                    at,
                    kind: VaraMonKind::Payload,
                    text: format!("  {from} → {to}: {what}"),
                    snr_db: None,
                    payload,
                });
            }
            "link_end" => {
                let caller = str_of(&v, "caller").to_string();
                let answerer = str_of(&v, "answerer").to_string();
                self.status.links += 1;
                for c in [&caller, &answerer] {
                    if let Some(st) = self.station(c, at) {
                        st.links += 1;
                    }
                }
                self.status.link = None;
                self.calls = Default::default();
                self.line(at, VaraMonKind::LinkEnd, text, None);
            }
            "cq" => {
                let call = str_of(&v, "call").to_string();
                if let Some(st) = self.station(&call, at) {
                    st.cq = true;
                }
                self.heard(&call, at, None, None);
                self.line(at, VaraMonKind::Cq, text, None);
            }
            "kiss" => {
                let len = u64_of(&v, "len");
                let payload = self
                    .show_payload
                    .then(|| printable(str_of(&v, "data")))
                    .filter(|p| !p.is_empty());
                self.push_line(VaraMonLine {
                    at,
                    kind: VaraMonKind::Kiss,
                    text: format!("  KISS {len} B"),
                    snr_db: None,
                    payload,
                });
            }
            "unproto" => {
                let snr = f32_of(&v, "snr");
                self.line(at, VaraMonKind::Unproto, text, snr);
            }
            // The end-of-run totals: the counters here already say as much.
            "summary" => return false,
            _ if text.is_empty() => return false,
            _ => self.line(at, VaraMonKind::Info, text, None),
        }
        true
    }
}

/// The running monitor process and the channels to its threads.
struct Monitor {
    child: Child,
    audio: Option<Sender<Vec<i16>>>,
    out: Receiver<Out>,
}

impl Monitor {
    fn spawn(cfg: &VaraMonConfig, my_call: &str) -> Result<Self, String> {
        let exe = cfg.exe.trim();
        let mut cmd = Command::new(exe);
        cmd.args(monitor_args(cfg, my_call))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: a console program started from a GUI would
            // otherwise open a console window of its own.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start {exe}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no standard input")?;
        let stdout = child.stdout.take().ok_or("no standard output")?;
        let stderr = child.stderr.take().ok_or("no error output")?;

        let (audio_tx, audio_rx) = crossbeam_channel::bounded::<Vec<i16>>(QUEUE_BLOCKS);
        let (out_tx, out_rx) = crossbeam_channel::unbounded::<Out>();
        std::thread::Builder::new()
            .name("varamon-in".into())
            .spawn(move || write_audio(stdin, audio_rx))
            .map_err(|e| e.to_string())?;
        let tx = out_tx.clone();
        std::thread::Builder::new()
            .name("varamon-out".into())
            .spawn(move || read_lines(stdout, tx, Out::Line))
            .map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("varamon-err".into())
            .spawn(move || read_lines(stderr, out_tx, Out::Err))
            .map_err(|e| e.to_string())?;
        Ok(Monitor { child, audio: Some(audio_tx), out: out_rx })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // Closing standard input is the polite stop; the kill makes it certain,
        // so a mode change never leaves a monitor running behind it.
        self.audio = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_audio(mut stdin: ChildStdin, rx: Receiver<Vec<i16>>) {
    let mut bytes = Vec::with_capacity(BLOCK * 2);
    for block in rx {
        bytes.clear();
        for s in block {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if stdin.write_all(&bytes).is_err() {
            break;
        }
    }
}

fn read_lines(stream: impl std::io::Read, tx: Sender<Out>, wrap: fn(String) -> Out) {
    let mut r = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                if !line.is_empty() && tx.send(wrap(line)).is_err() {
                    break;
                }
            }
        }
    }
}

pub struct VaraMonController {
    cfg: DigiConfig,
    vm: VaraMonConfig,
    tap_rate: f64,
    resampler: Option<MonoResampler>,
    scratch: Vec<f32>,
    pending: Vec<i16>,
    monitor: Option<Monitor>,
    tracker: Tracker,
    stderr: Vec<String>,
    level: f32,
    status_dirty: bool,
    last_status: Option<SystemTime>,
}

impl VaraMonController {
    pub fn new(cfg: DigiConfig, vm: VaraMonConfig, tap_rate: f64) -> Self {
        let mut c = VaraMonController {
            cfg,
            tracker: Tracker::new(vm.show_payload),
            vm,
            tap_rate,
            resampler: MonoResampler::new(tap_rate, PIPE_RATE),
            scratch: Vec::new(),
            pending: Vec::with_capacity(BLOCK),
            monitor: None,
            stderr: Vec::new(),
            level: 0.0,
            status_dirty: true,
            last_status: None,
        };
        c.start();
        c
    }

    fn start(&mut self) {
        self.monitor = None;
        self.pending.clear();
        self.stderr.clear();
        self.resampler = MonoResampler::new(self.tap_rate, PIPE_RATE);
        self.tracker.status.version.clear();
        self.tracker.status.link = None;
        self.tracker.calls = Default::default();
        self.status_dirty = true;
        if self.vm.exe.trim().is_empty() {
            self.tracker.status.state = VaraMonState::NotConfigured;
            self.tracker.status.message.clear();
            return;
        }
        match Monitor::spawn(&self.vm, &self.cfg.my_call) {
            Ok(m) => {
                self.monitor = Some(m);
                self.tracker.status.state = VaraMonState::Starting;
                self.tracker.status.message.clear();
            }
            Err(e) => {
                tracing::warn!("VARA monitor: {e}");
                self.tracker.status.state = VaraMonState::Failed;
                self.tracker.status.message = e;
            }
        }
    }

    /// Drain the child's output and notice it stopping.
    fn pump(&mut self) {
        let Some(m) = &mut self.monitor else { return };
        let now = now_unix();
        let mut lines = Vec::new();
        while let Ok(o) = m.out.try_recv() {
            lines.push(o);
        }
        for o in lines {
            match o {
                Out::Line(l) => {
                    if self.tracker.ingest(&l, now) {
                        self.status_dirty = true;
                    }
                }
                Out::Err(l) => {
                    tracing::debug!("xara-monitor: {l}");
                    self.stderr.push(l);
                    let n = self.stderr.len();
                    if n > STDERR_KEEP {
                        self.stderr.drain(..n - STDERR_KEEP);
                    }
                }
            }
        }
        let Some(m) = &mut self.monitor else { return };
        if let Ok(Some(exit)) = m.child.try_wait() {
            // Whatever it printed last is usually the reason.
            let mut out = Vec::new();
            while let Ok(o) = m.out.try_recv() {
                out.push(o);
            }
            for o in out {
                match o {
                    Out::Line(l) => {
                        self.tracker.ingest(&l, now);
                    }
                    Out::Err(l) => self.stderr.push(l),
                }
            }
            let why = self
                .stderr
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty() && !l.starts_with("xara-monitor 0."))
                .cloned()
                .unwrap_or_default();
            let msg = if why.is_empty() {
                format!("xara-monitor stopped ({exit})")
            } else {
                format!("xara-monitor stopped ({exit}): {why}")
            };
            tracing::warn!("VARA monitor: {msg}");
            self.tracker.status.state = VaraMonState::Failed;
            self.tracker.status.message = msg;
            self.monitor = None;
            self.status_dirty = true;
        }
    }

    fn vara_status(&self) -> VaraMonStatus {
        let mut s = self.tracker.status.clone();
        s.level = self.level;
        s
    }

    fn digi_status(&self) -> DigiStatus {
        DigiStatus {
            mode: Mode::VaraMon,
            step: QsoStep::Idle,
            dx_call: None,
            dx_grid: None,
            tx_next: false,
            tx_pending_msg: None,
            audio_hz: 1500.0,
            tx_even: false,
            transmitting: false,
            tx_watchdog: false,
            tx_refused: None,
            transcript: Vec::<TranscriptLine>::new(),
            config: self.cfg.clone(),
            jtty: None,
            vara_mon: Some(self.vara_status()),
            text_rx: String::new(),
            tx_sent: 0,
            fsq_heard: Vec::new(),
            fsq_messages: Vec::new(),
            rade: None,
            packet: None,
            navtex: None,
            acars: None,
            aprs: None,
            js8: None,
            atchat: None,
            fox_queue: Vec::new(),
            call_queue: Vec::new(),
            clock_offset_s: None,
            cw: None,
            wspr: None,
            pi4: None,
            qso: None,
        }
    }
}

impl DigiEngine for VaraMonController {
    fn mode(&self) -> Mode {
        Mode::VaraMon
    }

    fn on_rx_audio(&mut self, tap: &[f32]) {
        if !tap.is_empty() {
            let rms = (tap.iter().map(|x| x * x).sum::<f32>() / tap.len() as f32).sqrt();
            self.level += (rms.min(1.0) - self.level) * 0.2;
        }
        let Some(m) = &self.monitor else { return };
        let Some(audio) = &m.audio else { return };
        let input: &[f32] = match &mut self.resampler {
            Some(r) => {
                self.scratch.clear();
                r.push(tap, &mut self.scratch);
                &self.scratch
            }
            None => tap,
        };
        for &x in input {
            self.pending.push((x.clamp(-1.0, 1.0) * 28_000.0) as i16);
            if self.pending.len() == BLOCK {
                let block = std::mem::replace(&mut self.pending, Vec::with_capacity(BLOCK));
                match audio.try_send(block) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        self.tracker.status.dropped_blocks += 1;
                        self.status_dirty = true;
                    }
                    // The writer is gone; `pump` will find out why.
                    Err(TrySendError::Disconnected(_)) => {}
                }
            }
        }
    }

    fn poll(&mut self, _now: SystemTime, _dial_hz: f64) -> Vec<DigiAction> {
        self.pump();
        let now = SystemTime::now();
        // On change, and a few times a second regardless, for the meter.
        let due = self
            .last_status
            .map(|t| now.duration_since(t).map(|d| d.as_secs_f32() > 0.25).unwrap_or(true))
            .unwrap_or(true);
        if !self.status_dirty && !due {
            return Vec::new();
        }
        self.status_dirty = false;
        self.last_status = Some(now);
        vec![DigiAction::Status(self.digi_status())]
    }

    fn tx_burst_active(&self) -> bool {
        false
    }

    fn fill_tx_block(&mut self, _out: &mut [f32]) -> bool {
        false
    }

    fn on_burst_done(&mut self) {}

    fn abort(&mut self) {
        self.status_dirty = true;
    }

    fn abort_tx(&mut self) {}

    fn set_config(&mut self, cfg: DigiConfig) {
        // The operator's own call is one of the names the monitor resolves, and
        // it only reads its list at start.
        let restart = cfg.my_call.trim() != self.cfg.my_call.trim();
        self.cfg = cfg;
        if restart && self.monitor.is_some() {
            self.start();
        }
        self.status_dirty = true;
    }

    fn set_vara_mon_config(&mut self, vm: VaraMonConfig) {
        let restart = vm != self.vm || self.monitor.is_none();
        self.tracker.show_payload = vm.show_payload;
        // Only display changed: no need to lose the monitor's learned calls.
        let display_only =
            VaraMonConfig { show_payload: self.vm.show_payload, ..vm.clone() } == self.vm;
        self.vm = vm;
        if restart && !(display_only && self.monitor.is_some()) {
            self.start();
        }
        self.status_dirty = true;
    }

    fn clear_rx(&mut self) {
        self.tracker.clear();
        self.status_dirty = true;
    }

    fn set_audio_hz(&mut self, _hz: f32) {}

    /// VARA HF is centred on 1500 Hz at every bandwidth.
    fn audio_hz(&self) -> f32 {
        1500.0
    }

    fn status(&self) -> DigiStatus {
        self.digi_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::VaraBandwidth;

    // Records as xara-monitor 0.1.3 prints them for a synthetic two-station
    // session (`--json`), lightly trimmed.
    const SESSION: &[&str] = &[
        r#"{"t":"start","version":"0.1.3","source":"stdin","bw":2300,"blind":true,"calls":["EA5XYZ"]}"#,
        r#"{"t":"ctrl","ts":3.8,"unix":1700000003.8,"text":"CALLER   c26 CONNECT            f0 31/31 SN +7.2 df -0.1  caller → EA5XYZ","sender":"caller","code":26,"code_name":"CONNECT","fmt":0,"hits":31,"of":31,"snr":7.17,"df":-0.1,"blind":false,"key_kind":"answerer","key":"8B4B9C","key_name":"EA5XYZ"}"#,
        r#"{"t":"ctrl","ts":4.91,"unix":1700000004.9,"text":"ANSWERER c18 CONNECT REPLY q4   f1 15/15 SN +29.7 df +0.0  EA5XYZ","sender":"answerer","code":18,"code_name":"CONNECT REPLY q4","fmt":1,"hits":15,"of":15,"snr":29.7,"df":0.0,"blind":false,"key_kind":"answerer","key":"8B4B9C","key_name":"EA5XYZ"}"#,
        r#"{"t":"learned","ts":9.39,"unix":1700000009.4,"text":"learned caller key EA5ABC (session header)","key_kind":"caller","key":"0ACAFF","key_name":"EA5ABC","origin":"session header","also":[]}"#,
        r#"{"t":"data","ts":9.39,"unix":1700000009.4,"text":"DATA     L4  x1 [+] SN +18.1 afc -0.0  caller → answerer","level":4,"frames":"+","sn":18.07,"afc":-0.03,"sender":"caller"}"#,
        r#"{"t":"header","ts":9.39,"unix":1700000009.4,"text":"  HEADER EA5ABC → EA5XYZ: flags 80","call":"EA5ABC","answerer":"EA5XYZ","flags":128,"registered":false,"compression":"off","encryption":0,"start":4,"marker_ok":true}"#,
        r#"{"t":"payload","ts":19.45,"unix":1700000019.5,"text":"  ctl 8a r2 h2 last n4: 44 B \"Hi EA5ABC\"","sender":"answerer","ctl":138,"reply":2,"hint":2,"compressed":false,"last":4,"full":false,"status":"data","len":44,"data":"Hi EA5ABC, this is EA5XYZ.\r\nCopy fine here.\r\n"}"#,
        r#"{"t":"some_future_record","ts":20.0,"text":"something new","whatever":[1,2,3]}"#,
        r#"{"t":"link_end","ts":61.4,"unix":1700000061.4,"text":"link EA5ABC → EA5XYZ ended (QRT) after 57.6 s: 600 B caller→answerer, 44 B answerer→caller","caller":"EA5ABC","answerer":"EA5XYZ","why":"QRT","secs":57.6,"bytes":[600,44,0]}"#,
        r#"{"t":"summary","ts":64.42,"text":"summary: 10 transmissions","tx":10}"#,
    ];

    fn run(show_payload: bool, upto: usize) -> Tracker {
        let mut t = Tracker::new(show_payload);
        for l in &SESSION[..upto] {
            t.ingest(l, 1_800_000_000);
        }
        t
    }

    #[test]
    fn a_session_names_both_stations_and_tracks_the_link() {
        let t = run(false, 7);
        assert_eq!(t.status.state, VaraMonState::Running);
        assert_eq!(t.status.version, "0.1.3");
        let link = t.status.link.as_ref().expect("link in progress");
        assert_eq!((link.caller.as_str(), link.answerer.as_str()), ("EA5ABC", "EA5XYZ"));
        assert_eq!(link.level, 4);
        assert_eq!(link.bytes, [0, 44]);
        assert_eq!(link.started_at, 1_700_000_003);
        let abc = t.status.stations.iter().find(|s| s.call == "EA5ABC").unwrap();
        assert_eq!(abc.peer, "EA5XYZ");
        assert_eq!(abc.top_level, 4);
        assert_eq!(abc.last_snr_db, Some(18.07));
        let xyz = t.status.stations.iter().find(|s| s.call == "EA5XYZ").unwrap();
        assert_eq!(xyz.best_snr_db, Some(29.7));
        assert_eq!(xyz.peer, "EA5ABC");
        assert_eq!(t.status.transmissions, 1);
    }

    #[test]
    fn a_link_end_closes_the_link_and_counts_it() {
        let t = run(false, SESSION.len());
        assert!(t.status.link.is_none());
        assert_eq!(t.status.links, 1);
        assert!(t.status.stations.iter().all(|s| s.links == 1));
        let last = t.status.lines.last().unwrap();
        assert_eq!(last.kind, VaraMonKind::LinkEnd);
        // The summary record adds nothing; the unknown record shows its text.
        assert!(t.status.lines.iter().any(|l| l.text == "something new"));
        assert!(!t.status.lines.iter().any(|l| l.text.starts_with("summary")));
    }

    #[test]
    fn payload_content_stays_hidden_unless_asked_for() {
        let hidden = run(false, 7);
        let p = hidden.status.lines.iter().find(|l| l.kind == VaraMonKind::Payload).unwrap();
        assert_eq!(p.text, "  EA5XYZ → EA5ABC: 44 B");
        assert!(p.payload.is_none());
        assert!(!hidden.status.lines.iter().any(|l| l.text.contains("Hi EA5ABC")));

        let shown = run(true, 7);
        let p = shown.status.lines.iter().find(|l| l.kind == VaraMonKind::Payload).unwrap();
        assert_eq!(p.payload.as_deref(), Some("Hi EA5ABC, this is EA5XYZ.\nCopy fine here.\n"));
    }

    #[test]
    fn junk_and_partial_lines_are_ignored() {
        let mut t = Tracker::new(false);
        assert!(!t.ingest("xara-monitor 0.1.3: 2300 Hz", 0));
        assert!(!t.ingest(r#"{"t":"ctrl","#, 0));
        assert!(!t.ingest("", 0));
        assert!(t.status.lines.is_empty());
    }

    #[test]
    fn an_older_monitor_is_flagged_not_refused() {
        let mut t = Tracker::new(false);
        t.ingest(r#"{"t":"start","version":"0.1.0"}"#, 0);
        assert_eq!(t.status.state, VaraMonState::Running);
        assert!(t.status.message.contains("0.1.0"));
        let mut t = Tracker::new(false);
        t.ingest(r#"{"t":"start","version":"0.2.0-dev"}"#, 0);
        assert!(t.status.message.is_empty());
    }

    #[test]
    fn printable_keeps_lines_and_masks_control_bytes() {
        assert_eq!(printable("a\r\nb\rc\x01d"), "a\nb\nc·d");
    }

    #[test]
    fn the_command_line_carries_the_settings() {
        let cfg = VaraMonConfig {
            exe: "/x/xara-monitor".into(),
            bandwidth: VaraBandwidth::Hz500,
            calls: "k1abc, EA5XYZ".into(),
            blind: false,
            show_payload: true,
            extra_args: "--key-idle 10".into(),
        };
        assert_eq!(
            monitor_args(&cfg, "ea5abc"),
            [
                "--stdin",
                "48000",
                "--json",
                "--hf",
                "500",
                "--call",
                "EA5ABC",
                "--call",
                "K1ABC",
                "--call",
                "EA5XYZ",
                "--no-blind",
                "--key-idle",
                "10"
            ]
        );
        // The operator's own call is not listed twice.
        let cfg = VaraMonConfig { calls: "EA5ABC".into(), ..Default::default() };
        assert_eq!(monitor_args(&cfg, "EA5ABC").iter().filter(|a| *a == "EA5ABC").count(), 1);
    }

    #[test]
    fn no_executable_means_not_configured_and_nothing_started() {
        let c = VaraMonController::new(DigiConfig::default(), VaraMonConfig::default(), 48_000.0);
        let st = c.status().vara_mon.unwrap();
        assert_eq!(st.state, VaraMonState::NotConfigured);
        assert!(c.monitor.is_none());
    }

    #[test]
    fn a_missing_executable_fails_with_a_reason() {
        let vm = VaraMonConfig { exe: "/nonexistent/xara-monitor".into(), ..Default::default() };
        let c = VaraMonController::new(DigiConfig::default(), vm, 48_000.0);
        let st = c.status().vara_mon.unwrap();
        assert_eq!(st.state, VaraMonState::Failed);
        assert!(st.message.contains("/nonexistent/xara-monitor"), "{}", st.message);
    }
}

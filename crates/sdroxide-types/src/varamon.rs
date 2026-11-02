//! The VARA-compatible HF monitor (`Mode::VaraMon`): settings and live status.
//!
//! sdroxide does not decode VARA itself. The operator points it at their own
//! copy of `xara-monitor` (the passive monitor from the MIT-licensed xara
//! project), and the digi controller runs that program as a child process,
//! piping it the receiver's audio and reading back the records it prints. What
//! the operator installs — which version, built how — is theirs to decide; this
//! side only speaks the program's command line and its JSON output.
//!
//! The settings are station-wide (`varamon.json` on the engine's machine), not
//! part of [`crate::DigiConfig`]: the path names a file on the machine with the
//! radio, and a profile switch must not forget where it is.

use serde::{Deserialize, Serialize};

/// Most log lines kept for the traffic pane. A busy gateway channel produces a
/// line every second or two while a link is up, and the pane is a rolling view.
pub const VARA_MON_LINES_MAX: usize = 400;

/// Most stations kept in the heard list.
pub const VARA_MON_STATIONS_MAX: usize = 200;

/// The VARA HF bandwidth to listen for. The monitor searches one at a time, so
/// this has to match the traffic on the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VaraBandwidth {
    Hz500,
    #[default]
    Hz2300,
    Hz2750,
}

impl VaraBandwidth {
    pub const ALL: [VaraBandwidth; 3] =
        [VaraBandwidth::Hz500, VaraBandwidth::Hz2300, VaraBandwidth::Hz2750];

    /// The figure `xara-monitor --hf` takes, and the one the operator knows the
    /// mode by.
    pub fn hz(self) -> u32 {
        match self {
            VaraBandwidth::Hz500 => 500,
            VaraBandwidth::Hz2300 => 2300,
            VaraBandwidth::Hz2750 => 2750,
        }
    }
}

/// Settings for the VARA-compatible monitor (`varamon.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaraMonConfig {
    /// Full path to the operator's `xara-monitor` executable, on the machine
    /// the radio is attached to. Empty until the operator sets it; nothing is
    /// started without it.
    pub exe: String,
    /// Which VARA HF bandwidth to search for.
    pub bandwidth: VaraBandwidth,
    /// Callsigns to resolve by name — the gateways and stations expected on
    /// this channel — separated by spaces, commas or new lines. VARA control
    /// frames carry a hash of the callsign, not the callsign, so a station that
    /// is neither listed here nor named in a session header shows as `#HASH`.
    /// The operator's own callsign is always added.
    pub calls: String,
    /// Let the monitor guess unknown callsigns from the hashes it hears
    /// (`xara-monitor`'s default). Off passes `--no-blind`, which costs less CPU
    /// and resolves only the listed calls.
    pub blind: bool,
    /// Show what the stations sent — the bytes inside each data frame. Off by
    /// default: the log then says how much was carried, not what it said.
    pub show_payload: bool,
    /// Further arguments appended to the command line as they are, split on
    /// whitespace. For options this panel does not know about.
    pub extra_args: String,
}

impl Default for VaraMonConfig {
    fn default() -> Self {
        VaraMonConfig {
            exe: String::new(),
            bandwidth: VaraBandwidth::Hz2300,
            calls: String::new(),
            blind: true,
            show_payload: false,
            extra_args: String::new(),
        }
    }
}

impl VaraMonConfig {
    /// The listed callsigns, upper-cased, deduplicated, in the order given.
    pub fn call_list(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in self.calls.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
            let c = c.trim().to_ascii_uppercase();
            if !c.is_empty() && !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }
}

/// Where the monitor process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VaraMonState {
    /// No executable set yet.
    #[default]
    NotConfigured,
    /// Started, and nothing heard back from it yet.
    Starting,
    /// Running and answering.
    Running,
    /// The program could not be started, or stopped on its own; `message` says
    /// why.
    Failed,
}

/// What kind of record a log line came from — for colouring and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VaraMonKind {
    /// A control frame: connect requests, acknowledgements, breaks, QRT.
    #[default]
    Control,
    /// A connect request in particular — where a link starts.
    Connect,
    /// A data transmission: speed level and signal-to-noise.
    Data,
    /// The session header: who is calling whom.
    Header,
    /// The bytes a data frame carried.
    Payload,
    /// A link has ended, with its totals.
    LinkEnd,
    /// A CQ frame.
    Cq,
    /// A KISS (broadcast) frame.
    Kiss,
    /// A frame outside any session.
    Unproto,
    /// The monitor's own bookkeeping: a callsign learned, a key dropped.
    Info,
}

/// One line of the traffic log.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VaraMonLine {
    /// Unix seconds.
    pub at: i64,
    pub kind: VaraMonKind,
    /// The line as the monitor wrote it (payload lines rewritten so they say
    /// nothing of the content unless the operator asked to see it).
    pub text: String,
    /// The signal-to-noise ratio this frame was heard at, where it says.
    pub snr_db: Option<f32>,
    /// What was carried, printable, when the operator asked to see it.
    pub payload: Option<String>,
}

/// A station the monitor has heard and could name.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VaraMonStation {
    pub call: String,
    /// Unix seconds.
    pub first_at: i64,
    pub last_at: i64,
    /// Frames attributed to this station.
    pub frames: u32,
    /// Signal-to-noise of the most recent frame, and the best one.
    pub last_snr_db: Option<f32>,
    pub best_snr_db: Option<f32>,
    /// The station it was last in a link with, if any.
    pub peer: String,
    /// Links this station took part in (counted when the link starts).
    pub links: u32,
    /// The highest VARA speed level heard *from* this station.
    pub top_level: u8,
    /// Heard calling CQ.
    pub cq: bool,
}

/// The link on the air now, as far as the monitor can tell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VaraMonLink {
    /// Callsigns, or `#HASH` while unresolved; empty while unknown.
    pub caller: String,
    pub answerer: String,
    /// Unix seconds.
    pub started_at: i64,
    /// The speed level of the latest data transmission.
    pub level: u8,
    /// Payload bytes seen so far, caller → answerer and back.
    pub bytes: [u64; 2],
}

/// Live status of the monitor, carried in [`crate::DigiStatus::vara_mon`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VaraMonStatus {
    pub state: VaraMonState,
    /// The monitor's own version, from its start record.
    pub version: String,
    /// Why it is not running, or the last thing it said on its error output.
    pub message: String,
    /// Smoothed receive audio level, for a meter.
    pub level: f32,
    /// Log lines, newest last.
    pub lines: Vec<VaraMonLine>,
    /// Stations heard, most recent first.
    pub stations: Vec<VaraMonStation>,
    /// The link in progress, if one is.
    pub link: Option<VaraMonLink>,
    /// Counters since the monitor started.
    pub transmissions: u64,
    pub links: u64,
    /// Audio blocks the monitor could not take in time and were thrown away —
    /// non-zero means the machine is not keeping up with it.
    pub dropped_blocks: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_list_splits_dedups_and_uppercases() {
        let cfg = VaraMonConfig {
            calls: "ea5abc, EA5XYZ-10;\n ea5abc  k1abc".into(),
            ..Default::default()
        };
        assert_eq!(cfg.call_list(), vec!["EA5ABC", "EA5XYZ-10", "K1ABC"]);
    }

    #[test]
    fn an_old_file_without_newer_fields_still_loads() {
        let cfg: VaraMonConfig =
            serde_json::from_str(r#"{"exe":"/usr/local/bin/xara-monitor"}"#).unwrap();
        assert_eq!(cfg.exe, "/usr/local/bin/xara-monitor");
        assert!(cfg.blind);
        assert!(!cfg.show_payload);
        assert_eq!(cfg.bandwidth, VaraBandwidth::Hz2300);
    }
}

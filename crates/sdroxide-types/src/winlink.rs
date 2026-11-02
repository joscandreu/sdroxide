//! Winlink radio-email vocabulary: the mailbox as the UI sees it, and the
//! account settings a session needs.
//!
//! The protocol itself lives in `sdroxide-winlink`, which is native-only. What
//! is here is only the shape of a mailbox and a session's status, because both
//! have to cross the wire to a browser client that links none of that code.
//!
//! The mailbox lives on the machine with the radio and is read **lazily**: a
//! listing carries metadata, and the body and attachments are fetched one
//! message at a time. That is the same arrangement the picture store uses, and
//! for the same reason — a mailbox with attachments in it is far too big to
//! mirror into every client on connect.

use serde::{Deserialize, Serialize};

/// Where a message sits in its life.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub enum MailFolder {
    /// Arrived from a forwarding partner.
    #[default]
    Inbox,
    /// Composed here, waiting for a session to carry it.
    Outbox,
    /// Handed to a forwarding partner and accepted.
    Sent,
    /// Kept, but out of the way.
    Archive,
}

impl MailFolder {
    pub const ALL: [MailFolder; 4] =
        [MailFolder::Inbox, MailFolder::Outbox, MailFolder::Sent, MailFolder::Archive];

    /// Directory name on disk. Also the stable key in the wire protocol, so it
    /// must not change once written.
    pub fn dir_name(self) -> &'static str {
        match self {
            MailFolder::Inbox => "inbox",
            MailFolder::Outbox => "outbox",
            MailFolder::Sent => "sent",
            MailFolder::Archive => "archive",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MailFolder::Inbox => "INBOX",
            MailFolder::Outbox => "OUTBOX",
            MailFolder::Sent => "SENT",
            MailFolder::Archive => "ARCHIVE",
        }
    }
}

/// Most entries one listing request may ask for, so a client cannot make the
/// server build an unbounded response.
pub const MAIL_PAGE_MAX: u32 = 200;

/// One message's metadata — enough to draw a list row without fetching it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailEntry {
    pub mid: String,
    /// `YYYY/MM/DD HH:MM`, UTC, as the message carries it.
    pub date: String,
    pub from: String,
    /// Recipients, already joined for display.
    pub to: String,
    pub subject: String,
    pub folder: MailFolder,
    /// Size of the stored message, so a list can say what a fetch will cost
    /// before pulling it down a phone link.
    pub bytes: u64,
    pub attachments: u32,
    /// Not yet opened. Only meaningful in the inbox.
    pub unread: bool,
}

/// One page of a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailListing {
    pub folder: MailFolder,
    /// Where `entries` starts within the folder.
    pub offset: u32,
    /// How many the folder holds altogether, so the UI can page exactly rather
    /// than guessing from what it was given.
    pub total: u32,
    pub entries: Vec<MailEntry>,
}

/// An attachment, with its bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailAttachment {
    pub name: String,
    pub data: Vec<u8>,
}

/// One whole message, answering a fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailMessage {
    pub mid: String,
    pub date: String,
    /// `Private` for ordinary mail; also `Service`, `Inquiry`, `Position`.
    pub msg_type: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<MailAttachment>,
    pub folder: MailFolder,
}

/// A message the operator has composed, on its way to the outbox.
///
/// Separate from [`MailMessage`] because the fields the operator fills in are
/// not the fields a stored message has: the MID and date are minted server-side
/// at the moment it is filed, and a client must not choose either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailDraft {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<MailAttachment>,
}

/// What a forwarding session is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WinlinkStatus {
    /// A session is running right now.
    pub busy: bool,
    /// One line on what it is doing, for the window header.
    pub activity: String,
    /// Why the last session ended badly, if it did.
    pub last_error: Option<String>,
    /// Unix seconds of the last session that completed cleanly.
    pub last_session: Option<i64>,
    /// Messages received and sent by the last session.
    pub last_received: u32,
    pub last_sent: u32,
    /// Per-folder counts, in [`MailFolder::ALL`] order, so the folder tabs can
    /// show totals without listing anything.
    pub counts: [u32; 4],
    /// The protocol transcript of the most recent session. Kept because a
    /// forwarding session that fails is unreadable without it.
    pub log: Vec<String>,
    /// The VARA TNC is listening for peer-to-peer calls right now.
    pub listening: bool,
    /// The TNC sdroxide runs itself, when it does.
    pub tnc: ManagedTncStatus,
    /// Keyboard chat over VARA.
    pub chat: VaraChatStatus,
}

/// The gateway the CMS telnet transport dials by default.
pub const DEFAULT_CMS_ADDRESS: &str = "server.winlink.org:8772";

/// Which lane a forwarding session should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WinlinkLane {
    /// The CMS over the internet.
    #[default]
    Telnet,
    /// An RMS gateway over the air, using the radio's current packet mode.
    Packet,
    /// An RMS gateway over the air through an external VARA-compatible TNC
    /// (xara, or VARA itself), driven over its TCP command and data ports.
    /// Appended for the usual reason — postcard numbers variants by position.
    Vara,
}

/// The TNC address assumed until the operator says otherwise: the command port
/// every VARA-compatible TNC listens on by default. The data port is one above.
pub const DEFAULT_VARA_TNC: &str = "127.0.0.1:8300";

/// The Winlink VARA lane: which TNC to drive, and whom to call through it.
///
/// sdroxide does not contain a VARA modem. It talks to one that is already
/// running — xara (MIT-licensed, Linux/macOS/FreeBSD) or VARA — over the TNC's
/// command and data ports, exactly as Pat or Winlink Express do. The audio
/// between the TNC and the radio is the operator's wiring (a virtual cable to
/// sdroxide's audio, or a sound card on a rig).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaraTncConfig {
    /// `host:port` of the TNC's command port. The data port is the next one.
    pub address: String,
    /// The RMS gateway (or station) to call, e.g. `EA5XYZ-10`.
    pub gateway: String,
    /// The VARA HF bandwidth to ask the TNC for. `None` leaves the TNC as it is
    /// set up — what VARA FM, which has no bandwidth command, needs.
    pub bandwidth: Option<crate::VaraBandwidth>,
    /// Where the gateway listens (the dial). Zero leaves the radio where it is.
    pub freq_hz: f64,
    /// Key sdroxide's own transmitter when the TNC says `PTT ON`, and unkey on
    /// `PTT OFF`. The TNC's transmit audio is then expected on sdroxide's
    /// microphone input (a virtual cable), and goes out without the voice
    /// processing a microphone gets. Off when the TNC keys a radio itself.
    pub key_ptt: bool,
    /// The station called is a peer — another operator's Winlink client — not
    /// an RMS gateway. The TNC is put in its peer-to-peer timing, and only
    /// mail addressed to that station is offered: a peer cannot forward to the
    /// internet the way a gateway does.
    pub p2p: bool,
    /// Answer peer-to-peer calls. While on, sdroxide keeps the TNC listening
    /// (`LISTEN ON`) whenever no session is running, takes a caller's mail and
    /// hands over whatever is waiting for that station.
    pub listen: bool,
    /// Before calling, wait up to this long for the channel to go quiet (the
    /// TNC's `BUSY OFF`), then give up rather than transmit over another link.
    /// Zero calls straight away.
    pub busy_wait_s: u32,
    /// The operator's own list of VARA gateways and peer stations.
    pub gateways: Vec<VaraGateway>,
    /// Have sdroxide run the TNC (xara) itself rather than expect one running.
    pub managed: ManagedTncConfig,
    /// Keyboard chat through the same TNC.
    pub chat: VaraChatConfig,
}

/// What goes inside a chat link's byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChatFormat {
    /// VarAC's messages, switching to plain text for a station that sends
    /// plain text (VARA Chat and terminal programs).
    #[default]
    Auto,
    /// VarAC's messages only.
    VarAc,
    /// Plain text lines only, as VARA Chat sends them.
    Plain,
}

/// Keyboard chat over VARA: standing by for calls and CQs, and how to talk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaraChatConfig {
    /// Stand by for chat: keep the TNC in chat mode and listening, show the CQ
    /// frames it hears, and answer stations that call. Takes the TNC instead
    /// of the Winlink peer-to-peer listener.
    pub enabled: bool,
    pub format: ChatFormat,
    /// Sent after the opening report to a VarAC station, as VarAC does.
    pub name: String,
    pub qth: String,
    pub send_profile: bool,
}

impl Default for VaraChatConfig {
    fn default() -> Self {
        VaraChatConfig {
            enabled: false,
            format: ChatFormat::Auto,
            name: String::new(),
            qth: String::new(),
            send_profile: true,
        }
    }
}

/// Where the chat station is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChatState {
    #[default]
    Off,
    /// On the TNC, listening; no QSO.
    Standby,
    /// Calling a station.
    Calling,
    /// In a QSO.
    Connected,
    /// Saying goodbye: `<DISC>` sent, the link not yet closed.
    Closing,
}

/// Who a line of the conversation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChatLineKind {
    /// Typed here.
    Me,
    /// The other station.
    #[default]
    Them,
    /// About the link or the QSO rather than said: connected, reports, CQs.
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatLine {
    /// Unix seconds.
    pub at: i64,
    pub kind: ChatLineKind,
    pub text: String,
}

/// A CQ frame heard while standing by.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatCq {
    /// The callsign as sent, and the one to call (a VarAC slot suffix removed).
    pub raw: String,
    pub call: String,
    /// Where the caller waits, from the dial it was heard on.
    pub offset_hz: i32,
    /// A VarAC beacon rather than a CQ.
    pub beacon: bool,
    /// The bandwidth it was sent at, as the TNC reported it.
    pub bw: String,
    /// Unix seconds.
    pub at: i64,
}

/// Most chat lines kept; older ones scroll away.
pub const CHAT_LINES_MAX: usize = 500;
/// Most heard CQs kept.
pub const CHAT_CQ_MAX: usize = 30;

/// The chat station's state and conversation, carried in [`WinlinkStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VaraChatStatus {
    pub state: ChatState,
    /// The station in the QSO (or being called).
    pub peer: String,
    /// The other station sends plain text rather than VarAC's messages.
    pub plain: bool,
    pub lines: Vec<ChatLine>,
    /// CQs heard, newest first.
    pub heard: Vec<ChatCq>,
    /// Why the last thing asked for did not happen.
    pub message: String,
}

/// Which audio system the managed TNC opens its devices through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TncAudio {
    /// The TNC's own default for the platform (CoreAudio on macOS, PulseAudio
    /// elsewhere, for xara).
    #[default]
    Default,
    Pulse,
    Alsa,
    CoreAudio,
}

impl TncAudio {
    pub const ALL: [TncAudio; 4] =
        [TncAudio::Default, TncAudio::Pulse, TncAudio::Alsa, TncAudio::CoreAudio];

    /// The `--audio` value, or `None` for the TNC's default.
    pub fn arg(self) -> Option<&'static str> {
        match self {
            TncAudio::Default => None,
            TncAudio::Pulse => Some("pulse"),
            TncAudio::Alsa => Some("alsa"),
            TncAudio::CoreAudio => Some("coreaudio"),
        }
    }
}

/// What the managed TNC runs: VARA HF at a bandwidth, or VARA FM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TncMode {
    Hf500,
    #[default]
    Hf2300,
    Hf2750,
    FmNarrow,
    FmWide,
}

impl TncMode {
    pub const ALL: [TncMode; 5] =
        [TncMode::Hf500, TncMode::Hf2300, TncMode::Hf2750, TncMode::FmNarrow, TncMode::FmWide];

    /// The command-line pair that selects it: `--hf 2300`, `--fm narrow`.
    pub fn args(self) -> [&'static str; 2] {
        match self {
            TncMode::Hf500 => ["--hf", "500"],
            TncMode::Hf2300 => ["--hf", "2300"],
            TncMode::Hf2750 => ["--hf", "2750"],
            TncMode::FmNarrow => ["--fm", "narrow"],
            TncMode::FmWide => ["--fm", "wide"],
        }
    }

    pub fn is_fm(self) -> bool {
        matches!(self, TncMode::FmNarrow | TncMode::FmWide)
    }

    pub fn label(self) -> &'static str {
        match self {
            TncMode::Hf500 => "HF 500",
            TncMode::Hf2300 => "HF 2300",
            TncMode::Hf2750 => "HF 2750",
            TncMode::FmNarrow => "FM narrow",
            TncMode::FmWide => "FM wide",
        }
    }
}

/// How the managed TNC keys the transmitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TncPtt {
    /// The TNC says `PTT ON`/`PTT OFF` and sdroxide's Winlink session or
    /// listener keys the radio (`--ptt vox` on the TNC). Only while sdroxide
    /// itself is the TNC's host.
    #[default]
    Notices,
    /// The TNC keys sdroxide through its built-in rigctld server, so any
    /// program using the TNC — Pat, LinBPQ, VarAC — transmits too.
    Rigctld,
}

/// sdroxide running the VARA TNC (xara) as a child process.
///
/// The program is the operator's own build: sdroxide only starts it with the
/// command line these settings describe, watches it, and restarts it if it
/// stops. It listens on the VARA route's address (which must be this machine).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedTncConfig {
    pub enabled: bool,
    /// Full path to `xara` on the machine with the radio.
    pub exe: String,
    pub audio: TncAudio,
    /// What the TNC hears: the virtual cable sdroxide's audio output feeds.
    pub capture: String,
    /// What the TNC sends: the virtual cable sdroxide's microphone input reads.
    pub playback: String,
    pub mode: TncMode,
    pub ptt: TncPtt,
    /// PTT lead: how long the transmitter has to come up before the frame.
    pub ptt_lead_ms: u32,
    /// The TNC's transmit drive, dB (≤ 0).
    pub drive_db: f32,
    /// Also serve KISS on this port; 0 leaves it off.
    pub kiss_port: u16,
    /// Further arguments, appended as they are.
    pub extra_args: String,
}

impl Default for ManagedTncConfig {
    fn default() -> Self {
        ManagedTncConfig {
            enabled: false,
            exe: String::new(),
            audio: TncAudio::Default,
            capture: String::new(),
            playback: String::new(),
            mode: TncMode::Hf2300,
            ptt: TncPtt::Notices,
            ptt_lead_ms: 50,
            drive_db: -5.0,
            kiss_port: 0,
            extra_args: String::new(),
        }
    }
}

/// Where the managed TNC is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TncState {
    /// Not enabled.
    #[default]
    Off,
    /// Started; waiting for it to say its ports are open.
    Starting,
    Running,
    /// Could not start, or stopped; `message` says why. Retried after a pause
    /// unless the settings are what is wrong.
    Failed,
}

/// One audio device the TNC can open, as it lists them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TncDevice {
    /// What goes on the command line.
    pub id: String,
    /// What it is called, when that differs from the id.
    pub desc: String,
}

/// The managed TNC's state, for the settings and the MAIL window.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedTncStatus {
    pub state: TncState,
    /// From its start-up line, e.g. `0.1.3`.
    pub version: String,
    /// Why it is not running, or the last thing it said.
    pub message: String,
    /// The devices it can open, from `--list-devices`, for the pickers.
    pub capture_devices: Vec<TncDevice>,
    pub playback_devices: Vec<TncDevice>,
}

/// A VARA gateway or peer station the operator has written down.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaraGateway {
    /// Callsign with SSID, e.g. `EA5XYZ-10`.
    pub callsign: String,
    /// The bandwidth it runs; `None` leaves the TNC's own (VARA FM).
    pub bandwidth: Option<crate::VaraBandwidth>,
    /// Where it listens. Zero leaves the dial alone.
    pub freq_hz: f64,
    /// A peer station rather than an RMS gateway.
    pub p2p: bool,
    /// Free text — "Valencia, 24/7".
    pub label: String,
}

impl VaraTncConfig {
    /// Make the `i`th saved entry the one the next connect calls — every field
    /// it knows about itself, not just the name. Out of range does nothing.
    pub fn select_gateway(&mut self, i: usize) {
        let Some(g) = self.gateways.get(i).cloned() else { return };
        self.gateway = g.callsign;
        self.bandwidth = g.bandwidth;
        self.freq_hz = g.freq_hz;
        self.p2p = g.p2p;
    }

    /// The entry the fields above describe, for saving to the list.
    pub fn current_entry(&self) -> VaraGateway {
        VaraGateway {
            callsign: self.gateway.trim().to_uppercase(),
            bandwidth: self.bandwidth,
            freq_hz: self.freq_hz,
            p2p: self.p2p,
            label: String::new(),
        }
    }
}

impl Default for VaraTncConfig {
    fn default() -> Self {
        VaraTncConfig {
            address: DEFAULT_VARA_TNC.to_string(),
            gateway: String::new(),
            bandwidth: Some(crate::VaraBandwidth::Hz2300),
            freq_hz: 0.0,
            key_ptt: true,
            p2p: false,
            listen: false,
            busy_wait_s: 30,
            gateways: Vec::new(),
            managed: ManagedTncConfig::default(),
            chat: VaraChatConfig::default(),
        }
    }
}

/// One RMS gateway the operator has written down.
///
/// Winlink publishes the real gateway list through `api.winlink.org`'s
/// `ChannelList`, which refuses every request without an access key we do not
/// have. So the operator keeps their own list, which is what a packet operator
/// does anyway: the two or three gateways actually reachable from a given
/// location are learned by trying, and they do not change often.
///
/// When a key does arrive, the fetched list populates exactly this structure —
/// nothing above it needs to know where the entries came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WinlinkGateway {
    /// The gateway's callsign, including SSID — `OE1XAR-10`.
    pub callsign: String,
    /// Digipeaters to reach it through, in order. Usually empty.
    pub via: Vec<String>,
    /// Where to tune. Zero means "wherever the radio already is", which is the
    /// right answer for an operator who parks on one channel.
    pub freq_hz: f64,
    /// Speed this gateway runs. Selecting the gateway sets the modem to match,
    /// because calling a 1200-baud gateway at 9600 produces silence and no
    /// explanation.
    pub baud: crate::PacketBaud,
    /// Free text for the operator — "Vienna, 24/7" — shown in the picker.
    pub label: String,
}

/// Account settings for Winlink.
///
/// The password is the Winlink *account* password, which answers the secure
/// login challenge. It is stored in plaintext alongside the other service
/// credentials in [`crate::NetworkConfig`] — the same posture as QRZ, LoTW and
/// Club Log, and worth knowing rather than assuming otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WinlinkConfig {
    /// The account callsign. Upper-cased on use.
    pub callsign: String,
    /// Account password. **Case-sensitive** — the challenge response differs
    /// for `FOOBAR` and `FooBar`, so it must be entered exactly as issued.
    pub password: String,
    /// Maidenhead locator reported in the session greeting. Cosmetic.
    pub locator: String,
    /// Which CMS to dial. Overridable mainly for testing.
    pub cms_address: String,
    /// The client name announced in our SID.
    ///
    /// Winlink's production CMS refuses names it does not know, answering
    /// `*** Unknown client types are not allowed on production servers`. Until
    /// "sdroxide" is registered with the Winlink Development Team this is the
    /// knob that lets an operator get in, and it is deliberately visible in
    /// settings rather than silently defaulted to somebody else's client name.
    pub app_name: String,
    /// Which lane [`crate::Command::WinlinkConnect`] uses by default.
    #[serde(default)]
    pub lane: WinlinkLane,
    /// The RMS gateway to call over the air, e.g. `OE1XAR-10`.
    #[serde(default)]
    pub gateway: String,
    /// Digipeaters to route through, in order. Usually empty.
    #[serde(default)]
    pub gateway_via: Vec<String>,
    /// The speed the gateway named above runs.
    ///
    /// Held here rather than read from the modem because it is a property of
    /// *that gateway*, not of the band or of the operator's last session: a
    /// 2 m RMS may be 1200 or 9600, and the only way to know is to have been
    /// told. Applied to the modem when the packet lane connects.
    #[serde(default)]
    pub gateway_baud: crate::PacketBaud,
    /// Where the gateway listens. Zero means "wherever the radio already is",
    /// which is the right answer for an operator who parks on one channel.
    #[serde(default)]
    pub gateway_freq_hz: f64,
    /// The operator's own gateway list, since the published one needs an API
    /// key. Picking one fills in `gateway`, `gateway_via`, `gateway_baud` and
    /// `gateway_freq_hz`.
    #[serde(default)]
    pub gateways: Vec<WinlinkGateway>,
    /// Connect automatically on a timer.
    pub auto_connect: bool,
    /// How often to connect when `auto_connect` is on.
    pub auto_connect_minutes: u32,
    /// The VARA lane's TNC and gateway. Appended for the usual reason.
    #[serde(default)]
    pub vara: VaraTncConfig,
}

impl Default for WinlinkConfig {
    fn default() -> Self {
        WinlinkConfig {
            callsign: String::new(),
            password: String::new(),
            locator: String::new(),
            cms_address: DEFAULT_CMS_ADDRESS.to_string(),
            app_name: "sdroxide".to_string(),
            lane: WinlinkLane::default(),
            gateway: String::new(),
            gateway_via: Vec::new(),
            gateway_baud: crate::PacketBaud::default(),
            gateway_freq_hz: 0.0,
            gateways: Vec::new(),
            auto_connect: false,
            auto_connect_minutes: 30,
            vara: VaraTncConfig::default(),
        }
    }
}

impl WinlinkConfig {
    /// Whether a session could even be attempted.
    pub fn is_usable(&self) -> bool {
        !self.callsign.trim().is_empty() && !self.password.is_empty()
    }

    /// Make the `i`th saved gateway the one the next connect calls.
    ///
    /// Every field the gateway knows about itself moves, not just its name.
    /// This lives here rather than in the settings panel because copying only
    /// the callsign is a silent bug — the session then runs at whatever speed
    /// the modem happened to be on, and calling a 1200-baud gateway at 9600
    /// is indistinguishable from calling one that is off the air.
    ///
    /// Out-of-range indices are ignored, so a list edited concurrently by a
    /// second client cannot panic the one that was looking at it.
    pub fn select_gateway(&mut self, i: usize) {
        let Some(g) = self.gateways.get(i).cloned() else { return };
        self.gateway = g.callsign;
        self.gateway_via = g.via;
        self.gateway_baud = g.baud;
        self.gateway_freq_hz = g.freq_hz;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_dir_names_are_stable() {
        // These name directories on disk and ride the wire; changing one
        // orphans an operator's stored mail.
        let names: Vec<&str> = MailFolder::ALL.iter().map(|f| f.dir_name()).collect();
        assert_eq!(names, ["inbox", "outbox", "sent", "archive"]);
    }

    #[test]
    fn counts_line_up_with_folders() {
        let status = WinlinkStatus::default();
        assert_eq!(status.counts.len(), MailFolder::ALL.len());
    }

    #[test]
    fn a_config_saved_before_the_vara_lane_still_loads() {
        let cfg: WinlinkConfig =
            serde_json::from_str(r#"{"callsign":"EA5ABC","lane":"Packet"}"#).unwrap();
        assert_eq!(cfg.lane, WinlinkLane::Packet);
        assert_eq!(cfg.vara.address, DEFAULT_VARA_TNC);
        assert!(cfg.vara.key_ptt);
        let cfg: WinlinkConfig = serde_json::from_str(r#"{"lane":"Vara"}"#).unwrap();
        assert_eq!(cfg.lane, WinlinkLane::Vara);
    }

    #[test]
    fn selecting_a_vara_entry_takes_all_it_knows() {
        let mut v = VaraTncConfig {
            gateways: vec![
                VaraGateway {
                    callsign: "EA5XYZ-10".into(),
                    bandwidth: Some(crate::VaraBandwidth::Hz2750),
                    freq_hz: 14_105_000.0,
                    p2p: false,
                    label: "RMS".into(),
                },
                VaraGateway { callsign: "EA5QQQ".into(), p2p: true, ..Default::default() },
            ],
            ..Default::default()
        };
        v.select_gateway(0);
        assert_eq!(v.gateway, "EA5XYZ-10");
        assert_eq!(v.bandwidth, Some(crate::VaraBandwidth::Hz2750));
        assert!(!v.p2p);
        v.select_gateway(1);
        assert_eq!(v.gateway, "EA5QQQ");
        assert_eq!((v.bandwidth, v.freq_hz, v.p2p), (None, 0.0, true));
        let before = v.clone();
        v.select_gateway(9);
        assert_eq!(v, before);
    }

    #[test]
    fn default_config_is_not_usable() {
        // Nothing should try to connect until the operator has filled it in.
        assert!(!WinlinkConfig::default().is_usable());
        let cfg =
            WinlinkConfig { callsign: "OE3JJS".into(), password: "x".into(), ..Default::default() };
        assert!(cfg.is_usable());
        // Whitespace is not a callsign.
        let blank =
            WinlinkConfig { callsign: "   ".into(), password: "x".into(), ..Default::default() };
        assert!(!blank.is_usable());
    }

    fn two_gateways() -> WinlinkConfig {
        WinlinkConfig {
            gateways: vec![
                WinlinkGateway {
                    callsign: "OE1XAR-10".into(),
                    via: vec!["OE3XLR-1".into()],
                    freq_hz: 144_800_000.0,
                    baud: crate::PacketBaud::Vhf1200,
                    label: "Vienna".into(),
                },
                WinlinkGateway {
                    callsign: "DB0FHN-10".into(),
                    via: Vec::new(),
                    freq_hz: 438_425_000.0,
                    baud: crate::PacketBaud::Vhf9600,
                    label: String::new(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn selecting_a_gateway_takes_its_speed_and_channel() {
        // The whole point: a 9600 gateway must arrive as 9600. Copying only
        // the callsign left the modem at whatever it was, and the session
        // then failed with no evidence of why.
        let mut cfg = two_gateways();
        cfg.select_gateway(1);
        assert_eq!(cfg.gateway, "DB0FHN-10");
        assert_eq!(cfg.gateway_baud, crate::PacketBaud::Vhf9600);
        assert_eq!(cfg.gateway_freq_hz, 438_425_000.0);
        assert!(cfg.gateway_via.is_empty());
    }

    #[test]
    fn selecting_another_gateway_clears_the_previous_path() {
        // Switching from a digipeated gateway to a direct one must not leave
        // the old path behind: the call would go out via a digipeater the new
        // gateway has never heard of.
        let mut cfg = two_gateways();
        cfg.select_gateway(0);
        assert_eq!(cfg.gateway_via, ["OE3XLR-1"]);
        cfg.select_gateway(1);
        assert!(cfg.gateway_via.is_empty(), "the Vienna digipeater followed us to Franconia");
        assert_eq!(cfg.gateway_baud, crate::PacketBaud::Vhf9600);
    }

    #[test]
    fn selecting_a_gateway_that_is_not_there_changes_nothing() {
        let mut cfg = two_gateways();
        cfg.select_gateway(0);
        let before = cfg.clone();
        cfg.select_gateway(7);
        assert_eq!(cfg, before);
    }
}

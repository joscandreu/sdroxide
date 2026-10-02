use serde::{Deserialize, Serialize};

/// WSJT-X UDP broadcast configuration (`wsjtx.json`).
///
/// This is sdroxide *being* WSJT-X for the logging ecosystem: GridTracker,
/// JTAlert, N1MM+ and Log4OM all learn about decodes and contacts from the
/// datagrams WSJT-X sends to UDP 2237. It complements [`crate::RigctldConfig`]
/// and [`crate::TciServerConfig`], which offer control surfaces.
///
/// The clients can talk back — Reply, Halt Tx, Free Text, Replay, Highlight
/// Callsign — and a Reply keys the transmitter, so everything but Halt Tx is
/// ignored unless [`WsjtxConfig::accept_control`] says otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WsjtxConfig {
    /// Off by default: broadcasting where the station is and who it works is
    /// the operator's decision, even on the loopback interface.
    pub enabled: bool,
    /// Where to send. `127.0.0.1` reaches clients on this machine; a LAN
    /// address or a multicast group (`224.0.0.1`) reaches others.
    pub host: String,
    /// 2237 is the port every client defaults to.
    pub port: u16,
    /// The name clients see. Some loggers only accept traffic identifying
    /// itself as `WSJT-X`, which is why that — and not `sdroxide` — is the
    /// default.
    pub id: String,
    /// The N1MM+ contactinfo broadcast, which is a second dialect of the same
    /// idea (issue #337). Carried here rather than in a file of its own: it is
    /// the same setting — "tell my loggers what I worked" — and one page, one
    /// file and one command is what that should cost. Appended, as the wire
    /// requires.
    #[serde(default)]
    pub n1mm: N1mmConfig,
    /// Act on what the clients send back: answer a decode they Reply to, send
    /// their Free Text, Replay the decodes, colour the callsigns they
    /// highlight. Off by default — a Reply starts a transmission, and
    /// switching the broadcast on was never a promise to let anything on the
    /// port key the radio. Halt Tx is honoured whatever this says: it can only
    /// stop a transmission. Appended, as the wire requires.
    #[serde(default)]
    pub accept_control: bool,
}

impl Default for WsjtxConfig {
    fn default() -> Self {
        WsjtxConfig {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 2237,
            id: "WSJT-X".into(),
            n1mm: N1mmConfig::default(),
            accept_control: false,
        }
    }
}

impl WsjtxConfig {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// N1MM+ "contactinfo" UDP broadcast configuration, carried inside
/// [`WsjtxConfig`]'s file (issue #337).
///
/// A second dialect for the same purpose: a logger that does not speak WSJT-X's
/// protocol may well speak N1MM's, and the World Radio League's own desktop
/// bridge listens for both. N1MM sends one XML datagram per logged contact —
/// there is no decode stream and no status, so this is the logging half alone.
///
/// Its own destination rather than the WSJT-X one, because they are different
/// programs on different ports: 12060 is what N1MM's documentation recommends,
/// where WSJT-X's clients sit on 2237.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct N1mmConfig {
    /// Off by default, for the reason [`WsjtxConfig::enabled`] gives.
    pub enabled: bool,
    /// Where to send. N1MM's own documentation suggests `127.0.0.1` for this
    /// machine and a subnet broadcast (`192.168.1.255`) for the rest of a
    /// contest network.
    pub host: String,
    /// 12060, the port N1MM's documentation recommends.
    pub port: u16,
    /// What N1MM calls the `StationName`: the name of the computer that sent
    /// the packet. Loggers show it to tell one position of a multi-operator
    /// station from another.
    pub station: String,
}

impl Default for N1mmConfig {
    fn default() -> Self {
        N1mmConfig {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 12_060,
            station: "SDROXIDE".into(),
        }
    }
}

impl N1mmConfig {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A callsign a WSJT-X client asked to have coloured in the decode list
/// (its Highlight Callsign message) — how JTAlert and GridTracker mark the
/// stations they consider wanted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsjtxHighlight {
    /// Uppercased.
    pub call: String,
    /// Row background, as RGB. `None` leaves the row's own colour.
    pub bg: Option<[u8; 3]>,
    /// Callsign colour, as RGB. `None` leaves the row's own colour.
    pub fg: Option<[u8; 3]>,
    /// Colour only the newest decode from this station, not every one.
    pub last_only: bool,
}

/// The colours to draw a decode row from `call` with, if a client has
/// highlighted it. `newest` is whether this row is the newest one from that
/// station, which is all a `last_only` highlight colours.
pub fn wsjtx_highlight_for<'a>(
    highlights: &'a [WsjtxHighlight],
    call: &str,
    newest: bool,
) -> Option<&'a WsjtxHighlight> {
    let call = call.trim_start_matches('<').trim_end_matches('>');
    highlights.iter().find(|h| h.call.eq_ignore_ascii_case(call)).filter(|h| newest || !h.last_only)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hl(call: &str, last_only: bool) -> WsjtxHighlight {
        WsjtxHighlight { call: call.into(), bg: Some([255, 0, 0]), fg: None, last_only }
    }

    #[test]
    fn a_highlight_colours_every_row_from_the_station() {
        let list = [hl("W9XYZ", false)];
        assert!(wsjtx_highlight_for(&list, "W9XYZ", true).is_some());
        assert!(wsjtx_highlight_for(&list, "w9xyz", false).is_some(), "case does not matter");
        assert!(
            wsjtx_highlight_for(&list, "<W9XYZ>", false).is_some(),
            "a hashed call is the call"
        );
        assert!(wsjtx_highlight_for(&list, "K1ABC", true).is_none());
    }

    #[test]
    fn last_only_colours_the_newest_row_alone() {
        let list = [hl("W9XYZ", true)];
        assert!(wsjtx_highlight_for(&list, "W9XYZ", true).is_some());
        assert!(wsjtx_highlight_for(&list, "W9XYZ", false).is_none());
    }

    #[test]
    fn an_empty_list_colours_nothing() {
        assert!(wsjtx_highlight_for(&[], "W9XYZ", true).is_none());
    }

    #[test]
    fn control_is_off_unless_asked_for() {
        assert!(!WsjtxConfig::default().accept_control);
        let old: WsjtxConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(!old.accept_control, "a wsjtx.json from before the setting keeps control off");
    }
}

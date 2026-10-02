//! The WSJT-X datagram formats, as pure byte builders.
//!
//! Every message opens with the same header — magic, schema, type, and the
//! sending station's id — and continues with that type's fields in Qt
//! `QDataStream` encoding, big-endian throughout:
//!
//! | Qt type      | on the wire                                              |
//! |--------------|----------------------------------------------------------|
//! | `QString`    | `u32` byte count then UTF-8; `0xFFFFFFFF` means null      |
//! | `bool`       | one byte                                                  |
//! | `QTime`      | `u32` milliseconds since midnight                         |
//! | `QDateTime`  | `i64` Julian day, `u32` ms, `u8` time spec (1 = UTC)      |
//! | `double`     | 8 bytes IEEE-754 (Qt streams floats at double precision)  |
//! | `QColor`     | `i8` spec, then `u16` alpha, red, green, blue, padding    |
//!
//! Most of the traffic goes out: decodes, status, logged contacts. A client
//! talks back with a handful of its own — Reply, Halt Tx, Free Text, Replay,
//! Highlight Callsign — and [`parse`] reads those. What they are allowed to *do*
//! is decided in [`crate::control`], not here.

use sdroxide_types::QsoRecord;

/// Marks a datagram as this protocol.
const MAGIC: u32 = 0xadbc_cbda;
/// Schema WSJT-X 2.x speaks, and the one every current client expects.
const SCHEMA: u32 = 3;

/// Julian day number of 1970-01-01, for `QDate`.
const UNIX_EPOCH_JD: i64 = 2_440_588;

// Message type numbers, from `NetworkMessage.hpp`.
const T_HEARTBEAT: u32 = 0;
const T_STATUS: u32 = 1;
const T_DECODE: u32 = 2;
const T_CLEAR: u32 = 3;
const T_REPLY: u32 = 4;
const T_QSO_LOGGED: u32 = 5;
const T_CLOSE: u32 = 6;
const T_REPLAY: u32 = 7;
const T_HALT_TX: u32 = 8;
const T_FREE_TEXT: u32 = 9;
const T_LOCATION: u32 = 11;
const T_LOGGED_ADIF: u32 = 12;
const T_HIGHLIGHT: u32 = 13;
const T_SWITCH_CONFIG: u32 = 14;
const T_CONFIGURE: u32 = 15;

/// One decoded message, as the clients' decode window shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodeInfo {
    /// False replays an old decode (clients may re-sort rather than alert).
    pub new: bool,
    /// Unix seconds of the slot the message was decoded from.
    pub slot_utc: i64,
    pub snr_db: i32,
    /// Time offset from the slot start, in seconds.
    pub dt: f64,
    /// Audio offset within the passband, in Hz.
    pub audio_hz: u32,
    pub mode: String,
    pub message: String,
}

/// Where this station is and what it's doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusInfo {
    pub dial_hz: u64,
    pub mode: String,
    pub dx_call: String,
    /// The report we're sending them, as a bare number ("-13").
    pub report: String,
    pub tx_enabled: bool,
    pub transmitting: bool,
    pub decoding: bool,
    pub rx_df_hz: u32,
    pub tx_df_hz: u32,
    pub de_call: String,
    pub de_grid: String,
    pub dx_grid: String,
    pub tx_watchdog: bool,
    /// Transmit/receive period in seconds (FT8 15, FT4 7.5 → 7).
    pub tr_period_s: u32,
    /// The message queued for the next transmission.
    pub tx_message: String,
}

// ── QDataStream primitives ──────────────────────────────────────────────────

fn u8_(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn u32_(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn i32_(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn u64_(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn i64_(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn f64_(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn bool_(out: &mut Vec<u8>, v: bool) {
    out.push(u8::from(v));
}

/// A `QString`. An empty string is sent as a *null* string (`0xFFFFFFFF`),
/// which is how WSJT-X sends its own unset fields — clients test for null.
fn str_(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        u32_(out, 0xFFFF_FFFF);
        return;
    }
    u32_(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

/// Milliseconds since midnight UTC of the given instant — a `QTime` as the
/// wire carries it. Public because a client's Reply names a decode by exactly
/// this figure, and matching it against anything else would miss.
pub fn time_ms(unix: i64) -> u32 {
    (unix.rem_euclid(86_400) * 1000) as u32
}

/// A `QTime`: milliseconds since midnight UTC of the given instant.
fn time_(out: &mut Vec<u8>, unix: i64) {
    u32_(out, time_ms(unix));
}

/// A `QDateTime` in UTC.
fn datetime_(out: &mut Vec<u8>, unix: i64) {
    i64_(out, unix.div_euclid(86_400) + UNIX_EPOCH_JD);
    u32_(out, time_ms(unix));
    u8_(out, 1); // Qt::UTC
}

/// Magic, schema, message type, and the sending station's id.
fn header(out: &mut Vec<u8>, kind: u32, id: &str) {
    u32_(out, MAGIC);
    u32_(out, SCHEMA);
    u32_(out, kind);
    str_(out, id);
}

// ── Messages ────────────────────────────────────────────────────────────────

pub fn heartbeat(id: &str, version: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    header(&mut out, T_HEARTBEAT, id);
    u32_(&mut out, SCHEMA); // maximum schema we understand
    str_(&mut out, version);
    str_(&mut out, ""); // revision
    out
}

pub fn decode(id: &str, d: &DecodeInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    header(&mut out, T_DECODE, id);
    bool_(&mut out, d.new);
    time_(&mut out, d.slot_utc);
    i32_(&mut out, d.snr_db);
    f64_(&mut out, d.dt);
    u32_(&mut out, d.audio_hz);
    str_(&mut out, &d.mode);
    str_(&mut out, &d.message);
    bool_(&mut out, false); // low confidence
    bool_(&mut out, false); // off air
    out
}

pub fn status(id: &str, s: &StatusInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(160);
    header(&mut out, T_STATUS, id);
    u64_(&mut out, s.dial_hz);
    str_(&mut out, &s.mode);
    str_(&mut out, &s.dx_call);
    str_(&mut out, &s.report);
    str_(&mut out, &s.mode); // tx mode (never differs from the rx mode here)
    bool_(&mut out, s.tx_enabled);
    bool_(&mut out, s.transmitting);
    bool_(&mut out, s.decoding);
    u32_(&mut out, s.rx_df_hz);
    u32_(&mut out, s.tx_df_hz);
    str_(&mut out, &s.de_call);
    str_(&mut out, &s.de_grid);
    str_(&mut out, &s.dx_grid);
    bool_(&mut out, s.tx_watchdog);
    str_(&mut out, ""); // sub-mode (JT65 only)
    bool_(&mut out, false); // fast mode
    u8_(&mut out, 0); // special operation mode: none
    u32_(&mut out, 0); // frequency tolerance: not applicable
    u32_(&mut out, s.tr_period_s);
    str_(&mut out, "Default"); // configuration name
    str_(&mut out, &s.tx_message);
    out
}

pub fn clear(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    header(&mut out, T_CLEAR, id);
    out
}

pub fn close(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    header(&mut out, T_CLOSE, id);
    out
}

pub fn qso_logged(id: &str, q: &QsoRecord) -> Vec<u8> {
    let mut out = Vec::with_capacity(224);
    header(&mut out, T_QSO_LOGGED, id);
    datetime_(&mut out, q.end_utc);
    str_(&mut out, &q.call);
    str_(&mut out, q.grid.as_deref().unwrap_or(""));
    u64_(&mut out, q.freq_hz.max(0.0) as u64);
    str_(&mut out, &q.mode);
    str_(&mut out, &report(q.rst_sent));
    str_(&mut out, &report(q.rst_rcvd));
    str_(&mut out, &q.tx_pwr.map(|w| format!("{w:.0}")).unwrap_or_default());
    str_(&mut out, &q.comment);
    str_(&mut out, &q.name);
    datetime_(&mut out, q.start_utc);
    str_(&mut out, &q.operator);
    str_(&mut out, &q.my_call);
    str_(&mut out, &q.my_grid);
    str_(&mut out, &q.stx_string);
    str_(&mut out, &q.srx_string);
    str_(&mut out, ""); // ADIF propagation mode
    out
}

pub fn logged_adif(id: &str, adif: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(adif.len() + 32);
    header(&mut out, T_LOGGED_ADIF, id);
    str_(&mut out, adif);
    out
}

// ── Inbound: what a client sends back ───────────────────────────────────────

/// A colour from a Highlight Callsign message, as 8-bit RGB. Qt streams each
/// channel as 16 bits; the high byte is the 8-bit value it was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// One datagram from a client, read but not yet judged.
///
/// Every variant carries `id`: the instance the client means. A client that
/// talks to several WSJT-X instances addresses each by the id it heard in that
/// instance's own traffic, so a message naming another id is not for us.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Heartbeat {
        id: String,
        max_schema: u32,
        version: String,
        revision: String,
    },
    /// The client is going away.
    Close {
        id: String,
    },
    /// Clear our decode windows. Schema 2 clients send no window byte.
    Clear {
        id: String,
        window: Option<u8>,
    },
    /// "Answer this decode", as a double-click on it in WSJT-X would. The
    /// decode is named by the fields we sent it with.
    Reply {
        id: String,
        time_ms: u32,
        snr: i32,
        dt: f64,
        df_hz: u32,
        mode: String,
        message: String,
        low_confidence: bool,
        modifiers: u8,
    },
    /// Send every decode again.
    Replay {
        id: String,
    },
    /// Stop transmitting: at once, or (`auto_tx_only`) after the current over.
    HaltTx {
        id: String,
        auto_tx_only: bool,
    },
    /// Set the free-text message, and send it when `send` is set.
    FreeText {
        id: String,
        text: String,
        send: bool,
    },
    /// The station's locator, from a client with a GPS.
    Location {
        id: String,
        grid: String,
    },
    /// Colour a callsign in our decode list. `None` colours clear the
    /// highlight (an invalid `QColor`).
    Highlight {
        id: String,
        call: String,
        bg: Option<Rgb>,
        fg: Option<Rgb>,
        last_only: bool,
    },
    SwitchConfiguration {
        id: String,
        name: String,
    },
    Configure {
        id: String,
        mode: String,
        dx_call: String,
        dx_grid: String,
    },
    /// A type we do not read — including every one we send ourselves, so
    /// hearing our own traffic back is harmless.
    Unknown {
        id: String,
        kind: u32,
    },
}

impl Inbound {
    /// The instance this message is addressed to.
    pub fn id(&self) -> &str {
        match self {
            Inbound::Heartbeat { id, .. }
            | Inbound::Close { id }
            | Inbound::Clear { id, .. }
            | Inbound::Reply { id, .. }
            | Inbound::Replay { id }
            | Inbound::HaltTx { id, .. }
            | Inbound::FreeText { id, .. }
            | Inbound::Location { id, .. }
            | Inbound::Highlight { id, .. }
            | Inbound::SwitchConfiguration { id, .. }
            | Inbound::Configure { id, .. }
            | Inbound::Unknown { id, .. } => id,
        }
    }
}

/// Why a datagram could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not this protocol at all.
    BadMagic,
    /// The datagram ends inside a field.
    Truncated,
    /// A string that is not UTF-8.
    BadText,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ParseError::BadMagic => "not a WSJT-X datagram",
            ParseError::Truncated => "datagram ends inside a field",
            ParseError::BadText => "string is not UTF-8",
        })
    }
}

/// A bounds-checked `QDataStream` reader. Every read can fail, because a
/// datagram is whatever arrived on the port — nothing about it is promised.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let end = self.at.checked_add(n).ok_or(ParseError::Truncated)?;
        let s = self.b.get(self.at..end).ok_or(ParseError::Truncated)?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, ParseError> {
        Ok(self.u8()? != 0)
    }
    fn u16(&mut self) -> Result<u16, ParseError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| ParseError::Truncated)?))
    }
    fn u32(&mut self) -> Result<u32, ParseError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| ParseError::Truncated)?))
    }
    fn i32(&mut self) -> Result<i32, ParseError> {
        Ok(self.u32()? as i32)
    }
    fn f64(&mut self) -> Result<f64, ParseError> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().map_err(|_| ParseError::Truncated)?))
    }
    /// A `QString`; null reads as empty, which is what every caller wants.
    fn str(&mut self) -> Result<String, ParseError> {
        let n = self.u32()?;
        if n == 0xFFFF_FFFF {
            return Ok(String::new());
        }
        let bytes = self.take(n as usize)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ParseError::BadText)
    }
    /// A `QColor`; `None` for an invalid one (spec 0), which is how a client
    /// says "no colour".
    fn color(&mut self) -> Result<Option<Rgb>, ParseError> {
        let spec = self.u8()?;
        let _alpha = self.u16()?;
        let r = self.u16()?;
        let g = self.u16()?;
        let b = self.u16()?;
        let _pad = self.u16()?;
        Ok((spec != 0).then_some(Rgb { r: (r >> 8) as u8, g: (g >> 8) as u8, b: (b >> 8) as u8 }))
    }
    fn remaining(&self) -> usize {
        self.b.len() - self.at
    }
}

/// Read one datagram. Fields a newer schema appends after the ones read here
/// are ignored rather than refused, as Qt's own readers do.
pub fn parse(datagram: &[u8]) -> Result<Inbound, ParseError> {
    let mut r = Reader::new(datagram);
    if r.u32()? != MAGIC {
        return Err(ParseError::BadMagic);
    }
    let _schema = r.u32()?;
    let kind = r.u32()?;
    let id = r.str()?;
    Ok(match kind {
        T_HEARTBEAT => {
            Inbound::Heartbeat { id, max_schema: r.u32()?, version: r.str()?, revision: r.str()? }
        }
        T_CLOSE => Inbound::Close { id },
        T_CLEAR => {
            let window = if r.remaining() > 0 { Some(r.u8()?) } else { None };
            Inbound::Clear { id, window }
        }
        T_REPLY => Inbound::Reply {
            id,
            time_ms: r.u32()?,
            snr: r.i32()?,
            dt: r.f64()?,
            df_hz: r.u32()?,
            mode: r.str()?,
            message: r.str()?,
            low_confidence: r.bool()?,
            modifiers: r.u8()?,
        },
        T_REPLAY => Inbound::Replay { id },
        T_HALT_TX => Inbound::HaltTx { id, auto_tx_only: r.bool()? },
        T_FREE_TEXT => Inbound::FreeText { id, text: r.str()?, send: r.bool()? },
        T_LOCATION => Inbound::Location { id, grid: r.str()? },
        T_HIGHLIGHT => Inbound::Highlight {
            id,
            call: r.str()?,
            bg: r.color()?,
            fg: r.color()?,
            last_only: r.bool()?,
        },
        T_SWITCH_CONFIG => Inbound::SwitchConfiguration { id, name: r.str()? },
        T_CONFIGURE => {
            let mode = r.str()?;
            let _tolerance = r.u32()?;
            let _submode = r.str()?;
            let _fast = r.bool()?;
            let _tr_period = r.u32()?;
            let _rx_df = r.u32()?;
            let dx_call = r.str()?;
            let dx_grid = r.str()?;
            let _generate = r.bool()?;
            Inbound::Configure { id, mode, dx_call, dx_grid }
        }
        kind => Inbound::Unknown { id, kind },
    })
}

/// A signal report as the protocol carries it: a bare signed number for the
/// digital modes, empty when there is none.
fn report(db: Option<i16>) -> String {
    db.map(|v| v.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read fields back the way a client does, so the tests describe the wire
    /// rather than restating the writer.
    struct Reader<'a> {
        b: &'a [u8],
        at: usize,
    }

    impl<'a> Reader<'a> {
        fn new(b: &'a [u8]) -> Self {
            Reader { b, at: 0 }
        }
        fn u8(&mut self) -> u8 {
            self.at += 1;
            self.b[self.at - 1]
        }
        fn bool(&mut self) -> bool {
            self.u8() != 0
        }
        fn u32(&mut self) -> u32 {
            let v = u32::from_be_bytes(self.b[self.at..self.at + 4].try_into().unwrap());
            self.at += 4;
            v
        }
        fn i32(&mut self) -> i32 {
            self.u32() as i32
        }
        fn u64(&mut self) -> u64 {
            let v = u64::from_be_bytes(self.b[self.at..self.at + 8].try_into().unwrap());
            self.at += 8;
            v
        }
        fn i64(&mut self) -> i64 {
            self.u64() as i64
        }
        fn f64(&mut self) -> f64 {
            f64::from_be_bytes({
                let v = self.b[self.at..self.at + 8].try_into().unwrap();
                self.at += 8;
                v
            })
        }
        fn str(&mut self) -> Option<String> {
            let n = self.u32();
            if n == 0xFFFF_FFFF {
                return None;
            }
            let s = String::from_utf8(self.b[self.at..self.at + n as usize].to_vec()).unwrap();
            self.at += n as usize;
            Some(s)
        }
        fn header(&mut self) -> (u32, u32, Option<String>) {
            assert_eq!(self.u32(), MAGIC, "magic");
            let schema = self.u32();
            let kind = self.u32();
            (schema, kind, self.str())
        }
        fn at_end(&self) -> bool {
            self.at == self.b.len()
        }
    }

    #[test]
    fn every_message_opens_with_the_same_header() {
        for (packet, kind) in [
            (heartbeat("sdroxide", "0.6.0"), T_HEARTBEAT),
            (clear("sdroxide"), T_CLEAR),
            (close("sdroxide"), T_CLOSE),
            (logged_adif("sdroxide", "<CALL:5>W9XYZ<EOR>"), T_LOGGED_ADIF),
        ] {
            let mut r = Reader::new(&packet);
            let (schema, got, id) = r.header();
            assert_eq!(schema, SCHEMA);
            assert_eq!(got, kind);
            assert_eq!(id.as_deref(), Some("sdroxide"));
        }
    }

    #[test]
    fn a_decode_reads_back_field_for_field() {
        let d = DecodeInfo {
            new: true,
            // 2023-11-14 22:13:20 UTC → 22:13:20 into the day.
            slot_utc: 1_700_000_000,
            snr_db: -13,
            dt: 0.25,
            audio_hz: 1234,
            mode: "FT8".into(),
            message: "CQ W9XYZ EM48".into(),
        };
        let packet = decode("WSJT-X", &d);
        let mut r = Reader::new(&packet);
        assert_eq!(r.header().1, T_DECODE);
        assert!(r.bool(), "new");
        assert_eq!(r.u32(), ((22 * 3600 + 13 * 60 + 20) * 1000) as u32, "ms since midnight");
        assert_eq!(r.i32(), -13);
        assert_eq!(r.f64(), 0.25);
        assert_eq!(r.u32(), 1234);
        assert_eq!(r.str().as_deref(), Some("FT8"));
        assert_eq!(r.str().as_deref(), Some("CQ W9XYZ EM48"));
        assert!(!r.bool(), "low confidence");
        assert!(!r.bool(), "off air");
        assert!(r.at_end(), "trailing bytes would shift a client's next field");
    }

    #[test]
    fn status_carries_the_full_schema_3_field_list() {
        // A client reads these positionally: a missing or mistyped field
        // silently corrupts everything after it, so count them all off.
        let s = StatusInfo {
            dial_hz: 14_074_000,
            mode: "FT8".into(),
            dx_call: "W9XYZ".into(),
            report: "-13".into(),
            tx_enabled: true,
            transmitting: false,
            decoding: true,
            rx_df_hz: 1500,
            tx_df_hz: 1500,
            de_call: "AB1CD".into(),
            de_grid: "FN42".into(),
            dx_grid: "EM48".into(),
            tx_watchdog: false,
            tr_period_s: 15,
            tx_message: "W9XYZ AB1CD FN42".into(),
        };
        let packet = status("WSJT-X", &s);
        let mut r = Reader::new(&packet);
        assert_eq!(r.header().1, T_STATUS);
        assert_eq!(r.u64(), 14_074_000);
        assert_eq!(r.str().as_deref(), Some("FT8"));
        assert_eq!(r.str().as_deref(), Some("W9XYZ"));
        assert_eq!(r.str().as_deref(), Some("-13"));
        assert_eq!(r.str().as_deref(), Some("FT8"), "tx mode");
        assert!(r.bool(), "tx enabled");
        assert!(!r.bool(), "transmitting");
        assert!(r.bool(), "decoding");
        assert_eq!(r.u32(), 1500);
        assert_eq!(r.u32(), 1500);
        assert_eq!(r.str().as_deref(), Some("AB1CD"));
        assert_eq!(r.str().as_deref(), Some("FN42"));
        assert_eq!(r.str().as_deref(), Some("EM48"));
        assert!(!r.bool(), "tx watchdog");
        assert_eq!(r.str(), None, "sub-mode is null, not empty");
        assert!(!r.bool(), "fast mode");
        assert_eq!(r.u8(), 0, "special operation mode");
        assert_eq!(r.u32(), 0, "frequency tolerance");
        assert_eq!(r.u32(), 15, "T/R period");
        assert_eq!(r.str().as_deref(), Some("Default"));
        assert_eq!(r.str().as_deref(), Some("W9XYZ AB1CD FN42"));
        assert!(r.at_end());
    }

    #[test]
    fn a_logged_qso_reads_back_as_the_logger_expects() {
        let q = QsoRecord {
            call: "W9XYZ".into(),
            grid: Some("EM48".into()),
            rst_sent: Some(-9),
            rst_rcvd: Some(-12),
            freq_hz: 14_075_500.0,
            mode: "FT8".into(),
            band: "20m".into(),
            start_utc: 1_700_000_000,
            end_utc: 1_700_000_060,
            my_call: "AB1CD".into(),
            my_grid: "FN42".into(),
            ..Default::default()
        };
        let packet = qso_logged("WSJT-X", &q);
        let mut r = Reader::new(&packet);
        assert_eq!(r.header().1, T_QSO_LOGGED);
        // QDateTime off: Julian day then ms, in UTC.
        assert_eq!(r.i64(), 1_700_000_060 / 86_400 + UNIX_EPOCH_JD);
        assert_eq!(r.u32(), ((1_700_000_060 % 86_400) * 1000) as u32);
        assert_eq!(r.u8(), 1, "Qt::UTC");
        assert_eq!(r.str().as_deref(), Some("W9XYZ"));
        assert_eq!(r.str().as_deref(), Some("EM48"));
        assert_eq!(r.u64(), 14_075_500);
        assert_eq!(r.str().as_deref(), Some("FT8"));
        assert_eq!(r.str().as_deref(), Some("-9"));
        assert_eq!(r.str().as_deref(), Some("-12"));
        assert_eq!(r.str(), None, "tx power unset");
        assert_eq!(r.str(), None, "comment");
        assert_eq!(r.str(), None, "name");
        assert_eq!(r.i64(), 1_700_000_000 / 86_400 + UNIX_EPOCH_JD, "date on");
        assert_eq!(r.u32(), ((1_700_000_000 % 86_400) * 1000) as u32);
        assert_eq!(r.u8(), 1);
        assert_eq!(r.str(), None, "operator");
        assert_eq!(r.str().as_deref(), Some("AB1CD"));
        assert_eq!(r.str().as_deref(), Some("FN42"));
        assert_eq!(r.str(), None, "exchange sent");
        assert_eq!(r.str(), None, "exchange received");
        assert_eq!(r.str(), None, "propagation mode");
        assert!(r.at_end());
    }

    // ── Inbound ─────────────────────────────────────────────────────────────

    /// A client datagram, built with the same primitives the writer uses.
    fn pkt(kind: u32, id: &str, body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut out = Vec::new();
        header(&mut out, kind, id);
        body(&mut out);
        out
    }

    /// A `QColor` as Qt streams it: RGB spec (1), alpha, three 16-bit channels
    /// with the 8-bit value repeated in both bytes, and padding.
    fn color(out: &mut Vec<u8>, c: Option<(u8, u8, u8)>) {
        let (spec, (r, g, b)) = match c {
            Some(rgb) => (1u8, rgb),
            None => (0u8, (0, 0, 0)),
        };
        u8_(out, spec);
        for v in [0xFFFFu16, u16::from(r) * 0x101, u16::from(g) * 0x101, u16::from(b) * 0x101, 0] {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }

    fn reply_packet() -> Vec<u8> {
        pkt(T_REPLY, "WSJT-X", |o| {
            u32_(o, time_ms(1_700_000_010));
            i32_(o, -12);
            f64_(o, 0.3);
            u32_(o, 1234);
            str_(o, "FT8");
            str_(o, "CQ W9XYZ EM48");
            bool_(o, false);
            u8_(o, 0x02);
        })
    }

    #[test]
    fn a_reply_reads_every_field() {
        assert_eq!(
            parse(&reply_packet()),
            Ok(Inbound::Reply {
                id: "WSJT-X".into(),
                time_ms: time_ms(1_700_000_010),
                snr: -12,
                dt: 0.3,
                df_hz: 1234,
                mode: "FT8".into(),
                message: "CQ W9XYZ EM48".into(),
                low_confidence: false,
                modifiers: 0x02,
            })
        );
    }

    #[test]
    fn the_short_client_messages_read_back() {
        let id = || "WSJT-X".to_string();
        assert_eq!(parse(&pkt(T_REPLAY, "WSJT-X", |_| {})), Ok(Inbound::Replay { id: id() }));
        assert_eq!(parse(&pkt(T_CLOSE, "WSJT-X", |_| {})), Ok(Inbound::Close { id: id() }));
        for flag in [true, false] {
            assert_eq!(
                parse(&pkt(T_HALT_TX, "WSJT-X", |o| bool_(o, flag))),
                Ok(Inbound::HaltTx { id: id(), auto_tx_only: flag })
            );
        }
        assert_eq!(
            parse(&pkt(T_FREE_TEXT, "WSJT-X", |o| {
                str_(o, "TNX 73 GL");
                bool_(o, true);
            })),
            Ok(Inbound::FreeText { id: id(), text: "TNX 73 GL".into(), send: true })
        );
        assert_eq!(
            parse(&pkt(T_LOCATION, "WSJT-X", |o| str_(o, "FN42AB"))),
            Ok(Inbound::Location { id: id(), grid: "FN42AB".into() })
        );
        assert_eq!(
            parse(&pkt(T_SWITCH_CONFIG, "WSJT-X", |o| str_(o, "Contest"))),
            Ok(Inbound::SwitchConfiguration { id: id(), name: "Contest".into() })
        );
        assert_eq!(
            parse(&pkt(T_HEARTBEAT, "JTAlert", |o| {
                u32_(o, 3);
                str_(o, "2.70");
                str_(o, "");
            })),
            Ok(Inbound::Heartbeat {
                id: "JTAlert".into(),
                max_schema: 3,
                version: "2.70".into(),
                revision: String::new(),
            })
        );
    }

    #[test]
    fn a_clear_reads_with_or_without_its_window() {
        assert_eq!(
            parse(&pkt(T_CLEAR, "WSJT-X", |o| u8_(o, 2))),
            Ok(Inbound::Clear { id: "WSJT-X".into(), window: Some(2) })
        );
        // Schema 2 clients stop after the id.
        assert_eq!(
            parse(&clear("WSJT-X")),
            Ok(Inbound::Clear { id: "WSJT-X".into(), window: None })
        );
    }

    #[test]
    fn a_highlight_carries_its_colours_and_an_invalid_colour_clears() {
        let set = pkt(T_HIGHLIGHT, "WSJT-X", |o| {
            str_(o, "W9XYZ");
            color(o, Some((255, 0, 0)));
            color(o, Some((0, 0, 0)));
            bool_(o, true);
        });
        assert_eq!(
            parse(&set),
            Ok(Inbound::Highlight {
                id: "WSJT-X".into(),
                call: "W9XYZ".into(),
                bg: Some(Rgb { r: 255, g: 0, b: 0 }),
                fg: Some(Rgb { r: 0, g: 0, b: 0 }),
                last_only: true,
            })
        );
        let clear = pkt(T_HIGHLIGHT, "WSJT-X", |o| {
            str_(o, "W9XYZ");
            color(o, None);
            color(o, None);
            bool_(o, false);
        });
        let Ok(Inbound::Highlight { bg, fg, .. }) = parse(&clear) else {
            panic!("not a highlight")
        };
        assert_eq!((bg, fg), (None, None));
    }

    #[test]
    fn a_configure_reads_past_the_fields_it_ignores() {
        let p = pkt(T_CONFIGURE, "WSJT-X", |o| {
            str_(o, "FT4");
            u32_(o, 0);
            str_(o, "");
            bool_(o, false);
            u32_(o, 7);
            u32_(o, 1500);
            str_(o, "W9XYZ");
            str_(o, "EM48");
            bool_(o, true);
        });
        assert_eq!(
            parse(&p),
            Ok(Inbound::Configure {
                id: "WSJT-X".into(),
                mode: "FT4".into(),
                dx_call: "W9XYZ".into(),
                dx_grid: "EM48".into(),
            })
        );
    }

    #[test]
    fn our_own_traffic_reads_back_harmlessly() {
        // Multicast and a misconfigured loopback can hand us our own datagrams.
        let d =
            decode("WSJT-X", &DecodeInfo { message: "CQ W9XYZ EM48".into(), ..Default::default() });
        assert_eq!(parse(&d), Ok(Inbound::Unknown { id: "WSJT-X".into(), kind: T_DECODE }));
        let s = status("WSJT-X", &StatusInfo::default());
        assert_eq!(parse(&s), Ok(Inbound::Unknown { id: "WSJT-X".into(), kind: T_STATUS }));
        assert!(matches!(parse(&heartbeat("WSJT-X", "1.0")), Ok(Inbound::Heartbeat { .. })));
    }

    #[test]
    fn a_wrong_magic_is_not_this_protocol() {
        let mut p = reply_packet();
        p[0] ^= 0xFF;
        assert_eq!(parse(&p), Err(ParseError::BadMagic));
    }

    #[test]
    fn every_truncation_is_an_error_and_never_a_panic() {
        let p = reply_packet();
        for n in 0..p.len() {
            assert!(parse(&p[..n]).is_err(), "a reply cut to {n} bytes parsed");
        }
    }

    #[test]
    fn a_string_longer_than_the_datagram_is_refused() {
        let p = pkt(T_LOCATION, "WSJT-X", |o| {
            u32_(o, 0x7FFF_FFFF);
            o.extend_from_slice(b"FN42");
        });
        assert_eq!(parse(&p), Err(ParseError::Truncated));
    }

    #[test]
    fn fields_a_newer_schema_appends_are_ignored() {
        let mut p = reply_packet();
        p.extend_from_slice(&[0xAA; 12]);
        assert!(matches!(parse(&p), Ok(Inbound::Reply { .. })));
    }

    #[test]
    fn noise_never_panics_the_reader() {
        // A deterministic xorshift, so a failure reproduces. Half the buffers
        // keep a valid header so the per-type readers see garbage too.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for i in 0..4000 {
            let len = (next() % 96) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if i % 2 == 0 {
                let mut head = Vec::new();
                header(&mut head, (next() % 18) as u32, "WSJT-X");
                head.extend_from_slice(&buf);
                buf = head;
            }
            let _ = parse(&buf);
        }
    }
}

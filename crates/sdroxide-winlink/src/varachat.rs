//! Keyboard chat over a VARA link: what goes inside the byte stream.
//!
//! Two conventions are in use on the air, and this speaks both:
//!
//! * **Plain text**, as EA5HVK's VARA Chat and terminal-style programs send
//!   it: Latin-1 lines ending in CR.
//! * **VarAC's messages.** VarAC (4Z1AC) frames every message as its length
//!   in bytes, a space and the UTF-8 text — `11 hello there` — and carries
//!   signal reports and profile details as tags (`<R-05>`, `<NAME:…>`). Each
//!   side opens a QSO with `de CALL <R±NN>`, and `<DISC>` comes before the
//!   link is closed. A CQ frame's callsign suffix says which slot the caller
//!   waits on: -1…-5 are 750…3750 Hz below the calling frequency, -11…-15
//!   the same above, and -9 is a beacon.
//!
//! VarAC is closed and its formats are not published. The facts above come
//! from hfmodem by Saul St John, W9SSJ (<https://hfmodem.ssj.lol/>), whose
//! protocol descriptions are CC BY 4.0; this is sdroxide's own implementation
//! of them, not a port of hfmodem's (AGPL) code. VarAC's QSY handshake,
//! beacons, VMail and file transfer are not implemented.

/// The step between VarAC's slots.
pub const SLOT_STEP_HZ: i32 = 750;
/// VarAC's beacon suffix.
const BEACON_SUFFIX: u32 = 9;
/// The largest VarAC message accepted; anything claiming more is not VarAC.
const MAX_MESSAGE: usize = 1_000_000;

/// One message as VarAC sends it: the byte length, a space, the UTF-8 text.
pub fn frame(text: &str) -> Vec<u8> {
    let mut out = format!("{} ", text.len()).into_bytes();
    out.extend_from_slice(text.as_bytes());
    out
}

/// One line as a plain-text station expects it: Latin-1, ending in CR.
/// Characters Latin-1 cannot hold are sent as `?`.
pub fn plain_line(text: &str) -> Vec<u8> {
    let mut out: Vec<u8> = text
        .chars()
        .map(|c| match c {
            '\r' | '\n' => b' ',
            c if (c as u32) < 256 => c as u32 as u8,
            _ => b'?',
        })
        .collect();
    out.push(b'\r');
    out
}

/// VarAC's opening report: `de CALL <R+05>`, the S/N we hear them at.
pub fn opening(mycall: &str, snr_db: f32) -> String {
    let r = snr_db.round().clamp(-99.0, 99.0) as i32;
    format!(
        "de {} <R{}{:02}>",
        mycall.trim().to_uppercase(),
        if r < 0 { '-' } else { '+' },
        r.abs()
    )
}

/// The profile tags VarAC sends after its opening report, or `None` when
/// there is nothing to say. Values are cleaned of what would break a tag.
pub fn profile(name: &str, qth: &str, locator: &str) -> Option<String> {
    let clean = |s: &str| s.replace(['<', '>', '\r', '\n'], " ").trim().to_string();
    let tags: Vec<String> = [("NAME", name), ("QTH", qth), ("LOC", locator)]
        .into_iter()
        .map(|(k, v)| (k, clean(v)))
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("<{k}:{v}>"))
        .collect();
    (!tags.is_empty()).then(|| tags.join("\r\n"))
}

/// What the received bytes held so far.
#[derive(Debug, Default, PartialEq)]
pub struct Fed {
    /// Whole VarAC messages.
    pub messages: Vec<String>,
    /// Text from a station that does not frame its messages.
    pub plain: String,
}

/// Splits a link's received bytes into VarAC messages, however the modem cut
/// them. A stream that does not begin with a length is not VarAC: from then
/// on, for the rest of the link, everything is plain text.
#[derive(Debug, Default)]
pub struct Parser {
    pending: Vec<u8>,
    plain: bool,
}

impl Parser {
    /// The other station turned out to send plain text.
    pub fn is_plain(&self) -> bool {
        self.plain
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Fed {
        let mut fed = Fed::default();
        if self.plain {
            fed.plain = latin1(bytes);
            return fed;
        }
        self.pending.extend_from_slice(bytes);
        loop {
            if self.pending.is_empty() {
                break;
            }
            let space = self.pending.iter().position(|&b| b == b' ');
            let head_end = space.unwrap_or(self.pending.len());
            let head = &self.pending[..head_end];
            let digits = !head.is_empty() && head.len() <= 9 && head.iter().all(u8::is_ascii_digit);
            match (space, digits) {
                // Still arriving: a short run of digits and no space yet.
                (None, _) if head.len() <= 9 && head.iter().all(u8::is_ascii_digit) => break,
                (Some(_), true) => {}
                // Not a length: not VarAC. Hand the lot over as text.
                _ => {
                    self.plain = true;
                    fed.plain = latin1(&std::mem::take(&mut self.pending));
                    return fed;
                }
            }
            let size: usize =
                std::str::from_utf8(head).ok().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
            if size > MAX_MESSAGE {
                self.plain = true;
                fed.plain = latin1(&std::mem::take(&mut self.pending));
                return fed;
            }
            let start = head_end + 1;
            if self.pending.len() < start + size {
                break;
            }
            let mut end = start + size;
            // VarAC's VMail leaves its closing '>' out of the count (hfmodem).
            if self.pending[start..end].starts_with(b"<SM>")
                && self.pending.get(end - 1) != Some(&b'>')
                && self.pending.get(end) == Some(&b'>')
            {
                end += 1;
            }
            fed.messages.push(String::from_utf8_lossy(&self.pending[start..end]).into_owned());
            self.pending.drain(..end);
        }
        fed
    }
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

/// A received message, ready for the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// What to show. Tags are put into words; unknown ones are left as sent.
    pub text: String,
    /// Nothing but tags: a note about the QSO rather than something said.
    pub note: bool,
    /// The station is closing the QSO.
    pub disc: bool,
}

/// What a tag says, in words, or `None` to show it as it was sent.
fn describe(name: &str, value: Option<&str>) -> Option<String> {
    let services = |v: &str| {
        v.chars()
            .map(|c| match c {
                'V' => "VMail",
                'E' => "email",
                'A' => "AI",
                'B' => "BBS",
                'N' => "nothing",
                _ => "?",
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(match (name, value) {
        ("NAME", Some(v)) => format!("name {v}"),
        ("QTH", Some(v)) => format!("QTH {v}"),
        ("LOC", Some(v)) => format!("locator {v}"),
        ("EM", Some(v)) => format!("email {v}"),
        ("S", Some(v)) => format!("offers {}", services(v)),
        ("Q", Some(v)) => {
            let n: i64 = v.trim().parse().ok()?;
            format!("asks to move {:+} Hz", n * 10)
        }
        ("QF", Some(v)) => format!("is on {} Hz", v.trim()),
        ("DISC", None) => "is closing the QSO".into(),
        ("AWAY", None) => "is away".into(),
        ("AWQ", None) => "is away (leave a VMail)".into(),
        ("Q", None) => "is at the station".into(),
        ("TL", None) => "has arrived on the new frequency".into(),
        ("QSYR", None) => "is ready on the new frequency".into(),
        ("QSYU", None) => "asks to move up 750 Hz".into(),
        ("QSYD", None) => "asks to move down 750 Hz".into(),
        _ => return None,
    })
}

/// Put a VarAC message into words for `from` (a callsign).
pub fn render(message: &str, from: &str) -> Rendered {
    let mut text = String::new();
    let mut said: Vec<String> = Vec::new();
    let mut disc = false;
    let mut rest = message;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find('>') else {
            text.push_str(after);
            rest = "";
            break;
        };
        let tag = &after[1..close];
        let mut consumed = close + 1;
        let words = if let Some(r) = tag.strip_prefix('R').filter(|r| {
            r.len() == 3
                && (r.starts_with('+') || r.starts_with('-'))
                && r[1..].bytes().all(|b| b.is_ascii_digit())
        }) {
            Some(format!("hears you at {} dB", r.parse::<i32>().unwrap_or(0)))
        } else if tag == "QSYF" {
            // `<QSYF>014105750</QSYF>`: a frequency to move to.
            match after[consumed..].find("</QSYF>") {
                Some(end) => {
                    let hz = &after[consumed..consumed + end];
                    consumed += end + "</QSYF>".len();
                    Some(format!("asks to move to {} Hz", hz.trim_start_matches('0')))
                }
                None => None,
            }
        } else {
            let (name, value) = match tag.split_once(':') {
                Some((n, v)) => (n, Some(v)),
                None => (tag, None),
            };
            let known = !name.is_empty()
                && name.len() <= 8
                && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'/' || b == b'&');
            if known && name == "DISC" {
                disc = true;
            }
            known.then(|| describe(name, value)).flatten()
        };
        match words {
            Some(w) => said.push(w),
            None => text.push_str(&after[..consumed]),
        }
        rest = &after[consumed..];
    }
    text.push_str(rest);
    // `de CALL` in front of the opening report is only the sender's name.
    let words_only = text.trim();
    let opening = !said.is_empty()
        && words_only.to_ascii_lowercase().starts_with("de ")
        && words_only.split_whitespace().count() == 2;
    let text: String = if opening { String::new() } else { clean(&text) };
    if text.trim().is_empty() {
        let note =
            if said.is_empty() { String::new() } else { format!("{from} {}", said.join("; ")) };
        return Rendered { text: note, note: true, disc };
    }
    let mut out = text.trim_end().to_string();
    if !said.is_empty() {
        out.push_str(&format!(" ({})", said.join("; ")));
    }
    Rendered { text: out, note: false, disc }
}

/// Line breaks as `\n`, other control characters dropped.
pub fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

/// A heard CQ frame's callsign, read the way VarAC writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CqCall {
    /// The callsign to call: the slot suffix removed.
    pub call: String,
    /// Where the caller waits, relative to the frequency the CQ was heard on.
    pub offset_hz: i32,
    /// A beacon rather than a CQ.
    pub beacon: bool,
}

pub fn decode_cq(raw: &str) -> CqCall {
    let up = raw.trim().to_uppercase();
    let plain = CqCall { call: up.clone(), offset_hz: 0, beacon: false };
    let Some((call, suffix)) = up.rsplit_once('-') else { return plain };
    let Ok(n) = suffix.parse::<u32>() else { return plain };
    match n {
        BEACON_SUFFIX => CqCall { call: call.into(), offset_hz: 0, beacon: true },
        1..=5 => CqCall { call: call.into(), offset_hz: -(n as i32) * SLOT_STEP_HZ, beacon: false },
        11..=15 => {
            CqCall { call: call.into(), offset_hz: (n as i32 - 10) * SLOT_STEP_HZ, beacon: false }
        }
        // A real SSID.
        _ => plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_framed_by_their_byte_length() {
        assert_eq!(frame("hello there"), b"11 hello there");
        assert_eq!(frame("73 de EA5ABC ñ"), "15 73 de EA5ABC ñ".as_bytes());
        assert_eq!(plain_line("hola ñ\n€"), b"hola \xf1 ?\r");
    }

    #[test]
    fn the_opening_report_and_profile() {
        assert_eq!(opening("ea5abc", 5.4), "de EA5ABC <R+05>");
        assert_eq!(opening("EA5ABC", -12.6), "de EA5ABC <R-13>");
        assert_eq!(opening("EA5ABC", 140.0), "de EA5ABC <R+99>");
        assert_eq!(
            profile("José", "Valencia", "IM99"),
            Some("<NAME:José>\r\n<QTH:Valencia>\r\n<LOC:IM99>".into())
        );
        assert_eq!(profile("a<b>", "", ""), Some("<NAME:a b>".into()));
        assert_eq!(profile("", " ", ""), None);
    }

    #[test]
    fn a_stream_is_split_into_messages_however_it_arrives() {
        let mut p = Parser::default();
        let mut all = frame("hello there");
        all.extend(frame("<R-05>"));
        all.extend(frame("second\r\nline"));
        let mut got = Vec::new();
        for chunk in all.chunks(3) {
            let f = p.feed(chunk);
            assert!(f.plain.is_empty());
            got.extend(f.messages);
        }
        assert_eq!(got, ["hello there", "<R-05>", "second\r\nline"]);
        assert!(!p.is_plain());
    }

    #[test]
    fn unframed_text_switches_the_link_to_plain() {
        let mut p = Parser::default();
        let f = p.feed(b"Hola EA5ABC, aqui EA5XYZ\r");
        assert!(f.messages.is_empty());
        assert_eq!(f.plain, "Hola EA5ABC, aqui EA5XYZ\r");
        assert!(p.is_plain());
        assert_eq!(p.feed(b"12 not framed").plain, "12 not framed");
        // A plain line that happens to start with digits still turns plain.
        let mut p = Parser::default();
        assert_eq!(p.feed(b"73!\r").plain, "73!\r");
    }

    #[test]
    fn the_vmail_quirk_keeps_its_closing_bracket() {
        let mut p = Parser::default();
        let f = p.feed(b"5 <SM>x>3 abc");
        assert_eq!(f.messages, ["<SM>x>", "abc"]);
    }

    #[test]
    fn tags_are_put_into_words() {
        let r = render("de EA5XYZ <R-05>", "EA5XYZ");
        assert_eq!(
            r,
            Rendered { text: "EA5XYZ hears you at -5 dB".into(), note: true, disc: false }
        );
        let r = render("<NAME:Ana>\r\n<QTH:Alicante>\r\n<LOC:IM98>", "EA5XYZ");
        assert_eq!(r.text, "EA5XYZ name Ana; QTH Alicante; locator IM98");
        assert!(r.note);
        let r = render("Thanks for the QSO, 73 <DISC>", "EA5XYZ");
        assert_eq!(
            r,
            Rendered {
                text: "Thanks for the QSO, 73 (is closing the QSO)".into(),
                note: false,
                disc: true
            }
        );
        assert_eq!(render("<Q:-75>", "X").text, "X asks to move -750 Hz");
        assert_eq!(render("<QSYF>014105750</QSYF>", "X").text, "X asks to move to 14105750 Hz");
        // Not a tag VarAC uses: left as it was written.
        assert_eq!(render("a <b> c", "X").text, "a <b> c");
        assert_eq!(render("x < y", "X").text, "x < y");
    }

    #[test]
    fn cq_suffixes_name_the_slot() {
        assert_eq!(
            decode_cq("F6JON-2"),
            CqCall { call: "F6JON".into(), offset_hz: -1500, beacon: false }
        );
        assert_eq!(
            decode_cq("f6jon-13"),
            CqCall { call: "F6JON".into(), offset_hz: 2250, beacon: false }
        );
        assert_eq!(
            decode_cq("N1AMD-9"),
            CqCall { call: "N1AMD".into(), offset_hz: 0, beacon: true }
        );
        assert_eq!(
            decode_cq("EA5XYZ-10"),
            CqCall { call: "EA5XYZ-10".into(), offset_hz: 0, beacon: false }
        );
        assert_eq!(
            decode_cq("EA5XYZ"),
            CqCall { call: "EA5XYZ".into(), offset_hz: 0, beacon: false }
        );
    }
}

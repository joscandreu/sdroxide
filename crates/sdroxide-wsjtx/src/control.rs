//! What a client's message is allowed to do.
//!
//! [`crate::msg::parse`] reads a datagram; this decides what it means for the
//! radio. It is a pure function over the message and a snapshot of what the
//! engine knows — our id, our callsign, the decodes we broadcast, whether the
//! operator lets clients drive at all — so every rule here is tested without a
//! socket or an engine.
//!
//! The commands it produces are the ones the operator's own buttons send, and
//! the engine applies them through the same funnel as a click. A Reply from
//! JTAlert therefore answers a station exactly the way REPLY on the decode list
//! does, under the same ham-band lockout, Hold TX and watchdog.

use sdroxide_types::{Command, Decode, WsjtxHighlight};

use crate::msg::{Inbound, Rgb, time_ms};

/// FT8 carries 13 characters of free text, and that is all the sequencer sends.
const FREE_TEXT_MAX: usize = 13;

/// How far a Reply's audio offset may sit from the decode it names. The offset
/// went out as a whole number of hertz, so this is rounding and nothing else.
const DF_TOLERANCE_HZ: u32 = 5;

/// What the engine knows when a message arrives.
pub struct Context<'a> {
    /// Our instance id, as the broadcast announces it.
    pub id: &'a str,
    /// The operator's callsign, for telling a decode addressed to us.
    pub my_call: &'a str,
    /// [`sdroxide_types::WsjtxConfig::accept_control`].
    pub accept_control: bool,
    /// The decodes we broadcast on this band, oldest first.
    pub recent: &'a [Decode],
}

/// What the engine should do about one message.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Apply this command, as if the operator had sent it. Boxed: a
    /// `Command` dwarfs every other variant here.
    Cmd(Box<Command>),
    /// Broadcast every decode in [`Context::recent`] again.
    Replay,
    /// Colour a callsign in the decode list; both colours `None` clears it.
    Highlight(WsjtxHighlight),
    /// A client announced itself (its heartbeat), by its own id.
    ClientSeen(String),
    /// A client said it is going away.
    ClientGone(String),
}

/// Decide what `msg` does. Anything not for us, not permitted, or not
/// understood comes back empty.
pub fn translate(msg: &Inbound, ctx: &Context) -> Vec<Action> {
    // A client's own heartbeat and close carry *its* id, not ours — that is
    // how it is told apart — so those two are read before the id test. Our
    // own, heard back on a shared or multicast port, are not a client.
    match msg {
        Inbound::Heartbeat { id, .. } if id != ctx.id => {
            return vec![Action::ClientSeen(id.clone())];
        }
        Inbound::Close { id } if id != ctx.id => return vec![Action::ClientGone(id.clone())],
        Inbound::Heartbeat { .. } | Inbound::Close { .. } => return Vec::new(),
        _ => {}
    }
    // Everything else is addressed to an instance by its id. One meant for
    // another WSJT-X on the same port is not ours to act on.
    if msg.id() != ctx.id {
        return Vec::new();
    }
    // Halt can only take the transmitter off the air, so it needs no
    // permission: a client that can see us keying should be able to stop it.
    if let Inbound::HaltTx { auto_tx_only, .. } = msg {
        return if *auto_tx_only {
            vec![Action::Cmd(Box::new(Command::DigiStopQso))]
        } else {
            vec![
                Action::Cmd(Box::new(Command::DigiAbortTx)),
                Action::Cmd(Box::new(Command::DigiStopQso)),
            ]
        };
    }
    if !ctx.accept_control {
        return Vec::new();
    }
    match msg {
        Inbound::Reply { time_ms, df_hz, message, low_confidence, .. } => {
            if *low_confidence {
                return Vec::new();
            }
            reply(ctx, *time_ms, *df_hz, message).into_iter().collect()
        }
        Inbound::FreeText { text, send: true, .. } => {
            let text: String =
                text.trim().to_ascii_uppercase().chars().take(FREE_TEXT_MAX).collect();
            if text.is_empty() {
                return Vec::new();
            }
            vec![Action::Cmd(Box::new(Command::DigiSendText(text)))]
        }
        Inbound::Replay { .. } => vec![Action::Replay],
        Inbound::Highlight { call, bg, fg, last_only, .. } => {
            let call = call.trim().to_ascii_uppercase();
            if call.is_empty() {
                return Vec::new();
            }
            vec![Action::Highlight(WsjtxHighlight {
                call,
                bg: bg.map(rgb),
                fg: fg.map(rgb),
                last_only: *last_only,
            })]
        }
        _ => Vec::new(),
    }
}

/// Answer the decode a Reply names, the way REPLY on the decode list does.
///
/// The decode is found by the three things we sent it with — slot time, text
/// and audio offset — and the newest match wins, since a station repeating its
/// CQ sends the same text slot after slot. A Reply naming nothing we sent is
/// dropped: the text alone is not enough to start a transmission on.
fn reply(ctx: &Context, time: u32, df_hz: u32, message: &str) -> Option<Action> {
    let message = message.trim();
    let d = ctx.recent.iter().rev().find(|d| {
        time_ms(d.slot_utc) == time
            && d.message.trim() == message
            && (d.audio_hz.max(0.0) as u32).abs_diff(df_hz) <= DF_TOLERANCE_HZ
    })?;
    let from = d.from.clone()?;
    let to_me = !ctx.my_call.is_empty()
        && d.to.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(ctx.my_call));
    Some(Action::Cmd(Box::new(Command::DigiStartQso {
        from,
        grid: d.grid.clone(),
        snr: d.snr_db,
        audio_hz: d.audio_hz,
        // The decode list's own judgement: a station neither calling CQ nor
        // calling us is in somebody else's exchange, so wait for their CQ.
        wait_for_cq: !d.is_cq && !to_me,
    })))
}

fn rgb(c: Rgb) -> [u8; 3] {
    [c.r, c.g, c.b]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "WSJT-X";
    const SLOT: i64 = 1_700_000_010;

    fn decode(message: &str, to: Option<&str>, from: Option<&str>, is_cq: bool) -> Decode {
        Decode {
            slot_utc: SLOT,
            snr_db: -12,
            dt: 0.2,
            audio_hz: 1234.4,
            message: message.into(),
            to: to.map(Into::into),
            from: from.map(Into::into),
            grid: Some("EM48".into()),
            is_cq,
            cq_to: None,
            free_text: from.is_none(),
            rr73_to: None,
        }
    }

    fn cq() -> Decode {
        decode("CQ W9XYZ EM48", None, Some("W9XYZ"), true)
    }

    fn ctx(recent: &[Decode]) -> Context<'_> {
        Context { id: ID, my_call: "AB1CD", accept_control: true, recent }
    }

    fn reply_to(d: &Decode) -> Inbound {
        Inbound::Reply {
            id: ID.into(),
            time_ms: time_ms(d.slot_utc),
            snr: d.snr_db.into(),
            dt: d.dt.into(),
            df_hz: d.audio_hz as u32,
            mode: "FT8".into(),
            message: d.message.clone(),
            low_confidence: false,
            modifiers: 0,
        }
    }

    fn started(actions: &[Action]) -> Option<(&str, bool)> {
        let [Action::Cmd(cmd)] = actions else { return None };
        match cmd.as_ref() {
            Command::DigiStartQso { from, wait_for_cq, .. } => Some((from.as_str(), *wait_for_cq)),
            _ => None,
        }
    }

    #[test]
    fn a_reply_to_a_cq_answers_it_at_once() {
        let recent = [cq()];
        let got = translate(&reply_to(&recent[0]), &ctx(&recent));
        assert_eq!(
            got,
            vec![Action::Cmd(Box::new(Command::DigiStartQso {
                from: "W9XYZ".into(),
                grid: Some("EM48".into()),
                snr: -12,
                audio_hz: 1234.4,
                wait_for_cq: false,
            }))]
        );
    }

    #[test]
    fn a_reply_to_a_station_calling_us_answers_at_once() {
        let recent = [decode("AB1CD W9XYZ -07", Some("AB1CD"), Some("W9XYZ"), false)];
        let got = translate(&reply_to(&recent[0]), &ctx(&recent));
        assert_eq!(started(&got), Some(("W9XYZ", false)));
    }

    #[test]
    fn a_reply_to_someone_elses_exchange_waits_for_their_cq() {
        let recent = [decode("K1ABC W9XYZ R-07", Some("K1ABC"), Some("W9XYZ"), false)];
        let got = translate(&reply_to(&recent[0]), &ctx(&recent));
        assert_eq!(started(&got), Some(("W9XYZ", true)));
    }

    #[test]
    fn a_reply_naming_nothing_we_sent_does_nothing() {
        let recent = [cq()];
        let ctx = ctx(&recent);

        let mut wrong_time = reply_to(&recent[0]);
        if let Inbound::Reply { time_ms, .. } = &mut wrong_time {
            *time_ms += 15_000;
        }
        assert!(translate(&wrong_time, &ctx).is_empty(), "another slot");

        let mut wrong_text = reply_to(&recent[0]);
        if let Inbound::Reply { message, .. } = &mut wrong_text {
            *message = "CQ K1ABC FN42".into();
        }
        assert!(translate(&wrong_text, &ctx).is_empty(), "another message");

        let mut wrong_df = reply_to(&recent[0]);
        if let Inbound::Reply { df_hz, .. } = &mut wrong_df {
            *df_hz += DF_TOLERANCE_HZ + 1;
        }
        assert!(translate(&wrong_df, &ctx).is_empty(), "another frequency");

        let mut near_df = reply_to(&recent[0]);
        if let Inbound::Reply { df_hz, .. } = &mut near_df {
            *df_hz += DF_TOLERANCE_HZ;
        }
        assert!(started(&translate(&near_df, &ctx)).is_some(), "rounding is not a mismatch");
    }

    #[test]
    fn a_low_confidence_reply_does_nothing() {
        let recent = [cq()];
        let mut r = reply_to(&recent[0]);
        if let Inbound::Reply { low_confidence, .. } = &mut r {
            *low_confidence = true;
        }
        assert!(translate(&r, &ctx(&recent)).is_empty());
    }

    #[test]
    fn a_reply_to_free_text_has_nobody_to_answer() {
        let recent = [decode("TNX FER QSO", None, None, false)];
        assert!(translate(&reply_to(&recent[0]), &ctx(&recent)).is_empty());
    }

    #[test]
    fn the_newest_of_two_identical_decodes_is_the_one_answered() {
        let mut old = cq();
        old.snr_db = -20;
        let mut new = cq();
        new.snr_db = -3;
        let recent = [old, new];
        let got = translate(&reply_to(&recent[1]), &ctx(&recent));
        let [Action::Cmd(cmd)] = got.as_slice() else { panic!("no QSO started: {got:?}") };
        let Command::DigiStartQso { snr, .. } = cmd.as_ref() else { panic!("not a QSO: {cmd:?}") };
        assert_eq!(*snr, -3);
    }

    #[test]
    fn a_message_for_another_instance_is_ignored() {
        let recent = [cq()];
        let ctx = ctx(&recent);
        let other = "WSJT-X - Rig 2".to_string();
        for msg in [
            Inbound::Replay { id: other.clone() },
            Inbound::HaltTx { id: other.clone(), auto_tx_only: false },
            Inbound::FreeText { id: other.clone(), text: "HI".into(), send: true },
            Inbound::Highlight {
                id: other.clone(),
                call: "W9XYZ".into(),
                bg: None,
                fg: None,
                last_only: false,
            },
            Inbound::Location { id: other.clone(), grid: "FN42".into() },
        ] {
            assert!(translate(&msg, &ctx).is_empty(), "{msg:?}");
        }
        let mut r = reply_to(&recent[0]);
        if let Inbound::Reply { id, .. } = &mut r {
            *id = other;
        }
        assert!(translate(&r, &ctx).is_empty(), "reply");
    }

    #[test]
    fn without_control_only_halt_gets_through() {
        let recent = [cq()];
        let ctx = Context { accept_control: false, ..ctx(&recent) };
        assert!(translate(&reply_to(&recent[0]), &ctx).is_empty(), "reply");
        assert!(
            translate(&Inbound::FreeText { id: ID.into(), text: "HI".into(), send: true }, &ctx)
                .is_empty(),
            "free text"
        );
        assert!(translate(&Inbound::Replay { id: ID.into() }, &ctx).is_empty(), "replay");
        let hl = Inbound::Highlight {
            id: ID.into(),
            call: "W9XYZ".into(),
            bg: Some(Rgb { r: 1, g: 2, b: 3 }),
            fg: None,
            last_only: false,
        };
        assert!(translate(&hl, &ctx).is_empty(), "highlight");
        assert_eq!(
            translate(&Inbound::HaltTx { id: ID.into(), auto_tx_only: true }, &ctx),
            vec![Action::Cmd(Box::new(Command::DigiStopQso))]
        );
    }

    #[test]
    fn halt_stops_now_or_after_the_over() {
        let ctx = ctx(&[]);
        assert_eq!(
            translate(&Inbound::HaltTx { id: ID.into(), auto_tx_only: true }, &ctx),
            vec![Action::Cmd(Box::new(Command::DigiStopQso))],
            "auto-TX only: finish this over, then stand down"
        );
        assert_eq!(
            translate(&Inbound::HaltTx { id: ID.into(), auto_tx_only: false }, &ctx),
            vec![
                Action::Cmd(Box::new(Command::DigiAbortTx)),
                Action::Cmd(Box::new(Command::DigiStopQso))
            ],
            "a full halt takes the transmitter off the air at once"
        );
    }

    #[test]
    fn free_text_is_sent_only_when_asked_and_cut_to_what_ft8_carries() {
        let ctx = ctx(&[]);
        assert_eq!(
            translate(
                &Inbound::FreeText {
                    id: ID.into(),
                    text: " tnx fer the qso 73 ".into(),
                    send: true
                },
                &ctx
            ),
            vec![Action::Cmd(Box::new(Command::DigiSendText("TNX FER THE Q".into())))]
        );
        assert!(
            translate(&Inbound::FreeText { id: ID.into(), text: "HI".into(), send: false }, &ctx)
                .is_empty(),
            "setting the text without sending it has nothing to set here"
        );
        assert!(
            translate(&Inbound::FreeText { id: ID.into(), text: "  ".into(), send: true }, &ctx)
                .is_empty(),
            "nothing to send"
        );
    }

    #[test]
    fn replay_and_highlight_reach_the_engine() {
        let ctx = ctx(&[]);
        assert_eq!(translate(&Inbound::Replay { id: ID.into() }, &ctx), vec![Action::Replay]);
        let hl = Inbound::Highlight {
            id: ID.into(),
            call: "w9xyz".into(),
            bg: Some(Rgb { r: 255, g: 0, b: 0 }),
            fg: None,
            last_only: true,
        };
        assert_eq!(
            translate(&hl, &ctx),
            vec![Action::Highlight(WsjtxHighlight {
                call: "W9XYZ".into(),
                bg: Some([255, 0, 0]),
                fg: None,
                last_only: true,
            })]
        );
    }

    #[test]
    fn clients_are_tracked_by_their_own_id() {
        let ctx = ctx(&[]);
        let beat = Inbound::Heartbeat {
            id: "JTAlert".into(),
            max_schema: 3,
            version: "2.70".into(),
            revision: String::new(),
        };
        assert_eq!(translate(&beat, &ctx), vec![Action::ClientSeen("JTAlert".into())]);
        assert_eq!(
            translate(&Inbound::Close { id: "JTAlert".into() }, &ctx),
            vec![Action::ClientGone("JTAlert".into())]
        );
        // Our own traffic, heard back on a shared or multicast port, is not a
        // client arriving or leaving.
        assert!(translate(&Inbound::Close { id: ID.into() }, &ctx).is_empty());
        let own = Inbound::Heartbeat {
            id: ID.into(),
            max_schema: 3,
            version: "1".into(),
            revision: String::new(),
        };
        assert!(translate(&own, &ctx).is_empty());
    }

    #[test]
    fn the_messages_left_for_later_do_nothing() {
        let ctx = ctx(&[]);
        for msg in [
            Inbound::Location { id: ID.into(), grid: "FN42".into() },
            Inbound::SwitchConfiguration { id: ID.into(), name: "Contest".into() },
            Inbound::Configure {
                id: ID.into(),
                mode: "FT4".into(),
                dx_call: String::new(),
                dx_grid: String::new(),
            },
            Inbound::Clear { id: ID.into(), window: Some(2) },
            Inbound::Unknown { id: ID.into(), kind: 2 },
        ] {
            assert!(translate(&msg, &ctx).is_empty(), "{msg:?}");
        }
    }
}

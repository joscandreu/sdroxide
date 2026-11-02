//! The VARA CHAT window: keyboard chat over a VARA link.
//!
//! A conversation pane, a line to type into, and above them who the QSO is
//! with — or, standing by, a callsign to call and the CQs heard. Everything
//! happens on the engine's machine (the TNC is there); this only draws the
//! status the engine sends and turns clicks into commands.

use crate::i18n::tr;
use eframe::egui::{self, RichText};
use sdroxide_types::{ChatLine, ChatLineKind, ChatSaid, ChatState, Command};

impl crate::app::SdroxideApp {
    pub(in crate::app) fn vara_chat_window(
        &mut self,
        ctx: &egui::Context,
        cmds: &mut Vec<Command>,
    ) {
        if !self.mail.chat_open {
            return;
        }
        let mut open = true;
        egui::Window::new(tr!("varachat-title"))
            .id(crate::layout::salted_id(ctx, "VARA-CHAT"))
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_width(crate::layout::window_w(ctx, 620.0))
            .default_height(crate::layout::window_h(ctx, 520.0))
            .min_width(crate::layout::window_w(ctx, 360.0))
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                self.vara_chat_body(ui, cmds);
            });
        self.mail.chat_open = open;
    }

    fn vara_chat_body(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let st = self.mail.status().chat.clone();
        // Where the station stands, and the one button that fits it.
        ui.horizontal_wrapped(|ui| {
            let (text, color) = match st.state {
                ChatState::Off => (tr!("varachat-off"), crate::theme::YELLOW()),
                ChatState::Standby => (tr!("varachat-standby"), crate::theme::GREEN()),
                ChatState::Calling => {
                    (tr!("varachat-calling", call = st.peer.clone()), crate::theme::YELLOW())
                }
                ChatState::Connected => {
                    let how = if st.plain { tr!("varachat-plain") } else { tr!("varachat-varac") };
                    (
                        tr!("varachat-connected", call = st.peer.clone(), how = how),
                        crate::theme::CYAN(),
                    )
                }
                ChatState::Closing => {
                    (tr!("varachat-closing", call = st.peer.clone()), crate::theme::YELLOW())
                }
            };
            ui.label(RichText::new(text).strong().color(color));
            match st.state {
                ChatState::Calling | ChatState::Connected | ChatState::Closing => {
                    if ui
                        .button(tr!("varachat-hang-up"))
                        .on_hover_text(tr!("varachat-hang-up-hover"))
                        .clicked()
                    {
                        cmds.push(Command::VaraChatHangUp);
                    }
                }
                ChatState::Standby => {
                    let resp = crate::chrome::field(
                        ui,
                        egui::TextEdit::singleline(&mut self.mail.chat_call)
                            .desired_width(110.0)
                            .hint_text(tr!("varachat-callsign")),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let call = self.mail.chat_call.trim().to_uppercase();
                    if (ui
                        .add_enabled(!call.is_empty(), egui::Button::new(tr!("varachat-call")))
                        .clicked()
                        || enter)
                        && !call.is_empty()
                    {
                        cmds.push(Command::VaraChatCall { call, offset_hz: 0 });
                    }
                    if ui
                        .button(tr!("varachat-cq"))
                        .on_hover_text(tr!("varachat-cq-hover"))
                        .clicked()
                    {
                        cmds.push(Command::VaraChatCq);
                    }
                }
                ChatState::Off => {
                    ui.label(RichText::new(tr!("varachat-off-hint")).weak());
                }
            }
            if ui.button(tr!("varachat-clear")).clicked() {
                cmds.push(Command::VaraChatClear);
            }
        });
        if !st.message.is_empty() {
            ui.colored_label(crate::theme::ALERT(), &st.message);
        }

        // Heard CQs, while standing by: click one to call it.
        if st.state == ChatState::Standby && !st.heard.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr!("varachat-heard")).size(10.5).weak());
                let now = crate::time::now_unix();
                for cq in st.heard.iter().take(12) {
                    let age = now - cq.at;
                    let mut label = cq.call.clone();
                    if cq.beacon {
                        label.push_str(&tr!("varachat-beacon"));
                    } else if cq.offset_hz != 0 {
                        label.push_str(&format!(" {:+}", cq.offset_hz));
                    }
                    // VarAC waits on its slot for about two minutes after a CQ.
                    let fresh = age < 120;
                    let resp = crate::chrome::chip(ui, false, RichText::new(label).size(10.5))
                        .on_hover_text(tr!(
                            "varachat-heard-hover",
                            raw = cq.raw.clone(),
                            age = age.max(0)
                        ));
                    if resp.clicked() && !cq.beacon {
                        let offset_hz = if fresh { cq.offset_hz } else { 0 };
                        cmds.push(Command::VaraChatCall { call: cq.call.clone(), offset_hz });
                    }
                }
            });
        }
        ui.separator();

        // The conversation, following the newest line.
        let input_h = 34.0;
        let avail = ui.available_height();
        let body_h =
            if avail.is_finite() && avail > input_h + 40.0 { avail - input_h } else { 300.0 };
        egui::ScrollArea::vertical()
            .id_salt("vara-chat-lines")
            .max_height(body_h)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if st.lines.is_empty() {
                    ui.label(RichText::new(tr!("varachat-empty")).weak());
                }
                for l in &st.lines {
                    let (_, _, _, h, mi, _) = sdroxide_types::utc_ymd_hms(l.at);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!("{h:02}:{mi:02}"))
                                .monospace()
                                .size(10.0)
                                .color(crate::theme::CYAN_DIM()),
                        );
                        let text = line_words(l);
                        match l.kind {
                            ChatLineKind::Me => {
                                ui.label(RichText::new(text).color(crate::theme::GREEN()));
                            }
                            ChatLineKind::Them => {
                                ui.label(RichText::new(text).color(crate::theme::TEXT_STRONG()));
                            }
                            ChatLineKind::Note => {
                                ui.label(RichText::new(text).italics().weak());
                            }
                        }
                    });
                }
            });

        // The line to send.
        let connected = st.state == ChatState::Connected;
        ui.horizontal(|ui| {
            let resp = ui.add_enabled(
                connected,
                egui::TextEdit::singleline(&mut self.mail.chat_input)
                    .desired_width((ui.available_width() - 80.0).max(120.0))
                    .hint_text(tr!("varachat-type")),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let text = self.mail.chat_input.trim().to_string();
            if (ui
                .add_enabled(connected && !text.is_empty(), egui::Button::new(tr!("varachat-send")))
                .clicked()
                || enter)
                && connected
                && !text.is_empty()
            {
                cmds.push(Command::VaraChatSend(text));
                self.mail.chat_input.clear();
                resp.request_focus();
            }
        });
    }
}

/// A conversation line as shown: what was said, and what its tags or the link
/// said, in the viewer's language.
fn line_words(l: &ChatLine) -> String {
    if l.said.is_empty() {
        return l.text.clone();
    }
    // Link events are whole sentences; a station's tags read after its name.
    let (events, tags): (Vec<&ChatSaid>, Vec<&ChatSaid>) =
        l.said.iter().partition(|s| event(s).is_some());
    let mut parts: Vec<String> = events.iter().filter_map(|s| event(s)).collect();
    if !tags.is_empty() {
        let what = tags.iter().map(|s| tag_words(s)).collect::<Vec<_>>().join("; ");
        if l.text.trim().is_empty() {
            parts.push(tr!("varachat-said-by", call = l.from.clone(), what = what));
        } else {
            parts.push(format!("{} ({what})", l.text.trim_end()));
        }
    } else if !l.text.trim().is_empty() {
        parts.insert(0, l.text.clone());
    }
    parts.join(" ")
}

/// What happened on the link, as a sentence.
fn event(s: &ChatSaid) -> Option<String> {
    Some(match s {
        ChatSaid::Calling(c) => tr!("varachat-ev-calling", call = c.clone()),
        ChatSaid::CallStopped(c) => tr!("varachat-ev-call-stopped", call = c.clone()),
        ChatSaid::ConnectedTo(c) => tr!("varachat-ev-connected", call = c.clone()),
        ChatSaid::Called(c) => tr!("varachat-ev-called", call = c.clone()),
        ChatSaid::LinkClosed(c) => tr!("varachat-ev-closed", call = c.clone()),
        ChatSaid::NoAnswer(c) => tr!("varachat-ev-no-answer", call = c.clone()),
        ChatSaid::PlainPeer(c) => tr!("varachat-ev-plain", call = c.clone()),
        ChatSaid::CqSent => tr!("varachat-ev-cq-sent"),
        _ => return None,
    })
}

/// What one of a station's tags says about it, to follow its callsign.
fn tag_words(s: &ChatSaid) -> String {
    match s {
        ChatSaid::Report(db) => tr!("varachat-tag-report", db = *db),
        ChatSaid::Name(v) => tr!("varachat-tag-name", value = v.clone()),
        ChatSaid::Qth(v) => tr!("varachat-tag-qth", value = v.clone()),
        ChatSaid::Locator(v) => tr!("varachat-tag-locator", value = v.clone()),
        ChatSaid::Email(v) => tr!("varachat-tag-email", value = v.clone()),
        ChatSaid::Offers(letters) => {
            let what = letters
                .chars()
                .map(|c| match c {
                    'V' => "VMail".to_string(),
                    'E' => tr!("varachat-svc-email"),
                    'A' => "AI".to_string(),
                    'B' => "BBS".to_string(),
                    'N' => tr!("varachat-svc-nothing"),
                    _ => "?".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            tr!("varachat-tag-offers", what = what)
        }
        ChatSaid::QsyBy(hz) => tr!("varachat-tag-qsy-by", hz = format!("{hz:+}")),
        ChatSaid::QsyTo(hz) => tr!("varachat-tag-qsy-to", hz = hz.clone()),
        ChatSaid::IsOn(hz) => tr!("varachat-tag-is-on", hz = hz.clone()),
        ChatSaid::Closing => tr!("varachat-tag-closing"),
        ChatSaid::Away => tr!("varachat-tag-away"),
        ChatSaid::AwayVmail => tr!("varachat-tag-away-vmail"),
        ChatSaid::AtStation => tr!("varachat-tag-at-station"),
        ChatSaid::Arrived => tr!("varachat-tag-arrived"),
        ChatSaid::ReadyNew => tr!("varachat-tag-ready"),
        ChatSaid::QsyUp => tr!("varachat-tag-qsy-up"),
        ChatSaid::QsyDown => tr!("varachat-tag-qsy-down"),
        // Events are worded by `event`.
        other => event(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, said: Vec<ChatSaid>) -> ChatLine {
        ChatLine { at: 0, kind: ChatLineKind::Note, text: text.into(), from: "EA5XYZ".into(), said }
    }

    #[test]
    fn tags_and_events_are_put_into_words() {
        assert_eq!(line_words(&line("hola", vec![])), "hola");
        assert_eq!(
            line_words(&line("", vec![ChatSaid::Report(-3), ChatSaid::Name("Ana".into())])),
            "EA5XYZ hears you at -3 dB; name Ana"
        );
        assert_eq!(line_words(&line("73", vec![ChatSaid::Closing])), "73 (is closing the QSO)");
        assert_eq!(
            line_words(&line("", vec![ChatSaid::ConnectedTo("F6JON".into())])),
            "Connected to F6JON."
        );
    }
}

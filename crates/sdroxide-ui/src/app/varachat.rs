//! The VARA CHAT window: keyboard chat over a VARA link.
//!
//! A conversation pane, a line to type into, and above them who the QSO is
//! with — or, standing by, a callsign to call and the CQs heard. Everything
//! happens on the engine's machine (the TNC is there); this only draws the
//! status the engine sends and turns clicks into commands.

use crate::i18n::tr;
use eframe::egui::{self, RichText};
use sdroxide_types::{ChatLineKind, ChatState, Command};

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
                        match l.kind {
                            ChatLineKind::Me => {
                                ui.label(RichText::new(&l.text).color(crate::theme::GREEN()));
                            }
                            ChatLineKind::Them => {
                                ui.label(RichText::new(&l.text).color(crate::theme::TEXT_STRONG()));
                            }
                            ChatLineKind::Note => {
                                ui.label(RichText::new(&l.text).italics().weak());
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

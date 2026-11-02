//! The VARA-MON panel: VARA HF traffic reported by the operator's own
//! `xara-monitor`.
//!
//! Two panes. TRAFFIC is the record-by-record log — connect requests, speed
//! levels, the session header naming both ends, how much each data frame
//! carried, the link's end. STATIONS is who has been heard on the channel,
//! with the station each was last linked to and the best signal it came in at:
//! on a gateway frequency, the answer to "who is reachable from here".
//!
//! Above both, the monitor's state and its settings. sdroxide does not include
//! a VARA decoder, so until the operator points the panel at an `xara-monitor`
//! the setup is all there is to show, and it says where to get one.

use crate::i18n::tr;
use eframe::egui::{self, Color32, RichText};
use sdroxide_types::{
    Command, DigiStatus, VaraBandwidth, VaraMonKind, VaraMonState, VaraMonStatus,
};

use crate::app::SdroxideApp;
use crate::theme;

/// Where the program comes from. Shown in the setup, not linked: the panel may
/// be running in a browser on another machine than the one that will run it.
const XARA_URL: &str = "https://github.com/x0rro/xara";

fn hms(at: i64) -> String {
    let (_, _, _, h, mi, s) = sdroxide_types::utc_ymd_hms(at);
    format!("{h:02}:{mi:02}:{s:02}")
}

fn kind_color(k: VaraMonKind) -> Color32 {
    match k {
        VaraMonKind::Connect => theme::YELLOW(),
        VaraMonKind::Header | VaraMonKind::LinkEnd => theme::GREEN(),
        VaraMonKind::Data => theme::CYAN(),
        VaraMonKind::Payload | VaraMonKind::Info => theme::CYAN_DIM(),
        VaraMonKind::Cq | VaraMonKind::Kiss | VaraMonKind::Unproto => theme::PINK(),
        VaraMonKind::Control => theme::TEXT(),
    }
}

fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

impl SdroxideApp {
    pub(in crate::app) fn vara_mon_panel(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        panel_h: f32,
    ) {
        let st: VaraMonStatus =
            self.digi_status.as_ref().and_then(|s| s.vara_mon.clone()).unwrap_or_default();

        self.vara_mon_header(ui, cmds, &st);
        if st.state == VaraMonState::NotConfigured || self.vara_mon_setup_open {
            ui.separator();
            self.vara_mon_setup(ui, cmds, &st);
        }
        ui.separator();

        let pane = self.phone_pane(ui, sdroxide_types::Mode::VaraMon);
        let body_h = (panel_h - 90.0).max(80.0);
        ui.horizontal(|ui| {
            if pane.is_none_or(|p| p == 0) {
                ui.vertical(|ui| {
                    if pane.is_none() {
                        ui.set_width(ui.available_width() * 0.62);
                    }
                    vara_mon_traffic(ui, &st, body_h);
                });
            }
            if pane.is_none() {
                ui.separator();
            }
            if pane.is_none_or(|p| p == 1) {
                ui.vertical(|ui| {
                    vara_mon_stations(ui, &st, body_h);
                });
            }
        });
    }

    /// State, meter, counters, the link on the air, and the row of chips.
    fn vara_mon_header(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, st: &VaraMonStatus) {
        let status: Option<&DigiStatus> = self.digi_status.as_ref();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("VARA-MON").strong().color(theme::CYAN()));
            ui.label(RichText::new(tr!("varamon-beta")).size(9.5).color(theme::YELLOW()))
                .on_hover_text(tr!("varamon-beta-hover"));
            let (text, color) = match st.state {
                VaraMonState::NotConfigured => (tr!("varamon-state-not-set-up"), theme::YELLOW()),
                VaraMonState::Starting => (tr!("varamon-state-starting"), theme::YELLOW()),
                VaraMonState::Running => {
                    (tr!("varamon-state-running", version = st.version.clone()), theme::GREEN())
                }
                VaraMonState::Failed => (tr!("varamon-state-failed"), theme::ALERT()),
            };
            ui.label(RichText::new(text).size(10.5).color(color));
            ui.add(
                egui::ProgressBar::new(st.level.clamp(0.0, 1.0) * 3.0)
                    .desired_width(60.0)
                    .fill(theme::CYAN_DIM()),
            )
            .on_hover_text(tr!("varamon-level-hover"));
            ui.label(
                RichText::new(tr!(
                    "varamon-counts",
                    links = st.links,
                    transmissions = st.transmissions
                ))
                .size(10.0)
                .color(theme::CYAN_DIM()),
            );
            if st.dropped_blocks > 0 {
                ui.label(
                    RichText::new(tr!("varamon-dropped", n = st.dropped_blocks))
                        .size(10.0)
                        .color(theme::YELLOW()),
                )
                .on_hover_text(tr!("varamon-dropped-hover"));
            }
            super::clear_rx_chip_at(ui, cmds, !st.lines.is_empty() || !st.stations.is_empty());
            super::save_rx_chip_for(ui, status);
            if crate::chrome::chip(
                ui,
                self.vara_mon_setup_open,
                RichText::new(tr!("varamon-setup")),
            )
            .on_hover_text(tr!("varamon-setup-hover"))
            .clicked()
            {
                self.vara_mon_setup_open = !self.vara_mon_setup_open;
            }
        });
        if !st.message.is_empty() {
            let color =
                if st.state == VaraMonState::Failed { theme::ALERT() } else { theme::YELLOW() };
            ui.label(RichText::new(&st.message).size(10.5).color(color));
        }
        if let Some(link) = &st.link {
            let name = |c: &str| if c.is_empty() { "?".to_string() } else { c.to_string() };
            let now = crate::time::now_unix();
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(tr!("varamon-on-air")).size(10.0).strong().color(theme::YELLOW()),
                );
                ui.label(
                    RichText::new(format!("{} ⇄ {}", name(&link.caller), name(&link.answerer)))
                        .monospace()
                        .strong()
                        .color(theme::CYAN()),
                );
                ui.label(
                    RichText::new(tr!(
                        "varamon-link-detail",
                        level = link.level,
                        sent = link.bytes[0],
                        back = link.bytes[1],
                        time = duration(now - link.started_at)
                    ))
                    .size(10.0)
                    .color(theme::CYAN_DIM()),
                );
            });
        }
    }

    /// Where the monitor is and how to run it.
    fn vara_mon_setup(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>, st: &VaraMonStatus) {
        if st.state == VaraMonState::NotConfigured {
            ui.label(RichText::new(tr!("varamon-intro")).size(10.5));
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr!("varamon-get-it")).size(10.5));
                ui.label(RichText::new(XARA_URL).monospace().size(10.5).color(theme::CYAN()));
            });
            ui.add_space(4.0);
        }
        let edit = &mut self.vara_mon_edit;
        egui::Grid::new("varamon-setup").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new(tr!("varamon-program")).size(10.5));
            crate::chrome::field(
                ui,
                egui::TextEdit::singleline(&mut edit.exe)
                    .desired_width(300.0)
                    .hint_text("/usr/local/bin/xara-monitor"),
            )
            .on_hover_text(tr!("varamon-program-hover"));
            ui.end_row();

            ui.label(RichText::new(tr!("varamon-bandwidth")).size(10.5));
            ui.horizontal(|ui| {
                for bw in VaraBandwidth::ALL {
                    if crate::chrome::chip(
                        ui,
                        edit.bandwidth == bw,
                        RichText::new(format!("{} Hz", bw.hz())),
                    )
                    .clicked()
                    {
                        edit.bandwidth = bw;
                    }
                }
            });
            ui.end_row();

            ui.label(RichText::new(tr!("varamon-calls")).size(10.5));
            let resp = crate::chrome::field(
                ui,
                egui::TextEdit::singleline(&mut edit.calls)
                    .desired_width(300.0)
                    .hint_text(tr!("varamon-calls-hint")),
            )
            .on_hover_text(tr!("varamon-calls-hover"));
            if resp.changed() {
                edit.calls = edit.calls.to_uppercase();
            }
            ui.end_row();

            ui.label("");
            crate::chrome::checkbox(ui, &mut edit.blind, tr!("varamon-blind"))
                .on_hover_text(tr!("varamon-blind-hover"));
            ui.end_row();

            ui.label("");
            crate::chrome::checkbox(ui, &mut edit.show_payload, tr!("varamon-show-payload"))
                .on_hover_text(tr!("varamon-show-payload-hover"));
            ui.end_row();

            ui.label(RichText::new(tr!("varamon-extra-args")).size(10.5));
            crate::chrome::field(
                ui,
                egui::TextEdit::singleline(&mut edit.extra_args).desired_width(300.0),
            )
            .on_hover_text(tr!("varamon-extra-args-hover"));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let label = if st.state == VaraMonState::Running {
                tr!("varamon-apply")
            } else {
                tr!("varamon-start")
            };
            if crate::chrome::chip(ui, false, RichText::new(label).strong())
                .on_hover_text(tr!("varamon-apply-hover"))
                .clicked()
            {
                cmds.push(Command::SetVaraMonConfig(Box::new(self.vara_mon_edit.clone())));
            }
        });
    }
}

/// The record-by-record log, oldest at the top, following the newest.
fn vara_mon_traffic(ui: &mut egui::Ui, st: &VaraMonStatus, h: f32) {
    egui::ScrollArea::vertical()
        .id_salt("varamon-traffic")
        .max_height(h)
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if st.lines.is_empty() {
                let msg = match st.state {
                    VaraMonState::Running | VaraMonState::Starting => tr!("varamon-quiet"),
                    _ => tr!("varamon-not-running"),
                };
                ui.label(RichText::new(msg).weak());
                return;
            }
            for l in &st.lines {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(hms(l.at)).monospace().size(10.0).color(theme::CYAN_DIM()),
                    );
                    let snr =
                        l.snr_db.map(|s| format!("{s:+5.1}")).unwrap_or_else(|| "     ".into());
                    ui.label(RichText::new(snr).monospace().size(10.0).color(theme::CYAN_DIM()));
                    ui.label(
                        RichText::new(l.text.trim())
                            .monospace()
                            .size(10.5)
                            .color(kind_color(l.kind)),
                    );
                });
                if let Some(p) = &l.payload {
                    egui::Frame::new()
                        .fill(theme::ROW_BG())
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            ui.label(RichText::new(p).monospace().size(10.5));
                        });
                }
            }
        });
}

/// Who has been heard, most recent first.
fn vara_mon_stations(ui: &mut egui::Ui, st: &VaraMonStatus, h: f32) {
    egui::ScrollArea::vertical()
        .id_salt("varamon-stations")
        .max_height(h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if st.stations.is_empty() {
                ui.label(RichText::new(tr!("varamon-no-stations")).weak());
                return;
            }
            egui::Grid::new("varamon-stations-grid").striped(true).spacing([10.0, 2.0]).show(
                ui,
                |ui| {
                    for head in [
                        tr!("varamon-col-call"),
                        tr!("varamon-col-last"),
                        tr!("varamon-col-snr"),
                        tr!("varamon-col-level"),
                        tr!("varamon-col-peer"),
                        tr!("varamon-col-links"),
                    ] {
                        ui.label(RichText::new(head).size(9.5).weak());
                    }
                    ui.end_row();
                    for s in &st.stations {
                        let mut call =
                            RichText::new(&s.call).monospace().strong().color(theme::CYAN());
                        if s.cq {
                            call = call.color(theme::PINK());
                        }
                        let r = ui.label(call);
                        if s.cq {
                            r.on_hover_text(tr!("varamon-heard-cq"));
                        }
                        ui.label(RichText::new(hms(s.last_at)).monospace().size(10.0));
                        let snr = match (s.last_snr_db, s.best_snr_db) {
                            (Some(l), Some(b)) => format!("{l:+.0} / {b:+.0}"),
                            _ => "—".into(),
                        };
                        ui.label(RichText::new(snr).monospace().size(10.0))
                            .on_hover_text(tr!("varamon-snr-hover"));
                        let level = if s.top_level > 0 {
                            format!("L{}", s.top_level)
                        } else {
                            "—".into()
                        };
                        ui.label(RichText::new(level).monospace().size(10.0));
                        ui.label(RichText::new(&s.peer).monospace().size(10.0));
                        ui.label(RichText::new(s.links.to_string()).monospace().size(10.0));
                        ui.end_row();
                    }
                },
            );
        });
}

//! FT8/FT4/FT2 **chase**: answer, by itself, a station not yet worked once it
//! has been heard steadily for a while.
//!
//! The judgement is [`sdroxide_types::Chaser`]'s, made here on the screen
//! because the logbook it is made against is the screen's. What leaves for the
//! station is a single [`Command::DigiChaseQso`], which the engine refuses
//! unless its sequencer is free — so a chase never cuts into the operator's own
//! contact, CQ run or call queue — and which never counts as the operator
//! acting, so the transmit watchdog still stops a chase nobody answers.
//!
//! Per radio tab: each tab runs its own sequencer on its own band. The options
//! are the station's and shared; whether chase is on is never remembered.

use crate::i18n::{Tr, tr};
use eframe::egui::{self, RichText};
use sdroxide_types::{
    CHASE_ANY_SNR, ChaseAnswer, ChaseCtx, ChaseNeed, ChaseOrder, Command, Decode, DigiStatus,
    DxpedMode, Mode, QsoStep, parse_chase_list,
};

use crate::app::SdroxideApp;
use crate::app::persist::persist_chase_settings;

/// True when the sequencer has nothing of the operator's in hand and a chase
/// may start: idle, or a finished contact only waiting out its re-send window,
/// nothing queued, the watchdog down, and not the Fox. The engine asks the same
/// question again before it acts (`QsoMachine::can_chase`); asking it here too
/// keeps a refused command from marking a station as chased.
pub(in crate::app) fn sequencer_free(s: &DigiStatus) -> bool {
    matches!(s.mode, Mode::Ft8 | Mode::Ft4 | Mode::Ft2)
        && !s.tx_watchdog
        && !s.transmitting
        && s.call_queue.is_empty()
        && s.config.dxped_mode != DxpedMode::Fox
        && !s.config.my_call.trim().is_empty()
        && match s.step {
            QsoStep::Idle => true,
            QsoStep::Confirming => !s.tx_next,
            _ => false,
        }
}

impl SdroxideApp {
    /// Fold a batch of decodes into the chase, and call the best station in it
    /// when chase is on and the sequencer is free.
    pub(in crate::app) fn chase_on_decodes(&mut self, batch: &[Decode]) {
        let Some(st) = self.digi_status.clone() else { return };
        if !matches!(st.mode, Mode::Ft8 | Mode::Ft4 | Mode::Ft2) || batch.is_empty() {
            return;
        }
        let dial_hz = self.state.active_freq_hz();
        let band = if dial_hz > 0.0 { sdroxide_types::adif_band(dial_hz) } else { "" };
        let now = batch.iter().map(|d| d.slot_utc).max().unwrap_or_default();
        // Taken out for the length of this call: the novelty index borrows the
        // whole app, and cloning it every slot would copy the logbook's sets.
        let mut chaser = std::mem::take(&mut self.chaser);
        chaser.observe(batch, band);
        let settings = self.chase.clone();
        let may_call =
            settings.enabled && !band.is_empty() && self.tx_capable() && sequencer_free(&st);
        let log = self.log_index();
        let ctx = ChaseCtx { my_call: &st.config.my_call, my_grid: &st.config.my_grid, band, now };
        let tally = chaser.tally(&settings, log, &ctx);
        let pick = may_call.then(|| chaser.pick(batch, &settings, log, &ctx)).flatten();
        if let Some(p) = &pick {
            chaser.mark_tried(&p.call, band, now);
        }
        self.chaser = chaser;
        self.chase_tally = tally;
        if let Some(p) = pick {
            self.chase_last = Some((p.call.clone(), p.heard));
            self.ctrl.send(Command::DigiChaseQso {
                from: p.call,
                grid: p.grid,
                snr: p.snr_db,
                audio_hz: p.audio_hz,
                wait_for_cq: p.wait_for_cq,
            });
        }
    }

    /// Switch chase on or off on this tab.
    pub(in crate::app) fn chase_set_enabled(&mut self, on: bool) {
        self.chase.enabled = on;
        if !on {
            self.chase_last = None;
        }
    }

    /// The CHASE chip and its options chip, for the QSO action row.
    pub(in crate::app) fn chase_chips(&mut self, ui: &mut egui::Ui, tx_ok: bool) {
        let on = self.chase.enabled;
        let resp = ui.add_enabled_ui(tx_ok, |ui| {
            crate::chrome::chip_accent(
                ui,
                on,
                RichText::new(tr!("chase-chip")).size(15.0).strong(),
                crate::theme::YELLOW(),
                crate::theme::INK_ON_BRIGHT(),
            )
        });
        if resp
            .inner
            .on_hover_text(tr!(
                "chase-chip-hint",
                need = self.chase.need.tr(),
                heard = self.chase.min_heard,
                minutes = self.chase.window_min
            ))
            .clicked()
        {
            self.chase_set_enabled(!on);
        }
        if crate::chrome::chip(ui, self.show_chase_settings, RichText::new("⚙").size(14.0))
            .on_hover_text(tr!("chase-options"))
            .clicked()
        {
            self.show_chase_settings = !self.show_chase_settings;
        }
    }

    /// One line saying what the chase is doing, shown while it is on.
    pub(in crate::app) fn chase_readout(&mut self, ui: &mut egui::Ui) {
        if !self.chase.enabled {
            return;
        }
        let t = &self.chase_tally;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(tr!("chase-label")).size(9.5).strong().color(crate::theme::YELLOW()),
            );
            if let Some((call, n)) = &self.chase_last {
                ui.label(
                    RichText::new(tr!("chase-last", call = call.as_str(), heard = *n))
                        .size(11.0)
                        .color(crate::theme::GREEN()),
                );
            }
            if let Some((call, n)) = &t.next {
                ui.label(
                    RichText::new(tr!(
                        "chase-next",
                        call = call.as_str(),
                        heard = *n,
                        need = self.chase.min_heard
                    ))
                    .size(11.0)
                    .color(crate::theme::TEXT()),
                );
            }
            ui.label(
                RichText::new(tr!(
                    "chase-tally",
                    building = t.building,
                    eligible = t.eligible,
                    busy = t.busy,
                    worked = t.not_needed,
                    weak = t.weak,
                    resting = t.resting,
                    excluded = t.excluded
                ))
                .size(11.0)
                .color(crate::theme::gray(140)),
            )
            .on_hover_text(tr!("chase-tally-hint"));
        });
    }

    /// The chase options window.
    pub(in crate::app) fn chase_window(&mut self, ctx: &egui::Context) {
        if !self.show_chase_settings {
            return;
        }
        let mut open = true;
        let before = self.chase.clone();
        egui::Window::new(tr!("chase-window-title"))
            .id(crate::layout::salted_id(ctx, "FT8 Chase"))
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(false)
            .default_width(crate::layout::window_w(ctx, 400.0))
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                let s = &mut self.chase;
                ui.label(RichText::new(tr!("chase-window-intro")).weak());
                ui.add_space(6.0);
                egui::Grid::new("chase-cfg").num_columns(2).show(ui, |ui| {
                    ui.label(tr!("chase-need"));
                    egui::ComboBox::from_id_salt("chase-need")
                        .selected_text(s.need.tr())
                        .show_ui(ui, |ui| {
                            for n in ChaseNeed::ALL {
                                ui.selectable_value(&mut s.need, n, n.tr());
                            }
                        })
                        .response
                        .on_hover_text(tr!("chase-need-hint"));
                    ui.end_row();

                    ui.label(tr!("chase-answer"));
                    egui::ComboBox::from_id_salt("chase-answer")
                        .selected_text(s.answer.tr())
                        .show_ui(ui, |ui| {
                            for a in ChaseAnswer::ALL {
                                ui.selectable_value(&mut s.answer, a, a.tr());
                            }
                        })
                        .response
                        .on_hover_text(tr!("chase-answer-hint"));
                    ui.end_row();

                    ui.label(tr!("chase-heard"));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut s.min_heard).range(1..=20))
                            .on_hover_text(tr!("chase-heard-hint"));
                        ui.label(tr!("chase-heard-within"));
                        ui.add(
                            egui::DragValue::new(&mut s.window_min).range(1..=60).suffix(" min"),
                        );
                    });
                    ui.end_row();

                    ui.label(tr!("chase-min-snr"));
                    ui.add(
                        egui::DragValue::new(&mut s.min_snr_db)
                            .range(CHASE_ANY_SNR..=10)
                            .custom_formatter(|v, _| {
                                if v as i16 <= CHASE_ANY_SNR {
                                    tr!("chase-any-signal").to_string()
                                } else {
                                    format!("{v:+.0} dB")
                                }
                            }),
                    )
                    .on_hover_text(tr!("chase-min-snr-hint"));
                    ui.end_row();

                    ui.label(tr!("chase-dx-only"));
                    crate::chrome::checkbox(ui, &mut s.dx_only, "")
                        .on_hover_text(tr!("chase-dx-only-hint"));
                    ui.end_row();

                    ui.label(tr!("chase-order"));
                    ui.horizontal(|ui| {
                        for o in ChaseOrder::ALL {
                            ui.selectable_value(&mut s.order, o, o.tr());
                        }
                    });
                    ui.end_row();

                    ui.label(tr!("chase-retry"));
                    ui.add(
                        egui::DragValue::new(&mut s.retry_after_min).range(0..=240).suffix(" min"),
                    )
                    .on_hover_text(tr!("chase-retry-hint"));
                    ui.end_row();

                    ui.label(tr!("chase-watch"));
                    if crate::chrome::field(
                        ui,
                        egui::TextEdit::singleline(&mut self.chase_watch_text)
                            .hint_text("3Y0J, VP8*"),
                    )
                    .on_hover_text(tr!("chase-watch-hint"))
                    .changed()
                    {
                        s.watch = parse_chase_list(&self.chase_watch_text);
                    }
                    ui.end_row();

                    ui.label(tr!("chase-ignore"));
                    if crate::chrome::field(
                        ui,
                        egui::TextEdit::singleline(&mut self.chase_ignore_text)
                            .hint_text("VE3*, K1ABC"),
                    )
                    .on_hover_text(tr!("chase-ignore-hint"))
                    .changed()
                    {
                        s.ignore = parse_chase_list(&self.chase_ignore_text);
                    }
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(RichText::new(tr!("chase-safety-note")).weak().size(11.0));
            });
        if self.chase != before {
            persist_chase_settings(&self.chase);
        }
        self.show_chase_settings = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(step: QsoStep) -> DigiStatus {
        let cfg = sdroxide_types::DigiConfig { my_call: "EA4ABC".into(), ..Default::default() };
        DigiStatus { mode: Mode::Ft8, step, ..DigiStatus::idle(cfg) }
    }

    #[test]
    fn chase_waits_for_the_operator_s_work_to_finish() {
        assert!(sequencer_free(&status(QsoStep::Idle)));
        for busy in [QsoStep::CallingCq, QsoStep::TxGrid, QsoStep::TxReport, QsoStep::WaitCq] {
            assert!(!sequencer_free(&status(busy)), "{busy:?}");
        }
        let mut s = status(QsoStep::Confirming);
        assert!(sequencer_free(&s));
        s.tx_next = true; // a re-send of the final message is owed
        assert!(!sequencer_free(&s));
        let mut s = status(QsoStep::Idle);
        s.tx_watchdog = true;
        assert!(!sequencer_free(&s));
        let mut s = status(QsoStep::Idle);
        s.mode = Mode::Js8;
        assert!(!sequencer_free(&s));
    }
}

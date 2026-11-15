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
    CHASE_ANY_SNR, ChaseAnswer, ChaseCtx, ChaseDupe, ChaseNeed, ChaseOrder, Command, Decode,
    DigiStatus, DxpedMode, Mode, QsoStep, parse_chase_list,
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

/// How long a chase command waits for the station's status to show it taken
/// up before it is counted as refused. Several slots' worth of decodes can be
/// handled before that status arrives — a window the system is not drawing is
/// woken late, with a backlog — and each of them, read against the status from
/// before the command, would look free for another.
const CHASE_CONFIRM_S: f64 = 8.0;

/// A chase command sent to the station and not yet seen taken up.
#[derive(Debug, Clone)]
pub(in crate::app) struct ChaseSent {
    call: String,
    /// Answering a station that called us.
    caller: bool,
    band: String,
    sent_s: f64,
}

/// True when the sequencer holds only a contact the chase started, with
/// `chased`, that has not been answered yet — still sending them our grid, or
/// waiting for them to come free. A station calling us may take its place. The
/// engine asks the same question again (`QsoMachine::can_chase_caller`).
pub(in crate::app) fn chase_unanswered(s: &DigiStatus, chased: Option<&str>) -> bool {
    matches!(s.mode, Mode::Ft8 | Mode::Ft4 | Mode::Ft2)
        && !s.tx_watchdog
        && s.call_queue.is_empty()
        && s.config.dxped_mode != DxpedMode::Fox
        && !s.config.my_call.trim().is_empty()
        && matches!(s.step, QsoStep::TxGrid | QsoStep::WaitCq)
        && s.qso.as_ref().is_none_or(|q| q.rpt_rcvd.is_none())
        && chased.is_some_and(|c| s.dx_call.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(c)))
}

/// True when the contact in hand is the one the chase last started, `chased`,
/// and it is still going: something SKIP can drop. A finished contact, a CQ
/// run, or a station the operator picked themselves is not the chase's to skip.
pub(in crate::app) fn chase_skippable(s: &DigiStatus, chased: Option<&str>) -> bool {
    matches!(s.mode, Mode::Ft8 | Mode::Ft4 | Mode::Ft2)
        && !matches!(s.step, QsoStep::Idle | QsoStep::Confirming | QsoStep::CallingCq)
        && chased.is_some_and(|c| s.dx_call.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(c)))
}

impl SdroxideApp {
    /// Fold a batch of decodes into the chase, and call the best station in it
    /// when chase is on and the sequencer is free.
    ///
    /// A station calling us comes first, whatever the chase's filters say, and
    /// may also take the place of a chased contact nobody has answered yet.
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
        let can_tx = settings.enabled && !band.is_empty() && self.tx_capable();
        if self
            .chase_pending
            .as_ref()
            .is_some_and(|p| crate::time::now_unix_f64() - p.sent_s > CHASE_CONFIRM_S)
        {
            self.chase_pending = None;
        }
        let pending = self.chase_pending.as_ref().map(|p| p.caller);
        // A command still on its way counts as the contact it asked for: the
        // status in hand predates it.
        let free = sequencer_free(&st) && pending.is_none();
        let may_call = can_tx && free;
        // Not `free` alone: a call to us arrives in the slot we answer in, so
        // while we are calling a station in that same period the batch lands
        // with our burst on the air, every time. Only a chase that *called*
        // somebody gives way; one answering a caller is a contact under way.
        let calling =
            self.chase_own.as_ref().filter(|(_, caller)| !caller).map(|(c, _)| c.as_str());
        let unanswered = match pending {
            Some(caller) => !caller,
            None => chase_unanswered(&st, calling),
        };
        let may_answer = can_tx && (free || unanswered);
        let log = self.log_index();
        // The contest the sequencer will really run: EU VHF only with the
        // six-character locator its exchange needs (`QsoMachine::contest`).
        let contest =
            if st.config.my_grid.trim().len() == 6 { st.config.contest.cq_word() } else { "" };
        let ctx = ChaseCtx {
            my_call: &st.config.my_call,
            my_grid: &st.config.my_grid,
            band,
            mode: st.mode.label(),
            contest,
            now,
        };
        let tally = chaser.tally(&settings, log, &ctx);
        let caller = may_answer
            .then(|| chaser.pick_caller(&settings, log, &ctx, st.dx_call.as_deref()))
            .flatten();
        let is_caller = caller.is_some();
        let pick =
            caller.or_else(|| may_call.then(|| chaser.pick(batch, &settings, log, &ctx)).flatten());
        self.chaser = chaser;
        self.chase_tally = tally;
        if let Some(p) = pick {
            // Marked as tried only once the station takes it up (see
            // `chase_on_status`): a command it refuses — the status here was
            // a step behind — must not leave a station that called us resting.
            self.chase_pending = Some(ChaseSent {
                call: p.call.clone(),
                caller: is_caller,
                band: band.to_string(),
                sent_s: crate::time::now_unix_f64(),
            });
            self.chase_last = Some((p.call.clone(), p.heard));
            self.ctrl.send(Command::DigiChaseQso {
                from: p.call,
                grid: p.grid,
                snr: p.snr_db,
                audio_hz: p.audio_hz,
                wait_for_cq: p.wait_for_cq,
                caller: is_caller,
                max_calls: settings.max_calls,
            });
        }
    }

    /// Follow a new status from the station: a chase command it took up, and
    /// a chased contact it no longer holds.
    pub(in crate::app) fn chase_on_status(&mut self) {
        let Some(st) = self.digi_status.as_ref() else { return };
        let holds = |call: &str| {
            st.step != QsoStep::Idle
                && st.dx_call.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(call))
        };
        if let Some(p) = self.chase_pending.take_if(|p| holds(&p.call)) {
            self.chaser.mark_tried(&p.call, &p.band, crate::time::now_unix());
            self.chase_own = Some((p.call, p.caller));
            return;
        }
        if self
            .chase_own
            .as_ref()
            .is_some_and(|(c, _)| !st.dx_call.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(c)))
        {
            self.chase_own = None;
        }
    }

    /// Drop the chased contact in hand and let the chase move on. The station
    /// rests for the configured time from now, as one that never answered
    /// would, and the next decoded slot picks the next one. Sent straight to
    /// the station rather than as a STOP QSO through the screen's commands,
    /// which would switch the chase off.
    pub(in crate::app) fn chase_skip(&mut self) {
        let Some(st) = self.digi_status.as_ref() else { return };
        let chased = self.chase_own.as_ref().map(|(c, _)| c.as_str());
        if !self.chase.enabled || !chase_skippable(st, chased) {
            return;
        }
        let Some(call) = st.dx_call.clone() else { return };
        let dial_hz = self.state.active_freq_hz();
        if dial_hz > 0.0 {
            let band = sdroxide_types::adif_band(dial_hz);
            self.chaser.mark_skipped(&call, band, crate::time::now_unix());
        }
        self.chase_own = None;
        self.ctrl.send(Command::DigiStopQso);
    }

    /// Switch chase on or off on this tab.
    pub(in crate::app) fn chase_set_enabled(&mut self, on: bool) {
        self.chase.enabled = on;
        if !on {
            self.chase_last = None;
            self.chase_pending = None;
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
        let chased = self.chase_own.as_ref().map(|(c, _)| c.as_str());
        let skippable =
            on && tx_ok && self.digi_status.as_ref().is_some_and(|st| chase_skippable(st, chased));
        if on {
            let skip = ui.add_enabled_ui(skippable, |ui| {
                crate::chrome::chip(ui, false, RichText::new(tr!("chase-skip")).size(14.0))
            });
            if skip.inner.on_hover_text(tr!("chase-skip-hint")).clicked() {
                self.chase_skip();
            }
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

                    ui.label(tr!("chase-dupe"));
                    egui::ComboBox::from_id_salt("chase-dupe")
                        .selected_text(s.dupe.tr())
                        .show_ui(ui, |ui| {
                            for d in ChaseDupe::ALL {
                                ui.selectable_value(&mut s.dupe, d, d.tr());
                            }
                        })
                        .response
                        .on_hover_text(tr!("chase-dupe-hint"));
                    ui.end_row();

                    ui.label(tr!("chase-max-calls"));
                    ui.add(egui::DragValue::new(&mut s.max_calls).range(1..=20))
                        .on_hover_text(tr!("chase-max-calls-hint"));
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

    #[test]
    fn a_caller_may_replace_only_an_unanswered_chase() {
        let mut s = status(QsoStep::TxGrid);
        s.dx_call = Some("JA1XYZ".into());
        assert!(chase_unanswered(&s, Some("ja1xyz")));
        // The operator's own contact, not the chase's.
        assert!(!chase_unanswered(&s, Some("VK2ABC")));
        assert!(!chase_unanswered(&s, None));
        // They answered: the contact is under way.
        s.qso = Some(sdroxide_types::QsoLive {
            started_utc: 0,
            rpt_sent: Some(-10),
            rpt_rcvd: Some(-12),
        });
        assert!(!chase_unanswered(&s, Some("JA1XYZ")));
        let mut s = status(QsoStep::TxRReport);
        s.dx_call = Some("JA1XYZ".into());
        assert!(!chase_unanswered(&s, Some("JA1XYZ")));
        let mut s = status(QsoStep::WaitCq);
        s.dx_call = Some("JA1XYZ".into());
        assert!(chase_unanswered(&s, Some("JA1XYZ")));
        s.tx_watchdog = true;
        assert!(!chase_unanswered(&s, Some("JA1XYZ")));
    }

    #[test]
    fn only_the_chase_s_own_running_contact_can_be_skipped() {
        let mut s = status(QsoStep::TxReport);
        s.dx_call = Some("JA1XYZ".into());
        assert!(chase_skippable(&s, Some("ja1xyz")));
        assert!(!chase_skippable(&s, Some("VK2ABC")), "the operator's own contact");
        assert!(!chase_skippable(&s, None));
        for done in [QsoStep::Idle, QsoStep::Confirming, QsoStep::CallingCq] {
            let mut s = status(done);
            s.dx_call = Some("JA1XYZ".into());
            assert!(!chase_skippable(&s, Some("JA1XYZ")), "{done:?}");
        }
        let mut s = status(QsoStep::WaitCq);
        s.dx_call = Some("JA1XYZ".into());
        assert!(chase_skippable(&s, Some("JA1XYZ")));
    }
}

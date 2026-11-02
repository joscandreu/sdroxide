//! Settings → Winlink, the VARA route: the TNC, the gateway, being called,
//! and xara run by sdroxide.
//!
//! Laid out in the order an operator meets the questions, with a setup check
//! on top. The check looks across settings that live in different places (the
//! Winlink account, sdroxide's own audio devices, the rigctld server, the
//! radio's mode) and says what is still missing, with a button where sdroxide
//! can put it right itself.

use crate::i18n::tr;
use eframe::egui::{self, RichText};
use sdroxide_types::{
    AudioDevices, ManagedTncConfig, ManagedTncStatus, Mode, TncDevice, TncMode, TncPtt, TncState,
    VaraBandwidth, VaraTncConfig,
};

use super::net::{net_heading, net_row};

/// What the setup check needs to know from outside the VARA settings.
pub(in crate::app) struct VaraEnv<'a> {
    /// The Winlink account's callsign.
    pub callsign: &'a str,
    /// The engine (and so the TNC) runs on this machine.
    pub local: bool,
    /// The main receiver's mode.
    pub mode: Mode,
    /// This machine's audio devices and sdroxide's picks among them.
    pub audio: Option<&'a AudioDevices>,
    /// The rigctld server is on (as edited).
    pub rigctld_on: bool,
}

/// What a setup-check button asks for that the VARA settings cannot do
/// themselves; the settings window carries these out.
#[derive(Debug, Default, PartialEq)]
pub(in crate::app) struct VaraFixes {
    /// sdroxide's audio output and microphone input, by device name.
    pub output: Option<String>,
    pub input: Option<String>,
    pub rigctld_on: bool,
    pub mode: Option<Mode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    /// Must be put right before it can work.
    Missing,
    /// Worth knowing; it may be what is wanted.
    Note,
}

/// What a check's button does.
#[derive(Debug, Clone, PartialEq)]
enum Fix {
    Exe(String),
    Cables { output: Option<String>, input: Option<String> },
    RigctldOn,
    KeyPtt,
    Bandwidth(Option<VaraBandwidth>),
    Mode(Mode),
}

#[derive(Debug, Clone, PartialEq)]
struct Check {
    level: Level,
    text: String,
    fix: Option<(String, Fix)>,
}

fn check(level: Level, text: String) -> Check {
    Check { level, text, fix: None }
}

fn with_fix(level: Level, text: String, button: String, fix: Fix) -> Check {
    Check { level, text, fix: Some((button, fix)) }
}

/// What the program path holds, as far as this machine can tell.
#[derive(Debug, Clone, PartialEq, Default)]
struct ExeProbe {
    /// `Some(false)` when the path names no file here; `None` when it cannot
    /// be looked at (the TNC is on another machine).
    exists: Option<bool>,
    /// A copy of xara found in the usual places.
    found: Option<String>,
}

/// The modes a VARA link can go out in: the radio's sideband (or NFM for
/// VARA FM) — the same ones the engine keys in.
fn mode_fits(mode: Mode, fm: bool) -> bool {
    if fm {
        mode == Mode::Nfm
    } else {
        matches!(mode, Mode::Usb | Mode::Lsb | Mode::Digu | Mode::Digl)
    }
}

/// A device's name as other programs show it: xara lists CoreAudio devices
/// by UID with the name in its description (`BlackHole 2ch, virtual`).
fn device_name(id: &str, list: &[TncDevice]) -> String {
    match list.iter().find(|d| d.id == id) {
        Some(d) if !d.desc.is_empty() => {
            d.desc.trim_end_matches(", virtual").trim_end_matches(" (virtual)").trim().to_string()
        }
        _ => id.trim().to_string(),
    }
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The one of sdroxide's devices that is the TNC's `id`, by name.
fn same_device(id: &str, tnc_list: &[TncDevice], ours: &[String]) -> Option<String> {
    let want = norm(&device_name(id, tnc_list));
    if want.is_empty() {
        return None;
    }
    ours.iter()
        .find(|n| norm(n) == want)
        .or_else(|| {
            ours.iter().find(|n| {
                let n = norm(n);
                !n.is_empty() && (n.contains(&want) || want.contains(&n))
            })
        })
        .cloned()
}

fn is_virtual(d: &TncDevice) -> bool {
    let s = format!("{} {}", d.id, d.desc).to_lowercase();
    [
        "virtual",
        "blackhole",
        "vb-audio",
        "cable",
        "loopback",
        "soundflower",
        "null",
        "monitor of",
        "dax",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// Everything the VARA route still needs, in the order to deal with it.
fn checks(v: &VaraTncConfig, tnc: &ManagedTncStatus, env: &VaraEnv, exe: &ExeProbe) -> Vec<Check> {
    use Level::*;
    let m = &v.managed;
    let mut out = Vec::new();

    if env.callsign.trim().is_empty() {
        out.push(check(Missing, tr!("settings-vara-check-callsign")));
    }

    // The TNC.
    if m.enabled {
        if m.exe.trim().is_empty() {
            out.push(match &exe.found {
                Some(p) => with_fix(
                    Missing,
                    tr!("settings-vara-check-exe-found", path = p.clone()),
                    tr!("settings-vara-fix-use-it"),
                    Fix::Exe(p.clone()),
                ),
                None => check(Missing, tr!("settings-vara-check-exe")),
            });
        } else if exe.exists == Some(false) {
            out.push(match &exe.found {
                Some(p) => with_fix(
                    Missing,
                    tr!("settings-vara-check-exe-gone-found", path = p.clone()),
                    tr!("settings-vara-fix-use-it"),
                    Fix::Exe(p.clone()),
                ),
                None => check(Missing, tr!("settings-vara-check-exe-gone")),
            });
        } else {
            out.push(match tnc.state {
                TncState::Running => {
                    check(Ok, tr!("settings-tnc-running", version = tnc.version.clone()))
                }
                TncState::Starting => check(Note, tr!("winlink-tnc-starting")),
                TncState::Failed => {
                    check(Missing, tr!("winlink-tnc-failed", why = tnc.message.clone()))
                }
                TncState::Off => check(Note, tr!("settings-vara-check-press-apply")),
            });
        }
    } else if v.address.trim().is_empty() {
        out.push(check(Missing, tr!("settings-vara-check-address")));
    } else {
        out.push(check(Note, tr!("settings-vara-check-external", address = v.address.trim())));
    }

    // The audio between the TNC and sdroxide.
    if m.enabled {
        if m.capture.trim().is_empty() || m.playback.trim().is_empty() {
            out.push(check(Missing, tr!("settings-vara-check-cables")));
        } else if m.capture.trim() == m.playback.trim() {
            out.push(check(Missing, tr!("settings-vara-check-same-cable")));
        } else if env.local
            && let Some(a) = env.audio
        {
            let want_out = same_device(&m.capture, &tnc.capture_devices, &a.outputs);
            let want_in = same_device(&m.playback, &tnc.playback_devices, &a.inputs);
            let out_ok = want_out.is_some() && a.selected_output == want_out;
            let in_ok = want_in.is_some() && a.selected_input == want_in;
            if out_ok && in_ok {
                out.push(check(Ok, tr!("settings-vara-check-audio-ok")));
            } else if (out_ok || want_out.is_some()) && (in_ok || want_in.is_some()) {
                out.push(with_fix(
                    Missing,
                    tr!(
                        "settings-vara-check-audio-wrong",
                        capture = device_name(&m.capture, &tnc.capture_devices),
                        playback = device_name(&m.playback, &tnc.playback_devices)
                    ),
                    tr!("settings-vara-fix-cables"),
                    Fix::Cables { output: want_out, input: want_in },
                ));
            } else {
                out.push(check(Missing, tr!("settings-vara-check-audio-unknown")));
            }
        } else {
            out.push(check(Note, tr!("settings-vara-check-audio-remote")));
        }
    } else {
        out.push(check(Note, tr!("settings-vara-check-audio-external")));
    }

    // Keying.
    if m.enabled && m.ptt == TncPtt::Rigctld {
        if !env.rigctld_on {
            out.push(with_fix(
                Missing,
                tr!("settings-vara-check-rigctld"),
                tr!("settings-vara-fix-rigctld"),
                Fix::RigctldOn,
            ));
        }
    } else if !v.key_ptt {
        if m.enabled {
            out.push(with_fix(
                Missing,
                tr!("settings-vara-check-key-ptt"),
                tr!("settings-vara-fix-key-ptt"),
                Fix::KeyPtt,
            ));
        } else {
            out.push(check(Note, tr!("settings-vara-check-tnc-keys")));
        }
    }

    // The bandwidth the call asks for, against the modem xara runs.
    if m.enabled {
        let want = m.mode.bandwidth();
        let clash = match (m.mode.is_fm(), v.bandwidth) {
            (true, Some(_)) => true,
            (false, Some(b)) => Some(b) != want,
            (_, None) => false,
        };
        if clash {
            out.push(with_fix(
                if m.mode.is_fm() { Missing } else { Note },
                tr!("settings-vara-check-bandwidth", modem = tnc_mode_label(m.mode)),
                tr!("settings-vara-fix-bandwidth"),
                Fix::Bandwidth(want),
            ));
        }
    }

    // The radio's mode.
    let fm = m.enabled && m.mode.is_fm();
    if !mode_fits(env.mode, fm) {
        let want = if fm { Mode::Nfm } else { Mode::Digu };
        out.push(with_fix(
            Missing,
            tr!("settings-vara-check-mode", mode = env.mode.label(), want = want.label()),
            tr!("settings-vara-fix-mode", mode = want.label()),
            Fix::Mode(want),
        ));
    }

    if v.gateway.trim().is_empty() && !v.chat.enabled && !v.listen {
        out.push(check(Note, tr!("settings-vara-check-gateway")));
    }
    out
}

fn tnc_mode_label(m: TncMode) -> String {
    match m {
        TncMode::FmNarrow => tr!("settings-tnc-mode-fm-narrow"),
        TncMode::FmWide => tr!("settings-tnc-mode-fm-wide"),
        hf => hf.label().to_string(),
    }
}

/// Where xara usually ends up, on this machine: `PATH`, cargo's bin
/// directory, a checkout built in the home directory.
#[cfg(not(target_arch = "wasm32"))]
fn find_xara() -> Option<String> {
    let name = if cfg!(windows) { "xara.exe" } else { "xara" };
    let mut dirs: Vec<std::path::PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = std::path::PathBuf::from(home);
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join("xara/target/release"));
        dirs.push(home.join("src/xara/target/release"));
        dirs.push(home.join("Documents/xara/target/release"));
    }
    dirs.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"].map(std::path::PathBuf::from));
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Looks at the program path at most every two seconds, and only when the
/// TNC is on this machine.
fn probe_exe(ctx: &egui::Context, exe: &str, local: bool) -> ExeProbe {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ctx, exe, local);
        ExeProbe::default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !local {
            return ExeProbe::default();
        }
        let id = egui::Id::new("vara-exe-probe");
        let now = ctx.input(|i| i.time);
        let cached: Option<(String, f64, ExeProbe)> = ctx.data(|d| d.get_temp(id));
        if let Some((was, at, p)) = cached
            && was == exe
            && now - at < 2.0
        {
            return p;
        }
        let exists = (!exe.trim().is_empty()).then(|| std::path::Path::new(exe.trim()).is_file());
        let found = if exists == Some(true) { None } else { find_xara() };
        let p = ExeProbe { exists, found };
        ctx.data_mut(|d| d.insert_temp(id, (exe.to_string(), now, p.clone())));
        p
    }
}

fn step(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    net_heading(ui, text);
}

/// The Winlink VARA route.
pub(in crate::app) fn winlink_vara_settings(
    ui: &mut egui::Ui,
    v: &mut VaraTncConfig,
    tnc: &ManagedTncStatus,
    env: &VaraEnv,
) -> VaraFixes {
    let mut fixes = VaraFixes::default();
    ui.add_space(4.0);
    ui.label(RichText::new(tr!("settings-winlink-vara-intro")).weak());

    // What is still missing, first: it is what the operator came here for.
    let exe = probe_exe(ui.ctx(), &v.managed.exe, env.local);
    let list = checks(v, tnc, env, &exe);
    step(ui, &tr!("settings-vara-check-heading"));
    let missing = list.iter().filter(|c| c.level == Level::Missing).count();
    if missing == 0 {
        ui.colored_label(crate::theme::GREEN(), tr!("settings-vara-check-ready"));
    }
    for c in &list {
        ui.horizontal_wrapped(|ui| {
            let (mark, color) = match c.level {
                Level::Ok => ("✔", crate::theme::GREEN()),
                Level::Missing => ("⚠", crate::theme::YELLOW()),
                Level::Note => ("•", crate::theme::CYAN_DIM()),
            };
            ui.label(RichText::new(mark).color(color));
            ui.label(&c.text);
            if let Some((button, fix)) = &c.fix
                && ui.button(button).clicked()
            {
                match fix.clone() {
                    Fix::Exe(p) => v.managed.exe = p,
                    Fix::Cables { output, input } => {
                        fixes.output = output;
                        fixes.input = input;
                    }
                    Fix::RigctldOn => fixes.rigctld_on = true,
                    Fix::KeyPtt => v.key_ptt = true,
                    Fix::Bandwidth(b) => v.bandwidth = b,
                    Fix::Mode(m) => fixes.mode = Some(m),
                }
            }
        });
    }
    if missing > 0 {
        ui.label(RichText::new(tr!("settings-vara-check-apply")).weak());
    }

    // 1 · The TNC.
    step(ui, &tr!("settings-vara-step-tnc"));
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-vara-tnc-runs")));
        let was = v.managed.enabled;
        ui.selectable_value(&mut v.managed.enabled, true, tr!("settings-vara-tnc-managed"))
            .on_hover_text(tr!("settings-tnc-enabled-hover"));
        ui.selectable_value(&mut v.managed.enabled, false, tr!("settings-vara-tnc-external"))
            .on_hover_text(tr!("settings-vara-tnc-external-hover"));
        if v.managed.enabled && !was {
            // What xara run by sdroxide wants: a local address, and calls at
            // the modem's own bandwidth.
            if v.address.trim().is_empty() {
                v.address = "127.0.0.1:8300".into();
            }
            v.bandwidth = v.managed.mode.bandwidth();
            if v.managed.ptt == TncPtt::Notices {
                v.key_ptt = true;
            }
        }
    });
    if v.managed.enabled {
        managed_tnc_settings(ui, v, tnc);
    } else {
        net_row(ui, &tr!("settings-winlink-vara-tnc"), &mut v.address, 160.0);
        ui.label(RichText::new(tr!("settings-winlink-vara-tnc-hint")).weak());
        crate::chrome::checkbox(ui, &mut v.key_ptt, tr!("settings-winlink-vara-key-ptt"))
            .on_hover_text(tr!("settings-winlink-vara-key-ptt-hover"));
    }

    // 2 · Calling a gateway.
    step(ui, &tr!("settings-vara-step-gateway"));
    gateway_settings(ui, v);

    // 3 · Being called.
    step(ui, &tr!("settings-vara-step-called"));
    ui.label(RichText::new(tr!("settings-vara-called-hint")).weak());
    if crate::chrome::checkbox(ui, &mut v.listen, tr!("settings-winlink-vara-listen"))
        .on_hover_text(tr!("settings-winlink-vara-listen-hover"))
        .changed()
        && v.listen
    {
        v.chat.enabled = false;
    }
    vara_chat_settings(ui, v);

    // Rarely touched.
    ui.add_space(4.0);
    egui::CollapsingHeader::new(tr!("settings-vara-advanced"))
        .id_salt("vara-advanced")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(RichText::new(tr!("settings-vara-advanced-hint")).weak());
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-winlink-vara-busy")));
                ui.add(egui::DragValue::new(&mut v.busy_wait_s).range(0..=600).suffix(" s"))
                    .on_hover_text(tr!("settings-winlink-vara-busy-hover"));
            });
            if v.managed.enabled {
                net_row(ui, &tr!("settings-winlink-vara-tnc"), &mut v.address, 160.0);
                ui.label(RichText::new(tr!("settings-vara-address-managed-hint")).weak());
                managed_tnc_advanced(ui, &mut v.managed);
            }
        });
    fixes
}

/// The gateway (or peer) to call, and the list of saved ones.
fn gateway_settings(ui: &mut egui::Ui, v: &mut VaraTncConfig) {
    // Upper-cased where it is used, as the packet lane's is.
    net_row(ui, &tr!("settings-gateway"), &mut v.gateway, 140.0);
    ui.label(RichText::new(tr!("settings-vara-gateway-hint")).weak());
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-winlink-vara-bandwidth")));
        for bw in VaraBandwidth::ALL {
            ui.selectable_value(&mut v.bandwidth, Some(bw), format!("{} Hz", bw.hz()));
        }
        ui.selectable_value(&mut v.bandwidth, None, tr!("settings-winlink-vara-tnc-setting"))
            .on_hover_text(tr!("settings-winlink-vara-tnc-setting-hover"));
    });
    ui.label(RichText::new(tr!("settings-vara-bandwidth-hint")).weak());
    let mut mhz = v.freq_hz / 1e6;
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-frequency")));
        if ui
            .add(
                egui::DragValue::new(&mut mhz)
                    .speed(0.001)
                    .range(0.0..=1300.0)
                    .max_decimals(4)
                    .suffix(" MHz"),
            )
            .changed()
        {
            v.freq_hz = mhz * 1e6;
        }
        if v.freq_hz > 0.0 && ui.button(tr!("settings-clear")).clicked() {
            v.freq_hz = 0.0;
        }
    });
    ui.label(RichText::new(tr!("settings-winlink-vara-dial-hint")).weak());
    crate::chrome::checkbox(ui, &mut v.p2p, tr!("settings-winlink-vara-p2p"))
        .on_hover_text(tr!("settings-winlink-vara-p2p-hover"));

    ui.add_space(4.0);
    ui.label(RichText::new(tr!("settings-winlink-vara-my-stations")).strong());
    let mut remove = None;
    let mut pick = None;
    for (i, g) in v.gateways.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .button(tr!("settings-use-2"))
                .on_hover_text(tr!("settings-call-one-next-connect"))
                .clicked()
            {
                pick = Some(i);
            }
            let freq = if g.freq_hz > 0.0 {
                format!("{:.4} MHz", g.freq_hz / 1e6)
            } else {
                tr!("settings-current-dial")
            };
            let bw = match g.bandwidth {
                Some(b) => format!("{} Hz", b.hz()),
                None => tr!("settings-winlink-vara-tnc-setting"),
            };
            let kind = if g.p2p { tr!("settings-winlink-vara-peer") } else { String::new() };
            ui.label(format!(
                "{}{kind} — {freq}, {bw}{}{}",
                g.callsign,
                if g.label.is_empty() { "" } else { " — " },
                g.label,
            ));
            if ui
                .button(tr!("settings-forget"))
                .on_hover_text(tr!("settings-forget-gateway"))
                .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = pick {
        v.select_gateway(i);
    }
    if let Some(i) = remove {
        v.gateways.remove(i);
    }
    if ui
        .button(tr!("settings-add-gateway"))
        .on_hover_text(tr!("settings-save-gateway-above-list"))
        .clicked()
        && !v.gateway.trim().is_empty()
    {
        let entry = v.current_entry();
        // Saving the same callsign again updates it rather than listing it twice.
        match v.gateways.iter_mut().find(|g| g.callsign == entry.callsign) {
            Some(g) => {
                let label = std::mem::take(&mut g.label);
                *g = sdroxide_types::VaraGateway { label, ..entry };
            }
            None => v.gateways.push(entry),
        }
    }
}

/// Keyboard chat through the VARA TNC.
fn vara_chat_settings(ui: &mut egui::Ui, v: &mut VaraTncConfig) {
    use sdroxide_types::ChatFormat;
    let was = v.chat.enabled;
    crate::chrome::checkbox(ui, &mut v.chat.enabled, tr!("settings-varachat-enabled"))
        .on_hover_text(tr!("settings-varachat-enabled-hover"));
    // One program on the TNC at a time: standing by for chat and answering
    // Winlink peer-to-peer calls both want it, so turning one on turns the
    // other off.
    if v.chat.enabled && !was {
        v.listen = false;
    }
    if v.listen && v.chat.enabled {
        v.chat.enabled = false;
    }
    if !v.chat.enabled {
        return;
    }
    ui.indent("vara-chat", |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-varachat-format")));
            ui.selectable_value(
                &mut v.chat.format,
                ChatFormat::Auto,
                tr!("settings-varachat-auto"),
            )
            .on_hover_text(tr!("settings-varachat-auto-hover"));
            ui.selectable_value(&mut v.chat.format, ChatFormat::VarAc, "VarAC")
                .on_hover_text(tr!("settings-varachat-varac-hover"));
            ui.selectable_value(
                &mut v.chat.format,
                ChatFormat::Plain,
                tr!("settings-varachat-plain"),
            )
            .on_hover_text(tr!("settings-varachat-plain-hover"));
        });
        ui.label(RichText::new(tr!("settings-varachat-format-hint")).weak());
        crate::chrome::checkbox(ui, &mut v.chat.send_profile, tr!("settings-varachat-profile"))
            .on_hover_text(tr!("settings-varachat-profile-hover"));
        if v.chat.send_profile {
            net_row(ui, &tr!("settings-varachat-name"), &mut v.chat.name, 160.0);
            net_row(ui, &tr!("settings-varachat-qth"), &mut v.chat.qth, 160.0);
        }
    });
}

/// xara run by sdroxide: the program, its audio, its modem and keying.
fn managed_tnc_settings(ui: &mut egui::Ui, v: &mut VaraTncConfig, tnc: &ManagedTncStatus) {
    let m = &mut v.managed;
    net_row(ui, &tr!("settings-tnc-program"), &mut m.exe, 260.0);
    ui.label(RichText::new(tr!("settings-vara-program-hint")).weak());

    device_picker(
        ui,
        "tnc-capture",
        &tr!("settings-tnc-capture"),
        &mut m.capture,
        &tnc.capture_devices,
    );
    ui.label(RichText::new(tr!("settings-tnc-capture-hint")).weak());
    device_picker(
        ui,
        "tnc-playback",
        &tr!("settings-tnc-playback"),
        &mut m.playback,
        &tnc.playback_devices,
    );
    ui.label(RichText::new(tr!("settings-tnc-playback-hint")).weak());
    if tnc.capture_devices.is_empty() && tnc.playback_devices.is_empty() {
        ui.label(RichText::new(tr!("settings-vara-no-device-list")).weak());
    }

    let was = m.mode;
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-tnc-mode")));
        for mode in TncMode::ALL {
            ui.selectable_value(&mut m.mode, mode, tnc_mode_label(mode));
        }
    });
    ui.label(RichText::new(tr!("settings-vara-modem-hint")).weak());
    let mut notices = false;
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-tnc-ptt")));
        notices = ui
            .selectable_value(&mut m.ptt, TncPtt::Notices, tr!("settings-tnc-ptt-notices"))
            .on_hover_text(tr!("settings-tnc-ptt-notices-hover"))
            .clicked();
        ui.selectable_value(&mut m.ptt, TncPtt::Rigctld, tr!("settings-tnc-ptt-rigctld"))
            .on_hover_text(tr!("settings-tnc-ptt-rigctld-hover"));
    });
    ui.label(RichText::new(tr!("settings-vara-keying-hint")).weak());
    ui.label(RichText::new(tr!("settings-tnc-audio-routing")).weak());
    if v.managed.mode != was {
        // A call asks for the modem's own bandwidth.
        v.bandwidth = v.managed.mode.bandwidth();
    }
    if notices {
        // Keying through sdroxide's Winlink is keying on the TNC's notices.
        v.key_ptt = true;
    }
}

/// The managed TNC's rarely changed knobs.
fn managed_tnc_advanced(ui: &mut egui::Ui, m: &mut ManagedTncConfig) {
    use sdroxide_types::TncAudio;
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-tnc-audio")));
        for a in TncAudio::ALL {
            let label = match a.arg() {
                Some(name) => name.to_string(),
                None => tr!("settings-tnc-audio-default"),
            };
            ui.selectable_value(&mut m.audio, a, label);
        }
    });
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-tnc-lead")));
        ui.add(egui::DragValue::new(&mut m.ptt_lead_ms).range(0..=1000).suffix(" ms"))
            .on_hover_text(tr!("settings-tnc-lead-hover"));
        ui.add_sized([60.0, 22.0], egui::Label::new(tr!("settings-tnc-drive")));
        ui.add(egui::DragValue::new(&mut m.drive_db).range(-30.0..=0.0).speed(0.5).suffix(" dB"))
            .on_hover_text(tr!("settings-vara-drive-hover"));
    });
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(tr!("settings-tnc-kiss")));
        ui.add(egui::DragValue::new(&mut m.kiss_port).range(0..=65535))
            .on_hover_text(tr!("settings-tnc-kiss-hover"));
    });
    net_row(ui, &tr!("settings-tnc-extra"), &mut m.extra_args, 260.0);
}

/// A device field: a picker from the TNC's own list when it gave one, with
/// the virtual cables first, and the text field always, for a device it did
/// not list.
fn device_picker(
    ui: &mut egui::Ui,
    salt: &str,
    label: &str,
    value: &mut String,
    devices: &[TncDevice],
) {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 22.0], egui::Label::new(label));
        crate::chrome::field(ui, egui::TextEdit::singleline(value).desired_width(200.0));
        if !devices.is_empty() {
            let shown = devices
                .iter()
                .find(|d| d.id == *value)
                .map(|d| if d.desc.is_empty() { d.id.clone() } else { d.desc.clone() })
                .unwrap_or_else(|| tr!("settings-tnc-pick"));
            let mut sorted: Vec<&TncDevice> = devices.iter().collect();
            sorted.sort_by_key(|d| !is_virtual(d));
            egui::ComboBox::from_id_salt(salt).selected_text(shown).show_ui(ui, |ui| {
                for d in sorted {
                    let name = if d.desc.is_empty() {
                        d.id.clone()
                    } else {
                        format!("{} — {}", d.desc, d.id)
                    };
                    let text = if is_virtual(d) {
                        RichText::new(name).strong()
                    } else {
                        RichText::new(name).weak()
                    };
                    ui.selectable_value(value, d.id.clone(), text);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::{AudioRouting, ChatFormat};

    fn dev(id: &str, desc: &str) -> TncDevice {
        TncDevice { id: id.into(), desc: desc.into() }
    }

    fn audio(out: Option<&str>, inp: Option<&str>) -> AudioDevices {
        AudioDevices {
            outputs: vec![
                "MacBook Speakers".into(),
                "BlackHole 2ch".into(),
                "BlackHole 16ch".into(),
            ],
            inputs: vec!["MacBook Mic".into(), "BlackHole 2ch".into(), "BlackHole 16ch".into()],
            selected_output: out.map(Into::into),
            selected_input: inp.map(Into::into),
            routing: AudioRouting::default(),
        }
    }

    fn status() -> ManagedTncStatus {
        ManagedTncStatus {
            state: TncState::Running,
            version: "0.1.3".into(),
            capture_devices: vec![
                dev("BlackHole2ch_UID", "BlackHole 2ch, virtual"),
                dev("BlackHole16ch_UID", "BlackHole 16ch, virtual"),
            ],
            playback_devices: vec![
                dev("BlackHole2ch_UID", "BlackHole 2ch, virtual"),
                dev("BlackHole16ch_UID", "BlackHole 16ch, virtual"),
            ],
            ..Default::default()
        }
    }

    fn managed() -> VaraTncConfig {
        let mut v = VaraTncConfig::default();
        v.address = "127.0.0.1:8300".into();
        v.gateway = "EA5XYZ-10".into();
        v.key_ptt = true;
        v.bandwidth = Some(VaraBandwidth::Hz2300);
        v.managed.enabled = true;
        v.managed.exe = "/opt/xara".into();
        v.managed.capture = "BlackHole2ch_UID".into();
        v.managed.playback = "BlackHole16ch_UID".into();
        v
    }

    fn env<'a>(a: &'a AudioDevices, mode: Mode) -> VaraEnv<'a> {
        VaraEnv { callsign: "EA5ABC", local: true, mode, audio: Some(a), rigctld_on: false }
    }

    fn missing(list: &[Check]) -> Vec<&Check> {
        list.iter().filter(|c| c.level == Level::Missing).collect()
    }

    #[test]
    fn a_complete_setup_has_nothing_missing() {
        let a = audio(Some("BlackHole 2ch"), Some("BlackHole 16ch"));
        let list = checks(&managed(), &status(), &env(&a, Mode::Digu), &ExeProbe::default());
        assert!(missing(&list).is_empty(), "{list:#?}");
        assert!(list.iter().any(|c| c.level == Level::Ok && c.text.contains("0.1.3")));
    }

    #[test]
    fn sdroxide_audio_on_the_wrong_device_offers_the_cables() {
        let a = audio(None, Some("MacBook Mic"));
        let list = checks(&managed(), &status(), &env(&a, Mode::Digu), &ExeProbe::default());
        let fix = list.iter().find_map(|c| c.fix.as_ref().map(|f| f.1.clone()));
        assert_eq!(
            fix,
            Some(Fix::Cables {
                output: Some("BlackHole 2ch".into()),
                input: Some("BlackHole 16ch".into())
            })
        );
    }

    #[test]
    fn one_cable_both_ways_is_flagged() {
        let mut v = managed();
        v.managed.playback = v.managed.capture.clone();
        let a = audio(Some("BlackHole 2ch"), Some("BlackHole 2ch"));
        let list = checks(&v, &status(), &env(&a, Mode::Digu), &ExeProbe::default());
        assert_eq!(missing(&list).len(), 1, "{list:#?}");
    }

    #[test]
    fn mode_keying_and_bandwidth_each_get_a_fix() {
        let mut v = managed();
        v.managed.ptt = TncPtt::Rigctld;
        v.managed.mode = TncMode::FmNarrow;
        let a = audio(Some("BlackHole 2ch"), Some("BlackHole 16ch"));
        let list = checks(&v, &status(), &env(&a, Mode::Usb), &ExeProbe::default());
        let fixes: Vec<Fix> =
            list.iter().filter_map(|c| c.fix.as_ref().map(|f| f.1.clone())).collect();
        assert_eq!(fixes, vec![Fix::RigctldOn, Fix::Bandwidth(None), Fix::Mode(Mode::Nfm)]);
    }

    #[test]
    fn a_missing_program_offers_the_one_found() {
        let mut v = managed();
        v.managed.exe.clear();
        let a = audio(Some("BlackHole 2ch"), Some("BlackHole 16ch"));
        let probe = ExeProbe { exists: None, found: Some("/Users/x/.cargo/bin/xara".into()) };
        let list = checks(&v, &status(), &env(&a, Mode::Digu), &probe);
        assert_eq!(
            list.iter().find_map(|c| c.fix.as_ref().map(|f| f.1.clone())),
            Some(Fix::Exe("/Users/x/.cargo/bin/xara".into()))
        );
    }

    #[test]
    fn an_external_tnc_needs_only_its_address_and_a_mode() {
        let mut v = VaraTncConfig::default();
        v.chat.enabled = true;
        v.chat.format = ChatFormat::Auto;
        v.address = "127.0.0.1:8300".into();
        v.key_ptt = true;
        let a = audio(None, None);
        let list = checks(&v, &status(), &env(&a, Mode::Digu), &ExeProbe::default());
        assert!(missing(&list).is_empty(), "{list:#?}");
    }

    #[test]
    fn xara_names_match_sdroxide_names() {
        let list = [dev("BlackHole2ch_UID", "BlackHole 2ch, virtual")];
        let ours = ["BlackHole 16ch".to_string(), "BlackHole 2ch".to_string()];
        assert_eq!(same_device("BlackHole2ch_UID", &list, &ours), Some("BlackHole 2ch".into()));
        // PulseAudio names are the same on both sides.
        assert_eq!(
            same_device("vara_rx.monitor", &[], &["vara_rx.monitor".to_string()]),
            Some("vara_rx.monitor".into())
        );
        assert_eq!(same_device("nothing", &list, &ours), None);
    }
}

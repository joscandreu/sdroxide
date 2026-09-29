//! The SSTV and RIFP image panel.
//!
//! [`SstvUi`] draws two things the engine owns and one it does not. The five
//! transmit presets and the gallery of received pictures live on the machine
//! the radio is plugged into — the panel holds metadata, thumbnails and
//! whatever pixels it has been handed, and asks for the rest. The picture
//! currently arriving is the exception: scanlines are painted into a texture as
//! they come, so a picture builds up on screen exactly as it does on the air.
//!
//! Compositing stays here. The crop, the header strip and the text layers are
//! applied to the fetched source client-side, so the preview redraws on every
//! keystroke without a round trip, and only the finished picture goes back to
//! the engine as an ordinary `SstvTx` / `RifpTx`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use sdroxide_types::{
    Command, IMAGE_LAYERS_MAX, IMAGE_SLOTS, IMAGE_UPLOAD_MAX, ImageEntry, ImageKind, ImagePresets,
    Mode, RifpEncoding, RifpProfile, RifpSize, RifpStatus, SstvMode, SstvStatus, TextAnchor,
    TextLayer,
};

use crate::theme::ThemedScroll;

use crate::app::panels::widgets::{pick_image, sstv_section};
use crate::app::util::shorten;
use crate::app::{SdroxideApp, rx_only_hint};

// ───────────────────────────── SSTV panel ──────────────────────────────

/// A fetched transmit source: the picture the compositor works from, and which
/// stored version it is, so a slot changed from another screen is refetched
/// rather than composed from the old photograph.
pub(in crate::app) struct SstvSlot {
    src_rgb: Vec<u8>,
    sw: u16,
    sh: u16,
    version: u32,
}

/// Pictures the gallery will hold thumbnails for.
///
/// Not a limit on the collection — the engine counts the whole store and the
/// panel says how many more there are — but on what this client keeps as
/// textures.
const GALLERY_MAX: usize = 240;

/// The source crop-scaled to the transmitted size, kept so recomposing the
/// preview — every frame while a layer is dragged — only redraws the text.
pub(in crate::app) struct SstvBase {
    slot: usize,
    version: u32,
    dims: (u16, u16),
    rgb: Vec<u8>,
}

/// A text layer being dragged on the preview: moved by its body, or resized
/// by a corner handle.
#[derive(Clone)]
pub(in crate::app) struct LayerDrag {
    layer: usize,
    kind: DragKind,
}

#[derive(Clone)]
enum DragKind {
    /// Where in the layer's box it was grabbed, in picture pixels from the
    /// box's top-left, so the box does not jump to put its corner under the
    /// pointer.
    Move { grab: egui::Vec2 },
    /// Which corner is held, as fractions of the box (`(1, 1)` bottom-right),
    /// and the layer and its box as they were when it was grabbed — the
    /// resize is always worked out from those, so it does not drift.
    Resize { corner: (f32, f32), start: TextLayer, b0: crate::sstv::LayerBox },
}

/// Side of a corner handle on the preview, in screen points.
const HANDLE: f32 = 7.0;

/// A received-picture gallery entry.
pub(in crate::app) struct SstvRecv {
    entry: ImageEntry,
    thumb: egui::TextureHandle,
    /// The picture at full size, once it has been fetched — or promoted
    /// straight from the one that had just been received, which the panel
    /// already held. `None` until the operator enlarges it.
    full: Option<egui::TextureHandle>,
}

/// Image-panel state, shared by SSTV and RIFP: received gallery, in-progress
/// incoming picture, transmit slots, their text layers, the current mode, and
/// cached textures.
///
/// One workspace for both modes on purpose. The pictures an operator wants to
/// send, the captions on them, and the pictures that came back are the same
/// things whichever protocol carried them; only the control strip and the
/// transmit sizing differ.
pub(in crate::app) struct SstvUi {
    pub(in crate::app) tx_mode: SstvMode,
    /// Latest RIFP engine status (transfer progress, sessions, counters).
    pub(in crate::app) rifp: RifpStatus,
    /// Size of the picture currently arriving, so the live canvas can be built
    /// before the whole object is in. `(0, 0)` when nothing is arriving.
    pub(in crate::app) rx_dims: (u16, u16),
    /// Size the cached preview was composed at, so a change of transmit size
    /// rebuilds it.
    pub(in crate::app) preview_dims: (u16, u16),
    /// The banner drawn across the top of the transmit image, resolved from the
    /// station's digital-mode config. `None` when the operator switched it off.
    ///
    /// Resolved here rather than at each compose because it is what tells the
    /// preview it is stale: the strip changes when the callsign changes, when a
    /// template is edited, when a colour is picked, and comparing the composed
    /// banner catches all of that with one test.
    pub(in crate::app) banner: Option<crate::sstv::Banner>,
    /// What the `{…}` placeholders stand for this frame. Compared like the
    /// banner: a callsign edited, a mode changed or a minute ticking over all
    /// make the preview stale.
    pub(in crate::app) fields: crate::sstv::Fields,
    /// Whether the banner editor window is open.
    pub(in crate::app) banner_open: bool,
    /// Auto mode: RX auto-detects the mode; TX defaults to Martin 1 until a mode
    /// is heard or the operator picks one.
    pub(in crate::app) auto: bool,
    /// The engine's transmit presets: what is in each slot and the text over
    /// it. The truth — `slots` below only caches its pixels.
    pub(in crate::app) presets: ImagePresets,
    /// Thumbnails of the presets, rebuilt when the engine announces a change.
    pub(in crate::app) slot_thumbs: Vec<Option<egui::TextureHandle>>,
    /// The layers being edited and which slot they belong to. Only that one
    /// slot is client-owned, so an echo of our own write — or an edit made on
    /// another screen — can update the other four without fighting the
    /// keyboard or the pointer. See [`crate::sstv::tick`].
    pub(in crate::app) edit: Option<crate::sstv::LayerEdit>,
    /// Which of the slot's layers the editor controls act on.
    pub(in crate::app) sel_layer: usize,
    /// The layer being dragged on the preview, if one is.
    pub(in crate::app) drag: Option<LayerDrag>,
    /// Whether the layer's text box had the cursor last frame — with a drag,
    /// what keeps the slot in hand.
    pub(in crate::app) text_focus: bool,
    /// The SSTV contact while this screen is editing it — the same
    /// hold-while-typing arrangement as the layers, for the QSO box.
    pub(in crate::app) qso_edit: Option<crate::sstv::Held<sdroxide_types::SstvQso>>,
    /// Whether a QSO box field had the cursor last frame.
    pub(in crate::app) qso_focus: bool,
    /// The call the QSO box last asked a callsign lookup about, so each call is
    /// looked up once rather than once a frame.
    pub(in crate::app) looked_up: String,
    /// The panel's clock (egui time) as of this frame, for the edits made from
    /// outside it — a lookup answering, a log entry saved — which have to be
    /// stamped on the same clock the held values are timed on.
    pub(in crate::app) now: f64,
    /// The slot whose "Remove image" has been pressed once, and when — armed
    /// for a few seconds, waiting for the confirming second press.
    pub(in crate::app) confirm_remove: Option<(usize, f64)>,
    /// Put the cursor in the text box next frame: a layer was just added.
    pub(in crate::app) focus_text: bool,
    /// Where each layer landed in the last composed preview, for hit-testing a
    /// click and outlining the selection.
    pub(in crate::app) boxes: Vec<Option<crate::sstv::LayerBox>>,
    /// Where the preview was drawn on screen last frame.
    pub(in crate::app) preview_rect: egui::Rect,
    /// The crop-scaled source the preview is composed over.
    pub(in crate::app) base: Option<SstvBase>,
    /// Fetched source pictures, keyed by slot; `None` until one arrives.
    pub(in crate::app) slots: Vec<Option<SstvSlot>>,
    /// The version last asked for per slot.
    ///
    /// Deliberately not cleared when the answer lands: it records the *ask*, so
    /// a picture that fails to decode is asked for once rather than once a
    /// frame for ever. A slot changed on the radio arrives with a new version,
    /// which is what makes the next ask happen.
    pub(in crate::app) src_asked: HashMap<usize, u32>,
    pub(in crate::app) selected_slot: usize,
    pub(in crate::app) received: Vec<SstvRecv>,
    /// How many pictures the store holds altogether, so the gallery can say
    /// exactly how many older ones there are rather than guessing.
    pub(in crate::app) total: u32,
    /// Where the engine is saving received pictures, for the gallery to show.
    pub(in crate::app) dir: String,
    /// True while a listing page is outstanding — the reliable lane is finite
    /// and a scroll must not spray requests down it.
    pub(in crate::app) page_pending: bool,
    /// Name of the picture whose full size has been asked for. Like
    /// `src_asked`, it records the ask and is cleared when the viewer moves on,
    /// not when the answer arrives.
    pub(in crate::app) full_asked: Option<String>,
    /// The answer to that ask was empty — the store no longer has it.
    pub(in crate::app) full_gone: bool,
    /// In-progress incoming image (painted line-by-line).
    pub(in crate::app) rx_color: Option<egui::ColorImage>,
    pub(in crate::app) rx_tex: Option<egui::TextureHandle>,
    pub(in crate::app) rx_id: u32,
    /// The picture that has just finished arriving, held until the engine names
    /// the file it went into. Enlarging it then costs nothing, where fetching
    /// back what the panel was handed a moment ago would cost a megabyte.
    pub(in crate::app) fresh: Option<(egui::TextureHandle, Vec<u8>)>,
    /// The bytes of the full-size picture last fetched or received, and its
    /// name. One at a time: this is what "Save picture…" writes out, and
    /// keeping every picture's bytes for a button nobody may press would hold
    /// a gallery's worth of megabytes for nothing.
    pub(in crate::app) full_png: Option<(String, Vec<u8>)>,
    pub(in crate::app) status: SstvStatus,
    /// Received-gallery index currently shown enlarged in an overlay window.
    pub(in crate::app) enlarged: Option<usize>,
    /// Name of the picture whose DELETE has been pressed once and is waiting to
    /// be pressed again.
    ///
    /// Two presses rather than a modal, because the file goes for good and there
    /// is no undo to offer: a stray click on a chip beside SAVE would otherwise
    /// be the last anyone saw of a picture. Armed per name, so it cannot survive
    /// into the next picture and delete that one instead.
    pub(in crate::app) confirm_delete: Option<String>,
    /// Last VIS/free-run-detected mode we auto-applied to `tx_mode`, so a steady
    /// detection doesn't keep overriding the operator's manual mode choice.
    pub(in crate::app) last_detected: Option<SstvMode>,
    pub(in crate::app) preview_tex: Option<egui::TextureHandle>,
    pub(in crate::app) preview_dirty: bool,
    /// Whether the store has been listed this session.
    pub(in crate::app) listed: bool,
    /// File-picker result inbox (raw image bytes), filled by the picker task.
    pub(in crate::app) inbox: Arc<Mutex<Option<Vec<u8>>>>,
    pub(in crate::app) pick_target: Option<usize>,
    /// A local complaint about a picked file — too big to be worth sending, or
    /// one the engine refused. Cleared when the next picture is picked.
    pub(in crate::app) pick_error: Option<String>,
}

impl Default for SstvUi {
    fn default() -> Self {
        SstvUi {
            tx_mode: SstvMode::Martin1,
            rifp: RifpStatus::default(),
            rx_dims: (0, 0),
            preview_dims: (0, 0),
            banner: None,
            fields: crate::sstv::Fields::default(),
            banner_open: false,
            auto: true,
            presets: ImagePresets::default(),
            slot_thumbs: (0..IMAGE_SLOTS).map(|_| None).collect(),
            edit: None,
            sel_layer: 0,
            drag: None,
            text_focus: false,
            qso_edit: None,
            qso_focus: false,
            confirm_remove: None,
            looked_up: String::new(),
            now: 0.0,
            focus_text: false,
            boxes: Vec::new(),
            preview_rect: egui::Rect::NOTHING,
            base: None,
            slots: (0..IMAGE_SLOTS).map(|_| None).collect(),
            src_asked: HashMap::new(),
            selected_slot: 0,
            received: Vec::new(),
            total: 0,
            dir: String::new(),
            page_pending: false,
            full_asked: None,
            full_gone: false,
            rx_color: None,
            rx_tex: None,
            rx_id: 0,
            fresh: None,
            full_png: None,
            status: SstvStatus::default(),
            enlarged: None,
            confirm_delete: None,
            last_detected: None,
            preview_tex: None,
            preview_dirty: true,
            listed: false,
            inbox: Arc::new(Mutex::new(None)),
            pick_target: None,
            pick_error: None,
        }
    }
}

impl SstvUi {
    /// A decoded scanline arrived: paint it into the in-progress image.
    pub(in crate::app) fn on_line(&mut self, id: u32, y: u16, rgb: &[u8], ctx: &egui::Context) {
        let Some(mode) = self.status.detected else { return };
        let (w, h) = mode.dimensions();
        if self.rx_id != id || self.rx_color.is_none() {
            self.rx_id = id;
            self.rx_color =
                Some(crate::sstv::color_image(&vec![0u8; w as usize * h as usize * 3], w, h));
        }
        let Some(ci) = self.rx_color.as_mut() else { return };
        let (w, h) = (w as usize, h as usize);
        if (y as usize) < h && rgb.len() >= w * 3 {
            let row = y as usize * w;
            for x in 0..w {
                ci.pixels[row + x] = Color32::from_rgb(rgb[x * 3], rgb[x * 3 + 1], rgb[x * 3 + 2]);
            }
        }
        self.rx_tex = Some(ctx.load_texture("sstv_rx", ci.clone(), egui::TextureOptions::NEAREST));
    }

    /// A completed picture arrived. The gallery entry comes from the engine a
    /// moment later, naming the file it was saved as; what is held here is the
    /// full-size texture, so enlarging it does not fetch back what the panel
    /// has just been given.
    pub(in crate::app) fn on_image(&mut self, png: &[u8], ctx: &egui::Context) {
        self.fresh = crate::sstv::decode_image(png).map(|(rgb, w, h)| {
            let ci = crate::sstv::color_image(&rgb, w, h);
            (ctx.load_texture("sstv_recv", ci, egui::TextureOptions::NEAREST), png.to_vec())
        });
        self.rx_color = None;
        self.rx_tex = None;
    }

    /// RIFP: reassembled raster rows arrived — paint them into the live
    /// picture. Only the unencoded raster gets here; everything else appears
    /// whole in [`SstvUi::on_rifp_image`].
    pub(in crate::app) fn on_rifp_rows(
        &mut self,
        id: u32,
        y: u16,
        w: u16,
        h: u16,
        gray: &[u8],
        ctx: &egui::Context,
    ) {
        if self.rx_id != id || self.rx_color.is_none() || self.rx_dims != (w, h) {
            self.rx_id = id;
            self.rx_dims = (w, h);
            self.rx_color =
                Some(crate::sstv::color_image(&vec![0u8; w as usize * h as usize * 3], w, h));
        }
        let Some(ci) = self.rx_color.as_mut() else { return };
        let (wu, hu) = (w as usize, h as usize);
        for (row, pixels) in gray.chunks_exact(wu).enumerate() {
            let y = y as usize + row;
            if y >= hu {
                break;
            }
            for (x, &g) in pixels.iter().enumerate() {
                ci.pixels[y * wu + x] = Color32::from_gray(g);
            }
        }
        self.rx_tex = Some(ctx.load_texture("rifp_rx", ci.clone(), egui::TextureOptions::NEAREST));
    }

    /// RIFP: a complete, digest-verified picture arrived.
    pub(in crate::app) fn on_rifp_image(&mut self, png: &[u8], ctx: &egui::Context) {
        self.on_image(png, ctx);
        self.rx_dims = (0, 0);
    }

    /// The engine announced the transmit presets: adopt them, rebuild the slot
    /// thumbnails, and drop any cached source whose picture has been replaced.
    pub(in crate::app) fn on_presets(&mut self, presets: ImagePresets, ctx: &egui::Context) {
        for (i, slot) in presets.slots.iter().enumerate() {
            let held = self.slots.get(i).and_then(|s| s.as_ref()).map(|s| s.version);
            if held.is_some_and(|v| v != slot.version) || (held.is_some() && !slot.has_picture()) {
                if let Some(cell) = self.slots.get_mut(i) {
                    *cell = None;
                }
                if i == self.selected_slot {
                    self.preview_dirty = true;
                }
            }
            let thumb = (!slot.thumb.is_empty())
                .then(|| crate::sstv::decode_image(&slot.thumb))
                .flatten()
                .map(|(rgb, w, h)| {
                    let ci = crate::sstv::color_image(&rgb, w, h);
                    ctx.load_texture("sstv_slot", ci, egui::TextureOptions::LINEAR)
                });
            if let Some(cell) = self.slot_thumbs.get_mut(i) {
                *cell = thumb;
            }
        }
        // The editor takes the engine's layers for every slot except the one in
        // hand, so an edit made elsewhere shows up.
        if self.presets.slot(self.selected_slot).layers != presets.slot(self.selected_slot).layers
            && !matches!(&self.edit, Some(e) if e.slot == self.selected_slot)
        {
            self.preview_dirty = true;
        }
        self.presets = presets;
    }

    /// A fetched source picture for a slot.
    pub(in crate::app) fn on_slot_source(&mut self, slot: u8, version: u32, png: &[u8]) {
        let slot = slot as usize;
        let Some(cell) = self.slots.get_mut(slot) else { return };
        *cell = crate::sstv::load_source_bounded(png, 1024).map(|(rgb, sw, sh)| SstvSlot {
            src_rgb: rgb,
            sw,
            sh,
            version,
        });
        if slot == self.selected_slot {
            self.preview_dirty = true;
        }
    }

    /// One page of the received store.
    pub(in crate::app) fn on_listing(
        &mut self,
        listing: sdroxide_types::ImageListing,
        ctx: &egui::Context,
    ) {
        self.page_pending = false;
        self.total = listing.total;
        self.dir = listing.dir;
        for entry in listing.entries {
            self.insert_entry(entry, None, ctx);
        }
    }

    /// A picture the engine has just stored. It goes to the front of the
    /// gallery, taking the full-size texture the panel already holds from the
    /// picture that has this moment finished arriving.
    pub(in crate::app) fn on_saved(&mut self, entry: ImageEntry, ctx: &egui::Context) {
        // A picture already listed, announced again because who sent it has
        // just become known. Whichever of the two announcements lands second,
        // a sender once known is kept.
        if let Some(r) = self.received.iter_mut().find(|r| r.entry.name == entry.name) {
            let sender = entry.sender.clone().or_else(|| r.entry.sender.take());
            r.entry = ImageEntry { sender, ..entry };
            return;
        }
        let fresh = self.fresh.take();
        if let Some((_, png)) = &fresh {
            self.full_png = Some((entry.name.clone(), png.clone()));
        }
        self.total += 1;
        self.insert_entry(entry, fresh.map(|(tex, _)| tex), ctx);
    }

    /// A picture is no longer in the store: this screen's delete, or another's.
    ///
    /// The enlarged view is an index into the list, so it is moved with the
    /// list. Deleting the picture being looked at leaves the index on the
    /// next-older one, which is what makes culling a gallery a sequence of
    /// clicks in one place rather than a reopen after every one.
    pub(in crate::app) fn on_deleted(&mut self, name: &str) {
        let Some(at) = self.received.iter().position(|r| r.entry.name == name) else { return };
        self.received.remove(at);
        // `total` counts the whole store, of which this list is only a window.
        // It comes down for a picture that was in the window, because that is
        // the case where this client knows the store really did shrink; for one
        // past the end it is left alone and the next listing settles it.
        self.total = self.total.saturating_sub(1);
        if self.full_png.as_ref().is_some_and(|(n, _)| n == name) {
            self.full_png = None;
        }
        if self.confirm_delete.as_deref() == Some(name) {
            self.confirm_delete = None;
        }
        if let Some(v) = self.enlarged {
            let next = if at < v { v - 1 } else { v };
            self.enlarged = (next < self.received.len()).then_some(next);
            // Only a delete of the picture being looked at puts a different one
            // in the window; anything above it just shifted the same picture up.
            if at == v {
                self.full_asked = None;
                self.full_gone = false;
            }
        }
    }

    /// A fetched full-size picture. An empty answer means the store no longer
    /// has it — the file moved or was deleted between listing and opening.
    pub(in crate::app) fn on_file(&mut self, name: &str, png: &[u8], ctx: &egui::Context) {
        let Some((rgb, w, h)) = crate::sstv::decode_image(png) else {
            if self.full_asked.as_deref() == Some(name) {
                self.full_gone = true;
            }
            return;
        };
        let tex = ctx.load_texture(
            "sstv_full",
            crate::sstv::color_image(&rgb, w, h),
            egui::TextureOptions::NEAREST,
        );
        if let Some(r) = self.received.iter_mut().find(|r| r.entry.name == name) {
            r.full = Some(tex);
        }
        self.full_png = Some((name.to_string(), png.to_vec()));
    }

    /// Add a gallery entry in received order, skipping one already held.
    ///
    /// The listing and the just-arrived notification can name the same picture
    /// when one lands in the gap between the request and its answer; the name
    /// is what tells them apart.
    fn insert_entry(
        &mut self,
        entry: ImageEntry,
        full: Option<egui::TextureHandle>,
        ctx: &egui::Context,
    ) {
        if self.received.iter().any(|r| r.entry.name == entry.name) {
            return;
        }
        let Some((rgb, w, h)) = crate::sstv::decode_image(&entry.thumb) else { return };
        let thumb = ctx.load_texture(
            "sstv_thumb",
            crate::sstv::color_image(&rgb, w, h),
            egui::TextureOptions::LINEAR,
        );
        let at = self.received.partition_point(|r| r.entry.unix > entry.unix);
        self.received.insert(at, SstvRecv { entry, thumb, full });
        // The enlarged view is an index into this list, so anything inserted
        // above it moves what the operator is looking at.
        if let Some(v) = self.enlarged.as_mut() {
            if at <= *v {
                *v += 1;
            }
        }
        // A long session receives more pictures than this client will hold
        // textures for. The oldest go, not the newest — they are still in the
        // store, and `total` still counts them.
        self.received.truncate(GALLERY_MAX);
    }

    /// A callsign lookup answered: fill the QSO box's blank name and QTH, if
    /// it is about the call in the box. See [`SdroxideApp::sstv_apply_lookup`].
    pub(in crate::app) fn apply_lookup(&mut self, info: &sdroxide_types::CallsignInfo) {
        let q = self.qso();
        if !q.call.trim().eq_ignore_ascii_case(info.call.trim()) {
            return;
        }
        let name = info.name.clone().filter(|_| q.name.trim().is_empty());
        let qth = info.qth.clone().filter(|_| q.qth.trim().is_empty());
        if name.is_none() && qth.is_none() {
            return;
        }
        let q = crate::sstv::held_edit(&mut self.qso_edit, &self.presets.qso, self.now);
        if let Some(n) = name {
            q.name = n;
        }
        if let Some(t) = qth {
            q.qth = t;
        }
        self.preview_dirty = true;
    }

    /// A log entry for `call` was saved: if that is the contact in progress,
    /// it is finished.
    pub(in crate::app) fn mark_logged(&mut self, call: &str) {
        let q = self.qso();
        if q.logged || q.call.trim().is_empty() || !q.call.trim().eq_ignore_ascii_case(call.trim())
        {
            return;
        }
        crate::sstv::held_edit(&mut self.qso_edit, &self.presets.qso, self.now).logged = true;
    }

    /// The contact in progress, as this screen shows it.
    pub(in crate::app) fn qso(&self) -> &sdroxide_types::SstvQso {
        crate::sstv::held_shown(&self.qso_edit, &self.presets.qso)
    }

    /// The placeholders the selected slot's visible text uses that would
    /// print nothing.
    pub(in crate::app) fn empty_placeholders(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for l in self.current_layers().iter().filter(|l| l.visible) {
            for p in crate::sstv::empty_used(&l.text, &self.fields) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// The text layers of the slot currently being edited.
    pub(in crate::app) fn current_layers(&self) -> &[TextLayer] {
        crate::sstv::layers_shown(&self.edit, &self.presets, self.selected_slot)
    }

    /// Change the selected slot's layers: takes the slot in hand, committing
    /// any other, and marks the preview stale.
    pub(in crate::app) fn layers_mut(
        &mut self,
        now: f64,
        cmds: &mut Vec<Command>,
    ) -> &mut Vec<TextLayer> {
        self.preview_dirty = true;
        crate::sstv::edit_layers(&mut self.edit, &self.presets, self.selected_slot, now, cmds)
    }

    /// The per-frame half of editing: send a layer edit the operator has
    /// paused on, and hand the slot back to the engine once it agrees.
    pub(in crate::app) fn sync_edit(&mut self, now: f64, cmds: &mut Vec<Command>) {
        if let Some(q) =
            crate::sstv::held_tick(&mut self.qso_edit, &self.presets.qso, now, self.qso_focus)
        {
            cmds.push(Command::ImageSetQso(q));
        }
        let busy = self.drag.is_some() || self.text_focus;
        let held = self.edit.is_some();
        cmds.extend(crate::sstv::tick(&mut self.edit, &self.presets, now, busy));
        if held && self.edit.is_none() {
            // Handed back: what shows is the engine's now, which may differ.
            self.preview_dirty = true;
        }
    }

    /// Move to another slot, handing the one being left back to the engine
    /// first so an edit is never lost to a click.
    pub(in crate::app) fn select_slot(&mut self, slot: usize, cmds: &mut Vec<Command>) {
        cmds.extend(crate::sstv::commit(&mut self.edit, &self.presets));
        self.selected_slot = slot;
        self.sel_layer = 0;
        self.drag = None;
        self.preview_dirty = true;
    }

    /// The selected slot's picture composed at `dims`, and where each layer
    /// landed. `None` until its source has arrived.
    fn compose_current(
        &mut self,
        dims: (u16, u16),
    ) -> Option<(Vec<u8>, Vec<Option<crate::sstv::LayerBox>>)> {
        let sel = self.selected_slot;
        let slot = self.slots.get(sel).and_then(|s| s.as_ref())?;
        let fresh = self
            .base
            .as_ref()
            .is_some_and(|b| b.slot == sel && b.version == slot.version && b.dims == dims);
        if !fresh {
            self.base = Some(SstvBase {
                slot: sel,
                version: slot.version,
                dims,
                rgb: crate::sstv::crop_scale(&slot.src_rgb, slot.sw, slot.sh, dims.0, dims.1),
            });
        }
        let base = self.base.as_ref()?.rgb.clone();
        let layers = crate::sstv::resolve(self.current_layers(), &self.fields);
        Some(crate::sstv::compose(base, dims.0, dims.1, self.banner.as_ref(), &layers))
    }

    /// Rebuild the transmit preview when the size, slot, or text changed.
    /// `dims` is the transmitted picture size — the SSTV line format's, or the
    /// operator's chosen RIFP size.
    pub(in crate::app) fn ensure_preview(&mut self, dims: (u16, u16), ctx: &egui::Context) {
        if !self.preview_dirty {
            return;
        }
        self.preview_dirty = false;
        match self.compose_current(dims) {
            Some((rgb, boxes)) => {
                let ci = crate::sstv::color_image(&rgb, dims.0, dims.1);
                self.boxes = boxes;
                match &mut self.preview_tex {
                    Some(tex) => tex.set(ci, egui::TextureOptions::NEAREST),
                    None => {
                        self.preview_tex = Some(ctx.load_texture(
                            "sstv_preview",
                            ci,
                            egui::TextureOptions::NEAREST,
                        ))
                    }
                }
            }
            None => {
                self.preview_tex = None;
                self.boxes.clear();
            }
        }
    }

    /// The composed PNG for the current selection, for transmit.
    pub(in crate::app) fn compose_png(&mut self, dims: (u16, u16)) -> Option<Vec<u8>> {
        let (rgb, _) = self.compose_current(dims)?;
        crate::sstv::encode_png(&rgb, dims.0, dims.1)
    }

    /// The transmit preview, with the slot's text layers live on it: click a
    /// layer to pick it, drag it to move it, double-click an empty part of the
    /// picture to add one there.
    fn preview_ui(
        &mut self,
        ui: &mut egui::Ui,
        max: egui::Vec2,
        dims: (u16, u16),
        now: f64,
        cmds: &mut Vec<Command>,
    ) {
        let Some(tex) = self.preview_tex.clone() else {
            self.drag = None;
            let waiting = self.presets.slot(self.selected_slot).has_picture();
            ui.label(
                RichText::new(if waiting {
                    "Fetching this slot's picture…"
                } else {
                    "Load an image into this slot →"
                })
                .size(11.0)
                .weak(),
            );
            return;
        };
        // Allocated and painted by hand rather than `ui.add(Image)`: in the
        // centred, justified box the preview sits in, the response of an added
        // image spans the whole box while the picture is painted smaller in
        // its middle, and every layer box would be mapped to the wrong place.
        let image = egui::Image::new(&tex).max_width(max.x).max_height(max.y);
        let size = image.calc_size(ui.available_size(), Some(tex.size_vec2()));
        let (r, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        image.paint_at(ui, r);
        self.preview_rect = r;
        // Screen points per picture pixel; the image keeps its aspect ratio.
        let k = r.width() / f32::from(dims.0).max(1.0);
        let boxes = self.boxes.clone();
        let screen = |b: &crate::sstv::LayerBox| {
            egui::Rect::from_min_max(
                r.min + egui::vec2(b.x0, b.y0) * k,
                r.min + egui::vec2(b.x1, b.y1) * k,
            )
        };
        let pic = |p: egui::Pos2| (p - r.min) / k;
        // Every layer under the pointer, topmost first.
        let under = |p: egui::Pos2| -> Vec<usize> {
            (0..boxes.len())
                .rev()
                .filter(|&i| boxes[i].is_some_and(|b| screen(&b).expand(3.0).contains(p)))
                .collect()
        };
        // The layer a press grabs: the selected one if the pointer is on it —
        // otherwise a layer picked with its chip could never be dragged out
        // from under one drawn over it — and the topmost one if not.
        let sel = self.sel_layer;
        let grab = |p: egui::Pos2| {
            let u = under(p);
            if u.contains(&sel) { Some(sel) } else { u.first().copied() }
        };
        let hovered = resp.hover_pos().and_then(grab);
        let top = crate::sstv::banner_rows(self.banner.as_ref(), dims.1);

        // The selected layer's four corners, as fractions of its box and as
        // points on screen: where its resize handles are drawn and grabbed.
        let corners: Vec<((f32, f32), egui::Pos2)> = boxes
            .get(sel)
            .copied()
            .flatten()
            .map(|b| {
                let o = screen(&b).expand(2.0);
                [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                    .map(|c| (c, o.min + o.size() * egui::vec2(c.0, c.1)))
                    .to_vec()
            })
            .unwrap_or_default();
        let handle_at = |p: egui::Pos2| {
            corners
                .iter()
                .find(|(_, at)| {
                    egui::Rect::from_center_size(*at, egui::Vec2::splat(HANDLE + 4.0)).contains(p)
                })
                .map(|(c, _)| *c)
        };
        let resize_cursor = |c: (f32, f32)| {
            if c.0 == c.1 { egui::CursorIcon::ResizeNwSe } else { egui::CursorIcon::ResizeNeSw }
        };

        if resp.drag_started() {
            // Where the button went down, not where the pointer is now: egui
            // calls it a drag only once the pointer has moved a few points,
            // which is enough to have left a seven-point handle behind.
            let origin = ui.input(|i| i.pointer.press_origin());
            if let Some(p) = origin.or(resp.interact_pointer_pos()) {
                if let (Some(corner), Some(b0), Some(start)) = (
                    handle_at(p),
                    boxes.get(sel).copied().flatten(),
                    self.current_layers().get(sel),
                ) {
                    let start = start.clone();
                    self.drag = Some(LayerDrag {
                        layer: sel,
                        kind: DragKind::Resize { corner, start, b0 },
                    });
                } else if let Some((i, b)) = grab(p).and_then(|i| boxes[i].map(|b| (i, b))) {
                    self.sel_layer = i;
                    let grab = pic(p) - egui::vec2(b.x0, b.y0);
                    self.drag = Some(LayerDrag { layer: i, kind: DragKind::Move { grab } });
                }
            }
        }
        if let (Some(d), Some(p)) = (self.drag.clone(), resp.interact_pointer_pos()) {
            if resp.dragged() {
                let at = pic(p);
                let b = boxes.get(d.layer).copied().flatten();
                if let Some(l) = self.layers_mut(now, cmds).get_mut(d.layer) {
                    match d.kind {
                        DragKind::Move { grab } => {
                            if let Some(b) = b {
                                let o = at - grab;
                                crate::sstv::place_at(
                                    l,
                                    (o.x, o.y),
                                    (b.x1 - b.x0, b.y1 - b.y0),
                                    dims.0,
                                    dims.1,
                                    top,
                                );
                            }
                        }
                        DragKind::Resize { corner, start, b0 } => crate::sstv::resize_from(
                            l,
                            &start,
                            b0,
                            corner,
                            (at.x, at.y),
                            dims.0,
                            dims.1,
                            top,
                        ),
                    }
                }
            }
        }
        if !resp.dragged() {
            self.drag = None;
        }
        if resp.double_clicked() && hovered.is_none() {
            if let Some(p) = resp.interact_pointer_pos() {
                if self.current_layers().len() < IMAGE_LAYERS_MAX {
                    let at = pic(p);
                    let l = crate::sstv::layer_at("Text", (at.x, at.y), dims.0, dims.1, top);
                    let ls = self.layers_mut(now, cmds);
                    ls.push(l);
                    self.sel_layer = ls.len() - 1;
                    self.focus_text = true;
                }
            }
        } else if resp.clicked() {
            // Clicking the selected layer again where others overlap it steps
            // down through them, so every layer can be reached from the
            // picture, however they are stacked.
            if let Some(p) = resp.interact_pointer_pos() {
                let u = under(p);
                if let Some(at) = u.iter().position(|&i| i == sel) {
                    self.sel_layer = u[(at + 1) % u.len()];
                } else if let Some(&top) = u.first() {
                    self.sel_layer = top;
                }
            }
        }
        // A press on the picture takes the keyboard from the text box, so the
        // arrow keys that follow move the layer rather than the text cursor.
        if resp.clicked() || resp.drag_started() {
            resp.request_focus();
        }
        if resp.has_focus() {
            // Keep the arrows here rather than letting them walk the focus off
            // to the next widget.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
        }

        // Arrow keys nudge the selected layer a picture pixel, ten with
        // Shift — while the picture has the keyboard, or the pointer is over it
        // and the text box does not.
        if !self.text_focus && (resp.has_focus() || resp.hovered()) {
            let d = ui.input_mut(|i| {
                let mut d = egui::Vec2::ZERO;
                for (key, dir) in [
                    (egui::Key::ArrowLeft, egui::vec2(-1.0, 0.0)),
                    (egui::Key::ArrowRight, egui::vec2(1.0, 0.0)),
                    (egui::Key::ArrowUp, egui::vec2(0.0, -1.0)),
                    (egui::Key::ArrowDown, egui::vec2(0.0, 1.0)),
                ] {
                    while i.consume_key(egui::Modifiers::SHIFT, key) {
                        d += dir * 10.0;
                    }
                    while i.consume_key(egui::Modifiers::NONE, key) {
                        d += dir;
                    }
                }
                d
            });
            if d != egui::Vec2::ZERO {
                if let Some(b) = boxes.get(sel).copied().flatten() {
                    if let Some(l) = self.layers_mut(now, cmds).get_mut(sel) {
                        crate::sstv::nudge(l, b, (d.x, d.y), dims.0, dims.1, top);
                    }
                }
            }
        }

        // The boxes: the selected layer's always, with its resize handles; the
        // others while the pointer is over the picture, so the operator can see
        // what there is to grab.
        let painter = ui.painter_at(r.expand(HANDLE));
        for (i, b) in boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            let rect = screen(b).expand(2.0);
            if i == self.sel_layer {
                painter.rect_stroke(
                    rect,
                    2.0,
                    egui::Stroke::new(1.5, crate::theme::CYAN()),
                    egui::StrokeKind::Middle,
                );
            } else if resp.hovered() || self.drag.is_some() {
                let a = if hovered == Some(i) { 170 } else { 70 };
                painter.rect_stroke(
                    rect,
                    2.0,
                    egui::Stroke::new(1.0, Color32::from_white_alpha(a)),
                    egui::StrokeKind::Middle,
                );
            }
        }
        for (_, at) in &corners {
            let h = egui::Rect::from_center_size(*at, egui::Vec2::splat(HANDLE));
            painter.rect_filled(h, 1.0, crate::theme::CYAN());
            painter.rect_stroke(
                h,
                1.0,
                egui::Stroke::new(1.0, Color32::BLACK),
                egui::StrokeKind::Outside,
            );
        }
        match self.drag.as_ref().map(|d| &d.kind) {
            Some(DragKind::Resize { corner, .. }) => {
                ui.ctx().set_cursor_icon(resize_cursor(*corner))
            }
            Some(DragKind::Move { .. }) => ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing),
            None => {
                if let Some(c) = resp.hover_pos().and_then(handle_at) {
                    ui.ctx().set_cursor_icon(resize_cursor(c));
                } else if hovered.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
            }
        }
    }

    /// The selected slot's text layers: a chip per layer, the look of the one
    /// picked, and a text box for what it says.
    fn layer_editor(
        &mut self,
        ui: &mut egui::Ui,
        inner_w: f32,
        below: f32,
        dims: (u16, u16),
        now: f64,
        cmds: &mut Vec<Command>,
    ) {
        let layers = self.current_layers().to_vec();
        let n = layers.len();
        self.sel_layer = self.sel_layer.min(n.saturating_sub(1));
        let sel = self.sel_layer;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            ui.label(RichText::new("Text").size(10.0).weak()).on_hover_text(
                "Each piece of text is its own layer, drawn in this order — the last on \
                 top. Drag one on the picture to move it; double-click the picture to add \
                 one there.",
            );
            for (i, l) in layers.iter().enumerate() {
                let first = l.text.lines().find(|t| !t.trim().is_empty()).unwrap_or("(empty)");
                let mut label = RichText::new(shorten(first.trim(), 14)).size(10.5);
                if !l.visible {
                    label = label.weak().italics();
                }
                if crate::chrome::chip(ui, i == sel, label).clicked() {
                    self.sel_layer = i;
                }
            }
            if n < IMAGE_LAYERS_MAX
                && crate::chrome::chip(ui, false, "+ Add")
                    .on_hover_text("Add a text layer in the middle of the picture")
                    .clicked()
            {
                let l = self.new_centre_layer(dims);
                let ls = self.layers_mut(now, cmds);
                ls.push(l);
                self.sel_layer = ls.len() - 1;
                self.focus_text = true;
            }
        });
        let sel = self.sel_layer;

        if let Some(orig) = layers.get(sel) {
            let mut cur = orig.clone();
            let mut delete = false;
            let mut shift: isize = 0;
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
                ui.label(RichText::new("Size").size(10.0).weak())
                    .on_hover_text("Type size, as a share of the picture's height");
                ui.spacing_mut().slider_width = 90.0;
                crate::chrome::slider(
                    ui,
                    egui::Slider::new(&mut cur.size, TextLayer::SIZE_MIN..=40.0)
                        .step_by(0.5)
                        .show_value(false),
                );
                ui.add_enabled_ui(!cur.rainbow, |ui| {
                    ui.color_edit_button_srgb(&mut cur.ink).on_hover_text("Text colour");
                });
                crate::chrome::checkbox(ui, &mut cur.outline, "Outline").on_hover_text(
                    "An edge around the letters, so they read over any part of the picture",
                );
                ui.add_enabled_ui(cur.outline, |ui| {
                    ui.color_edit_button_srgb(&mut cur.outline_ink).on_hover_text("Outline colour");
                });
                crate::chrome::checkbox(ui, &mut cur.shadow, "Shadow");
                crate::chrome::checkbox(ui, &mut cur.rainbow, "Rainbow");
            });
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(RichText::new("Pin").size(10.0).weak()).on_hover_text(
                    "Which part of the picture the text keeps to. It stays there in every \
                     mode, whatever size the picture is sent at. Dragging picks the \
                     nearest one by itself.",
                );
                anchor_picker(ui, &mut cur);
                ui.add_space(6.0);
                let (vis, vis_hint) = if cur.visible {
                    ("Hide", "Keep this text in the slot but leave it off the picture")
                } else {
                    ("Show", "Put this text back on the picture")
                };
                if crate::chrome::chip(ui, false, vis).on_hover_text(vis_hint).clicked() {
                    cur.visible = !cur.visible;
                }
                if n > 1 {
                    if crate::chrome::chip(ui, false, "▲")
                        .on_hover_text("Draw this text above the next one")
                        .clicked()
                        && sel + 1 < n
                    {
                        shift = 1;
                    }
                    if crate::chrome::chip(ui, false, "▼")
                        .on_hover_text("Draw this text below the previous one")
                        .clicked()
                        && sel > 0
                    {
                        shift = -1;
                    }
                }
                if crate::chrome::chip(ui, false, "Delete").clicked() {
                    delete = true;
                }
            });
            if delete {
                self.layers_mut(now, cmds).remove(sel);
                self.sel_layer = sel.saturating_sub(1);
            } else if shift != 0 {
                let to = (sel as isize + shift) as usize;
                self.layers_mut(now, cmds).swap(sel, to);
                self.sel_layer = to;
            } else if cur != *orig {
                if let Some(l) = self.layers_mut(now, cmds).get_mut(sel) {
                    *l = cur;
                }
            }
            ui.add_space(3.0);
        }

        // What the selected layer says. With no layers yet, typing here makes
        // the first — the slot message this editor grew out of.
        let sel = self.sel_layer;
        let mut buf = self.current_layers().get(sel).map(|l| l.text.clone()).unwrap_or_default();
        let hint: String = std::iter::once("Substituted when the picture is composed:".to_string())
            .chain(crate::sstv::PLACEHOLDERS.iter().map(|(k, what)| format!("  {k} — {what}")))
            .collect::<Vec<_>>()
            .join("\n");
        let text_h = (ui.available_height() - below).max(48.0);
        let resp = ui
            .push_id(("sstv-layer-text", self.selected_slot, sel), |ui| {
                crate::chrome::field_sized(
                    ui,
                    egui::vec2(inner_w, text_h),
                    egui::TextEdit::multiline(&mut buf).hint_text(if n == 0 {
                        "Type to put text on this slot's picture"
                    } else {
                        "This layer's text"
                    }),
                )
            })
            .inner
            .on_hover_text(hint);
        if std::mem::take(&mut self.focus_text) {
            resp.request_focus();
        }
        self.text_focus = resp.has_focus();
        if resp.changed() {
            let ls = self.layers_mut(now, cmds);
            if let Some(l) = ls.get_mut(sel) {
                l.text = buf;
            } else if !buf.is_empty() {
                ls.push(TextLayer { text: buf, ..TextLayer::default() });
                self.sel_layer = self.current_layers().len() - 1;
            }
        }
    }

    /// A fresh layer for "+ Add": in the middle of the picture, or stepped a
    /// line at a time below or above it until it is clear of the layers
    /// already there. Added exactly on top of one, it would hide it — and a
    /// second press of the button would bury the first new layer.
    fn new_centre_layer(&self, dims: (u16, u16)) -> TextLayer {
        let mut l = TextLayer { text: "Text".into(), size: 12.0, ..TextLayer::default() };
        crate::sstv::pin(&mut l, TextAnchor::Center);
        let (w, h) = (f32::from(dims.0), f32::from(dims.1));
        let top = crate::sstv::banner_rows(self.banner.as_ref(), dims.1) as f32;
        let step = l.size * 1.2;
        let taken = |y: f32| {
            let (cx, cy) = (w / 2.0, top + (h - top) / 2.0 + y / 100.0 * h);
            self.boxes
                .iter()
                .flatten()
                .any(|b| b.x0 <= cx && cx <= b.x1 && b.y0 <= cy && cy <= b.y1)
        };
        let reach = ((h - top) / 2.0 / h * 100.0 / step) as i32;
        if let Some(y) = (1..=reach)
            .flat_map(|n| [n as f32 * step, -(n as f32) * step])
            .find(|&y| !taken(y))
            .filter(|_| taken(0.0))
        {
            l.y = y;
        }
        l
    }

    /// Send a picked file to the engine for a slot.
    ///
    /// Nothing is kept here: the engine scales the picture, stores it and
    /// announces the presets, and the slot fills in when that comes back. That
    /// round trip is the point — it is what makes the browser tab and the
    /// console show the same five pictures.
    pub(in crate::app) fn set_slot(
        &mut self,
        slot: usize,
        bytes: Vec<u8>,
        cmds: &mut Vec<Command>,
    ) {
        self.selected_slot = slot;
        // Checked here as well as engine-side: pushing forty megabytes down the
        // socket only to have it refused at the far end is a poor experience,
        // and it keeps a huge upload clear of the WebSocket's frame limit.
        if bytes.len() > IMAGE_UPLOAD_MAX {
            self.pick_error = Some(format!(
                "That picture is {} MB — the limit is {} MB.",
                bytes.len() / 1_048_576,
                IMAGE_UPLOAD_MAX / 1_048_576,
            ));
            return;
        }
        self.pick_error = None;
        cmds.push(Command::ImageSetSlot { slot: slot as u8, bytes });
    }
}

/// A new log entry for an SSTV contact: mode `SSTV` (ADIF has no sub-modes
/// for it), the picture mode and the note as the comment.
fn sstv_log_form(
    q: &sdroxide_types::SstvQso,
    freq_hz: f64,
    picture_mode: &str,
    now: i64,
) -> crate::app::logbook::LogEditForm {
    let mut form = crate::app::logbook::LogEditForm::new_entry(now, freq_hz, "SSTV");
    form.call = q.call.trim().to_uppercase();
    form.rst_sent = q.rsv_sent.trim().to_string();
    form.rst_rcvd = q.rsv_rcvd.trim().to_string();
    form.name = q.name.trim().to_string();
    form.qth = q.qth.trim().to_string();
    form.comment = [picture_mode, q.note.trim()]
        .iter()
        .filter(|t| !t.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    form
}

/// A 3×3 grid of small squares, one per anchor, the layer's own lit.
fn anchor_picker(ui: &mut egui::Ui, layer: &mut TextLayer) {
    let cell = 9.0;
    let gap = 2.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(3.0 * cell + 2.0 * gap, 3.0 * cell + 2.0 * gap),
        egui::Sense::hover(),
    );
    for (i, a) in TextAnchor::ALL.into_iter().enumerate() {
        let min = rect.min + egui::vec2((i % 3) as f32, (i / 3) as f32) * (cell + gap);
        let r = egui::Rect::from_min_size(min, egui::vec2(cell, cell));
        let resp = ui.interact(r, ui.id().with(("sstv-pin", i)), egui::Sense::click());
        let on = layer.anchor == a;
        let fill = if on {
            crate::theme::CYAN()
        } else if resp.hovered() {
            crate::theme::gray(120)
        } else {
            crate::theme::gray(55)
        };
        ui.painter().rect_filled(r, 1.5, fill);
        if resp.clicked() {
            crate::sstv::pin(layer, a);
        }
    }
}

/// One incoming RIFP transfer's chunk map: a lit cell per chunk received, dark
/// per chunk still missing. Beyond what fits, it degrades to a plain bar — the
/// point is to see *where* the holes are, and a thousand one-pixel cells show
/// nothing.
fn rifp_chunk_map(ui: &mut egui::Ui, session: &sdroxide_types::RifpSession) {
    let cells = session.total.max(session.have) as usize;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(120.0, 10.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, crate::theme::gray(20));
    let have = |i: usize| session.map.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 != 0);
    if cells > 0 && cells <= rect.width() as usize {
        let cw = rect.width() / cells as f32;
        for i in 0..cells {
            if !have(i) {
                continue;
            }
            let x = rect.left() + i as f32 * cw;
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(cw.max(1.0), 10.0)),
                0.0,
                crate::theme::GREEN(),
            );
        }
    } else if session.total > 0 {
        let mut fill = rect;
        fill.set_width(rect.width() * (session.have as f32 / session.total as f32).clamp(0.0, 1.0));
        p.rect_filled(fill, 2.0, crate::theme::GREEN());
    }
    p.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, crate::theme::gray(60)),
        egui::StrokeKind::Inside,
    );
    resp.on_hover_text("Chunks received (lit) and still missing (dark)");
}

fn sstv_level_bar(ui: &mut egui::Ui, level: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 10.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, crate::theme::gray(20));
    // Log scale (~ -60..0 dBFS mean-abs) so weak-but-decodable signals still show.
    let db = 20.0 * level.max(1e-6).log10();
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let mut fill = rect;
    fill.set_width(rect.width() * frac);
    let col = if frac > 0.06 { crate::theme::GREEN() } else { crate::theme::gray(45) };
    p.rect_filled(fill, 2.0, col);
    p.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, crate::theme::gray(60)),
        egui::StrokeKind::Inside,
    );
}

impl SdroxideApp {
    /// The image panel, shared by SSTV and RIFP: a live picture and a gallery
    /// on the left, a transmit compositor on the right, and a control strip
    /// that is the only part either mode owns alone.
    pub(in crate::app) fn image_panel(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<Command>,
        mode: Mode,
    ) {
        let ctx = ui.ctx().clone();
        let rifp = mode.is_rifp();
        // The store is the engine's; list it once when the panel first opens.
        // Pictures that arrive afterwards are announced one at a time.
        if !self.sstv.listed {
            self.sstv.listed = true;
            self.sstv.page_pending = true;
            cmds.push(Command::ImageList {
                kind: ImageKind::Sstv,
                offset: 0,
                count: sdroxide_types::IMAGE_PAGE_MAX,
            });
        }
        // Drain a completed file-pick (only consume the target once bytes arrive).
        let picked = self.sstv.inbox.lock().ok().and_then(|mut g| g.take());
        if let Some(bytes) = picked {
            if let Some(target) = self.sstv.pick_target.take() {
                self.sstv.set_slot(target, bytes, cmds);
            }
        }
        // Fetch the selected slot's source when what is cached is not what the
        // engine says is stored — a slot loaded here, or changed from another
        // screen. Asked once, not once per frame.
        let sel = self.sstv.selected_slot;
        let want = self.sstv.presets.slot(sel);
        let have = self.sstv.slots.get(sel).and_then(|s| s.as_ref()).map(|s| s.version);
        if want.has_picture()
            && have != Some(want.version)
            && self.sstv.src_asked.get(&sel) != Some(&want.version)
        {
            self.sstv.src_asked.insert(sel, want.version);
            cmds.push(Command::ImageGetSlot(sel as u8));
        }
        // What the placeholders stand for: the station, the mode, the clock.
        let fields = crate::sstv::Fields::new(
            &self.digi_cfg_edit.my_call,
            &self.digi_cfg_edit.my_grid,
            if rifp { "RIFP" } else { self.sstv.tx_mode.label() },
            crate::time::now_unix(),
        )
        .with_qso(self.sstv.qso());
        if self.sstv.fields != fields {
            self.sstv.fields = fields;
            self.sstv.preview_dirty = true;
        }
        // Keep the banner in sync with the operator config — the callsign it
        // prints, the two templates, the colours and the height.
        let banner = crate::sstv::Banner::from_config(&self.digi_cfg_edit, &self.sstv.fields);
        if self.sstv.banner != banner {
            self.sstv.banner = banner;
            self.sstv.preview_dirty = true;
        }
        let now = ctx.input(|i| i.time);
        self.sstv.now = now;
        self.sstv.sync_edit(now, cmds);
        self.sstv_lookup_call();
        // The transmitted size: SSTV's line format fixes it, RIFP leaves it to
        // the operator. Changing it invalidates the composed preview.
        let dims = if rifp {
            self.digi_cfg_edit.rifp_size.dimensions()
        } else {
            self.sstv.tx_mode.dimensions()
        };
        if self.sstv.preview_dims != dims {
            self.sstv.preview_dims = dims;
            self.sstv.preview_dirty = true;
        }
        self.sstv.ensure_preview(dims, &ctx);
        crate::repaint::after_ms(&ctx, 120);

        let st = self.sstv.status.clone();
        let (signal, tx_active, progress) = if rifp {
            (self.sstv.rifp.signal, self.sstv.rifp.tx_active, self.sstv.rifp.tx_progress)
        } else {
            (st.signal, st.tx_active, st.progress)
        };

        // Whole-panel size. The mode/signal/slant controls sit in a boxed strip
        // on the left above LIVE + RECEIVED; the transmit compositor spans the
        // full height on the right, reclaiming the space the old full-width
        // control rows used to leave empty at the top.
        let avail = ui.available_size();
        let full_h = avail.y;
        let handle_w = 7.0;
        // TRANSMIT (send) column takes a user-draggable fraction of the width; the
        // receive side (LIVE + RECEIVED) gets the rest. Each keeps a usable minimum.
        // A phone takes the receive side and the compositor in turns: each has a
        // 300 pt floor of its own, so together they want twice a phone's width
        // before either has drawn a picture.
        let pane = self.phone_pane(ui, self.state.rx[0].mode);
        let (left_w, tx_w) = match pane {
            // Whichever one is up takes the row.
            Some(0) => (avail.x, 0.0),
            Some(_) => (0.0, avail.x),
            None => {
                let tx = (avail.x * self.view.sstv_tx_fraction)
                    .clamp(300.0, (avail.x - handle_w - 300.0).max(300.0));
                ((avail.x - tx - handle_w).max(300.0), tx)
            }
        };
        // LIVE takes the rest of the receive side; the RECEIVED gallery width is a
        // user-draggable fraction of it (min one thumbnail column).
        let gallery_w = (left_w * self.view.sstv_gallery_fraction)
            .clamp(150.0, (left_w - handle_w - 160.0).max(150.0));
        let live_w = (left_w - gallery_w - handle_w).max(160.0);

        ui.horizontal_top(|ui| {
            // A received thumbnail was clicked → enlarge it (applied after the row).
            let mut enlarge: Option<usize> = None;
            // The gallery asked for the next page.
            let mut more = false;
            // A picture was deleted from the gallery's context menu. By name,
            // not by index: the answer comes back asynchronously and the list
            // may have grown a picture at the front by then.
            let mut delete: Option<String> = None;
            // "Reply to …" was picked on a received picture.
            let mut reply: Option<String> = None;

            // ── LEFT: boxed controls, then LIVE + RECEIVED ──
            if pane.is_none_or(|p| p == 0) {
            ui.allocate_ui_with_layout(
                egui::vec2(left_w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::Frame::new()
                        .fill(crate::theme::ROW_BG())
                        .stroke(egui::Stroke::new(1.0, crate::theme::LINE_LIT()))
                        .inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 7 })
                        .show(ui, |ui| {
                            ui.set_min_width(left_w - 16.0);
                            ui.set_max_width(left_w - 16.0);
                            if rifp {
                                self.rifp_controls(ui, cmds);
                                return;
                            }

                            // Mode selection: Auto + the per-mode chips.
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("SSTV")
                                        .size(12.0)
                                        .strong()
                                        .color(crate::theme::CYAN()),
                                );
                                self.digi_freq_chip(ui, cmds);
                                let auto_label = if self.sstv.auto {
                                    format!("Auto ({})", self.sstv.tx_mode.label())
                                } else {
                                    "Auto".to_string()
                                };
                                if crate::chrome::chip(ui, self.sstv.auto, &auto_label).clicked() {
                                    self.sstv.auto = true;
                                    self.sstv.tx_mode = SstvMode::Martin1;
                                    self.sstv.preview_dirty = true;
                                    cmds.push(Command::SstvSetMode(None));
                                }
                                for m in SstvMode::ALL {
                                    let active = !self.sstv.auto && self.sstv.tx_mode == m;
                                    if crate::chrome::chip(ui, active, m.label()).clicked() {
                                        self.sstv.auto = false;
                                        self.sstv.tx_mode = m;
                                        self.sstv.preview_dirty = true;
                                        cmds.push(Command::SstvSetMode(Some(m)));
                                    }
                                }
                            });
                            ui.add_space(5.0);

                            // Signal meter + activity, and the TX-slant trim.
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Signal").size(10.0).weak());
                                sstv_level_bar(ui, signal);
                                if tx_active {
                                    ui.label(
                                        RichText::new(format!("● TX {:.0}%", progress * 100.0))
                                            .size(11.0)
                                            .strong()
                                            .color(crate::theme::ALERT()),
                                    );
                                } else if st.rx_active {
                                    ui.label(
                                        RichText::new(format!("● RX {:.0}%", st.progress * 100.0))
                                            .size(11.0)
                                            .strong()
                                            .color(crate::theme::GREEN()),
                                    );
                                } else if let Some(bad) = st.unsupported.as_deref() {
                                    // A header came through cleanly for a mode
                                    // this build cannot draw. Without this the
                                    // panel says "listening…" beside a textbook
                                    // signal and the receiver looks broken
                                    // (issue #421).
                                    ui.label(
                                        RichText::new(format!("{bad} — not decoded"))
                                            .size(10.5)
                                            .strong()
                                            .color(crate::theme::ALERT()),
                                    )
                                    .on_hover_text(
                                        "A station is sending in a mode sdroxide does not have. \
                                         The signal and the tuning are fine — there is simply no \
                                         decoder for this one. Nothing to fix at your end.",
                                    );
                                } else if let Some(m) = st.detected {
                                    ui.label(
                                        RichText::new(format!("last: {}", m.label()))
                                            .size(10.0)
                                            .weak(),
                                    );
                                } else {
                                    ui.label(RichText::new("listening…").size(10.0).weak());
                                }

                                // Issue #397. A receiver that has locked on is
                                // committed for the whole length of the mode it
                                // locked on to, and Scottie DX is four and a
                                // half minutes — so a VIS misread as a slow
                                // mode costs every picture sent while it runs
                                // out. On QO-100, where one station follows
                                // another over the same transponder, that is
                                // the next few overs.
                                //
                                // Offered whether or not a picture is under
                                // way: re-arming an idle hunt costs nothing,
                                // and a chip that appears only once the mistake
                                // has been made is one the operator has to find
                                // in a hurry. The half-picture goes with it,
                                // here as well as in the decoder — leaving the
                                // abandoned frame on screen would say the
                                // button had not worked.
                                if crate::chrome::chip(ui, false, "Restart RX")
                                    .on_hover_text(
                                        "Abandon the picture being received and listen for the                                          next header. For a transmission that started decoding                                          in the wrong mode — the receiver is otherwise committed                                          until that mode runs out.",
                                    )
                                    .clicked()
                                {
                                    cmds.push(Command::SstvRestartRx);
                                    self.sstv.rx_color = None;
                                    self.sstv.rx_tex = None;
                                }

                                // Who sent it. The FSK ID arrives in tones a
                                // fraction of a second after the picture, which
                                // is exactly when the operator is looking at the
                                // frame and wondering whose it is — and unlike a
                                // banner drawn into the image, this is the
                                // station's own machine-readable identification.
                                if let Some(id) = st.rx_id.as_deref() {
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(format!("ID {id}"))
                                            .size(11.0)
                                            .strong()
                                            .color(crate::theme::CYAN_DIM()),
                                    )
                                    .on_hover_text(
                                        "The callsign the last station sent as an FSK ID after \
                                         its picture.",
                                    );
                                }

                                ui.add_space(12.0);
                                ui.separator();
                                ui.label(RichText::new("TX slant").size(10.0).weak()).on_hover_text(
                                    "Transmit clock trim (ppm) to remove slant on the far-end decoder",
                                );
                                ui.add_enabled_ui(self.digi_cfg_seeded, |ui| {
                                    ui.spacing_mut().slider_width = 130.0;
                                    let resp = crate::chrome::slider(ui, egui::Slider::new(
                                            &mut self.digi_cfg_edit.sstv_tx_ppm,
                                            -5000.0..=5000.0,
                                        )
                                        .suffix(" ppm")
                                        .fixed_decimals(0),
                                    );
                                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                                        cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                                    }
                                    if ui
                                        .small_button("0")
                                        .on_hover_text("Reset to 0 ppm")
                                        .clicked()
                                    {
                                        self.digi_cfg_edit.sstv_tx_ppm = 0.0;
                                        cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                                    }
                                    ui.separator();
                                    // The callsign in tones after the picture.
                                    // Beside the slant trim rather than in the
                                    // banner window: the banner identifies the
                                    // station to a person looking at the
                                    // picture, this identifies it to the
                                    // repeater decoding it, and the two are set
                                    // for different reasons.
                                    if crate::chrome::checkbox(
                                        ui,
                                        &mut self.digi_cfg_edit.sstv_fsk_id,
                                        "FSK ID",
                                    )
                                    .on_hover_text(
                                        "Send your callsign in tones after each picture — the \
                                         identification SSTV repeaters and other programs read. \
                                         Adds about 2.5 seconds, and sends nothing at all until \
                                         you have set a callsign.",
                                    )
                                    .changed()
                                    {
                                        cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                                    }
                                    ui.separator();
                                    // Dead air before the calibration header, so
                                    // the rig is really on the air by the time
                                    // the VIS code goes out. Here rather than in
                                    // the setup window because it is the same
                                    // kind of per-station trim as the slant
                                    // beside it, and the operator who needs it
                                    // finds out by transmitting.
                                    ui.label(RichText::new("TX lead").size(10.0).weak());
                                    if ui
                                        .add(
                                            egui::DragValue::new(
                                                &mut self.digi_cfg_edit.sstv_txdelay_ms,
                                            )
                                            .range(0..=3000)
                                            .speed(10.0)
                                            .suffix(" ms"),
                                        )
                                        .on_hover_text(
                                            "Silence sent after keying and before the picture's \
                                             leader and VIS code. A decoder that misses any of \
                                             that header shows no picture at all, so this covers \
                                             the gap between asking a rig for PTT and it really \
                                             being on the air. 0 for an SDR that keys instantly.",
                                        )
                                        .changed()
                                    {
                                        cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
                                    }
                                });
                            });
                        });
                    ui.add_space(6.0);

                    // LIVE + RECEIVED fill the remaining height of the left column.
                    //
                    // Stacked on a phone rather than side by side: halving 377
                    // points leaves two columns too narrow to show a picture in,
                    // and a received picture is the whole point of the mode.
                    let stack = pane.is_some();
                    let row_h = ui.available_height().max(160.0);
                    let (row_h, live_w, gallery_w) = if stack {
                        let w = ui.available_width();
                        ((row_h / 2.0).max(120.0), w, w)
                    } else {
                        (row_h, live_w, gallery_w)
                    };
                    let lay = if stack {
                        egui::Layout::top_down(egui::Align::Min)
                    } else {
                        egui::Layout::left_to_right(egui::Align::Min)
                    };
                    ui.with_layout(lay, |ui| {
                        // LIVE: the picture currently decoding, shown large.
                        sstv_section(ui, "LIVE", egui::vec2(live_w, row_h), |ui| {
                            ui.centered_and_justified(|ui| {
                                if let Some(tex) = &self.sstv.rx_tex {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .max_height(row_h - 34.0)
                                            .max_width(live_w - 16.0),
                                    );
                                } else {
                                    let msg = if rifp {
                                        // RIFP only paints live from the raw
                                        // raster; anything else appears whole.
                                        "waiting for a picture…"
                                    } else if signal > 0.0008 {
                                        "waiting for a signal…"
                                    } else {
                                        "no / low audio"
                                    };
                                    ui.label(RichText::new(msg).size(11.0).weak());
                                }
                            });
                        });
                        // Draggable vertical divider between LIVE and RECEIVED.
                        if !stack {
                            let hresp =
                                crate::chrome::split_handle(ui, egui::vec2(handle_w, row_h), None);
                            if hresp.dragged() {
                                // Dragging right shrinks the gallery (grows LIVE).
                                let d = hresp.drag_delta().x / left_w.max(1.0);
                                self.view.sstv_gallery_fraction =
                                    (self.view.sstv_gallery_fraction - d).clamp(0.2, 0.6);
                            }
                        }

                        // RECEIVED: narrow multi-column gallery of decoded
                        // pictures, thumbnailed by the engine out of its own
                        // store — so it is the same gallery here, on a browser
                        // tab, and on a client dialled in from anywhere else.
                        sstv_section(ui, "RECEIVED", egui::vec2(gallery_w, row_h), |ui| {
                            if self.sstv.received.is_empty() {
                                let msg = if self.sstv.page_pending {
                                    "Reading the radio's pictures…".to_string()
                                } else if self.sstv.dir.is_empty() {
                                    "Decoded pictures collect here.".to_string()
                                } else {
                                    format!(
                                        "Decoded pictures are saved {} and collect here.",
                                        self.store_where(&self.sstv.dir),
                                    )
                                };
                                ui.label(RichText::new(msg).size(11.0).weak());
                                return;
                            }
                            let thumb = egui::vec2(112.0, 90.0);
                            let shown = self.sstv.received.len() as u32;
                            let older = self.sstv.total.saturating_sub(shown);
                            egui::ScrollArea::vertical()
                                .id_salt("sstv-gallery")
                                .max_height(row_h - 24.0)
                                .auto_shrink([false, false])
                                .show_themed(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                                        for (i, r) in self.sstv.received.iter().enumerate() {
                                            let resp = ui
                                                .add(
                                                    egui::Image::new(&r.thumb)
                                                        .fit_to_exact_size(thumb)
                                                        .corner_radius(2.0)
                                                        .sense(egui::Sense::click()),
                                                )
                                                .on_hover_text(match &r.entry.sender {
                                                    Some(c) => format!(
                                                        "From {c} · click to enlarge · \
                                                         right-click to reply or delete"
                                                    ),
                                                    None => "Click to enlarge · right-click to \
                                                             delete"
                                                        .to_string(),
                                                });
                                            if resp.clicked() {
                                                enlarge = Some(i);
                                            }
                                            // A menu rather than an ✕ on the
                                            // thumbnail: a picture is 112 points
                                            // across in a wrapped grid, and a
                                            // delete target that close to the
                                            // one for opening it would be pressed
                                            // by accident.
                                            resp.context_menu(|ui| {
                                                ui.label(
                                                    RichText::new(shorten(&r.entry.name, 32))
                                                        .size(10.0)
                                                        .weak(),
                                                );
                                                if let Some(c) = &r.entry.sender {
                                                    if ui
                                                        .button(format!("Reply to {c}"))
                                                        .on_hover_text(
                                                            "Start a contact with this station: \
                                                             its call goes in the QSO box, and \
                                                             the rest of the box is emptied",
                                                        )
                                                        .clicked()
                                                    {
                                                        reply = Some(c.clone());
                                                        ui.close();
                                                    }
                                                }
                                                if ui.button("Delete this picture").clicked() {
                                                    delete = Some(r.entry.name.clone());
                                                    ui.close();
                                                }
                                            });
                                        }
                                    });
                                    // The rest of the store is a request away.
                                    // Explicit rather than automatic on scroll:
                                    // a page is a few hundred kilobytes and the
                                    // operator may be on a phone link.
                                    if older > 0 && self.sstv.received.len() >= GALLERY_MAX {
                                        ui.add_space(4.0);
                                        ui.label(
                                            RichText::new(format!("{older} older in the store"))
                                                .size(9.5)
                                                .weak(),
                                        );
                                    } else if older > 0 {
                                        ui.add_space(4.0);
                                        let label = if self.sstv.page_pending {
                                            "loading…".to_string()
                                        } else {
                                            format!("{older} older — load more")
                                        };
                                        if crate::chrome::chip(ui, false, label).clicked()
                                            && !self.sstv.page_pending
                                        {
                                            more = true;
                                        }
                                    }
                                });
                        });
                    });
                },
            );

            }

            // Draggable vertical divider between the receive side and the
            // TRANSMIT (send) column — mirrors the FT8 decode/QSO splitter.
            if pane.is_none() {
                let hresp = crate::chrome::split_handle(ui, egui::vec2(handle_w, full_h), None);
                if hresp.dragged() {
                    // Dragging right shrinks the TX column (grows the receive side).
                    let d = hresp.drag_delta().x / avail.x.max(1.0);
                    self.view.sstv_tx_fraction = (self.view.sstv_tx_fraction - d).clamp(0.22, 0.6);
                }
            }

            // ── RIGHT: transmit compositor, full height ──
            if pane.is_none_or(|p| p != 0) {
            ui.allocate_ui(egui::vec2(tx_w, full_h), |ui| {
                sstv_section(ui, "TRANSMIT", egui::vec2(tx_w, full_h), |ui| {
                    // The compositor is a fixed stack — the five slots, the
                    // load buttons, the preview, the message box and the
                    // transmit controls — and none of it can be dropped
                    // without dropping a control. On a screen with less height
                    // than the stack needs it used to run off the bottom edge
                    // and simply not be there (issue #231). Scrolled, the whole
                    // stack stays reachable, and where there is room the bar
                    // never appears.
                    egui::ScrollArea::vertical()
                        .id_salt("sstv-transmit")
                        .auto_shrink([false, false])
                        .show_themed(ui, |ui| {
                        let inner_w = tx_w - 16.0;

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            for i in 0..IMAGE_SLOTS {
                                let sel = self.sstv.selected_slot == i;
                                let size = egui::vec2(70.0, 54.0);
                                let resp = if let Some(tex) =
                                    self.sstv.slot_thumbs.get(i).and_then(|t| t.as_ref())
                                {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .fit_to_exact_size(size)
                                            .corner_radius(2.0)
                                            .sense(egui::Sense::click()),
                                    )
                                } else {
                                    let (rect, resp) =
                                        ui.allocate_exact_size(size, egui::Sense::click());
                                    ui.painter().rect_stroke(
                                        rect,
                                        2.0,
                                        egui::Stroke::new(1.0, crate::theme::gray(70)),
                                        egui::StrokeKind::Inside,
                                    );
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "+",
                                        egui::FontId::proportional(22.0),
                                        crate::theme::gray(110),
                                    );
                                    resp
                                };
                                // Active-tab highlight: a cyan wash + heavier border so
                                // it is obvious which slot the text editor targets.
                                if sel {
                                    ui.painter().rect_filled(
                                        resp.rect,
                                        2.0,
                                        Color32::from_rgba_unmultiplied(0x00, 0xd0, 0xf4, 34),
                                    );
                                    ui.painter().rect_stroke(
                                        resp.rect,
                                        2.0,
                                        egui::Stroke::new(2.5, crate::theme::CYAN()),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                                // Slot number badge (1..5), like a tab label.
                                let badge = egui::Rect::from_min_size(
                                    resp.rect.left_top() + egui::vec2(2.0, 2.0),
                                    egui::vec2(15.0, 13.0),
                                );
                                ui.painter().rect_filled(badge, 2.0, Color32::from_black_alpha(150));
                                ui.painter().text(
                                    badge.center(),
                                    egui::Align2::CENTER_CENTER,
                                    format!("{}", i + 1),
                                    egui::FontId::proportional(10.0),
                                    if sel { crate::theme::CYAN() } else { crate::theme::gray(170) },
                                );
                                let resp = resp.on_hover_text(
                                    "Click to edit this slot's text · double-click to load an image",
                                );
                                if resp.double_clicked() {
                                    self.sstv.pick_target = Some(i);
                                    pick_image(self.sstv.inbox.clone());
                                } else if resp.clicked() && !sel {
                                    self.sstv.select_slot(i, cmds);
                                }
                            }
                        });
                        ui.add_space(5.0);

                        // Load/replace/clear the active slot's picture.
                        ui.horizontal(|ui| {
                            let sel = self.sstv.selected_slot;
                            let has_img = self.sstv.presets.slot(sel).has_picture();
                            let label = if has_img { "Change image…" } else { "Load image…" };
                            if crate::chrome::chip(ui, false, label).clicked() {
                                self.sstv.pick_target = Some(sel);
                                pick_image(self.sstv.inbox.clone());
                            }
                            // Two presses, like deleting from the gallery: the
                            // stored picture goes for good, and a lone "Clear"
                            // here read as clearing whatever was nearest — the
                            // QSO box below, say. Armed per slot and only for a
                            // few seconds, so it cannot go off long after.
                            if has_img {
                                let armed = self
                                    .sstv
                                    .confirm_remove
                                    .is_some_and(|(s, t)| s == sel && now - t < 4.0);
                                let (text, hint) = if armed {
                                    ("Really remove?", "Click again to delete this slot's picture")
                                } else {
                                    (
                                        "Remove image",
                                        "Delete this slot's picture. Its text layers stay, and \
                                         come back over the next picture you load.",
                                    )
                                };
                                if crate::chrome::chip(ui, armed, text).on_hover_text(hint).clicked()
                                {
                                    if armed {
                                        self.sstv.confirm_remove = None;
                                        cmds.push(Command::ImageClearSlot(sel as u8));
                                    } else {
                                        self.sstv.confirm_remove = Some((sel, now));
                                    }
                                }
                            }
                            // The banner belongs to the station, not to the slot:
                            // one editor, reached from whichever slot is in front.
                            if crate::chrome::chip(ui, self.sstv.banner_open, "Banner…")
                                .on_hover_text(
                                    "What is printed across the top of every picture this station \
                                     sends",
                                )
                                .clicked()
                            {
                                self.sstv.banner_open = !self.sstv.banner_open;
                            }
                        });
                        if let Some(err) = &self.sstv.pick_error {
                            ui.label(RichText::new(err).size(10.0).color(crate::theme::YELLOW()));
                        }
                        ui.add_space(4.0);

                        // Who the contact is with: what `{tocall}` and friends
                        // print, and what a log entry is filled from.
                        self.sstv_qso_strip(ui, now, cmds);
                        ui.add_space(6.0);

                        // Preview gets a capped share of the height; the text box
                        // grows to fill whatever's left above the buttons.
                        let btn_h = 42.0;
                        let gap = 6.0;
                        let preview_h = (ui.available_height() * 0.45).clamp(80.0, 260.0);
                        egui::Frame::new()
                            .fill(crate::theme::gray(6))
                            .stroke(egui::Stroke::new(1.0, crate::theme::LINE_LIT()))
                            .inner_margin(2.0)
                            .show(ui, |ui| {
                                ui.set_min_size(egui::vec2(inner_w, preview_h));
                                ui.set_max_size(egui::vec2(inner_w, preview_h));
                                ui.centered_and_justified(|ui| {
                                    self.sstv.preview_ui(
                                        ui,
                                        egui::vec2(inner_w - 4.0, preview_h - 4.0),
                                        dims,
                                        now,
                                        cmds,
                                    );
                                });
                            });
                        ui.add_space(gap);

                        // The slot's text layers: which one, how it looks, and
                        // what it says — the text box filling the height above
                        // the buttons.
                        // A picture about to go out with a hole in it: a
                        // `{tocall}` with no call in the QSO box.
                        let empty = self.sstv.empty_placeholders();
                        let warn_h = if empty.is_empty() { 0.0 } else { 18.0 };
                        self.sstv.layer_editor(ui, inner_w, btn_h + gap + warn_h, dims, now, cmds);
                        ui.add_space(gap);
                        if !empty.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "⚠ {} empty — fill it in the QSO box",
                                    empty.join(", ")
                                ))
                                .size(10.5)
                                .color(crate::theme::YELLOW()),
                            )
                            .on_hover_text(
                                "The picture's text uses a placeholder with nothing to print. It \
                                 will be sent with a gap where it goes.",
                            );
                        }

                        // Large cut-corner TX / ABORT buttons.
                        ui.horizontal(|ui| {
                            // The source has to have arrived as well as be stored:
                            // pressing TX while it is still on its way would compose
                            // nothing and look like the button was broken. The radio
                            // has to have a transmitter too — composing a picture is
                            // worth doing on a receiver, sending it is not.
                            let tx_ok = self.tx_capable();
                            let can_tx = tx_ok
                                && self
                                    .sstv
                                    .slots
                                    .get(self.sstv.selected_slot)
                                    .is_some_and(|s| s.is_some())
                                && !tx_active;
                            let tx = ui
                                .add_enabled_ui(can_tx, |ui| {
                                    rx_only_hint(
                                        crate::chrome::chip_accent(
                                            ui,
                                            can_tx,
                                            RichText::new("   TX   ").size(16.0).strong(),
                                            crate::theme::ALERT(),
                                            Color32::WHITE,
                                        ),
                                        tx_ok,
                                    )
                                })
                                .inner;
                            if tx.clicked() {
                                // Compose from what is on screen, including an edit
                                // not yet sent, and send that edit now so the
                                // engine stores what went on the air.
                                let png = self.sstv.compose_png(dims);
                                cmds.extend(crate::sstv::send_now(
                                    &mut self.sstv.edit,
                                    &self.sstv.presets,
                                    now,
                                ));
                                cmds.extend(
                                    crate::sstv::held_send_now(
                                        &mut self.sstv.qso_edit,
                                        &self.sstv.presets.qso,
                                        now,
                                    )
                                    .map(Command::ImageSetQso),
                                );
                                if let Some(png) = png {
                                    cmds.push(if rifp {
                                        Command::RifpTx { png }
                                    } else {
                                        Command::SstvTx { mode: self.sstv.tx_mode, png }
                                    });
                                }
                            }
                            ui.add_space(8.0);
                            let abort = ui
                                .add_enabled_ui(tx_active, |ui| {
                                    crate::chrome::chip(
                                        ui,
                                        false,
                                        RichText::new(" ABORT TX ").size(15.0).strong(),
                                    )
                                })
                                .inner;
                            if abort.clicked() {
                                cmds.push(Command::DigiAbortTx);
                            }
                        });
                        });
                });
            });
            }

            if let Some(i) = enlarge {
                self.sstv.enlarged = Some(i);
            }
            if more {
                self.sstv.page_pending = true;
                cmds.push(Command::ImageList {
                    kind: ImageKind::Sstv,
                    offset: self.sstv.received.len() as u32,
                    count: sdroxide_types::IMAGE_PAGE_MAX,
                });
            }
            // The gallery entry goes when the engine says the file has, not
            // here: the store is on the radio's machine, and a thumbnail that
            // vanished from a delete that then failed would be a lie.
            if let Some(name) = delete {
                cmds.push(Command::ImageDelete { kind: ImageKind::Sstv, name });
            }
            // Chosen by the operator, so theirs: a new contact, touched, that
            // the next station's ID will not take away.
            if let Some(call) = reply {
                *crate::sstv::held_edit(&mut self.sstv.qso_edit, &self.sstv.presets.qso, now) =
                    sdroxide_types::SstvQso {
                        call,
                        call_from: sdroxide_types::CallSource::FskId,
                        touched: true,
                        ..Default::default()
                    };
                self.sstv.preview_dirty = true;
            }
        });

        self.banner_window(&ctx, cmds);

        // Enlarged view of a clicked received image (overlay window).
        if let Some(idx) = self.sstv.enlarged {
            let mut open = true;
            let mut save = false;
            let mut pressed_delete = false;
            if let Some(r) = self.sstv.received.get(idx) {
                // The full-size picture lives on the radio. Ask for it the
                // moment one is opened, and show the thumbnail scaled up in the
                // meantime rather than an empty window. Asked once per picture,
                // whatever the answer — a fetch that fails must not become a
                // request every frame.
                if r.full.is_none() && self.sstv.full_asked.as_deref() != Some(&r.entry.name) {
                    self.sstv.full_asked = Some(r.entry.name.clone());
                    self.sstv.full_gone = false;
                    cmds.push(Command::ImageGet {
                        kind: ImageKind::Sstv,
                        name: r.entry.name.clone(),
                    });
                }
                // A picture can be stored before the first listing has come
                // back, so the directory is not always known to name.
                let del_hint = if self.sstv.dir.is_empty() {
                    "Delete this picture from the store".to_string()
                } else {
                    format!("Delete this picture {}", self.store_where(&self.sstv.dir))
                };
                let r = &self.sstv.received[idx];
                let savable = self.sstv.full_png.as_ref().is_some_and(|(n, _)| *n == r.entry.name);
                let armed = self.sstv.confirm_delete.as_deref() == Some(&r.entry.name);
                egui::Window::new("Received image")
                    .id(crate::layout::salted_id(&ctx, "Received image"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([
                        crate::layout::window_w(&ctx, 660.0),
                        crate::layout::window_h(&ctx, 528.0),
                    ])
                    .frame(crate::chrome::window_frame())
                    .show(&ctx, |ui| {
                        crate::chrome::window_body_bg(ui);
                        // Scale up to fill the window width (preserving aspect).
                        // A thumbnail stands in until the real one arrives, at
                        // the size the real one will be, so nothing jumps.
                        let tex = r.full.as_ref().unwrap_or(&r.thumb);
                        let native =
                            egui::vec2(f32::from(r.entry.width), f32::from(r.entry.height));
                        let native = if native.x > 0.0 { native } else { tex.size_vec2() };
                        let avail_w = ui.available_width().min(1000.0);
                        let scale = (avail_w / native.x.max(1.0)).clamp(1.0, 4.0);
                        ui.add(egui::Image::new(tex).fit_to_exact_size(native * scale));
                        ui.horizontal(|ui| {
                            if self.sstv.full_gone {
                                ui.label(
                                    RichText::new("no longer in the store")
                                        .size(10.0)
                                        .color(crate::theme::YELLOW()),
                                );
                            } else if r.full.is_none() {
                                ui.label(RichText::new("loading full size…").size(10.0).weak());
                            } else if savable
                                && crate::chrome::chip(ui, false, "Save picture…")
                                    .on_hover_text("Save a copy on this computer")
                                    .clicked()
                            {
                                save = true;
                            }
                            // Two presses, the second one red: the file goes
                            // from the radio's disk and there is nothing to
                            // undo it with.
                            let del = if armed {
                                crate::chrome::chip_accent(
                                    ui,
                                    true,
                                    "Delete — sure?",
                                    crate::theme::PINK(),
                                    crate::theme::INK_ON_CYAN(),
                                )
                                .on_hover_text("Click again to delete it for good")
                            } else {
                                crate::chrome::chip(ui, false, "Delete…").on_hover_text(&del_hint)
                            };
                            if del.clicked() {
                                pressed_delete = true;
                            }
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}×{}",
                                    r.entry.name, r.entry.width, r.entry.height
                                ))
                                .size(10.0)
                                .weak(),
                            );
                        });
                        // RIFP knows where a picture came from and how it was
                        // carried; SSTV knows none of that, and says nothing.
                        if let Some(m) = &r.entry.rifp {
                            ui.add_space(4.0);
                            let from = m.sender.as_deref().unwrap_or("unidentified");
                            ui.label(
                                RichText::new(format!(
                                    "{from} · {} · {}×{} {}-bit · {} / {} · {} octets in {} chunks \
                                     ({} first pass) · session {}",
                                    m.filename,
                                    m.width,
                                    m.height,
                                    m.bits_per_pixel,
                                    m.media_type,
                                    m.content_encoding,
                                    m.encoded_size,
                                    m.chunk_count,
                                    m.chunks_first_pass,
                                    m.session,
                                ))
                                .size(10.5)
                                .weak(),
                            );
                            if let Some(hint) = &m.hint {
                                ui.label(RichText::new(hint).size(11.0).italics());
                            }
                        }
                    });
            } else {
                open = false;
            }
            if save {
                if let Some((name, png)) = &self.sstv.full_png {
                    crate::download::save_as(name, png, crate::download::Mime::Png);
                }
            }
            // First press arms the chip, second sends it. The gallery entry
            // stays until the engine confirms the file is gone.
            if pressed_delete {
                if let Some(name) = self.sstv.received.get(idx).map(|r| r.entry.name.clone()) {
                    if self.sstv.confirm_delete.as_deref() == Some(name.as_str()) {
                        self.sstv.confirm_delete = None;
                        cmds.push(Command::ImageDelete { kind: ImageKind::Sstv, name });
                    } else {
                        self.sstv.confirm_delete = Some(name);
                    }
                }
            }
            if !open {
                self.sstv.enlarged = None;
                self.sstv.full_asked = None;
                self.sstv.full_gone = false;
                self.sstv.confirm_delete = None;
            }
        }
    }

    /// Look up the QSO box's call, once per call, when it has stopped being
    /// typed — a lookup per keystroke would ask about "E", "EA" and "EA3" on
    /// the way to the call. A call already looked up this session is answered
    /// from the cache without asking again.
    fn sstv_lookup_call(&mut self) {
        if self.sstv.qso_focus {
            return;
        }
        let call = self.sstv.qso().call.trim().to_uppercase();
        if call == self.sstv.looked_up {
            return;
        }
        self.sstv.looked_up = call.clone();
        if call.len() < 3 {
            return;
        }
        match self.callsign_cache.get(&call).cloned() {
            Some(info) => self.sstv_apply_lookup(&info),
            None => {
                self.queue_lookup(call);
            }
        }
    }

    /// A callsign lookup answered: fill the QSO box's blank name and QTH, if
    /// it is about the call in the box. Filled by the program, so the contact
    /// stays untouched — the next station's ID may still replace it — and an
    /// answer about a call the box has since moved on from is ignored.
    pub(in crate::app) fn sstv_apply_lookup(&mut self, info: &sdroxide_types::CallsignInfo) {
        self.sstv.apply_lookup(info);
    }

    /// A log entry was saved: if it is the SSTV contact in progress, that
    /// contact is finished, and the next station's ID may start a new one.
    pub(in crate::app) fn sstv_mark_logged(&mut self, call: &str) {
        self.sstv.mark_logged(call);
    }

    /// The QSO box: the other station's call, the reports both ways, their
    /// name and a note — what the pictures' `{tocall}`, `{rsv}`, `{rsvr}`,
    /// `{name}` and `{note}` print — and the two things done with a contact:
    /// log it, and clear it for the next.
    fn sstv_qso_strip(&mut self, ui: &mut egui::Ui, now: f64, cmds: &mut Vec<Command>) {
        use sdroxide_types::CallSource;
        let shown = self.sstv.qso().clone();
        let mut cur = shown.clone();
        let mut focus = false;
        let mut log = false;
        let mut cleared = false;
        // Framed, and titled, so it reads as its own thing and not as more of
        // the slot's picture controls on the row above it.
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, crate::theme::gray(45)))
            .corner_radius(3.0)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
                    let title = RichText::new("QSO").size(10.0).strong();
                    ui.label(title.color(crate::theme::CYAN_DIM())).on_hover_text(
                        "The contact in progress. The pictures' text can print it, and Log… \
                         fills a log entry from it.",
                    );
                    let field = |ui: &mut egui::Ui, text: &mut String, w: f32, hint: &str| {
                        crate::chrome::field(
                            ui,
                            egui::TextEdit::singleline(text).desired_width(w).hint_text(hint),
                        )
                    };
                    let label =
                        |ui: &mut egui::Ui, t: &str| ui.label(RichText::new(t).size(10.0).weak());

                    label(ui, "To").on_hover_text("The station this contact is with — {tocall}");
                    let r = field(ui, &mut cur.call, 78.0, "call");
                    focus |= r.has_focus();
                    if r.changed() {
                        cur.call_from = CallSource::Typed;
                    }
                    if cur.call_from != CallSource::Typed {
                        let what = if cur.call_from == CallSource::FskId { "FSK" } else { "CW" };
                        ui.label(RichText::new(what).size(9.5).color(crate::theme::CYAN_DIM()))
                            .on_hover_text(
                                "Filled in from the ID this station sent after its picture",
                            );
                    }
                    // Another station identified while this contact was the
                    // operator's: theirs to take, not the program's to swap in.
                    if let Some(o) = cur.offered.clone() {
                        let offer = RichText::new(format!("← ID {o}")).size(10.5);
                        if crate::chrome::chip(ui, false, offer)
                            .on_hover_text(format!(
                                "{o} sent an FSK ID. The contact is yours (you typed in it), so \
                                 it was left alone — click to make {o} the call"
                            ))
                            .clicked()
                        {
                            cur.call = o;
                            cur.call_from = CallSource::FskId;
                            cur.offered = None;
                        }
                        if crate::chrome::chip(ui, false, RichText::new("×").size(10.5))
                            .on_hover_text("Ignore this ID")
                            .clicked()
                        {
                            cur.offered = None;
                        }
                    }

                    label(ui, "RSV").on_hover_text(
                        "The report you send them — {rsv}. Readability 1–5, strength 1–9, \
                         video 1–5.",
                    );
                    focus |= field(ui, &mut cur.rsv_sent, 30.0, "595").has_focus();
                    ui.menu_button(RichText::new("▾").size(10.0), |ui| {
                        for v in sdroxide_types::SstvQso::RSV_PRESETS {
                            if ui.button(v).clicked() {
                                cur.rsv_sent = v.to_string();
                                ui.close();
                            }
                        }
                    });
                    label(ui, "rcvd").on_hover_text("The report they sent you — {rsvr}");
                    focus |= field(ui, &mut cur.rsv_rcvd, 30.0, "").has_focus();

                    label(ui, "Name").on_hover_text("Their name — {name}");
                    focus |= field(ui, &mut cur.name, 70.0, "").has_focus();
                    label(ui, "QTH").on_hover_text("Where they are — {qth}");
                    focus |= field(ui, &mut cur.qth, 80.0, "").has_focus();
                    label(ui, "Note").on_hover_text("Anything else to print or log — {note}");
                    focus |= field(ui, &mut cur.note, 90.0, "").has_focus();

                    if cur.logged {
                        ui.label(RichText::new("✓ logged").size(10.0).color(crate::theme::GREEN()))
                            .on_hover_text(
                                "This contact is in the log. The next station's ID starts a new \
                                 one",
                            );
                    }
                    if crate::chrome::chip(ui, false, "Log…")
                        .on_hover_text(
                            "Open a log entry filled in from this contact, to check and save",
                        )
                        .clicked()
                    {
                        log = true;
                    }
                    if crate::chrome::chip(ui, false, "Clear QSO")
                        .on_hover_text(
                            "Empty these fields for the next contact (the report sent goes back \
                             to 595). The slots' pictures and text are not touched.",
                        )
                        .clicked()
                    {
                        cur = sdroxide_types::SstvQso::default();
                        cleared = true;
                    }
                });
            });
        self.sstv.qso_focus = focus;
        // Everything that changes the box from here is the operator's doing,
        // so the contact becomes theirs: an ID on the air is only offered now.
        // Clearing is the exception — it hands the box back to the program.
        if cur != shown && !cleared {
            cur.touched = true;
            cur.logged = false;
        }
        if cur != shown {
            *crate::sstv::held_edit(&mut self.sstv.qso_edit, &self.sstv.presets.qso, now) = cur;
            self.sstv.preview_dirty = true;
        }
        if log {
            // Stored now, so every screen agrees with what was logged.
            cmds.extend(
                crate::sstv::held_send_now(&mut self.sstv.qso_edit, &self.sstv.presets.qso, now)
                    .map(Command::ImageSetQso),
            );
            self.log_sstv_qso();
        }
    }

    /// Open a log entry filled in from the SSTV contact in progress: the call,
    /// both reports, the name, and the picture mode and note as the comment.
    /// Opened for checking rather than saved outright — a call read off an ID
    /// is worth a look before it goes in the log for good.
    fn log_sstv_qso(&mut self) {
        let mode = if self.state.rx[0].mode.is_rifp() { "RIFP" } else { self.sstv.tx_mode.label() };
        let form =
            sstv_log_form(self.sstv.qso(), self.on_air_freq_hz(), mode, crate::time::now_unix());
        let call = form.call.clone();
        self.log_edit = Some(form);
        self.show_logbook = true;
        if !call.is_empty() {
            self.queue_lookup(call);
        }
    }

    /// The editor for the banner drawn across the top of every transmitted
    /// picture: the two texts, the two colours, and how tall the strip is.
    ///
    /// (The QSO box is [`Self::sstv_qso_strip`], just below.)
    ///
    /// A window rather than another row in the transmit column, because this is
    /// set once for the station and then left alone, and the column it would
    /// have gone in is already the narrow half of a split panel.
    fn banner_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        if !self.sstv.banner_open {
            return;
        }
        let seeded = self.digi_cfg_seeded;
        let mut open = true;
        let mut changed = false;
        egui::Window::new("Image banner")
            .id(crate::layout::salted_id(ctx, "Image banner"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(crate::layout::window_w(ctx, 420.0))
            .frame(crate::chrome::window_frame())
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                ui.label(
                    RichText::new(
                        "Drawn into every picture this station transmits, over the slot's own \
                         message.",
                    )
                    .size(10.5)
                    .weak(),
                );
                ui.add_space(6.0);
                let cfg = &mut self.digi_cfg_edit;
                ui.add_enabled_ui(seeded, |ui| {
                    if crate::chrome::checkbox(ui, &mut cfg.sstv_banner, "Draw the banner")
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.add_enabled_ui(cfg.sstv_banner, |ui| {
                        // The placeholder list is the whole documentation for
                        // these two fields, so it hangs off both of them.
                        let hint: String = std::iter::once(
                            "Substituted when the picture is composed:".to_string(),
                        )
                        .chain(
                            crate::sstv::PLACEHOLDERS
                                .iter()
                                .map(|(k, what)| format!("  {k} — {what}")),
                        )
                        .collect::<Vec<_>>()
                        .join("\n");
                        egui::Grid::new("sstv-banner-grid")
                            .num_columns(2)
                            .spacing([10.0, 7.0])
                            .show(ui, |ui| {
                                ui.label("Top left");
                                let resp = crate::chrome::field(
                                    ui,
                                    egui::TextEdit::singleline(&mut cfg.sstv_banner_left)
                                        .desired_width(240.0)
                                        .hint_text("{call}"),
                                )
                                .on_hover_text(&hint);
                                // On focus loss, like the RIFP caption: this is
                                // persisted engine-side and a config write per
                                // keystroke would be absurd. The preview still
                                // follows every keystroke, because it is
                                // recomposed from `digi_cfg_edit` and not from
                                // what the engine has heard about.
                                if resp.lost_focus() && resp.changed() {
                                    changed = true;
                                }
                                ui.end_row();

                                ui.label("Top right");
                                let resp = crate::chrome::field(
                                    ui,
                                    egui::TextEdit::singleline(&mut cfg.sstv_banner_right)
                                        .desired_width(240.0)
                                        .hint_text("SDRoxide v{version}"),
                                )
                                .on_hover_text(&hint);
                                if resp.lost_focus() && resp.changed() {
                                    changed = true;
                                }
                                ui.end_row();

                                ui.label("Colours");
                                ui.horizontal(|ui| {
                                    changed |= ui
                                        .color_edit_button_srgb(&mut cfg.sstv_banner_fill)
                                        .on_hover_text(
                                            "The strip, at its top edge — it fades to black, or \
                                             to the gradient colour below it",
                                        )
                                        .changed();
                                    ui.label(RichText::new("strip").size(10.5).weak());
                                    ui.add_space(8.0);
                                    changed |= ui
                                        .color_edit_button_srgb(&mut cfg.sstv_banner_ink)
                                        .on_hover_text("Both texts")
                                        .changed();
                                    ui.label(RichText::new("text").size(10.5).weak());
                                });
                                ui.end_row();

                                ui.label("Gradient");
                                ui.horizontal(|ui| {
                                    changed |= crate::chrome::checkbox(
                                        ui,
                                        &mut cfg.sstv_style.banner_gradient,
                                        "fade to",
                                    )
                                    .on_hover_text(
                                        "Fade the strip from its top colour to a second one \
                                         instead of to black.",
                                    )
                                    .changed();
                                    ui.add_enabled_ui(cfg.sstv_style.banner_gradient, |ui| {
                                        changed |= ui
                                            .color_edit_button_srgb(
                                                &mut cfg.sstv_style.banner_fill2,
                                            )
                                            .changed();
                                    });
                                });
                                ui.end_row();

                                ui.label("Text outline");
                                ui.horizontal(|ui| {
                                    changed |= crate::chrome::checkbox(
                                        ui,
                                        &mut cfg.sstv_style.banner_outline,
                                        "in",
                                    )
                                    .on_hover_text(
                                        "Draw an outline around the banner text, so it stays \
                                         readable over a busy or light strip.",
                                    )
                                    .changed();
                                    ui.add_enabled_ui(cfg.sstv_style.banner_outline, |ui| {
                                        changed |= ui
                                            .color_edit_button_srgb(
                                                &mut cfg.sstv_style.banner_outline_ink,
                                            )
                                            .changed();
                                    });
                                });
                                ui.end_row();

                                ui.label("Text gradient");
                                ui.horizontal(|ui| {
                                    changed |= crate::chrome::checkbox(
                                        ui,
                                        &mut cfg.sstv_style.banner_ink_gradient,
                                        "fade to",
                                    )
                                    .on_hover_text(
                                        "Fade the banner text from its colour to a second one \
                                         across the width of the picture.",
                                    )
                                    .changed();
                                    ui.add_enabled_ui(
                                        cfg.sstv_style.banner_ink_gradient
                                            && !cfg.sstv_style.rainbow_text,
                                        |ui| {
                                            changed |= ui
                                                .color_edit_button_srgb(
                                                    &mut cfg.sstv_style.banner_ink2,
                                                )
                                                .changed();
                                        },
                                    );
                                });
                                ui.end_row();

                                ui.label("Rainbow text");
                                changed |= crate::chrome::checkbox(
                                    ui,
                                    &mut cfg.sstv_style.rainbow_text,
                                    "overriding the colours above",
                                )
                                .on_hover_text(
                                    "Draw the banner's text as a horizontal rainbow, ignoring the \
                                     colours above. Each text layer has a rainbow of its own.",
                                )
                                .changed();
                                ui.end_row();

                                ui.label("Height").on_hover_text(
                                    "How tall the strip is, in pixels of the transmitted \
                                     picture. The text is sized from it — an SSTV frame is only \
                                     320 pixels wide, so a taller banner is what makes it \
                                     readable on the far end.",
                                );
                                ui.spacing_mut().slider_width = 180.0;
                                let resp = crate::chrome::slider(
                                    ui,
                                    egui::Slider::new(&mut cfg.sstv_banner_height, 8..=64)
                                        .suffix(" px"),
                                );
                                changed |=
                                    resp.drag_stopped() || (resp.changed() && !resp.dragged());
                                ui.end_row();
                            });
                    });
                    ui.add_space(6.0);
                    if ui
                        .button("Reset")
                        .on_hover_text(
                            "Back to the callsign on the left and the version on the right",
                        )
                        .clicked()
                    {
                        let d = sdroxide_types::DigiConfig::default();
                        cfg.sstv_banner = d.sstv_banner;
                        cfg.sstv_banner_left = d.sstv_banner_left;
                        cfg.sstv_banner_right = d.sstv_banner_right;
                        cfg.sstv_banner_fill = d.sstv_banner_fill;
                        cfg.sstv_banner_ink = d.sstv_banner_ink;
                        cfg.sstv_banner_height = d.sstv_banner_height;
                        cfg.sstv_style = d.sstv_style;
                        changed = true;
                    }
                    if !seeded {
                        ui.label(
                            RichText::new(
                                "Waiting for the station's digital-mode settings to load…",
                            )
                            .size(10.0)
                            .weak(),
                        );
                    }
                });
            });
        if changed {
            cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
        }
        self.sstv.banner_open = open;
    }

    /// The RIFP half of the image panel's control strip: profile, picture size
    /// and encoding, robustness, the transfer readout, and the sessions being
    /// reassembled.
    fn rifp_controls(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let st = self.sstv.rifp.clone();
        let seeded = self.digi_cfg_seeded;
        let mut changed = false;

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("RIFP").size(12.0).strong().color(crate::theme::CYAN()));
            // Outside the enabled scope: which frequency to sit on has nothing
            // to do with whether the operator's digi config has loaded yet.
            self.digi_freq_chip(ui, cmds);
            ui.add_enabled_ui(seeded, |ui| {
                for p in RifpProfile::ALL {
                    let active = self.digi_cfg_edit.rifp_profile == p;
                    if crate::chrome::chip(ui, active, p.label())
                        .on_hover_text(format!(
                            "{} — {:.0} baud CPFSK, ±{:.0} Hz, {:.0} kHz occupied bandwidth",
                            p.name(),
                            p.symbol_rate(),
                            p.deviation_hz(),
                            p.bandwidth_hz() / 1000.0,
                        ))
                        .clicked()
                        && !active
                    {
                        self.digi_cfg_edit.rifp_profile = p;
                        changed = true;
                    }
                }
                ui.separator();
                ui.label(RichText::new("Size").size(10.0).weak());
                for s in RifpSize::ALL {
                    let active = self.digi_cfg_edit.rifp_size == s;
                    if crate::chrome::chip(ui, active, s.label()).clicked() && !active {
                        self.digi_cfg_edit.rifp_size = s;
                        self.sstv.preview_dirty = true;
                        changed = true;
                    }
                }
            });
        });
        ui.add_space(4.0);

        // The bandwidth warning, and a jump to the calling frequency. RIFP
        // itself is band-agnostic; what is legal is not.
        let dial = self.state.rx_freq_hz();
        ui.horizontal_wrapped(|ui| {
            let profile = self.digi_cfg_edit.rifp_profile;
            if profile.fits_at(dial) {
                ui.label(
                    RichText::new(format!(
                        "{} · ~{:.0} kHz occupied · dial is the channel centre",
                        profile.name(),
                        profile.bandwidth_hz() / 1000.0,
                    ))
                    .size(10.5)
                    .weak(),
                );
            } else {
                ui.label(
                    RichText::new(format!(
                        "⚠ {} occupies ~{:.0} kHz — too wide for a narrow-band segment",
                        profile.name(),
                        profile.bandwidth_hz() / 1000.0,
                    ))
                    .size(10.5)
                    .strong()
                    .color(crate::theme::ALERT()),
                )
                .on_hover_text(format!(
                    "RIFP assigns no frequency, and sdroxide will transmit it wherever you tune. \
                     A {:.0} kHz channel only fits where wideband or FM operation is allowed — \
                     {} — and not in a narrow-band segment, least of all on HF. Even inside those \
                     your own licence conditions may be narrower. You are the operator; check \
                     your own rules.",
                    profile.bandwidth_hz() / 1000.0,
                    profile.wide_segments_text(),
                ));
            }
            if (dial - sdroxide_types::RIFP_CALLING_HZ).abs() > 1.0
                && crate::chrome::chip(ui, false, "433.920")
                    .on_hover_text("The calling frequency the draft names")
                    .clicked()
            {
                cmds.push(Command::SetVfo {
                    vfo: self.state.active_vfo,
                    hz: sdroxide_types::RIFP_CALLING_HZ,
                });
            }
        });
        ui.add_space(5.0);

        // Encoding and depth: what the picture is turned into before framing.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(seeded, |ui| {
                ui.label(RichText::new("Encode").size(10.0).weak()).on_hover_text(
                    "How the picture is encoded into the object RIFP carries. Auto tries each and \
                 sends the smallest.",
                );
                for e in RifpEncoding::TX_MENU {
                    let active = self.digi_cfg_edit.rifp_encoding == e;
                    let hover = match e.manifest_pair() {
                        Some((mt, ce)) => format!("{mt} / {ce}"),
                        None => "Try every encoding, send the smallest (never lossy)".into(),
                    };
                    if crate::chrome::chip(ui, active, e.label()).on_hover_text(hover).clicked()
                        && !active
                    {
                        self.digi_cfg_edit.rifp_encoding = e;
                        changed = true;
                    }
                }
                ui.separator();
                ui.label(RichText::new("Gray").size(10.0).weak()).on_hover_text(
                "Grayscale depth. RIFP's raster is grayscale by definition — colour has no place \
                 in its manifest.",
            );
                for bits in [1u8, 2, 4, 8] {
                    let active = self.digi_cfg_edit.rifp_bits_per_pixel == bits;
                    if crate::chrome::chip(ui, active, &format!("{bits}b")).clicked() && !active {
                        self.digi_cfg_edit.rifp_bits_per_pixel = bits;
                        changed = true;
                    }
                }
                let mut dither = self.digi_cfg_edit.rifp_dither;
                if crate::chrome::chip(ui, dither, "Dither")
                    .on_hover_text("Diffuse quantisation error — worth it below 8 bits")
                    .clicked()
                {
                    dither = !dither;
                    self.digi_cfg_edit.rifp_dither = dither;
                    changed = true;
                }
            });
        });
        ui.add_space(5.0);

        // The content hint: a caption carried in the manifest itself, so it
        // reaches a receiver as text rather than as pixels they have to read.
        // Distinct from the slot message, which is drawn into the picture.
        ui.horizontal(|ui| {
            ui.add_enabled_ui(seeded, |ui| {
                ui.label(RichText::new("Caption").size(10.0).weak()).on_hover_text(
                    "Sent in the manifest as the content hint, and shown under the picture by \
                     receivers. Travels as text — it is not drawn into the image.",
                );
                let resp = crate::chrome::field(
                    ui,
                    egui::TextEdit::singleline(&mut self.digi_cfg_edit.rifp_content_hint)
                        .desired_width(f32::INFINITY)
                        .hint_text("What this picture is"),
                );
                // On focus loss rather than per keystroke: this is persisted
                // engine-side, and a config write per character is absurd.
                if resp.lost_focus() && resp.changed() {
                    changed = true;
                }
            });
        });
        ui.add_space(5.0);

        // Robustness: RIFP has no repair requests, so repetition is the only
        // recovery there is.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(seeded, |ui| {
                ui.label(RichText::new("Repeat data").size(10.0).weak()).on_hover_text(
                    "Send every data frame this many times. RIFP is one-way with no repair \
                     requests, so this is the only recovery a receiver gets.",
                );
                ui.spacing_mut().slider_width = 90.0;
                changed |= crate::chrome::slider(
                    ui,
                    egui::Slider::new(&mut self.digi_cfg_edit.rifp_data_repeats, 1..=4),
                )
                .drag_stopped();
                ui.label(RichText::new("Chunk").size(10.0).weak())
                    .on_hover_text("Payload octets per data frame (the profile recommends 192)");
                changed |= crate::chrome::slider(
                    ui,
                    egui::Slider::new(&mut self.digi_cfg_edit.rifp_chunk_size, 32..=1024)
                        .step_by(16.0),
                )
                .drag_stopped();
            });
            ui.separator();
            if st.tx_active {
                ui.label(
                    RichText::new(format!(
                        "● TX frame {}/{} · {} s left",
                        st.tx_frame, st.tx_frames, st.tx_remaining_s
                    ))
                    .size(11.0)
                    .strong()
                    .color(crate::theme::ALERT()),
                );
            }
            if let Some(enc) = st.tx_encoding {
                ui.label(
                    RichText::new(format!("sent as {} · {} octets", enc.label(), st.tx_bytes))
                        .size(10.0)
                        .weak(),
                );
            }
        });
        ui.add_space(5.0);

        // Counters and the sessions being reassembled.
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "frames {} · bad {} · pictures {}",
                    st.rx_frames, st.rx_bad_frames, st.rx_objects
                ))
                .size(10.0)
                .weak(),
            )
            .on_hover_text("Valid frames, frames that failed CRC, and complete verified pictures");
            if st.sessions.is_empty() {
                ui.label(RichText::new("no transfer in progress").size(10.0).weak());
            }
            for s in &st.sessions {
                ui.separator();
                let from = s.sender.as_deref().unwrap_or_else(|| shorten(&s.session, 8));
                let label = if s.total > 0 {
                    format!("{from} {}/{}", s.have, s.total)
                } else {
                    format!("{from} {}", s.have)
                };
                let colour =
                    if s.have_manifest { crate::theme::GREEN() } else { crate::theme::YELLOW() };
                ui.label(RichText::new(label).size(10.5).strong().color(colour)).on_hover_text(
                    if s.have_manifest {
                        format!("session {} · idle {} s", s.session, s.idle_s)
                    } else {
                        format!(
                            "session {} · chunks held, still waiting for the manifest · idle {} s",
                            s.session, s.idle_s
                        )
                    },
                );
                rifp_chunk_map(ui, s);
                if crate::chrome::chip(ui, false, "✕")
                    .on_hover_text("Forget this incomplete transfer")
                    .clicked()
                {
                    cmds.push(Command::RifpDropSession(s.session.clone()));
                }
            }
        });
        if let Some(err) = &st.last_error {
            ui.label(RichText::new(err).size(10.0).color(crate::theme::YELLOW()));
        }
        if changed && seeded {
            cmds.push(Command::SetDigiConfig(self.digi_cfg_edit.clone()));
        }
    }

    /// How to name a directory the engine reported.
    ///
    /// A path is only a path if the operator can walk to it. Driving a radio on
    /// another machine, it names a directory over there, and saying so is the
    /// difference between a useful hint and a wrong one.
    pub(in crate::app) fn store_where(&self, dir: &str) -> String {
        if self.ctrl.engine_is_remote() {
            format!("on the radio, in {dir}")
        } else {
            format!("in {dir}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, Rect, vec2};

    const DIMS: (u16, u16) = (320, 256);

    /// The transmit editor driven headlessly: the real preview and layer
    /// editor in an egui frame, synthetic pointer input, and every layer
    /// command echoed back the way the engine does.
    struct Rig {
        ctx: egui::Context,
        ui: SstvUi,
        t: f64,
        sent: Vec<Command>,
        pointer: Pos2,
        /// Echoes on their way back, and the frame each lands on.
        echoes: Vec<(usize, ImagePresets)>,
        frames: usize,
    }

    impl Rig {
        fn new(layers: Vec<TextLayer>) -> Rig {
            let mut ui = SstvUi::default();
            let s = &mut ui.presets.slots[0];
            (s.layers, s.width, s.height, s.version) = (layers, 64, 64, 1);
            ui.slots[0] =
                Some(SstvSlot { src_rgb: vec![90; 64 * 64 * 3], sw: 64, sh: 64, version: 1 });
            let mut rig = Rig {
                ctx: egui::Context::default(),
                ui,
                t: 0.0,
                sent: Vec::new(),
                pointer: Pos2::ZERO,
                echoes: Vec::new(),
                frames: 0,
            };
            rig.idle(3);
            rig
        }

        fn frame(&mut self, events: Vec<Event>) {
            self.t += 1.0 / 30.0;
            let input = egui::RawInput {
                time: Some(self.t),
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700.0, 900.0))),
                ..Default::default()
            };
            let mut cmds = Vec::new();
            let (ui, ctx) = (&mut self.ui, &self.ctx);
            // Laid out as the panel lays it out: a scrolled column, the
            // preview framed and centred in a box of fixed height.
            let mut out = ctx.run_ui(input, |p| {
                let now = p.ctx().input(|i| i.time);
                ui.sync_edit(now, &mut cmds);
                ui.ensure_preview(DIMS, &p.ctx().clone());
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(p, |p| {
                    let (w, h) = (640.0, 300.0);
                    egui::Frame::new().inner_margin(2.0).show(p, |p| {
                        p.set_min_size(vec2(w, h));
                        p.set_max_size(vec2(w, h));
                        p.centered_and_justified(|p| {
                            ui.preview_ui(p, vec2(w - 4.0, h - 4.0), DIMS, now, &mut cmds);
                        });
                    });
                    ui.layer_editor(p, 640.0, 50.0, DIMS, now, &mut cmds);
                });
            });
            // No renderer to hand the textures to.
            out.textures_delta.clear();
            // The engine answers a few frames later, as it does over the
            // channel.
            self.frames += 1;
            for c in &cmds {
                if let Command::ImageSetQso(q) = c {
                    let mut p = self.echoes.last().map_or(self.ui.presets.clone(), |e| e.1.clone());
                    p.qso = q.clone();
                    self.echoes.push((self.frames + 4, p));
                }
                if let Command::ImageSetLayers { slot, layers } = c {
                    let mut p = self.echoes.last().map_or(self.ui.presets.clone(), |e| e.1.clone());
                    p.slots[*slot as usize].layers = layers.clone();
                    self.echoes.push((self.frames + 4, p));
                }
            }
            while self.echoes.first().is_some_and(|e| e.0 <= self.frames) {
                let (_, p) = self.echoes.remove(0);
                self.ui.on_presets(p, &self.ctx);
            }
            self.sent.extend(cmds);
        }

        fn idle(&mut self, n: usize) {
            for _ in 0..n {
                self.frame(Vec::new());
            }
        }

        /// The centre of a layer's box, on screen.
        fn centre_of(&self, i: usize) -> Pos2 {
            let b = self.ui.boxes[i].expect("layer has a box");
            let r = self.ui.preview_rect;
            let k = r.width() / f32::from(DIMS.0);
            r.min + vec2((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0) * k
        }

        fn button(&mut self, pressed: bool) {
            let pos = self.pointer;
            self.frame(vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            }]);
        }

        fn move_to(&mut self, p: Pos2) {
            self.pointer = p;
            self.frame(vec![Event::PointerMoved(p)]);
        }

        fn drag(&mut self, from: Pos2, by: egui::Vec2) {
            self.move_to(from);
            self.button(true);
            for i in 1..=10 {
                self.move_to(from + by * (i as f32 / 10.0));
            }
            self.button(false);
            self.idle(2);
        }

        fn click(&mut self, at: Pos2) {
            self.move_to(at);
            self.button(true);
            self.button(false);
            self.idle(2);
        }

        /// A corner of a layer's outline on screen, where its handle is.
        fn corner_of(&self, i: usize, c: (f32, f32)) -> Pos2 {
            let b = self.ui.boxes[i].expect("layer has a box");
            let r = self.ui.preview_rect;
            let k = r.width() / f32::from(DIMS.0);
            let o = Rect::from_min_max(r.min + vec2(b.x0, b.y0) * k, r.min + vec2(b.x1, b.y1) * k)
                .expand(2.0);
            o.min + o.size() * vec2(c.0, c.1)
        }

        fn key(&mut self, key: egui::Key, shift: bool) {
            let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
            let ev =
                |pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            self.frame(vec![ev(true)]);
            self.frame(vec![ev(false)]);
        }

        /// What "+ Add" does.
        fn add(&mut self) {
            let l = self.ui.new_centre_layer(DIMS);
            let now = self.t;
            let mut cmds = Vec::new();
            let ls = self.ui.layers_mut(now, &mut cmds);
            ls.push(l);
            self.ui.sel_layer = ls.len() - 1;
            self.ui.focus_text = true;
            self.sent.extend(cmds);
            self.idle(3);
        }
    }

    fn first() -> Vec<TextLayer> {
        vec![TextLayer { text: "CQ SSTV".into(), size: 15.0, ..TextLayer::default() }]
    }

    /// Where a station that upgraded starts: its old message, converted.
    fn converted() -> Vec<TextLayer> {
        TextLayer::from_message("CQ SSTV\nde OE1TEST\nMartin 1", [255; 3], Some([0; 3]), false)
    }

    /// Every layer in turn: picked by a click, then dragged.
    fn pick_and_drag_all(rig: &mut Rig) {
        for i in 0..rig.ui.boxes.len() {
            rig.click(rig.centre_of(i));
            assert_eq!(rig.ui.sel_layer, i, "clicking layer {i} picks it");
            let before = rig.ui.boxes[i].unwrap();
            rig.drag(rig.centre_of(i), vec2(20.0, 20.0));
            let after = rig.ui.boxes[i].unwrap();
            assert!(after.x0 > before.x0 + 5.0, "layer {i}: {before:?} -> {after:?}");
            rig.idle(30);
        }
    }

    #[test]
    fn every_layer_of_a_converted_slot_can_be_picked_and_dragged_after_an_add() {
        let mut rig = Rig::new(converted());
        rig.add();
        rig.idle(20);
        assert_eq!(rig.ui.boxes.len(), 3);
        pick_and_drag_all(&mut rig);
    }

    #[test]
    fn two_adds_in_a_row_leave_every_layer_draggable() {
        let mut rig = Rig::new(first());
        rig.add();
        rig.add();
        rig.idle(20);
        assert_eq!(rig.ui.boxes.len(), 3);
        pick_and_drag_all(&mut rig);
    }

    /// Pulling the bottom-right handle out grows the selected layer about its
    /// top-left corner.
    #[test]
    fn a_corner_handle_resizes_the_selected_layer() {
        let mut rig = Rig::new(first());
        let size0 = rig.ui.current_layers()[0].size;
        let b0 = rig.ui.boxes[0].unwrap();
        rig.drag(rig.corner_of(0, (1.0, 1.0)), vec2(60.0, 30.0));
        let l = &rig.ui.current_layers()[0];
        assert!(l.size > size0 * 1.2, "{size0} -> {}", l.size);
        let b = rig.ui.boxes[0].unwrap();
        assert!((b.x0 - b0.x0).abs() < 1.0 && (b.y0 - b0.y0).abs() < 1.0, "{b0:?} -> {b:?}");
        assert!(b.x1 > b0.x1 + 10.0);
        // And the top-left one pushed in shrinks it about its bottom-right.
        let (size1, b1) = (l.size, b);
        rig.drag(rig.corner_of(0, (0.0, 0.0)), vec2(40.0, 20.0));
        let b = rig.ui.boxes[0].unwrap();
        assert!(rig.ui.current_layers()[0].size < size1, "it shrank");
        assert!((b.x1 - b1.x1).abs() < 1.0 && (b.y1 - b1.y1).abs() < 1.0, "{b1:?} -> {b:?}");
    }

    /// With the pointer over the picture, the arrows nudge the selected layer
    /// a pixel at a time, ten with Shift.
    #[test]
    fn arrow_keys_nudge_the_selected_layer() {
        let mut rig = Rig::new(first());
        let b0 = rig.ui.boxes[0].unwrap();
        rig.move_to(rig.centre_of(0));
        for _ in 0..3 {
            rig.key(egui::Key::ArrowRight, false);
        }
        rig.key(egui::Key::ArrowDown, true);
        rig.idle(2);
        let b = rig.ui.boxes[0].unwrap();
        assert!((b.x0 - b0.x0 - 3.0).abs() < 0.01, "{b0:?} -> {b:?}");
        assert!((b.y0 - b0.y0 - 10.0).abs() < 0.01, "{b0:?} -> {b:?}");
    }

    /// While the text box has the cursor, the arrows are the text's.
    #[test]
    fn arrow_keys_leave_the_layer_alone_while_typing() {
        let mut rig = Rig::new(first());
        rig.ui.focus_text = true;
        rig.idle(2);
        let b0 = rig.ui.boxes[0].unwrap();
        rig.move_to(rig.centre_of(0));
        rig.key(egui::Key::ArrowRight, false);
        rig.idle(2);
        assert_eq!(rig.ui.boxes[0].unwrap(), b0);
        // A click on the picture takes the keyboard back.
        rig.click(rig.centre_of(0));
        rig.key(egui::Key::ArrowRight, false);
        rig.idle(2);
        assert!((rig.ui.boxes[0].unwrap().x0 - b0.x0 - 1.0).abs() < 0.01);
    }

    /// The log entry carries the contact as typed, tidied: the call in
    /// capitals, the reports as the log parses them.
    #[test]
    fn a_contact_fills_a_log_entry() {
        let q = sdroxide_types::SstvQso {
            call: " ea3xyz".into(),
            rsv_rcvd: "575".into(),
            name: "Ana".into(),
            note: "nice pic".into(),
            ..Default::default()
        };
        let f = sstv_log_form(&q, 14_230_000.0, "Scottie 1", 0);
        assert_eq!(f.call, "EA3XYZ");
        assert_eq!((f.rst_sent.as_str(), f.rst_rcvd.as_str()), ("595", "575"));
        assert_eq!(f.mode, "SSTV");
        assert_eq!(f.name, "Ana");
        assert_eq!(f.comment, "Scottie 1 · nice pic");
        assert_eq!(f.freq_mhz, "14.2300");
    }

    /// A slot whose text asks for the other station's call, with no call in
    /// the QSO box, is flagged; filling the box clears the flag and the call
    /// is drawn.
    #[test]
    fn an_empty_tocall_is_flagged_until_the_box_is_filled() {
        let l = TextLayer { text: "{call} de {tocall} {rsv}".into(), ..TextLayer::default() };
        let mut rig = Rig::new(vec![l]);
        rig.ui.fields =
            crate::sstv::Fields::new("ea3abc", "", "Martin 1", 0).with_qso(rig.ui.qso());
        assert_eq!(rig.ui.empty_placeholders(), vec!["{tocall}"]);
        let w0 = rig.ui.boxes[0].map(|b| b.x1 - b.x0).unwrap();
        *crate::sstv::held_edit(&mut rig.ui.qso_edit, &rig.ui.presets.qso, rig.t) =
            sdroxide_types::SstvQso { call: "ea3xyz".into(), ..Default::default() };
        rig.ui.fields = rig.ui.fields.clone().with_qso(rig.ui.qso());
        rig.ui.preview_dirty = true;
        rig.idle(30);
        assert!(rig.ui.empty_placeholders().is_empty());
        let w1 = rig.ui.boxes[0].map(|b| b.x1 - b.x0).unwrap();
        assert!(w1 > w0, "the call now takes room: {w0} -> {w1}");
        // And the contact went to the engine.
        assert!(
            rig.sent.iter().any(|c| matches!(c, Command::ImageSetQso(q) if q.call == "ea3xyz"))
        );
    }

    fn lookup(call: &str, name: &str, qth: &str) -> sdroxide_types::CallsignInfo {
        sdroxide_types::CallsignInfo {
            call: call.into(),
            name: Some(name.into()),
            qth: Some(qth.into()),
            ..Default::default()
        }
    }

    /// A lookup fills the blanks of the contact it is about, and leaves the
    /// contact untouched — still the program's, still replaceable by an ID.
    #[test]
    fn a_lookup_fills_the_blanks_of_its_own_contact_only() {
        let mut rig = Rig::new(first());
        rig.ui.presets.qso = sdroxide_types::SstvQso {
            call: "EA3XYZ".into(),
            qth: "Barcelona".into(),
            ..Default::default()
        };
        rig.ui.apply_lookup(&lookup("ea1abc", "Bob", "Madrid"));
        assert!(rig.ui.qso().name.is_empty(), "about another call");
        rig.ui.apply_lookup(&lookup("ea3xyz", "Ana", "Girona"));
        assert_eq!(rig.ui.qso().name, "Ana");
        assert_eq!(rig.ui.qso().qth, "Barcelona", "not a blank");
        assert!(!rig.ui.qso().touched);
        rig.idle(30);
        assert!(rig.sent.iter().any(|c| matches!(c, Command::ImageSetQso(q) if q.name == "Ana")));
    }

    /// Saving the log entry of the contact in progress finishes it.
    #[test]
    fn logging_the_contact_marks_it_logged() {
        let mut rig = Rig::new(first());
        rig.ui.presets.qso =
            sdroxide_types::SstvQso { call: "EA3XYZ".into(), touched: true, ..Default::default() };
        rig.ui.mark_logged("EA1ABC");
        assert!(!rig.ui.qso().logged, "another call");
        rig.ui.mark_logged("ea3xyz");
        assert!(rig.ui.qso().logged);
        assert!(!rig.ui.qso().locked(), "the next ID may replace it");
    }

    /// A picture announced again with its sender updates the entry in place,
    /// and a sender once known survives the announcement without one landing
    /// after it.
    #[test]
    fn a_pictures_sender_arrives_after_the_picture() {
        let rig = Rig::new(first());
        let mut ui = rig.ui;
        let ctx = rig.ctx;
        let thumb = crate::sstv::encode_png(&[9; 12], 2, 2).unwrap();
        let entry = |sender: Option<&str>| ImageEntry {
            kind: ImageKind::Sstv,
            name: "sstv-1.png".into(),
            unix: 1,
            thumb: thumb.clone(),
            sender: sender.map(Into::into),
            ..Default::default()
        };
        ui.on_saved(entry(Some("EA3XYZ")), &ctx);
        ui.on_saved(entry(None), &ctx);
        assert_eq!(ui.received.len(), 1);
        assert_eq!(ui.total, 1, "counted once");
        assert_eq!(ui.received[0].entry.sender.as_deref(), Some("EA3XYZ"));
    }

    /// Two layers stacked exactly on one another: the one picked with its
    /// chip is the one a drag moves, and clicking there steps between them.
    #[test]
    fn a_layer_under_another_can_still_be_reached() {
        let l = TextLayer { text: "SAME".into(), size: 12.0, ..TextLayer::default() };
        let mut rig = Rig::new(vec![l.clone(), l]);
        let at = rig.centre_of(0);
        rig.click(at);
        assert_eq!(rig.ui.sel_layer, 1, "the top one first");
        rig.click(at);
        assert_eq!(rig.ui.sel_layer, 0, "then the one beneath");
        rig.click(at);
        assert_eq!(rig.ui.sel_layer, 1, "and round again");
        // Picked by its chip, the lower one is what a drag takes.
        rig.ui.sel_layer = 0;
        rig.idle(2);
        let (b0, b1) = (rig.ui.boxes[0].unwrap(), rig.ui.boxes[1].unwrap());
        rig.drag(at, vec2(0.0, 80.0));
        assert!(rig.ui.boxes[0].unwrap().y0 > b0.y0 + 20.0, "the selected layer moved");
        assert_eq!(rig.ui.boxes[1].unwrap(), b1, "the one on top stayed");
    }

    /// "+ Add" twice does not stack the second new layer on the first.
    #[test]
    fn added_layers_do_not_land_on_each_other() {
        let mut rig = Rig::new(first());
        rig.add();
        rig.add();
        rig.idle(5);
        let (a, b) = (rig.ui.boxes[1].unwrap(), rig.ui.boxes[2].unwrap());
        assert!(b.y0 >= a.y1 - 0.5 || b.y1 <= a.y0 + 0.5, "{a:?} overlaps {b:?}");
    }

    /// The preview box is wider than a 5:4 picture, so the picture is drawn
    /// centred in it. Layer boxes have to be mapped onto the picture as drawn
    /// — mapped onto the whole box, they were stretched and shifted, harmless
    /// near the top-left corner and wildly off for a layer in the middle.
    #[test]
    fn layer_boxes_are_mapped_onto_the_picture_as_drawn() {
        let rig = Rig::new(first());
        let r = rig.ui.preview_rect;
        let aspect = f32::from(DIMS.0) / f32::from(DIMS.1);
        assert!((r.width() / r.height() - aspect).abs() < 0.01, "{r:?}");
        // Centred in the 640-wide box the rig lays it out in.
        assert!(r.min.x > 100.0, "{r:?}");
    }

    #[test]
    fn the_first_layer_can_be_dragged() {
        let mut rig = Rig::new(first());
        let before = rig.ui.boxes[0].unwrap();
        rig.drag(rig.centre_of(0), vec2(0.0, 150.0));
        let after = rig.ui.boxes[0].unwrap();
        assert!(after.y0 > before.y0 + 30.0, "{before:?} -> {after:?}");
    }

    #[test]
    fn an_added_layer_can_be_dragged() {
        let mut rig = Rig::new(first());
        rig.add();
        assert_eq!(rig.ui.boxes.len(), 2, "the new layer is composed");
        let before = rig.ui.boxes[1].unwrap();
        rig.drag(rig.centre_of(1), vec2(0.0, 150.0));
        let after = rig.ui.boxes[1].unwrap();
        assert!(after.y0 > before.y0 + 30.0, "{before:?} -> {after:?}");
    }

    #[test]
    fn an_added_layer_can_be_picked_after_the_first() {
        let mut rig = Rig::new(first());
        rig.add();
        rig.idle(40);
        rig.click(rig.centre_of(0));
        assert_eq!(rig.ui.sel_layer, 0);
        rig.click(rig.centre_of(1));
        assert_eq!(rig.ui.sel_layer, 1);
        let before = rig.ui.boxes[1].unwrap();
        rig.drag(rig.centre_of(1), vec2(0.0, 150.0));
        let after = rig.ui.boxes[1].unwrap();
        assert!(after.y0 > before.y0 + 30.0, "{before:?} -> {after:?}");
    }
}

//! SSTV image compositing for transmit: crop/scale a source picture to the
//! selected mode's dimensions, stamp the operator's banner strip across the
//! top of it, and draw the slot's text layers over what is left (one bundled
//! font; each layer pinned to a point of the picture, sized as a fraction of
//! it, in its own colours).
//!
//! The banner is what the station puts its name on, so its two texts, its two
//! colours and its height come from [`sdroxide_types::DigiConfig`] rather than
//! from here — see [`Banner`]. The stock settings compose exactly the strip
//! this module used to hard-wire: the callsign at the left, `SDRoxide vX.Y.Z`
//! at the right, white on red fading to black.
//!
//! Pure-Rust (`image` + `ab_glyph`) so it runs identically in the native app and
//! the wasm browser client — the composed buffer is both the live preview and,
//! PNG-encoded, the transmit payload.

use std::collections::HashMap;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use eframe::egui;
use sdroxide_types::{TextAnchor, TextLayer};

/// The banner strip drawn across the top of a transmitted picture, with the
/// operator's templates already resolved against the station identity.
///
/// Resolved rather than carried as templates because the same banner is drawn
/// several times for one picture — the live preview on every keystroke, then
/// the transmit copy — and because it keeps the drawing code free of any
/// notion of what a `{call}` is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Banner {
    /// Height of the strip in picture pixels. The text is sized from it.
    pub height: u16,
    /// Text printed at the left end.
    pub left: String,
    /// Text printed at the right end, right-aligned.
    pub right: String,
    /// Colour at the top of the strip. It fades to [`Self::fill2`], or to black
    /// when that is `None`.
    pub fill: [u8; 3],
    /// The colour the strip fades to at its bottom, when the operator asked for
    /// a gradient. `None` fades to black, which is the original look.
    pub fill2: Option<[u8; 3]>,
    /// Colour both texts are printed in.
    pub ink: [u8; 3],
    /// The colour the text fades to across the strip, when the operator asked
    /// for a text gradient. `None` prints it in one colour.
    pub ink2: Option<[u8; 3]>,
    /// Override both texts with a horizontal rainbow, whatever the colours
    /// above say.
    pub rainbow: bool,
    /// Colour the texts are outlined in, when the operator asked for an
    /// outline. `None` prints them plain.
    pub outline: Option<[u8; 3]>,
}

impl Banner {
    /// The banner the station's digital-mode config asks for, or `None` when
    /// the operator has switched it off.
    ///
    /// A banner whose two texts both resolve to nothing still draws its strip:
    /// blanking the text is not the same request as turning the banner off,
    /// and a strip that vanished when a callsign had not been entered yet
    /// would look like a bug rather than like an empty field.
    pub fn from_config(cfg: &sdroxide_types::DigiConfig, fields: &Fields) -> Option<Banner> {
        cfg.sstv_banner.then(|| Banner {
            // A zero height would be an invisible banner that still pushed the
            // message down by nothing; one pixel is the smallest honest strip.
            height: cfg.sstv_banner_height.max(1),
            left: expand(&cfg.sstv_banner_left, fields),
            right: expand(&cfg.sstv_banner_right, fields),
            fill: cfg.sstv_banner_fill,
            fill2: cfg.sstv_style.banner_gradient.then_some(cfg.sstv_style.banner_fill2),
            ink: cfg.sstv_banner_ink,
            ink2: cfg.sstv_style.banner_ink_gradient.then_some(cfg.sstv_style.banner_ink2),
            rainbow: cfg.sstv_style.rainbow_text,
            outline: cfg.sstv_style.banner_outline.then_some(cfg.sstv_style.banner_outline_ink),
        })
    }
}

/// What the `{…}` placeholders in the banner and the text layers stand for,
/// resolved once per frame so every text in the picture agrees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields {
    pub call: String,
    pub grid: String,
    /// The transmit mode's name, `Martin 1` or `RIFP`.
    pub mode: String,
    /// UTC, `2026-09-27`.
    pub date: String,
    /// UTC, `14:05`. Minutes, not seconds: the picture is composed when TX is
    /// pressed, and a preview redrawn every second for a clock nobody reads to
    /// the second would be work for nothing.
    pub time: String,
    /// The contact in progress: the other station's call, in capitals…
    pub tocall: String,
    /// …the report sent to them and the one they sent…
    pub rsv: String,
    pub rsvr: String,
    /// …their name and QTH, and a free note.
    pub name: String,
    pub qth: String,
    pub note: String,
}

impl Fields {
    pub fn new(call: &str, grid: &str, mode: &str, unix: i64) -> Fields {
        let (y, mo, d, h, mi, _) = sdroxide_types::utc_ymd_hms(unix);
        Fields {
            call: call.trim().to_uppercase(),
            grid: grid.trim().to_string(),
            mode: mode.to_string(),
            date: format!("{y:04}-{mo:02}-{d:02}"),
            time: format!("{h:02}:{mi:02}"),
            ..Fields::default()
        }
    }

    /// These fields, with the contact in progress filled in.
    pub fn with_qso(self, q: &sdroxide_types::SstvQso) -> Fields {
        Fields {
            tocall: q.call.trim().to_uppercase(),
            rsv: q.rsv_sent.trim().to_string(),
            rsvr: q.rsv_rcvd.trim().to_string(),
            name: q.name.trim().to_string(),
            qth: q.qth.trim().to_string(),
            note: q.note.trim().to_string(),
            ..self
        }
    }

    /// What the placeholder `name` stands for, and the name it goes by in
    /// [`PLACEHOLDERS`] — `None` for a name that is not a placeholder.
    ///
    /// Each has one documented name and quietly answers to a few more, so an
    /// operator arriving from another program, or from the FT8 templates, is
    /// not tripped up by spelling: `{dx}` and `{yourcall}` are `{tocall}`,
    /// `{report}` and `{rsvs}` are `{rsv}`, `{mycall}` is `{call}`.
    ///
    /// Callsigns come out trimmed and in capitals, whatever was typed.
    fn lookup(&self, name: &str) -> Option<(&'static str, String)> {
        let callsign = |c: &str| c.trim().to_uppercase();
        let (doc, v) = match name.to_ascii_lowercase().as_str() {
            "call" | "mycall" => return Some(("{call}", callsign(&self.call))),
            "tocall" | "dx" | "yourcall" => return Some(("{tocall}", callsign(&self.tocall))),
            "grid" | "mygrid" => ("{grid}", self.grid.as_str()),
            "rsv" | "report" | "rsvs" => ("{rsv}", self.rsv.as_str()),
            "rsvr" => ("{rsvr}", self.rsvr.as_str()),
            "name" => ("{name}", self.name.as_str()),
            "qth" => ("{qth}", self.qth.as_str()),
            "note" => ("{note}", self.note.as_str()),
            "mode" => ("{mode}", self.mode.as_str()),
            "date" => ("{date}", self.date.as_str()),
            "time" => ("{time}", self.time.as_str()),
            "version" => ("{version}", env!("CARGO_PKG_VERSION")),
            _ => return None,
        };
        Some((doc, v.trim().to_string()))
    }
}

/// Walk the `{…}` runs of `template`, handing each name to `token`: what it
/// returns replaces the run, `None` leaves it as typed. An unclosed brace is
/// just text.
fn scan(template: &str, mut token: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find('}') else {
            break;
        };
        match token(&rest[1..close]) {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Substitute the placeholders in `template`.
///
/// `{call}` is uppercased — a callsign belongs in capitals on the air and the
/// header always printed it that way — but the template around it is left as
/// the operator typed it, which is what stops `SDRoxide` becoming `SDROXIDE`.
/// Placeholder names are matched without regard to case. An unrecognised
/// `{…}` is copied through untouched so a typo is visible in the preview
/// rather than silently printing nothing.
pub fn expand(template: &str, f: &Fields) -> String {
    scan(template, |name| f.lookup(name).map(|(_, v)| v))
}

/// The placeholders `template` uses that would print nothing — a `{tocall}`
/// with no call in the QSO box — by their documented names, each once. What
/// the panel warns about before a picture goes out with a hole in it.
pub fn empty_used(template: &str, f: &Fields) -> Vec<&'static str> {
    let mut out = Vec::new();
    scan(template, |name| {
        if let Some((doc, v)) = f.lookup(name) {
            if v.trim().is_empty() && !out.contains(&doc) {
                out.push(doc);
            }
        }
        None
    });
    out
}

/// The placeholders [`expand`] knows, with a word on each, for the hint text
/// beside the fields that take them.
pub const PLACEHOLDERS: [(&str, &str); 12] = [
    ("{call}", "your callsign, in capitals"),
    ("{grid}", "your locator"),
    ("{tocall}", "their callsign, from the QSO box ({dx} works too)"),
    ("{rsv}", "the report you send them ({report} works too)"),
    ("{rsvr}", "the report they sent you"),
    ("{name}", "their name"),
    ("{qth}", "their QTH"),
    ("{note}", "the QSO box's note"),
    ("{mode}", "the transmit mode"),
    ("{date}", "today's date, UTC"),
    ("{time}", "the time, UTC"),
    ("{version}", "the sdroxide version"),
];

/// The layers with their placeholders filled in and the hidden ones blanked,
/// ready to draw. Hidden layers keep their place in the list so the boxes
/// [`compose`] reports still index the same layers the editor holds.
pub fn resolve(layers: &[TextLayer], f: &Fields) -> Vec<TextLayer> {
    layers
        .iter()
        .map(|l| TextLayer {
            text: if l.visible { expand(&l.text, f) } else { String::new() },
            ..l.clone()
        })
        .collect()
}

/// The single font used for the header and the message overlay
/// (ChakraPetch-SemiBold, already bundled OFL for the UI's own text).
fn message_font() -> Option<FontRef<'static>> {
    const RAW: &[u8] = include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf");
    FontRef::try_from_slice(RAW).ok()
}

/// Decode arbitrary image file bytes (PNG/JPEG) to interleaved RGB + size.
pub fn decode_image(bytes: &[u8]) -> Option<(Vec<u8>, u16, u16)> {
    let img = image::load_from_memory(bytes).ok()?.to_rgb8();
    let (w, h) = (img.width() as u16, img.height() as u16);
    Some((img.into_raw(), w, h))
}

/// Decode an image file and downscale it so neither side exceeds `max` pixels
/// (keeping aspect ratio), returning interleaved RGB + size. Bounds the memory
/// held per transmit slot.
pub fn load_source_bounded(bytes: &[u8], max: u16) -> Option<(Vec<u8>, u16, u16)> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max as u32 || img.height() > max as u32 {
        img.resize(max as u32, max as u32, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as u16, rgb.height() as u16);
    Some((rgb.into_raw(), w, h))
}

/// Encode interleaved RGB to PNG.
pub fn encode_png(rgb: &[u8], w: u16, h: u16) -> Option<Vec<u8>> {
    let img = image::RgbImage::from_raw(w as u32, h as u32, rgb.to_vec())?;
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(buf.into_inner())
}

/// Crop-and-scale a source image to exactly `(w, h)`, filling the frame
/// (centre-crop, preserving aspect ratio).
pub fn crop_scale(src_rgb: &[u8], sw: u16, sh: u16, w: u16, h: u16) -> Vec<u8> {
    let Some(src) = image::RgbImage::from_raw(sw as u32, sh as u32, src_rgb.to_vec()) else {
        return vec![0u8; w as usize * h as usize * 3];
    };
    image::DynamicImage::ImageRgb8(src)
        .resize_to_fill(w as u32, h as u32, image::imageops::FilterType::Triangle)
        .to_rgb8()
        .into_raw()
}

/// Build the final transmit image over an already crop-scaled `base`: add the
/// banner strip, then the text layers, bottom first. `layers` are drawn as
/// they are — resolve their placeholders first with [`resolve`].
///
/// Returns the picture and, per layer, the box it landed in (picture pixels,
/// `None` for a layer with nothing to draw) — what the editor hit-tests a
/// click against and outlines on the preview.
///
/// Built over a base rather than from the source because the preview is
/// recomposed on every frame a layer is being dragged, and scaling a
/// thousand-pixel photograph down each time would make the drag stutter.
pub fn compose(
    mut img: Vec<u8>,
    w: u16,
    h: u16,
    banner: Option<&Banner>,
    layers: &[TextLayer],
) -> (Vec<u8>, Vec<Option<LayerBox>>) {
    let (wu, hu) = (w as usize, h as usize);
    if img.len() != wu * hu * 3 {
        img = vec![0; wu * hu * 3];
    }
    let strip = match banner {
        Some(b) => draw_banner(&mut img, wu, hu, b),
        None => 0,
    };
    debug_assert_eq!(strip, banner_rows(banner, h));
    let boxes = layers.iter().map(|l| draw_layer(&mut img, wu, hu, l, strip)).collect();
    (img, boxes)
}

/// Convert interleaved RGB to an egui image for a texture.
pub fn color_image(rgb: &[u8], w: u16, h: u16) -> egui::ColorImage {
    egui::ColorImage::from_rgb([w as usize, h as usize], rgb)
}

// ── Text layers: who owns them ──
//
// The presets live in the engine, so a slot's layers arrive as part of its
// status and can change while the operator is looking at them — another screen
// attached to the same radio, or the echo of this one's own write coming back
// a round trip later. Adopting every echo would snap a layer being dragged back
// to where it was a moment ago, and move the cursor in a layer being typed in;
// ignoring them all would mean an edit made elsewhere never showed up.
//
// So at most one slot is client-owned at a time: whichever the operator is
// editing. Every other slot takes the engine's word. Edits are sent once they
// pause, and the slot is handed back when the engine has echoed what was sent
// — or, if it never does (another screen got in between, or the engine clamped
// something), a few seconds later, when the engine's version is adopted. The
// same arrangement the voice keyer's slot labels use, kept here rather than in
// the panel so it can be tested without an `egui::Context`.
//
// Between handing a slot back and the engine echoing it, the layers are in
// flight: the presets still hold the old ones. That window is not hypothetical
// — moving to another slot and pressing TX straight back on this one composes
// from it (issue #752). Those writes are kept in an [`InFlight`] map until the
// engine says the same thing back (or the same few seconds pass), and
// everything shown or sent reads through it.

/// Seconds an edit waits for the operator to pause before it is sent.
const SEND_AFTER: f64 = 0.4;
/// Seconds after sending before the slot is handed back regardless.
const RELEASE_AFTER: f64 = 3.0;

/// A value the engine owns, held by this screen while the operator edits it —
/// the same arrangement as [`LayerEdit`], for something that is not per slot:
/// the SSTV contact in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Held<T> {
    pub value: T,
    /// When the latest change not yet sent was made; `None` once sent.
    pending: Option<f64>,
    /// When the value was last sent.
    sent: Option<f64>,
}

/// What to show: the value in hand, or the engine's.
pub fn held_shown<'a, T>(held: &'a Option<Held<T>>, stored: &'a T) -> &'a T {
    held.as_ref().map_or(stored, |h| &h.value)
}

/// Take the value in hand to change it, stamping the change `now`.
pub fn held_edit<'a, T: Clone>(held: &'a mut Option<Held<T>>, stored: &T, now: f64) -> &'a mut T {
    let h = held.get_or_insert_with(|| Held { value: stored.clone(), pending: None, sent: None });
    h.pending = Some(now);
    &mut h.value
}

/// Send the value once the operator pauses, and hand it back once the engine
/// agrees — see [`tick`], which this mirrors.
pub fn held_tick<T: Clone + PartialEq>(
    held: &mut Option<Held<T>>,
    stored: &T,
    now: f64,
    busy: bool,
) -> Option<T> {
    let h = held.as_mut()?;
    if let Some(t) = h.pending {
        if now - t < SEND_AFTER {
            return None;
        }
        h.pending = None;
        h.sent = Some(now);
        return (h.value != *stored).then(|| h.value.clone());
    }
    if !busy && (h.value == *stored || h.sent.is_none_or(|t| now - t > RELEASE_AFTER)) {
        *held = None;
    }
    None
}

/// Send the value in hand at once, if it has changed, keeping it in hand.
pub fn held_send_now<T: Clone + PartialEq>(
    held: &mut Option<Held<T>>,
    stored: &T,
    now: f64,
) -> Option<T> {
    let h = held.as_mut()?;
    h.pending?;
    h.pending = Some(now - SEND_AFTER);
    held_tick(held, stored, now, true)
}

/// The layers of the one slot the operator is editing.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerEdit {
    pub slot: usize,
    pub layers: Vec<TextLayer>,
    /// When the latest change not yet sent was made; `None` once sent.
    pending: Option<f64>,
    /// When the layers were last sent.
    sent: Option<f64>,
}

/// Layers handed back to the engine and not yet echoed, by slot, with when
/// they were sent. Shown, previewed and transmitted in place of the stale
/// presets meanwhile (issue #752).
pub type InFlight = HashMap<usize, (Vec<TextLayer>, f64)>;

/// The layers to show for `slot`: the ones in hand if it is this slot, else
/// the ones last written to the engine for it, else the engine's.
pub fn layers_shown<'a>(
    edit: &'a Option<LayerEdit>,
    sent: &'a InFlight,
    presets: &'a sdroxide_types::ImagePresets,
    slot: usize,
) -> &'a [TextLayer] {
    match edit {
        Some(e) if e.slot == slot => &e.layers,
        _ => match sent.get(&slot) {
            Some((layers, _)) => layers,
            None => presets.slots.get(slot).map_or(&[][..], |s| s.layers.as_slice()),
        },
    }
}

/// Take `slot` in hand to change it, returning its layers to edit. Whatever
/// other slot was in hand is committed first, and that command is pushed.
///
/// Call it only when something is about to change: the change is stamped
/// `now`, and is sent once the operator has paused for a moment after it.
pub fn edit_layers<'a>(
    edit: &'a mut Option<LayerEdit>,
    sent: &mut InFlight,
    presets: &sdroxide_types::ImagePresets,
    slot: usize,
    now: f64,
    cmds: &mut Vec<sdroxide_types::Command>,
) -> &'a mut Vec<TextLayer> {
    if !matches!(edit, Some(e) if e.slot == slot) {
        cmds.extend(commit(edit, sent, presets, now));
        *edit = Some(LayerEdit {
            slot,
            layers: layers_shown(&None, sent, presets, slot).to_vec(),
            pending: None,
            sent: None,
        });
    }
    let e = edit.as_mut().expect("claimed above");
    e.pending = Some(now);
    &mut e.layers
}

/// The periodic half: send an edit the operator has paused on, and hand the
/// slot back once the engine agrees with it. `busy` — a layer still being
/// dragged, or a text box with the cursor in it — keeps the slot in hand, but
/// does not hold back a send: a pause mid-sentence is still worth saving.
pub fn tick(
    edit: &mut Option<LayerEdit>,
    sent: &mut InFlight,
    presets: &sdroxide_types::ImagePresets,
    now: f64,
    busy: bool,
) -> Option<sdroxide_types::Command> {
    let e = edit.as_mut()?;
    if let Some(t) = e.pending {
        if now - t < SEND_AFTER {
            return None;
        }
        e.pending = None;
        e.sent = Some(now);
        // Compared with what the engine is about to hold, not with the presets
        // alone: going back to the old text before the first write is echoed
        // is still a change, and has to be written.
        if layers_shown(&None, sent, presets, e.slot) == e.layers.as_slice() {
            return None;
        }
        // The slot in hand is what is shown for it; an older write for it
        // still in flight would only be read once it was handed back.
        sent.remove(&e.slot);
        return Some(sdroxide_types::Command::ImageSetLayers {
            slot: e.slot as u8,
            layers: e.layers.clone(),
        });
    }
    if busy {
        return None;
    }
    let echoed = presets.slots.get(e.slot).is_some_and(|s| s.layers == e.layers);
    if echoed || e.sent.is_none_or(|t| now - t > RELEASE_AFTER) {
        *edit = None;
    }
    None
}

/// Send whatever is in hand now, keeping the slot in hand until the engine
/// echoes it — what transmitting does, so the preview does not flick back to
/// the engine's older copy for a round trip.
pub fn send_now(
    edit: &mut Option<LayerEdit>,
    sent: &mut InFlight,
    presets: &sdroxide_types::ImagePresets,
    now: f64,
) -> Option<sdroxide_types::Command> {
    let e = edit.as_mut()?;
    // Already sent: it is on its way, and a second copy would add nothing.
    e.pending?;
    e.pending = Some(now - SEND_AFTER);
    tick(edit, sent, presets, now, true)
}

/// Send whatever is in hand now and hand the slot back — what leaving it for
/// another slot does. What was sent is held in `sent` until it is echoed.
///
/// `None` when nothing was in hand or nothing changed, so a click that edits
/// nothing does not spray writes at the engine.
pub fn commit(
    edit: &mut Option<LayerEdit>,
    sent: &mut InFlight,
    presets: &sdroxide_types::ImagePresets,
    now: f64,
) -> Option<sdroxide_types::Command> {
    let e = edit.take()?;
    // Sent already and not changed since: the write is on its way, so keep
    // showing it until it lands rather than flicking back to the presets.
    if e.pending.is_none() {
        if let Some(t) = e.sent {
            let echoed = presets.slots.get(e.slot).is_some_and(|s| s.layers == e.layers);
            if !echoed {
                sent.insert(e.slot, (e.layers, t));
            }
            return None;
        }
    }
    if layers_shown(&None, sent, presets, e.slot) == e.layers.as_slice() {
        return None;
    }
    sent.insert(e.slot, (e.layers.clone(), now));
    Some(sdroxide_types::Command::ImageSetLayers { slot: e.slot as u8, layers: e.layers })
}

/// Drop the writes the engine has now echoed, and those it never echoed in
/// time (it sanitised them, or another screen wrote in between). One still in
/// flight survives an announcement that went out before the engine got to it.
pub fn settle_layers(sent: &mut InFlight, presets: &sdroxide_types::ImagePresets, now: f64) {
    sent.retain(|&slot, (layers, t)| {
        now - *t <= RELEASE_AFTER && presets.slots.get(slot).map(|s| &s.layers) != Some(layers)
    });
}

fn put(img: &mut [u8], w: usize, h: usize, x: i32, y: i32, r: u8, g: u8, b: u8) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let i = (y as usize * w + x as usize) * 3;
    img[i] = r;
    img[i + 1] = g;
    img[i + 2] = b;
}

fn blend(img: &mut [u8], w: usize, h: usize, x: i32, y: i32, r: u8, g: u8, b: u8, a: f32) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let a = a.clamp(0.0, 1.0);
    let i = (y as usize * w + x as usize) * 3;
    let mix = |o: u8, n: u8| (o as f32 * (1.0 - a) + n as f32 * a).round().clamp(0.0, 255.0) as u8;
    img[i] = mix(img[i], r);
    img[i + 1] = mix(img[i + 1], g);
    img[i + 2] = mix(img[i + 2], b);
}

/// The banner: a strip fading from the operator's colour at the top to black
/// at the bottom, their left text at the left and their right text at the
/// right. Returns how many rows of the picture it actually covered, which is
/// where the area the text layers are pinned to starts.
///
/// The type sizes off the height rather than being a setting of its own — the
/// eleven points the strip was drawn at were eleven points *because* it was
/// sixteen pixels tall, and two controls that have to be turned together are
/// worse than one.
fn draw_banner(img: &mut [u8], w: usize, h: usize, banner: &Banner) -> usize {
    let strip = usize::from(banner.height).min(h);
    if strip == 0 {
        return 0;
    }
    let [fr, fg, fb] = banner.fill;
    for y in 0..strip {
        // `t` runs 0 at the top edge to 1 at the bottom. With a second colour
        // the strip is a straight two-stop gradient; without one it keeps the
        // original fade to black.
        let t = y as f32 / strip as f32;
        let (r, g, b) = match banner.fill2 {
            Some([r2, g2, b2]) => {
                let lerp = |a: u8, c: u8| (f32::from(a) + (f32::from(c) - f32::from(a)) * t) as u8;
                (lerp(fr, r2), lerp(fg, g2), lerp(fb, b2))
            }
            None => {
                let shade = 1.0 - t;
                (
                    (f32::from(fr) * shade) as u8,
                    (f32::from(fg) * shade) as u8,
                    (f32::from(fb) * shade) as u8,
                )
            }
        };
        for x in 0..w {
            put(img, w, h, x as i32, y as i32, r, g, b);
        }
    }
    let Some(font) = message_font() else {
        return strip;
    };
    let ink = if banner.rainbow {
        Ink::Rainbow
    } else if let Some(c2) = banner.ink2 {
        Ink::Gradient(banner.ink, c2)
    } else {
        Ink::Solid(banner.ink)
    };
    let scale = PxScale::from(strip as f32 * 11.0 / 16.0);
    let baseline = (strip as f32 * 0.72).round();
    // The inset scales with the strip too, so a tall banner does not print
    // hard against the edge of the picture.
    let pad = (strip as f32 * 4.0 / 16.0).max(1.0);
    // An outline, in eight directions, ahead of the ink — the text layers'
    // technique, sized to the strip so a taller banner gets a bolder edge.
    let outline_off = (strip as f32 * 1.0 / 16.0).max(1.0);
    let draw_one = |img: &mut [u8], text: &str, x: f32| {
        if let Some(oc) = banner.outline {
            for (ox, oy) in [
                (-outline_off, 0.0),
                (outline_off, 0.0),
                (0.0, -outline_off),
                (0.0, outline_off),
                (-outline_off, -outline_off),
                (outline_off, -outline_off),
                (-outline_off, outline_off),
                (outline_off, outline_off),
            ] {
                draw_text(
                    img,
                    w,
                    h,
                    x + ox,
                    baseline + oy,
                    text,
                    &font,
                    scale,
                    Ink::Solid(oc),
                    1.0,
                );
            }
        }
        draw_text(img, w, h, x, baseline, text, &font, scale, ink, 1.0);
    };
    if !banner.left.is_empty() {
        draw_one(img, &banner.left, pad);
    }
    if !banner.right.is_empty() {
        let tw = text_width(&banner.right, &font, scale);
        draw_one(img, &banner.right, w as f32 - tw - pad);
    }
    strip
}

/// How many rows at the top of an `h`-row picture the banner covers — where
/// the area the layers are pinned to starts.
pub fn banner_rows(banner: Option<&Banner>, h: u16) -> usize {
    banner.map_or(0, |b| usize::from(b.height.min(h)))
}

/// The point an anchor stands for in a `w`×`h` picture whose top `top` rows
/// are the banner.
fn anchor_point(a: TextAnchor, w: f32, h: f32, top: f32) -> (f32, f32) {
    let (fx, fy) = a.fraction();
    (fx * w, top + fy * (h - top))
}

/// Re-pin a layer after a drag: its box, `size` picture pixels, should now
/// have its top-left at `origin`. It is pinned to whichever of the nine
/// anchors is nearest the box's centre — so a caption dragged into a corner
/// stays in that corner when the mode, and with it the picture size, changes —
/// with the offset that keeps it exactly where it was dropped. The box is kept
/// inside the picture, so text cannot be dragged out of reach.
pub fn place_at(
    layer: &mut TextLayer,
    origin: (f32, f32),
    size: (f32, f32),
    w: u16,
    h: u16,
    top: usize,
) {
    let (wf, hf, top) = (f32::from(w), f32::from(h), top as f32);
    let x0 = origin.0.clamp(0.0, (wf - size.0).max(0.0));
    let y0 = origin.1.clamp(0.0, (hf - size.1).max(0.0));
    let cx = (x0 + size.0 / 2.0) / wf;
    let cy = (y0 + size.1 / 2.0 - top) / (hf - top).max(1.0);
    layer.anchor = TextAnchor::nearest(cx, cy);
    let (fx, fy) = layer.anchor.fraction();
    let (ax, ay) = anchor_point(layer.anchor, wf, hf, top);
    layer.x = (x0 + fx * size.0 - ax) / wf * 100.0;
    layer.y = (y0 + fy * size.1 - ay) / hf * 100.0;
}

/// Pin a layer to `anchor`, a small margin in from the edges it touches —
/// the picker's "put it in that corner".
pub fn pin(layer: &mut TextLayer, anchor: TextAnchor) {
    const MARGIN: f32 = 2.0;
    let (fx, fy) = anchor.fraction();
    layer.anchor = anchor;
    layer.x = MARGIN * (1.0 - 2.0 * fx);
    layer.y = MARGIN * (1.0 - 2.0 * fy);
}

/// Resize a layer by one corner of its box, the opposite corner staying put.
///
/// `start` is the layer as it was when the corner was grabbed and `b0` the box
/// it had then; `corner` is which corner was grabbed, as fractions (`(1, 1)`
/// is bottom-right); `p` is where the pointer is now, in picture pixels. The
/// type scales by how far the pointer has travelled along the box's diagonal —
/// text keeps its proportions, so a corner cannot stretch it, only grow it —
/// and the layer is re-pinned so the far corner does not move.
pub fn resize_from(
    layer: &mut TextLayer,
    start: &TextLayer,
    b0: LayerBox,
    corner: (f32, f32),
    p: (f32, f32),
    w: u16,
    h: u16,
    top: usize,
) {
    let (bw, bh) = (b0.x1 - b0.x0, b0.y1 - b0.y0);
    if bw <= 0.0 || bh <= 0.0 || start.size <= 0.0 {
        return;
    }
    let fixed = (b0.x0 + (1.0 - corner.0) * bw, b0.y0 + (1.0 - corner.1) * bh);
    // The diagonal from the fixed corner to the grabbed one, and how far along
    // it the pointer now is.
    let d = ((corner.0 * 2.0 - 1.0) * bw, (corner.1 * 2.0 - 1.0) * bh);
    let along = ((p.0 - fixed.0) * d.0 + (p.1 - fixed.1) * d.1) / (d.0 * d.0 + d.1 * d.1);
    let size = (start.size * along).clamp(TextLayer::SIZE_MIN, TextLayer::SIZE_MAX);
    let f = size / start.size;
    let (nw, nh) = (bw * f, bh * f);
    let origin = (fixed.0 - (1.0 - corner.0) * nw, fixed.1 - (1.0 - corner.1) * nh);
    *layer = TextLayer { size, ..start.clone() };
    place_at(layer, origin, (nw, nh), w, h, top);
}

/// Move a layer by `d` picture pixels from the box `b` it has now — the arrow
/// keys' nudge. Re-pinned like a drag, and kept inside the picture.
pub fn nudge(layer: &mut TextLayer, b: LayerBox, d: (f32, f32), w: u16, h: u16, top: usize) {
    place_at(layer, (b.x0 + d.0, b.y0 + d.1), (b.x1 - b.x0, b.y1 - b.y0), w, h, top);
}

/// A new layer whose anchor point lands on picture pixel `at`, pinned to the
/// anchor nearest it.
pub fn layer_at(text: &str, at: (f32, f32), w: u16, h: u16, top: usize) -> TextLayer {
    let (wf, hf, top) = (f32::from(w), f32::from(h), top as f32);
    let anchor =
        TextAnchor::nearest(at.0 / wf, ((at.1 - top) / (hf - top).max(1.0)).clamp(0.0, 1.0));
    let (ax, ay) = anchor_point(anchor, wf, hf, top);
    TextLayer {
        text: text.to_string(),
        anchor,
        x: (at.0 - ax) / wf * 100.0,
        y: (at.1 - ay) / hf * 100.0,
        size: 12.0,
        ..TextLayer::default()
    }
}

/// Where a text layer landed in the picture, in picture pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Line height, as a multiple of the type size.
const LINE: f32 = 1.2;

/// A layer laid out: its type size once shrunk to fit, its box, and where
/// each line starts and sits.
struct Layout<'t> {
    px: f32,
    rect: LayerBox,
    lines: Vec<(&'t str, f32, f32)>,
}

/// Lay a layer out in a `w`×`h` picture whose top `top` rows are the banner.
///
/// The anchor is a point of the part below the banner; the offset and the
/// type size are fractions of the whole picture, so switching the banner off
/// moves a layer but never resizes it. A line too wide for the picture shrinks
/// the whole layer until it fits rather than running off the edge.
fn layout<'t>(
    layer: &'t TextLayer,
    font: &FontRef<'static>,
    w: usize,
    h: usize,
    top: usize,
) -> Option<Layout<'t>> {
    let lines: Vec<&str> = layer.text.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let (wf, hf) = (w as f32, h as f32);
    let mut px = (layer.size.clamp(TextLayer::SIZE_MIN, TextLayer::SIZE_MAX) / 100.0 * hf).max(4.0);
    let widest =
        |px: f32| lines.iter().map(|l| text_width(l, font, PxScale::from(px))).fold(0.0, f32::max);
    let room = wf * 0.96;
    let mut bw = widest(px);
    if bw > room {
        px = (px * room / bw).max(4.0);
        bw = widest(px);
    }
    let line_h = px * LINE;
    let bh = line_h * lines.len() as f32;
    let (fx, fy) = layer.anchor.fraction();
    let (ax, ay) = anchor_point(layer.anchor, wf, hf, top as f32);
    let (ax, ay) = (ax + layer.x / 100.0 * wf, ay + layer.y / 100.0 * hf);
    let (x0, y0) = (ax - fx * bw, ay - fy * bh);
    let ascent = font.as_scaled(PxScale::from(px)).ascent();
    let align = layer.anchor.align();
    let placed = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let lw = text_width(l, font, PxScale::from(px));
            (*l, x0 + (bw - lw) * align, y0 + i as f32 * line_h + (line_h - px) / 2.0 + ascent)
        })
        .collect();
    Some(Layout { px, rect: LayerBox { x0, y0, x1: x0 + bw, y1: y0 + bh }, lines: placed })
}

/// Draw one text layer: its shadow, its outline, then its ink. Returns the box
/// it occupies, or `None` when it has nothing to draw.
fn draw_layer(
    img: &mut [u8],
    w: usize,
    h: usize,
    layer: &TextLayer,
    top: usize,
) -> Option<LayerBox> {
    let font = message_font()?;
    let lay = layout(layer, &font, w, h, top)?;
    let scale = PxScale::from(lay.px);
    let ink = if layer.rainbow { Ink::Rainbow } else { Ink::Solid(layer.ink) };
    for &(line, x, baseline) in &lay.lines {
        if line.trim().is_empty() {
            continue;
        }
        if layer.shadow {
            let d = (lay.px * 0.07).max(1.0);
            draw_text(img, w, h, x + d, baseline + d, line, &font, scale, Ink::Solid([0; 3]), 0.6);
        }
        if layer.outline {
            // The glyphs drawn offset in eight directions, the edge as thick
            // as a twentieth of the type — the message overlay's proportions.
            let o = (lay.px * 0.05).max(1.0);
            for (ox, oy) in
                [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o), (-o, -o), (o, -o), (-o, o), (o, o)]
            {
                draw_text(
                    img,
                    w,
                    h,
                    x + ox,
                    baseline + oy,
                    line,
                    &font,
                    scale,
                    Ink::Solid(layer.outline_ink),
                    1.0,
                );
            }
        }
        draw_text(img, w, h, x, baseline, line, &font, scale, ink, 1.0);
    }
    Some(lay.rect)
}

fn text_width(text: &str, font: &FontRef<'static>, scale: PxScale) -> f32 {
    let scaled = font.as_scaled(scale);
    let mut width = 0.0;
    let mut prev = None;
    for ch in text.chars() {
        let g = font.glyph_id(ch);
        if let Some(p) = prev {
            width += scaled.kern(p, g);
        }
        width += scaled.h_advance(g);
        prev = Some(g);
    }
    width
}

/// How text is coloured as it is drawn: one colour, a gradient across the
/// picture, or a rainbow. `x` is the pixel's own position and `span` the width
/// the gradient or rainbow runs over (the picture's), so the same call draws
/// every variant.
#[derive(Clone, Copy)]
enum Ink {
    Solid([u8; 3]),
    Gradient([u8; 3], [u8; 3]),
    Rainbow,
}

impl Ink {
    /// The colour at pixel column `x`, over a run `span` wide.
    fn at(self, x: f32, span: f32) -> (u8, u8, u8) {
        let span = span.max(1.0);
        match self {
            Ink::Solid(c) => (c[0], c[1], c[2]),
            Ink::Gradient(a, b) => {
                let t = (x / span).clamp(0.0, 1.0);
                let l = |p: u8, q: u8| (f32::from(p) + (f32::from(q) - f32::from(p)) * t) as u8;
                (l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2]))
            }
            Ink::Rainbow => {
                // Hue sweeps a full turn across the picture's width. Saturation
                // and value are full, which is what makes it read as a rainbow
                // rather than a pastel.
                let hue = (x / span).rem_euclid(1.0) * 360.0;
                hsv_to_rgb(hue, 1.0, 1.0)
            }
        }
    }
}

/// HSV (h 0..360, s/v 0..1) to 8-bit RGB.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f32| ((f + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    (to(r), to(g), to(b))
}

fn draw_text(
    img: &mut [u8],
    w: usize,
    h: usize,
    x: f32,
    baseline: f32,
    text: &str,
    font: &FontRef<'static>,
    scale: PxScale,
    ink: Ink,
    alpha: f32,
) {
    let span = w as f32;
    let scaled = font.as_scaled(scale);
    let mut caret = x;
    let mut prev = None;
    for ch in text.chars() {
        let gid = font.glyph_id(ch);
        if let Some(p) = prev {
            caret += scaled.kern(p, gid);
        }
        let glyph = gid.with_scale_and_position(scale, point(caret, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                let (r, g, b) = ink.at(px as f32, span);
                blend(img, w, h, px, py, r, g, b, cov * alpha);
            });
        }
        caret += scaled.h_advance(gid);
        prev = Some(gid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::{Command, DigiConfig, ImagePresets, ImageSlotInfo, TextAnchor, TextLayer};

    fn text(t: &str) -> Vec<TextLayer> {
        vec![TextLayer { text: t.into(), ..TextLayer::default() }]
    }

    fn presets(texts: &[&str]) -> ImagePresets {
        ImagePresets {
            slots: texts
                .iter()
                .map(|t| ImageSlotInfo { layers: text(t), ..ImageSlotInfo::default() })
                .collect(),
            ..ImagePresets::default()
        }
    }

    fn fields(call: &str, grid: &str) -> Fields {
        Fields { call: call.into(), grid: grid.into(), ..Fields::default() }
    }

    fn none() -> InFlight {
        InFlight::new()
    }

    /// The echo of our own write comes back a round trip after it went out.
    /// Adopting it would snap a dragged layer back to where it was.
    #[test]
    fn a_server_echo_does_not_clobber_the_slot_being_edited() {
        let p = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        let mut cmds = Vec::new();
        edit_layers(&mut edit, &mut sent, &p, 2, 0.0, &mut cmds)[0].text =
            "CQ CQ de OE1TEST".into();
        assert!(cmds.is_empty(), "nothing else was in hand");
        assert_eq!(layers_shown(&edit, &sent, &p, 2)[0].text, "CQ CQ de OE1TEST");
        // Every other slot follows the engine.
        assert_eq!(layers_shown(&edit, &sent, &p, 3)[0].text, "d");
        assert_eq!(layers_shown(&None, &sent, &p, 2)[0].text, "old");
        // A slot the engine has never mentioned is empty rather than a panic.
        assert!(layers_shown(&None, &sent, &p, 99).is_empty());
    }

    /// Edits go out once the operator pauses, not on every frame of a drag,
    /// and the slot is handed back when the engine has echoed them.
    #[test]
    fn an_edit_is_sent_after_a_pause_and_released_on_its_echo() {
        let p = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        let mut cmds = Vec::new();
        edit_layers(&mut edit, &mut sent, &p, 2, 0.0, &mut cmds)[0].x = 10.0;
        edit_layers(&mut edit, &mut sent, &p, 2, 0.3, &mut cmds)[0].x = 20.0;
        assert_eq!(tick(&mut edit, &mut sent, &p, 0.5, true), None, "still moving");
        let Some(Command::ImageSetLayers { slot: 2, layers }) =
            tick(&mut edit, &mut sent, &p, 0.8, true)
        else {
            panic!("the paused edit should be sent");
        };
        assert_eq!(layers[0].x, 20.0);
        // Held while busy, even though sent.
        assert_eq!(tick(&mut edit, &mut sent, &p, 0.9, true), None);
        assert!(edit.is_some());
        // Not yet echoed: still held.
        assert_eq!(tick(&mut edit, &mut sent, &p, 1.0, false), None);
        assert!(edit.is_some());
        // Echoed: handed back.
        let mut echoed = p.clone();
        echoed.slots[2].layers = layers;
        assert_eq!(tick(&mut edit, &mut sent, &echoed, 1.1, false), None);
        assert!(edit.is_none());
    }

    /// An echo that never matches — another screen wrote in between — must
    /// not hold the slot for ever.
    #[test]
    fn a_slot_is_released_when_its_echo_never_comes() {
        let p = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        edit_layers(&mut edit, &mut sent, &p, 2, 0.0, &mut Vec::new())[0].text = "mine".into();
        assert!(tick(&mut edit, &mut sent, &p, 1.0, false).is_some());
        assert_eq!(tick(&mut edit, &mut sent, &p, 2.0, false), None);
        assert!(edit.is_some());
        assert_eq!(tick(&mut edit, &mut sent, &p, 4.5, false), None);
        assert!(edit.is_none(), "the engine's word is taken in the end");
    }

    #[test]
    fn editing_another_slot_commits_the_one_in_hand() {
        let p = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        let mut cmds = Vec::new();
        edit_layers(&mut edit, &mut sent, &p, 2, 0.0, &mut cmds)[0].text = "half-typed".into();
        let l4 = edit_layers(&mut edit, &mut sent, &p, 4, 0.1, &mut cmds);
        assert_eq!(l4[0].text, "e", "the new slot starts from the engine's layers");
        assert_eq!(cmds, vec![Command::ImageSetLayers { slot: 2, layers: text("half-typed") }]);
        // Committing something unchanged writes nothing.
        let mut sent = none();
        let mut same = None;
        edit_layers(&mut same, &mut sent, &p, 1, 0.0, &mut cmds);
        assert_eq!(commit(&mut same, &mut sent, &p, 0.0), None);
        assert!(same.is_none());
        assert!(sent.is_empty());
        assert_eq!(commit(&mut None, &mut sent, &p, 0.0), None);
    }

    /// #752: what goes on the air is what is on screen. A slot handed back to
    /// the engine — by moving to another one — is still read from the write in
    /// flight, not from the presets it has not reached yet.
    #[test]
    fn a_committed_edit_is_what_is_sent_until_the_engine_echoes_it() {
        let stale = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        let mut cmds = Vec::new();
        edit_layers(&mut edit, &mut sent, &stale, 2, 0.0, &mut cmds)[0].text = "new".into();
        assert!(commit(&mut edit, &mut sent, &stale, 0.1).is_some());
        assert_eq!(layers_shown(&edit, &sent, &stale, 2)[0].text, "new");

        // An announcement that left the engine before the write arrived does
        // not undo it.
        settle_layers(&mut sent, &stale, 0.2);
        assert_eq!(layers_shown(&None, &sent, &stale, 2)[0].text, "new");

        // Coming back to the slot before the echo picks up the new text, and
        // leaving it again unchanged writes nothing more.
        edit_layers(&mut edit, &mut sent, &stale, 2, 0.3, &mut cmds);
        assert_eq!(edit.as_ref().unwrap().layers[0].text, "new");
        assert_eq!(commit(&mut edit, &mut sent, &stale, 0.4), None);

        // The echo hands the slot back to the engine for good.
        let echoed = presets(&["a", "b", "new", "d", "e"]);
        settle_layers(&mut sent, &echoed, 0.5);
        assert!(sent.is_empty());
        assert_eq!(layers_shown(&None, &sent, &echoed, 2)[0].text, "new");
    }

    /// A slot sent by the pause timer and then left before its echo stays in
    /// flight too, rather than flicking back to the engine's older copy.
    #[test]
    fn leaving_a_slot_whose_edit_was_already_sent_keeps_it_in_flight() {
        let stale = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        edit_layers(&mut edit, &mut sent, &stale, 2, 0.0, &mut Vec::new())[0].text = "new".into();
        assert!(tick(&mut edit, &mut sent, &stale, 0.5, true).is_some());
        assert_eq!(commit(&mut edit, &mut sent, &stale, 0.6), None, "already on its way");
        assert_eq!(layers_shown(&None, &sent, &stale, 2)[0].text, "new");
        // Never echoed (the engine sanitised it, say): the engine's word wins
        // in the end.
        settle_layers(&mut sent, &stale, 4.0);
        assert_eq!(layers_shown(&None, &sent, &stale, 2)[0].text, "old");
    }

    /// Typing the old text back before the first write is echoed is still a
    /// change from what the engine is about to hold, so it has to be written.
    #[test]
    fn reverting_an_unechoed_edit_still_writes() {
        let stale = presets(&["a", "b", "old", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        let mut cmds = Vec::new();
        edit_layers(&mut edit, &mut sent, &stale, 2, 0.0, &mut cmds)[0].text = "new".into();
        commit(&mut edit, &mut sent, &stale, 0.1);
        edit_layers(&mut edit, &mut sent, &stale, 2, 0.2, &mut cmds)[0].text = "old".into();
        assert_eq!(
            commit(&mut edit, &mut sent, &stale, 0.3),
            Some(Command::ImageSetLayers { slot: 2, layers: text("old") }),
        );
        // The echo of the first write does not settle the second.
        settle_layers(&mut sent, &presets(&["a", "b", "new", "d", "e"]), 0.4);
        assert_eq!(layers_shown(&None, &sent, &stale, 2)[0].text, "old");
    }

    /// The stock settings have to compose the strip the header was hard-wired
    /// to draw before it could be edited, or every existing station's pictures
    /// change appearance on upgrade.
    #[test]
    fn the_stock_banner_is_the_header_that_was_hard_wired() {
        let cfg = DigiConfig { my_call: "oe1test".into(), ..DigiConfig::default() };
        let f = Fields::new(&cfg.my_call, &cfg.my_grid, "Martin 1", 0);
        let b = Banner::from_config(&cfg, &f).expect("on by default");
        assert_eq!(b.left, "OE1TEST");
        assert_eq!(b.right, format!("SDRoxide v{}", env!("CARGO_PKG_VERSION")));
        assert_eq!(b.height, 16);
        assert_eq!(b.fill, [170, 0, 0]);
    }

    #[test]
    fn switching_the_banner_off_leaves_no_strip_to_draw() {
        let cfg = DigiConfig { sstv_banner: false, ..DigiConfig::default() };
        assert_eq!(Banner::from_config(&cfg, &Fields::default()), None);
    }

    /// The callsign goes up in capitals, but only the callsign: uppercasing the
    /// whole template would turn `SDRoxide` into `SDROXIDE`.
    #[test]
    fn only_the_callsign_is_uppercased() {
        assert_eq!(expand("de {call} ", &fields(" oe1test ", "")), "de OE1TEST ");
        assert_eq!(expand("SDRoxide {grid}", &fields("", "jn88")), "SDRoxide jn88");
        assert_eq!(
            expand("v{version}", &Fields::default()),
            format!("v{}", env!("CARGO_PKG_VERSION"))
        );
        // Case-insensitive, so {CALL} works as well as {call}.
        assert_eq!(expand("{CALL}", &fields("oe1test", "")), "OE1TEST");
        // The clock and the mode.
        let f = Fields::new("oe1test", "JN88", "Martin 1", 1_790_000_000);
        assert_eq!(expand("{mode} {date} {time}", &f), "Martin 1 2026-09-21 14:13");
    }

    /// A typo has to reach the preview as itself. Swallowing it would leave the
    /// operator staring at a gap with nothing to tell them what went wrong.
    #[test]
    fn an_unknown_placeholder_survives_untouched() {
        let f = fields("oe1test", "");
        assert_eq!(expand("{callsign} {call}", &f), "{callsign} OE1TEST");
        assert_eq!(expand("100% {open", &f), "100% {open");
        assert_eq!(expand("}{}{", &f), "}{}{");
    }

    /// The text used to start at a fixed 16 rows whether or not a strip was
    /// there. With the banner off, the picture is the operator's from the top.
    #[test]
    fn the_text_starts_below_whatever_the_banner_actually_covered() {
        // A three-pixel-tall picture cannot hold a 16-pixel banner; the strip
        // must report what it covered, not what it was asked for.
        let mut img = vec![0u8; 8 * 3 * 3];
        let tall = Banner { height: 16, ..Banner::default() };
        assert_eq!(draw_banner(&mut img, 8, 3, &tall), 3);
        let none = Banner { height: 0, ..Banner::default() };
        assert_eq!(draw_banner(&mut img, 8, 3, &none), 0);
    }

    /// Composing with no banner must leave the top row of the picture alone —
    /// it is the check that "off" means off rather than "a black strip".
    #[test]
    fn composing_without_a_banner_leaves_the_top_row_of_the_picture() {
        let src = vec![200u8; 4 * 4 * 3];
        let (with, _) = compose(
            crop_scale(&src, 4, 4, 4, 4),
            4,
            4,
            Some(&Banner { height: 2, fill: [170, 0, 0], ..Banner::default() }),
            &[],
        );
        let (without, _) = compose(crop_scale(&src, 4, 4, 4, 4), 4, 4, None, &[]);
        assert_eq!(with[0], 170);
        assert_eq!(&without[..12], &[200u8; 12]);
    }

    /// The gradient option: the strip runs between the two colours instead of
    /// fading to black.
    #[test]
    fn a_gradient_strip_runs_between_its_two_colours() {
        let mut img = vec![0u8; 8 * 8 * 3];
        let b =
            Banner { height: 8, fill: [200, 0, 0], fill2: Some([0, 0, 200]), ..Banner::default() };
        assert_eq!(draw_banner(&mut img, 8, 8, &b), 8);
        assert_eq!(&img[0..3], &[200, 0, 0], "top row should be the first colour");
        let last = &img[7 * 8 * 3..7 * 8 * 3 + 3];
        assert!(last[2] > 150 && last[0] < 60, "bottom row should be mostly the second: {last:?}");
    }

    /// The outline option: the chosen outline colour reaches the strip, around
    /// the ink.
    #[test]
    fn a_text_outline_puts_the_outline_colour_in_the_strip() {
        let mut img = vec![0u8; 96 * 28 * 3];
        let b = Banner {
            height: 28,
            left: "OO".to_string(),
            fill: [0, 160, 0],
            ink: [255, 255, 255],
            outline: Some([255, 0, 0]),
            ..Banner::default()
        };
        draw_banner(&mut img, 96, 28, &b);
        let has = |c: [u8; 3]| {
            img.chunks_exact(3).any(|p| {
                (i32::from(p[0]) - i32::from(c[0])).abs() < 40
                    && (i32::from(p[1]) - i32::from(c[1])).abs() < 40
                    && (i32::from(p[2]) - i32::from(c[2])).abs() < 40
            })
        };
        assert!(has([255, 255, 255]), "the white ink is missing");
        assert!(has([255, 0, 0]), "the red outline is missing");
    }

    /// The text gradient: the ink moves from the first colour to the second
    /// across the width, so red pixels sit left of blue ones.
    #[test]
    fn a_text_gradient_runs_across_the_strip() {
        let (w, h) = (240usize, 28usize);
        let mut img = vec![0u8; w * h * 3];
        let b = Banner {
            height: 28,
            left: "IIIIIIIIII".to_string(),
            fill: [0, 0, 0],
            ink: [255, 0, 0],
            ink2: Some([0, 0, 255]),
            ..Banner::default()
        };
        draw_banner(&mut img, w, h, &b);
        // Compare the leftmost and rightmost inked pixels: a gradient moves the
        // colour from the first toward the second across the text, whatever the
        // text's own extent within the strip.
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 3;
            (i32::from(img[i]), i32::from(img[i + 1]), i32::from(img[i + 2]))
        };
        let (mut left, mut right) = (None, None);
        for y in 0..h {
            for x in 0..w {
                let p = at(x, y);
                if p.0.max(p.1).max(p.2) > 30 {
                    if left.is_none() {
                        left = Some((x, y));
                    }
                    right = Some((x, y));
                }
            }
        }
        let (lx, ly) = left.expect("some text should be drawn");
        let (rx, ry) = right.expect("some text should be drawn");
        let (lr, _, lb) = at(lx, ly);
        let (rr, _, rb) = at(rx, ry);
        assert!(rx > lx, "the text should span some width");
        assert!(lr > rr && rb > lb, "left {lr},{lb} should be redder than right {rr},{rb}");
    }

    /// A layer's rainbow covers the spectrum, not one hue.
    #[test]
    fn rainbow_text_covers_the_spectrum() {
        let (w, h) = (320usize, 120usize);
        let mut img = vec![0u8; w * h * 3];
        let layer = TextLayer {
            text: "WWWWWWWWWWWWWWWWWWWWWWWW".into(),
            rainbow: true,
            outline: false,
            size: 25.0,
            ..TextLayer::default()
        };
        draw_layer(&mut img, w, h, &layer, 0).expect("drawn");
        let (mut reds, mut greens, mut blues) = (0, 0, 0);
        for p in img.chunks_exact(3) {
            let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
            if r > 150 && g < 80 && b < 80 {
                reds += 1;
            }
            if g > 150 && r < 80 && b < 80 {
                greens += 1;
            }
            if b > 150 && r < 80 && g < 80 {
                blues += 1;
            }
        }
        assert!(reds > 0 && greens > 0 && blues > 0, "r{reds} g{greens} b{blues}");
    }

    fn boxes(w: u16, h: u16, top: u16, layers: &[TextLayer]) -> Vec<Option<LayerBox>> {
        let banner = (top > 0).then(|| Banner { height: top, ..Banner::default() });
        compose(vec![0; w as usize * h as usize * 3], w, h, banner.as_ref(), layers).1
    }

    /// Each anchor pins its own corner, edge or middle of the box to the same
    /// point of the picture, so a bottom-right caption hangs up and left from
    /// the corner rather than off it.
    #[test]
    fn a_layer_is_pinned_by_its_anchor() {
        let l = |anchor| TextLayer {
            text: "73".into(),
            anchor,
            x: 0.0,
            y: 0.0,
            ..TextLayer::default()
        };
        let b = boxes(
            320,
            256,
            0,
            &[l(TextAnchor::TopLeft), l(TextAnchor::BottomRight), l(TextAnchor::Center)],
        );
        let (tl, br, c) = (b[0].unwrap(), b[1].unwrap(), b[2].unwrap());
        assert_eq!((tl.x0, tl.y0), (0.0, 0.0));
        assert!((br.x1 - 320.0).abs() < 0.01 && (br.y1 - 256.0).abs() < 0.01);
        assert!(((c.x0 + c.x1) / 2.0 - 160.0).abs() < 0.01);
        assert!(((c.y0 + c.y1) / 2.0 - 128.0).abs() < 0.01);
    }

    /// Sizes and offsets are fractions of the picture, so the same preset looks
    /// the same in Robot 36 and in PD-290.
    #[test]
    fn a_layer_scales_with_the_picture() {
        let l =
            [TextLayer { text: "CQ".into(), x: 5.0, y: 5.0, size: 10.0, ..TextLayer::default() }];
        let small = boxes(320, 256, 0, &l)[0].unwrap();
        let big = boxes(640, 512, 0, &l)[0].unwrap();
        assert!((big.x0 - 2.0 * small.x0).abs() < 0.01);
        assert!((big.y1 - 2.0 * small.y1).abs() < 0.5);
        assert!(((big.x1 - big.x0) - 2.0 * (small.x1 - small.x0)).abs() < 1.0);
    }

    /// A top-pinned layer starts below the banner, not under it.
    #[test]
    fn a_top_layer_starts_below_the_banner() {
        let l = [TextLayer { text: "CQ".into(), x: 0.0, y: 0.0, ..TextLayer::default() }];
        assert_eq!(boxes(320, 256, 20, &l)[0].unwrap().y0, 20.0);
    }

    /// A line too wide for the picture shrinks to fit rather than running off
    /// the far edge.
    #[test]
    fn text_too_wide_is_shrunk_to_fit() {
        let l = [TextLayer {
            text: "CQ CQ CQ de OE1TEST OE1TEST".into(),
            size: 40.0,
            x: 0.0,
            ..TextLayer::default()
        }];
        let b = boxes(320, 256, 0, &l)[0].unwrap();
        assert!(b.x1 - b.x0 <= 320.0 * 0.96 + 0.5, "{b:?}");
    }

    /// Hidden and empty layers take no room and draw nothing, but keep their
    /// place in the list so the editor's indices still line up.
    #[test]
    fn hidden_layers_keep_their_place_and_draw_nothing() {
        let layers = [
            TextLayer { text: "gone".into(), visible: false, ..TextLayer::default() },
            TextLayer { text: "{call}".into(), ..TextLayer::default() },
        ];
        let resolved = resolve(&layers, &fields("oe1test", ""));
        assert_eq!(resolved[0].text, "");
        assert_eq!(resolved[1].text, "OE1TEST");
        let b = boxes(320, 256, 0, &resolved);
        assert_eq!(b.len(), 2);
        assert!(b[0].is_none() && b[1].is_some());
    }

    /// The layers a pre-layer message became land where the message was drawn:
    /// same baseline, give or take a pixel, on the 256-row picture the
    /// conversion is scaled for.
    #[test]
    fn a_converted_message_lands_where_it_was_drawn() {
        let font = message_font().unwrap();
        let conv = TextLayer::from_message("CQ\nde OE1TEST", [255; 3], Some([0; 3]), false);
        let title = layout(&conv[0], &font, 320, 256, 16).unwrap();
        let body = layout(&conv[1], &font, 320, 256, 16).unwrap();
        // The old overlay: baselines at 16 + 54 and 16 + 54 + 36, x = 6.
        assert!((title.lines[0].2 - 70.0).abs() < 1.0, "{}", title.lines[0].2);
        assert!((body.lines[0].2 - 106.0).abs() < 1.0, "{}", body.lines[0].2);
        assert!((title.lines[0].1 - 6.0).abs() < 0.01);
    }

    /// A layer dropped after a drag stays exactly where it was dropped, pinned
    /// to the nearest anchor — and so lays out into the same box again.
    #[test]
    fn a_dragged_layer_stays_where_it_is_dropped() {
        let mut l = TextLayer { text: "73".into(), ..TextLayer::default() };
        let before = boxes(320, 256, 16, std::slice::from_ref(&l))[0].unwrap();
        let size = (before.x1 - before.x0, before.y1 - before.y0);
        for (origin, anchor) in [
            ((250.0, 220.0), TextAnchor::BottomRight),
            ((140.0, 120.0), TextAnchor::Center),
            ((10.0, 30.0), TextAnchor::TopLeft),
        ] {
            place_at(&mut l, origin, size, 320, 256, 16);
            assert_eq!(l.anchor, anchor);
            let after = boxes(320, 256, 16, std::slice::from_ref(&l))[0].unwrap();
            assert!((after.x0 - origin.0).abs() < 0.01 && (after.y0 - origin.1).abs() < 0.01);
        }
        // Dragged past the edge, it stops at the edge.
        place_at(&mut l, (500.0, -40.0), size, 320, 256, 16);
        let edge = boxes(320, 256, 16, std::slice::from_ref(&l))[0].unwrap();
        assert!((edge.x1 - 320.0).abs() < 0.01 && edge.y0.abs() < 0.01, "{edge:?}");
    }

    /// Pinning to a corner puts the text just inside that corner.
    #[test]
    fn pinning_puts_the_text_just_inside_the_corner() {
        let mut l = TextLayer { text: "73".into(), ..TextLayer::default() };
        pin(&mut l, TextAnchor::BottomRight);
        let b = boxes(320, 256, 0, std::slice::from_ref(&l))[0].unwrap();
        assert!(b.x1 < 320.0 && b.x1 > 300.0 && b.y1 < 256.0 && b.y1 > 240.0, "{b:?}");
    }

    /// Pulling the bottom-right corner out grows the text and leaves its
    /// top-left where it was; pushing it in shrinks it.
    #[test]
    fn a_corner_resizes_the_text_about_the_opposite_corner() {
        let start = TextLayer { text: "73".into(), x: 10.0, y: 10.0, ..TextLayer::default() };
        let b0 = boxes(320, 256, 0, std::slice::from_ref(&start))[0].unwrap();
        let mut l = start.clone();
        let (bw, bh) = (b0.x1 - b0.x0, b0.y1 - b0.y0);
        resize_from(
            &mut l,
            &start,
            b0,
            (1.0, 1.0),
            (b0.x0 + 2.0 * bw, b0.y0 + 2.0 * bh),
            320,
            256,
            0,
        );
        assert!((l.size - 2.0 * start.size).abs() < 0.01, "{}", l.size);
        let b = boxes(320, 256, 0, std::slice::from_ref(&l))[0].unwrap();
        assert!((b.x0 - b0.x0).abs() < 0.5 && (b.y0 - b0.y0).abs() < 0.5, "{b0:?} -> {b:?}");
        // The top-left corner pulled in by half: half the size, and the
        // bottom-right is what stays.
        let mut l = start.clone();
        resize_from(
            &mut l,
            &start,
            b0,
            (0.0, 0.0),
            (b0.x0 + bw / 2.0, b0.y0 + bh / 2.0),
            320,
            256,
            0,
        );
        assert!((l.size - start.size / 2.0).abs() < 0.01, "{}", l.size);
        let b = boxes(320, 256, 0, std::slice::from_ref(&l))[0].unwrap();
        assert!((b.x1 - b0.x1).abs() < 0.5 && (b.y1 - b0.y1).abs() < 0.5, "{b0:?} -> {b:?}");
        // Dragged through the fixed corner, it stops at the smallest size.
        let mut l = start.clone();
        resize_from(&mut l, &start, b0, (1.0, 1.0), (b0.x0 - 50.0, b0.y0 - 50.0), 320, 256, 0);
        assert_eq!(l.size, TextLayer::SIZE_MIN);
    }

    #[test]
    fn a_nudge_moves_the_text_by_whole_pixels() {
        let l0 = TextLayer { text: "73".into(), x: 10.0, y: 10.0, ..TextLayer::default() };
        let b0 = boxes(320, 256, 0, std::slice::from_ref(&l0))[0].unwrap();
        let mut l = l0.clone();
        nudge(&mut l, b0, (1.0, -10.0), 320, 256, 0);
        let b = boxes(320, 256, 0, std::slice::from_ref(&l))[0].unwrap();
        assert!((b.x0 - b0.x0 - 1.0).abs() < 0.01 && (b.y0 - b0.y0 + 10.0).abs() < 0.01);
    }

    /// A layer added by double-clicking lands with its anchor on the point.
    #[test]
    fn a_layer_added_at_a_point_is_anchored_there() {
        let l = layer_at("CQ", (300.0, 240.0), 320, 256, 16);
        assert_eq!(l.anchor, TextAnchor::BottomRight);
        let b = boxes(320, 256, 16, std::slice::from_ref(&l))[0].unwrap();
        assert!((b.x1 - 300.0).abs() < 0.01 && (b.y1 - 240.0).abs() < 0.01);
    }

    /// Transmitting sends the edit at once but keeps the slot in hand, so the
    /// preview does not flick back to the engine's older copy.
    #[test]
    fn transmitting_sends_the_edit_and_keeps_the_slot() {
        let p = presets(&["a", "b", "c", "d", "e"]);
        let mut sent = none();
        let mut edit = None;
        edit_layers(&mut edit, &mut sent, &p, 1, 10.0, &mut Vec::new())[0].text = "now".into();
        assert!(matches!(
            send_now(&mut edit, &mut sent, &p, 10.0),
            Some(Command::ImageSetLayers { slot: 1, .. })
        ));
        assert_eq!(edit.as_ref().map(|e| e.slot), Some(1));
        // Nothing left to send.
        assert_eq!(send_now(&mut edit, &mut sent, &p, 10.1), None);
    }

    fn qso_fields() -> Fields {
        Fields::new("ea3abc", "JN11", "Martin 1", 0).with_qso(&sdroxide_types::SstvQso {
            call: " ea3xyz ".into(),
            rsv_sent: "595".into(),
            rsv_rcvd: "575".into(),
            name: "Ana".into(),
            note: String::new(),
            ..Default::default()
        })
    }

    /// The contact's fields print under their documented names and under the
    /// names other programs use, in any case.
    #[test]
    fn the_contact_prints_under_every_name_it_answers_to() {
        let f = qso_fields();
        assert_eq!(expand("{call} de {tocall} {rsv}", &f), "EA3ABC de EA3XYZ 595");
        assert_eq!(
            expand("{DX} {YOURCALL} {report} {RSVS} {rsvr}", &f),
            "EA3XYZ EA3XYZ 595 595 575"
        );
        assert_eq!(expand("{mycall} {MYGRID} tnx {Name}", &f), "EA3ABC JN11 tnx Ana");
    }

    /// A placeholder that would print nothing is reported once, by its
    /// documented name, whatever spelling was used.
    #[test]
    fn an_empty_placeholder_is_reported_by_its_documented_name() {
        let mut f = qso_fields();
        assert!(empty_used("{tocall} {rsv}", &f).is_empty());
        assert_eq!(empty_used("{note} {NOTE} {bogus}", &f), vec!["{note}"]);
        f.tocall.clear();
        assert_eq!(empty_used("{dx} {tocall} {yourcall}", &f), vec!["{tocall}"]);
    }

    #[test]
    fn a_held_value_is_sent_after_a_pause_and_released_on_its_echo() {
        let stored = 1u32;
        let mut held = None;
        *held_edit(&mut held, &stored, 0.0) = 2;
        assert_eq!(held_shown(&held, &stored), &2);
        assert_eq!(held_tick(&mut held, &stored, 0.2, true), None);
        assert_eq!(held_tick(&mut held, &stored, 0.5, true), Some(2));
        assert_eq!(held_tick(&mut held, &stored, 0.6, false), None);
        assert!(held.is_some(), "not echoed yet");
        assert_eq!(held_tick(&mut held, &2, 0.7, false), None);
        assert!(held.is_none(), "echoed");
        // Sent at once on demand, and not twice.
        *held_edit(&mut held, &stored, 1.0) = 3;
        assert_eq!(held_send_now(&mut held, &stored, 1.0), Some(3));
        assert_eq!(held_send_now(&mut held, &stored, 1.1), None);
    }
}

//! The engine-owned picture stores: the transmit presets an operator composes
//! from, and the galleries of what has come in.
//!
//! Both used to be client state, which meant a browser tab and the console
//! attached to the same radio showed different slots and different pictures —
//! and the browser, having no filesystem to keep them in, showed none at all.
//! The pictures themselves stay engine-side, because they are files on the
//! machine the radio is plugged into; everything here is the small metadata a
//! panel needs to draw a gallery without holding one.

use serde::{Deserialize, Serialize};

/// Which store a picture belongs to.
///
/// Two, not three: SSTV and RIFP share one because a received picture is a
/// received picture whichever protocol carried it, and the engine has always
/// written both into `sstv_rx`. Charts get their own — a quarter-hour chart
/// arriving every half hour would bury a session's SSTV, and the two are
/// browsed for entirely different reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ImageKind {
    /// SSTV and RIFP.
    #[default]
    Sstv,
    /// Weather fax.
    Wefax,
}

impl ImageKind {
    pub const ALL: [ImageKind; 2] = [ImageKind::Sstv, ImageKind::Wefax];

    /// The token this store's directory and file names are built from.
    pub fn key(self) -> &'static str {
        match self {
            ImageKind::Sstv => "sstv",
            ImageKind::Wefax => "wefax",
        }
    }

    /// What the panel calls a collection of these.
    pub fn label(self) -> &'static str {
        match self {
            ImageKind::Sstv => "pictures",
            ImageKind::Wefax => "charts",
        }
    }
}

/// How many transmit presets there are. Fixed so a slot number means the same
/// thing in the panel, in `slot{i}.png` on disk, and on the wire.
pub const IMAGE_SLOTS: usize = 5;

/// Longest edge a stored source picture is kept at.
///
/// The same bound the compositor already applied when it loaded a picked file,
/// moved to the one place that now decides it. Two reasons it has to be here:
/// the store must not hold a phone camera's forty megapixels, and every client
/// has to compose from byte-identical pixels or two operators looking at the
/// same preset would be sending different pictures.
pub const IMAGE_SOURCE_MAX_EDGE: u16 = 1024;

/// Longest edge of a preset's thumbnail — enough for the 70×54 slot button at
/// twice the pixel density.
pub const IMAGE_SLOT_THUMB_EDGE: u16 = 160;

/// Longest edge of a gallery thumbnail. A weather chart is three times wider
/// than it is tall, so this is a ~192×64 strip: legible in the list, and a
/// hundredth of the two megapixels behind it.
pub const IMAGE_THUMB_EDGE: u16 = 192;

/// Largest upload accepted for a preset slot. Generous enough for any camera
/// JPEG, small enough that it cannot be used to exhaust the engine's memory
/// from a socket that nothing authenticates.
pub const IMAGE_UPLOAD_MAX: usize = 16 * 1024 * 1024;

/// Most entries one listing request may return.
pub const IMAGE_PAGE_MAX: u32 = 48;

/// Longest file name accepted from a client.
pub const IMAGE_NAME_MAX: usize = 128;

/// One transmit preset as a panel sees it. The source pixels are fetched
/// separately, and only for the slot being composed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageSlotInfo {
    /// The text composited over this slot's picture, bottom layer first. Per
    /// slot, not per session: switching slots swaps the text, which is what
    /// makes the five of them presets rather than one picture and one caption.
    pub layers: Vec<TextLayer>,
    /// Size of the stored source; `(0, 0)` when the slot holds no picture.
    pub width: u16,
    pub height: u16,
    /// Fingerprint of the stored bytes, 0 when the slot is empty.
    ///
    /// A fingerprint rather than a counter on purpose. A client caches the
    /// source pixels it fetched and has to know when they have gone stale; a
    /// counter restarts at zero when the engine does, so a client still holding
    /// "version 1" of the old picture would accept the new one's "version 1"
    /// and go on composing the wrong photograph.
    pub version: u32,
    /// PNG thumbnail, longest edge [`IMAGE_SLOT_THUMB_EDGE`].
    ///
    /// Carried in the snapshot rather than fetched, because the panel draws all
    /// five slot buttons at once and only ever composes from one of them.
    pub thumb: Vec<u8>,
}

impl ImageSlotInfo {
    pub fn has_picture(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// Every transmit preset, emitted at startup and on every change — the same
/// contract as [`crate::VoiceStatus`], and for the same reason: what the
/// station sends belongs to the radio, not to whichever screen is looking at it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagePresets {
    /// Always [`IMAGE_SLOTS`] entries; index = slot number.
    pub slots: Vec<ImageSlotInfo>,
}

impl Default for ImagePresets {
    fn default() -> Self {
        ImagePresets { slots: vec![ImageSlotInfo::default(); IMAGE_SLOTS] }
    }
}

impl ImagePresets {
    pub fn slot(&self, i: usize) -> ImageSlotInfo {
        self.slots.get(i).cloned().unwrap_or_default()
    }
}

/// Most text layers one slot keeps. Far more than a 320-pixel picture can
/// carry legibly; the cap is there so a client cannot grow the preset file
/// without bound.
pub const IMAGE_LAYERS_MAX: usize = 16;

/// Longest text one layer keeps, in characters.
pub const IMAGE_LAYER_TEXT_MAX: usize = 512;

/// Where a text layer is pinned: one of nine points of the picture — its
/// corners, the middles of its edges, and its centre. The part of the picture
/// below the banner strip, when there is one, so a layer pinned to the top is
/// never printed over the banner's own text.
///
/// Pinned rather than placed at a pixel because the transmitted size changes
/// with the mode: a caption pinned bottom-right stays bottom-right in Robot 36
/// and in PD-290, where a pixel position would drift off one edge or the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextAnchor {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl TextAnchor {
    /// Row by row, top-left first — the order a 3×3 picker lays them out in.
    pub const ALL: [TextAnchor; 9] = [
        TextAnchor::TopLeft,
        TextAnchor::Top,
        TextAnchor::TopRight,
        TextAnchor::Left,
        TextAnchor::Center,
        TextAnchor::Right,
        TextAnchor::BottomLeft,
        TextAnchor::Bottom,
        TextAnchor::BottomRight,
    ];

    /// Where the anchor sits along each axis: 0 at the left or top edge, 0.5
    /// in the middle, 1 at the right or bottom edge. The same fraction places
    /// the anchor on the picture and on the layer's own box, which is what
    /// makes a bottom-right layer hang up and left from the corner.
    pub fn fraction(self) -> (f32, f32) {
        let i = TextAnchor::ALL.iter().position(|a| *a == self).unwrap_or(0);
        ((i % 3) as f32 * 0.5, (i / 3) as f32 * 0.5)
    }

    /// The anchor nearest a point given as fractions of the picture.
    pub fn nearest(fx: f32, fy: f32) -> TextAnchor {
        let third = |f: f32| {
            if f < 1.0 / 3.0 {
                0
            } else if f < 2.0 / 3.0 {
                1
            } else {
                2
            }
        };
        TextAnchor::ALL[third(fy) * 3 + third(fx)]
    }

    /// How lines of a layer line up with one another: flush with whichever
    /// edge the layer is pinned to, centred when it is pinned to neither.
    pub fn align(self) -> f32 {
        self.fraction().0
    }
}

/// One piece of text over a transmit picture.
///
/// Positions and sizes are fractions of the transmitted picture, not pixels,
/// for the reason [`TextAnchor`] gives: the same preset is sent at 320×240 and
/// at 800×616, and has to look the same in both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextLayer {
    /// What is printed. May run over several lines, and may carry the same
    /// `{call}`-style placeholders as the banner.
    pub text: String,
    pub anchor: TextAnchor,
    /// Offset from the anchor, in percent of the picture's width; positive is
    /// to the right.
    pub x: f32,
    /// Offset from the anchor, in percent of the picture's height; positive is
    /// down.
    pub y: f32,
    /// Type size, in percent of the picture's height. Text wider than the
    /// picture is shrunk to fit rather than cut off.
    pub size: f32,
    pub ink: [u8; 3],
    /// Draw an edge around the glyphs in [`Self::outline_ink`], so the text
    /// stays readable over any part of any photograph.
    pub outline: bool,
    pub outline_ink: [u8; 3],
    /// A soft dark shadow below and right of the text.
    pub shadow: bool,
    /// Paint the text as a horizontal rainbow instead of [`Self::ink`].
    pub rainbow: bool,
    /// Hidden layers stay in the preset but are not drawn.
    pub visible: bool,
}

impl Default for TextLayer {
    /// White with a black edge, which is what the slot message always was:
    /// readable over anything.
    fn default() -> Self {
        TextLayer {
            text: String::new(),
            anchor: TextAnchor::TopLeft,
            x: 2.0,
            y: 2.0,
            size: 10.0,
            ink: [255, 255, 255],
            outline: true,
            outline_ink: [0, 0, 0],
            shadow: false,
            rainbow: false,
            visible: true,
        }
    }
}

impl TextLayer {
    /// Smallest and largest type size accepted, in percent of the height.
    pub const SIZE_MIN: f32 = 2.0;
    pub const SIZE_MAX: f32 = 60.0;

    /// Bring a layer from a client, or from disk, into range: a size that
    /// cannot be drawn, an offset that throws the text a mile off the picture,
    /// or a paste of a novel.
    pub fn sanitise(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, dflt: f32| {
            if v.is_finite() { v.clamp(lo, hi) } else { dflt }
        };
        self.size = fix(self.size, Self::SIZE_MIN, Self::SIZE_MAX, 10.0);
        self.x = fix(self.x, -100.0, 100.0, 0.0);
        self.y = fix(self.y, -100.0, 100.0, 0.0);
        if let Some((cut, _)) = self.text.char_indices().nth(IMAGE_LAYER_TEXT_MAX) {
            self.text.truncate(cut);
        }
    }

    /// The layers a slot message from before layers existed becomes.
    ///
    /// The old overlay printed its first line half as large again as the rest
    /// — a title — at the top left, starting just below the banner, at a fixed
    /// 45 and 30 pixels. Those are fractions of the 256-row Martin and Scottie
    /// picture most stations send, which is the size an existing preset has to
    /// look unchanged at. One layer for the title and one for the lines below
    /// it, the second offset by exactly the title's line.
    pub fn from_message(
        message: &str,
        ink: [u8; 3],
        outline: Option<[u8; 3]>,
        rainbow: bool,
    ) -> Vec<TextLayer> {
        const REF_H: f32 = 256.0;
        const REF_W: f32 = 320.0;
        const LINE: f32 = 1.2;
        // The old overlay put a line's baseline a whole line height (1.2 of
        // the size) below where the line started. A layer sits its glyphs
        // snugly in its box instead — half the leading, then the ascent, 0.763
        // of the size in the bundled ChakraPetch — so its text starts this
        // much higher, and the converted layer is moved down by the difference.
        const DROP: f32 = LINE - (0.1 + 992.0 / 1300.0);
        if message.trim().is_empty() {
            return Vec::new();
        }
        let base = TextLayer {
            anchor: TextAnchor::TopLeft,
            x: 6.0 / REF_W * 100.0,
            y: DROP * 45.0 / REF_H * 100.0,
            ink,
            outline: outline.is_some(),
            outline_ink: outline.unwrap_or([0, 0, 0]),
            rainbow,
            ..TextLayer::default()
        };
        let title_px = 45.0;
        let (first, rest) = message.split_once('\n').unwrap_or((message, ""));
        let mut out = vec![TextLayer {
            text: first.to_string(),
            size: title_px / REF_H * 100.0,
            ..base.clone()
        }];
        if !rest.trim().is_empty() {
            out.push(TextLayer {
                text: rest.to_string(),
                size: 30.0 / REF_H * 100.0,
                y: (title_px * LINE + DROP * 30.0) / REF_H * 100.0,
                ..base
            });
        }
        out
    }
}

/// Bring a slot's layer list from a client or from disk into range.
pub fn sanitise_layers(layers: &mut Vec<TextLayer>) {
    layers.truncate(IMAGE_LAYERS_MAX);
    for l in layers.iter_mut() {
        l.sanitise();
    }
}

/// One picture in a received store, as a gallery lists it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageEntry {
    pub kind: ImageKind,
    /// Bare file name inside the store — never a path. It is also the whole of
    /// a chart's metadata (see [`crate::WefaxChartMeta`]) and the key every
    /// fetch names it by, which is why it is sanitised on the way back in.
    pub name: String,
    /// Unix seconds it was received, read out of the name; the file's
    /// modification time for anything this program did not write.
    pub unix: i64,
    pub width: u16,
    pub height: u16,
    /// Size of the stored file, so a gallery can say what a full-size fetch
    /// will cost before pulling it down a phone link.
    pub bytes: u64,
    /// PNG thumbnail, longest edge [`IMAGE_THUMB_EDGE`].
    pub thumb: Vec<u8>,
    /// What the RIFP manifest said, for a picture that has just arrived over
    /// RIFP. Always `None` for anything read back off disk: the manifest is not
    /// in the PNG and there is nowhere it survives, so the enlarged view says
    /// nothing rather than inventing a sender.
    pub rifp: Option<crate::RifpMeta>,
}

/// One page of a received store, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageListing {
    pub kind: ImageKind,
    /// Where `entries` starts within the whole store.
    pub offset: u32,
    /// How many pictures the store holds altogether, so a gallery can say
    /// "312 older" exactly instead of guessing from what it was given.
    pub total: u32,
    pub entries: Vec<ImageEntry>,
    /// The directory the pictures are in, for the panel to show. On a remote
    /// client this names a directory on the *radio's* machine, which is why the
    /// panel has to say so rather than just printing it.
    pub dir: String,
}

/// When a stored picture was received, read out of its file name.
///
/// The name is the only timestamp there is — neither store writes a sidecar —
/// so every form this program has ever written is accepted. `None` for a file
/// it did not write, and the caller falls back to the modification time.
pub fn received_at(kind: ImageKind, name: &str) -> Option<i64> {
    match kind {
        // `sstv-<unix millis>.png`, as the engine writes it.
        ImageKind::Sstv => {
            let rest = name.strip_prefix("sstv-")?;
            let digits = rest.strip_suffix(".png").or_else(|| rest.strip_suffix(".PNG"))?;
            digits.parse::<i64>().ok().map(|ms| ms / 1000)
        }
        ImageKind::Wefax => crate::WefaxChartMeta::from_file_name(name).map(|m| m.unix),
    }
}

/// Accept a file name from a client, or refuse it.
///
/// Names arrive over a socket that nothing authenticates and end up joined to a
/// directory this program will read. So: nothing but the characters our own
/// names use, no leading dot, no `..`, and a `.png` extension — which every
/// picture in either store has. Refusing is cheap and there is no legitimate
/// name this rejects; a file the operator dropped in by hand with a space in it
/// simply never appears in a listing, which is better than handing back a name
/// that cannot then be fetched.
pub fn safe_name(name: &str) -> Option<&str> {
    if name.is_empty() || name.len() > IMAGE_NAME_MAX {
        return None;
    }
    if name.starts_with('.') || name.contains("..") {
        return None;
    }
    // One pass kills `/`, `\`, `:`, NUL, newline and every Unicode look-alike
    // separator, rather than a blocklist that has to keep up with them.
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) {
        return None;
    }
    let dot = name.rfind('.')?;
    name[dot..].eq_ignore_ascii_case(".png").then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name is a fetch key that arrives over an unauthenticated socket and
    /// is joined to a directory we then read. Everything that could reach out
    /// of that directory has to be refused, and every name either store
    /// actually writes has to be accepted.
    #[test]
    fn a_name_from_outside_the_store_is_refused() {
        for bad in [
            "",
            "../../etc/passwd",
            "../secret.png",
            "..",
            "a/b.png",
            "a\\b.png",
            "/etc/shadow.png",
            ".hidden.png",
            "chart.jpg",
            "chart.png.jpg",
            "chart",
            "chart.png\0",
            "chart\n.png",
            "chart .png",
            "chárt.png",
            // A separator that is not ASCII is still a separator.
            "a\u{2044}b.png",
        ] {
            assert_eq!(safe_name(bad), None, "{bad:?} should be refused");
        }
        // Too long, one byte over.
        let long = format!("{}.png", "x".repeat(IMAGE_NAME_MAX));
        assert_eq!(safe_name(&long), None);

        // And the names the two stores really write, dots and all.
        for good in [
            "sstv-1753795200000.png",
            "wefax-20260729-141530Z-7878.1kHz-DWD.png",
            "wefax-20260729-141530Z.png",
            "chart.PNG",
            "holiday_2026.png",
        ] {
            assert_eq!(safe_name(good), Some(good), "{good:?} should be accepted");
        }
    }

    /// A gallery is browsed newest first, and the only timestamp either store
    /// keeps is in the file name.
    #[test]
    fn a_stored_name_carries_the_time_it_was_received() {
        assert_eq!(received_at(ImageKind::Sstv, "sstv-1753795200000.png"), Some(1_753_795_200));
        assert_eq!(received_at(ImageKind::Sstv, "sstv-1753795200000.PNG"), Some(1_753_795_200));
        assert_eq!(
            received_at(ImageKind::Wefax, "wefax-20260729-141530Z-7878.1kHz-DWD.png"),
            Some(crate::ymd_hms_to_unix(2026, 7, 29, 14, 15, 30)),
        );
        // The millisecond names charts were saved under before the date went in.
        assert_eq!(received_at(ImageKind::Wefax, "wefax-1753795200000.png"), Some(1_753_795_200));
        // Anything else has no date to give, and the caller falls back to mtime
        // rather than inventing one.
        for foreign in ["holiday.png", "sstv-.png", "sstv-abc.png", "wefax.png"] {
            assert_eq!(received_at(ImageKind::Sstv, foreign), None, "{foreign}");
            assert_eq!(received_at(ImageKind::Wefax, foreign), None, "{foreign}");
        }
    }

    #[test]
    fn the_presets_default_to_five_empty_slots() {
        let p = ImagePresets::default();
        assert_eq!(p.slots.len(), IMAGE_SLOTS);
        assert!(p.slots.iter().all(|s| !s.has_picture() && s.version == 0 && s.layers.is_empty()));
        // Asking for a slot that is not there gives an empty one rather than
        // panicking — the panel indexes this from a fixed-size strip.
        assert!(!p.slot(99).has_picture());
    }

    /// A pre-layer message has to come out looking as it did: a large title
    /// line and the smaller lines below it, at the top left.
    #[test]
    fn an_old_message_becomes_a_title_layer_and_a_body_layer() {
        let l = TextLayer::from_message(
            "CQ SSTV\nde OE1TEST\nJN88",
            [255, 0, 0],
            Some([0, 0, 0]),
            false,
        );
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].text, "CQ SSTV");
        assert_eq!(l[1].text, "de OE1TEST\nJN88");
        assert!(l[0].size > l[1].size);
        assert!(l[1].y > l[0].y && l[0].y > 0.0);
        assert!(l.iter().all(|x| x.anchor == TextAnchor::TopLeft && x.ink == [255, 0, 0]));
        // One line is one layer, and nothing is none.
        assert_eq!(TextLayer::from_message("73", [255; 3], None, false).len(), 1);
        assert!(TextLayer::from_message("  \n ", [255; 3], None, false).is_empty());
    }

    #[test]
    fn anchors_round_trip_through_their_grid_position() {
        for a in TextAnchor::ALL {
            let (fx, fy) = a.fraction();
            assert_eq!(TextAnchor::nearest(fx, fy), a);
        }
        assert_eq!(TextAnchor::nearest(0.9, 0.95), TextAnchor::BottomRight);
        assert_eq!(TextAnchor::nearest(0.5, 0.1), TextAnchor::Top);
    }

    #[test]
    fn a_layer_from_outside_is_brought_into_range() {
        let mut v = vec![
            TextLayer {
                size: f32::NAN,
                x: 1e9,
                text: "x".repeat(2000),
                ..TextLayer::default()
            };
            40
        ];
        sanitise_layers(&mut v);
        assert_eq!(v.len(), IMAGE_LAYERS_MAX);
        assert_eq!(v[0].size, 10.0);
        assert_eq!(v[0].x, 100.0);
        assert_eq!(v[0].text.chars().count(), IMAGE_LAYER_TEXT_MAX);
    }
}

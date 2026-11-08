//! The bundled font files, embedded once and shared by the two things that
//! draw with them: egui, for the interface, and the SSTV compositor, which
//! rasterises a transmit picture's text itself.
//!
//! Every face is under the SIL Open Font License, except Roboto Slab, which is
//! under the Apache License 2.0, and FE-Schrift, which sdroxide builds itself
//! from a public-domain drawing (`tools/gen_fe_font.py`) and dedicates to the
//! public domain (CC0); the licence of each sits beside it in `assets/fonts/`.
//!
//! The faces a picture's text can be drawn in are bundled, never taken from
//! the system: the picture is composed on whichever screen is in front of the
//! operator — the console, another computer, a browser tab — and has to come
//! out the same on all of them.

/// Chakra Petch Regular: the interface's own text.
pub const CHAKRA_REGULAR: &[u8] = include_bytes!("../assets/fonts/ChakraPetch-Regular.ttf");
/// Chakra Petch SemiBold: headings, the picture banner, and the default face
/// of a picture's text layers.
pub const CHAKRA_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf");
/// Share Tech Mono: the interface's monospace, and a face for picture text.
pub const SHARE_TECH_MONO: &[u8] = include_bytes!("../assets/fonts/ShareTechMono-Regular.ttf");

/// The bytes of the face a text layer names, by its id in
/// [`sdroxide_types::TEXT_FONTS`]. `None` for an id this build does not ship.
pub fn layer_face(id: &str) -> Option<&'static [u8]> {
    Some(match id {
        "" => CHAKRA_SEMIBOLD,
        "oswald" => include_bytes!("../assets/fonts/Oswald-Bold.ttf"),
        "roboto-slab" => include_bytes!("../assets/fonts/RobotoSlab-Bold.ttf"),
        "share-tech-mono" => SHARE_TECH_MONO,
        "comfortaa" => include_bytes!("../assets/fonts/Comfortaa-Bold.ttf"),
        "pacifico" => include_bytes!("../assets/fonts/Pacifico-Regular.ttf"),
        "black-ops-one" => include_bytes!("../assets/fonts/BlackOpsOne-Regular.ttf"),
        "fe-schrift" => include_bytes!("../assets/fonts/FESchrift-sdroxide.otf"),
        _ => return None,
    })
}

/// The egui font family a layer face is registered under (by
/// `theme::install_fonts`), so the editor can show each face's name in the
/// face itself.
pub fn layer_family(id: &str) -> eframe::egui::FontFamily {
    eframe::egui::FontFamily::Name(format!("sstv-face-{id}").into())
}

/// [`layer_family`] if `ctx` has it, else the plain proportional family: egui
/// panics on a family nobody registered, and a context the app's fonts were
/// never installed in — a headless test's — has none of them.
pub fn layer_family_in(ctx: &eframe::egui::Context, id: &str) -> eframe::egui::FontFamily {
    let family = layer_family(id);
    if ctx.fonts(|f| f.definitions().families.contains_key(&family)) {
        family
    } else {
        eframe::egui::FontFamily::Proportional
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every face the editor offers is bundled and parses, so none of them
    /// silently draws in the fallback.
    #[test]
    fn every_listed_face_is_bundled_and_parses() {
        for (id, name) in sdroxide_types::TEXT_FONTS {
            let bytes = layer_face(id).unwrap_or_else(|| panic!("{name} is not bundled"));
            let font = ab_glyph::FontRef::try_from_slice(bytes)
                .unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
            use ab_glyph::Font;
            for ch in "CQ de EA3ABC 73 595".chars() {
                assert_ne!(font.glyph_id(ch).0, 0, "{name} has no {ch:?}");
            }
        }
        assert!(layer_face("no-such-face").is_none());
    }
}

//! The screen's words for values defined in `sdroxide-types`.
//!
//! Those types carry an English `label()` of their own, and it stays: it is
//! what a log, a config file or the engine reads, and it must not change with
//! the operator's language. What the interface draws comes from here instead.
//! Each is an exhaustive `match`, so a variant added to a type is a build
//! error here until it has a message.

use sdroxide_types::{
    BandplanKind, CallsignStyle, ChromeStyle, FontSize, FreqStyle, LayoutMode, UiTheme, Verbosity,
};

use super::tr;

/// A value's name, in the interface's language.
pub(crate) trait Tr: Copy {
    fn tr(self) -> String;
}

impl Tr for LayoutMode {
    fn tr(self) -> String {
        match self {
            LayoutMode::Auto => tr!("layout-mode-auto"),
            LayoutMode::Desktop => tr!("layout-mode-desktop"),
            LayoutMode::Tablet => tr!("layout-mode-tablet"),
            LayoutMode::Small => tr!("layout-mode-small"),
            LayoutMode::Phone => tr!("layout-mode-phone"),
        }
    }
}

impl Tr for UiTheme {
    fn tr(self) -> String {
        match self {
            UiTheme::Default => tr!("ui-theme-default"),
            UiTheme::Light => tr!("ui-theme-light"),
            UiTheme::HighContrast => tr!("ui-theme-high-contrast"),
            UiTheme::GreenPhosphor => tr!("ui-theme-green-phosphor"),
            UiTheme::AmberPhosphor => tr!("ui-theme-amber-phosphor"),
            UiTheme::TealOrange => tr!("ui-theme-teal-orange"),
            UiTheme::Rainbow => tr!("ui-theme-rainbow"),
            UiTheme::Nord => tr!("ui-theme-nord"),
            UiTheme::NordDark => tr!("ui-theme-nord-dark"),
            UiTheme::Gruvbox => tr!("ui-theme-gruvbox"),
            UiTheme::Everforest => tr!("ui-theme-everforest"),
            UiTheme::SolarizedDark => tr!("ui-theme-solarized-dark"),
            UiTheme::Solarized => tr!("ui-theme-solarized"),
            UiTheme::Dracula => tr!("ui-theme-dracula"),
            UiTheme::CatppuccinMocha => tr!("ui-theme-catppuccin-mocha"),
            UiTheme::CatppuccinLatte => tr!("ui-theme-catppuccin-latte"),
            UiTheme::ModernMinimalist => tr!("ui-theme-modern-minimalist"),
        }
    }
}

impl Tr for ChromeStyle {
    fn tr(self) -> String {
        match self {
            ChromeStyle::Angled => tr!("chrome-style-angled"),
            ChromeStyle::Rectangular => tr!("chrome-style-rectangular"),
            ChromeStyle::Rounded => tr!("chrome-style-rounded"),
            ChromeStyle::Gradient => tr!("chrome-style-gradient"),
            ChromeStyle::Bevel => tr!("chrome-style-bevel"),
            ChromeStyle::Terminal => tr!("chrome-style-terminal"),
        }
    }
}

impl Tr for FontSize {
    fn tr(self) -> String {
        match self {
            FontSize::Small => tr!("font-size-small"),
            FontSize::Medium => tr!("font-size-medium"),
            FontSize::Large => tr!("font-size-large"),
        }
    }
}

impl Tr for BandplanKind {
    fn tr(self) -> String {
        match self {
            BandplanKind::Ham => tr!("bandplan-kind-ham"),
            BandplanKind::Cw => tr!("bandplan-kind-cw"),
            BandplanKind::Digi => tr!("bandplan-kind-digi"),
            BandplanKind::Phone => tr!("bandplan-kind-phone"),
            BandplanKind::Beacon => tr!("bandplan-kind-beacon"),
            BandplanKind::Broadcast => tr!("bandplan-kind-broadcast"),
            BandplanKind::Am => tr!("bandplan-kind-am"),
            BandplanKind::Cb => tr!("bandplan-kind-cb"),
        }
    }
}

impl Tr for Verbosity {
    fn tr(self) -> String {
        match self {
            Verbosity::Terse => tr!("speech-verbosity-terse"),
            Verbosity::Normal => tr!("speech-verbosity-normal"),
            Verbosity::Full => tr!("speech-verbosity-full"),
        }
    }
}

impl Tr for FreqStyle {
    fn tr(self) -> String {
        match self {
            FreqStyle::Dial => tr!("speech-freq-style-dial"),
            FreqStyle::Digits => tr!("speech-freq-style-digits"),
            FreqStyle::Full => tr!("speech-freq-style-full"),
        }
    }
}

impl Tr for CallsignStyle {
    fn tr(self) -> String {
        match self {
            CallsignStyle::Phonetic => tr!("speech-callsign-style-phonetic"),
            CallsignStyle::Letters => tr!("speech-callsign-style-letters"),
            CallsignStyle::AsIs => tr!("speech-callsign-style-as-is"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In English every value reads exactly as its `label()` does, so moving a
    /// picker onto these changes nothing for an English-speaking operator.
    /// Holds only while the loader is in English, which no test changes.
    #[test]
    fn english_matches_the_types_own_labels() {
        fn check<T: Tr + std::fmt::Debug>(all: &[T], label: fn(T) -> &'static str) {
            for &v in all {
                assert_eq!(v.tr(), label(v), "{v:?}");
            }
        }
        check(&LayoutMode::ALL, LayoutMode::label);
        check(&UiTheme::ALL, UiTheme::label);
        check(&ChromeStyle::ALL, ChromeStyle::label);
        check(&FontSize::ALL, FontSize::label);
        check(&BandplanKind::ALL, BandplanKind::label);
        check(&Verbosity::ALL, Verbosity::label);
        check(&FreqStyle::ALL, FreqStyle::label);
        check(&CallsignStyle::ALL, CallsignStyle::label);
    }
}

//! Catppuccin Mocha (dark) and Latte (light) with Klefki accents. One `Palette` per theme, mapped to
//! `egui::Visuals`, plus fonts and sizing.
//!
//! Klefki is a Steel/Fairy Pokémon: a ring of keys. Steel greys are the neutrals, mauve (the ring) is the
//! primary colour, key gold marks keys and secrets, and fairy pink marks focus.
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};
use std::sync::Arc;

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub struct Palette {
    /// Text fields and code: Mocha crust.
    pub field: Color32,
    /// Nav and top bar: Mocha mantle.
    pub chrome: Color32,
    /// Main surfaces: Mocha base.
    pub panel: Color32,
    /// Buttons, windows: Mocha surface0.
    pub panel_raised: Color32,
    pub row_alt: Color32,
    pub border: Color32,
    /// Fill of a hovered control.
    pub hover: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Klefki's ring.
    pub primary: Color32,
    /// Hyperlinks (deep blue in light, Mocha blue in dark).
    pub link: Color32,
    /// Hover and focus outlines (fairy pink).
    pub focus: Color32,
    /// Key gold: keys and secrets.
    pub key: Color32,
    pub selection: Color32,
    pub success: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub disabled: Color32,
    /// Text on a bright fill (badges).
    pub on_bright: Color32,
}

pub const DARK: Palette = Palette {
    field: hex(0x11111B),
    chrome: hex(0x181825),
    panel: hex(0x1E1E2E),
    panel_raised: hex(0x313244),
    row_alt: hex(0x232334),
    border: hex(0x45475A),
    hover: hex(0x3B3D50),
    text: hex(0xCDD6F4),
    text_muted: hex(0xA6ADC8),
    primary: hex(0xCBA6F7),
    link: hex(0x89B4FA),
    focus: hex(0xF5C2E7),
    key: hex(0xF9E2AF),
    selection: hex(0x403957),
    success: hex(0xA6E3A1),
    warn: hex(0xFAB387),
    danger: hex(0xF38BA8),
    disabled: hex(0x7F849C),
    on_bright: hex(0x11111B),
};

/// Catppuccin Latte surfaces and text. Latte's own pastel accents are too light to read as text on a
/// light background, so the accents here are deeper variants of the same hues.
pub const LIGHT: Palette = Palette {
    field: hex(0xFFFFFF),
    chrome: hex(0xE6E9EF),
    panel: hex(0xEFF1F5),
    panel_raised: hex(0xFFFFFF),
    row_alt: hex(0xE9ECF1),
    border: hex(0xBCC0CC),
    hover: hex(0xE0E4EB),
    text: hex(0x4C4F69),
    text_muted: hex(0x5C5F77),
    primary: hex(0x7C2FE0),
    link: hex(0x1A66A6),
    focus: hex(0xCF3F9B),
    key: hex(0x8A5A00),
    selection: hex(0xE7DFFA),
    success: hex(0x26741A),
    warn: hex(0xA83F05),
    danger: hex(0xC40F35),
    disabled: hex(0x8C8FA1),
    on_bright: hex(0xFFFFFF),
};

/// The palette for the theme egui is currently showing.
pub fn palette(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        egui::Theme::Dark => &DARK,
        egui::Theme::Light => &LIGHT,
    }
}

/// What the user picked. `System` follows the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn icon(self) -> &'static str {
        match self {
            Self::System => egui_phosphor::regular::DESKTOP,
            Self::Light => egui_phosphor::regular::SUN,
            Self::Dark => egui_phosphor::regular::MOON,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        }
    }
}

const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");

/// Family for emphasised text; JetBrains Mono Bold.
pub fn bold_family() -> FontFamily {
    FontFamily::Name("jetbrains-mono-bold".into())
}

pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    fonts.font_data.insert(
        "jetbrains-mono".into(),
        Arc::new(FontData::from_static(FONT_REGULAR)),
    );
    fonts.font_data.insert(
        "jetbrains-mono-bold".into(),
        Arc::new(FontData::from_static(FONT_BOLD)),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "jetbrains-mono".into());
    }
    // Bold falls back to the regular stack, so icons and symbols still render.
    let mut bold = vec!["jetbrains-mono-bold".to_owned()];
    bold.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(bold_family(), bold);
    fonts
}

pub fn visuals(p: &Palette, dark: bool) -> Visuals {
    let rounding = CornerRadius::same(6);
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel;
    v.window_fill = p.panel_raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = rounding;
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.row_alt;
    v.code_bg_color = p.field;
    v.hyperlink_color = p.link;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.primary);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_fill = p.panel_raised;
    w.inactive.weak_bg_fill = p.panel_raised;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.focus);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.active.bg_fill = p.selection;
    w.active.weak_bg_fill = p.selection;
    w.active.bg_stroke = Stroke::new(1.0, p.primary);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.open.bg_fill = p.panel_raised;
    w.open.weak_bg_fill = p.panel_raised;
    for ws in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        ws.corner_radius = rounding;
        // Hover and press change colour only: no growing, no thicker outlines.
        ws.expansion = 0.0;
    }
    v
}

/// Sizes are generous on purpose: this is a desktop tool used with a mouse, not a dense data grid.
pub fn style(style: &mut egui::Style) {
    use FontFamily::{Monospace, Proportional};
    style.text_styles = [
        (TextStyle::Small, FontId::new(12.0, Proportional)),
        (TextStyle::Body, FontId::new(15.0, Proportional)),
        (TextStyle::Button, FontId::new(15.0, Proportional)),
        (TextStyle::Heading, FontId::new(22.0, bold_family())),
        (TextStyle::Monospace, FontId::new(14.0, Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size = egui::vec2(40.0, 34.0);
    style.spacing.text_edit_width = 240.0;
    // Selectable labels swallow clicks meant for table rows.
    style.interaction.selectable_labels = false;
}

/// Fonts, sizing and both themes' visuals. Done once at start-up.
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_fonts(fonts());
    for (theme, palette, dark) in [
        (egui::Theme::Dark, &DARK, true),
        (egui::Theme::Light, &LIGHT, false),
    ] {
        ctx.set_visuals_of(theme, visuals(palette, dark));
        ctx.style_mut_of(theme, style);
    }
    set_choice(ctx, choice);
}

const STORAGE_KEY: &str = "theme";

/// The remembered choice, or Dark when nothing valid was saved.
pub fn load_choice(storage: Option<&dyn eframe::Storage>) -> ThemeChoice {
    storage
        .and_then(|s| s.get_string(STORAGE_KEY))
        .and_then(|v| ThemeChoice::parse(&v))
        .unwrap_or(ThemeChoice::Dark)
}

pub fn save_choice(storage: &mut dyn eframe::Storage, choice: ThemeChoice) {
    storage.set_string(STORAGE_KEY, choice.as_str().to_owned());
}

pub fn set_choice(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(choice.preference());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// In-memory stand-in for eframe's key-value storage.
    #[derive(Default)]
    pub(crate) struct MemoryStorage(pub std::collections::HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn saved_choice_is_restored_and_garbage_falls_back_to_dark() {
        let mut st = MemoryStorage::default();
        assert_eq!(load_choice(None), ThemeChoice::Dark);
        assert_eq!(load_choice(Some(&st)), ThemeChoice::Dark);
        for c in ThemeChoice::ALL {
            save_choice(&mut st, c);
            assert_eq!(load_choice(Some(&st)), c);
        }
        eframe::Storage::set_string(&mut st, "theme", "not-a-theme".to_owned());
        assert_eq!(load_choice(Some(&st)), ThemeChoice::Dark);
    }

    fn lum(c: Color32) -> f32 {
        let f = |u: u8| {
            let s = u as f32 / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (hi, lo) = (lum(a).max(lum(b)), lum(a).min(lum(b)));
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn hex_expands_to_rgb() {
        assert_eq!(hex(0xCBA6F7), Color32::from_rgb(0xCB, 0xA6, 0xF7));
    }

    fn both() -> [(&'static str, &'static Palette, bool); 2] {
        [("dark", &DARK, true), ("light", &LIGHT, false)]
    }

    #[test]
    fn palettes_are_catppuccin() {
        // Spot checks against the published Mocha and Latte values.
        assert_eq!(DARK.panel, hex(0x1E1E2E)); // Mocha base
        assert_eq!(DARK.chrome, hex(0x181825)); // Mocha mantle
        assert_eq!(DARK.field, hex(0x11111B)); // Mocha crust
        assert_eq!(DARK.text, hex(0xCDD6F4));
        assert_eq!(DARK.primary, hex(0xCBA6F7)); // Mocha mauve
        assert_eq!(LIGHT.panel, hex(0xEFF1F5)); // Latte base
        assert_eq!(LIGHT.chrome, hex(0xE6E9EF)); // Latte mantle
        assert_eq!(LIGHT.text, hex(0x4C4F69)); // Latte text
    }

    #[test]
    fn light_is_light_and_dark_is_dark() {
        assert!(lum(LIGHT.panel) > 0.6 && lum(LIGHT.chrome) > 0.6);
        assert!(lum(DARK.panel) < 0.05 && lum(DARK.chrome) < 0.05);
        assert!(lum(LIGHT.text) < 0.2 && lum(DARK.text) > 0.5);
    }

    #[test]
    fn visuals_use_the_palette() {
        for (name, p, dark) in both() {
            let v = visuals(p, dark);
            assert_eq!(v.dark_mode, dark, "{name}");
            assert_eq!(v.panel_fill, p.panel, "{name}");
            assert_eq!(v.extreme_bg_color, p.field, "{name}");
            assert_eq!(v.faint_bg_color, p.row_alt, "{name}");
            assert_eq!(v.selection.bg_fill, p.selection, "{name}");
            assert_eq!(v.hyperlink_color, p.link, "{name}");
            assert_eq!(v.widgets.hovered.bg_fill, p.hover, "{name}");
            assert_eq!(v.widgets.hovered.bg_stroke.color, p.focus, "{name}");
            assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::same(6));
        }
    }

    #[test]
    fn hovering_changes_colour_only_never_size() {
        for (name, p, dark) in both() {
            let w = visuals(p, dark).widgets;
            let states = [w.noninteractive, w.inactive, w.hovered, w.active, w.open];
            for ws in states {
                assert_eq!(ws.expansion, 0.0, "{name}: widgets must not grow");
                assert_eq!(ws.bg_stroke.width, 1.0, "{name}: outline width is constant");
                assert_eq!(
                    ws.fg_stroke.width, 1.0,
                    "{name}: foreground stroke is constant"
                );
            }
            assert_ne!(
                w.hovered.bg_fill, w.inactive.bg_fill,
                "{name}: hover is still visible"
            );
        }
    }

    #[test]
    fn text_is_readable_on_every_surface() {
        for (name, p, _) in both() {
            let surfaces = [p.field, p.chrome, p.panel, p.panel_raised, p.row_alt];
            let inks = [
                p.text,
                p.text_muted,
                p.primary,
                p.link,
                p.key,
                p.success,
                p.warn,
                p.danger,
            ];
            for bg in surfaces {
                for fg in inks {
                    let c = contrast(fg, bg);
                    assert!(c >= 4.5, "{name}: {fg:?} on {bg:?} = {c}");
                }
            }
            // Selected rows and hovered controls only carry body text.
            for bg in [p.selection, p.hover] {
                for fg in [p.text, p.text_muted] {
                    let c = contrast(fg, bg);
                    assert!(c >= 4.5, "{name}: {fg:?} on {bg:?} = {c}");
                }
            }
        }
    }

    #[test]
    fn outlines_and_dividers_are_visible() {
        // WCAG 1.4.11: non-text indicators need 3:1 against what they sit on.
        for (name, p, _) in both() {
            for bg in [p.panel, p.panel_raised, p.chrome, p.field] {
                let c = contrast(p.focus, bg);
                assert!(c >= 3.0, "{name}: focus on {bg:?} = {c}");
                let c = contrast(p.primary, bg);
                assert!(c >= 3.0, "{name}: primary on {bg:?} = {c}");
            }
        }
    }

    #[test]
    fn text_on_bright_fills_is_readable() {
        for (name, p, _) in both() {
            for fill in [p.primary, p.warn, p.danger, p.success, p.key] {
                let c = contrast(p.on_bright, fill);
                assert!(c >= 4.5, "{name}: {fill:?} = {c}");
            }
        }
    }

    #[test]
    fn theme_choice_round_trips() {
        for c in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::parse(c.as_str()), Some(c));
            assert!(!c.label().is_empty() && !c.icon().is_empty());
        }
        assert_eq!(ThemeChoice::parse("Solarized"), None);
        assert_eq!(ThemeChoice::parse(""), None);
        assert_eq!(
            ThemeChoice::parse("Dark"),
            None,
            "stored values are lower case"
        );
    }

    #[test]
    fn every_choice_has_a_distinct_label_and_icon() {
        let labels: std::collections::BTreeSet<_> =
            ThemeChoice::ALL.iter().map(|c| c.label()).collect();
        let icons: std::collections::BTreeSet<_> =
            ThemeChoice::ALL.iter().map(|c| c.icon()).collect();
        assert_eq!(labels.len(), ThemeChoice::ALL.len());
        assert_eq!(icons.len(), ThemeChoice::ALL.len());
    }

    #[test]
    fn both_themes_get_fonts_and_sizes() {
        let ctx = egui::Context::default();
        apply(&ctx, ThemeChoice::Light);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let mut size = 0.0;
            ctx.style_mut_of(theme, |s| size = s.spacing.interact_size.y);
            assert!(size >= 32.0, "{theme:?}");
        }
    }

    #[test]
    fn choosing_a_theme_switches_the_palette() {
        let ctx = egui::Context::default();
        apply(&ctx, ThemeChoice::Light);
        assert_eq!(palette(&ctx).panel, LIGHT.panel);
        set_choice(&ctx, ThemeChoice::Dark);
        assert_eq!(palette(&ctx).panel, DARK.panel);
    }

    #[test]
    fn controls_are_big_enough_to_hit() {
        let mut s = egui::Style::default();
        style(&mut s);
        assert!(s.spacing.interact_size.y >= 32.0);
        assert!(s.spacing.interact_size.x >= 36.0);
        assert!(s.text_styles[&TextStyle::Body].size >= 15.0);
        assert!(s.text_styles[&TextStyle::Button].size >= 15.0);
    }

    #[test]
    fn jetbrains_mono_leads_every_family() {
        let f = fonts();
        for family in [
            FontFamily::Proportional,
            FontFamily::Monospace,
            bold_family(),
        ] {
            let first = f.families[&family].first().unwrap();
            assert!(first.starts_with("jetbrains-mono"), "{family:?}: {first}");
        }
        // Icons stay available after the font swap.
        assert!(
            f.families[&FontFamily::Proportional]
                .iter()
                .any(|n| n == "phosphor")
        );
    }
}

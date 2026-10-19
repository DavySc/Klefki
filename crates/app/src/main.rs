mod activity;
mod command;
mod explain;
mod secrets;
mod shell;
mod sign_in;
mod theme;

use shell::ShellApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: viewport(),
        ..Default::default()
    };
    eframe::run_native(
        "Klefki",
        options,
        Box::new(|cc| {
            let choice = theme_flag().unwrap_or_else(|| theme::load_choice(cc.storage));
            theme::apply(&cc.egui_ctx, choice);
            let tone = theme::load_tone(cc.storage);
            theme::set_tone(&cc.egui_ctx, tone);
            let app = if demo() {
                ShellApp::new(klefki_core::api::fixture_snapshot(), choice)
            } else {
                // The engine loads the configuration and runs live status checks in the background; it wakes
                // the UI when something new is ready.
                let ctx = cc.egui_ctx.clone();
                ShellApp::with_core(
                    klefki_core::api::start(move || ctx.request_repaint()),
                    choice,
                )
            };
            let mut app = app;
            app.tone = tone;
            Ok(Box::new(app))
        }),
    )
}

/// The window, taskbar and Dock icon (`assets/icon/klefki.svg`), baked in as a 13 KB PNG. 256 px keeps the
/// macOS Dock sharp on a Retina screen; Windows scales it down to its title bar and taskbar sizes. eframe
/// already links the PNG decoder, so this adds no dependency; decoding it takes well under a millisecond.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon/klefki-256.png");

fn viewport() -> egui::ViewportBuilder {
    let builder = egui::ViewportBuilder::default()
        .with_title("Klefki")
        .with_inner_size(shell::OPENING_SIZE)
        .with_min_inner_size(shell::SMALLEST_SIZE);
    // A broken icon is not worth refusing to start over; the tests make sure it decodes.
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => builder.with_icon(icon),
        Err(_) => builder,
    }
}

/// `--demo` shows the built-in sample data instead of the real configuration.
fn demo() -> bool {
    std::env::args().any(|a| a == "--demo")
}

/// `--theme light|dark|system` overrides the remembered choice for this run.
fn theme_flag() -> Option<theme::ThemeChoice> {
    let mut args = std::env::args();
    args.find(|a| a == "--theme")?;
    args.next().and_then(|v| theme::ThemeChoice::parse(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_has_the_app_icon() {
        let icon = viewport().icon.expect("the window sets an icon");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn the_icon_is_a_tile_with_clear_corners() {
        let icon = viewport().icon.expect("the window sets an icon");
        let alpha = |x: usize, y: usize| icon.rgba[(y * icon.width as usize + x) * 4 + 3];
        let last = icon.width as usize - 1;
        // Rounded corners stay see-through so the tile sits on any taskbar; the middle is solid.
        for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
            assert_eq!(alpha(x, y), 0, "corner ({x}, {y}) is transparent");
        }
        assert_eq!(alpha(128, 128), 255);
        assert_eq!(alpha(128, 6), 255, "the tile reaches the top edge");
    }
}

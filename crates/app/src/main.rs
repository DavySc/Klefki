mod shell;
mod theme;

use shell::ShellApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klefki")
            .with_inner_size([1200.0, 720.0])
            .with_min_inner_size([800.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Klefki",
        options,
        Box::new(|cc| {
            let choice = theme_flag().unwrap_or_else(|| theme::load_choice(cc.storage));
            theme::apply(&cc.egui_ctx, choice);
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
            Ok(Box::new(app))
        }),
    )
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

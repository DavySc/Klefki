mod shell;
mod theme;

use shell::ShellApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("klefki")
            .with_inner_size([1200.0, 720.0])
            .with_min_inner_size([800.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "klefki",
        options,
        Box::new(|cc| {
            let choice = theme_flag().unwrap_or_else(|| theme::load_choice(cc.storage));
            theme::apply(&cc.egui_ctx, choice);
            Ok(Box::new(ShellApp::new(snapshot(), choice)))
        }),
    )
}

/// Real configuration by default; `--demo` shows the built-in sample data instead.
fn snapshot() -> std::sync::Arc<klefki_core::api::Snapshot> {
    if std::env::args().any(|a| a == "--demo") {
        klefki_core::api::fixture_snapshot()
    } else {
        klefki_core::api::load_snapshot()
    }
}

/// `--theme light|dark|system` overrides the remembered choice for this run.
fn theme_flag() -> Option<theme::ThemeChoice> {
    let mut args = std::env::args();
    args.find(|a| a == "--theme")?;
    args.next().and_then(|v| theme::ThemeChoice::parse(&v))
}

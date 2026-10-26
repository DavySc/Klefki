//! The Settings page.

use super::*;

/// Whether the Settings page's "Claude Code" row says `text`, beside its name.
fn settings_cli_row_says(h: &Harness<'static, ShellApp>, size: egui::Vec2, text: &str) -> bool {
    let page = egui::Rect::from_min_max(egui::pos2(220.0, 60.0), size.to_pos2());
    let row = h.get_by_label("Claude Code").rect();
    assert!(page.contains_rect(row), "{size:?}: Claude Code at {row:?}");
    h.query_all_by_label_contains(text).any(|n| {
        let r = n.rect();
        page.contains_rect(r) && r.bottom() > row.top() && r.left() > row.right()
    })
}

/// The Settings page says `text` under its Claude Code row, inside the page.
fn settings_cli_note_says(h: &Harness<'static, ShellApp>, size: egui::Vec2, text: &str) -> bool {
    let page = egui::Rect::from_min_max(egui::pos2(220.0, 60.0), size.to_pos2());
    let row = h.get_by_label("Claude Code").rect();
    h.query_all_by_label_contains(text).any(|n| {
        let r = n.rect();
        page.contains_rect(r) && r.top() >= row.bottom()
    })
}

#[test]
fn settings_names_the_claude_code_version_at_any_width() {
    // From the review of the top bar fix: in a narrow window the version showed nowhere.
    for size in [SMALLEST_SIZE, egui::vec2(900.0, 600.0), OPENING_SIZE] {
        let (mut h, engine) = live_harness();
        h.set_size(size);
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.303".to_owned(),
            )))
            .unwrap();
        h.run();
        click(&mut h, "Settings");
        assert!(settings_cli_row_says(&h, size, "2.1.303"), "{size:?}");
    }
}

#[test]
fn settings_says_when_claude_code_is_not_found_or_not_known_yet() {
    let (mut h, engine) = live_harness();
    h.set_size(OPENING_SIZE);
    click(&mut h, "Settings");
    assert!(settings_cli_row_says(&h, OPENING_SIZE, "Not known yet"));
    engine
        .events
        .send(klefki_core::api::Event::CliVersion(Some(
            "2.1.303".to_owned(),
        )))
        .unwrap();
    engine
        .events
        .send(klefki_core::api::Event::CliProblem(
            "Claude Code was not found on PATH.".to_owned(),
        ))
        .unwrap();
    h.run();
    assert!(settings_cli_row_says(&h, OPENING_SIZE, "Not found"));
    assert!(settings_cli_note_says(
        &h,
        OPENING_SIZE,
        "not found on PATH"
    ));
    assert!(!settings_cli_row_says(&h, OPENING_SIZE, "2.1.303"));
}

#[test]
fn settings_says_how_to_get_claude_code_found_and_looks_again() {
    // From the review: "Not found" named the problem but not where Klefki looked, what to do, or a way to try
    // again once it is done. The engine's reason stays, small, for the case that is not a missing program.
    for size in [OPENING_SIZE, SMALLEST_SIZE] {
        let (mut h, engine) = live_harness();
        h.set_size(size);
        click(&mut h, "Settings");
        engine
            .events
            .send(klefki_core::api::Event::CliProblem(
                "Live status is unavailable: /usr/bin/claude is not an executable file.".to_owned(),
            ))
            .unwrap();
        h.run();
        assert!(settings_cli_row_says(&h, size, "Not found"), "{size:?}");
        assert!(settings_cli_note_says(&h, size, "on your PATH"), "{size:?}");
        assert!(
            settings_cli_note_says(&h, size, "not an executable file"),
            "{size:?}"
        );
        drain_all(&engine);
        let again = h.get_by_label("Look again");
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        assert!(window.contains_rect(again.rect()), "{size:?}");
        again.click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Refresh { live_all: true }],
            "{size:?}"
        );
        // Found again: the version, and nothing to look again for.
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.303".to_owned(),
            )))
            .unwrap();
        h.run();
        assert!(settings_cli_row_says(&h, size, "2.1.303"), "{size:?}");
        assert!(h.query_by_label("Look again").is_none(), "{size:?}");
    }
}

#[test]
fn settings_switch_the_greys_and_remember_them() {
    let mut h = harness();
    h.run();
    h.get_by_label_contains("Settings").click();
    h.run();
    assert_eq!(h.state().page, Page::Settings);
    click(&mut h, "Warm gray");
    assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_DARK.panel);
    h.get_by_label_contains("Light").click();
    h.run();
    assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_LIGHT.panel);
    let mut storage = theme::tests::MemoryStorage::default();
    eframe::App::save(h.state_mut(), &mut storage);
    assert_eq!(theme::load_tone(Some(&storage)), Tone::Warm);
    assert_eq!(theme::load_choice(Some(&storage)), ThemeChoice::Light);
    click(&mut h, "Cool (Catppuccin)");
    assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
}

#[test]
fn the_palette_changes_the_greys_too() {
    let (mut h, _engine) = live_harness();
    h.run();
    ctrl_k(&mut h);
    type_into_palette(&mut h, "greys warm");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().tone, Tone::Warm);
    assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_DARK.panel);
}

#[test]
fn checking_with_claude_code_only_is_told_to_the_engine_and_remembered() {
    let (mut h, engine) = live_harness();
    h.run();
    h.state_mut().go(Page::Settings);
    h.run();
    drain(&engine);
    h.get_by_label("Check servers with Claude Code only")
        .click();
    h.run();
    assert_eq!(drain(&engine), [Command::CheckWithClaudeOnly(true)]);
    assert!(h.state().claude_only);
    let mut storage = theme::tests::MemoryStorage::default();
    eframe::App::save(h.state_mut(), &mut storage);
    assert!(crate::shell::load_claude_only(Some(&storage)));
    h.get_by_label("Check servers with Claude Code only")
        .click();
    h.run();
    assert_eq!(drain(&engine), [Command::CheckWithClaudeOnly(false)]);
    eframe::App::save(h.state_mut(), &mut storage);
    assert!(!crate::shell::load_claude_only(Some(&storage)));
    // Never saved: off.
    assert!(!crate::shell::load_claude_only(None));
}

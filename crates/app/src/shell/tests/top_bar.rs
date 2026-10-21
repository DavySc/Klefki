//! The top bar, and Klefki's own updates (decision 009).

use super::*;
use klefki_core::api::UpdateCheck;

#[test]
fn the_top_bar_never_draws_one_thing_over_another() {
    // Found in the UI review: at the smallest size the page's title ran into "Claude CLI 2.1.303". Every width
    // from the smallest to the one Klefki opens at, and the longest version the engine passes on (32 characters).
    use egui::accesskit::Role;
    let widths = (SMALLEST_SIZE.x as usize..=OPENING_SIZE.x as usize).step_by(50);
    for size in widths.map(|w| egui::vec2(w as f32, SMALLEST_SIZE.y)) {
        for version in ["2.1.303", "10.200.3000-beta.4+build.2026101"] {
            let (mut h, engine) = live_harness();
            h.set_size(size);
            engine
                .events
                .send(klefki_core::api::Event::CliVersion(Some(
                    version.to_owned(),
                )))
                .unwrap();
            h.run();
            let row = h.get_by_label("Klefki").rect();
            let items: Vec<(String, egui::Rect)> = h
                .query_all_by(|n| {
                    matches!(
                        n.role(),
                        Role::Label | Role::Button | Role::TextInput | Role::ComboBox
                    )
                })
                .filter(|n| n.rect().top() < row.bottom() && n.rect().bottom() > row.top())
                .map(|n| (format!("{n:?}"), n.rect()))
                .collect();
            assert!(items.len() >= 6, "{size:?}: {items:#?}");
            for (i, (a, ra)) in items.iter().enumerate() {
                for (b, rb) in &items[i + 1..] {
                    let both = ra.intersect(*rb);
                    assert!(
                        both.width() <= 0.0 || both.height() <= 0.0,
                        "{size:?}: {a} at {ra:?} is drawn over {b} at {rb:?}"
                    );
                }
            }
        }
    }
    // Where there is room, it is there.
    let (mut h, engine) = live_harness();
    h.set_size(OPENING_SIZE);
    engine
        .events
        .send(klefki_core::api::Event::CliVersion(Some(
            "2.1.303".to_owned(),
        )))
        .unwrap();
    h.run();
    h.get_by_label("Claude CLI 2.1.303");
    // Where the whole of it does not fit, the number alone, which says what it is on hover. Where that starts
    // depends on the platform's shortcut labels, so it is looked for across the widths.
    let mut number_only = 0;
    for width in (SMALLEST_SIZE.x as usize..=OPENING_SIZE.x as usize).step_by(10) {
        let (mut h, engine) = live_harness();
        h.set_size(egui::vec2(width as f32, SMALLEST_SIZE.y));
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.303".to_owned(),
            )))
            .unwrap();
        h.run();
        if h.query_by_label("Claude CLI 2.1.303").is_none() && h.query_by_label("2.1.303").is_some()
        {
            number_only += 1;
            h.get_by_label("2.1.303").hover();
            h.run();
            // By the name it gives itself, as Settings does.
            h.get_by_label("Claude Code 2.1.303");
        }
    }
    assert!(number_only > 0, "the number alone was never shown");
}

fn update_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness_at_60fps();
    h.state_mut().checks_for_updates = true;
    (h, engine)
}

fn update_checks_sent(engine: &klefki_core::api::Detached) -> usize {
    drain_all(engine)
        .into_iter()
        .filter(|c| matches!(c, Command::CheckForUpdates))
        .count()
}

#[test]
fn klefki_checks_for_updates_once_after_starting_not_while_starting() {
    let (mut h, engine) = update_harness();
    h.run();
    assert_eq!(update_checks_sent(&engine), 0, "not while starting");
    // Nothing else asks for frames here: the check must wake the window by itself.
    let frames = (crate::updates::START_DELAY * 60.0) as usize + 30;
    for _ in 0..frames {
        h.step();
    }
    assert!(h.ctx.input(|i| i.time) >= crate::updates::START_DELAY);
    assert_eq!(update_checks_sent(&engine), 1);
    for _ in 0..120 {
        h.step();
    }
    assert_eq!(update_checks_sent(&engine), 0, "once a run");
}

#[test]
fn a_build_without_a_release_key_never_checks_and_says_so() {
    let (mut h, engine) = live_harness_at_60fps();
    for _ in 0..((crate::updates::START_DELAY * 60.0) as usize + 30) {
        h.step();
    }
    assert_eq!(update_checks_sent(&engine), 0);
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label_contains("This build does not check for updates");
    assert!(h.query_by_label("Check now").is_none());
}

#[test]
fn a_found_update_shows_in_the_top_bar_and_opens_its_page() {
    let (mut h, engine) = update_harness();
    h.state_mut().updated_by_hand = Some("Klefki was installed from the .deb package.".into());
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    engine
        .events
        .send(Event::Update(UpdateCheck::Available {
            version,
            page: "https://github.com/DavySc/Klefki/releases/tag/v999.0.0".into(),
        }))
        .unwrap();
    h.run();
    drain_all(&engine);
    h.get_by_label_contains("Update: 999.0.0").click();
    h.run();
    assert!(drain_all(&engine).contains(&Command::OpenReleasePage(version)));
    // Settings says the same, with the same way to it.
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label("999.0.0 is available");
    h.get_by_label("Klefki was installed from the .deb package.");
    click(&mut h, "What's new");
    let sent = drain_all(&engine);
    assert!(sent.contains(&Command::OpenReleasePage(version)));
    assert!(
        !sent.contains(&Command::DownloadUpdate),
        "an install updated by hand downloads nothing"
    );
    assert!(h.query_by_label("Download").is_none());
}

fn found(engine: &klefki_core::api::Detached, version: klefki_core::api::Version) {
    engine
        .events
        .send(Event::Update(UpdateCheck::Available {
            version,
            page: format!("https://github.com/DavySc/Klefki/releases/tag/v{version}"),
        }))
        .unwrap();
}

fn downloads_sent(engine: &klefki_core::api::Detached) -> usize {
    drain_all(engine)
        .into_iter()
        .filter(|c| matches!(c, Command::DownloadUpdate))
        .count()
}

#[test]
fn a_found_update_downloads_by_itself_then_offers_a_restart() {
    let (mut h, engine) = update_harness();
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    found(&engine, version);
    h.run_steps(2);
    assert_eq!(downloads_sent(&engine), 1);
    h.get_by_label_contains("Downloading 999.0.0");
    engine
        .events
        .send(Event::UpdateDownload(UpdateDownload::Ready(version)))
        .unwrap();
    h.run();
    h.get_by_label_contains("Restart to update");
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label("999.0.0 is ready: it installs when Klefki closes");
    // Restarting installs it; the stand-in engine has nothing downloaded, so this says why it could not.
    click(&mut h, "Restart now");
    h.get_by_label("No update has been downloaded.");
    h.get_by_label_contains("Update: 999.0.0");
}

#[test]
fn with_automatic_updates_off_the_download_waits_for_a_click() {
    let (mut h, engine) = update_harness();
    h.state_mut().updates.auto = false;
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    found(&engine, version);
    h.run();
    assert_eq!(downloads_sent(&engine), 0);
    h.get_by_label_contains("Update: 999.0.0").click();
    h.run_steps(2);
    assert_eq!(downloads_sent(&engine), 1);
    engine
        .events
        .send(Event::UpdateDownload(UpdateDownload::Failed(
            "Could not download the update: no connection.".into(),
        )))
        .unwrap();
    h.run();
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label("Could not download the update: no connection.");
    h.get_by_label("Download").click();
    h.run_steps(2);
    assert_eq!(downloads_sent(&engine), 1, "tried again on request");
}

#[test]
fn restarting_to_update_waits_until_the_open_change_is_done() {
    let (mut h, engine) = update_harness();
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    found(&engine, version);
    engine
        .events
        .send(Event::UpdateDownload(UpdateDownload::Ready(version)))
        .unwrap();
    h.run();
    h.state_mut().applying = true;
    h.run();
    assert!(
        h.get_by_label_contains("Restart to update")
            .accesskit_node()
            .is_disabled()
    );
    h.state_mut().applying = false;
    h.run();
    assert!(
        !h.get_by_label_contains("Restart to update")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn settings_check_now_shows_progress_then_the_answer() {
    let (mut h, engine) = update_harness();
    h.state_mut().updates.auto = false;
    h.state_mut().go(Page::Settings);
    h.run();
    assert!(
        h.query_by_label_contains("Update: 999").is_none(),
        "no update shown before one is found"
    );
    h.get_by_label("Check now").click();
    // The spinner asks for frames until the answer comes.
    h.run_steps(2);
    assert_eq!(update_checks_sent(&engine), 1);
    h.get_by_label("Checking…");
    engine
        .events
        .send(Event::Update(UpdateCheck::Failed(
            "Could not check for updates: no connection.".into(),
        )))
        .unwrap();
    h.run();
    h.get_by_label("Could not check for updates: no connection.");
    h.get_by_label("Check now").click();
    h.run_steps(2);
    engine
        .events
        .send(Event::Update(UpdateCheck::UpToDate))
        .unwrap();
    h.run();
    h.get_by_label("Up to date · checked just now");
    assert!(h.query_by_label_contains("Could not check").is_none());
    h.get_by_label("Check now").click();
    h.run_steps(2);
    engine
        .events
        .send(Event::Update(UpdateCheck::NoRelease))
        .unwrap();
    h.run();
    h.get_by_label("No signed release is published yet");
}

#[test]
fn the_switch_says_whether_it_updates_or_only_checks() {
    let (mut h, _engine) = update_harness();
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label("Update Klefki automatically");
    h.state_mut().updated_by_hand = Some("Klefki was installed with the MSI.".into());
    h.run();
    h.get_by_label("Check for updates automatically");
}

#[test]
fn an_app_that_cannot_replace_itself_says_why_and_offers_the_page() {
    let (mut h, engine) = update_harness();
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    found(&engine, version);
    h.run_steps(2);
    assert_eq!(downloads_sent(&engine), 1);
    let why =
        "Klefki cannot replace itself in this folder. Move Klefki to a folder you can change.";
    engine
        .events
        .send(Event::UpdateDownload(UpdateDownload::Manual(why.into())))
        .unwrap();
    h.run();
    h.get_by_label_contains("Update: 999.0.0").click();
    h.run();
    assert!(drain_all(&engine).contains(&Command::OpenReleasePage(version)));
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label(why);
}

#[test]
fn an_update_that_failed_at_close_is_shown_and_not_downloaded_again_unasked() {
    let (mut h, engine) = update_harness();
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    let why = "The update could not be installed when Klefki closed: denied.";
    engine
        .events
        .send(Event::UpdateDownload(UpdateDownload::Failed(why.into())))
        .unwrap();
    found(&engine, version);
    h.run();
    assert_eq!(downloads_sent(&engine), 0);
    h.state_mut().go(Page::Settings);
    h.run();
    h.get_by_label(why);
    h.get_by_label("Download").click();
    h.run_steps(2);
    assert_eq!(downloads_sent(&engine), 1, "on request");
}

#[test]
fn after_an_update_klefki_says_so_once() {
    let (mut h, engine) = update_harness();
    let version = klefki_core::api::Version::current();
    h.state_mut().updates.updated = Some(version);
    h.run();
    h.get_by_label_contains(&format!("Updated to {version}"))
        .click();
    h.run();
    assert!(drain_all(&engine).contains(&Command::OpenReleasePage(version)));
    assert!(h.query_by_label_contains("Updated to").is_none());
}

#[test]
fn restarting_waits_for_every_open_dialog() {
    let (mut h, engine) = update_harness();
    h.run();
    let version = klefki_core::api::Version::parse("999.0.0").unwrap();
    found(&engine, version);
    engine
        .events
        .send(Event::UpdateDownload(UpdateDownload::Ready(version)))
        .unwrap();
    h.run();
    h.state_mut().forgetting = Some("x".into());
    h.run();
    assert!(
        h.get_by_label_contains("Restart to update")
            .accesskit_node()
            .is_disabled()
    );
    h.state_mut().forgetting = None;
    h.state_mut().import_waiting = true;
    h.run_steps(2);
    assert!(
        h.get_by_label_contains("Restart to update")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn turning_automatic_checks_off_is_remembered() {
    let (mut h, _engine) = update_harness();
    h.state_mut().go(Page::Settings);
    h.run();
    click(&mut h, "Update Klefki automatically");
    assert!(!h.state().updates.auto);
    let mut storage = crate::theme::tests::MemoryStorage::default();
    eframe::App::save(h.state_mut(), &mut storage);
    assert!(!crate::updates::Updates::load(Some(&storage)).auto);
}

#[test]
fn unknown_cli_version_is_not_shown_as_a_claim() {
    let mut h = harness_with(Snapshot::default());
    h.run();
    assert!(h.query_by_label_contains("Claude CLI").is_none());
}

#[test]
fn theme_dropdown_offers_every_theme_and_switches_the_palette() {
    use ThemeChoice::{Dark, Light, System};
    let shown = |o: ThemeOption| format!("{} {}", o.icon(), o.label());
    let mut h = harness();
    h.run();
    assert_eq!(theme::palette(&h.ctx).panel, theme::DARK.panel);
    let cool_dark = ThemeOption::new(Dark, Tone::Cool);

    // Closed, the dropdown shows the current theme only (its value, in words: the icon is drawn but not read).
    h.get_by_value(cool_dark.label());
    assert!(h.query_by_label_contains("Light").is_none());
    assert!(h.query_by_label_contains("System").is_none());

    // Open, every theme is offered, each with its own icon, and the pick closes the list.
    h.get_by_value(cool_dark.label()).click();
    h.run();
    for o in ThemeOption::ALL {
        h.query_by_label_contains(&shown(o))
            .unwrap_or_else(|| panic!("{} is not offered", o.label()));
    }
    h.get_by_label(&shown(ThemeOption::new(Light, Tone::Cool)))
        .click();
    h.run();
    assert_eq!(h.state().theme, Light);
    assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
    assert!(
        h.query_by_label_contains("Dark (warm)").is_none(),
        "the list stays open after a pick"
    );

    // A warm one sets the greys too.
    let warm_light = ThemeOption::new(Light, Tone::Warm);
    h.get_by_value(ThemeOption::new(Light, Tone::Cool).label())
        .click();
    h.run();
    h.get_by_label_contains(&shown(warm_light)).click();
    h.run();
    assert_eq!((h.state().theme, h.state().tone), (Light, Tone::Warm));
    assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_LIGHT.panel);

    // System with warm greys is one pick, and the closed dropdown says so.
    h.get_by_value(warm_light.label()).click();
    h.run();
    let system_warm = ThemeOption::new(System, Tone::Warm);
    h.get_by_label_contains(&shown(system_warm)).click();
    h.run();
    assert_eq!((h.state().theme, h.state().tone), (System, Tone::Warm));
    h.get_by_value(system_warm.label());

    // ... and back to cool greys in one pick.
    h.get_by_value(system_warm.label()).click();
    h.run();
    h.get_by_label_contains(&shown(ThemeOption::new(System, Tone::Cool)))
        .click();
    h.run();
    assert_eq!((h.state().theme, h.state().tone), (System, Tone::Cool));
}

#[test]
fn only_the_current_theme_has_a_check_mark() {
    let current = ThemeOption::new(ThemeChoice::Dark, Tone::Cool);
    let mut h = harness();
    h.run();
    h.get_by_value(current.label()).click();
    h.run();
    let checked = format!("{} {}  {}", current.icon(), current.label(), icon::CHECK);
    h.get_by_label(&checked);
    assert_eq!(
        ThemeOption::ALL
            .iter()
            .filter(|o| h
                .query_by_label_contains(&format!("{}  {}", o.label(), icon::CHECK))
                .is_some())
            .count(),
        1
    );
}

#[test]
fn the_theme_choice_is_saved() {
    use eframe::App as _;
    let mut storage = crate::theme::tests::MemoryStorage::default();
    let mut app = ShellApp::new(fixture_snapshot(), ThemeChoice::Light);
    app.save(&mut storage);
    assert_eq!(storage.0.get("theme").map(String::as_str), Some("light"));
}

#[test]
fn dropdown_options_do_not_change_size_on_hover() {
    let dark = ThemeOption::new(ThemeChoice::Dark, Tone::Cool);
    let mut h = harness();
    h.run();
    h.get_by_value(dark.label()).click();
    h.run();
    for option in ["System (cool)", "Light (cool)", "Dark (warm)"] {
        let before = h.get_by_label_contains(option).rect();
        h.get_by_label_contains(option).hover();
        h.run();
        let after = h.get_by_label_contains(option).rect();
        assert_eq!(before, after, "option {option} moved or resized on hover");
    }
}

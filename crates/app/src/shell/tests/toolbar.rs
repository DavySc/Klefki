//! The toolbar, and the names every control has in words.

use super::*;
use crate::theme::ThemeOption;

#[test]
fn nav_and_toolbar_targets_are_large() {
    let mut h = harness();
    h.run();
    let nav = h.get_by_label_contains("Secrets").rect();
    assert!(nav.height() >= 40.0, "nav item {nav:?}");
    // (Was the Add button; the toolbar now shows only actions that work, so the check uses Edit.)
    let add = tool(&h, Tool::Edit).rect();
    assert!(
        add.width() >= 40.0 && add.height() >= 34.0,
        "toolbar button {add:?}"
    );
}

#[test]
fn each_toolbar_button_says_on_hover_what_it_does_and_its_shortcut() {
    // Written out here, not taken from `Tool::tip`: the hover text is what people read.
    for (which, tip) in [
        (Tool::Add, "Add (Ctrl+N)"),
        (Tool::Import, "Import from a file (Ctrl+O)"),
        (Tool::Edit, "Edit (Ctrl+E)"),
        (Tool::Duplicate, "Duplicate (Ctrl+D)"),
        (Tool::Move, "Move (Ctrl+Shift+M)"),
        (Tool::SignIn, "Sign in"),
        (Tool::TurnOn, "Turn on"),
        (Tool::TurnOff, "Turn off"),
        (Tool::Delete, "Delete (Del)"),
        (Tool::Export, "Export the servers in the list"),
    ] {
        let (mut h, engine) = live_harness();
        show_rows(&mut h, &engine, vec![live_row("alpha", Scope::User, None)]);
        tool(&h, which).hover();
        h.run();
        // As it is, or with why it is off: "Edit (Ctrl+E): select one server".
        let with_reason = format!("{tip}: ");
        let shown =
            h.query_all_by_label(tip).count() + h.query_all_by_label_contains(&with_reason).count();
        assert_eq!(shown, 1, "{which:?}: \"{tip}\" on hover");
        // Said by a screen reader as the button's description, without hovering.
        let description = tool(&h, which).accesskit_node().description();
        assert!(
            description
                .as_deref()
                .is_some_and(|d| d == tip || d.starts_with(&with_reason)),
            "{which:?}: {description:?}"
        );
    }
}

#[test]
fn the_toolbar_shows_only_what_works() {
    let (mut h, _engine) = live_harness();
    h.run();
    // Duplicate, Add and Sign in were in this list until they were built; they are shown now.
    tool(&h, Tool::Duplicate);
    tool(&h, Tool::Add);
    tool(&h, Tool::SignIn);
    // And Export, since M3.
    tool(&h, Tool::Export);
    // The buttons once shown for what did not work yet. (Any button shown by its icon alone fails
    // `every_control_is_named_in_words`.)
    for gone in [icon::SWAP, icon::DOWNLOAD_SIMPLE] {
        assert!(!Tool::ALL.iter().any(|t| t.glyph() == gone), "{gone:?}");
        assert!(h.query_by_label(gone).is_none(), "{gone:?}");
    }
    assert!(
        h.query_by_label_contains("selected").is_none(),
        "no count without a selection"
    );
}

/// The controls on screen whose name has no word in it: an icon (a private-use character), a hint shown in the
/// box or a tooltip is no name to a screen reader.
fn unnamed_controls(h: &Harness<'static, ShellApp>) -> Vec<String> {
    use egui::accesskit::Role;
    h.query_all_by(|n| {
        matches!(
            n.role(),
            Role::Button | Role::ComboBox | Role::TextInput | Role::CheckBox | Role::RadioButton
        ) && !n
            .label()
            .is_some_and(|l| l.chars().any(char::is_alphabetic))
    })
    .map(|n| {
        format!(
            "{:?} {:?}",
            n.accesskit_node().role(),
            n.accesskit_node().label()
        )
    })
    .collect()
}

#[test]
fn every_control_is_named_in_words() {
    // WCAG 2.2 SC 4.1.2 Name, Role, Value: the toolbar, Refresh, the theme menu and the filter box were read out
    // as private-use characters or not at all.
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(fixture_snapshot()))
        .unwrap();
    h.run();
    click(&mut h, "issue-tracker");
    assert_eq!(
        unnamed_controls(&h),
        Vec::<String>::new(),
        "{:?}",
        h.state().page
    );
    for (page, _, _) in Page::ALL {
        h.state_mut().go(page);
        h.run();
        assert_eq!(unnamed_controls(&h), Vec::<String>::new(), "{page:?}");
    }
}

#[test]
fn the_top_bar_and_the_toolbar_are_named_by_what_they_do() {
    use egui::accesskit::Role;
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(fixture_snapshot()))
        .unwrap();
    h.run();
    h.get_by_role_and_label(Role::Button, "Refresh and re-check all");
    h.get_by_role_and_label(Role::ComboBox, "Theme");
    h.get_by_role_and_label(Role::Button, &format!("Commands ({})", shortcut("K")));
    h.get_by_role_and_label(Role::TextInput, "Filter servers");
    for tool in Tool::ALL {
        h.get_by_role_and_label(Role::Button, tool.name());
    }
    // The theme menu says which theme is on, in words.
    let current = ThemeOption::new(h.state().theme, h.state().tone);
    assert_eq!(
        h.get_by_role_and_label(Role::ComboBox, "Theme")
            .accesskit_node()
            .value()
            .as_deref(),
        Some(current.label())
    );
    // The filter box is named for what it filters on each page.
    for (page, name) in [
        (Page::ActivityLog, "Filter changes"),
        (Page::Secrets, "Filter secrets"),
        (Page::Desktop, "Filter servers"),
        (Page::Profiles, "Filter profiles"),
    ] {
        h.state_mut().go(page);
        h.run();
        h.get_by_role_and_label(Role::TextInput, name);
    }
}

#[test]
fn every_toolbar_and_nav_icon_is_drawn_by_the_icon_font() {
    // Found trying Move in the real app: the arrows icon is at U+E0A0, where JetBrains Mono, first in every
    // font family, has its own branch symbol, so the button showed a branch. These are the private-use
    // characters JetBrains Mono draws itself (its Powerline symbols).
    const MONO_SYMBOLS: [std::ops::RangeInclusive<char>; 2] =
        ['\u{E0A0}'..='\u{E0A2}', '\u{E0B0}'..='\u{E0B3}'];
    let glyphs = Tool::ALL
        .iter()
        .map(|t| t.glyph())
        .chain(Page::ALL.iter().map(|(_, g, _)| *g));
    for glyph in glyphs {
        for c in glyph.chars() {
            assert!(
                !MONO_SYMBOLS.iter().any(|r| r.contains(&c)),
                "U+{:04X} is drawn by JetBrains Mono, not the icon font",
                c as u32
            );
        }
    }
    // And the near variant: the icon once used for Move.
    assert!(
        MONO_SYMBOLS
            .iter()
            .any(|r| r.contains(&icon::ARROWS_LEFT_RIGHT.chars().next().unwrap()))
    );
}

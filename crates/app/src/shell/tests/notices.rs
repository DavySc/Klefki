//! The notices above the server list: the old switch-off list, and servers Klefki kept.

use super::*;

// ---- the old switch-off list

fn legacy(project: &str, names: &[&str], turns_off: &[&str], malformed: bool) -> LegacyDisabled {
    LegacyDisabled {
        project: project.to_owned(),
        shown: project.to_owned(),
        names: names.iter().map(|n| (*n).to_owned()).collect(),
        turns_off: turns_off.iter().map(|n| (*n).to_owned()).collect(),
        malformed,
    }
}

fn legacy_harness(
    lists: Vec<LegacyDisabled>,
) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: vec![live_row("gw", Scope::User, None)],
            projects: lists.iter().map(|l| l.project.clone()).collect(),
            legacy_disabled: lists,
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    (h, engine)
}

#[test]
fn no_old_list_shows_no_notice() {
    let (h, _engine) = legacy_harness(vec![]);
    assert!(h.query_by_label_contains("old switch-off list").is_none());
    assert!(h.query_by_label("Migrate…").is_none());
}

#[test]
fn the_old_list_is_shown_with_what_is_on_because_of_it() {
    let (h, _engine) = legacy_harness(vec![
        legacy("C:/Repos/WebApp", &["gw", "old"], &["gw"], false),
        legacy("/home/x/api", &["a"], &[], false),
        legacy("/srv/empty", &[], &[], false),
    ]);
    h.get_by_label_contains("Old switch-off lists in 3 projects");
    h.get_by_label_contains("empty: an empty list");
    h.get_by_label_contains("Claude Code no longer reads");
    h.get_by_label_contains("WebApp: gw, old (gw is on)");
    h.get_by_label_contains("api: a (already off)");
}

#[test]
fn migrating_asks_for_one_plan_covering_every_list_that_can_move() {
    let (mut h, engine) = legacy_harness(vec![
        legacy("C:/Repos/WebApp", &["gw", "x"], &["gw", "x"], false),
        legacy("/bad", &["a"], &["a"], true),
        legacy("/home/x/api", &["a"], &[], false),
    ]);
    h.get_by_label_contains("bad: holds something other than server names");
    click(&mut h, "Migrate…");
    let changes = duplicate_requested(&engine);
    assert_eq!(
        changes,
        vec![
            Change::MigrateLegacyDisabled {
                project: "C:/Repos/WebApp".into()
            },
            Change::MigrateLegacyDisabled {
                project: "/home/x/api".into()
            },
        ]
    );
}

#[test]
fn the_notice_is_built_once_per_snapshot() {
    // Review of #16: its lines were formatted every frame.
    let (mut h, engine) = legacy_harness(vec![legacy("/p", &["gw"], &["gw"], false)]);
    let first = h.state().legacy_cache.as_ref().unwrap().1.clone();
    h.run();
    assert!(Arc::ptr_eq(
        &first,
        &h.state().legacy_cache.as_ref().unwrap().1
    ));
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            legacy_disabled: vec![legacy("/q", &["x"], &[], false)],
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    let now = &h.state().legacy_cache.as_ref().unwrap().1;
    assert!(!Arc::ptr_eq(&first, now));
    h.get_by_label_contains("q: x (already off)");
}

#[test]
fn only_malformed_lists_cannot_be_migrated() {
    let (h, _engine) = legacy_harness(vec![legacy("/bad", &["a"], &["a"], true)]);
    assert!(h.get_by_label("Migrate…").accesskit_node().is_disabled());
}

#[test]
fn the_palette_offers_the_migration() {
    let (mut h, engine) = legacy_harness(vec![legacy("/p", &["gw"], &["gw"], false)]);
    let titles: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
    assert!(
        titles.iter().any(|t| t == "Migrate old switch-off lists"),
        "{titles:?}"
    );
    h.state_mut()
        .run(&egui::Context::default(), command::Action::MigrateLegacy);
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::MigrateLegacyDisabled {
            project: "/p".into()
        }]
    );
    h.run();
}

// ---- kept

fn kept_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a],
            kept: vec![klefki_core::api::Kept {
                id: "1760000000000-0".into(),
                name: "sso".into(),
                place: "in WebApp".into(),
                signs_in: true,
                back: false,
            }],
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    (h, engine)
}

#[test]
fn a_server_that_could_not_be_put_back_is_shown_and_can_be_put_back() {
    let (mut h, engine) = kept_harness();
    h.get_by_label("A server is out of the configuration");
    h.get_by_label("sso in WebApp (signs in with OAuth)");
    click(&mut h, "Put back");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::PutBack {
            kept: "1760000000000-0".into()
        }]
    );
}

#[test]
fn one_kept_server_is_spoken_of_as_one() {
    let (h, _engine) = kept_harness();
    h.get_by_label_contains("A change took it out and Claude Code could not put it back.");
    assert!(h.query_by_label_contains("took them out").is_none());
}

#[test]
fn a_server_back_without_some_settings_offers_only_forget() {
    // Cross-review round 2: the notice said it was out of the configuration while it was in the list, and Put
    // back failed with "already exists there".
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a],
            kept: vec![klefki_core::api::Kept {
                id: "1760000000000-0".into(),
                name: "sso".into(),
                place: "in WebApp".into(),
                signs_in: true,
                back: true,
            }],
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    h.get_by_label("Klefki keeps settings Claude Code left out");
    h.get_by_label("sso in WebApp is back without some settings");
    assert!(h.query_by_label("Put back").is_none());
    assert!(
        h.query_by_label_contains("out of the configuration")
            .is_none()
    );
    // Forgetting it says what was let go of.
    click(&mut h, "Forget\u{2026}");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::ForgetKept {
            kept: "1760000000000-0".into()
        }]
    );
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["- sso"], &[], false)))
        .unwrap();
    h.run();
    engine
        .events
        .send(Event::Applied {
            plan: 7,
            changed: true,
            backup: None,
            notes: Vec::new(),
        })
        .unwrap();
    h.run();
    h.get_by_label("Klefki no longer keeps the definition of sso in WebApp.");
}

#[test]
fn a_kept_server_can_be_forgotten_after_a_review() {
    let (mut h, engine) = kept_harness();
    click(&mut h, "Forget\u{2026}");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::ForgetKept {
            kept: "1760000000000-0".into()
        }]
    );
}

#[test]
fn nothing_kept_shows_no_notice() {
    let (h, _engine) = duplicate_harness();
    assert!(
        h.query_by_label_contains("out of the configuration")
            .is_none()
    );
}

#[test]
fn a_copy_is_selected_and_the_message_says_copied() {
    let (mut h, engine) = duplicate_harness();
    open_duplicate_of(&mut h, "beta");
    click(&mut h, "Review");
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ beta_copy"], &[], false)))
        .unwrap();
    h.run();
    let mut rows = h.state().snapshot.servers.clone();
    rows.push(live_row("beta_copy", Scope::Local, Some("C:/Repos/WebApp")));
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: rows,
            projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
            ..Snapshot::default()
        })))
        .unwrap();
    engine
        .events
        .send(Event::Applied {
            plan: 7,
            changed: true,
            backup: None,
            notes: Vec::new(),
        })
        .unwrap();
    h.run();
    assert_eq!(h.state().selected, BTreeSet::from([3]));
    h.get_by_label("Copied beta_copy in WebApp.");
}

//! Move: where a server can go, and what the change says while and after it runs.

use super::*;

#[test]
fn move_sends_a_local_server_to_the_user_and_cannot_choose_where_it_is_now() {
    let (mut h, engine) = duplicate_harness();
    open_move_of(&mut h, "beta");
    // Nothing is chosen yet, and where it is now cannot be.
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    h.get_by_label_contains("Choose where it goes.");
    let here = h.get_by_label("WebApp");
    assert!(here.accesskit_node().is_disabled());
    here.click();
    h.run();
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    click(&mut h, "For the user, in every project");
    h.get_by_label("Moves beta to the user.");
    h.get_by_label_contains("keeps its settings");
    click(&mut h, "Review");
    assert!(h.state().moving.is_none());
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Move {
            id: h.state().snapshot.servers[1].id(),
            to: Target::User,
        }]
    );
}

#[test]
fn move_chooses_one_place_at_a_time() {
    use egui::accesskit::Role;
    let (mut h, engine) = duplicate_harness();
    open_move_of(&mut h, "alpha");
    assert!(
        h.get_by_label("For the user, in every project")
            .accesskit_node()
            .is_disabled()
    );
    click(&mut h, "notes");
    click(&mut h, "WebApp");
    // Clicking the chosen one again keeps it chosen.
    click(&mut h, "WebApp");
    let chosen: Vec<String> = h
        .get_all_by_role(Role::RadioButton)
        .filter(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(chosen, vec!["WebApp".to_owned()]);
    h.get_by_label("Moves alpha to WebApp.");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Move {
            id: h.state().snapshot.servers[0].id(),
            to: Target::Local("C:/Repos/WebApp".into()),
        }]
    );
}

#[test]
fn move_will_not_go_where_the_name_is_taken() {
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    // Another alpha already in notes, written with another spelling of its path.
    let there = live_row("alpha", Scope::Local, Some(r"/home/me/notes/"));
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a, there],
            projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    h.state_mut().selected = BTreeSet::from([0]);
    h.run();
    press(&mut h, Tool::Move);
    click(&mut h, "notes");
    h.get_by_label_contains("alpha already exists in notes");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(duplicate_requested(&engine).is_empty());
    // Somewhere free is fine.
    click(&mut h, "WebApp");
    assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
}

#[test]
fn move_is_in_the_palette_for_user_and_local_servers_only() {
    let (mut h, _engine) = duplicate_harness();
    click(&mut h, "gamma");
    assert!(tool(&h, Tool::Move).accesskit_node().is_disabled());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run();
    assert!(h.query_by_label_contains("Move gamma").is_none());
    h.key_press(egui::Key::Escape);
    h.run();
    click(&mut h, "beta");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run();
    h.get_by_label_contains("Move beta").click();
    h.run();
    assert!(h.state().moving.is_some());
    h.get_by_label("Move beta");
}

#[test]
fn a_moved_server_is_selected_where_it_went_and_the_message_says_moved() {
    let (mut h, engine) = duplicate_harness();
    open_move_of(&mut h, "beta");
    click(&mut h, "notes");
    click(&mut h, "Review");
    engine
        .events
        .send(Event::PlanReady(plan_view(
            7,
            &["- beta", "+ beta"],
            &[],
            false,
        )))
        .unwrap();
    h.run();
    let (a, _) = two_rows();
    let moved = live_row("beta", Scope::Local, Some("/home/me/notes"));
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a, moved],
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
            notes: vec!["Claude Code left out alwaysAllow.".into()],
        })
        .unwrap();
    h.run();
    assert_eq!(h.state().selected, BTreeSet::from([1]));
    h.get_by_label("Moved beta to notes. Claude Code left out alwaysAllow.");
    // Something was left out: said as a warning, which stays until it is closed.
    assert!(h.state().outcome.as_ref().is_some_and(|o| o.warn));
    // Undo moves it back where it was.
    drain(&engine);
    click(&mut h, "Undo");
    assert_eq!(
        duplicate_requested(&engine),
        [Change::Move {
            id: ServerId::new(Scope::Local, Some("/home/me/notes"), "beta"),
            to: Target::Local("C:/Repos/WebApp".into()),
        }]
    );
}

#[test]
fn a_change_made_with_claude_code_says_each_step_while_it_runs() {
    let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
    click(&mut h, "Apply");
    h.get_by_label("Saving…");
    // A step of another plan is not this one's.
    for (plan, step) in [
        (8, "Claude Code is adding beta for the user…"),
        (7, "Claude Code is removing alpha for the user…"),
    ] {
        engine
            .events
            .send(Event::Applying {
                plan,
                step: step.into(),
            })
            .unwrap();
    }
    h.run();
    assert!(h.query_by_label("Saving…").is_none());
    assert!(h.query_by_label_contains("adding beta").is_none());
    h.get_by_label("Claude Code is removing alpha for the user…");
    h.get_by_label("Keep Klefki open until this is done.");
    drain(&engine);
}

#[test]
fn a_server_that_signs_in_says_it_signs_in_again_after_a_move_and_offers_it() {
    let (mut h, engine) = live_harness();
    let sso = ServerRow {
        oauth: Some(OAuthInfo {
            client_id: None,
            scopes: vec![],
        }),
        ..web_row("sso")
    };
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: vec![sso.clone()],
            projects: vec!["C:/Repos/WebApp".into()],
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    open_move_of(&mut h, "sso");
    h.get_by_label_contains("so you will need to sign in to it again afterwards");
    click(&mut h, "WebApp");
    click(&mut h, "Review");
    engine
        .events
        .send(Event::PlanReady(plan_view(
            7,
            &["- sso", "+ sso"],
            &[],
            false,
        )))
        .unwrap();
    h.run();
    let moved = ServerRow {
        connection: klefki_core::api::ConnectionId::new(
            Scope::Local,
            Some("C:/Repos/WebApp"),
            "sso",
        ),
        scope: Scope::Local,
        project: Some("C:/Repos/WebApp".into()),
        ..sso
    };
    engine
        .events
        .send(Event::Snapshot(Arc::new(Snapshot {
            servers: vec![moved],
            projects: vec!["C:/Repos/WebApp".into()],
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
    h.get_by_label("Moved sso to WebApp. Sign in now, or later from its Status tab.");
    // Moving it back would sign it out again: no Undo.
    assert!(h.query_by_label("Undo").is_none());
}

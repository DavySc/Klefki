//! Duplicate: the name and places it suggests, the fields it copies, and what it asks the engine for.

use super::*;

#[test]
fn duplicate_suggests_a_name_and_the_same_place() {
    let (mut h, engine) = duplicate_harness();
    click(&mut h, "beta");
    press(&mut h, Tool::Duplicate);
    h.get_by_label_contains("Duplicate beta");
    assert_eq!(h.state().duplicate.as_ref().unwrap().name, "beta_copy");
    click(&mut h, "Review");
    // Hidden while the plan is made; kept until it comes.
    assert!(h.query_by_label_contains("Duplicate beta").is_none());
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Duplicate {
            id: h.state().snapshot.servers[1].id(),
            name: "beta_copy".into(),
            targets: vec![Target::Local("C:/Repos/WebApp".into())],
            adjust: Default::default(),
        }]
    );
    engine
        .events
        .send(Event::PlanReady(plan_view(1, &["+ beta_copy"], &[], false)))
        .unwrap();
    h.run();
    assert!(h.state().duplicate.is_none());
    h.get_by_label_contains("Review the change");
}

/// The Duplicate dialog for a user server that signs in, its definition read.
fn duplicate_sso() -> (
    Harness<'static, ShellApp>,
    klefki_core::api::Detached,
    ServerRow,
) {
    let (mut h, engine) = live_harness();
    let mut sso = live_row("sso", Scope::User, None);
    sso.transport = TransportKind::Http;
    sso.target = "https://mcp.acme.com/v1/mcp".into();
    sso.oauth = Some(klefki_core::api::OAuthInfo {
        client_id: Some("acme-1".into()),
        scopes: Vec::new(),
    });
    show_rows(&mut h, &engine, vec![sso.clone()]);
    click(&mut h, "sso");
    drain(&engine);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    // Its address is read from the file (the list leaves out the query); Review waits for it.
    assert_eq!(drain(&engine), vec![Command::Inspect(sso.id())]);
    h.get_by_label_contains("Reading its address");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    engine
        .events
        .send(entry_event(&sso, SSO_DEFINITION, true))
        .unwrap();
    h.run();
    (h, engine, sso)
}

fn copy_fields_of<'a>(h: &'a mut Harness<'static, ShellApp>) -> &'a mut CopyFields {
    match &mut h.state_mut().duplicate.as_mut().unwrap().copy {
        CopyRead::Ready(fields) => fields,
        other => panic!("{other:?}"),
    }
}

#[test]
fn duplicate_changes_the_base_address_port_and_client_of_a_server_that_signs_in() {
    let (mut h, engine, sso) = duplicate_sso();
    // Filled in from the original, with what the base makes of each address.
    h.get_by_label("Base address");
    h.get_by_label("Client ID");
    h.get_by_label("Callback port");
    assert!(h.query_by_label("Client secret").is_none());
    h.get_by_label_contains("Server: https://mcp.acme.com/v1/mcp?tenant=7");
    h.get_by_label_contains("its client secret");
    let f = copy_fields_of(&mut h);
    f.base = "https://eu.acme.com/v2".into();
    f.callback_port = "9001".into();
    f.client_id = "acme-2".into();
    h.run();
    h.get_by_label_contains("Server: https://eu.acme.com/v2/mcp?tenant=7");
    h.get_by_label_contains(
        "Sign-in metadata: https://eu.acme.com/v2/.well-known/oauth-authorization-server",
    );
    // Another client: its own secret, asked for here.
    h.get_by_label("Client secret");
    h.get_by_label_contains("signs in as the client above");
    copy_fields_of(&mut h).secret.edit().push_str("new-s3cret");
    h.run();
    click(&mut h, "Review");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Duplicate {
            id: sso.id(),
            name: "sso_copy".into(),
            targets: vec![Target::User],
            adjust: klefki_core::api::Adjust {
                base: Some(klefki_core::api::Rebase {
                    from: "https://mcp.acme.com/v1".into(),
                    to: "https://eu.acme.com/v2".into(),
                }),
                callback_port: Some(Some(9001)),
                client: Some(klefki_core::api::Client {
                    id: Some("acme-2".into()),
                    secret: Some(Secret::new("new-s3cret".into())),
                }),
            },
        }]
    );
}

#[test]
fn duplicate_comes_back_as_it_was_when_its_plan_cannot_be_made() {
    // The secret store was locked: the error says "press Review again", so the dialog must still be there, with
    // the name, the new client and its secret as typed.
    let (mut h, engine, _sso) = duplicate_sso();
    h.state_mut().duplicate.as_mut().unwrap().name = "sso_eu".into();
    copy_fields_of(&mut h).client_id = "acme-2".into();
    h.run();
    copy_fields_of(&mut h).secret.edit().push_str("new-s3cret");
    h.run();
    click(&mut h, "Review");
    assert_eq!(duplicate_requested(&engine).len(), 1);
    assert!(h.query_by_label_contains("Duplicate sso").is_none());
    engine
        .events
        .send(Event::PlanFailed(
            "Klefki can't use your system's secret store. Start or unlock it and press Review again.\n\
             The store is locked."
                .to_owned(),
        ))
        .unwrap();
    h.run();
    h.get_by_label_contains("Duplicate sso");
    h.get_by_label(
        "Klefki can't use your system's secret store. Start or unlock it and press Review again.",
    );
    h.get_by_label("The store is locked.");
    // With the reason by the buttons, the dialog still fits in the window, the store's own long words included.
    for size in [egui::vec2(1200.0, 720.0), egui::vec2(800.0, 480.0)] {
        h.set_size(size);
        h.state_mut().duplicate.as_mut().unwrap().failed = Some(
            "Klefki can't use your system's secret store. Start or unlock it (GNOME Keyring or KWallet) and press \
             Review again.\nThe system's secret store is not available (Platform failure: zbus error: Failed to \
             connect to address `unix:path=/run/user/1000/bus`: No such file or directory (os error 2)). Klefki \
             keeps no plaintext copy of secrets anywhere else."
                .to_owned(),
        );
        h.run();
        h.run();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for label in ["Duplicate sso", "Review", "Cancel"] {
            let rect = h.get_by_label(label).rect();
            assert!(window.contains_rect(rect), "{size:?}: {label} at {rect:?}");
        }
    }
    h.set_size(egui::vec2(1100.0, 900.0));
    h.run();
    assert_eq!(h.state().duplicate.as_ref().unwrap().name, "sso_eu");
    let f = copy_fields_of(&mut h);
    assert_eq!(f.client_id, "acme-2");
    assert!(f.secret == Secret::new("new-s3cret".into()));
    // Not said twice, beside the window as well.
    assert!(h.state().outcome.is_none());
    // Review again: the reason goes, and the same change is asked for.
    click(&mut h, "Review");
    assert!(h.state().duplicate.as_ref().unwrap().failed.is_none());
    assert!(matches!(
        duplicate_requested(&engine).as_slice(),
        [Change::Duplicate { name, .. }] if name == "sso_eu"
    ));
}

#[test]
fn a_failed_plan_for_something_else_leaves_a_closed_duplicate_closed() {
    let (mut h, engine) = duplicate_harness();
    engine
        .events
        .send(Event::PlanFailed("alpha is gone.".to_owned()))
        .unwrap();
    h.run();
    assert!(h.state().duplicate.is_none());
    h.get_by_label_contains("alpha is gone.");
}

#[test]
fn duplicate_says_what_is_wrong_with_a_change_and_waits_for_it() {
    let (mut h, engine, sso) = duplicate_sso();
    let focus = |h: &mut Harness<'static, ShellApp>, id: &str| {
        h.ctx.memory_mut(|m| m.request_focus(egui::Id::new(id)));
        h.run();
    };
    focus(&mut h, "copy_callback_port");
    copy_fields_of(&mut h).callback_port = "70000".into();
    h.run();
    // Still typing: not said yet, but Review waits.
    assert!(h.query_by_label_contains("from 1 to 65535").is_none());
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    h.key_press(egui::Key::Tab);
    h.run();
    h.get_by_label_contains("from 1 to 65535");
    // Said as it is from then on: fixed, it goes.
    copy_fields_of(&mut h).callback_port = "8123".into();
    h.run();
    assert!(h.query_by_label_contains("from 1 to 65535").is_none());
    focus(&mut h, "copy_base");
    copy_fields_of(&mut h).base = String::new();
    h.run();
    h.key_press(egui::Key::Tab);
    h.run();
    h.get_by_label_contains("Type the address");
    // The addresses stay in view.
    h.get_by_label_contains("Sign-in metadata:");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    // Typed without its scheme, the base gets one when left.
    let base = egui::Id::new("copy_base");
    h.ctx.memory_mut(|m| m.request_focus(base));
    h.run();
    copy_fields_of(&mut h).base = "mcp.acme.com/v1".into();
    h.run();
    h.key_press(egui::Key::Tab);
    h.run();
    assert_eq!(copy_fields_of(&mut h).base, "https://mcp.acme.com/v1");
    // Untouched, the copy changes nothing.
    click(&mut h, "Review");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Duplicate {
            id: sso.id(),
            name: "sso_copy".into(),
            targets: vec![Target::User],
            adjust: Default::default(),
        }]
    );
}

#[test]
fn duplicate_warns_about_a_callback_port_another_program_holds_once_left() {
    let (mut h, engine, _) = duplicate_sso();
    let port = egui::Id::new("copy_callback_port");
    h.ctx.memory_mut(|m| m.request_focus(port));
    h.run();
    copy_fields_of(&mut h).callback_port = "8766".into();
    h.run();
    assert!(drain(&engine).is_empty(), "not while it is typed");
    h.key_press(egui::Key::Tab);
    h.run();
    assert_eq!(drain(&engine), vec![Command::CheckCallbackPort(8766)]);
    engine
        .events
        .send(Event::CallbackPort {
            port: 8766,
            warning: Some("Port 8766 is in use by another program.".into()),
        })
        .unwrap();
    h.run();
    h.get_by_label_contains("Port 8766 is in use");
    // A warning, not a stop.
    assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    copy_fields_of(&mut h).callback_port = "8767".into();
    h.run();
    assert!(h.query_by_label_contains("Port 8766 is in use").is_none());
}

#[test]
fn duplicate_of_a_server_whose_definition_cannot_be_read_copies_it_as_it_is() {
    let (mut h, engine) = live_harness();
    let mut web = live_row("web", Scope::User, None);
    web.transport = TransportKind::Http;
    web.target = "https://h.example/mcp".into();
    show_rows(&mut h, &engine, vec![web.clone()]);
    click(&mut h, "web");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    engine
        .events
        .send(Event::Entry {
            id: web.id(),
            json: Err("the server is no longer in the configuration".into()),
            editable: true,
        })
        .unwrap();
    h.run();
    h.get_by_label_contains("can't be changed here");
    assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    // A plain address: one field, the whole of it, and nothing about signing in.
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, vec![web.clone()]);
    click(&mut h, "web");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    engine
        .events
        .send(entry_event(
            &web,
            r#"{"type": "http", "url": "https://h.example/mcp"}"#,
            true,
        ))
        .unwrap();
    h.run();
    h.get_by_label("Address");
    assert!(h.query_by_label("Client ID").is_none());
    assert!(h.query_by_label_contains("Server:").is_none());
}

#[test]
fn duplicate_goes_wherever_it_is_ticked_under_the_name_typed() {
    let (mut h, engine) = duplicate_harness();
    click(&mut h, "alpha");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    // User is ticked for a user server; add both projects, untick the user.
    h.get_by_label_contains("For the user").click();
    h.get_by_label("WebApp").click();
    click(&mut h, "notes");
    h.state_mut().duplicate.as_mut().unwrap().name = "alpha-2".into();
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Duplicate {
            id: h.state().snapshot.servers[0].id(),
            name: "alpha-2".into(),
            targets: vec![
                Target::Local("C:/Repos/WebApp".into()),
                Target::Local("/home/me/notes".into()),
            ],
            adjust: Default::default(),
        }]
    );
}

#[test]
fn duplicate_needs_a_usable_name_and_a_place() {
    let (mut h, engine) = duplicate_harness();
    click(&mut h, "alpha");
    press(&mut h, Tool::Duplicate);
    h.state_mut().duplicate.as_mut().unwrap().name = "has space".into();
    h.run();
    h.get_by_label_contains("not allowed");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    h.state_mut().duplicate.as_mut().unwrap().name = "fine".into();
    h.run();
    assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    h.get_by_label_contains("For the user").click();
    h.run();
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    h.key_press(egui::Key::Enter);
    h.run();
    click(&mut h, "Review");
    assert!(duplicate_requested(&engine).is_empty());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().duplicate.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn duplicate_waits_while_another_plan_is_on_its_way() {
    // One plan at a time: a Disable not yet answered keeps Duplicate from asking for a second.
    let (mut h, engine) = duplicate_harness();
    click(&mut h, "alpha");
    press(&mut h, Tool::TurnOff);
    assert!(tool(&h, Tool::Duplicate).accesskit_node().is_disabled());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    assert!(h.state().duplicate.is_none());
    assert_eq!(drain(&engine).len(), 1, "only the Disable");
}

#[test]
fn a_project_server_cannot_be_duplicated_yet() {
    let (mut h, _engine) = duplicate_harness();
    click(&mut h, "gamma");
    assert!(tool(&h, Tool::Duplicate).accesskit_node().is_disabled());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    assert!(h.state().duplicate.is_none());
}

#[test]
fn duplicate_fits_in_the_window_however_many_projects_claude_knows() {
    let (mut h, _engine) = duplicate_harness_with_many_projects();
    open_duplicate_of(&mut h, "alpha");
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 900.0));
    for label in ["Review", "Cancel"] {
        let r = h.get_by_label(label).rect();
        assert!(
            window.contains_rect(r),
            "{label} at {r:?}, window {window:?}"
        );
    }
    let title = h.get_by_label_contains("Duplicate alpha").rect();
    assert!(window.contains_rect(title), "title at {title:?}");
}

#[test]
fn duplicate_finds_a_project_by_part_of_its_path_and_keeps_what_was_ticked() {
    let (mut h, engine) = duplicate_harness_with_many_projects();
    open_duplicate_of(&mut h, "alpha");
    h.state_mut().duplicate.as_mut().unwrap().places.filter = "HOME/me".into();
    h.run();
    assert!(h.query_by_label("project-00").is_none());
    click(&mut h, "notes");
    h.state_mut().duplicate.as_mut().unwrap().places.filter = "web".into();
    h.run();
    assert!(h.query_by_label("notes").is_none());
    click(&mut h, "WebApp");
    h.state_mut()
        .duplicate
        .as_mut()
        .unwrap()
        .places
        .filter
        .clear();
    h.run();
    click(&mut h, "Review");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Duplicate {
            id: h.state().snapshot.servers[0].id(),
            name: "alpha_copy".into(),
            targets: vec![
                Target::User,
                Target::Local("C:/Repos/WebApp".into()),
                Target::Local("/home/me/notes".into()),
            ],
            adjust: Default::default(),
        }]
    );
}

#[test]
fn duplicate_says_where_a_project_lives_only_when_its_folder_name_is_not_enough() {
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a],
            projects: vec![
                "C:/work/app".into(),
                "D:/play/App/".into(),
                "C:/Repos/WebApp".into(),
            ],
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    open_duplicate_of(&mut h, "alpha");
    h.get_by_label("app, C:/work");
    h.get_by_label("App, D:/play");
    h.get_by_label("WebApp");
}

#[test]
fn duplicate_lists_the_project_the_server_is_in_first() {
    let (mut h, _engine) = duplicate_harness_with_many_projects();
    open_duplicate_of(&mut h, "beta");
    let here = h.get_by_label("WebApp").rect();
    let other = h.get_by_label("project-00").rect();
    assert!(
        here.top() < other.top(),
        "WebApp {here:?}, project-00 {other:?}"
    );
}

#[test]
fn duplicate_says_in_words_where_the_copy_will_go() {
    let (mut h, _engine) = duplicate_harness();
    open_duplicate_of(&mut h, "alpha");
    h.get_by_label("Adds alpha_copy for the user.");
    h.get_by_label("WebApp").click();
    click(&mut h, "notes");
    h.get_by_label("Adds alpha_copy for the user and in WebApp and notes.");
    h.get_by_label_contains("For the user").click();
    h.run();
    h.get_by_label("Adds alpha_copy in WebApp and notes.");
    h.get_by_label("WebApp").click();
    click(&mut h, "notes");
    h.get_by_label("Tick at least one place.");
}

#[test]
fn duplicate_says_where_the_name_is_already_taken_before_review() {
    let (mut h, _engine) = duplicate_harness();
    open_duplicate_of(&mut h, "alpha");
    // beta is a local server in WebApp; the user has none by that name.
    h.state_mut().duplicate.as_mut().unwrap().name = "beta".into();
    h.run();
    assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    click(&mut h, "WebApp");
    h.get_by_label_contains("beta already exists in WebApp");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    // The user's own alpha.
    h.state_mut().duplicate.as_mut().unwrap().name = "alpha".into();
    h.run();
    h.get_by_label_contains("alpha already exists for the user");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    // gamma comes from WebApp's .mcp.json, not from a place a copy is written to: allowed,
    // and the review warns that the local copy would hide it.
    h.state_mut().duplicate.as_mut().unwrap().name = "gamma".into();
    h.get_by_label_contains("For the user").click();
    h.run();
    assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
}

#[test]
fn the_palette_offers_duplicate() {
    let (mut h, _engine) = duplicate_harness();
    click(&mut h, "alpha");
    ctrl_k(&mut h);
    type_into_palette(&mut h, "dup");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().duplicate.as_ref().unwrap().name, "alpha_copy");
}

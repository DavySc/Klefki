//! Editing a server: the form, the save bar, and the client secret in the system's secret store.

use super::*;

#[test]
fn the_auth_tab_shows_the_oauth_client_and_scopes() {
    use egui::accesskit::Role;
    let (mut h, engine) = live_harness();
    let mut signed = live_row("acme-prod", Scope::User, None);
    signed.oauth = Some(klefki_core::api::OAuthInfo {
        client_id: Some("klefki-client-1".into()),
        scopes: vec!["read".into(), "write".into()],
    });
    let mut dynamic = live_row("dyn", Scope::User, None);
    dynamic.oauth = Some(klefki_core::api::OAuthInfo {
        client_id: None,
        scopes: Vec::new(),
    });
    let plain = live_row("plain", Scope::User, None);
    show_rows(&mut h, &engine, vec![signed, dynamic, plain]);
    let open = |h: &mut Harness<'static, ShellApp>, name: &str| {
        h.get_by_label(name).click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Auth").click();
        h.run();
    };
    open(&mut h, "acme-prod");
    h.get_by_label("klefki-client-1");
    h.get_by_label("read, write");
    open(&mut h, "dyn");
    h.get_by_label_contains("registers itself");
    open(&mut h, "plain");
    h.get_by_label_contains("does not sign in with OAuth");
}

fn oauth_harness() -> (
    Harness<'static, ShellApp>,
    klefki_core::api::Detached,
    ServerRow,
) {
    use egui::accesskit::Role;
    let (mut h, engine) = live_harness();
    let mut sso = live_row("sso", Scope::User, None);
    sso.oauth = Some(klefki_core::api::OAuthInfo {
        client_id: Some("klefki-1".into()),
        scopes: Vec::new(),
    });
    show_rows(
        &mut h,
        &engine,
        vec![sso.clone(), live_row("plain", Scope::User, None)],
    );
    click(&mut h, "sso");
    h.get_by_role_and_label(Role::Button, "Auth").click();
    h.run();
    (h, engine, sso)
}

fn secret_event(row: &ServerRow, stored: Result<bool, String>) -> Event {
    Event::SecretStatus {
        id: row.id(),
        stored,
    }
}

#[test]
fn the_auth_tab_asks_once_whether_a_client_secret_is_stored() {
    let (mut h, engine, sso) = oauth_harness();
    assert_eq!(drain(&engine), vec![Command::SecretStatus(sso.id())]);
    h.get_by_label_contains("Checking");
    h.run();
    assert!(drain(&engine).is_empty(), "asked once, not every frame");
    engine.events.send(secret_event(&sso, Ok(true))).unwrap();
    h.run();
    h.get_by_label_contains("Stored in your system's secret store");
    // A server that does not sign in with OAuth has no client secret to ask about.
    click(&mut h, "plain");
    assert!(drain(&engine).is_empty());
}

#[test]
fn a_client_secret_is_typed_hidden_and_handed_over_once() {
    let (mut h, engine, sso) = oauth_harness();
    engine.events.send(secret_event(&sso, Ok(false))).unwrap();
    h.run();
    drain(&engine);
    h.get_by_label_contains("Not stored");
    click(&mut h, "Set client secret…");
    h.state_mut()
        .secret_input
        .as_mut()
        .unwrap()
        .edit()
        .push_str("s3cret-value");
    h.run();
    assert!(
        h.query_by_value("s3cret-value").is_none(),
        "not shown, not even to assistive technology"
    );
    click(&mut h, "Save");
    assert_eq!(
        drain(&engine),
        vec![Command::StoreSecret {
            id: sso.id(),
            secret: klefki_core::api::Secret::new("s3cret-value".into()),
        }]
    );
    assert!(h.state().secret_input.is_none(), "the app keeps no copy");
    h.get_by_label_contains("Saving");
}

#[test]
fn forgetting_a_client_secret_asks_first() {
    let (mut h, engine, sso) = oauth_harness();
    engine.events.send(secret_event(&sso, Ok(true))).unwrap();
    h.run();
    drain(&engine);
    click(&mut h, "Forget…");
    assert!(drain(&engine).is_empty(), "not yet");
    h.get_by_label_contains("cannot be undone");
    click(&mut h, "Forget it");
    assert_eq!(drain(&engine), vec![Command::ForgetSecret(sso.id())]);
}

#[test]
fn a_secret_store_problem_is_shown_and_can_be_retried() {
    let (mut h, engine, sso) = oauth_harness();
    drain(&engine);
    engine
        .events
        .send(secret_event(&sso, Err("The system's secret store is not available (no Secret Service). Klefki keeps no plaintext copy of secrets anywhere else.".into())))
        .unwrap();
    h.run();
    h.get_by_label_contains("no plaintext copy");
    click(&mut h, "Try again");
    assert_eq!(drain(&engine), vec![Command::SecretStatus(sso.id())]);
}

#[test]
fn the_form_edits_the_same_definition_as_the_json_tab() {
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    assert_eq!(form(&mut h).command, "node");
    // Nothing edited yet: nothing to review.
    assert!(
        h.get_by_label("Review and save…")
            .accesskit_node()
            .is_disabled()
    );
    form(&mut h).command = "node22".into();
    form(&mut h).args.push("--x".into());
    h.run();
    let text = json_text(&h);
    assert_eq!(
        text,
        "{\n  \"cwd\": \"/w\",\n  \"command\": \"node22\",\n  \"args\": [\n    \"a.js\",\n    \"--x\"\n  ],\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}",
        "the key the form does not show and the hidden value stay as they were"
    );
    click(&mut h, "Review and save…");
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![Change::Replace {
            id: a.id(),
            json: text
        }])]
    );
    // The JSON shows the same draft.
    click(&mut h, "Edit as JSON");
    h.get_by_value(&json_text(&h));
}

#[test]
fn editing_the_json_updates_the_form() {
    let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
    if let Some(Editor {
        state: EditorState::Ready { text, .. },
        ..
    }) = &mut h.state_mut().editor
    {
        *text = text.replace("\"node\"", "\"deno\"");
    }
    h.run();
    assert_eq!(form(&mut h).command, "deno");
    // JSON the form cannot read: the form says so instead of guessing.
    if let Some(Editor {
        state: EditorState::Ready { text, .. },
        ..
    }) = &mut h.state_mut().editor
    {
        *text = "{ \"command\": ".into();
    }
    h.run();
    h.get_by_label_contains("not valid");
    // Revert brings the form back.
    click(&mut h, "Revert");
    assert_eq!(form(&mut h).command, "node");
}

#[test]
fn values_stay_hidden_until_changed_and_new_ones_can_be_added() {
    let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
    click(&mut h, "Env");
    h.get_by_label("hidden");
    click(&mut h, "Change TOKEN");
    assert_eq!(form(&mut h).env, [("TOKEN".to_owned(), String::new())]);
    click(&mut h, "Add variable");
    assert_eq!(form(&mut h).env.len(), 2);
    form(&mut h).env[1] = ("MODE".into(), "dev".into());
    form(&mut h).env[0].1 = "n3w".into();
    h.run();
    assert!(
        json_text(&h).contains("\"MODE\": \"dev\""),
        "{}",
        json_text(&h)
    );
    // Typed values are not shown in the clear, to the eye or to assistive technology.
    assert!(h.query_by_value("n3w").is_none());
    assert!(h.query_by_value("dev").is_none());
    assert!(
        h.query_all_by_value("MODE").next().is_some(),
        "names are shown"
    );
    click(&mut h, "Remove MODE");
    assert!(!json_text(&h).contains("MODE"));
}

#[test]
fn a_typed_secret_is_met_with_a_reference_to_use_instead() {
    let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
    click(&mut h, "Env");
    // The value that is kept is never offered a hint: the app does not have it.
    assert!(h.query_by_label_contains("Looks like a secret").is_none());
    click(&mut h, "Add variable");
    form(&mut h).env[1] = ("MODE".into(), "dev".into());
    h.run();
    assert!(h.query_by_label_contains("Looks like a secret").is_none());
    form(&mut h).env[1] = (
        "GITHUB_TOKEN".into(),
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
    );
    h.run();
    h.get_by_label_contains("Looks like a secret");
    assert!(
        h.query_by_value("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
            .is_none()
    );
    click(&mut h, "Use ${GITHUB_TOKEN} instead");
    assert_eq!(form(&mut h).env[1].1, "${GITHUB_TOKEN}");
    assert!(
        json_text(&h).contains("\"GITHUB_TOKEN\": \"${GITHUB_TOKEN}\""),
        "{}",
        json_text(&h)
    );
    assert!(h.query_by_label_contains("Looks like a secret").is_none());
}

#[test]
fn the_form_does_not_widen_the_detail_panel() {
    // Seen in the real window: rows that asked for the panel's whole width plus a button made the resizable
    // panel grow a little every frame, until it reached its largest size.
    let long = "{\n  \"command\": \"npx\",\n  \"args\": [\n    \"-y\",\n    \"@modelcontextprotocol/server-github-with-a-long-name\"\n  ],\n  \"env\": {\n    \"GITHUB_PERSONAL_ACCESS_TOKEN\": \"<hidden>\"\n  }\n}";
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    show_rows(&mut h, &engine, vec![a.clone()]);
    click(&mut h, "alpha");
    let before = detail_left_edge(&h, "General");
    click(&mut h, "Edit definition");
    engine.events.send(entry_event(&a, long, true)).unwrap();
    h.run();
    assert_eq!(detail_left_edge(&h, "General"), before, "General");
    click(&mut h, "Env");
    assert_eq!(detail_left_edge(&h, "General"), before, "Env");
}

#[test]
fn a_problem_in_the_form_is_shown_and_blocks_review() {
    let (mut h, engine, _a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "  ".into();
    h.run();
    h.get_by_label_contains("needs a command");
    assert!(
        h.get_by_label("Review and save…")
            .accesskit_node()
            .is_disabled()
    );
    click(&mut h, "Review and save…");
    assert!(drain(&engine).is_empty());
}

#[test]
fn the_form_asks_for_one_plan_at_a_time() {
    let (mut h, engine, _a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    click(&mut h, "Review and save…");
    // The first is on its way: the button waits for its answer.
    assert!(
        h.get_by_label("Review and save…")
            .accesskit_node()
            .is_disabled()
    );
    click(&mut h, "Review and save…");
    assert_eq!(drain(&engine).len(), 1);
}

#[test]
fn project_servers_are_shown_but_not_edited_in_the_form() {
    let (h, _engine, _a) = form_harness("{\n  \"command\": \"x\"\n}", false);
    h.get_by_label_contains("does not edit files in a repository");
    assert!(h.query_by_label("Review and save…").is_none());
}

#[test]
fn a_server_that_signs_in_is_edited_in_the_form_and_says_it_signs_in_again() {
    let (mut h, engine, a) = form_harness(
        "{\n  \"type\": \"http\",\n  \"url\": \"https://h.example/mcp\",\n  \"oauth\": {\n    \"clientId\": \"c\"\n  }\n}",
        true,
    );
    // Before anything is typed.
    h.get_by_label_contains("you sign in again afterwards");
    form(&mut h).url = "https://h.example/v2/mcp".into();
    h.run();
    click(&mut h, "Review and save…");
    match &drain(&engine)[..] {
        [Command::Prepare(changes)] => match &changes[..] {
            [Change::Replace { id, json }] => {
                assert_eq!(*id, a.id());
                assert!(
                    json.contains("v2/mcp") && json.contains("\"clientId\": \"c\""),
                    "{json}"
                );
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_project_server_is_shown_read_only() {
    let (mut h, engine) = live_harness();
    let p = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
    show_rows(&mut h, &engine, vec![p.clone()]);
    press_edit(&mut h, "docs");
    engine
        .events
        .send(entry_event(&p, "{\n  \"command\": \"x\"\n}", false))
        .unwrap();
    h.run();
    h.get_by_label_contains("does not edit files in a repository");
    assert!(h.query_by_label("Review and save…").is_none());
    h.get_by_value("{\n  \"command\": \"x\"\n}");
}

#[test]
fn a_definition_that_cannot_be_read_says_why() {
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    show_rows(&mut h, &engine, vec![a.clone()]);
    press_edit(&mut h, "alpha");
    engine
        .events
        .send(Event::Entry {
            id: a.id(),
            json: Err("the server is no longer in the configuration".into()),
            editable: true,
        })
        .unwrap();
    h.run();
    h.get_by_label_contains("no longer in the configuration");
}

#[test]
fn after_a_save_the_definition_is_read_again() {
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    show_rows(&mut h, &engine, vec![a.clone()]);
    press_edit(&mut h, "alpha");
    engine.events.send(entry_event(&a, "{}", true)).unwrap();
    h.run();
    drain(&engine);
    engine
        .events
        .send(Event::Applied {
            plan: 1,
            changed: true,
            backup: None,
            notes: Vec::new(),
            undo: None,
        })
        .unwrap();
    h.run();
    assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
}

/// Another change, prepared and applied while the form is open: Turn off from the toolbar, say.
fn apply_another_change(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    plan: u64,
) {
    engine
        .events
        .send(Event::PlanReady(plan_view(plan, &["+ alpha"], &[], false)))
        .unwrap();
    h.run();
    click(h, "Apply");
    assert_eq!(drain(engine), vec![Command::Apply(plan)]);
    engine
        .events
        .send(Event::Applied {
            plan,
            changed: true,
            backup: None,
            notes: Vec::new(),
            undo: None,
        })
        .unwrap();
    h.run();
}

#[test]
fn a_draft_not_yet_saved_survives_another_change_being_applied() {
    // Found in the UI review: a command edited in the form, then the same server turned off, and the edit was
    // replaced by what was saved before.
    let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    apply_another_change(&mut h, &engine, 3);
    assert!(
        drain(&engine).is_empty(),
        "the definition is not asked for again"
    );
    assert_eq!(form(&mut h).command, "node22");
    assert!(
        !h.get_by_label("Review and save…")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn the_save_bar_says_while_a_draft_is_not_saved() {
    // A draft is kept through other changes, so it says it is not saved yet rather than passing for what the
    // file holds.
    let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
    assert!(h.query_by_label("Unsaved").is_none());
    form(&mut h).command = "node22".into();
    h.run();
    apply_another_change(&mut h, &engine, 3);
    h.get_by_label("Unsaved");
    // Once the message about the other change has faded: it sits over the save bar until then.
    h.state_mut().outcome.as_mut().unwrap().shown_at = Some(-1000.0);
    h.run();
    click(&mut h, "Revert");
    assert_eq!(form(&mut h).command, "node");
    assert!(h.query_by_label("Unsaved").is_none());
}

#[test]
fn a_draft_edited_as_json_survives_another_change_being_applied() {
    let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
    set_json_text(&mut h, |t| t.replace("a.js", "b.js"));
    h.run();
    apply_another_change(&mut h, &engine, 3);
    assert!(drain(&engine).is_empty());
    assert!(json_text(&h).contains("b.js"));
}

#[test]
fn a_saved_draft_is_read_again_once_applied() {
    // What was written is read back: the draft's typed values are dropped and the form starts from the file.
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    click(&mut h, "Review and save…");
    assert!(matches!(drain(&engine).as_slice(), [Command::Prepare(_)]));
    apply_another_change(&mut h, &engine, 5);
    assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
}

#[test]
fn a_draft_whose_review_was_cancelled_or_failed_survives_a_later_change() {
    let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    // Reviewed, then cancelled.
    click(&mut h, "Review and save…");
    engine
        .events
        .send(Event::PlanReady(plan_view(5, &["+ node22"], &[], false)))
        .unwrap();
    h.run();
    click(&mut h, "Cancel");
    // Reviewed, and the engine could not prepare it.
    click(&mut h, "Review and save…");
    engine
        .events
        .send(Event::PlanFailed("the file changed".into()))
        .unwrap();
    h.run();
    drain(&engine);
    apply_another_change(&mut h, &engine, 6);
    assert!(drain(&engine).is_empty());
    assert_eq!(form(&mut h).command, "node22");
}

/// `change`, asked for from the toolbar or a dialog while the form is open, applied with `after` as the list
/// the engine sends first.
fn apply_while_editing(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    change: Change,
    after: Vec<ServerRow>,
) {
    h.state_mut().prepare(vec![change]);
    h.run();
    drain(engine);
    engine
        .events
        .send(Event::PlanReady(plan_view(4, &["+ alpha"], &[], false)))
        .unwrap();
    h.run();
    click(h, "Apply");
    assert_eq!(drain(engine), vec![Command::Apply(4)]);
    engine.events.send(snapshot_of(after)).unwrap();
    engine
        .events
        .send(Event::Applied {
            plan: 4,
            changed: true,
            backup: None,
            notes: Vec::new(),
            undo: None,
        })
        .unwrap();
    h.run();
}

fn editing(h: &Harness<'static, ShellApp>) -> Option<ServerId> {
    h.state().editor.as_ref().map(|e| e.id.clone())
}

#[test]
fn a_draft_not_yet_saved_follows_its_server_when_it_is_moved() {
    // Found in the UI review: a command edited, then the same server moved, and the edit was gone.
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    // Somewhere else in the list once moved, after a server that was not there before.
    let moved = live_row("alpha", Scope::Local, Some("C:/Repos/WebApp"));
    let before = live_row("aaa", Scope::Local, Some("C:/Repos/WebApp"));
    apply_while_editing(
        &mut h,
        &engine,
        Change::Move {
            id: a.id(),
            to: Target::Local("C:/Repos/WebApp".into()),
        },
        vec![before, moved.clone()],
    );
    // Read again from the new place, to compare the draft with and to revert to.
    assert_eq!(drain(&engine), vec![Command::Inspect(moved.id())]);
    assert_eq!(editing(&h), Some(moved.id()));
    assert_eq!(h.state().selected, BTreeSet::from([1]));
    assert_eq!(form(&mut h).command, "node22");
    h.get_by_label("Unsaved");
    engine
        .events
        .send(entry_event(&moved, FORM_ORIGINAL, true))
        .unwrap();
    h.run();
    assert_eq!(form(&mut h).command, "node22");
    h.get_by_label("Unsaved");
}

#[test]
fn a_draft_that_follows_a_move_is_compared_with_what_the_new_place_holds() {
    // From the review of #16: a move through Claude Code can leave settings out, and the draft was still
    // compared with, reverted to and saved over the old place's definition.
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    let moved = live_row("alpha", Scope::Local, Some("C:/Repos/WebApp"));
    apply_while_editing(
        &mut h,
        &engine,
        Change::Move {
            id: a.id(),
            to: Target::Local("C:/Repos/WebApp".into()),
        },
        vec![moved.clone()],
    );
    assert_eq!(drain(&engine), vec![Command::Inspect(moved.id())]);
    let without_cwd = FORM_ORIGINAL.replace("\"cwd\": \"/w\",\n  ", "");
    assert!(!without_cwd.contains("cwd"));
    engine
        .events
        .send(entry_event(&moved, &without_cwd, true))
        .unwrap();
    h.run();
    // The edit is kept; what the move left out is not brought back as if it were an edit.
    assert_eq!(form(&mut h).command, "node22");
    assert!(!json_text(&h).contains("cwd"), "{}", json_text(&h));
    h.state_mut().outcome = None;
    h.run();
    click(&mut h, "Review and save…");
    match &drain(&engine)[..] {
        [Command::Prepare(changes)] => match &changes[..] {
            [Change::Replace { id, json }] => {
                assert_eq!(*id, moved.id());
                assert_eq!(*json, without_cwd.replace("\"node\"", "\"node22\""));
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    // Revert goes back to what the new place holds.
    h.state_mut().plan = None;
    h.state_mut().preparing = false;
    h.run();
    click(&mut h, "Revert");
    assert_eq!(json_text(&h), without_cwd);
    assert!(h.query_by_label("Unsaved").is_none());
}

#[test]
fn a_draft_not_yet_saved_follows_its_server_when_it_is_renamed() {
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    let renamed = live_row("Alpha", Scope::User, None);
    apply_while_editing(
        &mut h,
        &engine,
        Change::Rename {
            id: a.id(),
            name: "Alpha".into(),
        },
        vec![live_row("Aaa", Scope::User, None), renamed.clone()],
    );
    assert_eq!(drain(&engine), vec![Command::Inspect(renamed.id())]);
    assert_eq!(editing(&h), Some(renamed.id()));
    assert_eq!(h.state().selected, BTreeSet::from([1]));
    assert_eq!(form(&mut h).command, "node22");
}

#[test]
fn a_server_renamed_under_a_draft_is_brought_into_view() {
    // From the review of #16: a move brought its row into view, a rename did not.
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    let mut after: Vec<ServerRow> = (0..80)
        .map(|i| live_row(&format!("a{i:02}"), Scope::User, None))
        .collect();
    after.push(live_row("Alpha", Scope::User, None));
    apply_while_editing(
        &mut h,
        &engine,
        Change::Rename {
            id: a.id(),
            name: "Alpha".into(),
        },
        after,
    );
    assert_eq!(h.state().selected, BTreeSet::from([80]));
    h.run();
    assert!(
        h.state().on_screen.contains(&80),
        "{:?}",
        h.state().on_screen
    );
}

#[test]
fn the_move_and_casing_dialogs_say_an_unsaved_edit_goes_along() {
    // From the review of #16: said before the change is confirmed, not only seen afterwards.
    let (mut h, _engine, _) = form_harness(FORM_ORIGINAL, true);
    press(&mut h, Tool::Move);
    assert!(h.query_by_label(GOES_ALONG).is_none(), "nothing edited");
    h.key_press(egui::Key::Escape);
    h.run();
    form(&mut h).command = "node22".into();
    h.run();
    press(&mut h, Tool::Move);
    h.get_by_label(GOES_ALONG);

    let (mut h, engine) = casing_harness(vec![]);
    let local = h.state().snapshot.servers[1].clone();
    press_edit(&mut h, "gateway");
    engine
        .events
        .send(entry_event(&local, FORM_ORIGINAL, true))
        .unwrap();
    h.run();
    form(&mut h).command = "node22".into();
    h.run();
    click(&mut h, "Fix casing…");
    // Kept as it is spelled, the edited server is not renamed: nothing goes anywhere.
    click(&mut h, "Keep gateway");
    assert!(h.query_by_label(GOES_ALONG).is_none());
    click(&mut h, "Keep Gateway");
    h.get_by_label(GOES_ALONG);
}

#[test]
fn an_unedited_definition_is_read_again_from_where_its_server_was_moved() {
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    let moved = live_row("alpha", Scope::Local, Some("C:/Repos/WebApp"));
    apply_while_editing(
        &mut h,
        &engine,
        Change::Move {
            id: a.id(),
            to: Target::Local("C:/Repos/WebApp".into()),
        },
        vec![moved.clone()],
    );
    assert_eq!(drain(&engine), vec![Command::Inspect(moved.id())]);
    assert_eq!(editing(&h), Some(moved.id()));
}

#[test]
fn a_draft_stays_with_its_server_when_another_one_is_renamed() {
    let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
    form(&mut h).command = "node22".into();
    h.run();
    let other = live_row("beta", Scope::User, None);
    apply_while_editing(
        &mut h,
        &engine,
        Change::Rename {
            id: other.id(),
            name: "Beta".into(),
        },
        vec![a.clone(), live_row("Beta", Scope::User, None)],
    );
    assert!(drain(&engine).is_empty());
    assert_eq!(editing(&h), Some(a.id()));
    assert_eq!(form(&mut h).command, "node22");
}

#[test]
fn every_toolbar_button_stays_reachable_next_to_the_detail_panel() {
    // Found while testing Delete: with a row selected the detail panel took the toolbar's right half, and the
    // buttons past its edge were cut off and could not be clicked.
    let (mut h, engine) = live_harness();
    let (a, _) = two_rows();
    show_rows(&mut h, &engine, vec![a]);
    click(&mut h, "alpha");
    let detail_left = h.get_by_label("General").rect().min.x;
    // (Add and Import were in this list; they are no longer shown until they work.)
    for t in [
        Tool::Edit,
        Tool::Duplicate,
        Tool::Move,
        Tool::TurnOn,
        Tool::TurnOff,
        Tool::Delete,
    ] {
        let r = tool(&h, t).rect();
        assert!(
            r.max.x <= detail_left,
            "{t:?} at {r:?}, detail panel from x={detail_left}"
        );
    }
}

//! Add and Import: reading what is pasted or filled in, the places, the names, and what is sent to the engine.

use super::*;
use crate::shell::add::{Field, field_id, secret_id};
use klefki_core::api::{Part, ServerFields};

fn paste(h: &mut Harness<'static, ShellApp>, text: &str) {
    h.state_mut().add.as_mut().unwrap().text = text.to_owned();
    h.run();
}

// A click on the dimmed window beside a dialog is easily a slip (bringing Klefki back to the front, say), so it
// never throws away what was pasted, typed or chosen. Esc and Cancel close on purpose.

#[test]
fn a_click_beside_a_dialog_flashes_its_outline() {
    // The click beside does nothing to the dialog, so it shows that the dialog is still waiting for Esc or
    // Cancel, instead of seeming not to respond.
    let (mut h, _engine) = duplicate_harness();
    open_duplicate_of(&mut h, "beta");
    let id = egui::Id::new("duplicate_dialog");
    assert!(!nudged(&h.ctx, id));
    h.get_by_label_contains("Duplicate beta").click();
    h.run();
    assert!(!nudged(&h.ctx, id), "a click inside is no slip");
    click_beside(&mut h);
    assert!(nudged(&h.ctx, id));
    assert!(h.state().duplicate.is_some());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().duplicate.is_none());
}

#[test]
fn a_click_beside_the_add_dialog_keeps_what_was_pasted() {
    let (mut h, engine) = duplicate_harness();
    press(&mut h, Tool::Add);
    let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
    paste(&mut h, text);
    click_beside(&mut h);
    assert_eq!(h.state().add.as_ref().map(|f| f.text.as_str()), Some(text));
    h.get_by_label_contains("Adds files for the user.");
    h.key_press(egui::Key::Escape);
    h.run();
    click(&mut h, "Discard");
    assert!(h.state().add.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn a_click_beside_the_review_of_an_add_keeps_the_review_and_the_add_dialog() {
    let (mut h, engine) = duplicate_harness();
    press(&mut h, Tool::Add);
    let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
    paste(&mut h, text);
    click(&mut h, "Review");
    assert!(waits_behind_review(&h));
    drain(&engine);
    let mut plan = plan_view(9, &["+   \"files\""], &[], false);
    plan.action = "Add".to_owned();
    engine.events.send(Event::PlanReady(plan)).unwrap();
    h.run();
    click_beside(&mut h);
    assert!(drain(&engine).is_empty(), "the plan is not discarded");
    assert!(h.state().plan.is_some());
    assert_eq!(h.state().add.as_ref().map(|f| f.text.as_str()), Some(text));
}

#[test]
fn a_click_beside_a_plan_leaves_it_to_apply_or_cancel() {
    let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
    click_beside(&mut h);
    assert!(drain(&engine).is_empty());
    h.get_by_label_contains("Review the change");
    click(&mut h, "Cancel");
    assert_eq!(drain(&engine), vec![Command::Discard(7)]);
}

#[test]
fn a_click_beside_the_casing_dialog_keeps_it_open() {
    let (mut h, engine) = casing_harness(Vec::new());
    click(&mut h, "gateway");
    click(&mut h, "Fix casing…");
    click_beside(&mut h);
    assert!(h.state().casing.is_some());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().casing.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn a_click_beside_the_duplicate_dialog_keeps_the_name_typed() {
    let (mut h, engine) = duplicate_harness();
    open_duplicate_of(&mut h, "beta");
    h.state_mut().duplicate.as_mut().unwrap().name = "beta_two".into();
    h.run();
    click_beside(&mut h);
    assert_eq!(
        h.state().duplicate.as_ref().map(|f| f.name.as_str()),
        Some("beta_two")
    );
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().duplicate.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn a_click_beside_the_move_dialog_keeps_the_place_chosen() {
    let (mut h, engine) = duplicate_harness();
    open_move_of(&mut h, "beta");
    click(&mut h, "For the user, in every project");
    click_beside(&mut h);
    assert!(h.state().moving.is_some());
    h.get_by_label("Moves beta to the user.");
    click(&mut h, "Cancel");
    assert!(h.state().moving.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn add_reads_a_pasted_command_and_adds_it_where_it_asks() {
    let (mut h, engine) = duplicate_harness();
    press(&mut h, Tool::Add);
    h.get_by_label_contains("Add servers");
    assert!(!ready(&mut h));
    let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
    paste(&mut h, text);
    h.get_by_label_contains("npx -y @x/fs /tmp");
    h.get_by_label_contains("Adds files for the user.");
    click(&mut h, "Review");
    assert!(waits_behind_review(&h));
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Add {
            name: "files".into(),
            json: pasted_json(text, 0),
            targets: vec![Target::User],
            secret: None,
        }]
    );
}

#[test]
fn many_servers_are_counted_not_listed() {
    let (mut h, _engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    let text: String = (0..5)
        .map(|i| format!("claude mcp add -s user s{i} node\n"))
        .collect();
    paste(&mut h, &text);
    h.get_by_label_contains("Adds 5 servers for the user.");
}

#[test]
fn a_local_command_goes_to_the_selected_servers_project() {
    let (mut h, engine) = duplicate_harness();
    click(&mut h, "beta");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    let text = "claude mcp add one node\nclaude mcp add two -- node two.js";
    paste(&mut h, text);
    h.get_by_label_contains("Adds one and two in WebApp.");
    // Ticking a place by hand is kept when the text changes.
    click(&mut h, "notes");
    paste(&mut h, &format!("{text}\n"));
    h.get_by_label_contains("Adds one and two in WebApp and notes.");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    let targets = vec![
        Target::Local("C:/Repos/WebApp".into()),
        Target::Local("/home/me/notes".into()),
    ];
    assert_eq!(
        duplicate_requested(&engine),
        vec![
            Change::Add {
                name: "one".into(),
                json: pasted_json(text, 0),
                targets: targets.clone(),
                secret: None,
            },
            Change::Add {
                name: "two".into(),
                json: pasted_json(text, 1),
                targets,
                secret: None,
            },
        ]
    );
}

#[test]
fn add_explains_what_it_cannot_add_and_needs_usable_names() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    let text = "claude mcp add --client-id x corp node\nclaude mcp add -s user ok node\nclaude mcp add -s user ok node";
    paste(&mut h, text);
    h.get_by_label_contains("OAuth");
    h.get_by_label_contains("twice");
    assert!(!ready(&mut h));
    h.state_mut().add.as_mut().unwrap().names[2] = "ok2".into();
    h.run();
    assert!(ready(&mut h));
    h.get_by_label_contains("1 server cannot be added");
    // A name already used for the user, and one that is not a name.
    h.state_mut().add.as_mut().unwrap().names[1] = "alpha".into();
    h.run();
    h.get_by_label_contains("alpha already exists for the user");
    assert!(!ready(&mut h));
    h.state_mut().add.as_mut().unwrap().names[1] = "has space".into();
    h.run();
    h.get_by_label_contains("not allowed");
    assert!(!ready(&mut h));
    h.state_mut().add.as_mut().unwrap().names[1] = "ok".into();
    h.run();
    click(&mut h, "Review");
    let names: Vec<String> = duplicate_requested(&engine)
        .into_iter()
        .map(|c| match c {
            Change::Add { name, .. } => name,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(names, ["ok", "ok2"]);
}

#[test]
fn a_definition_without_a_name_needs_one_and_nowhere_is_not_a_place() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(
        &mut h,
        r#"{"type": "http", "url": "https://x.example/mcp"}"#,
    );
    h.get_by_label_contains("http · https://x.example/mcp");
    h.get_by_label_contains("Give it a name");
    assert!(!ready(&mut h));
    h.state_mut().add.as_mut().unwrap().names[0] = "x".into();
    h.run();
    assert!(ready(&mut h));
    h.get_by_label_contains("For the user").click();
    h.run();
    h.get_by_label_contains("Tick at least one place");
    assert!(!ready(&mut h));
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn add_hints_at_secrets_and_says_how_it_read_the_text() {
    let (mut h, _engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(
        &mut h,
        "claude mcp add -e GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789 -e B=$HOME gh node",
    );
    h.get_by_label_contains("GITHUB_TOKEN looks like a secret");
    h.get_by_label_contains("Claude Code fills it in");
    assert!(h.query_by_label_contains("ghp_abc").is_none());
    paste(&mut h, "npm install thing");
    h.get_by_label_contains("Nothing here adds a server");
    assert!(!ready(&mut h));
}

#[test]
fn add_is_in_the_palette_and_esc_closes_it_without_adding_anything() {
    let (mut h, engine) = duplicate_harness();
    ctrl_k(&mut h);
    type_into_palette(&mut h, "add a server");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().add.is_some());
    paste(&mut h, "claude mcp add x node");
    h.key_press(egui::Key::Escape);
    h.run();
    click(&mut h, "Discard");
    assert!(h.state().add.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

// Esc is a reflex, pressed to leave a field or a menu as often as a dialog, so it never throws away what was
// pasted or typed without asking. Cancel is clicked on purpose and closes at once.

#[test]
fn esc_closes_an_empty_add_dialog_at_once() {
    for mode in [AddMode::Paste, AddMode::Fill] {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        add_form(&mut h).mode = mode;
        // Choosing how a server connects, or where it goes, is not typing: there is nothing to lose. Nor is
        // ticking OAuth, or adding a row and leaving it empty.
        let f = add_form(&mut h);
        f.fields.transport = TransportKind::Http;
        f.fields.oauth = true;
        f.fields.headers.push((String::new(), " ".into()));
        f.fields.env.push((String::new(), String::new()));
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().add.is_none(), "{mode:?}");
    }
}

#[test]
fn esc_asks_before_throwing_away_what_was_pasted_or_typed() {
    type Typing = fn(&mut AddForm);
    let typed: [(&str, Typing); 5] = [
        ("pasted", |f| f.text = "claude mcp add x node".into()),
        ("name", |f| {
            f.mode = AddMode::Fill;
            f.name = "files".into();
            f.name_edited = true;
        }),
        ("command", |f| {
            f.mode = AddMode::Fill;
            f.fields.command = "npx -y @x/fs".into();
        }),
        ("url", |f| {
            f.mode = AddMode::Fill;
            f.fields.transport = TransportKind::Http;
            f.fields.url = "https://example.com/mcp".into();
        }),
        ("secret", |f| {
            f.mode = AddMode::Fill;
            *f.secret.edit() = "s3cret".into();
        }),
    ];
    for (what, fill) in typed {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        fill(add_form(&mut h));
        h.run();
        assert!(h.query_by_label(DISCARD_QUESTION).is_none(), "{what}");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().add.is_some(), "{what}: closed without asking");
        h.get_by_label(DISCARD_QUESTION);
        // Esc again backs out of the question, not out of the dialog.
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().add.is_some(), "{what}");
        assert!(h.query_by_label(DISCARD_QUESTION).is_none(), "{what}");
        h.key_press(egui::Key::Escape);
        h.run();
        click(&mut h, "Keep editing");
        assert!(h.state().add.is_some(), "{what}");
        assert!(h.query_by_label(DISCARD_QUESTION).is_none(), "{what}");
        h.key_press(egui::Key::Escape);
        h.run();
        click(&mut h, "Discard");
        assert!(h.state().add.is_none(), "{what}");
        assert!(duplicate_requested(&engine).is_empty(), "{what}");
    }
}

#[test]
fn the_discard_question_works_from_the_keyboard() {
    // From the review of #13: after Esc nothing had the keyboard, and Tab went into the paste box, which kept
    // it, so Discard and Keep editing could not be reached without a mouse.
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    let text = "claude mcp add x node";
    paste(&mut h, text);
    h.run();
    let typing_in = h.ctx.memory(|m| m.focused());
    assert!(typing_in.is_some(), "the paste box has the keyboard");
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    // The least destructive answer has the keyboard: Enter keeps editing.
    assert!(h.get_by_label("Keep editing").is_focused());
    h.key_press(egui::Key::Enter);
    h.run();
    h.run();
    assert!(h.query_by_label(DISCARD_QUESTION).is_none());
    assert_eq!(
        h.ctx.memory(|m| m.focused()),
        typing_in,
        "back where it was"
    );
    // One Tab back from Keep editing reaches Discard; Enter on it closes.
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    h.run();
    assert!(h.get_by_label("Discard").is_focused());
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().add.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn going_back_to_typing_puts_the_discard_question_away() {
    let (mut h, _engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(&mut h, "claude mcp add x node");
    h.key_press(egui::Key::Escape);
    h.run();
    h.get_by_label(DISCARD_QUESTION);
    h.event(egui::Event::Text("y".into()));
    h.run();
    assert!(h.query_by_label(DISCARD_QUESTION).is_none());
    h.get_by_label("Review");
    assert!(h.state().add.is_some());
}

#[test]
fn cancel_closes_a_filled_in_add_dialog_at_once() {
    let (mut h, _engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(&mut h, "claude mcp add x node");
    click(&mut h, "Cancel");
    assert!(h.state().add.is_none());
}

#[test]
fn esc_in_the_review_of_an_add_goes_back_to_the_dialog() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
    paste(&mut h, text);
    click(&mut h, "Review");
    assert!(waits_behind_review(&h));
    assert_eq!(duplicate_requested(&engine).len(), 1);
    let mut plan = plan_view(9, &["+   \"files\""], &[], false);
    plan.action = "Add".to_owned();
    engine.events.send(Event::PlanReady(plan)).unwrap();
    h.run();
    h.get_by_label(&format!("{} Back", icon::ARROW_LEFT));
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().plan.is_none());
    assert_eq!(drain(&engine), vec![Command::Discard(9)]);
    let form = h.state().add.as_ref().expect("the Add dialog is kept");
    assert!(!form.reviewing);
    assert_eq!(form.text, text);
}

fn fields_json(fields: &ServerFields) -> String {
    fields.read("x").json
}

#[test]
fn a_server_can_be_filled_in_part_by_part() {
    let (mut h, engine) = duplicate_harness();
    fill_in(&mut h);
    h.get_by_label_contains("Add a server");
    assert!(!ready(&mut h));
    // Nothing is marked wrong on a form just opened.
    assert!(h.query_by_label_contains("Give it a name").is_none());
    add_form(&mut h).fields.command = r#"npx -y @x/fs "C:\My files""#.into();
    h.run();
    // The name follows the command until one is typed.
    assert_eq!(add_form(&mut h).name, "fs");
    h.get_by_label_contains("Adds fs for the user.");
    name_it(&mut h, "files");
    add_form(&mut h).fields.command = r#"npx -y @x/fs "C:\My files""#.into();
    h.run();
    h.get_by_label_contains("Add variable").click();
    h.run();
    add_form(&mut h).fields.env[0] = ("ROOT".into(), "/srv".into());
    h.run();
    h.get_by_label_contains("Adds files for the user.");
    click(&mut h, "Review");
    assert!(waits_behind_review(&h));
    let mut fields = ServerFields {
        command: r#"npx -y @x/fs "C:\My files""#.into(),
        ..ServerFields::default()
    };
    fields.env.push(("ROOT".into(), "/srv".into()));
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Add {
            name: "files".into(),
            json: fields_json(&fields),
            targets: vec![Target::User],
            secret: None,
        }]
    );
}

#[test]
fn an_oauth_server_is_added_with_its_client_secret_typed_hidden() {
    let (mut h, engine) = duplicate_harness();
    fill_in(&mut h);
    click(&mut h, "HTTP");
    assert!(h.query_by_label_contains("Client ID").is_none());
    h.get_by_label(OAUTH_TOGGLE).click();
    h.run();
    h.get_by_label_contains("Client secret");
    name_it(&mut h, "sso");
    let f = add_form(&mut h);
    f.fields.url = "https://sso.example.com/mcp".into();
    f.fields.client_id = "client-7".into();
    f.fields.callback_port = "8123".into();
    *f.secret.edit() = "Typed-S3cret".into();
    h.run();
    // The secret is never drawn.
    assert!(h.query_by_label_contains("Typed-S3cret").is_none());
    click(&mut h, "Review");
    let fields = ServerFields {
        transport: TransportKind::Http,
        url: "https://sso.example.com/mcp".into(),
        oauth: true,
        client_id: "client-7".into(),
        callback_port: "8123".into(),
        ..ServerFields::default()
    };
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Add {
            name: "sso".into(),
            json: fields_json(&fields),
            targets: vec![Target::User],
            secret: Some(Secret::new("Typed-S3cret".into())),
        }]
    );
}

#[test]
fn an_empty_client_secret_is_none_and_a_secret_is_only_sent_for_oauth() {
    let (mut h, engine) = duplicate_harness();
    fill_in(&mut h);
    name_it(&mut h, "sso");
    let f = add_form(&mut h);
    f.fields.transport = TransportKind::Http;
    f.fields.url = "https://sso.example.com/mcp".into();
    f.fields.oauth = true;
    h.run();
    click(&mut h, "Review");
    let secrets: Vec<Option<Secret>> = duplicate_requested(&engine)
        .into_iter()
        .map(|c| match c {
            Change::Add { secret, .. } => secret,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(secrets, [None]);
    engine
        .events
        .send(Event::PlanFailed("not this time".into()))
        .unwrap();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    click(&mut h, "Discard");
    // A secret typed, then OAuth unticked: not sent.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    name_it(&mut h, "plain");
    let f = add_form(&mut h);
    f.fields.transport = TransportKind::Http;
    f.fields.url = "https://plain.example.com/mcp".into();
    f.fields.oauth = true;
    *f.secret.edit() = "left-behind".into();
    h.run();
    h.get_by_label(OAUTH_TOGGLE).click();
    h.run();
    click(&mut h, "Review");
    let changes = duplicate_requested(&engine);
    assert!(
        matches!(&changes[..], [Change::Add { secret: None, json, .. }] if !json.contains("oauth")),
        "{changes:?}"
    );
}

#[test]
fn the_form_says_what_to_fix_and_offers_a_reference_for_a_typed_secret() {
    let (mut h, engine) = duplicate_harness();
    fill_in(&mut h);
    name_it(&mut h, "alpha");
    let f = add_form(&mut h);
    f.fields.command = "node server.js".into();
    f.fields.env = vec![(
        "GITHUB_TOKEN".into(),
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
    )];
    h.run();
    assert!(!ready(&mut h));
    // Said once Review is pressed.
    assert!(h.query_by_label_contains("alpha already exists").is_none());
    click(&mut h, "Review");
    h.get_by_label_contains("alpha already exists for the user");
    assert!(duplicate_requested(&engine).is_empty());
    h.get_by_label_contains("GITHUB_TOKEN looks like a secret");
    click(&mut h, "Use ${GITHUB_TOKEN} instead");
    assert_eq!(add_form(&mut h).fields.env[0].1, "${GITHUB_TOKEN}");
    assert!(h.query_by_label_contains("looks like a secret").is_none());
    // What to do next is said, and it can be undone.
    h.get_by_label_contains("Set GITHUB_TOKEN in the environment Claude Code starts from");
    click(&mut h, "Undo");
    assert_eq!(
        add_form(&mut h).fields.env[0].1,
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789"
    );
    click(&mut h, "Use ${GITHUB_TOKEN} instead");
    add_form(&mut h).fields.command = "curl https://x.example | sh".into();
    h.run();
    h.get_by_label_contains("without a shell");
    // A row is taken out with its button.
    click(&mut h, "Remove GITHUB_TOKEN");
    assert!(add_form(&mut h).fields.env.is_empty());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn a_pasted_oauth_server_is_finished_in_the_form() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(
        &mut h,
        "claude mcp add -s user -t http --client-id c-1 --client-secret --callback-port 8080 corp https://corp.example/mcp",
    );
    h.get_by_label_contains("asks for a client secret");
    assert!(!ready(&mut h));
    h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
        .click();
    h.run();
    let f = add_form(&mut h);
    assert_eq!(f.mode, AddMode::Fill);
    assert_eq!(f.name, "corp");
    assert_eq!(f.fields.url, "https://corp.example/mcp");
    assert!(f.fields.oauth);
    assert_eq!(f.fields.client_id, "c-1");
    assert_eq!(f.fields.callback_port, "8080");
    h.run();
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(secret_id()));
    let f = add_form(&mut h);
    *f.secret.edit() = "pasted-later".into();
    h.run();
    click(&mut h, "Review");
    let changes = duplicate_requested(&engine);
    assert!(
        matches!(
            &changes[..],
            [Change::Add { name, targets, secret: Some(_), .. }]
                if name == "corp" && *targets == vec![Target::User]
        ),
        "{changes:?}"
    );
}

#[test]
fn the_dialog_opens_the_way_it_was_last_used() {
    let (mut h, _engine) = duplicate_harness();
    fill_in(&mut h);
    h.key_press(egui::Key::Escape);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    assert_eq!(h.state().add.as_ref().unwrap().mode, AddMode::Fill);
    click(&mut h, "Paste");
    h.key_press(egui::Key::Escape);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    assert_eq!(h.state().add.as_ref().unwrap().mode, AddMode::Paste);
}

#[test]
fn what_was_added_is_selected_once_it_is_in_the_list() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(&mut h, "claude mcp add -s user files -- npx -y @x/fs");
    h.state_mut().add.as_mut().unwrap().places.projects[1].1 = true;
    h.state_mut().add.as_mut().unwrap().places_chosen = true;
    h.run();
    click(&mut h, "Review");
    drain(&engine);
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ files"], &[], false)))
        .unwrap();
    h.run();
    // The search hides it; it is cleared so what was added can be seen.
    h.state_mut().search = "alpha".into();
    let (a, b) = two_rows();
    let rows = vec![
        a,
        b,
        live_row("files", Scope::User, None),
        live_row("files", Scope::Local, Some("/home/me/notes")),
        live_row("files", Scope::Local, Some("C:/Repos/WebApp")),
    ];
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
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
    // For the user and in notes, not the WebApp one that was there already.
    assert_eq!(h.state().selected, BTreeSet::from([2, 3]));
    assert_eq!(h.state().search, "");
}

#[test]
fn nothing_is_selected_for_an_add_that_was_cancelled() {
    let (mut h, engine) = duplicate_harness();
    click(&mut h, "beta");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(&mut h, "claude mcp add -s user alpha2 node");
    click(&mut h, "Review");
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ alpha2"], &[], false)))
        .unwrap();
    h.run();
    click(&mut h, "Cancel");
    // Another change, saved: the cancelled add selects nothing.
    let (a, b) = two_rows();
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a, b, live_row("alpha2", Scope::User, None)],
            projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
            ..Snapshot::default()
        })))
        .unwrap();
    engine
        .events
        .send(Event::Applied {
            plan: 8,
            changed: true,
            backup: None,
            notes: Vec::new(),
        })
        .unwrap();
    h.run();
    assert_eq!(h.state().selected, BTreeSet::from([1]));
}

/// What the form holds, to compare before and after a round trip through review.
fn form_state(h: &mut Harness<'static, ShellApp>) -> (String, ServerFields, Secret) {
    let f = add_form(h);
    (f.name.clone(), f.fields.clone(), f.secret.clone())
}

#[test]
fn a_review_that_fails_keeps_the_form_and_says_why_in_it() {
    let (mut h, engine) = duplicate_harness();
    sso_filled(&mut h);
    let before = form_state(&mut h);
    click(&mut h, "Review");
    assert!(waits_behind_review(&h));
    assert!(
        h.query_by_label("Fill in").is_none(),
        "hidden behind the review"
    );
    drain(&engine);
    engine
        .events
        .send(Event::PlanFailed(
            "The system's secret store is not available (no Secret Service).".into(),
        ))
        .unwrap();
    h.run();
    // The dialog is back as it was, saying why, and no toast says it elsewhere.
    assert_eq!(form_state(&mut h), before);
    assert!(!waits_behind_review(&h));
    h.get_by_label_contains("secret store is not available");
    assert!(h.state().outcome.is_none());
    // Fixed and sent again: the message goes.
    click(&mut h, "Review");
    assert_eq!(duplicate_requested(&engine).len(), 1);
    assert!(add_form(&mut h).failed.is_none());
    // Claude Code refusing the add, after review, also comes back to the form.
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
        .unwrap();
    h.run();
    engine
        .events
        .send(Event::ApplyFailed {
            plan: 7,
            error: "sso was not added for the user. Claude Code said: no".into(),
        })
        .unwrap();
    h.run();
    assert_eq!(form_state(&mut h), before);
    h.get_by_label_contains("Claude Code said: no");
}

#[test]
fn the_review_of_an_add_goes_back_to_the_form_as_it_was() {
    let (mut h, engine) = duplicate_harness();
    sso_filled(&mut h);
    let before = form_state(&mut h);
    click(&mut h, "Review");
    drain(&engine);
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
        .unwrap();
    h.run();
    h.get_by_label(&format!("{} Back", icon::ARROW_LEFT))
        .click();
    h.run();
    assert!(h.state().plan.is_none());
    assert!(matches!(&drain(&engine)[..], [Command::Discard(7)]));
    assert_eq!(form_state(&mut h), before);
    h.get_by_label("Fill in");
    // Cancel in the review drops the add as a whole.
    click(&mut h, "Review");
    engine
        .events
        .send(Event::PlanReady(plan_view(8, &["+ sso"], &[], false)))
        .unwrap();
    h.run();
    click(&mut h, "Cancel");
    assert!(h.state().add.is_none());
    // Other reviews have no Back.
    click(&mut h, "beta");
    h.key_press(egui::Key::Delete);
    h.run();
    engine
        .events
        .send(Event::PlanReady(plan_view(9, &["- beta"], &[], true)))
        .unwrap();
    h.run();
    assert!(
        h.query_by_label(&format!("{} Back", icon::ARROW_LEFT))
            .is_none()
    );
}

#[test]
fn review_pressed_too_soon_says_what_is_missing_under_each_field_and_goes_there() {
    let (mut h, engine) = duplicate_harness();
    fill_in(&mut h);
    click(&mut h, "HTTP");
    let f = add_form(&mut h);
    f.fields.headers = vec![(String::new(), "1".into())];
    h.run();
    assert!(
        h.query_by_label_contains("Type the server's address")
            .is_none()
    );
    click(&mut h, "Review");
    h.run();
    assert!(duplicate_requested(&engine).is_empty());
    // Each under its field, all at once.
    h.get_by_label_contains("Type the server's address");
    h.get_by_label_contains("Give it a name");
    h.get_by_label_contains("A header has a value but no name");
    h.get_by_label_contains("Fix what is marked above");
    assert_eq!(
        h.ctx.memory(|m| m.focused()),
        Some(field_id(Field::Part(Part::Address)))
    );
    // A message goes as soon as what it says is fixed.
    add_form(&mut h).fields.headers.clear();
    h.run();
    assert!(h.query_by_label_contains("A header has a value").is_none());
}

#[test]
fn an_address_is_checked_when_it_is_left_and_https_is_added_for_the_user() {
    let (mut h, _engine) = duplicate_harness();
    fill_in(&mut h);
    click(&mut h, "HTTP");
    let address = field_id(Field::Part(Part::Address));
    h.ctx.memory_mut(|m| m.request_focus(address));
    h.run();
    add_form(&mut h).fields.url = "not a".into();
    h.run();
    // Still typing: not wrong yet.
    assert!(h.query_by_label_contains("Start the address").is_none());
    h.key_press(egui::Key::Tab);
    h.run();
    h.get_by_label_contains("Start the address with https://");
    // Typed without the scheme, it is completed when left, as a browser does.
    h.ctx.memory_mut(|m| m.request_focus(address));
    h.run();
    add_form(&mut h).fields.url = "mcp.linear.app/mcp".into();
    h.run();
    h.key_press(egui::Key::Tab);
    h.run();
    let f = add_form(&mut h);
    assert_eq!(f.fields.url, "https://mcp.linear.app/mcp");
    assert_eq!(f.name, "linear");
    h.run();
    assert!(h.query_by_label_contains("Start the address").is_none());
}

#[test]
fn a_callback_port_another_program_holds_is_warned_about_once_left_and_does_not_stop_review() {
    let (mut h, engine) = duplicate_harness();
    // The engine checks the port, off the UI thread; here the test answers for it.
    let answer = |h: &mut Harness<'static, ShellApp>, port: u16, held: bool| {
        engine
            .events
            .send(klefki_core::api::Event::CallbackPort {
                port,
                warning: held.then(|| format!("Port {port} is in use by another program.")),
            })
            .unwrap();
        h.run();
    };
    let asked = |engine: &klefki_core::api::Detached| -> Vec<u16> {
        drain(engine)
            .into_iter()
            .filter_map(|c| match c {
                Command::CheckCallbackPort(port) => Some(port),
                _ => None,
            })
            .collect()
    };
    fill_in(&mut h);
    click(&mut h, "HTTP");
    let f = add_form(&mut h);
    f.fields.url = "https://sso.example.com/mcp".into();
    f.fields.oauth = true;
    h.run();
    let port = field_id(Field::Part(Part::CallbackPort));
    h.ctx.memory_mut(|m| m.request_focus(port));
    h.run();
    add_form(&mut h).fields.callback_port = "8766".into();
    h.run();
    // Not checked while it is typed in.
    assert_eq!(asked(&engine), Vec::<u16>::new());
    h.key_press(egui::Key::Tab);
    h.run();
    assert_eq!(asked(&engine), [8766]);
    answer(&mut h, 8766, true);
    h.get_by_label_contains("Port 8766 is in use by another program.");
    // A warning, not a problem: the port is the one the provider registered, and it may be free by sign-in.
    assert!(ready(&mut h));
    // Changed, the old answer is not shown; a free port says nothing, and a late answer for another port is not
    // taken for this one.
    h.ctx.memory_mut(|m| m.request_focus(port));
    h.run();
    add_form(&mut h).fields.callback_port = "8767".into();
    h.run();
    assert!(
        h.query_by_label_contains("in use by another program")
            .is_none()
    );
    h.key_press(egui::Key::Tab);
    h.run();
    assert_eq!(asked(&engine), [8767]);
    answer(&mut h, 8766, true);
    assert!(
        h.query_by_label_contains("in use by another program")
            .is_none()
    );
    answer(&mut h, 8767, false);
    assert!(
        h.query_by_label_contains("in use by another program")
            .is_none()
    );
    // Pressing Review checks again: the port may have been taken since.
    name_it(&mut h, "");
    h.run();
    click(&mut h, "Review");
    h.get_by_label_contains("Give it a name");
    assert_eq!(asked(&engine), [8767]);
    answer(&mut h, 8767, true);
    h.get_by_label_contains("Port 8767 is in use by another program.");
    // A wrong port is a problem, said instead, and not checked.
    add_form(&mut h).fields.callback_port = "99999".into();
    h.run();
    h.get_by_label_contains("must be a number from 1 to 65535");
    assert!(
        h.query_by_label_contains("in use by another program")
            .is_none()
    );
    h.ctx.memory_mut(|m| m.request_focus(port));
    h.run();
    h.key_press(egui::Key::Tab);
    h.run();
    assert_eq!(asked(&engine), Vec::<u16>::new());
}

#[test]
fn a_pasted_oauth_server_without_a_client_secret_is_added_as_pasted() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    let text = "claude mcp add -s user -t http --client-id c-1 corp https://corp.example/mcp";
    paste(&mut h, text);
    assert!(
        h.query_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
            .is_none()
    );
    h.get_by_label_contains("Adds corp for the user.");
    click(&mut h, "Review");
    assert_eq!(
        duplicate_requested(&engine),
        vec![Change::Add {
            name: "corp".into(),
            json: pasted_json(text, 0),
            targets: vec![Target::User],
            secret: None,
        }]
    );
}

#[test]
fn a_pasted_server_asking_for_a_secret_is_finished_in_the_form_or_said_why_not() {
    let (mut h, _engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    // The sign-in metadata address is a field of the form too.
    paste(
        &mut h,
        r#"claude mcp add-json -s user m --client-secret '{"type":"http","url":"https://m.example/mcp","oauth":{"authServerMetadataUrl":"https://id.example/meta"}}'"#,
    );
    h.get_by_label_contains("1 needs finishing in the form");
    h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
        .click();
    h.run();
    let f = add_form(&mut h);
    assert_eq!(f.mode, AddMode::Fill);
    assert_eq!(f.fields.metadata_url, "https://id.example/meta");
    h.run();
    h.get_by_label_contains("Metadata URL");
    // What the form cannot hold is not offered to it.
    click(&mut h, "Paste");
    paste(
        &mut h,
        r#"claude mcp add-json -s user m --client-secret '{"type":"http","url":"https://m.example/mcp","headersHelper":"get-token"}'"#,
    );
    assert!(
        h.query_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
            .is_none()
    );
    h.get_by_label_contains("Add it with Claude Code's own command");
    h.get_by_label_contains("1 server cannot be added");
}

#[test]
fn a_secret_option_followed_by_the_name_is_finished_in_the_form_with_the_name_typed_there() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(
        &mut h,
        "claude mcp add -s user --transport http --client-id abc --client-secret corp https://mcp.corp.example/mcp",
    );
    // Said once, with what to do.
    h.get_by_label_contains("may be the secret, and the name with it. Fill it in");
    assert_eq!(h.query_all_by_label_contains("--client-secret").count(), 1);
    h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
        .click();
    h.run();
    let f = add_form(&mut h);
    assert_eq!(f.mode, AddMode::Fill);
    assert_eq!(f.fields.url, "https://mcp.corp.example/mcp");
    assert_eq!(f.fields.client_id, "abc");
    // The name comes from the address, like a typed one, until it is typed.
    assert_eq!(f.name, "corp");
    assert!(!f.name_edited);
    // The keyboard is where it is asked for: in the secret.
    h.run();
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(secret_id()));
    let f = add_form(&mut h);
    *f.secret.edit() = "typed-here".into();
    h.run();
    click(&mut h, "Review");
    let changes = duplicate_requested(&engine);
    assert!(
        matches!(&changes[..], [Change::Add { name, secret: Some(_), .. }] if name == "corp"),
        "{changes:?}"
    );
}

#[test]
fn the_client_secret_can_be_shown_and_is_forgotten_with_the_dialog() {
    let (mut h, _engine) = duplicate_harness();
    sso_filled(&mut h);
    assert!(!add_form(&mut h).show_secret);
    click(&mut h, "Show the client secret");
    assert!(add_form(&mut h).show_secret);
    click(&mut h, "Hide the client secret");
    // Typed for real, so egui keeps undo steps of it.
    h.ctx.memory_mut(|m| m.request_focus(secret_id()));
    h.run();
    h.event(egui::Event::Text("x".into()));
    h.run();
    assert!(add_form(&mut h).secret.edit().ends_with('x'));
    let remembered = |h: &Harness<'static, ShellApp>| {
        h.ctx.data_mut(|d| {
            d.get_persisted::<egui::text_edit::TextEditState>(secret_id())
                .is_some()
        })
    };
    assert!(remembered(&h));
    click(&mut h, "Cancel");
    assert!(h.state().add.is_none());
    assert!(!remembered(&h));
}

#[test]
fn a_value_can_be_shown_to_check_it() {
    let (mut h, _engine) = duplicate_harness();
    fill_in(&mut h);
    add_form(&mut h).fields.env = vec![("TEAM".into(), "engineering".into())];
    h.run();
    click(&mut h, "Show TEAM");
    assert!(add_form(&mut h).rows.shown.contains(&0));
    click(&mut h, "Hide TEAM");
    assert!(add_form(&mut h).rows.shown.is_empty());
}

#[test]
fn what_was_added_is_selected_however_its_project_is_spelled() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(&mut h, "claude mcp add files -- npx -y @x/fs");
    let f = add_form(&mut h);
    f.places.user = false;
    f.places.projects[0].1 = true;
    f.places_chosen = true;
    h.run();
    h.get_by_label_contains("Adds files in WebApp.");
    click(&mut h, "Review");
    drain(&engine);
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ files"], &[], false)))
        .unwrap();
    h.run();
    let (a, b) = two_rows();
    // Claude Code wrote the key with a small drive letter and backslashes.
    let rows = vec![
        a,
        b,
        live_row("files", Scope::Local, Some(r"c:\Repos\WebApp")),
    ];
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
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
    assert_eq!(h.state().selected, BTreeSet::from([2]));
    h.get_by_label("Added files in WebApp.");
    // And a name taken under another spelling is taken.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    paste(&mut h, "claude mcp add files -- npx -y @x/fs");
    let f = add_form(&mut h);
    f.places.user = false;
    f.places.projects[0].1 = true;
    f.places_chosen = true;
    h.run();
    h.get_by_label_contains("files already exists in WebApp");
}

#[test]
fn an_oauth_add_says_to_sign_in_once_it_is_added() {
    let (mut h, engine) = duplicate_harness();
    sso_filled(&mut h);
    click(&mut h, "Review");
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
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
    assert!(h.state().add.is_none());
    h.get_by_label("Added sso for the user. Sign in to it from its Status tab.");
}

#[test]
fn the_oauth_settings_come_into_view_in_a_small_window() {
    let (mut h, _engine) = duplicate_harness();
    h.set_size(egui::vec2(960.0, 600.0));
    h.run();
    fill_in(&mut h);
    click(&mut h, "HTTP");
    h.get_by_label(OAUTH_TOGGLE).click();
    for _ in 0..4 {
        h.run();
    }
    // In the part of the body in view: between the tabs and the line above the buttons.
    let top = h.get_by_label("Paste").rect().bottom();
    let bottom = h.get_by_label("Review").rect().top();
    for label in ["Client ID", "Client secret", "Callback port"] {
        let r = h.get_by_label(label).rect();
        assert!(
            r.top() >= top && r.bottom() <= bottom,
            "{label} at {r:?}, view {top}..{bottom}"
        );
    }
}

#[test]
fn in_a_small_window_the_add_body_takes_the_height_the_window_has() {
    for (mode, failed) in [("Paste", false), ("Fill in", false), ("Fill in", true)] {
        let (mut h, _engine) = duplicate_harness_with_many_projects();
        h.set_size(egui::vec2(960.0, 600.0));
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        h.get_by_label(mode).click();
        h.run();
        if failed {
            add_form(&mut h).failed = Some("The secret store is locked. ".repeat(12));
            // Already in the first frame that says it, not one later.
            h.step();
            let bottom = h.get_by_label("Review").rect().bottom();
            assert!(
                bottom <= h.ctx.content_rect().bottom(),
                "the buttons end at {bottom} as the failure shows"
            );
        }
        for _ in 0..4 {
            h.run();
        }
        // The places list is longer than the window, so the body is as tall as it may be: the buttons end near
        // the bottom of the window, with the same margin as above the dialog, not far above it.
        let window = h.ctx.content_rect();
        let top = h.get_by_label("Paste").rect().top();
        let bottom = h.get_by_label("Review").rect().bottom();
        let below = window.bottom() - bottom;
        assert!(
            below >= 0.0,
            "{mode}: the buttons end at {bottom}, out of the window"
        );
        assert!(
            below <= top + 24.0,
            "{mode}: {below} left under the buttons, {top} above the dialog"
        );
    }
}

#[test]
fn the_wheel_over_the_places_scrolls_the_add_dialog_as_one() {
    let (mut h, _engine) = duplicate_harness_with_many_projects();
    h.set_size(egui::vec2(960.0, 600.0));
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    h.run();
    // Over the list of projects, the wheel moves the dialog's body, not a list of its own inside it.
    let heading = h.get_by_label_contains("just for you (42").rect();
    let over = h.get_by_label("project-01").rect().center();
    h.event(egui::Event::PointerMoved(over));
    h.run();
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -120.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    });
    for _ in 0..30 {
        h.step();
    }
    let moved = h.get_by_label_contains("just for you (42").rect();
    assert!(
        moved.top() < heading.top() - 20.0,
        "the heading was at {}, now {}",
        heading.top(),
        moved.top()
    );
}

#[test]
fn a_pasted_server_to_finish_does_not_widen_the_dialog() {
    for text in [
        "claude mcp add --transport http --client-id abc --client-secret corp https://mcp.corp.example/mcp",
        "claude mcp add -t http --client-id c-1 --client-secret --callback-port 8080 corp https://corp.example/mcp",
    ] {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let tabs = h.get_by_label("Paste").rect();
        paste(&mut h, text);
        h.run();
        h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE));
        assert_eq!(h.get_by_label("Paste").rect(), tabs, "{text}");
    }
}

#[test]
fn a_failure_in_the_add_dialog_says_the_plain_part_first_and_the_detail_apart() {
    let (mut h, _engine) = duplicate_harness();
    sso_filled(&mut h);
    add_form(&mut h).failed = Some(
        "Klefki can't use your system's secret store. Press Review again.\nThe system's secret store is not available (org.freedesktop.DBus.Error.ServiceUnknown)."
            .into(),
    );
    h.run();
    let plain = h
        .get_by_label("Klefki can't use your system's secret store. Press Review again.")
        .rect();
    let detail = h
        .get_by_label(
            "The system's secret store is not available (org.freedesktop.DBus.Error.ServiceUnknown).",
        )
        .rect();
    assert!(detail.top() >= plain.bottom(), "{plain:?} then {detail:?}");
    // A failure of one line is said as it is.
    add_form(&mut h).failed = Some("Claude Code refused it.".into());
    h.run();
    h.get_by_label("Claude Code refused it.");
}

#[test]
fn the_client_secret_toggle_says_in_words_what_it_does() {
    let (mut h, _engine) = duplicate_harness();
    fill_in(&mut h);
    click(&mut h, "HTTP");
    h.get_by_label(OAUTH_TOGGLE).click();
    h.run();
    h.run();
    // Much wider than tall, as the word stands next to the eye (the eye alone is about twice as wide as
    // tall); the name read out starts with that word.
    let tall = h.get_by_label("Show the client secret").rect().height();
    let show = h.get_by_label("Show the client secret").rect().width();
    assert!(show > 3.0 * tall, "{show} wide, {tall} tall");
    click(&mut h, "Show the client secret");
    let hide = h.get_by_label("Hide the client secret").rect().width();
    assert!(hide > 3.0 * tall, "{hide} wide, {tall} tall");
}

#[test]
fn the_tabs_stay_under_the_pointer_as_the_dialog_changes() {
    let (mut h, _engine) = duplicate_harness();
    fill_in(&mut h);
    h.run();
    let tabs = h.get_by_label("Paste").rect();
    click(&mut h, "HTTP");
    h.get_by_label(OAUTH_TOGGLE).click();
    h.run();
    h.run();
    assert_eq!(h.get_by_label("Paste").rect(), tabs);
    click(&mut h, "Paste");
    h.run();
    assert_eq!(h.get_by_label("Paste").rect(), tabs);
}

/// The Import dialog, open on `text` as read from `mcp-servers.json`.
fn import_open(text: &str) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = duplicate_harness();
    press(&mut h, Tool::Import);
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseImportFile { shown: None }]
    );
    engine
        .events
        .send(Event::ImportFile(ImportFile::Read {
            shown: "mcp-servers.json".into(),
            text: text.into(),
        }))
        .unwrap();
    h.run();
    h.run();
    (h, engine)
}

#[test]
fn import_shows_what_a_file_holds_and_adds_it_as_one_plan() {
    let (mut h, engine) = import_open(IMPORTED);
    h.get_by_label_contains("Import servers");
    h.get_by_label_contains("From mcp-servers.json");
    // Each server, what it runs or connects to, and what must be set where Claude Code runs.
    h.get_by_label_contains("https://docs.example.com/mcp");
    h.get_by_label_contains("AUTH_TOKEN");
    // alpha is there already: kept unless asked otherwise, and said so.
    h.get_by_label_contains("alpha already exists for the user.");
    h.get_by_label_contains("Adds docs and notion for the user.");
    click(&mut h, "Review");
    assert!(waits_behind_review(&h));
    assert_eq!(
        duplicate_requested(&engine),
        vec![
            Change::Add {
                name: "docs".into(),
                json: pasted_json(IMPORTED, 1),
                targets: vec![Target::User],
                secret: None,
            },
            Change::Add {
                name: "notion".into(),
                json: pasted_json(IMPORTED, 2),
                targets: vec![Target::User],
                secret: None,
            },
        ]
    );
}

#[test]
fn a_server_that_exists_is_kept_replaced_or_imported_under_another_name() {
    let (mut h, engine) = import_open(IMPORTED);
    click(&mut h, "Replace it");
    h.get_by_label_contains("Replaces alpha for the user.");
    click(&mut h, "Review");
    let alpha = live_row("alpha", Scope::User, None).id();
    let changes = duplicate_requested(&engine);
    assert!(
        changes.contains(&Change::Replace {
            id: alpha,
            json: pasted_json(IMPORTED, 0)
        }),
        "{changes:?}"
    );
    assert_eq!(changes.len(), 3, "{changes:?}");
    // Once done, what was imported is selected, replaced ones included, and the message says so.
    engine
        .events
        .send(Event::PlanReady(plan_view(7, &["+ docs"], &[], false)))
        .unwrap();
    h.run();
    let mut rows = h.state().snapshot.servers.clone();
    rows.push(live_row("docs", Scope::User, None));
    rows.push(live_row("notion", Scope::User, None));
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
    assert!(h.state().add.is_none());
    assert_eq!(h.state().selected, BTreeSet::from([0, 3, 4]));
    h.get_by_label_contains("Imported docs, notion and alpha for the user.");

    // Nearby: under a free name it is added like the others.
    let (mut h, engine) = import_open(IMPORTED);
    click(&mut h, "import it as alpha_2");
    h.get_by_label_contains("Adds alpha_2, docs and notion for the user.");
    click(&mut h, "Review");
    let changes = duplicate_requested(&engine);
    assert!(
        changes.iter().any(|c| matches!(
            c,
            Change::Add { name, targets, .. } if name == "alpha_2" && *targets == [Target::User]
        )),
        "{changes:?}"
    );

    // Kept where it is, it still goes where it is not: a project ticked as well.
    let (mut h, engine) = import_open(IMPORTED);
    click(&mut h, "notes");
    click(&mut h, "Review");
    let changes = duplicate_requested(&engine);
    assert!(
        changes.iter().any(|c| matches!(
            c,
            Change::Add { name, targets, .. }
                if name == "alpha" && *targets == [Target::Local("/home/me/notes".into())]
        )),
        "{changes:?}"
    );
    assert!(!changes.iter().any(|c| matches!(c, Change::Replace { .. })));

    // Nearby: when everything is kept, it says nothing would change, and Review asks for nothing.
    let (mut h, engine) = import_open(r#"{"mcpServers": {"alpha": {"command": "node"}}}"#);
    h.get_by_label_contains("nothing would change");
    click(&mut h, "Review");
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn each_oauth_server_asks_for_its_client_secret_and_only_it_gets_one() {
    let (mut h, engine) = import_open(IMPORTED);
    h.get_by_label_contains("Client secret of notion");
    assert!(h.query_by_label_contains("Client secret of docs").is_none());
    *h.state_mut().add.as_mut().unwrap().secrets[2].edit() = "s3cret".into();
    h.run();
    // Never shown as typed unless asked.
    assert!(h.query_by_label_contains("s3cret").is_none());
    click(&mut h, "Review");
    let changes = duplicate_requested(&engine);
    assert_eq!(
        changes.iter().find_map(|c| match c {
            Change::Add { name, secret, .. } if name == "notion" => Some(secret.clone()),
            _ => None,
        }),
        Some(Some(Secret::new("s3cret".into())))
    );
    assert!(changes.iter().any(|c| matches!(
        c,
        Change::Add { name, secret: None, .. } if name == "docs"
    )));

    // Nearby: a command that asks for a client secret is not imported until one is typed.
    let (mut h, engine) = import_open(
        "claude mcp add --transport http --client-id abc --client-secret -s user sso https://sso.example.com/mcp",
    );
    click(&mut h, "Review");
    assert!(duplicate_requested(&engine).is_empty());
    h.get_by_label_contains("Type the client secret it asks for");
    *h.state_mut().add.as_mut().unwrap().secrets[0].edit() = "x".into();
    h.run();
    click(&mut h, "Review");
    assert_eq!(duplicate_requested(&engine).len(), 1);
}

#[test]
fn a_server_unticked_is_left_out_and_nothing_ticked_imports_nothing() {
    let (mut h, engine) = import_open(IMPORTED);
    click(&mut h, "Import docs");
    h.get_by_label_contains("Adds notion for the user.");
    click(&mut h, "Import notion");
    click(&mut h, "Review");
    assert!(duplicate_requested(&engine).is_empty());
    // Nearby: one tick puts them all back.
    h.get_by_label_contains("All 3").click();
    h.run();
    h.get_by_label_contains("Adds docs and notion for the user.");
}

#[test]
fn the_palette_offers_import() {
    let (mut h, engine) = duplicate_harness();
    drain(&engine);
    ctrl_k(&mut h);
    type_into_palette(&mut h, "import");
    h.get_by_label_contains("Import servers from a file");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseImportFile { shown: None }]
    );
    // Nearby: while the dialog is open, another is not asked for.
    press(&mut h, Tool::Import);
    assert!(drain(&engine).is_empty());
    assert!(tool(&h, Tool::Import).accesskit_node().is_disabled());
}

#[test]
fn a_file_that_cannot_be_read_says_why_and_another_can_be_chosen() {
    let (mut h, engine) = import_open(IMPORTED);
    click(&mut h, "Choose another file…");
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseImportFile { shown: None }]
    );
    engine
        .events
        .send(Event::ImportFile(ImportFile::Failed {
            shown: "big.json".into(),
            why: "big.json is larger than 256 KiB.".into(),
        }))
        .unwrap();
    h.run();
    h.get_by_label_contains("big.json is larger than 256 KiB.");
    // Nearby: a file that holds no server says what it read instead, and Review adds nothing.
    engine
        .events
        .send(Event::ImportFile(ImportFile::Read {
            shown: "notes.txt".into(),
            text: "just some notes".into(),
        }))
        .unwrap();
    h.run();
    h.get_by_label_contains("From notes.txt");
    h.get_by_label_contains("Nothing here adds a server");
    click(&mut h, "Review");
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn without_a_file_dialog_a_path_is_typed_and_a_dropped_file_is_read() {
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseImportFile { shown: None }]
    );
    engine
        .events
        .send(Event::ImportFile(ImportFile::NoDialog))
        .unwrap();
    h.run();
    h.get_by_label_contains("Type the path of the file to import");
    // Quotes from a copied path are not part of it.
    let typed = whole("/home/me/servers.json");
    h.state_mut().add.as_mut().unwrap().path = format!("\"{typed}\"");
    h.run();
    click(&mut h, "Read");
    assert_eq!(drain(&engine), vec![Command::ReadImportFile(typed.into())]);
    // Nearby: a path that is not whole would be read from wherever Klefki was started, so it is refused.
    engine
        .events
        .send(Event::ImportFile(ImportFile::NoDialog))
        .unwrap();
    h.run();
    h.state_mut().add.as_mut().unwrap().path = "servers.json".into();
    h.run();
    click(&mut h, "Read");
    assert!(drain(&engine).is_empty());
    h.get_by_label_contains("Give the whole path, starting from the drive or /.");

    // A file dropped on the window is read for an import.
    let (mut h, engine) = duplicate_harness();
    /// A file dropped on the window: only its path is ever looked at.
    #[derive(Debug)]
    struct Dropped(std::path::PathBuf);
    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            panic!("the UI reads no file")
        }
    }
    let drop = |h: &mut Harness<'_, ShellApp>, paths: &[&str]| {
        for path in paths {
            h.input_mut()
                .dropped_files
                .push(Arc::new(Dropped(path.into())));
        }
        h.run();
    };
    drop(&mut h, &["/home/me/dropped.json"]);
    assert_eq!(
        drain(&engine),
        vec![Command::ReadImportFile("/home/me/dropped.json".into())]
    );
    // Nearby: more than one file, or a file dropped while something else is open, is not read, and says so.
    let (mut h, engine) = duplicate_harness();
    drop(&mut h, &["/home/me/a.json", "/home/me/b.json"]);
    assert!(drain(&engine).is_empty());
    h.get_by_label_contains("Drop one file at a time to import it.");
    let (mut h, engine) = duplicate_harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    assert!(h.state().add.is_some());
    drain(&engine);
    drop(&mut h, &["/home/me/dropped.json"]);
    assert!(drain(&engine).is_empty());
    h.get_by_label_contains("To import dropped.json, close what is open, then drop it again.");
}

#[test]
fn a_dialog_closed_without_a_file_leaves_a_way_to_type_the_path() {
    // Found in review: a desktop portal without a file chooser fails slowly, which looks like a cancel; without
    // a word, Import would seem to do nothing at all.
    let (mut h, engine) = duplicate_harness();
    press(&mut h, Tool::Import);
    drain(&engine);
    engine
        .events
        .send(Event::ImportFile(ImportFile::Cancelled))
        .unwrap();
    h.run();
    assert!(h.state().add.is_none());
    h.get_by_label_contains("No file chosen.");
    click(&mut h, "Type a path instead");
    let form = h.state().add.as_ref().expect("the Import dialog");
    assert_eq!(form.mode, AddMode::Import);
    h.get_by_label_contains("Type the path of the file to import");
    assert!(h.query_by_label_contains("No file chosen.").is_none());
    // The system's dialog can still be tried again from there.
    click(&mut h, "Choose a file…");
    assert_eq!(
        drain(&engine),
        vec![Command::ChooseImportFile { shown: None }]
    );

    // Nearby: closing the dialog asked for from the Import dialog leaves that dialog as it was, with no notice.
    let (mut h, engine) = import_open(IMPORTED);
    click(&mut h, "Choose another file…");
    drain(&engine);
    engine
        .events
        .send(Event::ImportFile(ImportFile::Cancelled))
        .unwrap();
    h.run();
    h.get_by_label_contains("From ");
    assert!(h.query_by_label_contains("No file chosen.").is_none());
}

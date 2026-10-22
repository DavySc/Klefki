//! The list seen from one project (slice S3 of the on/off plan): what Claude Code loads there, switched there only,
//! saved without a review when there is nothing to weigh, with Undo.

use super::*;
use egui::accesskit::Role;
use klefki_core::api::{Switch, Undo};

const A: &str = "/work/a";
const B: &str = "/work/b";

/// gw (a user server, off in b), loc (local to a) and other (local to b).
fn rows() -> Vec<ServerRow> {
    let mut gw = live_row("gw", Scope::User, None);
    gw.disabled_in = 1;
    gw.disabled_projects = vec![B.to_owned()];
    let mut other = live_row("other", Scope::Local, Some(B));
    other.enabled = false;
    vec![gw, live_row("loc", Scope::Local, Some(A)), other]
}

fn two_projects(h: &mut Harness<'static, ShellApp>, engine: &klefki_core::api::Detached) {
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows(),
            projects: vec![A.to_owned(), B.to_owned()],
            project_count: 2,
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
}

fn seen_from(h: &mut Harness<'static, ShellApp>, project: &str) {
    h.state_mut().look_in(Some(project.to_owned()));
    h.run();
}

fn switch_in(name: &str, scope: Scope, project: Option<&str>, place: &str, on: bool) -> Change {
    Change::SetEnabledIn {
        id: live_row(name, scope, project).id(),
        project: place.to_owned(),
        enabled: on,
    }
}

fn gw_off_in_a() -> Undo {
    Undo {
        switches: vec![Switch {
            project: A.into(),
            name: "gw".into(),
            off: true,
        }],
        records: Vec::new(),
    }
}

fn plan(id: u64, warnings: &[&str]) -> PlanView {
    let mut plan = plan_view(id, &["+ \"gw\""], warnings, false);
    plan.summary = vec!["Turn off gw in a".to_owned()];
    plan.action = "Turn off".to_owned();
    plan.restart = Some("Restart Claude Code sessions already open in a to pick it up.".to_owned());
    plan
}

fn ready(h: &mut Harness<'static, ShellApp>, engine: &klefki_core::api::Detached, plan: PlanView) {
    engine
        .events
        .send(klefki_core::api::Event::PlanReady(plan))
        .unwrap();
    h.run();
}

fn applied(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    plan: u64,
    undo: Option<Undo>,
) {
    engine
        .events
        .send(klefki_core::api::Event::Applied {
            plan,
            changed: true,
            backup: Some("/backups/claude.json.1".to_owned()),
            notes: Vec::new(),
            undo,
        })
        .unwrap();
    h.run();
}

#[test]
fn all_projects_is_the_list_as_before_and_one_project_shows_what_loads_there() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    h.get_by_role_and_label(Role::ComboBox, "Where");
    assert_eq!(h.state().lens, None);
    for name in ["gw", "loc", "other"] {
        h.get_by_label(name);
    }
    seen_from(&mut h, B);
    // A local server of another project never loads in b: not listed.
    assert!(h.query_by_label("loc").is_none());
    h.get_by_label("gw");
    h.get_by_label("other");
    // Off in b, though on in a: the list says how it stands here.
    assert_eq!(h.get_all_by_label("Off here").count(), 2);
    // Not the count of the projects it is off in: here is one project.
    assert!(h.query_by_label("off in 1/2").is_none());
    seen_from(&mut h, A);
    h.get_by_label("loc");
    assert!(h.query_by_label("other").is_none());
    assert!(h.query_by_label("Off here").is_none());
}

#[test]
fn the_where_list_offers_every_project_and_all_projects() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    h.get_by_role_and_label(Role::ComboBox, "Where").click();
    h.run();
    click(&mut h, "b/work");
    assert_eq!(h.state().lens.as_deref(), Some(B));
    assert!(h.query_by_label("loc").is_none());
    h.get_by_role_and_label(Role::ComboBox, "Where").click();
    h.run();
    click(&mut h, "All projects");
    assert_eq!(h.state().lens, None);
    h.get_by_label("loc");
}

#[test]
fn a_selection_that_does_not_load_there_is_let_go() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    click(&mut h, "loc");
    click(&mut h, "gw");
    h.state_mut().selected.insert(1);
    seen_from(&mut h, B);
    let selected: Vec<&str> = h
        .state()
        .selected
        .iter()
        .map(|&i| h.state().snapshot.servers[i].name.as_str())
        .collect();
    assert_eq!(selected, ["gw"]);
}

#[test]
fn a_switch_in_one_project_is_saved_without_a_review_and_offers_undo() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![switch_in(
            "gw",
            Scope::User,
            None,
            A,
            false
        )])]
    );
    ready(&mut h, &engine, plan(7, &[]));
    // Saved at once: no review to read and confirm.
    assert_eq!(drain(&engine), vec![Command::Apply(7)]);
    assert!(h.query_by_label("+ \"gw\"").is_none(), "no review shown");
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    let outcome = h.state().outcome.clone().unwrap();
    assert_eq!(
        outcome.text,
        "Turned off gw in a. Restart Claude Code sessions already open in a to pick it up."
    );
    assert!(outcome.undo.is_some());
    // The Undo does the same: straight back, no review.
    click(&mut h, "Undo");
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![Change::Undo(gw_off_in_a())])]
    );
    let mut back = plan(8, &[]);
    back.summary = vec!["Turn gw back on in a".to_owned()];
    back.action = "Undo".to_owned();
    ready(&mut h, &engine, back);
    assert_eq!(drain(&engine), vec![Command::Apply(8)]);
    applied(&mut h, &engine, 8, None);
    assert!(
        h.state()
            .outcome
            .as_ref()
            .is_some_and(|o| o.text.starts_with("Turned gw back on in a.")),
        "{:?}",
        h.state().outcome
    );
}

#[test]
fn a_switch_in_one_project_with_something_to_weigh_is_reviewed() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOn);
    drain(&engine);
    ready(
        &mut h,
        &engine,
        plan(7, &["gw will still not load in a: policy keeps it out."]),
    );
    assert_eq!(drain(&engine), vec![]);
    h.get_by_label_contains("gw will still not load in a");
    // Confirmed as any review; the outcome then says what it did all the same.
    h.get_by_role_and_label(Role::Button, "Turn off").click();
    h.run();
    assert_eq!(drain(&engine), vec![Command::Apply(7)]);
}

#[test]
fn a_switch_in_one_project_that_changes_nothing_says_so() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, B);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    drain(&engine);
    let mut nothing = plan_view(7, &[], &[], true);
    nothing.already = vec!["gw is already off in b.".to_owned()];
    ready(&mut h, &engine, nothing);
    assert_eq!(drain(&engine), vec![Command::Discard(7)]);
    assert_eq!(
        h.state().outcome.as_ref().map(|o| o.text.as_str()),
        Some("gw is already off in b.")
    );
}

#[test]
fn space_switches_the_selection_where_the_list_is_seen_from() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    // In b, gw is off: Space turns it on there.
    seen_from(&mut h, B);
    click(&mut h, "gw");
    h.key_press(egui::Key::Space);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![switch_in(
            "gw",
            Scope::User,
            None,
            B,
            true
        )])]
    );
    ready(&mut h, &engine, plan(7, &[]));
    assert_eq!(drain(&engine), vec![Command::Apply(7)]);
    applied(&mut h, &engine, 7, None);
    // In every project, Space goes through the review as the toolbar does: gw is on (somewhere), so off.
    h.state_mut().look_in(None);
    h.run();
    h.key_press(egui::Key::Space);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![Change::SetEnabled {
            id: live_row("gw", Scope::User, None).id(),
            enabled: false
        }])]
    );
    ready(&mut h, &engine, plan(8, &[]));
    assert_eq!(drain(&engine), vec![], "reviewed first");
}

#[test]
fn space_with_some_on_and_some_off_turns_them_all_on() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, B);
    click(&mut h, "gw");
    h.state_mut().selected.insert(2);
    h.key_press(egui::Key::Space);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![
            switch_in("gw", Scope::User, None, B, true),
            switch_in("other", Scope::Local, Some(B), B, true),
        ])]
    );
}

#[test]
fn ctrl_z_undoes_the_last_change_that_offers_undo() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    drain(&engine);
    ready(&mut h, &engine, plan(7, &[]));
    drain(&engine);
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![Change::Undo(gw_off_in_a())])]
    );
}

#[test]
fn ctrl_z_after_a_reviewed_change_opens_the_undos_review() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    drain(&engine);
    ready(&mut h, &engine, plan(7, &[]));
    h.get_by_role_and_label(Role::Button, "Turn off").click();
    h.run();
    assert_eq!(drain(&engine), vec![Command::Apply(7)]);
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    // A reviewed change's Undo is reviewed too.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(
        drain(&engine),
        vec![Command::Prepare(vec![Change::Undo(gw_off_in_a())])]
    );
    ready(&mut h, &engine, plan(8, &[]));
    assert_eq!(drain(&engine), vec![], "reviewed first");
}

#[test]
fn ctrl_z_in_a_text_field_is_the_fields_own() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    drain(&engine);
    ready(&mut h, &engine, plan(7, &[]));
    drain(&engine);
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(drain(&engine), vec![]);
    assert!(h.state().outcome.as_ref().is_some_and(|o| o.undo.is_some()));
}

#[test]
fn a_project_claude_code_forgets_returns_the_list_to_all_projects() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, B);
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows(),
            projects: vec![A.to_owned()],
            project_count: 1,
            ..Snapshot::default()
        })))
        .unwrap();
    h.run();
    assert_eq!(h.state().lens, None);
    h.get_by_label("loc");
}

#[test]
fn the_details_and_the_palette_say_which_project() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, B);
    click(&mut h, "gw");
    h.get_by_label("In b");
    // Off here; across projects it is still off in one of two.
    assert_eq!(h.get_all_by_label("off").count(), 1);
    let offered: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
    assert!(
        offered.contains(&"Turn on gw in b".to_owned()),
        "{offered:?}"
    );
    h.state_mut().look_in(None);
    h.run();
    assert!(h.query_by_label("In b").is_none());
    let offered: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
    assert!(offered.contains(&"Turn on gw".to_owned()), "{offered:?}");
}

#[test]
fn space_on_a_focused_button_is_the_buttons() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    tool(&h, Tool::Export).focus();
    h.run();
    h.key_press(egui::Key::Space);
    h.run();
    assert!(
        !drain(&engine)
            .iter()
            .any(|c| matches!(c, Command::Prepare(_))),
        "nothing switched"
    );
}

#[test]
fn going_to_a_server_that_does_not_load_there_shows_every_project() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, B);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, crate::command::Action::Select(1));
    h.run();
    assert_eq!(h.state().lens, None);
    assert_eq!(h.state().selected, BTreeSet::from([1]));
    assert_eq!(
        h.get_all_by_label("loc").count(),
        2,
        "in the list and the details"
    );
}

#[test]
fn an_undo_found_already_done_is_not_offered_again() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    ready(&mut h, &engine, plan(7, &[]));
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    drain(&engine);
    click(&mut h, "Undo");
    let mut nothing = plan_view(8, &[], &[], true);
    nothing.already = vec!["gw is already back on in a.".to_owned()];
    ready(&mut h, &engine, nothing);
    assert_eq!(
        drain(&engine),
        vec![
            Command::Prepare(vec![Change::Undo(gw_off_in_a())]),
            Command::Discard(8)
        ]
    );
    assert!(h.state().undoing.is_none());
    assert!(h.state().outcome.as_ref().is_some_and(|o| o.undo.is_none()));
}

/// Found in review of #104: what the review would have said quietly is said on the outcome when the review is
/// skipped (here, a profile that will no longer put the switch back).
#[test]
fn a_quiet_switch_says_the_reviews_notes_on_its_outcome() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    let note = "Writing switched gw in a; after this, stopping Writing leaves it as you set it.";
    let mut with_note = plan(7, &[]);
    with_note.notes = vec![note.to_owned()];
    ready(&mut h, &engine, with_note);
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    let text = h.state().outcome.as_ref().unwrap().text.clone();
    assert!(text.starts_with("Turned off gw in a."), "{text}");
    assert!(text.contains(note), "{text}");
    // Its Undo the same way.
    drain(&engine);
    click(&mut h, "Undo");
    let mut back = plan(8, &[]);
    back.summary = vec!["Turn gw back on in a".to_owned()];
    back.notes = vec!["Klefki's record of what Writing switched goes back as it was.".to_owned()];
    ready(&mut h, &engine, back);
    applied(&mut h, &engine, 8, None);
    let text = h.state().outcome.as_ref().unwrap().text.clone();
    assert!(text.contains("record of what Writing switched"), "{text}");
}

/// `key` goes down (and stays down until [`release`]): egui counts a second one as the system's repeat.
fn key(h: &mut Harness<'static, ShellApp>, key: egui::Key, modifiers: egui::Modifiers) {
    h.event(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    });
    h.run();
}

fn release(h: &mut Harness<'static, ShellApp>, key: egui::Key) {
    h.event(egui::Event::Key {
        key,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
}

/// Found in review of #104: a key held down repeats; once the first switch is saved, a repeat must not save again.
#[test]
fn a_held_space_switches_once_and_shift_or_ctrl_space_not_at_all() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    key(&mut h, egui::Key::Space, egui::Modifiers::NONE);
    assert_eq!(drain(&engine).len(), 1);
    ready(&mut h, &engine, plan(7, &[]));
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    drain(&engine);
    // Still held: the system repeats it.
    key(&mut h, egui::Key::Space, egui::Modifiers::NONE);
    key(&mut h, egui::Key::Space, egui::Modifiers::NONE);
    assert!(drain(&engine).is_empty(), "a repeat is not a press");
    release(&mut h, egui::Key::Space);
    // Shift+Space and Ctrl+Space are other keys (the grid's, the system's).
    key(&mut h, egui::Key::Space, egui::Modifiers::SHIFT);
    release(&mut h, egui::Key::Space);
    key(&mut h, egui::Key::Space, egui::Modifiers::COMMAND);
    release(&mut h, egui::Key::Space);
    assert!(drain(&engine).is_empty());
}

#[test]
fn a_held_ctrl_z_undoes_once() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    press(&mut h, Tool::TurnOff);
    ready(&mut h, &engine, plan(7, &[]));
    applied(&mut h, &engine, 7, Some(gw_off_in_a()));
    drain(&engine);
    // Ctrl+Shift+Z is redo elsewhere, not this.
    key(
        &mut h,
        egui::Key::Z,
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    );
    release(&mut h, egui::Key::Z);
    assert!(drain(&engine).is_empty());
    key(&mut h, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(drain(&engine).len(), 1, "the undo asked for");
    let mut back = plan(8, &[]);
    back.summary = vec!["Turn gw back on in a".to_owned()];
    ready(&mut h, &engine, back);
    assert_eq!(drain(&engine), vec![Command::Apply(8)]);
    applied(&mut h, &engine, 8, Some(gw_off_in_a()));
    drain(&engine);
    // Still held: the redo it now offers is not taken.
    key(&mut h, egui::Key::Z, egui::Modifiers::COMMAND);
    key(&mut h, egui::Key::Z, egui::Modifiers::COMMAND);
    assert!(drain(&engine).is_empty(), "a repeat is not a press");
}

#[test]
fn turn_on_and_off_say_where_and_how_they_save() {
    let (mut h, engine) = live_harness();
    two_projects(&mut h, &engine);
    seen_from(&mut h, A);
    click(&mut h, "gw");
    for (which, tip) in [
        (
            Tool::TurnOn,
            "Turn on in a (Space), saved at once with Undo",
        ),
        (
            Tool::TurnOff,
            "Turn off in a (Space), saved at once with Undo",
        ),
    ] {
        let description = tool(&h, which).accesskit_node().description();
        assert_eq!(description.as_deref(), Some(tip));
    }
}

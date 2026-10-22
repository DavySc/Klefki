//! A profile card's "Copy start command" (slice S5 of the on/off plan): asked for in one project, copied when it
//! arrives, with what would still differ from the profile said beside it.

use super::*;
use egui::accesskit::Role;
use klefki_core::api::StartPlan;

const NOTES: &str = "/home/notes";
const COPY: &str = "Copy start command";
const LINE: &str =
    r#"cd /home/notes && claude --settings '{"allowedMcpServers":[{"serverName":"alpha"}]}'"#;

fn plan(missing: &[&str], extra: &[&str]) -> StartPlan {
    StartPlan {
        profile: "Work".to_owned(),
        project: NOTES.to_owned(),
        args: vec![
            "--settings".to_owned(),
            r#"{"allowedMcpServers":[{"serverName":"alpha"}]}"#.to_owned(),
        ],
        line: LINE.to_owned(),
        to_do: Vec::new(),
        loads: vec!["alpha".to_owned()],
        missing: missing.iter().map(|s| (*s).to_owned()).collect(),
        extra: extra.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn copied(h: &Harness<'_, ShellApp>) -> Vec<String> {
    h.output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The Profiles page with Work, chosen to use in notes.
fn work_in_notes() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = profiles_harness();
    open_profiles(&mut h, &engine, work_profile());
    h.get_by_role_and_label(Role::ComboBox, "Use in").click();
    h.run();
    h.get_by_label_contains("notes").click();
    h.run();
    sent(&engine);
    (h, engine)
}

fn asked() -> Command {
    Command::StartCommand {
        profile: "Work".to_owned(),
        members: work_profile().profiles[0].members.clone(),
        project: NOTES.to_owned(),
    }
}

fn answer(
    h: &mut Harness<'static, ShellApp>,
    engine: &klefki_core::api::Detached,
    plan: Result<StartPlan, String>,
) {
    engine
        .events
        .send(Event::StartCommand {
            profile: "Work".to_owned(),
            project: NOTES.to_owned(),
            plan,
        })
        .unwrap();
    h.step();
}

#[test]
fn offered_only_for_one_project() {
    let (mut h, engine) = profiles_harness();
    open_profiles(&mut h, &engine, work_profile());
    assert!(h.query_by_label(COPY).is_none(), "no project chosen");
    h.get_by_role_and_label(Role::ComboBox, "Use in").click();
    h.run();
    click(&mut h, "Every project");
    assert!(h.query_by_label(COPY).is_none(), "every project");
    h.get_by_role_and_label(Role::ComboBox, "Use in").click();
    h.run();
    h.get_by_label_contains("notes").click();
    h.run();
    h.get_by_label(COPY);
}

#[test]
fn pressed_it_asks_the_engine_and_copies_the_line_when_it_comes() {
    let (mut h, engine) = work_in_notes();
    h.get_by_label(COPY).click();
    h.step();
    assert_eq!(sent(&engine), [asked()]);
    assert!(copied(&h).is_empty(), "nothing before there is a line");
    h.get_by_label("Reading the settings files…");
    answer(&mut h, &engine, Ok(plan(&[], &[])));
    assert_eq!(copied(&h), [LINE]);
    h.run();
    h.get_by_label(LINE);
    h.get_by_label("Claude Code loads only these servers there: alpha.");
    h.get_by_label_contains("Nothing is saved.");
    h.get_by_label("Copied");
    // Not copied again as frames go by.
    h.run();
    assert!(copied(&h).is_empty());
}

#[test]
fn what_would_still_differ_from_the_profile_is_said() {
    let (mut h, engine) = work_in_notes();
    click(&mut h, COPY);
    answer(
        &mut h,
        &engine,
        Ok(plan(
            &[
                "gone is off in notes, and a start command turns no server on: turn it on there first.",
            ],
            &["beta: allowedMcpServers in .claude/settings.local.json lets it load too."],
        )),
    );
    h.run();
    h.get_by_label("Not loaded, though in the profile:");
    h.get_by_label_contains("gone is off in notes");
    h.get_by_label("Loaded too, though not in the profile:");
    h.get_by_label_contains("beta: allowedMcpServers in .claude/settings.local.json");
    h.get_by_label("Loaded from the profile: alpha.");
    assert!(
        h.query_by_label_contains("loads only these servers")
            .is_none()
    );
}

#[test]
fn without_a_line_it_says_why_and_copies_nothing() {
    let (mut h, engine) = work_in_notes();
    click(&mut h, COPY);
    answer(
        &mut h,
        &engine,
        Err("Your organisation's settings let only its own list count.".to_owned()),
    );
    assert!(copied(&h).is_empty());
    h.run();
    h.get_by_label_contains("There is no start command: Your organisation's settings");
}

#[test]
fn a_change_to_the_files_asks_again_without_copying() {
    let (mut h, engine) = work_in_notes();
    click(&mut h, COPY);
    answer(&mut h, &engine, Ok(plan(&[], &[])));
    h.run();
    sent(&engine);
    engine
        .events
        .send(snapshot_with_beta_off_in_notes(true))
        .unwrap();
    h.run();
    let again = sent(&engine);
    assert_eq!(
        again.iter().filter(|c| **c == asked()).count(),
        1,
        "{again:?}"
    );
    answer(&mut h, &engine, Ok(plan(&[], &["beta loads too."])));
    assert!(copied(&h).is_empty(), "the clipboard is the user's");
    h.run();
    h.get_by_label("• beta loads too.");
}

#[test]
fn an_answer_after_the_page_was_left_copies_nothing() {
    let (mut h, engine) = work_in_notes();
    click(&mut h, COPY);
    h.state_mut().go(Page::Connections);
    h.run();
    answer(&mut h, &engine, Ok(plan(&[], &[])));
    assert!(copied(&h).is_empty());
}

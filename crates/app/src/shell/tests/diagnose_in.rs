//! The Status tab's "Diagnose in a project": a user server run from a project's folder, as a session there starts it.

use super::*;
use crate::shell::detail::diagnosis_report;
use crate::shell::diagnose_in::{DIAGNOSE, HEADING};
use egui::accesskit::Role;
use klefki_core::api::{Event, TransportKind};

fn with_projects(rows: Vec<ServerRow>, projects: &[&str]) -> Event {
    with_unusable(rows, projects, &[])
}

fn with_unusable(rows: Vec<ServerRow>, projects: &[&str], unusable: &[&str]) -> Event {
    Event::Snapshot(Arc::new(Snapshot {
        servers: rows,
        project_count: projects.len(),
        projects: projects.iter().map(|p| (*p).to_owned()).collect(),
        unusable_projects: unusable.iter().map(|p| (*p).to_owned()).collect(),
        ..Snapshot::default()
    }))
}

fn shown_with(
    row: &ServerRow,
    projects: &[&str],
) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(with_projects(vec![row.clone()], projects))
        .unwrap();
    h.run();
    open_status_tab(&mut h, &row.name);
    drain(&engine);
    (h, engine)
}

fn in_project(status: Status, folder: &str) -> Diagnosis {
    Diagnosis {
        folder: Some(folder.to_owned()),
        ..diagnosis(status, &[], &["it ran"], &[], None)
    }
}

#[test]
fn only_a_user_server_that_starts_a_command_offers_it() {
    let user = live_row("u", Scope::User, None);
    let (h, _engine) = shown_with(&user, &["/w/app"]);
    h.get_by_label(HEADING);

    let local = live_row("l", Scope::Local, Some("/w/app"));
    let (h, _engine) = shown_with(&local, &["/w/app"]);
    assert!(
        h.query_by_label(HEADING).is_none(),
        "it runs in its own project"
    );

    let http = ServerRow {
        transport: TransportKind::Http,
        target: "https://example.com/mcp".to_owned(),
        ..live_row("h", Scope::User, None)
    };
    let (h, _engine) = shown_with(&http, &["/w/app"]);
    assert!(
        h.query_by_label(HEADING).is_none(),
        "an address, not a folder"
    );
}

#[test]
fn it_lists_the_projects_where_claude_code_starts_it_and_diagnoses_the_one_chosen() {
    let row = ServerRow {
        disabled_in: 1,
        disabled_projects: vec!["/w/off".to_owned()],
        shadowed_in: vec!["/w/mine".to_owned()],
        ..live_row("u", Scope::User, None)
    };
    let (mut h, engine) = shown_with(&row, &["/w/zeta", "/w/off", "/w/mine", "/w/app"]);
    h.get_by_label_contains(
        "Not listed: 2 projects where it is off or another server called u loads.",
    );
    h.get_by_role_and_label(Role::ComboBox, "Project").click();
    h.run();
    let names: Vec<String> = h
        .query_all_by_role(Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| l.contains("/w"))
        .collect();
    // By folder name; neither where it is off nor where another loads in its place.
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(
        names[0].starts_with("app") && names[1].starts_with("zeta"),
        "{names:?}"
    );
    h.get_by_label_contains("zeta").click();
    h.run();
    click(&mut h, DIAGNOSE);
    assert_eq!(
        drain(&engine),
        vec![Command::DiagnoseIn {
            id: row.id(),
            project: "/w/zeta".to_owned(),
        }]
    );
    // While it runs: said where, and not asked again.
    h.get_by_label_contains("Diagnosing in /w/zeta");
    assert!(h.get_by_label(DIAGNOSE).accesskit_node().is_disabled());

    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: in_project(Status::Failed, "/w/zeta"),
        })
        .unwrap();
    h.run();
    h.get_by_label("Diagnosed in /w/zeta: Failed");
    h.get_by_label("it ran");
    assert!(!h.get_by_label(DIAGNOSE).accesskit_node().is_disabled());
    assert!(h.query_by_label_contains("Diagnosing in").is_none());
}

#[test]
fn the_first_project_is_chosen_until_another_is() {
    let row = live_row("u", Scope::User, None);
    let (mut h, engine) = shown_with(&row, &["/w/b", "/w/a"]);
    click(&mut h, DIAGNOSE);
    assert_eq!(
        drain(&engine),
        vec![Command::DiagnoseIn {
            id: row.id(),
            project: "/w/a".to_owned(),
        }]
    );
}

#[test]
fn with_no_project_to_start_it_in_it_says_so_and_offers_nothing() {
    // Claude Code was never started in a folder: nothing to choose, so no section.
    let none = live_row("u", Scope::User, None);
    let (h, _engine) = shown_with(&none, &[]);
    assert!(h.query_by_label(HEADING).is_none());

    // Every project has it off: said, with why.
    let off = ServerRow {
        disabled_in: 1,
        disabled_projects: vec!["/w/a".to_owned()],
        ..live_row("u", Scope::User, None)
    };
    let (h, _engine) = shown_with(&off, &["/w/a"]);
    h.get_by_label_contains("Claude Code knows no project where it starts this server.");
    h.get_by_label_contains("Not listed: 1 project where it is off");
    assert!(h.query_by_label(DIAGNOSE).is_none());
}

#[test]
fn the_report_says_which_project_it_ran_in() {
    let row = live_row("u", Scope::User, None);
    let text = diagnosis_report(&row, &in_project(Status::Connected, "/w/a"));
    assert!(text.contains("diagnosed in: /w/a\n"), "{text}");
    let home = diagnosis_report(&row, &diagnosis(Status::Connected, &[], &[], &[], None));
    assert!(!home.contains("diagnosed in"), "{home}");
}

/// A refusal is not a diagnosis: nothing ran, and the heading says so rather than a status.
#[test]
fn a_refusal_is_headed_not_diagnosed() {
    let row = live_row("u", Scope::User, None);
    let (mut h, engine) = shown_with(&row, &["/w/a"]);
    click(&mut h, DIAGNOSE);
    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: Diagnosis {
                note: Some("not diagnosed: the project folder was not found".to_owned()),
                ..in_project(Status::Unknown, "/w/a")
            },
        })
        .unwrap();
    h.run();
    h.get_by_label("Not diagnosed in /w/a");
    assert!(h.query_by_label_contains("Diagnosed in").is_none());
    h.get_by_label_contains("the project folder was not found");
}

/// A project whose folder is gone, or is a network folder, can only be refused: not offered, and counted.
#[test]
fn a_project_whose_folder_is_gone_is_not_offered() {
    let row = ServerRow {
        disabled_in: 1,
        disabled_projects: vec!["/w/off".to_owned()],
        ..live_row("u", Scope::User, None)
    };
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(with_unusable(
            vec![row.clone()],
            &["/w/gone", "/w/off", "//nas/x", "/w/here"],
            &["/w/gone", "//nas/x"],
        ))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "u");
    drain(&engine);
    h.get_by_label_contains(
        "Not listed: 1 project where it is off or another server called u loads; 2 projects whose folder is gone or is a network folder.",
    );
    click(&mut h, DIAGNOSE);
    assert_eq!(
        drain(&engine),
        vec![Command::DiagnoseIn {
            id: row.id(),
            project: "/w/here".to_owned(),
        }]
    );

    // Every project gone: nothing to choose, and why.
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(with_unusable(vec![row.clone()], &["/w/gone"], &["/w/gone"]))
        .unwrap();
    h.run();
    open_status_tab(&mut h, "u");
    h.get_by_label_contains("Claude Code knows no project where it starts this server.");
    h.get_by_label_contains("Not listed: 1 project whose folder is gone");
    assert!(h.query_by_label(DIAGNOSE).is_none());
}

fn last_start(project: &str, failed: bool) -> klefki_core::api::LastStart {
    use klefki_core::api::StartOutcome;
    klefki_core::api::LastStart {
        folder: project.to_owned(),
        project: Some(project.to_owned()),
        when: "Today 09:00".to_owned(),
        exact: String::new(),
        outcome: if failed {
            StartOutcome::Failed {
                code: Some("CONNECTION_CLOSED".to_owned()),
                said: "Connection closed".to_owned(),
                after: None,
            }
        } else {
            StartOutcome::Connected { after: None }
        },
        server_said: Vec::new(),
        hints: Vec::new(),
        partly: false,
        file: String::new(),
    }
}

/// Chosen first: where Claude Code's newest failed start was, or else its newest start, when it is listed.
#[test]
fn the_project_where_it_last_failed_is_chosen_first() {
    let row = live_row("u", Scope::User, None);
    let (mut h, engine) = shown_with(&row, &["/w/a", "/w/b", "/w/c"]);
    let starts = |starts| Event::LastStarts {
        id: row.id(),
        starts: klefki_core::api::LastStarts {
            starts,
            ..Default::default()
        },
    };
    engine
        .events
        .send(starts(vec![
            last_start("/w/c", false),
            last_start("/elsewhere", true),
            last_start("/w/b", true),
        ]))
        .unwrap();
    h.run();
    click(&mut h, DIAGNOSE);
    assert_eq!(
        drain(&engine),
        vec![Command::DiagnoseIn {
            id: row.id(),
            project: "/w/b".to_owned(),
        }]
    );

    // A variant: none failed in a listed project, so the newest start's.
    let (mut h, engine) = shown_with(&row, &["/w/a", "/w/b", "/w/c"]);
    engine
        .events
        .send(starts(vec![
            last_start("/w/c", false),
            last_start("/w/b", false),
        ]))
        .unwrap();
    h.run();
    click(&mut h, DIAGNOSE);
    assert_eq!(
        drain(&engine),
        vec![Command::DiagnoseIn {
            id: row.id(),
            project: "/w/c".to_owned(),
        }]
    );
}

/// The outcome is said next to the button too, and leads to the diagnosis below.
#[test]
fn the_outcome_is_said_by_the_button() {
    let row = live_row("u", Scope::User, None);
    let (mut h, engine) = shown_with(&row, &["/w/a"]);
    click(&mut h, DIAGNOSE);
    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: in_project(Status::Failed, "/w/a"),
        })
        .unwrap();
    h.run();
    let link = h.get_by_label("Failed in a: see below");
    assert_eq!(link.accesskit_node().role(), egui::accesskit::Role::Link);
    // Pressing it brings the diagnosis into view, which is drawn in the same frame.
    link.click();
    h.run();
    h.get_by_label("Diagnosed in /w/a: Failed");
}

/// Only the diagnosis from the project ends the wait: one from the home folder can come first.
#[test]
fn a_diagnosis_from_the_home_folder_does_not_end_the_wait() {
    let row = live_row("u", Scope::User, None);
    let (mut h, engine) = shown_with(&row, &["/w/a"]);
    click(&mut h, DIAGNOSE);
    engine
        .events
        .send(Event::Diagnosis {
            id: row.id(),
            diagnosis: diagnosis(Status::Connected, &[], &[], &[], None),
        })
        .unwrap();
    h.run();
    assert!(h.get_by_label(DIAGNOSE).accesskit_node().is_disabled());
    h.get_by_label_contains("Diagnosing in /w/a");
}

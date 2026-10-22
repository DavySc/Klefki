//! The Status tab's "Diagnose in a project": a user server run from a project's folder, as a session there starts it.

use super::*;
use crate::shell::detail::diagnosis_report;
use crate::shell::diagnose_in::{DIAGNOSE, HEADING};
use egui::accesskit::Role;
use klefki_core::api::{Event, TransportKind};

fn with_projects(rows: Vec<ServerRow>, projects: &[&str]) -> Event {
    Event::Snapshot(Arc::new(Snapshot {
        servers: rows,
        project_count: projects.len(),
        projects: projects.iter().map(|p| (*p).to_owned()).collect(),
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

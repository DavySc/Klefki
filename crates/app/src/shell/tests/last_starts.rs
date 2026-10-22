//! The Status tab's last starts by Claude Code, from its logs (decision 013).

use super::*;
use crate::shell::last_starts::{ABOUT, COPY, HEADING, report};
use klefki_core::api::{LastStart, LastStarts, StartOutcome};
use std::time::Duration;

fn start(folder: &str, when: &str, outcome: StartOutcome) -> LastStart {
    LastStart {
        folder: folder.to_owned(),
        when: when.to_owned(),
        exact: "22 Oct 2026, 07:32:16 (UTC+00:00)".to_owned(),
        outcome,
        server_said: Vec::new(),
        hints: Vec::new(),
        partly: false,
        file: format!("~/.cache/claude-cli-nodejs/{folder}/x.jsonl"),
    }
}

fn failed_in_proj() -> LastStart {
    LastStart {
        server_said: vec!["Error: Cannot find module 'foo'".to_owned()],
        hints: vec![Hint {
            title: "A file the server needs is missing".to_owned(),
            code: None,
            help: vec!["Install the server's dependencies.".to_owned()],
            points_at: None,
        }],
        partly: true,
        ..start(
            "~/proj",
            "Today 07:32",
            StartOutcome::Failed {
                code: Some("CONNECTION_CLOSED".to_owned()),
                said: "Connection closed".to_owned(),
                after: Some(Duration::from_millis(388)),
            },
        )
    }
}

fn connected_at_home() -> LastStart {
    LastStart {
        server_said: vec!["ok server up".to_owned()],
        ..start(
            "~",
            "Today 08:00",
            StartOutcome::Connected {
                after: Some(Duration::from_millis(62)),
            },
        )
    }
}

/// `s` shown on its Status tab, its last starts asked for and `answer` given.
fn shown(
    answer: LastStarts,
) -> (
    Harness<'static, ShellApp>,
    klefki_core::api::Detached,
    ServerRow,
) {
    let (mut h, engine) = live_harness();
    let row = live_row("s", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    open_status_tab(&mut h, "s");
    assert_eq!(drain(&engine), vec![Command::LastStarts(row.id())]);
    // Asked once, not every frame.
    h.run();
    assert_eq!(drain(&engine), Vec::<Command>::new());
    engine
        .events
        .send(Event::LastStarts {
            id: row.id(),
            starts: answer,
        })
        .unwrap();
    h.run();
    (h, engine, row)
}

#[test]
fn the_status_tab_shows_the_last_starts_the_failed_one_open() {
    use egui::accesskit::Role;
    let (h, _engine, _row) = shown(LastStarts {
        starts: vec![connected_at_home(), failed_in_proj()],
        ..LastStarts::default()
    });
    h.get_by_label(HEADING);
    // Each line twice: the line, and the arrow that opens it, named after it for the keyboard and screen readers.
    for line in [
        "Today 08:00 in ~: connected in 62 ms",
        "Today 07:32 in ~/proj: failed (CONNECTION_CLOSED)",
    ] {
        h.get_by_role_and_label(Role::Label, line);
        h.get_by_role_and_label(Role::Button, line);
    }
    // The failure is the one opened: what Claude Code and the server said, and what it probably means.
    h.get_by_label("Claude Code said: Connection closed (after 388 ms)");
    h.get_by_label("A file the server needs is missing");
    h.get_by_label("Error: Cannot find module 'foo'");
    h.get_by_label_contains("only its start and its end were read");
    // The other start is closed.
    assert!(h.query_by_label("ok server up").is_none());
    h.get_by_label("Copy last starts");
    // That Klefki's own checks are starts too is said where everyone sees it, not only in a hover.
    h.get_by_label(ABOUT);
}

#[test]
fn copying_says_copied_for_a_moment() {
    let (mut h, _engine, _row) = shown(LastStarts {
        starts: vec![failed_in_proj()],
        ..LastStarts::default()
    });
    h.get_by_label(COPY).click();
    // The frame that takes the click.
    h.step();
    let copied_at = h.ctx.input(|i| i.time);
    h.step();
    h.get_by_label("Copied");
    // For as long as Export's Copy says it, and not a frame longer.
    let mut steps = 0;
    while h.query_by_label("Copied").is_some() {
        h.step();
        steps += 1;
        assert!(steps < 100, "still Copied");
    }
    let gone_at = h.ctx.input(|i| i.time);
    assert_eq!(gone_at - copied_at, crate::shell::export::COPIED_SECS);
    // A second copy is confirmed again.
    h.get_by_label(COPY).click();
    h.step();
    h.step();
    assert!(h.query_by_label(COPY).is_none());
    // A screen reader hears it too.
    let node = h.get_by_label("Copied");
    assert_eq!(node.accesskit_node().live(), egui::accesskit::Live::Polite);
}

#[test]
fn showing_the_status_tab_again_reads_the_logs_again() {
    // A session may have started the server since: going back to the tab, or to the server, shows that.
    use egui::accesskit::Role;
    let (mut h, engine, row) = shown(LastStarts {
        starts: vec![failed_in_proj()],
        ..LastStarts::default()
    });
    h.get_by_role_and_label(Role::Button, "General").click();
    h.run();
    h.run();
    assert_eq!(drain(&engine), Vec::<Command>::new());
    h.get_by_role_and_label(Role::Button, "Status").click();
    h.run();
    assert_eq!(drain(&engine), vec![Command::LastStarts(row.id())]);
    // What was read stays meanwhile, so the section does not blink.
    h.get_by_label(HEADING);
    h.run();
    assert_eq!(drain(&engine), Vec::<Command>::new());
}

#[test]
fn nothing_logged_says_so_and_where_it_looked() {
    let (h, _engine, _row) = shown(LastStarts {
        looked_in: Some("~/.cache/claude-cli-nodejs".to_owned()),
        ..LastStarts::default()
    });
    // Claude Code keeps its logs for as long as `cleanupPeriodDays` says, so no number of days is promised.
    h.get_by_label_contains("Claude Code has no log of starting it");
    assert!(h.query_by_label_contains("in the last 30 days").is_none());
    h.get_by_label_contains("~/.cache/claude-cli-nodejs");
    assert!(h.query_by_label("Copy last starts").is_none());
}

#[test]
fn a_problem_and_a_shared_log_folder_are_said() {
    let (h, _engine, _row) = shown(LastStarts {
        starts: vec![connected_at_home()],
        shares_log_with: vec!["my.server".to_owned()],
        problem: Some("Claude Code loads another server of this name in its place.".to_owned()),
        looked_in: None,
    });
    h.get_by_label_contains("loads another server of this name");
    h.get_by_label_contains("Claude Code logs my.server in the same place");
    // The only start, so it is open.
    h.get_by_label("ok server up");
}

#[test]
fn a_check_that_ends_reads_the_logs_again() {
    // Klefki's own check is a start too: once it has answered, the starts are read again.
    let (mut h, engine, row) = shown(LastStarts {
        starts: vec![failed_in_proj()],
        ..LastStarts::default()
    });
    engine
        .events
        .send(status_event(&row, Status::Checking, None))
        .unwrap();
    h.run();
    assert_eq!(drain(&engine), Vec::<Command>::new());
    h.get_by_label(HEADING);
    engine
        .events
        .send(status_event(&row, Status::Connected, None))
        .unwrap();
    h.run();
    h.run();
    assert_eq!(drain(&engine), vec![Command::LastStarts(row.id())]);
    // What was read stays while it is read again.
    h.get_by_label(HEADING);
    // So does a change to the files.
    engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
    h.run();
    h.run();
    assert_eq!(drain(&engine), vec![Command::LastStarts(row.id())]);
    h.get_by_label(HEADING);
}

#[test]
fn an_answer_nobody_asked_for_is_dropped() {
    let (mut h, engine) = live_harness();
    let row = live_row("s", Scope::User, None);
    show_rows(&mut h, &engine, vec![row.clone()]);
    engine
        .events
        .send(Event::LastStarts {
            id: row.id(),
            starts: LastStarts {
                starts: vec![failed_in_proj()],
                ..LastStarts::default()
            },
        })
        .unwrap();
    h.run();
    assert!(h.state().last_starts.is_empty());
}

#[test]
fn the_report_to_copy_is_complete_and_plain() {
    let row = live_row("s", Scope::User, None);
    let text = report(
        &row,
        &LastStarts {
            starts: vec![failed_in_proj(), connected_at_home()],
            ..LastStarts::default()
        },
    );
    for part in [
        "Last starts of s by Claude Code",
        "Today 07:32 in ~/proj: failed (CONNECTION_CLOSED)",
        "at 22 Oct 2026, 07:32:16 (UTC+00:00)",
        "Claude Code said: Connection closed (after 388 ms)",
        "A file the server needs is missing",
        "Install the server's dependencies.",
        "the server said:\n    Error: Cannot find module 'foo'",
        "Today 08:00 in ~: connected in 62 ms",
        "    ok server up",
        "log: ~/.cache/claude-cli-nodejs/~/proj/x.jsonl",
    ] {
        assert!(text.contains(part), "{part:?} not in {text}");
    }
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "{text:?}"
    );
}

#[test]
fn a_line_opens_and_closes_when_clicked_and_tells_the_exact_time_on_hover() {
    use egui::accesskit::Role;
    let (mut h, _engine, _row) = shown(LastStarts {
        starts: vec![connected_at_home(), failed_in_proj()],
        ..LastStarts::default()
    });
    let home = "Today 08:00 in ~: connected in 62 ms";
    h.get_by_role_and_label(Role::Label, home).click();
    h.run();
    h.get_by_label("ok server up");
    h.get_by_role_and_label(Role::Label, home).click();
    h.run();
    assert!(h.query_by_label("ok server up").is_none());
    h.get_by_role_and_label(Role::Label, home).hover();
    h.run();
    h.get_by_label("22 Oct 2026, 07:32:16 (UTC+00:00)");
}

#[test]
fn the_copied_report_points_at_the_whole_program_claude_code_named() {
    // A program whose path has a space: Claude Code's message tells where it ends, so the marker covers all of it.
    let mut row = live_row("s", Scope::User, None);
    row.target = "/opt/My Tools/srv --port 1".to_owned();
    let mut start = failed_in_proj();
    start.outcome = StartOutcome::Failed {
        code: None,
        said: "spawn /opt/My Tools/srv ENOENT".to_owned(),
        after: None,
    };
    start.hints[0].points_at = Some((klefki_core::api::Focus::Command, "not found".to_owned()));
    let text = report(
        &row,
        &LastStarts {
            starts: vec![start],
            ..LastStarts::default()
        },
    );
    assert!(
        text.contains(&format!(
            "{} not found",
            "^".repeat("/opt/My Tools/srv".len())
        )),
        "{text}"
    );
}

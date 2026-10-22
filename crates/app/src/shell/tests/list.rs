//! The server list: grouping, the search, and what a row says about where a server is on, hidden or approved.

use super::*;

#[test]
fn grid_lists_fixture_servers_grouped_by_scope() {
    let mut h = harness();
    h.run();
    h.get_by_label("User");
    h.get_by_label("issue-tracker");
    h.get_by_label("Local · C:/Repos/Gateway");
    h.get_by_label("Project · C:/Repos/WebApp");
}

#[test]
fn search_filters_the_grid() {
    let mut app = ShellApp::new(fixture_snapshot(), ThemeChoice::Dark);
    let all = app.visible().len();
    app.search = "it.example".into();
    let hits = app.visible();
    assert_eq!(hits.len(), 1);
    assert!(hits.len() < all);
    app.search = "NO-SUCH-SERVER".into();
    assert!(app.visible().is_empty());
}

#[test]
fn grid_renders_any_snapshot_not_just_the_fixture() {
    // Loading from a real config is tested in klefki-core; the UI only renders what it is given.
    let row = |name: &str| ServerRow {
        connection: klefki_core::api::ConnectionId::new(Scope::User, None, name),
        name: name.to_owned(),
        scope: Scope::User,
        project: None,
        transport: klefki_core::api::TransportKind::Http,
        target: format!("https://{name}.example.com/mcp"),
        status: Status::Connected,
        enabled: true,
        disabled_in: 0,
        disabled_projects: Vec::new(),
        approval: None,
        shadowed: false,
        shadowed_in: Vec::new(),
        casing_collision: false,
        relative_paths: Vec::new(),
        preflight: Vec::new(),
        status_cached: false,
        oauth: None,
        plugin: None,
        managed: None,
        not_loaded: None,
        blocked: None,
        may_not_load: None,
    };
    let mut h = harness_with(Snapshot {
        servers: vec![row("REMOTE_1"), row("REMOTE_2")],
        ..Snapshot::default()
    });
    h.run();
    h.get_by_label("REMOTE_1");
    h.get_by_label("REMOTE_2");
}

fn snapshot_with_projects(count: usize, rows: Vec<ServerRow>) -> klefki_core::api::Event {
    klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
        servers: rows,
        project_count: count,
        ..Snapshot::default()
    }))
}

#[test]
fn a_user_server_off_in_some_projects_says_how_many() {
    let (mut h, engine) = live_harness();
    let mut row = live_row("gateway", Scope::User, None);
    row.disabled_in = 3;
    engine
        .events
        .send(snapshot_with_projects(17, vec![row]))
        .unwrap();
    h.run();
    h.get_by_label("gateway");
    h.get_by_label("off in 3/17");
    assert!(
        h.query_by_label_contains("Disabled").is_none(),
        "still on in 14 projects"
    );
}

#[test]
fn a_user_server_off_everywhere_reads_disabled_without_the_partial_badge() {
    let (mut h, engine) = live_harness();
    let mut row = live_row("gateway", Scope::User, None);
    row.disabled_in = 17;
    row.enabled = false;
    engine
        .events
        .send(snapshot_with_projects(17, vec![row]))
        .unwrap();
    h.run();
    h.get_by_label_contains("Disabled");
    assert!(h.query_by_label_contains("off in").is_none());
}

#[test]
fn a_user_server_hidden_in_some_projects_says_how_many_and_where() {
    let (mut h, engine) = live_harness();
    let mut row = live_row("gateway", Scope::User, None);
    row.shadowed_in = vec!["C:/Repos/WebApp".into(), "C:/Repos/Api".into()];
    engine
        .events
        .send(snapshot_with_projects(17, vec![row]))
        .unwrap();
    h.run();
    h.get_by_label("shadowed in 2/17").hover();
    h.run();
    h.get_by_label_contains(
        "In 2 of 17 projects, a local or project server of the same name is used",
    );
    assert!(
        h.query_by_label("shadowed").is_none(),
        "still used in 15 projects"
    );
    click(&mut h, "gateway");
    // Named as their groups are; the whole paths on hover.
    h.get_by_label("in 2 of 17 projects: WebApp, Api").hover();
    h.run();
    h.get_by_label_contains("C:/Repos/WebApp\nC:/Repos/Api");
}

#[test]
fn a_user_server_with_a_relative_path_is_marked_and_its_details_name_the_paths() {
    let (mut h, engine) = live_harness();
    let mut user = live_row("docs", Scope::User, None);
    user.relative_paths = vec!["build/index.js".into(), "./c.json (in CFG)".into()];
    // In its own project's folder a relative path is what it should be.
    let mut local = live_row("tools", Scope::Local, Some("C:/Repos/WebApp"));
    local.relative_paths = vec!["server.py".into()];
    engine
        .events
        .send(snapshot_with_projects(3, vec![user, local]))
        .unwrap();
    h.run();
    let badges: Vec<_> = h.query_all_by_label("relative path").collect();
    assert_eq!(badges.len(), 1, "only the user server");
    h.get_by_label("relative path").hover();
    h.run();
    h.get_by_label_contains(
        "sessions in other folders can't \
         find a file its command names",
    );
    click(&mut h, "docs");
    h.get_by_label(
        "Use full paths: sessions in other folders can't find build/index.js or ./c.json (in CFG).",
    );
    h.get_by_label_contains("so the check can pass here");
    click(&mut h, "tools");
    assert!(h.query_by_label_contains("Use full paths").is_none());
}

#[test]
fn what_keeps_a_server_from_starting_is_a_badge_and_its_details_say_what_to_do() {
    use klefki_core::api::{Preflight, PreflightKind};
    let (mut h, engine) = live_harness();
    let found = |kind, problem: &str, fix: &str| Preflight {
        kind,
        problem: problem.to_owned(),
        fix: fix.to_owned(),
    };
    let mut docker = live_row("fetch", Scope::User, None);
    docker.preflight = vec![
        found(
            PreflightKind::NoInput,
            "docker run has no -i, so the container never reads what Claude Code sends.",
            "Add -i after run.",
        ),
        found(
            PreflightKind::EmptyValue,
            "API_KEY is empty.",
            "Give it a value.",
        ),
    ];
    // Claude Code loads it nowhere: that is what its row says, not why it would not start.
    let mut replaced = live_row("wiki", Scope::User, None);
    replaced.preflight = vec![found(
        PreflightKind::NotFound,
        "Can't find x.",
        "Install it.",
    )];
    replaced.not_loaded = Some("Your organisation decides which servers Claude Code loads.".into());
    engine
        .events
        .send(snapshot_with_projects(1, vec![docker, replaced]))
        .unwrap();
    h.run();
    assert_eq!(
        h.query_all_by_label("no -i").count(),
        1,
        "the first finding names the badge"
    );
    assert!(
        h.query_by_label("not found").is_none(),
        "not on a server that is not loaded"
    );
    h.get_by_label("no -i").hover();
    h.run();
    h.get_by_label_contains("docker run has no -i, so the container never reads what Claude Code sends. API_KEY is empty.");
    click(&mut h, "fetch");
    h.get_by_label("docker run has no -i, so the container never reads what Claude Code sends.");
    h.get_by_label("Add -i after run.");
    h.get_by_label("API_KEY is empty.");
    h.get_by_label("Give it a value.");
}

#[test]
fn a_server_that_started_is_not_said_to_be_missing_its_program() {
    use klefki_core::api::{Preflight, PreflightKind};
    let (mut h, engine) = live_harness();
    let found = |kind, problem: &str| Preflight {
        kind,
        problem: problem.to_owned(),
        fix: "Do something.".to_owned(),
    };
    let mut row = live_row("npx-server", Scope::User, None);
    row.preflight = vec![
        found(
            PreflightKind::NotFound,
            "Can't find npx on the PATH Klefki was started with.",
        ),
        found(PreflightKind::EmptyValue, "LEVEL is empty."),
    ];
    engine
        .events
        .send(snapshot_with_projects(1, vec![row.clone()]))
        .unwrap();
    h.run();
    h.get_by_label("not found");
    // Claude Code found it after all (another PATH): only what still holds is said.
    for status in [Status::Connected, Status::Degraded, Status::NeedsAuth] {
        engine
            .events
            .send(status_event(&row, status, None))
            .unwrap();
        h.run();
        assert!(h.query_by_label("not found").is_none(), "{status:?}");
        h.get_by_label("empty value");
    }
    click(&mut h, "npx-server");
    assert!(h.query_by_label_contains("Can't find npx").is_none());
    h.get_by_label("LEVEL is empty.");
}

#[test]
fn a_user_server_hidden_everywhere_reads_shadowed_without_the_count() {
    let (mut h, engine) = live_harness();
    let mut row = live_row("gateway", Scope::User, None);
    row.shadowed_in = vec!["C:/Repos/WebApp".into(), "C:/Repos/Api".into()];
    row.shadowed = true;
    engine
        .events
        .send(snapshot_with_projects(2, vec![row]))
        .unwrap();
    h.run();
    h.get_by_label("shadowed");
    assert!(h.query_by_label_contains("shadowed in").is_none());
    click(&mut h, "gateway");
    h.get_by_label("in all 2 projects: WebApp, Api");
}

#[test]
fn a_long_list_of_projects_hiding_a_server_is_cut() {
    let (mut h, engine) = live_harness();
    let mut row = live_row("gateway", Scope::User, None);
    row.shadowed_in = (1..=9).map(|n| format!("/p{n}")).collect();
    engine
        .events
        .send(snapshot_with_projects(40, vec![row]))
        .unwrap();
    h.run();
    click(&mut h, "gateway");
    h.get_by_label("in 9 of 40 projects: p1, p2, p3, p4, p5 and 4 more");
}

#[test]
fn badges_that_do_not_fit_beside_the_name_are_counted_not_spilled() {
    let (mut h, engine) = live_harness();
    let mut row = live_row("gateway", Scope::User, None);
    row.disabled_in = 3;
    row.shadowed_in = vec!["C:/Repos/WebApp".into(), "C:/Repos/Api".into()];
    row.casing_collision = true;
    engine
        .events
        .send(snapshot_with_projects(17, vec![row]))
        .unwrap();
    for size in [SMALLEST_SIZE, OPENING_SIZE] {
        h.set_size(size);
        h.run();
        h.get_by_label("gateway");
        let type_left = h.get_by_label("Type").rect().left();
        let mut shown = 0;
        for text in ["casing", "off in 3/17", "shadowed in 2/17"] {
            if let Some(b) = h.query_by_label(text) {
                assert!(b.rect().right() <= type_left, "{text} spills at {size:?}");
                shown += 1;
            }
        }
        let more = (1..=3)
            .find_map(|n| h.query_by_label(&format!("+{n}")).map(|b| (n, b.rect())))
            .map_or(0, |(n, rect)| {
                assert!(rect.right() <= type_left, "+{n} spills at {size:?}");
                n
            });
        assert_eq!(shown + more, 3, "at {size:?}");
    }
}

#[test]
fn the_server_used_instead_says_which_user_server_it_replaces() {
    let (mut h, engine) = live_harness();
    let mut user = live_row("gateway", Scope::User, None);
    user.shadowed_in = vec!["C:/Repos/WebApp".into()];
    let local = live_row("gateway", Scope::Local, Some("C:/Repos/WebApp"));
    // Same name elsewhere, but the user server is not hidden there (say, turned off): it replaces nothing.
    let other = live_row("gateway", Scope::Local, Some("C:/Repos/Api"));
    engine
        .events
        .send(snapshot_with_projects(3, vec![user, local, other]))
        .unwrap();
    h.run();
    // The two local rows follow the user row, in either order; the detail panel says which is selected.
    h.get_all_by_label("gateway").nth(1).unwrap().click();
    h.run();
    let mut seen = 0;
    for step in 0..2 {
        if step == 1 {
            h.key_press(egui::Key::ArrowDown);
            h.run();
        }
        if h.query_by_label("C:/Repos/WebApp").is_some() {
            h.get_by_label("Replaces");
            h.get_by_label("the user server gateway");
            seen += 1;
        } else {
            h.get_by_label("C:/Repos/Api");
            assert!(h.query_by_label("Replaces").is_none());
        }
    }
    assert_eq!(seen, 1);
}

#[test]
fn a_rejected_project_server_says_rejected_and_pending_is_a_status() {
    let (mut h, engine) = live_harness();
    let mut rejected = live_row("no-thanks", Scope::Project, Some("C:/Repos/WebApp"));
    rejected.approval = Some(klefki_core::api::Approval::Rejected);
    let mut pending = live_row("waiting", Scope::Project, Some("C:/Repos/WebApp"));
    pending.approval = Some(klefki_core::api::Approval::Pending);
    pending.status = Status::PendingApproval;
    engine
        .events
        .send(snapshot_with_projects(1, vec![rejected, pending]))
        .unwrap();
    h.run();
    h.get_by_label("\u{25CF} Rejected");
    h.get_by_label("\u{25CF} Pending approval");
}

#[test]
fn the_detail_panel_explains_the_toggle_and_the_approval() {
    let (mut h, engine) = live_harness();
    let mut user = live_row("gateway", Scope::User, None);
    user.disabled_in = 3;
    let mut project = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
    project.approval = Some(klefki_core::api::Approval::Pending);
    engine
        .events
        .send(snapshot_with_projects(17, vec![user, project]))
        .unwrap();
    h.run();
    click(&mut h, "gateway");
    h.get_by_label("disabled in 3 of 17 projects");
    click(&mut h, "docs");
    h.get_by_label("Pending approval");
    assert!(h.query_by_label_contains("of 17 projects").is_none());
}

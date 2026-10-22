//! Engine tests: a user server diagnosed from a project's folder, as a session started there runs it.
use super::*;

const DEBUG_STDIO: &[u8] = include_bytes!("../../../../../fixtures/debug/failed-stdio.log");

fn diagnosis_of(events: &[Event], name: &str) -> Option<Diagnosis> {
    events.iter().rev().find_map(|e| match e {
        Event::Diagnosis { id, diagnosis } if id.name == name => Some(diagnosis.clone()),
        _ => None,
    })
}

/// The folder of every `--debug … mcp get` the fake CLI was asked to run, in order.
fn diagnose_folders(env: &Env) -> Vec<String> {
    std::fs::read_to_string(&env.log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|l| l["event"] == "start" && l["argv"][0] == "--debug")
        .map(|l| l["cwd"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A user server `bad-stdio` (and the servers in `more`) and `projects`, entries by folder name, each folder made in
/// the test's own unless its name starts with `missing` or is a network folder (`//…`), kept as it is. Every diagnose of `bad-stdio` answers with a failed start.
fn env_with(projects: &[(&str, serde_json::Value)], more: serde_json::Value) -> (Env, Vec<String>) {
    let rule = r#",{"args":["--debug","mcp","--debug-file","*","mcp","get","bad-stdio"],"debug_file_from":"dbg.log","stdout_file":"failed.txt"}"#;
    let env = Env::new(
        &serde_json::json!({}),
        &format!(r#"{{"rules":[{{"args":["--version"]}}{rule}]}}"#),
        &[("dbg.log", DEBUG_STDIO), ("failed.txt", FAILED)],
    );
    let mut entries = serde_json::Map::new();
    let mut paths = Vec::new();
    for (name, entry) in projects {
        // A network folder is named as it is; nothing is made for it.
        let path = if name.starts_with("//") {
            (*name).to_owned()
        } else {
            let folder = env.dir.path().join("projects").join(name);
            if !name.starts_with("missing") {
                std::fs::create_dir_all(&folder).unwrap();
            }
            folder.to_string_lossy().replace('\\', "/")
        };
        entries.insert(path.clone(), entry.clone());
        paths.push(path);
    }
    let mut servers =
        serde_json::json!({"bad-stdio": {"command": "node", "args": ["./server.js"]}});
    if let Some(more) = more.as_object() {
        servers.as_object_mut().unwrap().extend(more.clone());
    }
    std::fs::write(
        env.dir.path().join("cfg").join(".claude.json"),
        serde_json::json!({"mcpServers": servers, "projects": entries}).to_string(),
    )
    .unwrap();
    (env, paths)
}

fn started(env: &Env) -> Core {
    let core = start_with(
        EngineConfig {
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    core
}

fn user(name: &str) -> ServerId {
    ServerId::new(Scope::User, None, name)
}

#[test]
fn a_user_server_is_diagnosed_from_the_projects_folder_and_its_status_is_left_alone() {
    let (env, paths) = env_with(
        &[
            ("app", serde_json::json!({})),
            ("other", serde_json::json!({})),
        ],
        serde_json::json!({}),
    );
    let core = started(&env);
    core.send(Command::DiagnoseIn {
        id: user("bad-stdio"),
        project: paths[0].clone(),
    });
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    let d = diagnosis_of(&events, "bad-stdio").unwrap();
    assert_eq!(d.folder.as_deref(), Some(paths[0].as_str()));
    assert_eq!(d.status, Status::Failed);
    assert_eq!(d.note, None);
    assert_eq!(d.hints[0].title, "The command was not found");
    let ran = diagnose_folders(&env);
    assert_eq!(ran.len(), 1, "{ran:?}");
    assert_eq!(
        std::fs::canonicalize(&ran[0]).unwrap(),
        std::fs::canonicalize(&paths[0]).unwrap(),
        "run in the project's folder"
    );
    // The row's status is the home folder's: no "Checking…" and no answer for it.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Status { id, .. } if id.name == "bad-stdio")),
        "{events:#?}"
    );

    // A diagnose without a project still runs in the home folder and says no folder.
    core.send(Command::Diagnose(user("bad-stdio")));
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    assert_eq!(diagnosis_of(&events, "bad-stdio").unwrap().folder, None);
    let ran = diagnose_folders(&env);
    assert_eq!(
        std::fs::canonicalize(&ran[1]).unwrap(),
        std::fs::canonicalize(env.dir.path().join("home")).unwrap()
    );
}

#[test]
fn a_project_is_named_as_the_snapshot_lists_it_though_the_file_spells_it_otherwise() {
    let (env, paths) = env_with(&[("app/", serde_json::json!({}))], serde_json::json!({}));
    let core = started(&env);
    // The project as the snapshot lists it: without the trailing slash.
    let project = paths[0].trim_end_matches('/').to_owned();
    core.send(Command::DiagnoseIn {
        id: user("bad-stdio"),
        project: project.clone(),
    });
    let events = collect(&core, |e| diagnosis_of(e, "bad-stdio").is_some());
    let d = diagnosis_of(&events, "bad-stdio").unwrap();
    assert_eq!(d.note, None, "{d:?}");
    assert_eq!(d.folder.as_deref(), Some(project.as_str()));
    assert_eq!(diagnose_folders(&env).len(), 1);
}

#[test]
fn a_project_where_claude_code_would_not_start_it_is_refused_without_running_anything() {
    let (env, paths) = env_with(
        &[
            (
                "off",
                serde_json::json!({"disabledMcpServers": ["bad-stdio"]}),
            ),
            (
                "shadowed",
                serde_json::json!({"mcpServers": {"bad-stdio": {"command": "local"}}}),
            ),
            ("missing", serde_json::json!({})),
            (
                "fine",
                serde_json::json!({"mcpServers": {"loc": {"command": "x"}}}),
            ),
            ("//nas/share/app", serde_json::json!({})),
        ],
        serde_json::json!({"http": {"type": "http", "url": "https://example.com/mcp"}}),
    );
    let core = started(&env);
    let refused = |id: ServerId, project: String, why: &str| {
        let name = id.name.clone();
        core.send(Command::DiagnoseIn {
            id,
            project: project.clone(),
        });
        let events = collect(&core, |e| diagnosis_of(e, &name).is_some());
        let d = diagnosis_of(&events, &name).unwrap();
        let note = d.note.unwrap_or_default();
        assert!(note.starts_with("not diagnosed: "), "{note}");
        assert!(note.contains(why), "{note:?} should say {why:?}");
        assert_eq!(d.status, Status::Unknown);
        assert_eq!(d.folder.as_deref(), Some(project.as_str()));
    };
    refused(user("bad-stdio"), paths[0].clone(), "off in that project");
    refused(
        user("bad-stdio"),
        paths[1].clone(),
        "another server called bad-stdio",
    );
    refused(user("bad-stdio"), paths[2].clone(), "folder was not found");
    refused(user("bad-stdio"), paths[4].clone(), "network folder");
    refused(
        user("bad-stdio"),
        "/nowhere/known".to_owned(),
        "does not know that project",
    );
    // Only a user server is started in each project's folder; a local one already runs in its own.
    refused(
        ServerId::new(Scope::Local, Some(&paths[3]), "loc"),
        paths[3].clone(),
        "only a user server",
    );
    // The folder plays no part in how Claude Code reaches an HTTP server.
    refused(user("http"), paths[3].clone(), "by its address");
    assert_eq!(diagnose_folders(&env), Vec::<String>::new(), "nothing ran");
}

/// The projects whose folder is gone, or is a network folder Klefki does not run in, are in the snapshot, so they are
/// not offered; a folder that is there is not.
#[test]
fn the_snapshot_says_which_project_folders_cannot_be_run_in() {
    let (env, paths) = env_with(
        &[
            ("app", serde_json::json!({})),
            ("missing", serde_json::json!({})),
            ("//nas/share/app", serde_json::json!({})),
        ],
        serde_json::json!({}),
    );
    let core = start_with(
        EngineConfig {
            background_checks: false,
            ..env.config()
        },
        || {},
    );
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let snapshot = snapshots(&events).pop().unwrap().clone();
    assert!(
        snapshot.projects.contains(&paths[0]),
        "{:?}",
        snapshot.projects
    );
    assert_eq!(
        snapshot.unusable_projects,
        [paths[2].clone(), paths[1].clone()]
    );
}

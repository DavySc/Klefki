//! Engine tests: export, and the save and open dialogs.
use super::*;

fn exported(core: &Core, ids: Vec<ServerId>) -> crate::export::Exported {
    core.send(Command::Export(ids));
    collect(core, |e| e.iter().any(|x| matches!(x, Event::Exported(_))))
        .into_iter()
        .find_map(|e| match e {
            Event::Exported(out) => Some(out),
            _ => None,
        })
        .unwrap()
}

#[test]
fn an_export_reads_each_server_from_its_own_file_in_the_order_asked() {
    let (env, key, _repo) = repo_env(serde_json::json!({
        "mcpServers": {"loc": {"type": "stdio", "command": "x", "env": {"K": "v-12345678"}}}
    }));
    let claude_json_before =
        std::fs::read(env.dir.path().join("cfg").join(".claude.json")).unwrap();
    let core = env.start();
    let out = exported(
        &core,
        vec![
            project_id(&key, "ps"),
            local_id("loc", &key),
            user_id("gone"),
        ],
    );
    let block: serde_json::Value = serde_json::from_str(&out.text).unwrap();
    let servers = block["mcpServers"].as_object().unwrap();
    assert_eq!(servers.keys().collect::<Vec<_>>(), ["ps", "loc"]);
    assert_eq!(servers["ps"]["command"], "node");
    assert_eq!(
        servers["ps"]["env"],
        serde_json::json!({"NODE_OPTIONS": "${NODE_OPTIONS}"})
    );
    assert_eq!(servers["loc"]["env"], serde_json::json!({"K": "${K}"}));
    assert!(!out.text.contains("v-12345678"), "{}", out.text);
    assert_eq!(
        out.notes,
        ["Left out gone: it is no longer in the configuration."]
    );
    // Reading is all it does.
    assert_eq!(
        std::fs::read(env.dir.path().join("cfg").join(".claude.json")).unwrap(),
        claude_json_before
    );
}

fn first_event<T>(core: &Core, pick: impl Fn(&Event) -> Option<T>) -> T {
    collect(core, |e| e.iter().any(|x| pick(x).is_some()))
        .iter()
        .find_map(pick)
        .unwrap()
}

/// A window as the UI hands it over. Its handles are not there (as for a window not shown yet), which is all a
/// test can give without one.
struct Window;

impl raw_window_handle::HasWindowHandle for Window {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::Unavailable)
    }
}

impl raw_window_handle::HasDisplayHandle for Window {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::Unavailable)
    }
}

#[test]
fn the_save_dialog_opens_over_the_window_where_it_lives_and_its_answer_is_awaited_elsewhere() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let opened: Arc<Mutex<Vec<(String, bool, std::thread::ThreadId)>>> = Arc::default();
    let (answer, user) = mpsc::channel::<Option<PathBuf>>();
    let user = Arc::new(Mutex::new(user));
    let mut config = env.config();
    config.open_dialog = {
        let opened = Arc::clone(&opened);
        Arc::new(move |name, owner| {
            lock(&opened).push((
                name.to_owned(),
                owner.is_some(),
                std::thread::current().id(),
            ));
            let user = Arc::clone(&user);
            // The user answers when the test says so.
            Box::pin(async move { lock(&user).recv().ok().flatten() })
        })
    };
    let core = start_with(config, || {});
    core.choose_export_file("github-mcp.json".into(), Some(&Window));
    // Opened here, on the thread that has the window, over it; and this thread did not wait for the user.
    assert_eq!(
        *lock(&opened),
        vec![(
            "github-mcp.json".to_owned(),
            true,
            std::thread::current().id()
        )]
    );
    let file = env.dir.path().join("home").join("github-mcp.json");
    answer.send(Some(file.clone())).unwrap();
    let chosen = first_event(&core, |e| match e {
        Event::ExportFileChosen(choice) => Some(choice.clone()),
        _ => None,
    });
    assert_eq!(chosen, crate::export_file::FileChoice::Chosen(file));

    // Nearby: a dialog with no window to belong to opens all the same, and closing it offers a path to type.
    core.choose_export_file("x.json".into(), None);
    assert_eq!(lock(&opened).last().map(|o| o.1), Some(false));
    answer.send(None).unwrap();
    let chosen = first_event(&core, |e| match e {
        Event::ExportFileChosen(choice) => Some(choice.clone()),
        _ => None,
    });
    assert!(
        matches!(
            &chosen,
            crate::export_file::FileChoice::NoDialog { suggested }
                | crate::export_file::FileChoice::Cancelled { suggested }
                if suggested.ends_with("x.json")
        ),
        "{chosen:?}"
    );

    // A command with no dialog opened by the UI opens none here, off the UI's thread (on macOS, AppKit is
    // main-thread only): a path is asked for instead.
    let before = lock(&opened).len();
    core.send(Command::ChooseExportFile {
        name: "y.json".into(),
        shown: None,
    });
    let chosen = first_event(&core, |e| match e {
        Event::ExportFileChosen(choice) => Some(choice.clone()),
        _ => None,
    });
    assert!(
        matches!(&chosen, crate::export_file::FileChoice::NoDialog { suggested } if suggested.ends_with("y.json")),
        "{chosen:?}"
    );
    assert_eq!(lock(&opened).len(), before);
}

#[test]
fn the_open_dialog_opens_over_the_window_and_the_file_picked_is_read_elsewhere() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let opened: Arc<Mutex<Vec<(bool, std::thread::ThreadId)>>> = Arc::default();
    let (answer, user) = mpsc::channel::<Option<PathBuf>>();
    let user = Arc::new(Mutex::new(user));
    let mut config = env.config();
    config.pick_dialog = {
        let opened = Arc::clone(&opened);
        Arc::new(move |owner| {
            lock(&opened).push((owner.is_some(), std::thread::current().id()));
            let user = Arc::clone(&user);
            Box::pin(async move { lock(&user).recv().ok().flatten() })
        })
    };
    let core = start_with(config, || {});
    let file_of = |core: &Core| {
        first_event(core, |e| match e {
            Event::ImportFile(file) => Some(file.clone()),
            _ => None,
        })
    };
    core.choose_import_file(Some(&Window));
    // Opened here, over the window, and this thread did not wait for the user.
    assert_eq!(*lock(&opened), vec![(true, std::thread::current().id())]);
    let file = env.dir.path().join("servers.json");
    std::fs::write(&file, "{\"mcpServers\": {}}").unwrap();
    answer.send(Some(file.clone())).unwrap();
    assert_eq!(
        file_of(&core),
        crate::import_file::ImportFile::Read {
            shown: "servers.json".into(),
            text: "{\"mcpServers\": {}}".into()
        }
    );

    // Nearby: closing the dialog reads nothing; without a dialog from the UI, none is opened here.
    core.choose_import_file(None);
    answer.send(None).unwrap();
    assert!(matches!(
        file_of(&core),
        crate::import_file::ImportFile::Cancelled | crate::import_file::ImportFile::NoDialog
    ));
    let before = lock(&opened).len();
    core.send(Command::ChooseImportFile { shown: None });
    assert_eq!(file_of(&core), crate::import_file::ImportFile::NoDialog);
    assert_eq!(lock(&opened).len(), before);
    // A path typed or dropped is read the same way.
    core.send(Command::ReadImportFile(file));
    assert!(matches!(
        file_of(&core),
        crate::import_file::ImportFile::Read { .. }
    ));

    // Nearby: a pipe typed as the path (it would wait for a writer) is refused, not waited on.
    #[cfg(unix)]
    {
        let pipe = env.dir.path().join("pipe.json");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        core.send(Command::ReadImportFile(pipe));
        assert!(matches!(
            file_of(&core),
            crate::import_file::ImportFile::Failed { .. }
        ));
    }
}

#[test]
fn an_export_is_saved_where_the_user_chose_and_never_over_claudes_own_file() {
    let env = Env::new(&serde_json::json!({}), "{}", &[]);
    let core = env.start();
    let chosen = |core: &Core| {
        first_event(core, |e| match e {
            Event::ExportFileChosen(choice) => Some(choice.clone()),
            _ => None,
        })
    };
    let saved = |core: &Core| {
        first_event(core, |e| match e {
            Event::ExportSaved { shown, result, .. } => Some((shown.clone(), result.clone())),
            _ => None,
        })
    };
    // No dialog opened by the UI: a path to start from instead, and no dialog is opened here.
    core.send(Command::ChooseExportFile {
        name: "mcp-servers.json".into(),
        shown: None,
    });
    assert!(matches!(
        chosen(&core),
        crate::export_file::FileChoice::NoDialog { suggested } if suggested.ends_with("mcp-servers.json")
    ));
    let file = env.dir.path().join("home").join("servers.json");
    *lock(&env.chosen) = Some(file.clone());
    core.choose_export_file("servers.json".into(), None);
    assert_eq!(
        chosen(&core),
        crate::export_file::FileChoice::Chosen(file.clone())
    );
    core.send(Command::SaveExport {
        path: file.clone(),
        text: "{}".into(),
        replace: false,
    });
    assert_eq!(saved(&core), (file.to_string_lossy().into_owned(), Ok(())));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}\n");
    core.send(Command::SaveExport {
        path: file.clone(),
        text: "{\"again\": 1}".into(),
        replace: false,
    });
    assert_eq!(saved(&core).1, Err(crate::export_file::SaveError::Exists));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}\n");

    let claude_json = env.dir.path().join("cfg").join(".claude.json");
    let before = std::fs::read(&claude_json).unwrap();
    core.send(Command::SaveExport {
        path: claude_json.clone(),
        text: "{}".into(),
        replace: true,
    });
    assert!(matches!(
        saved(&core).1,
        Err(crate::export_file::SaveError::Refused(_))
    ));
    assert_eq!(std::fs::read(&claude_json).unwrap(), before);
}

#[test]
fn a_project_server_is_shown_but_not_editable() {
    let (env, key, _repo) = repo_env(serde_json::json!({}));
    let core = env.start();
    let (json, editable) = entry_of(&core, project_id(&key, "ps"));
    assert!(!editable);
    assert!(json.unwrap().contains("server.js"));
    let refused = plan_failure(
        &core,
        vec![Change::Delete {
            id: project_id(&key, "ps"),
        }],
    );
    assert!(refused.contains("repository"), "{refused}");
    let refused = plan_failure(
        &core,
        vec![Change::Replace {
            id: project_id(&key, "ps"),
            json: "{\"command\": \"x\"}".into(),
        }],
    );
    assert!(refused.contains("repository"), "{refused}");
}

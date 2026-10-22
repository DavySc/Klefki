//! Characterisation tests: every kind of change, the review the user sees for it and the files it leaves behind,
//! byte for byte, in `golden/*.txt`. They pin today's behaviour so code can be simplified without changing what the
//! user sees or what lands on disk. A difference fails the test; when the change is meant, run the tests with
//! `UPDATE_EXPECT=1` and review the updated files in the diff.
use super::*;
use expect_test::expect_file;

/// The review as the user sees it, in the order the dialog shows it.
fn render(plan: &PlanView) -> String {
    let mut out = String::new();
    let mut section = |title: &str, lines: &[String]| {
        if !lines.is_empty() {
            out.push_str(title.strip_suffix(':').unwrap_or(title));
            out.push_str(":\n");
            for line in lines {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
    };
    section("summary", &plan.summary);
    section("warnings", &plan.warnings);
    section("notes", &plan.notes);
    section("sessions", plan.sessions.as_slice());
    section("once saved", plan.restart.as_slice());
    section("already", &plan.already);
    for review in &plan.reviews {
        section(&format!("review {}", review.title), &review.lines);
    }
    for file in &plan.files {
        section(&format!("diff {}", file.path), &file.lines);
    }
    out.push_str(&format!(
        "button: {}{}\n",
        plan.action,
        if plan.destructive { " (dangerous)" } else { "" }
    ));
    if plan.nothing_to_do {
        out.push_str("nothing to do\n");
    }
    if plan.others_on {
        out.push_str("others on\n");
    }
    out
}

impl Env {
    /// `text` with this environment's folder, however it is spelled, as `<tmp>`, paths under it with `/`, and the
    /// time of a backup as `<time>`, so the files are the same on every system and every run.
    fn shown(&self, text: &str) -> String {
        let dir = self.dir.path();
        let mut spellings = vec![dir.to_string_lossy().into_owned()];
        if let Ok(real) = std::fs::canonicalize(dir) {
            spellings.push(real.to_string_lossy().into_owned());
        }
        let mut forms: Vec<String> = spellings
            .iter()
            .flat_map(|p| [p.replace('\\', "\\\\"), p.clone(), p.replace('\\', "/")])
            .collect();
        // The longest first: macOS's /private/var/… holds /var/….
        forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let mut text = text.replace("\r\n", "\n");
        for form in forms {
            text = text.replace(&form, "<tmp>");
        }
        let under = regex::Regex::new(r#"<tmp>[^\s"]*"#).unwrap();
        let text = under.replace_all(&text, |m: &regex::Captures| {
            m[0].replace("\\\\", "/").replace('\\', "/")
        });
        let time = regex::Regex::new(r"\d{1,2} [A-Z][a-z]{2} \d{4} at \d{2}:\d{2}").unwrap();
        time.replace_all(&text, "<time>").into_owned()
    }

    /// Every file a change may write, as it is now: Claude Code's configuration, the repository's settings and
    /// Klefki's own profile files. Files that are not there are left out.
    fn files_now(&self) -> String {
        let root = self.dir.path();
        let mut out = String::new();
        for path in [
            root.join("cfg").join(".claude.json"),
            root.join("repo")
                .join(".claude")
                .join("settings.local.json"),
            crate::profiles::path(&root.join("backups")),
            crate::profile_uses::path(&root.join("backups")),
        ] {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let name = path.strip_prefix(root).unwrap_or(&path);
                let name = name.to_string_lossy().replace('\\', "/");
                out.push_str(&format!("--- {name}\n{text}\n"));
            }
        }
        self.shown(&out)
    }
}

/// Prepares `changes`, applies the plan when there is something to do, and checks the review and the files after
/// against `golden/<name>.txt`.
fn check(env: &Env, core: &Core, changes: Vec<Change>, expected: expect_test::ExpectFile) {
    let plan = plan_for(core, changes);
    let mut out = format!("== review\n{}", env.shown(&render(&plan)));
    if !plan.nothing_to_do {
        let events = apply_and_wait(core, &plan);
        let outcome = events.iter().find_map(|e| match e {
            Event::Applied { changed, .. } => Some(format!("applied, changed: {changed}")),
            Event::ApplyFailed { error, .. } => Some(format!("failed: {error}")),
            _ => None,
        });
        out.push_str(&format!("== {}\n", env.shown(&outcome.unwrap_or_default())));
    }
    out.push_str("== files after\n");
    out.push_str(&env.files_now());
    expected.assert_eq(&out);
}

/// [`two_projects`], written as Claude Code writes it.
fn two_projects_env() -> (Env, Core) {
    let env = quiet_env(&serde_json::json!({}));
    write_config(&env, &two_projects("end"));
    let core = env.start();
    (env, core)
}

#[test]
fn golden_turn_a_user_server_off() {
    let (env, core) = two_projects_env();
    let change = Change::SetEnabled {
        id: user_id("gw"),
        enabled: false,
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/turn-off-user.txt"],
    );
}

#[test]
fn golden_turn_a_local_server_off() {
    let (env, core) = two_projects_env();
    let change = Change::SetEnabled {
        id: local_id("loc", "/work/a"),
        enabled: false,
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/turn-off-local.txt"],
    );
}

#[test]
fn golden_turn_on_a_server_that_is_on() {
    let (env, core) = two_projects_env();
    let change = Change::SetEnabled {
        id: user_id("gw"),
        enabled: true,
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/already-on.txt"],
    );
}

#[test]
fn golden_delete_a_user_server() {
    let (env, core) = two_projects_env();
    let change = Change::Delete { id: user_id("gw") };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/delete-user.txt"],
    );
}

#[test]
fn golden_delete_a_local_server() {
    let (env, core) = two_projects_env();
    let change = Change::Delete {
        id: local_id("loc", "/work/a"),
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/delete-local.txt"],
    );
}

/// Secrets typed into a stdio server's arguments, in the three ways they turn up. The review shows a server's
/// arguments in full, these too (the owner's decision of 2026-10-19: the user's own server, shown to the user),
/// while `klefki-cli list` hides them; the golden file pins this, so a change either way shows up.
#[test]
fn golden_duplicate_a_server_with_secrets_in_its_arguments() {
    let env = quiet_env(&serde_json::json!({}));
    let mut config = two_projects("end");
    config["mcpServers"]["args"] = serde_json::json!({
        "command": "npx",
        "args": ["-y", "args-mcp", "--api-key", "sk-live-ARGSECRET123456", "--token=tok-SECRET-98765", "ghp_abcdefghijklmnop1234"]
    });
    write_config(&env, &config);
    let core = env.start();
    let duplicate = Change::Duplicate {
        id: user_id("args"),
        name: "args-copy".to_owned(),
        targets: vec![Target::User],
        adjust: crate::adjust::Adjust::default(),
    };
    check(
        &env,
        &core,
        vec![duplicate],
        expect_file!["golden/duplicate-secret-arguments.txt"],
    );
}

#[test]
fn golden_edit_a_server_keeping_a_hidden_value() {
    let (env, core) = two_projects_env();
    let change = Change::Replace {
        id: user_id("gw"),
        json: r#"{"type": "stdio", "command": "node", "args": ["gw.js"], "env": {"TOKEN": "<hidden>", "MODE": "fast"}}"#
            .to_owned(),
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/edit-user.txt"],
    );
}

#[test]
fn golden_rename_a_server() {
    let (env, core) = two_projects_env();
    let change = Change::Rename {
        id: local_id("loc", "/work/a"),
        name: "Loc".to_owned(),
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/rename-local.txt"],
    );
}

#[test]
fn golden_duplicate_a_server_to_two_places() {
    let (env, core) = two_projects_env();
    let change = Change::Duplicate {
        id: user_id("gw"),
        name: "gw-copy".to_owned(),
        targets: vec![Target::User, Target::Local("/work/b".to_owned())],
        adjust: crate::adjust::Adjust::default(),
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/duplicate-user.txt"],
    );
}

#[test]
fn golden_move_a_local_server_to_the_user() {
    let (env, core) = two_projects_env();
    let change = Change::Move {
        id: local_id("loc", "/work/a"),
        to: Target::User,
    };
    check(
        &env,
        &core,
        vec![change],
        expect_file!["golden/move-local-to-user.txt"],
    );
}

#[test]
fn golden_add_a_stdio_server_and_an_http_server() {
    let (env, core) = two_projects_env();
    let changes = vec![
        Change::Add {
            name: "files".to_owned(),
            json: r#"{"command": "npx", "args": ["-y", "files-mcp", "/work"], "env": {"API_KEY": "${FILES_KEY}"}}"#
                .to_owned(),
            targets: vec![Target::User],
            secret: None,
        },
        Change::Add {
            name: "docs".to_owned(),
            json: r#"{"type": "http", "url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}"#
                .to_owned(),
            targets: vec![Target::Local("/work/a".to_owned())],
            secret: None,
        },
    ];
    check(&env, &core, changes, expect_file!["golden/add-two.txt"]);
}

#[test]
fn golden_several_changes_in_one_plan() {
    let (env, core) = two_projects_env();
    let changes = vec![
        Change::SetEnabled {
            id: local_id("loc", "/work/a"),
            enabled: false,
        },
        Change::Delete { id: user_id("gw") },
    ];
    check(&env, &core, changes, expect_file!["golden/mixed.txt"]);
}

#[test]
fn golden_approve_a_project_server() {
    let (env, key, _) = repo_env(serde_json::json!({}));
    let core = env.start();
    check(
        &env,
        &core,
        approve(&key),
        expect_file!["golden/approve-project.txt"],
    );
}

#[test]
fn golden_migrate_an_old_switch_off_list() {
    let env = quiet_env(&serde_json::json!({}));
    write_config(&env, &legacy_config());
    let core = env.start();
    check(
        &env,
        &core,
        vec![migrate("/work/a")],
        expect_file!["golden/migrate-legacy.txt"],
    );
}

#[test]
fn golden_use_a_profile_alone_then_stop_using_it() {
    let env = quiet_env(&serde_json::json!({}));
    write_config(&env, &designs_config());
    let core = env.start();
    check(
        &env,
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
        expect_file!["golden/use-profile-alone.txt"],
    );
    check(
        &env,
        &core,
        vec![stop_profile("Writing", None, false)],
        expect_file!["golden/stop-profile.txt"],
    );
}

#[test]
fn golden_restore_a_backup() {
    let (env, core) = two_projects_env();
    let plan = plan_for(&core, vec![Change::Delete { id: user_id("gw") }]);
    applied(&core, &plan);
    let backup = newest_backup(&core);
    verify_passes(&core);
    check(
        &env,
        &core,
        vec![Change::Restore { backup }],
        expect_file!["golden/restore.txt"],
    );
}

#[test]
fn golden_add_a_server_that_signs_in_with_oauth() {
    let (env, _, _) = oauth_add_env(ADD_JSON_OK);
    let store = Arc::new(crate::secrets::MemoryStore::default());
    let core = with_store(&env, Arc::clone(&store));
    collect(&core, |e| !snapshots(e).is_empty());
    let plan = plan_for(
        &core,
        vec![oauth_add("sso2", vec![Target::User], Some(CLIENT_SECRET))],
    );
    let mut out = format!("== review\n{}", env.shown(&render(&plan)));
    core.send(Command::Apply(plan.id));
    env.claude_writes_when_asked(None, "sso2", OAUTH_DEF);
    let events = collect(&core, |e| {
        e.iter()
            .any(|x| matches!(x, Event::Applied { .. } | Event::ApplyFailed { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Applied { changed: true, .. })),
        "{events:#?}"
    );
    out.push_str("== Claude Code ran\n");
    for (argv, _, secret) in env.changes_run() {
        out.push_str(&format!(
            "  {}{}\n",
            argv.join(" "),
            match secret {
                Some(hash) if hash == sha256(CLIENT_SECRET) =>
                    " (the client secret, in MCP_CLIENT_SECRET)",
                Some(_) => " (another secret, in MCP_CLIENT_SECRET)",
                None => "",
            }
        ));
    }
    out.push_str("== files after\n");
    out.push_str(&env.files_now());
    assert!(!out.contains(CLIENT_SECRET));
    expect_file!["golden/add-oauth.txt"].assert_eq(&out);
}

//! Engine tests: signing in to a server.
use super::*;

const LOGIN_PAGE: &[u8] =
    include_bytes!("../../../../../fixtures/mcp-login/synthetic-waiting-https.txt");
const LOGIN_HTTP_PAGE: &[u8] =
    include_bytes!("../../../../../fixtures/mcp-login/real-waiting-http.txt");
const LOGIN_SIGNED_IN: &[u8] =
    include_bytes!("../../../../../fixtures/mcp-login/real-signed-in-tail.txt");
const LOGIN_PORT_IN_USE: &[u8] =
    include_bytes!("../../../../../fixtures/mcp-login/real-port-in-use.txt");

fn sign_in_steps(events: &[Event], name: &str) -> Vec<SignInStep> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::SignIn { id, step } if id.name == name => Some(step.clone()),
            _ => None,
        })
        .collect()
}

fn sign_in_ended(events: &[Event], name: &str) -> bool {
    sign_in_steps(events, name).iter().any(|s| {
        matches!(
            s,
            SignInStep::SignedIn | SignInStep::Failed(_) | SignInStep::Cancelled
        )
    })
}

/// A user http server `web`, a local http server `loc` in a real project folder, a stdio server and an unapproved
/// project http server; `login` is the rule for `claude mcp login`.
fn sign_in_env(login: &str, files: &[(&str, &[u8])]) -> (Env, String) {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"repo":{"type":"http","url":"https://repo.example.com/mcp"}}}"#,
    )
    .unwrap();
    let project = project.to_str().unwrap().replace('\\', "/");
    let mut all: Vec<(&str, &[u8])> = vec![("connected.txt", CONNECTED)];
    all.extend_from_slice(files);
    let env = Env::new(
        &serde_json::json!({
            "mcpServers": {
                "web": http("https://mcp.example.com/mcp"),
                "cmd": {"type": "stdio", "command": "x"}
            },
            "projects": {project.clone(): {"mcpServers": {"loc": http("https://loc.example.com/mcp")}}}
        }),
        &format!(r#"{{"rules":[{login},{{"args":["mcp","get"],"stdout_file":"connected.txt"}}]}}"#),
        &all,
    );
    // Keep the project folder for as long as the environment.
    std::mem::forget(dir);
    (env, project)
}

fn started(core: &Core) -> Arc<Snapshot> {
    let events = collect(core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    events
        .iter()
        .find_map(|e| match e {
            Event::Snapshot(s) => Some(Arc::clone(s)),
            _ => None,
        })
        .unwrap()
}

#[test]
fn signing_in_runs_claude_in_the_servers_folder_opens_the_checked_page_and_checks_again() {
    let mut transcript = LOGIN_PAGE.to_vec();
    transcript.extend_from_slice(LOGIN_SIGNED_IN);
    let (env, project) = sign_in_env(
        r#"{"args":["mcp","login","*","--no-browser"],"stdout_file":"login.txt","exit_code":0}"#,
        &[("login.txt", &transcript)],
    );
    let core = env.start();
    started(&core);
    let gets_before = env.runs(&["mcp", "get", "loc"]);
    core.send(Command::SignIn(local_id("loc", &project)));
    let events = collect(&core, |e| {
        sign_in_ended(e, "loc")
            && e.iter().skip_while(|x| !matches!(x, Event::SignIn { step: SignInStep::SignedIn, .. }))
                .any(|x| matches!(x, Event::Status { id, status: Status::Connected, .. } if id.name == "loc"))
    });
    let steps = sign_in_steps(&events, "loc");
    assert_eq!(
        steps[0],
        SignInStep::Starting {
            wait: Duration::from_secs(20)
        }
    );
    let SignInStep::Waiting { host, link, opened } = &steps[1] else {
        panic!("{steps:?}");
    };
    assert_eq!(host, "auth.example.com");
    assert!(opened);
    assert_eq!(*lock(&env.opened), vec![link.clone()]);
    assert!(link.starts_with("https://auth.example.com/authorize?"));
    assert_eq!(steps.last(), Some(&SignInStep::SignedIn));
    // Confirmed by Claude's own check.
    assert!(env.runs(&["mcp", "get", "loc"]) > gets_before);
    // Run as `claude mcp login loc --no-browser`, in the project's folder (Claude finds a local server there).
    let log: Vec<serde_json::Value> = std::fs::read_to_string(&env.log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let login = log
        .iter()
        .find(|l| l["event"] == "start" && l["argv"][1] == "login")
        .unwrap();
    assert_eq!(
        login["argv"],
        serde_json::json!(["mcp", "login", "loc", "--no-browser"])
    );
    assert_eq!(
        std::fs::canonicalize(login["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&project).unwrap()
    );
    assert_eq!(login["secret_set"], false);
}

#[test]
fn a_user_server_signs_in_from_the_home_folder_and_a_failure_says_claudes_reason() {
    let (env, _) = sign_in_env(
        r#"{"args":["mcp","login","web","--no-browser"],"stdout_file":"login.txt","exit_code":1}"#,
        &[("login.txt", LOGIN_PORT_IN_USE)],
    );
    let core = env.start();
    started(&core);
    core.send(Command::SignIn(user_id("web")));
    let events = collect(&core, |e| sign_in_ended(e, "web"));
    let Some(SignInStep::Failed(why)) = sign_in_steps(&events, "web").pop() else {
        panic!("{events:?}");
    };
    assert!(
        why.said
            .as_deref()
            .is_some_and(|s| s.contains("port 8766 is already in use")),
        "{why:?}"
    );
    assert!(why.can_retry, "{why:?}");
    assert!(lock(&env.opened).is_empty());
    // The sign-in that ended is over as soon as it says so: trying again at once starts again.
    core.send(Command::SignIn(user_id("web")));
    let events = collect(&core, |e| sign_in_ended(e, "web"));
    let steps = sign_in_steps(&events, "web");
    assert!(
        matches!(steps.as_slice(), [SignInStep::Starting { .. }, .., SignInStep::Failed(w)] if w.can_retry),
        "{steps:?}"
    );
}

#[test]
fn only_http_and_sse_servers_that_claude_uses_sign_in_and_nothing_runs_otherwise() {
    let (env, project) = sign_in_env(
        r#"{"args":["mcp","login"],"stdout_file":"login.txt","exit_code":0}"#,
        &[("login.txt", LOGIN_SIGNED_IN)],
    );
    let core = env.start();
    started(&core);
    for (id, says) in [
        (user_id("cmd"), "HTTP or SSE"),
        (
            ServerId::new(Scope::Project, Some(&project), "repo"),
            "Approve",
        ),
        (user_id("gone"), "no longer"),
    ] {
        let name = id.name.clone();
        core.send(Command::SignIn(id));
        let events = collect(&core, |e| sign_in_ended(e, &name));
        let steps = sign_in_steps(&events, &name);
        assert!(
            matches!(steps.as_slice(), [SignInStep::Failed(w)] if w.title.contains(says) && !w.can_retry),
            "{name}: {steps:?}"
        );
    }
    assert_eq!(env.runs(&["mcp", "login"]), 0);
}

#[test]
fn a_page_that_is_not_https_is_never_opened() {
    let (env, _) = sign_in_env(
        r#"{"args":["mcp","login"],"stdout_file":"login.txt","hang":false,"exit_code":0}"#,
        &[("login.txt", LOGIN_HTTP_PAGE)],
    );
    let core = env.start();
    started(&core);
    core.send(Command::SignIn(user_id("web")));
    let events = collect(&core, |e| sign_in_ended(e, "web"));
    let steps = sign_in_steps(&events, "web");
    assert!(lock(&env.opened).is_empty(), "{steps:?}");
    assert!(
        !steps
            .iter()
            .any(|s| matches!(s, SignInStep::Waiting { .. })),
        "{steps:?}"
    );
}

#[test]
fn one_sign_in_at_a_time_and_cancel_stops_claude() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let (env, project) = sign_in_env(
        &format!(
            r#"{{"args":["mcp","login"],"hang":true,"write_pid_to":{}}}"#,
            serde_json::Value::from(pid_file.to_str().unwrap())
        ),
        &[],
    );
    let core = env.start();
    started(&core);
    core.send(Command::SignIn(user_id("web")));
    collect(&core, |e| {
        sign_in_steps(e, "web")
            .iter()
            .any(|s| matches!(s, SignInStep::Starting { .. }))
    });
    let pid = wait_for_pid(&pid_file);
    // Another one, and the same one again, are refused while it runs.
    core.send(Command::SignIn(local_id("loc", &project)));
    core.send(Command::SignIn(user_id("web")));
    let events = collect(&core, |e| {
        sign_in_ended(e, "loc")
            && sign_in_steps(e, "web")
                .iter()
                .any(|s| matches!(s, SignInStep::Failed(_)))
    });
    assert!(
        matches!(sign_in_steps(&events, "loc").as_slice(), [SignInStep::Failed(w)] if w.title.contains("already signing in to web")),
        "{events:?}"
    );
    // Pasting before the page is there is refused, not typed.
    core.send(Command::SignInPaste {
        id: user_id("web"),
        address: "http://localhost:1/callback?code=x&state=y".into(),
    });
    let events = collect(&core, |e| {
        sign_in_steps(e, "web")
            .iter()
            .any(|s| matches!(s, SignInStep::PasteRefused(_)))
    });
    assert!(!sign_in_ended(&events, "web"));
    core.send(Command::CancelSignIn(user_id("web")));
    collect(&core, |e| {
        sign_in_steps(e, "web").contains(&SignInStep::Cancelled)
    });
    assert!(gone_soon(pid), "claude {pid} still runs");
    // Once it ended, another sign-in may start.
    core.send(Command::SignIn(local_id("loc", &project)));
    collect(&core, |e| {
        sign_in_steps(e, "loc")
            .iter()
            .any(|s| matches!(s, SignInStep::Starting { .. }))
    });
    core.send(Command::CancelSignIn(local_id("loc", &project)));
    collect(&core, |e| {
        sign_in_steps(e, "loc").contains(&SignInStep::Cancelled)
    });
}

#[test]
fn closing_klefki_stops_a_running_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let (env, _) = sign_in_env(
        &format!(
            r#"{{"args":["mcp","login"],"hang":true,"write_pid_to":{}}}"#,
            serde_json::Value::from(pid_file.to_str().unwrap())
        ),
        &[],
    );
    let core = env.start();
    started(&core);
    core.send(Command::SignIn(user_id("web")));
    let pid = wait_for_pid(&pid_file);
    drop(core);
    assert!(
        gone_soon(pid),
        "claude {pid} still runs after Klefki closed"
    );
}

#[test]
fn open_again_opens_the_running_sign_ins_page_again() {
    let dir = tempfile::tempdir().unwrap();
    let line = dir.path().join("line.txt");
    // Claude prints the page and waits for a pasted address.
    let (env, _) = sign_in_env(
        &format!(
            r#"{{"args":["mcp","login"],"require_tty":true,"stdout_file":"login.txt","read_line_to":{}}}"#,
            serde_json::Value::from(line.to_str().unwrap())
        ),
        &[("login.txt", LOGIN_PAGE)],
    );
    let core = env.start();
    started(&core);
    core.send(Command::SignIn(user_id("web")));
    collect(&core, |e| {
        sign_in_steps(e, "web")
            .iter()
            .any(|s| matches!(s, SignInStep::Waiting { .. }))
    });
    core.send(Command::OpenSignInPage(user_id("web")));
    let events = collect(&core, |e| {
        sign_in_steps(e, "web")
            .iter()
            .any(|s| matches!(s, SignInStep::Waiting { .. }))
    });
    assert!(!sign_in_ended(&events, "web"));
    let opened = lock(&env.opened).clone();
    assert_eq!(opened.len(), 2);
    assert_eq!(opened[0], opened[1]);
    core.send(Command::CancelSignIn(user_id("web")));
    collect(&core, |e| {
        sign_in_steps(e, "web").contains(&SignInStep::Cancelled)
    });
}

#[test]
fn open_again_opens_only_the_page_claude_printed() {
    // Claude prints the page and ends.
    let (env, _) = sign_in_env(
        r#"{"args":["mcp","login"],"stdout_file":"login.txt","exit_code":0}"#,
        &[("login.txt", LOGIN_PAGE)],
    );
    let core = env.start();
    started(&core);
    // Before any sign-in: nothing to open.
    core.send(Command::OpenSignInPage(user_id("web")));
    core.send(Command::SignIn(user_id("web")));
    collect(&core, |e| sign_in_ended(e, "web"));
    assert_eq!(lock(&env.opened).len(), 1);
    // After it ended: nothing is opened again.
    core.send(Command::OpenSignInPage(user_id("web")));
    core.send(Command::Refresh { live_all: false });
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    assert_eq!(lock(&env.opened).len(), 1);
}

fn wait_for_pid(file: &Path) -> u32 {
    let until = Instant::now() + WAIT;
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|s| s.parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < until, "claude never started");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn gone_soon(pid: u32) -> bool {
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if !crate::cli::testutil::pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

//! End-to-end tests of `klefki-cli`: the real binary against a temporary `CLAUDE_CONFIG_DIR` and the `fake-claude`
//! stand-in. Never the real `~/.claude.json`, `~/.claude/` or Claude CLI.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

/// The built `fake-claude` binary (built on first use, as in core's tests).
fn fake_claude() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo)
            .args(["build", "-p", "fake-claude", "--message-format=json"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run cargo to build fake-claude");
        assert!(out.status.success(), "building fake-claude failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "fake-claude")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported the fake-claude executable")
    })
}

// Secrets typed into a stdio server's arguments, in the three ways they turn up. None may be printed.
const API_KEY: &str = "sk-live-ARGSECRET123456";
const TOKEN_FLAG: &str = "--token=tok-SECRET-98765";
const GHP: &str = "ghp_abcdefghijklmnop1234";
const HEADER: &str = "Authorization: Bearer hdr-SECRET-abcdef123";

fn get_output(name: &str, status: &str, extra: &str) -> String {
    format!(
        "{name}:\n  Scope: User config (available in all your projects)\n  Status: {status}\n{extra}  Type: stdio\n  Command: npx\n\nTo remove this server, run: claude mcp remove {name} -s user\n"
    )
}

/// A configuration with every kind of row: user servers (one on and connected, one failing, one off everywhere),
/// a local server that needs auth, and a project server, all in a temporary folder.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let project = root.join("Repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        let key = project.to_string_lossy().replace('\\', "/");
        let claude_json = serde_json::json!({
            "mcpServers": {
                "alpha": {"type": "stdio", "command": "npx", "args": ["-y", "alpha-mcp", "--api-key", API_KEY, TOKEN_FLAG, GHP]},
                "beta": {"type": "http", "url": "http://127.0.0.1:9/mcp"},
                "delta": {"type": "sse", "url": "https://delta.example.com/sse"},
                "remote": {"command": "npx", "args": ["mcp-remote", "https://r.example.com/mcp", "--header", HEADER]},
            },
            "projects": {
                key.clone(): {
                    "mcpServers": {"gamma": {"type": "http", "url": "https://gamma.example.com/mcp"}},
                    "disabledMcpServers": ["delta"],
                }
            }
        });
        std::fs::write(
            root.join("cfg").join(".claude.json"),
            serde_json::to_vec_pretty(&claude_json).unwrap(),
        )
        .unwrap();
        std::fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers": {"docs": {"command": "node", "args": ["tools/docs-mcp.js"]}}}"#,
        )
        .unwrap();
        let files: [(&str, String); 5] = [
            ("version.txt", "2.1.302 (Claude Code)\n".into()),
            ("alpha.txt", get_output("alpha", "✔ Connected", "")),
            (
                "beta.txt",
                get_output(
                    "beta",
                    "✘ Failed to connect",
                    "  Issue: ECONNREFUSED: Unable to connect. Is the computer able to access the url?\n",
                ),
            ),
            (
                "gamma.txt",
                get_output("gamma", "! Needs authentication", ""),
            ),
            ("docs.txt", get_output("docs", "✔ Connected", "")),
        ];
        for (name, text) in files {
            std::fs::write(root.join(name), text).unwrap();
        }
        std::fs::write(
            root.join("scenario.json"),
            r#"{"rules":[
                {"args":["--version"],"stdout_file":"version.txt"},
                {"args":["mcp","get","alpha"],"stdout_file":"alpha.txt"},
                {"args":["mcp","get","beta"],"stdout_file":"beta.txt"},
                {"args":["mcp","get","gamma"],"stdout_file":"gamma.txt"},
                {"args":["mcp","get","delta"],"stdout_file":"alpha.txt"},
                {"args":["mcp","get","remote"],"stdout_file":"alpha.txt"},
                {"args":["mcp","get","docs"],"stdout_file":"docs.txt"}]}"#,
        )
        .unwrap();
        // A copy named like the real CLI, on PATH and in ~/.local/bin, so a command that went looking for Claude
        // would find this one and leave a trace in the log.
        let bin = root.join("home").join(".local").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        std::fs::copy(fake_claude(), bin.join(exe)).unwrap();
        runnable(&bin.join(exe));
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn project(&self) -> String {
        self.root()
            .join("Repo")
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// The project as a group heading names it for a `klefki-cli` whose home folder is `home`: `~/…` under it, the
    /// full path otherwise. On Windows `HOME` is not redirected (see `command`), so the home there is the real
    /// profile, which often holds the temporary folder.
    fn shown_project(&self, home: Option<&Path>) -> String {
        let project = self.project();
        let Some(home) = home else {
            return project;
        };
        let home = home.to_string_lossy().replace('\\', "/");
        let home = home.trim_end_matches('/');
        let fold = |s: &str| {
            if cfg!(windows) || cfg!(target_os = "macos") {
                s.to_ascii_lowercase()
            } else {
                s.to_owned()
            }
        };
        // Compared folded, but the rest keeps its case, as the CLI prints it.
        match project.get(..home.len()) {
            Some(head) if !home.is_empty() && fold(head) == fold(home) => {
                match &project[home.len()..] {
                    rest if rest.starts_with('/') => format!("~{rest}"),
                    _ => project,
                }
            }
            _ => project,
        }
    }

    /// The home folder the commands from `command` see.
    fn home(&self) -> Option<PathBuf> {
        if cfg!(windows) {
            std::env::home_dir()
        } else {
            Some(self.root().join("home"))
        }
    }

    /// The environment every command runs with.
    fn vars(&self) -> [(&'static str, PathBuf); 7] {
        let root = self.root();
        [
            ("CLAUDE_CONFIG_DIR", root.join("cfg")),
            // The organisation's folder, not the machine's: empty unless a test writes `org/…`.
            ("KLEFKI_MANAGED_DIR", root.join("org")),
            // Klefki's own folder (backups, profiles), not the machine's.
            ("KLEFKI_DATA_DIR", root.join("data")),
            // Redirects the home folder on Linux and macOS. Windows finds it through the registry; a changed
            // `USERPROFILE` only breaks that lookup (CI: every user server "not checked: no home folder").
            ("HOME", root.join("home")),
            ("PATH", root.join("home").join(".local").join("bin")),
            ("FAKE_CLAUDE_SCENARIO", root.join("scenario.json")),
            ("FAKE_CLAUDE_LOG", root.join("log.jsonl")),
        ]
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_klefki-cli"));
        cmd.args(args).envs(self.vars()).stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run klefki-cli")
    }

    /// `status` with the fake CLI named explicitly.
    fn status(&self, extra: &[&str]) -> Output {
        let claude = fake_claude().to_string_lossy().into_owned();
        let mut args = vec!["status", "--claude", claude.as_str()];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    /// The servers the fake CLI was asked about (`mcp get <name>`), in order.
    fn gets(&self) -> Vec<String> {
        std::fs::read_to_string(self.root().join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|l| l["event"] == "start")
            .filter_map(|l| {
                let argv = l["argv"].as_array()?.clone();
                (argv.len() == 3 && argv[0] == "mcp" && argv[1] == "get")
                    .then(|| argv[2].as_str().unwrap().to_owned())
            })
            .collect()
    }

    fn any_run(&self) -> bool {
        self.root().join("log.jsonl").exists()
    }
}

/// Waits until the just-copied program at `path` can be started. On Linux, a test on another thread that starts a
/// process while this one writes the copy hands the open file to that child for a moment, and until the child has
/// started its own program, starting the copy fails with "text file busy" (ETXTBSY). `klefki-cli` would then find
/// no usable Claude CLI and exit 3.
fn runnable(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match Command::new(path)
            .arg("--version")
            .env_remove("FAKE_CLAUDE_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{} stays busy",
                    path.display()
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("cannot start {}: {e}", path.display()),
            Ok(_) => return,
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("output is UTF-8")
}

/// The line of `out` that starts (after indentation) with `name`.
fn line_of<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no line for {name} in:\n{out}"))
}

#[test]
fn list_shows_every_server_grouped_by_scope_and_project_without_running_anything() {
    let env = Env::new();
    let out = env.run(&["list"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert_groups_in_order(&stdout, &env.shown_project(env.home().as_deref()));
    let alpha = line_of(&stdout, "alpha");
    assert!(alpha.contains(" on "), "{alpha}");
    assert!(
        alpha.contains("stdio") && alpha.contains("npx -y alpha-mcp --api-key …"),
        "{alpha}"
    );
    assert!(line_of(&stdout, "delta").contains(" off "), "{stdout}");
    let gamma = line_of(&stdout, "gamma");
    assert!(gamma.contains("http") && gamma.contains("https://gamma.example.com/mcp"));
    assert!(
        line_of(&stdout, "docs").contains("pending approval"),
        "{stdout}"
    );
    // Columns line up: the transport starts at the same place on every row.
    let at = |name: &str| {
        line_of(&stdout, name)
            .find(" http")
            .or_else(|| line_of(&stdout, name).find(" stdio"))
            .or_else(|| line_of(&stdout, name).find(" sse"))
    };
    assert_eq!(at("alpha"), at("beta"), "{stdout}");
    assert_eq!(at("alpha"), at("gamma"), "{stdout}");
    assert!(!env.any_run(), "list started the Claude CLI");
}

/// `out` has a User group, then the Local and Project groups headed by `project`.
fn assert_groups_in_order(out: &str, project: &str) {
    let user = out.find("User").expect("a User group");
    let local = out
        .find(&format!("Local in {project}"))
        .unwrap_or_else(|| panic!("a Local group naming {project} in:\n{out}"));
    let project = out
        .find(&format!("Project in {project}"))
        .unwrap_or_else(|| panic!("a Project group naming {project} in:\n{out}"));
    assert!(user < local && local < project, "{out}");
}

/// The project under the home folder, as the temporary folder often is on Windows: headings shorten it to `~/…`.
/// Not on Windows, where a test cannot move the home folder (see `Env::command`).
#[cfg(not(windows))]
#[test]
fn list_shortens_a_project_under_the_home_folder() {
    let env = Env::new();
    let out = env
        .command(&["list"])
        .env("HOME", env.root())
        .output()
        .expect("run klefki-cli");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = env.shown_project(Some(env.root()));
    assert_eq!(shown, "~/Repo");
    assert_groups_in_order(&text(&out.stdout), &shown);
    let json = env
        .command(&["list", "--json"])
        .env("HOME", env.root())
        .output()
        .expect("run klefki-cli");
    let json = text(&json.stdout);
    assert!(json.contains(&format!("\"{}\"", env.project())), "{json}");
}

#[test]
fn list_json_has_one_object_per_server_with_stable_fields() {
    let env = Env::new();
    let out = env.run(&["list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["schema"], 1);
    let servers = v["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 6, "{v:#}");
    let find = |name: &str| servers.iter().find(|s| s["name"] == name).unwrap();
    assert_eq!(find("alpha")["scope"], "user");
    assert_eq!(find("alpha")["transport"], "stdio");
    assert_eq!(find("alpha")["enabled"], true);
    assert_eq!(find("alpha")["project"], serde_json::Value::Null);
    assert_eq!(find("delta")["enabled"], false);
    assert_eq!(find("gamma")["scope"], "local");
    assert_eq!(find("gamma")["project"], env.project());
    assert_eq!(find("gamma")["target"], "https://gamma.example.com/mcp");
    assert_eq!(find("docs")["scope"], "project");
    assert_eq!(find("docs")["approval"], "pending");
    assert!(v["warnings"].as_array().unwrap().is_empty());
    assert!(!env.any_run());
}

#[test]
fn list_and_status_show_what_the_organisation_sets_and_what_it_replaces() {
    let env = Env::new();
    let org = env.root().join("org");
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(
        org.join("managed-settings.json"),
        r#"{"managedMcpServers": {
            "beta": {"type": "http", "url": "https://beta.example.com/mcp"},
            "wiki": {"type": "sse", "url": "https://wiki.example.com/sse"}}}"#,
    )
    .unwrap();
    let out = env.run(&["list"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    // First: these win over every other server of the same name.
    assert!(stdout.starts_with("Organisation\n"), "{stdout}");
    let wiki = line_of(&stdout, "wiki");
    assert!(
        wiki.contains("https://wiki.example.com/sse") && wiki.contains("set by your organisation"),
        "{wiki}"
    );
    let beta = stdout
        .lines()
        .find(|l| l.contains("beta") && l.contains("127.0.0.1"))
        .unwrap_or_else(|| panic!("{stdout}"));
    // Said once, in the state column, with the reason as the note.
    assert!(
        beta.contains("  not loaded  ") && beta.contains("(Your organisation sets"),
        "{beta}"
    );
    assert_eq!(beta.matches("not loaded").count(), 1, "{beta}");

    let out = env.run(&["list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let servers = v["servers"].as_array().unwrap();
    let wiki = servers.iter().find(|s| s["name"] == "wiki").unwrap();
    assert_eq!(wiki["scope"], "organisation");
    assert!(
        wiki["managed"]
            .as_str()
            .unwrap()
            .ends_with("managed-settings.json")
    );
    assert_eq!(wiki["not_loaded"], serde_json::Value::Null);
    let user_beta = servers
        .iter()
        .find(|s| s["name"] == "beta" && s["scope"] == "user")
        .unwrap();
    assert!(
        user_beta["not_loaded"]
            .as_str()
            .unwrap()
            .contains("same name")
    );
    assert!(!env.any_run());

    // Checks neither: the organisation's is not one Klefki checks, and the user's beta is not what Claude Code runs.
    let out = env.status(&[]);
    let stdout = text(&out.stdout);
    assert!(!env.gets().contains(&"beta".to_owned()), "{stdout}");
    assert!(!env.gets().contains(&"wiki".to_owned()), "{stdout}");
    assert!(
        line_of(&stdout, "wiki").contains("your organisation sets it"),
        "{stdout}"
    );
}

#[test]
fn a_named_server_klefki_does_not_check_has_its_own_exit_code() {
    // P9: an organisation's server is one Klefki leaves to Claude Code's /mcp. Named, nothing is known about it: not
    // a pass (0), not a failure (1), but 5, so a script can tell "not checked" from both.
    let env = Env::new();
    let org = env.root().join("org");
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(
        org.join("managed-settings.json"),
        r#"{"managedMcpServers": {
            "beta": {"type": "http", "url": "https://beta.example.com/mcp"},
            "wiki": {"type": "sse", "url": "https://wiki.example.com/sse"}}}"#,
    )
    .unwrap();
    let out = env.status(&["wiki"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(5), "{stdout}{}", text(&out.stderr));
    assert!(line_of(&stdout, "wiki").contains("not checked"), "{stdout}");
    // Beside one that connects, still 5: one named was not checked.
    let out = env.status(&["wiki", "alpha"]);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stdout));
    // Review of #100: `beta` names the organisation's server and the user's it replaces. Claude Code runs the
    // organisation's, which Klefki does not check, so the answer is 5, not a failure for the replaced one.
    let out = env.status(&["beta"]);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stdout));
    let out = env.status(&["beta", "--json"]);
    assert_eq!(out.status.code(), Some(5));
    assert!(!env.gets().contains(&"wiki".to_owned()), "{:?}", env.gets());
    assert!(!env.gets().contains(&"beta".to_owned()), "{:?}", env.gets());
}

#[test]
fn a_named_server_the_organisation_does_not_allow_fails_the_run() {
    // The nearby variant: the organisation's allowed list leaves beta out, and no server of its name loads in its
    // place. Claude Code runs no beta at all, so naming it fails the run.
    let env = Env::new();
    let org = env.root().join("org");
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(
        org.join("managed-settings.json"),
        r#"{"allowedMcpServers": [{"serverName": "alpha"}]}"#,
    )
    .unwrap();
    let out = env.status(&["beta"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}{}", text(&out.stderr));
    assert!(line_of(&stdout, "beta").contains("Not loaded"), "{stdout}");
    assert!(stdout.ends_with("1 not loaded.\n"), "{stdout}");
    assert!(env.gets().is_empty(), "{:?}", env.gets());
}

/// Adds a user server `eps` that reads an unset variable, failing when checked.
fn with_unset_variable(env: &Env) {
    let file = env.root().join("cfg").join(".claude.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    v["mcpServers"]["eps"] = serde_json::json!({
        "type": "http",
        "url": "http://127.0.0.1:9/mcp",
        "headers": {"X-Team": "${KLEFKI_TEST_NEVER_SET_EPS}"},
    });
    std::fs::write(&file, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let failed = get_output("eps", "✘ Failed to connect", "");
    std::fs::write(env.root().join("eps.txt"), failed).unwrap();
    std::fs::write(
        env.root().join("scenario.json"),
        r#"{"rules":[{"args":["--version"],"stdout_file":"version.txt"},
            {"args":["mcp","get","eps"],"stdout_file":"eps.txt"},
            {"args":["mcp","get"],"stdout_file":"alpha.txt"}]}"#,
    )
    .unwrap();
}

#[test]
fn status_says_what_in_a_failing_definition_keeps_it_from_starting() {
    // P3's pre-flight findings, which the app shows beside a row, under a server that did not connect.
    let env = Env::new();
    with_unset_variable(&env);
    let out = env.status(&["eps"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    let under: Vec<&str> = stdout
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("eps "))
        .skip(1)
        .take_while(|l| l.starts_with("    "))
        .collect();
    let problem = under
        .iter()
        .position(|l| {
            l.trim_start().starts_with("eps: ") && l.contains("KLEFKI_TEST_NEVER_SET_EPS")
        })
        .unwrap_or_else(|| panic!("no finding under eps:\n{stdout}"));
    assert!(
        under
            .get(problem + 1)
            .is_some_and(|l| l.trim_start().starts_with("- ")),
        "the fix follows the problem:\n{stdout}"
    );
    let out = env.status(&["eps", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let found = &v["servers"][0]["preflight"][0];
    assert_eq!(found["kind"], "unset_variable", "{v:#}");
    assert!(
        found["problem"]
            .as_str()
            .unwrap()
            .contains("KLEFKI_TEST_NEVER_SET_EPS")
    );
    assert!(found["fix"].is_string(), "{v:#}");
}

#[test]
fn a_server_that_connects_shows_no_pre_flight_finding() {
    // alpha's program, npx, is not on the test's PATH, yet Claude Code started it: saying it is not there would
    // contradict "Connected" beside it, as in the app. A connected row keeps the table to one line.
    let env = Env::new();
    let out = env.status(&["alpha"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(!stdout.contains("alpha: "), "{stdout}");
    let out = env.status(&["alpha", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["servers"][0]["preflight"], serde_json::json!([]), "{v:#}");
}

#[test]
fn list_with_names_shows_only_those() {
    let env = Env::new();
    let out = env.run(&["list", "gamma", "alpha"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("gamma") && stdout.contains("alpha"),
        "{stdout}"
    );
    for other in ["beta", "delta", "docs"] {
        assert!(!stdout.contains(other), "{other} in {stdout}");
    }
    // Groups with nothing selected are left out.
    assert!(!stdout.contains("Project in"), "{stdout}");
}

#[test]
fn an_unknown_name_is_a_usage_error_that_suggests_a_case_match() {
    let env = Env::new();
    for cmd in ["list", "status"] {
        let out = env.run(&[cmd, "gama"]);
        assert_eq!(out.status.code(), Some(2), "{cmd}");
        let err = text(&out.stderr);
        assert!(err.contains("did you mean \"gamma\""), "{err}");
        let out = env.run(&[cmd, "Gamma"]);
        assert_eq!(out.status.code(), Some(2), "{cmd}");
        let err = text(&out.stderr);
        assert!(err.contains("Gamma") && err.contains("gamma"), "{err}");
        assert!(out.stdout.is_empty());
        let out = env.run(&[cmd, "nothing-like-it"]);
        assert_eq!(out.status.code(), Some(2), "{cmd}");
        assert!(text(&out.stderr).contains("nothing-like-it"));
    }
    assert!(env.gets().is_empty());
}

#[test]
fn status_checks_user_and_local_servers_that_are_on_and_reports_failures() {
    let env = Env::new();
    let out = env.status(&[]);
    let stdout = text(&out.stdout);
    // beta failed and gamma needs auth: not all is well.
    assert_eq!(
        out.status.code(),
        Some(1),
        "{stdout}\n{}",
        text(&out.stderr)
    );
    assert!(line_of(&stdout, "alpha").contains("Connected"), "{stdout}");
    let beta = line_of(&stdout, "beta");
    assert!(
        beta.contains("Failed") && beta.contains("ECONNREFUSED"),
        "{stdout}"
    );
    assert!(line_of(&stdout, "gamma").contains("Needs auth"), "{stdout}");
    // The app's plain-language hint is shown under a failure.
    let under_beta = stdout
        .lines()
        .skip_while(|l| *l != beta)
        .nth(1)
        .unwrap_or("");
    assert!(
        under_beta.starts_with("    ") && under_beta.trim().starts_with("beta: "),
        "no hint under beta:\n{stdout}"
    );
    // Off: shown, not checked.
    assert!(
        line_of(&stdout, "delta").contains("not checked"),
        "{stdout}"
    );
    // A project server runs the repository's command: not checked unless named, and the output says how.
    let docs = line_of(&stdout, "docs");
    assert!(docs.contains("klefki-cli status docs"), "{stdout}");
    assert!(
        docs.contains("node tools/docs-mcp.js"),
        "the command it would run: {stdout}"
    );
    let mut gets = env.gets();
    gets.sort();
    gets.dedup();
    assert_eq!(gets, ["alpha", "beta", "gamma", "remote"]);
    assert!(stdout.contains("2 connected"), "{stdout}");
}

#[test]
fn status_checks_a_named_project_server_and_exits_zero_when_it_connects() {
    let env = Env::new();
    let out = env.status(&["docs"]);
    let stdout = text(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}\n{}",
        text(&out.stderr)
    );
    assert!(line_of(&stdout, "docs").contains("Connected"), "{stdout}");
    assert!(!stdout.contains("alpha"), "{stdout}");
    // What it runs is said before it runs.
    let err = text(&out.stderr);
    assert!(
        err.contains("docs (from ") && err.contains("runs: node tools/docs-mcp.js"),
        "{err}"
    );
    // Only what was asked runs: no background check of the other servers.
    assert_eq!(env.gets(), ["docs"]);
}

#[test]
fn status_json_marks_what_was_checked_and_why_not() {
    let env = Env::new();
    let out = env.status(&["--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["claude_version"], "2.1.302");
    let servers = v["servers"].as_array().unwrap();
    let find = |name: &str| servers.iter().find(|s| s["name"] == name).unwrap();
    assert_eq!(find("alpha")["status"], "connected");
    assert_eq!(find("alpha")["checked"], true);
    assert_eq!(find("beta")["status"], "failed");
    assert!(
        find("beta")["detail"]
            .as_str()
            .unwrap()
            .contains("ECONNREFUSED")
    );
    assert!(find("beta")["hint"]["title"].is_string(), "{v:#}");
    assert_eq!(find("gamma")["status"], "needs_auth");
    assert_eq!(find("alpha")["skipped"], serde_json::Value::Null);
    assert_eq!(find("delta")["checked"], false);
    assert_eq!(find("delta")["skipped"], "off");
    // Only what this run found is a status: `select(.status == "connected")` never counts an unchecked server.
    assert_eq!(find("delta")["status"], serde_json::Value::Null);
    assert_eq!(find("docs")["checked"], false);
    assert_eq!(find("docs")["skipped"], "project_server");
    assert_eq!(find("docs")["status"], serde_json::Value::Null);
    assert_eq!(find("docs")["last_known"], "pending_approval");
}

#[test]
fn status_without_a_usable_claude_cli_says_so_and_fails() {
    let env = Env::new();
    let missing = env.root().join("no-such-claude");
    let out = env.run(&["status", "--claude", &missing.to_string_lossy()]);
    // Not 1: a script must tell "a server is down" from "the tool cannot work".
    assert_eq!(out.status.code(), Some(3));
    let err = text(&out.stderr);
    assert!(err.to_lowercase().contains("claude"), "{err}");
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    // A script shim is refused like in the app.
    let shim = env.root().join("claude.cmd");
    std::fs::write(&shim, "@echo off\n").unwrap();
    let out = env.run(&["status", "--claude", &shim.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(3));
    assert!(env.gets().is_empty());
}

/// Not on Windows: there the search starts in the real home folder (`~/.local/bin/claude.exe`), which a test cannot
/// redirect, so it could run the real Claude CLI (CLAUDE.md: tests never do). The search itself is core's, tested
/// there on every platform.
#[cfg(not(windows))]
#[test]
fn status_finds_the_claude_cli_like_the_app_when_not_named() {
    let env = Env::new();
    let out = env.run(&["status", "alpha"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(line_of(&text(&out.stdout), "alpha").contains("Connected"));
}

#[test]
fn usage_errors_exit_two_and_point_at_help() {
    let env = Env::new();
    for args in [
        &["frobnicate"][..],
        &["list", "--frob"],
        &["list", "--claude", "x"],
        &["status", "--claude"],
        &[],
    ] {
        let out = env.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = text(&out.stderr);
        assert!(
            err.contains("klefki-cli help") || err.contains("Usage"),
            "{args:?}: {err}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    assert!(!env.any_run());
}

#[test]
fn help_and_version_go_to_stdout() {
    let env = Env::new();
    for args in [&["help"][..], &["--help"], &["-h"]] {
        let out = env.run(args);
        assert_eq!(out.status.code(), Some(0));
        let stdout = text(&out.stdout);
        assert!(stdout.contains("list") && stdout.contains("status") && stdout.contains("--json"));
        assert!(
            stdout.contains("Exit"),
            "exit codes are documented: {stdout}"
        );
    }
    for args in [&["--version"][..], &["-V"]] {
        let out = env.run(args);
        assert_eq!(out.status.code(), Some(0));
        assert!(text(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn hostile_names_and_targets_reach_the_terminal_clean() {
    let env = Env::new();
    let cfg = env.root().join("cfg").join(".claude.json");
    std::fs::write(
        &cfg,
        serde_json::to_vec(&serde_json::json!({"mcpServers": {
            "evil\u{1b}[31m\u{202e}name": {"type": "http", "url": "https://x.example.com/\u{1b}]8;;https://bad/\u{7}"},
        }}))
        .unwrap(),
    )
    .unwrap();
    for args in [&["list"][..], &["list", "--json"]] {
        let out = env.run(args);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let stdout = text(&out.stdout);
        assert!(
            !stdout
                .chars()
                .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}'),
            "{stdout:?}"
        );
    }
}

#[test]
fn a_closed_pipe_ends_quietly() {
    let env = Env::new();
    let servers: serde_json::Map<String, serde_json::Value> = (0..2000)
        .map(|i| {
            (
                format!("server-{i}"),
                serde_json::json!({"type": "http", "url": format!("https://s{i}.example.com/mcp")}),
            )
        })
        .collect();
    std::fs::write(
        env.root().join("cfg").join(".claude.json"),
        serde_json::to_vec(&serde_json::json!({ "mcpServers": servers })).unwrap(),
    )
    .unwrap();
    let mut child = env
        .command(&["list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let err = text(&out.stderr);
    assert!(!err.contains("panicked"), "{err}");
    assert_eq!(out.status.code(), Some(0), "{err}");
}

#[test]
fn no_configuration_at_all_is_not_an_error() {
    let env = Env::new();
    std::fs::remove_file(env.root().join("cfg").join(".claude.json")).unwrap();
    let out = env.run(&["list"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("No MCP servers"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn secrets_in_a_stdio_servers_arguments_are_never_printed() {
    // Review of #25: `--api-key sk-live-…` was printed as written, into scrollback, logs and files.
    let env = Env::new();
    let claude = fake_claude().to_string_lossy().into_owned();
    for args in [
        &["list"][..],
        &["list", "--json"],
        &["status", "alpha", "--claude", &claude],
        &["status", "--json", "--claude", &claude],
        &["export"],
    ] {
        let out = env.run(args);
        let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
        for secret in [API_KEY, "tok-SECRET-98765", GHP, "hdr-SECRET-abcdef123"] {
            assert!(!all.contains(secret), "{args:?} printed {secret}:\n{all}");
        }
        assert!(all.contains("alpha"), "{args:?}: {all}");
    }
}

#[test]
fn a_named_server_that_is_off_is_not_checked_and_fails_the_run() {
    // Review of #25: `status delta` checked a server Claude Code does not start, and said "Connected".
    // P9: a script that names it asks whether it works in Claude Code; off, it does not, so the run fails (1).
    let env = Env::new();
    let out = env.status(&["delta"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let delta = line_of(&stdout, "delta");
    assert!(
        delta.contains("Off") && delta.contains("turn it on first"),
        "{stdout}"
    );
    assert!(!stdout.contains("Connected"), "{stdout}");
    // The summary says why the run failed, not only that nothing was checked.
    assert!(stdout.ends_with("\n1 off.\n"), "{stdout}");
    let out = env.status(&["delta", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["servers"][0]["skipped"], "off");
    assert_eq!(v["servers"][0]["checked"], false);
    assert!(env.gets().is_empty(), "{:?}", env.gets());
    // Beside a server that connects, still a failure: one of those named does not work.
    let out = env.status(&["alpha", "delta"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert_eq!(env.gets(), ["alpha"]);
}

#[test]
fn an_unnamed_server_that_is_off_does_not_fail_the_run() {
    // Without names, `status` answers for the servers Claude Code runs: one switched off is not a failure.
    let env = Env::new();
    std::fs::write(
        env.root().join("scenario.json"),
        r#"{"rules":[{"args":["--version"],"stdout_file":"version.txt"},
            {"args":["mcp","get"],"stdout_file":"alpha.txt"}]}"#,
    )
    .unwrap();
    let out = env.status(&[]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(line_of(&stdout, "delta").contains("Off"), "{stdout}");
}

#[test]
fn a_rejected_project_server_is_not_checked_even_when_named() {
    let env = Env::new();
    let settings = env.root().join("Repo").join(".claude");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(
        settings.join("settings.local.json"),
        r#"{"disabledMcpjsonServers": ["docs"]}"#,
    )
    .unwrap();
    let out = env.status(&["docs"]);
    // Claude Code does not run it, so a script asking about it learns it does not work.
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let docs = line_of(&stdout, "docs");
    assert!(
        docs.contains("Rejected") && !docs.contains("klefki-cli status"),
        "{stdout}"
    );
    assert!(
        !text(&out.stderr).contains("runs:"),
        "{}",
        text(&out.stderr)
    );
    assert!(env.gets().is_empty());
}

#[test]
fn when_every_check_breaks_down_the_tool_is_the_problem() {
    // Claude Code ran but said nothing understandable for any server: exit 3, like a missing CLI, not 1.
    let env = Env::new();
    std::fs::write(env.root().join("garbage.txt"), "nothing useful\n").unwrap();
    std::fs::write(
        env.root().join("scenario.json"),
        r#"{"rules":[{"args":["--version"],"stdout_file":"version.txt"},
            {"args":["mcp","get"],"stdout_file":"garbage.txt"}]}"#,
    )
    .unwrap();
    let out = env.status(&[]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(3), "{stdout}");
    let alpha = line_of(&stdout, "alpha");
    assert!(
        alpha.contains("Check failed") && !alpha.contains("Not checked"),
        "{stdout}"
    );
}

#[test]
fn a_mistyped_command_gets_a_suggestion_and_no_command_gets_the_help() {
    let env = Env::new();
    let out = env.run(&["lst"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(err.contains("did you mean \"list\""), "{err}");
    let out = env.run(&[]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(
        err.contains("Examples:") && err.contains("--claude"),
        "{err}"
    );
}

#[test]
fn export_prints_every_server_as_one_block_with_secrets_as_references_and_runs_nothing() {
    let env = Env::new();
    let out = env.run(&["export"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let block: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is only the block");
    let servers = block["mcpServers"]
        .as_object()
        .expect("an mcpServers block");
    let mut names: Vec<&str> = servers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["alpha", "beta", "delta", "docs", "gamma", "remote"]);
    assert_eq!(
        servers["alpha"]["args"],
        serde_json::json!([
            "-y",
            "alpha-mcp",
            "--api-key",
            "${API_KEY}",
            "--token=${TOKEN}",
            "${SECRET}"
        ])
    );
    assert_eq!(
        servers["docs"]["args"],
        serde_json::json!(["tools/docs-mcp.js"])
    );
    let stderr = text(&out.stderr);
    assert!(stderr.contains("Set these variables"), "{stderr}");
    assert!(
        stderr.contains("API_KEY  (alpha: argument --api-key)"),
        "{stderr}"
    );
    assert!(!env.any_run(), "export starts no program");
}

#[test]
fn export_with_names_holds_only_those_and_an_unknown_name_is_a_usage_error() {
    let env = Env::new();
    let out = env.run(&["export", "gamma", "docs"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let block: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut names: Vec<&String> = block["mcpServers"].as_object().unwrap().keys().collect();
    names.sort_unstable();
    assert_eq!(names, ["docs", "gamma"]);
    // Nothing to set: no variable list.
    assert!(
        !text(&out.stderr).contains("Set these"),
        "{}",
        text(&out.stderr)
    );

    let out = env.run(&["export", "alpah"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(
        text(&out.stderr).contains("did you mean \"alpha\""),
        "{}",
        text(&out.stderr)
    );
    let out = env.run(&["export", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("export always prints JSON"),
        "{}",
        text(&out.stderr)
    );
}

/// Is a process with this id still running? A zombie (ended, not yet reaped) is not.
fn pid_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    } else {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        let stat = String::from_utf8_lossy(&out.stdout);
        !stat.trim().is_empty() && !stat.trim().starts_with('Z')
    }
}

#[test]
fn status_ended_early_leaves_no_check_running() {
    // Regression: Ctrl+C during `status` ended klefki-cli but not the checks it had started (they lead a process
    // group of their own, so the terminal's Ctrl+C never reaches them), nor the servers those started.
    // On Windows a check is ended with klefki-cli however it ends; the test ends it outright.
    let ways: &[&str] = if cfg!(windows) {
        &["kill"]
    } else {
        &["INT", "TERM", "HUP"]
    };
    for way in ways {
        let env = Env::new();
        let pid_file = env.root().join("claude.pid");
        std::fs::write(
            env.root().join("scenario.json"),
            serde_json::json!({"rules": [
                {"args": ["--version"], "stdout_file": "version.txt"},
                {"args": ["mcp", "get", "alpha"], "hang": true, "write_pid_to": pid_file},
            ]})
            .to_string(),
        )
        .unwrap();
        let claude = fake_claude().to_string_lossy().into_owned();
        let mut cli = env
            .command(&["status", "--claude", &claude, "alpha"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start klefki-cli");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let claude_pid: u32 = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break pid;
            }
            assert!(std::time::Instant::now() < until, "the check never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        if *way == "kill" {
            cli.kill().unwrap();
        } else {
            let sent = Command::new("kill")
                .args([&format!("-{way}"), &cli.id().to_string()])
                .status()
                .unwrap();
            assert!(sent.success());
        }
        let ended = cli.wait().unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while pid_alive(claude_pid) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let left = pid_alive(claude_pid);
        if left {
            let _ = if cfg!(windows) {
                Command::new("taskkill")
                    .args(["/F", "/PID", &claude_pid.to_string()])
                    .output()
            } else {
                Command::new("kill")
                    .args(["-KILL", &claude_pid.to_string()])
                    .output()
            };
        }
        assert!(
            !left,
            "SIG{way}: the check {claude_pid} still ran after klefki-cli ended ({ended})"
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            // Ended by the signal, as the shell expects of a program that was interrupted.
            assert!(ended.signal().is_some(), "SIG{way}: {ended}");
        }
    }
}

#[test]
fn export_output_writes_the_block_to_a_new_file_and_never_over_one_without_force() {
    let env = Env::new();
    let file = env.root().join("servers.json");
    let path = file.to_string_lossy().into_owned();
    let out = env.run(&["export", "alpha", "--output", &path]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "the block goes to the file only");
    let written = std::fs::read_to_string(&file).unwrap();
    let block: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(block["mcpServers"]["alpha"]["args"][3], "${API_KEY}");
    assert!(!written.contains(API_KEY), "{written}");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains(&format!("Saved alpha to {path}")),
        "{stderr}"
    );
    assert!(
        stderr.contains("API_KEY  (alpha: argument --api-key)"),
        "{stderr}"
    );

    // There now: kept, and the way to replace it is named.
    let out = env.run(&["export", "-o", &path]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("--force"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), written);

    let out = env.run(&["export", &format!("--output={path}"), "--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("Saved 6 servers to"),
        "{}",
        text(&out.stderr)
    );
    let block: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(block["mcpServers"].as_object().unwrap().len(), 6);
}

#[test]
fn export_output_refuses_claude_codes_own_files_and_a_missing_folder() {
    let env = Env::new();
    let claude_json = env.root().join("cfg").join(".claude.json");
    let before = std::fs::read(&claude_json).unwrap();
    let out = env.run(&["export", "-o", &claude_json.to_string_lossy(), "--force"]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        text(&out.stderr).contains("Claude Code's own"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(std::fs::read(&claude_json).unwrap(), before);

    let mcp_json = env.root().join("Repo").join(".mcp.json");
    let before = std::fs::read(&mcp_json).unwrap();
    let out = env.run(&["export", "-o", &mcp_json.to_string_lossy(), "--force"]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        text(&out.stderr).contains(".mcp.json"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(std::fs::read(&mcp_json).unwrap(), before);

    let out = env.run(&[
        "export",
        "-o",
        &env.root().join("nope").join("x.json").to_string_lossy(),
    ]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        text(&out.stderr).contains("does not exist"),
        "{}",
        text(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

// ---- profiles (decision 012)

/// A profile's server: its scope, whether it is the project's, and its name.
type Member<'a> = (klefki_core::api::Scope, bool, &'a str);

impl Env {
    /// Saves the profiles as Klefki keeps them: `(name, servers)`.
    fn profiles(&self, profiles: &[(&str, &[Member])]) {
        let project = self.project();
        let profiles: Vec<serde_json::Value> = profiles
            .iter()
            .map(|(name, members)| {
                let members: Vec<String> = members
                    .iter()
                    .map(|(scope, local, server)| {
                        let at = local.then_some(project.as_str());
                        klefki_core::api::ConnectionId::new(*scope, at, server)
                            .as_str()
                            .to_owned()
                    })
                    .collect();
                serde_json::json!({"name": name, "members": members})
            })
            .collect();
        let dir = self.root().join("data");
        std::fs::create_dir_all(dir.join("backups")).unwrap();
        std::fs::write(
            dir.join("profiles.json"),
            serde_json::json!({"version": 1, "profiles": profiles}).to_string(),
        )
        .unwrap();
    }

    fn claude_json(&self) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(self.root().join("cfg").join(".claude.json")).unwrap(),
        )
        .unwrap()
    }

    fn disabled_in_project(&self) -> Vec<String> {
        let config = self.claude_json();
        let mut off: Vec<String> = config["projects"][self.project()]["disabledMcpServers"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
            .unwrap_or_default();
        off.sort();
        off
    }

    fn work(&self) {
        use klefki_core::api::Scope;
        self.profiles(&[(
            "Work",
            &[
                (Scope::User, false, "alpha"),
                (Scope::Local, true, "gamma"),
                (Scope::User, false, "gone"),
            ],
        )]);
    }
}

#[test]
fn profiles_lists_each_profile_with_its_servers() {
    let env = Env::new();
    let out = env.run(&["profiles"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("No profiles yet"),
        "{}",
        text(&out.stdout)
    );

    env.work();
    let out = env.run(&["profiles"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with("Work  3 servers\n"), "{stdout}");
    assert!(line_of(&stdout, "alpha").contains("User"), "{stdout}");
    assert!(line_of(&stdout, "gamma").contains("Local in"), "{stdout}");
    assert!(
        line_of(&stdout, "gone").contains("no longer configured"),
        "{stdout}"
    );
    assert!(!stdout.contains("Only these servers"), "{stdout}");
    assert!(!env.any_run(), "listing profiles runs nothing");

    let out = env.run(&["profiles", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["schema"], 1);
    let work = &json["profiles"][0];
    assert_eq!(work["name"], "Work");
    assert_eq!(work["servers"][0]["name"], "alpha");
    assert_eq!(work["servers"][0]["scope"], "user");
    assert_eq!(work["servers"][1]["project"], env.project());
    assert_eq!(work["servers"][2]["configured"], false);
    assert_eq!(work["alone_in"], serde_json::json!([]));
}

#[test]
fn use_shows_the_change_and_saves_it_only_when_told_to() {
    let env = Env::new();
    env.work();
    let before = std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap();
    let project = env.project();
    // Nobody at a terminal to ask: the change is shown, and nothing is saved.
    let out = env.run(&["use", "Work", "--project", &project, "--only"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("Use the profile Work in Repo, and turn off every other server there"),
        "{stdout}"
    );
    assert!(
        stdout.contains("This also turns off beta, docs and remote"),
        "{stdout}"
    );
    assert!(
        stdout.contains("gone is in Work but no longer configured"),
        "{stdout}"
    );
    assert!(
        stdout.contains(".claude.json"),
        "the file and its changes: {stdout}"
    );
    assert!(text(&out.stderr).contains("--yes"), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap(),
        before
    );

    let out = env.run(&["use", "Work", "--project", &project, "--only", "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("Saved."),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        env.disabled_in_project(),
        ["beta", "delta", "docs", "remote"]
    );
    let backups = std::fs::read_dir(env.root().join("data").join("backups"))
        .unwrap()
        .count();
    assert!(backups > 0, "a backup is kept before the change");
    // The engine asks Claude Code its version when it starts, as in the app; nothing else is run.
    let log = std::fs::read_to_string(env.root().join("log.jsonl")).unwrap_or_default();
    assert!(
        log.lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .all(|l| l["argv"] == serde_json::json!(["--version"])),
        "using a profile runs no Claude Code command: {log}"
    );

    // Now on alone there, as the files say.
    let out = env.run(&["profiles"]);
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "Only these servers are on in {}.",
            env.shown_project(env.home().as_deref())
        )),
        "{stdout}"
    );
    // Again: nothing to change, nothing written.
    let after = std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap();
    let out = env.run(&["use", "Work", "--project", &project, "--only", "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("Nothing to change"),
        "{}",
        text(&out.stdout)
    );
    // Which servers, and where (the owner's report: "already looks like this" alone did not say).
    assert!(
        text(&out.stdout).contains(" already the only ")
            && !text(&out.stdout).contains("already looks like this"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(
        std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap(),
        after
    );
}

#[test]
fn use_without_a_project_uses_the_current_folders() {
    let env = Env::new();
    // Turned on where it was off: delta is in no profile, so a profile with it turns it on.
    use klefki_core::api::Scope;
    env.profiles(&[("Delta", &[(Scope::User, false, "delta")])]);
    let out = env
        .command(&["use", "Delta", "-y"])
        .current_dir(env.root().join("Repo"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        env.disabled_in_project().is_empty(),
        "{:?}",
        env.disabled_in_project()
    );
    // A folder Claude Code does not know as a project: said, nothing changed.
    let out = env
        .command(&["use", "Delta", "-y"])
        .current_dir(env.root().join("home"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("Claude Code does not know"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn use_names_a_profile_that_is_not_there_and_suggests_one() {
    let env = Env::new();
    env.work();
    let out = env.run(&["use", "wrok", "--all", "-y"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("no profile is named \"wrok\"; did you mean \"Work\"?"),
        "{stderr}"
    );
    // The case aside, a name is the profile's.
    let out = env.run(&["use", "work", "--all", "-y"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn use_never_prints_a_secret_from_the_configuration() {
    let env = Env::new();
    env.work();
    let project = env.project();
    let out = env.run(&["use", "Work", "--project", &project, "--only"]);
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    for secret in [API_KEY, "tok-SECRET-98765", GHP, "hdr-SECRET-abcdef123"] {
        assert!(!all.contains(secret), "{secret} printed:\n{all}");
    }
}

/// Found in review of PR #59: in a folder inside a git repository, Claude Code uses the repository's project, and so
/// does `use`. A folder in a project that is not a git repository is a project of its own, unknown until Claude Code
/// starts there.
#[test]
fn use_in_a_folder_of_a_git_repository_uses_the_repositorys_project() {
    use klefki_core::api::Scope;
    let env = Env::new();
    env.profiles(&[("Delta", &[(Scope::User, false, "delta")])]);
    let sub = env.root().join("Repo").join("src").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    // Not yet a git repository: refused, nothing changed.
    let out = env
        .command(&["use", "Delta", "-y"])
        .current_dir(&sub)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(env.disabled_in_project(), ["delta"]);
    std::fs::create_dir_all(env.root().join("Repo").join(".git")).unwrap();
    let out = env
        .command(&["use", "Delta", "-y"])
        .current_dir(&sub)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("Use the profile Delta in Repo"),
        "{}",
        text(&out.stdout)
    );
    assert!(
        env.disabled_in_project().is_empty(),
        "{:?}",
        env.disabled_in_project()
    );
    // Named with --project, the same.
    let sub_path = sub.to_string_lossy().into_owned();
    let out = env.run(&["use", "Delta", "--project", &sub_path, "-y"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// Found in review of PR #59: the question names what is being decided, and a usage error shows `use`'s own usage.
#[test]
fn use_says_what_it_asks_about_and_its_own_usage() {
    let env = Env::new();
    env.work();
    let out = env.run(&["use", "Work", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("Usage: klefki-cli use PROFILE [--project DIR | --all] [--only] [--yes]"),
        "{stderr}"
    );
    let out = env.run(&["use", "wrok", "--all"]);
    assert!(
        text(&out.stderr).contains("Usage: klefki-cli use PROFILE"),
        "{}",
        text(&out.stderr)
    );
    let out = env.run(&["list", "--frob"]);
    assert!(
        !text(&out.stderr).contains("use PROFILE"),
        "{}",
        text(&out.stderr)
    );
}

// ---- golden output

impl Env {
    /// What `args` prints and how it ends, with this environment's folders as `<project>` and `<tmp>`, so the
    /// output reads the same on every system.
    fn transcript(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let mut text = format!(
            "$ klefki-cli {}\nexit: {:?}\n--- stdout\n{}--- stderr\n{}",
            args.join(" "),
            out.status.code(),
            text(&out.stdout),
            text(&out.stderr)
        )
        .replace("\r\n", "\n");
        let fake = fake_claude().to_string_lossy().into_owned();
        text = text.replace(&fake, "<fake-claude>");
        // The full path first: the table may shorten it under the home folder, JSON never does.
        text = text.replace(&self.project(), "<project>");
        text = text.replace(&self.shown_project(self.home().as_deref()), "<project>");
        let root = self.root().to_string_lossy().into_owned();
        for form in [root.replace('\\', "\\\\"), root.replace('\\', "/"), root] {
            text = text.replace(&form, "<tmp>");
        }
        stable_paths(&text)
    }
}

/// `text` with paths under `<tmp>` spelled with `/`, and a backup's time stamp (`claude.json.<digits>-<n>.bak`) as
/// `<stamp>`.
fn stable_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<tmp>") {
        out.push_str(&rest[..at]);
        let end = rest[at..]
            .find(|c: char| c.is_whitespace() || c == '"')
            .map_or(rest.len(), |e| at + e);
        let path = rest[at..end].replace('\\', "/");
        match (path.find("claude.json."), path.rfind(".bak")) {
            (Some(start), Some(stop)) if start < stop => {
                out.push_str(&path[..start + "claude.json.".len()]);
                out.push_str("<stamp>");
                out.push_str(&path[stop..]);
            }
            _ => out.push_str(&path),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Every command's full output on the configuration with every kind of row, byte for byte, so the code behind it can
/// be simplified without changing what users and their scripts read. When a change is meant, run the test with
/// `UPDATE_EXPECT=1` and review `golden/*.txt` in the diff.
#[test]
fn golden_output_of_every_command() {
    let env = Env::new();
    env.work();
    let project = env.project();
    let fake = fake_claude().to_string_lossy().into_owned();
    let runs: [(&str, Vec<&str>); 11] = [
        ("list.txt", vec!["list"]),
        ("list-json.txt", vec!["list", "--json"]),
        ("status.txt", vec!["status", "--claude", &fake]),
        (
            "status-json.txt",
            vec!["status", "--claude", &fake, "--json"],
        ),
        ("export.txt", vec!["export"]),
        ("profiles.txt", vec!["profiles"]),
        (
            "use-nothing-to-do.txt",
            vec!["use", "Work", "--project", &project],
        ),
        (
            "use-only-unconfirmed.txt",
            vec!["use", "Work", "--project", &project, "--only"],
        ),
        ("help.txt", vec!["--help"]),
        ("usage-error.txt", vec!["lst"]),
        ("unknown-name.txt", vec!["list", "ALPHA"]),
    ];
    for (file, args) in runs {
        let expected = expect_test::expect_file![format!("golden/{file}")];
        expected.assert_eq(&env.transcript(&args));
    }
    // Last, as it saves: the change, and the configuration it leaves.
    let saved = env.transcript(&["use", "Work", "--project", &project, "--only", "--yes"]);
    let after = std::fs::read_to_string(env.root().join("cfg").join(".claude.json")).unwrap();
    let after = after.replace(&project, "<project>");
    expect_test::expect_file!["golden/use-only-saved.txt"]
        .assert_eq(&format!("{saved}--- .claude.json after\n{after}\n"));
}

// ---- found by cargo-mutants (docs/quality.md): what no test checked before

/// `program args` at a terminal: a pseudo-terminal is its stdin, stdout and stderr, so all it prints is one stream.
#[cfg(unix)]
struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    shown: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: std::thread::JoinHandle<()>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

#[cfg(unix)]
impl Terminal {
    /// Starts `program args` in `env`'s environment, from its folder.
    fn start(env: &Env, program: &Path, args: &[&str]) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::Read;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 50,
                cols: 400,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(env.root());
        for (name, value) in env.vars() {
            cmd.env(name, value);
        }
        cmd.env("TERM", "dumb");
        let child = pair.slave.spawn_command(cmd).unwrap();
        // Only the child holds the other end now, so the output ends when it does.
        drop(pair.slave);
        let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut from = pair.master.try_clone_reader().unwrap();
        let into = std::sync::Arc::clone(&shown);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = from.read(&mut buf) {
                into.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let writer = pair.master.take_writer().unwrap();
        Self {
            child,
            writer,
            shown,
            reader,
            _master: pair.master,
        }
    }

    /// Everything shown so far, with the terminal's line ends as `\n`.
    fn shown(&self) -> String {
        String::from_utf8_lossy(&self.shown.lock().unwrap()).replace("\r\n", "\n")
    }

    fn type_in(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// Waits until `text` is shown.
    fn wait_for(&self, text: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !self.shown().contains(text) {
            assert!(
                std::time::Instant::now() < deadline,
                "{text:?} was never shown:\n{}",
                self.shown()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Waits for the program to end: its exit code and everything it showed, line ends and all.
    fn finish(mut self) -> (u32, String) {
        let code = self.child.wait().unwrap().exit_code();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !self.reader.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let shown = String::from_utf8_lossy(&self.shown.lock().unwrap()).into_owned();
        (code, shown)
    }
}

#[cfg(unix)]
fn klefki_cli() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_klefki-cli"))
}

/// At a terminal, `export --output` asks before it replaces a file, and replaces it only on a yes.
#[cfg(unix)]
#[test]
fn export_at_a_terminal_asks_before_replacing_a_file() {
    let env = Env::new();
    let file = env.root().join("servers.json");
    let path = file.to_string_lossy().into_owned();
    for (answer, code, kept) in [("n", 4, true), ("", 4, true), ("Yes", 0, false)] {
        std::fs::write(&file, "old").unwrap();
        let mut term = Terminal::start(&env, klefki_cli(), &["export", "alpha", "-o", &path]);
        term.wait_for(&format!("{path} is there already. Replace it? [y/N] "));
        term.type_in(&format!("{answer}\r"));
        let (got, shown) = term.finish();
        assert_eq!(got, code, "{answer:?}: {shown}");
        let now = std::fs::read_to_string(&file).unwrap();
        assert_eq!(now == "old", kept, "{answer:?}: {now}\n{shown}");
        if kept {
            assert!(shown.contains("add --force to replace it"), "{shown}");
        } else {
            assert!(shown.contains(&format!("Saved alpha to {path}")), "{shown}");
        }
    }
}

/// Asked only when someone can both read the question and answer it: with stderr, where the question goes, not a
/// terminal, an answer typed ahead is not taken for a yes.
#[cfg(unix)]
#[test]
fn export_does_not_ask_when_its_question_would_not_be_seen() {
    let env = Env::new();
    let file = env.root().join("servers.json");
    std::fs::write(&file, "old").unwrap();
    let path = file.to_string_lossy().into_owned();
    // The shell sends stderr to a file and starts klefki-cli in its place.
    let script = "exec \"$0\" \"$@\" 2>err.txt";
    let cli = klefki_cli().to_string_lossy().into_owned();
    let mut term = Terminal::start(
        &env,
        Path::new("/bin/sh"),
        &["-c", script, &cli, "export", "alpha", "-o", &path],
    );
    term.type_in("y\r");
    let (code, shown) = term.finish();
    let err = std::fs::read_to_string(env.root().join("err.txt")).unwrap();
    assert_eq!(code, 4, "{shown}\n{err}");
    assert!(!err.contains("Replace it?"), "{err}");
    assert!(err.contains("add --force to replace it"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
}

/// At a terminal, `status` shows how far the checks are on one line, and clears it before the table.
#[cfg(unix)]
#[test]
fn status_at_a_terminal_shows_its_progress_then_clears_it() {
    let env = Env::new();
    let fake = fake_claude().to_string_lossy().into_owned();
    let term = Terminal::start(
        &env,
        klefki_cli(),
        &["status", "--claude", &fake, "alpha", "beta"],
    );
    let (code, shown) = term.finish();
    assert_eq!(code, 1, "{shown:?}");
    assert!(
        shown.contains("\rChecking 2 servers, waiting for "),
        "{shown:?}"
    );
    assert!(shown.contains("Checked 2 of 2."), "{shown:?}");
    // The clearing: spaces over the whole line, between two returns, before the table.
    let cleared = shown
        .find("Checked 2 of 2.")
        .and_then(|at| shown[at..].find("\r ").map(|r| at + r))
        .unwrap_or_else(|| panic!("never cleared: {shown:?}"));
    // Each line covers the longer one before it: no leftover characters while it runs.
    let lines: Vec<usize> = shown[..cleared]
        .split('\r')
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().count())
        .collect();
    assert!(lines.len() >= 3, "{shown:?}");
    assert!(
        lines.windows(2).all(|w| w[0] <= w[1]),
        "{lines:?}: {shown:?}"
    );
    let rest = &shown[cleared + 1..];
    let spaces = rest.len() - rest.trim_start_matches(' ').len();
    assert!(spaces >= "Checked 2 of 2.".len(), "{shown:?}");
    assert!(rest[spaces..].starts_with('\r'), "{shown:?}");
    assert!(
        line_of(&rest.replace("\r\n", "\n"), "alpha").contains("Connected"),
        "{shown:?}"
    );

    // One server: "server", not "servers".
    let term = Terminal::start(&env, klefki_cli(), &["status", "--claude", &fake, "alpha"]);
    let (code, shown) = term.finish();
    assert_eq!(code, 0, "{shown:?}");
    assert!(
        shown.contains("\rChecking 1 server, waiting for alpha…"),
        "{shown:?}"
    );

    // Nothing to check: no progress, and nothing to clear. Named and off, delta fails the run (P9).
    let term = Terminal::start(&env, klefki_cli(), &["status", "--claude", &fake, "delta"]);
    let (code, shown) = term.finish();
    assert_eq!(code, 1, "{shown:?}");
    assert!(!shown.contains("Check"), "{shown:?}");
    assert!(
        !shown.contains("\r\r") && !shown.contains("\r "),
        "{shown:?}"
    );
}

/// At a terminal, `use` asks with the summary; a configuration changed while it waits is not overwritten, and says
/// so at once.
#[cfg(unix)]
#[test]
fn use_at_a_terminal_saves_on_yes_and_never_over_a_changed_file() {
    let env = Env::new();
    env.work();
    let project = env.project();
    let claude_json = env.root().join("cfg").join(".claude.json");
    let args = ["use", "Work", "--project", &project, "--only"];

    let mut term = Terminal::start(&env, klefki_cli(), &args);
    term.wait_for("[y/N] ");
    // Changed outside Klefki while the question is open, in what the change would do.
    let mut changed = env.claude_json();
    changed["projects"][&project]["disabledMcpServers"] = serde_json::json!(["delta", "beta"]);
    let changed = serde_json::to_vec_pretty(&changed).unwrap();
    std::fs::write(&claude_json, &changed).unwrap();
    let asked = std::time::Instant::now();
    term.type_in("y\r");
    let (code, shown) = term.finish();
    assert_eq!(code, 1, "{shown}");
    assert!(shown.contains("klefki-cli: nothing was saved: "), "{shown}");
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(30),
        "the failure is told when it happens, not after a deadline: {shown}"
    );
    assert_eq!(std::fs::read(&claude_json).unwrap(), changed);

    let mut term = Terminal::start(&env, klefki_cli(), &args);
    term.wait_for("and turn off every other server there? [y/N] ");
    term.type_in("y\r");
    let (code, shown) = term.finish();
    assert_eq!(code, 0, "{shown}");
    assert!(
        shown.contains("Saved. The files as they were are kept in "),
        "{shown}"
    );
    assert_eq!(
        env.disabled_in_project(),
        ["beta", "delta", "docs", "remote"]
    );
}

/// Everything means everything Klefki can export: the organisation's servers come with the machine and are left
/// out unnamed. Named, one is tried, left out with the reason, and the exit code says so.
#[test]
fn export_leaves_the_organisations_servers_out_unless_named() {
    let env = Env::new();
    let org = env.root().join("org");
    std::fs::create_dir_all(&org).unwrap();
    std::fs::write(
        org.join("managed-settings.json"),
        r#"{"managedMcpServers": {"wiki": {"type": "sse", "url": "https://wiki.example.com/sse"}}}"#,
    )
    .unwrap();
    let out = env.run(&["export"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let block: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(block["mcpServers"].get("wiki").is_none(), "{block:#}");
    assert!(block["mcpServers"].get("alpha").is_some(), "{block:#}");
    assert!(!text(&out.stderr).contains("wiki"), "{}", text(&out.stderr));

    let file = env.root().join("wiki.json");
    let out = env.run(&["export", "wiki", "-o", &file.to_string_lossy()]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("note: Left out wiki: "), "{stderr}");
    // Saved, though it holds none: not "Saved wiki".
    assert!(stderr.contains("Saved 0 servers to"), "{stderr}");
    let out = env.run(&["export", "wiki", "alpha"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
}

/// What is wrong with the configuration is said on stderr, before the output.
#[test]
fn warnings_about_the_configuration_go_to_stderr() {
    let env = Env::new();
    std::fs::write(
        env.root().join("Repo").join(".mcp.json"),
        r#"{"mcpServers": {"docs": {"command": "a"}, "docs": {"command": "b"}}}"#,
    )
    .unwrap();
    for args in [&["list"][..], &["export"], &["profiles"]] {
        let out = env.run(args);
        let stderr = text(&out.stderr);
        assert!(
            stderr.contains("warning: ") && stderr.contains("defines \"docs\" more than once"),
            "{args:?}: {stderr}"
        );
        assert!(!text(&out.stdout).contains("warning"), "{args:?}");
    }
}

/// Only a reader that went away is quiet; any other failure to write the output is an error.
#[cfg(target_os = "linux")]
#[test]
fn output_that_cannot_be_written_is_an_error() {
    let env = Env::new();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let out = env.command(&["list"]).stdout(full).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("klefki-cli: could not write the output: "),
        "{}",
        text(&out.stderr)
    );
}

/// A name close to none: with no profiles yet, where to make one; otherwise, where to see them.
#[test]
fn use_with_an_unknown_profile_says_where_profiles_are() {
    let env = Env::new();
    let out = env.run(&["use", "Zzzzzzzzzz", "--all", "-y"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("there are none yet; make one on Klefki's Profiles page"),
        "{stderr}"
    );
    env.work();
    let out = env.run(&["use", "Zzzzzzzzzz", "--all", "-y"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("no profile is named \"Zzzzzzzzzz\"; `klefki-cli profiles` lists them"),
        "{stderr}"
    );
}

/// A project Claude Code knows by a link is found from the folder the link leads to.
#[cfg(unix)]
#[test]
fn use_finds_a_project_known_by_a_link_from_its_real_folder() {
    use klefki_core::api::Scope;
    let env = Env::new();
    let link = env.root().join("Linked");
    std::os::unix::fs::symlink(env.root().join("Repo"), &link).unwrap();
    let link = link.to_string_lossy().into_owned();
    // The configuration names the project by the link.
    let mut config = env.claude_json();
    let entry = config["projects"][env.project()].take();
    config["projects"] = serde_json::json!({ link.clone(): entry });
    std::fs::write(
        env.root().join("cfg").join(".claude.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
    env.profiles(&[("Delta", &[(Scope::User, false, "delta")])]);
    let out = env.run(&["use", "Delta", "--project", &env.project(), "-y"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let config = env.claude_json();
    assert_eq!(
        config["projects"][&link]["disabledMcpServers"],
        serde_json::json!([]),
        "{config:#}"
    );
}

/// Makes `worktree` a git worktree of the repository in `main`, as `git worktree add` lays it out: a `.git` file that
/// names the worktree's folder in the main `.git`, whose `commondir` leads back to it. `relative`: the paths as
/// `worktree.useRelativePaths` writes them.
fn worktree_of(main: &Path, worktree: &Path, relative: bool) {
    let admin = main.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::write(admin.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", worktree.join(".git").display()),
    )
    .unwrap();
    let gitdir = if relative {
        "../Repo/.git/worktrees/wt".to_owned()
    } else {
        admin.to_string_lossy().into_owned()
    };
    std::fs::write(worktree.join(".git"), format!("gitdir: {gitdir}\n")).unwrap();
}

/// Found in the on/off audit (finding 7): Claude Code uses the main checkout's project in a git worktree, so `use`
/// there uses that project, from the worktree's root, a folder inside it, or named with --project. It said "start
/// Claude Code there once", which could never help.
#[test]
fn use_in_a_git_worktree_uses_the_main_checkouts_project() {
    use klefki_core::api::Scope;
    for relative in [false, true] {
        let env = Env::new();
        env.profiles(&[("Delta", &[(Scope::User, false, "delta")])]);
        let main = env.root().join("Repo");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        let wt = env.root().join("Repo-wt");
        worktree_of(&main, &wt, relative);
        for folder in [wt.clone(), wt.join("src")] {
            let out = env
                .command(&["use", "Delta", "-y"])
                .current_dir(&folder)
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(0),
                "{} (relative: {relative}): {}",
                folder.display(),
                text(&out.stderr)
            );
            assert!(
                text(&out.stdout).contains("Use the profile Delta in Repo"),
                "{}",
                text(&out.stdout)
            );
            assert!(
                env.disabled_in_project().is_empty(),
                "{:?}",
                env.disabled_in_project()
            );
        }
        let out = env.run(&["use", "Delta", "--project", &wt.to_string_lossy(), "-y"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }
}

/// Nearby: a `.git` file that leads nowhere Klefki can follow (a submodule's, or a broken one) is not taken for a
/// worktree: the folder is a project of its own, unknown until Claude Code starts there, as before.
#[test]
fn a_git_file_that_is_no_worktree_is_not_followed() {
    use klefki_core::api::Scope;
    let env = Env::new();
    env.profiles(&[("Delta", &[(Scope::User, false, "delta")])]);
    let main = env.root().join("Repo");
    // A submodule: its git folder sits in the main repository's, with no commondir.
    let modules = main.join(".git").join("modules").join("sub");
    std::fs::create_dir_all(&modules).unwrap();
    let sub = main.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
    let out = env
        .command(&["use", "Delta", "-y"])
        .current_dir(&sub)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(env.disabled_in_project(), ["delta"]);
    // Broken: no folder where it points.
    let broken = env.root().join("Broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
    let out = env
        .command(&["use", "Delta", "-y"])
        .current_dir(&broken)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(env.disabled_in_project(), ["delta"]);
}

/// On/off plan S4: `switch` stops the profile in use in a project and uses another there, as one change, and `stop`
/// puts back what a profile switched: back to before both. `profiles` says where each is in use.
#[test]
fn switch_and_stop_move_a_project_between_profiles_and_back() {
    use klefki_core::api::Scope;
    let env = Env::new();
    env.profiles(&[
        (
            "Work",
            &[(Scope::User, false, "alpha"), (Scope::Local, true, "gamma")],
        ),
        ("Docs", &[(Scope::User, false, "beta")]),
    ]);
    let project = env.project();
    let shown = env.shown_project(env.home().as_deref());
    let out = env.run(&["use", "Work", "--project", &project, "--only", "-y"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        env.disabled_in_project(),
        ["beta", "delta", "docs", "remote"]
    );
    let out = env.run(&["profiles"]);
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains(&format!("  In use in {shown}.")),
        "{stdout}"
    );
    let out = env.run(&["profiles", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["profiles"][0]["in_use"], serde_json::json!([project]));
    assert_eq!(json["profiles"][1]["in_use"], serde_json::json!([]));

    // Nobody to ask: shown, not saved.
    let before = std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap();
    expect_test::expect_file!["golden/switch-unconfirmed.txt"].assert_eq(&env.transcript(&[
        "switch",
        "Docs",
        "--project",
        &project,
        "--only",
    ]));
    assert_eq!(
        std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap(),
        before
    );
    // From the project's folder, saved: Work's switches go back, then only Docs' servers are on. delta was off before
    // Work, so it stays off.
    let out = env
        .command(&["switch", "docs", "--only", "--yes"])
        .current_dir(env.root().join("Repo"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        env.disabled_in_project(),
        ["alpha", "delta", "docs", "gamma", "remote"]
    );
    let stdout = text(&env.run(&["profiles"]).stdout);
    assert_eq!(stdout.matches("In use in").count(), 1, "{stdout}");
    assert!(
        stdout
            .split("\n\n")
            .any(|p| p.starts_with("Docs") && p.contains("In use in")),
        "{stdout}"
    );

    // Stopped: before both.
    let out = env.run(&["stop", "Docs", "--project", &project, "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("Stop using the profile Docs in Repo"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(env.disabled_in_project(), ["delta"]);
    // Again: nothing recorded, said, nothing changed.
    let out = env.run(&["stop", "Docs", "--all", "--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("no record of Docs switching anything"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(env.disabled_in_project(), ["delta"]);

    // With no profile in use, a switch is a use.
    let out = env.run(&["switch", "Docs", "--project", &project, "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("Use the profile Docs in Repo"),
        "{}",
        text(&out.stdout)
    );

    // Their own usage lines.
    let out = env.run(&["switch", "Docs", "--all"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr)
            .contains("Usage: klefki-cli switch PROFILE [--project DIR] [--only] [--yes]"),
        "{}",
        text(&out.stderr)
    );
    let out = env.run(&["stop", "Dcos"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("did you mean \"Docs\"?"), "{stderr}");
    assert!(
        stderr.contains("Usage: klefki-cli stop PROFILE [--project DIR | --all] [--yes]"),
        "{stderr}"
    );
}

/// The arguments and folder of each Claude Code the fake CLI was started as by `run` (no `mcp` command).
fn sessions(env: &Env) -> Vec<(Vec<String>, String)> {
    std::fs::read_to_string(env.root().join("log.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|l| l["event"] == "start" && l["argv"][0] == "--settings")
        .map(|l| {
            let argv = l["argv"].as_array().unwrap();
            (
                argv.iter()
                    .map(|a| a.as_str().unwrap().to_owned())
                    .collect(),
                l["cwd"].as_str().unwrap_or_default().replace('\\', "/"),
            )
        })
        .collect()
}

/// On/off plan S5: `run` starts Claude Code in the project with only the profile's servers, through `--settings`,
/// and saves nothing. The fake CLI stands in for Claude Code.
#[test]
fn run_starts_claude_code_in_the_project_with_only_the_profiles_servers() {
    use klefki_core::api::Scope;
    let env = Env::new();
    env.profiles(&[(
        "Work",
        &[(Scope::User, false, "alpha"), (Scope::Local, true, "gamma")],
    )]);
    let project = env.project();
    let before = std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap();
    let settings = r#"{"allowedMcpServers":[{"serverName":"alpha"},{"serverName":"gamma"}]}"#;

    let out = env.run(&["run", "work", "--project", &project, "--print"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let line = text(&out.stdout);
    if cfg!(windows) {
        assert!(line.starts_with("claude --% --settings \""), "{line}");
    } else {
        assert!(
            line.ends_with(&format!("claude --settings '{settings}'\n")),
            "{line}"
        );
        assert!(line.starts_with("cd "), "{line}");
    }
    let said = text(&out.stderr);
    assert!(
        said.contains("Claude Code started this way loads only Work's servers: alpha and gamma."),
        "{said}"
    );
    assert!(!env.any_run(), "--print starts nothing");

    // From the project's folder, started at once: nothing differs from the profile.
    let out = env
        .command(&["run", "Work", "--claude", &fake_claude().to_string_lossy()])
        .current_dir(env.root().join("Repo"))
        .output()
        .unwrap();
    assert!(
        text(&out.stderr).contains("loads only Work's servers"),
        "{}",
        text(&out.stderr)
    );
    let started = sessions(&env);
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].0, ["--settings", settings]);
    let real = |p: &str| std::fs::canonicalize(p).unwrap();
    assert_eq!(real(&started[0].1), real(&project));
    assert_eq!(
        std::fs::read(env.root().join("cfg").join(".claude.json")).unwrap(),
        before,
        "nothing is saved"
    );
}

/// What would differ from the profile is said, and nothing starts unless confirmed.
#[test]
fn run_says_what_differs_from_the_profile_and_starts_only_when_confirmed() {
    use klefki_core::api::Scope;
    let env = Env::new();
    // delta is off in the project; a rule in the project's local settings lets remote in too.
    env.profiles(&[(
        "Work",
        &[(Scope::User, false, "alpha"), (Scope::User, false, "delta")],
    )]);
    let claude_dir = env.root().join("Repo").join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.local.json"),
        r#"{"allowedMcpServers":[{"serverName":"remote"}]}"#,
    )
    .unwrap();
    let project = env.project();
    let out = env.run(&["run", "Work", "--project", &project]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(said.contains("loads from Work: alpha."), "{said}");
    assert!(
        said.contains("Not loaded, though in Work:\n  - delta is off in Repo"),
        "{said}"
    );
    assert!(
        said.contains(
            "Loaded too, though not in Work:\n  - remote: allowedMcpServers in .claude/settings.local.json lets it load too."
        ),
        "{said}"
    );
    assert!(said.contains("Nothing started: add --yes"), "{said}");
    assert!(sessions(&env).is_empty());
    let claude = fake_claude().to_string_lossy().into_owned();
    env.run(&[
        "run",
        "Work",
        "--project",
        &project,
        "--yes",
        "--claude",
        &claude,
    ]);
    assert_eq!(sessions(&env).len(), 1);
}

#[test]
fn run_takes_one_profile_in_one_project() {
    let env = Env::new();
    env.work();
    for (args, said) in [
        (&["run"][..], "run needs the name of a profile"),
        (
            &["run", "Work", "--all"][..],
            "run starts one session in one project",
        ),
        (&["run", "Work", "--only"][..], "leave out --only"),
        (&["list", "--print"][..], "--print is only for run"),
        (&["run", "Wrok"][..], "did you mean \"Work\"?"),
    ] {
        let out = env.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = text(&out.stderr);
        assert!(stderr.contains(said), "{args:?}: {stderr}");
    }
    assert!(!env.any_run());
}

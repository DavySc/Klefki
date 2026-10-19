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

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_klefki-cli"));
        cmd.args(args)
            .env("CLAUDE_CONFIG_DIR", self.root().join("cfg"))
            // Redirects the home folder on Linux and macOS. Windows finds it through the registry; a changed
            // `USERPROFILE` only breaks that lookup (CI: every user server "not checked: no home folder").
            .env("HOME", self.root().join("home"))
            .env("PATH", self.root().join("home").join(".local").join("bin"))
            .env("FAKE_CLAUDE_SCENARIO", self.root().join("scenario.json"))
            .env("FAKE_CLAUDE_LOG", self.root().join("log.jsonl"))
            .stdin(Stdio::null());
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
fn a_named_server_that_is_off_is_not_checked() {
    // Review of #25: `status delta` checked a server Claude Code does not start, and said "Connected".
    let env = Env::new();
    let out = env.status(&["delta"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let delta = line_of(&stdout, "delta");
    assert!(
        delta.contains("Off") && delta.contains("turn it on first"),
        "{stdout}"
    );
    assert!(!stdout.contains("Connected"), "{stdout}");
    let out = env.status(&["delta", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["servers"][0]["skipped"], "off");
    assert_eq!(v["servers"][0]["checked"], false);
    assert!(env.gets().is_empty(), "{:?}", env.gets());
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
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
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

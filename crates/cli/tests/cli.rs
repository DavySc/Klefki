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
        let project = root.join("repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        let key = project.to_string_lossy().replace('\\', "/");
        let claude_json = serde_json::json!({
            "mcpServers": {
                "alpha": {"type": "stdio", "command": "npx", "args": ["-y", "alpha-mcp"]},
                "beta": {"type": "http", "url": "http://127.0.0.1:9/mcp"},
                "delta": {"type": "sse", "url": "https://delta.example.com/sse"},
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
            .join("repo")
            .to_string_lossy()
            .replace('\\', "/")
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
    let user = stdout.find("User").expect("a User group");
    let local = stdout
        .find(&format!("Local in {}", env.project()))
        .expect("a Local group naming the project");
    let project = stdout
        .find(&format!("Project in {}", env.project()))
        .expect("a Project group naming the project");
    assert!(user < local && local < project, "{stdout}");
    let alpha = line_of(&stdout, "alpha");
    assert!(alpha.contains(" on "), "{alpha}");
    assert!(
        alpha.contains("stdio") && alpha.contains("npx -y alpha-mcp"),
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

#[test]
fn list_json_has_one_object_per_server_with_stable_fields() {
    let env = Env::new();
    let out = env.run(&["list", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let servers = v["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 5, "{v:#}");
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
        under_beta.starts_with("    ") && !under_beta.trim().is_empty(),
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
    let mut gets = env.gets();
    gets.sort();
    gets.dedup();
    assert_eq!(gets, ["alpha", "beta", "gamma"]);
    assert!(stdout.contains("1 connected"), "{stdout}");
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
    // Only what was asked runs: no background check of the other servers.
    assert_eq!(env.gets(), ["docs"]);
}

#[test]
fn status_json_marks_what_was_checked_and_why_not() {
    let env = Env::new();
    let out = env.status(&["--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["cli_version"], "2.1.302");
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
    assert_eq!(find("delta")["checked"], false);
    assert_eq!(find("delta")["skipped"], "off");
    assert_eq!(find("docs")["checked"], false);
    assert_eq!(find("docs")["skipped"], "project_server");
    assert_eq!(find("docs")["status"], "pending_approval");
}

#[test]
fn status_without_a_usable_claude_cli_says_so_and_fails() {
    let env = Env::new();
    let missing = env.root().join("no-such-claude");
    let out = env.run(&["status", "--claude", &missing.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.to_lowercase().contains("claude"), "{err}");
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    // A script shim is refused like in the app.
    let shim = env.root().join("claude.cmd");
    std::fs::write(&shim, "@echo off\n").unwrap();
    let out = env.run(&["status", "--claude", &shim.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(1));
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

use super::*;
use serde_json::json;
use std::path::PathBuf;

/// A folder with programs called `names` in it.
fn folder_with(dir: &Path, names: &[&str]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    for name in names {
        let p = dir.join(name);
        std::fs::write(&p, b"stub").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    dir.to_path_buf()
}

/// Variables `GITHUB_TOKEN` and `HOME` are set, nothing else.
fn vars(name: &str) -> Option<OsString> {
    matches!(name, "GITHUB_TOKEN" | "HOME").then(|| OsString::from("x"))
}

struct Machine {
    _dir: tempfile::TempDir,
    root: PathBuf,
    path: OsString,
}

/// A PATH with `node` and `npx` on it (`node.exe` and `npx.CMD` for Windows' lookups, in the case `PATHEXT` spells), and a `uvx` that is not a
/// program on Unix.
fn machine() -> Machine {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let bin = folder_with(
        &root.join("bin"),
        &["node", "npx", "node.exe", "node.EXE", "npx.CMD"],
    );
    std::fs::write(bin.join("uvx"), b"not a program").unwrap();
    let path = std::env::join_paths([bin]).unwrap();
    Machine {
        _dir: dir,
        root,
        path,
    }
}

fn found(m: &Machine, entry: Value) -> Vec<Preflight> {
    found_as(m, entry, true, false)
}

fn found_as(m: &Machine, entry: Value, trusted: bool, windows: bool) -> Vec<Preflight> {
    let probe = Probe::new(Some(&m.path), &vars, windows);
    findings(&entry, trusted, &probe)
}

fn kinds(list: &[Preflight]) -> Vec<PreflightKind> {
    list.iter().map(|f| f.kind).collect()
}

#[test]
fn a_sound_definition_has_nothing_to_say() {
    let m = machine();
    for entry in [
        json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"]}),
        json!({"type": "stdio", "command": "node", "args": ["${HOME}/srv/index.js"], "env": {"TOKEN": "${GITHUB_TOKEN}"}}),
        json!({"command": "node", "args": ["${MISSING:-./index.js}"]}),
        json!({"type": "http", "url": "https://x.example.com/mcp", "headers": {"Authorization": "Bearer ${GITHUB_TOKEN}"}}),
    ] {
        assert!(
            found(&m, entry.clone()).is_empty(),
            "{entry}: {:?}",
            found(&m, entry.clone())
        );
    }
}

#[test]
fn a_program_that_is_not_on_the_path_is_named() {
    let m = machine();
    let f = found(
        &m,
        json!({"command": "mcp-server-nothere", "args": ["--stdio"]}),
    );
    assert_eq!(kinds(&f), [PreflightKind::NotFound]);
    assert!(f[0].problem.contains("mcp-server-nothere"), "{f:?}");
    assert!(
        f[0].problem.contains("PATH Klefki was started with"),
        "{f:?}"
    );
    // A file of that name that is not a program is not one either (Unix: no executable bit).
    #[cfg(unix)]
    assert_eq!(
        kinds(&found(&m, json!({"command": "uvx", "args": ["x"]}))),
        [PreflightKind::NotFound]
    );
}

#[test]
fn windows_tries_each_ending_of_pathext() {
    let m = machine();
    // `npx` is `npx.cmd`, `node` is `node.exe`: Claude Code on Windows runs both from a plain name.
    for command in ["npx", "node", "node.exe", "NPX"] {
        let f = found_as(&m, json!({"command": command}), true, true);
        // Windows' file names ignore case, Linux's and the test folder's may not.
        if command == "NPX" && !cfg!(windows) {
            continue;
        }
        assert!(f.is_empty(), "{command}: {f:?}");
    }
    assert_eq!(
        kinds(&found_as(&m, json!({"command": "uvx"}), true, true)),
        [PreflightKind::NotFound],
        "uvx has no ending PATHEXT names"
    );
}

#[test]
fn the_path_a_server_sets_is_where_its_program_is_looked_for() {
    let m = machine();
    let own = folder_with(&m.root.join("own"), &["mytool"]);
    let own_path = own.to_string_lossy().into_owned();
    // Seen with Claude Code 2.1.308: a program only on the server's own PATH starts.
    assert!(found(&m, json!({"command": "mytool", "env": {"PATH": own_path}})).is_empty());
    let f = found(&m, json!({"command": "node", "env": {"PATH": own_path}}));
    assert_eq!(kinds(&f), [PreflightKind::NotFound]);
    assert!(f[0].problem.contains("PATH this server sets"), "{f:?}");
    // Filled in from the environment: not known here, so not judged.
    assert!(
        found(
            &m,
            json!({"command": "mytool", "env": {"PATH": "${HOME}/bin"}})
        )
        .is_empty()
    );
}

#[test]
fn a_full_path_is_looked_at_and_one_relative_to_the_session_is_left_to_its_own_hint() {
    let m = machine();
    let real = m.root.join("bin").join("node");
    assert!(found(&m, json!({"command": real.to_string_lossy()})).is_empty());
    let gone = m.root.join("gone").join("server");
    let f = found(&m, json!({"command": gone.to_string_lossy()}));
    assert_eq!(kinds(&f), [PreflightKind::NotFound], "{f:?}");
    assert!(f[0].problem.contains("doesn't exist"), "{f:?}");
    // Windows runs a full path without its ending too.
    let npx = m.root.join("bin").join("npx");
    assert!(found_as(&m, json!({"command": npx.to_string_lossy()}), true, true).is_empty());
    let gone_too = found_as(&m, json!({"command": gone.to_string_lossy()}), true, true);
    assert_eq!(kinds(&gone_too), [PreflightKind::NotFound]);
    for command in [
        "./bin/server",
        "build\\server.exe",
        "~/bin/server",
        "${HOME}/bin/x",
    ] {
        assert!(
            found(&m, json!({"command": command})).is_empty(),
            "{command}"
        );
    }
}

#[test]
fn a_repository_definition_never_makes_klefki_look_where_it_says() {
    let m = machine();
    let gone = m.root.join("gone").join("server");
    let own = folder_with(&m.root.join("own"), &["mytool"]);
    // A full path, or a PATH of its own: not looked at for a `.mcp.json`, so nothing is said.
    assert!(found_as(&m, json!({"command": gone.to_string_lossy()}), false, false).is_empty());
    assert!(
        found_as(
            &m,
            json!({"command": "node", "env": {"PATH": own.to_string_lossy()}}),
            false,
            false
        )
        .is_empty()
    );
    // A bare name is looked up in Klefki's own PATH only.
    assert_eq!(
        kinds(&found_as(&m, json!({"command": "nothere"}), false, false)),
        [PreflightKind::NotFound]
    );
}

#[test]
fn network_paths_are_never_looked_at() {
    let m = machine();
    for command in [
        "\\\\nas\\share\\mcp.exe",
        "//nas/share/mcp",
        "\\\\?\\C:\\x.exe",
        "\\\\.\\pipe\\x",
        // The NT form Win32 passes through: `\??\UNC\host\share`, with either slash.
        "\\??\\UNC\\nas\\share\\mcp.exe",
        "/??/UNC/nas/share/mcp.exe",
        "\\??/UNC/nas/share/mcp.exe",
    ] {
        assert!(
            found(&m, json!({"command": command})).is_empty(),
            "{command}"
        );
        assert!(
            found_as(&m, json!({"command": command}), true, true).is_empty(),
            "{command}"
        );
    }
    // A PATH folder on another machine is skipped, not searched.
    let f = found(&m, json!({"command": "node", "env": {"PATH": "//nas/bin"}}));
    assert_eq!(kinds(&f), [PreflightKind::NotFound], "{f:?}");
}

#[test]
fn without_a_known_path_no_program_is_judged() {
    let probe = Probe::new(None, &vars, false);
    assert!(findings(&json!({"command": "nothere"}), true, &probe).is_empty());
}

#[test]
fn a_program_is_looked_up_once_per_reading() {
    let m = machine();
    let probe = Probe::new(Some(&m.path), &vars, false);
    for _ in 0..3 {
        assert_eq!(
            findings(&json!({"command": "nothere"}), true, &probe).len(),
            1
        );
    }
    assert_eq!(probe.found.borrow().len(), 1);
}

#[test]
fn a_switch_git_bash_turned_into_a_drive_is_named() {
    let m = machine();
    let f = found(
        &m,
        json!({"command": "cmd", "args": ["C:/", "npx", "-y", "pkg"]}),
    );
    let f: Vec<_> = f
        .into_iter()
        .filter(|f| f.kind == PreflightKind::MangledSwitch)
        .collect();
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].problem.starts_with("C:/ was probably /c"), "{f:?}");
    assert!(f[0].fix.contains("back to /c"), "{f:?}");
    let two = found(
        &m,
        json!({"command": "C:\\Windows\\System32\\CMD.EXE", "args": ["S:/", "C:/", "npx"]}),
    );
    let two: Vec<_> = two
        .into_iter()
        .filter(|f| f.kind == PreflightKind::MangledSwitch)
        .collect();
    assert!(
        two[0].problem.starts_with("S:/ C:/ were probably /s /c"),
        "{two:?}"
    );
    // Only right after cmd, and only for cmd.
    for entry in [
        json!({"command": "cmd", "args": ["/c", "npx"]}),
        json!({"command": "cmd", "args": ["/c", "dir", "C:/"]}),
        json!({"command": "node", "args": ["C:/"]}),
    ] {
        assert!(
            !kinds(&found(&m, entry.clone())).contains(&PreflightKind::MangledSwitch),
            "{entry}"
        );
    }
}

#[test]
fn docker_run_without_i_is_named() {
    let m = machine();
    let no_input = |entry: Value| kinds(&found(&m, entry)).contains(&PreflightKind::NoInput);
    assert!(no_input(
        json!({"command": "docker", "args": ["run", "--rm", "mcp/fetch"]})
    ));
    assert!(no_input(
        json!({"command": "podman", "args": ["run", "-e", "X=1", "img"]})
    ));
    assert!(no_input(
        json!({"command": "docker.exe", "args": ["container", "run", "-t", "img"]})
    ));
    for args in [
        json!(["run", "-i", "--rm", "mcp/fetch"]),
        json!(["run", "--rm", "-it", "img"]),
        json!(["run", "-dit", "img"]),
        json!(["run", "--interactive", "img"]),
        json!(["run", "--interactive=true", "img"]),
        json!(["container", "run", "-i", "img"]),
        // Not a run: nothing to say.
        json!(["compose", "run", "x"]),
        json!(["exec", "-t", "box", "srv"]),
        json!(["mcp", "gateway", "run"]),
    ] {
        assert!(
            !no_input(json!({"command": "docker", "args": args})),
            "{args}"
        );
    }
    assert!(!no_input(json!({"command": "node", "args": ["run", "x"]})));
}

#[test]
fn unset_variables_are_named_once_with_a_default_meaning_none() {
    let m = machine();
    let f = found(
        &m,
        json!({"command": "node", "args": ["${API_URL}/x", "${API_URL}"], "env": {"KEY": "${API_KEY}", "T": "${GITHUB_TOKEN}", "D": "${DEBUG:-0}"}}),
    );
    assert_eq!(kinds(&f), [PreflightKind::UnsetVariable], "{f:?}");
    assert_eq!(
        f[0].problem,
        "${API_URL} and ${API_KEY} aren't set where Klefki was started. Unless they are set where Claude Code \
         starts, Claude Code passes them on as written."
    );
    let one = found(&m, json!({"type": "http", "url": "https://${HOST}/mcp"}));
    assert_eq!(
        one[0].problem,
        "${HOST} isn't set where Klefki was started. Unless it is set where Claude Code starts, Claude Code \
         passes it on as written."
    );
    assert!(one[0].fix.contains("${HOST:-value}"), "{one:?}");
    let header = found(
        &m,
        json!({"type": "sse", "url": "https://x/sse", "headers": {"X-Key": "${SSE_KEY}"}}),
    );
    assert_eq!(kinds(&header), [PreflightKind::UnsetVariable]);
    // Not a reference: `$NAME`, `${}`, an open `${`.
    assert!(
        found(
            &m,
            json!({"command": "node", "args": ["$NOPE", "${}", "${OPEN"]})
        )
        .is_empty()
    );
}

#[test]
fn a_variable_is_looked_up_as_claude_code_fills_it_in() {
    let m = machine();
    // Set in the user's or the organisation's settings: Claude Code fills it in.
    let entry = json!({"command": "node", "args": ["${FROM_SETTINGS}", "${API_URL}"]});
    let probe = Probe::new(Some(&m.path), &vars, false).with_settings(["FROM_SETTINGS".to_owned()]);
    let f = findings(&entry, true, &probe);
    assert_eq!(
        f[0].problem,
        "${API_URL} isn't set where Klefki was started. Unless it is set where Claude Code starts, Claude Code \
         passes it on as written."
    );
    // Windows compares names without regard to case; elsewhere case counts.
    let entry = json!({"command": "node", "args": ["${from_settings}"]});
    let windows =
        Probe::new(Some(&m.path), &vars, true).with_settings(["FROM_SETTINGS".to_owned()]);
    assert!(findings(&entry, true, &windows).is_empty());
    let unix = Probe::new(Some(&m.path), &vars, false).with_settings(["FROM_SETTINGS".to_owned()]);
    assert_eq!(
        kinds(&findings(&entry, true, &unix)),
        [PreflightKind::UnsetVariable]
    );
    // Claude Code never fills these in, whatever is set: no name, so nothing to set.
    assert!(
        found(
            &m,
            json!({"command": "node", "args": ["${1X}", "${HO-ME}", "${ A }", "${1X:-d}"]})
        )
        .is_empty()
    );
}

#[test]
fn a_secret_is_never_offered_a_default_written_in_the_file() {
    // Review A1: `${TOKEN:-value}` would put the token in `.claude.json` in plain text.
    let m = machine();
    for name in [
        "AZURE_DEVOPS_PAT",
        "API_KEY",
        "GITLAB_TOKEN",
        "CLIENT_SECRET",
    ] {
        let f = found(
            &m,
            json!({"command": "node", "env": {"X": format!("${{{name}}}")}}),
        );
        assert_eq!(kinds(&f), [PreflightKind::UnsetVariable], "{name}");
        assert_eq!(
            f[0].fix, "Set it before you start Claude Code and Klefki.",
            "{name}"
        );
    }
    // Several: the default is offered for one that is not a secret, never named after one that is.
    let f = found(
        &m,
        json!({"command": "node", "args": ["${API_KEY}", "${REGION}"]}),
    );
    assert_eq!(
        f[0].fix,
        "Set them before you start Claude Code and Klefki, or give a default: ${REGION:-value}."
    );
    // Not a word of its own: no secret.
    let path = found(&m, json!({"command": "node", "args": ["${PATTERN}"]}));
    assert!(path[0].fix.contains("${PATTERN:-value}"), "{path:?}");
    let both = found(
        &m,
        json!({"command": "node", "args": ["${API_KEY}", "${SECRET_TOKEN}"]}),
    );
    assert_eq!(
        both[0].fix,
        "Set them before you start Claude Code and Klefki."
    );
}

#[test]
fn many_unset_variables_are_counted_after_a_few() {
    let m = machine();
    let args: Vec<String> = (0..9).map(|i| format!("${{V{i}}}")).collect();
    let f = found(&m, json!({"command": "node", "args": args}));
    assert_eq!(
        f[0].problem,
        "${V0}, ${V1}, ${V2}, ${V3}, ${V4} and 4 more aren't set where Klefki was started. Unless they are set \
         where Claude Code starts, Claude Code passes them on as written."
    );
}

#[test]
fn empty_variables_are_named() {
    let m = machine();
    let f = found(
        &m,
        json!({"command": "node", "env": {"API_KEY": "", "LEVEL": "debug", "PAD": "  "}}),
    );
    assert_eq!(kinds(&f), [PreflightKind::EmptyValue]);
    assert_eq!(f[0].problem, "API_KEY and PAD are empty.");
    let one = found(&m, json!({"command": "node", "env": {"API_KEY": ""}}));
    assert_eq!(one[0].problem, "API_KEY is empty.");
}

#[test]
fn findings_come_in_one_order_and_are_cleaned() {
    let m = machine();
    let f = found(
        &m,
        json!({"command": "nothere\u{1b}[31m\u{202e}", "env": {"E": "", "R": "${R_UNSET}"}}),
    );
    assert_eq!(
        kinds(&f),
        [
            PreflightKind::NotFound,
            PreflightKind::UnsetVariable,
            PreflightKind::EmptyValue
        ]
    );
    assert!(!f[0].problem.contains(['\u{1b}', '\u{202e}']), "{f:?}");
    let long = "x".repeat(500);
    let f = found(&m, json!({"command": long}));
    assert!(f[0].problem.chars().count() < 200, "{f:?}");
}

#[test]
fn broken_and_remote_definitions_without_variables_have_none() {
    let m = machine();
    for entry in [
        json!("node a.js"),
        json!({"args": ["a.js"]}),
        json!({"command": ""}),
        json!({"command": 3}),
        json!({"type": "http", "url": "https://x.example.com"}),
        json!({"type": "carrier-pigeon", "command": "nothere"}),
    ] {
        assert!(found(&m, entry.clone()).is_empty(), "{entry}");
    }
}

#[test]
fn only_pathext_endings_that_are_endings_are_tried() {
    // Found by mutation testing: an empty ending, a lone dot or one without its dot names no other file.
    let m = machine();
    let own = folder_with(&m.root.join("own"), &["tool.", "toolEXE"]);
    let f = found_as(
        &m,
        json!({"command": "tool", "env": {"PATH": own.to_string_lossy(), "PATHEXT": ";.;EXE;.CMD"}}),
        true,
        true,
    );
    assert_eq!(kinds(&f), [PreflightKind::NotFound], "{f:?}");
}

#[test]
fn a_repository_never_sets_the_endings_looked_for() {
    // Found by mutation testing: a `.mcp.json`'s PATHEXT is not used either, only Klefki's.
    let m = machine();
    let f = found_as(
        &m,
        json!({"command": "node", "env": {"PATHEXT": ".NOTHING"}}),
        false,
        true,
    );
    assert!(f.is_empty(), "{f:?}");
    // The user's own definition may set them.
    let own = found_as(
        &m,
        json!({"command": "node", "env": {"PATHEXT": ".NOTHING"}}),
        true,
        true,
    );
    assert_eq!(kinds(&own), [PreflightKind::NotFound], "{own:?}");
}

#[test]
fn a_relative_folder_on_the_path_is_not_searched() {
    // Found by mutation testing. Tests run in the crate's folder, which has `src/lib.rs`; Windows' lookup needs no
    // executable bit, so only the rule keeps it from being found.
    let m = machine();
    assert!(Path::new("src").join("lib.rs").is_file());
    let f = found_as(
        &m,
        json!({"command": "lib.rs", "env": {"PATH": "src"}}),
        true,
        true,
    );
    assert_eq!(kinds(&f), [PreflightKind::NotFound], "{f:?}");
}

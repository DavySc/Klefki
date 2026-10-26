use super::*;
use serde_json::json;

fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect()
}

fn strings(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// A folder with a program `bin/prog` (and `here/prog`, which must never be found by name).
struct Place {
    dir: tempfile::TempDir,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for d in ["cfg", "org", "project/.claude", "bin", "other", "here"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let p = Self { dir };
        p.program("bin/prog");
        p.program("here/prog");
        p
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn program(&self, rel: &str) {
        let path = self.path(rel);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn write(&self, rel: &str, doc: &Value) {
        std::fs::write(self.path(rel), doc.to_string()).unwrap();
    }

    fn paths(&self) -> ConfigPaths {
        ConfigPaths {
            managed_dir: Some(self.path("org")),
            ..ConfigPaths::in_dir(self.path("cfg"))
        }
    }

    fn bin_path(&self) -> String {
        self.path("bin").to_string_lossy().into_owned()
    }

    /// Launches `entry` in `here/` with Klefki's environment `klefki` (PATH added) and the settings `settings`.
    fn launch_with(
        &self,
        entry: &Value,
        klefki: &[(&str, &str)],
        settings: &[(&str, &str)],
        os_rules: Os,
    ) -> Result<Launch, NotStarted> {
        let mut vars = os(klefki);
        if !klefki.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")) {
            vars.push(("PATH".into(), self.bin_path().into()));
        }
        let settings = strings(settings);
        let folder = self.path("here");
        launch(&Inputs {
            entry,
            folder: &folder,
            klefki_env: &vars,
            settings: &settings,
            claude_version: Some("2.1.310"),
            session_id: "11111111-2222-4333-8444-555555555555",
            os: os_rules,
        })
    }

    fn launch(&self, entry: &Value, klefki: &[(&str, &str)], settings: &[(&str, &str)]) -> Launch {
        self.launch_with(entry, klefki, settings, Os::Linux)
            .expect("launch")
    }
}

fn var<'a>(launch: &'a Launch, name: &str) -> Option<&'a str> {
    launch
        .env
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.to_str().unwrap())
}

fn stdio(env: Value) -> Value {
    json!({"type": "stdio", "command": "prog", "args": [], "env": env})
}

#[test]
fn a_project_is_trusted_only_where_its_own_trust_dialog_was_accepted() {
    let doc = json!({"projects": {
        "/p": {"hasTrustDialogAccepted": true},
        "/q": {"hasTrustDialogAccepted": false},
        "/r": {"hasTrustDialogAccepted": "true"},
        "/s": {}
    }});
    assert!(trusted(&doc, "/p"));
    for project in ["/q", "/r", "/s", "/t"] {
        assert!(!trusted(&doc, project), "{project}");
    }
    // Measured: a trusted parent does not make its subfolder trusted.
    assert!(!trusted(&doc, "/p/sub"));
    assert!(!trusted(&json!({}), "/p"));
}

#[test]
fn settings_env_comes_in_claude_codes_order_and_the_project_only_when_trusted() {
    let p = Place::new();
    p.write(
        "cfg/settings.json",
        &json!({"env": {"A": "user"}, "other": 1}),
    );
    p.write(
        "project/.claude/settings.json",
        &json!({"env": {"A": "project"}}),
    );
    p.write(
        "project/.claude/settings.local.json",
        &json!({"env": {"A": "local"}}),
    );
    p.write(
        "org/managed-settings.json",
        &json!({"env": {"A": "managed"}}),
    );
    std::fs::create_dir_all(p.path("org/managed-settings.d")).unwrap();
    p.write(
        "org/managed-settings.d/10.json",
        &json!({"env": {"A": "drop-in"}}),
    );
    let project = p.path("project");
    let a = |list: Vec<(String, String)>| list.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
    assert_eq!(
        a(settings_env(&p.paths(), Some(&project))),
        ["user", "project", "local", "managed", "drop-in"]
    );
    assert_eq!(
        a(settings_env(&p.paths(), None)),
        ["user", "managed", "drop-in"],
        "an untrusted project's files are never read"
    );
}

#[test]
fn settings_files_that_are_missing_broken_or_have_no_env_object_add_nothing() {
    let p = Place::new();
    std::fs::write(p.path("cfg/settings.json"), "{not json").unwrap();
    p.write("project/.claude/settings.json", &json!({"env": ["A"]}));
    p.write("project/.claude/settings.local.json", &json!([1, 2]));
    assert!(settings_env(&p.paths(), Some(&p.path("project"))).is_empty());
    let big = format!(
        "{{\"env\": {{\"A\": \"{}\"}}}}",
        "x".repeat(usize::try_from(MAX_FILE).unwrap())
    );
    std::fs::write(p.path("cfg/settings.json"), big).unwrap();
    assert!(
        settings_env(&p.paths(), None).is_empty(),
        "too large to read"
    );
}

#[test]
fn from_the_home_folder_the_users_settings_are_read_once() {
    let p = Place::new();
    let paths = ConfigPaths::in_dir(p.path("project/.claude"));
    p.write(
        "project/.claude/settings.json",
        &json!({"env": {"A": "user"}}),
    );
    assert_eq!(
        settings_env(&paths, Some(&p.path("project"))),
        strings(&[("A", "user")])
    );
}

#[test]
fn a_settings_value_that_is_not_text_is_passed_as_javascript_writes_it() {
    let p = Place::new();
    p.write(
        "cfg/settings.json",
        &json!({"env": {"N": 5, "F": 2.5, "W": 1.0, "M": -3.0, "Z0": -0.0, "E": 1e21, "B": true, "Z": null, "O": {"a": 1}, "L": [1, null, "x"]}}),
    );
    assert_eq!(
        settings_env(&p.paths(), None),
        strings(&[
            ("N", "5"),
            ("F", "2.5"),
            ("W", "1"),
            ("M", "-3"),
            ("Z0", "0"),
            ("E", "1e+21"),
            ("B", "true"),
            ("Z", "null"),
            ("O", "[object Object]"),
            ("L", "1,,x")
        ])
    );
}

#[test]
fn later_layers_win_klefki_then_settings_then_claude_codes_own_then_the_definition() {
    let p = Place::new();
    let entry = stdio(json!({"FOUR": "definition", "CLAUDECODE": "mine"}));
    let l = p.launch(
        &entry,
        &[
            ("ONE", "klefki"),
            ("TWO", "klefki"),
            ("THREE", "klefki"),
            ("FOUR", "klefki"),
        ],
        &[
            ("TWO", "settings"),
            ("THREE", "settings"),
            ("FOUR", "settings"),
        ],
    );
    assert_eq!(var(&l, "ONE"), Some("klefki"));
    assert_eq!(var(&l, "TWO"), Some("settings"));
    assert_eq!(var(&l, "FOUR"), Some("definition"));
    assert_eq!(
        var(&l, "CLAUDECODE"),
        Some("mine"),
        "the definition wins over Claude Code's own"
    );
    let names: Vec<&OsString> = l.env.iter().map(|(n, _)| n).collect();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names.len(), unique.len(), "each name once");
}

#[test]
fn claude_codes_own_variables_are_set_as_measured() {
    let p = Place::new();
    let l = p.launch(&stdio(json!({})), &[("CLAUDECODE", "0")], &[]);
    let folder = p.path("here").to_string_lossy().into_owned();
    assert_eq!(var(&l, "CLAUDECODE"), Some("1"), "over Klefki's own");
    assert_eq!(var(&l, "CLAUDE_CODE_ENTRYPOINT"), Some("cli"));
    assert_eq!(var(&l, "AI_AGENT"), Some("claude-code_2-1-310_harness"));
    assert_eq!(var(&l, "CLAUDE_PROJECT_DIR"), Some(folder.as_str()));
    assert_eq!(
        var(&l, "CLAUDE_CODE_SESSION_ID"),
        Some("11111111-2222-4333-8444-555555555555")
    );
    assert_eq!(var(&l, "NoDefaultCurrentDirectoryInExePath"), Some("1"));
    assert_eq!(var(&l, "COREPACK_ENABLE_AUTO_PIN"), Some("0"));
    assert_eq!(var(&l, "LC_CTYPE"), Some("C.UTF-8"));
    assert_eq!(l.cwd, p.path("here"));
}

#[test]
fn lc_ctype_is_set_on_linux_only_and_not_when_lc_all_is() {
    let p = Place::new();
    let entry = stdio(json!({}));
    let l = p.launch(&entry, &[("LC_CTYPE", "xx")], &[]);
    assert_eq!(var(&l, "LC_CTYPE"), Some("C.UTF-8"), "replaces Klefki's");
    let l = p.launch(&entry, &[("LC_ALL", "C")], &[]);
    assert_eq!(var(&l, "LC_CTYPE"), None);
    assert_eq!(var(&l, "LC_ALL"), Some("C"));
    let l = p.launch_with(&entry, &[], &[], Os::Mac).unwrap();
    assert_eq!(var(&l, "LC_CTYPE"), None, "measured on Linux only");
}

#[test]
fn without_claude_codes_version_ai_agent_is_not_set() {
    let p = Place::new();
    let vars = os(&[("PATH", &p.bin_path())]);
    let folder = p.path("here");
    let l = launch(&Inputs {
        entry: &stdio(json!({})),
        folder: &folder,
        klefki_env: &vars,
        settings: &[],
        claude_version: None,
        session_id: "x",
        os: Os::Linux,
    })
    .unwrap();
    assert_eq!(var(&l, "AI_AGENT"), None);
}

#[test]
fn the_client_secret_in_klefkis_environment_never_reaches_the_server() {
    let p = Place::new();
    let l = p.launch(
        &stdio(json!({})),
        &[("MCP_CLIENT_SECRET", "s3cr3t-value")],
        &[],
    );
    assert_eq!(var(&l, "MCP_CLIENT_SECRET"), None);
    // On Windows names compare without case.
    let l = p
        .launch_with(
            &stdio(json!({})),
            &[("mcp_client_secret", "s3cr3t-value")],
            &[],
            Os::Windows,
        )
        .err();
    assert_eq!(
        l,
        Some(NotStarted::NotFound("prog".to_owned())),
        "no prog.exe here"
    );
    let entry = json!({"command": p.path("bin/prog.exe").to_string_lossy()});
    p.program("bin/prog.exe");
    let l = p
        .launch_with(
            &entry,
            &[("mcp_client_secret", "s3cr3t-value")],
            &[],
            Os::Windows,
        )
        .unwrap();
    assert!(
        l.env
            .iter()
            .all(|(n, _)| !n.eq_ignore_ascii_case("MCP_CLIENT_SECRET")),
        "{:?}",
        l.env
    );
}

#[test]
fn variables_are_filled_in_as_claude_code_does() {
    let vars = |name: &str| match name {
        "HOME" => Some("/hh".to_owned()),
        "EMPTY" => Some(String::new()),
        "_X1" => Some("x1".to_owned()),
        _ => None,
    };
    // Each measured with Claude Code 2.1.310.
    for (written, filled) in [
        ("${HOME}", "/hh"),
        ("a${HOME}b${HOME}", "a/hhb/hh"),
        ("${_X1}", "x1"),
        ("${NOPE}", "${NOPE}"),
        ("${NOPE:-dflt}", "dflt"),
        ("${NOPE:-}", ""),
        ("${NOPE:-a:-b}", "a:-b"),
        ("${EMPTY:-dflt}", ""),
        ("${HOME:-dflt}", "/hh"),
        ("${NOPE:-${HOME}}", "${HOME}"),
        ("x${}y", "x${}y"),
        ("${HOME", "${HOME"),
        ("$HOME", "$HOME"),
        ("${ HOME }", "${ HOME }"),
        ("${HOME }", "${HOME }"),
        ("${HO-ME}", "${HO-ME}"),
        ("${1X}", "${1X}"),
        ("${é}", "${é}"),
        ("${1X:-d}", "${1X:-d}"),
        ("tail ${", "tail ${"),
        ("", ""),
    ] {
        assert_eq!(expand(written, &vars), filled, "{written}");
    }
}

#[test]
fn filling_in_sees_klefki_and_the_settings_but_not_claude_codes_own_or_the_definitions() {
    let p = Place::new();
    let entry = json!({
        "command": "${PROG:-nope}",
        "args": ["${FROM_SETTINGS}", "${FROM_KLEFKI}", "${CLAUDECODE:-no}", "${CLAUDE_PROJECT_DIR:-no}", "${OWN:-no}", 7],
        "env": {"OWN": "own", "USES": "${FROM_SETTINGS}-${OWN:-no}"}
    });
    let l = p.launch(
        &entry,
        &[("FROM_KLEFKI", "k"), ("PROG", "prog")],
        &[("FROM_SETTINGS", "s")],
    );
    assert_eq!(
        l.program,
        p.path("bin/prog"),
        "the command is filled in too"
    );
    assert_eq!(l.args, ["s", "k", "no", "no", "no", "7"]);
    assert_eq!(var(&l, "USES"), Some("s-no"));
}

#[test]
fn a_bare_name_is_looked_up_on_the_servers_own_path_never_in_the_working_folder() {
    let p = Place::new();
    p.program("other/only-here");
    let other = p.path("other").to_string_lossy().into_owned();
    // The definition's own PATH is the one searched.
    let l = p.launch(
        &json!({"command": "only-here", "env": {"PATH": other}}),
        &[],
        &[],
    );
    assert_eq!(l.program, p.path("other/only-here"));
    // `here/prog` is in the working folder; relative and empty PATH entries are never searched.
    for path in [".", "", "here", ":.:"] {
        let got = p.launch_with(
            &json!({"command": "prog", "env": {"PATH": path}}),
            &[],
            &[],
            Os::Linux,
        );
        assert_eq!(
            got.err(),
            Some(NotStarted::NotFound("prog".to_owned())),
            "PATH={path:?}"
        );
    }
    let got = p.launch_with(&json!({"command": "missing"}), &[], &[], Os::Linux);
    assert_eq!(got.err(), Some(NotStarted::NotFound("missing".to_owned())));
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_run_or_a_folder_is_not_the_program() {
    let p = Place::new();
    std::fs::write(p.path("other/plain"), "x").unwrap();
    std::fs::create_dir_all(p.path("other/dir")).unwrap();
    let other = p.path("other").to_string_lossy().into_owned();
    for name in ["plain", "dir"] {
        let got = p.launch_with(
            &json!({"command": name}),
            &[("PATH", &other)],
            &[],
            Os::Linux,
        );
        assert_eq!(got.err(), Some(NotStarted::NotFound(name.to_owned())));
    }
}

#[test]
fn a_command_with_a_folder_in_it_is_a_path_from_the_working_folder() {
    let p = Place::new();
    let l = p.launch(&json!({"command": "./prog"}), &[], &[]);
    assert_eq!(l.program, p.path("here").join("./prog"));
    let full = p.path("other/x");
    p.program("other/x");
    let l = p.launch(&json!({"command": full.to_string_lossy()}), &[], &[]);
    assert_eq!(l.program, full);
}

#[test]
fn a_name_shown_as_not_found_is_sanitised() {
    let p = Place::new();
    let got = p.launch_with(
        &json!({"command": "evil\u{1b}[31m\u{202e}x"}),
        &[],
        &[],
        Os::Linux,
    );
    assert_eq!(got.err(), Some(NotStarted::NotFound("evilx".to_owned())));
}

#[test]
fn on_windows_a_cmd_or_bat_is_left_to_claude_code_and_an_exe_is_started() {
    // Windows compares file names without case; the files here carry PATHEXT's own spelling.
    let p = Place::new();
    for f in [
        "bin/npx.CMD",
        "bin/old.BAT",
        "bin/node.EXE",
        "bin/tool.COM",
        "bin/script.ps1",
        "bin/both.EXE",
    ] {
        p.program(f);
    }
    p.program("other/both.CMD");
    let other = p.path("other").to_string_lossy().into_owned();
    let path = format!("{other};\"{}\"", p.bin_path());
    let run = |command: &str| {
        p.launch_with(
            &json!({"command": command}),
            &[("Path", &path)],
            &[],
            Os::Windows,
        )
        .map(|l| l.program)
    };
    assert_eq!(run("npx"), Err(NotStarted::NeedsShell));
    assert_eq!(run("npx.CMD"), Err(NotStarted::NeedsShell));
    assert_eq!(run("old"), Err(NotStarted::NeedsShell));
    assert_eq!(run("script.ps1"), Err(NotStarted::NeedsShell));
    assert_eq!(
        run("node"),
        Ok(p.path("bin/node.EXE")),
        "a folder in quotes is read"
    );
    assert_eq!(run("node.EXE"), Ok(p.path("bin/node.EXE")));
    assert_eq!(run("tool"), Ok(p.path("bin/tool.COM")));
    // The first folder of the PATH with a match decides, as Windows looks it up.
    assert_eq!(run("both"), Err(NotStarted::NeedsShell));
    // PATHEXT decides which endings are tried, in its order.
    let l = p.launch_with(
        &json!({"command": "both"}),
        &[("Path", &p.bin_path()), ("PATHEXT", ".EXE")],
        &[],
        Os::Windows,
    );
    assert_eq!(l.map(|l| l.program), Ok(p.path("bin/both.EXE")));
    // No `#!` is asked of a Windows program.
    std::fs::write(p.path("bin/node.EXE"), "MZ").unwrap();
    assert_eq!(run("node"), Ok(p.path("bin/node.EXE")));
}

#[test]
fn on_windows_variable_names_compare_without_case() {
    let p = Place::new();
    p.program("bin/prog.EXE");
    let l = p
        .launch_with(
            &json!({"command": "prog", "env": {"foo": "definition"}}),
            &[("Path", &p.bin_path()), ("FOO", "klefki")],
            &[("Foo", "settings")],
            Os::Windows,
        )
        .unwrap();
    let foo: Vec<_> = l
        .env
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("foo"))
        .collect();
    assert_eq!(
        foo,
        [&(OsString::from("foo"), OsString::from("definition"))]
    );
    // On Linux they are different variables.
    let l = p.launch(
        &json!({"command": "prog", "env": {"foo": "definition"}}),
        &[("FOO", "klefki")],
        &[],
    );
    assert_eq!(var(&l, "FOO"), Some("klefki"));
    assert_eq!(var(&l, "foo"), Some("definition"));
}

#[test]
fn only_a_stdio_server_with_a_command_is_started() {
    let p = Place::new();
    for entry in [
        json!({"type": "http", "url": "https://x", "command": "prog"}),
        json!({"type": "sse", "url": "https://x"}),
        json!({"type": "stdio"}),
        json!({"command": ""}),
        json!({"command": 5}),
        json!("prog"),
    ] {
        assert_eq!(
            p.launch_with(&entry, &[], &[], Os::Linux).err(),
            Some(NotStarted::NotStdio),
            "{entry}"
        );
    }
    assert!(
        p.launch_with(&json!({"command": "prog"}), &[], &[], Os::Linux)
            .is_ok()
    );
}

#[test]
fn the_values_to_hide_are_the_definitions_and_the_settings_not_klefkis() {
    let p = Place::new();
    let entry = stdio(json!({"TOKEN": "tok-${SUFFIX}", "N": 12345678}));
    let l = p.launch(
        &entry,
        &[("SUFFIX", "abcdef"), ("KLEFKI_ONLY", "klefki-value")],
        &[("S", "settings-secret")],
    );
    assert_eq!(
        l.hidden,
        ["12345678", "settings-secret", "tok-${SUFFIX}", "tok-abcdef"]
    );
    assert_eq!(var(&l, "N"), Some("12345678"));
}

#[test]
fn session_ids_look_like_claude_codes_and_differ() {
    let a = new_session_id();
    let b = new_session_id();
    assert_ne!(a, b);
    for id in [&a, &b] {
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12],
            "{id}"
        );
        assert!(
            id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
            "{id}"
        );
        assert!(parts[2].starts_with('4'), "version 4: {id}");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "variant 1: {id}"
        );
    }
}

#[test]
fn what_claude_code_keeps_from_its_servers_is_taken_out_one_name_at_a_time() {
    let p = Place::new();
    for name in [
        "MCP_CLIENT_SECRET",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_CODE_WEBSOCKET_AUTH_FILE_DESCRIPTOR",
        "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
        "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_",
        "otel_lower",
    ] {
        let l = p.launch(
            &stdio(json!({})),
            &[(name, "taken-out"), ("KEPT", "kept")],
            &[],
        );
        assert_eq!(var(&l, name), None, "{name}");
        assert_eq!(var(&l, "KEPT"), Some("kept"), "{name}");
    }
    // Measured as passed on by Claude Code.
    for name in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        "OTELX",
        "MY_OTEL_X",
    ] {
        let l = p.launch(&stdio(json!({})), &[(name, "kept")], &[]);
        assert_eq!(var(&l, name), Some("kept"), "{name}");
    }
    // A settings file or the definition may still set one: that is the user's own choice, as in Claude Code.
    let l = p.launch(
        &stdio(json!({"OTEL_A": "own"})),
        &[],
        &[("OTEL_B", "settings")],
    );
    assert_eq!(
        (var(&l, "OTEL_A"), var(&l, "OTEL_B")),
        (Some("own"), Some("settings"))
    );
}

#[test]
fn the_same_key_in_two_settings_files_takes_the_later_files_value() {
    let p = Place::new();
    p.write(
        "cfg/settings.json",
        &json!({"env": {"A": "user", "B": "user"}}),
    );
    p.write(
        "project/.claude/settings.json",
        &json!({"env": {"A": "project", "C": "project"}}),
    );
    p.write(
        "project/.claude/settings.local.json",
        &json!({"env": {"C": "local"}}),
    );
    p.write(
        "org/managed-settings.json",
        &json!({"env": {"A": "managed"}}),
    );
    let settings = settings_env(&p.paths(), Some(&p.path("project")));
    let pairs: Vec<(&str, &str)> = settings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let l = p.launch(&stdio(json!({})), &[("A", "klefki")], &pairs);
    assert_eq!(
        var(&l, "A"),
        Some("managed"),
        "managed settings win over the project's"
    );
    assert_eq!(var(&l, "B"), Some("user"));
    assert_eq!(
        var(&l, "C"),
        Some("local"),
        "settings.local.json wins over settings.json"
    );
}

#[cfg(unix)]
#[test]
fn a_file_with_no_hash_bang_line_is_not_run_through_a_shell() {
    let p = Place::new();
    let other = p.path("other").to_string_lossy().into_owned();
    let write = |name: &str, bytes: &[u8]| {
        p.program(&format!("other/{name}"));
        std::fs::write(p.path(&format!("other/{name}")), bytes).unwrap();
    };
    write("plain-script", b"echo hi\n");
    write("empty", b"");
    write("short", b"#");
    write("hashbang", b"#!/bin/sh\necho hi\n");
    write("bare-hashbang", b"#!");
    write("elf", b"\x7fELF\x02\x01");
    write("macho", &0xfeed_facf_u32.to_le_bytes());
    write("fat", &0xcafe_babe_u32.to_be_bytes());
    let run = |name: &str| {
        p.launch_with(
            &json!({"command": name}),
            &[("PATH", &other)],
            &[],
            Os::Linux,
        )
        .map(|l| l.program)
    };
    for name in ["plain-script", "empty", "short"] {
        assert_eq!(
            run(name),
            Err(NotStarted::NotProgram(name.to_owned())),
            "{name}"
        );
    }
    for name in ["hashbang", "bare-hashbang", "elf", "macho", "fat"] {
        assert_eq!(run(name), Ok(p.path(&format!("other/{name}"))), "{name}");
    }
    let by_path = p.path("other/plain-script").to_string_lossy().into_owned();
    assert!(
        matches!(run(&by_path), Err(NotStarted::NotProgram(_))),
        "by path too"
    );
}

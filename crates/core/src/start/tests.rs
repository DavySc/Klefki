//! Each case is one checked against Claude Code 2.1.308 with `claude --settings … mcp list` in a project like this one
//! (docs/progress/2026-10-22-start-with-a-profile.md): what loaded there is what the plan says loads.
use super::*;
use crate::api::Scope;
use serde_json::{Value, json};
use std::path::PathBuf;

struct Place {
    dir: tempfile::TempDir,
}

fn stdio(name: &str) -> Value {
    json!({"type": "stdio", "command": "node", "args": ["s.js", name]})
}

impl Place {
    /// User servers a, b, c, d and my.server; d is off in the project, which has the local server loc. The project
    /// of another folder has its own local server far.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for d in ["cfg", "org", "proj/.claude", "other"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let p = Self { dir };
        let doc = json!({
            "mcpServers": {"a": stdio("a"), "b": stdio("b"), "c": stdio("c"), "d": stdio("d"),
                           "my.server": stdio("m"), "web": {"type": "http", "url": "https://example.com/mcp"}},
            "claudeAiMcpEverConnected": ["claude.ai Notes"],
            "projects": {
                p.project(): {"disabledMcpServers": ["d"], "mcpServers": {"loc": stdio("loc")}},
                p.other(): {"mcpServers": {"far": stdio("far")}}
            }
        });
        p.write("cfg/.claude.json", &doc);
        p
    }

    fn project(&self) -> String {
        self.dir
            .path()
            .join("proj")
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn other(&self) -> String {
        self.dir
            .path()
            .join("other")
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn paths(&self) -> ConfigPaths {
        ConfigPaths {
            managed_dir: Some(self.dir.path().join("org")),
            ..ConfigPaths::in_dir(self.dir.path().join("cfg"))
        }
    }

    fn write(&self, rel: &str, doc: &Value) {
        std::fs::write(self.dir.path().join(rel), doc.to_string()).unwrap();
    }

    /// `rel` written with `/`, as this system writes paths.
    fn file(&self, rel: &str) -> PathBuf {
        rel.split('/')
            .fold(self.dir.path().to_path_buf(), |path, part| path.join(part))
    }

    fn plan(&self, members: &[ConnectionId]) -> Result<StartPlan, String> {
        plan(&self.paths(), "Work", members, &self.project(), false)
    }
}

fn user(name: &str) -> ConnectionId {
    ConnectionId::new(Scope::User, None, name)
}

/// The sentence of `list` about the server `name`.
fn about<'a>(list: &'a [String], name: &str) -> &'a str {
    list.iter()
        .find(|s| s.starts_with(&format!("{name} ")) || s.starts_with(&format!("{name}:")))
        .unwrap_or_else(|| panic!("nothing about {name} in {list:?}"))
}

#[test]
fn with_no_other_list_only_the_profiles_servers_load() {
    let p = Place::new();
    let plan = p.plan(&[user("a"), user("b")]).unwrap();
    assert_eq!(plan.loads, ["a", "b"]);
    assert!(plan.exact(), "{plan:?}");
    let settings = r#"{"allowedMcpServers":[{"serverName":"a"},{"serverName":"b"}]}"#;
    assert_eq!(plan.args, ["--settings", settings]);
    assert_eq!(
        plan.line,
        format!(
            "cd {} && claude --settings '{settings}'",
            crate::inspector::posix_word(&p.project()).unwrap()
        )
    );
    assert!(plan.to_do.is_empty());
}

#[test]
fn a_server_off_in_the_project_is_not_turned_on() {
    let p = Place::new();
    let plan = p.plan(&[user("a"), user("d")]).unwrap();
    assert_eq!(plan.loads, ["a"]);
    assert!(
        about(&plan.missing, "d").contains("is off in proj"),
        "{plan:?}"
    );
    assert!(plan.extra.is_empty());
}

#[test]
fn the_list_is_added_to_the_allowed_lists_of_every_settings_file() {
    for (rel, shown) in [
        (".claude/settings.json", ".claude/settings.json".to_owned()),
        (
            ".claude/settings.local.json",
            ".claude/settings.local.json".to_owned(),
        ),
        ("cfg/settings.json", String::new()),
    ] {
        let p = Place::new();
        let file = if rel.starts_with("cfg") {
            p.file(rel)
        } else {
            p.file(&format!("proj/{rel}"))
        };
        std::fs::write(&file, r#"{"allowedMcpServers":[{"serverName":"c"}]}"#).unwrap();
        let plan = p.plan(&[user("a")]).unwrap();
        assert_eq!(plan.loads, ["a"], "{rel}");
        assert!(plan.missing.is_empty(), "{rel}: {plan:?}");
        assert_eq!(plan.extra.len(), 1, "{rel}: {plan:?}");
        let shown = if shown.is_empty() {
            policy::home_as_tilde(&file)
        } else {
            shown
        };
        assert_eq!(
            about(&plan.extra, "c"),
            format!("c: allowedMcpServers in {shown} lets it load too."),
            "{rel}"
        );
    }
}

#[test]
fn the_organisations_allowed_list_counts_too_and_its_only_rule_leaves_the_list_out() {
    let p = Place::new();
    p.write(
        "org/managed-settings.json",
        &json!({"allowedMcpServers": [{"serverName": "b"}]}),
    );
    let plan = p.plan(&[user("a")]).unwrap();
    assert_eq!(plan.loads, ["a"]);
    assert!(
        about(&plan.extra, "b").contains("lets it load too"),
        "{plan:?}"
    );

    // With allowManagedMcpServersOnly, Claude Code loads what the organisation's list says, with a list or without.
    for doc in [
        json!({"allowedMcpServers": [{"serverName": "b"}], "allowManagedMcpServersOnly": true}),
        json!({"allowManagedMcpServersOnly": true}),
    ] {
        p.write("org/managed-settings.json", &doc);
        let why = p.plan(&[user("a")]).unwrap_err();
        assert!(why.contains("allowManagedMcpServersOnly"), "{why}");
    }
}

#[test]
fn a_command_rule_elsewhere_makes_a_name_count_for_no_local_server() {
    let p = Place::new();
    p.write(
        "cfg/settings.json",
        &json!({"allowedMcpServers": [{"serverCommand": ["node", "s.js", "b"]}]}),
    );
    let plan = p.plan(&[user("a"), user("web")]).unwrap();
    // A remote server's name still counts: the list has no address rule.
    assert_eq!(plan.loads, ["web"]);
    let a = about(&plan.missing, "a");
    assert!(a.contains("by their command only"), "{a}");
    assert!(
        about(&plan.extra, "b").contains("lets it load too"),
        "{plan:?}"
    );

    // Nearby: an address rule does the same for remote servers.
    p.write(
        "cfg/settings.json",
        &json!({"allowedMcpServers": [{"serverUrl": "https://other.example/*"}]}),
    );
    let plan = p.plan(&[user("a"), user("web")]).unwrap();
    assert_eq!(plan.loads, ["a"]);
    assert!(
        about(&plan.missing, "web").contains("by their address only"),
        "{plan:?}"
    );
}

#[test]
fn a_denied_rule_still_wins() {
    for rel in ["cfg/settings.json", "org/managed-settings.json"] {
        let p = Place::new();
        p.write(rel, &json!({"deniedMcpServers": [{"serverName": "a"}]}));
        let plan = p.plan(&[user("a"), user("b")]).unwrap();
        assert_eq!(plan.loads, ["b"], "{rel}");
        assert!(
            about(&plan.missing, "a").contains("deniedMcpServers"),
            "{rel}: {plan:?}"
        );
    }
}

#[test]
fn servers_a_name_cannot_let_in_are_said_and_left_out_of_the_list() {
    let p = Place::new();
    let far = ConnectionId::new(Scope::Local, Some(&p.other()), "far");
    let plan = p
        .plan(&[
            user("a"),
            user("my.server"),
            far,
            ConnectionId::new(Scope::ClaudeAi, None, "claude.ai Notes"),
            user("removed"),
        ])
        .unwrap();
    assert_eq!(plan.loads, ["a"]);
    assert_eq!(
        plan.args[1],
        r#"{"allowedMcpServers":[{"serverName":"a"}]}"#
    );
    assert!(about(&plan.missing, "my.server").contains("letters, digits"));
    assert!(about(&plan.missing, "far").contains("a server of other"));
    assert!(about(&plan.missing, "claude.ai Notes").contains("by its address only"));
    assert!(
        plan.missing
            .contains(&"One server of Work is no longer configured.".to_owned()),
        "{plan:?}"
    );
    assert_eq!(plan.missing.len(), 4, "{plan:?}");
}

#[test]
fn with_no_server_to_name_there_is_no_line() {
    let p = Place::new();
    let why = p.plan(&[user("my.server")]).unwrap_err();
    assert!(why.contains("None of the servers of Work"), "{why}");
    assert!(why.contains("letters, digits"), "{why}");
}

#[test]
fn a_local_server_of_the_project_is_named_like_any_other() {
    let p = Place::new();
    let loc = ConnectionId::new(Scope::Local, Some(&p.project()), "loc");
    let plan = p.plan(&[loc]).unwrap();
    assert_eq!(plan.loads, ["loc"]);
    assert!(plan.exact(), "{plan:?}");
}

#[test]
fn an_unknown_project_has_no_line() {
    let p = Place::new();
    let why = plan(&p.paths(), "Work", &[user("a")], "/nowhere/x", false).unwrap_err();
    assert!(why.contains("does not know"), "{why}");
}

#[test]
fn on_windows_the_line_is_for_powershell_and_says_where_to_run_it() {
    let p = Place::new();
    let plan = plan(&p.paths(), "Work", &[user("a")], &p.project(), true).unwrap();
    assert_eq!(
        plan.line,
        r#"claude --% --settings "{\"allowedMcpServers\":[{\"serverName\":\"a\"}]}""#
    );
    assert_eq!(plan.to_do.len(), 1);
    assert!(
        plan.to_do[0].starts_with("Run it in PowerShell, in "),
        "{:?}",
        plan.to_do
    );
    // The arguments are the same whatever the shell.
    assert_eq!(
        plan.args[1],
        r#"{"allowedMcpServers":[{"serverName":"a"}]}"#
    );
}

#[test]
fn the_organisations_servers_load_without_a_rule() {
    let p = Place::new();
    p.write(
        "org/managed-settings.json",
        &json!({"managedMcpServers": {"orgsrv": {"type": "http", "url": "https://org.example/mcp"}}}),
    );
    let plan = p.plan(&[user("a")]).unwrap();
    assert_eq!(plan.loads, ["a"]);
    assert!(
        about(&plan.extra, "orgsrv").contains("organisation's servers load"),
        "{plan:?}"
    );
    // A profile of the organisation's servers alone starts with an empty list: every other server stays out.
    let org = ConnectionId::new(Scope::Managed, None, "orgsrv");
    let plan = p.plan(&[org]).unwrap();
    assert_eq!(plan.args[1], r#"{"allowedMcpServers":[]}"#);
    assert_eq!(plan.loads, ["orgsrv"]);
    assert!(plan.exact(), "{plan:?}");
}

#[test]
fn only_a_line_klefki_could_have_made_is_started() {
    let p = Place::new();
    let good = p.plan(&[user("a")]).unwrap();
    let missing = p.file("no-such-claude");
    for args in [
        vec!["--settings".to_owned()],
        vec!["--mcp-config".to_owned(), good.args[1].clone()],
        vec![
            "--settings".to_owned(),
            r#"{"allowedMcpServers":[{"serverName":"a b"}]}"#.to_owned(),
        ],
        vec![
            "--settings".to_owned(),
            r#"{"allowedMcpServers":[{"serverName":"a"}],"apiKeyHelper":"x"}"#.to_owned(),
        ],
        vec![
            "--settings".to_owned(),
            r#"{"allowedMcpServers":[{"serverCommand":["sh"]}]}"#.to_owned(),
        ],
    ] {
        let plan = StartPlan {
            args: args.clone(),
            ..good.clone()
        };
        assert_eq!(
            run(&plan, &missing).unwrap_err(),
            "the start command is not one Klefki made",
            "{args:?}"
        );
    }
    let elsewhere = StartPlan {
        project: p.file("gone").to_string_lossy().into_owned(),
        ..good.clone()
    };
    assert!(
        run(&elsewhere, &missing)
            .unwrap_err()
            .contains("not a folder")
    );
    // A good line gets as far as looking for the program.
    assert!(!run(&good, &missing).unwrap_err().contains("Klefki made"));
}

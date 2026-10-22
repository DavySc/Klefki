use super::*;
use std::path::PathBuf;

struct Place {
    dir: tempfile::TempDir,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for d in ["cfg", "org", "project/.claude"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        Self { dir }
    }

    fn paths(&self) -> ConfigPaths {
        ConfigPaths {
            managed_dir: Some(self.dir.path().join("org")),
            ..ConfigPaths::in_dir(self.dir.path().join("cfg"))
        }
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    /// Writes `env.MCP_TIMEOUT = value` into the file at `rel`.
    fn set(&self, rel: &str, value: Value) {
        let doc = serde_json::json!({"env": {"MCP_TIMEOUT": value}, "other": true});
        std::fs::write(self.dir.path().join(rel), doc.to_string()).unwrap();
    }

    fn limit(&self, env: Option<&str>) -> TimeLimit {
        let env = env.map(OsString::from);
        limit(Some(&self.paths()), Some(&self.project()), &move |name| {
            (name == "MCP_TIMEOUT").then(|| env.clone()).flatten()
        })
    }
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

#[test]
fn with_nothing_set_it_is_claude_codes_own_30_seconds() {
    let p = Place::new();
    let user = policy::home_as_tilde(&p.dir.path().join("cfg").join("settings.json"));
    assert_eq!(
        p.limit(None),
        TimeLimit {
            limit: secs(30),
            set_in: LimitSetIn::Default,
            unreadable: false,
            change_in: Some(user),
        },
        "changed in the user's own file"
    );
    assert_eq!(
        (
            limit(None, None, &|_| None).limit,
            limit(None, None, &|_| None).change_in
        ),
        (secs(30), None),
        "nothing to read at all"
    );
}

#[test]
fn the_environment_sets_it_in_milliseconds() {
    let p = Place::new();
    let l = p.limit(Some("60000"));
    assert_eq!((l.limit, l.set_in), (secs(60), LimitSetIn::Environment));
    assert!(!l.unreadable);
    assert!(
        l.change_in.unwrap().ends_with("settings.json"),
        "the user's file, which wins over the environment"
    );
}

#[test]
fn each_settings_file_wins_over_the_ones_after_it_and_over_the_environment() {
    let p = Place::new();
    let user = policy::home_as_tilde(&p.dir.path().join("cfg").join("settings.json"));
    p.set("cfg/settings.json", serde_json::json!("40000"));
    assert_eq!(
        (p.limit(Some("60000")).limit, p.limit(Some("60000")).set_in),
        (secs(40), LimitSetIn::File(user)),
        "the user's file beats the environment (measured)"
    );
    p.set("project/.claude/settings.json", serde_json::json!("50000"));
    let project = |name: &str| {
        let file = p.project().join(".claude").join(name);
        LimitSetIn::File(policy::home_as_tilde(&file))
    };
    assert_eq!(p.limit(None).set_in, project("settings.json"));
    assert_eq!(p.limit(None).limit, secs(50));
    p.set(
        "project/.claude/settings.local.json",
        serde_json::json!(70000),
    );
    let l = p.limit(None);
    assert_eq!(
        (l.limit, l.set_in),
        (secs(70), project("settings.local.json")),
        "a number in the file counts as its text"
    );
    assert_eq!(
        p.limit(None).change_in,
        Some(policy::home_as_tilde(
            &p.project().join(".claude").join("settings.local.json")
        )),
        "changed where it is set"
    );
    p.set("org/managed-settings.json", serde_json::json!("90000"));
    let l = p.limit(None);
    assert_eq!(l.limit, secs(90));
    assert!(
        matches!(&l.set_in, LimitSetIn::Organisation(f) if f.ends_with("managed-settings.json")),
        "{l:?}"
    );
    assert_eq!(l.change_in, None, "only the organisation can change it");
}

#[test]
fn a_file_without_it_or_one_that_cannot_be_read_is_passed_over() {
    let p = Place::new();
    std::fs::write(
        p.dir.path().join("cfg/settings.json"),
        r#"{"env": {"OTHER": "1"}}"#,
    )
    .unwrap();
    std::fs::write(
        p.dir.path().join("project/.claude/settings.json"),
        "{ not json",
    )
    .unwrap();
    std::fs::write(
        p.dir.path().join("org/managed-settings.json"),
        r#"{"env": "x"}"#,
    )
    .unwrap();
    let l = p.limit(Some("45000"));
    assert_eq!((l.limit, l.set_in), (secs(45), LimitSetIn::Environment));
}

#[test]
fn values_are_read_as_javascripts_parse_int_reads_them() {
    // Each measured with Claude Code 2.1.308 against a server that answers after 5 s.
    let p = Place::new();
    for (value, want) in [
        ("3000x", Duration::from_millis(3000)),
        ("2.5e3", Duration::from_millis(2)),
        (" 2000", Duration::from_millis(2000)),
        ("60s", Duration::from_millis(60)),
        ("+8000", Duration::from_millis(8000)),
    ] {
        let l = p.limit(Some(value));
        assert_eq!((l.limit, l.unreadable), (want, false), "{value:?}");
    }
    for value in ["abc", "0", "-5", "", "-", " "] {
        let l = p.limit(Some(value));
        assert_eq!(
            (l.limit, &l.set_in, l.unreadable),
            (secs(30), &LimitSetIn::Environment, true),
            "{value:?} leaves Claude Code's own limit"
        );
    }
    assert_eq!(
        p.limit(Some("99999999999999999999999")).limit,
        Duration::from_millis(u64::MAX),
        "a huge number does not overflow"
    );
}

#[test]
fn a_file_that_sets_it_to_nothing_readable_still_decides() {
    let p = Place::new();
    p.set("project/.claude/settings.json", serde_json::json!(null));
    let l = p.limit(Some("60000"));
    assert_eq!(
        (l.limit, l.set_in, l.unreadable),
        (
            secs(30),
            LimitSetIn::File(policy::home_as_tilde(
                &p.project().join(".claude").join("settings.json")
            )),
            true
        )
    );
    p.set("project/.claude/settings.json", serde_json::json!(true));
    assert!(p.limit(Some("60000")).unreadable);
}

#[test]
fn from_the_home_folder_the_users_file_is_named_once_as_the_users() {
    let p = Place::new();
    let cfg = p.dir.path().join("cfg");
    p.set("cfg/settings.json", serde_json::json!("40000"));
    let l = limit(Some(&p.paths()), Some(&cfg), &|_| None);
    assert_eq!(
        l.set_in,
        LimitSetIn::File(policy::home_as_tilde(&cfg.join("settings.json")))
    );
    // The folder's own `.claude` is a project's, even beside the user's.
    p.set("project/.claude/settings.json", serde_json::json!("50000"));
    assert_eq!(p.limit(None).limit, secs(50));
}

#[test]
fn a_folder_that_is_not_a_local_absolute_path_is_not_looked_in() {
    let p = Place::new();
    p.set("project/.claude/settings.json", serde_json::json!("50000"));
    let l = limit(Some(&p.paths()), Some(Path::new("project")), &|_| None);
    assert_eq!(l.set_in, LimitSetIn::Default);
}

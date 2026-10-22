use super::*;
use serde_json::json;

fn unix(definition: &Value) -> Result<InspectorCommand, String> {
    command(definition, None, false)
}

fn line(definition: &Value) -> String {
    unix(definition).expect("a line").text
}

const OPEN: &str = "npx @modelcontextprotocol/inspector";

#[test]
fn a_stdio_server_is_the_program_and_its_arguments_after_two_dashes() {
    let server = json!({"command": "npx", "args": ["-y", "@scope/server", "--read-only"]});
    // The two dashes keep the Inspector from reading `-y` as its own flag.
    assert_eq!(
        line(&server),
        format!("{OPEN} -- npx -y @scope/server --read-only")
    );
    assert_eq!(
        line(&json!({"type": "stdio", "command": "node"})),
        format!("{OPEN} -- node")
    );
}

#[test]
fn a_word_with_anything_a_shell_reads_is_quoted_so_it_stays_one_word() {
    let server = json!({"command": "python", "args": ["my script.py", "it's", "$HOME", "a;b", "", "`x`", "!x", "*"]});
    assert_eq!(
        line(&server),
        format!("{OPEN} -- python 'my script.py' 'it'\\''s' '$HOME' 'a;b' '' '`x`' '!x' '*'")
    );
}

#[test]
fn environment_values_are_never_in_the_line_whatever_they_look_like() {
    let server = json!({"command": "srv", "env": {"LOG_LEVEL": "debug", "TOKEN": "ghp_aaaaaaaaaaaaaaaaaaaaaaaa", "PLAIN": "hunter2"}});
    let got = unix(&server).unwrap();
    for value in ["debug", "ghp_", "hunter2"] {
        assert!(!got.text.contains(value), "{}", got.text);
        assert!(
            got.notes.iter().all(|n| !n.contains(value)),
            "{:?}",
            got.notes
        );
    }
    let note = got
        .notes
        .iter()
        .find(|n| n.contains("environment"))
        .unwrap();
    for name in ["LOG_LEVEL", "TOKEN", "PLAIN"] {
        assert!(note.contains(name), "{note}");
    }
    assert!(
        !unix(&json!({"command": "srv"}))
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("environment"))
    );
}

#[test]
fn an_argument_that_looks_like_a_secret_is_replaced_and_said_so() {
    let server = json!({"command": "srv", "args": ["--api-key", "sk-live-0123456789abcdef", "--token=abcdefgh12345678", "--name", "x"]});
    let got = unix(&server).unwrap();
    assert!(!got.text.contains("sk-live"), "{}", got.text);
    assert!(!got.text.contains("abcdefgh12345678"), "{}", got.text);
    assert!(
        got.text.ends_with(&format!(
            "-- srv --api-key {HIDDEN} --token={HIDDEN} --name x"
        )),
        "{}",
        got.text
    );
    assert!(
        got.notes
            .iter()
            .any(|n| n.contains(HIDDEN) && n.contains("--api-key"))
    );
}

#[test]
fn a_remote_server_is_its_transport_and_its_address() {
    assert_eq!(
        line(&json!({"type": "http", "url": "https://example.com/mcp"})),
        format!("{OPEN} --transport http --server-url https://example.com/mcp")
    );
    assert_eq!(
        line(&json!({"type": "sse", "url": "https://example.com/sse"})),
        format!("{OPEN} --transport sse --server-url https://example.com/sse")
    );
}

#[test]
fn headers_are_named_but_their_values_stay_out() {
    let server = json!({"type": "http", "url": "https://example.com/mcp", "headers": {"Authorization": "Bearer abcdefghijklmnop", "X-Team": "blue"}});
    let got = unix(&server).unwrap();
    assert!(!got.text.contains("abcdefghijklmnop") && !got.text.contains("blue"));
    let note = got.notes.iter().find(|n| n.contains("headers")).unwrap();
    assert!(
        note.contains("Authorization") && note.contains("X-Team"),
        "{note}"
    );
    assert!(
        !note.contains("abcdefghijklmnop") && !note.contains("blue"),
        "{note}"
    );
}

#[test]
fn a_secret_in_the_address_is_replaced_and_the_rest_kept() {
    let got = unix(&json!({"type": "http", "url": "https://user:pw12345678@example.com/mcp?api_key=sk-live-0123456789abcdef&region=eu"})).unwrap();
    for gone in ["pw12345678", "sk-live", "user:"] {
        assert!(!got.text.contains(gone), "{}", got.text);
    }
    assert!(
        got.text.contains("example.com/mcp") && got.text.contains("region=eu"),
        "{}",
        got.text
    );
    assert!(got.text.contains(HIDDEN), "{}", got.text);
    assert!(got.notes.iter().any(|n| n.contains(HIDDEN)));
}

#[test]
fn a_server_that_signs_in_says_the_inspector_does_so_by_itself() {
    let got = unix(
        &json!({"type": "http", "url": "https://example.com/mcp", "oauth": {"clientId": "abc"}}),
    )
    .unwrap();
    assert!(got.notes.iter().any(|n| n.contains("signs in on its own")));
    assert!(!got.text.contains("abc"));
    let plain = unix(&json!({"type": "http", "url": "https://example.com/mcp"})).unwrap();
    assert!(!plain.notes.iter().any(|n| n.contains("signs in")));
}

#[test]
fn what_the_inspector_cannot_open_has_no_line_and_a_reason() {
    for (definition, why) in [
        (
            json!({"type": "ws", "url": "wss://example.com"}),
            "WebSocket",
        ),
        (json!({"type": "carrier-pigeon"}), "type"),
        (json!({"type": "stdio"}), "command"),
        (json!({"command": "  "}), "command"),
        (json!({"type": "http"}), "address"),
        (json!({"type": "http", "url": "not a url"}), "address"),
        (
            json!({"type": "http", "url": "ftp://example.com/x"}),
            "http",
        ),
        (json!("text"), "object"),
    ] {
        let err = unix(&definition).unwrap_err();
        assert!(err.contains(why), "{definition}: {err}");
    }
}

#[test]
fn text_that_could_disguise_the_line_is_refused_not_written() {
    for bad in ["a\nb", "a\rb", "a\tb", "a\u{1b}[31mb", "a\u{202e}b", "a\0b"] {
        for definition in [
            json!({"command": "srv", "args": [bad]}),
            json!({"command": bad}),
            json!({"type": "http", "url": format!("https://example.com/{bad}")}),
        ] {
            // An address with such a character is escaped by the parser, so only stdio words must be refused.
            let result = unix(&definition);
            if definition.get("type").is_none() {
                assert!(result.is_err(), "{bad:?} in {definition}");
            } else if let Ok(got) = result {
                assert!(!got.text.chars().any(|c| c.is_control()), "{:?}", got.text);
            }
        }
    }
}

#[test]
fn a_local_server_starts_where_claude_code_starts_it() {
    let server = json!({"command": "node", "args": ["server.js"]});
    let got = command(&server, Some("/home/u/my proj"), false).unwrap();
    assert_eq!(
        got.text,
        format!("{OPEN} --cwd '/home/u/my proj' -- node server.js")
    );
    // A user server has no folder of its own; a relative path is pointed out.
    let user = unix(&server).unwrap();
    assert!(!user.text.contains("--cwd"));
    assert!(user.notes.iter().any(|n| n.contains("relative")));
    // A folder that is not a plain local path (a network share) is not passed.
    assert!(
        !command(&server, Some("//host/share"), false)
            .unwrap()
            .text
            .contains("--cwd")
    );
    assert!(
        !command(&server, Some("proj"), false)
            .unwrap()
            .text
            .contains("--cwd")
    );
}

#[test]
fn a_reference_to_the_environment_is_kept_literal_and_pointed_out() {
    let server = json!({"command": "srv", "args": ["--dir", "${HOME}/x"]});
    let got = unix(&server).unwrap();
    assert!(got.text.ends_with("--dir '${HOME}/x'"), "{}", got.text);
    assert!(got.notes.iter().any(|n| n.contains("${NAME}")));
    let remote = unix(&json!({"type": "http", "url": "https://${HOST}/mcp"})).unwrap();
    assert!(remote.notes.iter().any(|n| n.contains("${NAME}")));
}

#[test]
fn the_line_always_says_it_is_not_run_and_what_it_needs() {
    let got = unix(&json!({"command": "srv"})).unwrap();
    assert!(got.notes.last().unwrap().contains("Node 22.19"));
    assert!(got.notes.last().unwrap().contains("does not run it"));
}

#[test]
fn notes_are_sanitised() {
    let got = unix(&json!({"command": "srv", "env": {"A\u{1b}[31mB": "x"}})).unwrap();
    assert!(got.notes.iter().all(|n| !n.contains('\u{1b}')));
}

mod windows {
    use super::*;

    fn win(definition: &Value) -> Result<InspectorCommand, String> {
        command(definition, None, true)
    }

    #[test]
    fn a_plain_word_stays_and_others_are_in_double_quotes() {
        let got = win(&json!({"command": "C:\\Program Files\\nodejs\\node.exe", "args": ["-y", "@scope/pkg", "a b", "x,y", ""]})).unwrap();
        assert_eq!(
            got.text,
            format!(
                "{OPEN} -- \"C:\\Program Files\\nodejs\\node.exe\" -y \"@scope/pkg\" \"a b\" \"x,y\" \"\""
            )
        );
    }

    #[test]
    fn backslashes_before_the_closing_quote_are_doubled() {
        let got = win(&json!({"command": "srv", "args": ["C:\\my dir\\"]})).unwrap();
        assert!(got.text.ends_with("\"C:\\my dir\\\\\""), "{}", got.text);
    }

    #[test]
    fn a_word_the_two_shells_read_differently_is_refused() {
        for bad in ["%PATH%", "$env:X", "a\"b", "a`b", "a^b", "${HOME}"] {
            let err = win(&json!({"command": "srv", "args": [bad]})).unwrap_err();
            assert!(err.contains("write the line by hand"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_folder_is_quoted_the_same_way() {
        let got = command(
            &json!({"command": "srv"}),
            Some("C:\\Users\\Ann Lee\\proj"),
            true,
        )
        .unwrap();
        // `is_safe_project_path` follows the system running the tests; on Windows it is kept, elsewhere left out.
        if cfg!(windows) {
            assert!(
                got.text.contains("--cwd \"C:\\Users\\Ann Lee\\proj\" --"),
                "{}",
                got.text
            );
        } else {
            assert!(!got.text.contains("--cwd"), "{}", got.text);
        }
    }
}

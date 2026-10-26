//! Test-only MCP server over stdio for the direct check (`klefki-core`'s `mcp/stdio.rs`). Never shipped.
//!
//! `fake-mcp-server MODE [ARG...]`. Each mode is one way a server behaves, as measured against Claude Code 2.1.310
//! (`docs/proposals/checks-without-claude.md`). Before anything else it writes its argv, folder and environment to
//! the file `FAKE_MCP_DUMP` names, if set, and every message it gets to `FAKE_MCP_LOG`, if set.
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("ok", String::as_str);
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
    if let Some(file) = std::env::var_os("FAKE_MCP_DUMP") {
        let env: serde_json::Map<String, Value> = std::env::vars_os()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), json!(v.to_string_lossy())))
            .collect();
        let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned());
        let dump = json!({"args": args, "cwd": cwd.unwrap_or_default(), "env": env, "pid": std::process::id()});
        std::fs::write(file, dump.to_string()).expect("write the dump");
    }
    match mode {
        "exit" => fail("Error: cannot open database"),
        "needsenv" if std::env::var(arg(1)).ok().as_deref() != Some(arg(2)) => {
            fail(&format!("{} is not set", arg(1)));
        }
        "slow" => std::thread::sleep(Duration::from_millis(arg(1).parse().unwrap_or(0))),
        "junk" => {
            say_raw("Starting server... (not JSON)\n[1, 2]\n{\"jsonrpc\": \"2.0\", \"id\": 0}\n")
        }
        "huge" => {
            let big = "x".repeat(3 << 20);
            say(
                &json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"data": big}}),
            );
        }
        "grandchild" => {
            // A child of its own that keeps running, its id written to the file named.
            let me = std::env::current_exe().expect("own path");
            let mut child = std::process::Command::new(me)
                .arg("sleep")
                .spawn()
                .expect("start the grandchild");
            std::fs::write(arg(1), child.id().to_string()).expect("write the pid");
            std::thread::spawn(move || child.wait());
        }
        "sleep" => {
            std::thread::sleep(Duration::from_secs(300));
            return;
        }
        "stderr" => {
            let mut err = std::io::stderr().lock();
            for line in [
                "starting",
                "token is s3cr3t-token-value",
                "\u{1b}[31mfatal: no config\u{1b}[0m",
            ] {
                let _ = writeln!(err, "{line}");
            }
            std::process::exit(3);
        }
        "floodping" | "floodroots" => {
            // Asks again and again and never reads its input, so the answers fill the pipe.
            let method = if mode == "floodping" {
                "ping"
            } else {
                "roots/list"
            };
            let mut out = std::io::stdout().lock();
            for i in 0..200_000 {
                let _ = writeln!(
                    out,
                    "{}",
                    json!({"jsonrpc": "2.0", "id": i, "method": method})
                );
            }
            let _ = out.flush();
            std::thread::sleep(Duration::from_secs(300));
            return;
        }
        "spam" => {
            for i in 0..2000 {
                say(
                    &json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"i": i}}),
                );
            }
        }
        _ => {}
    }
    serve(mode, &args);
}

fn serve(mode: &str, args: &[String]) {
    let log = std::env::var_os("FAKE_MCP_LOG");
    let stdin = std::io::stdin();
    let mut asked_roots = false;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        if let Some(log) = &log {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .expect("open the log");
            let _ = writeln!(f, "{line}");
        }
        let Ok(m) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = m.get("id").cloned() else {
            continue;
        };
        let Some(method) = m.get("method").and_then(Value::as_str) else {
            // An answer to a request of ours.
            if mode == "roots" && id == json!("r1") {
                let uri = &m["result"]["roots"][0]["uri"];
                if uri.as_str().is_some_and(|u| u.starts_with("file://")) {
                    asked_roots = true;
                }
            }
            continue;
        };
        let answer = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let error = |code: i64, message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
        let modern = mode.starts_with("modern");
        match method {
            "server/discover" => match mode {
                "silentdiscover" => {}
                "emptydiscover" => say(&answer(json!({}))),
                "otherdiscover" => say(&answer(json!({
                    "supportedVersions": ["2099-01-01"], "capabilities": {"tools": {}},
                    "serverInfo": {"name": "m", "version": "1"}}))),
                _ if modern => say(&answer(json!({
                    "supportedVersions": ["2025-11-25", "2026-07-28"],
                    "capabilities": if mode == "modernnotools" { json!({}) } else { json!({"tools": {}}) },
                    "serverInfo": {"name": "m", "version": "1"}}))),
                _ => say(&error(-32601, "Method not found")),
            },
            "initialize" => {
                let asked = m["params"]["protocolVersion"].clone();
                match mode {
                    "hang" => {}
                    "badinit" | "modernonly" => say(&error(-32602, "Unsupported protocol version")),
                    "roots" => {
                        say(&json!({"jsonrpc": "2.0", "id": "r1", "method": "roots/list"}));
                        say(&json!({"jsonrpc": "2.0", "id": "p1", "method": "ping"}));
                        say(&answer(init(asked)));
                    }
                    "oldversion" => say(&answer(init(json!("2024-11-05")))),
                    "weirdversion" => say(&answer(init(json!("1999-01-01")))),
                    "noversion" => say(&answer(
                        json!({"capabilities": {}, "serverInfo": {"name": "x", "version": "1"}}),
                    )),
                    "nocaps" => say(&answer(
                        json!({"protocolVersion": asked, "serverInfo": {"name": "x", "version": "1"}}),
                    )),
                    "noserverinfo" => say(&answer(
                        json!({"protocolVersion": asked, "capabilities": {"tools": {}}}),
                    )),
                    "serverinfonoversion" => say(&answer(json!({
                        "protocolVersion": asked, "capabilities": {"tools": {}}, "serverInfo": {"name": "x"}}))),
                    "notools" => say(&answer(json!({
                        "protocolVersion": asked, "capabilities": {}, "serverInfo": {"name": "x", "version": "1"}}))),
                    "stringid" => {
                        say(
                            &json!({"jsonrpc": "2.0", "id": id.to_string(), "result": init(asked)}),
                        );
                    }
                    "exitafterinit" => {
                        say(&answer(init(asked)));
                        std::process::exit(0);
                    }
                    _ => say(&answer(init(asked))),
                }
            }
            "tools/list" => {
                if modern
                    && m["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
                        != "2026-07-28"
                {
                    say(&error(-32602, "no protocol version"));
                    continue;
                }
                match mode {
                    "toolsfail" | "moderntoolsfail" => say(&error(-32603, "no tools today")),
                    "toolshang" => {}
                    "roots" if !asked_roots => say(&error(-32603, "roots were not answered")),
                    "badtool" => say(&answer(json!({"tools": [{"name": "a"}]}))),
                    "notools" | "modernnotools" => say(&answer(json!({"tools": 5}))),
                    "manytools" => {
                        let tools: Vec<Value> = (0..args.get(1).and_then(|n| n.parse().ok()).unwrap_or(3))
                            .map(|i| json!({"name": format!("t{i}"), "inputSchema": {"type": "object"}}))
                            .collect();
                        say(&answer(json!({"tools": tools})));
                    }
                    _ => say(&answer(json!({"tools": [
                        {"name": "t", "description": "a tool", "inputSchema": {"type": "object"}, "extra": 1}
                    ]}))),
                }
            }
            _ => say(&error(-32601, "Method not found")),
        }
    }
}

fn init(version: Value) -> Value {
    json!({"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}})
}

fn say(message: &Value) {
    say_raw(&format!("{message}\n"));
}

fn say_raw(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn fail(message: &str) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{message}");
    std::process::exit(1);
}

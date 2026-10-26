//! Checks a stdio MCP server by starting it the way Claude Code does and saying hello as Claude Code does, without
//! starting Claude Code (`docs/proposals/checks-without-claude.md`). What to start comes from
//! `config/server_env.rs`; which servers may be started, and when, is the caller's to decide.
//!
//! The conversation, measured with Claude Code 2.1.310:
//! 1. `server/discover`, with the newest protocol in `_meta`. An answer naming `2026-07-28` among its
//!    `supportedVersions`, with `capabilities` and `serverInfo`, means the newer protocol: no `initialize`, and
//!    `tools/list` carries the protocol in `_meta`. Any other answer, or none within [`DISCOVER_WAIT`], falls back to
//! 2. `initialize` (`2025-11-25`), then `notifications/initialized`.
//! 3. `tools/list`, unless the server says it has no tools.
//!
//! Connected means the tools came back (or there are none); "tools failed" means the hello worked but the tools did
//! not. Answers are checked the way Claude Code checks them: an `initialize` answer needs a protocol version Claude
//! Code speaks, `capabilities`, and `serverInfo` with a name and version; each tool a name and an object
//! `inputSchema`. The server may ask for the roots (the folder it runs in) and send `ping`; anything else it asks is
//! refused.
//!
//! Everything the server writes is untrusted: a line longer than [`MAX_LINE`] is skipped, only JSON objects are
//! looked at, and what is kept (a short reason, the end of its error output) is sanitised with the server's own values
//! hidden. Klefki writes nothing anywhere. The check ends the server and everything it started.
use crate::cli::tree;
use crate::config::server_env::Launch;
use crate::diagnose::redact::scrubbed;
use crate::sanitize::{clip, sanitize_line};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

/// The longest line read as a message; a longer one is skipped (measured: Claude Code connects past a 3 MB one).
pub(crate) const MAX_LINE: usize = 1 << 20;
/// How long Claude Code waits for an answer to `server/discover` before it says hello the older way (about 3 s).
pub(crate) const DISCOVER_WAIT: Duration = Duration::from_secs(3);
/// How long a server gets to end by itself once its input is closed, before its tree is ended.
const END_GRACE: Duration = Duration::from_millis(100);
/// How long to wait for the error output once the server is gone.
const PIPE_GRACE: Duration = Duration::from_millis(500);
/// The end of the error output kept, in bytes.
const MAX_STDERR: usize = 16 * 1024;
/// Error output lines kept for the reason, and their length.
const STDERR_LINES: usize = 3;
const MAX_REASON: usize = 300;
/// Messages read ahead of the conversation, at most: each at most [`MAX_LINE`].
const QUEUE: usize = 4;

const MODERN: &str = "2026-07-28";
const LEGACY: &str = "2025-11-25";
/// The protocol versions an `initialize` answer may name (those Claude Code 2.1.310 connects with).
const SPOKEN: [&str; 5] = [
    LEGACY,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
const DISCOVER_ID: &str = "server-discover-probe-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Connected,
    /// The hello worked; listing the tools did not.
    ToolsFailed,
    Failed,
    /// No hello within the time limit.
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub verdict: Verdict,
    /// Why, when not Connected: one short sanitised line.
    pub reason: String,
    /// The protocol the server spoke: [`MODERN`] or the version its `initialize` answer named.
    pub protocol: Option<String>,
    /// Tools on the first page of `tools/list`.
    pub tools: usize,
    /// The last lines of the server's error output, sanitised, its own values hidden.
    pub stderr: String,
    pub took: Duration,
}

/// Starts the server `launch` describes, says hello, lists its tools and ends it. `limit` is Claude Code's time limit
/// (`timeout::limit`), for the hello and again for the tools. `claude_version` is what the hello names as the client.
pub(crate) fn check(launch: &Launch, limit: Duration, claude_version: &str) -> Outcome {
    let started = Instant::now();
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .env_clear()
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&launch.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, tree) = match tree::spawn(&mut cmd, true) {
        Ok(started) => started,
        Err(e) => {
            return Outcome {
                verdict: Verdict::Failed,
                reason: line(&format!("Could not start it: {}", e.kind()), &launch.hidden),
                protocol: None,
                tools: 0,
                stderr: String::new(),
                took: started.elapsed(),
            };
        }
    };
    let messages = child.stdout.take().map(read_messages);
    let stderr = child.stderr.take().map(read_tail);
    let mut talk = Talk {
        input: child.stdin.take(),
        messages,
        roots: roots_answer(launch),
    };
    let mut outcome = talk.run(limit, claude_version, &launch.hidden);

    // Closing its input asks it to end; then everything it started is ended.
    drop(talk);
    let grace = Instant::now() + END_GRACE;
    while !tree::has_ended(&mut child).unwrap_or(true) && Instant::now() < grace {
        std::thread::sleep(Duration::from_millis(5));
    }
    // Not waited for yet, so the tree still has its id.
    tree.kill();
    let _ = child.kill();
    let _ = child.wait();
    if let Some(tail) = stderr.and_then(|rx| rx.recv_timeout(PIPE_GRACE).ok()) {
        outcome.stderr = last_lines(&tail, &launch.hidden);
    }
    outcome.took = started.elapsed();
    outcome
}

/// The conversation with one server.
struct Talk {
    input: Option<ChildStdin>,
    messages: Option<Receiver<Value>>,
    /// The answer to `roots/list`.
    roots: Value,
}

/// How waiting for an answer ended.
enum Wait {
    Answer(serde_json::Map<String, Value>),
    /// The server closed its output (or never had one).
    Closed,
    TimedOut,
}

impl Talk {
    fn run(&mut self, limit: Duration, claude_version: &str, hidden: &[String]) -> Outcome {
        let deadline = Instant::now() + limit;
        let client = client_info(claude_version);
        let capabilities =
            json!({"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}});
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion": MODERN,
            "io.modelcontextprotocol/clientInfo": client,
            "io.modelcontextprotocol/clientCapabilities": capabilities,
        });
        let fail = |verdict, reason: &str, protocol: Option<String>| Outcome {
            verdict,
            reason: line(reason, hidden),
            protocol,
            tools: 0,
            stderr: String::new(),
            took: Duration::ZERO,
        };

        self.send(
            &json!({"jsonrpc": "2.0", "id": DISCOVER_ID, "method": "server/discover",
            "params": {"_meta": meta}}),
        );
        let discover_until = deadline.min(Instant::now() + DISCOVER_WAIT);
        let (tools_id, server_caps, protocol, tools_params) = match self
            .wait(&json!(DISCOVER_ID), discover_until)
        {
            Wait::Answer(a) if modern(&a) => {
                let caps = a
                    .get("result")
                    .map_or(Value::Null, |r| r["capabilities"].clone());
                (0, caps, MODERN.to_owned(), Some(json!({"_meta": meta})))
            }
            Wait::Closed => return fail(Verdict::Failed, "It closed before it answered.", None),
            Wait::TimedOut if Instant::now() >= deadline => {
                return fail(Verdict::TimedOut, &no_answer(limit), None);
            }
            Wait::Answer(_) | Wait::TimedOut => {
                self.send(&json!({"method": "initialize", "params": {
                    "protocolVersion": LEGACY, "capabilities": capabilities, "clientInfo": client},
                    "jsonrpc": "2.0", "id": 0}));
                let answer = match self.wait(&json!(0), deadline) {
                    Wait::Answer(a) => a,
                    Wait::Closed => {
                        return fail(Verdict::Failed, "It closed before it answered.", None);
                    }
                    Wait::TimedOut => return fail(Verdict::TimedOut, &no_answer(limit), None),
                };
                if let Some(error) = answer.get("error") {
                    return fail(
                        Verdict::Failed,
                        &format!("It refused to start: {}", error_text(error)),
                        None,
                    );
                }
                let result = answer.get("result").unwrap_or(&Value::Null);
                let protocol = result["protocolVersion"].as_str().map(str::to_owned);
                if let Err(why) = good_hello(result) {
                    return fail(Verdict::Failed, why, protocol);
                }
                self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
                (
                    1,
                    result["capabilities"].clone(),
                    protocol.unwrap_or_default(),
                    None,
                )
            }
        };

        let connected = |tools| Outcome {
            verdict: Verdict::Connected,
            reason: String::new(),
            protocol: Some(protocol.clone()),
            tools,
            stderr: String::new(),
            took: Duration::ZERO,
        };
        // Measured: Claude Code asks for no tools from a server that says it has none.
        if server_caps.get("tools").is_none() {
            return connected(0);
        }
        let mut request = json!({"method": "tools/list", "jsonrpc": "2.0", "id": tools_id});
        if let Some(params) = tools_params {
            request["params"] = params;
        }
        self.send(&request);
        let tools_failed =
            |reason: &str| fail(Verdict::ToolsFailed, reason, Some(protocol.clone()));
        match self.wait(&json!(tools_id), Instant::now() + limit) {
            Wait::Answer(a) => match (
                a.get("error"),
                good_tools(a.get("result").unwrap_or(&Value::Null)),
            ) {
                (Some(error), _) => {
                    tools_failed(&format!("Listing its tools failed: {}", error_text(error)))
                }
                (None, Ok(count)) => connected(count),
                (None, Err(why)) => tools_failed(why),
            },
            Wait::Closed => tools_failed("It closed before it listed its tools."),
            Wait::TimedOut => tools_failed(&format!(
                "It did not list its tools within {}.",
                seconds(limit)
            )),
        }
    }

    /// Writes one message. A server that stopped reading shows as closed when its answer is waited for.
    fn send(&mut self, message: &Value) {
        let Some(input) = &mut self.input else { return };
        let mut text = message.to_string();
        text.push('\n');
        if input
            .write_all(text.as_bytes())
            .and_then(|()| input.flush())
            .is_err()
        {
            self.input = None;
        }
    }

    /// Waits until `until` for the answer to the request `id`, answering what the server asks in the meantime.
    fn wait(&mut self, id: &Value, until: Instant) -> Wait {
        loop {
            let Some(messages) = &self.messages else {
                return Wait::Closed;
            };
            let left = until.saturating_duration_since(Instant::now());
            let message = match messages.recv_timeout(left) {
                Ok(Value::Object(m)) => m,
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => return Wait::TimedOut,
                Err(RecvTimeoutError::Disconnected) => {
                    self.messages = None;
                    return Wait::Closed;
                }
            };
            match message.get("method").and_then(Value::as_str) {
                Some(method) => {
                    if let Some(asked) = message.get("id") {
                        let reply = self.reply(method, asked.clone());
                        self.send(&reply);
                    }
                }
                None if message.get("id").is_some_and(|got| same_id(got, id)) => {
                    return Wait::Answer(message);
                }
                None => {}
            }
        }
    }

    /// The answer to a request from the server.
    fn reply(&self, method: &str, id: Value) -> Value {
        match method {
            "ping" => json!({"result": {}, "jsonrpc": "2.0", "id": id}),
            "roots/list" => json!({"result": self.roots, "jsonrpc": "2.0", "id": id}),
            _ => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
            }
        }
    }
}

/// The client Claude Code names in its hello.
fn client_info(version: &str) -> Value {
    json!({"name": "claude-code", "title": "Claude Code", "version": version,
        "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"})
}

/// The roots Claude Code gives: the folder the server runs in.
fn roots_answer(launch: &Launch) -> Value {
    let roots: Vec<Value> = url::Url::from_file_path(&launch.cwd)
        .map(|u| json!({"uri": u.as_str()}))
        .into_iter()
        .collect();
    json!({"roots": roots})
}

/// Whether a `server/discover` answer means the newer protocol.
fn modern(answer: &serde_json::Map<String, Value>) -> bool {
    let result = answer.get("result").unwrap_or(&Value::Null);
    result["supportedVersions"]
        .as_array()
        .is_some_and(|v| v.iter().any(|v| v == MODERN))
        && result["capabilities"].is_object()
        && result["serverInfo"].is_object()
}

/// An `initialize` answer Claude Code accepts.
fn good_hello(result: &Value) -> Result<(), &'static str> {
    let Some(version) = result["protocolVersion"].as_str() else {
        return Err("Its answer to the hello names no protocol version.");
    };
    if !SPOKEN.contains(&version) {
        return Err("It speaks a protocol version Claude Code does not.");
    }
    if !result["capabilities"].is_object() {
        return Err("Its answer to the hello has no capabilities.");
    }
    let info = &result["serverInfo"];
    if !(info["name"].is_string() && info["version"].is_string()) {
        return Err("Its answer to the hello does not name the server and its version.");
    }
    Ok(())
}

/// The number of tools in a `tools/list` result Claude Code accepts.
fn good_tools(result: &Value) -> Result<usize, &'static str> {
    let Some(tools) = result["tools"].as_array() else {
        return Err("Its list of tools is not a list.");
    };
    let text_or_absent = |v: &Value| v.is_null() || v.is_string();
    let schema = |v: &Value| {
        v["type"] == "object"
            && (v["properties"].is_null() || v["properties"].is_object())
            && (v["required"].is_null()
                || v["required"]
                    .as_array()
                    .is_some_and(|r| r.iter().all(Value::is_string)))
    };
    let good = |tool: &Value| {
        tool["name"].is_string()
            && text_or_absent(&tool["title"])
            && text_or_absent(&tool["description"])
            && tool.get("inputSchema").is_some_and(schema)
            && tool.get("outputSchema").is_none_or(schema)
            && (tool["annotations"].is_null() || tool["annotations"].is_object())
    };
    if tools.iter().all(good) {
        Ok(tools.len())
    } else {
        Err("A tool in its list is not described the way MCP asks.")
    }
}

/// Whether the answer's `got` is the request's `id`. A number may come back as text (measured: Claude Code accepts
/// `"1"` for `1`).
fn same_id(got: &Value, id: &Value) -> bool {
    got == id
        || (id.is_number()
            && got
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|n| id.as_u64() == Some(n)))
}

fn error_text(error: &Value) -> String {
    match (error["message"].as_str(), error["code"].as_i64()) {
        (Some(message), Some(code)) => format!("{message} ({code})"),
        (Some(message), None) => message.to_owned(),
        (None, Some(code)) => format!("error {code}"),
        (None, None) => "no reason given".to_owned(),
    }
}

fn no_answer(limit: Duration) -> String {
    format!("It did not answer within {}.", seconds(limit))
}

fn seconds(limit: Duration) -> String {
    let ms = limit.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", limit.as_secs_f64())
    }
}

/// Untrusted text as one short line, the server's own values hidden.
fn line(text: &str, hidden: &[String]) -> String {
    clip(sanitize_line(&scrubbed(text, hidden)).trim(), MAX_REASON)
}

/// The last few lines of the error output that say something.
fn last_lines(tail: &[u8], hidden: &[String]) -> String {
    let text = scrubbed(&String::from_utf8_lossy(tail), hidden);
    let lines: Vec<String> = text
        .lines()
        .map(|l| clip(sanitize_line(l).trim(), MAX_REASON))
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

/// Reads the server's output on a thread of its own: each line that is a JSON value, at most [`MAX_LINE`] long, in
/// order. The channel closes when the output does.
fn read_messages<R: Read + Send + 'static>(out: R) -> Receiver<Value> {
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    std::thread::spawn(move || read_lines(BufReader::new(out), &tx));
    rx
}

fn read_lines<R: BufRead>(mut reader: R, tx: &SyncSender<Value>) {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let (used, done, ended) = match reader.fill_buf() {
            Ok([]) => return,
            Ok(buf) => match memchr::memchr(b'\n', buf) {
                Some(at) => (at + 1, true, &buf[..at]),
                None => (buf.len(), false, buf),
            },
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        if !too_long {
            if line.len() + ended.len() > MAX_LINE {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(ended);
            }
        }
        reader.consume(used);
        if done {
            if !too_long
                && let Ok(message) = serde_json::from_slice::<Value>(&line)
                && tx.send(message).is_err()
            {
                return;
            }
            line.clear();
            too_long = false;
        }
    }
}

/// Reads the error output on a thread of its own, keeping its last [`MAX_STDERR`] bytes.
fn read_tail<R: Read + Send + 'static>(mut err: R) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut tail = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match err.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > 2 * MAX_STDERR {
                        tail.drain(..tail.len() - MAX_STDERR);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if tail.len() > MAX_STDERR {
            tail.drain(..tail.len() - MAX_STDERR);
        }
        let _ = tx.send(tail);
    });
    rx
}

#[cfg(test)]
mod tests;

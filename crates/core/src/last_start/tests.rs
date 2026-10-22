use super::*;
use crate::engine::Focus;

const FAIL: &str = include_str!("../../../../fixtures/mcp-logs/fail.jsonl");
const OK: &str = include_str!("../../../../fixtures/mcp-logs/ok.jsonl");
const HANG: &str = include_str!("../../../../fixtures/mcp-logs/hang.jsonl");
const NOPE: &str = include_str!("../../../../fixtures/mcp-logs/nope.jsonl");
const REMOTE: &str = include_str!("../../../../fixtures/mcp-logs/remote.jsonl");
/// The folder the fixtures were written in.
const PROJ: &str = "/home/u/proj";

/// 2026-10-22T12:00:00Z: the fixtures are from the same morning.
const NOW: u64 = 1_792_670_400_000;

struct Logs {
    dir: tempfile::TempDir,
}

impl Logs {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Writes `text` as the log of `server` started in `folder` at `time` (the file name's form), with `cwd`
    /// in each line changed from the fixtures' folder to `folder`.
    fn put(&self, folder: &str, server: &str, time: &str, text: &str) -> PathBuf {
        let dir = self
            .root()
            .join(folder_name(folder))
            .join(format!("mcp-logs-{}", folder_name(server)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{time}.jsonl"));
        let cwd = serde_json::to_string(folder).unwrap();
        std::fs::write(&path, text.replace("\"/home/u/proj\"", &cwd)).unwrap();
        path
    }

    fn read(&self, name: &str) -> LastStarts {
        self.ask(name, None, &[], &[])
    }

    fn ask(
        &self,
        name: &str,
        within: Option<&str>,
        elsewhere: &[String],
        secrets: &[String],
    ) -> LastStarts {
        read(&Ask {
            root: self.root(),
            name,
            within,
            elsewhere,
            home: Some(Path::new("/home/u")),
            secrets,
            now_ms: NOW,
            offset: &|_| 0,
        })
    }
}

const T1: &str = "2026-10-22T07-32-16-377Z";
const T2: &str = "2026-10-22T08-00-00-000Z";
const T3: &str = "2026-10-22T09-00-00-000Z";
const T4: &str = "2026-10-22T10-00-00-000Z";

#[test]
fn folder_names_are_written_as_claude_code_writes_them() {
    // Expected values from Claude Code 2.1.308's own function, run in Node.
    assert_eq!(folder_name("/home/u/proj"), "-home-u-proj");
    assert_eq!(folder_name("C:\\Users\\Ana\\proj"), "C--Users-Ana-proj");
    assert_eq!(folder_name("/home/u/über 😀"), "-home-u--ber---");
    assert_eq!(folder_name("plugin:p1:srv"), "plugin-p1-srv");
    assert_eq!(folder_name("claude.ai Gmail"), "claude-ai-Gmail");
    assert_eq!(folder_name("my server.x"), "my-server-x");
    let long = format!("/home/u/{}", "x".repeat(250));
    assert_eq!(
        folder_name(&long),
        format!("-home-u-{}-47iwxx", "x".repeat(192))
    );
    let long = "/".to_owned() + &"ab/".repeat(80);
    assert!(
        folder_name(&long).ends_with("-a-ot5dch"),
        "{}",
        folder_name(&long)
    );
    assert_eq!(folder_name(&long).len(), 200 + 7);
    // Exactly at the cut: no hash.
    let at = "a".repeat(200);
    assert_eq!(folder_name(&at), at);
    assert_eq!(base36(0), "0");
    assert_eq!(base36(u32::MAX), "1z141z3");
}

#[test]
fn a_server_that_exits_says_why_with_what_it_printed() {
    let logs = Logs::new();
    logs.put(PROJ, "fail", T1, FAIL);
    let got = logs.read("fail");
    assert_eq!(got.problem, None);
    assert_eq!(got.starts.len(), 1);
    let s = &got.starts[0];
    assert_eq!(s.folder, "~/proj");
    assert_eq!(s.when, "Today 07:32");
    assert_eq!(s.exact, "22 Oct 2026, 07:32:16 (UTC+00:00)");
    assert_eq!(
        s.outcome,
        StartOutcome::Failed {
            code: Some("CONNECTION_CLOSED".into()),
            said: "Connection closed".into(),
            after: Some(Duration::from_millis(388)),
        }
    );
    assert_eq!(
        s.server_said,
        [
            "starting fail server, token=***",
            "Error: Cannot find module 'foo'",
            "at Module._resolve (node:internal)",
        ]
    );
    let titles: Vec<&str> = s.hints.iter().map(|h| h.title.as_str()).collect();
    assert!(
        titles.contains(&crate::diagnose::hints::HintKind::MissingFile.title()),
        "{titles:?}"
    );
    assert!(!s.partly);
    assert!(
        s.file.starts_with(&shown_path(logs.root(), None)),
        "{}",
        s.file
    );
}

#[test]
fn a_server_that_connected_says_how_fast_and_no_hint() {
    let logs = Logs::new();
    logs.put(PROJ, "ok", T1, OK);
    let s = &logs.read("ok").starts[0];
    assert_eq!(
        s.outcome,
        StartOutcome::Connected {
            after: Some(Duration::from_millis(62))
        }
    );
    assert_eq!(s.server_said, ["ok server up"]);
    assert!(s.hints.is_empty());
}

#[test]
fn timeouts_missing_programs_and_remote_failures_are_named() {
    let logs = Logs::new();
    logs.put(PROJ, "hang", T1, HANG);
    logs.put(PROJ, "nope", T1, NOPE);
    logs.put(PROJ, "remote", T1, REMOTE);
    let hang = &logs.read("hang").starts[0];
    assert!(
        matches!(&hang.outcome, StartOutcome::Failed { code: Some(c), after: Some(a), .. }
            if c == "CONNECT_TIMEOUT" && *a == Duration::from_millis(3027)),
        "{:?}",
        hang.outcome
    );
    assert_eq!(hang.server_said, ["hanging"]);
    assert_eq!(
        hang.hints[0].title,
        crate::diagnose::hints::HintKind::TimedOut.title()
    );

    let nope = &logs.read("nope").starts[0];
    assert!(
        matches!(&nope.outcome, StartOutcome::Failed { code: Some(c), said, .. }
            if c == "ENOENT" && said.contains("does-not-exist-xyz")),
        "{:?}",
        nope.outcome
    );
    assert_eq!(
        nope.hints[0].title,
        crate::diagnose::hints::HintKind::CommandNotFound.title()
    );
    assert_eq!(
        nope.hints[0].points_at.as_ref().map(|p| p.0),
        Some(Focus::Command)
    );

    let remote = &logs.read("remote").starts[0];
    assert!(
        matches!(&remote.outcome, StartOutcome::Failed { code: Some(c), .. } if c == "ECONNREFUSED"),
        "{:?}",
        remote.outcome
    );
    assert_eq!(
        remote.hints[0].title,
        crate::diagnose::hints::HintKind::ConnectionRefused.title()
    );
    // Claude Code's own lines (headers, the stack of its error) are not the server's words.
    assert!(remote.server_said.is_empty(), "{:?}", remote.server_said);
}

#[test]
fn the_newest_start_in_each_folder_newest_first_and_at_most_three() {
    let logs = Logs::new();
    logs.put("/home/u/a", "s", T1, FAIL);
    logs.put("/home/u/a", "s", T3, OK);
    logs.put("/home/u/b", "s", T2, FAIL);
    logs.put("/home/u", "s", T4, OK);
    logs.put("/home/u/c", "s", T1, OK);
    logs.put("/home/u/d", "other", T4, FAIL);
    let got = logs.read("s");
    let folders: Vec<&str> = got.starts.iter().map(|s| s.folder.as_str()).collect();
    assert_eq!(folders, ["~", "~/a", "~/b"]);
    assert!(matches!(
        got.starts[1].outcome,
        StartOutcome::Connected { .. }
    ));
    assert!(matches!(got.starts[2].outcome, StartOutcome::Failed { .. }));
}

#[test]
fn a_local_server_only_counts_starts_in_its_project_and_below() {
    let logs = Logs::new();
    logs.put("/home/u/app", "s", T1, FAIL);
    logs.put("/home/u/app/sub", "s", T2, FAIL);
    // Its name starts like the project's, but it is another folder.
    logs.put("/home/u/app-old", "s", T3, OK);
    logs.put("/home/u/other", "s", T4, OK);
    let got = logs.ask("s", Some("/home/u/app"), &[], &[]);
    let folders: Vec<&str> = got.starts.iter().map(|s| s.folder.as_str()).collect();
    assert_eq!(folders, ["~/app/sub", "~/app"]);
    // A project key as the file spells it on Windows is compared with Claude Code's own spelling.
    assert!(belongs(
        Some("C:\\Users\\Ana\\app\\sub"),
        &Ask {
            within: Some("C:/Users/Ana/app"),
            ..ask_for(logs.root())
        }
    ));
}

#[test]
fn a_user_server_leaves_out_projects_where_another_of_its_name_loads() {
    let logs = Logs::new();
    logs.put("/home/u/app", "s", T3, FAIL);
    logs.put("/home/u/app/sub", "s", T4, FAIL);
    logs.put("/home/u/b", "s", T1, OK);
    let got = logs.ask("s", None, &["/home/u/app".to_owned()], &[]);
    let folders: Vec<&str> = got.starts.iter().map(|s| s.folder.as_str()).collect();
    assert_eq!(folders, ["~/b"]);
}

fn ask_for(root: &Path) -> Ask<'_> {
    Ask {
        root,
        name: "s",
        within: None,
        elsewhere: &[],
        home: None,
        secrets: &[],
        now_ms: NOW,
        offset: &|_| 0,
    }
}

#[test]
fn the_servers_own_values_and_secret_shapes_are_hidden() {
    let logs = Logs::new();
    let line = |text: &str, error: bool| {
        let key = if error { "error" } else { "debug" };
        serde_json::json!({key: text, "timestamp": "2026-10-22T07:32:16.385Z", "cwd": PROJ})
            .to_string()
    };
    let text = [
        line("Starting connection with timeout of 30000ms", false),
        line(
            "Server stderr: using key hunter2-long-value for db in mode xyz12\nAuthorization: Bearer abcdefghijklmnop\nkey sk-ant-api03-abcdefghijklmnopqrstuvwxyz\n\u{1b}[31mred\u{202e}txt",
            true,
        ),
        line(
            "Connection failed (CONNECTION_CLOSED): bad password hunter2-long-value",
            true,
        ),
    ]
    .join("\n");
    logs.put(PROJ, "s", T1, &text);
    let got = logs.ask(
        "s",
        None,
        &[],
        &["hunter2-long-value".to_owned(), "xyz12".to_owned()],
    );
    let s = &got.starts[0];
    let all = format!("{:?}", s);
    for secret in [
        "hunter2-long-value",
        "abcdefghijklmnop",
        "abcdefghijklmnopqrstuvwxyz",
        "\u{1b}",
        "\u{202e}",
    ] {
        assert!(!all.contains(secret), "{secret:?} in {all}");
    }
    assert_eq!(s.server_said[0], "using key *** for db in mode xyz12");
    assert_eq!(s.server_said[3], "redtxt");
    // A short value is too common to hide.
}

#[test]
fn only_the_lines_klefki_knows_are_shown() {
    // A session's tool calls are logged with what the tool returned; none of it is shown.
    let logs = Logs::new();
    let text = format!(
        "{OK}{}\n{}\n",
        serde_json::json!({"debug": "Calling MCP tool: read", "timestamp": "2026-10-22T07:40:00.000Z", "cwd": PROJ}),
        serde_json::json!({"debug": "Tool 'read' failed after 0s: the file says PRIVATE-TEXT", "timestamp": "2026-10-22T07:40:00.000Z", "cwd": PROJ}),
    );
    logs.put(PROJ, "s", T1, &text);
    let all = format!("{:?}", logs.read("s"));
    assert!(!all.contains("PRIVATE-TEXT"));
    assert!(!all.contains("Calling MCP tool"));
}

#[test]
fn a_later_start_in_the_same_run_counts() {
    // A session reconnects: the last start decides, and the earlier one's output is not mixed in.
    let logs = Logs::new();
    logs.put(PROJ, "s", T1, &format!("{FAIL}{OK}"));
    let s = &logs.read("s").starts[0];
    assert!(
        matches!(s.outcome, StartOutcome::Connected { .. }),
        "{:?}",
        s.outcome
    );
    assert_eq!(s.server_said, ["ok server up"]);
}

#[test]
fn a_huge_log_is_read_at_its_two_ends_only() {
    // A long session: the start at the top, megabytes of tool calls, a reconnect that failed at the end.
    let logs = Logs::new();
    let path = logs.put(PROJ, "s", T1, OK);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let filler = serde_json::json!({"debug": "Calling MCP tool: x", "timestamp": "2026-10-22T07:40:00.000Z", "cwd": PROJ}).to_string() + "\n";
        let block = filler.repeat(4096);
        while f.metadata().unwrap().len() < 8 * 1024 * 1024 {
            f.write_all(block.as_bytes()).unwrap();
        }
        f.write_all(FAIL.as_bytes()).unwrap();
    }
    let s = &logs.read("s").starts[0];
    assert!(s.partly);
    assert!(
        matches!(s.outcome, StartOutcome::Failed { .. }),
        "{:?}",
        s.outcome
    );
}

/// Counts what is read.
struct Counting<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn a_gigabyte_log_costs_the_same_as_a_small_one() {
    // PLAN's log-tail criterion: memory stays flat whatever the size. A sparse 1 GB file (no disk used where the
    // system allows it); what is read, and so held, is two ends.
    let logs = Logs::new();
    let path = logs.put(PROJ, "s", T1, OK);
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(1 << 30).unwrap();
    drop(f);
    let mut counted = Counting {
        inner: File::open(&path).unwrap(),
        read: 0,
    };
    let (head, tail) = ends(&mut counted, 1 << 30).unwrap();
    assert_eq!(counted.read, 2 * END);
    assert_eq!(
        head.len() as u64 + tail.map_or(0, |t| t.len() as u64),
        2 * END
    );
    let started = std::time::Instant::now();
    let got = logs.read("s");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // The start is at the top; the end of the file is zeros, not a line.
    assert!(matches!(
        got.starts[0].outcome,
        StartOutcome::Connected { .. }
    ));
    assert!(got.starts[0].partly);
}

#[test]
fn one_huge_line_or_bytes_that_are_not_text_are_skipped() {
    let logs = Logs::new();
    logs.put("/home/u/a", "s", T1, &"x".repeat(3 * END as usize));
    let mut bad = OK.as_bytes().to_vec();
    bad.extend_from_slice(b"\xff\xfe{not json\n\n[1,2]\n{\"debug\":42}\n");
    let path = logs.put("/home/u/b", "s", T2, "");
    std::fs::write(&path, &bad).unwrap();
    let got = logs.read("s");
    assert_eq!(got.starts.len(), 2);
    assert!(matches!(
        got.starts[0].outcome,
        StartOutcome::Connected { .. }
    ));
    // Nothing known in it: no folder to tell, and the time from the file's name.
    assert_eq!(got.starts[1].outcome, StartOutcome::Unknown);
    assert_eq!(got.starts[1].folder, "an unknown folder");
    assert_eq!(got.starts[1].when, "Today 07:32");
}

#[test]
fn a_file_cut_short_or_growing_while_read_is_read_as_far_as_it_goes() {
    // The size was looked at before the writer cut it (or Claude Code removed old logs).
    let mut short = io::Cursor::new(b"{\"debug\":\"x\"}\n".to_vec());
    let (head, tail) = ends(&mut short, 10 * END).unwrap();
    assert_eq!(head.len(), 14);
    assert_eq!(tail, Some(Vec::new()));
    // It grew after: what was there when looked at, at most.
    let mut grown = io::Cursor::new(vec![b'a'; 3 * END as usize]);
    let (head, tail) = ends(&mut grown, 100).unwrap();
    assert_eq!(head.len() as u64, 2 * END);
    assert_eq!(tail, None);
}

#[test]
fn a_log_still_open_for_writing_is_read() {
    let logs = Logs::new();
    let path = logs.put(PROJ, "s", T1, "");
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    use std::io::Write;
    // The last line is half written.
    writer.write_all(OK.as_bytes()).unwrap();
    writer.write_all(b"{\"debug\":\"Sending SIG").unwrap();
    writer.flush().unwrap();
    let got = logs.read("s");
    assert!(matches!(
        got.starts[0].outcome,
        StartOutcome::Connected { .. }
    ));
    drop(writer);
}

#[test]
fn links_folders_and_other_names_are_not_logs() {
    let logs = Logs::new();
    let real = logs.put("/home/u/a", "s", T1, OK);
    let dir = real.parent().unwrap();
    std::fs::create_dir(dir.join(format!("{T4}.jsonl"))).unwrap();
    std::fs::write(dir.join("2026-10-22T11-00-00-000Z.jsonl.bak"), FAIL).unwrap();
    std::fs::write(dir.join("zzzz.jsonl"), FAIL).unwrap();
    assert_eq!(
        newest_file(dir).as_deref(),
        Some(format!("{T1}.jsonl").as_str())
    );
    #[cfg(unix)]
    {
        let other = logs.root().join("elsewhere.jsonl");
        std::fs::write(&other, FAIL).unwrap();
        std::os::unix::fs::symlink(&other, dir.join(format!("{T3}.jsonl"))).unwrap();
        // A pipe would make reading wait for a writer.
        let pipe = dir.join(format!("{T2}.jsonl"));
        let ok = std::process::Command::new("mkfifo").arg(&pipe).status();
        assert!(ok.unwrap().success());
        assert_eq!(
            newest_file(dir).as_deref(),
            Some(format!("{T1}.jsonl").as_str())
        );
        assert!(open(&pipe).is_err());
        assert!(open(&dir.join(format!("{T3}.jsonl"))).is_err());
        // A linked log folder is not followed either.
        let b = logs.root().join(folder_name("/home/u/b"));
        std::fs::create_dir(&b).unwrap();
        std::os::unix::fs::symlink(dir, b.join("mcp-logs-s")).unwrap();
    }
    let got = logs.read("s");
    assert_eq!(got.starts.len(), 1);
    assert!(matches!(
        got.starts[0].outcome,
        StartOutcome::Connected { .. }
    ));
}

#[test]
fn log_file_names_are_checked_character_by_character() {
    assert!(is_log_name("2026-10-22T07-32-16-377Z.jsonl"));
    for bad in [
        "2026-10-22T07-32-16-377Z.json",
        "2026-10-22T07:32:16-377Z.jsonl",
        "2026-10-22X07-32-16-377Z.jsonl",
        "2026-10-22T07-32-16-37aZ.jsonl",
        "12026-10-22T07-32-16-377Z.jsonl",
        "2026-10-22T07-32-16-377.jsonl",
    ] {
        assert!(!is_log_name(bad), "{bad}");
    }
    assert_eq!(
        name_ms("2026-10-22T07-32-16-377Z.jsonl"),
        Some(1_792_654_336_377)
    );
    assert_eq!(
        timestamp_ms("2026-10-22T07:32:16.377Z"),
        Some(1_792_654_336_377)
    );
    assert_eq!(
        timestamp_ms("2026-10-22T07:32:16Z"),
        Some(1_792_654_336_000)
    );
    for bad in [
        "",
        "2026-13-22T07:32:16.377Z",
        "2026-10-22 07:32:16.377Z",
        "yesterday at noon",
        // Each separator is checked on its own.
        "2026x10-22T07:32:16.377Z",
        "2026-10x22T07:32:16.377Z",
        "2026-10-22T07x32:16.377Z",
        "2026-10-22T07:32x16.377Z",
        // And each field's range, just past its end.
        "2026-10-22T24:00:00.000Z",
        "2026-10-22T23:60:00.000Z",
        "2026-10-22T23:59:61.000Z",
    ] {
        assert_eq!(timestamp_ms(bad), None, "{bad}");
    }
    // The last moment of each range counts (60 for a leap second).
    assert!(timestamp_ms("2026-10-22T23:59:60.000Z").is_some());
    assert_eq!(
        to_ms(2026, 10, 22, 23, 59, 59, 999),
        Some(1_792_713_599_999)
    );
}

#[test]
fn a_log_up_to_twice_the_piece_is_read_whole() {
    let mut text = vec![b'x'; usize::try_from(2 * END).unwrap()];
    let (head, tail) = ends(&mut std::io::Cursor::new(&text), 2 * END).unwrap();
    assert_eq!((head.len(), tail), (text.len(), None));
    text.push(b'y');
    let (head, tail) = ends(&mut std::io::Cursor::new(&text), 2 * END + 1).unwrap();
    assert_eq!(head.len() as u64, END);
    assert_eq!(tail.unwrap().last(), Some(&b'y'));
}

#[test]
fn a_log_without_folders_in_it_counts_by_its_folders_name() {
    // A log that names no folder (lines cut, or an older Claude Code): only its folder's name tells the project.
    let logs = Logs::new();
    let bare = FAIL.replace(",\"cwd\":\"/home/u/proj\"", "");
    assert!(!bare.contains("cwd"));
    logs.put("/home/u/app", "s", T1, &bare);
    logs.put("/home/u/other", "s", T2, &bare);
    let got = logs.ask("s", Some("/home/u/app"), &[], &[]);
    assert_eq!(got.starts.len(), 1, "{got:?}");
    assert_eq!(got.starts[0].folder, "an unknown folder");
    assert!(got.starts[0].file.contains("-home-u-app"));
}

#[test]
fn no_logs_is_no_starts_and_no_problem() {
    let logs = Logs::new();
    let got = read(&Ask {
        root: &logs.root().join("missing"),
        ..ask_for(logs.root())
    });
    assert_eq!(got.starts, []);
    assert_eq!(got.problem, None);
    assert!(got.looked_in.unwrap().ends_with("missing"));
    // A file where the folder should be cannot be listed.
    let file = logs.root().join("file");
    std::fs::write(&file, "x").unwrap();
    let got = read(&Ask {
        root: &file,
        ..ask_for(logs.root())
    });
    assert!(got.problem.is_some());
}

#[test]
fn the_log_folder_is_where_claude_code_keeps_it() {
    let cache = Path::new("/c");
    if cfg!(windows) {
        assert_eq!(root(cache), Path::new("/c/claude-cli-nodejs/Cache"));
    } else {
        assert_eq!(root(cache), Path::new("/c/claude-cli-nodejs"));
    }
}

#[test]
fn folders_under_home_keep_their_own_separator() {
    // Found on Windows CI: a folder logged as `/home/u/a` was shown as `~\a`.
    let home = Some(Path::new("/home/u"));
    assert_eq!(shown_path(Path::new("/home/u/a/b"), home), "~/a/b");
    assert_eq!(shown_path(Path::new("/home/u"), home), "~");
    assert_eq!(shown_path(Path::new("/elsewhere/a"), home), "/elsewhere/a");
    #[cfg(windows)]
    {
        let home = Some(Path::new("C:\\Users\\Ana"));
        assert_eq!(
            shown_path(Path::new("C:\\Users\\Ana\\code\\app"), home),
            "~\\code\\app"
        );
    }
}

/// One log line as Claude Code writes it, in the fixtures' folder.
fn line(text: &str, error: bool) -> String {
    let key = if error { "error" } else { "debug" };
    serde_json::json!({key: text, "timestamp": "2026-10-22T07:32:16.385Z", "cwd": PROJ}).to_string()
}

#[test]
fn a_header_token_is_hidden_without_its_scheme_too() {
    // Review B1: the server prints the token alone, not "Bearer <token>" as the header holds it.
    let logs = Logs::new();
    let text = [
        line("Starting connection with timeout of 30000ms", false),
        line("Server stderr: login with t0kenvalue42xyz failed", true),
        line("Server stderr: basic dXNlcjpwYXNzd29yZA== refused", true),
        line(
            "Connection failed (CONNECTION_CLOSED): t0kenvalue42xyz rejected",
            true,
        ),
    ]
    .join("\n");
    logs.put(PROJ, "s", T1, &text);
    let secrets = [
        "Bearer t0kenvalue42xyz".to_owned(),
        // The nearby variant: another scheme.
        "Basic dXNlcjpwYXNzd29yZA==".to_owned(),
        // A short last word is too common to hide.
        "Token ab12".to_owned(),
    ];
    let got = logs.ask("s", None, &[], &secrets);
    let s = &got.starts[0];
    let all = format!("{s:?}");
    assert!(!all.contains("t0kenvalue42xyz"), "{all}");
    assert!(!all.contains("dXNlcjpwYXNzd29yZA"), "{all}");
    assert_eq!(
        s.server_said,
        ["login with *** failed", "basic *** refused"]
    );
    // The scheme word itself is not a secret: "Bearer" elsewhere stays.
    assert_eq!(
        scrub("Bearer realm=x, ab12", &secrets),
        "Bearer realm=x, ab12"
    );
}

#[test]
fn a_start_that_connected_shows_only_what_was_printed_on_the_way_up() {
    // Review B2: what a server prints while it serves tool calls is not part of its start.
    let logs = Logs::new();
    let chatter = line(
        "Server stderr: handled read_file: PRIVATE-NOTES contents",
        true,
    );
    logs.put(PROJ, "s", T1, &format!("{OK}{chatter}\n"));
    let s = &logs.read("s").starts[0];
    assert_eq!(s.server_said, ["ok server up"]);

    // The nearby variant: the chatter at the end of a log too big to read whole, so it is in the tail.
    let filler = line("Calling MCP tool: read", false);
    let mut text = OK.to_owned();
    while text.len() < 3 * END as usize {
        text.push_str(&filler);
        text.push('\n');
    }
    text.push_str(&chatter);
    text.push('\n');
    logs.put(PROJ, "big", T1, &text);
    let s = &logs.read("big").starts[0];
    assert!(s.partly);
    assert_eq!(s.server_said, ["ok server up"]);
}

#[test]
fn a_projects_folder_name_is_compared_in_any_case_on_windows() {
    // Review B3: a session started in `c:\users\ana\app` writes `c--users-ana-app`.
    let project = folder_name("C:\\Users\\Ana\\app");
    assert!(starts_like("c--users-ana-app-sub", &project, true));
    assert!(starts_like("C--Users-Ana-app", &project, true));
    // Elsewhere a folder's case is its name.
    assert!(!starts_like("c--users-ana-app", &project, false));
    assert!(starts_like("C--Users-Ana-app", &project, false));
    // The nearby variant: another folder is still another folder in any case.
    assert!(!starts_like("c--users-bob-app", &project, true));
}

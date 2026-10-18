//! Signing in to a server: `claude mcp login <name> --no-browser` in a pseudo-terminal (decision 005).
//!
//! Claude listens on a `localhost` port for the browser's callback and prints the sign-in link. Klefki opens that
//! link only after checking it, and waits. If the browser cannot reach the callback, the user pastes the address it
//! ends up on; Klefki checks that too before typing it into the terminal, and types nothing else.
use super::locate;
use crate::model::ServerName;
use crate::sanitize::{clip, sanitize};
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// The longest link or pasted address accepted. Real ones are a few hundred characters.
const MAX_ADDRESS: usize = 8 * 1024;
/// Output kept for reading. The link comes in the first kilobyte and the result in the last line.
const MAX_OUTPUT: usize = 64 * 1024;
/// How often the process is looked at while nothing arrives.
const TICK: Duration = Duration::from_millis(50);
/// After the process ends, how long its last output may take to arrive (a Windows pseudo-console only closes its
/// output when Klefki closes it).
const DRAIN: Duration = Duration::from_millis(300);
const MAX_MESSAGE_CHARS: usize = 300;
/// What Claude prints while it waits for a pasted address; never a result.
const PROMPT: &str = "paste the redirect URL here";

/// The sign-in link Claude printed, checked: https, and made by Claude for this sign-in (it sends the browser back
/// to Claude's own `localhost` listener with a `state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Page {
    /// Exactly as printed; only ever passed to the browser.
    pub link: String,
    /// Where the page is, for showing.
    pub host: String,
    /// The callback address, without its query.
    callback: url::Url,
    state: String,
}

/// Finds the sign-in link in what Claude printed so far (already cleaned of escapes). `None` while there is none;
/// `Err` says why a link that was printed cannot be used.
pub(crate) fn find_page(text: &str) -> Option<Result<Page, String>> {
    // Only words that are complete: output arrives in pieces, and a link cut short can look whole.
    let complete = &text[..text.rfind(char::is_whitespace)?];
    complete
        .split_whitespace()
        .filter(|t| t.starts_with("https://") || t.starts_with("http://"))
        .find_map(|token| {
            let url = url::Url::parse(token).ok()?;
            let redirect = url
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .map(|(_, v)| v.into_owned())?;
            Some(check_page(token, &url, &redirect))
        })
}

fn check_page(token: &str, url: &url::Url, redirect: &str) -> Result<Page, String> {
    let host = url.host_str().map(|h| clip(&sanitize(h), 100));
    let Some(host) = host.filter(|h| !h.is_empty()) else {
        return Err("The sign-in link Claude Code printed has no host.".to_owned());
    };
    if token.len() > MAX_ADDRESS {
        return Err("The sign-in link Claude Code printed is too long to be real.".to_owned());
    }
    if url.scheme() != "https" {
        return Err(format!(
            "The sign-in page at {host} is not secure (it is not https), so Klefki will not open it."
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "The sign-in link for {host} carries a user name or password, so Klefki will not open it."
        ));
    }
    let callback = url::Url::parse(redirect)
        .ok()
        .filter(|c| matches!(c.scheme(), "http" | "https"))
        .filter(|c| c.port().is_some() && is_loopback(c))
        .filter(|c| c.username().is_empty() && c.password().is_none());
    let Some(mut callback) = callback else {
        return Err(format!(
            "The sign-in link for {host} does not send you back to Claude Code on this computer, so Klefki will not open it."
        ));
    };
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .filter(|s| !s.is_empty() && s.len() <= 1024);
    let Some(state) = state else {
        return Err(format!(
            "The sign-in link for {host} has no state to check the answer against, so Klefki will not open it."
        ));
    };
    callback.set_query(None);
    callback.set_fragment(None);
    Ok(Page {
        link: token.to_owned(),
        host,
        callback,
        state,
    })
}

fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Checks an address the user pasted from the browser before it is typed into Claude's prompt: one line of plain
/// characters, back to this sign-in's callback, with its `state`. Returns what to type, without the Enter.
pub(crate) fn check_paste(page: &Page, pasted: &str) -> Result<String, String> {
    let pasted = pasted.trim();
    let start = page.callback.as_str().trim_end_matches('/');
    let expected = format!("The right one starts with {start}.");
    if pasted.is_empty() {
        return Err(format!(
            "Paste the address your browser shows after you sign in. {expected}"
        ));
    }
    if pasted.len() > MAX_ADDRESS || pasted.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("That is not a web address. {expected}"));
    }
    let Ok(url) = url::Url::parse(pasted) else {
        return Err(format!("That is not a web address. {expected}"));
    };
    let same_place = url.scheme() == page.callback.scheme()
        && url.host() == page.callback.host()
        && url.port_or_known_default() == page.callback.port_or_known_default()
        && url.path() == page.callback.path()
        && url.username().is_empty()
        && url.password().is_none();
    if !same_place {
        return Err(format!(
            "That is not the address this sign-in sends you back to. {expected}"
        ));
    }
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v);
    if query("state").as_deref() != Some(page.state.as_str()) {
        return Err(
            "That address belongs to another sign-in. Paste the one from the page this sign-in opened."
                .to_owned(),
        );
    }
    if query("code").is_none() && query("error").is_none() {
        return Err(format!(
            "That address does not carry the answer yet. Finish signing in first. {expected}"
        ));
    }
    Ok(pasted.to_owned())
}

/// Claude's own words for why it stopped: its last line that is not the link, a prompt or a notice.
pub(crate) fn last_message(text: &str) -> Option<String> {
    const NOT_A_REASON: [&str; 4] = [
        "[mcp-sdk]",
        "Waiting for authorization",
        "Starting authentication",
        "Visit this URL",
    ];
    text.lines()
        .map(str::trim)
        .rfind(|l| {
            !l.is_empty()
                && !l.contains(PROMPT)
                && !l.contains("://")
                && !NOT_A_REASON.iter().any(|p| l.starts_with(p))
        })
        .map(|l| clip(l, MAX_MESSAGE_CHARS))
}

/// What the session reports as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    /// The sign-in link, checked.
    Page(Page),
    /// The pasted address was typed in; Claude is checking it.
    Pasted,
    /// The pasted address was not used, by Klefki or by Claude; says why. Claude keeps waiting.
    PasteRefused(String),
    Finished(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Claude said it signed in (exit code 0).
    SignedIn,
    Failed(String),
    Cancelled,
    TimedOut,
}

/// What reaches a running session: its terminal's output, and the user's requests.
pub(crate) enum Message {
    Output(Vec<u8>),
    Paste(String),
    Cancel,
}

/// The pseudo-terminal Claude runs in, as the session needs it. A trait so the session can be tested with a
/// scripted stand-in.
pub(crate) trait Terminal {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// The exit code once the process has ended.
    fn exit_code(&mut self) -> Option<i32>;
    fn kill(&mut self);
}

pub(crate) struct LoginSpec<'a> {
    pub program: &'a Path,
    pub name: &'a ServerName,
    pub cwd: &'a Path,
    /// Extra environment; tests point `fake-claude` at a scenario with it.
    pub env: &'a [(&'a str, &'a OsStr)],
}

/// Starts `claude mcp login <name> --no-browser` in a pseudo-terminal, with no shell. Its output is sent to `tx`
/// as [`Message::Output`] from a thread of its own.
pub(crate) fn start(spec: &LoginSpec<'_>, tx: Sender<Message>) -> io::Result<Pty> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    locate::check(spec.program)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            // Wide, so that no line is wrapped: a Windows pseudo-console re-renders output and would break the link
            // over lines at the edge.
            cols: 4000,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(io::Error::other)?;
    let mut cmd = CommandBuilder::new(spec.program);
    cmd.args(["mcp", "login", spec.name.as_str(), "--no-browser"]);
    cmd.cwd(spec.cwd);
    cmd.env("NO_COLOR", "1");
    // A secret in Klefki's own environment must not reach a child that was not given it on purpose.
    cmd.env_remove("MCP_CLIENT_SECRET");
    for (key, value) in spec.env {
        cmd.env(key, value);
    }
    let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
    // Only the child holds the terminal's other end now, so its output ends when it does (on Unix).
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
    let writer = pair.master.take_writer().map_err(io::Error::other)?;
    std::thread::Builder::new()
        .name("klefki-sign-in-output".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(Message::Output(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        })?;
    Ok(Pty {
        child,
        writer,
        _master: pair.master,
    })
}

/// A running `claude mcp login`. Dropping it kills the process and closes the terminal.
pub(crate) struct Pty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Terminal for Pty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    fn exit_code(&mut self) -> Option<i32> {
        let status = self.child.try_wait().ok()??;
        Some(i32::try_from(status.exit_code()).unwrap_or(i32::MAX))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if self.exit_code().is_none() {
            self.kill();
        }
    }
}

/// Terminal queries Claude's runtime or a Windows pseudo-console may send, and the answers a terminal would give.
/// A pseudo-console draws nothing until its cursor-position query is answered.
const ANSWERS: [(&[u8], &[u8]); 2] = [(b"\x1b[6n", b"\x1b[1;1R"), (b"\x1b[c", b"\x1b[?1;2c")];

/// Runs a started session until Claude ends, the user cancels, or `timeout` passes. Reports through `progress`;
/// the last report is always [`Progress::Finished`]. `messages` carries the terminal's output and the user's
/// requests.
pub(crate) fn drive(
    term: &mut dyn Terminal,
    messages: &Receiver<Message>,
    timeout: Duration,
    progress: &mut dyn FnMut(Progress),
) {
    let deadline = Instant::now() + timeout;
    let mut read = Reading::default();
    let outcome = loop {
        match messages.recv_timeout(TICK) {
            Ok(Message::Output(bytes)) => {
                answer_queries(term, &bytes);
                if let Err(why) = read.absorb(&bytes, progress) {
                    term.kill();
                    break Outcome::Failed(why);
                }
            }
            Ok(Message::Paste(pasted)) => {
                let checked = match &read.page {
                    Some(page) => check_paste(page, &pasted),
                    None => Err("The sign-in page is not ready yet.".to_owned()),
                };
                match checked {
                    Ok(address) => {
                        let mut line = address.into_bytes();
                        line.push(b'\r');
                        if term.write(&line).is_ok() {
                            read.pasted_at = Some(read.text.len());
                            progress(Progress::Pasted);
                        } else {
                            progress(Progress::PasteRefused(
                                "Klefki could not type the address in. Try again.".to_owned(),
                            ));
                        }
                    }
                    Err(why) => progress(Progress::PasteRefused(why)),
                }
            }
            Ok(Message::Cancel) => {
                term.kill();
                break Outcome::Cancelled;
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if let Some(code) = term.exit_code() {
            // Its last words may still be on their way. A link among them is still reported; a bad one no longer
            // matters, Claude has ended.
            let until = Instant::now() + DRAIN;
            while let Ok(Message::Output(bytes)) =
                messages.recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                let _ = read.absorb(&bytes, progress);
            }
            break if code == 0 {
                Outcome::SignedIn
            } else {
                Outcome::Failed(last_message(&read.text).unwrap_or_else(|| {
                    format!("Claude Code stopped without signing in (exit code {code}).")
                }))
            };
        }
        if Instant::now() >= deadline {
            term.kill();
            break Outcome::TimedOut;
        }
    };
    progress(Progress::Finished(outcome));
}

/// What has been read from Claude so far.
#[derive(Default)]
struct Reading {
    output: Vec<u8>,
    /// `output`, cleaned of escapes and control characters.
    text: String,
    page: Option<Page>,
    /// Where `text` stood when an address was typed in: what Claude says after it is its answer.
    pasted_at: Option<usize>,
}

impl Reading {
    /// Takes in more output and reports what it shows. `Err` when Claude printed a link Klefki will not open.
    fn absorb(&mut self, bytes: &[u8], progress: &mut dyn FnMut(Progress)) -> Result<(), String> {
        keep(&mut self.output, bytes);
        self.text = sanitize(&String::from_utf8_lossy(&self.output));
        if self.page.is_none() {
            match find_page(&self.text) {
                Some(Ok(found)) => {
                    self.page = Some(found.clone());
                    progress(Progress::Page(found));
                }
                Some(Err(why)) => return Err(why),
                None => {}
            }
        }
        if let Some(at) = self.pasted_at
            && let Some(answer) = refusal(self.text.get(at..).unwrap_or(""))
        {
            self.pasted_at = None;
            progress(Progress::PasteRefused(answer));
        }
        Ok(())
    }
}

fn answer_queries(term: &mut dyn Terminal, bytes: &[u8]) {
    for (query, answer) in ANSWERS {
        if bytes.windows(query.len()).any(|w| w == query) {
            let _ = term.write(answer);
        }
    }
}

/// Keeps the output for reading, at most [`MAX_OUTPUT`] bytes: past that, the oldest half goes (the link is found
/// long before).
fn keep(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(bytes);
    if output.len() > MAX_OUTPUT {
        output.drain(..output.len() - MAX_OUTPUT / 2);
    }
}

/// Claude's answer to a pasted address that it did not take: a whole line after the echo that is neither the prompt
/// nor its success message.
fn refusal(after: &str) -> Option<String> {
    let complete = &after[..after.rfind('\n')? + 1];
    complete
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(PROMPT) && !l.contains("://"))
        .find(|l| !l.starts_with("Authenticated"))
        .map(|l| clip(l, MAX_MESSAGE_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::mpsc;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/mcp-login")
            .join(name);
        std::fs::read(path).unwrap()
    }

    fn text_of(bytes: &[u8]) -> String {
        sanitize(&String::from_utf8_lossy(bytes))
    }

    fn https_page() -> Page {
        find_page(&text_of(&fixture("synthetic-waiting-https.txt")))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn the_link_is_found_in_claudes_real_output() {
        let page = https_page();
        assert_eq!(page.host, "auth.example.com");
        assert!(
            page.link
                .starts_with("https://auth.example.com/authorize?response_type=code&")
        );
        assert!(
            page.link
                .ends_with("&resource=http%3A%2F%2F127.0.0.1%3A8765%2Fmcp")
        );
        assert_eq!(page.callback.as_str(), "http://localhost:61983/callback");
        assert_eq!(page.state, "yikG7oorOB137qg1e_Nk72Ehk6xR2RxitLQlksJ1ZGM");
        // The link is printed twice (as a terminal hyperlink and as text); both are the same.
        assert!(!page.link.contains('\u{1b}') && !page.link.contains('\u{7}'));
    }

    #[test]
    fn a_link_that_is_not_https_is_refused_with_its_host() {
        // The spike's own server is plain http on 127.0.0.1.
        let why = find_page(&text_of(&fixture("real-waiting-http.txt")))
            .unwrap()
            .unwrap_err();
        assert!(
            why.contains("127.0.0.1") && why.contains("not https"),
            "{why}"
        );
        // A nearby variant: http on a public host.
        let why = find_page("Visit: http://login.example.com/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fcb&state=s\n")
            .unwrap()
            .unwrap_err();
        assert!(why.contains("login.example.com"), "{why}");
    }

    #[test]
    fn nothing_is_found_before_the_link_or_in_links_that_are_not_sign_in_links() {
        assert_eq!(find_page("Starting authentication for \"spike\"…\n"), None);
        assert_eq!(
            find_page("Server at https://mcp.example.com/mcp is slow\n"),
            None
        );
        // A link still arriving is not taken, even when what has come so far would pass.
        assert_eq!(
            find_page(
                "  https://auth.example.com/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fc&state=ab"
            ),
            None
        );
    }

    #[test]
    fn links_that_do_not_come_back_to_claude_on_this_computer_are_refused() {
        let link = |redirect: &str, extra: &str| {
            format!(
                "https://auth.example.com/a?redirect_uri={}&state=abc{extra}\n",
                url::form_urlencoded::byte_serialize(redirect.as_bytes()).collect::<String>()
            )
        };
        for redirect in [
            "http://evil.example.com:8080/callback",
            "http://localhost/callback", // no port: not Claude's listener
            "javascript:alert(1)",
            "http://user:pw@localhost:8080/callback",
            "http://localhost.evil.example.com:8080/callback",
            "http://10.0.0.1:8080/callback",
        ] {
            assert!(
                find_page(&link(redirect, "")).unwrap().is_err(),
                "{redirect}"
            );
        }
        for redirect in [
            "http://localhost:8080/callback",
            "http://127.0.0.1:8080/callback",
            "http://[::1]:8080/callback",
            "http://LOCALHOST:8080/callback",
        ] {
            assert!(
                find_page(&link(redirect, "")).unwrap().is_ok(),
                "{redirect}"
            );
        }
        // No state, and a user name in the link itself.
        assert!(
            find_page("https://auth.example.com/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fc\n")
                .unwrap()
                .is_err()
        );
        assert!(
            find_page("https://me@auth.example.com/a?redirect_uri=http%3A%2F%2Flocalhost%3A1%2Fc&state=s\n")
                .unwrap()
                .is_err()
        );
        // An absurdly long link.
        let long = link("http://localhost:8080/callback", &"&x=y".repeat(3000));
        assert!(find_page(&long).unwrap().is_err());
    }

    #[test]
    fn a_pasted_address_must_be_this_sign_ins_callback_with_its_state() {
        let page = https_page();
        let good = format!(
            "http://localhost:61983/callback?code=abc123&state={}",
            page.state
        );
        assert_eq!(check_paste(&page, &good), Ok(good.clone()));
        // Surrounding spaces from a copy are fine; the address typed in has none.
        assert_eq!(check_paste(&page, &format!("  {good}\n")), Ok(good.clone()));
        // An error answer from the server is passed on: Claude reports it.
        let denied = format!(
            "http://localhost:61983/callback?error=access_denied&state={}",
            page.state
        );
        assert!(check_paste(&page, &denied).is_ok());
        // 127.0.0.1 is not what Claude listens for here; Claude would refuse it too.
        for bad in [
            String::new(),
            "hello".to_owned(),
            good.replace("61983", "61984"),
            good.replace("localhost", "evil.example.com"),
            good.replace("/callback", "/other"),
            good.replace("http://", "https://"),
            good.replace(&page.state, "other-state"),
            format!("http://localhost:61983/callback?state={}", page.state),
            good.replace("?code", "\u{3}?code"),
            format!("{good}\r\u{3}"),
            format!("{good} extra"),
            format!("{good}{}", "a".repeat(MAX_ADDRESS)),
        ] {
            assert!(check_paste(&page, &bad).is_err(), "{bad:?}");
        }
        // The messages tell the user what the right address starts with.
        let why = check_paste(&page, "hello").unwrap_err();
        assert!(why.contains("http://localhost:61983/callback"), "{why}");
    }

    #[test]
    fn claudes_last_message_is_its_reason() {
        let text = text_of(&fixture("real-port-in-use.txt"));
        let why = last_message(&text).unwrap();
        assert!(why.starts_with("Couldn't complete authentication for \"fixed\": OAuth callback port 8766 is already in use"), "{why}");
        let why = last_message(&text_of(&fixture("real-not-oauth.txt"))).unwrap();
        assert!(why.contains("doesn't support OAuth login"), "{why}");
        // Only the link and prompts: no message.
        assert_eq!(
            last_message(&text_of(&fixture("real-waiting-http.txt"))),
            None
        );
    }

    /// A scripted terminal: what it was sent, and when its process "ends" (`exit`, shared so a test can end it
    /// while the session runs).
    #[derive(Default)]
    struct Script {
        written: Vec<u8>,
        exit: Rc<Cell<Option<i32>>>,
        killed: bool,
        /// Exit with this code once something is written (Claude taking a paste).
        exit_on_write: Option<i32>,
        fail_writes: bool,
    }

    impl Terminal for Script {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.fail_writes {
                return Err(io::Error::other("closed"));
            }
            self.written.extend_from_slice(bytes);
            if self.exit_on_write.is_some() && !bytes.starts_with(b"\x1b") {
                self.exit.set(self.exit_on_write);
            }
            Ok(())
        }
        fn exit_code(&mut self) -> Option<i32> {
            self.exit.get()
        }
        fn kill(&mut self) {
            self.killed = true;
            self.exit.set(Some(137));
        }
    }

    fn run(term: &mut Script, messages: Vec<Message>, timeout: Duration) -> Vec<Progress> {
        let (tx, rx) = mpsc::channel();
        for m in messages {
            tx.send(m).unwrap();
        }
        let mut seen = VecDeque::new();
        drive(term, &rx, timeout, &mut |p| seen.push_back(p));
        seen.into_iter().collect()
    }

    #[test]
    fn a_sign_in_through_the_browser_reports_the_page_then_signs_in() {
        let mut term = Script::default();
        let exit = Rc::clone(&term.exit);
        let (tx, rx) = mpsc::channel();
        tx.send(Message::Output(fixture("synthetic-waiting-https.txt")))
            .unwrap();
        let mut seen = Vec::new();
        drive(&mut term, &rx, Duration::from_secs(5), &mut |p| {
            if matches!(p, Progress::Page(_)) {
                // The browser reached Claude's listener: Claude prints its success and ends with 0.
                tx.send(Message::Output(fixture("real-signed-in-tail.txt")))
                    .unwrap();
                exit.set(Some(0));
            }
            seen.push(p);
        });
        assert_eq!(
            seen,
            vec![
                Progress::Page(https_page()),
                Progress::Finished(Outcome::SignedIn)
            ]
        );
        assert!(!term.killed);
        assert!(
            term.written.is_empty(),
            "nothing is typed when the browser answers"
        );
    }

    #[test]
    fn exit_code_zero_is_signed_in_and_anything_else_carries_claudes_reason() {
        let mut term = Script {
            exit: Rc::new(Cell::new(Some(0))),
            ..Script::default()
        };
        let seen = run(
            &mut term,
            vec![Message::Output(fixture("real-signed-in-tail.txt"))],
            Duration::from_secs(5),
        );
        assert_eq!(seen, vec![Progress::Finished(Outcome::SignedIn)]);
        assert!(!term.killed);

        let mut term = Script {
            exit: Rc::new(Cell::new(Some(1))),
            ..Script::default()
        };
        let seen = run(
            &mut term,
            vec![Message::Output(fixture("real-port-in-use.txt"))],
            Duration::from_secs(5),
        );
        let [Progress::Finished(Outcome::Failed(why))] = seen.as_slice() else {
            panic!("{seen:?}");
        };
        assert!(why.contains("port 8766 is already in use"), "{why}");

        // Silent failure: the exit code is the reason.
        let mut term = Script {
            exit: Rc::new(Cell::new(Some(3))),
            ..Script::default()
        };
        let seen = run(&mut term, vec![], Duration::from_secs(5));
        let [Progress::Finished(Outcome::Failed(why))] = seen.as_slice() else {
            panic!("{seen:?}");
        };
        assert!(why.contains("exit code 3"), "{why}");
    }

    #[test]
    fn a_link_that_is_not_https_ends_the_sign_in_and_kills_claude() {
        let mut term = Script::default();
        let seen = run(
            &mut term,
            vec![Message::Output(fixture("real-waiting-http.txt"))],
            Duration::from_secs(5),
        );
        let [Progress::Finished(Outcome::Failed(why))] = seen.as_slice() else {
            panic!("{seen:?}");
        };
        assert!(why.contains("not https"), "{why}");
        assert!(term.killed);
    }

    #[test]
    fn a_checked_paste_is_typed_with_enter_and_nothing_else_is() {
        let page = https_page();
        let good = format!(
            "http://localhost:61983/callback?code=c0de&state={}",
            page.state
        );
        let mut term = Script {
            exit_on_write: Some(0),
            ..Script::default()
        };
        let seen = run(
            &mut term,
            vec![
                // Before the page: refused, nothing typed.
                Message::Paste(good.clone()),
                Message::Output(fixture("synthetic-waiting-https.txt")),
                // A wrong address: refused by Klefki, nothing typed.
                Message::Paste("http://evil.example.com/?code=1".to_owned()),
                Message::Paste(format!("  {good}  ")),
            ],
            Duration::from_secs(5),
        );
        assert!(
            matches!(seen[0], Progress::PasteRefused(ref w) if w.contains("not ready")),
            "{seen:?}"
        );
        assert!(matches!(seen[1], Progress::Page(_)), "{seen:?}");
        assert!(
            matches!(seen[2], Progress::PasteRefused(ref w) if w.contains("http://localhost:61983/callback")),
            "{seen:?}"
        );
        assert_eq!(seen[3], Progress::Pasted);
        assert_eq!(seen.last(), Some(&Progress::Finished(Outcome::SignedIn)));
        assert_eq!(term.written, format!("{good}\r").into_bytes());
    }

    #[test]
    fn claudes_refusal_of_a_paste_is_reported_and_the_sign_in_goes_on() {
        let page = https_page();
        let good = format!(
            "http://localhost:61983/callback?code=c0de&state={}",
            page.state
        );
        let mut term = Script::default();
        let (tx, rx) = mpsc::channel();
        tx.send(Message::Output(fixture("synthetic-waiting-https.txt")))
            .unwrap();
        tx.send(Message::Paste(good)).unwrap();
        let mut seen = Vec::new();
        drive(&mut term, &rx, Duration::from_secs(5), &mut |p| {
            if p == Progress::Pasted {
                // The echo, Claude's refusal and its prompt again, as captured.
                tx.send(Message::Output(fixture("real-paste-refused-tail.txt")))
                    .unwrap();
            }
            if matches!(p, Progress::PasteRefused(_)) {
                tx.send(Message::Cancel).unwrap();
            }
            seen.push(p);
        });
        assert!(
            matches!(&seen[2], Progress::PasteRefused(w) if w.starts_with("That doesn't look like a redirect URL")),
            "{seen:?}"
        );
        assert_eq!(seen.last(), Some(&Progress::Finished(Outcome::Cancelled)));
        assert!(term.killed);
    }

    #[test]
    fn a_failed_write_is_reported_not_counted_as_pasted() {
        let page = https_page();
        let mut term = Script {
            fail_writes: true,
            ..Script::default()
        };
        let seen = run(
            &mut term,
            vec![
                Message::Output(fixture("synthetic-waiting-https.txt")),
                Message::Paste(format!(
                    "http://localhost:61983/callback?code=c&state={}",
                    page.state
                )),
                Message::Cancel,
            ],
            Duration::from_secs(5),
        );
        assert!(
            matches!(&seen[1], Progress::PasteRefused(w) if w.contains("could not type")),
            "{seen:?}"
        );
    }

    #[test]
    fn cancel_and_timeout_kill_claude() {
        let mut term = Script::default();
        let seen = run(&mut term, vec![Message::Cancel], Duration::from_secs(5));
        assert_eq!(seen, vec![Progress::Finished(Outcome::Cancelled)]);
        assert!(term.killed);

        let mut term = Script::default();
        let started = Instant::now();
        let seen = run(&mut term, vec![], Duration::from_millis(200));
        assert_eq!(seen, vec![Progress::Finished(Outcome::TimedOut)]);
        assert!(term.killed);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn terminal_queries_are_answered_so_a_windows_pseudo_console_draws() {
        let mut term = Script::default();
        run(
            &mut term,
            vec![Message::Output(b"\x1b[6n".to_vec()), Message::Cancel],
            Duration::from_secs(5),
        );
        assert_eq!(term.written, b"\x1b[1;1R");
    }

    #[test]
    fn a_link_split_over_reads_is_found_once_whole() {
        let bytes = fixture("synthetic-waiting-https.txt");
        let mut term = Script::default();
        let mut messages: Vec<Message> = bytes
            .chunks(7)
            .map(|c| Message::Output(c.to_vec()))
            .collect();
        messages.push(Message::Cancel);
        let seen = run(&mut term, messages, Duration::from_secs(5));
        let pages: Vec<_> = seen
            .iter()
            .filter(|p| matches!(p, Progress::Page(_)))
            .collect();
        assert_eq!(pages.len(), 1, "{seen:?}");
        assert_eq!(pages[0], &Progress::Page(https_page()));
    }

    /// `claude mcp login` played by fake-claude, in a real pseudo-terminal.
    struct Real {
        dir: tempfile::TempDir,
        scenario: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    impl Real {
        fn new(rule: &str, files: &[(&str, &[u8])]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let scenario = crate::cli::testutil::write_scenario(
                dir.path(),
                &format!(r#"{{"rules":[{rule}]}}"#),
                files,
            );
            let log = dir.path().join("log.jsonl");
            Self { dir, scenario, log }
        }

        /// Starts the session and drives it on a thread; returns what it reported once it ends, and a way in.
        fn run(
            &self,
            timeout: Duration,
        ) -> (Sender<Message>, std::thread::JoinHandle<Vec<Progress>>) {
            let (tx, rx) = mpsc::channel();
            let name = ServerName::parse("srv").unwrap();
            let env: Vec<(&str, &OsStr)> = vec![
                ("FAKE_CLAUDE_SCENARIO", self.scenario.as_os_str()),
                ("FAKE_CLAUDE_LOG", self.log.as_os_str()),
            ];
            let mut pty = start(
                &LoginSpec {
                    program: crate::cli::testutil::fake_claude(),
                    name: &name,
                    cwd: self.dir.path(),
                    env: &env,
                },
                tx.clone(),
            )
            .unwrap();
            let handle = std::thread::spawn(move || {
                let mut seen = Vec::new();
                drive(&mut pty, &rx, timeout, &mut |p| seen.push(p));
                seen
            });
            (tx, handle)
        }

        fn log(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }

        fn wait_for_pid(&self) -> u32 {
            let file = self.dir.path().join("pid");
            let until = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(pid) = std::fs::read_to_string(&file)
                    .ok()
                    .and_then(|s| s.parse().ok())
                {
                    return pid;
                }
                assert!(Instant::now() < until, "fake-claude never started");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn gone_soon(pid: u32) -> bool {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if !crate::cli::testutil::pid_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn claude_runs_in_a_real_terminal_with_no_shell_and_its_link_comes_through_whole() {
        let mut transcript = fixture("synthetic-waiting-https.txt");
        transcript.extend(fixture("real-signed-in-tail.txt"));
        let real = Real::new(
            r#"{"args":["mcp","login","srv","--no-browser"],"require_tty":true,"stdout_file":"out.txt","exit_code":0}"#,
            &[("out.txt", &transcript)],
        );
        let (_tx, handle) = real.run(Duration::from_secs(30));
        let seen = handle.join().unwrap();
        assert_eq!(
            seen,
            vec![
                Progress::Page(https_page()),
                Progress::Finished(Outcome::SignedIn)
            ]
        );
        let log = real.log();
        assert_eq!(
            log[0]["argv"],
            serde_json::json!(["mcp", "login", "srv", "--no-browser"])
        );
        assert_eq!(
            std::fs::canonicalize(log[0]["cwd"].as_str().unwrap()).unwrap(),
            std::fs::canonicalize(real.dir.path()).unwrap()
        );
        assert_eq!(log[0]["no_color_set"], true);
        assert_eq!(log[0]["secret_set"], false);
    }

    #[test]
    fn a_pasted_address_reaches_claude_through_a_real_terminal_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let line = dir.path().join("line.txt");
        let real = Real::new(
            &format!(
                r#"{{"args":["mcp","login","srv","--no-browser"],"require_tty":true,"stdout_file":"page.txt",
                    "read_line_to":{},"after_line_stdout_file":"tail.txt","exit_code":0}}"#,
                serde_json::Value::from(line.to_str().unwrap())
            ),
            &[
                ("page.txt", &fixture("synthetic-waiting-https.txt")),
                ("tail.txt", &fixture("real-signed-in-tail.txt")),
            ],
        );
        let page = https_page();
        let good = format!(
            "http://localhost:61983/callback?code=c0de&state={}",
            page.state
        );
        let (tx, rx) = mpsc::channel();
        let name = ServerName::parse("srv").unwrap();
        let env: Vec<(&str, &OsStr)> = vec![
            ("FAKE_CLAUDE_SCENARIO", real.scenario.as_os_str()),
            ("FAKE_CLAUDE_LOG", real.log.as_os_str()),
        ];
        let mut pty = start(
            &LoginSpec {
                program: crate::cli::testutil::fake_claude(),
                name: &name,
                cwd: real.dir.path(),
                env: &env,
            },
            tx.clone(),
        )
        .unwrap();
        let mut seen = Vec::new();
        drive(&mut pty, &rx, Duration::from_secs(30), &mut |p| {
            if matches!(p, Progress::Page(_)) {
                // A wrong one first: refused by Klefki, never typed.
                tx.send(Message::Paste(
                    "http://localhost:61983/callback?code=x&state=other".into(),
                ))
                .unwrap();
                tx.send(Message::Paste(good.clone())).unwrap();
            }
            seen.push(p);
        });
        assert!(matches!(seen[0], Progress::Page(_)), "{seen:?}");
        assert!(matches!(seen[1], Progress::PasteRefused(_)), "{seen:?}");
        assert_eq!(seen[2], Progress::Pasted);
        assert_eq!(seen.last(), Some(&Progress::Finished(Outcome::SignedIn)));
        // It had a terminal (else it exits 1), and read exactly the checked address, once.
        assert_eq!(std::fs::read_to_string(&line).unwrap(), good);
    }

    #[test]
    fn a_failure_in_a_real_terminal_carries_claudes_reason() {
        let real = Real::new(
            r#"{"args":["mcp","login","srv","--no-browser"],"stdout_file":"out.txt","exit_code":1}"#,
            &[("out.txt", &fixture("real-port-in-use.txt"))],
        );
        let (_tx, handle) = real.run(Duration::from_secs(30));
        let seen = handle.join().unwrap();
        let [Progress::Finished(Outcome::Failed(why))] = seen.as_slice() else {
            panic!("{seen:?}");
        };
        assert!(why.contains("port 8766 is already in use"), "{why}");
    }

    #[test]
    fn cancel_and_timeout_kill_the_real_process() {
        let rule = r#"{"args":["mcp","login"],"hang":true,"write_pid_to":"PID"}"#;
        for cancel in [true, false] {
            let real = Real::new("{}", &[]);
            let pid_file = real.dir.path().join("pid");
            let rule = rule.replace("PID", &pid_file.display().to_string().replace('\\', "\\\\"));
            std::fs::write(&real.scenario, format!(r#"{{"rules":[{rule}]}}"#)).unwrap();
            let (tx, handle) = real.run(Duration::from_secs(if cancel { 60 } else { 2 }));
            let pid = real.wait_for_pid();
            if cancel {
                tx.send(Message::Cancel).unwrap();
            }
            let seen = handle.join().unwrap();
            let want = if cancel {
                Outcome::Cancelled
            } else {
                Outcome::TimedOut
            };
            assert_eq!(seen, vec![Progress::Finished(want)]);
            assert!(gone_soon(pid), "fake-claude {pid} still runs");
        }
    }

    #[test]
    fn endless_output_is_kept_small() {
        let mut out = Vec::new();
        for _ in 0..100 {
            keep(&mut out, &[b'x'; 4096]);
        }
        assert!(out.len() <= MAX_OUTPUT);
    }
}

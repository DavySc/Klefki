//! Signing in to a server from Klefki: `claude mcp login` in a pseudo-terminal, one at a time (decision 005).
use super::{
    Command, Emitter, Event, Input, OpenUrl, Shared, SignInFailure, SignInStep, State, lock,
    where_to_run,
};
use crate::api::{Approval, Scope, ServerId, TransportKind};
use crate::cli::login::{self, LoginSpec, Message, Outcome, Progress};
use crate::config::inventory;
use crate::diagnose::hints::hints_for;
use crate::sanitize::sanitize;
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A sign-in that was started. Its session runs on a thread of its own and reports straight to the UI.
pub(super) struct Running {
    id: ServerId,
    tx: Sender<Message>,
    /// Where the checked sign-in page is and its link, once Claude printed it: the only link "open the page again"
    /// opens.
    page: Arc<Mutex<Option<(String, String)>>>,
    done: Arc<AtomicBool>,
}

impl Running {
    fn active(&self) -> bool {
        !self.done.load(Ordering::SeqCst)
    }

    fn is(&self, id: &ServerId) -> bool {
        self.id == *id && self.active()
    }
}

/// Starts signing in to `id`, or says why not.
pub(super) fn start(shared: &Arc<Shared>, state: &mut State, id: ServerId) {
    let emitter = &shared.emitter;
    let fail = |why: String, can_retry: bool| {
        emitter.emit(Event::SignIn {
            id: id.clone(),
            step: SignInStep::Failed(SignInFailure::plain(why, can_retry)),
        });
    };
    if let Some(running) = state.sign_in.as_ref().filter(|r| r.active()) {
        let what = if running.id == id {
            "this server".to_owned()
        } else {
            sanitize(&running.id.name)
        };
        return fail(
            format!("Klefki is already signing in to {what}. Finish or cancel that sign-in first."),
            true,
        );
    }
    let Some((row, file)) = inventory::locate(&state.snapshot, &state.keys, &id) else {
        return fail(
            "The server is no longer in the configuration.".to_owned(),
            false,
        );
    };
    if !matches!(row.transport, TransportKind::Http | TransportKind::Sse) {
        return fail(
            "Only servers reached over HTTP or SSE sign in.".to_owned(),
            false,
        );
    }
    if row.scope == Scope::Project && row.approval != Some(Approval::Approved) {
        return fail(
            "Approve this project server first: until then Claude Code does not use it.".to_owned(),
            false,
        );
    }
    let (name, cwd) = match where_to_run(shared, row, Some(file)) {
        Ok(found) => found,
        Err(why) => return fail(format!("Klefki cannot sign in to it: {why}."), false),
    };
    let Some(program) = super::claude_program(shared) else {
        return fail(
            "The Claude Code command line was not found, and signing in goes through it."
                .to_owned(),
            false,
        );
    };

    let (tx, rx) = mpsc::channel();
    let page = Arc::new(Mutex::new(None));
    let done = Arc::new(AtomicBool::new(false));
    state.sign_in = Some(Running {
        id: id.clone(),
        tx: tx.clone(),
        page: Arc::clone(&page),
        done: Arc::clone(&done),
    });
    let timeout = shared.config.sign_in_timeout;
    emitter.emit(Event::SignIn {
        id: id.clone(),
        step: SignInStep::Starting { wait: timeout },
    });

    let session = Session {
        id,
        emitter: emitter.clone(),
        open_url: Arc::clone(&shared.config.open_url),
        input: shared.input.clone(),
        page,
        done: Arc::clone(&done),
    };
    let env: Vec<(String, OsString)> = shared.config.cli_env.clone();
    let spawned = std::thread::Builder::new()
        .name("klefki-sign-in".into())
        .spawn(move || {
            let env: Vec<(&str, &OsStr)> = env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_os_str()))
                .collect();
            let spec = LoginSpec {
                program: &program,
                name: &name,
                cwd: &cwd,
                env: &env,
            };
            match login::start(&spec, tx) {
                Ok(mut pty) => login::drive(&mut pty, &rx, timeout, &mut |p| session.report(p)),
                Err(e) => session.finish(SignInStep::Failed(SignInFailure::plain(
                    format!(
                        "Klefki could not start Claude Code to sign in: {}",
                        sanitize(&e.to_string())
                    ),
                    true,
                ))),
            }
        });
    if spawned.is_err()
        && let Some(running) = &state.sign_in
    {
        running.done.store(true, Ordering::SeqCst);
        emitter.emit(Event::SignIn {
            id: running.id.clone(),
            step: SignInStep::Failed(SignInFailure::plain(
                "Klefki could not start the sign-in.".to_owned(),
                true,
            )),
        });
    }
}

/// What the session thread needs to report.
struct Session {
    id: ServerId,
    emitter: Emitter,
    open_url: OpenUrl,
    input: Sender<Input>,
    page: Arc<Mutex<Option<(String, String)>>>,
    done: Arc<AtomicBool>,
}

impl Session {
    /// The last report. Claude has ended by now (the session waits for that), so a new sign-in may start as soon
    /// as the user sees this one end.
    fn finish(&self, step: SignInStep) {
        self.done.store(true, Ordering::SeqCst);
        self.step(step);
    }

    fn step(&self, step: SignInStep) {
        self.emitter.emit(Event::SignIn {
            id: self.id.clone(),
            step,
        });
    }

    fn report(&self, progress: Progress) {
        let step = match progress {
            Progress::Page(page) => {
                *lock(&self.page) = Some((page.host.clone(), page.link.clone()));
                let opened = (self.open_url)(&page.link).is_ok();
                SignInStep::Waiting {
                    host: page.host,
                    link: page.link,
                    opened,
                }
            }
            Progress::Pasted => SignInStep::Pasted,
            Progress::PasteRefused(why) => SignInStep::PasteRefused(why),
            Progress::Finished(outcome) => {
                let step = match outcome {
                    Outcome::SignedIn => {
                        // Confirm it the way every status is known: Claude's own check.
                        let _ = self.input.send(Input::Ui(Command::Check(self.id.clone())));
                        SignInStep::SignedIn
                    }
                    Outcome::Failed(why) => SignInStep::Failed(SignInFailure::from_claude(&why)),
                    Outcome::Cancelled => SignInStep::Cancelled,
                    Outcome::TimedOut => SignInStep::Failed(SignInFailure::plain(
                        "Nothing came back from the browser in time, so the sign-in was stopped."
                            .to_owned(),
                        true,
                    )),
                };
                return self.finish(step);
            }
        };
        self.step(step);
    }
}

fn running<'a>(state: &'a State, id: &ServerId) -> Option<&'a Running> {
    state.sign_in.as_ref().filter(|r| r.is(id))
}

pub(super) fn paste(state: &State, id: &ServerId, address: String) {
    if let Some(running) = running(state, id) {
        let _ = running.tx.send(Message::Paste(address));
    }
}

pub(super) fn open_again(shared: &Shared, state: &State, id: &ServerId) {
    let Some(running) = running(state, id) else {
        return;
    };
    let Some((host, link)) = lock(&running.page).clone() else {
        return;
    };
    let opened = (shared.config.open_url)(&link).is_ok();
    shared.emitter.emit(Event::SignIn {
        id: id.clone(),
        step: SignInStep::Waiting { host, link, opened },
    });
}

pub(super) fn keep_waiting(state: &State, id: &ServerId) {
    if let Some(running) = running(state, id) {
        let _ = running.tx.send(Message::KeepWaiting);
    }
}

pub(super) fn cancel(state: &State, id: &ServerId) {
    if let Some(running) = running(state, id) {
        let _ = running.tx.send(Message::Cancel);
    }
}

/// How long closing Klefki waits for a running sign-in's `claude` to be stopped.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// Klefki is closing: stops a running sign-in and waits, briefly, until its `claude` is gone.
pub(super) fn stop(state: &mut State) {
    let Some(running) = state.sign_in.take().filter(Running::active) else {
        return;
    };
    let _ = running.tx.send(Message::Cancel);
    let until = Instant::now() + STOP_WAIT;
    while running.active() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl SignInFailure {
    /// Klefki's own words, with nothing more to add.
    fn plain(title: String, can_retry: bool) -> Self {
        Self {
            title,
            help: Vec::new(),
            said: None,
            can_retry,
        }
    }

    /// Claude Code's last message, turned into what happened and what to do. Its words are kept below, without
    /// any web page a server sent along with an error.
    fn from_claude(why: &str) -> Self {
        let said = Some(page_cut(why)).filter(|s| !s.is_empty());
        let lower = why.to_lowercase();
        if lower.contains("dynamic client registration") || lower.contains("support oauth") {
            return Self {
                title: "This server does not sign in through Claude Code.".to_owned(),
                help: vec![
                    "A server that takes a fixed token gets it in a header instead: add it on the Env tab."
                        .to_owned(),
                ],
                said,
                can_retry: false,
            };
        }
        if lower.contains("no mcp server named") {
            return Self {
                title: "Claude Code does not see this server.".to_owned(),
                help: vec![
                    "It may have been changed outside Klefki. Refresh, then sign in again."
                        .to_owned(),
                ],
                said,
                can_retry: false,
            };
        }
        match hints_for(why).first() {
            Some(&kind) => Self {
                title: format!("{}.", kind.title()),
                help: kind
                    .help()
                    .iter()
                    .map(|&h| h.to_owned())
                    // Advice to sign in, which is what just failed, says nothing here.
                    .filter(|h| !h.contains("Sign in"))
                    .collect(),
                said,
                can_retry: true,
            },
            None => Self {
                title: "The sign-in did not finish.".to_owned(),
                help: Vec::new(),
                said,
                can_retry: true,
            },
        }
    }
}

/// `text` up to a web page in it: servers answer some errors with a whole HTML page, which says nothing here.
fn page_cut(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let end = ["<!doctype", "<html", "<head", "<body"]
        .iter()
        .filter_map(|tag| lower.find(tag))
        .min()
        .unwrap_or(text.len());
    text[..end]
        .trim()
        .trim_end_matches(':')
        .trim_end()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claudes_failures_say_what_happened_and_whether_trying_again_can_help() {
        // Real: an HTTP server that takes a header token, not OAuth.
        let dcr = "SDK auth failed: Dynamic Client Registration rejected (HTTP 501): <!DOCTYPE HTML> <html lang=\"en\"> <head> <meta charset=\"utf-8\">";
        let f = SignInFailure::from_claude(dcr);
        assert_eq!(f.title, "This server does not sign in through Claude Code.");
        assert!(!f.can_retry);
        assert_eq!(
            f.said.as_deref(),
            Some("SDK auth failed: Dynamic Client Registration rejected (HTTP 501)")
        );
        let f = SignInFailure::from_claude(
            "\"stdiosrv\" doesn't support OAuth login — it's only available for HTTP and SSE servers.",
        );
        assert!(!f.can_retry, "{f:?}");
        let f =
            SignInFailure::from_claude("No MCP server named \"nope\". Configured servers: a, b");
        assert!(!f.can_retry, "{f:?}");

        // A server that is down: plain words and what to check; trying again can work once it runs.
        let f = SignInFailure::from_claude(
            "ECONNREFUSED: Unable to connect. Is the computer able to access the url?",
        );
        assert_eq!(f.title, "Nothing is listening at that address.");
        assert_eq!(f.help[0], "Check that the server is running.");
        assert!(f.can_retry);

        // Advice to sign in is not given when signing in is what failed.
        let f = SignInFailure::from_claude("HTTP 401 Unauthorized: invalid_token");
        assert!(!f.help.is_empty());
        assert!(f.help.iter().all(|h| !h.contains("Sign in")), "{f:?}");

        // Anything else keeps Claude's words under a plain title.
        let f = SignInFailure::from_claude(
            "Couldn't complete authentication for \"fixed\": OAuth callback port 8766 is already in use",
        );
        assert_eq!(f.title, "The sign-in did not finish.");
        assert!(f.said.unwrap().contains("8766"));
        assert!(f.can_retry);
    }

    #[test]
    fn a_web_page_in_an_error_is_left_out() {
        assert_eq!(page_cut("rejected: <html><body>x"), "rejected");
        assert_eq!(
            page_cut("rejected (HTTP 500): <!doctype html>"),
            "rejected (HTTP 500)"
        );
        assert_eq!(page_cut("no page here"), "no page here");
        assert_eq!(page_cut("<HTML>only a page"), "");
    }
}

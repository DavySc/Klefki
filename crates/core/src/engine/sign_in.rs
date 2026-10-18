//! Signing in to a server from Klefki: `claude mcp login` in a pseudo-terminal, one at a time (decision 005).
use super::{
    Command, Emitter, Event, Input, OpenUrl, Shared, SignInStep, State, lock, where_to_run,
};
use crate::api::{Approval, Scope, ServerId, TransportKind};
use crate::cli::login::{self, LoginSpec, Message, Outcome, Progress};
use crate::config::inventory;
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
    let fail = |why: String| {
        emitter.emit(Event::SignIn {
            id: id.clone(),
            step: SignInStep::Failed(why),
        });
    };
    if let Some(running) = state.sign_in.as_ref().filter(|r| r.active()) {
        let what = if running.id == id {
            "this server".to_owned()
        } else {
            sanitize(&running.id.name)
        };
        return fail(format!(
            "Klefki is already signing in to {what}. Finish or cancel that sign-in first."
        ));
    }
    let Some((row, file)) = inventory::locate(&state.snapshot, &state.keys, &id) else {
        return fail("The server is no longer in the configuration.".to_owned());
    };
    if !matches!(row.transport, TransportKind::Http | TransportKind::Sse) {
        return fail("Only servers reached over HTTP or SSE sign in.".to_owned());
    }
    if row.scope == Scope::Project && row.approval != Some(Approval::Approved) {
        return fail(
            "Approve this project server first: until then Claude Code does not use it.".to_owned(),
        );
    }
    let (name, cwd) = match where_to_run(shared, row, Some(file)) {
        Ok(found) => found,
        Err(why) => return fail(format!("Klefki cannot sign in to it: {why}.")),
    };
    let Some(program) = super::claude_program(shared) else {
        return fail(
            "The Claude Code command line was not found, and signing in goes through it."
                .to_owned(),
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
    emitter.emit(Event::SignIn {
        id: id.clone(),
        step: SignInStep::Starting,
    });

    let session = Session {
        id,
        emitter: emitter.clone(),
        open_url: Arc::clone(&shared.config.open_url),
        input: shared.input.clone(),
        page,
    };
    let env: Vec<(String, OsString)> = shared.config.cli_env.clone();
    let timeout = shared.config.sign_in_timeout;
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
                Err(e) => session.step(SignInStep::Failed(format!(
                    "Klefki could not start Claude Code to sign in: {}",
                    sanitize(&e.to_string())
                ))),
            }
            // Only now: the terminal is closed and Claude has ended.
            done.store(true, Ordering::SeqCst);
        });
    if spawned.is_err()
        && let Some(running) = &state.sign_in
    {
        running.done.store(true, Ordering::SeqCst);
        emitter.emit(Event::SignIn {
            id: running.id.clone(),
            step: SignInStep::Failed("Klefki could not start the sign-in.".to_owned()),
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
}

impl Session {
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
            Progress::Finished(Outcome::SignedIn) => {
                // Confirm it the way every status is known: Claude's own check.
                let _ = self.input.send(Input::Ui(Command::Check(self.id.clone())));
                SignInStep::SignedIn
            }
            Progress::Finished(Outcome::Failed(why)) => SignInStep::Failed(why),
            Progress::Finished(Outcome::Cancelled) => SignInStep::Cancelled,
            Progress::Finished(Outcome::TimedOut) => SignInStep::Failed(
                "Nothing came back from the browser in time, so the sign-in was stopped. Try again.".to_owned(),
            ),
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

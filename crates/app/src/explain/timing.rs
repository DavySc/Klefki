//! A check that ran out of time, explained: how long it ran, how long Claude Code waits and where that is set, and,
//! once it was given more time, whether it is slow or stuck and how to give it that time in Claude Code.
use super::Explanation;
use klefki_core::api::{
    Hint, LONGER_CHECK, LimitSetIn, ServerRow, Status, TimeLimit, Timing, TransportKind,
    words::duration,
};
use std::time::Duration;

/// The button that gives a server more time, and its hover text.
pub(crate) fn longer_button() -> String {
    format!("Check again with {}", duration(LONGER_CHECK))
}

/// Whether to offer a check given more time: after a timeout, when more time is more than Claude Code gives it and
/// the user could give it that much.
pub(crate) fn offers_longer(timing: &Timing) -> bool {
    timing.given.is_none()
        && timing.limit.limit < LONGER_CHECK
        && !matches!(timing.limit.set_in, LimitSetIn::Organisation(_))
}

/// Where the limit comes from, after its length: `(Claude Code's default; MCP_TIMEOUT is not set)`.
fn source(limit: &TimeLimit) -> String {
    let at = match &limit.set_in {
        LimitSetIn::Default => return "Claude Code's default; MCP_TIMEOUT is not set".to_owned(),
        LimitSetIn::Environment => "the environment Klefki was started with".to_owned(),
        LimitSetIn::File(file) => file.clone(),
        LimitSetIn::Organisation(file) => format!("your organisation's {file}"),
    };
    if limit.unreadable {
        format!(
            "MCP_TIMEOUT in {at} is not a number of milliseconds, so Claude Code uses its default"
        )
    } else {
        format!("MCP_TIMEOUT in {at}")
    }
}

/// `Claude Code waits 30 s for a server to connect (…).`
fn limit_line(limit: &TimeLimit) -> String {
    format!(
        "Claude Code waits {} for a server to connect ({}).",
        duration(limit.limit),
        source(limit)
    )
}

/// A limit with room for a start that took `took`: half as long again, in whole 10 s, in milliseconds.
fn suggested_ms(took: Duration) -> u128 {
    let ms = took.as_millis() * 3 / 2;
    ms.div_ceil(10_000).max(1) * 10_000
}

/// How to give the server `took` in Claude Code.
fn raise_line(limit: &TimeLimit, took: Duration) -> String {
    let ms = suggested_ms(took);
    match (&limit.set_in, &limit.change_in) {
        (LimitSetIn::Organisation(file), _) => format!(
            "Your organisation sets this limit in {file}, so only it can raise it: ask for at least {ms} ms."
        ),
        (_, Some(file)) => format!(
            "To give it that time, set \"MCP_TIMEOUT\": \"{ms}\" under \"env\" in {file}, then start Claude Code again."
        ),
        (_, None) => format!(
            "To give it that time, set \"MCP_TIMEOUT\": \"{ms}\" under \"env\" in Claude Code's settings.json, then start Claude Code again."
        ),
    }
}

fn first_start_line(row: &ServerRow) -> Option<String> {
    (row.transport == TransportKind::Stdio).then(|| {
        "A first run of npx or uvx downloads the server's package before it starts, and a container or a large \
         program can take long to start too."
            .to_owned()
    })
}

impl Explanation {
    /// For a row's live status, with how long its check took. `None` when there is nothing wrong to explain.
    pub(crate) fn for_status_timed(
        row: &ServerRow,
        status: Status,
        detail: Option<&str>,
        hint: Option<&Hint>,
        timing: Option<&Timing>,
    ) -> Option<Self> {
        let Some(timing) = timing else {
            return Self::for_status(row, status, detail, hint);
        };
        let took = duration(timing.took);
        if let Some(given) = timing.given
            && !timing.timed_out
        {
            // It answered, given more time: a warning only when it needed more than a session gives it.
            if timing.took <= timing.limit.limit && status == Status::Connected {
                return None;
            }
            // Connected is a warning here: it works only with more time than a session gives it.
            let mut e = Self::for_status(row, status, detail, hint).unwrap_or_else(|| {
                Self::build(
                    row,
                    status,
                    None,
                    "It needs more time to start than Claude Code gives it",
                    detail,
                )
            });
            if timing.took > timing.limit.limit {
                let mut help = vec![
                    format!(
                        "Given {}, it connected after {took}. {}",
                        duration(given),
                        limit_line(&timing.limit)
                    ),
                    raise_line(&timing.limit, timing.took),
                ];
                if row.transport == TransportKind::Stdio {
                    help.push(
                        "If it was downloading its package on a first run, the next start may be quick enough: \
                         use Check now to see."
                            .to_owned(),
                    );
                }
                help.append(&mut e.help);
                e.help = help;
            }
            return Some(e);
        }
        if !timing.timed_out {
            return Self::for_status(row, status, detail, hint);
        }
        let mut e = Self::for_status(row, status, detail, hint).unwrap_or_else(|| {
            let mut e = Self::build(
                row,
                status,
                None,
                "Claude Code did not answer in time",
                detail,
            );
            e.note = detail.map(|d| format!("Klefki said: {d}"));
            e
        });
        let mut help = Vec::new();
        match timing.given {
            Some(given) => {
                help.push(format!(
                    "Given {}, it still did not connect: it is probably stuck rather than slow.",
                    duration(given)
                ));
                help.push("Diagnose shows what it printed while it waited.".to_owned());
                e.help.clear();
            }
            None => {
                let ran = if status == Status::Unknown {
                    format!("Klefki stopped waiting for Claude Code's answer after {took}.")
                } else {
                    format!("It gave up after {took}.")
                };
                help.push(format!("{ran} {}", limit_line(&timing.limit)));
                help.extend(first_start_line(row));
                if offers_longer(timing) {
                    help.push(format!(
                        "{} tells a server that is slow to start from one that never answers.",
                        longer_button()
                    ));
                } else if let LimitSetIn::Organisation(file) = &timing.limit.set_in {
                    help.push(format!(
                        "Your organisation sets this limit in {file}, so only it can raise it."
                    ));
                }
            }
        }
        help.append(&mut e.help);
        e.help = help;
        Some(e)
    }

    /// A short line for a check given more time that answered within the usual limit: nothing to fix.
    pub(crate) fn timing_note(status: Status, timing: Option<&Timing>) -> Option<String> {
        let timing = timing?;
        (timing.given.is_some() && status == Status::Connected && timing.took <= timing.limit.limit).then(|| {
            format!(
                "It connected after {}, within the {} Claude Code waits: the timeout before was probably a one-off, \
                 such as a first download.",
                duration(timing.took),
                duration(timing.limit.limit)
            )
        })
    }
}

#[cfg(test)]
mod tests;

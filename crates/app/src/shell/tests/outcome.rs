//! The outcome message.

use super::*;
use crate::shell::outcome::{SUCCESS_SECONDS, success_seconds};
use klefki_core::api::{Switch, Undo};

#[test]
fn a_success_fades_and_a_problem_stays() {
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(Event::Applied {
            plan: 1,
            changed: true,
            backup: None,
            notes: Vec::new(),
            undo: None,
        })
        .unwrap();
    h.run();
    h.get_by_label("Saved.");
    for _ in 0..((SUCCESS_SECONDS + 1.0) * 60.0) as usize {
        h.step();
    }
    assert!(
        h.query_by_label("Saved.").is_none(),
        "gone after a few seconds"
    );
    engine
        .events
        .send(Event::ApplyFailed {
            plan: 2,
            error: "Claude Code is writing its configuration right now.".into(),
        })
        .unwrap();
    h.run();
    for _ in 0..((SUCCESS_SECONDS + 1.0) * 60.0) as usize {
        h.step();
    }
    h.get_by_label_contains("writing its configuration");
}

/// Sends the engine's answer that plan 1 was written and changed something.
fn applied(h: &mut Harness<'static, ShellApp>, engine: &klefki_core::api::Detached) {
    engine
        .events
        .send(Event::Applied {
            plan: 1,
            changed: true,
            backup: None,
            notes: Vec::new(),
            undo: None,
        })
        .unwrap();
    h.run();
}

#[test]
fn a_change_that_adds_nothing_offers_no_sign_in_for_the_selected_server() {
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, vec![web_row("web")]);
    click(&mut h, "web");
    applied(&mut h, &engine);
    let outcome = h.state().outcome.as_ref().unwrap();
    assert_eq!(outcome.text, "Saved.");
    assert_eq!(outcome.sign_in, None);
}

#[test]
fn servers_added_that_sign_in_say_to_sign_in_to_it_or_to_each() {
    // Two that can sign in: one sentence for both.
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, vec![web_row("a"), web_row("b")]);
    let state = h.state_mut();
    state.adding = vec![
        ("a".to_owned(), vec![Target::User]),
        ("b".to_owned(), vec![Target::User]),
    ];
    state.adding_verb = "Added";
    state.adding_signs_in = true;
    applied(&mut h, &engine);
    assert_eq!(h.state().selected.len(), 2);
    let outcome = h.state().outcome.as_ref().unwrap();
    assert_eq!(
        outcome.text,
        "Added a and b for the user. Sign in to each from its Status tab."
    );
    assert_eq!(outcome.sign_in, None);
    // One that cannot sign in from the card (a local program): it is told where to, not offered the button.
    let (mut h, engine) = live_harness();
    show_rows(&mut h, &engine, vec![live_row("s", Scope::User, None)]);
    let state = h.state_mut();
    state.adding = vec![("s".to_owned(), vec![Target::User])];
    state.adding_verb = "Added";
    state.adding_signs_in = true;
    applied(&mut h, &engine);
    assert_eq!(h.state().selected.len(), 1);
    let outcome = h.state().outcome.as_ref().unwrap();
    assert_eq!(
        outcome.text,
        "Added s for the user. Sign in to it from its Status tab."
    );
    assert_eq!(outcome.sign_in, None);
}

/// What the engine gives with a change that only switched: gw turned off in a.
fn switched_off() -> Undo {
    Undo {
        switches: vec![Switch {
            project: "/work/a".into(),
            name: "gw".into(),
            off: true,
        }],
        records: Vec::new(),
    }
}

fn steps(h: &mut Harness<'static, ShellApp>, seconds: f64) {
    for _ in 0..(seconds * 60.0) as usize {
        h.step();
    }
}

/// The on/off plan (S2): a change that only switched offers Undo, which stays until used or dismissed (a time limit
/// on a choice excludes whoever reads slowly), and asks the engine for the undo's plan, to review.
#[test]
fn switches_just_saved_offer_undo_until_it_is_used() {
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(Event::Applied {
            plan: 1,
            changed: true,
            backup: None,
            notes: Vec::new(),
            undo: Some(switched_off()),
        })
        .unwrap();
    h.run();
    steps(&mut h, SUCCESS_SECONDS * 3.0);
    h.get_by_label("Saved.");
    drain(&engine);
    click(&mut h, "Undo");
    assert_eq!(
        drain(&engine),
        [Command::Prepare(vec![Change::Undo(switched_off())])]
    );
    assert!(h.state().outcome.is_none());
}

/// Variant: a change that wrote nothing has nothing to undo.
#[test]
fn nothing_changed_offers_no_undo() {
    let (mut h, engine) = live_harness();
    engine
        .events
        .send(Event::Applied {
            plan: 1,
            changed: false,
            backup: None,
            notes: Vec::new(),
            undo: Some(switched_off()),
        })
        .unwrap();
    h.run();
    h.get_by_label("Nothing needed to change.");
    assert!(h.query_by_label("Undo").is_none());
}

/// Found reviewing S1 (#102): the success message fades after 4 s while it now says which sessions to restart and
/// where the backup is. It stays as long as it takes to read slowly.
#[test]
fn a_success_stays_as_long_as_it_takes_to_read() {
    assert_eq!(success_seconds("Saved."), SUCCESS_SECONDS);
    let long = "Saved. Restart Claude Code sessions already open in webapp to pick it up. The previous file is kept \
                in /home/me/.local/share/klefki/backups/claude.json.1761161234-0.bak";
    assert!(success_seconds(long) >= 8.0, "{}", success_seconds(long));
    assert!(success_seconds(&"word ".repeat(500)) <= 20.0);
    let (mut h, engine) = live_harness_at_60fps();
    engine
        .events
        .send(Event::Applied {
            plan: 1,
            changed: true,
            backup: Some(
                "/home/me/.local/share/klefki/backups/claude.json.1761161234-0.bak".into(),
            ),
            notes: Vec::new(),
            undo: None,
        })
        .unwrap();
    h.run();
    let text = h.state().outcome.as_ref().unwrap().text.clone();
    let lasts = success_seconds(&text);
    assert!(lasts > SUCCESS_SECONDS, "{text}");
    assert!(lasts >= SUCCESS_SECONDS + 2.0, "{text}: {lasts}");
    // The harness may let a moment go by while it settles.
    steps(&mut h, SUCCESS_SECONDS + 1.0);
    assert!(
        h.state().outcome.is_some(),
        "still there after the usual time"
    );
    steps(&mut h, lasts);
    assert!(h.state().outcome.is_none(), "gone after its time");
}

/// A success message stays while the pointer is on it: it is being read.
#[test]
fn a_success_stays_while_pointed_at() {
    let (mut h, engine) = live_harness_at_60fps();
    applied(&mut h, &engine);
    h.get_by_label("Saved.").hover();
    steps(&mut h, SUCCESS_SECONDS + 1.0);
    assert!(h.state().outcome.is_some(), "held while pointed at");
}

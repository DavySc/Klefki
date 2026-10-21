//! The outcome message.

use super::*;
use crate::shell::outcome::SUCCESS_SECONDS;

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

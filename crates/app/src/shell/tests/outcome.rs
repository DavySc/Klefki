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

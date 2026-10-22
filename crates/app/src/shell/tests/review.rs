//! The review dialog.

use super::*;

#[test]
fn the_review_leads_with_words_and_its_button_says_what_it_does() {
    let (mut h, engine) = live_harness();
    let mut plan = plan_view(4, &["+ alpha"], &[], false);
    plan.summary = vec!["Turn off alpha in 3 projects".into()];
    plan.action = "Turn off".into();
    plan.sessions = Some(SESSIONS.into());
    engine.events.send(Event::PlanReady(plan)).unwrap();
    h.run();
    h.get_by_label("Turn off alpha in 3 projects");
    h.get_by_label_contains("keep the servers they started with until you restart them");
    assert!(h.query_by_label("Apply").is_none());
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(drain(&engine), vec![Command::Apply(4)]);
    // Pressed again while saving: nothing more is sent.
    click(&mut h, "Turn off");
    assert!(drain(&engine).is_empty());
}

const SESSIONS: &str = "Claude Code sessions already open in a, b and c keep the servers they started with until you \
                        restart them, or switch the same servers there with /mcp. New sessions get this.";

#[test]
fn once_saved_the_outcome_says_again_what_open_sessions_get() {
    for changed in [true, false] {
        let (mut h, engine) = live_harness();
        let mut plan = plan_view(4, &["+ alpha"], &[], false);
        plan.sessions = Some(SESSIONS.into());
        plan.restart =
            Some("Restart Claude Code sessions already open in a, b and c to pick it up.".into());
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        click(&mut h, "Apply");
        engine
            .events
            .send(Event::Applied {
                plan: 4,
                changed,
                backup: None,
                notes: Vec::new(),
                undo: None,
            })
            .unwrap();
        h.run();
        let outcome = h.state().outcome.as_ref().expect("an outcome");
        // Said plainly, not as a warning: nothing went wrong.
        assert!(!outcome.warn);
        assert_eq!(
            outcome
                .text
                .contains("Restart Claude Code sessions already open in a, b and c"),
            changed,
            "only when something was written: {}",
            outcome.text
        );
        // Short enough to read before it fades: the review said it in full.
        assert!(!outcome.text.contains(SESSIONS), "{}", outcome.text);
    }
}

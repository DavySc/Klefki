//! The review dialog.

use super::*;

#[test]
fn the_review_leads_with_words_and_its_button_says_what_it_does() {
    let (mut h, engine) = live_harness();
    let mut plan = plan_view(4, &["+ alpha"], &[], false);
    plan.summary = vec!["Turn off alpha in 3 projects".into()];
    plan.action = "Turn off".into();
    plan.notes =
        vec!["A running Claude Code session may need a restart before it notices this.".into()];
    engine.events.send(Event::PlanReady(plan)).unwrap();
    h.run();
    h.get_by_label("Turn off alpha in 3 projects");
    h.get_by_label_contains("may need a restart");
    assert!(h.query_by_label("Apply").is_none());
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(drain(&engine), vec![Command::Apply(4)]);
    // Pressed again while saving: nothing more is sent.
    click(&mut h, "Turn off");
    assert!(drain(&engine).is_empty());
}

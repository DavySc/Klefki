//! Fix casing: which servers it offers to rename, and to which spelling.

use super::*;

fn renames(engine: &klefki_core::api::Detached) -> Vec<(ServerId, String)> {
    duplicate_requested(engine)
        .into_iter()
        .map(|c| match c {
            Change::Rename { id, name } => (id, name),
            other => panic!("not a rename: {other:?}"),
        })
        .collect()
}

#[test]
fn only_a_server_with_a_casing_collision_offers_the_fix() {
    let (mut h, _engine) = casing_harness(vec![]);
    click(&mut h, "other");
    assert!(h.query_by_label("Fix casing…").is_none());
    click(&mut h, "gateway");
    h.get_by_label("Fix casing…");
}

#[test]
fn the_fix_renames_the_others_to_the_spelling_chosen() {
    let (mut h, engine) = casing_harness(vec![]);
    click(&mut h, "gateway");
    click(&mut h, "Fix casing…");
    // Both spellings are offered; the selected server's own is the starting point.
    h.get_by_label("Keep gateway");
    h.get_by_label("Keep Gateway");
    assert_eq!(h.state().casing.as_ref().unwrap().keep, "gateway");
    click(&mut h, "Review");
    assert!(h.state().casing.is_none());
    let user = h.state().snapshot.servers[0].id();
    assert_eq!(renames(&engine), vec![(user, "gateway".to_owned())]);
}

#[test]
fn choosing_the_other_spelling_renames_the_other_server() {
    let (mut h, engine) = casing_harness(vec![]);
    click(&mut h, "gateway");
    click(&mut h, "Fix casing…");
    click(&mut h, "Keep Gateway");
    click(&mut h, "Review");
    let local = h.state().snapshot.servers[1].id();
    assert_eq!(renames(&engine), vec![(local, "Gateway".to_owned())]);
}

#[test]
fn a_project_server_cannot_be_renamed_so_its_spelling_is_the_one_kept() {
    let mut project = live_row("GATEWAY", Scope::Project, Some("C:/Repos/WebApp"));
    project.casing_collision = true;
    project.approval = Some(Approval::Approved);
    let (mut h, engine) = casing_harness(vec![project]);
    click(&mut h, "gateway");
    click(&mut h, "Fix casing…");
    assert_eq!(h.state().casing.as_ref().unwrap().keep, "GATEWAY");
    assert!(
        h.get_by_label("Keep gateway")
            .accesskit_node()
            .is_disabled()
    );
    click(&mut h, "Review");
    let ids: Vec<ServerId> = renames(&engine).into_iter().map(|(id, _)| id).collect();
    let rows = &h.state().snapshot.servers;
    assert_eq!(ids, vec![rows[0].id(), rows[1].id()]);
}

#[test]
fn two_project_servers_that_differ_cannot_be_fixed_from_here() {
    let mut one = live_row("GATEWAY", Scope::Project, Some("C:/Repos/WebApp"));
    let mut two = live_row("Gateway", Scope::Project, Some("C:/Repos/Other"));
    one.casing_collision = true;
    two.casing_collision = true;
    let (mut h, engine) = casing_harness(vec![one, two]);
    click(&mut h, "gateway");
    click(&mut h, "Fix casing…");
    h.get_by_label_contains(".mcp.json");
    assert!(h.get_by_label("Review").accesskit_node().is_disabled());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().casing.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

#[test]
fn cancelling_the_fix_asks_for_nothing() {
    let (mut h, engine) = casing_harness(vec![]);
    click(&mut h, "gateway");
    click(&mut h, "Fix casing…");
    click(&mut h, "Cancel");
    assert!(h.state().casing.is_none());
    assert!(duplicate_requested(&engine).is_empty());
}

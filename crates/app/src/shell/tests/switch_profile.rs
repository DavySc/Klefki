//! Switching a project from one profile to another (slice S4 of the on/off plan): on the profile's card, and from the
//! list seen from that project.

use super::*;
use egui::accesskit::Role;

const NOTES: &str = "/home/notes";

/// Work (in use in notes) and Notes, a second profile with beta.
fn two_profiles() -> klefki_core::api::Profiles {
    let mut profiles = work_profile();
    profiles.profiles.push(klefki_core::api::Profile {
        name: "Notes".to_owned(),
        members: vec![id_of("beta", Scope::User, None)],
    });
    profiles
}

/// The profiles harness with Work in use in notes, and the profiles in the snapshot as the engine gives them.
fn work_in_use(h: &mut Harness<'static, ShellApp>, engine: &klefki_core::api::Detached) {
    let klefki_core::api::Event::Snapshot(snapshot) = snapshot_with_work_in_use_in_notes() else {
        unreachable!()
    };
    let mut snapshot = (*snapshot).clone();
    snapshot.profiles = two_profiles().profiles;
    engine
        .events
        .send(klefki_core::api::Event::Snapshot(Arc::new(snapshot)))
        .unwrap();
    h.run();
}

fn switch_to_notes(only: bool) -> Change {
    Change::SwitchProfile {
        from: vec!["Work".to_owned()],
        name: "Notes".to_owned(),
        members: two_profiles().profiles[1].members.clone(),
        project: NOTES.to_owned(),
        only,
    }
}

/// The audit's finding 9: using a profile over another stacked them, and nothing said so. Chosen where another is in
/// use, the card switches from it as one change, or uses this one as well.
#[test]
fn a_profile_card_switches_from_the_one_in_use_there_or_uses_it_as_well() {
    let (mut h, engine) = profiles_harness();
    work_in_use(&mut h, &engine);
    open_profiles(&mut h, &engine, two_profiles());
    h.state_mut().profiles.as_mut().unwrap().filter = "Notes".to_owned();
    h.run();
    // Nowhere chosen, or a project where none is in use: Use, as before.
    h.get_by_label("Use profile…");
    assert!(h.query_by_label_contains("Switch from").is_none());
    h.get_by_role_and_label(Role::ComboBox, "Use in").click();
    h.run();
    h.get_by_label_contains("app/work").click();
    h.run();
    h.get_by_label("Use profile…");
    h.get_by_role_and_label(Role::ComboBox, "Use in").click();
    h.run();
    h.get_by_label_contains("notes").click();
    h.run();
    h.get_by_label("Work in use in notes.");
    assert!(h.query_by_label("Use profile…").is_none());
    click(&mut h, "Switch from Work…");
    assert_eq!(
        sent(&engine),
        [Command::Prepare(vec![switch_to_notes(true)])]
    );
    h.state_mut().preparing = false;
    h.get_by_role_and_label(Role::CheckBox, "Only these servers there")
        .click();
    h.run();
    click(&mut h, "Switch from Work…");
    assert_eq!(
        sent(&engine),
        [Command::Prepare(vec![switch_to_notes(false)])]
    );
    h.state_mut().preparing = false;
    click(&mut h, "Use as well…");
    assert!(matches!(
        &sent(&engine)[..],
        [Command::Prepare(c)] if matches!(&c[..], [Change::UseProfile { project: Some(p), only: false, .. }] if p == NOTES)
    ));
    // Every project: no one profile to switch from.
    h.state_mut().preparing = false;
    h.get_by_role_and_label(Role::ComboBox, "Use in").click();
    h.run();
    click(&mut h, "Every project");
    h.get_by_label("Use profile…");
    assert!(h.query_by_label_contains("Switch from").is_none());
}

/// The project lens (S3) says which profile is in use there, and switches to another, uses one, or stops it, each
/// after a review: it can turn many servers off at once.
#[test]
fn the_list_seen_from_a_project_names_its_profile_and_switches_it() {
    let (mut h, engine) = profiles_harness();
    work_in_use(&mut h, &engine);
    // Every project: no profile menu.
    assert!(h.query_by_label_contains("Work").is_none());
    h.state_mut().look_in(Some(NOTES.to_owned()));
    h.run();
    sent(&engine);
    h.get_by_label_contains("Work").click();
    h.run();
    click(&mut h, "Switch to Notes");
    assert_eq!(
        sent(&engine),
        [Command::Prepare(vec![switch_to_notes(true)])]
    );
    assert!(h.state().preparing, "reviewed, not saved at once");
    h.state_mut().preparing = false;
    h.get_by_label_contains("Work").click();
    h.run();
    click(&mut h, "Stop using Work");
    assert_eq!(
        sent(&engine),
        [Command::Prepare(vec![Change::StopProfile {
            name: "Work".to_owned(),
            project: Some(NOTES.to_owned()),
            delete: false,
        }])]
    );
    // Where none is in use: says so, and offers each profile to use.
    h.state_mut().preparing = false;
    h.state_mut().look_in(Some("/work/app".to_owned()));
    h.run();
    sent(&engine);
    h.get_by_label_contains("No profile").click();
    h.run();
    assert!(h.query_by_label("Stop using Work").is_none());
    click(&mut h, "Use Notes");
    assert!(matches!(
        &sent(&engine)[..],
        [Command::Prepare(c)] if matches!(&c[..], [Change::SwitchProfile { from, project, only: true, .. }] if from.is_empty() && project == "/work/app")
    ));
}

/// No profiles: nothing about them in the lens.
#[test]
fn no_profiles_no_profile_menu() {
    let (mut h, _engine) = profiles_harness();
    h.state_mut().look_in(Some(NOTES.to_owned()));
    h.run();
    assert!(h.query_by_label_contains("No profile").is_none());
}

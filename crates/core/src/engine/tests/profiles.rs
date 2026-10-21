//! Engine tests: profiles.
use super::*;

#[test]
fn a_renamed_or_moved_server_stays_in_its_profiles() {
    let env = quiet_env(&serde_json::json!({
        "mcpServers": {"gw": {"command": "gw"}, "db": {"command": "db"}, "other": {"command": "o"}},
        "projects": {"/work/a": {}}
    }));
    let backups = env.dir.path().join("backups");
    crate::profiles::save(
        &backups,
        "Work",
        &[user_id("gw").connection, user_id("db").connection],
        None,
    )
    .unwrap();
    let core = env.start();
    let plan = plan_for(&core, vec![rename(user_id("gw"), "gateway")]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "gw is in the profile Work, which keeps it where it goes."),
        "{:?}",
        plan.notes
    );
    apply_and_wait(&core, &plan);
    let members = || crate::profiles::load(&backups).profiles[0].members.clone();
    assert_eq!(
        members(),
        [user_id("gateway").connection, user_id("db").connection]
    );
    // Moved to a project: the same, under its new place.
    let plan = plan_for(
        &core,
        vec![Change::Move {
            id: user_id("db"),
            to: crate::model::Target::Local("/work/a".into()),
        }],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(
        members(),
        [
            user_id("gateway").connection,
            local_id("db", "/work/a").connection
        ]
    );
    // A server in no profile leaves the file as it is, and the review says nothing of profiles.
    let before = std::fs::read_to_string(crate::profiles::path(&backups)).unwrap();
    let plan = plan_for(&core, vec![rename(user_id("other"), "other2")]);
    assert!(
        !plan.notes.iter().any(|n| n.contains("profile")),
        "{:?}",
        plan.notes
    );
    apply_and_wait(&core, &plan);
    assert_eq!(
        std::fs::read_to_string(crate::profiles::path(&backups)).unwrap(),
        before
    );
}

fn off_in(env: &Env, project: &str) -> serde_json::Value {
    config_json(env)["projects"][project]["disabledMcpServers"].clone()
}

fn last_snapshot(events: &[Event]) -> Arc<Snapshot> {
    Arc::clone(
        events
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::Snapshot(s) => Some(s),
                _ => None,
            })
            .expect("a snapshot"),
    )
}

/// The owner's case (2026-10-20): a profile turned memory off in Designs, where it was on like everywhere else.
/// Stopping the profile, or deleting it with "Stop using and delete", puts memory back on there.
#[test]
fn stopping_a_profile_puts_back_what_it_switched() {
    for delete in [false, true] {
        let env = quiet_env(&designs_config());
        let backups = env.dir.path().join("backups");
        crate::profiles::save(
            &backups,
            "Writing",
            &[user_id("gw").connection, user_id("db").connection],
            None,
        )
        .unwrap();
        let core = env.start();
        let plan = plan_for(
            &core,
            vec![use_profile(
                "Writing",
                &["gw", "db"],
                Some("/work/designs"),
                true,
            )],
        );
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("Klefki remembers what this switches")),
            "{:?}",
            plan.notes
        );
        let events = apply_and_wait(&core, &plan);
        assert_eq!(off_in(&env, "/work/designs"), serde_json::json!(["memory"]));
        assert_eq!(
            crate::ops::profile_in_use("Writing", &last_snapshot(&events)),
            ["/work/designs"]
        );
        let plan = plan_for(&core, vec![stop_profile("Writing", None, delete)]);
        let rest = if delete { ", and delete it" } else { "" };
        assert_eq!(
            plan.summary,
            [format!("Stop using the profile Writing in designs{rest}")]
        );
        assert!(
            plan.notes
                .iter()
                .any(|n| n == "memory goes back on in designs, as before Writing was used."),
            "{:?}",
            plan.notes
        );
        let events = apply_and_wait(&core, &plan);
        assert_eq!(off_in(&env, "/work/designs"), serde_json::json!([]));
        // The rest of the project entry is as it was.
        assert_eq!(
            config_json(&env)["projects"]["/work/designs"]["allowedTools"],
            serde_json::json!([])
        );
        assert!(crate::profile_uses::load(&backups).of("Writing").is_empty());
        assert!(crate::ops::profile_in_use("Writing", &last_snapshot(&events)).is_empty());
        let left = crate::profiles::load(&backups).profiles;
        assert_eq!(left.is_empty(), delete, "{left:?}");
        if delete {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, Event::Profiles(p) if p.profiles.is_empty()))
            );
        }
    }
}

/// "As before" is not "as the backup was": what was switched since the profile was used stays, and is named.
#[test]
fn a_switch_changed_since_a_profile_was_used_is_left_and_named() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(
        off_in(&env, "/work/designs"),
        serde_json::json!(["memory", "db"])
    );
    // memory is turned back on by hand (or by Claude Code's /mcp) in the meantime, once Klefki's check of its
    // own write is done.
    verify_passes(&core);
    let mut now = config_json(&env);
    now["projects"]["/work/designs"]["disabledMcpServers"] = serde_json::json!(["db"]);
    write_config(&env, &now);
    let plan = plan_for(
        &core,
        vec![stop_profile("Writing", Some("/work/designs"), false)],
    );
    assert!(
        plan.warnings.iter().any(|w| w
            == "memory in designs was switched since Writing was used, so it is left as it is."),
        "{:?}",
        plan.warnings
    );
    apply_and_wait(&core, &plan);
    assert_eq!(off_in(&env, "/work/designs"), serde_json::json!([]));
    // Variant: memory turned on and off again since is as the profile left it, so it goes back.
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    let plan = plan_for(
        &core,
        vec![use_profile(
            "Writing",
            &["gw", "memory"],
            Some("/work/designs"),
            true,
        )],
    );
    apply_and_wait(&core, &plan);
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    let plan = plan_for(&core, vec![stop_profile("Writing", None, false)]);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    apply_and_wait(&core, &plan);
    assert_eq!(off_in(&env, "/work/designs"), serde_json::json!([]));
}

/// A switch the user changed through Klefki after a use is theirs: turned on and off again, it is not put back on
/// by Stop, and the change that switches it says so (found in review, PR #63).
#[test]
fn a_switch_changed_through_klefki_since_is_no_longer_the_profiles() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    let switch = |on: bool| Change::SetEnabled {
        id: user_id("memory"),
        enabled: on,
    };
    let plan = plan_for(&core, vec![switch(true)]);
    assert!(
        plan.notes.iter().any(|n| n
            == "Writing switched memory in designs; after this, stopping Writing leaves it as you set it."),
        "{:?}",
        plan.notes
    );
    apply_and_wait(&core, &plan);
    let plan = plan_for(&core, vec![switch(false)]);
    apply_and_wait(&core, &plan);
    let plan = plan_for(&core, vec![stop_profile("Writing", None, false)]);
    assert!(
        !plan.notes.iter().any(|n| n.starts_with("memory")),
        "{:?}",
        plan.notes
    );
    apply_and_wait(&core, &plan);
    // db goes back on; memory stays off as the user last set it.
    assert_eq!(off_in(&env, "/work/designs"), serde_json::json!(["memory"]));
}

/// Variant: a Restore that lands on the profile's state is the user's choice too.
#[test]
fn a_restore_since_a_profile_was_used_is_not_put_back() {
    let env = quiet_env(&designs_config());
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    // Another change keeps a backup with memory and db off, as the profile left them.
    let plan = plan_for(
        &core,
        vec![Change::SetEnabled {
            id: user_id("gw"),
            enabled: false,
        }],
    );
    applied(&core, &plan);
    let backup = newest_backup(&core);
    verify_passes(&core);
    // memory on by hand, then the backup restored: memory is off again, by the restore.
    let mut now = config_json(&env);
    now["projects"]["/work/designs"]["disabledMcpServers"] = serde_json::json!(["db", "gw"]);
    write_config(&env, &now);
    let plan = plan_for(&core, vec![Change::Restore { backup }]);
    apply_and_wait(&core, &plan);
    assert_eq!(
        off_in(&env, "/work/designs"),
        serde_json::json!(["db", "memory"])
    );
    let plan = plan_for(&core, vec![stop_profile("Writing", None, false)]);
    apply_and_wait(&core, &plan);
    assert_eq!(off_in(&env, "/work/designs"), serde_json::json!(["memory"]));
}

/// A record left under a profile name (a failed forget, an older Klefki, `profiles.json` changed by hand) is
/// dropped when a profile is renamed onto that name, so the file stays readable and the profile keeps its own
/// record; a new profile under that name starts with none (found in review, PR #63).
#[test]
fn a_profile_saved_under_a_leftover_name_does_not_lock_or_take_over_its_record() {
    let env = quiet_env(&designs_config());
    let backups = env.dir.path().join("backups");
    crate::profiles::save(&backups, "A", &[user_id("gw").connection], None).unwrap();
    crate::profile_uses::record(
        &backups,
        "B",
        &[crate::profile_uses::Switch {
            project: "/work/notes".into(),
            name: "db".into(),
            off: true,
        }],
        &[],
    )
    .unwrap();
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("A", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    core.send(Command::SaveProfile {
        name: "B".to_owned(),
        members: vec![user_id("gw").connection],
        replaces: Some("A".to_owned()),
    });
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let uses = &last_snapshot(&events).profile_uses;
    assert_eq!(uses.problem, None);
    assert_eq!(uses.of("B").len(), 2, "{uses:?}");
    assert!(
        uses.of("B").iter().all(|s| s.project == "/work/designs"),
        "{uses:?}"
    );
    // Variant: a new profile under a name with a leftover record starts with none.
    crate::profile_uses::record(
        &backups,
        "C",
        &[crate::profile_uses::Switch {
            project: "/work/notes".into(),
            name: "db".into(),
            off: true,
        }],
        &[],
    )
    .unwrap();
    core.send(Command::SaveProfile {
        name: "C".to_owned(),
        members: vec![user_id("gw").connection],
        replaces: None,
    });
    let events = collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    assert!(last_snapshot(&events).profile_uses.of("C").is_empty());
}

/// Two profiles one after another in one project end as before both, stopped in either order.
#[test]
fn profiles_used_one_after_another_end_as_before_both() {
    for writing_first in [true, false] {
        let env = quiet_env(&designs_config());
        let backups = env.dir.path().join("backups");
        let core = env.start();
        // Writing: gw alone, so memory and db go off. Research: gw and db alone, so db comes back on; memory is
        // off already, as Writing set it.
        let plan = plan_for(
            &core,
            vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
        );
        apply_and_wait(&core, &plan);
        let plan = plan_for(
            &core,
            vec![use_profile(
                "Research",
                &["gw", "db"],
                Some("/work/designs"),
                true,
            )],
        );
        apply_and_wait(&core, &plan);
        assert_eq!(off_in(&env, "/work/designs"), serde_json::json!(["memory"]));
        let (first, second) = if writing_first {
            ("Writing", "Research")
        } else {
            ("Research", "Writing")
        };
        let plan = plan_for(&core, vec![stop_profile(first, None, false)]);
        if writing_first {
            // Research wants memory off too, and switched db later: both are left as Research has them.
            for said in [
                "memory stays as it is in designs: the profile Research switched it that way too.",
                "db in designs is left as the profile Research switched it later.",
            ] {
                assert!(
                    plan.notes.iter().any(|n| n == said),
                    "{said}: {:?}",
                    plan.notes
                );
            }
            apply_and_wait(&core, &plan);
            assert_eq!(off_in(&env, "/work/designs"), serde_json::json!(["memory"]));
        } else {
            // Back to Writing's: db goes off again, memory stays off as Writing has it.
            apply_and_wait(&core, &plan);
            assert_eq!(
                off_in(&env, "/work/designs"),
                serde_json::json!(["memory", "db"])
            );
        }
        let plan = plan_for(&core, vec![stop_profile(second, None, false)]);
        apply_and_wait(&core, &plan);
        assert_eq!(
            off_in(&env, "/work/designs"),
            serde_json::json!([]),
            "writing first: {writing_first}"
        );
        assert_eq!(
            crate::profile_uses::load(&backups),
            crate::profile_uses::ProfileUses::default()
        );
    }
}

/// Used in every project, then stopped in one: only that one goes back. A renamed server and a renamed profile
/// keep what was switched; a project with nothing recorded is refused, saying why.
#[test]
fn stopping_in_one_project_after_renames() {
    let env = quiet_env(&designs_config());
    let backups = env.dir.path().join("backups");
    crate::profiles::save(&backups, "Writing", &[user_id("gw").connection], None).unwrap();
    let core = env.start();
    let plan = plan_for(&core, vec![use_profile("Writing", &["gw"], None, true)]);
    apply_and_wait(&core, &plan);
    let plan = plan_for(&core, vec![rename(user_id("memory"), "mem")]);
    apply_and_wait(&core, &plan);
    core.send(Command::SaveProfile {
        name: "Words".to_owned(),
        members: vec![user_id("gw").connection],
        replaces: Some("Writing".to_owned()),
    });
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    let plan = plan_for(
        &core,
        vec![stop_profile("Words", Some("/work/notes"), false)],
    );
    apply_and_wait(&core, &plan);
    assert_eq!(off_in(&env, "/work/notes"), serde_json::json!([]));
    let designs = off_in(&env, "/work/designs");
    assert!(
        designs
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("mem")),
        "{designs}"
    );
    let why = plan_failure(
        &core,
        vec![stop_profile("Words", Some("/work/notes"), false)],
    );
    assert!(
        why.contains("no record of Words switching anything in notes"),
        "{why}"
    );
    let plan = plan_for(&core, vec![stop_profile("Words", None, false)]);
    apply_and_wait(&core, &plan);
    assert_eq!(off_in(&env, "/work/designs"), serde_json::json!([]));
}

/// Delete only forgets what the profile switched, and leaves it switched.
#[test]
fn deleting_only_leaves_the_switches_and_forgets_them() {
    let env = quiet_env(&designs_config());
    let backups = env.dir.path().join("backups");
    crate::profiles::save(&backups, "Writing", &[user_id("gw").connection], None).unwrap();
    let core = env.start();
    let plan = plan_for(
        &core,
        vec![use_profile("Writing", &["gw"], Some("/work/designs"), true)],
    );
    apply_and_wait(&core, &plan);
    core.send(Command::DeleteProfile("Writing".to_owned()));
    collect(&core, |e| e.iter().any(|x| matches!(x, Event::Snapshot(_))));
    assert_eq!(
        off_in(&env, "/work/designs"),
        serde_json::json!(["memory", "db"])
    );
    assert!(crate::profile_uses::load(&backups).of("Writing").is_empty());
}

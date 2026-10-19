//! The Activity log page: every change Klefki saved, newest first, with the backup it kept before each one.
//! Asked of the engine each time the page is opened, and let go when it is left, so it costs nothing at start-up
//! and holds no memory while not shown.
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Activity, ActivityEntry, Backup};

/// How long "Copied" shows after a path is copied, in seconds.
const COPIED_SECONDS: f64 = 2.0;

#[derive(Default)]
pub struct ActivityPage {
    /// `None` until the engine answers.
    pub log: Option<Activity>,
    /// What the top bar's filter box holds on this page: it filters the changes, not the hidden list of servers.
    pub filter: String,
    /// The path last copied, and when.
    copied: Option<(String, f64)>,
}

impl ActivityPage {
    /// Draws the page. Returns the backup the user asked to restore, for the shell to have reviewed.
    pub fn show(&mut self, ui: &mut Ui) -> Option<String> {
        let pal = theme::palette(ui.ctx());
        ui.add_space(8.0);
        ui.label(RichText::new("Activity log").heading());
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Changes Klefki saved, newest first. Before each one it keeps a copy of every file it \
                     changes; it keeps the newest 20 copies of each file. Restoring a copy of .claude.json \
                     shows what it would change before anything is saved.",
                )
                .color(pal.text_muted),
            )
            .wrap(),
        );
        ui.add_space(12.0);
        let Some(log) = &self.log else {
            // No spinner: reading takes a moment, and a spinner would keep the window repainting.
            ui.label(RichText::new("Loading…").color(pal.text_muted));
            return None;
        };
        if let Some(problem) = &log.problem {
            ui.label(RichText::new(format!("{} {problem}", icon::WARNING)).color(pal.warn));
            ui.add_space(8.0);
        }
        if log.entries.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(icon::LIST_BULLETS)
                        .size(32.0)
                        .color(pal.text_muted),
                );
                ui.add_space(8.0);
                ui.label("No changes yet");
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "When you save a change in Klefki, it shows up here with the backup taken first.",
                        )
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
            });
            return None;
        }
        let now = ui.input(|i| i.time);
        if self
            .copied
            .as_ref()
            .is_some_and(|(_, at)| now - at >= COPIED_SECONDS)
        {
            self.copied = None;
        }
        let query = self.filter.trim().to_lowercase();
        let shown = |e: &&ActivityEntry| query.is_empty() || matches(e, &query);
        // Changes first; backups no logged change mentions (kept before the log existed) after them, folded away.
        let changes: Vec<&ActivityEntry> = log
            .entries
            .iter()
            .filter(|e| !e.summary.is_empty())
            .filter(shown)
            .collect();
        let others: Vec<&ActivityEntry> = log
            .entries
            .iter()
            .filter(|e| e.summary.is_empty())
            .filter(shown)
            .collect();
        let copied = self.copied.as_ref().map(|(path, _)| path.as_str());
        let mut asked = Asked::default();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if changes.is_empty() && others.is_empty() {
                    ui.label(
                        RichText::new(format!("No changes match \"{}\".", self.filter.trim()))
                            .color(pal.text_muted),
                    );
                }
                for (i, entry) in changes.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    asked.or(row(ui, entry, copied, true));
                }
                if !others.is_empty() {
                    ui.add_space(12.0);
                    egui::CollapsingHeader::new(format!("Other backups ({})", others.len()))
                        .id_salt("other-backups")
                        // Open while filtering, so a match is not hidden.
                        .open((!query.is_empty()).then_some(true))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "Copies Klefki kept that no change in this log mentions, such as ones \
                                         from before the log existed.",
                                    )
                                    .color(pal.text_muted),
                                )
                                .wrap(),
                            );
                            for entry in &others {
                                asked.or(row(ui, entry, copied, false));
                            }
                        });
                }
            });
        if let Some(path) = asked.copy {
            ui.ctx().copy_text(path.clone());
            self.copied = Some((path, now));
        }
        if let Some((_, at)) = &self.copied {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(
                    (COPIED_SECONDS - (now - at)).max(0.0),
                ));
        }
        asked.restore
    }
}

/// What a row's buttons asked for.
#[derive(Default)]
struct Asked {
    /// A path to copy.
    copy: Option<String>,
    /// A backup to restore.
    restore: Option<String>,
}

impl Asked {
    fn or(&mut self, other: Asked) {
        self.copy = other.copy.or(self.copy.take());
        self.restore = other.restore.or(self.restore.take());
    }
}

/// Whether `entry` has `query` (lower case) in what it says, its time or a backup's path.
fn matches(entry: &ActivityEntry, query: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(query);
    entry.summary.iter().any(|s| has(s))
        || entry.failed.as_deref().is_some_and(has)
        || has(&entry.when)
        || entry.backups.iter().any(|b| has(&b.path) || has(&b.of))
}

/// One change, or with `change` false, a backup no change mentions. Returns what its buttons asked for.
fn row(ui: &mut Ui, entry: &ActivityEntry, copied: Option<&str>, change: bool) -> Asked {
    let pal = theme::palette(ui.ctx());
    let mut asked = Asked::default();
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        // Wide enough for "Yesterday 23:59" and "13 Oct 2025 18:40", so the changes line up.
        ui.allocate_ui_with_layout(
            egui::vec2(150.0, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(150.0);
                ui.label(RichText::new(&entry.when).color(pal.text_muted))
                    .on_hover_text(&entry.exact);
            },
        );
        ui.vertical(|ui| {
            for line in &entry.summary {
                ui.add(egui::Label::new(line.as_str()).wrap());
            }
            if let Some(why) = &entry.failed {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} Did not finish: {why}", icon::WARNING))
                            .color(pal.warn),
                    )
                    .wrap(),
                );
            }
            for backup in &entry.backups {
                asked.or(backup_line(ui, backup, copied, change));
            }
            if entry.more_backups > 0 {
                ui.label(
                    RichText::new(format!(
                        "…and {} more backups, under Other backups",
                        entry.more_backups
                    ))
                    .small()
                    .color(pal.text_muted),
                );
            }
        });
    });
    ui.add_space(6.0);
    asked
}

/// What a backup is, with a button that copies where it is; the path itself shows on hover, so it does not take
/// over the page. A copy of `.claude.json` can be restored: of a change, to how things were before it (`change`).
fn backup_line(ui: &mut Ui, backup: &Backup, copied: Option<&str>, change: bool) -> Asked {
    let pal = theme::palette(ui.ctx());
    if !backup.kept {
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "Backup of {} no longer kept: Klefki keeps the newest 20.",
                    backup.of
                ))
                .small()
                .color(pal.text_muted),
            )
            .wrap(),
        );
        return Asked::default();
    }
    let mut asked = Asked::default();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("Backup of {}", backup.of))
                .small()
                .color(pal.text_muted),
        );
        if ui
            .small_button(format!("{} Copy path", icon::COPY))
            .on_hover_text(backup.path.as_str())
            .clicked()
        {
            asked.copy = Some(backup.path.clone());
        }
        if backup.of == ".claude.json" {
            let label = if change {
                "Restore to before this…"
            } else {
                "Restore this copy…"
            };
            if ui
                .small_button(format!("{} {label}", icon::CLOCK_COUNTER_CLOCKWISE))
                .on_hover_text(
                    "Shows which servers and switches would go back to how this copy has them, and asks before \
                     saving anything. Servers added since are kept, and Klefki keeps a copy of how things are now.",
                )
                .clicked()
            {
                asked.restore = Some(backup.path.clone());
            }
        }
    });
    // What was copied, in the open: hover text is out of reach of the keyboard.
    if copied == Some(backup.path.as_str()) {
        ui.add(
            egui::Label::new(
                RichText::new(format!("Copied {}", backup.path))
                    .small()
                    .color(pal.success),
            )
            .wrap(),
        );
    }
    asked
}

#[cfg(test)]
mod tests {
    use crate::shell::{Page, ShellApp};
    use crate::theme::ThemeChoice;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use klefki_core::api::{
        Activity, ActivityEntry, Backup, Change, Command, Core, Detached, Event, Snapshot,
    };
    use std::sync::Arc;

    fn harness() -> (Harness<'static, ShellApp>, Detached) {
        let (core, engine) = Core::detached();
        let h = Harness::builder()
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(move |cc| {
                crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
                ShellApp::with_core(core, ThemeChoice::Dark)
            });
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot::default())))
            .unwrap();
        (h, engine)
    }

    fn activity_asked(engine: &Detached) -> usize {
        std::iter::from_fn(|| engine.commands.try_recv().ok())
            .filter(|c| *c == Command::Activity)
            .count()
    }

    fn open(h: &mut Harness<'static, ShellApp>) {
        h.get_by_label("Activity log").click();
        h.run();
    }

    fn backup(path: &str, kept: bool) -> Backup {
        Backup {
            of: ".claude.json".to_owned(),
            path: path.to_owned(),
            kept,
        }
    }

    fn sample() -> Activity {
        Activity {
            entries: vec![
                ActivityEntry {
                    when: "Today 10:44".to_owned(),
                    exact: "19 Oct 2026, 10:44:29 (UTC+02:00)".to_owned(),
                    summary: vec!["Turn off alpha in 2 projects".to_owned()],
                    failed: None,
                    backups: vec![backup("/data/backups/claude.json.1-0.bak", true)],
                    more_backups: 0,
                },
                ActivityEntry {
                    when: "Yesterday 18:02".to_owned(),
                    exact: "18 Oct 2026, 18:02:11 (UTC+02:00)".to_owned(),
                    summary: vec!["Move beta to user".to_owned()],
                    failed: Some("Claude Code already added beta to user.".to_owned()),
                    backups: vec![backup("/data/backups/claude.json.0-0.bak", false)],
                    more_backups: 0,
                },
                ActivityEntry {
                    when: "13 Oct 09:12".to_owned(),
                    exact: "13 Oct 2026, 09:12:40 (UTC+02:00)".to_owned(),
                    summary: Vec::new(),
                    failed: None,
                    backups: vec![backup("/data/backups/claude.json.9-0.bak", true)],
                    more_backups: 0,
                },
            ],
            problem: None,
        }
    }

    #[test]
    fn the_log_is_read_only_once_the_page_is_opened() {
        let (mut h, engine) = harness();
        h.run();
        assert_eq!(activity_asked(&engine), 0, "nothing is read at start-up");
        open(&mut h);
        assert_eq!(h.state().page, Page::ActivityLog);
        assert_eq!(activity_asked(&engine), 1);
        h.get_by_label("Loading…");
        // Drawing the page again asks nothing more.
        h.run();
        assert_eq!(activity_asked(&engine), 0);
    }

    #[test]
    fn changes_show_with_when_what_and_their_backup() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        h.get_by_label("Today 10:44");
        h.get_by_label("Turn off alpha in 2 projects");
        h.get_by_label_contains("Did not finish: Claude Code already added beta");
        h.get_by_label("Backup of .claude.json no longer kept: Klefki keeps the newest 20.");
        // The path shows on hover and is copied by its button; it does not take over the page (cross-review U2).
        assert!(h.query_by_label_contains("/data/backups/").is_none());
        assert_eq!(h.get_all_by_label_contains("Copy path").count(), 1);
        assert!(h.query_by_label("Loading…").is_none());
        assert!(h.query_by_label_contains("more backups").is_none());
        let mut log = sample();
        log.entries[0].more_backups = 6;
        engine.events.send(Event::Activity(log)).unwrap();
        h.run();
        h.get_by_label("…and 6 more backups, under Other backups");
    }

    #[test]
    fn a_backup_path_can_be_copied() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        h.get_by_label_contains("Copy path").click();
        // The frame that sees the click copies.
        h.step();
        let copied: Vec<_> = h
            .output()
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(copied, ["/data/backups/claude.json.1-0.bak"]);
        h.run();
        // What was copied shows in the open, not only on hover (cross-review R3).
        h.get_by_label("Copied /data/backups/claude.json.1-0.bak");
        for _ in 0..30 {
            h.step();
        }
        h.run();
        assert!(
            h.query_by_label_contains("Copied").is_none(),
            "the note goes away"
        );
    }

    fn prepared(engine: &Detached) -> Vec<Vec<Change>> {
        std::iter::from_fn(|| engine.commands.try_recv().ok())
            .filter_map(|c| match c {
                Command::Prepare(changes) => Some(changes),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_kept_backup_of_claude_json_can_be_restored_after_review() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        let mut log = sample();
        log.entries[0].backups.push(Backup {
            of: "a project's settings.local.json".to_owned(),
            path: "/data/backups/settings.local-0123.1-0.bak".to_owned(),
            kept: true,
        });
        engine.events.send(Event::Activity(log)).unwrap();
        h.run();
        prepared(&engine);
        // Only for a copy of .claude.json that is still kept: not the gone one, not a project's settings.
        let [button] = &h
            .get_all_by_label_contains("Restore to before this…")
            .collect::<Vec<_>>()[..]
        else {
            panic!("one restore button");
        };
        button.click();
        h.run();
        assert_eq!(
            prepared(&engine),
            [vec![Change::Restore {
                backup: "/data/backups/claude.json.1-0.bak".to_owned()
            }]],
            "the review comes first; nothing is written yet"
        );
        // A backup no change mentions is a copy as it was then.
        h.get_by_label("Other backups (1)").click();
        h.run();
        h.get_by_label_contains("Restore this copy…").click();
        h.run();
        // The first review is still on its way: one review at a time.
        assert!(prepared(&engine).is_empty());
    }

    #[test]
    fn backups_no_change_mentions_are_folded_away_after_the_changes() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        let changes = h.get_by_label("Move beta to user").rect();
        let others = h.get_by_label("Other backups (1)").rect();
        assert!(others.top() > changes.bottom());
        assert!(h.query_by_label("13 Oct 09:12").is_none(), "folded");
        h.get_by_label("Other backups (1)").click();
        h.run();
        h.get_by_label("13 Oct 09:12");
        h.get_by_label_contains("no change in this log mentions");
        assert_eq!(h.get_all_by_label_contains("Copy path").count(), 2);
    }

    #[test]
    fn the_filter_box_filters_the_changes_on_this_page() {
        let (mut h, engine) = harness();
        h.run();
        let says = |h: &Harness<'static, ShellApp>, what: &str| {
            h.query_by(|n| n.placeholder().is_some_and(|p| p.contains(what)))
                .is_some()
        };
        assert!(says(&h, "Filter"));
        assert!(!says(&h, "Filter changes"), "the servers' filter elsewhere");
        open(&mut h);
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        assert!(says(&h, "Filter changes"));
        h.state_mut().activity.as_mut().unwrap().filter = "BETA".into();
        h.run();
        h.get_by_label("Move beta to user");
        assert!(h.query_by_label("Turn off alpha in 2 projects").is_none());
        h.state_mut().activity.as_mut().unwrap().filter = "claude.json.9".into();
        h.run();
        h.get_by_label("13 Oct 09:12");
        assert!(h.query_by_label("Move beta to user").is_none());
        h.state_mut().activity.as_mut().unwrap().filter = "postgres".into();
        h.run();
        h.get_by_label("No changes match \"postgres\".");
        // The servers' own filter is left as it was.
        assert_eq!(h.state().search, "");
    }

    #[test]
    fn a_saved_change_offers_the_activity_log_from_its_message() {
        let (mut h, engine) = harness();
        h.run();
        engine
            .events
            .send(Event::Applied {
                plan: 1,
                changed: true,
                backup: Some("/data/backups/claude.json.1-0.bak".into()),
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        activity_asked(&engine);
        h.get_by_label("Open activity log").click();
        h.run();
        assert_eq!(h.state().page, Page::ActivityLog);
        assert_eq!(activity_asked(&engine), 1);
        assert!(
            h.query_by_label_contains("The previous file is kept")
                .is_none(),
            "message closed"
        );
    }

    #[test]
    fn an_empty_log_says_what_will_appear() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine
            .events
            .send(Event::Activity(Activity::default()))
            .unwrap();
        h.run();
        h.get_by_label("No changes yet");
        h.get_by_label_contains("shows up here with the backup taken first");
    }

    #[test]
    fn a_log_that_could_not_be_read_says_so_and_shows_the_rest() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        let mut log = sample();
        log.problem =
            Some("2 lines of the activity log could not be read and are left out.".into());
        engine.events.send(Event::Activity(log)).unwrap();
        h.run();
        h.get_by_label_contains("2 lines of the activity log could not be read");
        h.get_by_label("Turn off alpha in 2 projects");
    }

    #[test]
    fn leaving_the_page_lets_the_log_go_and_coming_back_reads_it_again() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        assert!(h.state().activity.is_some());
        h.get_by_label("Connections").click();
        h.run();
        assert!(h.state().activity.is_none(), "nothing held while not shown");
        activity_asked(&engine);
        // A late answer, after the page was left, is not kept either.
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        assert!(h.state().activity.is_none());
        open(&mut h);
        assert_eq!(activity_asked(&engine), 1);
        h.get_by_label("Loading…");
    }

    #[test]
    fn a_change_saved_while_the_page_is_open_is_shown() {
        let (mut h, engine) = harness();
        h.run();
        let applied = || Event::Applied {
            plan: 1,
            changed: true,
            backup: None,
            notes: Vec::new(),
        };
        engine.events.send(applied()).unwrap();
        h.run();
        assert_eq!(
            activity_asked(&engine),
            0,
            "not while another page is shown"
        );
        open(&mut h);
        engine.events.send(Event::Activity(sample())).unwrap();
        h.run();
        activity_asked(&engine);
        engine.events.send(applied()).unwrap();
        h.run();
        assert_eq!(activity_asked(&engine), 1);
        // What was shown stays until the new log arrives: no flash of "Loading…".
        h.get_by_label("Turn off alpha in 2 projects");
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 2,
                error: "No".into(),
            })
            .unwrap();
        h.run();
        assert_eq!(activity_asked(&engine), 1);
    }

    #[test]
    fn the_command_palette_opens_the_page() {
        let (mut h, engine) = harness();
        h.run();
        let titles: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
        assert!(titles.iter().any(|t| t == "Activity log"), "{titles:?}");
        h.state_mut().run(
            &egui::Context::default(),
            crate::command::Action::ActivityLog,
        );
        h.run();
        assert_eq!(h.state().page, Page::ActivityLog);
        assert_eq!(activity_asked(&engine), 1);
    }

    #[test]
    fn the_page_is_built_not_promised() {
        let (mut h, _engine) = harness();
        h.run();
        open(&mut h);
        assert!(h.query_by_label_contains("Coming soon").is_none());
        assert!(h.query_by_label("Back to connections").is_none());
    }
}

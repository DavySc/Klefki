//! The Secrets page: which servers use secrets, and where each one lives, so they can be found and moved out of the
//! configuration files. Names and places only: the engine sends no value, so there is nothing here to hide.
//! Asked of the engine each time the page is opened, and let go when it is left, like the Activity log.
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    ConnectionId, Place, Scope, SecretKind, SecretUse, Secrets, ServerId, ServerSecrets,
    UnusedSecret,
};

#[derive(Default)]
pub struct SecretsPage {
    /// `None` until the engine answers.
    pub report: Option<Secrets>,
    /// What the top bar's filter box holds on this page: it filters the servers shown here.
    pub filter: String,
    /// The unused secret whose Forget is waiting for a yes.
    confirm: Option<ConnectionId>,
    /// What the last Forget did: the line to show, and whether it went wrong.
    pub outcome: Option<(String, bool)>,
}

/// What the page's buttons asked the shell to do.
#[derive(Debug, PartialEq)]
pub enum Asked {
    /// Show this server on the Connections page, at the tab where its secrets are.
    Open(ServerId, Tab),
    /// Delete this unused client secret from the system's store (the user said yes).
    Forget(ConnectionId),
    /// Read the page again.
    Reload,
}

/// The detail tab a server's secrets are edited in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Env,
    Auth,
}

/// What the system's secret store is called here, as the system itself calls it.
pub fn store_name() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else {
        "system keyring"
    }
}

impl SecretsPage {
    /// Draws the page. `live` is false for sample data, which has no files to read.
    pub fn show(&mut self, ui: &mut Ui, live: bool) -> Option<Asked> {
        let pal = theme::palette(ui.ctx());
        let Self {
            report,
            filter,
            confirm,
            outcome,
        } = self;
        let mut asked = None;
        // All of it scrolls: in a small window the notes at the top would leave no room for the list.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Secrets").heading());
                ui.add_space(4.0);
                muted(
                    ui,
                    "Where each server's tokens, keys and passwords live. Klefki shows names and places here, \
                     never the values.",
                );
                ui.add_space(12.0);
                if !live {
                    muted(ui, "Not available for sample data.");
                    return;
                }
                let Some(report) = report.as_ref() else {
                    // No spinner: reading takes a moment, and a spinner would keep the window repainting.
                    muted(ui, "Loading…");
                    return;
                };
                if let Some((line, failed)) = outcome {
                    let (glyph, color) = if *failed {
                        (icon::WARNING, pal.danger)
                    } else {
                        (icon::CHECK, pal.success)
                    };
                    ui.add(
                        egui::Label::new(RichText::new(format!("{glyph} {line}")).color(color))
                            .wrap(),
                    );
                    ui.add_space(8.0);
                }
                if let Some(problem) = &report.store_problem {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} Klefki could not look in the {}, so it cannot say which client secrets \
                                 are stored.",
                                icon::WARNING,
                                store_name()
                            ))
                            .color(pal.warn),
                        );
                        if ui.small_button("Try again").clicked() {
                            asked = Some(Asked::Reload);
                        }
                    });
                    // In the open, not in a tooltip: it says what to fix, and hover is out of the keyboard's reach.
                    muted(ui, problem);
                    ui.add_space(8.0);
                }
                let written = report.written();
                if written > 0 {
                    let count = if written == 1 {
                        "1 secret is".to_owned()
                    } else {
                        format!("{written} secrets are")
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!(
                                "{} {count} typed into a configuration file, readable by anyone who can read \
                                 the file. Open the server to put a ${{VARIABLE}} in its place, and set that \
                                 variable where Claude Code starts, or the server loses it.",
                                icon::WARNING
                            ))
                            .color(pal.warn),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                }
                for note in &report.notes {
                    muted(ui, note);
                }
                if report.servers.is_empty() && report.unused.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(icon::KEY).size(32.0).color(pal.text_muted));
                        ui.add_space(8.0);
                        ui.label("No server uses a secret");
                        ui.add_space(4.0);
                        muted(
                            ui,
                            "Tokens in a server's environment or headers, and OAuth client secrets, show up \
                             here.",
                        );
                    });
                    return;
                }
                let query = filter.trim().to_lowercase();
                let servers: Vec<&ServerSecrets> = report
                    .servers
                    .iter()
                    .filter(|s| query.is_empty() || matches(s, &query))
                    .collect();
                let unused: Vec<&UnusedSecret> = report
                    .unused
                    .iter()
                    .filter(|u| query.is_empty() || matches_unused(u, &query))
                    .collect();
                if servers.is_empty() && unused.is_empty() {
                    muted(ui, &format!("No secrets match \"{}\".", filter.trim()));
                }
                for (i, server) in servers.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    if let Some(a) = server_block(ui, server) {
                        asked = Some(a);
                    }
                }
                if !unused.is_empty() {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(format!("Unused client secrets ({})", unused.len())).strong(),
                    );
                    muted(
                        ui,
                        &format!(
                            "Kept in the {} for servers that are not in {} (removed or renamed outside \
                             Klefki). If you use another Claude Code configuration on this computer, a server \
                             there may still need one of these. A server added back with the same name and place \
                             uses its secret again.",
                            store_name(),
                            report.config
                        ),
                    );
                    ui.add_space(6.0);
                    for u in unused {
                        if let Some(a) = unused_line(ui, u, &report.config, confirm) {
                            asked = Some(a);
                        }
                    }
                }
            });
        if let Some(Asked::Forget(_)) = &asked {
            *confirm = None;
        }
        asked
    }
}

fn muted(ui: &mut Ui, text: &str) {
    let pal = theme::palette(ui.ctx());
    ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
}

/// One unused secret, with Forget, which asks first (`confirm`): a secret forgotten cannot be brought back.
fn unused_line(
    ui: &mut Ui,
    u: &UnusedSecret,
    config: &str,
    confirm: &mut Option<ConnectionId>,
) -> Option<Asked> {
    let pal = theme::palette(ui.ctx());
    let mut asked = None;
    let asking = confirm.as_ref() == Some(&u.connection);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&u.name).strong());
        ui.label(RichText::new(place_of(u.scope, u.project.as_deref())).color(pal.text_muted));
        if !asking
            && ui
                .small_button("Forget…")
                .on_hover_text(format!(
                    "Delete this client secret from the {}. Klefki asks first.",
                    store_name()
                ))
                .clicked()
        {
            *confirm = Some(u.connection.clone());
        }
    });
    if asking {
        ui.indent(("confirm", u.connection.as_str()), |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Delete the client secret kept for {}? No server in {config} uses it, but a server in \
                         another Claude Code configuration on this computer may: its sign-in would stop working. \
                         It cannot be brought back; you would type it in again.",
                        u.name
                    ))
                    .color(pal.warn),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                let forget = egui::Button::new(
                    RichText::new("Forget secret")
                        .strong()
                        .color(pal.on_bright),
                )
                .fill(pal.danger);
                if ui.add(forget).clicked() {
                    asked = Some(Asked::Forget(u.connection.clone()));
                }
                if ui.button("Keep").clicked() {
                    *confirm = None;
                }
            });
        });
    }
    ui.add_space(4.0);
    asked
}

/// `User`, `Local · C:/work/app`: where a server is.
fn place_of(scope: Scope, project: Option<&str>) -> String {
    match project {
        Some(project) => format!("{} · {project}", scope.label()),
        None => scope.label().to_owned(),
    }
}

/// A server and its Open button, then each secret it uses: what it is, and below it where it lives. Stacked rather
/// than in columns, so a long variable name or a narrow window does not push the place out of view.
fn server_block(ui: &mut Ui, server: &ServerSecrets) -> Option<Asked> {
    let pal = theme::palette(ui.ctx());
    let mut asked = None;
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&server.id.name).strong());
        ui.label(
            RichText::new(place_of(server.id.scope, server.id.project.as_deref()))
                .color(pal.text_muted),
        );
        if ui
            .small_button(format!("{} Open", icon::ARROW_SQUARE_OUT))
            .on_hover_text("Show this server on the Connections page, where its secrets are edited")
            .clicked()
        {
            asked = Some(Asked::Open(server.id.clone(), tab_for(&server.uses)));
        }
    });
    ui.indent(("secrets", server.id.connection.as_str()), |ui| {
        for u in &server.uses {
            ui.add_space(2.0);
            ui.add(egui::Label::new(what(u)).wrap());
            where_it_lives(ui, u, &server.file);
        }
    });
    ui.add_space(6.0);
    asked
}

/// `Environment variable GITHUB_TOKEN`, `Argument --api-key`, `OAuth client secret`.
fn what(u: &SecretUse) -> String {
    match (u.kind, u.name.as_str()) {
        (SecretKind::Address, "") => "Address sign-in".to_owned(),
        (SecretKind::Address, name) => format!("Address parameter {name}"),
        (SecretKind::Argument, "") => "An argument".to_owned(),
        (kind, "") => kind.label().to_owned(),
        (kind, name) => format!("{} {name}", kind.label()),
    }
}

/// The file's own name: `.claude.json`, `.mcp.json`. The whole path shows on hover.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn where_it_lives(ui: &mut Ui, u: &SecretUse, file: &str) {
    let pal = theme::palette(ui.ctx());
    let line = |ui: &mut Ui, text: String, color: egui::Color32| {
        ui.add(egui::Label::new(RichText::new(text).color(color)).wrap())
    };
    match &u.place {
        Place::Written { instead } => {
            let mut text = format!("{} Typed into {}", icon::WARNING, file_name(file));
            if let Some(instead) = instead {
                text.push_str(&format!(" · use {instead} instead"));
            }
            // Muted: the banner above says it once in the warning colour; a page of orange rows would not.
            line(ui, text, pal.text_muted).on_hover_text(format!(
                "{file}\nAnyone who can read this file can read the secret."
            ));
        }
        Place::Environment(variables) => {
            let (noun, pronoun) = if variables.len() == 1 {
                ("variable", "it")
            } else {
                ("variables", "them")
            };
            line(
                ui,
                format!("From the environment {noun} {}", variables.join(", ")),
                pal.text_muted,
            )
            .on_hover_text(format!(
                "Claude Code fills {pronoun} in from its own environment when it starts the server. The file \
                 holds only the name."
            ));
        }
        Place::Helper => {
            line(
                ui,
                "Made by a command Claude Code runs".to_owned(),
                pal.text_muted,
            )
            .on_hover_text(
                "headersHelper: Claude Code runs it each time it connects, for the headers.",
            );
        }
        Place::Store(Some(true)) => {
            line(
                ui,
                format!("{} In the {}", icon::LOCK_KEY, store_name()),
                pal.success,
            );
        }
        Place::Store(Some(false)) => {
            line(
                ui,
                "None stored · only needed when the server gave you a client secret".to_owned(),
                pal.text_muted,
            );
        }
        Place::Store(None) => {
            line(
                ui,
                format!("Not known: the {} could not be asked", store_name()),
                pal.text_muted,
            );
        }
    }
}

/// The tab to open a server at: where its first typed-in secret is edited, else where its first secret is.
fn tab_for(uses: &[SecretUse]) -> Tab {
    let tab = |u: &SecretUse| match u.kind {
        SecretKind::Env | SecretKind::Header => Tab::Env,
        SecretKind::ClientSecret => Tab::Auth,
        _ => Tab::General,
    };
    uses.iter()
        .find(|u| matches!(u.place, Place::Written { .. }))
        .or_else(|| uses.first())
        .map_or(Tab::General, tab)
}

/// Whether `server` has `query` (lower case) in its name, place, file, or what or where a secret is.
fn matches(server: &ServerSecrets, query: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(query);
    has(&server.id.name)
        || server.id.project.as_deref().is_some_and(has)
        || has(&server.file)
        || server.uses.iter().any(|u| {
            has(&what(u)) || matches!(&u.place, Place::Environment(v) if v.iter().any(|v| has(v)))
        })
}

fn matches_unused(u: &UnusedSecret, query: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(query);
    has(&u.name) || u.project.as_deref().is_some_and(has) || has("unused client secret")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shell::{DetailTab, Page, ShellApp};
    use crate::theme::ThemeChoice;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use klefki_core::api::{Command, Core, Detached, Event, fixture_snapshot};

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
            .send(Event::Snapshot(fixture_snapshot()))
            .unwrap();
        (h, engine)
    }

    fn drain(engine: &Detached) -> Vec<Command> {
        std::iter::from_fn(|| engine.commands.try_recv().ok()).collect()
    }

    fn asked_for_secrets(engine: &Detached) -> usize {
        drain(engine)
            .into_iter()
            .filter(|c| *c == Command::Secrets)
            .count()
    }

    fn open(h: &mut Harness<'static, ShellApp>) {
        h.get_by_label_contains("Secrets").click();
        h.run();
    }

    fn row_id(name: &str) -> ServerId {
        fixture_snapshot()
            .servers
            .iter()
            .find(|r| r.name == name)
            .unwrap()
            .id()
    }

    fn uses(kind: SecretKind, name: &str, place: Place) -> SecretUse {
        SecretUse {
            kind,
            name: name.to_owned(),
            place,
        }
    }

    fn gone() -> UnusedSecret {
        UnusedSecret {
            connection: ConnectionId::new(Scope::User, None, "old-gateway"),
            scope: Scope::User,
            project: None,
            name: "old-gateway".to_owned(),
        }
    }

    /// Also drawn by the look tests (`shell/tests/look.rs`).
    pub(crate) fn sample() -> Secrets {
        let first = fixture_snapshot().servers[0].name.clone();
        let second = fixture_snapshot().servers[1].name.clone();
        Secrets {
            servers: vec![
                ServerSecrets {
                    id: row_id(&first),
                    file: "~/.claude.json".to_owned(),
                    uses: vec![
                        uses(
                            SecretKind::Env,
                            "GITHUB_TOKEN",
                            Place::Written {
                                instead: Some("${GITHUB_TOKEN}".to_owned()),
                            },
                        ),
                        uses(SecretKind::ClientSecret, "", Place::Store(Some(true))),
                    ],
                },
                ServerSecrets {
                    id: row_id(&second),
                    file: "~/.claude.json".to_owned(),
                    uses: vec![uses(
                        SecretKind::Header,
                        "Authorization",
                        Place::Environment(vec!["REF_TOKEN".to_owned()]),
                    )],
                },
            ],
            unused: vec![gone()],
            store_problem: None,
            notes: Vec::new(),
            config: "~/.claude.json".to_owned(),
        }
    }

    #[test]
    fn the_page_is_read_only_once_it_is_opened_and_let_go_when_left() {
        let (mut h, engine) = harness();
        h.run();
        assert_eq!(asked_for_secrets(&engine), 0, "nothing is read at start-up");
        open(&mut h);
        assert_eq!(h.state().page, Page::Secrets);
        assert_eq!(asked_for_secrets(&engine), 1);
        h.get_by_label("Loading…");
        h.run();
        assert_eq!(asked_for_secrets(&engine), 0, "asked once, not every frame");
        h.get_by_label_contains("Connections").click();
        h.run();
        assert!(h.state().secrets.is_none(), "not held once left");
    }

    #[test]
    fn each_secret_shows_what_it_is_and_where_it_lives() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Secrets(sample())).unwrap();
        h.run();
        h.get_by_label("Environment variable GITHUB_TOKEN");
        h.get_by_label(&format!(
            "{} Typed into .claude.json · use ${{GITHUB_TOKEN}} instead",
            icon::WARNING
        ));
        h.get_by_label("Header Authorization");
        h.get_by_label("From the environment variable REF_TOKEN");
        h.get_by_label("OAuth client secret");
        h.get_by_label_contains(&format!("In the {}", store_name()));
        h.get_by_label_contains("1 secret is typed into a configuration file");
        h.get_by_label("Unused client secrets (1)");
        h.get_by_label("old-gateway");
        assert!(h.query_by_label("Loading…").is_none());
    }

    #[test]
    fn open_shows_the_server_at_the_tab_its_secret_is_edited_in() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Secrets(sample())).unwrap();
        h.run();
        let first = fixture_snapshot().servers[0].clone();
        h.get_all_by_label(&format!("{} Open", icon::ARROW_SQUARE_OUT))
            .next()
            .unwrap()
            .click();
        h.run();
        assert_eq!(h.state().page, Page::Connections);
        assert_eq!(h.state().selected, std::collections::BTreeSet::from([0]));
        assert_eq!(
            h.state().tab,
            DetailTab::HeadersEnv,
            "the typed-in env value"
        );
        // Variant: a server whose only secret is its client secret opens at Auth.
        let report = Secrets {
            servers: vec![ServerSecrets {
                id: first.id(),
                file: "~/.claude.json".to_owned(),
                uses: vec![uses(
                    SecretKind::ClientSecret,
                    "",
                    Place::Store(Some(false)),
                )],
            }],
            ..Secrets::default()
        };
        open(&mut h);
        engine.events.send(Event::Secrets(report)).unwrap();
        h.run();
        h.get_by_label_contains("None stored");
        h.get_by_label(&format!("{} Open", icon::ARROW_SQUARE_OUT))
            .click();
        h.run();
        assert_eq!(h.state().tab, DetailTab::Auth);
    }

    #[test]
    fn forget_asks_first_and_keep_changes_nothing() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Secrets(sample())).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label("Forget…").click();
        h.run();
        // Cross-review B1: what was checked, and who else may need it.
        h.get_by_label_contains("No server in ~/.claude.json uses it");
        h.get_by_label_contains("another Claude Code configuration on this computer may");
        h.get_by_label_contains("It cannot be brought back");
        h.get_by_label("Keep").click();
        h.run();
        assert!(
            h.query_by_label_contains("It cannot be brought back")
                .is_none()
        );
        assert!(drain(&engine).is_empty());
        h.get_by_label("Forget…").click();
        h.run();
        h.get_by_label("Forget secret").click();
        h.run();
        assert_eq!(
            drain(&engine),
            [Command::ForgetUnusedSecret(gone().connection)]
        );
        engine
            .events
            .send(Event::UnusedSecretForgotten {
                connection: gone().connection,
                result: Ok(()),
            })
            .unwrap();
        h.run();
        h.get_by_label_contains("Forgot the client secret kept for old-gateway.");
    }

    #[test]
    fn a_change_while_the_page_is_shown_reads_it_again() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Secrets(sample())).unwrap();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::Snapshot(fixture_snapshot()))
            .unwrap();
        h.run();
        // Cross-review N1: the definitions again, not the system's store, which can ask to be unlocked.
        assert_eq!(drain(&engine), [Command::SecretsAfterFileChange]);
        // What was shown stays until the answer comes.
        h.get_by_label("Header Authorization");
    }

    #[test]
    fn a_store_problem_is_said_with_a_way_to_try_again() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        let mut report = sample();
        report.store_problem = Some("locked".to_owned());
        engine.events.send(Event::Secrets(report)).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label_contains("could not look in the");
        // Cross-review N4: the reason in the open, not only on hover.
        h.get_by_label("locked");
        h.get_by_label("Try again").click();
        h.run();
        assert_eq!(asked_for_secrets(&engine), 1);
    }

    #[test]
    fn nothing_to_show_says_what_would_show_up() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine
            .events
            .send(Event::Secrets(Secrets::default()))
            .unwrap();
        h.run();
        h.get_by_label("No server uses a secret");
    }

    #[test]
    fn the_filter_box_filters_the_secrets_on_this_page() {
        let (mut h, engine) = harness();
        h.run();
        open(&mut h);
        engine.events.send(Event::Secrets(sample())).unwrap();
        h.run();
        assert!(
            h.query_by(|n| n
                .placeholder()
                .is_some_and(|p| p.contains("Filter secrets")))
                .is_some()
        );
        h.state_mut().secrets.as_mut().unwrap().filter = "ref_token".to_owned();
        h.run();
        h.get_by_label("Header Authorization");
        assert!(
            h.query_by_label("Environment variable GITHUB_TOKEN")
                .is_none()
        );
        assert!(h.query_by_label("old-gateway").is_none());
        h.state_mut().secrets.as_mut().unwrap().filter = "nothing-like-it".to_owned();
        h.run();
        h.get_by_label("No secrets match \"nothing-like-it\".");
    }

    #[test]
    fn sample_data_says_the_page_needs_the_real_configuration() {
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(|cc| {
                crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
                ShellApp::new(fixture_snapshot(), ThemeChoice::Dark)
            });
        h.run();
        open(&mut h);
        h.get_by_label("Not available for sample data.");
    }
}

//! The Export dialog: the servers' definitions as a block to copy or save to a file.

use super::list::keep_the_end;
use super::{ShellApp, closed_on_purpose, matches_search, names_in_a_line};
use crate::theme;
use egui::{RichText, Ui};
use klefki_core::api::{Command, Exported, FileChoice, SaveError};
use std::path::PathBuf;

/// What the Export dialog holds: what is exported, and the block once the engine sent it.
pub(super) struct ExportView {
    /// For the title: the one server's name, or `3 servers`.
    what: String,
    /// The servers' names in one line, when there is more than one: what is exported, without recalling the list.
    names: Option<String>,
    /// How the search decided which servers these are, when it did.
    scope: Option<String>,
    pub(super) result: Option<Exported>,
    /// What was last copied, and when: that button says "Copied" for a few seconds.
    pub(super) copied: Option<(Copied, f64)>,
    /// The file name the save dialog starts with.
    file_name: String,
    save: Saving,
    /// What the last save did, said as a sentence: `Ok` when it saved.
    status: Option<Result<String, String>>,
    /// The dialog's size in the last frame.
    size: egui::Vec2,
    /// How tall the save status was in the last frame (it wraps a long path).
    status_height: f32,
    /// The variables list was folded to make room for saving on a short window (once: it can be opened again).
    folded: bool,
}

/// Where saving the export to a file is.
#[derive(Clone, PartialEq)]
pub(super) enum Saving {
    Idle,
    /// The system's save dialog is open.
    Choosing,
    /// The save dialog closed without a file; typing a path is offered, starting from `path`.
    Offered {
        path: String,
    },
    /// The path is typed in: no save dialog could be shown (`no_dialog`), or the user asked to type it.
    Typing {
        path: String,
        focus: bool,
        no_dialog: bool,
    },
    /// Written by the engine; `typed` is the text of a typed path, to come back to if the save fails.
    Writing {
        path: std::path::PathBuf,
        typed: Option<String>,
    },
    /// A typed path names a file that is there already: replace it?
    Exists {
        path: std::path::PathBuf,
        typed: String,
    },
}

/// Which of the Export dialog's buttons copied.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Copied {
    Block,
    Names,
}

/// Selects the file's name without its extension in the text field `id` holding `path`, as a system save dialog does,
/// so the part people change is in view and typed over.
fn select_file_stem(ctx: &egui::Context, id: egui::Id, path: &str) {
    use egui::text::{CCursor, CCursorRange};
    let name_at = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let stem_end = path[name_at..]
        .rfind('.')
        .filter(|&i| i > 0)
        .map_or(path.len(), |i| name_at + i);
    let chars = |byte: usize| path[..byte].chars().count();
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(chars(name_at)),
        CCursor::new(chars(stem_end)),
    )));
    state.store(ctx, id);
}

/// Makes the widget `id` a polite live region: a screen reader reads out a change to it ("Copied", "Saved to …")
/// when it is next idle, without moving the keyboard focus (WCAG 2.2, 4.1.3 Status Messages).
pub(crate) fn announce(ctx: &egui::Context, id: egui::Id) {
    ctx.accesskit_node_builder(id, |node| node.set_live(egui::accesskit::Live::Polite));
}

/// How long the Copy button says "Copied".
pub(crate) const COPIED_SECS: f64 = 4.0;

impl ShellApp {
    /// The rows Export takes, in the list's order: the selection (all of it, even rows the search hides now), or
    /// every row the list shows when nothing is selected.
    fn export_rows(&self) -> Vec<usize> {
        let rows = if self.selected.is_empty() {
            self.visible()
        } else {
            self.in_list_order(self.selected.iter().copied().collect())
        };
        // A plugin's servers come with the plugin; Klefki does not hold their definitions.
        rows.into_iter()
            .filter(|&i| !self.snapshot.servers[i].scope.is_read_only())
            .collect()
    }

    /// Whether Export has anything to take; cheap enough for every frame (no sorting, no list built).
    pub(super) fn can_export(&self) -> bool {
        let q = self.search.trim().to_lowercase();
        self.core.is_some()
            && if self.selected.is_empty() {
                self.snapshot
                    .servers
                    .iter()
                    .any(|s| !s.scope.is_read_only() && matches_search(s, &q))
            } else {
                !self.changeable_selected().0.is_empty()
            }
    }

    /// Opens the Export dialog and asks the engine for the block.
    pub(super) fn open_export(&mut self) {
        let rows = self.export_rows();
        if self.core.is_none() || rows.is_empty() || self.exporting.is_some() {
            return;
        }
        let servers = &self.snapshot.servers;
        let what = match rows.as_slice() {
            [one] => servers[*one].name.clone(),
            _ => format!("{} servers", rows.len()),
        };
        let names: Vec<&str> = rows.iter().map(|&i| servers[i].name.as_str()).collect();
        // Which rows the search decided, said plainly: the two cases work differently.
        let search = self.search.trim();
        let scope = if search.is_empty() {
            None
        } else if self.selected.is_empty() {
            Some(format!(
                "Only the servers that match \u{201c}{search}\u{201d}."
            ))
        } else {
            let shown = self.visible();
            match rows.iter().filter(|i| !shown.contains(i)).count() {
                0 => None,
                1 => Some("Includes 1 selected server the search hides.".to_owned()),
                n => Some(format!("Includes {n} selected servers the search hides.")),
            }
        };
        // A plugin's servers come with the plugin: said, not dropped silently.
        let plugins = match self.changeable_selected().1 {
            0 => None,
            1 => Some("Leaves out 1 selected plugin server: it comes with its plugin.".to_owned()),
            n => Some(format!(
                "Leaves out {n} selected plugin servers: they come with their plugins."
            )),
        };
        let scope = match (scope, plugins) {
            (Some(a), Some(b)) => Some(format!("{a} {b}")),
            (a, b) => a.or(b),
        };
        self.exporting = Some(ExportView {
            file_name: klefki_core::api::export_file_name(&names),
            save: Saving::Idle,
            status: None,
            size: egui::Vec2::ZERO,
            status_height: 26.0,
            folded: false,
            what,
            names: (rows.len() > 1).then(|| names_in_a_line(&names)),
            scope,
            result: None,
            copied: None,
        });
        self.exports_pending += 1;
        self.send(Command::Export(
            rows.iter().map(|&i| servers[i].id()).collect(),
        ));
    }

    pub(super) fn export_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(view) = &mut self.exporting else {
            return;
        };
        let now = ctx.input(|i| i.time);
        // Enter copies, unless a button has the keyboard: Enter is that button's then (Close closes).
        let mut copy = view.result.is_some()
            && ctx.memory(|m| m.focused().is_none())
            && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut copy_names = false;
        let mut close = false;
        let mut choose = false;
        let mut save_typed = false;
        let mut replace = false;
        let mut keep = false;
        let mut type_instead = false;
        let muted = |ui: &mut Ui, text: &str| {
            ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
        };
        let screen = ctx.content_rect();
        let short = screen.height() < 640.0;
        let modal = egui::Modal::new(egui::Id::new("export_dialog")).show(ctx, |ui| {
            ui.set_width(640.0_f32.min(screen.width() - 48.0));
            ui.heading(format!("Export {}", view.what));
            if let Some(names) = &view.names {
                muted(ui, names);
            }
            if let Some(scope) = &view.scope {
                ui.add(egui::Label::new(RichText::new(scope).color(pal.warn)).wrap());
            }
            ui.add_space(6.0);
            match &view.result {
                // Text, not a spinner: an animation would repaint every frame, and reading takes milliseconds.
                None => {
                    ui.label("Reading the definitions…");
                }
                Some(result) => {
                    let (to_set, defaults): (Vec<_>, Vec<_>) =
                        result.variables.iter().partition(|v| v.default.is_none());
                    if result.variables.is_empty() {
                        muted(ui, "Nothing was left out: no value needed a reference.");
                    } else {
                        let title = match (to_set.len(), defaults.len()) {
                            (0, d) => format!("{d} settings as references, with their values as defaults"),
                            (1, _) => "1 variable to set where you add them".to_owned(),
                            (n, _) => format!("{n} variables to set where you add them"),
                        };
                        // Open when there is room; on a small window the block gets the room first.
                        // Saving's rows take room the block needs on a short window: the list folds, once.
                        let fold = short
                            && !view.folded
                            && (view.status.is_some()
                                || !matches!(view.save, Saving::Idle | Saving::Choosing));
                        view.folded |= fold;
                        egui::CollapsingHeader::new(RichText::new(title).strong())
                            .id_salt("export_variables")
                            .default_open(!short)
                            .open(fold.then_some(false))
                            .show(ui, |ui| {
                                muted(
                                    ui,
                                    "Values that may be secrets are written as ${NAME}; plain settings keep their value as ${NAME:-value}.",
                                );
                                egui::ScrollArea::vertical()
                                    .id_salt("export_variable_list")
                                    .max_height((screen.height() * 0.2).min(132.0))
                                    .show(ui, |ui| {
                                        // Lines of text, not rows of controls: no control height between them.
                                        ui.spacing_mut().interact_size.y = 0.0;
                                        for variable in to_set.iter().chain(&defaults) {
                                            ui.horizontal_wrapped(|ui| {
                                                let name = RichText::new(&variable.name).strong();
                                                ui.label(match &variable.default {
                                                    None => name.color(pal.key),
                                                    Some(_) => name,
                                                });
                                                if let Some(default) = &variable.default {
                                                    ui.label(format!("= {default} (default)"));
                                                }
                                                ui.label(
                                                    RichText::new(variable.replaces.join("; "))
                                                        .color(pal.text_muted),
                                                );
                                            });
                                        }
                                    });
                            });
                    }
                    for note in &result.notes {
                        ui.add(egui::Label::new(RichText::new(note).color(pal.warn)).wrap());
                    }
                    ui.add_space(6.0);
                    // The block takes what is left of the window's height, so the buttons (and what saving says
                    // under them) stay on screen.
                    let used = ui.cursor().top() - ui.min_rect().top();
                    let below = match view.save {
                        Saving::Typing { .. } | Saving::Exists { .. } => 64.0,
                        Saving::Offered { .. } => 32.0,
                        Saving::Writing { typed: Some(_), .. } => 40.0,
                        _ => 0.0,
                    } + if view.status.is_some() {
                        view.status_height
                    } else {
                        0.0
                    };
                    let room = screen.height() - 64.0 - used - 80.0 - below;
                    egui::ScrollArea::vertical()
                        .id_salt("export_block")
                        .max_height(room.clamp(if below > 0.0 { 64.0 } else { 96.0 }, 320.0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut result.text.as_str())
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let copied = |what| {
                    view.copied
                        .is_some_and(|(it, at)| it == what && now - at < COPIED_SECS)
                };
                let label = if copied(Copied::Block) { "Copied" } else { "Copy" };
                let button = egui::Button::new(RichText::new(label).strong().color(pal.on_bright))
                    .fill(pal.primary)
                    .min_size(egui::vec2(96.0, 34.0));
                let copy_button = ui
                    .add_enabled(view.result.is_some(), button)
                    .on_hover_text("Copy the block (Enter)")
                    .on_disabled_hover_text("Reading the definitions…");
                announce(ui.ctx(), copy_button.id);
                copy |= copy_button.clicked();
                let busy = matches!(view.save, Saving::Choosing | Saving::Writing { .. });
                choose |= ui
                    .add_enabled(
                        view.result.is_some() && !busy,
                        egui::Button::new("Save to file…").min_size(egui::vec2(96.0, 34.0)),
                    )
                    .on_hover_text("Save the block as a JSON file")
                    .on_disabled_hover_text(if busy {
                        "Waiting for the save dialog"
                    } else {
                        "Reading the definitions…"
                    })
                    .clicked();
                if view
                    .result
                    .as_ref()
                    .is_some_and(|r| r.variables.iter().any(|v| v.default.is_none()))
                {
                    let label = if copied(Copied::Names) { "Copied" } else { "Copy names" };
                    let names_button = ui
                        .add(egui::Button::new(label).min_size(egui::vec2(80.0, 34.0)))
                        .on_hover_text("Copy the variables to set, one NAME= a line");
                    announce(ui.ctx(), names_button.id);
                    copy_names |= names_button.clicked();
                }
                close |= ui
                    .add(egui::Button::new("Close").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
            // A typed path keeps its row while it is written and while replacing is asked, so nothing jumps.
            let typed_row = match &mut view.save {
                Saving::Typing { path, focus, .. } => Some((path, std::mem::take(focus), true)),
                Saving::Writing {
                    typed: Some(path), ..
                }
                | Saving::Exists { typed: path, .. } => Some((path, false, false)),
                _ => None,
            };
            if let Some((path, focus, editable)) = typed_row {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = ui.label("Save to");
                    let field = ui
                        .add_enabled(
                            editable,
                            egui::TextEdit::singleline(path)
                                .id(egui::Id::new("export_save_path"))
                                .desired_width((ui.available_width() - 96.0).max(120.0))
                                .font(egui::TextStyle::Monospace),
                        )
                        .labelled_by(label.id);
                    if focus {
                        field.request_focus();
                        select_file_stem(ui.ctx(), field.id, path);
                    }
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    save_typed |= ui
                        .add_enabled(
                            editable && !path.trim().is_empty(),
                            egui::Button::new("Save here").min_size(egui::vec2(88.0, 28.0)),
                        )
                        .clicked()
                        || (editable && entered);
                });
            }
            match &view.save {
                Saving::Typing {
                    no_dialog: true, ..
                } => muted(
                    ui,
                    "No save dialog could be opened here, so type where the file goes.",
                ),
                Saving::Offered { .. } => {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let said = ui.label(RichText::new("Not saved.").color(pal.text_muted));
                        announce(ui.ctx(), said.id);
                        type_instead |= ui.link("Type a path instead").clicked();
                    });
                }
                Saving::Exists { path, .. } => {
                    ui.horizontal_wrapped(|ui| {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let question = ui.label(
                            RichText::new(format!("{name} is there already. Replace it?"))
                                .color(pal.warn),
                        );
                        announce(ui.ctx(), question.id);
                        replace |= ui.button("Replace").clicked();
                        keep |= ui.button("Keep it").clicked();
                    });
                }
                _ => {}
            }
            if let Some(status) = &view.status {
                ui.add_space(6.0);
                let said = match status {
                    // One line, however long the path: its start gives way, so the file's name stays in view.
                    Ok(text) => {
                        let shown = keep_the_end(ui, text, ui.available_width());
                        let said = ui.add(egui::Label::new(
                            RichText::new(&shown).color(pal.success),
                        ));
                        if shown != *text {
                            ui.ctx()
                                .accesskit_node_builder(said.id, |node| node.set_value(text.clone()));
                            said.on_hover_text(text)
                        } else {
                            said
                        }
                    }
                    Err(text) => {
                        ui.add(egui::Label::new(RichText::new(text).color(pal.warn)).wrap())
                    }
                };
                announce(ui.ctx(), said.id);
                // A long path wraps; the block gives up that much room in the next frame.
                view.status_height = said.rect.height() + 6.0;
            }
        });
        if choose {
            view.save = Saving::Choosing;
            view.status = None;
            self.export_choices_pending += 1;
            self.export_choice_asked = Some(view.file_name.clone());
        }
        if (save_typed || replace)
            && let Some(result) = &view.result
        {
            let typed = match &view.save {
                Saving::Typing { path, .. } => Some(path.clone()),
                Saving::Exists { typed, .. } if replace => Some(typed.clone()),
                _ => None,
            };
            if let Some(typed) = typed {
                // Quotes from a copied path ("Copy as path" on Windows) are not part of it.
                let path = std::path::PathBuf::from(typed.trim().trim_matches('"'));
                if path.is_absolute() {
                    view.status = None;
                    view.save = Saving::Writing {
                        path: path.clone(),
                        typed: Some(typed),
                    };
                    if let Some(core) = &self.core {
                        core.send(Command::SaveExport {
                            path,
                            text: result.text.clone(),
                            replace,
                        });
                    }
                } else {
                    view.status = Some(Err(
                        "Not saved: give the whole path, starting from the drive or /.".to_owned(),
                    ));
                }
            }
        }
        if keep && let Saving::Exists { typed, .. } = &view.save {
            view.save = Saving::Typing {
                path: typed.clone(),
                focus: true,
                no_dialog: false,
            };
        }
        if type_instead && let Saving::Offered { path } = &view.save {
            view.save = Saving::Typing {
                path: path.clone(),
                focus: true,
                no_dialog: false,
            };
        }
        if let Some(result) = &view.result {
            if copy {
                ctx.copy_text(result.text.clone());
            }
            if copy_names {
                let lines: String = result
                    .variables
                    .iter()
                    .filter(|v| v.default.is_none())
                    .map(|v| format!("{}=\n", v.name))
                    .collect();
                ctx.copy_text(lines);
            }
            if copy || copy_names {
                let what = if copy { Copied::Block } else { Copied::Names };
                view.copied = Some((what, now));
                // Back to "Copy" once the confirmation has been seen; no repaint every frame meanwhile.
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS));
            }
        }
        // The dialog is centred by its size in the frame before; when what it holds changes its size, one more frame
        // puts it in place (or a click aimed at a button lands where the button was).
        let size = modal.response.rect.size();
        if view.size != size {
            view.size = size;
            ctx.request_repaint();
        }
        if close || closed_on_purpose(&modal) {
            self.exporting = None;
        }
    }

    /// The exported block arrived; shown once the last one asked for is in.
    pub(super) fn exported(&mut self, exported: Exported) {
        self.exports_pending = self.exports_pending.saturating_sub(1);
        if let Some(view) = &mut self.exporting
            && self.exports_pending == 0
        {
            view.result = Some(exported);
        }
    }

    /// The system's save dialog closed: save to the file chosen, or offer to type a path.
    pub(super) fn export_file_chosen(&mut self, choice: FileChoice) {
        self.export_choices_pending = self.export_choices_pending.saturating_sub(1);
        if let Some(view) = &mut self.exporting
            && self.export_choices_pending == 0
            && view.save == Saving::Choosing
        {
            view.save = Saving::Idle;
            match choice {
                FileChoice::Chosen(path) => {
                    if let Some(result) = &view.result {
                        // The system's dialog asked before replacing a file there.
                        let text = result.text.clone();
                        view.save = Saving::Writing {
                            path: path.clone(),
                            typed: None,
                        };
                        self.send(Command::SaveExport {
                            path,
                            text,
                            replace: true,
                        });
                    }
                }
                // A dialog that failed slowly looks like a cancel: the way to type a path stays in reach.
                FileChoice::Cancelled { suggested } => {
                    view.save = Saving::Offered {
                        path: suggested.to_string_lossy().into_owned(),
                    };
                }
                FileChoice::NoDialog { suggested } => {
                    view.save = Saving::Typing {
                        path: suggested.to_string_lossy().into_owned(),
                        focus: true,
                        no_dialog: true,
                    };
                }
            }
        }
    }

    /// The export was written to `path`, or why not.
    pub(super) fn export_saved(
        &mut self,
        path: PathBuf,
        shown: String,
        result: Result<(), SaveError>,
    ) {
        if let Some(view) = &mut self.exporting
            && let Saving::Writing { path: asked, typed } = &view.save
            && *asked == path
        {
            let typed = typed.clone();
            view.save = Saving::Idle;
            match result {
                Ok(()) => view.status = Some(Ok(format!("Saved to {shown}"))),
                Err(SaveError::Exists) => {
                    view.status = None;
                    view.save = Saving::Exists {
                        path,
                        typed: typed.unwrap_or_default(),
                    };
                }
                Err(why) => {
                    view.status = Some(Err(format!("Not saved: {why}")));
                    // A typed path stays, to be put right.
                    if let Some(path) = typed {
                        view.save = Saving::Typing {
                            path,
                            focus: false,
                            no_dialog: false,
                        };
                    }
                }
            }
        }
    }
}

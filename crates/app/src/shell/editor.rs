//! Editing a server in the detail panel: its definition as a form or as JSON, its values, its client secret, and
//! the save bar.

use super::{DetailTab, ShellApp, icon_button, transport_name};
use crate::theme;
use egui::{Align, Color32, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::words::capitalized;
use klefki_core::api::{Change, Command, Definition, Secret, ServerId, ServerRow, TransportKind};

/// The definition being edited, for one server: in the form, or as JSON.
pub(super) struct Editor {
    pub(super) id: ServerId,
    pub(super) state: EditorState,
    /// Asked for with Edit: brought into view when it arrives, for it is drawn below the summary, which can leave it
    /// under the save bar.
    pub(super) reveal: bool,
    /// Its server was moved or renamed under a draft: the definition is read again from the new place, and the draft
    /// carried onto it ([`Definition::rebase`]).
    pub(super) moved: bool,
}

pub(super) enum EditorState {
    /// Asked the engine for the definition.
    Loading,
    Failed(String),
    Ready {
        /// As the engine sent it, to tell whether anything was edited.
        original: String,
        /// The draft, as JSON: what the JSON editor edits and what is sent for review.
        text: String,
        /// Project servers are shown but not edited (they live in the repository).
        editable: bool,
        /// The same draft as the form shows it, or why the form cannot. Boxed, like `synced_form`: a definition is
        /// large next to the other states.
        form: Box<Result<Definition, String>>,
        /// What stops the form's draft from being saved.
        problem: Option<String>,
        /// `text` and `form` as they were when last brought in line, to tell which one was edited since.
        synced_text: String,
        synced_form: Option<Box<Definition>>,
        /// Edited as JSON rather than in the form: asked for, or a definition the form cannot show. It stays until
        /// the user goes back, so the form does not replace the JSON under the cursor once it becomes readable.
        as_json: bool,
    },
}

impl EditorState {
    pub(super) fn ready(text: String, editable: bool) -> Self {
        let mut state = Self::Ready {
            original: text.clone(),
            text,
            editable,
            form: Box::new(Err(String::new())),
            problem: None,
            synced_text: String::new(),
            synced_form: None,
            as_json: false,
        };
        state.sync();
        if let Self::Ready { form, as_json, .. } = &mut state {
            *as_json = form.is_err();
        }
        state
    }

    /// The draft differs from the definition it was read from.
    pub(super) fn edited(&mut self) -> bool {
        self.sync();
        matches!(self, Self::Ready { text, original, .. } if text != original)
    }

    /// Brings the form and the JSON in line after either was edited: an edited JSON text is read into the form,
    /// an edited form is written out as JSON. Comparisons only, unless something changed.
    pub(super) fn sync(&mut self) {
        let Self::Ready {
            text,
            form,
            problem,
            synced_text,
            synced_form,
            ..
        } = self
        else {
            return;
        };
        if *text != *synced_text {
            **form = Definition::from_json(text);
        } else if let Ok(f) = form.as_ref()
            && Some(f) != synced_form.as_deref()
        {
            *text = f.to_json();
        } else {
            return;
        }
        *synced_text = text.clone();
        *synced_form = form.as_ref().as_ref().ok().cloned().map(Box::new);
        *problem = form.as_ref().as_ref().ok().and_then(Definition::problem);
    }
}

/// What the Auth tab knows about one server's client secret.
pub(super) struct SecretView {
    pub(super) id: ServerId,
    /// The store's last answer: stored or not, or why it could not say.
    pub(super) known: Option<Result<bool, String>>,
    /// Waiting for the store: what to say meanwhile.
    pub(super) busy: Option<&'static str>,
    /// Forget… was pressed; asking before doing it.
    confirm_forget: bool,
}

impl ShellApp {
    /// Edit (toolbar, Ctrl+E, palette): the selected server's definition, in the form on the General tab.
    pub(super) fn edit_selected(&mut self) {
        let Some(s) = self
            .single_selected()
            .filter(|s| !s.scope.is_read_only())
            .cloned()
        else {
            return;
        };
        self.tab = DetailTab::General;
        if self.core.is_some() && self.editor_for(&s).is_none() {
            self.open_editor(&s);
        }
    }

    /// Whether `s`'s definition is being edited as JSON instead of in the form.
    fn editing_as_json(&mut self, s: &ServerRow) -> bool {
        matches!(
            self.editor_for(s),
            Some(EditorState::Ready { as_json, form, .. }) if *as_json || form.is_err()
        )
    }

    /// The definition as JSON, in place of the form, for what the form does not show. Saved through the same
    /// review as the form.
    fn json_editor(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        let Some(EditorState::Ready { text, form, .. }) = self.editor_for(s) else {
            return;
        };
        if let Err(why) = form.as_ref() {
            ui.add(egui::Label::new(RichText::new(why.as_str()).color(pal.warn)).wrap());
            ui.add_space(4.0);
        }
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Values shown as <hidden> keep what they are now. Type a new value to change one.",
                )
                .color(pal.text_muted),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(text)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(14),
        );
        self.form_end(ui, s);
    }

    /// The OAuth client secret of `s`: whether the system's secret store holds one, and setting, replacing or
    /// forgetting it. Asked for once per server shown.
    pub(super) fn client_secret(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        if !self.secret.as_ref().is_some_and(|v| s.is(&v.id)) {
            self.secret = Some(SecretView {
                id: s.id(),
                known: None,
                busy: Some("Checking…"),
                confirm_forget: false,
            });
            self.secret_input = None;
            self.send(Command::SecretStatus(s.id()));
        }
        let Some(view) = &mut self.secret else {
            return;
        };
        let mut send = None;
        ui.label(RichText::new("Client secret").color(pal.text_muted));
        if let Some(busy) = view.busy {
            ui.label(RichText::new(busy).color(pal.text_muted));
        } else {
            match &view.known {
                Some(Ok(true)) => {
                    ui.label("Stored in your system's secret store.");
                    ui.horizontal(|ui| {
                        if ui.button("Replace…").clicked() {
                            self.secret_input = Some(Secret::new(String::new()));
                        }
                        if ui.button("Forget…").clicked() {
                            view.confirm_forget = true;
                        }
                    });
                }
                Some(Ok(false)) => {
                    ui.label(RichText::new("Not stored.").color(pal.text_muted));
                    if self.secret_input.is_none() && ui.button("Set client secret…").clicked() {
                        self.secret_input = Some(Secret::new(String::new()));
                    }
                }
                Some(Err(why)) => {
                    ui.add(egui::Label::new(RichText::new(why.as_str()).color(pal.danger)).wrap());
                    if ui.button("Try again").clicked() {
                        send = Some(Command::SecretStatus(s.id()));
                        view.busy = Some("Checking…");
                    }
                }
                None => {}
            }
        }
        if view.confirm_forget && view.busy.is_none() {
            ui.add(
                egui::Label::new(
                    RichText::new("Forget the client secret? This cannot be undone; you would have to type it again.")
                        .color(pal.warn),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                if ui.button("Forget it").clicked() {
                    send = Some(Command::ForgetSecret(s.id()));
                    view.busy = Some("Forgetting…");
                    view.confirm_forget = false;
                }
                if ui.button("Keep").clicked() {
                    view.confirm_forget = false;
                }
            });
        }
        if view.busy.is_none()
            && let Some(input) = &mut self.secret_input
        {
            let (mut save, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(input.edit())
                        .password(true)
                        .hint_text("Client secret")
                        .desired_width(220.0),
                );
                save = ui
                    .add_enabled(!input.is_empty(), egui::Button::new("Save"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if save {
                // Moved, not copied: the field's value goes to the engine and the app keeps nothing.
                if let Some(secret) = self.secret_input.take() {
                    send = Some(Command::StoreSecret { id: s.id(), secret });
                    view.busy = Some("Saving…");
                }
            } else if cancel {
                self.secret_input = None;
            }
        }
        if let Some(command) = send {
            self.send(command);
        }
    }

    /// The server whose definition has edits not saved yet.
    pub(super) fn drafted(&self) -> Option<ServerId> {
        self.editor
            .as_ref()
            .filter(|e| matches!(&e.state, EditorState::Ready { text, original, .. } if text != original))
            .map(|e| e.id.clone())
    }

    /// Asks the engine for `s`'s definition, for the form and the JSON editor.
    pub(super) fn open_editor(&mut self, s: &ServerRow) {
        self.editor = Some(Editor {
            id: s.id(),
            state: EditorState::Loading,
            reveal: true,
            moved: false,
        });
        self.send(Command::Inspect(s.id()));
    }

    /// The editor's state for `s`, if its definition was asked for.
    fn editor_for(&mut self, s: &ServerRow) -> Option<&mut EditorState> {
        self.editor
            .as_mut()
            .filter(|e| s.is(&e.id))
            .map(|e| &mut e.state)
    }

    /// Brings the definition asked for with Edit, `shown`, into view once it has arrived. It scrolls no further than
    /// that takes, so the summary above stays when there is room for both. `detail` says when it is done: the save
    /// bar that comes with the form shortens the view only in the next pass.
    pub(super) fn reveal_definition(&self, s: &ServerRow, shown: &egui::Response) {
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.reveal && s.is(&e.id) && !matches!(e.state, EditorState::Loading))
        {
            shown.scroll_to_me(None);
        }
    }

    /// Why the form is not shown for `s`, or `None` when it can be edited. Draws that reason.
    fn form_unavailable(&mut self, ui: &mut Ui, s: &ServerRow) -> bool {
        let pal = theme::palette(ui.ctx());
        let muted = |ui: &mut Ui, text: &str, color: Color32| {
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        };
        let Some(state) = self.editor_for(s) else {
            // The definition is read only when asked for: `.claude.json` can be tens of megabytes.
            if ui
                .button("Edit definition")
                .on_hover_text("Change the command, address, arguments, environment or headers")
                .clicked()
            {
                self.open_editor(s);
            }
            return true;
        };
        match state {
            EditorState::Loading => muted(ui, "Loading…", pal.text_muted),
            EditorState::Failed(why) => muted(ui, why, pal.danger),
            EditorState::Ready {
                editable: false,
                original,
                ..
            } => {
                muted(
                    ui,
                    "Defined in the repository's .mcp.json. Klefki shows it but does not edit files in a repository.",
                    pal.text_muted,
                );
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut original.as_str())
                        .code_editor()
                        .desired_width(f32::INFINITY),
                );
            }
            EditorState::Ready { form, .. } => {
                // Said before anything is typed, so the sign-in it costs is no surprise at Review.
                if form
                    .as_ref()
                    .as_ref()
                    .is_ok_and(Definition::signs_in_with_oauth)
                {
                    muted(
                        ui,
                        "Signs in with OAuth: Claude Code saves a change by removing the server and adding it again, so you sign in again afterwards.",
                        pal.text_muted,
                    );
                    ui.add_space(4.0);
                }
                return false;
            }
        }
        true
    }

    /// The General tab's form: type, then command and arguments, or address and header command.
    pub(super) fn definition_form(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        ui.label(RichText::new("Definition").color(pal.text_muted));
        ui.add_space(4.0);
        if self.form_unavailable(ui, s) {
            return;
        }
        if self.editing_as_json(s) {
            self.json_editor(ui, s);
            return;
        }
        let Some(EditorState::Ready { form, .. }) = self.editor_for(s) else {
            return;
        };
        let Ok(f) = form.as_mut() else {
            return;
        };
        let width = ui.available_width();
        egui::Grid::new("definition")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Type").color(pal.text_muted));
                egui::ComboBox::from_id_salt("definition-type")
                    .selected_text(transport_name(f.transport))
                    .show_ui(ui, |ui| {
                        for t in [
                            TransportKind::Stdio,
                            TransportKind::Http,
                            TransportKind::Sse,
                            TransportKind::Ws,
                        ] {
                            ui.selectable_value(&mut f.transport, t, transport_name(t));
                        }
                    });
                ui.end_row();
                let field = |ui: &mut Ui, label: &str, value: &mut String| {
                    let l = ui.label(RichText::new(label).color(pal.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(value)
                            .desired_width((width - 120.0).max(120.0))
                            .font(egui::TextStyle::Monospace),
                    )
                    .labelled_by(l.id);
                    ui.end_row();
                };
                if f.transport == TransportKind::Stdio {
                    field(ui, "Command", &mut f.command);
                } else {
                    field(ui, "Address", &mut f.url);
                    field(ui, "Header command", &mut f.headers_helper);
                }
            });
        // Arguments under their own heading rather than in the grid, which would centre the label beside the list.
        if f.transport == TransportKind::Stdio {
            ui.add_space(6.0);
            ui.label(RichText::new("Arguments").color(pal.text_muted));
            let mut remove = None;
            for (i, arg) in f.args.iter_mut().enumerate() {
                // Right to left: the button takes its room first and the field exactly what is left, so the row
                // never asks for more than the panel has (a resizable panel would grow to fit, frame after frame).
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        if icon_button(ui, icon::X, &format!("Remove argument {}", i + 1)) {
                            remove = Some(i);
                        }
                        ui.add(
                            egui::TextEdit::singleline(arg)
                                .desired_width(ui.available_width())
                                .font(egui::TextStyle::Monospace),
                        );
                    },
                );
            }
            if let Some(i) = remove {
                f.args.remove(i);
            }
            if ui.small_button("Add argument").clicked() {
                f.args.push(String::new());
            }
        }
        self.form_end(ui, s);
    }

    /// The Env tab's form: environment variables for a command, headers for an address. Values that are there
    /// stay hidden; a new one is typed without being shown.
    pub(super) fn values_form(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        if self.form_unavailable(ui, s) {
            return;
        }
        // Environment and headers are part of the definition: while it is edited as JSON, they are there.
        if self.editing_as_json(s) {
            self.json_editor(ui, s);
            return;
        }
        let Some(EditorState::Ready { form, .. }) = self.editor_for(s) else {
            return;
        };
        let Ok(f) = form.as_mut() else {
            return;
        };
        let (pairs, title, noun) = if f.transport == TransportKind::Stdio {
            (&mut f.env, "Environment", "variable")
        } else {
            (&mut f.headers, "Headers", "header")
        };
        ui.label(RichText::new(title).color(pal.text_muted));
        ui.add_space(4.0);
        let mut remove = None;
        for (i, (name, value)) in pairs.iter_mut().enumerate() {
            // Right to left, as for arguments: the buttons first, then name and value share what is left.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                Layout::right_to_left(Align::Center),
                |ui| {
                    if icon_button(ui, icon::X, &format!("Remove {name}")) {
                        remove = Some(i);
                    }
                    let hidden = value == Definition::HIDDEN;
                    if hidden && icon_button(ui, icon::PENCIL_SIMPLE, &format!("Change {name}")) {
                        value.clear();
                    }
                    let half =
                        ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(40.0);
                    if hidden {
                        ui.add_sized(
                            [half, ui.spacing().interact_size.y],
                            egui::Label::new(RichText::new("hidden").color(pal.text_muted)),
                        )
                        .on_hover_text("Kept as it is. Change it to type a new value.");
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(half)
                                .hint_text("Value")
                                .password(true),
                        );
                    }
                    ui.add(
                        egui::TextEdit::singleline(name)
                            .desired_width(ui.available_width())
                            .hint_text("Name")
                            .font(egui::TextStyle::Monospace),
                    );
                },
            );
            if let Some(reference) = Definition::secret_suggestion(name, value) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Looks like a secret.").color(pal.warn));
                    if ui
                        .small_button(format!("Use {reference} instead"))
                        .on_hover_text(
                            "Claude Code fills the variable in from its own environment, so the secret stays out of ~/.claude.json.",
                        )
                        .clicked()
                    {
                        *value = reference;
                    }
                });
            }
        }
        if let Some(i) = remove {
            pairs.remove(i);
        }
        if ui.small_button(format!("Add {noun}")).clicked() {
            pairs.push((String::new(), String::new()));
        }
        self.form_end(ui, s);
    }

    /// Under the last field of a form or of the JSON: the quiet way between the two, and the save bar asked for. The
    /// fields are the way to edit, the JSON the way out when they fall short. Not in the save bar, which stays one
    /// line and so puts no switch where the pointer was when the form came in.
    fn form_end(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        self.save_bar_wanted = true;
        let Some(EditorState::Ready { form, as_json, .. }) = self.editor_for(s) else {
            return;
        };
        let quiet =
            |text: &str| egui::Button::new(RichText::new(text).color(pal.link)).frame(false);
        ui.add_space(6.0);
        if *as_json || form.is_err() {
            if ui
                .add_enabled(form.is_ok(), quiet("Back to the form"))
                .on_disabled_hover_text("The form cannot show the JSON as it is now: see above")
                .clicked()
            {
                *as_json = false;
            }
        } else if ui
            .add(quiet("Edit as JSON"))
            .on_hover_text("Every key of the definition, including those the form does not show")
            .clicked()
        {
            *as_json = true;
        }
    }

    /// What stops saving, then Review and save… and Revert.
    pub(super) fn save_bar(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        let can_prepare = self.can_prepare();
        let Some(EditorState::Ready {
            original,
            text,
            problem,
            ..
        }) = self.editor_for(s)
        else {
            return;
        };
        if let Some(problem) = problem {
            ui.add(egui::Label::new(RichText::new(capitalized(problem)).color(pal.danger)).wrap());
        }
        let edited = text != original;
        let ready = edited && problem.is_none() && can_prepare;
        let mut review = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Review and save…"))
                .clicked()
            {
                review = Some(text.clone());
            }
            if ui
                .add_enabled(edited, egui::Button::new("Revert"))
                .clicked()
            {
                *text = original.clone();
            }
            // A draft outlives other changes, so it does not pass for what the file holds.
            if text != original {
                ui.add(
                    egui::Label::new(RichText::new("Unsaved").small().color(pal.text_muted))
                        .extend(),
                )
                .on_hover_text("This edit is not saved yet: Review and save… writes it");
            }
        });
        if let Some(json) = review
            && self.can_prepare()
        {
            self.prepare(vec![Change::Replace { id: s.id(), json }]);
            self.draft_asked = true;
        }
    }
}

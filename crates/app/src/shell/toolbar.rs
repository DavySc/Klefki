//! The toolbar: one icon button per action, each named for screen readers, with its shortcut on hover.

use super::{ShellApp, name_for_screen_readers};
use crate::sign_in;
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;

/// The toolbar's buttons: icon and hover text. Only what works today: a button that can never be pressed is noise.
/// The toolbar's buttons, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tool {
    Add,
    Import,
    Edit,
    Duplicate,
    Move,
    SignIn,
    TurnOn,
    TurnOff,
    Delete,
    /// Last: it changes nothing.
    Export,
}

impl Tool {
    pub(super) const ALL: [Tool; 10] = [
        Tool::Add,
        Tool::Import,
        Tool::Edit,
        Tool::Duplicate,
        Tool::Move,
        Tool::SignIn,
        Tool::TurnOn,
        Tool::TurnOff,
        Tool::Delete,
        Tool::Export,
    ];

    pub(super) fn glyph(self) -> &'static str {
        match self {
            Tool::Add => icon::PLUS,
            Tool::Import => icon::TRAY_ARROW_DOWN,
            Tool::Edit => icon::PENCIL_SIMPLE,
            Tool::Duplicate => icon::COPY,
            Tool::Move => icon::ARROW_FAT_LINES_RIGHT,
            Tool::SignIn => icon::SIGN_IN,
            Tool::TurnOn => icon::TOGGLE_RIGHT,
            Tool::TurnOff => icon::TOGGLE_LEFT,
            Tool::Delete => icon::TRASH,
            Tool::Export => icon::EXPORT,
        }
    }

    /// What a screen reader calls the button, which shows only its icon (WCAG 4.1.2). Not the bare verb: a dialog's
    /// own button says "Turn off" or "Sign in", and the two must not read the same.
    pub(super) fn name(self) -> &'static str {
        match self {
            Tool::Add => "Add server",
            Tool::Import => "Import from a file",
            Tool::Edit => "Edit server",
            Tool::Duplicate => "Duplicate server",
            Tool::Move => "Move server",
            Tool::SignIn => "Sign in to server",
            Tool::TurnOn => "Turn on servers",
            Tool::TurnOff => "Turn off servers",
            Tool::Delete => "Delete servers",
            Tool::Export => "Export servers",
        }
    }

    /// The hover text. Export's says what it exports, the selection or the whole list, so the toolbar writes it.
    fn tip(self) -> &'static str {
        match self {
            Tool::Add => "Add (Ctrl+N)",
            Tool::Import => "Import from a file (Ctrl+O)",
            Tool::Edit => "Edit (Ctrl+E)",
            Tool::Duplicate => "Duplicate (Ctrl+D)",
            Tool::Move => "Move (Ctrl+Shift+M)",
            Tool::SignIn => "Sign in",
            Tool::TurnOn => "Turn on",
            Tool::TurnOff => "Turn off",
            Tool::Delete => "Delete (Del)",
            Tool::Export => "",
        }
    }
}

impl ShellApp {
    pub(super) fn toolbar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let read_only_hint = self.read_only_hint();
        let read_only = read_only_hint.is_some();
        // A plugin's server is switched like any other (decision 008).
        let can_toggle = self.core.is_some() && !self.selected.is_empty() && self.can_prepare();
        let can_edit = self.core.is_some() && self.selected.len() == 1 && !read_only;
        let can_duplicate = self.duplicable().is_some();
        let can_add = self.core.is_some() && self.can_prepare();
        let can_export = self.can_export();
        let export_tip = if self.selected.is_empty() {
            "Export the servers in the list"
        } else {
            "Export the selected servers"
        };
        let can_sign_in = self.core.is_some()
            && self.sign_in.is_none()
            && self.single_selected().is_some_and(|row| {
                // One is selected: its status as shown now.
                self.selected
                    .iter()
                    .all(|&i| sign_in::offers_sign_in(row, self.effective(i).0))
            });
        // Wraps onto a second line when the window is narrow: buttons past the edge would be cut off and unreachable.
        ui.horizontal_wrapped(|ui| {
            for tool in Tool::ALL {
                let tip = if tool == Tool::Export {
                    export_tip
                } else {
                    tool.tip()
                };
                let button = egui::Button::new(RichText::new(tool.glyph()).size(16.0))
                    .min_size(egui::vec2(44.0, 38.0));
                let (enabled, needs) = match tool {
                    _ if read_only
                        && ![Tool::Add, Tool::Import, Tool::TurnOn, Tool::TurnOff]
                            .contains(&tool) =>
                    {
                        (false, read_only_hint.as_deref().unwrap_or_default())
                    }
                    Tool::TurnOn | Tool::TurnOff | Tool::Delete => {
                        (can_toggle, "select a server first")
                    }
                    Tool::Duplicate | Tool::Move => {
                        (can_duplicate, "select one user or local server")
                    }
                    Tool::SignIn => (can_sign_in, "select one server that needs you to sign in"),
                    Tool::Add => (can_add, "wait for the change under way"),
                    Tool::Import => (
                        can_add && !self.import_waiting,
                        "wait for the change under way",
                    ),
                    Tool::Export => (can_export, "no server to export"),
                    Tool::Edit => (can_edit, "select one server"),
                };
                let hover = if enabled {
                    tip.to_owned()
                } else {
                    format!("{tip}: {needs}")
                };
                let response = ui
                    .add_enabled(enabled, button)
                    .on_hover_text(tip)
                    .on_disabled_hover_text(&hover);
                name_for_screen_readers(&response, tool.name());
                // The hover text, why it is off included, without hovering: its shortcut, or what to select.
                response
                    .ctx
                    .accesskit_node_builder(response.id, |node| node.set_description(hover));
                if response.clicked() {
                    match tool {
                        Tool::TurnOn => self.prepare_toggle(true),
                        Tool::TurnOff => self.prepare_toggle(false),
                        Tool::Delete => self.prepare_delete(),
                        Tool::Duplicate => self.open_duplicate(),
                        Tool::Move => self.open_move(),
                        Tool::Add => self.open_add(),
                        Tool::Import => self.ask_import(),
                        Tool::Export => self.open_export(),
                        Tool::SignIn => {
                            if let Some(s) = self.single_selected().cloned() {
                                self.start_sign_in(&s);
                            }
                        }
                        Tool::Edit => self.edit_selected(),
                    }
                }
            }
            if !self.selected.is_empty() {
                // Only beside the buttons: the detail panel says it too, so a line of its own would only push the
                // list down.
                let text = RichText::new(format!("{} selected", self.selected.len()))
                    .color(pal.text_muted);
                let galley = egui::WidgetText::from(text.clone()).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                );
                let needed = galley.size().x + 2.0 * ui.spacing().item_spacing.x + 8.0;
                if ui.available_size_before_wrap().x >= needed {
                    ui.separator();
                    ui.add(egui::Label::new(text).extend());
                }
            }
        });
    }
}

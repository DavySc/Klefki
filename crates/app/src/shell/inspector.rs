//! The Status tab's "Copy Inspector command": the line that opens the server in the MCP Inspector, to paste in a
//! terminal (decision 014). Klefki never runs it. The engine reads the definition and leaves out what may be a secret.

use super::export::{COPIED_SECS, announce};
use super::{DetailTab, ShellApp};
use crate::theme;
use egui::{Frame, Margin, RichText, Ui};
use klefki_core::api::{Command, InspectorCommand, Scope, ServerId, ServerRow, TransportKind};

pub(super) const HEADING: &str = "MCP Inspector";
pub(super) const ABOUT: &str = "The protocol's own debugging tool: call this server's tools and read what it answers, outside Claude Code.";
pub(super) const COPY: &str = "Copy Inspector command";
const COPY_HOVER: &str = "Copy the line that opens this server in the MCP Inspector, to paste in a terminal. \
     Klefki does not run it, and leaves out environment and header values and anything that looks like a secret.";

/// The Inspector's last answer for one server, and whether the line is still to be copied when it arrives.
pub(super) struct InspectorSeen {
    id: ServerId,
    line: Option<Result<InspectorCommand, String>>,
    copy: bool,
    /// When it was copied, in egui time.
    copied: Option<f64>,
}

/// Whether the Inspector can open `s`: a server of the user's own files that speaks stdio, http or sse. A plugin's,
/// the organisation's and a claude.ai connector's have no definition here, and the Inspector has no WebSocket.
pub(super) fn offers(s: &ServerRow) -> bool {
    matches!(s.scope, Scope::User | Scope::Local | Scope::Project)
        && matches!(
            s.transport,
            TransportKind::Stdio | TransportKind::Http | TransportKind::Sse
        )
}

impl ShellApp {
    /// Asks for the line; it is copied when it arrives.
    pub(super) fn copy_inspector_command(&mut self, id: ServerId) {
        match &mut self.inspector {
            Some(seen) if seen.id == id => seen.copy = true,
            _ => {
                self.inspector = Some(InspectorSeen {
                    id: id.clone(),
                    line: None,
                    copy: true,
                    copied: None,
                });
            }
        }
        self.send(Command::InspectorCommand(id));
    }

    pub(super) fn inspector_arrived(
        &mut self,
        id: &ServerId,
        line: Result<InspectorCommand, String>,
    ) {
        // Not copied after the user went elsewhere: the clipboard is not theirs to have changed then.
        let here =
            self.tab == DetailTab::Status && self.single_selected().is_some_and(|s| s.is(id));
        if let Some(seen) = self.inspector.as_mut().filter(|seen| seen.id == *id) {
            seen.copy &= here;
            seen.line = Some(line);
        }
    }

    /// The section under the last starts: what the Inspector is for, the button, and once it was asked for, the line.
    pub(super) fn inspector_section(&mut self, ui: &mut Ui, s: &ServerRow) {
        if self.core.is_none() || !offers(s) {
            return;
        }
        let pal = theme::palette(ui.ctx());
        ui.add_space(10.0);
        ui.separator();
        ui.label(
            RichText::new(HEADING)
                .family(theme::bold_family())
                .size(16.0),
        );
        ui.add(egui::Label::new(RichText::new(ABOUT).color(pal.text_muted)).wrap());
        ui.add_space(4.0);
        let now = ui.input(|i| i.time);
        let copied = self
            .inspector
            .as_ref()
            .filter(|seen| seen.id == s.id())
            .and_then(|seen| seen.copied)
            .is_some_and(|at| now - at < COPIED_SECS);
        let button = ui
            .button(if copied { "Copied" } else { COPY })
            .on_hover_text(COPY_HOVER);
        announce(ui.ctx(), button.id);
        if button.clicked() {
            self.copy_inspector_command(s.id());
        }
        self.inspector_panel(ui, s);
    }

    /// The line and what to know before running it, once it was asked for. Copies the line the frame it is first
    /// drawn.
    fn inspector_panel(&mut self, ui: &mut Ui, s: &ServerRow) {
        let pal = theme::palette(ui.ctx());
        let now = ui.input(|i| i.time);
        let Some(seen) = self.inspector.as_mut().filter(|seen| seen.id == s.id()) else {
            return;
        };
        let Some(line) = &seen.line else {
            return;
        };
        let muted = |ui: &mut Ui, text: &str| {
            ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
        };
        ui.add_space(6.0);
        match line {
            Ok(found) => {
                if std::mem::take(&mut seen.copy) {
                    ui.ctx().copy_text(found.text.clone());
                    seen.copied = Some(now);
                    // The button above was drawn before this: it says "Copied" in the next frame.
                    ui.ctx().request_repaint();
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS));
                }
                Frame::new()
                    .fill(pal.field)
                    .inner_margin(Margin::same(8))
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&found.text).monospace()).wrap());
                    });
                // What to do is in the text colour, first; what is only good to know is muted.
                for item in &found.to_do {
                    ui.add_space(2.0);
                    ui.add(egui::Label::new(format!("• {item}")).wrap());
                }
                for note in &found.notes {
                    ui.add_space(2.0);
                    muted(ui, &format!("• {note}"));
                }
            }
            Err(why) => {
                seen.copy = false;
                muted(ui, &format!("There is no Inspector command: {why}."));
            }
        }
    }
}

//! The sign-in dialog: Claude Code signs in to a server in the user's browser while Klefki shows where it is.
//!
//! The common path needs nothing from the user but the browser: the page opens, they sign in, the dialog closes. The
//! one fallback, pasting the address the browser ends up on, stays folded away until asked for.
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{Command, ServerId, ServerRow, SignInStep, Status, TransportKind};

/// Whether a server signs in through Claude Code: those reached over HTTP or SSE.
pub(crate) fn can_sign_in(row: &ServerRow) -> bool {
    matches!(row.transport, TransportKind::Http | TransportKind::Sse)
}

/// What the button says: signing in for the first time, or again (re-authorizing).
pub(crate) fn action_label(status: Status) -> &'static str {
    if status == Status::NeedsAuth {
        "Sign in"
    } else {
        "Sign in again"
    }
}

/// An open sign-in dialog.
pub(crate) struct SignInView {
    pub id: ServerId,
    /// The server's address, shown under the title.
    target: String,
    phase: Phase,
    /// The page, once Claude printed it: where it is, the link, and whether the browser opened.
    page: Option<(String, String, bool)>,
    /// The fallback is unfolded.
    paste_open: bool,
    pasted: String,
    /// Put the keyboard in the paste field on the next frame: when it is unfolded, and after a refusal.
    focus_field: bool,
    /// Why the last pasted address was not used.
    refused: Option<String>,
    /// The link was copied (said next to the button, briefly).
    copied_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Starting,
    Waiting,
    /// A pasted address is with Claude.
    Checking,
    Failed(String),
}

/// What the dialog asks of the shell after a frame.
#[derive(Debug, PartialEq)]
pub(crate) enum Next {
    Stay,
    /// Send this to the engine and keep the dialog.
    Send(Command),
    /// Close the dialog, sending this first if any.
    Close(Option<Command>),
}

impl SignInView {
    pub(crate) fn new(row: &ServerRow) -> Self {
        Self {
            id: row.id(),
            target: row.target.clone(),
            phase: Phase::Starting,
            page: None,
            paste_open: false,
            pasted: String::new(),
            focus_field: false,
            refused: None,
            copied_at: None,
        }
    }

    /// Takes in what the engine reported. `true` when the sign-in succeeded: the dialog closes.
    pub(crate) fn step(&mut self, step: SignInStep) -> bool {
        match step {
            SignInStep::Starting => self.phase = Phase::Starting,
            SignInStep::Waiting { host, link, opened } => {
                self.page = Some((host, link, opened));
                if self.phase != Phase::Checking {
                    self.phase = Phase::Waiting;
                }
            }
            SignInStep::Pasted => {
                self.phase = Phase::Checking;
                self.refused = None;
                // Claude has it now; the field does not keep the one-time code.
                self.pasted.clear();
            }
            SignInStep::PasteRefused(why) => {
                if self.phase != Phase::Starting {
                    self.phase = Phase::Waiting;
                }
                self.refused = Some(why);
                self.paste_open = true;
                self.focus_field = true;
            }
            SignInStep::SignedIn => return true,
            SignInStep::Failed(why) => self.phase = Phase::Failed(why),
            // Only sent after the dialog asked for it, and closed.
            SignInStep::Cancelled => {}
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn set_pasted(&mut self, text: &str) {
        self.pasted = text.to_owned();
    }

    #[cfg(test)]
    pub(crate) fn pasted(&self) -> &str {
        &self.pasted
    }

    fn running(&self) -> bool {
        !matches!(self.phase, Phase::Failed(_))
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Next {
        let pal = theme::palette(ctx);
        let now = ctx.input(|i| i.time);
        let mut next = Next::Stay;
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let modal = egui::Modal::new(egui::Id::new("sign_in_dialog")).show(ctx, |ui| {
            ui.set_width(500.0);
            ui.heading(format!("Sign in to {}", self.id.name));
            ui.add(
                egui::Label::new(RichText::new(&self.target).small().color(pal.text_muted))
                    .truncate(),
            );
            ui.add_space(14.0);
            match self.phase.clone() {
                Phase::Starting => busy(ui, pal, "Starting Claude Code…"),
                Phase::Checking => busy(ui, pal, "Claude Code is checking the address…"),
                Phase::Waiting => next = self.waiting(ui, pal, now, enter),
                Phase::Failed(why) => {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icon::WARNING_CIRCLE)
                                .size(18.0)
                                .color(pal.danger),
                        );
                        ui.label(RichText::new("The sign-in did not finish.").strong());
                    });
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(why).wrap());
                }
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if self.running() {
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                        .on_hover_text("Esc. Stops Claude Code's sign-in.")
                        .clicked()
                    {
                        next = Next::Close(Some(Command::CancelSignIn(self.id.clone())));
                    }
                } else {
                    let again =
                        egui::Button::new(RichText::new("Try again").strong().color(pal.on_bright))
                            .fill(pal.primary)
                            .min_size(egui::vec2(96.0, 34.0));
                    if ui.add(again).on_hover_text("Enter").clicked() || enter {
                        next = Next::Send(Command::SignIn(self.id.clone()));
                    }
                    if ui
                        .add(egui::Button::new("Close").min_size(egui::vec2(80.0, 34.0)))
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        next = Next::Close(None);
                    }
                }
            });
        });
        crate::shell::settle(ctx, &modal.response);
        if next == Next::Stay && modal.should_close() {
            next = Next::Close(
                self.running()
                    .then(|| Command::CancelSignIn(self.id.clone())),
            );
        }
        if let Next::Send(Command::SignIn(_)) = next {
            self.restart();
        }
        next
    }

    /// Back to the start, for Try again: nothing from the last attempt is kept.
    fn restart(&mut self) {
        self.phase = Phase::Starting;
        self.page = None;
        self.paste_open = false;
        self.pasted.clear();
        self.focus_field = false;
        self.refused = None;
        self.copied_at = None;
    }

    fn waiting(&mut self, ui: &mut Ui, pal: &theme::Palette, now: f64, enter: bool) -> Next {
        let mut next = Next::Stay;
        let Some((host, link, opened)) = self.page.clone() else {
            return next;
        };
        if opened {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::BROWSER).size(18.0).color(pal.primary));
                ui.label(RichText::new("Finish signing in in your browser.").strong());
            });
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(format!(
                    "The sign-in page at {host} is open in your browser. This closes by itself when you are done."
                ))
                .wrap(),
            );
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING).size(18.0).color(pal.warn));
                ui.label(RichText::new("Klefki could not open your browser.").strong());
            });
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(format!(
                    "Copy the link to the sign-in page at {host} and open it in your browser. This closes by itself when you are done."
                ))
                .wrap(),
            );
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if opened
                && ui
                    .button(format!("{} Open the page again", icon::ARROW_SQUARE_OUT))
                    .on_hover_text(format!("Opens {host} in your browser"))
                    .clicked()
            {
                next = Next::Send(Command::OpenSignInPage(self.id.clone()));
            }
            let copy = if opened {
                egui::Button::new(format!("{} Copy link", icon::COPY))
            } else {
                egui::Button::new(
                    RichText::new(format!("{} Copy link", icon::COPY))
                        .strong()
                        .color(pal.on_bright),
                )
                .fill(pal.primary)
            };
            if ui.add(copy).clicked() {
                ui.ctx().copy_text(link.clone());
                self.copied_at = Some(now);
            }
            if let Some(at) = self.copied_at {
                if now - at < 2.0 {
                    ui.label(RichText::new("Copied").color(pal.success));
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(
                            2.0 - (now - at),
                        ));
                } else {
                    self.copied_at = None;
                }
            }
        });
        ui.add_space(12.0);
        let caret = if self.paste_open {
            icon::CARET_DOWN
        } else {
            icon::CARET_RIGHT
        };
        if ui
            .add(
                egui::Button::new(format!(
                    "{caret} Your browser says it cannot reach localhost?"
                ))
                .frame(false),
            )
            .clicked()
        {
            self.paste_open = !self.paste_open;
            self.focus_field = self.paste_open;
        }
        if self.paste_open {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "That happens when the browser runs on another computer. Sign in there, then copy the \
                         address the browser ends up on (it starts with http://localhost) and paste it here.",
                    )
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            let mut send = false;
            let ready = !self.pasted.trim().is_empty();
            // Right to left, so the button keeps its place and the field takes what is left of the row.
            let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y + 8.0);
            ui.allocate_ui_with_layout(
                row,
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    send = ui
                        .add_enabled(ready, egui::Button::new("Continue"))
                        .clicked();
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.pasted)
                            .hint_text("http://localhost:…/callback?code=…")
                            .desired_width(ui.available_width()),
                    );
                    if std::mem::take(&mut self.focus_field) {
                        field.request_focus();
                    }
                    if field.changed() {
                        self.refused = None;
                    }
                    send |= ready && enter && field.lost_focus();
                },
            );
            if let Some(why) = &self.refused {
                ui.add(egui::Label::new(RichText::new(why).color(pal.danger)).wrap());
            }
            if send {
                // Kept in the field until Claude has it, so an address Klefki refuses can be corrected.
                next = Next::Send(Command::SignInPaste {
                    id: self.id.clone(),
                    address: self.pasted.clone(),
                });
            }
        }
        next
    }
}

/// A short wait. A still icon, not a spinner: a spinner repaints every frame for as long as it shows.
fn busy(ui: &mut Ui, pal: &theme::Palette, text: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(icon::HOURGLASS_MEDIUM)
                .size(18.0)
                .color(pal.text_muted),
        );
        ui.label(RichText::new(text).color(pal.text));
    });
}

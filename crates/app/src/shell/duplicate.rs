//! The Duplicate dialog: a copy of a server under another name or in other places, its address and sign-in
//! fields changed in the same step.

use super::add::{field_note, field_row, field_warning};
use super::places::Places;
use super::{ShellApp, closed_on_purpose, name_problem};
use crate::theme::{self, Palette};
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Change, Command, CopyFields, Part, Scope, ServerId, ServerRow, TransportKind,
};
use std::collections::BTreeSet;

/// What the Duplicate dialog holds: the copy's name, where it goes, and what it changes.
pub(super) struct DuplicateForm {
    source: ServerId,
    /// What is being copied, under the title: `http · https://…`.
    about: String,
    pub(super) name: String,
    pub(super) places: Places,
    /// Give the name field the keyboard on the first frame.
    focus: bool,
    /// The copy's address and sign-in settings.
    pub(super) copy: CopyRead,
    /// The new client's secret is shown while it is checked.
    show_secret: bool,
    /// The fields the keyboard has left: what is wrong with them is said from then on.
    left: BTreeSet<Part>,
    /// The callback port to check, sent to the engine after this frame: when the field is left.
    port_wanted: Option<u16>,
    /// The callback port sent to be checked, until its answer comes: it is asked once.
    port_pending: Option<u16>,
    /// The callback port last checked, and why a sign-in could not come back to it
    /// ([`Event::CallbackPort`](klefki_core::api::Event::CallbackPort)).
    port_checked: Option<(u16, Option<String>)>,
    /// Review was pressed and the plan is being prepared: the dialog is hidden, and comes back as it was if that fails.
    pub(super) reviewing: bool,
    /// Why the last Review failed, said in the dialog.
    pub(super) failed: Option<String>,
}

/// The Duplicate dialog's address and sign-in fields, from the original's definition.
#[derive(Debug, PartialEq)]
pub(super) enum CopyRead {
    /// Asked for ([`Command::Inspect`]); it comes in a moment.
    Reading,
    Ready(Box<CopyFields>),
    /// A program: nothing to change but its name and place.
    NoAddress,
    /// The definition could not be read; the copy is made as it is.
    Failed(String),
}

impl CopyRead {
    /// What the copy takes from the original, said under the places.
    fn keeps(&self) -> &'static str {
        match self {
            CopyRead::Ready(f) if f.client_changed() => {
                "The copy keeps the original's environment and header values, and signs in as \
                 the client above."
            }
            CopyRead::Ready(f) if f.oauth => {
                "The copy keeps the original's environment and header values, and its client \
                 secret."
            }
            _ => "The copy keeps the original's environment and header values.",
        }
    }
}

impl DuplicateForm {
    /// Back from a review that failed, as it was, so nothing typed is lost; the dialog says why.
    pub(super) fn review_failed(&mut self, why: String) {
        self.reviewing = false;
        self.failed = Some(why);
    }

    /// What the engine found about the OAuth callback `port`: whether a sign-in could come back to it now.
    pub(super) fn port_answered(&mut self, port: u16, warning: Option<String>) {
        if self.port_pending == Some(port) {
            self.port_pending = None;
        }
        self.port_checked = Some((port, warning));
    }

    /// Why a sign-in could not come back to the callback port, as last checked: only while it is still the one typed.
    fn port_warning(&self) -> Option<&str> {
        let CopyRead::Ready(fields) = &self.copy else {
            return None;
        };
        let (port, warning) = self.port_checked.as_ref()?;
        (fields.oauth && fields.typed_port() == Some(*port))
            .then_some(warning.as_deref())
            .flatten()
    }
}

/// Width of the labels of the Duplicate dialog's address and sign-in fields.
const COPY_LABEL_WIDTH: f32 = 124.0;

/// The Duplicate dialog's address and sign-in fields, filled in from the original: what overlapping servers differ
/// in, so a copy can be made and changed in one step. The addresses the copy gets are shown as they are typed.
fn copy_fields(ui: &mut Ui, pal: &Palette, form: &mut DuplicateForm) {
    let fields = match &mut form.copy {
        CopyRead::NoAddress => return,
        CopyRead::Reading => {
            // A local file read takes milliseconds: a spinner would only flicker, and keep the window repainting.
            ui.add_space(8.0);
            ui.label(
                RichText::new("Reading its address…")
                    .small()
                    .color(pal.text_muted),
            );
            return;
        }
        CopyRead::Failed(why) => {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Its address can't be changed here: {why}. The copy is made as it is."
                    ))
                    .small()
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            return;
        }
        CopyRead::Ready(fields) => fields,
    };
    let problems = fields.problems();
    // Said once the field is left, not while it is typed in: a half-typed address is not wrong yet. From then on
    // it is said as it is, so it goes as soon as it is fixed.
    let left = &mut form.left;
    let problem = |part: Part, left: &BTreeSet<Part>| {
        problems
            .iter()
            .find(|(p, _)| *p == part && left.contains(p))
            .map(|(_, why)| why.clone())
    };
    let wide = (ui.available_width() - COPY_LABEL_WIDTH - 8.0).max(120.0);
    ui.add_space(10.0);
    let label = if fields.base_is_whole_address() {
        "Address"
    } else {
        "Base address"
    };
    let base = field_row(ui, pal, label, COPY_LABEL_WIDTH, |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut fields.base)
                .id(egui::Id::new("copy_base"))
                .hint_text("https://…")
                .desired_width(wide),
        )
    });
    if base.lost_focus() {
        fields.complete_base();
        left.extend([Part::Address, Part::MetadataUrl]);
    }
    // What the base makes of each address, under it: the part kept is the original's.
    let (url, metadata) = fields.addresses();
    let mut shown = Vec::new();
    if !fields.base_is_whole_address() {
        shown.push(format!("Server: {url}"));
    }
    match metadata {
        Some((m, true)) => shown.push(format!("Sign-in metadata: {m}")),
        Some((m, false)) => shown.push(format!("Sign-in metadata: {m} (another host, kept)")),
        None => {}
    }
    // The addresses stay in view with a problem, which is said under them.
    field_note(ui, pal, COPY_LABEL_WIDTH, None, &shown.join("\n"));
    let address_problem = problem(Part::Address, left).or_else(|| problem(Part::MetadataUrl, left));
    field_note(ui, pal, COPY_LABEL_WIDTH, address_problem.as_deref(), "");
    if !fields.oauth {
        return;
    }
    let half = wide.min(260.0);
    let client = field_row(ui, pal, "Client ID", COPY_LABEL_WIDTH, |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut fields.client_id)
                .id(egui::Id::new("copy_client_id"))
                .hint_text("none: Claude Code registers itself")
                .desired_width(half),
        )
    });
    if client.lost_focus() {
        left.insert(Part::ClientId);
    }
    field_note(
        ui,
        pal,
        COPY_LABEL_WIDTH,
        problem(Part::ClientId, left).as_deref(),
        "",
    );
    // Another client has its own secret; the original's is not handed to it.
    if fields.client_changed() {
        let show = &mut form.show_secret;
        let secret = &mut fields.secret;
        field_row(ui, pal, "Client secret", COPY_LABEL_WIDTH, |ui| {
            ui.add(
                egui::TextEdit::singleline(secret.edit())
                    .id(egui::Id::new("copy_client_secret"))
                    .password(!*show)
                    .hint_text("optional")
                    .desired_width(half),
            );
            let (glyph, word) = if *show {
                (icon::EYE_SLASH, "Hide")
            } else {
                (icon::EYE, "Show")
            };
            let label = format!("{word} the client secret");
            let response = ui
                .small_button(format!("{glyph} {word}"))
                .on_hover_text(&label);
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
            if response.clicked() {
                *show = !*show;
            }
        });
        field_note(
            ui,
            pal,
            COPY_LABEL_WIDTH,
            None,
            "For the new client ID; the original's is not used. Leave it empty if the client has none.",
        );
    }
    let port = field_row(ui, pal, "Callback port", COPY_LABEL_WIDTH, |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut fields.callback_port)
                .id(egui::Id::new("copy_callback_port"))
                .hint_text("any")
                .desired_width(96.0),
        )
    });
    let typed = fields.typed_port();
    if port.lost_focus() {
        left.insert(Part::CallbackPort);
    }
    let port_problem = problem(Part::CallbackPort, left);
    field_note(ui, pal, COPY_LABEL_WIDTH, port_problem.as_deref(), "");
    if port.lost_focus() && typed.is_some() && typed != form.port_pending {
        form.port_wanted = typed;
    }
    if port_problem.is_none()
        && let Some(warning) = form.port_warning()
    {
        field_warning(ui, pal, COPY_LABEL_WIDTH, warning);
    }
}

impl ShellApp {
    /// The one selected server, if it can be duplicated (project servers cannot yet).
    pub(super) fn duplicable(&self) -> Option<&ServerRow> {
        self.core.as_ref()?;
        if !self.can_prepare() {
            return None;
        }
        self.single_selected()
            .filter(|s| matches!(s.scope, Scope::User | Scope::Local))
    }

    /// Opens the Duplicate dialog for the selected server: named `<name>_copy`, in the place it is now.
    pub(super) fn open_duplicate(&mut self) {
        let Some(row) = self.duplicable() else {
            return;
        };
        let mut places = Places::new(&self.snapshot.projects, row.project.as_deref());
        places.user = row.scope == Scope::User;
        places.tick_here();
        // A copy's address and sign-in can be changed: read from the definition in the file, which has the whole
        // address (the list leaves out a query string).
        let remote = matches!(row.transport, TransportKind::Http | TransportKind::Sse);
        let source = row.id();
        self.duplicate = Some(DuplicateForm {
            source: source.clone(),
            about: format!("{} \u{b7} {}", row.transport.label(), row.target),
            name: format!("{}_copy", row.name),
            places,
            focus: true,
            copy: if remote {
                CopyRead::Reading
            } else {
                CopyRead::NoAddress
            },
            show_secret: false,
            left: BTreeSet::new(),
            port_wanted: None,
            port_pending: None,
            port_checked: None,
            reviewing: false,
            failed: None,
        });
        if remote {
            self.send(Command::Inspect(source));
        }
    }

    pub(super) fn duplicate_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(form) = self.duplicate.as_mut().filter(|f| !f.reviewing) else {
            return;
        };
        let problem = name_problem(&form.name);
        let taken = form.places.taken(&form.name, &self.snapshot.servers);
        let places = form.places.in_words();
        let changes_ok = match &form.copy {
            CopyRead::Ready(fields) => fields.problems().is_empty(),
            // A moment: what the copy may change is not known yet.
            CopyRead::Reading => false,
            CopyRead::NoAddress | CopyRead::Failed(_) => true,
        };
        let ready = problem.is_none() && taken.is_none() && places.is_some() && changes_ok;
        let mut review = ready && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        // The list of projects gets what the window has left once the rest of the dialog is laid out.
        let fields_height = match &form.copy {
            CopyRead::Ready(f) if f.client_changed() => 250.0,
            CopyRead::Ready(f) if f.oauth => 190.0,
            CopyRead::Ready(_) => 80.0,
            _ => 0.0,
        };
        let list_height = (ctx.content_rect().height() - 500.0 - fields_height).clamp(96.0, 320.0);
        // Why the last Review failed takes room by the buttons: a plain line and, small, what the tool said.
        let failed_height = if form.failed.is_some() { 150.0 } else { 0.0 };
        // What the window leaves for the dialog's body, below its title and above its buttons.
        let body_height = (ctx.content_rect().height() - 260.0 - failed_height).max(120.0);
        let modal = egui::Modal::new(egui::Id::new("duplicate_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(format!("Duplicate {}", form.source.name));
            ui.add(
                egui::Label::new(RichText::new(&form.about).small().color(pal.text_muted))
                    .truncate(),
            );
            // On a short window the body scrolls; the title and the buttons stay in view.
            egui::ScrollArea::vertical()
                .max_height(body_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Name").color(pal.text_muted));
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut form.name).desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut form.focus) {
                        field.request_focus();
                    }
                    if let Some(problem) = &problem {
                        ui.label(RichText::new(problem).small().color(pal.danger));
                    }
                    copy_fields(ui, pal, form);
                    ui.add_space(12.0);
                    ui.label(RichText::new("Add it").color(pal.text_muted));
                    form.places.ui(ui, list_height, Some("where it is now"));
                });
            // Outside the scrolled body: what Review will lead to, or why it cannot yet, stays by the button.
            ui.add_space(10.0);
            if let Some(failed) = &form.failed {
                // As in Add: what happened and what to do first; what a tool said small and muted, under it.
                let (plain, detail) = failed.split_once('\n').unwrap_or((failed, ""));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::WARNING_CIRCLE).color(pal.danger));
                    ui.add(egui::Label::new(RichText::new(plain).color(pal.danger)).wrap());
                });
                if !detail.is_empty() {
                    ui.add(
                        egui::Label::new(RichText::new(detail).small().color(pal.text_muted))
                            .wrap(),
                    );
                }
            }
            if let Some(taken) = &taken {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} already exists {taken}. Choose another name, or untick it there.",
                            form.name
                        ))
                        .color(pal.danger),
                    )
                    .wrap(),
                );
            } else if let Some(places) = &places {
                if problem.is_none() {
                    ui.add(egui::Label::new(format!("Adds {} {places}.", form.name)).wrap());
                }
            } else {
                ui.label(RichText::new("Tick at least one place.").color(pal.text_muted));
            }
            let keeps = form.copy.keeps();
            ui.add(egui::Label::new(RichText::new(keeps).small().color(pal.text_muted)).wrap());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let confirm =
                    egui::Button::new(RichText::new("Review").strong().color(pal.on_bright))
                        .fill(pal.primary)
                        .min_size(egui::vec2(96.0, 34.0));
                review |= ui
                    .add_enabled(ready, confirm)
                    .on_hover_text("Enter")
                    .on_disabled_hover_text("Fix what is marked, and tick a place")
                    .clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
        });
        if let Some(port) = form.port_wanted.take() {
            form.port_pending = Some(port);
            self.send(Command::CheckCallbackPort(port));
        }
        let Some(form) = &mut self.duplicate else {
            return;
        };
        if review {
            let adjust = match &mut form.copy {
                CopyRead::Ready(fields) => {
                    let adjust = fields.adjust().unwrap_or_default();
                    // The form keeps the secret typed (wiped when dropped) until the plan is made, so a Review that
                    // fails can be pressed again without typing it again.
                    if let Some(Some(secret)) = adjust.client.as_ref().map(|c| &c.secret) {
                        fields.secret = secret.clone();
                    }
                    adjust
                }
                _ => Default::default(),
            };
            let change = Change::Duplicate {
                id: form.source.clone(),
                name: form.name.clone(),
                targets: form.places.targets(),
                adjust,
            };
            // A copy that signs in has no sign-in of its own yet.
            let signs_in = self
                .snapshot
                .servers
                .iter()
                .any(|r| r.id() == form.source && r.oauth.is_some());
            // Kept, hidden, until the plan comes: if it cannot be made, nothing typed is lost.
            form.reviewing = true;
            form.failed = None;
            self.prepare(vec![change]);
            self.adding_signs_in = signs_in;
        } else if cancel || closed_on_purpose(&modal) {
            self.duplicate = None;
        }
    }

    /// The definition Duplicate asked for arrived: what can be copied from it, or why nothing can.
    pub(super) fn copy_read(&mut self, id: &ServerId, json: &Result<String, String>) {
        if let Some(form) = &mut self.duplicate
            && form.source == *id
            && form.copy == CopyRead::Reading
        {
            form.copy = match json {
                Ok(text) => CopyFields::from_definition(text)
                    .map_or(CopyRead::NoAddress, |f| CopyRead::Ready(Box::new(f))),
                Err(why) => CopyRead::Failed(why.clone()),
            };
        }
    }
}

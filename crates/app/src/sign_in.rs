//! The sign-in dialog: Claude Code signs in to a server in the user's browser while Klefki shows where it is.
//!
//! The common path needs nothing from the user but the browser: the page opens, they sign in, the dialog closes. The
//! one fallback, pasting the address the browser ends up on, stays folded away until asked for.
use crate::theme;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Command, ServerId, ServerRow, SignInFailure, SignInStep, Status, TransportKind,
};

/// Whether a server signs in through Claude Code: those reached over HTTP or SSE.
pub(crate) fn can_sign_in(row: &ServerRow) -> bool {
    matches!(row.transport, TransportKind::Http | TransportKind::Sse)
}

/// Whether signing in is offered up front (Status tab, toolbar): when Claude Code said the server needs it, or the
/// server has OAuth set up. Any HTTP or SSE server can still sign in from its Auth tab and the palette.
pub(crate) fn offers_sign_in(row: &ServerRow, status: Status) -> bool {
    can_sign_in(row) && (status == Status::NeedsAuth || row.oauth.is_some())
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
    /// How long Claude Code waits for the browser, and when that ends (in the UI's clock, set on the first frame).
    wait: f64,
    ends_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Starting,
    Waiting,
    /// A pasted address is with Claude.
    Checking,
    Failed(SignInFailure),
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
            wait: 0.0,
            ends_at: None,
        }
    }

    /// Takes in what the engine reported. `true` when the sign-in succeeded: the dialog closes.
    pub(crate) fn step(&mut self, step: SignInStep) -> bool {
        match step {
            SignInStep::Starting { wait } => {
                self.phase = Phase::Starting;
                self.wait = wait.as_secs_f64();
                self.ends_at = None;
            }
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
    pub(crate) fn set_ends_at(&mut self, at: f64) {
        self.ends_at = Some(at);
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
        let id = egui::Id::new("sign_in_dialog");
        // Anchored at the top, so it grows downwards and its buttons stay where they were.
        let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, [0.0, 96.0]);
        if self.ends_at.is_none() && self.wait > 0.0 {
            self.ends_at = Some(now + self.wait);
        }
        let modal = egui::Modal::new(id).area(area).show(ctx, |ui| {
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
                Phase::Failed(failure) => failed(ui, pal, &failure),
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
                } else if self.can_retry() {
                    if ui
                        .add(primary(pal, "Try again", 96.0))
                        .on_hover_text("Enter")
                        .clicked()
                        || enter
                    {
                        next = Next::Send(Command::SignIn(self.id.clone()));
                    }
                    if ui
                        .add(egui::Button::new("Close").min_size(egui::vec2(80.0, 34.0)))
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        next = Next::Close(None);
                    }
                } else if ui
                    .add(primary(pal, "Close", 80.0))
                    .on_hover_text("Enter or Esc")
                    .clicked()
                    || enter
                {
                    // Starting it again would end the same way: closing is the way on.
                    next = Next::Close(None);
                }
            });
        });
        crate::shell::settle(ctx, &modal.response);
        // A click beside the dialog does not stop a sign-in that runs: it is most likely the click that brings
        // Klefki back from the browser. Esc and Cancel do.
        let stray = self.running() && modal.backdrop_response.clicked();
        if next == Next::Stay && !stray && modal.should_close() {
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
        self.ends_at = None;
    }

    fn can_retry(&self) -> bool {
        matches!(&self.phase, Phase::Failed(f) if f.can_retry)
    }

    /// How long Klefki still waits, and a way to wait longer once that is short (WCAG 2.2.1: warned, and at least
    /// 20 seconds to extend with one action). It repaints only when what it says changes.
    fn time_left(&mut self, ui: &mut Ui, pal: &theme::Palette, now: f64) -> Next {
        let Some(ends_at) = self.ends_at else {
            return Next::Stay;
        };
        let left = (ends_at - now).max(0.0);
        let ctx = ui.ctx().clone();
        let mut next = Next::Stay;
        if left > WARN_BEFORE {
            let minutes = (left / 60.0).ceil();
            let text = if minutes <= 1.0 {
                "This closes by itself when you are done. Klefki waits about a minute more."
                    .to_owned()
            } else {
                format!(
                    "This closes by itself when you are done. Klefki waits {minutes} more minutes."
                )
            };
            ui.add(egui::Label::new(text).wrap());
            let change = (left - (minutes - 1.0) * 60.0).min(left - WARN_BEFORE);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(change.max(0.05)));
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(match left.ceil() {
                        1.0 => "Klefki stops waiting in 1 second.".to_owned(),
                        n => format!("Klefki stops waiting in {n} seconds."),
                    })
                    .color(pal.warn),
                );
                if ui.button("Keep waiting").clicked() {
                    self.ends_at = Some(now + self.wait);
                    next = Next::Send(Command::KeepWaitingForSignIn(self.id.clone()));
                }
            });
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (left - left.ceil() + 1.0).clamp(0.05, 1.0),
            ));
        }
        next
    }

    fn waiting(&mut self, ui: &mut Ui, pal: &theme::Palette, now: f64, enter: bool) -> Next {
        let mut next;
        let Some((host, link, opened)) = self.page.clone() else {
            return Next::Stay;
        };
        if opened {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::BROWSER).size(18.0).color(pal.primary));
                ui.label(RichText::new("Finish signing in in your browser.").strong());
            });
            ui.add_space(4.0);
            with_host(ui, "The sign-in page at", &host, "is open in your browser.");
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING).size(18.0).color(pal.warn));
                ui.label(RichText::new("Klefki could not open your browser.").strong());
            });
            ui.add_space(4.0);
            with_host(
                ui,
                "Copy the link to the sign-in page at",
                &host,
                "and open it in your browser.",
            );
        }
        if let Some(own) = elsewhere(&host, &self.target) {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "That is not the server's own address ({own}). Many servers sign in through another site; \
                         go on only if you expect {host}."
                    ))
                    .color(pal.text_muted),
                )
                .wrap(),
            );
        }
        ui.add_space(4.0);
        next = self.time_left(ui, pal, now);
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

/// Hosting suffixes under which every subdomain belongs to someone else.
const SHARED_SUFFIXES: [&str; 12] = [
    "github.io",
    "gitlab.io",
    "vercel.app",
    "netlify.app",
    "pages.dev",
    "workers.dev",
    "fly.dev",
    "onrender.com",
    "herokuapp.com",
    "azurewebsites.net",
    "cloudfront.net",
    "amazonaws.com",
];

/// When the dialog offers to wait longer.
const WARN_BEFORE: f64 = 30.0;

fn primary(pal: &theme::Palette, text: &str, width: f32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).strong().color(pal.on_bright))
        .fill(pal.primary)
        .min_size(egui::vec2(width, 34.0))
}

/// What went wrong, what to do, and Claude's own words below.
fn failed(ui: &mut Ui, pal: &theme::Palette, failure: &SignInFailure) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(icon::WARNING_CIRCLE)
                .size(18.0)
                .color(pal.danger),
        );
        ui.add(egui::Label::new(RichText::new(&failure.title).strong()).wrap());
    });
    for help in &failure.help {
        ui.add_space(4.0);
        ui.add(egui::Label::new(help).wrap());
    }
    if let Some(said) = &failure.said {
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                RichText::new(format!("Claude Code said: {said}"))
                    .small()
                    .color(pal.text_muted),
            )
            .wrap(),
        );
    }
}

/// A sentence with the host in bold: it is what the user checks in the browser. One label, so it wraps as text.
fn with_host(ui: &mut Ui, before: &str, host: &str, after: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let visuals = ui.visuals();
    let format = |color| egui::TextFormat::simple(font.clone(), color);
    let mut job = egui::text::LayoutJob::default();
    job.append(&format!("{before} "), 0.0, format(visuals.text_color()));
    let bold = egui::FontId::new(font.size, theme::bold_family());
    job.append(
        host,
        0.0,
        egui::TextFormat::simple(bold, visuals.strong_text_color()),
    );
    job.append(&format!(" {after}"), 0.0, format(visuals.text_color()));
    ui.add(egui::Label::new(job).wrap());
}

/// The server's own host when the sign-in page is somewhere else. Hosts on the same registered domain
/// (`auth.example.com` for `mcp.example.com`) count as the same place; under a suffix that many owners share
/// (`co.uk`, `github.io`) one more label is needed. A rough rule, which errs towards pointing it out.
fn elsewhere(page_host: &str, target: &str) -> Option<String> {
    let rest = target.split_once("://").map_or(target, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let own = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    let page = page_host.trim_start_matches('[').trim_end_matches(']');
    let site = |h: &str| {
        let h = h.to_ascii_lowercase();
        let labels: Vec<&str> = h.rsplit('.').collect();
        let shared = match labels.as_slice() {
            [tld, second, ..] => {
                (tld.len() == 2 && second.len() <= 3)
                    || SHARED_SUFFIXES.contains(&format!("{second}.{tld}").as_str())
            }
            _ => false,
        };
        let take = if shared { 3 } else { 2 };
        labels
            .iter()
            .take(take)
            .map(|l| (*l).to_owned())
            .collect::<Vec<_>>()
    };
    (!own.is_empty() && site(own) != site(page)).then(|| own.to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_page_elsewhere_is_told_apart_from_one_on_the_same_site() {
        let same = [
            ("auth.example.com", "https://mcp.example.com/mcp"),
            ("localhost", "https://localhost:8443/mcp"),
            ("login.example.co.uk", "https://api.example.co.uk/mcp"),
            (
                "AUTH.Example.com",
                "https://user:pw@mcp.example.com:8443/x?y#z",
            ),
            ("::1", "http://[::1]:9/mcp"),
        ];
        for (page, target) in same {
            assert_eq!(elsewhere(page, target), None, "{page} for {target}");
        }
        let other = [
            (
                "login.other.org",
                "https://mcp.example.com/mcp",
                "mcp.example.com",
            ),
            ("b.co.uk", "https://a.co.uk/mcp", "a.co.uk"),
            (
                "evil.github.io",
                "https://tool.github.io/mcp",
                "tool.github.io",
            ),
            ("x.vercel.app", "https://y.vercel.app/mcp", "y.vercel.app"),
        ];
        for (page, target, own) in other {
            assert_eq!(
                elsewhere(page, target).as_deref(),
                Some(own),
                "{page} for {target}"
            );
        }
    }
}

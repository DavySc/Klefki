//! Failures laid out the way the Rust compiler lays out an error: what went wrong, where, what to do, and the
//! client's own words last.
//!
//! ```text
//! error[ENOTFOUND]: Can't reach the server's host
//!   --> acme-prod (User)
//!    |
//!    | https://acme-prod.example.com/mcp
//!    |         ^^^^^^^^^^^^^^^^^^^^^ this name could not be looked up
//!    |
//!    = help: You are probably offline or not connected to your VPN: check those first.
//!    = help: If both are fine, check the URL for typos.
//!    = note: Claude Code said: ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com
//! ```
//!
//! Every font in the app is monospaced, so the markers line up under the part they point at.

use crate::theme;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, RichText, Ui};
use klefki_core::api::{Hint, ServerRow, Status};
use std::fmt::Write;

const ARROW: &str = "  --> ";
const BAR: &str = "   |";
const EQ: &str = "   = ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Warning,
}

impl Level {
    fn of(status: Status) -> Self {
        if status == Status::Failed {
            Self::Error
        } else {
            Self::Warning
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    /// Characters before the marked part.
    pad: usize,
    width: usize,
    label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Explanation {
    level: Level,
    code: Option<String>,
    title: String,
    place: String,
    source: String,
    marker: Option<Marker>,
    help: Vec<String>,
    note: Option<String>,
}

impl Explanation {
    /// For a row's live status. `None` when there is nothing wrong to explain.
    pub(crate) fn for_status(
        row: &ServerRow,
        status: Status,
        detail: Option<&str>,
        hint: Option<&Hint>,
    ) -> Option<Self> {
        let fallback = match status {
            Status::Failed => "Claude Code could not connect to this server",
            Status::NeedsAuth => "This server needs you to sign in",
            Status::Degraded => "Connected, but listing the server's tools failed",
            _ => return None,
        };
        let mut e = Self::build(row, status, hint, fallback, detail);
        if hint.is_none() && status != Status::NeedsAuth {
            e.help
                .push("Diagnose shows what the server and Claude Code logged.".to_owned());
        }
        if hint.is_none() && status == Status::NeedsAuth {
            e.help.push(
                "Use Sign in: Claude Code opens the sign-in page in your browser, and Klefki waits until you are done."
                    .to_owned(),
            );
        }
        // Claude Code's own words stay: they are what to search for or quote.
        e.note = detail.map(|d| format!("Claude Code said: {d}"));
        Some(e)
    }

    /// For one of the causes Diagnose found. `said` is Claude's message, to find the program in a command line.
    pub(crate) fn for_hint(
        row: &ServerRow,
        status: Status,
        hint: &Hint,
        said: Option<&str>,
    ) -> Self {
        Self::build(row, status, Some(hint), "", said)
    }

    fn build(
        row: &ServerRow,
        status: Status,
        hint: Option<&Hint>,
        fallback: &str,
        said: Option<&str>,
    ) -> Self {
        let place = match &row.project {
            Some(project) => format!("{} ({}, {project})", row.name, row.scope.label()),
            None => format!("{} ({})", row.name, row.scope.label()),
        };
        let marker = hint.and_then(|h| {
            let range = h.span(&row.target, said)?;
            let (_, label) = h.points_at.as_ref()?;
            Some(Marker {
                pad: row.target[..range.start].chars().count(),
                width: row.target[range].chars().count().max(1),
                label: label.clone(),
            })
        });
        Self {
            level: Level::of(status),
            code: hint.and_then(|h| h.code.clone()),
            title: hint.map_or_else(|| fallback.to_owned(), |h| h.title.clone()),
            place,
            source: row.target.clone(),
            marker,
            help: hint.map(|h| h.help.clone()).unwrap_or_default(),
            note: None,
        }
    }

    fn head(&self) -> String {
        match &self.code {
            Some(code) => format!("{}[{code}]: ", self.level.word()),
            None => format!("{}: ", self.level.word()),
        }
    }

    fn marks(m: &Marker) -> String {
        format!("{}{} {}", " ".repeat(m.pad), "^".repeat(m.width), m.label)
    }

    /// The marker line for a line `columns` characters wide, and the label's indent and text when it has to go on
    /// a line of its own. The compiler puts the label after the markers; here that holds whenever it fits, and
    /// otherwise the label goes under the start of the markers, never cut off.
    fn marker_lines(m: &Marker, columns: usize) -> (String, Option<(usize, &str)>) {
        let gutter = BAR.len() + 1;
        if gutter + m.pad + m.width + 1 + m.label.chars().count() <= columns {
            return (Self::marks(m), None);
        }
        let marks = format!("{}{}", " ".repeat(m.pad), "^".repeat(m.width));
        // Under the markers, unless that leaves the label too little room to read.
        const ROOM: usize = 24;
        let indent = m.pad.min(columns.saturating_sub(gutter + ROOM));
        (marks, Some((indent, &m.label)))
    }

    /// The same lines as plain text, for a report to paste.
    pub(crate) fn text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}{}", self.head(), self.title);
        let _ = writeln!(out, "{ARROW}{}", self.place);
        let _ = writeln!(out, "{BAR}");
        let _ = writeln!(out, "{BAR} {}", self.source);
        if let Some(m) = &self.marker {
            let _ = writeln!(out, "{BAR} {}", Self::marks(m));
        }
        if !self.help.is_empty() || self.note.is_some() {
            let _ = writeln!(out, "{BAR}");
        }
        for help in &self.help {
            let _ = writeln!(out, "{EQ}help: {help}");
        }
        if let Some(note) = &self.note {
            let _ = writeln!(out, "{EQ}note: {note}");
        }
        out
    }

    pub(crate) fn show(&self, ui: &mut Ui, id: egui::Id) {
        let pal = theme::palette(ui.ctx());
        let size = egui::TextStyle::Body.resolve(ui.style()).size;
        let regular = FontId::monospace(size);
        let bold = FontId::new(size, theme::bold_family());
        let fmt = |font: &FontId, color: Color32| TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        };
        let accent = match self.level {
            Level::Error => pal.danger,
            Level::Warning => pal.warn,
        };
        let gutter = fmt(&bold, pal.link);
        let job = |parts: &[(&str, &TextFormat)]| {
            let mut job = LayoutJob::default();
            for (text, format) in parts {
                job.append(text, 0.0, (*format).clone());
            }
            job
        };
        // A prefix, then text that wraps under itself rather than under the prefix.
        let prefixed = |ui: &mut Ui, prefix: LayoutJob, body: RichText| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(prefix);
                ui.add(egui::Label::new(body).wrap());
            });
        };

        // Straight on the panel, like the rest of it: the gutter gives the lines their structure, a box would only
        // set them apart.
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            prefixed(
                ui,
                job(&[(&self.head(), &fmt(&bold, accent))]),
                RichText::new(&self.title)
                    .font(bold.clone())
                    .color(pal.text),
            );
            prefixed(
                ui,
                job(&[(ARROW, &gutter)]),
                RichText::new(&self.place)
                    .font(regular.clone())
                    .color(pal.text),
            );
            ui.label(job(&[(BAR, &gutter)]));
            let bar = format!("{BAR} ");
            // How many characters fit on a line: every font here is monospaced.
            let char_width = ui
                .painter()
                .layout_no_wrap("M".to_owned(), regular.clone(), pal.text)
                .size()
                .x
                .max(1.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a float width, never negative here, as a whole column count"
            )]
            let columns = (ui.available_width() / char_width) as usize;
            let marker = self.marker.as_ref().map(|m| Self::marker_lines(m, columns));
            // The command or URL is shown whole on one line, so the markers stay under their part; a long
            // one scrolls sideways instead of wrapping or widening the panel.
            egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add(
                    egui::Label::new(job(&[
                        (&bar, &gutter),
                        (&self.source, &fmt(&regular, pal.text)),
                    ]))
                    .extend(),
                );
                if let Some((marks, _)) = &marker {
                    ui.add(
                        egui::Label::new(job(&[(&bar, &gutter), (marks, &fmt(&bold, accent))]))
                            .extend(),
                    );
                }
            });
            if let Some((_, Some((indent, label)))) = &marker {
                let pad = " ".repeat(*indent);
                prefixed(
                    ui,
                    job(&[(&bar, &gutter), (&pad, &gutter)]),
                    RichText::new(*label).font(bold.clone()).color(accent),
                );
            }
            if !self.help.is_empty() || self.note.is_some() {
                ui.label(job(&[(BAR, &gutter)]));
            }
            for help in &self.help {
                prefixed(
                    ui,
                    job(&[(EQ, &gutter), ("help: ", &fmt(&bold, pal.success))]),
                    RichText::new(help).font(regular.clone()).color(pal.text),
                );
            }
            if let Some(note) = &self.note {
                prefixed(
                    ui,
                    job(&[(EQ, &gutter), ("note: ", &fmt(&bold, pal.text))]),
                    RichText::new(note)
                        .font(regular.clone())
                        .color(pal.text_muted),
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klefki_core::api::{Focus, Scope, TransportKind};

    fn row(name: &str, target: &str) -> ServerRow {
        let mut row = klefki_core::api::fixture_snapshot().servers[0].clone();
        row.name = name.to_owned();
        row.scope = Scope::User;
        row.project = None;
        row.transport = if target.contains("://") {
            TransportKind::Http
        } else {
            TransportKind::Stdio
        };
        row.target = target.to_owned();
        row
    }

    fn unknown_host() -> Hint {
        Hint {
            title: "Can't reach the server's host".to_owned(),
            code: Some("ENOTFOUND".to_owned()),
            help: vec![
                "You are probably offline or not connected to your VPN: check those first."
                    .to_owned(),
                "If both are fine, check the URL for typos.".to_owned(),
            ],
            points_at: Some((Focus::Host, "this name could not be looked up".to_owned())),
        }
    }

    #[test]
    fn an_unknown_host_reads_like_a_compiler_error() {
        let said = "ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com";
        let e = Explanation::for_status(
            &row("acme-prod", "https://acme-prod.example.com/mcp"),
            Status::Failed,
            Some(said),
            Some(&unknown_host()),
        )
        .unwrap();
        assert_eq!(
            e.text(),
            "error[ENOTFOUND]: Can't reach the server's host\n\
             \x20 --> acme-prod (User)\n\
             \x20  |\n\
             \x20  | https://acme-prod.example.com/mcp\n\
             \x20  |         ^^^^^^^^^^^^^^^^^^^^^ this name could not be looked up\n\
             \x20  |\n\
             \x20  = help: You are probably offline or not connected to your VPN: check those first.\n\
             \x20  = help: If both are fine, check the URL for typos.\n\
             \x20  = note: Claude Code said: ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com\n"
        );
    }

    #[test]
    fn the_marker_sits_under_its_part_whatever_comes_before_it() {
        // Characters, not bytes: a non-ASCII host or path before the part must not shift the marker.
        for (target, part) in [
            ("https://münchen.example/mcp", "münchen.example"),
            ("wss://h.example:8443/ws", "h.example"),
        ] {
            let e = Explanation::for_status(
                &row("x", target),
                Status::Failed,
                None,
                Some(&unknown_host()),
            )
            .unwrap();
            let text = e.text();
            let lines: Vec<&str> = text.lines().collect();
            let source = lines.iter().find(|l| l.contains(target)).unwrap();
            let marks = lines.iter().find(|l| l.contains('^')).unwrap();
            let col = |line: &str, needle: &str| line[..line.find(needle).unwrap()].chars().count();
            assert_eq!(col(source, part), col(marks, "^"), "{text}");
            assert_eq!(marks.matches('^').count(), part.chars().count(), "{text}");
        }
    }

    #[test]
    fn a_label_that_does_not_fit_after_the_markers_goes_under_them_whole() {
        let m = Marker {
            pad: 8,
            width: 17,
            label: "this name could not be looked up".to_owned(),
        };
        // Wide enough: on the marker line, as the compiler does.
        let (line, below) = Explanation::marker_lines(&m, 80);
        assert_eq!(
            line,
            format!("{}{} {}", " ".repeat(8), "^".repeat(17), m.label)
        );
        assert_eq!(below, None);
        // The exact fit still counts as fitting; one column less does not.
        let exact = BAR.len() + 1 + 8 + 17 + 1 + m.label.len();
        assert_eq!(Explanation::marker_lines(&m, exact).1, None);
        let (line, below) = Explanation::marker_lines(&m, exact - 1);
        assert_eq!(line, format!("{}{}", " ".repeat(8), "^".repeat(17)));
        assert_eq!(
            below,
            Some((8, m.label.as_str())),
            "under the start of the markers"
        );
        // Very narrow, or markers far to the right: the label moves left to keep room, but is never shortened.
        let far = Marker {
            pad: 60,
            ..m.clone()
        };
        let (_, below) = Explanation::marker_lines(&far, 40);
        let (indent, label) = below.unwrap();
        assert!(indent < 60, "{indent}");
        assert_eq!(label, m.label);
        assert_eq!(
            Explanation::marker_lines(&m, 0).1,
            Some((0, m.label.as_str()))
        );
    }

    #[test]
    fn a_failure_nothing_explains_still_says_what_to_do_next() {
        let e = Explanation::for_status(
            &row("bad", "npx -y pkg"),
            Status::Failed,
            Some("CONNECTION_CLOSED: Connection closed"),
            None,
        )
        .unwrap();
        let text = e.text();
        assert!(
            text.starts_with("error: Claude Code could not connect"),
            "{text}"
        );
        assert!(text.contains("   | npx -y pkg\n"), "{text}");
        assert!(!text.contains('^'), "nothing to point at: {text}");
        assert!(text.contains("= help: Diagnose"), "{text}");
        assert!(
            text.contains("= note: Claude Code said: CONNECTION_CLOSED"),
            "{text}"
        );
    }

    #[test]
    fn a_warning_is_not_called_an_error_and_a_healthy_server_has_nothing() {
        let r = row("x", "https://h.example/mcp");
        let degraded = Explanation::for_status(&r, Status::Degraded, None, None).unwrap();
        assert!(
            degraded.text().starts_with("warning: "),
            "{}",
            degraded.text()
        );
        for fine in [Status::Connected, Status::Checking, Status::Unknown] {
            assert_eq!(
                Explanation::for_status(&r, fine, Some("x"), None),
                None,
                "{fine:?}"
            );
        }
    }
}

//! The server list: rows grouped by where they live, each row's name, badges and status, and the words they use.

use super::{ShellApp, folder_name};
use crate::explain::{self, Explanation};
use crate::theme;
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use klefki_core::api::words::parent_folder;
use klefki_core::api::{
    Approval, Change, Command, Here, Hint, Preflight, PreflightKind, Scope, ServerRow, Snapshot,
    Status, Timing, words,
};
use std::sync::Arc;

pub(super) enum GridRow {
    Group(Group),
    Server(usize),
}

/// A group header: the scope and, for a project, its folder name up front and the rest of the path muted.
pub(super) struct Group {
    pub(super) title: String,
    /// The folder the project is in, shown quietly after the title.
    pub(super) location: Option<String>,
    /// The whole label, as one line: what assistive technology reads and what the tooltip shows.
    pub(super) full: String,
    count: usize,
}

/// A badge beside a server's name: what it says, its colours, and its hover text if any.
struct Badge {
    text: String,
    fill: Color32,
    color: Color32,
    hover: Option<String>,
}

const LAST_KNOWN: &str = "What Claude Code last saw, from its own cache, which can be old.";

/// A plugin's server's status, in the list and the details: where to see it.
pub(super) const PLUGIN_STATUS: &str = "See /mcp";
/// Why Klefki has no status for a plugin's server, and where it is.
const PLUGIN_NOT_CHECKED: &str =
    "Klefki does not check a plugin's servers. In Claude Code, /mcp shows their status.";
/// Why Klefki has no status for the organisation's server, and where it is.
const CONNECTOR_NOT_CHECKED: &str =
    "Klefki does not check claude.ai connectors. In Claude Code, /mcp shows their status.";
const ORGANISATION_NOT_CHECKED: &str = "Klefki does not check the servers your organisation sets. In Claude Code, /mcp shows their status.";

/// The status of a server Claude Code loads nowhere (decision 009).
pub(super) const NOT_LOADED: &str = "Not loaded";
/// The status of a server Claude Code's policy keeps out wherever it would load (decision 010).
pub(super) const BLOCKED_BY_POLICY: &str = "Blocked by policy";
/// The status of a claude.ai connector the user's own settings turn off wherever it would load (decision 011).
pub(super) const TURNED_OFF: &str = "Connectors off";

/// Seconds the pointer rests on a failed status before it is explained.
const EXPLAIN_AFTER: f64 = 0.1;

/// What the details say under a claude.ai connector (decision 011).
pub(super) const CONNECTOR_READ_ONLY: &str =
    "A connector of your claude.ai account. Change it on claude.ai; Klefki switches it on and off.";
/// More on claude.ai connectors, on request.
pub(super) const ABOUT_CONNECTORS: &str = "Claude Code fetches your connectors from claude.ai each session. Klefki lists the \
     ones Claude Code has connected before, so one you have since removed on claude.ai may still be here, and one you \
     have never used is not. A server you add yourself at the same address takes the connector's place; Klefki cannot \
     see a connector's address, so it cannot say when that happens.";

/// At most this many projects are named where a server is shadowed; the rest are counted.
const SHADOWED_NAMED: usize = 5;

/// At most this many whole paths are shown on hover; a tooltip taller than the window helps no one.
const SHADOWED_PATHS: usize = 20;

pub(super) fn group_of(s: &ServerRow) -> Group {
    if let Some(p) = &s.plugin {
        let title = format!("{} · {}", s.scope.label(), p.plugin);
        return Group {
            full: format!("{title}, from {}", p.marketplace),
            title,
            location: Some(p.marketplace.clone()),
            count: 0,
        };
    }
    match (&s.scope, &s.project) {
        (scope, Some(p)) => {
            let (parent, folder) = (parent_folder(p), folder_name(p));
            Group {
                title: format!("{} · {folder}", scope.label()),
                location: (!parent.is_empty()).then(|| parent.to_owned()),
                full: format!("{} · {p}", scope.label()),
                count: 0,
            }
        }
        (scope, None) => Group {
            title: scope.label().to_owned(),
            location: None,
            full: scope.label().to_owned(),
            count: 0,
        },
    }
}

/// The folder name stands out; where it lives is muted and cut to fit, with the whole path on hover. It is drawn,
/// not laid out, so a long path can run across the row without widening the column, and it reads as one label.
fn group_header(ui: &mut Ui, group: &Group, right: f32) {
    let pal = theme::palette(ui.ctx());
    let width = right;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &group.full));
    // Its own layer above the table, so the row's other cells (and their hover highlight) cannot paint over the
    // text; clipped to the table's width and to the scrolled area.
    let clip = ui.clip_rect();
    let painter = ui
        .ctx()
        .layer_painter(egui::LayerId::new(
            ui.layer_id().order,
            egui::Id::new("group-headers"),
        ))
        .with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(rect.min.x, clip.min.y.max(rect.min.y)),
            egui::pos2(width, clip.max.y.min(rect.max.y)),
        ));
    let font = egui::FontId::new(15.0, theme::bold_family());
    let title = painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        &group.title,
        font,
        pal.primary,
    );
    let mut x = title.max.x + 10.0;
    let count = format!("{}", group.count);
    let small = egui::FontId::proportional(13.0);
    let c = painter.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        count,
        small.clone(),
        pal.text_muted,
    );
    x = c.max.x + 12.0;
    if let Some(location) = &group.location {
        let room = (width - x - 24.0).max(0.0);
        let galley = ui.fonts_mut(|f| {
            let mut job =
                egui::text::LayoutJob::simple_singleline(location.clone(), small, pal.text_muted);
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            f.layout_job(job)
        });
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text_muted,
        );
    }
    response.on_hover_text(&group.full);
}

fn name_cell(
    ui: &mut Ui,
    s: &ServerRow,
    status: Status,
    project_count: usize,
    here: Option<&Here>,
) {
    let pal = theme::palette(ui.ctx());
    // Like a user server off in every project: a plugin's server whose plugin is. Seen from one project: off or
    // not loaded there.
    let loads = match here {
        Some(h) => !h.off && h.held.is_none(),
        None => s.enabled && !plugin_off_everywhere(s, project_count),
    };
    let color = if loads { pal.text } else { pal.disabled };
    ui.add(egui::Label::new(RichText::new(short_name(s)).color(color)).truncate());
    let loud = |text: &str, fill: Color32, hover: Option<&str>| Badge {
        text: text.to_owned(),
        fill,
        color: pal.on_bright,
        hover: hover.map(str::to_owned),
    };
    // Most important first: those that don't fit beside the name are the ones left to the "+N".
    // "Not loaded" is the status column's to say: once per row.
    let mut badges = Vec::new();
    if s.transport == klefki_core::api::TransportKind::Invalid {
        badges.push(loud("invalid", pal.danger, None));
    }
    if s.approval == Some(Approval::Rejected) {
        badges.push(loud("rejected", pal.danger, None));
    }
    let preflight = preflight_shown(s, status);
    // An empty variable is often meant (one cleared on purpose), so it is in the details only.
    let first = preflight.iter().find_map(|f| preflight_label(f.kind));
    if let (Some(label), None) = (first, &s.not_loaded) {
        badges.push(loud(label, pal.warn, Some(&preflight_hover(&preflight))));
    }
    let same_as = s.plugin.as_ref().and_then(|p| p.same_as.as_deref());
    if let (true, Some(same)) = (s.shadowed, same_as) {
        badges.push(loud(
            "left out",
            pal.warn,
            Some(&format!(
                "Claude Code leaves this server out: {same} has the same command or address"
            )),
        ));
    } else if s.shadowed {
        badges.push(loud(
            "shadowed",
            pal.warn,
            Some(if s.scope == Scope::User {
                "Every project has a local or project server of the same name, which Claude Code uses instead"
            } else {
                "This project has a local server of the same name, which Claude Code uses instead"
            }),
        ));
    }
    if s.casing_collision {
        badges.push(loud("casing", pal.warn, None));
    }
    if s.scope == Scope::User && !s.relative_paths.is_empty() && s.not_loaded.is_none() {
        badges.push(loud(
            "relative path",
            pal.warn,
            Some(
                "Claude Code starts user servers in each session's folder, so sessions in other folders can't \
                 find a file its command names. The details say which.",
            ),
        ));
    }
    // Seen from one project, the status says how it stands there; how it stands across projects is for the list
    // of every project.
    if here.is_some() {
        if let Some(why) = &s.may_not_load {
            badges.push(Badge {
                text: "may not load".to_owned(),
                fill: pal.border,
                color: pal.text,
                hover: Some(why.clone()),
            });
        }
        show_badges(ui, &badges);
        return;
    }
    // Switched off in some projects and held back in others: one badge with where it does load, so the state is
    // in view rather than behind a "+".
    let held_back = s.blocked.as_ref().filter(|_| s.not_loaded.is_none());
    let switched_off = s.scope.is_everywhere() && s.enabled && s.disabled_in > 0;
    if let (true, Some(b)) = (switched_off, held_back) {
        let loads = project_count.saturating_sub(s.disabled_in + b.projects.len());
        let what = if b.policy {
            "Claude Code's policy keeps it from loading"
        } else {
            "settings turn connectors from claude.ai off"
        };
        badges.push(loud(
            &format!("loads in {loads}/{project_count}"),
            pal.warn,
            Some(&format!(
                "Switched off in {} of {project_count} projects. In {} more, {what}: {} The details name the \
                 projects.",
                s.disabled_in,
                b.projects.len(),
                b.why
            )),
        ));
    } else if switched_off {
        badges.push(loud(
            &format!("off in {}/{}", s.disabled_in, project_count),
            pal.warn,
            None,
        ));
    } else if off_everywhere_known(s, project_count) {
        // Quiet, like the plugin's own off switch: turned off on purpose, not a problem.
        badges.push(Badge {
            text: "on in new".to_owned(),
            fill: pal.border,
            color: pal.text,
            hover: Some(format!(
                "Switched off in {} Claude Code knows. A project it opens for the first time still loads it.",
                all_known(project_count)
            )),
        });
    }
    // A project's own settings can switch the plugin off there: quiet, as it is often deliberate.
    if let Some(off) = s.plugin.as_ref().map(|p| p.off_in.len()).filter(|n| *n > 0) {
        badges.push(Badge {
            text: format!("plugin off in {off}/{project_count}"),
            fill: pal.border,
            color: pal.text,
            hover: Some(format!(
                "Its plugin is off in {off} of {project_count} projects, by the project's own settings or yours, \
                 so Claude Code does not load it there. The details name the projects."
            )),
        });
    }
    // Policy in some projects only: often deliberate, so quiet; everywhere is the status's to say.
    if let (false, Some(b)) = (switched_off, held_back) {
        let (text, what) = if b.policy {
            (
                "blocked",
                "Claude Code's policy keeps this server from loading",
            )
        } else {
            (
                "connectors off",
                "settings turn connectors from claude.ai off",
            )
        };
        badges.push(Badge {
            text: format!("{text} in {}/{}", b.projects.len(), project_count),
            fill: pal.border,
            color: pal.text,
            hover: Some(format!(
                "In {} of {project_count} projects, {what}. {}",
                b.projects.len(),
                b.why
            )),
        });
    }
    // What Klefki cannot check: a note, not a warning.
    if let Some(why) = &s.may_not_load {
        badges.push(Badge {
            text: "may not load".to_owned(),
            fill: pal.border,
            color: pal.text,
            hover: Some(why.clone()),
        });
    }
    if let (false, false, Some(same)) = (s.shadowed, s.shadowed_in.is_empty(), same_as) {
        badges.push(Badge {
            text: format!("left out in {}/{}", s.shadowed_in.len(), project_count),
            fill: pal.border,
            color: pal.text,
            hover: Some(format!(
                "In {} of {project_count} projects, Claude Code leaves this server out: {same} has the same \
                 command or address there. The details name the projects.",
                s.shadowed_in.len()
            )),
        });
    } else if !s.shadowed && !s.shadowed_in.is_empty() {
        // Often a deliberate per-project override, so quiet; hidden everywhere is the one worth acting on.
        badges.push(Badge {
            text: format!("shadowed in {}/{}", s.shadowed_in.len(), project_count),
            fill: pal.border,
            color: pal.text,
            hover: Some(format!(
                "In {} of {project_count} projects, a local or project server of the same name is used \
                 instead. The details name them.",
                s.shadowed_in.len()
            )),
        });
    }
    show_badges(ui, &badges);
}

/// The badges beside a name, most important first, as many as fit and a "+N" for the rest.
fn show_badges(ui: &mut Ui, badges: &[Badge]) {
    let pal = theme::palette(ui.ctx());
    // Badges never widen the column into the next one's text.
    let gap = ui.spacing().item_spacing.x;
    for (i, b) in badges.iter().enumerate() {
        let rest = badges.len() - i - 1;
        let reserve = if rest > 0 {
            gap + badge_width(ui, &format!("+{rest}"))
        } else {
            0.0
        };
        if gap + badge_width(ui, &b.text) + reserve > ui.available_width() {
            let more = &badges[i..];
            if gap + badge_width(ui, &format!("+{}", more.len())) <= ui.available_width() {
                let names: Vec<&str> = more.iter().map(|b| b.text.as_str()).collect();
                badge(ui, &format!("+{}", more.len()), pal.border, pal.text)
                    .on_hover_text(names.join("\n"));
            }
            return;
        }
        let response = badge(ui, &b.text, b.fill, b.color);
        if let Some(hover) = &b.hover {
            response.on_hover_text(hover);
        }
    }
}

/// `text` in one line of `width`: as it is when it fits, or else its `Saved to ` (when it has one), `…` and as much of
/// its end as fits.
pub(super) fn keep_the_end(ui: &Ui, text: &str, width: f32) -> String {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let fits = |t: &str| {
        ui.fonts_mut(|f| f.layout_no_wrap(t.to_owned(), font.clone(), Color32::WHITE))
            .size()
            .x
            <= width
    };
    if fits(text) {
        return text.to_owned();
    }
    let head = if text.starts_with("Saved to ") {
        "Saved to "
    } else {
        ""
    };
    let tail = &text[head.len()..];
    // The shortest cut that fits, found by halving: layout is the cost, not the length.
    let starts: Vec<usize> = tail.char_indices().map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(&format!("{head}…{}", &tail[starts[mid]..])) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    match starts.get(lo) {
        Some(&at) => format!("{head}…{}", &tail[at..]),
        None => format!("{head}…"),
    }
}

/// How wide [`badge`] draws `text`.
fn badge_width(ui: &Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, Color32::WHITE));
    galley.size().x + 12.0
}

/// The status of a server seen from a project where its switch is off.
pub(super) const OFF_HERE: &str = "Off here";

/// A server's status cell. Seen from one project (`here`), a server off there, or held back there whatever its
/// switch says, is said as that.
fn status_cell(
    ui: &mut Ui,
    s: &ServerRow,
    (status, detail): (Status, Option<&str>),
    hint: Option<&Hint>,
    timing: Option<&Timing>,
    last_known: bool,
    here: Option<&Here>,
) {
    let pal = theme::palette(ui.ctx());
    if let Some(h) = here {
        if h.off {
            ui.label(RichText::new(OFF_HERE).color(pal.disabled))
                .on_hover_text(
                    "Switched off in this project. Other projects keep their own switch.",
                );
            return;
        }
        if let Some(why) = &h.held {
            ui.label(RichText::new(NOT_LOADED).color(pal.text_muted))
                .on_hover_text(format!("Claude Code does not load this server here: {why}"));
            return;
        }
    }
    // Before the switch: on or off, Claude Code runs another server in its place, or policy keeps it out. Policy
    // says so: Claude Code itself drops such a server without a word.
    if let Some(why) = not_checked(s).filter(|_| s.not_loaded.is_some()) {
        let text = match &s.blocked {
            Some(b) if b.policy => BLOCKED_BY_POLICY,
            Some(_) => TURNED_OFF,
            None => NOT_LOADED,
        };
        ui.label(RichText::new(text).color(pal.text_muted))
            .on_hover_text(why);
        return;
    }
    // Off in every project; seen from one, its switch there is what counts, said above.
    if !s.enabled && here.is_none() {
        ui.label(RichText::new("Disabled").color(pal.disabled));
        return;
    }
    if s.approval == Some(Approval::Rejected) {
        ui.label(
            RichText::new(format!("\u{25CF} {}", Approval::Rejected.label())).color(pal.danger),
        );
        return;
    }
    // Never checked here: where its status is, not a check still to come.
    if let Some(why) = not_checked(s) {
        ui.label(RichText::new(PLUGIN_STATUS).color(pal.text_muted))
            .on_hover_text(why);
        return;
    }
    let slow = explain::slow_start(status, timing);
    let color = match status {
        // It connected only with more time than a session gives it: in Claude Code it still fails.
        Status::Connected if slow => pal.warn,
        Status::Connected => pal.success,
        Status::Degraded | Status::NeedsAuth | Status::PendingApproval => pal.warn,
        Status::Failed => pal.danger,
        Status::Checking => pal.primary,
        Status::NotConfigured | Status::Unknown => pal.text_muted,
    };
    let label = if slow {
        explain::SLOW_START
    } else {
        status.label()
    };
    let response = if last_known {
        // A status from Claude's cache: a ring in the status colour and muted words, not a dot. The column has no
        // room for words that say so; the tooltip and the Status tab do. Both colours keep the palette's contrast.
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut job = egui::text::LayoutJob::default();
        job.append(
            "\u{25CB} ",
            0.0,
            egui::TextFormat::simple(font.clone(), color),
        );
        job.append(label, 0.0, egui::TextFormat::simple(font, pal.text_muted));
        ui.label(job)
    } else {
        ui.label(RichText::new(format!("\u{25CF} {label}")).color(color))
    };
    let explained = slow
        || timing.is_some_and(|t| t.timed_out)
        || matches!(
            status,
            Status::Failed | Status::NeedsAuth | Status::Degraded
        );
    if explained {
        // Not egui's tooltip, which waits half a second for a perfectly still pointer over the words alone:
        // the whole cell answers, moving or not, after a moment that lets a sweep down the column pass by.
        if hovered_for(ui, response.id, ui.max_rect(), EXPLAIN_AFTER) {
            // Built only while shown, not every frame.
            response.show_tooltip_ui(|ui| {
                if let Some(explanation) =
                    Explanation::for_status_timed(s, status, detail, hint, timing)
                {
                    ui.set_max_width(560.0);
                    explanation.show(ui, ui.id().with("tooltip"));
                }
                if last_known {
                    ui.label(RichText::new(LAST_KNOWN).color(pal.text_muted));
                }
            });
        }
    } else if let Some(text) = detail.or(last_known.then_some(LAST_KNOWN)) {
        response.on_hover_text(text);
    }
}

/// Why Klefki never checks `s`, if it doesn't: Claude Code does not load it, or it is not one Klefki passes to Claude
/// Code (decisions 008 and 009).
pub(super) fn not_checked(s: &ServerRow) -> Option<String> {
    if let Some(why) = &s.not_loaded {
        return Some(format!("Claude Code does not load this server. {why}"));
    }
    match s.scope {
        Scope::Plugin => Some(PLUGIN_NOT_CHECKED.to_owned()),
        Scope::Managed => Some(ORGANISATION_NOT_CHECKED.to_owned()),
        Scope::ClaudeAi => Some(CONNECTOR_NOT_CHECKED.to_owned()),
        _ => None,
    }
}

/// The servers of `org`'s name, configured by hand, that Claude Code does not load because the organisation sets it.
pub(super) fn replaced_by_organisation(servers: &[ServerRow], org: &ServerRow) -> Vec<String> {
    servers
        .iter()
        .filter(|o| {
            matches!(o.scope, Scope::User | Scope::Local | Scope::Project)
                && o.name == org.name
                && o.not_loaded.is_some()
        })
        .map(|o| {
            let scope = o.scope.label().to_lowercase();
            match &o.project {
                None => format!("your {scope} server {}", o.name),
                Some(p) => format!("the {scope} server {} in {p}", o.name),
            }
        })
        .collect()
}

/// What the details say under a server the organisation's managed settings set.
pub(super) fn managed_read_only(file: &str) -> String {
    format!(
        "Your organisation sets this server, in {file}. Klefki switches it on and off; only an administrator can \
         change it."
    )
}

/// Whether the pointer has been over `rect` for `delay` seconds, moving or not, with no button down. Asks for the
/// frame in which that comes true, so it does not wait for the next mouse event.
fn hovered_for(ui: &Ui, id: egui::Id, rect: egui::Rect, delay: f64) -> bool {
    let ctx = ui.ctx();
    let key = id.with("hovered-since");
    let busy = ctx.input(|i| i.pointer.any_down() || i.is_scrolling());
    if busy || !ui.rect_contains_pointer(rect) {
        ctx.data_mut(|d| d.remove::<f64>(key));
        return false;
    }
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or(key, now));
    let left = delay - (now - since);
    if left > 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a repaint delay in seconds needs no more than f32"
        )]
        let left = left as f32;
        ctx.request_repaint_after_secs(left);
        return false;
    }
    true
}

fn badge(ui: &mut Ui, text: &str, fill: Color32, color: Color32) -> egui::Response {
    Frame::new()
        .fill(fill)
        .corner_radius(4)
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        })
        .response
}

/// "Use full paths: sessions in other folders can't find `a` or `b`.", the paths in the code font, for the detail
/// panel of a user server whose command names `paths` relative to where it starts.
pub(super) fn relative_job(ui: &Ui, paths: &[String], color: Color32) -> egui::text::LayoutJob {
    let text = |style: egui::TextStyle| egui::TextFormat {
        font_id: style.resolve(ui.style()),
        color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "Use full paths: sessions in other folders can't find ",
        0.0,
        text(egui::TextStyle::Body),
    );
    for (i, path) in paths.iter().enumerate() {
        if i > 0 {
            let between = if i + 1 == paths.len() { " or " } else { ", " };
            job.append(between, 0.0, text(egui::TextStyle::Body));
        }
        job.append(path, 0.0, text(egui::TextStyle::Monospace));
    }
    job.append(".", 0.0, text(egui::TextStyle::Body));
    job
}

/// A pre-flight finding on a badge, in a word or two; `None` for one the list leaves to the details.
fn preflight_label(kind: PreflightKind) -> Option<&'static str> {
    match kind {
        PreflightKind::NotFound => Some("no program"),
        PreflightKind::MangledSwitch => Some("Git Bash /c"),
        PreflightKind::NoInput => Some("missing -i"),
        PreflightKind::UnsetVariable => Some("unset variable"),
        PreflightKind::EmptyValue => None,
    }
}

/// The pre-flight findings to show while the server is `status`. Once it started, Claude Code found its program,
/// whatever PATH Klefki has: saying it can't would contradict the status beside it.
pub(super) fn preflight_shown(s: &ServerRow, status: Status) -> Vec<&Preflight> {
    let started = matches!(
        status,
        Status::Connected | Status::Degraded | Status::NeedsAuth
    );
    s.preflight
        .iter()
        .filter(|f| !(started && f.kind == PreflightKind::NotFound))
        .collect()
}

/// What a pre-flight badge says on hover: every finding, as the problem alone.
fn preflight_hover(found: &[&Preflight]) -> String {
    let problems: Vec<&str> = found.iter().map(|f| f.problem.as_str()).collect();
    format!(
        "This may keep the server from starting. {} The details say what to do.",
        problems.join(" ")
    )
}

/// The name the list shows: a plugin's server without the `plugin:<plugin>:` its group header already says, a
/// connector without `claude.ai `. The details show the whole name, which is the one Claude Code's `/mcp` shows.
fn short_name(s: &ServerRow) -> &str {
    // A claude.ai connector under the claude.ai group: the name it has there.
    if s.scope == Scope::ClaudeAi {
        return s
            .name
            .strip_prefix("claude.ai ")
            .filter(|rest| !rest.is_empty())
            .unwrap_or(&s.name);
    }
    s.plugin
        .as_ref()
        .and_then(|p| {
            s.name
                .strip_prefix("plugin:")?
                .strip_prefix(p.plugin.as_str())?
                .strip_prefix(':')
        })
        .filter(|rest| !rest.is_empty())
        .unwrap_or(&s.name)
}

pub(super) fn plugin_read_only(p: &klefki_core::api::PluginInfo) -> String {
    format!(
        "Claude Code reads this server from the plugin {}. Klefki switches it on and off; to change it, change the \
         plugin, or turn the plugin off in Claude Code (/plugin).",
        p.plugin
    )
}

/// A plugin's server whose plugin is off in every project Claude Code knows: it loads in none of them.
pub(super) fn plugin_off_everywhere(s: &ServerRow, project_count: usize) -> bool {
    project_count > 0
        && s.plugin
            .as_ref()
            .is_some_and(|p| p.off_in.len() == project_count)
}

/// Where a plugin's server's plugin is on, naming where it is off by folder, at most a few.
pub(super) fn plugin_on_text(p: &klefki_core::api::PluginInfo, project_count: usize) -> String {
    let off = p.off_in.len();
    let named: Vec<&str> = p.off_in[..off.min(SHADOWED_NAMED)]
        .iter()
        .map(|p| folder_name(p))
        .collect();
    format!(
        "in {} of {project_count} projects; off in {}",
        project_count.saturating_sub(off),
        words::and_more(&named, off)
    )
}

/// Where another server of the same name is used instead of `s`, for the detail panel.
pub(super) fn shadowed_text(s: &ServerRow, project_count: usize) -> String {
    if let Some(same) = s.plugin.as_ref().and_then(|p| p.same_as.as_deref()) {
        return if s.shadowed {
            format!("in every project: {same} has the same command or address")
        } else {
            let mut text = shadowed_in_text(s, project_count);
            text.push_str(&format!(" ({same} has the same command or address)"));
            text
        };
    }
    if s.shadowed_in.is_empty() {
        let text = if !s.shadowed {
            "no"
        } else if s.scope == Scope::User {
            "yes, in every project"
        } else {
            "yes, by a local server of the same name"
        };
        return text.to_owned();
    }
    shadowed_in_text(s, project_count)
}

/// The projects `s` is shadowed in, by folder name, at most a few.
fn shadowed_in_text(s: &ServerRow, project_count: usize) -> String {
    in_projects_text(&s.shadowed_in, project_count, s.shadowed)
}

/// "in 1 of 3 projects: p", or "in all 2 projects: p, q" when `all`: by folder name, at most a few.
pub(super) fn in_projects_text(projects: &[String], project_count: usize, all: bool) -> String {
    let count = projects.len();
    let mut text = if all {
        format!("in all {count} projects: ")
    } else {
        format!("in {count} of {project_count} projects: ")
    };
    // By folder name, as the list's group headers name them; the whole paths are on hover.
    let named: Vec<&str> = projects[..count.min(SHADOWED_NAMED)]
        .iter()
        .map(|p| folder_name(p))
        .collect();
    text.push_str(&words::and_more(&named, count));
    text
}

/// The whole paths of the projects where `s` is shadowed, one per line.
pub(super) fn shadowed_paths(s: &ServerRow) -> String {
    projects_paths(&s.shadowed_in)
}

/// The whole paths of `projects`, one per line, at most a screenful.
pub(super) fn projects_paths(projects: &[String]) -> String {
    let count = projects.len();
    let mut text = projects[..count.min(SHADOWED_PATHS)].join("\n");
    if count > SHADOWED_PATHS {
        text.push_str(&format!("\nand {} more", count - SHADOWED_PATHS));
    }
    text
}

impl ShellApp {
    fn grid_rows(&self) -> Vec<GridRow> {
        let mut rows = Vec::new();
        let mut last: Option<(Scope, Option<&str>)> = None;
        for i in self.visible() {
            let s = &self.snapshot.servers[i];
            // A plugin's servers are grouped by plugin, as the others are by project.
            let key = (
                s.scope,
                s.project
                    .as_deref()
                    .or(s.plugin.as_ref().map(|p| p.plugin.as_str())),
            );
            if last != Some(key) {
                rows.push(GridRow::Group(group_of(s)));
                last = Some(key);
            }
            if let Some(GridRow::Group(g)) = rows
                .iter_mut()
                .rev()
                .find(|r| matches!(r, GridRow::Group(_)))
            {
                g.count += 1;
            }
            rows.push(GridRow::Server(i));
        }
        rows
    }

    /// The grid's rows, rebuilt only when the snapshot or the search changed.
    pub(super) fn cached_grid_rows(&mut self) -> Arc<[GridRow]> {
        match &self.grid_cache {
            Some((search, rows)) if *search == self.search => Arc::clone(rows),
            _ => {
                let rows: Arc<[GridRow]> = self.grid_rows().into();
                self.grid_cache = Some((self.search.clone(), Arc::clone(&rows)));
                rows
            }
        }
    }

    pub(super) fn grid(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let rows = self.cached_grid_rows();
        let ctrl = ui.input(|i| i.modifiers.command);
        let project_count = self.snapshot.project_count;
        let mut clicked = None;
        // Group headers may run across the row's empty cells, but no further than the table itself.
        let right = ui.max_rect().right();
        // Only the rows in view are drawn, so the ones drawn are the ones on screen.
        let mut shown = std::mem::take(&mut self.on_screen_now);
        shown.clear();
        let reveal = self.reveal.take().and_then(|i| {
            rows.iter()
                .position(|r| matches!(r, GridRow::Server(s) if *s == i))
        });
        let mut table = TableBuilder::new(ui);
        if let Some(at) = reveal {
            table = table.scroll_to_row(at, Some(Align::Center));
        }
        table
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(220.0).at_least(120.0))
            .column(Column::exact(64.0))
            .column(Column::exact(176.0))
            .column(Column::remainder().clip(true))
            .header(32.0, |mut h| {
                for t in ["Name", "Type", "Status", "Target"] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).strong().color(pal.text_muted));
                    });
                }
            })
            .body(|body| {
                body.rows(36.0, rows.len(), |mut row| match &rows[row.index()] {
                    GridRow::Group(group) => {
                        row.col(|ui| group_header(ui, group, right));
                        for _ in 0..3 {
                            row.col(|_| {});
                        }
                    }
                    GridRow::Server(i) => {
                        shown.push(*i);
                        let s = &self.snapshot.servers[*i];
                        row.set_selected(self.selected.contains(i));
                        let (status, detail) = self.effective(*i);
                        let here = self.here(*i);
                        row.col(|ui| name_cell(ui, s, status, project_count, here));
                        row.col(|ui| {
                            ui.label(RichText::new(s.transport.label()).color(pal.text_muted));
                        });
                        let hint = self.hint(*i);
                        let timing = self.timing(*i);
                        let last_known = self.last_known(*i);
                        row.col(|ui| {
                            status_cell(ui, s, (status, detail), hint, timing, last_known, here);
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&s.target).monospace().color(pal.text_muted));
                        });
                        if row.response().clicked() {
                            clicked = Some(*i);
                        }
                    }
                });
            });
        if shown == self.on_screen {
            self.on_screen_now = shown;
        } else {
            let servers = &self.snapshot.servers;
            self.send(Command::Visible(
                shown
                    .iter()
                    .map(|&i| servers[i].connection.clone())
                    .collect(),
            ));
            self.on_screen_now = std::mem::replace(&mut self.on_screen, shown);
        }
        if let Some(i) = clicked {
            if ctrl {
                if !self.selected.remove(&i) {
                    self.selected.insert(i);
                }
            } else {
                self.selected.clear();
                self.selected.insert(i);
            }
        }
    }
}

/// A server Claude Code loads in every project, switched off in each one it knows: one it opens later still loads it.
/// Not one that loads nowhere anyway (replaced, held back, or its plugin off everywhere).
pub(super) fn off_everywhere_known(s: &ServerRow, project_count: usize) -> bool {
    s.scope.is_everywhere()
        && !s.enabled
        && project_count > 0
        && s.not_loaded.is_none()
        && !plugin_off_everywhere(s, project_count)
}

/// Every project Claude Code knows, in words: "all 3 projects", "the 1 project".
pub(super) fn all_known(project_count: usize) -> String {
    if project_count == 1 {
        "the 1 project".to_owned()
    } else {
        format!("all {project_count} projects")
    }
}

impl ShellApp {
    /// Where the list is seen from: every project, or one, where it shows what Claude Code loads and switches act
    /// there only. Not shown while Claude Code knows no project.
    pub(super) fn where_picker(&mut self, ui: &mut Ui) {
        if self.snapshot.projects.is_empty() {
            return;
        }
        let pal = theme::palette(ui.ctx());
        let mut picked: Option<Option<String>> = None;
        let mut asked: Option<Change> = None;
        let shown = match &self.lens {
            None => ALL_PROJECTS.to_owned(),
            Some(p) => crate::profiles::shown_project(p, &self.snapshot),
        };
        // The profile menu beside the Where list where both fit, else on the next line, never out of sight.
        let menu = self.lens.is_some() && !self.snapshot.profiles.is_empty();
        let mut menu_below = false;
        ui.horizontal(|ui| {
            let label = ui.label(RichText::new("Where").color(pal.text_muted));
            let room = ui.available_width() - 8.0;
            let beside = menu && room - PROFILE_MENU_WIDTH >= 120.0;
            menu_below = menu && !beside;
            let lens = self.lens.as_deref();
            let filter = &mut self.lens_filter;
            let projects: Vec<&String> = self.snapshot.projects.iter().collect();
            let combo = egui::ComboBox::from_id_salt("where")
                .selected_text(shown)
                .width(
                    (room - if beside { PROFILE_MENU_WIDTH } else { 0.0 }).clamp(120.0, 240.0),
                )
                .height(320.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(lens.is_none(), ALL_PROJECTS)
                        .on_hover_text("Every server, and how it stands across the projects Claude Code knows.")
                        .clicked()
                    {
                        picked = Some(None);
                    }
                    ui.separator();
                    if let Some(p) = crate::profiles::project_choices(ui, &projects, lens, filter) {
                        picked = Some(Some(p));
                    }
                });
            let combo = combo.response.labelled_by(label.id);
            match lens {
                Some(p) => {
                    combo.on_hover_text(p);
                    if beside
                        && let Some(change) = profile_menu(ui, &self.snapshot, p, &self.lens_profiles)
                    {
                        asked = Some(change);
                    }
                    // Beside it where there is room; the hover says the rest.
                    if ui.available_width() > 200.0 {
                        ui.add(
                            egui::Label::new(
                                RichText::new("Switches act in this project only")
                                    .color(pal.text_muted),
                            )
                            .truncate(),
                        );
                    }
                }
                None => {
                    combo.on_hover_text("Pick a project to see what Claude Code loads there and switch it there only.");
                }
            }
        });
        if menu_below && let Some(p) = &self.lens {
            ui.horizontal(|ui| {
                asked = profile_menu(ui, &self.snapshot, p, &self.lens_profiles).or(asked.take());
            });
        }
        ui.add_space(4.0);
        if let Some(project) = picked {
            self.lens_filter.clear();
            self.look_in(project);
        }
        if let Some(change) = asked {
            self.prepare(vec![change]);
        }
    }
}

/// What the profile menu takes beside the Where list, about: its label, a profile's name, the caret and the gaps.
const PROFILE_MENU_WIDTH: f32 = 180.0;

/// The profile in use in `project` (`in_use`, in the profiles' order), as a menu that switches to another profile
/// there, uses one, or stops one: each a change reviewed before it is saved (it can turn many servers off), with
/// Undo once saved. Not drawn when there are no profiles.
fn profile_menu(
    ui: &mut Ui,
    snapshot: &Snapshot,
    project: &str,
    in_use: &[String],
) -> Option<Change> {
    if snapshot.profiles.is_empty() {
        return None;
    }
    let shown = crate::profiles::shown_project(project, snapshot);
    let current = match in_use {
        [] => "No profile".to_owned(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    };
    let mut asked = None;
    // Named as what it is in both layouts, with a caret like the Where list's: it opens a list to choose from.
    let label = ui.label(RichText::new("Profile").color(theme::palette(ui.ctx()).text_muted));
    let menu = ui.menu_button(format!("{current} {}", icon::CARET_DOWN), |ui| {
        // The profiles in use first, ticked: what is chosen now, not something to do.
        for name in in_use {
            ui.label(format!("{} {name}", icon::CHECK));
        }
        if !in_use.is_empty() {
            ui.separator();
        }
        for profile in snapshot.profiles.iter().filter(|p| !in_use.contains(&p.name)) {
            let (text, tip) = if in_use.is_empty() {
                (
                    format!("Use {}", profile.name),
                    format!("Only the servers of {} on in {shown}, after a review.", profile.name),
                )
            } else {
                (
                    format!("Switch to {}", profile.name),
                    format!(
                        "Stop using {} in {shown} and have only the servers of {} on there, as one change, after a \
                         review.",
                        words::and_list(in_use),
                        profile.name
                    ),
                )
            };
            if ui.button(text).on_hover_text(tip).clicked() {
                asked = Some(Change::SwitchProfile {
                    from: in_use.to_vec(),
                    name: profile.name.clone(),
                    members: profile.members.clone(),
                    project: project.to_owned(),
                    // As the Profiles page has it at first (the owner's choice, 2026-10-20).
                    only: true,
                });
                ui.close();
            }
        }
        if !in_use.is_empty() {
            ui.separator();
            for name in in_use {
                if ui
                    .button(format!("Stop using {name}"))
                    .on_hover_text(format!(
                        "Put back what {name} switched in {shown}, unless it was switched again since, after a review."
                    ))
                    .clicked()
                {
                    asked = Some(Change::StopProfile {
                        name: name.clone(),
                        project: Some(project.to_owned()),
                        delete: false,
                    });
                    ui.close();
                }
            }
        }
    });
    let response = menu.response.labelled_by(label.id);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("Profile in {shown}: {current}"),
        )
    });
    response.on_hover_text(match in_use {
        [] => format!("No profile is in use in {shown}. Use one here."),
        _ => format!(
            "In use in {shown}: {}. Switch to another profile here, or stop it.",
            words::and_list(in_use)
        ),
    });
    asked
}

/// The Where list's choice of every project.
pub(super) const ALL_PROJECTS: &str = "All projects";

//! The command palette (Ctrl+K, ⌘K on macOS): every action Klefki offers and every server, found by typing a few
//! letters, run with Enter. Keyboard-first, like Raycast; the mouse works too.
use crate::theme;
use egui::{Key, RichText, Sense};

/// What choosing an entry does. The shell carries it out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Select this row of the snapshot.
    Select(usize),
    TurnOn,
    TurnOff,
    Delete,
    Edit,
    Check,
    Diagnose,
    Approve,
    Refresh,
    Search,
    Theme(crate::theme::ThemeChoice),
    Tone(crate::theme::Tone),
    Settings,
    Duplicate,
    /// Move the selected server to another project or the user.
    Move,
    Add,
    FixCasing,
    /// Move every old switch-off list (`mcpServersDisabled`) into the one Claude Code reads.
    MigrateLegacy,
    /// Sign in to the selected server, or sign in again.
    SignIn,
    /// Export the selected servers, or every server in the list.
    Export,
}

pub struct Entry {
    pub icon: &'static str,
    pub title: String,
    /// Shown muted after the title; also searched.
    pub subtitle: String,
    pub shortcut: Option<String>,
    pub action: Action,
}

#[derive(Default)]
pub struct CommandPalette {
    open: bool,
    query: String,
    /// Index into the filtered list.
    cursor: usize,
    /// Give the text field the keyboard on the first frame.
    focus: bool,
    /// The entries, built once when the palette opens and again only after [`invalidate`](Self::invalidate),
    /// rather than every frame: with thousands of servers that is thousands of strings per frame.
    entries: Option<Vec<Entry>>,
    /// The query `matches` was filtered for; typing re-filters, other frames reuse it.
    matched: Option<String>,
    matches: Vec<usize>,
}

/// How many entries are shown at once; typing narrows the rest down.
const SHOWN: usize = 9;

impl CommandPalette {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.cursor = 0;
        self.focus = true;
        self.invalidate();
    }

    /// The entries no longer match what they were built from (a new snapshot): build them again when next shown.
    pub fn invalidate(&mut self) {
        self.entries = None;
        self.matched = None;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub fn query_mut(&mut self) -> &mut String {
        &mut self.query
    }

    /// Draws the palette over everything and returns the action chosen this frame, if any. `entries` is called
    /// only when the palette has none built yet.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        entries: impl FnOnce() -> Vec<Entry>,
    ) -> Option<Action> {
        if !self.open {
            return None;
        }
        let entries = std::mem::take(&mut self.entries).unwrap_or_else(entries);
        let chosen = self.show_entries(ctx, &entries);
        self.entries = Some(entries);
        chosen
    }

    fn show_entries(&mut self, ctx: &egui::Context, entries: &[Entry]) -> Option<Action> {
        let pal = theme::palette(ctx);
        if self.matched.as_deref() != Some(self.query.as_str()) {
            self.matches = filter(&self.query, entries);
            self.matched = Some(self.query.clone());
        }
        let matches = std::mem::take(&mut self.matches);
        self.cursor = self.cursor.min(matches.len().saturating_sub(1));
        // Arrows move the highlight and are taken before the text field sees them.
        let (down, up, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
            )
        });
        if down && self.cursor + 1 < matches.len() {
            self.cursor += 1;
        }
        if up {
            self.cursor = self.cursor.saturating_sub(1);
        }
        let mut chosen = enter
            .then(|| matches.get(self.cursor))
            .flatten()
            .map(|&i| entries[i].action.clone());

        let modal = egui::Modal::new(egui::Id::new("command_palette")).show(ctx, |ui| {
            ui.set_width(560.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Type a command or a server name…")
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Heading),
            );
            if std::mem::take(&mut self.focus) {
                field.request_focus();
            }
            if field.changed() {
                self.cursor = 0;
            }
            ui.add_space(6.0);
            if matches.is_empty() {
                ui.label(RichText::new("Nothing matches.").color(pal.text_muted));
            }
            // Keep the highlighted entry in view.
            let first = self.cursor.saturating_sub(SHOWN - 1);
            for (row, &i) in matches.iter().enumerate().skip(first).take(SHOWN) {
                let entry = &entries[i];
                let active = row == self.cursor;
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), Sense::click());
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, &entry.title)
                });
                if active || response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        6.0,
                        if active { pal.selection } else { pal.hover },
                    );
                }
                // Drawn rather than laid out as widgets, so the row is one entry for assistive technology too.
                let painter = ui.painter_at(rect.shrink2(egui::vec2(10.0, 0.0)));
                let y = rect.center().y;
                let mut x = rect.left() + 10.0;
                let icon_color = if active { pal.primary } else { pal.text_muted };
                let r = painter.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_CENTER,
                    entry.icon,
                    egui::FontId::proportional(15.0),
                    icon_color,
                );
                x = r.right() + 10.0;
                let r = painter.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_CENTER,
                    &entry.title,
                    egui::FontId::proportional(15.0),
                    pal.text,
                );
                x = r.right() + 10.0;
                let mut end = rect.right() - 10.0;
                if let Some(shortcut) = &entry.shortcut {
                    let r = painter.text(
                        egui::pos2(end, y),
                        egui::Align2::RIGHT_CENTER,
                        shortcut,
                        egui::FontId::proportional(12.0),
                        pal.text_muted,
                    );
                    end = r.left() - 12.0;
                }
                if !entry.subtitle.is_empty() && end > x {
                    let galley = ui.fonts_mut(|f| {
                        let mut job = egui::text::LayoutJob::simple_singleline(
                            entry.subtitle.clone(),
                            egui::FontId::proportional(13.0),
                            pal.text_muted,
                        );
                        job.wrap = egui::text::TextWrapping::truncate_at_width(end - x);
                        f.layout_job(job)
                    });
                    painter.galley(
                        egui::pos2(x, y - galley.size().y / 2.0),
                        galley,
                        pal.text_muted,
                    );
                }
                if response.clicked() {
                    chosen = Some(entry.action.clone());
                }
                if response.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    self.cursor = row;
                }
            }
            if matches.len() > SHOWN {
                ui.label(
                    RichText::new(format!(
                        "{} more — keep typing to narrow down",
                        matches.len() - SHOWN
                    ))
                    .small()
                    .color(pal.text_muted),
                );
            }
        });
        if modal.should_close() || chosen.is_some() {
            self.open = false;
        }
        // Typing this frame changed the query; the next frame filters again.
        self.matches = matches;
        chosen
    }
}

/// The entries matching `query`, best first; all of them, in their own order, for an empty query.
pub fn filter(query: &str, entries: &[Entry]) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let mut scored: Vec<(i32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let title = score(query, &e.title);
            let subtitle = score(query, &e.subtitle).map(|s| s - 20);
            title.max(subtitle).map(|s| (s, i))
        })
        .collect();
    // Best score first; equal scores keep the entries' own order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// How well `query` matches `text`: its letters in order, case ignored. Letters that start a word and letters
/// next to each other count more, so "off gh" finds "Turn off github" before "Go to github". `None`: no match.
pub fn score(query: &str, text: &str) -> Option<i32> {
    // One pass over `text`, lower-cased as it is read: no copy of it is made. `at` counts lower-cased characters.
    let mut text = text.chars().flat_map(char::to_lowercase);
    let mut at = 0;
    let mut before: Option<char> = None;
    let mut total = 0;
    let mut previous: Option<usize> = None;
    for q in query.chars().flat_map(char::to_lowercase) {
        if q == ' ' {
            continue;
        }
        let start = at;
        let found = loop {
            let c = text.next()?;
            let here = at;
            at += 1;
            let word_start = before.is_none_or(|b| !b.is_alphanumeric());
            before = Some(c);
            if c == q {
                break (here, word_start);
            }
        };
        let (found, word_start) = found;
        total += 1;
        if word_start {
            total += 8;
        }
        if previous.is_some_and(|p| p + 1 == found) {
            total += 5;
        }
        // Gaps cost a little, so tighter matches win.
        total -= (found - start).min(10) as i32;
        previous = Some(found);
    }
    Some(total)
}

/// How a shortcut is written on this platform: `⌘K` on macOS, `Ctrl+K` elsewhere.
pub fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, subtitle: &str) -> Entry {
        Entry {
            icon: "",
            title: title.to_owned(),
            subtitle: subtitle.to_owned(),
            shortcut: None,
            action: Action::Refresh,
        }
    }

    #[test]
    fn letters_in_order_match_and_others_do_not() {
        assert!(score("gh", "github").is_some());
        assert!(score("GH", "github").is_some());
        assert!(score("hg", "github").is_none());
        assert!(score("", "anything").is_some());
        assert!(score("x", "").is_none());
    }

    #[test]
    fn word_starts_and_runs_rank_first() {
        let entries = [
            entry("Go to github", "User"),
            entry("Turn off github", "1 selected"),
            entry("Refresh", "Re-check every server"),
        ];
        assert_eq!(filter("off gh", &entries)[0], 1);
        assert_eq!(filter("go gh", &entries)[0], 0);
        assert_eq!(filter("refr", &entries), [2]);
        assert_eq!(filter("github", &entries).len(), 2);
        // The subtitle is searched too, below title matches.
        assert_eq!(filter("re-check", &entries), [2]);
    }

    #[test]
    fn an_empty_query_lists_everything_in_order() {
        let entries = [entry("b", ""), entry("a", "")];
        assert_eq!(filter("  ", &entries), [0, 1]);
    }

    /// The scoring as first written, copying the text into a vector of lower-cased characters.
    fn score_reference(query: &str, text: &str) -> Option<i32> {
        let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
        let mut at = 0;
        let mut total = 0;
        let mut previous: Option<usize> = None;
        for q in query.chars().flat_map(char::to_lowercase) {
            if q == ' ' {
                continue;
            }
            let found = (at..text.len()).find(|&i| text[i] == q)?;
            let word_start = found == 0 || !text[found - 1].is_alphanumeric();
            total += 1;
            if word_start {
                total += 8;
            }
            if previous.is_some_and(|p| p + 1 == found) {
                total += 5;
            }
            total -= (found - at).min(10) as i32;
            previous = Some(found);
            at = found + 1;
        }
        Some(total)
    }

    #[test]
    fn scoring_without_copying_scores_the_same() {
        let texts = [
            "",
            "github",
            "Turn off github",
            "Go to my-server_2 (Local · ~/work/x)",
            "aaaa bbbb aaaa",
            "İstanbul ǅemal ẞtraße", // lower-case to more than one character, or to another length
            "a-b c.d/e",
            "x y z",
        ];
        let queries = [
            "",
            " ",
            "a",
            "gh",
            "off gh",
            "GO MY",
            "ab",
            "aa aa",
            "i̇s",
            "i",
            "ss",
            "ǆ",
            "xyz",
            "zz",
            "-",
            "a b c d e",
        ];
        for t in texts {
            for q in queries {
                assert_eq!(score(q, t), score_reference(q, t), "{q:?} in {t:?}");
            }
        }
    }

    #[test]
    fn shortcuts_follow_the_platform() {
        let s = shortcut("K");
        assert!(s == "⌘K" || s == "Ctrl+K");
    }
}

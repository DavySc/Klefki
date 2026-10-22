//! The desktop-style shell: left nav, top bar, icon toolbar, grid and detail panel.
//! Pure UI over an immutable `Snapshot`; no IO.
use crate::command::{self, CommandPalette, Entry, shortcut};
use crate::explain::Explanation;
use crate::sign_in::{self, SignInView};
use crate::theme::{self, ThemeChoice, Tone};
use egui::{Frame, Margin, RichText, Sense, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Approval, Change, Command, ConnectionId, Core, Diagnosis, Event, Hint, ImportFile, LastStarts,
    PlanView, Scope, Secret, ServerId, ServerName, ServerRow, Snapshot, Status, Target, Timing,
    TransportKind, UpdateDownload, same_project,
    words::{self, capitalized},
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

mod add;
mod casing;
mod debug_report;
mod detail;
mod duplicate;
mod editor;
mod export;
mod inspector;
mod last_starts;
mod list;
mod move_dialog;
mod notices;
mod outcome;
mod places;
mod review;
mod settings;
mod toolbar;
mod top_bar;

use add::{AddForm, AddMode, forget_typed};
use casing::CasingForm;
use detail::{bring_focus_into_view, scroll_gutter};
use duplicate::DuplicateForm;
use editor::{Editor, SecretView};
use export::ExportView;
use list::{GridRow, group_of};
use move_dialog::MoveForm;
use notices::LegacyNotice;
use outcome::{Outcome, OutcomeChoice};
use places::places_in_words;
use review::DiffTops;
use top_bar::{bar_frame, search_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Connections,
    Profiles,
    Desktop,
    ScopeDrift,
    Secrets,
    Settings,
    ActivityLog,
}

impl Page {
    const ALL: [(Page, &'static str, &'static str); 7] = [
        (Page::Connections, icon::PLUGS_CONNECTED, "Connections"),
        (Page::Profiles, icon::STACK, "Profiles"),
        (Page::Desktop, icon::DESKTOP, "Claude Desktop"),
        (Page::ScopeDrift, icon::GIT_DIFF, "Scope drift"),
        (Page::Secrets, icon::KEY, "Secrets"),
        (Page::Settings, icon::GEAR, "Settings"),
        (Page::ActivityLog, icon::LIST_BULLETS, "Activity log"),
    ];

    pub(crate) fn title(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(p, ..)| *p == self)
            .map_or("", |(.., t)| t)
    }

    fn glyph(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(p, ..)| *p == self)
            .map_or("", |(_, g, _)| g)
    }

    /// Pages that are not built yet say what they will do, so the menu is a map rather than a dead end.
    fn ready(self) -> bool {
        matches!(
            self,
            Page::Connections
                | Page::Profiles
                | Page::Desktop
                | Page::Secrets
                | Page::Settings
                | Page::ActivityLog
        )
    }

    fn promise(self) -> &'static str {
        match self {
            Page::Connections
            | Page::Profiles
            | Page::Desktop
            | Page::Secrets
            | Page::ActivityLog => "",
            Page::ScopeDrift => {
                "Coming soon: servers whose definitions differ between scopes, side by side."
            }
            Page::Settings => {
                "Coming soon: choosing where Claude Code is, and how often to check servers."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DetailTab {
    General,
    Auth,
    HeadersEnv,
    Status,
}

impl DetailTab {
    // No JSON tab: the definition is edited in labelled fields, and as JSON only from there, when they fall short.
    const ALL: [(DetailTab, &'static str); 4] = [
        // Short names keep the tabs on one line in a panel of a third of the window.
        (DetailTab::General, "General"),
        (DetailTab::Status, "Status"),
        (DetailTab::HeadersEnv, "Env"),
        (DetailTab::Auth, "Auth"),
    ];
}

/// The size Klefki opens at.
pub(crate) const OPENING_SIZE: egui::Vec2 = egui::vec2(1200.0, 720.0);
/// The smallest it can be made; also the room the opening size gives at 150 % zoom.
pub(crate) const SMALLEST_SIZE: egui::Vec2 = egui::vec2(800.0, 480.0);

pub(crate) struct ShellApp {
    snapshot: Arc<Snapshot>,
    /// The engine. `None` for demo data and tests that feed the UI directly.
    core: Option<Core>,
    /// Live results from the engine, which take precedence over the snapshot's hints. One slot per row of the
    /// snapshot, by position, so drawing a row looks its status up without building a key.
    live: Vec<Option<Live>>,
    /// Bumped whenever `live` or the snapshot changes, so what is built from them can be kept until then.
    live_gen: u64,
    /// The selected row's explanation, for (row, `live_gen`): built once per status, not every frame.
    explained: Option<(ExplainedFor, Option<Arc<Explanation>>)>,
    /// The grid's rows for the current snapshot and search, built when either changes rather than every frame.
    grid_cache: Option<(String, Arc<[GridRow]>)>,
    /// The notice about old switch-off lists, for the snapshot it was built from.
    legacy_cache: Option<(Arc<Snapshot>, Arc<LegacyNotice>)>,
    /// The servers drawn in the grid at the last change, as told to the engine (its background checks do these
    /// first), and a buffer for this frame's: compared each frame, sent only when they differ.
    on_screen: Vec<usize>,
    on_screen_now: Vec<usize>,
    /// The latest explanation per server, from the Diagnose action.
    diagnoses: HashMap<ServerId, Diagnosis>,
    /// What Claude Code's logs say about each server's last starts, asked for when its Status tab is shown.
    last_starts: HashMap<ServerId, LastStartsSeen>,
    /// The line that opens a server in the MCP Inspector, for the one server it was asked for.
    inspector: Option<inspector::InspectorSeen>,
    debug_report: Option<debug_report::DebugSeen>,
    /// A diagnosis that came in for the server shown on its Status tab, to bring into view once drawn.
    reveal_diagnosis: Option<ServerId>,
    /// The server checked with more time, and when that was asked for (egui's clock, in seconds): the Status tab
    /// counts the seconds while it runs.
    longer_since: Option<(ServerId, f64)>,
    /// The detail panel's tab drew a definition that can be saved: the save bar goes under it this frame.
    save_bar_wanted: bool,
    /// How tall the save bar was last frame (none: 0), so the scroll area above it leaves that much room.
    save_bar_height: f32,
    /// The save bar was drawn in this frame: the outcome message keeps clear of it.
    save_bar_shown: bool,
    /// The room between the navigation and the detail panel, as last laid out: the outcome message goes there,
    /// over the list, when it fits.
    list_room: Option<egui::Rect>,
    /// The outcome message was drawn in this frame as a strip above the save bar, where there is no room beside
    /// the detail panel, and what was chosen on it.
    outcome_inline: bool,
    outcome_chosen: Option<OutcomeChoice>,
    cli_version: Option<String>,
    cli_problem: Option<String>,
    /// A change the engine has prepared and the user has not yet confirmed. Shared, so drawing it needs no copy.
    plan: Option<Arc<PlanView>>,
    /// Where each line of the plan's diffs goes, so only the lines in view are laid out on each frame.
    diff_tops: DiffTops,
    /// Apply was pressed and the engine has not answered yet.
    applying: bool,
    /// A plan was asked for and the engine has not answered yet.
    preparing: bool,
    /// The definition being viewed or edited, in the form or as JSON.
    editor: Option<Editor>,
    /// The editor's draft was sent for review, and no plan or failure has come back yet.
    draft_asked: bool,
    /// A profile's Use was sent for review, by name, and no plan or failure has come back yet: a plan that would
    /// write nothing is said on its card, not in a review with nothing to save.
    profile_asked: Option<String>,
    /// The plan that saves the editor's draft. Any other change applied leaves a draft's edits as they are.
    draft_plan: Option<u64>,
    /// Ctrl+K / ⌘K.
    command: CommandPalette,
    /// The Duplicate dialog, while open.
    duplicate: Option<DuplicateForm>,
    /// The Add dialog, while open.
    add: Option<AddForm>,
    /// How the Add dialog was last used, pasting or filling in; it opens that way again.
    add_mode: AddMode,
    /// The servers the prepared change adds, by name and place: selected once they are in the list.
    adding: Vec<(String, Vec<Target>)>,
    /// What is being added signs in with OAuth: once added, the user is reminded to sign in.
    adding_signs_in: bool,
    /// What the success message says was done to them: "Added", "Moved" or "Copied".
    adding_verb: &'static str,
    /// The Move dialog, while open.
    moving: Option<MoveForm>,
    /// The server a move under review takes away, and whether it signs in: what Undo moves back.
    moved_from: Option<(ServerId, bool)>,
    /// The servers the change under review moves or renames, with the name and place each lands at: an open
    /// definition follows its server there, unsaved edits and all.
    relocating: Vec<(ServerId, String, Target)>,
    /// What a slow change (through Claude Code's commands) is doing now, while it is applied.
    applying_step: Option<String>,
    /// The kept definition a Forget… under review lets go of, in words.
    forgetting: Option<String>,
    /// egui still remembers text typed into the closed Add dialog (undo steps of secrets): forget it.
    forget_typed: bool,
    /// A server to bring into view in the list, once.
    reveal: Option<usize>,
    /// The Fix casing dialog, while open.
    casing: Option<CasingForm>,
    /// The sign-in dialog, while open.
    sign_in: Option<SignInView>,
    /// What is known about the shown OAuth server's client secret.
    secret: Option<SecretView>,
    /// A client secret being typed. Wiped from memory when dropped; handed to the engine, never copied.
    secret_input: Option<Secret>,
    /// The outcome of the last change, shown until dismissed.
    outcome: Option<Outcome>,
    /// The Export dialog, while open.
    exporting: Option<ExportView>,
    /// Exports asked for and not answered yet: only the answer to the last one fills the dialog.
    exports_pending: usize,
    /// Save dialogs asked for and not answered yet: an answer counts only when it is the last one asked for.
    export_choices_pending: usize,
    /// The file name for a save dialog asked for in this frame: it is opened after drawing, where Klefki's window is
    /// at hand, so that the dialog belongs to it.
    export_choice_asked: Option<String>,
    /// Import asked for a file: the system's open dialog is opened next, over the window, by its thread.
    import_choice_asked: bool,
    /// A file to import was asked for (the dialog, or a path to read) and the engine has not answered yet.
    import_waiting: bool,
    pub page: Page,
    /// The Activity log page while it is shown; `None` on every other page, so the log is not held in memory.
    pub activity: Option<crate::activity::ActivityPage>,
    /// The Claude Desktop page while it is shown; like the Activity log, read when opened and let go when left.
    pub desktop: Option<crate::desktop::DesktopPage>,
    /// The Secrets page while it is shown; `None` on every other page, like the Activity log.
    pub secrets: Option<crate::secrets::SecretsPage>,
    /// The Profiles page while it is shown; read when opened and let go when left, like the Activity log.
    pub profiles: Option<crate::profiles::ProfilesPage>,
    /// A profile being made or edited when the Profiles page was left, to carry on with when it is opened again.
    profile_draft: Option<crate::profiles::Draft>,
    /// The profiles' names as last read, for the palette to offer each: with each snapshot, and after a save or delete.
    profile_names: Vec<String>,
    pub search: String,
    pub selected: BTreeSet<usize>,
    pub tab: DetailTab,
    pub theme: ThemeChoice,
    pub tone: Tone,
    /// Klefki's own updates: when to look, and what was found.
    pub updates: crate::updates::Updates,
    /// This build can check for updates (it has a release key).
    checks_for_updates: bool,
    /// Why this copy of Klefki is updated by hand (the MSI, the zip, the `.deb`); `None` when it installs updates
    /// itself.
    updated_by_hand: Option<String>,
}

impl ShellApp {
    pub(crate) fn new(snapshot: Arc<Snapshot>, theme: ThemeChoice) -> Self {
        Self {
            core: None,
            live: vec![None; snapshot.servers.len()],
            grid_cache: None,
            on_screen: Vec::new(),
            on_screen_now: Vec::new(),
            live_gen: 0,
            explained: None,
            legacy_cache: None,
            diagnoses: HashMap::new(),
            last_starts: HashMap::new(),
            inspector: None,
            debug_report: None,
            reveal_diagnosis: None,
            longer_since: None,
            save_bar_wanted: false,
            save_bar_height: 0.0,
            save_bar_shown: false,
            list_room: None,
            outcome_inline: false,
            outcome_chosen: None,
            cli_version: None,
            cli_problem: None,
            plan: None,
            diff_tops: DiffTops::default(),
            applying: false,
            preparing: false,
            editor: None,
            draft_asked: false,
            profile_asked: None,
            draft_plan: None,
            command: CommandPalette::default(),
            duplicate: None,
            add: None,
            add_mode: AddMode::default(),
            adding: Vec::new(),
            adding_signs_in: false,
            adding_verb: "Added",
            moving: None,
            moved_from: None,
            relocating: Vec::new(),
            applying_step: None,
            forgetting: None,
            forget_typed: false,
            reveal: None,
            casing: None,
            sign_in: None,
            secret: None,
            secret_input: None,
            outcome: None,
            exporting: None,
            exports_pending: 0,
            export_choices_pending: 0,
            export_choice_asked: None,
            import_choice_asked: false,
            import_waiting: false,
            snapshot,
            page: Page::Connections,
            activity: None,
            desktop: None,
            secrets: None,
            profiles: None,
            profile_draft: None,
            profile_names: Vec::new(),
            search: String::new(),
            selected: BTreeSet::new(),
            tab: DetailTab::General,
            theme,
            tone: Tone::default(),
            updates: crate::updates::Updates::default(),
            checks_for_updates: false,
            updated_by_hand: None,
        }
    }

    /// The app driven by the engine: the snapshot and every status arrive as events.
    pub(crate) fn with_core(core: Core, theme: ThemeChoice) -> Self {
        let mut app = Self::new(Arc::new(Snapshot::default()), theme);
        app.updated_by_hand = core.updated_by_hand().map(str::to_owned);
        app.checks_for_updates = core.checks_for_updates();
        app.core = Some(core);
        app
    }

    fn send(&self, command: Command) {
        if let Some(core) = &self.core {
            core.send(command);
        }
    }

    /// Shows `page`. The Activity log, Claude Desktop's servers and the Secrets page are read each time they are
    /// opened and let go when they are left.
    fn go(&mut self, page: Page) {
        if page == Page::ActivityLog {
            if self.activity.is_none() {
                self.activity = Some(crate::activity::ActivityPage::default());
                self.send(Command::Activity);
            }
        } else {
            self.activity = None;
        }
        if page == Page::Desktop {
            if self.desktop.is_none() {
                self.desktop = Some(crate::desktop::DesktopPage::default());
                self.send(Command::Desktop);
            }
        } else {
            self.desktop = None;
        }
        if page == Page::Secrets {
            if self.secrets.is_none() {
                self.secrets = Some(crate::secrets::SecretsPage::default());
                self.send(Command::Secrets);
            }
        } else {
            self.secrets = None;
        }
        if page == Page::Profiles {
            if self.profiles.is_none() {
                let draft = self.profile_draft.take();
                self.profiles = Some(crate::profiles::ProfilesPage::with_draft(draft));
                self.send(Command::Profiles);
            }
        } else if let Some(mut left) = self.profiles.take() {
            // A profile being made or edited is not lost by looking at another page.
            self.profile_draft = left.draft();
        }
        self.page = page;
    }

    /// `id` on the Connections page, selected and in view, at `tab`. Where the Secrets page's Open leads.
    fn show_server(&mut self, id: &ServerId, tab: crate::secrets::Tab) {
        self.go(Page::Connections);
        let Some(i) = self.snapshot.servers.iter().position(|r| r.is(id)) else {
            return;
        };
        if !self.visible().contains(&i) {
            // A search that hides it would hide what was asked for.
            self.search.clear();
        }
        self.selected = BTreeSet::from([i]);
        self.reveal = Some(i);
        self.tab = match tab {
            crate::secrets::Tab::General => DetailTab::General,
            crate::secrets::Tab::Env => DetailTab::HeadersEnv,
            crate::secrets::Tab::Auth => DetailTab::Auth,
        };
    }

    /// Re-reads the configuration and re-checks every server; on the Claude Desktop page, its file too.
    fn refresh(&self) {
        self.send(Command::Refresh { live_all: true });
        if self.desktop.is_some() {
            self.send(Command::Desktop);
        }
    }

    fn poll_core(&mut self) {
        let mut events = Vec::new();
        if let Some(core) = &self.core {
            while let Some(event) = core.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            self.apply_event(event);
        }
    }

    fn apply_event(&mut self, event: Event) {
        // A change saved while the Activity log is shown: read it again. What is shown stays until it arrives.
        if self.activity.is_some()
            && matches!(
                event,
                Event::Applied { changed: true, .. } | Event::ApplyFailed { .. }
            )
        {
            self.send(Command::Activity);
        }
        // Anything that can change where secrets are, while the Secrets page is shown: read it again. What is shown
        // stays until the answer arrives.
        // A file change (Claude Code saves often) does not ask the system's store again: that can prompt to unlock it.
        if self.secrets.is_some() {
            match event {
                Event::Snapshot(_) => self.send(Command::SecretsAfterFileChange),
                Event::Applied { changed: true, .. }
                | Event::ApplyFailed { .. }
                | Event::SecretStatus { .. } => self.send(Command::Secrets),
                _ => {}
            }
        }
        match event {
            Event::Update(found) => {
                self.updates.answered(found, crate::updates::now());
                if self.updated_by_hand.is_none() && self.updates.download_due() {
                    self.download_update();
                }
            }
            Event::UpdateDownload(done) => {
                // Found out only now (the app's folder cannot be changed): the page is the way from here.
                if let UpdateDownload::Manual(why) = &done {
                    self.updated_by_hand = Some(why.clone());
                }
                self.updates.downloaded(done);
            }
            Event::Desktop(desktop) => {
                // Not kept once the page is left: it is read again when the page is opened.
                if let Some(page) = &mut self.desktop {
                    page.set(desktop);
                }
            }
            Event::Secrets(report) => {
                if let Some(page) = &mut self.secrets {
                    page.report = Some(report);
                }
            }
            Event::UnusedSecretForgotten { connection, result } => {
                if let Some(page) = &mut self.secrets {
                    let name = page
                        .report
                        .as_ref()
                        .and_then(|r| r.unused.iter().find(|u| u.connection == connection))
                        .map_or_else(|| "the server".to_owned(), |u| u.name.clone());
                    page.outcome = Some(match result {
                        Ok(()) => (format!("Forgot the client secret kept for {name}."), false),
                        Err(why) => (format!("Kept the client secret for {name}: {why}"), true),
                    });
                }
            }
            Event::Profiles(profiles) => {
                let names: Vec<String> = profiles.profiles.iter().map(|p| p.name.clone()).collect();
                if names != self.profile_names {
                    self.profile_names = names;
                    self.command.invalidate();
                }
                if let Some(page) = &mut self.profiles {
                    page.set(profiles);
                }
            }
            Event::ProfileRefused(why) => {
                if let Some(page) = &mut self.profiles {
                    page.refused(why);
                }
            }
            Event::Activity(log) => {
                // Not kept once the page is left: it is read again when the page is opened.
                if let Some(page) = &mut self.activity {
                    page.log = Some(log);
                }
            }
            Event::Snapshot(snapshot) => self.take_snapshot(snapshot),
            Event::Diagnosis { id, diagnosis } => {
                if self.snapshot.servers.iter().any(|r| r.is(&id)) {
                    // It is drawn under the status and its explanation, often below the window: brought into view
                    // when it is for the server being looked at, and left where it is otherwise.
                    if self.tab == DetailTab::Status
                        && self.single_selected().is_some_and(|s| s.is(&id))
                    {
                        self.reveal_diagnosis = Some(id.clone());
                    }
                    self.diagnoses.insert(id, diagnosis);
                }
            }
            Event::LastStarts { id, starts } => {
                if let Some(seen) = self.last_starts.get_mut(&id) {
                    seen.starts = Some(starts);
                }
            }
            Event::InspectorCommand { id, line } => self.inspector_arrived(&id, line),
            Event::RedactedDefinition { id, result } => {
                self.redacted_definition_arrived(&id, &result);
            }
            Event::PlanReady(plan) => self.plan_ready(plan),
            Event::PlanFailed(why) => self.plan_failed(why),
            Event::Applied {
                plan,
                changed,
                backup,
                notes,
            } => self.applied(plan, changed, backup, notes),
            Event::Applying { plan, step } => {
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.applying_step = Some(step);
                }
            }
            Event::ApplyFailed { plan, error } => self.apply_failed(plan, error),
            Event::Reapplied { .. } => {
                self.outcome = Some(Outcome::problem(
                    "Claude Code overwrote the change right after it was saved, so it was written again. Close running Claude Code sessions if this keeps happening."
                        .to_owned(),
                ));
            }
            Event::Entry { id, json, editable } => {
                self.copy_read(&id, &json);
                self.definition_read(id, json, editable);
            }
            Event::ImportFile(file) => {
                self.import_waiting = false;
                let in_dialog = self.add.as_ref().is_some_and(|f| f.mode == AddMode::Import);
                if file != ImportFile::Cancelled {
                    self.open_import(file);
                } else if !in_dialog {
                    // A dialog closed without a file may have been one that could not show a file chooser (a
                    // desktop portal without one fails slowly, like a cancel): a way on is offered, at no cost to a
                    // real cancel.
                    self.note("No file chosen.".to_owned(), true);
                }
            }
            Event::Exported(exported) => self.exported(exported),
            Event::ExportFileChosen(choice) => self.export_file_chosen(choice),
            Event::ExportSaved {
                path,
                shown,
                result,
            } => self.export_saved(path, shown, result),
            Event::CallbackPort { port, warning } => {
                if let Some(form) = &mut self.add {
                    form.port_answered(port, warning.clone());
                }
                if let Some(form) = &mut self.duplicate {
                    form.port_answered(port, warning);
                }
            }
            Event::SecretStatus { id, stored } => {
                if let Some(view) = &mut self.secret {
                    view.answered(&id, stored);
                }
            }
            Event::SignIn { id, step } => {
                let Some(view) = self.sign_in.as_mut().filter(|v| v.id == id) else {
                    return;
                };
                if view.step(step) {
                    self.sign_in = None;
                    self.outcome = Some(Outcome {
                        text: format!("Signed in to {}.", id.name),
                        ok: true,
                        shown_at: None,
                        warn: false,
                        undo: None,
                        sign_in: None,
                        logged: false,
                        type_path: false,
                    });
                }
            }
            Event::CliVersion(version) => {
                // Sent once the program is found again (installed, say, then F5): the earlier problem is over.
                self.cli_version = version;
                self.cli_problem = None;
            }
            Event::CliProblem(problem) => self.cli_problem = Some(problem),
            Event::Status {
                id,
                status,
                detail,
                hint,
                timing,
            } => {
                // The check given more time has answered (or another has): the seconds stop.
                if status != Status::Checking
                    && self
                        .longer_since
                        .as_ref()
                        .is_some_and(|(asked, _)| *asked == id)
                {
                    self.longer_since = None;
                }
                if status != Status::Checking
                    && let Some(seen) = self.last_starts.get_mut(&id)
                {
                    seen.asked = false;
                }
                if let Some(i) = self.snapshot.servers.iter().position(|r| r.is(&id)) {
                    self.live[i] = Some(Live {
                        status,
                        detail,
                        hint,
                        timing,
                    });
                    self.live_gen += 1;
                }
            }
        }
    }

    /// A new snapshot of the configuration: what was selected, checked and diagnosed follows its server.
    fn take_snapshot(&mut self, snapshot: Arc<Snapshot>) {
        // Rows are kept by position; carry selection, live results and diagnoses over by identity: the
        // connection, as the file spells the server, since two servers can show the same name.
        let new_index: HashMap<&ConnectionId, usize> = snapshot
            .servers
            .iter()
            .enumerate()
            .map(|(i, r)| (&r.connection, i))
            .collect();
        let moved = |old: usize| {
            new_index
                .get(&self.snapshot.servers[old].connection)
                .copied()
        };
        let selected = self.selected.iter().filter_map(|&i| moved(i)).collect();
        let mut live = vec![None; snapshot.servers.len()];
        for (old, result) in self.live.iter_mut().enumerate() {
            // Dropped; the engine says again, right after each snapshot, which checks still run.
            if let (Some(new), Some(r)) = (moved(old), result.take())
                && r.status != Status::Checking
            {
                live[new] = Some(r);
            }
        }
        self.live_gen += 1;
        self.diagnoses
            .retain(|id, _| new_index.contains_key(&id.connection));
        // Read again when next shown: the server's definition, and so what is hidden in its log, may have changed.
        self.last_starts
            .retain(|id, _| new_index.contains_key(&id.connection));
        for seen in self.last_starts.values_mut() {
            seen.asked = false;
        }
        // The definition may have changed.
        self.inspector = None;
        self.debug_report = None;
        self.selected = selected;
        self.live = live;
        // Positions now mean other servers: tell the engine again.
        self.on_screen.clear();
        self.follow_editor(&snapshot, &new_index);
        self.grid_cache = None;
        self.command.invalidate();
        if let Some(page) = &mut self.profiles {
            page.snapshot_changed();
        }
        self.profile_names.clone_from(&snapshot.profiles);
        if snapshot.cli_version.is_some() {
            self.cli_version = snapshot.cli_version.clone();
        }
        self.snapshot = snapshot;
    }

    /// What to show for row `i`: the engine's latest answer, else what Claude's own cache said.
    fn effective(&self, i: usize) -> (Status, Option<&str>) {
        match self.live.get(i).and_then(Option::as_ref) {
            Some(live) => (live.status, live.detail.as_deref()),
            None => (self.snapshot.servers[i].status, None),
        }
    }

    /// Row `i` shows what Claude's cache last said, not an answer of this session's own checks. The cache can be
    /// days old, so it says so until the check answers.
    fn last_known(&self, i: usize) -> bool {
        self.live.get(i).is_some_and(Option::is_none) && self.snapshot.servers[i].status_cached
    }

    /// What the engine's latest answer for row `i` most likely means, in plain words.
    fn hint(&self, i: usize) -> Option<&Hint> {
        self.live.get(i)?.as_ref()?.hint.as_ref()
    }

    /// How long the row's last check took against Claude Code's time limit, when it ran out of time or was given
    /// more.
    fn timing(&self, i: usize) -> Option<&Timing> {
        self.live.get(i)?.as_ref()?.timing.as_ref()
    }

    /// Indices into the snapshot matching the search, ordered by scope, project and name.
    pub(crate) fn visible(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        let idx: Vec<usize> = (0..self.snapshot.servers.len())
            .filter(|&i| matches_search(&self.snapshot.servers[i], &q))
            .collect();
        self.in_list_order(idx)
    }

    /// `idx` (indices into the snapshot) ordered as the list shows them: by scope, project and name.
    fn in_list_order(&self, mut idx: Vec<usize>) -> Vec<usize> {
        // Each name is lower-cased once, not once per comparison.
        idx.sort_by_cached_key(|&i| {
            let s = &self.snapshot.servers[i];
            (s.scope, s.project.as_deref(), s.name.to_lowercase())
        });
        idx
    }

    fn show(&mut self, ui: &mut Ui) {
        self.poll_core();
        self.look_for_updates(ui.ctx());
        if ui.input(|i| i.key_pressed(egui::Key::F5)) {
            self.refresh();
        }
        let ctx = ui.ctx().clone();
        self.take_dropped_file(&ctx);
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K))
            && self.plan.is_none()
            && self.duplicate.is_none()
            && self.moving.is_none()
            && self.add.is_none()
            && self.casing.is_none()
            && self.sign_in.is_none()
            && self.exporting.is_none()
        {
            if self.command.is_open() {
                self.command.close();
            } else {
                self.command.open();
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            ctx.memory_mut(|m| m.request_focus(search_id()));
        }
        let idle = !ctx.egui_wants_keyboard_input()
            && self.plan.is_none()
            && !self.command.is_open()
            && self.duplicate.is_none()
            && self.moving.is_none()
            && self.add.is_none()
            && self.casing.is_none()
            && self.sign_in.is_none()
            && self.exporting.is_none();
        // Up and down walk the list, like any list.
        if idle && self.page == Page::Connections {
            let step = ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown) as i32
                    - i.key_pressed(egui::Key::ArrowUp) as i32
            });
            if step != 0 {
                self.move_selection(step);
            }
        }
        // Del and Ctrl+E act on the selection, unless a text field has the keyboard (Del edits text there). Only where
        // the selection is in view: on another page they would act on a server the user cannot see.
        if idle && self.core.is_some() && self.page == Page::Connections {
            if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.prepare_delete();
            }
            if self.selected.len() == 1
                && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::E))
            {
                self.edit_selected();
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::D)) {
                self.open_duplicate();
            }
            // Not Ctrl+M: on macOS, Cmd+M minimizes the window.
            if ui.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::M))
            {
                self.open_move();
            }
        }
        // Adding and importing need no selection, so they work from any page.
        if idle
            && self.core.is_some()
            && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::N))
        {
            self.open_add();
        }
        if idle && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.ask_import();
        }
        let pal = theme::palette(ui.ctx());
        self.save_bar_shown = false;
        self.outcome_inline = false;
        egui::Panel::top("top_bar")
            .frame(bar_frame(pal))
            .show(ui, |ui| self.top_bar(ui));
        let nav = egui::Panel::left("nav")
            .resizable(false)
            .exact_size(220.0)
            .frame(
                Frame::new()
                    .fill(pal.chrome)
                    .inner_margin(Margin::symmetric(0, 10)),
            )
            .show(ui, |ui| self.nav(ui));
        if self.page == Page::Connections {
            // A share of the window, so explanations get room on a wide screen without crowding a narrow one.
            let width = ui.available_width();
            // Named anew when its sizes changed: egui remembers a panel's width by name.
            let panel = egui::Panel::right("detail-panel")
                .default_size((width * 0.34).clamp(360.0, 640.0))
                .min_size(280.0)
                .max_size((width * 0.5).max(460.0))
                // The right margin is mostly inside, where the scroll bar shows (`scroll_gutter`).
                .frame(Frame::new().fill(pal.panel).inner_margin(Margin {
                    right: 12 - scroll_gutter(ui.style()).min(12),
                    ..Margin::same(12)
                }))
                .show(ui, |ui| self.detail(ui));
            let (nav, panel) = (nav.response.rect, panel.response.rect);
            self.list_room = Some(egui::Rect::from_x_y_ranges(
                nav.right()..=panel.left(),
                panel.y_range(),
            ));
            // The panel was laid out with the message in it, or without, by the room last frame; the room it
            // leaves now decides.
            if self.save_bar_shown
                && self.outcome.is_some()
                && self.outcome_inline == self.outcome_fits_beside(ui.ctx())
            {
                ui.ctx().request_discard(
                    "the outcome message moved beside the detail panel or into it",
                );
            }
        }
        self.plan_dialog(&ctx);
        self.duplicate_dialog(&ctx);
        self.move_dialog(&ctx);
        self.add_dialog(&ctx);
        if std::mem::take(&mut self.forget_typed) {
            forget_typed(&ctx);
        }
        self.casing_dialog(&ctx);
        self.sign_in_dialog(&ctx);
        self.export_dialog(&ctx);
        self.outcome_toast(&ctx);
        let mut command = std::mem::take(&mut self.command);
        let chosen = command.show(&ctx, || self.commands());
        self.command = command;
        if let Some(action) = chosen {
            self.run(&ctx, action);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(14)))
            .show(ui, |ui| match self.page {
                Page::Connections => {
                    self.warnings(ui);
                    self.organisation_notes(ui);
                    self.legacy_notice(ui);
                    self.kept_notice(ui);
                    self.toolbar(ui);
                    ui.add_space(6.0);
                    self.grid(ui);
                }
                // Taller than a small window once Claude Code is not found, with what to do about it.
                Page::Settings => {
                    egui::ScrollArea::vertical()
                        .id_salt("settings")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.settings(ui);
                            bring_focus_into_view(ui);
                        });
                }
                Page::Desktop => {
                    if let Some(page) = &mut self.desktop {
                        page.show(ui, &self.snapshot);
                    }
                }
                Page::Secrets => {
                    let live = self.core.is_some();
                    let asked = self.secrets.as_mut().and_then(|page| page.show(ui, live));
                    match asked {
                        Some(crate::secrets::Asked::Open(id, tab)) => self.show_server(&id, tab),
                        Some(crate::secrets::Asked::Forget(connection)) => {
                            self.send(Command::ForgetUnusedSecret(connection));
                        }
                        Some(crate::secrets::Asked::Reload) => {
                            if let Some(page) = &mut self.secrets {
                                page.outcome = None;
                            }
                            self.send(Command::Secrets);
                        }
                        None => {}
                    }
                }
                Page::Profiles => {
                    let live = self.core.is_some();
                    let snapshot = Arc::clone(&self.snapshot);
                    let asked = self
                        .profiles
                        .as_mut()
                        .and_then(|page| page.show(ui, &snapshot, live));
                    match asked {
                        Some(crate::profiles::Asked::Save {
                            name,
                            members,
                            replaces,
                        }) => self.send(Command::SaveProfile {
                            name,
                            members,
                            replaces,
                        }),
                        Some(crate::profiles::Asked::Delete(name)) => {
                            self.send(Command::DeleteProfile(name));
                        }
                        Some(crate::profiles::Asked::Use(change)) => {
                            if self.can_prepare()
                                && let Change::UseProfile { name, .. } = change.as_ref()
                            {
                                self.profile_asked = Some(name.clone());
                            }
                            self.prepare(vec![*change]);
                        }
                        None => {}
                    }
                }
                Page::ActivityLog => {
                    let restore = self.activity.as_mut().and_then(|page| page.show(ui));
                    if let Some(backup) = restore {
                        self.prepare(vec![Change::Restore { backup }]);
                    }
                }
                other => {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(other.glyph())
                                .size(32.0)
                                .color(pal.text_muted),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new(other.title()).heading());
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(RichText::new(other.promise()).color(pal.text_muted))
                                .wrap(),
                        );
                        ui.add_space(12.0);
                        if ui.button("Back to connections").clicked() {
                            self.go(Page::Connections);
                        }
                    });
                }
            });
    }

    fn nav(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        for (page, glyph, title) in Page::ALL {
            let active = self.page == page;
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, title)
            });
            // The menu orients; it should not compete with the list. Inactive items are dim, icons small and
            // all one colour, and the active item is a quiet rounded fill rather than a coloured bar.
            let pill = rect.shrink2(egui::vec2(8.0, 3.0));
            if active {
                ui.painter().rect_filled(pill, 6.0, pal.hover);
            } else if resp.hovered() {
                ui.painter().rect_filled(pill, 6.0, pal.row_alt);
            }
            let color = if active {
                pal.text
            } else if page.ready() {
                pal.text_muted
            } else {
                pal.disabled
            };
            ui.painter().text(
                rect.left_center() + egui::vec2(22.0, 0.0),
                egui::Align2::LEFT_CENTER,
                glyph,
                egui::FontId::proportional(16.0),
                color,
            );
            let label = ui.painter().text(
                rect.left_center() + egui::vec2(48.0, 0.0),
                egui::Align2::LEFT_CENTER,
                title,
                egui::FontId::proportional(15.0),
                color,
            );
            if !page.ready() {
                ui.painter().text(
                    egui::pos2(label.max.x + 8.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "soon",
                    egui::FontId::proportional(12.0),
                    pal.disabled,
                );
            }
            if resp.clicked() {
                self.go(page);
            }
        }
    }

    /// Problems found while reading Claude's configuration, or a hint when nothing is configured.
    fn warnings(&self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        const SHOWN: usize = 4;
        let warnings: Vec<&str> = self
            .snapshot
            .warnings
            .iter()
            .map(String::as_str)
            .chain(self.cli_problem.as_deref())
            .collect();
        if warnings.is_empty() {
            return;
        }
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for w in warnings.iter().take(SHOWN) {
                    ui.label(RichText::new(format!("{} {w}", icon::WARNING)).color(pal.warn));
                }
                if warnings.len() > SHOWN {
                    ui.label(
                        RichText::new(format!("… and {} more", warnings.len() - SHOWN))
                            .color(pal.text_muted),
                    );
                }
            });
        ui.add_space(6.0);
    }

    /// What Claude Code ignores in the organisation's files: one muted line each, whole on hover. Only an
    /// administrator can change it, so it does not take the warnings' room.
    fn organisation_notes(&self, ui: &mut Ui) {
        let notes = &self.snapshot.organisation_notes;
        if notes.is_empty() {
            return;
        }
        let pal = theme::palette(ui.ctx());
        for note in notes {
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{} {note}", icon::INFO)).color(pal.text_muted),
                )
                .truncate(),
            );
        }
        ui.add_space(6.0);
    }

    /// Everything the command palette offers right now: actions on the selection first, then the rest.
    pub(crate) fn commands(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let selected: Vec<&ServerRow> = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .collect();
        let live = self.core.is_some();
        let mut add = |icon: &'static str,
                       title: String,
                       subtitle: String,
                       key: Option<String>,
                       action: command::Action| {
            out.push(Entry {
                icon,
                title,
                subtitle,
                shortcut: key,
                action,
            });
        };
        let named = |rows: &[&ServerRow]| match rows {
            [one] => one.name.clone(),
            _ => format!("{} servers", rows.len()),
        };
        // Every server is switched, a plugin's too.
        if live && !selected.is_empty() {
            let what = named(&selected);
            add(
                icon::TOGGLE_RIGHT,
                format!("Turn on {what}"),
                String::new(),
                None,
                command::Action::TurnOn,
            );
            add(
                icon::TOGGLE_LEFT,
                format!("Turn off {what}"),
                String::new(),
                None,
                command::Action::TurnOff,
            );
        }
        // Beyond switching, a plugin's servers are read-only: nothing here changes, checks or exports them. With
        // others selected, the actions on the selection take those.
        let (changeable, _) = self.changeable_selected();
        if live && !changeable.is_empty() {
            let what = named(&changeable);
            if selected.len() == 1 {
                let s = selected[0];
                add(
                    icon::PENCIL_SIMPLE,
                    format!("Edit {}", s.name),
                    "Command, address, arguments, environment".into(),
                    Some(shortcut("E")),
                    command::Action::Edit,
                );
                if s.scope != Scope::Project {
                    add(
                        icon::COPY,
                        format!("Duplicate {}", s.name),
                        "To another project or the user".into(),
                        Some(shortcut("D")),
                        command::Action::Duplicate,
                    );
                    add(
                        icon::ARROW_FAT_LINES_RIGHT,
                        format!("Move {}", s.name),
                        "To another project or the user".into(),
                        Some(shortcut("Shift+M")),
                        command::Action::Move,
                    );
                }
                if s.casing_collision {
                    add(
                        icon::WARNING,
                        format!("Fix casing of {}", s.name),
                        "Give servers that differ only in case one spelling".into(),
                        None,
                        command::Action::FixCasing,
                    );
                }
                if sign_in::can_sign_in(s) && self.sign_in.is_none() {
                    add(
                        icon::SIGN_IN,
                        format!("Sign in to {}", s.name),
                        "Through Claude Code, in your browser".into(),
                        None,
                        command::Action::SignIn,
                    );
                }
                add(
                    icon::PULSE,
                    format!("Check {} now", s.name),
                    String::new(),
                    None,
                    command::Action::Check,
                );
                add(
                    icon::STETHOSCOPE,
                    format!("Diagnose {}", s.name),
                    "Explain what goes wrong".into(),
                    None,
                    command::Action::Diagnose,
                );
                if self.core.is_some() {
                    add(
                        icon::CLIPBOARD_TEXT,
                        format!("Copy debug report for {}", s.name),
                        "Copies its status, definition and logs as text; secrets replaced".into(),
                        None,
                        command::Action::CopyDebugReport,
                    );
                }
                if self.core.is_some() && inspector::offers(s) {
                    add(
                        icon::TERMINAL_WINDOW,
                        format!("Copy Inspector command for {}", s.name),
                        "Copies the line that opens it in the MCP Inspector; no secrets in it"
                            .into(),
                        None,
                        command::Action::CopyInspector,
                    );
                }
                if s.scope == Scope::Project && s.approval.is_some_and(|a| a != Approval::Approved)
                {
                    add(
                        icon::SHIELD_CHECK,
                        format!("Review and approve {}", s.name),
                        String::new(),
                        None,
                        command::Action::Approve,
                    );
                }
            }
            add(
                icon::EXPORT,
                format!("Export {what}"),
                "One mcpServers block; secrets and settings as ${VAR} references".into(),
                None,
                command::Action::Export,
            );
            add(
                icon::TRASH,
                format!("Delete {what}"),
                String::new(),
                Some("Del".into()),
                command::Action::Delete,
            );
        }
        if live && self.snapshot.legacy_disabled.iter().any(|l| !l.malformed) {
            add(
                icon::CLOCK_COUNTER_CLOCKWISE,
                "Migrate old switch-off lists".into(),
                "Switch off what older Claude Code versions had off".into(),
                None,
                command::Action::MigrateLegacy,
            );
        }
        if live {
            add(
                icon::ARROWS_CLOCKWISE,
                "Refresh".into(),
                "Read the configuration and check every server".into(),
                Some("F5".into()),
                command::Action::Refresh,
            );
        }
        add(
            icon::MAGNIFYING_GLASS,
            "Filter servers".into(),
            String::new(),
            Some(shortcut("F")),
            command::Action::Search,
        );
        for choice in ThemeChoice::ALL {
            add(
                choice.icon(),
                format!("Theme: {}", choice.label()),
                String::new(),
                None,
                command::Action::Theme(choice),
            );
        }
        for tone in Tone::ALL {
            add(
                icon::PALETTE,
                format!("Greys: {}", tone.label()),
                String::new(),
                None,
                command::Action::Tone(tone),
            );
        }
        add(
            icon::GEAR,
            "Settings".into(),
            "Appearance".into(),
            None,
            command::Action::Settings,
        );
        for i in self.visible() {
            let s = &self.snapshot.servers[i];
            add(
                icon::ARROW_RIGHT,
                format!("Go to {}", s.name),
                group_of(s).full,
                None,
                command::Action::Select(i),
            );
        }
        // After the servers, so they stay in view of an empty search; typing "act" finds it.
        add(
            icon::LIST_BULLETS,
            "Activity log".into(),
            "Changes Klefki saved, and their backups".into(),
            None,
            command::Action::ActivityLog,
        );
        add(
            icon::DESKTOP,
            "Claude Desktop".into(),
            "The servers in Claude Desktop's configuration, read-only".into(),
            None,
            command::Action::Desktop,
        );
        add(
            icon::STACK,
            "Profiles".into(),
            "Named sets of servers to turn on together in a project".into(),
            None,
            command::Action::Profiles,
        );
        if live {
            for name in &self.profile_names {
                add(
                    icon::STACK,
                    format!("Use profile {name}…"),
                    "Choose a project, then review".into(),
                    None,
                    command::Action::UseProfile(name.clone()),
                );
            }
            add(
                icon::PLUS,
                "New profile".into(),
                "Pick servers that belong together and give them a name".into(),
                None,
                command::Action::NewProfile,
            );
        }
        // Last, so the servers stay in view of an empty search; the toolbar and Ctrl+N are the quick way.
        if live {
            add(
                icon::PLUS,
                "Add a server".into(),
                "Paste a claude mcp add command or an mcpServers block".into(),
                Some(shortcut("N")),
                command::Action::Add,
            );
            add(
                icon::TRAY_ARROW_DOWN,
                "Import servers from a file".into(),
                "An export, or any mcpServers block; reviewed before anything is added".into(),
                Some(shortcut("O")),
                command::Action::Import,
            );
            // With a selection, Export is among the actions on it, above.
            if self.selected.is_empty() && self.can_export() {
                add(
                    icon::EXPORT,
                    "Export the servers in the list".into(),
                    "One mcpServers block; secrets and settings as ${VAR} references".into(),
                    None,
                    command::Action::Export,
                );
            }
        }
        out
    }

    pub(crate) fn run(&mut self, ctx: &egui::Context, action: command::Action) {
        use command::Action as A;
        match action {
            A::Select(i) => {
                self.selected.clear();
                self.selected.insert(i);
                self.go(Page::Connections);
            }
            A::TurnOn => self.prepare_toggle(true),
            A::TurnOff => self.prepare_toggle(false),
            A::Delete => self.prepare_delete(),
            A::Edit => self.edit_selected(),
            A::Check | A::Diagnose | A::Approve => {
                if let Some(s) = self.single_selected() {
                    let id = s.id();
                    match action {
                        A::Check => self.send(Command::Check(id)),
                        A::Diagnose => {
                            self.tab = DetailTab::Status;
                            self.send(Command::Diagnose(id));
                        }
                        _ => {
                            self.outcome = None;
                            self.send(Command::Prepare(vec![Change::Approve { id }]));
                        }
                    }
                }
            }
            A::CopyDebugReport => {
                if let Some(id) = self.single_selected().map(ServerRow::id) {
                    self.tab = DetailTab::Status;
                    self.copy_debug_report(id);
                }
            }
            A::CopyInspector => {
                if let Some(s) = self.single_selected().filter(|s| inspector::offers(s)) {
                    let id = s.id();
                    self.tab = DetailTab::Status;
                    self.copy_inspector_command(id);
                }
            }
            A::Refresh => self.refresh(),
            A::Search => ctx.memory_mut(|m| m.request_focus(search_id())),
            A::Theme(choice) => {
                self.theme = choice;
                theme::set_choice(ctx, choice);
            }
            A::Tone(tone) => {
                self.tone = tone;
                theme::set_tone(ctx, tone);
            }
            A::Settings => self.go(Page::Settings),
            A::ActivityLog => self.go(Page::ActivityLog),
            A::Desktop => self.go(Page::Desktop),
            A::Profiles => self.go(Page::Profiles),
            A::NewProfile | A::UseProfile(_) => {
                self.go(Page::Profiles);
                if let Some(page) = &mut self.profiles {
                    match action {
                        A::UseProfile(name) => page.reveal(name),
                        _ => page.start_new(),
                    }
                }
            }
            A::Duplicate => self.open_duplicate(),
            A::Move => self.open_move(),
            A::Add => self.open_add(),
            A::Import => self.ask_import(),
            A::FixCasing => self.open_casing(),
            A::MigrateLegacy => self.migrate_legacy(),
            A::Export => self.open_export(),
            A::SignIn => {
                if let Some(s) = self.single_selected().cloned() {
                    self.start_sign_in(&s);
                }
            }
        }
    }

    /// The selected rows, without plugin servers, which Klefki only shows (decision 008); and how many those were.
    fn changeable_selected(&self) -> (Vec<&ServerRow>, usize) {
        let rows: Vec<&ServerRow> = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .collect();
        let all = rows.len();
        let rows: Vec<&ServerRow> = rows
            .into_iter()
            .filter(|s| !s.scope.is_read_only())
            .collect();
        let left_out = all - rows.len();
        (rows, left_out)
    }

    /// Why the buttons that change servers are off: everything selected comes from a plugin. `None` otherwise.
    pub(crate) fn read_only_hint(&self) -> Option<String> {
        let (rows, plugins) = self.changeable_selected();
        if !rows.is_empty() || plugins == 0 {
            return None;
        }
        Some(match self.single_selected() {
            Some(s) if s.scope == Scope::Managed => {
                format!(
                    "{} is set by your organisation; only an administrator changes it",
                    s.name
                )
            }
            Some(s) if s.scope == Scope::ClaudeAi => {
                format!(
                    "{} is a claude.ai connector and is changed on claude.ai",
                    s.name
                )
            }
            Some(s) => format!("{} comes from a plugin and is changed there", s.name),
            None => {
                "the selected servers come from plugins, your organisation or claude.ai and are changed there"
                    .to_owned()
            }
        })
    }

    /// Says what a change to the selection left out: the plugin and organisation servers in it. With none left to
    /// change, names the one selected and where it can be changed.
    fn note_plugins_left_out(&mut self, changed: usize, left_out: usize) {
        if left_out == 0 {
            return;
        }
        let organisation = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .any(|s| matches!(s.scope, Scope::Managed | Scope::ClaudeAi));
        let text = if changed > 0 && !organisation {
            match left_out {
                1 => "Left out 1 plugin server: a plugin's server is changed in its plugin."
                    .to_owned(),
                n => format!(
                    "Left out {n} plugin servers: a plugin's server is changed in its plugin."
                ),
            }
        } else if changed > 0 {
            let what = match left_out {
                1 => "1 server".to_owned(),
                n => format!("{n} servers"),
            };
            format!(
                "Left out {what} Klefki only switches on and off: a plugin's server is changed in its plugin, your \
                 organisation's by an administrator, a claude.ai connector on claude.ai."
            )
        } else if let Some(s) = self
            .single_selected()
            .filter(|s| s.scope == Scope::ClaudeAi)
        {
            format!(
                "{} is a connector of your claude.ai account. Change it on claude.ai; Klefki switches it on and off.",
                s.name
            )
        } else if let Some(s) = self.single_selected().filter(|s| s.scope == Scope::Managed) {
            format!(
                "{} is set by your organisation. Only an administrator can change it; Klefki switches it on and off.",
                s.name
            )
        } else if let Some(s) = self.single_selected() {
            let plugin = s.plugin.as_ref().map_or("", |p| p.plugin.as_str());
            format!(
                "{} comes from the plugin {plugin}. Change it in the plugin, or turn the plugin off in Claude Code \
                 (/plugin).",
                s.name
            )
        } else {
            if organisation {
                "The selected servers come from plugins, your organisation or claude.ai. Klefki only switches them on \
                 and off."
                    .to_owned()
            } else {
                "The selected servers come from plugins. Change them in their plugins, or turn the plugins off in \
                 Claude Code (/plugin)."
                    .to_owned()
            }
        };
        self.note(text, false);
    }

    /// Opens the sign-in dialog for `row` and starts signing in. One at a time.
    fn start_sign_in(&mut self, row: &ServerRow) {
        if self.core.is_none() || self.sign_in.is_some() || !sign_in::can_sign_in(row) {
            return;
        }
        self.sign_in = Some(SignInView::new(row));
        self.send(Command::SignIn(row.id()));
    }

    fn sign_in_dialog(&mut self, ctx: &egui::Context) {
        let Some(view) = &mut self.sign_in else {
            return;
        };
        match view.show(ctx) {
            sign_in::Next::Stay => {}
            sign_in::Next::Send(command) => self.send(command),
            sign_in::Next::Close(command) => {
                self.sign_in = None;
                if let Some(command) = command {
                    self.send(command);
                }
            }
        }
    }

    /// Something to know that is not the outcome of a change, on the outcome message; it stays until dismissed.
    /// With `type_path`, it offers to type the path of a file to import.
    fn note(&mut self, text: String, type_path: bool) {
        self.outcome = Some(Outcome {
            text,
            ok: true,
            shown_at: None,
            sign_in: None,
            warn: true,
            undo: None,
            logged: false,
            type_path,
        });
    }
    fn single_selected(&self) -> Option<&ServerRow> {
        let mut it = self.selected.iter();
        match (it.next(), it.next()) {
            (Some(&i), None) => self.snapshot.servers.get(i),
            _ => None,
        }
    }

    /// Moves the selection `step` rows through the list as shown, starting at the top when nothing is selected.
    fn move_selection(&mut self, step: i32) {
        // The rows as the list shows them, already sorted: an arrow key does not sort them again.
        let order: Vec<usize> = self
            .cached_grid_rows()
            .iter()
            .filter_map(|row| match row {
                GridRow::Server(i) => Some(*i),
                GridRow::Group(_) => None,
            })
            .collect();
        if order.is_empty() {
            return;
        }
        let at = self
            .selected
            .iter()
            .next()
            .and_then(|i| order.iter().position(|o| o == i));
        let next = match at {
            None if step > 0 => 0,
            None => order.len() - 1,
            Some(p) => p.saturating_add_signed(step as isize).min(order.len() - 1),
        };
        self.selected.clear();
        self.selected.insert(order[next]);
    }

    /// Asks the engine what deleting the selected servers would change. Nothing is written yet.
    fn prepare_delete(&mut self) {
        let (rows, left_out) = self.changeable_selected();
        let changes: Vec<Change> = rows
            .into_iter()
            .map(|row| Change::Delete { id: row.id() })
            .collect();
        let changed = changes.len();
        if !changes.is_empty() {
            self.outcome = None;
            self.send(Command::Prepare(changes));
        }
        self.note_plugins_left_out(changed, left_out);
    }

    /// Asks the engine what switching the selected servers on or off would change. Nothing is written yet.
    fn prepare_toggle(&mut self, enabled: bool) {
        let changes: Vec<Change> = self
            .selected
            .iter()
            .filter_map(|&i| self.snapshot.servers.get(i))
            .map(|row| Change::SetEnabled {
                id: row.id(),
                enabled,
            })
            .collect();
        if !changes.is_empty() {
            self.prepare(changes);
        }
    }

    /// No plan is open or on its way, so a new one may be asked for.
    fn can_prepare(&self) -> bool {
        self.plan.is_none() && !self.preparing
    }

    fn prepare(&mut self, changes: Vec<Change>) {
        if self.can_prepare() {
            self.outcome = None;
            self.preparing = true;
            self.adding_signs_in = false;
            self.moved_from = None;
            // What will be in the list afterwards, selected once it is there.
            self.adding = changes
                .iter()
                .filter_map(|c| match c {
                    Change::Add { name, targets, .. } | Change::Duplicate { name, targets, .. } => {
                        Some((name.clone(), targets.clone()))
                    }
                    Change::Move { id, to } => Some((id.name.clone(), vec![to.clone()])),
                    _ => None,
                })
                .collect();
            self.relocating = changes
                .iter()
                .filter_map(|c| match c {
                    Change::Move { id, to } => Some((id.clone(), id.name.clone(), to.clone())),
                    Change::Rename { id, name } => {
                        let here = match (id.scope, &id.project) {
                            (Scope::User, _) => Target::User,
                            (Scope::Local, Some(p)) => Target::Local(p.clone()),
                            _ => return None,
                        };
                        Some((id.clone(), name.clone(), here))
                    }
                    _ => None,
                })
                .collect();
            self.adding_verb = match changes.first() {
                Some(Change::Move { .. }) => "Moved",
                Some(Change::Duplicate { .. }) => "Copied",
                _ => "Added",
            };
            // What Forget… lets go of, said once it is done.
            self.forgetting = changes.iter().find_map(|c| match c {
                Change::ForgetKept { kept } => self
                    .snapshot
                    .kept
                    .iter()
                    .find(|k| k.id == *kept)
                    .map(|k| format!("{} {}", k.name, k.place)),
                _ => None,
            });
            self.send(Command::Prepare(changes));
        }
    }

    /// Undo for a move just made: moving it back. Not for a server that signs in, which a move back signs out again.
    fn undo_move(&mut self, changed: bool) -> Option<Change> {
        let (from, signs_in) = self.moved_from.take()?;
        if !changed || signs_in {
            return None;
        }
        let row = self.single_selected()?;
        let to = match (from.scope, from.project) {
            (Scope::Local, Some(project)) => Target::Local(project),
            _ => Target::User,
        };
        Some(Change::Move { id: row.id(), to })
    }

    /// What was just added and where, as the success message says it, with what to do next for OAuth. `None`
    /// when the change added nothing.
    fn added_in_words(&self) -> Option<String> {
        if self.adding.is_empty() {
            return None;
        }
        let mut names: Vec<&str> = Vec::new();
        for (name, _) in &self.adding {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
        // Every place any of them went (an import adds some only where none of that name was).
        let targets = self.adding.iter().flat_map(|(_, t)| t);
        let user = targets.clone().any(|t| *t == Target::User);
        let mut folders: Vec<&str> = Vec::new();
        for folder in targets.filter_map(|t| match t {
            Target::Local(p) => Some(folder_name(p)),
            Target::User => None,
        }) {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        let verb = self.adding_verb;
        // A move goes to one place: "to webapp", "to the user".
        let places = match (verb, user, &folders[..]) {
            ("Moved", true, []) => "to the user".to_owned(),
            ("Moved", false, [folder]) => format!("to {folder}"),
            _ => places_in_words(user, &folders).unwrap_or_default(),
        };
        Some(format!(
            "{verb} {} {places}.",
            list_in_words(&names, "server")
        ))
    }

    /// Selects the servers the change just made added, those that are in the list, and brings the first into view.
    fn select_added(&mut self) {
        let adding = std::mem::take(&mut self.adding);
        let added: BTreeSet<usize> = self
            .snapshot
            .servers
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                adding
                    .iter()
                    .any(|(name, targets)| targets.iter().any(|t| lands_at(row, name, t)))
            })
            .map(|(i, _)| i)
            .collect();
        let Some(&first) = added.iter().next() else {
            return;
        };
        if !self.visible().contains(&first) {
            // A search that hides it would hide what was just added.
            self.search.clear();
        }
        self.selected = added;
        self.reveal = Some(first);
    }
}

impl eframe::App for ShellApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.show(ui);
        // The save dialog opens over Klefki's window, which only this thread may touch, and belongs to it: a click
        // on Klefki cannot send it behind, and on macOS it is the window's sheet.
        if let Some(name) = self.export_choice_asked.take()
            && let Some(core) = &self.core
        {
            core.choose_export_file(name, Some(frame));
        }
        // The open dialog, likewise.
        if std::mem::take(&mut self.import_choice_asked)
            && let Some(core) = &self.core
        {
            core.choose_import_file(Some(frame));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        theme::save_choice(storage, self.theme);
        theme::save_tone(storage, self.tone);
        self.updates.save(storage);
    }

    /// A downloaded update installs as Klefki closes, unless the user restarted into it already.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(core) = &self.core
            && self.updates.ready().is_some()
        {
            let _ = core.install_update(false);
        }
    }
}

/// Selectable items (tabs, list options) are unframed until hovered or selected. egui lays an unframed
/// item out with the stroke width subtracted from its padding, then adds the stroke when the frame
/// appears, so any stroke makes the item grow on hover. Without strokes they only change colour.
fn flat_items(ui: &mut Ui) {
    let w = &mut ui.visuals_mut().widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.bg_stroke = egui::Stroke::NONE;
    }
}

/// A dialog is centred by the size it had on the previous frame. When its size changes (text pasted, a long plan
/// laid out), draw once more so it is centred, and its buttons are where they are drawn, before the next click.
pub(crate) fn settle(ctx: &egui::Context, dialog: &egui::Response) {
    let id = dialog.id.with("settled_size");
    let size = dialog.rect.size();
    if ctx.data(|d| d.get_temp::<egui::Vec2>(id)) != Some(size) {
        ctx.data_mut(|d| d.insert_temp(id, size));
        ctx.request_repaint();
    }
}

/// Esc, or a close the dialog asked for itself; not a click on the dimmed window beside it. That click is easily a
/// slip (the one that brings Klefki back to the front, say), and a dialog holds what was pasted, typed or chosen:
/// closing it takes Esc or Cancel. The click is not ignored silently: the dialog's outline flashes, as a desktop
/// dialog does, so it is clear the dialog is still waiting.
fn closed_on_purpose<T>(modal: &egui::ModalResponse<T>) -> bool {
    let dialog = &modal.response;
    let ctx = &dialog.ctx;
    let id = dialog.layer_id.id.with("nudged_at");
    let now = ctx.input(|i| i.time);
    if modal.backdrop_response.clicked() {
        ctx.data_mut(|d| d.insert_temp(id, now));
    }
    let at = ctx.data(|d| d.get_temp::<f64>(id));
    if let Some(left) = at
        .map(|at| NUDGE_SECS - (now - at))
        .filter(|left| *left > 0.0)
    {
        let stroke = egui::Stroke::new(2.0, theme::palette(ctx).focus);
        ctx.layer_painter(dialog.layer_id).rect_stroke(
            dialog.rect,
            ctx.global_style().visuals.menu_corner_radius,
            stroke,
            egui::StrokeKind::Outside,
        );
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
    }
    !modal.backdrop_response.clicked() && modal.should_close()
}

/// Whether `row` is the server named `name` at `target`.
fn lands_at(row: &ServerRow, name: &str, target: &Target) -> bool {
    row.name == name
        && match target {
            Target::User => row.scope == Scope::User,
            Target::Local(p) => {
                row.scope == Scope::Local
                    && row.project.as_deref().is_some_and(|r| same_project(r, p))
            }
        }
}

/// Said in the Move and Fix casing dialogs when the server they change has an unsaved edit.
const GOES_ALONG: &str = "Your unsaved edit goes with it.";

/// How long a dialog's outline flashes after a click beside it.
const NUDGE_SECS: f64 = 0.2;

/// Whether the dialog `id` (the id its `Modal` was made with) flashed its outline after a click beside it.
#[cfg(test)]
fn nudged(ctx: &egui::Context, id: egui::Id) -> bool {
    ctx.data(|d| d.get_temp::<f64>(id.with("nudged_at")))
        .is_some()
}

/// Why `name` cannot be a server's name, as a sentence; `None` when it can.
fn name_problem(name: &str) -> Option<String> {
    ServerName::parse(name)
        .err()
        .map(|e| capitalized(&e.to_string()))
}

/// `a`, `a and b`, `a, b and c`; more than three are counted: `5 projects`.
fn list_in_words(items: &[&str], noun: &str) -> String {
    if items.len() <= 3 {
        words::and_list(items)
    } else {
        words::count(items.len(), noun)
    }
}

/// Whether the list shows `s` for the search `q` (trimmed and lower case; empty shows everything).
fn matches_search(s: &ServerRow, q: &str) -> bool {
    q.is_empty()
        || s.name.to_lowercase().contains(q)
        || s.target.to_lowercase().contains(q)
        || s.project
            .as_deref()
            .is_some_and(|p| p.to_lowercase().contains(q))
}

/// `a, b and c`; past eight names, `a, b, … and 12 more`.
fn names_in_a_line(names: &[&str]) -> String {
    const SHOWN: usize = 8;
    words::and_list_upto(names, SHOWN, SHOWN)
}

/// Names joined with commas; past six, the first five and how many more, so a long list stays readable. Core's
/// `ops::some_of` cuts the review's summary at the same count; keep the two alike.
fn some_names(names: &[String]) -> String {
    const NAMED: usize = 5;
    words::comma_list_upto(names, NAMED + 1, NAMED)
}

/// The last part of a project path: `C:/work/app` -> `app`. Unlike core's `words::folder_name`, a root gives an empty
/// name, as the app has always shown it (the owner's call, 2026-10-21).
pub(crate) fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) => &trimmed[cut + 1..],
        None => trimmed,
    }
}

/// Names `response` for assistive technology where what it shows, an icon or a hint in a box, is no name (WCAG 2.2
/// SC 4.1.2). Only the accessibility tree changes, nothing drawn.
fn name_for_screen_readers(response: &egui::Response, name: &str) {
    response
        .ctx
        .accesskit_node_builder(response.id, |node| node.set_label(name));
}

/// A small icon button that reads as `label` to assistive technology.
fn icon_button(ui: &mut Ui, glyph: &str, label: &str) -> bool {
    let response = ui.small_button(glyph).on_hover_text(label);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.clicked()
}

fn transport_name(t: TransportKind) -> &'static str {
    match t {
        TransportKind::Stdio => "Command (stdio)",
        TransportKind::Http => "HTTP",
        TransportKind::Sse => "SSE (older servers)",
        TransportKind::Ws => "WebSocket",
        TransportKind::ClaudeAi => "claude.ai",
        TransportKind::Invalid => "Unknown",
    }
}

/// The row and the `live_gen` an explanation was built for.
type ExplainedFor = (usize, u64);

/// The engine's latest answer for one row.
#[derive(Clone)]
struct Live {
    status: Status,
    detail: Option<String>,
    hint: Option<Hint>,
    timing: Option<Timing>,
}

/// A server's last starts as last read, and whether they have been asked for since they could have changed (a check
/// ended: it is a start too; the files changed; the section was shown again). Kept while read again, so the section
/// does not blink.
#[derive(Default)]
struct LastStartsSeen {
    starts: Option<LastStarts>,
    asked: bool,
    /// The frame the section was last drawn in.
    drawn: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::editor::EditorState;
    use crate::shell::list::{BLOCKED_BY_POLICY, TURNED_OFF};
    use crate::shell::list::{NOT_LOADED, PLUGIN_STATUS, in_projects_text};
    use crate::shell::places::Places;
    use crate::shell::toolbar::Tool;
    use crate::theme::ThemeOption;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use klefki_core::api::{Definition, LegacyDisabled, fixture_snapshot};

    /// Mirrors the app: the theme is applied once in the creation callback, before the first frame.
    fn harness_with(snapshot: Snapshot) -> Harness<'static, ShellApp> {
        harness_themed(snapshot, ThemeChoice::Dark)
    }

    fn harness_themed(snapshot: Snapshot, choice: ThemeChoice) -> Harness<'static, ShellApp> {
        // The harness sets its own theme once the app is built, so tell it the one we want.
        let harness_theme = match choice {
            ThemeChoice::Light => egui::Theme::Light,
            ThemeChoice::Dark | ThemeChoice::System => egui::Theme::Dark,
        };
        Harness::builder()
            .with_theme(harness_theme)
            // Tall enough for every fixture row: the grid only renders rows that are in view.
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(|cc| {
                crate::theme::apply(&cc.egui_ctx, choice);
                ShellApp::new(Arc::new(snapshot), choice)
            })
    }

    fn harness() -> Harness<'static, ShellApp> {
        harness_with((*fixture_snapshot()).clone())
    }

    #[test]
    fn navigation_switches_pages() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Secrets").click();
        h.run();
        assert_eq!(h.state().page, Page::Secrets);
        h.get_by_label_contains("Connections").click();
        h.run();
        assert_eq!(h.state().page, Page::Connections);
    }

    #[test]
    fn what_only_an_administrator_can_change_is_said_quietly() {
        let note = "Claude Code ignores the server \"run\" in /etc/claude-code/managed-settings.json: it names a \
                    program to run. Your administrator can change this.";
        let mut h = harness_with(Snapshot {
            organisation_notes: vec![note.to_owned()],
            ..Snapshot::default()
        });
        h.run();
        // One muted line, not the warning frame: the user cannot act on it (cross-review of #47).
        h.get_by_label_contains("Claude Code ignores the server \"run\"");
        assert!(h.query_by_label_contains(icon::WARNING).is_none());
    }

    #[test]
    fn clicking_a_row_selects_it_and_shows_detail() {
        let mut h = harness();
        h.run();
        click(&mut h, "issue-tracker");
        assert_eq!(h.state().selected.len(), 1);
        // Once in the grid and once in the detail panel.
        assert_eq!(h.get_all_by_label("https://it.example.com/mcp").count(), 2);
    }

    #[test]
    fn config_warnings_are_visible() {
        let mut h = harness_with(Snapshot {
            warnings: vec!["Could not read the config".to_owned()],
            ..Snapshot::default()
        });
        h.run();
        h.get_by_label_contains("Could not read the config");
    }

    #[test]
    fn many_warnings_are_summarised() {
        let mut h = harness_with(Snapshot {
            warnings: (0..10).map(|i| format!("problem number {i}")).collect(),
            ..Snapshot::default()
        });
        h.run();
        h.get_by_label_contains("problem number 3");
        h.get_by_label_contains("and 6 more");
        assert!(h.query_by_label_contains("problem number 9").is_none());
    }

    #[test]
    fn sanitised_fixture_text_reaches_the_ui() {
        let mut h = harness();
        h.run();
        // The fixture target contains ANSI escapes; the UI must only see the clean text.
        h.get_by_label("missing command");
    }

    #[test]
    fn light_theme_renders_the_whole_shell() {
        let mut h = harness_themed((*fixture_snapshot()).clone(), ThemeChoice::Light);
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
        click(&mut h, "issue-tracker");
        h.get_by_label_contains("Secrets");
        assert_eq!(h.get_all_by_label("https://it.example.com/mcp").count(), 2);
    }

    #[test]
    fn detail_tabs_do_not_change_size_on_hover() {
        let mut h = harness();
        h.run();
        click(&mut h, "issue-tracker");
        // (The Env tab was called "Headers/Env" when this test was written.)
        for tab in ["Auth", "Env", "General"] {
            let before = h.get_by_label(tab).rect();
            h.get_by_label(tab).hover();
            h.run();
            let after = h.get_by_label(tab).rect();
            assert_eq!(before, after, "tab {tab} moved or resized on hover");
        }
    }

    // ---- live status: the UI only reacts to events from the engine and sends commands back ----

    fn live_row(name: &str, scope: Scope, project: Option<&str>) -> ServerRow {
        ServerRow {
            connection: klefki_core::api::ConnectionId::new(scope, project, name),
            name: name.to_owned(),
            scope,
            project: project.map(str::to_owned),
            transport: klefki_core::api::TransportKind::Stdio,
            target: "npx something".to_owned(),
            status: Status::Unknown,
            enabled: true,
            disabled_in: 0,
            disabled_projects: Vec::new(),
            approval: None,
            shadowed: false,
            shadowed_in: Vec::new(),
            casing_collision: false,
            relative_paths: Vec::new(),
            preflight: Vec::new(),
            status_cached: false,
            oauth: None,
            plugin: None,
            managed: None,
            not_loaded: None,
            blocked: None,
            may_not_load: None,
        }
    }

    fn snapshot_of(rows: Vec<ServerRow>) -> klefki_core::api::Event {
        klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows,
            ..Snapshot::default()
        }))
    }

    fn live_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        live_harness_from(Harness::builder())
    }

    /// Sends the engine's snapshot of `rows` and lets the frame settle.
    fn show_rows(
        h: &mut Harness<'static, ShellApp>,
        engine: &klefki_core::api::Detached,
        rows: Vec<ServerRow>,
    ) {
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
    }

    /// Clicks what is named `label`, exactly, and lets the frame settle.
    fn click(h: &mut Harness<'static, ShellApp>, label: &str) {
        h.get_by_label(label).click();
        h.run();
    }

    /// The toolbar's button for `tool`, found by its name as a screen reader hears it.
    fn tool<'h>(h: &'h Harness<'static, ShellApp>, tool: Tool) -> egui_kittest::Node<'h> {
        h.get_by_role_and_label(egui::accesskit::Role::Button, tool.name())
    }

    /// Presses the toolbar's button for `tool` and lets the frame settle.
    fn press(h: &mut Harness<'static, ShellApp>, which: Tool) {
        tool(h, which).click();
        h.run();
    }

    /// Frames 1/60 s apart, as on a screen, so a delay is seen as it is and not skipped in quarter seconds.
    fn live_harness_at_60fps() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        live_harness_from(Harness::builder().with_step_dt(1.0 / 60.0))
    }

    fn live_harness_from(
        builder: egui_kittest::HarnessBuilder<ShellApp>,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (core, engine) = klefki_core::api::Core::detached();
        let h = builder
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(move |cc| {
                crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
                ShellApp::with_core(core, ThemeChoice::Dark)
            });
        (h, engine)
    }

    fn status_event(
        row: &ServerRow,
        status: Status,
        detail: Option<&str>,
    ) -> klefki_core::api::Event {
        klefki_core::api::Event::Status {
            id: row.id(),
            status,
            detail: detail.map(str::to_owned),
            hint: None,
            timing: None,
        }
    }

    fn unknown_host_event(row: &ServerRow) -> klefki_core::api::Event {
        klefki_core::api::Event::Status {
            id: row.id(),
            status: Status::Failed,
            detail: Some("ENOTFOUND: getaddrinfo ENOTFOUND acme-prod.example.com".to_owned()),
            hint: Some(klefki_core::api::Hint {
                title: "Can't reach the server's host".to_owned(),
                code: Some("ENOTFOUND".to_owned()),
                help: vec![
                    "You are probably offline or not connected to your VPN: check those first."
                        .to_owned(),
                ],
                points_at: Some((
                    klefki_core::api::Focus::Host,
                    "this name could not be looked up".to_owned(),
                )),
            }),
            timing: None,
        }
    }

    #[test]
    fn the_status_tab_explains_a_failure_in_plain_words_and_keeps_claudes() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        click(&mut h, "acme-prod");
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
        h.get_by_label_contains("not connected to your VPN");
        h.get_by_label_contains("getaddrinfo ENOTFOUND acme-prod.example.com");
    }

    #[test]
    fn the_explanation_follows_each_new_status() {
        // It is built once per status, not every frame; a new status must still replace it.
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        click(&mut h, "acme-prod");
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Can't reach").is_none());
        engine
            .events
            .send(status_event(&row, Status::Failed, Some("boom")))
            .unwrap();
        h.run();
        h.get_by_label_contains("could not connect");
        h.get_by_label_contains("Claude Code said: boom");
    }

    #[test]
    fn a_refresh_keeps_the_selection_on_the_same_server_when_two_look_alike() {
        // Review of the fix: the selection was carried over by the shown name. With "ab" and a server whose name
        // hides a right-to-left override (also shown "ab"), a refresh moved it to the other one, and Delete would
        // have deleted the server not selected.
        let (mut h, engine) = live_harness();
        let ab = live_row("ab", Scope::User, None);
        let mut lookalike = live_row("ab", Scope::User, None);
        lookalike.connection = klefki_core::api::ConnectionId::new(Scope::User, None, "a\u{202e}b");
        let rows = vec![ab.clone(), lookalike.clone()];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        engine
            .events
            .send(status_event(&ab, Status::Connected, None))
            .unwrap();
        h.run();
        h.state_mut().selected.insert(0);
        show_rows(&mut h, &engine, rows);
        assert_eq!(h.state().selected.iter().copied().collect::<Vec<_>>(), [0]);
        assert_eq!(
            h.state().effective(0).0,
            Status::Connected,
            "its status stays with it"
        );
        assert_ne!(h.state().effective(1).0, Status::Connected);
        // And the change asked for is about the selected one.
        h.key_press(egui::Key::Delete);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: ab.id() }])]
        );
        assert_ne!(ab.id(), lookalike.id());
    }

    #[test]
    fn the_explanation_survives_a_refresh_of_the_configuration() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        let other = live_row("other", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        // A new snapshot, with the row at a different position, carries live results over by identity.
        show_rows(&mut h, &engine, vec![other, row.clone()]);
        click(&mut h, "acme-prod");
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
    }

    #[test]
    fn the_explanation_follows_a_check_a_new_snapshot_cut_off() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut row = live_row("acme-prod", Scope::User, None);
        row.status = Status::Failed;
        row.status_cached = true;
        show_rows(&mut h, &engine, vec![row.clone()]);
        click(&mut h, "acme-prod");
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Claude Code could not connect to this server");
        // Being checked: nothing to explain yet.
        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        assert!(
            h.query_by_label_contains("Claude Code could not connect")
                .is_none()
        );
        // A new snapshot drops the check; what Claude's cache says is explained again.
        show_rows(&mut h, &engine, vec![row]);
        h.get_by_label_contains("Claude Code could not connect to this server");
    }

    #[test]
    fn hovering_a_failed_status_explains_it() {
        let (mut h, engine) = live_harness();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        h.get_by_label_contains(Status::Failed.label()).hover();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
        h.get_by_label_contains("getaddrinfo ENOTFOUND acme-prod.example.com");
    }

    #[test]
    fn a_failed_status_explains_itself_at_once_even_while_the_mouse_moves() {
        let (mut h, engine) = live_harness_at_60fps();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        let at = h
            .get_by_label_contains(Status::Failed.label())
            .rect()
            .center();
        // A hand on a mouse is rarely still: drift a pixel a frame, for a fifth of a second.
        for dx in 0..12 {
            h.hover_at(at + egui::vec2(dx as f32, 0.0));
            h.step();
        }
        h.get_by_label_contains("Can't reach the server's host");
    }

    #[test]
    fn the_whole_status_cell_explains_not_only_its_text() {
        let (mut h, engine) = live_harness_at_60fps();
        let row = live_row("acme-prod", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine.events.send(unknown_host_event(&row)).unwrap();
        h.run();
        let text = h.get_by_label_contains(Status::Failed.label()).rect();
        h.hover_at(egui::pos2(text.right() + 60.0, text.center().y));
        for _ in 0..12 {
            h.step();
        }
        h.get_by_label_contains("Can't reach the server's host");
        // And it goes when the pointer leaves the cell.
        h.hover_at(egui::pos2(text.right() + 60.0, text.center().y + 300.0));
        for _ in 0..12 {
            h.step();
        }
        assert!(
            h.query_by_label_contains("Can't reach the server's host")
                .is_none()
        );
    }

    #[test]
    fn a_snapshot_event_fills_the_grid_and_status_events_update_it_live() {
        let (mut h, engine) = live_harness();
        h.run();
        let row = live_row("azure-devops", Scope::User, None);
        show_rows(&mut h, &engine, vec![row.clone()]);
        h.get_by_label("azure-devops");
        h.get_by_label_contains("Not checked");

        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Checking");
        assert!(h.query_by_label_contains("Not checked").is_none());

        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Connected");
        assert!(h.query_by_label_contains("Checking").is_none());
    }

    #[test]
    fn a_status_from_claudes_cache_says_it_is_last_known_until_its_check_answers() {
        // Bug: a cached "Connected" looked like a fresh answer, though the cache can be days old.
        let (mut h, engine) = live_harness();
        let mut row = live_row("known", Scope::User, None);
        row.status = Status::Connected;
        row.status_cached = true;
        show_rows(&mut h, &engine, vec![row.clone()]);
        h.get_by_label("\u{25CB} Connected");
        h.state_mut().selected.insert(0);
        h.state_mut().tab = DetailTab::Status;
        h.run();
        h.get_by_label("Connected (last known)");

        engine
            .events
            .send(status_event(&row, Status::Failed, Some("boom")))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("last known").is_none());
        assert!(h.query_by_label_contains("\u{25CB}").is_none());
        h.get_by_label("\u{25CF} Failed");
    }

    #[test]
    fn only_statuses_from_claudes_cache_are_marked_last_known() {
        let (mut h, engine) = live_harness();
        let mut rows = Vec::new();
        for (name, status, cached) in [
            ("auth", Status::NeedsAuth, true),
            ("bad", Status::Failed, true),
            // Claude wrote it minutes ago: as good as a check.
            ("recent", Status::NeedsAuth, false),
            ("new", Status::Unknown, false),
            ("waiting", Status::PendingApproval, false),
        ] {
            let mut row = live_row(name, Scope::User, None);
            row.status = status;
            row.status_cached = cached;
            rows.push(row);
        }
        show_rows(&mut h, &engine, rows);
        assert_eq!(h.query_all_by_label("\u{25CB} Needs auth").count(), 1);
        assert_eq!(h.query_all_by_label("\u{25CF} Needs auth").count(), 1);
        h.get_by_label("\u{25CB} Failed");
        h.get_by_label("\u{25CF} Not checked");
        h.get_by_label("\u{25CF} Pending approval");
    }

    #[test]
    fn the_engine_is_told_which_servers_are_on_screen_when_that_changes() {
        let (mut h, engine) = live_harness();
        let rows: Vec<ServerRow> = (0..60)
            .map(|i| live_row(&format!("s{i:02}"), Scope::User, None))
            .collect();
        show_rows(&mut h, &engine, rows.clone());
        let told = visible_sent(&engine);
        assert_eq!(told.len(), 1, "once, not every frame: {told:?}");
        let shown = &told[0];
        assert!(
            !shown.is_empty() && shown.len() < rows.len(),
            "only those in view: {}",
            shown.len()
        );
        assert_eq!(shown[0], rows[0].connection);
        h.run();
        assert!(
            visible_sent(&engine).is_empty(),
            "nothing changed, nothing sent"
        );
        // A search changes what is in view.
        h.state_mut().search = "s59".to_owned();
        h.run();
        assert_eq!(visible_sent(&engine), [vec![rows[59].connection.clone()]]);
    }

    #[test]
    fn every_status_has_its_own_readable_label() {
        let (mut h, engine) = live_harness();
        let statuses = [
            Status::Connected,
            Status::Degraded,
            Status::NeedsAuth,
            Status::Failed,
            Status::PendingApproval,
            Status::NotConfigured,
            Status::Checking,
            Status::Unknown,
        ];
        let labels: std::collections::BTreeSet<_> = statuses.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels.len(),
            statuses.len(),
            "every status reads differently"
        );
        let rows: Vec<ServerRow> = (0..statuses.len())
            .map(|i| live_row(&format!("srv{i}"), Scope::User, None))
            .collect();
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        for (row, status) in rows.iter().zip(statuses) {
            engine.events.send(status_event(row, status, None)).unwrap();
        }
        h.run();
        for status in statuses {
            h.get_by_label(&format!("\u{25CF} {}", status.label()));
        }
    }

    #[test]
    fn a_new_snapshot_clears_checks_that_were_cut_off() {
        let (mut h, engine) = live_harness();
        let row = live_row("azure-devops", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Checking");
        // A refresh replaced the world; the running check's answer will be dropped by the engine.
        show_rows(&mut h, &engine, vec![row.clone()]);
        assert!(h.query_by_label_contains("Checking").is_none());
        h.get_by_label_contains("Not checked");
    }

    #[test]
    fn results_for_rows_that_are_gone_do_not_linger_or_crash() {
        let (mut h, engine) = live_harness();
        let gone = live_row("gone", Scope::User, None);
        let stays = live_row("stays", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![gone.clone(), stays.clone()]))
            .unwrap();
        engine
            .events
            .send(status_event(&gone, Status::Failed, Some("x")))
            .unwrap();
        engine
            .events
            .send(snapshot_of(vec![stays.clone()]))
            .unwrap();
        engine
            .events
            .send(status_event(&gone, Status::Failed, Some("late")))
            .unwrap();
        h.run();
        assert!(h.query_by_label("gone").is_none());
        h.get_by_label("stays");
    }

    #[test]
    fn the_cli_version_and_problems_are_shown() {
        let (mut h, engine) = live_harness();
        h.run();
        assert!(h.query_by_label_contains("Claude CLI").is_none());
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.302".to_owned(),
            )))
            .unwrap();
        h.run();
        h.get_by_label_contains("Claude CLI 2.1.302");
        engine
            .events
            .send(klefki_core::api::Event::CliProblem(
                "Live status is unavailable: nope.".to_owned(),
            ))
            .unwrap();
        h.run();
        h.get_by_label_contains("Live status is unavailable");
    }

    #[test]
    fn a_version_is_not_shown_once_claude_code_is_no_longer_found() {
        // From the review of the warning fix: the reverse case. Claude Code removed and F5 pressed, and the top bar
        // still named the version beside "not found".
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.303".to_owned(),
            )))
            .unwrap();
        h.run();
        h.get_by_label_contains("Claude CLI 2.1.303");
        engine
            .events
            .send(klefki_core::api::Event::CliProblem(
                "Live status is unavailable: the Claude CLI was not found.".to_owned(),
            ))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Claude CLI 2.1.303").is_none());
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.304".to_owned(),
            )))
            .unwrap();
        h.run();
        h.get_by_label_contains("Claude CLI 2.1.304");
    }

    #[test]
    fn the_live_status_warning_goes_once_claude_code_is_found() {
        // Found by the core review: Claude Code installed and F5 pressed, checks work again, and the warning stayed.
        // The engine names the version (or `None` when it cannot tell) on every refresh that finds the program.
        for version in [Some("2.1.303".to_owned()), None] {
            let (mut h, engine) = live_harness();
            engine
                .events
                .send(klefki_core::api::Event::CliProblem(
                    "Live status is unavailable: claude was not found.".to_owned(),
                ))
                .unwrap();
            h.run();
            h.get_by_label_contains("Live status is unavailable");
            engine
                .events
                .send(klefki_core::api::Event::CliVersion(version.clone()))
                .unwrap();
            h.run();
            assert!(
                h.query_by_label_contains("Live status is unavailable")
                    .is_none(),
                "still warned after {version:?}"
            );
        }
    }

    #[test]
    fn the_status_tab_shows_the_detail_and_check_now_asks_the_engine() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(
                &row,
                Status::Failed,
                Some("CONNECTION_CLOSED: Connection closed"),
            ))
            .unwrap();
        h.run();
        click(&mut h, "docs");
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        // Its last starts are read from Claude Code's logs, which runs nothing.
        assert_eq!(drain(&engine), vec![Command::LastStarts(row.id())]);
        h.get_by_label_contains("CONNECTION_CLOSED: Connection closed");
        // A project server's check runs the repository's own command, and the UI says so.
        h.get_by_label_contains("runs this server's command");
        h.get_by_label_contains("Check now").click();
        h.run();
        let sent = next_command(&engine).expect("a command was sent");
        assert_eq!(sent, klefki_core::api::Command::Check(row.id()));
        assert!(next_command(&engine).is_none(), "exactly one command");
    }

    #[test]
    fn check_now_does_not_warn_about_commands_for_remote_servers() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.transport = klefki_core::api::TransportKind::Http;
        show_rows(&mut h, &engine, vec![row]);
        click(&mut h, "gateway");
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        assert!(
            h.query_by_label_contains("runs this server's command")
                .is_none()
        );
        h.get_by_label_contains("Check now");
    }

    #[test]
    fn the_refresh_button_and_f5_ask_for_a_full_recheck() {
        let (mut h, engine) = live_harness();
        h.run();
        click(&mut h, "Refresh and re-check all");
        assert_eq!(
            engine.commands.try_recv().unwrap(),
            klefki_core::api::Command::Refresh { live_all: true }
        );
        h.key_press(egui::Key::F5);
        h.run();
        assert_eq!(
            engine.commands.try_recv().unwrap(),
            klefki_core::api::Command::Refresh { live_all: true }
        );
        assert!(engine.commands.try_recv().is_err());
    }

    #[test]
    fn disabled_rows_still_say_disabled_whatever_the_engine_reports() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("off", Scope::User, None);
        row.enabled = false;
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        h.get_by_label_contains("Disabled");
        assert!(h.query_by_label_contains("Connected").is_none());
    }

    // ---- Diagnose: the explanation the engine builds is shown, and asked for, from the Status tab ----

    fn diagnosis(
        status: Status,
        hints: &[(&str, &str)],
        said: &[&str],
        log: &[&str],
        note: Option<&str>,
    ) -> Diagnosis {
        Diagnosis {
            status,
            detail: Some("CONNECTION_CLOSED: Connection closed".to_owned()),
            hints: hints
                .iter()
                .map(|(t, a)| klefki_core::api::Hint {
                    title: (*t).to_owned(),
                    code: None,
                    help: vec![(*a).to_owned()],
                    points_at: None,
                })
                .collect(),
            server_said: said.iter().map(|l| (*l).to_owned()).collect(),
            client_log: log.iter().map(|l| (*l).to_owned()).collect(),
            note: note.map(str::to_owned),
        }
    }

    fn open_status_tab(h: &mut Harness<'static, ShellApp>, row_name: &str) {
        use egui::accesskit::Role;
        h.get_by_label(row_name).click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
    }

    /// Both sizes the detail panel must work at: the one Klefki opens at, and the smallest it can be made (which is
    /// also the room the opening size gives at 150 % zoom).
    const SIZES: [egui::Vec2; 2] = [OPENING_SIZE, SMALLEST_SIZE];

    /// A command with two arguments and a variable: three fields on General, two on Env.
    const SHORT_DEFINITION: &str = "{\n  \"command\": \"node\",\n  \"args\": [\"server.js\", \"--port=3000\"],\n  \"env\": {\"LOG\": \"1\"}\n}";

    /// A way to ask for the selected server's definition.
    type Ask = fn(&mut Harness<'static, ShellApp>);

    // ---- long text must wrap inside the detail panel, never resize it ----

    /// Where the detail panel's contents start. If the panel grows, this moves left.
    fn detail_left_edge(h: &Harness<'static, ShellApp>, label: &str) -> f32 {
        h.get_by_label(label).rect().min.x
    }

    // ---- switching servers on and off

    fn plan_view(id: u64, lines: &[&str], warnings: &[&str], nothing_to_do: bool) -> PlanView {
        PlanView {
            id,
            files: vec![klefki_core::api::FileDiff {
                path: ".claude.json".to_owned(),
                lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            }],
            warnings: warnings.iter().map(|w| (*w).to_owned()).collect(),
            nothing_to_do,
            already: Vec::new(),
            others_on: false,
            reviews: Vec::new(),
            summary: Vec::new(),
            action: String::new(),
            destructive: false,
            notes: Vec::new(),
        }
    }

    fn two_rows() -> (ServerRow, ServerRow) {
        (
            live_row("alpha", Scope::User, None),
            live_row("beta", Scope::Local, Some("C:/Repos/WebApp")),
        )
    }

    /// The commands sent since the last drain, without [`Command::Visible`]: the grid sends that by itself whenever
    /// the rows in view change, and it asks for nothing (see `visible_sent`).
    fn drain(engine: &klefki_core::api::Detached) -> Vec<Command> {
        drain_all(engine)
            .into_iter()
            .filter(|c| !matches!(c, Command::Visible(_)))
            .collect()
    }

    /// The next command other than [`Command::Visible`].
    fn next_command(engine: &klefki_core::api::Detached) -> Option<Command> {
        std::iter::from_fn(|| engine.commands.try_recv().ok())
            .find(|c| !matches!(c, Command::Visible(_)))
    }

    fn drain_all(engine: &klefki_core::api::Detached) -> Vec<Command> {
        let mut out = Vec::new();
        while let Ok(c) = engine.commands.try_recv() {
            out.push(c);
        }
        out
    }

    fn visible_sent(
        engine: &klefki_core::api::Detached,
    ) -> Vec<Vec<klefki_core::api::ConnectionId>> {
        drain_all(engine)
            .into_iter()
            .filter_map(|c| match c {
                Command::Visible(v) => Some(v),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn enable_and_disable_need_a_selection() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a]);
        press(&mut h, Tool::TurnOff);
        assert!(drain(&engine).is_empty(), "nothing selected, nothing asked");
    }

    #[test]
    fn disable_asks_the_engine_for_a_plan_for_each_selected_server() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone(), b.clone()]);
        click(&mut h, "alpha");
        press(&mut h, Tool::TurnOff);
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: a.id(),
                enabled: false
            }])]
        );
    }

    #[test]
    fn enable_asks_for_the_opposite() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone()]);
        click(&mut h, "alpha");
        press(&mut h, Tool::TurnOn);
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: a.id(),
                enabled: true
            }])]
        );
    }

    fn with_plan(
        lines: &[&str],
        warnings: &[&str],
        nothing: bool,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        engine
            .events
            .send(Event::PlanReady(plan_view(7, lines, warnings, nothing)))
            .unwrap();
        h.run();
        (h, engine)
    }

    #[test]
    fn warnings_come_before_what_each_server_would_run() {
        // Cross-review of the restore, U1: a reverted token was only said after every "would run" card.
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        let mut plan = plan_view(7, &["+ alpha"], &["alpha: the token goes back"], false);
        plan.reviews = (0..4)
            .map(|i| klefki_core::api::Review {
                title: format!("server{i} would run:"),
                lines: vec!["Command: x".to_owned()],
            })
            .collect();
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        let warning = h.get_by_label_contains("alpha: the token goes back").rect();
        let card = h.get_by_label("server0 would run:").rect();
        assert!(warning.bottom() <= card.top(), "{warning:?} {card:?}");
    }

    #[test]
    fn a_long_diff_lays_out_only_the_lines_in_view_and_scrolls_to_its_end() {
        // Performance review: every line of a diff was laid out on every frame (22 ms a frame at 16,000 lines).
        let lines: Vec<String> = (0..500)
            .map(|i| {
                if i == 250 {
                    // Wraps over several lines: the lines after it must still land where they are drawn.
                    format!("+ line {i} {}", "wide ".repeat(60))
                } else {
                    format!("+ line {i}")
                }
            })
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut h, _engine) = with_plan(&refs, &[], false);
        // Cross-review: with only the lines in view laid out, the heading says how many there are.
        h.get_by_label("Changes in .claude.json · 500 lines");
        let first = h.get_by_label("+ line 0").rect();
        assert!(
            h.query_by_label("+ line 499").is_none(),
            "laid out though not in view"
        );
        assert!(
            h.query_by_label("+ line 300").is_none(),
            "laid out though not in view"
        );
        let over = first.center();
        for _ in 0..200 {
            if h.query_by_label("+ line 499").is_some() {
                break;
            }
            h.event(egui::Event::PointerMoved(over));
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -400.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            for _ in 0..5 {
                h.step();
            }
        }
        let last = h.get_by_label("+ line 499").rect();
        let before = h.get_by_label("+ line 498").rect();
        assert!(
            h.query_by_label("+ line 0").is_none(),
            "laid out though scrolled away"
        );
        assert!(
            (last.top() - before.bottom()) < 10.0 && last.top() > before.top(),
            "the last lines overlap or have a gap: {before:?} {last:?}"
        );
        // The end of the diff is reachable: its last line is inside the box, not cut off below it.
        assert!(
            last.bottom() <= first.top() + 220.0 + 1.0,
            "{first:?} {last:?}"
        );
        // Where the lines went is let go with the plan.
        assert!(!h.state().diff_tops.files.is_empty());
        h.state_mut().plan = None;
        h.run();
        assert!(h.state().diff_tops.files.is_empty());
    }

    #[test]
    fn two_diffs_of_one_file_scroll_on_their_own() {
        // Found trying Import: a plan that adds one server by writing the file and another through Claude Code's
        // own command has two diffs of .claude.json. Both boxes had one scroll position, so scrolling one moved the
        // other (and egui flagged the clash).
        let (mut h, engine) = live_harness();
        engine.events.send(snapshot_of(vec![two_rows().0])).unwrap();
        let mut plan = plan_view(7, &[], &[], false);
        plan.files = ["a", "b"]
            .iter()
            .map(|which| klefki_core::api::FileDiff {
                path: ".claude.json".to_owned(),
                lines: (0..60).map(|i| format!("+ {which}{i}")).collect(),
            })
            .collect();
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        let over = h.get_by_label("+ a0").rect().center();
        h.get_by_label("+ b0");
        for _ in 0..3 {
            h.event(egui::Event::PointerMoved(over));
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -400.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            for _ in 0..5 {
                h.step();
            }
        }
        assert!(
            h.query_by_label("+ a0").is_none(),
            "the first did not scroll"
        );
        assert!(
            h.query_by_label("+ b0").is_some(),
            "the second scrolled with it"
        );
    }

    #[test]
    fn the_plan_dialog_shows_exactly_what_will_change_and_nothing_is_sent_yet() {
        let (_h, engine) = {
            let (h, engine) = with_plan(
                &["  projects[\"C:/x\"].disabledMcpServers", "+ alpha"],
                &["A running Claude Code session may need a restart before it notices this."],
                false,
            );
            h.get_by_label_contains("Review the change");
            h.get_by_label_contains("disabledMcpServers");
            h.get_by_label("+ alpha");
            h.get_by_label("Changes in .claude.json · 2 lines");
            h.get_by_label_contains("may need a restart");
            (h, engine)
        };
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn apply_sends_the_plan_once_even_if_pressed_twice() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        click(&mut h, "Apply");
        click(&mut h, "Apply");
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
    }

    #[test]
    fn cancel_discards_the_plan_and_closes_the_dialog() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        click(&mut h, "Cancel");
        assert_eq!(drain(&engine), vec![Command::Discard(7)]);
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    #[test]
    fn escape_cancels_like_the_cancel_button() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(drain(&engine), vec![Command::Discard(7)]);
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    #[test]
    fn a_plan_that_changes_nothing_cannot_be_applied() {
        let (mut h, engine) = with_plan(&[], &[], true);
        h.get_by_label_contains("Nothing would be written");
        click(&mut h, "Apply");
        assert!(drain(&engine).is_empty());
    }

    /// The owner's report: "already looks like this" alone did not say which server, or where.
    #[test]
    fn a_plan_that_changes_nothing_says_what_is_already_so() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        let mut plan = plan_view(7, &[], &[], true);
        plan.already = vec!["alpha is already on in app.".to_owned()];
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label("alpha is already on in app.");
        h.get_by_label("Nothing would be written.");
        assert!(
            h.query_by_label_contains("already looks like this")
                .is_none()
        );
    }

    #[test]
    fn a_saved_change_closes_the_dialog_and_says_where_the_backup_is() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: Some("/data/backups/claude.json.1.bak".to_owned()),
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
        h.get_by_label_contains(
            "Saved. The previous file is kept in /data/backups/claude.json.1.bak",
        );
    }

    #[test]
    fn a_failed_change_closes_the_dialog_and_shows_the_reason() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 7,
                error: "Claude Code is writing its configuration right now.".to_owned(),
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
        h.get_by_label_contains("Claude Code is writing its configuration");
        click(&mut h, "Dismiss");
        assert!(
            h.query_by_label_contains("writing its configuration")
                .is_none()
        );
    }

    #[test]
    fn a_plan_that_could_not_be_made_shows_the_reason() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(Event::PlanFailed(
                "ghost is no longer in the configuration.".to_owned(),
            ))
            .unwrap();
        h.run();
        h.get_by_label_contains("ghost is no longer");
    }

    #[test]
    fn a_change_claude_overwrote_is_reported_after_the_dialog_is_gone() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: Some("b".to_owned()),
                notes: Vec::new(),
            })
            .unwrap();
        engine.events.send(Event::Reapplied { plan: 7 }).unwrap();
        h.run();
        h.get_by_label_contains("overwrote the change right after it was saved");
    }

    #[test]
    fn the_toggle_buttons_do_not_start_a_second_plan_while_one_is_open() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a]);
        click(&mut h, "alpha");
        engine
            .events
            .send(Event::PlanReady(plan_view(8, &["+ alpha"], &[], false)))
            .unwrap();
        h.run();
        press(&mut h, Tool::TurnOff);
        // A click outside the dialog closes it (and drops the plan); it never starts a second plan.
        let sent = drain(&engine);
        assert!(
            sent.iter().all(|c| !matches!(c, Command::Prepare(_))),
            "a second plan cannot start: {sent:?}"
        );
    }

    #[test]
    fn escape_does_not_close_the_dialog_while_the_change_is_being_saved() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        click(&mut h, "Apply");
        h.key_press(egui::Key::Escape);
        h.run();
        // The engine writes the plan whatever comes after Apply; saying it was dropped would be a lie.
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
        h.get_by_label_contains("Review the change");
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    #[test]
    fn cancel_does_nothing_while_the_change_is_being_saved() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        click(&mut h, "Apply");
        click(&mut h, "Cancel");
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
        h.get_by_label_contains("Review the change");
    }

    #[test]
    fn a_double_click_on_disable_asks_for_one_plan() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a]);
        click(&mut h, "alpha");
        // The engine has not answered the first click yet.
        press(&mut h, Tool::TurnOff);
        press(&mut h, Tool::TurnOff);
        let prepares = drain(&engine)
            .into_iter()
            .filter(|c| matches!(c, Command::Prepare(_)))
            .count();
        assert_eq!(prepares, 1);
    }

    #[test]
    fn the_toggle_works_again_once_a_plan_could_not_be_made() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a]);
        click(&mut h, "alpha");
        press(&mut h, Tool::TurnOff);
        engine
            .events
            .send(Event::PlanFailed("alpha is gone.".to_owned()))
            .unwrap();
        h.run();
        press(&mut h, Tool::TurnOff);
        let prepares = drain(&engine)
            .into_iter()
            .filter(|c| matches!(c, Command::Prepare(_)))
            .count();
        assert_eq!(prepares, 2);
    }

    #[test]
    fn a_plan_arriving_while_another_is_open_is_discarded_not_shown() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        click(&mut h, "Apply");
        engine
            .events
            .send(Event::PlanReady(plan_view(9, &["+ zeta"], &[], false)))
            .unwrap();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(7), Command::Discard(9)]);
        assert!(h.query_by_label("+ zeta").is_none());
        // Still saving plan 7: its Apply stays off and its answer closes the dialog.
        h.get_by_label("+ alpha");
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Review the change").is_none());
    }

    // ---- approving a project server

    fn project_row(name: &str, approval: Option<klefki_core::api::Approval>) -> ServerRow {
        let mut row = live_row(name, Scope::Project, Some("C:/Repos/WebApp"));
        row.approval = approval;
        row
    }

    fn open_general(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
    }

    #[test]
    fn a_pending_or_rejected_project_server_offers_review_and_approve() {
        use klefki_core::api::Approval::{Pending, Rejected};
        for approval in [Pending, Rejected] {
            let (mut h, engine) = live_harness();
            let row = project_row("docs", Some(approval));
            show_rows(&mut h, &engine, vec![row.clone()]);
            open_general(&mut h, "docs");
            h.get_by_label_contains("Review and approve").click();
            h.run();
            assert_eq!(
                drain(&engine),
                vec![Command::Prepare(vec![Change::Approve { id: row.id() }])]
            );
        }
    }

    #[test]
    fn a_double_click_on_review_and_approve_asks_for_one_plan() {
        let (mut h, engine) = live_harness();
        let row = project_row("docs", Some(klefki_core::api::Approval::Pending));
        show_rows(&mut h, &engine, vec![row.clone()]);
        open_general(&mut h, "docs");
        h.get_by_label_contains("Review and approve").click();
        h.run();
        h.get_by_label_contains("Review and approve").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Approve { id: row.id() }])]
        );
    }

    #[test]
    fn approved_servers_and_other_scopes_have_no_approve_button() {
        let (mut h, engine) = live_harness();
        let approved = project_row("done", Some(klefki_core::api::Approval::Approved));
        let user = live_row("mine", Scope::User, None);
        show_rows(&mut h, &engine, vec![approved, user]);
        for name in ["done", "mine"] {
            open_general(&mut h, name);
            assert!(
                h.query_by_label_contains("Review and approve").is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_approval_dialog_shows_what_will_run_before_anything_is_saved() {
        let (mut h, engine) = live_harness();
        let row = project_row("docs", Some(klefki_core::api::Approval::Pending));
        engine.events.send(snapshot_of(vec![row])).unwrap();
        let mut plan = plan_view(
            3,
            &["  enabledMcpjsonServers", "+ docs"],
            &["Only approve it if you trust the repository."],
            false,
        );
        plan.reviews = vec![klefki_core::api::Review {
            title: "docs would be allowed to run:".to_owned(),
            lines: vec![
                "Command: node".to_owned(),
                "Argument 1: server.js".to_owned(),
                "Environment: NODE_OPTIONS=--require ./hook.js".to_owned(),
            ],
        }];
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label_contains("docs would be allowed to run");
        h.get_by_label("Command: node");
        h.get_by_label("Argument 1: server.js");
        h.get_by_label_contains("NODE_OPTIONS=--require ./hook.js");
        h.get_by_label_contains("trust the repository");
        assert!(drain(&engine).is_empty());
        // The button says what it does, and sends the plan.
        click(&mut h, "Approve");
        assert_eq!(drain(&engine), vec![Command::Apply(3)]);
    }

    #[test]
    fn a_long_plan_scrolls_and_keeps_its_buttons_in_the_window() {
        // Several pasted servers make a plan taller than the window; Add and Cancel must stay reachable.
        let (core, engine) = klefki_core::api::Core::detached();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1200.0, 720.0))
            .build_eframe(move |cc| {
                crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
                ShellApp::with_core(core, ThemeChoice::Dark)
            });
        h.run();
        let lines: Vec<String> = (0..40).map(|i| format!("+   \"line {i}\"")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut plan = plan_view(4, &lines, &[], false);
        plan.action = "Add".to_owned();
        plan.summary = (0..6)
            .map(|i| format!("Add server{i} for the user"))
            .collect();
        plan.reviews = (0..6)
            .map(|i| klefki_core::api::Review {
                title: format!("server{i} would run:"),
                lines: (1..=5).map(|a| format!("Argument {a}: value")).collect(),
            })
            .collect();
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 720.0));
        for button in ["Add", "Cancel"] {
            let rect = h.get_by_label(button).rect();
            assert!(window.contains_rect(rect), "{button} at {rect:?}");
        }
        click(&mut h, "Add");
        assert_eq!(drain(&engine), vec![Command::Apply(4)]);
    }

    #[test]
    fn a_saved_approval_without_a_backup_just_says_saved() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        h.get_by_label("Saved.");
    }

    #[test]
    fn live_results_and_selection_follow_their_rows_when_a_snapshot_reorders_them() {
        let (mut h, engine) = live_harness();
        let a = live_row("alpha", Scope::User, None);
        let b = live_row("beta", Scope::User, None);
        let c = live_row("gamma", Scope::Local, Some("C:/Repos/WebApp"));
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        engine
            .events
            .send(status_event(&b, Status::Failed, Some("beta broke")))
            .unwrap();
        h.run();
        click(&mut h, "beta");
        // Same rows, another order, one more in front.
        show_rows(&mut h, &engine, vec![c, b.clone(), a.clone()]);
        assert_eq!(h.state().selected.len(), 1);
        let selected = *h.state().selected.iter().next().unwrap();
        assert_eq!(h.state().snapshot.servers[selected].name, "beta");
        assert_eq!(h.state().effective(selected).0, Status::Failed);
        let alpha = h
            .state()
            .snapshot
            .servers
            .iter()
            .position(|r| r.name == "alpha")
            .unwrap();
        assert_eq!(h.state().effective(alpha).0, Status::Unknown);
        h.get_by_label_contains(Status::Failed.label());
    }

    #[test]
    fn the_grid_follows_a_new_snapshot_and_a_new_search() {
        let (mut h, engine) = live_harness();
        show_rows(&mut h, &engine, vec![live_row("alpha", Scope::User, None)]);
        h.get_by_label("alpha");
        show_rows(
            &mut h,
            &engine,
            vec![
                live_row("alpha", Scope::User, None),
                live_row("omega", Scope::User, None),
            ],
        );
        h.get_by_label("omega");
        h.state_mut().search = "alp".to_owned();
        h.run();
        assert!(h.query_by_label("omega").is_none());
        h.state_mut().search.clear();
        h.run();
        h.get_by_label("omega");
    }

    // ---- delete and edit

    #[test]
    fn delete_asks_the_engine_for_a_plan_for_each_selected_server() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone(), b.clone()]);
        press(&mut h, Tool::Delete);
        assert!(drain(&engine).is_empty(), "nothing selected, nothing asked");
        click(&mut h, "alpha");
        press(&mut h, Tool::Delete);
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: a.id() }])]
        );
    }

    fn plugin_row(name: &str, same_as: Option<&str>) -> ServerRow {
        ServerRow {
            plugin: Some(klefki_core::api::PluginInfo {
                plugin: "docs".into(),
                marketplace: "team".into(),
                same_as: same_as.map(str::to_owned),
                written_target: None,
                off_in: Vec::new(),
            }),
            shadowed: same_as.is_some(),
            ..live_row(name, Scope::Plugin, None)
        }
    }

    #[test]
    fn a_plugins_server_is_switched_on_and_off_but_nothing_else_changes_checks_or_exports_it() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let p = plugin_row("plugin:docs:search", None);
        show_rows(&mut h, &engine, vec![p.clone()]);
        // Under its plugin, by its own name; the details give the name Claude Code uses.
        h.get_by_label("Plugin · docs, from team");
        click(&mut h, "search");
        drain(&engine);
        h.get_by_label("plugin:docs:search");
        h.get_by_label("docs, from team");
        h.get_by_label_contains("Claude Code reads this server from the plugin docs.");
        // No form to edit it in.
        assert!(h.query_by_label("Review and save…").is_none());
        for t in [Tool::TurnOn, Tool::TurnOff] {
            assert!(!tool(&h, t).accesskit_node().is_disabled(), "{t:?}");
        }
        for t in [
            Tool::Delete,
            Tool::Edit,
            Tool::Duplicate,
            Tool::Move,
            Tool::Export,
            Tool::SignIn,
        ] {
            assert!(tool(&h, t).accesskit_node().is_disabled(), "{t:?}");
        }
        // The list says where its status is.
        h.get_by_label("See /mcp");
        // The note saying so comes first, before anyone looks for Edit.
        let note = h
            .get_by_label_contains("Claude Code reads this server")
            .rect()
            .top();
        assert!(note < h.get_by_label("docs, from team").rect().top());
        // Nor through the keyboard or the palette; Del says why it does nothing.
        h.key_press(egui::Key::Delete);
        h.run();
        h.get_by_label_contains("plugin:docs:search comes from the plugin docs");
        h.key_press(egui::Key::Space);
        for key in [egui::Key::E, egui::Key::D] {
            h.key_press_modifiers(egui::Modifiers::COMMAND, key);
        }
        h.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::M,
        );
        h.run();
        let offered: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
        let about: Vec<&String> = offered
            .iter()
            .filter(|t| t.contains("plugin:docs:search") && !t.starts_with("Go to"))
            .collect();
        assert_eq!(
            about,
            ["Turn on plugin:docs:search", "Turn off plugin:docs:search"],
            "{offered:?}"
        );
        // The Status tab says where its status is; nothing to check it with.
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Klefki does not check a plugin's servers");
        assert!(h.query_by_label("Check now").is_none());
        assert!(h.query_by_label("Diagnose").is_none());
        // Only its last starts are read, from Claude Code's logs: nothing runs.
        assert_eq!(drain(&engine), vec![Command::LastStarts(p.id())]);
        // Switched like a user server, through a review.
        press(&mut h, Tool::TurnOff);
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: p.id(),
                enabled: false
            }])]
        );
    }

    #[test]
    fn a_plugins_server_claude_code_leaves_out_says_which_server_it_repeats() {
        let (mut h, engine) = live_harness();
        let user = live_row("events", Scope::User, None);
        let p = plugin_row("plugin:docs:events", Some("events"));
        show_rows(&mut h, &engine, vec![user, p]);
        h.get_by_label("left out");
        h.get_by_label("Plugin · docs, from team");
        h.get_all_by_label("events").nth(1).unwrap().click();
        h.run();
        h.get_by_label("Left out");
        h.get_by_label("in every project: events has the same command or address");
        // No tabs for values or sign-in: there is no definition here to show them from.
        assert!(h.query_by_label("Env").is_none());
        assert!(h.query_by_label("Auth").is_none());
    }

    #[test]
    fn a_plugin_off_in_some_projects_says_where() {
        let (mut h, engine) = live_harness();
        let mut p = plugin_row("plugin:docs:search", None);
        p.plugin.as_mut().unwrap().off_in = vec!["/home/me/work/app".into(), "/home/me/web".into()];
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![p],
                project_count: 3,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        h.get_by_label("plugin off in 2/3");
        click(&mut h, "search");
        h.get_by_label("Plugin on");
        h.get_by_label("in 1 of 3 projects; off in app, web");
        h.get_by_label("yes");
    }

    #[test]
    fn a_plugin_off_in_every_project_reads_so() {
        // Cross-review of #46: it read "Enabled: yes" like a server that loads.
        let (mut h, engine) = live_harness();
        let mut p = plugin_row("plugin:docs:search", None);
        p.plugin.as_mut().unwrap().off_in = vec!["/home/me/work/app".into(), "/home/me/web".into()];
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![p],
                project_count: 2,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        h.get_by_label("plugin off in 2/2");
        click(&mut h, "search");
        h.get_by_label("yes, but its plugin is off in every project");
    }

    fn organisation_row(name: &str) -> ServerRow {
        ServerRow {
            managed: Some("/etc/claude-code/managed-settings.json".into()),
            ..live_row(name, Scope::Managed, None)
        }
    }

    #[test]
    fn a_claude_ai_connector_is_read_only_says_where_it_comes_from_and_switches() {
        let (mut h, engine) = live_harness();
        let slack = ServerRow {
            transport: TransportKind::ClaudeAi,
            target: "on claude.ai".into(),
            ..live_row("claude.ai Slack", Scope::ClaudeAi, None)
        };
        show_rows(&mut h, &engine, vec![slack]);
        h.get_by_label(PLUGIN_STATUS);
        // Under the claude.ai group, without the prefix it already says.
        assert!(h.query_by_label("claude.ai Slack").is_none());
        click(&mut h, "Slack");
        h.get_by_label_contains("A connector of your claude.ai account");
        h.get_by_label("About claude.ai connectors");
        assert!(tool(&h, Tool::Edit).accesskit_node().is_disabled());
        assert!(tool(&h, Tool::Delete).accesskit_node().is_disabled());
        drain(&engine);
        press(&mut h, Tool::TurnOff);
        let sent = drain(&engine);
        assert!(
            matches!(sent.as_slice(), [Command::Prepare(changes)] if matches!(
                changes.as_slice(),
                [Change::SetEnabled { enabled: false, .. }]
            )),
            "{sent:?}"
        );
    }

    #[test]
    fn a_connector_says_its_own_state_names_where_it_is_off_and_what_klefki_cannot_know() {
        let (mut h, engine) = live_harness();
        let gmail = ServerRow {
            transport: TransportKind::ClaudeAi,
            target: "on claude.ai".into(),
            disabled_in: 1,
            disabled_projects: vec!["/home/me/p1".into()],
            blocked: Some(klefki_core::api::Blocked {
                why: "Your settings turn off connectors from claude.ai with disableClaudeAiConnectors, in \
                      .claude/settings.local.json. Remove disableClaudeAiConnectors there to let them load."
                    .into(),
                projects: vec!["/home/me/p2".into()],
                policy: false,
            }),
            ..live_row("claude.ai Gmail", Scope::ClaudeAi, None)
        };
        let drive = ServerRow {
            may_not_load: Some("Your allowed list lets servers load by their address.".into()),
            ..live_row("claude.ai Drive", Scope::ClaudeAi, None)
        };
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![gmail, drive],
                project_count: 3,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        // The user's own setting, not policy; and both badges in view, not behind a "+".
        h.get_by_label("loads in 1/3");
        assert!(h.query_by_label("+1").is_none() && h.query_by_label("+2").is_none());
        assert!(h.query_by_label_contains("blocked in").is_none());
        h.get_by_label("may not load");
        click(&mut h, "Gmail");
        h.get_by_label("Connectors off");
        h.get_by_label("disabled in 1 of 3 projects: p1");
        // The type is "claude.ai" (as are the scope and the group), not a guessed "http".
        assert!(h.query_by_label("http").is_none());
        assert!(h.query_all_by_label("on claude.ai").count() >= 2);
        click(&mut h, "Drive");
        h.get_by_label("May not load");
    }

    #[test]
    fn a_connector_turned_off_wherever_it_would_load_says_so_in_its_status() {
        let (mut h, engine) = live_harness();
        let why = "Your settings turn off connectors from claude.ai with disableClaudeAiConnectors, in \
                   ~/.claude/settings.json. Remove disableClaudeAiConnectors there to let them load.";
        let slack = ServerRow {
            not_loaded: Some(why.into()),
            blocked: Some(klefki_core::api::Blocked {
                why: why.into(),
                projects: Vec::new(),
                policy: false,
            }),
            ..live_row("claude.ai Slack", Scope::ClaudeAi, None)
        };
        show_rows(&mut h, &engine, vec![slack]);
        h.get_by_label(TURNED_OFF);
        assert!(h.query_by_label(BLOCKED_BY_POLICY).is_none());
    }

    #[test]
    fn an_organisations_server_is_read_only_and_switches() {
        let (mut h, engine) = live_harness();
        show_rows(&mut h, &engine, vec![organisation_row("docs")]);
        // Not checked here: where its status is.
        h.get_by_label(PLUGIN_STATUS);
        click(&mut h, "docs");
        h.get_by_label_contains(
            "Your organisation sets this server, in /etc/claude-code/managed-settings.json",
        );
        h.get_by_label("Set in");
        // Nothing to edit, and no values or sign-in tabs.
        assert!(h.query_by_label("Env").is_none());
        assert!(tool(&h, Tool::Edit).accesskit_node().is_disabled());
        assert!(tool(&h, Tool::Delete).accesskit_node().is_disabled());
        drain(&engine);
        press(&mut h, Tool::TurnOff);
        let sent = drain(&engine);
        assert!(
            matches!(sent.as_slice(), [Command::Prepare(changes)] if matches!(
                changes.as_slice(),
                [Change::SetEnabled { enabled: false, .. }]
            )),
            "{sent:?}"
        );
    }

    #[test]
    fn a_server_policy_holds_back_in_some_projects_says_where_and_why() {
        let (mut h, engine) = live_harness();
        let why = "Blocked by policy: deniedMcpServers in /p/.claude/settings.json has serverName \"mine\".";
        let mine = ServerRow {
            blocked: Some(klefki_core::api::Blocked {
                why: why.into(),
                projects: vec!["/home/me/p".into()],
                policy: true,
            }),
            ..live_row("mine", Scope::User, None)
        };
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![mine],
                project_count: 3,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        h.get_by_label("blocked in 1/3");
        click(&mut h, "mine");
        h.get_by_label("Blocked");
        // Named like "Shadowed" (cross-review of #49).
        h.get_by_label(&format!("in 1 of 3 projects: p. {why}"));
    }

    #[test]
    fn a_server_policy_blocks_everywhere_says_so_in_the_list() {
        let (mut h, engine) = live_harness();
        let why =
            "A rule in your settings blocks the command localx. Remove the rule to let it load.";
        let x = ServerRow {
            not_loaded: Some(why.into()),
            blocked: Some(klefki_core::api::Blocked {
                why: why.into(),
                projects: vec!["/home/me/p".into()],
                policy: true,
            }),
            ..live_row("x", Scope::Local, Some("/home/me/p"))
        };
        show_rows(&mut h, &engine, vec![x]);
        // The cause in the status, not only "Not loaded"; no "blocked in" badge for the whole of it.
        h.get_by_label(BLOCKED_BY_POLICY);
        assert!(h.query_by_label(NOT_LOADED).is_none());
        assert!(h.query_by_label_contains("blocked in").is_none());
    }

    #[test]
    fn a_server_claude_code_does_not_load_says_why() {
        let (mut h, engine) = live_harness();
        let why = "Your organisation sets a server of the same name in /etc/claude-code/managed-settings.json, \
                   which Claude Code loads instead.";
        let mine = ServerRow {
            not_loaded: Some(why.into()),
            relative_paths: vec!["mine.js".into()],
            preflight: Vec::new(),
            ..live_row("mine", Scope::User, None)
        };
        show_rows(&mut h, &engine, vec![mine, organisation_row("other")]);
        // Said once, in the status column (cross-review of #47): no badge beside the name.
        assert!(h.query_by_label("not loaded").is_none());
        h.get_by_label(NOT_LOADED).hover();
        h.run();
        // Where Claude Code starts it does not matter: it does not start it.
        assert!(h.query_by_label("relative path").is_none());
        click(&mut h, "mine");
        h.get_by_label(&format!("Claude Code does not load this server. {why}"));
        h.state_mut().tab = DetailTab::Status;
        h.run();
        // No "Check now": the check would answer for the organisation's server.
        assert!(h.query_by_label("Check now").is_none());
    }

    #[test]
    fn an_organisations_server_comes_first_and_says_what_it_replaces() {
        let (mut h, engine) = live_harness();
        let why = "Your organisation sets a server of the same name.";
        let gone = |scope, project: Option<&str>| ServerRow {
            not_loaded: Some(why.into()),
            ..live_row("fs", scope, project)
        };
        engine
            .events
            .send(snapshot_of(vec![
                live_row("alpha", Scope::User, None),
                gone(Scope::User, None),
                gone(Scope::Local, Some("/w/repo")),
                organisation_row("fs"),
            ]))
            .unwrap();
        h.run();
        assert_eq!(h.state().visible().first(), Some(&3));
        h.state_mut().selected = BTreeSet::from([3]);
        h.run();
        h.get_by_label("your user server fs; the local server fs in /w/repo");
        assert!(h.query_by_label("Shadowed").is_none());
    }

    #[test]
    fn a_selection_with_a_plugins_server_acts_on_the_others_and_says_what_it_left_out() {
        let (mut h, engine) = live_harness();
        let user = live_row("alpha", Scope::User, None);
        let p = plugin_row("plugin:docs:search", None);
        show_rows(&mut h, &engine, vec![user.clone(), p.clone()]);
        h.state_mut().selected = BTreeSet::from([0, 1]);
        h.run();
        for t in [Tool::TurnOff, Tool::Delete, Tool::Export] {
            assert!(!tool(&h, t).accesskit_node().is_disabled(), "{t:?}");
        }
        // Edit takes one server; this selection holds two.
        assert!(tool(&h, Tool::Edit).accesskit_node().is_disabled());
        drain(&engine);
        // Switching takes both.
        press(&mut h, Tool::TurnOff);
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![
                Change::SetEnabled {
                    id: user.id(),
                    enabled: false
                },
                Change::SetEnabled {
                    id: p.id(),
                    enabled: false
                }
            ])]
        );
        assert!(h.query_by_label_contains("Left out").is_none());
        let offered: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
        assert!(
            offered.contains(&"Turn on 2 servers".to_owned()),
            "{offered:?}"
        );
        assert!(offered.contains(&"Delete alpha".to_owned()), "{offered:?}");
        // Deleting takes the other, and says what it left out.
        h.state_mut().plan = None;
        h.state_mut().preparing = false;
        h.key_press(egui::Key::Delete);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: user.id() }])]
        );
        h.get_by_label_contains("Left out 1 plugin server");
        // Export takes the rest too.
        h.state_mut().plan = None;
        h.state_mut().preparing = false;
        press(&mut h, Tool::Export);
        assert_eq!(drain(&engine), vec![Command::Export(vec![user.id()])]);

        // Only plugin servers selected: the buttons are off, and their hint names the server.
        h.state_mut().exporting = None;
        h.state_mut().selected = BTreeSet::from([1]);
        h.run();
        assert_eq!(
            h.state().read_only_hint().as_deref(),
            Some("plugin:docs:search comes from a plugin and is changed there")
        );
    }

    #[test]
    fn the_delete_key_deletes_the_selection_but_not_while_typing() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone()]);
        click(&mut h, "alpha");
        h.key_press(egui::Key::Delete);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Delete { id: a.id() }])]
        );
        // With the search field focused, Del edits the text.
        h.get_by_role(egui::accesskit::Role::TextInput).click();
        h.run();
        h.key_press(egui::Key::Delete);
        h.run();
        assert!(drain(&engine).is_empty());
    }

    fn press_edit(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        press(h, Tool::Edit);
    }

    #[test]
    fn edit_opens_the_form_and_asks_for_the_definition() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone()]);
        click(&mut h, "alpha");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Status")
            .click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::LastStarts(a.id())]);
        press(&mut h, Tool::Edit);
        assert_eq!(h.state().tab, DetailTab::General);
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
        h.get_by_label_contains("Loading");
        // Asked once, not every frame.
        h.run();
        assert!(drain(&engine).is_empty());
        engine
            .events
            .send(entry_event(&a, FORM_ORIGINAL, true))
            .unwrap();
        h.run();
        // Labelled fields, not the JSON: that is a fallback, reached from the form.
        assert!(
            h.query_all_by_value("node").next().is_some(),
            "the command field"
        );
        assert!(
            h.query_all_by_value("a.js").next().is_some(),
            "the argument field"
        );
        assert!(h.query_by_value(FORM_ORIGINAL).is_none());
        assert!(h.query_by_label("JSON").is_none(), "no JSON tab");
        h.get_by_label("Edit as JSON");
    }

    #[test]
    fn every_way_to_edit_opens_the_form() {
        type Way = fn(&mut Harness<'static, ShellApp>);
        let ways: [(&str, Way); 2] = [
            ("Ctrl+E", |h| {
                h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
                h.run();
            }),
            ("the palette", |h| {
                ctrl_k(h);
                type_into_palette(h, "edit alpha");
                h.key_press(egui::Key::Enter);
                h.run();
            }),
        ];
        for (how, way) in ways {
            let (mut h, engine) = live_harness();
            let (a, _) = two_rows();
            show_rows(&mut h, &engine, vec![a.clone()]);
            click(&mut h, "alpha");
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Status")
                .click();
            h.run();
            assert_eq!(drain(&engine), vec![Command::LastStarts(a.id())], "{how}");
            way(&mut h);
            assert_eq!(h.state().tab, DetailTab::General, "{how}");
            assert_eq!(drain(&engine), vec![Command::Inspect(a.id())], "{how}");
        }
    }

    #[test]
    fn json_is_reached_from_the_form_and_leads_back_to_it() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        click(&mut h, "Edit as JSON");
        h.get_by_value(&json_text(&h));
        h.get_by_label_contains("keep what they are now");
        assert!(
            h.query_by_value("a.js").is_none(),
            "the form is not shown next to the JSON"
        );
        set_json_text(&mut h, |t| t.replace("\"node\"", "\"deno\""));
        h.run();
        click(&mut h, "Back to the form");
        assert!(h.query_all_by_value("deno").next().is_some());
        assert!(h.query_by_value(&json_text(&h)).is_none());
        // JSON the form cannot read keeps you in the JSON, saying why.
        click(&mut h, "Edit as JSON");
        set_json_text(&mut h, |_| "{ \"command\": ".into());
        h.run();
        h.get_by_label_contains("not valid");
        assert!(
            h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        click(&mut h, "Revert");
        assert!(
            !h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        // The Env tab follows: the definition is being edited as JSON.
        click(&mut h, "Env");
        h.get_by_value(&json_text(&h));
        assert!(h.query_by_label("hidden").is_none());
    }

    #[test]
    fn a_definition_the_form_cannot_show_opens_as_json() {
        let odd = "{\n  \"command\": \"x\",\n  \"args\": \"a b\"\n}";
        let (mut h, engine, a) = form_harness(odd, true);
        h.get_by_label_contains("The form cannot show this definition");
        h.get_by_value(odd);
        assert!(
            h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        let fixed = "{\n  \"command\": \"x\",\n  \"args\": [\"a\", \"b\"]\n}";
        set_json_text(&mut h, |_| fixed.into());
        h.run();
        // Still the JSON being typed in, not the form popping up under the cursor.
        h.get_by_value(fixed);
        click(&mut h, "Review and save…");
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: fixed.into()
            }])]
        );
    }

    fn entry_event(row: &ServerRow, json: &str, editable: bool) -> Event {
        Event::Entry {
            id: row.id(),
            json: Ok(json.to_owned()),
            editable,
        }
    }

    #[test]
    fn an_edited_definition_is_sent_for_review_and_revert_undoes_it() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone()]);
        press_edit(&mut h, "alpha");
        drain(&engine);
        let original =
            "{\n  \"command\": \"node\",\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}";
        engine.events.send(entry_event(&a, original, true)).unwrap();
        h.run();
        click(&mut h, "Edit as JSON");
        h.get_by_label_contains("keep what they are now");
        // Unchanged: nothing to review.
        click(&mut h, "Review and save…");
        assert!(drain(&engine).is_empty());
        let edited = original.replace("node", "node22");
        if let Some(Editor {
            state: EditorState::Ready { text, .. },
            ..
        }) = &mut h.state_mut().editor
        {
            *text = edited.clone();
        }
        h.run();
        click(&mut h, "Review and save…");
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: edited
            }])]
        );
        click(&mut h, "Revert");
        match &h.state().editor {
            Some(Editor {
                state: EditorState::Ready { text, .. },
                ..
            }) => assert_eq!(text, original),
            _ => panic!("editor gone"),
        }
    }

    // ---- the edit form

    const FORM_ORIGINAL: &str = "{\n  \"cwd\": \"/w\",\n  \"command\": \"node\",\n  \"args\": [\n    \"a.js\"\n  ],\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}";

    /// alpha selected, its General tab's Edit pressed, and its definition arrived.
    fn form_harness(
        json: &str,
        editable: bool,
    ) -> (
        Harness<'static, ShellApp>,
        klefki_core::api::Detached,
        ServerRow,
    ) {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone()]);
        click(&mut h, "alpha");
        click(&mut h, "Edit definition");
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
        engine.events.send(entry_event(&a, json, editable)).unwrap();
        h.run();
        (h, engine, a)
    }

    fn form<'a>(h: &'a mut Harness<'static, ShellApp>) -> &'a mut Definition {
        match &mut h.state_mut().editor {
            Some(Editor {
                state: EditorState::Ready { form, .. },
                ..
            }) => form
                .as_mut()
                .as_mut()
                .expect("the form reads the definition"),
            _ => panic!("no form"),
        }
    }

    fn set_json_text(h: &mut Harness<'static, ShellApp>, edit: impl FnOnce(&str) -> String) {
        match &mut h.state_mut().editor {
            Some(Editor {
                state: EditorState::Ready { text, .. },
                ..
            }) => *text = edit(text),
            _ => panic!("no editor"),
        }
    }

    fn json_text(h: &Harness<'static, ShellApp>) -> String {
        match &h.state().editor {
            Some(Editor {
                state: EditorState::Ready { text, .. },
                ..
            }) => text.clone(),
            _ => panic!("no editor"),
        }
    }

    // ---- duplicate

    /// alpha (user), beta (local in WebApp), gamma (project in WebApp); Claude knows two projects.
    fn duplicate_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        let mut c = live_row("gamma", Scope::Project, Some("C:/Repos/WebApp"));
        c.approval = Some(Approval::Approved);
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, b, c],
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    fn duplicate_harness_with_many_projects()
    -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        let projects = (0..40)
            .map(|i| format!("C:/Users/me/source/Repos/project-{i:02}"))
            .chain(["C:/Repos/WebApp".into(), "/home/me/notes".into()])
            .collect();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, b],
                projects,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    fn duplicate_requested(engine: &klefki_core::api::Detached) -> Vec<Change> {
        drain(engine)
            .into_iter()
            .filter_map(|c| match c {
                Command::Prepare(changes) => Some(changes),
                _ => None,
            })
            .flatten()
            .collect()
    }

    const SSO_DEFINITION: &str = r#"{
  "type": "http",
  "url": "https://mcp.acme.com/v1/mcp?tenant=7",
  "headers": {"X-Team": "<hidden>"},
  "oauth": {"clientId": "acme-1", "callbackPort": 8123, "authServerMetadataUrl": "https://mcp.acme.com/v1/.well-known/oauth-authorization-server"}
}"#;

    // ---- fix casing

    /// Gateway (user) and gateway (local in WebApp) differ only in case; `other` is fine.
    fn casing_harness(
        extra: Vec<ServerRow>,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let mut user = live_row("Gateway", Scope::User, None);
        let mut local = live_row("gateway", Scope::Local, Some("C:/Repos/WebApp"));
        user.casing_collision = true;
        local.casing_collision = true;
        let mut rows = vec![user, local, live_row("other", Scope::User, None)];
        rows.extend(extra);
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: rows,
                projects: vec!["C:/Repos/WebApp".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    // ---- add

    fn pasted_json(text: &str, i: usize) -> String {
        klefki_core::api::Pasted::parse(text).unwrap().servers[i]
            .json
            .clone()
    }
    const DISCARD_QUESTION: &str = "Discard what you typed?";

    fn fill_in(h: &mut Harness<'static, ShellApp>) {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        click(h, "Fill in");
        assert_eq!(h.state().add.as_ref().unwrap().mode, AddMode::Fill);
    }

    fn add_form<'a>(h: &'a mut Harness<'static, ShellApp>) -> &'a mut AddForm {
        h.state_mut().add.as_mut().unwrap()
    }

    /// Review would ask for a plan now: what used to enable the button, which is now always pressable.
    fn ready(h: &mut Harness<'static, ShellApp>) -> bool {
        let servers = h.state().snapshot.servers.clone();
        h.state_mut()
            .add
            .as_mut()
            .is_some_and(|f| f.changes(&servers).is_some())
    }

    const OAUTH_TOGGLE: &str = "Use OAuth client settings from the provider";

    /// The Add dialog waits behind the review it asked for.
    fn waits_behind_review(h: &Harness<'static, ShellApp>) -> bool {
        h.state().add.as_ref().is_some_and(|f| f.reviewing)
    }

    /// Names the form's server, as typing into the name field does.
    fn name_it(h: &mut Harness<'static, ShellApp>, name: &str) {
        let f = add_form(h);
        f.name = name.into();
        f.name_edited = true;
    }

    /// alpha (user) and beta (local in WebApp), and the forty-odd projects of a long-time Claude Code user.
    /// The form for an OAuth server, filled in as far as Review, with a client secret typed.
    fn sso_filled(h: &mut Harness<'static, ShellApp>) {
        fill_in(h);
        click(h, "HTTP");
        h.get_by_label(OAUTH_TOGGLE).click();
        h.run();
        name_it(h, "sso");
        let f = add_form(h);
        f.fields.url = "https://sso.example.com/mcp".into();
        f.fields.headers = vec![("X-Team".into(), "eng".into())];
        f.fields.client_id = "client-7".into();
        *f.secret.edit() = "Typed-S3cret".into();
        h.run();
    }

    fn open_duplicate_of(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_some());
    }

    // ---- keyboard-first: command palette and arrow keys

    fn ctrl_k(h: &mut Harness<'static, ShellApp>) {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
    }

    fn type_into_palette(h: &mut Harness<'static, ShellApp>, text: &str) {
        *h.state_mut().command.query_mut() = text.to_owned();
        h.run();
    }

    #[test]
    fn the_palette_runs_a_command_by_name() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a, b]);
        ctrl_k(&mut h);
        assert!(h.state().command.is_open());
        h.get_by_label_contains("Go to alpha");
        type_into_palette(&mut h, "refr");
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(!h.state().command.is_open());
        assert_eq!(drain(&engine), vec![Command::Refresh { live_all: true }]);
    }

    #[test]
    fn the_palette_goes_to_a_server_and_acts_on_the_selection() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a, b.clone()]);
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go beta");
        h.key_press(egui::Key::Enter);
        h.run();
        let selected: Vec<_> = h
            .state()
            .selected
            .iter()
            .map(|&i| h.state().snapshot.servers[i].name.clone())
            .collect();
        assert_eq!(selected, ["beta"]);
        ctrl_k(&mut h);
        h.get_by_label_contains("Turn off beta");
        type_into_palette(&mut h, "off");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::SetEnabled {
                id: b.id(),
                enabled: false
            }])]
        );
    }

    #[test]
    fn the_open_palette_follows_a_new_snapshot() {
        // The palette keeps its entries while open; a snapshot arriving meanwhile must still show.
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a.clone()]);
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go");
        h.get_by_label_contains("Go to alpha");
        assert!(h.query_by_label_contains("Go to beta").is_none());
        show_rows(&mut h, &engine, vec![a, b]);
        h.get_by_label_contains("Go to beta");
        // And one without the server takes it away again, query unchanged.
        show_rows(&mut h, &engine, vec![live_row("gamma", Scope::User, None)]);
        assert!(h.query_by_label_contains("Go to alpha").is_none());
        h.get_by_label_contains("Go to gamma");
        // Reopening starts from the current entries too.
        h.key_press(egui::Key::Escape);
        h.run();
        ctrl_k(&mut h);
        h.get_by_label_contains("Go to gamma");
    }

    #[test]
    fn arrows_pick_the_entry_and_escape_closes_without_running_anything() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        show_rows(&mut h, &engine, vec![a, b]);
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go to");
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        // Second "Go to" entry: beta (user servers sort before local ones; alpha is first).
        let name = &h.state().snapshot.servers[*h.state().selected.iter().next().unwrap()].name;
        assert_eq!(name, "beta");
        ctrl_k(&mut h);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().command.is_open());
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn up_and_down_walk_the_list() {
        let (mut h, _engine) = live_harness();
        let rows = vec![
            live_row("one", Scope::User, None),
            live_row("two", Scope::User, None),
            live_row("three", Scope::Local, Some("C:/Repos/WebApp")),
        ];
        let engine = _engine;
        show_rows(&mut h, &engine, rows);
        let name = |h: &Harness<'static, ShellApp>| {
            h.state()
                .selected
                .iter()
                .next()
                .map(|&i| h.state().snapshot.servers[i].name.clone())
        };
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("one"));
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("three"));
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("three"), "stays at the end");
        h.key_press(egui::Key::ArrowUp);
        h.run();
        assert_eq!(name(&h).as_deref(), Some("two"));
    }

    #[test]
    fn arrows_do_not_move_the_list_while_typing_in_the_filter() {
        let (mut h, engine) = live_harness();
        show_rows(&mut h, &engine, vec![live_row("one", Scope::User, None)]);
        h.get_by_role(egui::accesskit::Role::TextInput).click();
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(h.state().selected.is_empty());
    }

    // ---- the review dialog, the result, the empty places

    #[test]
    fn a_page_that_is_not_built_says_what_it_will_do_and_leads_back() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Scope drift").click();
        h.run();
        h.get_by_label_contains("Coming soon");
        click(&mut h, "Back to connections");
        assert_eq!(h.state().page, Page::Connections);
    }

    #[test]
    fn with_nothing_selected_the_detail_panel_teaches_the_keys() {
        let (mut h, _engine) = live_harness();
        h.run();
        h.get_by_label("Select a connection");
        h.get_by_label_contains("all commands");
        h.get_by_label_contains("move through the list");
    }

    #[test]
    fn names_in_a_line_counts_past_eight() {
        assert_eq!(names_in_a_line(&["a", "b"]), "a and b");
        let many: Vec<String> = (1..=10).map(|i| format!("s{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            names_in_a_line(&many),
            "s1, s2, s3, s4, s5, s6, s7, s8 and 2 more"
        );
    }

    #[test]
    fn each_list_keeps_its_own_cut_off() {
        let names = |n: usize| (0..n).map(|i| format!("s{i}")).collect::<Vec<_>>();
        fn refs(v: &[String]) -> Vec<&str> {
            v.iter().map(String::as_str).collect()
        }
        assert_eq!(
            names_in_a_line(&refs(&names(8))),
            "s0, s1, s2, s3, s4, s5, s6 and s7"
        );
        assert_eq!(
            names_in_a_line(&refs(&names(9))),
            "s0, s1, s2, s3, s4, s5, s6, s7 and 1 more"
        );
        assert_eq!(some_names(&names(6)), "s0, s1, s2, s3, s4, s5");
        assert_eq!(some_names(&names(7)), "s0, s1, s2, s3, s4 and 2 more");
        assert_eq!(list_in_words(&refs(&names(3)), "server"), "s0, s1 and s2");
        assert_eq!(list_in_words(&refs(&names(4)), "server"), "4 servers");
        assert_eq!(
            in_projects_text(&names(5), 9, false),
            "in 5 of 9 projects: s0, s1, s2, s3, s4"
        );
        assert_eq!(
            in_projects_text(&names(6), 9, true),
            "in all 6 projects: s0, s1, s2, s3, s4 and 1 more"
        );
    }

    #[test]
    fn a_project_at_the_root_keeps_an_empty_name() {
        let projects = vec!["/".to_owned(), "/home/me/app/".to_owned()];
        assert_eq!(
            in_projects_text(&projects, 3, false),
            "in 2 of 3 projects: , app"
        );
        let group = group_of(&live_row("s", Scope::Local, Some("/")));
        assert_eq!(group.title, "Local \u{b7} ");
        assert_eq!(group.location, None);
        let group = group_of(&live_row("s", Scope::Local, Some("C:\\work\\app\\")));
        assert_eq!(group.title, "Local \u{b7} app");
        assert_eq!(group.location.as_deref(), Some("C:\\work"));
        for (path, folder) in [
            ("/", ""),
            ("\\", ""),
            ("C:\\", "C:"),
            ("a/b/", "b"),
            ("", ""),
        ] {
            assert_eq!(folder_name(path), folder, "{path:?}");
        }
    }

    /// A whole path on this system: `/home/me/x` here, `C:\home\me\x` on Windows, where a path from `/` is not.
    fn whole(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{}", path.replace('/', "\\"))
        } else {
            path.to_owned()
        }
    }

    #[test]
    fn selecting_a_server_does_not_move_the_list() {
        // "1 selected" goes beside the toolbar's buttons when it fits there, and is left out when it does not: on a
        // line of its own it pushed every row down under the pointer, and the next click hit another row.
        for size in [
            egui::vec2(1200.0, 720.0),
            egui::vec2(1100.0, 900.0),
            egui::vec2(1000.0, 600.0),
            egui::vec2(800.0, 480.0),
        ] {
            let (mut h, engine) = live_harness();
            h.set_size(size);
            let (a, b) = two_rows();
            show_rows(&mut h, &engine, vec![a, b]);
            let before = h.get_by_label("Name").rect().top();
            click(&mut h, "beta");
            h.run();
            let after = h.get_by_label("Name").rect().top();
            // Rounded to the pixel; it moved 46 px.
            assert!(
                (after - before).abs() < 1.0,
                "{size:?}: {before} -> {after}"
            );
            if let Some(count) = h.query_by_label("1 selected") {
                // On the row of the last button, which may have wrapped.
                let last = tool(&h, Tool::Export).rect();
                let y = count.rect().center().y;
                assert!(
                    last.top() <= y && y <= last.bottom(),
                    "{size:?}: beside the buttons, {:?} vs {last:?}",
                    count.rect()
                );
            }
        }
    }

    // ---- signing in (decision 005)

    use klefki_core::api::{OAuthInfo, SignInFailure, SignInStep};
    use std::time::Duration;

    fn starting() -> SignInStep {
        SignInStep::Starting {
            wait: Duration::from_secs(300),
        }
    }

    fn failure(title: &str, said: Option<&str>, can_retry: bool) -> SignInStep {
        SignInStep::Failed(SignInFailure {
            title: title.into(),
            help: vec!["Check that the server is running.".into()],
            said: said.map(Into::into),
            can_retry,
        })
    }

    /// A click on the dimmed window beside the dialog.
    fn click_beside(h: &mut Harness<'static, ShellApp>) {
        let at = egui::pos2(6.0, h.ctx.content_rect().max.y - 6.0);
        h.hover_at(at);
        h.drag_at(at);
        h.drop_at(at);
        h.run();
    }

    fn web_row(name: &str) -> ServerRow {
        ServerRow {
            transport: TransportKind::Http,
            target: format!("https://{name}.example.com/mcp"),
            ..live_row(name, Scope::User, None)
        }
    }

    const LINK: &str = "https://auth.example.com/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback&state=s";

    fn sign_in_event(row: &ServerRow, step: SignInStep) -> Event {
        Event::SignIn { id: row.id(), step }
    }

    fn waiting(opened: bool) -> SignInStep {
        SignInStep::Waiting {
            host: "auth.example.com".into(),
            link: LINK.into(),
            opened,
        }
    }

    /// A user http server that needs signing in, selected, on its Status tab.
    fn needs_auth_harness() -> (
        Harness<'static, ShellApp>,
        klefki_core::api::Detached,
        ServerRow,
    ) {
        let (mut h, engine) = live_harness();
        let row = web_row("web");
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::NeedsAuth, Some("needs auth")))
            .unwrap();
        h.run();
        open_status_tab(&mut h, "web");
        drain_all(&engine);
        (h, engine, row)
    }

    fn sign_in_commands(engine: &klefki_core::api::Detached) -> Vec<Command> {
        drain_all(engine)
            .into_iter()
            .filter(|c| {
                matches!(
                    c,
                    Command::SignIn(_)
                        | Command::SignInPaste { .. }
                        | Command::OpenSignInPage(_)
                        | Command::KeepWaitingForSignIn(_)
                        | Command::CancelSignIn(_)
                )
            })
            .collect()
    }

    #[test]
    fn needs_auth_offers_sign_in_first_and_the_dialog_shows_each_step() {
        let (mut h, engine, row) = needs_auth_harness();
        // The way out of "Needs auth" is named where the problem is explained.
        h.get_by_label_contains("Claude Code opens the sign-in page in your browser");
        click(&mut h, "Sign in");
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(row.id())]);
        h.get_by_label("Sign in to web");
        h.get_by_label("Starting Claude Code…");
        engine.events.send(sign_in_event(&row, starting())).unwrap();
        engine
            .events
            .send(sign_in_event(&row, waiting(true)))
            .unwrap();
        h.run();
        h.get_by_label("Finish signing in in your browser.");
        h.get_by_label("The sign-in page at auth.example.com is open in your browser.");
        // auth.example.com signs in for web.example.com: the same site, nothing to point out.
        assert!(
            h.query_by_label_contains("not the server's own address")
                .is_none()
        );
        h.get_by_label_contains("Klefki waits 5 more minutes.");
        // The fallback stays folded away.
        assert!(h.query_by_label_contains("paste it here").is_none());
        h.get_by_label_contains("Open the page again").click();
        h.run();
        assert_eq!(
            sign_in_commands(&engine),
            vec![Command::OpenSignInPage(row.id())]
        );
        h.get_by_label_contains("Copy link").click();
        h.run();
        h.get_by_label("Copied");
        engine
            .events
            .send(sign_in_event(&row, SignInStep::SignedIn))
            .unwrap();
        h.run();
        assert!(h.state().sign_in.is_none());
        assert!(h.query_by_label("Sign in to web").is_none());
        h.get_by_label("Signed in to web.");
    }

    #[test]
    fn the_general_tab_offers_sign_in_only_when_it_is_needed() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let row = web_row("web");
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::NeedsAuth, None))
            .unwrap();
        h.run();
        click(&mut h, "web");
        h.get_by_role_and_label(Role::Button, "General").click();
        h.run();
        drain_all(&engine);
        h.get_by_label("This server needs you to sign in.");
        click(&mut h, "Sign in");
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(row.id())]);
        h.key_press(egui::Key::Escape);
        h.run();
        engine
            .events
            .send(status_event(&row, Status::Connected, None))
            .unwrap();
        h.run();
        assert!(
            h.query_by_label("This server needs you to sign in.")
                .is_none()
        );
    }

    #[test]
    fn cancel_and_escape_stop_the_sign_in() {
        for escape in [false, true] {
            let (mut h, engine, row) = needs_auth_harness();
            click(&mut h, "Sign in");
            engine
                .events
                .send(sign_in_event(&row, waiting(true)))
                .unwrap();
            h.run();
            drain_all(&engine);
            if escape {
                h.key_press(egui::Key::Escape);
            } else {
                h.get_by_label("Cancel").click();
            }
            h.run();
            assert!(h.state().sign_in.is_none(), "escape: {escape}");
            assert_eq!(
                sign_in_commands(&engine),
                vec![Command::CancelSignIn(row.id())]
            );
            // What the engine says afterwards about it changes nothing.
            engine
                .events
                .send(sign_in_event(&row, SignInStep::Cancelled))
                .unwrap();
            h.run();
            assert!(h.state().sign_in.is_none());
            assert!(h.query_by_label_contains("Signed in").is_none());
        }
    }

    #[test]
    fn pasting_back_is_one_fold_away_and_hands_over_what_was_pasted() {
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine
            .events
            .send(sign_in_event(&row, waiting(true)))
            .unwrap();
        h.run();
        drain_all(&engine);
        h.get_by_label_contains("cannot reach localhost?").click();
        h.run();
        h.get_by_label_contains("paste it here");
        let address = "http://localhost:8080/callback?code=c&state=s";
        h.state_mut().sign_in.as_mut().unwrap().set_pasted(address);
        h.run();
        click(&mut h, "Continue");
        assert_eq!(
            sign_in_commands(&engine),
            vec![Command::SignInPaste {
                id: row.id(),
                address: address.into()
            }]
        );
        // Kept until Claude has it, so an address Klefki refuses can be corrected; then dropped (a one-time code).
        assert_eq!(h.state().sign_in.as_ref().unwrap().pasted(), address);
        engine
            .events
            .send(sign_in_event(
                &row,
                SignInStep::PasteRefused("That is not a web address.".into()),
            ))
            .unwrap();
        h.run();
        assert_eq!(h.state().sign_in.as_ref().unwrap().pasted(), address);
        h.get_by_label("That is not a web address.");
        engine
            .events
            .send(sign_in_event(&row, SignInStep::Pasted))
            .unwrap();
        h.run();
        assert_eq!(h.state().sign_in.as_ref().unwrap().pasted(), "");
        h.get_by_label("Claude Code is checking the address…");
        engine
            .events
            .send(sign_in_event(
                &row,
                SignInStep::PasteRefused("That address belongs to another sign-in.".into()),
            ))
            .unwrap();
        h.run();
        // Back to waiting, with the reason next to the field, which is open.
        h.get_by_label("That address belongs to another sign-in.");
        h.get_by_label("Finish signing in in your browser.");
        // Continue needs something to send.
        assert!(h.get_by_label("Continue").accesskit_node().is_disabled());
    }

    #[test]
    fn a_failed_sign_in_says_why_and_can_be_tried_again() {
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine
            .events
            .send(sign_in_event(
                &row,
                failure(
                    "The sign-in did not finish.",
                    Some("OAuth callback port 8766 is already in use"),
                    true,
                ),
            ))
            .unwrap();
        h.run();
        drain_all(&engine);
        h.get_by_label("The sign-in did not finish.");
        h.get_by_label("Check that the server is running.");
        h.get_by_label("Claude Code said: OAuth callback port 8766 is already in use");
        assert!(
            h.query_by_label("Cancel").is_none(),
            "nothing is running to cancel"
        );
        click(&mut h, "Try again");
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(row.id())]);
        h.get_by_label("Starting Claude Code…");
        engine
            .events
            .send(sign_in_event(&row, failure("Again.", None, true)))
            .unwrap();
        h.run();
        click(&mut h, "Close");
        assert!(h.state().sign_in.is_none());
        assert!(
            sign_in_commands(&engine).is_empty(),
            "closing a failed sign-in sends nothing"
        );
    }

    #[test]
    fn when_the_browser_did_not_open_copying_the_link_is_the_way_on() {
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine
            .events
            .send(sign_in_event(&row, waiting(false)))
            .unwrap();
        h.run();
        h.get_by_label("Klefki could not open your browser.");
        h.get_by_label_contains("Copy link");
        assert!(h.query_by_label_contains("Open the page again").is_none());
    }

    #[test]
    fn sign_in_is_offered_up_front_only_where_it_is_needed_and_always_by_one_name() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let web = web_row("web");
        let sso = ServerRow {
            oauth: Some(OAuthInfo {
                client_id: None,
                scopes: vec![],
            }),
            ..web_row("sso")
        };
        let sse = ServerRow {
            transport: TransportKind::Sse,
            ..web_row("events")
        };
        let cmd = live_row("cmd", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![
                web.clone(),
                sso.clone(),
                sse.clone(),
                cmd.clone(),
            ]))
            .unwrap();
        for row in [&web, &sso] {
            engine
                .events
                .send(status_event(row, Status::Connected, None))
                .unwrap();
        }
        engine
            .events
            .send(status_event(&sse, Status::NeedsAuth, None))
            .unwrap();
        h.run();
        // Offered in the detail panel: the toolbar's button is always there, by a name of its own.
        let offered = |h: &Harness<'static, ShellApp>| {
            h.query_all_by_label_contains("Sign in")
                .any(|n| n.accesskit_node().label().as_deref() != Some(Tool::SignIn.name()))
        };
        // A server that works and has no OAuth set up (most likely a token in a header): not up front.
        open_status_tab(&mut h, "web");
        assert!(!offered(&h));
        assert!(tool(&h, Tool::SignIn).accesskit_node().is_disabled());
        // Its Auth tab still has it, without contradicting itself.
        h.get_by_role_and_label(Role::Button, "Auth").click();
        h.run();
        h.get_by_label("Sign in");
        h.get_by_label("Claude Code signs in to this server for you when the server asks for it.");
        h.get_by_label("Klefki starts the sign-in and never sees it.");
        // OAuth set up, or Claude says it needs it: up front, and only ever called "Sign in".
        for name in ["sso", "events"] {
            open_status_tab(&mut h, name);
            h.get_by_label("Sign in");
            assert!(h.query_by_label_contains("again").is_none(), "{name}");
            assert!(!tool(&h, Tool::SignIn).accesskit_node().is_disabled());
        }
        click(&mut h, "cmd");
        assert!(!offered(&h));
        assert!(tool(&h, Tool::SignIn).accesskit_node().is_disabled());
        h.get_by_role_and_label(Role::Button, "Auth").click();
        h.run();
        h.get_by_label_contains("does not sign in with OAuth");
        click(&mut h, "sso");
        press(&mut h, Tool::SignIn);
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(sso.id())]);
        // One at a time: while the dialog is open, nothing else starts one.
        assert!(h.state().sign_in.is_some());
    }

    /// The click that brings Klefki back from the browser lands beside the dialog most of the time; it must not
    /// stop the sign-in. Only Esc and Cancel do.
    #[test]
    fn a_click_beside_the_dialog_does_not_stop_a_running_sign_in() {
        for checking in [false, true] {
            let (mut h, engine, row) = needs_auth_harness();
            click(&mut h, "Sign in");
            engine.events.send(sign_in_event(&row, starting())).unwrap();
            engine
                .events
                .send(sign_in_event(&row, waiting(true)))
                .unwrap();
            if checking {
                engine
                    .events
                    .send(sign_in_event(&row, SignInStep::Pasted))
                    .unwrap();
            }
            h.run();
            drain_all(&engine);
            click_beside(&mut h);
            assert!(h.state().sign_in.is_some(), "checking: {checking}");
            assert!(sign_in_commands(&engine).is_empty(), "checking: {checking}");
            h.get_by_label("Sign in to web");
        }
        // Once it has ended there is nothing to lose: a click beside closes it.
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine
            .events
            .send(sign_in_event(&row, failure("Down.", None, true)))
            .unwrap();
        h.run();
        drain_all(&engine);
        click_beside(&mut h);
        assert!(h.state().sign_in.is_none());
        assert!(sign_in_commands(&engine).is_empty());
    }

    #[test]
    fn when_trying_again_cannot_help_close_is_the_way_on() {
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine
            .events
            .send(sign_in_event(
                &row,
                failure(
                    "This server does not sign in through Claude Code.",
                    Some("Dynamic Client Registration rejected (HTTP 501)"),
                    false,
                ),
            ))
            .unwrap();
        h.run();
        drain_all(&engine);
        h.get_by_label("This server does not sign in through Claude Code.");
        assert!(h.query_by_label("Try again").is_none());
        // Close is the default: Enter closes, and starts nothing.
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.state().sign_in.is_none());
        assert!(sign_in_commands(&engine).is_empty());
    }

    #[test]
    fn the_wait_is_said_and_can_be_made_longer_before_it_ends() {
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine.events.send(sign_in_event(&row, starting())).unwrap();
        engine
            .events
            .send(sign_in_event(&row, waiting(true)))
            .unwrap();
        h.run();
        drain_all(&engine);
        h.get_by_label_contains("Klefki waits 5 more minutes.");
        assert!(h.query_by_label("Keep waiting").is_none());
        // Near the end: said in seconds, with one action to wait the whole time again.
        let now = h.ctx.input(|i| i.time);
        h.state_mut()
            .sign_in
            .as_mut()
            .unwrap()
            .set_ends_at(now + 20.0);
        h.run();
        h.get_by_label_contains("Klefki stops waiting in");
        click(&mut h, "Keep waiting");
        assert_eq!(
            sign_in_commands(&engine),
            vec![Command::KeepWaitingForSignIn(row.id())]
        );
        h.get_by_label_contains("Klefki waits 5 more minutes.");
        assert!(h.query_by_label("Keep waiting").is_none());
    }

    #[test]
    fn a_sign_in_page_on_another_site_is_pointed_out() {
        let (mut h, engine, row) = needs_auth_harness();
        click(&mut h, "Sign in");
        engine
            .events
            .send(sign_in_event(
                &row,
                SignInStep::Waiting {
                    host: "login.other.org".into(),
                    link: LINK.into(),
                    opened: true,
                },
            ))
            .unwrap();
        h.run();
        h.get_by_label_contains("at login.other.org is open");
        h.get_by_label_contains("That is not the server's own address (web.example.com)");
    }

    #[test]
    fn the_palette_signs_in_to_the_selected_server() {
        let (mut h, engine, row) = needs_auth_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
        h.get_by_label_contains("Sign in to web").click();
        h.run();
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(row.id())]);
        h.get_by_label("Sign in to web");
    }

    #[test]
    fn an_oauth_server_just_added_can_be_signed_in_to_from_the_card() {
        let (mut h, engine) = duplicate_harness();
        sso_filled(&mut h);
        click(&mut h, "Review");
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
            .unwrap();
        h.run();
        // The engine sends the new list first, then that the plan was written.
        let sso = web_row("sso");
        let mut rows = h.state().snapshot.servers.clone();
        rows.push(sso.clone());
        engine.events.send(snapshot_of(rows)).unwrap();
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        drain_all(&engine);
        h.get_by_label("Added sso for the user. Sign in now, or later from its Status tab.");
        // It does not fade while it offers something to do: shown long ago, it is still there.
        h.state_mut().outcome.as_mut().unwrap().shown_at = Some(-1000.0);
        h.run();
        click(&mut h, "Sign in");
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(sso.id())]);
        h.get_by_label("Sign in to sso");
        assert!(h.query_by_label_contains("Added sso").is_none());
    }

    // ---- move

    fn open_move_of(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        press(h, Tool::Move);
        assert!(h.state().moving.is_some());
        h.get_by_label(&format!("Move {name}"));
    }

    // ---- import

    /// An export of three servers: alpha (the user has one), docs (needs a variable) and notion (OAuth).
    const IMPORTED: &str = r#"{
  "mcpServers": {
    "alpha": {"command": "node", "args": ["a.js"]},
    "docs": {"type": "http", "url": "https://docs.example.com/mcp", "headers": {"Authorization": "Bearer ${AUTH_TOKEN}"}},
    "notion": {"type": "http", "url": "https://mcp.notion.com/mcp", "oauth": {"clientId": "abc"}}
  }
}"#;

    // ---- profiles

    fn profiles_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let a = live_row("alpha", Scope::User, None);
        let b = live_row("beta", Scope::User, None);
        let l = live_row("lint", Scope::Local, Some("/work/app"));
        // Two servers of one name: the summary says which.
        let la = live_row("alpha", Scope::Local, Some("/work/app"));
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, b, l, la],
                projects: vec!["/work/app".to_owned(), "/home/notes".to_owned()],
                project_count: 2,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        while engine.commands.try_recv().is_ok() {}
        (h, engine)
    }

    fn id_of(name: &str, scope: Scope, project: Option<&str>) -> ConnectionId {
        ConnectionId::new(scope, project, name)
    }

    fn sent(engine: &klefki_core::api::Detached) -> Vec<Command> {
        std::iter::from_fn(|| engine.commands.try_recv().ok()).collect()
    }

    fn work_profile() -> klefki_core::api::Profiles {
        klefki_core::api::Profiles {
            profiles: vec![klefki_core::api::Profile {
                name: "Work".to_owned(),
                members: vec![
                    id_of("alpha", Scope::User, None),
                    id_of("gone", Scope::User, None),
                ],
            }],
            problem: None,
        }
    }

    fn open_profiles(
        h: &mut Harness<'static, ShellApp>,
        engine: &klefki_core::api::Detached,
        profiles: klefki_core::api::Profiles,
    ) {
        h.state_mut().go(Page::Profiles);
        engine
            .events
            .send(klefki_core::api::Event::Profiles(profiles))
            .unwrap();
        h.run();
        while engine.commands.try_recv().is_ok() {}
    }

    /// The profiles harness, with beta off in notes: there Work's alpha is the only server on.
    fn snapshot_with_beta_off_in_notes(off: bool) -> klefki_core::api::Event {
        let a = live_row("alpha", Scope::User, None);
        let mut b = live_row("beta", Scope::User, None);
        if off {
            b.disabled_projects = vec!["/home/notes".to_owned()];
            b.disabled_in = 1;
        }
        let l = live_row("lint", Scope::Local, Some("/work/app"));
        let la = live_row("alpha", Scope::Local, Some("/work/app"));
        klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: vec![a, b, l, la],
            projects: vec!["/work/app".to_owned(), "/home/notes".to_owned()],
            project_count: 2,
            ..Snapshot::default()
        }))
    }

    #[test]
    fn a_profile_says_where_only_its_servers_are_on_and_follows_the_files() {
        let (mut h, engine) = profiles_harness();
        engine
            .events
            .send(snapshot_with_beta_off_in_notes(true))
            .unwrap();
        h.run();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_label("Only these servers are on in notes.");
        // Beta on again: worked out anew from the files, nothing remembered.
        engine
            .events
            .send(snapshot_with_beta_off_in_notes(false))
            .unwrap();
        h.run();
        assert!(
            h.query_by_label_contains("Only these servers are on")
                .is_none()
        );
    }

    #[test]
    fn the_palette_opens_profiles_starts_one_and_uses_one() {
        let (mut h, engine) = profiles_harness();
        // Each snapshot names the profiles, so opening the palette reads nothing.
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                profiles: vec!["Work".to_owned()],
                ..(*h.state().snapshot).clone()
            })))
            .unwrap();
        h.run();
        sent(&engine);
        ctrl_k(&mut h);
        assert_eq!(sent(&engine), []);
        type_into_palette(&mut h, "use work");
        h.get_by_label_contains("Use profile Work…");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().page, Page::Profiles);
        assert_eq!(sent(&engine), [Command::Profiles]);
        engine
            .events
            .send(klefki_core::api::Event::Profiles(work_profile()))
            .unwrap();
        h.run();
        h.run();
        // Where to use it has the keyboard: Space opens the list of projects.
        h.key_press(egui::Key::Space);
        h.run();
        h.get_by_label("Every project");

        ctrl_k(&mut h);
        type_into_palette(&mut h, "new profile");
        h.key_press(egui::Key::Enter);
        h.run();
        h.get_by_label("Name");
        h.get_by_label("Save profile");
    }

    #[test]
    fn the_palette_goes_to_profiles_from_anywhere_and_offers_no_change_on_sample_data() {
        let mut h = harness();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "profiles");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().page, Page::Profiles);
        ctrl_k(&mut h);
        type_into_palette(&mut h, "new profile");
        assert!(h.query_by_label_contains("New profile").is_none());
    }

    #[test]
    fn the_profiles_page_is_read_when_opened_and_let_go_when_left() {
        let (mut h, engine) = profiles_harness();
        h.get_by_label_contains("Profiles").click();
        h.run();
        assert_eq!(h.state().page, Page::Profiles);
        assert_eq!(sent(&engine), [Command::Profiles]);
        engine
            .events
            .send(klefki_core::api::Event::Profiles(work_profile()))
            .unwrap();
        h.run();
        h.get_by_label("Work");
        // A name two servers share says which one it is.
        h.get_by_label("1 server: alpha (User)");
        // A server no longer configured is named once, apart from the rest.
        h.get_by_label("1 no longer configured: gone.");
        h.get_by_label_contains("Connections").click();
        h.run();
        assert!(h.state().profiles.is_none());
    }

    #[test]
    fn a_long_profile_names_a_few_servers_and_counts_the_rest() {
        let (mut h, engine) = live_harness();
        let rows: Vec<ServerRow> = (0..8)
            .map(|i| live_row(&format!("s{i}"), Scope::User, None))
            .collect();
        let members = rows.iter().map(|r| r.connection.clone()).collect();
        show_rows(&mut h, &engine, rows);
        open_profiles(
            &mut h,
            &engine,
            klefki_core::api::Profiles {
                profiles: vec![klefki_core::api::Profile {
                    name: "Big".to_owned(),
                    members,
                }],
                problem: None,
            },
        );
        h.get_by_label("8 servers: s0, s1, s2, s3, s4 and 3 more");
    }

    #[test]
    fn what_save_said_stays_while_it_still_holds() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        h.get_by_label_contains("New profile").click();
        h.run();
        click(&mut h, "Save profile");
        // Ticking a server does not give the profile a name: the message stays.
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.run();
        h.get_by_label_contains("Give the profile a name.");
        // Unticking the last one after Save asked for one does not bring a new message before the next Save.
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.run();
        h.get_by_label_contains("Give the profile a name.");
        assert!(
            h.query_by_label_contains("Tick at least one server.")
                .is_none()
        );
        // Named now, with nothing ticked: Save asks for a server, and typing in the name does not take that back.
        h.get_by_label("Name").focus();
        h.get_by_label("Name").type_text("Writing");
        h.run();
        assert!(
            h.query_by_label_contains("Give the profile a name.")
                .is_none()
        );
        click(&mut h, "Save profile");
        h.get_by_label_contains("Tick at least one server.");
        h.get_by_label("Name").focus();
        h.get_by_label("Name").type_text("2");
        h.run();
        h.get_by_label_contains("Tick at least one server.");
    }

    #[test]
    fn a_new_profile_is_made_from_the_servers_ticked_in_it() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        h.get_by_label_contains("No profiles yet");
        h.get_by_label_contains("New profile").click();
        h.run();
        // The name is typed straight away.
        assert!(h.get_by_label("Name").accesskit_node().is_focused());
        // Save says what is missing, next to the name, rather than being greyed out.
        click(&mut h, "Save profile");
        h.get_by_label_contains("Give the profile a name.");
        assert!(sent(&engine).is_empty());
        h.get_by_label("Name").focus();
        h.get_by_label("Name").type_text("Writing");
        h.run();
        click(&mut h, "Save profile");
        h.get_by_label_contains("Tick at least one server.");
        // Grouped as on the Connections page.
        h.get_by_label("User");
        h.get_by_label("Local · app");
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.run();
        // Ticked: the message goes at once, not on the next Save.
        assert!(
            h.query_by_label_contains("Tick at least one server.")
                .is_none()
        );
        h.get_by_role_and_label(Role::CheckBox, "lint").click();
        h.run();
        click(&mut h, "Save profile");
        assert_eq!(
            sent(&engine),
            [Command::SaveProfile {
                name: "Writing".to_owned(),
                members: vec![
                    id_of("beta", Scope::User, None),
                    id_of("lint", Scope::Local, Some("/work/app")),
                ],
                replaces: None,
            }]
        );
        // What the engine refuses is said where the profile was being made.
        engine
            .events
            .send(klefki_core::api::Event::ProfileRefused(
                "There is a profile named Writing already.".to_owned(),
            ))
            .unwrap();
        h.run();
        h.get_by_label_contains("There is a profile named Writing already.");
        // Kept, it closes the form.
        click(&mut h, "Save profile");
        engine
            .events
            .send(klefki_core::api::Event::Profiles(work_profile()))
            .unwrap();
        h.run();
        assert!(h.query_by_label("Save profile").is_none());
    }

    #[test]
    fn enter_saves_a_profile_and_escape_cancels_it() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        h.get_by_label_contains("New profile").click();
        h.run();
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.run();
        h.get_by_label("Name").focus();
        h.get_by_label("Name").type_text("Quick");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(
            matches!(&sent(&engine)[..], [Command::SaveProfile { name, .. }] if name == "Quick")
        );
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label("Save profile").is_none());
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn a_profile_being_made_is_kept_while_another_page_is_shown() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        h.get_by_label_contains("New profile").click();
        h.run();
        h.get_by_label("Name").type_text("Draft");
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.run();
        h.state_mut().go(Page::Connections);
        h.run();
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        assert_eq!(h.get_by_label("Name").value().as_deref(), Some("Draft"));
        assert!(
            h.get_by_role_and_label(Role::CheckBox, "beta")
                .accesskit_node()
                .toggled()
                == Some(egui::accesskit::Toggled::True)
        );
    }

    #[test]
    fn a_profile_is_edited_with_its_servers_already_ticked() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_label_contains("Edit").click();
        h.run();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.get_by_label("Name").type_text("Office");
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.run();
        click(&mut h, "Save profile");
        // A server no longer configured is kept: it may come back, and leaving it out is the user's call.
        assert_eq!(
            sent(&engine),
            [Command::SaveProfile {
                name: "Office".to_owned(),
                members: vec![
                    id_of("alpha", Scope::User, None),
                    id_of("gone", Scope::User, None),
                    id_of("beta", Scope::User, None),
                ],
                replaces: Some("Work".to_owned()),
            }]
        );
    }

    #[test]
    fn using_a_profile_is_reviewed_for_the_place_chosen() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        // Nowhere chosen yet: pressing it says so, rather than nothing happening.
        click(&mut h, "Use profile…");
        h.get_by_label_contains("Choose a project to use it in first.");
        assert!(sent(&engine).is_empty());
        h.get_by_role_and_label(Role::ComboBox, "Use in").click();
        h.run();
        // By folder name, sorted, with Every project last.
        let names: Vec<String> = h
            .query_all_by_role(Role::Button)
            .filter_map(|n| n.accesskit_node().label())
            .filter(|l| l.starts_with("app") || l.starts_with("notes") || l == "Every project")
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(
            names[0].starts_with("app") && names[1].starts_with("notes"),
            "{names:?}"
        );
        assert_eq!(names[2], "Every project");
        h.get_by_label_contains("app/work").click();
        h.run();
        assert!(
            h.query_by_label_contains("Choose a project to use it in first.")
                .is_none()
        );
        // Ticked from the start (the owner's choice, 2026-10-20): a profile's servers become the ones on there.
        assert_eq!(
            h.get_by_role_and_label(Role::CheckBox, "Only these servers there")
                .accesskit_node()
                .toggled(),
            Some(egui::accesskit::Toggled::True)
        );
        click(&mut h, "Use profile…");
        assert_eq!(
            sent(&engine),
            [Command::Prepare(vec![Change::UseProfile {
                name: "Work".to_owned(),
                members: work_profile().profiles[0].members.clone(),
                project: Some("/work/app".to_owned()),
                only: true,
            }])]
        );
        // Every project is a choice too.
        h.state_mut().preparing = false;
        h.get_by_role_and_label(Role::ComboBox, "Use in").click();
        h.run();
        click(&mut h, "Every project");
        click(&mut h, "Use profile…");
        assert!(matches!(
            &sent(&engine)[..],
            [Command::Prepare(c)] if matches!(&c[..], [Change::UseProfile { project: None, only: true, .. }])
        ));
        // Unticked, only its servers are turned on.
        h.state_mut().preparing = false;
        h.get_by_role_and_label(Role::CheckBox, "Only these servers there")
            .click();
        h.run();
        click(&mut h, "Use profile…");
        assert!(matches!(
            &sent(&engine)[..],
            [Command::Prepare(c)] if matches!(&c[..], [Change::UseProfile { project: None, only: false, .. }])
        ));
    }

    /// The owner's report: using profiles that were already in effect opened a review that only said "The
    /// configuration already looks like this". Said on the profile's card instead, with which servers are on where.
    #[test]
    fn using_a_profile_that_changes_nothing_says_so_on_its_card() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_role_and_label(Role::ComboBox, "Use in").click();
        h.run();
        h.get_by_label_contains("app/work").click();
        h.run();
        // Unticked, as the owner had it.
        h.get_by_role_and_label(Role::CheckBox, "Only these servers there")
            .click();
        h.run();
        click(&mut h, "Use profile…");
        assert!(matches!(&sent(&engine)[..], [Command::Prepare(_)]));
        let mut plan = plan_view(
            7,
            &[],
            &["alpha will still not load in app: the local server alpha has its name there."],
            true,
        );
        plan.already = vec!["The server of Work, alpha, is already on in app. Other servers there are left as they are.".to_owned()];
        plan.others_on = true;
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        // No review with nothing to save: the plan is let go.
        assert!(h.query_by_label_contains("Review the change").is_none());
        assert_eq!(sent(&engine), [Command::Discard(7)]);
        h.get_by_label_contains("Nothing to change");
        h.get_by_label("The server of Work, alpha, is already on in app. Other servers there are left as they are.");
        h.get_by_label_contains("alpha will still not load in app");
        h.get_by_label_contains("tick Only these servers there");
        // Ticking it is a new way of using it: what was said goes, and the next Use is reviewed.
        h.get_by_role_and_label(Role::CheckBox, "Only these servers there")
            .click();
        h.run();
        assert!(h.query_by_label_contains("Nothing to change").is_none());
        click(&mut h, "Use profile…");
        assert!(matches!(&sent(&engine)[..], [Command::Prepare(_)]));
        engine
            .events
            .send(Event::PlanReady(plan_view(8, &["+ beta"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label_contains("Review the change");
    }

    /// Uses Work in app (unticked) and has it said on the card, as the owner would.
    fn work_said_unchanged(
        h: &mut Harness<'static, ShellApp>,
        engine: &klefki_core::api::Detached,
        already: &str,
        others_on: bool,
    ) {
        use egui::accesskit::Role;
        h.get_by_role_and_label(Role::ComboBox, "Use in").click();
        h.run();
        h.get_by_label_contains("app/work").click();
        h.run();
        click(h, "Use profile…");
        while engine.commands.try_recv().is_ok() {}
        let mut plan = plan_view(7, &[], &[], true);
        plan.already = vec![already.to_owned()];
        plan.others_on = others_on;
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label_contains("Nothing to change");
    }

    /// Cross-review of #60: the card said "already on" after another change had turned those servers off. What a Use
    /// found is true of the files then, so a new snapshot lets it go.
    #[test]
    fn what_a_profile_found_already_so_goes_when_the_files_change() {
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        work_said_unchanged(
            &mut h,
            &engine,
            "The server of Work, alpha, is already on in app.",
            true,
        );
        let mut alpha = live_row("alpha", Scope::User, None);
        alpha.disabled_projects = vec!["/work/app".to_owned()];
        alpha.disabled_in = 1;
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![alpha, live_row("beta", Scope::User, None)],
                projects: vec!["/work/app".to_owned(), "/home/notes".to_owned()],
                project_count: 2,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("Nothing to change").is_none());
        assert!(h.query_by_label_contains("already on in app").is_none());
    }

    /// Cross-review of #60: where none of its servers loads, ticking "only" turns things off and nothing on, so the
    /// card does not suggest it; the reason the engine gives is shown instead.
    #[test]
    fn the_hint_to_tick_only_is_given_only_when_other_servers_are_on() {
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        let why = "None of the servers of Work loads in app, so there is nothing to turn on there.";
        work_said_unchanged(&mut h, &engine, why, false);
        h.get_by_label(why);
        assert!(
            h.query_by_label_contains("tick Only these servers there")
                .is_none()
        );
    }

    /// Cross-review of #60: used alone where it is on alone, the card already says "Only these servers are on in
    /// app" under its servers; the no-op does not say it again.
    #[test]
    fn a_profile_on_alone_is_not_said_twice() {
        let (mut h, engine) = profiles_harness();
        let mut beta = live_row("beta", Scope::User, None);
        beta.disabled_projects = vec!["/work/app".to_owned()];
        beta.disabled_in = 1;
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![live_row("alpha", Scope::User, None), beta],
                projects: vec!["/work/app".to_owned(), "/home/notes".to_owned()],
                project_count: 2,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_label_contains("Only these servers are on in app");
        let again = "The server of Work, alpha, is already the only one on in app.";
        work_said_unchanged(&mut h, &engine, again, false);
        assert!(h.query_by_label(again).is_none());
    }

    /// The page was left before the answer came: the review says it, rather than nothing at all.
    #[test]
    fn a_profile_s_no_op_after_the_page_is_left_is_said_in_the_review() {
        use egui::accesskit::Role;
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_role_and_label(Role::ComboBox, "Use in").click();
        h.run();
        h.get_by_label_contains("app/work").click();
        h.run();
        click(&mut h, "Use profile…");
        h.state_mut().go(Page::Connections);
        h.run();
        let mut plan = plan_view(7, &[], &[], true);
        plan.already = vec!["The server of Work, alpha, is already on in app.".to_owned()];
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label_contains("Review the change");
        h.get_by_label("The server of Work, alpha, is already on in app.");
    }

    /// A no-op plan for any other change still opens its review, which says so.
    #[test]
    fn only_a_profile_s_no_op_is_said_on_its_card() {
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        h.state_mut().prepare(vec![Change::SetEnabled {
            id: ServerId::new(Scope::User, None, "alpha"),
            enabled: true,
        }]);
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &[], &[], true)))
            .unwrap();
        h.run();
        h.get_by_label_contains("Review the change");
        assert!(h.query_by_label_contains("Nothing to change").is_none());
    }

    #[test]
    fn deleting_a_profile_asks_first() {
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_label_contains("Delete").click();
        h.run();
        assert!(sent(&engine).is_empty());
        h.get_by_label_contains("Its servers stay as they are");
        click(&mut h, "Cancel");
        assert!(sent(&engine).is_empty());
        h.get_by_label_contains("Delete").click();
        h.run();
        click(&mut h, "Delete profile");
        assert_eq!(sent(&engine), [Command::DeleteProfile("Work".to_owned())]);
    }

    /// The profiles harness with beta off in notes, as Work's use there left it.
    fn snapshot_with_work_in_use_in_notes() -> klefki_core::api::Event {
        let klefki_core::api::Event::Snapshot(snapshot) = snapshot_with_beta_off_in_notes(true)
        else {
            unreachable!()
        };
        let mut snapshot = (*snapshot).clone();
        snapshot.profile_uses = klefki_core::api::ProfileUses {
            uses: vec![klefki_core::api::ProfileUse {
                profile: "Work".to_owned(),
                switches: vec![klefki_core::api::Switch {
                    project: "/home/notes".to_owned(),
                    name: "beta".to_owned(),
                    off: true,
                }],
            }],
            problem: None,
        };
        klefki_core::api::Event::Snapshot(Arc::new(snapshot))
    }

    /// The owner's ask (2026-10-20): a profile in use says where, and stopping it, or deleting it, puts back what it
    /// switched, after the usual review. Delete only keeps things as they are.
    #[test]
    fn a_profile_in_use_says_where_and_stops_or_deletes() {
        let (mut h, engine) = profiles_harness();
        engine
            .events
            .send(snapshot_with_work_in_use_in_notes())
            .unwrap();
        h.run();
        open_profiles(&mut h, &engine, work_profile());
        // On alone there too: said once, not twice.
        h.get_by_label_contains("In use in notes, where only these servers are on.");
        assert!(
            h.query_by_label_contains("Only these servers are on in")
                .is_none()
        );
        click(&mut h, "Stop using in notes…");
        assert_eq!(
            sent(&engine),
            [Command::Prepare(vec![Change::StopProfile {
                name: "Work".to_owned(),
                project: None,
                delete: false,
            }])]
        );
        h.state_mut().preparing = false;
        h.get_by_label_contains("Delete").click();
        h.run();
        h.get_by_label_contains("It is in use in notes.");
        click(&mut h, "Stop using and delete…");
        assert_eq!(
            sent(&engine),
            [Command::Prepare(vec![Change::StopProfile {
                name: "Work".to_owned(),
                project: None,
                delete: true,
            }])]
        );
        h.state_mut().preparing = false;
        h.get_by_label_contains("Delete").click();
        h.run();
        click(&mut h, "Delete only");
        assert_eq!(sent(&engine), [Command::DeleteProfile("Work".to_owned())]);
        // Switched since (beta back on): no longer in use, and Delete asks as before.
        engine
            .events
            .send(snapshot_with_beta_off_in_notes(false))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("In use in").is_none());
    }

    /// In use in more than one project, Stop asks where: one of them, or all (cross-review of PR #63).
    #[test]
    fn a_profile_in_use_in_several_projects_stops_in_the_one_chosen() {
        let (mut h, engine) = profiles_harness();
        let klefki_core::api::Event::Snapshot(snapshot) = snapshot_with_work_in_use_in_notes()
        else {
            unreachable!()
        };
        let mut snapshot = (*snapshot).clone();
        for row in &mut snapshot.servers {
            if row.name == "beta" {
                row.disabled_projects.push("/work/app".to_owned());
                row.disabled_in = 2;
            }
        }
        snapshot.profile_uses.uses[0]
            .switches
            .push(klefki_core::api::Switch {
                project: "/work/app".to_owned(),
                name: "beta".to_owned(),
                off: true,
            });
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(snapshot)))
            .unwrap();
        h.run();
        open_profiles(&mut h, &engine, work_profile());
        click(&mut h, "Stop using in…");
        click(&mut h, "notes");
        assert_eq!(
            sent(&engine),
            [Command::Prepare(vec![Change::StopProfile {
                name: "Work".to_owned(),
                project: Some("/home/notes".to_owned()),
                delete: false,
            }])]
        );
        h.state_mut().preparing = false;
        click(&mut h, "Stop using in…");
        click(&mut h, "All 2 projects");
        assert!(matches!(
            &sent(&engine)[..],
            [Command::Prepare(c)] if matches!(&c[..], [Change::StopProfile { project: None, delete: false, .. }])
        ));
    }

    #[test]
    fn the_filter_box_filters_profiles_by_name_or_server() {
        let (mut h, engine) = profiles_harness();
        let mut profiles = work_profile();
        profiles.profiles.push(klefki_core::api::Profile {
            name: "Home".to_owned(),
            members: vec![id_of("beta", Scope::User, None)],
        });
        open_profiles(&mut h, &engine, profiles);
        h.get_by_label("Home");
        h.state_mut().profiles.as_mut().unwrap().filter = "alp".to_owned();
        h.run();
        h.get_by_label("Work");
        assert!(h.query_by_label("Home").is_none());
        h.state_mut().profiles.as_mut().unwrap().filter = "zzz".to_owned();
        h.run();
        h.get_by_label_contains("No profiles match");
    }

    #[test]
    fn a_profiles_file_read_in_part_says_so() {
        let (mut h, engine) = profiles_harness();
        let mut profiles = work_profile();
        profiles.problem = Some(
            "profiles.json has 2 entries Klefki does not understand; they are left out.".into(),
        );
        open_profiles(&mut h, &engine, profiles);
        // Said once, with what it means; the profiles read can still be used, not changed.
        h.get_by_label_contains("2 entries Klefki does not understand");
        h.get_by_label_contains("until the file is fixed");
        h.get_by_label("Work");
        for button in ["New profile", "Edit", "Delete"] {
            assert!(
                h.get_by_label_contains(button)
                    .accesskit_node()
                    .is_disabled(),
                "{button}"
            );
        }
        assert!(
            !h.get_by_label("Use profile…")
                .accesskit_node()
                .is_disabled()
        );
    }

    /// Found in review of PR #58: the palette's New profile opened the editor though the page offers no New profile
    /// while the file is read in part; the work was refused only on Save.
    #[test]
    fn the_palettes_new_profile_says_why_not_while_the_file_is_read_in_part() {
        let (mut h, engine) = profiles_harness();
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, command::Action::NewProfile);
        h.run();
        let mut profiles = work_profile();
        profiles.problem = Some("profiles.json is from a newer Klefki.".into());
        engine
            .events
            .send(klefki_core::api::Event::Profiles(profiles))
            .unwrap();
        h.run();
        assert!(h.query_by_label("Save profile").is_none());
        h.get_by_label_contains(
            "No profile can be made until the profiles file can be read in full",
        );
        h.get_by_label_contains("those read can still be used");
        // Nothing read: nothing said to be usable.
        let nothing = klefki_core::api::Profiles {
            problem: Some("profiles.json is from a newer Klefki.".into()),
            ..klefki_core::api::Profiles::default()
        };
        engine
            .events
            .send(klefki_core::api::Event::Profiles(nothing))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("can still be used").is_none());
        // The variant: read in full, the editor opens.
        let (mut h, engine) = profiles_harness();
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, command::Action::NewProfile);
        engine
            .events
            .send(klefki_core::api::Event::Profiles(work_profile()))
            .unwrap();
        h.run();
        h.get_by_label("Save profile");
    }

    /// Found in review of PR #58: "Use profile Work…" picked while a profile is being edited left the request
    /// waiting, and focus jumped to Work's "Use in" once the edit was done.
    #[test]
    fn use_from_the_palette_while_editing_says_so_and_moves_no_focus_later() {
        let (mut h, engine) = profiles_harness();
        open_profiles(&mut h, &engine, work_profile());
        h.get_by_label_contains("New profile").click();
        h.run();
        let ctx = h.ctx.clone();
        h.state_mut()
            .run(&ctx, command::Action::UseProfile("Work".into()));
        h.run();
        h.get_by_label_contains("Finish or cancel the profile you are editing first");
        click(&mut h, "Cancel");
        h.run();
        let focused = h.query_all_by(|n| n.is_focused()).count();
        assert_eq!(focused, 0, "focus moved with nobody asking");
        // The variant: a profile that is no longer there is not waited for.
        h.state_mut()
            .run(&ctx, command::Action::UseProfile("Gone".into()));
        h.run();
        let mut later = work_profile();
        later.profiles[0].name = "Gone".into();
        engine
            .events
            .send(klefki_core::api::Event::Profiles(later))
            .unwrap();
        h.run();
        h.run();
        assert_eq!(h.query_all_by(|n| n.is_focused()).count(), 0);
    }

    #[test]
    fn the_editor_says_which_servers_will_not_load() {
        let (mut h, engine) = live_harness();
        let mut blocked = live_row("notion", Scope::User, None);
        blocked.not_loaded = Some("Your organisation's policy denies it.".to_owned());
        show_rows(&mut h, &engine, vec![blocked]);
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        h.get_by_label_contains("New profile").click();
        h.run();
        h.get_by_label("not loaded");
    }

    /// Pictures of every screen and dialog: a refactor that changes the look fails (see `docs/quality.md`).
    #[cfg(target_os = "linux")]
    mod look;

    mod add;
    mod casing;
    mod debug_report;
    mod detail;
    mod duplicate;
    mod editor;
    mod export;
    mod inspector;
    mod last_starts;
    mod list;
    mod move_dialog;
    mod notices;
    mod outcome;
    mod review;
    mod settings;
    mod toolbar;
    mod top_bar;
}

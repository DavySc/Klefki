//! The desktop-style shell: left nav, top bar, icon toolbar, grid and detail panel.
//! Pure UI over an immutable `Snapshot`; no IO.
use crate::command::{self, CommandPalette, Entry, shortcut};
use crate::explain::Explanation;
use crate::sign_in::{self, SignInView};
use crate::theme::{self, Palette, ThemeChoice, ThemeOption, Tone};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Approval, Change, Command, ConnectionId, CopyFields, Core, Definition, Diagnosis, Event,
    Exported, FileChoice, Hint, ImportFile, LegacyDisabled, Part, Pasted, PastedServer, PlanView,
    SaveError, Scope, Secret, ServerFields, ServerId, ServerName, ServerRow, Snapshot, Status,
    Target, TransportKind, UpdateCheck, UpdateDownload, same_project, secret_reference,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
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

    pub fn title(self) -> &'static str {
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
pub enum DetailTab {
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

/// The definition being edited, for one server: in the form, or as JSON.
struct Editor {
    id: ServerId,
    state: EditorState,
    /// Asked for with Edit: brought into view when it arrives, for it is drawn below the summary, which can leave it
    /// under the save bar.
    reveal: bool,
    /// Its server was moved or renamed under a draft: the definition is read again from the new place, and the draft
    /// carried onto it ([`Definition::rebase`]).
    moved: bool,
}

enum EditorState {
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
    fn ready(text: String, editable: bool) -> Self {
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
    fn edited(&mut self) -> bool {
        self.sync();
        matches!(self, Self::Ready { text, original, .. } if text != original)
    }

    /// Brings the form and the JSON in line after either was edited: an edited JSON text is read into the form,
    /// an edited form is written out as JSON. Comparisons only, unless something changed.
    fn sync(&mut self) {
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

/// What happened to the last change, for the banner.
#[derive(Debug, Clone, PartialEq)]
struct Outcome {
    text: String,
    ok: bool,
    /// When it was first shown (egui time, seconds). A success fades after a few seconds; a problem stays.
    shown_at: Option<f64>,
    /// A server just added that signs in: the card offers to sign in to it now, and stays until used or dismissed.
    sign_in: Option<ServerId>,
    /// It worked, with something to take in (settings Claude Code left out, say): drawn as a warning, and it stays.
    warn: bool,
    /// What undoes it (a move back), offered on the card, which then stays until used or dismissed.
    undo: Option<Change>,
    /// The change is in the Activity log, with its backup: the card offers to open it.
    logged: bool,
    /// No file came of asking for one to import: the card offers to type its path instead.
    type_path: bool,
}

/// How long a success message stays.
const SUCCESS_SECONDS: f64 = 4.0;

/// The outcome message: its text's widest, its narrowest beside the detail panel, and its distance from the
/// window's edges.
const TOAST_WIDTH: f32 = 420.0;
const TOAST_NARROWEST: f32 = 280.0;
const TOAST_MARGIN: f32 = 16.0;

/// What was chosen on the outcome message.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OutcomeChoice {
    SignIn,
    Undo,
    OpenLog,
    TypePath,
    Dismiss,
}

/// The size Klefki opens at.
pub const OPENING_SIZE: egui::Vec2 = egui::vec2(1200.0, 720.0);
/// The smallest it can be made; also the room the opening size gives at 150 % zoom.
pub const SMALLEST_SIZE: egui::Vec2 = egui::vec2(800.0, 480.0);

enum GridRow {
    Group(Group),
    Server(usize),
}

/// A group header: the scope and, for a project, its folder name up front and the rest of the path muted.
struct Group {
    title: String,
    /// The folder the project is in, shown quietly after the title.
    location: Option<String>,
    /// The whole label, as one line: what assistive technology reads and what the tooltip shows.
    full: String,
    count: usize,
}

pub struct ShellApp {
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
    /// A diagnosis that came in for the server shown on its Status tab, to bring into view once drawn.
    reveal_diagnosis: Option<ServerId>,
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
    pub fn new(snapshot: Arc<Snapshot>, theme: ThemeChoice) -> Self {
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
            reveal_diagnosis: None,
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
    pub fn with_core(core: Core, theme: ThemeChoice) -> Self {
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
            Event::Snapshot(snapshot) => {
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
                self.selected = selected;
                self.live = live;
                // Positions now mean other servers: tell the engine again.
                self.on_screen.clear();
                if let Some(e) = &mut self.editor
                    && !new_index.contains_key(&e.id.connection)
                {
                    // Gone from where it was: moved or renamed by the change being applied, it is followed there.
                    let landed = self
                        .relocating
                        .iter()
                        .filter(|_| self.applying)
                        .find(|(id, ..)| *id == e.id)
                        .and_then(|(_, name, to)| {
                            snapshot.servers.iter().position(|r| lands_at(r, name, to))
                        });
                    match landed {
                        Some(i) => {
                            e.id = snapshot.servers[i].id();
                            e.moved = true;
                            self.selected = BTreeSet::from([i]);
                            self.reveal = Some(i);
                        }
                        None => self.editor = None,
                    }
                }
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
            Event::PlanReady(plan) => {
                self.preparing = false;
                let saves_draft = std::mem::take(&mut self.draft_asked);
                let profile = self.profile_asked.take();
                // Said on the profile's card while the page is shown; once it is left, the review says it.
                let card = profile
                    .filter(|_| plan.nothing_to_do && self.plan.is_none())
                    .and_then(|name| Some((name, self.profiles.as_mut()?)));
                if let Some((name, page)) = card {
                    page.unchanged(&name, plan.already, plan.warnings, plan.others_on);
                    self.send(Command::Discard(plan.id));
                } else if self.plan.is_some() {
                    // Only one dialog at a time; the one open (perhaps being saved) keeps it.
                    self.send(Command::Discard(plan.id));
                } else {
                    self.draft_plan = saves_draft.then_some(plan.id);
                    self.plan = Some(Arc::new(plan));
                }
            }
            Event::PlanFailed(why) => {
                self.preparing = false;
                self.draft_asked = false;
                self.profile_asked = None;
                self.adding.clear();
                match self.add.as_mut().filter(|f| f.reviewing) {
                    // Said in the Add dialog, which comes back as it was, so nothing typed is lost.
                    Some(form) => {
                        form.reviewing = false;
                        form.failed = Some(why);
                    }
                    None => {
                        self.outcome = Some(Outcome {
                            text: why,
                            ok: false,
                            shown_at: None,
                            warn: false,
                            undo: None,
                            sign_in: None,
                            logged: false,
                            type_path: false,
                        })
                    }
                }
            }
            Event::Applied {
                plan,
                changed,
                backup,
                notes,
            } => {
                if changed {
                    // Read again what was saved, for the server still being edited; drop it for any other.
                    let saved_draft = self.draft_plan.take() == Some(plan);
                    let open = self
                        .editor
                        .take()
                        .filter(|e| self.single_selected().is_some_and(|s| s.is(&e.id)));
                    if let Some(mut e) = open {
                        if !saved_draft && e.state.edited() {
                            // Edits not saved yet stay: what was applied was another change. Moved or renamed,
                            // what the new place holds is read, to compare them with and revert to.
                            if e.moved {
                                self.send(Command::Inspect(e.id.clone()));
                            }
                            self.editor = Some(e);
                        } else {
                            self.editor = Some(Editor {
                                id: e.id.clone(),
                                state: EditorState::Loading,
                                reveal: false,
                                moved: false,
                            });
                            self.send(Command::Inspect(e.id));
                        }
                    }
                }
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.plan = None;
                    self.applying = false;
                    self.applying_step = None;
                }
                let added = self.added_in_words();
                if self.add.as_ref().is_some_and(|f| f.reviewing) {
                    self.close_add();
                }
                let signs_in = std::mem::take(&mut self.adding_signs_in) && added.is_some();
                // The list already holds what was saved: the engine sends it first.
                self.select_added();
                // One server added in one place: offer to sign in to it right here.
                let sign_in = self
                    .single_selected()
                    .filter(|_| signs_in && changed)
                    .filter(|row| sign_in::can_sign_in(row))
                    .map(ServerRow::id);
                let next = match (&sign_in, signs_in) {
                    (Some(_), _) => " Sign in now, or later from its Status tab.",
                    (None, true) if self.selected.len() > 1 => {
                        " Sign in to each from its Status tab."
                    }
                    (None, true) => " Sign in to it from its Status tab.",
                    (None, false) => "",
                };
                // How it went, when there is more to say (settings Claude Code left out, say), then the backup.
                let warn = changed && !notes.is_empty();
                let mut kept: String = notes.iter().map(|n| format!(" {n}")).collect();
                if let Some(b) = backup {
                    kept.push_str(&format!(" The previous file is kept in {b}"));
                }
                self.outcome = Some(Outcome {
                    // What was saved is read again next time it is shown.
                    text: match (changed, added) {
                        (false, _) => "Nothing needed to change.".to_owned(),
                        (true, None) if let Some(gone) = self.forgetting.take() => {
                            format!("Klefki no longer keeps the definition of {gone}.{kept}")
                        }
                        (true, Some(added)) => format!("{added}{next}{kept}"),
                        (true, None) if kept.is_empty() => "Saved.".to_owned(),
                        (true, None) => format!("Saved.{kept}"),
                    },
                    ok: true,
                    shown_at: None,
                    warn,
                    undo: self.undo_move(changed),
                    sign_in,
                    logged: changed,
                    type_path: false,
                });
            }
            Event::Applying { plan, step } => {
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.applying_step = Some(step);
                }
            }
            Event::ApplyFailed { plan, error } => {
                self.moved_from = None;
                if self.plan.as_ref().is_some_and(|p| p.id == plan) {
                    self.plan = None;
                    self.applying = false;
                    self.applying_step = None;
                }
                // Claude Code may have added some before one failed.
                self.select_added();
                if let Some(form) = self.add.as_mut().filter(|f| f.reviewing) {
                    // Said in the Add dialog, which comes back as it was, to try again or cancel.
                    form.reviewing = false;
                    form.failed = Some(error);
                    return;
                }
                self.outcome = Some(Outcome {
                    text: error,
                    ok: false,
                    shown_at: None,
                    warn: false,
                    undo: None,
                    sign_in: None,
                    logged: false,
                    type_path: false,
                });
            }
            Event::Reapplied { .. } => {
                self.outcome = Some(Outcome {
                    text: "Claude Code overwrote the change right after it was saved, so it was written again. Close running Claude Code sessions if this keeps happening."
                        .to_owned(),
                    ok: false,
                    shown_at: None,
                    warn: false,
                    undo: None,
                    sign_in: None,
                    logged: false,
                    type_path: false,
                });
            }
            Event::Entry { id, json, editable } => {
                if let Some(form) = &mut self.duplicate
                    && form.source == id
                    && form.copy == CopyRead::Reading
                {
                    form.copy = match &json {
                        Ok(text) => CopyFields::from_definition(text)
                            .map_or(CopyRead::NoAddress, |f| CopyRead::Ready(Box::new(f))),
                        Err(why) => CopyRead::Failed(why.clone()),
                    };
                }
                // Brought into view for the server shown on a tab with the form; one looked at later opens as it is.
                let shown = matches!(self.tab, DetailTab::General | DetailTab::HeadersEnv)
                    && self.single_selected().is_some_and(|s| s.is(&id));
                if let Some(editor) = &mut self.editor
                    && editor.id == id
                {
                    match (&mut editor.state, json) {
                        (EditorState::Loading, json) => {
                            editor.state = match json {
                                Ok(text) => EditorState::ready(text, editable),
                                Err(why) => EditorState::Failed(why),
                            };
                            editor.reveal &= shown;
                        }
                        // Read again after a move under a draft: the draft is carried onto what is there now.
                        (
                            EditorState::Ready {
                                original,
                                text,
                                editable: can_edit,
                                ..
                            },
                            Ok(now),
                        ) if editor.moved => {
                            if let Some(rebased) = Definition::rebase(original, &now, text) {
                                *text = rebased;
                            }
                            *original = now;
                            *can_edit = editable;
                        }
                        _ => {}
                    }
                    editor.moved = false;
                }
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
            Event::Exported(exported) => {
                self.exports_pending = self.exports_pending.saturating_sub(1);
                if let Some(view) = &mut self.exporting
                    && self.exports_pending == 0
                {
                    view.result = Some(exported);
                }
            }
            Event::ExportFileChosen(choice) => {
                self.export_choices_pending = self.export_choices_pending.saturating_sub(1);
                if let Some(view) = &mut self.exporting
                    && self.export_choices_pending == 0
                    && view.save == Saving::Choosing
                {
                    view.save = Saving::Idle;
                    match choice {
                        FileChoice::Chosen(path) => {
                            if let Some(result) = &view.result {
                                // The system's dialog asked before replacing a file there.
                                let text = result.text.clone();
                                view.save = Saving::Writing {
                                    path: path.clone(),
                                    typed: None,
                                };
                                self.send(Command::SaveExport {
                                    path,
                                    text,
                                    replace: true,
                                });
                            }
                        }
                        // A dialog that failed slowly looks like a cancel: the way to type a path stays in reach.
                        FileChoice::Cancelled { suggested } => {
                            view.save = Saving::Offered {
                                path: suggested.to_string_lossy().into_owned(),
                            };
                        }
                        FileChoice::NoDialog { suggested } => {
                            view.save = Saving::Typing {
                                path: suggested.to_string_lossy().into_owned(),
                                focus: true,
                                no_dialog: true,
                            };
                        }
                    }
                }
            }
            Event::ExportSaved {
                path,
                shown,
                result,
            } => {
                if let Some(view) = &mut self.exporting
                    && let Saving::Writing { path: asked, typed } = &view.save
                    && *asked == path
                {
                    let typed = typed.clone();
                    view.save = Saving::Idle;
                    match result {
                        Ok(()) => view.status = Some(Ok(format!("Saved to {shown}"))),
                        Err(SaveError::Exists) => {
                            view.status = None;
                            view.save = Saving::Exists {
                                path,
                                typed: typed.unwrap_or_default(),
                            };
                        }
                        Err(why) => {
                            view.status = Some(Err(format!("Not saved: {why}")));
                            // A typed path stays, to be put right.
                            if let Some(path) = typed {
                                view.save = Saving::Typing {
                                    path,
                                    focus: false,
                                    no_dialog: false,
                                };
                            }
                        }
                    }
                }
            }
            Event::CallbackPort { port, warning } => {
                if let Some(form) = &mut self.add {
                    if form.port_pending == Some(port) {
                        form.port_pending = None;
                    }
                    form.port_checked = Some((port, warning.clone()));
                }
                if let Some(form) = &mut self.duplicate {
                    if form.port_pending == Some(port) {
                        form.port_pending = None;
                    }
                    form.port_checked = Some((port, warning));
                }
            }
            Event::SecretStatus { id, stored } => {
                if let Some(view) = &mut self.secret
                    && view.id == id
                {
                    view.known = Some(stored);
                    view.busy = None;
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
            } => {
                if let Some(i) = self.snapshot.servers.iter().position(|r| r.is(&id)) {
                    self.live[i] = Some(Live {
                        status,
                        detail,
                        hint,
                    });
                    self.live_gen += 1;
                }
            }
        }
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

    /// Indices into the snapshot matching the search, ordered by scope, project and name.
    pub fn visible(&self) -> Vec<usize> {
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
    fn cached_grid_rows(&mut self) -> Arc<[GridRow]> {
        match &self.grid_cache {
            Some((search, rows)) if *search == self.search => Arc::clone(rows),
            _ => {
                let rows: Arc<[GridRow]> = self.grid_rows().into();
                self.grid_cache = Some((self.search.clone(), Arc::clone(&rows)));
                rows
            }
        }
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

    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(icon::KEY).size(17.0).color(pal.key));
            ui.label(
                RichText::new("Klefki")
                    .family(theme::bold_family())
                    .size(18.0)
                    .color(pal.primary),
            );
            ui.label(RichText::new(self.page.title()).color(pal.text_muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(icon::ARROWS_CLOCKWISE)
                    .on_hover_text("Refresh and re-check all (F5)")
                    .clicked()
                {
                    self.refresh();
                }
                if let Some(version) = self.updates.available() {
                    self.update_button(ui, version, pal.primary);
                } else if let Some(version) = self.updates.updated
                    && ui
                        .button(
                            RichText::new(format!(
                                "{} Updated to {version} · What's new",
                                icon::CHECK_CIRCLE
                            ))
                            .color(pal.primary),
                        )
                        .on_hover_text("Opens the release's page on GitHub.")
                        .clicked()
                {
                    self.updates.updated = None;
                    self.send(Command::OpenReleasePage(version));
                }
                let current = ThemeOption::new(self.theme, self.tone);
                let mut picked = None;
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(format!("{} {}", current.icon(), current.label()))
                    .width(140.0)
                    // All six and the separator fit without a scroll bar (the default is 200 px).
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        flat_items(ui);
                        let focus = pal.focus;
                        for (i, option) in ThemeOption::ALL.into_iter().enumerate() {
                            if i > 0 && option.tone != ThemeOption::ALL[i - 1].tone {
                                ui.separator();
                            }
                            let mut text = format!("{} {}", option.icon(), option.label());
                            if option == current {
                                // The fill alone is too faint to tell the current entry (WCAG 1.4.11).
                                text = format!("{text}  {}", icon::CHECK);
                            }
                            let response = ui.selectable_label(option == current, text);
                            if response.has_focus() {
                                // Focus must not look like the current entry's fill (WCAG 2.4.7).
                                ui.painter().rect_stroke(
                                    response.rect,
                                    ui.visuals().widgets.active.corner_radius,
                                    egui::Stroke::new(1.5, focus),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if response.clicked() {
                                picked = Some(option);
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Theme");
                if let Some(option) = picked {
                    self.theme = option.choice;
                    self.tone = option.tone;
                    theme::set_choice(ui.ctx(), option.choice);
                    theme::set_tone(ui.ctx(), option.tone);
                }
                if ui
                    .button(
                        RichText::new(format!("{} {}", icon::COMMAND, shortcut("K")))
                            .color(pal.text_muted),
                    )
                    .on_hover_text("Commands: everything Klefki can do, by name")
                    .clicked()
                {
                    self.command.open();
                }
                // On the Activity log it filters the changes, and says so: a box that filtered the hidden list of
                // servers would find nothing there.
                // The shortcut is left out there, so the longer words fit the box.
                let (query, hint) = if let Some(page) = &mut self.activity {
                    (
                        &mut page.filter,
                        format!("{} Filter changes", icon::MAGNIFYING_GLASS),
                    )
                } else if let Some(page) = &mut self.secrets {
                    (
                        &mut page.filter,
                        format!("{} Filter secrets", icon::MAGNIFYING_GLASS),
                    )
                } else if let Some(page) = &mut self.desktop {
                    (
                        &mut page.filter,
                        format!("{} Filter servers", icon::MAGNIFYING_GLASS),
                    )
                } else if let Some(page) = &mut self.profiles {
                    (
                        &mut page.filter,
                        format!("{} Filter profiles", icon::MAGNIFYING_GLASS),
                    )
                } else {
                    (
                        &mut self.search,
                        format!("{} Filter  {}", icon::MAGNIFYING_GLASS, shortcut("F")),
                    )
                };
                ui.add(
                    egui::TextEdit::singleline(query)
                        .id(search_id())
                        .hint_text(hint)
                        .desired_width(220.0),
                );
                if let Some(version) = self.shown_cli_version() {
                    // Only where it fits: in a narrow window it would be drawn over the title. Where the whole
                    // of it does not, the number alone, named in full on hover.
                    let full = format!("Claude CLI {version}");
                    let fitting = [full.as_str(), version].into_iter().find_map(|text| {
                        let galley =
                            egui::WidgetText::from(RichText::new(text).color(pal.text_muted))
                                .into_galley(
                                    ui,
                                    Some(egui::TextWrapMode::Extend),
                                    f32::INFINITY,
                                    egui::TextStyle::Body,
                                );
                        (galley.size().x <= ui.available_width()).then_some(galley)
                    });
                    if let Some(galley) = fitting {
                        ui.label(galley)
                            .on_hover_text(format!("Claude Code {version}"));
                    }
                }
            });
        });
    }

    /// Starts the automatic check for updates once the window has been open a moment, and wakes up for it.
    fn look_for_updates(&mut self, ctx: &egui::Context) {
        if !self.checks_for_updates || self.core.is_none() {
            return;
        }
        let uptime = ctx.input(|i| i.time);
        if self.updates.due(uptime, crate::updates::now()) {
            self.send(Command::CheckForUpdates);
        } else if self.updates.auto && uptime < crate::updates::START_DELAY {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                crate::updates::START_DELAY - uptime,
            ));
        }
    }

    /// The top bar's way to the update found: download it, then restart into it. Installs updated by hand open the
    /// release's page instead.
    fn update_button(&mut self, ui: &mut Ui, version: klefki_core::api::Version, color: Color32) {
        let label =
            |text: String| RichText::new(format!("{} {text}", icon::ARROW_CIRCLE_UP)).color(color);
        if let Some(why) = &self.updated_by_hand {
            let hover = format!(
                "Klefki {version} is out. Opens its page on GitHub, with what is new and the downloads. {why}"
            );
            if ui
                .button(label(format!("Update: {version}")))
                .on_hover_text(hover)
                .clicked()
            {
                self.send(Command::OpenReleasePage(version));
            }
        } else if self.updates.downloading {
            ui.add_enabled(
                false,
                egui::Button::new(label(format!("Downloading {version}…"))),
            );
        } else if self.updates.ready() == Some(version) {
            let busy = self.change_open();
            let response = ui
                .add_enabled(!busy, egui::Button::new(label("Restart to update".into())))
                .on_hover_text(format!(
                    "Installs Klefki {version} and starts it again. Otherwise it installs when Klefki closes."
                ))
                .on_disabled_hover_text(format!(
                    "Finish the open change first. Klefki {version} also installs when Klefki closes."
                ));
            if response.clicked() {
                self.restart_to_update(ui.ctx());
            }
        } else {
            let hover = match self.updates.download_failed() {
                Some(why) => format!("{why} Click to try again."),
                None => {
                    format!("Klefki {version} is out. Downloads it; you choose when to restart.")
                }
            };
            if ui
                .button(label(format!("Update: {version}")))
                .on_hover_text(hover)
                .clicked()
            {
                self.download_update();
            }
        }
    }

    fn download_update(&mut self) {
        self.updates.downloading();
        self.send(Command::DownloadUpdate);
    }

    /// Installs the downloaded update and closes Klefki, which the new version replaces.
    fn restart_to_update(&mut self, ctx: &egui::Context) {
        let Some(core) = &self.core else {
            return;
        };
        match core.install_update(true) {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(why) => self.updates.downloaded(UpdateDownload::Failed(why)),
        }
    }

    /// A change is being made or reviewed: restarting now would lose it.
    fn change_open(&self) -> bool {
        self.plan.is_some()
            || self.applying
            || self.preparing
            || self.editor.is_some()
            || self.add.is_some()
            || self.duplicate.is_some()
            || self.moving.is_some()
            || self.sign_in.is_some()
            || self.secret.is_some()
            || self.secret_input.is_some()
            || self.casing.is_some()
            || self.forgetting.is_some()
            || self.exporting.is_some()
            || self.import_waiting
    }

    fn check_for_updates_now(&mut self) {
        self.updates.check_now();
        self.send(Command::CheckForUpdates);
    }

    /// Claude Code's version, when it is known. Not while Claude Code cannot be found: the version would be of a
    /// program that is gone.
    fn shown_cli_version(&self) -> Option<&str> {
        self.cli_version
            .as_deref()
            .or(self.snapshot.cli_version.as_deref())
            .filter(|_| self.cli_problem.is_none())
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

    /// Projects with the switch-off list of older Claude Code versions: what is on because Claude Code no longer
    /// reads it, and a button that moves it into the list it does read. Read-only until the user reviews the plan.
    fn legacy_notice(&mut self, ui: &mut Ui) {
        if self.snapshot.legacy_disabled.is_empty() {
            return;
        }
        // Built once per snapshot, not every frame.
        let notice = match &self.legacy_cache {
            Some((for_snapshot, built)) if Arc::ptr_eq(for_snapshot, &self.snapshot) => {
                built.clone()
            }
            _ => {
                let built = Arc::new(LegacyNotice::new(&self.snapshot.legacy_disabled));
                self.legacy_cache = Some((self.snapshot.clone(), built.clone()));
                built
            }
        };
        let pal = theme::palette(ui.ctx());
        let can_migrate = self.core.is_some() && self.can_prepare() && notice.migratable;
        let mut migrate = false;
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(notice.title.as_str()).color(pal.warn));
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Older Claude Code versions kept switched-off servers in mcpServersDisabled. Claude Code no longer reads that list, so servers on it can be on again. Migrating moves them to the list it reads now.",
                        )
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
                for (line, path) in &notice.lines {
                    ui.add(egui::Label::new(line.as_str()).wrap())
                        .on_hover_text(path.as_str());
                }
                if let Some(more) = &notice.more {
                    ui.label(RichText::new(more.as_str()).color(pal.text_muted));
                }
                migrate = ui
                    .add_enabled(can_migrate, egui::Button::new("Migrate…"))
                    .on_hover_text("Review the change before anything is written")
                    .on_disabled_hover_text(if self.core.is_none() {
                        "Not connected to the configuration"
                    } else if !notice.migratable {
                        "Only lists of server names can be migrated"
                    } else {
                        "Wait for the open change to finish"
                    })
                    .clicked();
            });
        ui.add_space(6.0);
        if migrate {
            self.migrate_legacy();
        }
    }

    /// Servers a change took out and could not put back (Claude Code's command failed twice): Klefki kept their
    /// definitions, and offers to add each back where it was, or to forget it.
    fn kept_notice(&mut self, ui: &mut Ui) {
        if self.snapshot.kept.is_empty() {
            return;
        }
        let pal = theme::palette(ui.ctx());
        let can = self.core.is_some() && self.can_prepare();
        let mut chosen = None;
        let out = self.snapshot.kept.iter().filter(|k| !k.back).count();
        Frame::new()
            .stroke(egui::Stroke::new(1.0, pal.warn))
            .corner_radius(6)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (title, body) = match out {
                    0 => (
                        "Klefki keeps settings Claude Code left out".to_owned(),
                        "The server is back, but Claude Code did not keep all of its settings. Klefki keeps the whole definition until you forget it.",
                    ),
                    1 => (
                        "A server is out of the configuration".to_owned(),
                        "A change took it out and Claude Code could not put it back. Klefki kept its definition, with its client secret, so you can add it back where it was.",
                    ),
                    n => (
                        format!("{n} servers are out of the configuration"),
                        "A change took them out and Claude Code could not put them back. Klefki kept each definition, with its client secret, so you can add it back where it was.",
                    ),
                };
                ui.label(RichText::new(title).color(pal.warn));
                ui.add(egui::Label::new(RichText::new(body).color(pal.text_muted)).wrap());
                let wait = "Wait for the open change to finish";
                for kept in &self.snapshot.kept {
                    ui.horizontal(|ui| {
                        // The server first, then what can be done with it.
                        let mut what = format!("{} {}", kept.name, kept.place);
                        if kept.back {
                            what.push_str(" is back without some settings");
                        } else if kept.signs_in {
                            what.push_str(" (signs in with OAuth)");
                        }
                        ui.add(egui::Label::new(what).truncate());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(can, egui::Button::new("Forget\u{2026}"))
                                .on_hover_text("Delete Klefki's copy; the review says what is lost")
                                .on_disabled_hover_text(wait)
                                .clicked()
                            {
                                chosen = Some(Change::ForgetKept {
                                    kept: kept.id.clone(),
                                });
                            }
                            // Back already: only its fuller definition is left to forget.
                            if !kept.back
                                && ui
                                .add_enabled(can, egui::Button::new("Put back"))
                                .on_hover_text("Add it back where it was, after a review")
                                .on_disabled_hover_text(wait)
                                .clicked()
                            {
                                chosen = Some(Change::PutBack {
                                    kept: kept.id.clone(),
                                });
                            }
                        });
                    });
                }
            });
        ui.add_space(6.0);
        if let Some(change) = chosen {
            self.prepare(vec![change]);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
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
            for (glyph, tip) in ACTIONS {
                let tip = if glyph == icon::EXPORT {
                    export_tip
                } else {
                    tip
                };
                let button = egui::Button::new(RichText::new(glyph).size(16.0))
                    .min_size(egui::vec2(44.0, 38.0));
                let (enabled, needs) = match glyph {
                    _ if read_only
                        && ![
                            icon::PLUS,
                            icon::TRAY_ARROW_DOWN,
                            icon::TOGGLE_RIGHT,
                            icon::TOGGLE_LEFT,
                        ]
                        .contains(&glyph) =>
                    {
                        (false, read_only_hint.as_deref().unwrap_or_default())
                    }
                    g if g == icon::TOGGLE_RIGHT || g == icon::TOGGLE_LEFT || g == icon::TRASH => {
                        (can_toggle, "select a server first")
                    }
                    g if g == icon::COPY || g == icon::ARROW_FAT_LINES_RIGHT => {
                        (can_duplicate, "select one user or local server")
                    }
                    g if g == icon::SIGN_IN => {
                        (can_sign_in, "select one server that needs you to sign in")
                    }
                    g if g == icon::PLUS => (can_add, "wait for the change under way"),
                    g if g == icon::TRAY_ARROW_DOWN => (
                        can_add && !self.import_waiting,
                        "wait for the change under way",
                    ),
                    g if g == icon::EXPORT => (can_export, "no server to export"),
                    _ => (can_edit, "select one server"),
                };
                let clicked = ui
                    .add_enabled(enabled, button)
                    .on_hover_text(tip)
                    .on_disabled_hover_text(format!("{tip}: {needs}"))
                    .clicked();
                if clicked {
                    match glyph {
                        g if g == icon::TOGGLE_RIGHT => self.prepare_toggle(true),
                        g if g == icon::TOGGLE_LEFT => self.prepare_toggle(false),
                        g if g == icon::TRASH => self.prepare_delete(),
                        g if g == icon::COPY => self.open_duplicate(),
                        g if g == icon::ARROW_FAT_LINES_RIGHT => self.open_move(),
                        g if g == icon::PLUS => self.open_add(),
                        g if g == icon::TRAY_ARROW_DOWN => self.ask_import(),
                        g if g == icon::EXPORT => self.open_export(),
                        g if g == icon::SIGN_IN => {
                            if let Some(s) = self.single_selected().cloned() {
                                self.start_sign_in(&s);
                            }
                        }
                        _ => self.edit_selected(),
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
                if ui.available_width() >= needed {
                    ui.separator();
                    ui.add(egui::Label::new(text).extend());
                }
            }
        });
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

    /// Asks for one plan that moves every old switch-off list that can be moved.
    fn migrate_legacy(&mut self) {
        let changes: Vec<Change> = self
            .snapshot
            .legacy_disabled
            .iter()
            .filter(|l| !l.malformed)
            .map(|l| Change::MigrateLegacyDisabled {
                project: l.project.clone(),
            })
            .collect();
        if self.core.is_some() && !changes.is_empty() {
            self.prepare(changes);
        }
    }

    /// The one selected server, if it can be duplicated (project servers cannot yet).
    fn duplicable(&self) -> Option<&ServerRow> {
        self.core.as_ref()?;
        if !self.can_prepare() {
            return None;
        }
        self.single_selected()
            .filter(|s| matches!(s.scope, Scope::User | Scope::Local))
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

    /// Opens the Duplicate dialog for the selected server: named `<name>_copy`, in the place it is now.
    fn open_duplicate(&mut self) {
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
        });
        if remote {
            self.send(Command::Inspect(source));
        }
    }

    /// Opens the Move dialog for the selected server, with nowhere chosen yet: where it is now cannot be chosen.
    fn open_move(&mut self) {
        let Some(row) = self.duplicable() else {
            return;
        };
        let mut places = Places::new(&self.snapshot.projects, row.project.as_deref());
        places.one = true;
        places.now = Some(match (&row.project, row.scope) {
            (Some(p), Scope::Local) => Target::Local(p.clone()),
            _ => Target::User,
        });
        self.moving = Some(MoveForm {
            source: row.id(),
            about: format!("{} \u{b7} {}", row.transport.label(), row.target),
            places,
            signs_in: row.oauth.is_some(),
        });
    }

    /// Opens the Fix casing dialog for the selected server: every server that differs from it only in case.
    fn open_casing(&mut self) {
        if self.core.is_none() || !self.can_prepare() {
            return;
        }
        let Some(row) = self.single_selected().filter(|r| r.casing_collision) else {
            return;
        };
        let lower = row.name.to_lowercase();
        let members: Vec<CasingMember> = self
            .snapshot
            .servers
            .iter()
            .filter(|r| r.casing_collision && r.name.to_lowercase() == lower)
            .map(|r| CasingMember {
                id: r.id(),
                name: r.name.clone(),
                scope: r.scope,
                project: r.project.clone(),
            })
            .collect();
        let mut form = CasingForm {
            members,
            keep: row.name.clone(),
        };
        // A project server cannot change its name, so its spelling is the one to keep.
        if let [only] = form.fixed()[..] {
            form.keep = only.to_owned();
        }
        self.casing = Some(form);
    }

    /// The rows Export takes, in the list's order: the selection (all of it, even rows the search hides now), or
    /// every row the list shows when nothing is selected.
    fn export_rows(&self) -> Vec<usize> {
        let rows = if self.selected.is_empty() {
            self.visible()
        } else {
            self.in_list_order(self.selected.iter().copied().collect())
        };
        // A plugin's servers come with the plugin; Klefki does not hold their definitions.
        rows.into_iter()
            .filter(|&i| !self.snapshot.servers[i].scope.is_read_only())
            .collect()
    }

    /// Whether Export has anything to take; cheap enough for every frame (no sorting, no list built).
    fn can_export(&self) -> bool {
        let q = self.search.trim().to_lowercase();
        self.core.is_some()
            && if self.selected.is_empty() {
                self.snapshot
                    .servers
                    .iter()
                    .any(|s| !s.scope.is_read_only() && matches_search(s, &q))
            } else {
                !self.changeable_selected().0.is_empty()
            }
    }

    /// Opens the Export dialog and asks the engine for the block.
    fn open_export(&mut self) {
        let rows = self.export_rows();
        if self.core.is_none() || rows.is_empty() || self.exporting.is_some() {
            return;
        }
        let servers = &self.snapshot.servers;
        let what = match rows.as_slice() {
            [one] => servers[*one].name.clone(),
            _ => format!("{} servers", rows.len()),
        };
        let names: Vec<&str> = rows.iter().map(|&i| servers[i].name.as_str()).collect();
        // Which rows the search decided, said plainly: the two cases work differently.
        let search = self.search.trim();
        let scope = if search.is_empty() {
            None
        } else if self.selected.is_empty() {
            Some(format!(
                "Only the servers that match \u{201c}{search}\u{201d}."
            ))
        } else {
            let shown = self.visible();
            match rows.iter().filter(|i| !shown.contains(i)).count() {
                0 => None,
                1 => Some("Includes 1 selected server the search hides.".to_owned()),
                n => Some(format!("Includes {n} selected servers the search hides.")),
            }
        };
        // A plugin's servers come with the plugin: said, not dropped silently.
        let plugins = match self.changeable_selected().1 {
            0 => None,
            1 => Some("Leaves out 1 selected plugin server: it comes with its plugin.".to_owned()),
            n => Some(format!(
                "Leaves out {n} selected plugin servers: they come with their plugins."
            )),
        };
        let scope = match (scope, plugins) {
            (Some(a), Some(b)) => Some(format!("{a} {b}")),
            (a, b) => a.or(b),
        };
        self.exporting = Some(ExportView {
            file_name: klefki_core::api::export_file_name(&names),
            save: Saving::Idle,
            status: None,
            size: egui::Vec2::ZERO,
            status_height: 26.0,
            folded: false,
            what,
            names: (rows.len() > 1).then(|| names_in_a_line(&names)),
            scope,
            result: None,
            copied: None,
        });
        self.exports_pending += 1;
        self.send(Command::Export(
            rows.iter().map(|&i| servers[i].id()).collect(),
        ));
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(view) = &mut self.exporting else {
            return;
        };
        let now = ctx.input(|i| i.time);
        // Enter copies, unless a button has the keyboard: Enter is that button's then (Close closes).
        let mut copy = view.result.is_some()
            && ctx.memory(|m| m.focused().is_none())
            && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut copy_names = false;
        let mut close = false;
        let mut choose = false;
        let mut save_typed = false;
        let mut replace = false;
        let mut keep = false;
        let mut type_instead = false;
        let muted = |ui: &mut Ui, text: &str| {
            ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
        };
        let screen = ctx.content_rect();
        let short = screen.height() < 640.0;
        let modal = egui::Modal::new(egui::Id::new("export_dialog")).show(ctx, |ui| {
            ui.set_width(640.0_f32.min(screen.width() - 48.0));
            ui.heading(format!("Export {}", view.what));
            if let Some(names) = &view.names {
                muted(ui, names);
            }
            if let Some(scope) = &view.scope {
                ui.add(egui::Label::new(RichText::new(scope).color(pal.warn)).wrap());
            }
            ui.add_space(6.0);
            match &view.result {
                // Text, not a spinner: an animation would repaint every frame, and reading takes milliseconds.
                None => {
                    ui.label("Reading the definitions…");
                }
                Some(result) => {
                    let (to_set, defaults): (Vec<_>, Vec<_>) =
                        result.variables.iter().partition(|v| v.default.is_none());
                    if result.variables.is_empty() {
                        muted(ui, "Nothing was left out: no value needed a reference.");
                    } else {
                        let title = match (to_set.len(), defaults.len()) {
                            (0, d) => format!("{d} settings as references, with their values as defaults"),
                            (1, _) => "1 variable to set where you add them".to_owned(),
                            (n, _) => format!("{n} variables to set where you add them"),
                        };
                        // Open when there is room; on a small window the block gets the room first.
                        // Saving's rows take room the block needs on a short window: the list folds, once.
                        let fold = short
                            && !view.folded
                            && (view.status.is_some()
                                || !matches!(view.save, Saving::Idle | Saving::Choosing));
                        view.folded |= fold;
                        egui::CollapsingHeader::new(RichText::new(title).strong())
                            .id_salt("export_variables")
                            .default_open(!short)
                            .open(fold.then_some(false))
                            .show(ui, |ui| {
                                muted(
                                    ui,
                                    "Values that may be secrets are written as ${NAME}; plain settings keep their value as ${NAME:-value}.",
                                );
                                egui::ScrollArea::vertical()
                                    .id_salt("export_variable_list")
                                    .max_height((screen.height() * 0.2).min(132.0))
                                    .show(ui, |ui| {
                                        // Lines of text, not rows of controls: no control height between them.
                                        ui.spacing_mut().interact_size.y = 0.0;
                                        for variable in to_set.iter().chain(&defaults) {
                                            ui.horizontal_wrapped(|ui| {
                                                let name = RichText::new(&variable.name).strong();
                                                ui.label(match &variable.default {
                                                    None => name.color(pal.key),
                                                    Some(_) => name,
                                                });
                                                if let Some(default) = &variable.default {
                                                    ui.label(format!("= {default} (default)"));
                                                }
                                                ui.label(
                                                    RichText::new(variable.replaces.join("; "))
                                                        .color(pal.text_muted),
                                                );
                                            });
                                        }
                                    });
                            });
                    }
                    for note in &result.notes {
                        ui.add(egui::Label::new(RichText::new(note).color(pal.warn)).wrap());
                    }
                    ui.add_space(6.0);
                    // The block takes what is left of the window's height, so the buttons (and what saving says
                    // under them) stay on screen.
                    let used = ui.cursor().top() - ui.min_rect().top();
                    let below = match view.save {
                        Saving::Typing { .. } | Saving::Exists { .. } => 64.0,
                        Saving::Offered { .. } => 32.0,
                        Saving::Writing { typed: Some(_), .. } => 40.0,
                        _ => 0.0,
                    } + if view.status.is_some() {
                        view.status_height
                    } else {
                        0.0
                    };
                    let room = screen.height() - 64.0 - used - 80.0 - below;
                    egui::ScrollArea::vertical()
                        .id_salt("export_block")
                        .max_height(room.clamp(if below > 0.0 { 64.0 } else { 96.0 }, 320.0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut result.text.as_str())
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let copied = |what| {
                    view.copied
                        .is_some_and(|(it, at)| it == what && now - at < COPIED_SECS)
                };
                let label = if copied(Copied::Block) { "Copied" } else { "Copy" };
                let button = egui::Button::new(RichText::new(label).strong().color(pal.on_bright))
                    .fill(pal.primary)
                    .min_size(egui::vec2(96.0, 34.0));
                let copy_button = ui
                    .add_enabled(view.result.is_some(), button)
                    .on_hover_text("Copy the block (Enter)")
                    .on_disabled_hover_text("Reading the definitions…");
                announce(ui.ctx(), copy_button.id);
                copy |= copy_button.clicked();
                let busy = matches!(view.save, Saving::Choosing | Saving::Writing { .. });
                choose |= ui
                    .add_enabled(
                        view.result.is_some() && !busy,
                        egui::Button::new("Save to file…").min_size(egui::vec2(96.0, 34.0)),
                    )
                    .on_hover_text("Save the block as a JSON file")
                    .on_disabled_hover_text(if busy {
                        "Waiting for the save dialog"
                    } else {
                        "Reading the definitions…"
                    })
                    .clicked();
                if view
                    .result
                    .as_ref()
                    .is_some_and(|r| r.variables.iter().any(|v| v.default.is_none()))
                {
                    let label = if copied(Copied::Names) { "Copied" } else { "Copy names" };
                    let names_button = ui
                        .add(egui::Button::new(label).min_size(egui::vec2(80.0, 34.0)))
                        .on_hover_text("Copy the variables to set, one NAME= a line");
                    announce(ui.ctx(), names_button.id);
                    copy_names |= names_button.clicked();
                }
                close |= ui
                    .add(egui::Button::new("Close").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
            // A typed path keeps its row while it is written and while replacing is asked, so nothing jumps.
            let typed_row = match &mut view.save {
                Saving::Typing { path, focus, .. } => Some((path, std::mem::take(focus), true)),
                Saving::Writing {
                    typed: Some(path), ..
                }
                | Saving::Exists { typed: path, .. } => Some((path, false, false)),
                _ => None,
            };
            if let Some((path, focus, editable)) = typed_row {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = ui.label("Save to");
                    let field = ui
                        .add_enabled(
                            editable,
                            egui::TextEdit::singleline(path)
                                .id(egui::Id::new("export_save_path"))
                                .desired_width((ui.available_width() - 96.0).max(120.0))
                                .font(egui::TextStyle::Monospace),
                        )
                        .labelled_by(label.id);
                    if focus {
                        field.request_focus();
                        select_file_stem(ui.ctx(), field.id, path);
                    }
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    save_typed |= ui
                        .add_enabled(
                            editable && !path.trim().is_empty(),
                            egui::Button::new("Save here").min_size(egui::vec2(88.0, 28.0)),
                        )
                        .clicked()
                        || (editable && entered);
                });
            }
            match &view.save {
                Saving::Typing {
                    no_dialog: true, ..
                } => muted(
                    ui,
                    "No save dialog could be opened here, so type where the file goes.",
                ),
                Saving::Offered { .. } => {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let said = ui.label(RichText::new("Not saved.").color(pal.text_muted));
                        announce(ui.ctx(), said.id);
                        type_instead |= ui.link("Type a path instead").clicked();
                    });
                }
                Saving::Exists { path, .. } => {
                    ui.horizontal_wrapped(|ui| {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let question = ui.label(
                            RichText::new(format!("{name} is there already. Replace it?"))
                                .color(pal.warn),
                        );
                        announce(ui.ctx(), question.id);
                        replace |= ui.button("Replace").clicked();
                        keep |= ui.button("Keep it").clicked();
                    });
                }
                _ => {}
            }
            if let Some(status) = &view.status {
                ui.add_space(6.0);
                let said = match status {
                    // One line, however long the path: its start gives way, so the file's name stays in view.
                    Ok(text) => {
                        let shown = keep_the_end(ui, text, ui.available_width());
                        let said = ui.add(egui::Label::new(
                            RichText::new(&shown).color(pal.success),
                        ));
                        if shown != *text {
                            ui.ctx()
                                .accesskit_node_builder(said.id, |node| node.set_value(text.clone()));
                            said.on_hover_text(text)
                        } else {
                            said
                        }
                    }
                    Err(text) => {
                        ui.add(egui::Label::new(RichText::new(text).color(pal.warn)).wrap())
                    }
                };
                announce(ui.ctx(), said.id);
                // A long path wraps; the block gives up that much room in the next frame.
                view.status_height = said.rect.height() + 6.0;
            }
        });
        if choose {
            view.save = Saving::Choosing;
            view.status = None;
            self.export_choices_pending += 1;
            self.export_choice_asked = Some(view.file_name.clone());
        }
        if (save_typed || replace)
            && let Some(result) = &view.result
        {
            let typed = match &view.save {
                Saving::Typing { path, .. } => Some(path.clone()),
                Saving::Exists { typed, .. } if replace => Some(typed.clone()),
                _ => None,
            };
            if let Some(typed) = typed {
                // Quotes from a copied path ("Copy as path" on Windows) are not part of it.
                let path = std::path::PathBuf::from(typed.trim().trim_matches('"'));
                if path.is_absolute() {
                    view.status = None;
                    view.save = Saving::Writing {
                        path: path.clone(),
                        typed: Some(typed),
                    };
                    if let Some(core) = &self.core {
                        core.send(Command::SaveExport {
                            path,
                            text: result.text.clone(),
                            replace,
                        });
                    }
                } else {
                    view.status = Some(Err(
                        "Not saved: give the whole path, starting from the drive or /.".to_owned(),
                    ));
                }
            }
        }
        if keep && let Saving::Exists { typed, .. } = &view.save {
            view.save = Saving::Typing {
                path: typed.clone(),
                focus: true,
                no_dialog: false,
            };
        }
        if type_instead && let Saving::Offered { path } = &view.save {
            view.save = Saving::Typing {
                path: path.clone(),
                focus: true,
                no_dialog: false,
            };
        }
        if let Some(result) = &view.result {
            if copy {
                ctx.copy_text(result.text.clone());
            }
            if copy_names {
                let lines: String = result
                    .variables
                    .iter()
                    .filter(|v| v.default.is_none())
                    .map(|v| format!("{}=\n", v.name))
                    .collect();
                ctx.copy_text(lines);
            }
            if copy || copy_names {
                let what = if copy { Copied::Block } else { Copied::Names };
                view.copied = Some((what, now));
                // Back to "Copy" once the confirmation has been seen; no repaint every frame meanwhile.
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS));
            }
        }
        // The dialog is centred by its size in the frame before; when what it holds changes its size, one more frame
        // puts it in place (or a click aimed at a button lands where the button was).
        let size = modal.response.rect.size();
        if view.size != size {
            view.size = size;
            ctx.request_repaint();
        }
        if close || closed_on_purpose(&modal) {
            self.exporting = None;
        }
    }

    fn casing_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let draft = self.drafted();
        let Some(form) = &mut self.casing else {
            return;
        };
        let ready = form.fixable() && !form.changes().is_empty();
        let mut review = ready && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("casing_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Fix casing");
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "These servers differ only in upper and lower case. Choose the spelling they all get.",
                    )
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            for m in &form.members {
                let place = match (&m.project, m.scope) {
                    (Some(p), _) => format!("{} \u{b7} {}", m.scope.label(), folder_name(p)),
                    (None, _) => m.scope.label().to_owned(),
                };
                ui.add(
                    egui::Label::new(format!("{}  \u{b7}  {place}", m.name))
                        .truncate()
                        .selectable(false),
                );
            }
            ui.add_space(10.0);
            let spellings = form.spellings();
            for spelling in spellings {
                let allowed = form.allowed(&spelling);
                let selected = form.keep == spelling;
                let response = ui
                    .add_enabled(
                        allowed,
                        egui::Button::selectable(selected, format!("Keep {spelling}")),
                    )
                    .on_disabled_hover_text(
                        "A server from a repository's .mcp.json keeps its name; the others follow it",
                    );
                if response.clicked() {
                    form.keep = spelling;
                }
            }
            ui.add_space(8.0);
            if form.fixable() {
                let n = form.changes().len();
                ui.add(
                    egui::Label::new(format!(
                        "Renames {n} {} to {}. A server keeps its definition and stays off where it was off.",
                        if n == 1 { "server" } else { "servers" },
                        form.keep
                    ))
                    .wrap(),
                );
                let renamed = form.changes().into_iter().any(
                    |c| matches!(c, Change::Rename { id, .. } if Some(&id) == draft.as_ref()),
                );
                if renamed {
                    ui.label(RichText::new(GOES_ALONG).small().color(pal.text_muted));
                }
            } else {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Servers from different repositories' .mcp.json files use different spellings, and Klefki does not edit those files. Change them there.",
                        )
                        .color(pal.danger),
                    )
                    .wrap(),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let confirm =
                    egui::Button::new(RichText::new("Review").strong().color(pal.on_bright))
                        .fill(pal.primary)
                        .min_size(egui::vec2(96.0, 34.0));
                review |= ui
                    .add_enabled(ready, confirm)
                    .on_hover_text("Enter")
                    .on_disabled_hover_text("Nothing to rename with this choice")
                    .clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
        });
        if review {
            let changes = form.changes();
            self.casing = None;
            self.prepare(changes);
        } else if cancel || closed_on_purpose(&modal) {
            self.casing = None;
        }
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

    fn duplicate_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(form) = &mut self.duplicate else {
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
        // What the window leaves for the dialog's body, below its title and above its buttons.
        let body_height = (ctx.content_rect().height() - 260.0).max(120.0);
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
                CopyRead::Ready(fields) => fields.adjust().unwrap_or_default(),
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
            self.duplicate = None;
            self.prepare(vec![change]);
            self.adding_signs_in = signs_in;
        } else if cancel || closed_on_purpose(&modal) {
            self.duplicate = None;
        }
    }

    fn move_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let draft = self.drafted();
        let Some(form) = &mut self.moving else {
            return;
        };
        let draft_goes = draft.as_ref() == Some(&form.source);
        let name = form.source.name.clone();
        let taken = form.places.taken(&name, &self.snapshot.servers);
        let ready = taken.is_none() && form.places.targets().len() == 1;
        let mut review = ready && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        let list_height = (ctx.content_rect().height() - 420.0).clamp(96.0, 320.0);
        let modal = egui::Modal::new(egui::Id::new("move_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(format!("Move {name}"));
            ui.add(
                egui::Label::new(RichText::new(&form.about).small().color(pal.text_muted))
                    .truncate(),
            );
            ui.add_space(12.0);
            // What it costs, where the place is chosen.
            if form.signs_in {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} It signs in with OAuth: Claude Code removes it and adds it again, so you will need to sign in to it again afterwards.",
                            icon::WARNING
                        ))
                        .color(pal.warn),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
            }
            ui.label(RichText::new("Move it to").color(pal.text_muted));
            form.places.ui(ui, list_height, None);
            ui.add_space(10.0);
            // What Review will lead to, or why it cannot yet.
            if let Some(taken) = &taken {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{name} already exists {taken}. Rename one of them first, or choose another place."
                        ))
                        .color(pal.danger),
                    )
                    .wrap(),
                );
            } else if let Some(places) = form.places.one_in_words() {
                ui.add(egui::Label::new(format!("Moves {name} {places}.")).wrap());
            } else {
                ui.label(RichText::new("Choose where it goes.").color(pal.text_muted));
            }
            if !form.signs_in {
                ui.add(
                    egui::Label::new(
                        RichText::new("It keeps its settings, environment and header values.")
                            .small()
                            .color(pal.text_muted),
                    )
                    .wrap(),
                );
            }
            if draft_goes {
                ui.label(RichText::new(GOES_ALONG).small().color(pal.text_muted));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let confirm =
                    egui::Button::new(RichText::new("Review").strong().color(pal.on_bright))
                        .fill(pal.primary)
                        .min_size(egui::vec2(96.0, 34.0));
                review |= ui
                    .add_enabled(ready, confirm)
                    .on_hover_text("Enter")
                    .on_disabled_hover_text("Choose where it goes")
                    .clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
        });
        if review {
            let to = form.places.targets().remove(0);
            let change = Change::Move {
                id: form.source.clone(),
                to,
            };
            let signs_in = form.signs_in;
            let from = form.source.clone();
            self.moving = None;
            self.prepare(vec![change]);
            self.adding_signs_in = signs_in;
            self.moved_from = Some((from, signs_in));
        } else if cancel || closed_on_purpose(&modal) {
            self.moving = None;
        }
    }

    /// Opens the Add dialog, starting from the selected server's project if it has one.
    fn open_add(&mut self) {
        if self.core.is_none() || !self.can_prepare() {
            return;
        }
        let here = self
            .single_selected()
            .filter(|r| r.scope != Scope::User)
            .and_then(|r| r.project.clone());
        let mut places = Places::new(&self.snapshot.projects, here.as_deref());
        places.user = true;
        self.add = Some(AddForm::new(self.add_mode, places));
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(form) = &mut self.add else {
            return;
        };
        if form.reviewing {
            // Behind the review, as it was, to come Back to.
            return;
        }
        let focused = form.focus_was;
        if form.asking_to_discard
            && ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(_) | egui::Event::Paste(_)))
            })
        {
            // Back to typing: the question no longer fits what is there.
            form.asking_to_discard = false;
            form.focus_back = None;
        }
        form.reread();
        form.refill();
        let checked = form.check(&self.snapshot.servers);
        let places = form.places.in_words();
        let addable: Vec<usize> = (0..checked.len())
            .filter(|&i| checked[i] == Check::Ready)
            .collect();
        let import = form.mode == AddMode::Import;
        let (conflicts, import_plan) = if import {
            (
                form.conflicts(&self.snapshot.servers),
                form.import_plan(&self.snapshot.servers, &checked, false),
            )
        } else {
            (Vec::new(), None)
        };
        let ready = !addable.is_empty()
            && places.is_some()
            && checked.iter().all(|c| !matches!(c, Check::Name(_)))
            && (!import || import_plan.is_some());
        let mut choose_again = false;
        let mut review = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
        let mut cancel = false;
        let mut discard_now = false;
        // The top stays put, so the tabs and fields do not move as the dialog grows or shrinks; the body scrolls
        // as one, between the tabs and the buttons, which stay in view.
        let screen = ctx.content_rect().height();
        let top = (screen * 0.06).clamp(12.0, 48.0);
        // The body gets the window's height less what the rest of the dialog took on the last frame: the tabs, a
        // failure said above the buttons, the line there and the buttons.
        let overhead_id = egui::Id::new("add_dialog_overhead");
        let overhead = ctx
            .data(|d| d.get_temp::<f32>(overhead_id))
            .unwrap_or(150.0);
        let body = (screen - 2.0 * top - overhead).max(120.0);
        let id = egui::Id::new("add_dialog");
        let area =
            egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top));
        let modal = egui::Modal::new(id).area(area).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.horizontal(|ui| {
                if import {
                    ui.heading("Import servers");
                    return;
                }
                for (mode, label) in [(AddMode::Paste, "Paste"), (AddMode::Fill, "Fill in")] {
                    let tab = egui::Button::new(label)
                        .selected(form.mode == mode)
                        .frame(true)
                        .min_size(egui::vec2(80.0, 30.0));
                    if ui.add(tab).clicked() && form.mode != mode {
                        form.mode = mode;
                        form.focus = true;
                        form.tried = false;
                        form.failed = None;
                    }
                }
                ui.add_space(12.0);
                ui.heading(match form.mode {
                    AddMode::Paste | AddMode::Import => "Add servers",
                    AddMode::Fill => "Add a server",
                });
            });
            ui.add_space(4.0);
            // A solid bar, not one that shows on hover: a body that scrolls has to look like it.
            ui.spacing_mut().scroll = egui::style::ScrollStyle {
                // The handle in the text's colour: the theme's fill is the dialog's own.
                foreground_color: true,
                ..egui::style::ScrollStyle::solid()
            };
            let body_top = ui.min_rect().bottom();
            egui::ScrollArea::vertical()
                .id_salt("add_body")
                // Both, so it takes its height in one frame rather than growing into the dialog's last size; it still
                // shrinks to what it holds.
                .min_scrolled_height(body)
                .max_height(body)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    match form.mode {
                        AddMode::Paste => form.paste_ui(ui, pal, &checked),
                        AddMode::Fill => form.fill_ui(ui, pal, &checked),
                        AddMode::Import => {
                            choose_again = form.import_ui(ui, pal, &checked, &conflicts);
                        }
                    }
                    // Where they go is asked once there is something to import.
                    if import && form.read.is_err() {
                        return;
                    }
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(match form.mode {
                            AddMode::Paste => "Add them",
                            AddMode::Import => "Import them",
                            AddMode::Fill => "Add it",
                        })
                        .color(pal.text_muted),
                    );
                    // At its full length: the body scrolls, and one scroll is all there is under the wheel.
                    if form.places.ui(ui, f32::INFINITY, Some("selected")) {
                        form.places_chosen = true;
                    }
                });
            let body_height = ui.min_rect().bottom() - body_top;
            ui.separator();
            if let Some(failed) = &form.failed {
                // What happened and what to do first; the rest (what a tool said) small and muted, under it.
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
            // What Review will lead to, or why it cannot yet.
            let count = |f: fn(&Check) -> bool| checked.iter().filter(|c| f(c)).count();
            let skipped = count(|c| matches!(c, Check::Skipped(why) if !why.is_empty()));
            let unfinished = count(|c| matches!(c, Check::FillIn(_)));
            if form.mode != AddMode::Fill && form.read.is_ok() && skipped + unfinished > 0 {
                let mut parts = Vec::new();
                if skipped > 0 {
                    parts.push(if skipped == 1 {
                        "1 server cannot be added and is left out".to_owned()
                    } else {
                        format!("{skipped} servers cannot be added and are left out")
                    });
                }
                if unfinished > 0 {
                    parts.push(if unfinished == 1 {
                        "1 needs finishing in the form".to_owned()
                    } else {
                        format!("{unfinished} need finishing in the form")
                    });
                }
                ui.label(RichText::new(format!("{}.", parts.join("; "))).color(pal.text_muted));
            }
            let stop = if form.tried {
                pal.danger
            } else {
                pal.text_muted
            };
            match &places {
                None => {
                    ui.label(RichText::new("Tick at least one place.").color(stop));
                }
                Some(places) if ready && let Some(plan) = &import_plan => {
                    ui.add(egui::Label::new(plan.in_words(places)).wrap());
                }
                Some(places) if ready => {
                    let names = form.names_to_add(&addable);
                    ui.add(
                        egui::Label::new(format!(
                            "Adds {} {places}.",
                            list_in_words(&names, "server")
                        ))
                        .wrap(),
                    );
                }
                // Every server ticked is already there, and kept: Review would change nothing.
                Some(_) if import && import_plan.is_none() && !addable.is_empty() => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Each server ticked is there already and kept, so nothing would change. Replace them, or import them under other names.",
                            )
                            .color(stop),
                        )
                        .wrap(),
                    );
                }
                Some(_) if form.tried && import => {
                    ui.label(
                        RichText::new(
                            "Tick a server that can be imported, and fix what is marked above.",
                        )
                        .color(stop),
                    );
                }
                Some(_) if form.tried && form.mode == AddMode::Paste => {
                    ui.label(
                        RichText::new("Paste a server that can be added, with a usable name.")
                            .color(stop),
                    );
                }
                Some(_) if form.tried => {
                    ui.label(RichText::new("Fix what is marked above.").color(stop));
                }
                // The line is kept, so the buttons do not move when it fills in.
                Some(_) => {
                    ui.label(" ");
                }
            }
            ui.add_space(8.0);
            if form.asking_to_discard {
                // In place of the buttons, where the eye already is: a second dialog over this one would hide what
                // is about to be thrown away.
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::WARNING_CIRCLE).color(pal.danger));
                    ui.label(RichText::new("Discard what you typed?").strong());
                    ui.add_space(8.0);
                    let discard =
                        egui::Button::new(RichText::new("Discard").strong().color(pal.on_bright))
                            .fill(pal.danger)
                            .min_size(egui::vec2(96.0, 34.0));
                    discard_now = ui.add(discard).clicked();
                    let keep = ui
                        .add(egui::Button::new("Keep editing").min_size(egui::vec2(80.0, 34.0)))
                        .on_hover_text("Esc");
                    // The least destructive answer has the keyboard, so Enter keeps editing and Shift+Tab
                    // reaches Discard.
                    if std::mem::take(&mut form.focus_answer) {
                        keep.request_focus();
                    }
                    if keep.clicked() {
                        form.asking_to_discard = false;
                        if let Some(id) = form.focus_back.take() {
                            ui.memory_mut(|m| m.request_focus(id));
                        }
                    }
                });
                return (ui.min_rect().height() - body_height, body_height);
            }
            ui.horizontal(|ui| {
                // Always pressable: pressed too soon, it says what is missing rather than doing nothing.
                let confirm =
                    egui::Button::new(RichText::new("Review").strong().color(pal.on_bright))
                        .fill(pal.primary)
                        .min_size(egui::vec2(96.0, 34.0));
                review |= ui.add(confirm).on_hover_text(shortcut("Enter")).clicked();
                cancel |= ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)))
                    .on_hover_text("Esc")
                    .clicked();
            });
            // Measured in this frame: the area's own rect is the one from the last frame.
            (ui.min_rect().height() - body_height, body_height)
        });
        settle(ctx, &modal.response);
        form.focus_was = ctx.memory(|m| m.focused());
        let (taken, body_height) = modal.inner;
        if (taken - overhead).abs() > 0.5 {
            ctx.data_mut(|d| d.insert_temp(overhead_id, taken));
            // Laid out again within this frame, only if the body is, or would now be, held to the height it may take.
            let held = body_height >= body - 0.5;
            if held || body_height > (screen - 2.0 * top - taken).max(120.0) {
                ctx.request_discard("the Add dialog's body height");
            }
        }
        if !import {
            self.add_mode = form.mode;
        }
        let path_wanted = if std::mem::take(&mut form.path_wanted) {
            // Quotes from a copied path ("Copy as path" on Windows) are not part of it. A path that is not whole
            // would be read from wherever Klefki was started.
            let path = std::path::PathBuf::from(form.path.trim().trim_matches('"'));
            if path.is_absolute() {
                Some(path)
            } else {
                form.file_problem =
                    Some("Give the whole path, starting from the drive or /.".to_owned());
                None
            }
        } else {
            None
        };
        // The text and names may have changed in this frame: what is sent is checked again as it is now.
        let changes = review
            .then(|| form.changes(&self.snapshot.servers))
            .flatten();
        let signs_in = form.signs_in();
        if changes.is_none() && review {
            let checked = form.check(&self.snapshot.servers);
            form.show_what_stops_review(&checked);
        }
        // Esc asks first when there is something to lose, and backs out of that question when pressed again.
        let escaped = closed_on_purpose(&modal) && !review && !cancel && !discard_now;
        let close = !review
            && (cancel || discard_now || escaped && !form.asking_to_discard && !form.has_typing());
        if escaped && !close {
            form.asking_to_discard = !form.asking_to_discard;
            if form.asking_to_discard {
                form.focus_back = focused;
                form.focus_answer = true;
            } else if let Some(id) = form.focus_back.take() {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
        if let Some(port) = form.port_wanted.take() {
            form.port_pending = Some(port);
            self.send(Command::CheckCallbackPort(port));
        }
        if let Some(path) = path_wanted
            && !self.import_waiting
        {
            self.import_waiting = true;
            self.send(Command::ReadImportFile(path));
        } else if choose_again {
            self.ask_import();
        }
        if let Some(changes) = changes {
            if self.can_prepare() {
                self.prepare(changes);
                self.adding_signs_in = signs_in;
                if import {
                    self.adding_verb = "Imported";
                    // Replaced ones are selected with the rest once it is done.
                    if let Some(plan) = import_plan {
                        self.adding.extend(plan.replaces);
                    }
                }
                if let Some(form) = &mut self.add {
                    form.reviewing = true;
                    form.asking_to_discard = false;
                    form.failed = None;
                }
            }
        } else if close {
            self.close_add();
        }
    }

    /// Whether a file to import can be asked for now: nothing else is open or under way, or the Import dialog is open
    /// (to read another file into it).
    fn can_import(&self) -> bool {
        self.core.is_some()
            && self.can_prepare()
            && !self.import_waiting
            && self
                .add
                .as_ref()
                .is_none_or(|f| f.mode == AddMode::Import && !f.reviewing)
            && self.duplicate.is_none()
            && self.moving.is_none()
            && self.casing.is_none()
            && self.sign_in.is_none()
            && self.exporting.is_none()
            && !self.command.is_open()
    }

    /// Asks for a file to import, in the system's open dialog, opened next by [`eframe::App::ui`] over the window.
    fn ask_import(&mut self) {
        if self.can_import() {
            self.import_waiting = true;
            self.import_choice_asked = true;
        }
    }

    /// A file dropped on the window is read for an import, when one could be asked for; otherwise it says why not.
    fn take_dropped_file(&mut self, ctx: &egui::Context) {
        let paths: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let [path] = paths.as_slice() else {
            if !paths.is_empty() {
                self.note("Drop one file at a time to import it.".to_owned(), false);
            }
            return;
        };
        if self.can_import() {
            self.import_waiting = true;
            self.send(Command::ReadImportFile(path.clone()));
        } else {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.note(
                format!("To import {name}, close what is open, then drop it again."),
                false,
            );
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

    /// Opens the Import dialog on what came of asking for a file, or puts it into the one open.
    fn open_import(&mut self, file: ImportFile) {
        if let Some(form) = self.open_import_form() {
            form.load(file);
        }
    }

    /// The Import dialog, opened if it can be.
    fn open_import_form(&mut self) -> Option<&mut AddForm> {
        if self.add.is_none() {
            if !self.can_import() {
                return None;
            }
            let mut places = Places::new(&self.snapshot.projects, None);
            places.user = true;
            self.add = Some(AddForm::new(AddMode::Import, places));
        }
        self.add.as_mut().filter(|f| f.mode == AddMode::Import)
    }

    /// Closes the Add dialog, and has egui forget what it remembers of the secrets typed into it.
    fn close_add(&mut self) {
        self.add = None;
        self.forget_typed = true;
    }
    fn single_selected(&self) -> Option<&ServerRow> {
        let mut it = self.selected.iter();
        match (it.next(), it.next()) {
            (Some(&i), None) => self.snapshot.servers.get(i),
            _ => None,
        }
    }

    /// Settings. For now, how Klefki looks: light or dark, and which family of greys.
    fn settings(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(8.0);
        ui.label(RichText::new("Settings").heading());
        ui.add_space(16.0);
        ui.label(RichText::new("Appearance").strong());
        ui.add_space(6.0);
        egui::Grid::new("appearance")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Theme").color(pal.text_muted));
                ui.horizontal(|ui| {
                    for choice in ThemeChoice::ALL {
                        let text = format!("{} {}", choice.icon(), choice.label());
                        if ui.selectable_value(&mut self.theme, choice, text).changed() {
                            theme::set_choice(ui.ctx(), choice);
                        }
                    }
                });
                ui.end_row();
                ui.label(RichText::new("Greys").color(pal.text_muted));
                ui.horizontal(|ui| {
                    for tone in Tone::ALL {
                        if ui
                            .selectable_value(&mut self.tone, tone, tone.label())
                            .changed()
                        {
                            theme::set_tone(ui.ctx(), tone);
                        }
                    }
                });
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Both keep every text at a contrast of at least 4.5:1.")
                .small()
                .color(pal.text_muted),
        );
        // The top bar names the version only where it fits; here it always has room.
        ui.add_space(24.0);
        ui.label(RichText::new("Programs").strong());
        ui.add_space(6.0);
        egui::Grid::new("programs")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Claude Code").color(pal.text_muted));
                match (self.shown_cli_version(), &self.cli_problem) {
                    (Some(version), _) => ui.label(version),
                    // Beside it, so it is in view even when what follows is not.
                    (None, Some(_)) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Not found").color(pal.warn));
                            ui.add_space(12.0);
                            if ui.button("Look again").on_hover_text("F5").clicked() {
                                self.refresh();
                            }
                        })
                        .response
                    }
                    (None, None) => ui.label(RichText::new("Not known yet").color(pal.text_muted)),
                };
                ui.end_row();
            });
        // Not found: what to do about it, and a way to look again once it is done; the engine's reason under it,
        // for when it is there but cannot be run.
        if self.shown_cli_version().is_none()
            && let Some(problem) = self.cli_problem.clone()
        {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    "Klefki looks for claude in ~/.local/bin and on your PATH. Install Claude Code or add it \
                     to your PATH, then look again.",
                )
                .wrap(),
            );
            ui.add(egui::Label::new(RichText::new(problem).small().color(pal.text_muted)).wrap());
        }
        self.update_settings(ui);
    }

    fn update_settings(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        ui.add_space(24.0);
        ui.label(RichText::new("Updates").strong());
        ui.add_space(6.0);
        if !self.checks_for_updates {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Klefki {}. This build does not check for updates: it was not built by Klefki's release \
                         workflow.",
                        klefki_core::api::Version::current()
                    ))
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            return;
        }
        egui::Grid::new("updates")
            .num_columns(2)
            .spacing([24.0, 14.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Klefki").color(pal.text_muted));
                ui.horizontal(|ui| {
                    ui.label(klefki_core::api::Version::current().to_string());
                    ui.add_space(12.0);
                    if self.updates.checking {
                        ui.spinner();
                        ui.label(RichText::new("Checking…").color(pal.text_muted));
                        return;
                    }
                    if let Some(version) = self.updates.available() {
                        if self.updated_by_hand.is_some() {
                            ui.label(
                                RichText::new(format!("{version} is available")).color(pal.primary),
                            );
                        } else if self.updates.downloading {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!("Downloading {version}…"))
                                    .color(pal.text_muted),
                            );
                        } else if self.updates.ready() == Some(version) {
                            ui.label(
                                RichText::new(format!(
                                    "{version} is ready: it installs when Klefki closes"
                                ))
                                .color(pal.primary),
                            );
                            let busy = self.change_open();
                            if ui
                                .add_enabled(!busy, egui::Button::new("Restart now"))
                                .on_disabled_hover_text("Finish the open change first.")
                                .clicked()
                            {
                                self.restart_to_update(ui.ctx());
                            }
                        } else {
                            ui.label(
                                RichText::new(format!("{version} is available")).color(pal.primary),
                            );
                            if ui.button("Download").clicked() {
                                self.download_update();
                            }
                        }
                        if ui.button("What's new").clicked() {
                            self.send(Command::OpenReleasePage(version));
                        }
                        // Checking again would only find the same release.
                        return;
                    } else if let Some(UpdateCheck::NoRelease) = &self.updates.found {
                        ui.label(
                            RichText::new("No signed release is published yet")
                                .color(pal.text_muted),
                        );
                    } else if let Some(text) = self.updates.up_to_date(crate::updates::now()) {
                        ui.label(RichText::new(text).color(pal.text_muted));
                    }
                    if ui.button("Check now").clicked() {
                        self.check_for_updates_now();
                    }
                });
                ui.end_row();
            });
        if let Some(UpdateCheck::Failed(why)) = &self.updates.found
            && !self.updates.checking
        {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(why.as_str()).color(pal.warn)).wrap());
        }
        if self.updates.available().is_some() {
            let note = match (&self.updated_by_hand, self.updates.download_failed()) {
                (Some(why), _) => Some((why.clone(), pal.text_muted)),
                (None, Some(why)) => Some((why.to_owned(), pal.warn)),
                (None, None) => None,
            };
            if let Some((text, color)) = note {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
            }
        }
        ui.add_space(10.0);
        let automatic = if self.updated_by_hand.is_some() {
            "Check for updates automatically"
        } else {
            "Update Klefki automatically"
        };
        theme::checkbox(ui, &mut self.updates.auto, automatic);
        ui.add(
            egui::Label::new(
                RichText::new(
                    if self.updated_by_hand.is_some() {
                        "Once a day, Klefki asks GitHub whether a newer version is out. GitHub sees your IP address \
                         and Klefki's version, as with any download; nothing about your servers or settings is sent. \
                         A release is offered only if it carries Klefki's signature."
                    } else {
                        "Once a day, Klefki asks GitHub whether a newer version is out, and downloads it; it installs \
                         when Klefki closes, or when you restart. GitHub sees your IP address and Klefki's version, \
                         as with any download; nothing about your servers or settings is sent. A release is used only \
                         if it carries Klefki's signature, and each file only if it matches the signed checksum."
                    },
                )
                .small()
                .color(pal.text_muted),
            )
            .wrap(),
        );
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
            Some(p) => (p as i32 + step).clamp(0, order.len() as i32 - 1) as usize,
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

    /// The banner for the last change.
    /// The outcome of the last change, as a small card in the bottom-right corner: it does not push the list
    /// around, a success fades after a few seconds, and a problem stays until dismissed.
    fn outcome_toast(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let now = ctx.input(|i| i.time);
        let Some(outcome) = self.outcome.as_mut() else {
            return;
        };
        let shown_at = *outcome.shown_at.get_or_insert(now);
        // A card that offers something to do stays until it is used or dismissed: time limits on a choice exclude
        // whoever reads slowly.
        if outcome.ok && !outcome.warn && outcome.sign_in.is_none() && outcome.undo.is_none() {
            let left = SUCCESS_SECONDS - (now - shown_at);
            if left <= 0.0 {
                self.outcome = None;
                return;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        }
        let outcome = outcome.clone();
        let (color, glyph) = outcome_look(pal, &outcome);
        let mut chosen = self.outcome_chosen.take();
        if !self.outcome_inline {
            // In the window's bottom-right corner, but never over the save bar, whose buttons may be wanted while
            // it shows: beside the detail panel, over the list, where there is room (otherwise the detail panel
            // has it, above the bar).
            let screen = ctx.content_rect();
            let mut offset = egui::vec2(-TOAST_MARGIN, -TOAST_MARGIN);
            let mut width = TOAST_WIDTH;
            if self.save_bar_shown
                && let Some(room) = self.list_room
            {
                offset.x = room.right() - screen.right() - TOAST_MARGIN;
                width = self.outcome_width_beside(ctx).min(TOAST_WIDTH);
            }
            egui::Area::new(egui::Id::new("outcome"))
                .anchor(egui::Align2::RIGHT_BOTTOM, offset)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    outcome_frame(pal, color)
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: Color32::from_black_alpha(80),
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(width);
                            if let Some(choice) =
                                self.outcome_card(ui, &outcome, glyph, color, false)
                            {
                                chosen = Some(choice);
                            }
                        });
                });
        }
        match chosen {
            Some(OutcomeChoice::SignIn) => {
                if let Some(row) = outcome
                    .sign_in
                    .and_then(|id| self.snapshot.servers.iter().find(|r| r.is(&id)).cloned())
                {
                    self.outcome = None;
                    self.start_sign_in(&row);
                }
            }
            Some(OutcomeChoice::Undo) => {
                if let Some(change) = outcome.undo {
                    self.outcome = None;
                    self.prepare(vec![change]);
                }
            }
            Some(OutcomeChoice::OpenLog) => {
                self.outcome = None;
                self.go(Page::ActivityLog);
            }
            Some(OutcomeChoice::TypePath) => {
                self.outcome = None;
                if let Some(form) = self.open_import_form() {
                    form.typing = true;
                    form.focus = true;
                }
            }
            Some(OutcomeChoice::Dismiss) => self.outcome = None,
            None => {}
        }
    }

    /// How wide the outcome message's text can be beside the detail panel, over the list.
    fn outcome_width_beside(&self, ctx: &egui::Context) -> f32 {
        let frame = outcome_frame(theme::palette(ctx), Color32::TRANSPARENT);
        self.list_room.map_or(0.0, |room| {
            room.width() - 2.0 * TOAST_MARGIN - frame.total_margin().sum().x
        })
    }

    /// The outcome message fits beside the detail panel, so its save bar stays clear.
    fn outcome_fits_beside(&self, ctx: &egui::Context) -> bool {
        self.outcome_width_beside(ctx) >= TOAST_NARROWEST
    }

    /// The outcome message as a strip above the save bar, when it does not fit beside the detail panel: in the
    /// layout, so it covers nothing and the form above still scrolls.
    fn outcome_strip(&mut self, ui: &mut Ui) {
        let Some(outcome) = self.outcome.clone() else {
            return;
        };
        let pal = theme::palette(ui.ctx());
        let (color, glyph) = outcome_look(pal, &outcome);
        outcome_frame(pal, color).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.outcome_chosen = self.outcome_card(ui, &outcome, glyph, color, true);
        });
        self.outcome_inline = true;
    }

    /// The outcome message's text and buttons, and which of them was used.
    /// In a strip (`strip`), a long text scrolls past two and a half lines, the half line showing there is more,
    /// and the buttons wrap, so it leaves the form room and does not widen the panel.
    fn outcome_card(
        &self,
        ui: &mut Ui,
        outcome: &Outcome,
        glyph: &str,
        color: Color32,
        strip: bool,
    ) -> Option<OutcomeChoice> {
        let pal = theme::palette(ui.ctx());
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).size(16.0).color(color));
            let text = egui::Label::new(RichText::new(&outcome.text).color(pal.text)).wrap();
            if strip {
                // Wrapped lines follow each other with no spacing between them.
                let line = ui.text_style_height(&egui::TextStyle::Body);
                egui::ScrollArea::vertical()
                    .id_salt("outcome_text")
                    .max_height(2.5 * line)
                    .min_scrolled_height(2.5 * line)
                    .show(ui, |ui| ui.add(text));
            } else {
                ui.add(text);
            }
        });
        ui.with_layout(
            Layout::right_to_left(Align::Min).with_main_wrap(strip),
            |ui| {
                if ui.small_button("Dismiss").clicked() {
                    chosen = Some(OutcomeChoice::Dismiss);
                }
                // The backup's place stays on the Activity log after this card is gone.
                if outcome.type_path && ui.small_button("Type a path instead").clicked() {
                    chosen = Some(OutcomeChoice::TypePath);
                }
                if outcome.logged
                    && self.page != Page::ActivityLog
                    && ui.small_button("Open activity log").clicked()
                {
                    chosen = Some(OutcomeChoice::OpenLog);
                }
                if outcome.undo.is_some()
                    && ui
                        .add_enabled(self.can_prepare(), egui::Button::new("Undo"))
                        .on_hover_text("Move it back, after a review")
                        .clicked()
                {
                    chosen = Some(OutcomeChoice::Undo);
                }
                if outcome.sign_in.is_some() && self.sign_in.is_none() {
                    let button =
                        egui::Button::new(RichText::new("Sign in").strong().color(pal.on_bright))
                            .fill(pal.primary);
                    if ui.add(button).clicked() {
                        chosen = Some(OutcomeChoice::SignIn);
                    }
                }
            },
        );
        chosen
    }

    /// The confirmation step: shows exactly what would change, then applies or drops it.
    fn plan_dialog(&mut self, ctx: &egui::Context) {
        let pal = theme::palette(ctx);
        let Some(plan) = self.plan.clone() else {
            // What the last plan's diffs needed goes with it.
            self.diff_tops = DiffTops::default();
            return;
        };
        let applying = self.applying;
        let from_add = self.add.as_ref().is_some_and(|f| f.reviewing);
        let diff_tops = &mut self.diff_tops;
        let mut apply = false;
        let mut back = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("plan_dialog")).show(ctx, |ui| {
            ui.set_width(540.0);
            // Everything above the buttons scrolls when it is taller than the window (many servers added at once),
            // so the buttons always stay in view.
            let room = (ctx.content_rect().height() - 140.0).max(160.0);
            egui::ScrollArea::vertical()
                .id_salt(("plan_body", plan.id))
                .max_height(room)
                .min_scrolled_height(room)
                .show(ui, |ui| {
                    // What happens, in words, first; the exact lines follow for those who want them.
                    if plan.summary.is_empty() {
                        ui.heading("Review the change");
                    } else {
                        ui.label(RichText::new("Review the change").color(pal.text_muted));
                        for line in &plan.summary {
                            ui.add(egui::Label::new(RichText::new(line).heading()).wrap());
                        }
                    }
                    ui.add_space(8.0);
                    if plan.nothing_to_do {
                        // What is already so, by server and place: "already looks like this" alone does not say which.
                        for line in &plan.already {
                            ui.add(egui::Label::new(line.as_str()).wrap());
                        }
                        ui.label(if plan.already.is_empty() {
                            "The configuration already looks like this. Nothing would be written."
                        } else {
                            "Nothing would be written."
                        });
                    }
                    // Warnings first: what a change costs is read before the details (cross-review of the restore).
                    for warning in &plan.warnings {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} {warning}", icon::WARNING))
                                    .color(pal.warn),
                            )
                            .wrap(),
                        );
                    }
                    for review in &plan.reviews {
                        ui.add_space(4.0);
                        Frame::new()
                            .stroke(egui::Stroke::new(1.0, pal.warn))
                            .corner_radius(8)
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(&review.title).strong().color(pal.warn));
                                for line in &review.lines {
                                    ui.add(
                                        egui::Label::new(RichText::new(line).monospace()).wrap(),
                                    );
                                }
                            });
                    }
                    for (at, file) in plan.files.iter().enumerate() {
                        if file.lines.is_empty() {
                            continue;
                        }
                        ui.add_space(8.0);
                        // The size is in the heading: only the lines in view are laid out, so a screen reader would
                        // otherwise not know there are more, and the box below shows few at a time.
                        let count = file.lines.len();
                        ui.label(
                            RichText::new(format!(
                                "Changes in {} · {count} {}",
                                file.path,
                                if count == 1 { "line" } else { "lines" }
                            ))
                            .small()
                            .color(pal.text_muted),
                        );
                        Frame::new()
                            .fill(pal.field)
                            .corner_radius(8)
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                // Its own scroll position per plan and file: each review opens at the top.
                                // Only the lines in view are laid out: a diff can have thousands.
                                // A bar that is always drawn, beside the lines: the default one is invisible until
                                // pointed at, so a long diff would look no longer than the box.
                                ui.spacing_mut().scroll = egui::style::ScrollStyle {
                                    // In the text's colour, on a track that stands out from the box.
                                    foreground_color: true,
                                    ..egui::style::ScrollStyle::solid()
                                };
                                ui.visuals_mut().extreme_bg_color = pal.border;
                                egui::ScrollArea::vertical()
                                    // By its place in the plan: one file can have two diffs (what Klefki
                                    // writes, and what Claude Code's own command will).
                                    .id_salt(("plan_diff", plan.id, at))
                                    .max_height(220.0)
                                    // As wide as the box, so the bar is at its edge, not after the widest line in view.
                                    .auto_shrink([false, true])
                                    .scroll_bar_visibility(
                                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                                    )
                                    .show_viewport(ui, |ui, viewport| {
                                        let line = |ui: &Ui, line: &str| {
                                            let color = if line.starts_with("+ ") {
                                                pal.success
                                            } else if line.starts_with("- ") {
                                                pal.danger
                                            } else {
                                                pal.text_muted
                                            };
                                            egui::WidgetText::from(
                                                RichText::new(line).monospace().color(color),
                                            )
                                            .into_galley(
                                                ui,
                                                Some(egui::TextWrapMode::Wrap),
                                                ui.available_width(),
                                                egui::FontSelection::Default,
                                            )
                                        };
                                        let tops =
                                            diff_tops.of(ui, plan.id, at, &file.lines, |ui, l| {
                                                line(ui, l).size().y
                                            });
                                        let first = tops
                                            .partition_point(|&top| top <= viewport.min.y)
                                            .saturating_sub(1);
                                        let end = tops
                                            .partition_point(|&top| top < viewport.max.y)
                                            .min(file.lines.len());
                                        let total = tops.last().copied().unwrap_or(0.0);
                                        ui.add_space(tops[first]);
                                        for text in &file.lines[first..end.max(first)] {
                                            ui.add(egui::Label::new(line(ui, text)));
                                        }
                                        ui.add_space(total - tops[end.max(first)]);
                                    });
                            });
                    }
                    for note in &plan.notes {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{} {note}", icon::INFO))
                                    .small()
                                    .color(pal.text_muted),
                            )
                            .wrap(),
                        );
                    }
                });
            ui.add_space(12.0);
            let label = if !plan.action.is_empty() {
                plan.action.as_str()
            } else if plan.reviews.is_empty() {
                "Apply"
            } else {
                "Approve"
            };
            ui.horizontal(|ui| {
                let fill = if plan.destructive {
                    pal.danger
                } else {
                    pal.primary
                };
                let confirm = egui::Button::new(RichText::new(label).strong().color(pal.on_bright))
                    .fill(fill)
                    .min_size(egui::vec2(96.0, 34.0));
                let enabled = !plan.nothing_to_do && !applying;
                if ui
                    .add_enabled(enabled, confirm)
                    .on_hover_text("Enter")
                    .clicked()
                {
                    apply = true;
                }
                // Back to the Add dialog, as it was, to change something.
                if from_add
                    && ui
                        .add_enabled(
                            !applying,
                            egui::Button::new(format!("{} Back", icon::ARROW_LEFT))
                                .min_size(egui::vec2(80.0, 34.0)),
                        )
                        .on_hover_text("Change what is added (Esc)")
                        .clicked()
                {
                    back = true;
                }
                // Once Apply is pressed the engine writes the plan; closing now would hide that.
                if ui
                    .add_enabled(
                        !applying,
                        egui::Button::new("Cancel").min_size(egui::vec2(80.0, 34.0)),
                    )
                    .on_hover_text(if from_add { "Add nothing" } else { "Esc" })
                    .clicked()
                {
                    cancel = true;
                }
                if applying {
                    // Text, not a spinner: an animation would repaint every frame. A write takes milliseconds;
                    // Claude Code's commands take seconds each, so each one is named as it starts.
                    match &self.applying_step {
                        None => {
                            ui.label(RichText::new("Saving…").color(pal.text_muted));
                        }
                        Some(step) => {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(step).color(pal.text));
                                ui.label(
                                    RichText::new("Keep Klefki open until this is done.")
                                        .small()
                                        .color(pal.text_muted),
                                );
                            });
                        }
                    }
                }
                // Enter confirms, as in any dialog; not while a field would take it.
                if enabled
                    && !ui.ctx().egui_wants_keyboard_input()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    apply = true;
                }
            });
        });
        settle(ctx, &modal.response);
        if closed_on_purpose(&modal) && !applying {
            // Esc backs out one step: from the review of an Add, to the Add dialog, as it was.
            if from_add {
                back = true;
            } else {
                cancel = true;
            }
        }
        if apply {
            self.applying = true;
            self.send(Command::Apply(plan.id));
            // The dialog stays until the engine answers, so a second click cannot apply twice.
        } else if back || cancel {
            self.send(Command::Discard(plan.id));
            self.plan = None;
            self.applying = false;
            self.adding.clear();
            if back && let Some(form) = &mut self.add {
                form.reviewing = false;
            } else if from_add {
                self.close_add();
            }
        }
    }

    fn grid(&mut self, ui: &mut Ui) {
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
                        row.col(|ui| name_cell(ui, s, project_count));
                        row.col(|ui| {
                            ui.label(RichText::new(s.transport.label()).color(pal.text_muted));
                        });
                        let (status, detail) = self.effective(*i);
                        let hint = self.hint(*i);
                        let last_known = self.last_known(*i);
                        row.col(|ui| status_cell(ui, s, status, detail, hint, last_known));
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

    fn detail(&mut self, ui: &mut Ui) {
        let pal = theme::palette(ui.ctx());
        let gutter = Margin {
            right: scroll_gutter(ui.style()),
            ..Margin::ZERO
        };
        let Some(&i) = self
            .selected
            .iter()
            .next()
            .filter(|_| self.selected.len() == 1)
        else {
            Frame::new().inner_margin(gutter).show(ui, |ui| {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    if self.selected.is_empty() {
                        ui.label(
                            RichText::new(icon::PLUGS_CONNECTED)
                                .size(28.0)
                                .color(pal.text_muted),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new("Select a connection").strong());
                    } else {
                        ui.label(
                            RichText::new(format!("{} connections selected", self.selected.len()))
                                .strong(),
                        );
                        ui.label(
                            RichText::new("Turn them on or off, or delete them, together.")
                                .color(pal.text_muted),
                        );
                    }
                    ui.add_space(12.0);
                    for (keys, what) in [
                        ("↑ ↓".to_owned(), "move through the list"),
                        (shortcut("K"), "all commands"),
                        (shortcut("F"), "filter"),
                        ("F5".to_owned(), "check every server again"),
                    ] {
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 240.0).max(0.0) / 2.0);
                            ui.label(RichText::new(keys).monospace().color(pal.text));
                            ui.label(RichText::new(what).color(pal.text_muted));
                        });
                    }
                });
            });
            return;
        };
        // A second handle to the snapshot (a reference count), so the row can be read while `self` is borrowed
        // mutably below, without copying its strings every frame.
        let snapshot = Arc::clone(&self.snapshot);
        let s = &snapshot.servers[i];
        if let Some(editor) = &mut self.editor
            && s.is(&editor.id)
        {
            editor.state.sync();
        }
        let (status, detail) = {
            let (status, detail) = self.effective(i);
            (status, detail.map(str::to_owned))
        };
        let explanation = match &self.explained {
            Some((key, built)) if *key == (i, self.live_gen) => built.clone(),
            _ => {
                let built = Explanation::for_status(s, status, detail.as_deref(), self.hint(i))
                    .map(Arc::new);
                self.explained = Some(((i, self.live_gen), built.clone()));
                built
            }
        };
        // The name and the tabs stay in view while what is under them scrolls.
        Frame::new().inner_margin(gutter).show(ui, |ui| {
            ui.label(RichText::new(&s.name).heading());
            ui.horizontal(|ui| {
                flat_items(ui);
                // Compact pills: smaller text and padding, so the tabs sit on one line and do not shout.
                ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
                ui.spacing_mut().item_spacing.x = 4.0;
                for (tab, title) in DetailTab::ALL {
                    // A plugin's or the organisation's server has no definition here to show values or sign-in
                    // settings from.
                    if s.scope.is_read_only()
                        && !matches!(tab, DetailTab::General | DetailTab::Status)
                    {
                        continue;
                    }
                    ui.selectable_value(&mut self.tab, tab, RichText::new(title).size(13.5));
                }
            });
            ui.separator();
        });
        // A diagnosis or an open form is often taller than the window. Each server and tab keeps a place of its
        // own, by identity: rows are kept by position, which another server added or removed above changes. The
        // save bar's room is left below, as tall as it was last frame.
        self.save_bar_wanted = false;
        egui::ScrollArea::vertical()
            .id_salt(("detail", &s.connection, self.tab))
            .auto_shrink([false, false])
            .max_height((ui.available_height() - self.save_bar_height).max(0.0))
            .content_margin(gutter)
            .show(ui, |ui| {
                self.detail_tab(ui, i, s, status, detail.as_deref(), explanation.as_deref());
                bring_focus_into_view(ui);
            });
        // Under the form rather than at its end, so it, and why saving is blocked, stay in view; drawn after the
        // form, so Tab reaches it last. A change in its height is laid out again before the frame is shown.
        let height = if self.save_bar_wanted {
            let bar = Frame::new().inner_margin(gutter).show(ui, |ui| {
                if !self.outcome_fits_beside(ui.ctx()) {
                    self.outcome_strip(ui);
                }
                ui.separator();
                self.save_bar(ui, s);
            });
            self.save_bar_shown = true;
            bar.response.rect.height() + ui.spacing().item_spacing.y
        } else {
            0.0
        };
        if (height - self.save_bar_height).abs() > 0.5 {
            self.save_bar_height = height;
            ui.ctx()
                .request_discard("the detail panel's save bar changed height");
        }
        // A definition asked for with Edit is brought into view until a pass laid out with its save bar is shown.
        if !ui.ctx().will_discard()
            && let Some(e) = self.editor.as_mut().filter(|e| s.is(&e.id))
            && !matches!(e.state, EditorState::Loading)
        {
            e.reveal = false;
        }
    }

    /// The detail panel's open tab for the row `i`, `s`, under the name and tabs.
    fn detail_tab(
        &mut self,
        ui: &mut Ui,
        i: usize,
        s: &ServerRow,
        status: Status,
        detail: Option<&str>,
        explanation: Option<&Explanation>,
    ) {
        let pal = theme::palette(ui.ctx());
        // Long values must wrap inside the panel; without a cap a grid cell grows to fit its text, and the whole
        // panel grows with it.
        let col_width = (ui.available_width() - 110.0).max(100.0);
        let tab = match self.tab {
            DetailTab::HeadersEnv | DetailTab::Auth if s.scope.is_read_only() => DetailTab::General,
            tab => tab,
        };
        match tab {
            DetailTab::General => {
                // First: Claude Code does not run it at all.
                if let Some(why) = &s.not_loaded {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Claude Code does not load this server. {why}"))
                                .color(pal.warn),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                }
                // Then, before anyone looks for a way to edit it.
                if let Some(file) = &s.managed {
                    ui.add(
                        egui::Label::new(
                            RichText::new(managed_read_only(file)).color(pal.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                }
                if let Some(p) = &s.plugin {
                    ui.add(
                        egui::Label::new(RichText::new(plugin_read_only(p)).color(pal.text_muted))
                            .wrap(),
                    );
                    ui.add_space(8.0);
                }
                // A local or project server that runs where a user server of its name is hidden.
                let replaces = s.scope != Scope::User
                    && !s.shadowed
                    && s.project.as_ref().is_some_and(|p| {
                        self.snapshot.servers.iter().any(|u| {
                            u.scope == Scope::User && u.name == s.name && u.shadowed_in.contains(p)
                        })
                    });
                egui::Grid::new("general")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .max_col_width(col_width)
                    .show(ui, |ui| {
                        let mut kv = |k: &str, v: String| {
                            ui.label(RichText::new(k).color(pal.text_muted));
                            let value = ui.add(egui::Label::new(v).wrap());
                            ui.end_row();
                            value
                        };
                        kv("Scope", s.scope.label().to_owned());
                        match &s.plugin {
                            Some(p) => {
                                kv("Plugin", format!("{}, from {}", p.plugin, p.marketplace));
                                if !p.off_in.is_empty() {
                                    kv("Plugin on", plugin_on_text(p, self.snapshot.project_count))
                                        .on_hover_text(format!("Off in:\n{}", p.off_in.join("\n")))
                                } else {
                                    kv("Plugin on", "in every project".to_owned())
                                }
                            }
                            None => match &s.managed {
                                Some(file) => kv("Set in", file.clone()),
                                None => {
                                    kv("Project", s.project.clone().unwrap_or_else(|| "-".into()))
                                }
                            },
                        };
                        kv("Type", s.transport.label().to_owned());
                        let target = kv("Target", s.target.clone());
                        // As Claude Code runs it; what the plugin writes is a hover away.
                        if let Some(written) =
                            s.plugin.as_ref().and_then(|p| p.written_target.as_deref())
                        {
                            target.on_hover_text(format!("The plugin writes: {written}"));
                        }
                        let enabled = if s.scope.is_everywhere() && s.enabled && s.disabled_in > 0 {
                            if s.disabled_projects.is_empty() {
                                format!(
                                    "disabled in {} of {} projects",
                                    s.disabled_in, self.snapshot.project_count
                                )
                            } else {
                                format!(
                                    "disabled {}",
                                    in_projects_text(
                                        &s.disabled_projects,
                                        self.snapshot.project_count,
                                        false
                                    )
                                )
                            }
                        } else if plugin_off_everywhere(s, self.snapshot.project_count) {
                            "yes, but its plugin is off in every project".to_owned()
                        } else if s.enabled {
                            "yes".to_owned()
                        } else {
                            "no".to_owned()
                        };
                        let enabled = kv("Enabled", enabled);
                        if !s.disabled_projects.is_empty() {
                            enabled.on_hover_text(projects_paths(&s.disabled_projects));
                        }
                        if let Some(approval) = s.approval {
                            kv("Approval", approval.label().to_owned());
                        }
                        // The organisation's servers win over every other of their name: never hidden, and what
                        // they hide is said where they are.
                        if s.scope == Scope::Managed {
                            let replaced = replaced_by_organisation(&self.snapshot.servers, s);
                            if !replaced.is_empty() {
                                kv("Replaces", replaced.join("; "));
                            }
                        } else {
                            let shadowed = kv(
                                if s.scope == Scope::Plugin {
                                    "Left out"
                                } else {
                                    "Shadowed"
                                },
                                shadowed_text(s, self.snapshot.project_count),
                            );
                            if !s.shadowed_in.is_empty() {
                                shadowed.on_hover_text(shadowed_paths(s));
                            }
                        }
                        if replaces {
                            kv("Replaces", format!("the user server {}", s.name));
                        }
                        // Everywhere, the reason leads the details already.
                        if let Some(b) = s.blocked.as_ref().filter(|_| s.not_loaded.is_none()) {
                            let count = self.snapshot.project_count;
                            kv(
                                if b.policy {
                                    "Blocked"
                                } else {
                                    "Connectors off"
                                },
                                format!(
                                    "{}. {}",
                                    in_projects_text(&b.projects, count, false),
                                    b.why
                                ),
                            )
                            .on_hover_text(projects_paths(&b.projects));
                        }
                        // What Klefki cannot check: said, as a note.
                        if let Some(why) = &s.may_not_load {
                            kv("May not load", why.clone());
                        }
                    });
                if s.scope == Scope::ClaudeAi {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(RichText::new(CONNECTOR_READ_ONLY).color(pal.text_muted))
                            .wrap(),
                    );
                    egui::CollapsingHeader::new("About claude.ai connectors")
                        .id_salt("about-connectors")
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(ABOUT_CONNECTORS).color(pal.text_muted),
                                )
                                .wrap(),
                            );
                        });
                }
                if s.casing_collision && self.core.is_some() {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Another server's name differs from this one only in upper and lower case.",
                            )
                            .color(pal.warn),
                        )
                        .wrap(),
                    );
                    if ui
                        .add_enabled(self.can_prepare(), egui::Button::new("Fix casing…"))
                        .on_hover_text("Give them all the same spelling")
                        .clicked()
                    {
                        self.open_casing();
                    }
                }
                if s.scope == Scope::User && !s.relative_paths.is_empty() {
                    ui.add_space(8.0);
                    // What to do first, with the paths as values; why under it, quieter.
                    ui.add(egui::Label::new(relative_job(ui, &s.relative_paths, pal.warn)).wrap());
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Claude Code starts user servers in each session's folder. Klefki checks them \
                                 from your home folder, so the check can pass here.",
                            )
                            .color(pal.text_muted),
                        )
                        .wrap(),
                    );
                }
                if s.scope == Scope::Project
                    && s.approval.is_some_and(|a| a != Approval::Approved)
                    && self.core.is_some()
                {
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(self.can_prepare(), egui::Button::new("Review and approve…"))
                        .on_hover_text(
                            "See everything this server would run, then let Claude Code run it",
                        )
                        .clicked()
                    {
                        self.prepare(vec![Change::Approve { id: s.id() }]);
                    }
                }
                // Where the user lands first: the way out of "Needs auth" is here too, not only on the Status tab.
                if status == Status::NeedsAuth && self.core.is_some() && sign_in::can_sign_in(s) {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new("This server needs you to sign in.").color(pal.warn),
                        )
                        .wrap(),
                    );
                    let button =
                        egui::Button::new(RichText::new("Sign in").strong().color(pal.on_bright))
                            .fill(pal.primary);
                    if ui
                        .add_enabled(self.sign_in.is_none(), button)
                        .on_hover_text("Sign in through Claude Code, in your browser")
                        .clicked()
                    {
                        self.start_sign_in(s);
                    }
                }
                if !s.scope.is_read_only() && self.core.is_some() {
                    ui.add_space(12.0);
                    let form = ui.scope(|ui| self.definition_form(ui, s)).response;
                    self.reveal_definition(s, &form);
                }
            }
            DetailTab::Status => {
                egui::Grid::new("status")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .max_col_width(col_width)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Status").color(pal.text_muted));
                        if s.not_loaded.is_some() {
                            ui.label(NOT_LOADED);
                        } else if s.scope.is_read_only() {
                            ui.label(PLUGIN_STATUS);
                        } else if self.last_known(i) {
                            ui.label(format!("{} (last known)", status.label()));
                        } else {
                            ui.label(status.label());
                        }
                        ui.end_row();
                        if explanation.is_none() {
                            ui.label(RichText::new("Detail").color(pal.text_muted));
                            ui.add(egui::Label::new(detail.unwrap_or("-")).wrap());
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                if s.scope == Scope::Project && s.transport == TransportKind::Stdio {
                    ui.label(
                        RichText::new(
                            "Checking runs this server's command from its repository, so it only happens when you ask.",
                        )
                        .color(pal.warn),
                    );
                }
                if let Some(why) = not_checked(s) {
                    ui.add(egui::Label::new(RichText::new(why).color(pal.text_muted)).wrap());
                    return;
                }
                let can_check = self.core.is_some() && status != Status::Checking;
                ui.horizontal_wrapped(|ui| {
                if self.core.is_some() && sign_in::offers_sign_in(s, status) {
                    // The way out of "Needs auth", so it leads; otherwise it is one action among others.
                    let button = if status == Status::NeedsAuth {
                        egui::Button::new(RichText::new("Sign in").strong().color(pal.on_bright)).fill(pal.primary)
                    } else {
                        egui::Button::new("Sign in")
                    };
                    if ui
                        .add_enabled(self.sign_in.is_none(), button)
                        .on_hover_text("Sign in through Claude Code, in your browser")
                        .clicked()
                    {
                        self.start_sign_in(s);
                    }
                }
                if ui
                    .add_enabled(can_check, egui::Button::new("Check now"))
                    .on_hover_text("Check the status again")
                    .clicked()
                {
                    self.send(Command::Check(s.id()));
                }
                if ui
                    .add_enabled(can_check, egui::Button::new("Diagnose"))
                    .on_hover_text(
                        "Run the check again with the client's own debug output and explain what went wrong",
                    )
                    .clicked()
                {
                    self.send(Command::Diagnose(s.id()));
                }
                });
                // Under the actions, so they are in view without scrolling, even at the smallest window.
                if let Some(explanation) = &explanation {
                    ui.add_space(10.0);
                    explanation.show(ui, ui.id().with("status"));
                }
                if let Some(d) = self.diagnoses.get(&s.id()) {
                    let shown = ui.scope(|ui| diagnosis_panel(ui, s, d)).response;
                    if self.reveal_diagnosis.take_if(|id| s.is(id)).is_some() {
                        shown.scroll_to_me(Some(Align::TOP));
                    }
                }
            }
            DetailTab::Auth => {
                let muted = |ui: &mut Ui, text: &str| {
                    ui.add(egui::Label::new(RichText::new(text).color(pal.text_muted)).wrap());
                };
                match &s.oauth {
                    Some(oauth) => {
                        egui::Grid::new("oauth")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .max_col_width(col_width)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Signs in").color(pal.text_muted));
                                ui.label("with OAuth");
                                ui.end_row();
                                ui.label(RichText::new("Client id").color(pal.text_muted));
                                match &oauth.client_id {
                                    Some(id) => ui.add(
                                        egui::Label::new(RichText::new(id).monospace()).wrap(),
                                    ),
                                    None => ui.label(
                                        RichText::new("none: the client registers itself")
                                            .color(pal.text_muted),
                                    ),
                                };
                                ui.end_row();
                                ui.label(RichText::new("Scopes").color(pal.text_muted));
                                if oauth.scopes.is_empty() {
                                    ui.label(RichText::new("none listed").color(pal.text_muted));
                                } else {
                                    ui.add(egui::Label::new(oauth.scopes.join(", ")).wrap());
                                }
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        if self.core.is_some() {
                            self.client_secret(ui, s);
                            ui.add_space(8.0);
                        }
                    }
                    None if sign_in::can_sign_in(s) => muted(
                        ui,
                        "Claude Code signs in to this server for you when the server asks for it.",
                    ),
                    None => muted(
                        ui,
                        "This server does not sign in with OAuth. Headers and tokens are in the Env tab.",
                    ),
                }
                if self.core.is_some() && sign_in::can_sign_in(s) {
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(self.sign_in.is_none(), egui::Button::new("Sign in"))
                        .on_hover_text("Sign in through Claude Code, in your browser")
                        .clicked()
                    {
                        self.start_sign_in(s);
                    }
                    muted(ui, "Klefki starts the sign-in and never sees it.");
                }
            }
            DetailTab::HeadersEnv => {
                if self.core.is_some() {
                    let form = ui.scope(|ui| self.values_form(ui, s)).response;
                    self.reveal_definition(s, &form);
                } else {
                    ui.label(RichText::new("Not available for sample data.").color(pal.text_muted));
                }
            }
        }
    }

    /// Edit (toolbar, Ctrl+E, palette): the selected server's definition, in the form on the General tab.
    fn edit_selected(&mut self) {
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
    fn client_secret(&mut self, ui: &mut Ui, s: &ServerRow) {
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
    fn drafted(&self) -> Option<ServerId> {
        self.editor
            .as_ref()
            .filter(|e| matches!(&e.state, EditorState::Ready { text, original, .. } if text != original))
            .map(|e| e.id.clone())
    }

    /// Asks the engine for `s`'s definition, for the form and the JSON editor.
    fn open_editor(&mut self, s: &ServerRow) {
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
    fn reveal_definition(&self, s: &ServerRow, shown: &egui::Response) {
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
    fn definition_form(&mut self, ui: &mut Ui, s: &ServerRow) {
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
    fn values_form(&mut self, ui: &mut Ui, s: &ServerRow) {
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
    fn save_bar(&mut self, ui: &mut Ui, s: &ServerRow) {
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
            ui.add(egui::Label::new(RichText::new(sentence(problem)).color(pal.danger)).wrap());
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

/// Plain text a user can paste into a support request. The engine already sanitised and redacted it.
fn diagnosis_report(row: &ServerRow, d: &Diagnosis) -> String {
    use std::fmt::Write;
    let mut out = String::from("Klefki diagnostic report\n");
    let place = match &row.project {
        Some(project) => format!("{}, {project}", row.scope.label()),
        None => row.scope.label().to_owned(),
    };
    let _ = writeln!(out, "server: {} ({place})", row.name);
    let _ = writeln!(out, "transport: {}", row.transport.label());
    let _ = match &d.detail {
        Some(detail) => writeln!(out, "status: {}: {detail}", d.status.label()),
        None => writeln!(out, "status: {}", d.status.label()),
    };
    if let Some(note) = &d.note {
        let _ = writeln!(out, "note: {note}");
    }
    if !d.hints.is_empty() {
        out.push_str("possible causes:\n");
        for hint in &d.hints {
            out.push('\n');
            out.push_str(&Explanation::for_hint(row, d.status, hint, d.detail.as_deref()).text());
        }
        out.push('\n');
    }
    if !d.server_said.is_empty() {
        out.push_str("the server said:\n");
        for line in &d.server_said {
            let _ = writeln!(out, "  {line}");
        }
    }
    if !d.client_log.is_empty() {
        out.push_str("client log:\n");
        for line in &d.client_log {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}

/// What the client's debug output said, in the order a person needs it: the likely cause, what the server
/// printed itself, and the raw log behind a collapsed header.
fn diagnosis_panel(ui: &mut Ui, row: &ServerRow, d: &Diagnosis) {
    let pal = theme::palette(ui.ctx());
    ui.add_space(10.0);
    ui.separator();
    if let Some(note) = &d.note {
        ui.label(RichText::new(note).color(pal.warn));
    }
    if !d.hints.is_empty() {
        ui.label(
            RichText::new("What probably went wrong")
                .family(theme::bold_family())
                .size(16.0),
        );
        for (i, hint) in d.hints.iter().enumerate() {
            Explanation::for_hint(row, d.status, hint, d.detail.as_deref())
                .show(ui, ui.id().with(("cause", i)));
            ui.add_space(6.0);
        }
    }
    if !d.server_said.is_empty() {
        ui.label(
            RichText::new("The server said")
                .family(theme::bold_family())
                .size(16.0),
        );
        for line in &d.server_said {
            ui.label(RichText::new(line).monospace());
        }
        ui.add_space(4.0);
    }
    if !d.client_log.is_empty() {
        egui::CollapsingHeader::new("Client log")
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::both().max_height(220.0).show(ui, |ui| {
                    for line in &d.client_log {
                        ui.add(egui::Label::new(RichText::new(line).monospace().small()).extend());
                    }
                });
            });
    }
    ui.add_space(6.0);
    if ui
        .button("Copy report")
        .on_hover_text("Copy this explanation as plain text. Secrets are already removed.")
        .clicked()
    {
        ui.ctx().copy_text(diagnosis_report(row, d));
    }
}

/// The outcome message's color and glyph: a problem, a warning or a success.
fn outcome_look(pal: &Palette, outcome: &Outcome) -> (Color32, &'static str) {
    if outcome.type_path {
        (pal.primary, icon::INFO)
    } else if outcome.ok && outcome.warn {
        (pal.warn, icon::WARNING)
    } else if outcome.ok {
        (pal.success, icon::CHECK_CIRCLE)
    } else {
        (pal.danger, icon::WARNING_CIRCLE)
    }
}

/// The outcome message's card, edged in its color.
fn outcome_frame(pal: &Palette, color: Color32) -> Frame {
    Frame::new()
        .fill(pal.panel_raised)
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(8)
        .inner_margin(Margin::same(12))
}

fn bar_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.chrome)
        .stroke(egui::Stroke::new(1.0, pal.divider))
        .inner_margin(Margin::symmetric(12, 6))
}

fn search_id() -> egui::Id {
    egui::Id::new("search")
}

/// Room kept free at the right of the detail panel's content: its scroll bar floats over the content and, at its
/// widest when the pointer is on it, takes the clicks there.
fn scroll_gutter(style: &egui::Style) -> i8 {
    let bar = &style.spacing.scroll;
    (bar.bar_width + bar.bar_outer_margin).ceil() as i8
}

/// Scrolls the scroll area `ui` is in to a widget in it that has just gained keyboard focus. egui does not, as GTK
/// and WinUI do, so Tab could take focus below the window. Focus gained anywhere else is left to its own place.
fn bring_focus_into_view(ui: &Ui) {
    if let Some(r) = ui
        .memory(|m| m.focused())
        .and_then(|id| ui.ctx().read_response(id))
        && r.gained_focus()
        && ui.min_rect().contains_rect(r.rect)
    {
        ui.scroll_to_rect(r.rect, None);
    }
}

/// What the Fix casing dialog holds: the servers that differ only in case, and the spelling they are to get.
struct CasingForm {
    members: Vec<CasingMember>,
    keep: String,
}

struct CasingMember {
    id: ServerId,
    name: String,
    scope: Scope,
    project: Option<String>,
}

impl CasingForm {
    /// Each spelling once, in the order the servers are listed.
    fn spellings(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for m in &self.members {
            if !seen.contains(&m.name) {
                seen.push(m.name.clone());
            }
        }
        seen
    }

    /// The spellings of project servers: they live in a repository's `.mcp.json`, which Klefki does not edit.
    fn fixed(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for m in self.members.iter().filter(|m| m.scope == Scope::Project) {
            if !out.contains(&m.name.as_str()) {
                out.push(&m.name);
            }
        }
        out
    }

    /// Whether every server could get this spelling.
    fn allowed(&self, spelling: &str) -> bool {
        self.fixed().iter().all(|f| *f == spelling)
    }

    /// Whether any spelling can be given to all: not when project servers disagree among themselves.
    fn fixable(&self) -> bool {
        self.fixed().len() <= 1
    }

    /// Renames the servers that Klefki can rename and that do not have the spelling yet.
    fn changes(&self) -> Vec<Change> {
        self.members
            .iter()
            .filter(|m| m.scope != Scope::Project && m.name != self.keep)
            .map(|m| Change::Rename {
                id: m.id.clone(),
                name: self.keep.clone(),
            })
            .collect()
    }
}

/// How the Add dialog takes a server: pasted (a command or JSON, several at once), or filled in, part by part; or
/// read from a file (Import), which the dialog is then about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AddMode {
    #[default]
    Paste,
    Fill,
    Import,
}

/// A field of the form, for what is said under it and for giving it the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Field {
    Name,
    Part(Part),
    Secret,
}

fn field_id(field: Field) -> egui::Id {
    egui::Id::new(("add_field", field))
}

/// The client secret's text field, and each value row's: what egui remembers of them (undo steps) is forgotten
/// when the dialog closes, so a secret does not outlive the form.
fn secret_id() -> egui::Id {
    egui::Id::new("add_client_secret")
}

fn value_id(noun: &str, row: usize) -> egui::Id {
    egui::Id::new(("add_value", noun, row))
}

/// The most servers an import holds (a paste of more is refused), so the most client secret fields it has.
const IMPORT_ROWS: usize = 100;

/// The most value rows whose text-field memory is forgotten on close; a server has a handful.
const FORGET_ROWS: usize = 64;

/// What the value rows remember between frames: the rows shown as typed, and the value last replaced.
#[derive(Default)]
struct RowsState {
    shown: BTreeSet<usize>,
    replaced: Option<Replaced>,
}

/// A value replaced by a `${VAR}` reference: what to do next, and the way back.
struct Replaced {
    row: usize,
    variable: String,
    before: Secret,
}

/// What the Add dialog holds: the pasted text, what was read from it and the names to add those servers under; or
/// the form's server; and where they go.
struct AddForm {
    mode: AddMode,
    text: String,
    /// The text `read` was read from: it is read again only when the text changes.
    read_for: String,
    /// The servers found, or why there are none (empty while nothing is pasted).
    read: Result<Pasted, String>,
    /// One name per server read, as the user may have changed it.
    names: Vec<String>,
    /// The form's server: its name and parts.
    name: String,
    /// The name was typed; until then it follows what the server connects to or runs.
    name_edited: bool,
    fields: ServerFields,
    /// The form's OAuth client secret, typed hidden. Wiped from memory when dropped; handed to the engine once.
    secret: Secret,
    /// The client secret is shown as typed, to check a paste.
    show_secret: bool,
    /// Which value rows are shown as typed, and the value last replaced by a reference.
    rows: RowsState,
    /// What `fields` make under `name`, and every problem with them, read again only when either changes.
    filled: Option<Filled>,
    places: Places,
    /// A place was ticked by hand: what is pasted next no longer moves the ticks.
    places_chosen: bool,
    /// Give the first field the keyboard on the next frame.
    focus: bool,
    /// Give this field the keyboard, and bring it into view, on the next frame.
    focus_on: Option<Field>,
    /// Fields the keyboard has left: what is wrong with them is said from then on, not while they are typed in.
    left: BTreeSet<Field>,
    /// Review was pressed: everything that stops it is said.
    tried: bool,
    /// Review asked for a plan: the dialog waits behind it, as it was, to come back to.
    reviewing: bool,
    /// Esc was pressed with something pasted or typed: the dialog asks before throwing it away.
    asking_to_discard: bool,
    /// The field that had the keyboard before the question, to give it back once the question is put away.
    focus_back: Option<egui::Id>,
    /// What had the keyboard at the end of the last frame: Esc takes it away before the dialog sees the key.
    focus_was: Option<egui::Id>,
    /// Give Keep editing the keyboard on the next frame.
    focus_answer: bool,
    /// Why the last review or add failed, said in the dialog.
    failed: Option<String>,
    /// Bring the OAuth settings into view on the next frame.
    reveal_oauth: bool,
    /// The callback port to check, sent to the engine after this frame: when the field is left or Review is
    /// pressed, not while it is typed in.
    port_wanted: Option<u16>,
    /// The callback port sent to be checked, until its answer comes: it is asked once.
    port_pending: Option<u16>,
    /// The callback port last checked, and why a sign-in could not come back to it ([`Event::CallbackPort`]).
    port_checked: Option<(u16, Option<String>)>,
    /// Import: the file read, as it can be shown.
    file: Option<String>,
    /// Import: why the last file chosen could not be read.
    file_problem: Option<String>,
    /// Import: no open dialog could be shown, so a path is typed.
    no_dialog: bool,
    /// Import: the path is typed in, as asked for after a file dialog closed without a file.
    typing: bool,
    path: String,
    /// Import: Read was pressed for the path typed; it is sent to the engine after this frame.
    path_wanted: bool,
    /// Import, one per server read: whether it is imported.
    include: Vec<bool>,
    /// Import, one per server read: a server already there under its name is replaced rather than kept.
    replace: Vec<bool>,
    /// Import, one per server read: its client secret, typed hidden. Only one that signs in with OAuth gets one.
    secrets: Vec<Secret>,
    /// Import: the server whose client secret is shown as typed.
    secret_shown: Option<usize>,
}

struct Filled {
    name: String,
    fields: ServerFields,
    read: PastedServer,
    problems: Vec<(Part, String)>,
}

/// Whether one server can be added.
#[derive(Debug, PartialEq)]
enum Check {
    Ready,
    /// It cannot be added as it is, and is left out.
    Skipped(String),
    /// Its name needs changing before anything is added.
    Name(String),
    /// Pasted, it asks for a client secret: it is finished in the form, where the secret is typed.
    FillIn(String),
}

/// Import: a server whose name is taken in a ticked place.
struct Conflict {
    /// Where, in words: `for the user`, `in app`.
    place: String,
    /// A name it could be imported under instead.
    free: String,
}

/// Import: what Review sends, and what it comes to, by server name.
#[derive(Default)]
struct ImportPlan {
    /// Built only for Review (`build`): each holds a server's whole definition.
    changes: Vec<Change>,
    /// Something would be written.
    writes: bool,
    /// Added where there is none of that name yet.
    adds: Vec<String>,
    /// Replacing the one there, in those places.
    replaces: Vec<(String, Vec<Target>)>,
    /// Left as they are where there is one of that name already.
    keeps: Vec<String>,
}

impl ImportPlan {
    /// One line: what is added where, and what is replaced or kept.
    fn in_words(&self, places: &str) -> String {
        let mut out = Vec::new();
        if !self.adds.is_empty() {
            let names: Vec<&str> = self.adds.iter().map(String::as_str).collect();
            out.push(format!(
                "Adds {} {places}.",
                list_in_words(&names, "server")
            ));
        }
        if !self.replaces.is_empty() {
            let names: Vec<&str> = self.replaces.iter().map(|(n, _)| n.as_str()).collect();
            let mut user = false;
            let mut folders: Vec<&str> = Vec::new();
            for target in self.replaces.iter().flat_map(|(_, t)| t) {
                match target {
                    Target::User => user = true,
                    Target::Local(p) => {
                        let folder = folder_name(p);
                        if !folders.contains(&folder) {
                            folders.push(folder);
                        }
                    }
                }
            }
            out.push(format!(
                "Replaces {} {}.",
                list_in_words(&names, "server"),
                places_in_words(user, &folders).unwrap_or_default()
            ));
        }
        if !self.keeps.is_empty() {
            let names: Vec<&str> = self.keeps.iter().map(String::as_str).collect();
            out.push(format!(
                "Keeps the {} that {} there.",
                list_in_words(&names, "server"),
                if names.len() == 1 { "is" } else { "are" },
            ));
        }
        out.join(" ")
    }
}

/// The servers already in the ticked places, by name: where each is, as a target, and which it is.
struct Existing<'a> {
    by_name: HashMap<&'a str, Vec<(Target, ServerId)>>,
}

impl<'a> Existing<'a> {
    fn new(places: &Places, servers: &'a [ServerRow]) -> Self {
        let ticked: Vec<&str> = places.ticked().collect();
        let mut by_name: HashMap<&str, Vec<(Target, ServerId)>> = HashMap::new();
        for s in servers {
            let at = match (s.scope, s.project.as_deref()) {
                (Scope::User, None) if places.user => Some(Target::User),
                (Scope::Local, Some(project)) => ticked
                    .iter()
                    .find(|t| {
                        klefki_core::api::project_key(t) == klefki_core::api::project_key(project)
                    })
                    .map(|t| Target::Local((*t).to_owned())),
                _ => None,
            };
            if let Some(at) = at {
                by_name
                    .entry(s.name.as_str())
                    .or_default()
                    .push((at, s.id()));
            }
        }
        Self { by_name }
    }

    fn of(&self, name: &str) -> Vec<(Target, ServerId)> {
        self.by_name.get(name).cloned().unwrap_or_default()
    }
}

/// An imported server's client secret field: what egui remembers of it is forgotten when the dialog closes.
fn import_secret_id(row: usize) -> egui::Id {
    egui::Id::new(("import_client_secret", row))
}

impl AddForm {
    fn new(mode: AddMode, places: Places) -> Self {
        Self {
            mode,
            text: String::new(),
            read_for: String::new(),
            read: Err(String::new()),
            names: Vec::new(),
            name: String::new(),
            name_edited: false,
            fields: ServerFields::default(),
            secret: Secret::new(String::new()),
            show_secret: false,
            rows: RowsState::default(),
            filled: None,
            places,
            places_chosen: false,
            focus: true,
            focus_on: None,
            left: BTreeSet::new(),
            tried: false,
            reviewing: false,
            asking_to_discard: false,
            focus_back: None,
            focus_was: None,
            focus_answer: false,
            failed: None,
            reveal_oauth: false,
            port_wanted: None,
            port_pending: None,
            port_checked: None,
            file: None,
            file_problem: None,
            no_dialog: false,
            typing: false,
            path: String::new(),
            path_wanted: false,
            include: Vec::new(),
            replace: Vec::new(),
            secrets: Vec::new(),
            secret_shown: None,
        }
    }

    /// Import: takes what came of asking for a file. A file that could not be read leaves what was read before.
    fn load(&mut self, file: ImportFile) {
        match file {
            ImportFile::Read { shown, text } => {
                self.file = Some(shown);
                self.file_problem = None;
                self.no_dialog = false;
                self.typing = false;
                self.failed = None;
                self.tried = false;
                self.text = text;
                // Read again even when the text is the same: a file chosen again starts afresh.
                self.read_for = String::from("\0");
                self.reread();
            }
            ImportFile::Failed { why, .. } => self.file_problem = Some(why),
            ImportFile::NoDialog => {
                self.no_dialog = true;
                self.focus = true;
            }
            ImportFile::Cancelled => {}
        }
    }

    /// Something was pasted or typed that closing would throw away. How the server connects and where it goes are
    /// choices, quickly made again, not typing.
    fn has_typing(&self) -> bool {
        if self.mode == AddMode::Import {
            // The file is there to be read again; only what was typed is lost.
            return self.secrets.iter().any(|s| !s.is_empty())
                || ((self.no_dialog || self.typing) && !self.path.trim().is_empty());
        }
        let empty = |rows: &[(String, String)]| {
            rows.iter()
                .all(|(k, v)| k.trim().is_empty() && v.trim().is_empty())
        };
        let fields = ServerFields {
            transport: ServerFields::default().transport,
            oauth: ServerFields::default().oauth,
            ..self.fields.clone()
        };
        let fields = ServerFields {
            env: if empty(&fields.env) {
                Vec::new()
            } else {
                fields.env
            },
            headers: if empty(&fields.headers) {
                Vec::new()
            } else {
                fields.headers
            },
            ..fields
        };
        !self.text.trim().is_empty()
            || (self.name_edited && !self.name.trim().is_empty())
            || fields != ServerFields::default()
            || !self.secret.is_empty()
    }

    /// Reads the text again if it changed. A name the user changed is kept while the server it names is still read
    /// with the same name in the same place; the ticks follow what the text asks for until one is set by hand.
    fn reread(&mut self) {
        if self.text == self.read_for {
            return;
        }
        let read = if self.text.trim().is_empty() {
            Err(String::new())
        } else {
            Pasted::parse(&self.text)
        };
        let before: &[PastedServer] = self.read.as_ref().map_or(&[], |r| r.servers.as_slice());
        let names = match &read {
            Ok(r) => r
                .servers
                .iter()
                .enumerate()
                .map(|(i, s)| match before.get(i) {
                    Some(b) if b.name == s.name => self.names[i].clone(),
                    _ => s.name.clone(),
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        if let (Ok(r), false) = (&read, self.places_chosen) {
            // Local, Claude Code's default, means the project it is run in: the selected server's, if any.
            let local =
                !r.servers.is_empty() && r.servers.iter().all(|s| s.scope == Some(Scope::Local));
            for (_, on) in &mut self.places.projects {
                *on = false;
            }
            self.places.user = !(local && self.places.tick_here());
        }
        self.names = names;
        let count = read.as_ref().map_or(0, |r| r.servers.len());
        self.include = vec![true; count];
        self.replace = vec![false; count];
        self.secrets = vec![Secret::new(String::new()); count];
        self.secret_shown = None;
        self.read = read;
        self.read_for.clone_from(&self.text);
    }

    /// Reads the form's server again if its name or parts changed. Until a name is typed, it is the one the parts
    /// suggest.
    fn refill(&mut self) {
        self.fields.has_secret = !self.secret.is_empty();
        let parts_changed = self.filled.as_ref().is_none_or(|f| f.fields != self.fields);
        if parts_changed && !self.name_edited {
            self.name = self.fields.suggested_name();
        }
        let current = self
            .filled
            .as_ref()
            .is_some_and(|f| f.name == self.name && f.fields == self.fields);
        if !current {
            self.filled = Some(Filled {
                name: self.name.clone(),
                fields: self.fields.clone(),
                read: self.fields.read(&self.name),
                problems: self.fields.problems(),
            });
        }
    }

    /// What Review sends, read and checked as the dialog is now; `None` while something stops it.
    fn changes(&mut self, servers: &[ServerRow]) -> Option<Vec<Change>> {
        self.reread();
        self.refill();
        let checked = self.check(servers);
        let targets = self.places.targets();
        if targets.is_empty() || checked.iter().any(|c| matches!(c, Check::Name(_))) {
            return None;
        }
        let changes: Vec<Change> = match self.mode {
            AddMode::Paste => {
                let read = self.read.as_ref().ok()?;
                (0..checked.len())
                    .filter(|&i| checked[i] == Check::Ready)
                    .map(|i| Change::Add {
                        name: self.names[i].clone(),
                        json: read.servers[i].json.clone(),
                        targets: targets.clone(),
                        secret: None,
                    })
                    .collect()
            }
            AddMode::Import => self.import_plan(servers, &checked, true)?.changes,
            AddMode::Fill => {
                let filled = self.filled.as_ref()?;
                if checked != [Check::Ready] {
                    return None;
                }
                vec![Change::Add {
                    name: self.name.clone(),
                    json: filled.read.json.clone(),
                    targets,
                    // Only a server that signs in with OAuth has one; an empty field means none.
                    secret: (filled.fields.oauth
                        && filled.fields.transport != TransportKind::Stdio
                        && !self.secret.is_empty())
                    .then(|| self.secret.clone()),
                }]
            }
        };
        (!changes.is_empty()).then_some(changes)
    }

    /// Whether what is added signs in with OAuth, so the user is reminded to sign in once it is added.
    fn signs_in(&self) -> bool {
        match self.mode {
            AddMode::Paste => self
                .read
                .as_ref()
                .is_ok_and(|r| r.servers.iter().any(|s| s.oauth)),
            AddMode::Fill => self.filled.as_ref().is_some_and(|f| f.read.oauth),
            AddMode::Import => self.read.as_ref().is_ok_and(|r| {
                r.servers
                    .iter()
                    .zip(&self.include)
                    .any(|(s, &on)| on && s.oauth)
            }),
        }
    }

    /// Each server, in order: ready, left out, or waiting for a better name. The form has one.
    fn check(&self, servers: &[ServerRow]) -> Vec<Check> {
        // Runs on every frame: the rows are gone through once for all the names.
        let taken = Taken::new(&self.places, servers.iter());
        match self.mode {
            AddMode::Paste => {
                let Ok(read) = &self.read else {
                    return Vec::new();
                };
                read.servers
                    .iter()
                    .enumerate()
                    .map(|(i, server)| {
                        if let Some(problem) = &server.problem {
                            return Check::Skipped(problem.clone());
                        }
                        if server.asks_secret {
                            return if ServerFields::from_definition(&server.json).is_some() {
                                Check::FillIn(if server.word_left_out {
                                    "The word after --client-secret was left out, as it may be the secret, and the name with it. Fill it in: type the secret there, and check the name."
                                } else {
                                    "It asks for a client secret. Fill it in to add it, and type the secret there."
                                }
                                .to_owned())
                            } else {
                                Check::Skipped(
                                    "It asks for a client secret and has settings the form does not show. Add it with Claude Code's own command, which asks for the secret."
                                        .to_owned(),
                                )
                            };
                        }
                        let earlier = (0..i)
                            .filter(|&j| {
                                read.servers[j].problem.is_none() && !read.servers[j].asks_secret
                            })
                            .map(|j| self.names[j].as_str());
                        Self::name_check(&self.names[i], earlier, &taken)
                    })
                    .collect()
            }
            AddMode::Import => self.import_check(),
            AddMode::Fill => {
                let Some(filled) = &self.filled else {
                    return Vec::new();
                };
                vec![match (
                    Self::name_check(&self.name, std::iter::empty(), &taken),
                    &filled.read.problem,
                ) {
                    (Check::Ready, Some(problem)) => Check::Skipped(problem.clone()),
                    (check, _) => check,
                }]
            }
        }
    }

    /// Whether `name` can be added, after the names `earlier` in the same paste, to the places ticked.
    fn name_check<'a>(
        name: &str,
        mut earlier: impl Iterator<Item = &'a str>,
        taken: &Taken,
    ) -> Check {
        if name.is_empty() {
            return Check::Name("Give it a name.".to_owned());
        }
        if let Some(problem) = name_problem(name) {
            return Check::Name(problem);
        }
        if earlier.any(|e| e == name) {
            return Check::Name(format!("{name} is named twice in what you pasted."));
        }
        if let Some(taken) = taken.of(name) {
            return Check::Name(format!(
                "{name} already exists {taken}. Choose another name, or untick it there."
            ));
        }
        Check::Ready
    }

    /// The names of the servers at `addable`, for the line above the buttons.
    fn names_to_add(&self, addable: &[usize]) -> Vec<&str> {
        match self.mode {
            AddMode::Paste | AddMode::Import => {
                addable.iter().map(|&i| self.names[i].as_str()).collect()
            }
            AddMode::Fill => vec![self.name.as_str()],
        }
    }

    /// Review was pressed but cannot go ahead: says everything that stops it, and gives the first field that
    /// needs work the keyboard.
    fn show_what_stops_review(&mut self, checked: &[Check]) {
        self.tried = true;
        if self.mode != AddMode::Fill {
            return;
        }
        // It may have been taken, or let go, since the field was left.
        self.check_port();
        let name = matches!(checked.first(), Some(Check::Name(_))).then_some(Field::Name);
        let part = self
            .filled
            .as_ref()
            .and_then(|f| f.problems.first())
            .map(|(p, _)| Field::Part(*p));
        // In the order the form shows them: what it runs or connects to comes before the name.
        self.focus_on = match (part, name) {
            (Some(Field::Part(Part::Command | Part::Address)), _) => part,
            (_, Some(name)) => Some(name),
            (part, None) => part,
        };
    }

    /// Moves pasted server `i` into the form, to finish it there. False when the form cannot hold all of it.
    fn fill_in_from(&mut self, i: usize) -> bool {
        let Some(server) = self.read.as_ref().ok().and_then(|r| r.servers.get(i)) else {
            return false;
        };
        let Some(fields) = ServerFields::from_definition(&server.json) else {
            return false;
        };
        self.fields = fields;
        self.name = self.names[i].clone();
        // A paste whose name was left out (it may have been the secret) gets one from its address, as typed does.
        self.name_edited = !self.name.is_empty();
        self.secret = Secret::new(String::new());
        self.mode = AddMode::Fill;
        // Offered only when the secret is asked for: the keyboard goes there.
        self.focus_on = Some(Field::Secret);
        self.reveal_oauth = true;
        true
    }

    /// Import: each server, in order. One left unticked is skipped without a reason; one already in a ticked place
    /// is ready, as what happens to it there is chosen beside it.
    fn import_check(&self) -> Vec<Check> {
        let Ok(read) = &self.read else {
            return Vec::new();
        };
        let counts = |j: usize| read.servers[j].problem.is_none() && self.include[j];
        read.servers
            .iter()
            .enumerate()
            .map(|(i, server)| {
                if let Some(problem) = &server.problem {
                    return Check::Skipped(problem.clone());
                }
                if !self.include[i] {
                    return Check::Skipped(String::new());
                }
                if server.asks_secret && self.secrets[i].is_empty() {
                    return Check::Name(
                        "Type the client secret it asks for, or untick it.".to_owned(),
                    );
                }
                let name = &self.names[i];
                if (0..i).any(|j| counts(j) && self.names[j] == *name) {
                    return Check::Name(format!("{name} is in the file twice. Rename one."));
                }
                if name.is_empty() {
                    return Check::Name("Give it a name.".to_owned());
                }
                name_problem(name).map_or(Check::Ready, Check::Name)
            })
            .collect()
    }

    /// Import: what it comes to in words for the servers ready to import, and with `build`, what Review sends.
    fn import_plan(
        &self,
        servers: &[ServerRow],
        checked: &[Check],
        build: bool,
    ) -> Option<ImportPlan> {
        let read = self.read.as_ref().ok()?;
        let targets = self.places.targets();
        let existing = Existing::new(&self.places, servers);
        let mut plan = ImportPlan::default();
        for (i, server) in read.servers.iter().enumerate() {
            if checked.get(i) != Some(&Check::Ready) {
                continue;
            }
            let name = &self.names[i];
            let there = existing.of(name);
            let free: Vec<Target> = targets
                .iter()
                .filter(|t| !there.iter().any(|(at, _)| at == *t))
                .cloned()
                .collect();
            if !free.is_empty() {
                plan.adds.push(name.clone());
                plan.writes = true;
            }
            if !free.is_empty() && build {
                plan.changes.push(Change::Add {
                    name: name.clone(),
                    json: server.json.clone(),
                    targets: free,
                    // Only a server that signs in with OAuth takes one; an empty field means none.
                    secret: (server.oauth && !self.secrets[i].is_empty())
                        .then(|| self.secrets[i].clone()),
                });
            }
            if there.is_empty() {
                continue;
            }
            let places: Vec<Target> = there.iter().map(|(at, _)| at.clone()).collect();
            if self.replace[i] {
                plan.replaces.push((name.clone(), places));
                plan.writes = true;
                if build {
                    plan.changes
                        .extend(there.into_iter().map(|(_, id)| Change::Replace {
                            id,
                            json: server.json.clone(),
                        }));
                }
            } else {
                plan.keeps.push(name.clone());
            }
        }
        plan.writes.then_some(plan)
    }

    /// Import: for each server, where one of its name is already, and a name that is free; `None` when none is.
    fn conflicts(&self, servers: &[ServerRow]) -> Vec<Option<Conflict>> {
        let taken = Taken::new(&self.places, servers.iter());
        (0..self.names.len())
            .map(|i| {
                taken.of(&self.names[i]).map(|place| Conflict {
                    place,
                    free: self.free_name(i, &taken),
                })
            })
            .collect()
    }

    /// Import: a name for server `i` that nothing in the ticked places, and no other server in the file, has.
    fn free_name(&self, i: usize, taken: &Taken) -> String {
        let base = &self.names[i];
        (2..)
            .map(|n| format!("{base}_{n}"))
            .find(|name| {
                taken.of(name).is_none()
                    && !self.names.iter().any(|other| other == name)
                    && name_problem(name).is_none()
            })
            .expect("some number is free")
    }

    /// Import: the file, and each server read from it, with what to do about it.
    fn import_ui(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        checked: &[Check],
        conflicts: &[Option<Conflict>],
    ) -> bool {
        let mut choose_again = false;
        ui.horizontal(|ui| {
            if let Some(file) = &self.file {
                ui.add(egui::Label::new(RichText::new(format!("From {file}")).strong()).truncate());
            }
            // Without a dialog, asking again only says so again.
            if !self.no_dialog {
                let label = if self.file.is_some() {
                    "Choose another file…"
                } else {
                    "Choose a file…"
                };
                choose_again = ui.button(label).clicked();
            }
        });
        if let Some(problem) = &self.file_problem {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING_CIRCLE).color(pal.danger));
                ui.add(egui::Label::new(RichText::new(problem).color(pal.danger)).wrap());
            });
        }
        if self.no_dialog || self.typing {
            let said = if self.no_dialog {
                "No file dialog is available here. Type the path of the file to import."
            } else {
                "Type the path of the file to import."
            };
            ui.add(egui::Label::new(RichText::new(said).color(pal.text_muted)).wrap());
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.path)
                        .id(egui::Id::new("import_path"))
                        .hint_text("/path/to/mcp-servers.json")
                        .desired_width(480.0),
                );
                if std::mem::take(&mut self.focus) {
                    field.request_focus();
                }
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Read").clicked() || entered) && !self.path.trim().is_empty() {
                    self.path_wanted = true;
                }
            });
        }
        ui.add_space(6.0);
        let read = match &self.read {
            Err(why) if !why.is_empty() => {
                ui.add(egui::Label::new(RichText::new(why).color(pal.danger)).wrap());
                return choose_again;
            }
            Err(_) => return choose_again,
            Ok(read) => read,
        };
        let usable: Vec<usize> = (0..read.servers.len())
            .filter(|&i| read.servers[i].problem.is_none())
            .collect();
        if usable.len() > 1 {
            let ticked = usable.iter().filter(|&&i| self.include[i]).count();
            let mut all = ticked == usable.len();
            let label = format!("All {}", usable.len());
            let detail = if all {
                String::new()
            } else {
                format!("{ticked} ticked")
            };
            if place_mark(ui, &mut all, &label, &detail, None, Mark::Tick, true).changed() {
                for &i in &usable {
                    self.include[i] = all;
                }
            }
            ui.add_space(4.0);
        }
        let conflicting: Vec<usize> = (0..read.servers.len())
            .filter(|&i| checked[i] == Check::Ready && conflicts[i].is_some())
            .collect();
        if conflicting.len() > 1 {
            // The question on a line of its own, so the two answers stay together on the next.
            ui.label(
                RichText::new(format!(
                    "{} already exist. For all of them:",
                    conflicting.len()
                ))
                .color(pal.text_muted),
            );
            ui.horizontal_wrapped(|ui| {
                let all_replaced = conflicting.iter().all(|&i| self.replace[i]);
                let none_replaced = conflicting.iter().all(|&i| !self.replace[i]);
                // Neither is chosen while the servers differ.
                if ui.radio(none_replaced, "Keep the ones there").clicked() {
                    conflicting.iter().for_each(|&i| self.replace[i] = false);
                }
                if ui.radio(all_replaced, "Replace them").clicked() {
                    conflicting.iter().for_each(|&i| self.replace[i] = true);
                }
            });
            ui.add_space(4.0);
        }
        let mut rename = None;
        for (i, server) in read.servers.iter().enumerate() {
            ui.horizontal(|ui| {
                if server.problem.is_some() {
                    ui.label(RichText::new(&server.name).strikethrough());
                } else {
                    let label = format!("Import {}", self.names[i]);
                    tick_box(ui, &mut self.include[i], &label);
                    ui.add_enabled(
                        self.include[i],
                        egui::TextEdit::singleline(&mut self.names[i])
                            .hint_text("name")
                            .desired_width(180.0),
                    );
                }
            });
            // What it runs or connects to, whole: this is what is about to be added.
            ui.add(egui::Label::new(RichText::new(&server.summary).color(pal.text_muted)).wrap());
            let shown = server.problem.is_some() || self.include[i];
            match &checked[i] {
                Check::Skipped(why) | Check::Name(why) if !why.is_empty() => {
                    ui.add(egui::Label::new(RichText::new(why).small().color(pal.danger)).wrap());
                }
                _ => {}
            }
            if !shown {
                ui.add_space(4.0);
                continue;
            }
            if checked[i] == Check::Ready
                && let Some(conflict) = &conflicts[i]
            {
                let name = &self.names[i];
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{name} already exists {}.", conflict.place))
                            .color(pal.warn),
                    )
                    .wrap(),
                );
                let free = conflict.free.clone();
                // One choice of two, as radio buttons; importing it under another name instead is not a third
                // choice of the same kind (it renames the server, and the question goes), so it is a link apart.
                ui.horizontal_wrapped(|ui| {
                    if ui.radio(!self.replace[i], "Keep the one there").clicked() {
                        self.replace[i] = false;
                    }
                    if ui.radio(self.replace[i], "Replace it").clicked() {
                        self.replace[i] = true;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Or").color(pal.text_muted));
                    if ui.link(format!("import it as {free}")).clicked() {
                        rename = Some((i, free));
                    }
                });
            }
            if !server.needs.is_empty() {
                let names: Vec<&str> = server.needs.iter().map(String::as_str).collect();
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Set {} where Claude Code runs: it fills {} in when it starts the server.",
                            list_in_words(&names, "variable"),
                            if names.len() == 1 { "it" } else { "them" }
                        ))
                        .small()
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
            }
            for hint in &server.hints {
                ui.add(egui::Label::new(RichText::new(hint).small().color(pal.warn)).wrap());
            }
            if server.oauth || server.asks_secret {
                let label = format!("Client secret of {}", self.names[i]);
                let show = self.secret_shown == Some(i);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Client secret").color(pal.text_muted));
                    let field = ui.add(
                        egui::TextEdit::singleline(self.secrets[i].edit())
                            .id(import_secret_id(i))
                            .password(!show)
                            .hint_text(if server.asks_secret {
                                "it asks for one"
                            } else {
                                "optional"
                            })
                            .desired_width(260.0),
                    );
                    field.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, &label)
                    });
                    let (glyph, word) = if show {
                        (icon::EYE_SLASH, "Hide")
                    } else {
                        (icon::EYE, "Show")
                    };
                    let tip = format!("{word} the client secret of {}", self.names[i]);
                    let button = ui
                        .small_button(format!("{glyph} {word}"))
                        .on_hover_text(&tip);
                    button.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tip)
                    });
                    if button.clicked() {
                        self.secret_shown = (!show).then_some(i);
                    }
                });
                if self.replace[i] && conflicts[i].is_some() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Where it replaces one, it keeps the client secret Klefki has for that one.",
                            )
                            .small()
                            .color(pal.text_muted),
                        )
                        .wrap(),
                    );
                }
            }
            ui.add_space(6.0);
        }
        for note in &read.notes {
            ui.add(egui::Label::new(RichText::new(note).small().color(pal.text_muted)).wrap());
        }
        if let Some((i, name)) = rename {
            self.names[i] = name;
        }
        choose_again
    }

    /// The text field and what was read from it.
    fn paste_ui(&mut self, ui: &mut Ui, pal: &Palette, checked: &[Check]) {
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Paste a claude mcp add command, or JSON such as {\"mcpServers\": {…}}. Klefki only reads it; nothing is run until you review it.",
                )
                .small()
                .color(pal.text_muted),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        let field = ui.add(
            egui::TextEdit::multiline(&mut self.text)
                .id(egui::Id::new("add_paste"))
                .code_editor()
                .desired_rows(5)
                .desired_width(f32::INFINITY)
                .hint_text("claude mcp add --transport http notion https://mcp.notion.com/mcp"),
        );
        if std::mem::take(&mut self.focus) {
            field.request_focus();
        }
        ui.add_space(8.0);
        let mut fill_in = None;
        match &self.read {
            Err(why) if !why.is_empty() => {
                ui.add(egui::Label::new(RichText::new(why).color(pal.danger)).wrap());
            }
            Err(_) => {}
            Ok(read) => {
                for (i, server) in read.servers.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if server.problem.is_none() && !server.asks_secret {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.names[i])
                                    .hint_text("name")
                                    .desired_width(180.0),
                            );
                        } else if server.problem.is_none() {
                            ui.label(&server.name);
                        } else {
                            ui.label(RichText::new(&server.name).strikethrough());
                        }
                        ui.add(
                            egui::Label::new(RichText::new(&server.summary).color(pal.text_muted))
                                .truncate(),
                        );
                    });
                    match &checked[i] {
                        Check::Skipped(why) | Check::Name(why) => {
                            ui.add(
                                egui::Label::new(RichText::new(why).small().color(pal.danger))
                                    .wrap(),
                            );
                        }
                        // The button under the text, so a long line wraps rather than widening the dialog.
                        Check::FillIn(why) => {
                            ui.add(
                                egui::Label::new(RichText::new(why).small().color(pal.text_muted))
                                    .wrap(),
                            );
                            if ui
                                .button(format!("{} Fill in", icon::PENCIL_SIMPLE))
                                .clicked()
                            {
                                fill_in = Some(i);
                            }
                        }
                        Check::Ready => {}
                    }
                    for hint in &server.hints {
                        ui.add(
                            egui::Label::new(RichText::new(hint).small().color(pal.warn)).wrap(),
                        );
                    }
                    ui.add_space(4.0);
                }
                for note in &read.notes {
                    ui.add(
                        egui::Label::new(RichText::new(note).small().color(pal.text_muted)).wrap(),
                    );
                }
            }
        }
        if let Some(i) = fill_in {
            self.fill_in_from(i);
        }
    }

    /// What is wrong with `field`, once it has been left or Review was pressed; never while it is first typed in.
    fn message<'a>(&'a self, field: Field, name_check: Option<&'a str>) -> Option<&'a str> {
        if !self.tried && !self.left.contains(&field) {
            return None;
        }
        match field {
            Field::Name => name_check,
            Field::Part(part) => self
                .filled
                .as_ref()?
                .problems
                .iter()
                .find(|(p, _)| *p == part)
                .map(|(_, why)| why.as_str()),
            // Optional, and anything typed is kept as it is.
            Field::Secret => None,
        }
    }

    /// The callback port as typed, when it is one.
    fn typed_port(&self) -> Option<u16> {
        self.fields
            .callback_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
    }

    /// Asks whether a sign-in could come back to the callback port as typed now. The engine answers, off the UI
    /// thread.
    fn check_port(&mut self) {
        if self.fields.oauth && self.typed_port() != self.port_pending {
            self.port_wanted = self.typed_port();
        }
    }

    /// Why a sign-in could not come back to the callback port, as last checked: only while the port is still the
    /// one checked, and nothing is wrong with it.
    fn port_warning(&self) -> Option<&str> {
        let (port, warning) = self.port_checked.as_ref()?;
        let wrong = self
            .filled
            .as_ref()
            .is_some_and(|f| f.problems.iter().any(|(p, _)| *p == Part::CallbackPort));
        (self.fields.oauth && self.typed_port() == Some(*port) && !wrong)
            .then_some(warning.as_deref())
            .flatten()
    }

    /// Draws a text field for `field`: gives it the keyboard when asked, and notes when the keyboard leaves it.
    fn track(&mut self, field: Field, response: &egui::Response) {
        if self.focus_on == Some(field) {
            self.focus_on = None;
            response.request_focus();
            response.scroll_to_me(Some(Align::Center));
        }
        if response.lost_focus() {
            self.left.insert(field);
        }
    }

    /// The form: what the server runs or connects to, its name, its environment or headers, and the OAuth client
    /// settings the provider gave, if any. Each problem is said under its field.
    fn fill_ui(&mut self, ui: &mut Ui, pal: &Palette, checked: &[Check]) {
        ui.add_space(8.0);
        let label_width = 128.0;
        let field_width = (ui.available_width() - label_width - 16.0).max(120.0);
        let name_check = match checked.first() {
            Some(Check::Name(why)) => Some(why.clone()),
            _ => None,
        };
        let first = std::mem::take(&mut self.focus);
        let was = self.fields.transport;
        field_row(ui, pal, "Type", label_width, |ui| {
            for kind in [
                TransportKind::Stdio,
                TransportKind::Http,
                TransportKind::Sse,
            ] {
                ui.radio_value(&mut self.fields.transport, kind, transport_name(kind));
            }
        });
        if self.fields.transport != was {
            // Another list of rows: what was shown or replaced was in the other one.
            self.rows = RowsState::default();
        }
        if self.fields.transport == TransportKind::Stdio {
            let field = Field::Part(Part::Command);
            let response = field_row(ui, pal, "Command", label_width, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.fields.command)
                        .id(field_id(field))
                        .code_editor()
                        .hint_text("e.g. npx -y @modelcontextprotocol/server-filesystem /path")
                        .desired_width(field_width),
                )
            });
            if first {
                response.request_focus();
            }
            self.track(field, &response);
            field_note(
                ui,
                pal,
                label_width,
                self.message(field, None),
                "As you would type it in a terminal; put quotes around a path with spaces.",
            );
        } else {
            let field = Field::Part(Part::Address);
            let response = field_row(ui, pal, "Address", label_width, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.fields.url)
                        .id(field_id(field))
                        .hint_text("e.g. https://mcp.example.com/mcp")
                        .desired_width(field_width),
                )
            });
            if first {
                response.request_focus();
            }
            if response.lost_focus() {
                self.fields.complete_url();
            }
            self.track(field, &response);
            field_note(ui, pal, label_width, self.message(field, None), "");
        }
        let response = field_row(ui, pal, "Name", label_width, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .id(field_id(Field::Name))
                    .hint_text("e.g. github")
                    .desired_width(field_width),
            )
        });
        if response.changed() {
            // Cleared, it follows the parts again.
            self.name_edited = !self.name.is_empty();
        }
        self.track(Field::Name, &response);
        field_note(
            ui,
            pal,
            label_width,
            self.message(Field::Name, name_check.as_deref()),
            if self.name_edited || self.name.is_empty() {
                ""
            } else {
                "Taken from the address or command; type another if you like."
            },
        );
        if self.fields.transport == TransportKind::Stdio {
            let field = Field::Part(Part::Variables);
            value_rows(
                ui,
                pal,
                "Environment",
                "variable",
                label_width,
                &mut self.fields.env,
                &mut self.rows,
            );
            field_note(ui, pal, label_width, self.message(field, None), "");
            return;
        }
        let field = Field::Part(Part::Headers);
        value_rows(
            ui,
            pal,
            "Headers",
            "header",
            label_width,
            &mut self.fields.headers,
            &mut self.rows,
        );
        field_note(ui, pal, label_width, self.message(field, None), "");
        ui.add_space(4.0);
        let toggled = field_row(ui, pal, "", label_width, |ui| {
            // Drawn like the places below: egui's own small box reads as a radio button here.
            place_row(
                ui,
                &mut self.fields.oauth,
                "Use OAuth client settings from the provider",
                "",
                None,
            )
        });
        if toggled.changed() && self.fields.oauth {
            self.reveal_oauth = true;
        }
        field_note(
            ui,
            pal,
            label_width,
            None,
            "Most servers sign you in on their own. Tick this only if the provider gave you a client ID or secret.",
        );
        if !self.fields.oauth {
            return;
        }
        let block = ui.scope(|ui| {
            let half = field_width.min(300.0);
            for (part, label, hint, width) in [
                (Part::ClientId, "Client ID", "optional", half),
                (Part::CallbackPort, "Callback port", "optional", 96.0),
                (Part::Scopes, "Scopes", "optional, separated by spaces", half),
                (Part::MetadataUrl, "Metadata URL", "optional, rarely needed: https://…", field_width),
            ] {
                if part == Part::CallbackPort {
                    self.secret_row(ui, pal, label_width, half);
                }
                let field = Field::Part(part);
                let text = match part {
                    Part::ClientId => &mut self.fields.client_id,
                    Part::CallbackPort => &mut self.fields.callback_port,
                    Part::Scopes => &mut self.fields.scopes,
                    _ => &mut self.fields.metadata_url,
                };
                let response = field_row(ui, pal, label, label_width, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .id(field_id(field))
                            .hint_text(hint)
                            .desired_width(width),
                    )
                });
                self.track(field, &response);
                if part == Part::CallbackPort && response.lost_focus() {
                    self.check_port();
                }
                field_note(ui, pal, label_width, self.message(field, None), "");
                if part == Part::CallbackPort
                    && let Some(warning) = self.port_warning()
                {
                    field_warning(ui, pal, label_width, warning);
                }
            }
            field_note(
                ui,
                pal,
                label_width,
                None,
                "Claude Code keeps the client secret in its own secure storage, and Klefki keeps a copy in your system's. Leave it empty if the provider gave you none.",
            );
        });
        if std::mem::take(&mut self.reveal_oauth) {
            ui.scroll_to_rect(block.response.rect, None);
        }
    }

    /// The client secret: typed hidden, with a button to show it while checking a paste.
    fn secret_row(&mut self, ui: &mut Ui, pal: &Palette, label_width: f32, width: f32) {
        let focus = self.focus_on.take_if(|f| *f == Field::Secret).is_some();
        let show = &mut self.show_secret;
        let secret = &mut self.secret;
        field_row(ui, pal, "Client secret", label_width, |ui| {
            let field = ui.add(
                egui::TextEdit::singleline(secret.edit())
                    .id(secret_id())
                    .password(!*show)
                    .hint_text("optional")
                    .desired_width(width),
            );
            if focus {
                field.request_focus();
                field.scroll_to_me(Some(Align::Center));
            }
            // Named in words too: an eye alone is read many ways, and the name read out starts with what is shown.
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
        field_note(ui, pal, label_width, None, "");
    }
}

/// A row of the form: its label, left-aligned in a column, then what `add` draws.
/// Has egui forget the text fields of the closed Add dialog that may have held a secret: their undo steps keep
/// copies of what was typed.
fn forget_typed(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.remove::<egui::text_edit::TextEditState>(secret_id());
        d.remove::<egui::text_edit::TextEditState>(egui::Id::new("add_paste"));
        d.remove::<egui::text_edit::TextEditState>(egui::Id::new("import_path"));
        for row in 0..IMPORT_ROWS {
            d.remove::<egui::text_edit::TextEditState>(import_secret_id(row));
        }
        for row in 0..FORGET_ROWS {
            for noun in ["variable", "header"] {
                d.remove::<egui::text_edit::TextEditState>(value_id(noun, row));
            }
        }
    });
}

fn field_row<R>(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    label_width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(label_width, 30.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_size(egui::vec2(label_width, 30.0));
                ui.label(RichText::new(label).color(pal.text_muted));
            },
        );
        add(ui)
    })
    .inner
}

/// Under a field, in its column: what is wrong with it, in red, or else `help`, quietly. Nothing when both are
/// empty.
fn field_note(ui: &mut Ui, pal: &Palette, label_width: f32, problem: Option<&str>, help: &str) {
    let (text, color) = match problem {
        Some(problem) => (problem, pal.danger),
        None if !help.is_empty() => (help, pal.text_muted),
        None => return,
    };
    ui.horizontal(|ui| {
        ui.add_space(label_width + 8.0);
        ui.add(egui::Label::new(RichText::new(text).small().color(color)).wrap());
    });
}

/// Under a field, in its column: what may go wrong later with what it holds, though it can be added as it is.
fn field_warning(ui: &mut Ui, pal: &Palette, label_width: f32, warning: &str) {
    ui.horizontal(|ui| {
        ui.add_space(label_width + 8.0);
        ui.add(
            egui::Label::new(
                RichText::new(format!("{} {warning}", icon::WARNING))
                    .small()
                    .color(pal.warn),
            )
            .wrap(),
        );
    });
}

/// The rows of a new server's environment or headers, as on the Env tab: a name and a value each (typed hidden,
/// with a button to show it), a button to take a row out, one to add a row, and, under a value that looks like a
/// secret, one to write a reference to it instead, which says what to do next and can be undone.
fn value_rows(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    noun: &str,
    label_width: f32,
    rows: &mut Vec<(String, String)>,
    state: &mut RowsState,
) {
    let RowsState { shown, replaced } = state;
    let mut remove = None;
    for (i, (name, value)) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(label_width, 30.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_size(egui::vec2(label_width, 30.0));
                    ui.label(RichText::new(if i == 0 { title } else { "" }).color(pal.text_muted));
                },
            );
            ui.add(
                egui::TextEdit::singleline(&mut *name)
                    .hint_text("Name")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(170.0),
            );
            let visible = shown.contains(&i);
            ui.add(
                egui::TextEdit::singleline(&mut *value)
                    .id(value_id(noun, i))
                    .hint_text("Value")
                    .password(!visible)
                    .desired_width(ui.available_width() - 72.0),
            );
            let what = if name.is_empty() { noun } else { name.as_str() };
            let (glyph, verb) = if visible {
                (icon::EYE_SLASH, "Hide")
            } else {
                (icon::EYE, "Show")
            };
            if icon_button(ui, glyph, &format!("{verb} {what}")) && !shown.remove(&i) {
                shown.insert(i);
            }
            if icon_button(ui, icon::X, &format!("Remove {what}")) {
                remove = Some(i);
            }
        });
        let note = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            ui.horizontal(|ui| {
                ui.add_space(label_width + 8.0);
                ui.vertical(|ui| add(ui));
            });
        };
        if let Some(reference) = secret_reference(name, value) {
            note(ui, &mut |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} looks like a secret.", name.trim()))
                            .small()
                            .color(pal.warn),
                    )
                    .wrap(),
                );
                if ui
                    .small_button(format!("Use {reference} instead"))
                    .on_hover_text(
                        "Claude Code fills the variable in from its own environment, so the secret stays out of ~/.claude.json.",
                    )
                    .clicked()
                {
                    let variable = reference
                        .split_once("${")
                        .and_then(|(_, rest)| rest.split_once('}'))
                        .map_or_else(String::new, |(v, _)| v.to_owned());
                    let before = Secret::new(std::mem::replace(value, reference.clone()));
                    *replaced = Some(Replaced {
                        row: i,
                        variable,
                        before,
                    });
                }
            });
        } else if let Some(r) = replaced.as_mut().filter(|r| r.row == i) {
            let mut undo = false;
            note(ui, &mut |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Set {} in the environment Claude Code starts from, or the server will not get it.",
                            r.variable
                        ))
                        .small()
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
                undo = ui.small_button("Undo").clicked();
            });
            if undo {
                *value = std::mem::take(r.before.edit());
                *replaced = None;
            }
        }
    }
    if let Some(i) = remove {
        rows.remove(i);
        // The rows after it moved up: what was shown or replaced is no longer where it was.
        shown.clear();
        *replaced = None;
    }
    ui.horizontal(|ui| {
        ui.add_space(label_width + 8.0);
        if rows.is_empty() {
            ui.label(RichText::new(format!("{title}: none")).color(pal.text_muted));
        }
        if ui.small_button(format!("Add {noun}")).clicked() {
            rows.push((String::new(), String::new()));
        }
    });
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
        .map(|e| sentence(&e.to_string()))
}

/// `text` with its first letter made upper case, as a sentence starts.
fn sentence(text: &str) -> String {
    let mut c = text.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `a`, `a and b`, `a, b and c`; more than three are counted: `5 projects`.
fn list_in_words(items: &[&str], noun: &str) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] if items.len() <= 3 => format!("{} and {last}", rest.join(", ")),
        _ => format!("{} {noun}s", items.len()),
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

/// What the Export dialog holds: what is exported, and the block once the engine sent it.
struct ExportView {
    /// For the title: the one server's name, or `3 servers`.
    what: String,
    /// The servers' names in one line, when there is more than one: what is exported, without recalling the list.
    names: Option<String>,
    /// How the search decided which servers these are, when it did.
    scope: Option<String>,
    result: Option<Exported>,
    /// What was last copied, and when: that button says "Copied" for a few seconds.
    copied: Option<(Copied, f64)>,
    /// The file name the save dialog starts with.
    file_name: String,
    save: Saving,
    /// What the last save did, said as a sentence: `Ok` when it saved.
    status: Option<Result<String, String>>,
    /// The dialog's size in the last frame.
    size: egui::Vec2,
    /// How tall the save status was in the last frame (it wraps a long path).
    status_height: f32,
    /// The variables list was folded to make room for saving on a short window (once: it can be opened again).
    folded: bool,
}

/// Where saving the export to a file is.
#[derive(Clone, PartialEq)]
enum Saving {
    Idle,
    /// The system's save dialog is open.
    Choosing,
    /// The save dialog closed without a file; typing a path is offered, starting from `path`.
    Offered {
        path: String,
    },
    /// The path is typed in: no save dialog could be shown (`no_dialog`), or the user asked to type it.
    Typing {
        path: String,
        focus: bool,
        no_dialog: bool,
    },
    /// Written by the engine; `typed` is the text of a typed path, to come back to if the save fails.
    Writing {
        path: std::path::PathBuf,
        typed: Option<String>,
    },
    /// A typed path names a file that is there already: replace it?
    Exists {
        path: std::path::PathBuf,
        typed: String,
    },
}

/// Which of the Export dialog's buttons copied.
#[derive(Clone, Copy, PartialEq)]
enum Copied {
    Block,
    Names,
}

/// Selects the file's name without its extension in the text field `id` holding `path`, as a system save dialog does,
/// so the part people change is in view and typed over.
fn select_file_stem(ctx: &egui::Context, id: egui::Id, path: &str) {
    use egui::text::{CCursor, CCursorRange};
    let name_at = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let stem_end = path[name_at..]
        .rfind('.')
        .filter(|&i| i > 0)
        .map_or(path.len(), |i| name_at + i);
    let chars = |byte: usize| path[..byte].chars().count();
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(chars(name_at)),
        CCursor::new(chars(stem_end)),
    )));
    state.store(ctx, id);
}

/// Makes the widget `id` a polite live region: a screen reader reads out a change to it ("Copied", "Saved to …")
/// when it is next idle, without moving the keyboard focus (WCAG 2.2, 4.1.3 Status Messages).
fn announce(ctx: &egui::Context, id: egui::Id) {
    ctx.accesskit_node_builder(id, |node| node.set_live(egui::accesskit::Live::Polite));
}

/// How long the Copy button says "Copied".
const COPIED_SECS: f64 = 4.0;

/// `a, b and c`; past eight names, `a, b, … and 12 more`.
fn names_in_a_line(names: &[&str]) -> String {
    const SHOWN: usize = 8;
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        _ if names.len() <= SHOWN => {
            let (last, rest) = names.split_last().expect("not empty");
            format!("{} and {last}", rest.join(", "))
        }
        _ => format!(
            "{} and {} more",
            names[..SHOWN].join(", "),
            names.len() - SHOWN
        ),
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

/// What the Duplicate dialog holds: the copy's name, where it goes, and what it changes.
struct DuplicateForm {
    source: ServerId,
    /// What is being copied, under the title: `http · https://…`.
    about: String,
    name: String,
    places: Places,
    /// Give the name field the keyboard on the first frame.
    focus: bool,
    /// The copy's address and sign-in settings.
    copy: CopyRead,
    /// The new client's secret is shown while it is checked.
    show_secret: bool,
    /// The fields the keyboard has left: what is wrong with them is said from then on.
    left: BTreeSet<Part>,
    /// The callback port to check, sent to the engine after this frame: when the field is left.
    port_wanted: Option<u16>,
    /// The callback port sent to be checked, until its answer comes: it is asked once.
    port_pending: Option<u16>,
    /// The callback port last checked, and why a sign-in could not come back to it ([`Event::CallbackPort`]).
    port_checked: Option<(u16, Option<String>)>,
}

/// The Duplicate dialog's address and sign-in fields, from the original's definition.
#[derive(Debug, PartialEq)]
enum CopyRead {
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

/// The toolbar's buttons: icon and hover text. Only what works today: a button that can never be pressed is noise.
const ACTIONS: [(&str, &str); 10] = [
    (icon::PLUS, "Add (Ctrl+N)"),
    (icon::TRAY_ARROW_DOWN, "Import from a file (Ctrl+O)"),
    (icon::PENCIL_SIMPLE, "Edit (Ctrl+E)"),
    (icon::COPY, "Duplicate (Ctrl+D)"),
    (icon::ARROW_FAT_LINES_RIGHT, "Move (Ctrl+Shift+M)"),
    (icon::SIGN_IN, "Sign in"),
    (icon::TOGGLE_RIGHT, "Turn on"),
    (icon::TOGGLE_LEFT, "Turn off"),
    (icon::TRASH, "Delete (Del)"),
    // Last: it changes nothing. Its hover text says what it exports: the selection, or the whole list.
    (icon::EXPORT, ""),
];

/// What the Move dialog holds: the one place the server goes.
struct MoveForm {
    source: ServerId,
    /// What is being moved, under the title: `http · https://…`.
    about: String,
    places: Places,
    /// It signs in with OAuth, so it is signed out by the move.
    signs_in: bool,
}

/// Where a new server goes, as ticked in a dialog: for the user, and in any of the projects Claude Code knows.
struct Places {
    user: bool,
    /// Every project Claude Code knows, in the snapshot's order, and whether the server goes there.
    projects: Vec<(String, bool)>,
    /// The order the projects are listed in: `here` first, then by folder name.
    order: Vec<usize>,
    /// The project the dialog starts from (the selected server's), if any.
    here: Option<String>,
    /// Folder names (lower case) that more than one project has: only those rows also say where they live. The rest
    /// would all repeat the same few parent folders.
    shared_folders: BTreeSet<String>,
    /// Narrows the list of projects: part of a path, any case.
    filter: String,
    /// Choose one place, like radio buttons, rather than tick any number: the Move dialog.
    one: bool,
    /// Where the server is now, which cannot be chosen: the Move dialog.
    now: Option<Target>,
}

impl Places {
    /// Nothing ticked yet.
    fn new(projects: &[String], here: Option<&str>) -> Self {
        let projects: Vec<(String, bool)> = projects.iter().map(|p| (p.clone(), false)).collect();
        let mut order: Vec<usize> = (0..projects.len()).collect();
        order.sort_by_cached_key(|&i| {
            let path = &projects[i].0;
            (
                Some(path.as_str()) != here,
                folder_name(path).to_lowercase(),
            )
        });
        let mut seen = BTreeSet::new();
        let shared_folders = projects
            .iter()
            .map(|(p, _)| folder_name(p).to_lowercase())
            .filter(|f| !seen.insert(f.clone()))
            .collect();
        Self {
            user: false,
            projects,
            order,
            here: here.map(str::to_owned),
            shared_folders,
            filter: String::new(),
            one: false,
            now: None,
        }
    }

    /// Ticks the project the dialog starts from. False when there is none.
    fn tick_here(&mut self) -> bool {
        let mut ticked = false;
        for (path, on) in &mut self.projects {
            if Some(path.as_str()) == self.here.as_deref() {
                *on = true;
                ticked = true;
            }
        }
        ticked
    }

    /// The projects ticked, in the snapshot's order.
    fn ticked(&self) -> impl Iterator<Item = &str> {
        self.projects
            .iter()
            .filter(|(_, on)| *on)
            .map(|(p, _)| p.as_str())
    }

    fn targets(&self) -> Vec<Target> {
        self.user
            .then_some(Target::User)
            .into_iter()
            .chain(self.ticked().map(|p| Target::Local(p.to_owned())))
            .collect()
    }

    /// Where it goes, in words: `for the user and in a and b`. None when nothing is ticked.
    fn in_words(&self) -> Option<String> {
        let folders: Vec<&str> = self.ticked().map(folder_name).collect();
        places_in_words(self.user, &folders)
    }

    /// The one place chosen, as a move says it: `to the user`, `to app`. None when nothing is chosen.
    fn one_in_words(&self) -> Option<String> {
        if self.user {
            return Some("to the user".to_owned());
        }
        self.ticked()
            .next()
            .map(|p| format!("to {}", folder_name(p)))
    }

    /// The ticked places that already have a server called `name`, in words. Only the lists a new server is written
    /// to count: the engine refuses those, while a name in a project's `.mcp.json` is only hidden by it, which the
    /// review warns about.
    fn taken(&self, name: &str, servers: &[ServerRow]) -> Option<String> {
        Taken::new(self, servers.iter().filter(|s| s.name == name)).of(name)
    }

    /// The tick boxes: the user, then the projects in a list of at most `list_height`, searchable when long. `tag`
    /// marks the project the dialog starts from.
    /// Says whether a tick changed.
    fn ui(&mut self, ui: &mut Ui, list_height: f32, tag: Option<&str>) -> bool {
        let pal = theme::palette(ui.ctx());
        const NOW: &str = "where it is now";
        let mark = if self.one { Mark::Radio } else { Mark::Tick };
        let user_now = self.now == Some(Target::User);
        let mut changed = place_mark(
            ui,
            &mut self.user,
            "For the user",
            "in every project",
            user_now.then_some(NOW),
            mark,
            !user_now,
        )
        .changed();
        if changed && self.one && self.user {
            for (_, on) in &mut self.projects {
                *on = false;
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let ticked = self.projects.iter().filter(|(_, on)| *on).count();
            // Local scope, as the list's groups say it; not the shared .mcp.json, which Klefki never writes.
            let mut heading = format!("In a project, just for you ({})", self.projects.len());
            if ticked > 0 && !self.one {
                heading.push_str(&format!(", {ticked} ticked"));
            }
            ui.label(RichText::new(heading).color(pal.text_muted));
            // A short list is read at a glance; a long one is searched.
            if self.projects.len() > 8 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text(format!("{} Filter projects", icon::MAGNIFYING_GLASS))
                            .desired_width(220.0),
                    );
                });
            }
        });
        // As tall as the whole list would be, up to that room, and no shorter while filtering: the dialog keeps
        // its size and its buttons stay where they are as you type.
        let rows = self.projects.len().max(1) as f32;
        let height = (rows * (PLACE_ROW + PLACE_GAP)).min(list_height);
        egui::ScrollArea::vertical()
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = PLACE_GAP;
                let needle = self.filter.trim().to_lowercase();
                let mut shown = 0;
                let mut picked = None;
                for &i in &self.order {
                    let (path, on) = &mut self.projects[i];
                    if !path.to_lowercase().contains(&needle) {
                        continue;
                    }
                    shown += 1;
                    let here = self.here.as_deref() == Some(path.as_str());
                    let folder = folder_name(path);
                    let parent = if self.shared_folders.contains(&folder.to_lowercase()) {
                        parent_folder(path)
                    } else {
                        ""
                    };
                    let now = matches!(&self.now, Some(Target::Local(p)) if same_project(p, path));
                    let tag = if now { Some(NOW) } else { tag.filter(|_| here) };
                    let row = place_mark(ui, on, folder, parent, tag, mark, !now)
                        .on_hover_text(path.as_str());
                    if row.changed() {
                        changed = true;
                        picked = Some(i);
                    }
                }
                // One place at a time: choosing a project un-chooses the rest.
                if let Some(picked) = picked.filter(|_| self.one) {
                    self.user = false;
                    for (i, (_, on)) in self.projects.iter_mut().enumerate() {
                        *on = i == picked;
                    }
                }
                if shown == 0 {
                    ui.label(RichText::new("No project matches.").color(pal.text_muted));
                }
            });
        changed
    }
}

/// `for the user`, `in a and b`, `for the user and in 5 projects`; None for nowhere.
fn places_in_words(user: bool, folders: &[&str]) -> Option<String> {
    let projects =
        (!folders.is_empty()).then(|| format!("in {}", list_in_words(folders, "project")));
    match (user, projects) {
        (false, None) => None,
        (true, None) => Some("for the user".to_owned()),
        (true, Some(p)) => Some(format!("for the user and {p}")),
        (false, Some(p)) => Some(p),
    }
}

/// Height of a place in the Duplicate dialog, and the space between two.
const PLACE_ROW: f32 = 32.0;
const PLACE_GAP: f32 = 2.0;

/// One place in the Duplicate dialog: a tick box, a name and where it is, as wide as the list so the whole row can be
/// clicked. Drawn rather than laid out, so a long path is cut to fit instead of widening the dialog.
fn place_row(
    ui: &mut Ui,
    on: &mut bool,
    title: &str,
    detail: &str,
    tag: Option<&str>,
) -> egui::Response {
    place_mark(ui, on, title, detail, tag, Mark::Tick, true)
}

/// How a place is chosen: a tick box among many, or a radio button for one.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Tick,
    Radio,
}

/// A tick box, as the places a dialog lists have: filled when ticked, an outline when not.
fn paint_tick(painter: &egui::Painter, tick: egui::Rect, ticked: bool, pal: &Palette) {
    if ticked {
        painter.rect_filled(tick, 4.0, pal.primary);
        painter.text(
            tick.center(),
            egui::Align2::CENTER_CENTER,
            icon::CHECK,
            egui::FontId::proportional(13.0),
            pal.on_bright,
        );
    } else {
        painter.rect_stroke(
            tick,
            4.0,
            egui::Stroke::new(1.5, pal.text_muted),
            egui::StrokeKind::Inside,
        );
    }
}

/// A tick box on its own, beside a field: `label` is what it is read out as.
fn tick_box(ui: &mut Ui, on: &mut bool, label: &str) -> egui::Response {
    let pal = theme::palette(ui.ctx());
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let ticked = *on;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, ticked, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        if response.hovered() || response.has_focus() {
            painter.rect_filled(rect, 6.0, pal.hover);
        }
        let tick = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0));
        paint_tick(&painter, tick, ticked, pal);
    }
    response.on_hover_text(label)
}

/// [`place_row`] drawn as `mark`; a row that is not `enabled` cannot be chosen and is drawn faded.
fn place_mark(
    ui: &mut Ui,
    on: &mut bool,
    title: &str,
    detail: &str,
    tag: Option<&str>,
    mark: Mark,
    enabled: bool,
) -> egui::Response {
    let pal = theme::palette(ui.ctx());
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), PLACE_ROW), sense);
    if response.clicked() {
        // A radio button stays chosen when clicked again.
        let next = mark == Mark::Tick && *on;
        if *on != !next {
            *on = !next;
            response.mark_changed();
        }
    }
    let ticked = *on;
    response.widget_info(|| {
        let label = if detail.is_empty() {
            title.to_owned()
        } else {
            format!("{title}, {detail}")
        };
        let kind = match mark {
            Mark::Tick => egui::WidgetType::Checkbox,
            Mark::Radio => egui::WidgetType::RadioButton,
        };
        egui::WidgetInfo::selected(kind, enabled, ticked, label)
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    if enabled && (response.hovered() || response.has_focus()) {
        painter.rect_filled(rect, 6.0, pal.hover);
    }
    let tick = egui::Rect::from_center_size(
        rect.left_center() + egui::vec2(18.0, 0.0),
        egui::vec2(18.0, 18.0),
    );
    let text = if enabled { pal.text } else { pal.text_muted };
    if mark == Mark::Radio {
        let stroke = if ticked { pal.primary } else { pal.text_muted };
        // Where it is now cannot be chosen: faded, ring and all.
        let stroke = if enabled {
            stroke
        } else {
            stroke.gamma_multiply(0.4)
        };
        painter.circle_stroke(tick.center(), 8.25, egui::Stroke::new(1.5, stroke));
        if ticked {
            painter.circle_filled(tick.center(), 4.5, pal.primary);
        }
    } else {
        paint_tick(&painter, tick, ticked, pal);
    }
    let name = painter.text(
        egui::pos2(tick.right() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(15.0),
        text,
    );
    let small = egui::FontId::proportional(13.0);
    let mut right = rect.right() - 10.0;
    if let Some(tag) = tag {
        let t = painter.text(
            egui::pos2(right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            tag,
            small.clone(),
            pal.primary,
        );
        right = t.left() - 12.0;
    }
    let x = name.right() + 10.0;
    if !detail.is_empty() && right > x {
        let galley = ui.fonts_mut(|f| {
            let mut job =
                egui::text::LayoutJob::simple_singleline(detail.to_owned(), small, pal.text_muted);
            job.wrap = egui::text::TextWrapping::truncate_at_width(right - x);
            f.layout_job(job)
        });
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text_muted,
        );
    }
    response
}

/// Where a project folder lives: `C:/work/app` -> `C:/work`. Empty for a root.
fn parent_folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[..cut],
        _ => "",
    }
}

/// What the notice about old switch-off lists says, for one snapshot.
struct LegacyNotice {
    title: String,
    /// One line per project shown, with its full path for the hover.
    lines: Vec<(String, String)>,
    /// "… and N more" when there are more projects than lines.
    more: Option<String>,
    /// At least one list can be migrated (holds only names).
    migratable: bool,
}

impl LegacyNotice {
    fn new(lists: &[LegacyDisabled]) -> Self {
        const SHOWN: usize = 4;
        let title = if lists.len() == 1 {
            "Old switch-off list in 1 project".to_owned()
        } else {
            format!("Old switch-off lists in {} projects", lists.len())
        };
        let lines = lists
            .iter()
            .take(SHOWN)
            .map(|l| {
                let folder = folder_name(&l.shown);
                let line = if l.malformed {
                    format!(
                        "{folder}: holds something other than server names; edit it in .claude.json by hand"
                    )
                } else if l.names.is_empty() {
                    format!("{folder}: an empty list")
                } else {
                    let state = match l.turns_off.as_slice() {
                        [] => "already off".to_owned(),
                        [one] => format!("{one} is on"),
                        many => format!("{} are on", some_names(many)),
                    };
                    format!("{folder}: {} ({state})", some_names(&l.names))
                };
                (line, l.shown.clone())
            })
            .collect();
        Self {
            title: format!("{} {title}", icon::CLOCK_COUNTER_CLOCKWISE),
            lines,
            more: (lists.len() > SHOWN).then(|| format!("… and {} more", lists.len() - SHOWN)),
            migratable: lists.iter().any(|l| !l.malformed),
        }
    }
}

/// Names joined with commas; past the first few, only counted, so a long list stays readable. The app cannot use
/// core's `ops::some_of`, which cuts the review's summary the same way; keep the two alike.
fn some_names(names: &[String]) -> String {
    const NAMED: usize = 5;
    if names.len() <= NAMED + 1 {
        return names.join(", ");
    }
    format!(
        "{} and {} more",
        names[..NAMED].join(", "),
        names.len() - NAMED
    )
}

/// The last part of a project path: `C:/work/app` -> `app`.
fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(cut) if cut + 1 < trimmed.len() => &trimmed[cut + 1..],
        _ => trimmed,
    }
}

fn group_of(s: &ServerRow) -> Group {
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
            let trimmed = p.trim_end_matches(['/', '\\']);
            let (parent, folder) = match trimmed.rfind(['/', '\\']) {
                Some(cut) if cut + 1 < trimmed.len() => (&trimmed[..cut], &trimmed[cut + 1..]),
                _ => ("", trimmed),
            };
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

/// A badge beside a server's name: what it says, its colours, and its hover text if any.
struct Badge {
    text: String,
    fill: Color32,
    color: Color32,
    hover: Option<String>,
}

fn name_cell(ui: &mut Ui, s: &ServerRow, project_count: usize) {
    let pal = theme::palette(ui.ctx());
    // Like a user server off in every project: a plugin's server whose plugin is.
    let color = if s.enabled && !plugin_off_everywhere(s, project_count) {
        pal.text
    } else {
        pal.disabled
    };
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
fn keep_the_end(ui: &Ui, text: &str, width: f32) -> String {
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

const LAST_KNOWN: &str = "What Claude Code last saw, from its own cache, which can be old.";

fn status_cell(
    ui: &mut Ui,
    s: &ServerRow,
    status: Status,
    detail: Option<&str>,
    hint: Option<&Hint>,
    last_known: bool,
) {
    let pal = theme::palette(ui.ctx());
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
    if !s.enabled {
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
    let color = match status {
        Status::Connected => pal.success,
        Status::Degraded | Status::NeedsAuth | Status::PendingApproval => pal.warn,
        Status::Failed => pal.danger,
        Status::Checking => pal.primary,
        Status::NotConfigured | Status::Unknown => pal.text_muted,
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
        job.append(
            status.label(),
            0.0,
            egui::TextFormat::simple(font, pal.text_muted),
        );
        ui.label(job)
    } else {
        ui.label(RichText::new(format!("\u{25CF} {}", status.label())).color(color))
    };
    match status {
        Status::Failed | Status::NeedsAuth | Status::Degraded => {
            // Not egui's tooltip, which waits half a second for a perfectly still pointer over the words alone:
            // the whole cell answers, moving or not, after a moment that lets a sweep down the column pass by.
            if hovered_for(ui, response.id, ui.max_rect(), EXPLAIN_AFTER) {
                // Built only while shown, not every frame.
                response.show_tooltip_ui(|ui| {
                    if let Some(explanation) = Explanation::for_status(s, status, detail, hint) {
                        ui.set_max_width(560.0);
                        explanation.show(ui, ui.id().with("tooltip"));
                    }
                    if last_known {
                        ui.label(RichText::new(LAST_KNOWN).color(pal.text_muted));
                    }
                });
            }
        }
        _ => {
            if let Some(text) = detail.or(last_known.then_some(LAST_KNOWN)) {
                response.on_hover_text(text);
            }
        }
    }
}

/// A plugin's server's status, in the list and the details: where to see it.
const PLUGIN_STATUS: &str = "See /mcp";
/// Why Klefki has no status for a plugin's server, and where it is.
const PLUGIN_NOT_CHECKED: &str =
    "Klefki does not check a plugin's servers. In Claude Code, /mcp shows their status.";
/// Why Klefki has no status for the organisation's server, and where it is.
const CONNECTOR_NOT_CHECKED: &str =
    "Klefki does not check claude.ai connectors. In Claude Code, /mcp shows their status.";
const ORGANISATION_NOT_CHECKED: &str = "Klefki does not check the servers your organisation sets. In Claude Code, /mcp shows their status.";
/// The servers of `org`'s name, configured by hand, that Claude Code does not load because the organisation sets it.
fn replaced_by_organisation(servers: &[ServerRow], org: &ServerRow) -> Vec<String> {
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

/// The status of a server Claude Code loads nowhere (decision 009).
const NOT_LOADED: &str = "Not loaded";
/// The status of a server Claude Code's policy keeps out wherever it would load (decision 010).
const BLOCKED_BY_POLICY: &str = "Blocked by policy";
/// The status of a claude.ai connector the user's own settings turn off wherever it would load (decision 011).
const TURNED_OFF: &str = "Connectors off";

/// Why Klefki never checks `s`, if it doesn't: Claude Code does not load it, or it is not one Klefki passes to Claude
/// Code (decisions 008 and 009).
fn not_checked(s: &ServerRow) -> Option<String> {
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

/// Seconds the pointer rests on a failed status before it is explained.
const EXPLAIN_AFTER: f64 = 0.1;

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
        ctx.request_repaint_after_secs(left as f32);
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
fn relative_job(ui: &Ui, paths: &[String], color: Color32) -> egui::text::LayoutJob {
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

/// What the details say under a claude.ai connector (decision 011).
const CONNECTOR_READ_ONLY: &str =
    "A connector of your claude.ai account. Change it on claude.ai; Klefki switches it on and off.";
/// More on claude.ai connectors, on request.
const ABOUT_CONNECTORS: &str = "Claude Code fetches your connectors from claude.ai each session. Klefki lists the \
     ones Claude Code has connected before, so one you have since removed on claude.ai may still be here, and one you \
     have never used is not. A server you add yourself at the same address takes the connector's place; Klefki cannot \
     see a connector's address, so it cannot say when that happens.";

/// What the details say under a plugin's server.
fn managed_read_only(file: &str) -> String {
    format!(
        "Your organisation sets this server, in {file}. Klefki switches it on and off; only an administrator can \
         change it."
    )
}

fn plugin_read_only(p: &klefki_core::api::PluginInfo) -> String {
    format!(
        "Claude Code reads this server from the plugin {}. Klefki switches it on and off; to change it, change the \
         plugin, or turn the plugin off in Claude Code (/plugin).",
        p.plugin
    )
}

/// A plugin's server whose plugin is off in every project Claude Code knows: it loads in none of them.
fn plugin_off_everywhere(s: &ServerRow, project_count: usize) -> bool {
    project_count > 0
        && s.plugin
            .as_ref()
            .is_some_and(|p| p.off_in.len() == project_count)
}

/// Where a plugin's server's plugin is on, naming where it is off by folder, at most a few.
fn plugin_on_text(p: &klefki_core::api::PluginInfo, project_count: usize) -> String {
    let off = p.off_in.len();
    let named: Vec<&str> = p.off_in[..off.min(SHADOWED_NAMED)]
        .iter()
        .map(|p| folder_name(p))
        .collect();
    let mut text = format!(
        "in {} of {project_count} projects; off in {}",
        project_count.saturating_sub(off),
        named.join(", ")
    );
    if off > SHADOWED_NAMED {
        text.push_str(&format!(" and {} more", off - SHADOWED_NAMED));
    }
    text
}

/// At most this many projects are named where a server is shadowed; the rest are counted.
const SHADOWED_NAMED: usize = 5;

/// Where another server of the same name is used instead of `s`, for the detail panel.
fn shadowed_text(s: &ServerRow, project_count: usize) -> String {
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
fn in_projects_text(projects: &[String], project_count: usize, all: bool) -> String {
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
    text.push_str(&named.join(", "));
    if count > SHADOWED_NAMED {
        text.push_str(&format!(" and {} more", count - SHADOWED_NAMED));
    }
    text
}

/// At most this many whole paths are shown on hover; a tooltip taller than the window helps no one.
const SHADOWED_PATHS: usize = 20;

/// The whole paths of the projects where `s` is shadowed, one per line.
fn shadowed_paths(s: &ServerRow) -> String {
    projects_paths(&s.shadowed_in)
}

/// The whole paths of `projects`, one per line, at most a screenful.
fn projects_paths(projects: &[String]) -> String {
    let count = projects.len();
    let mut text = projects[..count.min(SHADOWED_PATHS)].join("\n");
    if count > SHADOWED_PATHS {
        text.push_str(&format!("\nand {} more", count - SHADOWED_PATHS));
    }
    text
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

/// What the Auth tab knows about one server's client secret.
struct SecretView {
    id: ServerId,
    /// The store's last answer: stored or not, or why it could not say.
    known: Option<Result<bool, String>>,
    /// Waiting for the store: what to say meanwhile.
    busy: Option<&'static str>,
    /// Forget… was pressed; asking before doing it.
    confirm_forget: bool,
}

/// The row and the `live_gen` an explanation was built for.
type ExplainedFor = (usize, u64);

/// The engine's latest answer for one row.
#[derive(Clone)]
struct Live {
    status: Status,
    detail: Option<String>,
    hint: Option<Hint>,
}

/// Which ticked places already have a server of a given name, for many names at once.
struct Taken<'a> {
    user: bool,
    /// The ticked projects, in order.
    ticked: Vec<&'a str>,
    /// By name: whether the user has one, and the ticked projects (in `ticked`) that have one of their own.
    names: HashMap<&'a str, (bool, Vec<usize>)>,
}

impl<'a> Taken<'a> {
    fn new(places: &'a Places, servers: impl Iterator<Item = &'a ServerRow>) -> Self {
        let ticked: Vec<&str> = places.ticked().collect();
        let mut at: HashMap<_, Vec<usize>> = HashMap::new();
        for (i, project) in ticked.iter().enumerate() {
            at.entry(klefki_core::api::project_key(project))
                .or_default()
                .push(i);
        }
        let mut names: HashMap<&str, (bool, Vec<usize>)> = HashMap::new();
        for s in servers {
            match (s.scope, s.project.as_deref()) {
                (Scope::User, None) => names.entry(s.name.as_str()).or_default().0 = true,
                (Scope::Local, Some(project)) => {
                    if let Some(found) = at.get(&klefki_core::api::project_key(project)) {
                        names.entry(s.name.as_str()).or_default().1.extend(found);
                    }
                }
                _ => {}
            }
        }
        Self {
            user: places.user,
            ticked,
            names,
        }
    }

    /// The ticked places that already have a server called `name`, in words.
    fn of(&self, name: &str) -> Option<String> {
        let (user, found) = self.names.get(name)?;
        let mut found = found.clone();
        found.sort_unstable();
        found.dedup();
        let folders: Vec<&str> = found.iter().map(|&i| folder_name(self.ticked[i])).collect();
        places_in_words(self.user && *user, &folders)
    }
}

/// Where each line of a plan's diffs starts, laid out once per plan, file and width.
#[derive(Default)]
struct DiffTops {
    plan: u64,
    /// Per file: the width laid out at, and the top of each line, with the end of the last one after them.
    files: HashMap<usize, (f32, Vec<f32>)>,
}

impl DiffTops {
    fn of(
        &mut self,
        ui: &Ui,
        plan: u64,
        file: usize,
        lines: &[String],
        height: impl Fn(&Ui, &str) -> f32,
    ) -> &[f32] {
        if self.plan != plan {
            self.plan = plan;
            self.files.clear();
        }
        let width = ui.available_width();
        let gap = ui.spacing().item_spacing.y;
        let (laid_at, tops) = self.files.entry(file).or_insert((f32::NAN, Vec::new()));
        if *laid_at != width || tops.len() != lines.len() + 1 {
            *laid_at = width;
            tops.clear();
            tops.push(0.0);
            let mut y = 0.0;
            for line in lines {
                y += height(ui, line) + gap;
                tops.push(y);
            }
        }
        tops
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn grid_lists_fixture_servers_grouped_by_scope() {
        let mut h = harness();
        h.run();
        h.get_by_label("User");
        h.get_by_label("issue-tracker");
        h.get_by_label("Local · C:/Repos/Gateway");
        h.get_by_label("Project · C:/Repos/WebApp");
    }

    #[test]
    fn search_filters_the_grid() {
        let mut app = ShellApp::new(fixture_snapshot(), ThemeChoice::Dark);
        let all = app.visible().len();
        app.search = "it.example".into();
        let hits = app.visible();
        assert_eq!(hits.len(), 1);
        assert!(hits.len() < all);
        app.search = "NO-SUCH-SERVER".into();
        assert!(app.visible().is_empty());
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
        h.get_by_label("issue-tracker").click();
        h.run();
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
    fn unknown_cli_version_is_not_shown_as_a_claim() {
        let mut h = harness_with(Snapshot::default());
        h.run();
        assert!(h.query_by_label_contains("Claude CLI").is_none());
    }

    #[test]
    fn nav_and_toolbar_targets_are_large() {
        let mut h = harness();
        h.run();
        let nav = h.get_by_label_contains("Secrets").rect();
        assert!(nav.height() >= 40.0, "nav item {nav:?}");
        // (Was the Add button; the toolbar now shows only actions that work, so the check uses Edit.)
        let add = h.get_by_label(icon::PENCIL_SIMPLE).rect();
        assert!(
            add.width() >= 40.0 && add.height() >= 34.0,
            "toolbar button {add:?}"
        );
    }

    #[test]
    fn grid_renders_any_snapshot_not_just_the_fixture() {
        // Loading from a real config is tested in klefki-core; the UI only renders what it is given.
        let row = |name: &str| ServerRow {
            connection: klefki_core::api::ConnectionId::new(Scope::User, None, name),
            name: name.to_owned(),
            scope: Scope::User,
            project: None,
            transport: klefki_core::api::TransportKind::Http,
            target: format!("https://{name}.example.com/mcp"),
            status: Status::Connected,
            enabled: true,
            disabled_in: 0,
            disabled_projects: Vec::new(),
            approval: None,
            shadowed: false,
            shadowed_in: Vec::new(),
            casing_collision: false,
            relative_paths: Vec::new(),
            status_cached: false,
            oauth: None,
            plugin: None,
            managed: None,
            not_loaded: None,
            blocked: None,
            may_not_load: None,
        };
        let mut h = harness_with(Snapshot {
            servers: vec![row("REMOTE_1"), row("REMOTE_2")],
            ..Snapshot::default()
        });
        h.run();
        h.get_by_label("REMOTE_1");
        h.get_by_label("REMOTE_2");
    }

    #[test]
    fn sanitised_fixture_text_reaches_the_ui() {
        let mut h = harness();
        h.run();
        // The fixture target contains ANSI escapes; the UI must only see the clean text.
        h.get_by_label("missing command");
    }

    #[test]
    fn theme_dropdown_offers_every_theme_and_switches_the_palette() {
        use ThemeChoice::{Dark, Light, System};
        let shown = |o: ThemeOption| format!("{} {}", o.icon(), o.label());
        let mut h = harness();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::DARK.panel);
        let cool_dark = ThemeOption::new(Dark, Tone::Cool);

        // Closed, the dropdown shows the current theme only.
        h.get_by_value(&shown(cool_dark));
        assert!(h.query_by_label_contains("Light").is_none());
        assert!(h.query_by_label_contains("System").is_none());

        // Open, every theme is offered, each with its own icon, and the pick closes the list.
        h.get_by_value(&shown(cool_dark)).click();
        h.run();
        for o in ThemeOption::ALL {
            h.query_by_label_contains(&shown(o))
                .unwrap_or_else(|| panic!("{} is not offered", o.label()));
        }
        h.get_by_label(&shown(ThemeOption::new(Light, Tone::Cool)))
            .click();
        h.run();
        assert_eq!(h.state().theme, Light);
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
        assert!(
            h.query_by_label_contains("Dark (warm)").is_none(),
            "the list stays open after a pick"
        );

        // A warm one sets the greys too.
        let warm_light = ThemeOption::new(Light, Tone::Warm);
        h.get_by_value(&shown(ThemeOption::new(Light, Tone::Cool)))
            .click();
        h.run();
        h.get_by_label_contains(&shown(warm_light)).click();
        h.run();
        assert_eq!((h.state().theme, h.state().tone), (Light, Tone::Warm));
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_LIGHT.panel);

        // System with warm greys is one pick, and the closed dropdown says so.
        h.get_by_value(&shown(warm_light)).click();
        h.run();
        let system_warm = ThemeOption::new(System, Tone::Warm);
        h.get_by_label_contains(&shown(system_warm)).click();
        h.run();
        assert_eq!((h.state().theme, h.state().tone), (System, Tone::Warm));
        h.get_by_value(&shown(system_warm));

        // ... and back to cool greys in one pick.
        h.get_by_value(&shown(system_warm)).click();
        h.run();
        h.get_by_label_contains(&shown(ThemeOption::new(System, Tone::Cool)))
            .click();
        h.run();
        assert_eq!((h.state().theme, h.state().tone), (System, Tone::Cool));
    }

    #[test]
    fn only_the_current_theme_has_a_check_mark() {
        let current = ThemeOption::new(ThemeChoice::Dark, Tone::Cool);
        let mut h = harness();
        h.run();
        h.get_by_value(&format!("{} {}", current.icon(), current.label()))
            .click();
        h.run();
        let checked = format!("{} {}  {}", current.icon(), current.label(), icon::CHECK);
        h.get_by_label(&checked);
        assert_eq!(
            ThemeOption::ALL
                .iter()
                .filter(|o| h
                    .query_by_label_contains(&format!("{}  {}", o.label(), icon::CHECK))
                    .is_some())
                .count(),
            1
        );
    }

    #[test]
    fn light_theme_renders_the_whole_shell() {
        let mut h = harness_themed((*fixture_snapshot()).clone(), ThemeChoice::Light);
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
        h.get_by_label("issue-tracker").click();
        h.run();
        h.get_by_label_contains("Secrets");
        assert_eq!(h.get_all_by_label("https://it.example.com/mcp").count(), 2);
    }

    #[test]
    fn the_theme_choice_is_saved() {
        use eframe::App as _;
        let mut storage = crate::theme::tests::MemoryStorage::default();
        let mut app = ShellApp::new(fixture_snapshot(), ThemeChoice::Light);
        app.save(&mut storage);
        assert_eq!(storage.0.get("theme").map(String::as_str), Some("light"));
    }

    #[test]
    fn detail_tabs_do_not_change_size_on_hover() {
        let mut h = harness();
        h.run();
        h.get_by_label("issue-tracker").click();
        h.run();
        // (The Env tab was called "Headers/Env" when this test was written.)
        for tab in ["Auth", "Env", "General"] {
            let before = h.get_by_label(tab).rect();
            h.get_by_label(tab).hover();
            h.run();
            let after = h.get_by_label(tab).rect();
            assert_eq!(before, after, "tab {tab} moved or resized on hover");
        }
    }

    #[test]
    fn dropdown_options_do_not_change_size_on_hover() {
        let dark = ThemeOption::new(ThemeChoice::Dark, Tone::Cool);
        let shown = format!("{} {}", dark.icon(), dark.label());
        let mut h = harness();
        h.run();
        h.get_by_value(&shown).click();
        h.run();
        for option in ["System (cool)", "Light (cool)", "Dark (warm)"] {
            let before = h.get_by_label_contains(option).rect();
            h.get_by_label_contains(option).hover();
            h.run();
            let after = h.get_by_label_contains(option).rect();
            assert_eq!(before, after, "option {option} moved or resized on hover");
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
        h.get_by_label("acme-prod").click();
        h.run();
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
        h.get_by_label("acme-prod").click();
        h.run();
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
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
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
        engine
            .events
            .send(snapshot_of(vec![other, row.clone()]))
            .unwrap();
        h.run();
        h.get_by_label("acme-prod").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
        h.get_by_label_contains("Can't reach the server's host");
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
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
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
    fn the_top_bar_never_draws_one_thing_over_another() {
        // Found in the UI review: at the smallest size the page's title ran into "Claude CLI 2.1.303". Every width
        // from the smallest to the one Klefki opens at, and the longest version the engine passes on (32 characters).
        use egui::accesskit::Role;
        let widths = (SMALLEST_SIZE.x as usize..=OPENING_SIZE.x as usize).step_by(50);
        for size in widths.map(|w| egui::vec2(w as f32, SMALLEST_SIZE.y)) {
            for version in ["2.1.303", "10.200.3000-beta.4+build.2026101"] {
                let (mut h, engine) = live_harness();
                h.set_size(size);
                engine
                    .events
                    .send(klefki_core::api::Event::CliVersion(Some(
                        version.to_owned(),
                    )))
                    .unwrap();
                h.run();
                let row = h.get_by_label("Klefki").rect();
                let items: Vec<(String, egui::Rect)> = h
                    .query_all_by(|n| {
                        matches!(
                            n.role(),
                            Role::Label | Role::Button | Role::TextInput | Role::ComboBox
                        )
                    })
                    .filter(|n| n.rect().top() < row.bottom() && n.rect().bottom() > row.top())
                    .map(|n| (format!("{n:?}"), n.rect()))
                    .collect();
                assert!(items.len() >= 6, "{size:?}: {items:#?}");
                for (i, (a, ra)) in items.iter().enumerate() {
                    for (b, rb) in &items[i + 1..] {
                        let both = ra.intersect(*rb);
                        assert!(
                            both.width() <= 0.0 || both.height() <= 0.0,
                            "{size:?}: {a} at {ra:?} is drawn over {b} at {rb:?}"
                        );
                    }
                }
            }
        }
        // Where there is room, it is there.
        let (mut h, engine) = live_harness();
        h.set_size(OPENING_SIZE);
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.303".to_owned(),
            )))
            .unwrap();
        h.run();
        h.get_by_label("Claude CLI 2.1.303");
        // Where the whole of it does not fit, the number alone, which says what it is on hover. Where that starts
        // depends on the platform's shortcut labels, so it is looked for across the widths.
        let mut number_only = 0;
        for width in (SMALLEST_SIZE.x as usize..=OPENING_SIZE.x as usize).step_by(10) {
            let (mut h, engine) = live_harness();
            h.set_size(egui::vec2(width as f32, SMALLEST_SIZE.y));
            engine
                .events
                .send(klefki_core::api::Event::CliVersion(Some(
                    "2.1.303".to_owned(),
                )))
                .unwrap();
            h.run();
            if h.query_by_label("Claude CLI 2.1.303").is_none()
                && h.query_by_label("2.1.303").is_some()
            {
                number_only += 1;
                h.get_by_label("2.1.303").hover();
                h.run();
                // By the name it gives itself, as Settings does.
                h.get_by_label("Claude Code 2.1.303");
            }
        }
        assert!(number_only > 0, "the number alone was never shown");
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

    /// Whether the Settings page's "Claude Code" row says `text`, beside its name.
    fn settings_cli_row_says(h: &Harness<'static, ShellApp>, size: egui::Vec2, text: &str) -> bool {
        let page = egui::Rect::from_min_max(egui::pos2(220.0, 60.0), size.to_pos2());
        let row = h.get_by_label("Claude Code").rect();
        assert!(page.contains_rect(row), "{size:?}: Claude Code at {row:?}");
        h.query_all_by_label_contains(text).any(|n| {
            let r = n.rect();
            page.contains_rect(r) && r.bottom() > row.top() && r.left() > row.right()
        })
    }

    /// The Settings page says `text` under its Claude Code row, inside the page.
    fn settings_cli_note_says(
        h: &Harness<'static, ShellApp>,
        size: egui::Vec2,
        text: &str,
    ) -> bool {
        let page = egui::Rect::from_min_max(egui::pos2(220.0, 60.0), size.to_pos2());
        let row = h.get_by_label("Claude Code").rect();
        h.query_all_by_label_contains(text).any(|n| {
            let r = n.rect();
            page.contains_rect(r) && r.top() >= row.bottom()
        })
    }

    #[test]
    fn settings_names_the_claude_code_version_at_any_width() {
        // From the review of the top bar fix: in a narrow window the version showed nowhere.
        for size in [SMALLEST_SIZE, egui::vec2(900.0, 600.0), OPENING_SIZE] {
            let (mut h, engine) = live_harness();
            h.set_size(size);
            engine
                .events
                .send(klefki_core::api::Event::CliVersion(Some(
                    "2.1.303".to_owned(),
                )))
                .unwrap();
            h.run();
            h.get_by_label("Settings").click();
            h.run();
            assert!(settings_cli_row_says(&h, size, "2.1.303"), "{size:?}");
        }
    }

    #[test]
    fn settings_says_when_claude_code_is_not_found_or_not_known_yet() {
        let (mut h, engine) = live_harness();
        h.set_size(OPENING_SIZE);
        h.get_by_label("Settings").click();
        h.run();
        assert!(settings_cli_row_says(&h, OPENING_SIZE, "Not known yet"));
        engine
            .events
            .send(klefki_core::api::Event::CliVersion(Some(
                "2.1.303".to_owned(),
            )))
            .unwrap();
        engine
            .events
            .send(klefki_core::api::Event::CliProblem(
                "Claude Code was not found on PATH.".to_owned(),
            ))
            .unwrap();
        h.run();
        assert!(settings_cli_row_says(&h, OPENING_SIZE, "Not found"));
        assert!(settings_cli_note_says(
            &h,
            OPENING_SIZE,
            "not found on PATH"
        ));
        assert!(!settings_cli_row_says(&h, OPENING_SIZE, "2.1.303"));
    }

    #[test]
    fn settings_says_how_to_get_claude_code_found_and_looks_again() {
        // From the review: "Not found" named the problem but not where Klefki looked, what to do, or a way to try
        // again once it is done. The engine's reason stays, small, for the case that is not a missing program.
        for size in [OPENING_SIZE, SMALLEST_SIZE] {
            let (mut h, engine) = live_harness();
            h.set_size(size);
            h.get_by_label("Settings").click();
            h.run();
            engine
                .events
                .send(klefki_core::api::Event::CliProblem(
                    "Live status is unavailable: /usr/bin/claude is not an executable file."
                        .to_owned(),
                ))
                .unwrap();
            h.run();
            assert!(settings_cli_row_says(&h, size, "Not found"), "{size:?}");
            assert!(settings_cli_note_says(&h, size, "on your PATH"), "{size:?}");
            assert!(
                settings_cli_note_says(&h, size, "not an executable file"),
                "{size:?}"
            );
            drain_all(&engine);
            let again = h.get_by_label("Look again");
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            assert!(window.contains_rect(again.rect()), "{size:?}");
            again.click();
            h.run();
            assert_eq!(
                drain(&engine),
                vec![Command::Refresh { live_all: true }],
                "{size:?}"
            );
            // Found again: the version, and nothing to look again for.
            engine
                .events
                .send(klefki_core::api::Event::CliVersion(Some(
                    "2.1.303".to_owned(),
                )))
                .unwrap();
            h.run();
            assert!(settings_cli_row_says(&h, size, "2.1.303"), "{size:?}");
            assert!(h.query_by_label("Look again").is_none(), "{size:?}");
        }
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
        h.get_by_label("docs").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Status").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![row])).unwrap();
        h.run();
        h.get_by_label("gateway").click();
        h.run();
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
        h.get_by_label(icon::ARROWS_CLOCKWISE).click();
        h.run();
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

    // ---- the toggle is per project; project servers have an approval ----

    fn snapshot_with_projects(count: usize, rows: Vec<ServerRow>) -> klefki_core::api::Event {
        klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
            servers: rows,
            project_count: count,
            ..Snapshot::default()
        }))
    }

    #[test]
    fn a_user_server_off_in_some_projects_says_how_many() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.disabled_in = 3;
        engine
            .events
            .send(snapshot_with_projects(17, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label("gateway");
        h.get_by_label("off in 3/17");
        assert!(
            h.query_by_label_contains("Disabled").is_none(),
            "still on in 14 projects"
        );
    }

    #[test]
    fn a_user_server_off_everywhere_reads_disabled_without_the_partial_badge() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.disabled_in = 17;
        row.enabled = false;
        engine
            .events
            .send(snapshot_with_projects(17, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label_contains("Disabled");
        assert!(h.query_by_label_contains("off in").is_none());
    }

    #[test]
    fn a_user_server_hidden_in_some_projects_says_how_many_and_where() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.shadowed_in = vec!["C:/Repos/WebApp".into(), "C:/Repos/Api".into()];
        engine
            .events
            .send(snapshot_with_projects(17, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label("shadowed in 2/17").hover();
        h.run();
        h.get_by_label_contains(
            "In 2 of 17 projects, a local or project server of the same name is used",
        );
        assert!(
            h.query_by_label("shadowed").is_none(),
            "still used in 15 projects"
        );
        h.get_by_label("gateway").click();
        h.run();
        // Named as their groups are; the whole paths on hover.
        h.get_by_label("in 2 of 17 projects: WebApp, Api").hover();
        h.run();
        h.get_by_label_contains("C:/Repos/WebApp\nC:/Repos/Api");
    }

    #[test]
    fn a_user_server_with_a_relative_path_is_marked_and_its_details_name_the_paths() {
        let (mut h, engine) = live_harness();
        let mut user = live_row("docs", Scope::User, None);
        user.relative_paths = vec!["build/index.js".into(), "./c.json (in CFG)".into()];
        // In its own project's folder a relative path is what it should be.
        let mut local = live_row("tools", Scope::Local, Some("C:/Repos/WebApp"));
        local.relative_paths = vec!["server.py".into()];
        engine
            .events
            .send(snapshot_with_projects(3, vec![user, local]))
            .unwrap();
        h.run();
        let badges: Vec<_> = h.query_all_by_label("relative path").collect();
        assert_eq!(badges.len(), 1, "only the user server");
        h.get_by_label("relative path").hover();
        h.run();
        h.get_by_label_contains(
            "sessions in other folders can't \
             find a file its command names",
        );
        h.get_by_label("docs").click();
        h.run();
        h.get_by_label(
            "Use full paths: sessions in other folders can't find build/index.js or ./c.json (in CFG).",
        );
        h.get_by_label_contains("so the check can pass here");
        h.get_by_label("tools").click();
        h.run();
        assert!(h.query_by_label_contains("Use full paths").is_none());
    }

    #[test]
    fn a_user_server_hidden_everywhere_reads_shadowed_without_the_count() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.shadowed_in = vec!["C:/Repos/WebApp".into(), "C:/Repos/Api".into()];
        row.shadowed = true;
        engine
            .events
            .send(snapshot_with_projects(2, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label("shadowed");
        assert!(h.query_by_label_contains("shadowed in").is_none());
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("in all 2 projects: WebApp, Api");
    }

    #[test]
    fn a_long_list_of_projects_hiding_a_server_is_cut() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.shadowed_in = (1..=9).map(|n| format!("/p{n}")).collect();
        engine
            .events
            .send(snapshot_with_projects(40, vec![row]))
            .unwrap();
        h.run();
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("in 9 of 40 projects: p1, p2, p3, p4, p5 and 4 more");
    }

    #[test]
    fn badges_that_do_not_fit_beside_the_name_are_counted_not_spilled() {
        let (mut h, engine) = live_harness();
        let mut row = live_row("gateway", Scope::User, None);
        row.disabled_in = 3;
        row.shadowed_in = vec!["C:/Repos/WebApp".into(), "C:/Repos/Api".into()];
        row.casing_collision = true;
        engine
            .events
            .send(snapshot_with_projects(17, vec![row]))
            .unwrap();
        for size in [SMALLEST_SIZE, OPENING_SIZE] {
            h.set_size(size);
            h.run();
            h.get_by_label("gateway");
            let type_left = h.get_by_label("Type").rect().left();
            let mut shown = 0;
            for text in ["casing", "off in 3/17", "shadowed in 2/17"] {
                if let Some(b) = h.query_by_label(text) {
                    assert!(b.rect().right() <= type_left, "{text} spills at {size:?}");
                    shown += 1;
                }
            }
            let more = (1..=3)
                .find_map(|n| h.query_by_label(&format!("+{n}")).map(|b| (n, b.rect())))
                .map_or(0, |(n, rect)| {
                    assert!(rect.right() <= type_left, "+{n} spills at {size:?}");
                    n
                });
            assert_eq!(shown + more, 3, "at {size:?}");
        }
    }

    #[test]
    fn the_server_used_instead_says_which_user_server_it_replaces() {
        let (mut h, engine) = live_harness();
        let mut user = live_row("gateway", Scope::User, None);
        user.shadowed_in = vec!["C:/Repos/WebApp".into()];
        let local = live_row("gateway", Scope::Local, Some("C:/Repos/WebApp"));
        // Same name elsewhere, but the user server is not hidden there (say, turned off): it replaces nothing.
        let other = live_row("gateway", Scope::Local, Some("C:/Repos/Api"));
        engine
            .events
            .send(snapshot_with_projects(3, vec![user, local, other]))
            .unwrap();
        h.run();
        // The two local rows follow the user row, in either order; the detail panel says which is selected.
        h.get_all_by_label("gateway").nth(1).unwrap().click();
        h.run();
        let mut seen = 0;
        for step in 0..2 {
            if step == 1 {
                h.key_press(egui::Key::ArrowDown);
                h.run();
            }
            if h.query_by_label("C:/Repos/WebApp").is_some() {
                h.get_by_label("Replaces");
                h.get_by_label("the user server gateway");
                seen += 1;
            } else {
                h.get_by_label("C:/Repos/Api");
                assert!(h.query_by_label("Replaces").is_none());
            }
        }
        assert_eq!(seen, 1);
    }

    #[test]
    fn a_rejected_project_server_says_rejected_and_pending_is_a_status() {
        let (mut h, engine) = live_harness();
        let mut rejected = live_row("no-thanks", Scope::Project, Some("C:/Repos/WebApp"));
        rejected.approval = Some(klefki_core::api::Approval::Rejected);
        let mut pending = live_row("waiting", Scope::Project, Some("C:/Repos/WebApp"));
        pending.approval = Some(klefki_core::api::Approval::Pending);
        pending.status = Status::PendingApproval;
        engine
            .events
            .send(snapshot_with_projects(1, vec![rejected, pending]))
            .unwrap();
        h.run();
        h.get_by_label("\u{25CF} Rejected");
        h.get_by_label("\u{25CF} Pending approval");
    }

    #[test]
    fn the_detail_panel_explains_the_toggle_and_the_approval() {
        let (mut h, engine) = live_harness();
        let mut user = live_row("gateway", Scope::User, None);
        user.disabled_in = 3;
        let mut project = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
        project.approval = Some(klefki_core::api::Approval::Pending);
        engine
            .events
            .send(snapshot_with_projects(17, vec![user, project]))
            .unwrap();
        h.run();
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("disabled in 3 of 17 projects");
        h.get_by_label("docs").click();
        h.run();
        h.get_by_label("Pending approval");
        assert!(h.query_by_label_contains("of 17 projects").is_none());
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

    #[test]
    fn a_diagnosis_is_shown_in_the_status_tab() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(
                    Status::Failed,
                    &[("The command was not found", "Claude could not start the server because the program is not on the PATH it uses.")],
                    &["'xyz' is not recognized as an internal or external command,", "operable program or batch file."],
                    &["[ERROR] MCP server \"bad-stdio\" Connection failed (CONNECTION_CLOSED)"],
                    Some("the client wrote no debug output"),
                ),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        h.get_by_label("The command was not found");
        h.get_by_label_contains("not on the PATH it uses");
        h.get_by_label("The server said");
        h.get_by_label("'xyz' is not recognized as an internal or external command,");
        h.get_by_label("operable program or batch file.");
        h.get_by_label_contains("the client wrote no debug output");
    }

    /// Both sizes the detail panel must work at: the one Klefki opens at, and the smallest it can be made (which is
    /// also the room the opening size gives at 150 % zoom).
    const SIZES: [egui::Vec2; 2] = [OPENING_SIZE, SMALLEST_SIZE];

    /// Where the detail panel's content can be seen: the height of its scroll bar, the rightmost one, when it has
    /// one; otherwise under its tabs to the bottom of the window.
    fn detail_view(h: &Harness<'static, ShellApp>, size: egui::Vec2) -> egui::Rect {
        use egui::accesskit::Role;
        let bar = h
            .query_all_by_role(Role::ScrollBar)
            .map(|b| b.rect())
            .max_by(|a, b| a.right().total_cmp(&b.right()))
            .filter(|b| b.right() > size.x - 40.0);
        let (top, bottom) = match bar {
            Some(b) => (b.top(), b.bottom()),
            None => (
                h.get_by_role_and_label(Role::Button, "General")
                    .rect()
                    .bottom(),
                size.y,
            ),
        };
        egui::Rect::from_min_max(
            egui::pos2(0.0, top.max(0.0)),
            egui::pos2(size.x, bottom.min(size.y)),
        )
    }

    /// Turns the wheel over the detail panel, a notch at a time, until `label` can be seen, and says where it was at
    /// first and where it ended up. It must start out of view, or the test proves nothing.
    fn wheel_the_detail_panel_to(
        h: &mut Harness<'static, ShellApp>,
        size: egui::Vec2,
        label: &str,
    ) -> (egui::Rect, egui::Rect) {
        let first = h.get_by_label(label).rect();
        let view = detail_view(h, size);
        assert!(
            !view.contains_rect(first),
            "{label} is already in view at {first:?} in {view:?}; the panel must be taller than that"
        );
        let over = egui::pos2(first.center().x, view.center().y);
        let mut rect = first;
        for _ in 0..60 {
            if detail_view(h, size).contains_rect(rect) {
                break;
            }
            h.event(egui::Event::PointerMoved(over));
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -120.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            for _ in 0..10 {
                h.step();
            }
            rect = h.get_by_label(label).rect();
        }
        (first, rect)
    }

    /// The live rows `rows` at `size`, the last one shown on its Status tab with a diagnosis of `lines` lines of what
    /// the server said: taller than the window.
    fn long_diagnosis_shown(
        size: egui::Vec2,
        rows: &[ServerRow],
        lines: usize,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        h.set_size(size);
        let row = rows.last().expect("a row to show");
        engine.events.send(snapshot_of(rows.to_vec())).unwrap();
        let said: Vec<String> = (0..lines).map(|i| format!("server line {i}")).collect();
        let said: Vec<&str> = said.iter().map(String::as_str).collect();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(
                    Status::Failed,
                    &[
                        ("The command was not found", "Claude could not start the server because the program is not on the PATH it uses."),
                        ("The server stopped while starting", "It closed its output before it answered, which usually means it crashed on start-up."),
                        ("A value it needs is missing", "Check the environment variables the server's documentation lists."),
                    ],
                    &said,
                    &["[ERROR] MCP server \"bad-stdio\" Connection failed (CONNECTION_CLOSED)"],
                    Some("the client wrote no debug output"),
                ),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, &row.name);
        (h, engine)
    }

    #[test]
    fn a_long_diagnosis_scrolls_into_view() {
        // Several causes and what the server said make the Status tab taller than the window; the report, and the
        // button that copies it, must still be reachable.
        for size in SIZES {
            let row = live_row("bad-stdio", Scope::User, None);
            let (mut h, _engine) = long_diagnosis_shown(size, &[row], 8);
            let (_, rect) = wheel_the_detail_panel_to(&mut h, size, "Copy report");
            assert!(
                detail_view(&h, size).contains_rect(rect),
                "{size:?}: Copy report at {rect:?}"
            );
            // What was scrolled past is still there, above.
            h.get_by_label("The command was not found");
        }
    }

    #[test]
    fn the_name_and_tabs_stay_in_view_while_the_details_scroll() {
        use egui::accesskit::Role;
        for size in SIZES {
            let row = live_row("bad-stdio", Scope::User, None);
            let (mut h, _engine) = long_diagnosis_shown(size, &[row], 8);
            let tabs = h.get_by_role_and_label(Role::Button, "General").rect();
            wheel_the_detail_panel_to(&mut h, size, "Copy report");
            assert_eq!(
                h.get_by_role_and_label(Role::Button, "General").rect(),
                tabs,
                "{size:?}: the tabs moved"
            );
            // The heading, and the row in the list, which both name the server.
            for name in h.query_all_by_label("bad-stdio") {
                assert!(
                    name.rect().top() >= 0.0,
                    "{size:?}: a name at {:?}",
                    name.rect()
                );
            }
            h.get_by_role_and_label(Role::Button, "General").click();
            h.run();
            h.get_by_label("Shadowed");
        }
    }

    #[test]
    fn another_servers_details_start_at_the_top() {
        let (mut h, engine) = live_harness();
        h.set_size(OPENING_SIZE);
        let a = live_row("first-stdio", Scope::User, None);
        let b = live_row("second-stdio", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        let said: Vec<String> = (0..30).map(|i| format!("server line {i}")).collect();
        let said: Vec<&str> = said.iter().map(String::as_str).collect();
        for row in [&a, &b] {
            engine
                .events
                .send(klefki_core::api::Event::Diagnosis {
                    id: row.id(),
                    diagnosis: diagnosis(Status::Failed, &[], &said, &[], None),
                })
                .unwrap();
        }
        h.run();
        open_status_tab(&mut h, "first-stdio");
        let (first, scrolled) = wheel_the_detail_panel_to(&mut h, OPENING_SIZE, "Copy report");
        assert!(scrolled.top() < first.top());
        h.get_by_label("second-stdio").click();
        h.run();
        let rect = h.get_by_label("Copy report").rect();
        assert!(
            (rect.top() - first.top()).abs() < 1.0,
            "at {rect:?}, not where the first one started, {first:?}"
        );
    }

    /// Scrolls the last of `rows` to its Copy report, then lists `then` instead, and says where Copy report was
    /// before and after.
    fn copy_report_across_a_new_list(
        rows: &[ServerRow],
        then: Vec<ServerRow>,
    ) -> (egui::Rect, egui::Rect) {
        let (mut h, engine) = long_diagnosis_shown(OPENING_SIZE, rows, 30);
        let (_, before) = wheel_the_detail_panel_to(&mut h, OPENING_SIZE, "Copy report");
        engine.events.send(snapshot_of(then)).unwrap();
        h.run();
        (before, h.get_by_label("Copy report").rect())
    }

    #[test]
    fn a_server_added_above_leaves_the_details_where_they_were() {
        // Rows are kept by position, so the server shown moves down a place when one is added above it (Add,
        // Duplicate, or `claude mcp add` in a terminal). Where its details were scrolled to goes with it.
        let (a, b, c) = (
            live_row("a-stdio", Scope::User, None),
            live_row("b-stdio", Scope::User, None),
            live_row("c-stdio", Scope::User, None),
        );
        let (before, after) = copy_report_across_a_new_list(&[b.clone(), c.clone()], vec![a, b, c]);
        assert!(
            (after.top() - before.top()).abs() < 1.0,
            "{before:?} then {after:?}"
        );
    }

    #[test]
    fn a_server_removed_above_leaves_the_details_where_they_were() {
        let (a, b, c) = (
            live_row("a-stdio", Scope::User, None),
            live_row("b-stdio", Scope::User, None),
            live_row("c-stdio", Scope::User, None),
        );
        let (before, after) = copy_report_across_a_new_list(&[a, b.clone(), c.clone()], vec![b, c]);
        assert!(
            (after.top() - before.top()).abs() < 1.0,
            "{before:?} then {after:?}"
        );
    }

    /// A row that failed, with an explanation of several steps on its Status tab.
    fn failed_with_steps(row: &ServerRow) -> klefki_core::api::Event {
        klefki_core::api::Event::Status {
            id: row.id(),
            status: Status::Failed,
            detail: Some("spawn npx ENOENT".to_owned()),
            hint: Some(klefki_core::api::Hint {
                title: "The command was not found".to_owned(),
                code: Some("ENOENT".to_owned()),
                help: vec![
                    "Claude Code could not find npx on the PATH it starts servers with.".to_owned(),
                    "Install Node.js, which brings npx with it, then open a new terminal so the PATH is read again.".to_owned(),
                    "If it is installed somewhere unusual, write the whole path to npx as the command instead.".to_owned(),
                    "On Windows, a server started through npx may need `cmd /c npx …` as its command.".to_owned(),
                    "Check that the server's package name is spelled as its documentation gives it, scope included.".to_owned(),
                    "Run the command yourself in a terminal to see the error it prints before Claude Code does.".to_owned(),
                    "If the server needs a newer Node.js than the one installed, update Node.js and check again.".to_owned(),
                ],
                points_at: None,
            }),
        }
    }

    #[test]
    fn the_status_actions_are_in_view_without_scrolling() {
        for size in SIZES {
            let (mut h, engine) = live_harness();
            h.set_size(size);
            let row = live_row("bad-stdio", Scope::User, None);
            engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
            engine.events.send(failed_with_steps(&row)).unwrap();
            h.run();
            open_status_tab(&mut h, "bad-stdio");
            h.get_by_label_contains("write the whole path to npx");
            let view = detail_view(&h, size);
            for action in ["Check now", "Diagnose"] {
                let rect = h.get_by_label(action).rect();
                assert!(view.contains_rect(rect), "{size:?}: {action} at {rect:?}");
            }
        }
    }

    #[test]
    fn a_diagnosis_that_arrives_is_brought_into_view() {
        for size in SIZES {
            let (mut h, engine) = live_harness();
            h.set_size(size);
            let row = live_row("bad-stdio", Scope::User, None);
            engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
            engine.events.send(failed_with_steps(&row)).unwrap();
            h.run();
            open_status_tab(&mut h, "bad-stdio");
            engine
                .events
                .send(klefki_core::api::Event::Diagnosis {
                    id: row.id(),
                    diagnosis: diagnosis(
                        Status::Failed,
                        &[("The command was not found", "Install Node.js.")],
                        &["server line"],
                        &[],
                        None,
                    ),
                })
                .unwrap();
            h.run();
            let rect = h.get_by_label("What probably went wrong").rect();
            assert!(
                detail_view(&h, size).contains_rect(rect),
                "{size:?}: the diagnosis starts at {rect:?}"
            );
        }
    }

    #[test]
    fn a_diagnosis_of_a_server_not_shown_waits_where_it_is() {
        // Brought into view only for the server being looked at: one opened later starts at the top.
        let (mut h, engine) = live_harness();
        h.set_size(OPENING_SIZE);
        let (a, b) = (
            live_row("a-stdio", Scope::User, None),
            live_row("b-stdio", Scope::User, None),
        );
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        engine.events.send(failed_with_steps(&b)).unwrap();
        h.run();
        open_status_tab(&mut h, "a-stdio");
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: b.id(),
                diagnosis: diagnosis(Status::Failed, &[], &["server line"; 40], &[], None),
            })
            .unwrap();
        h.run();
        h.get_by_label("b-stdio").click();
        h.run();
        let view = detail_view(&h, OPENING_SIZE);
        let diagnose = h.get_by_label("Diagnose").rect();
        assert!(view.contains_rect(diagnose), "Diagnose at {diagnose:?}");
        let report = h.get_by_label("Copy report").rect();
        assert!(!view.contains_rect(report), "Copy report at {report:?}");
    }

    /// A definition with 14 arguments: its form is taller than the window.
    fn long_definition() -> String {
        let args: Vec<String> = (0..14).map(|i| format!("\"--flag-{i}\"")).collect();
        format!(
            "{{\n  \"command\": \"node\",\n  \"args\": [{}]\n}}",
            args.join(", ")
        )
    }

    /// The form of [`long_definition`], at `size`.
    fn long_form(size: egui::Vec2) -> Harness<'static, ShellApp> {
        let (mut h, _engine, _) = form_harness(&long_definition(), true);
        h.set_size(size);
        h.run();
        h
    }

    #[test]
    fn a_long_definition_form_scrolls_to_its_last_field() {
        for size in SIZES {
            let mut h = long_form(size);
            let (_, rect) = wheel_the_detail_panel_to(&mut h, size, "Add argument");
            assert!(
                detail_view(&h, size).contains_rect(rect),
                "{size:?}: Add argument at {rect:?}"
            );
        }
    }

    #[test]
    fn the_save_bar_stays_in_view_while_a_long_form_scrolls() {
        for size in SIZES {
            let mut h = long_form(size);
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let save = h.get_by_label("Review and save…").rect();
            assert!(
                window.contains_rect(save),
                "{size:?}: Review and save… at {save:?}"
            );
            wheel_the_detail_panel_to(&mut h, size, "Add argument");
            assert_eq!(h.get_by_label("Review and save…").rect(), save, "{size:?}");
            h.get_by_label("Revert");
        }
    }

    /// Where the outcome message saying `text` is: from the start of its text to its Dismiss button, the last thing
    /// on it. (The text's own rect runs on below what is shown when it scrolls.)
    fn outcome_rect(h: &Harness<'static, ShellApp>, text: &str) -> egui::Rect {
        egui::Rect::from_min_max(
            h.get_by_label(text).rect().min,
            h.get_by_label("Dismiss").rect().max,
        )
    }

    #[test]
    fn the_outcome_message_never_covers_the_save_bar() {
        // Found in the UI review: for four seconds after a change was applied, its message sat over Review and
        // save… and Revert; one that offers Undo or warns stays until dismissed, and covered them all that time.
        // As long as one seen in the app, whose backup folder was deep in a temporary home.
        let long = "Moved files to webapp. The previous file is kept in \
            /home/someone/work/clients/acme/sandboxes/2026-10-19/home/.local/share/klefki/backups/\
            claude.json.0001792408049204-0.bak";
        let outcomes = [
            ("success", "Saved.", false, None),
            ("warning", long, true, None),
            (
                "undo",
                long,
                false,
                Some(Change::Move {
                    id: live_row("alpha", Scope::User, None).id(),
                    to: Target::User,
                }),
            ),
        ];
        for size in SIZES {
            for (kind, text, warn, undo) in outcomes.clone() {
                let mut h = long_form(size);
                form(&mut h).command = "node22".into();
                h.run();
                let save = h.get_by_label("Review and save…").rect();
                h.state_mut().outcome = Some(Outcome {
                    text: text.into(),
                    ok: true,
                    shown_at: None,
                    sign_in: None,
                    warn,
                    // With the button that opens the log, the widest the card's buttons get.
                    logged: kind != "success",
                    type_path: false,
                    undo,
                });
                h.run();
                let toast = outcome_rect(&h, text);
                let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                // The bar, and the panel's width, stay put.
                assert_eq!(
                    h.get_by_label("Review and save…").rect(),
                    save,
                    "{size:?} {kind}"
                );
                assert!(window.contains_rect(toast), "{size:?} {kind}: {toast:?}");
                for label in ["Review and save…", "Revert", "Unsaved"] {
                    let rect = h.get_by_label(label).rect();
                    assert!(
                        !toast.intersects(rect),
                        "{size:?} {kind}: the message at {toast:?} covers {label} at {rect:?}"
                    );
                }
                // Nor the form: its last field can still be scrolled to, and is not under the message. A success
                // fades while the panel is wheeled, so only the ones that stay are tried.
                if kind != "success" {
                    let (_, rect) = wheel_the_detail_panel_to(&mut h, size, "Add argument");
                    let toast = outcome_rect(&h, text);
                    assert!(
                        detail_view(&h, size).contains_rect(rect),
                        "{size:?} {kind}: Add argument can't be scrolled to while the message shows: {rect:?}"
                    );
                    assert!(
                        !toast.intersects(rect),
                        "{size:?} {kind}: the message at {toast:?} covers Add argument at {rect:?}"
                    );
                    h.get_by_label("Dismiss").click();
                    h.run();
                    assert_eq!(h.state().outcome, None, "{size:?} {kind}: Dismiss works");
                }
                h.get_by_label("Revert").click();
                h.run();
                assert_eq!(form(&mut h).command, "node", "{size:?} {kind}");
            }
        }
    }

    #[test]
    fn why_saving_is_blocked_shows_beside_the_save_button() {
        for size in SIZES {
            let mut h = long_form(size);
            form(&mut h).command.clear();
            h.run();
            let Some(Editor {
                state:
                    EditorState::Ready {
                        problem: Some(problem),
                        ..
                    },
                ..
            }) = &h.state().editor
            else {
                panic!("an empty command is a problem");
            };
            let problem = sentence(problem);
            let rect = h.get_by_label(&problem).rect();
            let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            assert!(
                window.contains_rect(rect),
                "{size:?}: {problem:?} at {rect:?}"
            );
            assert!(
                h.get_by_label("Review and save…")
                    .accesskit_node()
                    .is_disabled()
            );
        }
    }

    #[test]
    fn tab_from_the_last_field_goes_on_to_the_save_bar() {
        // The save bar sits below the form, so it comes next, not first; the way to the JSON ends the form.
        let mut h = long_form(OPENING_SIZE);
        form(&mut h).args[13].push('x');
        h.run();
        h.get_by_label("Add argument").focus();
        h.run();
        for next in ["Edit as JSON", "Review and save…"] {
            h.key_press(egui::Key::Tab);
            h.run();
            assert!(h.get_by_label(next).is_focused(), "{next}");
        }
    }

    #[test]
    fn keyboard_focus_brings_what_it_reaches_into_view() {
        // egui does not bring a widget that gains focus into view, as GTK and WinUI do.
        for size in SIZES {
            let mut h = long_form(size);
            h.get_by_label("Remove argument 1").focus();
            h.run();
            for _ in 0..40 {
                if h.get_by_label("Add argument").is_focused() {
                    break;
                }
                h.key_press(egui::Key::Tab);
                h.run();
            }
            let add = h.get_by_label("Add argument");
            assert!(add.is_focused(), "{size:?}: Tab reaches Add argument");
            let rect = add.rect();
            assert!(
                detail_view(&h, size).contains_rect(rect),
                "{size:?}: Add argument has focus at {rect:?}, out of view"
            );
        }
    }

    #[test]
    fn focus_elsewhere_leaves_the_details_where_they_are() {
        let mut h = long_form(OPENING_SIZE);
        let (_, scrolled) = wheel_the_detail_panel_to(&mut h, OPENING_SIZE, "Add argument");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        h.run();
        assert_eq!(h.get_by_label("Add argument").rect(), scrolled);
    }

    #[test]
    fn the_detail_panels_scroll_bar_leaves_the_buttons_beside_it_clickable() {
        // Also with a wider bar than egui's own: the room kept for it follows the style.
        for bar_width in [None, Some(16.0)] {
            let mut h = long_form(OPENING_SIZE);
            if let Some(width) = bar_width {
                h.ctx.all_styles_mut(|s| s.spacing.scroll.bar_width = width);
                h.run();
            }
            // The very edge of a button at the panel's right side, where the scroll bar shows.
            let rect = h.get_by_label("Remove argument 1").rect();
            click_at(&mut h, egui::pos2(rect.right() - 2.0, rect.center().y));
            assert_eq!(
                form(&mut h).args.len(),
                13,
                "{bar_width:?}: the click reached the button"
            );
            assert_eq!(form(&mut h).args[0], "--flag-1");
        }
    }

    /// A click of the mouse at `pos`, wherever that is.
    fn click_at(h: &mut Harness<'static, ShellApp>, pos: egui::Pos2) {
        h.event(egui::Event::PointerMoved(pos));
        h.run();
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.run();
        }
    }

    /// A command with two arguments and a variable: three fields on General, two on Env.
    const SHORT_DEFINITION: &str = "{\n  \"command\": \"node\",\n  \"args\": [\"server.js\", \"--port=3000\"],\n  \"env\": {\"LOG\": \"1\"}\n}";

    /// A way to ask for the selected server's definition.
    type Ask = fn(&mut Harness<'static, ShellApp>);

    /// alpha's definition, `json`, asked for by `ask` at `size`, and arrived.
    fn definition_asked_for(
        size: egui::Vec2,
        json: &str,
        ask: impl FnOnce(&mut Harness<'static, ShellApp>),
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        h.set_size(size);
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        ask(&mut h);
        h.run();
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
        engine.events.send(entry_event(&a, json, true)).unwrap();
        h.run();
        (h, engine)
    }

    /// The detail panel's text fields, top to bottom: those under its tabs, right of the list.
    fn detail_fields(h: &Harness<'static, ShellApp>) -> Vec<egui::Rect> {
        use egui::accesskit::Role;
        let tab = h.get_by_role_and_label(Role::Button, "General").rect();
        let mut fields: Vec<egui::Rect> = h
            .query_all_by_role(Role::TextInput)
            .chain(h.query_all_by_role(Role::PasswordInput))
            .map(|f| f.rect())
            .filter(|f| f.left() >= tab.left() && f.top() >= tab.bottom())
            .collect();
        fields.sort_by(|a, b| a.top().total_cmp(&b.top()));
        fields
    }

    #[test]
    fn the_definition_asked_for_is_brought_into_view() {
        // Found in review: at the smallest size the form arrived below the summary, under the save bar, so Edit
        // definition looked like it did nothing.
        use egui::accesskit::Role;
        let asks: [(&str, Ask); 3] = [
            ("Edit definition on General", |h| {
                h.get_by_label("Edit definition").click();
            }),
            ("Ctrl+E on Status", |h| {
                h.get_by_role_and_label(Role::Button, "Status").click();
                h.run();
                h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
            }),
            ("Edit definition on Env", |h| {
                h.get_by_role_and_label(Role::Button, "Env").click();
                h.run();
                h.get_by_label("Edit definition").click();
            }),
        ];
        for size in SIZES {
            for (how, ask) in asks {
                let (h, _engine) = definition_asked_for(size, SHORT_DEFINITION, ask);
                let view = detail_view(&h, size);
                let fields = detail_fields(&h);
                assert!(fields.len() >= 2, "{size:?}, {how}: {fields:?}");
                for field in fields {
                    assert!(
                        view.contains_rect(field),
                        "{size:?}, {how}: a field at {field:?}, out of {view:?}"
                    );
                }
                let end = h.get_by_label("Edit as JSON").rect();
                assert!(
                    view.contains_rect(end),
                    "{size:?}, {how}: the form ends at {end:?}, out of {view:?}"
                );
            }
            // Taller than the view, it comes in from its top.
            let (h, _engine) = definition_asked_for(size, &long_definition(), asks[0].1);
            let view = detail_view(&h, size);
            let first = detail_fields(&h)[0];
            assert!(
                view.contains_rect(first),
                "{size:?}, a long form: its first field at {first:?}, out of {view:?}"
            );
        }
    }

    #[test]
    fn a_definition_that_arrives_while_another_server_is_shown_waits_where_it_is() {
        // Brought into view only while its server is being looked at: going back to it later starts at its summary.
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        h.set_size(SMALLEST_SIZE);
        let (a, b) = two_rows();
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label("Edit definition").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
        h.get_by_label("beta").click();
        h.run();
        engine
            .events
            .send(entry_event(&a, SHORT_DEFINITION, true))
            .unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        let tab = h.get_by_role_and_label(Role::Button, "General").rect();
        let scope = h
            .get_all_by_label("Scope")
            .map(|l| l.rect())
            .find(|r| r.left() >= tab.left())
            .expect("the summary's first row");
        let view = detail_view(&h, SMALLEST_SIZE);
        assert!(
            view.contains_rect(scope),
            "Scope at {scope:?}, out of {view:?}"
        );
        h.get_by_label("Edit as JSON");
    }

    #[test]
    fn a_second_click_where_edit_definition_was_leaves_the_form_a_form() {
        // Found in review: the save bar came in under the pointer at the smallest size, with the way to the JSON on
        // its second line, so a double click on Edit definition opened the raw JSON.
        for size in SIZES {
            let mut at = egui::Pos2::ZERO;
            let (mut h, engine) = definition_asked_for(size, SHORT_DEFINITION, |h| {
                at = h.get_by_label("Edit definition").rect().center();
                click_at(h, at);
            });
            click_at(&mut h, at);
            assert!(
                h.query_by_label("Back to the form").is_none(),
                "{size:?}: the second click at {at:?} opened the JSON"
            );
            h.get_by_label("Edit as JSON");
            assert_eq!(drain(&engine), vec![], "{size:?}");
        }
    }

    #[test]
    fn the_client_log_is_collapsed_until_opened() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: row.id(),
                diagnosis: diagnosis(
                    Status::Failed,
                    &[],
                    &[],
                    &["LOGLINE-ONE", "LOGLINE-TWO"],
                    None,
                ),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        assert!(
            h.query_by_label_contains("LOGLINE-ONE").is_none(),
            "collapsed by default"
        );
        h.get_by_label_contains("Client log").click();
        h.run();
        h.get_by_label_contains("LOGLINE-ONE");
        h.get_by_label_contains("LOGLINE-TWO");
    }

    #[test]
    fn diagnose_asks_the_engine_and_waits_while_a_check_runs() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        h.get_by_label("Diagnose").click();
        h.run();
        assert_eq!(
            next_command(&engine).unwrap(),
            klefki_core::api::Command::Diagnose(row.id())
        );
        assert!(next_command(&engine).is_none(), "exactly one command");
        // While a check or diagnosis is running, asking again would only queue a duplicate.
        engine
            .events
            .send(status_event(&row, Status::Checking, None))
            .unwrap();
        h.run();
        assert!(h.get_by_label("Diagnose").accesskit_node().is_disabled());
        assert!(h.get_by_label("Check now").accesskit_node().is_disabled());
    }

    #[test]
    fn a_diagnosis_for_a_row_that_is_gone_is_dropped() {
        let (mut h, engine) = live_harness();
        let gone = live_row("gone", Scope::User, None);
        let stays = live_row("stays", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![stays.clone()]))
            .unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: gone.id(),
                diagnosis: diagnosis(Status::Failed, &[], &["NEVER-SHOWN"], &[], None),
            })
            .unwrap();
        h.run();
        open_status_tab(&mut h, "stays");
        assert!(h.query_by_label_contains("NEVER-SHOWN").is_none());
        assert!(h.query_by_label("The server said").is_none());
    }

    #[test]
    fn a_new_snapshot_forgets_diagnoses_of_rows_that_disappeared() {
        let (mut h, engine) = live_harness();
        let a = live_row("a", Scope::User, None);
        let b = live_row("b", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        engine
            .events
            .send(klefki_core::api::Event::Diagnosis {
                id: b.id(),
                diagnosis: diagnosis(Status::Failed, &[], &["B-SAID-THIS"], &[], None),
            })
            .unwrap();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        open_status_tab(&mut h, "b");
        assert!(
            h.query_by_label_contains("B-SAID-THIS").is_none(),
            "an old answer is not shown for a new row"
        );
    }

    #[test]
    fn the_report_to_copy_is_complete_plain_and_free_of_markup() {
        let mut row = live_row("bad-stdio", Scope::Project, Some("C:/Repos/WebApp"));
        row.transport = klefki_core::api::TransportKind::Stdio;
        let d = diagnosis(
            Status::Failed,
            &[(
                "The command was not found",
                "Install it or use its full path.",
            )],
            &["xyz is not recognized"],
            &["[ERROR] MCP server \"bad-stdio\" Connection failed"],
            Some("a note"),
        );
        let text = diagnosis_report(&row, &d);
        for needle in [
            "bad-stdio",
            "Project",
            "C:/Repos/WebApp",
            "Failed",
            "CONNECTION_CLOSED: Connection closed",
            "The command was not found",
            "Install it or use its full path.",
            "xyz is not recognized",
            "Connection failed",
            "a note",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
        assert!(!text.contains('\u{1b}') && !text.contains("\r"), "{text:?}");
        assert!(text.ends_with('\n'));
    }

    // ---- long text must wrap inside the detail panel, never resize it ----

    /// Where the detail panel's contents start. If the panel grows, this moves left.
    fn detail_left_edge(h: &Harness<'static, ShellApp>, label: &str) -> f32 {
        h.get_by_label(label).rect().min.x
    }

    #[test]
    fn a_very_long_status_detail_wraps_instead_of_widening_the_panel() {
        let (mut h, engine) = live_harness();
        let row = live_row("bad-stdio", Scope::User, None);
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        engine
            .events
            .send(status_event(&row, Status::Failed, Some("short")))
            .unwrap();
        h.run();
        open_status_tab(&mut h, "bad-stdio");
        let before = detail_left_edge(&h, "Check now");

        let long =
            "ECONNREFUSED: Unable to connect. Is the computer able to access the url? ".repeat(8);
        engine
            .events
            .send(status_event(&row, Status::Failed, Some(&long)))
            .unwrap();
        h.run();
        let after = detail_left_edge(&h, "Check now");
        assert_eq!(
            before, after,
            "the detail panel changed width because of a long detail"
        );
    }

    #[test]
    fn a_very_long_target_wraps_instead_of_widening_the_panel() {
        let short = {
            let (mut h, engine) = live_harness();
            engine
                .events
                .send(snapshot_of(vec![live_row("srv", Scope::User, None)]))
                .unwrap();
            h.run();
            h.get_by_label("srv").click();
            h.run();
            detail_left_edge(&h, "Scope")
        };
        let long = {
            let (mut h, engine) = live_harness();
            let mut row = live_row("srv", Scope::User, None);
            row.target = format!(
                "https://example.com/{}",
                "very-long-path-segment/".repeat(20)
            );
            engine.events.send(snapshot_of(vec![row])).unwrap();
            h.run();
            h.get_by_label("srv").click();
            h.run();
            detail_left_edge(&h, "Scope")
        };
        assert_eq!(short, long, "a long target made the detail panel wider");
    }

    #[test]
    fn a_name_is_taken_in_the_ticked_places_that_have_it_however_the_project_is_spelled() {
        // Performance review: the check now goes through the rows once for all names. It must find the same places.
        let projects: Vec<String> = ["C:/Repos/WebApp", "/home/u/b", "/home/u/a", "/home/u/c"]
            .map(str::to_owned)
            .to_vec();
        let rows = vec![
            live_row("alpha", Scope::User, None),
            live_row("alpha", Scope::Local, Some("c:\\Repos\\WebApp\\")),
            live_row("alpha", Scope::Local, Some("/home/u/a")),
            live_row("alpha", Scope::Local, Some("/home/u/c")),
            // Only hidden by a new one, not refused: the review says so.
            live_row("alpha", Scope::Project, Some("/home/u/b")),
            live_row("beta", Scope::Local, Some("/home/u/b")),
        ];
        let mut places = Places::new(&projects, None);
        places.user = true;
        for at in [0, 1, 2] {
            places.projects[at].1 = true;
        }
        assert_eq!(
            places.taken("alpha", &rows).as_deref(),
            Some("for the user and in WebApp and a")
        );
        assert_eq!(places.taken("beta", &rows).as_deref(), Some("in b"));
        assert_eq!(places.taken("gamma", &rows), None);
        // Variant: the user not ticked, and a project ticked that has none.
        places.user = false;
        places.projects[0].1 = false;
        assert_eq!(places.taken("alpha", &rows).as_deref(), Some("in a"));
        places.projects[2].1 = false;
        assert_eq!(places.taken("alpha", &rows), None);
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
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        assert!(drain(&engine).is_empty(), "nothing selected, nothing asked");
    }

    #[test]
    fn disable_asks_the_engine_for_a_plan_for_each_selected_server() {
        let (mut h, engine) = live_harness();
        let (a, b) = two_rows();
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_RIGHT).click();
        h.run();
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
        h.get_by_label("Apply").click();
        h.run();
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
    }

    #[test]
    fn cancel_discards_the_plan_and_closes_the_dialog() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Cancel").click();
        h.run();
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
        h.get_by_label("Apply").click();
        h.run();
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
        h.get_by_label("Dismiss").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(8, &["+ alpha"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        h.get_by_label("Apply").click();
        h.run();
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
        h.get_by_label("Apply").click();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(7)]);
        h.get_by_label_contains("Review the change");
    }

    #[test]
    fn a_double_click_on_disable_asks_for_one_plan() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        // The engine has not answered the first click yet.
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        engine
            .events
            .send(Event::PlanFailed("alpha is gone.".to_owned()))
            .unwrap();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        let prepares = drain(&engine)
            .into_iter()
            .filter(|c| matches!(c, Command::Prepare(_)))
            .count();
        assert_eq!(prepares, 2);
    }

    #[test]
    fn a_plan_arriving_while_another_is_open_is_discarded_not_shown() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Apply").click();
        h.run();
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
            engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
            h.run();
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
        engine.events.send(snapshot_of(vec![row.clone()])).unwrap();
        h.run();
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
        engine
            .events
            .send(snapshot_of(vec![approved, user]))
            .unwrap();
        h.run();
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
        h.get_by_label("Approve").click();
        h.run();
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
        h.get_by_label("Add").click();
        h.run();
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
        h.get_by_label("beta").click();
        h.run();
        // Same rows, another order, one more in front.
        engine
            .events
            .send(snapshot_of(vec![c, b.clone(), a.clone()]))
            .unwrap();
        h.run();
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
        engine
            .events
            .send(snapshot_of(vec![live_row("alpha", Scope::User, None)]))
            .unwrap();
        h.run();
        h.get_by_label("alpha");
        engine
            .events
            .send(snapshot_of(vec![
                live_row("alpha", Scope::User, None),
                live_row("omega", Scope::User, None),
            ]))
            .unwrap();
        h.run();
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
        engine
            .events
            .send(snapshot_of(vec![a.clone(), b.clone()]))
            .unwrap();
        h.run();
        h.get_by_label(icon::TRASH).click();
        h.run();
        assert!(drain(&engine).is_empty(), "nothing selected, nothing asked");
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TRASH).click();
        h.run();
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
        engine.events.send(snapshot_of(vec![p.clone()])).unwrap();
        h.run();
        // Under its plugin, by its own name; the details give the name Claude Code uses.
        h.get_by_label("Plugin · docs, from team");
        h.get_by_label("search").click();
        h.run();
        drain(&engine);
        h.get_by_label("plugin:docs:search");
        h.get_by_label("docs, from team");
        h.get_by_label_contains("Claude Code reads this server from the plugin docs.");
        // No form to edit it in.
        assert!(h.query_by_label("Review and save…").is_none());
        for glyph in [icon::TOGGLE_RIGHT, icon::TOGGLE_LEFT] {
            assert!(
                !h.get_by_label(glyph).accesskit_node().is_disabled(),
                "{glyph}"
            );
        }
        for glyph in [
            icon::TRASH,
            icon::PENCIL_SIMPLE,
            icon::COPY,
            icon::ARROW_FAT_LINES_RIGHT,
            icon::EXPORT,
            icon::SIGN_IN,
        ] {
            assert!(
                h.get_by_label(glyph).accesskit_node().is_disabled(),
                "{glyph}"
            );
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
        assert_eq!(drain(&engine), Vec::<Command>::new());
        // Switched like a user server, through a review.
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        engine.events.send(snapshot_of(vec![user, p])).unwrap();
        h.run();
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
        h.get_by_label("search").click();
        h.run();
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
        h.get_by_label("search").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![slack])).unwrap();
        h.run();
        h.get_by_label(PLUGIN_STATUS);
        // Under the claude.ai group, without the prefix it already says.
        assert!(h.query_by_label("claude.ai Slack").is_none());
        h.get_by_label("Slack").click();
        h.run();
        h.get_by_label_contains("A connector of your claude.ai account");
        h.get_by_label("About claude.ai connectors");
        assert!(
            h.get_by_label(icon::PENCIL_SIMPLE)
                .accesskit_node()
                .is_disabled()
        );
        assert!(h.get_by_label(icon::TRASH).accesskit_node().is_disabled());
        drain(&engine);
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        h.get_by_label("Gmail").click();
        h.run();
        h.get_by_label("Connectors off");
        h.get_by_label("disabled in 1 of 3 projects: p1");
        // The type is "claude.ai" (as are the scope and the group), not a guessed "http".
        assert!(h.query_by_label("http").is_none());
        assert!(h.query_all_by_label("on claude.ai").count() >= 2);
        h.get_by_label("Drive").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![slack])).unwrap();
        h.run();
        h.get_by_label(TURNED_OFF);
        assert!(h.query_by_label(BLOCKED_BY_POLICY).is_none());
    }

    #[test]
    fn an_organisations_server_is_read_only_and_switches() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(snapshot_of(vec![organisation_row("docs")]))
            .unwrap();
        h.run();
        // Not checked here: where its status is.
        h.get_by_label(PLUGIN_STATUS);
        h.get_by_label("docs").click();
        h.run();
        h.get_by_label_contains(
            "Your organisation sets this server, in /etc/claude-code/managed-settings.json",
        );
        h.get_by_label("Set in");
        // Nothing to edit, and no values or sign-in tabs.
        assert!(h.query_by_label("Env").is_none());
        assert!(
            h.get_by_label(icon::PENCIL_SIMPLE)
                .accesskit_node()
                .is_disabled()
        );
        assert!(h.get_by_label(icon::TRASH).accesskit_node().is_disabled());
        drain(&engine);
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        h.get_by_label("mine").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![x])).unwrap();
        h.run();
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
            ..live_row("mine", Scope::User, None)
        };
        engine
            .events
            .send(snapshot_of(vec![mine, organisation_row("other")]))
            .unwrap();
        h.run();
        // Said once, in the status column (cross-review of #47): no badge beside the name.
        assert!(h.query_by_label("not loaded").is_none());
        h.get_by_label(NOT_LOADED).hover();
        h.run();
        // Where Claude Code starts it does not matter: it does not start it.
        assert!(h.query_by_label("relative path").is_none());
        h.get_by_label("mine").click();
        h.run();
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
        engine
            .events
            .send(snapshot_of(vec![user.clone(), p.clone()]))
            .unwrap();
        h.run();
        h.state_mut().selected = BTreeSet::from([0, 1]);
        h.run();
        for glyph in [icon::TOGGLE_LEFT, icon::TRASH, icon::EXPORT] {
            assert!(
                !h.get_by_label(glyph).accesskit_node().is_disabled(),
                "{glyph}"
            );
        }
        // Edit takes one server; this selection holds two.
        assert!(
            h.get_by_label(icon::PENCIL_SIMPLE)
                .accesskit_node()
                .is_disabled()
        );
        drain(&engine);
        // Switching takes both.
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
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
        h.get_by_label(icon::EXPORT).click();
        h.run();
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
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
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
        h.get_by_label(icon::PENCIL_SIMPLE).click();
        h.run();
    }

    #[test]
    fn edit_opens_the_form_and_asks_for_the_definition() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Status")
            .click();
        h.run();
        h.get_by_label(icon::PENCIL_SIMPLE).click();
        h.run();
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
            engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
            h.run();
            h.get_by_label("alpha").click();
            h.run();
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Status")
                .click();
            h.run();
            way(&mut h);
            assert_eq!(h.state().tab, DetailTab::General, "{how}");
            assert_eq!(drain(&engine), vec![Command::Inspect(a.id())], "{how}");
        }
    }

    #[test]
    fn json_is_reached_from_the_form_and_leads_back_to_it() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label("Edit as JSON").click();
        h.run();
        h.get_by_value(&json_text(&h));
        h.get_by_label_contains("keep what they are now");
        assert!(
            h.query_by_value("a.js").is_none(),
            "the form is not shown next to the JSON"
        );
        set_json_text(&mut h, |t| t.replace("\"node\"", "\"deno\""));
        h.run();
        h.get_by_label("Back to the form").click();
        h.run();
        assert!(h.query_all_by_value("deno").next().is_some());
        assert!(h.query_by_value(&json_text(&h)).is_none());
        // JSON the form cannot read keeps you in the JSON, saying why.
        h.get_by_label("Edit as JSON").click();
        h.run();
        set_json_text(&mut h, |_| "{ \"command\": ".into());
        h.run();
        h.get_by_label_contains("not valid");
        assert!(
            h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Revert").click();
        h.run();
        assert!(
            !h.get_by_label("Back to the form")
                .accesskit_node()
                .is_disabled()
        );
        // The Env tab follows: the definition is being edited as JSON.
        h.get_by_label("Env").click();
        h.run();
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
        h.get_by_label("Review and save…").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "alpha");
        drain(&engine);
        let original =
            "{\n  \"command\": \"node\",\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}";
        engine.events.send(entry_event(&a, original, true)).unwrap();
        h.run();
        h.get_by_label("Edit as JSON").click();
        h.run();
        h.get_by_label_contains("keep what they are now");
        // Unchanged: nothing to review.
        h.get_by_label("Review and save…").click();
        h.run();
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
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: edited
            }])]
        );
        h.get_by_label("Revert").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label("Edit definition").click();
        h.run();
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

    #[test]
    fn the_auth_tab_shows_the_oauth_client_and_scopes() {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut signed = live_row("acme-prod", Scope::User, None);
        signed.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: Some("klefki-client-1".into()),
            scopes: vec!["read".into(), "write".into()],
        });
        let mut dynamic = live_row("dyn", Scope::User, None);
        dynamic.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: None,
            scopes: Vec::new(),
        });
        let plain = live_row("plain", Scope::User, None);
        engine
            .events
            .send(snapshot_of(vec![signed, dynamic, plain]))
            .unwrap();
        h.run();
        let open = |h: &mut Harness<'static, ShellApp>, name: &str| {
            h.get_by_label(name).click();
            h.run();
            h.get_by_role_and_label(Role::Button, "Auth").click();
            h.run();
        };
        open(&mut h, "acme-prod");
        h.get_by_label("klefki-client-1");
        h.get_by_label("read, write");
        open(&mut h, "dyn");
        h.get_by_label_contains("registers itself");
        open(&mut h, "plain");
        h.get_by_label_contains("does not sign in with OAuth");
    }

    // ---- the client secret, in the system's secret store

    fn oauth_harness() -> (
        Harness<'static, ShellApp>,
        klefki_core::api::Detached,
        ServerRow,
    ) {
        use egui::accesskit::Role;
        let (mut h, engine) = live_harness();
        let mut sso = live_row("sso", Scope::User, None);
        sso.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: Some("klefki-1".into()),
            scopes: Vec::new(),
        });
        engine
            .events
            .send(snapshot_of(vec![
                sso.clone(),
                live_row("plain", Scope::User, None),
            ]))
            .unwrap();
        h.run();
        h.get_by_label("sso").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Auth").click();
        h.run();
        (h, engine, sso)
    }

    fn secret_event(row: &ServerRow, stored: Result<bool, String>) -> Event {
        Event::SecretStatus {
            id: row.id(),
            stored,
        }
    }

    #[test]
    fn the_auth_tab_asks_once_whether_a_client_secret_is_stored() {
        let (mut h, engine, sso) = oauth_harness();
        assert_eq!(drain(&engine), vec![Command::SecretStatus(sso.id())]);
        h.get_by_label_contains("Checking");
        h.run();
        assert!(drain(&engine).is_empty(), "asked once, not every frame");
        engine.events.send(secret_event(&sso, Ok(true))).unwrap();
        h.run();
        h.get_by_label_contains("Stored in your system's secret store");
        // A server that does not sign in with OAuth has no client secret to ask about.
        h.get_by_label("plain").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn a_client_secret_is_typed_hidden_and_handed_over_once() {
        let (mut h, engine, sso) = oauth_harness();
        engine.events.send(secret_event(&sso, Ok(false))).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label_contains("Not stored");
        h.get_by_label("Set client secret…").click();
        h.run();
        h.state_mut()
            .secret_input
            .as_mut()
            .unwrap()
            .edit()
            .push_str("s3cret-value");
        h.run();
        assert!(
            h.query_by_value("s3cret-value").is_none(),
            "not shown, not even to assistive technology"
        );
        h.get_by_label("Save").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::StoreSecret {
                id: sso.id(),
                secret: klefki_core::api::Secret::new("s3cret-value".into()),
            }]
        );
        assert!(h.state().secret_input.is_none(), "the app keeps no copy");
        h.get_by_label_contains("Saving");
    }

    #[test]
    fn forgetting_a_client_secret_asks_first() {
        let (mut h, engine, sso) = oauth_harness();
        engine.events.send(secret_event(&sso, Ok(true))).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label("Forget…").click();
        h.run();
        assert!(drain(&engine).is_empty(), "not yet");
        h.get_by_label_contains("cannot be undone");
        h.get_by_label("Forget it").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::ForgetSecret(sso.id())]);
    }

    #[test]
    fn a_secret_store_problem_is_shown_and_can_be_retried() {
        let (mut h, engine, sso) = oauth_harness();
        drain(&engine);
        engine
            .events
            .send(secret_event(&sso, Err("The system's secret store is not available (no Secret Service). Klefki keeps no plaintext copy of secrets anywhere else.".into())))
            .unwrap();
        h.run();
        h.get_by_label_contains("no plaintext copy");
        h.get_by_label("Try again").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::SecretStatus(sso.id())]);
    }

    #[test]
    fn the_form_edits_the_same_definition_as_the_json_tab() {
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        assert_eq!(form(&mut h).command, "node");
        // Nothing edited yet: nothing to review.
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
        form(&mut h).command = "node22".into();
        form(&mut h).args.push("--x".into());
        h.run();
        let text = json_text(&h);
        assert_eq!(
            text,
            "{\n  \"cwd\": \"/w\",\n  \"command\": \"node22\",\n  \"args\": [\n    \"a.js\",\n    \"--x\"\n  ],\n  \"env\": {\n    \"TOKEN\": \"<hidden>\"\n  }\n}",
            "the key the form does not show and the hidden value stay as they were"
        );
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::Prepare(vec![Change::Replace {
                id: a.id(),
                json: text
            }])]
        );
        // The JSON shows the same draft.
        h.get_by_label("Edit as JSON").click();
        h.run();
        h.get_by_value(&json_text(&h));
    }

    #[test]
    fn editing_the_json_updates_the_form() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        if let Some(Editor {
            state: EditorState::Ready { text, .. },
            ..
        }) = &mut h.state_mut().editor
        {
            *text = text.replace("\"node\"", "\"deno\"");
        }
        h.run();
        assert_eq!(form(&mut h).command, "deno");
        // JSON the form cannot read: the form says so instead of guessing.
        if let Some(Editor {
            state: EditorState::Ready { text, .. },
            ..
        }) = &mut h.state_mut().editor
        {
            *text = "{ \"command\": ".into();
        }
        h.run();
        h.get_by_label_contains("not valid");
        // Revert brings the form back.
        h.get_by_label("Revert").click();
        h.run();
        assert_eq!(form(&mut h).command, "node");
    }

    #[test]
    fn values_stay_hidden_until_changed_and_new_ones_can_be_added() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label("Env").click();
        h.run();
        h.get_by_label("hidden");
        h.get_by_label("Change TOKEN").click();
        h.run();
        assert_eq!(form(&mut h).env, [("TOKEN".to_owned(), String::new())]);
        h.get_by_label("Add variable").click();
        h.run();
        assert_eq!(form(&mut h).env.len(), 2);
        form(&mut h).env[1] = ("MODE".into(), "dev".into());
        form(&mut h).env[0].1 = "n3w".into();
        h.run();
        assert!(
            json_text(&h).contains("\"MODE\": \"dev\""),
            "{}",
            json_text(&h)
        );
        // Typed values are not shown in the clear, to the eye or to assistive technology.
        assert!(h.query_by_value("n3w").is_none());
        assert!(h.query_by_value("dev").is_none());
        assert!(
            h.query_all_by_value("MODE").next().is_some(),
            "names are shown"
        );
        h.get_by_label("Remove MODE").click();
        h.run();
        assert!(!json_text(&h).contains("MODE"));
    }

    #[test]
    fn a_typed_secret_is_met_with_a_reference_to_use_instead() {
        let (mut h, _engine, _a) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label("Env").click();
        h.run();
        // The value that is kept is never offered a hint: the app does not have it.
        assert!(h.query_by_label_contains("Looks like a secret").is_none());
        h.get_by_label("Add variable").click();
        h.run();
        form(&mut h).env[1] = ("MODE".into(), "dev".into());
        h.run();
        assert!(h.query_by_label_contains("Looks like a secret").is_none());
        form(&mut h).env[1] = (
            "GITHUB_TOKEN".into(),
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
        );
        h.run();
        h.get_by_label_contains("Looks like a secret");
        assert!(
            h.query_by_value("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
                .is_none()
        );
        h.get_by_label("Use ${GITHUB_TOKEN} instead").click();
        h.run();
        assert_eq!(form(&mut h).env[1].1, "${GITHUB_TOKEN}");
        assert!(
            json_text(&h).contains("\"GITHUB_TOKEN\": \"${GITHUB_TOKEN}\""),
            "{}",
            json_text(&h)
        );
        assert!(h.query_by_label_contains("Looks like a secret").is_none());
    }

    #[test]
    fn the_form_does_not_widen_the_detail_panel() {
        // Seen in the real window: rows that asked for the panel's whole width plus a button made the resizable
        // panel grow a little every frame, until it reached its largest size.
        let long = "{\n  \"command\": \"npx\",\n  \"args\": [\n    \"-y\",\n    \"@modelcontextprotocol/server-github-with-a-long-name\"\n  ],\n  \"env\": {\n    \"GITHUB_PERSONAL_ACCESS_TOKEN\": \"<hidden>\"\n  }\n}";
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        let before = detail_left_edge(&h, "General");
        h.get_by_label("Edit definition").click();
        h.run();
        engine.events.send(entry_event(&a, long, true)).unwrap();
        h.run();
        assert_eq!(detail_left_edge(&h, "General"), before, "General");
        h.get_by_label("Env").click();
        h.run();
        assert_eq!(detail_left_edge(&h, "General"), before, "Env");
    }

    #[test]
    fn a_problem_in_the_form_is_shown_and_blocks_review() {
        let (mut h, engine, _a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "  ".into();
        h.run();
        h.get_by_label_contains("needs a command");
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Review and save…").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn the_form_asks_for_one_plan_at_a_time() {
        let (mut h, engine, _a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        h.get_by_label("Review and save…").click();
        h.run();
        // The first is on its way: the button waits for its answer.
        assert!(
            h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Review and save…").click();
        h.run();
        assert_eq!(drain(&engine).len(), 1);
    }

    #[test]
    fn project_servers_are_shown_but_not_edited_in_the_form() {
        let (h, _engine, _a) = form_harness("{\n  \"command\": \"x\"\n}", false);
        h.get_by_label_contains("does not edit files in a repository");
        assert!(h.query_by_label("Review and save…").is_none());
    }

    #[test]
    fn a_server_that_signs_in_is_edited_in_the_form_and_says_it_signs_in_again() {
        let (mut h, engine, a) = form_harness(
            "{\n  \"type\": \"http\",\n  \"url\": \"https://h.example/mcp\",\n  \"oauth\": {\n    \"clientId\": \"c\"\n  }\n}",
            true,
        );
        // Before anything is typed.
        h.get_by_label_contains("you sign in again afterwards");
        form(&mut h).url = "https://h.example/v2/mcp".into();
        h.run();
        h.get_by_label("Review and save…").click();
        h.run();
        match &drain(&engine)[..] {
            [Command::Prepare(changes)] => match &changes[..] {
                [Change::Replace { id, json }] => {
                    assert_eq!(*id, a.id());
                    assert!(
                        json.contains("v2/mcp") && json.contains("\"clientId\": \"c\""),
                        "{json}"
                    );
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_project_server_is_shown_read_only() {
        let (mut h, engine) = live_harness();
        let p = live_row("docs", Scope::Project, Some("C:/Repos/WebApp"));
        engine.events.send(snapshot_of(vec![p.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "docs");
        engine
            .events
            .send(entry_event(&p, "{\n  \"command\": \"x\"\n}", false))
            .unwrap();
        h.run();
        h.get_by_label_contains("does not edit files in a repository");
        assert!(h.query_by_label("Review and save…").is_none());
        h.get_by_value("{\n  \"command\": \"x\"\n}");
    }

    #[test]
    fn a_definition_that_cannot_be_read_says_why() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "alpha");
        engine
            .events
            .send(Event::Entry {
                id: a.id(),
                json: Err("the server is no longer in the configuration".into()),
                editable: true,
            })
            .unwrap();
        h.run();
        h.get_by_label_contains("no longer in the configuration");
    }

    #[test]
    fn after_a_save_the_definition_is_read_again() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        press_edit(&mut h, "alpha");
        engine.events.send(entry_event(&a, "{}", true)).unwrap();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::Applied {
                plan: 1,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
    }

    /// Another change, prepared and applied while the form is open: Turn off from the toolbar, say.
    fn apply_another_change(
        h: &mut Harness<'static, ShellApp>,
        engine: &klefki_core::api::Detached,
        plan: u64,
    ) {
        engine
            .events
            .send(Event::PlanReady(plan_view(plan, &["+ alpha"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(drain(engine), vec![Command::Apply(plan)]);
        engine
            .events
            .send(Event::Applied {
                plan,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
    }

    #[test]
    fn a_draft_not_yet_saved_survives_another_change_being_applied() {
        // Found in the UI review: a command edited in the form, then the same server turned off, and the edit was
        // replaced by what was saved before.
        let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        apply_another_change(&mut h, &engine, 3);
        assert!(
            drain(&engine).is_empty(),
            "the definition is not asked for again"
        );
        assert_eq!(form(&mut h).command, "node22");
        assert!(
            !h.get_by_label("Review and save…")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn the_save_bar_says_while_a_draft_is_not_saved() {
        // A draft is kept through other changes, so it says it is not saved yet rather than passing for what the
        // file holds.
        let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
        assert!(h.query_by_label("Unsaved").is_none());
        form(&mut h).command = "node22".into();
        h.run();
        apply_another_change(&mut h, &engine, 3);
        h.get_by_label("Unsaved");
        // Once the message about the other change has faded: it sits over the save bar until then.
        h.state_mut().outcome.as_mut().unwrap().shown_at = Some(-1000.0);
        h.run();
        h.get_by_label("Revert").click();
        h.run();
        assert_eq!(form(&mut h).command, "node");
        assert!(h.query_by_label("Unsaved").is_none());
    }

    #[test]
    fn a_draft_edited_as_json_survives_another_change_being_applied() {
        let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
        set_json_text(&mut h, |t| t.replace("a.js", "b.js"));
        h.run();
        apply_another_change(&mut h, &engine, 3);
        assert!(drain(&engine).is_empty());
        assert!(json_text(&h).contains("b.js"));
    }

    #[test]
    fn a_saved_draft_is_read_again_once_applied() {
        // What was written is read back: the draft's typed values are dropped and the form starts from the file.
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        h.get_by_label("Review and save…").click();
        h.run();
        assert!(matches!(drain(&engine).as_slice(), [Command::Prepare(_)]));
        apply_another_change(&mut h, &engine, 5);
        assert_eq!(drain(&engine), vec![Command::Inspect(a.id())]);
    }

    #[test]
    fn a_draft_whose_review_was_cancelled_or_failed_survives_a_later_change() {
        let (mut h, engine, _) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        // Reviewed, then cancelled.
        h.get_by_label("Review and save…").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(5, &["+ node22"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        // Reviewed, and the engine could not prepare it.
        h.get_by_label("Review and save…").click();
        h.run();
        engine
            .events
            .send(Event::PlanFailed("the file changed".into()))
            .unwrap();
        h.run();
        drain(&engine);
        apply_another_change(&mut h, &engine, 6);
        assert!(drain(&engine).is_empty());
        assert_eq!(form(&mut h).command, "node22");
    }

    /// `change`, asked for from the toolbar or a dialog while the form is open, applied with `after` as the list
    /// the engine sends first.
    fn apply_while_editing(
        h: &mut Harness<'static, ShellApp>,
        engine: &klefki_core::api::Detached,
        change: Change,
        after: Vec<ServerRow>,
    ) {
        h.state_mut().prepare(vec![change]);
        h.run();
        drain(engine);
        engine
            .events
            .send(Event::PlanReady(plan_view(4, &["+ alpha"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(drain(engine), vec![Command::Apply(4)]);
        engine.events.send(snapshot_of(after)).unwrap();
        engine
            .events
            .send(Event::Applied {
                plan: 4,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
    }

    fn editing(h: &Harness<'static, ShellApp>) -> Option<ServerId> {
        h.state().editor.as_ref().map(|e| e.id.clone())
    }

    #[test]
    fn a_draft_not_yet_saved_follows_its_server_when_it_is_moved() {
        // Found in the UI review: a command edited, then the same server moved, and the edit was gone.
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        // Somewhere else in the list once moved, after a server that was not there before.
        let moved = live_row("alpha", Scope::Local, Some("C:/Repos/WebApp"));
        let before = live_row("aaa", Scope::Local, Some("C:/Repos/WebApp"));
        apply_while_editing(
            &mut h,
            &engine,
            Change::Move {
                id: a.id(),
                to: Target::Local("C:/Repos/WebApp".into()),
            },
            vec![before, moved.clone()],
        );
        // Read again from the new place, to compare the draft with and to revert to.
        assert_eq!(drain(&engine), vec![Command::Inspect(moved.id())]);
        assert_eq!(editing(&h), Some(moved.id()));
        assert_eq!(h.state().selected, BTreeSet::from([1]));
        assert_eq!(form(&mut h).command, "node22");
        h.get_by_label("Unsaved");
        engine
            .events
            .send(entry_event(&moved, FORM_ORIGINAL, true))
            .unwrap();
        h.run();
        assert_eq!(form(&mut h).command, "node22");
        h.get_by_label("Unsaved");
    }

    #[test]
    fn a_draft_that_follows_a_move_is_compared_with_what_the_new_place_holds() {
        // From the review of #16: a move through Claude Code can leave settings out, and the draft was still
        // compared with, reverted to and saved over the old place's definition.
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        let moved = live_row("alpha", Scope::Local, Some("C:/Repos/WebApp"));
        apply_while_editing(
            &mut h,
            &engine,
            Change::Move {
                id: a.id(),
                to: Target::Local("C:/Repos/WebApp".into()),
            },
            vec![moved.clone()],
        );
        assert_eq!(drain(&engine), vec![Command::Inspect(moved.id())]);
        let without_cwd = FORM_ORIGINAL.replace("\"cwd\": \"/w\",\n  ", "");
        assert!(!without_cwd.contains("cwd"));
        engine
            .events
            .send(entry_event(&moved, &without_cwd, true))
            .unwrap();
        h.run();
        // The edit is kept; what the move left out is not brought back as if it were an edit.
        assert_eq!(form(&mut h).command, "node22");
        assert!(!json_text(&h).contains("cwd"), "{}", json_text(&h));
        h.state_mut().outcome = None;
        h.run();
        h.get_by_label("Review and save…").click();
        h.run();
        match &drain(&engine)[..] {
            [Command::Prepare(changes)] => match &changes[..] {
                [Change::Replace { id, json }] => {
                    assert_eq!(*id, moved.id());
                    assert_eq!(*json, without_cwd.replace("\"node\"", "\"node22\""));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        // Revert goes back to what the new place holds.
        h.state_mut().plan = None;
        h.state_mut().preparing = false;
        h.run();
        h.get_by_label("Revert").click();
        h.run();
        assert_eq!(json_text(&h), without_cwd);
        assert!(h.query_by_label("Unsaved").is_none());
    }

    #[test]
    fn a_draft_not_yet_saved_follows_its_server_when_it_is_renamed() {
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        let renamed = live_row("Alpha", Scope::User, None);
        apply_while_editing(
            &mut h,
            &engine,
            Change::Rename {
                id: a.id(),
                name: "Alpha".into(),
            },
            vec![live_row("Aaa", Scope::User, None), renamed.clone()],
        );
        assert_eq!(drain(&engine), vec![Command::Inspect(renamed.id())]);
        assert_eq!(editing(&h), Some(renamed.id()));
        assert_eq!(h.state().selected, BTreeSet::from([1]));
        assert_eq!(form(&mut h).command, "node22");
    }

    #[test]
    fn a_server_renamed_under_a_draft_is_brought_into_view() {
        // From the review of #16: a move brought its row into view, a rename did not.
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        let mut after: Vec<ServerRow> = (0..80)
            .map(|i| live_row(&format!("a{i:02}"), Scope::User, None))
            .collect();
        after.push(live_row("Alpha", Scope::User, None));
        apply_while_editing(
            &mut h,
            &engine,
            Change::Rename {
                id: a.id(),
                name: "Alpha".into(),
            },
            after,
        );
        assert_eq!(h.state().selected, BTreeSet::from([80]));
        h.run();
        assert!(
            h.state().on_screen.contains(&80),
            "{:?}",
            h.state().on_screen
        );
    }

    #[test]
    fn the_move_and_casing_dialogs_say_an_unsaved_edit_goes_along() {
        // From the review of #16: said before the change is confirmed, not only seen afterwards.
        let (mut h, _engine, _) = form_harness(FORM_ORIGINAL, true);
        h.get_by_label(icon::ARROW_FAT_LINES_RIGHT).click();
        h.run();
        assert!(h.query_by_label(GOES_ALONG).is_none(), "nothing edited");
        h.key_press(egui::Key::Escape);
        h.run();
        form(&mut h).command = "node22".into();
        h.run();
        h.get_by_label(icon::ARROW_FAT_LINES_RIGHT).click();
        h.run();
        h.get_by_label(GOES_ALONG);

        let (mut h, engine) = casing_harness(vec![]);
        let local = h.state().snapshot.servers[1].clone();
        press_edit(&mut h, "gateway");
        engine
            .events
            .send(entry_event(&local, FORM_ORIGINAL, true))
            .unwrap();
        h.run();
        form(&mut h).command = "node22".into();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        // Kept as it is spelled, the edited server is not renamed: nothing goes anywhere.
        h.get_by_label("Keep gateway").click();
        h.run();
        assert!(h.query_by_label(GOES_ALONG).is_none());
        h.get_by_label("Keep Gateway").click();
        h.run();
        h.get_by_label(GOES_ALONG);
    }

    #[test]
    fn an_unedited_definition_is_read_again_from_where_its_server_was_moved() {
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        let moved = live_row("alpha", Scope::Local, Some("C:/Repos/WebApp"));
        apply_while_editing(
            &mut h,
            &engine,
            Change::Move {
                id: a.id(),
                to: Target::Local("C:/Repos/WebApp".into()),
            },
            vec![moved.clone()],
        );
        assert_eq!(drain(&engine), vec![Command::Inspect(moved.id())]);
        assert_eq!(editing(&h), Some(moved.id()));
    }

    #[test]
    fn a_draft_stays_with_its_server_when_another_one_is_renamed() {
        let (mut h, engine, a) = form_harness(FORM_ORIGINAL, true);
        form(&mut h).command = "node22".into();
        h.run();
        let other = live_row("beta", Scope::User, None);
        apply_while_editing(
            &mut h,
            &engine,
            Change::Rename {
                id: other.id(),
                name: "Beta".into(),
            },
            vec![a.clone(), live_row("Beta", Scope::User, None)],
        );
        assert!(drain(&engine).is_empty());
        assert_eq!(editing(&h), Some(a.id()));
        assert_eq!(form(&mut h).command, "node22");
    }

    #[test]
    fn every_toolbar_button_stays_reachable_next_to_the_detail_panel() {
        // Found while testing Delete: with a row selected the detail panel took the toolbar's right half, and the
        // buttons past its edge were cut off and could not be clicked.
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine.events.send(snapshot_of(vec![a])).unwrap();
        h.run();
        h.get_by_label("alpha").click();
        h.run();
        let detail_left = h.get_by_label("General").rect().min.x;
        // (Add and Import were in this list; they are no longer shown until they work.)
        for glyph in [
            icon::PENCIL_SIMPLE,
            icon::COPY,
            icon::ARROW_FAT_LINES_RIGHT,
            icon::TOGGLE_RIGHT,
            icon::TOGGLE_LEFT,
            icon::TRASH,
        ] {
            let r = h.get_by_label(glyph).rect();
            assert!(
                r.max.x <= detail_left,
                "{glyph:?} at {r:?}, detail panel from x={detail_left}"
            );
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

    #[test]
    fn duplicate_suggests_a_name_and_the_same_place() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("beta").click();
        h.run();
        h.get_by_label(icon::COPY).click();
        h.run();
        h.get_by_label_contains("Duplicate beta");
        assert_eq!(h.state().duplicate.as_ref().unwrap().name, "beta_copy");
        h.get_by_label("Review").click();
        h.run();
        assert!(h.state().duplicate.is_none());
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: h.state().snapshot.servers[1].id(),
                name: "beta_copy".into(),
                targets: vec![Target::Local("C:/Repos/WebApp".into())],
                adjust: Default::default(),
            }]
        );
    }

    const SSO_DEFINITION: &str = r#"{
  "type": "http",
  "url": "https://mcp.acme.com/v1/mcp?tenant=7",
  "headers": {"X-Team": "<hidden>"},
  "oauth": {"clientId": "acme-1", "callbackPort": 8123, "authServerMetadataUrl": "https://mcp.acme.com/v1/.well-known/oauth-authorization-server"}
}"#;

    /// The Duplicate dialog for a user server that signs in, its definition read.
    fn duplicate_sso() -> (
        Harness<'static, ShellApp>,
        klefki_core::api::Detached,
        ServerRow,
    ) {
        let (mut h, engine) = live_harness();
        let mut sso = live_row("sso", Scope::User, None);
        sso.transport = TransportKind::Http;
        sso.target = "https://mcp.acme.com/v1/mcp".into();
        sso.oauth = Some(klefki_core::api::OAuthInfo {
            client_id: Some("acme-1".into()),
            scopes: Vec::new(),
        });
        engine.events.send(snapshot_of(vec![sso.clone()])).unwrap();
        h.run();
        h.get_by_label("sso").click();
        h.run();
        drain(&engine);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        // Its address is read from the file (the list leaves out the query); Review waits for it.
        assert_eq!(drain(&engine), vec![Command::Inspect(sso.id())]);
        h.get_by_label_contains("Reading its address");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        engine
            .events
            .send(entry_event(&sso, SSO_DEFINITION, true))
            .unwrap();
        h.run();
        (h, engine, sso)
    }

    fn copy_fields_of<'a>(h: &'a mut Harness<'static, ShellApp>) -> &'a mut CopyFields {
        match &mut h.state_mut().duplicate.as_mut().unwrap().copy {
            CopyRead::Ready(fields) => fields,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn duplicate_changes_the_base_address_port_and_client_of_a_server_that_signs_in() {
        let (mut h, engine, sso) = duplicate_sso();
        // Filled in from the original, with what the base makes of each address.
        h.get_by_label("Base address");
        h.get_by_label("Client ID");
        h.get_by_label("Callback port");
        assert!(h.query_by_label("Client secret").is_none());
        h.get_by_label_contains("Server: https://mcp.acme.com/v1/mcp?tenant=7");
        h.get_by_label_contains("its client secret");
        let f = copy_fields_of(&mut h);
        f.base = "https://eu.acme.com/v2".into();
        f.callback_port = "9001".into();
        f.client_id = "acme-2".into();
        h.run();
        h.get_by_label_contains("Server: https://eu.acme.com/v2/mcp?tenant=7");
        h.get_by_label_contains(
            "Sign-in metadata: https://eu.acme.com/v2/.well-known/oauth-authorization-server",
        );
        // Another client: its own secret, asked for here.
        h.get_by_label("Client secret");
        h.get_by_label_contains("signs in as the client above");
        copy_fields_of(&mut h).secret.edit().push_str("new-s3cret");
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: sso.id(),
                name: "sso_copy".into(),
                targets: vec![Target::User],
                adjust: klefki_core::api::Adjust {
                    base: Some(klefki_core::api::Rebase {
                        from: "https://mcp.acme.com/v1".into(),
                        to: "https://eu.acme.com/v2".into(),
                    }),
                    callback_port: Some(Some(9001)),
                    client: Some(klefki_core::api::Client {
                        id: Some("acme-2".into()),
                        secret: Some(Secret::new("new-s3cret".into())),
                    }),
                },
            }]
        );
    }

    #[test]
    fn duplicate_says_what_is_wrong_with_a_change_and_waits_for_it() {
        let (mut h, engine, sso) = duplicate_sso();
        let focus = |h: &mut Harness<'static, ShellApp>, id: &str| {
            h.ctx.memory_mut(|m| m.request_focus(egui::Id::new(id)));
            h.run();
        };
        focus(&mut h, "copy_callback_port");
        copy_fields_of(&mut h).callback_port = "70000".into();
        h.run();
        // Still typing: not said yet, but Review waits.
        assert!(h.query_by_label_contains("from 1 to 65535").is_none());
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.key_press(egui::Key::Tab);
        h.run();
        h.get_by_label_contains("from 1 to 65535");
        // Said as it is from then on: fixed, it goes.
        copy_fields_of(&mut h).callback_port = "8123".into();
        h.run();
        assert!(h.query_by_label_contains("from 1 to 65535").is_none());
        focus(&mut h, "copy_base");
        copy_fields_of(&mut h).base = String::new();
        h.run();
        h.key_press(egui::Key::Tab);
        h.run();
        h.get_by_label_contains("Type the address");
        // The addresses stay in view.
        h.get_by_label_contains("Sign-in metadata:");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        // Typed without its scheme, the base gets one when left.
        let base = egui::Id::new("copy_base");
        h.ctx.memory_mut(|m| m.request_focus(base));
        h.run();
        copy_fields_of(&mut h).base = "mcp.acme.com/v1".into();
        h.run();
        h.key_press(egui::Key::Tab);
        h.run();
        assert_eq!(copy_fields_of(&mut h).base, "https://mcp.acme.com/v1");
        // Untouched, the copy changes nothing.
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: sso.id(),
                name: "sso_copy".into(),
                targets: vec![Target::User],
                adjust: Default::default(),
            }]
        );
    }

    #[test]
    fn duplicate_warns_about_a_callback_port_another_program_holds_once_left() {
        let (mut h, engine, _) = duplicate_sso();
        let port = egui::Id::new("copy_callback_port");
        h.ctx.memory_mut(|m| m.request_focus(port));
        h.run();
        copy_fields_of(&mut h).callback_port = "8766".into();
        h.run();
        assert!(drain(&engine).is_empty(), "not while it is typed");
        h.key_press(egui::Key::Tab);
        h.run();
        assert_eq!(drain(&engine), vec![Command::CheckCallbackPort(8766)]);
        engine
            .events
            .send(Event::CallbackPort {
                port: 8766,
                warning: Some("Port 8766 is in use by another program.".into()),
            })
            .unwrap();
        h.run();
        h.get_by_label_contains("Port 8766 is in use");
        // A warning, not a stop.
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
        copy_fields_of(&mut h).callback_port = "8767".into();
        h.run();
        assert!(h.query_by_label_contains("Port 8766 is in use").is_none());
    }

    #[test]
    fn duplicate_of_a_server_whose_definition_cannot_be_read_copies_it_as_it_is() {
        let (mut h, engine) = live_harness();
        let mut web = live_row("web", Scope::User, None);
        web.transport = TransportKind::Http;
        web.target = "https://h.example/mcp".into();
        engine.events.send(snapshot_of(vec![web.clone()])).unwrap();
        h.run();
        h.get_by_label("web").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        engine
            .events
            .send(Event::Entry {
                id: web.id(),
                json: Err("the server is no longer in the configuration".into()),
                editable: true,
            })
            .unwrap();
        h.run();
        h.get_by_label_contains("can't be changed here");
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
        // A plain address: one field, the whole of it, and nothing about signing in.
        let (mut h, engine) = live_harness();
        engine.events.send(snapshot_of(vec![web.clone()])).unwrap();
        h.run();
        h.get_by_label("web").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        engine
            .events
            .send(entry_event(
                &web,
                r#"{"type": "http", "url": "https://h.example/mcp"}"#,
                true,
            ))
            .unwrap();
        h.run();
        h.get_by_label("Address");
        assert!(h.query_by_label("Client ID").is_none());
        assert!(h.query_by_label_contains("Server:").is_none());
    }

    #[test]
    fn duplicate_goes_wherever_it_is_ticked_under_the_name_typed() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        // User is ticked for a user server; add both projects, untick the user.
        h.get_by_label_contains("For the user").click();
        h.get_by_label("WebApp").click();
        h.get_by_label("notes").click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().name = "alpha-2".into();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: h.state().snapshot.servers[0].id(),
                name: "alpha-2".into(),
                targets: vec![
                    Target::Local("C:/Repos/WebApp".into()),
                    Target::Local("/home/me/notes".into()),
                ],
                adjust: Default::default(),
            }]
        );
    }

    #[test]
    fn duplicate_needs_a_usable_name_and_a_place() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::COPY).click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().name = "has space".into();
        h.run();
        h.get_by_label_contains("not allowed");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.state_mut().duplicate.as_mut().unwrap().name = "fine".into();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
        h.get_by_label_contains("For the user").click();
        h.run();
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.key_press(egui::Key::Enter);
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().duplicate.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn duplicate_waits_while_another_plan_is_on_its_way() {
        // One plan at a time: a Disable not yet answered keeps Duplicate from asking for a second.
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        h.get_by_label(icon::TOGGLE_LEFT).click();
        h.run();
        assert!(h.get_by_label(icon::COPY).accesskit_node().is_disabled());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_none());
        assert_eq!(drain(&engine).len(), 1, "only the Disable");
    }

    #[test]
    fn a_project_server_cannot_be_duplicated_yet() {
        let (mut h, _engine) = duplicate_harness();
        h.get_by_label("gamma").click();
        h.run();
        assert!(h.get_by_label(icon::COPY).accesskit_node().is_disabled());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_none());
    }

    // ---- the old switch-off list

    fn legacy(
        project: &str,
        names: &[&str],
        turns_off: &[&str],
        malformed: bool,
    ) -> LegacyDisabled {
        LegacyDisabled {
            project: project.to_owned(),
            shown: project.to_owned(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            turns_off: turns_off.iter().map(|n| (*n).to_owned()).collect(),
            malformed,
        }
    }

    fn legacy_harness(
        lists: Vec<LegacyDisabled>,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![live_row("gw", Scope::User, None)],
                projects: lists.iter().map(|l| l.project.clone()).collect(),
                legacy_disabled: lists,
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    #[test]
    fn no_old_list_shows_no_notice() {
        let (h, _engine) = legacy_harness(vec![]);
        assert!(h.query_by_label_contains("old switch-off list").is_none());
        assert!(h.query_by_label("Migrate…").is_none());
    }

    #[test]
    fn the_old_list_is_shown_with_what_is_on_because_of_it() {
        let (h, _engine) = legacy_harness(vec![
            legacy("C:/Repos/WebApp", &["gw", "old"], &["gw"], false),
            legacy("/home/x/api", &["a"], &[], false),
            legacy("/srv/empty", &[], &[], false),
        ]);
        h.get_by_label_contains("Old switch-off lists in 3 projects");
        h.get_by_label_contains("empty: an empty list");
        h.get_by_label_contains("Claude Code no longer reads");
        h.get_by_label_contains("WebApp: gw, old (gw is on)");
        h.get_by_label_contains("api: a (already off)");
    }

    #[test]
    fn migrating_asks_for_one_plan_covering_every_list_that_can_move() {
        let (mut h, engine) = legacy_harness(vec![
            legacy("C:/Repos/WebApp", &["gw", "x"], &["gw", "x"], false),
            legacy("/bad", &["a"], &["a"], true),
            legacy("/home/x/api", &["a"], &[], false),
        ]);
        h.get_by_label_contains("bad: holds something other than server names");
        h.get_by_label("Migrate…").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert_eq!(
            changes,
            vec![
                Change::MigrateLegacyDisabled {
                    project: "C:/Repos/WebApp".into()
                },
                Change::MigrateLegacyDisabled {
                    project: "/home/x/api".into()
                },
            ]
        );
    }

    #[test]
    fn the_notice_is_built_once_per_snapshot() {
        // Review of #16: its lines were formatted every frame.
        let (mut h, engine) = legacy_harness(vec![legacy("/p", &["gw"], &["gw"], false)]);
        let first = h.state().legacy_cache.as_ref().unwrap().1.clone();
        h.run();
        assert!(Arc::ptr_eq(
            &first,
            &h.state().legacy_cache.as_ref().unwrap().1
        ));
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                legacy_disabled: vec![legacy("/q", &["x"], &[], false)],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        let now = &h.state().legacy_cache.as_ref().unwrap().1;
        assert!(!Arc::ptr_eq(&first, now));
        h.get_by_label_contains("q: x (already off)");
    }

    #[test]
    fn only_malformed_lists_cannot_be_migrated() {
        let (h, _engine) = legacy_harness(vec![legacy("/bad", &["a"], &["a"], true)]);
        assert!(h.get_by_label("Migrate…").accesskit_node().is_disabled());
    }

    #[test]
    fn the_palette_offers_the_migration() {
        let (mut h, engine) = legacy_harness(vec![legacy("/p", &["gw"], &["gw"], false)]);
        let titles: Vec<String> = h.state().commands().into_iter().map(|e| e.title).collect();
        assert!(
            titles.iter().any(|t| t == "Migrate old switch-off lists"),
            "{titles:?}"
        );
        h.state_mut()
            .run(&egui::Context::default(), command::Action::MigrateLegacy);
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::MigrateLegacyDisabled {
                project: "/p".into()
            }]
        );
        h.run();
    }

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

    fn renames(engine: &klefki_core::api::Detached) -> Vec<(ServerId, String)> {
        duplicate_requested(engine)
            .into_iter()
            .map(|c| match c {
                Change::Rename { id, name } => (id, name),
                other => panic!("not a rename: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn only_a_server_with_a_casing_collision_offers_the_fix() {
        let (mut h, _engine) = casing_harness(vec![]);
        h.get_by_label("other").click();
        h.run();
        assert!(h.query_by_label("Fix casing…").is_none());
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…");
    }

    #[test]
    fn the_fix_renames_the_others_to_the_spelling_chosen() {
        let (mut h, engine) = casing_harness(vec![]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        // Both spellings are offered; the selected server's own is the starting point.
        h.get_by_label("Keep gateway");
        h.get_by_label("Keep Gateway");
        assert_eq!(h.state().casing.as_ref().unwrap().keep, "gateway");
        h.get_by_label("Review").click();
        h.run();
        assert!(h.state().casing.is_none());
        let user = h.state().snapshot.servers[0].id();
        assert_eq!(renames(&engine), vec![(user, "gateway".to_owned())]);
    }

    #[test]
    fn choosing_the_other_spelling_renames_the_other_server() {
        let (mut h, engine) = casing_harness(vec![]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        h.get_by_label("Keep Gateway").click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let local = h.state().snapshot.servers[1].id();
        assert_eq!(renames(&engine), vec![(local, "Gateway".to_owned())]);
    }

    #[test]
    fn a_project_server_cannot_be_renamed_so_its_spelling_is_the_one_kept() {
        let mut project = live_row("GATEWAY", Scope::Project, Some("C:/Repos/WebApp"));
        project.casing_collision = true;
        project.approval = Some(Approval::Approved);
        let (mut h, engine) = casing_harness(vec![project]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        assert_eq!(h.state().casing.as_ref().unwrap().keep, "GATEWAY");
        assert!(
            h.get_by_label("Keep gateway")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("Review").click();
        h.run();
        let ids: Vec<ServerId> = renames(&engine).into_iter().map(|(id, _)| id).collect();
        let rows = &h.state().snapshot.servers;
        assert_eq!(ids, vec![rows[0].id(), rows[1].id()]);
    }

    #[test]
    fn two_project_servers_that_differ_cannot_be_fixed_from_here() {
        let mut one = live_row("GATEWAY", Scope::Project, Some("C:/Repos/WebApp"));
        let mut two = live_row("Gateway", Scope::Project, Some("C:/Repos/Other"));
        one.casing_collision = true;
        two.casing_collision = true;
        let (mut h, engine) = casing_harness(vec![one, two]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        h.get_by_label_contains(".mcp.json");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().casing.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn cancelling_the_fix_asks_for_nothing() {
        let (mut h, engine) = casing_harness(vec![]);
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.state().casing.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    // ---- add

    fn paste(h: &mut Harness<'static, ShellApp>, text: &str) {
        h.state_mut().add.as_mut().unwrap().text = text.to_owned();
        h.run();
    }

    fn pasted_json(text: &str, i: usize) -> String {
        klefki_core::api::Pasted::parse(text).unwrap().servers[i]
            .json
            .clone()
    }

    // A click on the dimmed window beside a dialog is easily a slip (bringing Klefki back to the front, say), so it
    // never throws away what was pasted, typed or chosen. Esc and Cancel close on purpose.

    #[test]
    fn a_click_beside_a_dialog_flashes_its_outline() {
        // The click beside does nothing to the dialog, so it shows that the dialog is still waiting for Esc or
        // Cancel, instead of seeming not to respond.
        let (mut h, _engine) = duplicate_harness();
        open_duplicate_of(&mut h, "beta");
        let id = egui::Id::new("duplicate_dialog");
        assert!(!nudged(&h.ctx, id));
        h.get_by_label_contains("Duplicate beta").click();
        h.run();
        assert!(!nudged(&h.ctx, id), "a click inside is no slip");
        click_beside(&mut h);
        assert!(nudged(&h.ctx, id));
        assert!(h.state().duplicate.is_some());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().duplicate.is_none());
    }

    #[test]
    fn a_click_beside_the_add_dialog_keeps_what_was_pasted() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label(icon::PLUS).click();
        h.run();
        let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
        paste(&mut h, text);
        click_beside(&mut h);
        assert_eq!(h.state().add.as_ref().map(|f| f.text.as_str()), Some(text));
        h.get_by_label_contains("Adds files for the user.");
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label("Discard").click();
        h.run();
        assert!(h.state().add.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn a_click_beside_the_review_of_an_add_keeps_the_review_and_the_add_dialog() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label(icon::PLUS).click();
        h.run();
        let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
        paste(&mut h, text);
        h.get_by_label("Review").click();
        h.run();
        assert!(waits_behind_review(&h));
        drain(&engine);
        let mut plan = plan_view(9, &["+   \"files\""], &[], false);
        plan.action = "Add".to_owned();
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        click_beside(&mut h);
        assert!(drain(&engine).is_empty(), "the plan is not discarded");
        assert!(h.state().plan.is_some());
        assert_eq!(h.state().add.as_ref().map(|f| f.text.as_str()), Some(text));
    }

    #[test]
    fn a_click_beside_a_plan_leaves_it_to_apply_or_cancel() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        click_beside(&mut h);
        assert!(drain(&engine).is_empty());
        h.get_by_label_contains("Review the change");
        h.get_by_label("Cancel").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Discard(7)]);
    }

    #[test]
    fn a_click_beside_the_casing_dialog_keeps_it_open() {
        let (mut h, engine) = casing_harness(Vec::new());
        h.get_by_label("gateway").click();
        h.run();
        h.get_by_label("Fix casing…").click();
        h.run();
        click_beside(&mut h);
        assert!(h.state().casing.is_some());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().casing.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn a_click_beside_the_duplicate_dialog_keeps_the_name_typed() {
        let (mut h, engine) = duplicate_harness();
        open_duplicate_of(&mut h, "beta");
        h.state_mut().duplicate.as_mut().unwrap().name = "beta_two".into();
        h.run();
        click_beside(&mut h);
        assert_eq!(
            h.state().duplicate.as_ref().map(|f| f.name.as_str()),
            Some("beta_two")
        );
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().duplicate.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn a_click_beside_the_move_dialog_keeps_the_place_chosen() {
        let (mut h, engine) = duplicate_harness();
        open_move_of(&mut h, "beta");
        h.get_by_label("For the user, in every project").click();
        h.run();
        click_beside(&mut h);
        assert!(h.state().moving.is_some());
        h.get_by_label("Moves beta to the user.");
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.state().moving.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn add_reads_a_pasted_command_and_adds_it_where_it_asks() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label(icon::PLUS).click();
        h.run();
        h.get_by_label_contains("Add servers");
        assert!(!ready(&mut h));
        let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
        paste(&mut h, text);
        h.get_by_label_contains("npx -y @x/fs /tmp");
        h.get_by_label_contains("Adds files for the user.");
        h.get_by_label("Review").click();
        h.run();
        assert!(waits_behind_review(&h));
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Add {
                name: "files".into(),
                json: pasted_json(text, 0),
                targets: vec![Target::User],
                secret: None,
            }]
        );
    }

    #[test]
    fn many_servers_are_counted_not_listed() {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let text: String = (0..5)
            .map(|i| format!("claude mcp add -s user s{i} node\n"))
            .collect();
        paste(&mut h, &text);
        h.get_by_label_contains("Adds 5 servers for the user.");
    }

    #[test]
    fn a_local_command_goes_to_the_selected_servers_project() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("beta").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let text = "claude mcp add one node\nclaude mcp add two -- node two.js";
        paste(&mut h, text);
        h.get_by_label_contains("Adds one and two in WebApp.");
        // Ticking a place by hand is kept when the text changes.
        h.get_by_label("notes").click();
        h.run();
        paste(&mut h, &format!("{text}\n"));
        h.get_by_label_contains("Adds one and two in WebApp and notes.");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.run();
        let targets = vec![
            Target::Local("C:/Repos/WebApp".into()),
            Target::Local("/home/me/notes".into()),
        ];
        assert_eq!(
            duplicate_requested(&engine),
            vec![
                Change::Add {
                    name: "one".into(),
                    json: pasted_json(text, 0),
                    targets: targets.clone(),
                    secret: None,
                },
                Change::Add {
                    name: "two".into(),
                    json: pasted_json(text, 1),
                    targets,
                    secret: None,
                },
            ]
        );
    }

    #[test]
    fn add_explains_what_it_cannot_add_and_needs_usable_names() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let text = "claude mcp add --client-id x corp node\nclaude mcp add -s user ok node\nclaude mcp add -s user ok node";
        paste(&mut h, text);
        h.get_by_label_contains("OAuth");
        h.get_by_label_contains("twice");
        assert!(!ready(&mut h));
        h.state_mut().add.as_mut().unwrap().names[2] = "ok2".into();
        h.run();
        assert!(ready(&mut h));
        h.get_by_label_contains("1 server cannot be added");
        // A name already used for the user, and one that is not a name.
        h.state_mut().add.as_mut().unwrap().names[1] = "alpha".into();
        h.run();
        h.get_by_label_contains("alpha already exists for the user");
        assert!(!ready(&mut h));
        h.state_mut().add.as_mut().unwrap().names[1] = "has space".into();
        h.run();
        h.get_by_label_contains("not allowed");
        assert!(!ready(&mut h));
        h.state_mut().add.as_mut().unwrap().names[1] = "ok".into();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let names: Vec<String> = duplicate_requested(&engine)
            .into_iter()
            .map(|c| match c {
                Change::Add { name, .. } => name,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["ok", "ok2"]);
    }

    #[test]
    fn a_definition_without_a_name_needs_one_and_nowhere_is_not_a_place() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(
            &mut h,
            r#"{"type": "http", "url": "https://x.example/mcp"}"#,
        );
        h.get_by_label_contains("http · https://x.example/mcp");
        h.get_by_label_contains("Give it a name");
        assert!(!ready(&mut h));
        h.state_mut().add.as_mut().unwrap().names[0] = "x".into();
        h.run();
        assert!(ready(&mut h));
        h.get_by_label_contains("For the user").click();
        h.run();
        h.get_by_label_contains("Tick at least one place");
        assert!(!ready(&mut h));
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn add_hints_at_secrets_and_says_how_it_read_the_text() {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(
            &mut h,
            "claude mcp add -e GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789 -e B=$HOME gh node",
        );
        h.get_by_label_contains("GITHUB_TOKEN looks like a secret");
        h.get_by_label_contains("Claude Code fills it in");
        assert!(h.query_by_label_contains("ghp_abc").is_none());
        paste(&mut h, "npm install thing");
        h.get_by_label_contains("Nothing here adds a server");
        assert!(!ready(&mut h));
    }

    #[test]
    fn add_is_in_the_palette_and_esc_closes_it_without_adding_anything() {
        let (mut h, engine) = duplicate_harness();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "add a server");
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.state().add.is_some());
        paste(&mut h, "claude mcp add x node");
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label("Discard").click();
        h.run();
        assert!(h.state().add.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    // Esc is a reflex, pressed to leave a field or a menu as often as a dialog, so it never throws away what was
    // pasted or typed without asking. Cancel is clicked on purpose and closes at once.
    const DISCARD_QUESTION: &str = "Discard what you typed?";

    #[test]
    fn esc_closes_an_empty_add_dialog_at_once() {
        for mode in [AddMode::Paste, AddMode::Fill] {
            let (mut h, _engine) = duplicate_harness();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
            h.run();
            add_form(&mut h).mode = mode;
            // Choosing how a server connects, or where it goes, is not typing: there is nothing to lose. Nor is
            // ticking OAuth, or adding a row and leaving it empty.
            let f = add_form(&mut h);
            f.fields.transport = TransportKind::Http;
            f.fields.oauth = true;
            f.fields.headers.push((String::new(), " ".into()));
            f.fields.env.push((String::new(), String::new()));
            h.run();
            h.key_press(egui::Key::Escape);
            h.run();
            assert!(h.state().add.is_none(), "{mode:?}");
        }
    }

    #[test]
    fn esc_asks_before_throwing_away_what_was_pasted_or_typed() {
        type Typing = fn(&mut AddForm);
        let typed: [(&str, Typing); 5] = [
            ("pasted", |f| f.text = "claude mcp add x node".into()),
            ("name", |f| {
                f.mode = AddMode::Fill;
                f.name = "files".into();
                f.name_edited = true;
            }),
            ("command", |f| {
                f.mode = AddMode::Fill;
                f.fields.command = "npx -y @x/fs".into();
            }),
            ("url", |f| {
                f.mode = AddMode::Fill;
                f.fields.transport = TransportKind::Http;
                f.fields.url = "https://example.com/mcp".into();
            }),
            ("secret", |f| {
                f.mode = AddMode::Fill;
                *f.secret.edit() = "s3cret".into();
            }),
        ];
        for (what, fill) in typed {
            let (mut h, engine) = duplicate_harness();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
            h.run();
            fill(add_form(&mut h));
            h.run();
            assert!(h.query_by_label(DISCARD_QUESTION).is_none(), "{what}");
            h.key_press(egui::Key::Escape);
            h.run();
            assert!(h.state().add.is_some(), "{what}: closed without asking");
            h.get_by_label(DISCARD_QUESTION);
            // Esc again backs out of the question, not out of the dialog.
            h.key_press(egui::Key::Escape);
            h.run();
            assert!(h.state().add.is_some(), "{what}");
            assert!(h.query_by_label(DISCARD_QUESTION).is_none(), "{what}");
            h.key_press(egui::Key::Escape);
            h.run();
            h.get_by_label("Keep editing").click();
            h.run();
            assert!(h.state().add.is_some(), "{what}");
            assert!(h.query_by_label(DISCARD_QUESTION).is_none(), "{what}");
            h.key_press(egui::Key::Escape);
            h.run();
            h.get_by_label("Discard").click();
            h.run();
            assert!(h.state().add.is_none(), "{what}");
            assert!(duplicate_requested(&engine).is_empty(), "{what}");
        }
    }

    #[test]
    fn the_discard_question_works_from_the_keyboard() {
        // From the review of #13: after Esc nothing had the keyboard, and Tab went into the paste box, which kept
        // it, so Discard and Keep editing could not be reached without a mouse.
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let text = "claude mcp add x node";
        paste(&mut h, text);
        h.run();
        let typing_in = h.ctx.memory(|m| m.focused());
        assert!(typing_in.is_some(), "the paste box has the keyboard");
        h.key_press(egui::Key::Escape);
        h.run();
        h.run();
        // The least destructive answer has the keyboard: Enter keeps editing.
        assert!(h.get_by_label("Keep editing").is_focused());
        h.key_press(egui::Key::Enter);
        h.run();
        h.run();
        assert!(h.query_by_label(DISCARD_QUESTION).is_none());
        assert_eq!(
            h.ctx.memory(|m| m.focused()),
            typing_in,
            "back where it was"
        );
        // One Tab back from Keep editing reaches Discard; Enter on it closes.
        h.key_press(egui::Key::Escape);
        h.run();
        h.run();
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
        h.run();
        assert!(h.get_by_label("Discard").is_focused());
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.state().add.is_none());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn going_back_to_typing_puts_the_discard_question_away() {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(&mut h, "claude mcp add x node");
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label(DISCARD_QUESTION);
        h.event(egui::Event::Text("y".into()));
        h.run();
        assert!(h.query_by_label(DISCARD_QUESTION).is_none());
        h.get_by_label("Review");
        assert!(h.state().add.is_some());
    }

    #[test]
    fn cancel_closes_a_filled_in_add_dialog_at_once() {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(&mut h, "claude mcp add x node");
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.state().add.is_none());
    }

    #[test]
    fn esc_in_the_review_of_an_add_goes_back_to_the_dialog() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let text = "claude mcp add -s user files -- npx -y @x/fs /tmp";
        paste(&mut h, text);
        h.get_by_label("Review").click();
        h.run();
        assert!(waits_behind_review(&h));
        assert_eq!(duplicate_requested(&engine).len(), 1);
        let mut plan = plan_view(9, &["+   \"files\""], &[], false);
        plan.action = "Add".to_owned();
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label(&format!("{} Back", icon::ARROW_LEFT));
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().plan.is_none());
        assert_eq!(drain(&engine), vec![Command::Discard(9)]);
        let form = h.state().add.as_ref().expect("the Add dialog is kept");
        assert!(!form.reviewing);
        assert_eq!(form.text, text);
    }

    fn fill_in(h: &mut Harness<'static, ShellApp>) {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        h.get_by_label("Fill in").click();
        h.run();
        assert_eq!(h.state().add.as_ref().unwrap().mode, AddMode::Fill);
    }

    fn add_form<'a>(h: &'a mut Harness<'static, ShellApp>) -> &'a mut AddForm {
        h.state_mut().add.as_mut().unwrap()
    }

    fn fields_json(fields: &ServerFields) -> String {
        fields.read("x").json
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

    #[test]
    fn a_server_can_be_filled_in_part_by_part() {
        let (mut h, engine) = duplicate_harness();
        fill_in(&mut h);
        h.get_by_label_contains("Add a server");
        assert!(!ready(&mut h));
        // Nothing is marked wrong on a form just opened.
        assert!(h.query_by_label_contains("Give it a name").is_none());
        add_form(&mut h).fields.command = r#"npx -y @x/fs "C:\My files""#.into();
        h.run();
        // The name follows the command until one is typed.
        assert_eq!(add_form(&mut h).name, "fs");
        h.get_by_label_contains("Adds fs for the user.");
        name_it(&mut h, "files");
        add_form(&mut h).fields.command = r#"npx -y @x/fs "C:\My files""#.into();
        h.run();
        h.get_by_label_contains("Add variable").click();
        h.run();
        add_form(&mut h).fields.env[0] = ("ROOT".into(), "/srv".into());
        h.run();
        h.get_by_label_contains("Adds files for the user.");
        h.get_by_label("Review").click();
        h.run();
        assert!(waits_behind_review(&h));
        let mut fields = ServerFields {
            command: r#"npx -y @x/fs "C:\My files""#.into(),
            ..ServerFields::default()
        };
        fields.env.push(("ROOT".into(), "/srv".into()));
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Add {
                name: "files".into(),
                json: fields_json(&fields),
                targets: vec![Target::User],
                secret: None,
            }]
        );
    }

    #[test]
    fn an_oauth_server_is_added_with_its_client_secret_typed_hidden() {
        let (mut h, engine) = duplicate_harness();
        fill_in(&mut h);
        h.get_by_label("HTTP").click();
        h.run();
        assert!(h.query_by_label_contains("Client ID").is_none());
        h.get_by_label(OAUTH_TOGGLE).click();
        h.run();
        h.get_by_label_contains("Client secret");
        name_it(&mut h, "sso");
        let f = add_form(&mut h);
        f.fields.url = "https://sso.example.com/mcp".into();
        f.fields.client_id = "client-7".into();
        f.fields.callback_port = "8123".into();
        *f.secret.edit() = "Typed-S3cret".into();
        h.run();
        // The secret is never drawn.
        assert!(h.query_by_label_contains("Typed-S3cret").is_none());
        h.get_by_label("Review").click();
        h.run();
        let fields = ServerFields {
            transport: TransportKind::Http,
            url: "https://sso.example.com/mcp".into(),
            oauth: true,
            client_id: "client-7".into(),
            callback_port: "8123".into(),
            ..ServerFields::default()
        };
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Add {
                name: "sso".into(),
                json: fields_json(&fields),
                targets: vec![Target::User],
                secret: Some(Secret::new("Typed-S3cret".into())),
            }]
        );
    }

    #[test]
    fn an_empty_client_secret_is_none_and_a_secret_is_only_sent_for_oauth() {
        let (mut h, engine) = duplicate_harness();
        fill_in(&mut h);
        name_it(&mut h, "sso");
        let f = add_form(&mut h);
        f.fields.transport = TransportKind::Http;
        f.fields.url = "https://sso.example.com/mcp".into();
        f.fields.oauth = true;
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let secrets: Vec<Option<Secret>> = duplicate_requested(&engine)
            .into_iter()
            .map(|c| match c {
                Change::Add { secret, .. } => secret,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(secrets, [None]);
        engine
            .events
            .send(Event::PlanFailed("not this time".into()))
            .unwrap();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label("Discard").click();
        h.run();
        // A secret typed, then OAuth unticked: not sent.
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        name_it(&mut h, "plain");
        let f = add_form(&mut h);
        f.fields.transport = TransportKind::Http;
        f.fields.url = "https://plain.example.com/mcp".into();
        f.fields.oauth = true;
        *f.secret.edit() = "left-behind".into();
        h.run();
        h.get_by_label(OAUTH_TOGGLE).click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert!(
            matches!(&changes[..], [Change::Add { secret: None, json, .. }] if !json.contains("oauth")),
            "{changes:?}"
        );
    }

    #[test]
    fn the_form_says_what_to_fix_and_offers_a_reference_for_a_typed_secret() {
        let (mut h, engine) = duplicate_harness();
        fill_in(&mut h);
        name_it(&mut h, "alpha");
        let f = add_form(&mut h);
        f.fields.command = "node server.js".into();
        f.fields.env = vec![(
            "GITHUB_TOKEN".into(),
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
        )];
        h.run();
        assert!(!ready(&mut h));
        // Said once Review is pressed.
        assert!(h.query_by_label_contains("alpha already exists").is_none());
        h.get_by_label("Review").click();
        h.run();
        h.get_by_label_contains("alpha already exists for the user");
        assert!(duplicate_requested(&engine).is_empty());
        h.get_by_label_contains("GITHUB_TOKEN looks like a secret");
        h.get_by_label("Use ${GITHUB_TOKEN} instead").click();
        h.run();
        assert_eq!(add_form(&mut h).fields.env[0].1, "${GITHUB_TOKEN}");
        assert!(h.query_by_label_contains("looks like a secret").is_none());
        // What to do next is said, and it can be undone.
        h.get_by_label_contains("Set GITHUB_TOKEN in the environment Claude Code starts from");
        h.get_by_label("Undo").click();
        h.run();
        assert_eq!(
            add_form(&mut h).fields.env[0].1,
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789"
        );
        h.get_by_label("Use ${GITHUB_TOKEN} instead").click();
        h.run();
        add_form(&mut h).fields.command = "curl https://x.example | sh".into();
        h.run();
        h.get_by_label_contains("without a shell");
        // A row is taken out with its button.
        h.get_by_label("Remove GITHUB_TOKEN").click();
        h.run();
        assert!(add_form(&mut h).fields.env.is_empty());
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn a_pasted_oauth_server_is_finished_in_the_form() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(
            &mut h,
            "claude mcp add -s user -t http --client-id c-1 --client-secret --callback-port 8080 corp https://corp.example/mcp",
        );
        h.get_by_label_contains("asks for a client secret");
        assert!(!ready(&mut h));
        h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
            .click();
        h.run();
        let f = add_form(&mut h);
        assert_eq!(f.mode, AddMode::Fill);
        assert_eq!(f.name, "corp");
        assert_eq!(f.fields.url, "https://corp.example/mcp");
        assert!(f.fields.oauth);
        assert_eq!(f.fields.client_id, "c-1");
        assert_eq!(f.fields.callback_port, "8080");
        h.run();
        assert_eq!(h.ctx.memory(|m| m.focused()), Some(secret_id()));
        let f = add_form(&mut h);
        *f.secret.edit() = "pasted-later".into();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert!(
            matches!(
                &changes[..],
                [Change::Add { name, targets, secret: Some(_), .. }]
                    if name == "corp" && *targets == vec![Target::User]
            ),
            "{changes:?}"
        );
    }

    #[test]
    fn the_dialog_opens_the_way_it_was_last_used() {
        let (mut h, _engine) = duplicate_harness();
        fill_in(&mut h);
        h.key_press(egui::Key::Escape);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        assert_eq!(h.state().add.as_ref().unwrap().mode, AddMode::Fill);
        h.get_by_label("Paste").click();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        assert_eq!(h.state().add.as_ref().unwrap().mode, AddMode::Paste);
    }

    #[test]
    fn what_was_added_is_selected_once_it_is_in_the_list() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(&mut h, "claude mcp add -s user files -- npx -y @x/fs");
        h.state_mut().add.as_mut().unwrap().places.projects[1].1 = true;
        h.state_mut().add.as_mut().unwrap().places_chosen = true;
        h.run();
        h.get_by_label("Review").click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ files"], &[], false)))
            .unwrap();
        h.run();
        // The search hides it; it is cleared so what was added can be seen.
        h.state_mut().search = "alpha".into();
        let (a, b) = two_rows();
        let rows = vec![
            a,
            b,
            live_row("files", Scope::User, None),
            live_row("files", Scope::Local, Some("/home/me/notes")),
            live_row("files", Scope::Local, Some("C:/Repos/WebApp")),
        ];
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: rows,
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
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
        // For the user and in notes, not the WebApp one that was there already.
        assert_eq!(h.state().selected, BTreeSet::from([2, 3]));
        assert_eq!(h.state().search, "");
    }

    #[test]
    fn nothing_is_selected_for_an_add_that_was_cancelled() {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label("beta").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(&mut h, "claude mcp add -s user alpha2 node");
        h.get_by_label("Review").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ alpha2"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        // Another change, saved: the cancelled add selects nothing.
        let (a, b) = two_rows();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, b, live_row("alpha2", Scope::User, None)],
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        engine
            .events
            .send(Event::Applied {
                plan: 8,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        assert_eq!(h.state().selected, BTreeSet::from([1]));
    }

    /// alpha (user) and beta (local in WebApp), and the forty-odd projects of a long-time Claude Code user.
    /// The form for an OAuth server, filled in as far as Review, with a client secret typed.
    fn sso_filled(h: &mut Harness<'static, ShellApp>) {
        fill_in(h);
        h.get_by_label("HTTP").click();
        h.run();
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

    /// What the form holds, to compare before and after a round trip through review.
    fn form_state(h: &mut Harness<'static, ShellApp>) -> (String, ServerFields, Secret) {
        let f = add_form(h);
        (f.name.clone(), f.fields.clone(), f.secret.clone())
    }

    #[test]
    fn a_review_that_fails_keeps_the_form_and_says_why_in_it() {
        let (mut h, engine) = duplicate_harness();
        sso_filled(&mut h);
        let before = form_state(&mut h);
        h.get_by_label("Review").click();
        h.run();
        assert!(waits_behind_review(&h));
        assert!(
            h.query_by_label("Fill in").is_none(),
            "hidden behind the review"
        );
        drain(&engine);
        engine
            .events
            .send(Event::PlanFailed(
                "The system's secret store is not available (no Secret Service).".into(),
            ))
            .unwrap();
        h.run();
        // The dialog is back as it was, saying why, and no toast says it elsewhere.
        assert_eq!(form_state(&mut h), before);
        assert!(!waits_behind_review(&h));
        h.get_by_label_contains("secret store is not available");
        assert!(h.state().outcome.is_none());
        // Fixed and sent again: the message goes.
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(duplicate_requested(&engine).len(), 1);
        assert!(add_form(&mut h).failed.is_none());
        // Claude Code refusing the add, after review, also comes back to the form.
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
            .unwrap();
        h.run();
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 7,
                error: "sso was not added for the user. Claude Code said: no".into(),
            })
            .unwrap();
        h.run();
        assert_eq!(form_state(&mut h), before);
        h.get_by_label_contains("Claude Code said: no");
    }

    #[test]
    fn the_review_of_an_add_goes_back_to_the_form_as_it_was() {
        let (mut h, engine) = duplicate_harness();
        sso_filled(&mut h);
        let before = form_state(&mut h);
        h.get_by_label("Review").click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label(&format!("{} Back", icon::ARROW_LEFT))
            .click();
        h.run();
        assert!(h.state().plan.is_none());
        assert!(matches!(&drain(&engine)[..], [Command::Discard(7)]));
        assert_eq!(form_state(&mut h), before);
        h.get_by_label("Fill in");
        // Cancel in the review drops the add as a whole.
        h.get_by_label("Review").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(8, &["+ sso"], &[], false)))
            .unwrap();
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.state().add.is_none());
        // Other reviews have no Back.
        h.get_by_label("beta").click();
        h.run();
        h.key_press(egui::Key::Delete);
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(9, &["- beta"], &[], true)))
            .unwrap();
        h.run();
        assert!(
            h.query_by_label(&format!("{} Back", icon::ARROW_LEFT))
                .is_none()
        );
    }

    #[test]
    fn review_pressed_too_soon_says_what_is_missing_under_each_field_and_goes_there() {
        let (mut h, engine) = duplicate_harness();
        fill_in(&mut h);
        h.get_by_label("HTTP").click();
        h.run();
        let f = add_form(&mut h);
        f.fields.headers = vec![(String::new(), "1".into())];
        h.run();
        assert!(
            h.query_by_label_contains("Type the server's address")
                .is_none()
        );
        h.get_by_label("Review").click();
        h.run();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
        // Each under its field, all at once.
        h.get_by_label_contains("Type the server's address");
        h.get_by_label_contains("Give it a name");
        h.get_by_label_contains("A header has a value but no name");
        h.get_by_label_contains("Fix what is marked above");
        assert_eq!(
            h.ctx.memory(|m| m.focused()),
            Some(field_id(Field::Part(Part::Address)))
        );
        // A message goes as soon as what it says is fixed.
        add_form(&mut h).fields.headers.clear();
        h.run();
        assert!(h.query_by_label_contains("A header has a value").is_none());
    }

    #[test]
    fn an_address_is_checked_when_it_is_left_and_https_is_added_for_the_user() {
        let (mut h, _engine) = duplicate_harness();
        fill_in(&mut h);
        h.get_by_label("HTTP").click();
        h.run();
        let address = field_id(Field::Part(Part::Address));
        h.ctx.memory_mut(|m| m.request_focus(address));
        h.run();
        add_form(&mut h).fields.url = "not a".into();
        h.run();
        // Still typing: not wrong yet.
        assert!(h.query_by_label_contains("Start the address").is_none());
        h.key_press(egui::Key::Tab);
        h.run();
        h.get_by_label_contains("Start the address with https://");
        // Typed without the scheme, it is completed when left, as a browser does.
        h.ctx.memory_mut(|m| m.request_focus(address));
        h.run();
        add_form(&mut h).fields.url = "mcp.linear.app/mcp".into();
        h.run();
        h.key_press(egui::Key::Tab);
        h.run();
        let f = add_form(&mut h);
        assert_eq!(f.fields.url, "https://mcp.linear.app/mcp");
        assert_eq!(f.name, "linear");
        h.run();
        assert!(h.query_by_label_contains("Start the address").is_none());
    }

    #[test]
    fn a_callback_port_another_program_holds_is_warned_about_once_left_and_does_not_stop_review() {
        let (mut h, engine) = duplicate_harness();
        // The engine checks the port, off the UI thread; here the test answers for it.
        let answer = |h: &mut Harness<'static, ShellApp>, port: u16, held: bool| {
            engine
                .events
                .send(klefki_core::api::Event::CallbackPort {
                    port,
                    warning: held.then(|| format!("Port {port} is in use by another program.")),
                })
                .unwrap();
            h.run();
        };
        let asked = |engine: &klefki_core::api::Detached| -> Vec<u16> {
            drain(engine)
                .into_iter()
                .filter_map(|c| match c {
                    Command::CheckCallbackPort(port) => Some(port),
                    _ => None,
                })
                .collect()
        };
        fill_in(&mut h);
        h.get_by_label("HTTP").click();
        h.run();
        let f = add_form(&mut h);
        f.fields.url = "https://sso.example.com/mcp".into();
        f.fields.oauth = true;
        h.run();
        let port = field_id(Field::Part(Part::CallbackPort));
        h.ctx.memory_mut(|m| m.request_focus(port));
        h.run();
        add_form(&mut h).fields.callback_port = "8766".into();
        h.run();
        // Not checked while it is typed in.
        assert_eq!(asked(&engine), Vec::<u16>::new());
        h.key_press(egui::Key::Tab);
        h.run();
        assert_eq!(asked(&engine), [8766]);
        answer(&mut h, 8766, true);
        h.get_by_label_contains("Port 8766 is in use by another program.");
        // A warning, not a problem: the port is the one the provider registered, and it may be free by sign-in.
        assert!(ready(&mut h));
        // Changed, the old answer is not shown; a free port says nothing, and a late answer for another port is not
        // taken for this one.
        h.ctx.memory_mut(|m| m.request_focus(port));
        h.run();
        add_form(&mut h).fields.callback_port = "8767".into();
        h.run();
        assert!(
            h.query_by_label_contains("in use by another program")
                .is_none()
        );
        h.key_press(egui::Key::Tab);
        h.run();
        assert_eq!(asked(&engine), [8767]);
        answer(&mut h, 8766, true);
        assert!(
            h.query_by_label_contains("in use by another program")
                .is_none()
        );
        answer(&mut h, 8767, false);
        assert!(
            h.query_by_label_contains("in use by another program")
                .is_none()
        );
        // Pressing Review checks again: the port may have been taken since.
        name_it(&mut h, "");
        h.run();
        h.get_by_label("Review").click();
        h.run();
        h.get_by_label_contains("Give it a name");
        assert_eq!(asked(&engine), [8767]);
        answer(&mut h, 8767, true);
        h.get_by_label_contains("Port 8767 is in use by another program.");
        // A wrong port is a problem, said instead, and not checked.
        add_form(&mut h).fields.callback_port = "99999".into();
        h.run();
        h.get_by_label_contains("must be a number from 1 to 65535");
        assert!(
            h.query_by_label_contains("in use by another program")
                .is_none()
        );
        h.ctx.memory_mut(|m| m.request_focus(port));
        h.run();
        h.key_press(egui::Key::Tab);
        h.run();
        assert_eq!(asked(&engine), Vec::<u16>::new());
    }

    #[test]
    fn a_pasted_oauth_server_without_a_client_secret_is_added_as_pasted() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        let text = "claude mcp add -s user -t http --client-id c-1 corp https://corp.example/mcp";
        paste(&mut h, text);
        assert!(
            h.query_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
                .is_none()
        );
        h.get_by_label_contains("Adds corp for the user.");
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Add {
                name: "corp".into(),
                json: pasted_json(text, 0),
                targets: vec![Target::User],
                secret: None,
            }]
        );
    }

    #[test]
    fn a_pasted_server_asking_for_a_secret_is_finished_in_the_form_or_said_why_not() {
        let (mut h, _engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        // The sign-in metadata address is a field of the form too.
        paste(
            &mut h,
            r#"claude mcp add-json -s user m --client-secret '{"type":"http","url":"https://m.example/mcp","oauth":{"authServerMetadataUrl":"https://id.example/meta"}}'"#,
        );
        h.get_by_label_contains("1 needs finishing in the form");
        h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
            .click();
        h.run();
        let f = add_form(&mut h);
        assert_eq!(f.mode, AddMode::Fill);
        assert_eq!(f.fields.metadata_url, "https://id.example/meta");
        h.run();
        h.get_by_label_contains("Metadata URL");
        // What the form cannot hold is not offered to it.
        h.get_by_label("Paste").click();
        h.run();
        paste(
            &mut h,
            r#"claude mcp add-json -s user m --client-secret '{"type":"http","url":"https://m.example/mcp","headersHelper":"get-token"}'"#,
        );
        assert!(
            h.query_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
                .is_none()
        );
        h.get_by_label_contains("Add it with Claude Code's own command");
        h.get_by_label_contains("1 server cannot be added");
    }

    #[test]
    fn a_secret_option_followed_by_the_name_is_finished_in_the_form_with_the_name_typed_there() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(
            &mut h,
            "claude mcp add -s user --transport http --client-id abc --client-secret corp https://mcp.corp.example/mcp",
        );
        // Said once, with what to do.
        h.get_by_label_contains("may be the secret, and the name with it. Fill it in");
        assert_eq!(h.query_all_by_label_contains("--client-secret").count(), 1);
        h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE))
            .click();
        h.run();
        let f = add_form(&mut h);
        assert_eq!(f.mode, AddMode::Fill);
        assert_eq!(f.fields.url, "https://mcp.corp.example/mcp");
        assert_eq!(f.fields.client_id, "abc");
        // The name comes from the address, like a typed one, until it is typed.
        assert_eq!(f.name, "corp");
        assert!(!f.name_edited);
        // The keyboard is where it is asked for: in the secret.
        h.run();
        assert_eq!(h.ctx.memory(|m| m.focused()), Some(secret_id()));
        let f = add_form(&mut h);
        *f.secret.edit() = "typed-here".into();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert!(
            matches!(&changes[..], [Change::Add { name, secret: Some(_), .. }] if name == "corp"),
            "{changes:?}"
        );
    }

    #[test]
    fn the_client_secret_can_be_shown_and_is_forgotten_with_the_dialog() {
        let (mut h, _engine) = duplicate_harness();
        sso_filled(&mut h);
        assert!(!add_form(&mut h).show_secret);
        h.get_by_label("Show the client secret").click();
        h.run();
        assert!(add_form(&mut h).show_secret);
        h.get_by_label("Hide the client secret").click();
        h.run();
        // Typed for real, so egui keeps undo steps of it.
        h.ctx.memory_mut(|m| m.request_focus(secret_id()));
        h.run();
        h.event(egui::Event::Text("x".into()));
        h.run();
        assert!(add_form(&mut h).secret.edit().ends_with('x'));
        let remembered = |h: &Harness<'static, ShellApp>| {
            h.ctx.data_mut(|d| {
                d.get_persisted::<egui::text_edit::TextEditState>(secret_id())
                    .is_some()
            })
        };
        assert!(remembered(&h));
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.state().add.is_none());
        assert!(!remembered(&h));
    }

    #[test]
    fn a_value_can_be_shown_to_check_it() {
        let (mut h, _engine) = duplicate_harness();
        fill_in(&mut h);
        add_form(&mut h).fields.env = vec![("TEAM".into(), "engineering".into())];
        h.run();
        h.get_by_label("Show TEAM").click();
        h.run();
        assert!(add_form(&mut h).rows.shown.contains(&0));
        h.get_by_label("Hide TEAM").click();
        h.run();
        assert!(add_form(&mut h).rows.shown.is_empty());
    }

    #[test]
    fn what_was_added_is_selected_however_its_project_is_spelled() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(&mut h, "claude mcp add files -- npx -y @x/fs");
        let f = add_form(&mut h);
        f.places.user = false;
        f.places.projects[0].1 = true;
        f.places_chosen = true;
        h.run();
        h.get_by_label_contains("Adds files in WebApp.");
        h.get_by_label("Review").click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ files"], &[], false)))
            .unwrap();
        h.run();
        let (a, b) = two_rows();
        // Claude Code wrote the key with a small drive letter and backslashes.
        let rows = vec![
            a,
            b,
            live_row("files", Scope::Local, Some(r"c:\Repos\WebApp")),
        ];
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: rows,
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
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
        assert_eq!(h.state().selected, BTreeSet::from([2]));
        h.get_by_label("Added files in WebApp.");
        // And a name taken under another spelling is taken.
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        paste(&mut h, "claude mcp add files -- npx -y @x/fs");
        let f = add_form(&mut h);
        f.places.user = false;
        f.places.projects[0].1 = true;
        f.places_chosen = true;
        h.run();
        h.get_by_label_contains("files already exists in WebApp");
    }

    #[test]
    fn an_oauth_add_says_to_sign_in_once_it_is_added() {
        let (mut h, engine) = duplicate_harness();
        sso_filled(&mut h);
        h.get_by_label("Review").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ sso"], &[], false)))
            .unwrap();
        h.run();
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
        assert!(h.state().add.is_none());
        h.get_by_label("Added sso for the user. Sign in to it from its Status tab.");
    }

    #[test]
    fn the_oauth_settings_come_into_view_in_a_small_window() {
        let (mut h, _engine) = duplicate_harness();
        h.set_size(egui::vec2(960.0, 600.0));
        h.run();
        fill_in(&mut h);
        h.get_by_label("HTTP").click();
        h.run();
        h.get_by_label(OAUTH_TOGGLE).click();
        for _ in 0..4 {
            h.run();
        }
        // In the part of the body in view: between the tabs and the line above the buttons.
        let top = h.get_by_label("Paste").rect().bottom();
        let bottom = h.get_by_label("Review").rect().top();
        for label in ["Client ID", "Client secret", "Callback port"] {
            let r = h.get_by_label(label).rect();
            assert!(
                r.top() >= top && r.bottom() <= bottom,
                "{label} at {r:?}, view {top}..{bottom}"
            );
        }
    }

    #[test]
    fn in_a_small_window_the_add_body_takes_the_height_the_window_has() {
        for (mode, failed) in [("Paste", false), ("Fill in", false), ("Fill in", true)] {
            let (mut h, _engine) = duplicate_harness_with_many_projects();
            h.set_size(egui::vec2(960.0, 600.0));
            h.run();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
            h.run();
            h.get_by_label(mode).click();
            h.run();
            if failed {
                add_form(&mut h).failed = Some("The secret store is locked. ".repeat(12));
                // Already in the first frame that says it, not one later.
                h.step();
                let bottom = h.get_by_label("Review").rect().bottom();
                assert!(
                    bottom <= h.ctx.content_rect().bottom(),
                    "the buttons end at {bottom} as the failure shows"
                );
            }
            for _ in 0..4 {
                h.run();
            }
            // The places list is longer than the window, so the body is as tall as it may be: the buttons end near
            // the bottom of the window, with the same margin as above the dialog, not far above it.
            let window = h.ctx.content_rect();
            let top = h.get_by_label("Paste").rect().top();
            let bottom = h.get_by_label("Review").rect().bottom();
            let below = window.bottom() - bottom;
            assert!(
                below >= 0.0,
                "{mode}: the buttons end at {bottom}, out of the window"
            );
            assert!(
                below <= top + 24.0,
                "{mode}: {below} left under the buttons, {top} above the dialog"
            );
        }
    }

    #[test]
    fn the_wheel_over_the_places_scrolls_the_add_dialog_as_one() {
        let (mut h, _engine) = duplicate_harness_with_many_projects();
        h.set_size(egui::vec2(960.0, 600.0));
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        h.run();
        // Over the list of projects, the wheel moves the dialog's body, not a list of its own inside it.
        let heading = h.get_by_label_contains("just for you (42").rect();
        let over = h.get_by_label("project-01").rect().center();
        h.event(egui::Event::PointerMoved(over));
        h.run();
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        for _ in 0..30 {
            h.step();
        }
        let moved = h.get_by_label_contains("just for you (42").rect();
        assert!(
            moved.top() < heading.top() - 20.0,
            "the heading was at {}, now {}",
            heading.top(),
            moved.top()
        );
    }

    #[test]
    fn a_pasted_server_to_finish_does_not_widen_the_dialog() {
        for text in [
            "claude mcp add --transport http --client-id abc --client-secret corp https://mcp.corp.example/mcp",
            "claude mcp add -t http --client-id c-1 --client-secret --callback-port 8080 corp https://corp.example/mcp",
        ] {
            let (mut h, _engine) = duplicate_harness();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
            h.run();
            let tabs = h.get_by_label("Paste").rect();
            paste(&mut h, text);
            h.run();
            h.get_by_label(&format!("{} Fill in", icon::PENCIL_SIMPLE));
            assert_eq!(h.get_by_label("Paste").rect(), tabs, "{text}");
        }
    }

    #[test]
    fn a_failure_in_the_add_dialog_says_the_plain_part_first_and_the_detail_apart() {
        let (mut h, _engine) = duplicate_harness();
        sso_filled(&mut h);
        add_form(&mut h).failed = Some(
            "Klefki can't use your system's secret store. Press Review again.\nThe system's secret store is not available (org.freedesktop.DBus.Error.ServiceUnknown)."
                .into(),
        );
        h.run();
        let plain = h
            .get_by_label("Klefki can't use your system's secret store. Press Review again.")
            .rect();
        let detail = h
            .get_by_label(
                "The system's secret store is not available (org.freedesktop.DBus.Error.ServiceUnknown).",
            )
            .rect();
        assert!(detail.top() >= plain.bottom(), "{plain:?} then {detail:?}");
        // A failure of one line is said as it is.
        add_form(&mut h).failed = Some("Claude Code refused it.".into());
        h.run();
        h.get_by_label("Claude Code refused it.");
    }

    #[test]
    fn the_client_secret_toggle_says_in_words_what_it_does() {
        let (mut h, _engine) = duplicate_harness();
        fill_in(&mut h);
        h.get_by_label("HTTP").click();
        h.run();
        h.get_by_label(OAUTH_TOGGLE).click();
        h.run();
        h.run();
        // Much wider than tall, as the word stands next to the eye (the eye alone is about twice as wide as
        // tall); the name read out starts with that word.
        let tall = h.get_by_label("Show the client secret").rect().height();
        let show = h.get_by_label("Show the client secret").rect().width();
        assert!(show > 3.0 * tall, "{show} wide, {tall} tall");
        h.get_by_label("Show the client secret").click();
        h.run();
        let hide = h.get_by_label("Hide the client secret").rect().width();
        assert!(hide > 3.0 * tall, "{hide} wide, {tall} tall");
    }

    #[test]
    fn the_tabs_stay_under_the_pointer_as_the_dialog_changes() {
        let (mut h, _engine) = duplicate_harness();
        fill_in(&mut h);
        h.run();
        let tabs = h.get_by_label("Paste").rect();
        h.get_by_label("HTTP").click();
        h.run();
        h.get_by_label(OAUTH_TOGGLE).click();
        h.run();
        h.run();
        assert_eq!(h.get_by_label("Paste").rect(), tabs);
        h.get_by_label("Paste").click();
        h.run();
        h.run();
        assert_eq!(h.get_by_label("Paste").rect(), tabs);
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

    fn open_duplicate_of(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
        h.run();
        assert!(h.state().duplicate.is_some());
    }

    #[test]
    fn duplicate_fits_in_the_window_however_many_projects_claude_knows() {
        let (mut h, _engine) = duplicate_harness_with_many_projects();
        open_duplicate_of(&mut h, "alpha");
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 900.0));
        for label in ["Review", "Cancel"] {
            let r = h.get_by_label(label).rect();
            assert!(
                window.contains_rect(r),
                "{label} at {r:?}, window {window:?}"
            );
        }
        let title = h.get_by_label_contains("Duplicate alpha").rect();
        assert!(window.contains_rect(title), "title at {title:?}");
    }

    #[test]
    fn duplicate_finds_a_project_by_part_of_its_path_and_keeps_what_was_ticked() {
        let (mut h, engine) = duplicate_harness_with_many_projects();
        open_duplicate_of(&mut h, "alpha");
        h.state_mut().duplicate.as_mut().unwrap().places.filter = "HOME/me".into();
        h.run();
        assert!(h.query_by_label("project-00").is_none());
        h.get_by_label("notes").click();
        h.run();
        h.state_mut().duplicate.as_mut().unwrap().places.filter = "web".into();
        h.run();
        assert!(h.query_by_label("notes").is_none());
        h.get_by_label("WebApp").click();
        h.run();
        h.state_mut()
            .duplicate
            .as_mut()
            .unwrap()
            .places
            .filter
            .clear();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Duplicate {
                id: h.state().snapshot.servers[0].id(),
                name: "alpha_copy".into(),
                targets: vec![
                    Target::User,
                    Target::Local("C:/Repos/WebApp".into()),
                    Target::Local("/home/me/notes".into()),
                ],
                adjust: Default::default(),
            }]
        );
    }

    #[test]
    fn duplicate_says_where_a_project_lives_only_when_its_folder_name_is_not_enough() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine
            .events
            .send(klefki_core::api::Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a],
                projects: vec![
                    "C:/work/app".into(),
                    "D:/play/App/".into(),
                    "C:/Repos/WebApp".into(),
                ],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        open_duplicate_of(&mut h, "alpha");
        h.get_by_label("app, C:/work");
        h.get_by_label("App, D:/play");
        h.get_by_label("WebApp");
    }

    #[test]
    fn duplicate_lists_the_project_the_server_is_in_first() {
        let (mut h, _engine) = duplicate_harness_with_many_projects();
        open_duplicate_of(&mut h, "beta");
        let here = h.get_by_label("WebApp").rect();
        let other = h.get_by_label("project-00").rect();
        assert!(
            here.top() < other.top(),
            "WebApp {here:?}, project-00 {other:?}"
        );
    }

    #[test]
    fn duplicate_says_in_words_where_the_copy_will_go() {
        let (mut h, _engine) = duplicate_harness();
        open_duplicate_of(&mut h, "alpha");
        h.get_by_label("Adds alpha_copy for the user.");
        h.get_by_label("WebApp").click();
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("Adds alpha_copy for the user and in WebApp and notes.");
        h.get_by_label_contains("For the user").click();
        h.run();
        h.get_by_label("Adds alpha_copy in WebApp and notes.");
        h.get_by_label("WebApp").click();
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("Tick at least one place.");
    }

    #[test]
    fn duplicate_says_where_the_name_is_already_taken_before_review() {
        let (mut h, _engine) = duplicate_harness();
        open_duplicate_of(&mut h, "alpha");
        // beta is a local server in WebApp; the user has none by that name.
        h.state_mut().duplicate.as_mut().unwrap().name = "beta".into();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
        h.get_by_label("WebApp").click();
        h.run();
        h.get_by_label_contains("beta already exists in WebApp");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        // The user's own alpha.
        h.state_mut().duplicate.as_mut().unwrap().name = "alpha".into();
        h.run();
        h.get_by_label_contains("alpha already exists for the user");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        // gamma comes from WebApp's .mcp.json, not from a place a copy is written to: allowed,
        // and the review warns that the local copy would hide it.
        h.state_mut().duplicate.as_mut().unwrap().name = "gamma".into();
        h.get_by_label_contains("For the user").click();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    }

    #[test]
    fn the_palette_offers_duplicate() {
        let (mut h, _engine) = duplicate_harness();
        h.get_by_label("alpha").click();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "dup");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().duplicate.as_ref().unwrap().name, "alpha_copy");
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
        engine.events.send(snapshot_of(vec![a, b])).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(vec![a, b.clone()])).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(vec![a.clone()])).unwrap();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "go");
        h.get_by_label_contains("Go to alpha");
        assert!(h.query_by_label_contains("Go to beta").is_none());
        engine.events.send(snapshot_of(vec![a, b])).unwrap();
        h.run();
        h.get_by_label_contains("Go to beta");
        // And one without the server takes it away again, query unchanged.
        engine
            .events
            .send(snapshot_of(vec![live_row("gamma", Scope::User, None)]))
            .unwrap();
        h.run();
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
        engine.events.send(snapshot_of(vec![a, b])).unwrap();
        h.run();
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
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
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
        engine
            .events
            .send(snapshot_of(vec![live_row("one", Scope::User, None)]))
            .unwrap();
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).click();
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(h.state().selected.is_empty());
    }

    // ---- the review dialog, the result, the empty places

    #[test]
    fn the_review_leads_with_words_and_its_button_says_what_it_does() {
        let (mut h, engine) = live_harness();
        let mut plan = plan_view(4, &["+ alpha"], &[], false);
        plan.summary = vec!["Turn off alpha in 3 projects".into()];
        plan.action = "Turn off".into();
        plan.notes =
            vec!["A running Claude Code session may need a restart before it notices this.".into()];
        engine.events.send(Event::PlanReady(plan)).unwrap();
        h.run();
        h.get_by_label("Turn off alpha in 3 projects");
        h.get_by_label_contains("may need a restart");
        assert!(h.query_by_label("Apply").is_none());
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(drain(&engine), vec![Command::Apply(4)]);
        // Pressed again while saving: nothing more is sent.
        h.get_by_label("Turn off").click();
        h.run();
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn a_success_fades_and_a_problem_stays() {
        let (mut h, engine) = live_harness();
        engine
            .events
            .send(Event::Applied {
                plan: 1,
                changed: true,
                backup: None,
                notes: Vec::new(),
            })
            .unwrap();
        h.run();
        h.get_by_label("Saved.");
        for _ in 0..((SUCCESS_SECONDS + 1.0) * 60.0) as usize {
            h.step();
        }
        assert!(
            h.query_by_label("Saved.").is_none(),
            "gone after a few seconds"
        );
        engine
            .events
            .send(Event::ApplyFailed {
                plan: 2,
                error: "Claude Code is writing its configuration right now.".into(),
            })
            .unwrap();
        h.run();
        for _ in 0..((SUCCESS_SECONDS + 1.0) * 60.0) as usize {
            h.step();
        }
        h.get_by_label_contains("writing its configuration");
    }

    #[test]
    fn a_page_that_is_not_built_says_what_it_will_do_and_leads_back() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Scope drift").click();
        h.run();
        h.get_by_label_contains("Coming soon");
        h.get_by_label("Back to connections").click();
        h.run();
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

    /// An answer to Export. A variable's name ending in `=value` is a plain setting with that default.
    fn exported_block(text: &str, variables: &[(&str, &str)], notes: &[&str]) -> Event {
        Event::Exported(Exported {
            text: text.to_owned(),
            servers: 1,
            left_out: 0,
            variables: variables
                .iter()
                .map(|(name, replaces)| {
                    let (name, default) = match name.split_once('=') {
                        Some((name, default)) => (name, Some(default.to_owned())),
                        None => (*name, None),
                    };
                    klefki_core::api::Variable {
                        name: name.to_owned(),
                        default,
                        replaces: vec![(*replaces).to_owned()],
                    }
                })
                .collect(),
            notes: notes.iter().map(|n| (*n).to_owned()).collect(),
        })
    }

    fn copied(h: &Harness<'_, ShellApp>) -> Vec<String> {
        h.output()
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn export_without_a_selection_takes_the_list_and_shows_the_block_to_copy() {
        let (mut h, engine) = live_harness();
        let rows = vec![
            live_row("beta", Scope::User, None),
            live_row("alpha", Scope::User, None),
            live_row("gamma", Scope::Local, Some("/work/app")),
        ];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        h.run();
        drain(&engine);
        h.get_by_label(icon::EXPORT).hover();
        h.run();
        h.get_by_label_contains("Export the servers in the list");
        h.get_by_label(icon::EXPORT).click();
        h.run();
        // In the list's order.
        assert_eq!(
            drain(&engine),
            vec![Command::Export(vec![
                rows[1].id(),
                rows[0].id(),
                rows[2].id()
            ])]
        );
        h.get_by_label("Export 3 servers");
        // Which ones, without recalling the list.
        h.get_by_label("alpha, beta and gamma");
        h.get_by_label("Reading the definitions…");
        let block = "{\n  \"mcpServers\": {}\n}";
        engine
            .events
            .send(exported_block(
                block,
                &[
                    ("GITHUB_TOKEN", "beta: env GITHUB_TOKEN"),
                    ("LOG_LEVEL=debug", "gamma: env LOG_LEVEL"),
                ],
                &["beta signs in with OAuth: its client secret is not in the block. Type it in when you add it."],
            ))
            .unwrap();
        h.run();
        h.get_by_label("1 variable to set where you add them");
        h.get_by_label("GITHUB_TOKEN");
        h.get_by_label("beta: env GITHUB_TOKEN");
        h.get_by_label("LOG_LEVEL");
        h.get_by_label("= debug (default)");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy names")
            .click();
        h.step();
        assert_eq!(copied(&h), ["GITHUB_TOKEN=\n"], "only the ones to set");
        h.run();
        // The button that copied says so; the block's Copy does not claim the names.
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
        assert!(h.query_by_label("Copy names").is_none());
        h.get_by_label_contains("its client secret is not in the block");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy")
            .click();
        h.step();
        assert_eq!(copied(&h), [block]);
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy names");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
            .click();
        h.run();
        assert!(h.query_by_label_contains("Export 3").is_none());
        assert!(h.state().exporting.is_none());
    }

    #[test]
    fn export_with_a_selection_takes_only_that_and_enter_copies() {
        let (mut h, engine) = live_harness();
        let rows = vec![
            live_row("alpha", Scope::User, None),
            live_row("beta", Scope::User, None),
        ];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        h.run();
        h.get_by_label("beta").click();
        h.run();
        drain(&engine);
        h.get_by_label(icon::EXPORT).hover();
        h.run();
        h.get_by_label_contains("Export the selected servers");
        h.get_by_label(icon::EXPORT).click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Export(vec![rows[1].id()])]);
        h.get_by_label("Export beta");
        // Nothing to copy yet: Enter does nothing.
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.state().exporting.as_ref().unwrap().copied.is_none());
        engine.events.send(exported_block("{}", &[], &[])).unwrap();
        h.run();
        h.get_by_label("Nothing was left out: no value needed a reference.");
        assert!(h.query_by_label("Copy names").is_none(), "no names to copy");
        h.key_press(egui::Key::Enter);
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
        // The confirmation goes after a few seconds, so a second copy is confirmed again.
        for _ in 0..20 {
            h.step();
        }
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
        // Enter on a focused Close is Close's: it closes and copies nothing.
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
            .focus();
        h.step();
        h.key_press(egui::Key::Enter);
        h.step();
        assert!(copied(&h).is_empty(), "{:?}", copied(&h));
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().exporting.is_none());
    }

    #[test]
    fn the_palette_offers_export_and_an_earlier_answer_does_not_fill_a_later_export() {
        let (mut h, engine) = live_harness();
        let rows = vec![live_row("alpha", Scope::User, None)];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        h.run();
        assert!(
            h.state()
                .commands()
                .iter()
                .any(|e| e.title == "Export the servers in the list"),
        );
        h.get_by_label(icon::EXPORT).click();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label(icon::EXPORT).click();
        h.run();
        drain(&engine);
        let shown = |h: &Harness<'_, ShellApp>| {
            h.state()
                .exporting
                .as_ref()
                .and_then(|v| v.result.as_ref())
                .map(|r| r.text.clone())
        };
        engine
            .events
            .send(exported_block("first", &[], &[]))
            .unwrap();
        h.run();
        h.get_by_label("Reading the definitions…");
        assert_eq!(shown(&h), None);
        engine
            .events
            .send(exported_block("second", &[], &[]))
            .unwrap();
        h.run();
        assert_eq!(shown(&h).as_deref(), Some("second"));
    }

    #[test]
    fn export_takes_the_whole_selection_even_rows_the_search_hides_now() {
        let (mut h, engine) = live_harness();
        let rows = vec![
            live_row("alpha", Scope::User, None),
            live_row("beta", Scope::User, None),
        ];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        h.run();
        h.get_by_label("beta").click();
        h.run();
        h.state_mut().search = "alpha".into();
        h.run();
        assert_eq!(h.state().visible(), [0], "the search hides beta");
        drain(&engine);
        h.get_by_label(icon::EXPORT).click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::Export(vec![rows[1].id()])]);
        h.get_by_label("Export beta");
        h.get_by_label("Includes 1 selected server the search hides.");
    }

    #[test]
    fn export_says_when_the_search_narrowed_the_list() {
        let (mut h, engine) = live_harness();
        let rows = vec![
            live_row("alpha", Scope::User, None),
            live_row("alps", Scope::User, None),
            live_row("beta", Scope::User, None),
        ];
        engine.events.send(snapshot_of(rows.clone())).unwrap();
        h.run();
        h.state_mut().search = "alp".into();
        h.run();
        h.get_by_label(icon::EXPORT).click();
        h.run();
        h.get_by_label("Export 2 servers");
        h.get_by_label("alpha and alps");
        h.get_by_label("Only the servers that match \u{201c}alp\u{201d}.");
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

    /// Opens Export on `rows` (nothing selected) and fills it with `block`; the commands sent so far are drained.
    fn export_open(
        rows: Vec<ServerRow>,
        block: &str,
    ) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
        h.get_by_label(icon::EXPORT).click();
        h.run();
        engine.events.send(exported_block(block, &[], &[])).unwrap();
        h.run();
        drain(&engine);
        (h, engine)
    }

    #[test]
    fn the_save_dialog_opens_over_klefkis_window_from_the_ui_thread() {
        let opened: Arc<std::sync::Mutex<Vec<(String, bool, std::thread::ThreadId)>>> =
            Arc::default();
        let (core, engine) = klefki_core::api::Core::detached_with_dialog({
            let opened = Arc::clone(&opened);
            move |name, owned| {
                opened
                    .lock()
                    .unwrap()
                    .push((name.to_owned(), owned, std::thread::current().id()));
                None
            }
        });
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 900.0))
            .build_eframe(move |cc| {
                crate::theme::apply(&cc.egui_ctx, ThemeChoice::Dark);
                ShellApp::with_core(core, ThemeChoice::Dark)
            });
        engine
            .events
            .send(snapshot_of(vec![live_row("github", Scope::User, None)]))
            .unwrap();
        h.run();
        h.get_by_label(icon::EXPORT).click();
        h.run();
        engine
            .events
            .send(exported_block("{\n  \"mcpServers\": {}\n}", &[], &[]))
            .unwrap();
        h.run();
        drain(&engine);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        // Opened once, by the UI's own thread, with Klefki's window as its owner.
        assert_eq!(
            *opened.lock().unwrap(),
            vec![(
                "github-mcp.json".to_owned(),
                true,
                std::thread::current().id()
            )]
        );
        // The engine is handed that open dialog to wait for.
        let sent = drain(&engine);
        assert!(
            matches!(
                sent.as_slice(),
                [Command::ChooseExportFile { name, shown: Some(_) }] if name == "github-mcp.json"
            ),
            "{sent:?}"
        );
        // Nearby: more frames open no second dialog while the first is waiting.
        h.run();
        assert_eq!(opened.lock().unwrap().len(), 1);
        assert!(drain(&engine).is_empty());
    }

    #[test]
    fn save_to_file_asks_where_in_the_systems_dialog_and_says_where_it_was_saved() {
        let block = "{\n  \"mcpServers\": {}\n}";
        let (mut h, engine) = export_open(vec![live_row("github", Scope::User, None)], block);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        // One server: its name is the file's.
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseExportFile {
                name: "github-mcp.json".into(),
                shown: None
            }]
        );
        // While the system's dialog is open, a second one is not asked for.
        assert!(
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
                .accesskit_node()
                .is_disabled()
        );
        let file = std::path::PathBuf::from("/home/me/github-mcp.json");
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::Chosen(file.clone())))
            .unwrap();
        h.run();
        // The system's dialog asked before replacing a file, so this does not ask again.
        assert_eq!(
            drain(&engine),
            vec![Command::SaveExport {
                path: file.clone(),
                text: block.into(),
                replace: true
            }]
        );
        engine
            .events
            .send(Event::ExportSaved {
                path: file.clone(),
                shown: "/home/me/github-mcp.json".into(),
                result: Ok(()),
            })
            .unwrap();
        h.run();
        // Said where a screen reader announces it too.
        let status = h.get_by_label("Saved to /home/me/github-mcp.json");
        assert_eq!(
            status.accesskit_node().live(),
            egui::accesskit::Live::Polite
        );
        assert!(
            !h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
                .accesskit_node()
                .is_disabled()
        );
        // Closing the system's dialog changes nothing.
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::Cancelled {
                suggested: "/home/me/github-mcp.json".into(),
            }))
            .unwrap();
        h.run();
        assert!(drain(&engine).is_empty());
        assert!(h.state().exporting.is_some());
    }

    #[test]
    fn a_long_saved_path_keeps_to_one_line_and_its_file_name_in_view() {
        let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        let long = format!("/{}/a-mcp.json", "a-very-long-folder-name/".repeat(12));
        let file = std::path::PathBuf::from(&long);
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::Chosen(file.clone())))
            .unwrap();
        h.run();
        engine
            .events
            .send(Event::ExportSaved {
                path: file,
                shown: long.clone(),
                result: Ok(()),
            })
            .unwrap();
        h.run();
        // Read out whole; drawn as one line ending in the file's name, inside the dialog.
        let said = format!("Saved to {long}");
        let rect = h.get_by_label(&said).rect();
        let copy = h
            .get_by_role_and_label(egui::accesskit::Role::Button, "Copy")
            .rect();
        assert!(rect.height() < copy.height(), "{rect:?}");
        // The dialog's content is 640 wide, starting where Copy does.
        assert!(
            rect.left() >= copy.left() && rect.right() <= copy.left() + 640.0,
            "{rect:?}"
        );
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
    fn after_the_dialog_closes_without_a_file_a_path_can_still_be_typed() {
        // Found in review: a desktop portal without a file chooser fails slowly, which looks like a cancel; without
        // this the user clicked Save to file… and nothing happened.
        let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::Cancelled {
                suggested: whole("/home/me/Documents/a-mcp.json").into(),
            }))
            .unwrap();
        h.run();
        assert!(
            h.query_by_label("Save to").is_none(),
            "a cancel opens nothing"
        );
        let said = h.get_by_label("Not saved.");
        assert_eq!(said.accesskit_node().live(), egui::accesskit::Live::Polite);
        h.get_by_label("Type a path instead").click();
        h.run();
        let field = h.get_by_label("Save to");
        assert_eq!(
            field.accesskit_node().value().as_deref(),
            Some(whole("/home/me/Documents/a-mcp.json").as_str())
        );
        assert!(
            h.query_by_label_contains("No save dialog could be opened")
                .is_none()
        );
        // The file's name is selected, as a save dialog does: typing replaces it and keeps the folder and `.json`.
        h.get_by_label("Save to").type_text("servers");
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
            .click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::SaveExport {
                path: whole("/home/me/Documents/servers.json").into(),
                text: "{}".into(),
                replace: false
            }]
        );
    }

    #[test]
    fn on_a_short_window_the_variables_fold_when_saving_needs_the_room() {
        let (mut h, engine) = live_harness();
        h.set_size(egui::vec2(800.0, 480.0));
        engine
            .events
            .send(snapshot_of(vec![
                live_row("a", Scope::User, None),
                live_row("b", Scope::User, None),
            ]))
            .unwrap();
        h.run();
        h.get_by_label(icon::EXPORT).click();
        h.run();
        engine
            .events
            .send(exported_block("{}", &[("TOKEN", "a: env TOKEN")], &[]))
            .unwrap();
        h.run();
        // Opened by hand (as one left open at a larger size stays open).
        h.get_by_label("1 variable to set where you add them")
            .click();
        h.run();
        h.get_by_label("a: env TOKEN");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::NoDialog {
                suggested: "/home/me/mcp-servers.json".into(),
            }))
            .unwrap();
        h.run();
        assert!(
            h.query_by_label("a: env TOKEN").is_none(),
            "folded for the path row"
        );
        // Once: opened again, it stays open.
        h.get_by_label("1 variable to set where you add them")
            .click();
        h.run();
        h.get_by_label("a: env TOKEN");
        let close = h
            .get_by_role_and_label(egui::accesskit::Role::Button, "Close")
            .rect();
        assert!(close.bottom() <= 480.0, "{close:?}");
    }

    #[test]
    fn copied_is_announced_to_screen_readers() {
        let (mut h, _engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
        for label in ["Copy", "Copied"] {
            let button = h.get_by_role_and_label(egui::accesskit::Role::Button, label);
            // A polite live region: the change of label is read out, without moving the focus.
            assert_eq!(
                button.accesskit_node().live(),
                egui::accesskit::Live::Polite,
                "{label}"
            );
            if label == "Copy" {
                button.click();
                h.run();
            }
        }
    }

    #[test]
    fn a_failed_save_says_why_and_the_block_stays() {
        let (mut h, engine) = export_open(
            vec![
                live_row("a", Scope::User, None),
                live_row("b", Scope::User, None),
            ],
            "{}",
        );
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseExportFile {
                name: "mcp-servers.json".into(),
                shown: None
            }]
        );
        let file = std::path::PathBuf::from("/home/me/.claude.json");
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::Chosen(file.clone())))
            .unwrap();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ExportSaved {
                path: file,
                shown: "/home/me/.claude.json".into(),
                result: Err(SaveError::Refused(
                    "That is Claude Code's own configuration.".into(),
                )),
            })
            .unwrap();
        h.run();
        let status = h.get_by_label("Not saved: That is Claude Code's own configuration.");
        assert_eq!(
            status.accesskit_node().live(),
            egui::accesskit::Live::Polite
        );
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
    }

    #[test]
    fn without_a_save_dialog_a_path_is_typed_and_an_existing_file_is_replaced_only_when_confirmed()
    {
        let (mut h, engine) = export_open(
            vec![
                live_row("a", Scope::User, None),
                live_row("b", Scope::User, None),
            ],
            "{}",
        );
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        drain(&engine);
        let suggested = std::path::PathBuf::from(whole("/home/me/Documents/mcp-servers.json"));
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::NoDialog {
                suggested: suggested.clone(),
            }))
            .unwrap();
        h.run();
        h.get_by_label_contains("No save dialog could be opened");
        let field = h.get_by_label("Save to");
        assert_eq!(
            field.accesskit_node().value().as_deref(),
            Some(whole("/home/me/Documents/mcp-servers.json").as_str())
        );
        // The field has the keyboard, so Enter saves (and does not copy).
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(copied(&h).is_empty());
        assert_eq!(
            drain(&engine),
            vec![Command::SaveExport {
                path: suggested.clone(),
                text: "{}".into(),
                replace: false
            }]
        );
        engine
            .events
            .send(Event::ExportSaved {
                path: suggested.clone(),
                shown: whole("/home/me/Documents/mcp-servers.json"),
                result: Err(SaveError::Exists),
            })
            .unwrap();
        h.run();
        h.get_by_label("mcp-servers.json is there already. Replace it?");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Keep it")
            .click();
        h.run();
        assert!(drain(&engine).is_empty());
        assert!(h.query_by_label_contains("is there already").is_none());
        h.get_by_label("Save to");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
            .click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ExportSaved {
                path: suggested.clone(),
                shown: whole("/home/me/Documents/mcp-servers.json"),
                result: Err(SaveError::Exists),
            })
            .unwrap();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Replace")
            .click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::SaveExport {
                path: suggested.clone(),
                text: "{}".into(),
                replace: true
            }]
        );
        engine
            .events
            .send(Event::ExportSaved {
                path: suggested,
                shown: whole("/home/me/Documents/mcp-servers.json"),
                result: Ok(()),
            })
            .unwrap();
        h.run();
        h.get_by_label(&format!(
            "Saved to {}",
            whole("/home/me/Documents/mcp-servers.json")
        ));
        assert!(
            h.query_by_label("Save to").is_none(),
            "the path field is done"
        );
    }

    #[test]
    fn a_typed_path_must_be_a_whole_path_and_quotes_around_it_are_dropped() {
        let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::NoDialog {
                suggested: std::path::PathBuf::from("/home/me/a-mcp.json"),
            }))
            .unwrap();
        h.run();
        drain(&engine);
        h.get_by_label("Save to").focus();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.get_by_label("Save to").type_text("servers.json");
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
            .click();
        h.run();
        assert!(drain(&engine).is_empty());
        h.get_by_label_contains("the whole path");
        let whole = if cfg!(windows) {
            r"C:\me\servers.json"
        } else {
            "/me/servers.json"
        };
        h.get_by_label("Save to").focus();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.get_by_label("Save to").type_text(&format!("\"{whole}\""));
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save here")
            .click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::SaveExport {
                path: std::path::PathBuf::from(whole),
                text: "{}".into(),
                replace: false
            }]
        );
    }

    #[test]
    fn a_dialog_answer_after_export_was_closed_saves_nothing() {
        let (mut h, engine) = export_open(vec![live_row("a", Scope::User, None)], "{}");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
            .click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
            .click();
        h.run();
        // Open again, with another block.
        h.get_by_label(icon::EXPORT).click();
        h.run();
        engine
            .events
            .send(exported_block("second", &[], &[]))
            .unwrap();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ExportFileChosen(FileChoice::Chosen(
                "/x/a.json".into(),
            )))
            .unwrap();
        h.run();
        assert!(
            drain(&engine).is_empty(),
            "the file was picked for the export that was closed"
        );
        assert!(
            !h.get_by_role_and_label(egui::accesskit::Role::Button, "Save to file…")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn export_is_off_when_the_list_is_empty() {
        let (mut h, engine) = live_harness();
        engine.events.send(snapshot_of(Vec::new())).unwrap();
        h.run();
        assert!(h.get_by_label(icon::EXPORT).accesskit_node().is_disabled());
        h.get_by_label(icon::EXPORT).hover();
        h.run();
        h.get_by_label_contains("no server to export");
    }

    #[test]
    fn the_toolbar_shows_only_what_works() {
        let (mut h, _engine) = live_harness();
        h.run();
        // Duplicate (icon::COPY), Add (icon::PLUS) and Sign in (icon::SIGN_IN) were in this list until they were
        // built; they are shown now.
        h.get_by_label(icon::COPY);
        h.get_by_label(icon::PLUS);
        h.get_by_label(icon::SIGN_IN);
        // And Export, since M3.
        h.get_by_label(icon::EXPORT);
        for gone in [icon::SWAP, icon::DOWNLOAD_SIMPLE] {
            assert!(h.query_by_label(gone).is_none(), "{gone:?}");
        }
        assert!(
            h.query_by_label_contains("selected").is_none(),
            "no count without a selection"
        );
    }

    // ---- settings: appearance

    #[test]
    fn settings_switch_the_greys_and_remember_them() {
        let mut h = harness();
        h.run();
        h.get_by_label_contains("Settings").click();
        h.run();
        assert_eq!(h.state().page, Page::Settings);
        h.get_by_label("Warm gray").click();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_DARK.panel);
        h.get_by_label_contains("Light").click();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_LIGHT.panel);
        let mut storage = theme::tests::MemoryStorage::default();
        eframe::App::save(h.state_mut(), &mut storage);
        assert_eq!(theme::load_tone(Some(&storage)), Tone::Warm);
        assert_eq!(theme::load_choice(Some(&storage)), ThemeChoice::Light);
        h.get_by_label("Cool (Catppuccin)").click();
        h.run();
        assert_eq!(theme::palette(&h.ctx).panel, theme::LIGHT.panel);
    }

    #[test]
    fn the_palette_changes_the_greys_too() {
        let (mut h, _engine) = live_harness();
        h.run();
        ctrl_k(&mut h);
        type_into_palette(&mut h, "greys warm");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().tone, Tone::Warm);
        assert_eq!(theme::palette(&h.ctx).panel, theme::WARM_DARK.panel);
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("web").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "General").click();
        h.run();
        drain_all(&engine);
        h.get_by_label("This server needs you to sign in.");
        h.get_by_label("Sign in").click();
        h.run();
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
            h.get_by_label("Sign in").click();
            h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("Continue").click();
        h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("Try again").click();
        h.run();
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(row.id())]);
        h.get_by_label("Starting Claude Code…");
        engine
            .events
            .send(sign_in_event(&row, failure("Again.", None, true)))
            .unwrap();
        h.run();
        h.get_by_label("Close").click();
        h.run();
        assert!(h.state().sign_in.is_none());
        assert!(
            sign_in_commands(&engine).is_empty(),
            "closing a failed sign-in sends nothing"
        );
    }

    #[test]
    fn when_the_browser_did_not_open_copying_the_link_is_the_way_on() {
        let (mut h, engine, row) = needs_auth_harness();
        h.get_by_label("Sign in").click();
        h.run();
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
        // A server that works and has no OAuth set up (most likely a token in a header): not up front.
        open_status_tab(&mut h, "web");
        assert!(h.query_by_label_contains("Sign in").is_none());
        assert!(h.get_by_label(icon::SIGN_IN).accesskit_node().is_disabled());
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
            assert!(!h.get_by_label(icon::SIGN_IN).accesskit_node().is_disabled());
        }
        h.get_by_label("cmd").click();
        h.run();
        assert!(h.query_by_label_contains("Sign in").is_none());
        assert!(h.get_by_label(icon::SIGN_IN).accesskit_node().is_disabled());
        h.get_by_role_and_label(Role::Button, "Auth").click();
        h.run();
        h.get_by_label_contains("does not sign in with OAuth");
        h.get_by_label("sso").click();
        h.run();
        h.get_by_label(icon::SIGN_IN).click();
        h.run();
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
            h.get_by_label("Sign in").click();
            h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("Keep waiting").click();
        h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
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
        h.get_by_label("Review").click();
        h.run();
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
        h.get_by_label("Sign in").click();
        h.run();
        assert_eq!(sign_in_commands(&engine), vec![Command::SignIn(sso.id())]);
        h.get_by_label("Sign in to sso");
        assert!(h.query_by_label_contains("Added sso").is_none());
    }

    // ---- move

    fn open_move_of(h: &mut Harness<'static, ShellApp>, name: &str) {
        h.get_by_label(name).click();
        h.run();
        h.get_by_label(icon::ARROW_FAT_LINES_RIGHT).click();
        h.run();
        assert!(h.state().moving.is_some());
        h.get_by_label(&format!("Move {name}"));
    }

    #[test]
    fn move_sends_a_local_server_to_the_user_and_cannot_choose_where_it_is_now() {
        let (mut h, engine) = duplicate_harness();
        open_move_of(&mut h, "beta");
        // Nothing is chosen yet, and where it is now cannot be.
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.get_by_label_contains("Choose where it goes.");
        let here = h.get_by_label("WebApp");
        assert!(here.accesskit_node().is_disabled());
        here.click();
        h.run();
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.get_by_label("For the user, in every project").click();
        h.run();
        h.get_by_label("Moves beta to the user.");
        h.get_by_label_contains("keeps its settings");
        h.get_by_label("Review").click();
        h.run();
        assert!(h.state().moving.is_none());
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Move {
                id: h.state().snapshot.servers[1].id(),
                to: Target::User,
            }]
        );
    }

    #[test]
    fn move_chooses_one_place_at_a_time() {
        use egui::accesskit::Role;
        let (mut h, engine) = duplicate_harness();
        open_move_of(&mut h, "alpha");
        assert!(
            h.get_by_label("For the user, in every project")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("WebApp").click();
        h.run();
        // Clicking the chosen one again keeps it chosen.
        h.get_by_label("WebApp").click();
        h.run();
        let chosen: Vec<String> = h
            .get_all_by_role(Role::RadioButton)
            .filter(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
            .map(|n| n.accesskit_node().label().unwrap_or_default())
            .collect();
        assert_eq!(chosen, vec!["WebApp".to_owned()]);
        h.get_by_label("Moves alpha to WebApp.");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::Move {
                id: h.state().snapshot.servers[0].id(),
                to: Target::Local("C:/Repos/WebApp".into()),
            }]
        );
    }

    #[test]
    fn move_will_not_go_where_the_name_is_taken() {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        // Another alpha already in notes, written with another spelling of its path.
        let there = live_row("alpha", Scope::Local, Some(r"/home/me/notes/"));
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, there],
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        h.state_mut().selected = BTreeSet::from([0]);
        h.run();
        h.get_by_label(icon::ARROW_FAT_LINES_RIGHT).click();
        h.run();
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label_contains("alpha already exists in notes");
        assert!(h.get_by_label("Review").accesskit_node().is_disabled());
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
        // Somewhere free is fine.
        h.get_by_label("WebApp").click();
        h.run();
        assert!(!h.get_by_label("Review").accesskit_node().is_disabled());
    }

    #[test]
    fn move_is_in_the_palette_for_user_and_local_servers_only() {
        let (mut h, _engine) = duplicate_harness();
        h.get_by_label("gamma").click();
        h.run();
        assert!(
            h.get_by_label(icon::ARROW_FAT_LINES_RIGHT)
                .accesskit_node()
                .is_disabled()
        );
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
        assert!(h.query_by_label_contains("Move gamma").is_none());
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label("beta").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run();
        h.get_by_label_contains("Move beta").click();
        h.run();
        assert!(h.state().moving.is_some());
        h.get_by_label("Move beta");
    }

    #[test]
    fn a_moved_server_is_selected_where_it_went_and_the_message_says_moved() {
        let (mut h, engine) = duplicate_harness();
        open_move_of(&mut h, "beta");
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(
                7,
                &["- beta", "+ beta"],
                &[],
                false,
            )))
            .unwrap();
        h.run();
        let (a, _) = two_rows();
        let moved = live_row("beta", Scope::Local, Some("/home/me/notes"));
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a, moved],
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        engine
            .events
            .send(Event::Applied {
                plan: 7,
                changed: true,
                backup: None,
                notes: vec!["Claude Code left out alwaysAllow.".into()],
            })
            .unwrap();
        h.run();
        assert_eq!(h.state().selected, BTreeSet::from([1]));
        h.get_by_label("Moved beta to notes. Claude Code left out alwaysAllow.");
        // Something was left out: said as a warning, which stays until it is closed.
        assert!(h.state().outcome.as_ref().is_some_and(|o| o.warn));
        // Undo moves it back where it was.
        drain(&engine);
        h.get_by_label("Undo").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            [Change::Move {
                id: ServerId::new(Scope::Local, Some("/home/me/notes"), "beta"),
                to: Target::Local("C:/Repos/WebApp".into()),
            }]
        );
    }

    #[test]
    fn a_change_made_with_claude_code_says_each_step_while_it_runs() {
        let (mut h, engine) = with_plan(&["+ alpha"], &[], false);
        h.get_by_label("Apply").click();
        h.run();
        h.get_by_label("Saving…");
        // A step of another plan is not this one's.
        for (plan, step) in [
            (8, "Claude Code is adding beta for the user…"),
            (7, "Claude Code is removing alpha for the user…"),
        ] {
            engine
                .events
                .send(Event::Applying {
                    plan,
                    step: step.into(),
                })
                .unwrap();
        }
        h.run();
        assert!(h.query_by_label("Saving…").is_none());
        assert!(h.query_by_label_contains("adding beta").is_none());
        h.get_by_label("Claude Code is removing alpha for the user…");
        h.get_by_label("Keep Klefki open until this is done.");
        drain(&engine);
    }

    #[test]
    fn a_server_that_signs_in_says_it_signs_in_again_after_a_move_and_offers_it() {
        let (mut h, engine) = live_harness();
        let sso = ServerRow {
            oauth: Some(OAuthInfo {
                client_id: None,
                scopes: vec![],
            }),
            ..web_row("sso")
        };
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![sso.clone()],
                projects: vec!["C:/Repos/WebApp".into()],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        open_move_of(&mut h, "sso");
        h.get_by_label_contains("so you will need to sign in to it again afterwards");
        h.get_by_label("WebApp").click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(
                7,
                &["- sso", "+ sso"],
                &[],
                false,
            )))
            .unwrap();
        h.run();
        let moved = ServerRow {
            connection: klefki_core::api::ConnectionId::new(
                Scope::Local,
                Some("C:/Repos/WebApp"),
                "sso",
            ),
            scope: Scope::Local,
            project: Some("C:/Repos/WebApp".into()),
            ..sso
        };
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![moved],
                projects: vec!["C:/Repos/WebApp".into()],
                ..Snapshot::default()
            })))
            .unwrap();
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
        h.get_by_label("Moved sso to WebApp. Sign in now, or later from its Status tab.");
        // Moving it back would sign it out again: no Undo.
        assert!(h.query_by_label("Undo").is_none());
    }

    // ---- kept

    fn kept_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a],
                kept: vec![klefki_core::api::Kept {
                    id: "1760000000000-0".into(),
                    name: "sso".into(),
                    place: "in WebApp".into(),
                    signs_in: true,
                    back: false,
                }],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        (h, engine)
    }

    #[test]
    fn a_server_that_could_not_be_put_back_is_shown_and_can_be_put_back() {
        let (mut h, engine) = kept_harness();
        h.get_by_label("A server is out of the configuration");
        h.get_by_label("sso in WebApp (signs in with OAuth)");
        h.get_by_label("Put back").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::PutBack {
                kept: "1760000000000-0".into()
            }]
        );
    }

    #[test]
    fn one_kept_server_is_spoken_of_as_one() {
        let (h, _engine) = kept_harness();
        h.get_by_label_contains("A change took it out and Claude Code could not put it back.");
        assert!(h.query_by_label_contains("took them out").is_none());
    }

    #[test]
    fn a_server_back_without_some_settings_offers_only_forget() {
        // Cross-review round 2: the notice said it was out of the configuration while it was in the list, and Put
        // back failed with "already exists there".
        let (mut h, engine) = live_harness();
        let (a, _) = two_rows();
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: vec![a],
                kept: vec![klefki_core::api::Kept {
                    id: "1760000000000-0".into(),
                    name: "sso".into(),
                    place: "in WebApp".into(),
                    signs_in: true,
                    back: true,
                }],
                ..Snapshot::default()
            })))
            .unwrap();
        h.run();
        h.get_by_label("Klefki keeps settings Claude Code left out");
        h.get_by_label("sso in WebApp is back without some settings");
        assert!(h.query_by_label("Put back").is_none());
        assert!(
            h.query_by_label_contains("out of the configuration")
                .is_none()
        );
        // Forgetting it says what was let go of.
        h.get_by_label("Forget\u{2026}").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::ForgetKept {
                kept: "1760000000000-0".into()
            }]
        );
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["- sso"], &[], false)))
            .unwrap();
        h.run();
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
        h.get_by_label("Klefki no longer keeps the definition of sso in WebApp.");
    }

    #[test]
    fn a_kept_server_can_be_forgotten_after_a_review() {
        let (mut h, engine) = kept_harness();
        h.get_by_label("Forget\u{2026}").click();
        h.run();
        assert_eq!(
            duplicate_requested(&engine),
            vec![Change::ForgetKept {
                kept: "1760000000000-0".into()
            }]
        );
    }

    #[test]
    fn nothing_kept_shows_no_notice() {
        let (h, _engine) = duplicate_harness();
        assert!(
            h.query_by_label_contains("out of the configuration")
                .is_none()
        );
    }

    #[test]
    fn a_copy_is_selected_and_the_message_says_copied() {
        let (mut h, engine) = duplicate_harness();
        open_duplicate_of(&mut h, "beta");
        h.get_by_label("Review").click();
        h.run();
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ beta_copy"], &[], false)))
            .unwrap();
        h.run();
        let mut rows = h.state().snapshot.servers.clone();
        rows.push(live_row("beta_copy", Scope::Local, Some("C:/Repos/WebApp")));
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: rows,
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
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
        assert_eq!(h.state().selected, BTreeSet::from([3]));
        h.get_by_label("Copied beta_copy in WebApp.");
    }

    #[test]
    fn every_toolbar_and_nav_icon_is_drawn_by_the_icon_font() {
        // Found trying Move in the real app: the arrows icon is at U+E0A0, where JetBrains Mono, first in every
        // font family, has its own branch symbol, so the button showed a branch. These are the private-use
        // characters JetBrains Mono draws itself (its Powerline symbols).
        const MONO_SYMBOLS: [std::ops::RangeInclusive<char>; 2] =
            ['\u{E0A0}'..='\u{E0A2}', '\u{E0B0}'..='\u{E0B3}'];
        let glyphs = ACTIONS
            .iter()
            .map(|(g, _)| *g)
            .chain(Page::ALL.iter().map(|(_, g, _)| *g));
        for glyph in glyphs {
            for c in glyph.chars() {
                assert!(
                    !MONO_SYMBOLS.iter().any(|r| r.contains(&c)),
                    "U+{:04X} is drawn by JetBrains Mono, not the icon font",
                    c as u32
                );
            }
        }
        // And the near variant: the icon once used for Move.
        assert!(
            MONO_SYMBOLS
                .iter()
                .any(|r| r.contains(&icon::ARROWS_LEFT_RIGHT.chars().next().unwrap()))
        );
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

    /// The Import dialog, open on `text` as read from `mcp-servers.json`.
    fn import_open(text: &str) -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = duplicate_harness();
        h.get_by_label(icon::TRAY_ARROW_DOWN).click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseImportFile { shown: None }]
        );
        engine
            .events
            .send(Event::ImportFile(ImportFile::Read {
                shown: "mcp-servers.json".into(),
                text: text.into(),
            }))
            .unwrap();
        h.run();
        h.run();
        (h, engine)
    }

    #[test]
    fn import_shows_what_a_file_holds_and_adds_it_as_one_plan() {
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label_contains("Import servers");
        h.get_by_label_contains("From mcp-servers.json");
        // Each server, what it runs or connects to, and what must be set where Claude Code runs.
        h.get_by_label_contains("https://docs.example.com/mcp");
        h.get_by_label_contains("AUTH_TOKEN");
        // alpha is there already: kept unless asked otherwise, and said so.
        h.get_by_label_contains("alpha already exists for the user.");
        h.get_by_label_contains("Adds docs and notion for the user.");
        h.get_by_label("Review").click();
        h.run();
        assert!(waits_behind_review(&h));
        assert_eq!(
            duplicate_requested(&engine),
            vec![
                Change::Add {
                    name: "docs".into(),
                    json: pasted_json(IMPORTED, 1),
                    targets: vec![Target::User],
                    secret: None,
                },
                Change::Add {
                    name: "notion".into(),
                    json: pasted_json(IMPORTED, 2),
                    targets: vec![Target::User],
                    secret: None,
                },
            ]
        );
    }

    #[test]
    fn a_server_that_exists_is_kept_replaced_or_imported_under_another_name() {
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label("Replace it").click();
        h.run();
        h.get_by_label_contains("Replaces alpha for the user.");
        h.get_by_label("Review").click();
        h.run();
        let alpha = live_row("alpha", Scope::User, None).id();
        let changes = duplicate_requested(&engine);
        assert!(
            changes.contains(&Change::Replace {
                id: alpha,
                json: pasted_json(IMPORTED, 0)
            }),
            "{changes:?}"
        );
        assert_eq!(changes.len(), 3, "{changes:?}");
        // Once done, what was imported is selected, replaced ones included, and the message says so.
        engine
            .events
            .send(Event::PlanReady(plan_view(7, &["+ docs"], &[], false)))
            .unwrap();
        h.run();
        let mut rows = h.state().snapshot.servers.clone();
        rows.push(live_row("docs", Scope::User, None));
        rows.push(live_row("notion", Scope::User, None));
        engine
            .events
            .send(Event::Snapshot(Arc::new(Snapshot {
                servers: rows,
                projects: vec!["C:/Repos/WebApp".into(), "/home/me/notes".into()],
                ..Snapshot::default()
            })))
            .unwrap();
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
        assert!(h.state().add.is_none());
        assert_eq!(h.state().selected, BTreeSet::from([0, 3, 4]));
        h.get_by_label_contains("Imported docs, notion and alpha for the user.");

        // Nearby: under a free name it is added like the others.
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label("import it as alpha_2").click();
        h.run();
        h.get_by_label_contains("Adds alpha_2, docs and notion for the user.");
        h.get_by_label("Review").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert!(
            changes.iter().any(|c| matches!(
                c,
                Change::Add { name, targets, .. } if name == "alpha_2" && *targets == [Target::User]
            )),
            "{changes:?}"
        );

        // Kept where it is, it still goes where it is not: a project ticked as well.
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label("notes").click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert!(
            changes.iter().any(|c| matches!(
                c,
                Change::Add { name, targets, .. }
                    if name == "alpha" && *targets == [Target::Local("/home/me/notes".into())]
            )),
            "{changes:?}"
        );
        assert!(!changes.iter().any(|c| matches!(c, Change::Replace { .. })));

        // Nearby: when everything is kept, it says nothing would change, and Review asks for nothing.
        let (mut h, engine) = import_open(r#"{"mcpServers": {"alpha": {"command": "node"}}}"#);
        h.get_by_label_contains("nothing would change");
        h.get_by_label("Review").click();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn each_oauth_server_asks_for_its_client_secret_and_only_it_gets_one() {
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label_contains("Client secret of notion");
        assert!(h.query_by_label_contains("Client secret of docs").is_none());
        *h.state_mut().add.as_mut().unwrap().secrets[2].edit() = "s3cret".into();
        h.run();
        // Never shown as typed unless asked.
        assert!(h.query_by_label_contains("s3cret").is_none());
        h.get_by_label("Review").click();
        h.run();
        let changes = duplicate_requested(&engine);
        assert_eq!(
            changes.iter().find_map(|c| match c {
                Change::Add { name, secret, .. } if name == "notion" => Some(secret.clone()),
                _ => None,
            }),
            Some(Some(Secret::new("s3cret".into())))
        );
        assert!(changes.iter().any(|c| matches!(
            c,
            Change::Add { name, secret: None, .. } if name == "docs"
        )));

        // Nearby: a command that asks for a client secret is not imported until one is typed.
        let (mut h, engine) = import_open(
            "claude mcp add --transport http --client-id abc --client-secret -s user sso https://sso.example.com/mcp",
        );
        h.get_by_label("Review").click();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
        h.get_by_label_contains("Type the client secret it asks for");
        *h.state_mut().add.as_mut().unwrap().secrets[0].edit() = "x".into();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert_eq!(duplicate_requested(&engine).len(), 1);
    }

    #[test]
    fn a_server_unticked_is_left_out_and_nothing_ticked_imports_nothing() {
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label("Import docs").click();
        h.run();
        h.get_by_label_contains("Adds notion for the user.");
        h.get_by_label("Import notion").click();
        h.run();
        h.get_by_label("Review").click();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
        // Nearby: one tick puts them all back.
        h.get_by_label_contains("All 3").click();
        h.run();
        h.get_by_label_contains("Adds docs and notion for the user.");
    }

    #[test]
    fn the_palette_offers_import() {
        let (mut h, engine) = duplicate_harness();
        drain(&engine);
        ctrl_k(&mut h);
        type_into_palette(&mut h, "import");
        h.get_by_label_contains("Import servers from a file");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseImportFile { shown: None }]
        );
        // Nearby: while the dialog is open, another is not asked for.
        h.get_by_label(icon::TRAY_ARROW_DOWN).click();
        h.run();
        assert!(drain(&engine).is_empty());
        assert!(
            h.get_by_label(icon::TRAY_ARROW_DOWN)
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_says_why_and_another_can_be_chosen() {
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label("Choose another file…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseImportFile { shown: None }]
        );
        engine
            .events
            .send(Event::ImportFile(ImportFile::Failed {
                shown: "big.json".into(),
                why: "big.json is larger than 256 KiB.".into(),
            }))
            .unwrap();
        h.run();
        h.get_by_label_contains("big.json is larger than 256 KiB.");
        // Nearby: a file that holds no server says what it read instead, and Review adds nothing.
        engine
            .events
            .send(Event::ImportFile(ImportFile::Read {
                shown: "notes.txt".into(),
                text: "just some notes".into(),
            }))
            .unwrap();
        h.run();
        h.get_by_label_contains("From notes.txt");
        h.get_by_label_contains("Nothing here adds a server");
        h.get_by_label("Review").click();
        h.run();
        assert!(duplicate_requested(&engine).is_empty());
    }

    #[test]
    fn without_a_file_dialog_a_path_is_typed_and_a_dropped_file_is_read() {
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseImportFile { shown: None }]
        );
        engine
            .events
            .send(Event::ImportFile(ImportFile::NoDialog))
            .unwrap();
        h.run();
        h.get_by_label_contains("Type the path of the file to import");
        // Quotes from a copied path are not part of it.
        let typed = whole("/home/me/servers.json");
        h.state_mut().add.as_mut().unwrap().path = format!("\"{typed}\"");
        h.run();
        h.get_by_label("Read").click();
        h.run();
        assert_eq!(drain(&engine), vec![Command::ReadImportFile(typed.into())]);
        // Nearby: a path that is not whole would be read from wherever Klefki was started, so it is refused.
        engine
            .events
            .send(Event::ImportFile(ImportFile::NoDialog))
            .unwrap();
        h.run();
        h.state_mut().add.as_mut().unwrap().path = "servers.json".into();
        h.run();
        h.get_by_label("Read").click();
        h.run();
        assert!(drain(&engine).is_empty());
        h.get_by_label_contains("Give the whole path, starting from the drive or /.");

        // A file dropped on the window is read for an import.
        let (mut h, engine) = duplicate_harness();
        /// A file dropped on the window: only its path is ever looked at.
        #[derive(Debug)]
        struct Dropped(std::path::PathBuf);
        impl egui::DroppedFile for Dropped {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                panic!("the UI reads no file")
            }
        }
        let drop = |h: &mut Harness<'_, ShellApp>, paths: &[&str]| {
            for path in paths {
                h.input_mut()
                    .dropped_files
                    .push(Arc::new(Dropped(path.into())));
            }
            h.run();
        };
        drop(&mut h, &["/home/me/dropped.json"]);
        assert_eq!(
            drain(&engine),
            vec![Command::ReadImportFile("/home/me/dropped.json".into())]
        );
        // Nearby: more than one file, or a file dropped while something else is open, is not read, and says so.
        let (mut h, engine) = duplicate_harness();
        drop(&mut h, &["/home/me/a.json", "/home/me/b.json"]);
        assert!(drain(&engine).is_empty());
        h.get_by_label_contains("Drop one file at a time to import it.");
        let (mut h, engine) = duplicate_harness();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        h.run();
        assert!(h.state().add.is_some());
        drain(&engine);
        drop(&mut h, &["/home/me/dropped.json"]);
        assert!(drain(&engine).is_empty());
        h.get_by_label_contains("To import dropped.json, close what is open, then drop it again.");
    }

    #[test]
    fn a_dialog_closed_without_a_file_leaves_a_way_to_type_the_path() {
        // Found in review: a desktop portal without a file chooser fails slowly, which looks like a cancel; without
        // a word, Import would seem to do nothing at all.
        let (mut h, engine) = duplicate_harness();
        h.get_by_label(icon::TRAY_ARROW_DOWN).click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ImportFile(ImportFile::Cancelled))
            .unwrap();
        h.run();
        assert!(h.state().add.is_none());
        h.get_by_label_contains("No file chosen.");
        h.get_by_label("Type a path instead").click();
        h.run();
        let form = h.state().add.as_ref().expect("the Import dialog");
        assert_eq!(form.mode, AddMode::Import);
        h.get_by_label_contains("Type the path of the file to import");
        assert!(h.query_by_label_contains("No file chosen.").is_none());
        // The system's dialog can still be tried again from there.
        h.get_by_label("Choose a file…").click();
        h.run();
        assert_eq!(
            drain(&engine),
            vec![Command::ChooseImportFile { shown: None }]
        );

        // Nearby: closing the dialog asked for from the Import dialog leaves that dialog as it was, with no notice.
        let (mut h, engine) = import_open(IMPORTED);
        h.get_by_label("Choose another file…").click();
        h.run();
        drain(&engine);
        engine
            .events
            .send(Event::ImportFile(ImportFile::Cancelled))
            .unwrap();
        h.run();
        h.get_by_label_contains("From ");
        assert!(h.query_by_label_contains("No file chosen.").is_none());
    }

    // ---- updates (decision 009)

    fn update_harness() -> (Harness<'static, ShellApp>, klefki_core::api::Detached) {
        let (mut h, engine) = live_harness_at_60fps();
        h.state_mut().checks_for_updates = true;
        (h, engine)
    }

    fn update_checks_sent(engine: &klefki_core::api::Detached) -> usize {
        drain_all(engine)
            .into_iter()
            .filter(|c| matches!(c, Command::CheckForUpdates))
            .count()
    }

    #[test]
    fn klefki_checks_for_updates_once_after_starting_not_while_starting() {
        let (mut h, engine) = update_harness();
        h.run();
        assert_eq!(update_checks_sent(&engine), 0, "not while starting");
        // Nothing else asks for frames here: the check must wake the window by itself.
        let frames = (crate::updates::START_DELAY * 60.0) as usize + 30;
        for _ in 0..frames {
            h.step();
        }
        assert!(h.ctx.input(|i| i.time) >= crate::updates::START_DELAY);
        assert_eq!(update_checks_sent(&engine), 1);
        for _ in 0..120 {
            h.step();
        }
        assert_eq!(update_checks_sent(&engine), 0, "once a run");
    }

    #[test]
    fn a_build_without_a_release_key_never_checks_and_says_so() {
        let (mut h, engine) = live_harness_at_60fps();
        for _ in 0..((crate::updates::START_DELAY * 60.0) as usize + 30) {
            h.step();
        }
        assert_eq!(update_checks_sent(&engine), 0);
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label_contains("This build does not check for updates");
        assert!(h.query_by_label("Check now").is_none());
    }

    #[test]
    fn a_found_update_shows_in_the_top_bar_and_opens_its_page() {
        let (mut h, engine) = update_harness();
        h.state_mut().updated_by_hand = Some("Klefki was installed from the .deb package.".into());
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        engine
            .events
            .send(Event::Update(UpdateCheck::Available {
                version,
                page: "https://github.com/DavySc/Klefki/releases/tag/v999.0.0".into(),
            }))
            .unwrap();
        h.run();
        drain_all(&engine);
        h.get_by_label_contains("Update: 999.0.0").click();
        h.run();
        assert!(drain_all(&engine).contains(&Command::OpenReleasePage(version)));
        // Settings says the same, with the same way to it.
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label("999.0.0 is available");
        h.get_by_label("Klefki was installed from the .deb package.");
        h.get_by_label("What's new").click();
        h.run();
        let sent = drain_all(&engine);
        assert!(sent.contains(&Command::OpenReleasePage(version)));
        assert!(
            !sent.contains(&Command::DownloadUpdate),
            "an install updated by hand downloads nothing"
        );
        assert!(h.query_by_label("Download").is_none());
    }

    fn found(engine: &klefki_core::api::Detached, version: klefki_core::api::Version) {
        engine
            .events
            .send(Event::Update(UpdateCheck::Available {
                version,
                page: format!("https://github.com/DavySc/Klefki/releases/tag/v{version}"),
            }))
            .unwrap();
    }

    fn downloads_sent(engine: &klefki_core::api::Detached) -> usize {
        drain_all(engine)
            .into_iter()
            .filter(|c| matches!(c, Command::DownloadUpdate))
            .count()
    }

    #[test]
    fn a_found_update_downloads_by_itself_then_offers_a_restart() {
        let (mut h, engine) = update_harness();
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        found(&engine, version);
        h.run_steps(2);
        assert_eq!(downloads_sent(&engine), 1);
        h.get_by_label_contains("Downloading 999.0.0");
        engine
            .events
            .send(Event::UpdateDownload(UpdateDownload::Ready(version)))
            .unwrap();
        h.run();
        h.get_by_label_contains("Restart to update");
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label("999.0.0 is ready: it installs when Klefki closes");
        // Restarting installs it; the stand-in engine has nothing downloaded, so this says why it could not.
        h.get_by_label("Restart now").click();
        h.run();
        h.get_by_label("No update has been downloaded.");
        h.get_by_label_contains("Update: 999.0.0");
    }

    #[test]
    fn with_automatic_updates_off_the_download_waits_for_a_click() {
        let (mut h, engine) = update_harness();
        h.state_mut().updates.auto = false;
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        found(&engine, version);
        h.run();
        assert_eq!(downloads_sent(&engine), 0);
        h.get_by_label_contains("Update: 999.0.0").click();
        h.run_steps(2);
        assert_eq!(downloads_sent(&engine), 1);
        engine
            .events
            .send(Event::UpdateDownload(UpdateDownload::Failed(
                "Could not download the update: no connection.".into(),
            )))
            .unwrap();
        h.run();
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label("Could not download the update: no connection.");
        h.get_by_label("Download").click();
        h.run_steps(2);
        assert_eq!(downloads_sent(&engine), 1, "tried again on request");
    }

    #[test]
    fn restarting_to_update_waits_until_the_open_change_is_done() {
        let (mut h, engine) = update_harness();
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        found(&engine, version);
        engine
            .events
            .send(Event::UpdateDownload(UpdateDownload::Ready(version)))
            .unwrap();
        h.run();
        h.state_mut().applying = true;
        h.run();
        assert!(
            h.get_by_label_contains("Restart to update")
                .accesskit_node()
                .is_disabled()
        );
        h.state_mut().applying = false;
        h.run();
        assert!(
            !h.get_by_label_contains("Restart to update")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn settings_check_now_shows_progress_then_the_answer() {
        let (mut h, engine) = update_harness();
        h.state_mut().updates.auto = false;
        h.state_mut().go(Page::Settings);
        h.run();
        assert!(
            h.query_by_label_contains("Update: 999").is_none(),
            "no update shown before one is found"
        );
        h.get_by_label("Check now").click();
        // The spinner asks for frames until the answer comes.
        h.run_steps(2);
        assert_eq!(update_checks_sent(&engine), 1);
        h.get_by_label("Checking…");
        engine
            .events
            .send(Event::Update(UpdateCheck::Failed(
                "Could not check for updates: no connection.".into(),
            )))
            .unwrap();
        h.run();
        h.get_by_label("Could not check for updates: no connection.");
        h.get_by_label("Check now").click();
        h.run_steps(2);
        engine
            .events
            .send(Event::Update(UpdateCheck::UpToDate))
            .unwrap();
        h.run();
        h.get_by_label("Up to date · checked just now");
        assert!(h.query_by_label_contains("Could not check").is_none());
        h.get_by_label("Check now").click();
        h.run_steps(2);
        engine
            .events
            .send(Event::Update(UpdateCheck::NoRelease))
            .unwrap();
        h.run();
        h.get_by_label("No signed release is published yet");
    }

    #[test]
    fn the_switch_says_whether_it_updates_or_only_checks() {
        let (mut h, _engine) = update_harness();
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label("Update Klefki automatically");
        h.state_mut().updated_by_hand = Some("Klefki was installed with the MSI.".into());
        h.run();
        h.get_by_label("Check for updates automatically");
    }

    #[test]
    fn an_app_that_cannot_replace_itself_says_why_and_offers_the_page() {
        let (mut h, engine) = update_harness();
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        found(&engine, version);
        h.run_steps(2);
        assert_eq!(downloads_sent(&engine), 1);
        let why =
            "Klefki cannot replace itself in this folder. Move Klefki to a folder you can change.";
        engine
            .events
            .send(Event::UpdateDownload(UpdateDownload::Manual(why.into())))
            .unwrap();
        h.run();
        h.get_by_label_contains("Update: 999.0.0").click();
        h.run();
        assert!(drain_all(&engine).contains(&Command::OpenReleasePage(version)));
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label(why);
    }

    #[test]
    fn an_update_that_failed_at_close_is_shown_and_not_downloaded_again_unasked() {
        let (mut h, engine) = update_harness();
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        let why = "The update could not be installed when Klefki closed: denied.";
        engine
            .events
            .send(Event::UpdateDownload(UpdateDownload::Failed(why.into())))
            .unwrap();
        found(&engine, version);
        h.run();
        assert_eq!(downloads_sent(&engine), 0);
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label(why);
        h.get_by_label("Download").click();
        h.run_steps(2);
        assert_eq!(downloads_sent(&engine), 1, "on request");
    }

    #[test]
    fn after_an_update_klefki_says_so_once() {
        let (mut h, engine) = update_harness();
        let version = klefki_core::api::Version::current();
        h.state_mut().updates.updated = Some(version);
        h.run();
        h.get_by_label_contains(&format!("Updated to {version}"))
            .click();
        h.run();
        assert!(drain_all(&engine).contains(&Command::OpenReleasePage(version)));
        assert!(h.query_by_label_contains("Updated to").is_none());
    }

    #[test]
    fn restarting_waits_for_every_open_dialog() {
        let (mut h, engine) = update_harness();
        h.run();
        let version = klefki_core::api::Version::parse("999.0.0").unwrap();
        found(&engine, version);
        engine
            .events
            .send(Event::UpdateDownload(UpdateDownload::Ready(version)))
            .unwrap();
        h.run();
        h.state_mut().forgetting = Some("x".into());
        h.run();
        assert!(
            h.get_by_label_contains("Restart to update")
                .accesskit_node()
                .is_disabled()
        );
        h.state_mut().forgetting = None;
        h.state_mut().import_waiting = true;
        h.run_steps(2);
        assert!(
            h.get_by_label_contains("Restart to update")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn turning_automatic_checks_off_is_remembered() {
        let (mut h, _engine) = update_harness();
        h.state_mut().go(Page::Settings);
        h.run();
        h.get_by_label("Update Klefki automatically").click();
        h.run();
        assert!(!h.state().updates.auto);
        let mut storage = crate::theme::tests::MemoryStorage::default();
        eframe::App::save(h.state_mut(), &mut storage);
        assert!(!crate::updates::Updates::load(Some(&storage)).auto);
    }

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
        engine.events.send(snapshot_of(rows)).unwrap();
        h.run();
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
        h.get_by_label("Save profile").click();
        h.run();
        h.get_by_label_contains("Give the profile a name.");
        assert!(sent(&engine).is_empty());
        h.get_by_label("Name").focus();
        h.get_by_label("Name").type_text("Writing");
        h.run();
        h.get_by_label("Save profile").click();
        h.run();
        h.get_by_label_contains("Tick at least one server.");
        // Grouped as on the Connections page.
        h.get_by_label("User");
        h.get_by_label("Local · app");
        h.get_by_role_and_label(Role::CheckBox, "beta").click();
        h.get_by_role_and_label(Role::CheckBox, "lint").click();
        h.run();
        h.get_by_label("Save profile").click();
        h.run();
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
        h.get_by_label("Save profile").click();
        h.run();
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
        h.get_by_label("Save profile").click();
        h.run();
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
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Every project").click();
        h.run();
        h.get_by_label("Use profile…").click();
        h.run();
        assert!(matches!(
            &sent(&engine)[..],
            [Command::Prepare(c)] if matches!(&c[..], [Change::UseProfile { project: None, only: true, .. }])
        ));
        // Unticked, only its servers are turned on.
        h.state_mut().preparing = false;
        h.get_by_role_and_label(Role::CheckBox, "Only these servers there")
            .click();
        h.run();
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Use profile…").click();
        h.run();
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
        h.get_by_label("Cancel").click();
        h.run();
        assert!(sent(&engine).is_empty());
        h.get_by_label_contains("Delete").click();
        h.run();
        h.get_by_label("Delete profile").click();
        h.run();
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
        h.get_by_label_contains("In use in notes.");
        h.get_by_label("Stop using in notes…").click();
        h.run();
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
        h.get_by_label("Stop using and delete…").click();
        h.run();
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
        h.get_by_label("Delete only").click();
        h.run();
        assert_eq!(sent(&engine), [Command::DeleteProfile("Work".to_owned())]);
        // Switched since (beta back on): no longer in use, and Delete asks as before.
        engine
            .events
            .send(snapshot_with_beta_off_in_notes(false))
            .unwrap();
        h.run();
        assert!(h.query_by_label_contains("In use in").is_none());
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
        h.get_by_label("Cancel").click();
        h.run();
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
        engine.events.send(snapshot_of(vec![blocked])).unwrap();
        h.run();
        open_profiles(&mut h, &engine, klefki_core::api::Profiles::default());
        h.get_by_label_contains("New profile").click();
        h.run();
        h.get_by_label("not loaded");
    }
}

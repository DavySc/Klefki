//! The Add dialog, and Import, which is the same dialog about a file: servers pasted, filled in part by part, or
//! read from a file, and the places they go.

use super::places::{Mark, Places, Taken, place_mark, place_row, places_in_words, tick_box};
use super::{
    ShellApp, closed_on_purpose, folder_name, icon_button, list_in_words, name_problem, settle,
    transport_name,
};
use crate::command::shortcut;
use crate::theme::{self, Palette};
use egui::{Align, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use klefki_core::api::{
    Change, Command, ImportFile, Part, Pasted, PastedServer, Scope, Secret, ServerFields, ServerId,
    ServerRow, Target, TransportKind, secret_reference,
};
use std::collections::{BTreeSet, HashMap};

/// How the Add dialog takes a server: pasted (a command or JSON, several at once), or filled in, part by part; or
/// read from a file (Import), which the dialog is then about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AddMode {
    #[default]
    Paste,
    Fill,
    Import,
}

/// A field of the form, for what is said under it and for giving it the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Field {
    Name,
    Part(Part),
    Secret,
}

/// What the value rows remember between frames: the rows shown as typed, and the value last replaced.
#[derive(Default)]
pub(super) struct RowsState {
    pub(super) shown: BTreeSet<usize>,
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
pub(super) struct AddForm {
    pub(super) mode: AddMode,
    pub(super) text: String,
    /// The text `read` was read from: it is read again only when the text changes.
    read_for: String,
    /// The servers found, or why there are none (empty while nothing is pasted).
    read: Result<Pasted, String>,
    /// One name per server read, as the user may have changed it.
    pub(super) names: Vec<String>,
    /// The form's server: its name and parts.
    pub(super) name: String,
    /// The name was typed; until then it follows what the server connects to or runs.
    pub(super) name_edited: bool,
    pub(super) fields: ServerFields,
    /// The form's OAuth client secret, typed hidden. Wiped from memory when dropped; handed to the engine once.
    pub(super) secret: Secret,
    /// The client secret is shown as typed, to check a paste.
    pub(super) show_secret: bool,
    /// Which value rows are shown as typed, and the value last replaced by a reference.
    pub(super) rows: RowsState,
    /// What `fields` make under `name`, and every problem with them, read again only when either changes.
    filled: Option<Filled>,
    pub(super) places: Places,
    /// A place was ticked by hand: what is pasted next no longer moves the ticks.
    pub(super) places_chosen: bool,
    /// Give the first field the keyboard on the next frame.
    pub(super) focus: bool,
    /// Give this field the keyboard, and bring it into view, on the next frame.
    focus_on: Option<Field>,
    /// Fields the keyboard has left: what is wrong with them is said from then on, not while they are typed in.
    left: BTreeSet<Field>,
    /// Review was pressed: everything that stops it is said.
    tried: bool,
    /// Review asked for a plan: the dialog waits behind it, as it was, to come back to.
    pub(super) reviewing: bool,
    /// Esc was pressed with something pasted or typed: the dialog asks before throwing it away.
    asking_to_discard: bool,
    /// The field that had the keyboard before the question, to give it back once the question is put away.
    focus_back: Option<egui::Id>,
    /// What had the keyboard at the end of the last frame: Esc takes it away before the dialog sees the key.
    focus_was: Option<egui::Id>,
    /// Give Keep editing the keyboard on the next frame.
    focus_answer: bool,
    /// Why the last review or add failed, said in the dialog.
    pub(super) failed: Option<String>,
    /// Bring the OAuth settings into view on the next frame.
    reveal_oauth: bool,
    /// The callback port to check, sent to the engine after this frame: when the field is left or Review is
    /// pressed, not while it is typed in.
    port_wanted: Option<u16>,
    /// The callback port sent to be checked, until its answer comes: it is asked once.
    port_pending: Option<u16>,
    /// The callback port last checked, and why a sign-in could not come back to it
    /// ([`Event::CallbackPort`](klefki_core::api::Event::CallbackPort)).
    port_checked: Option<(u16, Option<String>)>,
    /// Import: the file read, as it can be shown.
    file: Option<String>,
    /// Import: why the last file chosen could not be read.
    file_problem: Option<String>,
    /// Import: no open dialog could be shown, so a path is typed.
    no_dialog: bool,
    /// Import: the path is typed in, as asked for after a file dialog closed without a file.
    pub(super) typing: bool,
    pub(super) path: String,
    /// Import: Read was pressed for the path typed; it is sent to the engine after this frame.
    path_wanted: bool,
    /// Import, one per server read: whether it is imported.
    include: Vec<bool>,
    /// Import, one per server read: a server already there under its name is replaced rather than kept.
    replace: Vec<bool>,
    /// Import, one per server read: its client secret, typed hidden. Only one that signs in with OAuth gets one.
    pub(super) secrets: Vec<Secret>,
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

impl AddForm {
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
    pub(super) fn changes(&mut self, servers: &[ServerRow]) -> Option<Vec<Change>> {
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

/// The most servers an import holds (a paste of more is refused), so the most client secret fields it has.
const IMPORT_ROWS: usize = 100;

/// The most value rows whose text-field memory is forgotten on close; a server has a handful.
const FORGET_ROWS: usize = 64;

pub(super) fn field_id(field: Field) -> egui::Id {
    egui::Id::new(("add_field", field))
}

/// The client secret's text field, and each value row's: what egui remembers of them (undo steps) is forgotten
/// when the dialog closes, so a secret does not outlive the form.
pub(super) fn secret_id() -> egui::Id {
    egui::Id::new("add_client_secret")
}

fn value_id(noun: &str, row: usize) -> egui::Id {
    egui::Id::new(("add_value", noun, row))
}

/// An imported server's client secret field: what egui remembers of it is forgotten when the dialog closes.
fn import_secret_id(row: usize) -> egui::Id {
    egui::Id::new(("import_client_secret", row))
}

/// A row of the form: its label, left-aligned in a column, then what `add` draws.
/// Has egui forget the text fields of the closed Add dialog that may have held a secret: their undo steps keep
/// copies of what was typed.
pub(super) fn forget_typed(ctx: &egui::Context) {
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

pub(super) fn field_row<R>(
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
pub(super) fn field_note(
    ui: &mut Ui,
    pal: &Palette,
    label_width: f32,
    problem: Option<&str>,
    help: &str,
) {
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
pub(super) fn field_warning(ui: &mut Ui, pal: &Palette, label_width: f32, warning: &str) {
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

impl ShellApp {
    /// Opens the Add dialog, starting from the selected server's project if it has one.
    pub(super) fn open_add(&mut self) {
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

    pub(super) fn add_dialog(&mut self, ctx: &egui::Context) {
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
    pub(super) fn ask_import(&mut self) {
        if self.can_import() {
            self.import_waiting = true;
            self.import_choice_asked = true;
        }
    }

    /// A file dropped on the window is read for an import, when one could be asked for; otherwise it says why not.
    pub(super) fn take_dropped_file(&mut self, ctx: &egui::Context) {
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

    /// Opens the Import dialog on what came of asking for a file, or puts it into the one open.
    pub(super) fn open_import(&mut self, file: ImportFile) {
        if let Some(form) = self.open_import_form() {
            form.load(file);
        }
    }

    /// The Import dialog, opened if it can be.
    pub(super) fn open_import_form(&mut self) -> Option<&mut AddForm> {
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
    pub(super) fn close_add(&mut self) {
        self.add = None;
        self.forget_typed = true;
    }
}

//! Claude Code sessions in zellij.
//!
//! Loaded twice:
//! - in the background (load_plugins): follows the sessions from the events
//!   of the Claude Code hook, names their tabs after the conversation and
//!   sends the sessions to the other plugins;
//! - with `view "list"` (floating, from a key binding): lists the sessions of
//!   all the zellij sessions, Enter or a click jumps to one, then the
//!   previous conversations, resumed in a new tab. `p` docks it on the right,
//!   `docked "true"` starts it docked; Tab (or its tab) shows the git panel
//!   in its place.
//!
//! The background instance of each zellij session writes its sessions in the
//! cache shared by the instances of the plugin; in a zellij session without
//! it, the panes of Claude Code are found by their title. It also writes
//! there the projects directory of Claude Code (from the hook), where the
//! list reads the transcripts of the previous conversations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use common::panel::{self, Panel};
use common::{
    EVENT_PIPE, NAME_MARKER, STATE_PIPE, SYNC_PIPE, Session, Status, is_renamable, project, search_match,
    status_from_title, topic_from_title, truncate,
};
use serde_json::Value;
use zellij_tile::prelude::*;

/// Longest conversation topic in a tab name
const TAB_TOPIC_LENGTH: usize = 30;
/// Lines above the sessions in the list
const LIST_HEADER: usize = 3;
/// Seconds between two reads of the zellij sessions by the list
const SCAN_INTERVAL: f64 = 2.0;
/// The sessions of each zellij session (<name>.json), shared by the instances
const SESSIONS_DIR: &str = "/cache/sessions";
/// The projects directory of Claude Code, shared by the instances
const PROJECTS_FILE: &str = "/cache/projects";
/// Transcripts read, the latest ones
const HISTORY_LENGTH: usize = 100;
/// End of a transcript read for its title and directory (the start too for
/// the directory, if not found)
const TRANSCRIPT_TAIL: u64 = 256 * 1024;
const TRANSCRIPT_HEAD: u64 = 64 * 1024;

/// A previous conversation of Claude Code, from its transcript
#[derive(Default)]
struct Past {
    id: String,
    cwd: String,
    /// Set with /rename, else by Claude, else the last prompt
    title: String,
    /// Unix time of the last change
    modified: u64,
}

/// An entry of the list
enum Item<'a> {
    Running(&'a Session),
    Past(&'a Past),
}

/// The list floating: in the middle
const FLOATING: [&str; 4] = ["10%", "20%", "80%", "60%"];

#[derive(Default)]
struct State {
    list: bool,
    /// List: docked at the start, floating or docked
    docked: bool,
    panel: Panel,
    permitted: bool,
    /// Background: the name of its zellij session, the projects directory of
    /// Claude Code
    zellij_session: String,
    projects: String,
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    /// Background: the sessions by id, the last name requested by tab
    sessions: BTreeMap<String, Session>,
    tab_names: HashMap<usize, String>,
    /// List: the sessions of its zellij session (received), the zellij
    /// sessions, the sessions shown, the previous conversations, the search
    /// and the selected entry
    current: Vec<Session>,
    zellij_sessions: Vec<SessionInfo>,
    shown: Vec<Session>,
    past: Vec<Past>,
    search: Option<String>,
    selected: usize,
    /// List: the first line shown, the entry of each line shown
    offset: usize,
    lines: Vec<Option<usize>>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.list = configuration.get("view").is_some_and(|view| view == "list");
        self.docked = configuration.get("docked").is_some_and(|docked| docked == "true");

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::MessageAndLaunchOtherPlugins,
            // The list: the transcripts, claude --resume
            PermissionType::FullHdAccess,
            PermissionType::RunCommands,
        ]);

        let mut events = vec![EventType::PermissionRequestResult, EventType::TabUpdate, EventType::PaneUpdate];
        if self.list {
            events.extend([
                EventType::Key,
                EventType::Mouse,
                EventType::SessionUpdate,
                EventType::Timer,
                EventType::HostFolderChanged,
                EventType::RunCommandResult,
            ]);
        } else {
            events.push(EventType::SessionUpdate);
            set_selectable(false);
        }
        subscribe(&events);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.permitted = true;
                if self.list {
                    let id = get_plugin_ids().plugin_id;
                    rename_plugin_pane(id, "Claude Code");
                    self.panel = Panel::new("claude");
                    self.panel.start(self.docked);
                    pipe_message_to_plugin(MessageToPlugin::new(SYNC_PIPE));
                    self.scan();
                    match fs::read_to_string(PROJECTS_FILE) {
                        Ok(projects) => change_host_folder(PathBuf::from(projects.trim())),
                        // No event of the hook yet: the default directory
                        Err(_) => run_command(
                            &["sh", "-c", "echo \"${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects\""],
                            BTreeMap::new(),
                        ),
                    }
                } else {
                    self.broadcast();
                }
                true
            },
            Event::TabUpdate(tabs) => {
                if self.list {
                    self.panel.set_tabs(&tabs);
                }
                self.tabs = tabs;
                self.refresh(false);
                false
            },
            Event::PaneUpdate(panes) => {
                if self.list {
                    self.panel.update(&panes, FLOATING);
                }
                self.panes = panes;
                self.refresh(false);
                false
            },
            Event::SessionUpdate(zellij_sessions, _) if !self.list => {
                let name = zellij_sessions.iter().find(|info| info.is_current_session).map(|info| info.name.clone());
                if let Some(name) = name.filter(|name| *name != self.zellij_session) {
                    // Renamed: the file of the previous name goes
                    if !self.zellij_session.is_empty() {
                        let _ = fs::remove_file(session_file(&self.zellij_session));
                    }
                    self.zellij_session = name;
                    self.broadcast();
                }
                false
            },
            Event::SessionUpdate(zellij_sessions, _) if self.list => {
                self.zellij_sessions = zellij_sessions;
                self.rebuild();
                true
            },
            Event::Timer(elapsed) if self.list && self.panel.timer(elapsed) => false,
            Event::Timer(_) if self.list => {
                self.scan();
                true
            },
            Event::RunCommandResult(Some(0), stdout, _, _) if self.list => {
                change_host_folder(PathBuf::from(String::from_utf8_lossy(&stdout).trim()));
                false
            },
            Event::HostFolderChanged(_) if self.list => {
                self.history();
                self.clamp();
                true
            },
            Event::Key(key) if self.list => self.key(key),
            Event::Mouse(Mouse::LeftClick(0, column)) if self.list => {
                self.panel.click(0, column);
                false
            },
            Event::Mouse(Mouse::Hover(line, column)) if self.list => {
                self.panel.hover(line, column);
                false
            },
            Event::Mouse(Mouse::LeftClick(line, _)) if self.list => {
                let row = (line.max(0) as usize).checked_sub(LIST_HEADER);
                if let Some(index) = row.and_then(|row| self.lines.get(row).copied().flatten()) {
                    self.selected = index;
                    self.open();
                }
                true
            },
            Event::Mouse(Mouse::ScrollDown(_)) if self.list => self.key(KeyWithModifier::new(BareKey::Down)),
            Event::Mouse(Mouse::ScrollUp(_)) if self.list => self.key(KeyWithModifier::new(BareKey::Up)),
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        match message.name.as_str() {
            EVENT_PIPE if !self.list => {
                self.event(&message);
                false
            },
            SYNC_PIPE if !self.list => {
                self.broadcast();
                false
            },
            STATE_PIPE if self.list => {
                if let Some(sessions) = message.payload.as_deref().and_then(|p| serde_json::from_str(p).ok()) {
                    self.current = sessions;
                    self.rebuild();
                }
                true
            },
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if self.list {
            self.render_list(rows, cols);
        }
    }
}

impl State {
    /// An event of the Claude Code hook
    fn event(&mut self, message: &PipeMessage) {
        let Some(input) = message.payload.as_deref().and_then(|p| serde_json::from_str::<Value>(p).ok()) else {
            return;
        };
        let Some(id) = input["session_id"].as_str() else {
            return;
        };
        let event = input["hook_event_name"].as_str().unwrap_or_default();
        let time = message.args.get("time").and_then(|t| t.parse().ok()).unwrap_or_default();

        // For the previous conversations of the list
        let projects = input["transcript_path"].as_str().and_then(|path| Path::new(path).parent()?.parent()?.to_str());
        if let Some(projects) = projects.filter(|projects| *projects != self.projects) {
            let _ = fs::write(PROJECTS_FILE, projects);
            self.projects = projects.to_string();
        }

        if event == "SessionEnd" {
            if self.sessions.remove(id).is_some() {
                self.refresh(true);
            }
            return;
        }

        let Some(pane_id) = message.args.get("pane_id").and_then(|p| p.parse().ok()) else {
            return;
        };

        let (status, tool, notice) = match event {
            "SessionStart" | "Stop" | "StopFailure" => (Status::Idle, None, None),
            "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
                (Status::Working, None, None)
            },
            "PreToolUse" => (Status::Working, input["tool_name"].as_str(), None),
            "PermissionRequest" => (Status::Waiting, input["tool_name"].as_str(), None),
            "Notification" if needs_answer(&input) => (Status::Waiting, None, input["message"].as_str()),
            _ => return,
        };

        // One Claude by pane: the session replaces the previous one (/clear, resume)
        self.sessions.retain(|other, session| other == id || session.pane_id != pane_id);

        let session = self.sessions.entry(id.to_string()).or_insert_with(|| Session {
            id: id.to_string(),
            since: time,
            ..Default::default()
        });
        session.pane_id = pane_id;
        if let Some(cwd) = input["cwd"].as_str() {
            session.cwd = cwd.to_string();
        }
        if session.status != status {
            session.since = time;
        }
        session.status = status;
        session.tool = tool.map(str::to_string);
        session.message = notice.map(str::to_string);

        self.refresh(true);
    }

    /// Follows the panes of the sessions (tab, topic) and forgets the closed
    /// ones; renames the tabs and sends the sessions when something changed.
    fn refresh(&mut self, mut changed: bool) {
        if self.list {
            return;
        }

        let known_panes = !self.panes.panes.is_empty();
        let mut closed = vec![];

        for session in self.sessions.values_mut() {
            let pane = self.panes.panes.iter().find_map(|(position, panes)| {
                panes.iter().find(|pane| !pane.is_plugin && pane.id == session.pane_id).map(|pane| (*position, pane))
            });
            match pane {
                Some((position, pane)) => {
                    let tab_id = self.tabs.iter().find(|tab| tab.position == position).map(|tab| tab.tab_id);
                    let topic = topic_from_title(&pane.title);
                    if session.tab_position != Some(position) || session.tab_id != tab_id || session.topic != topic {
                        session.tab_position = Some(position);
                        session.tab_id = tab_id;
                        session.topic = topic;
                        changed = true;
                    }
                },
                None if known_panes => closed.push(session.id.clone()),
                None => {},
            }
        }

        for id in closed {
            self.sessions.remove(&id);
            changed = true;
        }

        if changed {
            self.rename_tabs();
            self.broadcast();
        }
    }

    /// Names each tab after its session (the one that needs attention first,
    /// else the latest), unless the tab was renamed by hand.
    fn rename_tabs(&mut self) {
        let mut chosen: HashMap<usize, &Session> = HashMap::new();
        for session in self.sessions.values() {
            let Some(tab_id) = session.tab_id else { continue };
            let current = chosen.entry(tab_id).or_insert(session);
            if (session.status, session.since) > (current.status, current.since) {
                *current = session;
            }
        }

        for (tab_id, session) in chosen {
            let Some(tab) = self.tabs.iter().find(|tab| tab.tab_id == tab_id) else { continue };
            let name =
                format!("{} {}{}", session.status.icon(), truncate(session.title(), TAB_TOPIC_LENGTH), NAME_MARKER);
            // A tab never renamed shows the title of its focused pane
            let untitled = self
                .panes
                .panes
                .get(&tab.position)
                .is_some_and(|panes| panes.iter().any(|pane| pane.title == tab.name));
            if tab.name == name || !(untitled || is_renamable(&tab.name)) || self.tab_names.get(&tab_id) == Some(&name)
            {
                continue;
            }
            rename_tab_with_id(tab_id as u64, &name);
            self.tab_names.insert(tab_id, name);
        }
    }

    /// The sessions to all the plugins and to the file of the zellij
    /// session, in the order of the tabs
    fn broadcast(&mut self) {
        let zellij_session = self.zellij_session.clone();
        let mut sessions: Vec<&mut Session> = self.sessions.values_mut().collect();
        for session in &mut sessions {
            session.zellij_session.clone_from(&zellij_session);
        }
        sessions.sort_by_key(|session| (session.tab_position, session.pane_id));
        let Ok(payload) = serde_json::to_string(&sessions) else { return };

        if !zellij_session.is_empty() {
            let _ = fs::create_dir_all(SESSIONS_DIR);
            let _ = fs::write(session_file(&zellij_session), &payload);
        }
        if self.permitted {
            pipe_message_to_plugin(MessageToPlugin::new(STATE_PIPE).with_payload(payload));
        }
    }

    /// The other zellij sessions are only read on request: again in a while
    fn scan(&mut self) {
        if let Ok(snapshot) = get_session_list() {
            self.zellij_sessions = snapshot.live_sessions;
            self.rebuild();
        }
        set_timeout(SCAN_INTERVAL);
    }

    /// The sessions of all the zellij sessions, the current one first: from
    /// the background instance of each one, else from the pane titles. The
    /// files of the zellij sessions that are gone are removed.
    fn rebuild(&mut self) {
        if self.zellij_sessions.is_empty() {
            self.shown = self.current.clone();
            self.clamp();
            return;
        }

        let mut shown = vec![];
        let mut alive = HashSet::new();

        for info in &self.zellij_sessions {
            alive.insert(session_file(&info.name));

            let tracked: Vec<Session> = if info.is_current_session {
                self.current.clone()
            } else {
                fs::read_to_string(session_file(&info.name))
                    .ok()
                    .and_then(|content| serde_json::from_str(&content).ok())
                    .unwrap_or_default()
            };

            let panes = || {
                info.panes.panes.iter().flat_map(|(position, panes)| {
                    panes.iter().filter(|pane| !pane.is_plugin).map(move |pane| (*position, pane))
                })
            };

            // The tracked sessions still running, with the title of their pane
            for mut session in tracked {
                let Some((position, pane)) = panes().find(|(_, pane)| pane.id == session.pane_id) else { continue };
                session.tab_position = Some(position);
                session.topic = topic_from_title(&pane.title);
                session.zellij_session.clone_from(&info.name);
                shown.push(session);
            }

            // The other panes of Claude Code
            for (position, pane) in panes() {
                let Some(status) = status_from_title(&pane.title) else { continue };
                if shown.iter().any(|s: &Session| s.zellij_session == info.name && s.pane_id == pane.id) {
                    continue;
                }
                shown.push(Session {
                    id: format!("{}:{}", info.name, pane.id),
                    pane_id: pane.id,
                    tab_position: Some(position),
                    topic: topic_from_title(&pane.title),
                    status,
                    zellij_session: info.name.clone(),
                    ..Default::default()
                });
            }
        }

        let current = self.zellij_sessions.iter().find(|info| info.is_current_session).map(|info| info.name.clone());
        shown.sort_by_key(|s| {
            (Some(&s.zellij_session) != current.as_ref(), s.zellij_session.clone(), s.tab_position, s.pane_id)
        });

        if let Ok(files) = fs::read_dir(SESSIONS_DIR) {
            for file in files.flatten() {
                let path = file.path().to_string_lossy().to_string();
                if !alive.contains(&path) {
                    let _ = fs::remove_file(path);
                }
            }
        }

        self.shown = shown;
        self.clamp();
    }

    /// The previous conversations: the latest transcripts in the projects
    /// directory of Claude Code (/host)
    fn history(&mut self) {
        let mut files = vec![];
        for project in fs::read_dir("/host").into_iter().flatten().flatten() {
            for file in fs::read_dir(project.path()).into_iter().flatten().flatten() {
                let path = file.path();
                if path.extension().is_none_or(|extension| extension != "jsonl") {
                    continue;
                }
                let modified = file.metadata().and_then(|metadata| metadata.modified()).ok();
                let modified = modified.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
                files.push((modified.unwrap_or_default(), path));
            }
        }
        files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        self.past =
            files.into_iter().take(HISTORY_LENGTH).filter_map(|(modified, path)| transcript(&path, modified)).collect();
    }

    /// The entries matching the search: the sessions, then the previous
    /// conversations not running
    fn items(&self) -> Vec<Item<'_>> {
        let search = self.search.as_deref().unwrap_or_default();
        let running = self.shown.iter().filter(|s| search_match(search, &format!("{} {}", s.project(), s.title())));
        let past = self.past.iter().filter(|past| {
            !self.shown.iter().any(|s| s.id == past.id)
                && search_match(search, &format!("{} {}", project(&past.cwd), past.title))
        });
        running.map(Item::Running).chain(past.map(Item::Past)).collect()
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.items().len().saturating_sub(1));
    }

    fn key(&mut self, key: KeyWithModifier) -> bool {
        let count = self.items().len();
        match key.bare_key {
            BareKey::Enter => self.open(),
            BareKey::Esc if self.search.is_some() => {
                self.search = None;
                self.selected = 0;
            },
            BareKey::Backspace if self.search.is_some() => {
                if let Some(search) = &mut self.search {
                    search.pop();
                }
                self.selected = 0;
            },
            BareKey::Char(c) if self.search.is_some() && key.key_modifiers.is_empty() => {
                if let Some(search) = &mut self.search {
                    search.push(c);
                }
                self.selected = 0;
            },
            BareKey::Char('/') => self.search = Some(String::new()),
            BareKey::Char('p') => self.panel.toggle(),
            BareKey::Tab => panel::switch(panel::other("claude")),
            BareKey::Down | BareKey::Char('j') if count > 0 => self.selected = (self.selected + 1) % count,
            BareKey::Up | BareKey::Char('k') if count > 0 => self.selected = (self.selected + count - 1) % count,
            BareKey::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if index < count {
                    self.selected = index;
                    self.open();
                }
            },
            BareKey::Esc | BareKey::Char('q') => close_self(),
            _ => return false,
        }
        true
    }

    /// Jumps to the selected session, or resumes the selected conversation
    /// in a new tab; closed unless docked
    fn open(&self) {
        match self.items().get(self.selected) {
            Some(Item::Running(session)) => {
                let current = self.zellij_sessions.iter().find(|info| info.is_current_session);
                if session.zellij_session.is_empty() || current.is_none_or(|info| info.name == session.zellij_session) {
                    focus_pane_with_id(PaneId::Terminal(session.pane_id), false, false);
                } else {
                    switch_session_with_focus(
                        &session.zellij_session,
                        session.tab_position,
                        Some((session.pane_id, false)),
                    );
                }
            },
            Some(Item::Past(past)) => {
                let command = CommandToRun {
                    path: "claude".into(),
                    args: vec!["--resume".to_string(), past.id.clone()],
                    cwd: (!past.cwd.is_empty()).then(|| PathBuf::from(&past.cwd)),
                };
                open_command_pane_in_new_tab(command, BTreeMap::new());
            },
            None => return,
        }
        if !self.panel.docked() {
            close_self();
        }
    }

    fn render_list(&mut self, rows: usize, cols: usize) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        let items = self.items();
        let running = items.iter().filter(|item| matches!(item, Item::Running(_))).count();

        // Bold, then dimmed, within the width (a longer line would wrap)
        let (bold, dim) = match &self.search {
            Some(search) => (format!("/ {search}▏"), " · Enter · Esc".to_string()),
            None => (
                format!("{running} session{}", if running == 1 { "" } else { "s" }),
                " · ↑↓ or 1-9 to jump · / search · Tab git · p dock · Esc".to_string(),
            ),
        };
        let bold = truncate(&bold, cols);
        let dim = truncate(&dim, cols.saturating_sub(bold.chars().count()));
        let header = format!("\u{1b}[1m{bold}\u{1b}[0m\u{1b}[2m{dim}\u{1b}[0m");

        // The entry of each line; None: the title of the previous conversations
        let mut lines = vec![];
        for (index, item) in items.iter().enumerate() {
            if matches!(item, Item::Past(_)) && index == running {
                lines.push(None);
            }
            lines.push(Some(index));
        }

        // The selected entry on screen
        let height = rows.saturating_sub(LIST_HEADER);
        let position = lines.iter().position(|line| *line == Some(self.selected)).unwrap_or(0);
        let mut offset = self.offset.min(lines.len().saturating_sub(height));
        if position < offset {
            offset = if position > 0 && lines[position - 1].is_none() { position - 1 } else { position };
        } else if position >= offset + height {
            offset = position + 1 - height;
        }

        let project_width = items
            .iter()
            .map(|item| match item {
                Item::Running(session) => session.project(),
                Item::Past(past) => project(&past.cwd),
            })
            .map(|project| project.chars().count())
            .max()
            .unwrap_or(0)
            .min(20);
        // The zellij session, when there are several
        let several = self.shown.iter().any(|s| s.zellij_session != self.shown[0].zellij_session);
        let zellij_width = if several {
            self.shown.iter().map(|s| s.zellij_session.chars().count()).max().unwrap_or(0).min(20)
        } else {
            0
        };

        let mut output = vec![];
        for line in lines.iter().skip(offset).take(height) {
            let Some(index) = *line else {
                output.push("\u{1b}[2m  Previous conversations\u{1b}[0m".to_string());
                continue;
            };
            let (icon, color, zellij, project, title, detail, since) = match items[index] {
                Item::Running(session) => {
                    let color = match session.status {
                        Status::Idle => "32",
                        Status::Working => "34",
                        Status::Waiting => "33",
                    };
                    let detail = match session.status {
                        Status::Working => session.tool.clone().unwrap_or_default(),
                        Status::Waiting => session.message.clone().or(session.tool.clone()).unwrap_or_default(),
                        Status::Idle => String::new(),
                    };
                    let zellij = session.zellij_session.as_str();
                    (session.status.icon(), color, zellij, session.project(), session.title(), detail, session.since)
                },
                Item::Past(past) => {
                    ("↺", "2", "", project(&past.cwd), past.title.as_str(), String::new(), past.modified)
                },
            };
            let age = if since > 0 && now >= since { age(now - since) } else { String::new() };
            let number = if index < 9 { (index + 1).to_string() } else { " ".to_string() };

            // The title takes the room left by the other columns
            let zellij =
                if several { format!("{:<zellij_width$}  ", truncate(zellij, zellij_width)) } else { String::new() };
            let left = format!("{number} {zellij}{:<project_width$}  ", truncate(project, project_width));
            // Narrow (docked): without the detail
            let right =
                if cols >= 80 { format!("  {:<24}  {:>3}", truncate(&detail, 24), age) } else { format!("  {age:>3}") };
            let room = cols.saturating_sub(2 + left.chars().count() + right.chars().count());
            let title = truncate(title, room);
            let space = room - title.chars().count();
            let text = truncate(&format!("{left}{title}{:space$}{right}", ""), cols.saturating_sub(2));

            let selected = if index == self.selected { "\u{1b}[7m" } else { "" };
            output.push(format!("{selected}\u{1b}[{color}m{icon}\u{1b}[39m {text}\u{1b}[0m"));
        }
        let empty = items.is_empty();
        drop(items);

        self.offset = offset;
        self.lines = lines.into_iter().skip(offset).take(height).collect();

        if empty {
            let found = if self.search.is_some() { " found" } else { "" };
            output.push(format!("\u{1b}[2mNo Claude Code session{found}\u{1b}[0m"));
        }
        // Without a newline after the last line: the pane would scroll
        print!("{}\n{header}\n\n{}", panel::tabs("claude"), output.join("\n"));
    }
}

/// A previous conversation from the end of its transcript (the start too for
/// the directory, if not found); None without a title
fn transcript(path: &Path, modified: u64) -> Option<Past> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut past = Past { id: path.file_stem()?.to_str()?.to_string(), modified, ..Default::default() };
    let (mut custom, mut ai, mut prompt) = (None, None, None);

    let tail = read(&mut file, size.saturating_sub(TRANSCRIPT_TAIL), TRANSCRIPT_TAIL)?;
    // The latest first; the first line may be cut
    for line in tail.lines().rev() {
        let wanted = |kind: &str| line.starts_with(&format!("{{\"type\":\"{kind}\""));
        if custom.is_none() && wanted("custom-title") {
            custom = field(line, "customTitle");
        } else if ai.is_none() && wanted("ai-title") {
            ai = field(line, "aiTitle");
        } else if prompt.is_none() && wanted("last-prompt") {
            prompt = field(line, "lastPrompt");
        } else if past.cwd.is_empty() && line.contains("\"cwd\":\"") {
            past.cwd = field(line, "cwd").unwrap_or_default();
        }
        if custom.is_some() && !past.cwd.is_empty() {
            break;
        }
    }
    if past.cwd.is_empty() && size > TRANSCRIPT_TAIL {
        let head = read(&mut file, 0, TRANSCRIPT_HEAD)?;
        past.cwd = head.lines().filter(|line| line.contains("\"cwd\":\"")).find_map(|line| field(line, "cwd"))?;
    }

    past.title = custom.or(ai).or(prompt)?.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(past)
}

fn read(file: &mut File, start: u64, length: u64) -> Option<String> {
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![];
    file.take(length).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A string field of a JSON line
fn field(line: &str, name: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    value[name].as_str().filter(|text| !text.is_empty()).map(str::to_string)
}

/// A notification asking for a permission or an answer; not the others
/// (waiting for the next prompt, authentication, agent completed...)
fn needs_answer(input: &Value) -> bool {
    match input["notification_type"].as_str() {
        Some(kind) => {
            matches!(kind, "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input")
        },
        None => !input["message"].as_str().unwrap_or_default().contains("waiting for your input"),
    }
}

fn session_file(zellij_session: &str) -> String {
    format!("{SESSIONS_DIR}/{}.json", zellij_session.replace('/', "_"))
}

fn age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

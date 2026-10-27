//! Claude Code sessions in zellij.
//!
//! Loaded twice:
//! - in the background (load_plugins): follows the sessions from the events
//!   of the Claude Code hook, names their tabs after the conversation and
//!   sends the sessions to the other plugins;
//! - with `view "list"` (floating, from a key binding): lists the sessions of
//!   all the zellij sessions, Enter or a click jumps to one.
//!
//! The background instance of each zellij session writes its sessions in the
//! cache shared by the instances of the plugin; in a zellij session without
//! it, the panes of Claude Code are found by their title.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{
    EVENT_PIPE, NAME_MARKER, STATE_PIPE, SYNC_PIPE, Session, Status, is_renamable, status_from_title, topic_from_title,
    truncate,
};
use serde_json::Value;
use zellij_tile::prelude::*;

/// Longest conversation topic in a tab name
const TAB_TOPIC_LENGTH: usize = 30;
/// Lines above the sessions in the list
const LIST_HEADER: usize = 2;
/// Seconds between two reads of the zellij sessions by the list
const SCAN_INTERVAL: f64 = 2.0;
/// The sessions of each zellij session (<name>.json), shared by the instances
const SESSIONS_DIR: &str = "/cache/sessions";

#[derive(Default)]
struct State {
    list: bool,
    permitted: bool,
    /// Background: the name of its zellij session
    zellij_session: String,
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    /// Background: the sessions by id, the last name requested by tab
    sessions: BTreeMap<String, Session>,
    tab_names: HashMap<usize, String>,
    /// List: the sessions of its zellij session (received), the zellij
    /// sessions, the sessions shown and the selected one
    current: Vec<Session>,
    zellij_sessions: Vec<SessionInfo>,
    shown: Vec<Session>,
    selected: usize,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.list = configuration.get("view").is_some_and(|view| view == "list");

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::MessageAndLaunchOtherPlugins,
        ]);

        let mut events = vec![EventType::PermissionRequestResult, EventType::TabUpdate, EventType::PaneUpdate];
        if self.list {
            events.extend([EventType::Key, EventType::Mouse, EventType::SessionUpdate, EventType::Timer]);
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
                    let size = |value: &str| Some(value.to_string());
                    if let Some(coordinates) =
                        FloatingPaneCoordinates::new(size("10%"), size("20%"), size("80%"), size("60%"), None, None)
                    {
                        change_floating_panes_coordinates(vec![(PaneId::Plugin(id), coordinates)]);
                    }
                    pipe_message_to_plugin(MessageToPlugin::new(SYNC_PIPE));
                    self.scan();
                } else {
                    self.broadcast();
                }
                true
            },
            Event::TabUpdate(tabs) => {
                self.tabs = tabs;
                self.refresh(false);
                false
            },
            Event::PaneUpdate(panes) => {
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
            Event::Timer(_) if self.list => {
                self.scan();
                true
            },
            Event::Key(key) if self.list => self.key(key),
            Event::Mouse(Mouse::LeftClick(line, _)) if self.list => {
                let row = line.max(0) as usize;
                if row >= LIST_HEADER && row - LIST_HEADER < self.shown.len() {
                    self.selected = row - LIST_HEADER;
                    self.jump();
                }
                true
            },
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
            self.selected = self.selected.min(self.shown.len().saturating_sub(1));
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
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
    }

    fn key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Down | BareKey::Char('j') if !self.shown.is_empty() => {
                self.selected = (self.selected + 1) % self.shown.len();
            },
            BareKey::Up | BareKey::Char('k') if !self.shown.is_empty() => {
                self.selected = (self.selected + self.shown.len() - 1) % self.shown.len();
            },
            BareKey::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if index < self.shown.len() {
                    self.selected = index;
                    self.jump();
                }
            },
            BareKey::Enter => self.jump(),
            BareKey::Esc | BareKey::Char('q') => close_self(),
            _ => return false,
        }
        true
    }

    fn jump(&self) {
        let Some(session) = self.shown.get(self.selected) else { return };
        let current = self.zellij_sessions.iter().find(|info| info.is_current_session);
        if session.zellij_session.is_empty() || current.is_none_or(|info| info.name == session.zellij_session) {
            focus_pane_with_id(PaneId::Terminal(session.pane_id), false, false);
        } else {
            switch_session_with_focus(&session.zellij_session, session.tab_position, Some((session.pane_id, false)));
        }
        close_self();
    }

    fn render_list(&self, rows: usize, cols: usize) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();

        println!(
            "\u{1b}[1m{} session{}\u{1b}[0m \u{1b}[2m· ↑↓ or 1-9 to jump · Esc\u{1b}[0m",
            self.shown.len(),
            if self.shown.len() == 1 { "" } else { "s" }
        );
        println!();

        if self.shown.is_empty() {
            println!("\u{1b}[2mNo Claude Code session\u{1b}[0m");
            return;
        }

        let project_width = self.shown.iter().map(|s| s.project().chars().count()).max().unwrap_or(0).min(20);
        // The zellij session, when there are several
        let several = self.shown.iter().any(|s| s.zellij_session != self.shown[0].zellij_session);
        let zellij_width = if several {
            self.shown.iter().map(|s| s.zellij_session.chars().count()).max().unwrap_or(0).min(20)
        } else {
            0
        };

        for (index, session) in self.shown.iter().enumerate().take(rows.saturating_sub(LIST_HEADER)) {
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
            let age = if session.since > 0 && now >= session.since { age(now - session.since) } else { String::new() };

            // The topic takes the room left by the other columns
            let zellij = if several {
                format!("{:<zellij_width$}  ", truncate(&session.zellij_session, zellij_width))
            } else {
                String::new()
            };
            let left =
                format!("{} {zellij}{:<project_width$}  ", index + 1, truncate(session.project(), project_width));
            let right = format!("  {}  {:>3}", truncate(&detail, 24), age);
            let room = cols.saturating_sub(2 + left.chars().count() + right.chars().count());
            let topic = truncate(session.title(), room);
            let space = room - topic.chars().count();
            let line = truncate(&format!("{left}{topic}{:space$}{right}", ""), cols.saturating_sub(2));

            let selected = if index == self.selected { "\u{1b}[7m" } else { "" };
            println!("{selected}\u{1b}[{color}m{}\u{1b}[39m {line}\u{1b}[0m", session.status.icon());
        }
    }
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

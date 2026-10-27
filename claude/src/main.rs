//! Claude Code sessions in zellij.
//!
//! Loaded twice:
//! - in the background (load_plugins): follows the sessions from the events
//!   of the Claude Code hook, names their tabs after the conversation and
//!   sends the sessions to the other plugins;
//! - with `view "list"` (floating, from a key binding): lists the sessions,
//!   Enter or a click jumps to one.

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use common::{
    EVENT_PIPE, NAME_MARKER, STATE_PIPE, SYNC_PIPE, Session, Status, is_renamable, topic_from_title, truncate,
};
use serde_json::Value;
use zellij_tile::prelude::*;

/// Longest conversation topic in a tab name
const TAB_TOPIC_LENGTH: usize = 30;
/// Lines above the sessions in the list
const LIST_HEADER: usize = 2;

#[derive(Default)]
struct State {
    list: bool,
    permitted: bool,
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    /// Background: the sessions by id, the last name requested by tab
    sessions: BTreeMap<String, Session>,
    tab_names: HashMap<usize, String>,
    /// List: the sessions received, the selected one
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
            events.extend([EventType::Key, EventType::Mouse]);
        } else {
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
                    self.shown = sessions;
                    self.selected = self.selected.min(self.shown.len().saturating_sub(1));
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
            "SessionStart" | "Stop" => (Status::Idle, None, None),
            "UserPromptSubmit" | "PostToolUse" => (Status::Working, None, None),
            "PreToolUse" => (Status::Working, input["tool_name"].as_str(), None),
            "PermissionRequest" => (Status::Waiting, input["tool_name"].as_str(), None),
            // Waiting for the next prompt is not waiting for an answer
            "Notification" if input["notification_type"] == "idle_prompt" => return,
            "Notification" => (Status::Waiting, None, input["message"].as_str()),
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

    /// The sessions to all the plugins, in the order of the tabs
    fn broadcast(&self) {
        if !self.permitted {
            return;
        }
        let mut sessions: Vec<&Session> = self.sessions.values().collect();
        sessions.sort_by_key(|session| (session.tab_position, session.pane_id));
        if let Ok(payload) = serde_json::to_string(&sessions) {
            pipe_message_to_plugin(MessageToPlugin::new(STATE_PIPE).with_payload(payload));
        }
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
        if let Some(session) = self.shown.get(self.selected) {
            focus_pane_with_id(PaneId::Terminal(session.pane_id), false, false);
            close_self();
        }
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
            println!("\u{1b}[2mNo Claude Code session in this zellij session\u{1b}[0m");
            return;
        }

        let project_width = self.shown.iter().map(|s| s.project().chars().count()).max().unwrap_or(0).min(20);

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
            let left = format!("{} {:<project_width$}  ", index + 1, truncate(session.project(), project_width));
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

fn age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

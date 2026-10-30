//! Claude Code sessions in zellij, and the side panel.
//!
//! Loaded:
//! - in the background (load_plugins): follows the sessions from the events
//!   of the Claude Code hook, names their tabs after the conversation and
//!   sends the sessions to the other plugins. It opens the side panel of the
//!   active tab on request (`panel::open`: a key binding, a button of the
//!   bar), docked on the right or floating like the last one of the tab, or
//!   shows the requested view of the one already open;
//! - with `view "panel"` (opened by the background instance): the side
//!   panel, its tabs Git (the changes of the focused pane) and Claude (the
//!   sessions of all the zellij sessions, then the previous conversations).
//!
//! The background instance of each zellij session writes its sessions in the
//! cache shared by the instances of the plugin; in a zellij session without
//! it, the panes of Claude Code are found by their title. It also writes
//! there the projects directory of Claude Code (from the hook), where the
//! panel reads the transcripts of the previous conversations.
//!
//! Configuration of the background instance: `url` (its own, to open the
//! panel), `docked "false"` to open the panel floating at first.

mod frame;
mod git;
mod list;
mod panel;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{
    EVENT_PIPE, HELLO_PIPE, NAME_MARKER, OPEN_PIPE, SHOW_PIPE, STATE_PIPE, SYNC_PIPE, Session, Status, is_renamable,
    project_folder, topic_from_title, truncate,
};
use serde_json::Value;
use zellij_tile::prelude::*;

use crate::panel::Panel;

/// Longest conversation topic in a tab name
const TAB_TOPIC_LENGTH: usize = 30;
/// The sessions of each zellij session (<name>.json), shared by the instances
const SESSIONS_DIR: &str = "/cache/sessions";
/// The projects directory of Claude Code, shared by the instances
const PROJECTS_FILE: &str = "/cache/projects";
/// A panel opening: not opened again meanwhile
const OPENING: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    /// The side panel, else the background instance
    panel: Option<Panel>,
    permitted: bool,
    /// Background: its URL, the panel docked at first, the name of its zellij
    /// session, the projects directory of Claude Code
    url: Option<String>,
    docked: bool,
    zellij_session: String,
    projects: String,
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    /// Background: the sessions by id, the last name requested by tab
    sessions: BTreeMap<String, Session>,
    tab_names: HashMap<usize, String>,
    /// Background: the panels (their plugin), floating or not by tab (the
    /// last one), the one opening (and when)
    panels: HashSet<u32>,
    floating: HashMap<usize, bool>,
    opening: Option<SystemTime>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        // `list`: the key bindings of the versions before the panel
        if configuration.get("view").is_some_and(|view| view == "panel" || view == "list") {
            self.panel = Some(Panel::new(&configuration));
        }
        self.url = configuration.get("url").cloned();
        self.docked = configuration.get("docked").is_none_or(|docked| docked != "false");

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::MessageAndLaunchOtherPlugins,
            // The panel: the transcripts, claude --resume, git; the
            // background: zellij action new-pane
            PermissionType::FullHdAccess,
            PermissionType::RunCommands,
        ]);

        let mut events = vec![EventType::PermissionRequestResult];
        match &self.panel {
            Some(_) => events.extend(Panel::events()),
            None => {
                events.extend([EventType::TabUpdate, EventType::PaneUpdate, EventType::SessionUpdate]);
                set_selectable(false);
            },
        }
        subscribe(&events);
    }

    fn update(&mut self, event: Event) -> bool {
        if let Event::PermissionRequestResult(PermissionStatus::Granted) = event {
            self.permitted = true;
            match &mut self.panel {
                Some(panel) => panel.start(),
                None => self.broadcast(),
            }
            return true;
        }
        if let Some(panel) = &mut self.panel {
            return panel.update(event);
        }
        match event {
            Event::TabUpdate(tabs) => {
                self.tabs = tabs;
                self.refresh(false);
            },
            Event::PaneUpdate(panes) => {
                self.panes = panes;
                self.follow_panels();
                self.refresh(false);
            },
            Event::SessionUpdate(zellij_sessions, _) => {
                let name = zellij_sessions.iter().find(|info| info.is_current_session).map(|info| info.name.clone());
                if let Some(name) = name.filter(|name| *name != self.zellij_session) {
                    // Renamed: the file of the previous name goes
                    if !self.zellij_session.is_empty() {
                        let _ = fs::remove_file(session_file(&self.zellij_session));
                    }
                    self.zellij_session = name;
                    self.broadcast();
                }
            },
            _ => {},
        }
        false
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if let Some(panel) = &mut self.panel {
            return panel.pipe(&message);
        }
        match message.name.as_str() {
            EVENT_PIPE => self.event(&message),
            SYNC_PIPE => self.broadcast(),
            HELLO_PIPE => {
                if let PipeSource::Plugin(id) = message.source {
                    self.panels.insert(id);
                    self.opening = None;
                }
            },
            OPEN_PIPE => self.open_panel(message.payload.as_deref().unwrap_or("git")),
            _ => {},
        }
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if let Some(panel) = &mut self.panel {
            panel.render(rows, cols);
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

        // For the previous conversations of the panel
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
            // The one claude --resume looks in: of its projects folder
            let folder =
                input["transcript_path"].as_str().and_then(|path| Path::new(path).parent()?.file_name()?.to_str());
            if session.directory.is_empty() || folder.is_some_and(|folder| project_folder(cwd) == folder) {
                session.directory = cwd.to_string();
            }
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

    /// The panels still open, floating or not in their tab
    fn follow_panels(&mut self) {
        let mut open = HashSet::new();
        for (position, panes) in &self.panes.panes {
            let tab_id = self.tabs.iter().find(|tab| tab.position == *position).map(|tab| tab.tab_id);
            for pane in panes.iter().filter(|pane| pane.is_plugin && self.panels.contains(&pane.id)) {
                open.insert(pane.id);
                if let Some(tab_id) = tab_id {
                    self.floating.insert(tab_id, pane.is_floating);
                }
            }
        }
        // Known before their pane: kept while opening
        if self.opening.is_none() {
            self.panels.retain(|id| open.contains(id));
        }
    }

    /// The panel of the active tab showing this view (git or claude): the
    /// one open, else a new one, docked on the right (of the largest pane)
    /// or floating like the last one of the tab
    fn open_panel(&mut self, view: &str) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.active) else { return };
        let panes = self.panes.panes.get(&tab.position).map(Vec::as_slice).unwrap_or_default();
        if let Some(panel) = panes.iter().find(|pane| pane.is_plugin && self.panels.contains(&pane.id)) {
            let message = MessageToPlugin::new(SHOW_PIPE).with_destination_plugin_id(panel.id).with_payload(view);
            pipe_message_to_plugin(message);
            return;
        }
        if self.opening.is_some_and(|time| time.elapsed().is_ok_and(|e| e < OPENING)) {
            return;
        }
        let Some(url) = self.url.clone() else { return };

        // One panel by tab: its own configuration
        let configuration = format!("view=panel,show={view},tab={}", tab.tab_id);
        let floating = self.floating.get(&tab.tab_id).copied().unwrap_or(!self.docked);
        let [x, y, width, height] = frame::FLOATING;
        let command = if floating {
            let mut command = vec!["zellij", "action", "new-pane", "--floating", "--plugin", &url];
            command.extend([
                "--configuration",
                &configuration,
                "--x",
                x,
                "--y",
                y,
                "--width",
                width,
                "--height",
                height,
            ]);
            command
        } else {
            // On the right of the tallest pane (the rightmost of them): a
            // pane opened there, replaced by the panel (a plugin pane cannot
            // be opened in a direction)
            let largest = panes
                .iter()
                .filter(|pane| !pane.is_plugin && !pane.is_floating && pane.is_selectable)
                .max_by_key(|pane| (pane.pane_rows, pane.pane_x));
            if let Some(pane) = largest {
                focus_pane_with_id(PaneId::Terminal(pane.id), false, false);
            }
            let script = "zellij action new-pane --direction right -- sleep 10 > /dev/null \
                && zellij action launch-or-focus-plugin --in-place --close-replaced-pane --configuration \"$2\" \"$1\"";
            vec!["sh", "-c", script, "sh", &url, &configuration]
        };
        self.opening = Some(SystemTime::now());
        let env = BTreeMap::from([("ZELLIJ_SESSION_NAME".to_string(), self.zellij_session.clone())]);
        run_command_with_env_variables_and_cwd(&command, env, PathBuf::from("/"), BTreeMap::new());
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

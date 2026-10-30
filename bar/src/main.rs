//! Status bar, one line: the mode, the session and the tabs on the left, the
//! Git and Claude Code buttons, a hint and the time on the right. Or split in
//! two bars: the tabs (`view "tabs"`), the rest (`view "status"`).
//!
//! A click on a tab goes to it, a double click renames it (like the
//! RenameTab mode: Enter keeps the new name, Esc restores the previous one),
//! a click on the ✕ of the active tab asks to confirm (a second click within
//! a few seconds) and closes it. A click on the Git button opens the git
//! panel, on the Claude Code button (✳ and the sessions by status) their
//! list, like their key bindings. A double click on the session renames it
//! (Enter keeps the new name, Esc cancels).
//!
//! The directory of the Claude Code session of the focused pane (the one to
//! resume it in), on the left.
//!
//! The runtimes of the focused pane (mise: node, python, ruby), sent by its
//! shell: `zellij pipe --name bar::runtimes --args
//! pane_id=$ZELLIJ_PANE_ID,time=$EPOCHREALTIME -- "🤖 25.9.0 🐍 3.14.4"`, on
//! the right.
//!
//! The tabs of the panels (git, claude) show one in place of the other
//! through the bar of their tab, which knows their URLs.
//!
//! Configuration (all optional):
//! - `view`: `tabs` or `status` for a split bar, else both
//! - `timezone`: of the time (e.g. Europe/Zurich), else no time
//! - `hint`: text before the time
//! - `git`, `claude`: URL of the git and claude plugins, for their buttons
//! - `git_docked`, `claude_docked`: `true` when their key bindings start them
//!   docked (the same configuration: the same panel)

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use chrono_tz::Tz;
use common::{
    HELLO_PIPE, MODE_PIPE, NAME_MARKER, STATE_PIPE, SWITCH_PIPE, SYNC_PIPE, Session, Status, short_directory,
};
use zellij_tile::prelude::*;

/// Two clicks on the same tab within this delay rename it
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// To the bars of the other tabs (each tab has its own): the position of the
/// clicked tab
const CLICK_PIPE: &str = "bar::click";
/// From the shells: the runtimes of their pane
const RUNTIMES_PIPE: &str = "bar::runtimes";
/// A panel launched in place of another one asks how this long after
const SWITCH_ANSWER: Duration = Duration::from_secs(5);
/// Longest directory of a Claude Code session
const MAX_DIRECTORY: usize = 60;
/// Delay to confirm the close of a tab
const CONFIRM_CLOSE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy)]
enum Color {
    Rgb(u8, u8, u8),
    /// Of the palette of the terminal (0-15)
    Ansi(u8),
}
use Color::{Ansi, Rgb};

// Nord, the colors of the plivox theme (the tabs)
const POLAR0: Color = Rgb(0x2E, 0x34, 0x40);
const POLAR1: Color = Rgb(0x3B, 0x42, 0x52);
const POLAR3: Color = Rgb(0x4C, 0x56, 0x6A);
const COMMENT: Color = Rgb(0x61, 0x6E, 0x88);
const SNOW: Color = Rgb(0xD8, 0xDE, 0xE9);
const FROST: Color = Rgb(0x88, 0xC0, 0xD0);
const RED: Color = Rgb(0xBF, 0x61, 0x6A);
const ORANGE: Color = Rgb(0xD0, 0x87, 0x70);
const YELLOW: Color = Rgb(0xEB, 0xCB, 0x8B);
const GREEN: Color = Rgb(0xA3, 0xBE, 0x8C);
// The palette of the terminal, the colors of the prompt of zsh (the status)
const TERM_RED: Color = Ansi(1);
const TERM_GREEN: Color = Ansi(2);
const TERM_YELLOW: Color = Ansi(3);
const TERM_BLUE: Color = Ansi(4);
const TERM_MAGENTA: Color = Ansi(5);
const TERM_CYAN: Color = Ansi(6);
const TERM_GRAY: Color = Ansi(8);

#[derive(Default, PartialEq)]
enum View {
    #[default]
    All,
    Tabs,
    Status,
}

#[derive(Default)]
struct State {
    permitted: bool,
    view: View,
    timezone: Option<Tz>,
    hint: String,
    git_url: Option<String>,
    git_docked: bool,
    claude_url: Option<String>,
    claude_docked: bool,

    mode: InputMode,
    session: String,
    tabs: Vec<TabInfo>,
    sessions: Vec<Session>,
    /// The position of the tab of this bar
    tab: Option<usize>,
    /// The runtimes of the terminal panes (and when they were sent), the last
    /// focused one of the tab
    runtimes: HashMap<u32, (f64, String)>,
    terminal: Option<u32>,

    /// Clickable ranges of the last render: (first column, end column, target)
    targets: Vec<(usize, usize, Target)>,
    last_click: Option<(usize, SystemTime)>,
    /// A rename started by a double click
    renaming: bool,
    /// The last click on the session; its new name typed, the pane focused
    /// before
    session_click: Option<SystemTime>,
    /// The panel launched in place of another one: floating like it, and
    /// when
    switched: Option<(String, bool, SystemTime)>,
    session_name: Option<String>,
    focused_before: Option<PaneId>,
    /// The tab to close on a second click on its button, until this time
    close: Option<(usize, SystemTime)>,
    /// Unix time of the next minute, when the clock timer fires
    clock_at: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    /// Position of a tab
    Tab(usize),
    /// Id of the tab to close
    Close(usize),
    Session,
    Git,
    Claude,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.timezone = configuration.get("timezone").and_then(|tz| tz.parse().ok());
        self.hint = configuration.get("hint").cloned().unwrap_or_default();
        self.view = match configuration.get("view").map(String::as_str) {
            Some("tabs") => View::Tabs,
            Some("status") => View::Status,
            _ => View::All,
        };
        let docked = |name: &str| configuration.get(name).is_some_and(|docked| docked == "true");
        self.git_url = configuration.get("git").cloned();
        self.git_docked = docked("git_docked");
        self.claude_url = configuration.get("claude").cloned();
        self.claude_docked = docked("claude_docked");

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
            // The buttons: zellij action launch-or-focus-plugin
            PermissionType::RunCommands,
            // The runtimes sent by the shells
            PermissionType::ReadCliPipes,
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::Mouse,
            EventType::Key,
            EventType::Timer,
        ]);
        set_selectable(false);
        self.schedule_clock();
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.permitted = true;
                pipe_message_to_plugin(MessageToPlugin::new(SYNC_PIPE));
                false
            },
            Event::ModeUpdate(mode_info) => {
                // Esc in the RenameTab mode goes to the Tab mode: back to
                // Normal after a rename started from the bar
                if self.renaming && mode_info.mode != InputMode::RenameTab {
                    self.renaming = false;
                    if mode_info.mode == InputMode::Tab {
                        switch_to_input_mode(&InputMode::Normal);
                    }
                }
                let session = mode_info.session_name.unwrap_or_default();
                let changed = self.mode != mode_info.mode || self.session != session;
                self.mode = mode_info.mode;
                self.session = session;
                changed
            },
            Event::TabUpdate(tabs) => {
                let changed = self.tabs != tabs;
                self.tabs = tabs;
                changed
            },
            Event::PaneUpdate(manifest) => {
                let id = get_plugin_ids().plugin_id;
                self.tab = manifest
                    .panes
                    .iter()
                    .find(|(_, panes)| panes.iter().any(|p| p.is_plugin && p.id == id))
                    .map(|(tab, _)| *tab);
                let terminals = || manifest.panes.values().flatten().filter(|p| !p.is_plugin);
                self.runtimes.retain(|id, _| terminals().any(|p| p.id == *id));
                // The focused one of its tab, floating first (when shown)
                let shown = self.tabs.iter().any(|t| Some(t.position) == self.tab && t.are_floating_panes_visible);
                let focused = self.tab.and_then(|tab| manifest.panes.get(&tab)).and_then(|panes| {
                    let focused = |floating: bool| {
                        panes.iter().find(|p| !p.is_plugin && p.is_focused && p.is_floating == floating).map(|p| p.id)
                    };
                    shown.then(|| focused(true)).flatten().or_else(|| focused(false))
                });
                let changed = focused.is_some() && focused != self.terminal;
                self.terminal = focused.or(self.terminal);
                changed
            },
            Event::Key(key) => self.session_key(key),
            Event::Mouse(Mouse::LeftClick(_, column)) => self.click(column),
            Event::Mouse(Mouse::ScrollUp(_)) => {
                go_to_previous_tab();
                false
            },
            Event::Mouse(Mouse::ScrollDown(_)) => {
                go_to_next_tab();
                false
            },
            Event::Timer(_) => {
                // The close not confirmed in time
                if self.close.is_some_and(|(_, until)| SystemTime::now() >= until) {
                    self.close = None;
                }
                // Not again for the timer of a close
                if now() + 1 >= self.clock_at {
                    self.schedule_clock();
                }
                true
            },
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        match message.name.as_str() {
            STATE_PIPE => match message.payload.as_deref().and_then(|p| serde_json::from_str(p).ok()) {
                Some(sessions) if sessions != self.sessions => {
                    self.sessions = sessions;
                    true
                },
                _ => false,
            },
            // From a panel of the tab of this bar: the other in its place
            SWITCH_PIPE
                if self.tab.is_some() && self.tab == self.tabs.iter().find(|t| t.active).map(|t| t.position) =>
            {
                let (url, configuration) = match message.payload.as_deref() {
                    Some("git") => (self.git_url.as_deref(), self.git_config()),
                    Some("claude") => (self.claude_url.as_deref(), self.claude_config()),
                    _ => return false,
                };
                if url.is_some() {
                    let floating = message.args.get("floating").is_some_and(|f| f == "true");
                    let name = message.payload.clone().unwrap_or_default();
                    self.switched = Some((name, floating, SystemTime::now()));
                }
                self.launch(url, &configuration, true);
                false
            },
            // A panel starting: floating if launched in place of a floating
            // one
            HELLO_PIPE => {
                let PipeSource::Plugin(id) = message.source else {
                    return false;
                };
                let recent = |time: &SystemTime| time.elapsed().is_ok_and(|e| e < SWITCH_ANSWER);
                if let Some((_, floating, _)) = self
                    .switched
                    .take_if(|(name, _, time)| message.payload.as_deref() == Some(name.as_str()) && recent(time))
                {
                    let mode = if floating { "floating" } else { "docked" };
                    pipe_message_to_plugin(
                        MessageToPlugin::new(MODE_PIPE).with_destination_plugin_id(id).with_payload(mode),
                    );
                }
                false
            },
            RUNTIMES_PIPE => {
                // The zellij pipe of the shell waits for it
                if let PipeSource::Cli(pipe) = &message.source {
                    unblock_cli_pipe_input(pipe);
                }
                // Its end of input comes without payload
                let (Some(id), Some(payload)) =
                    (message.args.get("pane_id").and_then(|id| id.parse().ok()), message.payload)
                else {
                    return false;
                };
                // The shell sends them in the background: the latest kept
                let time = message.args.get("time").and_then(|t| t.parse().ok()).unwrap_or_default();
                if self.runtimes.get(&id).is_some_and(|(last, _)| *last > time) {
                    return false;
                }
                let runtimes = payload.trim().to_string();
                let changed = self.runtimes.get(&id).is_none_or(|(_, last)| *last != runtimes);
                self.runtimes.insert(id, (time, runtimes));
                changed && self.terminal == Some(id)
            },
            CLICK_PIPE => {
                if let Some(position) = message.payload.as_deref().and_then(|p| p.parse().ok()) {
                    self.last_click = Some((position, SystemTime::now()));
                }
                false
            },
            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        self.targets.clear();

        let status = self.view != View::Tabs;
        let mut left = Line::default();
        let mut buttons = vec![];
        if status {
            // Like the prompt of zsh: the colors of the terminal, no
            // background
            left.push(&format!(" {} ", mode_name(self.mode)), Some(mode_term_color(self.mode)), None, true);
            let start = left.width;
            match &self.session_name {
                Some(name) => left.push(&format!(" {name}▏"), Some(POLAR0), Some(YELLOW), true),
                None => left.push(&format!(" {} ", self.session), Some(TERM_YELLOW), None, false),
            }
            self.targets.push((start, left.width, Target::Session));
            left.push(" ", None, None, false);
            // The focused pane runs Claude Code: the directory to resume it in
            if let Some(directory) = self.claude_directory() {
                left.push(" ✳ ", Some(ORANGE), None, true);
                left.push(&format!("{directory} "), Some(TERM_CYAN), None, false);
            }
        }

        let mut right = Line::default();
        if status && self.git_url.is_some() {
            let start = right.width;
            right.push("⎇ git", Some(TERM_BLUE), None, false);
            buttons.push((start, right.width, Target::Git));
            right.push("  ", None, None, false);
        }
        if status {
            if let Some((start, end)) = self.claude_summary(&mut right) {
                buttons.push((start, end, Target::Claude));
            }
            self.runtimes_line(&mut right);
            if !self.hint.is_empty() {
                right.push(&format!("{}  ", self.hint), Some(TERM_GRAY), None, false);
            }
            if let Some(time) = self.time() {
                right.push(&format!("{time} "), Some(TERM_GRAY), None, false);
            }
        }

        if self.view != View::Status {
            let room = cols.saturating_sub(left.width + right.width);
            let tabs = self.tabs_line(left.width, room);
            left.append(tabs);
        }

        let space = cols.saturating_sub(left.width + right.width);
        let offset = left.width + space;
        self.targets.extend(buttons.into_iter().map(|(start, end, target)| (offset + start, offset + end, target)));
        print!("{}{:space$}{}", left.text, "", right.text);
    }
}

impl State {
    fn schedule_clock(&mut self) {
        // At the next minute
        let seconds = now();
        self.clock_at = seconds - seconds % 60 + 60;
        set_timeout((self.clock_at - seconds) as f64);
    }

    fn time(&self) -> Option<String> {
        let tz = self.timezone?;
        Some(Utc::now().with_timezone(&tz).format("%H:%M").to_string())
    }

    /// The Claude Code button: ✳ and the count of the sessions by status,
    /// the ones that need attention first; returns its columns in the line.
    fn claude_summary(&self, line: &mut Line) -> Option<(usize, usize)> {
        self.claude_url.as_ref()?;
        let start = line.width;
        line.push("✳", Some(ORANGE), None, true);
        if self.sessions.is_empty() {
            line.push(" claude", Some(TERM_BLUE), None, false);
        }
        for status in [Status::Waiting, Status::Working, Status::Idle] {
            let count = self.sessions.iter().filter(|s| s.status == status).count();
            if count > 0 {
                let text = format!(" {} {count}", status.icon());
                line.push(&text, Some(status_term_color(status)), None, status == Status::Waiting);
            }
        }
        let end = line.width;
        line.push("  ", None, None, false);
        Some((start, end))
    }

    /// The directory to resume the Claude Code session of the focused pane
    /// in (the start left out if too long)
    fn claude_directory(&self) -> Option<String> {
        let terminal = self.terminal?;
        // The sessions of this zellij session
        let session = self.sessions.iter().find(|s| s.pane_id == terminal)?;
        let directory = short_directory(if session.directory.is_empty() { &session.cwd } else { &session.directory });
        let length = directory.chars().count();
        Some(if length > MAX_DIRECTORY {
            format!("…{}", directory.chars().skip(length + 1 - MAX_DIRECTORY).collect::<String>())
        } else {
            directory
        })
    }

    /// The runtimes of the focused pane: the versions in green, a virtual
    /// environment in yellow
    fn runtimes_line(&self, line: &mut Line) {
        let Some(runtimes) =
            self.terminal.and_then(|id| self.runtimes.get(&id)).map(|(_, r)| r).filter(|r| !r.is_empty())
        else {
            return;
        };
        for (i, word) in runtimes.split_whitespace().enumerate() {
            if i > 0 {
                line.push(" ", None, None, false);
            }
            if !word.starts_with(|c: char| c.is_ascii_alphanumeric()) {
                line.push(word, None, None, false);
                continue;
            }
            let (version, environment) = word.split_at(word.find('(').unwrap_or(word.len()));
            line.push(version, Some(TERM_GREEN), None, false);
            if !environment.is_empty() {
                line.push(environment, Some(TERM_YELLOW), None, false);
            }
        }
        line.push("  ", None, None, false);
    }

    /// The tabs as pills, in `room` columns starting at `offset`: the names
    /// are shortened when they do not fit.
    fn tabs_line(&mut self, offset: usize, room: usize) -> Line {
        let statuses = self.tab_statuses();
        let mut max_name = 40;
        loop {
            let mut line = Line::default();
            let mut targets = vec![];
            for tab in &self.tabs {
                let start = line.width;
                self.tab_pill(&mut line, tab, statuses.get(&tab.tab_id).copied(), max_name);
                targets.push((offset + start, offset + line.width, Target::Tab(tab.position)));
                // The close button of the active tab, at the end of its pill
                if tab.active && self.mode != InputMode::RenameTab {
                    let start = line.width;
                    if self.close.is_some_and(|(tab_id, _)| tab_id == tab.tab_id) {
                        line.push(" close? ", Some(POLAR0), Some(RED), true);
                    } else {
                        line.push("✕ ", Some(POLAR3), Some(FROST), true);
                    }
                    targets.push((offset + start, offset + line.width, Target::Close(tab.tab_id)));
                }
                line.push(" ", None, None, false);
            }
            if line.width <= room || max_name <= 4 {
                self.targets.extend(targets);
                return line;
            }
            max_name -= 2;
        }
    }

    fn tab_pill(&self, line: &mut Line, tab: &TabInfo, status: Option<Status>, max_name: usize) {
        let index = tab.position + 1;
        let renaming = tab.active && self.mode == InputMode::RenameTab;
        let mut name = tab.name.trim_end_matches(NAME_MARKER).to_string();
        if !renaming {
            name = common::truncate(&name, max_name);
        }

        // The status icon set by the claude plugin, in color
        let icon = status.filter(|status| name.starts_with(status.icon()));
        if let Some(status) = icon {
            name = name[status.icon().len()..].to_string();
        }

        let mut indicators = String::new();
        if tab.are_floating_panes_visible {
            indicators.push_str(" ⧉");
        }
        if tab.is_fullscreen_active {
            indicators.push_str(" ⤢");
        }
        if tab.is_sync_panes_active {
            indicators.push_str(" ⇄");
        }

        let (bg, fg, index_fg, bold) = if renaming {
            (YELLOW, POLAR0, POLAR0, true)
        } else if tab.active {
            (FROST, POLAR0, POLAR0, true)
        } else {
            (POLAR1, SNOW, COMMENT, false)
        };

        line.push(&format!(" {index} "), Some(index_fg), Some(bg), bold);
        if let Some(status) = icon {
            let color = if tab.active || renaming { POLAR0 } else { status_color(status) };
            line.push(status.icon(), Some(color), Some(bg), true);
        }
        line.push(&format!("{name}{indicators} "), Some(fg), Some(bg), bold);
    }

    /// The status of the tabs with a Claude Code session, the one that needs
    /// attention first
    fn tab_statuses(&self) -> HashMap<usize, Status> {
        let mut statuses: HashMap<usize, Status> = HashMap::new();
        for session in &self.sessions {
            if let Some(tab_id) = session.tab_id {
                let status = statuses.entry(tab_id).or_insert(session.status);
                *status = (*status).max(session.status);
            }
        }
        statuses
    }

    /// Returns whether to render again
    fn click(&mut self, column: usize) -> bool {
        let target = self.targets.iter().find(|(start, end, _)| (*start..*end).contains(&column)).map(|t| t.2);
        // Any other click cancels a close
        let close = self.close.take();
        let Some(target) = target else { return close.is_some() };
        if target != Target::Session {
            self.session_click = None;
        }
        match target {
            Target::Tab(position) => {
                let now = SystemTime::now();
                let double = self.last_click.is_some_and(|(last, time)| {
                    last == position && now.duration_since(time).is_ok_and(|elapsed| elapsed < DOUBLE_CLICK)
                });
                if double {
                    self.last_click = None;
                    self.rename(position);
                } else {
                    // The second click goes to the bar of the tab switched to
                    self.last_click = Some((position, now));
                    if self.permitted {
                        pipe_message_to_plugin(MessageToPlugin::new(CLICK_PIPE).with_payload(position.to_string()));
                    }
                    switch_tab_to(position as u32 + 1);
                }
            },
            Target::Close(tab_id) => {
                if close.is_some_and(|(id, until)| id == tab_id && SystemTime::now() < until) {
                    close_tab_with_id(tab_id as u64);
                } else {
                    self.close = Some((tab_id, SystemTime::now() + CONFIRM_CLOSE));
                    set_timeout(CONFIRM_CLOSE.as_secs_f64());
                }
            },
            Target::Session => {
                let now = SystemTime::now();
                let double = self
                    .session_click
                    .is_some_and(|time| now.duration_since(time).is_ok_and(|elapsed| elapsed < DOUBLE_CLICK));
                self.session_click = Some(now);
                if double && self.session_name.is_none() {
                    self.session_click = None;
                    self.rename_session();
                }
            },
            Target::Git => self.launch(self.git_url.as_deref(), &self.git_config(), false),
            Target::Claude => self.launch(self.claude_url.as_deref(), &self.claude_config(), false),
        }
        true
    }

    /// Like the RenameTab mode of the Alt R binding: the name is typed from
    /// scratch, Esc restores it
    fn rename(&mut self, position: usize) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.position == position) else { return };
        if !tab.active {
            switch_tab_to(position as u32 + 1);
        }
        self.renaming = true;
        switch_to_input_mode(&InputMode::RenameTab);
        rename_tab_with_id(tab.tab_id as u64, "");
    }

    /// Typed in the bar: focused, the keys come to it
    fn rename_session(&mut self) {
        if !self.permitted {
            return;
        }
        self.focused_before = get_focused_pane_info().ok().map(|(_, pane)| pane);
        self.session_name = Some(String::new());
        set_selectable(true);
        focus_plugin_pane(get_plugin_ids().plugin_id, false, false);
    }

    /// A key of the session rename; returns whether to render again
    fn session_key(&mut self, key: KeyWithModifier) -> bool {
        let Some(name) = &mut self.session_name else { return false };
        match key.bare_key {
            BareKey::Char(c) if key.key_modifiers.is_empty() || key.key_modifiers.contains(&KeyModifier::Shift) => {
                name.push(c)
            },
            BareKey::Backspace => {
                name.pop();
            },
            BareKey::Enter => {
                if !name.trim().is_empty() {
                    rename_session(name.trim());
                }
                self.end_session_rename();
            },
            BareKey::Esc => self.end_session_rename(),
            _ => return false,
        }
        true
    }

    fn end_session_rename(&mut self) {
        self.session_name = None;
        set_selectable(false);
        if let Some(pane) = self.focused_before.take() {
            focus_pane_with_id(pane, false, false);
        }
    }

    /// The configurations of the key bindings of the panels (the same: the
    /// same panel)
    fn git_config(&self) -> String {
        if self.git_docked { "docked=true" } else { "" }.to_string()
    }

    fn claude_config(&self) -> String {
        if self.claude_docked { "view=list,docked=true" } else { "view=list" }.to_string()
    }

    /// Like their key bindings: the panel of the plugin (with this
    /// configuration) launched, or moved to the focused tab; or in place of
    /// the focused pane (the other panel), closed
    fn launch(&self, url: Option<&str>, configuration: &str, in_place: bool) {
        let Some(url) = url else { return };
        if !self.permitted {
            return;
        }
        let mut command = vec!["zellij", "action", "launch-or-focus-plugin"];
        if in_place {
            command.extend(["--in-place", "--close-replaced-pane"]);
        } else {
            command.extend(["--floating", "--move-to-focused-tab"]);
        }
        if !configuration.is_empty() {
            command.extend(["--configuration", configuration]);
        }
        command.push(url);
        let env = BTreeMap::from([("ZELLIJ_SESSION_NAME".to_string(), self.session.clone())]);
        run_command_with_env_variables_and_cwd(&command, env, PathBuf::from("/"), BTreeMap::new());
    }
}

/// Text with colors, and its width in columns
#[derive(Default)]
struct Line {
    text: String,
    width: usize,
}

impl Line {
    fn push(&mut self, text: &str, fg: Option<Color>, bg: Option<Color>, bold: bool) {
        for (color, base) in [(fg, 30), (bg, 40)] {
            match color {
                Some(Rgb(r, g, b)) => self.text.push_str(&format!("\u{1b}[{};2;{r};{g};{b}m", base + 8)),
                Some(Ansi(n)) if n < 8 => self.text.push_str(&format!("\u{1b}[{}m", base + u16::from(n))),
                Some(Ansi(n)) => self.text.push_str(&format!("\u{1b}[{}m", base + 52 + u16::from(n))),
                None => {},
            }
        }
        if bold {
            self.text.push_str("\u{1b}[1m");
        }
        self.text.push_str(text);
        self.text.push_str("\u{1b}[0m");
        self.width += text.chars().map(char_width).sum::<usize>();
    }

    fn append(&mut self, other: Line) {
        self.text.push_str(&other.text);
        self.width += other.width;
    }
}

/// Columns of a character: the emoji take two, the zero-width ones none
fn char_width(c: char) -> usize {
    match c {
        '\u{200b}'..='\u{200d}' | '\u{fe00}'..='\u{fe0f}' => 0,
        '\u{1f300}'..='\u{1faff}' => 2,
        _ => 1,
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

fn mode_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Normal => "NORMAL",
        InputMode::Locked => "LOCKED",
        InputMode::Pane => "PANE",
        InputMode::Tab => "TAB",
        InputMode::Resize => "RESIZE",
        InputMode::Move => "MOVE",
        InputMode::Scroll => "SCROLL",
        InputMode::EnterSearch | InputMode::Search => "SEARCH",
        InputMode::RenameTab | InputMode::RenamePane => "RENAME",
        InputMode::Session => "SESSION",
        InputMode::Prompt => "PROMPT",
        InputMode::Tmux => "TMUX",
    }
}

fn mode_term_color(mode: InputMode) -> Color {
    match mode {
        InputMode::Normal => TERM_BLUE,
        InputMode::Locked => TERM_RED,
        InputMode::Pane | InputMode::RenamePane => TERM_GREEN,
        InputMode::Tab | InputMode::RenameTab | InputMode::Resize | InputMode::Move | InputMode::Tmux => TERM_YELLOW,
        InputMode::Scroll | InputMode::EnterSearch | InputMode::Search => TERM_MAGENTA,
        InputMode::Session | InputMode::Prompt => TERM_CYAN,
    }
}

fn status_term_color(status: Status) -> Color {
    match status {
        Status::Idle => TERM_GREEN,
        Status::Working => TERM_CYAN,
        Status::Waiting => TERM_YELLOW,
    }
}

fn status_color(status: Status) -> Color {
    match status {
        Status::Idle => GREEN,
        Status::Working => FROST,
        Status::Waiting => YELLOW,
    }
}

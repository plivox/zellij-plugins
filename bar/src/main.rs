//! Status bar, one line: the mode, the session and the tabs on the left, the
//! Git and Claude Code buttons, a hint and the time on the right.
//!
//! A click on a tab goes to it, a double click renames it (like the
//! RenameTab mode: Enter keeps the new name, Esc restores the previous one),
//! a click on the ✕ of the active tab asks to confirm (a second click within
//! a few seconds) and closes it. A click on the Git button opens the git
//! panel, on the Claude Code button (✳ and the sessions by status) their
//! list, like their key bindings.
//!
//! Configuration (all optional):
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
use common::{NAME_MARKER, STATE_PIPE, SYNC_PIPE, Session, Status};
use zellij_tile::prelude::*;

/// Two clicks on the same tab within this delay rename it
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// To the bars of the other tabs (each tab has its own): the position of the
/// clicked tab
const CLICK_PIPE: &str = "bar::click";
/// Delay to confirm the close of a tab
const CONFIRM_CLOSE: Duration = Duration::from_secs(3);

// Nord, the colors of the plivox theme
type Rgb = (u8, u8, u8);
const POLAR0: Rgb = (0x2E, 0x34, 0x40);
const POLAR1: Rgb = (0x3B, 0x42, 0x52);
const POLAR3: Rgb = (0x4C, 0x56, 0x6A);
const COMMENT: Rgb = (0x61, 0x6E, 0x88);
const SNOW: Rgb = (0xD8, 0xDE, 0xE9);
const FROST: Rgb = (0x88, 0xC0, 0xD0);
const BLUE: Rgb = (0x81, 0xA1, 0xC1);
const RED: Rgb = (0xBF, 0x61, 0x6A);
const ORANGE: Rgb = (0xD0, 0x87, 0x70);
const YELLOW: Rgb = (0xEB, 0xCB, 0x8B);
const GREEN: Rgb = (0xA3, 0xBE, 0x8C);
const PURPLE: Rgb = (0xB4, 0x8E, 0xAD);

#[derive(Default)]
struct State {
    permitted: bool,
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

    /// Clickable ranges of the last render: (first column, end column, target)
    targets: Vec<(usize, usize, Target)>,
    last_click: Option<(usize, SystemTime)>,
    /// A rename started by a double click
    renaming: bool,
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
    Git,
    Claude,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.timezone = configuration.get("timezone").and_then(|tz| tz.parse().ok());
        self.hint = configuration.get("hint").cloned().unwrap_or_default();
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
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::Mouse,
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

        let mut left = Line::default();
        left.push(&format!(" {} ", mode_name(self.mode)), Some(POLAR0), Some(mode_color(self.mode)), true);
        left.push(&format!(" {}  ", self.session), Some(FROST), None, true);

        let mut right = Line::default();
        let mut buttons = vec![];
        if self.git_url.is_some() {
            let start = right.width;
            right.push(" ⎇ ", Some(FROST), Some(POLAR1), true);
            right.push("Git ", Some(COMMENT), Some(POLAR1), false);
            buttons.push((start, right.width, Target::Git));
            right.push("  ", None, None, false);
        }
        if let Some((start, end)) = self.claude_summary(&mut right) {
            buttons.push((start, end, Target::Claude));
        }
        if !self.hint.is_empty() {
            right.push(&format!("{}  ", self.hint), Some(POLAR3), None, false);
        }
        if let Some(time) = self.time() {
            right.push(&format!("{time} "), Some(POLAR3), None, false);
        }

        let room = cols.saturating_sub(left.width + right.width);
        let tabs = self.tabs_line(left.width, room);
        left.append(tabs);

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
        line.push(" ✳ ", Some(ORANGE), Some(POLAR1), true);
        if self.sessions.is_empty() {
            line.push("Claude ", Some(COMMENT), Some(POLAR1), false);
        }
        for status in [Status::Waiting, Status::Working, Status::Idle] {
            let count = self.sessions.iter().filter(|s| s.status == status).count();
            if count > 0 {
                let text = format!("{} {count} ", status.icon());
                line.push(&text, Some(status_color(status)), Some(POLAR1), status == Status::Waiting);
            }
        }
        let end = line.width;
        line.push("  ", None, None, false);
        Some((start, end))
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
            Target::Git => {
                let config = if self.git_docked { "docked=true" } else { "" };
                self.launch(self.git_url.as_deref(), config);
            },
            Target::Claude => {
                let config = if self.claude_docked { "view=list,docked=true" } else { "view=list" };
                self.launch(self.claude_url.as_deref(), config);
            },
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

    /// Like their key bindings: the panel of the plugin (with this
    /// configuration) launched, or moved to the focused tab
    fn launch(&self, url: Option<&str>, configuration: &str) {
        let Some(url) = url else { return };
        if !self.permitted {
            return;
        }
        let mut command = vec!["zellij", "action", "launch-or-focus-plugin", "--floating", "--move-to-focused-tab"];
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
    fn push(&mut self, text: &str, fg: Option<Rgb>, bg: Option<Rgb>, bold: bool) {
        if let Some((r, g, b)) = fg {
            self.text.push_str(&format!("\u{1b}[38;2;{r};{g};{b}m"));
        }
        if let Some((r, g, b)) = bg {
            self.text.push_str(&format!("\u{1b}[48;2;{r};{g};{b}m"));
        }
        if bold {
            self.text.push_str("\u{1b}[1m");
        }
        self.text.push_str(text);
        self.text.push_str("\u{1b}[0m");
        self.width += text.chars().count();
    }

    fn append(&mut self, other: Line) {
        self.text.push_str(&other.text);
        self.width += other.width;
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

fn mode_color(mode: InputMode) -> Rgb {
    match mode {
        InputMode::Normal => BLUE,
        InputMode::Locked => RED,
        InputMode::Pane | InputMode::RenamePane => GREEN,
        InputMode::Tab | InputMode::RenameTab => YELLOW,
        InputMode::Resize | InputMode::Move | InputMode::Tmux => ORANGE,
        InputMode::Scroll | InputMode::EnterSearch | InputMode::Search => PURPLE,
        InputMode::Session | InputMode::Prompt => FROST,
    }
}

fn status_color(status: Status) -> Rgb {
    match status {
        Status::Idle => GREEN,
        Status::Working => FROST,
        Status::Waiting => YELLOW,
    }
}

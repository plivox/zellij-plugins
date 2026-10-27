//! Status bar, one line: the mode, the session and the tabs on the left, the
//! Claude Code sessions, a hint and the time on the right.
//!
//! A click on a tab goes to it, a double click renames it (like the
//! RenameTab mode: Enter keeps the new name, Esc restores the previous one).
//! A click on the Claude Code sessions opens their list.
//!
//! Configuration (all optional):
//! - `timezone`: of the time (e.g. Europe/Zurich), else no time
//! - `hint`: text before the time
//! - `claude`: URL of the claude plugin, to open its list

use std::collections::{BTreeMap, HashMap};
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
    claude_url: Option<String>,

    mode: InputMode,
    session: String,
    tabs: Vec<TabInfo>,
    sessions: Vec<Session>,

    /// Clickable ranges of the last render: (first column, end column, target)
    targets: Vec<(usize, usize, Target)>,
    last_click: Option<(usize, SystemTime)>,
    /// A rename started by a double click
    renaming: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    /// Position of a tab
    Tab(usize),
    Claude,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.timezone = configuration.get("timezone").and_then(|tz| tz.parse().ok());
        self.hint = configuration.get("hint").cloned().unwrap_or_default();
        self.claude_url = configuration.get("claude").cloned();

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
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
            Event::Mouse(Mouse::LeftClick(_, column)) => {
                self.click(column);
                false
            },
            Event::Mouse(Mouse::ScrollUp(_)) => {
                go_to_previous_tab();
                false
            },
            Event::Mouse(Mouse::ScrollDown(_)) => {
                go_to_next_tab();
                false
            },
            Event::Timer(_) => {
                self.schedule_clock();
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
        let claude = self.claude_summary(&mut right);
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
        if let Some((start, end)) = claude {
            let offset = left.width + space;
            self.targets.push((offset + start, offset + end, Target::Claude));
        }
        print!("{}{:space$}{}", left.text, "", right.text);
    }
}

impl State {
    fn schedule_clock(&self) {
        // At the next minute
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        set_timeout((60 - seconds % 60) as f64);
    }

    fn time(&self) -> Option<String> {
        let tz = self.timezone?;
        Some(Utc::now().with_timezone(&tz).format("%H:%M").to_string())
    }

    /// The count of the Claude Code sessions by status, the ones that need
    /// attention first; returns the columns of the summary in the line.
    fn claude_summary(&self, line: &mut Line) -> Option<(usize, usize)> {
        if self.sessions.is_empty() {
            return None;
        }
        let start = line.width;
        for status in [Status::Waiting, Status::Working, Status::Idle] {
            let count = self.sessions.iter().filter(|s| s.status == status).count();
            if count > 0 {
                let text = format!("{} {count} ", status.icon());
                line.push(&text, Some(status_color(status)), None, status == Status::Waiting);
            }
        }
        let end = line.width;
        line.push(" ", None, None, false);
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

    fn click(&mut self, column: usize) {
        let Some(target) = self.targets.iter().find(|(start, end, _)| (*start..*end).contains(&column)).map(|t| t.2)
        else {
            return;
        };
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
            Target::Claude => self.open_claude_list(),
        }
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

    fn open_claude_list(&self) {
        let Some(url) = &self.claude_url else { return };
        if !self.permitted {
            return;
        }
        let config = BTreeMap::from([("view".to_string(), "list".to_string())]);
        pipe_message_to_plugin(
            MessageToPlugin::new("claude::open")
                .with_plugin_url(url)
                .with_plugin_config(config)
                .new_plugin_instance_should_float(true),
        );
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

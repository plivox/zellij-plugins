//! Git changes of the focused pane, in a floating panel (from a key binding).
//!
//! Follows the working directory of the focused terminal pane and lists its
//! changes: staged, not staged, untracked, in conflict, with the lines added
//! and removed. Enter (or a click) shows the diff of a file, `s` stages or
//! unstages it, `g` opens lazygit, `p` docks the panel on the right (in the
//! tiled panes) or floats it again, Tab (or its tab) shows the list of claude
//! in its place, Esc closes it.
//!
//! Configuration: `docked "true"` to start docked.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use common::panel::{self, Panel};
use common::truncate;
use zellij_tile::prelude::*;

/// Lines above the files
const HEADER: usize = 3;
/// Seconds between two reads of the changes while the panel is visible
const REFRESH_INTERVAL: f64 = 2.0;
/// Floating: on the right
const FLOATING: [&str; 4] = ["55%", "5%", "44%", "90%"];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Conflicts,
    Staged,
    Changes,
    Untracked,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Conflicts => "Conflicts",
            Section::Staged => "Staged",
            Section::Changes => "Changes",
            Section::Untracked => "Untracked",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Section::Conflicts => "35",
            Section::Staged => "32",
            Section::Changes => "31",
            Section::Untracked => "2",
        }
    }
}

struct Entry {
    section: Section,
    /// Status letter of git (M, A, D, R...)
    code: char,
    path: String,
    /// Lines added and removed
    stat: Option<(String, String)>,
}

#[derive(Default)]
struct State {
    permitted: bool,
    visible: bool,
    /// Docked at the start
    docked: bool,
    panel: Panel,
    /// The focused terminal pane and its working directory
    pane: Option<u32>,
    cwd: Option<PathBuf>,
    active_tab: usize,

    /// From git: the branch line, the files, the stats of the files (staged,
    /// not staged), the error
    branch: String,
    entries: Vec<Entry>,
    stats: HashMap<(bool, String), (String, String)>,
    error: Option<String>,

    selected: usize,
    /// The file staged or unstaged, selected again when listed
    follow: Option<String>,
    /// The first line shown, the entry of each line shown
    offset: usize,
    lines: Vec<Option<usize>>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.docked = configuration.get("docked").is_some_and(|docked| docked == "true");
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            // The tabs of the panels, through the bar
            PermissionType::MessageAndLaunchOtherPlugins,
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CwdChanged,
            EventType::RunCommandResult,
            EventType::Visible,
            EventType::Timer,
            EventType::Key,
            EventType::Mouse,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.permitted = true;
                self.visible = true;
                rename_plugin_pane(get_plugin_ids().plugin_id, "Git");
                self.panel = Panel::new("git");
                self.panel.start(self.docked);
                self.follow_cwd();
                set_timeout(REFRESH_INTERVAL);
                true
            },
            Event::TabUpdate(tabs) => {
                if let Some(tab) = tabs.iter().find(|tab| tab.active) {
                    self.active_tab = tab.position;
                }
                false
            },
            Event::PaneUpdate(manifest) => {
                self.panel.update(&manifest, FLOATING);
                let focused = manifest.panes.get(&self.active_tab).and_then(|panes| {
                    panes.iter().find(|pane| !pane.is_plugin && pane.is_focused && !pane.is_floating).map(|p| p.id)
                });
                if focused.is_some() && focused != self.pane {
                    self.pane = focused;
                    self.follow_cwd();
                }
                false
            },
            Event::CwdChanged(PaneId::Terminal(id), cwd, _) if Some(id) == self.pane => {
                self.set_cwd(cwd);
                true
            },
            Event::Visible(visible) => {
                self.visible = visible;
                if visible && self.permitted {
                    self.follow_cwd();
                    set_timeout(REFRESH_INTERVAL);
                }
                false
            },
            Event::Timer(_) => {
                if self.visible {
                    self.refresh();
                    set_timeout(REFRESH_INTERVAL);
                }
                false
            },
            Event::RunCommandResult(code, stdout, stderr, context) => self.result(code, &stdout, &stderr, &context),
            Event::Key(key) => self.key(key),
            Event::Mouse(Mouse::LeftClick(0, column)) => {
                if let Some(name) = panel::tab_at(column).filter(|name| *name != "git") {
                    panel::switch(name);
                }
                false
            },
            Event::Mouse(Mouse::LeftClick(line, _)) => {
                let row = (line.max(0) as usize).checked_sub(HEADER);
                if let Some(index) = row.and_then(|row| self.lines.get(row).copied().flatten()) {
                    self.selected = index;
                    self.diff();
                }
                true
            },
            Event::Mouse(Mouse::ScrollDown(_)) => self.key(KeyWithModifier::new(BareKey::Down)),
            Event::Mouse(Mouse::ScrollUp(_)) => self.key(KeyWithModifier::new(BareKey::Up)),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.render_panel(rows, cols);
    }
}

impl State {
    /// The working directory of the focused pane
    fn follow_cwd(&mut self) {
        if !self.permitted {
            return;
        }
        if let Some(cwd) = self.pane.and_then(|id| get_pane_cwd(PaneId::Terminal(id)).ok()) {
            self.set_cwd(cwd);
        }
    }

    fn set_cwd(&mut self, cwd: PathBuf) {
        if self.cwd.as_ref() != Some(&cwd) {
            self.cwd = Some(cwd);
            self.selected = 0;
            self.offset = 0;
        }
        self.refresh();
    }

    /// Reads the changes: the status, the stats of the staged and not staged
    /// files
    fn refresh(&self) {
        if self.permitted {
            self.git(&["status", "--porcelain=v1", "--branch", "--untracked-files=all", "-z"], "status");
            self.git(&["diff", "--numstat", "-z"], "changes");
            self.git(&["diff", "--numstat", "-z", "--cached"], "staged");
        }
    }

    fn git(&self, args: &[&str], kind: &str) {
        let Some(cwd) = &self.cwd else { return };
        let mut command = vec!["git", "--no-optional-locks"];
        command.extend(args);
        let context = BTreeMap::from([("kind".to_string(), kind.to_string())]);
        run_command_with_env_variables_and_cwd(&command, BTreeMap::new(), cwd.clone(), context);
    }

    fn result(&mut self, code: Option<i32>, stdout: &[u8], stderr: &[u8], context: &BTreeMap<String, String>) -> bool {
        let output = String::from_utf8_lossy(stdout);
        match context.get("kind").map(String::as_str) {
            Some("status") if code != Some(0) => {
                self.error = Some(String::from_utf8_lossy(stderr).lines().next().unwrap_or_default().to_string());
                self.entries.clear();
                self.branch.clear();
            },
            Some("status") => {
                self.error = None;
                self.status(&output);
            },
            Some(kind @ ("changes" | "staged")) if code == Some(0) => {
                let staged = kind == "staged";
                self.stats.retain(|(entry_staged, _), _| *entry_staged != staged);
                self.stats.extend(numstat(&output).into_iter().map(|(path, stat)| ((staged, path), stat)));
                self.apply_stats();
            },
            // After a stage or an unstage
            Some("action") => self.refresh(),
            _ => return false,
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        true
    }

    /// `git status --porcelain=v1 --branch -z`
    fn status(&mut self, output: &str) {
        let mut entries = vec![];
        let mut fields = output.split('\0').filter(|field| !field.is_empty());
        while let Some(field) = fields.next() {
            if let Some(branch) = field.strip_prefix("## ") {
                self.branch = branch.to_string();
                continue;
            }
            let mut chars = field.chars();
            let (Some(x), Some(y)) = (chars.next(), chars.next()) else { continue };
            let path = field.get(3..).unwrap_or_default().to_string();
            // Renamed or copied: the original path follows
            if matches!(x, 'R' | 'C') {
                fields.next();
            }
            let conflict = matches!((x, y), ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D'));
            if conflict {
                entries.push(Entry { section: Section::Conflicts, code: 'U', path, stat: None });
            } else if x == '?' {
                entries.push(Entry { section: Section::Untracked, code: '?', path, stat: None });
            } else {
                if x != ' ' {
                    entries.push(Entry { section: Section::Staged, code: x, path: path.clone(), stat: None });
                }
                if y != ' ' {
                    entries.push(Entry { section: Section::Changes, code: y, path, stat: None });
                }
            }
        }
        entries.sort_by(|a, b| (a.section, &a.path).cmp(&(b.section, &b.path)));
        self.entries = entries;
        if let Some(path) = self.follow.take()
            && let Some(index) = self.entries.iter().position(|entry| entry.path == path)
        {
            self.selected = index;
        }
        self.apply_stats();
    }

    fn apply_stats(&mut self) {
        for entry in &mut self.entries {
            let staged = entry.section == Section::Staged;
            entry.stat = self.stats.get(&(staged, entry.path.clone())).cloned();
        }
    }

    fn key(&mut self, key: KeyWithModifier) -> bool {
        let count = self.entries.len();
        match key.bare_key {
            BareKey::Down | BareKey::Char('j') if count > 0 => self.selected = (self.selected + 1) % count,
            BareKey::Up | BareKey::Char('k') if count > 0 => self.selected = (self.selected + count - 1) % count,
            BareKey::Enter | BareKey::Char('d') => self.diff(),
            BareKey::Char('s') | BareKey::Char(' ') => self.stage(),
            BareKey::Char('g') => self.open(CommandToRun::new("lazygit")),
            BareKey::Char('r') => self.refresh(),
            BareKey::Tab => panel::switch(panel::other("git")),
            BareKey::Char('p') => self.panel.toggle(),
            BareKey::Esc | BareKey::Char('q') => close_self(),
            _ => return false,
        }
        true
    }

    /// The diff of the selected file, in a floating pane (with the pager of
    /// git)
    fn diff(&self) {
        let Some(entry) = self.entries.get(self.selected) else { return };
        let mut args = vec!["diff"];
        match entry.section {
            Section::Staged => args.push("--cached"),
            Section::Untracked => args.extend(["--no-index", "/dev/null"]),
            Section::Changes | Section::Conflicts => {},
        }
        if entry.section != Section::Untracked {
            args.push("--");
        }
        args.push(&entry.path);
        let mut command = CommandToRun::new("git");
        command.args = args.into_iter().map(str::to_string).collect();
        self.open(command);
    }

    /// Stages the selected file, or unstages it
    fn stage(&mut self) {
        let Some(entry) = self.entries.get(self.selected) else { return };
        self.follow = Some(entry.path.clone());
        let args = match entry.section {
            Section::Staged => vec!["restore", "--staged", "--", &entry.path],
            _ => vec!["add", "--", &entry.path],
        };
        self.git(&args, "action");
    }

    /// A floating command pane in the working directory
    fn open(&self, mut command: CommandToRun) {
        command.cwd = self.cwd.clone();
        let size = |value: &str| Some(value.to_string());
        let coordinates = FloatingPaneCoordinates::new(size("5%"), size("5%"), size("90%"), size("90%"), None, None);
        open_command_pane_floating(command, coordinates, BTreeMap::new());
    }

    fn render_panel(&mut self, rows: usize, cols: usize) {
        let width = cols.saturating_sub(1);
        let directory = self.cwd.as_ref().map(|cwd| cwd.to_string_lossy().to_string()).unwrap_or_default();
        let project = common::project(&directory);

        let title = if self.branch.is_empty() {
            format!("\u{1b}[1m{project}\u{1b}[0m")
        } else {
            format!("\u{1b}[1m{project}\u{1b}[0m \u{1b}[36m⎇ {}\u{1b}[0m", truncate(&branch(&self.branch), width / 2))
        };
        let mut output = vec![
            panel::tabs("git"),
            title,
            format!(
                "\u{1b}[2m{}\u{1b}[0m",
                truncate("⏎ diff · s stage · g lazygit · Tab claude · p dock · Esc", width)
            ),
        ];

        if let Some(error) = &self.error {
            output.push(String::new());
            output.push(format!("\u{1b}[2m{}\u{1b}[0m", truncate(error, width)));
            self.lines.clear();
            print!("{}", output.join("\n"));
            return;
        }
        if self.entries.is_empty() {
            output.push(String::new());
            let text = if self.cwd.is_none() || self.branch.is_empty() { "…" } else { "No changes" };
            output.push(format!("\u{1b}[2m{text}\u{1b}[0m"));
            self.lines.clear();
            print!("{}", output.join("\n"));
            return;
        }

        // The entry of each line; None: the title of a section
        let mut lines = vec![];
        for (index, entry) in self.entries.iter().enumerate() {
            if index == 0 || self.entries[index - 1].section != entry.section {
                lines.push(None);
            }
            lines.push(Some(index));
        }

        // The selected entry on screen
        let height = rows.saturating_sub(HEADER);
        let position = lines.iter().position(|line| *line == Some(self.selected)).unwrap_or(0);
        let mut offset = self.offset.min(lines.len().saturating_sub(height));
        if position < offset {
            offset = if position > 0 && lines[position - 1].is_none() { position - 1 } else { position };
        } else if position >= offset + height {
            offset = position + 1 - height;
        }

        let mut shown = vec![];
        for (row, line) in lines.iter().enumerate().skip(offset).take(height) {
            let Some(index) = *line else {
                // The title of the section of the next entry
                let section = lines[row + 1..].iter().flatten().next().map(|i| self.entries[*i].section);
                if let Some(section) = section {
                    let count = self.entries.iter().filter(|entry| entry.section == section).count();
                    output.push(format!("\u{1b}[1;{}m{} ({count})\u{1b}[0m", section.color(), section.title()));
                }
                shown.push(None);
                continue;
            };
            let entry = &self.entries[index];
            let stat = match &entry.stat {
                Some((added, _)) if added == "-" => " binary".to_string(),
                Some((added, removed)) => format!(" +{added} -{removed}"),
                None => String::new(),
            };
            let path = truncate_start(&entry.path, width.saturating_sub(3 + stat.chars().count()));
            let space = width.saturating_sub(3 + path.chars().count() + stat.chars().count());
            let selected = if index == self.selected { "\u{1b}[7m" } else { "" };
            output.push(format!(
                "{selected} \u{1b}[{}m{}\u{1b}[39m {path}{:space$}\u{1b}[2m{stat}\u{1b}[0m",
                entry.section.color(),
                entry.code,
                ""
            ));
            shown.push(Some(index));
        }
        self.offset = offset;
        self.lines = shown;

        // Without a newline after the last line: the pane would scroll
        print!("{}", output.join("\n"));
    }
}

/// `git diff --numstat -z`: the lines added and removed by path
fn numstat(output: &str) -> Vec<(String, (String, String))> {
    let mut stats = vec![];
    let mut fields = output.split('\0');
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        // Renamed: an empty path, then the original and the new one
        let path = if path.is_empty() {
            fields.next();
            fields.next().unwrap_or_default()
        } else {
            path
        };
        stats.push((path.to_string(), (added.to_string(), removed.to_string())));
    }
    stats
}

/// `main...origin/main [ahead 1, behind 2]` as `main ↑1 ↓2`
fn branch(line: &str) -> String {
    let (name, tracking) = line.split_once(" [").unwrap_or((line, ""));
    let name = name.split("...").next().unwrap_or(name).trim_start_matches("No commits yet on ");
    let mut text = name.to_string();
    for part in tracking.trim_end_matches(']').split(", ") {
        if let Some(count) = part.strip_prefix("ahead ") {
            text.push_str(&format!(" ↑{count}"));
        } else if let Some(count) = part.strip_prefix("behind ") {
            text.push_str(&format!(" ↓{count}"));
        }
    }
    text
}

/// Cut at the start to `max` characters: the end of a path matters most
fn truncate_start(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count + 1 - max.max(1)).collect();
    format!("…{kept}")
}

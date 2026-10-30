//! The Claude view of the panel: the sessions of all the zellij sessions,
//! Enter or a click jumps to one, then the previous conversations, resumed
//! in a new tab in the directory `claude --resume` looks in.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use common::{
    SYNC_PIPE, Session, Status, project_folder, project_label, search_match, short_directory, status_from_title,
    topic_from_title, truncate,
};
use serde_json::Value;
use zellij_tile::prelude::*;

use crate::{PROJECTS_FILE, SESSIONS_DIR, session_file};

/// Lines above the sessions
const HEADER: usize = 2;
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

#[derive(Default)]
pub struct List {
    /// The sessions of its zellij session (received), the zellij sessions,
    /// the sessions shown, the previous conversations, the search and the
    /// selected entry
    current: Vec<Session>,
    zellij_sessions: Vec<SessionInfo>,
    shown: Vec<Session>,
    past: Vec<Past>,
    search: Option<String>,
    selected: usize,
    /// The first line shown, the entry of each line shown
    offset: usize,
    lines: Vec<Option<usize>>,
}

impl List {
    /// The sessions from the background instance, the projects directory of
    /// Claude Code for the previous conversations
    pub fn start(&mut self) {
        pipe_message_to_plugin(MessageToPlugin::new(SYNC_PIPE));
        self.scan();
        match fs::read_to_string(PROJECTS_FILE) {
            Ok(projects) => change_host_folder(PathBuf::from(projects.trim())),
            // No event of the hook yet: the default directory
            Err(_) => run_command(
                &["sh", "-c", "echo \"${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects\""],
                BTreeMap::from([("projects".to_string(), String::new())]),
            ),
        }
    }

    /// The projects directory of Claude Code (`projects` in the context of the
    /// command); whether it was
    pub fn result(&mut self, code: Option<i32>, stdout: &[u8], context: &BTreeMap<String, String>) -> bool {
        if !context.contains_key("projects") {
            return false;
        }
        if code == Some(0) {
            change_host_folder(PathBuf::from(String::from_utf8_lossy(stdout).trim()));
        }
        true
    }

    /// The projects directory is the host folder: its transcripts
    pub fn host_folder_changed(&mut self) {
        self.history();
        self.clamp();
    }

    /// The sessions of its zellij session, from the background instance
    pub fn set_current(&mut self, sessions: Vec<Session>) {
        self.current = sessions;
        self.rebuild();
    }

    pub fn set_zellij_sessions(&mut self, zellij_sessions: Vec<SessionInfo>) {
        self.zellij_sessions = zellij_sessions;
        self.rebuild();
    }

    /// The other zellij sessions are only read on request
    pub fn scan(&mut self) {
        if let Ok(snapshot) = get_session_list() {
            self.zellij_sessions = snapshot.live_sessions;
            self.rebuild();
        }
    }

    /// Typing a search: the keys are its own
    pub fn searching(&self) -> bool {
        self.search.is_some()
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
        // The project and the worktree, the title
        let running = self.shown.iter().filter(|s| {
            search_match(search, &format!("{} {} {}", project_label(&s.directory), s.project(), s.title()))
        });
        let past = self.past.iter().filter(|past| {
            !self.shown.iter().any(|s| s.id == past.id)
                && search_match(search, &format!("{} {}", project_label(&past.cwd), past.title))
        });
        running.map(Item::Running).chain(past.map(Item::Past)).collect()
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.items().len().saturating_sub(1));
    }

    /// A key; whether it was one of the list, and whether an entry was
    /// opened
    pub fn key(&mut self, key: &KeyWithModifier) -> (bool, bool) {
        let count = self.items().len();
        match key.bare_key {
            BareKey::Enter => return (true, self.open()),
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
            BareKey::Down | BareKey::Char('j') if count > 0 => self.selected = (self.selected + 1) % count,
            BareKey::Up | BareKey::Char('k') if count > 0 => self.selected = (self.selected + count - 1) % count,
            BareKey::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if index < count {
                    self.selected = index;
                    return (true, self.open());
                }
            },
            _ => return (false, false),
        }
        (true, false)
    }

    /// A click on a line of the view: opens its entry; whether one was
    pub fn click(&mut self, line: usize) -> bool {
        match line.checked_sub(HEADER).and_then(|row| self.lines.get(row).copied().flatten()) {
            Some(index) => {
                self.selected = index;
                self.open()
            },
            None => false,
        }
    }

    /// Jumps to the selected session, or resumes the selected conversation
    /// in a new tab; whether there was one
    fn open(&self) -> bool {
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
            None => return false,
        }
        true
    }

    /// The lines of the view, under the tabs of the panel: the directory of
    /// the selected entry on the last one
    pub fn render(&mut self, rows: usize, cols: usize) -> Vec<String> {
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
        let mut output = vec![format!("\u{1b}[1m{bold}\u{1b}[0m\u{1b}[2m{dim}\u{1b}[0m"), String::new()];

        // The entry of each line; None: the title of the previous conversations
        let mut lines = vec![];
        for (index, item) in items.iter().enumerate() {
            if matches!(item, Item::Past(_)) && index == running {
                lines.push(None);
            }
            lines.push(Some(index));
        }

        // The selected entry on screen; under the list, a blank line and the
        // directory of the selected entry
        let height = rows.saturating_sub(HEADER + 2);
        let position = lines.iter().position(|line| *line == Some(self.selected)).unwrap_or(0);
        let mut offset = self.offset.min(lines.len().saturating_sub(height));
        if position < offset {
            offset = if position > 0 && lines[position - 1].is_none() { position - 1 } else { position };
        } else if position >= offset + height {
            offset = position + 1 - height;
        }

        // The directory to resume in: of its projects folder
        let directory = |item: &Item| match item {
            Item::Running(session) if !session.directory.is_empty() => session.directory.clone(),
            Item::Running(session) => session.cwd.clone(),
            Item::Past(past) => past.cwd.clone(),
        };
        let project_width = items
            .iter()
            .map(|item| project_label(&directory(item)).chars().count())
            .max()
            .unwrap_or(0)
            .min(28.min(cols / 3));
        // The zellij session, when there are several
        let several = self.shown.iter().any(|s| s.zellij_session != self.shown[0].zellij_session);
        let zellij_width = if several {
            self.shown.iter().map(|s| s.zellij_session.chars().count()).max().unwrap_or(0).min(20)
        } else {
            0
        };

        for line in lines.iter().skip(offset).take(height) {
            let Some(index) = *line else {
                output.push("\u{1b}[2m  Previous conversations\u{1b}[0m".to_string());
                continue;
            };
            let (icon, color, zellij, title, detail, since) = match items[index] {
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
                    (session.status.icon(), color, zellij, session.title(), detail, session.since)
                },
                Item::Past(past) => ("↺", "2", "", past.title.as_str(), String::new(), past.modified),
            };
            let project = project_label(&directory(&items[index]));
            let age = if since > 0 && now >= since { age(now - since) } else { String::new() };
            let number = if index < 9 { (index + 1).to_string() } else { " ".to_string() };

            // The title takes the room left by the other columns
            let zellij =
                if several { format!("{:<zellij_width$}  ", truncate(zellij, zellij_width)) } else { String::new() };
            let left = format!("{number} {zellij}{:<project_width$}  ", truncate(&project, project_width));
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
        let selected = items.get(self.selected).map(|item| short_directory(&directory(item)));
        drop(items);

        self.offset = offset;
        self.lines = lines.into_iter().skip(offset).take(height).collect();

        if empty {
            let found = if self.search.is_some() { " found" } else { "" };
            output.push(format!("\u{1b}[2mNo Claude Code session{found}\u{1b}[0m"));
        }
        // At the bottom: the start of the directory left out if too long
        if let Some(directory) = selected {
            let room = cols.saturating_sub(5);
            let length = directory.chars().count();
            let directory = if length > room {
                format!("…{}", directory.chars().skip(length + 1 - room).collect::<String>())
            } else {
                directory
            };
            output.resize(rows.saturating_sub(1).max(output.len()), String::new());
            output.push(format!("\u{1b}[2m  ⌂ \u{1b}[22;36m{directory}\u{1b}[0m"));
        }
        output
    }
}

/// A previous conversation from the end of its transcript (the start too for
/// the directory, if not found); None without a title.
///
/// Its directory is the one `claude --resume` looks in: the one of its
/// projects folder (the conversation may have gone to another directory, a
/// worktree), else the latest one.
fn transcript(path: &Path, modified: u64) -> Option<Past> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut past = Past { id: path.file_stem()?.to_str()?.to_string(), modified, ..Default::default() };
    let folder = path.parent()?.file_name()?.to_str()?;
    let (mut custom, mut ai, mut prompt) = (None, None, None);
    let mut found = false;
    // Whether the directory of this line is the one of the folder
    let directory = |line: &str, past: &mut Past| {
        let Some(cwd) = field(line, "cwd").filter(|cwd| *cwd != past.cwd) else {
            return false;
        };
        let found = project_folder(&cwd) == folder;
        if found || past.cwd.is_empty() {
            past.cwd = cwd;
        }
        found
    };

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
        } else if !found && line.contains("\"cwd\":\"") {
            found = directory(line, &mut past);
        }
        if custom.is_some() && found {
            break;
        }
    }
    if !found && size > TRANSCRIPT_TAIL {
        let head = read(&mut file, 0, TRANSCRIPT_HEAD)?;
        // The earliest first
        head.lines().filter(|line| line.contains("\"cwd\":\"")).any(|line| directory(line, &mut past));
    }
    if past.cwd.is_empty() {
        return None;
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

fn age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

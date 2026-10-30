//! Shared between the plugins: the Claude Code sessions and the pipes that
//! carry them.

use serde::{Deserialize, Serialize};

/// From the Claude Code hook to all the plugins: the hook input (JSON) as
/// payload, the zellij pane of Claude and the time as arguments.
pub const EVENT_PIPE: &str = "claude::event";
/// From the claude plugin to all the plugins: the sessions (JSON).
pub const STATE_PIPE: &str = "claude::state";
/// To the claude plugin: asks for the sessions, sent again on STATE_PIPE.
pub const SYNC_PIPE: &str = "claude::sync";

/// To the claude plugin (a key binding, a button of the bar): the side panel
/// of the active tab, showing this view (git or claude).
pub const OPEN_PIPE: &str = "panel::open";
/// From a side panel starting to the claude plugin, which opens them.
pub const HELLO_PIPE: &str = "panel::hello";
/// From the claude plugin to a side panel already open: this view, focused
/// (closed if it was already).
pub const SHOW_PIPE: &str = "panel::show";
/// The titles of the side panel, by view (the bar highlights its button)
pub const GIT_TITLE: &str = "Git";
pub const CLAUDE_TITLE: &str = "Claude Code";

/// Ends the tab names set by the plugins (and by the zellij module of
/// Prezto): a tab without it was renamed by hand and is left alone.
pub const NAME_MARKER: char = '\u{200b}';

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Done, waiting for the next prompt
    #[default]
    Idle,
    Working,
    /// Asks for a permission or an answer
    Waiting,
}

impl Status {
    pub fn icon(self) -> &'static str {
        match self {
            Status::Idle => "✓",
            Status::Working => "⚙",
            Status::Waiting => "⚠",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Working => "working",
            Status::Waiting => "waiting",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Session {
    /// Claude Code session id
    pub id: String,
    /// zellij terminal pane running Claude
    pub pane_id: u32,
    pub tab_id: Option<usize>,
    pub tab_position: Option<usize>,
    pub cwd: String,
    /// Conversation topic: the pane title set by Claude Code
    pub topic: String,
    pub status: Status,
    /// Tool being used
    pub tool: Option<String>,
    /// Notification of a waiting session
    pub message: Option<String>,
    /// Unix time of the last status change
    pub since: u64,
    /// zellij session of the pane
    #[serde(default)]
    pub zellij_session: String,
    /// The directory to resume it in: the one of its projects folder
    /// (`claude --resume` looks there), else the first one
    #[serde(default)]
    pub directory: String,
}

impl Session {
    pub fn project(&self) -> &str {
        project(&self.cwd)
    }

    /// The topic, else the project
    pub fn title(&self) -> &str {
        if self.topic.is_empty() { self.project() } else { &self.topic }
    }
}

/// Last component of a working directory
pub fn project(cwd: &str) -> &str {
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd)
}

/// The project of a working directory, a worktree of Claude Code after it
/// (`Platform ⎇ fix-login`)
pub fn project_label(cwd: &str) -> String {
    match cwd.split_once("/.claude/worktrees/") {
        Some((root, rest)) => format!("{} ⎇ {}", project(root), rest.split('/').next().unwrap_or(rest)),
        None => project(cwd).to_string(),
    }
}

/// A directory with `~` for a home (/home/<user>, /Users/<user>)
pub fn short_directory(cwd: &str) -> String {
    for home in ["/home/", "/Users/"] {
        if let Some(rest) = cwd.strip_prefix(home) {
            return match rest.find('/') {
                Some(slash) => format!("~{}", &rest[slash..]),
                None => "~".to_string(),
            };
        }
    }
    cwd.to_string()
}

/// The projects folder of Claude Code for a working directory (where
/// `claude --resume` looks): each character other than a letter, a digit or
/// a dash replaced by a dash
pub fn project_folder(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

/// Whether each word of the search is in the text, ignoring the case
pub fn search_match(search: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    search.to_lowercase().split_whitespace().all(|word| text.contains(word))
}

/// The pane title without the status prefix of Claude Code (✳ when waiting,
/// a spinner when working); empty until Claude names the conversation.
pub fn topic_from_title(title: &str) -> String {
    let topic = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    if topic == "Claude Code" { String::new() } else { topic.to_string() }
}

/// The status of a pane from the title set by Claude Code (✳ when waiting
/// for a prompt, a braille spinner when working), for the panes without the
/// hook: None if it does not look like Claude Code.
pub fn status_from_title(title: &str) -> Option<Status> {
    match title.chars().next()? {
        '✳' => Some(Status::Idle),
        '\u{2800}'..='\u{28ff}' => Some(Status::Working),
        _ => None,
    }
}

/// Cut to `max` characters, with an ellipsis
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Whether the plugins may rename a tab: a default name ("Tab #<n>") or one
/// they set.
pub fn is_renamable(tab_name: &str) -> bool {
    tab_name.ends_with(NAME_MARKER)
        || tab_name.strip_prefix("Tab #").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories() {
        assert_eq!(
            project_label("/home/plivox/Projects/Platform/.claude/worktrees/fix-login/modules"),
            "Platform ⎇ fix-login"
        );
        assert_eq!(project_label("/home/plivox/Projects/Platform"), "Platform");
        assert_eq!(short_directory("/home/plivox/Projects/Platform"), "~/Projects/Platform");
        assert_eq!(short_directory("/Users/Plivox"), "~");
        assert_eq!(short_directory("/tmp"), "/tmp");
    }

    #[test]
    fn folder() {
        assert_eq!(
            project_folder("/Users/Plivox/Projects/@Github/@Plivox/Platform/.claude/worktrees/files-phase-2"),
            "-Users-Plivox-Projects--Github--Plivox-Platform--claude-worktrees-files-phase-2"
        );
    }

    #[test]
    fn topic() {
        assert_eq!(topic_from_title("✳ Fix login bug"), "Fix login bug");
        assert_eq!(topic_from_title("⠂ Fix login bug"), "Fix login bug");
        assert_eq!(topic_from_title("✳ Claude Code"), "");
        assert_eq!(topic_from_title(""), "");
    }

    #[test]
    fn status() {
        assert_eq!(status_from_title("✳ UI Port"), Some(Status::Idle));
        assert_eq!(status_from_title("⠂ UI Port"), Some(Status::Working));
        assert_eq!(status_from_title("plivox@hulk: ~"), None);
        assert_eq!(status_from_title(""), None);
    }

    #[test]
    fn renamable() {
        assert!(is_renamable("Tab #12"));
        assert!(is_renamable("⚙ Fix login bug\u{200b}"));
        assert!(!is_renamable("Tab #"));
        assert!(!is_renamable("work"));
    }

    #[test]
    fn search() {
        assert!(search_match("zell PLUG", "Homesible: zellij plugins"));
        assert!(search_match("", "Homesible"));
        assert!(!search_match("ovile", "Avis sur platform"));
    }

    #[test]
    fn truncated() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}

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
}

impl Session {
    /// Last component of the working directory
    pub fn project(&self) -> &str {
        self.cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(&self.cwd)
    }

    /// The topic, else the project
    pub fn title(&self) -> &str {
        if self.topic.is_empty() { self.project() } else { &self.topic }
    }
}

/// The pane title without the status prefix of Claude Code (✳ when waiting,
/// a spinner when working); empty until Claude names the conversation.
pub fn topic_from_title(title: &str) -> String {
    let topic = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    if topic == "Claude Code" { String::new() } else { topic.to_string() }
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
    fn topic() {
        assert_eq!(topic_from_title("✳ Fix login bug"), "Fix login bug");
        assert_eq!(topic_from_title("⠂ Fix login bug"), "Fix login bug");
        assert_eq!(topic_from_title("✳ Claude Code"), "");
        assert_eq!(topic_from_title(""), "");
    }

    #[test]
    fn renamable() {
        assert!(is_renamable("Tab #12"));
        assert!(is_renamable("⚙ Fix login bug\u{200b}"));
        assert!(!is_renamable("Tab #"));
        assert!(!is_renamable("work"));
    }

    #[test]
    fn truncated() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}

# zellij-plugins

[zellij](https://zellij.dev) plugins (0.45), written in Rust:

- **claude**: the [Claude Code](https://claude.com/claude-code) sessions of all the
  zellij sessions. Their tabs are named after the conversation, with its status
  (`⚙` working, `⚠` waiting for a permission or an answer, `✓` done), and a
  floating list jumps to any of them or resumes a previous conversation.
- **bar**: a one-line status bar: the mode, the session, the tabs, the Claude
  Code sessions and the time. A click on a tab goes to it, a double click
  renames it, a click on the Claude Code sessions opens their list.

The release has the plugins, the Claude Code hook (`claude-hook.sh`) and
their SHA256 (`sha256sums.txt`).

## claude

Loaded in the background, it follows the sessions from the events of the
Claude Code hook:

```kdl
load_plugins {
    "file:~/.config/zellij/plugins/claude.wasm"
}
```

The list, from a key binding: the sessions, then the previous conversations
(the latest transcripts of Claude Code), resumed in a new tab
(`claude --resume`, in their directory). `↑↓`/`jk` and Enter, a digit or a
click opens one, `/` searches (each word, in the project and the title), Esc
closes:

```kdl
bind "Alt l" {
    LaunchOrFocusPlugin "file:~/.config/zellij/plugins/claude.wasm" {
        floating true
        move_to_focused_tab true
        view "list"
    }
}
```

The hook, in `~/.claude/settings.json`, for the events of the plugin:

```json
{
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "PreToolUse": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command", "command": "~/.claude/hooks/zellij-claude" }] }]
  }
}
```

The transcripts are read in the projects directory of Claude Code, known from
the first event of the hook (`transcript_path`).

A tab is renamed only if it has a default name or one set by the plugins
(ending with a zero-width space): a tab renamed by hand is left alone.

Permissions: `ReadApplicationState`, `ChangeApplicationState`,
`ReadCliPipes`, `MessageAndLaunchOtherPlugins`, `FullHdAccess` (the
transcripts), `RunCommands` (`claude --resume`).

## bar

```kdl
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="file:~/.config/zellij/plugins/bar.wasm" {
                timezone "Europe/Zurich"
                hint "⏏ Alt q"
                claude "file:~/.config/zellij/plugins/claude.wasm"
            }
        }
        children
    }
}
```

All optional: `timezone` of the time (no time without it), `hint` before the
time, `claude` the URL of the claude plugin, to open its list.

Permissions: `ReadApplicationState`, `ChangeApplicationState`,
`MessageAndLaunchOtherPlugins`.

## Build

```sh
cargo build --release   # target/wasm32-wasip1/release/*.wasm
cargo test -p common --target "$(rustc -vV | sed -n 's/host: //p')"
```

A `v*` tag publishes a release.

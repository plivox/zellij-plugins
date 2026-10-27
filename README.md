# zellij-plugins

[zellij](https://zellij.dev) plugins (0.45), written in Rust:

- **claude**: the [Claude Code](https://claude.com/claude-code) sessions of all the
  zellij sessions. Their tabs are named after the conversation, with its status
  (`⚙` working, `⚠` waiting for a permission or an answer, `✓` done), and a
  floating list jumps to any of them or resumes a previous conversation.
- **bar**: a one-line status bar: the mode, the session, the tabs, the Git
  and Claude Code buttons and the time. A click on a tab goes to it, a double
  click renames it, its `✕` closes the active tab (a second click confirms);
  the Git button opens the git panel, the Claude Code button (`✳` and the
  sessions by status) their list.
- **git**: the git changes of the focused pane, in a floating panel: staged,
  not staged, untracked, in conflict, with the lines added and removed. Enter
  shows a diff, `s` stages or unstages a file, `g` opens lazygit, `p` docks
  it on the right. Docked, the git panel and the list of claude are stacked
  together.

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
click opens one, `/` searches (each word, in the project and the title), `p`
docks the list on the right (it then stays open), Esc closes. With
`docked "true"` in its configuration, it starts docked:

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
                git "file:~/.config/zellij/plugins/git.wasm"
                git_docked "true"
                claude "file:~/.config/zellij/plugins/claude.wasm"
            }
        }
        children
    }
}
```

All optional: `timezone` of the time (no time without it), `hint` before the
time, `git` and `claude` the URLs of the plugins, for their buttons.
The buttons open the panels like their key bindings
(`zellij action launch-or-focus-plugin`), with the same configuration:
`git_docked "true"`, `claude_docked "true"` when the key bindings have
`docked "true"`.

Permissions: `ReadApplicationState`, `ChangeApplicationState`,
`MessageAndLaunchOtherPlugins`, `RunCommands` (the buttons).

## git

From a key binding:

```kdl
bind "Alt g" {
    LaunchOrFocusPlugin "file:~/.config/zellij/plugins/git.wasm" {
        floating true
        move_to_focused_tab true
    }
}
```

It follows the working directory of the focused terminal pane, read again
every 2 seconds while shown. `↑↓`/`jk` select a file, Enter (or a click) shows
its diff in a floating pane (Ctrl-c closes it), `s` or Space stages or
unstages it, `g` opens [lazygit](https://github.com/jesseduffield/lazygit),
`r` reads again, `p` docks the panel on the right (in the tiled panes, a
third of the width, stacked with the list of claude when docked too) or
floats it again, Esc closes it. With `docked "true"`
in its configuration, it starts docked.

Permissions: `ReadApplicationState`, `ChangeApplicationState`, `RunCommands`.

## Build

```sh
cargo build --release   # target/wasm32-wasip1/release/*.wasm
cargo test -p common --target "$(rustc -vV | sed -n 's/host: //p')"
```

A `v*` tag publishes a release.

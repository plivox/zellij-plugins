# zellij-plugins

[zellij](https://zellij.dev) plugins (0.45), written in Rust:

- **claude**: the [Claude Code](https://claude.com/claude-code) sessions of all the
  zellij sessions. Their tabs are named after the conversation, with its status
  (`⚙` working, `⚠` waiting for a permission or an answer, `✓` done). It also
  opens the side panel of each tab: its views Git (the changes of the focused
  pane) and Claude (the sessions to jump to, the previous conversations to
  resume).
- **bar**: a one-line status bar: the mode, the session (and the directory
  of the Claude Code session of the focused pane), the tabs, the Git
  and Claude Code buttons and the time. A click on a tab goes to it, a double
  click renames it, its `✕` closes the active tab (a second click confirms);
  the Git and Claude Code (`✳` and the sessions by status) buttons open the
  side panel on their view; a double click on the session renames it.
  It shows the runtimes of the focused pane, sent by its shell. It can be
  split in two: the tabs on top, the rest at the bottom.

The release has the plugins, the Claude Code hook (`claude-hook.sh`) and
their SHA256 (`sha256sums.txt`).

## claude

Loaded in the background, it follows the sessions from the events of the
Claude Code hook, and opens the side panels: its own URL in its
configuration, `docked "false"` to open them floating at first:

```kdl
load_plugins {
    "file:~/.config/zellij/plugins/claude.wasm" {
        url "file:~/.config/zellij/plugins/claude.wasm"
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

A tab is renamed only if it has a default name or one set by the plugins
(ending with a zero-width space): a tab renamed by hand is left alone.

Permissions: `ReadApplicationState`, `ChangeApplicationState`,
`ReadCliPipes`, `MessageAndLaunchOtherPlugins`, `FullHdAccess` (the
transcripts), `RunCommands` (`claude --resume`, git, opening the panel).

### The side panel

One by tab, from key bindings (and the buttons of the bar), on a view: the
panel of the active tab is opened, or shows that view (closed if it already
did and had the focus):

```kdl
bind "Alt g" { MessagePlugin { name "panel::open"; payload "git"; } }
bind "Alt a" { MessagePlugin { name "panel::open"; payload "claude"; } }
```

It opens docked on the right of the tallest pane (35% of the width), or
floating, like the last panel of the tab. Floating or docked, its width and
its focus are the panel's, whatever it shows: its tabs on the first line (or
Tab) switch the view, `p` docks it or floats it again; its buttons on the
right of the tabs (or `-` and `+`) make it narrower or wider (floating, its
width kept when it floats again), ✕ (or Esc) closes it.

**Git**: the changes of the focused terminal pane (the last one, the panel
focused), read again every 2 seconds while shown: staged, not staged,
untracked, in conflict, with the lines added and removed. `↑↓`/`jk` select a
file, Enter (or a click) shows its diff in a floating pane (Ctrl-c closes
it), `s` or Space stages or unstages it, `g` opens
[lazygit](https://github.com/jesseduffield/lazygit), `r` reads again.

**Claude**: the sessions, then the previous conversations (the latest
transcripts of Claude Code, in its projects directory known from the hook),
resumed in a new tab (`claude --resume`, in the directory it looks in: the
one of their projects folder, even if the conversation went to another one,
a worktree). Their project shows the worktree (`Platform ⎇ fix-login`), the
directory of the selected one is at the bottom. `↑↓`/`jk` and Enter, a digit
or a click opens one (the panel closed if floating), `/` searches (each word,
in the project and the title).

## bar

```kdl
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="file:~/.config/zellij/plugins/bar.wasm" {
                timezone "Europe/Zurich"
                hint "⏏ Alt q"
                git "true"
                claude "true"
            }
        }
        children
    }
}
```

The tabs are pills in the colors of the plivox theme (Nord), the rest takes
the colors of the terminal, like a shell prompt.

Split in two bars, the tabs on top and the rest at the bottom:
`view "tabs"` and `view "status"` (the buttons and the time go to the
second).

All optional: `timezone` of the time (no time without it), `hint` before the
time, `git` and `claude` (set) for their buttons, which open the side panel
like the key bindings (`panel::open`, to the claude plugin). A double click
on the session renames it (Enter keeps the new name, Esc cancels).

The runtimes of the focused pane (e.g. mise: `🤖 25.9.0 🐍 3.14.4(venv)`, the
versions in green, a virtual environment in yellow), sent by its shell when
they change, the time to keep the latest:

```sh
zellij pipe --name bar::runtimes --args "pane_id=$ZELLIJ_PANE_ID,time=$EPOCHREALTIME" \
  -- "🤖 25.9.0 🐍 3.14.4" < /dev/null &> /dev/null &!
```

Permissions: `ReadApplicationState`, `ChangeApplicationState`,
`MessageAndLaunchOtherPlugins` (the buttons), `ReadCliPipes` (the runtimes).

## Build

```sh
cargo build --release   # target/wasm32-wasip1/release/*.wasm
cargo test -p common --target "$(rustc -vV | sed -n 's/host: //p')"
```

A `v*` tag publishes a release.

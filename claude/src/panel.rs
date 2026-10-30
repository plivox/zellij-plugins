//! The side panel: its tabs (Git, Claude) on the first line, floating or
//! docked on the right whatever it shows (its frame). Tab or a click on a tab
//! shows the other view, `p` docks the panel or floats it again, Esc closes
//! it.

use std::collections::BTreeMap;

use common::{HELLO_PIPE, SHOW_PIPE, STATE_PIPE};
use zellij_tile::prelude::*;

use crate::frame::{self, Frame};
use crate::git::Git;
use crate::list::List;

/// The views: their name (to show one), their tab and their pane title
const VIEWS: [(&str, &str, &str); 2] = [("git", " ⎇ Git ", "Git"), ("claude", " ✳ Claude ", "Claude Code")];
const GIT: usize = 0;
const CLAUDE: usize = 1;
/// Seconds between two reads of the shown view (the changes, the sessions)
const REFRESH_INTERVAL: f64 = 2.0;

#[derive(Default)]
pub struct Panel {
    view: usize,
    frame: Frame,
    git: Git,
    list: List,
    visible: bool,
}

impl Panel {
    /// `show`: the view shown first (git or claude)
    pub fn new(configuration: &BTreeMap<String, String>) -> Self {
        let show = configuration.get("show").map(String::as_str);
        Panel { view: view_index(show).unwrap_or(CLAUDE), visible: true, ..Default::default() }
    }

    pub fn events() -> Vec<EventType> {
        vec![
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CwdChanged,
            EventType::RunCommandResult,
            EventType::Visible,
            EventType::Timer,
            EventType::Key,
            EventType::Mouse,
            EventType::SessionUpdate,
            EventType::HostFolderChanged,
        ]
    }

    /// Permitted: known by the background instance (it opens the panels),
    /// both views started
    pub fn start(&mut self) {
        pipe_message_to_plugin(MessageToPlugin::new(HELLO_PIPE));
        self.show(self.view);
        self.git.start();
        self.list.start();
        set_timeout(REFRESH_INTERVAL);
    }

    pub fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                self.frame.set_tabs(&tabs);
                self.git.set_tabs(&tabs);
                false
            },
            Event::PaneUpdate(manifest) => {
                // Focused by a click on a tab (the click itself is not sent)
                if let Some(view) = self.frame.update(&manifest) {
                    self.show(view);
                }
                self.git.set_panes(&manifest);
                true
            },
            Event::CwdChanged(PaneId::Terminal(id), cwd, _) => self.git.cwd_changed(id, cwd) && self.view == GIT,
            Event::Visible(visible) => {
                self.visible = visible;
                if visible {
                    self.git.follow_cwd();
                }
                false
            },
            Event::Timer(elapsed) if self.frame.timer(elapsed) => false,
            Event::Timer(_) => {
                if self.visible {
                    match self.view {
                        GIT => self.git.refresh(),
                        _ => self.list.scan(),
                    }
                }
                set_timeout(REFRESH_INTERVAL);
                self.view == CLAUDE
            },
            Event::RunCommandResult(code, stdout, stderr, context) => {
                if let Some(render) = self.git.result(code, &stdout, &stderr, &context) {
                    return render && self.view == GIT;
                }
                self.list.result(code, &stdout, &context);
                false
            },
            Event::SessionUpdate(zellij_sessions, _) => {
                self.list.set_zellij_sessions(zellij_sessions);
                self.view == CLAUDE
            },
            Event::HostFolderChanged(_) => {
                self.list.host_folder_changed();
                self.view == CLAUDE
            },
            Event::Key(key) => self.key(&key),
            Event::Mouse(Mouse::Hover(line, column)) => {
                self.frame.hover(line, column, &labels());
                false
            },
            Event::Mouse(Mouse::LeftClick(0, column)) => {
                if let Some(view) = frame::tab_at(column, &labels()) {
                    self.show(view);
                }
                true
            },
            Event::Mouse(Mouse::LeftClick(line, _)) => {
                let line = (line.max(1) - 1) as usize;
                match self.view {
                    GIT => self.git.click(line),
                    _ => {
                        if self.list.click(line) {
                            self.opened();
                        }
                    },
                }
                true
            },
            Event::Mouse(Mouse::ScrollDown(_)) => self.key(&KeyWithModifier::new(BareKey::Down)),
            Event::Mouse(Mouse::ScrollUp(_)) => self.key(&KeyWithModifier::new(BareKey::Up)),
            _ => false,
        }
    }

    pub fn pipe(&mut self, message: &PipeMessage) -> bool {
        match message.name.as_str() {
            STATE_PIPE => {
                if let Some(sessions) = message.payload.as_deref().and_then(|p| serde_json::from_str(p).ok()) {
                    self.list.set_current(sessions);
                }
                self.view == CLAUDE
            },
            // From the background instance (a key binding, a button of the
            // bar): this view, focused; closed if it was already
            SHOW_PIPE => {
                let view = view_index(message.payload.as_deref()).unwrap_or(self.view);
                if view == self.view && self.frame.focused() {
                    close_self();
                    return false;
                }
                self.show(view);
                self.frame.focus();
                true
            },
            _ => false,
        }
    }

    pub fn render(&mut self, rows: usize, cols: usize) {
        let mut lines = match self.view {
            GIT => self.git.render(rows.saturating_sub(1), cols),
            _ => self.list.render(rows.saturating_sub(1), cols),
        };
        // More lines than rows would scroll the pane
        lines.truncate(rows.saturating_sub(1));
        // Without a newline after the last line: the pane would scroll
        print!("{}\n{}", frame::tabs(&labels(), self.view), lines.join("\n"));
    }

    fn key(&mut self, key: &KeyWithModifier) -> bool {
        // A search typed in the list takes the keys
        if self.view == CLAUDE && self.list.searching() {
            return self.list.key(key).0;
        }
        match key.bare_key {
            BareKey::Tab => self.show((self.view + 1) % VIEWS.len()),
            BareKey::Char('p') => self.frame.toggle(),
            BareKey::Esc | BareKey::Char('q') => close_self(),
            _ if self.view == GIT => return self.git.key(key),
            _ => {
                let (handled, opened) = self.list.key(key);
                if opened {
                    self.opened();
                }
                return handled;
            },
        }
        true
    }

    /// An entry of the list opened: the panel closed if floating
    fn opened(&self) {
        if self.frame.floating() {
            close_self();
        }
    }

    fn show(&mut self, view: usize) {
        self.view = view;
        rename_plugin_pane(get_plugin_ids().plugin_id, VIEWS[view].2);
        if view == GIT {
            self.git.refresh();
        }
    }
}

fn labels() -> [&'static str; 2] {
    VIEWS.map(|(_, label, _)| label)
}

fn view_index(name: Option<&str>) -> Option<usize> {
    VIEWS.iter().position(|(view, _, _)| Some(*view) == name)
}

//! The side panels (git, the list of claude): floating, or docked on the
//! right in the tiled panes. One panel at a time: their tabs, on the first
//! line, show the other one in place (through the bar, which knows their
//! URLs).

use std::time::{Duration, SystemTime};

use zellij_tile::prelude::*;

use crate::SWITCH_PIPE;

/// The panels: their name (for the bar) and their pane title
pub const PANELS: [(&str, &str); 2] = [("git", "Git"), ("claude", "Claude Code")];
/// The tabs of the panels, on the first line, one column apart
const TABS: [(&str, &str); 2] = [("git", " ⎇ Git "), ("claude", " ✳ Claude ")];
/// Docked: the share of the width of the tab, the fewest columns
const WIDTH_PERCENT: usize = 35;
const MIN_WIDTH: usize = 40;
/// Updates to reach the width, at most
const RESIZES: u8 = 6;
/// A click that focuses the panel is not sent to it: the mouse over a tab
/// this recently counts as a click on it
const HOVER_CLICK: Duration = Duration::from_millis(800);

/// Floating or docked, followed from the pane updates
#[derive(Default)]
pub struct Panel {
    /// Its name (git, claude)
    name: &'static str,
    floating: Option<bool>,
    /// Docked wanted, until known floating or docked
    start: Option<bool>,
    /// Docked or floated by the panel: to size or place
    toggled: bool,
    /// The other panel, docked in its tab
    other: Option<u32>,
    /// Docked: the resizes left to reach its width, the tiled panes of its
    /// tab (resized again when they change)
    resizes: u8,
    tiled: usize,
    /// The width when the last resize was asked (one at a time), the width
    /// of the tabs
    resized: Option<usize>,
    columns: usize,
    /// Focused, the tab under the mouse (and when)
    focused: bool,
    hover: Option<(&'static str, SystemTime)>,
}

impl Panel {
    pub fn new(name: &'static str) -> Self {
        Panel { name, ..Default::default() }
    }

    pub fn docked(&self) -> bool {
        self.floating == Some(false)
    }

    /// Once permitted: docked or floating, unless already docked (shown in
    /// place of the other panel)
    pub fn start(&mut self, docked: bool) {
        self.start = Some(docked);
    }

    /// Docks the panel in the tiled panes (zellij puts it on the right of the
    /// largest one), or in place of the other docked panel; or floats it
    /// again
    pub fn toggle(&mut self) {
        if let (Some(true), Some(other)) = (self.floating, self.other) {
            // Launched again in place of the other, through the bar
            focus_pane_with_id(PaneId::Plugin(other), false, false);
            switch(self.name);
            close_self();
            return;
        }
        self.toggled = true;
        toggle_pane_embed_or_eject_for_pane_id(PaneId::Plugin(get_plugin_ids().plugin_id));
    }

    /// The width of the tabs, from their update
    pub fn set_tabs(&mut self, tabs: &[TabInfo]) {
        if let Some(tab) = tabs.iter().find(|tab| tab.active) {
            self.columns = tab.display_area_columns;
        }
    }

    /// The mouse over the panel
    pub fn hover(&mut self, line: isize, column: usize) {
        // Out of the panel (sent with the click that focuses it): kept
        if column >= u16::MAX as usize {
            return;
        }
        self.hover = if line == 0 { tab_at(column).map(|name| (name, SystemTime::now())) } else { None };
    }

    /// A click on the panel: on the tab of the other, shows it
    pub fn click(&self, line: isize, column: usize) {
        if let Some(name) = tab_at(column).filter(|name| line == 0 && *name != self.name) {
            switch(name);
        }
    }

    /// Docked: sized; floating: placed at `coordinates` (x, y, width,
    /// height)
    pub fn update(&mut self, manifest: &PaneManifest, coordinates: [&str; 4]) {
        let id = get_plugin_ids().plugin_id;
        let Some(panes) = manifest.panes.values().find(|panes| panes.iter().any(|p| p.is_plugin && p.id == id)) else {
            return;
        };
        let Some(me) = panes.iter().find(|p| p.is_plugin && p.id == id) else {
            return;
        };
        let floating = me.is_floating;

        // Focused by a click on the tab of the other (the click itself is
        // not sent)
        if me.is_focused && !self.focused {
            let hover = self.hover.take().filter(|(_, time)| time.elapsed().is_ok_and(|e| e < HOVER_CLICK));
            if let Some((name, _)) = hover.filter(|(name, _)| *name != self.name) {
                switch(name);
            }
        }
        self.focused = me.is_focused;

        // Docked: its width again when the tiled panes change (a new pane, a
        // swap layout)
        let tiled = panes.iter().filter(|p| !p.is_floating && p.is_selectable).count();
        if !floating && tiled != self.tiled {
            self.resizes = RESIZES;
        }
        self.tiled = tiled;
        // Waits for the last resize (a few updates at most: the resizes left)
        let waiting = self.resized == Some(me.pane_columns) && self.resizes > 0;
        if waiting {
            self.resizes -= 1;
        } else if !floating && self.resizes > 0 && self.columns > 0 {
            self.resizes -= 1;
            let total = self.columns;
            let target = (total * WIDTH_PERCENT / 100).max(MIN_WIDTH).min(total / 2);
            // A resize moves the border by 5% of the width
            let step = (total / 20).max(1);
            let width = me.pane_columns;
            let (resize, steps) = if width > target {
                (Resize::Decrease, (width - target + step / 2) / step)
            } else {
                (Resize::Increase, (target - width + step / 2) / step)
            };
            if steps == 0 {
                self.resized = None;
                self.resizes = 0;
            } else {
                self.resized = Some(width);
                for _ in 0..steps {
                    resize_pane_with_id(ResizeStrategy::new(resize, Some(Direction::Left)), PaneId::Plugin(id));
                }
            }
        }
        let titles = PANELS.map(|(_, title)| title);
        self.other = panes
            .iter()
            .find(|p| p.is_plugin && p.id != id && !p.is_floating && titles.contains(&p.title.as_str()))
            .map(|p| p.id);
        if let Some(docked) = self.start.take() {
            self.floating = Some(floating);
            if floating && docked {
                self.toggle();
            } else if floating {
                place(coordinates);
            } else {
                // In place of the other: the focus back to the terminal on
                // its left (the next panes split it, not the panel)
                move_focus(Direction::Left);
            }
            return;
        }
        if Some(floating) == self.floating {
            return;
        }
        if self.toggled && floating {
            place(coordinates);
        }
        if !floating {
            self.resizes = RESIZES;
        }
        self.toggled = false;
        self.floating = Some(floating);
    }
}

/// The tabs of the panels as pills (the colors of the bar), `current`
/// highlighted
pub fn tabs(current: &str) -> String {
    TABS.iter()
        .map(|(name, label)| {
            if *name == current {
                // Nord: polar night on frost
                format!("\u{1b}[38;2;46;52;64;48;2;136;192;208;1m{label}\u{1b}[0m")
            } else {
                // Comment on polar night
                format!("\u{1b}[38;2;97;110;136;48;2;59;66;82m{label}\u{1b}[0m")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The panel of the tab at this column of the first line
pub fn tab_at(column: usize) -> Option<&'static str> {
    let mut start = 0;
    for (name, label) in TABS {
        let end = start + label.chars().count();
        if (start..end).contains(&column) {
            return Some(name);
        }
        start = end + 1;
    }
    None
}

/// Shows this panel in place of the focused one (through the bar)
pub fn switch(name: &str) {
    pipe_message_to_plugin(MessageToPlugin::new(SWITCH_PIPE).with_payload(name));
}

/// The other panel
pub fn other(name: &str) -> &'static str {
    PANELS.iter().map(|(other, _)| *other).find(|other| *other != name).unwrap_or_default()
}

/// Floating, shown (the floating panes may be hidden)
pub fn place([x, y, width, height]: [&str; 4]) {
    show_self(true);
    let size = |value: &str| Some(value.to_string());
    if let Some(coordinates) = FloatingPaneCoordinates::new(size(x), size(y), size(width), size(height), None, None) {
        change_floating_panes_coordinates(vec![(PaneId::Plugin(get_plugin_ids().plugin_id), coordinates)]);
    }
}

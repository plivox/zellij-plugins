//! The side panels (git, the list of claude): floating, or docked on the
//! right in the tiled panes. One panel at a time: their tabs, on the first
//! line, show the other one in place (through the bar, which knows their
//! URLs).

use zellij_tile::prelude::*;

use crate::SWITCH_PIPE;

/// The panels: their name (for the bar) and their pane title
pub const PANELS: [(&str, &str); 2] = [("git", "Git"), ("claude", "Claude Code")];
/// The tabs of the panels, on the first line
const TABS: [(&str, &str); 2] = [("git", " ⎇ Git "), ("claude", " ✳ Claude ")];

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

    /// Docked: narrowed; floating: placed at `coordinates` (x, y, width,
    /// height)
    pub fn update(&mut self, manifest: &PaneManifest, coordinates: [&str; 4]) {
        let id = get_plugin_ids().plugin_id;
        let Some(panes) = manifest.panes.values().find(|panes| panes.iter().any(|p| p.is_plugin && p.id == id)) else {
            return;
        };
        let Some(floating) = panes.iter().find(|p| p.is_plugin && p.id == id).map(|p| p.is_floating) else {
            return;
        };
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
        if self.toggled {
            if floating { place(coordinates) } else { narrow() }
        }
        self.toggled = false;
        self.floating = Some(floating);
    }
}

/// The tabs of the panels, `current` highlighted
pub fn tabs(current: &str) -> String {
    TABS.iter()
        .map(|(name, label)| {
            if *name == current { format!("\u{1b}[1;7m{label}\u{1b}[0m") } else { format!("\u{1b}[2m{label}\u{1b}[0m") }
        })
        .collect()
}

/// The panel of the tab at this column of the first line
pub fn tab_at(column: usize) -> Option<&'static str> {
    let mut start = 0;
    for (name, label) in TABS {
        let end = start + label.chars().count();
        if (start..end).contains(&column) {
            return Some(name);
        }
        start = end;
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

/// Docked: about a third of the width, from half (the focus stays on the
/// terminal: the next panes split it, not the panel)
fn narrow() {
    let id = PaneId::Plugin(get_plugin_ids().plugin_id);
    for _ in 0..3 {
        resize_pane_with_id(ResizeStrategy::new(Resize::Decrease, Some(Direction::Left)), id);
    }
}

/// Floating, shown (the floating panes may be hidden)
pub fn place([x, y, width, height]: [&str; 4]) {
    show_self(true);
    let size = |value: &str| Some(value.to_string());
    if let Some(coordinates) = FloatingPaneCoordinates::new(size(x), size(y), size(width), size(height), None, None) {
        change_floating_panes_coordinates(vec![(PaneId::Plugin(get_plugin_ids().plugin_id), coordinates)]);
    }
}

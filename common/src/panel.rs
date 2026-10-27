//! The side panels (git, the list of claude): floating, or docked on the right
//! in the tiled panes, stacked together when both are docked.

use zellij_tile::prelude::*;

/// The titles of the panels, to stack them together
pub const TITLES: [&str; 2] = ["Git", "Claude Code"];

/// Floating or docked, followed from the pane updates
#[derive(Default)]
pub struct Panel {
    floating: Option<bool>,
}

impl Panel {
    pub fn docked(&self) -> bool {
        self.floating == Some(false)
    }

    /// Docks the panel in the tiled panes (zellij puts it on the right of the
    /// largest one), or floats it again
    pub fn toggle(&self) {
        toggle_pane_embed_or_eject_for_pane_id(PaneId::Plugin(get_plugin_ids().plugin_id));
    }

    /// Docked or floated again: stacked with the other docked panel, else
    /// narrowed; floating, placed at `coordinates` (x, y, width, height)
    pub fn update(&mut self, manifest: &PaneManifest, coordinates: [&str; 4]) {
        let id = get_plugin_ids().plugin_id;
        let Some(panes) = manifest.panes.values().find(|panes| panes.iter().any(|p| p.is_plugin && p.id == id)) else {
            return;
        };
        let floating = panes.iter().find(|p| p.is_plugin && p.id == id).map(|p| p.is_floating);
        if floating == self.floating {
            return;
        }
        // Started floating: placed by the plugin
        if self.floating.is_some() || floating == Some(false) {
            if floating == Some(true) {
                place(coordinates);
            } else {
                let other = panes
                    .iter()
                    .find(|p| p.is_plugin && p.id != id && !p.is_floating && TITLES.contains(&p.title.as_str()));
                match other {
                    Some(other) => stack_panes(vec![PaneId::Plugin(other.id), PaneId::Plugin(id)]),
                    None => narrow(),
                }
            }
        }
        self.floating = floating;
    }
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

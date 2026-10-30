//! The frame of the side panel, whatever it shows: floating, or docked on
//! the right in the tiled panes (sized when it docks, its width kept after),
//! its tabs on the first line.

use std::time::{Duration, SystemTime};

use zellij_tile::prelude::*;

/// Floating: on the right
pub const FLOATING: [&str; 4] = ["55%", "5%", "44%", "90%"];
/// Docked: the share of the width of the tab, the fewest columns
const WIDTH_PERCENT: usize = 35;
const MIN_WIDTH: usize = 40;
/// Resizes to reach the width, at most, one at a time: checked on a timer
/// (they apply late, without a pane update), each waited for this many ticks
const RESIZES: u8 = 10;
const TICK: f64 = 0.1;
const WAITS: u8 = 10;
/// A click that focuses the panel is not sent to it: the mouse over a tab
/// this recently counts as a click on it
const HOVER_CLICK: Duration = Duration::from_millis(800);

#[derive(Default)]
pub struct Frame {
    floating: Option<bool>,
    /// Docked: the resizes left to reach its width, the width when the last
    /// one was asked, the ticks left to wait for it, a tick to come; the
    /// width of the tabs
    resizes: u8,
    resized: Option<usize>,
    waits: u8,
    ticking: bool,
    columns: usize,
    /// Focused, the tab under the mouse (and when)
    focused: bool,
    hover: Option<(usize, SystemTime)>,
}

impl Frame {
    pub fn floating(&self) -> bool {
        self.floating == Some(true)
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Docks the panel in the tiled panes (zellij puts it on the right of the
    /// largest one), or floats it again. Without waiting for its update (sent
    /// late): docked, sized; floating, shown where it floats (the floating
    /// panes may be hidden).
    pub fn toggle(&mut self) {
        let Some(floating) = self.floating else { return };
        let id = get_plugin_ids().plugin_id;
        toggle_pane_embed_or_eject_for_pane_id(PaneId::Plugin(id));
        if floating {
            self.resize();
            // Kept (zellij focuses the pane it docks next to)
            focus_plugin_pane(id, false, false);
        } else {
            place(FLOATING);
        }
        self.floating = Some(!floating);
    }

    /// Shown (the floating panes may be hidden) and focused
    pub fn focus(&self) {
        let id = get_plugin_ids().plugin_id;
        if self.floating() {
            show_self(true);
        }
        focus_plugin_pane(id, false, false);
    }

    /// The width of the tabs, from their update
    pub fn set_tabs(&mut self, tabs: &[TabInfo]) {
        if let Some(tab) = tabs.iter().find(|tab| tab.active) {
            self.columns = tab.display_area_columns;
        }
    }

    /// The mouse over the panel: the tab under it, of the labels on the
    /// first line
    pub fn hover(&mut self, line: isize, column: usize, labels: &[&str]) {
        // Out of the panel (sent with the click that focuses it): kept
        if column >= u16::MAX as usize {
            return;
        }
        self.hover = if line == 0 { tab_at(column, labels).map(|tab| (tab, SystemTime::now())) } else { None };
    }

    /// From its update: floating or docked (sized when it docks), focused.
    /// Returns the tab it was focused by a click on (the click is not sent).
    pub fn update(&mut self, manifest: &PaneManifest) -> Option<usize> {
        let id = get_plugin_ids().plugin_id;
        let me = manifest.panes.values().flatten().find(|p| p.is_plugin && p.id == id)?;

        let mut clicked = None;
        if me.is_focused && !self.focused {
            clicked = self.hover.take().filter(|(_, time)| time.elapsed().is_ok_and(|e| e < HOVER_CLICK));
        }
        self.focused = me.is_focused;

        // Docked when opened, or by zellij
        if !me.is_floating && self.floating != Some(false) {
            self.resize();
        }
        self.floating = Some(me.is_floating);
        clicked.map(|(tab, _)| tab)
    }

    /// A timer of the frame (shorter than the one of the panel): sizes it
    pub fn timer(&mut self, elapsed: f64) -> bool {
        if !self.ticking || elapsed >= 1.0 {
            return false;
        }
        self.ticking = false;
        self.size();
        true
    }

    /// Its width, soon (the layout settled)
    fn resize(&mut self) {
        self.resizes = RESIZES;
        self.resized = None;
        self.waits = WAITS;
        self.tick();
    }

    fn tick(&mut self) {
        if !self.ticking {
            self.ticking = true;
            set_timeout(TICK);
        }
    }

    /// One resize toward its width, once the last one is applied
    fn size(&mut self) {
        let me = get_pane_info(PaneId::Plugin(get_plugin_ids().plugin_id));
        let width = me.filter(|me| !me.is_floating).map(|me| me.pane_columns);
        // Not docked yet, the width of the tabs not known, the last resize
        // not applied
        let Some(width) = width.filter(|width| self.columns > 0 && self.resized != Some(*width)) else {
            if self.waits > 0 && self.resizes > 0 {
                self.waits -= 1;
                self.tick();
            }
            return;
        };
        if self.resizes == 0 {
            return;
        }
        let total = self.columns;
        let target = (total * WIDTH_PERCENT / 100).max(MIN_WIDTH).min(total / 2);
        // A resize moves the border by 5% of the width
        let step = (total / 20).max(1);
        if width.abs_diff(target) <= step / 2 {
            self.resizes = 0;
            return;
        }
        self.resizes -= 1;
        self.resized = Some(width);
        self.waits = WAITS;
        let resize = if width > target { Resize::Decrease } else { Resize::Increase };
        resize_pane_with_id(
            ResizeStrategy::new(resize, Some(Direction::Left)),
            PaneId::Plugin(get_plugin_ids().plugin_id),
        );
        self.tick();
    }
}

/// The tabs as pills (the colors of the bar), `current` highlighted
pub fn tabs(labels: &[&str], current: usize) -> String {
    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            if index == current {
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

/// The tab at this column of the first line (one column between them)
pub fn tab_at(column: usize, labels: &[&str]) -> Option<usize> {
    let mut start = 0;
    for (index, label) in labels.iter().enumerate() {
        let end = start + label.chars().count();
        if (start..end).contains(&column) {
            return Some(index);
        }
        start = end + 1;
    }
    None
}

/// Floating, shown (the floating panes may be hidden)
pub fn place([x, y, width, height]: [&str; 4]) {
    show_self(true);
    let size = |value: &str| Some(value.to_string());
    if let Some(coordinates) = FloatingPaneCoordinates::new(size(x), size(y), size(width), size(height), None, None) {
        change_floating_panes_coordinates(vec![(PaneId::Plugin(get_plugin_ids().plugin_id), coordinates)]);
    }
}

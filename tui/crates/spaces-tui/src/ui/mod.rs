//! Frame composition: sidebar, tab bar, pane area, and status bar.
//!
//! [`frame::render`] lays out the whole screen and delegates the left column
//! to [`sidebar::render_sidebar`] and the row above the panes to
//! [`tabs::render_tab_bar`]. [`render`], [`pane_area`], [`sidebar_area`],
//! [`tab_bar_area`] and [`visible_pane_regions`] are re-exported here so
//! `main.rs` reaches them as `ui::render`/`ui::pane_area`/… .
//!
//! `palette`, `sidebar` and `tabs` are `pub mod` rather than flattened:
//! `input`'s router hit-tests against `sidebar::SidebarHit` and
//! `tabs::TabBar` by their own paths, and both modules' doc comments explain
//! why every hit test derives its geometry fresh from the model at click
//! time rather than reusing rectangles a previous render produced.
pub mod brand;
pub mod files;
mod frame;
pub mod menu;
pub mod palette;
pub mod selection;
pub mod sidebar;
pub mod tabs;
pub mod toast;

pub use frame::{
    files_region, palette_area, pane_area, render, sidebar_area, tab_bar_area, visible_pane_regions,
};
pub use palette::{Command, Palette};

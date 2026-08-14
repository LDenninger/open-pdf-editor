//! Named icons, each aliasing one Phosphor glyph.
//!
//! Icons come from the **Phosphor** icon set (MIT), bundled as a font by the
//! `egui-phosphor` crate and installed by [`crate::theme::apply_theme`]. No Adobe
//! icon, artwork, or trademark is used here or anywhere in this crate.
//!
//! Widgets reference these constants rather than `egui_phosphor::regular::*`
//! directly, so that swapping the icon set is one edit to this file.

use egui_phosphor::regular;

/// Open a document.
pub const OPEN_DOCUMENT: &str = regular::FOLDER_OPEN;
/// Save the current document.
pub const SAVE_DOCUMENT: &str = regular::FLOPPY_DISK;
/// Show or hide the thumbnail rail.
pub const TOGGLE_RAIL: &str = regular::SIDEBAR_SIMPLE;
/// Scroll to the previous page.
pub const PREVIOUS_PAGE: &str = regular::CARET_LEFT;
/// Scroll to the next page.
pub const NEXT_PAGE: &str = regular::CARET_RIGHT;
/// Increase the zoom level.
pub const ZOOM_IN: &str = regular::MAGNIFYING_GLASS_PLUS;
/// Decrease the zoom level.
pub const ZOOM_OUT: &str = regular::MAGNIFYING_GLASS_MINUS;
/// Zoom so the widest page fills the canvas width.
pub const FIT_WIDTH: &str = regular::ARROWS_OUT;
/// Zoom so the current page fits entirely on screen.
///
/// A frame rather than a pair of arrows, so that this button is distinguishable
/// from [`ROTATE_VIEW`] at a glance.
pub const FIT_PAGE: &str = regular::FRAME_CORNERS;
/// Rotate the view without editing the document.
pub const ROTATE_VIEW: &str = regular::ARROWS_CLOCKWISE;
/// Text selection tool. Reserved: selection is not implemented in this track.
pub const SELECT_TEXT: &str = regular::CURSOR_TEXT;
/// Pan tool.
pub const PAN_CANVAS: &str = regular::HAND;

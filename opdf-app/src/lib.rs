//! The open-pdf-editor desktop shell.
//!
//! This crate is the user interface and nothing else. It never *draws* from a
//! [`opdf_core::document::Document`]: the render worker owns the document because
//! the rasterizer is not thread-safe, so the shell draws an immutable
//! [`opdf_core::document::DocumentSnapshot`] and asks a
//! [`opdf_core::render::RenderService`] handle for pixels asynchronously. It does
//! keep the document itself, so that a later edit or save has something to act
//! on; where documents come from is [`crate::opener`]'s business alone.
//!
//! Layout, zoom, scheduling, and caching are plain functions in their own modules
//! so that they are testable without a display; the widget modules under
//! `panels` are a thin drawing layer over them.
//!
//! The shell also owns the edit history: an [`opdf_ops::UndoStack`] over the same
//! document it holds. That is where the warning before a compacting save lives,
//! because the shell is the only layer that both drives a save and owns the
//! history that save invalidates — see [`crate::app::OpdfApp::confirm_compaction`].

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod app;
pub mod icons;
pub mod layout;
pub mod opener;
pub mod panels;
pub mod scheduler;
pub mod synthetic;
pub mod theme;
pub mod tiles;
pub mod viewer;
pub mod zoom;

pub use opener::{DocumentOpener, OpenedDocument};
pub use theme::{Theme, apply_theme};

/// Name shown in the window title bar and the about box.
pub const APPLICATION_NAME: &str = "opdf";

/// A one-line description of this build, for the status bar and `--version`.
pub fn describe_build() -> String {
    format!(
        "{APPLICATION_NAME} {} (rotate, delete, undo and save; no text selection, no search)",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_application_and_its_version() {
        let description = describe_build();
        assert!(
            description.starts_with("opdf "),
            "the description must lead with the binary name, got: {description}"
        );
        assert!(
            !description.contains("viewer only"),
            "this build edits and saves; calling itself a viewer understates it, got: {description}"
        );
        assert!(
            description.contains("no text selection"),
            "text selection and search are still missing and must be named rather than implied, got: {description}"
        );
    }
}

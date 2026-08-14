//! The open-pdf-editor desktop shell.
//!
//! This crate is the user interface and nothing else. It never holds a
//! [`opdf_core::document::Document`]: the render worker owns the document because
//! the rasterizer is not thread-safe, so the shell holds an immutable
//! [`opdf_core::document::DocumentSnapshot`] plus a
//! [`opdf_core::render::RenderService`] handle and asks for pixels asynchronously.
//!
//! Layout, zoom, scheduling, and caching are plain functions in their own modules
//! so that they are testable without a display; the widget modules under
//! `panels` are a thin drawing layer over them.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

/// Name shown in the window title bar and the about box.
pub const APPLICATION_NAME: &str = "opdf";

/// A one-line description of this build, for the status bar and `--version`.
pub fn describe_build() -> String {
    format!("{APPLICATION_NAME} {} (synthetic documents only)", env!("CARGO_PKG_VERSION"))
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
            description.contains("synthetic"),
            "this build renders synthetic documents only and must say so, got: {description}"
        );
    }
}

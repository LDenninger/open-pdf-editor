//! Binary entry point for the open-pdf-editor desktop shell.
//!
//! Owned by **Track D**. Built against `opdf_core::fakes::FakeRenderService`
//! until Track B lands a real rasterizer.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]

fn main() {
    println!("{}", opdf_app::describe_build());
}

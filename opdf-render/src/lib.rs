//! Rasterization backends and the render worker.
//!
//! Owned by **Track B**. Implements [`opdf_core::RenderService`] over PDFium,
//! and is complete when it passes
//! `opdf_core::contract::assert_render_service_contract`.

#![warn(missing_docs)]

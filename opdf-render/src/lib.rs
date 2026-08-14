//! Rasterization backends and the render worker.
//!
//! Owned by **Track B**. Implements [`opdf_core::RenderService`] over PDFium,
//! and is complete when it passes
//! `opdf_core::contract::assert_render_service_contract`.
//!
//! # How a page identity becomes a PDFium page index
//!
//! [`opdf_core::RenderRequest`] names a page by [`opdf_core::PageId`]. PDFium
//! knows nothing of those identities: it addresses pages by a zero-based index
//! into the file it opened. The bridge is the [`opdf_core::DocumentSnapshot`]
//! the service is opened with, whose `pages` are in document order — the same
//! order `Document::page_ids` reports, and therefore the same order PDFium
//! sees. **The nth `PageId` in the snapshot is PDFium page index n.** A request
//! naming a `PageId` absent from the snapshot, or a snapshot position beyond the
//! file's page count, is answered [`opdf_core::RenderResponse::Failed`] — never
//! a panic.
//!
//! That rule is also what lets the contract suite, whose constructor is
//! `Fn(DocumentSnapshot) -> S`, be applied to a rasterizer that needs a real
//! file: the test closure supplies the fixture path, the suite supplies the
//! snapshot, and the mapping between them is positional. No test-only
//! constructor and no change to `opdf-core` are required.
//!
//! # The snapshot is authoritative over the file
//!
//! Page geometry — size and rotation — is taken from the snapshot, not from
//! PDFium, because the snapshot may already reflect an edit the file on disk
//! does not. The rotation actually handed to PDFium is the difference between
//! the rotation the snapshot asks for and the rotation stored in the file, so a
//! page rotated by an unsaved command still rasterizes the way the UI expects.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod geometry;
pub mod library;
pub mod raster;

#[cfg(test)]
mod fixture;

pub use geometry::{MAX_TILE_EDGE, MAX_TILE_PIXELS, TileGeometry, compute_tile_geometry};
pub use library::bind_pdfium;
pub use raster::rasterize_page;

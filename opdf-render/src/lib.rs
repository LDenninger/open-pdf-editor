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
//!
//! # Threading
//!
//! Pdfium is not thread-safe, and neither is a document handle. One worker
//! thread per service owns that service's document, tile cache, and backlog;
//! [`service::PdfiumRenderService`] owns only channel endpoints. `submit`
//! pushes onto an unbounded sender and returns; `poll` drains a receiver with
//! `try_iter` and returns whatever has arrived. Neither blocks.
//!
//! **There is no single render thread.** There are N worker threads, one per
//! open service — the contract suite alone builds ten — serialized by a
//! process-wide mutex. That is a deliberate deviation and not an accident of
//! implementation: `pdfium-render`'s `thread_safe` feature serializes nothing
//! in 0.9.3, it only makes the wrapper types `Send` and `Sync`, so a thread per
//! service buys isolation between documents and none at all inside Pdfium.
//! Every call into Pdfium, including the drops that close a document or a page,
//! is therefore made while holding the process-wide lock. One unlocked call
//! corrupts Pdfium's global state permanently — see [`library`], which is why
//! the binding is reachable only through [`library::with_pdfium`].
//!
//! The consequence a caller must plan for is that the lock is held for the
//! whole of a rasterization, so any operation that needs Pdfium queues behind
//! whatever render is in flight. `submit` and `poll` never touch Pdfium and are
//! unaffected; opening a document does, and is documented at
//! [`service::PdfiumRenderService::open`] with the measured cost and the
//! non-blocking alternative.
//!
//! # Backpressure
//!
//! The worker serves the newest queued request first, because a scrolling user
//! cares about the tile under the viewport now. Above [`backlog::MAX_BACKLOG`]
//! queued requests the oldest is superseded — and answered
//! [`opdf_core::RenderResponse::Failed`], because the contract promises exactly
//! one response per submitted request and a silently dropped request is a tile
//! that never arrives.
//!
//! # Caching
//!
//! [`cache::TileCache`] keys on [`opdf_core::RenderRequest`] directly, so
//! `revision` and the bitwise `scale` are part of the key and a tile from
//! before an edit is unreachable after one. Eviction is least-recently-used
//! and bounded by bytes, defaulting to [`cache::DEFAULT_CACHE_BYTES`].
//!
//! The key alone is not enough to keep a revision honest, because the worker
//! resolves geometry against the snapshot it holds when it *renders*, not when
//! it accepted the request. A request left in the backlog across a rebind is
//! therefore superseded rather than answered — see
//! [`service::PdfiumRenderService::rebind`].
//!
//! # Not implemented here
//!
//! Text extraction, text selection geometry, search, and printing are out of
//! scope for this crate. It turns a [`opdf_core::RenderRequest`] into pixels.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod backlog;
pub mod cache;
pub mod geometry;
pub mod library;
pub mod raster;
pub mod service;
mod worker;

#[cfg(test)]
mod fixture;

pub use backlog::{Backlog, MAX_BACKLOG};
pub use cache::{DEFAULT_CACHE_BYTES, TileCache};
pub use geometry::{MAX_TILE_EDGE, MAX_TILE_PIXELS, TileGeometry, compute_tile_geometry};
pub use library::with_pdfium;
pub use raster::rasterize_page;
pub use service::PdfiumRenderService;

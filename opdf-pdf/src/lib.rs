//! PDF parsing, object model, and incremental save.
//!
//! Owned by **Track A**. [`PdfDocument`] implements [`opdf_core::Document`] and
//! [`opdf_core::DocumentIo`] over a real PDF file, backed by `lopdf`. No `lopdf`
//! type appears in this crate's public API.
//!
//! # Identity
//!
//! [`opdf_core::PageId`] is allocated at open, one per page in page-tree order,
//! and is stable for the lifetime of the open document: it survives reordering,
//! insertion, removal, and import. It is **not** stable across opens, and it is
//! never written to the file — the PDF format has nowhere to put it. Document
//! order lives in memory; the file's page tree is regenerated from it on save.
//!
//! # The trash
//!
//! [`opdf_core::Document::remove_page`] unlinks a page from document order but
//! does not destroy it: the page's slot is retained so that
//! [`opdf_core::Document::restore_page`] can hand back the original page, and
//! its PDF objects are left exactly where they were, because an incremental
//! save never deletes an object — it only stops referencing it. A removed page
//! is restorable until [`opdf_core::DocumentIo::save_compacted`] purges it, and
//! only until the document is closed: the trash is keyed on `PageId`, which is
//! a within-session identity.
//!
//! # Saving
//!
//! [`opdf_core::DocumentIo::save_incremental`] appends an update to the original
//! bytes rather than rewriting them, so the output always begins with the input
//! byte for byte. Saving a document that was not edited reproduces the original
//! file exactly. [`opdf_core::DocumentIo::save_compacted`] rewrites the file
//! without its revision history; it is lossy and is only for explicit user
//! request.
//!
//! # Limits of this implementation
//!
//! - A structural change flattens the page tree: intermediate `/Pages` nodes
//!   stop being referenced, after their inheritable attributes are written onto
//!   the pages that were inheriting them. The nodes themselves are not deleted.
//! - Encrypted documents are rejected with [`opdf_core::Error::Unsupported`].
//! - Importing a page copies its object graph but does not rewrite link or
//!   outline destinations that point at pages outside the copy.
//! - A page with no readable `/MediaBox` is assumed to be US Letter.
//! - Page content is never parsed, only carried.
//! - The trash does not survive a save and reopen, and
//!   [`opdf_core::DocumentIo::save_compacted`] purges it irreversibly.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod document;
mod error;
#[cfg(test)]
mod fixture;
mod geometry;
mod objects;
mod page_map;
mod save;

pub use document::PdfDocument;

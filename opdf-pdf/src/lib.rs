//! PDF parsing, object model, and incremental save.
//!
//! Owned by **Track A**. [`PdfDocument`] implements [`opdf_core::Document`] and
//! [`opdf_core::DocumentIo`] over a real PDF file, backed by `lopdf`. No `lopdf`
//! type appears in this crate's public API.

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

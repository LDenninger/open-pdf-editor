//! Structural-diff and round-trip verification for open-pdf-editor.
//!
//! This crate is Track E's reusable machinery, called from every track's own
//! tests rather than duplicated: [`diff`] compares two PDF byte buffers
//! structurally, [`corpus`] loads and validates the provenance-tracked test
//! corpus in `tests/corpus/`, and [`harness`] wires the two together into a
//! single open-save-reopen-diff assertion over any [`opdf_core::DocumentIo`]
//! implementation.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

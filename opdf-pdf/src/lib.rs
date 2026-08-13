//! PDF parsing, object model, and incremental save.
//!
//! Owned by **Track A**. Implements [`opdf_core::Document`] and
//! [`opdf_core::DocumentIo`] over a real PDF file, and is complete when it
//! passes `opdf_core::contract::assert_document_contract`.

#![warn(missing_docs)]

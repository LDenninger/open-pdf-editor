//! Page operations expressed as invertible commands, and the undo stack.
//!
//! Owned by **Track C**. Implements [`opdf_core::Command`] for merge, split,
//! reorder, rotate, delete, and extract, developed against
//! `opdf_core::fakes::VecDocument` before a real parser exists.

#![warn(missing_docs)]

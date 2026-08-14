//! Page operations expressed as invertible commands, and the undo stack.
//!
//! Owned by **Track C**. Implements [`opdf_core::Command`] for merge, split,
//! reorder, rotate, delete, and extract, developed against
//! `opdf_core::fakes::VecDocument` before a real parser exists.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod remove_page;

pub use remove_page::{RemovePage, RestorePage};

pub mod move_page;

pub use move_page::MovePage;

pub mod set_rotation;

pub use set_rotation::SetRotation;

pub mod insert_blank_page;

pub use insert_blank_page::InsertBlankPage;

pub mod sequence;

pub use sequence::Sequence;

pub mod import_pages;

pub use import_pages::ImportPages;

//! Contract surface shared by every open-pdf-editor crate.
//!
//! This crate contains no implementation. It defines the traits and value types
//! that implementation crates satisfy, in-memory fakes that let dependent crates
//! be developed before real implementations exist, and contract test suites that
//! every implementation must pass.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod page;

pub use error::{Error, Result};
pub use page::{PageId, PageIdAllocator, PageInfo, PageSize, Rotation};

pub mod document;

pub use document::{Document, DocumentIo, DocumentSnapshot};

pub mod fakes;

pub mod render;

pub use render::{RenderRequest, RenderResponse, RenderService, Tile};

#[cfg(feature = "contract-tests")]
pub mod contract;

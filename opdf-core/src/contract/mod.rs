//! Behavioural contract suites shared by every implementation crate.
//!
//! These are public functions rather than `#[test]` items because
//! implementations live in other crates and must call them from their own
//! test modules. They are compiled only under the `contract-tests` feature.
#![allow(clippy::expect_used)]

pub mod document;

pub use document::assert_document_contract;

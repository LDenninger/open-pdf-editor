//! In-memory implementations of the contracts, for use by dependent crates
//! before real implementations exist.

pub mod vec_document;

pub use vec_document::VecDocument;

pub mod fake_render_service;

pub use fake_render_service::FakeRenderService;

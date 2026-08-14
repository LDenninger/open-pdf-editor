//! Rasterized pixels on their way to the screen: a bounded cache keyed by
//! [`opdf_core::render::RenderRequest`], and the bridge that turns an
//! [`opdf_core::render::Tile`] into an egui texture.

pub mod cache;

pub use cache::TileCache;

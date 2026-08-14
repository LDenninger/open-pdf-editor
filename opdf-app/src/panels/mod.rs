//! The shell's chrome, one module per region: menu bar, toolbar, thumbnail rail,
//! page canvas, status bar.
//!
//! These modules draw. Every decision they draw from — what is visible, what scale
//! to render at, what to request — is made by [`crate::layout`], [`crate::zoom`],
//! [`crate::scheduler`], and [`crate::viewer`], which are tested headlessly.

pub mod canvas;
pub mod menu_bar;
pub mod thumbnail_rail;

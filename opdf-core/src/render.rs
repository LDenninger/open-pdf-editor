//! The rendering contract: what the user interface may assume about
//! rasterization, and what any rasterizer must provide.

use crate::Result;
use crate::error::Error;
use crate::page::{PageId, Rotation};

/// A request to rasterize one page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderRequest {
    /// Page to rasterize.
    pub page: PageId,
    /// Zoom factor, where 1.0 renders at 72 dpi — one pixel per PDF point.
    pub scale: f32,
    /// View rotation applied on top of the rotation stored on the page.
    pub rotation: Rotation,
}

impl RenderRequest {
    /// A request at the given scale with no additional view rotation.
    ///
    /// Returns [`Error::Unsupported`] for a scale that is not finite and positive.
    pub fn new(page: PageId, scale: f32) -> Result<Self> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(Error::Unsupported(format!("render scale {scale} must be finite and positive")));
        }
        Ok(Self {
            page,
            scale,
            rotation: Rotation::None,
        })
    }

    /// The same request with a view rotation applied.
    pub fn with_rotation(self, rotation: Rotation) -> Self {
        Self { rotation, ..self }
    }
}

/// A rasterized image, stored as 8-bit RGBA in row-major order.
#[derive(Clone, PartialEq, Debug)]
pub struct Tile {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Tile {
    /// Wrap a pixel buffer.
    ///
    /// Returns [`Error::Render`] if the buffer length does not equal
    /// `width * height * 4`, or if either dimension is zero.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::Render(format!("tile dimensions {width}x{height} must both be non-zero")));
        }
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(Error::Render(format!("tile buffer has {} bytes, expected {expected}", pixels.len())));
        }
        Ok(Self { width, height, pixels })
    }

    /// Width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The raw RGBA buffer, row-major, four bytes per pixel.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// One pixel as RGBA.
    ///
    /// Returns [`Error::Render`] if the coordinates fall outside the tile.
    pub fn pixel(&self, x_px: u32, y_px: u32) -> Result<[u8; 4]> {
        if x_px >= self.width || y_px >= self.height {
            return Err(Error::Render(format!(
                "pixel ({x_px}, {y_px}) is outside a {}x{} tile",
                self.width, self.height
            )));
        }
        let offset = (y_px as usize * self.width as usize + x_px as usize) * 4;
        Ok([self.pixels[offset], self.pixels[offset + 1], self.pixels[offset + 2], self.pixels[offset + 3]])
    }
}

/// The outcome of a submitted [`RenderRequest`].
#[derive(Clone, PartialEq, Debug)]
pub enum RenderResponse {
    /// Rasterization succeeded.
    Ready {
        /// The request this answers.
        request: RenderRequest,
        /// The rasterized image.
        tile: Tile,
    },
    /// Rasterization failed. The user interface shows a placeholder rather than
    /// treating this as fatal, because one damaged page must not close the document.
    Failed {
        /// The request this answers.
        request: RenderRequest,
        /// Human-readable cause.
        reason: String,
    },
}

impl RenderResponse {
    /// The request this response answers, whichever outcome it carries.
    pub const fn request(&self) -> &RenderRequest {
        match self {
            Self::Ready { request, .. } | Self::Failed { request, .. } => request,
        }
    }
}

/// Asynchronous rasterization, as seen by the user interface.
///
/// Implementations must never block in [`RenderService::poll`]: the caller runs
/// on the UI thread and a stalled poll drops frames.
pub trait RenderService: Send {
    /// Queue a request. Submitting the same request twice is permitted, and
    /// implementations may coalesce duplicates.
    fn submit(&self, request: RenderRequest);

    /// Collect every response completed since the last call, without blocking.
    fn poll(&self) -> Vec<RenderResponse>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_buffer_of_the_wrong_length() {
        let result = Tile::new(2, 2, vec![0; 8]);
        assert!(result.is_err());
    }

    #[test]
    fn accepts_a_buffer_of_the_exact_length() {
        let tile = Tile::new(2, 2, vec![0; 16]).unwrap();
        assert_eq!(tile.width(), 2);
        assert_eq!(tile.height(), 2);
    }

    #[test]
    fn reads_pixels_in_row_major_order() {
        let mut pixels = vec![0_u8; 16];
        pixels[4..8].copy_from_slice(&[1, 2, 3, 4]);
        let tile = Tile::new(2, 2, pixels).unwrap();
        assert_eq!(tile.pixel(1, 0).unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn rejects_pixels_outside_the_tile() {
        let tile = Tile::new(1, 1, vec![0; 4]).unwrap();
        assert!(tile.pixel(1, 0).is_err());
    }

    #[test]
    fn rejects_a_non_positive_scale() {
        assert!(RenderRequest::new(PageId::new(0), 0.0).is_err());
        assert!(RenderRequest::new(PageId::new(0), f32::NAN).is_err());
    }
}

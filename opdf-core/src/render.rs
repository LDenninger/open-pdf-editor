//! The rendering contract: what the user interface may assume about
//! rasterization, and what any rasterizer must provide.

use crate::Result;
use crate::error::Error;
use crate::page::{PageId, Rotation};

/// A request to rasterize one page.
///
/// This type is usable as a cache key: a tile cache and the UI's tile map both
/// key on it directly, rather than each inventing its own quantised-scale key.
///
/// `scale` participates in equality and hashing **bitwise**, through
/// [`f32::to_bits`]. Two requests whose scales differ only by floating-point
/// noise are therefore distinct keys, not the same one. A caller that wants
/// nearby scales to share a cache entry must quantise the scale itself before
/// building the request.
///
/// `revision` is what keeps a cache honest across edits: two requests that
/// differ only in revision are distinct keys, so a tile rasterized before a
/// structural change can never be served for the document as it stands now.
#[derive(Clone, Copy, Debug)]
pub struct RenderRequest {
    /// Page to rasterize.
    pub page: PageId,
    /// The value [`crate::document::Document::revision`] held when this request
    /// was built, normally read from a [`crate::document::DocumentSnapshot`].
    ///
    /// Opaque to the renderer, which neither validates nor interprets it — see
    /// [`RenderService`].
    pub revision: u64,
    /// Zoom factor, where 1.0 renders at 72 dpi — one pixel per PDF point.
    pub scale: f32,
    /// View rotation applied on top of the rotation stored on the page.
    pub rotation: Rotation,
}

impl RenderRequest {
    /// A request at the given scale and document revision, with no additional
    /// view rotation.
    ///
    /// `revision` is deliberately a required argument rather than a default or a
    /// builder step: a caller who omits it silently reintroduces stale tiles, so
    /// the decision is forced at the call site.
    ///
    /// Returns [`Error::Unsupported`] for a scale that is not finite and positive.
    pub fn new(page: PageId, revision: u64, scale: f32) -> Result<Self> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(Error::Unsupported(format!("render scale {scale} must be finite and positive")));
        }
        Ok(Self {
            page,
            revision,
            scale,
            rotation: Rotation::None,
        })
    }

    /// The same request with a view rotation applied.
    pub fn with_rotation(self, rotation: Rotation) -> Self {
        Self { rotation, ..self }
    }
}

//---------------------------------------------------------------------
// RenderRequest as a cache key: bitwise scale equality and hashing
//---------------------------------------------------------------------

impl PartialEq for RenderRequest {
    /// Compare `scale` bitwise, so that equality agrees with [`Hash`].
    fn eq(&self, other: &Self) -> bool {
        self.page == other.page && self.revision == other.revision && self.rotation == other.rotation && self.scale.to_bits() == other.scale.to_bits()
    }
}

impl Eq for RenderRequest {}

impl std::hash::Hash for RenderRequest {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.page.hash(state);
        self.revision.hash(state);
        self.rotation.hash(state);
        self.scale.to_bits().hash(state);
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
    /// `width * height * 4`, if either dimension is zero, or if
    /// `width * height * 4` overflows `usize` on the target — which it can do
    /// for plausible dimensions on a 32-bit target, where an unchecked
    /// multiplication would wrap and admit a buffer far too short for the
    /// dimensions claimed.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::Render(format!("tile dimensions {width}x{height} must both be non-zero")));
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixel_count| pixel_count.checked_mul(4))
            .ok_or_else(|| Error::Render(format!("tile dimensions {width}x{height} overflow the addressable buffer length")))?;
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
///
/// # A renderer must not validate `RenderRequest::revision`
///
/// The revision exists for the benefit of *caches*, not of the rasterizer. An
/// implementation carries it, and echoes it back unchanged inside the response's
/// request, so that a cache can tell an image of the old structure from an image
/// of the current one. It must never compare the revision against whatever state
/// it happens to hold, and must never fail, drop, or defer a request because the
/// two disagree.
///
/// This is a hard requirement rather than a convention: a real rasterizer may
/// legitimately hold several revisions at once — an in-flight request queued
/// before an edit, a snapshot taken after it — and a service that rejected
/// unfamiliar revisions would fail exactly the requests a cache most needs
/// answered. A service holding a snapshot at one revision must still answer a
/// request naming another.
pub trait RenderService: Send {
    /// Queue a request. Submitting the same request twice is permitted:
    /// identical pending requests may be answered by a single response, while
    /// distinct requests each receive their own response.
    ///
    /// The request's `revision` is carried, not checked — see the trait docs.
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
    fn rejects_dimensions_whose_buffer_length_overflows() {
        //--- u32::MAX squared fits a 64-bit usize, but the four bytes per pixel do not ---
        let result = Tile::new(u32::MAX, u32::MAX, Vec::new());
        assert!(
            matches!(result, Err(Error::Render(_))),
            "an overflowing buffer length must be reported as Error::Render, not accepted or panicked on"
        );
    }

    #[test]
    fn rejects_a_non_positive_scale() {
        assert!(RenderRequest::new(PageId::new(0), 0, 0.0).is_err());
        assert!(RenderRequest::new(PageId::new(0), 0, f32::NAN).is_err());
    }

    #[test]
    fn serves_as_a_hash_map_key() {
        let first = RenderRequest::new(PageId::new(1), 0, 1.0).unwrap();
        let same = RenderRequest::new(PageId::new(1), 0, 1.0).unwrap();
        let different_scale = RenderRequest::new(PageId::new(1), 0, 1.5).unwrap();

        let mut cache = std::collections::HashMap::new();
        cache.insert(first, "first");
        cache.insert(same, "same");
        cache.insert(different_scale, "different");

        assert_eq!(cache.len(), 2, "two equal requests must collapse to one key, a differing one must add a second");
        assert_eq!(cache[&first], "same", "an equal request must address the entry the first one created");
    }

    #[test]
    fn distinguishes_requests_by_revision() {
        let before = RenderRequest::new(PageId::new(1), 1, 1.0).unwrap();
        let after = RenderRequest::new(PageId::new(1), 2, 1.0).unwrap();

        assert_ne!(before, after, "requests differing only in revision must not compare equal");

        let mut cache = std::collections::HashMap::new();
        cache.insert(before, "stale");
        cache.insert(after, "current");

        assert_eq!(cache.len(), 2, "a tile cached before an edit must not be addressable after one");
        assert_eq!(cache[&before], "stale", "the pre-edit entry must survive under its own revision");
    }
}

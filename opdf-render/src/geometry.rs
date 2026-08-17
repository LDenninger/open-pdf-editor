//! Pixel geometry: turning a [`RenderRequest`] and a [`PageInfo`] into tile dimensions.
//!
//! The formula the contract pins is `round(size_pt * scale)`, rounded to
//! nearest and floored at one pixel, applied to the page's *display* size —
//! that is, the size after the page's stored rotation has been composed with
//! the request's view rotation.

use opdf_core::{PageInfo, RenderRequest, Rotation};

/// The pixel dimensions a request resolves to, plus the rotation those
/// dimensions already account for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TileGeometry {
    /// Tile width in pixels, never zero.
    pub width_px: u32,
    /// Tile height in pixels, never zero.
    pub height_px: u32,
    /// The page's stored rotation composed with the request's view rotation.
    pub total_rotation: Rotation,
}

/// Largest tile edge this renderer will produce, in pixels.
///
/// Pdfium takes bitmap dimensions as `i32`. Bounding each edge well below that
/// range keeps the conversion total and keeps a runaway zoom control from
/// reaching the allocator at all.
pub const MAX_TILE_EDGE: u32 = 32_768;

/// Largest tile this renderer will produce, in pixels — 64 megapixels, or
/// 256 MiB of RGBA.
///
/// Deliberately the same pixel budget [`opdf_core::fakes::FakeRenderService`]
/// applies, so the user interface sees one area limit rather than two. The two
/// renderers do not fail identically: [`MAX_TILE_EDGE`] is an additional
/// constraint with no analogue in the fake.
///
/// # What one tile at this ceiling actually costs
///
/// More than the three buffers this comment used to budget for. Two full-sized
/// buffers are live simultaneously at each of two moments in serving one
/// request:
///
/// 1. in [`crate::raster::rasterize_page`], Pdfium's own bitmap and the RGBA
///    copy taken out of it;
/// 2. in the render worker, the tile in the response and the copy
///    [`crate::cache::TileCache`] takes when it caches it — `insert` clones.
///
/// So 512 MiB is transiently resident twice over for a tile at this ceiling,
/// against the ~1 GiB the review measured end to end, and the cached copy then
/// occupies the whole of [`crate::cache::DEFAULT_CACHE_BYTES`] — the two
/// constants are equal, not four apart. See `DEFAULT_CACHE_BYTES` for why the
/// ceiling cannot simply come down.
pub const MAX_TILE_PIXELS: u64 = 64 * 1024 * 1024;

/// Resolve a request against a page's geometry.
///
/// Returns a human-readable reason, suitable for
/// [`opdf_core::RenderResponse::Failed`], when the request cannot be honoured.
pub fn compute_tile_geometry(page: PageInfo, request: &RenderRequest) -> Result<TileGeometry, String> {
    let total_rotation = page.rotation.rotated_by(request.rotation);
    let display = PageInfo {
        rotation: total_rotation,
        ..page
    }
    .display_size();

    let width_px = scale_edge(display.width_pt, request.scale)?;
    let height_px = scale_edge(display.height_pt, request.scale)?;

    let pixel_count = u64::from(width_px) * u64::from(height_px);
    if pixel_count > MAX_TILE_PIXELS {
        return Err(format!(
            "requested tile of {width_px}x{height_px} pixels at scale {} exceeds the renderer's limit of {MAX_TILE_PIXELS} pixels",
            request.scale
        ));
    }

    Ok(TileGeometry {
        width_px,
        height_px,
        total_rotation,
    })
}

/// Scale one edge from points to pixels: round to nearest, floor at one pixel.
fn scale_edge(size_pt: f32, scale: f32) -> Result<u32, String> {
    let scaled = f64::from(size_pt) * f64::from(scale);
    if !scaled.is_finite() {
        return Err(format!("scale {scale} applied to {size_pt} points does not produce a finite pixel count"));
    }
    let rounded = scaled.round().max(1.0);
    if rounded > f64::from(MAX_TILE_EDGE) {
        return Err(format!(
            "requested tile edge of {rounded} pixels at scale {scale} exceeds the renderer's limit of {MAX_TILE_EDGE} pixels"
        ));
    }
    Ok(rounded as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::{PageId, PageSize};

    fn build_page(rotation: Rotation) -> PageInfo {
        PageInfo {
            id: PageId::new(1),
            size: PageSize::A4,
            rotation,
        }
    }

    fn build_request(scale: f32) -> RenderRequest {
        RenderRequest::new(PageId::new(1), 7, scale).unwrap()
    }

    #[test]
    fn scales_an_unrotated_page_by_the_requested_factor() {
        let geometry = compute_tile_geometry(build_page(Rotation::None), &build_request(2.0)).unwrap();
        assert_eq!(geometry.width_px, 1190, "A4 width of 595 points at scale 2.0 must be 1190 pixels");
        assert_eq!(geometry.height_px, 1684, "A4 height of 842 points at scale 2.0 must be 1684 pixels");
    }

    #[test]
    fn rounds_to_nearest_rather_than_up() {
        //--- 595 * 0.51 = 303.45 and 842 * 0.51 = 429.42: rounding up would give 304x430 ---
        let geometry = compute_tile_geometry(build_page(Rotation::None), &build_request(0.51)).unwrap();
        assert_eq!(geometry.width_px, 303);
        assert_eq!(geometry.height_px, 429);
    }

    #[test]
    fn floors_a_vanishing_page_at_one_pixel() {
        let geometry = compute_tile_geometry(build_page(Rotation::None), &build_request(0.0005)).unwrap();
        assert_eq!(geometry.width_px, 1, "a tile must never have a zero dimension");
        assert_eq!(geometry.height_px, 1, "a tile must never have a zero dimension");
    }

    #[test]
    fn swaps_axes_for_a_quarter_turned_page() {
        let geometry = compute_tile_geometry(build_page(Rotation::Quarter), &build_request(1.0)).unwrap();
        assert_eq!(geometry.width_px, 842);
        assert_eq!(geometry.height_px, 595);
    }

    #[test]
    fn composes_view_rotation_with_stored_rotation() {
        //--- an unrotated page viewed at a quarter turn swaps its axes ---
        let request = build_request(1.0).with_rotation(Rotation::Quarter);
        let geometry = compute_tile_geometry(build_page(Rotation::None), &request).unwrap();
        assert_eq!((geometry.width_px, geometry.height_px), (842, 595));
        assert_eq!(geometry.total_rotation, Rotation::Quarter);

        //--- a quarter-turned page viewed at a further quarter turn composes to a half turn, restoring its axes ---
        let geometry = compute_tile_geometry(build_page(Rotation::Quarter), &request).unwrap();
        assert_eq!((geometry.width_px, geometry.height_px), (595, 842));
        assert_eq!(geometry.total_rotation, Rotation::Half);
    }

    #[test]
    fn fails_an_absurd_scale_instead_of_allocating_or_overflowing() {
        let reason = compute_tile_geometry(build_page(Rotation::None), &build_request(1e30)).unwrap_err();
        assert!(reason.contains("exceeds"), "the failure must name the limit it exceeded, got: {reason}");
    }

    #[test]
    fn fails_a_scale_whose_pixel_count_exceeds_the_budget_even_with_legal_edges() {
        //--- 595 x 842 points at scale 12.0 is 7140 x 10104 = 72.1 megapixels: both edges are legal, the area is not ---
        let reason = compute_tile_geometry(build_page(Rotation::None), &build_request(12.0)).unwrap_err();
        assert!(
            reason.contains(&MAX_TILE_PIXELS.to_string()),
            "the failure must name the pixel limit, got: {reason}"
        );
    }

    #[test]
    fn accepts_the_largest_scale_below_the_ceiling() {
        //--- 595 x 842 at scale 11.0 is 6545 x 9262 = 60.6 megapixels, just under the 64 megapixel limit ---
        let geometry = compute_tile_geometry(build_page(Rotation::None), &build_request(11.0)).unwrap();
        assert_eq!((geometry.width_px, geometry.height_px), (6545, 9262));
    }
}

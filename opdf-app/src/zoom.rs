//! Zoom: the ladder the buttons walk, the clamp everything passes through, the
//! anchoring that keeps the viewport where the user was looking, and the
//! quantisation that keeps the tile cache's key space finite.
//!
//! Like [`crate::layout`], this module refers to no egui type, so all of it is
//! tested headlessly.

/// Smallest zoom the viewer allows. Below this a page is unreadable and the
/// tiles are too small to be worth caching.
pub const MIN_ZOOM: f32 = 0.1;

/// Largest zoom the viewer allows.
///
/// This is a memory bound, not a taste judgement: `RenderRequest::new` accepts
/// any finite positive scale (see the contract reference's known gaps), and
/// `FakeRenderService` fails a request above 64 megapixels rather than
/// allocating it. Clamping here means the user never reaches that failure.
pub const MAX_ZOOM: f32 = 8.0;

/// The zoom levels the toolbar's plus and minus buttons walk, matching the
/// stops professional PDF viewers offer.
pub const ZOOM_STEPS: [f32; 13] = [0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];

/// Clamp a zoom factor into the supported range, mapping a non-finite value to 1.0.
pub fn clamp_zoom(zoom: f32) -> f32 {
    if !zoom.is_finite() {
        return 1.0;
    }
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// The next ladder stop above `zoom`, or [`MAX_ZOOM`] if there is none.
pub fn step_zoom_in(zoom: f32) -> f32 {
    let current = clamp_zoom(zoom);
    ZOOM_STEPS.iter().copied().find(|step| *step > current + 1e-4).unwrap_or(MAX_ZOOM)
}

/// The next ladder stop below `zoom`, or [`MIN_ZOOM`] if there is none.
pub fn step_zoom_out(zoom: f32) -> f32 {
    let current = clamp_zoom(zoom);
    ZOOM_STEPS.iter().copied().rev().find(|step| *step < current - 1e-4).unwrap_or(MIN_ZOOM)
}

//---------------------------------------------------------------------
// Anchoring and fit modes
//---------------------------------------------------------------------

/// The scroll offset that keeps the document point currently under `anchor_px`
/// still under `anchor_px` after the zoom changes.
///
/// `offset_px` is the current scroll offset and `anchor_px` is the distance from
/// the top of the viewport to the point the user is focused on — the pointer
/// during a wheel zoom, or half the viewport height for a keyboard zoom.
///
/// This is exact rather than approximate because [`crate::layout`] expresses the
/// whole content box in points, so screen position is `document_pt * zoom` with
/// no zoom-independent term to correct for.
pub fn anchor_scroll_offset(offset_px: f32, anchor_px: f32, old_zoom: f32, new_zoom: f32) -> f32 {
    if !old_zoom.is_finite() || old_zoom <= 0.0 {
        return offset_px.max(0.0);
    }
    let document_pt = (offset_px + anchor_px) / old_zoom;
    (document_pt * new_zoom - anchor_px).max(0.0)
}

/// The zoom at which the content box exactly fills the viewport's width.
///
/// Returns 1.0 for a degenerate content box or viewport, so a zero-page document
/// does not produce an infinite or NaN zoom.
pub fn fit_width_zoom(content_width_pt: f32, viewport_width_px: f32) -> f32 {
    if content_width_pt <= 0.0 || !viewport_width_px.is_finite() || viewport_width_px <= 0.0 {
        return 1.0;
    }
    clamp_zoom(viewport_width_px / content_width_pt)
}

/// The zoom at which one page fits entirely inside the viewport, whichever axis
/// binds first.
pub fn fit_page_zoom(page_width_pt: f32, page_height_pt: f32, viewport_width_px: f32, viewport_height_px: f32) -> f32 {
    if page_width_pt <= 0.0 || page_height_pt <= 0.0 {
        return 1.0;
    }
    let by_width = fit_width_zoom(page_width_pt, viewport_width_px);
    let by_height = if viewport_height_px <= 0.0 {
        1.0
    } else {
        viewport_height_px / page_height_pt
    };
    clamp_zoom(by_width.min(by_height))
}

//---------------------------------------------------------------------
// Render-scale quantisation: bounding the tile cache's key space
//---------------------------------------------------------------------

/// The scales tiles are actually rasterized at.
///
/// A continuous zoom must not become a continuous set of cache keys:
/// `RenderRequest` compares and hashes `scale` bitwise, so every distinct float
/// is a distinct entry and an unquantised pinch-zoom would allocate a fresh tile
/// per frame forever. Quantising to this ladder bounds each page to at most nine
/// cached tiles per revision.
pub const RENDER_SCALE_STEPS: [f32; 9] = [0.125, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];

/// The scale to rasterize at for a given effective scale (zoom multiplied by the
/// display's pixels-per-point).
///
/// Picks the smallest ladder step at or above `effective_scale`, so a tile is
/// downsampled rather than upscaled wherever the ladder allows, and clamps to the
/// top step — beyond which the tile is stretched, which costs sharpness but bounds
/// memory. Always returns a finite positive value suitable for
/// [`opdf_core::render::RenderRequest::new`].
pub fn quantize_render_scale(effective_scale: f32) -> f32 {
    if !effective_scale.is_finite() || effective_scale <= 0.0 {
        return RENDER_SCALE_STEPS[0];
    }
    RENDER_SCALE_STEPS
        .iter()
        .copied()
        .find(|step| *step >= effective_scale)
        .unwrap_or(RENDER_SCALE_STEPS[RENDER_SCALE_STEPS.len() - 1])
}

/// Round a scale to a fixed grid, so that scales computed per page — thumbnails,
/// which fit a target width rather than following the ladder — remain stable
/// cache keys across frames.
///
/// `steps_per_unit` is the grid's resolution: 256.0 rounds to the nearest 1/256.
/// Always returns a finite positive value.
pub fn quantize_scale(scale: f32, steps_per_unit: f32) -> f32 {
    if !scale.is_finite() || scale <= 0.0 || !steps_per_unit.is_finite() || steps_per_unit <= 0.0 {
        return RENDER_SCALE_STEPS[0];
    }
    ((scale * steps_per_unit).round() / steps_per_unit).max(1.0 / steps_per_unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_into_the_supported_range() {
        assert_eq!(clamp_zoom(0.001), MIN_ZOOM);
        assert_eq!(clamp_zoom(1000.0), MAX_ZOOM);
        assert_eq!(clamp_zoom(f32::NAN), 1.0, "a non-finite zoom must not propagate into a render request");
        assert_eq!(clamp_zoom(f32::INFINITY), 1.0);
    }

    #[test]
    fn walks_the_ladder_in_both_directions() {
        assert_eq!(step_zoom_in(1.0), 1.25);
        assert_eq!(step_zoom_out(1.0), 0.75);
        assert_eq!(step_zoom_in(MAX_ZOOM), MAX_ZOOM, "stepping in at the top must stay put, not wrap");
        assert_eq!(step_zoom_out(MIN_ZOOM), MIN_ZOOM, "stepping out at the bottom must stay put");
    }

    #[test]
    fn steps_off_a_value_between_two_stops() {
        assert_eq!(step_zoom_in(1.1), 1.25);
        assert_eq!(step_zoom_out(1.1), 1.0);
    }

    #[test]
    fn fits_the_content_box_to_the_viewport_width() {
        assert!((fit_width_zoom(635.0, 1270.0) - 2.0).abs() < 1e-4);
        assert_eq!(fit_width_zoom(0.0, 1270.0), 1.0, "an empty document must not produce an infinite zoom");
        assert_eq!(fit_width_zoom(635.0, 0.0), 1.0, "a zero-width viewport must not produce a zero zoom");
    }

    #[test]
    fn fits_a_page_by_whichever_axis_binds_first() {
        //--- width would allow 2.0, height only 1.0 ---
        assert!((fit_page_zoom(500.0, 800.0, 1000.0, 800.0) - 1.0).abs() < 1e-4);
        //--- height would allow 2.0, width only 1.0 ---
        assert!((fit_page_zoom(500.0, 400.0, 500.0, 800.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn keeps_the_anchored_point_under_the_pointer() {
        //--- the point 1300px down the content sits 300px below the viewport top ---
        let anchored = anchor_scroll_offset(1000.0, 300.0, 1.0, 2.0);
        assert!(
            (anchored - 2300.0).abs() < 1e-3,
            "doubling the zoom must double the document position and re-subtract the anchor, got {anchored}"
        );
    }

    #[test]
    fn leaves_the_offset_alone_when_the_zoom_does_not_change() {
        let anchored = anchor_scroll_offset(742.0, 311.0, 1.37, 1.37);
        assert!(
            (anchored - 742.0).abs() < 1e-2,
            "an identity zoom change must not drift the scroll offset, got {anchored}"
        );
    }

    #[test]
    fn never_anchors_to_a_negative_offset() {
        let anchored = anchor_scroll_offset(10.0, 400.0, 4.0, 0.25);
        assert!(anchored >= 0.0, "zooming far out at the top of a document must clamp at zero, got {anchored}");
    }

    #[test]
    fn survives_a_degenerate_previous_zoom() {
        assert_eq!(anchor_scroll_offset(500.0, 100.0, 0.0, 2.0), 500.0);
        assert_eq!(anchor_scroll_offset(500.0, 100.0, f32::NAN, 2.0), 500.0);
    }

    #[test]
    fn quantises_render_scale_up_to_the_nearest_ladder_step() {
        assert_eq!(quantize_render_scale(1.0), 1.0);
        assert_eq!(
            quantize_render_scale(1.01),
            1.5,
            "a scale between stops must round up, so the tile is downsampled not upscaled"
        );
        assert_eq!(quantize_render_scale(0.05), 0.125);
        assert_eq!(quantize_render_scale(99.0), 4.0, "the ladder's top step is the memory bound");
    }

    #[test]
    fn quantising_a_render_scale_is_idempotent() {
        for step in [0.03_f32, 0.4, 1.0, 1.7, 3.9, 12.0] {
            let once = quantize_render_scale(step);
            assert_eq!(quantize_render_scale(once), once, "quantising twice must not move the value, at {step}");
        }
    }

    #[test]
    fn quantising_a_render_scale_is_monotone() {
        let mut previous = 0.0_f32;
        for ii in 0..400 {
            let quantised = quantize_render_scale(ii as f32 * 0.02);
            assert!(quantised >= previous, "quantisation must never decrease as the input rises");
            previous = quantised;
        }
    }

    #[test]
    fn always_produces_a_scale_a_render_request_accepts() {
        for step in [f32::NAN, f32::INFINITY, -1.0, 0.0, 1e30] {
            let quantised = quantize_render_scale(step);
            assert!(
                quantised.is_finite() && quantised > 0.0,
                "quantising {step} produced {quantised}, which RenderRequest::new would reject"
            );
        }
        for step in [f32::NAN, -3.0, 0.0] {
            let quantised = quantize_scale(step, 256.0);
            assert!(quantised.is_finite() && quantised > 0.0, "quantising {step} produced {quantised}");
        }
    }

    #[test]
    fn rounds_a_free_scale_onto_a_stable_grid() {
        let first = quantize_scale(132.0 / 595.0, 256.0);
        let second = quantize_scale(132.0 / 595.0 + 1e-7, 256.0);
        assert_eq!(
            first.to_bits(),
            second.to_bits(),
            "two nearly equal scales must produce a bit-identical key, or the cache mints a new entry per frame"
        );
    }
}

//! Turning an [`opdf_core::render::Tile`] into something egui can draw, and
//! filing it in the cache — or throwing it away because the document has moved on.
//!
//! This is where the **stale-tile guard** lives. A response carries the request it
//! answers, and that request carries the revision it was built at. A response whose
//! revision is not the one currently being drawn is discarded here rather than
//! cached, so a tile rasterized before a structural change can never reach the
//! screen after one.

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use opdf_core::render::{RenderRequest, RenderResponse, Tile};

use crate::tiles::cache::TileCache;

/// The concrete cache the canvas and the rail hold: rendered tiles as egui textures.
pub type TextureCache = TileCache<TextureHandle>;

/// What one call to [`absorb_responses`] did, for the status bar and for tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AbsorbReport {
    /// Responses turned into textures and cached.
    pub stored: usize,
    /// Responses the render service reported as failed.
    pub failed: usize,
    /// Responses discarded because they answered a superseded revision.
    pub discarded: usize,
    /// Cache entries evicted to stay within budget afterwards.
    pub evicted: usize,
}

/// Bytes a tile occupies once uploaded: four bytes per pixel.
///
/// Saturating rather than wrapping, so an implausible tile inflates the budget
/// and gets evicted rather than wrapping to a small number and being kept forever.
pub fn measure_tile_bytes(tile: &Tile) -> usize {
    (tile.width() as usize).saturating_mul(tile.height() as usize).saturating_mul(4)
}

/// Copy a tile's RGBA buffer into an egui image.
///
/// [`Tile`] guarantees `pixels().len() == width * height * 4`, which is exactly
/// what `from_rgba_unmultiplied` requires, so this cannot fail. It does copy and
/// premultiply, which is a per-pixel cost paid once per tile rather than once per
/// frame — the resulting texture is reused until it is evicted.
pub fn build_color_image(tile: &Tile) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([tile.width() as usize, tile.height() as usize], tile.pixels())
}

/// A stable debug name for a texture, shown in egui's texture inspector.
pub fn name_texture(request: &RenderRequest) -> String {
    format!("tile:{}:r{}:s{}", request.page, request.revision, request.scale)
}

//---------------------------------------------------------------------
// Draining the render service
//---------------------------------------------------------------------

/// File every response into `cache`, discarding any that answers a revision other
/// than `revision`, then evict down to budget.
///
/// `revision` must come from the [`opdf_core::document::DocumentSnapshot`] the
/// caller is about to draw — not from a separately tracked field, which can drift
/// from the snapshot and reintroduce the stale-tile bug the revision exists to
/// prevent.
///
/// A `Failed` response is not an error the caller must handle: rasterization can
/// fail per page without that being fatal to the document, so the request is
/// cleared from the pending set (making it retryable) and the canvas shows a
/// placeholder.
pub fn absorb_responses(cache: &mut TextureCache, ctx: &Context, revision: u64, responses: Vec<RenderResponse>, protected_since: u64) -> AbsorbReport {
    let mut report = AbsorbReport::default();
    for response in responses {
        let request = *response.request();
        if request.revision != revision {
            cache.clear_pending(&request);
            report.discarded += 1;
            continue;
        }
        match response {
            RenderResponse::Ready { tile, .. } => {
                let bytes = measure_tile_bytes(&tile);
                let texture = ctx.load_texture(name_texture(&request), build_color_image(&tile), TextureOptions::LINEAR);
                cache.insert(request, texture, bytes);
                report.stored += 1;
            }
            RenderResponse::Failed { .. } => {
                cache.clear_pending(&request);
                report.failed += 1;
            }
        }
    }
    report.evicted = cache.evict_to_budget(protected_since);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::page::PageId;

    fn build_tile(width: u32, height: u32, fill: u8) -> Tile {
        Tile::new(width, height, vec![fill; (width * height * 4) as usize]).unwrap()
    }

    fn build_request(page: u64, revision: u64, scale: f32) -> RenderRequest {
        RenderRequest::new(PageId::new(page), revision, scale).unwrap()
    }

    #[test]
    fn measures_four_bytes_per_pixel() {
        assert_eq!(measure_tile_bytes(&build_tile(16, 8, 255)), 512);
    }

    #[test]
    fn converts_a_tile_at_its_exact_dimensions() {
        let image = build_color_image(&build_tile(16, 8, 200));
        assert_eq!(image.size, [16, 8]);
        assert_eq!(image.pixels.len(), 128);
    }

    #[test]
    fn names_textures_distinctly_across_page_revision_and_scale() {
        let base = name_texture(&build_request(1, 1, 1.0));
        assert_ne!(base, name_texture(&build_request(2, 1, 1.0)));
        assert_ne!(base, name_texture(&build_request(1, 2, 1.0)));
        assert_ne!(base, name_texture(&build_request(1, 1, 2.0)));
    }

    #[test]
    fn stores_a_ready_response_for_the_current_revision() {
        let ctx = Context::default();
        let mut cache = TextureCache::new(1 << 20);
        let request = build_request(1, 7, 1.0);
        let frame = cache.begin_frame();
        let report = absorb_responses(
            &mut cache,
            &ctx,
            7,
            vec![RenderResponse::Ready {
                request,
                tile: build_tile(8, 8, 255),
            }],
            frame,
        );
        assert_eq!(report.stored, 1);
        assert!(cache.contains(&request));
        assert_eq!(cache.used_bytes(), 256);
    }

    #[test]
    fn discards_a_tile_answering_a_superseded_revision() {
        let ctx = Context::default();
        let mut cache = TextureCache::new(1 << 20);
        let stale = build_request(1, 6, 1.0);
        cache.mark_pending(stale);
        let frame = cache.begin_frame();
        let report = absorb_responses(
            &mut cache,
            &ctx,
            7,
            vec![RenderResponse::Ready {
                request: stale,
                tile: build_tile(8, 8, 255),
            }],
            frame,
        );
        assert_eq!(report.discarded, 1);
        assert_eq!(report.stored, 0);
        assert!(
            !cache.contains(&stale),
            "a tile from before the edit must never enter the cache the canvas draws from"
        );
        assert_eq!(cache.pending_count(), 0, "a discarded response must still release its pending slot");
    }

    #[test]
    fn makes_a_failed_request_retryable_without_caching_anything() {
        let ctx = Context::default();
        let mut cache = TextureCache::new(1 << 20);
        let request = build_request(1, 7, 1.0);
        cache.mark_pending(request);
        let frame = cache.begin_frame();
        let report = absorb_responses(
            &mut cache,
            &ctx,
            7,
            vec![RenderResponse::Failed {
                request,
                reason: "damaged page".to_owned(),
            }],
            frame,
        );
        assert_eq!(report.failed, 1);
        assert!(!cache.contains(&request), "a failed render must not cache an empty texture");
        assert!(cache.mark_pending(request), "a failed request must be retryable on a later frame");
    }

    #[test]
    fn evicts_after_absorbing_so_a_scroll_burst_stays_bounded() {
        let ctx = Context::default();
        let mut cache = TextureCache::new(1_024);
        for ii in 0..8_u64 {
            let frame = cache.begin_frame();
            absorb_responses(
                &mut cache,
                &ctx,
                7,
                vec![RenderResponse::Ready {
                    request: build_request(ii, 7, 1.0),
                    tile: build_tile(16, 16, 255),
                }],
                frame,
            );
        }
        assert!(
            cache.used_bytes() <= 1_024,
            "absorbing must leave the cache within budget, got {} bytes",
            cache.used_bytes()
        );
    }

    #[test]
    fn absorbs_an_empty_batch_without_touching_the_cache() {
        let ctx = Context::default();
        let mut cache = TextureCache::new(1 << 20);
        let frame = cache.begin_frame();
        let report = absorb_responses(&mut cache, &ctx, 7, Vec::new(), frame);
        assert_eq!(report, AbsorbReport::default(), "polling an idle service every frame must be free");
    }
}

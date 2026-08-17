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
/// fail per page without that being fatal to the document. The request is
/// recorded as refused rather than merely cleared — a refusal is an answer, and
/// asking again every frame is a loop that never settles — and the canvas draws
/// that page as refused rather than as still loading.
pub fn absorb_responses(cache: &mut TextureCache, ctx: &Context, revision: u64, responses: Vec<RenderResponse>, protected_since: u64) -> AbsorbReport {
    absorb_responses_routed(&mut [cache], ctx, revision, responses, protected_since)
}

/// File every response into **whichever cache asked for it**, discarding any that
/// answers a revision other than `revision`, then evict every cache to budget.
///
/// A [`opdf_core::render::RenderService`] is a single worker owning a single
/// document — the rasterizer is not thread-safe, so there can be only one — and
/// one call to `poll` therefore drains the answers to *every* surface's requests
/// at once. The canvas and the thumbnail rail hold separate caches with separate
/// budgets, so a response has to be routed back to the cache that submitted it;
/// a response nobody claims falls to `caches[0]`, the canvas.
///
/// Filing everything into one cache instead is the bug this function exists to
/// prevent: the unclaimed cache's requests stay pending forever and its surface
/// draws placeholders that never resolve.
pub fn absorb_responses_routed(
    caches: &mut [&mut TextureCache],
    ctx: &Context,
    revision: u64,
    responses: Vec<RenderResponse>,
    protected_since: u64,
) -> AbsorbReport {
    let mut report = AbsorbReport::default();
    for response in responses {
        let request = *response.request();
        //--- the cache that asked, or the canvas if the request is no longer claimed ---
        let owner = caches.iter().position(|cache| cache.is_pending(&request)).unwrap_or(0);
        let Some(cache) = caches.get_mut(owner) else {
            continue;
        };

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
                //--- recorded as refused, not merely as not-yet-arrived: a request
                //--- only cleared is one the scheduler submits again next frame ---
                cache.note_failed(request);
                report.failed += 1;
            }
        }
    }
    //--- `protected_since` belongs to caches[0]'s clock, and a clock is per-cache:
    //--- every other cache is evicted by whoever calls `begin_frame` on it ---
    if let Some(canvas) = caches.first_mut() {
        report.evicted = canvas.evict_to_budget(protected_since);
    }
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

    /// A refusal is an answer, and the request must not come back.
    ///
    /// The rasterizer resolves a page through the index map frozen when the file
    /// was opened, so a page it cannot place will not become placeable by being
    /// asked again. Merely releasing the pending slot leaves the scheduler free to
    /// resubmit on the very next frame: a page that stays a placeholder, a status
    /// bar stuck on "Rendering 1 page", and an event loop that never sleeps.
    #[test]
    fn records_a_failed_request_as_answered_rather_than_as_still_wanted() {
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
        assert_eq!(cache.pending_count(), 0, "a failed response must release the request it answers");
        assert!(
            cache.has_failed(&request),
            "the canvas needs to know this page failed, not merely that it is absent"
        );
        assert!(
            !cache.mark_pending(request),
            "a refused request submitted again every frame is a loop that never settles"
        );
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
    fn files_a_response_into_the_cache_that_asked_for_it() {
        let ctx = Context::default();
        let mut canvas = TextureCache::new(1 << 20);
        let mut rail = TextureCache::new(1 << 20);
        let page_request = build_request(1, 7, 1.0);
        let thumbnail_request = build_request(1, 7, 0.25);
        canvas.mark_pending(page_request);
        rail.mark_pending(thumbnail_request);

        //--- one poll of one service answers both surfaces at once ---
        let frame = canvas.begin_frame();
        let report = absorb_responses_routed(
            &mut [&mut canvas, &mut rail],
            &ctx,
            7,
            vec![
                RenderResponse::Ready {
                    request: page_request,
                    tile: build_tile(8, 8, 255),
                },
                RenderResponse::Ready {
                    request: thumbnail_request,
                    tile: build_tile(4, 4, 255),
                },
            ],
            frame,
        );

        assert_eq!(report.stored, 2);
        assert!(canvas.contains(&page_request), "the canvas must receive the page it asked for");
        assert!(rail.contains(&thumbnail_request), "the rail must receive the thumbnail it asked for");
        assert!(
            !canvas.contains(&thumbnail_request),
            "a thumbnail must not be filed into the canvas cache: the rail would then wait forever for a tile it already has an answer to"
        );
        assert_eq!(rail.pending_count(), 0, "the rail's request must leave its pending set");
    }

    #[test]
    fn files_an_unclaimed_response_into_the_canvas_cache() {
        let ctx = Context::default();
        let mut canvas = TextureCache::new(1 << 20);
        let mut rail = TextureCache::new(1 << 20);
        let orphan = build_request(3, 7, 1.0);
        let frame = canvas.begin_frame();

        let report = absorb_responses_routed(
            &mut [&mut canvas, &mut rail],
            &ctx,
            7,
            vec![RenderResponse::Ready {
                request: orphan,
                tile: build_tile(8, 8, 255),
            }],
            frame,
        );

        assert_eq!(report.stored, 1);
        assert!(canvas.contains(&orphan), "a response nobody is waiting for falls to the canvas");
        assert!(rail.is_empty());
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

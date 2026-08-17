//! Deciding which pages to ask the render service for this frame.
//!
//! Requests are always built from the [`DocumentSnapshot`] currently being drawn,
//! so neither the document they name nor the revision they carry can drift from
//! the structure they describe. The function signature enforces that: there is no
//! document parameter and no revision parameter to get wrong.

use std::ops::Range;

use opdf_core::document::DocumentSnapshot;
use opdf_core::page::{PageSize, Rotation};
use opdf_core::render::RenderRequest;

/// Most requests submitted in one frame.
///
/// A fling-scroll through a long document sweeps hundreds of pages through the
/// viewport in a second. Without a cap, each frame queues every page it passed and
/// the render service spends the next minute rasterizing pixels nobody will ever
/// see. Capping means the pages nearest the user's focus are asked for first and
/// the rest are asked for on later frames, if they are still visible.
pub const MAX_SUBMISSIONS_PER_FRAME: usize = 8;

/// What one frame decided to submit.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RequestPlan {
    /// Requests to hand to [`opdf_core::render::RenderService::submit`], nearest
    /// the focus first.
    pub requests: Vec<RenderRequest>,
    /// How many wanted requests did not fit in this frame's budget. Non-zero means
    /// the caller should schedule another repaint.
    pub skipped: usize,
}

/// How this frame wants its pages rasterized.
///
/// Bundled rather than passed loose so that adding a rasterization concern does
/// not grow every call site's argument list.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderSettings {
    /// Scale to rasterize at, already quantised by
    /// [`crate::zoom::quantize_render_scale`]. Clamped per page against
    /// `max_texture_side` before a request is built.
    pub render_scale: f32,
    /// The *additional* rotation the user applied to the view. The page's own
    /// stored rotation is applied by the renderer and must not be repeated here.
    pub view_rotation: Rotation,
    /// Longest texture edge the graphics backend accepts, in pixels, from
    /// `egui::InputState::max_texture_side`.
    pub max_texture_side: usize,
}

impl RenderSettings {
    /// The scale to actually ask for when rasterizing a page of this display size.
    ///
    /// See [`crate::zoom::fit_render_scale_to_texture_limit`]: the ladder bounds
    /// the scale, but only the page's own size bounds the tile.
    pub fn scale_for_page(&self, display_size: PageSize) -> f32 {
        crate::zoom::fit_render_scale_to_texture_limit(self.render_scale, display_size.width_pt, display_size.height_pt, self.max_texture_side)
    }
}

/// Visible page indices ordered by distance from `focus`, nearest first, breaking
/// ties toward the later page so that scrolling down feels like it fills ahead.
///
/// This is what makes the first frames after a jump feel immediate: the page the
/// user is looking at is requested before the overscan band above and below it.
pub fn order_by_distance_from_focus(visible: Range<usize>, focus: usize) -> Vec<usize> {
    let mut indices: Vec<usize> = visible.collect();
    indices.sort_by_key(|index| (index.abs_diff(focus), *index < focus));
    indices
}

//---------------------------------------------------------------------
// Planning a frame's requests
//---------------------------------------------------------------------

/// Build the requests this frame should submit.
///
/// `visible` is the page range from [`crate::layout::find_visible_pages`], already
/// widened by the overscan band. `focus` is the page the user is on, used only for
/// ordering. `settings` carries the frame's quantised scale, view rotation, and
/// texture-size limit.
///
/// `is_wanted` decides whether a request still needs submitting — in practice
/// `|request| cache.wants(request)`, which is `false` for anything already
/// cached or already in flight.
///
/// It is deliberately a `Fn`, not a `FnMut`: **planning must not record
/// anything.** Marking a request as in flight while planning meant every
/// request that then failed to fit `budget` was left recorded as in flight and
/// never submitted, so nothing could ever clear it — a permanently blank page
/// and a repaint loop that never settles. The caller marks pending for exactly
/// the requests it goes on to submit.
///
/// At most `budget` requests are returned; the rest are counted in
/// [`RequestPlan::skipped`].
pub fn plan_render_requests(
    snapshot: &DocumentSnapshot,
    visible: Range<usize>,
    focus: usize,
    settings: RenderSettings,
    is_wanted: &dyn Fn(&RenderRequest) -> bool,
    budget: usize,
) -> RequestPlan {
    let mut plan = RequestPlan::default();
    for index in order_by_distance_from_focus(visible, focus) {
        let Some(page) = snapshot.pages.get(index) else {
            continue;
        };
        //--- both the document and the revision come off the snapshot being drawn, never from a caller-held field ---
        let Ok(request) = RenderRequest::new(snapshot.document, page.id, snapshot.revision, settings.scale_for_page(page.display_size())) else {
            continue;
        };
        let request = request.with_rotation(settings.view_rotation);
        if !is_wanted(&request) {
            continue;
        }
        if plan.requests.len() >= budget {
            plan.skipped += 1;
            continue;
        }
        plan.requests.push(request);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::build_synthetic_snapshot;
    use std::collections::HashSet;

    fn accept_everything() -> impl Fn(&RenderRequest) -> bool {
        |_request: &RenderRequest| true
    }

    /// Frame settings at scale 1.0 with a texture limit no synthetic page reaches.
    fn settings(view_rotation: Rotation) -> RenderSettings {
        RenderSettings {
            render_scale: 1.0,
            view_rotation,
            max_texture_side: 16_384,
        }
    }

    #[test]
    fn orders_the_focus_page_first_and_neighbours_outward() {
        assert_eq!(order_by_distance_from_focus(0..7, 3), vec![3, 4, 2, 5, 1, 6, 0]);
    }

    #[test]
    fn orders_a_focus_outside_the_visible_range_from_the_nearest_end() {
        assert_eq!(order_by_distance_from_focus(10..14, 0), vec![10, 11, 12, 13]);
    }

    #[test]
    fn orders_an_empty_range_to_nothing() {
        assert!(order_by_distance_from_focus(5..5, 5).is_empty());
    }

    #[test]
    fn submits_the_focus_page_before_the_overscan_band() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 4..12, 8, settings(Rotation::None), &accept, MAX_SUBMISSIONS_PER_FRAME);
        assert_eq!(
            plan.requests[0].page, snapshot.pages[8].id,
            "the page the user is looking at must be asked for first"
        );
    }

    #[test]
    fn caps_a_fling_scroll_at_the_frame_budget() {
        let snapshot = build_synthetic_snapshot(500).unwrap();
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..500, 250, settings(Rotation::None), &accept, MAX_SUBMISSIONS_PER_FRAME);
        assert_eq!(plan.requests.len(), MAX_SUBMISSIONS_PER_FRAME);
        assert_eq!(
            plan.skipped,
            500 - MAX_SUBMISSIONS_PER_FRAME,
            "everything that did not fit must be counted, so the caller knows to repaint"
        );
    }

    #[test]
    fn skips_requests_the_cache_already_holds() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        let already_cached: HashSet<u64> = snapshot.pages[4..8].iter().map(|page| page.id.get()).collect();
        let is_wanted = |request: &RenderRequest| !already_cached.contains(&request.page.get());
        let plan = plan_render_requests(&snapshot, 0..12, 6, settings(Rotation::None), &is_wanted, 32);
        assert_eq!(plan.requests.len(), 8, "four of the twelve visible pages are cached");
        assert!(plan.requests.iter().all(|request| !already_cached.contains(&request.page.get())));
    }

    #[test]
    fn builds_every_request_at_the_snapshots_own_revision() {
        let mut snapshot = build_synthetic_snapshot(6).unwrap();
        snapshot.revision = 41;
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..6, 0, settings(Rotation::None), &accept, 32);
        assert!(
            plan.requests.iter().all(|request| request.revision == 41),
            "a request must carry the revision of the snapshot it was planned from"
        );
    }

    #[test]
    fn a_new_revision_produces_keys_that_cannot_hit_the_old_entries() {
        let mut before = build_synthetic_snapshot(4).unwrap();
        before.revision = 1;
        let mut after = before.clone();
        after.revision = 2;

        let accept = accept_everything();
        let old_plan = plan_render_requests(&before, 0..4, 0, settings(Rotation::None), &accept, 32);
        let accept = accept_everything();
        let new_plan = plan_render_requests(&after, 0..4, 0, settings(Rotation::None), &accept, 32);

        for (old, new) in old_plan.requests.iter().zip(new_plan.requests.iter()) {
            assert_ne!(old, new, "after a revision change, no planned request may equal one planned before it");
        }
    }

    #[test]
    fn leaves_the_view_rotation_at_none_by_default_so_pages_are_not_rotated_twice() {
        let snapshot = build_synthetic_snapshot(4).unwrap();
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..4, 0, settings(Rotation::None), &accept, 32);
        assert!(
            plan.requests.iter().all(|request| request.rotation == Rotation::None),
            "the page's stored rotation is applied by the renderer; repeating it in the request would rotate the tile twice"
        );
    }

    #[test]
    fn carries_a_user_applied_view_rotation_into_every_request() {
        let snapshot = build_synthetic_snapshot(4).unwrap();
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..4, 0, settings(Rotation::Quarter), &accept, 32);
        assert!(plan.requests.iter().all(|request| request.rotation == Rotation::Quarter));
    }

    #[test]
    fn never_asks_for_a_tile_the_backend_cannot_upload() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        let accept = accept_everything();
        //--- scale 4 against a 2048 limit: the 1200-point page would be 4800px tall ---
        let tight = RenderSettings {
            render_scale: 4.0,
            view_rotation: Rotation::None,
            max_texture_side: 2048,
        };
        let plan = plan_render_requests(&snapshot, 0..20, 0, tight, &accept, 64);
        assert!(!plan.requests.is_empty(), "capping must shrink requests, not drop them");

        for request in &plan.requests {
            let page = snapshot.pages.iter().find(|page| page.id == request.page).unwrap();
            let size = page.display_size();
            let longest_px = size.width_pt.max(size.height_pt) * request.scale;
            assert!(
                longest_px <= 2048.0,
                "page {} would rasterize to {longest_px}px at scale {}, past the backend's 2048 limit",
                request.page,
                request.scale
            );
        }
    }

    #[test]
    fn caps_only_the_pages_that_need_it() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        let accept = accept_everything();
        let tight = RenderSettings {
            render_scale: 2.0,
            view_rotation: Rotation::None,
            max_texture_side: 2048,
        };
        let plan = plan_render_requests(&snapshot, 0..20, 0, tight, &accept, 64);
        let scales: HashSet<u32> = plan.requests.iter().map(|request| request.scale.to_bits()).collect();
        assert!(
            scales.len() > 1,
            "a page that fits must keep the full scale while an oversized one is reduced; every page got the same scale"
        );
        assert!(
            plan.requests.iter().any(|request| request.scale == 2.0),
            "pages small enough to fit must not be penalised for their neighbours"
        );
    }

    #[test]
    fn plans_nothing_for_an_empty_document() {
        let snapshot = DocumentSnapshot::default();
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..0, 0, settings(Rotation::None), &accept, 32);
        assert_eq!(plan, RequestPlan::default());
    }

    #[test]
    fn ignores_indices_past_the_end_of_the_snapshot() {
        let snapshot = build_synthetic_snapshot(3).unwrap();
        let accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..99, 0, settings(Rotation::None), &accept, 32);
        assert_eq!(plan.requests.len(), 3, "a stale visible range must not panic or invent pages");
    }
}

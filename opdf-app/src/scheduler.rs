//! Deciding which pages to ask the render service for this frame.
//!
//! Requests are always built from the [`DocumentSnapshot`] currently being drawn,
//! so the revision they carry cannot drift from the structure they describe. The
//! function signature enforces that: there is no revision parameter to get wrong.

use std::ops::Range;

use opdf_core::document::DocumentSnapshot;
use opdf_core::page::Rotation;
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
/// ordering. `render_scale` must already be quantised by
/// [`crate::zoom::quantize_render_scale`], or the cache mints a new key every frame.
/// `view_rotation` is the *additional* rotation the user applied to the view; the
/// page's own stored rotation is applied by the renderer and must not be passed here.
///
/// `is_wanted` decides whether a request still needs submitting — in practice
/// `|request| cache.mark_pending(*request)`, which returns `false` for anything
/// already cached or already in flight, and records the submission as it goes.
///
/// At most `budget` requests are returned; the rest are counted in
/// [`RequestPlan::skipped`].
pub fn plan_render_requests(
    snapshot: &DocumentSnapshot,
    visible: Range<usize>,
    focus: usize,
    render_scale: f32,
    view_rotation: Rotation,
    is_wanted: &mut dyn FnMut(&RenderRequest) -> bool,
    budget: usize,
) -> RequestPlan {
    let mut plan = RequestPlan::default();
    for index in order_by_distance_from_focus(visible, focus) {
        let Some(page) = snapshot.pages.get(index) else {
            continue;
        };
        //--- the revision comes off the snapshot being drawn, never from a caller-held field ---
        let Ok(request) = RenderRequest::new(page.id, snapshot.revision, render_scale) else {
            continue;
        };
        let request = request.with_rotation(view_rotation);
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

    fn accept_everything() -> impl FnMut(&RenderRequest) -> bool {
        |_request: &RenderRequest| true
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
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 4..12, 8, 1.0, Rotation::None, &mut accept, MAX_SUBMISSIONS_PER_FRAME);
        assert_eq!(
            plan.requests[0].page, snapshot.pages[8].id,
            "the page the user is looking at must be asked for first"
        );
    }

    #[test]
    fn caps_a_fling_scroll_at_the_frame_budget() {
        let snapshot = build_synthetic_snapshot(500).unwrap();
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..500, 250, 1.0, Rotation::None, &mut accept, MAX_SUBMISSIONS_PER_FRAME);
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
        let mut is_wanted = |request: &RenderRequest| !already_cached.contains(&request.page.get());
        let plan = plan_render_requests(&snapshot, 0..12, 6, 1.0, Rotation::None, &mut is_wanted, 32);
        assert_eq!(plan.requests.len(), 8, "four of the twelve visible pages are cached");
        assert!(plan.requests.iter().all(|request| !already_cached.contains(&request.page.get())));
    }

    #[test]
    fn builds_every_request_at_the_snapshots_own_revision() {
        let mut snapshot = build_synthetic_snapshot(6).unwrap();
        snapshot.revision = 41;
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..6, 0, 1.0, Rotation::None, &mut accept, 32);
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

        let mut accept = accept_everything();
        let old_plan = plan_render_requests(&before, 0..4, 0, 1.0, Rotation::None, &mut accept, 32);
        let mut accept = accept_everything();
        let new_plan = plan_render_requests(&after, 0..4, 0, 1.0, Rotation::None, &mut accept, 32);

        for (old, new) in old_plan.requests.iter().zip(new_plan.requests.iter()) {
            assert_ne!(old, new, "after a revision change, no planned request may equal one planned before it");
        }
    }

    #[test]
    fn leaves_the_view_rotation_at_none_by_default_so_pages_are_not_rotated_twice() {
        let snapshot = build_synthetic_snapshot(4).unwrap();
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..4, 0, 1.0, Rotation::None, &mut accept, 32);
        assert!(
            plan.requests.iter().all(|request| request.rotation == Rotation::None),
            "the page's stored rotation is applied by the renderer; repeating it in the request would rotate the tile twice"
        );
    }

    #[test]
    fn carries_a_user_applied_view_rotation_into_every_request() {
        let snapshot = build_synthetic_snapshot(4).unwrap();
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..4, 0, 1.0, Rotation::Quarter, &mut accept, 32);
        assert!(plan.requests.iter().all(|request| request.rotation == Rotation::Quarter));
    }

    #[test]
    fn plans_nothing_for_an_empty_document() {
        let snapshot = DocumentSnapshot::default();
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..0, 0, 1.0, Rotation::None, &mut accept, 32);
        assert_eq!(plan, RequestPlan::default());
    }

    #[test]
    fn ignores_indices_past_the_end_of_the_snapshot() {
        let snapshot = build_synthetic_snapshot(3).unwrap();
        let mut accept = accept_everything();
        let plan = plan_render_requests(&snapshot, 0..99, 0, 1.0, Rotation::None, &mut accept, 32);
        assert_eq!(plan.requests.len(), 3, "a stale visible range must not panic or invent pages");
    }
}

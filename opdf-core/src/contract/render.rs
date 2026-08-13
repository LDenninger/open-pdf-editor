//! Behavioural contract that every [`RenderService`] implementation must satisfy.

use crate::document::DocumentSnapshot;
use crate::page::{PageId, PageInfo, PageSize, Rotation};
use crate::render::{RenderRequest, RenderResponse, RenderService};

/// Poll until `expected` responses have arrived or the deadline passes.
///
/// Contract implementations may answer asynchronously, so a single `poll` is
/// not enough. The deadline keeps a broken implementation from hanging the suite.
fn drain_responses<S: RenderService>(service: &S, expected: usize) -> Vec<RenderResponse> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut collected = Vec::new();
    while collected.len() < expected {
        collected.extend(service.poll());
        if collected.len() >= expected || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    collected
}

/// The revision the suite's snapshot is captured at.
///
/// Deliberately not zero, so that an implementation which quietly assumes a
/// fresh document fails here rather than in a track's own tests.
const SNAPSHOT_REVISION: u64 = 7;

/// Build a two-page snapshot the contract suite renders against.
fn build_snapshot() -> DocumentSnapshot {
    DocumentSnapshot {
        revision: SNAPSHOT_REVISION,
        pages: vec![
            PageInfo {
                id: PageId::new(1),
                size: PageSize::A4,
                rotation: Rotation::None,
            },
            PageInfo {
                id: PageId::new(2),
                size: PageSize::A4,
                rotation: Rotation::Quarter,
            },
        ],
    }
}

/// Assert that an implementation honours the [`RenderService`] contract.
///
/// `make_service` receives the snapshot the service must be able to render.
///
/// # Panics
///
/// Panics with a descriptive message on the first violated requirement.
pub fn assert_render_service_contract<S, F>(make_service: F)
where
    S: RenderService,
    F: Fn(DocumentSnapshot) -> S,
{
    assert_polling_an_idle_service_is_empty(&make_service);
    assert_every_request_is_answered_once(&make_service);
    assert_tile_dimensions_follow_scale(&make_service);
    assert_pixel_dimensions_round_to_nearest_with_a_one_pixel_floor(&make_service);
    assert_rotation_swaps_tile_axes(&make_service);
    assert_unknown_pages_fail_without_panicking(&make_service);
    assert_view_rotation_composes_with_page_rotation(&make_service);
    assert_batched_requests_each_receive_a_response(&make_service);
    assert_a_foreign_revision_is_still_answered(&make_service);
    assert_revision_distinguishes_cache_keys();
}

//---------------------------------------------------------------------
// Revision handling
//---------------------------------------------------------------------

/// The renderer carries the revision; it never validates it.
///
/// A service holding a snapshot at one revision must still answer a request
/// naming another, because a real rasterizer may hold several at once — a
/// request queued before an edit, a snapshot taken after it. The response must
/// echo the requested revision back unchanged, so a cache can file the tile
/// under the key it asked for.
fn assert_a_foreign_revision_is_still_answered<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    let foreign = SNAPSHOT_REVISION.wrapping_add(1);
    let request = RenderRequest::new(PageId::new(1), foreign, 1.0).expect("scale 1.0 is valid");
    service.submit(request);

    let responses = drain_responses(&service, 1);
    assert_eq!(
        responses.len(),
        1,
        "a request naming a revision the service does not hold must still be answered"
    );
    assert_eq!(
        responses[0].request().revision,
        foreign,
        "the response must echo the requested revision unchanged, not substitute the one the service holds"
    );
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(tile.width(), 595, "a foreign revision must not change how the page is rasterized");
            assert_eq!(tile.height(), 842, "a foreign revision must not change how the page is rasterized");
        }
        RenderResponse::Failed { reason, .. } => {
            panic!("a renderer must not reject a request because its revision differs from the snapshot it holds, got: {reason}")
        }
    }
}

/// Two requests differing only in revision must be distinct cache keys.
///
/// This is the whole point of the field: a tile rasterized before a structural
/// change must not be addressable by a request built after one.
fn assert_revision_distinguishes_cache_keys() {
    let before = RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 1.0).expect("scale 1.0 is valid");
    let after = RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION.wrapping_add(1), 1.0).expect("scale 1.0 is valid");

    assert_ne!(before, after, "requests differing only in revision must not compare equal");

    let mut cache = std::collections::HashMap::new();
    cache.insert(before, "stale");
    cache.insert(after, "current");
    assert_eq!(cache.len(), 2, "requests differing only in revision must occupy distinct HashMap keys");
    assert_eq!(cache[&before], "stale", "the pre-change entry must remain addressable under its own revision");
}

fn assert_polling_an_idle_service_is_empty<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    assert!(service.poll().is_empty(), "polling before submitting must return nothing");
}

fn assert_every_request_is_answered_once<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    let request = RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 1.0).expect("scale 1.0 is valid");
    service.submit(request);

    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "each submitted request must produce exactly one response");
    assert_eq!(*responses[0].request(), request, "a response must identify the request it answers");
    //--- a direct poll, not a drain: the requirement is that nothing more arrives, so waiting for it would defeat the check ---
    assert!(service.poll().is_empty(), "a response must not be delivered twice");
}

fn assert_tile_dimensions_follow_scale<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 2.0).expect("scale 2.0 is valid"));

    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "a request at scale 2.0 must produce exactly one response");
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(tile.width(), 1190, "A4 width of 595 points at scale 2.0 must be 1190 pixels");
            assert_eq!(tile.height(), 1684, "A4 height of 842 points at scale 2.0 must be 1684 pixels");
        }
        RenderResponse::Failed { reason, .. } => panic!("rendering a valid page must succeed, got: {reason}"),
    }
}

/// Pin the pixel-dimension formula: `round(size_pt * scale)`, floored at one pixel.
///
/// Scale `0.51` distinguishes rounding to nearest from rounding up, and scale
/// `0.0005` proves the floor produces a 1x1 tile rather than a zero-sized one.
fn assert_pixel_dimensions_round_to_nearest_with_a_one_pixel_floor<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    //--- 595 * 0.51 = 303.45 and 842 * 0.51 = 429.42: rounding up would give 304x430 ---
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 0.51).expect("scale 0.51 is valid"));

    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "a request at scale 0.51 must produce exactly one response");
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(tile.width(), 303, "595 points at scale 0.51 is 303.45, which rounds to nearest as 303");
            assert_eq!(tile.height(), 429, "842 points at scale 0.51 is 429.42, which rounds to nearest as 429");
        }
        RenderResponse::Failed { reason, .. } => panic!("rendering at a fractional scale must succeed, got: {reason}"),
    }

    //--- 595 * 0.0005 = 0.2975 and 842 * 0.0005 = 0.421: both round to zero and must be floored to one ---
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 0.0005).expect("scale 0.0005 is valid"));

    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "a request at scale 0.0005 must produce exactly one response");
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(
                tile.width(),
                1,
                "a scale that rounds the width below one pixel must be floored to a 1-pixel width"
            );
            assert_eq!(
                tile.height(),
                1,
                "a scale that rounds the height below one pixel must be floored to a 1-pixel height"
            );
        }
        RenderResponse::Failed { reason, .. } => panic!("rendering at a tiny scale must succeed with a 1x1 tile, got: {reason}"),
    }
}

fn assert_rotation_swaps_tile_axes<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(2), SNAPSHOT_REVISION, 1.0).expect("scale 1.0 is valid"));

    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "a request for a rotated page must produce exactly one response");
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(tile.width(), 842, "a quarter-turned A4 page must be 842 pixels wide at scale 1.0");
            assert_eq!(tile.height(), 595, "a quarter-turned A4 page must be 595 pixels tall at scale 1.0");
        }
        RenderResponse::Failed { reason, .. } => panic!("rendering a rotated page must succeed, got: {reason}"),
    }
}

fn assert_unknown_pages_fail_without_panicking<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(u64::MAX), SNAPSHOT_REVISION, 1.0).expect("scale 1.0 is valid"));

    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "an unknown page must still produce a response");
    assert!(
        matches!(responses[0], RenderResponse::Failed { .. }),
        "an unknown page must report failure rather than a tile"
    );
}

fn assert_view_rotation_composes_with_page_rotation<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    //--- an unrotated page viewed at a quarter turn must swap its axes ---
    let service = make_service(build_snapshot());
    service.submit(
        RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 1.0)
            .expect("scale 1.0 is valid")
            .with_rotation(Rotation::Quarter),
    );
    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "a request carrying a view rotation must produce exactly one response");
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(tile.width(), 842, "an unrotated A4 page viewed at a quarter turn must be 842 pixels wide");
            assert_eq!(tile.height(), 595, "an unrotated A4 page viewed at a quarter turn must be 595 pixels tall");
        }
        RenderResponse::Failed { reason, .. } => panic!("rendering with a view rotation must succeed, got: {reason}"),
    }

    //--- a quarter-turned page viewed at a further quarter turn composes to a half turn, restoring its axes ---
    let service = make_service(build_snapshot());
    service.submit(
        RenderRequest::new(PageId::new(2), SNAPSHOT_REVISION, 1.0)
            .expect("scale 1.0 is valid")
            .with_rotation(Rotation::Quarter),
    );
    let responses = drain_responses(&service, 1);
    assert_eq!(responses.len(), 1, "a request composing two rotations must produce exactly one response");
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(
                tile.width(),
                595,
                "a quarter-turned A4 page viewed at a further quarter turn must be 595 pixels wide"
            );
            assert_eq!(
                tile.height(),
                842,
                "a quarter-turned A4 page viewed at a further quarter turn must be 842 pixels tall"
            );
        }
        RenderResponse::Failed { reason, .. } => panic!("composing page and view rotation must succeed, got: {reason}"),
    }
}

fn assert_batched_requests_each_receive_a_response<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    let first = RenderRequest::new(PageId::new(1), SNAPSHOT_REVISION, 1.0).expect("scale 1.0 is valid");
    let second = RenderRequest::new(PageId::new(2), SNAPSHOT_REVISION, 1.0).expect("scale 1.0 is valid");
    service.submit(first);
    service.submit(second);

    let responses = drain_responses(&service, 2);
    assert_eq!(responses.len(), 2, "every submitted request must receive its own response");
    assert!(
        responses.iter().any(|response| *response.request() == first),
        "the first submitted request must be answered"
    );
    assert!(
        responses.iter().any(|response| *response.request() == second),
        "the second submitted request must be answered"
    );
    //--- a direct poll, not a drain: the requirement is that nothing more arrives, so waiting for it would defeat the check ---
    assert!(service.poll().is_empty(), "a drained batch must not be delivered again");
}

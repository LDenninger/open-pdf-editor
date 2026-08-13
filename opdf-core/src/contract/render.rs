//! Behavioural contract that every [`RenderService`] implementation must satisfy.

use crate::document::DocumentSnapshot;
use crate::page::{PageId, PageInfo, PageSize, Rotation};
use crate::render::{RenderRequest, RenderResponse, RenderService};

/// Build a two-page snapshot the contract suite renders against.
fn build_snapshot() -> DocumentSnapshot {
    DocumentSnapshot {
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
    assert_rotation_swaps_tile_axes(&make_service);
    assert_unknown_pages_fail_without_panicking(&make_service);
}

fn assert_polling_an_idle_service_is_empty<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    assert!(service.poll().is_empty(), "polling before submitting must return nothing");
}

fn assert_every_request_is_answered_once<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    let request = RenderRequest::new(PageId::new(1), 1.0).expect("scale 1.0 is valid");
    service.submit(request);

    let responses = service.poll();
    assert_eq!(responses.len(), 1, "each submitted request must produce exactly one response");
    assert_eq!(*responses[0].request(), request, "a response must identify the request it answers");
    assert!(service.poll().is_empty(), "a response must not be delivered twice");
}

fn assert_tile_dimensions_follow_scale<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(1), 2.0).expect("scale 2.0 is valid"));

    let responses = service.poll();
    match &responses[0] {
        RenderResponse::Ready { tile, .. } => {
            assert_eq!(tile.width(), 1190, "A4 width of 595 points at scale 2.0 must be 1190 pixels");
            assert_eq!(tile.height(), 1684, "A4 height of 842 points at scale 2.0 must be 1684 pixels");
        }
        RenderResponse::Failed { reason, .. } => panic!("rendering a valid page must succeed, got: {reason}"),
    }
}

fn assert_rotation_swaps_tile_axes<S: RenderService, F: Fn(DocumentSnapshot) -> S>(make_service: &F) {
    let service = make_service(build_snapshot());
    service.submit(RenderRequest::new(PageId::new(2), 1.0).expect("scale 1.0 is valid"));

    let responses = service.poll();
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
    service.submit(RenderRequest::new(PageId::new(u64::MAX), 1.0).expect("scale 1.0 is valid"));

    let responses = service.poll();
    assert_eq!(responses.len(), 1, "an unknown page must still produce a response");
    assert!(
        matches!(responses[0], RenderResponse::Failed { .. }),
        "an unknown page must report failure rather than a tile"
    );
}

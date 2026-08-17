//! The render worker thread — the only code in the crate that touches Pdfium
//! outside its own tests.
//!
//! Pdfium is not thread-safe, and neither is a document handle capable of
//! reading a page's object graph. One thread owns both for the lifetime of the
//! service, and the rest of the program talks to it over channels.
//!
//! Owning the document on one thread is necessary but not sufficient: several
//! services, and therefore several worker threads, exist at once. Every call
//! into Pdfium — including the drops that close a document or a page — is
//! therefore made while holding [`crate::library::lock_pdfium`]. The lock is
//! taken per operation rather than for the worker's lifetime, so that one
//! worker cannot starve another.
//!
//! # Revisions
//!
//! A request's geometry is resolved against whatever snapshot is current when
//! the worker renders it. That makes the backlog and the snapshot a single
//! piece of state: a request queued under one snapshot must never be answered
//! under the next one. See [`accept_message`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use opdf_core::{DocumentSnapshot, PageId, PageInfo, RenderRequest, RenderResponse, Rotation};
use pdfium_render::prelude::{PdfDocument, PdfPageIndex, PdfPageRenderRotation};

use crate::backlog::{Backlog, MAX_BACKLOG};
use crate::cache::{DEFAULT_CACHE_BYTES, TileCache};
use crate::geometry::compute_tile_geometry;
use crate::library::{bind_pdfium, lock_pdfium};
use crate::raster::rasterize_page;

/// What the handle sends the worker.
#[derive(Debug)]
pub(crate) enum WorkerMessage {
    /// Rasterize one page.
    Render(RenderRequest),
    /// Replace the snapshot the worker resolves page identities against.
    Rebind(Box<DocumentSnapshot>),
    /// Stop, discarding anything still queued.
    Shutdown,
}

/// Own Pdfium and the document, and answer requests until told to stop.
pub(crate) fn run_worker(
    pdf_path: PathBuf,
    snapshot: DocumentSnapshot,
    requests: Receiver<WorkerMessage>,
    responses: Sender<RenderResponse>,
    ready: Sender<std::result::Result<(), String>>,
    rasterizations: Arc<AtomicU64>,
) {
    let pdfium = match bind_pdfium() {
        Ok(pdfium) => pdfium,
        Err(reason) => {
            let _ = ready.send(Err(reason));
            return;
        }
    };

    let document = {
        let _guard = lock_pdfium();
        match pdfium.load_pdf_from_file(&pdf_path, None) {
            Ok(document) => document,
            Err(error) => {
                let _ = ready.send(Err(format!("could not open {}: {error}", pdf_path.display())));
                return;
            }
        }
    };

    //--- captured before the first edit can move a page: this is the one
    //--- moment the snapshot's order and the file's order are the same ---
    let file_indices = map_file_indices(&snapshot);

    if ready.send(Ok(())).is_ok() {
        serve_requests(&document, snapshot, &file_indices, &requests, &responses, &rasterizations);
    }

    close_document(document);
}

/// Where each page sits in the file Pdfium has open.
///
/// Pdfium addresses pages by their position in the file it loaded, and that
/// file never changes while the worker runs — a save produces a new service.
/// The snapshot, by contrast, changes on every edit: `move_page` reorders it,
/// `remove_page` shortens it, `insert_page` extends it. Resolving a request by
/// its page's *current* snapshot position therefore addressed a different page
/// after any edit, and the user saw another page's content under the page they
/// asked for.
///
/// The mapping is taken once, from the snapshot the document was opened with,
/// and is never rebuilt — a [`WorkerMessage::Rebind`] describes new in-memory
/// state, not a new file.
fn map_file_indices(snapshot: &DocumentSnapshot) -> HashMap<PageId, usize> {
    snapshot.pages.iter().enumerate().map(|(position, page)| (page.id, position)).collect()
}

/// Answer requests until the channel closes or a shutdown arrives.
fn serve_requests(
    document: &PdfDocument<'_>,
    snapshot: DocumentSnapshot,
    file_indices: &HashMap<PageId, usize>,
    requests: &Receiver<WorkerMessage>,
    responses: &Sender<RenderResponse>,
    rasterizations: &AtomicU64,
) {
    let mut snapshot = snapshot;
    let mut backlog = Backlog::with_capacity(MAX_BACKLOG);
    let mut cache = TileCache::with_budget(DEFAULT_CACHE_BYTES);
    let mut is_shutting_down = false;

    loop {
        //--- block only when there is nothing to do, so an idle worker costs no cpu ---
        if backlog.is_empty() {
            match requests.recv() {
                Ok(message) => {
                    if !accept_message(message, &mut backlog, &mut snapshot, &mut cache, responses) {
                        is_shutting_down = true;
                    }
                }
                Err(_) => return,
            }
        }

        //--- take everything that has already arrived, so a newer request can overtake an older one ---
        for message in requests.try_iter() {
            if !accept_message(message, &mut backlog, &mut snapshot, &mut cache, responses) {
                is_shutting_down = true;
            }
        }

        if is_shutting_down {
            return;
        }

        if let Some(request) = backlog.take_newest() {
            let response = match cache.get(&request) {
                Some(tile) => RenderResponse::Ready { request, tile },
                None => {
                    let response = answer_request(document, &snapshot, file_indices, request, rasterizations);
                    if let RenderResponse::Ready { tile, .. } = &response {
                        cache.insert(request, tile);
                    }
                    response
                }
            };
            if responses.send(response).is_err() {
                return;
            }
        }
    }
}

/// Apply one message. Returns `false` when the worker has been told to stop.
///
/// A request evicted to keep the backlog bounded is answered immediately, so
/// that every submitted request still receives exactly one response.
///
/// # A rebind supersedes the backlog
///
/// The worker resolves a request's geometry against whatever snapshot is
/// current when it *renders* it, not when it accepted it. A request left in the
/// backlog across a [`WorkerMessage::Rebind`] would therefore be rendered
/// against the new snapshot and — because the cache is keyed on the request,
/// which carries the old revision — stored under the revision it did not
/// belong to. A revision 7 request came back with revision 8's swapped axes,
/// and every later revision 7 request was served that tile from the cache.
///
/// So a rebind drains the backlog, answering everything in it
/// [`RenderResponse::Failed`]. Failing is right rather than merely convenient:
/// the caller has already moved to the new revision and will resubmit, whereas
/// re-rendering against the retired snapshot would spend a rasterization on a
/// tile nothing will ask for again. Requests that arrive *after* the rebind are
/// untouched — they belong to the new snapshot.
///
/// The worker renders one request to completion before it reads the next
/// message, so there is never a request in flight when a rebind is applied: the
/// backlog is the whole set of requests the rebind can strand.
fn accept_message(
    message: WorkerMessage,
    backlog: &mut Backlog,
    snapshot: &mut DocumentSnapshot,
    cache: &mut TileCache,
    responses: &Sender<RenderResponse>,
) -> bool {
    match message {
        WorkerMessage::Render(request) => {
            if let Some(superseded) = backlog.push(request) {
                let _ = responses.send(RenderResponse::Failed {
                    request: superseded,
                    reason: format!("superseded: more than {MAX_BACKLOG} render requests were queued ahead of this one"),
                });
            }
            true
        }
        WorkerMessage::Rebind(replacement) => {
            //--- drained before the snapshot moves, so nothing queued under the old one can be answered under the new ---
            while let Some(superseded) = backlog.take_newest() {
                let _ = responses.send(RenderResponse::Failed {
                    request: superseded,
                    reason: format!(
                        "superseded by a rebind: the document moved from revision {} to revision {} while this request was queued, and answering it now would return the new revision's geometry",
                        snapshot.revision, replacement.revision
                    ),
                });
            }
            *snapshot = *replacement;
            cache.retain_revision(snapshot.revision);
            true
        }
        WorkerMessage::Shutdown => false,
    }
}

/// Close the document, which is a Pdfium call like any other.
fn close_document(document: PdfDocument<'_>) {
    let _guard = lock_pdfium();
    drop(document);
}

/// Resolve, rasterize, and package one request. Never panics.
fn answer_request(
    document: &PdfDocument<'_>,
    snapshot: &DocumentSnapshot,
    file_indices: &HashMap<PageId, usize>,
    request: RenderRequest,
    rasterizations: &AtomicU64,
) -> RenderResponse {
    //--- geometry comes from the *current* snapshot, so an unsaved rotation
    //--- shows immediately; the index comes from the file, which cannot move ---
    let Some(page_info) = find_page(snapshot, &request) else {
        return RenderResponse::Failed {
            request,
            reason: format!("unknown page {}", request.page),
        };
    };

    let geometry = match compute_tile_geometry(page_info, &request) {
        Ok(geometry) => geometry,
        Err(reason) => return RenderResponse::Failed { request, reason },
    };

    let Some(&position) = file_indices.get(&request.page) else {
        return RenderResponse::Failed {
            request,
            reason: format!(
                "page {} is not in the file this service opened — it was created after the document was opened, and an unsaved page has nothing to rasterize",
                request.page
            ),
        };
    };

    let index: PdfPageIndex = match position.try_into() {
        Ok(index) => index,
        Err(_) => {
            return RenderResponse::Failed {
                request,
                reason: format!("page position {position} is beyond the range pdfium can address"),
            };
        }
    };

    //--- declared before the page it guards, so the page's own closing call is still covered ---
    let _guard = lock_pdfium();

    let pages = document.pages();
    let page = match pages.get(index) {
        Ok(page) => page,
        Err(error) => {
            return RenderResponse::Failed {
                request,
                reason: format!("pdfium has no page at index {index}: {error}"),
            };
        }
    };

    let file_rotation = match page.rotation() {
        Ok(rotation) => convert_file_rotation(rotation),
        Err(error) => {
            return RenderResponse::Failed {
                request,
                reason: format!("pdfium could not report the rotation of page {index}: {error}"),
            };
        }
    };

    match rasterize_page(&page, geometry, file_rotation) {
        Ok(tile) => {
            rasterizations.fetch_add(1, Ordering::Relaxed);
            RenderResponse::Ready { request, tile }
        }
        Err(reason) => RenderResponse::Failed { request, reason },
    }
}

/// The geometry a request's page currently has.
///
/// Deliberately does not report a position: a snapshot position is not a file
/// position once the document has been edited. Use [`map_file_indices`] for
/// the index Pdfium needs.
fn find_page(snapshot: &DocumentSnapshot, request: &RenderRequest) -> Option<PageInfo> {
    snapshot.pages.iter().find(|page| page.id == request.page).copied()
}

/// Translate the rotation Pdfium reports for a page into a core rotation.
fn convert_file_rotation(rotation: PdfPageRenderRotation) -> Rotation {
    match rotation {
        PdfPageRenderRotation::None => Rotation::None,
        PdfPageRenderRotation::Degrees90 => Rotation::Quarter,
        PdfPageRenderRotation::Degrees180 => Rotation::Half,
        PdfPageRenderRotation::Degrees270 => Rotation::ThreeQuarter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use opdf_core::{PageSize, Rotation};

    fn build_snapshot(revision: u64, rotation: Rotation) -> DocumentSnapshot {
        DocumentSnapshot {
            revision,
            pages: vec![PageInfo {
                id: PageId::new(1),
                size: PageSize::A4,
                rotation,
            }],
        }
    }

    fn build_request(revision: u64) -> RenderRequest {
        RenderRequest::new(PageId::new(1), revision, 1.0).unwrap()
    }

    /// A request accepted while one snapshot was current, then left in the
    /// backlog while a rebind swapped that snapshot out, used to be rendered
    /// against the *new* snapshot and cached under its own — now superseded —
    /// revision key. The tile carried the new revision's geometry under the old
    /// revision's name, and every later request for the old revision was served
    /// that tile from the cache.
    ///
    /// A rebind therefore supersedes the whole backlog: nothing queued before it
    /// may be answered after it.
    #[test]
    fn a_rebind_supersedes_every_request_queued_before_it() {
        let (responses_tx, responses_rx) = unbounded::<RenderResponse>();
        let mut backlog = Backlog::with_capacity(MAX_BACKLOG);
        let mut snapshot = build_snapshot(7, Rotation::None);
        let mut cache = TileCache::with_budget(DEFAULT_CACHE_BYTES);

        let queued = build_request(7);
        assert!(accept_message(
            WorkerMessage::Render(queued),
            &mut backlog,
            &mut snapshot,
            &mut cache,
            &responses_tx
        ));
        assert!(!backlog.is_empty(), "the request must be queued before the rebind arrives");

        let rebind = WorkerMessage::Rebind(Box::new(build_snapshot(8, Rotation::Quarter)));
        assert!(accept_message(rebind, &mut backlog, &mut snapshot, &mut cache, &responses_tx));

        assert!(
            backlog.is_empty(),
            "a request queued at revision 7 must not survive into revision 8; rendering it now would answer it with revision 8 geometry and cache it under revision 7"
        );

        let answered: Vec<RenderResponse> = responses_rx.try_iter().collect();
        assert_eq!(answered.len(), 1, "every superseded request must still receive exactly one response");
        assert_eq!(*answered[0].request(), queued, "the response must name the request that was superseded");
        match &answered[0] {
            RenderResponse::Failed { reason, .. } => assert!(reason.contains("rebind"), "the reason must name the cause, got: {reason}"),
            RenderResponse::Ready { tile, .. } => panic!("a superseded request must not be answered with a {}x{} tile", tile.width(), tile.height()),
        }
    }

    /// The backlog is drained on rebind, but a request that arrives *after* the
    /// rebind belongs to the new snapshot and must be served normally.
    #[test]
    fn a_request_queued_after_a_rebind_is_untouched() {
        let (responses_tx, responses_rx) = unbounded::<RenderResponse>();
        let mut backlog = Backlog::with_capacity(MAX_BACKLOG);
        let mut snapshot = build_snapshot(7, Rotation::None);
        let mut cache = TileCache::with_budget(DEFAULT_CACHE_BYTES);

        let rebind = WorkerMessage::Rebind(Box::new(build_snapshot(8, Rotation::Quarter)));
        accept_message(rebind, &mut backlog, &mut snapshot, &mut cache, &responses_tx);
        accept_message(WorkerMessage::Render(build_request(8)), &mut backlog, &mut snapshot, &mut cache, &responses_tx);

        assert_eq!(
            backlog.take_newest(),
            Some(build_request(8)),
            "a request queued after the rebind must still be served"
        );
        assert!(responses_rx.try_iter().next().is_none(), "nothing queued after the rebind may be failed");
    }
}

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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use opdf_core::{DocumentSnapshot, PageInfo, RenderRequest, RenderResponse, Rotation};
use pdfium_render::prelude::{PdfDocument, PdfPageIndex, PdfPageRenderRotation};

use crate::backlog::{Backlog, MAX_BACKLOG};
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

    if ready.send(Ok(())).is_ok() {
        serve_requests(&document, snapshot, &requests, &responses, &rasterizations);
    }

    close_document(document);
}

/// Answer requests until the channel closes or a shutdown arrives.
fn serve_requests(
    document: &PdfDocument<'_>,
    snapshot: DocumentSnapshot,
    requests: &Receiver<WorkerMessage>,
    responses: &Sender<RenderResponse>,
    rasterizations: &AtomicU64,
) {
    let mut snapshot = snapshot;
    let mut backlog = Backlog::with_capacity(MAX_BACKLOG);
    let mut is_shutting_down = false;

    loop {
        //--- block only when there is nothing to do, so an idle worker costs no cpu ---
        if backlog.is_empty() {
            match requests.recv() {
                Ok(message) => {
                    if !accept_message(message, &mut backlog, &mut snapshot, responses) {
                        is_shutting_down = true;
                    }
                }
                Err(_) => return,
            }
        }

        //--- take everything that has already arrived, so a newer request can overtake an older one ---
        for message in requests.try_iter() {
            if !accept_message(message, &mut backlog, &mut snapshot, responses) {
                is_shutting_down = true;
            }
        }

        if is_shutting_down {
            return;
        }

        if let Some(request) = backlog.take_newest() {
            let response = answer_request(document, &snapshot, request, rasterizations);
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
fn accept_message(message: WorkerMessage, backlog: &mut Backlog, snapshot: &mut DocumentSnapshot, responses: &Sender<RenderResponse>) -> bool {
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
            *snapshot = *replacement;
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
fn answer_request(document: &PdfDocument<'_>, snapshot: &DocumentSnapshot, request: RenderRequest, rasterizations: &AtomicU64) -> RenderResponse {
    let Some((position, page_info)) = find_page(snapshot, &request) else {
        return RenderResponse::Failed {
            request,
            reason: format!("unknown page {}", request.page),
        };
    };

    let geometry = match compute_tile_geometry(page_info, &request) {
        Ok(geometry) => geometry,
        Err(reason) => return RenderResponse::Failed { request, reason },
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

/// The position of a request's page in the snapshot, and its geometry.
fn find_page(snapshot: &DocumentSnapshot, request: &RenderRequest) -> Option<(usize, PageInfo)> {
    snapshot
        .pages
        .iter()
        .position(|page| page.id == request.page)
        .map(|position| (position, snapshot.pages[position]))
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

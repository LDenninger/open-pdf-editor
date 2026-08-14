//! The [`RenderService`] handle the user interface holds.
//!
//! The handle owns no Pdfium state at all: it owns a sender, a receiver, and a
//! join handle. [`RenderService::submit`] pushes onto an unbounded channel and
//! returns; [`RenderService::poll`] drains a receiver with `try_iter` and
//! returns whatever has arrived. Neither ever blocks, which is the contract's
//! hard requirement — a stalled poll drops frames on the UI thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use opdf_core::{DocumentSnapshot, Error, RenderRequest, RenderResponse, RenderService, Result};

use crate::worker::{WorkerMessage, run_worker};

/// A Pdfium-backed rasterizer running on its own thread.
///
/// Dropping the service shuts the worker down and joins it, so a dropped
/// service leaves no thread and no open document behind.
#[derive(Debug)]
pub struct PdfiumRenderService {
    requests: Sender<WorkerMessage>,
    responses: Receiver<RenderResponse>,
    /// Responses produced by the handle itself, when the worker is unreachable.
    outbox: Mutex<Vec<RenderResponse>>,
    rasterizations: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PdfiumRenderService {
    /// Open `pdf_path` and answer requests for the pages `snapshot` describes.
    ///
    /// The nth `PageId` in `snapshot.pages` is Pdfium page index `n` — see the
    /// crate documentation. Blocks until the worker reports that the document
    /// opened, so a missing or malformed file is an error here rather than a
    /// failure on every subsequent request.
    pub fn open(pdf_path: &Path, snapshot: DocumentSnapshot) -> Result<Self> {
        let (request_tx, request_rx) = unbounded::<WorkerMessage>();
        let (response_tx, response_rx) = unbounded::<RenderResponse>();
        let (ready_tx, ready_rx) = bounded::<std::result::Result<(), String>>(1);

        let rasterizations = Arc::new(AtomicU64::new(0));
        let worker_counter = Arc::clone(&rasterizations);
        let worker_path: PathBuf = pdf_path.to_path_buf();

        let worker = std::thread::Builder::new()
            .name("opdf-render".to_string())
            .spawn(move || run_worker(worker_path, snapshot, request_rx, response_tx, ready_tx, worker_counter))
            .map_err(Error::Io)?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                requests: request_tx,
                responses: response_rx,
                outbox: Mutex::new(Vec::new()),
                rasterizations,
                worker: Some(worker),
            }),
            Ok(Err(reason)) => {
                let _ = worker.join();
                Err(Error::Render(reason))
            }
            Err(_) => Err(Error::Render("the render worker exited before reporting readiness".to_string())),
        }
    }

    /// Point the worker at a new snapshot, after a structural edit.
    ///
    /// Requests already queued are unaffected — the renderer never validates a
    /// revision, it only carries it.
    pub fn rebind(&self, snapshot: DocumentSnapshot) {
        let _ = self.requests.send(WorkerMessage::Rebind(Box::new(snapshot)));
    }

    /// How many tiles the worker has actually rasterized.
    ///
    /// Cache hits do not advance it, which is what makes the tile cache
    /// observable from a test without reaching inside the worker.
    pub fn rasterizations(&self) -> u64 {
        self.rasterizations.load(Ordering::Relaxed)
    }
}

impl RenderService for PdfiumRenderService {
    fn submit(&self, request: RenderRequest) {
        if self.requests.send(WorkerMessage::Render(request)).is_err() {
            //--- the worker is gone; answer here rather than leaving the request unanswered forever ---
            if let Ok(mut outbox) = self.outbox.lock() {
                outbox.push(RenderResponse::Failed {
                    request,
                    reason: "the render worker is no longer running".to_string(),
                });
            }
        }
    }

    fn poll(&self) -> Vec<RenderResponse> {
        let mut collected: Vec<RenderResponse> = match self.outbox.lock() {
            Ok(mut outbox) => std::mem::take(&mut *outbox),
            Err(_) => Vec::new(),
        };
        collected.extend(self.responses.try_iter());
        collected
    }
}

impl Drop for PdfiumRenderService {
    fn drop(&mut self) {
        let _ = self.requests.send(WorkerMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::ensure_contract_fixture;
    use opdf_core::{PageId, PageInfo, PageSize, Rotation};

    fn build_snapshot() -> DocumentSnapshot {
        DocumentSnapshot {
            revision: 7,
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

    fn build_service() -> PdfiumRenderService {
        PdfiumRenderService::open(&ensure_contract_fixture(), build_snapshot()).unwrap()
    }

    fn drain(service: &PdfiumRenderService, expected: usize) -> Vec<RenderResponse> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut collected = Vec::new();
        while collected.len() < expected && std::time::Instant::now() < deadline {
            collected.extend(service.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        collected
    }

    #[test]
    fn polling_an_idle_service_returns_nothing() {
        let service = build_service();
        assert!(service.poll().is_empty());
    }

    #[test]
    fn answers_a_submitted_request_on_the_worker_thread() {
        let service = build_service();
        let request = RenderRequest::new(PageId::new(1), 7, 1.0).unwrap();
        service.submit(request);

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1);
        assert_eq!(*responses[0].request(), request);
        match &responses[0] {
            RenderResponse::Ready { tile, .. } => assert_eq!((tile.width(), tile.height()), (595, 842)),
            RenderResponse::Failed { reason, .. } => panic!("rendering page one must succeed, got: {reason}"),
        }
        assert!(service.poll().is_empty(), "a response must not be delivered twice");
    }

    #[test]
    fn the_handle_crosses_thread_boundaries() {
        fn require_send<T: Send>(_value: &T) {}
        let service = build_service();
        require_send(&service);

        let request = RenderRequest::new(PageId::new(1), 7, 0.1).unwrap();
        service.submit(request);
        assert_eq!(drain(&service, 1).len(), 1);
    }

    #[test]
    fn reports_a_missing_file_when_opening_rather_than_on_every_request() {
        let result = PdfiumRenderService::open(Path::new("/nonexistent/missing.pdf"), build_snapshot());
        assert!(matches!(result, Err(Error::Render(_))), "opening a missing file must fail loudly");
    }

    #[test]
    fn an_unknown_page_identity_fails_without_panicking() {
        let service = build_service();
        service.submit(RenderRequest::new(PageId::new(u64::MAX), 7, 1.0).unwrap());

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1, "an unknown page must still produce a response");
        match &responses[0] {
            RenderResponse::Failed { reason, .. } => assert!(reason.contains("unknown page"), "got: {reason}"),
            RenderResponse::Ready { .. } => panic!("an unknown page must not yield a tile"),
        }
    }

    #[test]
    fn a_snapshot_position_beyond_the_file_fails_without_panicking() {
        //--- a three-page snapshot over a two-page file: the third position has no pdfium page ---
        let mut snapshot = build_snapshot();
        snapshot.pages.push(PageInfo {
            id: PageId::new(3),
            size: PageSize::A4,
            rotation: Rotation::None,
        });
        let service = PdfiumRenderService::open(&ensure_contract_fixture(), snapshot).unwrap();
        service.submit(RenderRequest::new(PageId::new(3), 7, 1.0).unwrap());

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], RenderResponse::Failed { .. }), "a position beyond the file must fail");
    }

    #[test]
    fn an_absurd_scale_fails_rather_than_allocating() {
        let service = build_service();
        service.submit(RenderRequest::new(PageId::new(1), 7, 1e30).unwrap());

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1, "an oversized request must still be answered");
        match &responses[0] {
            RenderResponse::Failed { reason, .. } => assert!(reason.contains("exceeds"), "the failure must name the limit, got: {reason}"),
            RenderResponse::Ready { tile, .. } => panic!("an absurd scale must fail, not yield a {}x{} tile", tile.width(), tile.height()),
        }
        assert_eq!(service.rasterizations(), 0, "a rejected request must never reach pdfium");
    }

    #[test]
    fn a_foreign_revision_is_rasterized_and_echoed_back_unchanged() {
        let service = build_service();
        let request = RenderRequest::new(PageId::new(1), 4_242, 1.0).unwrap();
        service.submit(request);

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1, "a revision the service does not hold must still be answered");
        assert_eq!(responses[0].request().revision, 4_242, "the requested revision must be echoed back unchanged");
        match &responses[0] {
            RenderResponse::Ready { tile, .. } => assert_eq!((tile.width(), tile.height()), (595, 842)),
            RenderResponse::Failed { reason, .. } => panic!("a foreign revision must not be rejected, got: {reason}"),
        }
    }
}

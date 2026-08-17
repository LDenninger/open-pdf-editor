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
    /// Tiles rasterized at the previous revision are dropped: their requests
    /// can never match again, so keeping them only costs memory.
    ///
    /// Requests still queued when the rebind is applied are **superseded**, and
    /// answered [`RenderResponse::Failed`] with a reason naming the rebind. The
    /// renderer resolves geometry against the snapshot it holds at the moment it
    /// renders, so answering a request queued under the old snapshot would
    /// return the new snapshot's geometry under the old snapshot's revision —
    /// and cache it there. Resubmit anything that matters at the new revision.
    ///
    /// A request submitted *after* this call is unaffected. A request naming a
    /// revision the service does not hold is still rasterized against the
    /// current snapshot and echoed back unchanged; the renderer validates the
    /// ordering of a rebind, not the revision number a caller writes down.
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

    /// A two-page A4 fixture whose pages are told apart by colour: page one
    /// red, page two green.
    ///
    /// The shared contract fixture cannot serve here — both of its pages are
    /// filled the same blue, so rasterizing the wrong one is invisible. That
    /// is precisely the bug this fixture exists to expose.
    fn write_two_colour_fixture() -> PathBuf {
        let contents = ["1 0 0 rg 0 0 595 842 re f", "0 1 0 rg 0 0 595 842 re f"];
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> /Contents 6 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", contents[0].len(), contents[0]),
            format!("<< /Length {} >>\nstream\n{}\nendstream", contents[1].len(), contents[1]),
        ];

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.7\n");
        let mut offsets: Vec<usize> = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n", objects.len() + 1).as_bytes());

        let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-fixtures");
        std::fs::create_dir_all(&target_dir).unwrap();
        let pdf_path = target_dir.join("two-colour-a4.pdf");
        //--- staged and renamed, for the reason given in fixture.rs ---
        let staging_path = target_dir.join(format!("two-colour-a4.pdf.{}.part", std::process::id()));
        std::fs::write(&staging_path, &bytes).unwrap();
        std::fs::rename(&staging_path, &pdf_path).unwrap();
        pdf_path
    }

    /// The red channel at the centre of a tile, as a coarse page identity.
    fn centre_is_red(tile: &opdf_core::Tile) -> bool {
        let centre = tile.pixel(tile.width() / 2, tile.height() / 2).unwrap();
        centre[0] > 200 && centre[1] < 60
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

    /// Pdfium addresses pages by their position in the file it opened; the
    /// snapshot's order changes on every edit. Resolving a request through the
    /// current snapshot's position meant that after a reorder, every page
    /// rasterized some *other* page's content — silently, with the right
    /// dimensions and the right revision.
    #[test]
    fn a_reordered_snapshot_still_rasterizes_each_page_from_its_own_place_in_the_file() {
        let pdf_path = write_two_colour_fixture();
        let red = PageInfo {
            id: PageId::new(1),
            size: PageSize::A4,
            rotation: Rotation::None,
        };
        let green = PageInfo {
            id: PageId::new(2),
            size: PageSize::A4,
            rotation: Rotation::None,
        };

        let service = PdfiumRenderService::open(
            &pdf_path,
            DocumentSnapshot {
                revision: 1,
                pages: vec![red, green],
            },
        )
        .unwrap();

        //--- baseline: before any edit, page one is the red page ---
        service.submit(RenderRequest::new(PageId::new(1), 1, 1.0).unwrap());
        match &drain(&service, 1)[0] {
            RenderResponse::Ready { tile, .. } => assert!(centre_is_red(tile), "page one must be the red page before any edit"),
            RenderResponse::Failed { reason, .. } => panic!("the baseline render must succeed, got: {reason}"),
        }

        //--- the user moves page two above page one; nothing is saved ---
        service.rebind(DocumentSnapshot {
            revision: 2,
            pages: vec![green, red],
        });

        service.submit(RenderRequest::new(PageId::new(1), 2, 1.0).unwrap());
        match &drain(&service, 1)[0] {
            RenderResponse::Ready { tile, .. } => assert!(
                centre_is_red(tile),
                "page one is still the red page after the reorder; rasterizing green means the request was resolved through the snapshot's new position instead of the file's"
            ),
            RenderResponse::Failed { reason, .. } => panic!("rendering a moved page must succeed, got: {reason}"),
        }
    }

    /// A page inserted after the document was opened has nothing to rasterize.
    /// It must say so rather than address whatever page happens to sit at that
    /// position in the file.
    #[test]
    fn a_page_created_after_opening_fails_loudly_rather_than_rendering_another_page() {
        let service = build_service();

        service.rebind(DocumentSnapshot {
            revision: 8,
            pages: vec![
                PageInfo {
                    id: PageId::new(1),
                    size: PageSize::A4,
                    rotation: Rotation::None,
                },
                PageInfo {
                    id: PageId::new(99),
                    size: PageSize::A4,
                    rotation: Rotation::None,
                },
            ],
        });

        service.submit(RenderRequest::new(PageId::new(99), 8, 1.0).unwrap());
        match &drain(&service, 1)[0] {
            RenderResponse::Failed { reason, .. } => assert!(
                reason.contains("not in the file this service opened"),
                "the reason must name the cause, got: {reason}"
            ),
            RenderResponse::Ready { .. } => panic!("a page that is not in the opened file must not rasterize as some other page"),
        }
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

    #[cfg(feature = "contract-tests")]
    #[test]
    fn satisfies_the_render_service_contract() {
        opdf_core::contract::assert_render_service_contract(|snapshot| {
            PdfiumRenderService::open(&ensure_contract_fixture(), snapshot).expect("the contract fixture must open")
        });
    }

    #[test]
    fn polling_never_blocks_on_a_render_in_flight() {
        let service = build_service();
        //--- 595 x 842 at scale 4.0 is 2380 x 3368, about 32 MiB of RGBA: milliseconds of work, not microseconds ---
        service.submit(RenderRequest::new(PageId::new(1), 7, 4.0).unwrap());

        let started = std::time::Instant::now();
        let _first_poll = service.poll();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "poll must return immediately while a render is in flight, took {elapsed:?}"
        );

        assert_eq!(drain(&service, 1).len(), 1, "the in-flight render must still be answered");
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
    fn a_repeated_request_is_answered_from_the_cache() {
        let service = build_service();
        let request = RenderRequest::new(PageId::new(1), 7, 1.0).unwrap();

        service.submit(request);
        assert_eq!(drain(&service, 1).len(), 1);
        assert_eq!(service.rasterizations(), 1);

        service.submit(request);
        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1, "a repeated request must still receive its own response");
        match &responses[0] {
            RenderResponse::Ready { tile, .. } => assert_eq!((tile.width(), tile.height()), (595, 842)),
            RenderResponse::Failed { reason, .. } => panic!("a cached request must succeed, got: {reason}"),
        }
        assert_eq!(service.rasterizations(), 1, "the second answer must come from the cache, not from pdfium");
    }

    #[test]
    fn rebinding_changes_the_geometry_and_drops_the_old_revision() {
        let service = build_service();
        let before = RenderRequest::new(PageId::new(1), 7, 1.0).unwrap();
        service.submit(before);
        assert_eq!(drain(&service, 1).len(), 1);
        assert_eq!(service.rasterizations(), 1);

        //--- the user rotates page one: a new revision, a new snapshot, and a page whose axes have swapped ---
        let mut rotated = build_snapshot();
        rotated.revision = 8;
        rotated.pages[0].rotation = Rotation::Quarter;
        service.rebind(rotated);

        let after = RenderRequest::new(PageId::new(1), 8, 1.0).unwrap();
        service.submit(after);
        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1);
        match &responses[0] {
            RenderResponse::Ready { tile, .. } => assert_eq!((tile.width(), tile.height()), (842, 595), "the rebound rotation must be honoured"),
            RenderResponse::Failed { reason, .. } => panic!("a request after a rebind must succeed, got: {reason}"),
        }
        assert_eq!(
            service.rasterizations(),
            2,
            "a request at a new revision must be rasterized, never served from the old one"
        );

        //--- and the pre-rebind tile is gone rather than lingering in memory ---
        service.submit(before);
        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1);
        assert_eq!(service.rasterizations(), 3, "tiles from a superseded revision must have been pruned");

        //--- the dimensions, not just the counter: this request names revision 7, and a stale 595x842
        //--- tile answering it would be indistinguishable from a correct answer if only counts were checked ---
        match &responses[0] {
            RenderResponse::Ready { tile, .. } => assert_eq!(
                (tile.width(), tile.height()),
                (842, 595),
                "a request submitted after the rebind is answered against the snapshot the service now holds, whatever revision the request names; 595x842 here would mean the pruned revision 7 tile was served"
            ),
            RenderResponse::Failed { reason, .. } => panic!("a request submitted after the rebind must succeed, got: {reason}"),
        }
    }

    /// The end-to-end shape of the backlog/rebind race, driven through the real
    /// worker rather than through [`crate::worker`]'s own unit test.
    ///
    /// A slow render is put in flight first so the request that follows it is
    /// certain to be sitting in the backlog when the rebind is applied. Against
    /// the unfixed worker this returned a 842x595 tile — revision 8's geometry —
    /// for a request that named revision 7, and cached it under revision 7.
    #[test]
    fn a_request_queued_across_a_rebind_is_never_answered_with_the_new_geometry() {
        let service = build_service();

        //--- 595 x 842 at scale 8.0 is 4760 x 6736, about 32 megapixels: long enough to hold the worker ---
        let slow = RenderRequest::new(PageId::new(1), 7, 8.0).unwrap();
        service.submit(slow);
        std::thread::sleep(std::time::Duration::from_millis(150));

        //--- queued while revision 7 is still current, and still queued when the rebind lands ---
        let queued = RenderRequest::new(PageId::new(1), 7, 1.0).unwrap();
        service.submit(queued);
        let mut rotated = build_snapshot();
        rotated.revision = 8;
        rotated.pages[0].rotation = Rotation::Quarter;
        service.rebind(rotated);

        let responses = drain(&service, 2);
        assert_eq!(responses.len(), 2, "both submitted requests must be answered exactly once");
        let answer = responses
            .iter()
            .find(|response| *response.request() == queued)
            .expect("the request queued across the rebind must be answered");
        match answer {
            RenderResponse::Ready { tile, .. } => assert_eq!(
                (tile.width(), tile.height()),
                (595, 842),
                "a revision 7 request must never be answered with revision 8's swapped axes"
            ),
            RenderResponse::Failed { reason, .. } => assert!(reason.contains("rebind"), "the reason must name the cause, got: {reason}"),
        }

        //--- and nothing may have been cached under the superseded revision ---
        let before_resubmission = service.rasterizations();
        service.submit(queued);
        assert_eq!(drain(&service, 1).len(), 1);
        assert!(
            service.rasterizations() > before_resubmission,
            "a resubmitted revision 7 request must be rasterized afresh; serving it from the cache means a tile with revision 8 geometry was stored under revision 7"
        );
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

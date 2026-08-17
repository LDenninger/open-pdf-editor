//! The [`RenderService`] handle the user interface holds.
//!
//! The handle owns no Pdfium state at all: it owns a sender and a receiver.
//! [`RenderService::submit`] pushes onto an unbounded channel and returns;
//! [`RenderService::poll`] drains a receiver with `try_iter` and returns
//! whatever has arrived. Neither ever blocks, which is the contract's hard
//! requirement — a stalled poll drops frames on the UI thread.
//!
//! # What blocks, and what it costs
//!
//! Pdfium is serialized process-wide (see [`crate::library`]), and the lock is
//! held for the whole of a rasterization. Anything that must reach Pdfium on
//! the calling thread therefore queues behind whatever render is in flight, and
//! a legal maximum-sized tile is hundreds of milliseconds of work.
//!
//! | Operation | Uncontended | Behind a 60 megapixel render |
//! |---|---|---|
//! | [`RenderService::submit`], [`RenderService::poll`] | immediate | immediate |
//! | [`PdfiumRenderService::open`] | 0.97 ms | 180.7 ms |
//! | [`PdfiumRenderService::open_deferred`] | immediate | immediate |
//! | dropping a service | immediate | immediate |
//!
//! Dropping used to join the worker, which had to take the lock to close its
//! document: 124.6 ms measured, and 438.9 ms against a full 60 megapixel tile
//! in this crate's own test. It now detaches instead — see [`Drop`].
//!
//! [`PdfiumRenderService::open`] still blocks by design, because reporting a
//! bad file at the call site is worth more than the latency in most callers. A
//! caller that cannot afford it — a UI thread opening a second document while
//! the first renders — uses [`PdfiumRenderService::open_deferred`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use opdf_core::{DocumentSnapshot, Error, RenderRequest, RenderResponse, RenderService, Result};

use crate::worker::{WorkerMessage, run_worker};

/// A Pdfium-backed rasterizer running on its own thread.
///
/// Dropping the service tells its worker to stop and returns immediately; the
/// worker closes the document on its own thread. See [`Drop`] for what that
/// costs and what it buys.
#[derive(Debug)]
pub struct PdfiumRenderService {
    requests: Sender<WorkerMessage>,
    responses: Receiver<RenderResponse>,
    /// Responses produced by the handle itself, when the worker is unreachable.
    outbox: Mutex<Vec<RenderResponse>>,
    rasterizations: Arc<AtomicU64>,
}

impl PdfiumRenderService {
    /// Open `pdf_path` and answer requests for the pages `snapshot` describes.
    ///
    /// The nth `PageId` in `snapshot.pages` is Pdfium page index `n` — see the
    /// crate documentation. Blocks until the worker reports that the document
    /// opened, so a missing or malformed file is an error here rather than a
    /// failure on every subsequent request.
    ///
    /// # Latency
    ///
    /// Opening a document is a Pdfium call, and Pdfium is serialized
    /// process-wide, so this waits for any render already in flight anywhere in
    /// the process: 0.97 ms uncontended, **180.7 ms** measured behind another
    /// service rasterizing a legal 60 megapixel tile, and worse the more
    /// documents are open. That is eleven dropped frames on a 60 Hz UI thread.
    /// Use [`Self::open_deferred`] where that matters.
    pub fn open(pdf_path: &Path, snapshot: DocumentSnapshot) -> Result<Self> {
        let (service, ready_rx) = Self::spawn(pdf_path, snapshot)?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(service),
            //--- dropping `service` here stops the worker, which is answering every request with this same reason ---
            Ok(Err(reason)) => Err(Error::Render(reason)),
            Err(_) => Err(Error::Render("the render worker exited before reporting readiness".to_string())),
        }
    }

    /// Open `pdf_path` without waiting for Pdfium.
    ///
    /// Returns as soon as the worker thread is spawned — it never takes the
    /// Pdfium lock on the calling thread, so it cannot be stalled by a render
    /// in flight. The error case moves with it: if the document cannot be
    /// opened, **every** request submitted to this service is answered
    /// [`RenderResponse::Failed`] carrying the reason the open failed, rather
    /// than the failure being reported here.
    ///
    /// The returned `Result` reports only that a thread could not be spawned.
    ///
    /// Prefer [`Self::open`] wherever the caller can afford to wait: an error
    /// at the call site is easier to act on than one that arrives per tile.
    pub fn open_deferred(pdf_path: &Path, snapshot: DocumentSnapshot) -> Result<Self> {
        let (service, _ready_rx) = Self::spawn(pdf_path, snapshot)?;
        //--- the ready channel is bounded at one, so dropping the receiver never blocks the worker ---
        Ok(service)
    }

    /// Spawn the worker and wire up the channels, without waiting for it.
    fn spawn(pdf_path: &Path, snapshot: DocumentSnapshot) -> Result<(Self, Receiver<std::result::Result<(), String>>)> {
        let (request_tx, request_rx) = unbounded::<WorkerMessage>();
        let (response_tx, response_rx) = unbounded::<RenderResponse>();
        let (ready_tx, ready_rx) = bounded::<std::result::Result<(), String>>(1);

        let rasterizations = Arc::new(AtomicU64::new(0));
        let worker_counter = Arc::clone(&rasterizations);
        let worker_path: PathBuf = pdf_path.to_path_buf();

        std::thread::Builder::new()
            .name("opdf-render".to_string())
            .spawn(move || run_worker(worker_path, snapshot, request_rx, response_tx, ready_tx, worker_counter))
            .map_err(Error::Io)?;

        Ok((
            Self {
                requests: request_tx,
                responses: response_rx,
                outbox: Mutex::new(Vec::new()),
                rasterizations,
            },
            ready_rx,
        ))
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

/// Tell the worker to stop, and return without waiting for it.
///
/// Joining would be tidier, and it is what this did. But the worker has to take
/// the process-wide Pdfium lock to close its document, so joining made dropping
/// a service cost whatever render was in flight anywhere in the process:
/// 124.6 ms measured in the review, 438.9 ms in this crate's own test against a
/// legal 60 megapixel tile. Closing a document on the UI thread is a normal
/// thing to do — the user closes a tab — and it must not drop frames.
///
/// The worker is detached instead. It sees the shutdown, or the closed request
/// channel, closes its document under the lock, and exits; nothing outlives it
/// but its own stack. The tradeoffs, in full:
///
/// - The document stays open for as long as the render in flight takes. A
///   caller that reopens the same path immediately gets a second handle to it,
///   which Pdfium permits.
/// - A worker still closing its document at process exit is terminated with the
///   process. Nothing it holds survives the process, so nothing is corrupted.
/// - Dropping many services queues that many closes behind the lock. Each is a
///   single Pdfium call and the threads do not accumulate.
impl Drop for PdfiumRenderService {
    fn drop(&mut self) {
        let _ = self.requests.send(WorkerMessage::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::ensure_contract_fixture;
    use opdf_core::{DocumentId, PageId, PageInfo, PageSize, Rotation};
    /// The document identity every request and snapshot in this module names.
    ///
    /// Fixed for the whole module so that two requests differ only in the fields
    /// the test varies, and so a rebind describes the *same* document at a new
    /// revision rather than a different one.
    fn test_document() -> DocumentId {
        static DOCUMENT: std::sync::OnceLock<DocumentId> = std::sync::OnceLock::new();
        *DOCUMENT.get_or_init(DocumentId::new_unique)
    }

    fn build_snapshot() -> DocumentSnapshot {
        DocumentSnapshot {
            document: test_document(),
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
                document: test_document(),
                revision: 1,
                pages: vec![red, green],
            },
        )
        .unwrap();

        //--- baseline: before any edit, page one is the red page ---
        service.submit(RenderRequest::new(test_document(), PageId::new(1), 1, 1.0).unwrap());
        match &drain(&service, 1)[0] {
            RenderResponse::Ready { tile, .. } => assert!(centre_is_red(tile), "page one must be the red page before any edit"),
            RenderResponse::Failed { reason, .. } => panic!("the baseline render must succeed, got: {reason}"),
        }

        //--- the user moves page two above page one; nothing is saved ---
        service.rebind(DocumentSnapshot {
            document: test_document(),
            revision: 2,
            pages: vec![green, red],
        });

        service.submit(RenderRequest::new(test_document(), PageId::new(1), 2, 1.0).unwrap());
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
            document: test_document(),
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

        service.submit(RenderRequest::new(test_document(), PageId::new(99), 8, 1.0).unwrap());
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
        let request = RenderRequest::new(test_document(), PageId::new(1), 7, 1.0).unwrap();
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

        let request = RenderRequest::new(test_document(), PageId::new(1), 7, 0.1).unwrap();
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
        service.submit(RenderRequest::new(test_document(), PageId::new(1), 7, 4.0).unwrap());

        let started = std::time::Instant::now();
        let _first_poll = service.poll();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "poll must return immediately while a render is in flight, took {elapsed:?}"
        );

        assert_eq!(drain(&service, 1).len(), 1, "the in-flight render must still be answered");
    }

    /// Put a long render in flight and wait until the worker is certainly
    /// inside it, so that whatever the caller does next has to contend for the
    /// process-wide Pdfium lock.
    ///
    /// 595 x 842 at scale 11.0 is 6545 x 9262 — 60.6 megapixels, just under the
    /// tile ceiling, and the same legal maximum-sized tile the review measured
    /// against. The short sleep only has to get the worker past its channel
    /// bookkeeping and into Pdfium.
    fn occupy_pdfium(service: &PdfiumRenderService) {
        service.submit(RenderRequest::new(test_document(), PageId::new(1), 7, 11.0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(30));
    }

    #[test]
    fn dropping_a_service_does_not_wait_for_a_render_in_flight() {
        let service = build_service();
        occupy_pdfium(&service);

        let started = std::time::Instant::now();
        drop(service);
        let elapsed = started.elapsed();

        //--- measured at 124.6 ms when Drop joined the worker, which had to take the pdfium lock to close its document ---
        assert!(
            elapsed < std::time::Duration::from_millis(25),
            "closing a document must not stall the caller behind another service's render, took {elapsed:?}"
        );
    }

    #[test]
    fn opening_a_document_deferred_does_not_wait_for_a_render_in_flight() {
        let busy = build_service();
        occupy_pdfium(&busy);

        let started = std::time::Instant::now();
        let opened = PdfiumRenderService::open_deferred(&ensure_contract_fixture(), build_snapshot()).unwrap();
        let elapsed = started.elapsed();

        //--- measured at 180.7 ms for the blocking `open` under the same contention, against 0.97 ms uncontended ---
        assert!(
            elapsed < std::time::Duration::from_millis(25),
            "open_deferred must not touch pdfium on the calling thread, took {elapsed:?}"
        );

        //--- and the service it returns is a real one ---
        opened.submit(RenderRequest::new(test_document(), PageId::new(1), 7, 1.0).unwrap());
        match &drain(&opened, 1)[0] {
            RenderResponse::Ready { tile, .. } => assert_eq!((tile.width(), tile.height()), (595, 842)),
            RenderResponse::Failed { reason, .. } => panic!("a deferred open of a good file must still render, got: {reason}"),
        }
    }

    #[test]
    fn a_deferred_open_of_a_missing_file_answers_every_request_with_the_reason() {
        let service = PdfiumRenderService::open_deferred(Path::new("/nonexistent/missing.pdf"), build_snapshot()).unwrap();
        service.submit(RenderRequest::new(test_document(), PageId::new(1), 7, 1.0).unwrap());

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1, "a request against a document that never opened must still be answered");
        match &responses[0] {
            RenderResponse::Failed { reason, .. } => assert!(
                reason.contains("missing.pdf"),
                "the failure must carry the open error, not a generic one, got: {reason}"
            ),
            RenderResponse::Ready { .. } => panic!("a document that never opened must not produce a tile"),
        }
    }

    #[test]
    fn an_unknown_page_identity_fails_without_panicking() {
        let service = build_service();
        service.submit(RenderRequest::new(test_document(), PageId::new(u64::MAX), 7, 1.0).unwrap());

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
        service.submit(RenderRequest::new(test_document(), PageId::new(3), 7, 1.0).unwrap());

        let responses = drain(&service, 1);
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], RenderResponse::Failed { .. }), "a position beyond the file must fail");
    }

    #[test]
    fn an_absurd_scale_fails_rather_than_allocating() {
        let service = build_service();
        service.submit(RenderRequest::new(test_document(), PageId::new(1), 7, 1e30).unwrap());

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
        let request = RenderRequest::new(test_document(), PageId::new(1), 7, 1.0).unwrap();

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
        let before = RenderRequest::new(test_document(), PageId::new(1), 7, 1.0).unwrap();
        service.submit(before);
        assert_eq!(drain(&service, 1).len(), 1);
        assert_eq!(service.rasterizations(), 1);

        //--- the user rotates page one: a new revision, a new snapshot, and a page whose axes have swapped ---
        let mut rotated = build_snapshot();
        rotated.revision = 8;
        rotated.pages[0].rotation = Rotation::Quarter;
        service.rebind(rotated);

        let after = RenderRequest::new(test_document(), PageId::new(1), 8, 1.0).unwrap();
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
        let slow = RenderRequest::new(test_document(), PageId::new(1), 7, 8.0).unwrap();
        service.submit(slow);
        std::thread::sleep(std::time::Duration::from_millis(150));

        //--- queued while revision 7 is still current, and still queued when the rebind lands ---
        let queued = RenderRequest::new(test_document(), PageId::new(1), 7, 1.0).unwrap();
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
        let request = RenderRequest::new(test_document(), PageId::new(1), 4_242, 1.0).unwrap();
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

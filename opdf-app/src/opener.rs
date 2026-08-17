//! Turning a path into a document and a service that can rasterize it.
//!
//! This is the shell's only knowledge of where documents come from. The real
//! implementation is [`PdfiumDocumentOpener`]; the fake one here lets every
//! headless test run the open path without PDFium on the machine.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use opdf_core::document::{Document, DocumentIo, DocumentSnapshot};
use opdf_core::fakes::{FakeRenderService, VecDocument};
use opdf_core::page::{PageId, PageSize};
use opdf_core::render::{RenderRequest, RenderResponse, RenderService};
use opdf_core::{Error, Result};

/// A document the shell can save.
///
/// [`opdf_core::DocumentIo`] is `Sized`, so its save methods cannot be reached
/// through the `Box<dyn Document>` the shell owns. This trait is the object-safe
/// subset the shell actually needs: no `open`, because opening produces the
/// document rather than acting on one, and that is [`DocumentOpener`]'s job.
pub trait EditableDocument: Document {
    /// Write changes as an incremental update appended to the original bytes.
    ///
    /// The shell's default save path: it appends rather than rewrites, so every
    /// structure the implementation does not model survives, and it does not
    /// purge the trash, so undo of a deletion survives it too.
    fn save_incremental(&mut self, path: &Path) -> Result<()>;

    /// Write a freshly serialized document, discarding unreferenced objects.
    ///
    /// Destructive to undo of a deletion — a trashed page is an unreferenced
    /// object — so the shell only ever reaches this on an explicit, confirmed
    /// request, and clears its undo stack afterwards.
    fn save_compacted(&mut self, path: &Path) -> Result<()>;
}

impl EditableDocument for opdf_pdf::PdfDocument {
    fn save_incremental(&mut self, path: &Path) -> Result<()> {
        DocumentIo::save_incremental(self, path)
    }

    fn save_compacted(&mut self, path: &Path) -> Result<()> {
        DocumentIo::save_compacted(self, path)
    }
}

/// Saving a [`VecDocument`] writes a marker file recording what was asked for.
///
/// [`VecDocument`] has no file format, and inventing a PDF writer for it would
/// be testing a fake against a fake — serializing PDF is `opdf-pdf`'s job. The
/// marker is enough to prove the call reached the object through the trait, and
/// which of the two save paths it took.
impl EditableDocument for VecDocument {
    fn save_incremental(&mut self, path: &Path) -> Result<()> {
        std::fs::write(path, format!("incremental {} pages\n", self.page_count()))?;
        Ok(())
    }

    fn save_compacted(&mut self, path: &Path) -> Result<()> {
        std::fs::write(path, format!("compacted {} pages\n", self.page_count()))?;
        Ok(())
    }
}

/// A document the shell has opened, together with the service that renders it.
///
/// The snapshot is taken at open time and handed over with the document so the
/// caller never has to remember to derive one — a service built from a different
/// snapshot than the shell is drawing is exactly the class of defect F7 and F14
/// describe.
pub struct OpenedDocument {
    /// The document itself, kept so later edits and saves have something to act on.
    pub document: Box<dyn EditableDocument>,
    /// The service that rasterizes this document, and only this document.
    pub service: Box<dyn RenderService>,
    /// The snapshot both of the above were built from.
    pub snapshot: DocumentSnapshot,
    /// Where the document was opened from, if it came from a file.
    ///
    /// Carried with the document rather than threaded separately, for the same
    /// reason the snapshot is: Save writes to this path, and a path that arrived
    /// by a different route than the document could point at a different file
    /// than the one the shell believes is open. A synthetic document has no
    /// origin, so Save has to ask.
    pub path: Option<PathBuf>,
}

/// Opening a document from disk.
pub trait DocumentOpener {
    /// Open the document at `path`.
    ///
    /// Returns whatever error the underlying implementation produces; the shell
    /// surfaces it to the user rather than interpreting it.
    fn open(&self, path: &Path) -> Result<OpenedDocument>;
}

//---------------------------------------------------------------------
// Asking the user which file to open
//---------------------------------------------------------------------

/// Asking the user to name a document.
///
/// Separated from [`DocumentOpener`] because a native file dialog cannot be
/// driven headlessly, and the interesting part of File ▸ Open is not the dialog
/// but what the shell does with its answer. With the dialog behind a trait, that
/// part is testable and only the dialog itself is judged by eye.
pub trait PathChooser {
    /// Show a picker for PDF files, returning the chosen path, or `None` if the
    /// user cancelled.
    fn choose_pdf(&self) -> Option<PathBuf>;

    /// Show a save dialog, returning the path to write to, or `None` if the user
    /// cancelled.
    ///
    /// Separate from [`PathChooser::choose_pdf`] because the two dialogs differ
    /// in more than their title: a save dialog accepts a name that does not
    /// exist yet, and confirms overwriting one that does.
    fn choose_save_path(&self) -> Option<PathBuf>;
}

/// The production chooser: the platform's own file dialog.
pub struct NativePathChooser;

impl PathChooser for NativePathChooser {
    fn choose_pdf(&self) -> Option<PathBuf> {
        rfd::FileDialog::new().add_filter("PDF document", &["pdf"]).pick_file()
    }

    fn choose_save_path(&self) -> Option<PathBuf> {
        rfd::FileDialog::new().add_filter("PDF document", &["pdf"]).save_file()
    }
}

/// A chooser that answers with a fixed decision, for tests.
pub struct FakeChooser {
    path: Option<PathBuf>,
}

impl FakeChooser {
    /// A chooser that always returns `path`.
    pub fn choosing(path: impl Into<PathBuf>) -> Self {
        Self { path: Some(path.into()) }
    }

    /// A chooser the user always cancels.
    pub fn cancelling() -> Self {
        Self { path: None }
    }
}

impl PathChooser for FakeChooser {
    fn choose_pdf(&self) -> Option<PathBuf> {
        self.path.clone()
    }

    fn choose_save_path(&self) -> Option<PathBuf> {
        self.path.clone()
    }
}

//---------------------------------------------------------------------
// The production opener
//---------------------------------------------------------------------

/// The production opener: a real parser and a real rasterizer.
pub struct PdfiumDocumentOpener;

impl DocumentOpener for PdfiumDocumentOpener {
    fn open(&self, path: &Path) -> Result<OpenedDocument> {
        let document = opdf_pdf::PdfDocument::open(path)?;
        let snapshot = DocumentSnapshot::of(&document)?;
        //--- the service is built from the same snapshot the shell will draw:
        //--- the rasterizer maps the nth PageId in the snapshot to PDFium page n,
        //--- so a service opened from any other snapshot resolves the wrong page ---
        let service = opdf_render::PdfiumRenderService::open(path, snapshot.clone())?;
        Ok(OpenedDocument {
            document: Box::new(document),
            service: Box::new(service),
            snapshot,
            path: Some(path.to_owned()),
        })
    }
}

//---------------------------------------------------------------------
// The in-memory opener
//---------------------------------------------------------------------

/// An opener that ignores the path and produces an in-memory document.
///
/// Used by every test that needs the open path without needing PDFium.
pub struct FakeOpener {
    page_count: Option<usize>,
    unrenderable_index: Option<usize>,
}

impl FakeOpener {
    /// An opener that always succeeds, producing a document of `page_count`
    /// A4 pages, every one of which rasterizes.
    pub fn with_pages(page_count: usize) -> Self {
        Self {
            page_count: Some(page_count),
            unrenderable_index: None,
        }
    }

    /// An opener that always fails, for testing the error path.
    pub fn failing() -> Self {
        Self {
            page_count: None,
            unrenderable_index: None,
        }
    }

    /// An opener whose service refuses the page at `unrenderable_index`.
    ///
    /// This is not a contrived case: after the F5 fix the rasterizer resolves a
    /// page through the index map frozen when the file was opened, so a page
    /// inserted since then has no position in the file and is refused by design.
    /// The shell has to survive a page that will never rasterize, however long it
    /// waits.
    pub fn with_unrenderable_page(page_count: usize, unrenderable_index: usize) -> Self {
        Self {
            page_count: Some(page_count),
            unrenderable_index: Some(unrenderable_index),
        }
    }
}

impl DocumentOpener for FakeOpener {
    fn open(&self, path: &Path) -> Result<OpenedDocument> {
        let Some(page_count) = self.page_count else {
            return Err(Error::Unsupported("this opener is configured to fail".to_owned()));
        };
        let document = VecDocument::with_pages(page_count, PageSize::A4);
        let snapshot = DocumentSnapshot::of(&document)?;
        let refused = self.unrenderable_index.and_then(|index| snapshot.pages.get(index)).map(|page| page.id);
        let service: Box<dyn RenderService> = match refused {
            Some(page) => Box::new(RefusingRenderService::new(snapshot.clone(), page)),
            None => Box::new(FakeRenderService::new(snapshot.clone())),
        };
        Ok(OpenedDocument {
            document: Box::new(document),
            service,
            snapshot,
            //--- the fake ignores the path when reading, but the shell saves back
            //--- to it, so a test can drive the whole save path through the fake ---
            path: Some(path.to_owned()),
        })
    }
}

/// A service that answers one page with a failure and delegates the rest.
///
/// The refusal is permanent and instant, which is the shape that matters: a page
/// the rasterizer cannot resolve does not become resolvable by asking again.
struct RefusingRenderService {
    inner: FakeRenderService,
    unrenderable: PageId,
    refused: Mutex<Vec<RenderRequest>>,
}

impl RefusingRenderService {
    fn new(snapshot: DocumentSnapshot, unrenderable: PageId) -> Self {
        Self {
            inner: FakeRenderService::new(snapshot),
            unrenderable,
            refused: Mutex::new(Vec::new()),
        }
    }
}

impl RenderService for RefusingRenderService {
    fn submit(&self, request: RenderRequest) {
        if request.page != self.unrenderable {
            self.inner.submit(request);
            return;
        }
        //--- the contract promises exactly one response per request, refusal included ---
        if let Ok(mut refused) = self.refused.lock() {
            refused.push(request);
        }
    }

    fn poll(&self) -> Vec<RenderResponse> {
        let mut responses: Vec<RenderResponse> = match self.refused.lock() {
            Ok(mut refused) => refused
                .drain(..)
                .map(|request| RenderResponse::Failed {
                    request,
                    reason: format!("{} has no position in the file this service was opened from", request.page),
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        responses.extend(self.inner.poll());
        responses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_opener_produces_a_document_and_a_matching_snapshot() {
        let opener = FakeOpener::with_pages(3);
        let opened = opener.open(Path::new("anything.pdf")).unwrap();
        assert_eq!(opened.document.page_count(), 3);
        assert_eq!(opened.snapshot.page_count(), 3);
        assert_eq!(opened.snapshot.revision, opened.document.revision());
    }

    #[test]
    fn the_fake_opener_reports_a_configured_failure() {
        let opener = FakeOpener::failing();
        assert!(opener.open(Path::new("broken.pdf")).is_err());
    }

    #[test]
    fn an_opened_document_can_be_saved_through_the_trait_object() {
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("out.pdf");

        let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
        let mut document = opened.document;
        document.save_incremental(&out).unwrap();

        assert!(out.exists(), "saving through the trait object must write a file");
    }

    #[test]
    fn the_real_opener_opens_a_corpus_file_and_reports_its_page_count() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/corpus/files/irs_f1040.pdf");
        let opened = PdfiumDocumentOpener.open(&path).unwrap();
        assert!(opened.document.page_count() > 0);
        assert_eq!(opened.snapshot.page_count(), opened.document.page_count());
    }
}

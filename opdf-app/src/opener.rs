//! Turning a path into a document and a service that can rasterize it.
//!
//! This is the shell's only knowledge of where documents come from. The real
//! implementation is [`PdfiumDocumentOpener`]; the fake one here lets every
//! headless test run the open path without PDFium on the machine.

use std::path::Path;
use std::sync::Mutex;

use opdf_core::document::{Document, DocumentIo, DocumentSnapshot};
use opdf_core::fakes::{FakeRenderService, VecDocument};
use opdf_core::page::{PageId, PageSize};
use opdf_core::render::{RenderRequest, RenderResponse, RenderService};
use opdf_core::{Error, Result};

/// A document the shell has opened, together with the service that renders it.
///
/// The snapshot is taken at open time and handed over with the document so the
/// caller never has to remember to derive one — a service built from a different
/// snapshot than the shell is drawing is exactly the class of defect F7 and F14
/// describe.
pub struct OpenedDocument {
    /// The document itself, kept so later edits and saves have something to act on.
    pub document: Box<dyn Document>,
    /// The service that rasterizes this document, and only this document.
    pub service: Box<dyn RenderService>,
    /// The snapshot both of the above were built from.
    pub snapshot: DocumentSnapshot,
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
    fn open(&self, _path: &Path) -> Result<OpenedDocument> {
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
    fn the_real_opener_opens_a_corpus_file_and_reports_its_page_count() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/corpus/files/irs_f1040.pdf");
        let opened = PdfiumDocumentOpener.open(&path).unwrap();
        assert!(opened.document.page_count() > 0);
        assert_eq!(opened.snapshot.page_count(), opened.document.page_count());
    }
}

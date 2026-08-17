//! Turning a path into a document and a service that can rasterize it.
//!
//! This is the shell's only knowledge of where documents come from. The real
//! implementation is [`PdfiumDocumentOpener`]; the fake one here lets every
//! headless test run the open path without PDFium on the machine.

use std::path::Path;

use opdf_core::document::{Document, DocumentIo, DocumentSnapshot};
use opdf_core::fakes::{FakeRenderService, VecDocument};
use opdf_core::page::PageSize;
use opdf_core::render::RenderService;
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
}

impl FakeOpener {
    /// An opener that always succeeds, producing a document of `page_count`
    /// A4 pages.
    pub fn with_pages(page_count: usize) -> Self {
        Self { page_count: Some(page_count) }
    }

    /// An opener that always fails, for testing the error path.
    pub fn failing() -> Self {
        Self { page_count: None }
    }
}

impl DocumentOpener for FakeOpener {
    fn open(&self, _path: &Path) -> Result<OpenedDocument> {
        let Some(page_count) = self.page_count else {
            return Err(Error::Unsupported("this opener is configured to fail".to_owned()));
        };
        let document = VecDocument::with_pages(page_count, PageSize::A4);
        let snapshot = DocumentSnapshot::of(&document)?;
        let service = Box::new(FakeRenderService::new(snapshot.clone()));
        Ok(OpenedDocument {
            document: Box::new(document),
            service,
            snapshot,
        })
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

//! The document contract: what every PDF document implementation must provide.

use std::path::Path;

use crate::Result;
use crate::page::{PageId, PageInfo, Rotation};

/// A paginated document that can be inspected and structurally modified.
///
/// Implementations address pages by [`PageId`], which survives reordering.
/// Indices appear only as insertion positions, and are always interpreted
/// against the document's state at the moment of the call.
pub trait Document {
    /// A counter that advances whenever this document's structure changes.
    ///
    /// Tile caches key on this so that an image rendered before a change is never
    /// mistaken for a current one. Every mutating method must advance it on success
    /// and leave it untouched on failure. Read-only methods never advance it.
    /// Values are opaque: only equality is meaningful, never ordering or arithmetic.
    fn revision(&self) -> u64;

    /// Number of pages currently in the document.
    fn page_count(&self) -> usize;

    /// Page identities in document order.
    fn page_ids(&self) -> Vec<PageId>;

    /// Metadata for one page.
    ///
    /// Returns [`crate::Error::PageNotFound`] if the page is absent.
    fn page(&self, id: PageId) -> Result<PageInfo>;

    /// Current position of a page in document order, counting from zero.
    ///
    /// Returns [`crate::Error::PageNotFound`] if the page is absent.
    fn index_of(&self, id: PageId) -> Result<usize>;

    /// Remove a page.
    ///
    /// Returns [`crate::Error::PageNotFound`] if the page is absent.
    fn remove_page(&mut self, id: PageId) -> Result<()>;

    /// Move a page to a new position.
    ///
    /// Removes the page from its current location, then inserts it at index
    /// `to_index` (0-indexed). After removal, `page_count() - 1` pages remain,
    /// so `to_index` is valid if `to_index <= page_count() - 1` (where
    /// `page_count()` is evaluated before the move).
    ///
    /// Returns [`crate::Error::PageNotFound`] if the page is absent, and
    /// [`crate::Error::IndexOutOfBounds`] if `to_index` exceeds the valid range.
    fn move_page(&mut self, id: PageId, to_index: usize) -> Result<()>;

    /// Replace a page's rotation.
    ///
    /// Returns [`crate::Error::PageNotFound`] if the page is absent.
    fn set_rotation(&mut self, id: PageId, rotation: Rotation) -> Result<()>;

    /// Insert a blank page of the given size at a position, returning its new identity.
    ///
    /// Returns [`crate::Error::IndexOutOfBounds`] if the position exceeds the page count.
    fn insert_page(&mut self, at_index: usize, size: crate::page::PageSize) -> Result<PageId>;

    /// Copy pages from another document of the same implementation, in the order
    /// given, inserting them at a position and returning their new identities.
    ///
    /// Returns [`crate::Error::PageNotFound`] if any source page is absent, and
    /// [`crate::Error::IndexOutOfBounds`] if the position exceeds the page count.
    fn import_pages(&mut self, source: &Self, ids: &[PageId], at_index: usize) -> Result<Vec<PageId>>
    where
        Self: Sized;
}

/// Reading a document from disk and writing it back.
///
/// Kept separate from [`Document`] so that in-memory fakes can satisfy the
/// structural contract without inventing a file format.
pub trait DocumentIo: Document + Sized {
    /// Read a document from disk.
    fn open(path: &Path) -> Result<Self>;

    /// Write changes as an incremental update appended to the original bytes.
    ///
    /// This is the default save path: it is fast on large files and preserves
    /// structure the implementation does not understand.
    fn save_incremental(&mut self, path: &Path) -> Result<()>;

    /// Write a freshly serialized document, discarding unreferenced objects.
    ///
    /// Slower than [`DocumentIo::save_incremental`] and lossy with respect to
    /// structure the implementation does not model, so it is only ever invoked
    /// on explicit user request.
    fn save_compacted(&mut self, path: &Path) -> Result<()>;
}

/// An immutable copy of a document's page list.
///
/// The UI holds a snapshot rather than the document itself, because the render
/// worker owns the document and cannot share it across threads.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DocumentSnapshot {
    /// Page metadata in document order.
    pub pages: Vec<PageInfo>,
    /// The value [`Document::revision`] held when this snapshot was captured.
    ///
    /// A caller builds [`crate::render::RenderRequest`]s against this value, so
    /// that tiles rendered from an older structure are never reused after a
    /// mutation. Opaque: compare it for equality only.
    pub revision: u64,
}

impl DocumentSnapshot {
    /// Capture the current page list of a document, together with its revision.
    pub fn of<D: Document + ?Sized>(document: &D) -> Result<Self> {
        let mut pages = Vec::with_capacity(document.page_count());
        for id in document.page_ids() {
            pages.push(document.page(id)?);
        }
        Ok(Self {
            pages,
            revision: document.revision(),
        })
    }

    /// Number of pages captured.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

//! The document contract: what every PDF document implementation must provide.

use std::any::Any;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Result;
use crate::error::Error;
use crate::page::{PageId, PageInfo, Rotation};

/// The identity of one open document, unique within this process.
///
/// Minted at construction and never reused, including after a document is
/// dropped. Unlike [`Document::revision`], which every implementation starts at
/// zero, an identity distinguishes two documents of the same type — which is
/// what a tile cache, a cross-document command, and a render worker each need
/// in order to refuse work built for a different document.
///
/// It is meaningful within one process only, exactly like [`PageId`]: nothing
/// in the PDF format can carry it, and persisting one would silently address
/// the wrong document after a reopen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(u64);

impl DocumentId {
    /// Mint an identity no other live or past document in this process holds.
    pub fn new_unique() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Unwrap the raw identifier, for in-memory interchange with code that
    /// cannot name the `DocumentId` type — a widget id, a bare `u64` key.
    ///
    /// The returned value must never be written to disk or carried across a
    /// save-and-reopen: an identity is unique within one process and means
    /// nothing outside it. See `docs/architecture/contracts.md`.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for DocumentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "document#{}", self.0)
    }
}

/// Pages taken out of one document and not yet put into another.
///
/// An **opaque owned carrier**. What is inside it — serialized bytes, an
/// implementation-specific structure — is deliberately not part of the contract,
/// and there is no way to inspect it: the payload is erased behind [`Any`] and
/// only the implementation that wrote it can name the type that gets it back.
///
/// That erasure is the whole design. [`Document::export_pages`] and
/// [`Document::import_portable`] are object-safe, so a caller holding two
/// `&dyn Document` can move pages between them without knowing either concrete
/// type — which [`Document::import_pages`], with its `&Self` source, cannot
/// express. The price is that the target may be a *different* implementation
/// from the source, and must then refuse: [`PortablePages::take`] answers
/// [`Error::Unsupported`] rather than letting a payload be reinterpreted.
///
/// `import_pages` remains the fast path, unchanged. This is the general one, and
/// it costs a copy.
pub struct PortablePages {
    /// The payload's type name, for the refusal message only.
    payload_type: &'static str,
    payload: Box<dyn Any + Send>,
}

impl PortablePages {
    /// Wrap an implementation's own payload.
    ///
    /// Called by [`Document::export_pages`]. `T` should be a type private to the
    /// implementation: privacy is what stops another implementation naming it,
    /// and naming it is the only way to get the payload back.
    pub fn new<T: Any + Send>(payload: T) -> Self {
        Self {
            payload_type: std::any::type_name::<T>(),
            payload: Box::new(payload),
        }
    }

    /// Take the payload back, if this carrier is one `T` was put into.
    ///
    /// Called by [`Document::import_portable`]. Returns [`Error::Unsupported`]
    /// when the carrier came from a different implementation — which is the
    /// contract's answer for a foreign carrier, and the reason no implementation
    /// has to invent one.
    pub fn take<T: Any + Send>(self) -> Result<T> {
        let expected = std::any::type_name::<T>();
        let actual = self.payload_type;
        self.payload.downcast::<T>().map(|payload| *payload).map_err(|_| {
            Error::Unsupported(format!(
                "these pages were exported by a different document implementation ({actual}) and cannot be imported here ({expected}); \
                 the carrier's contents are opaque, so there is nothing to fall back on"
            ))
        })
    }
}

impl std::fmt::Debug for PortablePages {
    /// Names the payload's type and nothing else: the contents are opaque by
    /// design, and a `Debug` that printed them would be an inspection route.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PortablePages").field("payload_type", &self.payload_type).finish()
    }
}

/// A paginated document that can be inspected and structurally modified.
///
/// Implementations address pages by [`PageId`], which survives reordering.
/// Indices appear only as insertion positions, and are always interpreted
/// against the document's state at the moment of the call.
pub trait Document {
    /// Which document this is, distinct from every other document in this process.
    ///
    /// Minted once, at construction, and unchanged for the life of the value —
    /// through every mutation, and through a compacting save that rewrites the
    /// backing bytes, because that is the same open document rewritten. A new
    /// identity means a *different* document, which is exactly what a tile cache
    /// and a cross-document command need to be told.
    ///
    /// Deliberately without a default body: a default would let an
    /// implementation silently share one identity across every instance, and
    /// nothing else in the contract would notice.
    fn id(&self) -> DocumentId;

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

    /// Restore a page previously removed from this document, with its original
    /// identity, geometry, and content, at `at_index`.
    ///
    /// A removed page is retained by the document, unreferenced, until an explicit
    /// compaction purges it. This mirrors how PDF incremental save already works:
    /// objects are never deleted, only unreferenced. It is what makes undo of a
    /// deletion exact rather than approximate.
    ///
    /// Returns [`crate::Error::PageNotFound`] if `id` was never a page of this
    /// document, or has been purged. Returns [`crate::Error::IndexOutOfBounds`] if
    /// `at_index` exceeds the page count. Returns [`crate::Error::Unsupported`] if
    /// `id` is currently present — restoring a live page is a caller error, not a
    /// no-op.
    ///
    /// Advances the revision on success.
    fn restore_page(&mut self, id: PageId, at_index: usize) -> Result<()>;

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

    /// Take copies of the given pages out of this document, in the order given,
    /// as an opaque carrier another document can import.
    ///
    /// The object-safe counterpart to [`Document::import_pages`], whose `&Self`
    /// source keeps it out of the vtable and so out of reach of a caller holding
    /// two `&dyn Document` — a cross-document drag in the user interface, for
    /// instance. `import_pages` stays the fast path where both concrete types are
    /// known; this pair costs a copy and works regardless.
    ///
    /// A read: the document is not modified and the revision does not advance.
    ///
    /// Returns [`crate::Error::PageNotFound`] if any page is absent, in which
    /// case no carrier is produced.
    fn export_pages(&self, ids: &[PageId]) -> Result<PortablePages>;

    /// Insert pages carried by [`Document::export_pages`] at a position,
    /// returning their new identities in this document.
    ///
    /// Consumes the carrier, because the pages it holds are moved into this
    /// document rather than borrowed from it.
    ///
    /// Returns [`crate::Error::IndexOutOfBounds`] if the position exceeds the
    /// page count, and [`crate::Error::Unsupported`] if the carrier was produced
    /// by a *different* implementation — its contents are opaque, so there is
    /// nothing to fall back on and guessing would corrupt. [`PortablePages::take`]
    /// produces that error, so no implementation has to invent it.
    ///
    /// Advances the revision on success.
    fn import_portable(&mut self, pages: PortablePages, at_index: usize) -> Result<Vec<PageId>>;
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
#[derive(Clone, PartialEq, Debug)]
pub struct DocumentSnapshot {
    /// Which document this is a snapshot *of*.
    ///
    /// Carried so that a caller which never touches the document — the shell, the
    /// scheduler, a tile cache — can still tell one document's work from
    /// another's. Every [`crate::render::RenderRequest`] is built from this
    /// value, exactly as `revision` is, so neither can drift from the structure
    /// it describes.
    pub document: DocumentId,
    /// Page metadata in document order.
    pub pages: Vec<PageInfo>,
    /// The value [`Document::revision`] held when this snapshot was captured.
    ///
    /// A caller builds [`crate::render::RenderRequest`]s against this value, so
    /// that tiles rendered from an older structure are never reused after a
    /// mutation. Opaque: compare it for equality only.
    pub revision: u64,
}

/// An empty snapshot of a document that does not exist — what a shell shows
/// before anything is open.
///
/// Hand-written rather than derived, because a derived `Default` would need a
/// `Default` for [`DocumentId`], and a default identity is one every empty
/// snapshot would share. Minting instead means two "no document open" states are
/// two distinct documents, which is the safe direction: their cache keys cannot
/// collide.
impl Default for DocumentSnapshot {
    fn default() -> Self {
        Self {
            document: DocumentId::new_unique(),
            pages: Vec::new(),
            revision: 0,
        }
    }
}

impl DocumentSnapshot {
    /// Capture the current page list of a document, together with its identity
    /// and revision.
    pub fn of<D: Document + ?Sized>(document: &D) -> Result<Self> {
        let mut pages = Vec::with_capacity(document.page_count());
        for id in document.page_ids() {
            pages.push(document.page(id)?);
        }
        Ok(Self {
            document: document.id(),
            pages,
            revision: document.revision(),
        })
    }

    /// Number of pages captured.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Document`] must remain usable as `dyn Document`.
    ///
    /// This is a compile-time assertion wearing a test's clothes: adding an
    /// associated type, a generic method, or a `Self`-typed argument without a
    /// `where Self: Sized` escape hatch makes the trait object-unsafe, and this
    /// line stops compiling. That matters because a UI holding one of several
    /// document implementations behind a `&dyn Document` cannot be written at all
    /// once object safety is lost — and the compiler error would otherwise surface
    /// in a dependent track rather than here.
    #[test]
    fn stays_object_safe() {
        let _: Option<&dyn Document> = None;
    }
}

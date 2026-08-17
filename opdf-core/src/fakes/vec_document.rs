//! In-memory [`Document`] implementation used to develop and test dependent
//! crates before a real PDF parser exists.

use crate::Result;
use crate::document::{Document, DocumentId, PortablePages};
use crate::error::Error;
use crate::page::{PageId, PageIdAllocator, PageInfo, PageSize, Rotation};

/// What a [`VecDocument`] puts inside a [`PortablePages`].
///
/// Private on purpose: privacy is what makes the carrier implementation-specific.
/// No other crate can name this type, so no other implementation can take a
/// `VecDocument`'s payload back out — [`PortablePages::take`] refuses instead.
#[derive(Debug)]
struct VecPortablePayload {
    pages: Vec<PageInfo>,
}

/// A document that stores page metadata in a vector and no content at all.
///
/// Removed pages are moved to `removed` rather than dropped, so that
/// [`Document::restore_page`] can hand back the original page — same identity,
/// same geometry, same rotation — instead of an approximation of it.
#[derive(Debug)]
pub struct VecDocument {
    id: DocumentId,
    pages: Vec<PageInfo>,
    removed: Vec<PageInfo>,
    allocator: PageIdAllocator,
    revision: u64,
}

/// Deliberately hand-written rather than derived: a derived `Default` would give
/// every `VecDocument` the same [`DocumentId`], which is precisely the defect
/// [`Document::id`] exists to prevent, and every construction path in this file
/// routes through here.
impl Default for VecDocument {
    fn default() -> Self {
        Self {
            id: DocumentId::new_unique(),
            pages: Vec::new(),
            removed: Vec::new(),
            allocator: PageIdAllocator::default(),
            revision: 0,
        }
    }
}

impl VecDocument {
    /// An empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// A document pre-filled with `count` unrotated pages of one size.
    pub fn with_pages(count: usize, size: PageSize) -> Self {
        let mut document = Self::new();
        for _ in 0..count {
            let id = document.allocator.allocate();
            document.pages.push(PageInfo {
                id,
                size,
                rotation: Rotation::None,
            });
        }
        document
    }

    fn find_index(&self, id: PageId) -> Result<usize> {
        self.pages.iter().position(|page| page.id == id).ok_or(Error::PageNotFound(id))
    }

    /// Advance the revision counter, called once by each mutation that succeeds.
    ///
    /// Wrapping is deliberate: the counter is opaque, only ever compared for
    /// equality, so an overflow panic would be a worse outcome than the
    /// unreachable collision it guards against.
    fn advance_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn check_insertion_index(&self, index: usize) -> Result<()> {
        if index > self.pages.len() {
            return Err(Error::IndexOutOfBounds {
                index,
                page_count: self.pages.len(),
            });
        }
        Ok(())
    }
}

impl Document for VecDocument {
    fn id(&self) -> DocumentId {
        self.id
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_ids(&self) -> Vec<PageId> {
        self.pages.iter().map(|page| page.id).collect()
    }

    fn page(&self, id: PageId) -> Result<PageInfo> {
        let index = self.find_index(id)?;
        Ok(self.pages[index])
    }

    fn index_of(&self, id: PageId) -> Result<usize> {
        self.find_index(id)
    }

    fn remove_page(&mut self, id: PageId) -> Result<()> {
        let index = self.find_index(id)?;
        //--- the page is retained rather than dropped, so that restore_page can return the original ---
        let page = self.pages.remove(index);
        self.removed.push(page);
        self.advance_revision();
        Ok(())
    }

    fn restore_page(&mut self, id: PageId, at_index: usize) -> Result<()> {
        if self.pages.iter().any(|page| page.id == id) {
            return Err(Error::Unsupported(format!("{id} is currently present and cannot be restored")));
        }
        //--- resolve the identity before the index, matching move_page's precedence ---
        let trash_index = self.removed.iter().position(|page| page.id == id).ok_or(Error::PageNotFound(id))?;
        self.check_insertion_index(at_index)?;

        //--- both checks have passed, so nothing below can fail and leave the trash half-emptied ---
        let page = self.removed.remove(trash_index);
        self.pages.insert(at_index, page);
        self.advance_revision();
        Ok(())
    }

    fn move_page(&mut self, id: PageId, to_index: usize) -> Result<()> {
        let from_index = self.find_index(id)?;
        //--- capture the count before removing: the error reports the pages present when the move was attempted ---
        let page_count = self.pages.len();
        //--- deliberately not remove_page: a move lifts the page out and puts it straight back, so it must never reach the trash ---
        let page = self.pages.remove(from_index);
        if to_index > self.pages.len() {
            //--- the page goes back where it came from, so this rejection is not a change: the revision must not advance ---
            self.pages.insert(from_index, page);
            return Err(Error::IndexOutOfBounds { index: to_index, page_count });
        }
        self.pages.insert(to_index, page);
        self.advance_revision();
        Ok(())
    }

    fn set_rotation(&mut self, id: PageId, rotation: Rotation) -> Result<()> {
        let index = self.find_index(id)?;
        self.pages[index].rotation = rotation;
        self.advance_revision();
        Ok(())
    }

    fn insert_page(&mut self, at_index: usize, size: PageSize) -> Result<PageId> {
        self.check_insertion_index(at_index)?;
        let id = self.allocator.allocate();
        self.pages.insert(
            at_index,
            PageInfo {
                id,
                size,
                rotation: Rotation::None,
            },
        );
        self.advance_revision();
        Ok(id)
    }

    fn import_pages(&mut self, source: &Self, ids: &[PageId], at_index: usize) -> Result<Vec<PageId>> {
        self.check_insertion_index(at_index)?;

        //--- resolve every source page before mutating, so failure leaves no partial import ---
        let mut imported = Vec::with_capacity(ids.len());
        for id in ids {
            imported.push(source.page(*id)?);
        }

        let mut new_ids = Vec::with_capacity(imported.len());
        for (offset, page) in imported.into_iter().enumerate() {
            let id = self.allocator.allocate();
            self.pages.insert(at_index + offset, PageInfo { id, ..page });
            new_ids.push(id);
        }
        self.advance_revision();
        Ok(new_ids)
    }

    fn export_pages(&self, ids: &[PageId]) -> Result<PortablePages> {
        //--- resolve every page before building the carrier, so a failure produces none ---
        let mut pages = Vec::with_capacity(ids.len());
        for id in ids {
            pages.push(self.page(*id)?);
        }
        Ok(PortablePages::new(VecPortablePayload { pages }))
    }

    fn import_portable(&mut self, pages: PortablePages, at_index: usize) -> Result<Vec<PageId>> {
        //--- the position is checked first, matching import_pages' precedence ---
        self.check_insertion_index(at_index)?;
        let payload: VecPortablePayload = pages.take()?;

        let mut new_ids = Vec::with_capacity(payload.pages.len());
        for (offset, page) in payload.pages.into_iter().enumerate() {
            let id = self.allocator.allocate();
            self.pages.insert(at_index + offset, PageInfo { id, ..page });
            new_ids.push(id);
        }
        self.advance_revision();
        Ok(new_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;

    #[test]
    fn builds_a_document_of_the_requested_length() {
        let document = VecDocument::with_pages(3, PageSize::A4);
        assert_eq!(document.page_count(), 3);
    }

    #[test]
    fn keeps_identity_stable_across_removal() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        document.remove_page(ids[0]).unwrap();
        assert_eq!(document.index_of(ids[2]).unwrap(), 1);
        assert_eq!(document.page(ids[2]).unwrap().id, ids[2]);
    }

    /// The round trip has to be exact, not merely the right shape: comparing whole
    /// `PageInfo` values catches a restore that invents a fresh identity, forgets
    /// the rotation, or drops the page back at the wrong index — each of which a
    /// page-count check would wave through.
    #[test]
    fn restores_a_removed_page_into_an_identical_page_list() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        document.set_rotation(ids[1], Rotation::Quarter).unwrap();
        let before = DocumentSnapshot::of(&document).unwrap();

        document.remove_page(ids[1]).unwrap();
        document.restore_page(ids[1], 1).unwrap();

        let after = DocumentSnapshot::of(&document).unwrap();
        assert_eq!(
            after.pages, before.pages,
            "a remove-then-restore round trip must leave the page list exactly as it was, identity and rotation included"
        );
        assert_ne!(
            after.revision, before.revision,
            "restoring is a mutation like any other, so the revision must not return to its pre-removal value"
        );
    }

    /// `move_page` lifts a page out of the vector and puts it straight back. If it
    /// routed through the trash, the copy it left behind would be the *pre-move*
    /// state, and a later restore would resurrect that stale copy in preference to
    /// the page as it actually stood when it was removed.
    #[test]
    fn does_not_route_moves_through_the_trash() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        document.move_page(ids[0], 2).unwrap();
        document.set_rotation(ids[0], Rotation::Quarter).unwrap();
        document.remove_page(ids[0]).unwrap();

        document.restore_page(ids[0], 0).unwrap();

        assert_eq!(
            document.page(ids[0]).unwrap().rotation,
            Rotation::Quarter,
            "a move must not leave a stale copy in the trash for restore_page to find in preference to the real one"
        );
    }

    #[test]
    fn leaves_document_untouched_when_import_names_a_missing_page() {
        let source = VecDocument::with_pages(1, PageSize::A4);
        let mut target = VecDocument::with_pages(2, PageSize::LETTER);
        let result = target.import_pages(&source, &[PageId::new(9999)], 0);
        assert!(result.is_err());
        assert_eq!(target.page_count(), 2);
    }

    #[test]
    fn reports_the_pre_move_page_count_when_rejecting_a_target() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let error = document.move_page(ids[0], 99).unwrap_err();
        assert_eq!(
            error.to_string(),
            "index 99 out of bounds for 3 pages",
            "the count must be the pages present when the move was attempted, not what remained mid-operation"
        );
    }

    #[test]
    fn snapshots_pages_in_document_order() {
        let document = VecDocument::with_pages(2, PageSize::A4);
        let snapshot = DocumentSnapshot::of(&document).unwrap();
        assert_eq!(snapshot.page_count(), 2);
        assert_eq!(snapshot.pages[0].id, document.page_ids()[0]);
    }

    #[test]
    fn snapshots_the_revision_alongside_the_pages() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        assert_eq!(before.revision, document.revision(), "a snapshot must carry the document's revision at capture");

        document.set_rotation(document.page_ids()[0], Rotation::Quarter).unwrap();
        let after = DocumentSnapshot::of(&document).unwrap();

        assert_eq!(
            after.revision,
            document.revision(),
            "a later snapshot must carry the revision current at its own capture"
        );
        assert_ne!(
            before.revision, after.revision,
            "a snapshot taken after a mutation must not report the revision of one taken before it"
        );
    }

    #[cfg(feature = "contract-tests")]
    #[test]
    fn satisfies_the_document_contract() {
        crate::contract::assert_document_contract(|count| VecDocument::with_pages(count, PageSize::A4));
    }
}

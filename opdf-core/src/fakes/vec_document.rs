//! In-memory [`Document`] implementation used to develop and test dependent
//! crates before a real PDF parser exists.

use crate::Result;
use crate::document::Document;
use crate::error::Error;
use crate::page::{PageId, PageIdAllocator, PageInfo, PageSize, Rotation};

/// A document that stores page metadata in a vector and no content at all.
#[derive(Debug, Default)]
pub struct VecDocument {
    pages: Vec<PageInfo>,
    allocator: PageIdAllocator,
    revision: u64,
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
        self.pages.remove(index);
        self.advance_revision();
        Ok(())
    }

    fn move_page(&mut self, id: PageId, to_index: usize) -> Result<()> {
        let from_index = self.find_index(id)?;
        //--- capture the count before removing: the error reports the pages present when the move was attempted ---
        let page_count = self.pages.len();
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

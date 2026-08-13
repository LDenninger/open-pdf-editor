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
        Ok(())
    }

    fn move_page(&mut self, id: PageId, to_index: usize) -> Result<()> {
        let from_index = self.find_index(id)?;
        let page = self.pages.remove(from_index);
        if to_index > self.pages.len() {
            self.pages.insert(from_index, page);
            return Err(Error::IndexOutOfBounds {
                index: to_index,
                page_count: self.pages.len(),
            });
        }
        self.pages.insert(to_index, page);
        Ok(())
    }

    fn set_rotation(&mut self, id: PageId, rotation: Rotation) -> Result<()> {
        let index = self.find_index(id)?;
        self.pages[index].rotation = rotation;
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
    fn snapshots_pages_in_document_order() {
        let document = VecDocument::with_pages(2, PageSize::A4);
        let snapshot = DocumentSnapshot::of(&document).unwrap();
        assert_eq!(snapshot.page_count(), 2);
        assert_eq!(snapshot.pages[0].id, document.page_ids()[0]);
    }
}

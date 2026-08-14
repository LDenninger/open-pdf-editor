//! The page identity layer: a `PageId` to `lopdf::ObjectId` mapping in document order.

// The first non-test caller is `PdfDocument`, which lands later in the track.
// Until then `-D warnings` would reject this module as dead code.
#![allow(dead_code)]

use lopdf::ObjectId;
use opdf_core::{Error, PageId, PageIdAllocator, PageSize, Result, Rotation};

/// One page's identity, its backing PDF object, and the geometry cached at open.
///
/// Geometry is cached rather than re-read so that reading a page never touches
/// the object graph, and so that a rotation change is a pure in-memory edit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageSlot {
    /// Session-stable identity, unaffected by reordering.
    pub(crate) id: PageId,
    /// The `lopdf` object backing this page. Never used as an identity.
    pub(crate) object_id: ObjectId,
    /// Media box dimensions, before rotation.
    pub(crate) size: PageSize,
    /// Rotation currently in effect for this page.
    pub(crate) rotation: Rotation,
    /// Whether `rotation` differs from what the file records, so a save must write `/Rotate`.
    pub(crate) rotation_changed: bool,
}

/// The ordered `PageId` to `ObjectId` mapping that defines document order.
///
/// The vector's order **is** document order. The file's `/Kids` array is
/// regenerated from this vector at save time, never read back into it.
///
/// A removed slot moves to `removed` rather than being dropped, so that
/// [`PageMap::restore_slot`] can hand back the original page — same identity,
/// same object, same geometry, same rotation — instead of an approximation.
/// The trash holds slots, never object bytes: the PDF objects themselves were
/// never deleted, only unlinked from the page tree.
#[derive(Debug, Default)]
pub(crate) struct PageMap {
    slots: Vec<PageSlot>,
    removed: Vec<PageSlot>,
    allocator: PageIdAllocator,
}

impl PageMap {
    //---------------------------------------------------------------------
    // Construction and inspection
    //---------------------------------------------------------------------

    /// An empty map.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of pages currently mapped.
    pub(crate) fn count_pages(&self) -> usize {
        self.slots.len()
    }

    /// Page identities in document order.
    pub(crate) fn collect_ids(&self) -> Vec<PageId> {
        self.slots.iter().map(|slot| slot.id).collect()
    }

    /// Backing object identifiers in document order, as the `/Kids` array needs them.
    pub(crate) fn collect_object_ids(&self) -> Vec<ObjectId> {
        self.slots.iter().map(|slot| slot.object_id).collect()
    }

    /// Every slot in document order.
    pub(crate) fn list_slots(&self) -> &[PageSlot] {
        &self.slots
    }

    /// Position of a page in document order.
    ///
    /// Returns [`Error::PageNotFound`] if the identity is not mapped.
    pub(crate) fn find_index(&self, id: PageId) -> Result<usize> {
        self.slots.iter().position(|slot| slot.id == id).ok_or(Error::PageNotFound(id))
    }

    /// The slot for a page.
    ///
    /// Returns [`Error::PageNotFound`] if the identity is not mapped.
    pub(crate) fn find_slot(&self, id: PageId) -> Result<&PageSlot> {
        let index = self.find_index(id)?;
        Ok(&self.slots[index])
    }

    /// The slot for a page, mutably.
    ///
    /// Returns [`Error::PageNotFound`] if the identity is not mapped.
    pub(crate) fn find_slot_mut(&mut self, id: PageId) -> Result<&mut PageSlot> {
        let index = self.find_index(id)?;
        Ok(&mut self.slots[index])
    }

    /// Reject an insertion position beyond the end of the document.
    ///
    /// A position equal to the page count is a valid append, not an error.
    pub(crate) fn check_insertion_index(&self, index: usize) -> Result<()> {
        if index > self.slots.len() {
            return Err(Error::IndexOutOfBounds {
                index,
                page_count: self.slots.len(),
            });
        }
        Ok(())
    }
}

impl PageMap {
    //---------------------------------------------------------------------
    // Mutation
    //---------------------------------------------------------------------

    /// Append a slot at the end of the document, returning its fresh identity.
    pub(crate) fn append_slot(&mut self, object_id: ObjectId, size: PageSize, rotation: Rotation) -> PageId {
        let id = self.allocator.allocate();
        self.slots.push(PageSlot {
            id,
            object_id,
            size,
            rotation,
            rotation_changed: false,
        });
        id
    }

    /// Insert a slot at a position, returning its fresh identity.
    ///
    /// Returns [`Error::IndexOutOfBounds`] if the position exceeds the page count.
    pub(crate) fn insert_slot(&mut self, at_index: usize, object_id: ObjectId, size: PageSize, rotation: Rotation) -> Result<PageId> {
        self.check_insertion_index(at_index)?;
        let id = self.allocator.allocate();
        self.slots.insert(
            at_index,
            PageSlot {
                id,
                object_id,
                size,
                rotation,
                rotation_changed: false,
            },
        );
        Ok(id)
    }

    /// Remove a page from document order, retaining its slot for restoration.
    ///
    /// Returns [`Error::PageNotFound`] if the identity is not mapped. The
    /// identity is retired from allocation — the allocator never reissues it —
    /// but the slot stays in the trash until [`PageMap::purge_trash`] discards it.
    pub(crate) fn remove_slot(&mut self, id: PageId) -> Result<()> {
        let index = self.find_index(id)?;
        //--- the slot is retained, not dropped, so restore_slot can hand back the original page ---
        let slot = self.slots.remove(index);
        self.removed.push(slot);
        Ok(())
    }

    /// Bring a removed page back at a position, with the slot it had when it left.
    ///
    /// Returns [`Error::Unsupported`] if the identity is currently present —
    /// restoring a live page is a caller error, not a no-op and not a duplicate.
    /// Returns [`Error::PageNotFound`] if the identity is neither present nor in
    /// the trash, and [`Error::IndexOutOfBounds`] if the position exceeds the
    /// page count. Every check runs before the trash is touched, so a rejected
    /// restore leaves the page restorable by a later, valid call.
    pub(crate) fn restore_slot(&mut self, id: PageId, at_index: usize) -> Result<()> {
        if self.slots.iter().any(|slot| slot.id == id) {
            return Err(Error::Unsupported(format!("{id} is currently present and cannot be restored")));
        }
        //--- resolve the identity before the index, matching relocate_slot's precedence ---
        let trash_index = self.removed.iter().position(|slot| slot.id == id).ok_or(Error::PageNotFound(id))?;
        self.check_insertion_index(at_index)?;

        //--- both checks have passed, so nothing below can fail and leave the trash half-emptied ---
        let slot = self.removed.remove(trash_index);
        self.slots.insert(at_index, slot);
        Ok(())
    }

    /// Move a page to a new position, preserving its identity and its object.
    ///
    /// Returns [`Error::PageNotFound`] if the identity is not mapped, and
    /// [`Error::IndexOutOfBounds`] if `to_index` exceeds the range left after
    /// the page is lifted out. The reported `page_count` is the count **before**
    /// the move, and a rejection restores the original order exactly.
    pub(crate) fn relocate_slot(&mut self, id: PageId, to_index: usize) -> Result<()> {
        let from_index = self.find_index(id)?;
        //--- capture the count before removing: the error reports the pages present when the move was attempted ---
        let page_count = self.slots.len();
        let slot = self.slots.remove(from_index);
        if to_index > self.slots.len() {
            //--- the page goes back where it came from, so this rejection is not a change ---
            self.slots.insert(from_index, slot);
            return Err(Error::IndexOutOfBounds { index: to_index, page_count });
        }
        self.slots.insert(to_index, slot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_map(count: usize) -> PageMap {
        let mut map = PageMap::new();
        for index in 0..count {
            map.append_slot((index as u32 + 1, 0), PageSize::A4, Rotation::None);
        }
        map
    }

    #[test]
    fn allocates_a_distinct_identity_per_slot() {
        let map = build_map(3);
        let ids = map.collect_ids();
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
    }

    #[test]
    fn keeps_identity_attached_to_its_object_across_a_move() {
        let mut map = build_map(3);
        let ids = map.collect_ids();
        let object_of_first = map.find_slot(ids[0]).unwrap().object_id;

        map.relocate_slot(ids[0], 2).unwrap();

        assert_eq!(map.collect_ids(), vec![ids[1], ids[2], ids[0]], "relocation must reorder, not renumber");
        assert_eq!(map.find_slot(ids[0]).unwrap().object_id, object_of_first, "a moved page keeps its object");
        assert_eq!(map.find_index(ids[0]).unwrap(), 2);
    }

    #[test]
    fn keeps_surviving_identities_after_a_removal() {
        let mut map = build_map(3);
        let ids = map.collect_ids();
        map.remove_slot(ids[0]).unwrap();

        assert_eq!(map.count_pages(), 2);
        assert!(matches!(map.find_slot(ids[0]), Err(Error::PageNotFound(_))));
        assert_eq!(map.find_index(ids[1]).unwrap(), 0);
        assert_eq!(map.find_index(ids[2]).unwrap(), 1);
    }

    #[test]
    fn rejects_an_unknown_identity_with_page_not_found() {
        let map = build_map(1);
        let unknown = PageId::new(u64::MAX);
        assert!(matches!(map.find_index(unknown), Err(Error::PageNotFound(_))));
        assert!(matches!(map.find_slot(unknown), Err(Error::PageNotFound(_))));
    }

    #[test]
    fn reports_the_pre_move_page_count_when_rejecting_a_target() {
        let mut map = build_map(3);
        let ids = map.collect_ids();
        let error = map.relocate_slot(ids[0], 99).unwrap_err();
        assert_eq!(
            error.to_string(),
            "index 99 out of bounds for 3 pages",
            "the count must be the pages present when the move was attempted, not what remained mid-operation"
        );
        assert_eq!(map.collect_ids(), ids, "a rejected relocation must leave the order untouched");
    }

    #[test]
    fn accepts_an_insertion_at_the_page_count_as_an_append() {
        let mut map = build_map(2);
        let end = map.count_pages();
        let appended = map.insert_slot(end, (9, 0), PageSize::LETTER, Rotation::None).unwrap();
        assert_eq!(map.find_index(appended).unwrap(), 2);
        assert!(matches!(
            map.insert_slot(99, (10, 0), PageSize::A4, Rotation::None),
            Err(Error::IndexOutOfBounds { .. })
        ));
        assert_eq!(map.count_pages(), 3, "a rejected insertion must not add a slot");
    }

    #[test]
    fn allocates_identities_that_were_never_in_use() {
        let mut map = build_map(2);
        let before = map.collect_ids();
        let inserted = map.insert_slot(1, (9, 0), PageSize::A4, Rotation::None).unwrap();
        assert!(!before.contains(&inserted), "a fresh slot must not reuse a retired identity");

        map.remove_slot(inserted).unwrap();
        let reinserted = map.insert_slot(1, (10, 0), PageSize::A4, Rotation::None).unwrap();
        assert_ne!(reinserted, inserted, "an identity is never handed out twice, even after removal");
    }

    #[test]
    fn hands_a_removed_slot_back_with_everything_it_had() {
        let mut map = build_map(3);
        let ids = map.collect_ids();
        map.find_slot_mut(ids[1]).unwrap().rotation = Rotation::Quarter;
        let before = *map.find_slot(ids[1]).unwrap();

        map.remove_slot(ids[1]).unwrap();
        map.restore_slot(ids[1], 0).unwrap();

        let restored = map.find_slot(ids[1]).unwrap();
        assert_eq!(restored.id, before.id, "a restored slot keeps its identity");
        assert_eq!(restored.object_id, before.object_id, "a restored slot points at the same pdf object");
        assert_eq!(restored.size, before.size, "a restored slot keeps its geometry");
        assert_eq!(restored.rotation, before.rotation, "a restored slot keeps its rotation");
        assert_eq!(map.find_index(ids[1]).unwrap(), 0, "a restored slot lands at the requested index");
        assert_eq!(map.count_pages(), 3);
    }

    #[test]
    fn accepts_a_restore_at_the_page_count_as_an_append() {
        let mut map = build_map(2);
        let ids = map.collect_ids();
        map.remove_slot(ids[1]).unwrap();
        let end = map.count_pages();
        map.restore_slot(ids[1], end).unwrap();
        assert_eq!(
            map.find_index(ids[1]).unwrap(),
            1,
            "restoring at the page count must append, or a last-page deletion cannot be undone"
        );
    }

    #[test]
    fn rejects_a_restore_without_consuming_the_page() {
        let mut map = build_map(3);
        let ids = map.collect_ids();
        map.remove_slot(ids[0]).unwrap();
        let order = map.collect_ids();

        assert!(matches!(map.restore_slot(ids[0], 99), Err(Error::IndexOutOfBounds { .. })));
        assert_eq!(map.collect_ids(), order, "a rejected restore must leave the order untouched");
        map.restore_slot(ids[0], 0)
            .expect("a restore rejected for its index must not have discarded the page");
    }

    #[test]
    fn distinguishes_an_unknown_identity_from_a_live_one() {
        let mut map = build_map(2);
        let ids = map.collect_ids();
        let unknown = PageId::new(u64::MAX);
        assert!(
            matches!(map.restore_slot(unknown, 0), Err(Error::PageNotFound(_))),
            "an identity the map never held is not restorable"
        );
        assert!(
            matches!(map.restore_slot(ids[0], 0), Err(Error::Unsupported(_))),
            "restoring a live page is a caller error, not a no-op and not a duplicate"
        );
        assert_eq!(map.collect_ids(), ids, "a rejected restore must leave the order untouched");
    }
}

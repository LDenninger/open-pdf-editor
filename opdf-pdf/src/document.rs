//! `PdfDocument`: a real PDF file behind the `Document` contract.

// The dirty state and the `lopdf` accessors gain their first non-test callers
// when the mutations and the save path land later in the track. Until then
// `-D warnings` would reject them as dead code.
#![allow(dead_code)]

use lopdf::{IncrementalDocument, Object, ObjectId};
use opdf_core::{Document, Error, PageId, PageInfo, Result};

use crate::error::convert_lopdf_error;
use crate::geometry::{read_page_rotation, read_page_size};
use crate::objects::ObjectSource;
use crate::page_map::PageMap;

/// What a save still has to write out.
///
/// Tracked separately because the two kinds of change need different work: a
/// rotation touches one page dictionary, while a change to the page set or
/// order rewrites the page tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirtyState {
    /// The page set or their order differs from the file's page tree.
    pub(crate) structure: bool,
    /// At least one page's rotation differs from what the file records.
    pub(crate) rotations: bool,
}

impl DirtyState {
    /// Whether the in-memory document still matches the bytes it was opened from.
    pub(crate) const fn is_clean(self) -> bool {
        !self.structure && !self.rotations
    }
}

/// A PDF file behind the [`Document`] contract.
///
/// Page order and identity live in memory; the file's page tree is regenerated
/// from them when the document is saved. The original bytes are retained in
/// full so that a save appends an incremental update rather than rewriting.
#[derive(Debug)]
pub struct PdfDocument {
    incremental: IncrementalDocument,
    root_pages_id: ObjectId,
    pub(crate) pages: PageMap,
    revision: u64,
    pub(crate) dirty: DirtyState,
}

impl PdfDocument {
    //---------------------------------------------------------------------
    // Construction
    //---------------------------------------------------------------------

    /// Read a document from bytes already in memory.
    ///
    /// Equivalent to [`opdf_core::DocumentIo::open`] without a file: the bytes
    /// are retained verbatim, so a later incremental save still appends to them.
    ///
    /// Returns [`Error::Malformed`] if the bytes are not a parseable PDF or
    /// contain no pages.
    pub fn load_from_bytes(bytes: &[u8]) -> Result<Self> {
        let incremental = IncrementalDocument::load_from(bytes).map_err(convert_lopdf_error)?;
        Self::build_from_incremental(incremental)
    }

    /// Build the identity map and cached geometry from a parsed document.
    pub(crate) fn build_from_incremental(mut incremental: IncrementalDocument) -> Result<Self> {
        let (root_pages_id, pages, version) = {
            let previous = incremental.get_prev_documents();
            let root_pages_id = previous
                .catalog()
                .and_then(|catalog| catalog.get(b"Pages"))
                .and_then(Object::as_reference)
                .map_err(convert_lopdf_error)?;

            let mut pages = PageMap::new();
            for object_id in previous.page_iter() {
                let size = read_page_size(previous, object_id);
                let rotation = read_page_rotation(previous, object_id);
                pages.append_slot(object_id, size, rotation);
            }
            (root_pages_id, pages, previous.version.clone())
        };

        if pages.count_pages() == 0 {
            return Err(Error::Malformed("document contains no pages".to_string()));
        }

        //--- an appended revision announces the same version as the file it extends ---
        incremental.new_document.version = version;

        Ok(Self {
            incremental,
            root_pages_id,
            pages,
            revision: 0,
            dirty: DirtyState::default(),
        })
    }

    //---------------------------------------------------------------------
    // Access for the save path
    //---------------------------------------------------------------------

    /// The page tree root every save rewrites.
    pub(crate) const fn root_pages_id(&self) -> ObjectId {
        self.root_pages_id
    }

    /// The `lopdf` layer, for the save path only.
    pub(crate) const fn incremental(&self) -> &IncrementalDocument {
        &self.incremental
    }

    /// The `lopdf` layer, mutably, for the save path and object creation only.
    pub(crate) const fn incremental_mut(&mut self) -> &mut IncrementalDocument {
        &mut self.incremental
    }

    /// Advance the revision counter, called once by each mutation that succeeds.
    ///
    /// Wrapping is deliberate: the counter is opaque and only ever compared for
    /// equality, so an overflow panic would be worse than the unreachable
    /// collision it guards against.
    pub(crate) fn advance_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

impl ObjectSource for PdfDocument {
    fn find_object(&self, object_id: ObjectId) -> Option<&Object> {
        //--- an appended object shadows the previous revision's object of the same id ---
        self.incremental
            .new_document
            .objects
            .get(&object_id)
            .or_else(|| self.incremental.get_prev_documents().objects.get(&object_id))
    }
}

impl Document for PdfDocument {
    //---------------------------------------------------------------------
    // Inspection
    //---------------------------------------------------------------------

    fn revision(&self) -> u64 {
        self.revision
    }

    fn page_count(&self) -> usize {
        self.pages.count_pages()
    }

    fn page_ids(&self) -> Vec<PageId> {
        self.pages.collect_ids()
    }

    fn page(&self, id: PageId) -> Result<PageInfo> {
        let slot = self.pages.find_slot(id)?;
        Ok(PageInfo {
            id: slot.id,
            size: slot.size,
            rotation: slot.rotation,
        })
    }

    fn index_of(&self, id: PageId) -> Result<usize> {
        self.pages.find_index(id)
    }

    //---------------------------------------------------------------------
    // Mutation (implemented in Task 7, Task 8, and Task 8b)
    //---------------------------------------------------------------------

    fn remove_page(&mut self, id: PageId) -> Result<()> {
        self.pages.remove_slot(id)?;
        self.dirty.structure = true;
        self.advance_revision();
        Ok(())
    }

    fn restore_page(&mut self, id: PageId, at_index: usize) -> Result<()> {
        Err(Error::Unsupported(format!("restore_page({id}, {at_index}) is not implemented yet")))
    }

    fn move_page(&mut self, id: PageId, to_index: usize) -> Result<()> {
        self.pages.relocate_slot(id, to_index)?;
        self.dirty.structure = true;
        self.advance_revision();
        Ok(())
    }

    fn set_rotation(&mut self, id: PageId, rotation: opdf_core::Rotation) -> Result<()> {
        let slot = self.pages.find_slot_mut(id)?;
        slot.rotation = rotation;
        //--- record it even when the value is unchanged: the file may inherit a different one from an ancestor ---
        slot.rotation_changed = true;
        self.dirty.rotations = true;
        self.advance_revision();
        Ok(())
    }

    fn insert_page(&mut self, at_index: usize, size: opdf_core::PageSize) -> Result<PageId> {
        Err(Error::Unsupported(format!("insert_page({at_index}, {size:?}) is not implemented yet")))
    }

    fn import_pages(&mut self, _source: &Self, ids: &[PageId], at_index: usize) -> Result<Vec<PageId>> {
        Err(Error::Unsupported(format!(
            "import_pages({} ids, {at_index}) is not implemented yet",
            ids.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use opdf_core::{PageSize, Rotation};

    #[test]
    fn reports_the_page_count_of_a_flat_document() {
        let document = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[PageSize::A4; 3])).unwrap();
        assert_eq!(document.page_count(), 3);
        assert_eq!(document.page_ids().len(), 3);
    }

    #[test]
    fn enumerates_a_nested_tree_in_depth_first_order() {
        let document = PdfDocument::load_from_bytes(&fixture::build_nested_page_tree()).unwrap();
        let widths: Vec<f32> = document.page_ids().into_iter().map(|id| document.page(id).unwrap().size.width_pt).collect();
        assert_eq!(widths, vec![100.0, 200.0, 300.0], "a nested page tree must flatten depth-first");
    }

    #[test]
    fn lists_identities_in_document_order() {
        let document = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[PageSize::A4; 4])).unwrap();
        for (index, id) in document.page_ids().into_iter().enumerate() {
            assert_eq!(document.index_of(id).unwrap(), index);
        }
    }

    #[test]
    fn caches_geometry_from_the_object_graph() {
        let document = PdfDocument::load_from_bytes(&fixture::build_rotated_pages()).unwrap();
        let rotations: Vec<Rotation> = document.page_ids().into_iter().map(|id| document.page(id).unwrap().rotation).collect();
        assert_eq!(rotations, vec![Rotation::Quarter, Rotation::Half, Rotation::ThreeQuarter]);
    }

    #[test]
    fn rejects_an_unknown_identity_with_page_not_found() {
        let document = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[PageSize::A4])).unwrap();
        let unknown = opdf_core::PageId::new(u64::MAX);
        assert!(matches!(document.page(unknown), Err(opdf_core::Error::PageNotFound(_))));
        assert!(matches!(document.index_of(unknown), Err(opdf_core::Error::PageNotFound(_))));
    }

    #[test]
    fn rejects_a_damaged_file_as_malformed() {
        let outcome = PdfDocument::load_from_bytes(&fixture::build_damaged_bytes());
        assert!(
            matches!(outcome, Err(opdf_core::Error::Malformed(_))),
            "a damaged file must not panic, got: {outcome:?}"
        );
    }

    #[test]
    fn starts_at_a_revision_that_reading_never_advances() {
        let document = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[PageSize::A4; 2])).unwrap();
        let before = document.revision();
        let ids = document.page_ids();
        let _ = document.page_count();
        let _ = document.page(ids[0]);
        let _ = document.index_of(ids[0]);
        assert_eq!(before, document.revision(), "inspecting a document must not advance its revision");
    }

    fn build_document(count: usize) -> PdfDocument {
        PdfDocument::load_from_bytes(&fixture::build_flat_pages(&vec![PageSize::A4; count])).expect("fixture must open")
    }

    #[test]
    fn removes_a_page_without_disturbing_its_neighbours() {
        let mut document = build_document(3);
        let ids = document.page_ids();
        document.remove_page(ids[0]).unwrap();

        assert_eq!(document.page_count(), 2);
        assert!(matches!(document.page(ids[0]), Err(opdf_core::Error::PageNotFound(_))));
        assert_eq!(document.index_of(ids[1]).unwrap(), 0);
        assert_eq!(document.index_of(ids[2]).unwrap(), 1);
    }

    #[test]
    fn moves_a_page_without_changing_any_identity() {
        let mut document = build_document(3);
        let ids = document.page_ids();
        document.move_page(ids[0], 2).unwrap();
        assert_eq!(document.page_ids(), vec![ids[1], ids[2], ids[0]]);
        assert_eq!(document.page_count(), 3);
    }

    #[test]
    fn rejects_a_move_beyond_the_document_and_keeps_the_order() {
        let mut document = build_document(2);
        let ids = document.page_ids();
        assert!(matches!(document.move_page(ids[0], 99), Err(opdf_core::Error::IndexOutOfBounds { .. })));
        assert_eq!(document.page_ids(), ids);
    }

    #[test]
    fn round_trips_a_rotation_including_clearing_it() {
        let mut document = build_document(1);
        let id = document.page_ids()[0];
        document.set_rotation(id, Rotation::Quarter).unwrap();
        assert_eq!(document.page(id).unwrap().rotation, Rotation::Quarter);
        document.set_rotation(id, Rotation::None).unwrap();
        assert_eq!(document.page(id).unwrap().rotation, Rotation::None);
    }

    #[test]
    fn advances_the_revision_on_each_successful_mutation() {
        let mut document = build_document(3);
        let ids = document.page_ids();

        let before = document.revision();
        document.remove_page(ids[0]).unwrap();
        assert_ne!(before, document.revision(), "remove_page must advance the revision");

        let before = document.revision();
        document.move_page(ids[1], 1).unwrap();
        assert_ne!(before, document.revision(), "move_page must advance the revision");

        let before = document.revision();
        document.set_rotation(ids[1], Rotation::Half).unwrap();
        assert_ne!(before, document.revision(), "set_rotation must advance the revision");
    }

    #[test]
    fn leaves_the_revision_untouched_when_a_mutation_is_rejected() {
        let mut document = build_document(3);
        let ids = document.page_ids();
        let unknown = opdf_core::PageId::new(u64::MAX);
        let before = document.revision();

        assert!(document.remove_page(unknown).is_err());
        assert!(document.move_page(unknown, 0).is_err());
        assert!(document.set_rotation(unknown, Rotation::Quarter).is_err());
        assert!(document.move_page(ids[0], 99).is_err());
        assert_eq!(
            before,
            document.revision(),
            "a rejected mutation must leave the revision untouched, even where the page was lifted out and put back"
        );
    }

    #[test]
    fn prefers_page_not_found_over_index_out_of_bounds() {
        let mut document = build_document(2);
        let unknown = opdf_core::PageId::new(u64::MAX);
        assert!(
            matches!(document.move_page(unknown, 99), Err(opdf_core::Error::PageNotFound(_))),
            "identity is checked before position"
        );
    }
}

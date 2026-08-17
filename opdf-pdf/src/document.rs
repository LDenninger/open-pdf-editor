//! `PdfDocument`: a real PDF file behind the `Document` contract.

use lopdf::{IncrementalDocument, Object, ObjectId};
use opdf_core::{Document, DocumentId, Error, PageId, PageInfo, PortablePages, Result};

use crate::error::convert_lopdf_error;
use crate::geometry::{read_page_rotation, read_page_size};
use crate::objects::{ObjectSource, build_blank_page, build_empty_document_bytes, copy_page_into};
use crate::page_map::PageMap;

/// The page tree root a document's catalog names.
///
/// Returns [`Error::Malformed`] if the catalog is missing or does not name a
/// page tree, which is the same answer parsing gives for a file without one.
fn read_root_pages_id(previous: &lopdf::Document) -> Result<ObjectId> {
    previous
        .catalog()
        .and_then(|catalog| catalog.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(convert_lopdf_error)
}

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
    id: DocumentId,
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

    /// Create a document with no pages.
    ///
    /// The result is a real, saveable PDF the moment it has a page: it is built
    /// from a minimal catalog and an empty page tree, then opened exactly as a
    /// file would be, so it satisfies the [`Document`] contract and takes the
    /// same save path as a parsed document. It has no pages yet, so both save
    /// methods reject it with [`Error::Unsupported`] until one is added by
    /// [`Document::insert_page`] or [`Document::import_pages`].
    ///
    /// Returns [`Error::Malformed`] if the generated document does not parse,
    /// which would mean this crate cannot read what it writes.
    pub fn empty() -> Result<Self> {
        let bytes = build_empty_document_bytes()?;
        let mut incremental = IncrementalDocument::load_from(&bytes[..]).map_err(convert_lopdf_error)?;
        let (root_pages_id, version) = {
            let previous = incremental.get_prev_documents();
            (read_root_pages_id(previous)?, previous.version.clone())
        };

        //--- an appended revision announces the same version as the file it extends ---
        incremental.new_document.version = version;

        Ok(Self {
            id: DocumentId::new_unique(),
            incremental,
            root_pages_id,
            pages: PageMap::new(),
            revision: 0,
            dirty: DirtyState::default(),
        })
    }

    /// Build the identity map and cached geometry from a parsed document.
    pub(crate) fn build_from_incremental(mut incremental: IncrementalDocument) -> Result<Self> {
        let (root_pages_id, pages, version) = {
            let previous = incremental.get_prev_documents();
            let root_pages_id = read_root_pages_id(previous)?;

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
            id: DocumentId::new_unique(),
            incremental,
            root_pages_id,
            pages,
            revision: 0,
            dirty: DirtyState::default(),
        })
    }

    /// Continue this document from bytes that already hold its whole state.
    ///
    /// A compacting save rewrites the file from scratch: the revision history is
    /// gone, the surviving objects are renumbered, and every pending change has
    /// been written out. The document must follow, or the next incremental save
    /// would append to bytes that no longer describe it — re-emitting the file
    /// the compaction was asked to replace, purged objects included.
    ///
    /// Identity is preserved: the document's own [`Document::id`], its
    /// `PageId`s, page order, geometry and rotation all stay as they were,
    /// because the bytes were written from them. Only the backing objects move,
    /// and the revision counter does not advance — the document a reader sees is
    /// unchanged. Minting a fresh identity here would tell every cache and every
    /// bound command that the user had opened a different file, when all that
    /// happened is that the one they have open was rewritten.
    ///
    /// Returns [`Error::Malformed`] if the bytes do not parse, name no page
    /// tree, or hold a different number of pages than this document does.
    /// Nothing is changed unless every check passes.
    pub(crate) fn rebase_onto(&mut self, bytes: &[u8]) -> Result<()> {
        let mut incremental = IncrementalDocument::load_from(bytes).map_err(convert_lopdf_error)?;
        let (root_pages_id, object_ids, version) = {
            let previous = incremental.get_prev_documents();
            let object_ids: Vec<ObjectId> = previous.page_iter().collect();
            (read_root_pages_id(previous)?, object_ids, previous.version.clone())
        };

        //--- the only fallible step that touches state checks itself before writing anything ---
        self.pages.rebind_objects(&object_ids)?;

        //--- an appended revision announces the same version as the file it extends ---
        incremental.new_document.version = version;
        //--- `self.id` is deliberately untouched: this is the same open document, rewritten ---
        self.incremental = incremental;
        self.root_pages_id = root_pages_id;
        //--- the new base already carries every change, so there is nothing left to append ---
        self.dirty = DirtyState::default();
        Ok(())
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

    fn id(&self) -> DocumentId {
        self.id
    }

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
        //--- no object work: the page's dictionary and streams were never deleted, only unlinked from the page tree ---
        self.pages.restore_slot(id, at_index)?;
        self.dirty.structure = true;
        self.advance_revision();
        Ok(())
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
        //--- check before allocating, so a rejected insertion leaves no orphan object behind ---
        self.pages.check_insertion_index(at_index)?;
        let parent_id = self.root_pages_id;
        let object_id = build_blank_page(&mut self.incremental.new_document, parent_id, size);
        let id = self.pages.insert_slot(at_index, object_id, size, opdf_core::Rotation::None)?;
        self.dirty.structure = true;
        self.advance_revision();
        Ok(id)
    }

    fn import_pages(&mut self, source: &Self, ids: &[PageId], at_index: usize) -> Result<Vec<PageId>> {
        self.pages.check_insertion_index(at_index)?;

        //--- resolve every source page before touching the target, so a failure leaves no partial import ---
        let mut resolved = Vec::with_capacity(ids.len());
        for id in ids {
            let slot = source.pages.find_slot(*id)?;
            resolved.push((slot.object_id, slot.size, slot.rotation));
        }

        let mut imported = Vec::with_capacity(resolved.len());
        for (offset, (object_id, size, rotation)) in resolved.into_iter().enumerate() {
            let copied = copy_page_into(source, object_id, &mut self.incremental.new_document)
                .ok_or_else(|| Error::Malformed(format!("source page object {object_id:?} could not be copied")))?;
            let id = self.pages.insert_slot(at_index + offset, copied, size, rotation)?;
            //--- the rotation is carried in memory but nothing has written it down: the source may
            //--- have rotated the page without saving, and `copy_page_into` copies the page's own
            //--- dictionary without the /Rotate it may have been inheriting from a parent node that
            //--- is not copied. Marking it makes the next save state it explicitly, either way.
            self.pages.find_slot_mut(id)?.rotation_changed = true;
            imported.push(id);
        }

        //--- an empty import changes nothing on disk, so it must not force the page tree to be rewritten ---
        if !imported.is_empty() {
            self.dirty.structure = true;
            self.dirty.rotations = true;
        }
        //--- but it still advances the revision: a spurious cache miss is cheaper than a stale tile ---
        self.advance_revision();
        Ok(imported)
    }

    /// Serialize the requested pages into a standalone PDF and carry its bytes.
    ///
    /// The pages are first imported into a scratch document, which reuses
    /// [`Document::import_pages`]'s object-graph copy verbatim — the same
    /// traversal, the same reference remapping — and only then serialized. Going
    /// through the importer rather than reaching into the object graph again is
    /// what keeps the two paths from drifting apart.
    ///
    /// Bytes rather than an `lopdf` structure because the carrier is opaque, so
    /// nothing is gained by exposing a richer payload, and bytes are the one
    /// representation this crate is already obliged to read and write correctly.
    fn export_pages(&self, ids: &[PageId]) -> Result<PortablePages> {
        //--- resolve every page before building anything, so a failure produces no carrier ---
        for id in ids {
            self.pages.find_slot(*id)?;
        }
        if ids.is_empty() {
            //--- a PDF with no pages does not parse, so an empty export is carried as empty bytes ---
            return Ok(PortablePages::new(PdfPortablePayload { bytes: Vec::new() }));
        }

        let mut scratch = Self::empty()?;
        scratch.import_pages(self, ids, 0)?;
        scratch.materialize_changes()?;

        let mut bytes = Vec::new();
        //--- `Document::save_to` reports through `std::io::Error`, not `lopdf::Error` ---
        scratch.build_compacted_document().save_to(&mut bytes).map_err(Error::from)?;
        Ok(PortablePages::new(PdfPortablePayload { bytes }))
    }

    fn import_portable(&mut self, pages: PortablePages, at_index: usize) -> Result<Vec<PageId>> {
        //--- the position is checked first, matching import_pages' precedence ---
        self.pages.check_insertion_index(at_index)?;
        let payload: PdfPortablePayload = pages.take()?;

        if payload.bytes.is_empty() {
            //--- an empty import changes nothing on disk but still advances the revision, exactly as import_pages does ---
            self.advance_revision();
            return Ok(Vec::new());
        }

        let source = Self::load_from_bytes(&payload.bytes)?;
        let ids = source.page_ids();
        self.import_pages(&source, &ids, at_index)
    }
}

/// What a [`PdfDocument`] puts inside a [`PortablePages`]: a standalone PDF.
///
/// Private on purpose. Privacy is what makes the carrier implementation-specific:
/// no other crate can name this type, so no other implementation can take a
/// `PdfDocument`'s payload back out — [`PortablePages::take`] refuses instead.
#[derive(Debug)]
struct PdfPortablePayload {
    /// A complete PDF holding the exported pages, in order. Empty for an export
    /// of no pages, which is not a document a parser would accept.
    bytes: Vec<u8>,
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

    #[test]
    fn inserts_a_blank_page_with_a_fresh_identity() {
        let mut document = build_document(2);
        let before = document.page_ids();
        let inserted = document.insert_page(1, PageSize::LETTER).unwrap();

        assert!(!before.contains(&inserted), "insert_page must return an identity not already in use");
        assert_eq!(document.index_of(inserted).unwrap(), 1);
        assert_eq!(document.page_count(), 3);
        assert_eq!(document.page(inserted).unwrap().size, PageSize::LETTER);
    }

    #[test]
    fn accepts_an_insertion_at_the_page_count_and_rejects_one_beyond_it() {
        let mut document = build_document(2);
        let end = document.page_count();
        let appended = document.insert_page(end, PageSize::A4).unwrap();
        assert_eq!(document.index_of(appended).unwrap(), 2);

        let before = document.revision();
        assert!(matches!(document.insert_page(99, PageSize::A4), Err(opdf_core::Error::IndexOutOfBounds { .. })));
        assert_eq!(before, document.revision(), "a rejected insertion must leave the revision untouched");
        assert_eq!(document.page_count(), 3, "a rejected insertion must not add a page");
    }

    #[test]
    fn imports_pages_in_the_requested_order_with_fresh_identities() {
        let source = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[
            PageSize::new(100.0, 100.0),
            PageSize::new(200.0, 200.0),
            PageSize::new(300.0, 300.0),
        ]))
        .unwrap();
        let mut target = build_document(2);
        let target_ids = target.page_ids();

        let imported = target.import_pages(&source, &source.page_ids(), 1).unwrap();

        assert_eq!(imported.len(), 3);
        assert_eq!(target.page_count(), 5);
        for (offset, id) in imported.iter().enumerate() {
            assert_eq!(target.index_of(*id).unwrap(), 1 + offset, "import must preserve the requested order");
            assert!(!target_ids.contains(id), "imported pages must receive identities not already in use");
        }
        assert_eq!(target.index_of(target_ids[1]).unwrap(), 4, "pages after the insertion point must shift");
        let widths: Vec<f32> = imported.iter().map(|id| target.page(*id).unwrap().size.width_pt).collect();
        assert_eq!(widths, vec![100.0, 200.0, 300.0], "imported geometry must survive the copy");
    }

    #[test]
    fn rejects_an_import_naming_an_unknown_source_page() {
        let source = build_document(1);
        let mut target = build_document(2);
        let before = target.page_ids();
        let before_revision = target.revision();

        let mut ids = source.page_ids();
        ids.push(opdf_core::PageId::new(u64::MAX));

        assert!(matches!(target.import_pages(&source, &ids, 0), Err(opdf_core::Error::PageNotFound(_))));
        assert_eq!(target.page_ids(), before, "a rejected import must leave the document untouched");
        assert_eq!(before_revision, target.revision());
    }

    #[test]
    fn rejects_an_import_beyond_the_target_document() {
        let source = build_document(1);
        let mut target = build_document(2);
        assert!(matches!(
            target.import_pages(&source, &source.page_ids(), 99),
            Err(opdf_core::Error::IndexOutOfBounds { .. })
        ));
        assert_eq!(target.page_count(), 2);
    }

    #[test]
    fn advances_the_revision_on_insert_and_import_including_an_empty_import() {
        let source = build_document(1);
        let mut target = build_document(2);

        let before = target.revision();
        target.insert_page(1, PageSize::A4).unwrap();
        assert_ne!(before, target.revision(), "insert_page must advance the revision");

        let before = target.revision();
        target.import_pages(&source, &source.page_ids(), 1).unwrap();
        assert_ne!(before, target.revision(), "import_pages must advance the revision");

        let before = target.revision();
        target.import_pages(&source, &[], 0).unwrap();
        assert_ne!(before, target.revision(), "an empty import still advances the revision");
    }

    #[test]
    fn restores_a_removed_page_with_its_original_identity_and_geometry() {
        let mut document = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[
            PageSize::new(100.0, 100.0),
            PageSize::new(200.0, 200.0),
            PageSize::A4,
        ]))
        .unwrap();
        let ids = document.page_ids();
        document.set_rotation(ids[1], Rotation::Quarter).unwrap();
        let before = document.page(ids[1]).unwrap();

        document.remove_page(ids[1]).unwrap();
        document.restore_page(ids[1], 0).unwrap();

        assert_eq!(
            document.page_ids().first().copied(),
            Some(ids[1]),
            "a restored page keeps its original identity"
        );
        let restored = document.page(ids[1]).unwrap();
        assert_eq!(restored.size, before.size, "a restored page keeps the geometry it had when it was removed");
        assert_eq!(
            restored.rotation, before.rotation,
            "a restored page keeps the rotation it had when it was removed"
        );
        assert_eq!(document.index_of(ids[1]).unwrap(), 0);
        assert_eq!(document.page_count(), 3);
    }

    #[test]
    fn restores_a_removed_page_onto_the_same_pdf_object() {
        let mut document = build_document(2);
        let ids = document.page_ids();
        let object_before = document.pages.find_slot(ids[0]).unwrap().object_id;

        document.remove_page(ids[0]).unwrap();
        document.restore_page(ids[0], 1).unwrap();

        assert_eq!(
            document.pages.find_slot(ids[0]).unwrap().object_id,
            object_before,
            "a restored page must point at the object it always pointed at, not a copy"
        );
    }

    #[test]
    fn restores_the_deletion_of_a_last_page() {
        let mut document = build_document(2);
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        let end = document.page_count();
        document.restore_page(ids[1], end).unwrap();
        assert_eq!(document.index_of(ids[1]).unwrap(), 1);
    }

    #[test]
    fn distinguishes_the_three_ways_a_restore_can_be_rejected() {
        let mut document = build_document(3);
        let ids = document.page_ids();
        document.remove_page(ids[0]).unwrap();
        let order = document.page_ids();
        let revision = document.revision();

        let rejected = document.restore_page(ids[0], 99);
        assert!(
            matches!(rejected, Err(opdf_core::Error::IndexOutOfBounds { .. })),
            "out of range, got: {rejected:?}"
        );

        let rejected = document.restore_page(opdf_core::PageId::new(u64::MAX), 0);
        assert!(matches!(rejected, Err(opdf_core::Error::PageNotFound(_))), "never held, got: {rejected:?}");

        let rejected = document.restore_page(ids[1], 0);
        assert!(
            matches!(rejected, Err(opdf_core::Error::Unsupported(_))),
            "currently present, got: {rejected:?}"
        );

        assert_eq!(document.page_ids(), order, "a rejected restore must leave the document untouched");
        assert_eq!(revision, document.revision(), "a failed restore_page must leave the revision untouched");
        document.restore_page(ids[0], 0).expect("no rejection may consume the trashed page");
    }

    #[test]
    fn advances_the_revision_when_a_restore_succeeds() {
        let mut document = build_document(2);
        let ids = document.page_ids();
        document.remove_page(ids[0]).unwrap();
        let before = document.revision();
        document.restore_page(ids[0], 0).unwrap();
        assert_ne!(before, document.revision(), "restore_page must advance the revision on success");
    }

    //---------------------------------------------------------------------
    // Portable pages
    //---------------------------------------------------------------------

    /// The reason option 2 was chosen over making `import_pages` object-safe:
    /// the copy still goes through the object graph, so geometry and rotation
    /// survive it. An object-safe `import_pages` taking `&dyn Document` could
    /// only have moved `PageInfo` metadata across and rebuilt blank pages.
    #[test]
    fn exports_and_imports_pages_with_their_geometry_and_rotation_intact() {
        let mut source = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[
            PageSize::new(100.0, 100.0),
            PageSize::new(200.0, 200.0),
            PageSize::new(300.0, 300.0),
        ]))
        .unwrap();
        let source_ids = source.page_ids();
        source.set_rotation(source_ids[1], Rotation::Quarter).unwrap();

        let mut target = build_document(2);
        let target_ids = target.page_ids();
        let carrier = source.export_pages(&source_ids[0..2]).unwrap();
        let imported = target.import_portable(carrier, 1).unwrap();

        assert_eq!(imported.len(), 2);
        assert_eq!(target.page_count(), 4);
        let widths: Vec<f32> = imported.iter().map(|id| target.page(*id).unwrap().size.width_pt).collect();
        assert_eq!(
            widths,
            vec![100.0, 200.0],
            "the carrier must preserve each page's own geometry, not a default size"
        );
        assert_eq!(
            target.page(imported[1]).unwrap().rotation,
            Rotation::Quarter,
            "the carrier must preserve a rotation the source had not saved"
        );
        assert_eq!(target.index_of(target_ids[1]).unwrap(), 3, "pages after the insertion point must shift");
        assert_eq!(source.page_count(), 3, "exporting must not mutate the source");
    }

    /// The cross-implementation case, with two *real* implementations rather
    /// than the contract suite's stand-in.
    #[test]
    fn refuses_a_carrier_exported_by_a_different_implementation() {
        let other = opdf_core::fakes::VecDocument::with_pages(2, PageSize::A4);
        let carrier = other.export_pages(&other.page_ids()).unwrap();

        let mut target = build_document(2);
        let before = target.page_ids();
        let before_revision = target.revision();

        let refused = target.import_portable(carrier, 1);
        assert!(
            matches!(refused, Err(opdf_core::Error::Unsupported(_))),
            "a VecDocument's carrier must be refused, not reinterpreted as PDF bytes, got: {refused:?}"
        );
        assert_eq!(target.page_ids(), before, "a refused import must leave the document untouched");
        assert_eq!(before_revision, target.revision(), "a refused import must leave the revision untouched");
    }

    #[test]
    fn satisfies_the_document_contract() {
        opdf_core::contract::assert_document_contract(|count| {
            PdfDocument::load_from_bytes(&fixture::build_flat_pages(&vec![PageSize::A4; count])).expect("a generated fixture must open")
        });
    }

    //---------------------------------------------------------------------
    // Documents created from nothing
    //---------------------------------------------------------------------

    #[test]
    fn creates_a_document_that_holds_no_pages() {
        let document = PdfDocument::empty().unwrap();
        assert_eq!(document.page_count(), 0);
        assert!(document.page_ids().is_empty());
        assert_eq!(document.revision(), 0, "a created document starts where a parsed one does");
    }

    #[test]
    fn fills_a_created_document_by_inserting_pages() {
        let mut document = PdfDocument::empty().unwrap();
        let first = document.insert_page(0, PageSize::A4).unwrap();
        let second = document.insert_page(1, PageSize::LETTER).unwrap();

        assert_eq!(document.page_ids(), vec![first, second]);
        assert_eq!(document.page(first).unwrap().size, PageSize::A4);
        assert_eq!(document.page(second).unwrap().size, PageSize::LETTER);
    }

    #[test]
    fn rejects_an_insertion_past_the_end_of_a_created_document() {
        let mut document = PdfDocument::empty().unwrap();
        assert!(matches!(document.insert_page(1, PageSize::A4), Err(opdf_core::Error::IndexOutOfBounds { .. })));
        assert_eq!(document.page_count(), 0, "a rejected insertion must not add a page");
    }

    #[test]
    fn fills_a_created_document_by_importing_pages() {
        let source = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0)])).unwrap();
        let mut document = PdfDocument::empty().unwrap();
        let imported = document.import_pages(&source, &source.page_ids(), 0).unwrap();

        assert_eq!(imported.len(), 2);
        assert_eq!(document.page_count(), 2);
        let widths: Vec<f32> = imported.iter().map(|id| document.page(*id).unwrap().size.width_pt).collect();
        assert_eq!(widths, vec![100.0, 200.0], "imported geometry must survive the copy into a created document");
    }

    #[test]
    fn a_created_document_satisfies_the_document_contract() {
        opdf_core::contract::assert_document_contract(|count| {
            let mut document = PdfDocument::empty().expect("a created document must be constructible");
            for index in 0..count {
                document.insert_page(index, PageSize::A4).expect("appending to a created document must succeed");
            }
            document
        });
    }
}

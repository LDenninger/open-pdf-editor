//! Writing in-memory page state back into PDF objects, and the `DocumentIo` contract.

use std::path::Path;

use lopdf::{IncrementalDocument, Object, ObjectId};
use opdf_core::{Document as _, DocumentIo, Error, Result};

use crate::document::PdfDocument;
use crate::error::convert_lopdf_error;
use crate::objects::{INHERITABLE_KEYS, find_inherited_attribute};

impl PdfDocument {
    //---------------------------------------------------------------------
    // Materialisation
    //---------------------------------------------------------------------

    /// Write every pending in-memory change into the appended revision.
    ///
    /// Structural changes come first, because flattening the page tree writes
    /// inherited attributes onto each page — including `/Rotate`, which an
    /// explicit rotation must then overwrite.
    pub(crate) fn materialize_changes(&mut self) -> Result<()> {
        if self.dirty.structure {
            self.materialize_inherited_attributes()?;
            self.rewrite_page_tree()?;
        }
        if self.dirty.rotations {
            self.write_rotation_overrides()?;
        }
        Ok(())
    }

    /// Copy every attribute a page inherits onto the page itself.
    ///
    /// Flattening the tree removes the intermediate `/Pages` nodes from the page
    /// hierarchy, so anything a page was inheriting through them has to be
    /// written down first or it is silently lost. Only pages that came from a
    /// previous revision need this: pages this session created or imported were
    /// built self-contained.
    fn materialize_inherited_attributes(&mut self) -> Result<()> {
        let mut pending: Vec<(ObjectId, &'static [u8], Object)> = Vec::new();
        {
            let previous = self.incremental().get_prev_documents();
            for slot in self.pages.list_slots() {
                if !previous.has_object(slot.object_id) {
                    continue;
                }
                let carries_own = |key: &[u8]| previous.get_dictionary(slot.object_id).map(|page| page.has(key)).unwrap_or(false);
                for key in INHERITABLE_KEYS {
                    if carries_own(key) {
                        continue;
                    }
                    if let Some(value) = find_inherited_attribute(previous, slot.object_id, key) {
                        pending.push((slot.object_id, key, value.clone()));
                    }
                }
            }
        }

        for (object_id, key, value) in pending {
            self.incremental_mut()
                .opt_clone_object_to_new_document(object_id)
                .map_err(convert_lopdf_error)?;
            self.incremental_mut()
                .new_document
                .get_dictionary_mut(object_id)
                .map_err(convert_lopdf_error)?
                .set(key.to_vec(), value);
        }
        Ok(())
    }

    /// Replace the root `/Pages` node with a flat list of every page, in order.
    ///
    /// Flattening rather than editing a nested tree in place is deliberate:
    /// rebalancing intermediate nodes and fixing their `/Count` up the chain for
    /// every insertion is far more machinery than one array write, and the
    /// intermediate nodes are not deleted — an incremental update never removes
    /// bytes, so they remain in the file, merely unreferenced.
    fn rewrite_page_tree(&mut self) -> Result<()> {
        let object_ids = self.pages.collect_object_ids();
        let root_pages_id = self.root_pages_id();

        for object_id in &object_ids {
            self.incremental_mut()
                .opt_clone_object_to_new_document(*object_id)
                .map_err(convert_lopdf_error)?;
            self.incremental_mut()
                .new_document
                .get_dictionary_mut(*object_id)
                .map_err(convert_lopdf_error)?
                .set("Parent", Object::Reference(root_pages_id));
        }

        self.incremental_mut()
            .opt_clone_object_to_new_document(root_pages_id)
            .map_err(convert_lopdf_error)?;
        let kids: Vec<Object> = object_ids.iter().map(|object_id| Object::Reference(*object_id)).collect();
        let count = i64::try_from(kids.len()).unwrap_or(i64::MAX);
        let pages = self
            .incremental_mut()
            .new_document
            .get_dictionary_mut(root_pages_id)
            .map_err(convert_lopdf_error)?;
        pages.set("Kids", Object::Array(kids));
        pages.set("Count", Object::Integer(count));
        Ok(())
    }

    /// Set `/Rotate` on every page whose rotation was changed in memory.
    fn write_rotation_overrides(&mut self) -> Result<()> {
        let overrides: Vec<(ObjectId, i64)> = self
            .pages
            .list_slots()
            .iter()
            .filter(|slot| slot.rotation_changed)
            .map(|slot| (slot.object_id, i64::from(slot.rotation.degrees())))
            .collect();

        for (object_id, degrees) in overrides {
            self.incremental_mut()
                .opt_clone_object_to_new_document(object_id)
                .map_err(convert_lopdf_error)?;
            self.incremental_mut()
                .new_document
                .get_dictionary_mut(object_id)
                .map_err(convert_lopdf_error)?
                .set("Rotate", Object::Integer(degrees));
        }
        Ok(())
    }

    //---------------------------------------------------------------------
    // Compaction
    //---------------------------------------------------------------------

    /// Merge the previous revision and the appended objects into one document.
    ///
    /// Objects the page tree no longer reaches are dropped and the survivors are
    /// renumbered, so the result is a fresh file with no revision history. This
    /// is lossy by design: anything unreferenced that a reader might still have
    /// wanted — a superseded annotation, a stale outline — is gone. That is why
    /// compaction is only ever invoked on an explicit user request.
    fn build_compacted_document(&self) -> lopdf::Document {
        let mut compacted = self.incremental().get_prev_documents().clone();
        for (object_id, object) in &self.incremental().new_document.objects {
            compacted.objects.insert(*object_id, object.clone());
        }
        compacted.max_id = self.incremental().new_document.max_id;
        //--- a rewritten file has no earlier revision, so the trailer must not claim one ---
        compacted.trailer.remove(b"Prev");
        compacted.prune_objects();
        compacted.renumber_objects();
        compacted
    }
}

impl DocumentIo for PdfDocument {
    //---------------------------------------------------------------------
    // Reading and writing files
    //---------------------------------------------------------------------

    fn open(path: &Path) -> Result<Self> {
        let incremental = IncrementalDocument::load(path).map_err(convert_lopdf_error)?;
        Self::build_from_incremental(incremental)
    }

    /// Write an incremental update appended to the bytes this document was opened from.
    ///
    /// When nothing has changed, the original bytes are reproduced exactly: an
    /// incremental update with no changed objects is no update at all, and
    /// appending an empty revision would grow the file for no reason. When
    /// something has changed, the output is the original bytes verbatim
    /// followed by one appended revision holding the changed objects and a new
    /// cross-reference section.
    ///
    /// Returns [`Error::Unsupported`] for a document with no pages.
    fn save_incremental(&mut self, path: &Path) -> Result<()> {
        if self.page_count() == 0 {
            return Err(Error::Unsupported("cannot save a document with no pages".to_string()));
        }

        if self.dirty.is_clean() {
            std::fs::write(path, self.incremental().get_prev_documents_bytes())?;
            return Ok(());
        }

        self.materialize_changes()?;

        //--- serialising the cross-reference stream consumes an object id and rewrites the trailer in place; restore both so a second save produces the same bytes ---
        let restored_max_id = self.incremental().new_document.max_id;
        let restored_trailer = self.incremental().new_document.trailer.clone();
        let outcome = self.incremental_mut().save(path);
        self.incremental_mut().new_document.max_id = restored_max_id;
        self.incremental_mut().new_document.trailer = restored_trailer;

        //--- `IncrementalDocument::save` reports through `std::io::Error`, not `lopdf::Error` ---
        outcome.map(|_| ()).map_err(Error::from)
    }

    /// Write a freshly serialized document, discarding unreferenced objects.
    ///
    /// Slower than [`DocumentIo::save_incremental`] and lossy with respect to
    /// structure nothing in the page tree references, so it is only ever invoked
    /// on an explicit user request — never as a default or an automatic fallback.
    ///
    /// This is the compaction the trash model names: pruning drops every removed
    /// page's objects from the written file, so the in-memory trash is purged to
    /// match and [`opdf_core::Document::restore_page`] reports
    /// [`Error::PageNotFound`] for those pages afterwards. The purge runs only
    /// once the file is written, so a failed compaction leaves the document
    /// exactly as it was.
    ///
    /// Compaction is also where the document changes what it is an update *to*:
    /// the bytes just written become its base, so a later
    /// [`DocumentIo::save_incremental`] appends to the compacted file. Were the
    /// original bytes kept, that next save would re-emit them as its prefix and
    /// hand back every object the compaction had just pruned — the removed
    /// pages included, with no API left to reach them.
    ///
    /// Returns [`Error::Unsupported`] for a document with no pages.
    fn save_compacted(&mut self, path: &Path) -> Result<()> {
        if self.page_count() == 0 {
            return Err(Error::Unsupported("cannot save a document with no pages".to_string()));
        }
        self.materialize_changes()?;

        //--- serialise once and reuse the bytes, so the document's new base is exactly what the file holds ---
        let mut bytes = Vec::new();
        //--- `Document::save_to` reports through `std::io::Error`, not `lopdf::Error` ---
        self.build_compacted_document().save_to(&mut bytes).map_err(Error::from)?;
        std::fs::write(path, &bytes)?;

        self.rebase_onto(&bytes)?;
        //--- the written file no longer contains the removed pages' objects, so the trash must stop claiming they are restorable ---
        self.pages.purge_trash();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use opdf_core::{PageSize, Rotation};

    fn read_appended_page(document: &PdfDocument, object_id: lopdf::ObjectId) -> &lopdf::Dictionary {
        document
            .incremental()
            .new_document
            .get_dictionary(object_id)
            .expect("the appended revision must carry this page")
    }

    #[test]
    fn writes_a_flat_kids_array_in_slot_order() {
        let mut document = PdfDocument::load_from_bytes(&fixture::build_nested_page_tree()).unwrap();
        let ids = document.page_ids();
        document.move_page(ids[0], 2).unwrap();
        document.materialize_changes().unwrap();

        let root_id = document.root_pages_id();
        let pages = document.incremental().new_document.get_dictionary(root_id).expect("the root must be rewritten");
        let kids = pages.get(b"Kids").and_then(lopdf::Object::as_array).expect("the root must carry kids");
        assert_eq!(kids.len(), 3, "the rewritten tree is flat: one kid per page");
        assert_eq!(pages.get(b"Count").and_then(lopdf::Object::as_i64).unwrap_or(0), 3);

        let written: Vec<lopdf::ObjectId> = kids.iter().filter_map(|kid| kid.as_reference().ok()).collect();
        let expected: Vec<lopdf::ObjectId> = document
            .page_ids()
            .into_iter()
            .map(|id| document.pages.find_slot(id).unwrap().object_id)
            .collect();
        assert_eq!(written, expected, "the kids array must follow slot order exactly");
    }

    #[test]
    fn reparents_every_page_to_the_root_node() {
        let mut document = PdfDocument::load_from_bytes(&fixture::build_nested_page_tree()).unwrap();
        let ids = document.page_ids();
        document.move_page(ids[2], 0).unwrap();
        document.materialize_changes().unwrap();

        let root_id = document.root_pages_id();
        for id in document.page_ids() {
            let object_id = document.pages.find_slot(id).unwrap().object_id;
            let parent = read_appended_page(&document, object_id)
                .get(b"Parent")
                .and_then(lopdf::Object::as_reference)
                .expect("a flattened page must name the root");
            assert_eq!(parent, root_id);
        }
    }

    #[test]
    fn materialises_inherited_geometry_before_flattening_drops_the_ancestor() {
        let mut document = PdfDocument::load_from_bytes(&fixture::build_inherited_geometry()).unwrap();
        let ids = document.page_ids();
        document.move_page(ids[0], 1).unwrap();
        document.materialize_changes().unwrap();

        for id in document.page_ids() {
            let object_id = document.pages.find_slot(id).unwrap().object_id;
            let page = read_appended_page(&document, object_id);
            assert!(page.has(b"MediaBox"), "flattening must not drop an inherited media box");
            assert_eq!(page.get(b"Rotate").and_then(lopdf::Object::as_i64).unwrap_or(0), 90);
        }
    }

    #[test]
    fn writes_an_explicit_rotation_over_an_inherited_one() {
        let mut document = PdfDocument::load_from_bytes(&fixture::build_inherited_geometry()).unwrap();
        let ids = document.page_ids();
        document.move_page(ids[0], 1).unwrap();
        document.set_rotation(ids[0], Rotation::Half).unwrap();
        document.materialize_changes().unwrap();

        let object_id = document.pages.find_slot(ids[0]).unwrap().object_id;
        let rotate = read_appended_page(&document, object_id)
            .get(b"Rotate")
            .and_then(lopdf::Object::as_i64)
            .unwrap_or(0);
        assert_eq!(rotate, 180, "an explicit rotation must win over the value materialised from the ancestor");
    }

    #[test]
    fn leaves_the_page_tree_alone_when_only_a_rotation_changed() {
        let mut document = PdfDocument::load_from_bytes(&fixture::build_flat_pages(&[PageSize::A4; 2])).unwrap();
        let ids = document.page_ids();
        document.set_rotation(ids[1], Rotation::Quarter).unwrap();
        document.materialize_changes().unwrap();

        let root_id = document.root_pages_id();
        assert!(
            !document.incremental().new_document.has_object(root_id),
            "a rotation must not force the page tree to be rewritten"
        );
        let object_id = document.pages.find_slot(ids[1]).unwrap().object_id;
        assert_eq!(
            read_appended_page(&document, object_id)
                .get(b"Rotate")
                .and_then(lopdf::Object::as_i64)
                .unwrap_or(0),
            90
        );
    }

    use std::path::PathBuf;

    fn write_fixture(directory: &Path, file_name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.join(file_name);
        std::fs::write(&path, bytes).expect("a fixture must be writable");
        path
    }

    #[test]
    fn opens_a_file_from_disk() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let path = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 3]));
        let document = PdfDocument::open(&path).unwrap();
        assert_eq!(document.page_count(), 3);
    }

    #[test]
    fn reports_a_missing_file_as_an_io_error() {
        let outcome = PdfDocument::open(Path::new("/nonexistent/definitely-not-here.pdf"));
        assert!(
            matches!(outcome, Err(opdf_core::Error::Io(_))),
            "a missing file is an io error, got: {outcome:?}"
        );
    }

    #[test]
    fn saving_an_unedited_document_reproduces_the_file_byte_for_byte() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let original = fixture::build_flat_pages(&[PageSize::A4, PageSize::LETTER, PageSize::new(200.0, 400.0)]);
        let source = write_fixture(directory.path(), "flat.pdf", &original);
        let destination = directory.path().join("flat-saved.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        document.save_incremental(&destination).unwrap();

        assert_eq!(
            std::fs::read(&destination).unwrap(),
            original,
            "opening and saving without editing must produce the identical file"
        );
    }

    #[test]
    fn saving_after_an_edit_appends_to_the_original_bytes() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let original = fixture::build_flat_pages(&[PageSize::A4; 3]);
        let source = write_fixture(directory.path(), "flat.pdf", &original);
        let destination = directory.path().join("flat-edited.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_incremental(&destination).unwrap();

        let saved = std::fs::read(&destination).unwrap();
        assert!(
            saved.starts_with(&original),
            "an incremental save must append to the original bytes, never rewrite them"
        );
        assert!(saved.len() > original.len(), "an edit must actually append something");
    }

    #[test]
    fn saving_after_an_edit_destroys_nothing_that_was_there() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let original = fixture::build_flat_pages(&[PageSize::A4; 3]);
        let source = write_fixture(directory.path(), "flat.pdf", &original);
        let destination = directory.path().join("flat-edited.pdf");

        let object_count_before = lopdf::Document::load_mem(&original).unwrap().objects.len();

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_incremental(&destination).unwrap();

        let reloaded = lopdf::Document::load(&destination).unwrap();
        assert!(
            reloaded.objects.len() >= object_count_before,
            "a removed page's objects survive the save: removal unlinks them from the page tree, it never deletes them"
        );
    }

    #[test]
    fn a_page_removed_and_restored_before_saving_is_written_back_intact() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let sizes = [PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0), PageSize::new(300.0, 300.0)];
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&sizes));
        let destination = directory.path().join("restored.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.set_rotation(ids[1], Rotation::Half).unwrap();
        document.remove_page(ids[1]).unwrap();
        document.restore_page(ids[1], 2).unwrap();
        document.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let geometry: Vec<(f32, Rotation)> = reopened
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = reopened.page(id).unwrap();
                (info.size.width_pt, info.rotation)
            })
            .collect();
        assert_eq!(
            geometry,
            vec![(100.0, Rotation::None), (300.0, Rotation::None), (200.0, Rotation::Half)],
            "a restored page must be written back at its restored position with the rotation it carried into the trash"
        );
    }

    #[test]
    fn a_restored_page_keeps_the_content_stream_it_always_had() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 3]));
        let destination = directory.path().join("restored.pdf");

        //--- capture the content the middle page renders from, before anything touches it ---
        let original = lopdf::Document::load(&source).unwrap();
        let original_page = original.page_iter().nth(1).expect("the fixture has three pages");
        let content_before = original.get_page_content(original_page);
        assert!(!content_before.is_empty(), "the fixture's pages carry content, or this test proves nothing");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.restore_page(ids[1], 1).unwrap();
        document.save_incremental(&destination).unwrap();

        let reopened = lopdf::Document::load(&destination).unwrap();
        let reopened_page = reopened.page_iter().nth(1).expect("the saved file has three pages");
        assert_eq!(
            reopened.get_page_content(reopened_page),
            content_before,
            "a restored page must render from the same bytes it always did: this is the content half of the trash contract, and VecDocument cannot prove it"
        );
    }

    #[test]
    fn the_trash_does_not_survive_a_save_and_reopen() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 3]));
        let destination = directory.path().join("trimmed.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_incremental(&destination).unwrap();

        //--- the removed page's bytes are still in the file, but its identity is not: a PageId is a within-session concept ---
        let mut reopened = PdfDocument::open(&destination).unwrap();
        assert_eq!(reopened.page_count(), 2);

        //--- the reopened document's trash is empty, so no identity at all is restorable ---
        let never_held = opdf_core::PageId::new(u64::MAX);
        let rejected = reopened.restore_page(never_held, 0);
        assert!(
            matches!(rejected, Err(opdf_core::Error::PageNotFound(_))),
            "a trash keyed on PageId cannot outlive the session that allocated the ids, got: {rejected:?}"
        );

        //--- and it is worse than merely absent. Allocation restarts at zero on every open, so the
        //--- removed page's id is now a live page — a different one. Replaying a deletion's inverse
        //--- across a save is not just unavailable, it is misdirected, which is why an undo stack
        //--- must never outlive a save.
        let rejected = reopened.restore_page(ids[1], 0);
        assert!(
            matches!(rejected, Err(opdf_core::Error::Unsupported(_))),
            "the removed page's identity now names a different, live page, got: {rejected:?}"
        );
        assert_eq!(reopened.page_count(), 2, "no rejected restore may add a page");
    }

    #[test]
    fn a_saved_edit_reopens_with_the_edited_structure() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let sizes = [PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0), PageSize::new(300.0, 300.0)];
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&sizes));
        let destination = directory.path().join("flat-edited.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.move_page(ids[0], 2).unwrap();
        document.set_rotation(ids[2], Rotation::Quarter).unwrap();
        document.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let geometry: Vec<(f32, Rotation)> = reopened
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = reopened.page(id).unwrap();
                (info.size.width_pt, info.rotation)
            })
            .collect();
        assert_eq!(
            geometry,
            vec![(200.0, Rotation::None), (300.0, Rotation::Quarter), (100.0, Rotation::None)],
            "the reopened document must carry the edited order and rotation"
        );
    }

    #[test]
    fn a_nested_tree_survives_flattening_with_its_inherited_geometry() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "inherited.pdf", &fixture::build_inherited_geometry());
        let destination = directory.path().join("inherited-edited.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.move_page(ids[0], 1).unwrap();
        document.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        assert_eq!(reopened.page_count(), 2);
        for id in reopened.page_ids() {
            let info = reopened.page(id).unwrap();
            assert_eq!(info.size, PageSize::A4, "inherited geometry must survive flattening");
            assert_eq!(info.rotation, Rotation::Quarter);
        }
    }

    #[test]
    fn saving_twice_produces_identical_files() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 2]));

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.set_rotation(ids[0], Rotation::Quarter).unwrap();

        let first = directory.path().join("first.pdf");
        let second = directory.path().join("second.pdf");
        document.save_incremental(&first).unwrap();
        document.save_incremental(&second).unwrap();

        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap(),
            "saving the same state twice must produce the same bytes"
        );
    }

    #[test]
    fn refuses_to_save_a_document_with_no_pages() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4]));
        let destination = directory.path().join("empty.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[0]).unwrap();

        assert!(
            matches!(document.save_incremental(&destination), Err(opdf_core::Error::Unsupported(_))),
            "a pdf with no pages is not a pdf worth writing"
        );
        assert!(!destination.exists(), "a refused save must not leave a file behind");
    }

    #[test]
    fn a_compacted_save_reopens_with_the_edited_structure() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let sizes = [PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0), PageSize::new(300.0, 300.0)];
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&sizes));
        let destination = directory.path().join("compacted.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_compacted(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let widths: Vec<f32> = reopened.page_ids().into_iter().map(|id| reopened.page(id).unwrap().size.width_pt).collect();
        assert_eq!(widths, vec![100.0, 300.0], "compaction must preserve the surviving pages in order");
    }

    #[test]
    fn a_compacted_save_drops_what_nothing_references() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 3]));
        let incremental_path = directory.path().join("incremental.pdf");
        let compacted_path = directory.path().join("compacted.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_incremental(&incremental_path).unwrap();
        document.save_compacted(&compacted_path).unwrap();

        let incremental_size = std::fs::metadata(&incremental_path).unwrap().len();
        let compacted_size = std::fs::metadata(&compacted_path).unwrap().len();
        assert!(
            compacted_size < incremental_size,
            "compaction discards the removed page and the superseded revision: {compacted_size} must be under {incremental_size}"
        );

        let reopened = lopdf::Document::load(&compacted_path).unwrap();
        assert!(reopened.trailer.get(b"Prev").is_err(), "a rewritten file has no previous revision to point at");
    }

    #[test]
    fn a_compacted_save_of_an_unedited_document_keeps_every_page() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "nested.pdf", &fixture::build_nested_page_tree());
        let destination = directory.path().join("compacted.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        document.save_compacted(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        assert_eq!(reopened.page_count(), 3);
    }

    #[test]
    fn a_compacting_save_purges_the_trash() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 3]));
        let destination = directory.path().join("compacted.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_compacted(&destination).unwrap();

        let rejected = document.restore_page(ids[1], 0);
        assert!(
            matches!(rejected, Err(opdf_core::Error::PageNotFound(_))),
            "compaction discarded the page's objects, so the trash must not claim it is still restorable, got: {rejected:?}"
        );
        assert_eq!(document.page_count(), 2, "a purge must not disturb the live pages");
    }

    #[test]
    fn a_failed_compaction_leaves_the_trash_intact() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4; 3]));

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();

        //--- a directory that does not exist cannot be written into ---
        assert!(document.save_compacted(&directory.path().join("no/such/dir/out.pdf")).is_err());
        document
            .restore_page(ids[1], 0)
            .expect("a compaction that never wrote a file must not have destroyed the trash");
    }

    #[test]
    fn a_save_after_compacting_reproduces_the_compacted_file_byte_for_byte() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let sizes = [PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0), PageSize::new(300.0, 300.0)];
        let original = fixture::build_flat_pages(&sizes);
        let source = write_fixture(directory.path(), "flat.pdf", &original);
        let compacted_path = directory.path().join("compacted.pdf");
        let resaved_path = directory.path().join("resaved.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_compacted(&compacted_path).unwrap();
        document.save_incremental(&resaved_path).unwrap();

        let compacted = std::fs::read(&compacted_path).unwrap();
        let resaved = std::fs::read(&resaved_path).unwrap();
        assert!(
            !resaved.starts_with(&original),
            "a save after compacting must not re-emit the original file as its prefix: the compacted bytes are the document's base now"
        );
        assert_eq!(
            resaved, compacted,
            "compacting rebases the document onto what it wrote, so an unedited save afterwards reproduces that file exactly"
        );
    }

    #[test]
    fn compacting_does_not_leave_a_removed_page_recoverable_from_the_next_save() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let sizes = [PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0), PageSize::new(300.0, 300.0)];
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&sizes));
        let compacted_path = directory.path().join("compacted.pdf");
        let resaved_path = directory.path().join("resaved.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_compacted(&compacted_path).unwrap();
        document.save_incremental(&resaved_path).unwrap();

        //--- the purged page's geometry is the marker: if its dictionary came back, its media box came with it ---
        let reopened = PdfDocument::open(&resaved_path).unwrap();
        let widths: Vec<f32> = reopened.page_ids().into_iter().map(|id| reopened.page(id).unwrap().size.width_pt).collect();
        assert_eq!(widths, vec![100.0, 300.0], "the saved file must hold exactly the surviving pages");

        let objects = lopdf::Document::load(&resaved_path).unwrap();
        let purged_page_survives = objects.objects.values().any(|object| {
            object
                .as_dict()
                .ok()
                .and_then(|page| page.get(b"MediaBox").ok())
                .and_then(|media_box| media_box.as_array().ok())
                .and_then(|corners| corners.get(2))
                .and_then(|corner| corner.as_float().ok())
                .map(|width| (width - 200.0).abs() < 0.5)
                .unwrap_or(false)
        });
        assert!(
            !purged_page_survives,
            "compaction pruned the removed page's dictionary, so no later save may write it back into the file"
        );
    }

    #[test]
    fn an_edit_after_compacting_appends_to_the_compacted_bytes() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let original = fixture::build_flat_pages(&[PageSize::A4; 3]);
        let source = write_fixture(directory.path(), "flat.pdf", &original);
        let compacted_path = directory.path().join("compacted.pdf");
        let edited_path = directory.path().join("edited.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[1]).unwrap();
        document.save_compacted(&compacted_path).unwrap();

        document.set_rotation(ids[2], Rotation::Quarter).unwrap();
        document.save_incremental(&edited_path).unwrap();

        let compacted = std::fs::read(&compacted_path).unwrap();
        let edited = std::fs::read(&edited_path).unwrap();
        assert!(
            edited.starts_with(&compacted),
            "an incremental save after compacting must append to the compacted file, which is the document's base now"
        );
        assert!(!edited.starts_with(&original), "the original file is no longer this document's base");

        //--- the appended revision has to resolve against the compacted base, cross-reference offsets included ---
        let reopened = PdfDocument::open(&edited_path).unwrap();
        let rotations: Vec<Rotation> = reopened.page_ids().into_iter().map(|id| reopened.page(id).unwrap().rotation).collect();
        assert_eq!(
            rotations,
            vec![Rotation::None, Rotation::Quarter],
            "the edit made after compacting must survive the save"
        );
    }

    #[test]
    fn an_edit_after_compacting_a_nested_tree_lands_on_the_page_it_names() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "nested.pdf", &fixture::build_nested_page_tree());
        let compacted_path = directory.path().join("compacted.pdf");
        let edited_path = directory.path().join("edited.pdf");

        //--- compacting an unedited nested tree renumbers every object without flattening it, so a mis-mapped slot would rotate the wrong page ---
        let mut document = PdfDocument::open(&source).unwrap();
        document.save_compacted(&compacted_path).unwrap();

        //--- the first page, not the middle one: a mapping that merely reversed would still hit the middle ---
        let ids = document.page_ids();
        document.set_rotation(ids[0], Rotation::Half).unwrap();
        document.save_incremental(&edited_path).unwrap();

        let reopened = PdfDocument::open(&edited_path).unwrap();
        let geometry: Vec<(f32, Rotation)> = reopened
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = reopened.page(id).unwrap();
                (info.size.width_pt, info.rotation)
            })
            .collect();
        assert_eq!(
            geometry,
            vec![(100.0, Rotation::Half), (200.0, Rotation::None), (300.0, Rotation::None)],
            "after a compacting save each page identity must still name the object it always did"
        );
    }

    #[test]
    fn saving_after_inserting_a_page_reopens_with_it() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::new(100.0, 100.0); 2]));
        let destination = directory.path().join("inserted.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        document.insert_page(1, PageSize::new(300.0, 400.0)).unwrap();
        document.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let sizes: Vec<(f32, f32)> = reopened
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = reopened.page(id).unwrap();
                (info.size.width_pt, info.size.height_pt)
            })
            .collect();
        assert_eq!(
            sizes,
            vec![(100.0, 100.0), (300.0, 400.0), (100.0, 100.0)],
            "a page created this session must be written out and read back at its position"
        );
    }

    #[test]
    fn saving_after_importing_pages_reopens_with_their_content() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source_path = write_fixture(directory.path(), "source.pdf", &fixture::build_flat_pages(&[PageSize::new(200.0, 200.0)]));
        let target_path = write_fixture(directory.path(), "target.pdf", &fixture::build_flat_pages(&[PageSize::new(100.0, 100.0)]));
        let destination = directory.path().join("imported.pdf");

        let source = PdfDocument::open(&source_path).unwrap();
        let mut target = PdfDocument::open(&target_path).unwrap();
        target.import_pages(&source, &source.page_ids(), 0).unwrap();
        target.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let widths: Vec<f32> = reopened.page_ids().into_iter().map(|id| reopened.page(id).unwrap().size.width_pt).collect();
        assert_eq!(
            widths,
            vec![200.0, 100.0],
            "an imported page must be written out at the position it was imported into"
        );

        let raw = lopdf::Document::load(&destination).unwrap();
        let imported_page = raw.page_iter().next().expect("the saved file has pages");
        assert!(
            !raw.get_page_content(imported_page).is_empty(),
            "an imported page's content stream must reach the file, or the page reopens blank"
        );
    }

    #[test]
    fn refuses_to_compact_a_document_with_no_pages() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source = write_fixture(directory.path(), "flat.pdf", &fixture::build_flat_pages(&[PageSize::A4]));
        let destination = directory.path().join("compacted.pdf");

        let mut document = PdfDocument::open(&source).unwrap();
        let ids = document.page_ids();
        document.remove_page(ids[0]).unwrap();
        assert!(matches!(document.save_compacted(&destination), Err(opdf_core::Error::Unsupported(_))));
    }

    //---------------------------------------------------------------------
    // Documents created from nothing
    //---------------------------------------------------------------------

    #[test]
    fn refuses_to_save_a_created_document_before_it_has_a_page() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let mut document = PdfDocument::empty().unwrap();

        let appended = directory.path().join("appended.pdf");
        let compacted = directory.path().join("compacted.pdf");
        assert!(matches!(document.save_incremental(&appended), Err(opdf_core::Error::Unsupported(_))));
        assert!(matches!(document.save_compacted(&compacted), Err(opdf_core::Error::Unsupported(_))));
        assert!(!appended.exists() && !compacted.exists(), "a refused save must not leave a file behind");
    }

    #[test]
    fn a_created_document_saves_and_reopens_with_the_pages_it_was_given() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let destination = directory.path().join("created.pdf");

        let mut document = PdfDocument::empty().unwrap();
        document.insert_page(0, PageSize::new(100.0, 200.0)).unwrap();
        document.insert_page(1, PageSize::new(300.0, 400.0)).unwrap();
        document.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let sizes: Vec<(f32, f32)> = reopened
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = reopened.page(id).unwrap();
                (info.size.width_pt, info.size.height_pt)
            })
            .collect();
        assert_eq!(
            sizes,
            vec![(100.0, 200.0), (300.0, 400.0)],
            "a document created from nothing must reopen with exactly the pages it was given"
        );
    }

    #[test]
    fn a_created_document_reopens_ready_to_be_edited_further() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let first_path = directory.path().join("created.pdf");
        let second_path = directory.path().join("extended.pdf");

        let mut document = PdfDocument::empty().unwrap();
        document.insert_page(0, PageSize::new(100.0, 100.0)).unwrap();
        document.save_incremental(&first_path).unwrap();

        //--- the file a created document writes must be a normal document to everything downstream ---
        let mut reopened = PdfDocument::open(&first_path).unwrap();
        reopened.insert_page(1, PageSize::new(200.0, 200.0)).unwrap();
        reopened.set_rotation(reopened.page_ids()[0], Rotation::Quarter).unwrap();
        reopened.save_incremental(&second_path).unwrap();

        let extended = PdfDocument::open(&second_path).unwrap();
        let geometry: Vec<(f32, Rotation)> = extended
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = extended.page(id).unwrap();
                (info.size.width_pt, info.rotation)
            })
            .collect();
        assert_eq!(geometry, vec![(100.0, Rotation::Quarter), (200.0, Rotation::None)]);
    }

    #[test]
    fn a_created_document_carries_imported_pages_into_the_file() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let source_path = write_fixture(
            directory.path(),
            "source.pdf",
            &fixture::build_flat_pages(&[PageSize::new(100.0, 100.0), PageSize::new(200.0, 200.0), PageSize::new(300.0, 300.0)]),
        );
        let destination = directory.path().join("extracted.pdf");

        //--- this is the shape `opdf-ops` extract and split need: an empty target filled by import ---
        let source = PdfDocument::open(&source_path).unwrap();
        let mut extracted = PdfDocument::empty().unwrap();
        let wanted: Vec<opdf_core::PageId> = source.page_ids()[..2].to_vec();
        extracted.import_pages(&source, &wanted, 0).unwrap();
        extracted.save_incremental(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        let widths: Vec<f32> = reopened.page_ids().into_iter().map(|id| reopened.page(id).unwrap().size.width_pt).collect();
        assert_eq!(widths, vec![100.0, 200.0], "an extraction must reopen holding exactly the pages it took");

        let raw = lopdf::Document::load(&destination).unwrap();
        for object_id in raw.page_iter() {
            assert!(
                !raw.get_page_content(object_id).is_empty(),
                "every imported page must bring its content stream into the new file"
            );
        }
    }

    #[test]
    fn a_created_document_compacts_to_a_file_with_no_earlier_revision() {
        let directory = tempfile::tempdir().expect("a temporary directory must be creatable");
        let destination = directory.path().join("created.pdf");

        let mut document = PdfDocument::empty().unwrap();
        document.insert_page(0, PageSize::A4).unwrap();
        document.save_compacted(&destination).unwrap();

        let reopened = PdfDocument::open(&destination).unwrap();
        assert_eq!(reopened.page_count(), 1);
        let raw = lopdf::Document::load(&destination).unwrap();
        assert!(raw.trailer.get(b"Prev").is_err(), "a compacted file starts a fresh history");
    }
}

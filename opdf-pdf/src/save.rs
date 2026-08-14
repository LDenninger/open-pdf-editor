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

    /// Not implemented until Task 12.
    fn save_compacted(&mut self, path: &Path) -> Result<()> {
        Err(Error::Unsupported(format!("save_compacted({}) is not implemented yet", path.display())))
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
}

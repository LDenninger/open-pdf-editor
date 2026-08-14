//! Writing in-memory page state back into PDF objects, and the `DocumentIo` contract.

// Materialisation gains its first non-test caller when `save_incremental` lands
// later in the track. Until then `-D warnings` would reject it as dead code.
#![allow(dead_code)]

use lopdf::{Object, ObjectId};
use opdf_core::Result;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use opdf_core::{Document as _, PageSize, Rotation};

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
}

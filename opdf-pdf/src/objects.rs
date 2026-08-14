//! Reading and copying `lopdf` objects across a merged previous/appended view.

// The first non-test caller is `PdfDocument`, which lands later in the track.
// Until then `-D warnings` would reject this module as dead code.
#![allow(dead_code)]

use lopdf::{Object, ObjectId};

/// The four attributes a PDF page may inherit from its ancestors in the page
/// tree, per ISO 32000-1 section 7.7.3.4.
pub(crate) const INHERITABLE_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Cap on the `/Parent` chain, so a cyclic tree terminates instead of hanging.
const PARENT_CHAIN_LIMIT: usize = 64;

/// Cap on a chain of indirect references, so a reference cycle terminates.
const REFERENCE_CHAIN_LIMIT: usize = 32;

//---------------------------------------------------------------------
// The merged object view
//---------------------------------------------------------------------

/// A read-only view of a PDF object graph, addressed by object identifier.
///
/// This exists so that copying works identically against a plain
/// `lopdf::Document` and against a document whose objects are split between a
/// previous revision and an appended one. Lookups are **raw**: a reference is
/// returned as a reference, not followed. Use [`resolve_reference`] to follow it.
pub(crate) trait ObjectSource {
    /// The object stored under this identifier, if any.
    fn find_object(&self, object_id: ObjectId) -> Option<&Object>;
}

impl ObjectSource for lopdf::Document {
    fn find_object(&self, object_id: ObjectId) -> Option<&Object> {
        self.objects.get(&object_id)
    }
}

//---------------------------------------------------------------------
// Walking the object graph
//---------------------------------------------------------------------

/// Follow a chain of indirect references to the value it ultimately names.
///
/// Returns the object unchanged if it is not a reference, and `None` if the
/// chain is broken or longer than [`REFERENCE_CHAIN_LIMIT`].
pub(crate) fn resolve_reference<'a, S: ObjectSource + ?Sized>(source: &'a S, object: &'a Object) -> Option<&'a Object> {
    let mut current = object;
    for _ in 0..REFERENCE_CHAIN_LIMIT {
        match current.as_reference() {
            Ok(object_id) => current = source.find_object(object_id)?,
            Err(_) => return Some(current),
        }
    }
    None
}

/// Find an attribute on a page or on the nearest ancestor that carries it.
///
/// The walk starts at the page itself, so a page's own value always wins over
/// an inherited one. The returned value may still be an indirect reference.
pub(crate) fn find_inherited_attribute<'a, S: ObjectSource + ?Sized>(source: &'a S, page_object_id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut current = page_object_id;
    for _ in 0..PARENT_CHAIN_LIMIT {
        let dictionary = source.find_object(current)?.as_dict().ok()?;
        if let Ok(value) = dictionary.get(key) {
            return Some(value);
        }
        current = dictionary.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

/// Whether an object is a `/Page` or `/Pages` node.
///
/// Copying stops at these: an annotation's destination may reference another
/// page, and following it would drag the whole document into a single import.
pub(crate) fn is_page_node<S: ObjectSource + ?Sized>(source: &S, object_id: ObjectId) -> bool {
    let Some(object) = source.find_object(object_id) else {
        return false;
    };
    let Ok(dictionary) = object.as_dict() else {
        return false;
    };
    matches!(dictionary.get_type(), Ok(type_name) if type_name == b"Page".as_slice() || type_name == b"Pages".as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    fn load(bytes: &[u8]) -> lopdf::Document {
        lopdf::Document::load_mem(bytes).expect("fixture must parse")
    }

    #[test]
    fn finds_an_attribute_carried_by_the_page_itself() {
        let document = load(&fixture::build_flat_pages(&[opdf_core::PageSize::A4]));
        let page_id = document.page_iter().next().expect("the fixture has one page");
        let media_box = find_inherited_attribute(&document, page_id, b"MediaBox").expect("the page carries a media box");
        assert!(media_box.as_array().is_ok());
    }

    #[test]
    fn finds_an_attribute_carried_by_an_ancestor() {
        let document = load(&fixture::build_inherited_geometry());
        for page_id in document.page_iter() {
            let rotate = find_inherited_attribute(&document, page_id, b"Rotate").expect("the root node carries a rotation");
            assert_eq!(rotate.as_i64().unwrap_or(0), 90);
        }
    }

    #[test]
    fn reports_an_absent_attribute_as_missing() {
        let document = load(&fixture::build_missing_media_box());
        let page_id = document.page_iter().next().expect("the fixture has one page");
        assert!(find_inherited_attribute(&document, page_id, b"MediaBox").is_none());
    }

    #[test]
    fn resolves_an_indirect_value_to_the_object_it_names() {
        let document = load(&fixture::build_indirect_media_box());
        let page_id = document.page_iter().next().expect("the fixture has one page");
        let value = find_inherited_attribute(&document, page_id, b"MediaBox").expect("the page names a media box");
        assert!(value.as_reference().is_ok(), "the fixture stores the media box indirectly");
        let resolved = resolve_reference(&document, value).expect("the reference must resolve");
        assert_eq!(resolved.as_array().map(Vec::len).unwrap_or(0), 4);
    }

    #[test]
    fn recognises_page_and_pages_nodes() {
        let document = load(&fixture::build_nested_page_tree());
        let page_id = document.page_iter().next().expect("the fixture has pages");
        assert!(is_page_node(&document, page_id), "a /Page dictionary is a page node");

        let root_id = document
            .catalog()
            .and_then(|catalog| catalog.get(b"Pages"))
            .and_then(lopdf::Object::as_reference)
            .expect("the fixture has a page tree root");
        assert!(is_page_node(&document, root_id), "a /Pages dictionary is a page node");

        let content_id = document.get_page_contents(page_id).first().copied().expect("the page has content");
        assert!(!is_page_node(&document, content_id), "a content stream is not a page node");
    }

    #[test]
    fn lists_exactly_the_four_inheritable_attributes() {
        assert_eq!(INHERITABLE_KEYS.len(), 4);
        assert!(INHERITABLE_KEYS.contains(&b"Resources".as_slice()));
        assert!(INHERITABLE_KEYS.contains(&b"MediaBox".as_slice()));
        assert!(INHERITABLE_KEYS.contains(&b"CropBox".as_slice()));
        assert!(INHERITABLE_KEYS.contains(&b"Rotate".as_slice()));
    }
}

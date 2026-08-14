//! Reading and copying `lopdf` objects across a merged previous/appended view.

// The first non-test caller is `PdfDocument`, which lands later in the track.
// Until then `-D warnings` would reject this module as dead code.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId};
use opdf_core::PageSize;

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

//---------------------------------------------------------------------
// Reference collection and remapping
//---------------------------------------------------------------------

/// Collect every object reference contained in a value, skipping `/Parent`.
///
/// `/Parent` points up the page tree, which is not part of the page being
/// copied — the caller reparents the copy explicitly.
fn collect_references(object: &Object, found: &mut Vec<ObjectId>) {
    match object {
        Object::Reference(object_id) => found.push(*object_id),
        Object::Array(items) => {
            for item in items {
                collect_references(item, found);
            }
        }
        Object::Dictionary(dictionary) => collect_dictionary_references(dictionary, found),
        Object::Stream(stream) => collect_dictionary_references(&stream.dict, found),
        _ => {}
    }
}

/// Collect every reference a dictionary's values contain, skipping `/Parent`.
fn collect_dictionary_references(dictionary: &Dictionary, found: &mut Vec<ObjectId>) {
    for (key, value) in dictionary.iter() {
        if key.as_slice() == b"Parent" {
            continue;
        }
        collect_references(value, found);
    }
}

/// Rewrite every reference in a value according to a source-to-target mapping.
///
/// References absent from the mapping are left alone: they name objects that
/// were deliberately not copied, and the caller overwrites them.
fn remap_references(object: &mut Object, mapping: &HashMap<ObjectId, ObjectId>) {
    match object {
        Object::Reference(object_id) => {
            if let Some(target_id) = mapping.get(object_id) {
                *object_id = *target_id;
            }
        }
        Object::Array(items) => {
            for item in items {
                remap_references(item, mapping);
            }
        }
        Object::Dictionary(dictionary) => {
            for (_, value) in dictionary.iter_mut() {
                remap_references(value, mapping);
            }
        }
        Object::Stream(stream) => {
            for (_, value) in stream.dict.iter_mut() {
                remap_references(value, mapping);
            }
        }
        _ => {}
    }
}

//---------------------------------------------------------------------
// Page construction and deep copy
//---------------------------------------------------------------------

/// Add an empty page of the given size to a document, returning its object id.
///
/// The page carries an explicit `/MediaBox` and an empty `/Resources`, so it
/// depends on nothing it might inherit. It has no `/Contents`, which is legal
/// and renders as a blank page.
pub(crate) fn build_blank_page(target: &mut Document, parent_id: ObjectId, size: PageSize) -> ObjectId {
    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Parent", Object::Reference(parent_id));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(size.width_pt),
            Object::Real(size.height_pt),
        ]),
    );
    page.set("Resources", Object::Dictionary(Dictionary::new()));
    target.add_object(Object::Dictionary(page))
}

/// Deep-copy one page and everything it reaches into another document.
///
/// Every copied object receives a fresh identifier in the target and every
/// reference between them is rewritten, so the copy shares nothing with the
/// source. The walk stops at other `/Page` and `/Pages` nodes — an annotation
/// destination may name another page, and following it would import the whole
/// document. Inheritable attributes are resolved from the source and written
/// onto the copy, and `/Parent` is removed for the caller to set.
///
/// Returns `None` if the page object is absent or is not a dictionary.
pub(crate) fn copy_page_into<S: ObjectSource + ?Sized>(source: &S, page_object_id: ObjectId, target: &mut Document) -> Option<ObjectId> {
    source.find_object(page_object_id)?.as_dict().ok()?;

    //--- resolve what the page inherits while its ancestors are still reachable ---
    let mut inherited: Vec<(&'static [u8], Object)> = Vec::new();
    for key in INHERITABLE_KEYS {
        if let Some(value) = find_inherited_attribute(source, page_object_id, key) {
            inherited.push((key, value.clone()));
        }
    }

    //--- pass one: give every reachable object a fresh identifier in the target ---
    let mut mapping: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<ObjectId> = vec![page_object_id];
    seen.insert(page_object_id);

    for (_, value) in &inherited {
        let mut referenced = Vec::new();
        collect_references(value, &mut referenced);
        for referenced_id in referenced {
            if seen.insert(referenced_id) && source.find_object(referenced_id).is_some() && !is_page_node(source, referenced_id) {
                pending.push(referenced_id);
            }
        }
    }

    while let Some(current) = pending.pop() {
        let Some(object) = source.find_object(current) else {
            continue;
        };
        mapping.insert(current, target.new_object_id());

        let mut referenced = Vec::new();
        collect_references(object, &mut referenced);
        for referenced_id in referenced {
            if seen.insert(referenced_id) && source.find_object(referenced_id).is_some() && !is_page_node(source, referenced_id) {
                pending.push(referenced_id);
            }
        }
    }

    //--- pass two: clone each object and rewrite its references into the target's numbering ---
    for (source_id, target_id) in &mapping {
        let Some(object) = source.find_object(*source_id) else {
            continue;
        };
        let mut copy = object.clone();
        remap_references(&mut copy, &mapping);
        target.set_object(*target_id, copy);
    }

    //--- the copy must stand alone: no parent, and every inherited attribute written out ---
    let copied_page_id = *mapping.get(&page_object_id)?;
    for (_, value) in &mut inherited {
        remap_references(value, &mapping);
    }
    let page = target.get_object_mut(copied_page_id).ok()?.as_dict_mut().ok()?;
    page.remove(b"Parent");
    for (key, value) in inherited {
        if !page.has(key) {
            page.set(key.to_vec(), value);
        }
    }
    Some(copied_page_id)
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

    #[test]
    fn builds_a_blank_page_carrying_the_requested_size() {
        let mut target = lopdf::Document::with_version("1.5");
        let parent_id = target.new_object_id();
        let page_id = build_blank_page(&mut target, parent_id, opdf_core::PageSize::A4);

        let page = target.get_dictionary(page_id).expect("the blank page must resolve");
        assert_eq!(page.get_type().unwrap_or_default(), b"Page".as_slice());
        let corners = page
            .get(b"MediaBox")
            .and_then(lopdf::Object::as_array)
            .expect("a blank page carries a media box");
        assert_eq!(corners[2].as_float().unwrap_or(0.0), 595.0);
        assert_eq!(corners[3].as_float().unwrap_or(0.0), 842.0);
    }

    #[test]
    fn copies_a_page_and_renumbers_everything_it_reaches() {
        let source = load(&fixture::build_flat_pages(&[opdf_core::PageSize::A4]));
        let source_page = source.page_iter().next().expect("the fixture has one page");
        let source_content = source.get_page_contents(source_page).first().copied().expect("the page has content");

        let mut target = lopdf::Document::with_version("1.5");
        //--- occupy the low object numbers, so a copy that failed to renumber would collide visibly ---
        for _ in 0..8 {
            target.add_object(lopdf::Object::Null);
        }
        let copied = copy_page_into(&source, source_page, &mut target).expect("a valid page must copy");

        let page = target.get_dictionary(copied).expect("the copy must resolve");
        let content_id = page.get(b"Contents").and_then(lopdf::Object::as_reference).expect("the copy keeps its content");
        assert_ne!(content_id, source_content, "a copied stream must be renumbered into the target");
        assert!(target.objects.contains_key(&content_id), "the renumbered stream must exist in the target");

        let copied_bytes = target.get_object(content_id).and_then(lopdf::Object::as_stream).expect("the copy is a stream");
        let source_bytes = source
            .get_object(source_content)
            .and_then(lopdf::Object::as_stream)
            .expect("the source is a stream");
        assert_eq!(copied_bytes.content, source_bytes.content, "renumbering must not alter the stream it copies");
    }

    #[test]
    fn copies_inherited_geometry_onto_the_page_it_copies() {
        let source = load(&fixture::build_inherited_geometry());
        let source_page = source.page_iter().next().expect("the fixture has pages");

        let mut target = lopdf::Document::with_version("1.5");
        let copied = copy_page_into(&source, source_page, &mut target).expect("a valid page must copy");

        let page = target.get_dictionary(copied).expect("the copy must resolve");
        assert!(page.has(b"MediaBox"), "a copy must not depend on an ancestor it left behind");
        assert_eq!(page.get(b"Rotate").and_then(lopdf::Object::as_i64).unwrap_or(0), 90);
        assert!(!page.has(b"Parent"), "the caller decides where a copied page is parented");
    }

    #[test]
    fn stops_copying_at_other_pages_in_the_source_tree() {
        let source = load(&fixture::build_nested_page_tree());
        let source_page = source.page_iter().next().expect("the fixture has pages");

        let mut target = lopdf::Document::with_version("1.5");
        copy_page_into(&source, source_page, &mut target).expect("a valid page must copy");

        let page_nodes = target.objects.values().filter(|object| {
            object
                .as_dict()
                .map(|dict| matches!(dict.get_type(), Ok(name) if name == b"Page".as_slice() || name == b"Pages".as_slice()))
                .unwrap_or(false)
        });
        assert_eq!(page_nodes.count(), 1, "copying one page must not drag the rest of the tree along");
    }
}

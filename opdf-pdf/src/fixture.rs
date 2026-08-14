//! Programmatically generated PDF fixtures.
//!
//! A real-world corpus is Track E's deliverable and does not exist yet. Every
//! fixture here is built from `lopdf` primitives so that each one isolates
//! exactly one parsing hazard and is readable in source form.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use opdf_core::PageSize;

//---------------------------------------------------------------------
// Shared builder helpers
//---------------------------------------------------------------------

/// Serialize a document to bytes, as a fixture file would be written to disk.
fn serialize_document(document: &mut Document) -> Vec<u8> {
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("a fixture must serialise");
    bytes
}

/// A `/MediaBox` array covering `size` with its origin at zero.
fn build_media_box(size: PageSize) -> Object {
    Object::Array(vec![
        Object::Real(0.0),
        Object::Real(0.0),
        Object::Real(size.width_pt),
        Object::Real(size.height_pt),
    ])
}

/// A page dictionary with a one-operator content stream, parented to `parent_id`.
fn build_page(document: &mut Document, parent_id: ObjectId, size: Option<PageSize>) -> ObjectId {
    let content_id = document.add_object(Stream::new(Dictionary::new(), b"0 0 1 rg\n".to_vec()));
    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Parent", Object::Reference(parent_id));
    if let Some(size) = size {
        page.set("MediaBox", build_media_box(size));
    }
    page.set("Resources", Object::Dictionary(Dictionary::new()));
    page.set("Contents", Object::Reference(content_id));
    document.add_object(Object::Dictionary(page))
}

/// Install a `/Pages` node and a `/Catalog` pointing at it, and set the trailer's `/Root`.
fn finish_document(document: &mut Document, pages_id: ObjectId, mut pages: Dictionary, kids: Vec<Object>) {
    let count = i64::try_from(kids.len()).unwrap_or(i64::MAX);
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(kids));
    pages.set("Count", Object::Integer(count));
    document.objects.insert(pages_id, Object::Dictionary(pages));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = document.add_object(Object::Dictionary(catalog));
    document.trailer.set("Root", Object::Reference(catalog_id));
}

//---------------------------------------------------------------------
// Page-tree shape fixtures
//---------------------------------------------------------------------

/// A flat page tree with one page per entry of `sizes`, each carrying its own `/MediaBox`.
pub(crate) fn build_flat_pages(sizes: &[PageSize]) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let kids: Vec<Object> = sizes
        .iter()
        .map(|size| Object::Reference(build_page(&mut document, pages_id, Some(*size))))
        .collect();
    finish_document(&mut document, pages_id, Dictionary::new(), kids);
    serialize_document(&mut document)
}

/// Root `/Pages` -> [intermediate `/Pages` -> [A, B], C]. Depth-first order is A, B, C.
pub(crate) fn build_nested_page_tree() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let root_id = document.new_object_id();
    let branch_id = document.new_object_id();

    let page_a = build_page(&mut document, branch_id, Some(PageSize::new(100.0, 100.0)));
    let page_b = build_page(&mut document, branch_id, Some(PageSize::new(200.0, 200.0)));
    let page_c = build_page(&mut document, root_id, Some(PageSize::new(300.0, 300.0)));

    let mut branch = Dictionary::new();
    branch.set("Type", Object::Name(b"Pages".to_vec()));
    branch.set("Parent", Object::Reference(root_id));
    branch.set("Kids", Object::Array(vec![Object::Reference(page_a), Object::Reference(page_b)]));
    branch.set("Count", Object::Integer(2));
    document.objects.insert(branch_id, Object::Dictionary(branch));

    let kids = vec![Object::Reference(branch_id), Object::Reference(page_c)];
    finish_document(&mut document, root_id, Dictionary::new(), kids);
    serialize_document(&mut document)
}

//---------------------------------------------------------------------
// Geometry fixtures
//---------------------------------------------------------------------

/// Two pages that carry no geometry of their own: `/MediaBox` and `/Rotate` sit on the root node.
pub(crate) fn build_inherited_geometry() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let kids: Vec<Object> = (0..2).map(|_| Object::Reference(build_page(&mut document, pages_id, None))).collect();

    let mut pages = Dictionary::new();
    pages.set("MediaBox", build_media_box(PageSize::A4));
    pages.set("Rotate", Object::Integer(90));
    finish_document(&mut document, pages_id, pages, kids);
    serialize_document(&mut document)
}

/// Three A4 pages rotated 90, 180, and -90 degrees, the last testing negative normalisation.
pub(crate) fn build_rotated_pages() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let mut kids = Vec::new();
    for degrees in [90_i64, 180, -90] {
        let page_id = build_page(&mut document, pages_id, Some(PageSize::A4));
        if let Ok(page) = document.get_dictionary_mut(page_id) {
            page.set("Rotate", Object::Integer(degrees));
        }
        kids.push(Object::Reference(page_id));
    }
    finish_document(&mut document, pages_id, Dictionary::new(), kids);
    serialize_document(&mut document)
}

/// One page with no `/MediaBox` anywhere in the tree.
pub(crate) fn build_missing_media_box() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let kids = vec![Object::Reference(build_page(&mut document, pages_id, None))];
    finish_document(&mut document, pages_id, Dictionary::new(), kids);
    serialize_document(&mut document)
}

/// One page whose `/MediaBox` is an indirect reference to a shared array object.
pub(crate) fn build_indirect_media_box() -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let media_box_id = document.add_object(build_media_box(PageSize::new(300.0, 500.0)));
    let page_id = build_page(&mut document, pages_id, None);
    if let Ok(page) = document.get_dictionary_mut(page_id) {
        page.set("MediaBox", Object::Reference(media_box_id));
    }
    finish_document(&mut document, pages_id, Dictionary::new(), vec![Object::Reference(page_id)]);
    serialize_document(&mut document)
}

//---------------------------------------------------------------------
// Damage fixtures
//---------------------------------------------------------------------

/// A plausible header followed by bytes that are not PDF syntax.
pub(crate) fn build_damaged_bytes() -> Vec<u8> {
    b"%PDF-1.7\nthis file claims to be a pdf and is not\n%%EOF\n".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_pages(bytes: &[u8]) -> usize {
        lopdf::Document::load_mem(bytes).expect("fixture must parse").page_iter().count()
    }

    #[test]
    fn builds_a_flat_document_of_the_requested_length() {
        let bytes = build_flat_pages(&[PageSize::A4, PageSize::LETTER, PageSize::new(200.0, 400.0)]);
        assert_eq!(count_pages(&bytes), 3);
    }

    #[test]
    fn builds_a_nested_tree_that_enumerates_depth_first() {
        let bytes = build_nested_page_tree();
        let document = lopdf::Document::load_mem(&bytes).expect("fixture must parse");
        let order: Vec<lopdf::ObjectId> = document.page_iter().collect();
        assert_eq!(order.len(), 3, "the nested fixture holds three pages");

        let widths: Vec<i64> = order
            .iter()
            .map(|object_id| {
                document
                    .get_dictionary(*object_id)
                    .and_then(|dict| dict.get(b"MediaBox"))
                    .and_then(lopdf::Object::as_array)
                    .map(|corners| corners[2].as_float().unwrap_or(0.0) as i64)
                    .unwrap_or(0)
            })
            .collect();
        assert_eq!(widths, vec![100, 200, 300], "depth-first order must be A, B, then C");
    }

    #[test]
    fn builds_a_document_whose_geometry_lives_on_the_root_node() {
        let bytes = build_inherited_geometry();
        let document = lopdf::Document::load_mem(&bytes).expect("fixture must parse");
        for object_id in document.page_iter() {
            let page = document.get_dictionary(object_id).expect("page must resolve");
            assert!(!page.has(b"MediaBox"), "the inherited fixture must not carry MediaBox on a page");
            assert!(!page.has(b"Rotate"), "the inherited fixture must not carry Rotate on a page");
        }
    }

    #[test]
    fn builds_the_remaining_fixtures_so_they_parse() {
        assert_eq!(count_pages(&build_rotated_pages()), 3);
        assert_eq!(count_pages(&build_missing_media_box()), 1);
        assert_eq!(count_pages(&build_indirect_media_box()), 1);
    }

    #[test]
    fn builds_bytes_that_are_not_a_parseable_document() {
        assert!(lopdf::Document::load_mem(&build_damaged_bytes()).is_err(), "the damaged fixture must not parse");
    }
}

//! Behavioural contract that every [`Document`] implementation must satisfy.

use crate::document::Document;
use crate::page::{PageId, PageSize, Rotation};

/// Assert that an implementation honours the [`Document`] contract.
///
/// `make_document` receives a page count and must return a document with that
/// many pages, each of a size the implementation supports.
///
/// # Panics
///
/// Panics with a descriptive message on the first violated requirement.
pub fn assert_document_contract<D, F>(make_document: F)
where
    D: Document,
    F: Fn(usize) -> D,
{
    assert_reports_its_page_count(&make_document);
    assert_lists_ids_in_order(&make_document);
    assert_rejects_unknown_page_ids(&make_document);
    assert_removal_preserves_other_identities(&make_document);
    assert_move_reorders_without_changing_identity(&make_document);
    assert_move_rejects_out_of_range_targets(&make_document);
    assert_rotation_round_trips(&make_document);
    assert_insert_returns_a_fresh_identity(&make_document);
    assert_import_preserves_order_and_allocates_fresh_ids(&make_document);
    assert_import_rejects_out_of_range_positions(&make_document);
}

fn assert_reports_its_page_count<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let document = make_document(3);
    assert_eq!(document.page_count(), 3, "page_count must match the number of pages present");
    assert_eq!(document.page_ids().len(), 3, "page_ids must return one entry per page");
}

fn assert_lists_ids_in_order<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let document = make_document(4);
    for (index, id) in document.page_ids().into_iter().enumerate() {
        let reported = document.index_of(id).expect("page_ids must return resolvable identities");
        assert_eq!(reported, index, "page_ids must be in document order");
    }
}

fn assert_rejects_unknown_page_ids<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let document = make_document(1);
    let unknown = PageId::new(u64::MAX);
    assert!(document.page(unknown).is_err(), "page() must reject unknown identities");
    assert!(document.index_of(unknown).is_err(), "index_of() must reject unknown identities");
}

fn assert_removal_preserves_other_identities<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(3);
    let ids = document.page_ids();
    document.remove_page(ids[0]).expect("removing an existing page must succeed");

    assert_eq!(document.page_count(), 2, "removal must reduce the page count by one");
    assert!(document.page(ids[0]).is_err(), "a removed page must no longer resolve");
    assert_eq!(document.index_of(ids[1]).expect("survivors keep their identity"), 0);
    assert_eq!(document.index_of(ids[2]).expect("survivors keep their identity"), 1);
}

fn assert_move_reorders_without_changing_identity<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(3);
    let ids = document.page_ids();
    document.move_page(ids[0], 2).expect("moving to a valid position must succeed");

    assert_eq!(
        document.page_ids(),
        vec![ids[1], ids[2], ids[0]],
        "move_page must reorder to the requested position"
    );
    assert_eq!(document.page_count(), 3, "move_page must not change the page count");
}

fn assert_move_rejects_out_of_range_targets<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(2);
    let ids = document.page_ids();
    assert!(document.move_page(ids[0], 99).is_err(), "move_page must reject positions beyond the document");
    assert_eq!(document.page_ids(), ids, "a rejected move must leave the order untouched");
}

fn assert_rotation_round_trips<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(1);
    let id = document.page_ids()[0];
    document.set_rotation(id, Rotation::Quarter).expect("setting rotation must succeed");
    assert_eq!(document.page(id).expect("page must still resolve").rotation, Rotation::Quarter);

    document.set_rotation(id, Rotation::None).expect("clearing rotation must succeed");
    assert_eq!(document.page(id).expect("page must still resolve").rotation, Rotation::None);
}

fn assert_insert_returns_a_fresh_identity<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(2);
    let before = document.page_ids();
    let inserted = document.insert_page(1, PageSize::A4).expect("inserting at a valid position must succeed");

    assert!(!before.contains(&inserted), "insert_page must return an identity not already in use");
    assert_eq!(document.index_of(inserted).expect("inserted page must resolve"), 1);
    assert_eq!(document.page_count(), 3, "insert_page must increase the page count by one");
    assert!(
        document.insert_page(99, PageSize::A4).is_err(),
        "insert_page must reject positions beyond the document"
    );
}

fn assert_import_preserves_order_and_allocates_fresh_ids<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let source = make_document(3);
    let source_ids = source.page_ids();
    let mut target = make_document(2);
    let target_ids = target.page_ids();

    let imported = target.import_pages(&source, &source_ids, 1).expect("importing existing pages must succeed");

    assert_eq!(imported.len(), 3, "import_pages must return one identity per imported page");
    assert_eq!(target.page_count(), 5, "import_pages must add one page per imported page");
    for (offset, id) in imported.iter().enumerate() {
        assert_eq!(
            target.index_of(*id).expect("an imported page must resolve"),
            1 + offset,
            "import_pages must preserve the order of the requested ids"
        );
        assert!(
            !target_ids.contains(id),
            "imported pages must receive identities not already in use in the target"
        );
    }
    assert_eq!(
        target.index_of(target_ids[1]).expect("an existing page must resolve"),
        4,
        "pages after the insertion point must shift by the number imported"
    );
}

fn assert_import_rejects_out_of_range_positions<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let source = make_document(1);
    let source_ids = source.page_ids();
    let mut target = make_document(2);

    assert!(
        target.import_pages(&source, &source_ids, 99).is_err(),
        "import_pages must reject positions beyond the document"
    );
    assert_eq!(target.page_count(), 2, "a rejected import must leave the document untouched");
}

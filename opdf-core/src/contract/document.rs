//! Behavioural contract that every [`Document`] implementation must satisfy.

use crate::document::Document;
use crate::error::Error;
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
    assert_removed_pages_can_be_restored(&make_document);
    assert_move_reorders_without_changing_identity(&make_document);
    assert_move_rejects_out_of_range_targets(&make_document);
    assert_rotation_round_trips(&make_document);
    assert_insert_returns_a_fresh_identity(&make_document);
    assert_import_preserves_order_and_allocates_fresh_ids(&make_document);
    assert_import_rejects_out_of_range_positions(&make_document);
    assert_mutations_reject_unknown_page_ids(&make_document);
    assert_import_rejects_unknown_source_pages(&make_document);
    assert_append_positions_are_valid(&make_document);
    assert_every_mutation_advances_the_revision(&make_document);
    assert_failed_mutations_leave_the_revision_untouched(&make_document);
    assert_read_only_calls_never_advance_the_revision(&make_document);
    assert_document_identity_is_stable_and_unique(&make_document);
}

//---------------------------------------------------------------------
// Identity
//---------------------------------------------------------------------

/// Two documents of the same implementation must never share an identity, and a
/// document's identity must not change as it is mutated.
///
/// This is the assertion that makes [`Document::id`] worth having. An
/// implementation that returned a constant would satisfy every other requirement
/// in this suite, because identity is only ever *compared*: a shared value looks
/// exactly like a working one until two documents are open at once and one
/// document's tiles are served for the other's pages.
///
/// Stability across a mutation is the other half. An identity that advanced with
/// the revision would make a cross-document command's binding fail against the
/// very document it was built for, and would tell a tile cache that every edit
/// opened a new document.
fn assert_document_identity_is_stable_and_unique<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let first = make_document(1);
    let second = make_document(1);
    let third = make_document(1);
    assert_ne!(
        first.id(),
        second.id(),
        "two documents must not share an identity, or a cache keyed on it serves one document's tiles for another"
    );
    assert_ne!(third.id(), first.id(), "an identity must not be handed out twice");
    assert_ne!(third.id(), second.id(), "an identity must not be handed out twice");

    //--- every kind of mutation, because forgetting exactly one is the realistic failure ---
    let mut document = make_document(2);
    let before = document.id();
    let ids = document.page_ids();
    document.set_rotation(ids[0], Rotation::Quarter).expect("setting rotation must succeed");
    document.move_page(ids[0], 1).expect("moving to a valid position must succeed");
    document.remove_page(ids[0]).expect("removing an existing page must succeed");
    document.restore_page(ids[0], 0).expect("restoring a removed page must succeed");
    document.insert_page(0, PageSize::A4).expect("inserting at a valid position must succeed");
    assert_eq!(
        document.id(),
        before,
        "a mutation must not change a document's identity: it is the same open document, edited"
    );

    let source = make_document(1);
    document
        .import_pages(&source, &source.page_ids(), 0)
        .expect("importing existing pages must succeed");
    assert_eq!(document.id(), before, "importing pages must not change the importing document's identity");
    assert_ne!(source.id(), document.id(), "importing from a document must not merge the two identities");

    //--- and a rejected mutation must not disturb it either ---
    assert!(document.remove_page(PageId::new(u64::MAX)).is_err());
    assert_eq!(document.id(), before, "a rejected mutation must not change a document's identity");

    //--- identity is stable across reads, exactly as the revision is ---
    assert_eq!(document.id(), document.id(), "id() must be stable when nothing has changed between two reads");
}

//---------------------------------------------------------------------
// Revision counter
//---------------------------------------------------------------------

/// Check each mutating method individually.
///
/// Checking one and generalising would let an implementation that advances on
/// `remove_page` but forgets `set_rotation` pass — and forgetting exactly one
/// mutation is the realistic failure, not forgetting all of them.
///
/// `restore_page` is the exception to the file's layout, not to the rule: its
/// revision behaviour on success and on failure is checked inside
/// `assert_removed_pages_can_be_restored`, where the trash state it needs is
/// already set up.
fn assert_every_mutation_advances_the_revision<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(3);
    let ids = document.page_ids();
    let before = document.revision();
    document.remove_page(ids[0]).expect("removing an existing page must succeed");
    assert_ne!(before, document.revision(), "remove_page must advance the revision on success");

    let mut document = make_document(3);
    let ids = document.page_ids();
    let before = document.revision();
    document.move_page(ids[0], 2).expect("moving to a valid position must succeed");
    assert_ne!(before, document.revision(), "move_page must advance the revision on success");

    let mut document = make_document(1);
    let id = document.page_ids()[0];
    let before = document.revision();
    document.set_rotation(id, Rotation::Quarter).expect("setting rotation must succeed");
    assert_ne!(before, document.revision(), "set_rotation must advance the revision on success");

    let mut document = make_document(2);
    let before = document.revision();
    document.insert_page(1, PageSize::A4).expect("inserting at a valid position must succeed");
    assert_ne!(before, document.revision(), "insert_page must advance the revision on success");

    let source = make_document(2);
    let mut target = make_document(2);
    let before = target.revision();
    target
        .import_pages(&source, &source.page_ids(), 1)
        .expect("importing existing pages must succeed");
    assert_ne!(before, target.revision(), "import_pages must advance the revision on success");
}

/// A rejected mutation changed nothing, so a cache keyed on the revision must
/// not be forced to discard tiles that are still valid.
fn assert_failed_mutations_leave_the_revision_untouched<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let unknown = PageId::new(u64::MAX);

    //--- an unknown identity: rejected before anything is touched ---
    let mut document = make_document(2);
    let before = document.revision();
    assert!(document.remove_page(unknown).is_err(), "remove_page must reject an unknown identity");
    assert_eq!(before, document.revision(), "a failed remove_page must leave the revision untouched");

    assert!(document.move_page(unknown, 0).is_err(), "move_page must reject an unknown identity");
    assert_eq!(before, document.revision(), "a failed move_page must leave the revision untouched");

    assert!(
        document.set_rotation(unknown, Rotation::Quarter).is_err(),
        "set_rotation must reject an unknown identity"
    );
    assert_eq!(before, document.revision(), "a failed set_rotation must leave the revision untouched");

    //--- an out-of-range index: an implementation may mutate internally before discovering this, and must still not advance ---
    let ids = document.page_ids();
    assert!(document.move_page(ids[0], 99).is_err(), "move_page must reject a position beyond the document");
    assert_eq!(
        before,
        document.revision(),
        "a move rejected for its index must leave the revision untouched, even if the page was lifted out and put back"
    );

    assert!(
        document.insert_page(99, PageSize::A4).is_err(),
        "insert_page must reject a position beyond the document"
    );
    assert_eq!(before, document.revision(), "a failed insert_page must leave the revision untouched");

    let source = make_document(1);
    assert!(
        document.import_pages(&source, &source.page_ids(), 99).is_err(),
        "import_pages must reject a position beyond the document"
    );
    assert_eq!(before, document.revision(), "a failed import_pages must leave the revision untouched");
}

fn assert_read_only_calls_never_advance_the_revision<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let document = make_document(2);
    let before = document.revision();
    let ids = document.page_ids();

    let _ = document.page_count();
    let _ = document.page_ids();
    let _ = document.page(ids[0]);
    let _ = document.index_of(ids[0]);

    assert_eq!(before, document.revision(), "inspecting a document must not advance its revision");
    assert_eq!(
        document.revision(),
        document.revision(),
        "revision() must be stable when nothing has changed between two reads"
    );
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
    assert!(
        matches!(document.page(unknown), Err(Error::PageNotFound(_))),
        "page() must reject an unknown identity with Error::PageNotFound, got: {:?}",
        document.page(unknown)
    );
    assert!(
        matches!(document.index_of(unknown), Err(Error::PageNotFound(_))),
        "index_of() must reject an unknown identity with Error::PageNotFound, got: {:?}",
        document.index_of(unknown)
    );
}

fn assert_removal_preserves_other_identities<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(3);
    let ids = document.page_ids();
    document.remove_page(ids[0]).expect("removing an existing page must succeed");

    assert_eq!(document.page_count(), 2, "removal must reduce the page count by one");
    assert!(
        matches!(document.page(ids[0]), Err(Error::PageNotFound(_))),
        "a removed page must no longer resolve, and must report Error::PageNotFound, got: {:?}",
        document.page(ids[0])
    );
    assert_eq!(document.index_of(ids[1]).expect("survivors keep their identity"), 0);
    assert_eq!(document.index_of(ids[2]).expect("survivors keep their identity"), 1);
}

/// Check that a deletion can be undone exactly, not approximately.
///
/// The whole point of `restore_page` is that the page comes back as *itself*:
/// an implementation that quietly substitutes a blank page of default geometry
/// under a fresh identity satisfies every count-based assertion and still leaves
/// undo broken, so identity, size, and rotation are each pinned individually.
fn assert_removed_pages_can_be_restored<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    //--- a restored page keeps its identity, geometry, and rotation, and lands where it was asked to ---
    let mut document = make_document(3);
    let ids = document.page_ids();
    document.set_rotation(ids[1], Rotation::Quarter).expect("setting rotation must succeed");
    let before = document.page(ids[1]).expect("an existing page must resolve");

    document.remove_page(ids[1]).expect("removing an existing page must succeed");
    let revision_before_restore = document.revision();
    document.restore_page(ids[1], 0).expect("restoring a removed page must succeed");

    //--- read the identity off the page list first: a restore that allocated a fresh id would otherwise fail as a bare lookup miss, naming the symptom rather than the rule ---
    assert_eq!(
        document.page_ids().first().copied(),
        Some(ids[1]),
        "restore_page must bring the page back under its original identity, never a freshly allocated one"
    );

    let restored = document.page(ids[1]).expect("a restored page must resolve");
    assert_eq!(
        restored.id, before.id,
        "a restored page must report its original identity through page(), consistently with page_ids()"
    );
    assert_eq!(
        restored.size, before.size,
        "a restored page must keep the geometry it had when it was removed, not a default size"
    );
    assert_eq!(
        restored.rotation, before.rotation,
        "a restored page must keep the rotation it had when it was removed, not Rotation::None"
    );
    assert_eq!(
        document.index_of(ids[1]).expect("a restored page must resolve"),
        0,
        "restore_page must place the page at the requested index"
    );
    assert_eq!(document.page_count(), 3, "restore_page must increase the page count by one");
    assert_ne!(
        revision_before_restore,
        document.revision(),
        "restore_page must advance the revision on success"
    );

    //--- undoing the deletion of a last page restores at page_count, which is an append, not an error ---
    let mut document = make_document(2);
    let ids = document.page_ids();
    document.remove_page(ids[1]).expect("removing an existing page must succeed");
    let end = document.page_count();
    document
        .restore_page(ids[1], end)
        .expect("restoring at page_count must append rather than fail, or the deletion of a last page cannot be undone");
    assert_eq!(
        document.index_of(ids[1]).expect("a restored page must resolve"),
        1,
        "restore_page at page_count must place the page last"
    );

    //--- an out-of-range index is rejected, changes nothing, and does not consume the page ---
    let mut document = make_document(3);
    let ids = document.page_ids();
    document.remove_page(ids[0]).expect("removing an existing page must succeed");
    let order = document.page_ids();
    let revision = document.revision();

    //--- each rejection is bound before it is inspected: a mutating call must not be repeated inside the failure message ---
    let rejected = document.restore_page(ids[0], 99);
    assert!(
        matches!(rejected, Err(Error::IndexOutOfBounds { .. })),
        "restore_page must reject a position beyond the document with Error::IndexOutOfBounds, got: {rejected:?}"
    );
    assert_eq!(document.page_ids(), order, "a rejected restore must leave the document untouched");
    assert_eq!(document.revision(), revision, "a failed restore_page must leave the revision untouched");
    document
        .restore_page(ids[0], 0)
        .expect("a restore rejected for its index must not have discarded the page");

    //--- an identity the document never held is not restorable ---
    let mut document = make_document(2);
    let revision = document.revision();
    let unknown = PageId::new(u64::MAX);
    let rejected = document.restore_page(unknown, 0);
    assert!(
        matches!(rejected, Err(Error::PageNotFound(_))),
        "restore_page must reject an identity the document never held with Error::PageNotFound, got: {rejected:?}"
    );
    assert_eq!(document.revision(), revision, "a failed restore_page must leave the revision untouched");

    //--- restoring a live page is a caller error, distinct from both of the above ---
    let ids = document.page_ids();
    let rejected = document.restore_page(ids[0], 0);
    assert!(
        matches!(rejected, Err(Error::Unsupported(_))),
        "restoring a page that is currently present must return Error::Unsupported, never succeed as a no-op or duplicate the page, got: {rejected:?}"
    );
    assert_eq!(document.page_ids(), ids, "a rejected restore must leave the document untouched");
    assert_eq!(document.revision(), revision, "a failed restore_page must leave the revision untouched");
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
    assert!(
        matches!(document.move_page(ids[0], 99), Err(Error::IndexOutOfBounds { .. })),
        "move_page must reject a position beyond the document with Error::IndexOutOfBounds"
    );
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
        matches!(document.insert_page(99, PageSize::A4), Err(Error::IndexOutOfBounds { .. })),
        "insert_page must reject a position beyond the document with Error::IndexOutOfBounds"
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
        matches!(target.import_pages(&source, &source_ids, 99), Err(Error::IndexOutOfBounds { .. })),
        "import_pages must reject a position beyond the document with Error::IndexOutOfBounds"
    );
    assert_eq!(target.page_count(), 2, "a rejected import must leave the document untouched");
}

fn assert_mutations_reject_unknown_page_ids<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let unknown = PageId::new(u64::MAX);
    let mut document = make_document(2);
    let before = document.page_ids();

    assert!(
        matches!(document.remove_page(unknown), Err(Error::PageNotFound(_))),
        "remove_page must reject an unknown identity with Error::PageNotFound"
    );
    assert!(
        matches!(document.move_page(unknown, 0), Err(Error::PageNotFound(_))),
        "move_page must reject an unknown identity with Error::PageNotFound, in preference to Error::IndexOutOfBounds"
    );
    assert!(
        matches!(document.set_rotation(unknown, Rotation::Quarter), Err(Error::PageNotFound(_))),
        "set_rotation must reject an unknown identity with Error::PageNotFound"
    );
    assert_eq!(document.page_ids(), before, "a rejected mutation must leave the document untouched");
}

fn assert_import_rejects_unknown_source_pages<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let source = make_document(1);
    let mut target = make_document(2);
    let before = target.page_ids();

    let mut ids = source.page_ids();
    ids.push(PageId::new(u64::MAX));

    assert!(
        matches!(target.import_pages(&source, &ids, 0), Err(Error::PageNotFound(_))),
        "import_pages must reject an unknown source identity with Error::PageNotFound"
    );
    assert_eq!(target.page_ids(), before, "a rejected import must leave the document untouched");
}

fn assert_append_positions_are_valid<D: Document, F: Fn(usize) -> D>(make_document: &F) {
    let mut document = make_document(2);
    let end = document.page_count();
    let appended = document
        .insert_page(end, PageSize::A4)
        .expect("inserting at page_count must append rather than fail");
    assert_eq!(
        document.index_of(appended).expect("an appended page must resolve"),
        2,
        "insert_page at page_count must place the page last"
    );
    assert_eq!(
        document.page(appended).expect("an appended page must resolve").size,
        PageSize::A4,
        "insert_page must honour the requested page size"
    );

    let source = make_document(1);
    let mut target = make_document(2);
    let target_end = target.page_count();
    let imported = target
        .import_pages(&source, &source.page_ids(), target_end)
        .expect("importing at page_count must append rather than fail");
    assert_eq!(
        target.index_of(imported[0]).expect("an imported page must resolve"),
        2,
        "import_pages at page_count must place pages last"
    );
}

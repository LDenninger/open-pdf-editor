//! Property tests for the invertibility claim every command in this crate makes.
//!
//! Each command's unit test pins one hand-picked example. That is enough to
//! show a command works and not enough to show it works for the inputs a user
//! actually produces — an empty selection, a range whose ends are the same, a
//! range typed backwards, a move to the position the page already occupies, a
//! rotation to the rotation already set. Two shipped defects, a panic on an
//! inverted range and an inverse bound to the wrong document, were invisible to
//! the example suite for exactly that reason.
//!
//! The core property is one sentence: **applying a command and then applying
//! the inverse it returned leaves the document's page list exactly as it was**
//! — same pages, same identities, same order, same rotations. Revision is
//! deliberately excluded, because undo is itself a mutation and must advance
//! it rather than rewind it.
//!
//! The cross-document operations carry a second property: **an inverse must
//! refuse every document but the one it was built against**, which is the
//! property whose absence let an extraction's undo delete source pages.

use std::num::NonZeroUsize;

use opdf_core::fakes::VecDocument;
use opdf_core::page::{PageInfo, PageSize, Rotation};
use opdf_core::{Command, Document, DocumentSnapshot};
use opdf_ops::{InsertBlankPage, Merge, MovePage, RemovePage, SetRotation, UndoStack, delete_selection, extract_range, rotate_selection, split_at};
use proptest::prelude::*;

//---------------------------------------------------------------------
// Generators
//---------------------------------------------------------------------

/// Small enough that shrinking reports a readable counterexample, large enough
/// that a selection can straddle both ends and leave gaps in the middle.
const MAX_PAGES: usize = 8;

/// A range end may run one past the document, so that out-of-bounds and
/// inverted ranges are generated rather than assumed away.
const MAX_RANGE_END: usize = MAX_PAGES + 2;

fn any_rotation() -> impl Strategy<Value = Rotation> {
    prop_oneof![
        Just(Rotation::None),
        Just(Rotation::Quarter),
        Just(Rotation::Half),
        Just(Rotation::ThreeQuarter),
    ]
}

fn any_size() -> impl Strategy<Value = PageSize> {
    prop_oneof![Just(PageSize::A4), Just(PageSize::LETTER), Just(PageSize::new(200.0_f32, 200.0_f32))]
}

/// A document as a list of page descriptions, including the empty document.
fn any_pages() -> impl Strategy<Value = Vec<(PageSize, Rotation)>> {
    prop::collection::vec((any_size(), any_rotation()), 0..=MAX_PAGES)
}

/// A document together with a selection mask of exactly its length, so that
/// selections range over everything from empty to the whole document.
fn any_pages_and_selection() -> impl Strategy<Value = (Vec<(PageSize, Rotation)>, Vec<bool>)> {
    any_pages().prop_flat_map(|pages| {
        let count = pages.len();
        (Just(pages), prop::collection::vec(any::<bool>(), count))
    })
}

//---------------------------------------------------------------------
// Helpers
//---------------------------------------------------------------------

fn build(pages: &[(PageSize, Rotation)]) -> VecDocument {
    let mut document = VecDocument::new();
    for (index, (size, rotation)) in pages.iter().enumerate() {
        let id = document.insert_page(index, *size).unwrap();
        document.set_rotation(id, *rotation).unwrap();
    }
    document
}

/// The page list, which is what "restores the original state" means here.
fn pages_of(document: &VecDocument) -> Vec<PageInfo> {
    DocumentSnapshot::of(document).unwrap().pages
}

fn selected(document: &VecDocument, mask: &[bool]) -> Vec<opdf_core::PageId> {
    document
        .page_ids()
        .into_iter()
        .zip(mask.iter())
        .filter_map(|(id, keep)| keep.then_some(id))
        .collect()
}

//---------------------------------------------------------------------
// Single-document commands
//---------------------------------------------------------------------

proptest! {
    #[test]
    fn undoing_a_removal_restores_the_page_list(pages in any_pages(), which in any::<prop::sample::Index>()) {
        prop_assume!(!pages.is_empty());
        let mut document = build(&pages);
        let before = pages_of(&document);
        let page = document.page_ids()[which.index(document.page_count())];

        let inverse = RemovePage { page }.apply(&mut document).unwrap();
        prop_assert_eq!(document.page_count(), before.len() - 1);

        inverse.apply(&mut document).unwrap();
        prop_assert_eq!(pages_of(&document), before);
    }

    /// `to_index` covers the degenerate move-to-where-it-already-is, both ends
    /// of the document, and one position past the last legal index.
    #[test]
    fn undoing_a_move_restores_the_page_list(pages in any_pages(), from in any::<prop::sample::Index>(), to in 0..=MAX_PAGES) {
        prop_assume!(!pages.is_empty());
        let mut document = build(&pages);
        let before = pages_of(&document);
        let page = document.page_ids()[from.index(document.page_count())];

        let command = MovePage { page, to_index: to };
        match command.apply(&mut document) {
            Ok(inverse) => {
                inverse.apply(&mut document).unwrap();
                prop_assert_eq!(pages_of(&document), before);
            }
            Err(_) => prop_assert_eq!(pages_of(&document), before, "a rejected move must leave the document untouched"),
        }
    }

    /// Includes rotating a page to the rotation it already has, which changes
    /// nothing and whose inverse must therefore also change nothing.
    #[test]
    fn undoing_a_rotation_restores_the_page_list(pages in any_pages(), which in any::<prop::sample::Index>(), rotation in any_rotation()) {
        prop_assume!(!pages.is_empty());
        let mut document = build(&pages);
        let before = pages_of(&document);
        let page = document.page_ids()[which.index(document.page_count())];

        let inverse = SetRotation { page, rotation }.apply(&mut document).unwrap();
        prop_assert_eq!(document.page(page).unwrap().rotation, rotation);

        inverse.apply(&mut document).unwrap();
        prop_assert_eq!(pages_of(&document), before);
    }

    #[test]
    fn undoing_an_insertion_restores_the_page_list(pages in any_pages(), at in 0..=MAX_PAGES, size in any_size()) {
        let mut document = build(&pages);
        let before = pages_of(&document);

        let command = InsertBlankPage { at_index: at, size };
        match command.apply(&mut document) {
            Ok(inverse) => {
                prop_assert_eq!(document.page_count(), before.len() + 1);
                inverse.apply(&mut document).unwrap();
                prop_assert_eq!(pages_of(&document), before);
            }
            Err(_) => prop_assert_eq!(pages_of(&document), before, "a rejected insertion must leave the document untouched"),
        }
    }
}

//---------------------------------------------------------------------
// Selections
//---------------------------------------------------------------------

proptest! {
    /// Covers the empty selection, single-page selections, non-contiguous
    /// selections with gaps at both ends, and deleting every page.
    #[test]
    fn undoing_a_selection_delete_restores_the_page_list((pages, mask) in any_pages_and_selection()) {
        let mut document = build(&pages);
        let before = pages_of(&document);
        let ids = selected(&document, &mask);
        let expected_remaining = before.len() - ids.len();

        let command: Box<dyn Command<VecDocument>> = delete_selection(&ids);
        let inverse = command.apply(&mut document).unwrap();
        prop_assert_eq!(document.page_count(), expected_remaining);

        inverse.apply(&mut document).unwrap();
        prop_assert_eq!(pages_of(&document), before, "every deleted page must return to its original index and identity");
    }

    #[test]
    fn undoing_a_selection_rotate_restores_the_page_list((pages, mask) in any_pages_and_selection(), rotation in any_rotation()) {
        let mut document = build(&pages);
        let before = pages_of(&document);
        let ids = selected(&document, &mask);

        let command: Box<dyn Command<VecDocument>> = rotate_selection(&ids, rotation);
        let inverse = command.apply(&mut document).unwrap();
        for id in &ids {
            prop_assert_eq!(document.page(*id).unwrap().rotation, rotation);
        }

        inverse.apply(&mut document).unwrap();
        prop_assert_eq!(pages_of(&document), before);
    }
}

//---------------------------------------------------------------------
// Cross-document commands
//---------------------------------------------------------------------

proptest! {
    /// `start` and `end` range over inverted, empty, single-page, whole-document
    /// and out-of-bounds ranges. Whatever the outcome, the source is read-only
    /// and the target either gains exactly the extracted pages or nothing.
    #[test]
    fn undoing_an_extraction_restores_the_target(
        source_pages in any_pages(),
        target_pages in any_pages(),
        start in 0..=MAX_RANGE_END,
        end in 0..=MAX_RANGE_END,
    ) {
        let source = build(&source_pages);
        let mut target = build(&target_pages);
        let source_before = pages_of(&source);
        let target_before = pages_of(&target);

        match extract_range(&source, &mut target, start, end) {
            Ok(extraction) => {
                prop_assert_eq!(end - start, extraction.page_ids().len());
                prop_assert_eq!(document_len(&target), target_before.len() + extraction.page_ids().len());
                extraction.undo(&mut target).unwrap();
                prop_assert_eq!(pages_of(&target), target_before);
            }
            Err(_) => prop_assert_eq!(pages_of(&target), target_before, "a rejected extraction must leave the target untouched"),
        }
        prop_assert_eq!(pages_of(&source), source_before, "extraction never mutates the source");
    }

    /// F12 as a property rather than a single example: whatever the documents
    /// and whatever the range, an extraction's undo must refuse the source. It
    /// used to succeed, because both documents allocate page ids from zero.
    #[test]
    fn an_extraction_s_undo_never_touches_the_source(
        source_pages in any_pages(),
        target_pages in any_pages(),
        start in 0..=MAX_PAGES,
        end in 0..=MAX_PAGES,
    ) {
        let mut source = build(&source_pages);
        let mut target = build(&target_pages);
        let source_before = pages_of(&source);

        let Ok(extraction) = extract_range(&source, &mut target, start, end) else {
            return Ok(());
        };

        prop_assert!(extraction.undo(&mut source).is_err(), "an extraction's undo must refuse the document it read from");
        prop_assert_eq!(pages_of(&source), source_before, "the refused undo must not have removed a source page");
    }

    /// The boundary ranges over both degenerate ends — split nothing off, split
    /// everything off — and one position past the document.
    #[test]
    fn undoing_a_split_restores_the_source_document(
        pages in any_pages(),
        target_pages in any_pages(),
        boundary in 0..=MAX_RANGE_END,
    ) {
        let mut document = build(&pages);
        let mut target = build(&target_pages);
        let before = pages_of(&document);
        let target_before = pages_of(&target);

        match split_at(&mut document, &mut target, boundary) {
            Ok(inverse) => {
                prop_assert_eq!(document_len(&document), boundary);
                prop_assert_eq!(document_len(&target), target_before.len() + before.len() - boundary);

                prop_assert!(inverse.apply(&mut target).is_err(), "the source's inverse must refuse the split-off target");

                inverse.apply(&mut document).unwrap();
                prop_assert_eq!(pages_of(&document), before);
            }
            Err(_) => {
                prop_assert_eq!(pages_of(&document), before, "a rejected split must leave the document untouched");
                prop_assert_eq!(pages_of(&target), target_before);
            }
        }
    }

    #[test]
    fn undoing_a_merge_restores_the_page_list(pages in any_pages(), sources in prop::collection::vec(any_pages(), 0..4)) {
        let mut document = build(&pages);
        let before = pages_of(&document);
        let appended: usize = sources.iter().map(Vec::len).sum();
        let merge: Merge<VecDocument> = Merge::new(sources.iter().map(|source| build(source)).collect());

        let inverse = merge.apply(&mut document).unwrap();
        prop_assert_eq!(document_len(&document), before.len() + appended);

        inverse.apply(&mut document).unwrap();
        prop_assert_eq!(pages_of(&document), before);
    }
}

//---------------------------------------------------------------------
// The undo stack over a generated edit session
//---------------------------------------------------------------------

/// One user action, resolved against whatever the document looks like when it
/// is reached rather than against the document as generated.
#[derive(Clone, Debug)]
enum Operation {
    Remove(prop::sample::Index),
    Move(prop::sample::Index, usize),
    Rotate(prop::sample::Index, Rotation),
    Insert(usize, PageSize),
    DeleteSelection(Vec<bool>),
    RotateSelection(Vec<bool>, Rotation),
}

fn any_operation() -> impl Strategy<Value = Operation> {
    prop_oneof![
        any::<prop::sample::Index>().prop_map(Operation::Remove),
        (any::<prop::sample::Index>(), 0..=MAX_PAGES).prop_map(|(page, to)| Operation::Move(page, to)),
        (any::<prop::sample::Index>(), any_rotation()).prop_map(|(page, rotation)| Operation::Rotate(page, rotation)),
        (0..=MAX_PAGES, any_size()).prop_map(|(at, size)| Operation::Insert(at, size)),
        prop::collection::vec(any::<bool>(), 0..=MAX_PAGES).prop_map(Operation::DeleteSelection),
        (prop::collection::vec(any::<bool>(), 0..=MAX_PAGES), any_rotation()).prop_map(|(mask, rotation)| Operation::RotateSelection(mask, rotation)),
    ]
}

fn command_for(document: &VecDocument, operation: &Operation) -> Option<Box<dyn Command<VecDocument>>> {
    let ids = document.page_ids();
    match operation {
        Operation::Remove(which) => {
            let page = *ids.get(which.index(ids.len().max(1)))?;
            Some(Box::new(RemovePage { page }))
        }
        Operation::Move(which, to) => {
            let page = *ids.get(which.index(ids.len().max(1)))?;
            Some(Box::new(MovePage { page, to_index: *to }))
        }
        Operation::Rotate(which, rotation) => {
            let page = *ids.get(which.index(ids.len().max(1)))?;
            Some(Box::new(SetRotation { page, rotation: *rotation }))
        }
        Operation::Insert(at, size) => Some(Box::new(InsertBlankPage { at_index: *at, size: *size })),
        Operation::DeleteSelection(mask) => Some(delete_selection(&selected(document, mask))),
        Operation::RotateSelection(mask, rotation) => Some(rotate_selection(&selected(document, mask), *rotation)),
    }
}

fn document_len(document: &VecDocument) -> usize {
    document.page_count()
}

proptest! {
    /// The end-to-end claim the undo stack exists to make: whatever edits were
    /// made, undoing all of them returns the document to where it started, and
    /// redoing all of them returns it to where the edits left it. Operations
    /// that fail or change nothing are simply not part of the history.
    #[test]
    fn undoing_and_redoing_a_whole_session_is_a_round_trip(
        pages in any_pages(),
        operations in prop::collection::vec(any_operation(), 0..8),
    ) {
        let mut document = build(&pages);
        let original = pages_of(&document);
        let mut stack: UndoStack<VecDocument> = UndoStack::with_limit(NonZeroUsize::new(64).unwrap());

        for operation in &operations {
            if let Some(command) = command_for(&document, operation) {
                //--- a rejected command is not part of the history; Sequence rolls it back, so the document is unchanged either way ---
                let _ = stack.apply(&mut document, command);
            }
        }
        let edited = pages_of(&document);

        while stack.undo(&mut document).unwrap() {}
        prop_assert_eq!(pages_of(&document), original, "undoing every recorded step must return the document to its starting state");

        while stack.redo(&mut document).unwrap() {}
        prop_assert_eq!(pages_of(&document), edited, "redoing every undone step must return the document to its edited state");
    }

    /// A command that changes nothing must not be recorded, so it can neither
    /// consume an undo press nor discard a redo branch.
    #[test]
    fn a_command_that_changes_nothing_leaves_the_history_alone(pages in any_pages()) {
        prop_assume!(!pages.is_empty());
        let mut document = build(&pages);
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        let page = document.page_ids()[0];
        stack.apply(&mut document, Box::new(RemovePage { page })).unwrap();
        stack.undo(&mut document).unwrap();
        let depths = (stack.undo_depth(), stack.redo_depth());

        stack.apply(&mut document, delete_selection(&[])).unwrap();
        stack.apply(&mut document, rotate_selection(&[], Rotation::Half)).unwrap();

        prop_assert_eq!((stack.undo_depth(), stack.redo_depth()), depths);
    }
}

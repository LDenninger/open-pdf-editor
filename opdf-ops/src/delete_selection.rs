//! Deleting a set of pages as one undoable action.

use opdf_core::{Command, Document, PageId};

use crate::remove_page::RemovePage;
use crate::sequence::Sequence;

/// Build a command that removes every page in `ids`, atomically.
///
/// If a page cannot be removed — most likely because a [`PageId`] in `ids`
/// no longer exists — every removal already applied is rolled back and the
/// whole selection is left untouched, per [`Sequence`]'s atomicity
/// guarantee.
///
/// An empty selection yields a command that changes nothing. It is not an
/// error — the selection simply had nothing in it — and `UndoStack` declines
/// to record a command that changes nothing, so it costs the user neither an
/// undo step nor their redo branch.
pub fn delete_selection<D: Document + 'static>(ids: &[PageId]) -> Box<dyn Command<D>> {
    let commands: Vec<Box<dyn Command<D>>> = ids.iter().map(|&page| Box::new(RemovePage { page }) as Box<dyn Command<D>>).collect();
    //--- the label goes into the undo menu, so it has to agree in number ---
    let label = match ids.len() {
        0 => "Delete no pages".to_string(),
        1 => "Delete 1 page".to_string(),
        count => format!("Delete {count} pages"),
    };
    Box::new(Sequence::new(label, commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::Rotation;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn deleting_a_selection_removes_every_named_page() {
        let mut document = VecDocument::with_pages(4, PageSize::A4);
        let ids = document.page_ids();

        let command: Box<dyn Command<VecDocument>> = delete_selection(&[ids[0], ids[2]]);
        command.apply(&mut document).unwrap();

        assert_eq!(document.page_ids(), vec![ids[1], ids[3]]);
    }

    #[test]
    fn an_unknown_page_in_the_selection_rolls_back_the_whole_delete_exactly() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();
        let bogus = PageId::new(9999);

        let command: Box<dyn Command<VecDocument>> = delete_selection(&[ids[0], bogus]);
        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "a failing selection must roll back to the exact original page list, ids included"
        );
    }

    /// The label goes straight into the undo menu, so it has to read as
    /// English rather than as a format string: "Delete 1 pages" was shipping.
    #[test]
    fn the_label_agrees_in_number_with_the_selection() {
        let one: Box<dyn Command<VecDocument>> = delete_selection(&[PageId::new(0)]);
        assert_eq!(one.label(), "Delete 1 page");

        let several: Box<dyn Command<VecDocument>> = delete_selection(&[PageId::new(0), PageId::new(1)]);
        assert_eq!(several.label(), "Delete 2 pages");

        let none: Box<dyn Command<VecDocument>> = delete_selection(&[]);
        assert_eq!(none.label(), "Delete no pages");
    }

    /// This is the ordering test Architecture calls out: deleting a
    /// *non-contiguous* selection and undoing it must land every page back
    /// at its original index, in original order — not merely restore the
    /// right pages in the wrong slots. Comparing the whole `pages` list
    /// (rather than, say, a sorted set of ids) is what catches an inverse
    /// that restores pages in the wrong order.
    #[test]
    fn undoing_a_non_contiguous_delete_restores_every_page_to_its_original_index() {
        let mut document = VecDocument::with_pages(4, PageSize::A4);
        let ids = document.page_ids();
        document.set_rotation(ids[1], Rotation::Quarter).unwrap();
        let before = DocumentSnapshot::of(&document).unwrap();

        //--- delete the first and third of four pages, leaving a gap on each side ---
        let command: Box<dyn Command<VecDocument>> = delete_selection(&[ids[0], ids[2]]);
        let inverse = command.apply(&mut document).unwrap();
        assert_eq!(document.page_ids(), vec![ids[1], ids[3]]);

        inverse.apply(&mut document).unwrap();
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "every page must return to its original index and identity, not just its original content"
        );
        assert_eq!(document.page_ids(), ids, "order must match exactly, not merely contain the same ids");
    }
}

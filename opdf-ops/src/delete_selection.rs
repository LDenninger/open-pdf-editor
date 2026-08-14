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
pub fn delete_selection<D: Document + 'static>(ids: &[PageId]) -> Box<dyn Command<D>> {
    let commands: Vec<Box<dyn Command<D>>> = ids.iter().map(|&page| Box::new(RemovePage { page }) as Box<dyn Command<D>>).collect();
    Box::new(Sequence::new(format!("Delete {} pages", ids.len()), commands))
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

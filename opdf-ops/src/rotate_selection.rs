//! Rotating a set of pages to the same orientation as one undoable action.

use opdf_core::{Command, Document, PageId, Rotation};

use crate::sequence::Sequence;
use crate::set_rotation::SetRotation;

/// Build a command that sets every page in `ids` to `rotation`, atomically.
pub fn rotate_selection<D: Document + 'static>(ids: &[PageId], rotation: Rotation) -> Box<dyn Command<D>> {
    let commands: Vec<Box<dyn Command<D>>> = ids
        .iter()
        .map(|&page| Box::new(SetRotation { page, rotation }) as Box<dyn Command<D>>)
        .collect();
    Box::new(Sequence::new(format!("Rotate {} pages to {} degrees", ids.len(), rotation.degrees()), commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn rotating_a_selection_rotates_every_named_page() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();

        let command: Box<dyn Command<VecDocument>> = rotate_selection(&[ids[0], ids[2]], Rotation::Half);
        command.apply(&mut document).unwrap();

        assert_eq!(document.page(ids[0]).unwrap().rotation, Rotation::Half);
        assert_eq!(document.page(ids[1]).unwrap().rotation, Rotation::None);
        assert_eq!(document.page(ids[2]).unwrap().rotation, Rotation::Half);
    }

    #[test]
    fn the_inverse_restores_the_whole_page_list_exactly() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();

        let command: Box<dyn Command<VecDocument>> = rotate_selection(&[ids[0], ids[2]], Rotation::Half);
        let inverse = command.apply(&mut document).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn an_unknown_page_rolls_back_every_rotation_already_applied() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();
        let bogus = PageId::new(9999);

        let command: Box<dyn Command<VecDocument>> = rotate_selection(&[ids[0], bogus], Rotation::Quarter);
        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }
}

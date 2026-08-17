//! Rotating a set of pages to the same orientation as one undoable action.

use opdf_core::{Command, Document, PageId, Rotation};

use crate::sequence::Sequence;
use crate::set_rotation::SetRotation;

/// Build a command that sets every page in `ids` to `rotation`, atomically.
///
/// An empty selection yields a command that changes nothing, on the same
/// terms as [`crate::delete_selection`].
pub fn rotate_selection<D: Document + ?Sized + 'static>(ids: &[PageId], rotation: Rotation) -> Box<dyn Command<D>> {
    let commands: Vec<Box<dyn Command<D>>> = ids
        .iter()
        .map(|&page| Box::new(SetRotation { page, rotation }) as Box<dyn Command<D>>)
        .collect();
    //--- the label goes into the undo menu, so it has to agree in number ---
    let degrees = rotation.degrees();
    let label = match ids.len() {
        0 => format!("Rotate no pages to {degrees} degrees"),
        1 => format!("Rotate 1 page to {degrees} degrees"),
        count => format!("Rotate {count} pages to {degrees} degrees"),
    };
    Box::new(Sequence::new(label, commands))
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
    fn the_label_agrees_in_number_with_the_selection() {
        let one: Box<dyn Command<VecDocument>> = rotate_selection(&[PageId::new(0)], Rotation::Quarter);
        assert_eq!(one.label(), "Rotate 1 page to 90 degrees");

        let several: Box<dyn Command<VecDocument>> = rotate_selection(&[PageId::new(0), PageId::new(1)], Rotation::Half);
        assert_eq!(several.label(), "Rotate 2 pages to 180 degrees");

        let none: Box<dyn Command<VecDocument>> = rotate_selection(&[], Rotation::None);
        assert_eq!(none.label(), "Rotate no pages to 0 degrees");
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

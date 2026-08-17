//! Inserting a blank page.

use opdf_core::{Command, Document, PageSize, Result};

use crate::remove_page::RemovePage;

/// Insert a blank page of a given size at a position.
pub struct InsertBlankPage {
    /// The position the new page should occupy.
    pub at_index: usize,
    /// The size of the new page.
    pub size: PageSize,
}

impl<D: Document + ?Sized> Command<D> for InsertBlankPage {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        let id = document.insert_page(self.at_index, self.size)?;
        Ok(Box::new(RemovePage { page: id }))
    }

    fn label(&self) -> String {
        format!("Insert a blank page at position {}", self.at_index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;

    #[test]
    fn inserting_grows_the_document_at_the_requested_position() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let command = InsertBlankPage {
            at_index: 1,
            size: PageSize::LETTER,
        };

        command.apply(&mut document).unwrap();

        assert_eq!(document.page_count(), 3);
        assert_eq!(document.page(document.page_ids()[1]).unwrap().size, PageSize::LETTER);
    }

    #[test]
    fn the_inverse_restores_the_whole_page_list_exactly() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = InsertBlankPage {
            at_index: 1,
            size: PageSize::LETTER,
        };
        let inverse = command.apply(&mut document).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "undoing an insertion never has to recreate anything, so identity round-trips exactly"
        );
    }

    #[test]
    fn an_out_of_range_position_leaves_the_document_untouched() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let command = InsertBlankPage {
            at_index: 99,
            size: PageSize::A4,
        };

        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }
}

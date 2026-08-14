//! Moving a page to a new position.

use opdf_core::{Command, Document, PageId, Result};

/// Move a page to a new index.
pub struct MovePage {
    /// The page to move.
    pub page: PageId,
    /// The position it should occupy after the move.
    pub to_index: usize,
}

impl<D: Document> Command<D> for MovePage {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        let from_index = document.index_of(self.page)?;
        document.move_page(self.page, self.to_index)?;
        Ok(Box::new(MovePage {
            page: self.page,
            to_index: from_index,
        }))
    }

    fn label(&self) -> String {
        format!("Move {} to position {}", self.page, self.to_index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn moving_reorders_the_page() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let command = MovePage { page: ids[0], to_index: 2 };

        command.apply(&mut document).unwrap();

        assert_eq!(document.page_ids(), vec![ids[1], ids[2], ids[0]]);
    }

    #[test]
    fn the_inverse_restores_the_whole_page_list() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = MovePage { page: ids[0], to_index: 2 };
        let inverse = command.apply(&mut document).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn an_out_of_range_target_leaves_the_document_untouched() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();
        let command = MovePage { page: ids[0], to_index: 99 };

        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn labels_describe_the_change() {
        let command: Box<dyn Command<VecDocument>> = Box::new(MovePage {
            page: PageId::new(2),
            to_index: 3,
        });
        assert_eq!(command.label(), "Move page#2 to position 4");
    }
}

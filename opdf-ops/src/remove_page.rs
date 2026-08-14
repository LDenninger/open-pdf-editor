//! Removing a page, and the command that restores one in its place.

use opdf_core::{Command, Document, PageId, Result};

/// Remove a single page from a document.
///
/// The removed page is not gone: `Document::remove_page` retains it,
/// unreferenced, until an explicit compaction, so this command's inverse
/// (`RestorePage`) can hand it back exactly as it was.
pub struct RemovePage {
    /// The page to remove.
    pub page: PageId,
}

impl<D: Document> Command<D> for RemovePage {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        let index = document.index_of(self.page)?;
        document.remove_page(self.page)?;
        Ok(Box::new(RestorePage {
            id: self.page,
            at_index: index,
        }))
    }

    fn label(&self) -> String {
        format!("Remove {}", self.page)
    }
}

/// Bring a previously removed page back at a position, under its original
/// identity.
///
/// This is [`RemovePage`]'s inverse, built directly on
/// `Document::restore_page`'s trash model: the document already holds the
/// page's original `PageId`, `PageSize`, and `Rotation`, so this command
/// only has to name the position, not reconstruct the page.
pub struct RestorePage {
    /// The page to restore. Must currently be in the trash — a page that
    /// was never removed, or one purged by compaction, is rejected.
    pub id: PageId,
    /// The position the restored page should occupy.
    pub at_index: usize,
}

impl<D: Document> Command<D> for RestorePage {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        document.restore_page(self.id, self.at_index)?;
        Ok(Box::new(RemovePage { page: self.id }))
    }

    fn label(&self) -> String {
        format!("Restore {} at position {}", self.id, self.at_index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::Rotation;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn removing_a_page_shrinks_the_document_and_preserves_other_identities() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let command = RemovePage { page: ids[1] };

        command.apply(&mut document).unwrap();

        assert_eq!(document.page_count(), 2);
        assert_eq!(document.page_ids(), vec![ids[0], ids[2]]);
    }

    #[test]
    fn the_inverse_restores_the_page_exactly() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        document.set_rotation(ids[1], Rotation::Quarter).unwrap();
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = RemovePage { page: ids[1] };
        let inverse = command.apply(&mut document).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "restore_page brings the page back under its original identity, size, and rotation — the round trip is now exact, not merely structural"
        );
    }

    #[test]
    fn failing_to_find_the_page_leaves_the_document_untouched() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let command = RemovePage { page: PageId::new(9999) };

        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn restoring_a_page_that_was_never_removed_is_rejected() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = RestorePage { id: ids[0], at_index: 0 };
        let result = command.apply(&mut document);

        assert!(
            result.is_err(),
            "restoring a currently-present page must be rejected (Error::Unsupported), not silently accepted"
        );
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn labels_describe_the_change() {
        let command: Box<dyn Command<VecDocument>> = Box::new(RemovePage { page: PageId::new(3) });
        assert_eq!(command.label(), "Remove page#3");

        let inverse: Box<dyn Command<VecDocument>> = Box::new(RestorePage {
            id: PageId::new(3),
            at_index: 1,
        });
        assert_eq!(inverse.label(), "Restore page#3 at position 2");
    }
}

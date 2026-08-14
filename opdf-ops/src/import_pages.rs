//! Importing pages from another document.

use opdf_core::{Command, Document, PageId, Result};

use crate::remove_page::RemovePage;
use crate::sequence::Sequence;

/// Copy pages from another document, in the order given, into a position.
pub struct ImportPages<D: Document> {
    /// The document pages are copied from.
    pub source: D,
    /// The source pages to copy, in the order they should appear.
    pub ids: Vec<PageId>,
    /// The position the copies should occupy.
    pub at_index: usize,
}

impl<D: Document + Send + 'static> Command<D> for ImportPages<D> {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        let new_ids = document.import_pages(&self.source, &self.ids, self.at_index)?;
        let removals: Vec<Box<dyn Command<D>>> = new_ids.into_iter().map(|id| Box::new(RemovePage { page: id }) as Box<dyn Command<D>>).collect();
        Ok(Box::new(Sequence::new("Undo import".to_string(), removals)))
    }

    fn label(&self) -> String {
        format!("Import {} pages at position {}", self.ids.len(), self.at_index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn importing_appends_copies_at_the_requested_position() {
        let source = VecDocument::with_pages(2, PageSize::A4);
        let source_ids = source.page_ids();
        let mut document = VecDocument::with_pages(1, PageSize::A4);

        let command = ImportPages {
            source,
            ids: source_ids,
            at_index: 1,
        };
        command.apply(&mut document).unwrap();

        assert_eq!(document.page_count(), 3);
    }

    #[test]
    fn the_inverse_restores_the_whole_page_list_exactly() {
        let source = VecDocument::with_pages(2, PageSize::A4);
        let source_ids = source.page_ids();
        let mut document = VecDocument::with_pages(1, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = ImportPages {
            source,
            ids: source_ids,
            at_index: 1,
        };
        let inverse = command.apply(&mut document).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "undoing an import is removing what was just added — never lossy, unlike undoing a removal"
        );
    }

    #[test]
    fn an_unknown_source_page_leaves_the_target_untouched() {
        let source = VecDocument::with_pages(1, PageSize::A4);
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = ImportPages {
            source,
            ids: vec![PageId::new(9999)],
            at_index: 0,
        };
        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }
}

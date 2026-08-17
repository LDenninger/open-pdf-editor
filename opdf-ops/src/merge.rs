//! Merging pages from other documents into this one, as one undoable action.

use opdf_core::{Command, Document, Result};

use crate::remove_page::RemovePage;
use crate::sequence::{Sequence, roll_back};

/// Append every page of every document in `sources`, in order, atomically.
///
/// Atomicity holds under the same single caveat as [`Sequence`]'s: if a source
/// fails to import and undoing an *earlier* source's import then fails too,
/// the document is left part-merged and the returned error names both
/// failures rather than reporting only the first.
pub struct Merge<D: Document> {
    sources: Vec<D>,
}

impl<D: Document> Merge<D> {
    /// Build a merge from an ordered list of source documents.
    pub fn new(sources: Vec<D>) -> Self {
        Self { sources }
    }
}

impl<D: Document + Send + 'static> Command<D> for Merge<D> {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        let mut removals: Vec<Box<dyn Command<D>>> = Vec::new();
        for source in &self.sources {
            let ids = source.page_ids();
            //--- always append: the position is recomputed after each source, so sources land in order ---
            let at_index = document.page_count();
            match document.import_pages(source, &ids, at_index) {
                Ok(new_ids) => {
                    for id in new_ids {
                        removals.push(Box::new(RemovePage { page: id }) as Box<dyn Command<D>>);
                    }
                }
                Err(error) => {
                    //--- identical to Sequence's rollback, and now literally the same code, so a failed rollback is reported there too ---
                    return Err(roll_back(document, removals, &self.label(), error));
                }
            }
        }
        removals.reverse();
        Ok(Box::new(Sequence::new(format!("Undo merge of {} documents", self.sources.len()), removals)))
    }

    fn label(&self) -> String {
        format!("Merge {} documents", self.sources.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn merging_appends_every_source_in_order() {
        let mut document = VecDocument::with_pages(1, PageSize::A4);
        let source_a = VecDocument::with_pages(2, PageSize::A4);
        let source_b = VecDocument::with_pages(1, PageSize::LETTER);

        let merge: Merge<VecDocument> = Merge::new(vec![source_a, source_b]);
        merge.apply(&mut document).unwrap();

        assert_eq!(document.page_count(), 4);
        let ids = document.page_ids();
        assert_eq!(
            document.page(ids[3]).unwrap().size,
            PageSize::LETTER,
            "the second source must land after the first, not overwrite it"
        );
    }

    #[test]
    fn the_inverse_restores_the_whole_page_list_exactly() {
        let mut document = VecDocument::with_pages(1, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let source_a = VecDocument::with_pages(2, PageSize::A4);
        let source_b = VecDocument::with_pages(1, PageSize::LETTER);

        let merge: Merge<VecDocument> = Merge::new(vec![source_a, source_b]);
        let inverse = merge.apply(&mut document).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "undoing a merge is removing what was just added — never lossy"
        );
    }

    #[test]
    fn three_sources_still_append_in_order_and_undo_cleanly() {
        //--- Merge's own rollback-on-failure path is structurally identical to
        //--- Sequence's (already proven by construction in Task 5's
        //--- `a_failing_step_rolls_back_every_step_already_applied`): every
        //--- collected removal is applied in reverse on the first `Err`. It is
        //--- not re-proven with an injected failure here because, given the
        //--- well-formed `ids`/`at_index` Merge always supplies itself,
        //--- `VecDocument::import_pages` cannot fail — there is no way to
        //--- reach that branch through Merge's public API against this fake.
        //--- This test instead scales the ordering check past two sources.
        let mut document = VecDocument::with_pages(1, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let source_a = VecDocument::with_pages(1, PageSize::A4);
        let source_b = VecDocument::with_pages(1, PageSize::LETTER);
        let source_c = VecDocument::with_pages(1, PageSize::A4);

        let merge: Merge<VecDocument> = Merge::new(vec![source_a, source_b, source_c]);
        let inverse = merge.apply(&mut document).unwrap();
        assert_eq!(document.page_count(), 4);
        let ids = document.page_ids();
        assert_eq!(
            document.page(ids[2]).unwrap().size,
            PageSize::LETTER,
            "the middle source must land between the other two, not at either end"
        );

        inverse.apply(&mut document).unwrap();
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }
}

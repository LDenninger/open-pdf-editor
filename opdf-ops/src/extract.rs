//! Extracting a contiguous range of pages into a target document.

use opdf_core::{Command, Document, Error, PageId, Result};

use crate::binding::BoundInverse;
use crate::remove_page::RemovePage;
use crate::sequence::Sequence;

//---------------------------------------------------------------------
// The result of an extraction
//---------------------------------------------------------------------

/// What [`extract_range`] added to the target, and how to take it back out.
///
/// Deliberately **not** a [`Command`]. The inverse of an extraction undoes the
/// append into the *target*, and it names the target's page ids; but
/// `Command::apply` and `UndoStack::apply` take any `&mut D` at all, and every
/// document implementation allocates page ids from zero, so those ids resolve
/// against the source too — against completely unrelated pages. Returning a
/// bare `Box<dyn Command<D>>` let a caller undo an extraction into the source
/// document and silently delete as many source pages as it had extracted.
///
/// Undoing therefore goes through [`Extraction::undo`], whose parameter is
/// named for the document it wants, and the command behind it is a
/// [`BoundInverse`] that rejects any other document even when it is handed
/// one — including via [`Extraction::into_target_command`], the only route
/// onto an undo stack.
pub struct Extraction<D: Document> {
    page_ids: Vec<PageId>,
    inverse: BoundInverse<D>,
}

impl<D: Document + 'static> Extraction<D> {
    /// The identities the extracted pages were given in the target, in the
    /// order they were appended.
    pub fn page_ids(&self) -> &[PageId] {
        &self.page_ids
    }

    /// Remove the extracted pages from `target`, restoring it to its
    /// pre-extraction state.
    ///
    /// Returns [`Error::Unsupported`] without touching anything if `target` is
    /// not the document the pages were extracted into.
    pub fn undo(&self, target: &mut D) -> Result<()> {
        self.inverse.apply(target)?;
        Ok(())
    }

    /// Take the inverse as a plain command, for the *target* document's undo
    /// stack.
    ///
    /// The command keeps its binding, so pushing it onto the wrong document's
    /// stack is an error at apply time rather than data loss.
    pub fn into_target_command(self) -> Box<dyn Command<D>> {
        Box::new(self.inverse)
    }
}

//---------------------------------------------------------------------
// Extraction
//---------------------------------------------------------------------

/// Copy pages `start_index..end_index` from `source` into `target`,
/// appending them at `target`'s current end.
///
/// `source` is never mutated. `target` need not be empty — extraction
/// always appends, so repeated extractions accumulate rather than
/// overwrite. The returned [`Extraction`] undoes the append into `target`,
/// and only into `target`.
pub fn extract_range<D: Document + 'static>(source: &D, target: &mut D, start_index: usize, end_index: usize) -> Result<Extraction<D>> {
    let page_ids = source.page_ids();
    if end_index > page_ids.len() {
        return Err(Error::IndexOutOfBounds {
            index: end_index,
            page_count: page_ids.len(),
        });
    }
    //--- Both ends are individually in range here, so an inverted range is
    //--- still possible and would panic on the slice below rather than
    //--- returning the Result this API promises.
    if start_index > end_index {
        return Err(Error::InvalidRange {
            start: start_index,
            end: end_index,
        });
    }
    let ids = &page_ids[start_index..end_index];
    let at_index = target.page_count();
    let new_ids = target.import_pages(source, ids, at_index)?;
    let removals: Vec<Box<dyn Command<D>>> = new_ids.iter().map(|&page| Box::new(RemovePage { page }) as Box<dyn Command<D>>).collect();
    let inverse: Box<dyn Command<D>> = Box::new(Sequence::new("Undo extraction".to_string(), removals));
    //--- bound to `target`, which the borrow checker guarantees is not `source` ---
    Ok(Extraction {
        page_ids: new_ids,
        inverse: BoundInverse::new(target, inverse),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn extraction_copies_the_range_without_touching_the_source() {
        let source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();

        extract_range(&source, &mut target, 1, 3).unwrap();

        assert_eq!(target.page_count(), 2);
        assert_eq!(source.page_count(), 4, "extraction must not mutate the source");
    }

    #[test]
    fn extraction_appends_rather_than_overwrites_an_existing_target() {
        let source = VecDocument::with_pages(2, PageSize::A4);
        let mut target = VecDocument::with_pages(1, PageSize::LETTER);

        extract_range(&source, &mut target, 0, 2).unwrap();

        assert_eq!(target.page_count(), 3);
    }

    #[test]
    fn the_inverse_removes_exactly_what_was_extracted() {
        let source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();
        let before = DocumentSnapshot::of(&target).unwrap();

        let extraction = extract_range(&source, &mut target, 1, 3).unwrap();
        assert_eq!(target.page_count(), 2);
        assert_eq!(extraction.page_ids().len(), 2, "the extraction must report the identities it created");

        extraction.undo(&mut target).unwrap();
        assert_eq!(DocumentSnapshot::of(&target).unwrap().pages, before.pages);
    }

    /// A user-typed range such as `5-2` reaches this function with both ends
    /// individually valid. Before the guard, the slice at the end of the
    /// bounds check panicked and took the process down.
    #[test]
    fn an_inverted_range_is_rejected_rather_than_panicking() {
        let source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();
        let before = DocumentSnapshot::of(&target).unwrap();

        let result = extract_range(&source, &mut target, 3, 1);

        assert!(
            matches!(result, Err(Error::InvalidRange { start: 3, end: 1 })),
            "an inverted range must be a recoverable error"
        );
        assert_eq!(DocumentSnapshot::of(&target).unwrap().pages, before.pages);
    }

    /// The degenerate-but-legal case, pinned so the guard cannot be
    /// tightened into `>=` and start rejecting an empty extraction.
    #[test]
    fn an_empty_range_extracts_nothing_and_succeeds() {
        let source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();

        extract_range(&source, &mut target, 2, 2).unwrap();

        assert_eq!(target.page_count(), 0);
    }

    /// A start beyond the document is caught by the inverted-range guard
    /// rather than by the bounds check, because `end_index` is clamped
    /// first — verify it is caught at all, which is what matters.
    #[test]
    fn a_start_beyond_the_document_is_rejected_rather_than_panicking() {
        let source = VecDocument::with_pages(2, PageSize::A4);
        let mut target = VecDocument::new();

        let result = extract_range(&source, &mut target, 9, 2);

        assert!(result.is_err());
    }

    /// F12 reproduction. Every `VecDocument` allocates page ids from zero, so
    /// the ids the extraction created in `target` collide with ids that name
    /// entirely different, live pages in `source`. Applying the extraction's
    /// inverse to the source used to succeed and delete two source pages —
    /// measured, 4 pages down to 2 — with the type system unable to object
    /// because both documents are the same `D`.
    #[test]
    fn the_inverse_refuses_to_apply_to_the_source_document() {
        let mut source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();
        let before = DocumentSnapshot::of(&source).unwrap();

        let extraction = extract_range(&source, &mut target, 1, 3).unwrap();
        let result = extraction.undo(&mut source);

        assert!(result.is_err(), "an inverse built against the target must not apply to the source");
        assert_eq!(
            DocumentSnapshot::of(&source).unwrap().pages,
            before.pages,
            "the rejected inverse must not have removed a single source page"
        );
    }

    /// The same guard has to survive being taken out as a plain command, which
    /// is the only shape an undo stack can hold and therefore the only shape
    /// that reaches production.
    #[test]
    fn the_inverse_still_refuses_the_source_once_it_is_a_plain_command() {
        let mut source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();
        let before = DocumentSnapshot::of(&source).unwrap();

        let command = extract_range(&source, &mut target, 1, 3).unwrap().into_target_command();

        assert!(command.apply(&mut source).is_err(), "a bare command must carry the binding too");
        assert_eq!(DocumentSnapshot::of(&source).unwrap().pages, before.pages);
        command.apply(&mut target).unwrap();
        assert_eq!(target.page_count(), 0, "the guard must not stand in the way of the legitimate undo");
    }

    /// A third, unrelated document is the case a page-id or page-count check
    /// would wave through: it looks exactly like the target did.
    #[test]
    fn the_inverse_refuses_a_look_alike_third_document() {
        let source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();
        let mut look_alike = VecDocument::new();

        let extraction = extract_range(&source, &mut target, 0, 4).unwrap();
        let other = extract_range(&source, &mut look_alike, 0, 4).unwrap();
        assert_eq!(
            extraction.page_ids(),
            other.page_ids(),
            "the two targets must be indistinguishable by page id for this test to mean anything"
        );

        assert!(
            extraction.undo(&mut look_alike).is_err(),
            "identical page ids must not make one document pass for the other"
        );
        assert_eq!(look_alike.page_count(), 4);
    }

    #[test]
    fn an_out_of_range_end_leaves_the_target_untouched() {
        let source = VecDocument::with_pages(2, PageSize::A4);
        let mut target = VecDocument::new();
        let before = DocumentSnapshot::of(&target).unwrap();

        let result = extract_range(&source, &mut target, 0, 99);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&target).unwrap().pages, before.pages);
    }
}

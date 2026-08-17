//! Splitting a document at a page boundary.

use opdf_core::{Command, Document, Error, Result};

use crate::binding::BoundInverse;
use crate::remove_page::RemovePage;
use crate::sequence::Sequence;

/// Split `document` at `boundary_index`: every page from `boundary_index`
/// onward moves into `target`, which the caller supplies already
/// constructed (normally empty).
///
/// This is a plain function rather than a [`Command`], because it mutates
/// two documents at once and [`Command::apply`] only ever receives one.
/// Returns the inverse of the removal from `document` — applying it
/// restores `document` to its pre-split state exactly, ids included, via
/// [`RemovePage`]'s `RestorePage` inverse. It does not undo `target`'s
/// population; a caller undoing a split discards `target` rather than
/// asking it to undo itself.
///
/// The inverse is a [`BoundInverse`] tied to `document`. Both documents are
/// the same type `D` and both allocate page ids from zero, so without the
/// binding the returned inverse would be accepted by `target` as readily as by
/// `document` — naming, there, pages it has no business restoring.
///
/// If the import into `target` succeeds but the following removal from
/// `document` fails — which, given page ids read fresh from `document`
/// immediately beforehand, should not happen — `target` is left holding
/// pages not removed from `document`. A caller that sees this function
/// return an error should discard `target`.
pub fn split_at<D: Document + 'static>(document: &mut D, target: &mut D, boundary_index: usize) -> Result<Box<dyn Command<D>>> {
    let page_ids = document.page_ids();
    if boundary_index > page_ids.len() {
        return Err(Error::IndexOutOfBounds {
            index: boundary_index,
            page_count: page_ids.len(),
        });
    }
    let tail_ids = &page_ids[boundary_index..];
    let at_index = target.page_count();
    target.import_pages(document, tail_ids, at_index)?;

    let removals: Vec<Box<dyn Command<D>>> = tail_ids.iter().map(|&page| Box::new(RemovePage { page }) as Box<dyn Command<D>>).collect();
    let inverse = Sequence::new("Split off the tail".to_string(), removals).apply(document)?;
    Ok(Box::new(BoundInverse::new(document, inverse)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn splitting_moves_the_tail_into_the_target() {
        let mut document = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();

        split_at(&mut document, &mut target, 2).unwrap();

        assert_eq!(document.page_count(), 2);
        assert_eq!(target.page_count(), 2);
    }

    #[test]
    fn the_inverse_restores_the_source_document_exactly() {
        let mut document = VecDocument::with_pages(4, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let mut target = VecDocument::new();

        let inverse = split_at(&mut document, &mut target, 2).unwrap();
        inverse.apply(&mut document).unwrap();

        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "restore_page makes undoing the document-side removal exact, ids included"
        );
    }

    /// The mirror of the extraction defect: the inverse restores `document`'s
    /// pages, and `target`'s ids overlap `document`'s entirely, so `target`
    /// must not be able to accept it.
    #[test]
    fn the_inverse_refuses_to_apply_to_the_split_off_target() {
        let mut document = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();

        let inverse = split_at(&mut document, &mut target, 2).unwrap();
        let before = DocumentSnapshot::of(&target).unwrap();

        assert!(
            inverse.apply(&mut target).is_err(),
            "an inverse that restores the source's pages must not be applicable to the target"
        );
        assert_eq!(DocumentSnapshot::of(&target).unwrap().pages, before.pages);
    }

    #[test]
    fn a_boundary_at_the_end_moves_nothing() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let mut target = VecDocument::new();

        split_at(&mut document, &mut target, 3).unwrap();

        assert_eq!(document.page_count(), 3);
        assert_eq!(target.page_count(), 0);
    }

    #[test]
    fn a_boundary_past_the_end_leaves_the_document_untouched() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let mut target = VecDocument::new();

        let result = split_at(&mut document, &mut target, 99);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
        assert_eq!(target.page_count(), 0);
    }
}

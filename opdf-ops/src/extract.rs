//! Extracting a contiguous range of pages into a target document.

use opdf_core::{Command, Document, Error, Result};

use crate::remove_page::RemovePage;
use crate::sequence::Sequence;

/// Copy pages `start_index..end_index` from `source` into `target`,
/// appending them at `target`'s current end.
///
/// `source` is never mutated. `target` need not be empty — extraction
/// always appends, so repeated extractions accumulate rather than
/// overwrite. Returns the inverse of the append into `target`.
pub fn extract_range<D: Document + 'static>(source: &D, target: &mut D, start_index: usize, end_index: usize) -> Result<Box<dyn Command<D>>> {
    let page_ids = source.page_ids();
    if end_index > page_ids.len() {
        return Err(Error::IndexOutOfBounds {
            index: end_index,
            page_count: page_ids.len(),
        });
    }
    let ids = &page_ids[start_index..end_index];
    let at_index = target.page_count();
    let new_ids = target.import_pages(source, ids, at_index)?;
    let removals: Vec<Box<dyn Command<D>>> = new_ids.into_iter().map(|page| Box::new(RemovePage { page }) as Box<dyn Command<D>>).collect();
    Ok(Box::new(Sequence::new("Undo extraction".to_string(), removals)))
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

        let inverse = extract_range(&source, &mut target, 1, 3).unwrap();
        assert_eq!(target.page_count(), 2);

        inverse.apply(&mut target).unwrap();
        assert_eq!(DocumentSnapshot::of(&target).unwrap().pages, before.pages);
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

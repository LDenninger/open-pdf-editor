//! Tying a command to the one document it is valid against.
//!
//! Most commands in this crate are produced by, and applied to, a single
//! document, so the only thing that can go wrong is a stale [`opdf_core::PageId`],
//! which the document itself rejects. The cross-document operations —
//! [`crate::extract_range`] and [`crate::split_at`] — are different: they touch
//! two documents of the *same* type `D`, and the inverse they hand back is
//! valid against exactly one of them. `Command::apply` accepts any `&mut D`,
//! and every document implementation allocates page ids from its own counter
//! starting at zero, so an inverse applied to the wrong document does not fail
//! — its page ids resolve, against entirely unrelated pages, and it deletes
//! them.
//!
//! [`BoundInverse`] closes that hole: it records which document the inverse was
//! built against and refuses to apply to any other one.

use std::ptr;

use opdf_core::{Command, Document, Error, Result};

//---------------------------------------------------------------------
// Document identity
//---------------------------------------------------------------------

/// The identity of one live document.
///
/// `opdf_core::Document` exposes no identity of its own — `revision` is a
/// change counter that every implementation starts at zero, and page ids are
/// allocated per document from zero as well, so neither distinguishes two
/// documents of the same type. The address of the document value is the only
/// discriminator available to this crate, and it is a sound one for the case
/// that matters: a cross-document operation borrows both documents at once, so
/// the borrow checker guarantees their addresses differ at the moment the
/// binding is captured.
///
/// The cost of using an address is that moving a document in memory — into a
/// `Box`, or through a `Vec` reallocation — invalidates bindings taken before
/// the move. That direction is safe: a stale binding makes
/// [`BoundInverse::apply`] fail loudly rather than mutate the wrong document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DocumentBinding(usize);

impl DocumentBinding {
    /// Capture the identity of `document`.
    pub fn of<D: Document>(document: &D) -> Self {
        Self(ptr::from_ref(document) as usize)
    }

    /// Whether `document` is the document this binding was captured from.
    pub fn matches<D: Document>(self, document: &D) -> bool {
        self == Self::of(document)
    }
}

//---------------------------------------------------------------------
// The guarded command
//---------------------------------------------------------------------

/// A command that applies only to the document it was built against.
///
/// Applying it to any other document returns [`Error::Unsupported`] and leaves
/// that document untouched. The command it hands back in turn — the redo of
/// this undo — carries the same binding, so the guard survives a full
/// undo/redo cycle rather than being shed on the first application.
pub struct BoundInverse<D: Document> {
    binding: DocumentBinding,
    inverse: Box<dyn Command<D>>,
}

impl<D: Document + 'static> BoundInverse<D> {
    /// Bind `inverse` to `document`.
    pub fn new(document: &D, inverse: Box<dyn Command<D>>) -> Self {
        Self {
            binding: DocumentBinding::of(document),
            inverse,
        }
    }

    /// The document this command is valid against.
    pub fn binding(&self) -> DocumentBinding {
        self.binding
    }
}

impl<D: Document + 'static> Command<D> for BoundInverse<D> {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        if !self.binding.matches(document) {
            return Err(Error::Unsupported(format!(
                "'{}' was built against a different document and must not be applied to this one: its page ids name pages of the document it came from",
                self.inverse.label()
            )));
        }
        let redo = self.inverse.apply(document)?;
        Ok(Box::new(Self {
            binding: self.binding,
            inverse: redo,
        }))
    }

    fn label(&self) -> String {
        self.inverse.label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    use crate::remove_page::RemovePage;

    #[test]
    fn a_bound_command_applies_to_the_document_it_was_bound_to() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let page = document.page_ids()[1];
        let bound = BoundInverse::new(&document, Box::new(RemovePage { page }));

        bound.apply(&mut document).unwrap();

        assert_eq!(document.page_count(), 2, "the guard must not stand in the way of the legitimate application");
    }

    /// Both documents allocate page ids from zero, so the removal below names a
    /// live page of `other` too. Without the binding it would succeed there.
    #[test]
    fn a_bound_command_refuses_a_different_document_of_the_same_type() {
        let document = VecDocument::with_pages(3, PageSize::A4);
        let mut other = VecDocument::with_pages(3, PageSize::A4);
        let before = DocumentSnapshot::of(&other).unwrap();
        let page = document.page_ids()[1];
        let bound = BoundInverse::new(&document, Box::new(RemovePage { page }));

        let result = bound.apply(&mut other);

        assert!(
            matches!(result, Err(Error::Unsupported(_))),
            "a mismatched document must be rejected, not mutated"
        );
        assert_eq!(
            DocumentSnapshot::of(&other).unwrap().pages,
            before.pages,
            "the rejected command must not have removed a page"
        );
    }

    /// A guard that is shed on first application protects the undo but not the
    /// redo that follows it, which is the same hazard one step later.
    #[test]
    fn the_command_returned_by_applying_one_carries_the_same_binding() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let mut other = VecDocument::with_pages(3, PageSize::A4);
        let page = document.page_ids()[1];
        let bound = BoundInverse::new(&document, Box::new(RemovePage { page }));

        let redo = bound.apply(&mut document).unwrap();

        assert!(redo.apply(&mut other).is_err(), "the redo of a bound undo must be bound too");
        assert_eq!(other.page_count(), 3);
    }

    #[test]
    fn the_label_is_the_wrapped_command_s_own() {
        let document = VecDocument::with_pages(1, PageSize::A4);
        let page = document.page_ids()[0];
        let bound: Box<dyn Command<VecDocument>> = Box::new(BoundInverse::new(&document, Box::new(RemovePage { page })));

        assert_eq!(bound.label(), "Remove page#0", "the guard is invisible to the undo menu");
    }
}

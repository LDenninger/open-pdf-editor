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

use opdf_core::{Command, Document, DocumentId, Error, Result};

//---------------------------------------------------------------------
// Document identity
//---------------------------------------------------------------------

/// The identity of one live document.
///
/// A thin wrapper over [`DocumentId`], which every [`Document`] mints once at
/// construction and keeps for its whole life. The wrapper exists so that this
/// crate's guard reads as a *binding* — a claim about which document a command
/// belongs to — rather than as an incidental equality of two ids.
///
/// The identity travels with the document value, so a binding survives the
/// document being moved: boxed, pushed through a reallocating `Vec`, or
/// returned by value from the function that opened it. That is the whole
/// difference from what this type used to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DocumentBinding(DocumentId);

impl DocumentBinding {
    /// Capture the identity of `document`.
    pub fn of<D: Document + ?Sized>(document: &D) -> Self {
        Self(document.id())
    }

    /// Whether `document` is the document this binding was captured from.
    pub fn matches<D: Document + ?Sized>(self, document: &D) -> bool {
        self == Self::of(document)
    }

    /// The identity this binding names.
    pub const fn document(self) -> DocumentId {
        self.0
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
pub struct BoundInverse<D: Document + ?Sized> {
    binding: DocumentBinding,
    inverse: Box<dyn Command<D>>,
}

impl<D: Document + ?Sized + 'static> BoundInverse<D> {
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

impl<D: Document + ?Sized + 'static> Command<D> for BoundInverse<D> {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        if !self.binding.matches(document) {
            return Err(Error::Unsupported(format!(
                "'{}' was built against {} and must not be applied to {}: its page ids name pages of the document it came from",
                self.inverse.label(),
                self.binding.document(),
                document.id()
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

    /// The cost the address-based binding could not avoid.
    ///
    /// A `DocumentBinding` built from `&document as *const _` names a stack
    /// slot, not a document. Moving the value — into a `Box`, through a `Vec`
    /// reallocation, or by the plain rebinding below — leaves the binding
    /// pointing at an address the document no longer occupies, and the
    /// legitimate undo is refused.
    #[test]
    fn a_binding_survives_the_document_moving_in_memory() {
        let source = VecDocument::with_pages(4, PageSize::A4);
        let mut target = VecDocument::new();
        let extraction = crate::extract_range(&source, &mut target, 0, 2).unwrap();

        //--- move the document; an address-based binding breaks here ---
        let mut moved = target;

        assert!(
            extraction.undo(&mut moved).is_ok(),
            "an identity-based binding must still recognise the document after it moves"
        );
        assert_eq!(moved.page_count(), 0, "the undo must actually have removed the extracted pages");
    }

    /// The same requirement one indirection further out, which is the shape the
    /// shell actually holds: a document behind a `Box`.
    #[test]
    fn a_binding_survives_the_document_being_boxed() {
        let document = VecDocument::with_pages(3, PageSize::A4);
        let page = document.page_ids()[1];
        let bound = BoundInverse::new(&document, Box::new(RemovePage { page }));

        let mut boxed = Box::new(document);

        bound.apply(&mut boxed).unwrap();
        assert_eq!(boxed.page_count(), 2, "boxing a document must not invalidate a command bound to it");
    }

    #[test]
    fn the_label_is_the_wrapped_command_s_own() {
        let document = VecDocument::with_pages(1, PageSize::A4);
        let page = document.page_ids()[0];
        let bound: Box<dyn Command<VecDocument>> = Box::new(BoundInverse::new(&document, Box::new(RemovePage { page })));

        assert_eq!(bound.label(), "Remove page#0", "the guard is invisible to the undo menu");
    }
}

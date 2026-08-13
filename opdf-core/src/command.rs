//! The mutation contract: every change to a document is an invertible command.

use crate::Result;
use crate::document::Document;

/// A single invertible mutation.
///
/// Applying a command returns the command that undoes it. The inverse is
/// produced at apply time because it usually depends on state the command
/// replaced, such as a page's previous rotation.
pub trait Command<D: Document> {
    /// Apply this change, returning the command that reverses it.
    ///
    /// On failure the document must be left exactly as it was found.
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>>;

    /// Short description for the undo menu, in sentence case: `"Rotate page 3"`.
    fn label(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::VecDocument;
    use crate::page::{PageId, Rotation};

    /// Minimal command proving the trait can express an invertible change.
    struct SetRotation {
        page: PageId,
        rotation: Rotation,
    }

    impl Command<VecDocument> for SetRotation {
        fn apply(&self, document: &mut VecDocument) -> Result<Box<dyn Command<VecDocument>>> {
            let previous = document.page(self.page)?.rotation;
            document.set_rotation(self.page, self.rotation)?;
            Ok(Box::new(SetRotation {
                page: self.page,
                rotation: previous,
            }))
        }

        fn label(&self) -> String {
            format!("Rotate {} to {} degrees", self.page, self.rotation.degrees())
        }
    }

    #[test]
    fn applying_the_returned_inverse_restores_the_original_state() {
        let mut document = VecDocument::with_pages(1, crate::page::PageSize::A4);
        let page = document.page_ids()[0];

        let command = SetRotation {
            page,
            rotation: Rotation::Quarter,
        };
        let inverse = command.apply(&mut document).unwrap();
        assert_eq!(document.page(page).unwrap().rotation, Rotation::Quarter);

        inverse.apply(&mut document).unwrap();
        assert_eq!(document.page(page).unwrap().rotation, Rotation::None);
    }

    #[test]
    fn labels_describe_the_change_for_the_undo_menu() {
        let command = SetRotation {
            page: PageId::new(3),
            rotation: Rotation::Half,
        };
        assert_eq!(command.label(), "Rotate page#3 to 180 degrees");
    }
}

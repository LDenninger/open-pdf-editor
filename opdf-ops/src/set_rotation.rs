//! Setting a page's rotation.

use opdf_core::{Command, Document, PageId, Result, Rotation};

/// Replace a page's rotation.
pub struct SetRotation {
    /// The page to rotate.
    pub page: PageId,
    /// The rotation to apply.
    pub rotation: Rotation,
}

impl<D: Document + ?Sized> Command<D> for SetRotation {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;

    #[test]
    fn the_inverse_restores_the_whole_page_list() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let page = document.page_ids()[0];
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = SetRotation {
            page,
            rotation: Rotation::Quarter,
        };
        let inverse = command.apply(&mut document).unwrap();
        assert_eq!(document.page(page).unwrap().rotation, Rotation::Quarter);

        inverse.apply(&mut document).unwrap();
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn an_unknown_page_leaves_the_document_untouched() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let command = SetRotation {
            page: PageId::new(9999),
            rotation: Rotation::Half,
        };

        let result = command.apply(&mut document);

        assert!(result.is_err());
        assert_eq!(DocumentSnapshot::of(&document).unwrap().pages, before.pages);
    }

    #[test]
    fn labels_describe_the_change() {
        let command: Box<dyn Command<VecDocument>> = Box::new(SetRotation {
            page: PageId::new(3),
            rotation: Rotation::Half,
        });
        assert_eq!(command.label(), "Rotate page#3 to 180 degrees");
    }
}

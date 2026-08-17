//! The top menu bar and the actions it emits.
//!
//! The bar itself decides nothing: it returns a [`MenuAction`] and the application
//! shell applies it. That keeps the action vocabulary in one place, testable, and
//! shared with the toolbar and the keyboard shortcuts.

/// Something the user asked for from the menu bar, toolbar, or keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuAction {
    /// Open a document from disk. Not implemented until Track A lands.
    OpenDocument,
    /// Write the document back to where it was opened from, as an incremental update.
    Save,
    /// Write the document to a path the user picks, as an incremental update.
    SaveAs,
    /// Close the open document and show the empty state.
    CloseDocument,
    /// Reverse the most recent edit.
    Undo,
    /// Reapply the most recently undone edit.
    Redo,
    /// Turn the current page a quarter turn clockwise. A document edit, unlike
    /// [`MenuAction::RotateViewClockwise`].
    RotatePageClockwise,
    /// Exit the application.
    Quit,
    /// Replace the document with a freshly generated synthetic one of this many pages.
    GenerateSynthetic(usize),
    /// Step one stop up the zoom ladder.
    ZoomIn,
    /// Step one stop down the zoom ladder.
    ZoomOut,
    /// Return to 100%.
    ZoomActual,
    /// Fit the content box to the viewport width and keep it fitted.
    FitWidth,
    /// Fit the current page entirely on screen and keep it fitted.
    FitPage,
    /// Jump to the first page.
    FirstPage,
    /// Jump to the last page.
    LastPage,
    /// Jump to the next page.
    NextPage,
    /// Jump to the previous page.
    PreviousPage,
    /// Show or hide the thumbnail rail.
    ToggleRail,
    /// Rotate the view a quarter turn clockwise. Not a document edit.
    RotateViewClockwise,
    /// Rotate the view a quarter turn counter-clockwise. Not a document edit.
    RotateViewCounterClockwise,
    /// Show the about box.
    ShowAbout,
}

impl MenuAction {
    /// A short sentence-case description, for the menu entry and for tests.
    pub fn label(&self) -> String {
        match self {
            Self::OpenDocument => "Open document".to_owned(),
            Self::Save => "Save".to_owned(),
            Self::SaveAs => "Save as…".to_owned(),
            Self::CloseDocument => "Close document".to_owned(),
            Self::Undo => "Undo".to_owned(),
            Self::Redo => "Redo".to_owned(),
            Self::RotatePageClockwise => "Rotate page clockwise".to_owned(),
            Self::Quit => "Quit".to_owned(),
            Self::GenerateSynthetic(page_count) => format!("Generate {page_count} synthetic pages"),
            Self::ZoomIn => "Zoom in".to_owned(),
            Self::ZoomOut => "Zoom out".to_owned(),
            Self::ZoomActual => "Actual size".to_owned(),
            Self::FitWidth => "Fit width".to_owned(),
            Self::FitPage => "Fit page".to_owned(),
            Self::FirstPage => "First page".to_owned(),
            Self::LastPage => "Last page".to_owned(),
            Self::NextPage => "Next page".to_owned(),
            Self::PreviousPage => "Previous page".to_owned(),
            Self::ToggleRail => "Toggle thumbnails".to_owned(),
            Self::RotateViewClockwise => "Rotate view clockwise".to_owned(),
            Self::RotateViewCounterClockwise => "Rotate view counter-clockwise".to_owned(),
            Self::ShowAbout => "About opdf".to_owned(),
        }
    }
}

//---------------------------------------------------------------------
// The bar
//---------------------------------------------------------------------

/// Draw the menu bar, returning the action the user chose, if any.
pub fn show_menu_bar(ui: &mut egui::Ui) -> Option<MenuAction> {
    let mut chosen = None;
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui
                .button(format!("{}  {}", crate::icons::OPEN_DOCUMENT, MenuAction::OpenDocument.label()))
                .clicked()
            {
                chosen = Some(MenuAction::OpenDocument);
                ui.close();
            }
            ui.separator();
            for action in [MenuAction::Save, MenuAction::SaveAs] {
                if ui.button(action.label()).clicked() {
                    chosen = Some(action);
                    ui.close();
                }
            }
            ui.separator();
            for page_count in [12_usize, 120, 1_200] {
                let action = MenuAction::GenerateSynthetic(page_count);
                if ui.button(action.label()).clicked() {
                    chosen = Some(action);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(MenuAction::CloseDocument.label()).clicked() {
                chosen = Some(MenuAction::CloseDocument);
                ui.close();
            }
            if ui.button(MenuAction::Quit.label()).clicked() {
                chosen = Some(MenuAction::Quit);
                ui.close();
            }
        });

        ui.menu_button("Edit", |ui| {
            for action in [MenuAction::Undo, MenuAction::Redo] {
                if ui.button(action.label()).clicked() {
                    chosen = Some(action);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(MenuAction::RotatePageClockwise.label()).clicked() {
                chosen = Some(MenuAction::RotatePageClockwise);
                ui.close();
            }
        });

        ui.menu_button("View", |ui| {
            for action in [
                MenuAction::ZoomIn,
                MenuAction::ZoomOut,
                MenuAction::ZoomActual,
                MenuAction::FitWidth,
                MenuAction::FitPage,
            ] {
                if ui.button(action.label()).clicked() {
                    chosen = Some(action);
                    ui.close();
                }
            }
            ui.separator();
            for action in [MenuAction::RotateViewClockwise, MenuAction::RotateViewCounterClockwise, MenuAction::ToggleRail] {
                if ui.button(action.label()).clicked() {
                    chosen = Some(action);
                    ui.close();
                }
            }
        });

        ui.menu_button("Go", |ui| {
            for action in [MenuAction::FirstPage, MenuAction::PreviousPage, MenuAction::NextPage, MenuAction::LastPage] {
                if ui.button(action.label()).clicked() {
                    chosen = Some(action);
                    ui.close();
                }
            }
        });

        ui.menu_button("Help", |ui| {
            if ui.button(MenuAction::ShowAbout.label()).clicked() {
                chosen = Some(MenuAction::ShowAbout);
                ui.close();
            }
        });
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const EVERY_ACTION: [MenuAction; 22] = [
        MenuAction::OpenDocument,
        MenuAction::Save,
        MenuAction::SaveAs,
        MenuAction::CloseDocument,
        MenuAction::Undo,
        MenuAction::Redo,
        MenuAction::RotatePageClockwise,
        MenuAction::Quit,
        MenuAction::GenerateSynthetic(12),
        MenuAction::ZoomIn,
        MenuAction::ZoomOut,
        MenuAction::ZoomActual,
        MenuAction::FitWidth,
        MenuAction::FitPage,
        MenuAction::FirstPage,
        MenuAction::LastPage,
        MenuAction::NextPage,
        MenuAction::PreviousPage,
        MenuAction::ToggleRail,
        MenuAction::RotateViewClockwise,
        MenuAction::RotateViewCounterClockwise,
        MenuAction::ShowAbout,
    ];

    #[test]
    fn labels_every_action_distinctly() {
        let labels: HashSet<String> = EVERY_ACTION.iter().map(MenuAction::label).collect();
        assert_eq!(
            labels.len(),
            EVERY_ACTION.len(),
            "two menu entries sharing a label is a menu the user cannot read"
        );
    }

    #[test]
    fn labels_are_non_empty_and_sentence_case() {
        for action in EVERY_ACTION {
            let label = action.label();
            assert!(!label.is_empty(), "{action:?} has no label");
            let first = label.chars().next().unwrap();
            assert!(first.is_uppercase(), "{label:?} must be sentence case");
            assert!(!label.ends_with('.'), "{label:?} must not end with a full stop");
        }
    }

    #[test]
    fn names_the_page_count_in_a_synthetic_generation_label() {
        assert_eq!(MenuAction::GenerateSynthetic(120).label(), "Generate 120 synthetic pages");
    }
}

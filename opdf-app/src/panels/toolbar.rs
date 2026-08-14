//! The toolbar strip: navigation, zoom, fit modes, and the rail toggle.
//!
//! Emits the same [`MenuAction`] vocabulary as the menu bar, so the application
//! shell has exactly one place that interprets a user request.

use crate::panels::menu_bar::MenuAction;
use crate::theme::Theme;
use crate::viewer::ViewerState;

/// What the toolbar produced this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolbarOutcome {
    /// A standard action, shared with the menu bar and keyboard shortcuts.
    Action(MenuAction),
    /// A jump to a specific zero-based page index, typed into the page field.
    JumpToPage(usize),
}

/// Render a zoom factor as the percentage a PDF viewer shows, e.g. `"125%"`.
///
/// Rounds to the nearest whole percent, and never reports `0%` for a very small
/// but non-zero zoom, because a viewer claiming zero zoom while showing content
/// is simply wrong.
pub fn format_zoom_percentage(zoom: f32) -> String {
    if !zoom.is_finite() || zoom <= 0.0 {
        return "100%".to_owned();
    }
    format!("{}%", ((zoom * 100.0).round() as i64).max(1))
}

/// Render the current position as `"page of count"`, e.g. `"7 / 240"`.
///
/// An empty document reports `"— / 0"` rather than `"0 / 0"`, so that "no
/// document" is visibly different from "the zeroth page".
pub fn format_page_position(current: Option<usize>, page_count: usize) -> String {
    match current {
        Some(index) if page_count > 0 => format!("{} / {page_count}", index + 1),
        _ => format!("— / {page_count}"),
    }
}

/// Interpret what the user typed into the page field as a zero-based page index.
///
/// Accepts a one-based decimal number with surrounding whitespace. Rejects
/// anything else, including zero, a negative, a number past the end of the
/// document, and an empty field.
pub fn parse_page_entry(entry: &str, page_count: usize) -> Option<usize> {
    let number: usize = entry.trim().parse().ok()?;
    if number == 0 || number > page_count {
        return None;
    }
    Some(number - 1)
}

//---------------------------------------------------------------------
// The toolbar widget
//---------------------------------------------------------------------

/// Draw the toolbar, returning what the user asked for.
///
/// `page_entry` is the text buffer behind the page-number field, owned by the
/// application shell so it survives between frames.
pub fn show_toolbar(ui: &mut egui::Ui, state: &ViewerState, page_entry: &mut String, theme: &Theme) -> Option<ToolbarOutcome> {
    let mut outcome = None;
    ui.horizontal(|ui| {
        ui.set_height(theme.toolbar_height);

        //--- rail ---
        if ui.button(crate::icons::TOGGLE_RAIL).on_hover_text(MenuAction::ToggleRail.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::ToggleRail));
        }
        ui.separator();

        //--- navigation ---
        if ui.button(crate::icons::PREVIOUS_PAGE).on_hover_text(MenuAction::PreviousPage.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::PreviousPage));
        }
        let field = ui.add(egui::TextEdit::singleline(page_entry).desired_width(44.0).horizontal_align(egui::Align::Center));
        let submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if let Some(index) = parse_page_entry(page_entry, state.page_count()).filter(|_| submitted) {
            outcome = Some(ToolbarOutcome::JumpToPage(index));
        }
        ui.label(format!("of {}", state.page_count()));
        if ui.button(crate::icons::NEXT_PAGE).on_hover_text(MenuAction::NextPage.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::NextPage));
        }
        ui.separator();

        //--- zoom ---
        if ui.button(crate::icons::ZOOM_OUT).on_hover_text(MenuAction::ZoomOut.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::ZoomOut));
        }
        if ui
            .button(format_zoom_percentage(state.zoom))
            .on_hover_text(MenuAction::ZoomActual.label())
            .clicked()
        {
            outcome = Some(ToolbarOutcome::Action(MenuAction::ZoomActual));
        }
        if ui.button(crate::icons::ZOOM_IN).on_hover_text(MenuAction::ZoomIn.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::ZoomIn));
        }
        ui.separator();

        //--- fit and rotate ---
        if ui.button(crate::icons::FIT_WIDTH).on_hover_text(MenuAction::FitWidth.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::FitWidth));
        }
        if ui.button(crate::icons::FIT_PAGE).on_hover_text(MenuAction::FitPage.label()).clicked() {
            outcome = Some(ToolbarOutcome::Action(MenuAction::FitPage));
        }
        if ui
            .button(crate::icons::ROTATE_VIEW)
            .on_hover_text(MenuAction::RotateViewClockwise.label())
            .clicked()
        {
            outcome = Some(ToolbarOutcome::Action(MenuAction::RotateViewClockwise));
        }
    });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_zoom_as_a_whole_percentage() {
        assert_eq!(format_zoom_percentage(1.0), "100%");
        assert_eq!(format_zoom_percentage(0.25), "25%");
        assert_eq!(format_zoom_percentage(1.335), "134%");
    }

    #[test]
    fn never_reports_zero_percent_while_showing_content() {
        assert_eq!(format_zoom_percentage(0.001), "1%");
    }

    #[test]
    fn falls_back_to_one_hundred_percent_for_a_degenerate_zoom() {
        assert_eq!(format_zoom_percentage(f32::NAN), "100%");
        assert_eq!(format_zoom_percentage(0.0), "100%");
        assert_eq!(format_zoom_percentage(-1.0), "100%");
    }

    #[test]
    fn formats_the_page_position_one_based() {
        assert_eq!(format_page_position(Some(0), 240), "1 / 240");
        assert_eq!(format_page_position(Some(239), 240), "240 / 240");
    }

    #[test]
    fn distinguishes_no_document_from_the_zeroth_page() {
        assert_eq!(format_page_position(None, 0), "— / 0");
        assert_eq!(format_page_position(Some(0), 0), "— / 0");
    }

    #[test]
    fn parses_a_one_based_page_number_into_a_zero_based_index() {
        assert_eq!(parse_page_entry("1", 10), Some(0));
        assert_eq!(parse_page_entry("10", 10), Some(9));
        assert_eq!(parse_page_entry("  7 ", 10), Some(6));
    }

    #[test]
    fn rejects_a_page_entry_that_names_no_page() {
        assert_eq!(parse_page_entry("0", 10), None, "PDF page numbers are one-based");
        assert_eq!(parse_page_entry("11", 10), None);
        assert_eq!(parse_page_entry("-3", 10), None);
        assert_eq!(parse_page_entry("", 10), None);
        assert_eq!(parse_page_entry("seven", 10), None);
        assert_eq!(parse_page_entry("3.5", 10), None);
        assert_eq!(parse_page_entry("1", 0), None, "an empty document has no page to jump to");
    }
}

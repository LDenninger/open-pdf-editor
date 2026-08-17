//! The bottom status bar: where in the document the user is, at what zoom, and how
//! much rendering is still outstanding.

use crate::panels::toolbar::{format_page_position, format_zoom_percentage};
use crate::theme::Theme;
use crate::tiles::TextureCache;
use crate::viewer::ViewerState;

/// A snapshot of the render loop's state, for display.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RenderStatus {
    /// Requests submitted but not yet answered.
    pub pending: usize,
    /// Textures currently cached.
    pub cached: usize,
    /// Bytes those textures occupy.
    pub used_bytes: usize,
    /// The cache's byte budget.
    pub budget_bytes: usize,
}

impl RenderStatus {
    /// Read the status out of a cache.
    pub fn of(cache: &TextureCache) -> Self {
        Self {
            pending: cache.pending_count(),
            cached: cache.len(),
            used_bytes: cache.used_bytes(),
            budget_bytes: cache.budget_bytes(),
        }
    }
}

/// A one-line summary of outstanding rendering, e.g. `"Rendering 6 pages"` or
/// `"Ready"`.
pub fn summarise_render_status(status: &RenderStatus) -> String {
    match status.pending {
        0 => "Ready".to_owned(),
        1 => "Rendering 1 page".to_owned(),
        pending => format!("Rendering {pending} pages"),
    }
}

/// Cache occupancy as mebibytes against the budget, e.g. `"148 / 256 MiB"`.
pub fn format_cache_usage(used_bytes: usize, budget_bytes: usize) -> String {
    const MEBIBYTE: usize = 1024 * 1024;
    format!("{} / {} MiB", used_bytes.div_ceil(MEBIBYTE), budget_bytes.div_ceil(MEBIBYTE))
}

//---------------------------------------------------------------------
// The status bar widget
//---------------------------------------------------------------------

/// Draw the status bar.
///
/// `last_error` is the most recent failure the user has not dismissed — an open
/// that did not happen, most often. It is shown here rather than in a modal
/// because the shell has no modal infrastructure, and because a failure that
/// changed nothing does not warrant interrupting the document that is still on
/// screen.
pub fn show_status_bar(ui: &mut egui::Ui, state: &ViewerState, status: &RenderStatus, last_error: Option<&str>, theme: &Theme) {
    ui.horizontal(|ui| {
        ui.set_height(theme.status_bar_height);
        ui.label(format_page_position(state.current_page(), state.page_count()));
        ui.separator();
        ui.label(format_zoom_percentage(state.zoom));
        ui.separator();
        match last_error {
            Some(message) => {
                ui.colored_label(theme.error_text, message);
            }
            None => {
                ui.colored_label(theme.text_muted, summarise_render_status(status));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.colored_label(theme.text_muted, format_cache_usage(status.used_bytes, status.budget_bytes));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_ready_when_nothing_is_outstanding() {
        assert_eq!(summarise_render_status(&RenderStatus::default()), "Ready");
    }

    #[test]
    fn pluralises_the_outstanding_page_count() {
        let one = RenderStatus {
            pending: 1,
            ..RenderStatus::default()
        };
        let many = RenderStatus {
            pending: 6,
            ..RenderStatus::default()
        };
        assert_eq!(summarise_render_status(&one), "Rendering 1 page");
        assert_eq!(summarise_render_status(&many), "Rendering 6 pages");
    }

    #[test]
    fn formats_cache_usage_against_its_budget() {
        assert_eq!(format_cache_usage(0, 256 * 1024 * 1024), "0 / 256 MiB");
        assert_eq!(format_cache_usage(147 * 1024 * 1024, 256 * 1024 * 1024), "147 / 256 MiB");
    }

    #[test]
    fn rounds_a_partial_mebibyte_up_so_a_used_cache_never_reads_as_empty() {
        assert_eq!(format_cache_usage(1, 1024 * 1024), "1 / 1 MiB");
    }

    #[test]
    fn reads_its_status_straight_out_of_a_cache() {
        let mut cache = TextureCache::new(4 * 1024 * 1024);
        cache.mark_pending(opdf_core::render::RenderRequest::new(opdf_core::page::PageId::new(0), 1, 1.0).unwrap());
        let status = RenderStatus::of(&cache);
        assert_eq!(status.pending, 1);
        assert_eq!(status.cached, 0);
        assert_eq!(status.budget_bytes, 4 * 1024 * 1024);
    }
}

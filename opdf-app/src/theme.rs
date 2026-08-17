//! The single source of colour, spacing, and typography for the shell.
//!
//! Every widget reads its colours and metrics from a [`Theme`] rather than
//! writing literals inline, so the interface can be restyled in one place and so
//! that a light variant can be added without hunting for scattered constants.

use egui::{Color32, Context, FontFamily, FontId, TextStyle, Vec2};

/// Colours, spacing, and metrics for the whole interface.
///
/// Distances suffixed `_pt` are in PDF points and are multiplied by the zoom
/// factor before being drawn. Everything else is in egui points — logical screen
/// units, independent of the display's pixel density.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    /// Background behind the menu bar, toolbar, and status bar.
    pub chrome_background: Color32,
    /// Background of the thumbnail rail and any docked side panel.
    pub panel_background: Color32,
    /// Background the pages sit on. Darker than the chrome, so paper reads as lit.
    pub canvas_background: Color32,
    /// Fill of a page that has been rasterized but whose tile is opaque white.
    pub page_paper: Color32,
    /// Fill of a page whose pixels have not arrived yet.
    pub page_placeholder: Color32,
    /// Hairline around every page rectangle.
    pub page_border: Color32,
    /// Drop shadow under a page rectangle.
    pub page_shadow: Color32,
    /// Selection and focus colour.
    pub accent: Color32,
    /// Default text colour.
    pub text_primary: Color32,
    /// Secondary text: status bar detail, thumbnail numbers.
    pub text_muted: Color32,
    /// Separator lines between chrome regions.
    pub separator: Color32,
    /// Standard inner padding for chrome regions, in egui points.
    pub gutter: f32,
    /// Vertical gap between consecutive pages, in PDF points.
    pub page_gap_pt: f32,
    /// Margin above the first page and below the last, in PDF points.
    pub canvas_margin_pt: f32,
    /// Corner radius for buttons and page rectangles.
    pub corner_radius: u8,
    /// Default width of the thumbnail rail, in egui points.
    pub rail_width: f32,
    /// Height of the toolbar strip, in egui points.
    pub toolbar_height: f32,
    /// Height of the status bar, in egui points.
    pub status_bar_height: f32,
}

impl Theme {
    /// The dark theme, which is the default: dense, low-chroma chrome so that page
    /// content is the brightest thing on screen.
    pub const fn dark() -> Self {
        Self {
            chrome_background: Color32::from_rgb(0x1b, 0x1d, 0x21),
            panel_background: Color32::from_rgb(0x17, 0x19, 0x1d),
            canvas_background: Color32::from_rgb(0x0e, 0x0f, 0x12),
            page_paper: Color32::from_rgb(0xff, 0xff, 0xff),
            page_placeholder: Color32::from_rgb(0x24, 0x26, 0x2b),
            page_border: Color32::from_rgb(0x3a, 0x3d, 0x45),
            page_shadow: Color32::from_black_alpha(0x66),
            accent: Color32::from_rgb(0x4d, 0x8d, 0xf0),
            text_primary: Color32::from_rgb(0xdf, 0xe2, 0xe8),
            text_muted: Color32::from_rgb(0x8b, 0x91, 0x9d),
            separator: Color32::from_rgb(0x2a, 0x2d, 0x33),
            gutter: 6.0,
            page_gap_pt: 14.0,
            canvas_margin_pt: 20.0,
            corner_radius: 2,
            rail_width: 168.0,
            toolbar_height: 32.0,
            status_bar_height: 22.0,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

//---------------------------------------------------------------------
// Installing the theme
//---------------------------------------------------------------------

/// Install `theme`'s fonts, colours, and spacing into `ctx`.
///
/// Call once at startup, and again whenever the theme changes. Installing the
/// Phosphor icon font is part of this, because an icon constant renders as a
/// missing glyph until the font is registered.
pub fn apply_theme(ctx: &Context, theme: &Theme) {
    //--- icon font, inserted just after the default proportional font ---
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    //--- a dense type ramp: this is a tool, not a document reader ---
    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(10.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(12.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(12.5, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(theme.gutter, theme.gutter * 0.5);
        style.spacing.button_padding = Vec2::new(theme.gutter, theme.gutter * 0.5);
        style.spacing.menu_margin = egui::Margin::same(4);
        style.spacing.scroll.bar_width = 10.0;
    });

    //--- colours ---
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = theme.chrome_background;
    visuals.window_fill = theme.panel_background;
    visuals.extreme_bg_color = theme.canvas_background;
    visuals.override_text_color = Some(theme.text_primary);
    visuals.selection.bg_fill = theme.accent.gamma_multiply(0.35);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, theme.accent);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, theme.separator);
    ctx.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_chrome_canvas_and_paper() {
        let theme = Theme::dark();
        assert_ne!(
            theme.chrome_background, theme.canvas_background,
            "chrome must read as distinct from the canvas behind the pages"
        );
        assert_ne!(
            theme.page_placeholder, theme.canvas_background,
            "an unrendered page must still read as a page, not as a hole in the canvas"
        );
    }

    #[test]
    fn installs_without_a_display() {
        let ctx = Context::default();
        let theme = Theme::dark();
        apply_theme(&ctx, &theme);
        let style = ctx.style();
        assert_eq!(
            style.spacing.item_spacing.x, theme.gutter,
            "apply_theme must push the theme's gutter into egui's spacing"
        );
    }
}

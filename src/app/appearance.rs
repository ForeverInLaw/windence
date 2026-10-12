use super::*;
use gpui_kit::component::theme::ThemeColor;

/// The resolved look of the app, shared by every view.
///
/// Views read the palette here at render time rather than holding a copy, so a
/// theme change reaches all of them without any of them going stale.
pub(super) struct Appearance {
    preference: ThemePreference,
    palette: CadencePalette,
}

impl gpui_kit::Global for Appearance {}

impl Appearance {
    /// Resolves the appearance for `window`, adopting the stored preference the
    /// first time. A window opened later joins the appearance already in use
    /// rather than resetting it to whatever was on disk at launch.
    pub(super) fn attach(window: &mut Window, cx: &mut App) {
        if !cx.has_global::<Self>() {
            let preference = services::AppServices::preferences(cx).theme;
            cx.set_global(Self {
                preference,
                palette: palette_for(false),
            });
        }
        let dark_mode = resolve_dark_mode(Self::preference(cx), window.appearance());
        apply_theme_mode(dark_mode, window, cx);
    }

    pub(super) fn palette(cx: &App) -> CadencePalette {
        cx.global::<Self>().palette
    }

    pub(super) fn preference(cx: &App) -> ThemePreference {
        cx.global::<Self>().preference
    }

    pub(super) fn set_preference(preference: ThemePreference, window: &mut Window, cx: &mut App) {
        let dark_mode = resolve_dark_mode(preference, window.appearance());
        let appearance = cx.global_mut::<Self>();
        appearance.preference = preference;
        apply_theme_mode(dark_mode, window, cx);
    }

    /// Re-resolves against the system appearance, for when it changes underneath
    /// us. Reports whether the preference actually follows the system.
    pub(super) fn follow_system(window: &mut Window, cx: &mut App) -> bool {
        if cx.global::<Self>().preference != ThemePreference::System {
            return false;
        }
        let dark_mode = is_dark_appearance(window.appearance());
        apply_theme_mode(dark_mode, window, cx);
        true
    }
}

fn palette_for(dark_mode: bool) -> CadencePalette {
    if dark_mode {
        CadencePalette::DARK
    } else {
        CadencePalette::LIGHT
    }
}

fn apply_theme_mode(dark_mode: bool, window: &mut Window, cx: &mut App) {
    let palette = palette_for(dark_mode);
    cx.global_mut::<Appearance>().palette = palette;
    Theme::change(
        if dark_mode {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        Some(window),
        cx,
    );
    Theme::update(cx, |theme| {
        apply_palette(palette, &mut theme.colors);
        theme.focus_ring = false;
    });
}

/// Shares Cadence's used color roles with kit controls and their base primitives.
fn apply_palette(palette: CadencePalette, colors: &mut ThemeColor) {
    colors.background = rgb(palette.canvas).into();
    colors.foreground = rgb(palette.text_primary).into();
    colors.muted_foreground = rgb(palette.text_muted).into();
    colors.border = rgb(palette.border).into();
    colors.input = rgb(palette.border).into();
    colors.ring = rgb(palette.focus_ring).into();
    colors.primary = rgb(palette.text_primary).into();
    colors.primary_hover = rgb(palette.accent_hover).into();
    colors.primary_foreground = rgb(palette.on_accent).into();
    colors.secondary = rgb(palette.control).into();
    colors.secondary_hover = rgb(palette.control_hover).into();
    colors.secondary_foreground = rgb(palette.text_primary).into();
    colors.danger = rgb(palette.destructive).into();
    colors.danger_foreground = rgb(palette.on_destructive).into();
    colors.popover = rgb(palette.surface_raised).into();
    colors.popover_foreground = rgb(palette.text).into();
    colors.accent = rgb(palette.control_hover).into();
    colors.accent_foreground = rgb(palette.text_primary).into();
    colors.list_hover = rgb(palette.surface_hover).into();
    colors.list_active = rgb(palette.selection).into();
    colors.selection = rgb(palette.selection).into();
    colors.switch = rgb(palette.control_hover).into();
    colors.switch_thumb = rgb(palette.surface).into();
    colors.link = rgb(palette.link).into();
}

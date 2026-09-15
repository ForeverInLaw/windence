use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// The Inter faces the UI names by weight. Bundled so rendering is the same
/// on every machine; a missing face renders as blank text on Windows, so
/// relying on system-installed copies is not an option.
pub(super) const FONT_FILES: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
];

/// The app's own SVG bundle: every glyph the UI names through
/// `CadenceIcon`, embedded from `assets/icons`. Kit components (Spinner,
/// Input, Switch, Avatar) request their own icons — loader, close, eye,
/// chevrons — which resolve through gpui-kit's default bundle behind this.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/icons"]
#[prefix = "icons/"]
pub(super) struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match Self::get(path) {
            Some(file) => Ok(Some(file.data)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut listed = gpui_kit::assets::Assets.list(path)?;
        listed.extend(
            Self::iter()
                .filter(|entry| entry.starts_with(path))
                .map(SharedString::from),
        );
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::icons::CadenceIcon;
    use gpui_kit::component::IconNamed;

    #[test]
    fn every_icon_is_bundled() {
        let bundled = AppAssets::iter()
            .filter(|path| path.starts_with("icons/"))
            .count();
        assert_eq!(
            bundled,
            CadenceIcon::ALL.len(),
            "icon list and bundle differ"
        );
        for icon in CadenceIcon::ALL {
            let path = icon.path();
            assert!(
                AppAssets
                    .load(&path)
                    .is_ok_and(|data| data.is_some_and(|data| !data.is_empty())),
                "missing {path}"
            );
        }
    }

    #[test]
    fn kit_icons_still_resolve_through_the_fallback() {
        assert!(
            AppAssets
                .load("icons/check.svg")
                .is_ok_and(|data| data.is_some())
        );
    }
}

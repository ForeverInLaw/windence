use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The asset path an icon name resolves to: `music` renders
/// `icons/music.svg`, first from Cadence's own set, then from the component
/// library's.
pub(super) fn icon_path(name: &str) -> String {
    format!("icons/{name}.svg")
}

/// Icons Cadence ships itself: glyphs the component library's asset set does
/// not include (transport, volume, music notes). Everything else resolves
/// through `gpui_component_assets`.
const CADENCE_ICONS: &[(&str, &[u8])] = &[
    (
        "icons/clock.svg",
        include_bytes!("../../assets/icons/clock.svg"),
    ),
    (
        "icons/heart-fill.svg",
        include_bytes!("../../assets/icons/heart-fill.svg"),
    ),
    (
        "icons/key.svg",
        include_bytes!("../../assets/icons/key.svg"),
    ),
    (
        "icons/list-music.svg",
        include_bytes!("../../assets/icons/list-music.svg"),
    ),
    (
        "icons/log-out.svg",
        include_bytes!("../../assets/icons/log-out.svg"),
    ),
    (
        "icons/music.svg",
        include_bytes!("../../assets/icons/music.svg"),
    ),
    (
        "icons/pin.svg",
        include_bytes!("../../assets/icons/pin.svg"),
    ),
    (
        "icons/pin-fill.svg",
        include_bytes!("../../assets/icons/pin-fill.svg"),
    ),
    (
        "icons/skip-back.svg",
        include_bytes!("../../assets/icons/skip-back.svg"),
    ),
    (
        "icons/shuffle.svg",
        include_bytes!("../../assets/icons/shuffle.svg"),
    ),
    (
        "icons/sparkles.svg",
        include_bytes!("../../assets/icons/sparkles.svg"),
    ),
    (
        "icons/skip-forward.svg",
        include_bytes!("../../assets/icons/skip-forward.svg"),
    ),
    (
        "icons/sun-moon.svg",
        include_bytes!("../../assets/icons/sun-moon.svg"),
    ),
    (
        "icons/volume-2.svg",
        include_bytes!("../../assets/icons/volume-2.svg"),
    ),
    (
        "icons/volume-x.svg",
        include_bytes!("../../assets/icons/volume-x.svg"),
    ),
];

/// The Inter faces the UI names by weight. Bundled so rendering is the same
/// on every machine; a missing face renders as blank text on Windows, so
/// relying on system-installed copies is not an option.
pub(super) const FONT_FILES: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
];

/// The app's asset source: gpui-component's embedded set with Cadence's own
/// icons layered on top.
pub(super) struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = CADENCE_ICONS.iter().find(|(known, _)| *known == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut listed = gpui_component_assets::Assets.list(path)?;
        listed.extend(
            CADENCE_ICONS
                .iter()
                .filter(|(known, _)| known.starts_with(path))
                .map(|(known, _)| SharedString::from(*known)),
        );
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_icon_loads_through_the_asset_source() {
        let assets = AppAssets;
        for (path, _) in CADENCE_ICONS {
            let loaded = assets.load(path).expect("asset load must not error");
            assert!(loaded.is_some(), "Cadence icon `{path}` did not load");
            assert!(
                !loaded.unwrap().is_empty(),
                "Cadence icon `{path}` is empty"
            );
        }
    }
}

use super::interactions::Fixture;
use crate::app::{appearance::Appearance, assets, services, windows};
use gpui_kit::component::Theme;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    DevicePixels, HeadlessAppContext, HeadlessAtlas, NoopTextSystem, PlatformAtlas,
    PlatformHeadlessRenderer, Quad, Scene, Shadow, Size, rgb,
};
use spotify_gpui_client::storage::ThemePreference;
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[derive(Default)]
struct PaintedFrame {
    quads: Vec<Quad>,
    shadows: Vec<Shadow>,
}

/// Observes production paint commands without rasterizing or claiming screenshots.
struct SceneProbe {
    frame: Rc<RefCell<PaintedFrame>>,
    atlas: Arc<HeadlessAtlas>,
}

impl PlatformHeadlessRenderer for SceneProbe {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        self.render_scene(scene, size)?;
        anyhow::bail!("the scene probe does not render pixels")
    }

    fn render_scene(&mut self, scene: &Scene, _: Size<DevicePixels>) -> anyhow::Result<()> {
        *self.frame.borrow_mut() = PaintedFrame {
            quads: scene.quads.clone(),
            shadows: scene.shadows.clone(),
        };
        Ok(())
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.atlas.clone()
    }
}

fn observe(fixture: &mut Fixture) {
    fixture.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.render_to_image().is_err());
    });
}

fn fixture(theme: ThemePreference, settings: bool) -> (Fixture, Rc<RefCell<PaintedFrame>>) {
    let frame = Rc::new(RefCell::new(PaintedFrame::default()));
    let observed = frame.clone();
    let cx = HeadlessAppContext::with_platform(
        Arc::new(NoopTextSystem),
        Arc::new(assets::AppAssets),
        move || {
            Ok(Some(Box::new(SceneProbe {
                frame: observed.clone(),
                atlas: Arc::new(HeadlessAtlas::default()),
            })))
        },
    );
    (Fixture::with_context(cx, settings, theme), frame)
}

#[test]
fn pointer_focus_keeps_buttons_usable_without_a_keyboard_ring() {
    let (mut fixture, frame) = fixture(ThemePreference::Dark, false);
    let before = fixture.update(|window, _| window.find("queue-toggle").bounds());
    fixture.update(|window, cx| window.click("queue-toggle", cx));
    observe(&mut fixture);
    fixture.update(|window, _| {
        assert_eq!(window.find("queue-toggle").focused(), Some(true));
        assert_eq!(window.find("queue-toggle").bounds(), before);
    });
    let focus = rgb(0xA8A5A1).into();
    assert!(
        frame
            .borrow()
            .shadows
            .iter()
            .all(|shadow| shadow.color != focus)
    );
    assert!(
        frame
            .borrow()
            .quads
            .iter()
            .all(|quad| quad.border_color != focus)
    );

    fixture.press("shift-tab");
    fixture.tab_to("queue-toggle");
    observe(&mut fixture);
    let scale = fixture.update(|window, _| window.scale_factor());
    let frame = frame.borrow();
    let ring = frame
        .shadows
        .iter()
        .find(|shadow| shadow.color == focus)
        .expect("keyboard focus paints a ring");
    assert_eq!(ring.inset, 1, "the ring fits inside clipped containers");
    assert_eq!(ring.blur_radius.0, 0.);
    assert_eq!(
        ring.element_bounds.size.width.0 - ring.bounds.size.width.0,
        2. * scale
    );
    assert_eq!(
        ring.element_bounds.size.height.0 - ring.bounds.size.height.0,
        2. * scale
    );
    assert!(
        ring.content_mask
            .bounds
            .contains(&ring.element_bounds.origin)
    );
    assert!(
        ring.content_mask
            .bounds
            .contains(&ring.element_bounds.bottom_right())
    );
    drop(frame);
    fixture.update(|window, _| assert_eq!(window.find("queue-toggle").bounds(), before));
    fixture.press("space");
    fixture.no_commands();
}

#[test]
fn appearance_changes_keep_component_and_base_colors_in_step() {
    let (mut fixture, _) = fixture(ThemePreference::Dark, true);
    for (id, expected) in [
        ("settings-appearance-light", ThemePreference::Light),
        ("settings-appearance-dark", ThemePreference::Dark),
        ("settings-appearance-light", ThemePreference::Light),
    ] {
        fixture.update(|window, cx| window.click(id, cx));
        fixture.update(|_, cx| {
            assert_eq!(Appearance::preference(cx), expected);
            assert_shared_colors(cx);
        });
    }
    fixture.update(|_, cx| {
        windows::open_main_window(cx);
        let handle = services::AppServices::main_window(cx).expect("new main window");
        handle
            .update(cx, |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(Appearance::preference(cx), ThemePreference::Light);
                assert_shared_colors(cx);
                window.remove_window();
            })
            .expect("draw new main window");
        services::AppServices::take_main_window(cx);
    });
}

fn assert_shared_colors(cx: &gpui_kit::App) {
    let palette = Appearance::palette(cx);
    let component = Theme::global(cx);
    let base = gpui_kit::base::Theme::global(cx).tokens.colors;
    for (name, own, kit, primitive) in [
        (
            "background",
            palette.canvas,
            component.background,
            base.background,
        ),
        (
            "foreground",
            palette.text_primary,
            component.foreground,
            base.foreground,
        ),
        ("border", palette.border, component.border, base.border),
        ("input", palette.border, component.input, base.input),
        ("ring", palette.focus_ring, component.ring, base.ring),
        (
            "selected",
            palette.selection,
            component.selection,
            base.selection,
        ),
        (
            "secondary",
            palette.control,
            component.secondary,
            base.secondary,
        ),
        (
            "accent text",
            palette.text_primary,
            component.accent_foreground,
            base.accent_foreground,
        ),
    ] {
        let own = rgb(own).into();
        assert_eq!(kit, own, "kit {name} disagrees with the active app palette");
        assert_eq!(
            primitive, own,
            "base {name} disagrees with the active app palette"
        );
    }
    assert_eq!(component.switch, rgb(palette.control_hover).into());
    assert_eq!(component.switch_thumb, rgb(palette.surface).into());
    assert_eq!(component.font_family.as_ref(), "Inter");
    assert!(
        !component.focus_ring,
        "clipped controls use a thin inner focus line"
    );
}

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
    fixture_with_layout(theme, settings, 1280., false)
}

fn fixture_with_layout(
    theme: ThemePreference,
    settings: bool,
    width: f32,
    collapsed: bool,
) -> (Fixture, Rc<RefCell<PaintedFrame>>) {
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
    (
        Fixture::with_layout(cx, settings, theme, width, collapsed),
        frame,
    )
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

#[test]
fn search_editor_focus_tints_its_frame_without_moving_the_input() {
    let (mut fixture, _) = fixture(ThemePreference::Light, false);
    let before = fixture.update(|window, _| window.find("search-input").bounds());
    fixture.update(|window, cx| window.click("search-input", cx));
    fixture.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("search-input").focused(), Some(true));
        assert_eq!(window.find("search-input").bounds(), before);
        let border = window
            .painted_quads()
            .into_iter()
            .find(|quad| quad.border_color == rgb(0x848281).into())
            .expect("the active editor frame has a visible border");
        assert_eq!(border.border_widths.left.0, window.scale_factor());
        assert_eq!(border.border_widths.right.0, window.scale_factor());
    });
    fixture.no_commands();
}

#[test]
fn client_id_editor_keeps_the_same_focus_frame_as_search() {
    let mut fixture = Fixture::sign_in(false);
    fixture.update(|_, cx| {
        services::AppServices::session(cx).update(cx, |session, cx| {
            session.handle_backend_event(
                spotify_gpui_client::backend::BackendEvent::SetupRequired,
                cx,
            );
        });
    });
    let before = fixture.update(|window, _| window.find("client-id-input").bounds());
    fixture.update(|window, cx| window.click("client-id-input", cx));
    fixture.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("client-id-input").bounds(), before);
        assert_eq!(window.find("client-id-input").focused(), Some(true));
        let border = window
            .painted_quads()
            .into_iter()
            .find(|quad| quad.border_color == rgb(0x848281).into())
            .expect("the setup editor shares the quiet focused frame");
        assert_eq!(border.border_widths.top.0, window.scale_factor());
    });
    fixture.no_commands();
}

#[test]
fn keyboard_focus_does_not_shrink_the_painted_slider_tracks() {
    let (mut fixture, frame) = fixture(ThemePreference::Dark, false);
    for (id, height) in [("progress-slider", 5.), ("volume-slider", 4.)] {
        let before = fixture.update(|window, _| window.find(id).bounds());
        fixture.tab_to(id);
        observe(&mut fixture);
        fixture.update(|window, cx| {
            assert_eq!(window.find(id).bounds(), before);
            let bounds = before.scale(window.scale_factor());
            let background = rgb(Appearance::palette(cx).surface_raised).into();
            let frame = frame.borrow();
            let track = frame
                .quads
                .iter()
                .find(|quad| {
                    quad.background.as_solid() == Some(background)
                        && quad.bounds.size.height.0 == height * window.scale_factor()
                        && bounds.contains(&quad.bounds.center())
                })
                .expect("the slider paints its thin track");
            assert_eq!(
                track.bounds.size.width.0, bounds.size.width.0,
                "focus must not steal width from the track"
            );
            let ring = frame
                .shadows
                .iter()
                .find(|shadow| shadow.inset == 1 && shadow.element_bounds == bounds)
                .expect("keyboard focus paints an inner ring on the slider");
            assert_eq!(
                ring.element_bounds.size.width.0 - ring.bounds.size.width.0,
                2. * window.scale_factor()
            );
        });
    }
    fixture.no_commands();
}

#[test]
fn switch_focus_is_a_keyboard_only_line_inside_its_track() {
    let (mut fixture, frame) = fixture(ThemePreference::Light, true);
    let before = fixture.update(|window, _| window.find("settings-autoplay").bounds());
    fixture.update(|window, cx| window.click("settings-autoplay", cx));
    observe(&mut fixture);
    let focus = rgb(0x848281).into();
    assert!(
        frame
            .borrow()
            .quads
            .iter()
            .all(|quad| quad.border_color != focus),
        "pointer focus must not tint the switch border"
    );
    fixture.press("shift-tab");
    fixture.tab_to("settings-autoplay");
    observe(&mut fixture);
    fixture.update(|window, _| {
        assert_eq!(window.find("settings-autoplay").bounds(), before);
        let frame = frame.borrow();
        let ring = frame
            .quads
            .iter()
            .find(|quad| quad.border_color == focus)
            .expect("keyboard focus tints the switch track border");
        assert_eq!(ring.border_widths.top.0, window.scale_factor());
        assert_eq!(ring.bounds, before.scale(window.scale_factor()));
        assert!(frame.shadows.iter().all(|shadow| shadow.color != focus));
    });
    fixture.no_commands();
}

#[test]
fn account_focus_line_paints_over_the_avatar_inside_its_bounds() {
    let (mut fixture, frame) = fixture(ThemePreference::Dark, false);
    let before = fixture.update(|window, _| window.find("account").bounds());
    fixture.tab_to("account");
    observe(&mut fixture);
    fixture.update(|window, _| {
        assert_eq!(window.find("account").bounds(), before);
        let bounds = before.scale(window.scale_factor());
        let frame = frame.borrow();
        let ring = frame
            .quads
            .iter()
            .find(|quad| quad.bounds == bounds && quad.border_color == rgb(0xA8A5A1).into())
            .expect("the avatar needs a focus line above its opaque fill");
        assert_eq!(ring.border_widths.left.0, window.scale_factor());
        assert!(ring.content_mask.bounds.contains(&bounds.origin));
        assert!(ring.content_mask.bounds.right() >= bounds.right());
        assert!(ring.content_mask.bounds.bottom() >= bounds.bottom());
        for fill in frame.quads.iter().filter(|quad| {
            quad.bounds == bounds && quad.background.as_solid().is_some_and(|color| color.a > 0.)
        }) {
            assert!(
                ring.order > fill.order,
                "the focus line must paint after the avatar fill"
            );
        }
    });
    fixture.no_commands();
}

#[test]
fn filled_controls_use_their_foreground_for_the_inner_focus_line() {
    for theme in [ThemePreference::Light, ThemePreference::Dark] {
        let (mut fixture, frame) = fixture(theme, true);
        fixture.tab_to("play-toggle");
        observe(&mut fixture);
        assert_filled_focus(&mut fixture, &frame, "play-toggle");
        fixture.update(|window, cx| window.click("settings-change-spotify-app", cx));
        fixture.tab_to("confirm-spotify-app-change");
        observe(&mut fixture);
        assert_filled_focus(&mut fixture, &frame, "confirm-spotify-app-change");
        fixture.press("escape");
        fixture.update(|_, cx| {
            services::AppServices::session(cx).update(cx, |session, cx| {
                session.handle_backend_event(
                    spotify_gpui_client::backend::BackendEvent::AuthorizationRequired,
                    cx,
                );
            });
        });
        fixture.tab_to("open-sign-in-window");
        observe(&mut fixture);
        assert_filled_focus(&mut fixture, &frame, "open-sign-in-window");
        fixture.no_commands();
    }
}

fn assert_filled_focus(fixture: &mut Fixture, frame: &RefCell<PaintedFrame>, id: &'static str) {
    fixture.update(|window, cx| {
        let bounds = window.find(id).bounds().scale(window.scale_factor());
        let foreground = rgb(Appearance::palette(cx).on_accent).into();
        let frame = frame.borrow();
        let ring = frame
            .shadows
            .iter()
            .find(|shadow| shadow.element_bounds == bounds && shadow.color.alpha(1.) == foreground)
            .unwrap_or_else(|| panic!("{id} needs its foreground focus line"));
        assert!(
            ring.color.a > 0.,
            "the line remains visible during the entrance fade"
        );
        assert_eq!(ring.inset, 1);
        assert_eq!(
            ring.element_bounds.size.width.0 - ring.bounds.size.width.0,
            2. * window.scale_factor()
        );
    });
}

#[test]
fn inner_focus_lines_fit_rows_and_controls_across_responsive_tiers() {
    for (width, collapsed) in [
        (1280., false),
        (1024., false),
        (720., false),
        (960., true),
        (720., true),
    ] {
        let (mut fixture, frame) =
            fixture_with_layout(ThemePreference::Dark, false, width, collapsed);
        for id in [
            gpui_kit::ElementId::from("queue-toggle"),
            ("spotify-track", 0usize).into(),
        ] {
            let before = fixture.update(|window, _| window.find(id.clone()).bounds());
            fixture.tab_to(id.clone());
            observe(&mut fixture);
            fixture.update(|window, _| {
                let control = window.find(id.clone());
                assert_eq!(
                    control.bounds(),
                    before,
                    "focus moves {id:?} at width {width}"
                );
                assert!(
                    control.bounds().right() <= gpui_kit::px(width),
                    "{id:?} overflows width {width}, collapsed {collapsed}: {:?}; viewport {:?}",
                    control.bounds(),
                    window.viewport_size()
                );
                let center = control.bounds().center().scale(window.scale_factor());
                let frame = frame.borrow();
                let ring = frame
                    .shadows
                    .iter()
                    .find(|shadow| {
                        shadow.inset == 1
                            && shadow.color == rgb(0xA8A5A1).into()
                            && shadow.element_bounds.contains(&center)
                    })
                    .expect("the focused control paints its inner line");
                assert!(ring.content_mask.bounds.left() <= ring.element_bounds.left());
                assert!(ring.content_mask.bounds.right() >= ring.element_bounds.right());
                assert!(ring.content_mask.bounds.top() <= ring.element_bounds.top());
                assert!(ring.content_mask.bounds.bottom() >= ring.element_bounds.bottom());
            });
        }
        fixture.no_commands();
    }
}

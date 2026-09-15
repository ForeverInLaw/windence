use super::*;
use gpui_kit::{ease_in_out, ease_out_quint};

/// The one motion scale every animated surface draws its timings, curves and
/// steps from. Nothing outside this module invents a duration or an easing.
///
/// The app animates opacity and paint-level position only. Nothing animated
/// here moves other elements' layout: the overlays (banner, menus, modal,
/// drawer) are absolutely positioned or deferred, so shifting them cannot
/// push anything else around, and the sidebar was already animated through
/// its own width before this ticket.
///
/// Reduced motion needs no per-surface guard: `with_animation` renders a
/// oneshot animation at its end state the moment `cx.reduce_motion()` is set
/// and stops scheduling frames.
///
/// What each surface animates, exactly:
/// - the notice banner: opacity and its `top` offset (rise), on `MEDIUM`;
/// - the account, row and sort menus: opacity plus a `top` start inset
///   toward the trigger, on `QUICK`;
/// - the confirmation modal card: opacity, on `FAST` (the grow is implied
///   by the fade — Div has no transform at this pin);
/// - the queue drawer: its `right` offset and opacity, on `FAST`;
/// - the sidebar rail: its own width and row fills, on its distance-scaled
///   variant of this scale (`SIDEBAR_OPEN_MILLIS` / `SIDEBAR_FLOOR_MILLIS`);
/// - hover reveals (heart, row actions): a visibility flip, which is not an
///   animation at this pin — see `track_row.rs` for why opacity cannot take
///   its place, with `MICRO` held as the step it would ride.
///
/// A hover-grade change: the eye should barely register the wait. Held for
/// the hover reveals, which stay a visibility flip at this pin (see
/// `track_row.rs`); the step is named so the surface class has its number.
#[allow(dead_code)]
pub(super) const MICRO: Duration = Duration::from_millis(80);
/// Something opening under the pointer: menus and their kind.
pub(super) const QUICK: Duration = Duration::from_millis(150);
/// A panel arriving over the page: the drawer, the modal card.
pub(super) const FAST: Duration = Duration::from_millis(250);
/// A self-dismissing banner that has to read as deliberate.
pub(super) const MEDIUM: Duration = Duration::from_millis(350);
/// The slowest step, reserved for cross-fades nothing else covers yet.
#[allow(dead_code)]
pub(super) const SLOW: Duration = Duration::from_millis(400);

/// Opens start on a step and leave on the next one down, so a surface is
/// always away faster than it arrived. Surfaces here close by unmounting —
/// a lingering exit would fight the notice timer and the menu teardowns —
/// so the constant holds the rule for the first surface that earns a real
/// exit animation.
#[allow(dead_code)]
pub(super) const CLOSE_FASTER_THAN_QUICK: Duration = Duration::from_millis(100);

/// The easing opens and closes ride: fast start, long settle.
pub(super) fn smooth_out() -> impl Fn(f32) -> f32 {
    ease_out_quint()
}

/// The easing cross-fades ride: eased at both ends, no fast side.
#[allow(dead_code)]
pub(super) fn in_out() -> impl Fn(f32) -> f32 {
    ease_in_out
}

/// The easing badge-like pops ride: out fast, back the same way.
#[allow(dead_code)]
pub(super) fn bounce() -> impl Fn(f32) -> f32 {
    gpui_kit::bounce(smooth_out())
}

/// A menu or modal starts a step under full opacity and eases to it — the
/// visible stand-in for the scale grow, while Div has no transform.
pub(super) const SCALE_STEP: f32 = 0.96;

/// How far below its place a surface starts its arrival: the banner rises
/// this far, the drawer keeps this much of itself inside the window at the
/// first frame so the slide reads as a move rather than a pop.
pub(super) const RISE_DISTANCE: f32 = 24.;

/// How far below its rest place an anchored menu starts, growing up into
/// its anchor as it fades in.
pub(super) const MENU_OPEN_INSET: f32 = 8.;

/// The one blur step. Nothing at this pin can animate a blur (Div has no
/// filter style), so the step is defined and unused — the seat is kept so
/// a surface class gets one number to name when the platform can.
#[allow(dead_code)]
pub(super) const BLUR_STEP: f32 = 8.;

/// Animation ids carry the surface's open generation so a surface that
/// re-opens starts its entrance from zero again instead of continuing an
/// older timeline. The stateless surfaces (menus, drawer, modal) are torn
/// down when they close, which resets their element state on its own; the
/// banner stays mounted in the workspace, so only it needs the counter.
pub(super) fn animation_id(id: &'static str, generation: usize) -> ElementId {
    ElementId::from((id, generation))
}

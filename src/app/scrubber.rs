use super::*;

#[derive(Clone)]
pub(super) struct ScrubberDrag;

pub(super) struct NoDragPreview;

impl Render for NoDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct Gesture {
    bounds: Bounds<Pixels>,
    playback_generation: u64,
    duration_ms: u32,
    preview_ms: u32,
}

/// Measures the drawn progress track rather than copying its layout math.
pub(super) struct Scrubber {
    pub(super) bounds: Rc<Cell<Bounds<Pixels>>>,
    gesture: Option<Gesture>,
}

impl Scrubber {
    pub(super) fn new() -> Self {
        Self {
            bounds: Rc::new(Cell::new(Bounds::default())),
            gesture: None,
        }
    }

    fn position_for_pointer(
        bounds: Bounds<Pixels>,
        pointer_x: Pixels,
        duration_ms: u32,
    ) -> Option<u32> {
        let width = f32::from(bounds.size.width);
        if width <= 0. || duration_ms == 0 {
            return None;
        }
        let fraction = (f32::from(pointer_x - bounds.origin.x) / width).clamp(0., 1.);
        Some((fraction * duration_ms as f32).round() as u32)
    }

    pub(super) fn begin(&mut self, pointer_x: Pixels, playback_generation: u64, duration_ms: u32) {
        let bounds = self.bounds.get();
        if let Some(preview_ms) = Self::position_for_pointer(bounds, pointer_x, duration_ms) {
            self.gesture = Some(Gesture {
                bounds,
                playback_generation,
                duration_ms,
                preview_ms,
            });
        }
    }

    pub(super) fn drag_to(&mut self, pointer_x: Pixels) {
        if let Some(gesture) = self.gesture.as_mut()
            && let Some(position_ms) =
                Self::position_for_pointer(gesture.bounds, pointer_x, gesture.duration_ms)
        {
            gesture.preview_ms = position_ms;
        }
    }

    pub(super) fn displayed_ms(&self, position_ms: u32, duration_ms: u32) -> u32 {
        self.gesture
            .as_ref()
            .map_or(position_ms, |gesture| gesture.preview_ms)
            .min(duration_ms)
    }

    pub(super) fn release(&mut self, playback_generation: u64) -> Option<u32> {
        let gesture = self.gesture.take()?;
        (gesture.playback_generation == playback_generation).then_some(gesture.preview_ms)
    }

    pub(super) fn cancel_if_changed(&mut self, playback_generation: u64) {
        if self
            .gesture
            .as_ref()
            .is_some_and(|gesture| gesture.playback_generation != playback_generation)
        {
            self.cancel();
        }
    }

    pub(super) fn cancel(&mut self) -> bool {
        self.gesture.take().is_some()
    }
}

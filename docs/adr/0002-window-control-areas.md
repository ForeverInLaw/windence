# Window control areas: drag surfaces never overlap the traffic lights

The Windows port draws macOS-style traffic lights itself and hands each dot to
the OS through a GPUI `WindowControlArea` (`Close`, `Min`, `Max`). Dragging the
frameless window works through separate strips marked `WindowControlArea::Drag`,
carved around the cluster band: on the main window they cover the sidebar's top
padding and the toolbar's middle gap; the onboarding window drags from its top
edge.

At the pinned Zed rev, GPUI resolves which control area covers the cursor in
two steps. The mouse hit test collects **every** hitbox under the point —
paint order does not occlude. The platform callback then walks all registered
control areas **in registration order (paint order)** and returns the first
match. So when a `Drag` area and a button dot overlap, whichever paints first
wins the whole overlap — three earlier fixes that only reordered elements
inside the lights overlay could not work, because the sidebar's strip painted
(and registered) before the overlay did.

## Decision

No `Drag` control area may intersect a traffic-light dot's rectangle. The
sidebar's top strip is carved around the cluster band with two rects (right of
it, below it); the cluster itself renders no drag pad; the dots carry no
`on_click` fallback — the native non-client path is the only click path.
Cluster geometry constants (`TRAFFIC_LIGHT_*`) live once in `src/app/mod.rs`
and are shared by the overlay and the sidebar strip so the carve-out cannot
drift from the dots.

## Considered Options

- **Keep an overlay-wide drag pad behind the dots** — rejected at this pin:
  correctness would depend on registration order between two files; any future
  paint-order change silently breaks clicks again.
- **Plain `on_click` handlers on the dots instead of control areas** —
  rejected: loses native double-click and non-client behavior for no gain;
  with a control area under the cursor the window receives no client events,
  so keeping both paths is dead code.
- **Dedicated title-bar view owning the whole top edge** — rejected: the owner
  wants the upstream reference layout, lights floating over content.

## Consequences

- Clicks on × − + are native (`HTCLOSE`/`HTMINBUTTON`/`HTMAXBUTTON`), including
  press state and hover glyphs, which the pinned Windows backend still delivers
  as normal GPUI mouse events.
- The 8px gaps between dots and the 9px corner left of them do not drag. This
  is accepted; thin gap pads can be added later without touching the dots if
  it matters.
- When adding any new full-window or top-edge overlay, check it against this
  rule first: a `Drag` area painted before the dots will make them unclickable.
- Revisit only together with a pin bump (ADR-0001): newer GPUI may change the
  resolution order or add occlusion opt-outs.

# Native Windows validation

Use this checklist after a GPUI snapshot or appearance change. Portable
interaction tests verify controls and layout. They do not prove OAuth,
native window behavior, WASAPI output, SMTC or Windows rendering.

## Builds to compare

- Baseline before the GPUI upgrade: commit `8241df2`,
  [artifact build](https://github.com/ForeverInLaw/windence/actions/runs/38279361089).
- GPUI upgrade: commit `42ec112`,
  [Windows CI](https://github.com/ForeverInLaw/windence/actions/runs/38279655986)
  and [artifact build](https://github.com/ForeverInLaw/windence/actions/runs/38281237057).
- Integrated code, including calm appearance, playlist search and review
  fixes: commit `6ec7270`,
  [Windows CI](https://github.com/ForeverInLaw/windence/actions/runs/38287231417)
  and [artifact build](https://github.com/ForeverInLaw/windence/actions/runs/38287241242).

Use files only after their run finishes successfully. The code commit is
recorded separately from later documentation commits.

The manual Release workflow builds `cadence-windows-x64` artifacts without
publishing. Download the full ZIP or installer from a successful run. The
ZIP includes required runtime DLLs; copying only the EXE is insufficient.

Quit the currently installed Cadence with Ctrl+Q before opening another
build. Otherwise single-instance activation can bring forward the old
executable. Keep the same account and preferences for the comparison.

## Functional checks

- [ ] Sign in from onboarding through the browser and reach the main window.
      Saved credentials and the existing Spotify app configuration still work.
- [ ] Bundled Inter renders in the main window, sign-in fields and menus.
      Light, dark and follow-system modes work after closing and reopening.
- [ ] Native caption controls close, minimize and maximize. Drag strips move
      the window. Resize, restore and reopening work. Closing the window
      keeps playback running; Ctrl+Q exits the process.
- [ ] Tab reaches enabled controls. Enter and Space activate them once.
      Menus and the queue return focus to their trigger when closed.
      Ctrl shortcuts, text editing and both slider keyboard controls work.
- [ ] Search inside a playlist by title, artist and album, including non-Latin
      text. Ctrl+F focuses local search; Ctrl+K opens Spotify search. Clear
      restores the rows, and another playlist starts with an empty query.
      After sorting and filtering, a row starts the selected occurrence in
      the full playlist queue, including duplicate songs. Search after
      scrolling returns to the first matching row.
- [ ] Drag progress within and outside its bounds. Release seeks once.
      Escape and changing the track cancel the gesture. Seek, pause, close
      and reopen without restoring an older playback position.
- [ ] SMTC shows metadata and handles playback and seek. It attaches to a
      reopened main window.
- [ ] Ordinary music and DJ narration play. Switching the default output or
      connecting Bluetooth changes the output and recovers from a stall.

## Appearance comparison

Compare baseline and final builds at the same window size, theme and DPI.
Use a window that fits the screen at each scale.

- [ ] Capture light and dark appearances with keyboard focus visible.
- [ ] Check full, compact and folded layouts with the sidebar expanded and
      collapsed. No content clips and control areas remain usable.
- [ ] Check Windows display scaling at 100%, 125% and 150%, as the screen
      permits. Borders and focus should be quieter while focused controls
      remain identifiable.

Record the commit and artifact used, pass/fail for each item, Windows scale,
and screenshots. Report failures with the steps that reproduce them.

## Native onboarding evidence

[Comparison run](https://github.com/ForeverInLaw/windence/actions/runs/38285394728)
captured the baseline `8241df2` and appearance build `38db4ad` in light and
dark themes, before and after Tab. Each window was 1200 by 720 pixels at
96 DPI (100%). The `cadence-native-onboarding` artifact includes the PNGs,
app logs and `report.json`. Its status was `captured-needs-visual-review`;
all eight window images were inspected. Borders are quieter, keyboard
focus remains visible, and text and controls fit without clipping.

The final artifact build above repeats this comparison for `6ec7270`.
Read its report and inspect its images before counting it as evidence.
This capture covers onboarding at 100% only. The functional checks and
signed-in main-window comparisons above still need a Windows account and
audio devices.

To repeat the optional capture, manually run Release on the integration
branch with `baseline_run_id=38279361089`. It does not sign in or seed app
data. It uses an interactive desktop and, if needed, attempts the built-in
display-resolution command once, then restores the original settings.
The optional step can fail while packaging succeeds; `report.json` gives
the capture result. It never publishes a release from a manual run.

## Sources

- [GPUI Kit 0.7 testing guide](https://gpui-kit.com/versions/v0.7.0/docs/test/)
  documents portable interaction helpers and the macOS/Metal pixel renderer.
- [Windows-only port decision](adr/0001-windows-only-port.md) records the
  selected GPUI family and the retained platform paths.
- [Window control areas](adr/0002-window-control-areas.md) records the native
  chrome hit-area rule.

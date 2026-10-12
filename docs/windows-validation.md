# Native Windows validation

Use this checklist after a GPUI snapshot or appearance change. Portable
interaction tests verify controls and layout. They do not prove OAuth,
native window behavior, WASAPI output, SMTC or Windows rendering.

## Builds to compare

- Baseline before the GPUI upgrade: commit `8241df2`,
  [artifact build](https://github.com/ForeverInLaw/windence/actions/runs/38279361089).
  The run must finish successfully before its files count as build evidence.
- Upgraded build: record the exact commit, successful CI run and Release
  artifact run here after they finish.
- Final appearance build: record its exact commit and artifact run after
  the appearance ticket is integrated.

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

## Sources

- [GPUI Kit 0.7 testing guide](https://gpui-kit.com/versions/v0.7.0/docs/test/)
  documents portable interaction helpers and the macOS/Metal pixel renderer.
- [Windows-only port decision](adr/0001-windows-only-port.md) records the
  selected GPUI family and the retained platform paths.
- [Window control areas](adr/0002-window-control-areas.md) records the native
  chrome hit-area rule.

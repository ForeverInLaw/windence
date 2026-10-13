# Native Windows validation

## Store package

Run initial installation and runtime checks before first Store certification.
Migration and Store-delivered update checks follow the first publication.
Record OS build, package version and results in IVO-79. See the
[delivery guide](microsoft-store.md) for packaging.

- Install and launch on x64 Windows 10 build 19041 or newer and Windows 11.
- Confirm Windence's name and icon in Start, Settings and the app window.
- Confirm the privacy policy is accessible before sign-in and in Settings.
- Complete both browser OAuth steps. Restart and confirm saved credentials work.
- Play audio, change the default output device and test SMTC metadata/controls.
- Launch again from Start. Confirm one player remains and comes forward.
- Close the window while playing, reopen it, then quit completely.
- With a closed existing Inno/ZIP copy, back up `%APPDATA%\Cadence` and install
  the Store version. Confirm preferences, history, playlist cache and saved
  playback survive. Confirm no old process handles the Store activation.
- Update Store version A to B. Confirm authentication and profile data survive,
  saved queue/position restore, and playback starts paused after relaunch.
- Confirm Store handles downloads on a Windows metered connection. The app
  itself does not download an installer.

CI currently verifies a test-signed package installation, onboarding and the
unvirtualized profile path. It does not cover the remaining checks above.

## Temporary certification access

Build `a88d389`, version 1.3.2, passed
[Windows Release CI](https://github.com/ForeverInLaw/windence/actions/runs/38329998805).
The unsigned Store artifact and development files match their SHA256 sidecars.
The native capture report has no error. All four light/dark onboarding images
were inspected at 1200 by 720 pixels and 96 DPI. Review access is visible and
the existing controls fit. The owner helper also passed its native Credential
Manager check using synthetic credentials.

The owner enabled the server session on 2026-10-13. Provisioning renewed both
Spotify credentials and verified Premium. A live HTTPS check with the review
code returned both tokens and confirmed the expected account through Spotify.
The owner reported submitting 1.3.2.0 for certification the same day. These
server checks do not test playback in the app. The Windows runtime results
below have not been recorded:

- [ ] From setup, open Review access, enter the private code and connect.
      With an existing developer-app configuration, first use Change developer
      app to clear the old account state.
- [ ] Open a playlist, search, play audio, seek, change the queue and test SMTC.
- [ ] Reconnect without browser login. Quit completely and confirm a fresh
      launch asks for the code again.
- [ ] Sign out of review access and confirm ordinary browser setup is available.

See [Store review access](store-review-access.md) for provisioning, private
certification notes and revocation. This path does not verify browser sign-in.

## GPUI and appearance checks

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
  [Windows CI at `4b62011`](https://github.com/ForeverInLaw/windence/actions/runs/38287890799)
  and [artifact build](https://github.com/ForeverInLaw/windence/actions/runs/38287241242).

Use files only after their run finishes successfully. The code commit is
recorded separately from later test and documentation commits. `4b62011`
also fixes the lock scope in a test; its application code matches the
packaged `6ec7270` code. CI passed build, strict Clippy and 334 portable
tests (185 library, 149 application). Linux passed 335 tests, build,
strict Clippy and formatting. The ZIP and installer checksums match their
SHA-256 sidecars; the ZIP contains the EXE, runtime DLL and notices.

Further [PR #39](https://github.com/ForeverInLaw/windence/pull/39) review
fixes at `0733bf7` clear stale playlist rows, redraw expired confirmations,
and reject old position reports after seeking or restoring playback.
Linux passed 340 tests, strict Clippy and formatting on that code. See
the PR checks for Windows CI. The packaged `6ec7270` build above predates
these fixes.

[IVO-78](https://linear.app/ivoxygen/issue/IVO-78/keshirovat-plejlisty-i-zagruzhat-treki-po-stranicam)
adds persistent playlist prefixes and paging in the same PR. Linux passed
373 tests, strict all-target Clippy and formatting. The playlist checks below
cover cold and cached opens, account changes and full-context actions. See
the PR for Windows CI results. The packaged build above also predates this
feature. SQLite schema 13 keeps existing preferences and playback, but older
builds cannot open the upgraded profile.

The manual Release workflow builds `windence-windows-x64` artifacts without
publishing. Download the full ZIP or installer from a successful run. The
ZIP includes required runtime DLLs; copying only the EXE is insufficient.

Quit the currently installed player with Ctrl+Q before opening another
build. Otherwise single-instance activation can bring forward the old
executable. Keep the same account and preferences for the comparison.

For the public rename, check the sidebar label, window title, Task Manager,
Start shortcut and installed-app name. All should show Windence. An Inno
upgrade keeps the previous installation directory but removes its old
`Cadence.exe` and generated shortcuts. Authentication and profile data must
survive. Older comparison artifacts keep their original Cadence filenames.

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
      scrolling returns to the first matching row. While another playlist
      loads, Play and Shuffle cannot start the previous playlist. The same
      holds if loading fails or the new playlist is empty.
- [ ] Copy the redirect URI in onboarding. Its confirmation disappears
      after four seconds without mouse or keyboard input. Timed
      confirmations in the main window also disappear without input.
- [ ] Open a large uncached playlist without a saved sort. Only its first
      50 raw items load. Scroll to the end: another page loads once. Load more
      also works when a page has no displayable rows. Reopen the playlist and
      restart the app: saved rows appear and paging resumes from the raw
      offset, including local tracks and episodes that were hidden.
- [ ] Change a playlist in Spotify, then reopen it or start playback.
      The new version replaces the old prefix without mixing rows. A selected
      row from an old version does not start a different song. Repeated songs
      keep their own dates and positions. Search, sort, Play and Shuffle wait
      for the full context; a failed page keeps the prefix and offers Retry.
- [ ] Leave a playlist while it loads. Returning uses its saved prefix.
      Sign out during loading and sign in to another account. The earlier
      account's rows never appear and its queued writes cannot refill the cache.
- [ ] Use different accounts for the library and playback sign-in steps.
      A curated playlist asks for the same account instead of showing or
      caching the other account's data.
- [ ] Drag progress within and outside its bounds. Release seeks once.
      Escape and changing the track cancel the gesture. Seek, pause, close
      and reopen without restoring an older playback position. A delayed
      pre-seek report must not move the position back after two seconds.
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

The final artifact build above also captured `6ec7270`. Its report has no
error and all four windows use the same size and DPI as the baseline.
The final images were inspected with the same result; unchanged baseline
images were checked against the reviewed originals. Both probes restored
the runner's original display resolution.
This capture covers onboarding at 100% only. The functional checks and
signed-in main-window comparisons above still need a Windows account and
audio devices.

To repeat the optional capture, manually run Release on the integration
branch with `baseline_run_id=38279361089`. It does not sign in or seed app
data. It uses an interactive desktop and, if needed, attempts the built-in
display-resolution command once, then restores the original settings.
The optional step can fail while packaging succeeds; `report.json` gives
the capture result. It never publishes a release from a manual run.
Each capture uses a fresh profile so an upgraded SQLite schema cannot stop
the older baseline build from opening.

## Sources

- [GPUI Kit 0.7 testing guide](https://gpui-kit.com/versions/v0.7.0/docs/test/)
  documents portable interaction helpers and the macOS/Metal pixel renderer.
- [Windows-only port decision](adr/0001-windows-only-port.md) records the
  selected GPUI family and the retained platform paths.
- [Window control areas](adr/0002-window-control-areas.md) records the native
  chrome hit-area rule.

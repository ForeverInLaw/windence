# Windows updater proposal

Research for [IVO-76](https://linear.app/ivoxygen/issue/IVO-76). Checked on 2026-10-12 against port commit `5f3780f` and the stable versions below. This is a proposal. No updater, packaging change, signing key, or release is included.

## Recommendation

Use WinSparkle for signed updates of the existing Inno Setup install. Keep portable ZIP updates manual, with an explicit "Download portable update" action that opens the port's GitHub Releases. This preserves both distribution modes and avoids writing our own replacement process. WinSparkle verifies an EdDSA-signed installer before launching it and documents support for Inno Setup. [Signing](https://winsparkle.org/guides/getting-started/), [installer support](https://winsparkle.org/guides/publishing-updates/).

This is the best fit for the current scope. Automatic replacement of portable folders is not required. Velopack would solve that extra requirement, but it requires packaging migration and additional verification work. Its current Windows failure paths also need more validation. Reconsider Velopack if updating portable folders in place becomes a product requirement.

## Options

| Option | Maintenance checked | Fit and implementation cost | Decision |
| --- | --- | --- | --- |
| WinSparkle | Stable [0.9.4](https://github.com/vslavik/winsparkle/releases/tag/v0.9.4), released 2026-07-21. | One DLL, C API, native update dialog, appcast, built-in EdDSA verification. Works with our installer. Low to medium integration cost, with shutdown and signing work. [Integration](https://winsparkle.org/guides/integrating-winsparkle/). | Recommended. Portable stays manual. |
| Velopack | Stable [1.2.161](https://github.com/velopack/velopack/releases/tag/1.2.161), released 2026-09-29. | Official Rust API. Installer and self-updating portable ZIP share full-folder packages. Requires a new installer, ZIP layout, and release feed. [Rust integration](https://docs.velopack.io/getting-started/rust), [outputs](https://docs.velopack.io/packaging/overview). | Future candidate if in-place portable updates are required. |
| Rust `self_update` | Stable [1.3.0](https://docs.rs/crate/self_update/1.3.0), published 2026-09-02. | GitHub discovery and optional zipsign verification. Replacing only the executable leaves our DLL stale. Its directory swap can fail on Windows while a bundled DLL is loaded. Installer execution needs a separate path. [Versioned docs](https://github.com/jaemk/self_update/blob/v1.3.0/README.md). | Reject for the current multi-file GUI package. |
| MSIX App Installer | Microsoft-supported Windows mechanism. | Requires signed MSIX packages and an `.appinstaller` association. Changes the distribution model and does not provide an unpacked portable ZIP. [App Installer](https://learn.microsoft.com/en-us/windows/msix/app-installer/app-installer-file-overview), [signing](https://learn.microsoft.com/en-us/windows/msix/package/signing-package-overview). | Reject for this scope. Reconsider for a Store package. |

Cost and fit judgments are conclusions from these sources and the port's files, not vendor estimates.

## Package paths and permissions

The [Inno script](../../scripts/cadence.iss) uses the fixed AppId `8C3F5D2A-6B41-4E88-9A17-2D5C7E4B9F31` and `PrivilegesRequired=lowest`. Keep both. Normal updates should remain per user without an administrator prompt. Inno reuses the prior install directory by default. [Privileges](https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm), [prior directory](https://jrsoftware.org/ishelp/topic_setup_usepreviousappdir.htm).

The [ZIP](../../scripts/package-windows.ps1) contains a versioned directory with `Cadence.exe`, `vcruntime140.dll`, the license, and third-party notices. Portable users should download the new ZIP, close Cadence, extract the whole archive into a new writable folder, and launch the new executable. Do not replace only the EXE or launch an installer from portable mode. No in-place portable replacement or elevation is proposed.

Make install mode explicit at packaging time. Add a marker to installer builds only, and use it to select the Settings action. Do not guess from the folder name, registry entries for another copy, or whether an updater DLL exists. This is a proposed implementation boundary, not existing behavior.

## Release selection and verification

Keep distribution in `ForeverInLaw/windence` GitHub Releases. The [current workflow](../../.github/workflows/release.yml) publishes only version tags, marks suffix tags as prereleases, and leaves manual runs artifact-only. Preserve that policy and the tag/version check. Publish a Windows x64 appcast as an asset of each stable release. Its installer URL must name that exact versioned asset. The stable feed can use GitHub's latest-release asset URL, so no website or feed service is needed. [GitHub release asset links](https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases), [appcast format](https://winsparkle.org/guides/publishing-updates/).

V1 checks stable releases only. Preview builds may open the release page for manual selection. They must not install an older stable release automatically. Automatic preview updates and a channel switch are extra scope. WinSparkle does not implement Sparkle's `sparkle:channel` extension. Separate feeds are needed if that feature is added. [Open channel request](https://github.com/vslavik/winsparkle/issues/248), [pinned parser](https://github.com/vslavik/winsparkle/blob/v0.9.4/src/appcast.cpp).

Generate a dedicated EdDSA key and embed its public key in Cadence. Sign the final installer after all packaging and Authenticode steps. Include its signature, length, version, Windows x64 platform, and minimum Windows version in the appcast. Stop if public-key setup fails. WinSparkle's compatibility path otherwise accepts unsigned downloads. A SHA256 sidecar or GitHub provenance alone does not replace this publisher check. [Signing](https://winsparkle.org/guides/getting-started/), [publishing fields](https://winsparkle.org/guides/publishing-updates/), [pinned download verification](https://github.com/vslavik/winsparkle/blob/v0.9.4/src/updatedownloader.cpp).

Keep all download and release-note URLs on HTTPS. The feed's metadata is separate from the signed payload. Before execution, compare the verified installer's embedded version with the proposed version and reject a downgrade or mismatch. Launch it with application-owned arguments, rather than trusting arbitrary arguments from the feed. WinSparkle exposes a downloaded-installer callback for custom handling. These guards are proposed requirements, not asserted built-in guarantees. [Installer callback](https://winsparkle.org/c-api/callbacks/).

The current release has SHA256 sidecars and GitHub build provenance but no configured Authenticode signing. Authenticode-sign and timestamp our EXE, updater DLL where applicable, and installer before public distribution. It addresses Windows publisher trust separately from EdDSA update verification. Keep Microsoft-signed runtime DLL signatures. Signing does not guarantee immediate SmartScreen trust. [Microsoft signing options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options).

## User flow and state

Installed users choose "Check for updates" in Settings. WinSparkle shows the available version and release notes. Only their "Install update" action downloads and launches the signed installer. Disable automatic checking explicitly before initialization. V1 has no background download or installation. [Manual checking API](https://github.com/vslavik/winsparkle/blob/v0.9.4/include/winsparkle.h), [update flow](https://winsparkle.org/appendix/user-experience/).

Portable users choose "Download portable update". Open the stable release page and show the close, extract, and launch instructions above. A preview release page may be opened explicitly. Files downloaded through the browser are installed manually; Cadence does not verify or apply them. Retain the existing checksum/provenance instructions and signed executable distribution.

WinSparkle's shutdown callbacks run outside the UI thread. Send a quit request to GPUI through a channel. Quit through the existing [services shutdown](../../src/app/services.rs), which saves position and stops the backend. Do not call `process::exit` or stop the player from the callback thread. The installer starts before the shutdown-request callback, so test its handling of files still in use. [Callbacks](https://winsparkle.org/c-api/callbacks/), [Inno close behavior](https://jrsoftware.org/ishelp/topic_setup_closeapplications.htm).

Choose one restart path. The current Inno `[Run]` entry skips silent installs. A silent update needs an explicit post-update launch change or tested Windows Restart Manager registration. Do not assume WinSparkle restarts Cadence. Keep one launched instance and let the [Windows lifecycle](../../src/lifecycle/windows.rs) handle activation. [Inno restart requirements](https://jrsoftware.org/ishelp/topic_setup_restartapplications.htm).

Preserve `%APPDATA%\Cadence\cadence.sqlite3`, its WAL, preferences, queue, history, and logs. Keep keyring service `com.cadence.spotify` and accounts `oauth-token` and `playback-refresh-token` unchanged. These live outside the install folder. Portable currently means no installer, not folder-local user data. Restore saved state without starting audio solely because an update occurred. [Storage](../../src/storage/mod.rs), [OAuth vault](../../src/spotify.rs), [playback vault](../../src/playback.rs).

Cancellation, failed checks, invalid signatures, and failed downloads leave Cadence running. A failed check must show an error rather than "Up to date". Installation errors, low space, or file locks need a visible retry or repair path. Do not promise rollback or uninterrupted playback. Native tests must establish the actual installer behavior.

## Why defer Velopack

Velopack changes installed paths and replaces its `current` directory for updates. It could update every bundled file for both modes without our own helper, but existing users need a packaging migration. [Windows layout](https://docs.velopack.io/packaging/operating-systems/windows).

Its pinned Rust download code authenticates no feed signature. It checks a package's length and feed hash, skips verification of an existing cached package, and extracts a new updater during download. A future adoption needs signed metadata before parsing/download handling and cache revalidation. [Download source](https://github.com/velopack/velopack/blob/1.2.161/src/lib-rust/src/manager.rs).

Its Windows apply code attempts elevation for unwritable folders. Failure after moving the old folder aside can leave the install broken. A future adoption needs native failure tests and a repair plan. These costs outweigh its portable automation benefit for the present scope. [Windows apply source](https://github.com/velopack/velopack/blob/1.2.161/src/bins/src/commands/apply_windows_impl.rs).

## Owner decisions before implementation

1. Accept manual portable updates. If in-place portable updates are required, reopen the packaging decision and evaluate Velopack with the checks above.
2. Choose an Authenticode provider and publisher identity. Azure eligibility depends on region and individual versus organization status. Qualifying open-source projects can apply to SignPath Foundation. No purchase or account was created. [Microsoft options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options).
3. Name the EdDSA key custodian and configure protected signing access for tagged-release CI. Back up the key outside the repository and define rotation/recovery. No private key or GitHub token goes into the player.

These decisions do not block IVO-76 research. Updater implementation, publication, and installation need a separate task.

## Follow-up implementation acceptance

- Pin the WinSparkle x64 DLL and C API. Add a small process-owned Rust adapter. Keep view code independent of native callbacks and retain both existing package formats.
- Add explicit install-mode markers, the two Settings actions, manual-only checking, and safe lifecycle cleanup. Portable mode must never execute an installer.
- Add final-payload signing and appcast generation to tagged releases. Test version/asset matching, stable-only selection, no downgrade, missing assets, unavailable network, invalid signatures, and cancellation. Manual workflow runs publish nothing.
- On native Windows, update version A to B in a per-user install. Verify full EXE/DLL replacement, no unexpected elevation, one restarted instance, released audio/SMTC resources, unchanged credentials/profile data, and saved position.
- Test the shutdown/installer race, installer failure, low space, file locks, custom install directory, and retry/repair. The profile survives repair.
- Exercise portable close/extract/launch with the full ZIP and confirm the same profile and credentials. Document its manual nature. Obtain owner approval before publication or installation on a user's device.

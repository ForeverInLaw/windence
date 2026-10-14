# Microsoft Store delivery

The owner confirmed the first Store publication on 2026-10-14.
[Install Windence](https://apps.microsoft.com/detail/9PJ0NRJRJWH8).
Temporary certification access has been retired. Version 1.3.3 removes
review-code sign-in; ordinary Spotify browser sign-in remains available.

Windence uses Microsoft Store for public installation and updates.
[IVO-79](https://linear.app/ivoxygen/issue/IVO-79/ustanovka-i-avtoobnovleniya-cherez-microsoft-store)
tracks packaging, native checks, CI delivery and publication separately.
CI delivery is enabled. Microsoft Entra access and the published product were
verified in the [Store access check](https://github.com/ForeverInLaw/windence/actions/runs/38623594545).
The first automatic package submission is tracked separately from that read-only check.

Store manages automatic updates. Windence does not add its own update checks,
an Updates section or a reminder banner. CI builds and submits new packages;
Microsoft certifies, signs and delivers them.

## Package identity

These values come from the owner's Partner Center product identity page.
They are public identifiers, not credentials.

| Field | Value |
| --- | --- |
| Name | `redstone.md.Windence` |
| Publisher | `CN=C0C4D47C-912F-43D4-8BBF-FE47FBB8F883` |
| Publisher display name | `redstone.md` |
| Store product ID | `9PJ0NRJRJWH8` |
| Architecture | x64 |
| Minimum Windows version | Windows 10 build 19041, version 2004 |

`packaging/store/AppxManifest.xml` is the manifest template. Packaging replaces
its placeholder version with Cargo's stable `major.minor.patch.0`. Store reserves
the fourth component as zero. Each component must be at most 65535 and the major
component must be nonzero. Increase the Cargo version for each Store update.
Do not change the identity to fix a build or installation problem.

## First upload

1. In GitHub Actions, run **Release** on the branch containing this change.
   It builds the development installer/ZIP and then MSIX from the same binary.
2. Wait for a successful run. Download the `windence-store-x64` artifact and
   extract it. The upload file is `Windence-<version>-windows-x64.msix`.
   Its `.sha256` sidecar is a checksum, not an upload package.
3. In Partner Center, open Windence's first submission, then **Packages**.
   Upload the `.msix`. Do not upload the Actions ZIP or development installer.
   Check the recognized publisher, architecture and four-part version.
4. Complete pricing, properties, age ratings, descriptions, screenshots,
   support information and privacy policy. [PRIVACY.md](../PRIVACY.md) contains
   the text for the inline privacy field. Support uses the project's GitHub page.
5. Explain `runFullTrust` if Partner Center asks. The app runs an existing Win32
   desktop executable using GPUI, browser OAuth with a loopback callback,
   Windows Credential Manager, WASAPI audio and system media controls. It does
   not request administrator rights or install a service.
6. Provide certification notes explaining the Spotify Premium requirement and
   the guided setup of a Spotify developer Client ID. Ensure reviewers can
   complete that setup. Do not include a client secret.
7. After initial installation checks and review, submit for certification in Partner Center.
   Wait until the first version is published and live before enabling CI delivery.

Microsoft signs the public package after certification. No separate paid
certificate or repository signing key is needed for this Store channel. The CI
artifact is unsigned, so double-clicking it is not a supported local installation
path. A single-architecture `.msix` is an accepted Store upload; `.msixupload`
is optional and is not needed for this first package.
[Package requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements),
[first publication requirement](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/overview).

## Build and test locally

On Windows with the pinned Rust toolchain, Visual Studio and Windows SDK:

```powershell
scripts\package-msix.ps1
```

The Store output is `dist\store`. `-SkipBuild` reuses the release binary.
`-ExpectedVersion v1.3.3` checks Cargo against a release tag. MakeAppx performs
its semantic validation; it does not replace certification or native checks.
Logo PNGs are committed. Regenerate them with the existing
`python scripts/build-icon.py` tool and its `resvg-py`/Pillow dependencies.

CI also runs `scripts\test-msix.ps1` through Windows PowerShell on its clean
runner. It unpacks the actual artifact, signs a separate copy with a temporary
certificate, trusts that certificate on the runner, installs the copy and opens
onboarding through its registered app identity. It verifies that SQLite uses
the original profile path. It removes the test package, certificate and test
profile afterward. The upload artifact never receives this signature.
Only run that test on a disposable clean Windows machine with administrator rights.

Use the [Store native checklist](windows-validation.md#store-package) to record
readiness. Initial runtime checks can use a locally test-signed copy. Store
delivery, migration and Store A-to-B checks follow the first publication.
CI onboarding does not prove Spotify login, audio, SMTC or those later checks.
The privacy policy is available from onboarding and Settings. Its link pins
the policy revision shipped with this build, so it works before the first Store
listing and before the packaging branch merges. A policy change must update
that link and the Partner Center policy text together.

## Existing installations

Close the current player completely before launching the Store copy. Both
copies intentionally share the per-user single-instance pipe. Leaving the old
copy running can bring it forward instead of starting the new app.

The profile stays at `%APPDATA%\Cadence`. Credential Manager still uses
`com.cadence.spotify`; this public rename does not change token account names.
Back up the whole profile folder while the app is closed before testing the
transition. Keep the old installer until the Store copy has passed the native
checklist. Then remove the old installation manually. The app does not uninstall
another copy or delete its profile. ZIP means no installer, not folder-local data.

## Microsoft Store installer

Microsoft generates a small EXE that installs the latest certified Windence
version from Store. It uses the Store service and keeps Store-managed updates.
It is available from the Store listing and the README download link.
See [Microsoft's Web Installer guide](https://learn.microsoft.com/en-us/windows/apps/distribute-through-store/how-to-use-store-web-installer-for-distribution).

The Release workflow downloads that file in its own `store-installer` job.
`scripts/download-store-installer.ps1` uses the URL in Microsoft's official
[badge implementation](https://get.microsoft.com/badge/ms-store-badge.bundled.js).
It requires a valid Authenticode signature from Microsoft Corporation and writes
a SHA256 sidecar. The `windence-store-installer` artifact contains both files.
This job needs no Partner Center credentials and does not build or sign an installer.

After package validation, Microsoft signature validation and a successful Store
submission, the `github-release` job creates a GitHub release with the Microsoft
installer, unsigned MSIX and their checksums. Only this job has `contents: write`;
it has no Partner Center credentials. Manual runs create Actions artifacts only.
Release notes combine `docs/releases/store-downloads.md`, optional notes for the
tagged version and GitHub-generated changes. They identify the MSIX as a Partner
Center artifact. The Store installer downloads the version currently certified
for the user's market; a new source tag does not skip certification.

The README Windows screenshot was downloaded unchanged from the published
[Windence Store listing](https://apps.microsoft.com/detail/9PJ0NRJRJWH8) on
2026-10-14. It replaces the upstream Cadence screenshot.

## Configure CI/CD

The official Store CLI currently supports updates to **free** products only.
It cannot create this app or publish its first loose MSIX submission.
[Microsoft's setup guide](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/github-actions).

After first publication, the owner performs these one-time steps:

1. Associate a Microsoft Entra tenant with the Partner Center account.
2. Register an automation application in Entra. Add it in Partner Center's
   **Account settings > User management > Microsoft Entra applications** with
   the **Manager** role.
3. Create the `store` GitHub Environment. Allow the `windows-port` branch for the
   manual access check and `v*.*.*` tags for releases. A required reviewer is
   optional if you want to approve each release.
   Add environment secrets
   `MSSTORE_TENANT_ID`, `MSSTORE_SELLER_ID`, `MSSTORE_CLIENT_ID` and
   `MSSTORE_CLIENT_SECRET`. Use the secret value, not its ID. Keep it out of chat,
   commits and issue descriptions. Track its expiry date.
4. After merging the workflow into `windows-port`, run **Check Microsoft Store
   access** from GitHub Actions on that branch. It reads the product and checks
   identity, first publication and pending submissions without building or
   submitting a package. It does not print credentials or listing metadata.
   Resolve any access error or pending submission before enabling delivery.
5. In repository **Settings > Secrets and variables > Actions > Variables**,
   set `STORE_PUBLISH_ENABLED` to `true`. Leave it unset until access is ready.

Stable `v<major>.<minor>.<patch>` tags then build and test the MSIX. The protected
Store job only accepts commits already merged into `windows-port`. It downloads
that run's exact unsigned artifact and uses official CLI `v0.4.3` with the setup
action pinned to its v1.4 commit.

Before submission, the job validates the package, reads the Store product through
the CLI and requires matching identity, an existing published version and no
pending submission. It then creates its own submission through the official
Store API. A competing draft makes creation fail; the job never deletes a draft.
Only the returned submission ID is updated, uploaded and committed. Existing
listing, pricing and certification notes are preserved. A failed upload or commit
stops delivery and leaves the API-created submission for manual recovery.
Do not edit an API-created submission in Partner Center while CI is running.

If delivery fails, inspect Partner Center before retrying. Public logs show only
the operation and HTTP status, never provider messages or upload URLs. When a
delivery fix needs newer code, run **Retry Microsoft Store delivery** on
`windows-port` with the failed Release run ID. It checks the source tag and
successful Windows build, downloads that run's exact MSIX and uses the current
delivery scripts. It requires no pending submission and never deletes a draft.
This avoids rebuilding the package or moving an existing release tag.

The job serializes Store submissions across tags. If a submission is pending,
finish it in Partner Center before retrying. A delivery success means the submission
was sent, not that Microsoft finished certification or users received the update.
Development EXE/ZIP and preview tags remain Actions artifacts. Stable tags also
publish the Microsoft Store installer and MSIX upload package to GitHub Releases.

[CLI draft handling](https://github.com/microsoft/msstore-cli/blob/v0.4.3/MSStore.CLI/Helpers/IStorePackagedAPIExtensions.cs),
[Store API submission creation](https://learn.microsoft.com/en-us/windows/uwp/monetize/create-an-app-submission),
[package upload and commit](https://learn.microsoft.com/en-us/windows/uwp/monetize/manage-app-submissions).

## Verified first package

This 1.3.0 package is historical evidence. It still has a Cadence sidebar label
and its development installer uses the old name. Use the latest verified build
below for screenshots and replace the draft's package before certification.

Source commit `af634a5`, checked on 2026-10-12:

- [Successful Release run](https://github.com/ForeverInLaw/windence/actions/runs/38317647506).
- [Store artifact](https://github.com/ForeverInLaw/windence/actions/runs/38317647506/artifacts/11777613730),
  `Windence-1.3.0-windows-x64.msix`, package version `1.3.0.0`, 16,878,558 bytes.
- SHA256 `9be4f232ff7d60d742048fbd8ef0af6e5c4d13b0f6f94cd309afd0beda52a68a`.
- MakeAppx validation, unsigned artifact inspection and the temporary signed
  installation/launch test passed on Windows Server 2025, build 26100.
- Installed family `redstone.md.Windence_dxxngvs4d91sj` matches Partner Center.
  The profile was created in `C:\Users\runneradmin\AppData\Roaming\Cadence`.
- Linux passed 373 portable tests, formatting and strict all-target Clippy.
  PowerShell parsed, submission guards passed and actionlint accepted the workflow.
- The policy link returned HTTP 200 without GitHub authentication.

The Store publishing job was skipped. The repository publishing flag is unset.
No Store submission or public release was created. Client Windows 10/11,
authenticated playback and Store-delivered migration/update checks remain open.
GitHub retains this artifact until 2026-10-26; it can be rebuilt from the branch.

## Corrected public name, 1.3.1

Source commit `07f3500`, checked on 2026-10-13:

- The sidebar label, application menu and user-facing errors use Windence.
- The development installer, ZIP, executable and shortcuts use Windence.
  The Inno AppId stays the same, so an upgrade uses the previous installation
  directory and profile. It removes only the old executable and generated
  shortcuts. Store installation still does not remove another installation.
- Shared payload packaging checks the actual EXE's product name, description
  and original filename. Stale binary metadata stops packaging.
- [Successful Release run](https://github.com/ForeverInLaw/windence/actions/runs/38320850270).
- [Store artifact](https://github.com/ForeverInLaw/windence/actions/runs/38320850270/artifacts/11779320198),
  `Windence-1.3.1-windows-x64.msix`, package version `1.3.1.0`.
- MSIX SHA256 `918a34ab34c7c7f2cfb5fbf6f72f36eff974dbd29bd066e46c519dad75b0bf38`.
- [Development artifact](https://github.com/ForeverInLaw/windence/actions/runs/38320850270/artifacts/11778444511)
  contains `Windence-1.3.1-windows-x64-setup.exe` and the matching ZIP.
- Checksums, unsigned MSIX identity and ZIP payload were verified after download.
  CI built the installer, installed a separately test-signed MSIX and opened it
  on Windows Server 2025. Portable tests, formatting, strict Clippy and actionlint
  passed. The capture script accepted the previous Cadence-named ZIP and the new
  Windence-named ZIP. Light and dark onboarding captures were inspected.

An Inno upgrade on client Windows and the signed-in main window still need the
owner's check. The data directory, credentials and single-instance identity
retain their existing names. This change does not migrate user data.

This build completed the public rename. For certification, use the 1.3.2 build
below, which also adds temporary review access. CI did not send the submission
or publish the app.

## First Store publication

The owner confirmed publication on 2026-10-14. Store product ID:
`9PJ0NRJRJWH8`. The first submitted package was version 1.3.2.0 from `a88d389`.
Its [Release run](https://github.com/ForeverInLaw/windence/actions/runs/38329998805)
passed package validation, test-signed installation and native onboarding.
The owner uploaded the package and completed certification in Partner Center.
Publication does not verify the remaining native or Store A-to-B checks.

## Certification access retired, 1.3.3

On 2026-10-14 the owner requested retirement of temporary review access.
The broker code and stored Spotify tokens were cleared. Both review containers,
their network and Caddy volumes were removed. All eight unrelated containers
kept their IDs and start times. The review endpoint no longer accepts connections.

Version 1.3.3 removes the review form, broker client, backend command and special
playback/refresh path. The service source, owner helper and their CI jobs were
removed. Normal Spotify authorization, profile paths and Credential Manager
entries keep working through their existing paths.

The already published 1.3.2 package still contains the old Review access option.
Its server access is disabled. Upload the 1.3.3 MSIX to replace that UI in Store,
and update the inline privacy policy using [PRIVACY.md](../PRIVACY.md).
Retired access cannot be used for later certification. Future certification notes
must describe ordinary sign-in and provide any reviewer access Microsoft requires.
See the [retirement record](store-review-access.md).

# Microsoft Store installation and updates

Status: accepted, 2026-10-12. Scope: [IVO-79](https://linear.app/ivoxygen/issue/IVO-79/ustanovka-i-avtoobnovleniya-cherez-microsoft-store).

Updated on 2026-10-13: the owner removed in-app update checks and reminders
from scope. Store manages automatic delivery; CI submits new versions to Store.

Updated on 2026-10-14: the owner confirmed the first Store publication.
Temporary certification access is retired. The broker and its stored tokens
were removed, and version 1.3.3 removes the review-code sign-in path.
Regular Spotify browser sign-in remains the only authentication path.

Updated on 2026-10-14: Microsoft Entra access is verified and automatic Store
submission is enabled for stable tags merged into `windows-port`. GitHub
releases can include Microsoft's Store Web Installer and the unsigned MSIX
upload artifact. The Microsoft installer retrieves the latest certified Store
version. It does not create another update channel or require our own certificate.

The Windows Port's public name is Windence. It uses an MSIX package distributed
through Microsoft Store. Microsoft signs the package after certification and
delivers updates. The owner does not plan to buy a separate signing certificate.
EXE and ZIP remain development artifacts in GitHub Actions.

This replaces the WinSparkle recommendation in the earlier
[updater research](../research/windows-updater.md). That research evaluated the
previous EXE/ZIP distribution model. We keep it as a record of those options.

The package runs the existing GPUI executable as `win32App` at `mediumIL`.
It requires Windows 10 build 19041 or newer, x64. It does not use AppContainer.
The existing `%APPDATA%\Cadence` profile, Windows Credential Manager service
names and single-instance pipe keep their names for compatibility. Renaming
the public app must not create another profile or lose saved authentication.
Native migration and Store updates still need verification.

The first package was built in Windows CI and uploaded manually by the owner.
Later stable versions can use the Store delivery pipeline after the owner
configures API access. An existing pending submission blocks automation so it does not
overwrite listing changes. The CLI reads product identity; the official Store
API creates and commits the update. A competing draft stops creation without
deleting anything. Store certification remains separate from CI success.

Microsoft Store manages update checks, downloads and installation. Windence
does not add an Updates section, an update banner or its own check schedule.
The remaining delivery work is the first automatic package submission and
verification that Store updates preserve the existing profile and saved playback state.

Sources: [distribution paths](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/choose-distribution-path),
[MSIX package requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements),
[CLI prerequisites](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/overview),
[automatic Store updates](https://support.microsoft.com/en-us/accounts-billing/get-updates-for-apps-and-games-in-microsoft-store).

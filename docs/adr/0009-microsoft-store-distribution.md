# Microsoft Store installation and updates

Status: accepted, 2026-10-12. Scope: [IVO-79](https://linear.app/ivoxygen/issue/IVO-79/ustanovka-i-avtoobnovleniya-cherez-microsoft-store).

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

The first package is built in Windows CI and uploaded manually by the owner.
First certification and publication happen in Partner Center. Later stable
versions can use Microsoft's Store Developer CLI after the owner configures
API access. An existing pending submission blocks automation so it does not
overwrite listing changes. Store certification remains separate from CI success.

In-app update checks and reminders are a later layer of IVO-79. They report
Store availability and open Store. They do not download or replace executable
files, and do not control Store's own automatic update setting.

Sources: [distribution paths](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/choose-distribution-path),
[MSIX package requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements),
[CLI prerequisites](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/overview).

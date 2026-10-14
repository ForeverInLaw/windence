Install Windence from [Microsoft Store](https://apps.microsoft.com/detail/9PJ0NRJRJWH8).
Microsoft signs the app and delivers updates through Store.

### Downloads

- **Windence-Store-Installer.exe** is Microsoft's small installer. It downloads
  the latest certified Windence version from Store. Its Microsoft signature is
  verified in Windows CI.
- The versioned **.msix** is an unsigned upload package for Partner Center.
  Use the Store installer above for normal installation.
- The `.sha256` files contain checksums for the corresponding downloads.

Store availability can lag behind this source release while certification completes.

### Requirements

Windows 10 version 2004 (build 19041) or newer, or Windows 11, on an x64 PC.
Spotify Premium and a Spotify developer Client ID are required. Windence guides
you through Client ID setup and browser sign-in on first launch.

Close an older Cadence or Windence installation before opening the Store copy.
The app uses the existing `%APPDATA%\Cadence` profile and Credential Manager
entries. The [migration checklist](https://github.com/ForeverInLaw/windence/blob/windows-port/docs/windows-validation.md#store-package)
explains how to verify the transition before removing the older installation.

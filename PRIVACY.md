# Windence privacy policy

Last updated: 14 October 2026.

Windence is an independent Spotify player for Windows. It is not affiliated
with Spotify or Microsoft. The project is maintained at
[ForeverInLaw/windence](https://github.com/ForeverInLaw/windence).

## Data the application uses

When you connect Spotify, Windence accesses account information such as your
Spotify account identifier, display name, profile image and country, along with
your saved music, playlists and playback information. It uses these to display
your library and play music. Searches, playback reports and actions such as
saving a track send requests directly to Spotify.

If you turn on "Report plays to Spotify" in Settings, Windence also tells Spotify
which track you played, for how long and from which playlist or album, along with
an id for this device. Spotify may then add the play to your history. This setting
is off by default.

Spotify handles sign-in in your browser. Windence does not ask for or store your
Spotify password. It uses authorization and playback tokens to access Spotify.
The Spotify developer Client ID you enter is public configuration, not a client
secret. Windence does not ask for a client secret.

## Data stored on your device

Windence stores preferences, its library cache, queue, history, current track
and playback position locally. On Windows the profile folder is
`%APPDATA%\Cadence`. The historical folder name stays the same when the app is
installed from Microsoft Store. Authentication tokens use Windows Credential
Manager. Other local application data is not encrypted by Windence.

Local log files help diagnose errors and are rotated to limit their size. Logs
can contain technical details and identifiers. The application does not
automatically upload them. Review and remove personal information before sharing
logs or screenshots. Do not publish authentication tokens.

## Third-party services

Spotify and the servers providing music and images receive requests necessary
for those services, including network information such as your IP address and
authorization information where required. Their handling of data follows
[Spotify's privacy policy](https://www.spotify.com/legal/privacy-policy/).

Microsoft Store handles Store installation and updates under
[Microsoft's privacy statement](https://privacy.microsoft.com/privacystatement).
When you visit the project's GitHub page or submit an issue, GitHub handles that
interaction under [GitHub's privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

The Windence application does not sell personal data, run advertising or a
separate analytics service, or automatically send your Spotify account, listening
history or local logs to the project maintainer.

## Retired certification access

Temporary Microsoft Store certification access was retired on 14 October 2026.
The service was shut down and its stored account tokens were removed. Review
codes no longer grant access to the service. Older app versions may still show
the Review access option. Ordinary Spotify sign-in uses Spotify directly.

## Your controls

You can sign out in Windence and revoke its Spotify access in your Spotify
account's application settings. Signing out removes stored authorization,
library cache and saved playback state. It does not delete every preference,
history entry or log file. To remove remaining local application data, close
Windence completely and delete the profile folder above. Spotify controls data
held by Spotify; use its account tools for those records.

## Contact and changes

For privacy questions, contact the maintainer through
[the project's GitHub issues](https://github.com/ForeverInLaw/windence/issues).
GitHub issues are public. Do not include private account details or secrets.

Changes to this policy will update the date and text in the project repository.

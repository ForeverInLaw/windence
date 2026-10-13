# Temporary access for Store certification

The owner can let Microsoft reviewers use their Spotify Premium session without
sharing a mailbox or password. This is real library access and playback. It does
not verify the normal browser sign-in flow or guarantee Store certification.
Explain this method in Partner Center's private Notes for certification.

## Service

`services/review-broker` runs separately from other services. Caddy terminates
HTTPS for `spotic.qvq.cz`. The broker uses the owner's two Spotify refresh
credentials to supply short-lived library and playback tokens. Refresh
credentials stay on the server. The app checks that both sessions belong to
the same Spotify account.

Library requests recheck access at least every five minutes while used.
Spotify controls the actual access-token lifetime; the service cannot shorten
it or terminate an already connected playback session.

The public API exposes `/healthz` and `POST /v1/session`. Provisioning and
revocation use `docker exec` locally or over SSH. There is no public admin API.
Review codes contain 256 random bits, expire after 30 days, and are stored only
as hashes. The service does not log requests, tokens or review codes. State files
are mode 0600 and live outside the repository at `/var/lib/windence-review`.

Before deployment, check ports, addresses and available resources. Copy
`.env.example` to `.env` and set `BIND_IP` locally. The example address is reserved
for documentation. Never commit the real address or the environment file.
Bind HTTPS to the public address rather than all interfaces so a private
Tailscale listener can continue to use its own port 443. The compose project
has its own network and containers; do not restart another compose project or
change the server firewall for this service.

```sh
docker compose -f services/review-broker/compose.yaml config --quiet
docker compose -f services/review-broker/compose.yaml build broker
install -d -m 700 -o 10001 -g 10001 /var/lib/windence-review
docker compose -f services/review-broker/compose.yaml up -d
curl --fail https://spotic.qvq.cz/healthz
```

The service starts with review access disabled. The broker is limited to 256 MB
and half a CPU; Caddy is limited to 128 MB and half a CPU. Only the broker's
loopback port is exposed locally. Do not mount session state in other containers.
Build dependencies are separate from the Rust application. Test the broker with:

```sh
docker run --rm --network none --memory 256m --cpus 0.5 \
  -v "$PWD/services/review-broker:/tests:ro" -w /tests \
  windence-review-broker python -m unittest -v test_app
```

## Enable access from Windows

1. Sign in normally with the owner's account and play a track.
2. Copy the public Client ID from Settings, then quit completely with Ctrl+Q.
3. Run the helper below as the same Windows user. It reads Windence's existing
   Credential Manager entries and sends them through SSH stdin. It does not
   export credentials to a file or pass them as command arguments.
4. Paste the code printed in the local console into Partner Center. Keep it out
   of issues, chat, screenshots and CI logs.

```powershell
.\scripts\review-access.ps1 -Action Enable -ClientId '<public-client-id>' -Port <ssh-port>
```

Use `-SshUser` and `-RemoteHost` if the SSH connection needs another user or host.
SSH access is needed only by the owner to provision the service, not by reviewers.
The helper needs Windows OpenSSH and access to Docker on the server. If either
saved credential is unavailable, sign in again before enabling access. Credentials
can rotate; run the helper again if the broker stops renewing successfully.

## Verify and submit

Test on a clean Windows profile using the same package that will be submitted.
On the setup screen choose **Review access**, paste the code and select
**Connect**. Check the library, playlist search, audio, seeking, queue, SMTC and
reconnection. After restarting, enter the code again. The code and review tokens
are kept only in memory. The normal browser login remains available separately.
If an older developer-app configuration is present, use **Change developer app**
first; this signs out and clears the cached account state.

Replace the existing MSIX in the draft submission with the new version containing
review access. Update Partner Center's inline privacy policy from
[PRIVACY.md](../PRIVACY.md), including its optional certification access section.
Use these notes, replacing the code placeholder privately:

```text
Windence is an independent Spotify player requiring Spotify Premium.
The owner provides temporary access to their Premium account for certification.
This uses the actual Spotify library and audio playback, without browser login.

1. On the setup screen select Review access.
2. Enter this review code: [PRIVATE REVIEW CODE]
3. Select Connect, open a playlist and play a track.
4. Test search within a playlist, queue, seeking and Windows media controls.
5. Press Ctrl+Q to quit completely. Enter the same code after relaunching.

Ordinary users sign in through Spotify in their browser. The review access code
is provided only for certification. Please report if browser sign-in must also
be tested with separate credentials.
```

## Revoke after review

```powershell
.\scripts\review-access.ps1 -Action Disable -Port <ssh-port>
```

This removes the server's stored tokens and prevents issuing further tokens.
Existing access tokens and playback connections may still work: also revoke both
Spotify app permissions in the account's Apps page and sign out everywhere.
A password change alone is not the revocation mechanism for OAuth permissions.
Reauthorize the owner's own Windence installation afterwards. While reviewers
use the account, avoid simultaneous playback on other devices.

Sources: [Microsoft certification notes](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/manage-submission-options#notes-for-certification),
[Spotify token renewal](https://developer.spotify.com/documentation/web-api/tutorials/refreshing-tokens),
[Spotify sign-out and app access](https://support.spotify.com/us/article/how-to-log-out/).

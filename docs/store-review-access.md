# Retired Store certification access

Temporary review access was retired on 2026-10-14 after the owner confirmed
Windence's first Microsoft Store publication.

The review code was revoked and the broker's stored Spotify credentials were
cleared before shutdown. The broker and Caddy containers, their network and TLS
volumes were removed. All eight unrelated containers kept their IDs and start
times. The review endpoint no longer accepts connections.

Version 1.3.3 removes the review-code form, broker client and special library,
playback and token-refresh paths. It also removes the service source, owner
PowerShell helper and their CI jobs. The published 1.3.2 package may still show
Review access, but the service is disabled. Ordinary browser sign-in remains
available in both versions.

Previously issued Spotify access tokens can remain valid until Spotify expires
them. To invalidate them sooner, the account owner can revoke the two Spotify
application grants and sign in to Windence again. Server credentials and review
codes are no longer available for renewal.

For later submissions, describe ordinary browser sign-in in Partner Center and
provide reviewer access if Microsoft requests it. Update the inline privacy
policy from [PRIVACY.md](../PRIVACY.md). Do not put credentials in Git or Linear.

The removed implementation is recorded in commit `a88d389`; review fixes are
recorded in `8018b54` and `2b05d3a`. There is no supported reprovisioning path.

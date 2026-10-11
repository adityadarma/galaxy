# Release Packaging

Local debug builds remain ad-hoc signed. Distribution builds use:

```sh
DEVELOPER_ID_APPLICATION="Developer ID Application: Company (TEAMID)" \
APPLE_ID="release@example.com" APPLE_TEAM_ID="TEAMID" \
APPLE_APP_PASSWORD="@keychain:AC_PASSWORD" \
scripts/package-release.sh
scripts/create-dmg.sh
```

`package-release.sh` builds Release, signs with hardened runtime, verifies the
bundle, creates a zip and SHA-256 file, and optionally submits to Apple
notarization. It never requires credentials unless notarization variables are
provided. Credentials must come from the environment or keychain, never from
tracked files.

## Sparkle

Sparkle is not bundled yet. Adding it requires pinning a signed Sparkle release,
embedding its framework, generating an EdDSA appcast signing key outside the
repository, and configuring `SUFeedURL` plus the public key in the app. Until
those are available, auto-update is intentionally disabled rather than shipping
an unsigned or unverifiable update channel.

For closed-beta distribution and tester acceptance, use `docs/BETA-README.md`,
`docs/BETA-FEEDBACK.md`, and `docs/BETA-TRIAGE.md`.

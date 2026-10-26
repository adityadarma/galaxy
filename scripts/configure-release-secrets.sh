#!/usr/bin/env bash
# Uploads Galaxy's signing backup and Sparkle key to GitHub Actions. It creates
# the signing identity through make-signing-cert.sh when no backup exists.
set -euo pipefail

REPO="${1:-adityadarma/galaxy}"
IDENTITY="Galaxy Self-Signed"
SPARKLE_ACCOUNT="dev.adityadarma.galaxy"
BACKUP_DIR="${GALAXY_SIGNING_DIR:-$HOME/.galaxy-signing}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATE_KEYS="$ROOT/app/build/SourcePackages/artifacts/sparkle/Sparkle/bin/generate_keys"
MAKE_CERT="$ROOT/scripts/make-signing-cert.sh"

usage() {
    printf 'Usage: %s [owner/repo]\n' "$0"
    printf 'Creates/uses %q and uploads release secrets to GitHub Actions.\n' "$IDENTITY"
}

[[ $# -le 1 ]] || { usage >&2; exit 2; }
command -v gh >/dev/null || { printf 'GitHub CLI (gh) is required.\n' >&2; exit 1; }
command -v openssl >/dev/null || { printf 'openssl is required.\n' >&2; exit 1; }
[[ -x "$GENERATE_KEYS" ]] || {
    printf 'Sparkle generate_keys was not found at %s. Build Galaxy once first.\n' "$GENERATE_KEYS" >&2
    exit 1
}
gh repo view "$REPO" >/dev/null
P12="$BACKUP_DIR/galaxy-signing.p12"
PASSFILE="$BACKUP_DIR/galaxy-signing.p12.password"
bash "$MAKE_CERT"
[[ -f "$P12" && -f "$PASSFILE" ]] || { printf 'Signing backup was not created.\n' >&2; exit 1; }

SPARKLE_PRIVATE="$BACKUP_DIR/sparkle-private-key.txt"
SPARKLE_PUBLIC="$BACKUP_DIR/sparkle-public-key.txt"
if [[ ! -f "$SPARKLE_PRIVATE" ]]; then
    "$GENERATE_KEYS" --account "$SPARKLE_ACCOUNT" >/dev/null
    "$GENERATE_KEYS" --account "$SPARKLE_ACCOUNT" -x "$SPARKLE_PRIVATE"
    chmod 600 "$SPARKLE_PRIVATE"
fi
"$GENERATE_KEYS" --account "$SPARKLE_ACCOUNT" -p > "$SPARKLE_PUBLIC"
chmod 600 "$SPARKLE_PUBLIC"

base64 < "$P12" | gh secret set GALAXY_SELF_SIGN_P12_BASE64 --repo "$REPO"
gh secret set GALAXY_SELF_SIGN_P12_PASSWORD --repo "$REPO" < "$PASSFILE"
gh secret set SPARKLE_PRIVATE_KEY --repo "$REPO" < "$SPARKLE_PRIVATE"
gh variable set SPARKLE_PUBLIC_KEY --repo "$REPO" < "$SPARKLE_PUBLIC"

printf 'Configured GitHub Actions secrets for %s.\n' "$REPO"
printf 'Back up %s securely; it is required for future updates.\n' "$BACKUP_DIR"
gh secret list --repo "$REPO"
gh variable list --repo "$REPO"

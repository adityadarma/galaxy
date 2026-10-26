#!/usr/bin/env bash
# Creates Galaxy's persistent self-signed code-signing identity once.
set -euo pipefail

IDENTITY="Galaxy Self-Signed"
BACKUP_DIR="${GALAXY_SIGNING_DIR:-$HOME/.galaxy-signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
P12="$BACKUP_DIR/galaxy-signing.p12"
PASSFILE="$BACKUP_DIR/galaxy-signing.p12.password"

if [[ -f "$P12" && -f "$PASSFILE" ]]; then
    printf 'Using existing %s backup in %s.\n' "$IDENTITY" "$BACKUP_DIR"
    exit 0
fi

if security find-identity -v -p codesigning "$KEYCHAIN" | grep -q "\"$IDENTITY\""; then
    printf 'A valid %s identity exists, but its PKCS#12 backup is missing.\n' "$IDENTITY" >&2
    printf 'Export that existing identity instead of generating a replacement.\n' >&2
    exit 1
fi

mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/certificate.conf" <<EOF
[req]
distinguished_name = subject
x509_extensions = extensions
prompt = no

[subject]
CN = $IDENTITY

[extensions]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF

# The identity must remain valid for every release that should preserve macOS
# permissions and be eligible to replace an installed Galaxy build.
openssl req -x509 -newkey rsa:3072 -sha256 -days 7300 -nodes \
    -config "$WORK/certificate.conf" -keyout "$WORK/key.pem" -out "$WORK/certificate.pem" 2>/dev/null
openssl rand -base64 24 > "$PASSFILE"
chmod 600 "$PASSFILE"

# Legacy PKCS#12 encryption is required for macOS security import compatibility.
openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/certificate.pem" \
    -name "$IDENTITY" -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 \
    -passout "file:$PASSFILE" -out "$P12"
chmod 600 "$P12"

PASSWORD="$(<"$PASSFILE")"
security import "$P12" -k "$KEYCHAIN" -f pkcs12 -P "$PASSWORD" -T /usr/bin/codesign >/dev/null
# Self-signed certificates have no CA chain. Trust it locally so macOS reports
# it as a valid code-signing identity, matching the temporary CI keychain.
security add-trusted-cert -d -r trustRoot -k "$KEYCHAIN" "$WORK/certificate.pem"

IDENTITY_SHA1="$(security find-identity -v -p codesigning "$KEYCHAIN" | awk -F'"' -v name="$IDENTITY" '$2 == name { print $1 }' | awk '{ print $2 }' | tail -n 1)"
[[ -n "$IDENTITY_SHA1" ]] || { printf 'Certificate import did not create a valid signing identity.\n' >&2; exit 1; }

printf 'Created and imported %q.\n' "$IDENTITY"
printf 'Code-signing identity SHA-1: %s\n' "$IDENTITY_SHA1"
printf 'Private backup files: %s\n' "$BACKUP_DIR"

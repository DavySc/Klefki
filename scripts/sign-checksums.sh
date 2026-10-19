#!/usr/bin/env bash
# Signs a release's SHA256SUMS with minisign and checks the signature against the committed public key.
#
#   MINISIGN_SECRET_KEY=... MINISIGN_PASSWORD=... scripts/sign-checksums.sh dist/SHA256SUMS "trusted comment"
#
# Signing is switched on by committing `minisign.pub` at the repository root. Until then this prints a notice
# and does nothing. Once the public key is committed, a missing secret or a signature that does not verify
# fails the release. The secret key is written only to a private temporary file, removed on exit.
set -euo pipefail

sums=$1
comment=$2
pub="$(dirname "$0")/../minisign.pub"

if [[ ! -f "$pub" ]]; then
  echo "::notice::minisign.pub is not committed yet, so $sums is not signed (see docs/RELEASING.md)."
  exit 0
fi
if [[ -z "${MINISIGN_SECRET_KEY:-}" ]]; then
  echo "::error::minisign.pub is committed but the MINISIGN_SECRET_KEY secret is not set."
  exit 1
fi

key=$(umask 077 && mktemp)
trap 'rm -f "$key"' EXIT
printf '%s\n' "$MINISIGN_SECRET_KEY" > "$key"
printf '%s\n' "${MINISIGN_PASSWORD:-}" | minisign -S -s "$key" -m "$sums" -t "$comment" > /dev/null
minisign -V -p "$pub" -m "$sums"

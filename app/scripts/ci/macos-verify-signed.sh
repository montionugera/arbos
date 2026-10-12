#!/usr/bin/env bash
# Refuse to publish anything Gatekeeper would refuse: the bundle must carry a
# valid Developer ID signature and a stapled notarization ticket, and so must
# the disk image when there is one. Exits non-zero on the first failure.
# usage: macos-verify-signed.sh <Something.app> [disk image]
set -euo pipefail
app="$1"
dmg="${2:-}"

fail() {
  echo "error: $*" >&2
  exit 1
}

[[ -d "$app" ]] || fail "$app is not an app bundle"
echo "verifying $app"
codesign --verify --deep --strict --verbose=2 "$app"
# An ad-hoc or development signature verifies fine; only Developer ID passes
# Gatekeeper on another Mac, so check the chain explicitly.
codesign --display --verbose=2 "$app" 2>&1 | grep -q '^Authority=Developer ID Application' \
  || fail "$app is not signed with a Developer ID Application certificate"
spctl --assess --type execute --verbose=2 "$app"
xcrun stapler validate "$app"

if [[ -n "$dmg" ]]; then
  [[ -f "$dmg" ]] || fail "$dmg does not exist"
  echo "verifying $dmg"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
  xcrun stapler validate "$dmg"
fi
echo "signed and notarized"

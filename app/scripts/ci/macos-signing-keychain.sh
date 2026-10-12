#!/usr/bin/env bash
# Import a Developer ID Application certificate into a throwaway keychain so
# codesign can use it without a prompt, and export CODESIGN_IDENTITY.
#
# A temporary keychain rather than the runner's login keychain: codesign must
# find exactly one identity, nothing may prompt, and deleting the whole file
# afterwards is the one reliable way to leave no private key on the runner.
#
# usage: macos-signing-keychain.sh import <cert.p12>   p12 password in CERT_PASSWORD
#        macos-signing-keychain.sh remove
# Env:   SIGNING_KEYCHAIN    keychain path (default $RUNNER_TEMP/arbos-signing.keychain-db)
#        ARBOS_APPLE_TEAM_ID when set, the identity must end in "(<team id>)"
#        GITHUB_ENV          CODESIGN_IDENTITY and SIGNING_KEYCHAIN are appended when set
set -euo pipefail
# The certificate password is passed on a command line below. Never trace it.
set +x

keychain="${SIGNING_KEYCHAIN:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/arbos-signing.keychain-db}"

fail() {
  echo "error: $*" >&2
  exit 1
}

import() {
  local p12="$1"
  [[ -f "$p12" ]] || fail "certificate file $p12 does not exist"
  [[ -n "${CERT_PASSWORD:-}" ]] || fail "CERT_PASSWORD (the .p12 password) is not set"
  local keychain_password
  keychain_password="$(openssl rand -hex 24)"

  security create-keychain -p "$keychain_password" "$keychain"
  # No auto-lock and no lock on sleep: notarization can take a while.
  security set-keychain-settings -lut 21600 "$keychain"
  security unlock-keychain -p "$keychain_password" "$keychain"
  # Put the new keychain first in the user search list and keep the rest.
  local -a search=()
  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%\"}"
    line="${line#\"}"
    [[ -n "$line" ]] && search+=("$line")
  done < <(security list-keychains -d user)
  # (`${arr[@]+...}`: an empty array is an unbound variable to macOS's bash 3.2.)
  security list-keychains -d user -s "$keychain" ${search[@]+"${search[@]}"}

  security import "$p12" -k "$keychain" -P "$CERT_PASSWORD" -f pkcs12 \
    -T /usr/bin/codesign -T /usr/bin/security
  # Let codesign use the key without a UI prompt. The command dumps the
  # keychain items it touched; nothing in there is needed in the log.
  security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null

  local identity
  identity="$(security find-identity -v -p codesigning "$keychain" \
    | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -n 1)"
  [[ -n "$identity" ]] || fail "the certificate holds no valid Developer ID Application identity"
  if [[ -n "${ARBOS_APPLE_TEAM_ID:-}" && "$identity" != *"(${ARBOS_APPLE_TEAM_ID})" ]]; then
    fail "the identity \"$identity\" does not belong to team ${ARBOS_APPLE_TEAM_ID}"
  fi
  echo "signing identity: $identity"
  if [[ -n "${GITHUB_ENV:-}" ]]; then
    printf 'CODESIGN_IDENTITY=%s\nSIGNING_KEYCHAIN=%s\n' "$identity" "$keychain" >>"$GITHUB_ENV"
  fi
}

remove() {
  if [[ -f "$keychain" ]]; then
    security delete-keychain "$keychain"
    echo "removed $keychain"
  fi
}

case "${1:-}" in
  import) [[ $# -eq 2 ]] || fail "usage: $0 import <cert.p12>"; import "$2" ;;
  remove) remove ;;
  *) fail "usage: $0 import <cert.p12> | remove" ;;
esac

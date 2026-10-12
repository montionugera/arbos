#!/usr/bin/env bash
# Exercise the Mac release scripts on any OS with stand-ins for the Apple
# tools and gh. Each stub appends its arguments to $STUB_LOG, so the checks
# below are about which commands run with which arguments, not about Apple.
# Run: app/scripts/ci/test-macos-release-scripts.sh
set -euo pipefail
ci="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export STUB_LOG="$work/calls.log"
export STUB_STATE="$work/state"
mkdir -p "$work/bin" "$STUB_STATE"
export PATH="$work/bin:$PATH"

stub() {
  local name="$1"
  shift
  printf '#!/usr/bin/env bash\nprintf "%%s\\n" "%s $*" >>"$STUB_LOG"\n%s\n' "$name" "$*" >"$work/bin/$name"
  chmod +x "$work/bin/$name"
}

fail() {
  echo "FAIL: $*" >&2
  echo "--- calls" >&2
  cat "$STUB_LOG" >&2 2>/dev/null || true
  exit 1
}

expect_call() {
  grep -qF -- "$1" "$STUB_LOG" || fail "expected a call matching: $1"
}

reject_call() {
  if grep -qF -- "$1" "$STUB_LOG"; then fail "unexpected call: $1"; fi
}

# ---------------------------------------------------------------- keychain
stub openssl 'echo deadbeef'
stub security '
case "$1" in
  list-keychains)
    if [ "$2" = "-d" ] && [ "$#" -eq 3 ]; then
      printf "    \"/Users/runner/Library/Keychains/login.keychain-db\"\n    \"/Library/Keychains/System.keychain\"\n"
    fi ;;
  find-identity) cat "$STUB_STATE/identities" ;;
  create-keychain) : >"${@: -1}" ;;
esac'

run_import() {
  : >"$STUB_LOG"
  : >"$work/github.env"
  : >"$work/cert.p12"
  SIGNING_KEYCHAIN="$work/signing.keychain-db" GITHUB_ENV="$work/github.env" CERT_PASSWORD="hunter2" \
    "$ci/macos-signing-keychain.sh" import "$work/cert.p12" >"$work/import.out" 2>&1
}

printf '  1) ABC "Developer ID Application: Example Corp (TEAM12345)"\n     1 valid identities found\n' >"$STUB_STATE/identities"
ARBOS_APPLE_TEAM_ID=TEAM12345 run_import || fail "import with a matching team should pass: $(cat "$work/import.out")"
expect_call "security create-keychain -p deadbeef $work/signing.keychain-db"
expect_call "security unlock-keychain -p deadbeef $work/signing.keychain-db"
expect_call "security list-keychains -d user -s $work/signing.keychain-db /Users/runner/Library/Keychains/login.keychain-db /Library/Keychains/System.keychain"
expect_call "security import $work/cert.p12 -k $work/signing.keychain-db -P hunter2 -f pkcs12 -T /usr/bin/codesign -T /usr/bin/security"
expect_call "security set-key-partition-list -S apple-tool:,apple: -s -k deadbeef $work/signing.keychain-db"
expect_call "security find-identity -v -p codesigning $work/signing.keychain-db"
grep -qxF "CODESIGN_IDENTITY=Developer ID Application: Example Corp (TEAM12345)" "$work/github.env" || fail "CODESIGN_IDENTITY not exported"
grep -qxF "SIGNING_KEYCHAIN=$work/signing.keychain-db" "$work/github.env" || fail "SIGNING_KEYCHAIN not exported"
grep -qF "hunter2" "$work/import.out" && fail "the certificate password was printed"

if ARBOS_APPLE_TEAM_ID=OTHER run_import; then fail "a foreign team id must be rejected"; fi
grep -q "does not belong to team OTHER" "$work/import.out" || fail "wrong message for a foreign team: $(cat "$work/import.out")"

printf '     0 valid identities found\n' >"$STUB_STATE/identities"
if run_import; then fail "a certificate without a Developer ID identity must be rejected"; fi
grep -q "no valid Developer ID Application identity" "$work/import.out" || fail "wrong message for a missing identity"

if CERT_PASSWORD='' SIGNING_KEYCHAIN="$work/k" "$ci/macos-signing-keychain.sh" import "$work/cert.p12" 2>/dev/null; then
  fail "an empty CERT_PASSWORD must be rejected"
fi

: >"$STUB_LOG"
SIGNING_KEYCHAIN="$work/signing.keychain-db" "$ci/macos-signing-keychain.sh" remove >/dev/null
expect_call "security delete-keychain $work/signing.keychain-db"
: >"$STUB_LOG"
SIGNING_KEYCHAIN="$work/absent.keychain-db" "$ci/macos-signing-keychain.sh" remove >/dev/null
reject_call "security delete-keychain"

# ------------------------------------------------------------------ verify
app="$work/package/Zeron.app"
dmg="$work/package/zeron-0.2.112.2584-macos-arm64.dmg"
mkdir -p "$app/Contents/MacOS"
echo dmg >"$dmg"
stub codesign 'if [ "$1" = "--display" ]; then cat "$STUB_STATE/codesign-display" >&2; fi'
stub spctl ''
stub xcrun ''

printf 'Authority=Developer ID Application: Example Corp (TEAM12345)\nAuthority=Developer ID Certification Authority\n' >"$STUB_STATE/codesign-display"
: >"$STUB_LOG"
"$ci/macos-verify-signed.sh" "$app" "$dmg" >/dev/null || fail "a Developer ID signature should verify"
expect_call "codesign --verify --deep --strict --verbose=2 $app"
expect_call "spctl --assess --type execute --verbose=2 $app"
expect_call "xcrun stapler validate $app"
expect_call "spctl --assess --type open --context context:primary-signature --verbose=2 $dmg"
expect_call "xcrun stapler validate $dmg"

printf 'Signature=adhoc\n' >"$STUB_STATE/codesign-display"
if "$ci/macos-verify-signed.sh" "$app" >/dev/null 2>&1; then fail "an ad-hoc signature must be rejected"; fi

printf 'Authority=Developer ID Application: Example Corp (TEAM12345)\n' >"$STUB_STATE/codesign-display"
stub spctl 'exit 3'
if "$ci/macos-verify-signed.sh" "$app" >/dev/null 2>&1; then fail "a Gatekeeper rejection must fail the check"; fi
stub spctl ''
: >"$STUB_LOG"
"$ci/macos-verify-signed.sh" "$app" >/dev/null || fail "verify without a dmg should pass"
reject_call "--type open"

# ----------------------------------------------------------------- publish
pkg="$work/package"
tarball="$pkg/zeron-0.2.112.2584-macos-arm64-app.tar.gz"
echo app >"$tarball"
echo 0.2.112.2584 >"$pkg/version.txt"
tar_sha="$(shasum -a 256 "$tarball" | cut -d' ' -f1)"
dmg_sha="$(shasum -a 256 "$dmg" | cut -d' ' -f1)"
linux_sha="$(printf 'a%.0s' {1..64})"

# gh: the release exists and holds a Linux tarball, an old Mac build, and a manifest.
stub gh '
case "$1 $2" in
  "release view")
    if [ "${3:-}" = "edge" ] && [ "${4:-}" = "--json" ]; then
      # The second listing is returned unfiltered on purpose: the script
      # must not rely on the jq filter alone before deleting.
      case "$7" in
        ".assets[].name") printf "manifest.json\nlatest.txt\narbos-0.2.112.2584-linux-x86_64.tar.gz\nzeron-0.2.112.2580-macos-arm64-app.tar.gz\n" ;;
        *select*) printf "zeron-0.2.112.2580-macos-arm64-app.tar.gz\thttps://api/assets/1\nzeron-0.2.112.2580-macos-arm64.dmg\thttps://api/assets/2\nzeron-0.2.112.2584-macos-arm64-app.tar.gz\thttps://api/assets/3\nzeron-0.2.112.2584-macos-arm64.dmg\thttps://api/assets/4\narbos-0.2.112.2584-linux-x86_64.tar.gz\thttps://api/assets/5\n" ;;
      esac
    fi ;;
  "release download")
    cp "$STUB_STATE/remote-manifest.json" "$7/manifest.json" ;;
esac'
cat >"$STUB_STATE/remote-manifest.json" <<JSON
{"version": "0.2.112.2584", "files": {"arbos-0.2.112.2584-linux-x86_64.tar.gz": {"sha256": "$linux_sha"}, "zeron-0.2.112.2580-macos-arm64-app.tar.gz": {"sha256": "$dmg_sha"}}}
JSON

: >"$STUB_LOG"
GH_TOKEN=x "$ci/publish-edge-macos.sh" "$pkg" 0123abcd >"$work/publish.out" 2>&1 || fail "publish should succeed: $(cat "$work/publish.out")"
expect_call "gh release download edge -p manifest.json -D $pkg"
reject_call "gh release create"
expect_call "gh release upload edge zeron-0.2.112.2584-macos-arm64-app.tar.gz zeron-0.2.112.2584-macos-arm64.dmg manifest.json latest.txt --clobber"
expect_call "gh api -X DELETE https://api/assets/1"
expect_call "gh api -X DELETE https://api/assets/2"
reject_call "gh api -X DELETE https://api/assets/3"
reject_call "gh api -X DELETE https://api/assets/4"
reject_call "gh api -X DELETE https://api/assets/5"
python3 - "$pkg/manifest.json" "$tar_sha" "$dmg_sha" "$linux_sha" <<'PY'
import json, sys
manifest = json.load(open(sys.argv[1]))
tar_sha, dmg_sha, linux_sha = sys.argv[2:]
assert manifest == {
    "version": "0.2.112.2584",
    "files": {
        "arbos-0.2.112.2584-linux-x86_64.tar.gz": {"sha256": linux_sha},
        "zeron-0.2.112.2584-macos-arm64-app.tar.gz": {"sha256": tar_sha},
        "zeron-0.2.112.2584-macos-arm64.dmg": {"sha256": dmg_sha},
    },
}, manifest
PY
[[ "$(cat "$pkg/latest.txt")" == "0.2.112.2584" ]] || fail "latest.txt"

# No release yet: it is created at the head commit.
rm -f "$STUB_STATE/created"
stub gh '
case "$1 $2" in
  "release create") touch "$STUB_STATE/created" ;;
  "release view") [ -f "$STUB_STATE/created" ] || exit 1 ;;
esac'
: >"$STUB_LOG"
GH_TOKEN=x "$ci/publish-edge-macos.sh" "$pkg" 0123abcd >"$work/publish.out" 2>&1 || fail "first publish should succeed: $(cat "$work/publish.out")"
expect_call "gh release create edge --title Arbos edge --notes Arbos edge builds from main."
expect_call "--target 0123abcd"
reject_call "gh release download"

# The artifact must carry the version the updater will ask for.
echo 0.2.112.9999 >"$pkg/version.txt"
if GH_TOKEN=x "$ci/publish-edge-macos.sh" "$pkg" 0123abcd >"$work/publish.out" 2>&1; then
  fail "a version mismatch must refuse to publish"
fi
grep -q "does not carry version 0.2.112.9999" "$work/publish.out" || fail "wrong message for a version mismatch"
echo 0.2.112.2584 >"$pkg/version.txt"

# Two tarballs are ambiguous.
echo other >"$pkg/zeron-0.2.112.2583-macos-arm64-app.tar.gz"
if GH_TOKEN=x "$ci/publish-edge-macos.sh" "$pkg" 0123abcd >/dev/null 2>&1; then
  fail "two tarballs must refuse to publish"
fi

echo "macos release scripts: ok"

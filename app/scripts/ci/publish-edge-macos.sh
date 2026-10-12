#!/usr/bin/env bash
# Attach the packaged Mac app to the GitHub Release tagged `edge`: merge its
# files into the shared manifest.json (the Linux workflow publishes to the same
# one), upload, then drop the Mac assets of earlier builds. Run it only after
# macos-verify-signed.sh passed.
# usage: publish-edge-macos.sh <package-dir> <head-sha>   (needs GH_TOKEN)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dir="$1"
head_sha="$2"
tag=edge
notes="Arbos edge builds from main. The Linux tarball is published by the Linux tarball workflow. The Mac app is published by the macOS release workflow, signed and notarized."

fail() {
  echo "error: $*" >&2
  exit 1
}

shopt -s nullglob
tarballs=("$dir"/*-macos-*-app.tar.gz)
dmgs=("$dir"/*-macos-*.dmg)
shopt -u nullglob
(( ${#tarballs[@]} == 1 )) || fail "expected exactly one *-macos-*-app.tar.gz in $dir, found ${#tarballs[@]}"
(( ${#dmgs[@]} <= 1 )) || fail "expected at most one *-macos-*.dmg in $dir, found ${#dmgs[@]}"
[[ -f "$dir/version.txt" ]] || fail "$dir/version.txt is missing"
version="$(tr -d '\n' <"$dir/version.txt")"

# The updater downloads the file named after the manifest version, so the
# artifact must carry the same version in its name.
assets=()
entries=()
for path in "${tarballs[@]}" "${dmgs[@]}"; do
  name="$(basename "$path")"
  [[ "$name" == *"-${version}-macos-"* ]] || fail "$name does not carry version $version"
  digest="$(shasum -a 256 "$path" | cut -d' ' -f1)"
  assets+=("$name")
  entries+=("${name}=${digest}")
done

"$here/fetch-release-manifest.sh" "$tag" "$dir"
"$here/merge-release-manifest.py" "$dir/manifest.json" "$version" "${entries[@]}"

cd "$dir"
if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" --title "Arbos edge" --notes "$notes" --target "$head_sha"
fi
gh release upload "$tag" "${assets[@]}" manifest.json latest.txt --clobber
echo "uploaded: ${assets[*]} manifest.json latest.txt"

# Earlier Mac builds are no longer in the manifest; remove their assets.
# `gh release view` reports a GraphQL node id, the delete route needs the
# numeric id in the asset API URL.
gh release view "$tag" --json assets --jq '.assets[] | select(.name | test("-macos-")) | "\(.name)\t\(.apiUrl)"' \
  | while IFS=$'\t' read -r name api_url; do
    [[ -n "$name" && -n "$api_url" ]] || continue
    # Only ever this platform's files, whatever the listing returned.
    [[ "$name" == *-macos-* ]] || continue
    keep=false
    for asset in "${assets[@]}"; do
      [[ "$name" == "$asset" ]] && keep=true
    done
    $keep && continue
    echo "removing older asset $name"
    gh api -X DELETE "$api_url"
  done

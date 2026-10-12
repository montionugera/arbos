#!/usr/bin/env bash
# Download the current manifest.json of a GitHub Release into a directory so
# merge-release-manifest.py can start from it. A release or manifest that does
# not exist yet is not an error (first publish); a failed download is, because
# publishing a merge of nothing would drop the other platform's entries.
# usage: fetch-release-manifest.sh <tag> <dest-dir>   (needs GH_TOKEN)
set -euo pipefail
tag="$1"
dest="$2"
mkdir -p "$dest"
rm -f "$dest/manifest.json"
if gh release view "$tag" --json assets --jq '.assets[].name' 2>/dev/null | grep -qx manifest.json; then
  gh release download "$tag" -p manifest.json -D "$dest"
  echo "fetched manifest.json from the $tag release"
else
  echo "no manifest.json on the $tag release yet"
fi

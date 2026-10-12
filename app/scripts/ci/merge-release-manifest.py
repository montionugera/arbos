#!/usr/bin/env python3
"""Merge one platform's build into the edge release manifest.

usage: merge-release-manifest.py <manifest.json> <version> <file>=<sha256>...

The Linux and the Mac workflows both publish to the GitHub Release tagged
`edge`, and the running app reads one `manifest.json` there:

    {"version": "0.2.112.<build>", "files": {"<artifact>": {"sha256": "..."}}}

Overwriting the file from either job would drop the other platform's entries,
so each job downloads the current manifest, runs this script, and uploads the
result. <manifest.json> may be missing (first release); it is rewritten in
place and `latest.txt` is written beside it.

Entries are grouped by the platform token in the artifact name (`-linux-`,
`-macos-`, `-windows-`): adding files for a platform replaces that platform's
previous entries and keeps every other platform's. A manifest `version` that
is already newer than this build (compared numerically, component by component)
is kept, so a job finishing late never downgrades the release; its files are
still added.
"""
import json
import re
import sys
from pathlib import Path

PLATFORMS = ("-linux-", "-macos-", "-windows-")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def parse_version(text):
    """Dotted decimal components, or None. Mirrors the update crate."""
    parts = text.strip().lstrip("v").split(".")
    if not parts or not all(part.isdigit() for part in parts):
        return None
    return [int(part) for part in parts]


def platform_of(name):
    found = [token for token in PLATFORMS if token in name]
    if len(found) != 1:
        raise ValueError(f"artifact name {name!r} must contain exactly one of {', '.join(PLATFORMS)}")
    return found[0]


def load_existing(path):
    if not path.exists():
        return {}
    try:
        manifest = json.loads(path.read_text())
    except (OSError, ValueError) as error:
        print(f"warning: ignoring unreadable {path}: {error}", file=sys.stderr)
        return {}
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files", {}), dict):
        print(f"warning: ignoring malformed {path}", file=sys.stderr)
        return {}
    return manifest


def merge(existing, version, files):
    """Return (merged manifest, published version, note or None)."""
    new_platforms = {platform_of(name) for name in files}
    kept = {
        name: meta
        for name, meta in existing.get("files", {}).items()
        if not any(token in name for token in new_platforms)
    }
    merged_files = dict(kept)
    merged_files.update({name: {"sha256": digest} for name, digest in files.items()})

    published = version
    note = None
    current = existing.get("version")
    current_parts = parse_version(current) if isinstance(current, str) else None
    if current_parts is not None and current_parts > parse_version(version):
        published = current.strip()
        note = (
            f"keeping manifest version {published}: it is newer than this build ({version}); "
            "this build's files were added without changing the version"
        )
    return {"version": published, "files": merged_files}, published, note


def main(argv):
    if len(argv) < 4:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    path = Path(argv[1])
    version = argv[2]
    if parse_version(version) is None:
        print(f"error: version {version!r} is not dotted decimal", file=sys.stderr)
        return 2
    files = {}
    for entry in argv[3:]:
        name, separator, digest = entry.partition("=")
        if not separator or not name or not SHA256.match(digest):
            print(f"error: expected <file>=<sha256>, got {entry!r}", file=sys.stderr)
            return 2
        try:
            platform_of(name)
        except ValueError as error:
            print(f"error: {error}", file=sys.stderr)
            return 2
        files[name] = digest

    merged, published, note = merge(load_existing(path), version, files)
    path.write_text(json.dumps(merged, indent=2, sort_keys=True) + "\n")
    (path.parent / "latest.txt").write_text(published + "\n")
    if note:
        print(note)
    for name, meta in sorted(merged["files"].items()):
        marker = "+" if name in files else " "
        digest = meta.get("sha256") if isinstance(meta, dict) else None
        print(f"{marker} {name} {digest or '(no checksum)'}")
    print(f"version {published}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

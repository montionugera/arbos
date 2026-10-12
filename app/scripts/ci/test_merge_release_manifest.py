#!/usr/bin/env python3
"""Fixture checks for merge-release-manifest.py. Run: python3 app/scripts/ci/test_merge_release_manifest.py"""
import json
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("merge-release-manifest.py")
LINUX = "arbos-0.2.112.2584-linux-x86_64.tar.gz"
MAC = "zeron-0.2.112.2584-macos-arm64-app.tar.gz"
MAC_DMG = "zeron-0.2.112.2584-macos-arm64.dmg"
SHA_A = "a" * 64
SHA_B = "b" * 64
SHA_C = "c" * 64


def run(workdir, existing, version, *files):
    manifest = workdir / "manifest.json"
    if existing is None:
        if manifest.exists():
            manifest.unlink()
    elif isinstance(existing, str):
        manifest.write_text(existing)
    else:
        manifest.write_text(json.dumps(existing))
    result = subprocess.run(
        [sys.executable, str(SCRIPT), str(manifest), version, *files],
        capture_output=True,
        text=True,
    )
    merged = json.loads(manifest.read_text()) if result.returncode == 0 else None
    latest = (workdir / "latest.txt").read_text() if result.returncode == 0 else None
    return result, merged, latest


def check(workdir):
    # First release: no manifest yet.
    result, merged, latest = run(workdir, None, "0.2.112.2584", f"{LINUX}={SHA_A}")
    assert result.returncode == 0, result.stderr
    assert merged == {"version": "0.2.112.2584", "files": {LINUX: {"sha256": SHA_A}}}
    assert latest == "0.2.112.2584\n"

    # The Mac job adds its files and keeps the Linux entry.
    result, merged, latest = run(workdir, merged, "0.2.112.2584", f"{MAC}={SHA_B}", f"{MAC_DMG}={SHA_C}")
    assert result.returncode == 0, result.stderr
    assert merged["version"] == "0.2.112.2584"
    assert merged["files"] == {
        LINUX: {"sha256": SHA_A},
        MAC: {"sha256": SHA_B},
        MAC_DMG: {"sha256": SHA_C},
    }
    assert f"+ {MAC} {SHA_B}" in result.stdout
    assert f"  {LINUX} {SHA_A}" in result.stdout

    # The next Mac build replaces the stale Mac entries only.
    new_mac = MAC.replace("2584", "2585")
    result, merged, latest = run(workdir, merged, "0.2.112.2585", f"{new_mac}={SHA_C}")
    assert result.returncode == 0, result.stderr
    assert merged == {
        "version": "0.2.112.2585",
        "files": {LINUX: {"sha256": SHA_A}, new_mac: {"sha256": SHA_C}},
    }
    assert latest == "0.2.112.2585\n"

    # The Linux job for the same commit replaces the Linux entry and keeps the Mac one.
    new_linux = LINUX.replace("2584", "2585")
    result, merged, latest = run(workdir, merged, "0.2.112.2585", f"{new_linux}={SHA_B}")
    assert result.returncode == 0, result.stderr
    assert merged["files"] == {new_linux: {"sha256": SHA_B}, new_mac: {"sha256": SHA_C}}

    # A late job must not downgrade the version; its files still land.
    result, merged, latest = run(workdir, merged, "0.2.112.2584", f"{MAC}={SHA_A}")
    assert result.returncode == 0, result.stderr
    assert merged["version"] == "0.2.112.2585"
    assert latest == "0.2.112.2585\n"
    assert merged["files"] == {new_linux: {"sha256": SHA_B}, MAC: {"sha256": SHA_A}}
    assert "keeping manifest version 0.2.112.2585" in result.stdout
    assert "newer than this build (0.2.112.2584)" in result.stdout

    # Numeric, not lexical: 0.2.112.2600 is newer than 0.2.112.999.
    result, merged, latest = run(
        workdir, {"version": "0.2.112.999", "files": {}}, "0.2.112.2600", f"{LINUX}={SHA_A}"
    )
    assert result.returncode == 0, result.stderr
    assert merged["version"] == "0.2.112.2600"
    assert "keeping" not in result.stdout

    # A longer version is newer than its prefix, as in the update crate.
    result, merged, latest = run(workdir, {"version": "0.2.112", "files": {}}, "0.2.112.1", f"{LINUX}={SHA_A}")
    assert result.returncode == 0, result.stderr
    assert merged["version"] == "0.2.112.1"

    # A pre-manifest or corrupt file is ignored with a warning, not fatal.
    result, merged, latest = run(workdir, "not json", "0.2.112.2584", f"{LINUX}={SHA_A}")
    assert result.returncode == 0, result.stderr
    assert "warning: ignoring unreadable" in result.stderr
    assert merged == {"version": "0.2.112.2584", "files": {LINUX: {"sha256": SHA_A}}}

    # Entries without a sha256 survive the merge untouched.
    result, merged, latest = run(
        workdir, {"version": "0.2.112.1", "files": {LINUX: {}}}, "0.2.112.2584", f"{MAC}={SHA_B}"
    )
    assert result.returncode == 0, result.stderr
    assert merged["files"] == {LINUX: {}, MAC: {"sha256": SHA_B}}

    # Bad input is rejected before anything is written.
    (workdir / "manifest.json").unlink()
    for bad in (
        ["0.2.112.2584", f"{LINUX}=abc"],
        ["0.2.112.2584", f"arbos-0.2.112.2584-plan9-amd64.tar.gz={SHA_A}"],
        ["0.2.112.2584", f"arbos-linux-macos-x.tar.gz={SHA_A}"],
        ["0.2.112.2584", LINUX],
        ["0.2.112.x", f"{LINUX}={SHA_A}"],
        ["0.2.112.2584"],
    ):
        result, merged, latest = run(workdir, None, *bad)
        assert result.returncode == 2, (bad, result.stdout, result.stderr)
        assert not (workdir / "manifest.json").exists(), bad


def main():
    with tempfile.TemporaryDirectory() as tmp:
        check(Path(tmp))
    print("merge-release-manifest: ok")


if __name__ == "__main__":
    main()

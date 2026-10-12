# Packaging

## Linux (implemented)

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/arbos-<version>-linux-<arch>.tar.gz` containing:

- `zeron` (and its `arbos` alias) — the binary (headed by default; `zeron headless` runs the engine alone)
- `zeron.desktop` — XDG desktop entry template (`Exec=zeron` for packagers;
  the installers rewrite `Exec`, `TryExec`, and `Icon` to absolute paths under
  `~/.zeron/app/current`, since `~/.local/bin` is often not on a desktop
  session's `PATH`)
- `zeron.png` — 1024×1024 Arbos app icon
- `install.sh` — installs into `~/.zeron/app/<version>` behind a `current`
  symlink (the curl installer's layout, which the in-app updater manages),
  links `~/.local/bin/zeron` (and `arbos`) to it, and writes the desktop entry
  and icon under `$XDG_DATA_HOME` (default `~/.local/share`). The curl
  installer does the same from the extracted tarball;
  `scripts/test-linux-desktop-entry.sh` checks both

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
scripts/package-macos.sh    # → target/package/arbos-<version>-macos-<arch>.dmg
```

Builds the release binary, assembles `Arbos.app` (Info.plist + icns, with the
binary installed as `Arbos.app/Contents/MacOS/Arbos`), ad-hoc signs it (set
`CODESIGN_IDENTITY` for a real Developer ID), and wraps it in a dmg. The
auto-update tarball (`arbos-<version>-macos-<arch>-app.tar.gz`) carries the
same `Arbos.app`; installs from before the rebrand looked for a
`zeron-…-app.tar.gz` the manifest no longer lists, so they update by
downloading the dmg once. `ARBOS_BUILD=<n>` (the commit count, as CI passes
it) makes the version `0.2.112.<n>` — the full build version the updater
orders on. The manual steps the script automates, for reference (run on a
macOS host — gpui needs Metal; no cross-build from Linux):

1. Build the universal (or per-arch) binary:
   ```sh
   cargo build --release -p zeron --target aarch64-apple-darwin
   cargo build --release -p zeron --target x86_64-apple-darwin
   lipo -create -output zeron \
     target/aarch64-apple-darwin/release/zeron \
     target/x86_64-apple-darwin/release/zeron
   ```
2. Assemble the bundle (the cargo binary is `zeron`; the bundle executable is
   `Arbos`):
   ```sh
   mkdir -p Arbos.app/Contents/{MacOS,Resources}
   cp zeron Arbos.app/Contents/MacOS/Arbos
   sed "s/__VERSION__/$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')/" \
     dist/macos/Info.plist > Arbos.app/Contents/Info.plist
   ```
3. Icon: generate `arbos.icns` from `dist/macos/icon-1024.png` (the macOS-shaped
   variant of the artwork — squircle mask, margins, and shadow pre-baked, since
   `sips` can't apply an alpha mask) and place it at
   `Arbos.app/Contents/Resources/arbos.icns`:
   ```sh
   mkdir arbos.iconset && sips -z 256 256 dist/macos/icon-1024.png --out arbos.iconset/icon_256x256.png
   iconutil -c icns arbos.iconset -o Arbos.app/Contents/Resources/arbos.icns
   ```
4. Sign + notarize (required for distribution):
   ```sh
   codesign --deep --force --options runtime --sign "Developer ID Application: …" Arbos.app
   xcrun notarytool submit Arbos.zip --keychain-profile … --wait
   xcrun stapler staple Arbos.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname Arbos -srcfolder Arbos.app -ov -format UDZO arbos.dmg`).

The bundle id stays `sh.zeron.app` (and the `zeron://` scheme with it) so an
in-place update from a pre-rebrand install keeps its LaunchServices identity,
TCC permissions, and deep links.

## Windows

```powershell
./scripts/package-windows.ps1 -ReleasesUrl https://github.com/zeronsh/zeron/releases/latest/download
```

Produces, under `target/package/`:

- `zeron-<version>-windows-<arch>-setup.exe` — the per-user installer built
  from `dist/windows/zeron.iss` with Inno Setup 6
- `zeron-<version>-windows-<arch>.zip` — the portable package
- `zeron-<version>-windows-<arch>.exe` — the bare executable the in-app
  updater downloads

The installer and the zip both carry `zeron-update.json`, the marker that lets
the app update itself in place. CI runs `scripts/test-windows-installer.ps1`
against the setup on every Windows build.

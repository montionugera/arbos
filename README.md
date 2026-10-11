# Arbos

Arbos is a coding-agent app. You open a folder, talk to Main, and watch workers. The harness is fx. It speaks engy (`https://api.engy.ai/v1`, model `glm-5.3`).

Current status is in [docs/STATUS.md](docs/STATUS.md). The design is in [docs/design/arbos-rebuild-ideation.md](docs/design/arbos-rebuild-ideation.md).

## Install

The download is a Linux tarball. There is no Mac disk image.

On the GitHub Release tagged `edge`, take `arbos-<version>-linux-<arch>.tar.gz`. This tree is version `0.2.111`, and the published file is `arbos-0.2.111-linux-x86_64.tar.gz`:

https://github.com/unarbos/arbos/releases/download/edge/arbos-0.2.111-linux-x86_64.tar.gz

Unpack it and run `install.sh`. That script does not need root. It copies the app to `~/.zeron/app` and links `~/.local/bin/arbos`. `arbos` and `zeron` start the same program. `arbos --version` prints `Arbos 0.2.111`.

If `~/.local/bin` is not on your `PATH`, add it, then open a new terminal. The application menu entry is still named Zeron. The window title is Arbos.

## Paste an engy key

Set `ENGY_API_KEY` in the environment, then start `arbos`. The app stores the key and does not write it into the project. On Linux the file is `~/.local/share/arbos/secrets/engy`, mode 600. On macOS the key goes to the login keychain under the service `arbos`.

Settings → Keys can save a key for AI Gateway, OpenRouter, Ollama, Codex, or Grok. That page has no engy row. The engy key is `ENGY_API_KEY`.

## Open a folder

The first screen says "Add a project to get started" and "A project is a folder on one of your devices." Click **Add a project**. You can also click **+** on the left rail. Pick this computer, then the folder.

The app creates `<folder>/.arbos/` and a Main chat. The left rail shows one icon per project.

## Talk to Main

Open the **Main** tab. The composer says "Do anything…". The chip shows `engy` and `glm-5.3`. Type the prompt there.

## Watch workers

Ask Main to spawn a worker. Each worker is its own tab. When a worker finishes, Main shows a line that starts with `Worked`. A worker runs on this machine, or on another install you have registered.

The phone app in `ios/` can list those projects, follow Main, and send a prompt. It has no voice and no calls. Apple did not accept the TestFlight upload. See [docs/STATUS.md](docs/STATUS.md).

The root license is MIT. `fx/` stays Apache-2.0. See `fx/LICENSE` and the `THIRD_PARTY_NOTICES.md` files in `app/` and `fx/`.

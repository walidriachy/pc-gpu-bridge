# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.2] — 2026-11-21

### Added
- **Automatic updates.** Both apps check for a new signed version a couple of minutes after they start and then every 6 hours. They install it and restart by themselves, and the PC waits until a running job has finished. **Settings → Check for updates now** checks right away. Updates are signed with the project's key, so a download that isn't signed is refused. You install **0.1.2 by hand once**; every later version arrives by itself.
- **The PC helper updates itself.** When the Windows app starts and finds an older helper still running (an update replaces the app, but the old helper keeps going), it stops the old one, installs the new one and starts it again.
- **Settings → Restart the helper** (Windows).
- **Tailscale stays connected.** Once paired, PC Bridge checks every minute. If Tailscale isn't open, it opens it. If Tailscale was disconnected, it connects it again. If Tailscale is signed out, PC Bridge leaves it alone, because only you can sign in. On Windows, Tailscale is also set to stay up when nobody is signed in to Windows.

### Fixed
- **The PC dot flickered red and the speed check failed ("request or response body error").** Before and after every job, the helper listed every file in the shared folder to spot the job's new files, and it did that on its main loop. With a big shared folder (still being synced for the first time), it stopped answering for over a minute. File listing, output copying, cleanup and status checks now run in the background. Dependency and Git folders (`node_modules`, `.git`, `.venv`…) and Syncthing's temporary files are skipped. Files Syncthing brings in from the Mac during a job are no longer mistaken for job results.
- **A job that had already run on the PC could run a second time on the Mac** if bringing a result back failed, or if the connection dropped mid-job. Once the PC accepts a job, PC Bridge never re-runs it. It warns you instead (see the Jobs tab).
- **"program not found on the PC: ffmpeg" (exit code 127) right after setup**, even though FFmpeg was installed. The helper kept the PATH from the moment it started. It now reads Windows' current PATH from the registry for every job and status check, so programs installed later (FFmpeg, Claude Code, Blender…) work straight away, without a restart.
- **The PC helper didn't start after a reboot** if the PC wizard was never closed with **Finish** (common when you pair from the Mac). The helper now starts whenever it's installed.
- Inline scripts (`powershell -Command '…'`, `node -e '…'`, `bash -c '…'`) were sometimes mistaken for file names and rewritten as PC paths.
- The Claude instructions no longer claim a fixed "RTX 3090, 32 GB RAM". They point Claude to `pc_gpu` and `pc_memory` for the real figures.

## [0.1.1] — 2026-11-21

### Fixed
- **Windows: "Set up the worker Claude" reported "The install didn't finish" although Claude Code was installed.** Claude Code's native installer puts `claude.exe` in `%USERPROFILE%\.local\bin`, which usually isn't on PATH. PC Bridge now finds it there, adds that folder to your user PATH automatically (no admin needed, works with spaces in user names), and gives the agent the full path. "Check again" fixes existing installs.
- The PC agent finds Claude Code in `~/.local/bin` even before PATH changes take effect, so worker-Claude jobs and the status screen see it.

## [0.1.0] — 2026-11-21

First public release.

### Added
- **Mac app** (menu bar + window): 8-step setup wizard, then the Status, Jobs, Settings and Help screens, and diagnostics.
- **Windows app** (tray + window): 9-step setup wizard that installs Tailscale, FFmpeg, Python, Syncthing, the agent, and optionally Claude Code, Ollama and ComfyUI. It also sets up Wake-on-LAN, power settings and pairing.
- **PC agent** (FastAPI): streamed `/run` and `/delegate`, FIFO GPU queue, result cache, 14-day cleanup, strict root, pairing, optional allowlist, Syncthing, Ollama and ComfyUI endpoints.
- **Smart, memory-aware router**: compares Mac and PC time (transfer, queue, wake), watches Mac memory pressure, estimates job RAM, and learns from history.
- **14 MCP tools** for Claude Code and Claude Desktop, plus a CLAUDE.md snippet that you preview before it's added.
- **Shims** for ffmpeg/ffprobe/python/blender/comfy, and per-project opt-in shims for npm/pnpm/npx/node/docker.
- Automatic switch from `libx264`/`libx265` to NVENC encoders.
- Mac and PC free RAM side by side in both apps.
- `pc-bridge doctor` health check command.
- CI on Linux, macOS and Windows, and a release pipeline that builds `PC Bridge.dmg` and `PC Bridge Setup.exe`.

### Fixed
- The Tailscale check on the Mac reported "not connected" when the app was opened from Finder. The Tailscale CLI needs `TAILSCALE_BE_CLI=1` when there's no terminal.
- Mac paths are now always built with `/`, including in the Windows build of the shared core.

[Unreleased]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/walidriachy/pc-gpu-bridge/releases/tag/v0.1.0

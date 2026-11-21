# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/walidriachy/pc-gpu-bridge/releases/tag/v0.1.0

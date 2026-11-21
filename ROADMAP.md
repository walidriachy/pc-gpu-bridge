# PC Bridge — Roadmap

> **Living document.** Updated at the end of every phase in the same commit as the work.
> Status keys: ✅ done · 🔄 in progress · ⏳ next · 🧊 later / known limitation

_Last updated: 2026-11-21 — **v0.1.1** (Windows Claude Code PATH fix); v0.1.0 public (MIT); CI green on Linux, macOS and Windows_

## Architecture in one picture

```
 MacBook (brain)                                   Windows PC (muscle)
 ┌──────────────────────────────┐   Tailscale     ┌───────────────────────────────┐
 │ Claude Code / Claude Desktop │   (private,     │ PC Bridge app (tray)          │
 │   └─ MCP: `pc-bridge mcp`    │    encrypted)   │   └─ starts Python agent      │
 │ Shims ~/.pcbridge/bin/ffmpeg │ ──────────────► │ FastAPI agent :8765           │
 │   └─ `pc-bridge shim ffmpeg` │   token auth    │   queue · GPU · claude -p     │
 │ PC Bridge app (menu bar)     │ ◄────────────── │ D:\PCBridge\ (strict root)    │
 │ Syncthing  ◄──── files ────► │                 │ Syncthing                     │
 └──────────────────────────────┘                 └───────────────────────────────┘
```

One Rust binary (`pc-bridge`) = desktop app + MCP server + shim runner. Shared logic lives in
`crates/bridge-core` (pure Rust, unit-tested without a GUI).

## Phases

| # | Phase | Status | Notes |
|---|-------|--------|-------|
| 0 | Repo scaffold, roadmap, architecture | ✅ | |
| 1 | PC agent (FastAPI): /run /delegate /status /gpu /memory /files /cancel /pair, queue, strict root, cache, cleanup | ✅ | 30 tests, ruff clean |
| 2 | `bridge-core` Rust crate: path mapping, **memory-aware** router, GPU/RAM job detection, ffmpeg NVENC rewrite, agent client, MCP server | ✅ | 44 tests, clippy clean |
| 2b | **Memory-aware routing (added 2026-11-21)**: Mac memory pressure, RAM estimates, docker/node/npm/pnpm shims (opt-in per project), generic `pc_run_task`, Mac vs PC RAM side by side, Help text | ✅ | Mac reports its RAM to the PC (`POST /peer/memory`) so both apps show Mac vs PC |
| 3 | Tauri app: wizards (Mac 8 steps / Windows 9 steps), Status/Jobs/Settings/Help, tray/menu bar, diagnostics | ✅ | React + TS, light/dark |
| 4 | Installers & system actions: winget installs, WoL adapter config, power, autostart, keychain, MCP config writers, shims | ✅ | Windows paths verified in CI build only (no Windows box here) |
| 5 | CI (tests + lint, macOS + Windows) and release workflow (DMG + Setup.exe) | ✅ | Windows CI caught 2 path-separator bugs, both fixed |
| 6 | Docs: README, GUIDE.md (beginner), unsigned-app instructions | ✅ | |
| 7 | GitHub repo, first release, CI green | ✅ | Public repo, v0.1.0 release with `PC Bridge.dmg` + `PC Bridge Setup.exe` |

## Detailed checklist

### Phase 1 — PC agent
- [x] Config + strict root resolution (`D:\PCBridge` → fallback `C:\PCBridge`; refuses other paths)
- [x] Pairing: 6-digit code (15 min expiry, 5 attempts, one use) → token
- [x] FIFO job queue, 1 GPU slot, job folders `jobs/YYYY-MM-DD/<id>_<name>/`
- [x] POST /run streamed (NDJSON), exit code passthrough
- [x] POST /delegate (`claude -p … --output-format stream-json` in `worker/`)
- [x] GET /status, GET /gpu (nvidia-smi), POST /jobs/{id}/cancel
- [x] POST /files/push, GET /files/pull (root-confined)
- [x] Result cache (content hash, size cap, LRU)
- [x] Optional command allowlist; 14-day cleanup
- [x] Tests: paths, queue, pairing, cache, GPU parse, end-to-end with mock commands

### Phase 2 — bridge-core
- [x] Mac memory pressure (`memory_pressure` / `vm_stat`) + per-job RAM estimate (pandas/numpy, docker, next build, LLMs)
- [x] Router sends RAM-heavy jobs or jobs during Mac memory pressure to the PC
- [x] Shims + MCP tools for docker, node/npm/pnpm (opt-in per project via `.pcbridge` file) and generic `pc_run_task`
- [x] Path mapping Mac ⇄ PC (shared folders → `workspace\<name>\…`)
- [x] Smart router (Mac est. vs PC est. + transfer + queue; learns from SQLite history)
- [x] Python GPU detection (torch/diffusers/whisper/comfy/marker)
- [x] ffmpeg NVENC auto-swap (libx264→h264_nvenc, libx265→hevc_nvenc when safe)
- [x] HTTP client (streaming), Wake-on-LAN magic packet
- [x] MCP stdio server: pc_run, pc_delegate_task, pc_status, pc_gpu, pc_push_file, pc_pull_file, pc_wake, comfy_generate, ollama_chat
- [x] Tests: mapping, router, detection, ffmpeg rewrite, MCP protocol, mock-agent e2e

- [x] Claude wiring: MCP entries (Claude Code + Desktop), CLAUDE.md block, shims, PATH — idempotent + backups + undo
- [x] Real cross-language e2e (`scripts/e2e-real-agent.sh`)

### Phase 3–4 — App
- [x] Wizard framework (what/why/do-it/check/progress/resume — step saved after every move)
- [x] Windows wizard steps 1–9, Mac wizard steps 1–8
- [x] Status, Jobs, Settings, Help (diagram + FAQ + security), Run diagnostics
- [x] Tray/menu bar icon green/yellow/red with quick menu (pause, wake, open last output)
- [x] Mac vs PC free RAM side by side (both apps; Mac reports its RAM to the PC)
- [x] Help: "apps open on the Mac still use Mac RAM; only jobs sent to the PC use PC RAM"
- [x] Writes Claude Code + Claude Desktop MCP config, CLAUDE.md snippet (preview first)
- [x] Windows: winget installs (Tailscale, FFmpeg, Python, Syncthing, Ollama), Claude Code installer, agent venv, WoL adapter + Fast-startup off (one admin prompt), power plan, autostart
- [x] Per-project opt-in for npm/pnpm/docker explained in Settings

### Phase 5–7 — Ship
- [x] `ci.yml`: agent pytest + ruff (Linux + Windows); Rust fmt + clippy + tests (macOS + Windows); UI lint + tests; real-agent e2e; installer build on both OSes
- [x] `release.yml`: on tag `v*` → `PC Bridge.dmg` (Apple Silicon) + `PC Bridge Setup.exe`
- [x] README (assumptions first), `docs/GUIDE.md` (mirrors both wizards, unsigned-app steps, troubleshooting), LICENSE (MIT)
- [x] Repo created and pushed, CI green
- [x] Open-source docs: README, USAGE, ARCHITECTURE (diagrams), CONTRIBUTING, SECURITY, CODE_OF_CONDUCT, CHANGELOG, issue/PR templates, Dependabot
- [x] Public visibility + tag v0.1.0 → installers on the release page

## What's next (after v0.1)

```mermaid
timeline
  title PC Bridge roadmap
  v0.1 — now : Both apps + wizards : Smart, memory-aware router : 14 MCP tools : CI + installers
  v0.2 : Live job progress in the menu bar : In-app auto-update : Signed + notarised builds
  v0.3 : Several PCs / GPUs : Linux PC agent : Wake from away via an always-on tailnet device
```

| Version | Item | Why |
|---|---|---|
| 0.2 | Live progress (percent / ETA) for ffmpeg and ComfyUI jobs in the menu bar | Long encodes currently show only "running" |
| 0.2 | Auto-update (Tauri updater) | Beginners shouldn't need to re-download installers |
| 0.2 | Code signing + notarisation | Removes the "unidentified developer" / SmartScreen steps |
| 0.2 | Send whole project folders for Node/Docker without a separate `npm ci` on the PC | Removes a known limitation |
| 0.3 | Several PCs, choose by free VRAM | Households/studios with more than one GPU box |
| 0.3 | Linux PC agent (same Python agent, systemd unit) | Many GPU boxes run Linux |
| 0.3 | Wake-on-LAN from away, relayed by an always-on device on the tailnet | Today WoL only works on the home network |

## Decisions log
- **2026-11-21** One Rust binary serves app + MCP + shims → no Python/Node needed on the Mac.
- **2026-11-21** Agent streams NDJSON (one JSON object per line) — simple, proxy-safe, easy to test.
- **2026-11-21** Scope addition from user: memory-aware routing (RAM-heavy jobs + Mac memory pressure → PC).
- **2026-11-21** Node/Docker shims are opt-in per project: a `.pcbridge` file in the project with `offload = ["npm","docker"]` — avoids surprising redirection of everyday commands.
- **2026-11-21** Real-agent e2e found that POSIX agent roots (tests, WSL2 mode) need `/` paths and keep `python3`; fixed in `PathMapper`/`plan`.
- **2026-11-21** Both apps show Mac vs PC RAM: the Mac posts its memory to the agent on every status poll (window + tray), the PC app reads it from `/status.peer_memory`.
- **2026-11-21** macOS Tailscale CLI is the app binary; started from a GUI app (no TTY) it tries to open its GUI → always set `TAILSCALE_BE_CLI=1` (found by the first real user run). Added `pc-bridge doctor` to check this without the window.
- **2026-11-21** (v0.1.1) Claude Code's native Windows installer leaves `claude.exe` in `%USERPROFILE%\.local\bin` off PATH → app finds it, adds it to the *user* PATH via the registry (keeps `%VARS%`, no admin), and passes the full path to the agent; the agent also falls back to `~/.local/bin`. Found by the first real Windows run.
- **2026-11-21** Open-sourced under MIT with full contributor docs.
- **2026-11-21** Windows app launches the agent as a child process (autostart with Windows) instead of a Windows Service → no admin rights needed day-to-day.

## Known limitations
- Files a job opens **without naming them on the command line** (e.g. a script reading `data.csv` internally) reach the PC only through Syncthing; files named in the command are pushed automatically if missing.
- Wake-on-LAN works only when the Mac is on the same home network as the PC (Tailscale can't carry the wake signal).
- Node/Docker offload needs the project's dependencies installed on the PC side (Windows `node_modules` are not synced from the Mac).
- The apps are **not code-signed**: macOS Gatekeeper and Windows SmartScreen warn on first open (one-time bypass in `docs/GUIDE.md`).
- GitHub stores release files with spaces as dots, so the downloads are named `PC.Bridge.dmg` / `PC.Bridge.Setup.exe` (the release page label shows the readable name).
- The Windows side (winget installs, WoL adapter script, power plan, tray) is compiled and built in CI but was not run on a real Windows PC during development.
- Only an Apple Silicon DMG is produced (no Intel Mac build).

# PC Bridge — Roadmap

> **Living document.** Updated at the end of every phase in the same commit as the work.
> Status keys: ✅ done · 🔄 in progress · ⏳ next · 🧊 later / known limitation

_Last updated: 2026-11-21 — Phase 1 complete (agent, 26 tests green); Phase 2 in progress_

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
| 1 | PC agent (FastAPI): /run /delegate /status /gpu /memory /files /cancel /pair, queue, strict root, cache, cleanup | ✅ | 26 tests, ruff clean |
| 2 | `bridge-core` Rust crate: path mapping, **memory-aware** router, GPU/RAM job detection, ffmpeg NVENC rewrite, agent client, MCP server | 🔄 | |
| 2b | **Memory-aware routing (added 2026-11-21)**: Mac memory pressure, RAM estimates, docker/node/npm/pnpm shims (opt-in per project), generic `pc_run_task`, Mac vs PC RAM side by side, Help text | 🔄 | agent side ✅ (`/memory`, RAM in `/status`) |
| 3 | Tauri app: wizards (Mac 8 steps / Windows 9 steps), Status/Jobs/Settings/Help, tray/menu bar, diagnostics | ⏳ | |
| 4 | Installers & system actions: winget installs, WoL adapter config, power, autostart, keychain, MCP config writers, shims | ⏳ | |
| 5 | CI (tests + lint, macOS + Windows) and release workflow (DMG + Setup.exe) | ⏳ | |
| 6 | Docs: README, GUIDE.md (beginner), unsigned-app instructions | ⏳ | |
| 7 | GitHub repo, first release, CI green | ⏳ | |

## Detailed checklist

### Phase 1 — PC agent
- [x] Config + strict root resolution (`D:\PCBridge` → fallback `C:\PCBridge`; refuses other paths)
- [x] Pairing: 6-digit code (10 min expiry, 5 attempts) → token
- [x] FIFO job queue, 1 GPU slot, job folders `jobs/YYYY-MM-DD/<id>_<name>/`
- [x] POST /run streamed (NDJSON), exit code passthrough
- [x] POST /delegate (`claude -p … --output-format stream-json` in `worker/`)
- [x] GET /status, GET /gpu (nvidia-smi), POST /jobs/{id}/cancel
- [x] POST /files/push, GET /files/pull (root-confined)
- [x] Result cache (content hash, size cap, LRU)
- [x] Optional command allowlist; 14-day cleanup
- [x] Tests: paths, queue, pairing, cache, GPU parse, end-to-end with mock commands

### Phase 2 — bridge-core
- [ ] Mac memory pressure (`memory_pressure` / `vm_stat`) + per-job RAM estimate (pandas/numpy, docker, next build, LLMs)
- [ ] Router sends RAM-heavy jobs or jobs during Mac memory pressure to the PC
- [ ] Shims + MCP tools for docker, node/npm/pnpm (opt-in per project via `.pcbridge` file) and generic `pc_run_task`
- [ ] Path mapping Mac ⇄ PC (shared folders → `workspace\<name>\…`)
- [ ] Smart router (Mac est. vs PC est. + transfer + queue; learns from SQLite history)
- [ ] Python GPU detection (torch/diffusers/whisper/comfy/marker)
- [ ] ffmpeg NVENC auto-swap (libx264→h264_nvenc, libx265→hevc_nvenc when safe)
- [ ] HTTP client (streaming), Wake-on-LAN magic packet
- [ ] MCP stdio server: pc_run, pc_delegate_task, pc_status, pc_gpu, pc_push_file, pc_pull_file, pc_wake, comfy_generate, ollama_chat
- [ ] Tests: mapping, router, detection, ffmpeg rewrite, MCP protocol, mock-agent e2e

### Phase 3–4 — App
- [ ] Wizard framework (what/why/do-it/check/progress/resume)
- [ ] Windows wizard steps 1–9, Mac wizard steps 1–8
- [ ] Status, Jobs, Settings, Help (diagram + FAQ + security), Run diagnostics
- [ ] Tray/menu bar icon green/yellow/red with quick menu
- [ ] Mac vs PC free RAM side by side (both apps)
- [ ] Help: "apps open on the Mac still use Mac RAM; only jobs sent to the PC use PC RAM"
- [ ] Writes Claude Code + Claude Desktop MCP config, CLAUDE.md snippet (preview first)

## Decisions log
- **2026-11-21** One Rust binary serves app + MCP + shims → no Python/Node needed on the Mac.
- **2026-11-21** Agent streams NDJSON (one JSON object per line) — simple, proxy-safe, easy to test.
- **2026-11-21** Scope addition from user: memory-aware routing (RAM-heavy jobs + Mac memory pressure → PC).
- **2026-11-21** Node/Docker shims are opt-in per project: a `.pcbridge` file in the project with `offload = ["npm","docker"]` — avoids surprising redirection of everyday commands.
- **2026-11-21** Windows app launches the agent as a child process (autostart with Windows) instead of a Windows Service → no admin rights needed day-to-day.

## Known limitations
_(filled in as discovered)_

# PC Bridge

**Let Claude on your Mac send heavy work to your Windows PC's graphics card and memory, automatically.**

Two desktop apps from one codebase: a **menu bar app on the Mac** and a **system tray app on Windows**. You and Claude keep working on the Mac. Video encodes, AI image generation, GPU Python jobs, big data processing, Docker and large JavaScript builds go to the PC when that's faster or when the Mac is short on memory. The results land back in your Mac folder.

> **New here?** Follow the [beginner guide](docs/GUIDE.md). You won't need Terminal.

## Assumptions

These are the choices this build makes. Each one is something you can change later.

1. **Hardware:** a MacBook with Apple Silicon (M1 Pro or newer) and a Windows 10/11 PC with an NVIDIA RTX card (built for an RTX 3090 with 24 GB VRAM and 32 GB RAM), connected by wired Ethernet.
2. **Only the Apple Silicon DMG is built.** Intel Macs aren't targeted.
3. **Tailscale is the only network path.** On the PC, the agent listens only on `127.0.0.1` and the Tailscale IP, never on your home network or the internet. Both computers need a free Tailscale account (the same one).
4. **Everything on the PC lives under one folder:** `D:\PCBridge` if a D: drive exists, otherwise `C:\PCBridge`. You can change it in wizard step 1. The agent refuses to read or write anywhere else.
5. **Files move with Syncthing.** PC Bridge installs and configures it for the folders you choose, so you don't use Syncthing's own screens. A file named on a command line is also copied directly if Syncthing hasn't delivered it yet.
6. **One GPU job at a time** on the PC (`max_parallel_jobs = 1`), first come, first served. That avoids running out of VRAM.
7. **The PC's own Claude ("worker Claude")** uses your Claude subscription and runs `claude -p` with edit permissions limited to `PCBridge\worker`. This step is optional.
8. **The apps are not code-signed** (signing costs a yearly fee). macOS and Windows will warn you the first time you open them; the guide shows the one click to get past it.
9. **No admin rights in everyday use.** Windows asks for admin once: during setup, for Wake-on-LAN and power settings. The agent runs as a normal process started with Windows, not as a Windows service.
10. **Node and Docker offload is opt-in per project**, using a `.pcbridge` file. Everyday `npm install` and `git` commands stay on the Mac.
11. **Job folders on the PC are deleted after 14 days.** The result cache is capped at 20 GB.

## Download

Download the installers from the [latest release](https://github.com/walidriachy/pc-gpu-bridge/releases/latest):

| Computer | File |
|---|---|
| Mac (Apple Silicon) | `PC Bridge.dmg` |
| Windows PC | `PC Bridge Setup.exe` |

GitHub saves files with spaces in their names using dots instead, so your browser may download them as `PC.Bridge.dmg` and `PC.Bridge.Setup.exe`. They're the same files.

**Set up the PC first, then the Mac.**

## How it works

```
 MacBook (brain)                                   Windows PC (muscle)
 ┌──────────────────────────────┐   Tailscale     ┌───────────────────────────────┐
 │ Claude Code / Claude Desktop │   (private,     │ PC Bridge (tray)              │
 │   └─ MCP: `pc-bridge mcp`    │    encrypted)   │   └─ runs the Python agent    │
 │ Shims ~/.pcbridge/bin/ffmpeg │ ──────────────► │ FastAPI agent :8765           │
 │   └─ `pc-bridge shim ffmpeg` │   token auth    │   queue · GPU · claude -p     │
 │ PC Bridge (menu bar)         │ ◄────────────── │ D:\PCBridge\ (one folder)     │
 │ Syncthing  ◄──── files ────► │                 │ Syncthing                     │
 └──────────────────────────────┘                 └───────────────────────────────┘
```

* **One Rust binary, four jobs.** `pc-bridge` is the desktop app. `pc-bridge mcp` is the MCP server Claude talks to. `pc-bridge shim <tool>` redirects commands such as `ffmpeg`, `python`, `blender`, `docker` and `npm`. `pc-bridge run -- <cmd>` forces any command to run on the PC.
* **Smart router.** For each job it estimates the time on the Mac against the time on the PC, counting file transfer, queue wait and wake-up. It learns from past runs, which it stores in SQLite.
* **Memory-aware routing.** It watches Mac memory pressure (`memory_pressure` / `vm_stat`) and estimates each job's RAM. A job that needs more RAM than the Mac has free goes to the PC, and so does any heavy job while pressure is critical. Apps you have open on the Mac still use the Mac's RAM; only jobs sent to the PC use the PC's RAM.
* **NVENC.** `libx264`/`libx265` encodes are switched to `h264_nvenc`/`hevc_nvenc` on the PC when it's safe to do so.
* **Never stuck.** If the PC can't be reached, PC Bridge tries Wake-on-LAN, then falls back to running the job on the Mac and shows a notification.

### Claude tools (MCP)

| Tool | What it does |
|---|---|
| `pc_run` | Run a command on the PC (paths translated, outputs copied back) |
| `pc_delegate_task` | Hand a whole task to the worker Claude on the PC |
| `pc_run_task` | Generic "run this task on the PC", for any command in a shared folder |
| `pc_docker`, `pc_node_build` | Docker and npm/pnpm builds or tests on the PC |
| `pc_status`, `pc_gpu`, `pc_memory` | PC health, GPU use, and Mac vs PC free RAM |
| `pc_route_advice` | Ask where a command would run, and why |
| `pc_push_file`, `pc_pull_file` | Copy files to and from the PC |
| `pc_wake` | Wake the PC |
| `comfy_generate`, `ollama_chat` | ComfyUI image generation and local LLM chat on the PC |

## Repository layout

| Path | What |
|---|---|
| `agent/` | Python FastAPI agent that runs on the PC (queue, GPU, files, pairing, cache, cleanup) |
| `crates/bridge-core/` | Shared Rust logic: path mapping, router, detection, MCP, shims, client |
| `app/` | Tauri v2 app: React/TypeScript UI in `src/`, Rust backend in `src-tauri/` |
| `scripts/e2e-real-agent.sh` | Real end-to-end test: Rust engine against the live Python agent |
| `docs/GUIDE.md` | Beginner guide |
| `ROADMAP.md` | Living roadmap, decisions and known limitations |

## Developing

```bash
# PC agent
python3 -m venv agent/.venv && agent/.venv/bin/pip install -e "agent[dev]"
(cd agent && .venv/bin/ruff check . && .venv/bin/python -m pytest -q)

# Rust core + app backend
(cd app && npm ci && npm run build)   # the app embeds the built UI
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/e2e-real-agent.sh

# UI
(cd app && npm run lint && npm test)

# Run the app / build installers
(cd app && npx tauri dev)
(cd app && npx tauri build --bundles dmg)   # or nsis on Windows
```

CI runs all of the above on macOS, Windows and Linux. Pushing a tag such as `v0.1.0` builds both installers and attaches them to a GitHub release.

## Security

* Tailscale only: the agent never binds to your LAN or a public address.
* Pairing uses a 6-digit code that works once, expires after 15 minutes, and allows 5 attempts. It's exchanged for a 256-bit token, stored in the Mac's Keychain. The PC keeps only a SHA-256 hash of the token.
* Every request is authenticated. Paths are confined to the PC root, and an optional command allowlist is in `PCBridge\config\settings.json`.

## Known limitations

See [ROADMAP.md → Known limitations](ROADMAP.md#known-limitations).

## License

MIT — see [LICENSE](LICENSE).

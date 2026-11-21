# Architecture

This page shows how the pieces fit together. The diagrams are [Mermaid](https://mermaid.js.org/) and render directly on GitHub.

## System map

```mermaid
flowchart LR
  subgraph MAC["💻 MacBook — the brain"]
    direction TB
    CC["Claude Code / Claude Desktop"]
    MCP["pc-bridge mcp<br/>(MCP server, stdio)"]
    SHIM["~/.pcbridge/bin: ffmpeg, python, npm…<br/>→ pc-bridge shim TOOL"]
    ENG["bridge-core Engine<br/>router · path mapper · memory watcher"]
    HIST[("history.sqlite<br/>learned timings")]
    APPM["PC Bridge app<br/>(menu bar + window)"]
    STM["Syncthing (private instance)"]
    CC --> MCP --> ENG
    CC -- "shell commands" --> SHIM --> ENG
    ENG <--> HIST
    APPM --> ENG
  end

  subgraph PC["🖥 Windows PC — the muscle"]
    direction TB
    APPW["PC Bridge app<br/>(system tray)"]
    AG["Python agent (FastAPI :8765)<br/>auth · queue · cache · cleanup"]
    GPU["NVIDIA GPU (NVENC, CUDA)<br/>32 GB RAM"]
    WC["Worker Claude<br/>claude -p in PCBridge/worker"]
    OL["Ollama / ComfyUI (optional)"]
    STP["Syncthing"]
    ROOT[("D:/PCBridge<br/>one strict root")]
    APPW -- "starts + watches" --> AG
    AG --> GPU
    AG --> WC
    AG --> OL
    AG <--> ROOT
    STP <--> ROOT
  end

  ENG == "Tailscale (WireGuard) tunnel<br/>+ bearer token" ==> AG
  STM <-. "file sync over Tailscale" .-> STP
  APPM -. "Wake-on-LAN (same LAN)" .-> PC
```

### One binary, four modes (Mac)

| Command | Who runs it | Purpose |
|---|---|---|
| `pc-bridge` | You (double-click) | The desktop app: wizard, status, jobs, settings, help, menu bar |
| `pc-bridge mcp` | Claude Code / Claude Desktop | MCP server over stdio that exposes the `pc_*` tools |
| `pc-bridge shim <tool> …` | The shell scripts in `~/.pcbridge/bin` | Transparent redirect: decides Mac or PC, runs, then returns output and exit code |
| `pc-bridge run -- <cmd>` | You or Claude | Forces any command onto the PC |
| `pc-bridge doctor` | You (support) | Prints the health checks as JSON |

On Windows the same binary is the tray app. It also creates a Python virtual environment in `PCBridge\config\venv` and starts the agent from it.

## Life of a job

```mermaid
sequenceDiagram
  autonumber
  participant C as Claude (Mac)
  participant E as Engine (Mac)
  participant A as Agent (PC)
  participant G as GPU / CPU (PC)
  C->>E: ffmpeg -i clip.mov -c:v libx264 out.mp4
  E->>E: classify (GPU / RAM-heavy / light)<br/>estimate Mac vs PC time + Mac memory pressure
  alt PC is faster or the Mac is short on memory
    E->>A: GET /health (wake with WoL if asleep)
    E->>A: POST /files/push (only inputs not yet synced)
    E->>A: POST /run (argv rewritten to h264_nvenc and PC paths)
    A->>A: cache hit? then replay result
    A->>G: run in queue (1 GPU slot, FIFO)
    A-->>E: NDJSON stream: queued · started · stdout · stderr · exit
    E->>A: GET /files/pull (new outputs)
    E-->>C: same output + exit code, paths shown as Mac paths
    E->>E: record timing in history.sqlite
  else Mac is faster, PC unreachable, or redirect paused
    E->>E: run the real tool locally (PCBRIDGE_LOCAL=1)
    E-->>C: output + exit code (+ notification if it was a fallback)
  end
```

## Routing decision

```mermaid
flowchart TD
  S(["new command"]) --> P{"redirect paused?"}
  P -- yes --> MAC(["run on Mac"])
  P -- no --> K{"job kind"}
  K -- light --> MAC
  K -- "GPU or RAM-heavy" --> R{"PC reachable?<br/>wake it if needed"}
  R -- no --> FB(["run on Mac + notify"])
  R -- yes --> M{"job needs more RAM than the Mac<br/>has free, and fits on the PC?"}
  M -- yes --> PCX(["run on PC"])
  M -- no --> CR{"Mac memory pressure<br/>critical?"}
  CR -- yes --> PCX
  CR -- no --> T{"PC time + transfer + queue + wake<br/>shorter than Mac time × swap penalty?"}
  T -- yes --> PCX
  T -- no --> MAC
```

* **Transfer time** = 2 × size (MB) × 8 ÷ measured Mbps (inputs up, outputs back).
* **Queue** = jobs waiting × 60 s. **Wake** = 25 s if the PC was asleep.
* **Swap penalty** on the Mac time: ×1.5 when memory pressure is *warn*, ×3 when it is *critical*.
* **Speed-ups** start from defaults (ffmpeg 4×, Blender 5×, ComfyUI 8×, GPU Python 6×, RAM-heavy 1.3×). Once a tool has 3 or more past runs, the median seconds-per-MB of its last 20 runs replaces the default.

### How jobs are classified

| Kind | Examples | How it's detected |
|---|---|---|
| GPU | `ffmpeg` encodes, `blender`, ComfyUI, `python` with torch/diffusers/whisper/… | tool name, ffmpeg arguments, Python imports, `# pcbridge: gpu` marker |
| RAM-heavy | pandas/numpy/polars scripts, `docker`, `next build`, `npm test`, `ollama` with large models | Python imports, opted-in tool + subcommand, model size in the name (e.g. `70b`), `# pcbridge: ram` |
| Light | `git`, `npm install`, small scripts | everything else; `# pcbridge: local` forces it |

## Path mapping

Each shared Mac folder is given a name, and the PC holds it under `workspace\<name>`:

```
/Users/me/Videos/clip.mov   ⇄   workspace/Videos/clip.mov   ⇄   D:\PCBridge\workspace\Videos\clip.mov
```

Command arguments are rewritten on the way in. Paths in the output (stdout and stderr) are rewritten back to Mac paths on the way out. Any path outside the shared folders is refused on the PC.

## PC folder layout

```
D:\PCBridge\
├── workspace\    shared folders (Syncthing) — one sub-folder per Mac folder
├── jobs\         jobs\2026-11-21\<id>_<name>\  (stdout.log, stderr.log, outputs) — deleted after 14 days
├── worker\       scratch space for the worker Claude
├── cache\        result cache (content-hash keyed, LRU, 20 GB cap)
├── models\       Ollama / ComfyUI models (optional)
├── logs\         agent.log, syncthing.log
└── config\       settings.json, auth.json (token hashes), venv\, agent-src\
```

## Security model

```mermaid
flowchart LR
  M["Mac"] -- "6-digit code: 15 min, 5 tries, once" --> A["Agent"]
  A -- "256-bit token, kept in Mac Keychain" --> M
  M -- "Bearer token on every request" --> A
  A --> H[("auth.json<br/>SHA-256 hashes only")]
```

* The agent binds only to `127.0.0.1` and the PC's Tailscale IP.
* Every path is resolved and checked against the root. An optional `allowlist` limits which programs can run.
* The PC's own tray app authenticates with a separate local token file that never leaves the PC.

## Code map

| Path | Language | Responsibility |
|---|---|---|
| `agent/pcbridge_agent/app.py` | Python | HTTP API, streaming, Syncthing/Ollama/ComfyUI endpoints |
| `agent/pcbridge_agent/jobs.py` | Python | FIFO queue, process runner, output detection, keep-awake |
| `agent/pcbridge_agent/{pairing,cache,cleanup,gpu,memory,config}.py` | Python | One concern each |
| `crates/bridge-core/src/router.rs` | Rust | Routing decision + SQLite history |
| `crates/bridge-core/src/detect.rs` | Rust | Job classification, RAM estimates, NVENC rewrite, `.pcbridge` opt-ins |
| `crates/bridge-core/src/shim.rs` | Rust | The engine: plan → wake → push → run → pull → record |
| `crates/bridge-core/src/mcp.rs` | Rust | MCP protocol + tool definitions |
| `crates/bridge-core/src/paths.rs` | Rust | Mac ⇄ PC path mapping |
| `crates/bridge-core/src/claude_config.rs` | Rust | Idempotent edits to Claude configs, CLAUDE.md, shims, PATH |
| `app/src-tauri/src/{mac,win,tray}.rs` | Rust | Tauri commands for each OS, tray icon |
| `app/src/` | TypeScript/React | Wizards and the four main screens |

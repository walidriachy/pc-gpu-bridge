# Using PC Bridge

Setting up for the first time? Start with the [beginner guide](GUIDE.md). This page covers everyday use and every setting.

- [Ask Claude](#ask-claude)
- [Run things yourself](#run-things-yourself)
- [Claude tools (MCP)](#claude-tools-mcp)
- [Per-project opt-in: npm, pnpm, Docker](#per-project-opt-in-npm-pnpm-docker)
- [Hints inside Python scripts](#hints-inside-python-scripts)
- [The apps](#the-apps)
- [Configuration reference](#configuration-reference)
- [Environment variables](#environment-variables)
- [Where things are stored](#where-things-are-stored)

## Ask Claude

Once you've connected Claude (Mac wizard, step 5), you don't need to do anything special. Talk to Claude as usual:

| You say | What happens |
|---|---|
| "Convert `interview.mov` to a 1080p MP4 for YouTube" | `ffmpeg` runs on the PC with the NVENC hardware encoder. The MP4 appears next to the original |
| "Transcribe every file in `Podcasts/` with Whisper" | The GPU Python job runs on the PC |
| "Generate product shots with my ComfyUI workflow" | `comfy_generate` queues the workflow on ComfyUI on the PC (if installed) |
| "Ask the local Llama 70B to summarise this report" | `ollama_chat` runs on the PC. Models stay loaded, and the least recently used one is unloaded when another needs the VRAM |
| "Run the full test suite / build the Next.js app" | Goes to the PC if that project has opted in (see below) |
| "Process this 8 GB CSV with pandas" | RAM-heavy, so it goes to the PC's 32 GB if the Mac can't comfortably hold it |
| "Take this whole folder and caption every image" | `pc_delegate_task` hands the task to the worker Claude next to the GPU |
| "Is my PC on? How busy is the GPU?" | `pc_status`, `pc_gpu`, `pc_memory` |

Claude learns about these tools from a short block that PC Bridge adds to `~/.claude/CLAUDE.md`. You can preview the block in the wizard before it's added.

## Run things yourself

The wizard can add `~/.pcbridge/bin` to the front of your `PATH`, so these commands are routed automatically when you type them in Terminal:

```
ffmpeg  ffprobe  python  python3  blender  comfy
npm  npx  pnpm  node  docker        ← only in projects that opted in
```

Each one decides Mac or PC for itself, prints `[PC Bridge] …` on stderr when it sends the job away, and returns the real exit code.

```bash
pc-bridge run -- python train.py --epochs 20     # force this command onto the PC
PCBRIDGE_LOCAL=1 ffmpeg -i a.mov b.mp4           # force this one to stay on the Mac
pc-bridge doctor                                 # print health checks (Tailscale etc.) as JSON
```

> `pc-bridge` lives inside the app (`/Applications/PC Bridge.app/Contents/MacOS/pc-bridge`). The wizard also puts a `pc-bridge` launcher in `~/.pcbridge/bin`, so you can type it in Terminal.

## Claude tools (MCP)

| Tool | Arguments (main ones) | What it does |
|---|---|---|
| `pc_run` | `command`, `cwd`, `mode` (`pc` or `auto`) | Run a command on the PC. Paths are translated, missing inputs pushed, outputs pulled back |
| `pc_run_task` | `command`, `cwd` | The generic "run this task on the PC" option: any command line, run in PowerShell on the PC |
| `pc_delegate_task` | `task`, `files` | The worker Claude on the PC does the whole task and returns its answer and job folder |
| `pc_docker` | `args`, `cwd` | `docker …` on the PC (build, run, compose) |
| `pc_node_build` | `tool` (`npm`/`pnpm`/`npx`/`node`), `args`, `cwd` | Big JavaScript builds and test suites on the PC |
| `pc_status` | – | Reachable? Queue, running jobs, installed tools |
| `pc_gpu` | – | GPU name, utilisation, VRAM, temperature |
| `pc_memory` | – | Mac and PC free RAM side by side, plus Mac memory pressure |
| `pc_route_advice` | `command`, `cwd` | Where this command would run, and why (nothing is run) |
| `pc_push_file` | `mac_path`, `pc_rel_path` | Copy a Mac file to the PC |
| `pc_pull_file` | `pc_path`, `mac_path` | Copy a PC file (for example a job output) to the Mac |
| `pc_wake` | – | Send Wake-on-LAN and wait for the PC |
| `comfy_generate` | `workflow` (ComfyUI API-format JSON) | Queue an image workflow on ComfyUI on the PC and return the image paths |
| `ollama_chat` | `model`, `prompt` or `messages` | Chat with a local model on the PC; models stay loaded in VRAM |

The MCP server is registered as **`pc-bridge`** in `~/.claude.json` (Claude Code) and in `~/Library/Application Support/Claude/claude_desktop_config.json` (Claude Desktop).

## Per-project opt-in: npm, pnpm, Docker

JavaScript builds and Docker can easily surprise you if they suddenly run on another computer, so they're off until you opt a project in. Create a file named `.pcbridge` in the project folder (or in any folder above it):

```toml
# .pcbridge
offload = ["npm", "pnpm", "docker"]
```

Even then, only heavy work goes to the PC: `npm/pnpm test` and `build`, `run` scripts whose name contains `build`, `test` or `e2e`, `next build`, and `docker build`, `run`, `compose` and `buildx`. `npm install`, `npm run dev` and similar stay on the Mac.

**Requirement:** the project's dependencies must exist on the PC too. Run `pc-bridge run -- npm ci` once in the project, because Mac `node_modules` aren't synced.

## Hints inside Python scripts

Add one comment anywhere in a script to override the automatic classification:

```python
# pcbridge: gpu     ← always treat as a GPU job
# pcbridge: ram     ← memory-heavy, prefer the PC when the Mac is under pressure
# pcbridge: local   ← never send this script to the PC
```

## The apps

**Mac (menu bar dot + window)**

| Screen | What's there |
|---|---|
| Status | PC online or asleep, **Mac vs PC free RAM side by side**, GPU load and VRAM, running job, the "Send heavy jobs to the PC" switch, Wake PC |
| Jobs | Every PC job with Open output / Folder / Cancel, and routing history (where each command ran and how long it took) |
| Settings | Notifications, re-pair or unpair, shared folders, npm/pnpm/Docker opt-in help, Connect or Disconnect Claude |
| Help | How it works, RAM explanation, FAQ, security, **Run diagnostics** |

The menu bar menu shows the status, GPU, free RAM and the current job, plus Pause redirecting, Open job folder, Open PC Bridge and Quit.

**Windows (tray dot + window):** the same four screens. Status shows this PC and the last free RAM your Mac reported. Settings has Start with Windows, Open folder, Open logs and a new pairing code.

## Configuration reference

### Mac: `~/.pcbridge/config.json`

| Key | Default | Meaning |
|---|---|---|
| `pc_host` | – | Tailscale name or IP of the PC (set by pairing) |
| `port` | `8765` | Agent port |
| `pc_root` | `D:\PCBridge` | PC root folder (reported by the PC when you pair) |
| `shared_folders` | `[]` | `[{ "mac_path": "/Users/me/Videos", "name": "Videos" }]` |
| `redirect_enabled` | `true` | Master switch (the menu bar "Pause redirecting" sets this) |
| `measured_mbps` | `300` | Link speed used for transfer estimates (set by wizard step 6) |
| `sync_wait_secs` | `3` | How long to wait for Syncthing before pulling outputs directly |
| `notify_on_fallback` | `true` | Notify when a job had to run on the Mac |
| `pc_mac_address` | – | PC Ethernet MAC address for Wake-on-LAN (sent by the PC) |

### PC: `<root>\config\settings.json`

| Key | Default | Meaning |
|---|---|---|
| `port` | `8765` | Agent port |
| `max_parallel_jobs` | `1` | Jobs that may run at once (keep 1 for GPU jobs) |
| `cleanup_days` | `14` | Delete job folders older than this |
| `cache_max_gb` | `20` | Result-cache size cap (least recently used results are dropped first) |
| `allowlist` | `[]` | If not empty, only these program names may run, e.g. `["ffmpeg","python","blender"]` |
| `delegate_args` | `--permission-mode acceptEdits --allowedTools Bash,Read,Write,Edit,Glob,Grep` | Extra arguments for the worker `claude -p` |
| `bind_hosts` | `[]` | Empty means `127.0.0.1` plus the Tailscale IP. Don't add `0.0.0.0` |
| `claude_cmd`, `nvidia_smi_cmd` | `claude`, `nvidia-smi` | Override program locations |

After editing it, quit PC Bridge from the tray menu and open it again so the agent restarts.

## Environment variables

| Variable | Where | Effect |
|---|---|---|
| `PCBRIDGE_LOCAL=1` | Mac | Run the next command on the Mac, no questions asked |
| `PCBRIDGE_HOME` | Mac | Use another settings folder instead of `~/.pcbridge` (testing) |
| `PCBRIDGE_TOKEN` | Mac | Use this token instead of the Keychain one (testing/CI) |
| `PCBRIDGE_NO_NOTIFY=1` | Mac | No macOS notifications |
| `PCBRIDGE_ROOT` | PC | Agent root folder override |

## Where things are stored

| Mac | |
|---|---|
| `~/.pcbridge/config.json` | Settings |
| `~/.pcbridge/history.sqlite` | Routing history (timings) |
| `~/.pcbridge/bin/` | Shims + `pc-bridge` launcher |
| `~/.pcbridge/syncthing/` | Private Syncthing instance (GUI on 127.0.0.1:8385) |
| Keychain → `com.pcbridge.app` | Pairing token |

| PC | |
|---|---|
| `<root>\jobs\YYYY-MM-DD\<id>_<name>\` | Logs and outputs of each job (14 days) |
| `<root>\logs\` | `agent.log`, `syncthing.log` |
| `<root>\config\` | `settings.json`, `auth.json`, `local_token`, `venv\` |
| `%USERPROFILE%\.pcbridge\windows.json` | Wizard progress for the Windows app |

# Using PC Bridge

Setting up for the first time? Start with the [beginner guide](GUIDE.md). This page covers everyday use and every setting.

- [Ask Claude](#ask-claude)
- [Run things yourself](#run-things-yourself)
- [Claude tools (MCP)](#claude-tools-mcp)
- [Missing programs on the PC](#missing-programs-on-the-pc)
- [How job results are found](#how-job-results-are-found)
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
| "Process this 8 GB CSV with pandas" | RAM-heavy, so it goes to the PC's memory if the Mac can't comfortably hold it |
| "Take this whole folder and caption every image" | `pc_delegate_task` hands the task to the worker Claude next to the GPU |
| "Is my PC on? How busy is the GPU?" | `pc_status`, `pc_gpu`, `pc_memory` |
| "Render `scene.blend`" on a PC without Blender | The job stops with "Blender isn't installed on the PC". Claude asks you, then installs it with `pc_install` and tries again |

Claude learns about these tools from a short block that PC Bridge adds to `~/.claude/CLAUDE.md`. You can preview the block in the wizard before it's added. One line of it tells Claude: *if a job fails because a program is missing on the PC, offer to install it with `pc_install` (ask the user first).* If you connected Claude with an older version, click **Settings → Reconnect to Claude** on the Mac to get that line (the MCP tool itself arrives with the app update).

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
| `pc_status` | – | Reachable? Queue, running jobs, installed tools, and a "Programs on the PC: installed … · not installed …" line |
| `pc_install` | `program` (`ffmpeg`, `blender`, `pytorch`, `ollama`, `node`, `docker` or `git`) | Install a missing program on the PC, in its normal Windows place. Claude is told to **ask you first**. See [Missing programs on the PC](#missing-programs-on-the-pc) |
| `pc_gpu` | – | GPU name, utilisation, VRAM, temperature |
| `pc_memory` | – | Mac and PC free RAM side by side, plus Mac memory pressure |
| `pc_route_advice` | `command`, `cwd` | Where this command would run, and why (nothing is run) |
| `pc_push_file` | `mac_path`, `pc_rel_path` | Copy a Mac file to the PC |
| `pc_pull_file` | `pc_path`, `mac_path` | Copy a PC file (for example a job output) to the Mac |
| `pc_wake` | – | Send Wake-on-LAN and wait for the PC |
| `comfy_generate` | `workflow` (ComfyUI API-format JSON) | Queue an image workflow on ComfyUI on the PC and return the image paths |
| `ollama_chat` | `model`, `prompt` or `messages` | Chat with a local model on the PC; models stay loaded in VRAM |

That's 15 tools. The MCP server is registered as **`pc-bridge`** in `~/.claude.json` (Claude Code) and in `~/Library/Application Support/Claude/claude_desktop_config.json` (Claude Desktop).

## Missing programs on the PC

When a job needs a program the PC doesn't have, the job still fails, but now it says exactly what's missing and how to add it:

```text
PC Bridge: program not found on the PC: blender (it isn't installed there, or Windows can't find it)
PC Bridge: Blender isn't installed on the PC. Ask Claude to install it (tool pc_install, program "blender"), or on the PC open PC Bridge → Settings → Programs → Install.
```

The PC spots this when the program itself can't be started (exit code 127), when a Python job says `No module named 'torch'` (or `torchvision`, `torchaudio`), and when a script's shell says a known program "is not recognized" or "command not found". Claude's job result then ends with a note to ask you and use `pc_install`.

A job the PC has accepted is **never run again on the Mac**, even when a program was missing. You decide whether to install it.

**Three ways to install:**

| Where | How |
|---|---|
| Claude | Say yes when Claude offers, or ask "install Blender on my PC". Claude uses `pc_install` |
| PC app | **Settings → Programs**: every program with what it's for, its size, and ✅ **Installed** or an **Install** button |
| PC wizard | Step 6 has a **PyTorch (GPU)** option next to Ollama and ComfyUI |

**What can be installed** (nothing else; the PC refuses any other name):

| `program` | Installs | For | Space it needs (about) | Installed with |
|---|---|---|---|---|
| `ffmpeg` | FFmpeg | Video and audio encoding | 0.3 GB | winget `Gyan.FFmpeg` |
| `blender` | Blender | 3D modelling and rendering | 1 GB | winget `BlenderFoundation.Blender` |
| `pytorch` | PyTorch (GPU): torch, torchvision, torchaudio | AI in Python: Whisper, Stable Diffusion, your own models | 6 GB | pip, CUDA 12.8 build from download.pytorch.org, into the PC's own Python |
| `ollama` | Ollama | Local AI chat models | 1 GB | winget `Ollama.Ollama` |
| `node` | Node.js LTS | JavaScript builds and tests (npm, npx) | 0.2 GB | winget `OpenJS.NodeJS.LTS` |
| `docker` | Docker Desktop | Docker builds and containers | 3 GB | winget `Docker.DockerDesktop` |
| `git` | Git | Version control | 0.4 GB | winget `Git.Git` |

Good to know:

* Programs go in their **normal Windows place** (for example `C:\Program Files`, or Python's own folder for PyTorch), never in the PC Bridge folder or your shared folders, so nothing big syncs back to the Mac.
* Before installing, the PC checks there's room on the Windows drive (usually `C:`): the program's size **plus 2 GB**. If not, it says how much is free and how much is needed.
* Some installers show a Windows **"Do you want to allow this app to make changes?"** prompt on the PC's screen. Someone at the PC has to click **Yes**. If a winget install shows no progress for 90 seconds, its log says so; after 20 minutes without progress (or 3 hours in all) PC Bridge stops it and explains why.
* Installs have their own lane, one at a time: they never make GPU jobs wait, and they don't wait for a running render. Each install is still listed on the Jobs page with its log in the job folder. Asking twice for the same program follows the install that's already running.
* If winget says the program is already installed, that counts as success. If it says the PC must restart to finish, the install counts as done and you're told to restart the PC.
* Blender's installer doesn't add it to PATH. The PC also looks in `C:\Program Files\Blender Foundation\` and uses the newest version there.
* A bare `python` or `python3` job runs the PC's own Python (the one PyTorch is installed into). It skips PC Bridge's private Python and the Microsoft Store placeholder that only opens the Store (a Python installed from the Store is fine).
* A job that runs another Python (a venv's `python.exe`, conda, `py -3.11`) and lacks `torch` is told the exact `pip install` command for that Python, since `pc_install pytorch` only fills the PC's main Python.
* Still shows as missing after installing? On the PC, **Settings → Restart the helper**, then **Check again**.

## How job results are found

After a job, the PC lists the files it created or changed and sends them back. To stay fast on very big shared folders, it only looks in:

* the folder the job ran in, and up to **2 levels of sub-folders** below it (all of it for worker-Claude jobs);
* each folder that a command argument names inside the PC Bridge folder, for example `-o renders/` or `out=frames/f_%04d.png` (up to 32 folders). For a file argument, only the files directly in its folder are checked. For Blender's `-o //renders/f_####`, the `.blend` file's folder is checked 2 levels deep.

Each check stops after 200,000 files or 5 seconds. Files a job writes **anywhere else** in a shared folder still reach the Mac through Syncthing, but they aren't listed as the job's outputs, aren't copied into the job folder, and aren't reused by the result cache. If that matters, name the output folder on the command line, or run the job from that folder.

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
| Settings | Notifications, re-pair or unpair, shared folders, npm/pnpm/Docker opt-in help, Connect or Disconnect Claude, **Check for updates now** |
| Help | How it works, RAM explanation, FAQ, security, **Run diagnostics** |

The menu bar menu shows the status, GPU, free RAM and the current job, plus Pause redirecting, Open job folder, Open PC Bridge and Quit.

**Windows (tray dot + window):** the same four screens. Status shows this PC and the last free RAM your Mac reported. Settings has Start with Windows, Open folder, Open logs, **Check for updates now**, **Programs** (see what's installed, free space on the Windows drive, and **Install** buttons), **Restart the helper** and a new pairing code.

**Both apps, in the background:** they install signed updates by themselves (2 minutes after start, then every 6 hours; the PC waits for a running job). Once paired, they keep Tailscale open and connected. The Windows app also replaces an outdated helper when it starts. The Mac app checks it's running from `/Applications/PC Bridge.app` when it starts; if macOS started a hidden temporary copy instead, it reopens itself from Applications (see the [guide](GUIDE.md#pc-bridge-is-damaged-and-cant-be-opened-version-012)).

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
| `C:\Program Files\…`, Python's own folder | Programs installed through **Settings → Programs** or `pc_install` (outside the PC Bridge folder) |
